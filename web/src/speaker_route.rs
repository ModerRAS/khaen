//! 声道设置：输入/输出逐槽角色、派生流向、可执行的 plan path。
//!
//! 播放侧只执行 `direct` 与 `center_downmix`。`reserved_downmix` 留在流向里，不生成混音。
//! `upmix` 出现在策略字段中时拒绝，不生成信号。

use serde::{Deserialize, Serialize};

/// 中置折进左右的线性系数（与 `content_map::CENTER_DOWNMIX_GAIN_LINEAR` 相同，−3.000 dB）。
pub const CENTER_DOWNMIX_GAIN: f64 = 0.7079458;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleInfo {
    pub id: &'static str,
    pub zh: &'static str,
    pub en: &'static str,
}

pub const ROLES: &[RoleInfo] = &[
    RoleInfo { id: "FL", zh: "前左", en: "FL" },
    RoleInfo { id: "FR", zh: "前右", en: "FR" },
    RoleInfo { id: "C", zh: "中置", en: "C" },
    RoleInfo { id: "LFE", zh: "低音", en: "LFE" },
    RoleInfo { id: "SUB2", zh: "低音2", en: "SUB2" },
    RoleInfo { id: "SUB3", zh: "低音3", en: "SUB3" },
    RoleInfo { id: "SL", zh: "环绕左", en: "SL" },
    RoleInfo { id: "SR", zh: "环绕右", en: "SR" },
    RoleInfo { id: "BL", zh: "后左", en: "BL" },
    RoleInfo { id: "BR", zh: "后右", en: "BR" },
    RoleInfo { id: "TFL", zh: "高度前左", en: "TFL" },
    RoleInfo { id: "TFR", zh: "高度前右", en: "TFR" },
    RoleInfo { id: "TRL", zh: "高度后左", en: "TRL" },
    RoleInfo { id: "TRR", zh: "高度后右", en: "TRR" },
];

const SUB_ROLES: [&str; 3] = ["LFE", "SUB2", "SUB3"];

/// 旧布局 id 收成同一套语义角色。
pub fn canonical_role(raw: &str) -> Option<&'static str> {
    let id = raw.trim();
    if id.is_empty() {
        return None;
    }
    match id {
        "HFL" | "TFL" => Some("TFL"),
        "HFR" | "TFR" => Some("TFR"),
        "HRL" | "TRL" => Some("TRL"),
        "HRR" | "TRR" => Some("TRR"),
        "SUB1" | "LFE" => Some("LFE"),
        other => ROLES.iter().find(|role| role.id == other).map(|role| role.id),
    }
}

pub fn role_label(id: &str) -> String {
    ROLES
        .iter()
        .find(|role| role.id == id)
        .map(|role| format!("{} {}", role.zh, role.en))
        .unwrap_or_else(|| id.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotBind {
    pub channel: u16,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputBind {
    pub has_center: bool,
    pub sub_count: u8,
    /// `silence` 或 `upmix`。本次只接受 `silence`。
    #[serde(default = "default_missing_input")]
    pub missing_input: String,
    pub slots: Vec<SlotBind>,
}

fn default_missing_input() -> String {
    "silence".to_string()
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SpeakerRouteRequest {
    pub input: Vec<SlotBind>,
    pub output: OutputBind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RouteEdge {
    pub strategy: &'static str,
    pub from_channel: Option<u16>,
    pub from_role: Option<String>,
    pub to_channel: Option<u16>,
    pub to_role: Option<String>,
    pub gain: Option<f64>,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PathPair {
    pub out_index: u16,
    pub in_index: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpeakerRouteResult {
    pub ok: bool,
    pub errors: Vec<String>,
    pub edges: Vec<RouteEdge>,
    pub paths: Vec<PathPair>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpeakerRouteCatalog {
    pub roles: Vec<RoleJson>,
    pub input_512: Vec<SlotBind>,
    pub output_412: OutputBind,
    pub center_gain_linear: f64,
    pub center_gain_label: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoleJson {
    pub id: &'static str,
    pub zh: &'static str,
    pub en: &'static str,
}

pub fn catalog() -> SpeakerRouteCatalog {
    SpeakerRouteCatalog {
        roles: ROLES
            .iter()
            .map(|role| RoleJson {
                id: role.id,
                zh: role.zh,
                en: role.en,
            })
            .collect(),
        input_512: preset_input_512(),
        output_412: preset_output_412(),
        center_gain_linear: CENTER_DOWNMIX_GAIN,
        center_gain_label: "−3 dB",
    }
}

pub fn preset_input_512() -> Vec<SlotBind> {
    ["FL", "FR", "C", "LFE", "SL", "SR", "TFL", "TFR"]
        .into_iter()
        .enumerate()
        .map(|(channel, role)| SlotBind {
            channel: channel as u16,
            role: Some(role.to_string()),
        })
        .collect()
}

pub fn preset_output_412() -> OutputBind {
    let roles = [
        Some("FL"),
        Some("FR"),
        Some("SL"),
        Some("SR"),
        Some("TFL"),
        Some("TFR"),
        None,
        Some("LFE"),
    ];
    OutputBind {
        has_center: false,
        sub_count: 1,
        missing_input: "silence".to_string(),
        slots: roles
            .into_iter()
            .enumerate()
            .map(|(channel, role)| SlotBind {
                channel: channel as u16,
                role: role.map(str::to_string),
            })
            .collect(),
    }
}

/// `nsp-5.1.2`（或语义上的单流 5.1.2）对输出 `4.1.2` 不能走 channelmap remap。
pub fn rejects_channelmap_draft(input_layout: &str, output_layout: &str) -> bool {
    let input = input_layout.trim();
    let output = output_layout.trim();
    (input == "nsp-5.1.2" || input == "5.1.2") && output == "4.1.2"
}

struct Assigned {
    channel: u16,
    role: &'static str,
}

pub fn derive(request: &SpeakerRouteRequest) -> SpeakerRouteResult {
    let mut errors = Vec::new();
    let missing = request.output.missing_input.trim();
    if missing != "silence" && missing != "upmix" {
        errors.push(format!("missing_input 只能是 silence 或 upmix（got {missing}）"));
    }
    if request.output.sub_count > 3 {
        errors.push("低音炮数量只能是 0、1、2 或 3".to_string());
    }
    if missing == "upmix" {
        errors.push("上混口子尚未接入".to_string());
    }

    let input = normalize_slots(&request.input, "输入", &mut errors);
    let output = normalize_slots(&request.output.slots, "输出", &mut errors);
    check_duplicates(&input, "输入", &mut errors);
    check_duplicates(&output, "输出", &mut errors);
    check_output_inventory(&request.output, &output, &mut errors);

    let mut edges = Vec::new();
    let mut paths = Vec::new();
    if errors.is_empty() {
        edges.extend(build_edges(&input, &output));
        paths = direct_paths(&edges);
        paths.sort_by_key(|path| (path.out_index, path.in_index));
    }

    SpeakerRouteResult {
        ok: errors.is_empty(),
        errors,
        edges,
        paths,
    }
}

fn normalize_slots(slots: &[SlotBind], side: &str, errors: &mut Vec<String>) -> Vec<Assigned> {
    let mut assigned = Vec::new();
    for slot in slots {
        let Some(raw) = slot.role.as_deref() else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        match canonical_role(raw) {
            Some(role) => assigned.push(Assigned {
                channel: slot.channel,
                role,
            }),
            None => errors.push(format!("{side}槽 {} 的角色 {raw:?} 不在词表里", slot.channel)),
        }
    }
    assigned
}

fn check_duplicates(slots: &[Assigned], side: &str, errors: &mut Vec<String>) {
    for (index, slot) in slots.iter().enumerate() {
        if slots[..index].iter().any(|earlier| earlier.role == slot.role) {
            errors.push(format!(
                "{side}角色 {} 出现了多次",
                role_label(slot.role)
            ));
        }
    }
}

fn check_output_inventory(output: &OutputBind, slots: &[Assigned], errors: &mut Vec<String>) {
    let center_slots: Vec<u16> = slots
        .iter()
        .filter(|slot| slot.role == "C")
        .map(|slot| slot.channel)
        .collect();
    if output.has_center && center_slots.len() != 1 {
        errors.push("有中置时，恰好一个输出槽要选中置 C".to_string());
    }
    if !output.has_center && !center_slots.is_empty() {
        errors.push("无中置时，输出槽不能再标成中置 C".to_string());
    }
    if output.has_center && (find_role(slots, "FL").is_none() || find_role(slots, "FR").is_none())
    {
        errors.push("有中置时，前左 FL 和前右 FR 都要有槽".to_string());
    }
    if !output.has_center && (find_role(slots, "FL").is_none() || find_role(slots, "FR").is_none())
    {
        errors.push("无中置、需要下混时，前左 FL 和前右 FR 都要有槽".to_string());
    }
    let needed = required_subs(output.sub_count);
    let actual: Vec<&str> = slots
        .iter()
        .filter(|slot| SUB_ROLES.contains(&slot.role))
        .map(|slot| slot.role)
        .collect();
    if actual.len() != needed.len() || needed.iter().any(|role| !actual.contains(role)) {
        errors.push(format!(
            "低音炮数量是 {}，输出侧低音角色必须正好是 {}",
            output.sub_count,
            if needed.is_empty() {
                "没有".to_string()
            } else {
                needed.join("、")
            }
        ));
    }
}

fn required_subs(count: u8) -> Vec<&'static str> {
    SUB_ROLES.into_iter().take(count as usize).collect()
}

fn find_role(slots: &[Assigned], role: &str) -> Option<u16> {
    slots
        .iter()
        .find(|slot| slot.role == role)
        .map(|slot| slot.channel)
}

fn build_edges(input: &[Assigned], output: &[Assigned]) -> Vec<RouteEdge> {
    let mut edges = Vec::new();
    let output_has_center = find_role(output, "C").is_some();
    for source in input {
        if source.role == "C" && !output_has_center {
            if let (Some(left), Some(right)) = (find_role(output, "FL"), find_role(output, "FR")) {
                edges.push(downmix_edge(source, left, "FL"));
                edges.push(downmix_edge(source, right, "FR"));
            }
            continue;
        }
        if let Some(dest) = find_role(output, source.role) {
            edges.push(RouteEdge {
                strategy: "direct",
                from_channel: Some(source.channel),
                from_role: Some(source.role.to_string()),
                to_channel: Some(dest),
                to_role: Some(source.role.to_string()),
                gain: Some(1.0),
                label: format!("{} 直通", role_label(source.role)),
            });
        } else {
            edges.push(RouteEdge {
                strategy: "reserved_downmix",
                from_channel: Some(source.channel),
                from_role: Some(source.role.to_string()),
                to_channel: None,
                to_role: None,
                gain: None,
                label: format!("{} 保留，本次不混", role_label(source.role)),
            });
        }
    }
    for dest in output {
        let fed = edges.iter().any(|edge| {
            edge.strategy == "direct" && edge.to_role.as_deref() == Some(dest.role)
        });
        let downmix_target = dest.role == "FL" || dest.role == "FR";
        let fed_by_center = edges.iter().any(|edge| edge.strategy == "center_downmix")
            && downmix_target
            && !output_has_center;
        if !fed && !fed_by_center {
            edges.push(RouteEdge {
                strategy: "silence",
                from_channel: None,
                from_role: None,
                to_channel: Some(dest.channel),
                to_role: Some(dest.role.to_string()),
                gain: None,
                label: format!("{} 无输入", role_label(dest.role)),
            });
        }
    }
    edges
}

fn downmix_edge(source: &Assigned, to_channel: u16, to_role: &str) -> RouteEdge {
    RouteEdge {
        strategy: "center_downmix",
        from_channel: Some(source.channel),
        from_role: Some("C".to_string()),
        to_channel: Some(to_channel),
        to_role: Some(to_role.to_string()),
        gain: Some(CENTER_DOWNMIX_GAIN),
        label: format!("中置下混到 {} −3 dB", role_label(to_role)),
    }
}

fn direct_paths(edges: &[RouteEdge]) -> Vec<PathPair> {
    edges
        .iter()
        .filter(|edge| edge.strategy == "direct")
        .filter_map(|edge| {
            Some(PathPair {
                out_index: edge.to_channel?,
                in_index: edge.from_channel?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset_pair() -> SpeakerRouteRequest {
        SpeakerRouteRequest {
            input: preset_input_512(),
            output: preset_output_412(),
        }
    }

    #[test]
    fn preset_512_to_412_compiles_known_paths() {
        let result = derive(&preset_pair());
        assert!(result.ok, "{:?}", result.errors);
        let paths: Vec<(u16, u16)> = result
            .paths
            .iter()
            .map(|path| (path.out_index, path.in_index))
            .collect();
        assert_eq!(
            paths,
            vec![(0, 0), (1, 1), (2, 4), (3, 5), (4, 6), (5, 7), (7, 3)]
        );
        let center: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.strategy == "center_downmix")
            .map(|edge| edge.to_role.clone())
            .collect();
        assert_eq!(center, vec![Some("FL".into()), Some("FR".into())]);
        assert!(result
            .edges
            .iter()
            .any(|edge| edge.strategy == "direct" && edge.from_role.as_deref() == Some("TFL")));
        assert!(!result
            .paths
            .iter()
            .any(|path| path.in_index == 2));
    }

    #[test]
    fn center_present_is_direct_and_not_folded() {
        let mut request = preset_pair();
        request.output.has_center = true;
        request.output.slots[6].role = Some("C".into());
        let result = derive(&request);
        assert!(result.ok, "{:?}", result.errors);
        assert!(result
            .edges
            .iter()
            .any(|edge| edge.strategy == "direct" && edge.from_role.as_deref() == Some("C")));
        assert!(!result
            .edges
            .iter()
            .any(|edge| edge.strategy == "center_downmix"));
    }

    #[test]
    fn extra_input_role_is_reserved_not_a_path() {
        let mut request = preset_pair();
        request.input.push(SlotBind {
            channel: 8,
            role: Some("BL".into()),
        });
        let result = derive(&request);
        assert!(result.ok, "{:?}", result.errors);
        assert!(result.edges.iter().any(|edge| {
            edge.strategy == "reserved_downmix" && edge.from_role.as_deref() == Some("BL")
        }));
        assert!(!result.paths.iter().any(|path| path.in_index == 8));
    }

    #[test]
    fn upmix_is_rejected() {
        let mut request = preset_pair();
        request.output.missing_input = "upmix".into();
        let result = derive(&request);
        assert!(!result.ok);
        assert!(result.errors.iter().any(|error| error.contains("上混")));
        assert!(result.paths.is_empty());
    }

    #[test]
    fn duplicate_output_role_is_rejected() {
        let mut request = preset_pair();
        request.output.slots[6].role = Some("FL".into());
        let result = derive(&request);
        assert!(!result.ok);
        assert!(result.errors.iter().any(|error| error.contains("前左")));
    }

    #[test]
    fn hfl_alias_matches_height() {
        let mut request = preset_pair();
        request.output.slots[4].role = Some("HFL".into());
        let result = derive(&request);
        assert!(result.ok, "{:?}", result.errors);
        assert!(result
            .paths
            .iter()
            .any(|path| path.out_index == 4 && path.in_index == 6));
    }

    #[test]
    fn draft_pair_is_rejected() {
        assert!(rejects_channelmap_draft("nsp-5.1.2", "4.1.2"));
        assert!(rejects_channelmap_draft(" 5.1.2 ", "4.1.2"));
        assert!(!rejects_channelmap_draft("h16-5.1.2", "4.1.2"));
        assert!(!rejects_channelmap_draft("h16-5.1", "5.1"));
    }
}
