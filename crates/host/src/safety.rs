//! 故障隔离：last-good 保留、安全静音、欠载/溢出处理。
//!
//! 原则（T24）：
//! - 故障后**不能**让音频线程崩，也不能继续用坏系数：进 Faulted 安全静音，输出静音。
//! - 切图失败时保留 **last-good**（上一个成功生效的版本），失败版本绝不变成现役。
//! - 欠载/溢出按连续次数判定；偶发一次不算故障，连续超阈值才进安全静音。
//! - 恢复必须显式调用 `clear_fault`，不允许"下一次成功就自动恢复"。

/// 运行时状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    /// 还没开始跑。
    Idle,
    /// 正常运行。
    Running,
    /// 用户静音（不是故障）。
    Muted,
    /// 故障导致的安全静音：输出静音、现役图保留 last-good，等显式恢复。
    Faulted,
}

impl RuntimeState {
    /// 状态名（协议里用的字符串）。
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeState::Idle => "idle",
            RuntimeState::Running => "running",
            RuntimeState::Muted => "muted",
            RuntimeState::Faulted => "faulted",
        }
    }

    /// 该状态下是否必须输出静音。
    pub fn requires_mute(self) -> bool {
        matches!(self, RuntimeState::Muted | RuntimeState::Faulted)
    }
}

/// 故障原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultReason {
    /// 处理链报错。
    ProcessorFault,
    /// 设备欠载（缓冲该有数据却没有）。
    Underrun,
    /// 设备溢出（生产者追上消费者）。
    Overrun,
    /// 图切换失败。
    CommitFailed,
    /// 其它内部错误。
    Internal,
}

impl FaultReason {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            FaultReason::ProcessorFault => "processor_fault",
            FaultReason::Underrun => "underrun",
            FaultReason::Overrun => "overrun",
            FaultReason::CommitFailed => "commit_failed",
            FaultReason::Internal => "internal",
        }
    }
}

/// 安全策略阈值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyPolicy {
    /// 连续欠载达到该次数即进安全静音。
    pub max_consecutive_underruns: u32,
    /// 连续溢出达到该次数即进安全静音。
    pub max_consecutive_overruns: u32,
}

impl Default for SafetyPolicy {
    fn default() -> Self {
        Self {
            max_consecutive_underruns: 3,
            max_consecutive_overruns: 3,
        }
    }
}

/// 一条故障记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRecord {
    /// 原因分类。
    pub reason: FaultReason,
    /// 发生在第几块（0 表示非音频线程触发）。
    pub block: u64,
    /// 人看的说明。
    pub message: String,
}

/// 安全状态快照（给 status 用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyStatus {
    /// 当前状态。
    pub state: RuntimeState,
    /// 已处理块数。
    pub blocks: u64,
    /// 累计故障数。
    pub total_faults: u64,
    /// 连续欠载次数。
    pub consecutive_underruns: u32,
    /// 连续溢出次数。
    pub consecutive_overruns: u32,
    /// 现役版本（还没成功切过图为 None）。
    pub active_version: Option<u64>,
    /// last-good 版本：切图失败时保持不动。
    pub last_good_version: Option<u64>,
    /// 最近一次故障。
    pub last_fault: Option<FaultRecord>,
    /// 用户静音请求。
    pub user_mute: bool,
}

/// 恢复失败的 reason。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryError {
    /// 当前没有故障，无需恢复。
    NotFaulted,
}

impl core::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RecoveryError::NotFaulted => write!(f, "runtime is not faulted"),
        }
    }
}

impl std::error::Error for RecoveryError {}

/// 安全监督器。所有方法都在非实时线程调用，音频线程只读 `requires_mute` 的结果。
#[derive(Debug)]
pub struct SafetySupervisor {
    policy: SafetyPolicy,
    state: RuntimeState,
    blocks: u64,
    total_faults: u64,
    consecutive_underruns: u32,
    consecutive_overruns: u32,
    active_version: Option<u64>,
    last_good_version: Option<u64>,
    last_fault: Option<FaultRecord>,
    user_mute: bool,
}

impl SafetySupervisor {
    /// 新建（Idle）。
    pub fn new(policy: SafetyPolicy) -> Self {
        Self {
            policy,
            state: RuntimeState::Idle,
            blocks: 0,
            total_faults: 0,
            consecutive_underruns: 0,
            consecutive_overruns: 0,
            active_version: None,
            last_good_version: None,
            last_fault: None,
            user_mute: false,
        }
    }

    /// 当前状态。
    pub fn state(&self) -> RuntimeState {
        self.state
    }

    /// 是否必须静音输出（音频线程只读这个；含用户静音，供仍把用户静音放在安全监督器里的调用方使用）。
    pub fn requires_mute(&self) -> bool {
        self.state.requires_mute() || self.user_mute
    }

    /// 仅安全故障是否要求静音（不含用户静音）。
    ///
    /// Runtime 使用 OutputStage 单独处理用户静音/校准旁路；若这里仍把 user_mute 算进去，
    /// 校准扫频会被用户静音再次清零，无法做到“保持用户静音状态但临时放行”。
    pub fn fault_requires_mute(&self) -> bool {
        self.state == RuntimeState::Faulted
    }

    /// 用户静音请求。
    pub fn user_mute_requested(&self) -> bool {
        self.user_mute
    }

    /// 开始运行（Idle → Running）。
    pub fn start(&mut self) {
        if self.state == RuntimeState::Idle {
            self.state = RuntimeState::Running;
        }
    }

    /// 记录一块正常处理完成。
    pub fn note_block_ok(&mut self) {
        self.blocks += 1;
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        if self.state == RuntimeState::Idle {
            self.state = RuntimeState::Running;
        }
    }

    /// 记录一次欠载；返回是否因此进入安全静音。
    pub fn note_underrun(&mut self) -> bool {
        self.consecutive_underruns += 1;
        if self.consecutive_underruns >= self.policy.max_consecutive_underruns {
            self.enter_fault(
                FaultReason::Underrun,
                format!("{} consecutive underruns", self.consecutive_underruns),
            );
            return true;
        }
        false
    }

    /// 记录一次溢出；返回是否因此进入安全静音。
    pub fn note_overrun(&mut self) -> bool {
        self.consecutive_overruns += 1;
        if self.consecutive_overruns >= self.policy.max_consecutive_overruns {
            self.enter_fault(
                FaultReason::Overrun,
                format!("{} consecutive overruns", self.consecutive_overruns),
            );
            return true;
        }
        false
    }

    /// 记录处理链故障：立刻进安全静音。
    pub fn note_processor_fault(&mut self, message: impl Into<String>) {
        self.enter_fault(FaultReason::ProcessorFault, message);
    }

    /// 记录图切换失败：**不**改变现役版本，保留 last-good，并进安全静音等人工确认。
    pub fn note_commit_failed(&mut self, message: impl Into<String>) {
        self.enter_fault(FaultReason::CommitFailed, message);
    }

    /// 记录一次成功的图切换：现役版本与 last-good 同时前进。
    pub fn note_commit_applied(&mut self, version: u64) {
        self.active_version = Some(version);
        self.last_good_version = Some(version);
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        // 成功切换本身不代表故障已清除：Faulted 仍要显式 clear_fault。
        if self.state == RuntimeState::Idle {
            self.state = RuntimeState::Running;
        }
    }

    /// 用户静音开关（与故障静音独立）。
    pub fn set_user_mute(&mut self, mute: bool) {
        self.user_mute = mute;
        if mute {
            if self.state == RuntimeState::Running {
                self.state = RuntimeState::Muted;
            }
        } else if self.state == RuntimeState::Muted {
            self.state = RuntimeState::Running;
        }
    }

    /// 显式清除故障：只允许从 Faulted 恢复。
    pub fn clear_fault(&mut self) -> Result<(), RecoveryError> {
        if self.state != RuntimeState::Faulted {
            return Err(RecoveryError::NotFaulted);
        }
        self.state = if self.user_mute {
            RuntimeState::Muted
        } else {
            RuntimeState::Running
        };
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        Ok(())
    }

    /// 状态快照。
    pub fn status(&self) -> SafetyStatus {
        SafetyStatus {
            state: self.state,
            blocks: self.blocks,
            total_faults: self.total_faults,
            consecutive_underruns: self.consecutive_underruns,
            consecutive_overruns: self.consecutive_overruns,
            active_version: self.active_version,
            last_good_version: self.last_good_version,
            last_fault: self.last_fault.clone(),
            user_mute: self.user_mute,
        }
    }

    fn enter_fault(&mut self, reason: FaultReason, message: impl Into<String>) {
        self.total_faults += 1;
        self.state = RuntimeState::Faulted;
        self.last_fault = Some(FaultRecord {
            reason,
            block: self.blocks,
            message: message.into(),
        });
    }
}
