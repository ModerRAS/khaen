//! 块边界命令：控制线程 -> 块线程（Runtime 的唯一 owner），一次性 ack 回执。
//!
//! 控制线程只做重活（读方案文件、解码、准入、图编译、surface 补丁、计划历史），产出的
//! PlanDraft 是 Send 的；真正的执行器状态变更（静音、换图）只在块线程的块边界发生：
//!
//!     控制线程                        块线程（轮询命令）
//!     prepare/surface -> PlanDraft -> CommitPlan{draft, format, ack} -> stage_plan_draft
//!     surface=mode   -> Mute{...}     -> request_mute -> RunBlock 入口生效
//!     <- CommandReply（applied_block / idempotent / 拒绝原因）
//!
//! RT 路径（Runtime::run_block）不碰命令通道：命令在它前后的块循环里处理。

use std::sync::mpsc::Sender;

use khaen_host::alsa_backend::PcmOutput;
use khaen_host::control::error;
use khaen_host::engine::{EngineConfig, MuteReceipt};
use khaen_host::runtime::{PlanDraft, Runtime, RuntimeStatus};

/// 一条需要在块边界执行的命令（控制线程 -> 块线程）。
pub enum ControlCommand {
    /// 用户静音/解除静音（块边界生效，回执带 applied_block 与 idempotent）。
    Mute {
        /// 目标静音状态。
        mute: bool,
        /// 一次性回执通道。
        ack: Sender<CommandReply>,
    },
    /// 换图：控制线程已编译好的草稿，块线程排入下一块边界。
    CommitPlan {
        /// 已准入、已编译的草稿（Send）。
        draft: PlanDraft,
        /// 草稿编译时使用的执行器格式；块线程校验它必须等于数据面格式才 stage
        /// （format 只改控制面编译格式时，这里会如实拒绝而不是把不匹配的图塞进引擎）。
        format: EngineConfig,
        /// 一次性回执通道。
        ack: Sender<CommandReply>,
    },
}

/// 块线程对命令的回执。
#[derive(Debug)]
pub enum CommandReply {
    /// 静音命令回执（与 SlotEngine::request_mute 同一份语义）。
    Mute {
        /// 生效后的静音状态。
        mute: bool,
        /// 将生效的块号。
        applied_block: u64,
        /// 是否因为状态已相同而没有排队。
        idempotent: bool,
    },
    /// 换图命令已排入下一块边界。
    PlanApplied {
        /// 新图将生效的块号。
        applied_block: u64,
    },
    /// 命令被拒绝（没有执行任何块边界动作）。
    Rejected {
        /// 稳定错误码（契约 B.3）。
        code: &'static str,
        /// 人读说明。
        message: String,
    },
}

/// 块线程侧的执行器抽象：生产实现是 Runtime<P>，测试夹具可用 SlotEngine 包装。
///
/// 只有块线程能拿到实现（Runtime 的唯一 owner），因此控制线程无法直接调用这些方法。
pub trait ControlTarget {
    /// 数据面执行器格式（进程内固定；用于校验草稿格式与 status.format）。
    fn data_plane_format(&self) -> EngineConfig;

    /// 已完成的块数（快照发布周期/回执语义用）。
    fn blocks(&self) -> u64;

    /// 请求静音/解除静音（下一块边界生效）。
    fn request_mute(&mut self, mute: bool) -> MuteReceipt;

    /// 把草稿排入下一块边界，返回将生效的块号。
    fn stage_plan_draft(&mut self, draft: PlanDraft) -> u64;

    /// 完整运行时状态快照（非实时路径；控制通道用它发布 status）。
    fn status(&self) -> RuntimeStatus;

    /// 安全监督器累计故障数（status.faults）。
    fn total_faults(&self) -> u64;
}

impl<P: PcmOutput> ControlTarget for Runtime<P> {
    fn data_plane_format(&self) -> EngineConfig {
        self.config().engine.clone()
    }

    fn blocks(&self) -> u64 {
        Runtime::<P>::blocks(self)
    }

    fn request_mute(&mut self, mute: bool) -> MuteReceipt {
        Runtime::<P>::request_mute(self, mute)
    }

    fn stage_plan_draft(&mut self, draft: PlanDraft) -> u64 {
        Runtime::<P>::stage_plan_draft(self, draft)
    }

    fn status(&self) -> RuntimeStatus {
        Runtime::<P>::status(self)
    }

    fn total_faults(&self) -> u64 {
        self.safety().status().total_faults
    }
}

/// 在块边界执行一条命令（块线程调用；块循环不是 RT 路径，允许少量堆操作）。
///
/// 语义：
/// - Mute：request_mute 只登记，下一块入口由 SlotEngine 生效；回执带 applied_block。
/// - CommitPlan：先校验草稿格式与数据面格式一致（format op 只改控制面编译格式时，
///   这一步如实拒绝，不把不匹配的图塞进引擎），一致才 stage。
pub fn execute_command<T: ControlTarget>(command: ControlCommand, target: &mut T) {
    match command {
        ControlCommand::Mute { mute, ack } => {
            let receipt = target.request_mute(mute);
            let _ = ack.send(CommandReply::Mute {
                mute: receipt.mute,
                applied_block: receipt.applied_block,
                idempotent: receipt.idempotent,
            });
        }
        ControlCommand::CommitPlan { draft, format, ack } => {
            let runtime_format = target.data_plane_format();
            if format != runtime_format {
                let _ = ack.send(CommandReply::Rejected {
                    code: error::NOT_APPLIED,
                    message: format!(
                        "prepared plan targets {} Hz / {} frames / {} ch, but the running rt-host data plane is {} Hz / {} frames / {} ch; draft not staged. A format change needs an rt-host restart (device_restart_required=true; M3b hot reconfiguration is not implemented)",
                        format.sample_rate,
                        format.block_frames,
                        format.channels,
                        runtime_format.sample_rate,
                        runtime_format.block_frames,
                        runtime_format.channels
                    ),
                });
                return;
            }
            let applied_block = target.stage_plan_draft(draft);
            let _ = ack.send(CommandReply::PlanApplied { applied_block });
        }
    }
}
