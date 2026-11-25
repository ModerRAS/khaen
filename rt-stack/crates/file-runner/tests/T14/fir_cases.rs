//! `khaen-fir/v1` manifest 解析/校验/写作（对齐 T55 报告字段面 + fir-toolbox §2.2 规则 1–7）。

use khaen_file_runner::fir::{
    build_fir_manifest, family_constant_delay, family_expected_delay, load_fir_manifest,
    load_fir_manifest_str, magnitude_db_at, manifest_semantic_sha256, parse_fir_manifest,
    validate_fir_manifest, write_fir_manifest, FirManifestSpec, FIR_FAMILIES,
    FIR_REFERENCE_TOLERANCE_DB, FIR_TAPS_MAX, FIR_TAPS_MIN, PROFILE_EXTENDED, PROFILE_OFFLINE_ONLY,
    PROFILE_REGULAR,
};
use khaen_file_runner::json::Json;
use khaen_file_runner::sha256::sha256_hex;
use khaen_file_runner::wav::encode_wav_f32;
use khaen_file_runner::{
    E_FILE_BUDGET, E_FILE_DELAY, E_FILE_FORMAT, E_FILE_HASH, E_FILE_NUMERIC, E_FILE_RATE,
    E_FILE_REF, E_FILE_SCHEMA, E_FILE_TAPS,
};

use crate::common::{
    endpoint_measured, endpoint_na, fir, fir_custom, map_mut, nested_mut, raw_f32le,
};

const HASH_ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn fir_manifest_round_trip_and_semantic_digest() {
    let (manifest_json, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    let text = manifest_json.to_canonical();
    let parsed = parse_fir_manifest(&Json::parse(&text).expect("json")).expect("manifest");
    assert_eq!(parsed.schema, "khaen-fir/v1");
    assert_eq!(parsed.family, "linear");
    assert_eq!(parsed.taps, 3);
    assert_eq!(parsed.fs, 48_000);
    assert_eq!(parsed.delay_samples, 1.0);
    assert_eq!(parsed.coefficients.file, "linear.raw");
    assert_eq!(parsed.coefficients.format, "raw_f32le");
    assert_eq!(parsed.coefficients.bytes, bytes.len() as u64);
    assert!(parsed.limits.fits_budget);
    let loaded = validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR).expect("valid");
    assert_eq!(loaded.coeffs, vec![0.0f32, 1.0, 0.0]);
    assert_eq!(loaded.file_sha256, sha256_hex(&bytes));
    // 语义摘要稳定且不含 created_unix。
    let digest = manifest_semantic_sha256(&parsed);
    assert_eq!(digest.len(), 64);
    assert_eq!(digest, manifest_semantic_sha256(&parsed));
    // writer 端 canonical 与 reader 端一致。
    let spec = spec_of(&parsed);
    assert_eq!(
        write_fir_manifest(&spec).expect("write"),
        manifest_json.to_canonical()
    );
}

fn spec_of(parsed: &khaen_file_runner::fir::FirManifest) -> FirManifestSpec<'_> {
    FirManifestSpec {
        family: &parsed.family,
        fs: parsed.fs,
        taps: parsed.taps,
        delay_samples: parsed.delay_samples,
        tool_name: &parsed.tool_name,
        tool_version: &parsed.tool_version,
        coefficients_file: &parsed.coefficients.file,
        coefficients_format: &parsed.coefficients.format,
        coefficients_bytes: parsed.coefficients.bytes,
        coefficients_sha256: &parsed.coefficients.sha256,
        profile: &parsed.limits.profile,
        route_taps_cap: parsed.limits.route_taps_cap,
        ram_bytes_f32: parsed.limits.ram_bytes_f32,
        fits_budget: parsed.limits.fits_budget,
        reference_hz: 1000.0,
        reference_gain_db: 0.0,
        measured_gain_db: None,
        endpoint_f20: endpoint_measured(20.0, 0.0),
        endpoint_f20k: endpoint_measured(20_000.0, 0.0),
        design: None,
        source: None,
        verification_method: "t14-fixture/analytic",
        warnings: Vec::new(),
    }
}

#[test]
fn fir_rejects_hash_and_size_mismatch() {
    let (mut manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes[..2], PROFILE_REGULAR)
            .expect_err("hash mismatch")
            .code(),
        E_FILE_HASH
    );
    // sha 字段被篡改。
    map_mut(nested_mut(&mut manifest, &["coefficients"]))
        .insert("sha256".to_string(), Json::String(HASH_ZERO.to_string()));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("sha mismatch")
            .code(),
        E_FILE_HASH
    );
}

#[test]
fn fir_rejects_taps_and_ram_mismatch() {
    let (mut manifest, bytes) = fir("min_fast", &[1.0, 0.5], "mf.raw");
    // 只改 taps：先被 ram_bytes_f32 规则拦（预算），再通过 ram 修正后由样本数拦（taps）。
    map_mut(&mut manifest).insert("taps".to_string(), Json::Number(3.0));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("ram mismatch")
            .code(),
        E_FILE_BUDGET
    );
    map_mut(nested_mut(&mut manifest, &["limits"]))
        .insert("ram_bytes_f32".to_string(), Json::Number(12.0));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("taps mismatch")
            .code(),
        E_FILE_TAPS
    );
}

#[test]
fn fir_rejects_non_finite_and_all_zero_coefficients() {
    // NaN 系数：manifest 的 bytes/sha 与 NaN 文件一致，加载后数值闸门拒绝。
    let (mut manifest, _) = fir("min_fast", &[1.0, 1.0], "nan.raw");
    let nan_bytes = raw_f32le(&[f32::NAN, 1.0]);
    let coefficients = map_mut(nested_mut(&mut manifest, &["coefficients"]));
    coefficients.insert("sha256".to_string(), Json::String(sha256_hex(&nan_bytes)));
    coefficients.insert("bytes".to_string(), Json::Number(nan_bytes.len() as f64));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &nan_bytes, PROFILE_REGULAR)
            .expect_err("nan")
            .code(),
        E_FILE_NUMERIC
    );
    // 全零。
    let (mut manifest, _) = fir("min_fast", &[1.0, 1.0], "zero.raw");
    let zero_bytes = raw_f32le(&[0.0, 0.0]);
    let coefficients = map_mut(nested_mut(&mut manifest, &["coefficients"]));
    coefficients.insert("sha256".to_string(), Json::String(sha256_hex(&zero_bytes)));
    coefficients.insert("bytes".to_string(), Json::Number(zero_bytes.len() as f64));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &zero_bytes, PROFILE_REGULAR)
            .expect_err("all zero")
            .code(),
        E_FILE_NUMERIC
    );
}

#[test]
fn fir_enforces_family_delay_rules() {
    // linear taps=3 的合法延迟是 1；声明 0 -> E_FILE_DELAY。
    let (mut manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    map_mut(&mut manifest).insert("delay_samples".to_string(), Json::Number(0.0));
    map_mut(&mut manifest).insert("delay_ms".to_string(), Json::Number(0.0));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("linear delay")
            .code(),
        E_FILE_DELAY
    );
    // nos 必须 h=[1.0] taps=1。
    let (mut manifest, _) = fir("nos", &[1.0], "nos.raw");
    let half = raw_f32le(&[0.5]);
    let coefficients = map_mut(nested_mut(&mut manifest, &["coefficients"]));
    coefficients.insert("sha256".to_string(), Json::String(sha256_hex(&half)));
    coefficients.insert("bytes".to_string(), Json::Number(4.0));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &half, PROFILE_REGULAR)
            .expect_err("nos delta")
            .code(),
        E_FILE_NUMERIC
    );
    // 家族延迟规则表。
    assert!(family_constant_delay("linear"));
    assert!(family_constant_delay("brickwall"));
    assert!(!family_constant_delay("min_fast"));
    assert_eq!(family_expected_delay("linear", 7), Some(3.0));
    assert_eq!(family_expected_delay("nos", 1), Some(0.0));
    assert_eq!(family_expected_delay("min_fast", 7), None);
}

#[test]
fn fir_enforces_reference_normalization() {
    let (mut manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    let reference = nested_mut(&mut manifest, &["reference"]);
    map_mut(reference).insert("gain_db".to_string(), Json::Number(6.0));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    let error = validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR).expect_err("ref");
    assert_eq!(error.code(), E_FILE_REF);
    assert!(error.message().contains("reference normalization"));
    // 容差常量口径：0.01 dB。
    assert!((FIR_REFERENCE_TOLERANCE_DB - 0.01).abs() < 1e-12);
    // 参考频率必须在 (0, fs/2)。
    let (mut manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    map_mut(nested_mut(&mut manifest, &["reference"]))
        .insert("hz".to_string(), Json::Number(48_000.0));
    let parsed = parse_fir_manifest(&manifest).expect("manifest");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("hz")
            .code(),
        E_FILE_REF
    );
}

#[test]
fn fir_requires_complete_endpoints_and_passing_verification() {
    let (manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    // 缺 f20 端点。
    let mut broken = manifest.clone();
    let endpoints = nested_mut(&mut broken, &["response", "endpoints"]);
    map_mut(endpoints).remove("f20");
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("endpoint")
            .code(),
        E_FILE_SCHEMA
    );
    // 端点状态非法。
    let mut broken = manifest.clone();
    let endpoints = nested_mut(&mut broken, &["response", "endpoints"]);
    map_mut(endpoints).insert(
        "f20".to_string(),
        Json::from_entries(vec![("status", Json::String("maybe".to_string()))]),
    );
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("status")
            .code(),
        E_FILE_SCHEMA
    );
    // not_applicable_at_fs 需要非空 reason。
    let mut broken = manifest.clone();
    let endpoints = nested_mut(&mut broken, &["response", "endpoints"]);
    map_mut(endpoints).insert("f20k".to_string(), endpoint_na());
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert!(validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR).is_ok());
    // verification.passed=false 拒绝加载。
    let mut broken = manifest.clone();
    map_mut(nested_mut(&mut broken, &["verification"]))
        .insert("passed".to_string(), Json::Bool(false));
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("verification")
            .code(),
        E_FILE_SCHEMA
    );
}

#[test]
fn fir_enforces_budget_and_profile_rules() {
    let (manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    // fits_budget=false 禁止 runtime 加载。
    let mut broken = manifest.clone();
    map_mut(nested_mut(&mut broken, &["limits"]))
        .insert("fits_budget".to_string(), Json::Bool(false));
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("budget")
            .code(),
        E_FILE_BUDGET
    );
    // offline_only 禁止加载。
    let mut broken = manifest.clone();
    map_mut(nested_mut(&mut broken, &["limits"])).insert(
        "profile".to_string(),
        Json::String(PROFILE_OFFLINE_ONLY.to_string()),
    );
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("offline")
            .code(),
        E_FILE_BUDGET
    );
    // extended manifest 不得进入 regular runtime profile。
    let (ext_manifest, ext_bytes) = fir_custom(
        "min_fast",
        &[1.0, 0.0],
        "ext.raw",
        PROFILE_EXTENDED,
        FIR_TAPS_MAX,
    );
    let parsed = parse_fir_manifest(&ext_manifest).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &ext_bytes, PROFILE_REGULAR)
            .expect_err("extended->regular")
            .code(),
        E_FILE_BUDGET
    );
    assert!(validate_fir_manifest(&parsed, &ext_bytes, PROFILE_EXTENDED).is_ok());
    // route_taps_cap < taps。
    let (manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    let mut broken = manifest;
    map_mut(nested_mut(&mut broken, &["limits"]))
        .insert("route_taps_cap".to_string(), Json::Number(2.0));
    let parsed = parse_fir_manifest(&broken).expect("parse ok");
    assert_eq!(
        validate_fir_manifest(&parsed, &bytes, PROFILE_REGULAR)
            .expect_err("cap")
            .code(),
        E_FILE_BUDGET
    );
}

#[test]
fn fir_rejects_unknown_fields_but_keeps_x_extension_warning() {
    let (manifest, _) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    let mut broken = manifest.clone();
    map_mut(&mut broken).insert("bogus".to_string(), Json::Bool(true));
    assert_eq!(
        parse_fir_manifest(&broken).expect_err("unknown").code(),
        E_FILE_SCHEMA
    );
    let mut extended = manifest;
    map_mut(&mut extended).insert("x_vendor".to_string(), Json::String("v".to_string()));
    let parsed = parse_fir_manifest(&extended).expect("x_ allowed");
    assert!(parsed
        .warnings
        .iter()
        .any(|warning| warning.contains("x_ extension")));
}

#[test]
fn fir_build_rejects_invalid_specs() {
    let mut spec = FirManifestSpec {
        family: "linear",
        fs: 48_000,
        taps: 3,
        delay_samples: 1.0,
        tool_name: "t",
        tool_version: "1",
        coefficients_file: "c.raw",
        coefficients_format: "raw_f32le",
        coefficients_bytes: 12,
        coefficients_sha256: HASH_ZERO,
        profile: PROFILE_REGULAR,
        route_taps_cap: 4096,
        ram_bytes_f32: 12,
        fits_budget: true,
        reference_hz: 1000.0,
        reference_gain_db: 0.0,
        measured_gain_db: None,
        endpoint_f20: endpoint_na(),
        endpoint_f20k: endpoint_na(),
        design: None,
        source: None,
        verification_method: "t",
        warnings: Vec::new(),
    };
    // 家族未知。
    let mut bad = spec.clone();
    bad.family = "not_a_family";
    assert_eq!(
        build_fir_manifest(&bad).expect_err("family").code(),
        E_FILE_SCHEMA
    );
    // taps 越界。
    let mut bad = spec.clone();
    bad.taps = 0;
    assert_eq!(
        build_fir_manifest(&bad).expect_err("taps 0").code(),
        E_FILE_TAPS
    );
    let mut bad = spec.clone();
    bad.taps = FIR_TAPS_MAX + 1;
    assert_eq!(
        build_fir_manifest(&bad).expect_err("taps max").code(),
        E_FILE_TAPS
    );
    // sha 非 64 hex。
    let mut bad = spec.clone();
    bad.coefficients_sha256 = "zz";
    assert_eq!(
        build_fir_manifest(&bad).expect_err("sha").code(),
        E_FILE_HASH
    );
    // 负延迟。
    let mut bad = spec.clone();
    bad.delay_samples = -1.0;
    assert_eq!(
        build_fir_manifest(&bad).expect_err("delay").code(),
        E_FILE_DELAY
    );
    // 合法最小 taps。
    spec.taps = FIR_TAPS_MIN;
    assert!(build_fir_manifest(&spec).is_ok());
    // 家族表完整（七家族 + lowdelay）。
    assert_eq!(FIR_FAMILIES.len(), 8);
    assert!(FIR_FAMILIES.contains(&"linear"));
    assert!(FIR_FAMILIES.contains(&"lowdelay"));
}

#[test]
fn fir_magnitude_db_matches_manual_complex_sum() {
    // h = [1, -1]，f = fs/4：H = 1 - j，|H| = sqrt(2) = 3.0103 dB。
    let db = magnitude_db_at(&[1.0, -1.0], 48_000.0, 12_000.0);
    assert!((db - 20.0 * 2.0f64.sqrt().log10()).abs() < 1e-12);
    assert!((db - 3.010_299_956_639_812_7).abs() < 1e-9);
    // 纯延迟 |H| = 1 -> 0 dB。
    assert!(magnitude_db_at(&[0.0, 1.0, 0.0], 48_000.0, 1_000.0).abs() < 1e-12);
    // 近 DC 处 h=[0.5,0.5] -> |H| ~ 1（20 Hz 处 cos(omega/2) 偏差 < 0.001 dB）。
    assert!(magnitude_db_at(&[0.5, 0.5], 48_000.0, 20.0).abs() < 1e-3);
    // 非有限输入 -> NaN。
    assert!(magnitude_db_at(&[1.0], 0.0, 100.0).is_nan());
    assert!(magnitude_db_at(&[1.0], 48_000.0, f64::NAN).is_nan());
}

#[test]
fn fir_coefficient_wav_path_validation() {
    let coeffs = [0.25f32, 0.5, 0.25];
    let wav = encode_wav_f32(&coeffs, 1, 48_000).expect("encode");
    let sha = sha256_hex(&wav);
    let spec = FirManifestSpec {
        family: "linear",
        fs: 48_000,
        taps: 3,
        delay_samples: 1.0,
        tool_name: "t",
        tool_version: "1",
        coefficients_file: "c.wav",
        coefficients_format: "wav_f32_mono",
        coefficients_bytes: wav.len() as u64,
        coefficients_sha256: &sha,
        profile: PROFILE_REGULAR,
        route_taps_cap: 4096,
        ram_bytes_f32: 12,
        fits_budget: true,
        reference_hz: 1000.0,
        reference_gain_db: magnitude_db_at(&coeffs, 48_000.0, 1000.0),
        measured_gain_db: None,
        endpoint_f20: endpoint_na(),
        endpoint_f20k: endpoint_na(),
        design: None,
        source: None,
        verification_method: "t",
        warnings: Vec::new(),
    };
    let manifest = build_fir_manifest(&spec).expect("manifest");
    let parsed = parse_fir_manifest(&manifest).expect("parse");
    let loaded = validate_fir_manifest(&parsed, &wav, PROFILE_REGULAR).expect("load wav");
    assert_eq!(loaded.coeffs, coeffs);
    // 立体声 WAV 拒绝。
    let stereo = encode_wav_f32(&[0.25, 0.25, 0.5, 0.5, 0.25, 0.25], 2, 48_000).expect("stereo");
    let mut bad_manifest = manifest.clone();
    let coefficients = map_mut(nested_mut(&mut bad_manifest, &["coefficients"]));
    coefficients.insert("sha256".to_string(), Json::String(sha256_hex(&stereo)));
    coefficients.insert("bytes".to_string(), Json::Number(stereo.len() as f64));
    let parsed = parse_fir_manifest(&bad_manifest).expect("parse");
    assert_eq!(
        validate_fir_manifest(&parsed, &stereo, PROFILE_REGULAR)
            .expect_err("stereo")
            .code(),
        E_FILE_FORMAT
    );
    // PCM WAV 拒绝。
    let pcm = crate::common::wav_container(1, 16, 1, 48_000, &crate::common::pcm16(&[0, 1, 2]));
    let mut bad_manifest = manifest.clone();
    let coefficients = map_mut(nested_mut(&mut bad_manifest, &["coefficients"]));
    coefficients.insert("sha256".to_string(), Json::String(sha256_hex(&pcm)));
    coefficients.insert("bytes".to_string(), Json::Number(pcm.len() as f64));
    let parsed = parse_fir_manifest(&bad_manifest).expect("parse");
    assert_eq!(
        validate_fir_manifest(&parsed, &pcm, PROFILE_REGULAR)
            .expect_err("pcm coeff")
            .code(),
        E_FILE_FORMAT
    );
    // 采样率不符（manifest fs=96k，WAV 48k）。
    let mut bad_manifest = manifest.clone();
    map_mut(&mut bad_manifest).insert("fs".to_string(), Json::Number(96_000.0));
    map_mut(&mut bad_manifest).insert(
        "delay_ms".to_string(),
        Json::Number(1.0 / 96_000.0 * 1000.0),
    );
    let parsed = parse_fir_manifest(&bad_manifest).expect("parse");
    assert_eq!(
        validate_fir_manifest(&parsed, &wav, PROFILE_REGULAR)
            .expect_err("rate")
            .code(),
        E_FILE_RATE
    );
}

#[test]
fn fir_load_str_entry_point_checks_schema() {
    let (manifest, bytes) = fir("linear", &[0.0, 1.0, 0.0], "linear.raw");
    let text = manifest.to_canonical();
    let loaded = load_fir_manifest_str(&text, &bytes, PROFILE_REGULAR).expect("load");
    assert_eq!(loaded.manifest.taps, 3);
    let mut bad = manifest;
    map_mut(&mut bad).insert(
        "schema".to_string(),
        Json::String("khaen-fir/v2".to_string()),
    );
    assert_eq!(
        parse_fir_manifest(&bad).expect_err("schema").code(),
        E_FILE_SCHEMA
    );
    assert!(load_fir_manifest(&bad, &bytes, PROFILE_REGULAR).is_err());
}
