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

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "khaen-rt-host requires Linux ALSA; this build is a compile-only stub. \
         Use the Linux release artifact on the target machine."
    );
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::net::Ipv4Addr;
    use std::process::ExitCode;
    use std::time::Duration;

    use khaen_host::aes67_input::Aes67InputConfig;
    use khaen_host::runtime::{
        LinuxRuntime, ReceiverConfig, RuntimeConfig, RuntimeError, RuntimeStatus,
        SupervisorOptions, UnloadedPolicy,
    };
    use khaen_host::{AdmissionLimits, DeviceConfig, DeviceId, EngineConfig, SafetyPolicy};

    /// 环境变量读取失败：退出码 2，信息点名变量。
    struct ConfigError(String);

    impl ConfigError {
        fn new(message: impl Into<String>) -> Self {
            Self(message.into())
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

    fn build_config() -> Result<(RuntimeConfig, String, u64, u64), ConfigError> {
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
        Ok((config, alsa_device, run_seconds, stats_every))
    }

    fn split_argv(command: String) -> Vec<String> {
        command
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    }

    fn print_status(runtime: &LinuxRuntime, status: &RuntimeStatus) {
        let device = &status.device;
        println!(
            "blocks={} recv_pkts={} fill={} underruns={} xruns={} out_errors={} \
             state={} device_state={} plan_rate={} device_rate={:?} rate_asserted={} \
             graph={:?} last_error={:?}",
            status.blocks,
            status.receiver.packets,
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
        let _ = runtime;
    }

    pub fn run() -> ExitCode {
        let (config, alsa_device, run_seconds, stats_every) = match build_config() {
            Ok(value) => value,
            Err(err) => {
                eprintln!("khaen-rt-host config error: {}", err.0);
                return ExitCode::from(2);
            }
        };
        let plan_path = env_optional("KHAEN_PLAN");
        println!(
            "khaen-rt-host: device={} rate={} block={} channels={} aes67={}:{} plan={:?}",
            alsa_device,
            config.engine.sample_rate,
            config.engine.block_frames,
            config.engine.channels,
            config.input.multicast_addr,
            config.input.port,
            plan_path
        );

        let mut runtime: LinuxRuntime = match LinuxRuntime::new(config.clone()) {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!(
                    "khaen-rt-host: cannot open runtime: [{:?}] {err}",
                    err.code()
                );
                return ExitCode::from(1);
            }
        };

        if let Some(path) = plan_path.as_deref() {
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    eprintln!("khaen-rt-host: cannot read plan {path}: {err}");
                    return ExitCode::from(2);
                }
            };
            match runtime.apply_plan(&bytes, &AdmissionLimits::default()) {
                Ok(version) => println!("khaen-rt-host: plan applied at version {version}"),
                Err(err) => {
                    eprintln!("khaen-rt-host: plan rejected: [{:?}] {err}", err.code());
                    return ExitCode::from(1);
                }
            }
        }

        let block_frames = config.engine.block_frames as u64;
        let sample_rate = u64::from(config.engine.sample_rate);
        let target_blocks = if run_seconds == 0 {
            0
        } else {
            (run_seconds * sample_rate + block_frames - 1) / block_frames
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
            if stats_every != 0 && blocks % stats_every == 0 {
                // 验证工具的让步点：状态打印与 supervise 轮询都在块线程上（见模块文档）。
                runtime.poll_device();
                let status = runtime.status();
                print_status(&runtime, &status);
                if status.state.requires_mute() {
                    eprintln!("khaen-rt-host: safety requires mute; continuing (fail-closed blocks are silent)");
                }
            }
        }

        let status = runtime.status();
        print_status(&runtime, &status);
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
        std::thread::sleep(Duration::from_millis(0));
        ExitCode::SUCCESS
    }
}
