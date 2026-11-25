//! [NONRT CLI] T67 前置：真实校准 stage backend 板端命令（MOTU 播放 + UMIK/MOTU 参考通道采集）。
//!
//! 板端 smoke（授权窗口；48k、−50 dBFS 默认、禁 192k）：
//!
//!   khaen-cal-stage preflight --mode acoustic --playback-device hw:4,0 \
//!     --capture-device hw:3,0 --mic-device hw:3,0 --mic-serial 718-7696 \
//!     --mic-cal-file /var/lib/khaen-calib/umik-90deg.txt \
//!     --mic-cal-sha256 <64 hex> --mic-orientation 90deg
//!
//!   khaen-cal-stage measure --smoke --out /var/lib/khaen-calib/t67-smoke \
//!     --mode acoustic --playback-device hw:4,0 --playback-channels 22 \
//!     --capture-device hw:3,0 --capture-channels 1 \
//!     --mic-device hw:3,0 --mic-serial 718-7696 \
//!     --mic-cal-file /var/lib/khaen-calib/umik-90deg.txt \
//!     --mic-cal-sha256 <64 hex> --mic-orientation 90deg \
//!     --outputs 0:FL,1:FR --layout 4.1.2 --level-db -50
//!
//! 电学环回（T1 抽检；MOTU 参考麦 ch0 同源，例如播放 out9、采集 in7 时 capture-channel=6）：
//!
//!   khaen-cal-stage measure --out /var/lib/khaen-calib/t1-smoke --mode loopback \
//!     --playback-device hw:4,0 --playback-channels 22 \
//!     --capture-device hw:4,0 --capture-channels 20 --capture-channel 6 \
//!     --outputs 8:OUT9 --layout 4.1.2 --level-db -12 --seconds 5 --repeats 1
//!
//! 本机干跑（离线合成；证据必须标 simulated，不得当板端实测）：
//!
//!   khaen-cal-stage measure --engine fake --smoke --out scratch/stage-smoke-dryrun \
//!     --mode acoustic --mic-serial SIM --mic-cal-sha256 <64 hex> --mic-cal-file sim.txt
//!
//! 退出码：0 = 通过；2 = 用法/配置错误；3 = 设备不可用（NOT_RUN 口径）；4 = 保护/质量门失败。

use std::path::PathBuf;
use std::process::ExitCode;

use khaen_calibration::job::sha256_hex;
use khaen_calibration::stage_backend::{
    run_board_measurement, EngineKind, StageBackendConfig, StageDeviceConfig, StageMeasurementMode,
    StageSweepConfig, STAGE_DEFAULT_DRIVE_LEVEL_DBFS, STAGE_FORBIDDEN_SAMPLE_RATES_HZ,
    STAGE_UMIK_SAMPLE_RATE_HZ,
};

const USAGE: &str = "\
khaen-cal-stage preflight [--engine auto|fake|tools] [device flags]
khaen-cal-stage measure --out DIR [--engine auto|fake|tools] [device/sweep/mic flags]

常见选项：
  --mode acoustic|loopback        测量模式（默认 acoustic = UMIK-1）
  --playback-device hw:4,0        MOTU 播放设备（默认 hw:4,0）
  --playback-channels 22          MOTU 播放通道（默认 22）
  --capture-device hw:3,0         采集设备（声学 UMIK 默认 hw:3,0；环回用 hw:4,0）
  --capture-channels 1            采集读块通道（声学 1；MOTU 环回 20）
  --capture-channel N             MOTU 环回参考输入通道（0-based；声学禁用）
  --rate 48000                    采样率（192k 一律拒绝；UMIK 仅 48000）
  --block-frames 128              period 帧数（64/128）
  --buffer-frames 512             buffer 帧数（period 整数倍）
  --settle-ms 1200                采集先起的稳定等待
  --level-db -50                  驱动电平 dBFS（默认 -50；> -30 需 --hot-level-authorized）
  --hot-level-authorized          显式授权“更大音量”（仅白天窗口）
  --outputs 0:FL,1:FR             逐输出 channel:SPEAKER（顺序=播放顺序）
  --layout 4.1.2                  layout id（证据）
  --seconds 5 --repeats 3         扫频参数（--smoke 改为 1 s / 1 遍）
  --smoke                         板端 smoke 短会话（1 s、1 遍、gap 0.2、pre 0.75）
  --mic-serial 718-7696           UMIK 序列号（声学必填）
  --mic-cal-file <path>           UMIK 校准文件（声学必填；用于记录/校验 sha256）
  --mic-cal-sha256 <64 hex>       校准文件 sha256（给文件时自动复算校验）
  --mic-orientation 90deg         麦朝向（默认 90deg = 听音位多通道）
  --session-label stage           证据 session 前缀
  --out DIR                       证据输出目录（measure 必填）
";

#[derive(Debug)]
struct Args {
    command: String,
    values: std::collections::BTreeMap<String, String>,
    flags: std::collections::BTreeSet<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut command = String::new();
        let mut values = std::collections::BTreeMap::new();
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
                "smoke" | "hot-level-authorized" | "help" | "h"
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
            return Err("missing command (preflight|measure)".to_string());
        }
        Ok(Args {
            command,
            values,
            flags,
        })
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|v| v.as_str())
    }

    fn or_default(&self, key: &str, default: &str) -> String {
        self.value(key).unwrap_or(default).to_string()
    }

    fn parse_or<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.value(key) {
            Some(text) => text
                .parse::<T>()
                .map_err(|_| format!("--{key} has invalid value {text:?}")),
            None => Ok(default),
        }
    }

    fn has(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
}

fn parse_outputs(text: &str) -> Result<Vec<khaen_calibration::sweep::SweepOutput>, String> {
    let mut outputs = Vec::new();
    for (slot, token) in text.split(',').enumerate() {
        let token = token.trim();
        if token.is_empty() {
            return Err("--outputs contains an empty entry".to_string());
        }
        let (channel, speaker) = token
            .split_once(':')
            .ok_or_else(|| format!("--outputs entry {token:?} must be channel:SPEAKER"))?;
        let channel: u32 = channel
            .parse()
            .map_err(|_| format!("--outputs channel {channel:?} is not a number"))?;
        if speaker.trim().is_empty() {
            return Err("--outputs speaker must be non-empty".to_string());
        }
        outputs.push(khaen_calibration::sweep::SweepOutput::new(
            slot as u32,
            speaker.trim(),
            channel,
        ));
    }
    if outputs.is_empty() {
        return Err("--outputs must name at least one output".to_string());
    }
    Ok(outputs)
}

fn build_config(args: &Args) -> Result<StageBackendConfig, String> {
    let mode = StageMeasurementMode::parse(args.value("mode").unwrap_or("acoustic"))
        .ok_or_else(|| "--mode must be acoustic or loopback".to_string())?;
    let engine = EngineKind::parse(args.value("engine").unwrap_or("auto"))
        .ok_or_else(|| "--engine must be auto|fake|tools".to_string())?;
    let rate: u32 = args.parse_or("rate", STAGE_UMIK_SAMPLE_RATE_HZ)?;
    if STAGE_FORBIDDEN_SAMPLE_RATES_HZ.contains(&rate) {
        return Err("192000 Hz is explicitly forbidden (task hard rule)".to_string());
    }
    let block_frames: u16 = args.parse_or("block-frames", 128u16)?;
    let buffer_frames: u32 = args.parse_or("buffer-frames", 512u32)?;
    let settle_ms: u64 = args.parse_or("settle-ms", 1_200u64)?;
    let capture_channel: Option<u16> = match args.value("capture-channel") {
        Some(text) => Some(
            text.parse()
                .map_err(|_| format!("--capture-channel has invalid value {text:?}"))?,
        ),
        None => None,
    };
    let outputs = parse_outputs(args.value("outputs").unwrap_or("0:FL,1:FR"))?;
    let mut sweep = if args.has("smoke") {
        StageSweepConfig::smoke()
    } else if mode == StageMeasurementMode::ElectricalLoopback {
        StageSweepConfig::loopback()
    } else {
        StageSweepConfig::default()
    };
    sweep.seconds = args.parse_or("seconds", sweep.seconds)?;
    sweep.repeats = args.parse_or("repeats", sweep.repeats)?;
    sweep.repeat_gap_seconds = args.parse_or("gap", sweep.repeat_gap_seconds)?;
    sweep.pre_seconds = args.parse_or("pre", sweep.pre_seconds)?;
    sweep.post_seconds = args.parse_or("post", sweep.post_seconds)?;
    sweep.fade_ms = args.parse_or("fade-ms", sweep.fade_ms)?;
    sweep.k_main_trim_db = args.parse_or("trim-db", sweep.k_main_trim_db)?;

    let level_db: f64 = args.parse_or("level-db", STAGE_DEFAULT_DRIVE_LEVEL_DBFS)?;
    let devices = StageDeviceConfig {
        playback_device: args.or_default("playback-device", "hw:4,0"),
        playback_channels: args.parse_or("playback-channels", 22u16)?,
        capture_device: args.or_default("capture-device", "hw:3,0"),
        capture_channels: args.parse_or("capture-channels", 1u16)?,
        capture_channel,
        sample_rate: rate,
        block_frames,
        buffer_frames,
        settle_ms,
        sample_format: khaen_core::abi::sample_format::S24_3LE_INTERLEAVED,
    };

    let mic = if mode == StageMeasurementMode::AcousticUmik {
        let serial = args
            .value("mic-serial")
            .ok_or_else(|| "--mic-serial is required for acoustic UMIK mode".to_string())?
            .to_string();
        let mic_device = args.or_default(
            "mic-device",
            args.value("capture-device").unwrap_or("hw:3,0"),
        );
        let (cal_file, cal_sha256) = resolve_mic_calibration(args)?;
        Some(StageBackendConfig::umik1_declaration(
            mic_device,
            serial,
            cal_file,
            cal_sha256,
            args.or_default("mic-orientation", "90deg"),
        ))
    } else {
        None
    };
    let mut level = khaen_calibration::stage_backend::StageLevelConfig::default();
    level.default_drive_level_dbfs = level_db;
    level.hot_level_authorized = args.has("hot-level-authorized");
    Ok(StageBackendConfig {
        mode,
        devices,
        mic,
        level,
        sweep,
        protection: Default::default(),
        outputs,
        engine,
        session_label: args.or_default("session-label", "stage"),
        graph_id: khaen_calibration::sweep::PLACEHOLDER_GRAPH_ID.to_string(),
        epoch_id: 1,
        apply_settle_ms: khaen_calibration::stage_backend::DEFAULT_APPLY_SETTLE_MS,
        // 板端 smoke CLI 不接二次测量播放旁路：保持既有 aplay 行为。
        remeasure_playback: khaen_calibration::stage_backend::RemeasurePlayback::Aplay,
        remeasure_host: Default::default(),
    })
}

fn resolve_mic_calibration(args: &Args) -> Result<(String, String), String> {
    let cal_file = args
        .value("mic-cal-file")
        .ok_or_else(|| "--mic-cal-file is required for acoustic UMIK mode".to_string())?
        .to_string();
    let declared = args.value("mic-cal-sha256").map(|s| s.to_string());
    let bytes = std::fs::read(&cal_file)
        .map_err(|error| format!("read --mic-cal-file {cal_file:?}: {error}"))?;
    let actual = sha256_hex(&bytes);
    match declared {
        Some(declared) => {
            if declared != actual {
                return Err(format!(
                    "--mic-cal-sha256 mismatch: declared {declared}, actual {actual}"
                ));
            }
        }
        None => {}
    }
    Ok((cal_file, actual))
}

fn print_json_line(label: &str, json: &str) {
    println!(
        "{{\"{}_schema\":\"khaen-cal-stage-cli/v1\",\"result\":{}}}",
        label, json
    );
}

fn main_entry() -> Result<u8, (String, u8)> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        println!("{USAGE}");
        return Ok(0);
    }
    let args = Args::parse(&raw).map_err(|message| (message, 2))?;
    let config = build_config(&args).map_err(|message| (message, 2))?;
    match args.command.as_str() {
        "preflight" => {
            let mut engine = config.build_engine();
            let probe = engine
                .probe(&config.devices)
                .map_err(|error| (error.to_string(), 3))?;
            config
                .validate()
                .map_err(|failure| (format!("{}: {}", failure.code, failure.message), 2))?;
            let ok = probe.playback_ok && probe.capture_ok;
            println!(
                "{{\"command\":\"preflight\",\"engine\":\"{}\",\"mode\":\"{}\",\
                 \"playback_ok\":{},\"capture_ok\":{},\"capture_sample_rates\":{:?},\
                 \"capture_max_channels\":{},\"notes\":{:?}}}",
                engine.name(),
                config.mode.as_str(),
                probe.playback_ok,
                probe.capture_ok,
                probe.capture_sample_rates,
                probe.capture_max_channels,
                probe.notes
            );
            if ok {
                Ok(0)
            } else {
                Ok(3)
            }
        }
        "measure" => {
            let out = args
                .value("out")
                .ok_or_else(|| ("--out is required for measure".to_string(), 2))?;
            let out_dir = PathBuf::from(out);
            match run_board_measurement(&config, &out_dir) {
                Ok(evidence) => {
                    print_json_line(
                        "board_measurement",
                        &format!(
                            "{{\"status\":\"PASS\",\"engine\":\"{}\",\"simulated\":{},\
                             \"out_dir\":{:?},\"gates\":{}}}",
                            evidence.engine,
                            evidence.simulated,
                            evidence.out_dir,
                            khaen_calibration::stage_backend::gates_json(&evidence.gates)
                        ),
                    );
                    Ok(0)
                }
                Err(failure) => {
                    let code = match failure.code.as_str() {
                        "E_STAGE_DEVICE_UNAVAILABLE" => 3,
                        "E_STAGE_CONFIG_INVALID" => 2,
                        _ => 4,
                    };
                    eprintln!(
                        "{{\"status\":\"FAIL\",\"code\":\"{}\",\"message\":{:?},\"out_dir\":{:?}}}",
                        failure.code, failure.message, out
                    );
                    Ok(code)
                }
            }
        }
        other => Err((format!("unknown command {other:?} (preflight|measure)"), 2)),
    }
}

fn main() -> ExitCode {
    match main_entry() {
        Ok(code) => ExitCode::from(code),
        Err((message, code)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            ExitCode::from(code)
        }
    }
}
