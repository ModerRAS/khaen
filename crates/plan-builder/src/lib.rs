//! 方案组装：把设计/校准产物（通道布局、分频、矩阵、保护、路由）拼成 plan ABI v3。
//!
//! # 公共入口
//!
//! 组装器叫 PlanBuilder（方案组装器）：PlanBuilder::new(config) 带方案级配置
//! （块大小 / 采样率 / 通道数），各段 setter 直接对应 ABI v3 的五个区段
//! （ContentSection / CrossoverSection / MatrixSection / ProtectionSection / RoutingSection），
//! PlanBuilder::build 做 fail-closed 校验后产出 khaen_core::plan::Plan，
//! PlanBuilder::build_bytes 再走 Plan::encode 返回可直接落盘的字节。
//!
//! 便捷构造：PlanBuilder::identity 一次填满五段"逐样本直通"方案（identity 判据与
//! khaen_host::graph 的编译期折叠一致），用来做基线、测试与故障回退。
//!
//! # 用法
//!
//! 典型路径是"设置区段 -> build -> 落盘/交给 host 准入"：
//!
//! - 组装：PlanBuilder::new(config).set_content_layout(...).set_matrix(...).set_routing(...)
//! - 校验：build() 返回 Result<Plan, PlanBuildError>，错误按 PlanBuildErrorCode match
//! - 序列化：build_bytes() = build()?.encode()（ABI v3 的 CRC / graph_id 由 core 负责）
//!
//! # 职责边界
//!
//! - 只做组装与序列化：ABI v3 的编码/解码/CRC-32C/graph_id 归 khaen_core::plan；
//!   本 crate 负责"把人的意图变成合法的 Plan"，并把错误翻成稳定错误码
//!   （PlanBuildErrorCode，可用 as_str() 取稳定小写下划线编码）。
//! - 不做 DSP：不卷积、不拟合、不设计滤波器。分频/矩阵/PEQ 的系数必须由 designer /
//!   calibration 产出后传进来；本 crate 只校验它们能被 ABI 表示、且引用关系自洽。
//! - 不判预算：内存/延迟/系数/矩阵格数/常驻状态等准入预算是 host 的策略
//!   （khaen_host::admission::AdmissionLimits，随部署规模可调）；本 crate 只保证结构性合法
//!   （可编码、可原样解码、lane 链与引用不越界）。代表性方案"被 host 准入且图编译成功"
//!   由 tests/builder.rs 锁住，不代表任意可 build 的方案都在默认预算内。
//! - fail-closed：任何不自洽（通道数不匹配、矩阵维度/系数不对、路由引用不存在的通道、
//!   超过 ABI 解码器上限、采样率/块大小不在运行时契约里）都在 build 返回错误，绝不产出
//!   "先编码、事后才在 host 里炸"的方案。
//!
//! # 确定性
//!
//! 相同输入（相同 PlanConfig + 相同区段内容与顺序）必产出完全相同的字节与 graph_id：
//! 组装不读时钟、不用随机数、不遍历哈希表，也不重排调用方给的 folding / filter / entry /
//! route 顺序（顺序会影响浮点累加顺序，属于方案内容的一部分）。
//!
//! # 关于 ABI v3 没表达的东西（如实声明）
//!
//! - 分频区段在 ABI v3 里只有"每 lane 一组 FIR 系数"（CrossoverFilter { lane, coeffs }），
//!   没有滤波器类型/阶数/截止频率字段；这些设计期描述无法进方案文件。本 crate 只接收系数，
//!   不把类型/阶数/截止频率伪编码进文件。
//! - 保护区段里同样没有"增益上限"字段；ABI 能表达的是每 lane 的 gain_db（电平）、polarity、
//!   delay 与 peq。保护策略（限幅/温度模型）不在这份契约里。
//! - 矩阵抽取/插值因子只校验非零；"固定速率图要求 decimation == interpolation、且不超过
//!   host 因子上限"是 khaen_host 的编译期策略（khaen_host::graph::CompileError::UnsupportedRates），
//!   本 crate 不复制该策略常量。
//!
//! # 错误码示例
//!
//! 不支持 44.1 kHz 时返回 UnsupportedSampleRate（带 PlanBuildErrorCode::as_str 编码），
//! 而不是先编码再让 host 报错。

mod error;
mod validate;

pub use error::{BuildStage, PlanBuildError, PlanBuildErrorCode};

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};

/// content 区段折叠表项数上限（ABI v3 的计数域是 u16）。
pub const MAX_FOLDS: usize = u16::MAX as usize;

/// crossover 区段滤波器数量上限（ABI v3 的计数域是 u16）。
pub const MAX_CROSSOVER_FILTERS: usize = u16::MAX as usize;

/// 单个分频滤波器的抽头上限。
///
/// 与 khaen_core::plan 的解码器防护一致（超过它的方案 encode 得出来、decode 不回去）。
pub const MAX_CROSSOVER_TAPS: usize = 1 << 20;

/// 矩阵系数总数上限。
///
/// 与 khaen_core::plan 的解码器防护一致（rows * cols * taps 超过它的方案 decode 不回去）。
pub const MAX_MATRIX_COEFFICIENTS: usize = 1 << 22;

/// protection 区段保护项数量上限（ABI v3 的计数域是 u16）。
pub const MAX_PROTECTION_ENTRIES: usize = u16::MAX as usize;

/// 单 lane PEQ 段数上限（ABI v3 的计数域是 u16）。
pub const MAX_PEQ_SECTIONS: usize = u16::MAX as usize;

/// routing 区段路由表项数上限（ABI v3 的计数域是 u16）。
pub const MAX_ROUTES: usize = u16::MAX as usize;

/// 方案级配置：字段与 Plan 的头部对齐，输入/输出通道数分开。
///
/// Plan 本身不存通道数：输入通道数在 content.in_channels，输出 DAC 通道数在
/// routing.dac_channels；host 的 EngineConfig.input_channels / output_channels 必须分别
/// 等于它们。build 时按这两个宽度分别校验，不再要求输入宽度 == DAC 宽度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanConfig {
    /// 每块帧数（必须落在 khaen_core::rt::BLOCK_SIZES 里）。
    pub block_size: u32,
    /// 采样率（Hz，必须落在 khaen_core::rt::SAMPLE_RATES_HZ 里）。
    pub sample_rate: u32,
    /// 输入通道数（对应 content.in_channels）。
    pub input_channels: u16,
    /// 输出 DAC 通道数（对应 routing.dac_channels，缺 routing 时为 matrix.rows）。
    pub output_channels: u16,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            block_size: u32::from(khaen_core::rt::DEFAULT_BLOCK_SIZE),
            sample_rate: 96_000,
            input_channels: 2,
            output_channels: 2,
        }
    }
}

impl PlanConfig {
    /// 显式构造等宽配置（输入 = 输出；旧调用方保持原行为）。
    pub const fn new(block_size: u32, sample_rate: u32, channels: u16) -> Self {
        Self {
            block_size,
            sample_rate,
            input_channels: channels,
            output_channels: channels,
        }
    }

    /// 显式构造输入/输出宽度不同的配置（例如 2 输入 → 22 执行器/DAC）。
    pub const fn new_split(
        block_size: u32,
        sample_rate: u32,
        input_channels: u16,
        output_channels: u16,
    ) -> Self {
        Self {
            block_size,
            sample_rate,
            input_channels,
            output_channels,
        }
    }

    /// 只校验方案级配置（build 的第一步）；失败返回稳定错误码。
    pub fn validate(&self) -> Result<(), PlanBuildError> {
        validate::validate_config(*self)
    }
}

/// 方案组装器：五个可选区段 + 方案级配置，build 时统一 fail-closed 校验。
///
/// setter 都是消耗式链式调用（返回 Self），未设置的区段在 Plan 里保持 None：
/// host 的图执行器把缺省区段按恒等处理，但 matrix 例外（必须有）。便捷方法
/// （add_fold / add_crossover_filter / add_protection / add_route）在区段缺席时
/// 以 config.channels 作缺省 lane / DAC 数；拓扑不是"通道数 = lane 数"时请用完整 setter。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanBuilder {
    config: PlanConfig,
    content: Option<ContentSection>,
    crossover: Option<CrossoverSection>,
    matrix: Option<MatrixSection>,
    protection: Option<ProtectionSection>,
    routing: Option<RoutingSection>,
}

impl PlanBuilder {
    /// 创建只带方案级配置的组装器；五个区段都缺席，build 会因缺 matrix 失败。
    pub fn new(config: PlanConfig) -> Self {
        Self {
            config,
            content: None,
            crossover: None,
            matrix: None,
            protection: None,
            routing: None,
        }
    }

    /// 创建 identity 方案组装器：五段全部填成"逐样本直通"形态。
    ///
    /// identity 判据（与 khaen_host::graph 的编译期折叠一致）：content 每 lane 单
    /// fold(src = dst, gain = 1.0)；crossover 每 lane [1.0]；matrix rows == cols == channels、
    /// taps = 1、对角 1 其余 0、decimation = interpolation = 1；protection 每 lane
    /// delay 0 / polarity +1 / 0 dB / 无 PEQ；routing lane i -> dac i 且 dac_channels == channels。
    ///
    /// 注意：矩阵是 O(channels^2) 个 f32，通道数很大时请改用显式 setter 以节省内存。
    pub fn identity(config: PlanConfig) -> Self {
        let input_channels = config.input_channels;
        let output_channels = config.output_channels;
        // 等宽时就是原来的逐样本直通；不等宽时 content 把前 min(in,out) 路直通，
        // 其余输出 lane 由 content 填静音，matrix/crossover/protection 仍按输出宽度。
        let folds = (0..input_channels.min(output_channels))
            .map(|channel| khaen_core::plan::Fold {
                src: channel,
                dst: channel,
                gain: 1.0,
            })
            .collect();
        Self::new(config)
            .set_content(ContentSection {
                in_channels: input_channels,
                out_lanes: output_channels,
                folds,
            })
            .set_crossover_identity(output_channels)
            .set_matrix_identity(output_channels)
            .set_protection_identity(output_channels)
            .set_routing_identity(output_channels)
    }

    /// 方案级配置。
    pub fn config(&self) -> PlanConfig {
        self.config
    }

    /// content 区段（未设置返回 None）。
    pub fn content(&self) -> Option<&ContentSection> {
        self.content.as_ref()
    }

    /// crossover 区段（未设置返回 None）。
    pub fn crossover(&self) -> Option<&CrossoverSection> {
        self.crossover.as_ref()
    }

    /// matrix 区段（未设置返回 None；build 要求必须有）。
    pub fn matrix(&self) -> Option<&MatrixSection> {
        self.matrix.as_ref()
    }

    /// protection 区段（未设置返回 None）。
    pub fn protection(&self) -> Option<&ProtectionSection> {
        self.protection.as_ref()
    }

    /// routing 区段（未设置返回 None）。
    pub fn routing(&self) -> Option<&RoutingSection> {
        self.routing.as_ref()
    }

    /// 替换 content 区段。
    pub fn set_content(mut self, content: ContentSection) -> Self {
        self.content = Some(content);
        self
    }

    /// 用给定折叠表的 content 布局替换 content 区段。
    pub fn set_content_layout(
        mut self,
        in_channels: u16,
        out_lanes: u16,
        folds: impl IntoIterator<Item = Fold>,
    ) -> Self {
        self.content = Some(ContentSection {
            in_channels,
            out_lanes,
            folds: folds.into_iter().collect(),
        });
        self
    }

    /// identity content：每通道单 fold(src = dst, gain = 1.0)，in_channels = out_lanes = channels。
    pub fn set_content_identity(mut self, channels: u16) -> Self {
        self.content = Some(ContentSection {
            in_channels: channels,
            out_lanes: channels,
            folds: (0..channels)
                .map(|channel| Fold {
                    src: channel,
                    dst: channel,
                    gain: 1.0,
                })
                .collect(),
        });
        self
    }

    /// 追加一条折叠；content 缺席时以 config 的输入/输出宽度建布局再追加。
    pub fn add_fold(mut self, fold: Fold) -> Self {
        let input_channels = self.config.input_channels;
        let output_channels = self.config.output_channels;
        let content = self.content.get_or_insert_with(|| ContentSection {
            in_channels: input_channels,
            out_lanes: output_channels,
            folds: Vec::new(),
        });
        content.folds.push(fold);
        self
    }

    /// 替换 crossover 区段。
    pub fn set_crossover(mut self, crossover: CrossoverSection) -> Self {
        self.crossover = Some(crossover);
        self
    }

    /// 用给定 lanes 与滤波器组替换 crossover 区段。
    pub fn set_crossover_filters(
        mut self,
        lanes: u16,
        filters: impl IntoIterator<Item = CrossoverFilter>,
    ) -> Self {
        self.crossover = Some(CrossoverSection {
            lanes,
            filters: filters.into_iter().collect(),
        });
        self
    }

    /// identity crossover：每 lane 一个 [1.0] 滤波器（host 编译期折叠为直通）。
    pub fn set_crossover_identity(mut self, lanes: u16) -> Self {
        self.crossover = Some(CrossoverSection {
            lanes,
            filters: (0..lanes)
                .map(|lane| CrossoverFilter {
                    lane,
                    coeffs: vec![1.0],
                })
                .collect(),
        });
        self
    }

    /// 追加一个分频滤波器；crossover 缺席时以 config.channels 为 lanes 建恒等段再追加。
    ///
    /// 同一 lane 重复追加会在 build 报 DuplicateCrossoverLane。
    pub fn add_crossover_filter(mut self, lane: u16, coeffs: impl Into<Vec<f32>>) -> Self {
        let channels = self.config.output_channels;
        let crossover = self.crossover.get_or_insert_with(|| CrossoverSection {
            lanes: channels,
            filters: Vec::new(),
        });
        crossover.filters.push(CrossoverFilter {
            lane,
            coeffs: coeffs.into(),
        });
        self
    }

    /// 替换 matrix 区段。
    pub fn set_matrix(mut self, matrix: MatrixSection) -> Self {
        self.matrix = Some(matrix);
        self
    }

    /// 用显式网格参数替换 matrix 区段（coeffs 按行优先，长度必须是 rows * cols * taps）。
    pub fn set_matrix_cells(
        mut self,
        rows: u16,
        cols: u16,
        decimation: u8,
        interpolation: u8,
        taps: u32,
        coeffs: impl Into<Vec<f32>>,
    ) -> Self {
        self.matrix = Some(MatrixSection {
            rows,
            cols,
            decimation,
            interpolation,
            taps,
            coeffs: coeffs.into(),
        });
        self
    }

    /// identity 矩阵：lanes x lanes、taps = 1、对角 1 其余 0、decimation = interpolation = 1。
    ///
    /// 只设置 matrix 区段（content / crossover / protection / routing 保持原样）。
    pub fn set_matrix_identity(mut self, lanes: u16) -> Self {
        self.matrix = Some(identity_matrix(lanes));
        self
    }

    /// 替换 protection 区段。
    pub fn set_protection(mut self, protection: ProtectionSection) -> Self {
        self.protection = Some(protection);
        self
    }

    /// identity protection：每 lane 一条 delay 0 / polarity +1 / 0 dB / 无 PEQ 的保护项。
    pub fn set_protection_identity(mut self, lanes: u16) -> Self {
        self.protection = Some(identity_protection(lanes));
        self
    }

    /// 追加一条保护项；protection 缺席时以 config.channels 为 lanes 建 identity 段再追加。
    ///
    /// 同一 lane 重复追加会在 build 报 DuplicateProtectionLane。
    pub fn add_protection(mut self, entry: ProtectionEntry) -> Self {
        let channels = self.config.output_channels;
        let protection = self
            .protection
            .get_or_insert_with(|| identity_protection(channels));
        protection.entries.push(entry);
        self
    }

    /// 设置某 lane 的保护电平（dB）：已有该 lane 的 entry 则改 gain_db，否则追加一条 identity entry。
    ///
    /// protection 缺席时以 config.channels 为 lanes 建 identity 段；lane 越界的错误由 build 报出。
    pub fn set_protection_gain_db(mut self, lane: u16, gain_db: f32) -> Self {
        let channels = self.config.output_channels;
        let protection = self
            .protection
            .get_or_insert_with(|| identity_protection(channels));
        match protection
            .entries
            .iter_mut()
            .find(|entry| entry.lane == lane)
        {
            Some(entry) => entry.gain_db = gain_db,
            None => protection.entries.push(ProtectionEntry {
                lane,
                delay: 0,
                polarity: 1,
                gain_db,
                peq: Vec::new(),
            }),
        }
        self
    }

    /// 替换 routing 区段。
    pub fn set_routing(mut self, routing: RoutingSection) -> Self {
        self.routing = Some(routing);
        self
    }

    /// 用给定 DAC 通道数与路由表替换 routing 区段（未映射的 DAC 输出静音）。
    pub fn set_routing_map(
        mut self,
        dac_channels: u16,
        routes: impl IntoIterator<Item = (u16, u16)>,
    ) -> Self {
        self.routing = Some(RoutingSection {
            dac_channels,
            routes: routes.into_iter().collect(),
        });
        self
    }

    /// identity routing：lane i -> dac i，dac_channels = lanes。
    pub fn set_routing_identity(mut self, lanes: u16) -> Self {
        self.routing = Some(RoutingSection {
            dac_channels: lanes,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        });
        self
    }

    /// 追加一条路由；routing 缺席时以 config.channels 为 dac_channels 建恒等段再追加。
    pub fn add_route(mut self, lane: u16, dac: u16) -> Self {
        let channels = self.config.output_channels;
        let routing = self.routing.get_or_insert_with(|| RoutingSection {
            dac_channels: channels,
            routes: Vec::new(),
        });
        routing.routes.push((lane, dac));
        self
    }

    /// fail-closed 校验并产出 Plan。
    ///
    /// 校验顺序固定：config -> content -> crossover -> matrix -> protection -> routing -> lane 链。
    /// 首个失败项决定返回的错误码（PlanBuildErrorCode + BuildStage + detail）。
    pub fn build(&self) -> Result<Plan, PlanBuildError> {
        validate::assemble(self)
    }

    /// build 后按 ABI v3 编码，返回可直接落盘的字节（CRC / graph_id 由 khaen_core 生成）。
    ///
    /// 相同输入必得相同字节；失败返回与 build 相同的错误。
    pub fn build_bytes(&self) -> Result<Vec<u8>, PlanBuildError> {
        self.build().map(|plan| plan.encode())
    }
}

fn identity_matrix(lanes: u16) -> MatrixSection {
    let count = usize::from(lanes);
    let mut coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        coeffs[lane * count + lane] = 1.0;
    }
    MatrixSection {
        rows: lanes,
        cols: lanes,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs,
    }
}

fn identity_protection(lanes: u16) -> ProtectionSection {
    ProtectionSection {
        lanes,
        entries: (0..lanes)
            .map(|lane| ProtectionEntry {
                lane,
                delay: 0,
                polarity: 1,
                gain_db: 0.0,
                peq: Vec::new(),
            })
            .collect(),
    }
}
