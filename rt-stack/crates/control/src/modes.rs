//! [NONRT] 模式/状态机（core-v1 §5.1/§5.4；compat A01–A22/K39–K44） ——WBS T44 写域。
//!
//! 本模块是 P1 模式面的唯一 owner（T26 在真实 RuntimeEngine::switch_mode 里接线）：
//!
//! - **互斥**：off / toslink / target_multichannel / aes67 / dante 同一时刻只有一个生效模式
//!   （A01/K39）；切换串行，在途再次切换 -> ERR_SWITCH_BUSY（120 / REST 409 switch_busy /
//!   exit 10）；calibration job 持有设备（DeviceOwner::Calibration）时 -> ERR_DEVICE_BUSY
//!   （117 / REST 409 device_busy）。
//! - **唯一 servo/epoch**：每个播放模式唯一 clock servo owner（toslink/target = MOTU frame
//!   域 async servo；aes67/dante = PTP 域 async servo；off = 无），复用 T19 ClockServo
//!   claim/release 语义（第二个闭环 claim 必失败）；每次提交一次模式切换 = 一次
//!   discontinuity -> 新 epoch（core-v1 §5.2，EpochClock 单调、永不复用）。
//! - **schema/hash 原子切换**：状态快照带 schema + config_hash；候选状态先算好并校验
//!   （stale expected hash -> ERR_PLAN_STALE 119，零副作用），只有全部步骤成功才做一次
//!   整体发布（无半发布）；config_hash 复用 T42 RuntimeConfig::fingerprint()
//!   （mode/fs/chunksize/target/coeff 的 sha256），target hash 复用 TargetSpec::fingerprint()。
//! - **失败 off 回退**：失败先逆序停掉本模式已启单元、释放 servo，再按 FallbackPolicy
//!   回退：默认 Off（A16：mode=off + state=error + last_error 可查），可选 LastGood
//!   （新 host 自研 last-good，非旧 C）。回退后 last_exit_code=9（A03 已回滚）。
//! - **模拟服务状态**：ModeServices 抽象 + SimulatedServices 内存实现（无设备/无线程/
//!   无 systemd 调用），按 A07 顺序编排：停旧（data→PTP）→ 启目标 PTP → 等锁 → 启 data →
//!   自检 → 新 epoch/servo。单元名只来自 A11 的 tspi-* overlay units。
//! - **禁调用 C**：ServiceUnit 枚举编译期就没有 CamillaDSP 单元；服务名带
//!   FORBIDDEN_SERVICE_MARKERS 时 debug_assert 拒绝；legacy status 的 camilladsp 恒为
//!   null（G-no-C）。测试断言全部模式/单元里 0 个 C 标记。
//! - **MOTU/CueMix 不受影响**：ModeController 持有 CueMixRouting 只读模型，任何切换/
//!   失败都不写它（A22/P11，用户 Toslink 直通不动）。**F-29**："切换前后 diff=0" 只能
//!   由 `CueMixSource` 真实读取设备快照证明；无读取器/无设备时 `CueMixCheck::not_run()`
//!   （NOT_RUN），禁止用进程内模型自比冒充板端 diff。
//! - **T42 RuntimeEngine 接线（F-28）**：`ModeEngine` 适配器把本模块状态机接进 REST
//!   engine 接口；`switch_mode` 返回新 epoch/唯一 servo/回滚证据（`mode_evidence()`），
//!   未接线的 engine 回执在 REST 层显式标 synthetic。回滚错误码见 F-30（124，不复用 119）。
//!
//! 写域：仅本文件 + tests/T44/ + evidence/T44/（公共根/Cargo.toml/lib.rs 归 integrator）。
//!
//! 状态机迁移（与 core-v1 §5.1/§5.4 对齐）：
//!
//! switch(mode) 两阶段：stop old -> enter new -> self-check -> publish。
//! Idle(Off) --ok--> Idle(Playback)；Idle(mode) --ok--> Idle(mode2)。
//! 任一 enter/self-check 失败 -> Error(Off)（默认）或 Error(LastGood)。
//! begin_calibration 先切 off 再置 Calibration；end_calibration 回 Idle。
//!
//! 本模块**不打开设备/不启服务/不跑 RT**：服务、时钟均以模型替身驱动，所有“已验证”仅指
//! 本地 cargo test -p tspi-control modes 真实执行（见 evidence/T44/）。

use tspi_dsp_core::rt::{RtStatus, BLOCK_SIZES, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ};
use tspi_rt_host::aoip::{
    inferno_device_id_valid, validate_aes67_capture, validate_aes67_service, Aes67CapturePlan,
    Aes67Encoding, Aes67FlowSdp, Aes67FlowSet, Aes67ServiceState, AoipError, AoipProtocol,
    H16Container, H16FlowConfig, InfernoEnv, PtpRefClock, PtpSource, AES67_RTP_PORT_DEFAULT,
    H16_CONTAINER_CHANNELS, INFERNO_CLOCK_PATH_DEFAULT,
};
use tspi_rt_host::clock::{
    ClockError, ClockServo, DiscontinuityReason, EpochClock, LockState, ServoConfig, ServoGrant,
    ServoOwner,
};

use tspi_dsp_compat::ws;

use crate::rest::{
    CoeffRef, CommitOutcome, ControlMode, EngineInfo, EngineSnapshot, JsonValue, ModeAck,
    ModeStateEvidence, MuteAck, PrepareOutcome, RuntimeConfig, RuntimeEngine, TargetSpec,
    VolumeAck, ALLOWED_MODES, MODE_STATUS_SCHEMA, P1_412_CHANNELS,
};

// ---------------------------------------------------------------------------
// 常量（契约冻结；A01-A22 / K39-K44 / T02 core-v1）
// ---------------------------------------------------------------------------

/// 本模块状态快照 schema（T26/T70 消费；与 T42 REST 的 MODE_STATUS_SCHEMA 兼容映射）。
pub const MODE_STATE_SCHEMA: &str = "tspi-mode-state/v1";
/// A04 legacy tspi-mode status --json schema（字段名保持，可 null）。
pub const LEGACY_STATUS_SCHEMA: &str = "tspi-audio/v1";

/// 播放模式默认 chunksize/period frames（T02 Q-11：runtime 仅 64/128）。
pub const MODE_DEFAULT_CHUNKSIZE: u16 = 128;
/// toslink/target_multichannel 的 MOTU USB 采样率（P1 日常播放 48k）。
pub const MODE_PLAYBACK_RATE_HZ: u32 = 48_000;
/// Dante/Inferno 端点采样率（A08/A14：96k）。
pub const DANTE_RATE_HZ: u32 = 96_000;
/// AES67 端点采样率（A09/A12：48k）。
pub const AES67_RATE_HZ: u32 = 48_000;

/// toslink profile 名（R10/R11 回 actual profile）。
pub const PROFILE_TOSLINK: &str = "toslink-2.0";
/// target_multichannel profile 名（布局/target 来自 T57 解析结果，不硬编码）。
pub const PROFILE_TARGET_MULTICHANNEL: &str = "target-multichannel";
/// AES67 profile 名。
pub const PROFILE_AES67: &str = "aes67-48k";
/// Dante/Inferno profile 名。
pub const PROFILE_DANTE: &str = "dante-96k";
/// off profile（无音频 profile；状态里为 None，仅内部编排用）。
pub const PROFILE_OFF: &str = "off";

/// toslink 输出通道数（2.0 stereo strict；其余输出静音，不实现上混）。
pub const TOSLINK_CHANNELS: u16 = 2;
/// P1 目标多声道容器宽度（4.1.2 的 8 容器/7 角色；实际宽度由 T57 解析校验）。
pub const TARGET_CHANNELS: u16 = P1_412_CHANNELS;
/// AES67 逻辑 12 通道（H16 7.1.4 两条 flow 8+4）。
pub const AES67_LOGIC_CHANNELS: u16 = 12;
/// Dante/Inferno 容器 16 通道（T43 H16Container::container16）。
pub const DANTE_CHANNELS: u16 = H16_CONTAINER_CHANNELS;

/// AES67 B1 PTP domain（daemon.conf ptp_domain，A12）。
pub const AES67_PTP_DOMAIN: u8 = 0;
/// 板端实测 Inferno device id（inferno 运行验证：00000a00020f0001）。
pub const INFERNO_DEVICE_ID: &str = "00000a00020f0001";
/// Statime PTPv1 虚拟钟绑定网卡（A14；板端实验口径 end0）。
pub const INFERNO_BIND_IFACE: &str = "end0";

/// 禁用标记（G-no-C）：任何服务单元名含这些子串即视为违规（本模块应为 0 命中）。
pub const FORBIDDEN_SERVICE_MARKERS: [&str; 3] = ["camilladsp", "tspi-camilladsp", "camilla-"];

/// 全部可选模式（A01；canonical 小写，与 T42 ALLOWED_MODES 同一集合）。
pub const SELECTABLE_MODES: [ControlMode; 5] = [
    ControlMode::Off,
    ControlMode::Toslink,
    ControlMode::TargetMultichannel,
    ControlMode::Aes67,
    ControlMode::Dante,
];

/// ALLOWED_MODES 的只读视图（供 T26/T70 生成 profiles 列表，不复制第二份）。
pub fn allowed_mode_names() -> &'static [&'static str] {
    &ALLOWED_MODES
}

// ---------------------------------------------------------------------------
// 模拟服务（A07/A11；无 systemd、无设备、无线程）
// ---------------------------------------------------------------------------

/// A11 的实际 overlay 服务单元（只列模式编排会启停的；tspi-camilladsp 不在枚举里）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServiceUnit {
    /// tspi-aes67-daemon.service（B1：内建 PTPv2 slave + AES67 端点，A09）。
    Aes67Daemon,
    /// tspi-ravenna-lkm.service（MergingRavennaALSA.ko，AES67 设备侧）。
    RavennaLkm,
    /// tspi-statime-inferno.service（PTPv1 follower + usrvclock，A08/A14）。
    StatimeInferno,
    /// tspi-ptp4l-master.service（仅 B2/B3；P1 B1 默认不启，A10/K44）。
    Ptp4lMaster,
    /// tspi-phc2sys.service（仅 B2/B3；P1 B1 默认不启，A10/K44）。
    Phc2sys,
}

impl ServiceUnit {
    /// 全部单元（状态导出/清理顺序用）。
    pub const ALL: [ServiceUnit; 5] = [
        ServiceUnit::Aes67Daemon,
        ServiceUnit::RavennaLkm,
        ServiceUnit::StatimeInferno,
        ServiceUnit::Ptp4lMaster,
        ServiceUnit::Phc2sys,
    ];

    /// actual unit 名（A11；日志/证据用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ServiceUnit::Aes67Daemon => "tspi-aes67-daemon",
            ServiceUnit::RavennaLkm => "tspi-ravenna-lkm",
            ServiceUnit::StatimeInferno => "tspi-statime-inferno",
            ServiceUnit::Ptp4lMaster => "tspi-ptp4l-master",
            ServiceUnit::Phc2sys => "tspi-phc2sys",
        }
    }

    /// 是否内核驱动/LKM（启动失败按设备类 exit 4，而不是 daemon 自检 8）。
    pub const fn is_driver(self) -> bool {
        matches!(self, ServiceUnit::RavennaLkm)
    }

    /// 是否提供 PTP 时钟源（启动会改变 ptp_lock 模型状态）。
    pub const fn provides_ptp(self) -> bool {
        matches!(
            self,
            ServiceUnit::Aes67Daemon | ServiceUnit::StatimeInferno | ServiceUnit::Ptp4lMaster
        )
    }

    /// 是否命中禁用标记（G-no-C 守卫；正常恒 false）。
    pub fn is_forbidden(self) -> bool {
        let name = self.as_str();
        FORBIDDEN_SERVICE_MARKERS
            .iter()
            .any(|marker| name.contains(marker))
    }

    /// 索引（固定数组状态用）。
    const fn index(self) -> usize {
        match self {
            ServiceUnit::Aes67Daemon => 0,
            ServiceUnit::RavennaLkm => 1,
            ServiceUnit::StatimeInferno => 2,
            ServiceUnit::Ptp4lMaster => 3,
            ServiceUnit::Phc2sys => 4,
        }
    }
}

/// 模拟服务运行态（systemd 语义子集；无 socket/dependency 自动拉起）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceRunState {
    /// 未运行。
    Inactive,
    /// 启动中。
    Activating,
    /// 已就绪（active）。
    Active,
    /// 停止中。
    Deactivating,
    /// 启动/运行失败（保留 failed 供 last_error/证据）。
    Failed,
}

impl ServiceRunState {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ServiceRunState::Inactive => "inactive",
            ServiceRunState::Activating => "activating",
            ServiceRunState::Active => "active",
            ServiceRunState::Deactivating => "deactivating",
            ServiceRunState::Failed => "failed",
        }
    }

    /// 是否占用设备/需要先停。
    pub const fn is_busy(self) -> bool {
        matches!(
            self,
            ServiceRunState::Activating | ServiceRunState::Active | ServiceRunState::Deactivating
        )
    }
}

/// 服务状态投影（状态 JSON/证据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStatus {
    /// 单元。
    pub unit: ServiceUnit,
    /// 运行态。
    pub state: ServiceRunState,
}

impl ServiceStatus {
    /// JSON 投影。
    pub fn to_json(self) -> JsonValue {
        JsonValue::obj(vec![
            ("unit", JsonValue::str(self.unit.as_str())),
            ("state", JsonValue::str(self.state.as_str())),
            (
                "active",
                JsonValue::Bool(self.state == ServiceRunState::Active),
            ),
        ])
    }
}

/// 模拟服务调用记录（顺序断言用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceCall {
    /// 启动某单元。
    Start(ServiceUnit),
    /// 停止某单元。
    Stop(ServiceUnit),
}

/// 服务层错误（映射到模式错误；不跨 RT 边界）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// unit 不存在/未安装（A03 exit 3）。
    NotPresent(ServiceUnit),
    /// unit 启动失败（driver -> exit 4；daemon/自检 -> exit 8，对齐 A16 板端 rc=8）。
    StartFailed(ServiceUnit),
    /// unit 停止失败（设备可能仍被占用 -> exit 4）。
    StopFailed(ServiceUnit),
    /// PTP 未在窗口内锁定（A03 exit 5）。
    PtpTimeout,
    /// AoIP 端点未就绪（A03 exit 6）。
    AoipNotReady(String),
}

impl ServiceError {
    /// 机器名（日志/证据）。
    pub fn name(&self) -> &'static str {
        match self {
            ServiceError::NotPresent(_) => "service_not_present",
            ServiceError::StartFailed(_) => "service_start_failed",
            ServiceError::StopFailed(_) => "service_stop_failed",
            ServiceError::PtpTimeout => "ptp_timeout",
            ServiceError::AoipNotReady(_) => "aoip_not_ready",
        }
    }
}

/// 模式编排依赖的服务/时钟探测面（生产由 T26 接 systemd/clock，测试用 SimulatedServices）。
pub trait ModeServices: Send {
    /// 启动单元（幂等；Active 再启返回 Ok）。
    fn start(&mut self, unit: ServiceUnit) -> Result<(), ServiceError>;
    /// 停止单元（幂等；Inactive 再停返回 Ok）。
    fn stop(&mut self, unit: ServiceUnit) -> Result<(), ServiceError>;
    /// 单元当前状态。
    fn state(&self, unit: ServiceUnit) -> ServiceRunState;
    /// 唯一 PTP 锁状态（A15：每 mode 唯一 PTP 来源）。
    fn ptp_lock(&self) -> LockState;
    /// AES67 daemon 实际状态（A12 status_file 摘要；不 ready = 端点不可用）。
    fn aes67_service(&self) -> Aes67ServiceState;
    /// 批量状态投影（默认由 state 派生）。
    fn snapshot(&self, units: &[ServiceUnit]) -> Vec<ServiceStatus> {
        units
            .iter()
            .map(|unit| ServiceStatus {
                unit: *unit,
                state: self.state(*unit),
            })
            .collect()
    }

    /// 诊断/测试下取回内存模拟实现（生产实现返回 None；不改变语义）。
    fn as_simulated(&self) -> Option<&SimulatedServices> {
        None
    }

    /// 诊断/测试下取回内存模拟实现（可变；生产实现返回 None）。
    fn as_simulated_mut(&mut self) -> Option<&mut SimulatedServices> {
        None
    }
}

/// 内存模拟服务（无 systemd/设备/线程；可注入失败与状态，供 T44 私有测试与 T26 开发回环）。
#[derive(Clone, Debug)]
pub struct SimulatedServices {
    states: [ServiceRunState; 5],
    ptp_lock: LockState,
    lock_after_start: LockState,
    aes67_service: Aes67ServiceState,
    aes67_after_start: Aes67ServiceState,
    missing: [bool; 5],
    fail_start: [bool; 5],
    fail_stop: [bool; 5],
    calls: Vec<ServiceCall>,
}

impl Default for SimulatedServices {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulatedServices {
    /// 默认：全 inactive、PTP unlocked；启动 PTP 提供者后自动 Locked、启动 AES67 daemon 后
    /// service ready（48k/domain 0）——只在模型里成立，不是板端事实。
    pub fn new() -> Self {
        SimulatedServices {
            states: [ServiceRunState::Inactive; 5],
            ptp_lock: LockState::Unlocked,
            lock_after_start: LockState::Locked,
            aes67_service: Aes67ServiceState::new(false, 0, 0),
            aes67_after_start: Aes67ServiceState::new(true, AES67_RATE_HZ, AES67_PTP_DOMAIN),
            missing: [false; 5],
            fail_start: [false; 5],
            fail_stop: [false; 5],
            calls: Vec::new(),
        }
    }

    /// 启动后 PTP 得到的锁状态（默认 Locked；设 Unlocked/Locking 模拟超时）。
    pub fn with_lock_after_start(mut self, lock: LockState) -> Self {
        self.lock_after_start = lock;
        self
    }

    /// AES67 daemon 启动后上报的服务状态（默认 ready/48k/domain0）。
    pub fn with_aes67_after_start(mut self, service: Aes67ServiceState) -> Self {
        self.aes67_after_start = service;
        self
    }

    /// 当前 PTP 锁状态。
    pub const fn lock(&self) -> LockState {
        self.ptp_lock
    }

    /// 直接设置 PTP 锁（测试/模拟外部 GM）。
    pub fn set_lock(&mut self, lock: LockState) {
        self.ptp_lock = lock;
    }

    /// 直接设置 AES67 服务状态（测试 daemon 未 ready/速率/domain 不符）。
    pub fn set_aes67_service(&mut self, service: Aes67ServiceState) {
        self.aes67_service = service;
    }

    /// 标记单元缺失（启动 -> NotPresent）。
    pub fn mark_missing(&mut self, unit: ServiceUnit) {
        self.missing[unit.index()] = true;
    }

    /// 注入下一次启动失败（启动 -> StartFailed，状态 Failed）。
    pub fn fail_next_start(&mut self, unit: ServiceUnit) {
        self.fail_start[unit.index()] = true;
    }

    /// 注入下一次停止失败（停止 -> StopFailed，状态保留）。
    pub fn fail_next_stop(&mut self, unit: ServiceUnit) {
        self.fail_stop[unit.index()] = true;
    }

    /// 调用记录（按发生顺序）。
    pub fn calls(&self) -> &[ServiceCall] {
        &self.calls
    }

    /// 清空调用记录。
    pub fn clear_calls(&mut self) {
        self.calls.clear();
    }

    /// 启动过的单元（按启动顺序，去重后保持首次出现顺序）。
    pub fn started_units(&self) -> Vec<ServiceUnit> {
        let mut out = Vec::new();
        for call in &self.calls {
            if let ServiceCall::Start(unit) = call {
                if !out.contains(unit) {
                    out.push(*unit);
                }
            }
        }
        out
    }

    /// 当前 Active 单元。
    pub fn active_units(&self) -> Vec<ServiceUnit> {
        ServiceUnit::ALL
            .iter()
            .copied()
            .filter(|unit| self.states[unit.index()] == ServiceRunState::Active)
            .collect()
    }
}

impl ModeServices for SimulatedServices {
    fn start(&mut self, unit: ServiceUnit) -> Result<(), ServiceError> {
        debug_assert!(
            !unit.is_forbidden(),
            "forbidden CamillaDSP unit in mode profile"
        );
        self.calls.push(ServiceCall::Start(unit));
        if self.missing[unit.index()] {
            return Err(ServiceError::NotPresent(unit));
        }
        if self.states[unit.index()] == ServiceRunState::Active {
            return Ok(());
        }
        if self.fail_start[unit.index()] {
            self.fail_start[unit.index()] = false;
            self.states[unit.index()] = ServiceRunState::Failed;
            return Err(ServiceError::StartFailed(unit));
        }
        self.states[unit.index()] = ServiceRunState::Activating;
        self.states[unit.index()] = ServiceRunState::Active;
        if unit.provides_ptp() {
            self.ptp_lock = self.lock_after_start;
        }
        if unit == ServiceUnit::Aes67Daemon {
            self.aes67_service = self.aes67_after_start;
        }
        Ok(())
    }

    fn stop(&mut self, unit: ServiceUnit) -> Result<(), ServiceError> {
        self.calls.push(ServiceCall::Stop(unit));
        if self.states[unit.index()] == ServiceRunState::Inactive {
            return Ok(());
        }
        if self.fail_stop[unit.index()] {
            self.fail_stop[unit.index()] = false;
            return Err(ServiceError::StopFailed(unit));
        }
        self.states[unit.index()] = ServiceRunState::Deactivating;
        self.states[unit.index()] = ServiceRunState::Inactive;
        if unit.provides_ptp() {
            self.ptp_lock = LockState::Unlocked;
        }
        if unit == ServiceUnit::Aes67Daemon {
            self.aes67_service = Aes67ServiceState::new(false, 0, 0);
        }
        Ok(())
    }

    fn state(&self, unit: ServiceUnit) -> ServiceRunState {
        self.states[unit.index()]
    }

    fn ptp_lock(&self) -> LockState {
        self.ptp_lock
    }

    fn aes67_service(&self) -> Aes67ServiceState {
        self.aes67_service
    }

    fn as_simulated(&self) -> Option<&SimulatedServices> {
        Some(self)
    }

    fn as_simulated_mut(&mut self) -> Option<&mut SimulatedServices> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// 模式 profile（A01/A07-A10/A12/A14；所有值来自契约，不读板端）
// ---------------------------------------------------------------------------

/// 模式输入来源（P1：Toslink 立体声 / 预渲染多声道文件 / AoIP 网络端点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeInput {
    /// 无输入（off）。
    None,
    /// MOTU Optical1/2 的 Toslink 2.0 立体声（stereo strict 直出）。
    ToslinkStereo,
    /// 预渲染多声道 file 输入（WBS T74 file 路径；非离线 dump）。
    FileMultichannel,
    /// AES67 网络端点（H16 等；AES67/RAVENNA 协议保持）。
    Aes67Network,
    /// Dante/Inferno 网络端点（inferno ALSA 插件/pipe）。
    DanteNetwork,
}

impl ModeInput {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ModeInput::None => "none",
            ModeInput::ToslinkStereo => "toslink_stereo",
            ModeInput::FileMultichannel => "file_multichannel",
            ModeInput::Aes67Network => "aes67_network",
            ModeInput::DanteNetwork => "dante_network",
        }
    }

    /// 是否网络端点（需要 AoIP 就绪自检）。
    pub const fn is_network(self) -> bool {
        matches!(self, ModeInput::Aes67Network | ModeInput::DanteNetwork)
    }
}

const NO_UNITS: [ServiceUnit; 0] = [];
const AES67_PTP_UNITS: [ServiceUnit; 1] = [ServiceUnit::Aes67Daemon];
const AES67_DATA_UNITS: [ServiceUnit; 1] = [ServiceUnit::RavennaLkm];
const DANTE_PTP_UNITS: [ServiceUnit; 1] = [ServiceUnit::StatimeInferno];
const DANTE_DATA_UNITS: [ServiceUnit; 0] = [];

/// 一个模式的静态 profile（编排 + actual 返回；layout 对 target 由 T57 解析结果带入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeProfile {
    /// 模式。
    pub mode: ControlMode,
    /// R10/R11 actual profile 名。
    pub name: &'static str,
    /// 输入来源。
    pub input: ModeInput,
    /// 实际采样率（48k/96k）。
    pub sample_rate: u32,
    /// chunksize（64/128）。
    pub chunksize: u16,
    /// 输入/容器通道数（off=0）。
    pub channels: u16,
    /// 设备格式标签（MOTU S24_3LE；AoIP 设备 S32_LE，T43 device_format）。
    pub format: &'static str,
    /// 输出布局（target 模式为解析出的 target layout；其余固定/None）。
    pub layout: Option<String>,
    /// 唯一 PTP 来源（A15；off/toslink/target = None）。
    pub ptp_source: Option<PtpSource>,
    /// 唯一 clock servo owner（A15；off = None）。
    pub servo_owner: Option<ServoOwner>,
    /// 先启的 PTP 单元（等锁前）。
    pub ptp_units: &'static [ServiceUnit],
    /// 等锁后启的数据面单元。
    pub data_units: &'static [ServiceUnit],
    /// 是否要求 PTP 锁（A03 exit 5）。
    pub requires_ptp_lock: bool,
    /// toslink stereo strict 标记（2.0 直出，其余输出静音，不实现上混）。
    pub stereo_strict: bool,
}

impl ModeProfile {
    /// 由模式 + 已解析 target 构造 profile（target 只影响 target_multichannel 的布局/宽度）。
    pub fn for_mode(mode: ControlMode, target: &TargetSpec) -> Result<ModeProfile, ModeError> {
        match mode {
            ControlMode::Off => Ok(ModeProfile {
                mode,
                name: PROFILE_OFF,
                input: ModeInput::None,
                sample_rate: MODE_PLAYBACK_RATE_HZ,
                chunksize: MODE_DEFAULT_CHUNKSIZE,
                channels: 0,
                format: "-",
                layout: None,
                ptp_source: None,
                servo_owner: None,
                ptp_units: &NO_UNITS,
                data_units: &NO_UNITS,
                requires_ptp_lock: false,
                stereo_strict: false,
            }),
            ControlMode::Toslink => {
                check_rate_and_chunksize(mode, MODE_PLAYBACK_RATE_HZ, MODE_DEFAULT_CHUNKSIZE)?;
                Ok(ModeProfile {
                    mode,
                    name: PROFILE_TOSLINK,
                    input: ModeInput::ToslinkStereo,
                    sample_rate: MODE_PLAYBACK_RATE_HZ,
                    chunksize: MODE_DEFAULT_CHUNKSIZE,
                    channels: TOSLINK_CHANNELS,
                    format: "S24_3LE",
                    layout: Some("2.0".to_string()),
                    ptp_source: None,
                    servo_owner: Some(ServoOwner::MotuFrame),
                    ptp_units: &NO_UNITS,
                    data_units: &NO_UNITS,
                    requires_ptp_lock: false,
                    stereo_strict: true,
                })
            }
            ControlMode::TargetMultichannel => {
                check_rate_and_chunksize(mode, MODE_PLAYBACK_RATE_HZ, MODE_DEFAULT_CHUNKSIZE)?;
                if target.channels == 0 || usize::from(target.channels) > MAX_OUTPUT_CHANNELS {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        format!(
                            "target channels {} must be 1..={MAX_OUTPUT_CHANNELS}",
                            target.channels
                        ),
                    ));
                }
                Ok(ModeProfile {
                    mode,
                    name: PROFILE_TARGET_MULTICHANNEL,
                    input: ModeInput::FileMultichannel,
                    sample_rate: MODE_PLAYBACK_RATE_HZ,
                    chunksize: MODE_DEFAULT_CHUNKSIZE,
                    channels: target.channels,
                    format: "S24_3LE",
                    layout: Some(target.layout.clone()),
                    ptp_source: None,
                    servo_owner: Some(ServoOwner::MotuFrame),
                    ptp_units: &NO_UNITS,
                    data_units: &NO_UNITS,
                    requires_ptp_lock: false,
                    stereo_strict: false,
                })
            }
            ControlMode::Aes67 => {
                check_rate_and_chunksize(mode, AES67_RATE_HZ, MODE_DEFAULT_CHUNKSIZE)?;
                Ok(ModeProfile {
                    mode,
                    name: PROFILE_AES67,
                    input: ModeInput::Aes67Network,
                    sample_rate: AES67_RATE_HZ,
                    chunksize: MODE_DEFAULT_CHUNKSIZE,
                    channels: AES67_LOGIC_CHANNELS,
                    format: "S32_LE",
                    layout: None,
                    ptp_source: Some(AoipProtocol::Aes67.expected_ptp_source()),
                    servo_owner: Some(ServoOwner::Ptp),
                    ptp_units: &AES67_PTP_UNITS,
                    data_units: &AES67_DATA_UNITS,
                    requires_ptp_lock: true,
                    stereo_strict: false,
                })
            }
            ControlMode::Dante => {
                check_rate_and_chunksize(mode, DANTE_RATE_HZ, MODE_DEFAULT_CHUNKSIZE)?;
                Ok(ModeProfile {
                    mode,
                    name: PROFILE_DANTE,
                    input: ModeInput::DanteNetwork,
                    sample_rate: DANTE_RATE_HZ,
                    chunksize: MODE_DEFAULT_CHUNKSIZE,
                    channels: DANTE_CHANNELS,
                    format: "S32_LE",
                    layout: None,
                    ptp_source: Some(AoipProtocol::Inferno.expected_ptp_source()),
                    servo_owner: Some(ServoOwner::Ptp),
                    ptp_units: &DANTE_PTP_UNITS,
                    data_units: &DANTE_DATA_UNITS,
                    requires_ptp_lock: true,
                    stereo_strict: false,
                })
            }
        }
    }

    /// 是否播放模式（target 布局/输出有意义）。
    pub const fn is_playback(&self) -> bool {
        self.mode.is_playback()
    }

    /// 全部会被启停的单元（先 PTP 后 data；停止逆序）。
    pub fn units(&self) -> Vec<ServiceUnit> {
        let mut units = Vec::with_capacity(self.ptp_units.len() + self.data_units.len());
        units.extend_from_slice(self.ptp_units);
        units.extend_from_slice(self.data_units);
        units
    }

    /// profile 里是否出现禁用单元（G-no-C；正常恒 false）。
    pub fn forbidden_units(&self) -> Vec<ServiceUnit> {
        self.units()
            .into_iter()
            .filter(|unit| unit.is_forbidden())
            .collect()
    }
}

fn check_rate_and_chunksize(
    mode: ControlMode,
    sample_rate: u32,
    chunksize: u16,
) -> Result<(), ModeError> {
    if !SAMPLE_RATES_HZ.contains(&sample_rate) {
        return Err(ModeError::new(
            ModeErrorKind::SelfCheckFailed,
            format!(
                "mode {} sample rate {sample_rate} not in {SAMPLE_RATES_HZ:?}",
                mode.as_str()
            ),
        ));
    }
    if !BLOCK_SIZES.contains(&chunksize) {
        return Err(ModeError::new(
            ModeErrorKind::SelfCheckFailed,
            format!(
                "mode {} chunksize {chunksize} not in {BLOCK_SIZES:?}",
                mode.as_str()
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 设备 owner / 状态 / 回退策略 / 错误码（core-v1 §5.1/§5.4；A03/A16）
// ---------------------------------------------------------------------------

/// 音频设备唯一 owner（core-v1 §5.4；calibration 与 playback 不得同时打开设备）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceOwner {
    /// 无进程打开 ALSA 设备。
    Idle,
    /// 正常播放 host 持有设备。
    Playback,
    /// 校准 daemon 独占（扫频/采集）。
    Calibration,
}

impl DeviceOwner {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            DeviceOwner::Idle => "idle",
            DeviceOwner::Playback => "playback",
            DeviceOwner::Calibration => "calibration",
        }
    }
}

/// 状态机 phase（A04 state / R10 state；具体切换步骤见 SwitchReport）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeStatePhase {
    /// 稳定（无在途切换）。
    Idle,
    /// 切换在途（REST 202 只表示已接受）。
    Switching,
    /// 最近一次切换失败并已回退（A16 state=error）。
    Error,
}

impl ModeStatePhase {
    /// 机器名（A04 legacy state 取值 idle/switching/error）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ModeStatePhase::Idle => "idle",
            ModeStatePhase::Switching => "switching",
            ModeStatePhase::Error => "error",
        }
    }
}

/// 失败回退策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackPolicy {
    /// 默认（A16）：失败后 mode=off + state=error + last_error；用户听音路由不变。
    Off,
    /// 新 host 自研 last-good：失败后重启最近一次成功的播放模式（非旧 C）。
    LastGood,
}

impl FallbackPolicy {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            FallbackPolicy::Off => "off",
            FallbackPolicy::LastGood => "last_good",
        }
    }
}

/// 实际生效的回退动作（状态 JSON/证据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackAction {
    /// 无回退（成功切换）。
    None,
    /// 已回退到 off（A16 默认）。
    Off,
    /// 已恢复到新 host last-good 模式。
    LastGood,
}

impl FallbackAction {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            FallbackAction::None => "none",
            FallbackAction::Off => "off",
            FallbackAction::LastGood => "last_good",
        }
    }
}

/// 模式错误类别（A03 退出码 + core-v1 §9.2 RtStatus 双映射；T42 REST 用 rt_status）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeErrorKind {
    /// 未知模式（exit 1；REST 400 unknown_mode 由 T42 parse 侧给）。
    UnknownMode,
    /// 参数错误（exit 2）。
    InvalidArgument,
    /// 陈旧期望（schema/hash 原子切换的 stale 拒绝；exit 2；RtStatus 119）。
    Stale,
    /// 前置缺失（unit 不存在/配置缺失；exit 3）。
    PrerequisiteMissing,
    /// 设备不可用（driver 启停失败/参数不符；exit 4）。
    DeviceUnavailable,
    /// 设备被其它 owner 占用（calibration；exit 4 / REST 409 device_busy）。
    DeviceBusy,
    /// PTP 未锁（exit 5）。
    PtpNotLocked,
    /// AoIP 端点未就绪（exit 6）。
    AoipNotReady,
    /// DSP/host 启动失败（epoch/servo 建立失败；exit 7）。
    DspStartFailed,
    /// 自检失败（daemon 缺库/服务名实/格式不符；exit 8，对齐 A16 板端 aes67 rc=8）。
    SelfCheckFailed,
    /// 已回滚（exit 9；状态 last_exit_code 用）。
    RolledBack,
    /// 另一切换在途（exit 10 / REST 409 switch_busy）。
    SwitchBusy,
}

impl ModeErrorKind {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ModeErrorKind::UnknownMode => "unknown_mode",
            ModeErrorKind::InvalidArgument => "invalid_argument",
            ModeErrorKind::Stale => "stale",
            ModeErrorKind::PrerequisiteMissing => "prerequisite_missing",
            ModeErrorKind::DeviceUnavailable => "device_unavailable",
            ModeErrorKind::DeviceBusy => "device_busy",
            ModeErrorKind::PtpNotLocked => "ptp_not_locked",
            ModeErrorKind::AoipNotReady => "aoip_not_ready",
            ModeErrorKind::DspStartFailed => "dsp_start_failed",
            ModeErrorKind::SelfCheckFailed => "self_check_failed",
            ModeErrorKind::RolledBack => "rolled_back",
            ModeErrorKind::SwitchBusy => "switch_busy",
        }
    }

    /// A03 退出码（0 就绪/1 未知/2 参数/3 前置/4 设备/5 PTP/6 AoIP/7 DSP/8 自检/9 已回滚/10 在途）。
    pub const fn exit_code(self) -> u8 {
        match self {
            ModeErrorKind::UnknownMode => 1,
            ModeErrorKind::InvalidArgument | ModeErrorKind::Stale => 2,
            ModeErrorKind::PrerequisiteMissing => 3,
            ModeErrorKind::DeviceUnavailable | ModeErrorKind::DeviceBusy => 4,
            ModeErrorKind::PtpNotLocked => 5,
            ModeErrorKind::AoipNotReady => 6,
            ModeErrorKind::DspStartFailed => 7,
            ModeErrorKind::SelfCheckFailed => 8,
            ModeErrorKind::RolledBack => 9,
            ModeErrorKind::SwitchBusy => 10,
        }
    }

    /// RtStatus（T42 rt_status_to_error / T24 WS 映射消费）。
    ///
    /// core-v1 §9.2 已冻结码 101–123 原样复用；唯一例外是 RolledBack：它用 control 层
    /// 扩展码 124（`ws::RT_STATUS_MODE_ROLLED_BACK`），因为 119 只表示 plan stale。
    pub const fn rt_status(self) -> RtStatus {
        match self {
            ModeErrorKind::UnknownMode => RtStatus::ERR_UNSUPPORTED,
            ModeErrorKind::InvalidArgument => RtStatus::ERR_UNSUPPORTED,
            ModeErrorKind::Stale => RtStatus::ERR_PLAN_STALE,
            ModeErrorKind::PrerequisiteMissing => RtStatus::ERR_UNSUPPORTED,
            ModeErrorKind::DeviceUnavailable => RtStatus::ERR_DEVICE_PARAMS,
            ModeErrorKind::DeviceBusy => RtStatus::ERR_DEVICE_BUSY,
            ModeErrorKind::PtpNotLocked => RtStatus::ERR_ADMISSION_CLOCK,
            ModeErrorKind::AoipNotReady => RtStatus::RT_E_DEVICE_LOST,
            ModeErrorKind::DspStartFailed => RtStatus::ERR_ADMISSION_MEMORY,
            ModeErrorKind::SelfCheckFailed => RtStatus::ERR_HASH,
            // F-30：回滚不复用 119（119 只表示 plan stale），用 control 层专用扩展码
            // 124（compat::ws::RT_STATUS_MODE_ROLLED_BACK；REST 409 rolled_back）。
            ModeErrorKind::RolledBack => RtStatus::from_code(ws::RT_STATUS_MODE_ROLLED_BACK),
            ModeErrorKind::SwitchBusy => RtStatus::ERR_SWITCH_BUSY,
        }
    }
}

/// 模式错误（NONRT；带人读 detail 与回滚标记）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeError {
    /// 类别。
    pub kind: ModeErrorKind,
    /// 细节（证据/日志；不含机密）。
    pub detail: String,
    /// 是否已执行回退（切换失败路径恒 true）。
    pub rolled_back: bool,
}

impl ModeError {
    /// 构造（未回退）。
    pub fn new(kind: ModeErrorKind, detail: impl Into<String>) -> Self {
        ModeError {
            kind,
            detail: detail.into(),
            rolled_back: false,
        }
    }

    /// 标记已回退。
    pub fn with_rolled_back(mut self) -> Self {
        self.rolled_back = true;
        self
    }

    /// A03 退出码（主因）。
    pub const fn exit_code(&self) -> u8 {
        self.kind.exit_code()
    }

    /// RtStatus（REST 映射用）。
    pub const fn rt_status(&self) -> RtStatus {
        self.kind.rt_status()
    }

    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("kind", JsonValue::str(self.kind.as_str())),
            ("detail", JsonValue::str(self.detail.as_str())),
            ("exit_code", JsonValue::from(self.exit_code())),
            ("rt_status", JsonValue::from(self.rt_status().0)),
            ("rolled_back", JsonValue::Bool(self.rolled_back)),
        ])
    }
}

/// A03 完整退出码枚举（0..=10 全覆盖；K39 判据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeExitCode {
    /// 0：就绪。
    Ready,
    /// 1：未知模式。
    Unknown,
    /// 2：参数错误。
    Argument,
    /// 3：前置缺失。
    Prerequisite,
    /// 4：设备错误/占用。
    Device,
    /// 5：PTP 未锁。
    PtpNotLocked,
    /// 6：AoIP 流未就绪。
    AoipNotReady,
    /// 7：DSP 启动失败。
    DspStartFailed,
    /// 8：自检失败。
    SelfCheckFailed,
    /// 9：已回滚。
    RolledBack,
    /// 10：另一切换中。
    SwitchBusy,
}

impl ModeExitCode {
    /// u8 值。
    pub const fn as_u8(self) -> u8 {
        match self {
            ModeExitCode::Ready => 0,
            ModeExitCode::Unknown => 1,
            ModeExitCode::Argument => 2,
            ModeExitCode::Prerequisite => 3,
            ModeExitCode::Device => 4,
            ModeExitCode::PtpNotLocked => 5,
            ModeExitCode::AoipNotReady => 6,
            ModeExitCode::DspStartFailed => 7,
            ModeExitCode::SelfCheckFailed => 8,
            ModeExitCode::RolledBack => 9,
            ModeExitCode::SwitchBusy => 10,
        }
    }

    /// 由 u8 解析（0..=10；越界 None）。
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(ModeExitCode::Ready),
            1 => Some(ModeExitCode::Unknown),
            2 => Some(ModeExitCode::Argument),
            3 => Some(ModeExitCode::Prerequisite),
            4 => Some(ModeExitCode::Device),
            5 => Some(ModeExitCode::PtpNotLocked),
            6 => Some(ModeExitCode::AoipNotReady),
            7 => Some(ModeExitCode::DspStartFailed),
            8 => Some(ModeExitCode::SelfCheckFailed),
            9 => Some(ModeExitCode::RolledBack),
            10 => Some(ModeExitCode::SwitchBusy),
            _ => None,
        }
    }

    /// 由错误类别映射（RolledBack 类别 -> 9）。
    pub const fn from_kind(kind: ModeErrorKind) -> Self {
        match kind.exit_code() {
            0 => ModeExitCode::Ready,
            1 => ModeExitCode::Unknown,
            2 => ModeExitCode::Argument,
            3 => ModeExitCode::Prerequisite,
            4 => ModeExitCode::Device,
            5 => ModeExitCode::PtpNotLocked,
            6 => ModeExitCode::AoipNotReady,
            7 => ModeExitCode::DspStartFailed,
            8 => ModeExitCode::SelfCheckFailed,
            9 => ModeExitCode::RolledBack,
            _ => ModeExitCode::SwitchBusy,
        }
    }

    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ModeExitCode::Ready => "ready",
            ModeExitCode::Unknown => "unknown",
            ModeExitCode::Argument => "argument",
            ModeExitCode::Prerequisite => "prerequisite",
            ModeExitCode::Device => "device",
            ModeExitCode::PtpNotLocked => "ptp_not_locked",
            ModeExitCode::AoipNotReady => "aoip_not_ready",
            ModeExitCode::DspStartFailed => "dsp_start_failed",
            ModeExitCode::SelfCheckFailed => "self_check_failed",
            ModeExitCode::RolledBack => "rolled_back",
            ModeExitCode::SwitchBusy => "switch_busy",
        }
    }
}

// ---------------------------------------------------------------------------
// 唯一 servo/epoch（core-v1 §5.2；T19 接口）
// ---------------------------------------------------------------------------

/// 模式时钟：每个模式唯一 servo owner + 每次切换新 epoch（禁止双闭环/跨 epoch 拼接）。
#[derive(Debug)]
struct ModeClock {
    epoch: EpochClock,
    servo: Option<ClockServo>,
    grant: Option<ServoGrant>,
    claims: u64,
}

impl ModeClock {
    fn new() -> Self {
        ModeClock {
            epoch: EpochClock::new(),
            servo: None,
            grant: None,
            claims: 0,
        }
    }

    fn epoch_id(&self) -> Option<u64> {
        self.epoch.current().map(|stamp| stamp.epoch_id)
    }

    fn owner(&self) -> Option<ServoOwner> {
        self.grant.map(|grant| grant.owner)
    }

    fn claims(&self) -> u64 {
        self.claims
    }

    /// 释放当前 servo（幂等；旧 grant 立即失效）。
    fn release(&mut self) {
        if let (Some(mut servo), Some(grant)) = (self.servo.take(), self.grant.take()) {
            let _ = servo.release(grant);
        }
    }

    /// 新 epoch + 唯一 servo claim（旧 servo 先释放；同一时刻最多一个 owner）。
    fn enter(&mut self, profile: &ModeProfile, now_mono_ns: u64) -> Result<u64, ModeError> {
        self.release();
        let epoch_id = match self.epoch.current() {
            Some(_) => {
                self.epoch
                    .on_discontinuity(DiscontinuityReason::Explicit, now_mono_ns, None, None)
                    .map_err(|error| clock_error(error, "new epoch"))?
                    .new_epoch_id
            }
            None => {
                self.epoch
                    .begin(now_mono_ns)
                    .map_err(|error| clock_error(error, "first epoch"))?
                    .epoch_id
            }
        };
        if let Some(owner) = profile.servo_owner {
            let mut servo = ClockServo::new(ServoConfig::default(), profile.sample_rate);
            let grant = servo
                .claim(owner)
                .map_err(|error| clock_error(error, "servo claim"))?;
            self.servo = Some(servo);
            self.grant = Some(grant);
            self.claims = self.claims.saturating_add(1);
        }
        Ok(epoch_id)
    }
}

fn clock_error(error: ClockError, context: &str) -> ModeError {
    ModeError::new(
        ModeErrorKind::DspStartFailed,
        format!("clock {context} failed: {error:?}"),
    )
}

// ---------------------------------------------------------------------------
// 状态快照（schema/hash 原子发布单元）
// ---------------------------------------------------------------------------

/// 模式状态快照（一次整体发布；T26 映射到 EngineSnapshot，T70 生成 Web 视图）。
#[derive(Clone, Debug)]
pub struct ModeState {
    /// schema（原子发布的一部分）。
    pub schema: &'static str,
    /// 修订号（每次成功/失败发布 +1；单调）。
    pub revision: u64,
    /// 切换序号（每次 switch 尝试 +1）。
    pub switch_seq: u64,
    /// phase（idle/switching/error）。
    pub phase: ModeStatePhase,
    /// 生效模式。
    pub mode: ControlMode,
    /// 生效 profile（off = None）。
    pub profile: Option<ModeProfile>,
    /// 解析后的 target（T57 绑定；config hash/target hash 来源）。
    pub target: TargetSpec,
    /// 设备唯一 owner。
    pub device_owner: DeviceOwner,
    /// 当前 epoch（每次模式提交新建；0 = 尚未建立）。
    pub epoch_id: u64,
    /// 唯一 servo owner。
    pub servo_owner: Option<ServoOwner>,
    /// servo claim 序号（进程内单调；旧 grant 切换后失效）。
    pub servo_claims: u64,
    /// 唯一 PTP 来源（A15）。
    pub ptp_source: Option<PtpSource>,
    /// PTP 锁状态。
    pub ptp_lock: LockState,
    /// actual 采样率。
    pub sample_rate: u32,
    /// actual chunksize。
    pub chunksize: u16,
    /// actual 通道数。
    pub channels: u16,
    /// actual 设备格式。
    pub format: &'static str,
    /// 输出布局（target 模式为 T57 布局；其余固定/空）。
    pub layout: String,
    /// target hash（T57/T42 TargetSpec::fingerprint）。
    pub target_hash: String,
    /// 配置 hash（T42 RuntimeConfig::fingerprint；schema/hash 原子切换绑定）。
    pub config_hash: String,
    /// 相关服务实际状态（模拟）。
    pub services: Vec<ServiceStatus>,
    /// 最近一次错误（成功切换清空）。
    pub last_error: Option<ModeError>,
    /// A03 退出码（成功 0；失败回退 9）。
    pub last_exit_code: u8,
    /// 最近一次回退动作。
    pub fallback: FallbackAction,
    /// 告警（如 allow-unlocked-ptp）。
    pub warnings: Vec<String>,
    /// 最近提交 unix 秒（调用方给，纯函数无系统时钟）。
    pub switched_unix: u64,
}

impl ModeState {
    /// 是否播放模式。
    pub const fn is_playback(&self) -> bool {
        self.mode.is_playback()
    }

    /// profile 名（R10 actual profile）。
    pub fn profile_name(&self) -> Option<&'static str> {
        self.profile.as_ref().map(|profile| profile.name)
    }

    /// T42 ModeAck（REST switch_mode 返回；validate_mode_ack 可接受的字段域）。
    pub fn to_mode_ack(&self) -> ModeAck {
        ModeAck {
            mode: self.mode,
            profile: self.profile_name().map(str::to_string),
            target_hash: self.target_hash.clone(),
            sample_rate: self.sample_rate,
            chunksize: self.chunksize,
            switch_state: match self.phase {
                ModeStatePhase::Idle => "switched".to_string(),
                _ => "switching".to_string(),
            },
        }
    }

    /// 该状态对应的 T42 RuntimeConfig（config hash 的唯一来源）。
    pub fn runtime_config(&self) -> RuntimeConfig {
        let profile = self.profile.as_ref();
        RuntimeConfig {
            mode: self.mode,
            sample_rate: profile
                .map(|profile| profile.sample_rate)
                .unwrap_or(MODE_PLAYBACK_RATE_HZ),
            chunksize: profile
                .map(|profile| profile.chunksize)
                .unwrap_or(MODE_DEFAULT_CHUNKSIZE),
            target: self.target.clone(),
            coeff: None,
        }
    }

    /// 状态 JSON（内部 schema；T26/T70 消费）。
    pub fn to_json(&self) -> JsonValue {
        let services = self
            .services
            .iter()
            .map(|status| status.to_json())
            .collect();
        let warnings = self
            .warnings
            .iter()
            .map(|warning| JsonValue::str(warning.as_str()))
            .collect();
        JsonValue::obj(vec![
            ("schema", JsonValue::str(self.schema)),
            ("revision", JsonValue::from(self.revision)),
            ("switch_seq", JsonValue::from(self.switch_seq)),
            ("state", JsonValue::str(self.phase.as_str())),
            ("mode", JsonValue::str(self.mode.as_str())),
            ("mode_label", JsonValue::str(self.mode.label())),
            ("profile", opt_str_json(self.profile_name())),
            (
                "input",
                match self.profile.as_ref() {
                    Some(profile) => JsonValue::str(profile.input.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("device_owner", JsonValue::str(self.device_owner.as_str())),
            ("epoch", JsonValue::from(self.epoch_id)),
            (
                "servo_owner",
                match self.servo_owner {
                    Some(owner) => JsonValue::str(owner.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("servo_claims", JsonValue::from(self.servo_claims)),
            (
                "ptp_source",
                match self.ptp_source {
                    Some(source) => JsonValue::str(source.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("ptp_lock", JsonValue::str(self.ptp_lock.as_str())),
            ("sample_rate", JsonValue::from(self.sample_rate)),
            ("chunksize", JsonValue::from(self.chunksize)),
            ("channels", JsonValue::from(self.channels)),
            ("format", JsonValue::str(self.format)),
            ("layout", JsonValue::str(self.layout.as_str())),
            ("target_hash", JsonValue::str(self.target_hash.as_str())),
            ("config_hash", JsonValue::str(self.config_hash.as_str())),
            ("services", JsonValue::Arr(services)),
            (
                "last_error",
                match &self.last_error {
                    Some(error) => error.to_json(),
                    None => JsonValue::Null,
                },
            ),
            ("last_exit_code", JsonValue::from(self.last_exit_code)),
            ("fallback", JsonValue::str(self.fallback.as_str())),
            ("warnings", JsonValue::Arr(warnings)),
            ("switched_unix", JsonValue::from(self.switched_unix)),
        ])
    }

    /// R10/A04 REST 兼容状态体（T42 MODE_STATUS_SCHEMA 顶层字段；不发明第二套字段名）。
    pub fn rest_status_json(&self) -> JsonValue {
        let progress = if self.phase == ModeStatePhase::Idle {
            100u32
        } else {
            0u32
        };
        JsonValue::obj(vec![
            ("schema", JsonValue::str(MODE_STATUS_SCHEMA)),
            ("mode", JsonValue::str(self.mode.as_str())),
            ("mode_label", JsonValue::str(self.mode.label())),
            ("target", JsonValue::Null),
            ("state", JsonValue::str(self.phase.as_str())),
            ("step", JsonValue::Null),
            ("progress", JsonValue::from(progress)),
            ("steps", JsonValue::Arr(Vec::new())),
            (
                "last_error",
                match &self.last_error {
                    Some(error) => error.to_json(),
                    None => JsonValue::Null,
                },
            ),
            ("warnings", JsonValue::Arr(Vec::new())),
            ("profile", opt_str_json(self.profile_name())),
            ("layout", JsonValue::str(self.layout.as_str())),
            ("target_hash", JsonValue::str(self.target_hash.as_str())),
            ("config_hash", JsonValue::str(self.config_hash.as_str())),
            ("sample_rate", JsonValue::from(self.sample_rate)),
            ("chunksize", JsonValue::from(self.chunksize)),
            ("channels", JsonValue::from(self.channels)),
            ("epoch", JsonValue::from(self.epoch_id)),
            ("switch_state", JsonValue::str(self.phase.as_str())),
            ("last_exit_code", JsonValue::from(self.last_exit_code)),
            ("mock", JsonValue::Bool(true)),
        ])
    }

    /// A04 legacy tspi-audio/v1 状态（字段名保持；camilladsp 恒 null，G-no-C）。
    pub fn legacy_status_json(&self) -> JsonValue {
        let outputs = self.legacy_outputs_json();
        let ptp = JsonValue::obj(vec![
            (
                "source",
                match self.ptp_source {
                    Some(source) => JsonValue::str(source.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("lock", JsonValue::str(self.ptp_lock.as_str())),
            (
                "role",
                opt_str_json(self.ptp_source.map(|source| source.as_str())),
            ),
        ]);
        let aoip = match self.profile.as_ref().map(|profile| profile.input) {
            Some(ModeInput::Aes67Network) => JsonValue::obj(vec![
                ("protocol", JsonValue::str("aes67")),
                ("sample_rate", JsonValue::from(self.sample_rate)),
                ("channels", JsonValue::from(self.channels)),
            ]),
            Some(ModeInput::DanteNetwork) => JsonValue::obj(vec![
                ("protocol", JsonValue::str("inferno")),
                ("sample_rate", JsonValue::from(self.sample_rate)),
                ("channels", JsonValue::from(self.channels)),
            ]),
            _ => JsonValue::Null,
        };
        JsonValue::obj(vec![
            ("schema", JsonValue::str(LEGACY_STATUS_SCHEMA)),
            ("ts", JsonValue::from(self.switched_unix)),
            ("mode", JsonValue::str(self.mode.as_str())),
            ("mode_label", JsonValue::str(self.mode.label())),
            ("state", JsonValue::str(self.phase.as_str())),
            ("layout", JsonValue::str(self.layout.as_str())),
            ("outputs", outputs),
            (
                "ptp_role",
                opt_str_json(self.ptp_source.map(|source| source.as_str())),
            ),
            ("ptp_lock", JsonValue::str(self.ptp_lock.as_str())),
            ("pid", JsonValue::Null),
            ("ptp", ptp),
            ("input_content", JsonValue::Null),
            ("aoip", aoip),
            ("camilladsp", JsonValue::Null),
            ("alsa", JsonValue::Null),
            (
                "last_error",
                match &self.last_error {
                    Some(error) => error.to_json(),
                    None => JsonValue::Null,
                },
            ),
            ("profile", opt_str_json(self.profile_name())),
            ("target_hash", JsonValue::str(self.target_hash.as_str())),
            ("config_hash", JsonValue::str(self.config_hash.as_str())),
            ("sample_rate", JsonValue::from(self.sample_rate)),
            ("chunksize", JsonValue::from(self.chunksize)),
            ("epoch", JsonValue::from(self.epoch_id)),
            ("switch_state", JsonValue::str(self.phase.as_str())),
            ("last_exit_code", JsonValue::from(self.last_exit_code)),
        ])
    }

    fn legacy_outputs_json(&self) -> JsonValue {
        if !self.mode.is_playback() {
            return JsonValue::Arr(Vec::new());
        }
        if matches!(self.mode, ControlMode::Toslink) {
            let items = ["FL", "FR"]
                .iter()
                .enumerate()
                .map(|(index, role)| {
                    JsonValue::obj(vec![
                        ("channel", JsonValue::from(index as u32)),
                        ("role", JsonValue::str(*role)),
                    ])
                })
                .collect();
            return JsonValue::Arr(items);
        }
        if matches!(self.mode, ControlMode::TargetMultichannel) {
            let items = self
                .target
                .roles
                .iter()
                .map(|entry| {
                    JsonValue::obj(vec![
                        ("channel", JsonValue::from(entry.dac)),
                        ("role", JsonValue::str(entry.role.as_str())),
                    ])
                })
                .collect();
            return JsonValue::Arr(items);
        }
        JsonValue::Arr(Vec::new())
    }
}

fn opt_str_json(value: Option<&str>) -> JsonValue {
    match value {
        Some(text) => JsonValue::str(text),
        None => JsonValue::Null,
    }
}

// ---------------------------------------------------------------------------
// 请求 / 报告（A03/A06/A07；R10 steps）
// ---------------------------------------------------------------------------

/// 一次模式切换请求（所有时间由调用方给，纯函数、可复现）。
#[derive(Clone, Debug)]
pub struct ModeRequest {
    /// 目标模式。
    pub mode: ControlMode,
    /// 期望的当前 config hash（stale 拒绝；None = 不校验）。
    pub expected_config_hash: Option<String>,
    /// mono 时刻（epoch 起点；由宿主单调时钟给）。
    pub now_mono_ns: u64,
    /// unix 秒（状态/证据）。
    pub now_unix: u64,
    /// 是否允许 PTP 未锁硬切（CLI --allow-unlocked-ptp；告警记录）。
    pub allow_unlocked_ptp: bool,
    /// dry-run：只算候选状态，不启停服务、不改状态。
    pub dry_run: bool,
    /// force：同模式也重新应用（默认同模式幂等）。
    pub force: bool,
}

impl ModeRequest {
    /// 最小请求（默认时间常量，便于确定性测试）。
    pub fn new(mode: ControlMode) -> Self {
        ModeRequest {
            mode,
            expected_config_hash: None,
            now_mono_ns: 1_000_000,
            now_unix: 1_764_000_000,
            allow_unlocked_ptp: false,
            dry_run: false,
            force: false,
        }
    }

    /// 带时间。
    pub fn at(mode: ControlMode, now_mono_ns: u64, now_unix: u64) -> Self {
        let mut request = ModeRequest::new(mode);
        request.now_mono_ns = now_mono_ns;
        request.now_unix = now_unix;
        request
    }

    /// 设置期望 hash。
    pub fn with_expected_config_hash(mut self, hash: impl Into<String>) -> Self {
        self.expected_config_hash = Some(hash.into());
        self
    }

    /// 允许未锁 PTP。
    pub fn with_allow_unlocked_ptp(mut self, allow: bool) -> Self {
        self.allow_unlocked_ptp = allow;
        self
    }

    /// dry-run。
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// force。
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

/// 切换步骤（R10 steps/证据；无实时耗时承诺）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchStep {
    /// 步骤名（stop/start/wait_ptp_lock/self_check/new_epoch/claim_servo/publish/...）。
    pub step: &'static str,
    /// 是否成功。
    pub ok: bool,
    /// 细节（unit 名/错误名）。
    pub detail: String,
}

impl SwitchStep {
    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("step", JsonValue::str(self.step)),
            ("ok", JsonValue::Bool(self.ok)),
            ("detail", JsonValue::str(self.detail.as_str())),
        ])
    }
}

/// 一次切换的编排报告（最后一份保存在 controller；证据/Web steps 用）。
#[derive(Clone, Debug)]
pub struct SwitchReport {
    /// 切换序号。
    pub seq: u64,
    /// 目标模式。
    pub mode: ControlMode,
    /// 已执行步骤。
    pub steps: Vec<SwitchStep>,
    /// 启动的单元（顺序）。
    pub started_units: Vec<ServiceUnit>,
    /// 停止的单元（顺序）。
    pub stopped_units: Vec<ServiceUnit>,
    /// 告警。
    pub warnings: Vec<String>,
    /// 新 epoch（失败回退后为回退态 epoch）。
    pub epoch_id: u64,
    /// 目标/回退后 config hash。
    pub config_hash: String,
    /// unix 秒。
    pub finished_unix: u64,
    /// 是否成功。
    pub ok: bool,
}

impl SwitchReport {
    fn new(seq: u64, mode: ControlMode, now_unix: u64) -> Self {
        SwitchReport {
            seq,
            mode,
            steps: Vec::new(),
            started_units: Vec::new(),
            stopped_units: Vec::new(),
            warnings: Vec::new(),
            epoch_id: 0,
            config_hash: String::new(),
            finished_unix: now_unix,
            ok: false,
        }
    }

    fn push_step(&mut self, step: &'static str, ok: bool, detail: impl Into<String>) {
        self.steps.push(SwitchStep {
            step,
            ok,
            detail: detail.into(),
        });
    }

    /// JSON 投影（R10 steps）。
    pub fn to_json(&self) -> JsonValue {
        let steps = self.steps.iter().map(SwitchStep::to_json).collect();
        let started = self
            .started_units
            .iter()
            .map(|unit| JsonValue::str(unit.as_str()))
            .collect();
        let stopped = self
            .stopped_units
            .iter()
            .map(|unit| JsonValue::str(unit.as_str()))
            .collect();
        let warnings = self
            .warnings
            .iter()
            .map(|warning| JsonValue::str(warning.as_str()))
            .collect();
        JsonValue::obj(vec![
            ("seq", JsonValue::from(self.seq)),
            ("mode", JsonValue::str(self.mode.as_str())),
            ("ok", JsonValue::Bool(self.ok)),
            ("steps", JsonValue::Arr(steps)),
            ("started_units", JsonValue::Arr(started)),
            ("stopped_units", JsonValue::Arr(stopped)),
            ("warnings", JsonValue::Arr(warnings)),
            ("epoch", JsonValue::from(self.epoch_id)),
            ("config_hash", JsonValue::str(self.config_hash.as_str())),
            ("finished_unix", JsonValue::from(self.finished_unix)),
        ])
    }
}

// ---------------------------------------------------------------------------
// MOTU/CueMix 只读快照（A22/P11：切换前后 diff=0，用户直通不动）
// ---------------------------------------------------------------------------
//
// F-29 口径：本模块只持有**进程内基线/模型**，任何切换都不写它。要让「diff=0」成为
// 设备事实，必须由 `CueMixSource` 在切换前后**真实读取**设备快照；没有读取器/无设备时
// 结果是 `CueMixCheck::not_run()`（NOT_RUN），不得再把内存模型的自比当作板端证据。

/// CueMix 真实读取面（F-29；生产由 T26/MOTU 控制面实现，测试可注入假读取器）。
///
/// `read_snapshot()` 返回 None 表示当前无法读取（无设备/未接线）。实现必须是只读的：
/// 模式切换路径不写 CueMix（A22/P11）。
pub trait CueMixSource: Send {
    /// 读取一次设备当前 CueMix 路由快照（None = 不可测）。
    fn read_snapshot(&self) -> Option<CueMixRouting>;
}

/// 一次模式切换前后的 CueMix 核对结果（F-29 证据，不假 diff=0）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueMixCheck {
    /// 证据来源：board_read = 切换前后真实设备快照；not_run = 无读取器/无设备。
    pub source: &'static str,
    /// 是否有真实前后快照（true 才能引用 diff_count）。
    pub verified: bool,
    /// 切换前后差异条数（verified 时 Some；NOT_RUN 时 None）。
    pub diff_count: Option<usize>,
    /// 切换前快照摘要（verified 时 Some）。
    pub before: Option<String>,
    /// 切换后快照摘要（verified 时 Some）。
    pub after: Option<String>,
}

impl CueMixCheck {
    /// 本进程没有真实读取面：如实 NOT_RUN（禁止写 0）。
    pub const fn not_run() -> Self {
        CueMixCheck {
            source: "not_run",
            verified: false,
            diff_count: None,
            before: None,
            after: None,
        }
    }

    /// JSON 投影（状态/证据）。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("source", JsonValue::str(self.source)),
            ("verified", JsonValue::Bool(self.verified)),
            (
                "diff_count",
                match self.diff_count {
                    Some(count) => JsonValue::from(count),
                    None => JsonValue::Null,
                },
            ),
            (
                "before",
                match &self.before {
                    Some(value) => JsonValue::str(value.as_str()),
                    None => JsonValue::Null,
                },
            ),
            (
                "after",
                match &self.after {
                    Some(value) => JsonValue::str(value.as_str()),
                    None => JsonValue::Null,
                },
            ),
        ])
    }
}

/// CueMix 一条路由（input -> output，enabled）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CueMixRoute {
    /// 输入通道（只作板端快照标识）。
    pub input: u8,
    /// 输出通道（0..=27）。
    pub output: u8,
    /// 是否直通。
    pub enabled: bool,
}

/// CueMix 路由只读快照（模式切换绝不写）。注意：这是数据模型，不是设备读回；
/// 设备事实必须经 `CueMixSource` 读取（F-29）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueMixRouting {
    /// 路由表。
    pub routes: Vec<CueMixRoute>,
}

impl CueMixRouting {
    /// 进程内参考基线（Optical1/2→Main1；校准 in7/in8→out7/out8；in7→out9 未连）。
    /// 仅模型/断言用，**不是**切换前后设备读回（F-29：真实 diff 见 `CueMixSource`）。
    pub fn board_baseline() -> Self {
        CueMixRouting {
            routes: vec![
                CueMixRoute {
                    input: 0,
                    output: 0,
                    enabled: true,
                },
                CueMixRoute {
                    input: 1,
                    output: 0,
                    enabled: true,
                },
                CueMixRoute {
                    input: 6,
                    output: 6,
                    enabled: true,
                },
                CueMixRoute {
                    input: 7,
                    output: 7,
                    enabled: true,
                },
                CueMixRoute {
                    input: 6,
                    output: 8,
                    enabled: false,
                },
            ],
        }
    }

    /// 规范化摘要（CueMixCheck before/after 证据用；顺序按 routes 顺序）。
    pub fn canonical(&self) -> String {
        self.routes
            .iter()
            .map(|route| {
                format!(
                    "{}>{}:{}",
                    route.input,
                    route.output,
                    if route.enabled { 1 } else { 0 }
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// 与另一快照的差异条数（A22：真实读取时才可作为「切换前后必须 0」的证据）。
    pub fn diff_count(&self, other: &CueMixRouting) -> usize {
        let mut count = 0;
        for route in &self.routes {
            let matched = other.routes.iter().find(|candidate| {
                candidate.input == route.input && candidate.output == route.output
            });
            match matched {
                Some(candidate) if candidate.enabled == route.enabled => {}
                _ => count += 1,
            }
        }
        for route in &other.routes {
            if !self
                .routes
                .iter()
                .any(|candidate| candidate.input == route.input && candidate.output == route.output)
            {
                count += 1;
            }
        }
        count
    }
}

// ---------------------------------------------------------------------------
// 编排入口（ModeController）
// ---------------------------------------------------------------------------

/// 一次成功编排的中间结果。
struct EnteredMode {
    ptp_lock: LockState,
    epoch_id: u64,
    servo_owner: Option<ServoOwner>,
    servo_claims: u64,
}

enum SwitchAttempt {
    Done {
        state: ModeState,
        report: SwitchReport,
    },
    Failed {
        cause: ModeError,
        report: SwitchReport,
    },
}

/// 模式状态机（A01–A22/K39–K44 的唯一 owner；T26 在 RuntimeEngine::switch_mode 接线）。
pub struct ModeController {
    target: TargetSpec,
    services: Box<dyn ModeServices + Send>,
    policy: FallbackPolicy,
    clock: ModeClock,
    current: ModeState,
    last_good: Option<ModeState>,
    switch_busy: bool,
    revision: u64,
    switch_seq: u64,
    last_report: Option<SwitchReport>,
    cue_mix: CueMixRouting,
    /// 真实 CueMix 读取面（F-29；None = 无设备/未接线 -> NOT_RUN）。
    cue_mix_source: Option<Box<dyn CueMixSource + Send>>,
    /// 最近一次切换前后的 CueMix 核对结果（F-29；不假 diff=0）。
    last_cue_mix: CueMixCheck,
    next_mono_ns: u64,
    next_unix: u64,
    stale_rejections: u64,
    busy_rejections: u64,
}

impl ModeController {
    /// 新建（默认 off/idle、48k/128、无 servo/epoch；target 必须通过 T42 TargetSpec::validate）。
    pub fn new(
        target: TargetSpec,
        services: Box<dyn ModeServices + Send>,
    ) -> Result<Self, ModeError> {
        target.validate().map_err(|error| {
            ModeError::new(
                ModeErrorKind::InvalidArgument,
                format!("invalid target: {} ({})", error.code, error.message),
            )
        })?;
        let target_hash = target.fingerprint();
        let off_profile = ModeProfile::for_mode(ControlMode::Off, &target)?;
        let config = runtime_config_for(&off_profile, &target);
        let current = ModeState {
            schema: MODE_STATE_SCHEMA,
            revision: 0,
            switch_seq: 0,
            phase: ModeStatePhase::Idle,
            mode: ControlMode::Off,
            profile: None,
            target: target.clone(),
            device_owner: DeviceOwner::Idle,
            epoch_id: 0,
            servo_owner: None,
            servo_claims: 0,
            ptp_source: None,
            ptp_lock: LockState::Unlocked,
            sample_rate: MODE_PLAYBACK_RATE_HZ,
            chunksize: MODE_DEFAULT_CHUNKSIZE,
            channels: 0,
            format: "-",
            layout: String::new(),
            target_hash,
            config_hash: config.fingerprint(),
            services: Vec::new(),
            last_error: None,
            last_exit_code: 0,
            fallback: FallbackAction::None,
            warnings: Vec::new(),
            switched_unix: 0,
        };
        Ok(ModeController {
            target,
            services,
            policy: FallbackPolicy::Off,
            clock: ModeClock::new(),
            current,
            last_good: None,
            switch_busy: false,
            revision: 0,
            switch_seq: 0,
            last_report: None,
            cue_mix: CueMixRouting::board_baseline(),
            cue_mix_source: None,
            last_cue_mix: CueMixCheck::not_run(),
            next_mono_ns: 2_000_000,
            next_unix: 1_764_000_001,
            stale_rejections: 0,
            busy_rejections: 0,
        })
    }

    /// 用默认 P1 4.1.2 target 新建（便捷构造）。
    pub fn with_default_target(services: Box<dyn ModeServices + Send>) -> Result<Self, ModeError> {
        ModeController::new(TargetSpec::p1_412(), services)
    }

    /// 设置回退策略。
    pub fn with_fallback_policy(mut self, policy: FallbackPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 设置 CueMix 进程内模型（只读；默认参考基线；不是设备读回）。
    pub fn with_cue_mix(mut self, routing: CueMixRouting) -> Self {
        self.cue_mix = routing;
        self
    }

    /// 注入真实 CueMix 读取面（F-29；None = 无设备/未接线 -> NOT_RUN）。
    ///
    /// 只读：模式切换前后各读一次，仅用于核对 diff；任何切换路径都不写设备。
    pub fn with_cue_mix_source(mut self, source: Box<dyn CueMixSource + Send>) -> Self {
        self.cue_mix_source = Some(source);
        self
    }

    /// 最近一次切换的 CueMix 核对结果（F-29 证据；NOT_RUN 时 verified=false）。
    pub fn last_cue_mix(&self) -> &CueMixCheck {
        &self.last_cue_mix
    }

    /// 读一次 CueMix（无读取器返回 None；不伪造）。
    fn read_cue_mix(&self) -> Option<CueMixRouting> {
        self.cue_mix_source
            .as_ref()
            .and_then(|source| source.read_snapshot())
    }

    fn build_cue_mix_check(
        &self,
        before: Option<CueMixRouting>,
        after: Option<CueMixRouting>,
    ) -> CueMixCheck {
        match (before, after) {
            (Some(before), Some(after)) => CueMixCheck {
                source: "board_read",
                verified: true,
                diff_count: Some(before.diff_count(&after)),
                before: Some(before.canonical()),
                after: Some(after.canonical()),
            },
            _ => CueMixCheck::not_run(),
        }
    }

    /// 当前状态。
    pub fn current(&self) -> &ModeState {
        &self.current
    }

    /// 新 host last-good（最近一次成功播放模式；非旧 C）。
    pub fn last_good(&self) -> Option<&ModeState> {
        self.last_good.as_ref()
    }

    /// 最近一次切换报告。
    pub fn last_report(&self) -> Option<&SwitchReport> {
        self.last_report.as_ref()
    }

    /// CueMix 进程内模型（只读；不是设备读回；F-29 真实 diff 见 `last_cue_mix()`）。
    pub fn cue_mix(&self) -> &CueMixRouting {
        &self.cue_mix
    }

    /// T42 `RuntimeEngine::mode_evidence` 的 actual 证据（F-28：epoch/servo/回滚）。
    pub fn evidence(&self) -> ModeStateEvidence {
        ModeStateEvidence {
            epoch: self.current.epoch_id,
            switch_seq: self.current.switch_seq,
            servo_owner: self
                .current
                .servo_owner
                .map(|owner| owner.as_str().to_string()),
            servo_claims: self.current.servo_claims,
            rolled_back: self
                .current
                .last_error
                .as_ref()
                .map(|error| error.rolled_back)
                .unwrap_or(false),
            last_exit_code: self.current.last_exit_code,
            fallback: self.current.fallback.as_str(),
        }
    }

    /// target。
    pub fn target(&self) -> &TargetSpec {
        &self.target
    }

    /// 回退策略。
    pub const fn policy(&self) -> FallbackPolicy {
        self.policy
    }

    /// 是否切换在途（同步实现下仅在 switch 内部短暂为 true；状态机字段）。
    pub const fn is_switching(&self) -> bool {
        self.switch_busy
    }

    /// 服务面（只读；测试/诊断）。
    pub fn services(&self) -> &(dyn ModeServices + Send) {
        &*self.services
    }

    /// 服务面（可变；测试注入锁/失败）。
    pub fn services_mut(&mut self) -> &mut (dyn ModeServices + Send) {
        &mut *self.services
    }

    /// stale/忙拒绝计数（证据）。
    pub const fn counters(&self) -> (u64, u64) {
        (self.stale_rejections, self.busy_rejections)
    }

    /// A03 退出码（最近一次切换结果；成功 0 / 失败回退 9）。
    pub const fn exit_code(&self) -> u8 {
        self.current.last_exit_code
    }

    /// 构造某模式的 profile（不改变状态）。
    pub fn profile_for(&self, mode: ControlMode) -> Result<ModeProfile, ModeError> {
        ModeProfile::for_mode(mode, &self.target)
    }

    /// 当前状态 JSON。
    pub fn status_json(&self) -> JsonValue {
        self.current.to_json()
    }

    /// 当前状态 legacy tspi-audio/v1（A04）。
    pub fn legacy_status_json(&self) -> JsonValue {
        self.current.legacy_status_json()
    }

    /// T26 EngineSnapshot 需要的 actual 字段（不复制第二套状态）。
    pub fn engine_fields(&self) -> ModeEngineFields {
        ModeEngineFields {
            mode: self.current.mode,
            mode_profile: self.current.profile_name().map(str::to_string),
            target_hash: self.current.target_hash.clone(),
            sample_rate: self.current.sample_rate,
            chunksize: self.current.chunksize,
            epoch: self.current.epoch_id,
            device_owner: self.current.device_owner.as_str().to_string(),
            calibration_owner: self.current.device_owner == DeviceOwner::Calibration,
            config_hash: self.current.config_hash.clone(),
            last_exit_code: self.current.last_exit_code,
        }
    }

    /// 只算候选状态（dry-run；零副作用、服务调用 0）。
    pub fn preview(&self, mode: ControlMode) -> Result<ModeState, ModeError> {
        self.profile_for(mode)?;
        let request = ModeRequest::new(mode);
        Ok(self.candidate_state(&self.profile_for(mode)?, &request, mode))
    }

    /// T42 RuntimeEngine::switch_mode 口径（返回 ModeAck 或冻结 RtStatus；T26 接线用）。
    pub fn switch_mode(&mut self, mode: ControlMode) -> Result<ModeAck, RtStatus> {
        if !SELECTABLE_MODES.contains(&mode) {
            return Err(RtStatus::ERR_UNSUPPORTED);
        }
        let now_mono_ns = self.next_mono_ns;
        self.next_mono_ns = self.next_mono_ns.saturating_add(1_000_000);
        let now_unix = self.next_unix;
        self.next_unix = self.next_unix.saturating_add(1);
        let request = ModeRequest::at(mode, now_mono_ns, now_unix);
        match self.switch(&request) {
            Ok(state) => Ok(state.to_mode_ack()),
            Err(error) => Err(error.rt_status()),
        }
    }

    /// 模式切换（两阶段：stop old -> enter new -> self-check -> 原子发布；失败按策略回退）。
    pub fn switch(&mut self, request: &ModeRequest) -> Result<ModeState, ModeError> {
        if self.switch_busy {
            self.busy_rejections += 1;
            return Err(ModeError::new(
                ModeErrorKind::SwitchBusy,
                "another mode switch is in progress",
            ));
        }
        if self.current.device_owner == DeviceOwner::Calibration {
            self.busy_rejections += 1;
            return Err(ModeError::new(
                ModeErrorKind::DeviceBusy,
                "device is owned by calibration job; finish/cancel it before switching mode",
            ));
        }
        let profile = self.profile_for(request.mode)?;
        if let Some(expected) = request.expected_config_hash.as_deref() {
            if expected != self.current.config_hash {
                self.stale_rejections += 1;
                return Err(ModeError::new(
                    ModeErrorKind::Stale,
                    format!(
                        "stale config hash: expected {expected}, actual {}",
                        self.current.config_hash
                    ),
                ));
            }
        }
        if request.dry_run {
            return Ok(self.candidate_state(&profile, request, request.mode));
        }
        if !request.force
            && self.current.phase == ModeStatePhase::Idle
            && self.current.mode == request.mode
            && request.mode != ControlMode::Off
        {
            return Ok(self.current.clone());
        }
        // F-29：切换前后各读一次真实 CueMix（有读取器时）；无读取器/无设备 -> NOT_RUN。
        let cue_before = self.read_cue_mix();
        self.switch_busy = true;
        let attempt = self.perform_switch(&profile, request);
        self.switch_busy = false;
        let cue_after = self.read_cue_mix();
        self.last_cue_mix = self.build_cue_mix_check(cue_before, cue_after);
        match attempt {
            SwitchAttempt::Done { state, report } => {
                self.revision = self.revision.saturating_add(1);
                self.switch_seq = self.switch_seq.saturating_add(1);
                if profile.mode.is_playback() {
                    self.last_good = Some(state.clone());
                }
                self.current = state.clone();
                self.last_report = Some(report);
                Ok(state)
            }
            SwitchAttempt::Failed { cause, report } => {
                self.revision = self.revision.saturating_add(1);
                self.switch_seq = self.switch_seq.saturating_add(1);
                let fallback = self.finish_failure(&profile, &cause, request, report);
                self.current = fallback;
                Err(cause.with_rolled_back())
            }
        }
    }

    fn candidate_state(
        &self,
        profile: &ModeProfile,
        request: &ModeRequest,
        mode: ControlMode,
    ) -> ModeState {
        let config = runtime_config_for(profile, &self.target);
        let units = profile.units();
        let published_profile = if mode == ControlMode::Off {
            None
        } else {
            Some(profile.clone())
        };
        ModeState {
            schema: MODE_STATE_SCHEMA,
            revision: self.revision.saturating_add(1),
            switch_seq: self.switch_seq.saturating_add(1),
            phase: ModeStatePhase::Idle,
            mode,
            profile: published_profile,
            target: self.target.clone(),
            device_owner: if mode.is_playback() {
                DeviceOwner::Playback
            } else {
                DeviceOwner::Idle
            },
            epoch_id: self.current.epoch_id,
            servo_owner: self.current.servo_owner,
            servo_claims: self.current.servo_claims,
            ptp_source: self.current.ptp_source,
            ptp_lock: self.services.ptp_lock(),
            sample_rate: config.sample_rate,
            chunksize: config.chunksize,
            channels: if mode == ControlMode::Off {
                0
            } else {
                profile.channels
            },
            format: if mode == ControlMode::Off {
                "-"
            } else {
                profile.format
            },
            layout: profile.layout.clone().unwrap_or_default(),
            target_hash: self.target.fingerprint(),
            config_hash: config.fingerprint(),
            services: self.services.snapshot(&units),
            last_error: None,
            last_exit_code: 0,
            fallback: FallbackAction::None,
            warnings: Vec::new(),
            switched_unix: request.now_unix,
        }
    }

    fn perform_switch(&mut self, profile: &ModeProfile, request: &ModeRequest) -> SwitchAttempt {
        let mut report = SwitchReport::new(
            self.switch_seq.saturating_add(1),
            profile.mode,
            request.now_unix,
        );
        report.push_step(
            "preflight",
            true,
            format!("mode={} profile={}", profile.mode.as_str(), profile.name),
        );
        // 1) 停旧（逆序：data -> PTP），保证同一时刻单一设备 owner。
        if let Some(old) = self.current.profile.clone() {
            for unit in old.units().iter().rev() {
                self.stop_unit(&mut report, *unit);
            }
        }
        for unit in profile.units().iter().rev() {
            if self.services.state(*unit).is_busy() {
                self.stop_unit(&mut report, *unit);
            }
        }
        // 2) 释放唯一 servo（新 epoch/servo 由 enter_mode 建立）。
        self.clock.release();
        report.push_step("release_servo", true, "old servo released (if any)");
        if report.steps.iter().any(|step| !step.ok) {
            let cause = ModeError::new(
                ModeErrorKind::DeviceUnavailable,
                "failed to stop previous mode services",
            );
            return SwitchAttempt::Failed { cause, report };
        }
        // 3) 进入新模式（PTP -> wait lock -> data -> 自检 -> 新 epoch/servo）。
        match self.enter_mode(profile, request, &mut report) {
            Ok(entered) => {
                let config = runtime_config_for(profile, &self.target);
                let units = profile.units();
                let services = self.services.snapshot(&units);
                let state = ModeState {
                    schema: MODE_STATE_SCHEMA,
                    revision: self.revision.saturating_add(1),
                    switch_seq: report.seq,
                    phase: ModeStatePhase::Idle,
                    mode: profile.mode,
                    profile: if profile.mode == ControlMode::Off {
                        None
                    } else {
                        Some(profile.clone())
                    },
                    target: self.target.clone(),
                    device_owner: if profile.mode.is_playback() {
                        DeviceOwner::Playback
                    } else {
                        DeviceOwner::Idle
                    },
                    epoch_id: entered.epoch_id,
                    servo_owner: entered.servo_owner,
                    servo_claims: entered.servo_claims,
                    ptp_source: profile.ptp_source,
                    ptp_lock: entered.ptp_lock,
                    sample_rate: config.sample_rate,
                    chunksize: config.chunksize,
                    channels: if profile.mode == ControlMode::Off {
                        0
                    } else {
                        profile.channels
                    },
                    format: if profile.mode == ControlMode::Off {
                        "-"
                    } else {
                        profile.format
                    },
                    layout: profile.layout.clone().unwrap_or_default(),
                    target_hash: self.target.fingerprint(),
                    config_hash: config.fingerprint(),
                    services,
                    last_error: None,
                    last_exit_code: 0,
                    fallback: FallbackAction::None,
                    warnings: report.warnings.clone(),
                    switched_unix: request.now_unix,
                };
                report.ok = true;
                report.epoch_id = state.epoch_id;
                report.config_hash = state.config_hash.clone();
                report.push_step("publish", true, state.config_hash.clone());
                SwitchAttempt::Done { state, report }
            }
            Err(cause) => SwitchAttempt::Failed { cause, report },
        }
    }

    fn stop_unit(&mut self, report: &mut SwitchReport, unit: ServiceUnit) {
        if self.services.state(unit) == ServiceRunState::Inactive {
            return;
        }
        match self.services.stop(unit) {
            Ok(()) => {
                report.stopped_units.push(unit);
                report.push_step("stop", true, unit.as_str().to_string());
            }
            Err(error) => {
                report.push_step(
                    "stop",
                    false,
                    format!("{}: {}", unit.as_str(), error.name()),
                );
            }
        }
    }

    fn start_unit(
        &mut self,
        report: &mut SwitchReport,
        unit: ServiceUnit,
    ) -> Result<(), ModeError> {
        match self.services.start(unit) {
            Ok(()) => {
                report.started_units.push(unit);
                report.push_step("start", true, unit.as_str().to_string());
                Ok(())
            }
            Err(error) => {
                report.push_step(
                    "start",
                    false,
                    format!("{}: {}", unit.as_str(), error.name()),
                );
                Err(service_error_to_mode_error(error))
            }
        }
    }

    fn enter_mode(
        &mut self,
        profile: &ModeProfile,
        request: &ModeRequest,
        report: &mut SwitchReport,
    ) -> Result<EnteredMode, ModeError> {
        for unit in profile.ptp_units {
            self.start_unit(report, *unit)?;
        }
        let mut ptp_lock = self.services.ptp_lock();
        if profile.requires_ptp_lock {
            if request.allow_unlocked_ptp {
                report.push_step(
                    "wait_ptp_lock",
                    true,
                    format!(
                        "skipped (allow_unlocked_ptp=true, lock={})",
                        ptp_lock.as_str()
                    ),
                );
                report
                    .warnings
                    .push("allow_unlocked_ptp: PTP not locked, switching anyway".to_string());
            } else {
                if ptp_lock != LockState::Locked {
                    report.push_step(
                        "wait_ptp_lock",
                        false,
                        format!("lock={}", ptp_lock.as_str()),
                    );
                    return Err(ModeError::new(
                        ModeErrorKind::PtpNotLocked,
                        format!(
                            "PTP not locked for mode {} (lock={})",
                            profile.mode.as_str(),
                            ptp_lock.as_str()
                        ),
                    ));
                }
                report.push_step("wait_ptp_lock", true, "locked".to_string());
            }
        }
        for unit in profile.data_units {
            self.start_unit(report, *unit)?;
        }
        if profile.requires_ptp_lock {
            ptp_lock = self.services.ptp_lock();
        }
        self.self_check(profile)?;
        report.push_step("self_check", true, profile.mode.as_str().to_string());
        let epoch_id = self.clock.enter(profile, request.now_mono_ns)?;
        report.push_step("new_epoch", true, format!("epoch={epoch_id}"));
        let servo_owner = self.clock.owner();
        if let Some(owner) = servo_owner {
            report.push_step("claim_servo", true, owner.as_str().to_string());
        }
        report.epoch_id = epoch_id;
        Ok(EnteredMode {
            ptp_lock,
            epoch_id,
            servo_owner,
            servo_claims: self.clock.claims(),
        })
    }

    fn self_check(&self, profile: &ModeProfile) -> Result<(), ModeError> {
        match profile.mode {
            ControlMode::Off => {
                for unit in ServiceUnit::ALL {
                    if self.services.state(unit).is_busy() {
                        return Err(ModeError::new(
                            ModeErrorKind::SelfCheckFailed,
                            format!(
                                "off mode but {} is {}",
                                unit.as_str(),
                                self.services.state(unit).as_str()
                            ),
                        ));
                    }
                }
                Ok(())
            }
            ControlMode::Toslink => {
                if profile.channels != TOSLINK_CHANNELS || !profile.stereo_strict {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        "toslink must be stereo strict (FL/FR only, no upmix)",
                    ));
                }
                if profile.sample_rate != MODE_PLAYBACK_RATE_HZ
                    || !BLOCK_SIZES.contains(&profile.chunksize)
                {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        "toslink profile rate/chunksize mismatch",
                    ));
                }
                Ok(())
            }
            ControlMode::TargetMultichannel => {
                self.target.validate().map_err(|error| {
                    ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        format!("target invalid: {} ({})", error.code, error.message),
                    )
                })?;
                if profile.channels != self.target.channels {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        format!(
                            "target_multichannel channels {} != target.channels {}",
                            profile.channels, self.target.channels
                        ),
                    ));
                }
                if profile.layout.as_deref() != Some(self.target.layout.as_str()) {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        "target_multichannel layout does not match resolved target",
                    ));
                }
                Ok(())
            }
            ControlMode::Aes67 => {
                if profile.ptp_source != Some(AoipProtocol::Aes67.expected_ptp_source()) {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        "aes67 PTP source mismatch (A15: unique source per mode)",
                    ));
                }
                let plan = aes67_capture_plan().map_err(|error| {
                    ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        format!("static aes67 profile invalid: {}", error.name()),
                    )
                })?;
                let service = self.services.aes67_service();
                validate_aes67_service(&service, &plan).map_err(|error| {
                    let kind = match error {
                        AoipError::StreamLost => ModeErrorKind::AoipNotReady,
                        _ => ModeErrorKind::SelfCheckFailed,
                    };
                    ModeError::new(kind, format!("aes67 service not ready: {}", error.name()))
                })
            }
            ControlMode::Dante => {
                if profile.ptp_source != Some(AoipProtocol::Inferno.expected_ptp_source()) {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        "dante PTP source mismatch (A15: unique source per mode)",
                    ));
                }
                if !inferno_device_id_valid(INFERNO_DEVICE_ID) {
                    return Err(ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        "inferno device id invalid",
                    ));
                }
                let env = InfernoEnv::new(
                    INFERNO_DEVICE_ID,
                    INFERNO_CLOCK_PATH_DEFAULT,
                    INFERNO_BIND_IFACE,
                    DANTE_RATE_HZ,
                    DANTE_CHANNELS,
                );
                env.validate(&H16Container::container16()).map_err(|error| {
                    ModeError::new(
                        ModeErrorKind::SelfCheckFailed,
                        format!("inferno endpoint not ready: {}", error.name()),
                    )
                })
            }
        }
    }

    fn finish_failure(
        &mut self,
        profile: &ModeProfile,
        cause: &ModeError,
        request: &ModeRequest,
        mut report: SwitchReport,
    ) -> ModeState {
        // 1) 清理：任何仍 active 的单元逆序停掉（不半启）。
        for unit in ServiceUnit::ALL.iter().rev() {
            if self.services.state(*unit).is_busy() {
                match self.services.stop(*unit) {
                    Ok(()) => {
                        report.stopped_units.push(*unit);
                        report.push_step("cleanup", true, unit.as_str().to_string());
                    }
                    Err(error) => {
                        report.push_step(
                            "cleanup",
                            false,
                            format!("{}: {}", unit.as_str(), error.name()),
                        );
                    }
                }
            }
        }
        self.clock.release();
        report.push_step("release_servo", true, "cleanup release");
        let _ = profile;
        // 2) 失败回退：默认 off（A16）；LastGood 策略尝试自研 last-good。
        let mut state = if self.policy == FallbackPolicy::LastGood {
            match self.last_good.clone() {
                Some(last) if last.mode.is_playback() => {
                    match self.restore_last_good(&last, request, &mut report) {
                        Ok(state) => state,
                        Err(error) => {
                            report.warnings.push(format!(
                                "last-good restore failed ({}); falling back to off",
                                error.detail
                            ));
                            self.off_error_state(cause, request, &mut report)
                        }
                    }
                }
                _ => self.off_error_state(cause, request, &mut report),
            }
        } else {
            self.off_error_state(cause, request, &mut report)
        };
        state.last_error = Some(cause.clone().with_rolled_back());
        state.last_exit_code = ModeExitCode::RolledBack.as_u8();
        state.warnings = report.warnings.clone();
        state.revision = self.revision;
        state.switch_seq = report.seq;
        state.switched_unix = request.now_unix;
        report.ok = false;
        report.epoch_id = state.epoch_id;
        report.config_hash = state.config_hash.clone();
        report.finished_unix = request.now_unix;
        self.last_report = Some(report);
        state
    }

    fn restore_last_good(
        &mut self,
        last: &ModeState,
        request: &ModeRequest,
        report: &mut SwitchReport,
    ) -> Result<ModeState, ModeError> {
        let profile = match &last.profile {
            Some(profile) => profile.clone(),
            None => {
                return Err(ModeError::new(
                    ModeErrorKind::SelfCheckFailed,
                    "last-good has no profile",
                ))
            }
        };
        self.enter_mode(&profile, request, report)?;
        let config = runtime_config_for(&profile, &self.target);
        let units = profile.units();
        Ok(ModeState {
            schema: MODE_STATE_SCHEMA,
            revision: self.revision,
            switch_seq: report.seq,
            phase: ModeStatePhase::Idle,
            mode: profile.mode,
            profile: Some(profile.clone()),
            target: self.target.clone(),
            device_owner: DeviceOwner::Playback,
            epoch_id: self.clock.epoch_id().unwrap_or(0),
            servo_owner: self.clock.owner(),
            servo_claims: self.clock.claims(),
            ptp_source: profile.ptp_source,
            ptp_lock: self.services.ptp_lock(),
            sample_rate: config.sample_rate,
            chunksize: config.chunksize,
            channels: profile.channels,
            format: profile.format,
            layout: profile.layout.clone().unwrap_or_default(),
            target_hash: self.target.fingerprint(),
            config_hash: config.fingerprint(),
            services: self.services.snapshot(&units),
            last_error: None,
            last_exit_code: ModeExitCode::RolledBack.as_u8(),
            fallback: FallbackAction::LastGood,
            warnings: Vec::new(),
            switched_unix: request.now_unix,
        })
    }

    fn off_error_state(
        &mut self,
        cause: &ModeError,
        request: &ModeRequest,
        report: &mut SwitchReport,
    ) -> ModeState {
        let off_profile =
            ModeProfile::for_mode(ControlMode::Off, &self.target).unwrap_or_else(|_| {
                ModeProfile::for_mode(ControlMode::Off, &TargetSpec::p1_412())
                    .expect("off profile is static")
            });
        let epoch_id = match self.clock.enter(&off_profile, request.now_mono_ns) {
            Ok(epoch_id) => epoch_id,
            Err(_) => self.clock.epoch_id().unwrap_or(0),
        };
        let config = runtime_config_for(&off_profile, &self.target);
        report.push_step("fallback", true, "off".to_string());
        ModeState {
            schema: MODE_STATE_SCHEMA,
            revision: self.revision,
            switch_seq: report.seq,
            phase: ModeStatePhase::Error,
            mode: ControlMode::Off,
            profile: None,
            target: self.target.clone(),
            device_owner: DeviceOwner::Idle,
            epoch_id,
            servo_owner: None,
            servo_claims: self.clock.claims(),
            ptp_source: None,
            ptp_lock: self.services.ptp_lock(),
            sample_rate: config.sample_rate,
            chunksize: config.chunksize,
            channels: 0,
            format: "-",
            layout: String::new(),
            target_hash: self.target.fingerprint(),
            config_hash: config.fingerprint(),
            services: self.services.snapshot(&ServiceUnit::ALL),
            last_error: Some(cause.clone().with_rolled_back()),
            last_exit_code: ModeExitCode::RolledBack.as_u8(),
            fallback: FallbackAction::Off,
            warnings: Vec::new(),
            switched_unix: request.now_unix,
        }
    }

    /// 进入校准独占（先把播放切到 off 释放设备；core-v1 §5.4 硬规则）。
    pub fn begin_calibration(&mut self) -> Result<ModeState, ModeError> {
        if self.switch_busy {
            return Err(ModeError::new(
                ModeErrorKind::SwitchBusy,
                "another mode switch is in progress",
            ));
        }
        if self.current.device_owner == DeviceOwner::Calibration {
            return Err(ModeError::new(
                ModeErrorKind::DeviceBusy,
                "calibration already owns the device",
            ));
        }
        if self.current.mode != ControlMode::Off {
            let now_mono_ns = self.next_mono_ns;
            self.next_mono_ns = self.next_mono_ns.saturating_add(1_000_000);
            let now_unix = self.next_unix;
            self.next_unix = self.next_unix.saturating_add(1);
            let request = ModeRequest::at(ControlMode::Off, now_mono_ns, now_unix);
            self.switch(&request)?;
        }
        self.current.device_owner = DeviceOwner::Calibration;
        Ok(self.current.clone())
    }

    /// 退出校准独占（owner 回 idle，之后才允许切换模式）。
    pub fn end_calibration(&mut self) -> Result<ModeState, ModeError> {
        if self.current.device_owner != DeviceOwner::Calibration {
            return Err(ModeError::new(
                ModeErrorKind::InvalidArgument,
                "no calibration owner to release",
            ));
        }
        self.current.device_owner = DeviceOwner::Idle;
        Ok(self.current.clone())
    }
}

/// T26 EngineSnapshot 需要的 actual 字段（不复制第二套状态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeEngineFields {
    /// actual 模式。
    pub mode: ControlMode,
    /// actual profile。
    pub mode_profile: Option<String>,
    /// target hash。
    pub target_hash: String,
    /// actual 采样率。
    pub sample_rate: u32,
    /// actual chunksize。
    pub chunksize: u16,
    /// epoch。
    pub epoch: u64,
    /// 设备 owner。
    pub device_owner: String,
    /// 是否校准独占。
    pub calibration_owner: bool,
    /// config hash。
    pub config_hash: String,
    /// A03 退出码。
    pub last_exit_code: u8,
}

fn runtime_config_for(profile: &ModeProfile, target: &TargetSpec) -> RuntimeConfig {
    RuntimeConfig {
        mode: profile.mode,
        sample_rate: profile.sample_rate,
        chunksize: profile.chunksize,
        target: target.clone(),
        coeff: None,
    }
}

fn service_error_to_mode_error(error: ServiceError) -> ModeError {
    match error {
        ServiceError::NotPresent(unit) => ModeError::new(
            ModeErrorKind::PrerequisiteMissing,
            format!("service {} not present", unit.as_str()),
        ),
        ServiceError::StartFailed(unit) => {
            if unit.is_driver() {
                ModeError::new(
                    ModeErrorKind::DeviceUnavailable,
                    format!("driver unit {} failed to start", unit.as_str()),
                )
            } else {
                ModeError::new(
                    ModeErrorKind::SelfCheckFailed,
                    format!("unit {} failed to start (self-check/lib)", unit.as_str()),
                )
            }
        }
        ServiceError::StopFailed(unit) => ModeError::new(
            ModeErrorKind::DeviceUnavailable,
            format!("unit {} failed to stop", unit.as_str()),
        ),
        ServiceError::PtpTimeout => ModeError::new(ModeErrorKind::PtpNotLocked, "PTP lock timeout"),
        ServiceError::AoipNotReady(detail) => ModeError::new(ModeErrorKind::AoipNotReady, detail),
    }
}

/// AES67 B1 静态接收计划（H16 7.1.4 两条 flow 8+4；只做 T43 交叉校验，不发协议）。
fn aes67_capture_plan() -> Result<Aes67CapturePlan, AoipError> {
    let flows = [
        Aes67FlowSdp {
            session_name: Some("H16-4721:16"),
            connection_addr: "239.69.1.1",
            connection_ttl: None,
            port: AES67_RTP_PORT_DEFAULT,
            payload_type: 96,
            encoding: Aes67Encoding::L24,
            sample_rate_hz: AES67_RATE_HZ,
            channels: 8,
            ptime_us: Some(1_000),
            ts_refclk: Some(PtpRefClock {
                domain: AES67_PTP_DOMAIN,
                grandmaster_present: true,
            }),
            mediaclk_direct: true,
        },
        Aes67FlowSdp {
            session_name: Some("H16-4721:16"),
            connection_addr: "239.69.1.2",
            connection_ttl: None,
            port: AES67_RTP_PORT_DEFAULT + 1,
            payload_type: 97,
            encoding: Aes67Encoding::L24,
            sample_rate_hz: AES67_RATE_HZ,
            channels: 4,
            ptime_us: Some(1_000),
            ts_refclk: Some(PtpRefClock {
                domain: AES67_PTP_DOMAIN,
                grandmaster_present: true,
            }),
            mediaclk_direct: true,
        },
    ];
    let set = Aes67FlowSet::new(&flows)?;
    validate_aes67_capture(&set, &H16FlowConfig::logical12(), AES67_PTP_DOMAIN)
}

/// 供证据/测试复用的 AES67 静态 plan 校验（Aes67ServiceState ready/48k/domain0 应通过）。
pub fn aes67_plan_is_valid() -> bool {
    aes67_capture_plan().is_ok()
}

/// 供证据/测试复用的 Inferno 环境校验（A14 稳定 ID/clock path/iface/96k/16ch）。
pub fn dante_env_is_valid() -> bool {
    inferno_device_id_valid(INFERNO_DEVICE_ID)
        && InfernoEnv::new(
            INFERNO_DEVICE_ID,
            INFERNO_CLOCK_PATH_DEFAULT,
            INFERNO_BIND_IFACE,
            DANTE_RATE_HZ,
            DANTE_CHANNELS,
        )
        .validate(&H16Container::container16())
        .is_ok()
}

/// CueMix 基线快照（证据常量；A22）。
pub fn board_cue_mix_baseline() -> CueMixRouting {
    CueMixRouting::board_baseline()
}

/// 全部可选模式的 PTP 来源/唯一 servo 表（K44 判据；供 T26/T73 直接引用）。
pub fn mode_clock_ownership() -> Vec<(ControlMode, Option<PtpSource>, Option<ServoOwner>)> {
    SELECTABLE_MODES
        .iter()
        .filter_map(|mode| {
            ModeProfile::for_mode(*mode, &TargetSpec::p1_412())
                .ok()
                .map(|profile| (*mode, profile.ptp_source, profile.servo_owner))
        })
        .collect()
}

/// 全部服务单元名（A11 inventory 视图；G-no-C 断言用）。
pub fn service_unit_names() -> Vec<&'static str> {
    ServiceUnit::ALL.iter().map(|unit| unit.as_str()).collect()
}

// ---------------------------------------------------------------------------
// T42 RuntimeEngine 适配：ModeController 接线（F-28/F-13）
// ---------------------------------------------------------------------------

/// 把 T44 `ModeController` 接进 T42 `RuntimeEngine` 的适配器（daemon 生产路径）。
///
/// - `switch_mode` 走真实状态机（互斥/在途 `ERR_SWITCH_BUSY`/失败回滚/last-good/
///   新 epoch/唯一 servo），不再像 `MemoryEngine` 那样直接改 snapshot 回 `switched`；
/// - 其余 actual ack（prepare/commit/volume/mute）原样委托内层 engine；内层是内存替身时
///   `engine_info().synthetic` 保持 true（F-13：commit 仍不是真实 worker all-ack）；
/// - `mode_evidence()` 暴露 epoch/servo/回滚事实（F-28：成功切换必须有新 epoch + 唯一 servo）。
///
/// 生产接线（T26）应把内层换成 rt-host `PlanRegistry` engine、服务面换成真实
/// systemd/clock 实现；本适配器不改变任何 trait 语义。
///
/// N-01：模式切换后若又发生真实 commit，`snapshot()` 的 sample_rate/chunksize/target/epoch
/// 必须回到 inner 的实际提交（否则 GET format/status 会退回切换时的旧块长/旧 epoch）；
/// 状态机 owner 的字段（mode/profile/设备 owner）仍以 controller 为准。
pub struct ModeEngine {
    inner: Box<dyn RuntimeEngine + Send>,
    controller: ModeController,
    switched: bool,
    /// 最近一次 switch_mode 之前 inner 的 epoch（N-01：判定切换后是否发生过真实 commit）。
    inner_epoch_at_switch: u64,
}

impl ModeEngine {
    /// 用内层 engine 的 actual target 建状态机（target 必须通过 TargetSpec::validate）。
    pub fn new(
        inner: Box<dyn RuntimeEngine + Send>,
        services: Box<dyn ModeServices + Send>,
    ) -> Result<Self, ModeError> {
        let target = inner.snapshot().target;
        let controller = ModeController::new(target, services)?;
        Ok(ModeEngine {
            inner,
            controller,
            switched: false,
            inner_epoch_at_switch: 0,
        })
    }

    /// 内层 engine 的只读引用。
    pub fn inner(&self) -> &(dyn RuntimeEngine + Send) {
        &*self.inner
    }

    /// 状态机只读引用。
    pub fn controller(&self) -> &ModeController {
        &self.controller
    }

    /// 状态机可变引用（测试注入服务失败等）。
    pub fn controller_mut(&mut self) -> &mut ModeController {
        &mut self.controller
    }
}

impl RuntimeEngine for ModeEngine {
    fn engine_info(&self) -> EngineInfo {
        let mut info = self.inner.engine_info();
        info.mode_controller = true;
        // 当前 daemon 用 SimulatedServices（无 systemd/设备）；T26 换真实服务面时改这里。
        info.mode_services = "simulated";
        info
    }

    fn snapshot(&self) -> EngineSnapshot {
        let inner = self.inner.snapshot();
        // 只有发生过模式切换才覆盖：首帧保持内层 actual，避免把「尚未切换」显示成 off。
        if !self.switched {
            return inner;
        }
        // N-01：切换后又发生真实 commit 时，块长/采样率/target/epoch 必须以 inner 的实际提交
        // 为准；继续用 controller 里切换时的旧值覆盖会让 GET /api/audio/format 回到旧块长/
        // 旧 epoch（rest.rs 的 epoch 也是 inner commit ack 的值）。
        let committed_since_switch = inner.epoch != self.inner_epoch_at_switch;
        let fields = self.controller.engine_fields();
        let mut snapshot = inner;
        // 状态机 owner 的字段（mode/profile/设备 owner）始终以 controller 为准。
        snapshot.mode = fields.mode;
        snapshot.mode_profile = fields.mode_profile;
        snapshot.device_owner = fields.device_owner;
        snapshot.calibration_owner = fields.calibration_owner;
        if committed_since_switch {
            // epoch 单调（core-v1 INV-L7）：提交 epoch 与切换 epoch 取较大者，禁止倒退。
            snapshot.epoch = snapshot.epoch.max(fields.epoch);
        } else {
            snapshot.target_hash = fields.target_hash;
            snapshot.sample_rate = fields.sample_rate;
            snapshot.chunksize = fields.chunksize;
            snapshot.target = self.controller.target().clone();
            if fields.epoch != 0 {
                snapshot.epoch = fields.epoch;
            }
        }
        snapshot
    }

    fn coeff_known(&self, coeff: &CoeffRef) -> bool {
        self.inner.coeff_known(coeff)
    }

    fn prepare(&mut self, desired: &RuntimeConfig) -> Result<PrepareOutcome, RtStatus> {
        self.inner.prepare(desired)
    }

    fn commit(&mut self, engine_token: u64) -> Result<CommitOutcome, RtStatus> {
        self.inner.commit(engine_token)
    }

    fn abort(&mut self, engine_token: u64) -> RtStatus {
        self.inner.abort(engine_token)
    }

    fn set_volume(&mut self, volume_db: f64) -> Result<VolumeAck, RtStatus> {
        self.inner.set_volume(volume_db)
    }

    fn set_mute(&mut self, mute: bool) -> Result<MuteAck, RtStatus> {
        self.inner.set_mute(mute)
    }

    fn switch_mode(&mut self, mode: ControlMode) -> Result<ModeAck, RtStatus> {
        // N-01：记录切换前 inner 的 epoch；之后每次真实 commit 都会推进它，
        // snapshot 据此判断切换后是否已有实际提交。
        self.inner_epoch_at_switch = self.inner.snapshot().epoch;
        let result = self.controller.switch_mode(mode);
        // 失败也要标记：状态机已把 current 回滚到 off/last-good，snapshot 必须反映回退结果。
        self.switched = true;
        result
    }

    fn mode_evidence(&self) -> Option<ModeStateEvidence> {
        if self.switched {
            Some(self.controller.evidence())
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// 私有测试（T44）：include! 进 lib 单测目标（leaf 不改 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod modes_tests {
    use super::*;

    include!("../tests/T44/modes_contract.rs");
}
