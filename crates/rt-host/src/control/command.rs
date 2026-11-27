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
use std::sync::Arc;
use std::time::Instant;

use khaen_host::alsa_backend::PcmOutput;
use khaen_host::control::error;
use khaen_host::engine::{EngineConfig, MuteReceipt};
use khaen_host::runtime::{
    CalibrationPlayback, PlanDraft, ReconfigureFailure, ReconfigureOutcome, Runtime, RuntimeConfig,
    RuntimeStatus,
};
use serde_json::json;

use super::reconfigure::{
    failure_json, AppliedReconfigure, ChannelsView, DeviceProbe, SharedReconfigure,
};
use crate::recovery::DeviceLifecycle;

/// 一条需要在块边界执行的命令（控制线程 -> 块线程）。
pub enum ControlCommand {
    /// 用户静音/解除静音（块边界生效，回执带 applied_block 与 idempotent）。
    Mute {
        /// 目标静音状态。
        mute: bool,
        /// 一次性回执通道。
        ack: Sender<CommandReply>,
    },
    /// 校准模式（T70f-c）：开始一段预渲染扫频播放（控制线程已把 T61 时间轴渲染成交错 f32）。
    /// 块线程只做块边界登记；真正的逐块写出在 [ControlTarget::run_calibration_block] 路径。
    CalPlaybackBegin {
        /// 预渲染好的整段播放数据 + 进度共享句柄。
        playback: CalibrationPlayback,
        /// 一次性回执通道（登记成功时返回总块数）。
        ack: Sender<CommandReply>,
    },
    /// 校准模式：结束扫频播放（未播完也停止；块线程回到节目路径）。
    CalPlaybackEnd {
        /// 一次性回执通道。
        ack: Sender<CommandReply>,
    },
    /// 热重配（M3b）：控制线程已校验/探测的完整新配置；块线程在块边界执行
    /// （静音 → 关旧 PCM → 换配置/重开设备/重建收包 → hw 回读校验 → 恢复）。
    Reconfigure {
        /// 幂等 token（回执与追踪器对账用）。
        token: String,
        /// 目标完整运行时配置（装箱：比其它命令变体大得多）。
        config: Box<RuntimeConfig>,
        /// 控制线程按目标格式预编译的方案草稿（只有引擎格式变化时才有）。
        plan: Option<PlanDraft>,
        /// 只读 hw params 探测（块线程在新设备打开后回读校验用）。
        probe: Arc<dyn DeviceProbe>,
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

/// 一笔热重配请求（控制线程校验/探测后交给块线程执行）。
pub struct ReconfigureRequest {
    /// 目标完整运行时配置。
    pub config: RuntimeConfig,
    /// 控制线程按目标格式预编译的方案草稿（只有引擎格式变化时才有）。
    pub plan: Option<PlanDraft>,
    /// 只读 hw params 探测（新设备打开后的回读校验用）。
    pub probe: Arc<dyn DeviceProbe>,
}

/// 热重配成功回执（机械结果 + 目标视图）。
#[derive(Debug)]
pub struct ReconfigureDone {
    /// host seam 的机械结果（格式/是否重建/是否丢图/hw 回读/块计数）。
    pub outcome: ReconfigureOutcome,
    /// 生效后的协议视图（追踪器/控制面对账用）。
    pub target: ChannelsView,
}

/// 命令执行上下文（块线程）：设备生命周期 + 热重配追踪器 + 单调毫秒时钟。
pub struct CommandContext<'a> {
    /// 设备生命周期（absent 判定 / 重配失败后如实登记缺失）。
    pub lifecycle: &'a mut DeviceLifecycle,
    /// 热重配追踪器（块线程记录 generation/last_gap/last_result/在飞 token）。
    pub reconfigure: &'a SharedReconfigure,
    /// 进程内单调毫秒（DeviceLifecycle 的 now_ms 口径）。
    pub now_ms: u64,
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
    /// 校准播放已登记（块线程下一次循环开始逐块写出）。
    CalPlaybackStarted {
        /// 总块数。
        total_blocks: u64,
    },
    /// 校准播放已结束。
    CalPlaybackEnded {
        /// 结束前是否有进行中的校准播放。
        was_active: bool,
    },
    /// 热重配成功（新配置已生效并通过 hw 回读校验；实测 gap 在内）。
    Reconfigured {
        /// 机械结果 + 目标视图。
        done: ReconfigureDone,
        /// 从静音到恢复的实测空档（毫秒）。
        gap_ms: u64,
    },
    /// 热重配失败（host 已尽力按旧配置回滚；rollback 状态里如实说明）。
    ReconfigureFailed {
        /// 失败信息（稳定码/阶段/回滚状态）。
        failure: ReconfigureFailure,
        /// 从静音到恢复的实测空档（毫秒）。
        gap_ms: u64,
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

    /// 控制路径：登记一段校准播放（预渲染数据在此一次性移交；返回总块数）。
    fn begin_calibration_playback(&mut self, playback: CalibrationPlayback) -> Result<u64, String>;

    /// 控制路径：结束校准播放，返回此前是否有校准播放。
    fn end_calibration_playback(&mut self) -> bool;

    /// 用户静音请求（热重配前后原样恢复用；安全静音不在此列）。
    fn user_mute_requested(&self) -> bool;

    /// 输出设备当前是否已打开（重配失败上报用）。
    fn device_started(&self) -> bool;

    /// 收包线程当前是否在跑（重配失败上报用）。
    fn receiver_running(&self) -> bool;

    /// 控制路径：热重配（块线程执行；机械动作在 host 的 Runtime::reconfigure）。
    fn reconfigure(
        &mut self,
        request: ReconfigureRequest,
    ) -> Result<ReconfigureDone, ReconfigureFailure>;
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

    fn begin_calibration_playback(&mut self, playback: CalibrationPlayback) -> Result<u64, String> {
        Runtime::<P>::begin_calibration_playback(self, playback)
            .map_err(|err| format!("[{:?}] {err}", err.code()))
    }

    fn end_calibration_playback(&mut self) -> bool {
        Runtime::<P>::end_calibration_playback(self)
    }

    fn user_mute_requested(&self) -> bool {
        Runtime::<P>::user_mute_requested(self)
    }

    fn device_started(&self) -> bool {
        Runtime::<P>::device_started(self)
    }

    fn receiver_running(&self) -> bool {
        Runtime::<P>::receiver_running(self)
    }

    fn reconfigure(
        &mut self,
        request: ReconfigureRequest,
    ) -> Result<ReconfigureDone, ReconfigureFailure> {
        let ReconfigureRequest {
            config,
            plan,
            probe,
            ..
        } = request;
        let target = ChannelsView::from_config(&config);
        Runtime::<P>::reconfigure(self, config, plan, move || probe.probe())
            .map(|outcome| ReconfigureDone { outcome, target })
    }
}

/// 在块边界执行一条命令（块线程调用；块循环不是 RT 路径，允许少量堆操作）。
///
/// 语义：
/// - Mute：request_mute 只登记，下一块入口由 SlotEngine 生效；回执带 applied_block。
/// - CommitPlan：先校验草稿格式与数据面格式一致（format op 只改控制面编译格式时，
///   这一步如实拒绝，不把不匹配的图塞进引擎），一致才 stage。
/// - Reconfigure：设备 absent 时直接拒绝（fail-closed）；否则静音 → host 热重配（内部按
///   新配置重开设备/重建收包与输入抖动环/回读校验，失败回滚）→ 恢复用户静音；
///   gap_ms 是"静音到恢复"的实测值（不是声学空档），结果写进追踪器供 status/对账用。
pub fn execute_command<T: ControlTarget>(
    command: ControlCommand,
    target: &mut T,
    ctx: &mut CommandContext<'_>,
) {
    match command {
        ControlCommand::Mute { mute, ack } => {
            let receipt = target.request_mute(mute);
            let _ = ack.send(CommandReply::Mute {
                mute: receipt.mute,
                applied_block: receipt.applied_block,
                idempotent: receipt.idempotent,
            });
        }
        ControlCommand::CalPlaybackBegin { playback, ack } => {
            match target.begin_calibration_playback(playback) {
                Ok(total_blocks) => {
                    let _ = ack.send(CommandReply::CalPlaybackStarted { total_blocks });
                }
                Err(message) => {
                    let _ = ack.send(CommandReply::Rejected {
                        code: error::NOT_APPLIED,
                        message: format!("calibration playback not started: {message}"),
                    });
                }
            }
        }
        ControlCommand::CalPlaybackEnd { ack } => {
            let was_active = target.end_calibration_playback();
            let _ = ack.send(CommandReply::CalPlaybackEnded { was_active });
        }
        ControlCommand::Reconfigure {
            token,
            config,
            plan,
            probe,
            ack,
        } => {
            {
                let mut tracker = crate::control::reconfigure::lock_tracker(ctx.reconfigure);
                tracker.begin(&token);
            }
            // 设备 absent：fail-closed 拒绝（不碰句柄；退避重开由监督循环负责，进程不退出）。
            if ctx.lifecycle.is_absent() {
                let failure = ReconfigureFailure {
                    code: "device_absent",
                    stage: "preflight",
                    message: "output device is absent: channels_commit is rejected fail-closed; the block loop keeps retrying the device with backoff and the process stays up (retry after the device recovers)".to_string(),
                    rolled_back: true,
                    device_started: target.device_started(),
                    receiver_running: target.receiver_running(),
                };
                let generation = {
                    let tracker = crate::control::reconfigure::lock_tracker(ctx.reconfigure);
                    tracker.generation
                };
                {
                    let mut tracker = crate::control::reconfigure::lock_tracker(ctx.reconfigure);
                    tracker.record_failure(&token, 0, failure_json(&failure, Some(0), generation));
                }
                let _ = ack.send(CommandReply::ReconfigureFailed { failure, gap_ms: 0 });
                return;
            }
            // 块线程在块边界：先安全静音（旧格式也静音），再执行热重配，最后恢复用户静音。
            let was_muted = target.user_mute_requested();
            let started = Instant::now();
            let _ = target.request_mute(true);
            let result = target.reconfigure(ReconfigureRequest {
                config: *config,
                plan,
                probe,
            });
            let _ = target.request_mute(was_muted);
            let gap_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let generation = {
                let tracker = crate::control::reconfigure::lock_tracker(ctx.reconfigure);
                tracker.generation
            };
            match result {
                Ok(done) => {
                    let next_generation = generation.saturating_add(1);
                    let applied = AppliedReconfigure {
                        token: token.clone(),
                        gap_ms,
                        target: done.target.clone(),
                        format: done.outcome.format.clone(),
                        engine_changed: done.outcome.engine_rebuilt,
                        device_was_absent: done.outcome.device_was_absent,
                        graph_dropped: done.outcome.graph_dropped,
                    };
                    let result_json = json!({
                        "ok": true,
                        "op": "channels_commit",
                        "applied": true,
                        "generation": next_generation,
                        "gap_ms": gap_ms,
                        "format": {
                            "block_frames": done.outcome.format.block_frames,
                            "sample_rate": done.outcome.format.sample_rate,
                            "channels": done.outcome.format.output_channels,
                        },
                        "engine_rebuilt": done.outcome.engine_rebuilt,
                        "graph_dropped": done.outcome.graph_dropped,
                        "device_was_absent": done.outcome.device_was_absent,
                        "receiver_rebound": done.outcome.receiver_rebound,
                        "hw": {
                            "sample_rate": done.outcome.hw.sample_rate,
                            "channels": done.outcome.hw.channels,
                        },
                        "blocks": done.outcome.blocks,
                    });
                    {
                        let mut tracker =
                            crate::control::reconfigure::lock_tracker(ctx.reconfigure);
                        tracker.record_success(applied, gap_ms, result_json);
                    }
                    // 设备此前 absent、这次重配把它打开了：设备生命周期如实转为 present。
                    if done.outcome.device_was_absent && ctx.lifecycle.is_absent() {
                        ctx.lifecycle.note_recovered(ctx.now_ms);
                    }
                    let _ = ack.send(CommandReply::Reconfigured { done, gap_ms });
                }
                Err(failure) => {
                    let result_json = failure_json(&failure, Some(gap_ms), generation);
                    {
                        let mut tracker =
                            crate::control::reconfigure::lock_tracker(ctx.reconfigure);
                        tracker.record_failure(&token, gap_ms, result_json);
                    }
                    // 回滚后设备没打开：如实登记 absent（监督循环按退避重开，进程不退出）。
                    if !failure.device_started {
                        ctx.lifecycle.note_absent(
                            ctx.now_ms,
                            format!(
                                "channels reconfiguration failed at stage {} ({}): {}; the previous configuration was restored but the output device is not open",
                                failure.stage, failure.code, failure.message
                            ),
                        );
                    }
                    let _ = ack.send(CommandReply::ReconfigureFailed { failure, gap_ms });
                }
            }
        }
        ControlCommand::CommitPlan { draft, format, ack } => {
            let runtime_format = target.data_plane_format();
            if format != runtime_format {
                let _ = ack.send(CommandReply::Rejected {
                    code: error::NOT_APPLIED,
                    message: format!(
                        "prepared plan targets {} Hz / {} frames / {} ch, but the running rt-host data plane is {} Hz / {} frames / {} ch; draft not staged. Prepare the plan against the current data-plane format, or reconfigure the data plane first with channels_prepare/channels_commit (M3b hot reconfiguration; the plan format then follows the new engine format)",
                        format.sample_rate,
                        format.block_frames,
                        format.output_channels,
                        runtime_format.sample_rate,
                        runtime_format.block_frames,
                        runtime_format.output_channels
                    ),
                });
                return;
            }
            let applied_block = target.stage_plan_draft(draft);
            let _ = ack.send(CommandReply::PlanApplied { applied_block });
        }
    }
}

/// 测试/单命令入口：没有真实设备生命周期与追踪器的场合（校准测试等）。
///
/// 热重配命令需要真实上下文（absent 判定/追踪器），请用 [`execute_command`]。
#[cfg(test)]
pub fn execute_command_isolated<T: ControlTarget>(command: ControlCommand, target: &mut T) {
    let mut lifecycle = DeviceLifecycle::new();
    let tracker = super::reconfigure::shared_tracker();
    let mut ctx = CommandContext {
        lifecycle: &mut lifecycle,
        reconfigure: &tracker,
        now_ms: 0,
    };
    execute_command(command, target, &mut ctx);
}
