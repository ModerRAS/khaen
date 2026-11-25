//! [NONRT] 可变 N/O channel-map 扩展点（core-v1 §11.10） ——WBS T25 写域。
//!
//! ## 状态（F-31 口径：模块职责标注）
//!
//! 本文件是**扩展点占位**（T06 scaffold -> T25 follow-up）的实现起点，**不是** P1 运行时图：
//!
//! - 可变 N/O 的权威解析/校验在 `crate::roles`（T57 overlay layout-presets；唯一 role→DAC
//!   来源），本模块不得复制第二份映射表；
//! - S1 内容判定/上混状态机在 `crate::content`，其定位是**退役对照兼容资料
//!   （compat-shim）**，不是 compat 的公开运行时：P1 不消费、不链入控制面，REST
//!   `/api/channelmap/input-mode` 恒 `not_supported`（R47 唯一例外）；
//! - 需要可变 N/O 时由 T25 在本文件新增最小 API（选择/零填/校验/scatter），上混状态机
//!   只留在退役核对测试（tests/T25）里，不得提升为生产路径。
//!
//! 依据：compat-matrix §5 Y01–Y11（layout/roles 契约）、§6 C01–C09（C01–C08 内容上混仍 RETIRE；
//! C09 = 「缺中置内容适配」——**已由 host 自持实现**（`rt-host/src/content_map.rs`，task-210；
//! host 运行时接线 task-203），本文件保留为**旧 S1/兼容层原型参考**（host 不再依赖/调用 compat）；
//! C09 规格口径见 compat-matrix **v0.7**）、DEC §1/§6；本文件与 `content.rs` 的职责边界
//! 以本条注释为准（F-31）。
//!
//! ## task-200 扩展：缺中置内容适配（center-absent downmix；先证据后改）
//!
//! 中置内容消失的位置（证据：`rt-stack/evidence/center-downmix/LAYER-ANALYSIS.md`）：
//! 4.1.2 输出布局 7 个角色没有 C（`audio/overlay/etc/tspi-audio/layout-presets.yml:219-229`），
//! 5.1 内容的 C 声道在 plan 的 A_map「选择/零填」处没有任何 role 可选中而被丢弃
//! （core-v1 §1/§11.10、core/src/nodes.rs:17、calibration/src/compile_apply.rs:3046/3081
//! 均冻结「plan 运行期禁止混音、A_map 每 role 至多一源且源唯一」）。
//!
//! 因此修复必须落在 **plan 之前的内容适配层**（本文件）：在捕获内容样本域把 C 以标准
//! −3 dB 预混进 FL/FR（"虚拟中置"），并把 C 源消费掉（置零，防重复混）；plan 的
//! A_map/graph_id/RT 图语义完全不变。规则：
//!
//! 1. **仅缺中置时启用**：输出布局无 C 角色（中置音箱缺席）且输入内容布局有 C 时启用；
//! 2. **不改有中置配置**：输出布局含 C（5.1.2/5.3.2/7.1.4 等）时本适配层 passthrough
//!    （状态 `DisabledCenterPresent`，缓冲逐位不变）；
//! 3. **只改 FL/FR**：其余源（SL/SR/LFE/高度等）逐样本不变；C 源在混后置零；
//! 4. **系数**：−3 dB/每目标声道（`CENTER_DOWNMIX_GAIN_DB`，10^(−3/20)≈0.7079），
//!    依据 ITU-R BS.775 / Dolby 5.1→2.0 常用 center mix level，与 S1 预设「中置 −3 dB」口径一致；
//! 5. **削波/余量保护**：沿用 D7 口径（`calibration-spec/ACCEPTANCE.md:9,190`；
//!    `calibration/src/compile_apply.rs:175-183` 的 D7 常量、`peq_fit.rs:330` 的 program peak
//!    基准默认 −18 dBFS）：最坏相干叠加 `20·log10(1+g)`≈+4.65 dB，预测累计峰值须 ≤−6 dBFS
//!    （headroom ≥6 dB）；不满足则对**全部源等量 trim**（保持声道间相对电平），并报告
//!    predicted peak / headroom / trim。
//!
//! RT 备注：`CenterDownmixPlan::apply_planar_f32_in_place` 是纯标量、零堆分配、无 panic
//! 分支（长度/索引先校验）的 `&mut [f32]` 循环，可在宿主块循环里用预分配平面缓冲调用；
//! 宿主接线（capture 块 → 内容适配层 → plan A_map）：task-200 时点为 NOT_RUN（`rt-host/src/main.rs`
//! 属他人写域）；task-203 已接入 host plan-arena 播放链，task-210 起由 **rt-host 自持**
//! `content_map` 承接（本文件原型不再被 host 调用）——本文件不接线、不碰板端（NOT_RUN）。
//!
//! 本模块**不是**通用 downmix：不做 5.1→2.0 的环绕/低音折叠，不实现节目上混/自动内容上混，
//! 只处理"缺中置音箱"这一种缺失（其余缺失角色仍由 plan 显式零填/拒收）。

use crate::roles::LayoutPresets;

// ---------------------------------------------------------------------------
// 冻结常量（缺中置下混；数值改动需 Lead 裁决 + 证据更新）
// ---------------------------------------------------------------------------

/// 中置角色 id（layout-presets / plan 角色命名）。
pub const CENTER_ROLE_ID: &str = "C";
/// 中置下混目标角色（顺序 = 左、右）。
pub const CENTER_DOWNMIX_TARGET_ROLES: [&str; 2] = ["FL", "FR"];
/// 标准中置下混系数（每个目标声道）：−3.0 dB。
///
/// 依据：ITU-R BS.775 / Dolby 5.1→2.0 常用 center mix level 0.7071（−3 dB），与 S1
/// upmix 预设的「中置 −3 dB」口径一致（`upmix/presets/upmix-presets.yml`）。
pub const CENTER_DOWNMIX_GAIN_DB: f64 = -3.0;
/// `CENTER_DOWNMIX_GAIN_DB` 的线性系数（f32；与 f64 精确值差 < 1e-7）。
pub const CENTER_DOWNMIX_GAIN_LINEAR: f32 = 0.707_945_8;
/// D7 预测累计峰值上限（dBFS；与 `calibration-spec/ACCEPTANCE.md:190`、D7 常量同值）。
pub const CENTER_DOWNMIX_PEAK_LIMIT_DBFS: f64 = -6.0;
/// D7 最小 headroom（dB；= −predicted_peak_limit）。
pub const CENTER_DOWNMIX_MIN_HEADROOM_DB: f64 = 6.0;
/// D7 program peak 基准默认值（dBFS；同 `calibration/src/peq_fit.rs:330` 默认 −18 dBFS）。
pub const CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS: f64 = -18.0;

/// 中置下混标准系数的 f64 精确值（10^(dB/20)）。
pub fn center_downmix_gain_linear_f64() -> f64 {
    10.0_f64.powf(CENTER_DOWNMIX_GAIN_DB / 20.0)
}

/// 最坏相干叠加增益（dB）：FL/FR 与 C 同相且都满幅时 `20·log10(1+g)` ≈ +4.65 dB。
///
/// 该值只用于 headroom 报告/校验，不是运行增益；正常内容（program peak 基准 −18 dBFS）下
/// 预测峰值 ≈ −13.35 dBFS，仍在 D7 的 −6 dBFS 门内，故默认不做 trim（−3 dB 系数不缩水）。
pub fn center_downmix_worst_case_added_db() -> f64 {
    20.0 * (1.0 + center_downmix_gain_linear_f64()).log10()
}

// ---------------------------------------------------------------------------
// 类型：声明/状态/错误
// ---------------------------------------------------------------------------

/// 中置音箱声明。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CenterSpeakerState {
    /// 由输出布局决定：布局含 C 角色 = 中置存在；不含 = 中置缺席（默认）。
    Auto,
    /// 显式声明中置存在：不做下混；若输出布局无 C 角色则拒收（配置矛盾）。
    Present,
    /// 显式声明中置缺席：输出布局含 C 角色则拒收（不改有中置配置）。
    Absent,
}

/// 缺中置适配计划状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CenterDownmixStatus {
    /// 已启用：C 以 −3 dB 预混进 FL/FR，C 源将被消费（置零）。
    Applied,
    /// 未启用：输出布局含 C（有中置配置）——不改变任何样本。
    DisabledCenterPresent,
    /// 未启用：输入内容布局没有 C（无中置内容）——不改变任何样本。
    DisabledNoCenterContent,
}

impl CenterDownmixStatus {
    /// 是否启用下混。
    pub fn is_applied(self) -> bool {
        matches!(self, CenterDownmixStatus::Applied)
    }

    /// 稳定状态字符串（报告/日志用；不随文案变化）。
    pub fn as_str(self) -> &'static str {
        match self {
            CenterDownmixStatus::Applied => "applied",
            CenterDownmixStatus::DisabledCenterPresent => "disabled_center_present",
            CenterDownmixStatus::DisabledNoCenterContent => "disabled_no_center_content",
        }
    }
}

/// channel-map 模块错误码（E_CHANNEL_MAP_*；字段路径 + 明细，不静默降级/不 panic）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelMapErrorCode {
    /// 引用的输入/输出布局在 overlay 预设中不存在。
    LayoutUnknown,
    /// 布局通道声明非法（stream 越界/通道越界/通道数非正）。
    LayoutChannels,
    /// 必需角色缺失（C 内容/FL/FR 目标）。
    RoleMissing,
    /// 角色/通道表冲突（重复角色、重复通道、FL==FR==C 等）。
    RoleChannelConflict,
    /// 中置声明与输出布局矛盾（声明 Present 但无 C 角色；声明 Absent 但含 C 角色）。
    CenterPolicyConflict,
    /// headroom 配置非法（program peak 非有限等）。
    HeadroomConfig,
    /// planar 缓冲长度/frames 与计划不匹配。
    BufferSize,
}

impl ChannelMapErrorCode {
    /// 全部错误码（测试/报告用）。
    pub const ALL: &'static [ChannelMapErrorCode] = &[
        ChannelMapErrorCode::LayoutUnknown,
        ChannelMapErrorCode::LayoutChannels,
        ChannelMapErrorCode::RoleMissing,
        ChannelMapErrorCode::RoleChannelConflict,
        ChannelMapErrorCode::CenterPolicyConflict,
        ChannelMapErrorCode::HeadroomConfig,
        ChannelMapErrorCode::BufferSize,
    ];

    /// 稳定错误码字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelMapErrorCode::LayoutUnknown => "E_CHANNEL_MAP_LAYOUT_UNKNOWN",
            ChannelMapErrorCode::LayoutChannels => "E_CHANNEL_MAP_LAYOUT_CHANNELS",
            ChannelMapErrorCode::RoleMissing => "E_CHANNEL_MAP_ROLE_MISSING",
            ChannelMapErrorCode::RoleChannelConflict => "E_CHANNEL_MAP_ROLE_CHANNEL_CONFLICT",
            ChannelMapErrorCode::CenterPolicyConflict => "E_CHANNEL_MAP_CENTER_POLICY_CONFLICT",
            ChannelMapErrorCode::HeadroomConfig => "E_CHANNEL_MAP_HEADROOM_CONFIG",
            ChannelMapErrorCode::BufferSize => "E_CHANNEL_MAP_BUFFER_SIZE",
        }
    }
}

/// channel-map 模块错误（code + 字段路径 + 明细；不 panic、不静默降级）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelMapError {
    /// 稳定错误码。
    pub code: ChannelMapErrorCode,
    /// 字段路径（例如 source.role_channels）。
    pub field: String,
    /// 人读明细。
    pub detail: String,
}

impl ChannelMapError {
    /// 构造（无字段路径）。
    pub fn new(code: ChannelMapErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: String::new(),
            detail: detail.into(),
        }
    }

    /// 构造（带字段路径）。
    pub fn with_field(
        code: ChannelMapErrorCode,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 稳定错误码字符串。
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

impl std::fmt::Display for ChannelMapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.field.is_empty() {
            write!(formatter, "{}: {}", self.code.as_str(), self.detail)
        } else {
            write!(
                formatter,
                "{} at {}: {}",
                self.code.as_str(),
                self.field,
                self.detail
            )
        }
    }
}

impl std::error::Error for ChannelMapError {}

// ---------------------------------------------------------------------------
// 类型：内容源域 / 输出目标 / 配置 / headroom
// ---------------------------------------------------------------------------

/// 内容源域描述：输入内容布局的「角色 → planar 通道索引」表。
///
/// 来源可以是 T57 输入布局预设（`from_layout`），也可以是 T43 H16 RolePlan/显式配置
/// （`new`：例如 `[("FL",0),("FR",1),("C",2),("LFE",3),("SL",4),("SR",5)]`）。
/// 本模块不复制 overlay 映射表：索引由调用方/预设给出。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CenterDownmixSource {
    /// 输入布局 id（报告/证据用；`new` 时可为显式配置名）。
    pub layout_id: String,
    /// planar 通道数（= 每通道帧缓冲的分段数）。
    pub channels: usize,
    /// 角色 id → planar 通道索引（不得重复角色/通道）。
    pub role_channels: Vec<(String, usize)>,
}

impl CenterDownmixSource {
    /// 显式构造并校验（role_channels 不得重复角色/通道；索引必须 < channels）。
    pub fn new(
        layout_id: impl Into<String>,
        channels: usize,
        role_channels: Vec<(String, usize)>,
    ) -> Result<Self, ChannelMapError> {
        let source = Self {
            layout_id: layout_id.into(),
            channels,
            role_channels,
        };
        source.validate()?;
        Ok(source)
    }

    /// 从 overlay 输入布局预设构造（T57 唯一 role→通道来源；多 flow 按 streams 顺序展平）。
    pub fn from_layout(presets: &LayoutPresets, layout_id: &str) -> Result<Self, ChannelMapError> {
        let layout = presets.input_layout(layout_id).ok_or_else(|| {
            ChannelMapError::with_field(
                ChannelMapErrorCode::LayoutUnknown,
                "input_layouts",
                format!("未知输入布局 {layout_id:?}"),
            )
        })?;
        let stream_count = layout.streams.len();
        let mut stream_offsets: Vec<usize> = Vec::with_capacity(stream_count);
        if stream_count > 0 {
            let mut offset = 0usize;
            for width in &layout.streams {
                stream_offsets.push(offset);
                offset += *width as usize;
            }
        }
        let mut role_channels: Vec<(String, usize)> = Vec::with_capacity(layout.roles.len());
        for role in &layout.roles {
            let channel = if stream_count == 0 {
                if role.stream != 0 {
                    return Err(ChannelMapError::with_field(
                        ChannelMapErrorCode::LayoutChannels,
                        format!("input_layouts.{layout_id}.roles.{}", role.id),
                        format!(
                            "多 flow 输入布局必须声明 streams（角色 {} 的 stream={}）",
                            role.id, role.stream
                        ),
                    ));
                }
                role.channel as usize
            } else {
                let stream = role.stream as usize;
                if stream >= stream_count {
                    return Err(ChannelMapError::with_field(
                        ChannelMapErrorCode::LayoutChannels,
                        format!("input_layouts.{layout_id}.roles.{}", role.id),
                        format!("stream={stream} 越界（streams 数 {stream_count}）"),
                    ));
                }
                let width = layout.streams[stream] as usize;
                if role.channel as usize >= width {
                    return Err(ChannelMapError::with_field(
                        ChannelMapErrorCode::LayoutChannels,
                        format!("input_layouts.{layout_id}.roles.{}", role.id),
                        format!("通道 {} 越界（stream {stream} 宽 {width}）", role.channel),
                    ));
                }
                stream_offsets[stream] + role.channel as usize
            };
            role_channels.push((role.id.clone(), channel));
        }
        let channels = if stream_count > 0 {
            let last = stream_count - 1;
            stream_offsets[last] + layout.streams[last] as usize
        } else {
            let max_channel = role_channels
                .iter()
                .map(|(_, channel)| channel + 1)
                .max()
                .unwrap_or(0);
            max_channel.max(layout.channels.unwrap_or(0) as usize)
        };
        Self::new(layout_id, channels, role_channels)
    }

    /// 某角色对应的 planar 通道索引。
    pub fn role_channel(&self, role: &str) -> Option<usize> {
        self.role_channels
            .iter()
            .find(|(id, _)| id == role)
            .map(|(_, channel)| *channel)
    }

    /// 校验角色/通道表（构造期与 `build` 前都调用；幂等）。
    pub fn validate(&self) -> Result<(), ChannelMapError> {
        if self.channels == 0 {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::LayoutChannels,
                "source.channels",
                "内容源域通道数必须 >0",
            ));
        }
        if self.role_channels.is_empty() {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::RoleMissing,
                "source.role_channels",
                format!("内容源域角色表为空（layout={}）", self.layout_id),
            ));
        }
        let mut seen_channels: Vec<usize> = Vec::with_capacity(self.role_channels.len());
        for (role, channel) in &self.role_channels {
            if role.is_empty() {
                return Err(ChannelMapError::with_field(
                    ChannelMapErrorCode::RoleMissing,
                    "source.role_channels",
                    "角色 id 不能为空",
                ));
            }
            if *channel >= self.channels {
                return Err(ChannelMapError::with_field(
                    ChannelMapErrorCode::LayoutChannels,
                    format!("source.role_channels.{role}"),
                    format!("通道索引 {channel} 越界（channels={}）", self.channels),
                ));
            }
            if seen_channels.contains(channel) {
                return Err(ChannelMapError::with_field(
                    ChannelMapErrorCode::RoleChannelConflict,
                    format!("source.role_channels.{role}"),
                    format!("通道 {channel} 被多个角色占用（内容源域必须单射）"),
                ));
            }
            if self
                .role_channels
                .iter()
                .filter(|(id, _)| id == role)
                .count()
                > 1
            {
                return Err(ChannelMapError::with_field(
                    ChannelMapErrorCode::RoleChannelConflict,
                    format!("source.role_channels.{role}"),
                    format!("角色 {role} 重复"),
                ));
            }
            seen_channels.push(*channel);
        }
        Ok(())
    }
}

/// 输出目标描述：输出布局是否含 C 角色（= 中置音箱是否存在于该布局）。
///
/// 只取「是否有 C」这一个事实：本适配层在 plan 之前，不需要输出槽位/scatter 信息
/// （scatter 仍由 plan 自己表达）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CenterDownmixTarget {
    /// 输出布局 id（报告/证据用）。
    pub layout_id: String,
    /// 输出布局是否含 C 角色。
    pub has_center_role: bool,
}

impl CenterDownmixTarget {
    /// 显式构造。
    pub fn new(layout_id: impl Into<String>, has_center_role: bool) -> Self {
        Self {
            layout_id: layout_id.into(),
            has_center_role,
        }
    }

    /// 从 overlay 输出布局预设构造。
    pub fn from_layout(presets: &LayoutPresets, layout_id: &str) -> Result<Self, ChannelMapError> {
        let layout = presets.output_layout(layout_id).ok_or_else(|| {
            ChannelMapError::with_field(
                ChannelMapErrorCode::LayoutUnknown,
                "output_layouts",
                format!("未知输出布局 {layout_id:?}"),
            )
        })?;
        let has_center_role = layout.roles.iter().any(|role| role.id == CENTER_ROLE_ID);
        Ok(Self::new(layout.id.clone(), has_center_role))
    }
}

/// 缺中置适配配置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDownmixConfig {
    /// 中置音箱声明（默认 Auto：由输出布局是否含 C 角色决定）。
    pub center_state: CenterSpeakerState,
    /// D7 program peak 基准（dBFS；默认 −18.0，同 `peq_fit` 默认）。
    ///
    /// 该值不是实时峰值，而是「节目素材峰值基准」：headroom 检查用它 + 最坏相干叠加
    /// 增益估算预测峰值；实际峰值/限幅仍由宿主保护节点（limiter/clip）兜底。
    pub program_peak_dbfs: f64,
}

impl Default for CenterDownmixConfig {
    fn default() -> Self {
        Self {
            center_state: CenterSpeakerState::Auto,
            program_peak_dbfs: CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
        }
    }
}

impl CenterDownmixConfig {
    /// 默认 Auto + D7 program peak 基准。
    pub fn auto() -> Self {
        Self::default()
    }

    /// 指定中置声明。
    pub fn with_center_state(mut self, center_state: CenterSpeakerState) -> Self {
        self.center_state = center_state;
        self
    }

    /// 指定 D7 program peak 基准（dBFS）。
    pub fn with_program_peak_dbfs(mut self, program_peak_dbfs: f64) -> Self {
        self.program_peak_dbfs = program_peak_dbfs;
        self
    }
}

/// headroom 检查结果（D7 口径；报告/证据用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDownmixHeadroom {
    /// 中置下混系数（dB；冻结 −3.0）。
    pub center_gain_db: f64,
    /// 中置下混系数（线性 f32）。
    pub center_gain_linear: f32,
    /// 最坏相干叠加增益（dB；20·log10(1+g)≈+4.65）。
    pub worst_case_added_db: f64,
    /// program peak 基准（dBFS）。
    pub program_peak_dbfs: f64,
    /// 未 trim 时的预测累计峰值（dBFS）= program_peak + worst_case_added。
    pub predicted_peak_dbfs: f64,
    /// D7 预测峰值上限（dBFS；−6.0）。
    pub peak_limit_dbfs: f64,
    /// D7 最小 headroom（dB；6.0）。
    pub min_headroom_db: f64,
    /// 需要施加的全局 trim（dB；0 = 不衰减，负值 = 衰减）。
    pub trim_db: f64,
    /// trim 后的预测累计峰值（dBFS；≤ peak_limit）。
    pub predicted_peak_after_trim_dbfs: f64,
    /// trim 后的 headroom（dB；≥ min_headroom）。
    pub headroom_after_trim_db: f64,
    /// 是否满足 D7 headroom 政策（trim 后）。
    pub within_policy: bool,
}

impl CenterDownmixHeadroom {
    /// 按 D7 口径计算 headroom（program peak 基准必须是有限值）。
    pub fn compute(program_peak_dbfs: f64) -> Result<Self, ChannelMapError> {
        if !program_peak_dbfs.is_finite() {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::HeadroomConfig,
                "config.program_peak_dbfs",
                format!("program peak 基准必须是有限 dBFS（实得 {program_peak_dbfs}）"),
            ));
        }
        let center_gain_db = CENTER_DOWNMIX_GAIN_DB;
        let center_gain_linear = CENTER_DOWNMIX_GAIN_LINEAR;
        let worst_case_added_db = center_downmix_worst_case_added_db();
        let predicted_peak_dbfs = program_peak_dbfs + worst_case_added_db;
        let peak_limit_dbfs = CENTER_DOWNMIX_PEAK_LIMIT_DBFS;
        let min_headroom_db = CENTER_DOWNMIX_MIN_HEADROOM_DB;
        // 只在超门时衰减：trim = 上限 − 预测峰值（≤0）；否则 0（−3 dB 系数不缩水）。
        let trim_db = if predicted_peak_dbfs > peak_limit_dbfs {
            peak_limit_dbfs - predicted_peak_dbfs
        } else {
            0.0
        };
        let predicted_peak_after_trim_dbfs = predicted_peak_dbfs + trim_db;
        let headroom_after_trim_db = -predicted_peak_after_trim_dbfs;
        let within_policy = predicted_peak_after_trim_dbfs <= peak_limit_dbfs + 1e-9
            && headroom_after_trim_db >= min_headroom_db - 1e-9;
        Ok(Self {
            center_gain_db,
            center_gain_linear,
            worst_case_added_db,
            program_peak_dbfs,
            predicted_peak_dbfs,
            peak_limit_dbfs,
            min_headroom_db,
            trim_db,
            predicted_peak_after_trim_dbfs,
            headroom_after_trim_db,
            within_policy,
        })
    }

    /// trim 线性系数（f32；1.0 = 不衰减）。
    pub fn trim_linear(&self) -> f32 {
        10.0_f32.powf(self.trim_db as f32 / 20.0)
    }
}

// ---------------------------------------------------------------------------
// 计划：构建 + 应用
// ---------------------------------------------------------------------------

/// 缺中置内容适配计划（NONRT 设计期产物；apply 是零分配样本域变换）。
#[derive(Clone, Debug, PartialEq)]
pub struct CenterDownmixPlan {
    /// 状态（Applied / DisabledCenterPresent / DisabledNoCenterContent）。
    pub status: CenterDownmixStatus,
    /// 内容源域（输入布局角色→planar 通道）。
    pub source: CenterDownmixSource,
    /// 输出目标（是否含 C 角色）。
    pub target: CenterDownmixTarget,
    /// 内容 C 通道索引（Applied 时 Some）。
    pub center_channel: Option<usize>,
    /// FL 通道索引（Applied 时 Some）。
    pub left_channel: Option<usize>,
    /// FR 通道索引（Applied 时 Some）。
    pub right_channel: Option<usize>,
    /// D7 headroom 检查结果（trim 只在 Applied 时生效）。
    pub headroom: CenterDownmixHeadroom,
}

impl CenterDownmixPlan {
    /// 构建计划：仅当「输出布局无 C（中置缺席）」且「输入内容有 C」时启用。
    ///
    /// - `CenterSpeakerState::Auto`：中置存在性由 `target.has_center_role` 决定；
    /// - `Present`：输出布局无 C → `E_CHANNEL_MAP_CENTER_POLICY_CONFLICT`（声明与布局矛盾）；
    /// - `Absent`：输出布局含 C → `E_CHANNEL_MAP_CENTER_POLICY_CONFLICT`（不改有中置配置）。
    pub fn build(
        source: &CenterDownmixSource,
        target: &CenterDownmixTarget,
        config: &CenterDownmixConfig,
    ) -> Result<Self, ChannelMapError> {
        source.validate()?;
        let headroom = CenterDownmixHeadroom::compute(config.program_peak_dbfs)?;
        let declared_present = match config.center_state {
            CenterSpeakerState::Auto => target.has_center_role,
            CenterSpeakerState::Present => true,
            CenterSpeakerState::Absent => false,
        };
        if declared_present && !target.has_center_role {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::CenterPolicyConflict,
                "config.center_state",
                format!(
                    "声明中置存在（{:?}）但输出布局 {} 无 C 角色（配置矛盾，拒绝静默下混）",
                    config.center_state, target.layout_id
                ),
            ));
        }
        if !declared_present && target.has_center_role {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::CenterPolicyConflict,
                "config.center_state",
                format!(
                    "输出布局 {} 含 C 角色（有中置配置），声明中置缺席（{:?}）会改变有中置配置——拒绝",
                    target.layout_id, config.center_state
                ),
            ));
        }
        let plan = |status: CenterDownmixStatus,
                    center_channel: Option<usize>,
                    left_channel: Option<usize>,
                    right_channel: Option<usize>| Self {
            status,
            source: source.clone(),
            target: target.clone(),
            center_channel,
            left_channel,
            right_channel,
            headroom,
        };
        if declared_present {
            // 有中置配置：passthrough（不改任何样本）。
            return Ok(plan(
                CenterDownmixStatus::DisabledCenterPresent,
                None,
                None,
                None,
            ));
        }
        let Some(center_channel) = source.role_channel(CENTER_ROLE_ID) else {
            return Ok(plan(
                CenterDownmixStatus::DisabledNoCenterContent,
                None,
                None,
                None,
            ));
        };
        let left_channel = source
            .role_channel(CENTER_DOWNMIX_TARGET_ROLES[0])
            .ok_or_else(|| {
                ChannelMapError::with_field(
                    ChannelMapErrorCode::RoleMissing,
                    "source.role_channels",
                    format!(
                        "输入布局 {} 缺 {} 角色（中置下混目标），无法启用",
                        source.layout_id, CENTER_DOWNMIX_TARGET_ROLES[0]
                    ),
                )
            })?;
        let right_channel = source
            .role_channel(CENTER_DOWNMIX_TARGET_ROLES[1])
            .ok_or_else(|| {
                ChannelMapError::with_field(
                    ChannelMapErrorCode::RoleMissing,
                    "source.role_channels",
                    format!(
                        "输入布局 {} 缺 {} 角色（中置下混目标），无法启用",
                        source.layout_id, CENTER_DOWNMIX_TARGET_ROLES[1]
                    ),
                )
            })?;
        if left_channel == right_channel
            || center_channel == left_channel
            || center_channel == right_channel
        {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::RoleChannelConflict,
                "source.role_channels",
                format!(
                    "C/FL/FR 通道必须互异（C={center_channel}, FL={left_channel}, FR={right_channel}）"
                ),
            ));
        }
        Ok(plan(
            CenterDownmixStatus::Applied,
            Some(center_channel),
            Some(left_channel),
            Some(right_channel),
        ))
    }

    /// 便捷构造：直接从 overlay 预设 id 构建源域/目标。
    pub fn from_layouts(
        presets: &LayoutPresets,
        input_layout_id: &str,
        output_layout_id: &str,
        config: &CenterDownmixConfig,
    ) -> Result<Self, ChannelMapError> {
        let source = CenterDownmixSource::from_layout(presets, input_layout_id)?;
        let target = CenterDownmixTarget::from_layout(presets, output_layout_id)?;
        Self::build(&source, &target, config)
    }

    /// 是否启用下混。
    pub fn is_applied(&self) -> bool {
        self.status.is_applied()
    }

    /// 在 planar f32 内容块上应用计划（零堆分配、无 panic 分支；长度/索引先校验）。
    ///
    /// 语义：
    /// - `planar` 布局 = `source.channels` 个平面，每平面 `frames` 个样本，长度必须严格
    ///   等于 `source.channels × frames`；
    /// - 未启用（CenterPresent / NoCenterContent）：只校验长度，样本逐位不变；
    /// - 启用：先按 D7 headroom 结果做全局 trim（仅超门时；保持声道间相对电平），
    ///   再 `FL += g·C`、`FR += g·C`，最后 `C = 0`（消费 C 源；重复调用不会重复叠加中置）。
    ///
    /// 注意：每个内容块只应调用一次本函数（trim 是乘性增益，重复调用会重复应用 trim；
    /// C 源置零保证即使用错也不会重复混入中置）。
    pub fn apply_planar_f32_in_place(
        &self,
        planar: &mut [f32],
        frames: usize,
    ) -> Result<(), ChannelMapError> {
        if frames == 0 || self.source.channels == 0 {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::BufferSize,
                "apply.frames",
                format!(
                    "frames 与源域通道数必须 >0（frames={frames}, channels={}）",
                    self.source.channels
                ),
            ));
        }
        let expected = self.source.channels.checked_mul(frames).ok_or_else(|| {
            ChannelMapError::with_field(
                ChannelMapErrorCode::BufferSize,
                "apply.frames",
                "channels × frames 溢出 usize",
            )
        })?;
        if planar.len() != expected {
            return Err(ChannelMapError::with_field(
                ChannelMapErrorCode::BufferSize,
                "apply.planar",
                format!(
                    "planar 长度 {} != channels {} × frames {} = {expected}",
                    planar.len(),
                    self.source.channels,
                    frames
                ),
            ));
        }
        if !self.is_applied() {
            return Ok(());
        }
        let (center, left, right) =
            match (self.center_channel, self.left_channel, self.right_channel) {
                (Some(center), Some(left), Some(right)) => (center, left, right),
                _ => {
                    return Err(ChannelMapError::with_field(
                        ChannelMapErrorCode::RoleMissing,
                        "plan.channels",
                        "Applied 计划缺 C/FL/FR 通道索引（计划内部不一致）",
                    ))
                }
            };
        let trim = self.headroom.trim_linear();
        if trim != 1.0 {
            for sample in planar.iter_mut() {
                *sample *= trim;
            }
        }
        let gain = self.headroom.center_gain_linear;
        let center_base = center * frames;
        let left_base = left * frames;
        let right_base = right * frames;
        let mut frame = 0usize;
        while frame < frames {
            let center_sample = planar[center_base + frame];
            planar[left_base + frame] += gain * center_sample;
            planar[right_base + frame] += gain * center_sample;
            planar[center_base + frame] = 0.0;
            frame += 1;
        }
        Ok(())
    }
}
