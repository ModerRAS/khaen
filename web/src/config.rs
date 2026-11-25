//! 配置模型与加载。
//!
//! 默认路径：Linux \`/etc/khaen-audio-web/config.toml\`，Windows（开发）\`./config.toml\`。
//! 支持环境变量覆盖（见 README），TSPI_MOCK=1 时无需配置文件。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PLACEHOLDER_TOKEN: &str = "CHANGE_ME";

/// 默认配置文件内容（deploy/config.toml.example 内嵌，避免两份文档漂移）。
pub const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../deploy/config.toml.example");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// 监听地址。生产默认 0.0.0.0:80：局域网直接访问 http://<设备IP>/，无需记端口；
    /// systemd unit 以 root 运行，可直接绑定 <1024 端口；8080 留给 AES67 daemon 自带 Web UI。
    /// 如 80 被占用/无权限，改 0.0.0.0:8088 回退。
    pub listen: String,
    /// 访问 token。空值或 CHANGE_ME 时首次启动自动生成并写回配置文件。
    pub token: String,
    /// 登录会话有效期（秒）。
    pub session_ttl_secs: u64,
    /// 管理密码哈希文件（Argon2id，0600；不存在=尚未设置密码，首启显示设置页）。
    pub auth_file: String,
    /// 是否给会话 Cookie 加 Secure（仅 HTTPS 部署时开启）。
    pub cookie_secure: bool,
    /// journalctl 默认读取行数。
    pub log_lines: usize,
    /// TSPI_MOCK=1 时强制开启；一般不在配置文件里写。
    pub mock: bool,
    pub camilladsp: CamillaConfig,
    pub ptp: PtpConfig,
    pub aes67: AoipConfig,
    pub inferno: AoipConfig,
    pub alsa: AlsaConfig,
    pub system: PowerConfig,
    pub mode: ModeConfig,
    pub channelmap: ChannelmapConfig,
    pub motu: MotuConfig,
    /// 备用口令（登录限速用的失败锁定时长，秒）。
    pub login_lockout_secs: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:80".into(),
            token: PLACEHOLDER_TOKEN.into(),
            session_ttl_secs: 12 * 3600,
            auth_file: "/etc/khaen-audio-web/auth.json".into(),
            cookie_secure: false,
            log_lines: 100,
            mock: false,
            camilladsp: CamillaConfig::default(),
            ptp: PtpConfig::default(),
            aes67: AoipConfig::aes67_default(),
            inferno: AoipConfig::inferno_default(),
            alsa: AlsaConfig::default(),
            system: PowerConfig::default(),
            mode: ModeConfig::default(),
            channelmap: ChannelmapConfig::default(),
            motu: MotuConfig::default(),
            login_lockout_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CamillaConfig {
    /// CamillaDSP websocket 地址。
    pub ws_url: String,
    /// systemd 单元名（F-04 legacy 只读展示：旧 web 不再提供 CamillaDSP 启停接口；
    /// 该值只用于 status/logs 的历史对照。板端旧 C 实际 unit 是 tspi-camilladsp.service）。
    pub service: String,
    /// 当前生效配置（CamillaDSP 启动读取的文件，切换配置时同步写这里）。
    pub config_file: String,
    /// 可选配置目录。
    pub config_dir: String,
    /// 单次 websocket 查询超时（毫秒）。
    pub ws_timeout_ms: u64,
}

impl Default for CamillaConfig {
    fn default() -> Self {
        Self {
            ws_url: "ws://127.0.0.1:1234".into(),
            service: "tspi-camilladsp.service".into(),
            config_file: "/etc/camilladsp/config.yml".into(),
            config_dir: "/etc/camilladsp/configs".into(),
            ws_timeout_ms: 3000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PtpConfig {
    /// ptp4l 配置文件。
    pub conf_file: String,
    /// pmc 可执行文件。
    pub pmc_bin: String,
    /// PTPv2（linuxptp）服务。
    pub ptp4l_service: String,
    pub phc2sys_service: String,
    /// PTPv1（Inferno/Statime）服务，用于 Dante 模式展示。
    pub statime_service: String,
    /// 认为“已锁定”的 offset 阈值（纳秒）。
    pub lock_offset_threshold_ns: i64,
}

impl Default for PtpConfig {
    fn default() -> Self {
        Self {
            conf_file: "/etc/ptp4l.conf".into(),
            pmc_bin: "pmc".into(),
            ptp4l_service: "ptp4l.service".into(),
            phc2sys_service: "phc2sys.service".into(),
            statime_service: "statime.service".into(),
            lock_offset_threshold_ns: 1_000_000,
        }
    }
}

/// AES67 daemon 与 Inferno 共用的“AoIP 服务”配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AoipConfig {
    pub service: String,
    /// 只读展示的配置/环境文件（第一个存在者优先展示，全部会列出）。
    pub config_files: Vec<String>,
    /// 自带 Web UI 地址（仅展示）。
    pub webui_url: String,
    /// journalctl 行数。
    pub log_lines: usize,
}

impl Default for AoipConfig {
    fn default() -> Self {
        Self::aes67_default()
    }
}

impl AoipConfig {
    pub fn aes67_default() -> Self {
        Self {
            service: "aes67-daemon.service".into(),
            config_files: vec![
                "/etc/aes67-daemon/aes67-daemon.json".into(),
                "/etc/aes67-daemon.json".into(),
            ],
            webui_url: "http://127.0.0.1:8080".into(),
            log_lines: 100,
        }
    }

    pub fn inferno_default() -> Self {
        Self {
            service: "inferno.service".into(),
            // Inferno 上游无配置文件：配置走 INFERNO_* 环境变量 / ALSA 插件参数。
            config_files: vec!["/etc/default/inferno".into(), "/etc/asound.conf".into()],
            webui_url: String::new(),
            log_lines: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlsaConfig {
    pub aplay_bin: String,
    /// 测试音默认设备（空 = 自动选 MOTU / 第一个 playback 设备）。
    pub default_device: String,
}

impl Default for AlsaConfig {
    fn default() -> Self {
        Self {
            aplay_bin: "aplay".into(),
            default_device: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PowerConfig {
    /// 默认关闭，需显式开启。
    pub allow_reboot: bool,
    pub allow_shutdown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    #[default]
    Off,
    Aes67,
    Dante,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Aes67 => "aes67",
            Self::Dante => "dante",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::Aes67 => "AES67",
            Self::Dante => "Dante",
        }
    }
}

impl std::str::FromStr for RunMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "aes67" => Ok(Self::Aes67),
            "dante" => Ok(Self::Dante),
            other => Err(format!("未知运行模式: {other}（可选 off/aes67/dante）")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModeProfile {
    /// 页面展示名。
    pub label: String,
    /// PTP 版本展示（PTPv1 / PTPv2）。
    pub ptp_version: String,
    /// 该模式使用的 PTP 服务（按启动顺序）。空 = PTP 由 AoIP 服务内部实现（B1）。
    pub ptp_services: Vec<String>,
    /// PTP 来源：aes67-daemon / statime / linuxptp（展示 + 状态读取策略）。
    pub ptp_source: String,
    /// PTP 角色：slave / master / none。
    pub ptp_role: String,
    /// 该模式使用的 AoIP 服务。
    pub aoip_service: String,
    /// 该模式的 CamillaDSP 配置文件。
    pub camilla_config: String,
    /// 输入格式（展示 + 格式页默认值）。
    pub input_sample_rate: u32,
    pub input_bit_depth: u32,
    pub channels: u32,
    /// 输出（MOTU）格式。
    pub output_sample_rate: u32,
    pub output_bit_depth: u32,
    /// 默认 chunksize / target_level。
    pub default_chunksize: u32,
    pub default_target_level: u32,
}

impl Default for ModeProfile {
    fn default() -> Self {
        Self {
            label: String::new(),
            ptp_version: "PTPv2".into(),
            ptp_services: Vec::new(),
            ptp_source: "linuxptp".into(),
            ptp_role: "slave".into(),
            aoip_service: String::new(),
            camilla_config: String::new(),
            input_sample_rate: 48000,
            input_bit_depth: 24,
            channels: 2,
            output_sample_rate: 48000,
            output_bit_depth: 24,
            default_chunksize: 128,
            default_target_level: 2048,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModeConfig {
    /// 是否允许通过 Web 切换运行模式。
    pub enabled: bool,
    /// 当前模式持久化文件。
    pub state_file: String,
    /// audio-stack 统一脚本（dante|aes67|off）。为空或文件不存在时走内置编排。
    pub switch_script: String,
    /// 启动时恢复上次模式。
    pub restore_on_start: bool,
    /// 单次切换脚本超时。
    pub switch_timeout_secs: u64,
    /// 等待 PTP 锁定超时。
    pub ptp_lock_timeout_secs: u64,
    /// 等待 AoIP 服务就绪超时。
    pub aoip_ready_timeout_secs: u64,
    /// 等待 CamillaDSP 进入 Running 超时。
    pub camilla_ready_timeout_secs: u64,
    /// 状态轮询间隔。
    pub poll_ms: u64,
    pub dante: ModeProfile,
    pub aes67: ModeProfile,
}

impl Default for ModeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            state_file: "/etc/khaen-audio-web/state.json".into(),
            switch_script: "/usr/local/bin/tspi-mode".into(),
            // D4：默认开机不自动恢复音频模式（audio off，用户手动切换）。
            restore_on_start: false,
            switch_timeout_secs: 180,
            ptp_lock_timeout_secs: 60,
            aoip_ready_timeout_secs: 30,
            camilla_ready_timeout_secs: 30,
            poll_ms: 500,
            dante: ModeProfile {
                label: "Dante（Inferno / PTPv1）".into(),
                ptp_version: "PTPv1".into(),
                ptp_services: vec!["statime.service".into()],
                ptp_source: "statime".into(),
                ptp_role: "slave".into(),
                aoip_service: "inferno.service".into(),
                camilla_config: "/etc/camilladsp/configs/dante.yml".into(),
                input_sample_rate: 96000,
                input_bit_depth: 24,
                channels: 2,
                output_sample_rate: 96000,
                output_bit_depth: 24,
                default_chunksize: 128,
                default_target_level: 2048,
            },
            aes67: ModeProfile {
                label: "AES67（aes67-daemon / PTPv2）".into(),
                ptp_version: "PTPv2".into(),
                // 方案 B1（默认）：本机不做 PTP master，aes67-daemon 自带 PTPv2 slave，
                // 不拉起 ptp4l/phc2sys。要用 B2（本机 ptp4l 做 GM）时改为
                // vec!["ptp4l.service".into(), "phc2sys.service".into()] 并设 ptp_source = "linuxptp"。
                ptp_services: Vec::new(),
                ptp_source: "aes67-daemon".into(),
                ptp_role: "slave".into(),
                aoip_service: "aes67-daemon.service".into(),
                camilla_config: "/etc/camilladsp/configs/aes67.yml".into(),
                input_sample_rate: 48000,
                input_bit_depth: 24,
                channels: 2,
                output_sample_rate: 48000,
                output_bit_depth: 24,
                default_chunksize: 128,
                default_target_level: 2048,
            },
        }
    }
}

impl AppConfig {
    /// mock 模式默认配置（无需配置文件即可 `TSPI_MOCK=1 cargo run`）。
    /// 默认同样监听 80（与生产口径一致）；非 root 或 80 被占用时用
    /// `--listen 127.0.0.1:8088` 回退。
    pub fn mock_defaults() -> Self {
        Self {
            mock: true,
            token: "mock-token".into(),
            listen: "127.0.0.1:80".into(),
            ..Self::default()
        }
    }
}

impl ModeConfig {
    pub fn profile(&self, mode: RunMode) -> Option<&ModeProfile> {
        match mode {
            RunMode::Off => None,
            RunMode::Aes67 => Some(&self.aes67),
            RunMode::Dante => Some(&self.dante),
        }
    }
}

/// 声道分配/校准（channelmap）配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelmapConfig {
    /// 是否启用声道分配页/接口。
    pub enabled: bool,
    /// audio-stack 契约文件（active）。
    pub config_file: String,
    /// 多用户配置档目录（*.yml）。
    pub profiles_dir: String,
    /// apply 时渲染出的 CamillaDSP 配置（只读展示）。
    pub render_file: String,
    /// FIR 文件目录。
    pub fir_dir: String,
    /// tspi-mode 脚本（channelmap apply/rollback）。
    pub script: String,
    /// 兼容保留字段（已不使用）：校验/渲染统一走 `cli_bin`；CLI 缺失时返回 503，不回退内置规则。
    pub validate_bin: String,
    /// 布局预设文件（audio-stack 提供；不存在时回退内置预设）。
    pub presets_file: String,
    /// tspi-channelmap CLI（probe/validate/print/render/apply/rollback/profiles）。
    pub cli_bin: String,
    /// 输入通道检测：SDP 文件目录（可配置；不存在时回退 CamillaDSP 配置）。
    pub sdp_dir: String,
    /// 新建布局时默认选中的预设 id。
    pub default_layout: String,
    /// 默认输出设备（空 = 自动优先 MOTU / 第一个播放设备）。
    pub output_device: String,
    /// 输入内容检测运行态（tspi-upmix 检测器写；WebUI 只读）。
    pub input_state_file: String,
    /// 输入内容模式用户覆盖（WebUI 写；tspi-upmix/渲染侧读取）。
    pub input_mode_file: String,
    /// tspi-upmix CLI（可选；存在时 input-mode set 优先走 CLI，否则直接写覆盖文件）。
    pub upmix_bin: String,
}

impl Default for ChannelmapConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            config_file: "/etc/tspi-audio/channelmap.yml".into(),
            profiles_dir: "/etc/tspi-audio/profiles".into(),
            render_file: "/run/tspi/camilladsp.yml".into(),
            fir_dir: "/etc/tspi-audio/coeffs".into(),
            script: "/usr/local/bin/tspi-mode".into(),
            validate_bin: "/usr/local/bin/tspi-channelmap".into(),
            presets_file: "/etc/tspi-audio/layout-presets.yml".into(),
            cli_bin: "/usr/local/bin/tspi-channelmap".into(),
            sdp_dir: "/etc/aes67/sdp".into(),
            default_layout: "5.3.2".into(),
            output_device: String::new(),
            input_state_file: "/run/tspi/input-content.json".into(),
            input_mode_file: "/etc/tspi-audio/input-mode.json".into(),
            upmix_bin: "/usr/local/bin/tspi-upmix".into(),
        }
    }
}

/// MOTU UltraLite-mk5 / CueMix。mock 模式不使用这里的目录，也不执行发现命令。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MotuConfig {
    /// 写入前快照目录，只保留最近 20 份。
    pub snapshot_dir: String,
    /// CDC-ECM 口没有地址时幂等添加的链路本地地址。
    pub link_local_cidr: String,
    /// 状态缓存时间。页面轮询应短于或接近这个值，避免每次都重连设备。
    pub cache_ttl_ms: u64,
    pub discover_timeout_ms: u64,
    pub read_timeout_ms: u64,
    /// 全量读在静默这么久之后结束。
    pub idle_ms: u64,
    pub ws_port: u16,
}

impl Default for MotuConfig {
    fn default() -> Self {
        Self {
            snapshot_dir: "/var/lib/khaen-audio-web/motu-snapshots".into(),
            link_local_cidr: "169.254.111.200/16".into(),
            cache_ttl_ms: 12_000,
            discover_timeout_ms: 2_500,
            read_timeout_ms: 5_000,
            idle_ms: 250,
            ws_port: 1280,
        }
    }
}

/// 配置加载结果。
#[derive(Debug, Clone)]
pub struct LoadReport {
    pub path: PathBuf,
    pub created: bool,
    pub token_generated: bool,
    pub warnings: Vec<String>,
    pub mock: bool,
}

/// 默认配置路径。
pub fn default_config_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("config.toml")
    } else {
        PathBuf::from("/etc/khaen-audio-web/config.toml")
    }
}

/// 读取环境变量布尔值（1/true/yes/on）。
pub fn env_truthy(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// 生成 32 位十六进制随机 token。
pub fn generate_token() -> String {
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_err() {
        // 极端情况下退回时间戳 + 进程号，保证服务仍能启动（安全性降低并告警）。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let v = nanos ^ (pid << 64);
        return format!("{:032x}", v);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn is_placeholder_token(token: &str) -> bool {
    let t = token.trim();
    t.is_empty()
        || t.eq_ignore_ascii_case(PLACEHOLDER_TOKEN)
        || t.eq_ignore_ascii_case("changeme")
        || t == "change-me-now"
}

/// 用默认模板生成完整配置文本（token 已替换）。
pub fn default_config_text(token: &str) -> String {
    DEFAULT_CONFIG_TEMPLATE.replace(
        &format!("token = \"{PLACEHOLDER_TOKEN}\""),
        &format!("token = \"{token}\""),
    )
}

/// 把配置文件里的 token 行替换为新值，保留其余内容与注释。
pub fn rewrite_token_line(text: &str, token: &str) -> String {
    let mut out = String::with_capacity(text.len() + 40);
    let mut replaced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !replaced && (trimmed.starts_with("token ") || trimmed.starts_with("token=")) {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str(&format!("token = \"{token}\""));
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&format!("\ntoken = \"{token}\"\n"));
    }
    out
}

/// 写文件并尽量收紧权限（Unix 下 0600）。
pub fn write_private_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn apply_env_overrides(config: &mut AppConfig) {
    if let Ok(v) = std::env::var("TSPI_LISTEN") {
        if !v.trim().is_empty() {
            config.listen = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_TOKEN") {
        if !v.trim().is_empty() {
            config.token = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_AUTH_FILE") {
        if !v.trim().is_empty() {
            config.auth_file = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_CAMILLADSP_WS") {
        if !v.trim().is_empty() {
            config.camilladsp.ws_url = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_MODE_SCRIPT") {
        config.mode.switch_script = v.trim().to_string();
    }
    if let Ok(v) = std::env::var("TSPI_CHANNELMAP_FILE") {
        if !v.trim().is_empty() {
            config.channelmap.config_file = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_STATE_FILE") {
        if !v.trim().is_empty() {
            config.mode.state_file = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_INPUT_STATE_FILE") {
        if !v.trim().is_empty() {
            config.channelmap.input_state_file = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_INPUT_MODE_FILE") {
        if !v.trim().is_empty() {
            config.channelmap.input_mode_file = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("TSPI_UPMIX_BIN") {
        if !v.trim().is_empty() {
            config.channelmap.upmix_bin = v.trim().to_string();
        }
    }
    if let Some(v) = env_truthy("TSPI_ALLOW_REBOOT") {
        config.system.allow_reboot = v;
    }
    if let Some(v) = env_truthy("TSPI_ALLOW_SHUTDOWN") {
        config.system.allow_shutdown = v;
    }
    if let Some(v) = env_truthy("TSPI_MODE_ENABLED") {
        config.mode.enabled = v;
    }
    if let Some(v) = env_truthy("TSPI_RESTORE_MODE") {
        config.mode.restore_on_start = v;
    }
}

/// 加载配置：显式路径 > TSPI_CONFIG > 默认路径。
///
/// - 文件不存在且非 mock：按模板生成（含随机 token），打印提示。
/// - 文件存在但 token 为占位符且非 mock：自动生成并改写 token 行。
pub fn load(
    cli_path: Option<PathBuf>,
    mock_requested: bool,
) -> Result<(AppConfig, LoadReport), String> {
    let path = cli_path
        .or_else(|| std::env::var("TSPI_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(default_config_path);

    let mut report = LoadReport {
        path: path.clone(),
        created: false,
        token_generated: false,
        warnings: Vec::new(),
        mock: mock_requested,
    };

    let mut config = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取配置文件 {} 失败: {e}", path.display()))?;
        toml::from_str::<AppConfig>(&text)
            .map_err(|e| format!("解析配置文件 {} 失败: {e}", path.display()))?
    } else if mock_requested {
        let c = AppConfig {
            token: "mock-token".into(),
            ..AppConfig::default()
        };
        report
            .warnings
            .push("mock 模式：未找到配置文件，使用内置 mock 默认值".into());
        c
    } else {
        let token = generate_token();
        let text = default_config_text(&token);
        let c = AppConfig {
            token,
            ..AppConfig::default()
        };
        match write_private_file(&path, &text) {
            Ok(()) => {
                report.created = true;
                report.token_generated = true;
                report
                    .warnings
                    .push(format!("首次启动：已生成默认配置 {}", path.display()));
            }
            Err(e) => {
                report.token_generated = true;
                report.warnings.push(format!(
                    "无法写入 {}（{e}），本次使用内存中的随机 token，请手动创建配置",
                    path.display()
                ));
            }
        }
        c
    };

    config.mock = mock_requested;

    if path.exists() && !mock_requested && is_placeholder_token(&config.token) {
        let token = generate_token();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let rewritten = rewrite_token_line(&text, &token);
        match write_private_file(&path, &rewritten) {
            Ok(()) => {
                config.token = token;
                report.token_generated = true;
                report
                    .warnings
                    .push("检测到占位 token，已自动生成新 token 并写回配置文件".into());
            }
            Err(e) => {
                config.token = token;
                report.token_generated = true;
                report.warnings.push(format!(
                    "检测到占位 token，但写回 {} 失败（{e}）；本次使用临时 token",
                    path.display()
                ));
            }
        }
    }

    if mock_requested && is_placeholder_token(&config.token) {
        config.token = "mock-token".into();
    }

    apply_env_overrides(&mut config);
    // 根治 token 双文件不一致：服务启动即把当前 token 同步到 /etc/tspi/web-token。
    if !mock_requested && !is_placeholder_token(&config.token) {
        if let Err(e) = sync_web_token(&config.token) {
            report.warnings.push(e);
        }
    }
    Ok((config, report))
}

/// 把当前 token 同步到 OEM 约定的 token 文件（默认 /etc/tspi/web-token）。
/// 目录不存在时跳过（开发机/mock）；写失败只告警不阻断启动。
fn sync_web_token(token: &str) -> Result<(), String> {
    let path =
        std::env::var("TSPI_WEB_TOKEN_FILE").unwrap_or_else(|_| "/etc/tspi/web-token".to_string());
    let p = std::path::Path::new(&path);
    if p.parent().map(|d| !d.exists()).unwrap_or(true) {
        return Ok(());
    }
    sync_web_token_to(p, token).map_err(|e| format!("同步 {path} 失败: {e}"))
}

fn sync_web_token_to(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    write_private_file(path, &format!("{token}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_sync_writes_private_file() {
        // 不走 tempfile：%TEMP% 指向 R:\Temp 时 Rust 临时目录会 PermissionDenied，用 crate 内 target。
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("token-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("web-token");
        sync_web_token_to(&path, "abc123").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc123\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_parses_and_matches_defaults() {
        let text = DEFAULT_CONFIG_TEMPLATE;
        let parsed: AppConfig = toml::from_str(text).expect("example config must parse");
        let defaults = AppConfig::default();
        assert_eq!(parsed.listen, defaults.listen);
        assert_eq!(parsed.camilladsp.ws_url, "ws://127.0.0.1:1234");
        assert_eq!(parsed.mode.dante.input_sample_rate, 96000);
        assert_eq!(parsed.mode.aes67.input_sample_rate, 48000);
        assert!(
            parsed.mode.aes67.ptp_services.is_empty(),
            "AES67 默认方案 B1 不启动 linuxptp"
        );
        assert_eq!(parsed.mode.aes67.ptp_source, "aes67-daemon");
        assert_eq!(parsed.mode.dante.ptp_source, "statime");
        assert_eq!(parsed.mode.aes67.input_bit_depth, 24);
        assert_eq!(parsed.mode.dante.output_bit_depth, 24);
        assert_eq!(
            parsed.mode.aes67.camilla_config,
            "/etc/camilladsp/configs/aes67.yml"
        );
        assert!(parsed.mode.enabled);
        // D4：默认不自动恢复音频模式
        assert!(!parsed.mode.restore_on_start);
        assert!(!parsed.system.allow_reboot);
        assert!(parsed.channelmap.enabled);
        assert_eq!(
            parsed.channelmap.config_file,
            "/etc/tspi-audio/channelmap.yml"
        );
        assert_eq!(parsed.channelmap.default_layout, "5.3.2");
        assert_eq!(
            parsed.motu.snapshot_dir,
            "/var/lib/khaen-audio-web/motu-snapshots"
        );
        assert_eq!(parsed.motu.link_local_cidr, defaults.motu.link_local_cidr);
        assert_eq!(parsed.motu.ws_port, 1280);
    }

    #[test]
    fn generated_config_has_real_token_and_parses() {
        let text = default_config_text("abc123");
        assert!(text.contains("token = \"abc123\""));
        let parsed: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed.token, "abc123");
    }

    #[test]
    fn rewrite_token_preserves_comments() {
        let src = "# 注释\ntoken = \"CHANGE_ME\" # 行尾\nlisten = \"0.0.0.0:80\"\n";
        let out = rewrite_token_line(src, "deadbeef");
        assert!(out.contains("token = \"deadbeef\""));
        assert!(out.contains("# 注释"));
        assert!(out.contains("listen"));
    }

    #[test]
    fn rewrite_token_appends_when_missing() {
        let out = rewrite_token_line("listen = \"x\"\n", "ff");
        assert!(out.contains("token = \"ff\""));
    }

    #[test]
    fn placeholder_detection() {
        assert!(is_placeholder_token(""));
        assert!(is_placeholder_token("CHANGE_ME"));
        assert!(is_placeholder_token(" change-me-now "));
        assert!(!is_placeholder_token("0123456789abcdef"));
    }

    #[test]
    fn token_is_32_hex_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn run_mode_parsing() {
        use std::str::FromStr;
        assert_eq!(RunMode::from_str("dante").unwrap(), RunMode::Dante);
        assert_eq!(RunMode::from_str("AES67").unwrap(), RunMode::Aes67);
        assert_eq!(RunMode::from_str("off").unwrap(), RunMode::Off);
        assert!(RunMode::from_str("ptp").is_err());
    }

    #[test]
    fn profile_lookup() {
        let m = ModeConfig::default();
        assert_eq!(m.profile(RunMode::Dante).unwrap().ptp_version, "PTPv1");
        assert_eq!(m.profile(RunMode::Aes67).unwrap().ptp_version, "PTPv2");
        assert!(m.profile(RunMode::Off).is_none());
    }
}
