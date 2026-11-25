//! [NONRT] rt-host 自持的「缺中置内容适配」模块（plan-arena 播放链 capture 帧进图之前）。
//!
//! ## 为什么在 rt-host（task-210 / Cursor v1.1 N-10）
//!
//! task-200 先在 `compat::channel_map` 实现了缺中置下混（S1 兼容层），task-203 又把 host
//! 接线直接调用 `compat::channel_map` + `compat::roles`，给 rt-host 增加了 `tspi-dsp-compat`
//! 依赖。Cursor v1.1 复审 N-10 指出：若「host 不得调用 compat::CenterDownmix」仍是硬约束，
//! 这是 P1 分层违规（host 不应依赖/调用 compat 的 S1 兼容层）。因此本模块把内容适配
//! 实现迁入 rt-host 自持（本文件），rt-host 不再依赖/调用 compat；`compat::channel_map`
//! 保留为旧 S1/兼容层参考实现（测试保留），不再被 host 消费。
//!
//! ## 行为（与 task-200/task-203 冻结口径逐项一致）
//!
//! 中置内容消失的位置：4.1.2 输出布局 7 个角色没有 C，5.1 内容的 C 声道在 plan 的 A_map
//! 「选择/零填」处没有任何 role 可选中而被丢弃（core-v1 §1/§11.10；plan 运行期禁止混音、
//! A_map 每 role 至多一源且源唯一）。因此修复落在 **plan 之前的内容适配层**（本模块）：
//!
//! 1. **仅缺中置时启用**：输出布局无 C 角色（中置音箱缺席）且输入内容布局有 C 时才启用；
//! 2. **不改有中置配置**：输出布局含 C（5.1/5.1.2/5.3.2/7.1.4 等）时 passthrough
//!    （状态 `CenterDownmixStatus::DisabledCenterPresent`，缓冲逐位不变）；
//! 3. **只改 FL/FR**：其余源（SL/SR/LFE/高度等）逐样本不变；C 源在混后置零（防重复混）；
//! 4. **系数**：`FL += g·C`、`FR += g·C`，`g = 10^(−3/20) ≈ 0.7079458`（`CENTER_DOWNMIX_GAIN_LINEAR`，
//!    ITU-R BS.775 / Dolby 5.1→2.0 常用 center mix level，与 S1 upmix 预设「中置 −3 dB」一致）；
//! 5. **削波/余量保护（D7）**：最坏相干叠加 `20·log10(1+g) ≈ +4.649 dB`；预测累计峰值
//!    （program peak 基准 + 最坏叠加）超过 −6 dBFS 门时对**全部源等量 trim**（保持声道间
//!    相对电平），否则不 trim（默认 −18 dBFS 基准 → 预测 −13.351 dBFS，不缩水）。
//!
//! 本模块**不是**通用 downmix：不做 5.1→2.0 的环绕/低音折叠，不做节目上混/自动内容上混，
//! 只处理「缺中置音箱」这一种缺失（其余缺失角色仍由 plan 显式零填/拒收）。
//!
//! ## 布局来源：T57 overlay layout-presets v2（host 自持最小严格读取器）
//!
//! host 不复制 role→channel 映射表，仍在启动期读取同一份 T57 overlay 文件
//! （默认 `/etc/tspi-audio/layout-presets.yml`），与 compat/control 消费同一来源。
//! 为此本模块自带一个**最小、严格**的 YAML 子集读取器，只消费：
//! `schema`（必须精确 `tspi-layout-presets/v2`）、`input_layouts`、`output_layouts`；
//! `remap_presets` 段 host 不消费（路由绑定由 compat/control 负责），读取时整体忽略。
//! 未知顶层/嵌套字段、重复 key/角色 id/容器槽位、schema 不匹配一律拒收（不「尽量解析」）。
//! 与 compat::roles 的差异（如实记录，见 evidence/cursor-fix-v1-hostmap/）：
//! - 只实现 host 需要的字段/校验；label/description 只校验类型、不保存；
//! - 序列条目支持内联映射 `{..}` 与标量（overlay/WebUI 模板格式）；块状多行角色条目不支持；
//! - 不计算 source sha256 / layout binding hash（host 内容适配不需要版本绑定 hash）。
//!
//! ## RT 纪律
//!
//! `CenterDownmixPlan::apply_planar_f32_in_place` 是纯标量、零堆分配、无 panic 分支的
//! `&mut [f32]` 循环（长度/索引先校验），可在宿主块循环里用预分配平面缓冲调用；每个内容块
//! 只调用一次（C 源置零保证重复调用不会重复混入中置）。
//!
//! 真实板端/ALSA 出声 NOT_RUN（Windows/CI 只 fake/file 端到端；证据
//! `rt-stack/evidence/cursor-fix-v1-hostmap/`）。
//!
//! 错误码字符串沿用 `E_CHANNEL_MAP_*`（task-200/task-203 既有诊断口径，host 自持后不改
//! 诊断/证据契约）；解析/IO 用 host 自有的 `E_CONTENT_MAP_*`。

use std::fmt;
use std::path::Path;

use tspi_dsp_core::rt::{MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES};

// ---------------------------------------------------------------------------
// 冻结常量（缺中置下混；数值改动需 Lead 裁决 + 证据更新）
// ---------------------------------------------------------------------------

/// overlay layout-presets 的唯一 schema（不匹配即拒收，不尽量解析）。
pub const LAYOUT_PRESETS_SCHEMA: &str = "tspi-layout-presets/v2";
/// 中置角色 id（layout-presets / plan 角色命名）。
pub const CENTER_ROLE_ID: &str = "C";
/// 中置下混目标角色（顺序 = 左、右）。
pub const CENTER_DOWNMIX_TARGET_ROLES: [&str; 2] = ["FL", "FR"];
/// 标准中置下混系数（每个目标声道）：−3.0 dB。
///
/// 依据：ITU-R BS.775 / Dolby 5.1→2.0 常用 center mix level 0.7071（−3 dB），与 S1
/// upmix 预设的「中置 −3 dB」口径一致（`upmix/presets/upmix-presets.yml`）。
pub const CENTER_DOWNMIX_GAIN_DB: f64 = -3.0;
/// `CENTER_DOWNMIX_GAIN_DB` 的线性系数（f32；与 f64 精确值差 < 1e-7）。
pub const CENTER_DOWNMIX_GAIN_LINEAR: f32 = 0.707_945_8;
/// D7 预测累计峰值上限（dBFS；与 `calibration-spec/ACCEPTANCE.md:190`、D7 常量同值）。
pub const CENTER_DOWNMIX_PEAK_LIMIT_DBFS: f64 = -6.0;
/// D7 最小 headroom（dB；= −predicted_peak_limit）。
pub const CENTER_DOWNMIX_MIN_HEADROOM_DB: f64 = 6.0;
/// D7 program peak 基准默认值（dBFS；同 `calibration/src/peq_fit.rs:330` 默认 −18 dBFS）。
pub const CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS: f64 = -18.0;

/// 中置下混标准系数的 f64 精确值（10^(dB/20)）。
pub fn center_downmix_gain_linear_f64() -> f64 {
    10.0_f64.powf(CENTER_DOWNMIX_GAIN_DB / 20.0)
}

/// 最坏相干叠加增益（dB）：FL/FR 与 C 同相且都满幅时 `20·log10(1+g)` ≈ +4.65 dB。
///
/// 该值只用于 headroom 报告/校验，不是运行增益；正常内容（program peak 基准 −18 dBFS）下
/// 预测峰值 ≈ −13.35 dBFS，仍在 D7 的 −6 dBFS 门内，故默认不做 trim（−3 dB 系数不缩水）。
pub fn center_downmix_worst_case_added_db() -> f64 {
    20.0 * (1.0 + center_downmix_gain_linear_f64()).log10()
}

// ---------------------------------------------------------------------------
// 类型：错误（code + 字段路径 + 明细；不 panic、不静默降级）
// ---------------------------------------------------------------------------

/// content-map 模块错误码（稳定字符串；下混错误沿用 `E_CHANNEL_MAP_*` 既有诊断口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentMapErrorCode {
    /// overlay layout-presets YAML 解析失败（schema/结构/字段非法）。
    LayoutParse,
    /// overlay 预设文件 IO 失败。
    Io,
    /// 引用的输入/输出布局在 overlay 预设中不存在。
    LayoutUnknown,
    /// 布局通道声明非法（stream 越界/通道越界/通道数非正）。
    LayoutChannels,
    /// 必需角色缺失（C 内容/FL/FR 目标）。
    RoleMissing,
    /// 角色/通道表冲突（重复角色、重复通道、FL==FR==C 等）。
    RoleChannelConflict,
    /// 中置声明与输出布局矛盾（声明 Present 但无 C 角色；声明 Absent 但含 C 角色）。
    CenterPolicyConflict,
    /// headroom 配置非法（program peak 非有限等）。
    HeadroomConfig,
    /// planar 缓冲长度/frames 与计划不匹配。
    BufferSize,
}

impl ContentMapErrorCode {
    /// 全部错误码（测试/报告用）。
    pub const ALL: &'static [ContentMapErrorCode] = &[
        ContentMapErrorCode::LayoutParse,
        ContentMapErrorCode::Io,
        ContentMapErrorCode::LayoutUnknown,
        ContentMapErrorCode::LayoutChannels,
        ContentMapErrorCode::RoleMissing,
        ContentMapErrorCode::RoleChannelConflict,
        ContentMapErrorCode::CenterPolicyConflict,
        ContentMapErrorCode::HeadroomConfig,
        ContentMapErrorCode::BufferSize,
    ];

    /// 稳定错误码字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ContentMapErrorCode::LayoutParse => "E_CONTENT_MAP_LAYOUT_PARSE",
            ContentMapErrorCode::Io => "E_CONTENT_MAP_IO",
            ContentMapErrorCode::LayoutUnknown => "E_CHANNEL_MAP_LAYOUT_UNKNOWN",
            ContentMapErrorCode::LayoutChannels => "E_CHANNEL_MAP_LAYOUT_CHANNELS",
            ContentMapErrorCode::RoleMissing => "E_CHANNEL_MAP_ROLE_MISSING",
            ContentMapErrorCode::RoleChannelConflict => "E_CHANNEL_MAP_ROLE_CHANNEL_CONFLICT",
            ContentMapErrorCode::CenterPolicyConflict => "E_CHANNEL_MAP_CENTER_POLICY_CONFLICT",
            ContentMapErrorCode::HeadroomConfig => "E_CHANNEL_MAP_HEADROOM_CONFIG",
            ContentMapErrorCode::BufferSize => "E_CHANNEL_MAP_BUFFER_SIZE",
        }
    }
}

/// content-map 模块错误（code + 字段路径 + 明细；不 panic、不静默降级）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentMapError {
    /// 稳定错误码。
    pub code: ContentMapErrorCode,
    /// 字段路径（例如 source.role_channels）。
    pub field: String,
    /// 人读明细。
    pub detail: String,
}

impl ContentMapError {
    /// 构造（无字段路径）。
    pub fn new(code: ContentMapErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: String::new(),
            detail: detail.into(),
        }
    }

    /// 构造（带字段路径）。
    pub fn with_field(
        code: ContentMapErrorCode,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 稳定错误码字符串。
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for ContentMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field.is_empty() {
            write!(formatter, "{}: {}", self.code.as_str(), self.detail)
        } else {
            write!(
                formatter,
                "{} at {}: {}",
                self.code.as_str(),
                self.field,
                self.detail
            )
        }
    }
}

impl std::error::Error for ContentMapError {}

fn yaml_error(field: impl Into<String>, detail: impl Into<String>) -> ContentMapError {
    ContentMapError::with_field(ContentMapErrorCode::LayoutParse, field, detail)
}

// ---------------------------------------------------------------------------
// 布局预设类型（host 自持最小读取器的输出；只保留内容适配需要的字段）
// ---------------------------------------------------------------------------

/// 输入布局角色（capture 侧；stream 默认 0）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputLayoutRole {
    /// 角色 id。
    pub id: String,
    /// 流序号（H16 多 flow；缺省 0）。
    pub stream: u16,
    /// 流内 0-based 通道号。
    pub channel: u16,
}

/// 输入布局预设（input_layouts.<id>）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputLayoutPreset {
    /// 布局 id。
    pub id: String,
    /// 声明通道数；None = auto（以实测为准）。
    pub channels: Option<u16>,
    /// 声明的 flow 通道数列表（可选）。
    pub streams: Vec<u16>,
    /// 角色表（文件顺序）。
    pub roles: Vec<InputLayoutRole>,
}

/// 输出布局角色（playback 侧；outputs 是容器/槽位 0-based 索引）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLayoutRole {
    /// 角色 id。
    pub id: String,
    /// 该角色占用的容器槽位（非空；同一布局内必须单射）。
    pub outputs: Vec<u16>,
}

/// 输出布局预设（output_layouts.<id>）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLayoutPreset {
    /// 布局 id。
    pub id: String,
    /// 角色表（文件顺序 = 角色索引顺序；不含空槽）。
    pub roles: Vec<OutputLayoutRole>,
}

/// 解析后的 overlay layout 预设文档（tspi-layout-presets/v2；host 只消费 input/output）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPresets {
    schema: String,
    input_layouts: Vec<InputLayoutPreset>,
    output_layouts: Vec<OutputLayoutPreset>,
}

impl LayoutPresets {
    /// 解析 overlay 预设字节（host 自持读取器；schema 精确匹配、未知字段拒收）。
    pub fn parse_overlay(bytes: &[u8]) -> Result<Self, ContentMapError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| yaml_error("$", format!("overlay 预设不是合法 UTF-8：{error}")))?;
        let root = parse_yaml(text)?;
        interpret_presets(&root)
    }

    /// 从文件读取并解析 overlay 预设（只读；IO 失败 = E_CONTENT_MAP_IO）。
    pub fn load_overlay_file(path: &Path) -> Result<Self, ContentMapError> {
        let bytes = std::fs::read(path).map_err(|error| {
            ContentMapError::with_field(
                ContentMapErrorCode::Io,
                "path",
                format!("读取 overlay 预设失败（{}）：{error}", path.display()),
            )
        })?;
        Self::parse_overlay(&bytes)
    }

    /// schema 字面值。
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// 输入布局表（文件顺序）。
    pub fn input_layouts(&self) -> &[InputLayoutPreset] {
        &self.input_layouts
    }

    /// 输出布局表（文件顺序）。
    pub fn output_layouts(&self) -> &[OutputLayoutPreset] {
        &self.output_layouts
    }

    /// 按 id 查输出布局。
    pub fn output_layout(&self, layout_id: &str) -> Option<&OutputLayoutPreset> {
        self.output_layouts
            .iter()
            .find(|layout| layout.id == layout_id)
    }

    /// 按 id 查输入布局。
    pub fn input_layout(&self, layout_id: &str) -> Option<&InputLayoutPreset> {
        self.input_layouts
            .iter()
            .find(|layout| layout.id == layout_id)
    }
}

// ---------------------------------------------------------------------------
// 最小严格 YAML 子集解析器（只服务 layout-presets v2 的 input/output 段）
// ---------------------------------------------------------------------------

/// 解析后的最小 YAML 节点（映射/序列/标量）。
#[derive(Clone, Debug, PartialEq)]
enum YamlNode {
    Map(Vec<(String, YamlNode)>),
    Seq(Vec<YamlNode>),
    Scalar(String),
}

impl YamlNode {
    fn type_name(&self) -> &'static str {
        match self {
            YamlNode::Map(_) => "mapping",
            YamlNode::Seq(_) => "sequence",
            YamlNode::Scalar(_) => "scalar",
        }
    }
}

/// 预处理后的一行（缩进 + 去掉注释/行尾后的内容）。
struct YamlLine {
    indent: usize,
    text: String,
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        match character {
            '"' => in_double = true,
            '\'' => in_single = true,
            '#' => {
                let preceded_by_space = index > 0
                    && line[..index]
                        .chars()
                        .next_back()
                        .map(|prev| prev == ' ' || prev == '\t')
                        .unwrap_or(false);
                if index == 0 || preceded_by_space {
                    return &line[..index];
                }
            }
            _ => {}
        }
    }
    line
}

fn yaml_lines(text: &str) -> Result<Vec<YamlLine>, ContentMapError> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let content = strip_yaml_comment(raw).trim_end();
        if content.trim().is_empty() {
            continue;
        }
        let indent = content
            .chars()
            .take_while(|character| *character == ' ')
            .count();
        let rest = &content[indent..];
        if rest.starts_with('\t') {
            return Err(yaml_error(
                "$",
                "缩进不允许使用 tab（只支持空格缩进）".to_string(),
            ));
        }
        lines.push(YamlLine {
            indent,
            text: rest.trim_end().to_string(),
        });
    }
    Ok(lines)
}

fn is_yaml_seq_line(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn parse_yaml(text: &str) -> Result<YamlNode, ContentMapError> {
    let lines = yaml_lines(text)?;
    if lines.is_empty() {
        return Err(yaml_error("$", "overlay 预设为空".to_string()));
    }
    let mut index = 0usize;
    let root = parse_yaml_block(&lines, &mut index, lines[0].indent)?;
    if index != lines.len() {
        return Err(yaml_error(
            "$",
            format!("YAML 解析后仍有未消费的行（第 {} 行起）", index + 1),
        ));
    }
    Ok(root)
}

fn parse_yaml_block(
    lines: &[YamlLine],
    index: &mut usize,
    indent: usize,
) -> Result<YamlNode, ContentMapError> {
    if *index >= lines.len() {
        return Err(yaml_error("$", "YAML 在期望块的位置提前结束".to_string()));
    }
    if is_yaml_seq_line(&lines[*index].text) {
        parse_yaml_seq(lines, index, indent)
    } else {
        parse_yaml_map(lines, index, indent).map(YamlNode::Map)
    }
}

fn parse_yaml_map(
    lines: &[YamlLine],
    index: &mut usize,
    indent: usize,
) -> Result<Vec<(String, YamlNode)>, ContentMapError> {
    let mut fields: Vec<(String, YamlNode)> = Vec::new();
    while *index < lines.len() {
        let line = &lines[*index];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(yaml_error(
                "$",
                format!(
                    "意外缩进（期望 {indent} 个空格，实得 {}）：{:?}",
                    line.indent, line.text
                ),
            ));
        }
        if is_yaml_seq_line(&line.text) {
            break;
        }
        let (key_raw, rest) = split_yaml_mapping_entry(&line.text, &format!("$[{}]", *index + 1))?;
        let key = yaml_key(key_raw)?;
        *index += 1;
        let value = parse_yaml_value_after_key(lines, index, indent, rest)?;
        if fields.iter().any(|(existing, _)| existing == &key) {
            return Err(yaml_error(format!("$.{key}"), format!("重复 key：{key:?}")));
        }
        fields.push((key, value));
    }
    Ok(fields)
}

fn parse_yaml_seq(
    lines: &[YamlLine],
    index: &mut usize,
    indent: usize,
) -> Result<YamlNode, ContentMapError> {
    let mut items: Vec<YamlNode> = Vec::new();
    while *index < lines.len() {
        let line = &lines[*index];
        if line.indent != indent || !is_yaml_seq_line(&line.text) {
            break;
        }
        let after_dash = &line.text[1..];
        let trimmed = after_dash.trim_start();
        let item_indent = indent + 1 + (after_dash.len() - trimmed.len());
        *index += 1;
        let item = if trimmed.is_empty() {
            if *index < lines.len() && lines[*index].indent > indent {
                let child_indent = lines[*index].indent;
                parse_yaml_block(lines, index, child_indent)?
            } else {
                YamlNode::Scalar(String::new())
            }
        } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
            parse_yaml_flow_value(trimmed)?
        } else if split_yaml_mapping_entry(trimmed, "$").is_ok() {
            let (key_raw, rest) = split_yaml_mapping_entry(trimmed, "$")?;
            let key = yaml_key(key_raw)?;
            let value = parse_yaml_value_after_key(lines, index, item_indent, rest)?;
            let mut fields = vec![(key, value)];
            fields.extend(parse_yaml_map(lines, index, item_indent)?);
            YamlNode::Map(fields)
        } else {
            YamlNode::Scalar(yaml_scalar(trimmed))
        };
        items.push(item);
    }
    if *index < lines.len() && lines[*index].indent > indent {
        return Err(yaml_error(
            "$",
            format!("序列条目后出现意外缩进：{:?}", lines[*index].text),
        ));
    }
    Ok(YamlNode::Seq(items))
}

fn parse_yaml_value_after_key(
    lines: &[YamlLine],
    index: &mut usize,
    parent_indent: usize,
    rest: &str,
) -> Result<YamlNode, ContentMapError> {
    if !rest.is_empty() {
        return parse_yaml_flow_value(rest);
    }
    if *index < lines.len() && lines[*index].indent > parent_indent {
        let child_indent = lines[*index].indent;
        return parse_yaml_block(lines, index, child_indent);
    }
    Ok(YamlNode::Scalar(String::new()))
}

fn parse_yaml_flow_value(text: &str) -> Result<YamlNode, ContentMapError> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        parse_yaml_flow_map(trimmed)
    } else if trimmed.starts_with('[') {
        parse_yaml_flow_seq(trimmed)
    } else {
        Ok(YamlNode::Scalar(yaml_scalar(trimmed)))
    }
}

fn parse_yaml_flow_seq(text: &str) -> Result<YamlNode, ContentMapError> {
    let Some(inner) = text
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Err(yaml_error("$", format!("内联序列缺少闭合 ']'：{text:?}")));
    };
    let mut items: Vec<YamlNode> = Vec::new();
    for item in split_yaml_flow_items(inner)? {
        if item.trim().is_empty() {
            continue;
        }
        items.push(parse_yaml_flow_value(item)?);
    }
    Ok(YamlNode::Seq(items))
}

fn parse_yaml_flow_map(text: &str) -> Result<YamlNode, ContentMapError> {
    let Some(inner) = text
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return Err(yaml_error("$", format!("内联映射缺少闭合 '}}'：{text:?}")));
    };
    let mut fields: Vec<(String, YamlNode)> = Vec::new();
    for item in split_yaml_flow_items(inner)? {
        if item.trim().is_empty() {
            continue;
        }
        let (key_raw, rest) = split_yaml_mapping_entry(item, "$")?;
        let key = yaml_key(key_raw)?;
        if fields.iter().any(|(existing, _)| existing == &key) {
            return Err(yaml_error(format!("$.{key}"), format!("重复 key：{key:?}")));
        }
        let value = if rest.is_empty() {
            YamlNode::Scalar(String::new())
        } else {
            parse_yaml_flow_value(rest)?
        };
        fields.push((key, value));
    }
    Ok(YamlNode::Map(fields))
}

fn split_yaml_flow_items(text: &str) -> Result<Vec<&str>, ContentMapError> {
    let mut items: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        match character {
            '"' => in_double = true,
            '\'' => in_single = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if depth != 0 || in_single || in_double {
        return Err(yaml_error("$", format!("内联结构未闭合：{text:?}")));
    }
    items.push(text[start..].trim());
    Ok(items)
}

fn split_yaml_mapping_entry<'a>(
    text: &'a str,
    field: &str,
) -> Result<(&'a str, &'a str), ContentMapError> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut depth = 0i32;
    for (index, character) in text.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        match character {
            '"' => in_double = true,
            '\'' => in_single = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ':' if depth == 0 => {
                return Ok((text[..index].trim(), text[index + 1..].trim()));
            }
            _ => {}
        }
    }
    Err(yaml_error(
        field,
        format!("映射条目缺少 ':'：{text:?}（内联结构或非映射行）"),
    ))
}

fn yaml_key(raw: &str) -> Result<String, ContentMapError> {
    let key = yaml_scalar(raw);
    if key.is_empty() {
        return Err(yaml_error("$", "映射 key 不能为空".to_string()));
    }
    Ok(key)
}

fn yaml_scalar(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        yaml_unescape_double(&raw[1..raw.len() - 1])
    } else if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        raw[1..raw.len() - 1].replace("''", "'")
    } else {
        raw.to_string()
    }
}

fn yaml_unescape_double(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

// ---------------------------------------------------------------------------
// layout-presets v2 语义解释（严格校验）
// ---------------------------------------------------------------------------

fn interpret_presets(root: &YamlNode) -> Result<LayoutPresets, ContentMapError> {
    let fields = require_yaml_map(root, "$")?;
    check_unknown_yaml_keys(
        fields,
        &["schema", "input_layouts", "output_layouts", "remap_presets"],
        "$",
    )?;
    let schema = require_yaml_str(fields, "schema", "$")?.to_string();
    if schema != LAYOUT_PRESETS_SCHEMA {
        return Err(yaml_error(
            "schema",
            format!("必须为 {LAYOUT_PRESETS_SCHEMA}，实际 {schema:?}（不匹配即拒收，不尽量解析）"),
        ));
    }
    let Some(output_value) = yaml_map_get(fields, "output_layouts") else {
        return Err(yaml_error(
            "output_layouts",
            "缺少 output_layouts 段".to_string(),
        ));
    };
    let output_layouts = parse_output_layouts(output_value)?;
    if output_layouts.is_empty() {
        return Err(yaml_error(
            "output_layouts",
            "至少需要一个输出布局".to_string(),
        ));
    }
    let input_layouts = match yaml_map_get(fields, "input_layouts") {
        Some(value) => parse_input_layouts(value)?,
        None => Vec::new(),
    };
    // remap_presets 段 host 不消费（路由绑定属 compat/control）；只校验存在时是映射。
    if let Some(value) = yaml_map_get(fields, "remap_presets") {
        require_yaml_map(value, "remap_presets")?;
    }
    Ok(LayoutPresets {
        schema,
        input_layouts,
        output_layouts,
    })
}

fn require_yaml_map<'a>(
    node: &'a YamlNode,
    field: &str,
) -> Result<&'a [(String, YamlNode)], ContentMapError> {
    match node {
        YamlNode::Map(fields) => Ok(fields),
        other => Err(yaml_error(
            field,
            format!("必须是 mapping，实际 {}", other.type_name()),
        )),
    }
}

fn require_yaml_seq<'a>(
    node: &'a YamlNode,
    field: &str,
) -> Result<&'a [YamlNode], ContentMapError> {
    match node {
        YamlNode::Seq(items) => Ok(items),
        other => Err(yaml_error(
            field,
            format!("必须是 sequence，实际 {}", other.type_name()),
        )),
    }
}

fn yaml_map_get<'a>(fields: &'a [(String, YamlNode)], key: &str) -> Option<&'a YamlNode> {
    fields
        .iter()
        .find(|(existing, _)| existing == key)
        .map(|(_, value)| value)
}

fn require_yaml_str<'a>(
    fields: &'a [(String, YamlNode)],
    key: &str,
    field: &str,
) -> Result<&'a str, ContentMapError> {
    let value = yaml_map_get(fields, key)
        .ok_or_else(|| yaml_error(format!("{field}.{key}"), format!("缺少 {key}")))?;
    match value {
        YamlNode::Scalar(scalar) => Ok(scalar),
        other => Err(yaml_error(
            format!("{field}.{key}"),
            format!("必须是 scalar，实际 {}", other.type_name()),
        )),
    }
}

fn check_optional_yaml_scalar(
    fields: &[(String, YamlNode)],
    key: &str,
    field: &str,
) -> Result<(), ContentMapError> {
    if let Some(YamlNode::Scalar(_)) = yaml_map_get(fields, key) {
        return Ok(());
    }
    if yaml_map_get(fields, key).is_some() {
        return Err(yaml_error(
            format!("{field}.{key}"),
            format!("{key} 必须是 scalar"),
        ));
    }
    Ok(())
}

fn check_unknown_yaml_keys(
    fields: &[(String, YamlNode)],
    allowed: &[&str],
    field: &str,
) -> Result<(), ContentMapError> {
    for (key, _) in fields {
        if !allowed.contains(&key.as_str()) {
            return Err(yaml_error(
                format!("{field}.{key}"),
                format!(
                    "未知字段 {key:?}（允许：{}；未知字段一律拒收）",
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn yaml_int(node: &YamlNode, field: &str) -> Result<i64, ContentMapError> {
    let YamlNode::Scalar(scalar) = node else {
        return Err(yaml_error(
            field,
            format!("必须是整数，实际 {}", node.type_name()),
        ));
    };
    scalar
        .parse::<i64>()
        .map_err(|_| yaml_error(field, format!("必须是整数，实际 {scalar:?}")))
}

fn yaml_u16_in_range(
    node: &YamlNode,
    field: &str,
    min: u16,
    max_exclusive: usize,
) -> Result<u16, ContentMapError> {
    let raw = yaml_int(node, field)?;
    if raw < i64::from(min) || raw as usize >= max_exclusive {
        return Err(yaml_error(
            field,
            format!("整数 {raw} 越界（允许 {min}..{})", max_exclusive - 1),
        ));
    }
    Ok(raw as u16)
}

fn validate_layout_id(layout_id: &str, field: &str) -> Result<(), ContentMapError> {
    if layout_id.is_empty() || layout_id.len() > 64 {
        return Err(yaml_error(
            field,
            format!("布局 id 长度必须在 1..=64 字节（实得 {layout_id:?}）"),
        ));
    }
    if !layout_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || ". _ - >".contains(character))
    {
        return Err(yaml_error(
            field,
            format!("布局 id 字符集非法（只允许 ASCII 字母数字 . _ - >）：{layout_id:?}"),
        ));
    }
    Ok(())
}

fn validate_role_id(role_id: &str, field: &str) -> Result<(), ContentMapError> {
    if role_id.is_empty() || role_id.len() > 64 {
        return Err(yaml_error(
            field,
            format!("角色 id 长度必须在 1..=64 字节（实得 {role_id:?}）"),
        ));
    }
    if !role_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(yaml_error(
            field,
            format!("角色 id 字符集非法（只允许 ASCII 字母数字 _）：{role_id:?}"),
        ));
    }
    Ok(())
}

fn parse_output_layouts(value: &YamlNode) -> Result<Vec<OutputLayoutPreset>, ContentMapError> {
    let entries = require_yaml_map(value, "output_layouts")?;
    let mut layouts: Vec<OutputLayoutPreset> = Vec::with_capacity(entries.len());
    for (layout_id, entry) in entries {
        let field = format!("output_layouts.{layout_id}");
        validate_layout_id(layout_id, &field)?;
        let fields = require_yaml_map(entry, &field)?;
        check_unknown_yaml_keys(fields, &["label", "description", "roles"], &field)?;
        check_optional_yaml_scalar(fields, "label", &field)?;
        check_optional_yaml_scalar(fields, "description", &field)?;
        let Some(roles_value) = yaml_map_get(fields, "roles") else {
            return Err(yaml_error(
                format!("{field}.roles"),
                "缺少 roles".to_string(),
            ));
        };
        let role_items = require_yaml_seq(roles_value, &format!("{field}.roles"))?;
        if role_items.is_empty() {
            return Err(yaml_error(
                format!("{field}.roles"),
                "布局至少需要一个角色".to_string(),
            ));
        }
        if role_items.len() > MAX_OUTPUT_ROLES {
            return Err(yaml_error(
                format!("{field}.roles"),
                format!("角色数 {} 超过上限 {MAX_OUTPUT_ROLES}", role_items.len()),
            ));
        }
        let mut roles: Vec<OutputLayoutRole> = Vec::with_capacity(role_items.len());
        let mut used_outputs: Vec<u16> = Vec::new();
        for (role_position, item) in role_items.iter().enumerate() {
            let role_field = format!("{field}.roles[{role_position}]");
            let role_fields = require_yaml_map(item, &role_field)?;
            check_unknown_yaml_keys(role_fields, &["id", "label", "outputs"], &role_field)?;
            let role_id = require_yaml_str(role_fields, "id", &role_field)?.to_string();
            validate_role_id(&role_id, &format!("{role_field}.id"))?;
            if roles.iter().any(|role| role.id == role_id) {
                return Err(yaml_error(
                    format!("{role_field}.id"),
                    format!("角色 id 重复：{role_id}"),
                ));
            }
            check_optional_yaml_scalar(role_fields, "label", &role_field)?;
            let Some(outputs_value) = yaml_map_get(role_fields, "outputs") else {
                return Err(yaml_error(
                    format!("{role_field}.outputs"),
                    "缺少 outputs".to_string(),
                ));
            };
            let output_items = require_yaml_seq(outputs_value, &format!("{role_field}.outputs"))?;
            if output_items.is_empty() {
                return Err(yaml_error(
                    format!("{role_field}.outputs"),
                    "outputs 不能为空（角色必须显式占用至少一个容器槽位）".to_string(),
                ));
            }
            let mut outputs: Vec<u16> = Vec::with_capacity(output_items.len());
            for (output_position, output_value) in output_items.iter().enumerate() {
                let output_field = format!("{role_field}.outputs[{output_position}]");
                let output =
                    yaml_u16_in_range(output_value, &output_field, 0, MAX_OUTPUT_CHANNELS)?;
                if used_outputs.contains(&output) {
                    return Err(yaml_error(
                        output_field,
                        format!("容器槽位 {output} 被多个角色占用（scatter 必须单射）"),
                    ));
                }
                used_outputs.push(output);
                outputs.push(output);
            }
            roles.push(OutputLayoutRole {
                id: role_id,
                outputs,
            });
        }
        layouts.push(OutputLayoutPreset {
            id: layout_id.clone(),
            roles,
        });
    }
    Ok(layouts)
}

fn parse_input_layouts(value: &YamlNode) -> Result<Vec<InputLayoutPreset>, ContentMapError> {
    let entries = require_yaml_map(value, "input_layouts")?;
    let mut layouts: Vec<InputLayoutPreset> = Vec::with_capacity(entries.len());
    for (layout_id, entry) in entries {
        let field = format!("input_layouts.{layout_id}");
        validate_layout_id(layout_id, &field)?;
        let fields = require_yaml_map(entry, &field)?;
        check_unknown_yaml_keys(
            fields,
            &["label", "description", "channels", "streams", "roles"],
            &field,
        )?;
        check_optional_yaml_scalar(fields, "label", &field)?;
        check_optional_yaml_scalar(fields, "description", &field)?;
        let channels = match yaml_map_get(fields, "channels") {
            None => None,
            Some(YamlNode::Scalar(text)) if text == "auto" => None,
            Some(node) => Some(yaml_u16_in_range(
                node,
                &format!("{field}.channels"),
                1,
                MAX_INPUT_CHANNELS + 1,
            )?),
        };
        let mut streams: Vec<u16> = Vec::new();
        if let Some(streams_value) = yaml_map_get(fields, "streams") {
            let stream_items = require_yaml_seq(streams_value, &format!("{field}.streams"))?;
            for (stream_position, stream_value) in stream_items.iter().enumerate() {
                let stream_field = format!("{field}.streams[{stream_position}]");
                streams.push(yaml_u16_in_range(
                    stream_value,
                    &stream_field,
                    1,
                    MAX_INPUT_CHANNELS + 1,
                )?);
            }
        }
        let Some(roles_value) = yaml_map_get(fields, "roles") else {
            return Err(yaml_error(
                format!("{field}.roles"),
                "缺少 roles".to_string(),
            ));
        };
        let role_items = require_yaml_seq(roles_value, &format!("{field}.roles"))?;
        if role_items.is_empty() {
            return Err(yaml_error(
                format!("{field}.roles"),
                "输入布局至少需要一个角色".to_string(),
            ));
        }
        if role_items.len() > MAX_INPUT_CHANNELS {
            return Err(yaml_error(
                format!("{field}.roles"),
                format!(
                    "输入角色数 {} 超过上限 {MAX_INPUT_CHANNELS}",
                    role_items.len()
                ),
            ));
        }
        let mut roles: Vec<InputLayoutRole> = Vec::with_capacity(role_items.len());
        let mut used_channels: Vec<(u16, u16)> = Vec::new();
        for (role_position, item) in role_items.iter().enumerate() {
            let role_field = format!("{field}.roles[{role_position}]");
            let role_fields = require_yaml_map(item, &role_field)?;
            check_unknown_yaml_keys(
                role_fields,
                &["id", "label", "stream", "channel"],
                &role_field,
            )?;
            let role_id = require_yaml_str(role_fields, "id", &role_field)?.to_string();
            validate_role_id(&role_id, &format!("{role_field}.id"))?;
            if roles.iter().any(|role| role.id == role_id) {
                return Err(yaml_error(
                    format!("{role_field}.id"),
                    format!("输入角色 id 重复：{role_id}"),
                ));
            }
            check_optional_yaml_scalar(role_fields, "label", &role_field)?;
            let stream = match yaml_map_get(role_fields, "stream") {
                None => 0u16,
                Some(node) => {
                    yaml_u16_in_range(node, &format!("{role_field}.stream"), 0, MAX_INPUT_CHANNELS)?
                }
            };
            let Some(channel_value) = yaml_map_get(role_fields, "channel") else {
                return Err(yaml_error(
                    format!("{role_field}.channel"),
                    "缺少 channel".to_string(),
                ));
            };
            let channel = yaml_u16_in_range(
                channel_value,
                &format!("{role_field}.channel"),
                0,
                MAX_INPUT_CHANNELS,
            )?;
            if used_channels.contains(&(stream, channel)) {
                return Err(yaml_error(
                    format!("{role_field}.channel"),
                    format!("(stream,channel)=({stream},{channel}) 被多个输入角色占用"),
                ));
            }
            used_channels.push((stream, channel));
            roles.push(InputLayoutRole {
                id: role_id,
                stream,
                channel,
            });
        }
        layouts.push(InputLayoutPreset {
            id: layout_id.clone(),
            channels,
            streams,
            roles,
        });
    }
    Ok(layouts)
}

// ---------------------------------------------------------------------------
// 类型：声明/状态
// ---------------------------------------------------------------------------

/// 中置音箱声明。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CenterSpeakerState {
    /// 由输出布局决定：布局含 C 角色 = 中置存在；不含 = 中置缺席（默认）。
    Auto,
    /// 显式声明中置存在：不做下混；若输出布局无 C 角色则拒收（配置矛盾）。
    Present,
    /// 显式声明中置缺席：输出布局含 C 角色则拒收（不改有中置配置）。
    Absent,
}

/// 缺中置适配计划状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CenterDownmixStatus {
    /// 已启用：C 以 −3 dB 预混进 FL/FR，C 源将被消费（置零）。
    Applied,
    /// 未启用：输出布局含 C（有中置配置）——不改变任何样本。
    DisabledCenterPresent,
    /// 未启用：输入内容布局没有 C（无中置内容）——不改变任何样本。
    DisabledNoCenterContent,
}

impl CenterDownmixStatus {
    /// 是否启用下混。
    pub fn is_applied(self) -> bool {
        matches!(self, CenterDownmixStatus::Applied)
    }

    /// 稳定状态字符串（报告/日志用；不随文案变化）。
    pub fn as_str(self) -> &'static str {
        match self {
            CenterDownmixStatus::Applied => "applied",
            CenterDownmixStatus::DisabledCenterPresent => "disabled_center_present",
            CenterDownmixStatus::DisabledNoCenterContent => "disabled_no_center_content",
        }
    }
}

// ---------------------------------------------------------------------------
// 类型：内容源域 / 输出目标 / 配置 / headroom
// ---------------------------------------------------------------------------

/// 内容源域描述：输入内容布局的「角色 → planar 通道索引」表。
///
/// 来源可以是 T57 输入布局预设（`from_layout`），也可以是 T43 H16 RolePlan/显式配置
/// （`new`：例如 `[("FL",0),("FR",1),("C",2),("LFE",3),("SL",4),("SR",5)]`）。
/// 本模块不复制 overlay 映射表：索引由调用方/预设给出。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CenterDownmixSource {
    /// 输入布局 id（报告/证据用；`new` 时可为显式配置名）。
    pub layout_id: String,
    /// planar 通道数（= 每通道帧缓冲的分段数）。
    pub channels: usize,
    /// 角色 id → planar 通道索引（不得重复角色/通道）。
    pub role_channels: Vec<(String, usize)>,
}

impl CenterDownmixSource {
    /// 显式构造并校验（role_channels 不得重复角色/通道；索引必须 < channels）。
    pub fn new(
        layout_id: impl Into<String>,
        channels: usize,
        role_channels: Vec<(String, usize)>,
    ) -> Result<Self, ContentMapError> {
        let source = Self {
            layout_id: layout_id.into(),
            channels,
            role_channels,
        };
        source.validate()?;
        Ok(source)
    }

    /// 从 overlay 输入布局预设构造（多 flow 按 streams 顺序展平）。
    pub fn from_layout(presets: &LayoutPresets, layout_id: &str) -> Result<Self, ContentMapError> {
        let layout = presets.input_layout(layout_id).ok_or_else(|| {
            ContentMapError::with_field(
                ContentMapErrorCode::LayoutUnknown,
                "input_layouts",
                format!("未知输入布局 {layout_id:?}"),
            )
        })?;
        let stream_count = layout.streams.len();
        let mut stream_offsets: Vec<usize> = Vec::with_capacity(stream_count);
        if stream_count > 0 {
            let mut offset = 0usize;
            for width in &layout.streams {
                stream_offsets.push(offset);
                offset += *width as usize;
            }
        }
        let mut role_channels: Vec<(String, usize)> = Vec::with_capacity(layout.roles.len());
        for role in &layout.roles {
            let channel = if stream_count == 0 {
                if role.stream != 0 {
                    return Err(ContentMapError::with_field(
                        ContentMapErrorCode::LayoutChannels,
                        format!("input_layouts.{layout_id}.roles.{}", role.id),
                        format!(
                            "多 flow 输入布局必须声明 streams（角色 {} 的 stream={}）",
                            role.id, role.stream
                        ),
                    ));
                }
                role.channel as usize
            } else {
                let stream = role.stream as usize;
                if stream >= stream_count {
                    return Err(ContentMapError::with_field(
                        ContentMapErrorCode::LayoutChannels,
                        format!("input_layouts.{layout_id}.roles.{}", role.id),
                        format!("stream={stream} 越界（streams 数 {stream_count}）"),
                    ));
                }
                let width = layout.streams[stream] as usize;
                if role.channel as usize >= width {
                    return Err(ContentMapError::with_field(
                        ContentMapErrorCode::LayoutChannels,
                        format!("input_layouts.{layout_id}.roles.{}", role.id),
                        format!("通道 {} 越界（stream {stream} 宽 {width}）", role.channel),
                    ));
                }
                stream_offsets[stream] + role.channel as usize
            };
            role_channels.push((role.id.clone(), channel));
        }
        let channels = if stream_count > 0 {
            let last = stream_count - 1;
            stream_offsets[last] + layout.streams[last] as usize
        } else {
            let max_channel = role_channels
                .iter()
                .map(|(_, channel)| channel + 1)
                .max()
                .unwrap_or(0);
            max_channel.max(layout.channels.unwrap_or(0) as usize)
        };
        Self::new(layout_id, channels, role_channels)
    }

    /// 某角色对应的 planar 通道索引。
    pub fn role_channel(&self, role: &str) -> Option<usize> {
        self.role_channels
            .iter()
            .find(|(id, _)| id == role)
            .map(|(_, channel)| *channel)
    }

    /// 校验角色/通道表（构造期与 `build` 前都调用；幂等）。
    pub fn validate(&self) -> Result<(), ContentMapError> {
        if self.channels == 0 {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::LayoutChannels,
                "source.channels",
                "内容源域通道数必须 >0".to_string(),
            ));
        }
        if self.role_channels.is_empty() {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::RoleMissing,
                "source.role_channels",
                format!("内容源域角色表为空（layout={}）", self.layout_id),
            ));
        }
        let mut seen_channels: Vec<usize> = Vec::with_capacity(self.role_channels.len());
        for (role, channel) in &self.role_channels {
            if role.is_empty() {
                return Err(ContentMapError::with_field(
                    ContentMapErrorCode::RoleMissing,
                    "source.role_channels",
                    "角色 id 不能为空".to_string(),
                ));
            }
            if *channel >= self.channels {
                return Err(ContentMapError::with_field(
                    ContentMapErrorCode::LayoutChannels,
                    format!("source.role_channels.{role}"),
                    format!("通道索引 {channel} 越界（channels={}）", self.channels),
                ));
            }
            if seen_channels.contains(channel) {
                return Err(ContentMapError::with_field(
                    ContentMapErrorCode::RoleChannelConflict,
                    format!("source.role_channels.{role}"),
                    format!("通道 {channel} 被多个角色占用（内容源域必须单射）"),
                ));
            }
            if self
                .role_channels
                .iter()
                .filter(|(id, _)| id == role)
                .count()
                > 1
            {
                return Err(ContentMapError::with_field(
                    ContentMapErrorCode::RoleChannelConflict,
                    format!("source.role_channels.{role}"),
                    format!("角色 {role} 重复"),
                ));
            }
            seen_channels.push(*channel);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 类型：输出目标 / 配置 / headroom
// ---------------------------------------------------------------------------

/// 输出目标描述：输出布局是否含 C 角色（= 中置音箱是否存在于该布局）。
///
/// 只取「是否有 C」这一个事实：本适配层在 plan 之前，不需要输出槽位/scatter 信息
/// （scatter 仍由 plan 自己表达）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CenterDownmixTarget {
    /// 输出布局 id（报告/证据用）。
    pub layout_id: String,
    /// 输出布局是否含 C 角色。
    pub has_center_role: bool,
}

impl CenterDownmixTarget {
    /// 显式构造。
    pub fn new(layout_id: impl Into<String>, has_center_role: bool) -> Self {
        Self {
            layout_id: layout_id.into(),
            has_center_role,
        }
    }

    /// 从 overlay 输出布局预设构造。
    pub fn from_layout(presets: &LayoutPresets, layout_id: &str) -> Result<Self, ContentMapError> {
        let layout = presets.output_layout(layout_id).ok_or_else(|| {
            ContentMapError::with_field(
                ContentMapErrorCode::LayoutUnknown,
                "output_layouts",
                format!("未知输出布局 {layout_id:?}"),
            )
        })?;
        let has_center_role = layout.roles.iter().any(|role| role.id == CENTER_ROLE_ID);
        Ok(Self::new(layout.id.clone(), has_center_role))
    }
}

/// 缺中置适配配置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDownmixConfig {
    /// 中置音箱声明（默认 Auto：由输出布局是否含 C 角色决定）。
    pub center_state: CenterSpeakerState,
    /// D7 program peak 基准（dBFS；默认 −18.0，同 `peq_fit` 默认）。
    ///
    /// 该值不是实时峰值，而是「节目素材峰值基准」：headroom 检查用它 + 最坏相干叠加
    /// 增益估算预测峰值；实际峰值/限幅仍由宿主保护节点（limiter/clip）兜底。
    pub program_peak_dbfs: f64,
}

impl Default for CenterDownmixConfig {
    fn default() -> Self {
        Self {
            center_state: CenterSpeakerState::Auto,
            program_peak_dbfs: CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
        }
    }
}

impl CenterDownmixConfig {
    /// 默认 Auto + D7 program peak 基准。
    pub fn auto() -> Self {
        Self::default()
    }

    /// 指定中置声明。
    pub fn with_center_state(mut self, center_state: CenterSpeakerState) -> Self {
        self.center_state = center_state;
        self
    }

    /// 指定 D7 program peak 基准（dBFS）。
    pub fn with_program_peak_dbfs(mut self, program_peak_dbfs: f64) -> Self {
        self.program_peak_dbfs = program_peak_dbfs;
        self
    }
}

/// headroom 检查结果（D7 口径；报告/证据用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDownmixHeadroom {
    /// 中置下混系数（dB；冻结 −3.0）。
    pub center_gain_db: f64,
    /// 中置下混系数（线性 f32）。
    pub center_gain_linear: f32,
    /// 最坏相干叠加增益（dB；20·log10(1+g)≈+4.65）。
    pub worst_case_added_db: f64,
    /// program peak 基准（dBFS）。
    pub program_peak_dbfs: f64,
    /// 未 trim 时的预测累计峰值（dBFS）= program_peak + worst_case_added。
    pub predicted_peak_dbfs: f64,
    /// D7 预测峰值上限（dBFS；−6.0）。
    pub peak_limit_dbfs: f64,
    /// D7 最小 headroom（dB；6.0）。
    pub min_headroom_db: f64,
    /// 需要施加的全局 trim（dB；0 = 不衰减，负值 = 衰减）。
    pub trim_db: f64,
    /// trim 后的预测累计峰值（dBFS；≤ peak_limit）。
    pub predicted_peak_after_trim_dbfs: f64,
    /// trim 后的 headroom（dB；≥ min_headroom）。
    pub headroom_after_trim_db: f64,
    /// 是否满足 D7 headroom 政策（trim 后）。
    pub within_policy: bool,
}

impl CenterDownmixHeadroom {
    /// 按 D7 口径计算 headroom（program peak 基准必须是有限值）。
    pub fn compute(program_peak_dbfs: f64) -> Result<Self, ContentMapError> {
        if !program_peak_dbfs.is_finite() {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::HeadroomConfig,
                "config.program_peak_dbfs",
                format!("program peak 基准必须是有限 dBFS（实得 {program_peak_dbfs}）"),
            ));
        }
        let center_gain_db = CENTER_DOWNMIX_GAIN_DB;
        let center_gain_linear = CENTER_DOWNMIX_GAIN_LINEAR;
        let worst_case_added_db = center_downmix_worst_case_added_db();
        let predicted_peak_dbfs = program_peak_dbfs + worst_case_added_db;
        let peak_limit_dbfs = CENTER_DOWNMIX_PEAK_LIMIT_DBFS;
        let min_headroom_db = CENTER_DOWNMIX_MIN_HEADROOM_DB;
        // 只在超门时衰减：trim = 上限 − 预测峰值（≤0）；否则 0（−3 dB 系数不缩水）。
        let trim_db = if predicted_peak_dbfs > peak_limit_dbfs {
            peak_limit_dbfs - predicted_peak_dbfs
        } else {
            0.0
        };
        let predicted_peak_after_trim_dbfs = predicted_peak_dbfs + trim_db;
        let headroom_after_trim_db = -predicted_peak_after_trim_dbfs;
        let within_policy = predicted_peak_after_trim_dbfs <= peak_limit_dbfs + 1e-9
            && headroom_after_trim_db >= min_headroom_db - 1e-9;
        Ok(Self {
            center_gain_db,
            center_gain_linear,
            worst_case_added_db,
            program_peak_dbfs,
            predicted_peak_dbfs,
            peak_limit_dbfs,
            min_headroom_db,
            trim_db,
            predicted_peak_after_trim_dbfs,
            headroom_after_trim_db,
            within_policy,
        })
    }

    /// trim 线性系数（f32；1.0 = 不衰减）。
    pub fn trim_linear(&self) -> f32 {
        10.0_f32.powf(self.trim_db as f32 / 20.0)
    }
}

// ---------------------------------------------------------------------------
// 计划：构建 + 应用
// ---------------------------------------------------------------------------

/// 缺中置内容适配计划（NONRT 设计期产物；apply 是零分配样本域变换）。
#[derive(Clone, Debug, PartialEq)]
pub struct CenterDownmixPlan {
    /// 状态（Applied / DisabledCenterPresent / DisabledNoCenterContent）。
    pub status: CenterDownmixStatus,
    /// 内容源域（输入布局角色→planar 通道）。
    pub source: CenterDownmixSource,
    /// 输出目标（是否含 C 角色）。
    pub target: CenterDownmixTarget,
    /// 内容 C 通道索引（Applied 时 Some）。
    pub center_channel: Option<usize>,
    /// FL 通道索引（Applied 时 Some）。
    pub left_channel: Option<usize>,
    /// FR 通道索引（Applied 时 Some）。
    pub right_channel: Option<usize>,
    /// D7 headroom 检查结果（trim 只在 Applied 时生效）。
    pub headroom: CenterDownmixHeadroom,
}

impl CenterDownmixPlan {
    /// 构建计划：仅当「输出布局无 C（中置缺席）」且「输入内容有 C」时启用。
    ///
    /// - `CenterSpeakerState::Auto`：中置存在性由 `target.has_center_role` 决定；
    /// - `Present`：输出布局无 C → `E_CHANNEL_MAP_CENTER_POLICY_CONFLICT`（声明与布局矛盾）；
    /// - `Absent`：输出布局含 C → `E_CHANNEL_MAP_CENTER_POLICY_CONFLICT`（不改有中置配置）。
    pub fn build(
        source: &CenterDownmixSource,
        target: &CenterDownmixTarget,
        config: &CenterDownmixConfig,
    ) -> Result<Self, ContentMapError> {
        source.validate()?;
        let headroom = CenterDownmixHeadroom::compute(config.program_peak_dbfs)?;
        let declared_present = match config.center_state {
            CenterSpeakerState::Auto => target.has_center_role,
            CenterSpeakerState::Present => true,
            CenterSpeakerState::Absent => false,
        };
        if declared_present && !target.has_center_role {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::CenterPolicyConflict,
                "config.center_state",
                format!(
                    "声明中置存在（{:?}）但输出布局 {} 无 C 角色（配置矛盾，拒绝静默下混）",
                    config.center_state, target.layout_id
                ),
            ));
        }
        if !declared_present && target.has_center_role {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::CenterPolicyConflict,
                "config.center_state",
                format!(
                    "输出布局 {} 含 C 角色（有中置配置），声明中置缺席（{:?}）会改变有中置配置——拒绝",
                    target.layout_id, config.center_state
                ),
            ));
        }
        let plan = |status: CenterDownmixStatus,
                    center_channel: Option<usize>,
                    left_channel: Option<usize>,
                    right_channel: Option<usize>| Self {
            status,
            source: source.clone(),
            target: target.clone(),
            center_channel,
            left_channel,
            right_channel,
            headroom,
        };
        if declared_present {
            // 有中置配置：passthrough（不改任何样本）。
            return Ok(plan(
                CenterDownmixStatus::DisabledCenterPresent,
                None,
                None,
                None,
            ));
        }
        let Some(center_channel) = source.role_channel(CENTER_ROLE_ID) else {
            return Ok(plan(
                CenterDownmixStatus::DisabledNoCenterContent,
                None,
                None,
                None,
            ));
        };
        let left_channel = source
            .role_channel(CENTER_DOWNMIX_TARGET_ROLES[0])
            .ok_or_else(|| {
                ContentMapError::with_field(
                    ContentMapErrorCode::RoleMissing,
                    "source.role_channels",
                    format!(
                        "输入布局 {} 缺 {} 角色（中置下混目标），无法启用",
                        source.layout_id, CENTER_DOWNMIX_TARGET_ROLES[0]
                    ),
                )
            })?;
        let right_channel = source
            .role_channel(CENTER_DOWNMIX_TARGET_ROLES[1])
            .ok_or_else(|| {
                ContentMapError::with_field(
                    ContentMapErrorCode::RoleMissing,
                    "source.role_channels",
                    format!(
                        "输入布局 {} 缺 {} 角色（中置下混目标），无法启用",
                        source.layout_id, CENTER_DOWNMIX_TARGET_ROLES[1]
                    ),
                )
            })?;
        if left_channel == right_channel
            || center_channel == left_channel
            || center_channel == right_channel
        {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::RoleChannelConflict,
                "source.role_channels",
                format!(
                    "C/FL/FR 通道必须互异（C={center_channel}, FL={left_channel}, FR={right_channel}）"
                ),
            ));
        }
        Ok(plan(
            CenterDownmixStatus::Applied,
            Some(center_channel),
            Some(left_channel),
            Some(right_channel),
        ))
    }

    /// 便捷构造：直接从 overlay 预设 id 构建源域/目标。
    pub fn from_layouts(
        presets: &LayoutPresets,
        input_layout_id: &str,
        output_layout_id: &str,
        config: &CenterDownmixConfig,
    ) -> Result<Self, ContentMapError> {
        let source = CenterDownmixSource::from_layout(presets, input_layout_id)?;
        let target = CenterDownmixTarget::from_layout(presets, output_layout_id)?;
        Self::build(&source, &target, config)
    }

    /// 是否启用下混。
    pub fn is_applied(&self) -> bool {
        self.status.is_applied()
    }

    /// 在 planar f32 内容块上应用计划（零堆分配、无 panic 分支；长度/索引先校验）。
    ///
    /// 语义：
    /// - `planar` 布局 = `source.channels` 个平面，每平面 `frames` 个样本，长度必须严格
    ///   等于 `source.channels × frames`；
    /// - 未启用（CenterPresent / NoCenterContent）：只校验长度，样本逐位不变；
    /// - 启用：先按 D7 headroom 结果做全局 trim（仅超门时；保持声道间相对电平），
    ///   再 `FL += g·C`、`FR += g·C`，最后 `C = 0`（消费 C 源；重复调用不会重复叠加中置）。
    ///
    /// 注意：每个内容块只应调用一次本函数（trim 是乘性增益，重复调用会重复应用 trim；
    /// C 源置零保证即使用错也不会重复混入中置）。
    pub fn apply_planar_f32_in_place(
        &self,
        planar: &mut [f32],
        frames: usize,
    ) -> Result<(), ContentMapError> {
        if frames == 0 || self.source.channels == 0 {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::BufferSize,
                "apply.frames",
                format!(
                    "frames 与源域通道数必须 >0（frames={frames}, channels={}）",
                    self.source.channels
                ),
            ));
        }
        let expected = self.source.channels.checked_mul(frames).ok_or_else(|| {
            ContentMapError::with_field(
                ContentMapErrorCode::BufferSize,
                "apply.frames",
                "channels × frames 溢出 usize".to_string(),
            )
        })?;
        if planar.len() != expected {
            return Err(ContentMapError::with_field(
                ContentMapErrorCode::BufferSize,
                "apply.planar",
                format!(
                    "planar 长度 {} != channels {} × frames {} = {expected}",
                    planar.len(),
                    self.source.channels,
                    frames
                ),
            ));
        }
        if !self.is_applied() {
            return Ok(());
        }
        let (center, left, right) =
            match (self.center_channel, self.left_channel, self.right_channel) {
                (Some(center), Some(left), Some(right)) => (center, left, right),
                _ => {
                    return Err(ContentMapError::with_field(
                        ContentMapErrorCode::RoleMissing,
                        "plan.channels",
                        "Applied 计划缺 C/FL/FR 通道索引（计划内部不一致）".to_string(),
                    ))
                }
            };
        let trim = self.headroom.trim_linear();
        if trim != 1.0 {
            for sample in planar.iter_mut() {
                *sample *= trim;
            }
        }
        let gain = self.headroom.center_gain_linear;
        let center_base = center * frames;
        let left_base = left * frames;
        let right_base = right * frames;
        let mut frame = 0usize;
        while frame < frames {
            let center_sample = planar[center_base + frame];
            planar[left_base + frame] += gain * center_sample;
            planar[right_base + frame] += gain * center_sample;
            planar[center_base + frame] = 0.0;
            frame += 1;
        }
        Ok(())
    }
}
