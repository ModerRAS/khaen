//! channelmap v2 路由：文档读写、validate/apply/rollback、profiles、FIR、测试音。

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::channelmap::{
    self, ChannelMap, ChannelMapView, FirFileEntry, InputRoleView, LayoutPresets, ProfileEntry,
    SaveOutcome, ValidationReport,
};
use crate::config::RunMode;
use crate::error::{AppError, AppResult};
use crate::speaker_route::{self, SpeakerRouteCatalog, SpeakerRouteRequest, SpeakerRouteResult};
use crate::state::AppState;

fn mode_sample_rate(mode: RunMode) -> u32 {
    if mode == RunMode::Dante {
        96_000
    } else {
        48_000
    }
}

async fn presets_for(state: &AppState) -> LayoutPresets {
    state.channelmap.load_presets().await
}

async fn probe_for(state: &AppState, doc: Option<&ChannelMap>) -> channelmap::ProbeInfo {
    state.channelmap.probe(&*state.alsa, doc).await
}

fn input_role_views(doc: &ChannelMap, presets: &LayoutPresets) -> Vec<InputRoleView> {
    let def = doc
        .input
        .presets
        .get(&doc.input.layout)
        .or_else(|| presets.input_layouts.get(&doc.input.layout));
    let Some(def) = def else {
        return Vec::new();
    };
    channelmap::resolve_input_roles(doc, presets)
        .into_iter()
        .map(|(id, channel)| {
            let label = def
                .roles
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.label.clone())
                .unwrap_or_else(|| id.clone());
            InputRoleView { id, label, channel }
        })
        .collect()
}

async fn build_view(state: &AppState) -> AppResult<ChannelMapView> {
    let doc = state.channelmap.load().await?;
    let presets = presets_for(state).await;
    let probe = probe_for(state, doc.as_ref()).await;
    let mode = state.mode.current_mode();
    let rate = mode_sample_rate(mode);
    let validation = match doc.as_ref() {
        Some(d) => state.channelmap.validate(d, &probe, rate).await?,
        None => ValidationReport {
            valid: true,
            errors: Vec::new(),
            warnings: vec![
                "尚未配置 channelmap：请用首启向导生成 active（probe → 选布局 → apply）".into(),
            ],
            channel_mismatch: false,
        },
    };
    let effective_roles = doc
        .as_ref()
        .map(|d| channelmap::effective_roles(d, &presets))
        .unwrap_or_default();
    let input_roles = doc
        .as_ref()
        .map(|d| input_role_views(d, &presets))
        .unwrap_or_default();
    let profiles = state.channelmap.profiles().await;
    let fir_files = state.channelmap.fir_files().await;
    Ok(ChannelMapView {
        enabled: state.config.channelmap.enabled,
        needs_setup: doc.is_none(),
        path: state.config.channelmap.config_file.clone(),
        presets_file: state.config.channelmap.presets_file.clone(),
        profiles_dir: state.config.channelmap.profiles_dir.clone(),
        fir_dir: state.config.channelmap.fir_dir.clone(),
        render_file: state.config.channelmap.render_file.clone(),
        document: doc,
        effective_roles,
        input_roles,
        validation,
        presets,
        probe,
        profiles,
        fir_files,
        mode_off: mode == RunMode::Off,
        mock: state.mock,
    })
}

/// GET /api/channelmap
pub async fn get_channelmap(State(state): State<AppState>) -> AppResult<Json<ChannelMapView>> {
    Ok(Json(build_view(&state).await?))
}

/// GET /api/channelmap/input-mode：输入内容模式（运行态 + 覆盖）。
pub async fn get_input_mode(
    State(state): State<AppState>,
) -> AppResult<Json<channelmap::InputModeView>> {
    Ok(Json(state.channelmap.input_mode().await))
}

#[derive(Debug, Deserialize)]
pub struct InputModeRequest {
    pub mode: String,
}

/// PUT /api/channelmap/input-mode：写覆盖（auto|stereo|multichannel，兼容 force-*）。
pub async fn set_input_mode(
    State(state): State<AppState>,
    Json(body): Json<InputModeRequest>,
) -> AppResult<Json<channelmap::InputModeView>> {
    Ok(Json(state.channelmap.set_input_mode(&body.mode).await?))
}

#[derive(Debug, Deserialize, Default)]
pub struct DraftRequest {
    #[serde(default)]
    pub input_layout: Option<String>,
    #[serde(default)]
    pub output_layout: Option<String>,
    #[serde(default)]
    pub profile_name: Option<String>,
    #[serde(default)]
    pub capture_channels: Option<u32>,
    #[serde(default)]
    pub playback_channels: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct DraftResponse {
    pub document: ChannelMap,
    pub probe: channelmap::ProbeInfo,
    pub presets: LayoutPresets,
}

/// GET /api/channelmap/speaker-route：角色词表和 5.1.2 / 4.1.2 预设。
pub async fn get_speaker_route() -> Json<SpeakerRouteCatalog> {
    Json(speaker_route::catalog())
}

/// POST /api/channelmap/speaker-route：由逐槽角色派生流向和可执行 path。
pub async fn post_speaker_route(Json(body): Json<SpeakerRouteRequest>) -> Json<SpeakerRouteResult> {
    Json(speaker_route::derive(&body))
}

/// POST /api/channelmap/draft：首启向导/换布局生成显式草稿。
pub async fn draft_channelmap(
    State(state): State<AppState>,
    Json(body): Json<DraftRequest>,
) -> AppResult<Json<DraftResponse>> {
    let presets = presets_for(&state).await;
    let probe = probe_for(&state, None).await;
    let input_layout = body
        .input_layout
        .filter(|s| !s.trim().is_empty())
        .or_else(|| presets.input_layouts.keys().next().cloned())
        .ok_or_else(|| AppError::bad_request("没有可用的输入布局预设"))?;
    let output_layout = body
        .output_layout
        .filter(|s| !s.trim().is_empty())
        .or_else(|| presets.output_layouts.keys().next().cloned())
        .ok_or_else(|| AppError::bad_request("没有可用的输出布局预设"))?;
    let cap = body.capture_channels.or_else(|| probe.capture_channels());
    let pb = body.playback_channels.or_else(|| probe.playback_channels());
    if speaker_route::rejects_channelmap_draft(&input_layout, &output_layout) {
        return Err(AppError::bad_request(
            "nsp-5.1.2 到 4.1.2 不能用 channelmap 重映射：中置要在内容适配里下混。请用声道位置页生成 plan path。",
        ));
    }
    let name = body
        .profile_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{input_layout}__to__{output_layout}"));
    let document =
        channelmap::draft_document(&input_layout, &output_layout, &name, &presets, cap, pb);
    Ok(Json(DraftResponse {
        document,
        probe,
        presets,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct DocumentRequest {
    #[serde(default)]
    pub document: Option<ChannelMap>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub validation: ValidationReport,
    pub probe: channelmap::ProbeInfo,
    pub effective_roles: Vec<channelmap::RoleDef>,
    pub input_roles: Vec<InputRoleView>,
}

/// POST /api/channelmap/validate
pub async fn validate_doc(
    State(state): State<AppState>,
    Json(body): Json<DocumentRequest>,
) -> AppResult<Json<ValidateResponse>> {
    let presets = presets_for(&state).await;
    let doc = match body.document {
        Some(d) => d,
        None => state
            .channelmap
            .load()
            .await?
            .ok_or_else(|| AppError::bad_request("没有可校验的 channelmap"))?,
    };
    let probe = probe_for(&state, Some(&doc)).await;
    let rate = mode_sample_rate(state.mode.current_mode());
    let validation = state.channelmap.validate(&doc, &probe, rate).await?;
    Ok(Json(ValidateResponse {
        validation,
        probe,
        effective_roles: channelmap::effective_roles(&doc, &presets),
        input_roles: input_role_views(&doc, &presets),
    }))
}

#[derive(Debug, Serialize)]
pub struct SaveResponse {
    pub ok: bool,
    pub outcome: SaveOutcome,
    pub validation: ValidationReport,
}

/// PUT /api/channelmap：校验→（可选 force 存草稿）→备份→写 active（物化显式 roles）。
pub async fn save_channelmap(
    State(state): State<AppState>,
    Json(body): Json<DocumentRequest>,
) -> AppResult<Json<SaveResponse>> {
    let mut doc = body
        .document
        .ok_or_else(|| AppError::bad_request("缺少 document"))?;
    if doc.schema.is_empty() {
        doc.schema = channelmap::CHANNELMAP_SCHEMA.into();
    }
    if doc.version == 0 {
        doc.version = 2;
    }
    let presets = presets_for(&state).await;
    channelmap::materialize_document(&mut doc, &presets);
    let probe = probe_for(&state, Some(&doc)).await;
    let rate = mode_sample_rate(state.mode.current_mode());
    let validation = state.channelmap.validate(&doc, &probe, rate).await?;
    if !validation.valid && !body.force {
        return Err(AppError::bad_request(format!(
            "channelmap 校验失败：{}（force=true 可强制保存草稿）",
            validation.errors.join("；")
        )));
    }
    let outcome = state.channelmap.save(&doc).await?;
    Ok(Json(SaveResponse {
        ok: true,
        outcome,
        validation,
    }))
}

#[derive(Debug, Serialize)]
pub struct ApplyResponse {
    pub ok: bool,
    pub saved: bool,
    pub applied: bool,
    pub saved_only: bool,
    pub rolled_back: bool,
    pub message: String,
    pub via: String,
    pub mode: RunMode,
    pub validation: ValidationReport,
    pub view: ChannelMapView,
}

/// POST /api/channelmap/apply：保存→校验→tspi-channelmap apply（off 只保存/渲染）。
pub async fn apply_channelmap(
    State(state): State<AppState>,
    Json(body): Json<DocumentRequest>,
) -> AppResult<Json<ApplyResponse>> {
    if let Some(mut doc) = body.document {
        let presets = presets_for(&state).await;
        channelmap::materialize_document(&mut doc, &presets);
        let probe = probe_for(&state, Some(&doc)).await;
        let rate = mode_sample_rate(state.mode.current_mode());
        let validation = state.channelmap.validate(&doc, &probe, rate).await?;
        if !validation.valid {
            return Err(AppError::bad_request(format!(
                "channelmap 校验失败，已阻止 apply：{}",
                validation.errors.join("；")
            )));
        }
        state.channelmap.save(&doc).await?;
    }
    let mode = state.mode.current_mode();
    let doc = state
        .channelmap
        .load()
        .await?
        .ok_or_else(|| AppError::bad_request("没有可应用的 channelmap"))?;
    let probe = probe_for(&state, Some(&doc)).await;
    let rate = mode_sample_rate(mode);
    let validation = state.channelmap.validate(&doc, &probe, rate).await?;
    if !validation.valid {
        return Err(AppError::bad_request(format!(
            "channelmap 校验失败，已阻止 apply：{}",
            validation.errors.join("；")
        )));
    }
    let outcome = state.channelmap.apply(mode, &probe).await?;
    let view = build_view(&state).await?;
    Ok(Json(ApplyResponse {
        ok: true,
        saved: outcome.saved,
        applied: outcome.applied,
        saved_only: outcome.saved_only,
        rolled_back: outcome.rolled_back,
        message: outcome.message,
        via: outcome.via,
        mode,
        validation,
        view,
    }))
}

/// POST /api/channelmap/rollback
pub async fn rollback_channelmap(
    State(state): State<AppState>,
) -> AppResult<Json<channelmap::ApplyOutcome>> {
    let mode = state.mode.current_mode();
    Ok(Json(state.channelmap.rollback(mode).await?))
}

#[derive(Debug, Serialize)]
pub struct ProfilesResponse {
    pub profiles: Vec<ProfileEntry>,
    pub profiles_dir: String,
}

/// GET /api/channelmap/profiles
pub async fn list_profiles(State(state): State<AppState>) -> AppResult<Json<ProfilesResponse>> {
    Ok(Json(ProfilesResponse {
        profiles: state.channelmap.profiles().await,
        profiles_dir: state.config.channelmap.profiles_dir.clone(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SaveProfileRequest {
    pub name: String,
    #[serde(default)]
    pub document: Option<ChannelMap>,
}

#[derive(Debug, Serialize)]
pub struct SaveProfileResponse {
    pub ok: bool,
    pub profile: ProfileEntry,
}

/// POST /api/channelmap/profiles（按 name 保存 profile）
pub async fn save_profile(
    State(state): State<AppState>,
    Json(body): Json<SaveProfileRequest>,
) -> AppResult<Json<SaveProfileResponse>> {
    let doc = match body.document {
        Some(d) => d,
        None => state
            .channelmap
            .load()
            .await?
            .ok_or_else(|| AppError::bad_request("没有可保存的 channelmap"))?,
    };
    let profile = state.channelmap.save_profile(&body.name, &doc).await?;
    Ok(Json(SaveProfileResponse { ok: true, profile }))
}

#[derive(Debug, Deserialize)]
pub struct ProfileNameRequest {
    pub name: String,
}

/// POST /api/channelmap/profiles/load：复制为 active（不自动 apply）
pub async fn load_profile(
    State(state): State<AppState>,
    Json(body): Json<ProfileNameRequest>,
) -> AppResult<Json<ChannelMapView>> {
    state.channelmap.load_profile(&body.name).await?;
    Ok(Json(build_view(&state).await?))
}

/// POST /api/channelmap/profiles/delete
pub async fn delete_profile(
    State(state): State<AppState>,
    Json(body): Json<ProfileNameRequest>,
) -> AppResult<Json<serde_json::Value>> {
    state.channelmap.delete_profile(&body.name).await?;
    Ok(Json(
        serde_json::json!({ "ok": true, "deleted": body.name }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub name: Option<String>,
}

/// GET /api/channelmap/export?name= —— 导出 YAML
pub async fn export_channelmap(
    State(state): State<AppState>,
    Query(q): Query<ExportQuery>,
) -> AppResult<impl IntoResponse> {
    let yaml = match q.name.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => state.channelmap.export_profile_yaml(name).await?,
        None => state.channelmap.export_active_yaml().await?,
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/yaml; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=channelmap.yml",
            ),
        ],
        yaml,
    ))
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub yaml: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub ok: bool,
    pub validation: ValidationReport,
    pub path: String,
}

/// POST /api/channelmap/import
pub async fn import_channelmap(
    State(state): State<AppState>,
    Json(body): Json<ImportRequest>,
) -> AppResult<Json<ImportResponse>> {
    let mut doc = channelmap::parse_document(&body.yaml)?;
    let presets = presets_for(&state).await;
    channelmap::materialize_document(&mut doc, &presets);
    let probe = probe_for(&state, Some(&doc)).await;
    let rate = mode_sample_rate(state.mode.current_mode());
    let validation = state.channelmap.validate(&doc, &probe, rate).await?;
    if !validation.valid {
        return Err(AppError::bad_request(format!(
            "导入的 channelmap 校验失败：{}",
            validation.errors.join("；")
        )));
    }
    let path = match body.name.filter(|s| !s.trim().is_empty()) {
        Some(name) => state.channelmap.save_profile(&name, &doc).await?.path,
        None => state.channelmap.save(&doc).await?.path,
    };
    Ok(Json(ImportResponse {
        ok: true,
        validation,
        path,
    }))
}

#[derive(Debug, Serialize)]
pub struct FirResponse {
    pub files: Vec<FirFileEntry>,
    pub fir_dir: String,
}

/// GET /api/channelmap/fir
pub async fn list_fir(State(state): State<AppState>) -> AppResult<Json<FirResponse>> {
    Ok(Json(FirResponse {
        files: state.channelmap.fir_files().await,
        fir_dir: state.config.channelmap.fir_dir.clone(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct UploadFirRequest {
    pub name: String,
    pub data_base64: String,
}

/// POST /api/channelmap/fir
pub async fn upload_fir(
    State(state): State<AppState>,
    Json(body): Json<UploadFirRequest>,
) -> AppResult<Json<FirFileEntry>> {
    let bytes = decode_base64(&body.data_base64)
        .map_err(|e| AppError::bad_request(format!("base64 解码失败: {e}")))?;
    Ok(Json(state.channelmap.save_fir(&body.name, &bytes).await?))
}

#[derive(Debug, Deserialize, Default)]
pub struct TestToneRequest {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub output_channel: Option<u32>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u32>,
    #[serde(default)]
    pub device: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TestToneResponse {
    pub played: bool,
    pub mock: bool,
    pub role: Option<String>,
    pub output_channel: u32,
    pub device: String,
    pub channels: u32,
    pub sample_rate: u32,
    pub kind: String,
    pub duration_ms: u32,
    pub wav_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// POST /api/channelmap/test-tone
pub async fn test_tone(
    State(state): State<AppState>,
    Json(body): Json<TestToneRequest>,
) -> AppResult<Json<TestToneResponse>> {
    let doc = state
        .channelmap
        .load()
        .await?
        .ok_or_else(|| AppError::bad_request("尚未配置 channelmap"))?;
    let presets = presets_for(&state).await;
    let roles = channelmap::effective_roles(&doc, &presets);
    let probe = probe_for(&state, Some(&doc)).await;
    let idx = match (body.output_channel, body.role.as_deref()) {
        (Some(i), _) => i,
        (None, Some(role)) => roles
            .iter()
            .find(|r| r.id == role)
            .and_then(|r| r.outputs.first().copied())
            .ok_or_else(|| AppError::bad_request(format!("角色 {role} 没有输出通道映射")))?,
        (None, None) => return Err(AppError::bad_request("需要 role 或 output_channel")),
    };
    let channels = probe
        .playback_channels()
        .or_else(|| channelmap::output_layout_channels(&doc, &presets))
        .unwrap_or(idx + 1)
        .max(idx + 1);
    let rate = mode_sample_rate(state.mode.current_mode());
    let device = body
        .device
        .filter(|d| !d.trim().is_empty())
        .or_else(|| {
            (!doc.output.device.trim().is_empty() && doc.output.device != "auto")
                .then(|| doc.output.device.clone())
        })
        .or_else(|| probe.playback.as_ref().map(|p| p.device.clone()))
        .ok_or_else(|| {
            AppError::bad_request("没有可用输出设备（先 probe 或设置 output.device）")
        })?;
    let kind = body.kind.unwrap_or_else(|| "tone".into());
    let sweep = kind == "sweep";
    let duration_ms = body
        .duration_ms
        .unwrap_or(if sweep { 2000 } else { 1000 })
        .min(10_000);
    let wav = channelmap::build_test_tone_wav(channels, idx, rate, duration_ms, sweep);
    if state.mock {
        return Ok(Json(TestToneResponse {
            played: true,
            mock: true,
            role: body.role.clone(),
            output_channel: idx,
            device,
            channels,
            sample_rate: rate,
            kind,
            duration_ms,
            wav_bytes: wav.len(),
            message: Some("mock：未执行 aplay（返回 WAV 元数据）".into()),
        }));
    }
    let path = std::env::temp_dir().join(format!(
        "tspi-tone-{}-{}.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    tokio::fs::write(&path, &wav)
        .await
        .map_err(|e| AppError::internal(format!("写测试音失败: {e}")))?;
    let path_str = path.to_string_lossy().to_string();
    let out = state
        .runner
        .run_with_timeout(
            "aplay",
            &["-q", "-D", &device, &path_str],
            std::time::Duration::from_secs(20),
        )
        .await;
    let _ = tokio::fs::remove_file(&path).await;
    match out {
        Ok(o) if o.success => Ok(Json(TestToneResponse {
            played: true,
            mock: false,
            role: body.role.clone(),
            output_channel: idx,
            device,
            channels,
            sample_rate: rate,
            kind,
            duration_ms,
            wav_bytes: wav.len(),
            message: None,
        })),
        Ok(o) => Err(AppError::unavailable(format!(
            "aplay 播放失败（退出码 {:?}）：{}",
            o.status,
            o.stderr.trim().lines().next().unwrap_or("无错误输出")
        ))),
        Err(e) => Err(AppError::unavailable(format!("aplay 执行失败: {e}"))),
    }
}

/// GET /api/channelmap/render：查看最近渲染的 CamillaDSP 配置（只读）。
pub async fn render_config(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    let doc = state.channelmap.load().await?;
    let probe = probe_for(&state, doc.as_ref()).await;
    let text = state.channelmap.render_text(&probe).await?;
    Ok(Json(serde_json::json!({
        "render_file": state.config.channelmap.render_file,
        "text": text,
    })))
}

/// 最小 base64 解码（标准字母表，忽略空白）。
fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let mut table = [255u8; 256];
    for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[*c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in input.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = table[c as usize];
        if v == 255 {
            return Err(format!("非法字符 0x{c:02x}"));
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello");
        assert_eq!(decode_base64("AAEC").unwrap(), vec![0, 1, 2]);
        assert!(decode_base64("!!").is_err());
    }

    #[test]
    fn mode_rate() {
        assert_eq!(mode_sample_rate(RunMode::Dante), 96_000);
        assert_eq!(mode_sample_rate(RunMode::Aes67), 48_000);
        assert_eq!(mode_sample_rate(RunMode::Off), 48_000);
    }
}
