//! `khaen-cal-ir-draft/v1` 校准 IR 草案：解析/绑定/stale 六原因/P 与 D/polarity 应用（对齐 T13）。

use khaen_file_runner::ir::{
    binding_json, parse_ir, IrCurrent, Polarity, PostEncoding, StaleReason, IR_HASH_ALGO,
    IR_MAX_ROLES, IR_SCHEMA,
};
use khaen_file_runner::json::Json;
use khaen_file_runner::{E_FILE_DELAY, E_FILE_IR, E_FILE_TAPS};

use crate::common::{
    ir_draft, ir_role_metadata, map_mut, nested_mut, HASH_A, HASH_B, ROLE_SCATTER,
};

fn current() -> IrCurrent {
    let draft = ir_role_metadata(&[0; 6], &[false; 6]);
    let parsed = parse_ir(&draft).expect("parse");
    IrCurrent {
        fs_hz: 48_000,
        layout_id: Some("4.1.2".to_string()),
        role_indices: parsed.role_indices(),
        profile_sha256: HASH_A.to_string(),
        target_sha256: None,
        calibration_sha256: HASH_B.to_string(),
    }
}

#[test]
fn ir_parse_valid_role_metadata() {
    let draft = ir_role_metadata(
        &[2, 0, 1, 0, 0, 3],
        &[false, true, false, false, false, false],
    );
    let parsed = parse_ir(&draft).expect("valid ir");
    assert_eq!(parsed.schema, IR_SCHEMA);
    assert_eq!(parsed.ir_version, 1);
    assert_eq!(parsed.ir_hash, HASH_A);
    assert_eq!(parsed.hash_algo, IR_HASH_ALGO);
    assert_eq!(parsed.encoding, PostEncoding::RoleMetadata);
    assert_eq!(parsed.source_schema, "khaen-calibration/v1");
    assert_eq!(parsed.source_sha256, HASH_B);
    assert_eq!(parsed.source_sample_rate, 48_000);
    assert_eq!(parsed.layout.as_deref(), Some("4.1.2"));
    assert_eq!(parsed.profile.profile_id, "t14-profile");
    assert_eq!(parsed.profile.profile_sha256, HASH_A);
    assert_eq!(parsed.profile.stereo_mask, vec!["FL", "FR"]);
    assert_eq!(parsed.roles.len(), 6);
    assert_eq!(parsed.post.len(), 6);
    assert_eq!(parsed.role_indices()[0], ("FL".to_string(), 0));
    let fl = parsed.post_role("FL").expect("FL post");
    assert_eq!(fl.delay_samples, 2);
    assert_eq!(fl.p_taps, 1);
    let fr = parsed.post_role("FR").expect("FR post");
    assert_eq!(fr.polarity, Polarity::Invert);
    assert_eq!(fr.polarity.multiplier(), -1);
    assert_eq!(parsed.curve_target_sha256, None);
    assert_eq!(parsed.g_residual_rule, "modal_residual_only");
    assert_eq!(parsed.extensions_count, 0);
}

#[test]
fn ir_parse_valid_coeff_embedded() {
    let draft = ir_draft(
        "coeff_embedded",
        48_000,
        "4.1.2",
        &[2, 0, 0, 0, 0, 1],
        &[false; 6],
        None,
    );
    let parsed = parse_ir(&draft).expect("valid ir");
    assert_eq!(parsed.encoding, PostEncoding::CoeffEmbedded);
    assert_eq!(parsed.post_role("FL").expect("FL").p_taps, 3);
    assert_eq!(parsed.post_role("SUB1").expect("SUB1").p_taps, 2);
}

#[test]
fn ir_rejects_structural_errors() {
    let valid = ir_role_metadata(&[0; 6], &[false; 6]);
    let mut bad = valid.clone();
    map_mut(&mut bad).insert("bogus".to_string(), Json::Bool(true));
    assert_eq!(parse_ir(&bad).expect_err("unknown").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(&mut bad).insert(
        "schema".to_string(),
        Json::String("khaen-cal-ir-draft/v2".to_string()),
    );
    assert_eq!(parse_ir(&bad).expect_err("schema").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(&mut bad).insert("ir_version".to_string(), Json::Number(2.0));
    assert_eq!(parse_ir(&bad).expect_err("version").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(&mut bad).insert("ir_hash".to_string(), Json::String("abc".to_string()));
    assert_eq!(parse_ir(&bad).expect_err("hash").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(&mut bad).insert("hash_algo".to_string(), Json::String("other".to_string()));
    assert_eq!(parse_ir(&bad).expect_err("algo").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(&mut bad).insert("encoding".to_string(), Json::String("both".to_string()));
    assert_eq!(parse_ir(&bad).expect_err("encoding").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(&mut bad).insert("extensions".to_string(), Json::Bool(true));
    assert_eq!(parse_ir(&bad).expect_err("extensions").code(), E_FILE_IR);

    let mut bad = valid.clone();
    map_mut(nested_mut(&mut bad, &["source"])).insert(
        "schema".to_string(),
        Json::String("khaen-calibration/v2".to_string()),
    );
    assert_eq!(parse_ir(&bad).expect_err("source schema").code(), E_FILE_IR);

    // x_ 扩展字段允许。
    let mut extended = valid;
    map_mut(&mut extended).insert("x_note".to_string(), Json::String("ok".to_string()));
    assert!(parse_ir(&extended).is_ok());
}

#[test]
fn ir_rejects_role_table_errors() {
    let valid = ir_role_metadata(&[0; 6], &[false; 6]);
    // 重复 role name。
    let mut bad = valid.clone();
    let roles = match nested_mut(&mut bad, &["roles"]) {
        Json::Array(entries) => entries,
        other => panic!("roles not array: {:?}", other),
    };
    roles[1] = roles[0].clone();
    assert_eq!(parse_ir(&bad).expect_err("dup").code(), E_FILE_IR);
    // role 数超过 T13 上限。
    let mut too_many = ir_draft("role_metadata", 48_000, "4.1.2", &[0; 6], &[false; 6], None);
    let roles = match nested_mut(&mut too_many, &["roles"]) {
        Json::Array(entries) => entries,
        other => panic!("roles not array: {:?}", other),
    };
    while roles.len() <= IR_MAX_ROLES {
        let mut extra = roles[0].clone();
        map_mut(&mut extra).insert(
            "name".to_string(),
            Json::String(format!("R{}", roles.len())),
        );
        map_mut(&mut extra).insert(
            "index".to_string(),
            Json::Number(100.0 + roles.len() as f64),
        );
        roles.push(extra);
    }
    assert_eq!(parse_ir(&too_many).expect_err("too many").code(), E_FILE_IR);
    // stereo_strict 但 mask 不是 [FL, FR]。
    let mut bad = valid.clone();
    map_mut(nested_mut(&mut bad, &["profile"]))
        .insert("stereo_strict".to_string(), Json::Bool(true));
    map_mut(nested_mut(&mut bad, &["profile"])).insert(
        "stereo_mask".to_string(),
        Json::Array(vec![Json::String("FL".to_string())]),
    );
    assert_eq!(parse_ir(&bad).expect_err("strict mask").code(), E_FILE_IR);
    // stereo_mask 含未知 role。
    let mut bad = valid;
    map_mut(nested_mut(&mut bad, &["profile"])).insert(
        "stereo_mask".to_string(),
        Json::Array(vec![
            Json::String("FL".to_string()),
            Json::String("XX".to_string()),
        ]),
    );
    assert_eq!(parse_ir(&bad).expect_err("mask role").code(), E_FILE_IR);
}

#[test]
fn ir_rejects_post_rules() {
    let valid = ir_role_metadata(&[0; 6], &[false; 6]);
    // role_metadata 的 p_taps 必须 = 1。
    let mut bad = valid.clone();
    let post = match nested_mut(&mut bad, &["post"]) {
        Json::Array(entries) => entries,
        other => panic!("post not array: {:?}", other),
    };
    map_mut(&mut post[0]).insert("p_taps".to_string(), Json::Number(2.0));
    assert_eq!(parse_ir(&bad).expect_err("p_taps rm").code(), E_FILE_TAPS);
    // coeff_embedded 的 p_taps 必须 = delay+1。
    let embedded = ir_draft(
        "coeff_embedded",
        48_000,
        "4.1.2",
        &[2, 0, 0, 0, 0, 0],
        &[false; 6],
        None,
    );
    let mut bad = embedded.clone();
    let post = match nested_mut(&mut bad, &["post"]) {
        Json::Array(entries) => entries,
        other => panic!("post not array: {:?}", other),
    };
    map_mut(&mut post[0]).insert("p_taps".to_string(), Json::Number(2.0));
    assert_eq!(parse_ir(&bad).expect_err("p_taps emb").code(), E_FILE_TAPS);
    // 分数延迟（delay_q16 != 0）拒绝：file runner 只支持整数 sample。
    let mut bad = valid.clone();
    let post = match nested_mut(&mut bad, &["post"]) {
        Json::Array(entries) => entries,
        other => panic!("post not array: {:?}", other),
    };
    map_mut(&mut post[0]).insert("delay_q16".to_string(), Json::Number(1.0));
    assert_eq!(parse_ir(&bad).expect_err("q16").code(), E_FILE_DELAY);
    // post index 与 roles 不一致。
    let mut bad = valid.clone();
    let post = match nested_mut(&mut bad, &["post"]) {
        Json::Array(entries) => entries,
        other => panic!("post not array: {:?}", other),
    };
    map_mut(&mut post[0]).insert("index".to_string(), Json::Number(99.0));
    assert_eq!(parse_ir(&bad).expect_err("index").code(), E_FILE_IR);
    // post 名称不在 roles。
    let mut bad = valid.clone();
    let post = match nested_mut(&mut bad, &["post"]) {
        Json::Array(entries) => entries,
        other => panic!("post not array: {:?}", other),
    };
    map_mut(&mut post[0]).insert("name".to_string(), Json::String("XX".to_string()));
    assert_eq!(parse_ir(&bad).expect_err("name").code(), E_FILE_IR);
    // post 覆盖少于 roles。
    let mut bad = valid;
    let post = match nested_mut(&mut bad, &["post"]) {
        Json::Array(entries) => entries,
        other => panic!("post not array: {:?}", other),
    };
    post.pop();
    assert_eq!(parse_ir(&bad).expect_err("coverage").code(), E_FILE_IR);
}

#[test]
fn ir_stale_six_reasons_each_independent() {
    let draft = ir_role_metadata(&[0; 6], &[false; 6]);
    let parsed = parse_ir(&draft).expect("parse");
    let base = current();
    assert!(!parsed.is_stale(&base));
    assert!(parsed.stale_reasons(&base).is_empty());

    let mut changed = base.clone();
    changed.fs_hz = 96_000;
    assert_eq!(parsed.stale_reasons(&changed), vec![StaleReason::Fs]);

    let mut changed = base.clone();
    changed.layout_id = Some("5.1".to_string());
    assert_eq!(parsed.stale_reasons(&changed), vec![StaleReason::Layout]);

    let mut changed = base.clone();
    changed.role_indices[0] = ("FL".to_string(), 9);
    assert_eq!(parsed.stale_reasons(&changed), vec![StaleReason::Roles]);

    let mut changed = base.clone();
    changed.profile_sha256 = HASH_B.to_string();
    assert_eq!(parsed.stale_reasons(&changed), vec![StaleReason::Profile]);

    let mut changed = base.clone();
    changed.target_sha256 = Some(HASH_A.to_string());
    assert_eq!(parsed.stale_reasons(&changed), vec![StaleReason::Target]);

    let mut changed = base;
    changed.calibration_sha256 = HASH_A.to_string();
    assert_eq!(
        parsed.stale_reasons(&changed),
        vec![StaleReason::Calibration]
    );

    // 多原因按固定顺序 Fs/Layout/Roles/Profile/Target/Calibration。
    let mut all = IrCurrent {
        fs_hz: 96_000,
        layout_id: None,
        role_indices: vec![("OTHER".to_string(), 0)],
        profile_sha256: HASH_B.to_string(),
        target_sha256: Some(HASH_A.to_string()),
        calibration_sha256: HASH_A.to_string(),
    };
    assert_eq!(
        parsed.stale_reasons(&all),
        vec![
            StaleReason::Fs,
            StaleReason::Layout,
            StaleReason::Roles,
            StaleReason::Profile,
            StaleReason::Target,
            StaleReason::Calibration,
        ]
    );
    all.layout_id = Some("4.1.2".to_string());
    assert_eq!(
        parsed
            .stale_reasons(&all)
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>(),
        vec![
            "STALE_FS",
            "STALE_ROLES",
            "STALE_PROFILE",
            "STALE_TARGET",
            "STALE_CALIBRATION"
        ]
    );
}

#[test]
fn ir_apply_post_role_metadata_applies_delay_and_polarity_once() {
    let draft = ir_role_metadata(
        &[2, 1, 0, 0, 0, 0],
        &[false, true, false, false, false, false],
    );
    let parsed = parse_ir(&draft).expect("parse");
    let fl = parsed.apply_post_to_p("FL", &[1.0]).expect("FL");
    assert_eq!(fl, vec![0.0f32, 0.0, 1.0]);
    let fr = parsed.apply_post_to_p("FR", &[0.5]).expect("FR");
    assert_eq!(fr, vec![0.0f32, -0.5]);
    // 其余角色原样（D=0、正相）。
    assert_eq!(
        parsed.apply_post_to_p("C", &[1.0]).expect("C"),
        vec![1.0f32]
    );
    // 未知 role。
    assert_eq!(
        parsed
            .apply_post_to_p("XX", &[1.0])
            .expect_err("role")
            .code(),
        E_FILE_IR
    );
    // role_metadata 要求 base 长度 1。
    assert_eq!(
        parsed
            .apply_post_to_p("FL", &[1.0, 2.0])
            .expect_err("len")
            .code(),
        E_FILE_TAPS
    );
}

#[test]
fn ir_apply_post_coeff_embedded_does_not_double_apply() {
    let draft = ir_draft(
        "coeff_embedded",
        48_000,
        "4.1.2",
        &[2, 0, 0, 0, 0, 0],
        &[false; 6],
        None,
    );
    let parsed = parse_ir(&draft).expect("parse");
    let base = vec![0.0f32, 0.0, 1.0];
    let applied = parsed.apply_post_to_p("FL", &base).expect("FL");
    assert_eq!(applied, base, "coeff_embedded 不得再加一次 D/polarity");
    // 长度必须等于 p_taps（3）。
    assert_eq!(
        parsed
            .apply_post_to_p("FL", &[1.0])
            .expect_err("len")
            .code(),
        E_FILE_TAPS
    );
}

#[test]
fn ir_binding_json_records_stale_and_encoding() {
    let draft = ir_role_metadata(&[0; 6], &[false; 6]);
    let parsed = parse_ir(&draft).expect("parse");
    let json = binding_json(&parsed, &[StaleReason::Fs, StaleReason::Roles]);
    let text = json.to_canonical();
    assert!(text.contains("\"encoding\":\"role_metadata\""));
    assert!(text.contains("\"stale_reasons\":[\"STALE_FS\",\"STALE_ROLES\"]"));
    assert!(text.contains("\"ir_hash\""));
    let empty = binding_json(&parsed, &[]);
    assert!(empty.to_canonical().contains("\"stale_reasons\":[]"));
}

#[test]
fn ir_curve_and_g_residual_contracts() {
    let valid = ir_role_metadata(&[0; 6], &[false; 6]);
    // curve 目标曲线绑定。
    let mut with_curve = valid.clone();
    map_mut(&mut with_curve).insert(
        "curve".to_string(),
        Json::from_entries(vec![("target_sha256", Json::String(HASH_A.to_string()))]),
    );
    let parsed = parse_ir(&with_curve).expect("curve");
    assert_eq!(parsed.curve_target_sha256.as_deref(), Some(HASH_A));
    // 曲线 hash 格式错误。
    let mut bad = valid.clone();
    map_mut(&mut bad).insert(
        "curve".to_string(),
        Json::from_entries(vec![("target_sha256", Json::String("zz".to_string()))]),
    );
    assert_eq!(parse_ir(&bad).expect_err("curve hash").code(), E_FILE_IR);
    // G residual 规则只允许 modal_residual_only。
    let mut bad = valid.clone();
    map_mut(nested_mut(&mut bad, &["g_residual"])).insert(
        "rule".to_string(),
        Json::String("full_remeasure".to_string()),
    );
    assert_eq!(parse_ir(&bad).expect_err("g rule").code(), E_FILE_IR);
    // participants 必须在 roles 内。
    let mut bad = valid.clone();
    map_mut(nested_mut(&mut bad, &["g_residual"])).insert(
        "participants".to_string(),
        Json::Array(vec![Json::String("XX".to_string())]),
    );
    assert_eq!(parse_ir(&bad).expect_err("g participant").code(), E_FILE_IR);
    // extensions 计数。
    let mut bad = valid;
    map_mut(&mut bad).insert(
        "extensions".to_string(),
        Json::Array(vec![
            Json::String("a".to_string()),
            Json::String("b".to_string()),
        ]),
    );
    assert_eq!(parse_ir(&bad).expect("extensions").extensions_count, 2);
    // 角色 index 与 ROLE_SCATTER 一致（避免夹具与 session 语义漂移）。
    let parsed = parse_ir(&ir_role_metadata(&[0; 6], &[false; 6])).expect("parse");
    for (index, role) in parsed.roles.iter().enumerate() {
        assert_eq!(role.index, ROLE_SCATTER[index] as u32);
    }
}
