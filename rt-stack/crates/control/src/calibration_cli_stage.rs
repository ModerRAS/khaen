//! [NONRT] CLI stage backend —— 真实音频校准（KHAEN_CAL_STAGE=cli）。
//!
//! 角色：daemon 在 CLI 模式（env \`KHAEN_CAL_STAGE=cli\` / toml \`[calibration] stage="cli"\`）
//! 构造本 backend 注入 T60 JobManager（khaen_calibration::job::JobManager）。每个 JobStage
//! 都调用本机脚本 \`/usr/local/lib/khaen/calib-stage.sh <stage> <job_dir> <json_args>\`：
//! 真实 MOTU/UMIK 测量（measure-multi）、Farina 分析、PEQ 拟合、merge、validate、file apply。
//!
//! ## 诚实边界（证据必须照抄）
//! - 测量/分析/拟合/合并是真实音频路径（synthetic=false, real_audio=true）；
//! - **apply 是 file-backend**：只把 calibration.yml 写到
//!   \`/var/lib/khaen-rt-stack/calibration/active.yml\` 并记录 state，host 当前没有加载
//!   calibration.yml 的 PEQ/plan 路径 ⇒ 状态 = **NOT_APPLIED_TO_AUDIO**，不是 actual；
//! - retest 是真实重测，但因为 apply 未进音频路径，测的是未加滤波的硬件（见 result.json）。
//!
//! ## 设备交接
//! T60 worker 在 sweep_capture/remeasure/holdout 前调 \`SafeOutputController::
//! acquire_calibration_owner\`，stage 成功后调 \`release_calibration_owner\`。本模块的
//! \`CliSafety\` 用 \`systemctl stop/start khaen-rt-host\` 真让出/恢复 MOTU（恢复后保持
//! \`90-safe-mute.env\` 的 mute）；脚本自身也在 measure/retest 内 stop/start 兜底。
//!
//! 只在本 job 的 worker 线程（NONRT）调用；不触碰 RT 图、不直接打开 PCM。

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use khaen_calibration::job::{
    sha256_hex, ArtifactKind, CalibrationJobBackend, ExitDisposition, JobStage,
    SafeOutputController, SafetyError, StageCtx, StageFailure,
};

/// CLI 后端的 engine 名（REST /api/calibration/jobs 的 engine 字段）。
pub const CAL_PORT_ENGINE_CLI: &str = "t60-jobmanager-cli";
/// CLI 后端的 backend 组成说明。
pub const CAL_PORT_BACKEND_CLI: &str = "cli-stage-script+real-measure+file-apply";
/// CLI 后端的口径 note（REST 原样带出；页面/证据必须显示 NOT_APPLIED_TO_AUDIO）。
pub const CAL_PORT_NOTE_CLI: &str =
    "T60 JobManager 状态机真实执行；preflight/measure/analyze/fit/merge=真实 MOTU+UMIK 音频(CLI stage script)；apply=file-backend active.yml，host 尚未消费 calibration.yml ⇒ NOT_APPLIED_TO_AUDIO（不是 actual）";

/// 默认 stage 脚本路径（可被 env \`KHAEN_CAL_STAGE_SCRIPT\` 覆盖）。
pub const DEFAULT_STAGE_SCRIPT: &str = "/usr/local/lib/khaen/calib-stage.sh";
/// 默认 host unit（可被 env \`KHAEN_CAL_HOST_UNIT\` 覆盖）。
pub const DEFAULT_HOST_UNIT: &str = "khaen-rt-host";
/// 默认校准状态根（可被 env \`KHAEN_CAL_ROOT\` 覆盖）。
pub const DEFAULT_CAL_ROOT: &str = "/var/lib/khaen-rt-stack/calibration";
/// 默认 mute drop-in（可被 env \`KHAEN_CAL_SAFE_MUTE_ENV\` 覆盖）。
pub const DEFAULT_SAFE_MUTE_ENV: &str = "/etc/khaen-rt-stack/90-safe-mute.env";
/// stage 脚本单次执行上限（默认 15 分钟；env \`KHAEN_CAL_STAGE_TIMEOUT_SECS\` 覆盖）。
pub const DEFAULT_STAGE_TIMEOUT_SECS: u64 = 900;

/// CLI stage backend 配置（daemon 启动时由 env 决定；构造期不触碰设备）。
#[derive(Debug, Clone)]
pub struct CliStageConfig {
    /// 本机脚本路径。
    pub script: PathBuf,
    /// job 请求 layout（默认 4.1.2）。
    pub layout: String,
    /// job 请求采样率（默认 48000）。
    pub sample_rate: u32,
    /// 驱动电平（默认 -25 dBFS；P08 固定口径）。
    pub drive_level_dbfs: f32,
    /// 每点重复次数（默认 2；measure-multi 的 delay 归一化在 repeats>1 才正确）。
    pub repeats: u32,
    /// 通道表（默认 0,1,2,3,4,5,7；与 calib-stage.env 同口径）。
    pub channels: String,
    /// 通道名（默认 FL,FR,SL,SR,HFL,HFR,SUB1）。
    pub names: String,
    /// 单个 stage 超时秒。
    pub timeout_secs: u64,
}

impl Default for CliStageConfig {
    fn default() -> Self {
        CliStageConfig {
            script: PathBuf::from(DEFAULT_STAGE_SCRIPT),
            layout: "4.1.2".to_string(),
            sample_rate: 48_000,
            drive_level_dbfs: -25.0,
            repeats: 2,
            channels: "0,1,2,3,4,5,7".to_string(),
            names: "FL,FR,SL,SR,HFL,HFR,SUB1".to_string(),
            timeout_secs: DEFAULT_STAGE_TIMEOUT_SECS,
        }
    }
}

impl CliStageConfig {
    /// 从环境变量构造（缺省用默认；未知值不静默改变安全口径）。
    pub fn from_env() -> Self {
        let mut config = CliStageConfig::default();
        if let Ok(value) = std::env::var("KHAEN_CAL_STAGE_SCRIPT") {
            if !value.trim().is_empty() {
                config.script = PathBuf::from(value);
            }
        }
        if let Ok(value) = std::env::var("KHAEN_CAL_CHANNELS") {
            if !value.trim().is_empty() {
                config.channels = value;
            }
        }
        if let Ok(value) = std::env::var("KHAEN_CAL_NAMES") {
            if !value.trim().is_empty() {
                config.names = value;
            }
        }
        if let Ok(value) = std::env::var("KHAEN_CAL_LEVEL_DBFS") {
            if let Ok(level) = value.trim().parse::<f32>() {
                if (-60.0..=0.0).contains(&level) {
                    config.drive_level_dbfs = level;
                }
            }
        }
        if let Ok(value) = std::env::var("KHAEN_CAL_REPEATS") {
            if let Ok(repeats) = value.trim().parse::<u32>() {
                config.repeats = repeats.clamp(1, 8);
            }
        }
        if let Ok(value) = std::env::var("KHAEN_CAL_STAGE_TIMEOUT_SECS") {
            if let Ok(secs) = value.trim().parse::<u64>() {
                config.timeout_secs = secs.max(30);
            }
        }
        config
    }
}

/// T60 JobStage -> 脚本 stage 名（脚本只接受这 7 个规范名；别名由脚本内部兼容）。
pub const fn script_stage(stage: JobStage) -> &'static str {
    match stage {
        JobStage::Preflight => "preflight",
        JobStage::SweepCapture => "measure",
        JobStage::QualityCheck => "analyze",
        JobStage::FitP => "fit",
        JobStage::SolveG => "merge",
        JobStage::Compile => "merge",
        JobStage::Prepared => "preflight",
        JobStage::Apply => "apply",
        JobStage::Remeasure => "retest",
        JobStage::IndependentHoldout => "retest",
    }
}

/// 转义 JSON 字符串（args 只用 ASCII/安全值；仍按规范转义）。
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 从 JSON 文本提取字符串字段（本模块只读自己脚本产出的固定字段；非通用解析器）。
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut cursor = 0usize;
    while let Some(pos) = text[cursor..].find(&needle) {
        let start = cursor + pos + needle.len();
        let rest = text[start..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            cursor = start;
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with('"') {
            cursor = start;
            continue;
        }
        let mut out = String::new();
        let mut chars = rest[1..].chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some(other) => out.push(other),
                    None => return None,
                },
                '"' => return Some(out),
                c => out.push(c),
            }
        }
        return None;
    }
    None
}

/// JSON 布尔字段。
fn json_bool_field(text: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{key}\"");
    let mut cursor = 0usize;
    while let Some(pos) = text[cursor..].find(&needle) {
        let start = cursor + pos + needle.len();
        let rest = text[start..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            cursor = start;
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with("true") {
            return Some(true);
        }
        if rest.starts_with("false") {
            return Some(false);
        }
        cursor = start;
    }
    None
}

/// CLI stage backend（每个 job 一个实例；只做 argv 调脚本 + artifact 登记）。
pub struct CliStageBackend {
    config: CliStageConfig,
    /// 已登记 artifact 相对路径（避免重复读大文件/触发 immutable）。
    recorded: BTreeSet<String>,
}

impl CliStageBackend {
    /// 构造（不触碰设备/脚本）。
    pub fn new(config: CliStageConfig) -> Self {
        CliStageBackend {
            config,
            recorded: BTreeSet::new(),
        }
    }

    /// 传给脚本的 json_args（P08 授权/电平/通道口径原样传递）。
    fn args_json(&self, audible: bool) -> String {
        format!(
            "{{\"layout\":\"{}\",\"sample_rate\":{},\"drive_level_dbfs\":{},\"repeats\":{},\"channels\":\"{}\",\"names\":\"{}\",\"audible_sweep_authorized\":{}}}",
            json_escape(&self.config.layout),
            self.config.sample_rate,
            self.config.drive_level_dbfs,
            self.config.repeats,
            json_escape(&self.config.channels),
            json_escape(&self.config.names),
            audible
        )
    }

    /// 调脚本并等待（有界超时）；stdout/stderr 落 \`<stage>.log\`。
    fn run_script(
        &self,
        stage: &str,
        job_dir: &Path,
        args_json: &str,
    ) -> Result<(String, std::process::ExitStatus), StageFailure> {
        let log_path = job_dir.join(format!("{stage}.log"));
        let log = fs::File::create(&log_path).map_err(|error| {
            StageFailure::failed(
                "E_CLI_STAGE_IO",
                format!("create {}: {error}", log_path.display()),
            )
        })?;
        let log_err = log.try_clone().map_err(|error| {
            StageFailure::failed("E_CLI_STAGE_IO", format!("clone log handle: {error}"))
        })?;
        let mut child = Command::new(&self.config.script)
            .arg(stage)
            .arg(job_dir)
            .arg(args_json)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|error| {
                StageFailure::failed(
                    "E_CLI_STAGE_SPAWN",
                    format!(
                        "spawn {} (cli stage backend requires Linux daemon): {error}",
                        self.config.script.display()
                    ),
                )
            })?;
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_secs);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut text = String::new();
                    if let Ok(mut file) = fs::File::open(&log_path) {
                        let _ = file.read_to_string(&mut text);
                    }
                    let tail: String = text
                        .chars()
                        .rev()
                        .take(4000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    return Ok((tail, status));
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(StageFailure::failed(
                            "E_CLI_STAGE_TIMEOUT",
                            format!(
                                "stage {stage} exceeded {}s ({}); killed",
                                self.config.timeout_secs,
                                self.config.script.display()
                            ),
                        ));
                    }
                    thread::sleep(Duration::from_millis(200));
                }
                Err(error) => {
                    let _ = child.kill();
                    return Err(StageFailure::failed(
                        "E_CLI_STAGE_WAIT",
                        format!("wait stage {stage}: {error}"),
                    ));
                }
            }
        }
    }

    /// 登记本 job 目录里产出/刷新的关键 artifact（关键文件子集，避免 artifact 上限）。
    fn register_artifacts(&mut self, ctx: &StageCtx<'_>, job_dir: &Path, stage: &str) {
        let mut stack = vec![PathBuf::from(job_dir)];
        let mut files = Vec::new();
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    files.push(path);
                }
            }
        }
        files.sort();
        for path in files {
            let Ok(relative) = path.strip_prefix(job_dir) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative == "artifact-manifest.json" || self.recorded.contains(&relative) {
                continue;
            }
            let Some(kind) = artifact_kind_for(&relative) else {
                continue;
            };
            let version = format!("cli-{stage}-v1");
            match ctx.record_artifact_file(kind, &relative, &version) {
                Ok(_) => {
                    self.recorded.insert(relative);
                }
                Err(error) => {
                    // 已登记过同内容/不可变冲突不致命；其他错误也只在 stderr 提示，不伪造成功。
                    eprintln!(
                        "cli-stage: artifact {relative} not recorded: {}",
                        error.message
                    );
                    self.recorded.insert(relative);
                }
            }
        }
    }
}

/// 关键 artifact 过滤（只登记证据核心文件）。
fn artifact_kind_for(relative: &str) -> Option<ArtifactKind> {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    if name.starts_with("capture") && name.ends_with(".raw") {
        return Some(ArtifactKind::RawCapture);
    }
    if name.ends_with(".analysis.json") {
        return Some(ArtifactKind::Other("analysis".to_string()));
    }
    if name == "calibration.yml" {
        return Some(ArtifactKind::CalibrationDraft);
    }
    if name.ends_with(".result.json") {
        return Some(ArtifactKind::Other("stage_result".to_string()));
    }
    if name.ends_with(".log") {
        return Some(ArtifactKind::Other("stage_log".to_string()));
    }
    None
}

impl CalibrationJobBackend for CliStageBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        let name = script_stage(stage);
        // P08：真实测量必须显式授权（REST start 的 audible_sweep_authorized）。
        if stage.needs_calibration_device() && !ctx.request().audible_sweep_authorized {
            return Err(StageFailure::failed(
                "E_LEVEL_NOT_AUTHORIZED",
                format!(
                    "stage {} requires explicit audible_sweep_authorized=true (P08); refusing to make sound",
                    stage.as_str()
                ),
            ));
        }
        let job_dir = ctx.artifacts_dir();
        if let Err(error) = fs::create_dir_all(&job_dir) {
            return Err(StageFailure::failed(
                "E_CLI_STAGE_IO",
                format!("create job dir {}: {error}", job_dir.display()),
            ));
        }
        let args = self.args_json(ctx.request().audible_sweep_authorized);
        ctx.report_progress(0.05);
        eprintln!(
            "cli-stage: {} ({}) -> {} {}",
            stage.as_str(),
            name,
            self.config.script.display(),
            args
        );
        let (tail, status) = self.run_script(name, &job_dir, &args)?;
        if !status.success() {
            return Err(StageFailure::failed(
                "E_CLI_STAGE",
                format!(
                    "calib-stage.sh {name} failed (exit {:?}); log tail: {}",
                    status.code(),
                    tail
                ),
            ));
        }
        let result_path = job_dir.join(format!("{name}.result.json"));
        let result_text = fs::read_to_string(&result_path).map_err(|error| {
            StageFailure::failed(
                "E_CLI_STAGE_RESULT",
                format!(
                    "stage {name} exited 0 but {} unreadable ({error}); refusing to fake success",
                    result_path.display()
                ),
            )
        })?;
        if json_bool_field(&result_text, "ok") != Some(true) {
            return Err(StageFailure::failed(
                "E_CLI_STAGE_RESULT",
                format!("stage {name} result.json does not declare ok=true"),
            ));
        }
        // apply 必须带 applied_version（K14：没有 ack 不得当成功）。
        if name == "apply" {
            let Some(applied) = json_string_field(&result_text, "applied_version") else {
                return Err(StageFailure::failed(
                    "E_CLI_STAGE_RESULT",
                    "apply result.json missing applied_version; refusing to fake ack",
                ));
            };
            ctx.set_applied_version(applied);
            if let Some(last_good) = json_string_field(&result_text, "last_good_version") {
                if last_good != "none" && !last_good.trim().is_empty() {
                    ctx.set_last_good_version(last_good);
                }
            }
        }
        self.register_artifacts(ctx, &job_dir, name);
        ctx.report_progress(1.0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SafeOutputController：真 systemctl stop/start（保持 mute）
// ---------------------------------------------------------------------------

/// CLI 模式的设备交接/安全恢复（systemctl stop/start host + file-backend last-good）。
pub struct CliSafety {
    host_unit: String,
    calib_root: PathBuf,
    safe_mute_env: PathBuf,
    active_yml: PathBuf,
    last_good_yml: PathBuf,
    state_json: PathBuf,
}

impl CliSafety {
    /// 从 env 构造（不执行任何命令）。
    pub fn from_env() -> Self {
        let calib_root = std::env::var("KHAEN_CAL_ROOT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CAL_ROOT));
        let host_unit = std::env::var("KHAEN_CAL_HOST_UNIT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_HOST_UNIT.to_string());
        let safe_mute_env = std::env::var("KHAEN_CAL_SAFE_MUTE_ENV")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SAFE_MUTE_ENV));
        CliSafety {
            host_unit,
            active_yml: calib_root.join("active.yml"),
            last_good_yml: calib_root.join("last-good.yml"),
            state_json: calib_root.join("calib-state.json"),
            calib_root,
            safe_mute_env,
        }
    }

    /// 测试/证据用构造。
    pub fn with_paths(
        host_unit: impl Into<String>,
        calib_root: impl Into<PathBuf>,
        safe_mute_env: impl Into<PathBuf>,
    ) -> Self {
        let calib_root = calib_root.into();
        CliSafety {
            host_unit: host_unit.into(),
            active_yml: calib_root.join("active.yml"),
            last_good_yml: calib_root.join("last-good.yml"),
            state_json: calib_root.join("calib-state.json"),
            calib_root,
            safe_mute_env: safe_mute_env.into(),
        }
    }

    fn systemctl(&self, verb: &str) -> Result<(), SafetyError> {
        let output = Command::new("systemctl")
            .arg(verb)
            .arg(&self.host_unit)
            .output()
            .map_err(|error| {
                SafetyError::new("SYSTEMCTL_SPAWN", format!("systemctl {verb}: {error}"))
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(SafetyError::new(
                "SYSTEMCTL_FAILED",
                format!(
                    "systemctl {verb} {} exited {:?}",
                    self.host_unit,
                    output.status.code()
                ),
            ))
        }
    }

    fn is_active(&self) -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", self.host_unit.as_str()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn wait_active(&self, want_active: bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_active() == want_active {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    fn mute_configured(&self) -> bool {
        fs::read_to_string(&self.safe_mute_env)
            .map(|text| text.contains("KHAEN_HOST_SAFE_MODE=mute"))
            .unwrap_or(false)
    }

    fn read_state_field(&self, key: &str) -> Option<String> {
        let text = fs::read_to_string(&self.state_json).ok()?;
        json_string_field(&text, key)
    }
}

impl SafeOutputController for CliSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        if self.is_active() {
            self.systemctl("stop")?;
        }
        if !self.wait_active(false, Duration::from_secs(10)) {
            return Err(SafetyError::new(
                "DEVICE_BUSY",
                format!("{} still active after stop (10s)", self.host_unit),
            ));
        }
        // 让 USB/ALSA 句柄真正释放
        thread::sleep(Duration::from_millis(800));
        Ok(())
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        // 两种 disposition 都恢复 host：mute drop-in 保证不出声；host 是唯一 PCM owner。
        if !self.is_active() {
            self.systemctl("start")?;
        }
        if !self.wait_active(true, Duration::from_secs(20)) {
            return Err(SafetyError::new(
                "DEVICE_RELEASE",
                format!("{} did not become active after start (20s)", self.host_unit),
            ));
        }
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        let bytes = fs::read(&self.last_good_yml).map_err(|error| {
            SafetyError::new(
                "LAST_GOOD_MISSING",
                format!("read {}: {error}", self.last_good_yml.display()),
            )
        })?;
        fs::create_dir_all(&self.calib_root).map_err(|error| {
            SafetyError::new(
                "LAST_GOOD_IO",
                format!("create {}: {error}", self.calib_root.display()),
            )
        })?;
        fs::write(&self.active_yml, &bytes).map_err(|error| {
            SafetyError::new(
                "LAST_GOOD_IO",
                format!("write {}: {error}", self.active_yml.display()),
            )
        })?;
        Ok(format!("file-last-good-{}", &sha256_hex(&bytes)[..16]))
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        if self.mute_configured() {
            if !self.is_active() {
                self.systemctl("start")?;
                if !self.wait_active(true, Duration::from_secs(20)) {
                    return Err(SafetyError::new(
                        "SAFE_MUTE",
                        format!("{} did not become active (20s)", self.host_unit),
                    ));
                }
            }
            return Ok(());
        }
        // mute drop-in 缺失：最保守做法 = 停 host（零输出），绝不放声。
        if self.is_active() {
            self.systemctl("stop")?;
        }
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        self.read_state_field("last_good_version")
    }
}

impl std::fmt::Debug for CliSafety {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CliSafety")
            .field("host_unit", &self.host_unit)
            .field("calib_root", &self.calib_root)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_mapping_covers_all_t60_stages() {
        for stage in JobStage::ALL {
            let name = script_stage(stage);
            assert!(
                [
                    "preflight",
                    "measure",
                    "analyze",
                    "fit",
                    "merge",
                    "apply",
                    "retest"
                ]
                .contains(&name),
                "stage {} mapped to unexpected script stage {name}",
                stage.as_str()
            );
        }
        assert_eq!(script_stage(JobStage::SweepCapture), "measure");
        assert_eq!(script_stage(JobStage::QualityCheck), "analyze");
        assert_eq!(script_stage(JobStage::FitP), "fit");
        assert_eq!(script_stage(JobStage::SolveG), "merge");
        assert_eq!(script_stage(JobStage::Apply), "apply");
        assert_eq!(script_stage(JobStage::Remeasure), "retest");
    }

    #[test]
    fn args_json_is_well_formed_and_escaped() {
        let backend = CliStageBackend::new(CliStageConfig::default());
        let json = backend.args_json(true);
        assert!(json.contains("\"layout\":\"4.1.2\""));
        assert!(json.contains("\"audible_sweep_authorized\":true"));
        assert!(json.contains("\"channels\":\"0,1,2,3,4,5,7\""));
        assert!(json.contains("\"drive_level_dbfs\":-25"));
    }

    #[test]
    fn json_field_parsing_handles_script_result_shape() {
        let text = r#"{"ok": true, "applied_version": "file-active-abc123", "last_good_version": "none", "note": "a\"b"}"#;
        assert_eq!(json_bool_field(text, "ok"), Some(true));
        assert_eq!(
            json_string_field(text, "applied_version").as_deref(),
            Some("file-active-abc123")
        );
        assert_eq!(
            json_string_field(text, "last_good_version").as_deref(),
            Some("none")
        );
        assert_eq!(json_string_field(text, "missing"), None);
    }

    #[test]
    fn cli_safety_construction_does_not_run_commands() {
        let safety =
            CliSafety::with_paths("khaen-rt-host", "/tmp/does-not-exist-cal", "/tmp/no.env");
        assert_eq!(safety.last_good_version(), None);
        assert!(!safety.mute_configured());
    }
}
