//! 图准备：T12 compile/T09 DAG、完整图版本（规范 SHA-256）、IR 应用、identity 保护。

use tspi_file_runner::graph::{self, GraphKind};
use tspi_file_runner::json::Json;
use tspi_file_runner::session::{self, SourceProvider};
use tspi_file_runner::{
    E_FILE_CHANNELS, E_FILE_GRAPH, E_FILE_HASH, E_FILE_IR, E_FILE_PATH_TABLE, E_FILE_RATE,
    E_FILE_TAPS, FALLBACK_IDENTITY_P_POST, FALLBACK_MUTE, TRIGGER_COMPILE_REJECT,
    TRIGGER_GRAPH_HASH_MISMATCH, TRIGGER_IR_ABSENT, TRIGGER_IR_STALE,
};

use crate::common::{
    fir_fs, ir_draft, ir_role_metadata, linear_delay_one, map_mut, min_fast_delay, nested_mut,
    protection, protection_none, set_field, Fixture, HASH_A,
};

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn graph_id_of(fixture: &Fixture) -> String {
    fixture.prepare().expect("prepare ok").graph_id
}

#[test]
fn graph_prepare_identity_normal_graph() {
    let fixture = Fixture::identity(100);
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::Normal);
    assert!(!prepared.is_protected());
    assert!(prepared.protection.is_none());
    assert!(prepared.compile.is_some());
    assert!(prepared.ir_binding.is_none());
    let graph = prepared.graph.as_ref().expect("graph");
    assert_eq!((graph.l_a, graph.l_g, graph.l_p), (1, 1, 1));
    assert_eq!(graph.l_total, 1);
    assert_eq!(graph.flush_zeros, 0, "flush = L_F - 1");
    assert_eq!(graph.sample_rate_hz, 48_000);
    assert_eq!(graph.block_size, 128);
    assert_eq!(graph.expected_workers, 4);
    assert_eq!(graph.plan_version, 1);
    assert_eq!(graph.container_channels(), 8);
    assert!(graph.state_samples() > 0);
    // 系数布局自洽。
    assert_eq!(graph.a.len(), 2 * 1);
    assert_eq!(graph.g.len(), 3 * 3 * 1);
    assert_eq!(graph.p.len(), 6 * 1);
    assert_eq!(prepared.graph_id.len(), 64);
    assert!(prepared
        .graph_id
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    // 8 个系数文件（2 A + 6 P）全部进入绑定。
    assert_eq!(prepared.fir_bindings.len(), 8);
    // compile 摘要与 fused 设计期摘要（fused 只作元数据，不执行）。
    let compile = prepared.compile.as_ref().expect("compile");
    assert_eq!(compile.full_path_len, 1);
    assert!(compile.route_count > 0);
    assert!(compile.fused_feasible);
    if compile.fused_built {
        assert!(prepared.fused_digest.is_some());
    }
    let _ = graph.coeffs();
}

#[test]
fn graph_hash_deterministic_and_version_fields_excluded() {
    let fixture = Fixture::identity(32);
    let hash = graph_id_of(&fixture);
    assert_eq!(hash, graph_id_of(&fixture), "同输入同图必须同 hash");
    // plan_version 不参与图版本。
    let mut bumped = Fixture::identity(32);
    bumped.plan_version = 9;
    assert_eq!(hash, graph_id_of(&bumped));
    // created_unix / x_ 扩展字段不参与图版本。
    let (text, source) = fixture.build();
    let mut json = Json::parse(&text).expect("json");
    map_mut(&mut json).insert("created_unix".to_string(), Json::Number(1_700_000_000.0));
    map_mut(&mut json).insert("x_vendor".to_string(), Json::String("v".to_string()));
    let session = session::parse_session_json(&json).expect("session");
    let prepared = graph::prepare(session, &source).expect("prepare");
    assert_eq!(prepared.graph_id, hash);
    assert_eq!(prepared.session.extensions, vec!["x_vendor".to_string()]);
    assert_eq!(prepared.session.created_unix, Some(1_700_000_000));
}

#[test]
fn graph_hash_sensitive_to_roles_and_mapping() {
    let base = graph_id_of(&Fixture::identity(32));
    let mut roles = Fixture::identity(32);
    roles.set_scatter("C", 5);
    assert_ne!(base, graph_id_of(&roles), "scatter 变化必须改变图版本");
    // 参与者（低带 dense 顺序）变化。
    let mut participants = Fixture::identity(32);
    participants.participants = vec!["FL".to_string(), "FR".to_string()];
    assert_ne!(base, graph_id_of(&participants));
}

#[test]
fn graph_hash_sensitive_to_coefficients() {
    let base = graph_id_of(&Fixture::identity(32));
    // P 系数变化。
    let mut p_changed = Fixture::identity(32);
    p_changed.set_p("linear", &[0.0, 1.0, 0.0]);
    let hash = graph_id_of(&p_changed);
    assert_ne!(base, hash);
    // A 系数变化（taps=2 的 min_fast）。
    let mut a_changed = Fixture::identity(32);
    let (manifest, bytes) = crate::common::fir("min_fast", &[1.0, 0.25], "a0.raw");
    a_changed.a[0] = (0, manifest, bytes);
    assert_ne!(base, graph_id_of(&a_changed));
    // G residual 变化。
    let mut g_changed = Fixture::identity(32);
    g_changed.set_g(0, 1, "min_fast", &[1.0, 0.5]);
    assert_ne!(base, graph_id_of(&g_changed));
}

#[test]
fn graph_hash_sensitive_to_engine_and_protection() {
    let base = graph_id_of(&Fixture::identity(32));
    // d_samples 改变 G 支撑。
    let mut delayed = Fixture::identity(32);
    delayed.d_samples = 1;
    assert_ne!(base, graph_id_of(&delayed));
    // block_size。
    let mut block = Fixture::identity(32);
    block.block_size = 64;
    assert_ne!(base, graph_id_of(&block));
    // workers。
    let mut workers = Fixture::identity(32);
    workers.workers = 2;
    assert_ne!(base, graph_id_of(&workers));
    // 保护策略（fallback）也进入图版本。
    let mut protected = Fixture::identity(32);
    protected.protection = Some(protection("mute", &[TRIGGER_GRAPH_HASH_MISMATCH], true));
    assert_ne!(base, graph_id_of(&protected));
    // profile / max_taps。
    let mut profile = Fixture::identity(32);
    profile.set_p_custom("min_fast", &[1.0, 0.0], "extended", 16_384);
    profile.profile = "extended".to_string();
    profile.max_taps = 16_384;
    profile.extended_admitted = true;
    assert_ne!(base, graph_id_of(&profile));
}

#[test]
fn graph_hash_sensitive_to_fs_but_not_to_input_samples() {
    let base = graph_id_of(&Fixture::identity(32));
    let mut rate = Fixture::identity(32);
    rate.set_fs(96_000);
    assert_ne!(base, graph_id_of(&rate), "fs 变化必须改变图版本");
    // 输入样本变化不改图版本（输入 SHA 在报告里单独记录）。
    let mut samples = Fixture::identity(32);
    samples.input_planar[0] = 0.75;
    assert_eq!(base, graph_id_of(&samples));
}

#[test]
fn graph_declared_graph_id_must_match() {
    let mut fixture = Fixture::identity(32);
    fixture.graph_id = Some(ZERO_HASH.to_string());
    // 默认保护会切换到 identity fallback，而不是静默接受错误版本。
    let prepared = fixture.prepare().expect("protected prepare");
    assert!(prepared.is_protected());
    assert_eq!(
        prepared.protection.as_ref().expect("protection").reason,
        TRIGGER_GRAPH_HASH_MISMATCH
    );
    // 关闭该 trigger -> 直接报 E_FILE_GRAPH。
    fixture.protection = Some(protection("identity_p_post", &[], true));
    let error = fixture.prepare().expect_err("mismatch rejected");
    assert_eq!(error.code(), E_FILE_GRAPH);
    // 声明正确的 graph_id -> 正常图。
    let mut ok = Fixture::identity(32);
    ok.graph_id = Some(graph_id_of(&ok));
    let prepared = ok.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::Normal);
}

#[test]
fn graph_protection_graph_hash_mismatch_identity_p_post() {
    let mut fixture = Fixture::identity(64);
    fixture.graph_id = Some(ZERO_HASH.to_string());
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    assert!(prepared.is_protected());
    assert!(prepared.compile.is_none());
    assert!(prepared.fused_digest.is_none());
    let protection = prepared.protection.as_ref().expect("protection");
    assert_eq!(protection.reason, TRIGGER_GRAPH_HASH_MISMATCH);
    assert_eq!(protection.fallback, FALLBACK_IDENTITY_P_POST);
    assert_eq!(protection.graph_kind, "identity_p_post");
    let graph = prepared.graph.as_ref().expect("identity graph");
    // G=I（全零 residual）、A=[1]、P 保留 base（nos）-> 直通，L_F=1。
    assert_eq!((graph.l_a, graph.l_g, graph.l_p), (1, 1, 1));
    assert!(graph.g.iter().all(|value| *value == 0.0));
    assert!(graph.a.iter().all(|value| *value == 1.0));
    assert_eq!(graph.flush_zeros, 0);
}

#[test]
fn graph_protection_mute_has_no_graph() {
    let mut fixture = Fixture::identity(64);
    fixture.graph_id = Some(ZERO_HASH.to_string());
    fixture.protection = Some(protection(
        FALLBACK_MUTE,
        &[TRIGGER_GRAPH_HASH_MISMATCH],
        true,
    ));
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionMute);
    assert!(prepared.is_protected());
    assert!(prepared.graph.is_none());
    assert!(prepared.compile.is_none());
    let protection = prepared.protection.as_ref().expect("protection");
    assert_eq!(protection.fallback, FALLBACK_MUTE);
    assert_eq!(protection.graph_kind, "mute");
}

#[test]
fn graph_protection_ir_stale_uses_base_p_without_ir() {
    let mut fixture = Fixture::identity(64);
    // IR 的源采样率与当前 fs 不一致 -> STALE_FS。
    fixture.ir = Some(ir_draft(
        "role_metadata",
        96_000,
        "4.1.2",
        &[0; 6],
        &[false; 6],
        None,
    ));
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    assert_eq!(
        prepared.protection.as_ref().expect("protection").reason,
        TRIGGER_IR_STALE
    );
    assert_eq!(prepared.ir_stale_codes(), vec!["STALE_FS"]);
    let binding = prepared.ir_binding.as_ref().expect("ir binding");
    assert!(!binding.applied, "stale IR 不得应用");
    assert_eq!(binding.stale.len(), 1);
    // fallback 图用 base P（无 IR delay/polarity）。
    let graph = prepared.graph.as_ref().expect("graph");
    assert_eq!(graph.l_p, 1);
    // 关闭 ir_stale trigger -> 拒绝而不是静默。
    fixture.protection = Some(protection(
        "identity_p_post",
        &[TRIGGER_GRAPH_HASH_MISMATCH],
        true,
    ));
    let error = fixture.prepare().expect_err("stale without protection");
    assert_eq!(error.code(), tspi_file_runner::E_FILE_IR_STALE);
}

#[test]
fn graph_protection_ir_absent_optional() {
    let mut fixture = Fixture::identity(64);
    fixture.protection = Some(protection(
        FALLBACK_IDENTITY_P_POST,
        &[TRIGGER_IR_ABSENT],
        true,
    ));
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    assert_eq!(
        prepared.protection.as_ref().expect("protection").reason,
        TRIGGER_IR_ABSENT
    );
    assert!(prepared.ir_binding.is_none());
    // 默认 trigger 集合不含 ir_absent -> 正常图（IR 可选）。
    let normal = Fixture::identity(64).prepare().expect("normal");
    assert_eq!(normal.graph_kind, GraphKind::Normal);
}

#[test]
fn graph_protection_compile_reject_falls_back() {
    let mut fixture = Fixture::identity(64);
    // d_samples 让 L_G 超出 max_taps -> core::compile 拒绝。
    fixture.d_samples = 5_000;
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    let engaged = prepared.protection.as_ref().expect("protection");
    assert_eq!(engaged.reason, TRIGGER_COMPILE_REJECT);
    assert!(prepared.compile.is_none());
    // fallback 图不含超长 G：identity G=I / L_G=1。
    assert_eq!(prepared.graph.as_ref().expect("graph").l_g, 1);
    // 关闭 compile_reject trigger -> E_FILE_TAPS。
    fixture.protection = Some(protection("identity_p_post", &[], true));
    let error = fixture.prepare().expect_err("reject");
    assert_eq!(error.code(), E_FILE_TAPS);
}

#[test]
fn graph_ir_role_metadata_applied_once() {
    let mut fixture = Fixture::identity(32);
    fixture.ir = Some(ir_role_metadata(
        &[2, 1, 0, 0, 0, 0],
        &[false, true, false, false, false, false],
    ));
    let prepared = fixture.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::Normal);
    let binding = prepared.ir_binding.as_ref().expect("binding");
    assert!(binding.applied);
    assert!(binding.stale.is_empty());
    let graph = prepared.graph.as_ref().expect("graph");
    assert_eq!(graph.l_p, 3, "role_metadata D+1 扩展 P");
    assert_eq!(graph.l_total, 3);
    assert_eq!(graph.flush_zeros, 2);
    // P 行：FL 延迟 2、FR 延迟 1 且反相，其余直通。
    assert_eq!(&graph.p[0..3], &[0.0f32, 0.0, 1.0]);
    assert_eq!(&graph.p[3..6], &[0.0f32, -1.0, 0.0]);
    assert_eq!(&graph.p[6..9], &[1.0f32, 0.0, 0.0]);
}

#[test]
fn graph_ir_coeff_embedded_never_double_applies() {
    let mut fixture = Fixture::identity(32);
    // P 系数已含 2 sample 延迟（coeff_embedded）。
    fixture.set_p("min_fast", &[0.0, 0.0, 1.0]);
    fixture.ir = Some(ir_draft(
        "coeff_embedded",
        48_000,
        "4.1.2",
        &[2, 2, 2, 2, 2, 2],
        &[false; 6],
        None,
    ));
    let prepared = fixture.prepare().expect("prepare");
    let graph = prepared.graph.as_ref().expect("graph");
    assert_eq!(graph.l_p, 3);
    assert_eq!(
        &graph.p[0..3],
        &[0.0f32, 0.0, 1.0],
        "coeff_embedded 不得再加 D"
    );
    let run = tspi_file_runner::runner::run(&prepared, &Default::default()).expect("run");
    // 单次 2 sample 延迟：y[n] = x[n-2]。
    let input = &fixture.input_planar[0..32];
    let output = &run.output[0];
    assert_eq!(output.len(), 32 + 2);
    assert_eq!(output[0], 0.0);
    assert_eq!(output[1], 0.0);
    assert_eq!(&output[2..34], input);
}

#[test]
fn graph_ir_mapping_errors() {
    // IR role 名与 session 不一致。
    let mut fixture = Fixture::identity(32);
    let mut ir = ir_role_metadata(&[0; 6], &[false; 6]);
    let roles = match nested_mut(&mut ir, &["roles"]) {
        Json::Array(entries) => entries,
        other => panic!("roles not array: {:?}", other),
    };
    map_mut(&mut roles[0]).insert("name".to_string(), Json::String("XX".to_string()));
    fixture.ir = Some(ir);
    assert_eq!(fixture.prepare().expect_err("ir role").code(), E_FILE_IR);
    // role_metadata 要求 P base 长度 1；给 taps=2 的 P -> E_FILE_TAPS。
    let mut fixture = Fixture::identity(32);
    fixture.set_p("min_fast", &[1.0, 0.0]);
    fixture.ir = Some(ir_role_metadata(&[0; 6], &[false; 6]));
    assert_eq!(
        fixture.prepare().expect_err("p base len").code(),
        E_FILE_TAPS
    );
    // coeff_embedded 要求 P 长度 = p_taps（3）；给 taps=2 -> E_FILE_TAPS。
    let mut fixture = Fixture::identity(32);
    fixture.set_p("min_fast", &[1.0, 0.0]);
    fixture.ir = Some(ir_draft(
        "coeff_embedded",
        48_000,
        "4.1.2",
        &[2; 6],
        &[false; 6],
        None,
    ));
    assert_eq!(
        fixture.prepare().expect_err("embedded len").code(),
        E_FILE_TAPS
    );
}

#[test]
fn graph_slot_and_input_consistency_errors() {
    // slot file 与 manifest coefficients.file 不一致。
    let fixture = Fixture::identity(16);
    let (text, mut source) = fixture.build();
    let a0_bytes = source.load("a0.raw").expect("a0");
    source.insert("a0_copy.raw", a0_bytes);
    let mut json = Json::parse(&text).expect("json");
    let a = match nested_mut(&mut json, &["coefficients", "a"]) {
        Json::Array(entries) => entries,
        other => panic!("a not array: {:?}", other),
    };
    map_mut(&mut a[0]).insert("file".to_string(), Json::String("a0_copy.raw".to_string()));
    let session = session::parse_session_json(&json).expect("session");
    assert_eq!(
        graph::prepare(session, &source)
            .expect_err("path table")
            .code(),
        E_FILE_PATH_TABLE
    );
    // 系数 manifest fs 与 session fs 不一致。
    let mut fixture = Fixture::identity(16);
    let (manifest, bytes) = fir_fs("nos", &[1.0], "p_FL.raw", 96_000);
    fixture.p[0] = ("FL".to_string(), manifest, bytes);
    assert_eq!(fixture.prepare().expect_err("fs").code(), E_FILE_RATE);
    // 输入 SHA 不符。
    let fixture = Fixture::identity(16);
    let (text, source) = fixture.build();
    let _ = source.load("input.wav").expect("input");
    let mut json = Json::parse(&text).expect("json");
    map_mut(nested_mut(&mut json, &["audio", "input"]))
        .insert("sha256".to_string(), Json::String(ZERO_HASH.to_string()));
    let session = session::parse_session_json(&json).expect("session");
    assert_eq!(
        graph::prepare(session, &source)
            .expect_err("input sha")
            .code(),
        E_FILE_HASH
    );
    // 输入通道数与 session 不一致（session 声明 1ch，WAV 实际 2ch）。
    let fixture = Fixture::identity(16);
    let (text, source) = fixture.build();
    let mut json = Json::parse(&text).expect("json");
    map_mut(nested_mut(&mut json, &["audio"]))
        .insert("input_channels".to_string(), Json::Number(1.0));
    let roles = match nested_mut(&mut json, &["roles"]) {
        Json::Array(entries) => entries,
        other => panic!("roles not array: {:?}", other),
    };
    map_mut(&mut roles[1]).insert("src".to_string(), Json::Null);
    let a = match nested_mut(&mut json, &["coefficients", "a"]) {
        Json::Array(entries) => entries,
        other => panic!("a not array: {:?}", other),
    };
    a.truncate(1);
    let session = session::parse_session_json(&json).expect("session");
    assert_eq!(
        graph::prepare(session, &source)
            .expect_err("channels")
            .code(),
        E_FILE_CHANNELS
    );
    // 输入采样率与 session 不一致：session 改 96k，WAV 仍 48k。
    let fixture = Fixture::identity(16);
    let (text, source) = fixture.build();
    let mut json = Json::parse(&text).expect("json");
    map_mut(nested_mut(&mut json, &["audio"]))
        .insert("sample_rate_hz".to_string(), Json::Number(96_000.0));
    let session = session::parse_session_json(&json).expect("session");
    assert_eq!(
        graph::prepare(session, &source).expect_err("rate").code(),
        E_FILE_RATE
    );
}

#[test]
fn graph_fir_manifests_are_revalidated_at_load() {
    // 会话内 inline manifest 被篡改（sha 不符）-> E_FILE_HASH。
    let mut fixture = Fixture::identity(16);
    let (mut manifest, bytes) = crate::common::nos("a0.raw");
    map_mut(nested_mut(&mut manifest, &["coefficients"]))
        .insert("sha256".to_string(), Json::String(ZERO_HASH.to_string()));
    fixture.a[0] = (0, manifest, bytes);
    assert_eq!(fixture.prepare().expect_err("sha").code(), E_FILE_HASH);
    // manifest files 通过 manifest_file 引用读取（blob 源路径）。
    let fixture = Fixture::identity(16);
    let (text, mut source) = fixture.build();
    source.insert(
        "a0.manifest.json",
        crate::common::nos("a0.raw").0.to_canonical().into_bytes(),
    );
    let mut json = Json::parse(&text).expect("json");
    let a = match nested_mut(&mut json, &["coefficients", "a"]) {
        Json::Array(entries) => entries,
        other => panic!("a not array: {:?}", other),
    };
    map_mut(&mut a[0]).remove("manifest");
    map_mut(&mut a[0]).insert(
        "manifest_file".to_string(),
        Json::String("a0.manifest.json".to_string()),
    );
    let session = session::parse_session_json(&json).expect("session");
    let prepared = graph::prepare(session, &source).expect("prepare via manifest_file");
    assert_eq!(prepared.graph_kind, GraphKind::Normal);
}

#[test]
fn graph_set_field_helper_is_used_by_fixtures() {
    // 保护策略 JSON 通过 set_field 复用（防止夹具助手漂移）。
    let mut policy = protection("mute", &[TRIGGER_COMPILE_REJECT], true);
    set_field(&mut policy, "mute_on_exec_fault", Json::Bool(false));
    let mut fixture = Fixture::identity(16);
    fixture.protection = Some(policy);
    let session = session::parse_session(&fixture.session_text()).expect("session");
    assert!(!session.protection.mute_on_exec_fault);
    // 未使用 helper 的常量仍被引用（避免 dead_code 噪声）。
    let _ = (
        protection_none,
        min_fast_delay,
        linear_delay_one,
        HASH_A,
        FALLBACK_MUTE,
    );
}
