//! CamillaDSP 控制：websocket API 客户端 + 配置管理 + mock。
//!
//! 协议依据 HEnquist/camilladsp 的 \`websocket.md\`（本地核对过 4.1.3 源码）：
//! - 命令：字符串 \`"GetState"\` 或对象 \`{"SetVolume": -12.0}\`；
//! - 回复：\`{"GetState":{"result":"Ok","value":"Running"}}\`，
//!   错误时 result 是错误名字符串或 \`{"ConfigValidationError":"..."}\`；
//! - 4.x 没有独立的 xrun 计数命令，xrun 只能从 journalctl 日志 + StopReason 推断（本服务在总览里做这个推断）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::config::CamillaConfig;
use crate::error::{AppError, AppResult};
use crate::service::ServiceManager;
use crate::util;

/// CamillaDSP 实时状态。
#[derive(Debug, Clone, Serialize, Default)]
pub struct CamillaLive {
    pub reachable: bool,
    pub version: Option<String>,
    pub state: Option<String>,
    pub stop_reason: Option<String>,
    pub capture_rate: Option<u64>,
    pub processing_load: Option<f64>,
    pub buffer_level: Option<i64>,
    pub rate_adjust: Option<f64>,
    pub clipped_samples: Option<u64>,
    pub volume_db: Option<f64>,
    pub mute: Option<bool>,
    pub config_path: Option<String>,
    pub config_title: Option<String>,
    pub error: Option<String>,
}

impl CamillaLive {
    pub fn is_running(&self) -> bool {
        self.state.as_deref() == Some("Running")
    }
    pub fn stop_reason_is_error(&self) -> bool {
        matches!(
            self.stop_reason.as_deref(),
            Some("CaptureError" | "PlaybackError" | "CaptureFormatChange" | "PlaybackFormatChange")
        )
    }
}

/// xrun 线索（来自 journalctl 日志）。
#[derive(Debug, Clone, Serialize, Default)]
pub struct XrunInfo {
    pub recent_count: u64,
    pub last_line: Option<String>,
    pub hint: String,
}

/// 在日志里统计 xrun/underrun/overrun 行。
pub fn scan_xrun(logs: &str) -> XrunInfo {
    let mut count = 0u64;
    let mut last = None;
    for line in logs.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("xrun") || lower.contains("underrun") || lower.contains("overrun") {
            count += 1;
            last = Some(line.trim().chars().take(300).collect::<String>());
        }
    }
    let hint = if count == 0 {
        "最近日志中未发现 xrun/underrun/overrun".to_string()
    } else {
        format!("最近日志中发现 {count} 条 xrun 线索；若持续增长，尝试增大 chunksize/target_level")
    };
    XrunInfo {
        recent_count: count,
        last_line: last,
        hint,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified_unix: Option<u64>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApplyOutcome {
    /// websocket | systemd_restart
    pub applied_via: String,
    pub config_name: Option<String>,
    pub config_path: String,
    pub persisted_to: Vec<String>,
    pub backup: Option<String>,
    pub message: Option<String>,
}

#[async_trait]
pub trait CamillaBackend: Send + Sync {
    fn ws_url(&self) -> &str;
    fn service(&self) -> &str;
    fn config_file(&self) -> &str;
    fn config_dir(&self) -> &str;
    fn mock(&self) -> bool;
    async fn live(&self) -> CamillaLive;
    async fn list_configs(&self) -> Vec<ConfigEntry>;
    async fn read_named_config(&self, name: &str) -> AppResult<(String, String)>;
    async fn current_config_yaml(&self) -> AppResult<String>;
    async fn apply_named_config(&self, name: &str) -> AppResult<ApplyOutcome>;
    async fn apply_config_path(&self, path: &str) -> AppResult<ApplyOutcome>;
    /// 用给定内容覆盖 primary_path（并同步 config_file），然后 WS 应用 / 失败则 systemd 重启。
    async fn apply_config_content(
        &self,
        content: &str,
        primary_path: &str,
    ) -> AppResult<ApplyOutcome>;
    async fn set_volume(&self, db: f64) -> AppResult<f64>;
}

const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;

/// 校验 CamillaDSP YAML 配置（语法 + 必须是映射 + 可转 JSON）。
pub fn validate_camilla_yaml(content: &str) -> AppResult<Value> {
    if content.len() > MAX_CONFIG_BYTES {
        return Err(AppError::bad_request("配置文件过大（>2 MiB）"));
    }
    if content.trim().is_empty() {
        return Err(AppError::bad_request("配置文件为空"));
    }
    let yaml: serde_yaml::Value = serde_yaml::from_str(content)
        .map_err(|e| AppError::bad_request(format!("YAML 语法错误: {e}")))?;
    if !yaml.is_mapping() {
        return Err(AppError::bad_request(
            "CamillaDSP 配置必须是 YAML 映射（mapping）",
        ));
    }
    let json = serde_json::to_value(&yaml).map_err(|e| {
        AppError::bad_request(format!("配置无法转换为 JSON（键必须是字符串等）: {e}"))
    })?;
    Ok(json)
}

/// 从完整配置 YAML 中取 devices 下的关键字段。
pub fn extract_devices_fields(content: &str) -> Option<DeviceFields> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(content).ok()?;
    let devices = yaml.get("devices")?;
    let f = |k: &str| devices.get(k).cloned();
    Some(DeviceFields {
        sample_rate: f("samplerate").and_then(|v| v.as_u64()).map(|v| v as u32),
        chunksize: f("chunksize").and_then(|v| v.as_u64()).map(|v| v as u32),
        target_level: f("target_level").and_then(|v| v.as_u64()).map(|v| v as u32),
        capture_channels: devices
            .get("capture")
            .and_then(|c| c.get("channels"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        playback_channels: devices
            .get("playback")
            .and_then(|c| c.get("channels"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        capture_device: devices
            .get("capture")
            .and_then(|c| c.get("device"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        playback_device: devices
            .get("playback")
            .and_then(|c| c.get("device"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceFields {
    pub sample_rate: Option<u32>,
    pub chunksize: Option<u32>,
    pub target_level: Option<u32>,
    pub capture_channels: Option<u32>,
    pub playback_channels: Option<u32>,
    pub capture_device: Option<String>,
    pub playback_device: Option<String>,
}

/// 修改 YAML 映射中 devices 下的字段（返回新的 YAML 文本）。
pub fn patch_devices_fields(
    content: &str,
    sample_rate: Option<u32>,
    chunksize: Option<u32>,
    target_level: Option<u32>,
) -> AppResult<String> {
    let mut value: serde_yaml::Value = serde_yaml::from_str(content)
        .map_err(|e| AppError::bad_request(format!("YAML 语法错误: {e}")))?;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| AppError::bad_request("配置必须是 YAML 映射"))?;
    let devices_key = serde_yaml::Value::String("devices".into());
    if !map.contains_key(&devices_key) {
        return Err(AppError::bad_request(
            "配置缺少 devices 段，无法修改格式参数",
        ));
    }
    let devices = map
        .get_mut(&devices_key)
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| AppError::bad_request("devices 段必须是映射"))?;
    let mut set = |key: &str, v: u32| {
        devices.insert(
            serde_yaml::Value::String(key.into()),
            serde_yaml::Value::Number(serde_yaml::Number::from(v)),
        );
    };
    if let Some(v) = sample_rate {
        set("samplerate", v);
    }
    if let Some(v) = chunksize {
        set("chunksize", v);
    }
    if let Some(v) = target_level {
        set("target_level", v);
    }
    let text = serde_yaml::to_string(&value)
        .map_err(|e| AppError::internal(format!("序列化 YAML 失败: {e}")))?;
    // 二次校验
    validate_camilla_yaml(&text)?;
    Ok(text)
}

fn command_name(cmd: &Value) -> String {
    match cmd {
        Value::String(s) => s.clone(),
        Value::Object(map) => map.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    }
}

fn result_error_text(result: &Value) -> String {
    match result {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .iter()
            .next()
            .map(|(k, v)| format!("{k}: {v}"))
            .unwrap_or_else(|| "未知错误".to_string()),
        other => other.to_string(),
    }
}

/// 从回复对象中取 value；result 非 Ok 时返回错误文本。
pub fn reply_value(reply: &Value, command: &str) -> Result<Value, String> {
    let obj = reply
        .get(command)
        .ok_or_else(|| format!("回复中缺少命令 {command}"))?;
    let result = obj.get("result").cloned().unwrap_or(Value::Null);
    if result == Value::String("Ok".into()) {
        Ok(obj.get("value").cloned().unwrap_or(Value::Null))
    } else {
        Err(result_error_text(&result))
    }
}

pub struct RealCamilla {
    cfg: CamillaConfig,
    services: Arc<dyn ServiceManager>,
    timeout: Duration,
}

impl RealCamilla {
    pub fn new(cfg: CamillaConfig, services: Arc<dyn ServiceManager>) -> Self {
        let timeout = Duration::from_millis(cfg.ws_timeout_ms.clamp(200, 30_000));
        Self {
            cfg,
            services,
            timeout,
        }
    }

    async fn ws_commands(&self, commands: &[Value]) -> Result<Vec<(String, Value)>, String> {
        let (ws, _resp) = connect_async(self.cfg.ws_url.as_str())
            .await
            .map_err(|e| format!("连接 CamillaDSP websocket({}) 失败: {e}", self.cfg.ws_url))?;
        let (mut sink, mut stream) = ws.split();
        let mut out = Vec::with_capacity(commands.len());
        for cmd in commands {
            let name = command_name(cmd);
            let text = serde_json::to_string(cmd).map_err(|e| format!("命令序列化失败: {e}"))?;
            sink.send(Message::text(text))
                .await
                .map_err(|e| format!("发送 {name} 失败: {e}"))?;
            let reply = loop {
                match tokio::time::timeout(self.timeout, stream.next()).await {
                    Ok(Some(Ok(Message::Text(t)))) => break t.as_str().to_string(),
                    Ok(Some(Ok(Message::Binary(b)))) => {
                        break String::from_utf8_lossy(&b).to_string()
                    }
                    Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                    Ok(Some(Ok(Message::Close(_)))) => {
                        return Err("CamillaDSP 关闭了连接".to_string())
                    }
                    Ok(Some(Ok(_))) => continue,
                    Ok(Some(Err(e))) => return Err(format!("websocket 读取失败: {e}")),
                    Ok(None) => return Err("CamillaDSP websocket 连接结束".to_string()),
                    Err(_) => return Err(format!("等待 {name} 回复超时")),
                }
            };
            let parsed: Value = serde_json::from_str(&reply).map_err(|e| {
                format!(
                    "解析 {name} 回复失败: {e}（原文: {}）",
                    reply.chars().take(200).collect::<String>()
                )
            })?;
            out.push((name, parsed));
        }
        let _ = sink.send(Message::Close(None)).await;
        Ok(out)
    }

    async fn set_config_json(&self, json_string: &str) -> Result<(), String> {
        let replies = self
            .ws_commands(&[json!({ "SetConfigJson": json_string })])
            .await?;
        for (name, reply) in replies {
            reply_value(&reply, &name)?;
        }
        Ok(())
    }

    async fn persist_and_apply(
        &self,
        content: &str,
        primary_path: &str,
        config_name: Option<&str>,
    ) -> AppResult<ApplyOutcome> {
        let json_value = validate_camilla_yaml(content)?;
        let json_string = serde_json::to_string(&json_value)?;
        let primary = std::path::PathBuf::from(primary_path);
        let mut persisted_to = Vec::new();
        let mut backups: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
        let mut backup_path: Option<String> = None;

        // 备份 + 写入 primary，再同步 config_file（两者可能不同）
        let mut targets = vec![primary.clone()];
        let config_file = std::path::PathBuf::from(&self.cfg.config_file);
        if primary != config_file {
            targets.push(config_file.clone());
        }
        for target in &targets {
            if let Ok(Some(b)) = util::backup_file(target).await {
                if backup_path.is_none() {
                    backup_path = Some(b.display().to_string());
                }
                backups.push((target.clone(), b));
            }
            util::write_atomic(target, content)
                .await
                .map_err(|e| AppError::internal(format!("写入 {} 失败: {e}", target.display())))?;
            util::prune_backups(target, 5).await;
            persisted_to.push(target.display().to_string());
        }

        // 优先 websocket 应用
        let mut outcome = ApplyOutcome {
            applied_via: "websocket".into(),
            config_name: config_name.map(str::to_string),
            config_path: primary.display().to_string(),
            persisted_to,
            backup: backup_path,
            message: None,
        };
        match self.set_config_json(&json_string).await {
            Ok(()) => Ok(outcome),
            Err(ws_err) => {
                // 失败回退：systemd 重启
                match self.services.restart(&self.cfg.service).await {
                    Ok(()) => {
                        outcome.applied_via = "systemd_restart".into();
                        outcome.message = Some(format!(
                            "websocket 应用失败（{ws_err}），已通过 systemd 重启 CamillaDSP"
                        ));
                        Ok(outcome)
                    }
                    Err(restart_err) => {
                        // 回滚已写文件，尽量恢复原状
                        let mut rollback_errors = Vec::new();
                        for (target, backup) in backups {
                            if let Err(e) = tokio::fs::copy(&backup, &target).await {
                                rollback_errors.push(format!("{}: {e}", target.display()));
                            }
                        }
                        let rollback_note = if rollback_errors.is_empty() {
                            "已从备份恢复配置文件".to_string()
                        } else {
                            format!("回滚部分失败: {}", rollback_errors.join("; "))
                        };
                        let _ = self.services.restart(&self.cfg.service).await;
                        Err(AppError::unavailable(format!(
                            "应用配置失败：websocket 错误（{ws_err}），systemd 重启失败（{restart_err}）；{rollback_note}"
                        )))
                    }
                }
            }
        }
    }
}

#[async_trait]
impl CamillaBackend for RealCamilla {
    fn ws_url(&self) -> &str {
        &self.cfg.ws_url
    }
    fn service(&self) -> &str {
        &self.cfg.service
    }
    fn config_file(&self) -> &str {
        &self.cfg.config_file
    }
    fn config_dir(&self) -> &str {
        &self.cfg.config_dir
    }
    fn mock(&self) -> bool {
        false
    }

    async fn live(&self) -> CamillaLive {
        let mut live = CamillaLive::default();
        let commands: Vec<Value> = [
            "GetVersion",
            "GetState",
            "GetStopReason",
            "GetConfigFilePath",
            "GetConfigTitle",
            "GetVolume",
            "GetMute",
            "GetCaptureRate",
            "GetClippedSamples",
            "GetProcessingLoad",
            "GetBufferLevel",
            "GetRateAdjust",
        ]
        .iter()
        .map(|c| Value::String((*c).to_string()))
        .collect();
        match self.ws_commands(&commands).await {
            Ok(replies) => {
                live.reachable = true;
                for (name, reply) in replies {
                    let Ok(v) = reply_value(&reply, &name) else {
                        continue;
                    };
                    match name.as_str() {
                        "GetVersion" => live.version = v.as_str().map(str::to_string),
                        "GetState" => live.state = v.as_str().map(str::to_string),
                        "GetStopReason" => live.stop_reason = v.as_str().map(str::to_string),
                        "GetConfigFilePath" => live.config_path = v.as_str().map(str::to_string),
                        "GetConfigTitle" => live.config_title = v.as_str().map(str::to_string),
                        "GetVolume" => live.volume_db = v.as_f64(),
                        "GetMute" => live.mute = v.as_bool(),
                        "GetCaptureRate" => live.capture_rate = v.as_u64(),
                        "GetClippedSamples" => live.clipped_samples = v.as_u64(),
                        "GetProcessingLoad" => live.processing_load = v.as_f64(),
                        "GetBufferLevel" => live.buffer_level = v.as_i64(),
                        "GetRateAdjust" => live.rate_adjust = v.as_f64(),
                        _ => {}
                    }
                }
            }
            Err(e) => live.error = Some(e),
        }
        live
    }

    async fn list_configs(&self) -> Vec<ConfigEntry> {
        let dir = std::path::Path::new(&self.cfg.config_dir);
        let mut out = Vec::new();
        let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
            return out;
        };
        let current_name = std::path::Path::new(&self.cfg.config_file)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if !(lower.ends_with(".yml") || lower.ends_with(".yaml")) {
                continue;
            }
            let meta = entry.metadata().await.ok();
            out.push(ConfigEntry {
                current: current_name.as_deref() == Some(name.as_str()),
                name,
                path: entry.path().display().to_string(),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified_unix: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    async fn read_named_config(&self, name: &str) -> AppResult<(String, String)> {
        util::validate_yaml_name(name).map_err(AppError::bad_request)?;
        let path = std::path::Path::new(&self.cfg.config_dir).join(name);
        let (text, truncated) = util::read_text_limited(&path, MAX_CONFIG_BYTES)
            .await
            .map_err(|e| AppError::not_found(format!("读取 {} 失败: {e}", path.display())))?;
        if truncated {
            return Err(AppError::bad_request("配置文件过大（>2 MiB）"));
        }
        Ok((path.display().to_string(), text))
    }

    async fn current_config_yaml(&self) -> AppResult<String> {
        match self.ws_commands(&[Value::String("GetConfig".into())]).await {
            Ok(replies) => {
                for (name, reply) in replies {
                    match reply_value(&reply, &name) {
                        Ok(v) => {
                            if let Some(s) = v.as_str() {
                                return Ok(s.to_string());
                            }
                        }
                        Err(e) => {
                            return Err(AppError::unavailable(format!(
                                "CamillaDSP 返回 GetConfig 错误: {e}"
                            )))
                        }
                    }
                }
                Err(AppError::unavailable("CamillaDSP 未返回配置"))
            }
            Err(_) => {
                // websocket 不可用时退回读配置文件
                let (_text, _t) = util::read_text_limited(
                    std::path::Path::new(&self.cfg.config_file),
                    MAX_CONFIG_BYTES,
                )
                .await
                .map_err(|e| {
                    AppError::unavailable(format!("websocket 与配置文件均不可用: {}", e))
                })?;
                Ok(_text)
            }
        }
    }

    async fn apply_named_config(&self, name: &str) -> AppResult<ApplyOutcome> {
        let (_path, content) = self.read_named_config(name).await?;
        self.persist_and_apply(&content, &self.cfg.config_file, Some(name))
            .await
    }

    async fn apply_config_path(&self, path: &str) -> AppResult<ApplyOutcome> {
        if path.trim().is_empty() {
            return Err(AppError::bad_request("配置路径为空"));
        }
        let (content, truncated) =
            util::read_text_limited(std::path::Path::new(path), MAX_CONFIG_BYTES)
                .await
                .map_err(|e| AppError::not_found(format!("读取 {path} 失败: {e}")))?;
        if truncated {
            return Err(AppError::bad_request("配置文件过大（>2 MiB）"));
        }
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        self.persist_and_apply(&content, path, name.as_deref())
            .await
    }

    async fn apply_config_content(
        &self,
        content: &str,
        primary_path: &str,
    ) -> AppResult<ApplyOutcome> {
        self.persist_and_apply(content, primary_path, None).await
    }

    async fn set_volume(&self, db: f64) -> AppResult<f64> {
        if !(-150.0..=50.0).contains(&db) {
            return Err(AppError::bad_request("音量必须在 -150 ~ +50 dB 之间"));
        }
        let replies = self
            .ws_commands(&[json!({ "SetVolume": db })])
            .await
            .map_err(|e| AppError::unavailable(format!("设置音量失败: {e}")))?;
        let mut new_db = db;
        for (name, reply) in replies {
            let v = reply_value(&reply, &name)
                .map_err(|e| AppError::unavailable(format!("CamillaDSP 拒绝音量设置: {e}")))?;
            if let Some(f) = v.as_f64() {
                new_db = f;
            } else if let Some(f) = v.get("volume").and_then(|v| v.as_f64()) {
                new_db = f;
            }
        }
        Ok(new_db)
    }
}

// ===== mock =====

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApplyRecord {
    pub config_name: Option<String>,
    pub path: String,
    pub bytes: usize,
}

struct MockInner {
    live: CamillaLive,
    configs: Vec<(String, String)>,
    current: String,
    applied: Vec<ApplyRecord>,
    fail_apply: bool,
}

pub struct MockCamilla {
    cfg: CamillaConfig,
    inner: Mutex<MockInner>,
}

pub const MOCK_AES67_YAML: &str = "title: AES67 输入 → MOTU 输出
description: mock 配置（48 kHz / 24-bit）
devices:
  samplerate: 48000
  chunksize: 128
  target_level: 2048
  enable_rate_adjust: true
  capture:
    type: Alsa
    channels: 2
    device: \"hw:Loopback,1,0\"
    format: S32LE
  playback:
    type: Alsa
    channels: 2
    device: \"hw:MOTU,0,0\"
    format: S32LE
filters: {}
mixers: {}
pipeline: []
";

pub const MOCK_DANTE_YAML: &str = "title: Dante 输入 → MOTU 输出
description: mock 配置（96 kHz / 24-bit）
devices:
  samplerate: 96000
  chunksize: 128
  target_level: 2048
  enable_rate_adjust: true
  capture:
    type: Alsa
    channels: 2
    device: \"hw:Loopback,0,0\"
    format: S32LE
  playback:
    type: Alsa
    channels: 2
    device: \"hw:MOTU,0,0\"
    format: S32LE
filters: {}
mixers: {}
pipeline: []
";

impl MockCamilla {
    pub fn new(cfg: CamillaConfig) -> Self {
        Self::with_fail_apply(cfg, false)
    }

    pub fn with_fail_apply(cfg: CamillaConfig, fail_apply: bool) -> Self {
        let live = CamillaLive {
            reachable: true,
            version: Some("4.1.3".into()),
            state: Some("Running".into()),
            stop_reason: Some("None".into()),
            capture_rate: Some(96000),
            processing_load: Some(8.4),
            buffer_level: Some(2048),
            rate_adjust: Some(1.0),
            clipped_samples: Some(0),
            volume_db: Some(-12.0),
            mute: Some(false),
            config_path: Some(cfg.config_file.clone()),
            config_title: Some("AES67 输入 → MOTU 输出".into()),
            error: None,
        };
        let current = MOCK_AES67_YAML.to_string();
        Self {
            cfg,
            inner: Mutex::new(MockInner {
                live,
                configs: vec![
                    ("aes67.yml".to_string(), MOCK_AES67_YAML.to_string()),
                    ("dante.yml".to_string(), MOCK_DANTE_YAML.to_string()),
                    ("broken.yml".to_string(), "devices: [unclosed\n".to_string()),
                ],
                current,
                applied: Vec::new(),
                fail_apply,
            }),
        }
    }

    pub fn applied(&self) -> Vec<ApplyRecord> {
        self.inner.lock().unwrap().applied.clone()
    }
    pub fn applied_count(&self) -> usize {
        self.inner.lock().unwrap().applied.len()
    }
    pub fn fail_next_apply(&self, fail: bool) {
        self.inner.lock().unwrap().fail_apply = fail;
    }
    pub fn set_state(&self, state: &str) {
        self.inner.lock().unwrap().live.state = Some(state.to_string());
    }
    pub fn set_live(&self, live: CamillaLive) {
        self.inner.lock().unwrap().live = live;
    }

    fn apply_mock(&self, content: &str, path: &str, name: Option<&str>) -> AppResult<ApplyOutcome> {
        validate_camilla_yaml(content)?;
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_apply {
            return Err(AppError::unavailable(
                "mock 注入：CamillaDSP 应用配置失败（用于验证回滚）",
            ));
        }
        inner.current = content.to_string();
        inner.live.config_path = Some(path.to_string());
        inner.live.state = Some("Running".into());
        inner.applied.push(ApplyRecord {
            config_name: name.map(str::to_string),
            path: path.to_string(),
            bytes: content.len(),
        });
        Ok(ApplyOutcome {
            applied_via: "websocket".into(),
            config_name: name.map(str::to_string),
            config_path: path.to_string(),
            persisted_to: vec![path.to_string()],
            backup: None,
            message: Some("mock 模式：配置仅保存在内存".into()),
        })
    }
}

#[async_trait]
impl CamillaBackend for MockCamilla {
    fn ws_url(&self) -> &str {
        &self.cfg.ws_url
    }
    fn service(&self) -> &str {
        &self.cfg.service
    }
    fn config_file(&self) -> &str {
        &self.cfg.config_file
    }
    fn config_dir(&self) -> &str {
        &self.cfg.config_dir
    }
    fn mock(&self) -> bool {
        true
    }

    async fn live(&self) -> CamillaLive {
        self.inner.lock().unwrap().live.clone()
    }

    async fn list_configs(&self) -> Vec<ConfigEntry> {
        let inner = self.inner.lock().unwrap();
        inner
            .configs
            .iter()
            .map(|(name, content)| ConfigEntry {
                name: name.clone(),
                path: format!("{}/{}", self.cfg.config_dir, name),
                size: content.len() as u64,
                modified_unix: Some(util::now_unix()),
                current: name == "aes67.yml",
            })
            .collect()
    }

    async fn read_named_config(&self, name: &str) -> AppResult<(String, String)> {
        util::validate_yaml_name(name).map_err(AppError::bad_request)?;
        let inner = self.inner.lock().unwrap();
        inner
            .configs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(n, c)| (format!("{}/{}", self.cfg.config_dir, n), c.clone()))
            .ok_or_else(|| AppError::not_found(format!("配置 {name} 不存在（mock）")))
    }

    async fn current_config_yaml(&self) -> AppResult<String> {
        Ok(self.inner.lock().unwrap().current.clone())
    }

    async fn apply_named_config(&self, name: &str) -> AppResult<ApplyOutcome> {
        let (_path, content) = self.read_named_config(name).await?;
        let target = format!("{}/{}", self.cfg.config_dir, name);
        self.apply_mock(&content, &target, Some(name))
    }

    async fn apply_config_path(&self, path: &str) -> AppResult<ApplyOutcome> {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        let inner = self.inner.lock().unwrap();
        let content = inner
            .configs
            .iter()
            .find(|(n, _)| Some(n.as_str()) == name.as_deref())
            .map(|(_, c)| c.clone())
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "mock 模式：模式配置 {path} 不存在（可用 dante.yml / aes67.yml）"
                ))
            })?;
        drop(inner);
        self.apply_mock(&content, path, name.as_deref())
    }

    async fn apply_config_content(
        &self,
        content: &str,
        primary_path: &str,
    ) -> AppResult<ApplyOutcome> {
        self.apply_mock(content, primary_path, None)
    }

    async fn set_volume(&self, db: f64) -> AppResult<f64> {
        if !(-150.0..=50.0).contains(&db) {
            return Err(AppError::bad_request("音量必须在 -150 ~ +50 dB 之间"));
        }
        let mut inner = self.inner.lock().unwrap();
        inner.live.volume_db = Some(db);
        Ok(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_yaml_accepts_mapping() {
        assert!(validate_camilla_yaml(MOCK_AES67_YAML).is_ok());
        assert!(validate_camilla_yaml(MOCK_DANTE_YAML).is_ok());
    }

    #[test]
    fn validate_yaml_rejects_errors() {
        let err = validate_camilla_yaml("devices: [unclosed\n").unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
        let err = validate_camilla_yaml("just-a-string\n").unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
        let err = validate_camilla_yaml("   ").unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn reply_value_ok_and_error() {
        let ok = json!({"GetState": {"result": "Ok", "value": "Running"}});
        assert_eq!(reply_value(&ok, "GetState").unwrap(), json!("Running"));
        let simple_err = json!({"GetState": {"result": "ShutdownInProgressError"}});
        assert!(reply_value(&simple_err, "GetState")
            .unwrap_err()
            .contains("ShutdownInProgress"));
        let detail_err =
            json!({"SetConfigJson": {"result": {"ConfigValidationError": "bad pipeline"}}});
        assert!(reply_value(&detail_err, "SetConfigJson")
            .unwrap_err()
            .contains("bad pipeline"));
        let missing = json!({"Other": {"result": "Ok"}});
        assert!(reply_value(&missing, "GetState").is_err());
    }

    #[test]
    fn device_fields_extract_and_patch() {
        let f = extract_devices_fields(MOCK_AES67_YAML).unwrap();
        assert_eq!(f.sample_rate, Some(48000));
        assert_eq!(f.chunksize, Some(128));
        assert_eq!(f.target_level, Some(2048));
        assert_eq!(f.capture_channels, Some(2));
        assert_eq!(f.capture_device.as_deref(), Some("hw:Loopback,1,0"));

        let patched =
            patch_devices_fields(MOCK_AES67_YAML, Some(96000), Some(256), Some(1024)).unwrap();
        let f2 = extract_devices_fields(&patched).unwrap();
        assert_eq!(f2.sample_rate, Some(96000));
        assert_eq!(f2.chunksize, Some(256));
        assert_eq!(f2.target_level, Some(1024));
        // 其他字段保留
        assert_eq!(f2.playback_device.as_deref(), Some("hw:MOTU,0,0"));
        assert!(patched.contains("title:"));
    }

    #[test]
    fn patch_rejects_missing_devices() {
        assert!(patch_devices_fields("title: x\n", Some(48000), None, None).is_err());
    }

    #[test]
    fn xrun_scan() {
        let logs = "ok line\nALSA playback underrun detected\nstate Running\nXRun recovery\n";
        let info = scan_xrun(logs);
        assert_eq!(info.recent_count, 2);
        assert!(info.last_line.as_deref().unwrap().contains("XRun"));
        assert!(scan_xrun("all good\n").hint.contains("未发现"));
    }

    #[tokio::test]
    async fn mock_config_switch_and_volume() {
        let stage = MockCamilla::new(CamillaConfig::default());
        let before = stage.applied_count();
        let outcome = stage.apply_named_config("dante.yml").await.unwrap();
        assert_eq!(outcome.applied_via, "websocket");
        assert_eq!(stage.applied_count(), before + 1);
        let yaml = stage.current_config_yaml().await.unwrap();
        assert!(yaml.contains("96000"));
        let v = stage.set_volume(-20.0).await.unwrap();
        assert_eq!(v, -20.0);
        assert!((stage.live().await.volume_db.unwrap() + 20.0).abs() < 1e-9);
        assert!(stage.set_volume(100.0).await.is_err());
    }

    #[tokio::test]
    async fn mock_apply_failure_path() {
        let stage = MockCamilla::with_fail_apply(CamillaConfig::default(), true);
        let err = stage.apply_named_config("dante.yml").await.unwrap_err();
        assert!(matches!(err, AppError::ServiceUnavailable(_)));
        assert_eq!(stage.applied_count(), 0);
        // 原配置未变
        assert!(stage.current_config_yaml().await.unwrap().contains("48000"));
    }

    #[tokio::test]
    async fn mock_unknown_and_broken_config() {
        let stage = MockCamilla::new(CamillaConfig::default());
        assert!(stage.read_named_config("../etc/passwd.yml").await.is_err());
        assert!(stage.read_named_config("missing.yml").await.is_err());
        assert!(stage.apply_named_config("broken.yml").await.is_err());
    }
}
