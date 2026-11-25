//! T25 输入内容检测（S1 兼容资料）+ Aux1/Aux2 双路径 fader 原子绑定私有集成测试。
//!
//! 任务：WBS T25（rt-stack compat leaf content；deps T24/T23/T44 已 PASS）。
//! 写域：crates/compat/tests/T25/。被测模块：crates/compat/src/content.rs。
//!
//! 覆盖（对应 compat-matrix §6 C01-C08、§4 W18/W19、R47；DEC §1/§6）：
//! - C01/C02/C08 字段与 schema：state/override JSON 严格解析、未知值拒收、daemon 未运行壳；
//! - C03 优先级 override > detector > auto；auto 默认直通；
//! - C04 判定只用 C/SL/SR/height，LFE 永远不作判据（H16 旧固件 L/R+LFE 仍判 stereo）；
//! - C05 时间参数：500 ms 采样、hold 2500 ms、迟滞 6 dB、启动冻结 2500 ms；
//! - C06 H16 内部上混推断（multichannel=true / stereo=false / unknown=null）；
//! - C07 P1 显式拒绝：input-mode GET not_supported 壳、PUT not_supported、退役 unit 依赖证据；
//! - W18 GetCaptureSignalRms 读取（长度/有限/-240 floor）与 role 映射；
//! - W19 SetFaderVolume [id, db] 形状/范围与命令 JSON；Aux1/Aux2 0/-120 dB；
//! - 双上混=0 不变量：任何绑定/切换计划/失败回退都不得两路同时有声；
//! - mode 原子绑定：content_mode 与 fader 同值提交，失败进入 both-muted 安全态。
//!
//! Cargo 自动把 tests/T25/main.rs 发现为集成测试 target（与 T13/T24/T57 同机制），
//! 不需要 integrator 注册 public_registration.rs。测试名统一 content_ 前缀。
//! DoD：cargo test -p khaen-compat content

use khaen_compat::content::*;
use khaen_compat::ws::{
    json_encode, json_parse, JsonValue, RMS_FLOOR_DB, VOLUME_MAX_DB, VOLUME_MIN_DB,
};

// ---------------------------------------------------------------------------
// 夹具辅助
// ---------------------------------------------------------------------------

fn config() -> ContentConfig {
    ContentConfig::default()
}

fn detect(pairs: &[(&str, f64)]) -> DetectionResult {
    detect_content(pairs, &config()).expect("detect_content")
}

fn new_daemon() -> ContentDaemon {
    ContentDaemon::new(config()).expect("daemon")
}

fn tick_ok(
    daemon: &mut ContentDaemon,
    now_ms: f64,
    rms: &[(&str, f64)],
    unix: u64,
) -> ContentDaemonTick {
    daemon.tick(now_ms, rms, unix).expect("daemon tick")
}

fn writes_of(plan: &FaderSwitchPlan) -> Vec<(i32, f64)> {
    plan.writes
        .iter()
        .map(|write| (write.fader_id, write.volume_db))
        .collect()
}

fn apply_ok(daemon: &mut ContentDaemon, plan: &FaderSwitchPlan) {
    daemon
        .mark_faders_applied(plan)
        .expect("mark faders applied");
}

fn project_file(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read_lossy(path: &std::path::Path) -> String {
    String::from_utf8_lossy(&std::fs::read(path).expect("read input file")).into_owned()
}

fn json_text(value: &JsonValue) -> String {
    json_encode(value).expect("json encode")
}

// ---------------------------------------------------------------------------
// A. 契约常量 / 枚举 / 错误码 / 与 T24/T57 的只读一致性
// ---------------------------------------------------------------------------

#[test]
fn content_contract_constants_frozen() {
    assert_eq!(INPUT_CONTENT_SCHEMA, "tspi-input-content/v1");
    assert_eq!(INPUT_MODE_SCHEMA, "tspi-input-mode/v1");
    assert_eq!(INPUT_MODE_CHOICES, ["auto", "stereo", "multichannel"]);
    assert_eq!(DEFAULT_THRESHOLD_DB, -50.0);
    assert_eq!(DEFAULT_HYSTERESIS_DB, 6.0);
    assert_eq!(DEFAULT_HOLD_MS, 2500);
    assert_eq!(DEFAULT_STARTUP_FREEZE_MS, 2500);
    assert_eq!(DEFAULT_SAMPLE_INTERVAL_MS, 500);
    assert_eq!(CONTENT_RMS_FLOOR_DB, -240.0);
    assert_eq!(AUX_OPEN_DB, 0.0);
    assert_eq!(AUX_MUTE_DB, -120.0);
    assert_eq!(FADER_AUX_STEREO, 1);
    assert_eq!(FADER_AUX_PASS, 2);
    assert_eq!(AUX_FADER_IDS, [1, 2]);
    assert_eq!(S1_DEFAULT_CONTAINER_LAYOUT, "h16-7.1.4");
    assert_eq!(H16_7_1_4_CHANNELS, 12);
    assert_eq!(H16_7_1_4_ROLE_INDEX.len(), H16_7_1_4_CHANNELS);
    assert_eq!(DAEMON_RUNNING_WINDOW_SECS, 10);
}

#[test]
fn content_p1_policy_constants_frozen() {
    assert!(!P1_CONTENT_UPMMIX_SUPPORTED);
    assert_eq!(P1_NO_CONTENT_UPMMIX_REASON, "p1-no-content-upmix");
    assert_eq!(P1_INPUT_MODE_STATUS, "not_supported");
    assert_eq!(P1_RETIRED_CONTENT_UNIT, "tspi-upmix-autodetect.service");
    assert_eq!(
        P1_RETIRED_CONTENT_UNIT_CAMILLA_DEPS,
        [
            "Wants=tspi-camilladsp.service",
            "After=tspi-camilladsp.service"
        ]
    );
    assert_eq!(P1_FORBIDDEN_CONTENT_FEATURES.len(), 3);
    assert_eq!(CMD_GET_CAPTURE_SIGNAL_RMS, "GetCaptureSignalRms");
    assert_eq!(CMD_SET_FADER_VOLUME, "SetFaderVolume");
}

#[test]
fn content_mode_vocabulary_and_alias_normalization() {
    assert_eq!(ContentMode::ALL.len(), 3);
    assert_eq!(ContentMode::Auto.as_str(), "auto");
    assert_eq!(ContentMode::Stereo.as_str(), "stereo");
    assert_eq!(ContentMode::Multichannel.as_str(), "multichannel");
    assert_eq!(
        ContentMode::from_str_strict("auto"),
        Some(ContentMode::Auto)
    );
    assert_eq!(ContentMode::from_str_strict("force-stereo"), None);
    assert_eq!(ContentMode::from_str_strict("Auto"), None);
    for alias in ["auto", " AUTO ", "自动"] {
        assert_eq!(
            ContentMode::normalize(alias),
            Some(ContentMode::Auto),
            "{alias}"
        );
    }
    for alias in [
        "stereo",
        "force-stereo",
        "2.0",
        "2ch",
        "force-2.0",
        " Force-Stereo ",
    ] {
        assert_eq!(
            ContentMode::normalize(alias),
            Some(ContentMode::Stereo),
            "{alias}"
        );
    }
    for alias in [
        "multichannel",
        "force-multichannel",
        "multi",
        "mc",
        "mch",
        "force-multi",
    ] {
        assert_eq!(
            ContentMode::normalize(alias),
            Some(ContentMode::Multichannel),
            "{alias}"
        );
    }
    assert_eq!(ContentMode::normalize("5.1"), None);
    assert_eq!(ContentMode::normalize(""), None);
    assert!(ContentMode::Stereo.is_forced());
    assert!(!ContentMode::Auto.is_forced());
    assert_eq!(ContentMode::Stereo.s1_path(), AuxPath::StereoUpmix);
    assert_eq!(ContentMode::Auto.s1_path(), AuxPath::PassThrough);
    assert_eq!(ContentMode::Multichannel.s1_path(), AuxPath::PassThrough);
}

#[test]
fn content_detected_vocabulary_and_h16_inference() {
    assert_eq!(DetectedKind::ALL.len(), 3);
    assert_eq!(DetectedKind::Unknown.as_str(), "unknown");
    assert_eq!(
        DetectedKind::from_str_strict("multichannel"),
        Some(DetectedKind::Multichannel)
    );
    assert_eq!(DetectedKind::from_str_strict("Multi"), None);
    assert_eq!(DetectedKind::Multichannel.h16_internal_upmix(), Some(true));
    assert_eq!(DetectedKind::Stereo.h16_internal_upmix(), Some(false));
    assert_eq!(DetectedKind::Unknown.h16_internal_upmix(), None);
    assert_eq!(h16_internal_upmix(DetectedKind::Multichannel), Some(true));
}

#[test]
fn content_detector_and_reason_vocabulary() {
    assert_eq!(ContentDetectorKind::ALL.len(), 3);
    assert_eq!(ContentDetectorKind::Energy.as_str(), "energy");
    assert_eq!(ContentDetectorKind::Manual.as_str(), "manual");
    assert_eq!(ContentDetectorKind::Fallback.as_str(), "fallback");
    assert_eq!(
        ContentDetectorKind::from_str_strict("manual"),
        Some(ContentDetectorKind::Manual)
    );
    assert_eq!(ContentReason::NoData.to_reason_string(), "no-data");
    assert_eq!(ContentReason::NoSignal.to_reason_string(), "no-signal");
    assert_eq!(ContentReason::FlFrOnly.to_reason_string(), "fl-fr-only");
    assert_eq!(
        ContentReason::SecondaryActive.to_reason_string(),
        "secondary-active"
    );
    assert_eq!(
        ContentReason::HysteresisBand.to_reason_string(),
        "hysteresis-band"
    );
    assert_eq!(
        ContentReason::StartupFreeze.to_reason_string(),
        "startup-freeze"
    );
    assert_eq!(
        ContentReason::DaemonNotRunning.to_reason_string(),
        "daemon-not-running"
    );
    assert_eq!(
        ContentReason::Override(ContentMode::Stereo).to_reason_string(),
        "override-stereo"
    );
    assert_eq!(
        ContentReason::Override(ContentMode::Multichannel).to_reason_string(),
        "override-multichannel"
    );
    assert_eq!(ContentReason::WsError.code(), "ws-error");
    assert_eq!(ContentReason::FaderError.code(), "fader-error");
}

#[test]
fn content_config_defaults_and_validation_matrix() {
    let defaults = config();
    assert_eq!(defaults.exit_threshold_db(), -56.0);
    assert!(defaults.validate().is_ok());
    let mut bad = defaults;
    bad.threshold_db = f64::NAN;
    assert_eq!(
        bad.validate().unwrap_err().code(),
        ContentErrorCode::InvalidConfig
    );
    bad = defaults;
    bad.hysteresis_db = -1.0;
    assert!(bad.validate().is_err());
    bad.hysteresis_db = f64::INFINITY;
    assert!(bad.validate().is_err());
    bad = defaults;
    bad.hold_ms = 0;
    assert!(bad.validate().is_err());
    bad.hold_ms = 600_001;
    assert!(bad.validate().is_err());
    bad = defaults;
    bad.startup_freeze_ms = 600_001;
    assert!(bad.validate().is_err());
    bad = defaults;
    bad.sample_interval_ms = 0;
    assert!(bad.validate().is_err());
    bad.sample_interval_ms = 60_001;
    assert!(bad.validate().is_err());
}

#[test]
fn content_error_codes_catalog_and_json() {
    assert_eq!(ContentErrorCode::ALL.len(), 14);
    let mut seen: Vec<&str> = ContentErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), ContentErrorCode::ALL.len());
    for code in ContentErrorCode::ALL {
        assert!(!code.as_str().is_empty());
        assert_eq!(ContentErrorCode::from_str(code.as_str()), Some(*code));
        assert!(!format!("{code}").is_empty());
    }
    let err = ContentError::new(ContentErrorCode::FieldRange, "bad field");
    assert_eq!(err.code(), ContentErrorCode::FieldRange);
    assert_eq!(err.message(), "bad field");
    let json = err.to_json();
    assert_eq!(
        json.get("code").and_then(JsonValue::as_str),
        Some("field_range")
    );
    assert_eq!(
        json.get("message").and_then(JsonValue::as_str),
        Some("bad field")
    );
    assert_eq!(err.to_string(), "field_range: bad field");
}

#[test]
fn content_uses_t24_contract_constants_and_commands() {
    assert_eq!(CONTENT_RMS_FLOOR_DB, RMS_FLOOR_DB);
    assert_eq!(RMS_FLOOR_DB, -240.0);
    assert_eq!(VOLUME_MIN_DB, -150.0);
    assert_eq!(VOLUME_MAX_DB, 50.0);
    assert!(khaen_compat::ws::SUPPORTED_COMMANDS.contains(&CMD_GET_CAPTURE_SIGNAL_RMS));
    assert!(khaen_compat::ws::SUPPORTED_COMMANDS.contains(&CMD_SET_FADER_VOLUME));
    assert!(khaen_compat::ws::is_supported_command(
        "GetCaptureSignalRms"
    ));
    assert!(khaen_compat::ws::is_supported_command("SetFaderVolume"));
}

#[test]
fn content_h16_role_index_matches_overlay_preset() {
    let path = project_file("../../../audio/overlay/etc/tspi-audio/layout-presets.yml");
    assert!(path.exists(), "overlay layout preset missing: {path:?}");
    let text = read_lossy(&path);
    assert!(text.contains("h16-7.1.4"));
    assert_eq!(H16_7_1_4_ROLE_INDEX.len(), 12);
    for (index, (role, channel)) in H16_7_1_4_ROLE_INDEX.iter().enumerate() {
        assert_eq!(*channel, index);
        let stream = if *channel < 8 { 0 } else { 1 };
        let stream_channel = if *channel < 8 { *channel } else { *channel - 8 };
        let id_token = format!("- {{id: {role},");
        let position_token = format!("stream: {stream}, channel: {stream_channel}}}");
        assert!(
            text.lines()
                .any(|line| line.contains(&id_token) && line.contains(&position_token)),
            "role {role} not found with {position_token}"
        );
    }
}

// ---------------------------------------------------------------------------
// B. detect_content 判定（C04/C05）
// ---------------------------------------------------------------------------

#[test]
fn content_detect_fl_fr_only_is_stereo() {
    let result = detect(&[("FL", -20.0), ("FR", -21.0)]);
    assert_eq!(result.detected, DetectedKind::Stereo);
    assert_eq!(result.reason, ContentReason::FlFrOnly);
    assert_eq!(result.rms_min_db, None);
    assert_eq!(result.h16_internal_upmix(), Some(false));
}

#[test]
fn content_detect_lfe_is_never_criterion() {
    // H16 旧固件 2.0 内容会填 L/R + LFE：LFE 有能量仍必须判 stereo。
    let result = detect(&[("FL", -20.0), ("FR", -20.5), ("LFE", -10.0)]);
    assert_eq!(result.detected, DetectedKind::Stereo);
    assert_eq!(result.reason, ContentReason::FlFrOnly);
    assert_eq!(result.rms_min_db, None);
    assert!(result.active_channels.iter().any(|role| role == "LFE"));
}

#[test]
fn content_detect_lfe_alone_is_unknown_no_data() {
    let result = detect(&[("LFE", -10.0)]);
    assert_eq!(result.detected, DetectedKind::Unknown);
    assert_eq!(result.reason, ContentReason::NoData);
    assert_eq!(result.confidence, 0.0);
}

#[test]
fn content_detect_secondary_roles_are_multichannel() {
    for pair in [
        ("C", -20.0),
        ("SL", -25.0),
        ("SR", -25.0),
        ("BL", -30.0),
        ("BR", -30.0),
        ("TFL", -35.0),
        ("HFL", -35.0),
        ("TFR", -35.0),
        ("TRL", -35.0),
        ("TRR", -35.0),
        ("HRL", -35.0),
        ("HRR", -35.0),
    ] {
        let result = detect(&[("FL", -20.0), ("FR", -20.0), pair]);
        assert_eq!(
            result.detected,
            DetectedKind::Multichannel,
            "role {} should be a multichannel criterion",
            pair.0
        );
        assert_eq!(result.reason, ContentReason::SecondaryActive);
        assert_eq!(result.h16_internal_upmix(), Some(true));
    }
}

#[test]
fn content_detect_threshold_is_strictly_greater() {
    let exact = detect(&[("FL", -20.0), ("C", -50.0)]);
    assert_eq!(exact.detected, DetectedKind::Unknown);
    assert_eq!(exact.reason, ContentReason::HysteresisBand);
    let above = detect(&[("FL", -20.0), ("C", -49.9)]);
    assert_eq!(above.detected, DetectedKind::Multichannel);
}

#[test]
fn content_detect_hysteresis_band_is_unknown() {
    // exit = -56：-52 落在迟滞带 → unknown，保持当前状态（由 daemon 层处理）。
    let result = detect(&[("FL", -20.0), ("FR", -20.0), ("C", -52.0)]);
    assert_eq!(result.detected, DetectedKind::Unknown);
    assert_eq!(result.reason, ContentReason::HysteresisBand);
    assert_eq!(result.rms_min_db, Some(-52.0));
}

#[test]
fn content_detect_below_exit_threshold_returns_stereo() {
    let result = detect(&[("FL", -20.0), ("C", -56.1)]);
    assert_eq!(result.detected, DetectedKind::Stereo);
    assert_eq!(result.reason, ContentReason::FlFrOnly);
}

#[test]
fn content_detect_empty_and_all_quiet_are_unknown() {
    let empty = detect(&[]);
    assert_eq!(empty.detected, DetectedKind::Unknown);
    assert_eq!(empty.reason, ContentReason::NoData);
    assert_eq!(empty.active_channels.len(), 0);
    let quiet = detect(&[("FL", -120.0), ("C", -130.0), ("LFE", -140.0)]);
    assert_eq!(quiet.detected, DetectedKind::Unknown);
    assert_eq!(quiet.reason, ContentReason::NoSignal);
    assert_eq!(quiet.active_channels.len(), 0);
}

#[test]
fn content_detect_active_channels_sorted_and_thresholded() {
    // >= -50 dB 才进 active；按 dB 降序，同值按角色名升序。
    let result = detect(&[
        ("FL", -20.0),
        ("FR", -20.0),
        ("C", -30.0),
        ("LFE", -10.0),
        ("SL", -70.0),
    ]);
    assert_eq!(
        result.active_channels,
        vec![
            "LFE".to_string(),
            "FL".to_string(),
            "FR".to_string(),
            "C".to_string()
        ]
    );
    assert_eq!(result.rms_min_db, Some(-30.0));
}

#[test]
fn content_detect_confidence_formulas_match_s1() {
    let multi = detect(&[("FL", -20.0), ("C", -40.0)]);
    // 0.2 + (sec - thr)/20 = 0.2 + 10/20 = 0.70
    assert!(
        (multi.confidence - 0.70).abs() < 1e-9,
        "{}",
        multi.confidence
    );
    let multi_loud = detect(&[("FL", -20.0), ("C", -30.0)]);
    assert!((multi_loud.confidence - 1.0).abs() < 1e-9);
    let stereo_far = detect(&[("FL", -20.0)]);
    // sec = floor(-240)：margin = min(20, -50 - (-240)) = 20 → 0.5 + 20/40 = 1.0
    assert!((stereo_far.confidence - 1.0).abs() < 1e-9);
    // 副声道必须 <= 退出阈值（-56）才会离开迟滞带判 stereo：-58 → margin = 8。
    let stereo_near = detect(&[("FL", -20.0), ("C", -58.0)]);
    assert!(
        (stereo_near.confidence - 0.70).abs() < 1e-9,
        "{}",
        stereo_near.confidence
    );
}

#[test]
fn content_detect_unknown_roles_are_not_criteria() {
    // D-2：未知角色不参与多声道判定，但可作为能量出现在 active_channels。
    let result = detect(&[("FL", -20.0), ("FR", -20.0), ("SUB9", -10.0)]);
    assert_eq!(result.detected, DetectedKind::Stereo);
    assert!(result.active_channels.iter().any(|role| role == "SUB9"));
    let only_unknown = detect(&[("FOO", -10.0)]);
    assert_eq!(only_unknown.detected, DetectedKind::Unknown);
    assert_eq!(only_unknown.reason, ContentReason::NoData);
}

#[test]
fn content_detect_role_names_are_case_sensitive() {
    let result = detect(&[("fl", -20.0), ("FR", -20.0)]);
    assert_eq!(result.detected, DetectedKind::Stereo);
    assert_eq!(
        result.active_channels,
        vec!["FR".to_string(), "fl".to_string()]
    );
}

#[test]
fn content_detect_ignores_nonfinite_values() {
    let result = detect(&[("FL", f64::NAN), ("FR", f64::INFINITY), ("C", -40.0)]);
    assert_eq!(result.detected, DetectedKind::Multichannel);
    assert_eq!(result.rms_min_db, Some(-40.0));
    assert_eq!(result.active_channels, vec!["C".to_string()]);
}

#[test]
fn content_detect_rms_min_db_rounded_to_0_1_db() {
    let result = detect(&[("FL", -20.0), ("C", -40.06)]);
    assert_eq!(result.rms_min_db, Some(-40.1));
}

// ---------------------------------------------------------------------------
// C. GetCaptureSignalRms（W18）与 SetFaderVolume（W19）形态
// ---------------------------------------------------------------------------

#[test]
fn content_parse_get_capture_signal_rms_ok_and_floor() {
    let mut items: Vec<JsonValue> = (0..12)
        .map(|i| JsonValue::Number(-10.0 - i as f64))
        .collect();
    items[3] = JsonValue::Number(-300.0);
    let value = JsonValue::Array(items);
    let parsed = parse_get_capture_signal_rms(&value, 12).expect("rms parse");
    assert_eq!(parsed.len(), 12);
    assert_eq!(parsed[0], -10.0);
    assert_eq!(parsed[3], CONTENT_RMS_FLOOR_DB);
}

#[test]
fn content_parse_get_capture_signal_rms_negative_matrix() {
    assert_eq!(
        parse_get_capture_signal_rms(&JsonValue::Null, 2)
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldType
    );
    let wrong_len = JsonValue::Array(vec![JsonValue::Number(-10.0)]);
    assert_eq!(
        parse_get_capture_signal_rms(&wrong_len, 2)
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldRange
    );
    let bad_type = JsonValue::Array(vec![JsonValue::Number(-10.0), JsonValue::Null]);
    assert_eq!(
        parse_get_capture_signal_rms(&bad_type, 2)
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldType
    );
    let nonfinite = JsonValue::Array(vec![JsonValue::Number(-10.0), JsonValue::Number(f64::NAN)]);
    assert_eq!(
        parse_get_capture_signal_rms(&nonfinite, 2)
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldRange
    );
}

#[test]
fn content_rms_by_role_maps_h16_layout() {
    let capture: Vec<f64> = (0..12).map(|i| -(i as f64) - 10.0).collect();
    let mapped = rms_by_role(&capture, &H16_7_1_4_ROLE_INDEX);
    assert_eq!(mapped.len(), 12);
    for (index, (role, db)) in mapped.iter().enumerate() {
        assert_eq!(*db, capture[index]);
        assert_eq!(*role, H16_7_1_4_ROLE_INDEX[index].0);
    }
    assert_eq!(mapped[0], ("FL", -10.0));
    assert_eq!(mapped[11], ("TRR", -21.0));
}

#[test]
fn content_rms_by_role_missing_and_nonfinite_use_floor() {
    let capture = [-20.0, f64::NAN, -300.0];
    let mapped = rms_by_role(&capture, &[("FL", 0), ("FR", 1), ("C", 2), ("SL", 9)]);
    assert_eq!(mapped[0].1, -20.0);
    assert_eq!(mapped[1].1, CONTENT_RMS_FLOOR_DB);
    assert_eq!(mapped[2].1, CONTENT_RMS_FLOOR_DB);
    assert_eq!(mapped[3].1, CONTENT_RMS_FLOOR_DB);
}

#[test]
fn content_parse_set_fader_volume_ok() {
    let payload = JsonValue::Array(vec![JsonValue::Int(1), JsonValue::Number(-120.0)]);
    assert_eq!(parse_set_fader_volume(&payload), Ok((1, -120.0)));
    let payload = JsonValue::Array(vec![JsonValue::Int(2), JsonValue::Number(0.0)]);
    assert_eq!(parse_set_fader_volume(&payload), Ok((2, 0.0)));
    // S1 接受 JSON 数字形态的整数 id（解析层统一 Number）。
    let payload = JsonValue::Array(vec![JsonValue::Number(1.0), JsonValue::Number(-120.0)]);
    assert_eq!(parse_set_fader_volume(&payload), Ok((1, -120.0)));
}

#[test]
fn content_parse_set_fader_volume_negative_matrix() {
    assert_eq!(
        parse_set_fader_volume(&JsonValue::Null).unwrap_err().code(),
        ContentErrorCode::FieldType
    );
    let one = JsonValue::Array(vec![JsonValue::Int(1)]);
    assert_eq!(
        parse_set_fader_volume(&one).unwrap_err().code(),
        ContentErrorCode::FieldRange
    );
    let three = JsonValue::Array(vec![
        JsonValue::Int(1),
        JsonValue::Number(0.0),
        JsonValue::Number(1.0),
    ]);
    assert_eq!(
        parse_set_fader_volume(&three).unwrap_err().code(),
        ContentErrorCode::FieldRange
    );
    let string_id = JsonValue::Array(vec![JsonValue::String("1".into()), JsonValue::Number(0.0)]);
    assert_eq!(
        parse_set_fader_volume(&string_id).unwrap_err().code(),
        ContentErrorCode::FieldType
    );
    let negative_id = JsonValue::Array(vec![JsonValue::Int(-1), JsonValue::Number(0.0)]);
    assert_eq!(
        parse_set_fader_volume(&negative_id).unwrap_err().code(),
        ContentErrorCode::FieldRange
    );
    let too_loud = JsonValue::Array(vec![JsonValue::Int(1), JsonValue::Number(60.0)]);
    assert_eq!(
        parse_set_fader_volume(&too_loud).unwrap_err().code(),
        ContentErrorCode::FieldRange
    );
    let nonfinite = JsonValue::Array(vec![JsonValue::Int(1), JsonValue::Number(f64::NAN)]);
    assert_eq!(
        parse_set_fader_volume(&nonfinite).unwrap_err().code(),
        ContentErrorCode::FieldRange
    );
}

#[test]
fn content_fader_write_command_json_shape() {
    let write = FaderWrite {
        fader_id: FADER_AUX_STEREO,
        volume_db: AUX_MUTE_DB,
    };
    assert_eq!(
        write.to_command_text().expect("command text"),
        "{\"SetFaderVolume\":[1,-120]}"
    );
    let write = FaderWrite {
        fader_id: FADER_AUX_PASS,
        volume_db: AUX_OPEN_DB,
    };
    assert_eq!(
        write.to_command_text().expect("command text"),
        "{\"SetFaderVolume\":[2,0]}"
    );
    let parsed = json_parse(&write.to_command_text().expect("text")).expect("parse");
    let pairs = parsed.as_object().expect("object");
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].0, "SetFaderVolume");
    let args = pairs[0].1.as_array().expect("array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_i64(), Some(2));
}

// ---------------------------------------------------------------------------
// D. Aux1/Aux2 fader 绑定 / 切换计划 / 原子切换（无双上混）
// ---------------------------------------------------------------------------

#[test]
fn content_fader_path_combinations() {
    assert_eq!(FaderBinding::both_muted().path(), AuxPath::Muted);
    assert_eq!(FaderBinding::direct().path(), AuxPath::PassThrough);
    assert_eq!(FaderBinding::upmix().path(), AuxPath::StereoUpmix);
    assert_eq!(
        FaderBinding::camilla_startup_default().path(),
        AuxPath::DoubleUpmix
    );
    assert!(FaderBinding::direct().is_safe());
    assert!(FaderBinding::direct().is_steady_state());
    assert!(!FaderBinding::direct().is_silent());
    assert!(FaderBinding::both_muted().is_safe());
    assert!(!FaderBinding::both_muted().is_steady_state());
    assert!(!FaderBinding::camilla_startup_default().is_safe());
}

#[test]
fn content_fader_binding_for_each_mode() {
    assert_eq!(
        FaderBinding::for_mode(ContentMode::Stereo),
        FaderBinding::upmix()
    );
    assert_eq!(
        FaderBinding::for_mode(ContentMode::Multichannel),
        FaderBinding::direct()
    );
    assert_eq!(
        FaderBinding::for_mode(ContentMode::Auto),
        FaderBinding::direct()
    );
    assert_eq!(FaderBinding::direct().writes(), [(1, -120.0), (2, 0.0)]);
    assert_eq!(FaderBinding::upmix().writes(), [(1, 0.0), (2, -120.0)]);
}

#[test]
fn content_fader_validate_rejects_non_path_levels() {
    let bad = FaderBinding {
        aux_stereo_db: -20.0,
        aux_pass_db: -120.0,
    };
    assert_eq!(
        bad.validate().unwrap_err().code(),
        ContentErrorCode::InvalidFader
    );
    let nonfinite = FaderBinding {
        aux_stereo_db: f64::NAN,
        aux_pass_db: -120.0,
    };
    assert_eq!(
        nonfinite.validate().unwrap_err().code(),
        ContentErrorCode::InvalidFader
    );
    assert!(FaderBinding::both_muted().validate().is_ok());
    assert!(FaderBinding::camilla_startup_default().validate().is_ok());
}

#[test]
fn content_binding_coherence_matrix() {
    for mode in ContentMode::ALL {
        assert!(ContentBinding::for_mode(*mode).validate().is_ok(), "{mode}");
    }
    assert!(ContentBinding::safe_muted().validate().is_ok());
    let incoherent = ContentBinding {
        content_mode: ContentMode::Stereo,
        faders: FaderBinding::direct(),
    };
    assert_eq!(
        incoherent.validate().unwrap_err().code(),
        ContentErrorCode::IncoherentBinding
    );
    let incoherent = ContentBinding {
        content_mode: ContentMode::Multichannel,
        faders: FaderBinding::upmix(),
    };
    assert_eq!(
        incoherent.validate().unwrap_err().code(),
        ContentErrorCode::IncoherentBinding
    );
    let incoherent = ContentBinding {
        content_mode: ContentMode::Auto,
        faders: FaderBinding::upmix(),
    };
    assert_eq!(
        incoherent.validate().unwrap_err().code(),
        ContentErrorCode::IncoherentBinding
    );
    let safe = ContentBinding {
        content_mode: ContentMode::Stereo,
        faders: FaderBinding::both_muted(),
    };
    assert!(safe.validate().is_ok());
}

#[test]
fn content_binding_rejects_double_upmix() {
    let binding = ContentBinding {
        content_mode: ContentMode::Auto,
        faders: FaderBinding::camilla_startup_default(),
    };
    assert!(binding.is_double_upmix());
    assert_eq!(
        binding.validate().unwrap_err().code(),
        ContentErrorCode::DoubleUpmix
    );
    assert_eq!(
        ContentSwitcher::new(binding).unwrap_err().code(),
        ContentErrorCode::DoubleUpmix
    );
}

#[test]
fn content_switch_plan_idempotent_when_target_matches() {
    for mode in ContentMode::ALL {
        let binding = ContentBinding::for_mode(*mode);
        let plan = binding.plan_to(*mode);
        assert!(plan.is_idempotent());
        assert_eq!(plan.writes.len(), 0);
        assert!(plan.validate().is_ok());
    }
}

#[test]
fn content_switch_plan_direct_to_stereo_closes_then_opens() {
    let plan = FaderSwitchPlan::plan(
        ContentMode::Multichannel,
        FaderBinding::direct(),
        ContentMode::Stereo,
    );
    assert_eq!(writes_of(&plan), vec![(2, -120.0), (1, 0.0)]);
    assert!(plan.validate().is_ok());
    let states = plan.intermediate_bindings();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0], FaderBinding::both_muted());
    assert_eq!(states[1], FaderBinding::upmix());
    assert!(states.iter().all(|state| !state.is_double_upmix()));
}

#[test]
fn content_switch_plan_stereo_to_direct_closes_then_opens() {
    let plan = FaderSwitchPlan::plan(
        ContentMode::Stereo,
        FaderBinding::upmix(),
        ContentMode::Multichannel,
    );
    assert_eq!(writes_of(&plan), vec![(1, -120.0), (2, 0.0)]);
    assert!(plan.validate().is_ok());
    assert_eq!(
        plan.intermediate_bindings(),
        vec![FaderBinding::both_muted(), FaderBinding::direct()]
    );
}

#[test]
fn content_switch_plan_from_unsafe_camilla_default() {
    let unsafe_start = FaderBinding::camilla_startup_default();
    let to_direct =
        FaderSwitchPlan::plan(ContentMode::Auto, unsafe_start, ContentMode::Multichannel);
    // Aux2 已经是开的，只需关掉 Aux1。
    assert_eq!(writes_of(&to_direct), vec![(1, -120.0)]);
    assert!(to_direct.validate().is_ok());
    assert!(to_direct
        .intermediate_bindings()
        .iter()
        .all(|s| !s.is_double_upmix()));
    let to_stereo = FaderSwitchPlan::plan(ContentMode::Auto, unsafe_start, ContentMode::Stereo);
    // Aux1 已经是开的，只需关掉 Aux2。
    assert_eq!(writes_of(&to_stereo), vec![(2, -120.0)]);
    assert!(to_stereo.validate().is_ok());
    assert_eq!(
        to_stereo.intermediate_bindings(),
        vec![FaderBinding::upmix()]
    );
}

#[test]
fn content_switch_plan_intermediates_never_double_upmix() {
    let starts = [
        FaderBinding::direct(),
        FaderBinding::upmix(),
        FaderBinding::both_muted(),
        FaderBinding::camilla_startup_default(),
    ];
    for start in starts {
        for target in ContentMode::ALL {
            let plan = FaderSwitchPlan::plan(ContentMode::Auto, start, *target);
            assert!(plan.validate().is_ok(), "{start:?} -> {target}");
            for state in plan.intermediate_bindings() {
                assert!(
                    !state.is_double_upmix(),
                    "{start:?} -> {target} via {state:?}"
                );
            }
            assert_eq!(plan.to, FaderBinding::for_mode(*target));
        }
    }
}

#[test]
fn content_switch_plan_validate_rejects_bad_target() {
    let mut plan = FaderSwitchPlan::plan(
        ContentMode::Multichannel,
        FaderBinding::direct(),
        ContentMode::Stereo,
    );
    plan.to = FaderBinding::direct();
    assert_eq!(
        plan.validate().unwrap_err().code(),
        ContentErrorCode::IncoherentBinding
    );
    let mut plan = FaderSwitchPlan::plan(
        ContentMode::Multichannel,
        FaderBinding::direct(),
        ContentMode::Stereo,
    );
    plan.writes.push(FaderWrite {
        fader_id: 9,
        volume_db: 0.0,
    });
    assert_eq!(
        plan.validate().unwrap_err().code(),
        ContentErrorCode::InvalidFader
    );
}

#[test]
fn content_switcher_apply_success_commits_binding_atomically() {
    let mut switcher = ContentSwitcher::safe_default();
    assert_eq!(switcher.binding().content_mode, ContentMode::Multichannel);
    let mut written = Vec::new();
    let plan = switcher
        .apply(ContentMode::Stereo, |write| {
            written.push((write.fader_id, write.volume_db));
            Ok(())
        })
        .expect("apply");
    assert_eq!(written, vec![(2, -120.0), (1, 0.0)]);
    assert_eq!(plan.target_mode, ContentMode::Stereo);
    assert_eq!(switcher.binding().content_mode, ContentMode::Stereo);
    assert_eq!(switcher.binding().faders, FaderBinding::upmix());
    assert!(switcher.binding().validate().is_ok());
    // 已到位：再次 apply 为零写入。
    let plan = switcher
        .apply(ContentMode::Stereo, |_| panic!("no writes expected"))
        .expect("idempotent apply");
    assert_eq!(plan.writes.len(), 0);
}

#[test]
fn content_switcher_apply_failure_fails_safe() {
    let mut switcher = ContentSwitcher::safe_default();
    let err = switcher
        .apply(ContentMode::Stereo, |write| {
            Err(format!("fader {} io error", write.fader_id))
        })
        .unwrap_err();
    assert_eq!(err.code(), ContentErrorCode::FaderWriteFailed);
    assert!(err.message().contains("SetFaderVolume"));
    assert_eq!(switcher.binding(), &ContentBinding::safe_muted());
    assert!(!switcher.binding().is_double_upmix());
    assert!(switcher.binding().validate().is_ok());
}

#[test]
fn content_switcher_startup_safe_plan_from_camilla_default() {
    let switcher = ContentSwitcher::camilla_startup_default();
    assert!(!switcher.is_safe());
    let plan = switcher.startup_safe_plan();
    assert_eq!(writes_of(&plan), vec![(1, -120.0)]);
    assert!(plan.validate().is_ok());
    let mut switcher = switcher.clone();
    let mut written = Vec::new();
    switcher
        .apply(ContentMode::Multichannel, |write| {
            written.push((write.fader_id, write.volume_db));
            Ok(())
        })
        .expect("startup apply");
    assert_eq!(written, vec![(1, -120.0)]);
    assert!(switcher.is_safe());
    assert_eq!(switcher.binding().faders, FaderBinding::direct());
}

#[test]
fn content_switcher_safe_default_is_direct() {
    let switcher = ContentSwitcher::safe_default();
    assert!(switcher.is_safe());
    assert_eq!(switcher.binding().faders, FaderBinding::direct());
    assert_eq!(switcher.binding().content_mode, ContentMode::Multichannel);
    assert_eq!(switcher.plan_to(ContentMode::Multichannel).writes.len(), 0);
}

// ---------------------------------------------------------------------------
// E. override > detector > auto 合并视图（C03）
// ---------------------------------------------------------------------------

#[test]
fn content_view_override_stereo_beats_detected_multichannel() {
    let view = build_content_mode_view(
        Some(ContentMode::Auto),
        DetectedKind::Multichannel,
        Some(ContentMode::Stereo),
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(view.content_mode, ContentMode::Stereo);
    assert!(view.notes.contains(&"manual-override"));
    assert!(view.notes.contains(&"h16-internal-upmix-passthrough"));
    assert!(view.daemon_running);
    assert_eq!(view.h16_internal_upmix, Some(true));
}

#[test]
fn content_view_override_multichannel_forces_direct() {
    let view = build_content_mode_view(
        Some(ContentMode::Auto),
        DetectedKind::Stereo,
        Some(ContentMode::Multichannel),
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(view.content_mode, ContentMode::Multichannel);
    assert!(view.notes.contains(&"manual-override"));
    // S1 WebUI 同样保留检测提示（覆盖值只改生效模式，不篡改 detector 观测）。
    assert!(view.notes.contains(&"stereo-upmix-required"));
    assert_eq!(view.h16_internal_upmix, Some(false));
}

#[test]
fn content_view_auto_resolves_by_detector() {
    let stereo = build_content_mode_view(
        Some(ContentMode::Auto),
        DetectedKind::Stereo,
        None,
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(stereo.content_mode, ContentMode::Stereo);
    assert!(stereo.notes.contains(&"stereo-upmix-required"));
    let multi = build_content_mode_view(
        Some(ContentMode::Auto),
        DetectedKind::Multichannel,
        None,
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(multi.content_mode, ContentMode::Multichannel);
    let unknown = build_content_mode_view(
        Some(ContentMode::Auto),
        DetectedKind::Unknown,
        None,
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(unknown.content_mode, ContentMode::Multichannel);
}

#[test]
fn content_view_no_state_is_auto_with_reason_code() {
    let view = build_content_mode_view(None, DetectedKind::Unknown, None, false, 1_000, None);
    assert_eq!(view.content_mode, ContentMode::Auto);
    assert!(!view.state_available);
    assert!(!view.daemon_running);
    assert!(view.notes.contains(&"daemon-not-running"));
    assert_eq!(view.h16_internal_upmix, None);
}

#[test]
fn content_view_state_mode_wins_over_detector() {
    let view = build_content_mode_view(
        Some(ContentMode::Stereo),
        DetectedKind::Multichannel,
        None,
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(view.content_mode, ContentMode::Stereo);
}

#[test]
fn content_view_daemon_running_window_is_10_seconds() {
    let fresh = build_content_mode_view(None, DetectedKind::Unknown, None, true, 1_000, Some(990));
    assert!(fresh.daemon_running);
    let stale = build_content_mode_view(None, DetectedKind::Unknown, None, true, 1_000, Some(989));
    assert!(!stale.daemon_running);
    let future =
        build_content_mode_view(None, DetectedKind::Unknown, None, true, 1_000, Some(1_001));
    assert!(future.daemon_running);
}

#[test]
fn content_view_override_auto_is_not_manual() {
    let view = build_content_mode_view(
        Some(ContentMode::Auto),
        DetectedKind::Stereo,
        Some(ContentMode::Auto),
        true,
        1_000,
        Some(1_000),
    );
    assert_eq!(view.content_mode, ContentMode::Stereo);
    assert!(!view.notes.contains(&"manual-override"));
}

#[test]
fn content_input_mode_override_json_roundtrip() {
    let override_file = InputModeOverride {
        mode: ContentMode::Multichannel,
        updated_unix: 1_794_724_357,
        updated_by: "webui".to_string(),
    };
    let json = override_file.to_json();
    assert_eq!(
        json.get("schema").and_then(JsonValue::as_str),
        Some("tspi-input-mode/v1")
    );
    assert_eq!(
        json.get("mode").and_then(JsonValue::as_str),
        Some("multichannel")
    );
    let text = override_file.to_json_string().expect("encode");
    let parsed = InputModeOverride::from_json_str(&text).expect("parse");
    assert_eq!(parsed, override_file);
}

#[test]
fn content_input_mode_override_force_alias_normalized() {
    let value = json_parse("{\"schema\":\"tspi-input-mode/v1\",\"mode\":\"force-multichannel\"}")
        .expect("parse");
    let override_file = InputModeOverride::from_json(&value).expect("normalize");
    assert_eq!(override_file.mode, ContentMode::Multichannel);
    let value = json_parse("{\"schema\":\"tspi-input-mode/v1\",\"mode\":\" FORCE-STEREO \"}")
        .expect("parse");
    let override_file = InputModeOverride::from_json(&value).expect("normalize");
    assert_eq!(override_file.mode, ContentMode::Stereo);
}

#[test]
fn content_input_mode_override_negative_matrix() {
    let unknown =
        json_parse("{\"schema\":\"tspi-input-mode/v1\",\"mode\":\"5.1\"}").expect("parse");
    assert_eq!(
        InputModeOverride::from_json(&unknown).unwrap_err().code(),
        ContentErrorCode::UnknownContentMode
    );
    let missing = json_parse("{\"schema\":\"tspi-input-mode/v1\"}").expect("parse");
    assert_eq!(
        InputModeOverride::from_json(&missing).unwrap_err().code(),
        ContentErrorCode::MissingField
    );
    let wrong_schema =
        json_parse("{\"schema\":\"tspi-input-mode/v2\",\"mode\":\"auto\"}").expect("parse");
    assert_eq!(
        InputModeOverride::from_json(&wrong_schema)
            .unwrap_err()
            .code(),
        ContentErrorCode::InvalidSchema
    );
    assert_eq!(
        InputModeOverride::from_json_str("not json")
            .unwrap_err()
            .code(),
        ContentErrorCode::InvalidJson
    );
}

#[test]
fn content_input_mode_override_metadata_optional_and_unknown_keys_ignored() {
    let value = json_parse(
        "{\"schema\":\"tspi-input-mode/v1\",\"mode\":\"auto\",\"updated_by\":\"cli\",\"x_extra\":1}",
    )
    .expect("parse");
    let override_file = InputModeOverride::from_json(&value).expect("parse");
    assert_eq!(override_file.mode, ContentMode::Auto);
    assert_eq!(override_file.updated_unix, 0);
    assert_eq!(override_file.updated_by, "cli");
    let new_override = InputModeOverride::new(ContentMode::Stereo);
    assert_eq!(new_override.updated_unix, 0);
    assert_eq!(new_override.updated_by, "");
}

// ---------------------------------------------------------------------------
// F. S1 autodetect daemon 状态机（C05/C06；启动冻结 / hold / 迟滞 / override / 失败回退）
// ---------------------------------------------------------------------------

const STEREO_20: [(&str, f64); 2] = [("FL", -20.0), ("FR", -20.5)];
const MULTI_714: [(&str, f64); 5] = [
    ("FL", -18.0),
    ("FR", -18.5),
    ("C", -22.0),
    ("SL", -24.0),
    ("TFL", -30.0),
];

#[test]
fn content_daemon_initial_state_and_safe_default() {
    let daemon = new_daemon();
    assert_eq!(daemon.current_mode(), ContentMode::Multichannel);
    assert_eq!(daemon.override_mode(), None);
    assert!(daemon.last_state().is_none());
    assert!(daemon.switcher().is_safe());
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::direct());
}

#[test]
fn content_daemon_rejects_invalid_config() {
    let mut bad = config();
    bad.hold_ms = 0;
    assert_eq!(
        ContentDaemon::new(bad).unwrap_err().code(),
        ContentErrorCode::InvalidConfig
    );
    assert!(ContentDaemon::new(config()).is_ok());
    let mut daemon = new_daemon();
    assert_eq!(
        daemon.tick(f64::NAN, &STEREO_20, 1_000).unwrap_err().code(),
        ContentErrorCode::InvalidConfig
    );
    assert_eq!(
        daemon.tick(-1.0, &STEREO_20, 1_000).unwrap_err().code(),
        ContentErrorCode::InvalidConfig
    );
}

#[test]
fn content_daemon_startup_freeze_keeps_direct() {
    let mut daemon = new_daemon();
    for now_ms in [0.0, 500.0, 2_000.0, 2_499.0] {
        let tick = tick_ok(&mut daemon, now_ms, &STEREO_20, 1_000);
        assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
        assert_eq!(tick.state.detected, DetectedKind::Stereo);
        assert_eq!(tick.state.reason, "startup-freeze");
        assert_eq!(tick.state.detector, ContentDetectorKind::Energy);
        assert!(tick.fader_plan.is_none());
    }
}

#[test]
fn content_daemon_stereo_switch_after_hold() {
    let mut daemon = new_daemon();
    // 冻结结束后的首轮：开始 pending（不立即切换）。
    let tick = tick_ok(&mut daemon, 2_500.0, &STEREO_20, 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    assert_eq!(tick.state.reason, "fl-fr-only");
    let tick = tick_ok(&mut daemon, 4_000.0, &STEREO_20, 1_001);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    assert!(tick.fader_plan.is_none());
    // 恰好连续 2500 ms（>= hold）→ 切换 stereo 并下发 fader 计划。
    let tick = tick_ok(&mut daemon, 5_000.0, &STEREO_20, 1_002);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
    assert_eq!(tick.detection.detected, DetectedKind::Stereo);
    assert_eq!(tick.state.h16_internal_upmix, Some(false));
    let plan = tick.fader_plan.expect("stereo fader plan");
    assert_eq!(plan.target_mode, ContentMode::Stereo);
    assert_eq!(writes_of(&plan), vec![(2, -120.0), (1, 0.0)]);
    apply_ok(&mut daemon, &plan);
    assert_eq!(daemon.current_mode(), ContentMode::Stereo);
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::upmix());
    // 稳定后不再重复下发。
    let tick = tick_ok(&mut daemon, 5_500.0, &STEREO_20, 1_003);
    assert!(tick.fader_plan.is_none());
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
}

#[test]
fn content_daemon_pending_resets_on_flicker() {
    let mut daemon = new_daemon();
    let _ = tick_ok(&mut daemon, 2_500.0, &STEREO_20, 1_000);
    // 2.0 候选被打断（多声道候选）→ 计时重置。
    let _ = tick_ok(&mut daemon, 3_000.0, &MULTI_714, 1_001);
    // 再次 2.0；从 3_500 重新计时。
    let _ = tick_ok(&mut daemon, 3_500.0, &STEREO_20, 1_002);
    let tick = tick_ok(&mut daemon, 5_000.0, &STEREO_20, 1_003);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    let tick = tick_ok(&mut daemon, 6_000.0, &STEREO_20, 1_004);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
}

#[test]
fn content_daemon_hysteresis_keeps_multichannel() {
    let mut daemon = new_daemon();
    // 先进入多声道（冻结后首轮即 current=multichannel，无需切换）。
    let tick = tick_ok(&mut daemon, 2_500.0, &MULTI_714, 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    assert_eq!(tick.state.reason, "secondary-active");
    // 副声道 -52（迟滞带）：判 unknown/hysteresis-band，但保持多声道。
    let band = [("FL", -20.0), ("FR", -20.0), ("C", -52.0)];
    for now_ms in [3_000.0, 4_000.0, 5_000.0, 6_000.0] {
        let tick = tick_ok(&mut daemon, now_ms, &band, 1_001);
        assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
        assert_eq!(tick.state.detected, DetectedKind::Unknown);
        assert_eq!(tick.state.reason, "hysteresis-band");
        assert!(tick.fader_plan.is_none());
    }
    // 只有 FL/FR（副声道 <= exit）→ 判 stereo，hold 后切 stereo。
    let stereo = [("FL", -20.0), ("FR", -20.0)];
    let _ = tick_ok(&mut daemon, 6_500.0, &stereo, 1_002);
    let tick = tick_ok(&mut daemon, 9_000.0, &stereo, 1_003);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
    let plan = tick.fader_plan.expect("plan");
    assert_eq!(plan.target_mode, ContentMode::Stereo);
}

#[test]
fn content_daemon_manual_override_is_immediate() {
    let mut daemon = new_daemon();
    daemon.set_override(Some(ContentMode::Stereo));
    let tick = tick_ok(&mut daemon, 0.0, &MULTI_714, 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
    assert_eq!(tick.state.detector, ContentDetectorKind::Manual);
    assert_eq!(tick.state.reason, "override-stereo");
    assert_eq!(tick.state.override_mode, Some(ContentMode::Stereo));
    let plan = tick.fader_plan.expect("override plan");
    assert_eq!(plan.target_mode, ContentMode::Stereo);
    apply_ok(&mut daemon, &plan);
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::upmix());
    // override multichannel 直接回直通。
    daemon.set_override(Some(ContentMode::Multichannel));
    let tick = tick_ok(&mut daemon, 500.0, &STEREO_20, 1_001);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    assert_eq!(tick.state.reason, "override-multichannel");
    let plan = tick.fader_plan.expect("override back plan");
    assert_eq!(writes_of(&plan), vec![(1, -120.0), (2, 0.0)]);
    apply_ok(&mut daemon, &plan);
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::direct());
}

#[test]
fn content_daemon_override_release_restarts_hold() {
    let mut daemon = new_daemon();
    daemon.set_override(Some(ContentMode::Multichannel));
    let _ = tick_ok(&mut daemon, 0.0, &STEREO_20, 1_000);
    daemon.set_override(None);
    // 解除覆盖后从冻结/hold 重新开始：不能立即带上混。
    let tick = tick_ok(&mut daemon, 2_500.0, &STEREO_20, 1_001);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    assert_eq!(tick.state.override_mode, None);
    let tick = tick_ok(&mut daemon, 4_000.0, &STEREO_20, 1_002);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    let tick = tick_ok(&mut daemon, 5_000.0, &STEREO_20, 1_003);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
}

#[test]
fn content_daemon_auto_default_direct_after_freeze_without_signal() {
    let mut daemon = new_daemon();
    let empty: [(&str, f64); 0] = [];
    let tick = tick_ok(&mut daemon, 2_500.0, &empty, 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    assert_eq!(tick.state.detected, DetectedKind::Unknown);
    assert_eq!(tick.state.reason, "no-data");
    assert!(tick.fader_plan.is_none());
}

#[test]
fn content_daemon_unsafe_startup_emits_safe_plan() {
    let mut daemon = ContentDaemon::new_unsafe_startup(config()).expect("daemon");
    assert!(!daemon.switcher().is_safe());
    let tick = tick_ok(&mut daemon, 0.0, &STEREO_20, 1_000);
    let plan = tick.fader_plan.expect("startup safe plan");
    assert_eq!(writes_of(&plan), vec![(1, -120.0)]);
    assert!(plan.validate().is_ok());
    assert!(plan
        .intermediate_bindings()
        .iter()
        .all(|state| !state.is_double_upmix()));
    apply_ok(&mut daemon, &plan);
    assert!(daemon.switcher().is_safe());
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::direct());
}

#[test]
fn content_daemon_dry_run_writes_state_but_no_fader_plan() {
    let mut daemon = new_daemon();
    daemon.set_dry_run(true);
    daemon.set_override(Some(ContentMode::Stereo));
    let tick = tick_ok(&mut daemon, 0.0, &STEREO_20, 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
    assert!(tick.fader_plan.is_none());
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::direct());
}

#[test]
fn content_daemon_fader_error_fails_safe_and_is_reported() {
    let mut daemon = new_daemon();
    daemon.set_override(Some(ContentMode::Stereo));
    let tick = tick_ok(&mut daemon, 0.0, &STEREO_20, 1_000);
    assert!(tick.fader_plan.is_some());
    daemon.mark_fader_error("ws-closed");
    assert_eq!(daemon.switcher().binding(), &ContentBinding::safe_muted());
    let tick = tick_ok(&mut daemon, 500.0, &STEREO_20, 1_001);
    assert_eq!(tick.state.detector, ContentDetectorKind::Fallback);
    assert_eq!(tick.state.reason, "fader-error: ws-closed");
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
    // 下一轮恢复能量检测，并重试目标 fader 计划（both-muted → upmix 只需开 Aux1）。
    let tick = tick_ok(&mut daemon, 1_000.0, &STEREO_20, 1_002);
    assert_eq!(tick.state.detector, ContentDetectorKind::Manual);
    let plan = tick.fader_plan.expect("retry plan");
    assert_eq!(writes_of(&plan), vec![(1, 0.0)]);
    assert!(plan.validate().is_ok());
}

#[test]
fn content_daemon_ws_error_state() {
    let mut daemon = new_daemon();
    daemon.set_override(Some(ContentMode::Auto));
    let tick = daemon.tick_ws_error("connect refused", 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Auto);
    assert_eq!(tick.state.detected, DetectedKind::Unknown);
    assert_eq!(tick.state.reason, "ws-error: connect refused");
    assert_eq!(tick.state.detector, ContentDetectorKind::Fallback);
    assert_eq!(tick.state.override_mode, Some(ContentMode::Auto));
    assert_eq!(tick.state.h16_internal_upmix, None);
    assert!(tick.fader_plan.is_none());
    assert_eq!(tick.state.container_layout.as_deref(), Some("h16-7.1.4"));
}

#[test]
fn content_daemon_state_rms_rounding_to_0_1_db() {
    let mut daemon = new_daemon();
    let tick = tick_ok(
        &mut daemon,
        3_000.0,
        &[("FL", -20.06), ("FR", -20.05), ("C", -30.04)],
        1_000,
    );
    let rms = &tick.state.rms_db;
    let get = |role: &str| {
        rms.iter()
            .find(|(name, _)| name == role)
            .map(|(_, db)| *db)
            .expect("role in rms")
    };
    assert_eq!(get("FL"), -20.1);
    assert_eq!(get("FR"), -20.1);
    assert_eq!(get("C"), -30.0);
}

#[test]
fn content_daemon_state_override_and_detector_fields() {
    let mut daemon = new_daemon();
    daemon.set_override(Some(ContentMode::Auto));
    let tick = tick_ok(&mut daemon, 3_000.0, &STEREO_20, 1_000);
    assert_eq!(tick.state.override_mode, Some(ContentMode::Auto));
    assert_eq!(tick.state.detector, ContentDetectorKind::Energy);
    let tick = tick_ok(&mut daemon, 3_500.0, &MULTI_714, 1_001);
    assert_eq!(tick.state.detected, DetectedKind::Multichannel);
    assert_eq!(tick.state.h16_internal_upmix, Some(true));
    assert_eq!(tick.state.threshold_db, -50.0);
    assert_eq!(tick.state.hold_ms, 2500);
}

#[test]
fn content_daemon_container_layout_override() {
    let mut daemon = new_daemon();
    daemon.set_container_layout("h16-5.1.2");
    let tick = tick_ok(&mut daemon, 3_000.0, &STEREO_20, 1_000);
    assert_eq!(tick.state.container_layout.as_deref(), Some("h16-5.1.2"));
}

#[test]
fn content_daemon_multichannel_to_stereo_transition_binding() {
    let mut daemon = new_daemon();
    // 多声道内容保持直通（启动冻结后）。
    let tick = tick_ok(&mut daemon, 2_500.0, &MULTI_714, 1_000);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    // 2.0 出现，hold 后切 stereo；从直通绑定的切换必须安全。
    let _ = tick_ok(&mut daemon, 3_000.0, &STEREO_20, 1_001);
    let tick = tick_ok(&mut daemon, 5_500.0, &STEREO_20, 1_002);
    assert_eq!(tick.state.content_mode, ContentMode::Stereo);
    assert_eq!(tick.state.reason, "fl-fr-only");
    let plan = tick.fader_plan.expect("plan");
    assert_eq!(plan.from_mode, ContentMode::Multichannel);
    assert_eq!(plan.from, FaderBinding::direct());
    assert!(plan.validate().is_ok());
    apply_ok(&mut daemon, &plan);
    // 再回到多声道：hold 后切回，fader 先关 Aux1 再开 Aux2。
    let _ = tick_ok(&mut daemon, 6_000.0, &MULTI_714, 1_003);
    let tick = tick_ok(&mut daemon, 8_500.0, &MULTI_714, 1_004);
    assert_eq!(tick.state.content_mode, ContentMode::Multichannel);
    let plan = tick.fader_plan.expect("back plan");
    assert_eq!(writes_of(&plan), vec![(1, -120.0), (2, 0.0)]);
    apply_ok(&mut daemon, &plan);
    assert_eq!(daemon.switcher().binding().faders, FaderBinding::direct());
}

// ---------------------------------------------------------------------------
// G. S1 运行态 JSON（C01）序列化 / 严格解析
// ---------------------------------------------------------------------------

fn sample_state() -> ContentState {
    let mut daemon = new_daemon();
    daemon.set_override(Some(ContentMode::Auto));
    let tick = tick_ok(&mut daemon, 5_000.0, &MULTI_714, 1_794_724_357);
    tick.state
}

#[test]
fn content_state_json_field_order_and_types() {
    let state = sample_state();
    let json = state.to_json();
    let pairs = json.as_object().expect("state object");
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "schema",
            "content_mode",
            "detected",
            "override",
            "confidence",
            "active_channels",
            "rms_db",
            "threshold_db",
            "hold_ms",
            "reason",
            "h16_internal_upmix",
            "container_layout",
            "detector",
            "updated_unix",
        ]
    );
    assert_eq!(
        json.get("schema").and_then(JsonValue::as_str),
        Some("tspi-input-content/v1")
    );
    assert_eq!(
        json.get("content_mode").and_then(JsonValue::as_str),
        Some("multichannel")
    );
    assert_eq!(
        json.get("detected").and_then(JsonValue::as_str),
        Some("multichannel")
    );
    assert_eq!(
        json.get("override").and_then(JsonValue::as_str),
        Some("auto")
    );
    assert!(json.get("confidence").and_then(JsonValue::as_f64).is_some());
    assert!(json
        .get("active_channels")
        .and_then(JsonValue::as_array)
        .is_some());
    assert!(json.get("rms_db").and_then(JsonValue::as_object).is_some());
    assert_eq!(
        json.get("threshold_db").and_then(JsonValue::as_f64),
        Some(-50.0)
    );
    assert_eq!(json.get("hold_ms").and_then(JsonValue::as_u64), Some(2500));
    assert!(json.get("reason").and_then(JsonValue::as_str).is_some());
    assert_eq!(
        json.get("h16_internal_upmix").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(
        json.get("container_layout").and_then(JsonValue::as_str),
        Some("h16-7.1.4")
    );
    assert_eq!(
        json.get("detector").and_then(JsonValue::as_str),
        Some("energy")
    );
    assert_eq!(
        json.get("updated_unix").and_then(JsonValue::as_u64),
        Some(1_794_724_357)
    );
}

#[test]
fn content_state_json_roundtrip() {
    let state = sample_state();
    let text = state.to_json_string().expect("state encode");
    let parsed = ContentState::from_json_str(&text).expect("state parse");
    assert_eq!(parsed, state);
}

#[test]
fn content_state_from_json_schema_mismatch() {
    let state = sample_state();
    let mut json = state.to_json();
    json = JsonValue::Object(
        json.as_object()
            .expect("object")
            .iter()
            .map(|(key, value)| {
                if key == "schema" {
                    (
                        key.clone(),
                        JsonValue::String("tspi-input-content/v2".into()),
                    )
                } else {
                    (key.clone(), value.clone())
                }
            })
            .collect(),
    );
    assert_eq!(
        ContentState::from_json(&json).unwrap_err().code(),
        ContentErrorCode::InvalidSchema
    );
}

#[test]
fn content_state_from_json_bad_fields_matrix() {
    let state = sample_state();
    let base = state.to_json();
    let with = |key: &str, value: JsonValue| -> JsonValue {
        JsonValue::Object(
            base.as_object()
                .expect("object")
                .iter()
                .map(|(name, old)| {
                    if name == key {
                        (name.clone(), value.clone())
                    } else {
                        (name.clone(), old.clone())
                    }
                })
                .collect(),
        )
    };
    let no_detected = JsonValue::Object(
        base.as_object()
            .expect("object")
            .iter()
            .filter(|(name, _)| name != "detected")
            .cloned()
            .collect(),
    );
    assert_eq!(
        ContentState::from_json(&no_detected).unwrap_err().code(),
        ContentErrorCode::MissingField
    );
    assert_eq!(
        ContentState::from_json(&with("detected", JsonValue::String("5.1".into())))
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldRange
    );
    assert_eq!(
        ContentState::from_json(&with("confidence", JsonValue::Number(2.0)))
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldRange
    );
    assert_eq!(
        ContentState::from_json(&with(
            "active_channels",
            JsonValue::Array(vec![JsonValue::Int(1)])
        ))
        .unwrap_err()
        .code(),
        ContentErrorCode::FieldType
    );
    assert_eq!(
        ContentState::from_json(&with(
            "rms_db",
            JsonValue::Object(vec![("FL".into(), JsonValue::String("x".into()))])
        ))
        .unwrap_err()
        .code(),
        ContentErrorCode::FieldType
    );
    assert_eq!(
        ContentState::from_json(&with("h16_internal_upmix", JsonValue::Int(1)))
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldType
    );
    assert_eq!(
        ContentState::from_json(&with("container_layout", JsonValue::Bool(true)))
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldType
    );
    assert_eq!(
        ContentState::from_json(&with("detector", JsonValue::String("magic".into())))
            .unwrap_err()
            .code(),
        ContentErrorCode::FieldRange
    );
    assert_eq!(
        ContentState::from_json_str("not json").unwrap_err().code(),
        ContentErrorCode::InvalidJson
    );
}

#[test]
fn content_state_from_json_ignores_unknown_keys() {
    let state = sample_state();
    let mut pairs: Vec<(String, JsonValue)> = state.to_json().as_object().expect("object").to_vec();
    pairs.push(("x_future".into(), JsonValue::Number(1.0)));
    let json = JsonValue::Object(pairs);
    let parsed = ContentState::from_json(&json).expect("forward compatible parse");
    assert_eq!(parsed, state);
}

#[test]
fn content_state_daemon_not_running_shell() {
    let state = ContentState::daemon_not_running(
        Some(ContentMode::Auto),
        &config(),
        Some("h16-7.1.4"),
        1_000,
    );
    assert_eq!(state.content_mode, ContentMode::Auto);
    assert_eq!(state.detected, DetectedKind::Unknown);
    assert_eq!(state.reason, "daemon-not-running");
    assert_eq!(state.detector, ContentDetectorKind::Fallback);
    assert_eq!(state.threshold_db, -50.0);
    assert_eq!(state.hold_ms, 2500);
    assert_eq!(state.h16_internal_upmix, None);
    let json = state.to_json();
    assert!(json.get("h16_internal_upmix").expect("field").is_null());
    assert_eq!(
        json.get("override").and_then(JsonValue::as_str),
        Some("auto")
    );
}

// ---------------------------------------------------------------------------
// H. P1 显式拒绝（C07 / R47；DEC §1/§6）
// ---------------------------------------------------------------------------

#[test]
fn content_p1_input_mode_get_status_exact() {
    let text = p1_input_mode_get_status_text().expect("status text");
    assert_eq!(
        text,
        "{\"status\":\"not_supported\",\"reason\":\"p1-no-content-upmix\"}"
    );
    let json = p1_input_mode_get_status();
    assert_eq!(
        json.get("status").and_then(JsonValue::as_str),
        Some(P1_INPUT_MODE_STATUS)
    );
    assert_eq!(
        json.get("reason").and_then(JsonValue::as_str),
        Some(P1_NO_CONTENT_UPMMIX_REASON)
    );
}

#[test]
fn content_p1_input_mode_put_is_rejected() {
    for requested in ["stereo", "multichannel", "auto", "force-stereo"] {
        let error = p1_reject_input_mode_put(requested);
        assert_eq!(error.code(), ContentErrorCode::P1NotSupported);
        assert!(error.message().contains(P1_NO_CONTENT_UPMMIX_REASON));
        assert!(error.message().contains(requested));
    }
}

#[test]
fn content_p1_playback_modes_matrix() {
    let toslink = p1_playback_content("toslink").expect("toslink");
    assert!(toslink.stereo_strict);
    assert!(toslink.direct_mapping);
    assert!(!toslink.upmix);
    assert!(!toslink.content_detection);
    assert_eq!(toslink.mode, "toslink");
    let target = p1_playback_content("target_multichannel").expect("target");
    assert!(!target.stereo_strict);
    assert!(target.direct_mapping);
    assert!(!target.upmix);
    assert!(!target.content_detection);
    for mode in ["aes67", "dante"] {
        let content = p1_playback_content(mode).expect("aoip mode");
        assert!(content.direct_mapping);
        assert!(!content.upmix);
        assert!(!content.content_detection);
    }
    let off = p1_playback_content("off").expect("off");
    assert!(!off.direct_mapping);
    assert!(!off.upmix);
    let upper = p1_playback_content(" TOSLINK ").expect("case-insensitive");
    assert!(upper.stereo_strict);
    let json = toslink.to_json();
    assert_eq!(
        json.get("mode").and_then(JsonValue::as_str),
        Some("toslink")
    );
    assert_eq!(json.get("upmix").and_then(JsonValue::as_bool), Some(false));
}

#[test]
fn content_p1_playback_unknown_mode_rejected() {
    for mode in ["camilla", "", "p5", "multi"] {
        let error = p1_playback_content(mode).unwrap_err();
        assert_eq!(error.code(), ContentErrorCode::UnknownPlaybackMode);
    }
}

#[test]
fn content_p1_s1_state_cannot_drive_playback() {
    for mode in ContentMode::ALL {
        let error = p1_apply_s1_content(*mode).unwrap_err();
        assert_eq!(error.code(), ContentErrorCode::P1NotSupported);
        assert!(error.message().contains(P1_NO_CONTENT_UPMMIX_REASON));
    }
    assert_eq!(
        p1_direct_content_mode(ContentMode::Stereo),
        ContentMode::Multichannel
    );
    assert_eq!(
        p1_direct_content_mode(ContentMode::Auto),
        ContentMode::Multichannel
    );
}

#[test]
fn content_p1_aux_fader_path_switch_rejected() {
    assert!(!p1_aux_fader_path_switch_supported());
    for fader_id in AUX_FADER_IDS {
        let error = p1_reject_aux_fader_path_switch(fader_id);
        assert_eq!(error.code(), ContentErrorCode::P1NotSupported);
        assert!(error.message().contains(P1_NO_CONTENT_UPMMIX_REASON));
    }
}

#[test]
fn content_p1_legacy_status_input_content_is_null() {
    let value = p1_legacy_status_input_content();
    assert!(value.is_null());
    assert_eq!(json_text(&value), "null");
}

#[test]
fn content_p1_retired_unit_evidence() {
    let path =
        project_file("../../../audio/overlay/etc/systemd/system/tspi-upmix-autodetect.service");
    assert!(path.exists(), "retired S1 unit missing: {path:?}");
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some(P1_RETIRED_CONTENT_UNIT)
    );
    let text = read_lossy(&path);
    assert!(text.contains("tspi-upmix"));
    for marker in P1_RETIRED_CONTENT_UNIT_CAMILLA_DEPS {
        assert!(
            text.contains(marker),
            "S1 unit must declare {marker} (C07 requires clearing it)"
        );
    }
    assert!(text.contains("ExecStart="));
    assert!(text.contains("autodetect"));
    assert!(text.contains("--hold-ms 2500"));
    assert!(text.contains("--startup-freeze-ms 2500"));
    assert!(text.contains("/run/tspi/input-content.json"));
    assert!(text.contains("/etc/tspi-audio/input-mode.json"));
}

#[test]
fn content_p1_retired_unit_name_and_dependencies_frozen() {
    assert!(P1_RETIRED_CONTENT_UNIT.ends_with(".service"));
    assert_eq!(
        P1_RETIRED_CONTENT_UNIT_CAMILLA_DEPS[0],
        "Wants=tspi-camilladsp.service"
    );
    assert_eq!(
        P1_RETIRED_CONTENT_UNIT_CAMILLA_DEPS[1],
        "After=tspi-camilladsp.service"
    );
    assert_eq!(P1_FORBIDDEN_CONTENT_FEATURES.len(), 3);
    assert!(!P1_CONTENT_UPMMIX_SUPPORTED);
}

// ---------------------------------------------------------------------------
// I. 不变量 / 命令形状 / 一次性对拍
// ---------------------------------------------------------------------------

#[test]
fn content_plan_commands_are_w19_shaped() {
    let plan = FaderSwitchPlan::plan(
        ContentMode::Multichannel,
        FaderBinding::direct(),
        ContentMode::Stereo,
    );
    let commands = plan.commands_json().expect("commands");
    assert_eq!(commands.len(), 2);
    for (index, text) in commands.iter().enumerate() {
        let parsed = json_parse(text).expect("command parse");
        let pairs = parsed.as_object().expect("object");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, CMD_SET_FADER_VOLUME);
        let args = pairs[0].1.as_array().expect("array");
        assert_eq!(args.len(), 2);
        let fader_id = args[0].as_i64().expect("id");
        assert!(fader_id == 1 || fader_id == 2);
        let db = args[1].as_f64().expect("db");
        assert!(db == AUX_OPEN_DB || db == AUX_MUTE_DB);
        if index == 1 {
            assert_eq!(fader_id, 1);
            assert_eq!(db, AUX_OPEN_DB);
        }
    }
}

#[test]
fn content_fader_switch_plan_safe_for_all_start_and_target_pairs() {
    let starts = [
        FaderBinding::direct(),
        FaderBinding::upmix(),
        FaderBinding::both_muted(),
        FaderBinding::camilla_startup_default(),
    ];
    for start in starts {
        for target in ContentMode::ALL {
            let plan = FaderSwitchPlan::plan(ContentMode::Auto, start, *target);
            assert!(plan.validate().is_ok(), "{start:?} -> {target}");
            let mut previous = start;
            for state in plan.intermediate_bindings() {
                assert!(!state.is_double_upmix(), "{previous:?} -> {state:?}");
                previous = state;
            }
            assert_eq!(previous, FaderBinding::for_mode(*target));
        }
    }
}

#[test]
fn content_double_upmix_invariant_never_reachable_via_public_flows() {
    // 原子绑定构造器都安全；唯一不安全的构造是显式模拟的 Camilla 启动默认态。
    for mode in ContentMode::ALL {
        assert!(!ContentBinding::for_mode(*mode).is_double_upmix());
    }
    assert!(!ContentBinding::safe_muted().is_double_upmix());
    let unsafe_start = FaderBinding::camilla_startup_default();
    assert!(unsafe_start.is_double_upmix());
    let safe_start =
        FaderSwitchPlan::plan(ContentMode::Auto, unsafe_start, ContentMode::Multichannel);
    assert!(safe_start.validate().is_ok());
}

#[test]
fn content_s1_state_records_are_read_only_snapshots() {
    // S1 state 只作兼容记录：P1 侧没有任何入口把它变成上混/切换。
    let state = sample_state();
    let json = state.to_json();
    assert_eq!(
        json.get("schema").and_then(JsonValue::as_str),
        Some(INPUT_CONTENT_SCHEMA)
    );
    assert_eq!(
        p1_apply_s1_content(state.content_mode).unwrap_err().code(),
        ContentErrorCode::P1NotSupported
    );
    // 状态可完整读回（WebUI/退役证据只读消费）。
    let text = json_text(&json);
    let parsed = ContentState::from_json_str(&text).expect("read back");
    assert_eq!(parsed, state);
}
#[test]
fn content_module_is_marked_retired_compat_shim() {
    // F-31：content 是 S1 退役对照兼容资料，不是 compat 公开运行时；不得被生产路径链接。
    assert_eq!(COMPAT_STATUS, "retired_s1_reference_not_runtime");
    // P1 入口仍显式拒绝（上混状态机只能作退役核对）。
    assert_eq!(
        p1_apply_s1_content(ContentMode::Stereo).unwrap_err().code(),
        ContentErrorCode::P1NotSupported
    );
    assert_eq!(
        p1_direct_content_mode(ContentMode::Stereo),
        ContentMode::Multichannel
    );
    assert!(!p1_aux_fader_path_switch_supported());
    assert_eq!(
        p1_reject_aux_fader_path_switch(1).code(),
        ContentErrorCode::P1NotSupported
    );
    assert!(p1_legacy_status_input_content().is_null());
}
