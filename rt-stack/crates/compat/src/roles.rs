//! [NONRT] overlay layout 预设解析 + hash 绑定 + 角色修正基线（core-v1 §11.10 / Y10–Y11） ——WBS T57 写域。
//!
//! 本模块是 rt-stack compat leaf roles（WBS T57，deps T01/T23/T13）。职责：
//!
//! 1. 唯一 overlay layout 预设解析：解析板端 audio/overlay/etc/tspi-audio/layout-presets.yml
//!    （schema tspi-layout-presets/v2，只读）。4.1.2 的唯一 role→DAC 映射由解析结果给出，
//!    不是代码里的硬编码快照：FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7、容器 ch6 空（8 槽/7 物理角色）。
//!    解析结果绑定 源文件 SHA-256 与 语义绑定 hash（layout id + 容器宽度 + 角色顺序/输出），
//!    供 T25/T26/T58/T66 做 graph/profile stale 绑定。
//! 2. Web 内嵌副本 drift 检查（compat-matrix Y10）：overlay 是唯一权威；Web 副本只用于
//!    drift 比对（check_web_template_drift）。禁止用 Web 模板替代 overlay（含 Web 模板 SUB4 旧错）：
//!    生产路径只接受 overlay 字节，drift 不一致必须显式上报/拒收，不静默采用任一副本。
//! 3. corrected 高度默认全频：corrected_baseline 对每个角色（含 HFL/HFR）返回 identity
//!    ——无 HP/LP、无人工 18 ms、无任何按 role 名注入的滤波/延迟/极性/增益。任何滤波只能来自
//!    显式配置（RoleCorrectionConfig）；未知滤波 kind 整份拒收（E_ROLES_CORRECTION_FILTER_UNKNOWN），
//!    不猜、不降级。legacy_s1 的带限/18 ms 是被否定的生成器设计，本模块的 corrected profile
//!    （corrected_fullrange_height）无法表达它（legacy 仅 T58 oracle 证据自行构造，不进本模块）。
//! 4. MIMO participants 显式：P1 4.1.2 的 5 个 participants（FL/FR/SL/SR/SUB1）来自显式
//!    MimoScope（任务边界），不是从高度 HP1800/角色频响/role 名推导；dense 0..4 → DAC
//!    0/1/2/3/7 与容器 scatter [0,1,2,3,4,5,7] 分开保存。本模块没有任何按频响/名字推导
//!    participants 的入口；显式 scope 之外的输出（如 HFL/HFR）不进入低带矩阵。
//! 5. 与 T13 接口：输出 calibration::LayoutRoleTable / RenderRoleBase（T13 只消费），
//!    供校准 layout 交叉校验与渲染基线合并；不复制 T13 的校准解析/合并语义。
//!
//! 权威契约（只读输入）：
//! - rt-stack/spec/compat-matrix.md §5 Y10–Y11（layout-presets v2 严格 schema、4.1.2 唯一映射、
//!   设备宽度校验、禁止 Web 模板 SUB4、dense 0..4→DAC 0/1/2/3/7 另表）；
//! - rt-stack/spec/core-v1.md §11.10（role 表/零填/stereo_strict）、§3（设备宽度/实际参数）；
//! - rt-stack/reference/SPEC.md §5（4.1.2 O=7/C=8/participants=[0,1,2,3,6]/scatter=[0,1,2,3,4,5,7]、
//!   高度全频、stereo_strict）；
//! - rt-stack/spec/mimo-t6.md §3.1（participants/槽位 normative；HFL/HFR 显式排除、不靠名字推断）；
//! - rt-stack/spec/fir-toolbox.md §5.1/§5.3（规范链序、无默认角色 HP/LP）；
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T57 行与 §1/§5/§7、DSP-HOST-DECISION.md §0/§1；
//! - T13 实装 crates/compat/src/calibration.rs（LayoutRoleTable/RenderRoleBase/sha256_hex）。
//!
//! # 非目标 / 边界
//! - 不设计/拟合/校验滤波系数（T10/T39/T40/T41/T64）、不算 G/P、不写 plan/ABI 字节（T12/T23/T26）；
//! - 不做 channelmap 渲染/stereo_strict 决策（T25）、不实现节目上混/下混（DEC §1）；
//! - 不碰板端/ALSA/CueMix/服务/网络；不新增依赖（纯 std + tspi-dsp-core；YAML 子集解析器为本模块
//!   私有实现——T13 的解析器是私有函数，不能跨模块调用，也不改 calibration.rs 公共 API）；
//! - 不解析/不消费 calibration YAML（T13 负责），只产出 T13 需要的 layout 角色表；
//! - 未板端/未 RT 实测（NOT_RUN，见 evidence/T57/MANIFEST.md）。
//!
//! # 数据流（供 T25/T26/T66 消费）
//!
//! ~~~text
//! overlay layout-presets.yml 字节
//!   → LayoutPresets::parse_overlay（严格 schema/未知键拒收）
//!   → bind_output_layout("4.1.2", actual_device_out_channels)
//!       → BoundLayout { container_channels=8, roles[7], layout_hash, source_sha256 }
//!           ├─ scatter() = [0,1,2,3,4,5,7]；unused_container_channels() = [6]
//!           ├─ to_layout_role_table()/render_role_bases() → T13 校准适配
//!           └─ role_mask(["FL","FR"]) → T23 allowed_role_mask（显式集合，不按名字推断）
//!   → MimoScope::p1_4_1_2_explicit().validate(&bound) → MimoParticipants（dense 0..4→DAC 0/1/2/3/7）
//!   → RoleCorrectionConfig（显式滤波；空=全频 identity）→ corrected_baseline/apply
//! ~~~

use std::fmt;
use std::path::Path;

use tspi_dsp_core::rt::{
    MAX_INPUT_CHANNELS, MAX_LOW_BAND_PARTICIPANTS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES,
};

use crate::calibration::{sha256_hex, LayoutRole, LayoutRoleTable, RenderRoleBase};

// ---------------------------------------------------------------------------
// 冻结常量（契约/容量边界；不是音频调参默认值）
// ---------------------------------------------------------------------------

/// overlay layout-presets 的唯一 schema（Y10；不匹配即拒收，不尽量解析）。
pub const LAYOUT_PRESETS_SCHEMA: &str = "tspi-layout-presets/v2";
/// P1 当前验收布局 id（WBS §3/DEC §1；布局本身仍来自 overlay 解析，不在代码里复制映射表）。
pub const TARGET_LAYOUT_4_1_2: &str = "4.1.2";
/// corrected 基线 profile id（T58 双 graph 的 corrected 侧；legacy_s1 不由此模块产出）。
pub const CORRECTED_PROFILE_ID: &str = "corrected_fullrange_height";
/// P1 4.1.2 MIMO participants（显式任务边界，WBS §3/mimo-t6 §3.1）：不是按 role 名/频响推导。
pub const P1_MIMO_PARTICIPANT_IDS: [&str; 5] = ["FL", "FR", "SL", "SR", "SUB1"];
/// layout 语义绑定 hash 的域分隔字符串（版本化；改格式必须换域名）。
pub const LAYOUT_BINDING_HASH_DOMAIN: &str = "tspi-layout-binding/v1";
/// 角色修正链 hash 的域分隔字符串。
pub const CORRECTION_HASH_DOMAIN: &str = "tspi-role-correction/v1";
/// MIMO participants 绑定 hash 的域分隔字符串。
pub const MIMO_PARTICIPANTS_HASH_DOMAIN: &str = "tspi-mimo-participants/v1";
/// 布局 id 最大字节数（防超长 key 注入报告/哈希）。
pub const MAX_LAYOUT_ID_BYTES: usize = 64;
/// 角色 id 最大字节数（与 TspiRoleV1 ctl_meta/name_hash 报告口径一致）。
pub const MAX_ROLE_ID_BYTES: usize = 64;

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// roles 模块错误码（E_ROLES_*；字段路径 + 明细，不静默降级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RolesErrorCode {
    /// 输入不是合法 UTF-8。
    Encoding,
    /// 读取 overlay 文件失败（IO）。
    Io,
    /// YAML 子集语法错误（缩进/引号/流式集合/重复 key 等）。
    YamlSyntax,
    /// 使用了本模块明确不支持的 YAML 特性（anchor/alias/tag/块标量/多文档/merge key/tab 缩进）。
    YamlUnsupported,
    /// YAML 顶层不是 mapping。
    YamlRoot,
    /// schema 缺失/不是 tspi-layout-presets/v2。
    Schema,
    /// 必填字段缺失（schema/output_layouts/roles/id/outputs 等）。
    MissingField,
    /// 字段类型错误（该是 mapping/sequence/字符串/整数而不是）。
    FieldType,
    /// 未知字段（严格 schema：拒收并报路径，不丢弃后继续）。
    FieldUnknown,
    /// 布局 id 非法（空/超长/字符集）。
    LayoutId,
    /// 引用的布局不存在（bind/remap 的 input/output）。
    LayoutUnknown,
    /// 布局 roles 为空或超过 MAX_OUTPUT_ROLES。
    LayoutRoles,
    /// 角色 id 非法/重复。
    RoleId,
    /// 角色 outputs 为空/索引越界/同一容器通道被多角色占用（scatter 单射）。
    RoleOutputs,
    /// 实际设备宽度不足（role 最大通道 index 必须 < actual 通道数）。
    DeviceWidth,
    /// MIMO participants 非法（空/超上限/重复/未知角色/布局不一致）。
    Participants,
    /// 修正 profile 缺失/未知（只接受 corrected_fullrange_height；legacy_s1 不由此模块表达）。
    CorrectionProfile,
    /// 修正配置语法错误（未知指令/缺冒号/identity 与滤波混写等）。
    CorrectionSyntax,
    /// 修正配置出现未知滤波 kind（必须显式配置已知 kind；不猜/不降级/不按 role 名注入）。
    CorrectionFilterUnknown,
    /// 修正配置引用的角色非法/重复/不在 layout 中。
    CorrectionRole,
    /// remap preset 非法（id/input/output/routes/src/dest）。
    Remap,
    /// overlay 与 Web 内嵌副本语义 drift（严格消费者用 ensure_identical）。
    DriftMismatch,
}

impl RolesErrorCode {
    /// 全部错误码（测试/报告用）。
    pub const ALL: &'static [RolesErrorCode] = &[
        RolesErrorCode::Encoding,
        RolesErrorCode::Io,
        RolesErrorCode::YamlSyntax,
        RolesErrorCode::YamlUnsupported,
        RolesErrorCode::YamlRoot,
        RolesErrorCode::Schema,
        RolesErrorCode::MissingField,
        RolesErrorCode::FieldType,
        RolesErrorCode::FieldUnknown,
        RolesErrorCode::LayoutId,
        RolesErrorCode::LayoutUnknown,
        RolesErrorCode::LayoutRoles,
        RolesErrorCode::RoleId,
        RolesErrorCode::RoleOutputs,
        RolesErrorCode::DeviceWidth,
        RolesErrorCode::Participants,
        RolesErrorCode::CorrectionProfile,
        RolesErrorCode::CorrectionSyntax,
        RolesErrorCode::CorrectionFilterUnknown,
        RolesErrorCode::CorrectionRole,
        RolesErrorCode::Remap,
        RolesErrorCode::DriftMismatch,
    ];

    /// 稳定错误码字符串（E_ROLES_*；供 REST/报告映射，不随文案变化）。
    pub fn as_str(self) -> &'static str {
        match self {
            RolesErrorCode::Encoding => "E_ROLES_ENCODING",
            RolesErrorCode::Io => "E_ROLES_IO",
            RolesErrorCode::YamlSyntax => "E_ROLES_YAML_SYNTAX",
            RolesErrorCode::YamlUnsupported => "E_ROLES_YAML_UNSUPPORTED",
            RolesErrorCode::YamlRoot => "E_ROLES_YAML_ROOT",
            RolesErrorCode::Schema => "E_ROLES_SCHEMA",
            RolesErrorCode::MissingField => "E_ROLES_MISSING_FIELD",
            RolesErrorCode::FieldType => "E_ROLES_FIELD_TYPE",
            RolesErrorCode::FieldUnknown => "E_ROLES_FIELD_UNKNOWN",
            RolesErrorCode::LayoutId => "E_ROLES_LAYOUT_ID",
            RolesErrorCode::LayoutUnknown => "E_ROLES_LAYOUT_UNKNOWN",
            RolesErrorCode::LayoutRoles => "E_ROLES_LAYOUT_ROLES",
            RolesErrorCode::RoleId => "E_ROLES_ROLE_ID",
            RolesErrorCode::RoleOutputs => "E_ROLES_ROLE_OUTPUTS",
            RolesErrorCode::DeviceWidth => "E_ROLES_DEVICE_WIDTH",
            RolesErrorCode::Participants => "E_ROLES_PARTICIPANTS",
            RolesErrorCode::CorrectionProfile => "E_ROLES_CORRECTION_PROFILE",
            RolesErrorCode::CorrectionSyntax => "E_ROLES_CORRECTION_SYNTAX",
            RolesErrorCode::CorrectionFilterUnknown => "E_ROLES_CORRECTION_FILTER_UNKNOWN",
            RolesErrorCode::CorrectionRole => "E_ROLES_CORRECTION_ROLE",
            RolesErrorCode::Remap => "E_ROLES_REMAP",
            RolesErrorCode::DriftMismatch => "E_ROLES_DRIFT_MISMATCH",
        }
    }
}

/// roles 模块错误（code + 字段路径 + 明细；不 panic、不泄露内部路径之外的信息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolesError {
    /// 稳定错误码。
    pub code: RolesErrorCode,
    /// 字段路径（例如 output_layouts.4.1.2.roles[6].outputs）。
    pub field: String,
    /// 人读明细（含行号/实际值）。
    pub detail: String,
}

impl RolesError {
    /// 构造（无字段路径）。
    pub fn new(code: RolesErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: String::new(),
            detail: detail.into(),
        }
    }

    /// 构造（带字段路径）。
    pub fn with_field(
        code: RolesErrorCode,
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

impl fmt::Display for RolesError {
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

impl std::error::Error for RolesError {}

// ---------------------------------------------------------------------------
// YAML 子集解析（私有；严格，未知特性/重复 key 拒收）
// ---------------------------------------------------------------------------

/// 本模块需要的 YAML 子集值（layout-presets v2 只用到 mapping/sequence/标量）。
#[derive(Clone, Debug, PartialEq)]
enum Yaml {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Seq(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    fn type_name(&self) -> &'static str {
        match self {
            Yaml::Null => "null",
            Yaml::Bool(_) => "bool",
            Yaml::Int(_) => "int",
            Yaml::Float(_) => "float",
            Yaml::Str(_) => "string",
            Yaml::Seq(_) => "sequence",
            Yaml::Map(_) => "mapping",
        }
    }

    fn as_map(&self) -> Option<&[(String, Yaml)]> {
        match self {
            Yaml::Map(fields) => Some(fields),
            _ => None,
        }
    }

    fn as_seq(&self) -> Option<&[Yaml]> {
        match self {
            Yaml::Seq(items) => Some(items),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Str(value) => Some(value),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Yaml::Int(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct YamlLine {
    indent: usize,
    text: String,
    number: usize,
}

fn yaml_syntax(number: usize, detail: impl Into<String>) -> RolesError {
    RolesError::with_field(
        RolesErrorCode::YamlSyntax,
        format!("line {number}"),
        detail.into(),
    )
}

fn yaml_unsupported(number: usize, detail: impl Into<String>) -> RolesError {
    RolesError::with_field(
        RolesErrorCode::YamlUnsupported,
        format!("line {number}"),
        detail.into(),
    )
}

/// 解析 UTF-8 字节（BOM 容忍；非 UTF-8 = E_ROLES_ENCODING）。
fn parse_yaml(bytes: &[u8]) -> Result<Yaml, RolesError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        RolesError::new(
            RolesErrorCode::Encoding,
            format!("输入不是合法 UTF-8：{error}"),
        )
    })?;
    parse_yaml_text(text)
}

fn parse_yaml_text(text: &str) -> Result<Yaml, RolesError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<YamlLine> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let stripped = strip_comment(raw, number)?;
        let trimmed_end = stripped.trim_end();
        if trimmed_end.trim().is_empty() {
            continue;
        }
        let indent = trimmed_end.len() - trimmed_end.trim_start().len();
        if trimmed_end[..indent].contains('\t') {
            return Err(yaml_unsupported(number, "缩进含 tab（YAML 禁止 tab 缩进）"));
        }
        let content = trimmed_end[indent..].to_string();
        if content.starts_with("---") || content.starts_with("...") {
            return Err(yaml_unsupported(number, "多文档/文档标记（---/...）不支持"));
        }
        if content.starts_with('%') {
            return Err(yaml_unsupported(number, "YAML 指令（%）不支持"));
        }
        if content.starts_with('&') || content.starts_with('*') || content.starts_with('!') {
            return Err(yaml_unsupported(number, "anchor/alias/tag 不支持"));
        }
        if content.starts_with("<<") {
            return Err(yaml_unsupported(number, "merge key（<<）不支持"));
        }
        lines.push(YamlLine {
            indent,
            text: content,
            number,
        });
    }
    if lines.is_empty() {
        return Ok(Yaml::Null);
    }
    let mut position = 0usize;
    let first_indent = lines[0].indent;
    let value = parse_block(&lines, &mut position, first_indent)?;
    if position != lines.len() {
        return Err(yaml_syntax(
            lines[position].number,
            "存在无法归属的剩余行（缩进不一致）",
        ));
    }
    Ok(value)
}

/// 去掉行内注释（# 前必须是空白或行首，且不在引号内）；引号未闭合 = 语法错误。
fn strip_comment(raw: &str, number: usize) -> Result<String, RolesError> {
    let chars: Vec<char> = raw.chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut comment_at: Option<usize> = None;
    let mut byte_index = 0usize;
    for (index, &character) in chars.iter().enumerate() {
        let previous = if index == 0 {
            None
        } else {
            Some(chars[index - 1])
        };
        if in_double {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_double = false;
            }
            byte_index += character.len_utf8();
            continue;
        }
        if in_single {
            if character == '\'' {
                in_single = false;
            }
            byte_index += character.len_utf8();
            continue;
        }
        match character {
            '"' if is_token_start(previous) => in_double = true,
            '\'' if is_token_start(previous) => in_single = true,
            '#' if previous.is_none() || matches!(previous, Some(' ') | Some('\t')) => {
                comment_at = Some(byte_index);
                break;
            }
            _ => {}
        }
        byte_index += character.len_utf8();
    }
    if in_double || in_single {
        return Err(yaml_syntax(number, "引号字符串未闭合"));
    }
    Ok(match comment_at {
        Some(index) => raw[..index].to_string(),
        None => raw.to_string(),
    })
}

fn is_token_start(previous: Option<char>) -> bool {
    matches!(
        previous,
        None | Some(' ') | Some('\t') | Some('{') | Some('[') | Some(',') | Some(':') | Some('-')
    )
}

fn parse_block(
    lines: &[YamlLine],
    position: &mut usize,
    indent: usize,
) -> Result<Yaml, RolesError> {
    let line = lines
        .get(*position)
        .ok_or_else(|| yaml_syntax(0, "内部错误：空 block"))?;
    if line.indent != indent {
        return Err(yaml_syntax(
            line.number,
            format!("缩进不一致：期望 {indent}，实际 {}", line.indent),
        ));
    }
    if is_seq_line(line, indent) {
        parse_seq(lines, position, indent)
    } else {
        parse_map(lines, position, indent)
    }
}

fn is_seq_line(line: &YamlLine, indent: usize) -> bool {
    line.indent == indent && (line.text == "-" || line.text.starts_with("- "))
}

fn parse_map(lines: &[YamlLine], position: &mut usize, indent: usize) -> Result<Yaml, RolesError> {
    let mut fields: Vec<(String, Yaml)> = Vec::new();
    while *position < lines.len() {
        let line = &lines[*position];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(yaml_syntax(line.number, "缩进超出映射层级"));
        }
        if is_seq_line(line, indent) {
            return Err(yaml_syntax(line.number, "映射中出现序列项（缺少父 key）"));
        }
        let (key, rest) = split_block_key(&line.text, line.number)?;
        if fields.iter().any(|(existing, _)| existing == &key) {
            return Err(yaml_syntax(line.number, format!("重复 key：{key}")));
        }
        let number = line.number;
        *position += 1;
        let rest_trimmed = rest.trim();
        let value = if rest_trimmed.is_empty() {
            if *position < lines.len() && lines[*position].indent > indent {
                let child_indent = lines[*position].indent;
                parse_block(lines, position, child_indent)?
            } else {
                Yaml::Null
            }
        } else {
            parse_flow_value(rest_trimmed, number)?
        };
        fields.push((key, value));
    }
    Ok(Yaml::Map(fields))
}

fn parse_seq(lines: &[YamlLine], position: &mut usize, indent: usize) -> Result<Yaml, RolesError> {
    let mut items: Vec<Yaml> = Vec::new();
    while *position < lines.len() {
        let line = &lines[*position];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(yaml_syntax(line.number, "序列项缩进超出层级"));
        }
        if !is_seq_line(line, indent) {
            break;
        }
        let number = line.number;
        let rest = &line.text[1..];
        if rest.trim().is_empty() {
            *position += 1;
            let value = if *position < lines.len() && lines[*position].indent > indent {
                let child_indent = lines[*position].indent;
                parse_block(lines, position, child_indent)?
            } else {
                Yaml::Null
            };
            items.push(value);
            continue;
        }
        if !rest.starts_with(' ') && !rest.starts_with('\t') {
            return Err(yaml_syntax(number, "序列项 '-' 后必须跟空格"));
        }
        let trimmed = rest.trim_start();
        let leading = rest.len() - trimmed.len();
        let content_indent = indent + 1 + leading;
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            *position += 1;
            items.push(parse_flow_value(trimmed, number)?);
        } else if is_block_mapping_item(trimmed) {
            let mut item_lines = vec![YamlLine {
                indent: content_indent,
                text: trimmed.to_string(),
                number,
            }];
            *position += 1;
            while *position < lines.len() && lines[*position].indent > indent {
                item_lines.push(lines[*position].clone());
                *position += 1;
            }
            let mut item_position = 0usize;
            let value = parse_block(&item_lines, &mut item_position, content_indent)?;
            if item_position != item_lines.len() {
                return Err(yaml_syntax(
                    item_lines[item_position].number,
                    "序列项内存在无法归属的行",
                ));
            }
            items.push(value);
        } else {
            *position += 1;
            items.push(parse_flow_value(trimmed, number)?);
        }
    }
    Ok(Yaml::Seq(items))
}

/// 序列项是否以 block mapping 开始（- key: value；冒号后必须是空白/行尾，且不在引号内）。
fn is_block_mapping_item(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (index, &character) in chars.iter().enumerate() {
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
        let previous = if index == 0 {
            None
        } else {
            Some(chars[index - 1])
        };
        match character {
            '"' if is_token_start(previous) => in_double = true,
            '\'' if is_token_start(previous) => in_single = true,
            ':' => {
                let next = chars.get(index + 1).copied();
                return next.is_none() || next == Some(' ') || next == Some('\t');
            }
            _ => {}
        }
    }
    false
}

fn split_block_key(text: &str, number: usize) -> Result<(String, &str), RolesError> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for &(byte_index, character) in &chars {
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
            ':' => {
                let raw_key = text[..byte_index].trim();
                let key = unquote_key(raw_key, number)?;
                if key.is_empty() {
                    return Err(yaml_syntax(number, "key 为空"));
                }
                return Ok((key, &text[byte_index + 1..]));
            }
            _ => {}
        }
    }
    Err(yaml_syntax(number, "缺少 ':'（不是合法 mapping 行）"))
}

fn unquote_key(raw: &str, number: usize) -> Result<String, RolesError> {
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        let mut parser = FlowParser::new(raw, number);
        let value = parser.parse_quoted()?;
        parser.skip_whitespace();
        if parser.peek().is_some() {
            return Err(yaml_syntax(number, "quoted key 后存在多余字符"));
        }
        Ok(value)
    } else {
        Ok(raw.to_string())
    }
}

/// 流式值解析（{...}/[...]/引号/plain 标量；必须消费整段文本）。
fn parse_flow_value(text: &str, number: usize) -> Result<Yaml, RolesError> {
    let mut parser = FlowParser::new(text, number);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.peek().is_some() {
        return Err(yaml_syntax(number, "值后存在多余字符"));
    }
    Ok(value)
}

struct FlowParser {
    chars: Vec<char>,
    position: usize,
    line: usize,
}

impl FlowParser {
    fn new(text: &str, line: usize) -> Self {
        Self {
            chars: text.chars().collect(),
            position: 0,
            line,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let value = self.peek();
        if value.is_some() {
            self.position += 1;
        }
        value
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.position += 1;
        }
    }

    fn error(&self, detail: impl Into<String>) -> RolesError {
        yaml_syntax(self.line, detail)
    }

    fn parse_value(&mut self) -> Result<Yaml, RolesError> {
        self.skip_whitespace();
        match self.peek() {
            Some('{') => self.parse_flow_map(),
            Some('[') => self.parse_flow_seq(),
            Some('\'') | Some('"') => Ok(Yaml::Str(self.parse_quoted()?)),
            Some(_) => self.parse_plain_value(),
            None => Err(self.error("流式值缺失")),
        }
    }

    fn parse_flow_map(&mut self) -> Result<Yaml, RolesError> {
        self.bump();
        let mut fields: Vec<(String, Yaml)> = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some('}') => {
                    self.bump();
                    break;
                }
                None => return Err(self.error("flow mapping 未闭合（缺 '}'）")),
                _ => {}
            }
            let key = self.parse_key()?;
            self.skip_whitespace();
            if self.bump() != Some(':') {
                return Err(self.error("flow mapping key 后缺 ':'"));
            }
            let value = self.parse_value()?;
            if fields.iter().any(|(existing, _)| existing == &key) {
                return Err(self.error(format!("flow mapping 重复 key：{key}")));
            }
            fields.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some('}') => {
                    self.bump();
                    break;
                }
                None => return Err(self.error("flow mapping 未闭合（缺 '}'）")),
                Some(character) => {
                    return Err(self.error(format!("flow mapping 非法字符 '{character}'")));
                }
            }
        }
        Ok(Yaml::Map(fields))
    }

    fn parse_flow_seq(&mut self) -> Result<Yaml, RolesError> {
        self.bump();
        let mut items: Vec<Yaml> = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(']') => {
                    self.bump();
                    break;
                }
                None => return Err(self.error("flow sequence 未闭合（缺 ']'）")),
                _ => {}
            }
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {
                    self.bump();
                    break;
                }
                None => return Err(self.error("flow sequence 未闭合（缺 ']'）")),
                Some(character) => {
                    return Err(self.error(format!("flow sequence 非法字符 '{character}'")));
                }
            }
        }
        Ok(Yaml::Seq(items))
    }

    fn parse_key(&mut self) -> Result<String, RolesError> {
        match self.peek() {
            Some('\'') | Some('"') => self.parse_quoted(),
            _ => {
                let start = self.position;
                while let Some(character) = self.peek() {
                    if matches!(character, ':' | ',' | '}' | '{' | '[' | ']') {
                        break;
                    }
                    self.position += 1;
                }
                let raw: String = self.chars[start..self.position].iter().collect();
                let key = raw.trim().to_string();
                if key.is_empty() {
                    return Err(self.error("flow mapping key 为空"));
                }
                Ok(key)
            }
        }
    }

    fn parse_quoted(&mut self) -> Result<String, RolesError> {
        let quote = self.bump().ok_or_else(|| self.error("引号缺失"))?;
        let mut output = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error("引号字符串未闭合")),
                Some(character) if character == quote => {
                    if quote == '\'' && self.peek() == Some('\'') {
                        self.bump();
                        output.push('\'');
                        continue;
                    }
                    return Ok(output);
                }
                Some('\\') if quote == '"' => {
                    let escaped = self.bump().ok_or_else(|| self.error("转义序列不完整"))?;
                    match escaped {
                        'n' => output.push('\n'),
                        't' => output.push('\t'),
                        'r' => output.push('\r'),
                        '"' => output.push('"'),
                        '\\' => output.push('\\'),
                        '/' => output.push('/'),
                        other => return Err(self.error(format!("不支持的转义 \\{other}"))),
                    }
                }
                Some(character) => output.push(character),
            }
        }
    }

    fn parse_plain_value(&mut self) -> Result<Yaml, RolesError> {
        let start = self.position;
        while let Some(character) = self.peek() {
            if matches!(character, ',' | ']' | '}') {
                break;
            }
            self.position += 1;
        }
        let raw: String = self.chars[start..self.position].iter().collect();
        let text = raw.trim();
        if text.is_empty() {
            return Err(self.error("flow 标量为空"));
        }
        if text.starts_with('|') || text.starts_with('>') {
            return Err(yaml_unsupported(self.line, "块标量（|/>）不支持"));
        }
        if text.starts_with('&') || text.starts_with('*') || text.starts_with('!') {
            return Err(yaml_unsupported(self.line, "anchor/alias/tag 不支持"));
        }
        Ok(plain_scalar(text))
    }
}

fn plain_scalar(text: &str) -> Yaml {
    match text {
        "null" | "Null" | "NULL" | "~" => Yaml::Null,
        "true" | "True" | "TRUE" => Yaml::Bool(true),
        "false" | "False" | "FALSE" => Yaml::Bool(false),
        _ => {
            if let Ok(value) = text.parse::<i64>() {
                Yaml::Int(value)
            } else if let Ok(value) = text.parse::<f64>() {
                if value.is_finite() {
                    Yaml::Float(value)
                } else {
                    Yaml::Str(text.to_string())
                }
            } else {
                Yaml::Str(text.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// YAML 取值辅助（字段路径 + 严格类型）
// ---------------------------------------------------------------------------

fn map_get<'a>(fields: &'a [(String, Yaml)], key: &str) -> Option<&'a Yaml> {
    fields
        .iter()
        .find(|(existing, _)| existing == key)
        .map(|(_, value)| value)
}

fn require_map<'a>(value: &'a Yaml, field: &str) -> Result<&'a [(String, Yaml)], RolesError> {
    value.as_map().ok_or_else(|| {
        RolesError::with_field(
            RolesErrorCode::FieldType,
            field,
            format!("应为 mapping，实际 {}", value.type_name()),
        )
    })
}

fn require_seq<'a>(value: &'a Yaml, field: &str) -> Result<&'a [Yaml], RolesError> {
    value.as_seq().ok_or_else(|| {
        RolesError::with_field(
            RolesErrorCode::FieldType,
            field,
            format!("应为 sequence，实际 {}", value.type_name()),
        )
    })
}

fn required_str<'a>(
    fields: &'a [(String, Yaml)],
    key: &str,
    field: &str,
) -> Result<&'a str, RolesError> {
    let value = map_get(fields, key).ok_or_else(|| {
        RolesError::with_field(
            RolesErrorCode::MissingField,
            format!("{field}.{key}"),
            format!("缺少必填字段 {key}"),
        )
    })?;
    value.as_str().ok_or_else(|| {
        RolesError::with_field(
            RolesErrorCode::FieldType,
            format!("{field}.{key}"),
            format!("应为字符串，实际 {}", value.type_name()),
        )
    })
}

fn optional_str(
    fields: &[(String, Yaml)],
    key: &str,
    field: &str,
) -> Result<Option<String>, RolesError> {
    match map_get(fields, key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::FieldType,
                    format!("{field}.{key}"),
                    format!("应为字符串，实际 {}", value.type_name()),
                )
            }),
    }
}

fn require_int(value: &Yaml, field: &str) -> Result<i64, RolesError> {
    value.as_i64().ok_or_else(|| {
        RolesError::with_field(
            RolesErrorCode::FieldType,
            field,
            format!("应为整数，实际 {}", value.type_name()),
        )
    })
}

fn check_unknown_keys(
    fields: &[(String, Yaml)],
    allowed: &[&str],
    field: &str,
) -> Result<(), RolesError> {
    for (key, _) in fields {
        if !allowed.contains(&key.as_str()) {
            return Err(RolesError::with_field(
                RolesErrorCode::FieldUnknown,
                format!("{field}.{key}"),
                format!("未知字段（严格 schema：拒收，不丢弃后继续）；允许 {allowed:?}"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// id 校验
// ---------------------------------------------------------------------------

fn validate_layout_id(id: &str, field: &str) -> Result<(), RolesError> {
    if id.is_empty() || id.len() > MAX_LAYOUT_ID_BYTES {
        return Err(RolesError::with_field(
            RolesErrorCode::LayoutId,
            field,
            format!("布局 id 非法（空或超过 {MAX_LAYOUT_ID_BYTES} 字节）：{id:?}"),
        ));
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        return Err(RolesError::with_field(
            RolesErrorCode::LayoutId,
            field,
            format!("布局 id 字符集非法（只允许 ASCII 字母数字 . _ -）：{id:?}"),
        ));
    }
    Ok(())
}

fn validate_role_id(id: &str, field: &str) -> Result<(), RolesError> {
    if id.is_empty() || id.len() > MAX_ROLE_ID_BYTES {
        return Err(RolesError::with_field(
            RolesErrorCode::RoleId,
            field,
            format!("角色 id 非法（空或超过 {MAX_ROLE_ID_BYTES} 字节）：{id:?}"),
        ));
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(RolesError::with_field(
            RolesErrorCode::RoleId,
            field,
            format!("角色 id 字符集非法（只允许 ASCII 字母数字 _ -）：{id:?}"),
        ));
    }
    Ok(())
}

fn validate_remap_id(id: &str, field: &str) -> Result<(), RolesError> {
    if id.is_empty() || id.len() > MAX_LAYOUT_ID_BYTES * 2 {
        return Err(RolesError::with_field(
            RolesErrorCode::Remap,
            field,
            format!("remap preset id 非法（空或过长）：{id:?}"),
        ));
    }
    if !id.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '>')
    }) {
        return Err(RolesError::with_field(
            RolesErrorCode::Remap,
            field,
            format!("remap preset id 字符集非法（只允许 ASCII 字母数字 . _ - >）：{id:?}"),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 类型化预设
// ---------------------------------------------------------------------------

/// 输入布局角色（capture 侧；stream 默认 0）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputLayoutRole {
    /// 角色 id。
    pub id: String,
    /// 显示名（可选）。
    pub label: Option<String>,
    /// 流序号（H16 多 flow；缺省 0）。
    pub stream: u16,
    /// 流内 0-based 通道号。
    pub channel: u16,
}

/// 输入布局预设（input_layouts.&lt;id&gt;）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputLayoutPreset {
    /// 布局 id。
    pub id: String,
    /// 显示名（可选）。
    pub label: Option<String>,
    /// 描述（可选）。
    pub description: Option<String>,
    /// 声明通道数；None = auto（以实测为准，Y03）。
    pub channels: Option<u16>,
    /// 声明的 flow 通道数列表（可选）。
    pub streams: Vec<u16>,
    /// 角色表（文件顺序）。
    pub roles: Vec<InputLayoutRole>,
}

/// 输出布局角色（playback 侧；outputs 是容器/槽位 0-based 索引，不是设备物理口名）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLayoutRole {
    /// 角色 id。
    pub id: String,
    /// 显示名（可选）。
    pub label: Option<String>,
    /// 该角色占用的容器槽位（非空；同一布局内必须单射）。
    pub outputs: Vec<u16>,
}

/// 输出布局预设（output_layouts.&lt;id&gt;）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLayoutPreset {
    /// 布局 id。
    pub id: String,
    /// 显示名（可选）。
    pub label: Option<String>,
    /// 描述（可选）。
    pub description: Option<String>,
    /// 角色表（文件顺序 = 角色索引顺序；不含空槽）。
    pub roles: Vec<OutputLayoutRole>,
}

/// remap 单条路由（src 输入角色 id → dest 输出角色 id）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemapRoute {
    /// 输入角色 id。
    pub src: String,
    /// 输出角色 id。
    pub dest: String,
}

/// remap 预设（remap_presets.&lt;id&gt;）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemapPreset {
    /// 预设 id（形如 5.1.2-&gt;5.1.2）。
    pub id: String,
    /// 显示名（可选）。
    pub label: Option<String>,
    /// 输入布局 id。
    pub input: String,
    /// 输出布局 id。
    pub output: String,
    /// 路由表（文件顺序）。
    pub routes: Vec<RemapRoute>,
}

/// 解析后的 overlay layout 预设文档（tspi-layout-presets/v2）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPresets {
    schema: String,
    input_layouts: Vec<InputLayoutPreset>,
    output_layouts: Vec<OutputLayoutPreset>,
    remap_presets: Vec<RemapPreset>,
    source_sha256: String,
}

impl LayoutPresets {
    /// 解析 overlay 预设字节（唯一权威入口；Web 内嵌副本不得用于生产绑定）。
    ///
    /// 严格性（Y10/Y22）：schema 精确匹配；未知顶层/嵌套字段拒收；重复 key/角色 id/
    /// 容器槽位拒收；输出槽位必须 &lt; MAX_OUTPUT_CHANNELS；角色数 &lt;= MAX_OUTPUT_ROLES。
    pub fn parse_overlay(bytes: &[u8]) -> Result<Self, RolesError> {
        let source_sha256 = sha256_hex(bytes);
        let root = parse_yaml(bytes)?;
        let root_fields = match root.as_map() {
            Some(fields) => fields,
            None => {
                return Err(RolesError::with_field(
                    RolesErrorCode::YamlRoot,
                    "$",
                    format!("顶层必须是 mapping，实际 {}", root.type_name()),
                ))
            }
        };
        check_unknown_keys(
            root_fields,
            &["schema", "input_layouts", "output_layouts", "remap_presets"],
            "$",
        )?;
        let schema = required_str(root_fields, "schema", "$")?.to_string();
        if schema != LAYOUT_PRESETS_SCHEMA {
            return Err(RolesError::with_field(
                RolesErrorCode::Schema,
                "schema",
                format!(
                    "必须为 {LAYOUT_PRESETS_SCHEMA}，实际 {schema:?}（不匹配即拒收，不尽量解析）"
                ),
            ));
        }
        let output_layouts = match map_get(root_fields, "output_layouts") {
            Some(value) => parse_output_layouts(value)?,
            None => {
                return Err(RolesError::with_field(
                    RolesErrorCode::MissingField,
                    "output_layouts",
                    "缺少 output_layouts 段",
                ))
            }
        };
        if output_layouts.is_empty() {
            return Err(RolesError::with_field(
                RolesErrorCode::LayoutId,
                "output_layouts",
                "至少需要一个输出布局",
            ));
        }
        let input_layouts = match map_get(root_fields, "input_layouts") {
            Some(value) => parse_input_layouts(value)?,
            None => Vec::new(),
        };
        let remap_presets = match map_get(root_fields, "remap_presets") {
            Some(value) => parse_remap_presets(value, &input_layouts, &output_layouts)?,
            None => Vec::new(),
        };
        Ok(Self {
            schema,
            input_layouts,
            output_layouts,
            remap_presets,
            source_sha256,
        })
    }

    /// 从文件读取并解析 overlay 预设（只读；IO 失败 = E_ROLES_IO）。
    pub fn load_overlay_file(path: &Path) -> Result<Self, RolesError> {
        let bytes = std::fs::read(path).map_err(|error| {
            RolesError::with_field(
                RolesErrorCode::Io,
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

    /// 源文件字节 SHA-256（小写 hex；overlay 版本绑定证据）。
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    /// 输入布局表（文件顺序）。
    pub fn input_layouts(&self) -> &[InputLayoutPreset] {
        &self.input_layouts
    }

    /// 输出布局表（文件顺序）。
    pub fn output_layouts(&self) -> &[OutputLayoutPreset] {
        &self.output_layouts
    }

    /// remap 预设表（文件顺序）。
    pub fn remap_presets(&self) -> &[RemapPreset] {
        &self.remap_presets
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

    /// 按 id 查 remap 预设。
    pub fn remap_preset(&self, preset_id: &str) -> Option<&RemapPreset> {
        self.remap_presets
            .iter()
            .find(|preset| preset.id == preset_id)
    }

    /// 输出布局的语义绑定 hash（layout id + 容器宽度 + 角色顺序/输出；不含源字节 hash）。
    pub fn layout_hash(&self, layout_id: &str) -> Result<String, RolesError> {
        let layout = self.output_layout(layout_id).ok_or_else(|| {
            RolesError::with_field(
                RolesErrorCode::LayoutUnknown,
                format!("output_layouts.{layout_id}"),
                "未知输出布局",
            )
        })?;
        Ok(layout_binding_hash(&self.schema, layout))
    }

    /// 绑定输出布局到实际播放设备宽度，产出 BoundLayout（角色索引 = 文件顺序）。
    ///
    /// 容器宽度 = max(outputs)+1（4.1.2 → 8，ch6 空）；设备实际通道数必须 &gt;= 容器宽度
    /// 且 &gt; 最大槽位 index（Y11/A18：解析 + 实际设备宽度校验，禁止硬编码快照）。
    pub fn bind_output_layout(
        &self,
        layout_id: &str,
        device_output_channels: u16,
    ) -> Result<BoundLayout, RolesError> {
        let preset = self.output_layout(layout_id).ok_or_else(|| {
            RolesError::with_field(
                RolesErrorCode::LayoutUnknown,
                format!("output_layouts.{layout_id}"),
                "未知输出布局",
            )
        })?;
        let container_channels = container_channels_for(preset);
        let roles: Vec<BoundRole> = preset
            .roles
            .iter()
            .enumerate()
            .map(|(index, role)| BoundRole {
                role_index: index as u16,
                id: role.id.clone(),
                outputs: role.outputs.clone(),
            })
            .collect();
        let bound = BoundLayout {
            layout_id: layout_id.to_string(),
            container_channels,
            roles,
            source_sha256: self.source_sha256.clone(),
            layout_hash: layout_binding_hash(&self.schema, preset),
        };
        bound.validate_output_device_width(device_output_channels)?;
        Ok(bound)
    }

    /// 绑定 P1 当前验收布局 4.1.2（映射仍来自解析结果，不是硬编码）。
    pub fn bind_4_1_2(&self, device_output_channels: u16) -> Result<BoundLayout, RolesError> {
        self.bind_output_layout(TARGET_LAYOUT_4_1_2, device_output_channels)
    }
}

fn container_channels_for(layout: &OutputLayoutPreset) -> u16 {
    let max_output = layout
        .roles
        .iter()
        .flat_map(|role| role.outputs.iter().copied())
        .max()
        .unwrap_or(0);
    max_output.saturating_add(1)
}

fn layout_binding_hash(schema: &str, layout: &OutputLayoutPreset) -> String {
    let mut canonical = String::new();
    canonical.push_str(LAYOUT_BINDING_HASH_DOMAIN);
    canonical.push('\n');
    canonical.push_str(&format!("schema={schema}\n"));
    canonical.push_str(&format!("layout={}\n", layout.id));
    canonical.push_str(&format!("container={}\n", container_channels_for(layout)));
    for (index, role) in layout.roles.iter().enumerate() {
        canonical.push_str(&format!("role{index}={}", role.id));
        for output in &role.outputs {
            canonical.push_str(&format!(":{output}"));
        }
        canonical.push('\n');
    }
    sha256_hex(canonical.as_bytes())
}

/// 输出布局解析后的绑定结果（role 索引 = 文件顺序；容器槽位显式）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundRole {
    /// 角色索引（0..O-1；与 TspiRoleV1.role_id/T23 roles[i] 对齐）。
    pub role_index: u16,
    /// 角色 id（只作 CTL 元数据/name_hash；不参与滤波/路由决策）。
    pub id: String,
    /// 容器槽位（非空；同一布局内单射）。
    pub outputs: Vec<u16>,
}

/// 绑定到设备宽度的输出布局。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundLayout {
    /// 布局 id。
    pub layout_id: String,
    /// 播放容器槽位数（4.1.2 = 8；ch6 空）。
    pub container_channels: u16,
    /// 角色表（文件顺序）。
    pub roles: Vec<BoundRole>,
    /// overlay 源文件字节 SHA-256（版本证据；drift 时与 Web 副本对比）。
    pub source_sha256: String,
    /// 语义绑定 hash（layout id/容器/角色顺序/输出）。
    pub layout_hash: String,
}

impl BoundLayout {
    /// 角色数 O。
    pub fn role_count(&self) -> usize {
        self.roles.len()
    }

    /// 按 id 查角色索引。
    pub fn role_index(&self, role_id: &str) -> Option<u16> {
        self.roles
            .iter()
            .find(|role| role.id == role_id)
            .map(|role| role.role_index)
    }

    /// 按 id 查角色。
    pub fn role(&self, role_id: &str) -> Option<&BoundRole> {
        self.roles.iter().find(|role| role.id == role_id)
    }

    /// 按索引查角色。
    pub fn role_by_index(&self, role_index: u16) -> Option<&BoundRole> {
        self.roles.iter().find(|role| role.role_index == role_index)
    }

    /// 角色首个容器槽位（T13 LayoutRoleTable::first_output 口径）。
    pub fn first_output(&self, role_id: &str) -> Option<u16> {
        self.role(role_id)
            .and_then(|role| role.outputs.first().copied())
    }

    /// 容器 scatter（按角色顺序的首槽位；4.1.2 = [0,1,2,3,4,5,7]）。
    pub fn scatter(&self) -> Vec<u16> {
        self.roles
            .iter()
            .filter_map(|role| role.outputs.first().copied())
            .collect()
    }

    /// 被角色占用的容器槽位（升序）。
    pub fn used_container_channels(&self) -> Vec<u16> {
        let mut channels: Vec<u16> = self
            .roles
            .iter()
            .flat_map(|role| role.outputs.iter().copied())
            .collect();
        channels.sort_unstable();
        channels.dedup();
        channels
    }

    /// 未被任何角色占用的容器槽位（显式零填；4.1.2 = [6]）。
    pub fn unused_container_channels(&self) -> Vec<u16> {
        let used = self.used_container_channels();
        (0..self.container_channels)
            .filter(|channel| !used.contains(channel))
            .collect()
    }

    /// 设备宽度要求（T17 RequiredDeviceWidth 的 compat 侧等价物；T26 负责转换）。
    pub fn required_device_width(&self) -> LayoutDeviceWidth {
        let max_output_channel_index = self
            .roles
            .iter()
            .flat_map(|role| role.outputs.iter().copied())
            .max()
            .unwrap_or(0);
        LayoutDeviceWidth {
            min_output_channels: self.container_channels,
            max_output_channel_index,
        }
    }

    /// 校验实际播放设备宽度（Y11/A18：实际通道数必须 &gt;= 容器宽度且 &gt; 最大槽位 index）。
    pub fn validate_output_device_width(
        &self,
        actual_output_channels: u16,
    ) -> Result<(), RolesError> {
        let required = self.required_device_width();
        if actual_output_channels < required.min_output_channels
            || actual_output_channels <= required.max_output_channel_index
        {
            return Err(RolesError::with_field(
                RolesErrorCode::DeviceWidth,
                format!("output_layouts.{}.device_channels", self.layout_id),
                format!(
                    "实际播放通道 {actual_output_channels} 不足：layout {} 需要 >= {} 通道且最大槽位 index {} < 实际通道数（禁止错位输出）",
                    self.layout_id, required.min_output_channels, required.max_output_channel_index
                ),
            ));
        }
        Ok(())
    }

    /// 显式角色集合 → 位图（role_index 位；T23 allowed_role_mask 数据面）。
    ///
    /// 只按显式传入的 role id 集合置位；本模块不按名字/频响推断 stereo/height 语义。
    pub fn role_mask(&self, role_ids: &[&str]) -> Result<u16, RolesError> {
        let mut seen: Vec<u16> = Vec::new();
        let mut mask = 0u16;
        for role_id in role_ids {
            let index = self.role_index(role_id).ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::RoleId,
                    "role_mask",
                    format!("角色 {role_id} 不在 layout {} 中", self.layout_id),
                )
            })?;
            if seen.contains(&index) {
                return Err(RolesError::with_field(
                    RolesErrorCode::RoleId,
                    "role_mask",
                    format!("角色 {role_id} 重复"),
                ));
            }
            if index as usize >= u16::BITS as usize {
                return Err(RolesError::with_field(
                    RolesErrorCode::RoleId,
                    "role_mask",
                    format!("角色索引 {index} 超出 u16 位图容量"),
                ));
            }
            seen.push(index);
            mask |= 1u16 << index;
        }
        Ok(mask)
    }

    /// 转成 T13 消费的 LayoutRoleTable（layout 交叉校验/校准合并输入）。
    pub fn to_layout_role_table(&self) -> LayoutRoleTable {
        LayoutRoleTable::new(
            self.layout_id.clone(),
            self.roles
                .iter()
                .map(|role| LayoutRole {
                    id: role.id.clone(),
                    outputs: role.outputs.clone(),
                })
                .collect(),
        )
    }

    /// 转成 T13 的渲染基线角色（未校准 identity；height 同样无默认带限/延迟）。
    pub fn render_role_bases(&self) -> Vec<RenderRoleBase> {
        self.roles
            .iter()
            .filter_map(|role| {
                role.outputs
                    .first()
                    .map(|output| RenderRoleBase::uncalibrated(role.id.clone(), u32::from(*output)))
            })
            .collect()
    }
}

/// 设备宽度要求（compat 侧；与 T17 RequiredDeviceWidth 字段语义一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutDeviceWidth {
    /// 最少播放通道数（= 容器槽位数）。
    pub min_output_channels: u16,
    /// 播放角色使用的最大 0-based 槽位 index。
    pub max_output_channel_index: u16,
}

// ---------------------------------------------------------------------------
// 类型化解析（input/output/remap）
// ---------------------------------------------------------------------------

fn parse_output_layouts(value: &Yaml) -> Result<Vec<OutputLayoutPreset>, RolesError> {
    let entries = require_map(value, "output_layouts")?;
    let mut layouts: Vec<OutputLayoutPreset> = Vec::with_capacity(entries.len());
    for (layout_id, entry) in entries {
        let field = format!("output_layouts.{layout_id}");
        validate_layout_id(layout_id, &field)?;
        let fields = require_map(entry, &field)?;
        check_unknown_keys(fields, &["label", "description", "roles"], &field)?;
        let label = optional_str(fields, "label", &field)?;
        let description = optional_str(fields, "description", &field)?;
        let roles_value = map_get(fields, "roles").ok_or_else(|| {
            RolesError::with_field(
                RolesErrorCode::MissingField,
                format!("{field}.roles"),
                "缺少 roles",
            )
        })?;
        let role_items = require_seq(roles_value, &format!("{field}.roles"))?;
        if role_items.is_empty() {
            return Err(RolesError::with_field(
                RolesErrorCode::LayoutRoles,
                format!("{field}.roles"),
                "布局至少需要一个角色",
            ));
        }
        if role_items.len() > MAX_OUTPUT_ROLES {
            return Err(RolesError::with_field(
                RolesErrorCode::LayoutRoles,
                format!("{field}.roles"),
                format!("角色数 {} 超过上限 {MAX_OUTPUT_ROLES}", role_items.len()),
            ));
        }
        let mut roles: Vec<OutputLayoutRole> = Vec::with_capacity(role_items.len());
        let mut used_outputs: Vec<u16> = Vec::new();
        for (role_position, item) in role_items.iter().enumerate() {
            let role_field = format!("{field}.roles[{role_position}]");
            let role_fields = require_map(item, &role_field)?;
            check_unknown_keys(role_fields, &["id", "label", "outputs"], &role_field)?;
            let role_id = required_str(role_fields, "id", &role_field)?.to_string();
            validate_role_id(&role_id, &format!("{role_field}.id"))?;
            if roles.iter().any(|role| role.id == role_id) {
                return Err(RolesError::with_field(
                    RolesErrorCode::RoleId,
                    format!("{role_field}.id"),
                    format!("角色 id 重复：{role_id}"),
                ));
            }
            let label = optional_str(role_fields, "label", &role_field)?;
            let outputs_value = map_get(role_fields, "outputs").ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::MissingField,
                    format!("{role_field}.outputs"),
                    "缺少 outputs",
                )
            })?;
            let output_items = require_seq(outputs_value, &format!("{role_field}.outputs"))?;
            if output_items.is_empty() {
                return Err(RolesError::with_field(
                    RolesErrorCode::RoleOutputs,
                    format!("{role_field}.outputs"),
                    "outputs 不能为空（角色必须显式占用至少一个容器槽位）",
                ));
            }
            let mut outputs: Vec<u16> = Vec::with_capacity(output_items.len());
            for (output_position, output_value) in output_items.iter().enumerate() {
                let output_field = format!("{role_field}.outputs[{output_position}]");
                let raw = require_int(output_value, &output_field)?;
                if raw < 0 || raw as usize >= MAX_OUTPUT_CHANNELS {
                    return Err(RolesError::with_field(
                        RolesErrorCode::RoleOutputs,
                        output_field,
                        format!("容器槽位索引 {raw} 越界（0..{MAX_OUTPUT_CHANNELS}）"),
                    ));
                }
                let output = raw as u16;
                if used_outputs.contains(&output) {
                    return Err(RolesError::with_field(
                        RolesErrorCode::RoleOutputs,
                        output_field,
                        format!("容器槽位 {output} 被多个角色占用（scatter 必须单射）"),
                    ));
                }
                used_outputs.push(output);
                outputs.push(output);
            }
            roles.push(OutputLayoutRole {
                id: role_id,
                label,
                outputs,
            });
        }
        layouts.push(OutputLayoutPreset {
            id: layout_id.clone(),
            label,
            description,
            roles,
        });
    }
    Ok(layouts)
}

fn parse_input_layouts(value: &Yaml) -> Result<Vec<InputLayoutPreset>, RolesError> {
    let entries = require_map(value, "input_layouts")?;
    let mut layouts: Vec<InputLayoutPreset> = Vec::with_capacity(entries.len());
    for (layout_id, entry) in entries {
        let field = format!("input_layouts.{layout_id}");
        validate_layout_id(layout_id, &field)?;
        let fields = require_map(entry, &field)?;
        check_unknown_keys(
            fields,
            &["label", "description", "channels", "streams", "roles"],
            &field,
        )?;
        let label = optional_str(fields, "label", &field)?;
        let description = optional_str(fields, "description", &field)?;
        let channels = match map_get(fields, "channels") {
            None => None,
            Some(Yaml::Str(text)) if text == "auto" => None,
            Some(Yaml::Int(raw)) => {
                if *raw <= 0 || *raw as usize > MAX_INPUT_CHANNELS {
                    return Err(RolesError::with_field(
                        RolesErrorCode::FieldType,
                        format!("{field}.channels"),
                        format!("channels 必须在 1..{MAX_INPUT_CHANNELS} 或为 auto，实际 {raw}"),
                    ));
                }
                Some(*raw as u16)
            }
            Some(other) => {
                return Err(RolesError::with_field(
                    RolesErrorCode::FieldType,
                    format!("{field}.channels"),
                    format!("channels 必须是正整数或 auto，实际 {}", other.type_name()),
                ))
            }
        };
        let mut streams: Vec<u16> = Vec::new();
        if let Some(streams_value) = map_get(fields, "streams") {
            let stream_items = require_seq(streams_value, &format!("{field}.streams"))?;
            for (stream_position, stream_value) in stream_items.iter().enumerate() {
                let stream_field = format!("{field}.streams[{stream_position}]");
                let raw = require_int(stream_value, &stream_field)?;
                if raw <= 0 || raw as usize > MAX_INPUT_CHANNELS {
                    return Err(RolesError::with_field(
                        RolesErrorCode::FieldType,
                        stream_field,
                        format!("stream 通道数必须在 1..{MAX_INPUT_CHANNELS}，实际 {raw}"),
                    ));
                }
                streams.push(raw as u16);
            }
        }
        let roles_value = map_get(fields, "roles").ok_or_else(|| {
            RolesError::with_field(
                RolesErrorCode::MissingField,
                format!("{field}.roles"),
                "缺少 roles",
            )
        })?;
        let role_items = require_seq(roles_value, &format!("{field}.roles"))?;
        if role_items.is_empty() {
            return Err(RolesError::with_field(
                RolesErrorCode::LayoutRoles,
                format!("{field}.roles"),
                "输入布局至少需要一个角色",
            ));
        }
        if role_items.len() > MAX_INPUT_CHANNELS {
            return Err(RolesError::with_field(
                RolesErrorCode::LayoutRoles,
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
            let role_fields = require_map(item, &role_field)?;
            check_unknown_keys(
                role_fields,
                &["id", "label", "stream", "channel"],
                &role_field,
            )?;
            let role_id = required_str(role_fields, "id", &role_field)?.to_string();
            validate_role_id(&role_id, &format!("{role_field}.id"))?;
            if roles.iter().any(|role| role.id == role_id) {
                return Err(RolesError::with_field(
                    RolesErrorCode::RoleId,
                    format!("{role_field}.id"),
                    format!("输入角色 id 重复：{role_id}"),
                ));
            }
            let label = optional_str(role_fields, "label", &role_field)?;
            let stream = match map_get(role_fields, "stream") {
                None => 0u16,
                Some(value) => {
                    let raw = require_int(value, &format!("{role_field}.stream"))?;
                    if raw < 0 || raw as usize >= MAX_INPUT_CHANNELS {
                        return Err(RolesError::with_field(
                            RolesErrorCode::FieldType,
                            format!("{role_field}.stream"),
                            format!("stream 索引 {raw} 越界"),
                        ));
                    }
                    raw as u16
                }
            };
            let channel_value = map_get(role_fields, "channel").ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::MissingField,
                    format!("{role_field}.channel"),
                    "缺少 channel",
                )
            })?;
            let channel_raw = require_int(channel_value, &format!("{role_field}.channel"))?;
            if channel_raw < 0 || channel_raw as usize >= MAX_INPUT_CHANNELS {
                return Err(RolesError::with_field(
                    RolesErrorCode::FieldType,
                    format!("{role_field}.channel"),
                    format!("channel 索引 {channel_raw} 越界（0..{MAX_INPUT_CHANNELS}）"),
                ));
            }
            let channel = channel_raw as u16;
            if used_channels.contains(&(stream, channel)) {
                return Err(RolesError::with_field(
                    RolesErrorCode::RoleOutputs,
                    format!("{role_field}.channel"),
                    format!("(stream,channel)=({stream},{channel}) 被多个输入角色占用"),
                ));
            }
            used_channels.push((stream, channel));
            roles.push(InputLayoutRole {
                id: role_id,
                label,
                stream,
                channel,
            });
        }
        layouts.push(InputLayoutPreset {
            id: layout_id.clone(),
            label,
            description,
            channels,
            streams,
            roles,
        });
    }
    Ok(layouts)
}

fn parse_remap_presets(
    value: &Yaml,
    input_layouts: &[InputLayoutPreset],
    output_layouts: &[OutputLayoutPreset],
) -> Result<Vec<RemapPreset>, RolesError> {
    let entries = require_map(value, "remap_presets")?;
    let mut presets: Vec<RemapPreset> = Vec::with_capacity(entries.len());
    for (preset_id, entry) in entries {
        let field = format!("remap_presets.{preset_id}");
        validate_remap_id(preset_id, &field)?;
        let fields = require_map(entry, &field)?;
        check_unknown_keys(fields, &["label", "input", "output", "routes"], &field)?;
        let label = optional_str(fields, "label", &field)?;
        let input_id = required_str(fields, "input", &field)?.to_string();
        let output_id = required_str(fields, "output", &field)?.to_string();
        let input_layout = input_layouts
            .iter()
            .find(|layout| layout.id == input_id)
            .ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::LayoutUnknown,
                    format!("{field}.input"),
                    format!("未知输入布局 {input_id}"),
                )
            })?;
        let output_layout = output_layouts
            .iter()
            .find(|layout| layout.id == output_id)
            .ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::LayoutUnknown,
                    format!("{field}.output"),
                    format!("未知输出布局 {output_id}"),
                )
            })?;
        let routes_value = map_get(fields, "routes").ok_or_else(|| {
            RolesError::with_field(
                RolesErrorCode::MissingField,
                format!("{field}.routes"),
                "缺少 routes",
            )
        })?;
        let route_items = require_seq(routes_value, &format!("{field}.routes"))?;
        if route_items.is_empty() {
            return Err(RolesError::with_field(
                RolesErrorCode::Remap,
                format!("{field}.routes"),
                "routes 不能为空",
            ));
        }
        let mut routes: Vec<RemapRoute> = Vec::with_capacity(route_items.len());
        for (route_position, item) in route_items.iter().enumerate() {
            let route_field = format!("{field}.routes[{route_position}]");
            let route_fields = require_map(item, &route_field)?;
            check_unknown_keys(route_fields, &["src", "dest"], &route_field)?;
            let src = required_str(route_fields, "src", &route_field)?.to_string();
            let dest = required_str(route_fields, "dest", &route_field)?.to_string();
            if !input_layout.roles.iter().any(|role| role.id == src) {
                return Err(RolesError::with_field(
                    RolesErrorCode::Remap,
                    format!("{route_field}.src"),
                    format!("输入布局 {input_id} 中不存在角色 {src}"),
                ));
            }
            if !output_layout.roles.iter().any(|role| role.id == dest) {
                return Err(RolesError::with_field(
                    RolesErrorCode::Remap,
                    format!("{route_field}.dest"),
                    format!("输出布局 {output_id} 中不存在角色 {dest}"),
                ));
            }
            if routes.iter().any(|route| route.src == src) {
                return Err(RolesError::with_field(
                    RolesErrorCode::Remap,
                    format!("{route_field}.src"),
                    format!("route src 重复：{src}"),
                ));
            }
            routes.push(RemapRoute { src, dest });
        }
        presets.push(RemapPreset {
            id: preset_id.clone(),
            label,
            input: input_id,
            output: output_id,
            routes,
        });
    }
    Ok(presets)
}

// ---------------------------------------------------------------------------
// Web 内嵌副本 drift 检查（Y10）
// ---------------------------------------------------------------------------

/// overlay 与 Web 内嵌副本的 drift 报告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutDriftReport {
    /// 原始字节是否逐字节相同。
    pub byte_identical: bool,
    /// 语义（schema + 全部 input/output/remap 字段）是否一致。
    pub semantic_identical: bool,
    /// overlay 源字节 SHA-256。
    pub overlay_source_sha256: String,
    /// Web 副本源字节 SHA-256。
    pub web_source_sha256: String,
    /// 语义差异（排序、去重；overlay-only ... / web-only ...）。
    pub differences: Vec<String>,
}

impl LayoutDriftReport {
    /// 严格消费者：语义不一致即 E_ROLES_DRIFT_MISMATCH。
    pub fn ensure_identical(&self) -> Result<(), RolesError> {
        if self.semantic_identical {
            return Ok(());
        }
        Err(RolesError::with_field(
            RolesErrorCode::DriftMismatch,
            "layout-presets",
            format!(
                "overlay 与 Web 内嵌副本语义 drift（{} 处）；overlay 是唯一权威，禁止改用 Web 副本",
                self.differences.len()
            ),
        ))
    }
}

/// 比较 overlay 与 Web 内嵌副本（两者都必须能按 overlay 严格 schema 解析）。
///
/// 生产绑定只用 overlay 解析结果；本函数只用于 drift 检测/告警。禁止用 Web 副本
/// 替代 overlay（含旧 Web 模板 SUB4 映射）。
pub fn check_web_template_drift(
    overlay_bytes: &[u8],
    web_template_bytes: &[u8],
) -> Result<LayoutDriftReport, RolesError> {
    let overlay = LayoutPresets::parse_overlay(overlay_bytes)?;
    let web = LayoutPresets::parse_overlay(web_template_bytes)?;
    let mut differences: Vec<String> = Vec::new();
    if overlay.schema != web.schema {
        differences.push(format!(
            "schema: overlay={} web={}",
            overlay.schema, web.schema
        ));
    }
    let overlay_lines = canonical_output_layout_lines(&overlay.output_layouts);
    let web_lines = canonical_output_layout_lines(&web.output_layouts);
    compare_canonical(&overlay_lines, &web_lines, &mut differences);
    let overlay_input_lines = canonical_input_layout_lines(&overlay.input_layouts);
    let web_input_lines = canonical_input_layout_lines(&web.input_layouts);
    compare_canonical(&overlay_input_lines, &web_input_lines, &mut differences);
    let overlay_remap_lines = canonical_remap_lines(&overlay.remap_presets);
    let web_remap_lines = canonical_remap_lines(&web.remap_presets);
    compare_canonical(&overlay_remap_lines, &web_remap_lines, &mut differences);
    differences.sort();
    differences.dedup();
    Ok(LayoutDriftReport {
        byte_identical: overlay_bytes == web_template_bytes,
        semantic_identical: differences.is_empty(),
        overlay_source_sha256: overlay.source_sha256,
        web_source_sha256: web.source_sha256,
        differences,
    })
}

fn compare_canonical(overlay: &[String], web: &[String], differences: &mut Vec<String>) {
    for line in overlay {
        if !web.contains(line) {
            differences.push(format!("overlay-only {line}"));
        }
    }
    for line in web {
        if !overlay.contains(line) {
            differences.push(format!("web-only {line}"));
        }
    }
}

fn canonical_output_layout_lines(layouts: &[OutputLayoutPreset]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for layout in layouts {
        lines.push(format!(
            "output_layouts.{}.label={}",
            layout.id,
            layout.label.clone().unwrap_or_default()
        ));
        lines.push(format!(
            "output_layouts.{}.description={}",
            layout.id,
            layout.description.clone().unwrap_or_default()
        ));
        for (index, role) in layout.roles.iter().enumerate() {
            let outputs = role
                .outputs
                .iter()
                .map(u16::to_string)
                .collect::<Vec<String>>()
                .join(",");
            lines.push(format!(
                "output_layouts.{}.roles[{index}]={}:{outputs}",
                layout.id, role.id
            ));
            lines.push(format!(
                "output_layouts.{}.roles[{index}].label={}",
                layout.id,
                role.label.clone().unwrap_or_default()
            ));
        }
    }
    lines.sort();
    lines.dedup();
    lines
}

fn canonical_input_layout_lines(layouts: &[InputLayoutPreset]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for layout in layouts {
        lines.push(format!(
            "input_layouts.{}.label={}",
            layout.id,
            layout.label.clone().unwrap_or_default()
        ));
        lines.push(format!(
            "input_layouts.{}.channels={}",
            layout.id,
            layout
                .channels
                .map(|channels| channels.to_string())
                .unwrap_or_else(|| "auto".to_string())
        ));
        lines.push(format!(
            "input_layouts.{}.streams={}",
            layout.id,
            layout
                .streams
                .iter()
                .map(u16::to_string)
                .collect::<Vec<String>>()
                .join(",")
        ));
        for (index, role) in layout.roles.iter().enumerate() {
            lines.push(format!(
                "input_layouts.{}.roles[{index}]={}:{}:{}",
                layout.id, role.id, role.stream, role.channel
            ));
        }
    }
    lines.sort();
    lines.dedup();
    lines
}

fn canonical_remap_lines(presets: &[RemapPreset]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for preset in presets {
        lines.push(format!(
            "remap_presets.{}.input={}",
            preset.id, preset.input
        ));
        lines.push(format!(
            "remap_presets.{}.output={}",
            preset.id, preset.output
        ));
        for (index, route) in preset.routes.iter().enumerate() {
            lines.push(format!(
                "remap_presets.{}.routes[{index}]={}->{}",
                preset.id, route.src, route.dest
            ));
        }
    }
    lines.sort();
    lines.dedup();
    lines
}

// ---------------------------------------------------------------------------
// MIMO participants（显式 scope；不从高度 HP1800/role 名/频响推导）
// ---------------------------------------------------------------------------

/// 一个低带 MIMO participant（dense 索引 ↔ 角色索引 ↔ 容器/DAC 槽位）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimoParticipant {
    /// dense 索引（0..M-1；G 的行/列）。
    pub dense_index: u16,
    /// 角色索引（layout 文件顺序）。
    pub role_index: u16,
    /// 角色 id。
    pub role_id: String,
    /// 容器/DAC 槽位（该角色首槽位；4.1.2 SUB1 = 7）。
    pub dac: u16,
}

/// 校验后的 MIMO participants（dense 0..M-1 与角色/DAC 的显式映射）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimoParticipants {
    /// 布局 id。
    pub layout_id: String,
    /// 布局语义绑定 hash。
    pub layout_hash: String,
    /// participants（dense 顺序）。
    pub participants: Vec<MimoParticipant>,
}

impl MimoParticipants {
    /// M（participants 数；P1 4.1.2 = 5）。
    pub fn m(&self) -> usize {
        self.participants.len()
    }

    /// dense → 角色索引（4.1.2 = [0,1,2,3,6]）。
    pub fn dense_to_role(&self) -> Vec<u16> {
        self.participants
            .iter()
            .map(|participant| participant.role_index)
            .collect()
    }

    /// dense → 容器/DAC 槽位（4.1.2 = [0,1,2,3,7]）。
    pub fn dense_to_dac(&self) -> Vec<u16> {
        self.participants
            .iter()
            .map(|participant| participant.dac)
            .collect()
    }

    /// 角色索引列表。
    pub fn role_indices(&self) -> Vec<u16> {
        self.dense_to_role()
    }

    /// 角色索引是否参与低带矩阵。
    pub fn contains_role_index(&self, role_index: u16) -> bool {
        self.participants
            .iter()
            .any(|participant| participant.role_index == role_index)
    }

    /// 规范化文本（evidence/报告；确定性）。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(MIMO_PARTICIPANTS_HASH_DOMAIN);
        canonical.push('\n');
        canonical.push_str(&format!("layout={}\n", self.layout_id));
        canonical.push_str(&format!("layout_hash={}\n", self.layout_hash));
        for participant in &self.participants {
            canonical.push_str(&format!(
                "dense{}={}:role{}:dac{}\n",
                participant.dense_index,
                participant.role_id,
                participant.role_index,
                participant.dac
            ));
        }
        canonical
    }

    /// 绑定 hash（canonical 文本 SHA-256；供 graph/manifest stale 绑定）。
    pub fn binding_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

/// 显式 MIMO scope（participants 列表是任务边界数据，不是推导结果）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimoScope {
    /// 布局 id。
    pub layout_id: String,
    /// 显式 participants（角色 id，dense 顺序）。
    pub participants: Vec<String>,
}

impl MimoScope {
    /// 构造显式 scope（非空、不重复、&lt;= MAX_LOW_BAND_PARTICIPANTS、角色 id 合法）。
    pub fn new(
        layout_id: impl Into<String>,
        participants: Vec<String>,
    ) -> Result<Self, RolesError> {
        let layout_id = layout_id.into();
        validate_layout_id(&layout_id, "mimo_scope.layout_id")?;
        if participants.is_empty() {
            return Err(RolesError::with_field(
                RolesErrorCode::Participants,
                "mimo_scope.participants",
                "participants 不能为空（必须显式给出 scope）",
            ));
        }
        if participants.len() > MAX_LOW_BAND_PARTICIPANTS {
            return Err(RolesError::with_field(
                RolesErrorCode::Participants,
                "mimo_scope.participants",
                format!(
                    "participants 数 {} 超过上限 {MAX_LOW_BAND_PARTICIPANTS}",
                    participants.len()
                ),
            ));
        }
        for (index, role_id) in participants.iter().enumerate() {
            validate_role_id(role_id, &format!("mimo_scope.participants[{index}]"))?;
            if participants[..index].contains(role_id) {
                return Err(RolesError::with_field(
                    RolesErrorCode::Participants,
                    format!("mimo_scope.participants[{index}]"),
                    format!("participant 重复：{role_id}"),
                ));
            }
        }
        Ok(Self {
            layout_id,
            participants,
        })
    }

    /// P1 4.1.2 显式 scope（FL/FR/SL/SR/SUB1；任务边界，非 HP1800/role 名推导）。
    pub fn p1_4_1_2_explicit() -> Self {
        Self {
            layout_id: TARGET_LAYOUT_4_1_2.to_string(),
            participants: P1_MIMO_PARTICIPANT_IDS
                .iter()
                .map(|role_id| role_id.to_string())
                .collect(),
        }
    }

    /// participants 列表。
    pub fn participants(&self) -> &[String] {
        &self.participants
    }

    /// 对照已绑定 layout 校验并展开 dense→role→DAC 映射。
    pub fn validate(&self, layout: &BoundLayout) -> Result<MimoParticipants, RolesError> {
        if layout.layout_id != self.layout_id {
            return Err(RolesError::with_field(
                RolesErrorCode::Participants,
                "mimo_scope.layout_id",
                format!(
                    "scope layout {} 与绑定 layout {} 不一致（禁止跨布局套用）",
                    self.layout_id, layout.layout_id
                ),
            ));
        }
        let mut participants: Vec<MimoParticipant> = Vec::with_capacity(self.participants.len());
        for (dense_index, role_id) in self.participants.iter().enumerate() {
            let role = layout.role(role_id).ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::Participants,
                    format!("mimo_scope.participants[{dense_index}]"),
                    format!("角色 {role_id} 不在 layout {} 中", layout.layout_id),
                )
            })?;
            let dac = role.outputs.first().copied().ok_or_else(|| {
                RolesError::with_field(
                    RolesErrorCode::Participants,
                    format!("mimo_scope.participants[{dense_index}]"),
                    format!("角色 {role_id} 没有容器槽位"),
                )
            })?;
            participants.push(MimoParticipant {
                dense_index: dense_index as u16,
                role_index: role.role_index,
                role_id: role_id.clone(),
                dac,
            });
        }
        Ok(MimoParticipants {
            layout_id: layout.layout_id.clone(),
            layout_hash: layout.layout_hash.clone(),
            participants,
        })
    }
}

// ---------------------------------------------------------------------------
// 角色修正（corrected 基线：高度全频、无默认 HP/LP/18 ms；滤波只能显式配置）
// ---------------------------------------------------------------------------

/// 显式滤波 kind（设计器产物分类；本模块只做显式配置门，不设计系数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleFilterKind {
    /// crossover 高通支路（T39；显式配置才存在）。
    CrossoverHighPass,
    /// crossover 低通支路（T39）。
    CrossoverLowPass,
    /// 输出目标 FIR（T10/T11/T33–T38）。
    TargetFir,
    /// phase/delay/polarity FIR（T40）。
    PhaseDelayPolarity,
    /// 模态 PEQ（T41/T64；IIR 允许）。
    ModalPeq,
}

impl RoleFilterKind {
    /// 全部已知 kind（报告/错误提示）。
    pub const ALL: [RoleFilterKind; 5] = [
        RoleFilterKind::CrossoverHighPass,
        RoleFilterKind::CrossoverLowPass,
        RoleFilterKind::TargetFir,
        RoleFilterKind::PhaseDelayPolarity,
        RoleFilterKind::ModalPeq,
    ];

    /// 稳定字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            RoleFilterKind::CrossoverHighPass => "crossover_highpass",
            RoleFilterKind::CrossoverLowPass => "crossover_lowpass",
            RoleFilterKind::TargetFir => "target_fir",
            RoleFilterKind::PhaseDelayPolarity => "phase_delay_polarity",
            RoleFilterKind::ModalPeq => "modal_peq",
        }
    }

    /// 解析显式 kind 字面值（未知返回 None；调用方必须拒收，不猜/不降级）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "crossover_highpass" => Some(RoleFilterKind::CrossoverHighPass),
            "crossover_lowpass" => Some(RoleFilterKind::CrossoverLowPass),
            "target_fir" => Some(RoleFilterKind::TargetFir),
            "phase_delay_polarity" => Some(RoleFilterKind::PhaseDelayPolarity),
            "modal_peq" => Some(RoleFilterKind::ModalPeq),
            _ => None,
        }
    }

    /// 是否带限类（HP/LP）。corrected 高度默认必须为 false（无 HP/LP）。
    pub fn is_band_limit(self) -> bool {
        matches!(
            self,
            RoleFilterKind::CrossoverHighPass | RoleFilterKind::CrossoverLowPass
        )
    }
}

/// 单角色修正链（corrected 基线：空 filters = 全频 identity）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleCorrection {
    /// 角色 id（仅报告/绑定；不参与按名字注入）。
    pub role_id: String,
    /// 显式滤波 kind（按规范链序；空 = identity/全频）。
    pub filters: Vec<RoleFilterKind>,
}

impl RoleCorrection {
    /// identity（全频；无 HP/LP、无 18 ms 人工延迟——本类型根本没有延迟字段）。
    pub fn identity(role_id: impl Into<String>) -> Self {
        Self {
            role_id: role_id.into(),
            filters: Vec::new(),
        }
    }

    /// 是否全频 identity（无任何滤波）。
    pub fn is_fullrange_identity(&self) -> bool {
        self.filters.is_empty()
    }

    /// 是否含带限（HP/LP）滤波。
    pub fn has_band_limit(&self) -> bool {
        self.filters.iter().any(|kind| kind.is_band_limit())
    }

    /// 规范化行（role FL: identity / role SUB1: crossover_lowpass）。
    pub fn canonical_line(&self) -> String {
        if self.filters.is_empty() {
            format!("role {}: identity", self.role_id)
        } else {
            let kinds = self
                .filters
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<&str>>()
                .join(",");
            format!("role {}: {kinds}", self.role_id)
        }
    }
}

/// 显式角色修正配置（文本格式；只允许 corrected profile + 已知 kind）。
///
/// 文本格式（UTF-8；# 行注释）：
///
/// ~~~text
/// # 必须显式声明 profile；只接受 corrected_fullrange_height
/// profile corrected_fullrange_height
/// role FL: identity
/// role SUB1: crossover_lowpass
/// ~~~
///
/// 未列出的角色 = identity（全频，无 HP/LP/延迟/极性/增益注入）；列出的角色只有显式
/// 声明的 kind。未知 kind / 未知指令 / 未知 profile 整份拒收。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RoleCorrectionConfig {
    profile: Option<String>,
    entries: Vec<(String, Vec<RoleFilterKind>)>,
}

impl RoleCorrectionConfig {
    /// 空配置（corrected 全频基线）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 解析显式配置文本（严格；见类型文档）。
    pub fn parse_text(text: &str) -> Result<Self, RolesError> {
        let mut config = RoleCorrectionConfig::default();
        for (index, raw_line) in text.lines().enumerate() {
            let number = index + 1;
            let line = strip_correction_comment(raw_line).trim().to_string();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let keyword = parts.next().unwrap_or("");
            let rest = parts.next().unwrap_or("").trim();
            match keyword {
                "profile" => {
                    if config.profile.is_some() {
                        return Err(RolesError::with_field(
                            RolesErrorCode::CorrectionProfile,
                            format!("line {number}"),
                            "profile 重复声明",
                        ));
                    }
                    if rest.is_empty() {
                        return Err(RolesError::with_field(
                            RolesErrorCode::CorrectionProfile,
                            format!("line {number}"),
                            "profile 行缺少 id",
                        ));
                    }
                    if rest != CORRECTED_PROFILE_ID {
                        return Err(RolesError::with_field(
                            RolesErrorCode::CorrectionProfile,
                            format!("line {number}"),
                            format!(
                                "未知修正 profile {rest:?}；本模块只产出 {CORRECTED_PROFILE_ID}（legacy_s1 不进 corrected 基线）"
                            ),
                        ));
                    }
                    config.profile = Some(rest.to_string());
                }
                "role" => {
                    let colon = rest.find(':').ok_or_else(|| {
                        RolesError::with_field(
                            RolesErrorCode::CorrectionSyntax,
                            format!("line {number}"),
                            "role 行缺少 ':'",
                        )
                    })?;
                    let role_id = rest[..colon].trim();
                    let kinds_text = rest[colon + 1..].trim();
                    if kinds_text.is_empty() {
                        return Err(RolesError::with_field(
                            RolesErrorCode::CorrectionSyntax,
                            format!("line {number}"),
                            "role 行缺少滤波声明（identity 或显式 kind）",
                        ));
                    }
                    let tokens: Vec<&str> = kinds_text
                        .split(|character: char| character == ',' || character.is_whitespace())
                        .filter(|token| !token.is_empty())
                        .collect();
                    if tokens.is_empty() {
                        return Err(RolesError::with_field(
                            RolesErrorCode::CorrectionSyntax,
                            format!("line {number}"),
                            "role 行缺少滤波声明",
                        ));
                    }
                    let mut filters: Vec<RoleFilterKind> = Vec::new();
                    if tokens.len() == 1 && tokens[0] == "identity" {
                        filters = Vec::new();
                    } else {
                        for token in tokens {
                            if token == "identity" {
                                return Err(RolesError::with_field(
                                    RolesErrorCode::CorrectionSyntax,
                                    format!("line {number}"),
                                    "identity 不能与显式滤波混写",
                                ));
                            }
                            let kind = RoleFilterKind::parse(token).ok_or_else(|| {
                                RolesError::with_field(
                                    RolesErrorCode::CorrectionFilterUnknown,
                                    format!("role {role_id}"),
                                    format!(
                                        "未知滤波 kind {token:?}（只允许显式已知 kind {:?}；禁止按 role 名注入默认滤波）",
                                        RoleFilterKind::ALL
                                            .iter()
                                            .map(|kind| kind.as_str())
                                            .collect::<Vec<&str>>()
                                    ),
                                )
                            })?;
                            if filters.contains(&kind) {
                                return Err(RolesError::with_field(
                                    RolesErrorCode::CorrectionSyntax,
                                    format!("line {number}"),
                                    format!("滤波 kind 重复：{}", kind.as_str()),
                                ));
                            }
                            filters.push(kind);
                        }
                    }
                    config.set_role(role_id, filters)?;
                }
                other => {
                    return Err(RolesError::with_field(
                        RolesErrorCode::CorrectionSyntax,
                        format!("line {number}"),
                        format!("未知指令 {other:?}（只允许 profile/role）"),
                    ))
                }
            }
        }
        if config.profile.is_none() {
            return Err(RolesError::with_field(
                RolesErrorCode::CorrectionProfile,
                "profile",
                format!("缺少 profile 行（必须显式声明 {CORRECTED_PROFILE_ID}）"),
            ));
        }
        Ok(config)
    }

    /// 已声明的 profile（空配置为 None）。
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// 是否没有任何显式条目（= 全频基线）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 查某角色的显式 filters（未列出返回 None = identity）。
    pub fn role_filters(&self, role_id: &str) -> Option<&[RoleFilterKind]> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == role_id)
            .map(|(_, filters)| filters.as_slice())
    }

    /// 设置某角色的显式 filters（重复角色/非法 id 拒收）。
    pub fn set_role(
        &mut self,
        role_id: impl Into<String>,
        filters: Vec<RoleFilterKind>,
    ) -> Result<(), RolesError> {
        let role_id = role_id.into();
        validate_role_id(&role_id, "correction.role")?;
        if self
            .entries
            .iter()
            .any(|(existing, _)| existing == &role_id)
        {
            return Err(RolesError::with_field(
                RolesErrorCode::CorrectionRole,
                format!("role {role_id}"),
                "角色修正重复声明",
            ));
        }
        self.entries.push((role_id, filters));
        Ok(())
    }

    /// 应用到已绑定 layout：未列出角色 = identity；列出角色 = 显式 filters。
    ///
    /// 只接受 corrected_fullrange_height profile；非空配置必须声明 profile；
    /// 引用不在 layout 中的角色 = 拒收（禁止把别的布局/名字的滤波套进来）。
    pub fn apply(&self, layout: &BoundLayout) -> Result<Vec<RoleCorrection>, RolesError> {
        if let Some(profile) = &self.profile {
            if profile != CORRECTED_PROFILE_ID {
                return Err(RolesError::with_field(
                    RolesErrorCode::CorrectionProfile,
                    "correction.profile",
                    format!("未知修正 profile {profile:?}；只接受 {CORRECTED_PROFILE_ID}"),
                ));
            }
        } else if !self.entries.is_empty() {
            return Err(RolesError::with_field(
                RolesErrorCode::CorrectionProfile,
                "correction.profile",
                format!("非空修正配置必须显式声明 profile {CORRECTED_PROFILE_ID}"),
            ));
        }
        for (role_id, _) in &self.entries {
            if layout.role(role_id).is_none() {
                return Err(RolesError::with_field(
                    RolesErrorCode::CorrectionRole,
                    format!("role {role_id}"),
                    format!("角色不在 layout {} 中（禁止跨布局套用）", layout.layout_id),
                ));
            }
        }
        let corrections = layout
            .roles
            .iter()
            .map(|role| match self.role_filters(&role.id) {
                Some(filters) => RoleCorrection {
                    role_id: role.id.clone(),
                    filters: filters.to_vec(),
                },
                None => RoleCorrection::identity(role.id.clone()),
            })
            .collect();
        Ok(corrections)
    }
}

/// corrected 全频基线：每个角色 identity（含高度；无 HP/LP/18 ms/极性/增益注入）。
pub fn corrected_baseline(layout: &BoundLayout) -> Vec<RoleCorrection> {
    layout
        .roles
        .iter()
        .map(|role| RoleCorrection::identity(role.id.clone()))
        .collect()
}

/// 修正链绑定 hash（canonical 行 SHA-256；供 graph/manifest stale 绑定）。
pub fn corrections_hash(corrections: &[RoleCorrection]) -> String {
    let mut canonical = String::new();
    canonical.push_str(CORRECTION_HASH_DOMAIN);
    canonical.push('\n');
    canonical.push_str(&format!("profile={CORRECTED_PROFILE_ID}\n"));
    for correction in corrections {
        canonical.push_str(&correction.canonical_line());
        canonical.push('\n');
    }
    sha256_hex(canonical.as_bytes())
}

fn strip_correction_comment(line: &str) -> &str {
    match line.find('#') {
        Some(index) => &line[..index],
        None => line,
    }
}
