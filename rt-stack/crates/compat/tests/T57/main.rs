//! T57 roles 私有集成测试（WBS T57；写域 crates/compat/tests/T57/）。
//!
//! 覆盖 WBS T57 / 任务卡 DoD：
//! - 唯一 overlay layout 预设解析 + hash 绑定：4.1.2 FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7、
//!   容器 8 槽/ch6 空/7 物理角色、scatter [0,1,2,3,4,5,7]；未知 schema/字段/重复 key 拒收；
//! - 不读 Web 模板 SUB4：生产绑定只用 overlay；Web 内嵌副本只做 drift 比对，旧 SUB4 副本被
//!   drift 检出且不改变 overlay 绑定；
//! - corrected 高度默认全频：无 HP/LP、无人工 18 ms；identity 基线逐角色成立；
//! - 未知滤波必须显式配置：未知 kind 整份拒收；显式 kind 只作用于被配置角色；不按 role 名注入；
//! - MIMO participants 显式 5（FL/FR/SL/SR/SUB1），dense 0..4 → DAC 0/1/2/3/7，
//!   不从高度 HP1800/role 名/频响推导；
//! - 设备宽度校验、role_mask、T13 LayoutRoleTable/RenderRoleBase 互通（校准示例交叉校验+合并）；
//! - YAML 子集解析（flow/引号/注释/块映射序列项）与不支持特性拒收、错误码目录/Display。
//!
//! Cargo 自动把 tests/T57/main.rs 发现为集成测试 target；DoD 命令：
//! cargo test -p tspi-dsp-compat fullrange_height（测试名含 fullrange_height）。
//! 只读输入：overlay/web layout-presets.yml 与 calibration-spec 示例经 include_str! 绑定；
//! 不写任何临时文件（本机 %TEMP% 在内存盘 R:，写盘可能 PermissionDenied）。

use tspi_dsp_compat::calibration::{
    merge_calibration, parse_calibration_yaml_with_layout, Polarity,
};
use tspi_dsp_compat::roles::*;

const OVERLAY_PRESETS: &str =
    include_str!("../../../../../audio/overlay/etc/tspi-audio/layout-presets.yml");
const WEB_PRESETS: &str = include_str!("../../../../../web/assets/layout-presets.yml");
const CALIBRATION_412_EXAMPLE: &str =
    include_str!("../../../../../calibration-spec/examples/calibration-4.1.2.example.yml");

/// 仓库内 overlay layout-presets.yml 的 SHA-256。
/// 2026-11-22 追加输入布局 nsp-5.1.2 后更新；追加前的 11,228 B 基线仍记在
/// `rt-stack/spec/core-v1.md`（4856109265371666a6001804330e2a595dad8599b3aecc2f4390be679b2427da）。
const OVERLAY_SHA256: &str = "a698edb4b1b507297a46f7fc35821e3511d5b7c1fa3d9babc6d6271181853e73";
/// 4.1.2 语义绑定 hash（tspi-layout-binding/v1 canonical 文本 SHA-256；测试/证据双绑）。
const LAYOUT_4_1_2_HASH: &str = "6a1927040dfe1759dc0693a074f510879c88e74544df4fabcc68257f34d912c6";
/// corrected 全频基线修正链 hash（7 角色 identity）。
const CORRECTIONS_BASELINE_HASH: &str =
    "bdb4089ad53e4ba393772b89fb0b7b343db78f7ee6fc4392922796020f8375b2";
/// 5.3.2 语义绑定 hash（证明不是只支持硬编码 4.1.2）。
const LAYOUT_5_3_2_HASH: &str = "00af93919b5a70a7c68ce3027fd00725fcaa2447bc075ef72c31249e87bf326c";

fn overlay() -> LayoutPresets {
    LayoutPresets::parse_overlay(OVERLAY_PRESETS.as_bytes())
        .expect("overlay layout-presets 必须可解析")
}

fn bound_412() -> BoundLayout {
    overlay()
        .bind_4_1_2(22)
        .expect("4.1.2 必须绑定到 22 通道播放设备")
}

fn role_ids(bound: &BoundLayout) -> Vec<&str> {
    bound.roles.iter().map(|role| role.id.as_str()).collect()
}

fn minimal_doc(roles_body: &str) -> String {
    format!(
        "schema: tspi-layout-presets/v2\noutput_layouts:\n  \"4.1.2\":\n    roles:\n{roles_body}"
    )
}

fn role_line(role_id: &str, outputs: &str) -> String {
    format!("      - {{id: {role_id}, outputs: {outputs}}}\n")
}

fn parse_error(bytes: &[u8]) -> RolesError {
    LayoutPresets::parse_overlay(bytes).expect_err("必须拒收非法 layout-presets")
}

// ---------------------------------------------------------------------------
// 1. overlay 4.1.2 唯一映射 + hash 绑定
// ---------------------------------------------------------------------------

#[test]
fn roles_overlay_4_1_2_mapping_hash_binding_and_ch6_empty() {
    let presets = overlay();
    assert_eq!(presets.schema(), LAYOUT_PRESETS_SCHEMA);
    assert_eq!(presets.source_sha256(), OVERLAY_SHA256);
    assert_eq!(presets.output_layouts().len(), 14);

    let bound = presets.bind_4_1_2(22).expect("bind 4.1.2");
    assert_eq!(bound.layout_id, TARGET_LAYOUT_4_1_2);
    assert_eq!(bound.container_channels, 8);
    assert_eq!(bound.role_count(), 7);
    assert_eq!(
        role_ids(&bound),
        ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"]
    );
    let outputs: Vec<Vec<u16>> = bound
        .roles
        .iter()
        .map(|role| role.outputs.clone())
        .collect();
    assert_eq!(
        outputs,
        vec![
            vec![0u16],
            vec![1],
            vec![2],
            vec![3],
            vec![4],
            vec![5],
            vec![7]
        ]
    );
    // 唯一 role→DAC：SUB1=7（不是 Web 模板 SUB4）、ch6 空。
    assert_eq!(bound.first_output("SUB1"), Some(7));
    assert_eq!(bound.role_index("SUB1"), Some(6));
    assert!(bound.role("SUB4").is_none());
    assert_eq!(bound.scatter(), vec![0, 1, 2, 3, 4, 5, 7]);
    assert_eq!(bound.used_container_channels(), vec![0, 1, 2, 3, 4, 5, 7]);
    assert_eq!(bound.unused_container_channels(), vec![6]);
    // 设备宽度要求（解析 + 实际设备宽度校验，不硬编码快照）。
    assert_eq!(
        bound.required_device_width(),
        LayoutDeviceWidth {
            min_output_channels: 8,
            max_output_channel_index: 7,
        }
    );
    // hash 绑定：语义 hash 与解析来源绑定。
    assert_eq!(bound.layout_hash, LAYOUT_4_1_2_HASH);
    assert_eq!(presets.layout_hash("4.1.2").unwrap(), LAYOUT_4_1_2_HASH);
    assert_eq!(bound.source_sha256, OVERLAY_SHA256);
}

// ---------------------------------------------------------------------------
// 2. 不读 Web 模板 SUB4：drift 检出但 overlay 绑定不变
// ---------------------------------------------------------------------------

#[test]
fn roles_overlay_4_1_2_sub1_dac7_not_web_template_sub4() {
    let bound = bound_412();
    assert_eq!(bound.first_output("SUB1"), Some(7));
    assert!(bound.role("SUB4").is_none());

    // 仓库内 Web 内嵌副本当前与 overlay 逐字节一致（B1c 复核：三处 diff=0）。
    let drift = check_web_template_drift(OVERLAY_PRESETS.as_bytes(), WEB_PRESETS.as_bytes())
        .expect("drift 检查必须可解析两侧副本");
    assert!(drift.byte_identical);
    assert!(drift.semantic_identical);
    assert!(drift.differences.is_empty());
    drift.ensure_identical().expect("仓库副本应无 drift");

    // 合成旧 Web 模板（SUB4/槽 6）必须被 drift 检出，且不得改变 overlay 绑定。
    let web_old = WEB_PRESETS.replace(
        "{id: SUB1, label: Subwoofer, outputs: [7]}",
        "{id: SUB4, label: Subwoofer, outputs: [6]}",
    );
    assert_ne!(web_old, WEB_PRESETS, "fixture 替换必须命中 4.1.2 SUB1 行");
    let drift = check_web_template_drift(OVERLAY_PRESETS.as_bytes(), web_old.as_bytes())
        .expect("合成 Web 副本仍应是合法 v2 文档（否则应报解析错误而非 drift）");
    assert!(!drift.semantic_identical);
    assert!(drift.differences.iter().any(|line| line.contains("SUB4")));
    assert!(drift.differences.iter().any(|line| line.contains("4.1.2")));
    let error = drift
        .ensure_identical()
        .expect_err("drift 必须被严格消费者拒收");
    assert_eq!(error.code, RolesErrorCode::DriftMismatch);
    assert_eq!(error.code_str(), "E_ROLES_DRIFT_MISMATCH");

    // 生产绑定只用 overlay：仍然 SUB1=7，无 SUB4。
    let bound_again = overlay().bind_4_1_2(22).unwrap();
    assert_eq!(bound_again.first_output("SUB1"), Some(7));
    assert!(bound_again.role("SUB4").is_none());
    assert_eq!(bound_again.layout_hash, LAYOUT_4_1_2_HASH);
}

// ---------------------------------------------------------------------------
// 3. corrected 高度默认全频（无 HP/LP/18 ms）
// ---------------------------------------------------------------------------

#[test]
fn roles_fullrange_height_corrected_baseline_identity_everywhere() {
    let bound = bound_412();
    let corrections = corrected_baseline(&bound);
    assert_eq!(corrections.len(), 7);
    for correction in &corrections {
        assert!(
            correction.is_fullrange_identity(),
            "角色 {} 的 corrected 基线必须是全频 identity",
            correction.role_id
        );
        assert!(!correction.has_band_limit());
        assert!(correction.filters.is_empty());
        assert_eq!(
            correction.canonical_line(),
            format!("role {}: identity", correction.role_id)
        );
    }
    // 高度角色 HFL/HFR 显式检查：全频、无 HP/LP、无 18 ms（本类型无延迟字段）。
    for height in ["HFL", "HFR"] {
        let correction = corrections
            .iter()
            .find(|correction| correction.role_id == height)
            .expect("高度角色必须存在于 4.1.2");
        assert!(correction.is_fullrange_identity());
        assert!(!correction.has_band_limit());
    }
    let canonical: String = corrections
        .iter()
        .map(|correction| correction.canonical_line())
        .collect::<Vec<String>>()
        .join("\n");
    for forbidden in ["1800", "18000", "10000", "18ms", "highpass", "lowpass"] {
        assert!(
            !canonical.contains(forbidden),
            "corrected 基线不得含旧高度带限/18 ms 痕迹：{forbidden}"
        );
    }
    assert_eq!(corrections_hash(&corrections), CORRECTIONS_BASELINE_HASH);
    // 空显式配置 = 基线（逐项相等）。
    let baseline_via_config = RoleCorrectionConfig::empty()
        .apply(&bound)
        .expect("空配置应等价基线");
    assert_eq!(baseline_via_config, corrections);
}

// ---------------------------------------------------------------------------
// 4. 不按 role 名注入（高度/任意名字都无默认滤波）
// ---------------------------------------------------------------------------

#[test]
fn roles_fullrange_height_no_role_name_injection() {
    let bound = bound_412();

    // 只显式配置 FL，其它角色（含 HFL/HFR）不得被注入任何滤波。
    let config = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole FL: target_fir\n",
    )
    .expect("显式配置必须可解析");
    let corrections = config.apply(&bound).expect("显式配置应用");
    for correction in &corrections {
        if correction.role_id == "FL" {
            assert_eq!(correction.filters, vec![RoleFilterKind::TargetFir]);
        } else {
            assert!(
                correction.is_fullrange_identity(),
                "角色 {} 未被配置却被注入滤波（禁止按 role 名注入）",
                correction.role_id
            );
        }
    }

    // 任意角色名（含字面 HFL/HEIGHT_L 等）在无显式配置时都必须是 identity：
    // 本模块不按名字注入任何默认滤波/延迟。
    let synthetic = minimal_doc(&format!(
        "{}{}{}{}",
        role_line("HEIGHT_L", "[4]"),
        role_line("HEIGHT_R", "[5]"),
        role_line("HFL", "[0]"),
        role_line("SUB1", "[7]"),
    ));
    let synthetic_presets =
        LayoutPresets::parse_overlay(synthetic.as_bytes()).expect("合成改名布局可解析");
    let synthetic_bound = synthetic_presets
        .bind_4_1_2(22)
        .expect("合成改名布局可绑定");
    let synthetic_baseline = corrected_baseline(&synthetic_bound);
    assert_eq!(synthetic_baseline.len(), 4);
    for correction in &synthetic_baseline {
        assert!(
            correction.is_fullrange_identity(),
            "角色 {} 无显式配置时必须全频 identity（禁止按 role 名注入）",
            correction.role_id
        );
        assert!(!correction.has_band_limit());
    }
    // 名字进入绑定 hash（语义变化），但修正链仍全部 identity。
    assert_ne!(synthetic_bound.layout_hash, bound.layout_hash);
}

// ---------------------------------------------------------------------------
// 5. 显式滤波只在配置时存在
// ---------------------------------------------------------------------------

#[test]
fn roles_fullrange_height_explicit_filters_only_when_configured() {
    let bound = bound_412();
    let config = RoleCorrectionConfig::parse_text(
        "# 显式配置示例\nprofile corrected_fullrange_height\nrole HFL: crossover_highpass\nrole SUB1: crossover_lowpass, modal_peq\n",
    )
    .expect("显式滤波配置必须可解析");
    assert_eq!(
        config.role_filters("HFL"),
        Some(&[RoleFilterKind::CrossoverHighPass][..])
    );
    assert_eq!(
        config.role_filters("SUB1"),
        Some(&[RoleFilterKind::CrossoverLowPass, RoleFilterKind::ModalPeq][..])
    );
    let corrections = config.apply(&bound).expect("显式配置应用");
    let hfl = corrections
        .iter()
        .find(|correction| correction.role_id == "HFL")
        .unwrap();
    assert_eq!(hfl.filters, vec![RoleFilterKind::CrossoverHighPass]);
    assert!(hfl.has_band_limit());
    let sub1 = corrections
        .iter()
        .find(|correction| correction.role_id == "SUB1")
        .unwrap();
    assert_eq!(
        sub1.filters,
        vec![RoleFilterKind::CrossoverLowPass, RoleFilterKind::ModalPeq]
    );
    let hfr = corrections
        .iter()
        .find(|correction| correction.role_id == "HFR")
        .unwrap();
    assert!(hfr.is_fullrange_identity());
    assert_ne!(corrections_hash(&corrections), CORRECTIONS_BASELINE_HASH);

    // 显式 identity 与“未列出”等价（hash 回到基线）。
    let explicit_identity = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole HFL: identity\n",
    )
    .expect("显式 identity 可解析");
    assert_eq!(
        corrections_hash(&explicit_identity.apply(&bound).unwrap()),
        CORRECTIONS_BASELINE_HASH
    );

    // 全部已知 kind 都能显式解析（不是只支持一两种）。
    for kind in RoleFilterKind::ALL {
        let text = format!(
            "profile corrected_fullrange_height\nrole FL: {}\n",
            kind.as_str()
        );
        let parsed = RoleCorrectionConfig::parse_text(&text).expect("已知 kind 必须可解析");
        assert_eq!(parsed.role_filters("FL"), Some(&[kind][..]));
        assert_eq!(
            kind.is_band_limit(),
            matches!(
                kind,
                RoleFilterKind::CrossoverHighPass | RoleFilterKind::CrossoverLowPass
            )
        );
    }
}

// ---------------------------------------------------------------------------
// 6. 未知滤波 kind 整份拒收
// ---------------------------------------------------------------------------

#[test]
fn roles_unknown_filter_kind_rejected() {
    for text in [
        "profile corrected_fullrange_height\nrole HFL: hp1800\n",
        "profile corrected_fullrange_height\nrole HFL: lowpass\n",
        "profile corrected_fullrange_height\nrole HFL: highpass\n",
        "profile corrected_fullrange_height\nrole HFL: height_bandlimit\n",
        "profile corrected_fullrange_height\nrole HFL: 18ms\n",
    ] {
        let error = RoleCorrectionConfig::parse_text(text).expect_err("未知滤波 kind 必须拒收");
        assert_eq!(error.code, RolesErrorCode::CorrectionFilterUnknown);
        assert_eq!(error.code_str(), "E_ROLES_CORRECTION_FILTER_UNKNOWN");
        assert!(
            error.field.contains("HFL"),
            "错误必须带角色路径：{}",
            error.field
        );
    }
    // identity 与显式滤波混写 = 语法错误（不允许隐式叠加）。
    let error = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole HFL: identity, crossover_highpass\n",
    )
    .expect_err("identity 与滤波混写必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionSyntax);
    // 同一 kind 重复 = 语法错误。
    let error = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole HFL: target_fir, target_fir\n",
    )
    .expect_err("重复 kind 必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionSyntax);
    // 非法 kind 字面值（大小写/别名）也不接受。
    assert!(RoleFilterKind::parse("CrossoverHighPass").is_none());
    assert!(RoleFilterKind::parse("hp").is_none());
}

// ---------------------------------------------------------------------------
// 7. corrected profile 严格性（legacy_s1 不可表达）
// ---------------------------------------------------------------------------

#[test]
fn roles_correction_profile_legacy_rejected() {
    let error = RoleCorrectionConfig::parse_text("profile legacy_s1\n")
        .expect_err("legacy_s1 不由此模块产出");
    assert_eq!(error.code, RolesErrorCode::CorrectionProfile);
    assert_eq!(error.code_str(), "E_ROLES_CORRECTION_PROFILE");
    // 缺 profile 行。
    let error =
        RoleCorrectionConfig::parse_text("role FL: identity\n").expect_err("缺 profile 行必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionProfile);
    // profile 重复。
    let error = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nprofile corrected_fullrange_height\n",
    )
    .expect_err("profile 重复必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionProfile);
    // 未知指令。
    let error = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\ninject FL highpass\n",
    )
    .expect_err("未知指令必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionSyntax);
    // 角色重复。
    let error = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole HFL: identity\nrole HFL: identity\n",
    )
    .expect_err("角色重复必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionRole);
    // 非法角色 id。
    let error = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole bad id: identity\n",
    )
    .expect_err("非法角色 id 必须拒收");
    assert_eq!(error.code, RolesErrorCode::RoleId);

    // 引用不在 layout 的角色 = 拒收（禁止跨布局套用）。
    let bound = bound_412();
    let config = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole SUB9: modal_peq\n",
    )
    .expect("语法合法（角色存在性在 apply 校验）");
    let error = config.apply(&bound).expect_err("未知角色必须拒收");
    assert_eq!(error.code, RolesErrorCode::CorrectionRole);

    // 非空配置缺 profile（程序化构造）= 拒收。
    let mut config = RoleCorrectionConfig::empty();
    config
        .set_role("HFL", vec![RoleFilterKind::CrossoverHighPass])
        .expect("set_role");
    let error = config.apply(&bound).expect_err("非空配置必须显式 profile");
    assert_eq!(error.code, RolesErrorCode::CorrectionProfile);
}

// ---------------------------------------------------------------------------
// 8. MIMO participants 显式 5（非 HP1800/role 名推导）
// ---------------------------------------------------------------------------

#[test]
fn roles_mimo_participants_explicit_five_from_scope() {
    let bound = bound_412();
    assert_eq!(P1_MIMO_PARTICIPANT_IDS, ["FL", "FR", "SL", "SR", "SUB1"]);
    let scope = MimoScope::p1_4_1_2_explicit();
    assert_eq!(scope.layout_id, "4.1.2");
    assert_eq!(
        scope.participants(),
        &["FL", "FR", "SL", "SR", "SUB1"]
            .iter()
            .map(|role_id| role_id.to_string())
            .collect::<Vec<String>>()[..]
    );

    let participants = scope.validate(&bound).expect("P1 显式 scope 必须可绑定");
    assert_eq!(participants.m(), 5);
    assert_eq!(participants.layout_id, "4.1.2");
    assert_eq!(participants.layout_hash, LAYOUT_4_1_2_HASH);
    let ids: Vec<&str> = participants
        .participants
        .iter()
        .map(|participant| participant.role_id.as_str())
        .collect();
    assert_eq!(ids, ["FL", "FR", "SL", "SR", "SUB1"]);
    // dense 0..4 → 角色索引 [0,1,2,3,6] → DAC 0/1/2/3/7（另表保存，不与容器 scatter 混用）。
    assert_eq!(participants.dense_to_role(), vec![0, 1, 2, 3, 6]);
    assert_eq!(participants.dense_to_dac(), vec![0, 1, 2, 3, 7]);
    assert_eq!(participants.role_indices(), vec![0, 1, 2, 3, 6]);
    assert!(participants.contains_role_index(6));
    assert!(!participants.contains_role_index(4));
    assert!(!participants.contains_role_index(5));
    for (index, participant) in participants.participants.iter().enumerate() {
        assert_eq!(participant.dense_index, index as u16);
    }
    // binding hash 确定性 + canonical 文本可复算。
    let hash = participants.binding_hash();
    assert_eq!(hash.len(), 64);
    assert_eq!(
        hash,
        MimoScope::p1_4_1_2_explicit()
            .validate(&bound)
            .unwrap()
            .binding_hash()
    );
    let canonical = participants.canonical();
    assert!(canonical.starts_with(MIMO_PARTICIPANTS_HASH_DOMAIN));
    assert!(canonical.contains("layout=4.1.2"));
    assert!(canonical.contains("dense4=SUB1:role6:dac7"));
}

// ---------------------------------------------------------------------------
// 9. participants 与滤波/高度无关（显式数据，不推导）
// ---------------------------------------------------------------------------

#[test]
fn roles_mimo_participants_not_derived_from_filters_or_heights() {
    let bound = bound_412();
    let base = MimoScope::p1_4_1_2_explicit()
        .validate(&bound)
        .expect("基线 participants");
    // 给高度显式加带限滤波：participants 不得变化（不是按滤波/频响推导）。
    let config = RoleCorrectionConfig::parse_text(
        "profile corrected_fullrange_height\nrole HFL: crossover_highpass\nrole HFR: crossover_highpass\n",
    )
    .expect("显式高度带限配置");
    let corrections = config.apply(&bound).expect("应用");
    assert!(corrections
        .iter()
        .any(|correction| correction.has_band_limit()));
    let after = MimoScope::p1_4_1_2_explicit()
        .validate(&bound)
        .expect("带限后 participants");
    assert_eq!(after.participants, base.participants);

    // 显式 2 路 scope → M=2，不会自动把高度/SUB 加进来。
    let two_way = MimoScope::new("4.1.2", vec!["FL".to_string(), "FR".to_string()])
        .expect("显式 2 路 scope")
        .validate(&bound)
        .expect("2 路 scope 绑定");
    assert_eq!(two_way.m(), 2);
    assert_eq!(two_way.dense_to_dac(), vec![0, 1]);
    assert!(!two_way.contains_role_index(6));
    assert!(!two_way.contains_role_index(4));

    // 显式 5 路顺序不同 = 调用方数据（本模块不做任何重排/推断）。
    let reordered = MimoScope::new(
        "4.1.2",
        vec![
            "SUB1".to_string(),
            "FL".to_string(),
            "FR".to_string(),
            "SL".to_string(),
            "SR".to_string(),
        ],
    )
    .expect("显式重排 scope")
    .validate(&bound)
    .expect("重排 scope 绑定");
    assert_eq!(reordered.dense_to_dac(), vec![7, 0, 1, 2, 3]);
    assert_eq!(reordered.m(), 5);
}

// ---------------------------------------------------------------------------
// 10. participants 负向矩阵
// ---------------------------------------------------------------------------

#[test]
fn roles_mimo_participants_negative_matrix() {
    let bound = bound_412();
    let error = MimoScope::new("4.1.2", Vec::new()).expect_err("空 participants 必须拒收");
    assert_eq!(error.code, RolesErrorCode::Participants);
    let error = MimoScope::new("4.1.2", vec!["FL".to_string(), "FL".to_string()])
        .expect_err("重复 participant 必须拒收");
    assert_eq!(error.code, RolesErrorCode::Participants);
    let error =
        MimoScope::new("4.1.2", vec!["bad id".to_string()]).expect_err("非法角色 id 必须拒收");
    assert_eq!(error.code, RolesErrorCode::RoleId);
    let too_many: Vec<String> = (0..9).map(|index| format!("R{index}")).collect();
    let error = MimoScope::new("4.1.2", too_many).expect_err("超上限 participants 必须拒收");
    assert_eq!(error.code, RolesErrorCode::Participants);

    let unknown = MimoScope::new("4.1.2", vec!["FL".to_string(), "SUB9".to_string()])
        .expect("语法合法")
        .validate(&bound)
        .expect_err("未知 participant 角色必须拒收");
    assert_eq!(unknown.code, RolesErrorCode::Participants);
    let mismatch = MimoScope::new("5.3.2", vec!["FL".to_string()])
        .expect("语法合法")
        .validate(&bound)
        .expect_err("scope/layout 不一致必须拒收");
    assert_eq!(mismatch.code, RolesErrorCode::Participants);
    let bad_layout =
        MimoScope::new("bad layout", vec!["FL".to_string()]).expect_err("非法 layout id 必须拒收");
    assert_eq!(bad_layout.code, RolesErrorCode::LayoutId);
}

// ---------------------------------------------------------------------------
// 11. layout hash 语义稳定 + 映射敏感
// ---------------------------------------------------------------------------

#[test]
fn roles_layout_hash_semantic_stable_and_mapping_sensitive() {
    let base = overlay();
    let base_hash = base.layout_hash("4.1.2").expect("4.1.2 hash");

    // 注释/空白/仅 label 变化不改变语义 hash（源字节 hash 变化）。
    let reformatted = format!(
        "# reformatted copy for hash stability test\n\n{}",
        OVERLAY_PRESETS.replace(
            "label: \"4.1.2（用户当前布局）\"",
            "label: \"4.1.2 user layout\""
        )
    );
    let reformatted_presets =
        LayoutPresets::parse_overlay(reformatted.as_bytes()).expect("重排文档可解析");
    assert_eq!(reformatted_presets.layout_hash("4.1.2").unwrap(), base_hash);
    assert_ne!(reformatted_presets.source_sha256(), OVERLAY_SHA256);

    // 映射变化（SUB1 7→6）改变语义 hash/容器/空槽。
    let changed = OVERLAY_PRESETS.replace(
        "{id: SUB1, label: Subwoofer, outputs: [7]}",
        "{id: SUB1, label: Subwoofer, outputs: [6]}",
    );
    assert_ne!(changed, OVERLAY_PRESETS);
    let changed_presets = LayoutPresets::parse_overlay(changed.as_bytes()).expect("改映射可解析");
    assert_ne!(changed_presets.layout_hash("4.1.2").unwrap(), base_hash);
    let changed_bound = changed_presets.bind_4_1_2(22).expect("改映射可绑定");
    assert_eq!(changed_bound.first_output("SUB1"), Some(6));
    assert_eq!(changed_bound.container_channels, 7);
    assert_eq!(changed_bound.unused_container_channels(), Vec::<u16>::new());

    // 角色顺序变化（FL/FR 互换）改变角色索引语义 → hash 变化。
    let swapped = OVERLAY_PRESETS.replace(
        "      - {id: FL, label: Front L, outputs: [0]}\n      - {id: FR, label: Front R, outputs: [1]}",
        "      - {id: FR, label: Front R, outputs: [1]}\n      - {id: FL, label: Front L, outputs: [0]}",
    );
    assert_ne!(swapped, OVERLAY_PRESETS);
    let swapped_presets = LayoutPresets::parse_overlay(swapped.as_bytes()).expect("交换顺序可解析");
    assert_ne!(swapped_presets.layout_hash("4.1.2").unwrap(), base_hash);
    let swapped_bound = swapped_presets.bind_4_1_2(22).expect("交换顺序可绑定");
    assert_eq!(
        role_ids(&swapped_bound),
        ["FR", "FL", "SL", "SR", "HFL", "HFR", "SUB1"]
    );
}

// ---------------------------------------------------------------------------
// 12. 其它布局通用解析（非只支持 4.1.2 硬编码）
// ---------------------------------------------------------------------------

#[test]
fn roles_layouts_other_than_4_1_2_parse_generically() {
    let presets = overlay();
    let bound = presets.bind_output_layout("5.3.2", 22).expect("5.3.2 bind");
    assert_eq!(bound.role_count(), 10);
    assert_eq!(bound.container_channels, 10);
    assert_eq!(bound.scatter(), vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(bound.unused_container_channels(), Vec::<u16>::new());
    assert_eq!(bound.layout_hash, LAYOUT_5_3_2_HASH);

    let two_one = presets.bind_output_layout("2.1", 22).expect("2.1 bind");
    assert_eq!(two_one.role_count(), 3);
    assert_eq!(two_one.container_channels, 3);
    assert_eq!(two_one.scatter(), vec![0, 1, 2]);
    assert_eq!(
        presets.bind_output_layout("4.0", 22).unwrap().role_count(),
        4
    );

    let error = presets
        .bind_output_layout("9.9.9", 22)
        .expect_err("未知布局必须拒收");
    assert_eq!(error.code, RolesErrorCode::LayoutUnknown);
}

// ---------------------------------------------------------------------------
// 13. input_layouts / remap_presets 解析
// ---------------------------------------------------------------------------

#[test]
fn roles_input_layouts_and_remap_presets_parse() {
    let presets = overlay();
    assert_eq!(presets.input_layouts().len(), 9);
    assert_eq!(presets.remap_presets().len(), 1);

    let pc2 = presets.input_layout("pc-2ch").expect("pc-2ch");
    assert_eq!(pc2.channels, Some(2));
    assert_eq!(pc2.roles.len(), 2);
    assert_eq!(pc2.roles[0].id, "FL");
    assert_eq!(pc2.roles[0].stream, 0);
    assert_eq!(pc2.roles[0].channel, 0);
    assert_eq!(pc2.roles[1].id, "FR");
    assert_eq!(pc2.roles[1].channel, 1);

    let h16 = presets.input_layout("h16-5.1.2").expect("h16-5.1.2");
    assert_eq!(h16.streams, vec![6, 2]);
    assert_eq!(h16.roles.len(), 8);
    assert_eq!(h16.roles[6].id, "TFL");
    assert_eq!(h16.roles[6].stream, 1);
    assert_eq!(h16.roles[6].channel, 0);
    // auto 通道数 = None（以实测为准）。
    assert_eq!(h16.channels, None);

    let remap = presets.remap_preset("5.1.2->5.1.2").expect("5.1.2->5.1.2");
    assert_eq!(remap.input, "h16-5.1.2");
    assert_eq!(remap.output, "5.1.2");
    assert_eq!(remap.routes.len(), 8);
    assert!(remap
        .routes
        .iter()
        .any(|route| route.src == "LFE" && route.dest == "SUB1"));
    assert!(remap
        .routes
        .iter()
        .any(|route| route.src == "TFL" && route.dest == "HFL"));

    // remap 引用未知角色 = 拒收。
    let bad = OVERLAY_PRESETS.replace("{src: FL, dest: FL}", "{src: FL, dest: NOPE}");
    let error = parse_error(bad.as_bytes());
    assert_eq!(error.code, RolesErrorCode::Remap);
}

// ---------------------------------------------------------------------------
// 14. 设备宽度校验
// ---------------------------------------------------------------------------

#[test]
fn roles_device_width_validation() {
    let bound = bound_412();
    bound.validate_output_device_width(22).expect("22ch 合法");
    bound.validate_output_device_width(8).expect("8ch 最小合法");
    let error = bound
        .validate_output_device_width(7)
        .expect_err("7ch 必须拒收");
    assert_eq!(error.code, RolesErrorCode::DeviceWidth);
    assert_eq!(error.code_str(), "E_ROLES_DEVICE_WIDTH");
    let error = overlay().bind_4_1_2(7).expect_err("bind 时也必须校验宽度");
    assert_eq!(error.code, RolesErrorCode::DeviceWidth);
    let error = overlay()
        .bind_output_layout("5.3.2", 9)
        .expect_err("5.3.2 需要 10ch");
    assert_eq!(error.code, RolesErrorCode::DeviceWidth);
    overlay()
        .bind_output_layout("5.3.2", 10)
        .expect("5.3.2 10ch 合法");
}

// ---------------------------------------------------------------------------
// 15. role_mask + T13 互通（layout 交叉校验/校准合并）
// ---------------------------------------------------------------------------

#[test]
fn roles_role_mask_and_t13_interop() {
    let bound = bound_412();
    assert_eq!(bound.role_mask(&["FL", "FR"]).unwrap(), 0b0000_0011);
    assert_eq!(bound.role_mask(&[]).unwrap(), 0);
    assert_eq!(bound.role_mask(&["SUB1"]).unwrap(), 1 << 6);
    let error = bound.role_mask(&["NOPE"]).expect_err("未知角色必须拒收");
    assert_eq!(error.code, RolesErrorCode::RoleId);
    let error = bound
        .role_mask(&["FL", "FL"])
        .expect_err("重复角色必须拒收");
    assert_eq!(error.code, RolesErrorCode::RoleId);

    // T13 LayoutRoleTable：4.1.2 校准示例可交叉校验（layout/角色/index 一致）。
    let table = bound.to_layout_role_table();
    assert_eq!(table.layout_id, "4.1.2");
    assert_eq!(table.roles.len(), 7);
    assert_eq!(table.first_output("SUB1"), Some(7));
    assert!(table.contains("HFL"));
    let document =
        parse_calibration_yaml_with_layout(CALIBRATION_412_EXAMPLE.as_bytes(), Some(&table))
            .expect("4.1.2 校准示例应通过 layout 交叉校验");
    assert!(document.layout_checked);
    assert_eq!(document.channels.len(), 7);

    // 跨布局套用 = 拒收。
    let other_table = overlay()
        .bind_output_layout("5.3.2", 22)
        .unwrap()
        .to_layout_role_table();
    let error =
        parse_calibration_yaml_with_layout(CALIBRATION_412_EXAMPLE.as_bytes(), Some(&other_table))
            .expect_err("4.1.2 校准套 5.3.2 布局必须拒收");
    assert_eq!(error.code_str(), "E_CAL_LAYOUT_MISMATCH");

    // T13 RenderRoleBase：未校准 identity 基线（高度同样无默认带限），合并示例校准。
    let base_roles = bound.render_role_bases();
    assert_eq!(base_roles.len(), 7);
    assert_eq!(base_roles[0].name, "FL");
    assert_eq!(base_roles[0].index, 0);
    assert_eq!(base_roles[6].name, "SUB1");
    assert_eq!(base_roles[6].index, 7);
    for role in &base_roles {
        assert_eq!(role.delay_samples, 0);
        assert!(role.peq.is_empty());
        assert_eq!(role.polarity, Polarity::Normal);
    }
    let merged = merge_calibration(&document, Some(&table), &base_roles, 48_000)
        .expect("校准合并（示例 fs=48k）");
    assert_eq!(merged.roles.len(), 7);
    let hfl = merged
        .roles
        .iter()
        .find(|role| role.name == "HFL")
        .expect("HFL 合并角色");
    assert_eq!(hfl.delay_samples, 3);
    assert_eq!(hfl.polarity, Polarity::Normal);
    assert!(hfl.peq.is_empty());
    assert!(hfl.calibrated);
}

// ---------------------------------------------------------------------------
// 16. schema/严格性负向矩阵
// ---------------------------------------------------------------------------

#[test]
fn roles_overlay_schema_and_strictness_negative_matrix() {
    let valid_roles = format!("{}{}", role_line("FL", "[0]"), role_line("FR", "[1]"));
    let valid = minimal_doc(&valid_roles);
    LayoutPresets::parse_overlay(valid.as_bytes()).expect("最小合法文档");

    // schema 不匹配 / 缺失。
    let error = parse_error(
        valid
            .replace(LAYOUT_PRESETS_SCHEMA, "tspi-layout-presets/v1")
            .as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::Schema);
    let error = parse_error(
        minimal_doc(&valid_roles)
            .replace("schema: tspi-layout-presets/v2\n", "")
            .as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::MissingField);
    // 未知顶层字段。
    let error = parse_error(format!("{valid}extra: 1\n").as_bytes());
    assert_eq!(error.code, RolesErrorCode::FieldUnknown);
    assert_eq!(error.field, "$.extra");
    // 未知嵌套字段（layout 级 / role 级）。
    let error = parse_error(
        "schema: tspi-layout-presets/v2\noutput_layouts:\n  \"4.1.2\":\n    extra: 1\n    roles:\n"
            .to_string()
            .as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::FieldUnknown);
    let error = parse_error(minimal_doc("      - {id: FL, outputs: [0], extra: 1}\n").as_bytes());
    assert_eq!(error.code, RolesErrorCode::FieldUnknown);
    // roles 缺失/为空/超上限。
    let error = parse_error(
        "schema: tspi-layout-presets/v2\noutput_layouts:\n  \"4.1.2\":\n    label: x\n".as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::MissingField);
    // roles: 后无内容 = null（FieldType）；显式空数组 = LayoutRoles。
    let error = parse_error(minimal_doc("").as_bytes());
    assert_eq!(error.code, RolesErrorCode::FieldType);
    let error = parse_error(
        "schema: tspi-layout-presets/v2\noutput_layouts:\n  \"4.1.2\":\n    roles: []\n".as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::LayoutRoles);
    let mut too_many = String::new();
    for index in 0..17u16 {
        too_many.push_str(&role_line(&format!("R{index}"), &format!("[{}]", index)));
    }
    let error = parse_error(minimal_doc(&too_many).as_bytes());
    assert_eq!(error.code, RolesErrorCode::LayoutRoles);
    // 角色 id 重复 / 非法。
    let error = parse_error(
        minimal_doc(&format!(
            "{}{}",
            role_line("FL", "[0]"),
            role_line("FL", "[1]")
        ))
        .as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::RoleId);
    let error = parse_error(minimal_doc(&role_line("BAD ID", "[0]")).as_bytes());
    assert_eq!(error.code, RolesErrorCode::RoleId);
    // outputs 缺失/为空/类型错/越界/重复。
    let error = parse_error(minimal_doc("      - {id: FL}\n").as_bytes());
    assert_eq!(error.code, RolesErrorCode::MissingField);
    let error = parse_error(minimal_doc(&role_line("FL", "[]")).as_bytes());
    assert_eq!(error.code, RolesErrorCode::RoleOutputs);
    let error = parse_error(minimal_doc(&role_line("FL", "0")).as_bytes());
    assert_eq!(error.code, RolesErrorCode::FieldType);
    let error = parse_error(minimal_doc(&role_line("FL", "[22]")).as_bytes());
    assert_eq!(error.code, RolesErrorCode::RoleOutputs);
    let error = parse_error(
        minimal_doc(&format!(
            "{}{}",
            role_line("FL", "[0]"),
            role_line("FR", "[0]")
        ))
        .as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::RoleOutputs);
    // 布局 id 非法。
    let error = parse_error(
        "schema: tspi-layout-presets/v2\noutput_layouts:\n  \"bad id\":\n    roles:\n      - {id: FL, outputs: [0]}\n"
            .as_bytes(),
    );
    assert_eq!(error.code, RolesErrorCode::LayoutId);
    // 顶层不是 mapping / 重复 key / 非 UTF-8。
    let error = parse_error("- a\n- b\n".as_bytes());
    assert_eq!(error.code, RolesErrorCode::YamlRoot);
    let error = parse_error("schema: a\nschema: b\n".as_bytes());
    assert_eq!(error.code, RolesErrorCode::YamlSyntax);
    let error = parse_error(&[0xFFu8, 0xFE, 0x00]);
    assert_eq!(error.code, RolesErrorCode::Encoding);
    // 缺失 ':' / 流式未闭合 / 值后多余字符。
    let error = parse_error("schema tspi-layout-presets/v2\n".as_bytes());
    assert_eq!(error.code, RolesErrorCode::YamlSyntax);
    let error = parse_error("schema: {x: 1\n".as_bytes());
    assert_eq!(error.code, RolesErrorCode::YamlSyntax);
    let error = parse_error("schema: [1] junk\n".as_bytes());
    assert_eq!(error.code, RolesErrorCode::YamlSyntax);
}

// ---------------------------------------------------------------------------
// 17. YAML 子集：flow/引号/注释/块映射序列项；不支持特性拒收
// ---------------------------------------------------------------------------

#[test]
fn roles_yaml_subset_flow_quoted_scalars_and_unsupported() {
    let text = "schema: \"tspi-layout-presets/v2\"\noutput_layouts:\n  \"4.1.2\":\n    label: 'Single ''quoted'''\n    description: \"with \\\"escaped\\\" quote and # hash inside\"\n    roles:\n      - id: FL\n        outputs: [0]\n        label: Front L\n      - id: FR\n        outputs: [1, 2]\n";
    let presets = LayoutPresets::parse_overlay(text.as_bytes()).expect("合法 YAML 子集");
    let layout = presets.output_layout("4.1.2").expect("4.1.2");
    assert_eq!(layout.label.as_deref(), Some("Single 'quoted'"));
    assert_eq!(
        layout.description.as_deref(),
        Some("with \"escaped\" quote and # hash inside")
    );
    assert_eq!(layout.roles[0].id, "FL");
    assert_eq!(layout.roles[0].label.as_deref(), Some("Front L"));
    assert_eq!(layout.roles[1].outputs, vec![1, 2]);
    // 多输出角色：scatter 取首槽位，容器宽度 = max+1。
    let bound = presets.bind_4_1_2(4).expect("4ch 设备绑定");
    assert_eq!(bound.container_channels, 3);
    assert_eq!(bound.scatter(), vec![0, 1]);
    assert_eq!(bound.used_container_channels(), vec![0, 1, 2]);

    // 不支持的 YAML 特性：块标量/anchor/alias/tag/tab 缩进/多文档。
    for (text, expected) in [
        (
            "schema: tspi-layout-presets/v2\noutput_layouts: |\n  x\n",
            RolesErrorCode::YamlUnsupported,
        ),
        (
            "schema: &anchor tspi-layout-presets/v2\n",
            RolesErrorCode::YamlUnsupported,
        ),
        ("schema: *alias\n", RolesErrorCode::YamlUnsupported),
        (
            "schema: !tag tspi-layout-presets/v2\n",
            RolesErrorCode::YamlUnsupported,
        ),
        (
            "\tschema: tspi-layout-presets/v2\n",
            RolesErrorCode::YamlUnsupported,
        ),
        (
            "---\nschema: tspi-layout-presets/v2\n",
            RolesErrorCode::YamlUnsupported,
        ),
    ] {
        let error = parse_error(text.as_bytes());
        assert_eq!(error.code, expected, "输入：{text:?}");
    }
    // 引号未闭合 = 语法错误。
    let error = parse_error("schema: \"unterminated\n".as_bytes());
    assert_eq!(error.code, RolesErrorCode::YamlSyntax);
}

// ---------------------------------------------------------------------------
// 18. Web 副本 drift 报告语义（仅注释变化 = 语义一致但字节不同）
// ---------------------------------------------------------------------------

#[test]
fn roles_web_template_drift_report_and_ensure_identical() {
    let commented = format!("# drift comment only\n{}", WEB_PRESETS);
    let drift = check_web_template_drift(OVERLAY_PRESETS.as_bytes(), commented.as_bytes())
        .expect("drift 检查");
    assert!(!drift.byte_identical);
    assert!(drift.semantic_identical);
    assert!(drift.differences.is_empty());
    drift
        .ensure_identical()
        .expect("仅注释变化不构成语义 drift");
    assert_eq!(drift.overlay_source_sha256, OVERLAY_SHA256);
    assert_ne!(drift.web_source_sha256, OVERLAY_SHA256);

    // 语义变化（4.1.2 少一个角色）必须被 drift 检出。
    let web_missing_role =
        WEB_PRESETS.replace("      - {id: SUB1, label: Subwoofer, outputs: [7]}\n", "");
    assert_ne!(web_missing_role, WEB_PRESETS);
    let drift = check_web_template_drift(OVERLAY_PRESETS.as_bytes(), web_missing_role.as_bytes())
        .expect("缺角色副本仍可解析");
    assert!(!drift.semantic_identical);
    assert!(drift.differences.iter().any(|line| line.contains("SUB1")));
    assert_eq!(
        drift.ensure_identical().unwrap_err().code,
        RolesErrorCode::DriftMismatch
    );
}

// ---------------------------------------------------------------------------
// 19. 错误码目录 + Display/Error 契约
// ---------------------------------------------------------------------------

#[test]
fn roles_error_codes_catalog_and_display() {
    assert_eq!(RolesErrorCode::ALL.len(), 22);
    for code in RolesErrorCode::ALL {
        assert!(code.as_str().starts_with("E_ROLES_"));
    }
    let error = RolesError::with_field(
        RolesErrorCode::DeviceWidth,
        "output_layouts.4.1.2.device_channels",
        "实际播放通道 7 不足",
    );
    assert_eq!(error.code_str(), "E_ROLES_DEVICE_WIDTH");
    let text = format!("{error}");
    assert!(text.contains("E_ROLES_DEVICE_WIDTH"));
    assert!(text.contains("output_layouts.4.1.2.device_channels"));
    assert!(text.contains("实际播放通道 7 不足"));
    let error = RolesError::new(RolesErrorCode::Schema, "x");
    assert_eq!(format!("{error}"), "E_ROLES_SCHEMA: x");
    let boxed: Box<dyn std::error::Error> = Box::new(error);
    assert!(boxed.to_string().contains("E_ROLES_SCHEMA"));
}

// ---------------------------------------------------------------------------
// 20. 文件读取入口（仓库 overlay 只读）
// ---------------------------------------------------------------------------

#[test]
fn roles_load_overlay_file_from_repo() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("../../../audio/overlay/etc/tspi-audio/layout-presets.yml");
    let loaded = LayoutPresets::load_overlay_file(&path).expect("仓库 overlay 必须可读");
    assert_eq!(loaded.source_sha256(), OVERLAY_SHA256);
    assert_eq!(loaded.layout_hash("4.1.2").unwrap(), LAYOUT_4_1_2_HASH);

    let error = LayoutPresets::load_overlay_file(std::path::Path::new(
        "no-such-dir/definitely-missing-layout-presets.yml",
    ))
    .expect_err("缺失文件必须报 E_ROLES_IO");
    assert_eq!(error.code, RolesErrorCode::Io);
}
