//! T66v3 单点 artifact 包验收：确定性、不可变写入、可校验、JSON 合法、方案可被 host 接受。
//!
//! 全部离线、平台无关（Windows 本机可跑）；真实设备/ALSA 不在本测试范围（NOT_RUN）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use khaen_calibration::plan_compile::PlanCompileOptions;
use khaen_calibration::single_point_bundle::{
    build_bundle, bundle_paths, design_digest, error, verify_bundle, write_bundle, Provenance,
    BUNDLE_SCHEMA, MANIFEST_FILE_NAME, MANIFEST_SCHEMA, PARAMS_FILE_NAME, REPORT_FILE_NAME,
    REPORT_SCHEMA,
};
use khaen_core::plan::Plan;
use khaen_designer::modal_peq::{BandType, ChannelPeq, DesignMode, ModalBand, PeqDesign};
use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::graph::PlanGraph;
use khaen_host::EngineConfig;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_SIZE: u32 = 128;
const CHANNELS: u16 = 2;

fn band(kind: BandType, freq_hz: f64, q: f64, gain_db: f64) -> ModalBand {
    ModalBand::from_params(kind, freq_hz, q, gain_db, f64::from(SAMPLE_RATE)).expect("band")
}

fn design() -> PeqDesign {
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    let mut left = ChannelPeq::new("L", 0).expect("channel");
    left.push_band(band(BandType::Peaking, 63.0, 4.0, -6.0));
    left.push_band(band(BandType::Peaking, 125.0, 6.0, -3.0));
    let mut right = ChannelPeq::new("R", 1).expect("channel");
    right.push_band(band(BandType::Peaking, 80.0, 5.0, -4.5));
    design.channels.push(left);
    design.channels.push(right);
    design
}

fn options() -> PlanCompileOptions {
    PlanCompileOptions {
        sample_rate: SAMPLE_RATE,
        block_size: BLOCK_SIZE,
        channels: CHANNELS,
        lane_gain_db: vec![0.0, 2.5],
        lane_delay: vec![0, 16],
    }
}

fn host_config() -> EngineConfig {
    EngineConfig {
        block_frames: BLOCK_SIZE as usize,
        sample_rate: SAMPLE_RATE,
        channels: usize::from(CHANNELS),
    }
}

/// 唯一临时目录；Drop 时递归删除。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-single-point-{}-{}-{}",
            std::process::id(),
            tag,
            unique
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    fs::read(dir.join(name)).expect("read artifact")
}

fn parse(bytes: &[u8]) -> serde_json::Value {
    let text = std::str::from_utf8(bytes).expect("utf-8 artifact");
    serde_json::from_str(text).expect("artifact must be valid JSON")
}

#[test]
fn bundle_is_deterministic_and_self_consistent() {
    let first = build_bundle(&design(), &options(), Provenance::Synthetic, None).expect("bundle");
    let second = build_bundle(&design(), &options(), Provenance::Synthetic, None).expect("bundle");
    assert_eq!(first.bundle_sha256, second.bundle_sha256);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(first.files, second.files);

    // manifest 里记录每个文件的 sha256/字节数，且与文件本身一致。
    let manifest = parse(&first.manifest);
    assert_eq!(manifest["schema"], serde_json::json!(MANIFEST_SCHEMA));
    assert_eq!(
        manifest["bundle_sha256"],
        serde_json::json!(first.bundle_sha256)
    );
    let files = manifest["files"].as_array().expect("files");
    assert_eq!(files.len(), 3);
    for file in files {
        let path = file["path"].as_str().expect("path");
        let bundle_file = first.file(path).expect("bundle file");
        assert_eq!(file["sha256"], serde_json::json!(bundle_file.sha256));
        assert_eq!(file["bytes"], serde_json::json!(bundle_file.bytes.len()));
    }

    // 独立重算整包摘要：按路径排序的 "path\tsha256\n" 的 sha256。
    let mut entries: Vec<(&str, &str)> = first
        .files
        .iter()
        .map(|file| (file.path, file.sha256.as_str()))
        .collect();
    entries.sort();
    let mut joined = String::new();
    for (path, sha) in entries {
        joined.push_str(path);
        joined.push('\t');
        joined.push_str(sha);
        joined.push('\n');
    }
    let recomputed = khaen_calibration::job::sha256_hex(joined.as_bytes());
    assert_eq!(recomputed, first.bundle_sha256);

    // report.json 只写已知事实：方案摘要、来源、限制声明。
    let report = parse(&read_file_bytes(&first, REPORT_FILE_NAME));
    assert_eq!(report["schema"], serde_json::json!(REPORT_SCHEMA));
    assert_eq!(report["provenance"], serde_json::json!("SYNTHETIC"));
    assert_eq!(
        report["plan"]["sha256"],
        serde_json::json!(first.compiled.sha256)
    );
    assert_eq!(
        report["plan"]["graph_id"],
        serde_json::json!(first.compiled.graph_id)
    );
    let limits = report["limits"].as_array().expect("limits");
    assert!(limits.iter().any(|entry| entry
        .as_str()
        .is_some_and(|text| text.contains("not asserted"))));
}

fn read_file_bytes(
    bundle: &khaen_calibration::single_point_bundle::PlanBundle,
    path: &str,
) -> Vec<u8> {
    bundle.file(path).expect("file").bytes.clone()
}

#[test]
fn params_json_is_valid_and_carries_options_and_digest() {
    let digest = design_digest(&design());
    let bundle = build_bundle(
        &design(),
        &options(),
        Provenance::Measured,
        Some("ab".repeat(32).as_str()),
    )
    .expect("bundle");
    let params = parse(&read_file_bytes(&bundle, PARAMS_FILE_NAME));
    assert_eq!(params["schema"], serde_json::json!(BUNDLE_SCHEMA));
    assert_eq!(params["provenance"], serde_json::json!("MEASURED"));
    assert_eq!(params["design_digest"], serde_json::json!(digest));
    assert_eq!(
        params["input_artifact_sha256"],
        serde_json::json!("ab".repeat(32))
    );
    assert_eq!(params["options"]["sample_rate"], serde_json::json!(48_000));
    assert_eq!(params["options"]["block_size"], serde_json::json!(128));
    assert_eq!(params["options"]["channels"], serde_json::json!(2));
    assert_eq!(params["options"]["lane_gain_db"][1], serde_json::json!(2.5));
    assert_eq!(params["options"]["lane_delay"][1], serde_json::json!(16));
    assert_eq!(
        params["design"]["channels"]
            .as_array()
            .expect("channels")
            .len(),
        2
    );

    // 无输入证据时如实写 null，不编造。
    let plain = build_bundle(&design(), &options(), Provenance::Unknown, None).expect("bundle");
    let params = parse(&read_file_bytes(&plain, PARAMS_FILE_NAME));
    assert_eq!(params["input_artifact_sha256"], serde_json::Value::Null);
}

#[test]
fn design_digest_ignores_volatile_fields_but_follows_coefficients() {
    let mut first = design();
    first.created_unix = 1;
    first.tool_version = "0.0.1".to_string();
    first.tool_name = "other-tool".to_string();
    first.fit_report_sha256 = Some("cd".repeat(32));
    first.warnings.push("a warning".to_string());
    first.channels[0].name = "left-renamed".to_string();

    let mut second = design();
    second.created_unix = 1_700_000_000;
    second.tool_version = "9.9.9".to_string();
    second.tool_name = "khaen-designer".to_string();
    second.fit_report_sha256 = None;
    second.channels[0].name = "L".to_string();

    assert_eq!(design_digest(&first), design_digest(&second));
    let bundle_first =
        build_bundle(&first, &options(), Provenance::Measured, None).expect("bundle");
    let bundle_second =
        build_bundle(&second, &options(), Provenance::Measured, None).expect("bundle");
    assert_eq!(bundle_first.compiled.bytes, bundle_second.compiled.bytes);
    assert_eq!(bundle_first.bundle_sha256, bundle_second.bundle_sha256);

    // 改系数必须改摘要与方案字节。
    let mut changed = design();
    changed.channels[0].bands[0] = band(BandType::Peaking, 63.0, 4.0, -5.5);
    assert_ne!(design_digest(&changed), design_digest(&design()));
    let bundle_changed =
        build_bundle(&changed, &options(), Provenance::Measured, None).expect("bundle");
    assert_ne!(bundle_changed.compiled.sha256, bundle_first.compiled.sha256);
}

#[test]
fn write_is_idempotent_and_immutable() {
    let dir = TempDir::new("write");
    let bundle = build_bundle(&design(), &options(), Provenance::Synthetic, None).expect("bundle");
    write_bundle(dir.path(), &bundle).expect("first write");
    // 同内容重复写幂等。
    write_bundle(dir.path(), &bundle).expect("idempotent write");

    // 不同内容的包写到同一目录：必须拒绝，且不破坏已有文件。
    let mut changed_options = options();
    changed_options.lane_gain_db = vec![1.0, 2.5];
    let changed = build_bundle(&design(), &changed_options, Provenance::Synthetic, None)
        .expect("changed bundle");
    let err = write_bundle(dir.path(), &changed).expect_err("different content must be rejected");
    assert_eq!(err.code(), error::ARTIFACT_CONFLICT);
    assert_eq!(read(dir.path(), PLAN_ARTIFACT_PATH), bundle.compiled.bytes);
    assert_eq!(
        read(dir.path(), PARAMS_FILE_NAME),
        read_file_bytes(&bundle, PARAMS_FILE_NAME)
    );
}

const PLAN_ARTIFACT_PATH: &str = "plan/plan.bin";

#[test]
fn verify_detects_tampering_and_missing_files() {
    let dir = TempDir::new("verify");
    let bundle = build_bundle(&design(), &options(), Provenance::Synthetic, None).expect("bundle");
    write_bundle(dir.path(), &bundle).expect("write");
    verify_bundle(dir.path(), &bundle).expect("verify clean bundle");

    // 改一个字节 → verify_mismatch。
    let mut plan_bytes = read(dir.path(), PLAN_ARTIFACT_PATH);
    plan_bytes[0] ^= 0x01;
    fs::write(dir.path().join(PLAN_ARTIFACT_PATH), &plan_bytes).expect("tamper");
    let err = verify_bundle(dir.path(), &bundle).expect_err("tamper must be rejected");
    assert_eq!(err.code(), error::VERIFY_MISMATCH);

    // 恢复后删 report → verify_missing。
    fs::write(dir.path().join(PLAN_ARTIFACT_PATH), &bundle.compiled.bytes).expect("restore");
    fs::remove_file(dir.path().join(REPORT_FILE_NAME)).expect("remove report");
    let err = verify_bundle(dir.path(), &bundle).expect_err("missing must be rejected");
    assert_eq!(err.code(), error::VERIFY_MISSING);
}

#[test]
fn bundle_plan_is_accepted_by_host() {
    let bundle = build_bundle(&design(), &options(), Provenance::Synthetic, None).expect("bundle");
    let decoded = Plan::decode(&bundle.compiled.bytes).expect("decode");
    assert_eq!(decoded, bundle.compiled.plan);
    let admission = admit(&decoded, &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());
    PlanGraph::compile(&decoded, &host_config(), &AdmissionLimits::default())
        .expect("host graph compile");
    assert_eq!(
        bundle.file(PLAN_ARTIFACT_PATH).expect("plan file").sha256,
        bundle.compiled.sha256
    );
}

#[test]
fn bundle_paths_are_fixed_and_no_timestamp_is_embedded() {
    let paths = bundle_paths();
    assert_eq!(paths.len(), 4);
    assert!(paths.iter().any(|path| path.ends_with(MANIFEST_FILE_NAME)));
    let bundle = build_bundle(&design(), &options(), Provenance::Synthetic, None).expect("bundle");
    let all = [
        read_file_bytes(&bundle, PARAMS_FILE_NAME),
        read_file_bytes(&bundle, REPORT_FILE_NAME),
        bundle.manifest.clone(),
    ];
    for bytes in all {
        let text = std::str::from_utf8(&bytes).expect("utf-8");
        assert!(
            !text.contains("created_unix") && !text.contains("timestamp"),
            "reproducible bundle must not embed timestamps: {text}"
        );
    }
}
