// T11 私有测试：target CSV/JSON + sidecar 导入、28 拒收码、归一化/相位/端点/插值、
// flat/tilt10 内置目标、非模态 solver 读取接口。
//
// 本文件由 crates/designer/src/targets.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （见 tests/T11/README.md；不改 integrator 独占的 public_registration.rs）。
// 运行：cargo test -p khaen-designer targets
//
// 独立 oracle 说明：sha256 已知向量（""/"abc"）与规范点序列哈希由 Node.js crypto 独立复算
// （见 evidence/T11/MANIFEST.md §5），不是用本实现自证。

// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_approx(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: actual {actual} vs expected {expected} (tol {tol})"
    );
}

#[track_caller]
fn expect_code<T: std::fmt::Debug>(
    result: Result<T, TargetError>,
    code: TargetErrorCode,
) -> TargetError {
    match result {
        Ok(value) => panic!("期望 {} 错误，实际成功: {value:?}", code.as_str()),
        Err(error) => {
            assert_eq!(
                error.code,
                code,
                "错误码不符（期望 {}，实际 {}）：{}",
                code.as_str(),
                error.code_str(),
                error.detail
            );
            error
        }
    }
}

fn opts() -> ImportOptions {
    ImportOptions::new(48_000.0)
}

/// sidecar 基础字段（§6.3 必填）；fields 覆盖同名字段（值为原始 JSON 片段）。
fn sidecar(fields: &[(&str, &str)]) -> String {
    let base: [(&str, &str); 7] = [
        ("schema", "\"khaen-target-draft/v1\""),
        ("frequency_unit", "\"Hz\""),
        ("magnitude_unit", "\"dB\""),
        ("runtime_rate", "48000"),
        ("normalization", "\"none\""),
        ("interpolation", "\"log_frequency_linear\""),
        ("edge_policy", "\"hold\""),
    ];
    let mut merged: Vec<(&str, &str)> = Vec::new();
    for (key, value) in base {
        if fields.iter().any(|(override_key, _)| *override_key == key) {
            continue;
        }
        merged.push((key, value));
    }
    merged.extend(fields.iter().copied());
    let mut text = String::from("{");
    for (index, (key, value)) in merged.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push('"');
        text.push_str(key);
        text.push_str("\":");
        text.push_str(value);
    }
    text.push('}');
    text
}

/// CSV 形式 sidecar：带正确 source_hash。
fn sidecar_csv(csv: &str, fields: &[(&str, &str)]) -> String {
    let hash = sha256_hex(csv.as_bytes());
    let mut text = sidecar(fields);
    text.pop();
    text.push_str(",\"source_hash\":\"");
    text.push_str(&hash);
    text.push_str("\"}");
    text
}

fn csv_mag(points: &[(f64, f64)]) -> String {
    let mut text = String::from("freq_hz,mag_db\n");
    for (freq, mag) in points {
        text.push_str(&format!("{freq},{mag}\n"));
    }
    text
}

fn csv_phase(points: &[(f64, f64, f64)]) -> String {
    let mut text = String::from("freq_hz,mag_db,phase_deg\n");
    for (freq, mag, phase) in points {
        text.push_str(&format!("{freq},{mag},{phase}\n"));
    }
    text
}

fn import_csv_ok(csv: &str, fields: &[(&str, &str)]) -> TargetCurve {
    let text = sidecar_csv(csv, fields);
    import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("CSV 导入应成功")
}

fn import_json_obj(object: &str) -> Result<TargetCurve, TargetError> {
    import_json_bytes(object.as_bytes(), &opts())
}

fn json_points_mag(points: &[(f64, f64)]) -> String {
    let mut text = String::from("[");
    for (index, (freq, mag)) in points.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!("{{\"freq_hz\":{freq},\"mag_db\":{mag}}}"));
    }
    text.push(']');
    text
}

/// 相位 JSON points 构造 helper（当前相位用例走 CSV/sidecar；保留与 json_points_mag 对称，供后续相位 JSON 用例）。
#[allow(dead_code)]
fn json_points_phase(points: &[(f64, f64, f64)]) -> String {
    let mut text = String::from("[");
    for (index, (freq, mag, phase)) in points.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!(
            "{{\"freq_hz\":{freq},\"mag_db\":{mag},\"phase_deg\":{phase}}}"
        ));
    }
    text.push(']');
    text
}

/// 基础 JSON 文档（§6.3 必填 + points）。
fn json_doc(points: &str, extra: &[(&str, &str)]) -> String {
    let mut text = String::from(
        r#"{"schema":"khaen-target-draft/v1","frequency_unit":"Hz","magnitude_unit":"dB","runtime_rate":48000,"normalization":"none","interpolation":"log_frequency_linear","edge_policy":"hold","points":"#,
    );
    text.push_str(points);
    for (key, value) in extra {
        text.push_str(&format!(",\"{key}\":{value}"));
    }
    text.push('}');
    text
}

fn temp_case_dir(tag: &str) -> std::path::PathBuf {
    // 不用 %TEMP%：本机 TEMP 指向内存盘 R:\Temp，测试进程无写权限；用 crate 内 tests/T11 临时子目录。
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("T11")
        .join(format!("tmp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

// ---------------------------------------------------------------------------
// §6.9 码表 / §6.3 规范序列化 / sha256
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_error_code_table_is_28_unique() {
    assert_eq!(TargetErrorCode::ALL.len(), 28);
    let mut codes: Vec<&str> = Vec::new();
    for (index, code) in TargetErrorCode::ALL.iter().enumerate() {
        let text = code.as_str();
        assert!(text.starts_with("E_TGT_"), "码表字面值必须以 E_TGT_ 开头");
        assert_eq!(code.stage(), index as u8 + 1, "stage 必须等于码表顺序");
        assert!(!codes.contains(&text), "码表重复: {text}");
        codes.push(text);
    }
    // §6.9 码表 #1–#28 抽样核对（首/中/尾）。
    assert_eq!(TargetErrorCode::ALL[0].as_str(), "E_TGT_SCHEMA");
    assert_eq!(TargetErrorCode::ALL[13].as_str(), "E_TGT_MAG_MISSING");
    assert_eq!(TargetErrorCode::ALL[26].as_str(), "E_TGT_HASH");
    assert_eq!(TargetErrorCode::ALL[27].as_str(), "E_TGT_BUDGET");
}

#[test]
fn t11_targets_sha256_known_vectors() {
    // FIPS 180-4 已知向量（由 Node.js crypto 独立复算，见 MANIFEST §5）。
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // 规范点序列 sha256（无 phase）：与 Node.js crypto 复算值一致。
    let points = [
        TargetPoint::new(20.0, 0.0),
        TargetPoint::new(20_000.0, -10.0),
    ];
    let hash = canonical_points_sha256(&points).expect("规范哈希");
    assert_eq!(
        hash,
        "bbc46542f389e3de35077a7b99f452a55fd115e0aa77da904f9cfa608bac2827"
    );
    // 带 phase 的规范序列哈希。
    let points = [
        TargetPoint::new(20.0, 0.0).with_phase(0.0),
        TargetPoint::new(20_000.0, -10.0).with_phase(-180.0),
    ];
    assert_eq!(
        canonical_points_sha256(&points).expect("规范哈希"),
        "c27fcd42736a477466ee718362e24b4c98515cab584262cdd8cbae14c4af107d"
    );
}

#[test]
fn t11_targets_canonical_number_format() {
    assert_eq!(canonical_number(20.0).as_deref(), Some("20"));
    assert_eq!(canonical_number(-10.0).as_deref(), Some("-10"));
    assert_eq!(canonical_number(0.0).as_deref(), Some("0"));
    assert_eq!(canonical_number(-0.0).as_deref(), Some("0"));
    assert_eq!(canonical_number(0.5).as_deref(), Some("0.5"));
    assert_eq!(canonical_number(0.1).as_deref(), Some("0.1"));
    assert_eq!(canonical_number(1234.5).as_deref(), Some("1234.5"));
    assert_eq!(
        canonical_number(1e20).as_deref(),
        Some("100000000000000000000")
    );
    assert_eq!(canonical_number(1e21).as_deref(), Some("1e+21"));
    assert_eq!(canonical_number(1e-6).as_deref(), Some("0.000001"));
    assert_eq!(canonical_number(1e-7).as_deref(), Some("1e-7"));
    assert_eq!(canonical_number(-2.5e-8).as_deref(), Some("-2.5e-8"));
    assert_eq!(canonical_number(f64::NAN), None);
    assert_eq!(canonical_number(f64::INFINITY), None);
}

#[test]
fn t11_targets_canonical_points_json_exact() {
    let points = [
        TargetPoint::new(20.0, 0.0),
        TargetPoint::new(20_000.0, -10.0),
    ];
    assert_eq!(
        canonical_points_json(&points).expect("序列化"),
        "[{\"freq_hz\":20,\"mag_db\":0},{\"freq_hz\":20000,\"mag_db\":-10}]"
    );
    let points = [
        TargetPoint::new(20.0, 0.0).with_phase(0.0),
        TargetPoint::new(20_000.0, -10.0).with_phase(-180.0),
    ];
    assert_eq!(
        canonical_points_json(&points).expect("序列化"),
        "[{\"freq_hz\":20,\"mag_db\":0,\"phase_deg\":0},{\"freq_hz\":20000,\"mag_db\":-10,\"phase_deg\":-180}]"
    );
}

// ---------------------------------------------------------------------------
// 形式 A：CSV + sidecar（§6.2/§6.3）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_csv_mag_minimal_accepts() {
    let csv = csv_mag(&[(20.0, 0.0), (1000.0, 0.0), (20_000.0, -10.0)]);
    let curve = import_csv_ok(&csv, &[]);
    let report = curve.report();
    assert_eq!(report.form, TargetForm::Csv);
    assert_eq!(report.point_count, 3);
    assert!(!report.has_phase);
    assert!(
        report.source_hash_verified,
        "CSV 形式 source_hash 必须校验通过"
    );
    assert!(report.covers_20_20k);
    assert_eq!(report.at_20hz.value, Some(0.0));
    assert_eq!(report.at_20hz.source, CoverageSource::GridPoint(0));
    assert_eq!(report.at_20khz.value, Some(-10.0));
    assert_eq!(report.at_20khz.source, CoverageSource::GridPoint(2));
    assert_eq!(curve.meta().schema, TARGET_SCHEMA);
    assert_eq!(curve.meta().runtime_rate, 48_000.0);
    assert_eq!(curve.meta().edge_policy, EdgePolicy::Hold);
    assert!(!curve.has_phase());
    assert_eq!(curve.phase_total_deg_at(1000.0).expect("采样"), None);
}

#[test]
fn t11_targets_csv_with_phase_absolute_accepts() {
    let csv = csv_phase(&[
        (20.0, 0.0, 0.0),
        (1000.0, 0.0, -45.0),
        (20_000.0, 0.0, -90.0),
    ]);
    let curve = import_csv_ok(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    let report = curve.report();
    assert!(report.has_phase);
    assert_eq!(report.phase_target, PhaseTargetClass::Absolute);
    let unwrap = report.unwrap.expect("unwrap 报告");
    assert_eq!(unwrap.policy, UnwrapPolicy::Unwrapped);
    assert_eq!(unwrap.wraps_applied, 0);
    assert_approx(unwrap.max_abs_delta_deg, 45.0, 1e-12, "max|Δφ|");
    // absolute：总相位 = 给定相位（不额外叠加延迟）。
    assert_eq!(curve.phase_total_deg_at(1000.0).expect("相位"), Some(-45.0));
    assert_eq!(curve.meta().phase_ref, PhaseRef::Absolute);
    assert_eq!(curve.meta().phase_unit.as_deref(), Some("deg"));
}

#[test]
fn t11_targets_csv_header_must_be_exact() {
    let csv = "frequency,magnitude\n20,0\n20000,-10\n";
    let error = expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar(&[]), &opts()),
        TargetErrorCode::CsvFormat,
    );
    assert!(error.detail.contains("表头"));
}

#[test]
fn t11_targets_csv_field_count_mismatch_rejected() {
    let csv = "freq_hz,mag_db\n20,0,extra\n20000,-10\n";
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar(&[]), &opts()),
        TargetErrorCode::CsvFormat,
    );
}

#[test]
fn t11_targets_csv_comment_line_rejected() {
    let csv = "freq_hz,mag_db\n# comment\n20,0\n20000,-10\n";
    let error = expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar(&[]), &opts()),
        TargetErrorCode::CsvFormat,
    );
    assert!(error.detail.contains("注释"));
}

#[test]
fn t11_targets_csv_empty_field_rejected() {
    let csv = "freq_hz,mag_db\n20,\n20000,-10\n";
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar(&[]), &opts()),
        TargetErrorCode::CsvFormat,
    );
}

#[test]
fn t11_targets_csv_invalid_number_rejected() {
    let csv = "freq_hz,mag_db\n20,abc\n20000,-10\n";
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar(&[]), &opts()),
        TargetErrorCode::CsvFormat,
    );
}

#[test]
fn t11_targets_csv_crlf_and_spaces_accepted() {
    let csv = "freq_hz,mag_db\r\n 20 , 0 \r\n20000, -10\r\n";
    let curve = import_csv_ok(csv, &[]);
    assert_eq!(curve.report().point_count, 2);
    assert_eq!(curve.raw_points()[0].freq_hz, 20.0);
}

#[test]
fn t11_targets_csv_point_count_bounds() {
    let csv = csv_mag(&[(20.0, 0.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::Points,
    );
    let mut rows = Vec::new();
    for index in 0..=MAX_POINTS {
        rows.push((20.0 + index as f64, 0.0));
    }
    let csv = csv_mag(&rows);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::Points,
    );
}

#[test]
fn t11_targets_csv_missing_sidecar_file_is_missing_field() {
    let dir = temp_case_dir("csv-missing-sidecar");
    let csv_path = dir.join("target.csv");
    std::fs::write(&csv_path, csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)])).expect("写 CSV");
    let error = expect_code(
        load_csv_file(&csv_path, None, &opts()),
        TargetErrorCode::MissingField,
    );
    assert!(error.detail.contains("sidecar"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn t11_targets_csv_load_from_file_auto_sidecar() {
    let dir = temp_case_dir("csv-load");
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, -10.0)]);
    let csv_path = dir.join("target.csv");
    std::fs::write(&csv_path, &csv).expect("写 CSV");
    let sidecar_path = default_sidecar_path(&csv_path);
    assert_eq!(
        sidecar_path.file_name().and_then(|name| name.to_str()),
        Some("target.csv.json"),
        "默认 sidecar = 同名 + .json"
    );
    std::fs::write(&sidecar_path, sidecar_csv(&csv, &[])).expect("写 sidecar");
    let curve = load_csv_file(&csv_path, None, &opts()).expect("文件导入");
    assert_eq!(curve.report().form, TargetForm::Csv);
    assert!(curve.report().source_hash_verified);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn t11_targets_csv_hash_missing_and_mismatch() {
    // 缺 source_hash → E_TGT_HASH（§6.9.1 矩阵：source_hash 缺失 → E_TGT_HASH）。
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar(&[]), &opts()),
        TargetErrorCode::Hash,
    );
    // 哈希不符。
    let mut text = sidecar(&[]);
    text.pop();
    text.push_str(
        ",\"source_hash\":\"0000000000000000000000000000000000000000000000000000000000000000\"}",
    );
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Hash,
    );
    // 非 hex 长度。
    let mut text = sidecar(&[]);
    text.pop();
    text.push_str(",\"source_hash\":\"abc\"}");
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Hash,
    );
    // source_file_sha256 与实际文件字节不符。
    let mut text = sidecar_csv(&csv, &[]);
    text.pop();
    text.push_str(",\"source_file_sha256\":\"1111111111111111111111111111111111111111111111111111111111111111\"}");
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Hash,
    );
}

#[test]
fn t11_targets_csv_source_file_hash_verified() {
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let mut text = sidecar_csv(&csv, &[]);
    text.pop();
    text.push_str(",\"source_file_sha256\":\"");
    text.push_str(&sha256_hex(csv.as_bytes()));
    text.push_str("\"}");
    let curve = import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("导入");
    assert!(curve.report().source_hash_verified);
    assert!(curve.report().source_file_sha256_verified);
}

#[test]
fn t11_targets_csv_sidecar_points_is_mix_error() {
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let mut text = sidecar_csv(&csv, &[]);
    text.pop();
    text.push_str(",\"points\":[{\"freq_hz\":20,\"mag_db\":0}]}");
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::SidecarMix,
    );
}

// ---------------------------------------------------------------------------
// 形式 B：单 JSON（§6.3）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_json_minimal_accepts() {
    let doc = json_doc(&json_points_mag(&[(20.0, 0.0), (20_000.0, -10.0)]), &[]);
    let curve = import_json_obj(&doc).expect("JSON 导入");
    let report = curve.report();
    assert_eq!(report.form, TargetForm::Json);
    assert_eq!(report.point_count, 2);
    assert!(!report.has_phase);
    assert!(!report.source_hash_verified);
    assert!(report.covers_20_20k);
    assert_eq!(curve.target_sha256().len(), 64);
}

#[test]
fn t11_targets_json_schema_missing_or_wrong() {
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    // 缺 schema。
    let doc = format!(
        "{{\"frequency_unit\":\"Hz\",\"magnitude_unit\":\"dB\",\"runtime_rate\":48000,\"normalization\":\"none\",\"interpolation\":\"log_frequency_linear\",\"edge_policy\":\"hold\",\"points\":{points}}}"
    );
    expect_code(import_json_obj(&doc), TargetErrorCode::Schema);
    // 错 schema。
    let doc = json_doc(&points, &[]).replace("khaen-target-draft/v1", "khaen-target-draft/v2");
    expect_code(import_json_obj(&doc), TargetErrorCode::Schema);
    // JSON 语法错误 / 非对象根。
    expect_code(import_json_obj("{\"schema\":"), TargetErrorCode::Schema);
    expect_code(import_json_obj("[1,2,3]"), TargetErrorCode::Schema);
}

#[test]
fn t11_targets_json_missing_required_field() {
    // 缺 runtime_rate → E_TGT_RATE（§6.9.1 矩阵：runtime_rate 缺失/不等于 fs → RATE）。
    let doc = format!(
        "{{\"schema\":\"khaen-target-draft/v1\",\"frequency_unit\":\"Hz\",\"magnitude_unit\":\"dB\",\"normalization\":\"none\",\"interpolation\":\"log_frequency_linear\",\"edge_policy\":\"hold\",\"points\":{}}}",
        json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)])
    );
    expect_code(import_json_obj(&doc), TargetErrorCode::Rate);
    // 缺 normalization → E_TGT_MISSING_FIELD。
    let doc = format!(
        "{{\"schema\":\"khaen-target-draft/v1\",\"frequency_unit\":\"Hz\",\"magnitude_unit\":\"dB\",\"runtime_rate\":48000,\"interpolation\":\"log_frequency_linear\",\"edge_policy\":\"hold\",\"points\":{}}}",
        json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)])
    );
    expect_code(import_json_obj(&doc), TargetErrorCode::MissingField);
    // 缺 points（JSON 形式必填）。
    let doc = r#"{"schema":"khaen-target-draft/v1","frequency_unit":"Hz","magnitude_unit":"dB","runtime_rate":48000,"normalization":"none","interpolation":"log_frequency_linear","edge_policy":"hold"}"#;
    expect_code(import_json_obj(doc), TargetErrorCode::MissingField);
}

#[test]
fn t11_targets_json_unknown_fields_and_x_prefix() {
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    // 顶层未知非 x_ 字段 → E_TGT_FIELD_UNKNOWN。
    let doc = json_doc(&points, &[("unknown_thing", "1")]);
    expect_code(import_json_obj(&doc), TargetErrorCode::FieldUnknown);
    // x_ 前缀 → warning，不拒收。
    let doc = json_doc(&points, &[("x_note", "\"ext\"")]);
    let curve = import_json_obj(&doc).expect("x_ 字段应 warning 通过");
    assert!(curve
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::XFieldTopLevel));
}

#[test]
fn t11_targets_json_point_unknown_field() {
    let points = "[{\"freq_hz\":20,\"mag_db\":0,\"foo\":1},{\"freq_hz\":20000,\"mag_db\":0}]";
    let doc = json_doc(points, &[]);
    expect_code(import_json_obj(&doc), TargetErrorCode::PointField);
    // points 内 x_ 前缀 → warning。
    let points = "[{\"freq_hz\":20,\"mag_db\":0,\"x_tag\":1},{\"freq_hz\":20000,\"mag_db\":0}]";
    let curve = import_json_obj(&json_doc(points, &[])).expect("points x_ 应通过");
    assert!(curve
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::XFieldPoint));
}

#[test]
fn t11_targets_json_units_enforced() {
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    // 单位不是 Hz → E_TGT_UNIT（不猜 kHz）。
    let doc =
        json_doc(&points, &[]).replace("\"frequency_unit\":\"Hz\"", "\"frequency_unit\":\"kHz\"");
    let error = expect_code(import_json_obj(&doc), TargetErrorCode::Unit);
    assert!(error.detail.contains("kHz"));
    let doc = json_doc(&points, &[])
        .replace("\"magnitude_unit\":\"dB\"", "\"magnitude_unit\":\"linear\"");
    expect_code(import_json_obj(&doc), TargetErrorCode::Unit);
    let doc = json_doc(&points, &[]).replace("\"magnitude_unit\":\"dB\"", "\"magnitude_unit\":5");
    expect_code(import_json_obj(&doc), TargetErrorCode::Unit);
    // 单位完全缺失 → E_TGT_UNIT（矩阵：单位缺失 → UNIT，不默认猜测）。
    let doc = json_doc(&points, &[]).replace("\"frequency_unit\":\"Hz\",", "");
    expect_code(import_json_obj(&doc), TargetErrorCode::Unit);
}

#[test]
fn t11_targets_json_rate_must_equal_profile() {
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let doc = json_doc(&points, &[]).replace("\"runtime_rate\":48000", "\"runtime_rate\":44100");
    expect_code(import_json_obj(&doc), TargetErrorCode::Rate);
    let doc =
        json_doc(&points, &[]).replace("\"runtime_rate\":48000", "\"runtime_rate\":\"48000\"");
    expect_code(import_json_obj(&doc), TargetErrorCode::Rate);
    // runtime_rate=96000 且 profile fs=96000 → 通过。
    let doc = json_doc(&points, &[]).replace("\"runtime_rate\":48000", "\"runtime_rate\":96000");
    let curve = import_json_bytes(doc.as_bytes(), &ImportOptions::new(96_000.0)).expect("96k");
    assert_eq!(curve.fs(), 96_000.0);
}

#[test]
fn t11_targets_json_source_hash_canonical() {
    // 规范点序列 [{"freq_hz":20,"mag_db":0},{"freq_hz":20000,"mag_db":-10}]
    // sha256 = bbc46542...（Node.js crypto 独立复算）。
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, -10.0)]);
    let hash = "bbc46542f389e3de35077a7b99f452a55fd115e0aa77da904f9cfa608bac2827";
    let doc = json_doc(&points, &[("source_hash", &format!("\"{hash}\""))]);
    let curve = import_json_obj(&doc).expect("规范 source_hash 应通过");
    assert!(curve.report().source_hash_verified);
    // 篡改 → E_TGT_HASH。
    let bad = "00".repeat(32);
    let doc = json_doc(&points, &[("source_hash", &format!("\"{bad}\""))]);
    expect_code(import_json_obj(&doc), TargetErrorCode::Hash);
    // 非 hex。
    let doc = json_doc(&points, &[("source_hash", "\"abc\"")]);
    expect_code(import_json_obj(&doc), TargetErrorCode::Hash);
}

#[test]
fn t11_targets_json_source_file_sha256() {
    // 单文件 JSON 无法自引用校验自身字节（写入后哈希变化）：合法 hex 只登记，verified=false + warning。
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let doc = json_doc(
        &points,
        &[("source_file_sha256", &format!("\"{}\"", "11".repeat(32)))],
    );
    let curve = import_json_obj(&doc).expect("JSON source_file_sha256 应登记不冒充校验");
    assert!(!curve.report().source_file_sha256_verified);
    assert!(curve
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::SourceFileHashNotVerifiable));
    // 非 hex → E_TGT_HASH。
    let doc = json_doc(&points, &[("source_file_sha256", "\"abc\"")]);
    expect_code(import_json_obj(&doc), TargetErrorCode::Hash);
}

#[test]
fn t11_targets_json_interp_and_norm_values() {
    let points = json_points_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let doc = json_doc(&points, &[]).replace("log_frequency_linear", "linear");
    expect_code(import_json_obj(&doc), TargetErrorCode::Interp);
    let doc = json_doc(&points, &[]).replace(
        "\"normalization\":\"none\"",
        "\"normalization\":\"loudness\"",
    );
    expect_code(import_json_obj(&doc), TargetErrorCode::Norm);
}

#[test]
fn t11_targets_normalization_ref1k_and_peak() {
    // ref_1k：1 kHz 处插值为 0 dB（曲线 20 Hz +6、1 kHz 0、20 kHz 0）。
    let rows = [(20.0, 6.0), (1_000.0, 3.0), (20_000.0, -3.0)];
    let csv = csv_mag(&rows);
    let curve = import_csv_ok(&csv, &[("normalization", "\"ref_1k\"")]);
    let report = curve.report();
    assert_approx(report.normalization_offset_db, -3.0, 1e-12, "ref_1k offset");
    assert_approx(
        curve.mag_db_at(1_000.0).expect("采样"),
        0.0,
        1e-12,
        "1k 归一",
    );

    // peak：max(mag)=0（原 max=+6）。
    let curve = import_csv_ok(&csv, &[("normalization", "\"peak\"")]);
    let report = curve.report();
    assert_approx(report.normalization_offset_db, -6.0, 1e-12, "peak offset");
    let max = curve
        .final_mag_db()
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    assert_approx(max, 0.0, 1e-12, "peak 归一");
}

#[test]
fn t11_targets_reference_gain_range_and_effect() {
    let csv = csv_mag(&[(20.0, 0.0), (1_000.0, 0.0), (20_000.0, 0.0)]);
    // 独立 reference gain：形状不变、整体 +3 dB。
    let curve = import_csv_ok(&csv, &[("reference_gain_db", "3")]);
    let report = curve.report();
    assert_eq!(report.reference_gain_db, 3.0);
    assert_eq!(report.normalization, Normalization::None);
    assert_approx(report.applied_offset_db, 3.0, 1e-12, "applied_offset");
    assert_approx(
        curve.mag_db_at(1_000.0).expect("采样"),
        3.0,
        1e-12,
        "最终幅度",
    );
    assert_approx(
        curve.mag_shape_db_at(1_000.0).expect("形状"),
        0.0,
        1e-12,
        "形状幅度",
    );
    // 越界 [-24,+24]。
    let curve_err = import_csv_bytes(
        csv.as_bytes(),
        &sidecar_csv(&csv, &[("reference_gain_db", "24.5")]),
        &opts(),
    );
    expect_code(curve_err, TargetErrorCode::GainRange);
    let curve_err = import_csv_bytes(
        csv.as_bytes(),
        &sidecar_csv(&csv, &[("reference_gain_db", "-25")]),
        &opts(),
    );
    expect_code(curve_err, TargetErrorCode::GainRange);
}

#[test]
fn t11_targets_boost_levels() {
    // 1 kHz 为 0，20 Hz +8 dB → max_boost=8 → warning。
    let rows = [(20.0, 8.0), (1_000.0, 0.0), (20_000.0, 0.0)];
    let csv = csv_mag(&rows);
    let curve = import_csv_ok(&csv, &[]);
    let report = curve.report();
    assert_approx(report.max_boost_db, 8.0, 1e-12, "max_boost");
    assert!(report.boost_warning);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::Boost));

    // +13 dB 超过拒收阈值且未 allow_boost → E_TGT_BOOST。
    let rows = [(20.0, 13.0), (1_000.0, 0.0), (20_000.0, 0.0)];
    let csv = csv_mag(&rows);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::Boost,
    );
    // 显式 allow_boost → 通过。
    let text = sidecar_csv(&csv, &[]);
    let mut options = opts();
    options.allow_boost = true;
    let curve = import_csv_bytes(csv.as_bytes(), &text, &options).expect("allow_boost 通过");
    assert!(curve.report().allow_boost);
    assert_approx(curve.report().max_boost_db, 13.0, 1e-12, "max_boost 13");

    // 硬上限 +18：19 dB 即使 allow_boost 也拒收。
    let rows = [(20.0, 19.0), (1_000.0, 0.0), (20_000.0, 0.0)];
    let csv = csv_mag(&rows);
    let text = sidecar_csv(&csv, &[]);
    let mut options = opts();
    options.allow_boost = true;
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &options),
        TargetErrorCode::Boost,
    );
}

#[test]
fn t11_targets_edge_policy_and_coverage() {
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    // 非法值 → E_TGT_EDGE。
    expect_code(
        import_csv_bytes(
            csv.as_bytes(),
            &sidecar_csv(&csv, &[("edge_policy", "\"clamp\"")]),
            &opts(),
        ),
        TargetErrorCode::Edge,
    );
    // reject 且网格不覆盖 20/20k → E_TGT_EDGE。
    let partial = csv_mag(&[(30.0, 0.0), (16_000.0, 0.0)]);
    let text = sidecar_csv(&partial, &[("edge_policy", "\"reject\"")]);
    expect_code(
        import_csv_bytes(partial.as_bytes(), &text, &opts()),
        TargetErrorCode::Edge,
    );
    // reject 且覆盖 → 通过；采样网格外 → E_TGT_EDGE。
    let text = sidecar_csv(&csv, &[("edge_policy", "\"reject\"")]);
    let curve = import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("reject+覆盖");
    expect_code(curve.sample(15.0), TargetErrorCode::Edge);
    expect_code(curve.sample(21_000.0), TargetErrorCode::Edge);
    // require_20_20k_coverage=false 时允许 reject 不覆盖（设计域不需要 20/20k）。
    let mut options = opts();
    options.require_20_20k_coverage = false;
    let text = sidecar_csv(&partial, &[("edge_policy", "\"reject\"")]);
    import_csv_bytes(partial.as_bytes(), &text, &options).expect("不要求覆盖时应通过");
}

// ---------------------------------------------------------------------------
// 相位语义（§6.6）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_phase_unit_missing_or_extra() {
    // 有 phase 却缺 phase_unit → E_TGT_PHASE_UNIT（§6.9.1 矩阵优先于 MISSING_FIELD）。
    let csv = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -10.0)]);
    let text = sidecar_csv(
        &csv,
        &[
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseUnit,
    );
    // 无 phase 却给 phase_unit → E_TGT_PHASE_UNIT。
    let plain = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let text = sidecar_csv(&plain, &[("phase_unit", "\"deg\"")]);
    expect_code(
        import_csv_bytes(plain.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseUnit,
    );
    // phase_unit 非 deg → E_TGT_UNIT。
    let text = sidecar_csv(
        &csv,
        &[
            ("phase_unit", "\"rad\""),
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Unit,
    );
}

#[test]
fn t11_targets_phase_ref_missing_or_conflict() {
    let csv = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -10.0)]);
    // 有 phase 但缺 phase_ref → E_TGT_PHASE_REF。
    let text = sidecar_csv(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseRef,
    );
    // phase_ref 非法。
    let text = sidecar_csv(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"weird\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseRef,
    );
    // 无 phase 却声明 absolute → E_TGT_PHASE_REF。
    let plain = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let text = sidecar_csv(&plain, &[("phase_ref", "\"absolute\"")]);
    expect_code(
        import_csv_bytes(plain.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseRef,
    );
    // delay_removed 缺 target_delay_samples → E_TGT_PHASE_REF。
    let text = sidecar_csv(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"delay_removed\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseRef,
    );
}

#[test]
fn t11_targets_phase_unwrap_policies() {
    // unwrapped 且相邻 |Δφ| > 180 → E_TGT_PHASE_JUMP。
    let csv = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -270.0)]);
    let mut text = sidecar_csv(
        &csv,
        &[("phase_unit", "\"deg\""), ("phase_ref", "\"absolute\"")],
    );
    text.pop();
    text.push_str(",\"unwrap_policy\":\"unwrapped\"}");
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::PhaseJump,
    );
    // auto：-270° 展开为 +90°（相对上一点），报告 wraps_applied。
    let mut text = sidecar_csv(
        &csv,
        &[("phase_unit", "\"deg\""), ("phase_ref", "\"absolute\"")],
    );
    text.pop();
    text.push_str(",\"unwrap_policy\":\"auto\"}");
    let curve = import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("auto 展开");
    let unwrap = curve.report().unwrap.expect("unwrap");
    assert_eq!(unwrap.policy, UnwrapPolicy::Auto);
    assert_eq!(unwrap.wraps_applied, 1, "一次 ±360 展开");
    assert_approx(unwrap.raw_max_abs_delta_deg, 270.0, 1e-12, "raw max|Δ|");
    assert_approx(unwrap.max_abs_delta_deg, 90.0, 1e-12, "展开后 max|Δ|");
    assert_approx(
        curve
            .phase_total_deg_at(20_000.0)
            .expect("相位")
            .expect("有相位"),
        90.0,
        1e-12,
        "展开后的 φ",
    );
}

#[test]
fn t11_targets_phase_fs2_ruling() {
    // magnitude-only：freq == fs/2 (24000) 允许（Lead 裁决）。
    let csv = csv_mag(&[(20.0, 0.0), (24_000.0, -6.0)]);
    let curve = import_csv_ok(&csv, &[]);
    assert_eq!(curve.report().max_freq_hz, 24_000.0);
    // 带 phase：freq == fs/2 → E_TGT_PHASE_REAL（F-23(a)：静态可判的实系数反例）。
    // 判据为什么在静态层可判：实系数 FIR 在 Nyquist 的响应必为实数
    //   H(fs/2) = Σ_n h[n]·e^(−jπn) = Σ_n h[n]·(−1)^n ∈ ℝ
    // 故 fs/2 处相位只能是 0°/180°；样例 −90° 直接违反实系数约束（任何 taps/D 都不可实现）。
    // Lead 裁决更严：带 phase 目标只要含 fs/2 一律拒收（Nyquist 相位退化），不止非 0/180 情形。
    for phase in [-90.0, 0.0, 180.0] {
        let csv = csv_phase(&[(20.0, 0.0, 0.0), (24_000.0, 0.0, phase)]);
        let text = sidecar_csv(
            &csv,
            &[
                ("phase_unit", "\"deg\""),
                ("phase_ref", "\"absolute\""),
                ("unwrap_policy", "\"unwrapped\""),
            ],
        );
        let error = expect_code(
            import_csv_bytes(csv.as_bytes(), &text, &opts()),
            TargetErrorCode::PhaseReal,
        );
        assert!(
            error.detail.contains("实系数") || error.detail.contains("Nyquist"),
            "fs/2 拒收须写明实系数/Nyquist 原因: {}",
            error.detail
        );
    }
}

#[test]
#[ignore = "requires repo-external evidence; not shipped"]
fn t11_targets_phase_real_ownership() {
    // F-23(b)：E_TGT_PHASE_REAL 的后半句「相位无法由实系数因果 FIR 表示」静态层不可判，
    // 责任层见 spec §6.6/§6.9 #19/§6.9.1（T37 设计层 + T55 报告层），本静态层只判 fs/2 子条件。
    //
    // 为何静态层不可判：任意有限点集 (f_k, H_k)（f_k ∈ (0, fs/2)）都可由足够长度（L ≥ 2N）的
    // 实系数 FIR 精确插值——未知数 L 个实 h[n]、方程 2N 个（每点实/虚各一），频点互异时线性系统
    // 满秩；因此非 Nyquist 的任意有限相位点都不构成静态可判的实系数反例，静态层猜测会误拒。
    // 可表示性只在给定 taps / D / 预算后才有意义，属设计期（T37 corrected：E_FIR_NONCAUSAL /
    // 相位误差判据 INCOMPLETE；T55：corrected_phase_target/corrected_complex_error + E_TGT_BUDGET）。
    let csv = csv_phase(&[(20.0, 0.0, 90.0), (1_000.0, 0.0, 0.0), (20_000.0, 0.0, 60.0)]);
    let curve = import_csv_ok(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    assert_eq!(curve.report().phase_target, PhaseTargetClass::Absolute);
    assert_approx(
        curve
            .phase_total_deg_at(1_000.0)
            .expect("相位")
            .expect("有相位"),
        0.0,
        1e-12,
        "非 Nyquist 相位原样透传（静态层不判实系数可表示性）",
    );
    // spec 责任层归属必须写清（不留空），且与代码/测试互引一致（可复跑检查）。
    let spec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/fir-toolbox.md");
    let spec = std::fs::read_to_string(&spec_path).expect("读取 spec/fir-toolbox.md");
    for needle in [
        "静态导入层 T11 判定",
        "静态导入层不可判",
        "由设计层 T37",
        "报告层 T55",
        "t37_non_causal_target_rejected",
        "t55_report_corrected_complex_target_equivalence_and_delay",
    ] {
        assert!(
            spec.contains(needle),
            "spec 缺少 E_TGT_PHASE_REAL 责任层归属/测试互引: {needle}"
        );
    }
}

#[test]
fn t11_targets_phase_absolute_no_delay_overlay() {
    // absolute 相位即总相位；采样不额外叠加延迟。
    let csv = csv_phase(&[
        (20.0, 0.0, 0.0),
        (1_000.0, 0.0, -90.0),
        (20_000.0, 0.0, -180.0),
    ]);
    let curve = import_csv_ok(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    assert_approx(
        curve
            .phase_total_deg_at(1_000.0)
            .expect("相位")
            .expect("有"),
        -90.0,
        1e-12,
        "absolute φ_tot = given",
    );
    assert!(curve
        .report()
        .notes
        .iter()
        .any(|note| note.contains("absolute")));
}

#[test]
fn t11_targets_phase_delay_removed_transform() {
    // φ_tot(f) = φ_ex(f) − 360·f·D/fs；D=48 samples @48k → 1 kHz 扣 36°。
    let csv = csv_phase(&[
        (20.0, 0.0, 0.0),
        (1_000.0, 0.0, -90.0),
        (20_000.0, 0.0, -180.0),
    ]);
    let curve = import_csv_ok(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"delay_removed\""),
            ("unwrap_policy", "\"unwrapped\""),
            ("target_delay_samples", "12"),
        ],
    );
    let report = curve.report();
    assert_eq!(report.phase_target, PhaseTargetClass::DelayRemoved);
    assert_eq!(report.target_delay_samples, Some(12));
    assert_eq!(curve.meta().phase_ref, PhaseRef::DelayRemoved);
    // φ_tot = φ_ex − 360·f·D/fs；D=12 sample @48k → 1 kHz 扣 90°。
    assert_approx(
        curve.phase_ex_deg_at(1_000.0).expect("φ_ex").expect("有"),
        -90.0,
        1e-12,
        "φ_ex",
    );
    assert_approx(
        curve
            .phase_total_deg_at(1_000.0)
            .expect("φ_tot")
            .expect("有"),
        -180.0,
        1e-9,
        "φ_tot",
    );
    // D_used 一致性（±0.01 sample）。
    curve.check_delay_consistency(12.0).expect("一致");
    expect_code(curve.check_delay_consistency(12.02), TargetErrorCode::Delay);
}

#[test]
fn t11_targets_delay_samples_validation() {
    let csv = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -10.0)]);
    let phase_fields = [
        ("phase_unit", "\"deg\""),
        ("phase_ref", "\"delay_removed\""),
        ("unwrap_policy", "\"unwrapped\""),
    ];
    // 非整数。
    let mut fields = phase_fields.to_vec();
    fields.push(("target_delay_samples", "10.5"));
    let text = sidecar_csv(&csv, &fields);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Delay,
    );
    // 超过 200 ms（48k → 9600 sample 为界）。
    let mut fields = phase_fields.to_vec();
    fields.push(("target_delay_samples", "9601"));
    let text = sidecar_csv(&csv, &fields);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Delay,
    );
    // 边界 9600 通过。
    let mut fields = phase_fields.to_vec();
    fields.push(("target_delay_samples", "9600"));
    let text = sidecar_csv(&csv, &fields);
    import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("9600 sample = 200 ms 应通过");
    // 非 delay_removed 却给 target_delay_samples → E_TGT_DELAY。
    let mut fields = vec![("phase_unit", "\"deg\""), ("phase_ref", "\"absolute\"")];
    fields.push(("unwrap_policy", "\"unwrapped\""));
    fields.push(("target_delay_samples", "10"));
    let text = sidecar_csv(&csv, &fields);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &opts()),
        TargetErrorCode::Delay,
    );
}

// ---------------------------------------------------------------------------
// 频点/幅度/相位混合等点级拒收码（§6.4）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_freq_invalid_and_nan() {
    let csv = csv_mag(&[(0.0, 0.0), (20_000.0, 0.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::FreqInvalid,
    );
    let csv = csv_mag(&[(-5.0, 0.0), (20_000.0, 0.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::FreqInvalid,
    );
    let csv = "freq_hz,mag_db\nNaN,0\n20000,0\n";
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(csv, &[]), &opts()),
        TargetErrorCode::FreqInvalid,
    );
}

#[test]
fn t11_targets_freq_order_and_duplicate() {
    let csv = csv_mag(&[(20.0, 0.0), (20.0, -1.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::FreqOrder,
    );
    let csv = csv_mag(&[(1_000.0, 0.0), (20.0, 0.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::FreqOrder,
    );
}

#[test]
fn t11_targets_freq_range_above_nyquist() {
    let csv = csv_mag(&[(20.0, 0.0), (25_000.0, 0.0)]);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(&csv, &[]), &opts()),
        TargetErrorCode::FreqRange,
    );
}

#[test]
fn t11_targets_mag_phase_nan() {
    let csv = "freq_hz,mag_db\n20,NaN\n20000,0\n";
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(csv, &[]), &opts()),
        TargetErrorCode::Nan,
    );
    let csv = "freq_hz,mag_db\n20,inf\n20000,0\n";
    expect_code(
        import_csv_bytes(csv.as_bytes(), &sidecar_csv(csv, &[]), &opts()),
        TargetErrorCode::Nan,
    );
    // JSON 非数值类型 → E_TGT_NAN。
    let points = "[{\"freq_hz\":20,\"mag_db\":\"oops\"},{\"freq_hz\":20000,\"mag_db\":0}]";
    expect_code(
        import_json_obj(&json_doc(points, &[])),
        TargetErrorCode::Nan,
    );
}

#[test]
fn t11_targets_mag_missing_and_phase_mixed() {
    // 缺 mag → E_TGT_MAG_MISSING。
    let points = "[{\"freq_hz\":20},{\"freq_hz\":20000,\"mag_db\":0}]";
    expect_code(
        import_json_obj(&json_doc(points, &[])),
        TargetErrorCode::MagMissing,
    );
    // phase 部分有部分没有 → E_TGT_PHASE_MIXED。
    let points = "[{\"freq_hz\":20,\"mag_db\":0,\"phase_deg\":0},{\"freq_hz\":20000,\"mag_db\":0}]";
    let extra = [
        ("phase_unit", "\"deg\""),
        ("phase_ref", "\"absolute\""),
        ("unwrap_policy", "\"unwrapped\""),
    ];
    expect_code(
        import_json_obj(&json_doc(points, &extra)),
        TargetErrorCode::PhaseMixed,
    );
}

// ---------------------------------------------------------------------------
// 族相位策略（§6.10）与预算（§6.9 码 28）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_family_phase_policy() {
    let csv = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -10.0)]);
    let phase_fields = [
        ("phase_unit", "\"deg\""),
        ("phase_ref", "\"absolute\""),
        ("unwrap_policy", "\"unwrapped\""),
    ];
    let text = sidecar_csv(&csv, &phase_fields);
    // linear（幅度族）收到 phase → E_TGT_PHASE_UNSUPPORTED。
    let mut options = opts();
    options.family = Some(TargetFamily::Linear);
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &options),
        TargetErrorCode::PhaseUnsupported,
    );
    // 显式 allow_phase_ignored → 通过且报告 phase_target=ignored。
    options.allow_phase_ignored = true;
    let curve = import_csv_bytes(csv.as_bytes(), &text, &options).expect("ignored 通过");
    assert_eq!(curve.report().phase_target, PhaseTargetClass::Ignored);
    // corrected/lowdelay 必须 phase：无 phase → E_TGT_PHASE_UNSUPPORTED。
    let plain = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let plain_text = sidecar_csv(&plain, &[]);
    let mut options = opts();
    options.family = Some(TargetFamily::Corrected);
    expect_code(
        import_csv_bytes(plain.as_bytes(), &plain_text, &options),
        TargetErrorCode::PhaseUnsupported,
    );
    // corrected + phase → 通过。
    let curve = import_csv_bytes(csv.as_bytes(), &text, &options).expect("corrected + phase");
    assert_eq!(curve.report().phase_target, PhaseTargetClass::Absolute);
}

#[test]
fn t11_targets_budget_checks() {
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let text = sidecar_csv(&csv, &[]);
    // 点数预算。
    let mut options = opts();
    options.budget = Some(TargetBudget {
        max_points: Some(1),
        ..TargetBudget::default()
    });
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &options),
        TargetErrorCode::Budget,
    );
    // 延迟预算（delay_removed D_target=48 vs 预算 10）。
    let phase = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -10.0)]);
    let phase_text = sidecar_csv(
        &phase,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"delay_removed\""),
            ("unwrap_policy", "\"unwrapped\""),
            ("target_delay_samples", "48"),
        ],
    );
    let mut options = opts();
    options.budget = Some(TargetBudget {
        delay_budget_samples: Some(10.0),
        ..TargetBudget::default()
    });
    expect_code(
        import_csv_bytes(phase.as_bytes(), &phase_text, &options),
        TargetErrorCode::Budget,
    );
    // taps 预算 < 8。
    let mut options = opts();
    options.budget = Some(TargetBudget {
        max_taps: Some(4),
        ..TargetBudget::default()
    });
    expect_code(
        import_csv_bytes(csv.as_bytes(), &text, &options),
        TargetErrorCode::Budget,
    );
}

// ---------------------------------------------------------------------------
// 插值/端点/采样（§6.7）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_log_interpolation_values() {
    // 2 点：20 Hz 0 dB、20 kHz −10 dB；log-f 线性。
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, -10.0)]);
    let curve = import_csv_ok(&csv, &[]);
    // 200 Hz：t = log10(10)/log10(1000) = 1/3 → mag = −10/3。
    assert_approx(
        curve.mag_db_at(200.0).expect("采样"),
        -10.0 / 3.0,
        1e-12,
        "200 Hz",
    );
    // 中点 632.455 Hz：t = 0.5 → −5 dB。
    let mid = (20.0f64 * 20_000.0).sqrt();
    assert_approx(curve.mag_db_at(mid).expect("采样"), -5.0, 1e-12, "几何中点");
    // 20 kHz 精确网格点。
    assert_approx(
        curve.mag_db_at(20_000.0).expect("采样"),
        -10.0,
        1e-12,
        "20 kHz",
    );
    // 采样来源。
    let sample = curve.sample(200.0).expect("采样");
    assert_eq!(sample.mag_source, CoverageSource::Interpolated);
    let sample = curve.sample(20.0).expect("采样");
    assert_eq!(sample.mag_source, CoverageSource::GridPoint(0));
}

#[test]
fn t11_targets_edge_hold_sampling_and_report() {
    // 30–16000 Hz + hold：20 Hz/20 kHz 取端点并报告来源。
    let csv = csv_mag(&[(30.0, 1.0), (16_000.0, -1.0)]);
    let curve = import_csv_ok(&csv, &[]);
    let report = curve.report();
    assert!(!report.covers_20_20k);
    assert_eq!(report.at_20hz.source, CoverageSource::HeldLow);
    assert_eq!(report.at_20hz.value, Some(1.0));
    assert_eq!(report.at_20khz.source, CoverageSource::HeldHigh);
    assert_eq!(report.at_20khz.value, Some(-1.0));
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::EdgeHeld));
    let sample = curve.sample(10.0).expect("hold 采样");
    assert_eq!(sample.mag_source, CoverageSource::HeldLow);
    assert_approx(sample.mag_db, 1.0, 1e-12, "hold 下端点");
    let sample = curve.sample(22_000.0).expect("hold 采样");
    assert_eq!(sample.mag_source, CoverageSource::HeldHigh);
    // 采样频率非法。
    expect_code(curve.sample(0.0), TargetErrorCode::FreqInvalid);
    expect_code(curve.sample(-20.0), TargetErrorCode::FreqInvalid);
}

#[test]
fn t11_targets_phase_interpolation_and_sample_sources() {
    let csv = csv_phase(&[(20.0, 0.0, 0.0), (20_000.0, 0.0, -180.0)]);
    let curve = import_csv_ok(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
        ],
    );
    // log-f 线性相位：200 Hz 为 −60°。
    assert_approx(
        curve.phase_total_deg_at(200.0).expect("相位").expect("有"),
        -60.0,
        1e-9,
        "200 Hz φ",
    );
    let sample = curve.sample(200.0).expect("采样");
    assert_eq!(sample.phase_source, Some(CoverageSource::Interpolated));
    assert_approx(
        sample.phase_total_deg.expect("相位"),
        -60.0,
        1e-9,
        "sample φ",
    );
    // 无 phase 的目标：phase 字段为 None。
    let plain = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let curve = import_csv_ok(&plain, &[]);
    let sample = curve.sample(1_000.0).expect("采样");
    assert_eq!(sample.phase_total_deg, None);
    assert_eq!(sample.phase_source, None);
    assert_eq!(curve.phase_ex_deg_at(1_000.0).expect("φ_ex"), None);
}

#[test]
fn t11_targets_grid_resolution_warnings() {
    // 2 点整段 20–20 kHz：低频段与中高频段都粗于建议值 → 两条 warning（不拒收）。
    let csv = csv_mag(&[(20.0, 0.0), (25.0, 0.0), (20_000.0, 0.0)]);
    let curve = import_csv_ok(&csv, &[]);
    let warnings = &curve.report().warnings;
    assert!(warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::GridResolutionLow));
    assert!(warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::GridResolutionHigh));
    // 内置 480 点几何网格（1/48 oct）不触发低频段 warning。
    let flat = builtin_flat(&BuiltinOptions::new(48_000.0)).expect("flat");
    assert!(!flat
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::GridResolutionLow));
}

#[test]
fn t11_targets_note_and_duplicate_field_warnings() {
    let csv = csv_mag(&[(20.0, 0.0), (20_000.0, 0.0)]);
    let long_note = "x".repeat(501);
    let text = sidecar_csv(&csv, &[("note", &format!("\"{long_note}\""))]);
    let curve = import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("长 note warning");
    assert!(curve
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::NoteTooLong));
    // 重复顶层字段 → warning（保留第一次出现的值）。
    let mut text = sidecar_csv(&csv, &[]);
    text.pop();
    text.push_str(",\"normalization\":\"peak\"}");
    let curve = import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("重复字段 warning");
    assert_eq!(curve.report().normalization, Normalization::None);
    assert!(curve
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::DuplicateField));
    // 无 phase 时给 unwrap_policy → warning。
    let text = sidecar_csv(&csv, &[("unwrap_policy", "\"auto\"")]);
    let curve = import_csv_bytes(csv.as_bytes(), &text, &opts()).expect("unwrap warning");
    assert!(curve
        .report()
        .warnings
        .iter()
        .any(|warning| warning.code == TargetWarningCode::PhaseMetaIgnored));
}

// ---------------------------------------------------------------------------
// 内置目标（§6.8）：flat 默认；tilt10 = 20 Hz 0 → 20 kHz −10 dB；旧 −3 dB@16k superseded
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_flat_builtin() {
    let flat = builtin_flat(&BuiltinOptions::new(48_000.0)).expect("flat");
    let report = flat.report();
    assert_eq!(report.form, TargetForm::Builtin);
    assert_eq!(report.point_count, BUILTIN_GRID_POINTS);
    assert_eq!(flat.raw_points()[0].freq_hz, 20.0);
    assert_eq!(flat.raw_points()[BUILTIN_GRID_POINTS - 1].freq_hz, 20_000.0);
    for (index, mag) in flat.final_mag_db().iter().enumerate() {
        assert_approx(*mag, 0.0, 1e-12, &format!("flat mag[{index}]"));
    }
    assert!(report.covers_20_20k);
    assert_eq!(report.normalization, Normalization::None);
    assert_eq!(report.reference_gain_db, 0.0);
    assert_eq!(flat.target_sha256(), flat.target_sha256());
    // 采样任意频率都是 0 dB。
    assert_approx(
        flat.mag_db_at(123.0).expect("采样"),
        0.0,
        1e-12,
        "flat 123 Hz",
    );
    assert_approx(
        flat.mag_db_at(15_000.0).expect("采样"),
        0.0,
        1e-12,
        "flat 15k",
    );
}

#[test]
fn t11_targets_tilt10_reference_values() {
    let tilt = builtin_tilt10(&BuiltinOptions::new(48_000.0)).expect("tilt10");
    let at = |freq: f64| tilt.mag_db_at(freq).expect("采样");
    // §6.8 参考值（20 Hz 0；100 Hz −2.329900；1 kHz −5.663233；10 kHz −8.996567；20 kHz −10）。
    assert_approx(at(20.0), 0.0, 1e-12, "20 Hz");
    assert_approx(at(100.0), -2.329900, 1e-6, "100 Hz");
    assert_approx(at(1_000.0), -5.663233, 1e-6, "1 kHz");
    assert_approx(at(10_000.0), -8.996567, 1e-6, "10 kHz");
    assert_approx(at(20_000.0), -10.0, 1e-12, "20 kHz");
    // 单调下降（log-f 上线性）。
    let grid = tilt.frequencies_hz();
    let values = tilt.final_mag_db();
    for index in 1..values.len() {
        assert!(
            values[index] < values[index - 1],
            "tilt10 必须单调下降 @ {} Hz",
            grid[index]
        );
    }
    // 20 Hz → 20 kHz 恰好 −10 dB；16 kHz 不是旧预设的 −3 dB。
    assert_approx(at(20.0) - at(20_000.0), 10.0, 1e-12, "总斜率");
    assert_approx(at(16_000.0), -9.677, 0.002, "16 kHz tilt10");
    assert!(
        at(16_000.0) < -9.0,
        "旧 −3 dB@16k 预设已 superseded，tilt10 在 16 kHz 约 −9.68 dB"
    );
}

#[test]
fn t11_targets_builtin_fs_and_reference_gain() {
    // 44.1 kHz：20 kHz < fs/2=22050 → 通过。
    builtin_tilt10(&BuiltinOptions::new(44_100.0)).expect("44.1k");
    // 32 kHz：20 kHz > fs/2=16000 → E_TGT_FREQ_RANGE。
    expect_code(
        builtin_tilt10(&BuiltinOptions::new(32_000.0)),
        TargetErrorCode::FreqRange,
    );
    // 非法 fs。
    expect_code(
        builtin_flat(&BuiltinOptions::new(0.0)),
        TargetErrorCode::Rate,
    );
    // reference gain 独立：形状不变、整体 +3 dB。
    let tilt = builtin_tilt10(&BuiltinOptions::new(48_000.0).with_reference_gain_db(3.0))
        .expect("tilt10 +3");
    let report = tilt.report();
    assert_eq!(report.normalization, Normalization::None);
    assert_eq!(report.reference_gain_db, 3.0);
    assert_approx(report.applied_offset_db, 3.0, 1e-12, "applied");
    assert_approx(tilt.mag_db_at(20.0).expect("采样"), 3.0, 1e-12, "20 Hz +3");
    assert_approx(
        tilt.mag_db_at(1_000.0).expect("采样"),
        -2.663233,
        1e-6,
        "1 kHz +3",
    );
    assert_approx(
        tilt.mag_shape_db_at(1_000.0).expect("形状"),
        -5.663233,
        1e-6,
        "形状不含 ref gain",
    );
}

#[test]
fn t11_targets_builtin_default_and_superseded() {
    assert_eq!(BuiltinTarget::DEFAULT, BuiltinTarget::Flat);
    assert_eq!(BuiltinTarget::ALL[0].id(), "flat");
    assert_eq!(BuiltinTarget::ALL[1].id(), "tilt10");
    assert_eq!(BuiltinTarget::from_id("flat"), Some(BuiltinTarget::Flat));
    assert_eq!(
        BuiltinTarget::from_id("tilt10"),
        Some(BuiltinTarget::Tilt10)
    );
    // 旧 tilt / superseded id 不是内置目标。
    assert_eq!(BuiltinTarget::from_id("tilt"), None);
    assert_eq!(BuiltinTarget::from_id(SUPERSEDED_LEGACY_TILT_ID), None);
    assert!(!SUPERSEDED_LEGACY_TILT_NOTE.is_empty());
    let default_curve =
        builtin_target(BuiltinTarget::DEFAULT, &BuiltinOptions::new(48_000.0)).expect("默认目标");
    assert!(default_curve
        .final_mag_db()
        .iter()
        .all(|mag| mag.abs() <= 1e-12));
    // 内置目标 note 记录 superseded 说明。
    assert!(default_curve
        .report()
        .notes
        .iter()
        .any(|note| note.contains("superseded")));
}

// ---------------------------------------------------------------------------
// 非模态 solver 读取接口
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_nonmodal_solver_interface() {
    let tilt = builtin_tilt10(&BuiltinOptions::new(48_000.0)).expect("tilt10");
    let source: &dyn NonModalTargetSource = &tilt;
    assert_eq!(source.target_fs(), 48_000.0);
    assert_eq!(source.target_point_count(), BUILTIN_GRID_POINTS);
    assert!(!source.has_phase_target());
    assert_approx(
        source.mag_db_at(1_000.0).expect("采样"),
        -5.663233,
        1e-6,
        "trait 采样",
    );
    // 原生网格 + 网格采样。
    let grid = source.native_grid_hz();
    assert_eq!(grid.len(), BUILTIN_GRID_POINTS);
    let samples = sample_target_grid(source, &grid[..3]).expect("网格采样");
    assert_eq!(samples.len(), 3);
    assert_approx(samples[0].mag_db, 0.0, 1e-12, "20 Hz");
    assert_eq!(samples[0].mag_source, CoverageSource::GridPoint(0));
    // 相位族接口：无 phase 返回 None。
    assert_eq!(source.phase_total_deg_at(1_000.0).expect("相位"), None);
    // 设计网格 helper：端点精确、几何步长。
    let design = log_frequency_grid(20.0, 20_000.0, 480);
    assert_eq!(design.len(), 480);
    assert_eq!(design[0], 20.0);
    assert_eq!(design[479], 20_000.0);
    assert_approx(
        (design[1] / design[0]).log2(),
        9.965784 / 479.0,
        1e-9,
        "octave 步长",
    );
    // 幅度点表（T64/T55 消费）与最终幅度一致。
    let curve = tilt.magnitude_curve();
    assert_eq!(curve.len(), BUILTIN_GRID_POINTS);
    assert_eq!(tilt.modal_peq_curve_points(), curve);
}

// ---------------------------------------------------------------------------
// 报告 JSON / 确定性
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_report_json_deterministic() {
    let csv = csv_phase(&[
        (20.0, 0.0, 0.0),
        (1_000.0, 0.0, -45.0),
        (20_000.0, 0.0, -90.0),
    ]);
    let curve = import_csv_ok(
        &csv,
        &[
            ("phase_unit", "\"deg\""),
            ("phase_ref", "\"absolute\""),
            ("unwrap_policy", "\"unwrapped\""),
            ("reference_gain_db", "1.5"),
        ],
    );
    let first = curve.to_json_report();
    let second = curve.to_json_report();
    assert_eq!(first, second, "报告必须确定性");
    assert!(first.starts_with('{') && first.ends_with('}'));
    for key in [
        "\"schema\":\"khaen-target-report-draft/v1\"",
        "\"form\":\"csv\"",
        "\"point_count\":3",
        "\"phase_target\":\"absolute\"",
        "\"canonical_points_sha256\"",
        "\"at_20hz\"",
        "\"at_20khz\"",
        "\"warnings\"",
    ] {
        assert!(first.contains(key), "报告缺少 {key}: {first}");
    }
    // 报告中的 canonical hash 与 target_sha256 一致。
    assert!(first.contains(curve.target_sha256()));
}

#[test]
fn t11_targets_json_load_from_file() {
    let dir = temp_case_dir("json-load");
    let doc = json_doc(&json_points_mag(&[(20.0, 0.0), (20_000.0, -10.0)]), &[]);
    let path = dir.join("target.json");
    std::fs::write(&path, &doc).expect("写 JSON");
    let curve = load_json_file(&path, &opts()).expect("JSON 文件导入");
    assert_eq!(curve.report().form, TargetForm::Json);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 证据样例输出（--nocapture 时打印，供 evidence/T11 从真实测试输出提取）
// ---------------------------------------------------------------------------

#[test]
fn t11_targets_evidence_samples() {
    let flat = builtin_flat(&BuiltinOptions::new(48_000.0)).expect("flat");
    println!("## T11-SAMPLE flat-report begin");
    println!("{}", flat.to_json_report());
    println!("## T11-SAMPLE flat-report end");
    let tilt = builtin_tilt10(&BuiltinOptions::new(48_000.0)).expect("tilt10");
    println!("## T11-SAMPLE tilt10-report begin");
    println!("{}", tilt.to_json_report());
    println!("## T11-SAMPLE tilt10-report end");
    println!(
        "## T11-SAMPLE flat-sha256 {} points {}",
        flat.target_sha256(),
        flat.report().point_count
    );
    println!(
        "## T11-SAMPLE tilt10-sha256 {} points {}",
        tilt.target_sha256(),
        tilt.report().point_count
    );
    for freq in [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0] {
        println!(
            "## T11-SAMPLE tilt10-point f={freq} mag@={:.12}",
            tilt.mag_db_at(freq).expect("采样")
        );
    }
    // superseded 说明（旧 −3 dB@16k tilt 不是内置目标）。
    println!(
        "## T11-SAMPLE superseded-note {}",
        SUPERSEDED_LEGACY_TILT_NOTE
    );
}
