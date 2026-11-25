//! khaen 实时主机进程（T70f）：AES67 RTP 输入 → 方案图 → ALSA 播放。
//!
//! 这个进程是**验证工具 + 实时数据面**，不是最终形态：
//! - 它独占音频设备（ALSA PCM），**不要**和 MOTU keeper / prod_bridge / aplay 同时跑；
//! - 设备监督器的生产装配留到与 daemon 的联动（T70f 后续），本进程默认
//!   `SupervisorOptions::enabled = false`（状态如实为 absent，不假装断言过速率）；
//! - 方案切换：启动时读一次方案文件（`KHAEN_PLAN`）；运行期不换图。
//!   运行期换图需要跨进程控制通道（T70f 未实现），本进程不宣称支持。
//!
//! # 环境变量
//!
//! | 变量 | 默认 | 含义 |
//! | --- | --- | --- |
//! | `KHAEN_ALSA_DEVICE` | `default` | ALSA PCM 设备名（例如 `hw:MOTU,0`） |
//! | `KHAEN_SAMPLE_RATE` | `48000` | 方案/媒体采样率（唯一事实来源） |
//! | `KHAEN_BLOCK_FRAMES` | `128` | 块长（帧） |
//! | `KHAEN_CHANNELS` | `2` | 输出通道数 |
//! | `KHAEN_AES67_GROUP` | `239.69.0.1` | AES67 组播地址 |
//! | `KHAEN_AES67_PORT` | `5004` | RTP 目的端口 |
//! | `KHAEN_AES67_PAYLOAD_TYPE` | `96` | 期望的 RTP 动态 payload type（96..=127） |
//! | `KHAEN_AES67_CHANNELS` | `2` | RTP 流声道数 |
//! | `KHAEN_AES67_MAP` | `0,1` | RTP 声道 → 输出声道位置（逗号分隔，长度必须等于 RTP 声道数） |
//! | `KHAEN_JITTER_FRAMES` | `192` | 首块预填抖动水位（媒体帧；0 = 不预填） |
//! | `KHAEN_PLAN` | 未设置 | 方案文件路径（ABI v3）；未设置时按 `KHAEN_UNLOADED` 策略运行 |
//! | `KHAEN_UNLOADED` | `silence` | 未加载方案时的策略：`silence`（fail-closed）或 `passthrough` |
//! | `KHAEN_RUN_SECONDS` | `0` | 运行秒数；0 = 一直跑（Ctrl-C 结束） |
//! | `KHAEN_STATS_EVERY_BLOCKS` | `4000` | 每 N 块打印一次统计并轮询设备监督器（0 = 不打印） |
//! | `KHAEN_STATUS_FILE` | 未设置 | 状态 JSON 落盘路径（每统计周期原子替换一次；soak 无人值守时用来取证） |
//!
//! # 退出码
//!
//! 0 = 正常跑完（`KHAEN_RUN_SECONDS` 到点）；1 = 运行期错误（带稳定错误码）；2 = 配置/环境错误。
//!
//! # 实时约束说明（重要）
//!
//! `Runtime::run_block` 本身是零分配/无锁/无 IO 的；但本验证工具的**主循环**等于块线程：
//! 每 `KHAEN_STATS_EVERY_BLOCKS` 块会打印统计并轮询设备监督器（sysfs/命令 = IO）。
//! 这是为了在没有控制线程的今天先把真机链路跑通；**生产形态必须把监控移到独立线程/进程**，
//! 这一点在 `docs/DESIGN.md` 的线程模型里明确写着，不在这里假装满足 RT 认证。
//!
//! # 平台说明
//!
//! 除 `AlsaPcm` 外，本文件的配置解析/状态文件/块循环都在 `app` 模块里、与平台无关地编译，
//! 这样 Windows 上 `cargo clippy`/测试期就能检查这些逻辑的**类型正确性**（运行期仍需要
//! Linux + ALSA 才能出声；Windows 只打一条说明并退出 2）。

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    app::run::<khaen_host::alsa_backend::AlsaPcm>()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "khaen-rt-host requires Linux ALSA; this build is a compile-only stub. \
         Use the Linux release artifact on the target machine."
    );
    std::process::ExitCode::from(2)
}

// Windows 上 main 不实例化 app（没有 ALSA），但模块仍参与编译以检查逻辑类型；
// 因此非 Linux 目标允许这些“只被 Linux 调用者使用”的项为 dead_code。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod app {
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use khaen_core::plan::Plan;
    use khaen_host::admission::admit;
    use khaen_host::aes67_input::Aes67InputConfig;
    use khaen_host::alsa_backend::PcmOutput;
    use khaen_host::runtime::{
        ReceiverConfig, Runtime, RuntimeConfig, RuntimeStatus, SupervisorOptions, UnloadedPolicy,
    };
    use khaen_host::{AdmissionLimits, DeviceConfig, DeviceId, EngineConfig, SafetyPolicy};

    /// 环境变量读取失败：退出码 2，信息点名变量。
    pub struct ConfigError(String);

    impl ConfigError {
        fn new(message: impl Into<String>) -> Self {
            Self(message.into())
        }
        pub fn message(&self) -> &str {
            &self.0
        }
    }

    fn env_string(key: &str, default: &str) -> Result<String, ConfigError> {
        match std::env::var(key) {
            Ok(value) if !value.is_empty() => Ok(value),
            Ok(_) => Err(ConfigError::new(format!("{key} must not be empty"))),
            Err(_) => Ok(default.to_string()),
        }
    }

    fn env_parse<T>(key: &str, default: T) -> Result<T, ConfigError>
    where
        T: std::str::FromStr,
    {
        match std::env::var(key) {
            Ok(value) if !value.is_empty() => value.parse().map_err(|_| {
                ConfigError::new(format!("{key} must be a valid value, got {value:?}"))
            }),
            Ok(_) => Err(ConfigError::new(format!("{key} must not be empty"))),
            Err(_) => Ok(default),
        }
    }

    fn env_optional(key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|value| !value.is_empty())
    }

    fn parse_channels_map(text: &str) -> Result<Vec<u16>, ConfigError> {
        let mut map = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(ConfigError::new("KHAEN_AES67_MAP has an empty entry"));
            }
            let position: u16 = part.parse().map_err(|_| {
                ConfigError::new(format!(
                    "KHAEN_AES67_MAP entry {part:?} is not a channel index"
                ))
            })?;
            map.push(position);
        }
        if map.is_empty() {
            return Err(ConfigError::new("KHAEN_AES67_MAP must not be empty"));
        }
        Ok(map)
    }

    fn split_argv(command: String) -> Vec<String> {
        command
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    }

    #[derive(Debug)]
    pub struct Settings {
        pub config: RuntimeConfig,
        pub alsa_device: String,
        pub plan_path: Option<PathBuf>,
        pub status_file: Option<PathBuf>,
        pub run_seconds: u64,
        pub stats_every: u64,
    }

    /// 启动方案读取/解码失败（退出码 2：输入或配置问题）。
    pub const STARTUP_PLAN_READ: &str = "startup_plan_read";
    /// 启动方案不是合法的 ABI v3 文件（退出码 2）。
    pub const STARTUP_PLAN_DECODE: &str = "startup_plan_decode";
    /// 启动方案没通过准入预算（退出码 1：与运行时拒绝同一类）。
    pub const STARTUP_PLAN_REJECTED: &str = "startup_plan_admission_rejected";

    /// 启动方案错误：稳定码 + 退出码 + 人读消息。
    #[derive(Debug)]
    pub struct StartupPlanError {
        code: &'static str,
        exit_code: u8,
        message: String,
    }

    impl StartupPlanError {
        fn new(code: &'static str, exit_code: u8, message: impl Into<String>) -> Self {
            Self {
                code,
                exit_code,
                message: message.into(),
            }
        }

        /// 稳定错误码。
        pub fn code(&self) -> &'static str {
            self.code
        }

        /// 进程退出码（2 = 输入/配置，1 = 运行时拒绝）。
        pub fn exit_code(&self) -> u8 {
            self.exit_code
        }

        /// 人读消息。
        pub fn message(&self) -> &str {
            &self.message
        }
    }

    /// 通过预检的启动方案。
    #[derive(Debug)]
    pub struct StartupPlan {
        /// 方案文件字节（交给 Runtime::apply_plan）。
        pub bytes: Vec<u8>,
        /// graph_id（十六进制）。
        pub graph_id: String,
        /// 准入估算延迟（样本）。
        pub latency_samples: u64,
    }

    /// 读方案文件并做启动预检：ABI v3 解码 + 准入预算。
    ///
    /// 预检不碰实时线程（在 Runtime::apply_plan 之前），失败时用稳定码与退出码报告；
    /// 这样设备窗口里第一眼就能区分「文件不对」和「预算不够」。
    pub fn load_startup_plan(path: &Path) -> Result<StartupPlan, StartupPlanError> {
        let bytes = std::fs::read(path).map_err(|err| {
            StartupPlanError::new(
                STARTUP_PLAN_READ,
                2,
                format!("cannot read plan {}: {err}", path.display()),
            )
        })?;
        let plan = Plan::decode(&bytes).map_err(|err| {
            StartupPlanError::new(
                STARTUP_PLAN_DECODE,
                2,
                format!("plan {} is not a valid ABI v3 file: {err}", path.display()),
            )
        })?;
        let admission = admit(&plan, &AdmissionLimits::default());
        if !admission.accepted {
            let reasons: Vec<String> = admission
                .rejections
                .iter()
                .map(|rejection| format!("{} ({})", rejection.code, rejection.detail))
                .collect();
            return Err(StartupPlanError::new(
                STARTUP_PLAN_REJECTED,
                1,
                format!(
                    "plan {} rejected by admission: {}",
                    path.display(),
                    reasons.join("; ")
                ),
            ));
        }
        Ok(StartupPlan {
            graph_id: plan.graph_id_hex(),
            latency_samples: admission.budget.latency_samples,
            bytes,
        })
    }
    pub fn build_settings() -> Result<Settings, ConfigError> {
        let alsa_device = env_string("KHAEN_ALSA_DEVICE", "default")?;
        let sample_rate: u32 = env_parse("KHAEN_SAMPLE_RATE", 48_000)?;
        let block_frames: usize = env_parse("KHAEN_BLOCK_FRAMES", 128)?;
        let channels: usize = env_parse("KHAEN_CHANNELS", 2)?;

        let group: Ipv4Addr = env_string("KHAEN_AES67_GROUP", "239.69.0.1")?
            .parse()
            .map_err(|_| ConfigError::new("KHAEN_AES67_GROUP must be an IPv4 address"))?;
        let port: u16 = env_parse("KHAEN_AES67_PORT", 5004)?;
        let payload_type: u8 = env_parse("KHAEN_AES67_PAYLOAD_TYPE", 96)?;
        let rtp_channels: usize = env_parse("KHAEN_AES67_CHANNELS", 2)?;
        let channels_map = parse_channels_map(&env_string("KHAEN_AES67_MAP", "0,1")?)?;
        if channels_map.len() != rtp_channels {
            return Err(ConfigError::new(format!(
                "KHAEN_AES67_MAP has {} entries but KHAEN_AES67_CHANNELS is {rtp_channels}",
                channels_map.len()
            )));
        }
        let jitter_frames: usize = env_parse("KHAEN_JITTER_FRAMES", 192)?;
        let run_seconds: u64 = env_parse("KHAEN_RUN_SECONDS", 0)?;
        let stats_every: u64 = env_parse("KHAEN_STATS_EVERY_BLOCKS", 4000)?;
        let status_file = env_optional("KHAEN_STATUS_FILE").map(PathBuf::from);
        let plan_path = env_optional("KHAEN_PLAN").map(PathBuf::from);
        let unloaded = match env_string("KHAEN_UNLOADED", "silence")?.as_str() {
            "silence" => UnloadedPolicy::Silence,
            "passthrough" => UnloadedPolicy::Passthrough,
            other => {
                return Err(ConfigError::new(format!(
                    "KHAEN_UNLOADED must be silence or passthrough, got {other:?}"
                )))
            }
        };

        let input = Aes67InputConfig {
            multicast_addr: group,
            port,
            payload_type,
            sample_rate,
            channels: rtp_channels,
            channels_map,
            output_channels: channels,
            block_frames,
            jitter_target_frames: jitter_frames,
        };
        let config = RuntimeConfig {
            input,
            engine: EngineConfig {
                block_frames,
                sample_rate,
                channels,
            },
            device: DeviceConfig {
                device: DeviceId::new(alsa_device.clone()),
                sample_rate,
                block_frames,
                channels,
            },
            safety: SafetyPolicy::default(),
            receiver: ReceiverConfig::default(),
            // 生产装配（真实速率读写命令）属于与 daemon 的联动，本工具不猜：
            // 环境里给齐 KHAEN_RATE_READ/KHAEN_RATE_ASSERT 才启用监督器。
            supervisor: match (
                env_optional("KHAEN_RATE_READ"),
                env_optional("KHAEN_RATE_ASSERT"),
            ) {
                (Some(read), Some(assert)) => SupervisorOptions {
                    enabled: true,
                    plan_rate: sample_rate,
                    card_match: env_optional("KHAEN_DEVICE_ID"),
                    control_argv: env_optional("KHAEN_CONTROL_COMMAND").map(split_argv),
                    rate_read_argv: split_argv(read),
                    rate_assert_argv: split_argv(assert),
                    ..SupervisorOptions::default()
                },
                _ => SupervisorOptions::default(),
            },
            unloaded_policy: unloaded,
        };
        Ok(Settings {
            config,
            alsa_device,
            plan_path,
            status_file,
            run_seconds,
            stats_every,
        })
    }

    /// 把运行状态写成单行 JSON 文件（先写 .tmp 再 rename，读方不会看到半截）。
    ///
    /// 字段级契约见 docs/DEPLOY.md 4.3；只在控制/统计周期调用，不在 run_block 路径上。
    pub fn write_status_file(path: &Path, status: &RuntimeStatus) -> std::io::Result<()> {
        let graph_id = match status.graph.graph_id.as_deref() {
            Some(id) => format!("\"{id}\""),
            None => "null".to_string(),
        };
        let device_rate = match status.device_rate {
            Some(rate) => rate.to_string(),
            None => "null".to_string(),
        };
        let last_error = match status.last_error {
            Some(code) => format!("\"{code}\""),
            None => "null".to_string(),
        };
        let ssrc = match status.input.ssrc {
            Some(ssrc) => ssrc.to_string(),
            None => "null".to_string(),
        };
        let json = format!(
            concat!(
                "{{\"schema\":\"khaen-rt-status/v1\",\"blocks\":{},",
                "\"state\":\"{}\",\"muted\":{},\"safe\":{},",
                "\"plan_rate\":{},\"device_rate\":{},\"rate_asserted\":{},",
                "\"input\":{{\"packets\":{},\"underruns\":{},\"jitter_fill\":{},\"ssrc\":{}}},",
                "\"output\":{{\"xruns\":{},\"blocks_written\":{},\"bytes_written\":{}}},",
                "\"receiver\":{{\"packets_received\":{},\"queue_full_drops\":{},\"recv_errors\":{}}},",
                "\"graph_id\":{},\"last_error\":{}}}\n"
            ),
            status.blocks,
            status.state.as_str(),
            status.muted,
            status.state.requires_mute(),
            status.plan_rate,
            device_rate,
            status.rate_asserted,
            status.input.packets,
            status.input.underruns,
            status.input.jitter_fill,
            ssrc,
            status.output.xruns,
            status.output.blocks_written,
            status.output.bytes_written,
            status.receiver.packets_received,
            status.receiver.queue_full_drops,
            status.receiver.recv_errors,
            graph_id,
            last_error
        );
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, json.as_bytes())?;
        std::fs::rename(&temporary, path)
    }

    pub fn print_status(status: &RuntimeStatus) {
        let device = &status.device;
        println!(
            "blocks={} recv_pkts={} fill={} underruns={} xruns={} out_errors={} \
             state={} device_state={} plan_rate={} device_rate={:?} rate_asserted={} \
             graph={:?} last_error={:?}",
            status.blocks,
            status.receiver.packets_received,
            status.input.jitter_fill,
            status.input.underruns,
            status.output.xruns,
            status.output_errors,
            status.state.as_str(),
            device.state.as_str(),
            status.plan_rate,
            status.device_rate,
            status.rate_asserted,
            status.graph.graph_id,
            status.last_error
        );
    }

    /// 数据面主循环（平台无关；`P` 为 PCM 设备类型，真机是 AlsaPcm，Windows 不实例化）。
    pub fn run<P: PcmOutput + 'static>() -> ExitCode {
        let settings = match build_settings() {
            Ok(value) => value,
            Err(err) => {
                eprintln!("khaen-rt-host config error: {}", err.message());
                return ExitCode::from(2);
            }
        };
        println!(
            "khaen-rt-host: device={} rate={} block={} channels={} aes67={}:{} plan={:?}",
            settings.alsa_device,
            settings.config.engine.sample_rate,
            settings.config.engine.block_frames,
            settings.config.engine.channels,
            settings.config.input.multicast_addr,
            settings.config.input.port,
            settings.plan_path
        );

        let mut runtime: Runtime<P> = match Runtime::new(settings.config.clone()) {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!(
                    "khaen-rt-host: cannot open runtime: [{:?}] {err}",
                    err.code()
                );
                return ExitCode::from(1);
            }
        };

        if let Some(path) = settings.plan_path.as_deref() {
            let startup = match load_startup_plan(path) {
                Ok(startup) => startup,
                Err(err) => {
                    eprintln!(
                        "khaen-rt-host: startup plan failed: [{}] {}",
                        err.code(),
                        err.message()
                    );
                    return ExitCode::from(err.exit_code());
                }
            };
            println!(
                "khaen-rt-host: plan preflight ok (graph_id {}, est. latency {} samples)",
                startup.graph_id, startup.latency_samples
            );
            match runtime.apply_plan(&startup.bytes, &AdmissionLimits::default()) {
                Ok(version) => println!(
                    "khaen-rt-host: plan applied at version {version} (graph_id {})",
                    startup.graph_id
                ),
                Err(err) => {
                    eprintln!("khaen-rt-host: plan rejected: [{:?}] {err}", err.code());
                    return ExitCode::from(1);
                }
            }
        }

        let block_frames = settings.config.engine.block_frames as u64;
        let sample_rate = u64::from(settings.config.engine.sample_rate);
        let target_blocks = if settings.run_seconds == 0 {
            0
        } else {
            (settings.run_seconds * sample_rate).div_ceil(block_frames)
        };
        let mut blocks: u64 = 0;
        loop {
            if target_blocks != 0 && blocks >= target_blocks {
                break;
            }
            if let Err(err) = runtime.run_block() {
                eprintln!(
                    "khaen-rt-host: run_block failed at block {blocks}: [{:?}] {err}",
                    err.code()
                );
                return ExitCode::from(1);
            }
            blocks += 1;
            if settings.stats_every != 0 && blocks.is_multiple_of(settings.stats_every) {
                // 验证工具的让步点：状态打印与 supervise 轮询都在块线程上（见模块文档）。
                runtime.poll_device();
                let status = runtime.status();
                print_status(&status);
                if status.state.requires_mute() {
                    eprintln!("khaen-rt-host: safety requires mute; continuing (fail-closed blocks are silent)");
                }
                if let Some(path) = settings.status_file.as_deref() {
                    if let Err(err) = write_status_file(path, &status) {
                        eprintln!(
                            "khaen-rt-host: cannot write status file {}: {err}",
                            path.display()
                        );
                    }
                }
            }
        }

        let status = runtime.status();
        print_status(&status);
        if let Some(path) = settings.status_file.as_deref() {
            if let Err(err) = write_status_file(path, &status) {
                eprintln!(
                    "khaen-rt-host: cannot write final status file {}: {err}",
                    path.display()
                );
            }
        }
        println!(
            "khaen-rt-host: finished {blocks} blocks ({} s), exiting",
            (blocks * block_frames) as f64 / sample_rate as f64
        );
        // 明确停收包线程并 join；drop 也会做，但这里要让失败可见。
        if let Err(err) = runtime.stop_receiver() {
            eprintln!(
                "khaen-rt-host: receiver stop failed: [{:?}] {err}",
                err.code()
            );
            return ExitCode::from(1);
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::app;
    use khaen_host::alsa_backend::AlsaStats;
    use khaen_host::runtime::{ReceiverStats, RuntimeStatus};
    use khaen_host::safety::RuntimeState;
    use khaen_host::supervisor::{LifecycleState, SupervisorStatus};

    fn status() -> RuntimeStatus {
        RuntimeStatus {
            state: RuntimeState::Running,
            blocks: 12,
            input: khaen_host::aes67_input::Aes67InputStats {
                packets: 34,
                underruns: 2,
                jitter_fill: 192,
                ..Default::default()
            },
            output: AlsaStats::default(),
            receiver: ReceiverStats::default(),
            device: SupervisorStatus {
                state: LifecycleState::Absent,
                plan_rate: 48_000,
                device_rate: None,
                rate_asserted: false,
                last_error: None,
                attempts: 0,
                since_ms: 0,
            },
            device_rate: None,
            plan_rate: 48_000,
            rate_asserted: false,
            output_errors: 0,
            output_started: true,
            graph: Default::default(),
            switch_pending: false,
            muted: false,
            last_error: None,
        }
    }

    #[test]
    fn status_file_is_valid_single_line_json() {
        let mut path = std::env::temp_dir();
        path.push(format!("khaen-rt-host-status-{}.json", std::process::id()));
        app::write_status_file(&path, &status()).expect("write status file");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(text.matches('\n').count(), 1, "must be a single line");
        assert!(text.ends_with('\n'));
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(doc["schema"], serde_json::json!("khaen-rt-status/v1"));
        assert_eq!(doc["blocks"], serde_json::json!(12));
        assert_eq!(doc["state"], serde_json::json!("running"));
        assert_eq!(doc["muted"], serde_json::json!(false));
        assert_eq!(doc["plan_rate"], serde_json::json!(48_000));
        assert_eq!(doc["device_rate"], serde_json::Value::Null);
        assert_eq!(doc["input"]["packets"], serde_json::json!(34));
        assert_eq!(doc["input"]["ssrc"], serde_json::Value::Null);
        assert_eq!(doc["graph_id"], serde_json::Value::Null);
        assert_eq!(doc["last_error"], serde_json::Value::Null);
        // 原子替换：临时文件不残留。
        assert!(!path.with_extension("tmp").exists());
    }

    use std::sync::atomic::{AtomicU32, Ordering};

    use khaen_core::plan::{
        ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan,
        ProtectionEntry, ProtectionSection, RoutingSection,
    };

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_plan_path(tag: &str) -> std::path::PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "khaen-rt-host-plan-{}-{}-{}.bin",
            std::process::id(),
            tag,
            unique
        ))
    }

    /// 全恒等方案 + crossover 每 lane 一条 `taps` 抽头 FIR（lane 0 冲激，其余静音）。
    fn fir_plan(lanes: u16, taps: usize) -> Vec<u8> {
        let folds: Vec<Fold> = (0..lanes)
            .map(|lane| Fold {
                src: lane,
                dst: lane,
                gain: 1.0,
            })
            .collect();
        let filters: Vec<CrossoverFilter> = (0..lanes)
            .map(|lane| {
                let mut coeffs = vec![0.0f32; taps];
                if lane == 0 {
                    coeffs[0] = 1.0;
                }
                CrossoverFilter { lane, coeffs }
            })
            .collect();
        let mut matrix_coeffs = vec![0.0f32; usize::from(lanes) * usize::from(lanes)];
        for lane in 0..usize::from(lanes) {
            matrix_coeffs[lane * usize::from(lanes) + lane] = 1.0;
        }
        let entries: Vec<ProtectionEntry> = (0..lanes)
            .map(|lane| ProtectionEntry {
                lane,
                delay: 0,
                polarity: 1,
                gain_db: 0.0,
                peq: Vec::new(),
            })
            .collect();
        let plan = Plan {
            block_size: 128,
            sample_rate: 48_000,
            content: Some(ContentSection {
                in_channels: lanes,
                out_lanes: lanes,
                folds,
            }),
            crossover: Some(CrossoverSection { lanes, filters }),
            matrix: Some(MatrixSection {
                rows: lanes,
                cols: lanes,
                decimation: 1,
                interpolation: 1,
                taps: 1,
                coeffs: matrix_coeffs,
            }),
            protection: Some(ProtectionSection { lanes, entries }),
            routing: Some(RoutingSection {
                dac_channels: lanes,
                routes: (0..lanes).map(|lane| (lane, lane)).collect(),
            }),
        };
        plan.encode_checked().expect("encode fir plan")
    }

    #[test]
    fn startup_plan_preflights_2048_tap_fir() {
        let bytes = fir_plan(2, 2048);
        let path = temp_plan_path("fir");
        std::fs::write(&path, &bytes).expect("write plan");
        let startup = app::load_startup_plan(&path).expect("preflight");
        let _ = std::fs::remove_file(&path);
        assert_eq!(startup.bytes, bytes);
        assert_eq!(startup.graph_id.len(), 64);
        assert!(startup
            .graph_id
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
        // 2048 抽头 => 估算延迟 (2048-1)/2 = 1023 样本（48 kHz）。
        assert_eq!(startup.latency_samples, 1023);
    }

    #[test]
    fn startup_plan_reports_read_decode_and_admission_failures() {
        let missing = temp_plan_path("missing");
        let error = app::load_startup_plan(&missing).expect_err("missing plan");
        assert_eq!(error.code(), app::STARTUP_PLAN_READ);
        assert_eq!(error.exit_code(), 2);

        let garbage = temp_plan_path("garbage");
        std::fs::write(&garbage, b"not a plan").expect("write garbage");
        let error = app::load_startup_plan(&garbage).expect_err("garbage plan");
        let _ = std::fs::remove_file(&garbage);
        assert_eq!(error.code(), app::STARTUP_PLAN_DECODE);
        assert_eq!(error.exit_code(), 2);

        // 33 lane 超过准入默认上限 32 => 准入拒绝（退出码 1）。
        let over_budget = temp_plan_path("lanes");
        std::fs::write(&over_budget, fir_plan(33, 1)).expect("write plan");
        let error = app::load_startup_plan(&over_budget).expect_err("over budget");
        let _ = std::fs::remove_file(&over_budget);
        assert_eq!(error.code(), app::STARTUP_PLAN_REJECTED);
        assert_eq!(error.exit_code(), 1);
        assert!(error.message().contains("lanes"), "{}", error.message());
    }

    #[test]
    fn invalid_unloaded_value_is_rejected() {
        std::env::set_var("KHAEN_UNLOADED", "loud");
        let err = app::build_settings().expect_err("loud must be rejected");
        assert!(
            err.message().contains("KHAEN_UNLOADED"),
            "{}",
            err.message()
        );
        std::env::remove_var("KHAEN_UNLOADED");
    }
}
