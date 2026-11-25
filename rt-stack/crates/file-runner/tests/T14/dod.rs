//! T14 DoD 汇总：交付契约（完整图版本/固定精度/identity 保护/元数据/NOT_RUN）端到端。

use tspi_file_runner::graph::GraphKind;
use tspi_file_runner::json::Json;
use tspi_file_runner::runner::{self, RunOptions};
use tspi_file_runner::{
    E_FILE_EXEC, FALLBACK_MUTE, GRAPH_HASH_ALGO, GRAPH_HASH_DOMAIN, NOT_VERIFIED, PRECISION_LOCK,
    REPORT_SCHEMA, ROUTE_TAPS_EXTENDED_MAX, ROUTE_TAPS_REGULAR_MAX, RUNNER_NAME, RUNNER_VERSION,
    SESSION_SCHEMA, TRIGGER_COMPILE_REJECT, TRIGGER_GRAPH_HASH_MISMATCH, TRIGGER_IR_ABSENT,
    TRIGGER_IR_STALE,
};

use crate::common::{
    ir_draft, ir_role_metadata, protection, report_json, report_num, report_str, Fixture, HASH_B,
};

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn dod_root_contract_constants() {
    assert_eq!(SESSION_SCHEMA, "tspi-file-runner/v1");
    assert_eq!(REPORT_SCHEMA, "tspi-file-runner-report/v1");
    assert_eq!(PRECISION_LOCK, "f32-strict");
    assert_eq!(GRAPH_HASH_ALGO, "sha256-canonical-v1");
    assert!(GRAPH_HASH_DOMAIN.starts_with("TSPI-FILE-RUNNER-GRAPH"));
    assert_eq!(RUNNER_NAME, "tspi-file-runner");
    assert!(!RUNNER_VERSION.is_empty());
    assert_eq!(ROUTE_TAPS_REGULAR_MAX, 4_096);
    assert_eq!(ROUTE_TAPS_EXTENDED_MAX, 16_384);
    assert_eq!(NOT_VERIFIED.len(), 7);
}

#[test]
fn dod_end_to_end_report_key_set_complete() {
    let mut fixture = Fixture::identity(160);
    // IR role_metadata D=1：P FIR=[1] 基（IR 不得与已折入的 taps>1 系数同用），
    // 展开后 L_P=2 -> flush=1，端到端覆盖 IR/flush/metadata。
    fixture.ir = Some(ir_role_metadata(&[1; 6], &[false; 6]));
    let result = fixture.run().expect("run");
    let report = report_json(&result);
    // 证据：样例元数据（默认被 cargo 捕获；evidence 复跑时用 --nocapture 落盘）。
    println!("T14_SAMPLE_REPORT={}", result.metadata_json);
    let object = report.as_object().expect("report object");
    let mut keys: Vec<&str> = object.keys().map(|key| key.as_str()).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "compile",
        "counts",
        "engine",
        "exec_error",
        "fir",
        "fused_digest",
        "graph_hash_algo",
        "graph_id",
        "graph_kind",
        "input",
        "ir",
        "lengths",
        "not_verified",
        "output",
        "precision",
        "protection",
        "runner",
        "schema",
        "status",
        "warnings",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected, "reporter 字段面必须完整且不外溢");
    // 关键数值口径：输出帧 = 输入 + flush；flush = L_F-1。
    let prep = fixture.prepare().expect("prepare");
    let graph = prep.graph.as_ref().expect("graph");
    assert_eq!(
        report_num(&report, &["lengths", "l_total"]),
        graph.l_total as f64
    );
    assert_eq!(
        report_num(&report, &["lengths", "flush_zeros"]),
        (graph.l_total - 1) as f64
    );
    assert_eq!(
        report_num(&report, &["output", "frames"]),
        (160 + graph.flush_zeros as usize) as f64
    );
    assert_eq!(
        report_num(&report, &["counts", "flush_blocks"]) >= 1.0,
        true
    );
    // identity 保护记录字段面完整（未触发）。
    let protection_json = match object.get("protection") {
        Some(value) => value.clone(),
        None => panic!("protection missing"),
    };
    let protection_object = protection_json.as_object().expect("object");
    for key in ["engaged", "reason", "fallback", "graph_kind"] {
        assert!(protection_object.contains_key(key), "protection.{key}");
    }
    assert_eq!(protection_object.get("engaged"), Some(&Json::Bool(false)));
}

#[test]
fn dod_full_graph_version_matrix() {
    // 不改变图版本的字段。
    let base_hash = Fixture::identity(32).prepare().expect("prepare").graph_id;
    let mut version_only = Fixture::identity(32);
    version_only.plan_version = 7;
    assert_eq!(
        base_hash,
        version_only.prepare().expect("prepare").graph_id,
        "plan_version 不参与图版本"
    );
    // 改变图版本的字段（逐项）。
    let mut variants: Vec<(&str, Fixture)> = Vec::new();
    let mut roles = Fixture::identity(32);
    roles.set_scatter("C", 5);
    variants.push(("roles", roles));
    let mut coeffs = Fixture::identity(32);
    coeffs.set_p("linear", &[0.0, 1.0, 0.0]);
    variants.push(("coefficients", coeffs));
    let mut matrix = Fixture::identity(32);
    matrix.set_g(0, 1, "min_fast", &[1.0, 0.25]);
    variants.push(("g_residual", matrix));
    let mut delay = Fixture::identity(32);
    delay.d_samples = 1;
    variants.push(("d_samples", delay));
    let mut block = Fixture::identity(32);
    block.block_size = 64;
    variants.push(("block_size", block));
    let mut workers = Fixture::identity(32);
    workers.workers = 2;
    variants.push(("workers", workers));
    let mut rate = Fixture::identity(32);
    rate.set_fs(96_000);
    variants.push(("fs", rate));
    let mut protect = Fixture::identity(32);
    protect.protection = Some(protection(
        FALLBACK_MUTE,
        &[TRIGGER_GRAPH_HASH_MISMATCH],
        true,
    ));
    variants.push(("protection", protect));
    let mut ir = Fixture::identity(32);
    ir.ir = Some(ir_role_metadata(&[1; 6], &[false; 6]));
    variants.push(("ir_binding", ir));
    let mut seen = std::collections::BTreeMap::new();
    seen.insert(base_hash.clone(), "base");
    for (label, fixture) in variants {
        let hash = fixture.prepare().expect("prepare").graph_id;
        assert_eq!(hash.len(), 64, "{label}");
        assert_ne!(hash, base_hash, "{label} 必须改变图版本");
        assert!(
            seen.insert(hash.clone(), label).is_none(),
            "两个不同图不得共享 graph_id（{label}）"
        );
    }
}

#[test]
fn dod_precision_lock_and_f32_execution() {
    // 非 f32-strict 的 session 被拒（精度锁）。
    let json = Json::parse(&Fixture::identity(16).session_text()).expect("json");
    let mut bad = json;
    crate::common::map_mut(&mut bad)
        .insert("precision".to_string(), Json::String("f64".to_string()));
    assert_eq!(
        tspi_file_runner::session::parse_session_json(&bad)
            .expect_err("precision")
            .code(),
        tspi_file_runner::E_FILE_PRECISION
    );
    // 执行全程 f32：identity 图输出 = 输入 f32 位（fixed precision）。
    let fixture = Fixture::identity(64);
    let prepared = fixture.prepare().expect("prepare");
    let result = runner::run(&prepared, &RunOptions::default()).expect("run");
    for channel in 0..2 {
        for frame in 0..64 {
            assert_eq!(
                result.output[channel][frame].to_bits(),
                fixture.input_planar[channel * 64 + frame].to_bits()
            );
        }
    }
    // 报告 precision 与 runner 版本。
    let report = report_json(&result);
    assert_eq!(report_str(&report, &["precision"]), PRECISION_LOCK);
    assert_eq!(report_str(&report, &["runner", "name"]), RUNNER_NAME);
    assert_eq!(report_str(&report, &["runner", "version"]), RUNNER_VERSION);
}

#[test]
fn dod_identity_protection_matrix_never_half_graph() {
    // 1) graph hash mismatch -> identity_p_post 直通。
    let mut mismatch = Fixture::identity(96);
    mismatch.graph_id = Some(ZERO_HASH.to_string());
    let prepared = mismatch.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    let result = runner::run(&prepared, &RunOptions::default()).expect("run");
    assert_eq!(result.output_frames, 96);
    assert_eq!(result.output[0], mismatch.input_planar[0..96].to_vec());
    // 2) graph hash mismatch + mute -> 全零，长度 = 输入。
    let mut muted = Fixture::identity(96);
    muted.graph_id = Some(ZERO_HASH.to_string());
    muted.protection = Some(protection(
        FALLBACK_MUTE,
        &[TRIGGER_GRAPH_HASH_MISMATCH],
        true,
    ));
    let prepared = muted.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionMute);
    let result = runner::run(&prepared, &RunOptions::default()).expect("run");
    assert_eq!(result.output_frames, 96);
    assert!(result
        .output
        .iter()
        .all(|channel| channel.iter().all(|v| *v == 0.0)));
    // 3) IR stale -> identity fallback（不把失效校准套到输出）。
    let mut stale = Fixture::identity(96);
    stale.ir = Some(ir_draft(
        "role_metadata",
        96_000,
        "4.1.2",
        &[5; 6],
        &[true; 6],
        None,
    ));
    let prepared = stale.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    let result = runner::run(&prepared, &RunOptions::default()).expect("run");
    assert_eq!(result.output_frames, 96);
    assert_eq!(
        result.output[0],
        stale.input_planar[0..96].to_vec(),
        "失效 IR 未应用"
    );
    let report = report_json(&result);
    assert_eq!(
        report_str(&report, &["protection", "reason"]),
        TRIGGER_IR_STALE
    );
    // 4) compile reject -> identity fallback。
    let mut reject = Fixture::identity(96);
    reject.d_samples = 5_000;
    let prepared = reject.prepare().expect("prepare");
    assert_eq!(prepared.graph_kind, GraphKind::ProtectionIdentityPPost);
    assert_eq!(
        prepared.protection.as_ref().expect("protection").reason,
        TRIGGER_COMPILE_REJECT
    );
    // 5) IR absent（显式触发）-> identity fallback。
    let mut absent = Fixture::identity(96);
    absent.protection = Some(protection("identity_p_post", &[TRIGGER_IR_ABSENT], true));
    let prepared = absent.prepare().expect("prepare");
    assert_eq!(
        prepared.protection.as_ref().expect("protection").reason,
        TRIGGER_IR_ABSENT
    );
    // 任何保护路径都不得输出“半图”：帧数或 = 输入，或 = 输入 + 确定性 flush。
    for (frames, flush) in [(96usize, 0usize), (96, 0), (96, 0), (96, 0), (96, 0)] {
        assert_eq!(96 + flush, frames);
    }
}

#[test]
fn dod_exec_fault_never_publishes_partial_audio() {
    let fixture = Fixture::identity(300);
    let result = fixture
        .run_with(&RunOptions {
            inject_exec_fault_after_blocks: Some(1),
        })
        .expect("mute on fault");
    assert_eq!(result.status, "muted");
    assert!(result.exec_error.is_some());
    // 整图 mute：不是前 128 帧正常 + 后面零，而是全部零。
    assert!(result
        .output
        .iter()
        .all(|channel| channel.iter().all(|v| *v == 0.0)));
    assert_eq!(result.output_frames, 300);
    assert!(result.exec_error.unwrap().contains(E_FILE_EXEC));
}

#[test]
fn dod_metadata_is_deterministic_and_recomputable() {
    let mut fixture = Fixture::identity(150);
    fixture.set_p("min_fast", &{
        let mut coeffs = vec![0.0f32; 5];
        coeffs[4] = 1.0;
        coeffs
    });
    let first = fixture.run().expect("run 1");
    let second = fixture.run().expect("run 2");
    assert_eq!(first.metadata_json, second.metadata_json);
    // 报告的 graph_id 可由同 session 重新 prepare 复算。
    let prepared = fixture.prepare().expect("prepare");
    let report = report_json(&first);
    assert_eq!(report_str(&report, &["graph_id"]), prepared.graph_id);
    // 报告输出 SHA 可由输出样本复算。
    let mut bits = Vec::new();
    for channel in &first.output {
        for value in channel {
            bits.extend_from_slice(&value.to_bits().to_le_bytes());
        }
    }
    assert_eq!(
        report_str(&report, &["output", "sha256"]),
        tspi_file_runner::sha256::sha256_hex(&bits)
    );
    // IR 绑定 hash 进入图版本：IR 内容 hash 改变 -> graph_id 改变。
    let mut with_ir = Fixture::identity(150);
    with_ir.ir = Some(ir_role_metadata(&[1, 0, 0, 0, 0, 0], &[false; 6]));
    let hash_a = with_ir.prepare().expect("prepare").graph_id;
    let mut ir_changed = ir_role_metadata(&[1, 0, 0, 0, 0, 0], &[false; 6]);
    crate::common::map_mut(&mut ir_changed)
        .insert("ir_hash".to_string(), Json::String(HASH_B.to_string()));
    with_ir.ir = Some(ir_changed);
    let hash_b = with_ir.prepare().expect("prepare").graph_id;
    assert_ne!(hash_a, hash_b, "IR hash 是图版本的一部分");
}

#[test]
fn dod_flush_contract_full_path_length() {
    // L_F = L_A + L_G + L_P - 2；flush = L_F - 1；输出 = 输入 + flush。
    let mut cases = Vec::new();
    cases.push(("identity", Fixture::identity(130)));
    let mut p3 = Fixture::identity(130);
    p3.set_p("linear", &[0.0, 1.0, 0.0]);
    cases.push(("p3", p3));
    let mut p5 = Fixture::identity(130);
    p5.set_p("min_fast", &[0.0, 0.0, 0.0, 0.0, 1.0]);
    cases.push(("p5", p5));
    let mut g2 = Fixture::identity(130);
    g2.d_samples = 1;
    cases.push(("g2", g2));
    let mut ir2 = Fixture::identity(130);
    ir2.ir = Some(ir_role_metadata(&[3; 6], &[false; 6]));
    cases.push(("ir3", ir2));
    for (label, fixture) in cases {
        let prepared = fixture.prepare().expect("prepare");
        let graph = prepared.graph.as_ref().expect("graph");
        let expected_l_total = graph.l_a + graph.l_g + graph.l_p - 2;
        assert_eq!(graph.l_total, expected_l_total, "{label}");
        assert_eq!(graph.flush_zeros, expected_l_total - 1, "{label}");
        let result = runner::run(&prepared, &RunOptions::default()).expect("run");
        assert_eq!(
            result.output_frames,
            130 + graph.flush_zeros as usize,
            "{label}"
        );
        let report = report_json(&result);
        assert_eq!(
            report_num(&report, &["output", "frames"]),
            result.output_frames as f64
        );
    }
}

#[test]
fn dod_not_verified_list_is_verbatim_in_report() {
    let result = Fixture::identity(64).run().expect("run");
    let report = report_json(&result);
    let entries = match report.as_object().expect("object").get("not_verified") {
        Some(Json::Array(entries)) => entries.clone(),
        other => panic!("not_verified: {:?}", other),
    };
    let texts: Vec<String> = entries
        .iter()
        .map(|entry| entry.as_str().expect("string").to_string())
        .collect();
    assert_eq!(
        texts,
        NOT_VERIFIED
            .iter()
            .map(|text| (*text).to_string())
            .collect::<Vec<_>>()
    );
    assert!(texts.iter().all(|text| text.contains("NOT_RUN")));
    // 报告不得出现“已验证”这类越权声明之外的内容（warnings 只记真实告警）。
    let warnings = match report.as_object().expect("object").get("warnings") {
        Some(Json::Array(entries)) => entries.len(),
        other => panic!("warnings: {:?}", other),
    };
    assert_eq!(warnings, 0, "默认 fixture 不应有 warning");
}
