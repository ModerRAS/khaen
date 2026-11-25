//! [NONRT] 执行 + `khaen-file-runner-report/v1` 元数据（离线 file runner 的 run 阶段）。
//!
//! 执行语义（对齐 T03 SPEC §3.2/§3.3 与 core-v1 §6.3）：
//!
//! 1. 输入尾部补 `flush_zeros = L_F − 1` 个零，再补零到 block 整数倍（B ∈ {64,128}）；
//! 2. 每个 block 经 T09 `execute_block_scheduled`（固定 4/2 worker 静态 DAG、固定 reduce 顺序）执行
//!    factorized core 图；block 边界由 [-state] 保留因果历史，不丢样/不重算；
//! 3. 拼接后截断到 `L_out = L_in + L_F − 1`（离线语义；identity 图因此 = 输入延迟 D）；
//! 4. 任一 block 失败/迟到/cutoff/非有限输出 -> **整块/整图无效**，绝不发布半图；按保护策略
//!    安全 mute（默认）或返回错误；metadata 如实记录 invalid/exec_error。
//!
//! identity 保护（prepare 阶段已决定，见 `crate::graph`）：`GraphKind::ProtectionIdentityPPost` 时执行
//! G=I 但保留 P_post 的 fallback 图；`ProtectionMute` 时输出全零（长度 = 输入帧数，无尾）。
//!
//! 元数据（`khaen-file-runner-report/v1`）是**确定性** canonical JSON：不含时间戳/耗时，同输入同图
//! 两次运行逐字节一致；包含 input/output SHA-256、graph_id/plan_version/abi_version/precision、
//! compile 摘要、fused 设计摘要、FIR/IR 绑定、identity 保护记录、flush/block/worker 计数、
//! decode 统计与 NOT_RUN 清单（禁止把未测项说成已验证）。

use std::sync::atomic::{AtomicU32, Ordering};

use khaen_core::abi::{TspiBlockAckV1, ABI_VERSION};
use khaen_core::workers::{
    execute_block_scheduled, WorkerPartials, WorkerScheduleStats, WORKER_ACK_FLAG_VALID,
};

use crate::graph::{CompileSummary, ExecutableGraph, GraphKind, PreparedSession};
use crate::json::Json;
use crate::sha256::sha256_hex;
use crate::wav::{self, DecodedAudio};
use crate::{
    RunnerError, RunnerResult, E_FILE_CHANNELS, E_FILE_EXEC, E_FILE_INTERNAL, E_FILE_OUTPUT,
    NOT_VERIFIED, PRECISION_LOCK, REPORT_SCHEMA, RUNNER_NAME, RUNNER_VERSION,
};

/// 离线执行 epoch id（单 epoch；确定性）。
pub const OFFLINE_EPOCH_ID: u64 = 1;
/// worker ack 的有界 spin 预算（离线驱动；与 T09 测试同口径）。
pub const OFFLINE_SPIN_MAX_ITERS: u32 = 1_000_000;

/// 运行选项。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// [TEST] 在第 N 个 block 后注入执行故障（模拟 worker/设备故障；验证安全 mute 路径）。
    pub inject_exec_fault_after_blocks: Option<u64>,
}

/// 执行统计（报告用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionCounts {
    /// 总 block 数（含 flush 块）。
    pub blocks_total: u64,
    /// flush 块数（输入耗尽的零块）。
    pub flush_blocks: u64,
    /// 输入帧数。
    pub input_frames: u64,
    /// 输出帧数（= L_out 或 mute 帧数）。
    pub output_frames: u64,
    /// 执行的 DAG tile 总数。
    pub worker_tiles: u64,
    /// invalid block 数（DAG 内部判定）。
    pub invalid_blocks: u64,
    /// cutoff block 数。
    pub cutoff_blocks: u64,
    /// 迟到 worker 计数。
    pub late_workers: u64,
    /// 输出非有限样本数（应为 0）。
    pub nonfinite_out: u64,
}

/// 单次运行结果。
#[derive(Clone, Debug)]
pub struct RunResult {
    /// 实际图种类。
    pub graph_kind: GraphKind,
    /// 是否处于保护路径。
    pub protected: bool,
    /// 输出 planar f32（每容器通道一个 Vec，长度 = output_frames）。
    pub output: Vec<Vec<f32>>,
    /// 输出帧数。
    pub output_frames: usize,
    /// 容器通道数。
    pub container_channels: usize,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// 执行统计。
    pub counts: ExecutionCounts,
    /// 执行故障（保护/mute 时的原始错误码与消息）。
    pub exec_error: Option<String>,
    /// 运行状态：`ok`/`protected`/`muted`。
    pub status: String,
    /// 元数据 canonical JSON（`khaen-file-runner-report/v1`）。
    pub metadata_json: String,
}

impl RunResult {
    /// 把输出编码为 f32 WAV（interleaved；确定性）。
    pub fn output_wav_f32(&self) -> RunnerResult<Vec<u8>> {
        let channels = match u16::try_from(self.container_channels) {
            Ok(value) => value,
            Err(_) => return Err(RunnerError::new(E_FILE_OUTPUT, "too many output channels")),
        };
        let mut planar = Vec::with_capacity(self.container_channels * self.output_frames);
        for channel in 0..self.container_channels {
            planar.extend_from_slice(&self.output[channel]);
        }
        wav::encode_wav_f32(&planar, channels, self.sample_rate_hz)
    }
}

/// 执行已 prepare 的 session（不写文件、不做 ALSA；纯内存）。
pub fn run(prepared: &PreparedSession, options: &RunOptions) -> RunnerResult<RunResult> {
    let (outcome, exec_error) = match execute(prepared, options) {
        Ok(outcome) => (outcome, None),
        Err(error) => {
            if !prepared.session.protection.mute_on_exec_fault {
                return Err(error);
            }
            (
                mute_outcome(prepared),
                Some(format!("{}: {}", error.code(), error.message())),
            )
        }
    };
    let mut counts = outcome.counts;
    let mut status = if exec_error.is_some() {
        "muted".to_string()
    } else if prepared.is_protected() {
        "protected".to_string()
    } else {
        "ok".to_string()
    };
    let mut output = outcome.output;
    // 非有限输出：按保护策略 mute（绝不把非有限样本交给 host/设备）。
    let nonfinite = count_nonfinite(&output);
    if nonfinite > 0 {
        counts.nonfinite_out = nonfinite;
        if !prepared.session.protection.mute_on_exec_fault {
            return Err(RunnerError::new(
                E_FILE_EXEC,
                format!("{} non-finite output samples", nonfinite),
            ));
        }
        output = mute_planar(
            prepared.session.audio.container_channels as usize,
            outcome.output_frames,
        );
        status = "muted".to_string();
    }
    let metadata_json = report_json(prepared, &output, counts, exec_error.as_deref(), &status);
    Ok(RunResult {
        graph_kind: prepared.graph_kind,
        protected: prepared.is_protected(),
        output,
        output_frames: outcome.output_frames,
        container_channels: prepared.session.audio.container_channels as usize,
        sample_rate_hz: prepared.session.audio.sample_rate_hz,
        counts,
        exec_error,
        status,
        metadata_json,
    })
}

/// 便捷：解析 session + prepare + run（一次调用）。
pub fn run_session(
    session_text: &str,
    source: &dyn crate::session::SourceProvider,
    options: &RunOptions,
) -> RunnerResult<RunResult> {
    let prepared = crate::graph::prepare_from_json(session_text, source)?;
    run(&prepared, options)
}

struct ExecutionOutcome {
    output: Vec<Vec<f32>>,
    output_frames: usize,
    counts: ExecutionCounts,
}

fn execute(prepared: &PreparedSession, options: &RunOptions) -> RunnerResult<ExecutionOutcome> {
    if prepared.graph_kind == GraphKind::ProtectionMute {
        let frames = prepared.input.frames();
        return Ok(ExecutionOutcome {
            output: mute_planar(prepared.session.audio.container_channels as usize, frames),
            output_frames: frames,
            counts: ExecutionCounts {
                input_frames: frames as u64,
                output_frames: frames as u64,
                ..ExecutionCounts::default()
            },
        });
    }
    let graph = prepared
        .graph
        .as_ref()
        .ok_or_else(|| RunnerError::new(E_FILE_INTERNAL, "prepared graph missing"))?;
    execute_graph(graph, &prepared.input, options)
}

fn execute_graph(
    graph: &ExecutableGraph,
    input: &DecodedAudio,
    options: &RunOptions,
) -> RunnerResult<ExecutionOutcome> {
    let block = graph.block_size as usize;
    if block == 0 {
        return Err(RunnerError::new(E_FILE_INTERNAL, "block_size is zero"));
    }
    let containers = graph.container_channels();
    let n_src = graph.topology.n_src as usize;
    if input.channels() != n_src {
        return Err(RunnerError::new(
            E_FILE_CHANNELS,
            format!(
                "input channels {} != graph sources {}",
                input.channels(),
                n_src
            ),
        ));
    }
    // 输入切分与 host 共用 source（WBS T14 固定 buffer source 接口）。
    let mut source = crate::source::BlockSource::new(input, graph.block_size, graph.flush_zeros)?;
    let input_frames = source.input_frames();
    let total_frames = source.total_frames();
    let blocks_total = source.blocks_total();
    let mut input_block = vec![0.0f32; n_src * block];
    let mut output_block = vec![0.0f32; containers * block];
    let mut state = vec![0.0f32; graph.state_samples()];
    let mut partials = Box::new(WorkerPartials::new());
    let bad_output = AtomicU32::new(0);
    let mut stats = WorkerScheduleStats::default();
    let mut output: Vec<Vec<f32>> = vec![Vec::with_capacity(total_frames); containers];
    let mut counts = ExecutionCounts {
        input_frames: input_frames as u64,
        output_frames: total_frames as u64,
        ..ExecutionCounts::default()
    };
    for seq in 0..blocks_total as u64 {
        if let Some(fault_at) = options.inject_exec_fault_after_blocks {
            if seq == fault_at {
                return Err(RunnerError::new(
                    E_FILE_EXEC,
                    format!("[TEST] injected exec fault before block {}", seq),
                ));
            }
        }
        let chunk = match source.next_block(&mut input_block)? {
            Some(chunk) => chunk,
            None => break,
        };
        let valid_frames = chunk.frames_valid;
        let ack = TspiBlockAckV1 {
            flags: WORKER_ACK_FLAG_VALID,
            ..TspiBlockAckV1::new(
                seq,
                OFFLINE_EPOCH_ID,
                graph.plan_version,
                graph.expected_workers,
            )
        };
        let status = execute_block_scheduled(
            &graph.plan,
            &graph.coeffs(),
            &mut state,
            &input_block,
            block,
            &mut output_block,
            block,
            OFFLINE_EPOCH_ID,
            seq,
            valid_frames,
            &ack,
            &mut partials,
            &bad_output,
            OFFLINE_SPIN_MAX_ITERS,
            &mut stats,
        );
        if !status.is_ok() {
            return Err(RunnerError::new(
                E_FILE_EXEC,
                format!(
                    "core DAG failed at block {} (RtStatus {})",
                    seq,
                    status.code()
                ),
            ));
        }
        if bad_output.load(Ordering::Acquire) != 0 {
            return Err(RunnerError::new(
                E_FILE_EXEC,
                format!("core DAG reported invalid block {}", seq),
            ));
        }
        counts.worker_tiles = counts
            .worker_tiles
            .saturating_add(stats.executed_tiles as u64);
        counts.invalid_blocks = counts
            .invalid_blocks
            .saturating_add(stats.invalid_blocks as u64);
        counts.cutoff_blocks = counts
            .cutoff_blocks
            .saturating_add(stats.cutoff_blocks as u64);
        counts.late_workers = counts
            .late_workers
            .saturating_add(stats.late_workers as u64);
        if chunk.is_flush {
            counts.flush_blocks = counts.flush_blocks.saturating_add(1);
        }
        for channel in 0..containers {
            let start = channel * block;
            output[channel].extend_from_slice(&output_block[start..start + block]);
        }
    }
    for channel in output.iter_mut() {
        channel.truncate(total_frames);
    }
    counts.blocks_total = blocks_total as u64;
    Ok(ExecutionOutcome {
        output,
        output_frames: total_frames,
        counts,
    })
}

fn mute_outcome(prepared: &PreparedSession) -> ExecutionOutcome {
    let containers = prepared.session.audio.container_channels as usize;
    let input_frames = prepared.input.frames();
    let frames = prepared
        .graph
        .as_ref()
        .map(|graph| input_frames + graph.flush_zeros as usize)
        .unwrap_or(input_frames);
    ExecutionOutcome {
        output: mute_planar(containers, frames),
        output_frames: frames,
        counts: ExecutionCounts {
            input_frames: input_frames as u64,
            output_frames: frames as u64,
            ..ExecutionCounts::default()
        },
    }
}

fn mute_planar(containers: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut output = Vec::with_capacity(containers);
    for _ in 0..containers {
        output.push(vec![0.0f32; frames]);
    }
    output
}

fn count_nonfinite(output: &[Vec<f32>]) -> u64 {
    let mut count = 0u64;
    for channel in output {
        for &value in channel {
            if !value.is_finite() {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

fn output_sha256(output: &[Vec<f32>]) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    for channel in output {
        for value in channel {
            buffer.extend_from_slice(&value.to_bits().to_le_bytes());
        }
    }
    sha256_hex(&buffer)
}

fn output_stats(output: &[Vec<f32>]) -> (f64, f64) {
    let mut peak = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut count = 0u64;
    for channel in output {
        for &value in channel {
            let magnitude = (value as f64).abs();
            peak = peak.max(magnitude);
            sum_sq += (value as f64) * (value as f64);
            count = count.saturating_add(1);
        }
    }
    let rms = if count == 0 {
        0.0
    } else {
        (sum_sq / count as f64).sqrt()
    };
    (peak, rms)
}

fn compile_summary_json(summary: &CompileSummary) -> Json {
    Json::from_entries(vec![
        ("chosen", Json::String(summary.chosen.clone())),
        ("reason", Json::String(summary.reason.clone())),
        ("fused_feasible", Json::Bool(summary.fused_feasible)),
        ("fused_built", Json::Bool(summary.fused_built)),
        ("full_path_len", Json::Number(summary.full_path_len as f64)),
        ("fused_len_gp", Json::Number(summary.fused_len_gp as f64)),
        ("route_count", Json::Number(summary.route_count as f64)),
        (
            "factorized_macs_per_sample",
            Json::Number(summary.factorized_macs_per_sample as f64),
        ),
        (
            "fused_macs_per_sample",
            Json::Number(summary.fused_macs_per_sample as f64),
        ),
        (
            "saved_macs_per_sample",
            Json::Number(summary.saved_macs_per_sample as f64),
        ),
        ("taps_limit", Json::Number(summary.taps_limit as f64)),
        (
            "extended_taps_admitted",
            Json::Bool(summary.extended_taps_admitted),
        ),
    ])
}

fn ir_binding_json(prepared: &PreparedSession) -> Json {
    match prepared.ir_binding.as_ref() {
        None => Json::Null,
        Some(binding) => {
            let mut object = match crate::ir::binding_json(&binding.draft, &binding.stale) {
                Json::Object(map) => map,
                other => return other,
            };
            object.insert("applied".to_string(), Json::Bool(binding.applied));
            Json::Object(object)
        }
    }
}

fn report_json(
    prepared: &PreparedSession,
    output: &[Vec<f32>],
    counts: ExecutionCounts,
    exec_error: Option<&str>,
    status: &str,
) -> String {
    let session = &prepared.session;
    let (peak, rms) = output_stats(output);
    let fir_json = crate::graph::fir_bindings_json(prepared);
    let mut entries: Vec<(&str, Json)> = vec![
        ("schema", Json::String(REPORT_SCHEMA.to_string())),
        (
            "runner",
            Json::from_entries(vec![
                ("name", Json::String(RUNNER_NAME.to_string())),
                ("version", Json::String(RUNNER_VERSION.to_string())),
            ]),
        ),
        ("status", Json::String(status.to_string())),
        ("precision", Json::String(PRECISION_LOCK.to_string())),
        ("graph_id", Json::String(prepared.graph_id.clone())),
        (
            "graph_hash_algo",
            Json::String(crate::GRAPH_HASH_ALGO.to_string()),
        ),
        (
            "graph_kind",
            Json::String(prepared.graph_kind.as_str().to_string()),
        ),
        (
            "fused_digest",
            match &prepared.fused_digest {
                Some(digest) => Json::String(digest.clone()),
                None => Json::Null,
            },
        ),
        (
            "engine",
            Json::from_entries(vec![
                ("abi_version", Json::Number(ABI_VERSION as f64)),
                (
                    "plan_version",
                    Json::Number(session.engine.plan_version as f64),
                ),
                ("block_size", Json::Number(session.engine.block_size as f64)),
                (
                    "expected_workers",
                    Json::Number(session.engine.expected_workers as f64),
                ),
                ("profile", Json::String(session.engine.profile.clone())),
                (
                    "sample_rate_hz",
                    Json::Number(session.audio.sample_rate_hz as f64),
                ),
                (
                    "container_channels",
                    Json::Number(session.audio.container_channels as f64),
                ),
                (
                    "input_channels",
                    Json::Number(session.audio.input_channels as f64),
                ),
                ("layout_id", Json::String(session.audio.layout_id.clone())),
                ("d_samples", Json::Number(session.d_samples as f64)),
            ]),
        ),
        (
            "input",
            Json::from_entries(vec![
                ("file", Json::String(session.audio.input_file.clone())),
                ("sha256", Json::String(prepared.input_file_sha256.clone())),
                ("bytes", Json::Number(prepared.input_file_bytes as f64)),
                (
                    "channels",
                    Json::Number(prepared.input.info.channels as f64),
                ),
                ("frames", Json::Number(prepared.input.info.frames as f64)),
                (
                    "sample_rate_hz",
                    Json::Number(prepared.input.info.sample_rate as f64),
                ),
                (
                    "format",
                    Json::String(prepared.input.info.format.as_str().to_string()),
                ),
                (
                    "decode",
                    Json::from_entries(vec![
                        (
                            "nonfinite_in",
                            Json::Number(prepared.input.stats.nonfinite_in as f64),
                        ),
                        ("clipped", Json::Number(prepared.input.stats.clipped as f64)),
                        (
                            "nonfinite_out",
                            Json::Number(prepared.input.stats.nonfinite_out as f64),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "output",
            Json::from_entries(vec![
                ("format", Json::String("wav_f32".to_string())),
                ("channels", Json::Number(output.len() as f64)),
                ("frames", Json::Number(counts.output_frames as f64)),
                ("sha256", Json::String(output_sha256(output))),
                ("peak_abs", Json::Number(peak)),
                ("rms", Json::Number(rms)),
            ]),
        ),
        (
            "lengths",
            match prepared.graph.as_ref() {
                Some(graph) => Json::from_entries(vec![
                    ("l_a", Json::Number(graph.l_a as f64)),
                    ("l_g", Json::Number(graph.l_g as f64)),
                    ("l_p", Json::Number(graph.l_p as f64)),
                    ("l_total", Json::Number(graph.l_total as f64)),
                    ("flush_zeros", Json::Number(graph.flush_zeros as f64)),
                ]),
                None => Json::Null,
            },
        ),
        (
            "compile",
            match &prepared.compile {
                Some(summary) => compile_summary_json(summary),
                None => Json::Null,
            },
        ),
        ("ir", ir_binding_json(prepared)),
        ("fir", Json::Array(fir_json)),
        (
            "protection",
            match &prepared.protection {
                Some(protection) => Json::from_entries(vec![
                    ("engaged", Json::Bool(true)),
                    ("reason", Json::String(protection.reason.clone())),
                    ("fallback", Json::String(protection.fallback.clone())),
                    ("graph_kind", Json::String(protection.graph_kind.clone())),
                ]),
                None => Json::from_entries(vec![
                    ("engaged", Json::Bool(false)),
                    ("reason", Json::Null),
                    (
                        "fallback",
                        Json::String(session.protection.fallback.clone()),
                    ),
                    ("graph_kind", Json::String("normal".to_string())),
                ]),
            },
        ),
        (
            "counts",
            Json::from_entries(vec![
                ("blocks_total", Json::Number(counts.blocks_total as f64)),
                ("flush_blocks", Json::Number(counts.flush_blocks as f64)),
                ("input_frames", Json::Number(counts.input_frames as f64)),
                ("output_frames", Json::Number(counts.output_frames as f64)),
                ("worker_tiles", Json::Number(counts.worker_tiles as f64)),
                ("invalid_blocks", Json::Number(counts.invalid_blocks as f64)),
                ("cutoff_blocks", Json::Number(counts.cutoff_blocks as f64)),
                ("late_workers", Json::Number(counts.late_workers as f64)),
                ("nonfinite_out", Json::Number(counts.nonfinite_out as f64)),
            ]),
        ),
        (
            "exec_error",
            match exec_error {
                Some(text) => Json::String(text.to_string()),
                None => Json::Null,
            },
        ),
        (
            "warnings",
            Json::Array(
                prepared
                    .warnings
                    .iter()
                    .map(|text| Json::String(text.clone()))
                    .collect(),
            ),
        ),
        (
            "not_verified",
            Json::Array(
                NOT_VERIFIED
                    .iter()
                    .map(|text| Json::String((*text).to_string()))
                    .collect(),
            ),
        ),
    ];
    entries.sort_by(|a, b| a.0.cmp(b.0));
    Json::from_entries(entries).to_canonical()
}
