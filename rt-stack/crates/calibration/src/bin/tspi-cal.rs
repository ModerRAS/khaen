//! [NONRT CLI] T67 前置：校准 job 端到端命令（JobManager -> FitP(T64/T65) -> T66 compile/apply）。
//!
//! 用法（本机离线 dry-run；--engine 只支持 fake，真实设备/板端 NOT_RUN）：
//!
//!   tspi-cal run --design synthetic --out scratch/fitwiring-dryrun
//!   tspi-cal run --design synthetic --out scratch/fitwiring-apply --apply
//!   tspi-cal run --design synthetic --out scratch/fitwiring-fault --apply --fault withhold-ack
//!
//! 行为：
//! - Full job 走 T60 JobManager：preflight/sweep/quality（StageBackend fake 测量）
//!   -> FitP（T64 fit_peq + T65 fit_phase）-> SolveG（G 输入接线）-> Compile（T66 plan.bin）
//!   -> Prepared（T66 prepare/admission 模型）；--apply 时继续 Apply（T66 commit + offline all-ack）
//!   -> Remeasure/IndependentHoldout（fake 测量）；
//! - 无 --apply 时在 prepared 等 apply，CLI 主动 rollback 收尾（aborted before commit），不留悬挂 job；
//! - 证据写 --out：jobs/（JobManager artifact 树）、job.json、summary.json；
//! - T3 应用后实测/独立 holdout/真实设备/真实 apply 全部 NOT_RUN，summary 显式标注。
//!
//! 退出码：0 = job complete；2 = 用法/设计输入错误；3 = 真实引擎 NOT_WIRED（NOT_RUN 口径）；
//! 4 = job failed/rolled_back/cancelled（failure 码打印到 stderr）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use tspi_calibration::fit_wiring::{
    apply_measured_target_fir_experiment, enable_target_fir_p_post, synthetic_design, FitWiring,
    FitWiringDesign, WithheldAck,
};
use tspi_calibration::host_apply::HostApplyConfig;
use tspi_calibration::job::{
    ExitDisposition, JobManager, JobManagerConfig, JobRequest, JobSnapshot, SafeOutputController,
    SafetyError,
};
use tspi_calibration::compile_apply::ExperimentMode;
use tspi_calibration::measured_design::{
    build_measured_design_opts, load_g_paths_file, load_measured_spec, MeasuredBuildOptions,
};
use tspi_calibration::rt60::{
    analyze_wav_path, csd_json, unix_now, warning_json, Rt60AnalyzedIr, Rt60Config, Rt60Report,
    Rt60Source,
};
use tspi_calibration::stage_backend::{
    EngineKind, RemeasureHostConfig, RemeasurePlayback, Rt60ExternalIr, Rt60StageConfig,
    StageBackend, StageBackendConfig, StageDeviceConfig, StageLevelConfig, StageMeasurementMode,
    StageSweepConfig,
};
use tspi_calibration::sweep::SweepOutput;
use tspi_calibration::phase_fit::{DePhasedResponse, PhaseFitPoint};
use tspi_fir_designer::modal_peq::log_frequency_grid;
use tspi_fir_designer::targets::{builtin_target, BuiltinOptions, BuiltinTarget};
use tspi_modal_design::timebase::CommonD0;
use tspi_dsp_compat::ws::{json_encode, json_object, json_parse, JsonValue};

const USAGE: &str = "\
tspi-cal status|enter|measure|solve|remeasure|leave [--audible-authorized] [--sock PATH] [--f-hi HZ] [--target flat|tilt10]
tspi-cal run --design synthetic --out DIR [--apply] [--fault none|fit-gate|bad-artifact|withhold-ack]
tspi-cal run --engine tools --audible-authorized --out DIR --apply [--apply-host --host-backend alsa]

设计输入：
  --design synthetic     离线合成设计输入（默认）
  --design measured      真实测量设计输入（需 --measurement FILE；旧工具 analysis JSON 桥接）
  --measurement FILE     measured 模式 spec（schema tspi-cal-measured-design/v1 或 .../v2；
                          v2 可给 design_positions[] / validation_positions[] 做多点 + 独立 holdout）
  --layout NAME          布局 id（默认 synthetic=stereo-2.0；measured 用 spec 内 layout）
  --target-tilt10        仅 synthetic：把 T64 目标换成 tilt10 并启用 target FIR（P_post 非 identity，
                         用于真实 apply 生效的可测差异；默认 flat + target FIR 关闭 = P identity）
  --target-fir-taps N    target FIR 抽头数（默认 512；会同时把 compile l_p 提到该长度）
  --max-peq-stages N     T64 PEQ 段数上限 1..=32（默认库默认值）；设 1 可让残差 FIR 承载目标曲线
  --phase-delay N        仅 synthetic：用给定整数采样延迟重建 T65 输入（S0/S1b 声明延迟对照；
                         与 --target-tilt10 同用会被 tilt 的 P FIR GD 冲突拒绝，属预期）
  --peq-modal-band A:B   T64 modal 拟合带（默认库默认值；用于把 IIR 段压到 0、让残差 FIR 承载 P）
  --peq-wide-band A:B    T64 wide 拟合带（同上）

实验旁路（EXPERIMENT / NON_ACCEPTABLE；默认全关，生产路径行为不变）：
  --experiment-single-point 仅 measured：开启单点 G 实验（允许 4.1.2 五执行器 layout、
                            FitP/T65 生产门不阻断 compile；产物一律标 EXPERIMENT/NON_ACCEPTABLE）
  --experiment-multipoint-pilot 仅 measured：开启多点 pilot 实验（spec v2 design>=2 + 独立
                            validation>=1；FitP/T65 生产门不阻断 compile；产物 EXPERIMENT/
                            MULTIPOINT_PILOT_NOT_CERTIFIABLE，不产生 T6/holdout 通过结论）
  --allow-incomplete-artifact 仅实验：允许 T54 artifact 非 READY（INCOMPLETE）仍编译；
                            不带本 flag 时仍以 E_CA_ARTIFACT_STATUS fail-closed
  --experiment-target-fir    仅 measured 实验：启用 T64 target FIR（P_post 非 identity，
                            用于验证 compile/host 消费目标曲线 FIR；产物 EXPERIMENT）
  --compile-force-factorized 实验：强制 compile 形态 factorized（host plan-arena 当前只接受
                             factorized；fused 加载 NOT_WIRED；生产默认按 T12 成本选择，不变）
  --g-paths FILE         实验：注入求解器输出 G（schema tspi-cal-g-paths/v1；替换 identity baseline）
  --g-taps N             实验：声明 G 支撑 L_G（必须与 --g-paths 文件 l_g 一致）

  measured spec 的 crossover 节（生产默认关闭；当前仅实验路径，需 --experiment-single-point
  且 layout 必须为 4.1.2）：
  \"crossover\": {\"type\":\"LR4\",\"freq_hz\":80,\"taps\":2048,\"method\":\"analytic_phase\",
                \"attenuation_db\":80,
                \"assignments\":[{\"output\":0,\"branch\":\"highpass\"}, ...]}
  -> P_post = crossover ⊛ target FIR（core 规范段序），plan diag flags 置 DIAG_FLAG_CROSSOVER(bit2)，
     D7 证据绑定卷积后 P 行预测；80Hz LR4 2048 taps + target 512 -> L_P=2559。
     未赋值输出 = 直通（无默认 HP）；超 max_taps/INFEASIBLE 一律 fail-closed。

测量引擎（engine）：
  --engine fake          离线合成测量引擎（默认）
  --engine tools         ALSA arecord/aplay 真实测量（别名 alsa-tools；auto 等价于 tools）
  --audible-authorized   tools 模式必须显式声明本窗口允许可闻扫频（缺省拒绝，防误出声）
  --playback-dev NAME    playback ALSA 设备（默认 hw:UltraLitemk5,0）
  --playback-channels N  playback 设备通道数（默认 22）
  --capture-dev NAME     capture ALSA 设备（默认 hw:0,0 = UMIK-1）
  --capture-channels N   设备读块通道数（默认 2；UMIK-1 在本机暴露 2ch）
  --capture-channel N    2ch 采集时选哪一路进 T62（默认 0）
  --level-db DBFS        驱动电平（默认 tools=-30 / fake=-50；> -30 需 --hot-level-authorized）
  --hot-level-authorized 显式授权 > -30 dBFS
  --sweep-seconds S      单遍扫频秒数（默认 5.0）
  --sweep-start-hz F     起始频率（默认 20）
  --sweep-end-hz F       结束频率（默认 20000）
  --sweep-repeats N      每 output 重复遍数 1..=10（默认 1）
  --sweep-gap S          遍间静音秒（默认 0.5）
  --pre-seconds S        前静音秒（默认 1.5）
  --post-seconds S       后静音秒（默认 1.5）
  --channels LIST        playback 通道列表（默认 0；4.1.2=0,1,2,3,4,5,7）
  --names LIST           对应 speaker 名（默认 CH<n>；4.1.2=FL,FR,SL,SR,HFL,HFR,SUB1）
  --k-main-trim-db DB    MOTU kMainTrim 记录值（默认 22.0，必须与实测一致）
  --mic-serial TEXT      UMIK 序列号（默认 unknown）
  --mic-cal-file TEXT    UMIK 校准文件名（默认占位，见证据诚实边界）
  --mic-cal-sha256 HEX   校准文件 sha256（默认占位声明文本 hash）
  --settle-ms N          采集先起后等待（默认 1200）
  --apply-settle-ms N    apply-host 退出 → remeasure 采集之间的 settle（默认 1000；0=关闭，<500 拒绝）
  --remeasure-playback M 二次测量（remeasure/holdout）播放路径：aplay（默认，既有行为）|
                         host-plan（T61 timeline → 2ch WAV → tspi-rt-host plan-arena，测得“校正量”）
                         仅 stereo-2.0 单点（2ch plan；7ch/4.1.2 = NOT_IMPLEMENTED）；sweep 恒 aplay
  --remeasure-plan PATH  显式 plan.bin（缺省从 --apply-host 的 host-apply/plan.bin 或 job
                         artifacts 的 compile/plan.bin 解析；找不到 fail-closed，不回退 aplay）
  --remeasure-host-capture-channels N  host MOTU duplex capture 宽度（默认 20；与 UMIK 采集无关）
  --min-snr-db DB        保护门 SNR 下限（默认 25；调低只为诊断时仍落 capture 证据，summary 会记实际 SNR）
  --max-xruns N          保护门 xrun 上限（默认 0）

RT60/CSD 后处理（goal-6；默认开启，观察量，不阻断主流程；真实房间测量离线态 NOT_RUN）：
  --no-rt60              关闭 RT60/CSD（仍写 rt60/status.json 留痕）
  --rt60-ir FILES        外部真实反卷积 IR WAV（逗号分隔；优先于 job 采集反卷积）
  --rt60-label NAMES     与 --rt60-ir 对应的标签（逗号分隔；默认取文件名）
  --rt60-window-ms N     RT60 分析窗口（默认 900 ms，自直达声起）
  --rt60-csd-window-ms N CSD 窗口（默认 600 ms）
  --rt60-tail-ms N       反卷积 IR 尾部（默认 900 ms）
  --rt60-bands LIST      倍频带中心频率 Hz（逗号分隔；默认 31.5,63,125,250,500,1000,2000,4000,8000,16000）

离线 IR 分析（不跑 job；同一 Rust 算法，输出 summary.json/rt60.csv/csd/）：
  tspi-cal rt60 --ir FILES --out DIR [--label NAMES] [--window-ms N] [--bands LIST]

其它：
  --out DIR              证据输出目录（jobs/ artifact 树 + job.json + summary.json；必填）
  --apply                走到 T66 apply（默认停在 prepared 后 CLI 主动 rollback 收尾）
  --fault NAME           负向路径注入：fit-gate | bad-artifact | withhold-ack
  --apply-host           Apply 段改用真实 tspi-rt-host plan-arena（默认离线合成 ack 模型）
  --host-bin PATH        tspi-rt-host 可执行文件（默认 env TSPI_RT_HOST_BIN 或 PATH 中 tspi-rt-host）
  --host-backend NAME    host ALSA backend：fake（离线，默认）| alsa（板端窗口）
  --host-max-blocks N    host 运行 block 数上限（默认 8；板端可放大）
  --host-workers N       host worker 数 2|4（默认 4；dsp-host 4 核需与 plan 一致）
  --host-cpus TEXT       host dsp_cpus（默认 4-7；dsp-host 用 2,3）
  --host-safe-mode NAME  host 安全模式 normal|mute（测试期建议 mute）
  --host-engine NAME     host DSP 引擎 auto|direct|partitioned（默认 auto；大 L_P 时
                         auto->partitioned 产生 single-executor all-ack（swap.single_executor=true），
                         direct 为 4-worker all-ack）
  --label TEXT           job label（默认 tspi-cal-dryrun）
";

#[derive(Debug, Default)]
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

#[derive(Debug)]
struct Args {
    command: String,
    values: BTreeMap<String, String>,
    flags: std::collections::BTreeSet<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut command = String::new();
        let mut values = BTreeMap::new();
        let mut flags = std::collections::BTreeSet::new();
        let mut index = 0;
        while index < raw.len() {
            let token = &raw[index];
            if !token.starts_with("--") {
                if command.is_empty() {
                    command = token.clone();
                    index += 1;
                    continue;
                }
                return Err(format!("unexpected positional argument {token:?}"));
            }
            let key = token.trim_start_matches("--").to_string();
            let takes_value = !matches!(
                key.as_str(),
                "apply"
                    | "apply-host"
                    | "audible-authorized"
                    | "no-rt60"
                    | "hot-level-authorized"
                    | "target-tilt10"
                    | "experiment-single-point"
                    | "experiment-multipoint-pilot"
                    | "allow-incomplete-artifact"
                    | "experiment-target-fir"
                    | "compile-force-factorized"
                    | "help"
                    | "h"
            );
            if !takes_value {
                flags.insert(key);
                index += 1;
                continue;
            }
            let Some(value) = raw.get(index + 1) else {
                return Err(format!("missing value for --{key}"));
            };
            values.insert(key, value.clone());
            index += 2;
        }
        if command.is_empty() {
            return Err("missing command (run)".to_string());
        }
        Ok(Args {
            command,
            values,
            flags,
        })
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|value| value.as_str())
    }

    fn has(&self, key: &str) -> bool {
        self.flags.contains(key)
    }
}

fn fixture_hash(label: &str) -> String {
    tspi_calibration::job::sha256_hex(format!("tspi-cal-cli/v1\n{label}\n").as_bytes())
}

fn fake_stage_config(label: &str) -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Fake;
    config.devices = StageDeviceConfig {
        playback_device: "hw:0,0".to_string(),
        playback_channels: 2,
        capture_device: "hw:1,0".to_string(),
        capture_channels: 1,
        capture_channel: None,
        sample_rate: 48_000,
        block_frames: 128,
        buffer_frames: 512,
        settle_ms: 10,
        sample_format: tspi_dsp_core::abi::sample_format::S24_3LE_INTERLEAVED,
    };
    config.mic = Some(StageBackendConfig::umik1_declaration(
        "hw:1,0",
        "SIM-0001",
        "synthetic-cal.txt",
        fixture_hash("umik-cal"),
        "90deg",
    ));
    config.level = StageLevelConfig::default();
    config.sweep = StageSweepConfig::smoke();
    config.outputs = vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)];
    config.session_label = label.to_string();
    config.graph_id = fixture_hash("host-graph");
    config.epoch_id = 1;
    config
}

fn arg_u64(args: &Args, key: &str, default: u64) -> Result<u64, (String, u8)> {
    match args.value(key) {
        None => Ok(default),
        Some(text) => text
            .parse::<u64>()
            .map_err(|error| (format!("--{key} 非法（{text:?}）：{error}"), 2u8)),
    }
}

fn arg_u32(args: &Args, key: &str, default: u32) -> Result<u32, (String, u8)> {
    arg_u64(args, key, default as u64).map(|value| value as u32)
}

fn arg_f64(args: &Args, key: &str, default: f64) -> Result<f64, (String, u8)> {
    match args.value(key) {
        None => Ok(default),
        Some(text) => text
            .parse::<f64>()
            .map_err(|error| (format!("--{key} 非法（{text:?}）：{error}"), 2u8)),
    }
}

/// 真实测量（arecord/aplay）StageBackend 配置：设备/通道/电平/sweep/UMIK 声明全部显式。
fn tools_stage_config(args: &Args, label: &str) -> Result<StageBackendConfig, (String, u8)> {
    let playback_device = args
        .value("playback-dev")
        .unwrap_or("hw:UltraLitemk5,0")
        .to_string();
    let playback_channels = arg_u64(args, "playback-channels", 22)? as u16;
    let capture_device = args.value("capture-dev").unwrap_or("hw:0,0").to_string();
    let capture_channels = arg_u64(args, "capture-channels", 2)? as u16;
    if !(1..=2).contains(&capture_channels) {
        return Err((
            "--capture-channels 只接受 1|2（UMIK 声学采集口径）".to_string(),
            2,
        ));
    }
    let capture_channel = if capture_channels > 1 {
        Some(arg_u64(args, "capture-channel", 0)? as u16)
    } else {
        None
    };
    let sample_rate = arg_u32(args, "sample-rate", 48_000)?;
    let block_frames = arg_u64(args, "block-frames", 128)? as u16;
    let buffer_frames = arg_u64(args, "buffer-frames", 512)? as u32;
    let settle_ms = arg_u64(args, "settle-ms", 1_200)?;
    let apply_settle_ms = arg_u64(
        args,
        "apply-settle-ms",
        tspi_calibration::stage_backend::DEFAULT_APPLY_SETTLE_MS,
    )?;
    let mut config = StageBackendConfig::default();
    config.apply_settle_ms = apply_settle_ms;
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Tools;
    config.devices = StageDeviceConfig {
        playback_device,
        playback_channels,
        capture_device: capture_device.clone(),
        capture_channels,
        capture_channel,
        sample_rate,
        block_frames,
        buffer_frames,
        settle_ms,
        sample_format: tspi_dsp_core::abi::sample_format::S24_3LE_INTERLEAVED,
    };
    // UMIK 声明：本机无 miniDSP 官方序列号校准文件 → cal_file/sha256 为**声明占位**，
    // 证据里如实写明；FR 只做 1 kHz 相对化（不声称绝对 SPL 校准）。
    let mic_serial = args.value("mic-serial").unwrap_or("unknown").to_string();
    let mic_cal_file = args
        .value("mic-cal-file")
        .unwrap_or("umik1-serial-unknown-cal-placeholder.txt")
        .to_string();
    let mic_cal_sha = match args.value("mic-cal-sha256") {
        Some(value) => {
            if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err((
                    "--mic-cal-sha256 必须是 64 位 hex（或省略使用占位声明 hash）".to_string(),
                    2,
                ));
            }
            value.to_ascii_lowercase()
        }
        None => tspi_calibration::job::sha256_hex(
            format!("tspi-umik1-cal-placeholder/v1\nserial={mic_serial}\ncal_file={mic_cal_file}\n")
                .as_bytes(),
        ),
    };
    config.mic = Some(StageBackendConfig::umik1_declaration(
        capture_device,
        mic_serial,
        mic_cal_file,
        mic_cal_sha,
        "90deg",
    ));
    let level_db = arg_f64(args, "level-db", -30.0)?;
    let mut level = StageLevelConfig::default();
    level.default_drive_level_dbfs = level_db;
    level.hot_level_authorized = args.has("hot-level-authorized");
    config.level = level;
    config.protection.min_snr_db = arg_f64(args, "min-snr-db", 25.0)?;
    config.protection.max_xruns = arg_u64(args, "max-xruns", 0)?;
    let mut sweep = StageSweepConfig::default();
    sweep.start_hz = arg_f64(args, "sweep-start-hz", 20.0)?;
    sweep.end_hz = arg_f64(args, "sweep-end-hz", 20_000.0)?;
    sweep.seconds = arg_f64(args, "sweep-seconds", 5.0)?;
    sweep.repeats = arg_u32(args, "sweep-repeats", 1)?;
    sweep.repeat_gap_seconds = arg_f64(args, "sweep-gap", 0.5)?;
    sweep.pre_seconds = arg_f64(args, "pre-seconds", 1.5)?;
    sweep.post_seconds = arg_f64(args, "post-seconds", 1.5)?;
    sweep.k_main_trim_db = arg_f64(args, "k-main-trim-db", 22.0)?;
    config.sweep = sweep;
    let channels_text = args.value("channels").unwrap_or("0");
    let names_text = args.value("names").unwrap_or("");
    let channels: Vec<u32> = channels_text
        .split(',')
        .filter(|token| !token.trim().is_empty())
        .map(|token| {
            token
                .trim()
                .parse::<u32>()
                .map_err(|error| (format!("--channels 项 {token:?} 非法：{error}"), 2u8))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if channels.is_empty() {
        return Err(("--channels 至少一项".to_string(), 2));
    }
    let names: Vec<String> = names_text
        .split(',')
        .filter(|token| !token.trim().is_empty())
        .map(|token| token.trim().to_string())
        .collect();
    let mut outputs = Vec::with_capacity(channels.len());
    for (slot, channel) in channels.iter().enumerate() {
        let name = names
            .get(slot)
            .cloned()
            .unwrap_or_else(|| format!("CH{channel}"));
        outputs.push(SweepOutput::new(slot as u32, name, *channel));
    }
    config.outputs = outputs;
    config.session_label = label.to_string();
    config.graph_id = fixture_hash("host-graph");
    config.epoch_id = 1;
    config
        .validate()
        .map_err(|failure| (format!("tools stage config 非法：{failure:?}"), 2u8))?;
    Ok(config)
}

/// 二次测量播放参数（--remeasure-playback / --remeasure-plan / host bin+backend）。
///
/// 默认 aplay 保持既有行为；host-plan 复用 apply-host 的 --host-bin/--host-backend 语义，
/// 并在 --apply-host 时把 apply 阶段的 plan bundle 目录作为 plan 解析首选。
fn apply_remeasure_playback_args(
    args: &Args,
    config: &mut StageBackendConfig,
    apply_bundle_dir: Option<PathBuf>,
    auto_apply: bool,
) -> Result<(), (String, u8)> {
    let mode = match args.value("remeasure-playback") {
        None => RemeasurePlayback::Aplay,
        Some(value) => RemeasurePlayback::parse(value).ok_or_else(|| {
            (
                format!("--remeasure-playback {value:?} 不支持（aplay|host-plan）"),
                2u8,
            )
        })?,
    };
    config.remeasure_playback = mode;
    let mut host = RemeasureHostConfig::default();
    if let Some(host_bin) = args.value("host-bin") {
        host.host_bin = PathBuf::from(host_bin);
    }
    if let Some(backend) = args.value("host-backend") {
        host.backend = backend.to_string();
    }
    host.capture_channels =
        arg_u64(args, "remeasure-host-capture-channels", host.capture_channels as u64)? as u16;
    if let Some(plan) = args.value("remeasure-plan") {
        host.plan_path = Some(PathBuf::from(plan));
    }
    if mode == RemeasurePlayback::HostPlan {
        if !auto_apply {
            return Err((
                "--remeasure-playback host-plan 需要 --apply：无 --apply 时 job 停在 prepared，永远走不到 Remeasure/Holdout"
                    .to_string(),
                2u8,
            ));
        }
        if args.value("host-safe-mode") == Some("mute") {
            return Err((
                "--remeasure-playback host-plan 与 --host-safe-mode mute 互斥：mute 会把测量激励静音，测到的是静音/本底，不是校正量"
                    .to_string(),
                2u8,
            ));
        }
        if config.engine == EngineKind::Tools && host.backend == "fake" {
            eprintln!(
                "warning: --remeasure-playback host-plan 配 --host-backend fake 时不打开任何设备（仅离线接线）；真实测量会因无播放而失败"
            );
        }
        host.apply_bundle_dir = apply_bundle_dir;
    }
    host.validate().map_err(|failure| {
        (
            format!(
                "remeasure host 配置非法：{} {}",
                failure.code, failure.message
            ),
            2u8,
        )
    })?;
    config.remeasure_host = host;
    if mode == RemeasurePlayback::HostPlan && config.engine == EngineKind::Fake {
        eprintln!(
            "note: --engine fake 时 --remeasure-playback host-plan 只做配置/证据接线：FakeEngine 合成测量、不启动 host（summary 会标 host_executed=false）"
        );
    }
    Ok(())
}

fn job_request(
    args: &Args,
    label: &str,
    auto_apply: bool,
    engine_tools: bool,
    measured_layout: Option<&str>,
    measured_rate: Option<u32>,
) -> Result<JobRequest, (String, u8)> {
    let layout = measured_layout
        .map(str::to_string)
        .or_else(|| args.value("layout").map(str::to_string))
        .unwrap_or_else(|| "stereo-2.0".to_string());
    let sample_rate = measured_rate.unwrap_or(arg_u32(args, "sample-rate", 48_000)?);
    let repeats = arg_u32(args, "sweep-repeats", 1)?;
    let drive_level_dbfs = arg_f64(
        args,
        "level-db",
        if engine_tools { -30.0 } else { -50.0 },
    )?;
    let audible_sweep_authorized = if engine_tools {
        if !args.has("audible-authorized") {
            return Err((
                "--engine tools 需要显式 --audible-authorized（本窗口允许可闻扫频）".to_string(),
                2u8,
            ));
        }
        true
    } else {
        true
    };
    Ok(JobRequest {
        kind: tspi_calibration::job::JobKind::Full,
        label: label.to_string(),
        layout,
        sample_rate,
        design_points: 1,
        holdout_points: 1,
        repeats,
        drive_level_dbfs: drive_level_dbfs as f32,
        audible_sweep_authorized,
        auto_apply,
        expected_applied_version: None,
        inputs: Vec::new(),
    })
}

/// 真实测量配置证据（设备/电平/sweep/UMIK；不做设备操作）。
fn audio_config_json(config: &StageBackendConfig) -> JsonValue {
    let mic = match config.mic.as_ref() {
        Some(mic) => json_object(vec![
            ("device", text(mic.device.clone())),
            ("model", text(mic.model.clone())),
            ("serial", text(mic.serial.clone())),
            ("cal_file", text(mic.cal_file.clone())),
            ("cal_sha256", text(mic.cal_sha256.clone())),
            ("orientation", text(mic.orientation.clone())),
            ("clock_domain", text(mic.clock_domain.clone())),
            ("note", text("本机无 miniDSP 官方序列号校准文件：cal_file/sha256 为声明占位；FR 只做 1 kHz 相对化，不声称绝对 SPL")),
        ]),
        None => JsonValue::Null,
    };
    json_object(vec![
        ("engine", text(config.engine.as_str().to_string())),
        ("mode", text(config.mode.as_str().to_string())),
        ("playback_device", text(config.devices.playback_device.clone())),
        ("playback_channels", JsonValue::UInt(config.devices.playback_channels as u64)),
        ("capture_device", text(config.devices.capture_device.clone())),
        ("capture_channels", JsonValue::UInt(config.devices.capture_channels as u64)),
        ("capture_channel", match config.devices.capture_channel {
            Some(channel) => JsonValue::UInt(channel as u64),
            None => JsonValue::Null,
        }),
        ("sample_rate", JsonValue::UInt(config.devices.sample_rate as u64)),
        ("block_frames", JsonValue::UInt(config.devices.block_frames as u64)),
        ("buffer_frames", JsonValue::UInt(config.devices.buffer_frames as u64)),
        ("settle_ms", JsonValue::UInt(config.devices.settle_ms)),
        ("apply_settle_ms", JsonValue::UInt(config.apply_settle_ms)),
        (
            "remeasure_playback",
            json_object(vec![
                (
                    "mode",
                    text(config.remeasure_playback.as_str().to_string()),
                ),
                (
                    "host_bin",
                    text(config.remeasure_host.host_bin.to_string_lossy().into_owned()),
                ),
                ("host_backend", text(config.remeasure_host.backend.clone())),
                (
                    "host_capture_channels",
                    JsonValue::UInt(config.remeasure_host.capture_channels as u64),
                ),
                (
                    "plan_override",
                    opt_text(
                        config
                            .remeasure_host
                            .plan_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned()),
                    ),
                ),
                (
                    "stereo_2_0_single_point",
                    JsonValue::Bool(config.remeasure_playback == RemeasurePlayback::HostPlan),
                ),
                ("seven_ch_or_4_1_2", text("NOT_IMPLEMENTED".to_string())),
                ("sweep_capture_playback", text("aplay".to_string())),
            ]),
        ),
        ("drive_level_dbfs", JsonValue::Number(config.level.default_drive_level_dbfs)),
        ("hot_level_authorized", JsonValue::Bool(config.level.hot_level_authorized)),
        (
            "sweep",
            json_object(vec![
                ("start_hz", JsonValue::Number(config.sweep.start_hz)),
                ("end_hz", JsonValue::Number(config.sweep.end_hz)),
                ("seconds", JsonValue::Number(config.sweep.seconds)),
                ("repeats", JsonValue::UInt(config.sweep.repeats as u64)),
                ("gap_seconds", JsonValue::Number(config.sweep.repeat_gap_seconds)),
                ("pre_seconds", JsonValue::Number(config.sweep.pre_seconds)),
                ("post_seconds", JsonValue::Number(config.sweep.post_seconds)),
                ("k_main_trim_db", JsonValue::Number(config.sweep.k_main_trim_db)),
            ]),
        ),
        (
            "outputs",
            JsonValue::Array(
                config
                    .outputs
                    .iter()
                    .map(|output| {
                        json_object(vec![
                            ("slot", JsonValue::UInt(output.slot as u64)),
                            ("speaker", text(output.speaker.clone())),
                            ("channel", JsonValue::UInt(output.channel as u64)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("mic", mic),
    ])
}

fn text(value: impl Into<String>) -> JsonValue {
    JsonValue::String(value.into())
}

fn opt_text(value: Option<String>) -> JsonValue {
    match value {
        Some(value) => JsonValue::String(value),
        None => JsonValue::Null,
    }
}

fn artifacts_json(snapshot: &JobSnapshot) -> JsonValue {
    JsonValue::Array(
        snapshot
            .artifacts
            .iter()
            .map(|record| {
                json_object(vec![
                    ("kind", text(record.kind.as_str().to_string())),
                    ("stage", text(record.stage.as_str().to_string())),
                    ("path", text(record.relative_path.clone())),
                    ("sha256", text(record.sha256.clone())),
                    ("bytes", JsonValue::UInt(record.bytes)),
                ])
            })
            .collect(),
    )
}

fn failure_json(snapshot: &JobSnapshot) -> JsonValue {
    match snapshot.failure.as_ref() {
        Some(failure) => json_object(vec![
            ("code", text(failure.code.clone())),
            ("message", text(failure.message.clone())),
            (
                "stage",
                match failure.stage {
                    Some(stage) => text(stage.as_str().to_string()),
                    None => JsonValue::Null,
                },
            ),
        ]),
        None => JsonValue::Null,
    }
}

fn job_json(snapshot: &JobSnapshot) -> String {
    snapshot.to_json()
}

fn host_apply_json(config: Option<&HostApplyConfig>, snapshot: &JobSnapshot) -> JsonValue {
    let Some(config) = config else {
        return json_object(vec![("enabled", JsonValue::Bool(false))]);
    };
    let report_path = config.work_dir.join("host-report.json");
    let report_bytes = std::fs::read(&report_path).ok();
    let report_sha = report_bytes
        .as_ref()
        .map(|bytes| tspi_calibration::job::sha256_hex(bytes));
    let evidence_rel = "apply/host-apply.json";
    let evidence = snapshot
        .artifacts
        .iter()
        .find(|record| record.relative_path == evidence_rel)
        .and_then(|record| {
            std::fs::read(std::path::Path::new(&snapshot.artifacts_dir).join(&record.relative_path))
                .ok()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok());
    json_object(vec![
        ("enabled", JsonValue::Bool(true)),
        ("backend", text(config.backend.clone())),
        ("host_bin", text(config.host_bin.to_string_lossy().into_owned())),
        ("max_blocks", JsonValue::UInt(config.max_blocks)),
        (
            "real_audio",
            JsonValue::Bool(config.is_real_audio()),
        ),
        (
            "work_dir",
            text(config.work_dir.to_string_lossy().into_owned()),
        ),
        (
            "host_report_path",
            text(report_path.to_string_lossy().into_owned()),
        ),
        ("host_report_sha256", opt_text(report_sha)),
        ("evidence_artifact", text(evidence_rel.to_string())),
        ("evidence", opt_text(evidence)),
        (
            "note",
            text(
                "block all-ack 来自真实 tspi-rt-host plan-arena 运行；backend=fake 时无真实声卡/声音（离线接线验证），backend=alsa 属板端窗口。"
                    .to_string(),
            ),
        ),
    ])
}

/// 二次测量播放路径的 summary 段（默认 aplay / host-plan；证据从 job artifacts 反查）。
fn remeasure_playback_json(config: &StageBackendConfig, snapshot: &JobSnapshot) -> JsonValue {
    let evidence: Vec<JsonValue> = snapshot
        .artifacts
        .iter()
        .filter(|record| record.relative_path.contains("-host-plan/"))
        .map(|record| {
            json_object(vec![
                ("kind", text(record.kind.as_str().to_string())),
                ("path", text(record.relative_path.clone())),
                ("sha256", text(record.sha256.clone())),
                ("bytes", JsonValue::UInt(record.bytes)),
            ])
        })
        .collect();
    let host_executed = !evidence.is_empty();
    json_object(vec![
        (
            "mode",
            text(config.remeasure_playback.as_str().to_string()),
        ),
        (
            "host_bin",
            text(config.remeasure_host.host_bin.to_string_lossy().into_owned()),
        ),
        ("host_backend", text(config.remeasure_host.backend.clone())),
        (
            "plan_override",
            opt_text(
                config
                    .remeasure_host
                    .plan_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
        ),
        (
            "stereo_2_0_single_point",
            JsonValue::Bool(config.remeasure_playback == RemeasurePlayback::HostPlan),
        ),
        ("seven_ch_or_4_1_2", text("NOT_IMPLEMENTED".to_string())),
        ("sweep_capture_playback", text("aplay".to_string())),
        ("host_executed", JsonValue::Bool(host_executed)),
        ("host_playback_evidence", JsonValue::Array(evidence)),
        (
            "note",
            text(if config.remeasure_playback == RemeasurePlayback::HostPlan {
                "remeasure/holdout 播放走 tspi-rt-host plan-arena（[capture] source=file + [paths] plan=apply/compile plan）；plan 在测量播放路径里，identity/plan 两次实测差即校正量。声学实测仍须出声窗口（本机离线运行不得声称声学效果）。"
                    .to_string()
            } else {
                "remeasure/holdout 播放仍走 aplay 直放（默认）：测量路径不含 plan，不能从该路径读“校正量”；需要 --remeasure-playback host-plan。"
                    .to_string()
            }),
        ),
    ])
}

fn wait_terminal(manager: &JobManager) -> Result<JobSnapshot, String> {
    manager
        .wait_terminal(Duration::from_secs(600))
        .map_err(|error| format!("wait job terminal: {error}"))
}

fn wait_awaiting_apply(manager: &JobManager) -> Result<JobSnapshot, String> {
    for _ in 0..60_000 {
        let snapshot = manager
            .snapshot()
            .ok_or_else(|| "no job snapshot".to_string())?;
        if snapshot.awaiting_apply {
            return Ok(snapshot);
        }
        if snapshot.state.is_terminal() {
            return Ok(snapshot);
        }
        sleep(Duration::from_millis(2));
    }
    Err("timeout waiting for prepared/awaiting_apply".to_string())
}

/// 仅 synthetic：把目标切到 tilt10 并启用 T64 target FIR（让 P_post 非 identity）。
///
/// 为什么需要：默认 synthetic/measured 设计都关 target FIR（P=identity），compile 出的
/// plan 在 P_post 上没有可听/可测差异，无法证明 host "真的消费 P_post"。用 tilt10 目标 +
/// 平坦实测曲线 ⇒ T64 residual = tilt10 形状 ⇒ plan 的 roles[].p_taps > 1 且频响可测。
/// 用给定的逐通道复延迟重建 T65 输入（magnitude=1，相位 = 2πfτ/fs），使 T65 拟合出
/// 精确的整数 delay_samples（用于 S0/S1b 的"声明延迟 vs 实测延迟"数字域对照）。
fn set_phase_delays(
    design: &mut FitWiringDesign,
    default_delay: f64,
    per_channel: &[(String, f64)],
) -> Result<(), String> {
    let fs = design.plan.sample_rate_hz;
    let grid = log_frequency_grid(80.0, 8_000.0, 1.0 / 24.0);
    let mut rebuilt = Vec::new();
    for response in &design.phase_responses {
        let position = response.position().to_string();
        let channel = response.channel().to_string();
        let delay_samples = per_channel
            .iter()
            .find(|(name, _)| name == &channel)
            .map(|(_, delay)| *delay)
            .unwrap_or(default_delay);
        let points: Vec<PhaseFitPoint> = grid
            .iter()
            .map(|freq| {
                let theta = 2.0 * std::f64::consts::PI * freq * delay_samples / fs as f64;
                PhaseFitPoint::new(*freq, theta.cos(), theta.sin())
            })
            .collect();
        rebuilt.push(
            DePhasedResponse::from_aligned_segment(
                position,
                channel,
                fs,
                points,
                CommonD0::zero(),
            )
            .map_err(|error| format!("rebuild phase response: {error}"))?,
        );
    }
    design.phase_responses = rebuilt;
    Ok(())
}

fn apply_synthetic_tilt(design: &mut FitWiringDesign, taps: usize) -> Result<(), String> {
    design.target = builtin_target(
        BuiltinTarget::Tilt10,
        &BuiltinOptions::new(design.plan.sample_rate_hz as f64),
    )
    .map_err(|error| format!("builtin tilt10 target: {error}"))?;
    // P FIR 是线性相位 FIR（GD=(taps-1)/2）；T66 禁止 D_i 与 GD_p 双扣 → 把 T65 的 role delay
    // 归零并把 L_P 提到 target FIR 抽头数（plan 尺寸随之增大）。
    enable_target_fir_p_post(design, taps)?;
    set_phase_delays(design, 0.0, &[])?;
    Ok(())
}

/// T66 compile 产物摘要（plan.bin + compile-report 的 plan/roles 关键字段）。
fn compile_plan_json(snapshot: &JobSnapshot) -> JsonValue {
    let Some(record) = snapshot
        .artifacts
        .iter()
        .find(|record| record.relative_path == "compile/plan.bin")
    else {
        return json_object(vec![("present", JsonValue::Bool(false))]);
    };
    let artifacts_dir = std::path::Path::new(&snapshot.artifacts_dir);
    let report_path = artifacts_dir.join("compile/compile-report.json");
    let report = std::fs::read(&report_path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| json_parse(&text).ok());
    let plan = report.as_ref().and_then(|document| document.get("plan"));
    let plan_uint = |key: &str| -> JsonValue {
        plan.and_then(|plan| plan.get(key))
            .and_then(|value| value.as_u64())
            .map(JsonValue::UInt)
            .unwrap_or(JsonValue::Null)
    };
    let plan_text = |key: &str| -> JsonValue {
        plan.and_then(|plan| plan.get(key))
            .and_then(|value| value.as_str())
            .map(|value| text(value.to_string()))
            .unwrap_or(JsonValue::Null)
    };
    let roles = report
        .as_ref()
        .and_then(|document| document.get("roles"))
        .and_then(|value| value.as_array())
        .map(|roles| {
            roles
                .iter()
                .map(|role| {
                    let role_uint = |key: &str| -> JsonValue {
                        role.get(key)
                            .and_then(|value| value.as_u64())
                            .map(JsonValue::UInt)
                            .unwrap_or(JsonValue::Null)
                    };
                    json_object(vec![
                        (
                            "role",
                            text(
                                role.get("role_id")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("?")
                                    .to_string(),
                            ),
                        ),
                        ("out_channel", role_uint("out_channel")),
                        ("p_taps", role_uint("p_taps")),
                        ("peq_count", role_uint("peq_count")),
                        ("delay_samples", role_uint("delay_samples")),
                        ("polarity", role_uint("polarity")),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();
    json_object(vec![
        ("present", JsonValue::Bool(true)),
        ("path", text(record.relative_path.clone())),
        ("bytes", JsonValue::UInt(record.bytes)),
        ("sha256", text(record.sha256.clone())),
        (
            "compile_report_path",
            text(report_path.to_string_lossy().into_owned()),
        ),
        (
            "plan",
            json_object(vec![
                ("path_count", plan_uint("path_count")),
                ("diag_count", plan_uint("diag_count")),
                ("role_count", plan_uint("role_count")),
                ("l_g", plan_uint("l_g")),
                ("l_p", plan_uint("l_p")),
                ("max_taps", plan_uint("max_taps")),
                ("chosen_form", plan_text("chosen_form")),
            ]),
        ),
        ("roles", JsonValue::Array(roles)),
        (
            "p_post_note",
            text(
                "roles[].p_taps = T66 P_post 对角 FIR 抽头数：生产默认 P=identity（target_fir.enabled=false，taps=0/短零填充）；实验 --experiment-target-fir 时 = T64 目标曲线 FIR 抽头数（如 512，同时 L_P=taps、force factorized）。measured spec 配 crossover 节（仅实验 + 4.1.2）时 P_post = crossover ⊛ target FIR（如 LR4 80Hz 2048 ⊛ 512 -> composed 2559，diag flags 含 DIAG_FLAG_CROSSOVER bit2，D7 证据绑定卷积后 P 行）。真实 ALSA apply/T3 measured/holdout = NOT_RUN（分频生产验收未完成）。"
                    .to_string(),
            ),
        ),
    ])
}

/// 解析 --phase-delay：单值（全部通道）或 "FL:37,FR:0"（逐通道覆盖）。
fn parse_delay_spec(text: &str) -> Result<(f64, Vec<(String, f64)>), (String, u8)> {
    let parse_one = |value: &str| -> Result<f64, (String, u8)> {
        let delay = value
            .trim()
            .parse::<f64>()
            .map_err(|error| (format!("--phase-delay 项 {value:?} 非法：{error}"), 2u8))?;
        if !(0.0..=4096.0).contains(&delay) {
            return Err(("--phase-delay 只接受 0..=4096 采样".to_string(), 2u8));
        }
        Ok(delay)
    };
    if !text.contains(':') {
        return Ok((parse_one(text)?, Vec::new()));
    }
    let mut per_channel = Vec::new();
    for item in text.split(',') {
        let mut parts = item.split(':');
        let name = parts
            .next()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| (format!("--phase-delay 项 {item:?} 缺通道名"), 2u8))?;
        let value = parts
            .next()
            .ok_or_else(|| (format!("--phase-delay 项 {item:?} 缺延迟值"), 2u8))?;
        per_channel.push((name.to_string(), parse_one(value)?));
    }
    Ok((0.0, per_channel))
}

fn parse_band(text: &str) -> Result<[f64; 2], (String, u8)> {
    let mut parts = text.split(':');
    let lo = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| (format!("频带 {text:?} 非法（A:B Hz）"), 2u8))?;
    let hi = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| (format!("频带 {text:?} 非法（A:B Hz）"), 2u8))?;
    Ok([lo, hi])
}

fn run(args: &Args) -> Result<u8, (String, u8)> {
    let out = args
        .value("out")
        .ok_or_else(|| ("--out DIR is required".to_string(), 2u8))?;
    let out_dir = PathBuf::from(out);
    std::fs::create_dir_all(&out_dir)
        .map_err(|error| (format!("create --out {out:?}: {error}"), 2u8))?;
    let engine_name = args.value("engine").unwrap_or("fake");
    let engine_tools = match engine_name {
        "fake" => false,
        // ToolsEngine = arecord/aplay 真实测量（stage_backend.rs:1245+）；auto 在 Linux 板端等价 tools。
        "tools" | "alsa-tools" | "auto" => true,
        other => {
            return Err((
                format!("--engine {other:?} 不支持（fake|tools|alsa-tools|auto）"),
                2,
            ))
        }
    };
    let design = args.value("design").unwrap_or("synthetic");
    if design != "synthetic" && design != "measured" {
        return Err((
            format!("--design {design:?} 不支持（synthetic|measured）"),
            2,
        ));
    }
    // 实验旁路 flag（**默认全关**；任何实验参数都必须显式带 --experiment-single-point）。
    let experiment_single = args.has("experiment-single-point");
    let experiment_multipoint = args.has("experiment-multipoint-pilot");
    if experiment_single && experiment_multipoint {
        return Err((
            "--experiment-single-point 与 --experiment-multipoint-pilot 互斥（一个产物只声明一种 pilot 形态）"
                .to_string(),
            2u8,
        ));
    }
    let experiment_flag = experiment_single || experiment_multipoint;
    let allow_incomplete = args.has("allow-incomplete-artifact");
    if allow_incomplete && !experiment_flag {
        return Err((
            "--allow-incomplete-artifact 必须与 --experiment-single-point / --experiment-multipoint-pilot 同时使用（生产路径禁止放宽 T54 状态门）"
                .to_string(),
            2u8,
        ));
    }
    if (args.value("g-paths").is_some() || args.value("g-taps").is_some()) && !experiment_single {
        return Err((
            "--g-paths/--g-taps 必须与 --experiment-single-point 同时使用（G 注入是单点实验旁路；生产路径 G 恒为 identity baseline）"
                .to_string(),
            2u8,
        ));
    }
    if experiment_flag && design != "measured" {
        return Err((
            "--experiment-single-point/--experiment-multipoint-pilot 只用于 --design measured"
                .to_string(),
            2u8,
        ));
    }
    let experiment = ExperimentMode {
        single_point: experiment_single,
        multipoint_pilot: experiment_multipoint,
        allow_incomplete_artifact: allow_incomplete,
    };
    let mut measured_layout: Option<String> = None;
    let mut measured_rate: Option<u32> = None;
    let measured = if design == "measured" {
        let measurement = args.value("measurement").ok_or_else(|| {
            (
                "--design measured 需要 --measurement FILE（schema tspi-cal-measured-design/v1）"
                    .to_string(),
                2u8,
            )
        })?;
        let spec =
            load_measured_spec(std::path::Path::new(measurement)).map_err(|error| (error, 2u8))?;
        let build_options = if experiment_flag {
            let g_paths = match args.value("g-paths") {
                Some(path) => {
                    let loaded = load_g_paths_file(std::path::Path::new(path))
                        .map_err(|error| (error, 2u8))?;
                    if let Some(taps) = args.value("g-taps") {
                        let taps: u32 = taps
                            .parse()
                            .map_err(|error| (format!("--g-taps 非法（{taps:?}）：{error}"), 2u8))?;
                        if taps != loaded.l_g {
                            return Err((
                                format!(
                                    "--g-taps {taps} 与 --g-paths 文件 l_g={} 不一致（禁止静默改支撑）",
                                    loaded.l_g
                                ),
                                2u8,
                            ));
                        }
                    }
                    Some(loaded)
                }
                None => None,
            };
            MeasuredBuildOptions {
                experiment,
                layout_override: args.value("layout").map(str::to_string),
                g_paths,
            }
        } else {
            MeasuredBuildOptions::production()
        };
        let built = build_measured_design_opts(&spec, &build_options)
            .map_err(|error| (error, 2u8))?;
        measured_layout = Some(built.effective_layout.clone());
        measured_rate = Some(spec.sample_rate_hz);
        Some(built)
    } else {
        None
    };
    let auto_apply = args.has("apply");
    let fault = args.value("fault").unwrap_or("none");
    if fault == "withhold-ack" && !auto_apply {
        return Err(("--fault withhold-ack requires --apply".to_string(), 2));
    }
    if args.has("apply-host") && fault != "none" {
        return Err((
            "--apply-host 与 --fault 负向注入互斥（host 会给出真实 ack）".to_string(),
            2,
        ));
    }
    let host_apply = if args.has("apply-host") {
        if !auto_apply {
            return Err(("--apply-host requires --apply".to_string(), 2));
        }
        let mut config = HostApplyConfig::default();
        config.work_dir = out_dir.join("host-apply");
        if let Some(host_bin) = args.value("host-bin") {
            config.host_bin = PathBuf::from(host_bin);
        }
        if let Some(backend) = args.value("host-backend") {
            config.backend = backend.to_string();
        }
        if let Some(max_blocks) = args.value("host-max-blocks") {
            config.max_blocks = max_blocks
                .parse::<u64>()
                .map_err(|error| (format!("--host-max-blocks 非法：{error}"), 2))?;
        }
        if let Some(workers) = args.value("host-workers") {
            config.workers = workers
                .parse::<u8>()
                .map_err(|error| (format!("--host-workers 非法：{error}"), 2))?;
        }
        if let Some(cpus) = args.value("host-cpus") {
            config.dsp_cpus = cpus.to_string();
        }
        if let Some(safe_mode) = args.value("host-safe-mode") {
            config.safe_mode = safe_mode.to_string();
        }
        if let Some(engine) = args.value("host-engine") {
            config.engine = engine.to_string();
        }
        config
            .validate()
            .map_err(|error| (format!("host apply 配置非法：{error}"), 2))?;
        Some(config)
    } else {
        None
    };
    if design == "measured" && fault != "none" {
        return Err((
            "--design measured 与 --fault 负向注入互斥（真实测量输入不做合成故障注入）".to_string(),
            2,
        ));
    }
    let label = args.value("label").unwrap_or("tspi-cal-dryrun");
    let mut design_input: FitWiringDesign = match measured.as_ref() {
        Some(measured) => measured.design.clone(),
        None => synthetic_design().map_err(|error| (format!("synthetic design: {error}"), 2))?,
    };
    // 实验模式必须一路带到 compile：FitWiring 据此放宽 FitP/T65 门，compile 据此
    // 放宽 T54 状态门并给产物打 EXPERIMENT/NON_ACCEPTABLE（生产恒 OFF）。
    if let Some(measured) = measured.as_ref() {
        design_input.compile.experiment = measured.experiment;
    }
    if args.has("compile-force-factorized") {
        if !experiment_flag {
            return Err((
                "--compile-force-factorized 必须与实验 flag 同时使用（生产 compile 按 T12 成本选择形态）"
                    .to_string(),
                2,
            ));
        }
        design_input.compile.options.force_form =
            Some(tspi_dsp_core::compile::FusionForm::Factorized);
        eprintln!(
            "EXPERIMENT: compile force_form=Factorized（host plan-arena 当前只接受 factorized；fused NOT_WIRED）"
        );
    }
    if args.has("experiment-target-fir") {
        if measured.is_none() {
            return Err(("--experiment-target-fir 只用于 --design measured".to_string(), 2));
        }
        if !experiment_single {
            return Err((
                "--experiment-target-fir 必须与 --experiment-single-point 同时使用（多点 pilot 不启用该旁路）"
                    .to_string(),
                2,
            ));
        }
        let taps = arg_u64(args, "target-fir-taps", 512)? as usize;
        // 联动逻辑收口在库函数（L_P 同步 / F-16 零延迟 / factorized 强制 / experiment 强制），
        // 单测见 tests/T67/fit_wiring.rs：t67_wiring_target_fir_*。
        let outcome =
            apply_measured_target_fir_experiment(&mut design_input, taps, experiment)
                .map_err(|error| (error, 2u8))?;
        eprintln!(
            "EXPERIMENT: measured target FIR enabled (taps={}) — P_post 含目标曲线 FIR（l_p={} / force_form=factorized / zero_phase_delay={}）；产物 NON_ACCEPTABLE",
            outcome.taps, outcome.taps, outcome.zero_phase_delay
        );
    }
    if let Some(wiring) = design_input.crossover.as_ref() {
        eprintln!(
            "EXPERIMENT: measured crossover P_post enabled (kind={} fc={}Hz taps={} assignments={} composed_taps={} l_p={} zero_phase_delay={}) — P_post=crossover⊛target FIR；产物 NON_ACCEPTABLE，真实 ALSA/T3/holdout NOT_RUN",
            wiring.design.kind.as_str(),
            wiring.design.spec.fc_hz,
            wiring.config.taps,
            wiring.assignments.len(),
            design_input.crossover_required_taps().unwrap_or(0),
            design_input.compile.l_p,
            design_input.zero_phase_delay,
        );
    }
    if args.has("target-tilt10") {
        if measured.is_some() {
            return Err((
                "--target-tilt10 只用于 --design synthetic（measured 设计的 target 来自 spec）"
                    .to_string(),
                2,
            ));
        }
        let taps = arg_u64(args, "target-fir-taps", 512)? as usize;
        if !(8..=4096).contains(&taps) {
            return Err(("--target-fir-taps 只接受 8..=4096".to_string(), 2));
        }
        apply_synthetic_tilt(&mut design_input, taps).map_err(|error| (error, 2))?;
    }
    if let Some(delay) = args.value("phase-delay") {
        if measured.is_some() {
            return Err(("--phase-delay 只用于 --design synthetic".to_string(), 2));
        }
        let (default_delay, per_channel) = parse_delay_spec(delay)?;
        set_phase_delays(&mut design_input, default_delay, &per_channel)
            .map_err(|error| (error, 2))?;
    }
    if let Some(band) = args.value("peq-modal-band") {
        design_input.peq_options.modal_band_hz = parse_band(band)?;
    }
    if let Some(band) = args.value("peq-wide-band") {
        design_input.peq_options.wide_band_hz = parse_band(band)?;
    }
    if let Some(stages) = args.value("max-peq-stages") {
        let stages = stages
            .parse::<usize>()
            .map_err(|error| (format!("--max-peq-stages 非法：{error}"), 2))?;
        if !(1..=32).contains(&stages) {
            return Err(("--max-peq-stages 只接受 1..=32".to_string(), 2));
        }
        design_input.peq_options.max_total_stages = stages;
    }
    match fault {
        "none" => {}
        "fit-gate" => {
            let quality = design_input
                .design_quality
                .as_mut()
                .expect("synthetic design has quality");
            quality.continue_allowed = false;
            quality.reasons = vec!["E_POS_SNR_LOW".to_string()];
        }
        "bad-artifact" => {
            design_input.artifact.status = "INCOMPLETE".to_string();
        }
        "withhold-ack" => {}
        other => {
            return Err((
                format!("--fault {other:?} 不支持（none|fit-gate|bad-artifact|withhold-ack）"),
                2,
            ));
        }
    }
    let mut wiring = FitWiring::new(design_input)
        .map_err(|error| (format!("fit-wiring design invalid: {error}"), 2))?;
    if fault == "withhold-ack" {
        wiring = wiring.with_ack_source(Box::new(WithheldAck));
    }
    if let Some(host_apply) = host_apply.clone() {
        wiring = wiring.with_host_apply(host_apply);
    }
    let manager = JobManager::new(
        JobManagerConfig {
            artifacts_root: out_dir.join("jobs"),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(OfflineSafety),
    )
    .map_err(|error| (format!("job manager: {error}"), 2))?;
    let mut backend_config = if engine_tools {
        tools_stage_config(args, label)?
    } else {
        fake_stage_config(label)
    };
    apply_remeasure_playback_args(
        args,
        &mut backend_config,
        host_apply.as_ref().map(|config| config.work_dir.clone()),
        auto_apply,
    )?;
    let rt60_stage = rt60_stage_config(args)?;
    let backend = StageBackend::from_config(backend_config.clone())
        .map_err(|error| (format!("stage backend: {error}"), 2))?
        .with_delegate(Box::new(wiring))
        .with_rt60(rt60_stage.clone());
    let request = job_request(
        args,
        label,
        auto_apply,
        engine_tools,
        measured_layout.as_deref(),
        measured_rate,
    )?;
    let job_id = manager
        .start(request, Box::new(backend))
        .map_err(|error| (format!("start job: {error}"), 2))?;
    let mut rolled_back_before_apply = false;
    let snapshot = if auto_apply {
        wait_terminal(&manager).map_err(|error| (error, 4))?
    } else {
        let prepared = wait_awaiting_apply(&manager).map_err(|error| (error, 4))?;
        if prepared.awaiting_apply {
            manager
                .rollback()
                .map_err(|error| (format!("rollback prepared job: {error}"), 4))?;
            rolled_back_before_apply = true;
        }
        wait_terminal(&manager).map_err(|error| (error, 4))?
    };
    std::fs::write(out_dir.join("job.json"), job_json(&snapshot))
        .map_err(|error| (format!("write job.json: {error}"), 4))?;
    let summary = json_object(vec![
        ("schema", text("tspi-cal-cli/v1")),
        ("command", text("run")),
        ("job_id", text(job_id.to_string())),
        ("label", text(label)),
        (
            "design_source",
            text(if measured.is_some() {
                "measured"
            } else {
                "synthetic_offline"
            }),
        ),
        ("measured", JsonValue::Bool(measured.is_some())),
        ("simulated", JsonValue::Bool(!engine_tools && measured.is_none())),
        (
            "engine",
            text(if engine_tools {
                "alsa-tools (arecord/aplay)"
            } else {
                "fake-synth"
            }),
        ),
        ("audio_config", audio_config_json(&backend_config)),
        (
            "remeasure_playback",
            remeasure_playback_json(&backend_config, &snapshot),
        ),
        ("rt60", rt60_summary_json(&snapshot, &rt60_stage)),
        (
            "measured_design",
            match measured.as_ref() {
                Some(measured) => text(measured.summary_json.clone()),
                None => JsonValue::Null,
            },
        ),
        (
            "experiment",
            match measured.as_ref() {
                Some(measured) if measured.experiment.is_active() => json_object(vec![
                    ("active", JsonValue::Bool(true)),
                    ("verdict", text("EXPERIMENT")),
                    ("acceptability", text("NON_ACCEPTABLE")),
                    ("applied", JsonValue::Bool(false)),
                    ("certifiable", JsonValue::Bool(false)),
                    ("scope", text(measured.experiment.scope().to_string())),
                    (
                        "single_point",
                        JsonValue::Bool(measured.experiment.single_point),
                    ),
                    (
                        "multipoint_pilot",
                        JsonValue::Bool(measured.experiment.multipoint_pilot),
                    ),
                    (
                        "allow_incomplete_artifact",
                        JsonValue::Bool(measured.experiment.allow_incomplete_artifact),
                    ),
                    (
                        "compile_force_factorized",
                        JsonValue::Bool(args.has("compile-force-factorized")),
                    ),
                    ("effective_layout", text(measured.effective_layout.clone())),
                    ("g_source", text(measured.g_source.clone())),
                    ("g_paths_sha256", text(measured.g_paths_sha256.clone())),
                    (
                        "envelope_artifact",
                        text("compile/experiment-envelope.json".to_string()),
                    ),
                    (
                        "note",
                        text(format!(
                            "实验旁路产物：EXPERIMENT / NON_ACCEPTABLE / applied=false；不得写成 READY/PASS；scope={}（{}）",
                            measured.experiment.scope(),
                            if measured.experiment.multipoint_pilot {
                                "多点 pilot 只做接线/预测评估，不产生 T6/holdout 通过结论"
                            } else {
                                "不产生空间/多点结论"
                            }
                        )),
                    ),
                ]),
                _ => JsonValue::Null,
            },
        ),
        (
            "board_run",
            text(if engine_tools { "REAL_AUDIO" } else { "NOT_RUN" }),
        ),
        (
            "real_apply",
            text(match host_apply.as_ref() {
                Some(config) if config.is_real_audio() => "REAL_HOST_PLAN_ARENA_ALSA",
                Some(_) => "HOST_PLAN_ARENA_FAKE_OFFLINE",
                None => "OFFLINE_SYNTHETIC_ACK",
            }),
        ),
        (
            "t3_applied_measurement",
            text(if !engine_tools {
                "NOT_RUN"
            } else if backend_config.remeasure_playback == RemeasurePlayback::HostPlan {
                // 播放经 plan-arena plan（测量路径带 plan）；但生产 systemd host 仍不消费该
                // plan，且计划只做“测量回放”而非“生产链路生效”，不得写成 REMETEST_APPLIED。
                "REMETEST_MEASURED_THROUGH_PLAN_ARENA_HOST"
            } else {
                "REMETEST_MEASURED_NOT_APPLIED_TO_AUDIO"
            }),
        ),
        ("compile_plan", compile_plan_json(&snapshot)),
        (
            "t3_note",
            text(if backend_config.remeasure_playback == RemeasurePlayback::HostPlan {
                "remeasure/holdout 播放已走 tspi-rt-host plan-arena（T61 timeline → 2ch WAV → [capture] source=file），测量路径真的经过 apply/compile 的 plan；identity 与 plan 两次实测之差即“校正量”。生产 systemd host 是否消费该 plan 是另一条链（不改生产路径）；声学实测须在出声窗口执行，本机离线运行不得声称已测到。"
                    .to_string()
            } else {
                "remeasure 是再测一次真实音频（measured）；但默认播放仍走 aplay 直放（路径不含 plan），故不能从该路径读“校正量”：需要 --remeasure-playback host-plan。T3 结论 NOT_CONFIRMED，不得写成校正生效。"
                    .to_string()
            }),
        ),
        ("auto_apply", JsonValue::Bool(auto_apply)),
        ("fault", text(fault)),
        ("rolled_back_before_apply", JsonValue::Bool(rolled_back_before_apply)),
        ("state", text(snapshot.state.as_str().to_string())),
        ("terminal_reason", opt_text(snapshot.terminal_reason.clone())),
        (
            "applied_version",
            opt_text(snapshot.applied_version.clone()),
        ),
        ("failure", failure_json(&snapshot)),
        ("artifacts", artifacts_json(&snapshot)),
        ("artifacts_dir", text(snapshot.artifacts_dir.clone())),
        ("host_apply", host_apply_json(host_apply.as_ref(), &snapshot)),
    ]);
    let summary_text =
        json_encode(&summary).map_err(|error| (format!("encode summary.json: {error}"), 4))?;
    std::fs::write(out_dir.join("summary.json"), &summary_text)
        .map_err(|error| (format!("write summary.json: {error}"), 4))?;
    let applied = snapshot
        .applied_version
        .clone()
        .map(|version| format!("{version:?}"))
        .unwrap_or_else(|| "null".to_string());
    println!(
        "{{\"command\":\"run\",\"job\":\"{job_id}\",\"state\":\"{}\",\"applied_version\":{applied},\"out\":{:?}}}",
        snapshot.state.as_str(),
        out
    );
    if let Ok(plan_text) = json_encode(&compile_plan_json(&snapshot)) {
        println!("{{\"compile_plan\":{plan_text}}}");
    }
    // 无 --apply 的 dry-run 在 prepared 后主动 rollback 是设计行为（不是失败）。
    let failed = if rolled_back_before_apply {
        false
    } else {
        matches!(
            snapshot.state,
            tspi_calibration::job::JobState::Failed
                | tspi_calibration::job::JobState::RolledBack
                | tspi_calibration::job::JobState::Cancelled
        )
    };
    if failed {
        eprintln!(
            "status=FAIL code={} stage={:?} reason={:?}",
            snapshot
                .failure
                .as_ref()
                .map(|failure| failure.code.as_str())
                .unwrap_or("NONE"),
            snapshot.failure.as_ref().and_then(|failure| failure.stage),
            snapshot.terminal_reason
        );
        return Ok(4);
    }
    Ok(0)
}


// ---------------------------------------------------------------------------
// goal-6：RT60/CSD 后处理（CLI 配置 + summary + 离线 IR 分析子命令）
// ---------------------------------------------------------------------------

fn parse_f64_list(text: &str, key: &str) -> Result<Vec<f64>, (String, u8)> {
    let mut out = Vec::new();
    for token in text.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let value = token
            .parse::<f64>()
            .map_err(|error| (format!("--{key} 项 {token:?} 非法：{error}"), 2u8))?;
        out.push(value);
    }
    if out.is_empty() {
        return Err((format!("--{key} 至少需要一个数值"), 2u8));
    }
    Ok(out)
}

fn parse_f64_arg(args: &Args, key: &str, target: &mut f64) -> Result<(), (String, u8)> {
    if let Some(text) = args.value(key) {
        *target = text
            .parse::<f64>()
            .map_err(|error| (format!("--{key} 非法（{text:?}）：{error}"), 2u8))?;
    }
    Ok(())
}

fn parse_comma_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .collect()
}

/// 从 CLI 参数构建 RT60 stage 配置（默认为默认配置 = 开启；--no-rt60 关闭）。
fn rt60_stage_config(args: &Args) -> Result<Rt60StageConfig, (String, u8)> {
    if args.has("no-rt60") {
        if args.value("rt60-ir").is_some() {
            return Err(("--no-rt60 与 --rt60-ir 互斥".to_string(), 2u8));
        }
        return Ok(Rt60StageConfig::disabled());
    }
    let mut config = Rt60StageConfig::default();
    parse_f64_arg(args, "rt60-window-ms", &mut config.window_ms)?;
    parse_f64_arg(args, "rt60-csd-window-ms", &mut config.csd_window_ms)?;
    parse_f64_arg(args, "rt60-tail-ms", &mut config.tail_ms)?;
    if let Some(text) = args.value("rt60-bands") {
        config.bands_hz = parse_f64_list(text, "rt60-bands")?;
    }
    if let Some(text) = args.value("rt60-ir") {
        let paths = parse_comma_list(text);
        if paths.is_empty() {
            return Err(("--rt60-ir 至少需要一个 WAV 文件".to_string(), 2u8));
        }
        let labels = args
            .value("rt60-label")
            .map(parse_comma_list)
            .unwrap_or_default();
        if !labels.is_empty() && labels.len() != paths.len() {
            return Err(("--rt60-label 数量必须与 --rt60-ir 一致".to_string(), 2u8));
        }
        for (index, path_text) in paths.iter().enumerate() {
            let path = PathBuf::from(path_text);
            if !path.is_file() {
                return Err((format!("--rt60-ir 文件不存在：{}", path.display()), 2u8));
            }
            let label = labels.get(index).cloned().unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("IR")
                    .to_string()
            });
            config.external_irs.push(Rt60ExternalIr { label, path });
        }
    }
    config
        .validate()
        .map_err(|failure| (format!("RT60 配置非法：{} {}", failure.code, failure.message), 2u8))?;
    Ok(config)
}

/// run 子命令 summary.json 的 rt60 段（如实标注观察量/离线态）。
fn rt60_summary_json(snapshot: &JobSnapshot, config: &Rt60StageConfig) -> JsonValue {
    let mut paths: Vec<JsonValue> = Vec::new();
    for artifact in &snapshot.artifacts {
        let path = artifact.relative_path.as_str();
        let is_rt60 = path.starts_with("rt60/")
            || path.starts_with("csd/")
            || path.contains("/rt60/")
            || path.contains("/csd/");
        if is_rt60 {
            paths.push(text(path.to_string()));
        }
    }
    let external: Vec<JsonValue> = config
        .external_irs
        .iter()
        .map(|ir| {
            json_object(vec![
                ("label", text(ir.label.clone())),
                ("path", text(ir.path.display().to_string())),
            ])
        })
        .collect();
    json_object(vec![
        ("enabled", JsonValue::Bool(config.enabled)),
        ("default_on", JsonValue::Bool(true)),
        ("external_irs", JsonValue::Array(external)),
        ("artifacts", JsonValue::Array(paths)),
        (
            "real_room_measurement",
            text("NOT_RUN (offline run: no real room measurement performed)"),
        ),
        (
            "discipline",
            text("RT60/CSD are observation/report artifacts; NOT an apply pass/fail gate"),
        ),
    ])
}

/// 离线 IR 分析子命令：tspi-cal rt60 --ir FILES --out DIR [--label NAMES] ...
fn rt60_run(args: &Args) -> Result<u8, (String, u8)> {
    let out_dir = PathBuf::from(
        args.value("out")
            .ok_or(("rt60 需要 --out DIR".to_string(), 2u8))?,
    );
    let ir_text = args
        .value("ir")
        .ok_or(("rt60 需要 --ir FILE[,FILE...]".to_string(), 2u8))?;
    let paths = parse_comma_list(ir_text);
    if paths.is_empty() {
        return Err(("--ir 至少需要一个 WAV 文件".to_string(), 2u8));
    }
    let labels = args
        .value("label")
        .map(parse_comma_list)
        .unwrap_or_default();
    if !labels.is_empty() && labels.len() != paths.len() {
        return Err(("--label 数量必须与 --ir 一致".to_string(), 2u8));
    }
    let mut config = Rt60Config::default();
    parse_f64_arg(args, "window-ms", &mut config.window_ms)?;
    parse_f64_arg(args, "csd-window-ms", &mut config.csd_window_ms)?;
    if let Some(text) = args.value("bands") {
        config.bands_hz = parse_f64_list(text, "bands")?;
    }
    config
        .validate()
        .map_err(|message| (format!("RT60 配置非法：{message}"), 2u8))?;
    let mut entries: Vec<Rt60AnalyzedIr> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (index, path_text) in paths.iter().enumerate() {
        let path = PathBuf::from(path_text);
        let label = labels.get(index).cloned().unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("IR")
                .to_string()
        });
        match analyze_wav_path(&path, &label, &config) {
            Ok((analysis, bytes)) => entries.push(Rt60AnalyzedIr {
                analysis,
                source_path: Some(path.display().to_string()),
                ir_sha256: Some(tspi_calibration::job::sha256_hex(&bytes)),
                ir_wav_path: None,
                repeats_averaged: 1,
            }),
            Err(error) => errors.push(format!("{label}: {error}")),
        }
    }
    if entries.is_empty() {
        return Err((
            format!("所有 IR 分析失败：{}", errors.join("; ")),
            2u8,
        ));
    }
    std::fs::create_dir_all(&out_dir)
        .map_err(|error| (format!("mkdir {}: {error}", out_dir.display()), 2u8))?;
    let csd_dir = out_dir.join("csd");
    std::fs::create_dir_all(&csd_dir)
        .map_err(|error| (format!("mkdir {}: {error}", csd_dir.display()), 2u8))?;
    let report = Rt60Report {
        config,
        source: Rt60Source {
            kind: "external_ir",
            stage: "offline_cli".to_string(),
            engine: "offline".to_string(),
            simulated: true,
            real_measurement: "NOT_RUN",
            ir_origin:
                "tspi-cal rt60 offline CLI: external deconvolved IR WAVs (no device, no audio)"
                    .to_string(),
            capture_sha256: None,
            sweep_start_hz: None,
            sweep_end_hz: None,
            sweep_seconds: None,
        },
        generated_unix: unix_now(),
        irs: entries,
        errors: errors.clone(),
    };
    std::fs::write(out_dir.join("summary.json"), report.summary_json())
        .map_err(|error| (format!("write summary.json: {error}"), 2u8))?;
    std::fs::write(out_dir.join("rt60.csv"), report.rt60_csv())
        .map_err(|error| (format!("write rt60.csv: {error}"), 2u8))?;
    for entry in &report.irs {
        let name = tspi_calibration::rt60::safe_artifact_label(&entry.analysis.label);
        std::fs::write(
            csd_dir.join(format!("{name}.json")),
            csd_json(entry, report.config.csd_window_ms),
        )
        .map_err(|error| (format!("write csd/{name}.json: {error}"), 2u8))?;
    }
    if !errors.is_empty() {
        std::fs::write(
            out_dir.join("warning.json"),
            warning_json(
                "offline_cli",
                "RT60 offline CLI partially failed; successful IRs are recorded",
                &errors,
            ),
        )
        .map_err(|error| (format!("write warning.json: {error}"), 2u8))?;
    }
    let bands_total: usize = report.irs.iter().map(|e| e.analysis.bands.len()).sum();
    let bands_valid: usize = report
        .irs
        .iter()
        .flat_map(|e| e.analysis.bands.iter())
        .filter(|band| band.valid)
        .count();
    let valid_in_range = report
        .irs
        .iter()
        .flat_map(|e| e.analysis.bands.iter())
        .filter(|band| band.valid)
        .filter(|band| band.rt60_s.map(|rt| (0.2..=0.8).contains(&rt)).unwrap_or(false))
        .count();
    let summary = json_object(vec![
        ("schema", text("tspi-cal-cli-rt60/v1")),
        ("command", text("rt60")),
        ("out", text(out_dir.display().to_string())),
        ("irs", JsonValue::Array(report.irs.iter().map(|e| text(e.analysis.label.clone())).collect())),
        ("bands_total", JsonValue::UInt(bands_total as u64)),
        ("bands_valid", JsonValue::UInt(bands_valid as u64)),
        ("bands_valid_0p2_0p8", JsonValue::UInt(valid_in_range as u64)),
        ("errors", JsonValue::UInt(errors.len() as u64)),
        (
            "real_room_measurement",
            text("NOT_RUN (offline analysis of supplied IR WAVs only)"),
        ),
        (
            "note",
            text("RT60/CSD are observation values; valid=false rows are traceability only"),
        ),
    ]);
    if let Ok(line) = json_encode(&summary) {
        println!("{line}");
    }
    Ok(0)
}

fn main_entry() -> Result<u8, (String, u8)> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw
        .iter()
        .any(|argument| argument == "--help" || argument == "-h" || argument == "help")
    {
        println!("{USAGE}");
        return Ok(0);
    }
    let args = Args::parse(&raw).map_err(|message| (message, 2))?;
    match args.command.as_str() {
        "run" => run(&args),
        "rt60" => rt60_run(&args),
        other => Err((format!("unknown command {other:?} (run|rt60)"), 2)),
    }
}

fn host_client(command: &str, args: &[String]) -> ExitCode {
    let audible = args.iter().any(|item| item == "--audible-authorized");
    let mut sock = tspi_rt_host::cal_control::socket_path();
    let mut f_hi = String::new();
    let mut target = String::new();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--sock" => {
                sock = args.get(index + 1).cloned().unwrap_or_default();
                index += 2;
            }
            "--f-hi" => {
                f_hi = args.get(index + 1).cloned().unwrap_or_default();
                index += 2;
            }
            "--target" => {
                target = args.get(index + 1).cloned().unwrap_or_default();
                index += 2;
            }
            _ => index += 1,
        }
    }
    let audible_json = if audible { "true" } else { "false" };
    let mut request = format!(r#"{{"op":"{command}","audible":{audible_json}"#);
    if !f_hi.is_empty() {
        request.push_str(&format!(r#","f_hi":{f_hi}"#));
    }
    if !target.is_empty() {
        request.push_str(&format!(r#","target":"{target}""#));
    }
    request.push('}');
    match tspi_rt_host::cal_control::exchange(&sock, &request) {
        Ok(line) => {
            println!("{line}");
            if line.contains("\"ok\":true") {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(3)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(command) = args.first() {
        if matches!(
            command.as_str(),
            "status" | "enter" | "measure" | "solve" | "remeasure" | "leave"
        ) {
            return host_client(command, &args[1..]);
        }
    }
    match main_entry() {
        Ok(code) => ExitCode::from(code),
        Err((message, code)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            ExitCode::from(code)
        }
    }
}
