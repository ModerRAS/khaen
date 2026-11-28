#![recursion_limit = "256"]
//! khaen 实时主机进程（T70f）：AES67 RTP 输入 → 方案图 → ALSA 播放。
//!
//! 这个进程是**验证工具 + 实时数据面**，不是最终形态：
//! - 它独占音频设备（ALSA PCM），**不要**和 MOTU keeper / prod_bridge / aplay 同时跑；
//! - 设备监督器的生产装配留到与 daemon 的联动（T70f 后续），本进程默认
//!   `SupervisorOptions::enabled = false`（状态如实为 absent，不假装断言过速率）；
//! - 方案切换：启动时读一次方案文件（`KHAEN_PLAN`）；运行期可经 Unix socket 控制通道
//!   （T70f-a 已实现服务端：status/mute/prepare/commit/abort/coeff/inspect/surface/rollback/format；
//!   T70f-c 校准 cal_*；M3b 热重配 channels_status/prepare/commit/abort）
//!   在块边界换图/静音。daemon 侧的代理客户端（把 REST 转发成本协议）是 T70f-b，尚未实现；
//!   在它接上之前，daemon 的 `/api/*` 仍只作用于 daemon 自己的控制面副本，不代表已作用于本进程。
//! - 采样率回读：打开 PCM 前后回读 `/proc/asound/.../hw_params` 的 `rate:`；若与方案速率
//!   不一致（例如 MOTU 被外部改到 192 kHz），fail-closed 不播放、关掉设备并按退避重试，
//!   状态文件记录 `last_device_error`（不带着错速播放，也不退出进程）。回读依赖 `hw:`
//!   直连设备名；`default`/`plughw:` 读不到时只打印 unknown，不假装断言成功。
//!
//! # 声卡掉线不退出（T42o）
//!
//! 声卡被拔掉/重新枚举（ALSA 写 `alsa pcm disconnected`）或启动时不在位时，进程**不退出**：
//! 关闭 PCM 句柄、进入 fail-closed 的 `absent` 状态（不输出、状态文件 `safe=true`），按固定
//! 退避（2 s 起、翻倍、封顶 5 s，日志每 10 次尝试限频一条）用 `Runtime::open_device` 重开；
//! 恢复后重建输入抖动缓冲（重新预填）、显式清除设备丢失故障、当前方案 version/graph_id 保持
//! 不变，块计数继续增长回到 running。设备缺失永不导致退出；只有配置/方案准入类错误与无法
//! 恢复的内部错误才退出（稳定退出码）。
//!
//! # 跨进程控制通道（T70f-a）
//!
//! 进程内跑契约 B 的行 JSON 服务端（`control` 模块）：Unix domain socket，一行 JSON 请求 /
//! 一行 JSON 响应，每连接一个处理线程；路径取 `KHAEN_CONTROL_SOCKET`（默认
//! `/run/khaen/rt-host.sock`），`KHAEN_CONTROL=off` 关闭。控制线程**不直接碰 Runtime**：
//! 静音/换图通过 mpsc 命令交给块线程、在块边界执行并回一次性 ack；status 读块线程发布的
//! 状态快照（附 `age_ms`/`stale`），不阻塞块线程。`Runtime::run_block` 一行未改。
//!
//! 如实边界：
//! - 控制通道起不来（路径不可写/已被占用等）只打印原因、数据面继续跑，**不退出进程**；
//! - `format` 只改控制面的编译格式（commit 如实报 `device_restart_required=true` /
//!   `pcm_reconfigured=false`）；真正换数据面格式/通道/AES67 映射走 M3b 热重配：
//!   `channels_prepare`（校验 + 只读设备探测 + token/diff）→ `channels_commit`（块边界静音、
//!   关旧 PCM、重开新 hw params、重建收包/输入抖动环/引擎、hw 回读校验、恢复；失败按旧配置
//!   回滚并如实报 `rolled_back`），进程/PID 不变、块计数连续、实测 `gap_ms` 上报；
//!   格式不匹配的方案 commit 仍回 `not_applied`，不会把不匹配的图塞进引擎；
//! - 传输层不做认证（本机可信连接，socket 权限由部署/umask 决定）；跨机/多用户部署需要另外的
//!   鉴权层，不在本任务；
//! - 设备缺失（T42o absent）时控制通道照常响应 status，mute/commit 仍受理，图在恢复出块后的
//!   第一个块边界生效（如实上报 `applied_block`）。
//!
//! # 热重配（M3b，进程/PID 不变）
//!
//! `channels_status/prepare/commit/abort`（同一控制通道）把"换通道数/采样率/AES67 组播-端口-PT-
//! 映射/抖动水位"变成可回滚的两阶段操作：prepare 在控制线程校验（映射长度 = AES67 RTP 通道数、
//! 槽位 < 输出通道且单射、PT 96..=127、速率属支持集合、准入、只读读 `/proc/asound` 探测）并返回
//! token + planned generation + diff；commit 经命令通道交给块线程，在块边界执行：安全静音 → 关旧
//! PCM → 按新配置重开设备、重建 AES67 收包线程（端口/组播变化时）与输入抖动环、引擎格式变化时
//! 重建 SlotEngine（旧图丢弃，如实报 `graph_dropped`）→ 回读 hw rate/channels 校验 → 恢复用户静音。
//! 任何一步失败都按旧配置回滚（`rolled_back` 如实说明、`generation` 不前进、设备恢复不了就保持
//! fail-closed absent 并继续退避重开）；与进程内校准模式互斥（校准中 commit -> `cal_busy`，
//! 重配中 cal_enter -> `reconfiguring`）。实测 `gap_ms` 是"静音到恢复"的控制窗口，**不是声学空档**；
//! 目标参考 ≤2 s，不设硬上限（真机数值 NOT_RUN，见交接）。
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
//! | `KHAEN_RUN_SECONDS` | `0` | 按块数换算的运行秒数；0 = 一直跑（Ctrl-C 结束）。设备缺失期间块不增长，进程保持运行 |
//! | `KHAEN_STATS_EVERY_BLOCKS` | `4000` | 每 N 块打印一次统计并轮询设备监督器（0 = 不打印） |
//! | `KHAEN_STATUS_FILE` | 未设置 | 状态 JSON 落盘路径（每统计周期与设备状态变化时原子替换；soak 无人值守时用来取证） |
//! | `KHAEN_CONTROL` | `on` | 控制通道开关：`off`/`0`/`false` 关闭；其它非法值按配置错误退出 2 |
//! | `KHAEN_CONTROL_SOCKET` | `/run/khaen/rt-host.sock` | 控制通道 Unix socket 路径 |
//!
//! # 状态文件字段（schema 仍是 `khaen-rt-status/v1`，均为向后兼容的纯增量）
//!
//! 原有字段不变；T42o 新增：`device_state`（`present`/`absent`）、`absent_since`（本次缺失起点，
//! 进程内单调毫秒，present 为 `null`）、`reopen_attempts`（本次缺失期间的重开次数）、
//! `last_device_error`（最近一次设备错误文本或 `null`）。设备缺失期间 `safe` 恒为 `true`。
//! M3b 新增：`format`（数据面块长/速率/通道）与 `reconfigure`（`generation`、`last_gap_ms`、
//! `last_result`、`in_flight`）。
//!
//! # 退出码
//!
//! 0 = 正常跑完（`KHAEN_RUN_SECONDS` 到点）；1 = 运行期不可恢复错误（带稳定错误码）；
//! 2 = 配置/环境错误（含方案文件读取/ABI 错误）。**设备缺失/掉线永不退出**。
//!
//! # 实时约束说明（重要）
//!
//! `Runtime::run_block` 本身是零分配/无锁/无 IO 的；但本验证工具的**主循环**等于块线程：
//! 每 `KHAEN_STATS_EVERY_BLOCKS` 块会打印统计并轮询设备监督器（sysfs/命令 = IO），每个块前方
//! 用 try_recv 处理控制命令（无锁等待）、按统计周期克隆状态快照发布。
//! 控制通道把"重活"（读方案、解码、准入、图编译、surface 补丁）放在控制线程，但**命令执行与
//! 状态采集仍在块线程**上（这是验证工具的让步）；**生产形态必须把监控移到独立线程/进程**，
//! 这一点在 `docs/DESIGN.md` 的线程模型里明确写着，不在这里假装满足 RT 认证。
//!
//! # 平台说明
//!
//! 除 `AlsaPcm` 外，本文件的配置解析/状态文件/设备监督循环/控制协议核心都在 `app` /
//! `recovery` / `control` 模块里、与平台无关地编译，这样 Windows 上 `cargo clippy`/测试期
//! 就能检查这些逻辑的**类型正确性**；Unix socket 传输是 `cfg(unix)` 的（Windows 无传输实现，
//! 控制核心与内存双工/回环夹具仍参与测试）。运行期仍需要 Linux + ALSA 才能出声；
//! Windows 只打一条说明并退出 2。
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

// 设备缺失/重开监督循环（T42o）：平台无关（时钟/设备都可注入），Windows 上同样参与
// 编译与单元测试；只有 Linux 的 main 会真正跑它。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod recovery;

// 跨进程控制通道（T70f-a）：契约 B 的行 JSON 服务端。协议核心/命令/快照平台无关
// （Windows 上同样参与编译与单元测试）；Unix socket 传输只在 cfg(unix) 下编译。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod control;

// IO I2a：从 SDP 解析 AES67 单流参数（纯解析，无设备/网络；Windows 也参与单测）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod sdp_config;

// Windows 上 main 不实例化 app（没有 ALSA），但模块仍参与编译以检查逻辑类型；
// 因此非 Linux 目标允许这些“只被 Linux 调用者使用”的项为 dead_code。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod app {
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use std::sync::{mpsc, Arc, Mutex};

    use crate::control::snapshot::SharedSnapshot;
    use crate::control::{
        Aes67InputSource, ControlBridge, ControlConfig, ControlCore, ControlCoreConfig,
    };
    use crate::recovery::DeviceLifecycle;
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

    /// 读 SDP 文件（启动期一次；限制 64 KiB，避免误指大文件）。
    fn read_sdp_file(path: &str) -> Result<String, ConfigError> {
        const MAX_SDP_BYTES: u64 = 64 * 1024;
        let metadata = std::fs::metadata(path).map_err(|error| {
            ConfigError::new(format!("KHAEN_AES67_SDP cannot stat {path}: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::new(format!(
                "KHAEN_AES67_SDP {path} is not a regular file"
            )));
        }
        if metadata.len() > MAX_SDP_BYTES {
            return Err(ConfigError::new(format!(
                "KHAEN_AES67_SDP {path} is {} bytes, limit {MAX_SDP_BYTES}",
                metadata.len()
            )));
        }
        let bytes = std::fs::read(path).map_err(|error| {
            ConfigError::new(format!("KHAEN_AES67_SDP cannot read {path}: {error}"))
        })?;
        String::from_utf8(bytes)
            .map_err(|_| ConfigError::new(format!("KHAEN_AES67_SDP {path} is not valid UTF-8")))
    }

    /// 读取多流配置文件（启动期一次；限制 64 KiB）。
    fn read_streams_file(path: &str) -> Result<String, ConfigError> {
        const MAX_BYTES: u64 = 64 * 1024;
        let metadata = std::fs::metadata(path).map_err(|error| {
            ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS cannot stat {path}: {error}"
            ))
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS {path} is not a regular file"
            )));
        }
        if metadata.len() > MAX_BYTES {
            return Err(ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS {path} is {} bytes, limit {MAX_BYTES}",
                metadata.len()
            )));
        }
        let bytes = std::fs::read(path).map_err(|error| {
            ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS cannot read {path}: {error}"
            ))
        })?;
        String::from_utf8(bytes).map_err(|_| {
            ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS {path} is not valid UTF-8"
            ))
        })
    }

    /// 解析多流 JSON（数组；每项 group/port/payload_type/channels/map，可选 jitter_frames）。
    ///
    /// 输出宽度/块长/采样率继承主流（RuntimeConfig 要求所有流一致）；槽位必须 < 输入宽度。
    pub fn parse_additional_streams(
        text: &str,
        sample_rate: u32,
        block_frames: usize,
        output_channels: usize,
        default_jitter_frames: usize,
    ) -> Result<Vec<Aes67InputConfig>, ConfigError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
            ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS is not valid JSON: {error}"
            ))
        })?;
        let Some(entries) = value.as_array() else {
            return Err(ConfigError::new(
                "KHAEN_AES67_ADDITIONAL_STREAMS must be a JSON array",
            ));
        };
        const MAX_ADDITIONAL: usize = khaen_host::aes67_bus::MAX_BUS_STREAMS - 1;
        if entries.len() > MAX_ADDITIONAL {
            return Err(ConfigError::new(format!(
                "KHAEN_AES67_ADDITIONAL_STREAMS has {} entries, limit {MAX_ADDITIONAL}",
                entries.len()
            )));
        }
        let mut streams: Vec<Aes67InputConfig> = Vec::with_capacity(entries.len());
        let mut endpoints: Vec<(std::net::Ipv4Addr, u16)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let object = entry.as_object().ok_or_else(|| {
                ConfigError::new(format!(
                    "KHAEN_AES67_ADDITIONAL_STREAMS[{index}] must be a JSON object"
                ))
            })?;
            for key in object.keys() {
                if !matches!(
                    key.as_str(),
                    "group" | "port" | "payload_type" | "channels" | "map" | "jitter_frames"
                ) {
                    return Err(ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS[{index}] has unknown field {key:?}; supported: group/port/payload_type/channels/map/jitter_frames"
                    )));
                }
            }
            let group_text = object
                .get("group")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].group must be an IPv4 address string"
                    ))
                })?;
            let group: std::net::Ipv4Addr = group_text.parse().map_err(|_| {
                ConfigError::new(format!(
                    "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].group is not an IPv4 address"
                ))
            })?;
            let port = object
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].port must be 1..=65535"
                    ))
                })?;
            let payload_type = object
                .get("payload_type")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].payload_type must be 96..=127"
                    ))
                })?;
            let channels = object
                .get("channels")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].channels must be a positive integer"
                    ))
                })?;
            let map_entries = object
                .get("map")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].map must be an array of input slots"
                    ))
                })?;
            let mut channels_map = Vec::with_capacity(map_entries.len());
            for slot in map_entries {
                let slot = slot.as_u64().and_then(|value| u16::try_from(value).ok()).ok_or_else(
                    || {
                        ConfigError::new(format!(
                            "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].map entries must be u16 channel slots"
                        ))
                    },
                )?;
                channels_map.push(slot);
            }
            let jitter_frames = match object.get("jitter_frames") {
                Some(value) => value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        ConfigError::new(format!(
                            "KHAEN_AES67_ADDITIONAL_STREAMS[{index}].jitter_frames must be a non-negative integer"
                        ))
                    })?,
                None => default_jitter_frames,
            };
            let config = Aes67InputConfig {
                multicast_addr: group,
                port,
                payload_type,
                sample_rate,
                channels,
                channels_map,
                output_channels,
                block_frames,
                jitter_target_frames: jitter_frames,
            };
            config.validate().map_err(|error| {
                ConfigError::new(format!(
                    "KHAEN_AES67_ADDITIONAL_STREAMS[{index}] is invalid: {}",
                    error.name()
                ))
            })?;
            if endpoints.contains(&(group, port)) {
                return Err(ConfigError::new(format!(
                    "KHAEN_AES67_ADDITIONAL_STREAMS[{index}] duplicates another stream's group/port ({group}:{port})"
                )));
            }
            endpoints.push((group, port));
            streams.push(config);
        }
        Ok(streams)
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
        /// 控制通道配置（KHAEN_CONTROL / KHAEN_CONTROL_SOCKET）。
        pub control: ControlConfig,
        /// AES67 输入配置来源（SDP 文件或手工 env；channels_status 如实上报）。
        pub input_source: Aes67InputSource,
    }

    /// 设备实际速率与方案速率不一致（退出码 1：运行期拒绝，不带着错速播放）。
    pub const DEVICE_RATE_MISMATCH: &str = "device_rate_mismatch";

    /// ALSA 设备名解析结果：卡序号或卡 id。
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AlsaCard {
        /// `hw:0` / `hw:0,1` 里的卡序号。
        Index(u32),
        /// `hw:CARD=UltraLitemk5,...` 里的卡 id。
        Id(String),
    }

    /// 解析 `KHAEN_ALSA_DEVICE`：只认 `hw:` 直连写法；`default`/`plughw:` 等不做速率回读。
    pub fn parse_alsa_device(device: &str) -> Option<AlsaCard> {
        let rest = device.strip_prefix("hw:")?;
        let first = rest.split(',').next()?.trim();
        if let Some(id) = first.strip_prefix("CARD=") {
            if id.is_empty() {
                return None;
            }
            return Some(AlsaCard::Id(id.to_string()));
        }
        first.parse::<u32>().ok().map(AlsaCard::Index)
    }

    /// 从 `/proc/asound/cards` 文本里按卡 id 找卡序号（行格式：` 0 [UltraLitemk5 ]: ...`）。
    pub fn card_index_for_id(cards_text: &str, card_id: &str) -> Option<u32> {
        for line in cards_text.lines() {
            let line = line.trim_start();
            let Some(open) = line.find('[') else {
                continue;
            };
            let Some(close) = line[open..].find(']') else {
                continue;
            };
            let id = line[open + 1..open + close].trim();
            if id == card_id {
                let index_text = line[..open].trim();
                return index_text.parse::<u32>().ok();
            }
        }
        None
    }

    /// 解析 hw_params 文本里的 `rate:` 行（`closed` 或没有该行返回 None）。
    pub fn parse_hw_params_rate(text: &str) -> Option<u32> {
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("rate:") else {
                continue;
            };
            return rest.split_whitespace().next()?.parse::<u32>().ok();
        }
        None
    }

    /// 解析 hw_params 文本里的 `channels:` 行（`closed` 或没有该行返回 None）。
    pub fn parse_hw_params_channels(text: &str) -> Option<u32> {
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("channels:") else {
                continue;
            };
            return rest.split_whitespace().next()?.parse::<u32>().ok();
        }
        None
    }

    /// 读 `/proc/asound/card<N>/pcm<D>p/sub0/hw_params` 里的实际速率。
    pub fn read_hw_rate(proc_asound: &Path, card: u32, pcm_index: u32) -> Option<u32> {
        let path = proc_asound
            .join(format!("card{card}"))
            .join(format!("pcm{pcm_index}p"))
            .join("sub0")
            .join("hw_params");
        let text = std::fs::read_to_string(path).ok()?;
        parse_hw_params_rate(&text)
    }

    /// 打开设备时按方案速率协商，打开后做一次回读校验；不一致就 fail-closed。
    pub fn check_device_rate(plan_rate: u32, rate_after: Option<u32>) -> Result<(), &'static str> {
        match rate_after {
            Some(rate) if rate != plan_rate => Err(DEVICE_RATE_MISMATCH),
            _ => Ok(()),
        }
    }

    /// 解析设备名到卡序号（id 写法需要 `/proc/asound/cards` 文本）。
    pub fn resolve_card_index(device: &str, cards_text: &str) -> Option<u32> {
        match parse_alsa_device(device)? {
            AlsaCard::Index(index) => Some(index),
            AlsaCard::Id(id) => card_index_for_id(cards_text, &id),
        }
    }

    /// 设备名里的 PCM 序号（`hw:0,1` 的 1；缺省 0）。
    pub fn device_pcm_index(device: &str) -> u32 {
        let Some(raw) = device
            .strip_prefix("hw:")
            .and_then(|rest| rest.split(',').nth(1))
        else {
            return 0;
        };
        let raw = raw.trim();
        raw.strip_prefix("DEV=")
            .unwrap_or(raw)
            .parse::<u32>()
            .unwrap_or(0)
    }

    /// 读当前卡在 `/proc/asound/cards` 的文本（没有该文件返回空串）。
    pub fn read_cards_text(proc_asound: &Path) -> String {
        std::fs::read_to_string(proc_asound.join("cards")).unwrap_or_default()
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
        // IO I1：输入/输出宽度可分开启动；KHAEN_CHANNELS 仍同时设置两者（向后兼容）。
        let input_channels: usize = env_parse("KHAEN_INPUT_CHANNELS", channels)?;
        let output_channels: usize = env_parse("KHAEN_OUTPUT_CHANNELS", channels)?;

        // IO I2a：可选 SDP 文件作为 AES67 单流参数来源。显式 env 与 SDP 冲突时
        // fail-closed，不静默让其中一边获胜。SDP 只解析；设备/网络动作仍由 host 接收器做。
        let (group, port, payload_type, rtp_channels, channels_map, input_source) =
            match env_optional("KHAEN_AES67_SDP") {
                Some(path) => {
                    let text = read_sdp_file(&path)?;
                    let sdp =
                        crate::sdp_config::parse_sdp_input(&text, sample_rate, input_channels)
                            .map_err(|message| {
                                ConfigError::new(format!("KHAEN_AES67_SDP {path}: {message}"))
                            })?;
                    for warning in &sdp.warnings {
                        eprintln!("khaen-rt-host: SDP warning: {warning}");
                    }
                    if let Some(raw) = env_optional("KHAEN_AES67_GROUP") {
                        let value: Ipv4Addr = raw.parse().map_err(|_| {
                            ConfigError::new("KHAEN_AES67_GROUP must be an IPv4 address")
                        })?;
                        if value != sdp.multicast_addr {
                            return Err(ConfigError::new(format!(
                                "KHAEN_AES67_GROUP={value} conflicts with SDP group {}; remove the env override or use a matching SDP",
                                sdp.multicast_addr
                            )));
                        }
                    }
                    if let Some(raw) = env_optional("KHAEN_AES67_PORT") {
                        let value: u16 = raw.parse().map_err(|_| {
                            ConfigError::new("KHAEN_AES67_PORT must be a port number")
                        })?;
                        if value != sdp.port {
                            return Err(ConfigError::new(format!(
                                "KHAEN_AES67_PORT={value} conflicts with SDP port {}",
                                sdp.port
                            )));
                        }
                    }
                    if let Some(raw) = env_optional("KHAEN_AES67_PAYLOAD_TYPE") {
                        let value: u8 = raw.parse().map_err(|_| {
                            ConfigError::new("KHAEN_AES67_PAYLOAD_TYPE must be a number")
                        })?;
                        if value != sdp.payload_type {
                            return Err(ConfigError::new(format!(
                            "KHAEN_AES67_PAYLOAD_TYPE={value} conflicts with SDP payload type {}",
                            sdp.payload_type
                        )));
                        }
                    }
                    if let Some(raw) = env_optional("KHAEN_AES67_CHANNELS") {
                        let value: usize = raw.parse().map_err(|_| {
                            ConfigError::new("KHAEN_AES67_CHANNELS must be a positive integer")
                        })?;
                        if value != sdp.channels {
                            return Err(ConfigError::new(format!(
                                "KHAEN_AES67_CHANNELS={value} conflicts with SDP {} channel(s)",
                                sdp.channels
                            )));
                        }
                    }
                    if let Some(raw) = env_optional("KHAEN_AES67_MAP") {
                        let value = parse_channels_map(&raw)?;
                        if value != sdp.channels_map {
                            return Err(ConfigError::new(format!(
                            "KHAEN_AES67_MAP={raw} conflicts with the SDP implied channel map {:?}",
                            sdp.channels_map
                        )));
                        }
                    }
                    let input_source = Aes67InputSource::Sdp {
                        sha256: khaen_core::sha256::sha256_hex(text.as_bytes()),
                    };
                    (
                        sdp.multicast_addr,
                        sdp.port,
                        sdp.payload_type,
                        sdp.channels,
                        sdp.channels_map,
                        input_source,
                    )
                }
                None => {
                    let group: Ipv4Addr = env_string("KHAEN_AES67_GROUP", "239.69.0.1")?
                        .parse()
                        .map_err(|_| {
                            ConfigError::new("KHAEN_AES67_GROUP must be an IPv4 address")
                        })?;
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
                    (
                        group,
                        port,
                        payload_type,
                        rtp_channels,
                        channels_map,
                        Aes67InputSource::Manual,
                    )
                }
            };
        let jitter_frames: usize = env_parse("KHAEN_JITTER_FRAMES", 192)?;
        let run_seconds: u64 = env_parse("KHAEN_RUN_SECONDS", 0)?;
        let stats_every: u64 = env_parse("KHAEN_STATS_EVERY_BLOCKS", 4000)?;
        let status_file = env_optional("KHAEN_STATUS_FILE").map(PathBuf::from);
        let plan_path = env_optional("KHAEN_PLAN").map(PathBuf::from);
        // 控制通道：非法值按配置错误处理（不静默回退，避免"以为开了其实没开"）。
        let control = ControlConfig::from_env().map_err(|err| ConfigError::new(err.message()))?;
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
            output_channels: input_channels,
            block_frames,
            jitter_target_frames: jitter_frames,
        };
        // 多路 AES67（IO I3c）：额外的流从 JSON 文件读入；宽度/块长/速率继承主流。
        let additional_inputs = match env_optional("KHAEN_AES67_ADDITIONAL_STREAMS") {
            Some(path) => {
                let text = read_streams_file(&path)?;
                let streams = parse_additional_streams(
                    &text,
                    sample_rate,
                    block_frames,
                    input_channels,
                    jitter_frames,
                )?;
                if streams.iter().any(|stream| {
                    stream.multicast_addr == input.multicast_addr && stream.port == input.port
                }) {
                    return Err(ConfigError::new(format!(
                        "KHAEN_AES67_ADDITIONAL_STREAMS duplicates the primary AES67 endpoint {}:{}",
                        input.multicast_addr, input.port
                    )));
                }
                streams
            }
            None => Vec::new(),
        };
        // IO I4b：可选 ALSA 采集输入（Some = 用本机 capture 替代 AES67 收包）。
        // 与 SDP/多流互斥：两套输入来源同时配置会含糊，按配置错误拒绝而不是静默忽略。
        let capture = match env_optional("KHAEN_ALSA_CAPTURE") {
            Some(device) => {
                if env_optional("KHAEN_AES67_SDP").is_some() {
                    return Err(ConfigError::new(
                        "KHAEN_ALSA_CAPTURE cannot be combined with KHAEN_AES67_SDP; choose one input source",
                    ));
                }
                if env_optional("KHAEN_AES67_ADDITIONAL_STREAMS").is_some() {
                    return Err(ConfigError::new(
                        "KHAEN_ALSA_CAPTURE cannot be combined with KHAEN_AES67_ADDITIONAL_STREAMS; choose one input source",
                    ));
                }
                let capture_channels: usize =
                    env_parse("KHAEN_ALSA_CAPTURE_CHANNELS", input_channels)?;
                let channels_map = match env_optional("KHAEN_ALSA_CAPTURE_MAP") {
                    Some(raw) => parse_channels_map(&raw)?,
                    None => (0..capture_channels as u16).collect(),
                };
                if channels_map.len() != capture_channels {
                    return Err(ConfigError::new(format!(
                        "KHAEN_ALSA_CAPTURE_MAP has {} entries but KHAEN_ALSA_CAPTURE_CHANNELS is {capture_channels}",
                        channels_map.len()
                    )));
                }
                let pending_blocks: usize = env_parse("KHAEN_ALSA_CAPTURE_PENDING_BLOCKS", 4)?;
                Some(khaen_host::capture::CaptureConfig {
                    device,
                    channels: capture_channels,
                    channels_map,
                    input_channels,
                    sample_rate,
                    block_frames,
                    pending_blocks,
                })
            }
            None => None,
        };
        let config = RuntimeConfig {
            additional_inputs,
            capture,
            input,
            engine: EngineConfig {
                block_frames,
                sample_rate,
                input_channels,
                output_channels,
            },
            device: DeviceConfig {
                device: DeviceId::new(alsa_device.clone()),
                sample_rate,
                block_frames,
                input_channels,
                output_channels,
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
            control,
            input_source,
        })
    }

    /// 把运行状态写成单行 JSON 文件（先写 .tmp 再 rename，读方不会看到半截）。
    ///
    /// 字段级契约见 docs/DEPLOY.md 4.3；只在控制/统计周期调用，不在 run_block 路径上。
    ///
    /// T42o 新增设备缺失字段（schema 仍是 khaen-rt-status/v1，向后兼容的纯增量）：
    /// `device_state`=present|absent、`absent_since`（进程内单调毫秒或 null）、
    /// `reopen_attempts`（本次缺失期间的重开次数）、`last_device_error`（null 或转义后的错误文本）。
    /// 设备缺失期间 `safe` 恒为 true（fail-closed：没有输出）。
    /// 旧入口（保持向后兼容）：不含 format/reconfigure 增量字段；仅测试使用。
    #[cfg(test)]
    pub fn write_status_file(
        path: &Path,
        status: &RuntimeStatus,
        lifecycle: &DeviceLifecycle,
    ) -> std::io::Result<()> {
        write_status_file_ext(path, status, lifecycle, None, None, None)
    }

    /// 增量版状态文件写入：`format`（数据面块长/速率/通道）与 `reconfigure`（M3b 热重配
    /// generation/last_gap_ms/last_result/在飞）是向后兼容的纯增量字段。
    pub fn write_status_file_ext(
        path: &Path,
        status: &RuntimeStatus,
        lifecycle: &DeviceLifecycle,
        format: Option<&EngineConfig>,
        reconfigure: Option<&crate::control::ReconfigureTracker>,
        test_tone: Option<&crate::control::test_tone::TestToneSlot>,
    ) -> std::io::Result<()> {
        let graph_id = match status.graph.graph_id.as_deref() {
            Some(id) => json_string(id),
            None => "null".to_string(),
        };
        let device_rate = match status.device_rate {
            Some(rate) => rate.to_string(),
            None => "null".to_string(),
        };
        let last_error = match status.last_error {
            Some(code) => json_string(code),
            None => "null".to_string(),
        };
        let ssrc = match status.input.ssrc {
            Some(ssrc) => ssrc.to_string(),
            None => "null".to_string(),
        };
        let absent_since = match lifecycle.absent_since_ms() {
            Some(ms) => ms.to_string(),
            None => "null".to_string(),
        };
        let last_device_error = match lifecycle.last_device_error() {
            Some(text) => json_string(text),
            None => "null".to_string(),
        };
        let device_state = if lifecycle.is_absent() {
            "absent"
        } else {
            "present"
        };
        let safe = status.state.requires_mute() || lifecycle.is_absent();
        let format_json = match format {
            Some(format) => serde_json::json!({
                "block_frames": format.block_frames,
                "sample_rate": format.sample_rate,
                // 向后兼容：channels 仍表示输出宽度；新增 input/output 明确拆分。
                "channels": format.output_channels,
                "input_channels": format.input_channels,
                "output_channels": format.output_channels,
            })
            .to_string(),
            None => "null".to_string(),
        };
        let reconfigure_json = match reconfigure {
            Some(tracker) => tracker.status_json().to_string(),
            None => "null".to_string(),
        };
        // IO I3d：每路 AES67 输入统计（加法字段；顺序与 channels_status.input_streams 一致）。
        let input_streams_json: serde_json::Value = serde_json::Value::Array(
            status
                .input_streams
                .iter()
                .map(|stream| {
                    serde_json::json!({
                        "packets": stream.packets,
                        "frames_received": stream.frames_received,
                        "frames_filled": stream.frames_filled,
                        "underruns": stream.underruns,
                        "overruns": stream.overruns,
                        "lost": stream.lost,
                        "late": stream.late,
                        "malformed": stream.malformed,
                        "foreign_payloads": stream.foreign_payloads,
                        "ssrc": stream.ssrc,
                        "jitter_fill": stream.jitter_fill,
                        "asrc_bypasses": stream.asrc_bypasses,
                    })
                })
                .collect(),
        );
        let input_streams_json = input_streams_json.to_string();
        // 通道识别测试音（加法字段；没有控制通道时为 null，读方按不可用处理）。
        let test_tone_json = match test_tone {
            Some(slot) => slot.status_json().to_string(),
            None => "null".to_string(),
        };
        // T41：ASRC 状态（未启用为 null；纯增量字段，旧读方忽略即可）。
        let asrc_json = match status.input.asrc {
            Some(asrc) => {
                let last_error = match asrc.last_error {
                    Some(code) => json_string(code),
                    None => "null".to_string(),
                };
                format!(
                    "{{\"enabled\":{},\"ratio_ppm\":{},\"ppm_estimate\":{},\"fill_frames\":{},\"last_error\":{}}}",
                    asrc.enabled, asrc.ratio_ppm, asrc.ppm_estimate, asrc.fill_frames, last_error
                )
            }
            None => "null".to_string(),
        };
        // IO I4b：采集统计（AES67 模式为 null；纯增量字段）。
        let capture_json = match &status.capture {
            Some(capture) => format!(
                "{{\"blocks_captured\":{},\"frames_captured\":{},\"frames_delivered\":{},\"blocks_skipped\":{},\"frames_dropped\":{},\"partial_blocks\":{},\"queue_full\":{},\"underruns\":{},\"xruns\":{},\"read_errors\":{},\"running\":{},\"last_error\":{}}}",
                capture.blocks_captured,
                capture.frames_captured,
                capture.frames_delivered,
                capture.blocks_skipped,
                capture.frames_dropped,
                capture.partial_blocks,
                capture.queue_full,
                capture.underruns,
                capture.xruns,
                capture.read_errors,
                capture.running,
                match capture.last_error {
                    Some(code) => json_string(code),
                    None => "null".to_string(),
                }
            ),
            None => "null".to_string(),
        };
        let json = format!(
            concat!(
                "{{\"schema\":\"khaen-rt-status/v1\",\"blocks\":{},",
                "\"state\":\"{}\",\"muted\":{},\"safe\":{},\"program_mute\":{},\"calibration_bypass\":{},\"input_paused\":{},",
                "\"plan_rate\":{},\"device_rate\":{},\"rate_asserted\":{},",
                "\"input\":{{\"packets\":{},\"underruns\":{},\"jitter_fill\":{},\"ssrc\":{},\"asrc\":{}}},",
                "\"output\":{{\"xruns\":{},\"blocks_written\":{},\"bytes_written\":{}}},",
                "\"receiver\":{{\"packets_received\":{},\"queue_full_drops\":{},\"recv_errors\":{}}},",
                "\"graph_id\":{},\"last_error\":{},",
                "\"device_state\":\"{}\",\"absent_since\":{},\"reopen_attempts\":{},\"last_device_error\":{},",
                "\"format\":{},\"reconfigure\":{},\"input_streams\":{},\"input_mode\":\"{}\",\"capture\":{},\"speaker_test\":{}}}\n"
            ),
            status.blocks,
            status.state.as_str(),
            status.muted,
            safe,
            status.output_stage.program_mute_applied,
            status.output_stage.calibration_bypass,
            status.input_paused,
            status.plan_rate,
            device_rate,
            status.rate_asserted,
            status.input.packets,
            status.input.underruns,
            status.input.jitter_fill,
            ssrc,
            asrc_json,
            status.output.xruns,
            status.output.blocks_written,
            status.output.bytes_written,
            status.receiver.packets_received,
            status.receiver.queue_full_drops,
            status.receiver.recv_errors,
            graph_id,
            last_error,
            device_state,
            absent_since,
            lifecycle.reopen_attempts(),
            last_device_error,
            format_json,
            reconfigure_json,
            input_streams_json,
            status.input_mode,
            capture_json,
            test_tone_json
        );
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, json.as_bytes())?;
        std::fs::rename(&temporary, path)
    }

    /// 把文本编码成 JSON 字符串字面量（含两侧引号）；引号/反斜杠/控制字符都转义。
    fn json_string(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                control if control < ' ' => {
                    out.push_str(&format!("\\u{:04x}", u32::from(control)));
                }
                other => out.push(other),
            }
        }
        out.push('"');
        out
    }

    pub fn print_status(status: &RuntimeStatus, lifecycle: &DeviceLifecycle) {
        let device = &status.device;
        println!(
            "blocks={} recv_pkts={} fill={} underruns={} xruns={} out_errors={} \
             state={} device_state={} pcm={} absent_since={:?} reopen_attempts={} \
             plan_rate={} device_rate={:?} rate_asserted={} graph={:?} last_error={:?}",
            status.blocks,
            status.receiver.packets_received,
            status.input.jitter_fill,
            status.input.underruns,
            status.output.xruns,
            status.output_errors,
            status.state.as_str(),
            device.state.as_str(),
            if lifecycle.is_absent() {
                "absent"
            } else {
                "present"
            },
            lifecycle.absent_since_ms(),
            lifecycle.reopen_attempts(),
            status.plan_rate,
            status.device_rate,
            status.rate_asserted,
            status.graph.graph_id,
            status.last_error
        );
        if let Some(error) = lifecycle.last_device_error() {
            println!("last_device_error={error:?}");
        }
    }

    /// 数据面主循环（平台无关；`P` 为 PCM 设备类型，真机是 AlsaPcm，Windows 不实例化）。
    ///
    /// 设备生命周期交给 [`crate::recovery::drive_loop`]：声卡不在位/掉线都**不退出**，
    /// 而是 fail-closed + 按退避重开。这里只负责配置解析、运行时构造、方案预检/加载，
    /// 以及把不可恢复的配置/方案错误映射成稳定退出码。
    pub fn run<P: PcmOutput + 'static>() -> ExitCode {
        let settings = match build_settings() {
            Ok(value) => value,
            Err(err) => {
                eprintln!("khaen-rt-host config error: {}", err.message());
                return ExitCode::from(2);
            }
        };
        println!(
            "khaen-rt-host: device={} rate={} block={} in_channels={} out_channels={} aes67={}:{} plan={:?}",
            settings.alsa_device,
            settings.config.engine.sample_rate,
            settings.config.engine.block_frames,
            settings.config.engine.input_channels,
            settings.config.engine.output_channels,
            settings.config.input.multicast_addr,
            settings.config.input.port,
            settings.plan_path
        );

        // 设备延迟打开：声卡不在位时构造不失败（旧实现会直接退出）；后续由监督循环重开。
        let mut runtime: Runtime<P> = match Runtime::new_deferred(settings.config.clone()) {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!(
                    "khaen-rt-host: cannot open runtime: [{:?}] {err}",
                    err.code()
                );
                return ExitCode::from(1);
            }
        };

        // 方案先于设备校验/加载：文件/ABI/准入错误属于不可恢复的配置类错误，必须退出
        // （即使声卡此刻不在位）；设备缺失不影响方案加载与 version/graph_id 语义。
        let mut initial_plan: Option<(Plan, u64)> = None;
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
                Ok(version) => {
                    // 控制通道的 surface 补丁需要"最近一次已生效方案"作基线：这里按预检结果
                    // 再解码一次（启动期一次性开销；预检已证明能解码）。
                    match Plan::decode(&startup.bytes) {
                        Ok(plan) => initial_plan = Some((plan, version)),
                        Err(err) => {
                            eprintln!(
                                "khaen-rt-host: startup plan cannot be decoded for the control channel: {err}"
                            );
                            return ExitCode::from(2);
                        }
                    }
                    println!(
                        "khaen-rt-host: plan applied at version {version} (graph_id {})",
                        startup.graph_id
                    );
                }
                Err(err) => {
                    eprintln!("khaen-rt-host: plan rejected: [{:?}] {err}", err.code());
                    return ExitCode::from(1);
                }
            }
        }

        // 热重配追踪器（M3b）：块线程在块边界执行 channels_commit 时写，控制核心与状态文件读。
        // 没有控制通道时也创建（状态文件要如实报 generation/last_gap/last_result）。
        let reconfigure = crate::control::shared_tracker();
        // 跨进程控制通道（T70f-a）：数据面优先——控制通道起不来只打印原因并继续，不退出进程。
        // 块线程是 Runtime 的唯一 owner；控制线程只发块边界命令并读状态快照。
        let mut bridge =
            ControlBridge::disabled().with_reconfigure(std::sync::Arc::clone(&reconfigure));
        let mut control_server: Option<crate::control::ControlServer> = None;
        // 控制核心句柄（校准测量线程在进程退出前要取消/join；只有控制通道起来才存在）。
        let mut shutdown_handle: Option<Arc<Mutex<crate::control::ControlCore>>> = None;
        if settings.control.enabled {
            let (command_tx, command_rx) = mpsc::channel();
            let snapshot: SharedSnapshot = Arc::new(Mutex::new(None));
            // 进程内校准（T70f-c）：配置非法只让 cal_* 保持 not_wired，数据面照常跑。
            // 命令通道是块线程的唯一入口：真机校准播放也要经它（不新开 socket/不换设备 owner）。
            let calibration = match crate::control::CalibrationConfig::from_env() {
                Ok(None) => None,
                Ok(Some(config)) => {
                    let factory =
                        crate::control::cal::engine::build_factory(&config, command_tx.clone());
                    println!(
                        "khaen-rt-host: in-process calibration enabled (engine {}, kind {}, simulated={}, artifact dir {})",
                        factory.engine_name(),
                        factory.kind().as_str(),
                        factory.simulated(),
                        config.artifact_dir.display()
                    );
                    Some(crate::control::CalibrationSetup { config, factory })
                }
                Err(err) => {
                    eprintln!(
                        "khaen-rt-host: calibration configuration rejected ({}); calibration ops stay not_wired",
                        err.message()
                    );
                    None
                }
            };
            let (initial_plan_value, initial_version) = match initial_plan {
                Some((plan, version)) => (Some(plan), version),
                None => (None, 0),
            };
            // 热重配协议上下文（M3b）：启动时的完整运行时配置 + 只读 /proc/asound 探测。
            let channels = crate::control::ChannelsConfig {
                config: settings.config.clone(),
                probe: Arc::new(crate::control::AlsaDeviceProbe::new(
                    settings.alsa_device.clone(),
                    Path::new("/proc/asound").to_path_buf(),
                )),
                input_source: Some(settings.input_source.clone()),
            };
            let core = ControlCore::new(ControlCoreConfig {
                data_plane_format: runtime.config().engine.clone(),
                limits: AdmissionLimits::default(),
                commands: command_tx,
                snapshot: Arc::clone(&snapshot),
                mode: khaen_host::control::RuntimeMode::Aes67,
                initial_plan: initial_plan_value,
                initial_version,
                command_timeout: crate::control::core::COMMAND_ACK_TIMEOUT,
                calibration,
                channels: Some(channels),
                reconfigure: Some(std::sync::Arc::clone(&reconfigure)),
            });
            // 测试音会话槽：状态文件/桥读同一份（只有控制通道起来后才有）。
            let test_tone_shared: std::sync::Arc<crate::control::test_tone::TestToneSlot> =
                core.test_tone_shared();
            let core = Arc::new(Mutex::new(core));
            let core_handle = Arc::clone(&core);
            match crate::control::start(&settings.control, core) {
                Ok(server) => {
                    shutdown_handle = Some(core_handle);
                    if let Some(path) = server.socket_path() {
                        println!(
                            "khaen-rt-host: control channel listening on {} (KHAEN_CONTROL=off disables it)",
                            path.display()
                        );
                    }
                    control_server = Some(server);
                    bridge = ControlBridge::new(command_rx, snapshot, settings.stats_every)
                        .with_reconfigure(std::sync::Arc::clone(&reconfigure))
                        .with_test_tone(std::sync::Arc::clone(&test_tone_shared));
                }
                Err(err) => {
                    eprintln!(
                        "khaen-rt-host: control channel unavailable ({err}); data plane continues without runtime control"
                    );
                }
            }
        } else {
            println!("khaen-rt-host: control channel disabled by KHAEN_CONTROL=off");
        }

        let options = crate::recovery::LoopOptions {
            alsa_device: settings.alsa_device,
            proc_asound: Path::new("/proc/asound").to_path_buf(),
            status_file: settings.status_file,
            run_seconds: settings.run_seconds,
            stats_every: settings.stats_every,
        };
        let mut pacer = crate::recovery::RealPacer::new();
        // control_server 必须活到块循环结束（Drop 时停止 accept 并移除 socket）。
        // 无论控制通道是否起来都走带桥的主循环：桥 disabled 时所有命令/快照操作是空操作
        // （与既有 T42o 行为一致），但热重配追踪器始终是 app 的这一份（状态文件如实读它）。
        let exit = crate::recovery::drive_loop_with_control(
            &mut runtime,
            &options,
            &mut pacer,
            &mut bridge,
        )
        .0;
        // 数据面已停：先让进程内校准取消并等测量线程收尾（不把半截测量线程留到进程退出），
        // 再 Drop 控制通道句柄（停止 accept 并移除 socket 文件）。
        if let Some(handle) = shutdown_handle {
            match handle.lock() {
                Ok(mut core) => core.shutdown(),
                Err(poisoned) => poisoned.into_inner().shutdown(),
            }
        }
        #[cfg(unix)]
        drop(control_server);
        #[cfg(not(unix))]
        let _ = control_server;
        exit
    }
}

#[cfg(test)]
mod tests {
    use super::app;
    use crate::recovery::DeviceLifecycle;
    use khaen_host::aes67_input::AsrcInputStats;
    use khaen_host::alsa_backend::AlsaStats;
    use khaen_host::runtime::{ReceiverStats, RuntimeStatus};
    use khaen_host::safety::RuntimeState;
    use khaen_host::supervisor::{LifecycleState, SupervisorStatus};

    #[test]
    fn additional_streams_json_is_strict_and_validated() {
        let text = r#"[
            {"group":"239.69.1.2","port":5006,"payload_type":97,"channels":2,"map":[2,3],"jitter_frames":64},
            {"group":"239.69.1.3","port":5008,"payload_type":98,"channels":1,"map":[1]}
        ]"#;
        let streams = app::parse_additional_streams(text, 48_000, 128, 4, 192)
            .unwrap_or_else(|error| panic!("parse streams: {}", error.message()));
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].multicast_addr.to_string(), "239.69.1.2");
        assert_eq!(streams[0].port, 5006);
        assert_eq!(streams[0].channels_map, vec![2, 3]);
        assert_eq!(streams[0].jitter_target_frames, 64);
        assert_eq!(
            streams[1].jitter_target_frames, 192,
            "default jitter inherited"
        );
        assert_eq!(streams[1].output_channels, 4);

        for bad in [
            r#"[{"group":"239.69.1.2","port":5006,"payload_type":97,"channels":2,"map":[2,3],"extra":1}]"#,
            r#"[{"group":"239.69.1.2","port":5006,"payload_type":97,"channels":2,"map":[4,3]}]"#,
            r#"[{"group":"239.69.1.2","port":5006,"payload_type":97,"channels":2,"map":[1]}]"#,
            r#"{"group":"239.69.1.2"}"#,
            r#"[{"group":"239.69.1.2","port":5006,"payload_type":97,"channels":2,"map":[2,3]},{"group":"239.69.1.2","port":5006,"payload_type":97,"channels":2,"map":[0,1]}]"#,
        ] {
            assert!(
                app::parse_additional_streams(bad, 48_000, 128, 4, 192).is_err(),
                "must reject {bad}"
            );
        }
    }

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
            input_streams: vec![khaen_host::aes67_input::Aes67InputStats {
                packets: 34,
                underruns: 2,
                jitter_fill: 192,
                ..Default::default()
            }],
            input_mode: "aes67",
            capture: None,
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
            output_stage: Default::default(),
            input_paused: false,
            last_error: None,
        }
    }

    #[test]
    fn status_file_is_valid_single_line_json() {
        let mut path = std::env::temp_dir();
        path.push(format!("khaen-rt-host-status-{}.json", std::process::id()));
        app::write_status_file(&path, &status(), &DeviceLifecycle::new())
            .expect("write status file");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(text.matches('\n').count(), 1, "must be a single line");
        assert!(text.ends_with('\n'));
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(doc["schema"], serde_json::json!("khaen-rt-status/v1"));
        assert_eq!(doc["blocks"], serde_json::json!(12));
        assert_eq!(doc["state"], serde_json::json!("running"));
        assert_eq!(doc["muted"], serde_json::json!(false));
        assert_eq!(doc["safe"], serde_json::json!(false));
        assert_eq!(doc["plan_rate"], serde_json::json!(48_000));
        assert_eq!(doc["device_rate"], serde_json::Value::Null);
        assert_eq!(doc["input"]["packets"], serde_json::json!(34));
        assert_eq!(doc["input"]["ssrc"], serde_json::Value::Null);
        assert_eq!(
            doc["input_streams"].as_array().map(Vec::len),
            Some(1),
            "per-stream stats must be an array (primary stream only in this fixture)"
        );
        assert_eq!(doc["input_streams"][0]["packets"], serde_json::json!(34));
        assert_eq!(doc["input_mode"], serde_json::json!("aes67"));
        assert_eq!(doc["capture"], serde_json::Value::Null);
        // T41：未启用 ASRC 时 input.asrc 为 null（纯增量字段）。
        assert_eq!(doc["input"]["asrc"], serde_json::Value::Null);
        assert_eq!(doc["graph_id"], serde_json::Value::Null);
        assert_eq!(doc["last_error"], serde_json::Value::Null);
        // T42o 设备缺失字段：present 时 absent_since/last_device_error 为 null、attempts=0。
        assert_eq!(doc["device_state"], serde_json::json!("present"));
        assert_eq!(doc["absent_since"], serde_json::Value::Null);
        assert_eq!(doc["reopen_attempts"], serde_json::json!(0));
        assert_eq!(doc["last_device_error"], serde_json::Value::Null);
        // 原子替换：临时文件不残留。
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn status_file_reports_capture_stats_in_capture_mode() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-rt-host-status-capture-{}.json",
            std::process::id()
        ));
        let mut runtime_status = status();
        runtime_status.input_mode = "alsa_capture";
        runtime_status.capture = Some(khaen_host::capture::CaptureStats {
            blocks_captured: 7,
            frames_captured: 896,
            frames_delivered: 768,
            blocks_skipped: 1,
            frames_dropped: 128,
            partial_blocks: 0,
            queue_full: 0,
            underruns: 2,
            xruns: 1,
            read_errors: 0,
            recycle_full: 0,
            running: true,
            last_error: Some("xrun: underrun"),
        });
        app::write_status_file(&path, &runtime_status, &DeviceLifecycle::new())
            .expect("write status file");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(doc["input_mode"], serde_json::json!("alsa_capture"));
        assert_eq!(doc["capture"]["blocks_captured"], serde_json::json!(7));
        assert_eq!(doc["capture"]["underruns"], serde_json::json!(2));
        assert_eq!(doc["capture"]["xruns"], serde_json::json!(1));
        assert_eq!(doc["capture"]["running"], serde_json::json!(true));
        assert_eq!(
            doc["capture"]["last_error"],
            serde_json::json!("xrun: underrun")
        );
    }

    #[test]
    fn status_file_reports_asrc_snapshot() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-rt-host-status-asrc-{}.json",
            std::process::id()
        ));
        let mut runtime = status();
        runtime.input.asrc = Some(AsrcInputStats {
            enabled: true,
            ratio_ppm: -200,
            ppm_estimate: -199,
            fill_frames: 256,
            last_error: Some("ERR_ASRC_PROCESS"),
        });
        runtime.input.asrc_bypasses = 3;
        app::write_status_file(&path, &runtime, &DeviceLifecycle::new())
            .expect("write status file");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(doc["input"]["asrc"]["enabled"], serde_json::json!(true));
        assert_eq!(doc["input"]["asrc"]["ratio_ppm"], serde_json::json!(-200));
        assert_eq!(
            doc["input"]["asrc"]["ppm_estimate"],
            serde_json::json!(-199)
        );
        assert_eq!(doc["input"]["asrc"]["fill_frames"], serde_json::json!(256));
        assert_eq!(
            doc["input"]["asrc"]["last_error"],
            serde_json::json!("ERR_ASRC_PROCESS")
        );
    }

    #[test]
    fn status_file_includes_format_and_reconfigure_delta() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-rt-host-status-hot-{}.json",
            std::process::id()
        ));
        let tracker = crate::control::ReconfigureTracker {
            generation: 3,
            last_gap_ms: Some(37),
            last_result: Some(serde_json::json!({
                "ok": true,
                "applied": true,
                "gap_ms": 37,
                "note": "\"quoted\" 文本要正确转义",
            })),
            ..crate::control::ReconfigureTracker::default()
        };
        let format = khaen_host::engine::EngineConfig {
            block_frames: 128,
            sample_rate: 48_000,
            input_channels: 4,
            output_channels: 4,
        };
        // 测试音会话槽（active 状态）：状态文件同样要上报 speaker_test 增量字段。
        let test_tone = crate::control::test_tone::TestToneSlot::shared();
        let progress = std::sync::Arc::new(khaen_host::runtime::CalibrationProgress::new(10));
        let request = crate::control::test_tone::TestToneRequest {
            dac: 3,
            ms: 1_000,
            kind: crate::control::test_tone::TestToneKind::Sine,
            freq_hz: Some(1_000.0),
            level_db: -30.0,
        };
        test_tone.install(&request, progress, 10, 128, 48_000);
        app::write_status_file_ext(
            &path,
            &status(),
            &DeviceLifecycle::new(),
            Some(&format),
            Some(&tracker),
            Some(&test_tone),
        )
        .expect("write status file with deltas");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(text.matches('\n').count(), 1, "must be a single line");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        // schema 不变：只加向后兼容的增量字段。
        assert_eq!(doc["schema"], serde_json::json!("khaen-rt-status/v1"));
        assert_eq!(doc["blocks"], serde_json::json!(12));
        assert_eq!(doc["format"]["block_frames"], serde_json::json!(128));
        assert_eq!(doc["format"]["sample_rate"], serde_json::json!(48_000));
        assert_eq!(doc["format"]["channels"], serde_json::json!(4));
        assert_eq!(doc["reconfigure"]["generation"], serde_json::json!(3));
        assert_eq!(doc["reconfigure"]["last_gap_ms"], serde_json::json!(37));
        assert_eq!(
            doc["reconfigure"]["last_result"]["applied"],
            serde_json::json!(true)
        );
        assert_eq!(
            doc["reconfigure"]["last_result"]["note"],
            serde_json::json!("\"quoted\" 文本要正确转义")
        );
        // 测试音增量字段：只有目标 DAC + 种类 + 剩余时间。
        assert_eq!(doc["speaker_test"]["active"], serde_json::json!(true));
        assert_eq!(doc["speaker_test"]["dac"], serde_json::json!(3));
        assert_eq!(doc["speaker_test"]["kind"], serde_json::json!("sine"));
        assert_eq!(doc["speaker_test"]["remaining_ms"], serde_json::json!(26));

        // 旧入口（不带增量参数）仍写 `null`，老调用方/测试行为不变。
        app::write_status_file(&path, &status(), &DeviceLifecycle::new())
            .expect("write legacy status file");
        let text = std::fs::read_to_string(&path).expect("read legacy status file");
        let _ = std::fs::remove_file(&path);
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert!(doc["format"].is_null());
        assert!(doc["reconfigure"].is_null());
        assert!(doc["speaker_test"].is_null());
    }

    #[test]
    fn status_file_marks_absent_device_safe_and_escapes_error() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-rt-host-status-absent-{}.json",
            std::process::id()
        ));
        let mut lifecycle = DeviceLifecycle::new();
        // 设备缺失：safe 必须为 true（fail-closed），并带上缺失起点/重开次数/错误文本。
        lifecycle.note_absent(1_234, "alsa pcm disconnected: \"hot\" \\ path");
        let _ = lifecycle.note_attempt(1_234);
        let _ = lifecycle.note_attempt(3_234);
        app::write_status_file(&path, &status(), &lifecycle).expect("write status file");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(doc["schema"], serde_json::json!("khaen-rt-status/v1"));
        assert_eq!(doc["device_state"], serde_json::json!("absent"));
        assert_eq!(doc["safe"], serde_json::json!(true));
        assert_eq!(doc["absent_since"], serde_json::json!(1_234));
        assert_eq!(doc["reopen_attempts"], serde_json::json!(2));
        // 错误文本里的引号/反斜杠必须被 JSON 正确转义（否则状态文件不是合法 JSON）。
        assert_eq!(
            doc["last_device_error"],
            serde_json::json!("alsa pcm disconnected: \"hot\" \\ path")
        );

        // 恢复后：present，absent_since 回到 null、attempts 清零；last_device_error 保留"最近一次"语义。
        lifecycle.note_recovered(9_000);
        app::write_status_file(&path, &status(), &lifecycle).expect("write recovered status file");
        let text = std::fs::read_to_string(&path).expect("read status file");
        let _ = std::fs::remove_file(&path);
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(doc["device_state"], serde_json::json!("present"));
        assert_eq!(doc["safe"], serde_json::json!(false));
        assert_eq!(doc["absent_since"], serde_json::Value::Null);
        assert_eq!(doc["reopen_attempts"], serde_json::json!(0));
        assert_eq!(
            doc["last_device_error"],
            serde_json::json!("alsa pcm disconnected: \"hot\" \\ path")
        );
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
    fn parses_alsa_device_names() {
        assert_eq!(
            app::parse_alsa_device("hw:0"),
            Some(app::AlsaCard::Index(0))
        );
        assert_eq!(
            app::parse_alsa_device("hw:2,1"),
            Some(app::AlsaCard::Index(2))
        );
        assert_eq!(
            app::parse_alsa_device("hw:CARD=UltraLitemk5,DEV=0"),
            Some(app::AlsaCard::Id("UltraLitemk5".to_string()))
        );
        assert_eq!(app::parse_alsa_device("default"), None);
        assert_eq!(app::parse_alsa_device("plughw:CARD=MOTU"), None);
        assert_eq!(app::device_pcm_index("hw:0"), 0);
        assert_eq!(app::device_pcm_index("hw:2,1"), 1);
        assert_eq!(app::device_pcm_index("hw:CARD=UltraLitemk5,DEV=0"), 0);
        assert_eq!(app::device_pcm_index("hw:CARD=UltraLitemk5,DEV=3"), 3);
    }

    #[test]
    fn resolves_card_index_from_cards_text() {
        let cards = " 0 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n                      MOTU UltraLite-mk5 at usb-0000:00:14.0-2.1, high speed\n 1 [U18dB          ]: USB-Audio - Umik-1  Gain: 18dB\n";
        assert_eq!(app::card_index_for_id(cards, "UltraLitemk5"), Some(0));
        assert_eq!(app::card_index_for_id(cards, "U18dB"), Some(1));
        assert_eq!(app::card_index_for_id(cards, "MOTU"), None);
        assert_eq!(
            app::resolve_card_index("hw:CARD=UltraLitemk5,DEV=0", cards),
            Some(0)
        );
        assert_eq!(app::resolve_card_index("hw:4,0", cards), Some(4));
        assert_eq!(app::resolve_card_index("default", cards), None);
    }

    #[test]
    fn parses_hw_params_rate_and_reads_proc_tree() {
        assert_eq!(app::parse_hw_params_rate("closed\n"), None);
        assert_eq!(
            app::parse_hw_params_rate(
                "access: RW_INTERLEAVED\nformat: S24_3LE\nrate: 192000 (192000/1)\nchannels: 22\n"
            ),
            Some(192_000)
        );
        assert_eq!(
            app::parse_hw_params_rate("rate: 48000 (48000/1)\n"),
            Some(48_000)
        );

        let root = std::env::temp_dir().join(format!(
            "khaen-rt-host-proc-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let sub = root.join("card0").join("pcm0p").join("sub0");
        std::fs::create_dir_all(&sub).expect("proc tree");
        std::fs::write(sub.join("hw_params"), "rate: 192000 (192000/1)\n").expect("hw_params");
        assert_eq!(app::read_hw_rate(&root, 0, 0), Some(192_000));
        assert_eq!(app::read_hw_rate(&root, 1, 0), None);
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn device_rate_mismatch_fails_closed() {
        assert_eq!(app::check_device_rate(48_000, Some(48_000)), Ok(()));
        assert_eq!(app::check_device_rate(48_000, None), Ok(()));
        assert_eq!(
            app::check_device_rate(48_000, Some(192_000)),
            Err(app::DEVICE_RATE_MISMATCH)
        );
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
    fn capture_env_builds_capture_config() {
        std::env::set_var("KHAEN_ALSA_CAPTURE", "hw:CARD=FAKE,DEV=0");
        std::env::set_var("KHAEN_ALSA_CAPTURE_CHANNELS", "2");
        std::env::set_var("KHAEN_ALSA_CAPTURE_MAP", "0,1");
        let settings = app::build_settings().unwrap_or_else(|err| panic!("{}", err.message()));
        let capture = settings.config.capture.expect("capture config");
        assert_eq!(capture.device, "hw:CARD=FAKE,DEV=0");
        assert_eq!(capture.channels, 2);
        assert_eq!(capture.channels_map, vec![0, 1]);
        assert_eq!(
            capture.input_channels,
            settings.config.engine.input_channels
        );
        assert_eq!(capture.sample_rate, settings.config.engine.sample_rate);
        assert_eq!(capture.block_frames, settings.config.engine.block_frames);
        assert!(settings.config.additional_inputs.is_empty());
        std::env::remove_var("KHAEN_ALSA_CAPTURE");
        std::env::remove_var("KHAEN_ALSA_CAPTURE_CHANNELS");
        std::env::remove_var("KHAEN_ALSA_CAPTURE_MAP");
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
