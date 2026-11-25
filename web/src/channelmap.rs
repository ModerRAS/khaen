//! channelmap v2 对接：通用布局 / remap / 角色校准 / 低音管理 / FIR。
//!
//! 契约来源：audio-stack 的 tspi-channelmap（schema=tspi-channelmap/v2）
//! 与 /etc/tspi-audio/layout-presets.yml（schema=tspi-layout-presets/v2）。
//! 命令：tspi-channelmap probe/validate/print/render/apply/rollback/profiles/...
//! 退出码：0 ok；2 schema/用法；3 取值/校验；4 渲染/--check；5 apply/重启；9 回滚。
//!
//! 设计：WebUI 只维护显式 output.roles + sources（物化后保存），
//! remap.preset/routes 仅用于“套用预设生成 routes”，保存前会物化成 sources。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::alsa::{AlsaBackend, AlsaView};
use crate::cmd::{CommandOutput, CommandRunner};
use crate::config::ChannelmapConfig;
use crate::error::{AppError, AppResult};
use crate::util;

pub const CHANNELMAP_SCHEMA: &str = "tspi-channelmap/v2";
pub const PRESETS_SCHEMA: &str = "tspi-layout-presets/v2";
pub const EMBEDDED_LAYOUT_PRESETS: &str = include_str!("../assets/layout-presets.yml");

// ===== 基础模型 =====

/// 通道数：auto 或固定整数（与 tspi-channelmap v2 一致）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChannelCount {
    Auto(String),
    Fixed(u32),
}

impl Default for ChannelCount {
    fn default() -> Self {
        ChannelCount::Auto("auto".into())
    }
}

impl ChannelCount {
    pub fn is_auto(&self) -> bool {
        matches!(self, ChannelCount::Auto(_))
    }
    pub fn fixed(&self) -> Option<u32> {
        match self {
            ChannelCount::Fixed(v) => Some(*v),
            ChannelCount::Auto(_) => None,
        }
    }
}

fn is_false(v: &bool) -> bool {
    !*v
}
fn default_true() -> bool {
    true
}
fn default_polarity() -> String {
    "normal".into()
}
fn default_route_mode() -> String {
    "direct".into()
}
fn default_fir_type() -> String {
    "LinkwitzRiley".into()
}
fn default_order() -> u32 {
    4
}
fn default_lfe_role() -> String {
    "LFE".into()
}
fn default_hp_hz() -> f64 {
    80.0
}
fn default_sub_gain() -> f64 {
    0.0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InputSection {
    pub layout: String,
    pub channels: ChannelCount,
    /// 用户/本地自定义输入布局（覆盖 presets 中同名项）。
    pub presets: BTreeMap<String, InputLayoutDef>,
    pub probe_order: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutputSection {
    pub device: String,
    pub channels: ChannelCount,
    pub layout: String,
    /// 显式角色（WebUI 编辑后物化；为空时用 layout preset + remap 生成）。
    pub roles: Vec<RoleDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RemapSection {
    pub preset: Option<String>,
    pub routes: Vec<Route>,
    pub drop_unused: bool,
    #[serde(default = "default_true")]
    pub silence_unmapped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Route {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    pub gain_db: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    #[serde(default = "default_route_mode")]
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoleSource {
    /// 输入布局中的源角色名（v2 字段为 source，resolved 输出为 src）。
    #[serde(rename = "source", alias = "src")]
    pub source: String,
    pub gain_db: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub inverted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct XoBand {
    pub freq: f64,
    #[serde(default = "default_order")]
    pub order: u32,
    #[serde(rename = "type", default = "default_fir_type")]
    pub filter_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PeqBand {
    pub freq: f64,
    pub q: f64,
    pub gain_db: f64,
    /// peaking | low_shelf | high_shelf。空字符串按 peaking。
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    pub filter_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FirRef {
    Path(String),
    Obj(FirObj),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FirObj {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samplerate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u32>,
}

impl FirRef {
    pub fn path(&self) -> &str {
        match self {
            FirRef::Path(p) => p,
            FirRef::Obj(o) => &o.path,
        }
    }
    pub fn samplerate(&self) -> Option<u32> {
        match self {
            FirRef::Path(_) => None,
            FirRef::Obj(o) => o.samplerate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleDef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    pub outputs: Vec<u32>,
    pub sources: Vec<RoleSource>,
    pub gain_db: f64,
    pub delay_ms: f64,
    #[serde(default = "default_polarity")]
    pub polarity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highpass: Option<XoBand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lowpass: Option<XoBand>,
    pub peq: Vec<PeqBand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fir: Option<FirRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_to: Option<String>,
}

impl Default for RoleDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: None,
            enabled: true,
            mute: false,
            outputs: Vec::new(),
            sources: Vec::new(),
            gain_db: 0.0,
            delay_ms: 0.0,
            polarity: "normal".into(),
            highpass: None,
            lowpass: None,
            peq: Vec::new(),
            fir: None,
            link_to: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BassRedirect {
    pub enabled: bool,
    pub sources: Vec<String>,
    pub targets: Vec<String>,
    pub gain_db: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BassManagement {
    pub enabled: bool,
    #[serde(default = "default_lfe_role")]
    pub lfe_role: String,
    pub lfe_to: Vec<String>,
    pub lfe_gain_db: f64,
    pub redirect: BassRedirect,
    #[serde(default = "default_hp_hz")]
    pub mains_highpass_hz: f64,
    #[serde(default = "default_order")]
    pub mains_highpass_order: u32,
    #[serde(default = "default_hp_hz")]
    pub sub_lowpass_hz: f64,
    #[serde(default = "default_order")]
    pub sub_lowpass_order: u32,
    #[serde(default = "default_sub_gain")]
    pub sub_gain_db: f64,
}

impl Default for BassManagement {
    fn default() -> Self {
        Self {
            enabled: false,
            lfe_role: "LFE".into(),
            lfe_to: Vec::new(),
            lfe_gain_db: 0.0,
            redirect: BassRedirect {
                enabled: true,
                sources: Vec::new(),
                targets: Vec::new(),
                gain_db: 0.0,
            },
            mains_highpass_hz: 80.0,
            mains_highpass_order: 4,
            sub_lowpass_hz: 80.0,
            sub_lowpass_order: 4,
            sub_gain_db: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelMap {
    pub schema: String,
    pub version: u32,
    pub profile: ProfileInfo,
    pub input: InputSection,
    pub output: OutputSection,
    pub remap: RemapSection,
    pub bass_management: BassManagement,
}

impl Default for ChannelMap {
    fn default() -> Self {
        Self {
            schema: CHANNELMAP_SCHEMA.into(),
            version: 2,
            profile: ProfileInfo::default(),
            input: InputSection {
                layout: String::new(),
                channels: ChannelCount::default(),
                presets: BTreeMap::new(),
                probe_order: None,
            },
            output: OutputSection {
                device: "auto".into(),
                channels: ChannelCount::default(),
                layout: String::new(),
                roles: Vec::new(),
            },
            remap: RemapSection {
                preset: None,
                routes: Vec::new(),
                drop_unused: true,
                silence_unmapped: true,
            },
            bass_management: BassManagement::default(),
        }
    }
}

// ===== 布局预设（tspi-layout-presets/v2） =====

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InputRoleDef {
    pub id: String,
    pub label: Option<String>,
    #[serde(default)]
    pub stream: u32,
    pub channel: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InputLayoutDef {
    pub label: Option<String>,
    pub description: Option<String>,
    pub channels: Option<ChannelCount>,
    pub streams: Vec<u32>,
    pub roles: Vec<InputRoleDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutputRoleDef {
    pub id: String,
    pub label: Option<String>,
    pub outputs: Vec<u32>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutputLayoutDef {
    pub label: Option<String>,
    pub description: Option<String>,
    pub roles: Vec<OutputRoleDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RemapPresetDef {
    pub label: Option<String>,
    pub description: Option<String>,
    pub input: String,
    pub output: String,
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LayoutPresets {
    pub schema: String,
    pub input_layouts: BTreeMap<String, InputLayoutDef>,
    pub output_layouts: BTreeMap<String, OutputLayoutDef>,
    pub remap_presets: BTreeMap<String, RemapPresetDef>,
}

impl LayoutPresets {
    pub fn parse(text: &str) -> AppResult<Self> {
        let v: LayoutPresets = serde_yaml::from_str(text)
            .map_err(|e| AppError::internal(format!("layout-presets YAML 解析失败: {e}")))?;
        if !v.schema.is_empty() && v.schema != PRESETS_SCHEMA {
            return Err(AppError::internal(format!(
                "layout-presets schema 应为 {PRESETS_SCHEMA}，实际 {}",
                v.schema
            )));
        }
        Ok(v)
    }
}

/// 从 preset 或文档内联定义解析输入角色到全局通道号（0-based）。
pub fn resolve_input_roles(doc: &ChannelMap, presets: &LayoutPresets) -> BTreeMap<String, u32> {
    let def = doc
        .input
        .presets
        .get(&doc.input.layout)
        .or_else(|| presets.input_layouts.get(&doc.input.layout));
    let mut out = BTreeMap::new();
    if let Some(def) = def {
        let streams = &def.streams;
        for role in &def.roles {
            let idx = if !streams.is_empty() {
                let st = role.stream as usize;
                if st >= streams.len() {
                    continue;
                }
                streams[..st].iter().sum::<u32>() + role.channel
            } else {
                role.channel
            };
            out.insert(role.id.clone(), idx);
        }
    }
    out
}

/// 输入布局总通道数（显式 channels 或 streams 合计；未知 None）。
pub fn input_layout_channels(doc: &ChannelMap, presets: &LayoutPresets) -> Option<u32> {
    let def = doc
        .input
        .presets
        .get(&doc.input.layout)
        .or_else(|| presets.input_layouts.get(&doc.input.layout))?;
    if let Some(c) = def.channels.as_ref().and_then(ChannelCount::fixed) {
        return Some(c);
    }
    if !def.streams.is_empty() {
        return Some(def.streams.iter().sum());
    }
    None
}

/// 输出布局角色定义（显式 roles 优先，否则 preset）。
pub fn output_layout_roles(doc: &ChannelMap, presets: &LayoutPresets) -> Vec<OutputRoleDef> {
    if !doc.output.roles.is_empty() {
        return doc
            .output
            .roles
            .iter()
            .map(|r| OutputRoleDef {
                id: r.id.clone(),
                label: r.label.clone(),
                outputs: r.outputs.clone(),
                enabled: r.enabled,
            })
            .collect();
    }
    presets
        .output_layouts
        .get(&doc.output.layout)
        .map(|l| l.roles.clone())
        .unwrap_or_default()
}

/// 输出布局所需物理通道数（最大 outputs+1）。
pub fn output_layout_channels(doc: &ChannelMap, presets: &LayoutPresets) -> Option<u32> {
    if let Some(c) = doc.output.channels.fixed() {
        return Some(c);
    }
    let roles = output_layout_roles(doc, presets);
    let max = roles.iter().flat_map(|r| r.outputs.iter().copied()).max()?;
    Some(max + 1)
}

fn route_sources(
    routes: &[Route],
    input_roles: &BTreeMap<String, u32>,
) -> BTreeMap<String, Vec<RoleSource>> {
    let mut out: BTreeMap<String, Vec<RoleSource>> = BTreeMap::new();
    for r in routes {
        if r.mode == "drop" || r.mode == "silence" {
            if let Some(dest) = &r.dest {
                out.entry(dest.clone()).or_default();
            }
            continue;
        }
        let (Some(src), Some(dest)) = (r.src.as_ref(), r.dest.as_ref()) else {
            continue;
        };
        if !input_roles.contains_key(src) {
            continue;
        }
        out.entry(dest.clone()).or_default().push(RoleSource {
            source: src.clone(),
            gain_db: r.gain_db,
            inverted: false,
            mute: r.mute,
        });
    }
    out
}

/// 物化输出角色：显式 roles 优先；否则 layout preset + remap preset/routes/同名直通。
pub fn effective_roles(doc: &ChannelMap, presets: &LayoutPresets) -> Vec<RoleDef> {
    let input_roles = resolve_input_roles(doc, presets);
    if !doc.output.roles.is_empty() {
        let mut roles = doc.output.roles.clone();
        for r in &mut roles {
            if r.sources.is_empty() {
                if let Some(src) = input_roles.get(&r.id) {
                    let _ = src;
                    r.sources.push(RoleSource {
                        source: r.id.clone(),
                        ..Default::default()
                    });
                }
            }
        }
        return roles;
    }
    let routes: Vec<Route> = if !doc.remap.routes.is_empty() {
        doc.remap.routes.clone()
    } else if let Some(preset) = &doc.remap.preset {
        presets
            .remap_presets
            .get(preset)
            .map(|p| p.routes.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let routed = route_sources(&routes, &input_roles);
    output_layout_roles(doc, presets)
        .into_iter()
        .enumerate()
        .map(|(idx, rd)| {
            let mut role = RoleDef {
                id: rd.id.clone(),
                label: rd.label.clone(),
                enabled: rd.enabled,
                outputs: rd.outputs.clone(),
                ..Default::default()
            };
            if !role.enabled && role.outputs.is_empty() {
                role.outputs.push(idx as u32);
            }
            if let Some(sources) = routed.get(&rd.id) {
                role.sources = sources.clone();
            } else if !routes.is_empty() {
                role.sources = Vec::new();
            } else if input_roles.contains_key(&rd.id) {
                role.sources.push(RoleSource {
                    source: rd.id.clone(),
                    ..Default::default()
                });
            }
            role
        })
        .collect()
}

/// 构造显式文档：用于首启向导/套用预设；保存前调用。
pub fn materialize_document(doc: &mut ChannelMap, presets: &LayoutPresets) {
    doc.schema = CHANNELMAP_SCHEMA.into();
    doc.version = 2;
    if doc.profile.name.trim().is_empty() {
        doc.profile.name = format!(
            "{}__to__{}",
            if doc.input.layout.is_empty() {
                "input"
            } else {
                &doc.input.layout
            },
            if doc.output.layout.is_empty() {
                "output"
            } else {
                &doc.output.layout
            }
        );
    }
    if doc.input.layout.is_empty() {
        if let Some((k, _)) = presets.input_layouts.iter().next() {
            doc.input.layout = k.clone();
        }
    }
    if doc.output.layout.is_empty() {
        if let Some((k, _)) = presets.output_layouts.iter().next() {
            doc.output.layout = k.clone();
        }
    }
    doc.output.roles = effective_roles(doc, presets);
    doc.remap.preset = None;
    doc.remap.routes.clear();
}

/// 首启/换布局草稿：输入布局 x 输出布局，按 remap preset 或同名直通生成显式 roles。
pub fn draft_document(
    input_layout: &str,
    output_layout: &str,
    profile_name: &str,
    presets: &LayoutPresets,
    capture_channels: Option<u32>,
    playback_channels: Option<u32>,
) -> ChannelMap {
    let mut doc = ChannelMap {
        schema: CHANNELMAP_SCHEMA.into(),
        version: 2,
        profile: ProfileInfo {
            name: profile_name.to_string(),
            description: format!("{input_layout} -> {output_layout}（WebUI 生成）"),
        },
        input: InputSection {
            layout: input_layout.into(),
            channels: capture_channels.map_or(ChannelCount::default(), ChannelCount::Fixed),
            presets: BTreeMap::new(),
            probe_order: None,
        },
        output: OutputSection {
            device: "auto".into(),
            channels: playback_channels.map_or(ChannelCount::default(), ChannelCount::Fixed),
            layout: output_layout.into(),
            roles: Vec::new(),
        },
        ..Default::default()
    };
    let preset = presets
        .remap_presets
        .values()
        .find(|p| p.input == input_layout && p.output == output_layout);
    if let Some(p) = preset {
        doc.remap.routes = p.routes.clone();
        doc.remap.preset = None;
        doc.remap.drop_unused = true;
        doc.remap.silence_unmapped = true;
    }
    materialize_document(&mut doc, presets);
    doc.remap.drop_unused = true;
    doc.remap.silence_unmapped = true;
    doc
}

// ===== probe（tspi-channelmap probe --json） =====

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProbeDevice {
    pub device: String,
    #[serde(default)]
    pub channels_min: Option<u32>,
    #[serde(default)]
    pub channels_max: Option<u32>,
    pub formats: Vec<String>,
    pub rates: Vec<u32>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProbeInfo {
    pub ok: bool,
    pub playback: Option<ProbeDevice>,
    pub capture: Option<ProbeDevice>,
    pub cards: Vec<String>,
    pub notes: Vec<String>,
    pub needs_user_choice: bool,
}

impl ProbeInfo {
    pub fn playback_channels(&self) -> Option<u32> {
        self.playback.as_ref().and_then(|p| p.channels_max)
    }
    pub fn capture_channels(&self) -> Option<u32> {
        self.capture.as_ref().and_then(|p| p.channels_max)
    }
}

/// 解析 probe 输出的 JSON（容错空/异常）。
pub fn parse_probe_json(text: &str) -> Option<ProbeInfo> {
    serde_json::from_str::<ProbeInfo>(text).ok()
}

// ===== 校验 =====

#[derive(Debug, Clone, Serialize, Default)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub channel_mismatch: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ProbeChannels {
    pub capture: Option<u32>,
    pub playback: Option<u32>,
}

fn num_range(v: f64, lo: f64, hi: f64, path: &str, errors: &mut Vec<String>) {
    if !(lo..=hi).contains(&v) {
        errors.push(format!("{path} 取值 {v} 超出 [{lo}, {hi}]"));
    }
}

fn validate_xo(xo: &Option<XoBand>, path: &str, strict_hz: f64, errors: &mut Vec<String>) {
    let Some(xo) = xo else { return };
    if xo.filter_type != "LinkwitzRiley" {
        errors.push(format!("{path}.type 只能是 LinkwitzRiley"));
    }
    if ![2, 4, 6, 8].contains(&xo.order) {
        errors.push(format!("{path}.order 必须是 2/4/6/8"));
    }
    if !(20.0..strict_hz / 2.0).contains(&xo.freq) {
        errors.push(format!("{path}.freq 必须在 [20, {strict_hz}/2)"));
    }
}

/// 本地校验（仅 mock/测试/演示使用），规则镜像板端 tspi-channelmap v2。
///
/// 真机不再回退到这里：板端 Python 是唯一权威；CLI 不可用时 `validate()` 直接报错。
/// 对齐要点：`peq.type` ∈ {peaking, low_shelf, high_shelf}；
/// peaking 的 q ∈ [0.1, 10]，shelf 的 q（RBJ slope S）∈ [0.1, 1.0]。
pub fn local_validate(
    doc: &ChannelMap,
    presets: &LayoutPresets,
    probe: &ProbeChannels,
    mode_sample_rate: u32,
    base_dir: Option<&std::path::Path>,
    fir_files: &[FirFileEntry],
) -> ValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut channel_mismatch = false;
    let strict_hz = 48_000.0f64;

    if doc.schema != CHANNELMAP_SCHEMA {
        errors.push(format!("schema 必须是 {CHANNELMAP_SCHEMA}"));
    }
    if doc.version != 2 {
        errors.push("version 必须是 2".into());
    }
    if doc.profile.name.trim().is_empty() {
        errors.push("profile.name 必须是非空字符串".into());
    }
    let input_def = doc
        .input
        .presets
        .get(&doc.input.layout)
        .or_else(|| presets.input_layouts.get(&doc.input.layout));
    let Some(input_def) = input_def else {
        errors.push(format!("input.layout 未知输入布局：{}", doc.input.layout));
        return ValidationReport {
            valid: false,
            errors,
            warnings,
            channel_mismatch: true,
        };
    };
    let cap = probe
        .capture
        .or(doc.input.channels.fixed())
        .or_else(|| input_def.channels.as_ref().and_then(ChannelCount::fixed))
        .or_else(|| {
            if input_def.streams.is_empty() {
                None
            } else {
                Some(input_def.streams.iter().sum())
            }
        });
    let Some(cap) = cap else {
        errors.push("input.channels=auto 未解析：需 probe 或 WebUI 选择输入通道数".into());
        channel_mismatch = true;
        return ValidationReport {
            valid: false,
            errors,
            warnings,
            channel_mismatch,
        };
    };
    if !(1..=64).contains(&cap) {
        errors.push("input.channels 必须在 1..64".into());
    }
    let mut in_role_ch = BTreeMap::new();
    for (i, r) in input_def.roles.iter().enumerate() {
        let pth = format!("input.layouts.{}.roles[{i}]", doc.input.layout);
        if r.id.trim().is_empty() {
            errors.push(format!("{pth}.id 必须非空"));
            continue;
        }
        if in_role_ch.contains_key(&r.id) {
            errors.push(format!("{pth}.id 重复：{}", r.id));
            continue;
        }
        let idx = if input_def.streams.is_empty() {
            r.channel
        } else {
            let st = r.stream as usize;
            if st >= input_def.streams.len() {
                errors.push(format!("{pth}.stream {} >= streams 数", r.stream));
                continue;
            }
            if r.channel >= input_def.streams[st] {
                errors.push(format!(
                    "{pth}.channel 必须 < flow{} 通道数 {}",
                    r.stream, input_def.streams[st]
                ));
                continue;
            }
            input_def.streams[..st].iter().sum::<u32>() + r.channel
        };
        if idx >= cap {
            channel_mismatch = true;
            errors.push(format!("{pth}.channel 全局通道 {idx} >= 输入通道数 {cap}"));
        } else {
            in_role_ch.insert(r.id.clone(), idx);
        }
    }

    // 输出
    let roles = effective_roles(doc, presets);
    if roles.is_empty() {
        errors.push("output.layout 未知或 output.roles 为空".into());
    }

    let pb = probe
        .playback
        .or(doc.output.channels.fixed())
        .or_else(|| output_layout_channels(doc, presets));
    let Some(pb) = pb else {
        errors.push("output.channels=auto 未解析：需 probe 或 WebUI 选择输出通道数".into());
        channel_mismatch = true;
        return ValidationReport {
            valid: false,
            errors,
            warnings,
            channel_mismatch,
        };
    };
    if let Some(declared) = doc.output.channels.fixed() {
        if declared != pb && probe.playback.is_some() {
            warnings.push(format!("profile 声明输出 {declared}ch，probe 实测 {pb}ch"));
        }
    }
    let mut used_out: BTreeMap<u32, String> = BTreeMap::new();
    let mut role_ids = BTreeMap::new();
    for (i, role) in roles.iter().enumerate() {
        let pth = format!("output.roles[{i}]");
        if role.id.trim().is_empty() {
            errors.push(format!("{pth}.id 必须非空"));
            continue;
        }
        if role_ids.insert(role.id.clone(), ()).is_some() {
            errors.push(format!("{pth}.id 重复：{}", role.id));
            continue;
        }
        num_range(
            role.gain_db,
            -60.0,
            12.0,
            &format!("{pth}.gain_db"),
            &mut errors,
        );
        num_range(
            role.delay_ms,
            0.0,
            200.0,
            &format!("{pth}.delay_ms"),
            &mut errors,
        );
        if role.polarity != "normal" && role.polarity != "invert" {
            errors.push(format!("{pth}.polarity 必须是 normal|invert"));
        }
        if role.enabled {
            if role.outputs.is_empty() {
                errors.push(format!("{pth}.outputs enabled 角色至少一个输出通道"));
            }
            for o in &role.outputs {
                if *o >= pb {
                    channel_mismatch = true;
                    errors.push(format!(
                        "{pth}.outputs 输出通道 {o} >= 播放通道数 {pb}（设备不够）"
                    ));
                } else if let Some(other) = used_out.get(o) {
                    errors.push(format!("{pth}.outputs 输出通道 {o} 与 {other} 冲突"));
                } else {
                    used_out.insert(*o, role.id.clone());
                }
            }
        }
        validate_xo(
            &role.highpass,
            &format!("{pth}.highpass"),
            strict_hz,
            &mut errors,
        );
        validate_xo(
            &role.lowpass,
            &format!("{pth}.lowpass"),
            strict_hz,
            &mut errors,
        );
        if role.peq.len() > 32 {
            errors.push(format!("{pth}.peq 最多 32 条"));
        }
        for (j, p) in role.peq.iter().enumerate() {
            let pp = format!("{pth}.peq[{j}]");
            if !(20.0..strict_hz / 2.0).contains(&p.freq) {
                errors.push(format!("{pp}.freq 必须在 [20, {strict_hz}/2)"));
            }
            let ty = if p.filter_type.is_empty() {
                "peaking"
            } else {
                p.filter_type.as_str()
            };
            if !matches!(ty, "peaking" | "low_shelf" | "high_shelf") {
                errors.push(format!("{pp}.type 只能是 peaking|low_shelf|high_shelf"));
            }
            // 与板端 tspi-channelmap（唯一权威）一致（裁决 5）：
            // peaking 的 q ∈ [0.1, 10]；shelf 的 q 是 RBJ slope S，必须 ∈ [0.1, 1.0]
            // （CamillaDSP slope=12*S，check_slope 拒绝 slope>12）。
            let shelf = matches!(ty, "low_shelf" | "high_shelf");
            if shelf {
                if !p.q.is_finite() || !(0.1..=1.0).contains(&p.q) {
                    errors.push(format!(
                        "{pp}.q shelf 的 q 是 RBJ slope S，必须在 [0.1,1.0]（CamillaDSP slope=12q 上限 12 dB/oct）"
                    ));
                }
            } else if !p.q.is_finite() || !(0.1..=10.0).contains(&p.q) {
                errors.push(format!(
                    "{pp}.q 必须在 [0.1, 10]（与板端 tspi-channelmap 一致）"
                ));
            }
            if !(-24.0..=24.0).contains(&p.gain_db) {
                errors.push(format!("{pp}.gain_db 必须在 [-24, 24]"));
            }
        }
        for (j, s) in role.sources.iter().enumerate() {
            let sp = format!("{pth}.sources[{j}]");
            if !in_role_ch.contains_key(&s.source) {
                errors.push(format!("{sp}.source 未知输入源 {}", s.source));
            }
            num_range(
                s.gain_db,
                -60.0,
                12.0,
                &format!("{sp}.gain_db"),
                &mut errors,
            );
        }
        if let Some(fir) = &role.fir {
            let path = fir.path();
            if path.trim().is_empty() {
                errors.push(format!("{pth}.fir.path 必须非空"));
            } else {
                let resolved = if std::path::Path::new(path).is_absolute() {
                    std::path::PathBuf::from(path)
                } else if let Some(base) = base_dir {
                    base.join(path)
                } else {
                    std::path::PathBuf::from(path)
                };
                let known = fir_files
                    .iter()
                    .any(|f| f.name == *path || f.path == resolved.display().to_string());
                if !known && !resolved.exists() {
                    warnings.push(format!("{pth}.fir 文件当前不存在：{}", resolved.display()));
                }
                let actual = fir.samplerate().or_else(|| {
                    fir_files
                        .iter()
                        .find(|f| f.name == *path || f.path == resolved.display().to_string())
                        .and_then(|f| f.sample_rate)
                });
                if let Some(rate) = actual {
                    if rate != mode_sample_rate {
                        errors.push(format!(
                            "{pth}.fir 采样率 {rate} != 当前模式 {mode_sample_rate}（按模式分 profile）"
                        ));
                    }
                }
            }
        }
        if let Some(link) = &role.link_to {
            if link == &role.id {
                errors.push(format!("{pth}.link_to 不能指向自己"));
            } else if !roles.iter().any(|r| &r.id == link) {
                errors.push(format!("{pth}.link_to 未知角色 {link}"));
            }
        }
    }

    // remap 与 sources 不能同时非空
    let has_explicit_sources = roles.iter().any(|r| !r.sources.is_empty());
    let has_routes = !doc.remap.routes.is_empty() || doc.remap.preset.is_some();
    if has_explicit_sources && has_routes {
        errors.push(
            "remap.routes/preset 与 roles[].sources 不能同时存在（sources 是最终矩阵）".into(),
        );
    }
    if let Some(preset) = &doc.remap.preset {
        if !presets.remap_presets.contains_key(preset) {
            errors.push(format!("remap.preset 未知：{preset}"));
        }
    }
    for (i, r) in doc.remap.routes.iter().enumerate() {
        let pth = format!("remap.routes[{i}]");
        if !["direct", "fold", "duplicate", "mix", "drop", "silence"].contains(&r.mode.as_str()) {
            errors.push(format!("{pth}.mode 非法：{}", r.mode));
        }
        match r.mode.as_str() {
            "drop" => {
                if r.src
                    .as_ref()
                    .map(|s| !in_role_ch.contains_key(s))
                    .unwrap_or(true)
                {
                    errors.push(format!("{pth}.src 必须是已知输入角色"));
                }
            }
            "silence" => {
                if r.dest
                    .as_ref()
                    .map(|d| !role_ids.contains_key(d))
                    .unwrap_or(true)
                {
                    errors.push(format!("{pth}.dest 必须是已知输出角色"));
                }
            }
            _ => {
                if r.src
                    .as_ref()
                    .map(|s| !in_role_ch.contains_key(s))
                    .unwrap_or(true)
                {
                    errors.push(format!("{pth}.src 必须是已知输入角色"));
                }
                if r.dest
                    .as_ref()
                    .map(|d| !role_ids.contains_key(d))
                    .unwrap_or(true)
                {
                    errors.push(format!("{pth}.dest 必须是已知输出角色"));
                }
            }
        }
    }
    if has_explicit_sources {
        for role in roles.iter().filter(|r| r.enabled && !r.mute) {
            if role.sources.is_empty() {
                warnings.push(format!("输出角色 {} 无输入源（将静音）", role.id));
            }
        }
    }

    // bass_management
    let bm = &doc.bass_management;
    if bm.enabled {
        for list in [&bm.lfe_to, &bm.redirect.sources, &bm.redirect.targets] {
            for x in list {
                if !role_ids.contains_key(x) {
                    errors.push(format!("bass_management 引用未知输出角色 {x}"));
                }
            }
        }
        num_range(
            bm.lfe_gain_db,
            -60.0,
            12.0,
            "bass_management.lfe_gain_db",
            &mut errors,
        );
        num_range(
            bm.sub_gain_db,
            -60.0,
            12.0,
            "bass_management.sub_gain_db",
            &mut errors,
        );
        num_range(
            bm.mains_highpass_hz,
            20.0,
            strict_hz / 2.0,
            "bass_management.mains_highpass_hz",
            &mut errors,
        );
        num_range(
            bm.sub_lowpass_hz,
            20.0,
            strict_hz / 2.0,
            "bass_management.sub_lowpass_hz",
            &mut errors,
        );
        for order in [bm.mains_highpass_order, bm.sub_lowpass_order] {
            if ![2, 4, 6, 8].contains(&order) {
                errors.push("bass_management 阶数必须是 2/4/6/8".into());
            }
        }
        for rid in bm.redirect.targets.iter().chain(bm.lfe_to.iter()) {
            if let Some(role) = roles.iter().find(|r| &r.id == rid) {
                if !role.sources.is_empty() {
                    errors.push(format!(
                        "bass_management enabled 时 {rid}.sources 必须为空（由渲染器自动生成）"
                    ));
                }
            }
        }
    }
    ValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings,
        channel_mismatch,
    }
}

// ===== 文件/状态辅助 =====

#[derive(Debug, Clone, Serialize, Default)]
pub struct ProfileEntry {
    pub name: String,
    pub path: String,
    pub ok: bool,
    pub profile_name: Option<String>,
    pub description: Option<String>,
    pub input_layout: Option<String>,
    pub output_layout: Option<String>,
    pub errors: usize,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FirFileEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified_unix: Option<u64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bits: Option<u32>,
    pub taps: Option<u32>,
    pub group_delay_ms: Option<f64>,
    pub level: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SaveOutcome {
    pub path: String,
    pub backup: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApplyOutcome {
    pub saved: bool,
    pub applied: bool,
    pub rolled_back: bool,
    pub saved_only: bool,
    pub via: String,
    pub message: String,
    pub backup: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputRoleView {
    pub id: String,
    pub label: String,
    pub channel: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelMapView {
    pub enabled: bool,
    pub needs_setup: bool,
    pub path: String,
    pub presets_file: String,
    pub profiles_dir: String,
    pub fir_dir: String,
    pub render_file: String,
    pub document: Option<ChannelMap>,
    pub effective_roles: Vec<RoleDef>,
    pub input_roles: Vec<InputRoleView>,
    pub validation: ValidationReport,
    pub presets: LayoutPresets,
    pub probe: ProbeInfo,
    pub profiles: Vec<ProfileEntry>,
    pub fir_files: Vec<FirFileEntry>,
    pub mode_off: bool,
    pub mock: bool,
}

/// WebUI 输入内容模式视图（运行态文件只读 + 覆盖文件）。
#[derive(Debug, Clone, Serialize, Default)]
pub struct InputModeView {
    pub state_available: bool,
    pub daemon_running: bool,
    pub content_mode: String,
    pub detected: String,
    #[serde(rename = "override")]
    pub override_mode: Option<String>,
    pub confidence: f64,
    pub active_channels: Vec<String>,
    pub rms_db: BTreeMap<String, f64>,
    pub threshold_db: Option<f64>,
    pub hold_ms: Option<u32>,
    pub reason: Option<String>,
    pub h16_internal_upmix: Option<bool>,
    pub container_layout: Option<String>,
    pub detector: Option<String>,
    pub updated_unix: Option<u64>,
    pub state_path: String,
    pub mode_path: String,
    pub notes: Vec<String>,
}

/// 归一化输入内容模式：auto|stereo|multichannel，兼容 force-stereo/force-multichannel。
pub fn normalize_input_mode(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Some("auto"),
        "stereo" | "force-stereo" | "2.0" | "2ch" => Some("stereo"),
        "multichannel" | "force-multichannel" | "multi" | "mc" => Some("multichannel"),
        _ => None,
    }
}

fn parse_input_override(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let raw = v.get("mode").and_then(|x| x.as_str())?;
    normalize_input_mode(raw).map(|s| s.to_string())
}

/// 合并运行态与覆盖：override > detector > auto（auto 默认直通）。
pub fn build_input_mode_view(
    state: Option<&serde_json::Value>,
    override_mode: Option<String>,
    state_path: &str,
    mode_path: &str,
    now: u64,
) -> InputModeView {
    let mut view = InputModeView {
        state_available: state.is_some(),
        content_mode: "auto".into(),
        detected: "unknown".into(),
        override_mode: override_mode.clone(),
        state_path: state_path.into(),
        mode_path: mode_path.into(),
        ..Default::default()
    };
    let mut state_mode: Option<String> = None;
    if let Some(s) = state {
        view.detected = match s.get("detected").and_then(|x| x.as_str()) {
            Some("stereo") => "stereo".into(),
            Some("multichannel") => "multichannel".into(),
            _ => "unknown".into(),
        };
        state_mode = s
            .get("content_mode")
            .and_then(|x| x.as_str())
            .and_then(normalize_input_mode)
            .map(|s| s.to_string());
        view.confidence = s.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0);
        view.active_channels = s
            .get("active_channels")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(obj) = s.get("rms_db").and_then(|x| x.as_object()) {
            for (k, v) in obj {
                if let Some(f) = v.as_f64() {
                    view.rms_db.insert(k.clone(), f);
                }
            }
        }
        view.threshold_db = s.get("threshold_db").and_then(|x| x.as_f64());
        view.hold_ms = s.get("hold_ms").and_then(|x| x.as_u64()).map(|v| v as u32);
        view.reason = s.get("reason").and_then(|x| x.as_str()).map(str::to_string);
        view.h16_internal_upmix = s.get("h16_internal_upmix").and_then(|x| x.as_bool());
        view.container_layout = s
            .get("container_layout")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        view.detector = s
            .get("detector")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        view.updated_unix = s.get("updated_unix").and_then(|x| x.as_u64());
        view.daemon_running = view
            .updated_unix
            .map(|t| now.saturating_sub(t) <= 10)
            .unwrap_or(false);
    }
    let ov = override_mode.as_deref();
    view.content_mode = match ov {
        Some("stereo") => "stereo".into(),
        Some("multichannel") => "multichannel".into(),
        _ => {
            if let Some(m) = state_mode {
                if m == "auto" {
                    if view.detected == "stereo" {
                        "stereo".into()
                    } else if view.state_available {
                        "multichannel".into()
                    } else {
                        "auto".into()
                    }
                } else {
                    m
                }
            } else if view.detected == "stereo" {
                "stereo".into()
            } else if view.state_available {
                "multichannel".into()
            } else {
                "auto".into()
            }
        }
    };
    // 未运行/无状态时 reason 明确给 daemon-not-running（便于 UI 显示）
    if view.reason.is_none() && !view.state_available {
        view.reason = Some("daemon-not-running".into());
    }
    if !view.state_available {
        view.notes.push(
            "未检测到 tspi-upmix 输入内容检测器（daemon 未运行）：content_mode 按覆盖值/auto，auto 默认直通"
                .into(),
        );
    }
    if view.h16_internal_upmix == Some(true) || view.detected == "multichannel" {
        view.notes
            .push("H16 已内部上混或多声道内容：板端直通，不再二次上混（防双上混）".into());
    } else if view.detected == "stereo" {
        view.notes
            .push("检测到 2.0 内容：按上混路径处理（需 upmix 矩阵 profile）".into());
    }
    if let Some(m) = ov.filter(|m| *m != "auto") {
        view.notes.push(format!("已手动强制为 {m}（覆盖检测器）"));
    }
    view
}

struct MockStore {
    doc: Option<ChannelMap>,
    profiles: BTreeMap<String, ChannelMap>,
    fir: BTreeMap<String, (Vec<u8>, FirFileEntry)>,
    probe: ProbeInfo,
    apply_fails: bool,
    applied: Vec<String>,
    input_override: Option<String>,
}

pub struct ChannelMapService {
    pub cfg: ChannelmapConfig,
    runner: Arc<dyn CommandRunner>,
    pub mock: bool,
    mock_store: std::sync::Mutex<Option<MockStore>>,
}

impl ChannelMapService {
    pub fn new(cfg: ChannelmapConfig, runner: Arc<dyn CommandRunner>, mock: bool) -> Self {
        let store = if mock {
            let probe = ProbeInfo {
                ok: true,
                playback: Some(ProbeDevice {
                    device: "plughw:1,0".into(),
                    channels_min: Some(22),
                    channels_max: Some(22),
                    formats: vec!["S32_LE".into()],
                    rates: vec![48000, 96000],
                    notes: Vec::new(),
                }),
                capture: Some(ProbeDevice {
                    device: "plughw:Loopback,1,0".into(),
                    channels_min: Some(16),
                    channels_max: Some(16),
                    formats: vec!["S32_LE".into()],
                    rates: vec![48000],
                    notes: Vec::new(),
                }),
                cards: vec![
                    "0 [PCH]: HDA Intel PCH".into(),
                    "1 [UltraLite]: MOTU UltraLite-mk5".into(),
                ],
                notes: Vec::new(),
                needs_user_choice: false,
            };
            Some(MockStore {
                doc: None,
                profiles: BTreeMap::new(),
                fir: BTreeMap::new(),
                probe,
                apply_fails: false,
                applied: Vec::new(),
                input_override: None,
            })
        } else {
            None
        };
        Self {
            cfg,
            runner,
            mock,
            mock_store: std::sync::Mutex::new(store),
        }
    }

    pub fn with_mock_doc(self, doc: ChannelMap) -> Self {
        if let Some(store) = self.mock_store.lock().unwrap().as_mut() {
            store.doc = Some(doc);
        }
        self
    }

    pub fn set_mock_apply_fail(&self, fail: bool) {
        if let Some(store) = self.mock_store.lock().unwrap().as_mut() {
            store.apply_fails = fail;
        }
    }

    /// 布局预设：优先读配置的 layout-presets.yml；失败回退固件内置模板（编译期内嵌）。
    pub async fn load_presets(&self) -> LayoutPresets {
        if !self.mock {
            if let Ok(text) = tokio::fs::read_to_string(&self.cfg.presets_file).await {
                if let Ok(p) = LayoutPresets::parse(&text) {
                    if !p.input_layouts.is_empty() || !p.output_layouts.is_empty() {
                        return p;
                    }
                }
            }
        }
        LayoutPresets::parse(EMBEDDED_LAYOUT_PRESETS).unwrap_or_default()
    }

    async fn cli_available(&self) -> bool {
        !self.mock
            && tokio::fs::metadata(&self.cfg.cli_bin)
                .await
                .map(|m| m.is_file())
                .unwrap_or(false)
    }

    async fn run_cli(&self, args: &[&str]) -> Option<CommandOutput> {
        self.runner
            .run_with_timeout(&self.cfg.cli_bin, args, Duration::from_secs(180))
            .await
            .ok()
    }

    /// probe：优先 tspi-channelmap probe --json；失败回退 ALSA stream0。
    pub async fn probe(&self, alsa: &dyn AlsaBackend, doc: Option<&ChannelMap>) -> ProbeInfo {
        if self.mock {
            let store = self.mock_store.lock().unwrap();
            return store.as_ref().unwrap().probe.clone();
        }
        if self.cli_available().await {
            let args: Vec<&str> = vec!["probe", "--json"];
            if let Some(out) = self.run_cli(&args).await {
                if out.success {
                    if let Some(p) = parse_probe_json(&out.stdout) {
                        return p;
                    }
                }
            }
        }
        // 回退：ALSA 实测（stream0 通道数）
        let view: AlsaView = alsa.view().await;
        let mut probe = ProbeInfo::default();
        if let Some(pcm) = view
            .pcm
            .iter()
            .find(|p| p.pcm_type == "playback" && p.channels.unwrap_or(0) > 0)
        {
            let ch = pcm.channels.unwrap_or(0);
            probe.playback = Some(ProbeDevice {
                device: doc
                    .map(|d| d.output.device.clone())
                    .filter(|d| d != "auto" && !d.is_empty())
                    .unwrap_or_else(|| format!("plughw:{},{}", pcm.card, pcm.device)),
                channels_min: Some(ch),
                channels_max: Some(ch),
                formats: Vec::new(),
                rates: Vec::new(),
                notes: Vec::new(),
            });
            probe.ok = true;
        }
        if let Some(pcm) = view
            .pcm
            .iter()
            .find(|p| p.pcm_type == "capture" && p.channels.unwrap_or(0) > 0)
        {
            let ch = pcm.channels.unwrap_or(0);
            probe.capture = Some(ProbeDevice {
                device: format!("plughw:{},{}", pcm.card, pcm.device),
                channels_min: Some(ch),
                channels_max: Some(ch),
                formats: Vec::new(),
                rates: Vec::new(),
                notes: Vec::new(),
            });
            probe.ok = true;
        }
        probe.cards = view
            .cards
            .iter()
            .map(|c| format!("{} [{}]: {}", c.index, c.id, c.name))
            .collect();
        probe.notes = view.warnings;
        probe.needs_user_choice = probe.playback.is_none() || probe.capture.is_none();
        probe
    }

    pub async fn load(&self) -> AppResult<Option<ChannelMap>> {
        if self.mock {
            let store = self.mock_store.lock().unwrap();
            return Ok(store.as_ref().unwrap().doc.clone());
        }
        let path = std::path::Path::new(&self.cfg.config_file);
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Ok(None);
        }
        let (text, _) = util::read_text_limited(path, 4 * 1024 * 1024)
            .await
            .map_err(|e| AppError::internal(format!("读取 {} 失败: {e}", path.display())))?;
        let doc = parse_document(&text)?;
        Ok(Some(doc))
    }

    pub async fn save(&self, doc: &ChannelMap) -> AppResult<SaveOutcome> {
        let mut doc = doc.clone();
        let presets = self.load_presets().await;
        materialize_document(&mut doc, &presets);
        let yaml = serde_yaml::to_string(&doc)
            .map_err(|e| AppError::internal(format!("序列化 channelmap 失败: {e}")))?;
        if self.mock {
            let mut store = self.mock_store.lock().unwrap();
            let store = store.as_mut().unwrap();
            store.doc = Some(doc);
            return Ok(SaveOutcome {
                path: format!("mock:{}", self.cfg.config_file),
                backup: None,
                profile: None,
            });
        }
        let path = std::path::Path::new(&self.cfg.config_file);
        let backup = util::backup_file(path)
            .await
            .map_err(|e| AppError::internal(format!("备份失败: {e}")))?
            .map(|p| p.display().to_string());
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::internal(format!("创建目录失败: {e}")))?;
        }
        util::write_atomic(path, &yaml)
            .await
            .map_err(|e| AppError::internal(format!("写入 {} 失败: {e}", path.display())))?;
        util::prune_backups(path, 10).await;
        Ok(SaveOutcome {
            path: path.display().to_string(),
            backup,
            profile: None,
        })
    }

    /// 校验：板端 `tspi-channelmap validate` 是唯一权威（2026-11-16 裁决 3）。
    ///
    /// - mock（测试/演示，`TSPI_MOCK=1`）：用 `local_validate`，规则与板端 Python 对齐
    ///   （`peq.type` 白名单、peaking q ∈ [0.1, 10]、shelf q ∈ [0.1, 1.0]）；
    /// - 真机：必须委托 CLI；CLI 不存在/不可执行/输出无法解析时返回明确错误，
    ///   **不得**回退本地规则放行（否则会出现「Web 说合法、板端 apply 失败」）。
    pub async fn validate(
        &self,
        doc: &ChannelMap,
        probe: &ProbeInfo,
        mode_sample_rate: u32,
    ) -> AppResult<ValidationReport> {
        if self.mock {
            let presets = self.load_presets().await;
            let fir = self.fir_files().await;
            return Ok(local_validate(
                doc,
                &presets,
                &ProbeChannels {
                    capture: probe.capture_channels(),
                    playback: probe.playback_channels(),
                },
                mode_sample_rate,
                std::path::Path::new(&self.cfg.config_file).parent(),
                &fir,
            ));
        }
        if !self.cli_available().await {
            return Err(AppError::unavailable(format!(
                "tspi-channelmap 不可用，无法校验（{} 不存在或不可执行）",
                self.cfg.cli_bin
            )));
        }
        let path = std::env::temp_dir().join(format!(
            "tspi-channelmap-{}-{}.yml",
            std::process::id(),
            util::now_unix()
        ));
        let yaml = serde_yaml::to_string(doc)
            .map_err(|e| AppError::internal(format!("序列化失败: {e}")))?;
        tokio::fs::write(&path, &yaml)
            .await
            .map_err(|e| AppError::internal(format!("写临时文件失败: {e}")))?;
        let path_s = path.to_string_lossy().to_string();
        let rate_s = mode_sample_rate.to_string();
        let mut args: Vec<&str> = vec![
            "validate",
            &path_s,
            "--json",
            "--samplerate",
            &rate_s,
            "--presets",
            &self.cfg.presets_file,
        ];
        let cap_s = probe.capture_channels().map(|v| v.to_string());
        let pb_s = probe.playback_channels().map(|v| v.to_string());
        if let Some(c) = &cap_s {
            args.push("--capture-channels");
            args.push(c);
        }
        if let Some(p) = &pb_s {
            args.push("--playback-channels");
            args.push(p);
        }
        let out = self.run_cli(&args).await;
        let _ = tokio::fs::remove_file(&path).await;
        let out = out.ok_or_else(|| {
            AppError::unavailable(format!(
                "tspi-channelmap 不可用，无法校验（执行 {} 失败）",
                self.cfg.cli_bin
            ))
        })?;
        parse_validate_output(&out).ok_or_else(|| {
            let detail = out
                .stderr
                .lines()
                .chain(out.stdout.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("无输出");
            AppError::unavailable(format!(
                "tspi-channelmap 校验输出无法解析（exit={:?}）：{detail}",
                out.status
            ))
        })
    }

    pub async fn profiles(&self) -> Vec<ProfileEntry> {
        if self.mock {
            let store = self.mock_store.lock().unwrap();
            let store = store.as_ref().unwrap();
            return store
                .profiles
                .iter()
                .map(|(name, doc)| ProfileEntry {
                    name: name.clone(),
                    path: format!("mock:profiles/{name}.yml"),
                    ok: true,
                    profile_name: Some(doc.profile.name.clone()),
                    description: Some(doc.profile.description.clone()),
                    input_layout: Some(doc.input.layout.clone()),
                    output_layout: Some(doc.output.layout.clone()),
                    errors: 0,
                    active: false,
                })
                .collect();
        }
        let dir = std::path::Path::new(&self.cfg.profiles_dir);
        let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
            return Vec::new();
        };
        let meaning = |path: &std::path::Path| -> Option<ChannelMap> {
            let text = std::fs::read_to_string(path).ok()?;
            serde_yaml::from_str::<ChannelMap>(&text).ok()
        };
        let mut out = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".yml") && !name.ends_with(".yaml") {
                continue;
            }
            let path = entry.path();
            let doc = meaning(&path);
            let meta = entry.metadata().await.ok();
            out.push(ProfileEntry {
                name: name
                    .trim_end_matches(".yml")
                    .trim_end_matches(".yaml")
                    .to_string(),
                path: path.display().to_string(),
                ok: doc.is_some(),
                profile_name: doc.as_ref().map(|d| d.profile.name.clone()),
                description: doc.as_ref().map(|d| d.profile.description.clone()),
                input_layout: doc.as_ref().map(|d| d.input.layout.clone()),
                output_layout: doc.as_ref().map(|d| d.output.layout.clone()),
                errors: 0,
                active: false,
            });
            let _ = meta;
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// 输入内容模式：读 /run/tspi/input-content.json + /etc/tspi-audio/input-mode.json。
    pub async fn input_mode(&self) -> InputModeView {
        let state_path = &self.cfg.input_state_file;
        let mode_path = &self.cfg.input_mode_file;
        if self.mock {
            let store = self.mock_store.lock().unwrap();
            let override_mode = store.as_ref().and_then(|s| s.input_override.clone());
            return build_input_mode_view(
                None,
                override_mode,
                state_path,
                mode_path,
                util::now_unix(),
            );
        }
        let override_mode = tokio::fs::read_to_string(mode_path)
            .await
            .ok()
            .and_then(|t| parse_input_override(&t));
        let state = tokio::fs::read_to_string(state_path)
            .await
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        build_input_mode_view(
            state.as_ref(),
            override_mode,
            state_path,
            mode_path,
            util::now_unix(),
        )
    }

    /// 写覆盖模式：mock 内存；真机优先 tspi-upmix input-mode set，失败/不存在则直接写覆盖文件。
    pub async fn set_input_mode(&self, raw: &str) -> AppResult<InputModeView> {
        let mode = normalize_input_mode(raw).ok_or_else(|| {
            AppError::bad_request("mode 必须是 auto|stereo|multichannel（兼容 force-* 别名）")
        })?;
        if self.mock {
            if let Some(store) = self.mock_store.lock().unwrap().as_mut() {
                store.input_override = Some(mode.to_string());
            }
            return Ok(self.input_mode().await);
        }
        let cli = &self.cfg.upmix_bin;
        if !cli.trim().is_empty() && tokio::fs::metadata(cli).await.is_ok() {
            if let Ok(out) = self
                .runner
                .run_with_timeout(cli, &["input-mode", "set", mode], Duration::from_secs(20))
                .await
            {
                if out.success {
                    return Ok(self.input_mode().await);
                }
            }
        }
        let path = std::path::Path::new(&self.cfg.input_mode_file);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::internal(format!("创建目录失败: {e}")))?;
        }
        let payload = serde_json::json!({
            "schema": "tspi-input-mode/v1",
            "mode": mode,
            "updated_unix": util::now_unix(),
            "updated_by": "webui",
        });
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|e| AppError::internal(format!("序列化失败: {e}")))?;
        util::write_atomic(path, &format!("{text}\n"))
            .await
            .map_err(|e| AppError::internal(format!("写入 {} 失败: {e}", path.display())))?;
        Ok(self.input_mode().await)
    }

    pub async fn save_profile(&self, name: &str, doc: &ChannelMap) -> AppResult<ProfileEntry> {
        validate_profile_name(name)?;
        if self.mock {
            let mut store = self.mock_store.lock().unwrap();
            let store = store.as_mut().unwrap();
            store.profiles.insert(name.to_string(), doc.clone());
            return Ok(ProfileEntry {
                name: name.to_string(),
                path: format!("mock:profiles/{name}.yml"),
                ok: true,
                profile_name: Some(doc.profile.name.clone()),
                description: Some(doc.profile.description.clone()),
                input_layout: Some(doc.input.layout.clone()),
                output_layout: Some(doc.output.layout.clone()),
                errors: 0,
                active: false,
            });
        }
        let mut doc = doc.clone();
        let presets = self.load_presets().await;
        materialize_document(&mut doc, &presets);
        let yaml = serde_yaml::to_string(&doc)
            .map_err(|e| AppError::internal(format!("序列化 profile 失败: {e}")))?;
        let dir = std::path::Path::new(&self.cfg.profiles_dir);
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| AppError::internal(format!("创建 profiles 目录失败: {e}")))?;
        let path = dir.join(format!("{name}.yml"));
        util::write_atomic(&path, &yaml)
            .await
            .map_err(|e| AppError::internal(format!("写入 profile 失败: {e}")))?;
        Ok(ProfileEntry {
            name: name.to_string(),
            path: path.display().to_string(),
            ok: true,
            profile_name: Some(doc.profile.name.clone()),
            description: Some(doc.profile.description.clone()),
            input_layout: Some(doc.input.layout.clone()),
            output_layout: Some(doc.output.layout.clone()),
            errors: 0,
            active: false,
        })
    }

    pub async fn load_profile(&self, name: &str) -> AppResult<ChannelMap> {
        validate_profile_name(name)?;
        let doc = if self.mock {
            let store = self.mock_store.lock().unwrap();
            store
                .as_ref()
                .unwrap()
                .profiles
                .get(name)
                .cloned()
                .ok_or_else(|| AppError::not_found(format!("profile {name} 不存在")))?
        } else {
            let path = std::path::Path::new(&self.cfg.profiles_dir).join(format!("{name}.yml"));
            let text = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| AppError::not_found(format!("读取 {} 失败: {e}", path.display())))?;
            parse_document(&text)?
        };
        self.save(&doc).await?;
        Ok(doc)
    }

    pub async fn delete_profile(&self, name: &str) -> AppResult<()> {
        validate_profile_name(name)?;
        if self.mock {
            let mut store = self.mock_store.lock().unwrap();
            let store = store.as_mut().unwrap();
            return store
                .profiles
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| AppError::not_found(format!("profile {name} 不存在")));
        }
        let path = std::path::Path::new(&self.cfg.profiles_dir).join(format!("{name}.yml"));
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| AppError::not_found(format!("删除失败: {e}")))
    }

    pub async fn fir_files(&self) -> Vec<FirFileEntry> {
        if self.mock {
            let store = self.mock_store.lock().unwrap();
            return store
                .as_ref()
                .unwrap()
                .fir
                .values()
                .map(|(_, e)| e.clone())
                .collect();
        }
        let dir = std::path::Path::new(&self.cfg.fir_dir);
        let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(bytes) = tokio::fs::read(&path).await else {
                continue;
            };
            let meta = entry.metadata().await.ok();
            let (sample_rate, channels, bits, error) = analyze_fir(&bytes);
            let (taps, group_delay_ms, level) =
                fir_metrics(&name, &bytes, sample_rate, channels, bits);
            out.push(FirFileEntry {
                name,
                path: path.display().to_string(),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(bytes.len() as u64),
                modified_unix: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
                sample_rate,
                channels,
                bits,
                taps,
                group_delay_ms,
                level,
                error,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub async fn save_fir(&self, name: &str, bytes: &[u8]) -> AppResult<FirFileEntry> {
        let safe = sanitize_file_name(name)?;
        if bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
            return Err(AppError::bad_request("FIR 文件为空或超过 16 MiB"));
        }
        let lower = safe.to_ascii_lowercase();
        if ![".wav", ".txt", ".raw", ".bin", ".fir", ".coe"]
            .iter()
            .any(|ext| lower.ends_with(ext))
        {
            return Err(AppError::bad_request(
                "FIR 类型不支持（wav/txt/raw/bin/fir/coe）",
            ));
        }
        let (sample_rate, channels, bits, error) = analyze_fir(bytes);
        let (taps, group_delay_ms, level) = fir_metrics(&safe, bytes, sample_rate, channels, bits);
        if self.mock {
            let mut store = self.mock_store.lock().unwrap();
            let store = store.as_mut().unwrap();
            let entry = FirFileEntry {
                name: safe.clone(),
                path: format!("mock:fir/{safe}"),
                size: bytes.len() as u64,
                modified_unix: Some(util::now_unix()),
                sample_rate,
                channels,
                bits,
                taps,
                group_delay_ms,
                level,
                error,
            };
            store
                .fir
                .insert(safe.clone(), (bytes.to_vec(), entry.clone()));
            return Ok(entry);
        }
        let dir = std::path::Path::new(&self.cfg.fir_dir);
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| AppError::internal(format!("创建 FIR 目录失败: {e}")))?;
        let path = dir.join(&safe);
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| AppError::internal(format!("写 FIR 失败: {e}")))?;
        Ok(FirFileEntry {
            name: safe,
            path: path.display().to_string(),
            size: bytes.len() as u64,
            modified_unix: Some(util::now_unix()),
            sample_rate,
            channels,
            bits,
            taps,
            group_delay_ms,
            level,
            error,
        })
    }

    /// 应用：保存 active 后调 tspi-channelmap apply（mode off 时只校验/渲染/持久化）。
    pub async fn apply(
        &self,
        mode: crate::config::RunMode,
        probe: &ProbeInfo,
    ) -> AppResult<ApplyOutcome> {
        let presets = self.load_presets().await;
        let Some(mut doc) = self.load().await? else {
            return Err(AppError::bad_request("没有 active channelmap"));
        };
        materialize_document(&mut doc, &presets);
        let save = self.save(&doc).await?;
        let mode_str = match mode {
            crate::config::RunMode::Dante => "dante",
            crate::config::RunMode::Aes67 => "aes67",
            crate::config::RunMode::Off => "off",
        };
        let fs = if mode == crate::config::RunMode::Dante {
            96_000
        } else {
            48_000
        };
        let off = mode == crate::config::RunMode::Off;
        if self.mock {
            let mut store = self.mock_store.lock().unwrap();
            let store = store.as_mut().unwrap();
            if store.apply_fails {
                return Err(AppError::unavailable(
                    "mock 注入：tspi-channelmap apply 失败（已回滚 last-good）",
                ));
            }
            store.applied.push(format!("apply@{}", util::now_unix()));
            return Ok(ApplyOutcome {
                saved: true,
                applied: !off,
                saved_only: off,
                via: "mock-tspi-channelmap".into(),
                message: if off {
                    "mock：已保存并渲染；模式 off，未启动 CamillaDSP".into()
                } else {
                    "mock：channelmap 已应用（校验→渲染→重启 CamillaDSP）".into()
                },
                backup: save.backup,
                ..Default::default()
            });
        }
        if !self.cli_available().await {
            return Err(AppError::unavailable(format!(
                "tspi-channelmap 不存在（{}），配置已保存但未应用",
                self.cfg.cli_bin
            )));
        }
        let fs_s = fs.to_string();
        let mut args: Vec<&str> = vec![
            "apply",
            "--file",
            &self.cfg.config_file,
            "--mode",
            mode_str,
            "--samplerate",
            &fs_s,
            "--presets",
            &self.cfg.presets_file,
        ];
        let cap_s = probe.capture_channels().map(|v| v.to_string());
        let pb_s = probe.playback_channels().map(|v| v.to_string());
        if let Some(c) = &cap_s {
            args.push("--capture-channels");
            args.push(c);
        }
        if let Some(p) = &pb_s {
            args.push("--playback-channels");
            args.push(p);
        }
        let out = self.run_cli(&args).await.ok_or_else(|| {
            AppError::unavailable(format!("执行 {} apply 失败", self.cfg.cli_bin))
        })?;
        let code = out.status.unwrap_or(-1);
        if !out.success {
            let meaning = match code {
                2 => "schema/用法错误",
                3 => "取值/校验失败",
                4 => "渲染/camilladsp --check 失败",
                5 => "apply/重启失败",
                9 => "失败并已回滚 last-good",
                _ => "应用失败",
            };
            let detail = out
                .stderr
                .trim()
                .lines()
                .next()
                .filter(|l| !l.is_empty())
                .unwrap_or("无错误输出");
            return Err(AppError::unavailable(format!(
                "tspi-channelmap apply 失败（退出码 {code}：{meaning}）：{detail}"
            )));
        }
        Ok(ApplyOutcome {
            saved: true,
            applied: !off,
            saved_only: off,
            via: "tspi-channelmap apply".into(),
            message: if off {
                "已保存并渲染到 /run/tspi/camilladsp.yml；模式 off，未启动 CamillaDSP".into()
            } else {
                "channelmap 已应用（校验→渲染→重启 CamillaDSP）".into()
            },
            backup: save.backup,
            exit_code: out.status,
            ..Default::default()
        })
    }

    /// 回滚 last-good。
    pub async fn rollback(&self, mode: crate::config::RunMode) -> AppResult<ApplyOutcome> {
        let mode_str = match mode {
            crate::config::RunMode::Dante => "dante",
            crate::config::RunMode::Aes67 => "aes67",
            crate::config::RunMode::Off => "off",
        };
        if self.mock {
            return Ok(ApplyOutcome {
                saved: true,
                applied: mode != crate::config::RunMode::Off,
                saved_only: mode == crate::config::RunMode::Off,
                rolled_back: true,
                via: "mock-tspi-channelmap".into(),
                message: "mock：已回滚 last-good（未执行真实命令）".into(),
                ..Default::default()
            });
        }
        if !self.cli_available().await {
            return Err(AppError::unavailable(format!(
                "tspi-channelmap 不存在（{}），无法 rollback",
                self.cfg.cli_bin
            )));
        }
        let out = self
            .run_cli(&[
                "rollback",
                "--mode",
                mode_str,
                "--presets",
                &self.cfg.presets_file,
            ])
            .await
            .ok_or_else(|| AppError::unavailable("执行 tspi-channelmap rollback 失败"))?;
        if !out.success {
            let code = out.status.unwrap_or(-1);
            return Err(AppError::unavailable(format!(
                "tspi-channelmap rollback 失败（退出码 {code}）：{}",
                out.stderr.trim().lines().next().unwrap_or("无错误输出")
            )));
        }
        Ok(ApplyOutcome {
            saved: true,
            applied: mode != crate::config::RunMode::Off,
            saved_only: mode == crate::config::RunMode::Off,
            rolled_back: true,
            via: "tspi-channelmap rollback".into(),
            message: "已回滚到 last-good".into(),
            exit_code: out.status,
            ..Default::default()
        })
    }

    /// 渲染配置文本：真机委托 `tspi-channelmap render`（唯一权威）；mock 读渲染文件做展示。
    ///
    /// CLI 不存在/不可执行/渲染失败时返回明确错误，不静默回退旧文件（2026-11-16 裁决 3）。
    pub async fn render_text(&self, probe: &ProbeInfo) -> AppResult<Option<String>> {
        if self.mock {
            return Ok(util::read_text_limited(
                std::path::Path::new(&self.cfg.render_file),
                2 * 1024 * 1024,
            )
            .await
            .ok()
            .map(|(t, _)| t));
        }
        if !self.cli_available().await {
            return Err(AppError::unavailable(format!(
                "tspi-channelmap 不可用，无法渲染（{} 不存在或不可执行）",
                self.cfg.cli_bin
            )));
        }
        let rate = 48_000u32.to_string();
        let mut args: Vec<&str> = vec![
            "render",
            &self.cfg.config_file,
            "--samplerate",
            &rate,
            "--presets",
            &self.cfg.presets_file,
        ];
        let cap_s = probe.capture_channels().map(|v| v.to_string());
        let pb_s = probe.playback_channels().map(|v| v.to_string());
        if let Some(c) = &cap_s {
            args.push("--capture-channels");
            args.push(c);
        }
        if let Some(p) = &pb_s {
            args.push("--playback-channels");
            args.push(p);
        }
        let out = self.run_cli(&args).await.ok_or_else(|| {
            AppError::unavailable(format!(
                "tspi-channelmap 不可用，无法渲染（执行 {} 失败）",
                self.cfg.cli_bin
            ))
        })?;
        if !out.success {
            let detail = out
                .stderr
                .lines()
                .chain(out.stdout.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("无输出");
            return Err(AppError::unavailable(format!(
                "tspi-channelmap render 失败（exit={:?}）：{detail}",
                out.status
            )));
        }
        if out.stdout.trim().is_empty() {
            return Err(AppError::unavailable(
                "tspi-channelmap render 无输出，无法展示渲染配置",
            ));
        }
        Ok(Some(out.stdout))
    }

    /// 当前 active YAML 文本（mock 由文档序列化）。
    pub async fn export_active_yaml(&self) -> AppResult<String> {
        if let Some(doc) = self.load().await? {
            return serde_yaml::to_string(&doc)
                .map_err(|e| AppError::internal(format!("序列化失败: {e}")));
        }
        Err(AppError::not_found("channelmap 尚未配置"))
    }

    /// 导出 profile YAML 文本。
    pub async fn export_profile_yaml(&self, name: &str) -> AppResult<String> {
        validate_profile_name(name)?;
        if self.mock {
            let store = self.mock_store.lock().unwrap();
            let doc = store
                .as_ref()
                .unwrap()
                .profiles
                .get(name)
                .ok_or_else(|| AppError::not_found(format!("profile {name} 不存在")))?;
            return serde_yaml::to_string(doc)
                .map_err(|e| AppError::internal(format!("序列化失败: {e}")));
        }
        let path = std::path::Path::new(&self.cfg.profiles_dir).join(format!("{name}.yml"));
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| AppError::not_found(format!("读取 {} 失败: {e}", path.display())))
    }
}

fn validate_profile_name(name: &str) -> AppResult<()> {
    let n = name.trim();
    if n.is_empty() || n.len() > 64 {
        return Err(AppError::bad_request("profile 名长度 1..64"));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(AppError::bad_request("profile 名只允许字母数字._-"));
    }
    Ok(())
}

fn sanitize_file_name(name: &str) -> AppResult<String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 128 {
        return Err(AppError::bad_request("文件名非法"));
    }
    if n.contains('/') || n.contains('\\') || n.contains("..") || n.starts_with('.') {
        return Err(AppError::bad_request("文件名不得含路径分隔符或 .."));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
    {
        return Err(AppError::bad_request("文件名含非法字符"));
    }
    Ok(n.to_string())
}

/// 解析 WAV 头（返回 samplerate/channels/bits/error）。
pub fn analyze_fir(bytes: &[u8]) -> (Option<u32>, Option<u32>, Option<u32>, Option<String>) {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return (None, None, None, None);
    }
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        if id == b"fmt " && pos + 8 + 16 <= bytes.len() {
            let b = &bytes[pos + 8..];
            return (
                Some(u32::from_le_bytes([b[4], b[5], b[6], b[7]])),
                Some(u16::from_le_bytes([b[2], b[3]]) as u32),
                Some(u16::from_le_bytes([b[14], b[15]]) as u32),
                None,
            );
        }
        pos = pos.saturating_add(8 + size + (size & 1));
    }
    (None, None, None, Some("无法解析 WAV fmt 块".into()))
}

pub fn fir_metrics(
    name: &str,
    bytes: &[u8],
    sample_rate: Option<u32>,
    channels: Option<u32>,
    bits: Option<u32>,
) -> (Option<u32>, Option<f64>, String) {
    let lower = name.to_ascii_lowercase();
    let mut taps = None;
    if lower.ends_with(".wav") && bytes.len() >= 12 && &bytes[0..4] == b"RIFF" {
        let mut pos = 12usize;
        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ]) as usize;
            if id == b"data" {
                let bps = bits.unwrap_or(16).max(8) as usize / 8;
                let ch = channels.unwrap_or(1).max(1) as usize;
                taps = Some((size / (bps * ch)).max(1) as u32);
                break;
            }
            pos = pos.saturating_add(8 + size + (size & 1));
        }
    } else if lower.ends_with(".txt") || lower.ends_with(".coe") || lower.ends_with(".fir") {
        taps = Some(
            bytes
                .split(|b| *b == 10u8)
                .filter(|line| !line.is_empty() && line.iter().any(|c| !c.is_ascii_whitespace()))
                .count()
                .max(1) as u32,
        );
    } else if lower.ends_with(".raw") || lower.ends_with(".bin") {
        taps = Some((bytes.len() / 4).max(1) as u32);
    }
    let group_delay_ms = match (taps, sample_rate) {
        (Some(t), Some(fs)) if fs > 0 => Some((t as f64 / 2.0) / fs as f64 * 1000.0),
        _ => None,
    };
    let level = match taps {
        Some(t) if t > 8192 => "long",
        Some(t) if t > 4096 => "experimental",
        Some(t) if (256..=2048).contains(&t) => "recommended",
        Some(_) => "small",
        None => "unknown",
    }
    .to_string();
    (taps, group_delay_ms, level)
}

/// 生成单通道测试 WAV（16-bit PCM），用于每角色测试音/扫频。
pub fn build_test_tone_wav(
    channels: u32,
    channel: u32,
    sample_rate: u32,
    duration_ms: u32,
    sweep: bool,
) -> Vec<u8> {
    let channels = channels.max(1);
    let channel = channel.min(channels - 1);
    let rate = sample_rate.max(8_000);
    let frames = (rate as u64 * duration_ms as u64 / 1000).max(1) as u32;
    let data_bytes = frames as u64 * channels as u64 * 2;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * channels * 2).to_le_bytes());
    out.extend_from_slice(&((channels * 2) as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    let mut phase = 0f64;
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        let total = duration_ms as f64 / 1000.0;
        let freq = if sweep {
            let k = (10_000.0f64 / 100.0).ln();
            100.0 * (t / total * k).exp()
        } else {
            1000.0
        };
        phase += 2.0 * std::f64::consts::PI * freq / rate as f64;
        let fade = (n as f64 / (rate as f64 * 0.01)).min(1.0)
            * ((frames.saturating_sub(n)) as f64 / (rate as f64 * 0.01)).min(1.0);
        let sample = ((phase.sin() * 0.25 * fade) * i16::MAX as f64) as i16;
        for ch in 0..channels {
            out.extend_from_slice(&(if ch == channel { sample } else { 0 }).to_le_bytes());
        }
    }
    out
}

pub fn parse_document(text: &str) -> AppResult<ChannelMap> {
    if text.trim().is_empty() {
        return Err(AppError::bad_request("channelmap 文件为空"));
    }
    let mut doc: ChannelMap = serde_yaml::from_str(text)
        .map_err(|e| AppError::bad_request(format!("channelmap YAML 解析失败: {e}")))?;
    if doc.schema.is_empty() {
        doc.schema = CHANNELMAP_SCHEMA.into();
    }
    if doc.version == 0 {
        doc.version = 2;
    }
    Ok(doc)
}

fn parse_validate_output(out: &CommandOutput) -> Option<ValidationReport> {
    #[derive(Deserialize)]
    struct Item {
        path: String,
        msg: String,
    }
    #[derive(Deserialize)]
    struct Raw {
        ok: bool,
        #[serde(default)]
        errors: Vec<Item>,
        #[serde(default)]
        warnings: Vec<Item>,
    }
    if let Ok(raw) = serde_json::from_str::<Raw>(out.stdout.trim()) {
        return Some(ValidationReport {
            valid: raw.ok,
            errors: raw
                .errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.msg))
                .collect(),
            warnings: raw
                .warnings
                .iter()
                .map(|e| format!("{}: {}", e.path, e.msg))
                .collect(),
            channel_mismatch: false,
        });
    }
    // CLI 自身执行失败（如缺 python3-yaml、脚本不存在、权限错误）不是 schema/取值错误：
    // 返回 None 让 validate() 回退本地校验，避免把“命令跑不起来”误报成 channelmap 校验失败。
    let validation_failed =
        !out.success && (out.stderr.contains("ERROR [") || out.stdout.contains("WARN ["));
    if out.success || validation_failed {
        return Some(ValidationReport {
            valid: out.success,
            errors: if out.success {
                Vec::new()
            } else {
                out.stderr.lines().map(str::to_string).collect()
            },
            warnings: Vec::new(),
            channel_mismatch: false,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presets() -> LayoutPresets {
        LayoutPresets::parse(EMBEDDED_LAYOUT_PRESETS).unwrap()
    }

    #[test]
    fn embedded_presets_parse() {
        let p = presets();
        assert!(p.input_layouts.contains_key("h16-7.1.4"));
        assert!(p.output_layouts.contains_key("5.3.2"));
        assert_eq!(p.input_layouts["h16-7.1.4"].streams, vec![8, 4]);
        assert_eq!(p.input_layouts["h16-7.1.4"].roles.len(), 12);
        assert_eq!(p.output_layouts["5.3.2"].roles.len(), 10);
        // task-15：RZ50 布局集（4.0/4.1/4.1.2/5.1.4/7.1.2/7.2）保留为独立布局；
        // 用户口径暂不做跨布局 remap/upmix，因此只允许同布局 remap preset。
        assert_eq!(p.output_layouts["4.1.2"].roles.len(), 7);
        // 板端部署真相（audio/overlay/etc/tspi-audio/layout-presets.yml）：
        // FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7（ch6 空；MOTU Line Out 1-6,8，SUB=Line Out 8）。
        assert_eq!(p.output_layouts["4.1.2"].roles[4].id, "HFL");
        assert_eq!(p.output_layouts["4.1.2"].roles[4].outputs, vec![4]);
        assert_eq!(p.output_layouts["4.1.2"].roles[5].id, "HFR");
        assert_eq!(p.output_layouts["4.1.2"].roles[5].outputs, vec![5]);
        assert_eq!(p.output_layouts["4.1.2"].roles[6].id, "SUB1");
        assert_eq!(p.output_layouts["4.1.2"].roles[6].outputs, vec![7]);
        assert!(p.remap_presets.contains_key("5.1.2->5.1.2"));
        assert!(!p.remap_presets.contains_key("7.1.4->5.3.2"));
        assert!(!p.remap_presets.contains_key("7.1.4->4.1.2"));
        for (key, preset) in &p.remap_presets {
            assert_eq!(
                key,
                &format!(
                    "{}->{}",
                    preset.input.trim_start_matches("h16-"),
                    preset.output
                ),
                "不允许跨布局 remap preset：{key}"
            );
        }
    }

    #[test]
    fn four_one_two_layout_matches_deployed_motu_mapping() {
        // 板端部署文件 audio/overlay/etc/tspi-audio/layout-presets.yml（真相）：
        // FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，ch6 空（MOTU Line Out 1-6,8；SUB=Line Out 8）。
        // 按旧表（SUB1=4/HFL5/HFR6）接线会把 20–300 Hz 打进 8010 高度箱、SUB 静音。
        let p = presets();
        let expected = [
            ("FL", 0u32),
            ("FR", 1),
            ("SL", 2),
            ("SR", 3),
            ("HFL", 4),
            ("HFR", 5),
            ("SUB1", 7),
        ];
        let layout = &p.output_layouts["4.1.2"];
        assert_eq!(layout.roles.len(), expected.len());
        for (i, (id, out)) in expected.iter().enumerate() {
            assert_eq!(layout.roles[i].id, *id, "4.1.2 roles[{i}]");
            assert_eq!(
                layout.roles[i].outputs,
                vec![*out],
                "4.1.2 {id} outputs 应与板端部署一致"
            );
        }
        assert!(
            layout.roles.iter().all(|r| !r.outputs.contains(&6)),
            "4.1.2 没有角色映射到 ch6（ch6 空闲）"
        );

        // 渲染/校验路径：Web UI 的 draft 4.1.2 必须得到同一映射，并且本地校验通过。
        let doc = draft_document("h16-5.1.2", "4.1.2", "test-4.1.2", &p, Some(8), Some(8));
        assert_eq!(doc.output.roles.len(), 7);
        for (id, out) in expected {
            let role = doc
                .output
                .roles
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("draft 4.1.2 缺少角色 {id}"));
            assert_eq!(role.outputs, vec![out], "draft 4.1.2 {id} outputs");
        }
        let report = local_validate(
            &doc,
            &p,
            &ProbeChannels {
                capture: Some(8),
                playback: Some(8),
            },
            48000,
            None,
            &[],
        );
        assert!(report.valid, "{:?}", report);
    }

    #[test]
    fn draft_materializes_explicit_roles_and_sources() {
        let p = presets();
        let doc = draft_document("h16-5.1.2", "5.1.2", "test", &p, Some(8), Some(8));
        assert_eq!(doc.schema, CHANNELMAP_SCHEMA);
        assert_eq!(doc.version, 2);
        assert_eq!(doc.output.roles.len(), 8);
        assert!(doc.remap.routes.is_empty());
        let sub1 = doc.output.roles.iter().find(|r| r.id == "SUB1").unwrap();
        assert_eq!(sub1.sources[0].source, "LFE");
        let hfl = doc.output.roles.iter().find(|r| r.id == "HFL").unwrap();
        assert_eq!(hfl.sources[0].source, "TFL");
        let roles = effective_roles(&doc, &p);
        assert_eq!(roles.len(), 8);
    }

    #[test]
    fn local_validation_catches_bad_outputs() {
        let p = presets();
        let mut doc = draft_document("h16-5.1.2", "5.1.2", "t", &p, Some(8), Some(8));
        let report = local_validate(
            &doc,
            &p,
            &ProbeChannels {
                capture: Some(8),
                playback: Some(8),
            },
            48000,
            None,
            &[],
        );
        assert!(report.valid, "{:?}", report);
        doc.output.roles[0].outputs = vec![99];
        let report = local_validate(
            &doc,
            &p,
            &ProbeChannels {
                capture: Some(8),
                playback: Some(8),
            },
            48000,
            None,
            &[],
        );
        assert!(!report.valid);
        assert!(report.channel_mismatch);
    }

    fn local_report(doc: &ChannelMap) -> ValidationReport {
        let p = presets();
        local_validate(
            doc,
            &p,
            &ProbeChannels {
                capture: Some(8),
                playback: Some(8),
            },
            48000,
            None,
            &[],
        )
    }

    #[test]
    fn local_validation_mirrors_board_peq_rules() {
        // mock/测试用本地规则必须镜像板端 tspi-channelmap：type 白名单 + 按 type 分流的 q 界限。
        let p = presets();
        let mut doc = draft_document("h16-5.1.2", "5.1.2", "t", &p, Some(8), Some(8));
        doc.output.roles[0].peq.push(PeqBand {
            freq: 100.0,
            q: 0.7,
            gain_db: 2.5,
            filter_type: String::new(), // 缺省 = peaking
        });
        let report = local_report(&doc);
        assert!(report.valid, "{:?}", report);

        // v1.1 shelf：q=S ∈ [0.1, 1.0]
        doc.output.roles[0].peq[0].filter_type = "low_shelf".into();
        assert!(local_report(&doc).valid);
        doc.output.roles[0].peq[0].filter_type = "high_shelf".into();
        doc.output.roles[0].peq[0].q = 1.0;
        assert!(local_report(&doc).valid, "shelf q=1.0");
        doc.output.roles[0].peq[0].q = 1.5;
        let report = local_report(&doc);
        assert!(!report.valid, "shelf q=1.5");
        assert!(report.errors.iter().any(|e| e.contains("[0.1,1.0]")));
        doc.output.roles[0].peq[0].q = 0.7;

        // 未知 type：拒绝（板端 schema error rc=2）
        doc.output.roles[0].peq[0].filter_type = "notch".into();
        let report = local_report(&doc);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.contains("peq[0].type")));

        // q > 10：拒绝（与板端 Python 对齐，不再允许 1e6）
        doc.output.roles[0].peq[0].filter_type = "peaking".into();
        doc.output.roles[0].peq[0].q = 15.0;
        let report = local_report(&doc);
        assert!(!report.valid);
        assert!(report.errors.iter().any(|e| e.contains("[0.1, 10]")));

        // q < 0.1：拒绝
        doc.output.roles[0].peq[0].q = 0.05;
        assert!(!local_report(&doc).valid);
    }

    #[tokio::test]
    async fn validate_errors_when_cli_missing_in_real_mode() {
        use crate::cmd::RealCommandRunner;
        let mut cfg = ChannelmapConfig::default();
        cfg.cli_bin = "/definitely/not/exist/tspi-channelmap".into();
        let svc = ChannelMapService::new(
            cfg,
            Arc::new(RealCommandRunner::new(Duration::from_secs(2))),
            false,
        );
        let p = presets();
        let doc = draft_document("h16-5.1.2", "5.1.2", "t", &p, Some(8), Some(8));
        let err = svc
            .validate(&doc, &ProbeInfo::default(), 48_000)
            .await
            .expect_err("CLI 缺失时真机模式必须报错，不得回退本地规则");
        assert!(err.message().contains("tspi-channelmap 不可用"), "{err}");
    }

    #[test]
    fn materialize_clears_remap_and_keeps_sources() {
        let p = presets();
        let mut doc = draft_document("h16-5.1.2", "5.1.2", "t", &p, Some(8), Some(8));
        doc.remap.preset = Some("5.1.2->5.1.2".into());
        materialize_document(&mut doc, &p);
        assert!(doc.remap.preset.is_none());
        assert!(doc.remap.routes.is_empty());
        assert!(!doc.output.roles.is_empty());
        assert!(doc
            .output
            .roles
            .iter()
            .all(|r| !r.sources.is_empty() || !r.enabled));
    }

    fn cli_out(status: i32, success: bool, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            program: "tspi-channelmap".into(),
            args: Vec::new(),
            status: Some(status),
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn cli_fatal_failure_is_not_mistaken_for_validation_errors() {
        // 真机缺 python3-yaml：CLI 启动失败 → parse 返回 None，调用方必须报「不可用」错误
        //（2026-11-16 起不再回退本地规则放行）。
        let out = cli_out(
            2,
            false,
            "",
            "tspi-channelmap: 需要 python3-yaml（Debian 包 python3-yaml）",
        );
        assert!(parse_validate_output(&out).is_none());
    }

    #[test]
    fn cli_validation_errors_are_reported() {
        let out = cli_out(
            3,
            false,
            "",
            "ERROR [value] output.roles[0].outputs: 输出通道 99 超出播放通道数 22\nWARN [warn] remap: preset 未使用",
        );
        let report = parse_validate_output(&out).unwrap();
        assert!(!report.valid);
        assert!(report.errors[0].contains("output.roles"));
    }

    #[test]
    fn cli_success_returns_valid_report() {
        let out = cli_out(0, true, "", "WARN [warn] remap: x");
        let report = parse_validate_output(&out).unwrap();
        assert!(report.valid);
    }

    #[test]
    fn input_mode_normalize_aliases() {
        assert_eq!(normalize_input_mode("auto"), Some("auto"));
        assert_eq!(normalize_input_mode("force-stereo"), Some("stereo"));
        assert_eq!(
            normalize_input_mode("force-multichannel"),
            Some("multichannel")
        );
        assert_eq!(normalize_input_mode("2.0"), Some("stereo"));
        assert_eq!(normalize_input_mode("bogus"), None);
    }

    #[test]
    fn input_mode_view_merges_state_and_override() {
        let state = serde_json::json!({
            "schema": "tspi-input-content/v1",
            "content_mode": "auto",
            "detected": "stereo",
            "confidence": 0.91,
            "active_channels": ["FL", "FR"],
            "rms_db": {"FL": -20.1, "FR": -20.0, "C": -96.0},
            "threshold_db": -60.0,
            "hold_ms": 1500,
            "reason": "fl-fr-only",
            "h16_internal_upmix": false,
            "container_layout": "h16-7.1.4",
            "detector": "energy",
            "updated_unix": 1000
        });
        let view = build_input_mode_view(
            Some(&state),
            Some("auto".to_string()),
            "/run/tspi/input-content.json",
            "/etc/tspi-audio/input-mode.json",
            1005,
        );
        assert!(view.state_available);
        assert!(view.daemon_running);
        assert_eq!(view.detected, "stereo");
        assert_eq!(
            view.content_mode, "stereo",
            "detected=stereo 时 auto 应走 stereo"
        );
        assert_eq!(view.override_mode.as_deref(), Some("auto"));
        assert_eq!(view.confidence, 0.91);
        assert_eq!(view.active_channels, vec!["FL", "FR"]);
        assert_eq!(view.container_layout.as_deref(), Some("h16-7.1.4"));
        assert!(view.notes.iter().any(|n| n.contains("上混")));
    }

    #[test]
    fn input_mode_view_override_wins_and_multichannel_blocks_upmix() {
        let state = serde_json::json!({
            "content_mode": "multichannel",
            "detected": "multichannel",
            "confidence": 0.99,
            "h16_internal_upmix": true,
            "updated_unix": 2000
        });
        let view = build_input_mode_view(
            Some(&state),
            Some("stereo".to_string()),
            "/run/x",
            "/etc/y",
            2001,
        );
        assert_eq!(view.content_mode, "stereo", "override 优先级最高");
        assert_eq!(view.override_mode.as_deref(), Some("stereo"));
        assert!(view.notes.iter().any(|n| n.contains("不再二次上混")));
    }

    #[test]
    fn input_mode_view_accepts_upmix_daemon_sample() {
        // upmix-dev 可联调版样例（multichannel，h16-7.1.4 容器；LFE 不参与判定）
        let state = serde_json::json!({
            "schema": "tspi-input-content/v1",
            "content_mode": "multichannel",
            "detected": "multichannel",
            "override": null,
            "confidence": 1.0,
            "active_channels": ["FL","FR","C","LFE","SL","SR","BL","BR","TFL","TFR","TRL","TRR"],
            "rms_db": {"FL": -29.0, "FR": -29.0, "C": -29.0, "LFE": -45.0},
            "threshold_db": -50.0,
            "hold_ms": 2500,
            "reason": "secondary-active",
            "h16_internal_upmix": true,
            "container_layout": "h16-7.1.4",
            "detector": "energy",
            "updated_unix": 1794725356
        });
        let view = build_input_mode_view(
            Some(&state),
            None,
            "/run/tspi/input-content.json",
            "/etc/tspi-audio/input-mode.json",
            1794725400,
        );
        assert!(view.state_available);
        assert_eq!(view.content_mode, "multichannel");
        assert_eq!(view.detected, "multichannel");
        assert_eq!(view.override_mode, None);
        assert_eq!(view.confidence, 1.0);
        assert_eq!(view.reason.as_deref(), Some("secondary-active"));
        assert_eq!(view.h16_internal_upmix, Some(true));
        assert_eq!(view.active_channels.len(), 12);
        assert!(view.notes.iter().any(|n| n.contains("不再二次上混")));
    }

    #[test]
    fn input_mode_view_no_daemon_defaults_unknown() {
        let view = build_input_mode_view(None, None, "/run/x", "/etc/y", 3000);
        assert!(!view.state_available);
        assert_eq!(view.content_mode, "auto");
        assert_eq!(view.detected, "unknown");
        assert_eq!(view.reason.as_deref(), Some("daemon-not-running"));
        assert!(view.notes.iter().any(|n| n.contains("daemon")));
    }

    #[test]
    fn probe_json_parses() {
        let text = r#"{"ok": true, "playback": {"device": "hw:1,0", "channels_min": 22, "channels_max": 22, "formats": ["S32_LE"], "rates": [48000]}, "capture": {"device": "hw:0,0", "channels_max": 16}, "cards": ["0 [PCH]"], "notes": [], "needs_user_choice": false}"#;
        let p = parse_probe_json(text).unwrap();
        assert_eq!(p.playback_channels(), Some(22));
        assert_eq!(p.capture_channels(), Some(16));
    }

    #[test]
    fn fir_metrics_and_tone() {
        let mut txt = Vec::new();
        for _ in 0..1024 {
            txt.extend_from_slice(b"0\n");
        }
        let (taps, delay, level) = fir_metrics("a.txt", &txt, Some(48000), Some(1), Some(16));
        assert_eq!(taps, Some(1024));
        assert_eq!(level, "recommended");
        assert!((delay.unwrap() - 10.66).abs() < 0.1);
        let wav = build_test_tone_wav(8, 2, 48000, 100, false);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 8);
    }

    #[test]
    fn profile_name_validation() {
        assert!(validate_profile_name("5.3.2-h16-714").is_ok());
        assert!(validate_profile_name("../evil").is_err());
        assert!(validate_profile_name("bad name").is_err());
    }
}
