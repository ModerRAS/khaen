//! audio_profile 单元测试：模型默认值/校验/下混/fanout、存储两阶段、profile -> plan。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use khaen_core::plan::Plan;
use khaen_host::EngineConfig;
use serde_json::{json, Value};

use super::model::{
    AudioProfile, DownmixMode, FanoutMode, ProfileDiff, SourceKind, SpeakerRole, Validation,
    DEVICE_CHANNELS,
};
use super::store::{AudioProfileStore, CurrentProfile, DraftTokenError, PREPARE_TTL_SECS};

/// 临时目录（Drop 时递归删除）。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "khaen-audio-profile-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn default_profile() -> AudioProfile {
    AudioProfile::default()
}

fn profile_with(rows: Vec<Value>, inputs: Value) -> AudioProfile {
    AudioProfile::parse(&json!({
        "outputs": rows,
        "inputs": inputs,
    }))
    .expect("parse profile")
}

fn two_way() -> Value {
    json!([
        { "dac": 0, "enabled": true, "role": "L" },
        { "dac": 1, "enabled": true, "role": "R" }
    ])
}

fn two_way_rows() -> Vec<Value> {
    two_way().as_array().cloned().unwrap_or_default()
}

fn code_set(validation: &Validation) -> Vec<&'static str> {
    validation.errors.iter().map(|issue| issue.code).collect()
}

// ---------------------------------------------------------------------------
// 模型默认值
// ---------------------------------------------------------------------------

#[test]
fn default_profile_is_the_documented_2_0_layout() {
    let profile = default_profile();
    assert_eq!(profile.device_channels, DEVICE_CHANNELS);
    assert_eq!(profile.outputs.len(), 22);
    for dac in 0..22_u16 {
        let row = &profile.outputs[dac as usize];
        assert_eq!(row.dac, dac);
        let expected_enabled = dac < 2;
        assert_eq!(row.enabled, expected_enabled, "dac {dac}");
        let expected_role = match dac {
            0 => SpeakerRole::L,
            1 => SpeakerRole::R,
            _ => SpeakerRole::None,
        };
        assert_eq!(row.role, expected_role, "dac {dac}");
        assert_eq!(row.gain_db, 0.0);
        assert_eq!(row.custom_label, None);
    }
    assert_eq!(profile.sources.len(), 2);
    let aes67 = &profile.sources[0];
    assert_eq!(aes67.kind, SourceKind::Aes67);
    assert!(aes67.enabled);
    assert_eq!(aes67.preset.as_deref(), Some("stereo"));
    assert_eq!(aes67.channels[0].channel, 0);
    assert_eq!(aes67.channels[0].role, SpeakerRole::L);
    assert_eq!(aes67.channels[1].role, SpeakerRole::R);
    let device = &profile.sources[1];
    assert_eq!(device.kind, SourceKind::Device);
    assert!(!device.enabled);
    assert_eq!(profile.downmix.center.mode, DownmixMode::Auto);
    assert_eq!(profile.downmix.center.gain_db, -3.0);
    assert_eq!(profile.downmix.lfe.mode, DownmixMode::Auto);
    assert_eq!(profile.downmix.lfe.gain_db, -3.0);
    assert!(!profile.downmix.lfe.include_heights);
    assert_eq!(profile.fanout, FanoutMode::Unity);
    assert!(profile.validate().ok());
    let lanes = profile.enabled_lanes();
    assert_eq!(lanes.len(), 2);
    assert_eq!(
        (lanes[0].lane, lanes[0].dac, lanes[0].role),
        (0, 0, SpeakerRole::L)
    );
    assert_eq!(
        (lanes[1].lane, lanes[1].dac, lanes[1].role),
        (1, 1, SpeakerRole::R)
    );
}

#[test]
fn json_roundtrip_is_stable() {
    let profile = default_profile();
    let reparsed = AudioProfile::parse(&profile.to_json()).expect("roundtrip");
    assert_eq!(profile, reparsed);
    let body = profile.to_json();
    assert_eq!(body["schema"], json!("khaen-audio-profile/v1"));
    assert_eq!(body["device"]["channels"], json!(22));
    assert_eq!(body["outputs"].as_array().expect("outputs").len(), 22);
    assert_eq!(body["inputs"]["sources"][0]["kind"], json!("aes67"));
    assert_eq!(body["fanout"]["mode"], json!("unity"));
}

#[test]
fn parse_fills_missing_rows_and_keeps_gap_rows_disabled() {
    let profile = AudioProfile::parse(&json!({
        "outputs": [
            { "dac": 0, "enabled": true, "role": "L" },
            { "dac": 2, "enabled": true, "role": "C" }
        ]
    }))
    .expect("parse sparse outputs");
    assert_eq!(profile.outputs.len(), 22);
    assert!(!profile.outputs[1].enabled);
    assert_eq!(profile.outputs[1].role, SpeakerRole::None);
    assert!(profile.outputs[2].enabled);
    assert_eq!(profile.outputs[2].role, SpeakerRole::C);
}

#[test]
fn disabled_rows_are_normalised_to_none() {
    let profile = AudioProfile::parse(&json!({
        "outputs": [
            { "dac": 0, "enabled": true, "role": "L" },
            { "dac": 1, "enabled": false, "role": "R" }
        ]
    }))
    .expect("parse");
    assert_eq!(profile.outputs[1].role, SpeakerRole::None);
    assert_eq!(profile.enabled_lanes().len(), 1);
}

// ---------------------------------------------------------------------------
// 校验：4 类结构性错误 + warning
// ---------------------------------------------------------------------------

#[test]
fn enabled_output_without_role_is_a_structural_error() {
    let profile = profile_with(
        vec![
            json!({ "dac": 0, "enabled": true, "role": "None" }),
            json!({ "dac": 1, "enabled": true, "role": "L" }),
        ],
        json!({}),
    );
    let validation = profile.validate();
    assert!(!validation.ok());
    assert_eq!(code_set(&validation), vec!["enabled_without_role"]);
    assert_eq!(validation.errors[0].path, "outputs[0]");
}

#[test]
fn dac_out_of_range_is_a_structural_error() {
    let profile = profile_with(
        vec![
            json!({ "dac": 0, "enabled": true, "role": "L" }),
            json!({ "dac": 99, "enabled": true, "role": "R" }),
        ],
        json!({}),
    );
    let validation = profile.validate();
    assert!(!validation.ok());
    assert!(validation
        .errors
        .iter()
        .any(|issue| issue.code == "dac_out_of_range" && issue.path == "outputs[99].dac"));
}

#[test]
fn all_outputs_disabled_is_a_structural_error() {
    let mut value = default_profile().to_json();
    for row in value["outputs"].as_array_mut().expect("outputs") {
        row["enabled"] = json!(false);
        row["role"] = json!("None");
    }
    let validation = AudioProfile::parse(&value).expect("parse").validate();
    assert!(!validation.ok());
    assert_eq!(code_set(&validation), vec!["no_enabled_outputs"]);
}

#[test]
fn custom_without_label_is_a_structural_error() {
    let profile = profile_with(
        vec![
            json!({ "dac": 0, "enabled": true, "role": "L" }),
            json!({ "dac": 1, "enabled": true, "role": "Custom" }),
        ],
        json!({}),
    );
    let validation = profile.validate();
    assert!(!validation.ok());
    assert!(validation
        .errors
        .iter()
        .any(|issue| issue.code == "custom_without_label"));

    let labelled = profile_with(
        vec![
            json!({ "dac": 0, "enabled": true, "role": "L" }),
            json!({ "dac": 1, "enabled": true, "role": "Custom", "custom_label": "Rear helper" }),
        ],
        json!({}),
    );
    assert!(labelled.validate().ok());
}

#[test]
fn duplicate_roles_are_valid_and_ordered_by_role_then_dac() {
    let profile = profile_with(
        vec![
            json!({ "dac": 2, "enabled": true, "role": "L" }),
            json!({ "dac": 0, "enabled": true, "role": "L" }),
            json!({ "dac": 1, "enabled": true, "role": "R" }),
        ],
        json!({}),
    );
    let validation = profile.validate();
    assert!(validation.ok());
    assert!(!validation
        .warnings
        .iter()
        .any(|issue| issue.code.contains("duplicate")));
    let lanes = profile.enabled_lanes();
    assert_eq!(
        lanes.iter().map(|lane| lane.dac).collect::<Vec<_>>(),
        vec![0, 2, 1]
    );
    assert_eq!(
        lanes.iter().map(|lane| lane.lane).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn missing_center_and_lfe_only_produce_warnings() {
    let profile = profile_with(
        two_way_rows(),
        json!({
            "sources": [{
                "kind": "aes67",
                "enabled": true,
                "channels": [
                    { "channel": 0, "role": "L" },
                    { "channel": 1, "role": "R" },
                    { "channel": 2, "role": "C" },
                    { "channel": 3, "role": "LFE" }
                ]
            }]
        }),
    );
    let validation = profile.validate();
    assert!(validation.ok(), "缺少角色不能报错: {:?}", validation.errors);
    assert!(validation
        .warnings
        .iter()
        .any(|issue| issue.code == "downmix_center"));
    assert!(validation
        .warnings
        .iter()
        .any(|issue| issue.code == "downmix_lfe"));
    assert!(validation
        .warnings
        .iter()
        .all(|issue| issue.code != "unmapped_center"));
}

#[test]
fn downmix_disabled_reports_silent_inputs_as_warnings() {
    let profile = AudioProfile::parse(&json!({
        "outputs": two_way(),
        "inputs": {
            "sources": [{
                "kind": "aes67",
                "enabled": true,
                "channels": [
                    { "channel": 0, "role": "L" },
                    { "channel": 1, "role": "R" },
                    { "channel": 2, "role": "C" },
                    { "channel": 3, "role": "LFE" }
                ]
            }]
        },
        "downmix": {
            "center": { "mode": "none" },
            "lfe": { "mode": "none" }
        }
    }))
    .expect("parse");
    let validation = profile.validate();
    assert!(validation.ok());
    assert!(validation
        .warnings
        .iter()
        .any(|issue| issue.code == "unmapped_center"));
    assert!(validation
        .warnings
        .iter()
        .any(|issue| issue.code == "unmapped_lfe"));
}

#[test]
fn repeated_input_roles_are_allowed_and_summed() {
    let profile = AudioProfile::parse(&json!({
        "outputs": two_way(),
        "inputs": {
            "sources": [{
                "kind": "aes67",
                "enabled": true,
                "channels": [
                    { "channel": 0, "role": "L" },
                    { "channel": 1, "role": "L" }
                ]
            }]
        }
    }))
    .expect("parse");
    assert!(profile.validate().ok());
    assert_eq!(profile.input_count(&[SpeakerRole::L]), 2);
}

#[test]
fn four_sections_diff_is_reported() {
    let base = default_profile();
    let mut next = base.clone();
    assert_eq!(base.diff(&next), ProfileDiff::default());
    next.outputs[2].enabled = true;
    next.outputs[2].role = SpeakerRole::C;
    assert!(base.diff(&next).outputs_changed);
    next.fanout = FanoutMode::Group;
    assert!(base.diff(&next).fanout_changed);
    next.downmix.center.gain_db = -6.0;
    assert!(base.diff(&next).downmix_changed);
    next.sources[1].enabled = true;
    assert!(base.diff(&next).inputs_changed);
}

#[test]
fn fanout_gain_is_unity_or_one_over_sqrt_n() {
    assert_eq!(FanoutMode::Unity.lane_gain(8), 1.0);
    assert!((FanoutMode::Group.lane_gain(4) - 0.5).abs() < 1e-6);
    assert!((FanoutMode::Group.lane_gain(1) - 1.0).abs() < 1e-6);
    assert_eq!(FanoutMode::Group.lane_gain(0), 1.0);
}

#[test]
fn parse_rejects_request_shape_errors() {
    assert!(AudioProfile::parse(&json!([])).is_err());
    let error =
        AudioProfile::parse(&json!({ "schema": "khaen-audio-profile/v2" })).expect_err("schema");
    assert_eq!(error.field, "profile.schema");
    let error = AudioProfile::parse(&json!({
        "outputs": [
            { "dac": 0, "enabled": true, "role": "L" },
            { "dac": 0, "enabled": true, "role": "R" }
        ]
    }))
    .expect_err("duplicate dac");
    assert!(error.message.contains("duplicate DAC"), "{error}");
    let error = AudioProfile::parse(&json!({
        "outputs": [{ "dac": 0, "enabled": true, "role": "Left" }]
    }))
    .expect_err("bad role");
    assert!(error.field.contains("role"), "{error}");
    // 设备通道数不再是固定 22：1..=22 都接受，0/23 仍拒绝（plan ABI v3 上限）。
    let parsed = AudioProfile::parse(&json!({ "device": { "channels": 8 } })).expect("8 channels");
    assert_eq!(parsed.device_channels, 8);
    assert_eq!(parsed.outputs.len(), 8);
    assert_eq!(parsed.sources[0].channels.len(), 2);
    for channels in [0_u64, 23] {
        let error = AudioProfile::parse(&json!({ "device": { "channels": channels } }))
            .expect_err("device channel count outside 1..=22");
        assert_eq!(error.field, "profile.device.channels");
    }
    let error = AudioProfile::parse(&json!({
        "device": { "channels": 8 },
        "inputs": { "sources": [{
            "kind": "device",
            "enabled": true,
            "channels": [{ "channel": 8, "role": "L" }]
        }] }
    }))
    .expect_err("enabled input channel out of range");
    assert!(error.message.contains("outside"), "{error}");
    // 禁用源不参与 plan：其通道标签可以暂时超出 inputs.channels（启用时再收敛）。
    let parsed = AudioProfile::parse(&json!({
        "device": { "channels": 8 },
        "inputs": { "sources": [{
            "kind": "device",
            "enabled": false,
            "channels": [{ "channel": 8, "role": "L" }]
        }] }
    }))
    .expect("disabled source channels may exceed the input width");
    assert_eq!(parsed.sources[1].channels[0].channel, 8);
}

// ---------------------------------------------------------------------------
// 存储：两阶段 prepare/commit/abort
// ---------------------------------------------------------------------------

#[test]
fn first_run_get_defaults_to_2_0_without_writing_a_file() {
    let dir = TempDir::new("store-default");
    let store = AudioProfileStore::new(dir.path());
    match store.current() {
        CurrentProfile::Default { revision } => assert_eq!(revision, 0),
        other => panic!("expected default: {other:?}"),
    }
    assert!(!store.file_path().exists(), "GET 默认值不能写盘");
}

#[test]
fn prepare_commit_abort_roundtrip_persists_and_bumps_revision() {
    let dir = TempDir::new("store-commit");
    let store = AudioProfileStore::new(dir.path());
    let profile = default_profile();
    let now = Instant::now();
    let draft = store.put_draft(
        profile.clone(),
        ProfileDiff::default(),
        now,
        "2026-11-26T00:00:00Z",
    );
    assert_eq!(draft.token.len(), 16);
    assert!(draft.token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(draft.expires_in_s, PREPARE_TTL_SECS);
    assert!(!store.file_path().exists(), "prepare 不能写盘");

    let saved = store
        .save(profile.clone(), "2026-11-26T00:00:01Z")
        .expect("save");
    assert_eq!(saved.revision, 1);
    assert!(store.file_path().exists());
    match store.current() {
        CurrentProfile::Saved(saved) => {
            assert_eq!(saved.profile, profile);
            assert_eq!(saved.revision, 1);
        }
        other => panic!("expected saved: {other:?}"),
    }
    // 再保存一次 revision+1。
    let saved = store.save(profile, "2026-11-26T00:00:02Z").expect("save");
    assert_eq!(saved.revision, 2);
}

#[test]
fn draft_token_is_opaque_and_consumed_once() {
    let dir = TempDir::new("store-token");
    let store = AudioProfileStore::new(dir.path());
    let now = Instant::now();
    let draft = store.put_draft(default_profile(), ProfileDiff::default(), now, "t0");
    // token 不符。
    assert_eq!(
        store
            .take_draft("0000000000000000", now)
            .expect_err("mismatch"),
        DraftTokenError::Mismatch
    );
    // 正确 token 一次消费。
    let taken = store.take_draft(&draft.token, now).expect("take");
    assert_eq!(taken.token, draft.token);
    assert_eq!(
        store.take_draft(&draft.token, now).expect_err("consumed"),
        DraftTokenError::NoDraft
    );
}

#[test]
fn expired_draft_is_rejected() {
    let dir = TempDir::new("store-expired");
    let store = AudioProfileStore::new(dir.path());
    let now = Instant::now();
    let draft = store.put_draft(default_profile(), ProfileDiff::default(), now, "t0");
    let later = now + Duration::from_secs(PREPARE_TTL_SECS + 1);
    assert_eq!(
        store.take_draft(&draft.token, later).expect_err("expired"),
        DraftTokenError::Expired
    );
}

#[test]
fn corrupt_saved_file_is_reported_as_unreadable() {
    let dir = TempDir::new("store-corrupt");
    let store = AudioProfileStore::new(dir.path());
    std::fs::write(store.file_path(), b"{ not json").expect("write corrupt file");
    match store.current() {
        CurrentProfile::Unreadable { reason } => {
            assert!(reason.contains("not valid JSON"), "{reason}")
        }
        other => panic!("expected unreadable: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// profile -> plan
// ---------------------------------------------------------------------------

fn engine_config() -> EngineConfig {
    EngineConfig::default()
}

fn decode_plan(bytes: &[u8]) -> Plan {
    Plan::decode(bytes).expect("decode plan")
}

#[test]
fn default_profile_generates_a_two_lane_plan_with_full_device_routing() {
    let profile = default_profile();
    let generated = super::plan::generate(&profile, &engine_config()).expect("generate");
    let (bytes, lane_count, preview, downmix) = (
        generated.bytes,
        generated.lane_count,
        generated.folds_preview,
        generated.downmix_applied,
    );
    assert_eq!(lane_count, 2);
    let plan = decode_plan(&bytes);
    let content = plan.content.as_ref().expect("content");
    assert_eq!(content.in_channels, 22);
    assert_eq!(content.out_lanes, 2);
    // 2 条直通 fold（ch0 -> lane0, ch1 -> lane1），增益 1.0。
    assert_eq!(content.folds.len(), 2);
    let routing = plan.routing.as_ref().expect("routing");
    assert_eq!(routing.dac_channels, 22);
    assert_eq!(routing.routes, vec![(0, 0), (1, 1)]);
    let crossover = plan.crossover.as_ref().expect("crossover");
    assert_eq!(crossover.lanes, 2);
    assert_eq!(crossover.filters.len(), 2);
    let matrix = plan.matrix.as_ref().expect("matrix");
    assert_eq!((matrix.rows, matrix.cols), (2, 2));
    let protection = plan.protection.as_ref().expect("protection");
    assert_eq!(protection.lanes, 2);
    assert_eq!(protection.entries.len(), 2);
    assert!(downmix.is_empty());
    assert_eq!(preview.len(), 2);
    assert_eq!(preview[0]["role"], json!("L"));
    assert_eq!(preview[0]["lane"], json!(0));
    assert!(bytes.starts_with(b"KHAENPLN"), "ABI v3 magic");
}

#[test]
fn split_input_output_profile_generates_two_in_twenty_two_out_plan() {
    let profile = AudioProfile::parse(&json!({
        "device": { "channels": 22 },
        "inputs": {
            "channels": 2,
            "sources": [
                {
                    "kind": "aes67",
                    "enabled": true,
                    "channels": [
                        { "channel": 0, "role": "L" },
                        { "channel": 1, "role": "R" },
                    ],
                },
                // 禁用的 device 源保留 4 个通道标签（> inputs.channels=2）：不参与 plan，
                // 因此不应让 profile 失效（启用时 UI 会把通道收敛到输入宽度）。
                {
                    "kind": "device",
                    "enabled": false,
                    "channels": [
                        { "channel": 0, "role": "None" },
                        { "channel": 1, "role": "None" },
                        { "channel": 2, "role": "None" },
                        { "channel": 3, "role": "None" },
                    ],
                },
            ],
        },
        "outputs": [
            { "dac": 0, "enabled": true, "role": "L" },
            { "dac": 1, "enabled": true, "role": "R" },
        ],
    }))
    .expect("2-in / 22-out profile");
    assert_eq!(profile.input_channels, 2);
    assert_eq!(profile.device_channels, 22);
    let generated = super::plan::generate(&profile, &engine_config()).expect("generate");
    let plan = decode_plan(&generated.bytes);
    let content = plan.content.as_ref().expect("content");
    assert_eq!(content.in_channels, 2, "content is 2 channels");
    assert_eq!(content.out_lanes, 2);
    let routing = plan.routing.as_ref().expect("routing");
    assert_eq!(routing.dac_channels, 22, "routing keeps all 22 DAC slots");
    assert_eq!(routing.routes, vec![(0, 0), (1, 1)]);
    // 输入通道 2 不属于 inputs.channels=2：parse 必须拒绝，不得静默截断/重映射。
    assert!(AudioProfile::parse(&json!({
        "device": { "channels": 22 },
        "inputs": {
            "channels": 2,
            "sources": [{
                "kind": "aes67",
                "enabled": true,
                "channels": [{ "channel": 2, "role": "C" }],
            }],
        },
        "outputs": [{ "dac": 0, "enabled": true, "role": "L" }],
    }))
    .is_err());
}

#[test]
fn explicit_device_channels_drive_plan_content_and_routing_widths() {
    let profile = AudioProfile::parse(&json!({
        "device": { "channels": 6 },
        "outputs": [{ "dac": 0, "enabled": true, "role": "L" }],
    }))
    .expect("parse 6ch profile");
    assert_eq!(profile.device_channels, 6);
    assert_eq!(profile.outputs.len(), 6);
    let generated = super::plan::generate(&profile, &engine_config()).expect("generate");
    let plan = decode_plan(&generated.bytes);
    let content = plan.content.as_ref().expect("content");
    assert_eq!(content.in_channels, 6);
    assert_eq!(content.out_lanes, 1);
    let routing = plan.routing.as_ref().expect("routing");
    assert_eq!(routing.dac_channels, 6);
    assert_eq!(routing.routes, vec![(0, 0)]);
}

#[test]
fn missing_center_and_lfe_are_folded_with_the_configured_gains() {
    let profile = AudioProfile::parse(&json!({
        "outputs": two_way(),
        "inputs": {
            "sources": [{
                "kind": "aes67",
                "enabled": true,
                "channels": [
                    { "channel": 0, "role": "L" },
                    { "channel": 1, "role": "R" },
                    { "channel": 2, "role": "C" },
                    { "channel": 3, "role": "LFE" }
                ]
            }]
        }
    }))
    .expect("parse");
    let generated = super::plan::generate(&profile, &engine_config()).expect("generate");
    let (bytes, preview, downmix) = (
        generated.bytes,
        generated.folds_preview,
        generated.downmix_applied,
    );
    let plan = decode_plan(&bytes);
    let content = plan.content.as_ref().expect("content");
    // 2 直通 + C 下混到 2 lane + LFE 下混到 2 lane = 6 条 fold。
    assert_eq!(content.folds.len(), 6, "{:?}", content.folds);
    let center_folds = content.folds.iter().filter(|fold| fold.src == 2).count();
    let lfe_folds = content.folds.iter().filter(|fold| fold.src == 3).count();
    assert_eq!(center_folds, 2);
    assert_eq!(lfe_folds, 2);
    let expected = super::model::db_to_gain(-3.0);
    for fold in &content.folds {
        if fold.src == 2 || fold.src == 3 {
            assert!((fold.gain - expected).abs() < 1e-6, "{}", fold.gain);
        }
    }
    assert_eq!(downmix.len(), 2);
    let kinds = downmix
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind"))
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["center", "lfe"]);
    assert_eq!(downmix[0]["lanes"], json!([0, 1]));
    assert_eq!(downmix[1]["lanes"], json!([0, 1]));
    // 预览包含下混标记。
    assert!(preview
        .iter()
        .any(|entry| entry["downmix"] == json!("center")));
    assert!(preview.iter().any(|entry| entry["downmix"] == json!("lfe")));
}

#[test]
fn group_fanout_normalises_by_sqrt_of_the_role_group() {
    let profile = AudioProfile::parse(&json!({
        "outputs": [
            { "dac": 0, "enabled": true, "role": "L" },
            { "dac": 1, "enabled": true, "role": "L" },
            { "dac": 2, "enabled": true, "role": "L" },
            { "dac": 3, "enabled": true, "role": "L" },
            { "dac": 4, "enabled": true, "role": "R" }
        ],
        "inputs": { "sources": [{
            "kind": "aes67",
            "enabled": true,
            "channels": [{ "channel": 0, "role": "L" }, { "channel": 1, "role": "R" }]
        }] },
        "fanout": { "mode": "group" }
    }))
    .expect("parse");
    let generated = super::plan::generate(&profile, &engine_config()).expect("generate");
    let (bytes, preview) = (generated.bytes, generated.folds_preview);
    let plan = decode_plan(&bytes);
    let folds = &plan.content.as_ref().expect("content").folds;
    let left = folds
        .iter()
        .filter(|fold| fold.src == 0)
        .collect::<Vec<_>>();
    assert_eq!(left.len(), 4);
    for fold in left {
        assert!((fold.gain - 0.5).abs() < 1e-6, "{}", fold.gain);
    }
    let right = folds
        .iter()
        .filter(|fold| fold.src == 1)
        .collect::<Vec<_>>();
    assert_eq!(right.len(), 1);
    assert!((right[0].gain - 1.0).abs() < 1e-6);
    // 预览同样反映 group 增益。
    let preview_gain = preview
        .iter()
        .find(|entry| entry["channel"] == json!(0))
        .and_then(|entry| entry["gain"].as_f64())
        .expect("preview gain");
    assert!((preview_gain - 0.5).abs() < 1e-9, "{preview_gain}");
}

#[test]
fn source_gain_and_lane_gain_stack() {
    let profile = AudioProfile::parse(&json!({
        "outputs": [
            { "dac": 0, "enabled": true, "role": "L" },
            { "dac": 1, "enabled": true, "role": "L" }
        ],
        "inputs": { "sources": [{
            "kind": "aes67",
            "enabled": true,
            "gain_db": -6.0,
            "channels": [{ "channel": 0, "role": "L" }]
        }] }
    }))
    .expect("parse");
    let bytes = super::plan::generate(&profile, &engine_config())
        .expect("generate")
        .bytes;
    let plan = decode_plan(&bytes);
    let folds = &plan.content.as_ref().expect("content").folds;
    assert_eq!(folds.len(), 2);
    let expected = super::model::db_to_gain(-6.0);
    for fold in folds {
        assert!((fold.gain - expected).abs() < 1e-6, "{}", fold.gain);
    }
}

#[test]
fn generated_plan_file_is_content_addressed_and_atomic() {
    let dir = TempDir::new("plan-file");
    let plans_dir = dir.path().join("plans");
    let generated =
        super::plan::generate_and_store(&default_profile(), &engine_config(), &plans_dir)
            .expect("generate and store");
    let path = std::path::Path::new(&generated.plan_path);
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some(format!("{}.plan", generated.sha256).as_str())
    );
    let bytes = std::fs::read(path).expect("read plan");
    assert_eq!(khaen_host::sha256_hex(&bytes), generated.sha256);
    // 目录里没有半截 tmp 文件。
    let leftovers = std::fs::read_dir(&plans_dir)
        .expect("list plans")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn empty_output_profile_refuses_plan_generation() {
    let mut value = default_profile().to_json();
    for row in value["outputs"].as_array_mut().expect("outputs") {
        row["enabled"] = json!(false);
        row["role"] = json!("None");
    }
    let profile = AudioProfile::parse(&value).expect("parse");
    let error = super::plan::generate(&profile, &engine_config()).expect_err("no lanes");
    assert!(
        matches!(error, super::plan::PlanGenerateError::InvalidProfile(_)),
        "{error}"
    );
}
