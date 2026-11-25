//! T26 host **file 端到端**：WAV -> `tspi-file-runner` session/plan（T14）-> T09 4-worker
//! 静态 DAG（T14 prepared graph 自带 `WorkerDagPlan`）-> WAV。
//!
//! 本模块是 T26 入口集成的一部分：rt-host crate 不能依赖 file-runner（file-runner 已依赖
//! rt-host，反向依赖会成环），因此 file 路径的命令入口放在 `rt-check`（它同时依赖二者），
//! 由 `rt-check host-file` 真实执行；fake 设备路径见 `tspi-rt-host` bin 的
//! `fake-io`/`selftest`。两条路径共用 T14 `BlockSource` 的输入切分语义（T14 source.rs）。
//!
//! 明确未做：真实 ALSA/板端播放、RT/soak、控制面 REST/WS。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tspi_dsp_core::workers::WorkerDagPlan;
use tspi_file_runner::fir::{build_fir_manifest, magnitude_db_at, FirManifestSpec};
use tspi_file_runner::graph;
use tspi_file_runner::json::Json;
use tspi_file_runner::runner::{self, RunOptions};
use tspi_file_runner::session::FsSource;
use tspi_file_runner::sha256::sha256_hex;
use tspi_file_runner::source::BlockSource;
use tspi_file_runner::wav;

/// host-file 采样率（v1 runtime 48k；96k 由 session 自带，不改这里）。
const FS: u32 = 48_000;
/// host-file 生产块长（T05 Q-11 runtime 仅 64/128）。
const BLOCK: u16 = 128;
/// 4 核（T26 生产 worker 数；2 核是独立降级 profile）。
const WORKERS: u8 = 4;

/// 2.0 -> 4.1.2 角色：FL/FR 有源，其余零填；scatter 按 WBS §3 唯一映射（SUB1=7、ch6 空）。
const ROLES: [(&str, Option<u16>, u16); 6] = [
    ("FL", Some(0), 0),
    ("FR", Some(1), 1),
    ("C", None, 2),
    ("SL", None, 3),
    ("SR", None, 4),
    ("SUB1", None, 7),
];
/// 低带 participants（dense 顺序）。
const PARTICIPANTS: [&str; 3] = ["FL", "FR", "SUB1"];

/// host-file 运行结果（打印/写入 report）。
#[derive(Clone, Debug)]
pub struct HostFileOutcome {
    /// 输入帧数。
    pub input_frames: usize,
    /// 输出帧数。
    pub output_frames: usize,
    /// 执行块数（含 flush 块）。
    pub blocks: u64,
    /// worker 数（来自 prepared graph 的 T09 DAG）。
    pub workers: u8,
    /// block 大小。
    pub block_size: u16,
    /// 运行状态（ok/protected/muted）。
    pub status: String,
    /// 图种类。
    pub graph_kind: String,
    /// 输出 WAV 路径。
    pub output_path: String,
    /// 输出 WAV 字节数。
    pub output_bytes: usize,
    /// 独立解码复核的帧数（= 输出帧数）。
    pub decoded_frames: usize,
    /// T14 报告（`tspi-file-runner-report/v1` canonical JSON）。
    pub runner_report: String,
}

impl HostFileOutcome {
    /// 打印用摘要 JSON（不含 T14 完整报告）。
    pub fn summary_json(&self) -> String {
        format!(
            "{{\"status\":\"ok\",\"mode\":\"host-file\",\"input_frames\":{},\"output_frames\":{},\"blocks\":{},\"workers\":{},\"block_size\":{},\"run_status\":\"{}\",\"graph_kind\":\"{}\",\"output\":\"{}\",\"output_bytes\":{},\"decoded_frames\":{}}}",
            self.input_frames,
            self.output_frames,
            self.blocks,
            self.workers,
            self.block_size,
            self.status,
            self.graph_kind,
            self.output_path.replace('\\', "/"),
            self.output_bytes,
            self.decoded_frames
        )
    }
}

fn raw_f32le(coeffs: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(coeffs.len() * 4);
    for value in coeffs {
        out.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    out
}

fn endpoint_measured(freq_hz: f64, mag_db: f64) -> Json {
    Json::from_entries(vec![
        ("status", Json::String("measured".to_string())),
        ("freq_hz", Json::Number(freq_hz)),
        ("mag_db", Json::Number(mag_db)),
        ("phase_deg", Json::Number(0.0)),
    ])
}

/// 构造 `tspi-fir/v1` manifest（nos = 纯延迟，reference_gain_db=0；与 T14 fixture 同口径）。
fn fir_manifest(coeffs: &[f32], file: &str) -> Result<Json, String> {
    let bytes = raw_f32le(coeffs);
    let sha = sha256_hex(&bytes);
    let taps = coeffs.len() as u32;
    let manifest = build_fir_manifest(&FirManifestSpec {
        family: "nos",
        fs: FS,
        taps,
        delay_samples: 0.0,
        tool_name: "t26-host-file",
        tool_version: "1.0.0",
        coefficients_file: file,
        coefficients_format: "raw_f32le",
        coefficients_bytes: bytes.len() as u64,
        coefficients_sha256: &sha,
        profile: "regular",
        route_taps_cap: 4096,
        ram_bytes_f32: taps as u64 * 4,
        fits_budget: true,
        reference_hz: 1000.0,
        reference_gain_db: 0.0,
        measured_gain_db: None,
        endpoint_f20: endpoint_measured(20.0, 0.0),
        endpoint_f20k: endpoint_measured(20_000.0, 0.0),
        design: None,
        source: None,
        verification_method: "t26-host-file/identity",
        warnings: Vec::new(),
    })
    .map_err(|err| format!("{}: {}", err.code(), err.message()))?;
    let _ = magnitude_db_at(coeffs, FS as f64, 1000.0);
    Ok(manifest)
}

fn write_bytes(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(name);
    fs::write(&path, bytes).map_err(|err| format!("write {}: {}", path.display(), err))
}

/// 生成确定性 2ch 输入 WAV + identity 系数 blob + session manifest（供 e2e 与快速回归）。
pub fn make_sample(dir: &Path, frames: usize) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| format!("mkdir {}: {}", dir.display(), err))?;
    if frames == 0 {
        return Err("frames must be > 0".to_string());
    }
    // 确定性周期斜坡（振幅 <= 0.5，不触发 decode clip）。
    let mut planar = Vec::with_capacity(2 * frames);
    for frame in 0..frames {
        planar.push((frame % 64) as f32 / 64.0 - 0.5);
    }
    for frame in 0..frames {
        planar.push(0.5 - (frame % 32) as f32 / 64.0);
    }
    let wav_bytes = wav::encode_wav_f32(&planar, 2, FS)
        .map_err(|err| format!("{}: {}", err.code(), err.message()))?;
    write_bytes(dir, "input.wav", &wav_bytes)?;

    let identity = [1.0f32];
    // A：两个源各一条 identity FIR。
    write_bytes(dir, "a0.raw", &raw_f32le(&identity))?;
    write_bytes(dir, "a1.raw", &raw_f32le(&identity))?;
    let a_manifest0 = fir_manifest(&identity, "a0.raw")?;
    let a_manifest1 = fir_manifest(&identity, "a1.raw")?;
    // P：每个角色一条 identity FIR（含零填角色，保持保护基线）。
    let mut p_entries = Vec::new();
    for (name, _, _) in ROLES {
        let file = format!("p_{}.raw", name);
        write_bytes(dir, &file, &raw_f32le(&identity))?;
        let manifest = fir_manifest(&identity, &file)?;
        p_entries.push(Json::from_entries(vec![
            ("role", Json::String(name.to_string())),
            ("file", Json::String(file)),
            ("manifest", manifest),
        ]));
    }
    let role_entries: Vec<Json> = ROLES
        .iter()
        .enumerate()
        .map(|(index, (name, src, scatter))| {
            Json::from_entries(vec![
                ("name", Json::String((*name).to_string())),
                ("index", Json::Number(index as f64)),
                (
                    "src",
                    match src {
                        Some(value) => Json::Number(*value as f64),
                        None => Json::Null,
                    },
                ),
                ("scatter", Json::Number(*scatter as f64)),
            ])
        })
        .collect();
    let session = Json::from_entries(vec![
        ("schema", Json::String("tspi-file-runner/v1".to_string())),
        ("precision", Json::String("f32-strict".to_string())),
        (
            "audio",
            Json::from_entries(vec![
                ("sample_rate_hz", Json::Number(FS as f64)),
                ("container_channels", Json::Number(8.0)),
                ("input_channels", Json::Number(2.0)),
                ("layout_id", Json::String("4.1.2".to_string())),
                (
                    "input",
                    Json::from_entries(vec![("file", Json::String("input.wav".to_string()))]),
                ),
            ]),
        ),
        (
            "engine",
            Json::from_entries(vec![
                ("abi_version", Json::Number(1.0)),
                ("plan_version", Json::Number(1.0)),
                ("block_size", Json::Number(BLOCK as f64)),
                ("expected_workers", Json::Number(WORKERS as f64)),
                ("profile", Json::String("regular".to_string())),
                ("max_taps", Json::Number(4096.0)),
                ("extended_taps_admitted", Json::Bool(false)),
            ]),
        ),
        ("roles", Json::Array(role_entries)),
        (
            "participants",
            Json::Array(
                PARTICIPANTS
                    .iter()
                    .map(|name| Json::String((*name).to_string()))
                    .collect(),
            ),
        ),
        ("d_samples", Json::Number(0.0)),
        (
            "coefficients",
            Json::from_entries(vec![
                (
                    "a",
                    Json::Array(vec![
                        Json::from_entries(vec![
                            ("src", Json::Number(0.0)),
                            ("file", Json::String("a0.raw".to_string())),
                            ("manifest", a_manifest0),
                        ]),
                        Json::from_entries(vec![
                            ("src", Json::Number(1.0)),
                            ("file", Json::String("a1.raw".to_string())),
                            ("manifest", a_manifest1),
                        ]),
                    ]),
                ),
                ("g", Json::Array(Vec::new())),
                ("p", Json::Array(p_entries)),
            ]),
        ),
        (
            "output",
            Json::from_entries(vec![("format", Json::String("wav_f32".to_string()))]),
        ),
    ]);
    let text = session.to_canonical();
    write_bytes(dir, "session.json", text.as_bytes())
}

/// 真实 file 端到端：读 session（blob 由 FsSource 从同目录解析）-> prepare -> 执行 -> 写 WAV。
pub fn run_session(
    dir: &Path,
    session_name: &str,
    output_path: &Path,
) -> Result<HostFileOutcome, String> {
    let session_path = dir.join(session_name);
    let text = fs::read_to_string(&session_path)
        .map_err(|err| format!("read {}: {}", session_path.display(), err))?;
    let source = FsSource::new(dir);
    let prepared = graph::prepare_from_json(&text, &source)
        .map_err(|err| format!("prepare: {}: {}", err.code(), err.message()))?;
    let (block_size, workers, graph_kind) = match prepared.graph.as_ref() {
        Some(graph) => (
            graph.block_size,
            graph.expected_workers,
            format!("{:?}", prepared.graph_kind),
        ),
        None => {
            return Err(
                "prepared session has no executable graph (protection/mute path)".to_string(),
            )
        }
    };
    if workers != WORKERS {
        return Err(format!("expected {WORKERS} workers, got {workers}"));
    }
    // T09 DAG 必须与 prepared graph 一致（4-core 接线证据；再 build 一次校验）。
    let plan = WorkerDagPlan::build(
        &prepared.graph.as_ref().expect("graph checked").topology,
        block_size,
        workers,
        1,
    )
    .map_err(|status| format!("WorkerDagPlan::build: {}", status.code()))?;
    let blocks = BlockSource::from_prepared(&prepared)
        .map_err(|err| format!("block source: {}: {}", err.code(), err.message()))?
        .blocks_total();
    let result = runner::run(&prepared, &RunOptions::default())
        .map_err(|err| format!("run: {}: {}", err.code(), err.message()))?;
    let wav_bytes = result
        .output_wav_f32()
        .map_err(|err| format!("encode output: {}: {}", err.code(), err.message()))?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("mkdir {}: {}", parent.display(), err))?;
    }
    fs::write(output_path, &wav_bytes)
        .map_err(|err| format!("write {}: {}", output_path.display(), err))?;
    let decoded = wav::decode_wav(&wav_bytes)
        .map_err(|err| format!("decode output: {}: {}", err.code(), err.message()))?;
    let input_frames = prepared.input.frames();
    let out = HostFileOutcome {
        input_frames,
        output_frames: result.output_frames,
        blocks,
        workers: plan.expected_workers,
        block_size: plan.block_size,
        status: result.status.clone(),
        graph_kind,
        output_path: output_path.to_string_lossy().to_string(),
        output_bytes: wav_bytes.len(),
        decoded_frames: decoded.frames(),
        runner_report: result.metadata_json.clone(),
    };
    if out.decoded_frames != out.output_frames {
        return Err(format!(
            "decoded frames {} != output frames {}",
            out.decoded_frames, out.output_frames
        ));
    }
    Ok(out)
}

fn write_report(path: &Path, outcome: &HostFileOutcome) -> Result<(), String> {
    let text = format!(
        "{{\"schema\":\"tspi-host-file-report/v1\",\"host\":{},\"runner\":{}}}",
        outcome.summary_json(),
        outcome.runner_report
    );
    fs::write(path, text).map_err(|err| format!("write {}: {}", path.display(), err))
}

fn usage() {
    eprintln!(
        "usage: rt-check host-file [--work DIR [--frames N]] | [--session FILE --output FILE [--report FILE]]"
    );
}

struct Config {
    work: Option<PathBuf>,
    frames: usize,
    session: Option<PathBuf>,
    output: Option<PathBuf>,
    report: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Config, ExitCode> {
    let mut config = Config {
        work: None,
        frames: 512,
        session: None,
        output: None,
        report: None,
    };
    let mut index = 0usize;
    while index < args.len() {
        let take = |offset: usize| -> Result<&str, ExitCode> {
            args.get(offset).map(String::as_str).ok_or_else(|| {
                eprintln!("rt-check host-file: 缺少参数值: {}", args[index]);
                ExitCode::from(2)
            })
        };
        match args[index].as_str() {
            "--work" => {
                config.work = Some(PathBuf::from(take(index + 1)?));
                index += 2;
            }
            "--frames" => {
                let value = take(index + 1)?;
                config.frames = value.parse().map_err(|_| {
                    eprintln!("rt-check host-file: --frames 不是整数: {value}");
                    ExitCode::from(2)
                })?;
                index += 2;
            }
            "--session" => {
                config.session = Some(PathBuf::from(take(index + 1)?));
                index += 2;
            }
            "--output" => {
                config.output = Some(PathBuf::from(take(index + 1)?));
                index += 2;
            }
            "--report" => {
                config.report = Some(PathBuf::from(take(index + 1)?));
                index += 2;
            }
            other => {
                eprintln!("rt-check host-file: 未知参数: {other}");
                usage();
                return Err(ExitCode::from(2));
            }
        }
    }
    Ok(config)
}

/// `rt-check host-file` 入口（返回进程退出码）。
pub fn run_cli(args: &[String]) -> ExitCode {
    let config = match parse(args) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let outcome = match (
        config.work.as_deref(),
        config.session.as_deref(),
        config.output.as_deref(),
    ) {
        (Some(work), _, _) => {
            let dir = work.to_path_buf();
            let result = make_sample(&dir, config.frames)
                .and_then(|_| run_session(&dir, "session.json", &dir.join("output.wav")));
            result
        }
        (None, Some(session), Some(output)) => {
            let dir = session.parent().unwrap_or_else(|| Path::new("."));
            let name = session
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_else(|| "session.json".to_string());
            run_session(dir, &name, output)
        }
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };
    match outcome {
        Ok(outcome) => {
            let summary = outcome.summary_json();
            if let Some(report) = config.report.as_deref() {
                if let Err(err) = write_report(report, &outcome) {
                    eprintln!("FAIL host-file: {err}");
                    return ExitCode::from(2);
                }
            }
            if let Some(work) = config.work.as_deref() {
                let report = work.join("host-file-report.json");
                if let Err(err) = write_report(&report, &outcome) {
                    eprintln!("FAIL host-file: {err}");
                    return ExitCode::from(2);
                }
            }
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("FAIL host-file: {err}");
            ExitCode::from(1)
        }
    }
}
