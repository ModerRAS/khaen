// T76：remeasure/holdout 播放走 plan-arena host（stereo 2.0 单点）——离线单测。
//
// 注册方式：crates/calibration/src/stage_backend.rs 末尾
//   #[cfg(test)] mod t76_tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/T76/host_plan_remeasure.rs")); }
// 运行：cargo test -p khaen-calibration t76_
//
// 全部离线、不打开声卡：
// - ToolsEngine 用 rustc 现场编译的 stub 可执行文件模拟 arecord/aplay/khaen-rt-host
//   （Windows/Linux 通用：直接 spawn PE/ELF，不依赖 shell 解释器；stub 把 argv/config 写 witness 文件）；
// - 端到端 job 用 FakeEngine + RecordingEngine（合成测量）验证 sweep=Aplay / 二次=HostPlan；
// - 声学效果 NOT_RUN（需出声窗口 + 用户同意占用 MOTU）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::*;
use crate::job::{
    ExitDisposition, JobKind, JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobState,
    SafeOutputController, SafetyError,
};
use crate::sweep::SweepOutput;

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let id = UNIQUE.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("khaen-t76-{tag}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn graph_id_hex(seed: u8) -> String {
    crate::compile_apply::hex32(&[seed; 32])
}

fn plan_bytes_for(
    seed: u8,
    sample_rate: u32,
    block: u16,
    n_in: u16,
    n_out: u16,
    workers: u8,
) -> Vec<u8> {
    let mut bytes = vec![0u8; 256];
    bytes[0..4].copy_from_slice(&khaen_core::abi::KHAEN_PLAN_MAGIC.to_le_bytes());
    bytes[4..6].copy_from_slice(&khaen_core::abi::KHAEN_PLAN_ABI_V2.to_le_bytes());
    bytes[6..8].copy_from_slice(&128u16.to_le_bytes());
    bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&sample_rate.to_le_bytes());
    bytes[16..18].copy_from_slice(&block.to_le_bytes());
    bytes[18..20].copy_from_slice(&n_in.to_le_bytes());
    bytes[20..22].copy_from_slice(&n_out.to_le_bytes());
    bytes[32] = workers;
    bytes[34] = 0;
    bytes[36..68].copy_from_slice(&[seed; 32]);
    bytes
}

fn write_compile_artifacts(
    root: &Path,
    seed: u8,
    sample_rate: u32,
    block: u16,
    n_in: u16,
    n_out: u16,
    workers: u8,
) -> Vec<u8> {
    let compile = root.join("compile");
    std::fs::create_dir_all(&compile).expect("create compile dir");
    let plan = plan_bytes_for(seed, sample_rate, block, n_in, n_out, workers);
    std::fs::write(compile.join("plan.bin"), &plan).expect("write plan.bin");
    let manifest = format!(
        "{{\"plan_sha256\":\"{}\",\"host_graph_sha256\":\"{}\"}}",
        crate::job::sha256_hex(&plan),
        crate::compile_apply::hex32(&[seed; 32])
    );
    std::fs::write(compile.join("compile-report.json"), manifest).expect("write compile-report");
    plan
}

fn default_host_config() -> RemeasureHostConfig {
    RemeasureHostConfig {
        host_bin: PathBuf::from("khaen-rt-host"),
        ..RemeasureHostConfig::default()
    }
}

fn device_config(playback_channels: u16) -> StageDeviceConfig {
    StageDeviceConfig {
        playback_device: "hw:UltraLitemk5,0".to_string(),
        playback_channels,
        capture_device: "hw:0,0".to_string(),
        capture_channels: 1,
        capture_channel: None,
        sample_rate: 48_000,
        block_frames: 128,
        buffer_frames: 512,
        settle_ms: 10,
        sample_format: khaen_core::abi::sample_format::S24_3LE_INTERLEAVED,
    }
}

fn stage_config(playback_channels: u16) -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Tools;
    config.devices = device_config(playback_channels);
    config.mic = Some(StageBackendConfig::umik1_declaration(
        "hw:0,0",
        "718-7696",
        "umik-90deg.txt",
        "0123456789abcdef".repeat(4),
        "90deg",
    ));
    config.sweep = StageSweepConfig::smoke();
    config.outputs = vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)];
    config.session_label = "t76".to_string();
    config.graph_id = graph_id_hex(0x5a);
    config.epoch_id = 1;
    config
}

// ---------------------------------------------------------------------------
// rustc 现场编译的 stub 可执行文件（arecord / aplay / khaen-rt-host）
// ---------------------------------------------------------------------------

const STUB_SOURCE: &str = r#"
use std::io::{Read, Write};

fn parse_arg(args: &[String], flag: &str) -> Option<usize> {
    args.iter()
        .position(|value| value == flag)
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
}

fn main() {
    let exe = std::env::current_exe().unwrap_or_default();
    let stem = exe
        .file_stem()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let witness = exe.with_extension("witness");
    let mut log = String::new();
    log.push_str(&format!("exe={}\n", exe.display()));
    for arg in &args {
        log.push_str(&format!("arg={}\n", arg));
    }
    let _ = std::fs::write(&witness, log);
    if stem.contains("arecord") {
        let channels = parse_arg(&args, "-c").unwrap_or(1);
        let block = parse_arg(&args, "--period-size").unwrap_or(128);
        let blocks: usize = std::env::var("KHAEN_T76_ARECORD_BLOCKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(300);
        let buffer = vec![0u8; block * channels * 3];
        let mut stdout = std::io::stdout();
        for _ in 0..blocks {
            if stdout.write_all(&buffer).is_err() {
                break;
            }
            let _ = stdout.flush();
        }
        std::process::exit(0);
    }
    if stem.contains("aplay") {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        std::process::exit(0);
    }
    // khaen-rt-host stub：把 --config 原文抄到 exe.config，写 witness，按需注入失败。
    if let Some(index) = args.iter().position(|value| value == "--config") {
        if let Some(config) = args.get(index + 1) {
            let _ = std::fs::copy(config, exe.with_extension("config"));
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(120));
    if exe.with_extension("fail").exists() {
        eprintln!("stub rt-host: injected failure");
        std::process::exit(3);
    }
    println!("{{\"status\":\"ok\",\"graph\":\"plan-arena\",\"blocks\":10,\"alsa\":{{\"xruns\":0,\"underruns\":0}}}}");
    std::process::exit(0);
}
"#;

fn rustc_bin() -> PathBuf {
    if let Some(cargo) = option_env!("CARGO") {
        let candidate =
            PathBuf::from(cargo).with_file_name(if cfg!(windows) { "rustc.exe" } else { "rustc" });
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("rustc")
}

fn stub_prototype() -> &'static Path {
    static PROTOTYPE: OnceLock<PathBuf> = OnceLock::new();
    PROTOTYPE
        .get_or_init(|| {
            let dir = temp_dir("stub-prototype");
            let source = dir.join("stub.rs");
            std::fs::write(&source, STUB_SOURCE).expect("write stub source");
            let output = dir.join(if cfg!(windows) { "stub.exe" } else { "stub" });
            let compiled = std::process::Command::new(rustc_bin())
                .args(["--edition", "2021", "-C", "opt-level=1", "-o"])
                .arg(&output)
                .arg(&source)
                .output()
                .expect("spawn rustc for test stub");
            assert!(
                compiled.status.success(),
                "rustc stub build failed: {}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            output
        })
        .as_path()
}

/// 测试专属 stub 副本（按文件名分派行为；witness/config/fail 文件都在同目录）。
fn stub_tool(tag: &str, kind: &str) -> PathBuf {
    let dir = temp_dir(&format!("{tag}-{kind}"));
    let name = format!("stub-{kind}{}", if cfg!(windows) { ".exe" } else { "" });
    let target = dir.join(name);
    std::fs::copy(stub_prototype(), &target).expect("copy stub prototype");
    target
}

fn witness_path(exe: &Path) -> PathBuf {
    exe.with_extension("witness")
}

fn witness_args(exe: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(witness_path(exe)).expect("read stub witness");
    text.lines()
        .filter_map(|line| line.strip_prefix("arg=").map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// aplay 模式：argv 与既有行为逐参数一致
// ---------------------------------------------------------------------------

#[test]
fn t76_aplay_mode_argv_is_unchanged() {
    let tag = format!("aplay-{}", UNIQUE.fetch_add(1, Ordering::SeqCst));
    let arecord = stub_tool(&tag, "arecord");
    let aplay = stub_tool(&tag, "aplay");
    let config = stage_config(2);
    let player = build_sweep_session(
        &config,
        "t76-aplay",
        "stereo-2.0",
        -50.0,
        SweepSessionKind::Acoustic,
    )
    .expect("sweep session");
    let timeline = player.timeline().clone();
    let mut source = TimelinePlaybackSource::new(
        player,
        &config.devices,
        CancelToken::new(),
        config.protection.peak_ceiling_dbfs,
    )
    .expect("playback source");
    let request = base_audio_request(&config, timeline.total_frames);
    let mut engine = ToolsEngine {
        arecord_bin: arecord.to_string_lossy().into_owned(),
        aplay_bin: aplay.to_string_lossy().into_owned(),
    };
    let capture = engine
        .run_session(&request, &mut source)
        .expect("aplay session");
    assert!(capture.capture_frames > 0, "stub arecord must feed blocks");
    assert_eq!(
        witness_args(&aplay),
        vec![
            "-q",
            "-t",
            "raw",
            "-f",
            "S24_3LE",
            "-c",
            "2",
            "-r",
            "48000",
            "-D",
            "hw:UltraLitemk5,0",
            "--period-size",
            "128",
            "--buffer-size",
            "512",
            "-",
        ],
        "aplay argv must stay bit-identical"
    );
    assert_eq!(
        witness_args(&arecord),
        vec![
            "-q",
            "-t",
            "raw",
            "-f",
            "S24_3LE",
            "-c",
            "1",
            "-r",
            "48000",
            "-D",
            "hw:0,0",
            "--period-size",
            "128",
            "--buffer-size",
            "512",
            "-",
        ],
        "arecord argv must stay bit-identical"
    );
}

// ---------------------------------------------------------------------------
// host-plan 模式：渲染 2ch WAV + 写 toml + 调用 khaen-rt-host（不碰 aplay）
// ---------------------------------------------------------------------------

const T76_HOST_CONFIG_TEXT: &str = concat!(
    "[host]\n",
    "block_size = 128\n",
    "sample_rate = 48000\n",
    "workers = 4\n",
    "graph = \"plan-arena\"\n",
    "safe_mode = \"normal\"\n",
    "[alsa]\n",
    "backend = \"alsa\"\n",
    "io_mode = \"synth\"\n",
    "[paths]\n",
    "plan = \"/root/compile/plan.bin\"\n",
    "plan_manifest = \"/root/compile/compile-report.json\"\n",
    "[capture]\n",
    "source = \"file\"\n",
    "path = \"/root/plan-input.wav\"\n",
    "format = \"wav\"\n",
    "eof = \"exit\"\n",
    "[run]\n",
    "max_blocks = 0\n"
);

fn host_plan_spec(dir: &Path, host_bin: PathBuf) -> HostPlanPlayback {
    HostPlanPlayback {
        host_bin,
        config_path: dir.join("rt-host.toml"),
        config_text: T76_HOST_CONFIG_TEXT.to_string(),
        wav_path: dir.join("plan-input.wav"),
        report_path: dir.join("host-report.json"),
        input_channels: 2,
        backend: "alsa".to_string(),
        timeout_ms: 5_000,
        plan_sha256: "aa".repeat(32),
        plan_graph_id: graph_id_hex(0x33),
        plan_path: PathBuf::from("/root/compile/plan.bin"),
        plan_manifest_path: PathBuf::from("/root/compile/compile-report.json"),
    }
}

fn run_host_plan_session(
    config: &StageBackendConfig,
    spec: HostPlanPlayback,
    arecord: &Path,
) -> (Result<AudioCaptureResult, StageError>, u64) {
    let mut render = config.clone();
    render.devices.playback_channels = spec.input_channels;
    let player = build_sweep_session(
        &render,
        "t76-host",
        "stereo-2.0",
        -50.0,
        SweepSessionKind::Acoustic,
    )
    .expect("sweep session");
    let timeline = player.timeline().clone();
    let mut source = TimelinePlaybackSource::new(
        player,
        &render.devices,
        CancelToken::new(),
        config.protection.peak_ceiling_dbfs,
    )
    .expect("playback source");
    let mut request = base_audio_request(&render, timeline.total_frames);
    request.playback = SessionPlayback::HostPlan(spec);
    let mut engine = ToolsEngine {
        arecord_bin: arecord.to_string_lossy().into_owned(),
        // host-plan 模式绝不能回落到 aplay：指向不存在的可执行文件，一旦被调用就会失败。
        aplay_bin: "t76-aplay-must-not-run".to_string(),
    };
    let result = engine.run_session(&request, &mut source);
    (result, timeline.total_frames)
}

#[test]
fn t76_host_plan_renders_wav_and_invokes_rt_host() {
    let tag = format!("host-{}", UNIQUE.fetch_add(1, Ordering::SeqCst));
    let arecord = stub_tool(&tag, "arecord");
    let host = stub_tool(&tag, "rt-host");
    let dir = temp_dir(&format!("{tag}-work"));
    let spec = host_plan_spec(&dir, host.clone());
    let config = stage_config(22);
    let (result, total_frames) = run_host_plan_session(&config, spec.clone(), &arecord);
    let capture = result.expect("host-plan session");
    assert!(capture.capture_frames > 0, "stub arecord must feed blocks");
    assert!(capture.notes.contains("host-plan"), "{}", capture.notes);
    assert!(capture.notes.contains("exit=0"), "{}", capture.notes);

    // 1) WAV：2ch / S24_3LE / 48k，data 长度 = timeline 帧数 × 6B。
    let wav = std::fs::read(&spec.wav_path).expect("plan-input.wav");
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2, "channels");
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        48_000,
        "sample rate"
    );
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 24, "bits");
    let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as u64;
    assert_eq!(
        data_len,
        total_frames * 6,
        "data = timeline frames x 2ch x 3B"
    );
    assert_eq!(wav.len() as u64, 44 + data_len);

    // 2) rt-host.toml 原文落盘；host 确实收到 run --config <toml>。
    assert_eq!(
        std::fs::read_to_string(&spec.config_path).expect("rt-host.toml"),
        spec.config_text
    );
    assert_eq!(
        witness_args(&host),
        vec![
            "run".to_string(),
            "--config".to_string(),
            spec.config_path.to_string_lossy().into_owned(),
        ]
    );
    let host_copy = host.with_extension("config");
    assert_eq!(
        std::fs::read_to_string(host_copy).expect("stub copied config"),
        spec.config_text,
        "host must consume the generated plan-arena file-source config"
    );
    // 3) host stdout JSON 原样落盘（证据）。
    let report = std::fs::read_to_string(&spec.report_path).expect("host-report.json");
    assert!(report.contains("\"plan-arena\""), "{report}");
}

#[test]
fn t76_host_plan_nonzero_exit_fails_honestly() {
    let tag = format!("hostfail-{}", UNIQUE.fetch_add(1, Ordering::SeqCst));
    let arecord = stub_tool(&tag, "arecord");
    let host = stub_tool(&tag, "rt-host");
    std::fs::write(host.with_extension("fail"), b"1").expect("fail marker");
    let dir = temp_dir(&format!("{tag}-work"));
    let spec = host_plan_spec(&dir, host);
    let config = stage_config(22);
    let (result, _) = run_host_plan_session(&config, spec, &arecord);
    let error = result.expect_err("host non-zero exit must fail the session");
    assert_eq!(error.code, E_PLAYBACK_FAILED);
    assert!(
        error.message.contains("退出码") && error.message.contains("injected failure"),
        "{}",
        error.message
    );
}

#[test]
fn t76_host_plan_timeout_fails_honestly() {
    let tag = format!("hosttimeout-{}", UNIQUE.fetch_add(1, Ordering::SeqCst));
    let arecord = stub_tool(&tag, "arecord");
    let host = stub_tool(&tag, "rt-host");
    let dir = temp_dir(&format!("{tag}-work"));
    let mut spec = host_plan_spec(&dir, host);
    spec.timeout_ms = 1;
    let config = stage_config(22);
    let (result, _) = run_host_plan_session(&config, spec, &arecord);
    let error = result.expect_err("host timeout must fail the session");
    assert_eq!(error.code, E_PLAYBACK_FAILED);
    assert!(error.message.contains("超时"), "{}", error.message);
}

// ---------------------------------------------------------------------------
// plan 产物解析（apply bundle / compile artifacts / explicit）与 fail-closed
// ---------------------------------------------------------------------------

#[test]
fn t76_remeasure_plan_resolution_and_fail_closed() {
    let root = temp_dir("resolve");
    let plan = write_compile_artifacts(&root, 0x42, 48_000, 128, 2, 8, 4);
    let host = default_host_config();
    let resolved = resolve_remeasure_plan(&root, &host, None).expect("compile artifact plan");
    assert_eq!(resolved.source, "compile-artifact");
    assert_eq!(resolved.plan_path, root.join("compile/plan.bin"));
    assert_eq!(resolved.geometry.n_in, 2);
    assert_eq!(resolved.geometry.n_out, 8);
    assert_eq!(resolved.geometry.workers, 4);
    assert_eq!(resolved.plan_sha256, crate::job::sha256_hex(&plan));
    assert_eq!(resolved.geometry.graph_id_hex(), graph_id_hex(0x42));

    // 缺产物：fail-closed（不回退 aplay），错误码明确。
    let empty = temp_dir("resolve-empty");
    let error = resolve_remeasure_plan(&empty, &host, None).expect_err("missing plan must fail");
    assert_eq!(error.code, E_REMEASURE_PLAN_MISSING);
    assert!(
        error.message.contains("compile/plan.bin"),
        "{}",
        error.message
    );

    // applied_version graph_id 不符：拒绝（stale artifact）。
    let stale =
        resolve_remeasure_plan(&root, &host, Some("pv-9:deadbeef")).expect_err("stale plan");
    assert_eq!(stale.code, E_REMEASURE_PLAYBACK_CONFIG);
    // 一致：接受。
    let version = format!("pv-9:{}", graph_id_hex(0x42));
    resolve_remeasure_plan(&root, &host, Some(&version)).expect("matching applied version");

    // apply bundle 优先（apply-host 写出的同一份字节）。
    let bundle = temp_dir("resolve-bundle");
    std::fs::copy(root.join("compile/plan.bin"), bundle.join("plan.bin")).unwrap();
    std::fs::copy(
        root.join("compile/compile-report.json"),
        bundle.join("compile-report.json"),
    )
    .unwrap();
    let mut host = host.clone();
    host.apply_bundle_dir = Some(bundle.clone());
    let resolved = resolve_remeasure_plan(&root, &host, None).expect("apply bundle plan");
    assert_eq!(resolved.source, "apply-bundle");
    assert_eq!(resolved.plan_path, bundle.join("plan.bin"));

    // 显式 --remeasure-plan（manifest 取同目录 compile-report.json）。
    let explicit = temp_dir("resolve-explicit");
    std::fs::copy(root.join("compile/plan.bin"), explicit.join("plan.bin")).unwrap();
    std::fs::copy(
        root.join("compile/compile-report.json"),
        explicit.join("compile-report.json"),
    )
    .unwrap();
    let mut host = default_host_config();
    host.plan_path = Some(explicit.join("plan.bin"));
    let resolved = resolve_remeasure_plan(&empty, &host, None).expect("explicit plan");
    assert_eq!(resolved.source, "explicit");
    // 显式 plan 缺 manifest 同样 fail-closed。
    std::fs::remove_file(explicit.join("compile-report.json")).unwrap();
    let error = resolve_remeasure_plan(&empty, &host, None).expect_err("explicit missing manifest");
    assert_eq!(error.code, E_REMEASURE_PLAN_MISSING);
}

// ---------------------------------------------------------------------------
// stereo 2.0 单点口径（7ch/4.1.2 = NOT_IMPLEMENTED）与 toml 内容
// ---------------------------------------------------------------------------

fn geometry_2ch() -> HostPlanGeometry {
    HostPlanGeometry {
        abi_version: 2,
        sample_rate: 48_000,
        block_size: 128,
        n_in: 2,
        n_out: 8,
        workers: 4,
        graph_id: [0x33; 32],
    }
}

fn stereo_outputs() -> Vec<SweepOutput> {
    vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)]
}

#[test]
fn t76_host_plan_rejects_7ch_and_4_1_2_as_not_implemented() {
    let devices = device_config(22);
    let geometry = geometry_2ch();
    validate_remeasure_host_plan(&geometry, "stereo-2.0", &stereo_outputs(), &devices, true)
        .expect("stereo 2.0 single point is supported");

    let layout =
        validate_remeasure_host_plan(&geometry, "4.1.2", &stereo_outputs(), &devices, true)
            .expect_err("4.1.2 layout must be rejected");
    assert_eq!(layout.code, E_REMEASURE_PLAYBACK_CONFIG);
    assert!(
        layout.message.contains("NOT_IMPLEMENTED"),
        "{}",
        layout.message
    );

    let mut seven_input = geometry.clone();
    seven_input.n_in = 7;
    let error = validate_remeasure_host_plan(
        &seven_input,
        "stereo-2.0",
        &stereo_outputs(),
        &devices,
        true,
    )
    .expect_err("7ch plan input must be rejected");
    assert!(
        error.message.contains("NOT_IMPLEMENTED"),
        "{}",
        error.message
    );

    let seven_outputs = vec![
        SweepOutput::new(0, "FL", 0),
        SweepOutput::new(1, "FR", 1),
        SweepOutput::new(2, "SL", 2),
    ];
    let error =
        validate_remeasure_host_plan(&geometry, "stereo-2.0", &seven_outputs, &devices, true)
            .expect_err("7ch outputs must be rejected");
    assert!(
        error.message.contains("NOT_IMPLEMENTED"),
        "{}",
        error.message
    );

    let mut too_wide = geometry.clone();
    too_wide.n_out = 23;
    let error =
        validate_remeasure_host_plan(&too_wide, "stereo-2.0", &stereo_outputs(), &devices, true)
            .expect_err("plan.n_out > device playback width must be rejected");
    assert_eq!(error.code, E_REMEASURE_PLAYBACK_CONFIG);

    let mut wrong_block = geometry.clone();
    wrong_block.block_size = 64;
    let error = validate_remeasure_host_plan(
        &wrong_block,
        "stereo-2.0",
        &stereo_outputs(),
        &devices,
        true,
    )
    .expect_err("plan/device block mismatch must be rejected");
    assert_eq!(error.code, E_REMEASURE_PLAYBACK_CONFIG);

    // FakeEngine（离线合成，不启动 host）：设备宽度不强制，只做配置接线（summary 标 simulated）。
    let narrow = device_config(2);
    validate_remeasure_host_plan(&geometry, "stereo-2.0", &stereo_outputs(), &narrow, false)
        .expect("simulated run does not launch host, so device width is not enforced");
    validate_remeasure_host_plan(&geometry, "stereo-2.0", &stereo_outputs(), &narrow, true)
        .expect_err("real engine still requires plan.n_out <= device playback width");
}

#[test]
fn t76_host_plan_config_text_is_plan_arena_file_source() {
    let devices = device_config(22);
    let geometry = geometry_2ch();
    let host = RemeasureHostConfig {
        host_bin: PathBuf::from("khaen-rt-host"),
        backend: "alsa".to_string(),
        capture_channels: 20,
        plan_path: None,
        apply_bundle_dir: None,
        work_dir: None,
    };
    let text = remeasure_host_config_text(
        Path::new("/root/candidate/compile/plan.bin"),
        Path::new("/root/candidate/compile/compile-report.json"),
        Path::new("/root/candidate/remeasure-host-plan/plan-input.wav"),
        &devices,
        &geometry,
        &host,
    );
    for needle in [
        "[host]",
        "block_size = 128",
        "sample_rate = 48000",
        "workers = 4",
        "graph = \"plan-arena\"",
        "safe_mode = \"normal\"",
        "[alsa]",
        "backend = \"alsa\"",
        "io_mode = \"synth\"",
        "device = \"hw:UltraLitemk5,0\"",
        "capture_channels = 20",
        "playback_channels = 22",
        "period_frames = 128",
        "buffer_frames = 512",
        "[paths]",
        "plan = \"/root/candidate/compile/plan.bin\"",
        "plan_manifest = \"/root/candidate/compile/compile-report.json\"",
        "[capture]",
        "source = \"file\"",
        "path = \"/root/candidate/remeasure-host-plan/plan-input.wav\"",
        "format = \"wav\"",
        "eof = \"exit\"",
        "[run]",
        "max_blocks = 0",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    // file 源与 loopback 互斥；测量不得 mute。
    assert!(!text.contains("loopback"), "{text}");
    assert!(
        !text.contains("mute"),
        "measurement must not be muted:\n{text}"
    );
    // Windows 路径统一写正斜杠（rt-host 的 TOML 子集不做转义；板端路径不受影响）。
    let windows = remeasure_host_config_text(
        Path::new(r"C:\root\compile\plan.bin"),
        Path::new(r"C:\root\compile\compile-report.json"),
        Path::new(r"C:\root\wav\plan-input.wav"),
        &devices,
        &geometry,
        &host,
    );
    assert!(
        windows.contains("plan = \"C:/root/compile/plan.bin\""),
        "{windows}"
    );
}

// ---------------------------------------------------------------------------
// 端到端：Full job 里 sweep 必须 Aplay，只有 remeasure/holdout 走 HostPlan
// ---------------------------------------------------------------------------

struct RecordingEngine {
    inner: FakeEngine,
    requests: Arc<Mutex<Vec<AudioSessionRequest>>>,
}

impl StageAudioEngine for RecordingEngine {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn probe(&mut self, devices: &StageDeviceConfig) -> Result<StageProbe, StageError> {
        self.inner.probe(devices)
    }

    fn run_session(
        &mut self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
    ) -> Result<AudioCaptureResult, StageError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        self.inner.run_session(request, playback)
    }
}

struct OfflineSafety;

impl SafeOutputController for OfflineSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }
    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }
    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok("last-good-v0".to_string())
    }
    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }
    fn last_good_version(&self) -> Option<String> {
        Some("last-good-v0".to_string())
    }
}

fn fake_host_plan_config() -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Fake;
    config.devices = device_config(22);
    config.mic = Some(StageBackendConfig::umik1_declaration(
        "hw:0,0",
        "718-7696",
        "umik-90deg.txt",
        "0123456789abcdef".repeat(4),
        "90deg",
    ));
    config.sweep = StageSweepConfig::smoke();
    config.outputs = stereo_outputs();
    config.session_label = "t76-e2e".to_string();
    config.graph_id = graph_id_hex(0x5a);
    config.epoch_id = 1;
    config.remeasure_playback = RemeasurePlayback::HostPlan;
    config.remeasure_host = default_host_config();
    config
}

fn artifact_bytes(snapshot: &JobSnapshot, relative: &str) -> Vec<u8> {
    let path = Path::new(&snapshot.artifacts_dir).join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read artifact {relative}: {error}"))
}

#[test]
fn t76_full_job_host_plan_only_for_secondary_stages() {
    use crate::fit_wiring::FitWiring;
    let root = temp_dir("e2e");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = RecordingEngine {
        inner: FakeEngine::new(FakeEngineConfig {
            preroll_frames: 2_400,
            tail_frames: 2_400,
            gain_db: 0.0,
            noise_dbfs: -90.0,
            ..FakeEngineConfig::default()
        }),
        requests: requests.clone(),
    };
    let wiring = FitWiring::new(crate::fit_wiring::synthetic_design().expect("synthetic design"))
        .expect("fit wiring");
    let backend = StageBackend::new(fake_host_plan_config(), Box::new(engine))
        .expect("stage backend")
        .with_delegate(Box::new(wiring));
    let manager = JobManager::new(
        JobManagerConfig {
            artifacts_root: root.join("jobs"),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(OfflineSafety),
    )
    .expect("job manager");
    let request = JobRequest {
        kind: JobKind::Full,
        label: "t76-host-plan-e2e".to_string(),
        layout: "stereo-2.0".to_string(),
        sample_rate: 48_000,
        design_points: 1,
        holdout_points: 1,
        repeats: 1,
        drive_level_dbfs: -50.0,
        audible_sweep_authorized: true,
        auto_apply: true,
        expected_applied_version: None,
        inputs: Vec::new(),
    };
    manager
        .start(request, Box::new(backend))
        .expect("start job");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(180))
        .expect("job terminal");
    assert_eq!(
        snapshot.state,
        JobState::Complete,
        "full job must complete: {:?}",
        snapshot.failure
    );
    let requests = requests.lock().expect("requests lock").clone();
    assert_eq!(
        requests.len(),
        3,
        "one session for sweep + one remeasure + one holdout"
    );
    assert_eq!(
        requests[0].playback,
        SessionPlayback::Aplay,
        "sweep_capture must stay on aplay (measuring the uncorrected room)"
    );
    let applied = snapshot.applied_version.clone().expect("applied version");
    for (session, stage_dir) in [
        (&requests[1], "remeasure"),
        (&requests[2], "independent_holdout"),
    ] {
        match &session.playback {
            SessionPlayback::HostPlan(host) => {
                assert_eq!(host.input_channels, 2);
                let text = &host.config_text;
                for needle in [
                    "graph = \"plan-arena\"",
                    "io_mode = \"synth\"",
                    "safe_mode = \"normal\"",
                    "source = \"file\"",
                    "format = \"wav\"",
                    "eof = \"exit\"",
                    "max_blocks = 0",
                    "compile/plan.bin",
                    "compile/compile-report.json",
                ] {
                    assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
                }
                assert!(
                    host.config_path
                        .to_string_lossy()
                        .contains(&format!("{stage_dir}-host-plan")),
                    "{}",
                    host.config_path.display()
                );
                assert!(host.wav_path.to_string_lossy().ends_with("plan-input.wav"));
                assert!(
                    applied.ends_with(&host.plan_graph_id),
                    "{applied} vs {}",
                    host.plan_graph_id
                );
                assert_eq!(host.plan_sha256.len(), 64);
            }
            other => panic!("{stage_dir} playback must be host-plan, got {other:?}"),
        }
    }
    // fake 引擎不启动 host：remeasure summary 如实标 host_executed=false（simulated）。
    let summary = String::from_utf8(artifact_bytes(&snapshot, "remeasure/summary.json"))
        .expect("remeasure summary utf8");
    assert!(
        summary.contains("\"plan_playback\":{\"mode\":\"host-plan\""),
        "{summary}"
    );
    assert!(summary.contains("\"host_executed\":false"), "{summary}");
    assert!(summary.contains("compile/plan.bin"), "{summary}");
}
