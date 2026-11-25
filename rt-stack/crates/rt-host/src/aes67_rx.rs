//! [NONRT] 自研用户态 AES67 接收端 → host capture 输入的**最小接线适配**（task-235）。
//!
//! 与 T43 \`aoip.rs\`（AES67/Inferno 端点接入，复用外部 daemon/ALSA 卡）不同：本模块接入
//! **自研用户态接收端** \`tspi-aoip-aes67\`（SDP/RTP L24/抖动缓冲/capture 固定块），把
//! 固定 128 帧块转成 host 图的 **planar f32** 输入，并复用 T43 的 H16 容器/角色映射语义
//! （\`H16FlowConfig\`/\`H16Container\`/\`plan_role_selection\`，禁止静默丢弃/静默零填）。
//!
//! 16ch（NSP 2x8ch AES67 Phase 1/1.5）：图输入宽度 16 = H16 Container16 的 16 个 capture
//! 通道（8+8）。前 12 个路径是按 H16 逻辑角色的显式映射（RX_ROLES_16 只用于角色覆盖校验）；
//! Lw/Rw/Aux12/Aux13 四个非逻辑容器通道在 16ch 全容器布局下作为 aux 数据通道**整块透传**
//! 到图输入（不占 H16 逻辑角色，plan 用 aux role 消费），因此 capture_index = 0..15 恒等映射。
//! 8ch/2ch 路径仍是角色映射 + strict 空槽零填。report_json 附带每图通道 RMS/峰值电平表
//! （NONRT 证据：16ch 全通道独立信号验证，不参与 RT 决策）。
//!
//! 边界（与 task-197 \`capture.source=aes67\` 占位对齐）：
//! - 本模块只提供**库级适配**（bind/read/fill_block/report）；host 入口（\`main.rs\`
//!   \`run\` 的 capture source 分支）仍是 RAVENNA ALSA 卡路径；把 userspace 传输作为
//!   \`capture.source=aes67\` 的第二传输接进 \`run\` 需要新增配置项并归 T26/T74 串行接线，
//!   本任务不修改 host bin **NOT_WIRED**；
//! - 真实网卡/组播/IGMP/PTP/电平/听音 **NOT_RUN**；离线 loopback 由
//!   \`tspi-aoip-aes67\` 的集成测试覆盖，本模块测试用确定性 jitter reader 覆盖映射。
//!
//! 失流语义（T43 对齐）：任一 flow 首个样本未到/欠载 → **整块静音** 并计数（禁止半块
//! 输出、禁止拿另一条 flow 数据顶替）；持续失流/端点错误的上层 fail-closed 状态机归
//! T43/T26（本模块只返回 \`BlockStatus\`）。

use tspi_aoip_aes67::capture::CaptureBlockSource;
use tspi_aoip_aes67::clock::{frames_to_ns_ceil, MediaClock, UsrvClock};
use tspi_aoip_aes67::jitter::{BlockStatus, JitterReader, JitterStats};
use tspi_aoip_aes67::receiver::{Aes67Receiver, ReceiverConfig, ReceiverStats};
use tspi_aoip_aes67::rtp;

use crate::aoip::{self, H16Container, H16FlowConfig, H16Role, NonLogicalPolicy, RoleSelection};

/// host capture 块长（task-197/T43 固定 128 帧）。
pub const AES67_RX_BLOCK_FRAMES: usize = 128;
/// 2ch 图输入 strict 布局名（与 file/AoIP capture 同口径）。
pub const AES67_RX_LAYOUT_STEREO: &str = "2.0-strict";
/// 8ch 图输入 strict 布局名。
pub const AES67_RX_LAYOUT_412: &str = "4.1.2-strict";
/// 16ch 图输入布局名（H16 Container16 8+8：图通道 = 容器 capture 顺序 = H16 slot 0..15）。
pub const AES67_RX_LAYOUT_16: &str = "16ch-h16";
/// NSP 单流 5.1.2：图输入就是线序 L R C LFE Ls Rs Ltf Rtf，不按 4.1.2 角色重排。
pub const AES67_RX_LAYOUT_512_CONTENT: &str = "5.1.2-content-order";
/// 图输入最大通道数（host 接 2/8/16；16 = H16 Container16 8+8，与 main.rs 的
/// capture_layout_roles 一致）。
pub const AES67_RX_MAX_GRAPH_CHANNELS: usize = 16;
/// 每流最大通道数（AES67 媒体面）。
pub const AES67_RX_MAX_FLOW_CHANNELS: usize = 8;

/// 2ch 图输入 → H16 角色（FL/FR）。
const RX_ROLES_2CH: [Option<H16Role>; 2] = [Some(H16Role::FrontLeft), Some(H16Role::FrontRight)];
/// 8ch 4.1.2 图输入 → H16 角色（FL FR SL SR TFL TFR 空 LFE；与 task-197 一致）。
const RX_ROLES_412: [Option<H16Role>; 8] = [
    Some(H16Role::FrontLeft),
    Some(H16Role::FrontRight),
    Some(H16Role::SurroundLeft),
    Some(H16Role::SurroundRight),
    Some(H16Role::TopFrontLeft),
    Some(H16Role::TopFrontRight),
    None,
    Some(H16Role::Lfe),
];
/// 16ch H16 Container16 图输入 → H16 角色（图通道 = 容器 capture 顺序 = H16 slot 0..15）。
///
/// slot 8/9（Lw/Rw）与 12/13（Aux12/Aux13）在 H16 逻辑上无角色（Phase 1 显式零填）；
/// 12 个逻辑角色必须全部映射（plan_role_selection 禁止静默丢弃/静默零填）。
const RX_ROLES_16: [Option<H16Role>; 16] = [
    Some(H16Role::FrontLeft),
    Some(H16Role::FrontRight),
    Some(H16Role::Center),
    Some(H16Role::Lfe),
    Some(H16Role::SurroundLeft),
    Some(H16Role::SurroundRight),
    Some(H16Role::BackLeft),
    Some(H16Role::BackRight),
    None, // Lw
    None, // Rw
    Some(H16Role::TopFrontLeft),
    Some(H16Role::TopFrontRight),
    None, // Aux12
    None, // Aux13
    Some(H16Role::TopRearLeft),
    Some(H16Role::TopRearRight),
];

/// 接收 flow 组合计划：决定容器通道顺序与逻辑角色（复用 T43 H16 映射）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aes67RxFlowPlan {
    /// H16 7.1.4：两条 flow 8+4 → 12ch 容器（全部有逻辑角色）。
    Logical12,
    /// H16 16ch：两条 flow 8+8 → 16ch 容器（capture 8/9/12/13 = Lw/Rw/空/空）。
    Container16,
    /// 显式 H16 slot 列表（自定义 flow 组成；如单 flow 2ch = slots [0,1]）。
    Custom(H16FlowConfig),
}

impl Aes67RxFlowPlan {
    /// 展开为 T43 flow 配置（容器/角色映射的唯一来源）。
    pub fn flow_config(self) -> H16FlowConfig {
        match self {
            Aes67RxFlowPlan::Logical12 => H16FlowConfig::logical12(),
            Aes67RxFlowPlan::Container16 => H16FlowConfig::container16(),
            Aes67RxFlowPlan::Custom(config) => config,
        }
    }
}

/// 适配配置（图输入宽度 + flow 组合计划）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67RxAdapter {
    /// flow 组合计划（flow 数/每 flow 通道数由此决定）。
    pub flow_plan: Aes67RxFlowPlan,
    /// host 图输入宽度（2=2.0-strict / 8=4.1.2-strict）。
    pub graph_channels: u16,
    /// true：单流 8ch 按线序写入图（NSP 5.1.2）。跳过 4.1.2 角色选择。
    pub content_order: bool,
}

impl Aes67RxAdapter {
    /// 构造并校验（图输入只允许 2/8/16；角色映射必须能显式覆盖容器角色）。
    pub fn new(flow_plan: Aes67RxFlowPlan, graph_channels: u16) -> Result<Self, String> {
        let adapter = Self {
            flow_plan,
            graph_channels,
            content_order: false,
        };
        adapter.validate()?;
        Ok(adapter)
    }

    /// NSP 5.1.2 单流 8ch：图输入保持线序，不走 4.1.2 角色表。
    pub fn new_content_order(
        flow_plan: Aes67RxFlowPlan,
        graph_channels: u16,
    ) -> Result<Self, String> {
        let adapter = Self {
            flow_plan,
            graph_channels,
            content_order: true,
        };
        adapter.validate()?;
        Ok(adapter)
    }

    /// 校验 flow 计划 + 图宽度 + 角色映射（不打开 socket）。
    pub fn validate(&self) -> Result<(), String> {
        let config = self.flow_plan.flow_config();
        if self.content_order {
            if self.graph_channels != 8 {
                return Err(aes67rx_error(
                    "ERR_AES67RX_LAYOUT",
                    format!(
                        "5.1.2 内容顺序只接受 8 通道图输入（got {}）",
                        self.graph_channels
                    ),
                ));
            }
            if config.flow_count() != 1 || config.flow_len(0) != Some(8) {
                return Err(aes67rx_error(
                    "ERR_AES67RX_PLAN",
                    format!(
                        "5.1.2 内容顺序只接受一条 8 通道 flow（got {} 条 / {:?} ch）",
                        config.flow_count(),
                        config.flow_len(0)
                    ),
                ));
            }
            H16Container::from_flow_config(&config).map_err(|error| {
                aes67rx_error(
                    "ERR_AES67RX_PLAN",
                    format!("H16 flow 配置非法：{}", error.name()),
                )
            })?;
            return Ok(());
        }
        let container = H16Container::from_flow_config(&config).map_err(|error| {
            aes67rx_error(
                "ERR_AES67RX_PLAN",
                format!("H16 flow 配置非法：{}", error.name()),
            )
        })?;
        let (_, roles) = rx_layout_roles(self.graph_channels as usize)?;
        let mut selected = RoleSelection::NONE;
        for role in roles.iter().flatten() {
            selected = selected.with(*role);
        }
        let ignored = RoleSelection::from_mask(container.logical_present_mask() & !selected.mask());
        aoip::plan_role_selection(
            &container,
            selected,
            ignored,
            NonLogicalPolicy::ExplicitlyIgnore,
        )
        .map_err(|error| {
            aes67rx_error(
                "ERR_AES67RX_ROLES",
                format!(
                    "AES67 RX 角色映射不可接受（图 {}ch）：{}（容器 {}ch，flow {} 条）",
                    self.graph_channels,
                    error.name(),
                    container.capture_channels(),
                    config.flow_count()
                ),
            )
        })?;
        Ok(())
    }

    /// flow 数。
    pub fn flow_count(&self) -> usize {
        self.flow_plan.flow_config().flow_count()
    }

    /// 第 index 条 flow 通道数。
    pub fn flow_channels(&self, index: usize) -> Option<u16> {
        self.flow_plan.flow_config().flow_len(index).map(u16::from)
    }

    /// 容器通道数。
    pub fn container_channels(&self) -> Result<u16, String> {
        let config = self.flow_plan.flow_config();
        H16Container::from_flow_config(&config)
            .map(|container| container.capture_channels())
            .map_err(|error| {
                aes67rx_error(
                    "ERR_AES67RX_PLAN",
                    format!("H16 flow 配置非法：{}", error.name()),
                )
            })
    }
}

/// 图宽度 → (strict 布局名, 角色表)（与 host bin 的 capture 严格布局同口径）。
fn rx_layout_roles(
    graph_channels: usize,
) -> Result<(&'static str, &'static [Option<H16Role>]), String> {
    match graph_channels {
        2 => Ok((AES67_RX_LAYOUT_STEREO, &RX_ROLES_2CH)),
        8 => Ok((AES67_RX_LAYOUT_412, &RX_ROLES_412)),
        // 16 = H16 Container16 8+8（NSP 16ch Phase 1）；只有 12 个逻辑角色，4 个非逻辑槽
        // 显式 None 零填（plan_role_selection 要求显式忽略）。
        16 => Ok((AES67_RX_LAYOUT_16, &RX_ROLES_16)),
        other => Err(aes67rx_error(
            "ERR_AES67RX_LAYOUT",
            format!("AES67 RX 图输入只支持 strict 2/8/16ch（got {other}；不做隐式上/下混）"),
        )),
    }
}

/// 稳定错误文本（前缀码；记录到 host 日志/证据；非 RT 路径）。
pub fn aes67rx_error(code: &str, detail: String) -> String {
    format!("{code}: {detail}")
}

/// 适配层计数（单 consumer 线程普通字段；非阻塞）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Aes67RxCounters {
    /// \`fill_block\` 调用次数。
    pub blocks: u64,
    /// 正常输出块数。
    pub ok_blocks: u64,
    /// 预热（首包未到）整块静音数。
    pub warmup_blocks: u64,
    /// 欠载整块静音数。
    pub underrun_blocks: u64,
    /// 时钟未到期块数。
    pub not_due_blocks: u64,
    /// 交付 host 图的帧数（仅 Ok 块计入）。
    pub frames_delivered: u64,
    /// 合成媒体时钟路径（fill_block_paced）调用次数。
    pub paced_blocks: u64,
    /// 合成时钟预滚静音块数（抖动缓冲水位 < 目标延迟；不消费媒体帧）。
    pub preroll_blocks: u64,
    /// 合成时钟因 origin/epoch 变化重置的次数（内部 reanchor / 新会话）。
    pub clock_resets: u64,
}

/// 单条 flow 的静态描述（报告/证据用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67RxFlowInfo {
    /// flow 序号。
    pub index: u8,
    /// 通道数。
    pub channels: u16,
    /// 端口。
    pub port: u16,
    /// 是否组播。
    pub multicast: bool,
}

/// 单条 flow 的合成媒体时钟状态（播放路径；与 standalone aes67-rx 同口径）。
///
/// - `origin_ns` = 抖动缓冲首包锚点（writer 在 `anchor/reanchor` 时更新）；
/// - `epoch` = 抖动缓冲流纪元（reanchor 时 +1）；
/// - `delivered_frames` = 本 flow 已交付/已越过的媒体帧数（合成时基的唯一来源）；
/// - `prerolled` = 是否已完成"水位 ≥ 目标延迟"的预滚（预滚前只出静音、不消费）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FlowMediaClock {
    origin_ns: Option<u64>,
    epoch: u32,
    delivered_frames: u64,
    latency_frames: u64,
    prerolled: bool,
}

/// [NONRT 证据] 环境变量门控的 capture 输入 dump（16ch 全通道 FFT/RMS 复核用）。
///
/// 设置 TSPI_AES67_RX_DUMP=<prefix> 后，把每个 Ok 块的图输入按帧交错 f32 追加到
/// 预分配内存（容量由 TSPI_AES67_RX_DUMP_SECONDS 控制，默认 5 s，48 kHz），退出时
/// 写 <prefix>.f32 + <prefix>.json。块循环内只做 memcpy，无分配；默认关闭，不参与
/// 任何 RT 决策与生产配置。
struct Aes67RxDump {
    prefix: std::path::PathBuf,
    capacity_frames: usize,
    frames: usize,
    channels: usize,
    buffer: Vec<f32>,
}

impl Aes67RxDump {
    fn from_env(channels: usize, sample_rate: u32) -> Option<Self> {
        let prefix = std::env::var_os("TSPI_AES67_RX_DUMP")?;
        if prefix.is_empty() {
            return None;
        }
        let seconds = std::env::var("TSPI_AES67_RX_DUMP_SECONDS")
            .ok()
            .and_then(|text| text.parse::<u32>().ok())
            .unwrap_or(5)
            .clamp(1, 60);
        let capacity_frames = (seconds as usize) * sample_rate as usize;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(capacity_frames.saturating_mul(channels))
            .ok()?;
        Some(Self {
            prefix: std::path::PathBuf::from(prefix),
            capacity_frames,
            frames: 0,
            channels,
            buffer,
        })
    }

    fn push_block(&mut self, dst: &[f32], stride: usize, channels: usize, block_frames: usize) {
        if self.frames >= self.capacity_frames || block_frames == 0 {
            return;
        }
        // Only the current block (block_frames), truncated to the remaining dump
        // capacity - never the whole remaining capacity (that would read past dst).
        let take = block_frames.min(self.capacity_frames - self.frames);
        for frame in 0..take {
            for slot in 0..channels {
                self.buffer.push(dst[slot * stride + frame]);
            }
        }
        self.frames += take;
    }

    fn report_json(&self) -> String {
        // JSON must not contain raw Windows backslashes in a string value.
        let path = self.prefix.display().to_string().replace('\\', "/");
        format!(
            "{{\"path\":\"{}\",\"frames\":{},\"channels\":{},\"sample_rate\":48000}}",
            path, self.frames, self.channels
        )
    }

    fn write_files(&self) {
        if self.frames == 0 {
            return;
        }
        let raw_path = self.prefix.with_extension("f32");
        let json_path = self.prefix.with_extension("json");
        let mut bytes = Vec::with_capacity(self.buffer.len() * 4);
        for value in &self.buffer {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        if std::fs::write(&raw_path, &bytes).is_err() {
            return;
        }
        let meta = format!(
            "{{\"format\":\"f32le frame-interleaved\",\"channels\":{},\"frames\":{},\"sample_rate\":48000,\"layout\":\"16ch-h16\"}}\n",
            self.channels, self.frames
        );
        let _ = std::fs::write(json_path, meta);
    }
}

impl Drop for Aes67RxDump {
    fn drop(&mut self) {
        self.write_files();
    }
}

/// 自研用户态 AES67 接收端 → host capture 适配器。
pub struct Aes67RxCapture {
    sources: Vec<CaptureBlockSource>,
    receivers: Vec<Aes67Receiver>,
    adapter: Aes67RxAdapter,
    container: H16Container,
    flow_lens: Vec<usize>,
    capture_index: [Option<u16>; AES67_RX_MAX_GRAPH_CHANNELS],
    graph_channels: usize,
    layout: &'static str,
    /// 每条 flow 的 i32 交错暂存（启动期一次分配；块循环只复用）。
    flow_staging: Vec<Vec<i32>>,
    /// 容器通道交错暂存（启动期一次分配）。
    container_staging: Vec<i32>,
    /// 每条 flow 的合成媒体时钟状态（播放路径 `fill_block_paced`）。
    flow_clocks: Vec<FlowMediaClock>,
    /// 每条 flow 本块的合成读数时刻（ns；启动期一次分配，热路径只复用）。
    flow_now: Vec<u64>,
    counters: Aes67RxCounters,
    /// 图通道 RMS/峰值电平表（NONRT 证据：16ch 全通道独立信号验证；仅累计 Ok 块）。
    channel_sumsq: [f64; AES67_RX_MAX_GRAPH_CHANNELS],
    channel_peak: [f32; AES67_RX_MAX_GRAPH_CHANNELS],
    meter_frames: u64,
    clock: UsrvClock,
    flow_info: Vec<Aes67RxFlowInfo>,
    local_addrs: Vec<std::net::SocketAddr>,
    /// [NONRT 证据] 可选的 capture 输入 dump（默认 None；环境变量门控）。
    dump: Option<Aes67RxDump>,
}

impl std::fmt::Debug for Aes67RxCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aes67RxCapture")
            .field("adapter", &self.adapter)
            .field("layout", &self.layout)
            .field("container_channels", &self.container.capture_channels())
            .field("graph_channels", &self.graph_channels)
            .field("flow_lens", &self.flow_lens)
            .field("counters", &self.counters)
            .finish()
    }
}

impl Aes67RxCapture {
    /// 绑定全部 flow 的 UDP 接收端（组播/单播）并构造 capture 适配器。
    ///
    /// \`flow_receivers\` 顺序必须与 flow 计划一致（每条 flow 一个 UDP socket/jitter 环）；
    /// 每条 \`ReceiverConfig.channels\` 必须等于该 flow 的通道数。启动期一次性分配。
    pub fn bind(
        flow_receivers: &[ReceiverConfig],
        adapter: Aes67RxAdapter,
    ) -> Result<Self, String> {
        adapter.validate()?;
        let config = adapter.flow_plan.flow_config();
        if flow_receivers.len() != config.flow_count() {
            return Err(aes67rx_error(
                "ERR_AES67RX_PLAN",
                format!(
                    "flow 数不符：配置 {} 条 vs 计划 {} 条",
                    flow_receivers.len(),
                    config.flow_count()
                ),
            ));
        }
        let mut receivers = Vec::with_capacity(flow_receivers.len());
        let mut readers = Vec::with_capacity(flow_receivers.len());
        let mut flow_info = Vec::with_capacity(flow_receivers.len());
        for (index, receiver_config) in flow_receivers.iter().enumerate() {
            let expected = adapter.flow_channels(index).ok_or_else(|| {
                aes67rx_error("ERR_AES67RX_PLAN", format!("flow {index} 不在计划内"))
            })?;
            if receiver_config.channels != expected {
                return Err(aes67rx_error(
                    "ERR_AES67RX_PLAN",
                    format!(
                        "flow {index} 通道数 {} != 计划 {expected}",
                        receiver_config.channels
                    ),
                ));
            }
            let mut receiver = Aes67Receiver::bind(*receiver_config).map_err(|error| {
                aes67rx_error(
                    "ERR_AES67RX_SOCKET",
                    format!("flow {index} 接收端绑定失败：{}", error.name()),
                )
            })?;
            let reader = receiver.take_reader().ok_or_else(|| {
                aes67rx_error(
                    "ERR_AES67RX_CONFIG",
                    format!("flow {index} jitter reader 已取出"),
                )
            })?;
            if reader.channels() != expected as usize
                || reader.block_frames() != AES67_RX_BLOCK_FRAMES
            {
                return Err(aes67rx_error(
                    "ERR_AES67RX_CONFIG",
                    format!(
                        "flow {index} jitter 布局不符（channels {} block {}）",
                        reader.channels(),
                        reader.block_frames()
                    ),
                ));
            }
            flow_info.push(Aes67RxFlowInfo {
                index: index as u8,
                channels: receiver_config.channels,
                port: receiver_config.port,
                multicast: receiver_config.multicast,
            });
            readers.push(reader);
            receivers.push(receiver);
        }
        let local_addrs = receivers
            .iter()
            .map(|receiver| receiver.local_addr())
            .collect();
        let mut capture = Self::from_readers(readers, adapter)?;
        capture.flow_info = flow_info;
        capture.local_addrs = local_addrs;
        capture.receivers = receivers;
        Ok(capture)
    }

    /// 由已就绪的 jitter reader 构造（测试/嵌入式；不打开 socket）。
    pub fn from_readers(
        readers: Vec<JitterReader>,
        adapter: Aes67RxAdapter,
    ) -> Result<Self, String> {
        adapter.validate()?;
        let config = adapter.flow_plan.flow_config();
        if readers.len() != config.flow_count() {
            return Err(aes67rx_error(
                "ERR_AES67RX_PLAN",
                format!(
                    "flow 数不符：reader {} 条 vs 计划 {} 条",
                    readers.len(),
                    config.flow_count()
                ),
            ));
        }
        let container = H16Container::from_flow_config(&config).map_err(|error| {
            aes67rx_error(
                "ERR_AES67RX_PLAN",
                format!("H16 flow 配置非法：{}", error.name()),
            )
        })?;
        let graph_channels = adapter.graph_channels as usize;
        let (layout, capture_index) = if adapter.content_order {
            let mut capture_index = [None; AES67_RX_MAX_GRAPH_CHANNELS];
            for slot in 0..graph_channels {
                capture_index[slot] = Some(slot as u16);
            }
            (AES67_RX_LAYOUT_512_CONTENT, capture_index)
        } else {
            let (layout, roles) = rx_layout_roles(graph_channels)?;
            let mut selected = RoleSelection::NONE;
            for role in roles.iter().flatten() {
                selected = selected.with(*role);
            }
            let ignored =
                RoleSelection::from_mask(container.logical_present_mask() & !selected.mask());
            let plan = aoip::plan_role_selection(
                &container,
                selected,
                ignored,
                NonLogicalPolicy::ExplicitlyIgnore,
            )
            .map_err(|error| {
                aes67rx_error(
                    "ERR_AES67RX_ROLES",
                    format!("AES67 RX 角色映射不可接受：{}", error.name()),
                )
            })?;
            let mut capture_index = [None; AES67_RX_MAX_GRAPH_CHANNELS];
            for (slot, role) in roles.iter().enumerate() {
                capture_index[slot] = match role {
                    Some(role) => Some(plan.capture_index(*role).ok_or_else(|| {
                        aes67rx_error("ERR_AES67RX_ROLES", format!("角色 {role:?} 未映射"))
                    })?),
                    None => None,
                };
            }
            // 16ch 全容器布局（图 16 = H16 Container16 8+8）：图输入就是容器 capture 顺序，
            // 非逻辑容器通道（Lw/Rw/Aux12/Aux13）作为 aux 数据透传到图输入，由 plan 的 aux
            // role 显式消费（NSP 16ch 验收：16 通道全部到达 host；不再静默零填）。角色覆盖校验
            // 仍由 RX_ROLES_16 的 12 个逻辑角色保证（非逻辑通道按 ExplicitlyIgnore 计数）。
            // 只限图宽 16。8ch 显式 slot 的容器宽也是 8，若按“图宽==容器宽”恒等覆盖，
            // 会把 4.1.2 的空槽（图 ch6）和角色重排抹成 0..7。
            if graph_channels == 16 && graph_channels == container.capture_channels() as usize {
                for (slot, capture) in capture_index[..graph_channels].iter_mut().enumerate() {
                    *capture = Some(slot as u16);
                }
            }
            (layout, capture_index)
        };
        let block = AES67_RX_BLOCK_FRAMES;
        let container_channels = container.capture_channels() as usize;
        let mut sources = Vec::with_capacity(readers.len());
        let mut flow_lens = Vec::with_capacity(readers.len());
        for (index, reader) in readers.into_iter().enumerate() {
            let expected = config.flow_len(index).ok_or_else(|| {
                aes67rx_error("ERR_AES67RX_PLAN", format!("flow {index} 不在计划内"))
            })? as usize;
            if reader.channels() != expected || reader.block_frames() != block {
                return Err(aes67rx_error(
                    "ERR_AES67RX_CONFIG",
                    format!(
                        "flow {index} jitter 布局不符（channels {} block {}；期望 {expected}/{block}）",
                        reader.channels(),
                        reader.block_frames()
                    ),
                ));
            }
            flow_lens.push(expected);
            sources.push(CaptureBlockSource::new(reader).map_err(|error| {
                aes67rx_error(
                    "ERR_AES67RX_JITTER",
                    format!("flow {index} capture source 构造失败：{}", error.name()),
                )
            })?);
        }
        let mut flow_staging = Vec::with_capacity(flow_lens.len());
        for channels in &flow_lens {
            flow_staging.push(vec![0i32; block * channels]);
        }
        // 合成媒体时钟：每条 flow 一个（目标延迟取自该 flow 的 jitter reader）。
        let mut flow_clocks = Vec::with_capacity(sources.len());
        for source in &sources {
            flow_clocks.push(FlowMediaClock {
                origin_ns: None,
                epoch: 0,
                delivered_frames: 0,
                latency_frames: source.latency_frames(),
                prerolled: false,
            });
        }
        let flow_now = vec![0u64; sources.len()];
        Ok(Self {
            sources,
            receivers: Vec::new(),
            adapter,
            container,
            flow_lens,
            capture_index,
            graph_channels,
            layout,
            flow_staging,
            container_staging: vec![0i32; block * container_channels],
            flow_clocks,
            flow_now,
            counters: Aes67RxCounters::default(),
            channel_sumsq: [0.0; AES67_RX_MAX_GRAPH_CHANNELS],
            channel_peak: [0.0; AES67_RX_MAX_GRAPH_CHANNELS],
            meter_frames: 0,
            clock: UsrvClock::new(tspi_aoip_aes67::AES67_RATE_HZ),
            flow_info: Vec::new(),
            local_addrs: Vec::new(),
            dump: Aes67RxDump::from_env(graph_channels, tspi_aoip_aes67::AES67_RATE_HZ),
        })
    }

    /// 图输入布局名。
    pub fn layout(&self) -> &'static str {
        self.layout
    }

    /// 图输入通道数。
    pub fn graph_channels(&self) -> usize {
        self.graph_channels
    }

    /// 容器通道数。
    pub fn container_channels(&self) -> usize {
        self.container.capture_channels() as usize
    }

    /// flow 数。
    pub fn flow_count(&self) -> usize {
        self.sources.len()
    }

    /// flow 静态描述（bind 路径；from_readers 为空）。
    pub fn flow_info(&self) -> &[Aes67RxFlowInfo] {
        &self.flow_info
    }

    /// 本地绑定地址（bind 路径）。
    pub fn local_addrs(&self) -> &[std::net::SocketAddr] {
        &self.local_addrs
    }

    /// capture 计数快照。
    pub fn counters(&self) -> Aes67RxCounters {
        self.counters
    }

    /// 每条 flow 的抖动缓冲统计。
    pub fn jitter_stats(&self) -> Vec<JitterStats> {
        self.sources
            .iter()
            .map(|source| source.jitter_stats())
            .collect()
    }

    /// 每条 flow 的接收端统计（bind 路径；from_readers 无 socket 统计）。
    pub fn receiver_stats(&self) -> Vec<ReceiverStats> {
        self.receivers
            .iter()
            .map(|receiver| receiver.stats())
            .collect()
    }

    /// 图通道 slot → 容器 capture index（None = strict 空槽，显式零填）。
    pub fn capture_index(&self) -> &[Option<u16>] {
        &self.capture_index[..self.graph_channels]
    }

    /// 用内部用户态时钟读一个块（host 入口便捷路径）。
    pub fn fill_block(
        &mut self,
        dst: &mut [f32],
        stride: usize,
        block: usize,
        now_ns: u64,
    ) -> Result<BlockStatus, String> {
        let graph_channels = self.graph_channels;
        if block != AES67_RX_BLOCK_FRAMES {
            return Err(aes67rx_error(
                "ERR_AES67RX_BLOCK",
                format!("capture 块长必须是 {AES67_RX_BLOCK_FRAMES}（got {block}）"),
            ));
        }
        if stride < block || dst.len() < graph_channels * stride {
            return Err(aes67rx_error(
                "ERR_AES67RX_BLOCK",
                format!(
                    "planar 缓冲不足（dst {} stride {stride} block {block} channels {graph_channels}）",
                    dst.len()
                ),
            ));
        }
        // 1) 每条 flow 取整块（任一 flow 不 Ok → 整块静音，T43 失流语义）。
        let mut worst = BlockStatus::Ok;
        let Self {
            sources,
            flow_staging,
            flow_lens,
            channel_sumsq: sumsq,
            channel_peak: peak,
            meter_frames,
            ..
        } = self;
        for (index, source) in sources.iter_mut().enumerate() {
            let required = AES67_RX_BLOCK_FRAMES * flow_lens[index];
            let read = source
                .read_block(now_ns, &mut flow_staging[index][..required])
                .map_err(|error| {
                    aes67rx_error(
                        "ERR_AES67RX_JITTER",
                        format!("flow {index} 读块失败：{}", error.name()),
                    )
                })?;
            if block_status_rank(read.status) > block_status_rank(worst) {
                worst = read.status;
            }
        }
        // 2) 汇总容器交错样本（flow 顺序拼接 = H16Container capture 顺序）。
        let container_channels = self.container.capture_channels() as usize;
        if worst == BlockStatus::Ok {
            let mut channel_offset = 0usize;
            for (index, channels) in self.flow_lens.iter().enumerate() {
                for frame in 0..AES67_RX_BLOCK_FRAMES {
                    let dst_base = (frame * container_channels + channel_offset) * 1;
                    let src_base = frame * channels;
                    self.container_staging[dst_base..dst_base + channels]
                        .copy_from_slice(&self.flow_staging[index][src_base..src_base + channels]);
                }
                channel_offset += channels;
            }
            // 3) 容器 → host 图 planar f32（显式角色映射；空槽显式 0.0）。
            for (slot, capture) in self.capture_index[..graph_channels].iter().enumerate() {
                match capture {
                    Some(container_index) => {
                        for frame in 0..AES67_RX_BLOCK_FRAMES {
                            let sample = self.container_staging
                                [frame * container_channels + *container_index as usize];
                            dst[slot * stride + frame] = rtp::i32_full_scale_to_f32(sample);
                        }
                    }
                    None => {
                        dst[slot * stride..slot * stride + AES67_RX_BLOCK_FRAMES].fill(0.0);
                    }
                }
            }
            // 图通道电平表（NONRT 证据：16ch 全通道独立信号/RMS；仅累计 Ok 块）。
            for slot in 0..graph_channels {
                for frame in 0..AES67_RX_BLOCK_FRAMES {
                    let value = dst[slot * stride + frame];
                    let square = f64::from(value) * f64::from(value);
                    sumsq[slot] += square;
                    let magnitude = value.abs();
                    if magnitude > peak[slot] {
                        peak[slot] = magnitude;
                    }
                }
            }
            *meter_frames = meter_frames.saturating_add(AES67_RX_BLOCK_FRAMES as u64);
            if let Some(dump) = self.dump.as_mut() {
                dump.push_block(dst, stride, graph_channels, AES67_RX_BLOCK_FRAMES);
            }
            self.counters.blocks += 1;
            self.counters.ok_blocks += 1;
            self.counters.frames_delivered += AES67_RX_BLOCK_FRAMES as u64;
            Ok(BlockStatus::Ok)
        } else {
            for slot in 0..graph_channels {
                dst[slot * stride..slot * stride + AES67_RX_BLOCK_FRAMES].fill(0.0);
            }
            self.counters.blocks += 1;
            match worst {
                BlockStatus::Warmup => self.counters.warmup_blocks += 1,
                BlockStatus::Underrun => self.counters.underrun_blocks += 1,
                BlockStatus::NotDue => self.counters.not_due_blocks += 1,
                BlockStatus::Ok => {}
            }
            Ok(worst)
        }
    }

    /// 用内部 \`UsrvClock\` 读一个块（host 块循环入口；热路径无分配）。
    pub fn fill_block_with_clock(
        &mut self,
        dst: &mut [f32],
        stride: usize,
        block: usize,
    ) -> Result<BlockStatus, String> {
        let now_ns = self.clock.now_ns();
        self.fill_block(dst, stride, block, now_ns)
    }

    /// 播放路径：用**合成媒体时钟**读一个固定块（对齐 standalone `tools/aes67-rx`）。
    ///
    /// 与 `fill_block(now_ns)` 的区别（T239-crackle 修复）：媒体 target 不再由墙钟推导，
    /// 而是 `origin_ns + frames_to_ns_ceil(latency_frames + delivered_frames)`，其中
    /// `delivered_frames` 由本函数按"已交付块数"推进。这样 ALSA duplex/USB 节拍量化、
    /// 设备钟 vs 墙钟漂移都不会变成 NotDue（静音插块）或 resync（丢块）。
    ///
    /// - 未锚定 / 预滚水位 < 目标延迟：整块静音（Warmup），不消费媒体帧；
    /// - Ok/Underrun：`delivered_frames += block`（欠载由 reader 写静音并推进游标，时钟不倒退）；
    /// - NotDue（仅 origin/epoch 竞争窗口）：`delivered_frames = read_frame()` 后写一块静音，
    ///   下一块自动对齐（绝不重复上一块、绝不半块）；
    /// - origin/epoch 变化（内部 reanchor / 新会话）：重置该 flow 的 delivered/preroll。
    pub fn fill_block_paced(
        &mut self,
        dst: &mut [f32],
        stride: usize,
        block: usize,
    ) -> Result<BlockStatus, String> {
        let graph_channels = self.graph_channels;
        if block != AES67_RX_BLOCK_FRAMES {
            return Err(aes67rx_error(
                "ERR_AES67RX_BLOCK",
                format!("capture 块长必须是 {AES67_RX_BLOCK_FRAMES}（got {block}）"),
            ));
        }
        if stride < block || dst.len() < graph_channels * stride {
            return Err(aes67rx_error(
                "ERR_AES67RX_BLOCK",
                format!(
                    "planar 缓冲不足（dst {} stride {stride} block {block} channels {graph_channels}）",
                    dst.len()
                ),
            ));
        }
        self.counters.paced_blocks = self.counters.paced_blocks.saturating_add(1);
        // 1) 每条 flow：同步 origin/epoch -> 预滚 -> 合成 now。
        let mut ready = true;
        let mut clock_reset = false;
        for index in 0..self.sources.len() {
            let source = &self.sources[index];
            let origin = match source.origin_ns() {
                Some(origin) => origin,
                None => {
                    ready = false;
                    break;
                }
            };
            let epoch = source.epoch();
            let flow_clock = &mut self.flow_clocks[index];
            if flow_clock.origin_ns != Some(origin) || flow_clock.epoch != epoch {
                // 首次同步（origin_ns=None）不算"重置"；只有已有时基后又变化（内部 reanchor /
                // 新会话）才计入 clock_resets，证据里可直接区分启动与真实重锚。
                let had_clock = flow_clock.origin_ns.is_some();
                flow_clock.origin_ns = Some(origin);
                flow_clock.epoch = epoch;
                flow_clock.delivered_frames = 0;
                flow_clock.prerolled = false;
                clock_reset = clock_reset || had_clock;
            }
            if !flow_clock.prerolled {
                if source.available_frames() < flow_clock.latency_frames {
                    ready = false;
                    break;
                }
                flow_clock.prerolled = true;
            }
            self.flow_now[index] = origin.saturating_add(frames_to_ns_ceil(
                flow_clock
                    .latency_frames
                    .saturating_add(flow_clock.delivered_frames),
                tspi_aoip_aes67::AES67_RATE_HZ,
            ));
        }
        if clock_reset {
            self.counters.clock_resets = self.counters.clock_resets.saturating_add(1);
        }
        if !ready {
            for slot in 0..graph_channels {
                dst[slot * stride..slot * stride + AES67_RX_BLOCK_FRAMES].fill(0.0);
            }
            self.counters.blocks = self.counters.blocks.saturating_add(1);
            self.counters.warmup_blocks = self.counters.warmup_blocks.saturating_add(1);
            self.counters.preroll_blocks = self.counters.preroll_blocks.saturating_add(1);
            return Ok(BlockStatus::Warmup);
        }
        // 2) 按各自合成 now 读全部 flow，并推进/对齐各 flow 的交付帧游标。
        let mut worst = BlockStatus::Ok;
        for index in 0..self.sources.len() {
            let required = AES67_RX_BLOCK_FRAMES * self.flow_lens[index];
            let now_ns = self.flow_now[index];
            let status = {
                let source = &mut self.sources[index];
                source
                    .read_block(now_ns, &mut self.flow_staging[index][..required])
                    .map_err(|error| {
                        aes67rx_error(
                            "ERR_AES67RX_JITTER",
                            format!("flow {index} 读块失败：{}", error.name()),
                        )
                    })?
                    .status
            };
            let flow_clock = &mut self.flow_clocks[index];
            match status {
                BlockStatus::Ok | BlockStatus::Underrun => {
                    flow_clock.delivered_frames = flow_clock
                        .delivered_frames
                        .saturating_add(AES67_RX_BLOCK_FRAMES as u64);
                }
                BlockStatus::NotDue => {
                    flow_clock.delivered_frames = self.sources[index].read_frame();
                }
                BlockStatus::Warmup => {
                    flow_clock.prerolled = false;
                }
            }
            if block_status_rank(status) > block_status_rank(worst) {
                worst = status;
            }
        }
        // 3) 汇总/映射（与 fill_block 同口径；任一 flow 非 Ok → 整块静音）。
        let Self {
            flow_staging,
            flow_lens,
            container_staging,
            capture_index,
            container,
            counters,
            channel_sumsq: sumsq,
            channel_peak: peak,
            meter_frames,
            dump,
            ..
        } = self;
        let container_channels = container.capture_channels() as usize;
        counters.blocks = counters.blocks.saturating_add(1);
        if worst == BlockStatus::Ok {
            let mut channel_offset = 0usize;
            for (index, channels) in flow_lens.iter().enumerate() {
                for frame in 0..AES67_RX_BLOCK_FRAMES {
                    let dst_base = frame * container_channels + channel_offset;
                    let src_base = frame * channels;
                    container_staging[dst_base..dst_base + channels]
                        .copy_from_slice(&flow_staging[index][src_base..src_base + channels]);
                }
                channel_offset += channels;
            }
            for (slot, capture) in capture_index[..graph_channels].iter().enumerate() {
                match capture {
                    Some(container_index) => {
                        for frame in 0..AES67_RX_BLOCK_FRAMES {
                            let sample = container_staging
                                [frame * container_channels + *container_index as usize];
                            dst[slot * stride + frame] = rtp::i32_full_scale_to_f32(sample);
                        }
                    }
                    None => {
                        dst[slot * stride..slot * stride + AES67_RX_BLOCK_FRAMES].fill(0.0);
                    }
                }
            }
            // 图通道电平表（NONRT 证据：16ch 全通道独立信号/RMS；仅累计 Ok 块）。
            for slot in 0..graph_channels {
                for frame in 0..AES67_RX_BLOCK_FRAMES {
                    let value = dst[slot * stride + frame];
                    let square = f64::from(value) * f64::from(value);
                    sumsq[slot] += square;
                    let magnitude = value.abs();
                    if magnitude > peak[slot] {
                        peak[slot] = magnitude;
                    }
                }
            }
            *meter_frames = meter_frames.saturating_add(AES67_RX_BLOCK_FRAMES as u64);
            if let Some(dump) = dump.as_mut() {
                dump.push_block(dst, stride, graph_channels, AES67_RX_BLOCK_FRAMES);
            }
            counters.ok_blocks = counters.ok_blocks.saturating_add(1);
            counters.frames_delivered = counters
                .frames_delivered
                .saturating_add(AES67_RX_BLOCK_FRAMES as u64);
            Ok(BlockStatus::Ok)
        } else {
            for slot in 0..graph_channels {
                dst[slot * stride..slot * stride + AES67_RX_BLOCK_FRAMES].fill(0.0);
            }
            match worst {
                BlockStatus::Warmup => {
                    counters.warmup_blocks = counters.warmup_blocks.saturating_add(1)
                }
                BlockStatus::Underrun => {
                    counters.underrun_blocks = counters.underrun_blocks.saturating_add(1)
                }
                BlockStatus::NotDue => {
                    counters.not_due_blocks = counters.not_due_blocks.saturating_add(1)
                }
                BlockStatus::Ok => {}
            }
            Ok(worst)
        }
    }

    /// 请求内部重锚（全部 flow 的接收端；下一个到达包作为新流首包）。
    /// 返回实际转发到的接收端数量（`from_readers` 测试路径无 socket = 0）。
    pub fn request_reanchor(&self) -> usize {
        for receiver in &self.receivers {
            receiver.request_reanchor();
        }
        self.receivers.len()
    }

    /// 接收端数量（`from_readers` 测试路径 = 0）。
    pub fn receiver_count(&self) -> usize {
        self.receivers.len()
    }

    /// [NONRT 诊断] 收包统计快照写入调用方预分配切片：`(datagrams, accepted_packets)`。
    /// 返回写入条数（≤ out.len() 且 ≤ flow 数）；热路径每 ~500ms 调用一次、零分配。
    pub fn io_stats_into(&self, out: &mut [(u64, u64)]) -> usize {
        let mut written = 0usize;
        for receiver in &self.receivers {
            if written >= out.len() {
                break;
            }
            let stats = receiver.io_stats();
            out[written] = (stats.datagrams, stats.accepted_packets);
            written += 1;
        }
        written
    }

    /// 停止全部接收端线程（幂等；非 RT 退出路径）。
    pub fn stop(&mut self) {
        for receiver in self.receivers.iter_mut() {
            receiver.stop();
        }
    }

    /// 证据 JSON（NONRT；收流后调用）。含适配配置/角色映射/计数/每 flow dev 统计。
    pub fn report_json(&self) -> String {
        let mut json = String::with_capacity(1024);
        use std::fmt::Write as _;
        let _ = write!(
            json,
            "{{\"proto\":\"tspi-rt-host/aes67-rx\",\"version\":\"{}\",\
\"layout\":\"{}\",\"graph_channels\":{},\"container_channels\":{},\
\"flow_count\":{},\"flow_lens\":[",
            tspi_aoip_aes67::PROTO_VERSION,
            self.layout,
            self.graph_channels,
            self.container.capture_channels(),
            self.sources.len()
        );
        for (index, channels) in self.flow_lens.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let _ = write!(json, "{channels}");
        }
        json.push_str("],\"capture_index\":[");
        for (index, capture) in self.capture_index[..self.graph_channels].iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            match capture {
                Some(value) => {
                    let _ = write!(json, "{value}");
                }
                None => json.push_str("null"),
            }
        }
        json.push_str("],\"counters\":{");
        let _ = write!(
            json,
            "\"blocks\":{},\"ok_blocks\":{},\"warmup_blocks\":{},\
\"underrun_blocks\":{},\"not_due_blocks\":{},\"frames_delivered\":{},\
\"paced_blocks\":{},\"preroll_blocks\":{},\"clock_resets\":{}",
            self.counters.blocks,
            self.counters.ok_blocks,
            self.counters.warmup_blocks,
            self.counters.underrun_blocks,
            self.counters.not_due_blocks,
            self.counters.frames_delivered,
            self.counters.paced_blocks,
            self.counters.preroll_blocks,
            self.counters.clock_resets
        );
        json.push_str("},\"channel_rms\":[");
        for (slot, sumsq) in self.channel_sumsq[..self.graph_channels].iter().enumerate() {
            if slot > 0 {
                json.push(',');
            }
            let frames = self.meter_frames.max(1) as f64;
            let _ = write!(json, "{:.6}", (*sumsq / frames).sqrt());
        }
        json.push_str("],\"channel_peak\":[");
        for (slot, peak) in self.channel_peak[..self.graph_channels].iter().enumerate() {
            if slot > 0 {
                json.push(',');
            }
            let _ = write!(json, "{:.6}", peak);
        }
        json.push_str("],\"meter_frames\":");
        let _ = write!(json, "{}", self.meter_frames);
        json.push_str(",\"jitter\":[");
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let stats = source.jitter_stats();
            let _ = write!(
                json,
                "{{\"flow\":{},\"packets\":{},\"frames_received\":{},\
\"frames_played\":{},\"underruns\":{},\"warmups\":{},\"overruns\":{},\
\"late_packets\":{},\"lost_packets\":{},\"timestamp_jumps\":{},\
\"stream_restarts\":{},\"high_water_frames\":{}}}",
                index,
                stats.packets,
                stats.frames_received,
                stats.frames_played,
                stats.underruns,
                stats.warmups,
                stats.overruns,
                stats.late_packets,
                stats.lost_packets,
                stats.timestamp_jumps,
                stats.stream_restarts,
                stats.high_water_frames
            );
        }
        // Cross-flow alignment evidence (NSP 16ch acceptance gate): both flows must
        // advance the same logical capture block. read_frame is the per-flow media
        // cursor; buffer frames is the jitter high-water. A growing skew or a
        // persistent buffer-depth difference is the FAIL signature for cross-flow
        // drift / one flow running alone.
        json.push_str("],\"flow_read_frames\":[");
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let _ = write!(json, "{}", source.read_frame());
        }
        json.push_str("],\"flow_buffer_frames\":[");
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let _ = write!(json, "{}", source.available_frames());
        }
        let mut skew = 0u64;
        let mut buffer_skew = 0u64;
        if let (Some(min_read), Some(max_read)) = (
            self.sources.iter().map(|s| s.read_frame()).min(),
            self.sources.iter().map(|s| s.read_frame()).max(),
        ) {
            skew = max_read - min_read;
        }
        if let (Some(min_buffer), Some(max_buffer)) = (
            self.sources.iter().map(|s| s.available_frames()).min(),
            self.sources.iter().map(|s| s.available_frames()).max(),
        ) {
            buffer_skew = max_buffer - min_buffer;
        }
        json.push_str("],\"flow_skew_frames\":");
        let _ = write!(json, "{skew}");
        json.push_str(",\"flow_buffer_skew_frames\":");
        let _ = write!(json, "{buffer_skew}");
        if let Some(dump) = self.dump.as_ref() {
            json.push_str(",\"dump\":");
            json.push_str(&dump.report_json());
        }
        json.push('}');
        json
    }
}

impl Drop for Aes67RxCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// \`BlockStatus\` 严重度排序（整块静音策略取最严重者）。
fn block_status_rank(status: BlockStatus) -> u8 {
    match status {
        BlockStatus::Ok => 0,
        BlockStatus::Warmup => 1,
        BlockStatus::Underrun => 2,
        BlockStatus::NotDue => 3,
    }
}

#[cfg(test)]
mod aes67_rx_proto_tests {
    include!("../tests/T235/aes67_rx.rs");
}
