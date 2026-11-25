//! 运行时控制面：`status` / `mute` / `volume`。
//!
//! 挂在**同一只** Unix socket（默认 `/run/khaen-rt-stack/host.sock`，
//! `KHAEN_HOST_SOCK` 可换）上，与既有校准 op（enter/measure/solve/
//! remeasure/leave）在同一个 `CalControl::handle` 里分发，不新开线程、
//! 不另建监听。
//!
//! ## 事实边界（2026-11-25 源码现状）
//!
//! - 生产 `khaen-rt-host run` 的块循环本来只在**进程退出**时把 blocks/
//!   frames、deadline miss、gate 拒绝、safety mute、ALSA sink 的 xrun/underrun/
//!   overrun/recovery/fault 写进最终报告。本模块把这些计数以原子快照发布给 socket
//!   （非 RT）侧；音频线程既不加锁、不分配，也不做任何 I/O。
//! - host 音频链**没有音量通路**：plan 系数与 ALSA S24_3LE 直写之间没有
//!   snd_mixer/fader 元件，块循环里也没有任何增益状态。`volume` op 因此固定
//!   返回 `unsupported` 并给出原因与替代方案，不假装成功、不静默夹取。
//! - `mute` 是唯一的写入通路：只写“整块静音”标志（输出 0 样本），
//!   不改任何系数/音量；写入在**块边界**生效（`begin_block` 处 latch），
//!   回执带生效后的 actual mute 与生效块号（`applied_block`）。
//!
//! ## RT 安全
//!
//! - 音频线程只做 `AtomicU64`/`AtomicBool`/`AtomicU32` 的
//!   load/store/fetch_add（Relaxed；只有块边界生效握手用 Release/Acquire）；
//! - 静态元数据（graph/sample_rate/block_size/graph_id/last_good）只在启动期由非 RT
//!   线程写入 `Mutex<Option<RuntimeMeta>>`，之后的读取都在 socket 线程；
//!   音频线程从不触碰该锁（有单测 `rt_path_never_locks_metadata` 证明）；
//! - socket 线程最多在 `APPLY_TIMEOUT_MS` 内轮询 `applied_seq`，不忙等 RT；
//! - `RuntimeControl::new()` 是进程内唯一状态（`shared()`）；
//!   `start_run` 复位动态计数，供同一进程内重复 run / 测试。
//!
//! ## 未接线（如实上报，不伪造）
//!
//! - `volume`：无音频通路（见上）；
//! - `prepare` / `commit`：host 只在 legacy 路径里做过同图 re-plan v2
//!   演示，没有对外的 prepare/commit op —— 属于 daemon 侧 HostEngine 的下一任务。

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::cal_control::json_bool;

/// 与 `khaen-control` REST 合约同口径的音量上下限（dB，
/// `crates/control/src/rest.rs` 的 VOLUME_MIN_DB/VOLUME_MAX_DB）。
pub const VOLUME_MIN_DB: f64 = -150.0;
/// 与 `khaen-control` REST 合约同口径的音量上限（dB）。
pub const VOLUME_MAX_DB: f64 = 50.0;

/// mute 写入等待“块边界生效”的上限。48 kHz/128 = 2.67 ms/块，250 ms 约 90 块。
pub const APPLY_TIMEOUT_MS: u64 = 250;
const APPLY_POLL_US: u64 = 200;

pub const STATE_IDLE: u8 = 0;
pub const STATE_STARTING: u8 = 1;
pub const STATE_RUNNING: u8 = 2;
pub const STATE_STOPPED: u8 = 3;

/// status JSON schema（宿主侧运行时快照；daemon HostEngine 原样透传）。
pub const STATUS_SCHEMA: &str = "khaen-rt-host-status/v1";

/// 已接线的写能力（status JSON `wired`）：daemon 侧据此决定是否写。
pub const WIRED: &[&str] = &["status", "mute"];
/// 未接线的写能力（`not_wired`）：volume 无音频通路；
/// prepare/commit 属 daemon HostEngine 下一任务（host 内部只有同图 re-plan 演示）。
pub const NOT_WIRED: &[&str] = &["volume", "prepare", "commit"];

static HUB: OnceLock<RuntimeControl> = OnceLock::new();

fn state_name(state: u8) -> &'static str {
    match state {
        STATE_STARTING => "Starting",
        STATE_RUNNING => "Running",
        STATE_STOPPED => "Stopped",
        _ => "Idle",
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out
}

fn json_bool_text(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

/// ALSA/fake sink 的实时计数（status 快照的一部分；非 RT 导出，无 Drop）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SinkCounters {
    pub blocks: u64,
    pub bytes_written: u64,
    pub silence_blocks: u64,
    pub xruns: u64,
    pub underruns: u64,
    pub overruns: u64,
    pub recoveries: u64,
    pub muted_blocks: u64,
    pub faults: u64,
    pub short_reads: u64,
    pub clipped_samples: u64,
    pub nonfinite_in: u64,
    pub nonfinite_out: u64,
}

/// 一个已完成块的发布快照（块循环末尾调用一次；全部 Copy、零分配）。
#[derive(Clone, Copy, Debug, Default)]
pub struct BlockSnapshot {
    /// 刚完成块的序号（0 基）。
    pub block_seq: u64,
    /// 本块输出是静音（安全静音或控制面 forced mute）。
    pub mute_this_block: bool,
    /// 本块静音的原因包含安全路径（safe_mode / 调度门 / capture 失流）。
    pub safety_mute: bool,
    /// 本块静音由控制面 forced mute 触发（与 `safety_mute` 同时为真时按安全侧计）。
    pub forced_mute: bool,
    pub deadline_misses: u64,
    pub gate_rejects: u64,
    pub watchdog_actions: u64,
    pub sink: SinkCounters,
}

/// 启动期静态元数据（只有非 RT 线程写；之后只读）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeMeta {
    pub graph: String,
    /// 32 B graph_id 的小写 hex（无 plan 的 legacy 图 = 空串，`graph_id_source="none"`）。
    pub graph_id: String,
    /// `plan-header`（T20 plan）或 `none`（stereo_identity/2x2_offdiag）。
    pub graph_id_source: String,
    /// `alsa` 或 `fake`（host 报告同口径）。
    pub backend: String,
    /// `normal` 或 `mute`（配置 [host] safe_mode）。
    pub safe_mode: String,
    pub sample_rate: u32,
    pub block_size: u16,
    pub epoch: u64,
    /// 初始 plan_version（legacy 路径后续 re-plan v2 时用 `publish_plan_version` 更新）。
    pub plan_version: u32,
    /// last-good 文件的 JSON 片段（`main.rs::last_good_report` 输出）。
    pub last_good: String,
}

/// 进程内运行时控制面（单例：`shared()`）。
#[derive(Debug)]
pub struct RuntimeControl {
    meta: Mutex<Option<RuntimeMeta>>,
    state: AtomicU8,
    plan_version: AtomicU32,
    blocks: AtomicU64,
    xruns: AtomicU64,
    underruns: AtomicU64,
    overruns: AtomicU64,
    recoveries: AtomicU64,
    faults: AtomicU64,
    gate_rejects: AtomicU64,
    watchdog_actions: AtomicU64,
    deadline_misses: AtomicU64,
    silence_blocks: AtomicU64,
    muted_blocks: AtomicU64,
    safety_mute_blocks: AtomicU64,
    forced_mute_blocks: AtomicU64,
    clipped_samples: AtomicU64,
    current_block: AtomicU64,
    silent: AtomicBool,
    safety_mute: AtomicBool,
    /// 控制面请求的 forced mute（socket 线程写）。
    requested_mute: AtomicBool,
    /// 请求序号（socket 线程递增；块边界 latch 后写 `applied_seq`）。
    requested_seq: AtomicU64,
    /// 已 latch 的请求序号。
    applied_seq: AtomicU64,
    /// 请求生效的块号。
    applied_block: AtomicU64,
    /// 已生效的 forced mute（块循环写）。
    applied_forced_mute: AtomicBool,
}

impl Default for RuntimeControl {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeControl {
    pub fn new() -> Self {
        Self {
            meta: Mutex::new(None),
            state: AtomicU8::new(STATE_IDLE),
            plan_version: AtomicU32::new(0),
            blocks: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            recoveries: AtomicU64::new(0),
            faults: AtomicU64::new(0),
            gate_rejects: AtomicU64::new(0),
            watchdog_actions: AtomicU64::new(0),
            deadline_misses: AtomicU64::new(0),
            silence_blocks: AtomicU64::new(0),
            muted_blocks: AtomicU64::new(0),
            safety_mute_blocks: AtomicU64::new(0),
            forced_mute_blocks: AtomicU64::new(0),
            clipped_samples: AtomicU64::new(0),
            current_block: AtomicU64::new(0),
            silent: AtomicBool::new(false),
            safety_mute: AtomicBool::new(false),
            requested_mute: AtomicBool::new(false),
            requested_seq: AtomicU64::new(0),
            applied_seq: AtomicU64::new(0),
            applied_block: AtomicU64::new(0),
            applied_forced_mute: AtomicBool::new(false),
        }
    }

    /// 启动期（非 RT）登记静态元数据并复位动态计数/静音握手。
    ///
    /// 同一进程内重复 run（测试/离线 harness）会覆盖上一次的元数据；生产进程只调用一次。
    pub fn start_run(&self, meta: RuntimeMeta) {
        let plan_version = meta.plan_version;
        if let Ok(mut slot) = self.meta.lock() {
            *slot = Some(meta);
        }
        self.plan_version.store(plan_version, Ordering::Release);
        self.state.store(STATE_STARTING, Ordering::Release);
        self.blocks.store(0, Ordering::Relaxed);
        self.xruns.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.overruns.store(0, Ordering::Relaxed);
        self.recoveries.store(0, Ordering::Relaxed);
        self.faults.store(0, Ordering::Relaxed);
        self.gate_rejects.store(0, Ordering::Relaxed);
        self.watchdog_actions.store(0, Ordering::Relaxed);
        self.deadline_misses.store(0, Ordering::Relaxed);
        self.silence_blocks.store(0, Ordering::Relaxed);
        self.muted_blocks.store(0, Ordering::Relaxed);
        self.safety_mute_blocks.store(0, Ordering::Relaxed);
        self.forced_mute_blocks.store(0, Ordering::Relaxed);
        self.clipped_samples.store(0, Ordering::Relaxed);
        self.current_block.store(0, Ordering::Relaxed);
        self.silent.store(false, Ordering::Relaxed);
        self.safety_mute.store(false, Ordering::Relaxed);
        self.requested_mute.store(false, Ordering::Relaxed);
        self.requested_seq.store(0, Ordering::Relaxed);
        self.applied_seq.store(0, Ordering::Relaxed);
        self.applied_block.store(0, Ordering::Relaxed);
        self.applied_forced_mute.store(false, Ordering::Relaxed);
    }

    pub fn mark_running(&self) {
        self.state.store(STATE_RUNNING, Ordering::Release);
    }

    pub fn mark_stopped(&self) {
        self.state.store(STATE_STOPPED, Ordering::Release);
    }

    /// legacy 路径同图 re-plan v2 生效后更新实际 plan_version（非 RT 调用）。
    pub fn publish_plan_version(&self, plan_version: u32) {
        self.plan_version.store(plan_version, Ordering::Release);
    }

    /// [RT] 块边界：latch 挂起中的 mute 请求并返回本块应否静音。只做原子读写。
    ///
    /// 读取顺序固定为 seq → requested → applied：socket 线程先写 `requested_mute`、
    /// 再 Release 递增 `requested_seq`，所以 Acquire 读到新 seq 后一定看得到新
    /// requested（不会用旧值 latch 新序号）。
    pub fn begin_block(&self, block_seq: u64) -> bool {
        self.current_block.store(block_seq, Ordering::Relaxed);
        let pending = self.requested_seq.load(Ordering::Acquire);
        let requested = self.requested_mute.load(Ordering::Acquire);
        let applied = self.applied_seq.load(Ordering::Acquire);
        if pending != applied {
            self.applied_forced_mute.store(requested, Ordering::Release);
            self.applied_block.store(block_seq, Ordering::Release);
            self.applied_seq.store(pending, Ordering::Release);
        }
        self.applied_forced_mute.load(Ordering::Acquire)
    }

    /// [RT] 一个块完成后发布快照。全部原子写，零分配、不加锁。
    pub fn publish_block(&self, snapshot: BlockSnapshot) {
        self.blocks
            .store(snapshot.block_seq.saturating_add(1), Ordering::Release);
        let sink = snapshot.sink;
        self.xruns.store(sink.xruns, Ordering::Relaxed);
        self.underruns.store(sink.underruns, Ordering::Relaxed);
        self.overruns.store(sink.overruns, Ordering::Relaxed);
        self.recoveries.store(sink.recoveries, Ordering::Relaxed);
        self.faults.store(
            sink.faults
                .saturating_add(snapshot.gate_rejects)
                .saturating_add(snapshot.watchdog_actions),
            Ordering::Relaxed,
        );
        self.gate_rejects
            .store(snapshot.gate_rejects, Ordering::Relaxed);
        self.watchdog_actions
            .store(snapshot.watchdog_actions, Ordering::Relaxed);
        self.deadline_misses
            .store(snapshot.deadline_misses, Ordering::Relaxed);
        self.clipped_samples
            .store(sink.clipped_samples, Ordering::Relaxed);
        if snapshot.mute_this_block {
            self.silence_blocks.fetch_add(1, Ordering::Relaxed);
            self.muted_blocks.fetch_add(1, Ordering::Relaxed);
            if snapshot.safety_mute {
                self.safety_mute_blocks.fetch_add(1, Ordering::Relaxed);
            } else if snapshot.forced_mute {
                self.forced_mute_blocks.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.silent
            .store(snapshot.mute_this_block, Ordering::Relaxed);
        self.safety_mute.store(
            snapshot.safety_mute && snapshot.mute_this_block,
            Ordering::Relaxed,
        );
    }

    fn meta(&self) -> Option<RuntimeMeta> {
        self.meta.lock().ok().and_then(|slot| slot.clone())
    }

    fn state_value(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    fn effective_mute(&self, safe_mode: &str) -> bool {
        self.applied_forced_mute.load(Ordering::Acquire)
            || self.safety_mute.load(Ordering::Acquire)
            || safe_mode == "mute"
    }

    /// 运行时快照 JSON（status op；字段名对齐 host 最终报告 / `EngineSnapshot` 口径）。
    pub fn status_json(&self) -> String {
        let meta = self.meta();
        let graph = meta.as_ref().map(|meta| meta.graph.as_str()).unwrap_or("");
        let graph_id = meta
            .as_ref()
            .map(|meta| meta.graph_id.as_str())
            .unwrap_or("");
        let graph_id_source = meta
            .as_ref()
            .map(|meta| meta.graph_id_source.as_str())
            .unwrap_or("none");
        let backend = meta
            .as_ref()
            .map(|meta| meta.backend.as_str())
            .unwrap_or("none");
        let safe_mode = meta
            .as_ref()
            .map(|meta| meta.safe_mode.as_str())
            .unwrap_or("normal");
        let sample_rate = meta.as_ref().map(|meta| meta.sample_rate).unwrap_or(0);
        let block_size = meta
            .as_ref()
            .map(|meta| meta.block_size as u64)
            .unwrap_or(0);
        let epoch = meta.as_ref().map(|meta| meta.epoch).unwrap_or(0);
        let last_good = meta
            .as_ref()
            .map(|meta| meta.last_good.as_str())
            .unwrap_or("{\"present\":false}");
        let state = state_name(self.state_value());
        let plan_version = self.plan_version.load(Ordering::Acquire);
        let blocks = self.blocks.load(Ordering::Acquire);
        let frames = blocks.saturating_mul(block_size);
        let forced_mute = self.applied_forced_mute.load(Ordering::Acquire);
        let safety_mute = self.safety_mute.load(Ordering::Acquire);
        let mute = self.effective_mute(safe_mode);
        let silent = self.silent.load(Ordering::Acquire);
        let actual = state == "Running";
        let mut out = String::with_capacity(2048);
        let _ = write!(
            out,
            "{{\"ok\":true,\"error\":\"\",\"op\":\"status\",\"schema\":\"{}\"",
            STATUS_SCHEMA
        );
        out.push_str(",\"engine\":\"rt-host\",\"synthetic\":false");
        let _ = write!(out, ",\"actual\":{}", json_bool_text(actual));
        let _ = write!(out, ",\"runtime_state\":\"{}\"", state);
        // 对齐 daemon EngineSnapshot.device_owner 枚举：alsa = host 独占播放；fake = 未持有设备。
        let device_owner = if backend == "alsa" {
            "playback"
        } else {
            "idle"
        };
        let _ = write!(out, ",\"device_owner\":\"{device_owner}\"");
        let _ = write!(out, ",\"backend\":\"{}\"", json_escape(backend));
        let _ = write!(out, ",\"graph\":\"{}\"", json_escape(graph));
        let _ = write!(out, ",\"graph_id\":\"{}\"", json_escape(graph_id));
        let _ = write!(
            out,
            ",\"graph_id_source\":\"{}\"",
            json_escape(graph_id_source)
        );
        let _ = write!(
            out,
            ",\"sample_rate\":{sample_rate},\"chunksize\":{block_size},\"block_size\":{block_size}"
        );
        let _ = write!(
            out,
            ",\"epoch\":{epoch},\"actual_version\":\"v{plan_version:04}\",\"plan_version\":{plan_version}"
        );
        let _ = write!(out, ",\"blocks\":{blocks},\"frames\":{frames}");
        let _ = write!(
            out,
            ",\"xruns\":{},\"underruns\":{},\"overruns\":{},\"recoveries\":{}",
            self.xruns.load(Ordering::Acquire),
            self.underruns.load(Ordering::Acquire),
            self.overruns.load(Ordering::Acquire),
            self.recoveries.load(Ordering::Acquire)
        );
        let _ = write!(
            out,
            ",\"faults\":{},\"gate_rejects\":{},\"watchdog_actions\":{}",
            self.faults.load(Ordering::Acquire),
            self.gate_rejects.load(Ordering::Acquire),
            self.watchdog_actions.load(Ordering::Acquire)
        );
        let _ = write!(
            out,
            ",\"deadline_misses\":{},\"clipped_samples\":{}",
            self.deadline_misses.load(Ordering::Acquire),
            self.clipped_samples.load(Ordering::Acquire)
        );
        let _ = write!(
            out,
            ",\"silence_blocks\":{},\"muted_blocks\":{},\"safety_mute_blocks\":{},\"forced_mute_blocks\":{}",
            self.silence_blocks.load(Ordering::Acquire),
            self.muted_blocks.load(Ordering::Acquire),
            self.safety_mute_blocks.load(Ordering::Acquire),
            self.forced_mute_blocks.load(Ordering::Acquire)
        );
        let _ = write!(
            out,
            ",\"forced_mute\":{},\"mute\":{},\"silent\":{},\"safety_mute\":{},\"safe_mode\":\"{}\"",
            json_bool_text(forced_mute),
            json_bool_text(mute),
            json_bool_text(silent),
            json_bool_text(safety_mute),
            json_escape(safe_mode)
        );
        out.push_str(",\"volume_db\":null");
        let _ = write!(out, ",\"last_good_coeff\":{last_good}");
        out.push_str(",\"wired\":[");
        append_array(&mut out, WIRED);
        out.push_str("],\"not_wired\":[");
        append_array(&mut out, NOT_WIRED);
        out.push_str("]}");
        out
    }

    /// op 分发（由 `CalControl::handle` 在未知 op 之前调用）。
    ///
    /// 返回 `Some(response)` = 本模块接管的 op；`None` = 交给校准侧
    /// 维持既有 `unknown_op` 行为。
    pub fn handle(&self, op: &str, line: &str) -> Option<String> {
        match op {
            "status" | "runtime_status" => Some(self.status_json()),
            "mute" => Some(self.mute_json(json_bool(line, "mute"))),
            "volume" => Some(self.volume_json(parse_json_f64(line, "db"))),
            _ => None,
        }
    }

    fn snapshot_tail(&self, out: &mut String) {
        let meta = self.meta();
        let state = state_name(self.state_value());
        let plan_version = self.plan_version.load(Ordering::Acquire);
        let safe_mode = meta
            .as_ref()
            .map(|meta| meta.safe_mode.as_str())
            .unwrap_or("normal");
        let epoch = meta.as_ref().map(|meta| meta.epoch).unwrap_or(0);
        let graph = meta.as_ref().map(|meta| meta.graph.as_str()).unwrap_or("");
        let graph_id = meta
            .as_ref()
            .map(|meta| meta.graph_id.as_str())
            .unwrap_or("");
        let sample_rate = meta.as_ref().map(|meta| meta.sample_rate).unwrap_or(0);
        let block_size = meta
            .as_ref()
            .map(|meta| meta.block_size as u64)
            .unwrap_or(0);
        let backend = meta
            .as_ref()
            .map(|meta| meta.backend.as_str())
            .unwrap_or("none");
        let actual = state == "Running";
        let _ = write!(
            out,
            ",\"runtime_state\":\"{}\",\"engine\":\"rt-host\",\"synthetic\":false,\"actual\":{}",
            state,
            json_bool_text(actual)
        );
        let _ = write!(
            out,
            ",\"epoch\":{epoch},\"actual_version\":\"v{plan_version:04}\""
        );
        let _ = write!(out, ",\"graph\":\"{}\"", json_escape(graph));
        let _ = write!(out, ",\"graph_id\":\"{}\"", json_escape(graph_id));
        let _ = write!(out, ",\"backend\":\"{}\"", json_escape(backend));
        let _ = write!(
            out,
            ",\"sample_rate\":{sample_rate},\"block_size\":{block_size}"
        );
        let _ = write!(out, ",\"safe_mode\":\"{}\"", json_escape(safe_mode));
    }

    fn error_json(&self, op: &str, error: &str, reason: &str) -> String {
        let mut out = String::with_capacity(512);
        let _ = write!(
            out,
            "{{\"ok\":false,\"error\":\"{}\",\"op\":\"{}\",\"reason\":\"{}\"",
            json_escape(error),
            json_escape(op),
            json_escape(reason)
        );
        self.snapshot_tail(&mut out);
        out.push('}');
        out
    }

    fn mute_receipt(&self, ok: bool, requested: bool, idempotent: bool, error: &str) -> String {
        let meta = self.meta();
        let safe_mode = meta
            .as_ref()
            .map(|meta| meta.safe_mode.as_str())
            .unwrap_or("normal");
        let forced_mute = self.applied_forced_mute.load(Ordering::Acquire);
        let safety_mute = self.safety_mute.load(Ordering::Acquire);
        let mute = self.effective_mute(safe_mode);
        let silent = self.silent.load(Ordering::Acquire);
        let applied_block = self.applied_block.load(Ordering::Acquire);
        let applied_seq = self.applied_seq.load(Ordering::Acquire);
        let mut out = String::with_capacity(768);
        let _ = write!(
            out,
            "{{\"ok\":{},\"error\":\"{}\",\"op\":\"mute\",\"accepted\":{},\"requested_mute\":{}",
            json_bool_text(ok),
            json_escape(error),
            json_bool_text(ok),
            json_bool_text(requested)
        );
        let _ = write!(
            out,
            ",\"mute\":{},\"forced_mute\":{},\"safety_mute\":{},\"silent\":{}",
            json_bool_text(mute),
            json_bool_text(forced_mute),
            json_bool_text(safety_mute),
            json_bool_text(silent)
        );
        let _ = write!(
            out,
            ",\"applied_block\":{applied_block},\"applied_seq\":{applied_seq},\"idempotent\":{}",
            json_bool_text(idempotent)
        );
        out.push_str(",\"volume_db\":null");
        self.snapshot_tail(&mut out);
        out.push('}');
        out
    }

    /// `mute` op：写整块静音，块边界生效；幂等（重复写同一值返回同一生效块号）。
    fn mute_json(&self, requested: Option<bool>) -> String {
        let Some(requested) = requested else {
            return self.error_json(
                "mute",
                "bad_mute",
                "mute 必须是 JSON 布尔：{\"op\":\"mute\",\"mute\":true|false}",
            );
        };
        if self.state_value() != STATE_RUNNING {
            return self.error_json(
                "mute",
                "runtime_not_running",
                "host 块循环未运行：mute 只能在块边界生效（先让 khaen-rt-host run 进入 Running）",
            );
        }
        let applied_forced = self.applied_forced_mute.load(Ordering::Acquire);
        let pending =
            self.requested_seq.load(Ordering::Acquire) != self.applied_seq.load(Ordering::Acquire);
        if !pending && applied_forced == requested {
            return self.mute_receipt(true, requested, true, "");
        }
        // 先写值、再 Release 递增序号（begin_block 按 seq→value 顺序读）。
        self.requested_mute.store(requested, Ordering::Release);
        let seq = self
            .requested_seq
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let deadline = Instant::now() + Duration::from_millis(APPLY_TIMEOUT_MS);
        loop {
            if self.applied_seq.load(Ordering::Acquire) >= seq {
                return self.mute_receipt(true, requested, false, "");
            }
            if Instant::now() >= deadline {
                return self.mute_receipt(false, requested, false, "not_applied");
            }
            std::thread::sleep(Duration::from_micros(APPLY_POLL_US));
        }
    }

    /// `volume` op：先做与 REST 同口径的范围校验，再如实返回 unsupported。
    ///
    /// host 音频链没有音量通路（plan 系数固定 + ALSA S24_3LE 直写，无 snd_mixer/fader
    /// 状态），本构建不新增混音线程/增益通路，所以越界显式拒绝、合法值明确
    /// `unsupported`，不假装成功。
    fn volume_json(&self, db: Option<f64>) -> String {
        let Some(db) = db else {
            return self.error_json(
                "volume",
                "bad_volume",
                "db 必须是有限数字：{\"op\":\"volume\",\"db\":-6.0}",
            );
        };
        if !db.is_finite() || !(VOLUME_MIN_DB..=VOLUME_MAX_DB).contains(&db) {
            let mut out = String::with_capacity(512);
            let _ = write!(
                out,
                "{{\"ok\":false,\"error\":\"volume_out_of_range\",\"op\":\"volume\",\"requested_db\":{db},\"min_db\":{VOLUME_MIN_DB},\"max_db\":{VOLUME_MAX_DB},\"volume_db\":null"
            );
            self.snapshot_tail(&mut out);
            out.push('}');
            return out;
        }
        let mut out = String::with_capacity(1024);
        let _ = write!(
            out,
            "{{\"ok\":false,\"error\":\"unsupported\",\"op\":\"volume\",\"supported\":false,\"requested_db\":{db},\"volume_db\":null,\"min_db\":{VOLUME_MIN_DB},\"max_db\":{VOLUME_MAX_DB}"
        );
        out.push_str(
            ",\"reason\":\"host 音频链没有音量通路：plan 系数与 ALSA S24_3LE 直写之间没有 snd_mixer/fader 元件，块循环里也没有任何增益状态；本构建不新增混音线程/无锁增益通路，因此不能假装写入成功\"",
        );
        out.push_str(
            ",\"alternatives\":[\"在 host 之外用 ALSA mixer（如 amixer -c <card>）调硬件音量：绕过 host 系数链，未接线/未验收\",\"在下一版 plan ABI 增加 per-role gain 并在块边界切换：需要 plan 重编/commit 通路\",\"由 daemon 的 prepare/commit 重新生成含增益的系数后整体换图：prepare/commit op 未接线（见 not_wired）\"]",
        );
        self.snapshot_tail(&mut out);
        out.push('}');
        out
    }
}

fn append_array(out: &mut String, items: &[&str]) {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "\"{}\"", json_escape(item));
    }
}

/// 请求里的数字字段（严格 token：数字/+-./eE，后接逗号/右括号/结束；非有限值拒绝）。
fn parse_json_f64(line: &str, key: &str) -> Option<f64> {
    let pattern = format!("\"{key}\":");
    let start = line.find(&pattern)? + pattern.len();
    let rest = line[start..].trim_start();
    let end = rest
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let tail = rest[end..].trim_start();
    if !(tail.is_empty() || tail.starts_with(',') || tail.starts_with('}')) {
        return None;
    }
    let value: f64 = rest[..end].parse().ok()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

/// 进程内唯一运行时控制面。
pub fn shared() -> &'static RuntimeControl {
    HUB.get_or_init(RuntimeControl::new)
}

/// 生产入口：启动期登记元数据（非 RT）。
pub fn start_run(meta: RuntimeMeta) {
    shared().start_run(meta);
}

/// 生产入口：块循环进入 Running（非 RT，循环前调用一次）。
pub fn mark_running() {
    shared().mark_running();
}

/// 生产入口：块循环退出（非 RT）。
pub fn mark_stopped() {
    shared().mark_stopped();
}

/// 生产入口：plan_version 变化（同图 re-plan v2 生效后）。
pub fn publish_plan_version(plan_version: u32) {
    shared().publish_plan_version(plan_version);
}

/// 生产入口：[RT] 块边界 latch（返回本块是否 forced mute）。
pub fn begin_block(block_seq: u64) -> bool {
    shared().begin_block(block_seq)
}

/// 生产入口：[RT] 块结束后发布快照。
pub fn publish_block(snapshot: BlockSnapshot) {
    shared().publish_block(snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cal_control::{json_bool, json_f64, json_string};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn meta() -> RuntimeMeta {
        RuntimeMeta {
            graph: "plan-arena".to_string(),
            graph_id: "ab".repeat(32),
            graph_id_source: "plan-header".to_string(),
            backend: "fake".to_string(),
            safe_mode: "normal".to_string(),
            sample_rate: 48_000,
            block_size: 128,
            epoch: 1,
            plan_version: 1,
            last_good: "{\"present\":false}".to_string(),
        }
    }

    fn number(body: &str, key: &str) -> f64 {
        json_f64(body, key).unwrap_or_else(|| panic!("缺少数字字段 {key}：{body}"))
    }

    fn count(body: &str, key: &str) -> u64 {
        number(body, key) as u64
    }

    fn text(body: &str, key: &str) -> String {
        json_string(body, key).unwrap_or_else(|| panic!("缺少字符串字段 {key}：{body}"))
    }

    fn flag(body: &str, key: &str) -> bool {
        json_bool(body, key).unwrap_or_else(|| panic!("缺少布尔字段 {key}：{body}"))
    }

    /// status 快照字段齐全、取值自洽（frames == blocks·chunksize；静音块分解守恒）。
    #[test]
    fn status_snapshot_is_complete_and_self_consistent() {
        let control = RuntimeControl::new();
        let idle = control.status_json();
        assert_eq!(text(&idle, "runtime_state"), "Idle");
        assert_eq!(text(&idle, "schema"), STATUS_SCHEMA);
        assert!(!flag(&idle, "actual"));
        assert_eq!(count(&idle, "blocks"), 0);
        assert_eq!(count(&idle, "frames"), 0);
        for key in [
            "runtime_state",
            "graph",
            "graph_id",
            "backend",
            "sample_rate",
            "chunksize",
            "blocks",
            "frames",
            "xruns",
            "underruns",
            "faults",
            "deadline_misses",
            "silence_blocks",
            "forced_mute",
            "safe_mode",
            "epoch",
            "actual_version",
            "last_good_coeff",
        ] {
            assert!(
                idle.contains(&format!("\"{key}\":")),
                "缺字段 {key}：{idle}"
            );
        }
        assert!(idle.contains("\"volume_db\":null"), "{idle}");
        assert!(
            idle.contains("\"last_good_coeff\":{\"present\":false}"),
            "{idle}"
        );

        control.start_run(meta());
        control.mark_running();
        control.publish_block(BlockSnapshot {
            block_seq: 0,
            gate_rejects: 1,
            watchdog_actions: 2,
            deadline_misses: 3,
            sink: SinkCounters {
                xruns: 4,
                underruns: 5,
                overruns: 6,
                recoveries: 7,
                faults: 8,
                clipped_samples: 9,
                ..SinkCounters::default()
            },
            ..BlockSnapshot::default()
        });
        control.publish_block(BlockSnapshot {
            block_seq: 1,
            mute_this_block: true,
            safety_mute: true,
            gate_rejects: 1,
            watchdog_actions: 2,
            deadline_misses: 3,
            sink: SinkCounters {
                xruns: 4,
                underruns: 5,
                overruns: 6,
                recoveries: 7,
                faults: 8,
                clipped_samples: 9,
                ..SinkCounters::default()
            },
            ..BlockSnapshot::default()
        });
        let body = control.status_json();
        assert_eq!(text(&body, "runtime_state"), "Running");
        assert!(flag(&body, "actual"));
        assert_eq!(text(&body, "graph"), "plan-arena");
        assert_eq!(text(&body, "graph_id"), "ab".repeat(32));
        assert_eq!(text(&body, "graph_id_source"), "plan-header");
        assert_eq!(count(&body, "sample_rate"), 48_000);
        assert_eq!(count(&body, "chunksize"), 128);
        assert_eq!(count(&body, "block_size"), 128);
        assert_eq!(count(&body, "epoch"), 1);
        assert_eq!(text(&body, "actual_version"), "v0001");
        assert_eq!(count(&body, "blocks"), 2);
        assert_eq!(count(&body, "frames"), 256);
        assert_eq!(count(&body, "xruns"), 4);
        assert_eq!(count(&body, "underruns"), 5);
        assert_eq!(count(&body, "overruns"), 6);
        assert_eq!(count(&body, "recoveries"), 7);
        assert_eq!(count(&body, "faults"), 11, "sink.faults + gate + watchdog");
        assert_eq!(count(&body, "deadline_misses"), 3);
        assert_eq!(count(&body, "clipped_samples"), 9);
        assert_eq!(count(&body, "silence_blocks"), 1);
        assert_eq!(count(&body, "muted_blocks"), 1);
        assert_eq!(count(&body, "safety_mute_blocks"), 1);
        assert_eq!(count(&body, "forced_mute_blocks"), 0);
        assert!(flag(&body, "silent"));
        assert!(flag(&body, "safety_mute"));
        assert!(!flag(&body, "forced_mute"));
        // 安全静音块上 actual mute 为 true（安全静音也是“实际静音”，与控制面 forced mute 分开报）。
        assert!(flag(&body, "mute"));
        assert_eq!(text(&body, "safe_mode"), "normal");
        // 自洽：frames == blocks · chunksize；静音块 = 安全静音块 + forced mute 块。
        assert_eq!(
            count(&body, "frames"),
            count(&body, "blocks") * count(&body, "chunksize")
        );
        assert_eq!(
            count(&body, "silence_blocks"),
            count(&body, "safety_mute_blocks") + count(&body, "forced_mute_blocks")
        );
        // plan_version 变化 = actual_version 变化（legacy re-plan v2）。
        control.publish_plan_version(2);
        assert_eq!(text(&control.status_json(), "actual_version"), "v0002");
        control.mark_stopped();
        let stopped = control.status_json();
        assert_eq!(text(&stopped, "runtime_state"), "Stopped");
        assert!(!flag(&stopped, "actual"));
    }

    /// RT 入口不碰静态元数据锁：持锁期间 begin_block/publish_block 必须正常返回。
    #[test]
    fn rt_path_never_locks_metadata() {
        let control = RuntimeControl::new();
        control.start_run(meta());
        control.mark_running();
        let guard = control.meta.lock().expect("meta");
        assert!(!control.begin_block(0));
        control.publish_block(BlockSnapshot {
            block_seq: 0,
            ..BlockSnapshot::default()
        });
        drop(guard);
        assert_eq!(count(&control.status_json(), "blocks"), 1);
    }

    /// 假块循环 harness：只推进块边界（不打开声卡、不启动真实 RT 线程）。
    fn spawn_block_harness(
        control: Arc<RuntimeControl>,
        stop: Arc<AtomicBool>,
        period: Duration,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut block = 0u64;
            while !stop.load(Ordering::Acquire) {
                let forced = control.begin_block(block);
                control.publish_block(BlockSnapshot {
                    block_seq: block,
                    mute_this_block: forced,
                    forced_mute: forced,
                    ..BlockSnapshot::default()
                });
                block += 1;
                thread::sleep(period);
            }
        })
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// mute 幂等、块边界生效、回执带 actual mute 与生效块号；不改音量。
    #[test]
    fn mute_is_idempotent_and_applies_at_block_boundary() {
        let control = Arc::new(RuntimeControl::new());
        control.start_run(meta());
        control.mark_running();
        // 未运行时不接受写：显式 runtime_not_running（不假装成功）。
        let idle_control = RuntimeControl::new();
        let idle = idle_control.mute_json(Some(true));
        assert!(!flag(&idle, "ok"), "{idle}");
        assert_eq!(text(&idle, "error"), "runtime_not_running");

        let stop = Arc::new(AtomicBool::new(false));
        let harness =
            spawn_block_harness(control.clone(), stop.clone(), Duration::from_micros(100));
        let running = wait_until(Duration::from_millis(500), || {
            count(&control.status_json(), "blocks") > 0
        });
        assert!(running, "假块循环没有推进");

        let before_blocks = count(&control.status_json(), "blocks");
        let first = control.mute_json(Some(true));
        assert!(flag(&first, "ok"), "{first}");
        assert!(flag(&first, "accepted"), "{first}");
        assert!(flag(&first, "requested_mute"), "{first}");
        assert!(
            flag(&first, "mute"),
            "回执必须带生效后的 actual mute：{first}"
        );
        assert!(flag(&first, "forced_mute"), "{first}");
        assert!(!flag(&first, "idempotent"), "{first}");
        assert!(
            first.contains("\"volume_db\":null"),
            "mute 不改音量：{first}"
        );
        let applied_block = count(&first, "applied_block");
        assert!(
            applied_block >= before_blocks,
            "生效块号 {applied_block} 必须 >= 请求时的已完成块数 {before_blocks}"
        );

        // 幂等：重复写同一值返回同一生效块号，不重新生效。
        assert!(wait_until(Duration::from_millis(500), || {
            count(&control.status_json(), "forced_mute_blocks") > 0
        }));
        let second = control.mute_json(Some(true));
        assert!(flag(&second, "ok"), "{second}");
        assert!(flag(&second, "idempotent"), "{second}");
        assert_eq!(count(&second, "applied_block"), applied_block);
        let busy = control.status_json();
        assert!(flag(&busy, "forced_mute"), "{busy}");
        assert!(flag(&busy, "mute"), "{busy}");
        assert_eq!(text(&busy, "runtime_state"), "Running");

        // 解除：新的块边界生效，块号前进。
        let off = control.mute_json(Some(false));
        assert!(flag(&off, "ok"), "{off}");
        assert!(!flag(&off, "idempotent"), "{off}");
        assert!(!flag(&off, "mute"), "{off}");
        assert!(!flag(&off, "forced_mute"), "{off}");
        assert!(
            count(&off, "applied_block") > applied_block,
            "解除静音必须在下一次块边界生效：{off}"
        );

        stop.store(true, Ordering::Release);
        harness.join().expect("harness");
    }

    /// volume：越界显式拒绝（不夹取），合法值明确 unsupported（无音频通路）。
    #[test]
    fn volume_range_is_rejected_explicitly_and_valid_value_is_unsupported() {
        let control = RuntimeControl::new();
        control.start_run(meta());
        control.mark_running();
        let low = control
            .handle("volume", "{\"op\":\"volume\",\"db\":-151}")
            .expect("volume op");
        assert!(!flag(&low, "ok"), "{low}");
        assert_eq!(text(&low, "error"), "volume_out_of_range");
        assert_eq!(json_f64(&low, "requested_db"), Some(-151.0));
        assert_eq!(json_f64(&low, "min_db"), Some(VOLUME_MIN_DB));
        assert_eq!(json_f64(&low, "max_db"), Some(VOLUME_MAX_DB));
        assert!(low.contains("\"volume_db\":null"), "{low}");
        let high = control
            .handle("volume", "{\"op\":\"volume\",\"db\":50.5}")
            .expect("volume op");
        assert_eq!(text(&high, "error"), "volume_out_of_range");
        let missing = control
            .handle("volume", "{\"op\":\"volume\"}")
            .expect("volume op");
        assert_eq!(text(&missing, "error"), "bad_volume");
        let not_a_number = control
            .handle("volume", "{\"op\":\"volume\",\"db\":\"NaN\"}")
            .expect("volume op");
        assert_eq!(text(&not_a_number, "error"), "bad_volume");
        // 边界值合法 → unsupported（没有假装成功）。
        for db in [VOLUME_MIN_DB, VOLUME_MAX_DB, -6.0] {
            let line = format!("{{\"op\":\"volume\",\"db\":{db}}}");
            let body = control.handle("volume", &line).expect("volume op");
            assert!(!flag(&body, "ok"), "{body}");
            assert_eq!(text(&body, "error"), "unsupported");
            assert!(!flag(&body, "supported"));
            assert!(text(&body, "reason").contains("没有音量通路"), "{body}");
            assert!(body.contains("\"alternatives\":["), "{body}");
            assert!(body.contains("\"volume_db\":null"), "{body}");
        }
    }

    /// 未知 op 不归本模块接管（保持校准侧既有 unknown_op 行为）。
    #[test]
    fn unknown_op_is_not_claimed() {
        let control = RuntimeControl::new();
        assert!(control.handle("bogus", "{\"op\":\"bogus\"}").is_none());
        assert!(control.handle("enter", "{\"op\":\"enter\"}").is_none());
        assert!(control.handle("mute", "{\"op\":\"mute\"}").is_some());
        assert!(control.handle("volume", "{\"op\":\"volume\"}").is_some());
    }
}
