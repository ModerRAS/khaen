//! T13 calibration 私有集成测试（WBS T13；写域 crates/compat/tests/T13/）。
//!
//! 覆盖 WBS T13 DoD：
//! - 既有 tspi-calibration v1/v1.1 只读接受（CONTRACT §2/§3/§5/§10；未知 schema/type/枚举拒收）；
//! - 未知扩展字段 warning（不参与应用）；v1.1 shelf q=RBJ slope S、peaking q 无上限；
//! - 校准 -> 渲染基线合并（delay 相加 / polarity XOR / gain 相加 / PEQ 追加、type 不降级、
//!   二次应用拒收、跨布局拒收）；
//! - 内部 IR 草案 tspi-cal-ir-draft/v1：version/hash/role/fs/taps/placement/P/phase/D、
//!   stereo mask/target profile、stale 判定；
//! - curve 不双叠（只绑定一次、只落 TargetFir、G 只 modal residual）；
//! - 与 T11 目标曲线数据合同（TargetCurveSource -> TargetCurveRef）；
//! - 与 T12 compile 接口（design_delay_to_plan / StagePlan / CompileOptions / CompileInput+compile）。
//!
//! Cargo 自动把 tests/T13/main.rs 发现为集成测试 target；DoD 命令：
//! `cargo test -p tspi-dsp-compat calibration`（测试名统一 calibration_ 前缀）。
//! 只读输入：calibration-spec/examples/*.yml 经 include_str! 绑定；不改任何校准文件。
//! 本文件不写临时文件（%TEMP% 在内存盘 R: 上可能 PermissionDenied），全部内存夹具。

use tspi_dsp_compat::calibration::*;
use tspi_dsp_core::compile::{compile, CompileInput, CompileOptions};

const EXAMPLE_412: &str =
    include_str!("../../../../../calibration-spec/examples/calibration-4.1.2.example.yml");
const EXAMPLE_532: &str =
    include_str!("../../../../../calibration-spec/examples/calibration-5.3.2.example.yml");
const EXAMPLE_V11: &str =
    include_str!("../../../../../calibration-spec/examples/calibration-v11-highshelf.example.yml");

// ---------------------------------------------------------------------------
// 夹具/辅助
// ---------------------------------------------------------------------------

fn sha_a() -> String {
    "a".repeat(64)
}

fn sha_b() -> String {
    "b".repeat(64)
}

fn sha_c() -> String {
    "c".repeat(64)
}

fn hex64(byte: char) -> String {
    std::iter::repeat(byte).take(64).collect()
}

/// 4.1.2 唯一 role->DAC 表（WBS §3：FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7、ch6 空）。
fn layout_412() -> LayoutRoleTable {
    LayoutRoleTable::new(
        "4.1.2",
        vec![
            LayoutRole {
                id: "FL".into(),
                outputs: vec![0],
            },
            LayoutRole {
                id: "FR".into(),
                outputs: vec![1],
            },
            LayoutRole {
                id: "SL".into(),
                outputs: vec![2],
            },
            LayoutRole {
                id: "SR".into(),
                outputs: vec![3],
            },
            LayoutRole {
                id: "HFL".into(),
                outputs: vec![4],
            },
            LayoutRole {
                id: "HFR".into(),
                outputs: vec![5],
            },
            LayoutRole {
                id: "SUB1".into(),
                outputs: vec![7],
            },
        ],
    )
}

/// 5.3.2 10 路角色表（与 calibration-5.3.2.example.yml 的 index 一致）。
fn layout_532() -> LayoutRoleTable {
    LayoutRoleTable::new(
        "5.3.2",
        vec![
            LayoutRole {
                id: "FL".into(),
                outputs: vec![0],
            },
            LayoutRole {
                id: "FR".into(),
                outputs: vec![1],
            },
            LayoutRole {
                id: "C".into(),
                outputs: vec![2],
            },
            LayoutRole {
                id: "SL".into(),
                outputs: vec![3],
            },
            LayoutRole {
                id: "SR".into(),
                outputs: vec![4],
            },
            LayoutRole {
                id: "SUB1".into(),
                outputs: vec![5],
            },
            LayoutRole {
                id: "SUB2".into(),
                outputs: vec![6],
            },
            LayoutRole {
                id: "SUB3".into(),
                outputs: vec![7],
            },
            LayoutRole {
                id: "HFL".into(),
                outputs: vec![8],
            },
            LayoutRole {
                id: "HFR".into(),
                outputs: vec![9],
            },
        ],
    )
}

fn base_roles_412() -> Vec<RenderRoleBase> {
    let mut roles = vec![
        RenderRoleBase::uncalibrated("FL", 0),
        RenderRoleBase::uncalibrated("FR", 1),
        RenderRoleBase::uncalibrated("SL", 2),
        RenderRoleBase::uncalibrated("SR", 3),
        RenderRoleBase::uncalibrated("HFL", 4),
        RenderRoleBase::uncalibrated("HFR", 5),
        RenderRoleBase::uncalibrated("SUB1", 7),
    ];
    // 渲染基线带一点非零值，验证相加/XOR/追加语义。
    roles[0].gain_db = -1.0;
    roles[1].delay_samples = 1;
    roles[2].peq = vec![PeqBand::new(200.0, 1.0, -2.0, PeqType::Peaking)];
    roles[5].polarity = Polarity::Invert;
    roles
}

fn profile_412() -> IrProfileBinding {
    IrProfileBinding {
        profile_id: "target-4.1.2".into(),
        profile_sha256: sha_a(),
        stereo_strict: false,
        stereo_mask: vec!["FL".into(), "FR".into()],
        target_profile: "target_multichannel_4_1_2".into(),
    }
}

fn profile_stereo_strict() -> IrProfileBinding {
    IrProfileBinding {
        profile_id: "stereo-strict-2.0".into(),
        profile_sha256: sha_b(),
        stereo_strict: true,
        stereo_mask: vec!["FL".into(), "FR".into()],
        target_profile: "stereo_strict_2_0".into(),
    }
}

struct MockCurve {
    curve_id: String,
    fs_hz: f64,
    target_sha256: String,
    phase_ref: Option<String>,
    target_delay_samples: Option<u32>,
    point_count: usize,
}

impl TargetCurveSource for MockCurve {
    fn curve_id(&self) -> &str {
        &self.curve_id
    }
    fn fs_hz(&self) -> f64 {
        self.fs_hz
    }
    fn target_sha256(&self) -> &str {
        &self.target_sha256
    }
    fn phase_ref(&self) -> Option<&str> {
        self.phase_ref.as_deref()
    }
    fn target_delay_samples(&self) -> Option<u32> {
        self.target_delay_samples
    }
    fn point_count(&self) -> usize {
        self.point_count
    }
}

fn mock_curve(fs_hz: f64) -> MockCurve {
    MockCurve {
        curve_id: "tilt10-2026-11-17".into(),
        fs_hz,
        target_sha256: hex64('d'),
        phase_ref: Some("none".into()),
        target_delay_samples: None,
        point_count: 480,
    }
}

fn valid_curve_412() -> TargetCurveRef {
    TargetCurveRef::from_source(&mock_curve(48_000.0)).expect("valid curve")
}

fn doc_412() -> CalibrationDocument {
    parse_calibration_yaml_with_layout(EXAMPLE_412.as_bytes(), Some(&layout_412()))
        .expect("4.1.2 example 必须可解析")
}

fn merged_412() -> MergedCalibration {
    merge_calibration(&doc_412(), Some(&layout_412()), &base_roles_412(), 48_000)
        .expect("4.1.2 merge 必须成功")
}

fn merged_412_plain() -> MergedCalibration {
    // 无基线改动 of 4.1.2（用于 IR/compile 测试，避免基线 delay 干扰 placement 断言）。
    let roles = vec![
        RenderRoleBase::uncalibrated("FL", 0),
        RenderRoleBase::uncalibrated("FR", 1),
        RenderRoleBase::uncalibrated("SL", 2),
        RenderRoleBase::uncalibrated("SR", 3),
        RenderRoleBase::uncalibrated("HFL", 4),
        RenderRoleBase::uncalibrated("HFR", 5),
        RenderRoleBase::uncalibrated("SUB1", 7),
    ];
    merge_calibration(&doc_412(), Some(&layout_412()), &roles, 48_000).expect("merge")
}

fn ir_412_plain() -> IrDraft {
    let merged = merged_412_plain();
    let mut builder = IrDraftBuilder::new(&merged, profile_412(), 1_800_000_000).expect("builder");
    builder
        .with_g_residual_participants(
            ["FL", "FR", "SL", "SR", "SUB1"]
                .iter()
                .map(|name| name.to_string())
                .collect(),
        )
        .expect("participants");
    builder.build().expect("IR build")
}

fn expect_error<T: std::fmt::Debug>(
    result: Result<T, CalibrationError>,
    code: CalibrationErrorCode,
) {
    match result {
        Ok(value) => panic!("期望错误 {code:?}，实际 Ok({value:?})"),
        Err(error) => assert_eq!(error.code, code, "错误码不符：{error}"),
    }
}

fn has_warning(document: &CalibrationDocument, code: CalibrationWarningCode) -> bool {
    document.warnings.iter().any(|warning| warning.code == code)
}

fn minimal_v1(body: &str) -> String {
    format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n{body}"
    )
}

fn minimal_v11(body: &str) -> String {
    format!(
        "schema: tspi-calibration/v1.1\nsample_rate: 48000\nreference_index: 0\nchannels:\n{body}"
    )
}

fn channel_line(name: &str, index: u32, delay: u32, polarity: &str, filters: &str) -> String {
    format!(
        "  - {{name: {name}, index: {index}, delay_samples: {delay}, polarity: {polarity}, filters: {filters}}}\n"
    )
}

// ---------------------------------------------------------------------------
// 1. 解析/校验：示例文件与 v1/v1.1 语义
// ---------------------------------------------------------------------------

#[test]
fn calibration_example_v1_files_parse() {
    let doc = doc_412();
    assert_eq!(doc.schema, CalibrationSchema::V1);
    assert_eq!(doc.schema_string, "tspi-calibration/v1");
    assert_eq!(doc.sample_rate, 48_000);
    assert_eq!(doc.reference_index, 0);
    assert_eq!(doc.layout.as_deref(), Some("4.1.2"));
    assert_eq!(doc.channels.len(), 7);
    assert!(doc.source_sha256.len() == 64);
    assert!(doc.layout_checked);
    let fl = doc.channel("FL").expect("FL");
    assert_eq!(fl.index, 0);
    assert_eq!(fl.delay_samples, 0);
    assert_eq!(fl.polarity, Polarity::Normal);
    assert_eq!(fl.filters.len(), 1);
    assert!((fl.filters[0].band.gain_db - (-4.0)).abs() < 1e-12);
    let sub = doc.channel("SUB1").expect("SUB1");
    assert_eq!(sub.index, 7);
    assert_eq!(sub.delay_samples, 12);
    assert_eq!(doc.delay_max_samples(), 9_600);

    let doc532 = parse_calibration_yaml_with_layout(EXAMPLE_532.as_bytes(), Some(&layout_532()))
        .expect("5.3.2");
    assert_eq!(doc532.channels.len(), 10);
    assert_eq!(doc532.channel("SR").unwrap().polarity, Polarity::Invert);
    assert_eq!(doc532.channel("HFR").unwrap().index, 9);
}

#[test]
fn calibration_example_files_parse_without_layout_table() {
    let doc = parse_calibration_yaml(EXAMPLE_412.as_bytes()).expect("no-layout parse");
    assert!(!doc.layout_checked);
    let doc532 = parse_calibration_yaml(EXAMPLE_532.as_bytes()).expect("no-layout parse 532");
    assert_eq!(doc532.channels.len(), 10);
}

#[test]
fn calibration_v11_shelf_types_preserved() {
    let doc = parse_calibration_yaml_with_layout(EXAMPLE_V11.as_bytes(), Some(&layout_412()))
        .expect("v1.1 example");
    assert_eq!(doc.schema, CalibrationSchema::V11);
    assert_eq!(doc.schema_string, "tspi-calibration/v1.1");
    let fl = doc.channel("FL").expect("FL");
    assert_eq!(fl.filters.len(), 1);
    assert_eq!(fl.filters[0].band.filter_type, PeqType::HighShelf);
    assert!(fl.filters[0].type_explicit);
    assert!((fl.filters[0].band.q - 0.7).abs() < 1e-12);
    let fr = doc.channel("FR").expect("FR");
    // v1.1 不要求 layout 全角色覆盖（partial calibration 合法），但给 layout 表时会 warning。
    assert!(has_warning(
        &doc,
        CalibrationWarningCode::LayoutRoleUncalibrated
    ));
    assert_eq!(fr.polarity, Polarity::Invert);
    // 含 shelf 的 v1.1 不应给 "v1 可读" 提示。
    assert!(!has_warning(&doc, CalibrationWarningCode::V11ReadableByV1));
}

#[test]
fn calibration_v11_peaking_only_is_v1_readable_warning() {
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 1.0, gain_db: -1.0}]",
    );
    let yaml = minimal_v11(&body);
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("v1.1 peaking");
    assert!(has_warning(&doc, CalibrationWarningCode::V11ReadableByV1));
    assert_eq!(
        doc.channels[0].filters[0].band.filter_type,
        PeqType::Peaking
    );
}

#[test]
fn calibration_v11_peaking_q_above_10_warning() {
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 40.0, q: 12.0, gain_db: -3.0}]",
    );
    let yaml = minimal_v11(&body);
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("v1.1 q=12 合法");
    assert!(has_warning(&doc, CalibrationWarningCode::PeakingQHigh));
    assert!(!has_warning(&doc, CalibrationWarningCode::V11ReadableByV1));
    assert!((doc.channels[0].filters[0].band.q - 12.0).abs() < 1e-12);
}

#[test]
fn calibration_v1_rejects_shelf_type() {
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 4000.0, q: 0.7, gain_db: -2.0, type: high_shelf}]",
    );
    let yaml = minimal_v1(&body);
    expect_error(
        parse_calibration_yaml(yaml.as_bytes()),
        CalibrationErrorCode::FilterTypeNotInV1,
    );
}

#[test]
fn calibration_unknown_filter_type_rejected() {
    for (schema_yaml, body) in [
        (
            minimal_v1(&channel_line(
                "FL",
                0,
                0,
                "normal",
                "[{freq: 100.0, q: 1.0, gain_db: 0.0, type: notch}]",
            )),
            (),
        ),
        (
            minimal_v11(&channel_line(
                "FL",
                0,
                0,
                "normal",
                "[{freq: 100.0, q: 1.0, gain_db: 0.0, type: lowpass}]",
            )),
            (),
        ),
    ] {
        let _ = body;
        expect_error(
            parse_calibration_yaml(schema_yaml.as_bytes()),
            CalibrationErrorCode::FilterTypeUnknown,
        );
    }
}

#[test]
fn calibration_unknown_schema_rejected() {
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v2\nsample_rate: 48000\nreference_index: 0\nchannels: []\n",
        ),
        CalibrationErrorCode::Schema,
    );
    expect_error(
        parse_calibration_yaml(b"sample_rate: 48000\nreference_index: 0\nchannels: []\n"),
        CalibrationErrorCode::Schema,
    );
    expect_error(
        parse_calibration_yaml(b"schema: 5\nsample_rate: 48000\n"),
        CalibrationErrorCode::Schema,
    );
}

#[test]
fn calibration_unknown_fields_warn_but_accept() {
    let yaml = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nvendor_block: {{a: 1}}\nchannels:\n{}{}",
        channel_line(
            "FL",
            0,
            0,
            "normal",
            "[{freq: 100.0, q: 1.0, gain_db: -1.0, x_note: hi}]",
        )
        .replace("filters:", "future_field: 1, filters:"),
        ""
    );
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("unknown fields = warning");
    assert!(has_warning(&doc, CalibrationWarningCode::UnknownTopField));
    assert!(has_warning(
        &doc,
        CalibrationWarningCode::UnknownChannelField
    ));
    assert!(has_warning(
        &doc,
        CalibrationWarningCode::UnknownFilterField
    ));
    assert_eq!(doc.channels.len(), 1);
}

#[test]
fn calibration_duplicate_field_warns_last_wins() {
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nsample_rate: 96000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n";
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("duplicate warning");
    assert_eq!(doc.sample_rate, 96_000);
    assert!(doc
        .warnings
        .iter()
        .any(|warning| warning.code == CalibrationWarningCode::DuplicateField));
}

#[test]
fn calibration_sample_rate_rules() {
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000.0\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n",
        ),
        CalibrationErrorCode::SampleRate,
    );
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: -1\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n",
        ),
        CalibrationErrorCode::SampleRate,
    );
    let body = channel_line("FL", 0, 0, "normal", "[]");
    let yaml = format!(
        "schema: tspi-calibration/v1\nsample_rate: 44100\nreference_index: 0\nchannels:\n{body}"
    );
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("44.1k 声明集合内");
    assert_eq!(doc.sample_rate, 44_100);
    assert!(!has_warning(
        &doc,
        CalibrationWarningCode::SampleRateNonDeclared
    ));
    let yaml32 = format!(
        "schema: tspi-calibration/v1\nsample_rate: 32000\nreference_index: 0\nchannels:\n{body}"
    );
    let doc32 = parse_calibration_yaml(yaml32.as_bytes()).expect("32k 仅 warning");
    assert!(has_warning(
        &doc32,
        CalibrationWarningCode::SampleRateNonDeclared
    ));
}

#[test]
fn calibration_reference_index_rules() {
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n",
        ),
        CalibrationErrorCode::MissingField,
    );
    let body = channel_line("FL", 0, 0, "normal", "[]");
    let bad_ref = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 9\nchannels:\n{body}"
    );
    expect_error(
        parse_calibration_yaml(bad_ref.as_bytes()),
        CalibrationErrorCode::ReferenceIndex,
    );
    // reference 指向 delay≠0 的通道：warning。
    let body2 = format!(
        "{}{}",
        channel_line("FL", 0, 0, "normal", "[]"),
        channel_line("FR", 1, 3, "normal", "[]")
    );
    let yaml = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 1\nchannels:\n{body2}"
    );
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("reference nonzero warning");
    assert!(has_warning(
        &doc,
        CalibrationWarningCode::ReferenceNotZeroDelay
    ));
}

#[test]
fn calibration_delay_rules() {
    let cases = [
        (
            channel_line("FL", 0, 0, "normal", "[]")
                .replace("delay_samples: 0", "delay_samples: -1"),
            CalibrationErrorCode::Delay,
        ),
        (
            channel_line("FL", 0, 0, "normal", "[]")
                .replace("delay_samples: 0", "delay_samples: 1.5"),
            CalibrationErrorCode::Delay,
        ),
        (
            channel_line("FL", 0, 0, "normal", "[]")
                .replace("delay_samples: 0", "delay_samples: 9601"),
            CalibrationErrorCode::Delay,
        ),
    ];
    for (body, code) in cases {
        let yaml = minimal_v1(&body);
        expect_error(parse_calibration_yaml(yaml.as_bytes()), code);
    }
    // min != 0 -> 归一化错误。
    let body = format!(
        "{}{}",
        channel_line("FL", 0, 2, "normal", "[]"),
        channel_line("FR", 1, 5, "normal", "[]")
    );
    let yaml = minimal_v1(&body);
    expect_error(
        parse_calibration_yaml(yaml.as_bytes()),
        CalibrationErrorCode::DelayNormalization,
    );
    // 上限恰为 9600 可接受（另有 FL=0 保证 min=0）。
    let body = format!(
        "{}{}",
        channel_line("FL", 0, 0, "normal", "[]"),
        channel_line("SUB1", 1, 9600, "normal", "[]")
    );
    let yaml = minimal_v1(&body);
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("9600 = floor(0.2fs) 合法");
    assert_eq!(doc.channel("SUB1").unwrap().delay_samples, 9_600);
}

#[test]
fn calibration_polarity_rules() {
    let body = channel_line("FL", 0, 0, "invert", "[]");
    let doc = parse_calibration_yaml(minimal_v1(&body).as_bytes()).expect("invert");
    assert_eq!(doc.channels[0].polarity, Polarity::Invert);
    assert_eq!(Polarity::Invert.multiplier(), -1);
    let body = channel_line("FL", 0, 0, "1", "[]");
    let doc = parse_calibration_yaml(minimal_v1(&body).as_bytes()).expect("alias 1");
    assert_eq!(doc.channels[0].polarity, Polarity::Normal);
    assert!(doc.channels[0].polarity_alias);
    assert!(has_warning(&doc, CalibrationWarningCode::PolarityAlias));
    let body = channel_line("FL", 0, 0, "-1", "[]");
    let doc = parse_calibration_yaml(minimal_v1(&body).as_bytes()).expect("alias -1");
    assert_eq!(doc.channels[0].polarity, Polarity::Invert);
    let body = channel_line("FL", 0, 0, "flip", "[]");
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::Polarity,
    );
    // 缺省 polarity -> warning + normal。
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, filters: []}\n";
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("polarity default");
    assert_eq!(doc.channels[0].polarity, Polarity::Normal);
    assert!(has_warning(&doc, CalibrationWarningCode::PolarityDefault));
    assert_eq!(Polarity::Normal.xor(Polarity::Normal), Polarity::Normal);
    assert_eq!(Polarity::Normal.xor(Polarity::Invert), Polarity::Invert);
    assert_eq!(Polarity::Invert.xor(Polarity::Invert), Polarity::Normal);
}

#[test]
fn calibration_gain_rules() {
    let body =
        channel_line("FL", 0, 0, "normal", "[]").replace(", polarity", ", gain_db: 7.0, polarity");
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::TrimGain,
    );
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 1.0, gain_db: -25.0}]",
    );
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::FilterGain,
    );
}

#[test]
fn calibration_channels_rules() {
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels: []\n",
        ),
        CalibrationErrorCode::Channels,
    );
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\n",
        ),
        CalibrationErrorCode::MissingField,
    );
    let mut body = String::new();
    for index in 0..33u32 {
        let name = format!("CH{index}");
        body.push_str(&channel_line(&name, index, 0, "normal", "[]"));
    }
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::Channels,
    );
    let body = format!(
        "{}{}",
        channel_line("FL", 0, 0, "normal", "[]"),
        channel_line("FL", 1, 0, "normal", "[]")
    );
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::ChannelName,
    );
    let body = format!(
        "{}{}",
        channel_line("FL", 0, 0, "normal", "[]"),
        channel_line("FR", 0, 0, "normal", "[]")
    );
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::ChannelIndex,
    );
    let body = channel_line("fl", 0, 0, "normal", "[]");
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::ChannelName,
    );
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - [1, 2]\n";
    expect_error(
        parse_calibration_yaml(yaml.as_bytes()),
        CalibrationErrorCode::FieldType,
    );
}

#[test]
fn calibration_filters_required_and_count() {
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal}\n";
    expect_error(
        parse_calibration_yaml(yaml.as_bytes()),
        CalibrationErrorCode::Filters,
    );
    let mut filters = String::from("[");
    for index in 0..33 {
        if index > 0 {
            filters.push(',');
        }
        filters.push_str(&format!(
            "{{freq: {}, q: 1.0, gain_db: -1.0}}",
            20.0 + index as f64
        ));
    }
    filters.push(']');
    let body = channel_line("FL", 0, 0, "normal", &filters);
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::FilterCount,
    );
    let body = channel_line("FL", 0, 0, "normal", "[]");
    let doc = parse_calibration_yaml(minimal_v1(&body).as_bytes()).expect("empty filters ok");
    assert!(doc.channels[0].filters.is_empty());
}

#[test]
fn calibration_filter_freq_rules() {
    let cases = [19.0f64, 20_001.0, 24_000.0];
    for freq in cases {
        let body = channel_line(
            "FL",
            0,
            0,
            "normal",
            &format!("[{{freq: {freq}, q: 1.0, gain_db: -1.0}}]"),
        );
        expect_error(
            parse_calibration_yaml(minimal_v1(&body).as_bytes()),
            CalibrationErrorCode::FilterFreq,
        );
    }
    for freq in [20.0f64, 20_000.0] {
        let body = channel_line(
            "FL",
            0,
            0,
            "normal",
            &format!("[{{freq: {freq}, q: 1.0, gain_db: -1.0}}]"),
        );
        let doc = parse_calibration_yaml(minimal_v1(&body).as_bytes())
            .unwrap_or_else(|error| panic!("freq={freq} 应接受：{error}"));
        assert_eq!(doc.channels[0].filters.len(), 1);
    }
}

#[test]
fn calibration_filter_q_rules() {
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 12.0, gain_db: -1.0}]",
    );
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::FilterQ,
    );
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 0.05, gain_db: -1.0}]",
    );
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::FilterQ,
    );
    // v1.1 shelf q 是 slope S，必须在 [0.1,1.0]。
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 1.5, gain_db: -1.0, type: low_shelf}]",
    );
    expect_error(
        parse_calibration_yaml(minimal_v11(&body).as_bytes()),
        CalibrationErrorCode::FilterQ,
    );
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 0.7, gain_db: -1.0, type: low_shelf}]",
    );
    let doc = parse_calibration_yaml(minimal_v11(&body).as_bytes()).expect("shelf S=0.7");
    assert_eq!(
        doc.channels[0].filters[0].band.filter_type,
        PeqType::LowShelf
    );
}

#[test]
fn calibration_positive_boost_rules() {
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 1.0, gain_db: 7.0}, {freq: 200.0, q: 1.0, gain_db: 6.0}]",
    );
    expect_error(
        parse_calibration_yaml(minimal_v1(&body).as_bytes()),
        CalibrationErrorCode::PositiveBoost,
    );
    let body = channel_line(
        "FL",
        0,
        0,
        "normal",
        "[{freq: 100.0, q: 1.0, gain_db: 4.0}, {freq: 200.0, q: 1.0, gain_db: 3.0}]",
    );
    let doc = parse_calibration_yaml(minimal_v1(&body).as_bytes()).expect("7dB warning");
    assert!(has_warning(&doc, CalibrationWarningCode::PositiveBoostWarn));
}

#[test]
fn calibration_playback_channels_bound() {
    let body = channel_line("FL", 0, 0, "normal", "[]");
    let yaml = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nmeasurement:\n  playback: {{device: \"hw:motu,0\", channels: 4}}\nchannels:\n{}{}",
        body,
        channel_line("SUB1", 7, 0, "normal", "[]")
    );
    expect_error(
        parse_calibration_yaml(yaml.as_bytes()),
        CalibrationErrorCode::ChannelIndex,
    );
}

#[test]
fn calibration_measurement_rules() {
    let body = channel_line("FL", 0, 0, "normal", "[]");
    let bad_mode = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nmeasurement:\n  mode: magic\nchannels:\n{body}"
    );
    expect_error(
        parse_calibration_yaml(bad_mode.as_bytes()),
        CalibrationErrorCode::Measurement,
    );
    let through_no_offsets = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nmeasurement:\n  mode: through_render\nchannels:\n{body}"
    );
    expect_error(
        parse_calibration_yaml(through_no_offsets.as_bytes()),
        CalibrationErrorCode::Measurement,
    );
    let offsets_unknown_name = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nmeasurement:\n  mode: through_render\n  render_offsets:\n    - {{name: ZZ, delay_samples: 1, polarity: normal}}\nchannels:\n{body}"
    );
    let doc = parse_calibration_yaml(offsets_unknown_name.as_bytes()).expect("offset warning");
    assert!(has_warning(
        &doc,
        CalibrationWarningCode::UnknownMeasurementField
    ));
    let unknown_meas_field = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nmeasurement:\n  mic_position: xyz\nchannels:\n{body}"
    );
    let doc = parse_calibration_yaml(unknown_meas_field.as_bytes()).expect("meas unknown warning");
    assert!(has_warning(
        &doc,
        CalibrationWarningCode::UnknownMeasurementField
    ));
}

#[test]
fn calibration_metadata_rules() {
    let body = channel_line("FL", 0, 0, "normal", "[]");
    let tool_string = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\ntool: tspi-calibrate\nversion: \"0.1.0\"\nchannels:\n{body}"
    );
    let doc = parse_calibration_yaml(tool_string.as_bytes()).expect("tool string");
    assert_eq!(doc.tool.as_ref().unwrap().name, "tspi-calibrate");
    assert_eq!(doc.tool_version.as_deref(), Some("0.1.0"));
    let tool_bad = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\ntool: {{version: \"0.1.0\"}}\nchannels:\n{body}"
    );
    expect_error(
        parse_calibration_yaml(tool_bad.as_bytes()),
        CalibrationErrorCode::Metadata,
    );
    let created_bad = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\ncreated_unix: -5\nchannels:\n{body}"
    );
    expect_error(
        parse_calibration_yaml(created_bad.as_bytes()),
        CalibrationErrorCode::Metadata,
    );
    let long_note = "n".repeat(501);
    let note_yaml = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nnote: \"{long_note}\"\nchannels:\n{body}"
    );
    let doc = parse_calibration_yaml(note_yaml.as_bytes()).expect("long note warning");
    assert!(has_warning(&doc, CalibrationWarningCode::NoteTooLong));
}

#[test]
fn calibration_yaml_syntax_and_unsupported_features() {
    // TAB 缩进：不支持。
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n\t- {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n",
        ),
        CalibrationErrorCode::YamlUnsupported,
    );
    // anchor/tag/block scalar/multi-doc：明确不支持。
    for text in [
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nnote: &a hello\nchannels: []\n",
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nnote: !tag hello\nchannels: []\n",
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nnote: |\n  hello\nchannels: []\n",
        "---\nschema: tspi-calibration/v1\n---\nschema: spi\n",
    ] {
        expect_error(
            parse_calibration_yaml(text.as_bytes()),
            CalibrationErrorCode::YamlUnsupported,
        );
    }
    // 未闭合引号：不支持（多行字符串）。
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nnote: \"abc\nchannels: []\n",
        ),
        CalibrationErrorCode::YamlUnsupported,
    );
    // 顶层是序列：YamlRoot。
    expect_error(
        parse_calibration_yaml(b"- a\n- b\n"),
        CalibrationErrorCode::YamlRoot,
    );
    // 空文档：YamlRoot。
    expect_error(
        parse_calibration_yaml(b"# only comment\n\n"),
        CalibrationErrorCode::YamlRoot,
    );
    // 非法缩进（mapping 中缩进变深却不属于上一键）。
    expect_error(
        parse_calibration_yaml(
            b"schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n   extra: 1\n",
        ),
        CalibrationErrorCode::YamlSyntax,
    );
}

#[test]
fn calibration_yaml_flow_and_quoted_scalars() {
    let yaml = "schema: \"tspi-calibration/v1\"\nsample_rate: 48000\nreference_index: 0\nlayout: \"4.1.2\"\nnote: \"a: b, c # not comment\"  # real comment\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: [{freq: 100.0, q: 1.0, gain_db: -1.0}]}\n  - name: SUB1\n    index: 7\n    delay_samples: 12\n    polarity: invert\n    filters: []\n";
    let doc = parse_calibration_yaml_with_layout(yaml.as_bytes(), Some(&layout_412()))
        .expect("flow+block mixed");
    assert_eq!(doc.layout.as_deref(), Some("4.1.2"));
    assert_eq!(doc.note.as_deref(), Some("a: b, c # not comment"));
    assert_eq!(doc.channels.len(), 2);
    assert_eq!(doc.channels[1].filters.len(), 0);
}

#[test]
fn calibration_layout_cross_check_rules() {
    // layout 数字不匹配 -> LayoutMismatch。
    expect_error(
        parse_calibration_yaml_with_layout(EXAMPLE_412.as_bytes(), Some(&layout_532())),
        CalibrationErrorCode::LayoutMismatch,
    );
    // 缺 layout 字段但给了角色表 -> LayoutMismatch。
    let no_layout = EXAMPLE_412.replace("layout: \"4.1.2\"\n", "");
    expect_error(
        parse_calibration_yaml_with_layout(no_layout.as_bytes(), Some(&layout_412())),
        CalibrationErrorCode::LayoutMismatch,
    );
    // index 与 layout 映射不一致。
    let bad_index = EXAMPLE_412.replace(
        "{name: SUB1, index: 7, delay_samples: 12",
        "{name: SUB1, index: 6, delay_samples: 12",
    );
    expect_error(
        parse_calibration_yaml_with_layout(bad_index.as_bytes(), Some(&layout_412())),
        CalibrationErrorCode::LayoutIndexMismatch,
    );
    // 角色不在 layout -> LayoutRoleUnknown。
    let bad_role = format!(
        "{}{}",
        channel_line("FL", 0, 0, "normal", "[]"),
        channel_line("C", 2, 0, "normal", "[]")
    );
    let yaml = format!(
        "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nlayout: \"4.1.2\"\nchannels:\n{bad_role}"
    );
    expect_error(
        parse_calibration_yaml_with_layout(yaml.as_bytes(), Some(&layout_412())),
        CalibrationErrorCode::LayoutRoleUnknown,
    );
}

#[test]
fn calibration_source_sha256_and_known_vectors() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let doc = parse_calibration_yaml(EXAMPLE_412.as_bytes()).expect("parse");
    assert_eq!(doc.source_sha256, sha256_hex(EXAMPLE_412.as_bytes()));
    // 任何字节变化 source hash 必须变化（stale 依据）。
    let mut changed = EXAMPLE_412.as_bytes().to_vec();
    changed.push(b'\n');
    let doc2 = parse_calibration_yaml(&changed).expect("parse changed");
    assert_ne!(doc.source_sha256, doc2.source_sha256);
}

#[test]
fn calibration_error_and_warning_code_catalogs() {
    assert_eq!(CalibrationErrorCode::ALL.len(), 59);
    let mut literals: Vec<&str> = CalibrationErrorCode::ALL
        .iter()
        .map(|code| code.as_str())
        .collect();
    literals.sort_unstable();
    literals.dedup();
    assert_eq!(literals.len(), 59, "错误码字面值必须唯一");
    for code in CalibrationErrorCode::ALL {
        let text = code.as_str();
        assert!(
            text.starts_with("E_CAL_") || text.starts_with("E_IR_"),
            "错误码前缀非法：{text}"
        );
    }
    assert_eq!(CalibrationWarningCode::ALL.len(), 17);
    let mut warning_literals: Vec<&str> = CalibrationWarningCode::ALL
        .iter()
        .map(|code| code.as_str())
        .collect();
    warning_literals.sort_unstable();
    warning_literals.dedup();
    assert_eq!(warning_literals.len(), 17);
    for code in CalibrationWarningCode::ALL {
        assert!(code.as_str().starts_with("W_CAL_"));
    }
}

// ---------------------------------------------------------------------------
// 2. 合并：CONTRACT §4.1（delay 相加 / polarity XOR / gain 相加 / PEQ 追加）
// ---------------------------------------------------------------------------

#[test]
fn calibration_merge_contract_412() {
    let merged = merged_412();
    assert_eq!(merged.schema, CalibrationSchema::V1);
    assert_eq!(merged.sample_rate, 48_000);
    assert_eq!(merged.layout_id.as_deref(), Some("4.1.2"));
    assert_eq!(merged.roles.len(), 7);
    let fl = merged.roles.iter().find(|role| role.name == "FL").unwrap();
    assert!(fl.calibrated);
    // FL: base delay 0 + cal 0；base gain -1 + cal 0（示例 FL 无 trim） -> -1；PEQ 0+1。
    assert_eq!(fl.delay_samples, 0);
    assert!((fl.gain_db - (-1.0)).abs() < 1e-12);
    assert_eq!(fl.peq.len(), 1);
    assert_eq!(fl.peq[0].filter_type, PeqType::Peaking);
    // FR: base delay 1 + cal 2 = 3；polarity normal。
    let fr = merged.roles.iter().find(|role| role.name == "FR").unwrap();
    assert_eq!(fr.delay_samples, 3);
    // SR: 示例 normal；HFR: 基线 invert XOR normal = invert。
    let hfr = merged.roles.iter().find(|role| role.name == "HFR").unwrap();
    assert_eq!(hfr.polarity, Polarity::Invert);
    // SL: 基线 peq 1 段 + 示例 0 段 = 1 段（基线保留）。
    let sl = merged.roles.iter().find(|role| role.name == "SL").unwrap();
    assert_eq!(sl.peq.len(), 1);
    assert!((sl.peq[0].freq_hz - 200.0).abs() < 1e-12);
    // SUB1: 示例 delay 12；PEQ 1 段。
    let sub = merged
        .roles
        .iter()
        .find(|role| role.name == "SUB1")
        .unwrap();
    assert_eq!(sub.delay_samples, 12);
    assert_eq!(sub.peq.len(), 1);
    assert_eq!(sub.peq[0].filter_type, PeqType::Peaking);
    // 顺序按基线角色数组顺序（FL FR SL SR HFL HFR SUB1）。
    assert_eq!(merged.roles[0].name, "FL");
    assert_eq!(merged.roles[6].name, "SUB1");
}

#[test]
fn calibration_merge_polarity_xor_invert() {
    let doc = doc_412();
    let mut base = base_roles_412();
    // 基线 SR 反相 + 校准 SR normal = invert。
    base[3].polarity = Polarity::Invert;
    let merged = merge_calibration(&doc, Some(&layout_412()), &base, 48_000).expect("merge");
    let sr = merged.roles.iter().find(|role| role.name == "SR").unwrap();
    assert_eq!(sr.polarity, Polarity::Invert);
    // 基线 FR 反相 + 校准 FR normal = invert。
    base[1].polarity = Polarity::Invert;
    let merged = merge_calibration(&doc, Some(&layout_412()), &base, 48_000).expect("merge2");
    let fr = merged.roles.iter().find(|role| role.name == "FR").unwrap();
    assert_eq!(fr.polarity, Polarity::Invert);
}

#[test]
fn calibration_merge_preserves_peq_types_and_order() {
    let yaml = "schema: tspi-calibration/v1.1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: [{freq: 4000.0, q: 0.7, gain_db: -2.0, type: high_shelf}]}\n";
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("v1.1");
    let mut base = vec![RenderRoleBase::uncalibrated("FL", 0)];
    base[0].peq = vec![
        PeqBand::new(100.0, 1.0, -1.0, PeqType::LowShelf),
        PeqBand::new(200.0, 2.0, -2.0, PeqType::Peaking),
    ];
    let merged = merge_calibration(&doc, None, &base, 48_000).expect("merge");
    let fl = &merged.roles[0];
    assert_eq!(fl.peq.len(), 3);
    assert_eq!(fl.peq[0].filter_type, PeqType::LowShelf);
    assert_eq!(fl.peq[1].filter_type, PeqType::Peaking);
    assert_eq!(fl.peq[2].filter_type, PeqType::HighShelf);
    assert!((fl.peq[2].q - 0.7).abs() < 1e-12);
}

#[test]
fn calibration_merge_rejects_role_not_found() {
    let doc = doc_412();
    let mut base = base_roles_412();
    base.retain(|role| role.name != "SL");
    expect_error(
        merge_calibration(&doc, Some(&layout_412()), &base, 48_000),
        CalibrationErrorCode::RoleNotFound,
    );
}

#[test]
fn calibration_merge_rejects_index_mismatch() {
    let doc = doc_412();
    let mut base = base_roles_412();
    base[0].index = 9;
    expect_error(
        merge_calibration(&doc, None, &base, 48_000),
        CalibrationErrorCode::RoleIndexMismatch,
    );
}

#[test]
fn calibration_merge_rejects_double_apply() {
    let doc = doc_412();
    let mut base = base_roles_412();
    base[0].origin = RoleSpecOrigin::CalibrationApplied;
    expect_error(
        merge_calibration(&doc, None, &base, 48_000),
        CalibrationErrorCode::DoubleApply,
    );
}

#[test]
fn calibration_merge_rejects_fs_mismatch() {
    let doc = doc_412();
    expect_error(
        merge_calibration(&doc, None, &base_roles_412(), 96_000),
        CalibrationErrorCode::FsMismatch,
    );
}

#[test]
fn calibration_merge_limits() {
    let doc = doc_412();
    // 基线 delay 9500 + 校准 SUB1 12 = 9512 <= 9600（合法），再加到超限。
    let mut base = base_roles_412();
    let sub = base.iter_mut().find(|role| role.name == "SUB1").unwrap();
    sub.delay_samples = 9_600;
    expect_error(
        merge_calibration(&doc, None, &base, 48_000),
        CalibrationErrorCode::DelayOverflow,
    );
    // gain 合并越界（channelmap [-60,+12]）。
    let mut base = base_roles_412();
    let fl = base.iter_mut().find(|role| role.name == "FL").unwrap();
    fl.gain_db = 11.0;
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, gain_db: 4.0, filters: []}\n";
    let doc_gain = parse_calibration_yaml(yaml.as_bytes()).expect("parse trim");
    expect_error(
        merge_calibration(&doc_gain, None, &base, 48_000),
        CalibrationErrorCode::MergedGainRange,
    );
    // PEQ 合并超 32 段。
    let mut base = base_roles_412();
    let fl = base.iter_mut().find(|role| role.name == "FL").unwrap();
    fl.peq = (0..32)
        .map(|index| PeqBand::new(20.0 + index as f64, 1.0, -1.0, PeqType::Peaking))
        .collect();
    expect_error(
        merge_calibration(&doc, None, &base, 48_000),
        CalibrationErrorCode::PeqOverflow,
    );
    // 基线非法：重复 name。
    let mut bad_base = base_roles_412();
    bad_base.push(RenderRoleBase::uncalibrated("FL", 100));
    expect_error(
        merge_calibration(&doc, None, &bad_base, 48_000),
        CalibrationErrorCode::BaseRoleInvalid,
    );
}

#[test]
fn calibration_merge_partial_coverage_warning() {
    let doc = doc_412();
    let base = vec![
        RenderRoleBase::uncalibrated("FL", 0),
        RenderRoleBase::uncalibrated("FR", 1),
    ];
    // 校准文件含 7 个角色，其中 5 个不在 base -> RoleNotFound（先报缺失）。
    expect_error(
        merge_calibration(&doc, None, &base, 48_000),
        CalibrationErrorCode::RoleNotFound,
    );
    // 只校准 FL 的小文件 + base 两角色 -> 未覆盖 FR warning。
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n";
    let small = parse_calibration_yaml(yaml.as_bytes()).expect("small");
    let merged = merge_calibration(&small, None, &base, 48_000).expect("partial");
    assert!(merged
        .warnings
        .iter()
        .any(|warning| warning.code == CalibrationWarningCode::RoleUncalibrated));
}

// ---------------------------------------------------------------------------
// 3. 内部 IR 草案：version/hash/role/stale/placement/P/phase/D
// ---------------------------------------------------------------------------

#[test]
fn calibration_ir_build_basic() {
    let ir = ir_412_plain();
    assert_eq!(ir.schema(), "tspi-cal-ir-draft/v1");
    assert_eq!(ir.ir_version(), 1);
    assert_eq!(ir.hash_algo(), "tspi-cal-ir-canonical-v1");
    assert_eq!(ir.ir_hash().len(), 64);
    assert_eq!(ir.created_unix(), 1_800_000_000);
    assert_eq!(ir.source().schema, CalibrationSchema::V1);
    assert_eq!(ir.source().schema_string, "tspi-calibration/v1");
    assert_eq!(ir.source().sample_rate, 48_000);
    assert!(ir.source().legacy_v1);
    assert_eq!(
        ir.source().source_sha256,
        sha256_hex(EXAMPLE_412.as_bytes())
    );
    assert_eq!(ir.layout_id(), Some("4.1.2"));
    assert_eq!(ir.profile().profile_id, "target-4.1.2");
    assert_eq!(ir.roles().len(), 7);
    assert_eq!(ir.post().len(), 7);
    let fl = ir.post_role("FL").expect("FL");
    assert_eq!(fl.index, 0);
    assert_eq!(fl.delay_samples, 0);
    assert_eq!(fl.delay_q16, 0);
    assert_eq!(fl.polarity, Polarity::Normal);
    assert!((fl.gain_db - 0.0).abs() < 1e-12);
    assert_eq!(fl.peq.len(), 1);
    assert_eq!(
        fl.placement,
        vec![PostStage::ModalPeq, PostStage::Gain, PostStage::Protection]
    );
    assert_eq!(fl.p_taps, 1);
    let sub = ir.post_role("SUB1").expect("SUB1");
    assert_eq!(sub.delay_samples, 12);
    assert_eq!(sub.p_taps, 13);
    assert_eq!(sub.placement[0], PostStage::ModalPeq);
    assert_eq!(sub.placement[1], PostStage::Gain);
    assert_eq!(sub.placement[2], PostStage::Delay);
    assert_eq!(sub.placement[3], PostStage::Protection);
    assert_eq!(ir.curve_application_count(), 0);
    assert!(ir.curve().is_none());
    assert_eq!(
        ir.g_residual().participants,
        vec!["FL", "FR", "SL", "SR", "SUB1"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
    assert_eq!(ir.g_residual().rule, GResidualRule::ModalResidualOnly);
    assert!(ir.g_residual().curve_id().is_none());
    ir.validate_curve_single_application()
        .expect("curve single");
    assert_eq!(ir.encoding(), PostEncoding::CoeffEmbedded);
    assert!(ir.extensions().is_empty());
}

#[test]
fn calibration_ir_hash_deterministic_and_semantic() {
    let merged = merged_412_plain();
    let mut builder_a = IrDraftBuilder::new(&merged, profile_412(), 0).expect("a");
    builder_a.with_curve(valid_curve_412()).expect("curve a");
    let ir_a = builder_a.build().expect("ir a");
    let mut builder_b = IrDraftBuilder::new(&merged, profile_412(), 123_456).expect("b");
    builder_b.with_curve(valid_curve_412()).expect("curve b");
    let ir_b = builder_b.build().expect("ir b");
    // created_unix 不参与 hash；canonical form 包含它。
    assert_eq!(ir_a.ir_hash(), ir_b.ir_hash());
    assert_ne!(ir_a.canonical_form(), ir_b.canonical_form());
    assert_eq!(ir_a.canonical_hash_input(), ir_b.canonical_hash_input());
    assert!(ir_a.canonical_form().contains("\"created_unix\":0"));
    assert!(!ir_a.canonical_form().contains(": 1"));
    // 语义字段变化 -> hash 变化。
    let mut base = base_roles_412();
    base[0].gain_db = -6.0;
    let merged2 = merge_calibration(&doc_412(), Some(&layout_412()), &base, 48_000).expect("merge");
    let mut builder_c = IrDraftBuilder::new(&merged2, profile_412(), 0).expect("c");
    builder_c.with_curve(valid_curve_412()).expect("curve c");
    let ir_c = builder_c.build().expect("ir c");
    assert_ne!(ir_a.ir_hash(), ir_c.ir_hash());
    // profile hash 变化 -> hash 变化。
    let mut profile2 = profile_412();
    profile2.profile_sha256 = sha_b();
    let mut builder_d = IrDraftBuilder::new(&merged, profile2, 0).expect("d");
    builder_d.with_curve(valid_curve_412()).expect("curve d");
    let ir_d = builder_d.build().expect("ir d");
    assert_ne!(ir_a.ir_hash(), ir_d.ir_hash());
    // 无 curve 与有 curve 的 hash 不同（curve 绑定进 hash）。
    let ir_no_curve = ir_412_plain();
    assert_ne!(ir_no_curve.ir_hash(), ir_a.ir_hash());
}

#[test]
fn calibration_ir_extension_fields_rules() {
    let merged = merged_412_plain();
    let mut builder = IrDraftBuilder::new(&merged, profile_412(), 0).expect("builder");
    expect_error(
        builder.with_extension("vendor", "x"),
        CalibrationErrorCode::IrFieldUnknown,
    );
    builder.with_extension("x_vendor", "acme").expect("x_ ok");
    expect_error(
        builder.with_extension("x_vendor", "other"),
        CalibrationErrorCode::IrExtensionDuplicate,
    );
    let mut builder_a = IrDraftBuilder::new(&merged, profile_412(), 0).expect("a");
    builder_a.with_extension("x_b", "2").expect("b");
    builder_a.with_extension("x_a", "1").expect("a");
    let mut builder_b = IrDraftBuilder::new(&merged, profile_412(), 0).expect("b");
    builder_b.with_extension("x_a", "1").expect("a");
    builder_b.with_extension("x_b", "2").expect("b");
    // 扩展按 key 排序进入 hash：插入顺序不影响 ir_hash。
    assert_eq!(
        builder_a.build().unwrap().ir_hash(),
        builder_b.build().unwrap().ir_hash()
    );
    let mut builder = IrDraftBuilder::new(&merged, profile_412(), 0).expect("c");
    builder.with_extension("x_note", "hello").expect("x_note");
    builder.with_extension("x_note2", "world").expect("x_note2");
    let ir = builder.build().expect("build");
    assert_eq!(ir.extensions().len(), 2);
    assert!(ir.canonical_form().contains("\"x_note\""));
}

#[test]
fn calibration_ir_profile_and_stereo_mask_rules() {
    let merged = merged_412_plain();
    let mut bad_id = profile_412();
    bad_id.profile_id = "  ".into();
    expect_error(
        IrDraftBuilder::new(&merged, bad_id, 0),
        CalibrationErrorCode::IrProfile,
    );
    let mut bad_sha = profile_412();
    bad_sha.profile_sha256 = "zz".into();
    expect_error(
        IrDraftBuilder::new(&merged, bad_sha, 0),
        CalibrationErrorCode::IrProfile,
    );
    let mut bad_target = profile_412();
    bad_target.target_profile = String::new();
    expect_error(
        IrDraftBuilder::new(&merged, bad_target, 0),
        CalibrationErrorCode::IrProfile,
    );
    // stereo_strict 必须恰为 FL/FR。
    let mut bad_strict = profile_stereo_strict();
    bad_strict.stereo_mask = vec!["FL".into()];
    expect_error(
        IrDraftBuilder::new(&merged, bad_strict, 0),
        CalibrationErrorCode::IrStereoMask,
    );
    // 非 strict mask 角色必须在 role 表内。
    let mut bad_mask = profile_412();
    bad_mask.stereo_mask = vec!["ZZ".into()];
    expect_error(
        IrDraftBuilder::new(&merged, bad_mask, 0),
        CalibrationErrorCode::IrStereoMask,
    );
    // 合法 strict 场景。
    let ir = IrDraftBuilder::new(&merged, profile_stereo_strict(), 0)
        .expect("strict ok")
        .build()
        .expect("build");
    assert!(ir.profile().stereo_strict);
    assert_eq!(ir.profile().stereo_mask, vec!["FL", "FR"]);
}

#[test]
fn calibration_ir_rate_unsupported_rejected() {
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 44100\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n";
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("44.1k parse ok");
    let base = vec![RenderRoleBase::uncalibrated("FL", 0)];
    let merged = merge_calibration(&doc, None, &base, 44_100).expect("merge 44.1k");
    expect_error(
        IrDraftBuilder::new(&merged, profile_412(), 0),
        CalibrationErrorCode::IrRateUnsupported,
    );
}

#[test]
fn calibration_ir_stale_reasons() {
    let mut builder = IrDraftBuilder::new(&merged_412_plain(), profile_412(), 0).expect("builder");
    builder.with_curve(valid_curve_412()).expect("curve");
    let ir = builder.build().expect("ir");
    let roles: Vec<(&str, u32)> = ir
        .roles()
        .iter()
        .map(|role| (role.name.as_str(), role.index))
        .collect();
    let profile_sha = ir.profile().profile_sha256.clone();
    let target_sha = ir.curve().unwrap().target_sha256.clone();
    let cal_sha = ir.source().source_sha256.clone();
    let current = IrCurrent {
        fs_hz: 48_000,
        layout_id: Some("4.1.2"),
        role_indices: &roles,
        profile_sha256: &profile_sha,
        target_sha256: Some(&target_sha),
        calibration_sha256: &cal_sha,
    };
    assert!(ir.stale_reasons(&current).is_empty());
    assert!(!ir.is_stale(&current));
    // 逐项变化。
    let mut fs_changed = current;
    fs_changed.fs_hz = 96_000;
    assert_eq!(ir.stale_reasons(&fs_changed), vec![StaleReason::Fs]);
    let mut layout_changed = current;
    layout_changed.layout_id = Some("5.3.2");
    assert_eq!(ir.stale_reasons(&layout_changed), vec![StaleReason::Layout]);
    let mut role_pair = roles[0];
    role_pair.1 = 5;
    let bad_roles = vec![role_pair];
    let mut roles_changed = current;
    roles_changed.role_indices = &bad_roles;
    assert_eq!(ir.stale_reasons(&roles_changed), vec![StaleReason::Roles]);
    let other_profile = sha_c();
    let mut profile_changed = current;
    profile_changed.profile_sha256 = &other_profile;
    assert_eq!(
        ir.stale_reasons(&profile_changed),
        vec![StaleReason::Profile]
    );
    let other_target = sha_b();
    let mut target_changed = current;
    target_changed.target_sha256 = Some(&other_target);
    assert_eq!(ir.stale_reasons(&target_changed), vec![StaleReason::Target]);
    let mut target_removed = current;
    target_removed.target_sha256 = None;
    assert_eq!(ir.stale_reasons(&target_removed), vec![StaleReason::Target]);
    let other_cal = sha_c();
    let mut cal_changed = current;
    cal_changed.calibration_sha256 = &other_cal;
    assert_eq!(
        ir.stale_reasons(&cal_changed),
        vec![StaleReason::Calibration]
    );
    // 全变 -> 全原因按固定顺序。
    let mut all_changed = current;
    all_changed.fs_hz = 96_000;
    all_changed.layout_id = None;
    all_changed.role_indices = &bad_roles;
    all_changed.profile_sha256 = &other_profile;
    all_changed.target_sha256 = None;
    all_changed.calibration_sha256 = &other_cal;
    assert_eq!(
        ir.stale_reasons(&all_changed),
        vec![
            StaleReason::Fs,
            StaleReason::Layout,
            StaleReason::Roles,
            StaleReason::Profile,
            StaleReason::Target,
            StaleReason::Calibration
        ]
    );
    for reason in [
        StaleReason::Fs,
        StaleReason::Layout,
        StaleReason::Roles,
        StaleReason::Profile,
        StaleReason::Target,
        StaleReason::Calibration,
    ] {
        assert!(reason.as_str().starts_with("STALE_"));
    }
}

// ---------------------------------------------------------------------------
// 4. T11 目标曲线数据合同 + curve 不双叠
// ---------------------------------------------------------------------------

#[test]
fn calibration_curve_source_adapter() {
    let curve = TargetCurveRef::from_source(&mock_curve(48_000.0)).expect("valid");
    assert_eq!(curve.curve_id, "tilt10-2026-11-17");
    assert_eq!(curve.fs_hz, 48_000.0);
    assert_eq!(curve.phase_ref, CalPhaseRef::None);
    assert_eq!(curve.target_delay_samples, None);
    assert_eq!(curve.point_count, 480);
    // 未知 phase_ref -> 拒收。
    let mut bad = mock_curve(48_000.0);
    bad.phase_ref = Some("auto".into());
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurvePhase,
    );
    bad.phase_ref = None;
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurvePhase,
    );
    // hash 非 64 hex。
    let mut bad = mock_curve(48_000.0);
    bad.target_sha256 = "zz".into();
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurveHash,
    );
    // 点数 < 2。
    let mut bad = mock_curve(48_000.0);
    bad.point_count = 1;
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurvePoints,
    );
    // delay_removed 必须有 D；其它 phase 不得带 D。
    let mut bad = mock_curve(48_000.0);
    bad.phase_ref = Some("delay_removed".into());
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurvePhase,
    );
    bad.target_delay_samples = Some(512);
    let ok = TargetCurveRef::from_source(&bad).expect("delay_removed ok");
    assert_eq!(ok.phase_ref, CalPhaseRef::DelayRemoved);
    assert_eq!(ok.target_delay_samples, Some(512));
    let mut bad = mock_curve(48_000.0);
    bad.phase_ref = Some("absolute".into());
    bad.target_delay_samples = Some(512);
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurvePhase,
    );
    // fs 非正。
    let mut bad = mock_curve(0.0);
    bad.fs_hz = 0.0;
    expect_error(
        TargetCurveRef::from_source(&bad),
        CalibrationErrorCode::IrCurveFs,
    );
    // 字面值/requires_delay 覆盖。
    assert_eq!(CalPhaseRef::None.as_str(), "none");
    assert_eq!(CalPhaseRef::Absolute.as_str(), "absolute");
    assert_eq!(CalPhaseRef::DelayRemoved.as_str(), "delay_removed");
    assert!(!CalPhaseRef::None.requires_delay());
    assert!(CalPhaseRef::DelayRemoved.requires_delay());
    assert_eq!(CalPhaseRef::from_id("nope"), None);
    assert_eq!(
        CalPhaseRef::from_id("delay_removed"),
        Some(CalPhaseRef::DelayRemoved)
    );
}

#[test]
fn calibration_ir_curve_single_application() {
    let merged = merged_412_plain();
    let mut builder = IrDraftBuilder::new(&merged, profile_412(), 0).expect("builder");
    builder.with_curve(valid_curve_412()).expect("first curve");
    expect_error(
        builder.with_curve(valid_curve_412()),
        CalibrationErrorCode::IrCurveDoubleApply,
    );
    builder
        .with_g_residual_participants(
            ["FL", "FR", "SL", "SR", "SUB1"]
                .iter()
                .map(|name| name.to_string())
                .collect(),
        )
        .expect("participants");
    let ir = builder.build().expect("build");
    assert_eq!(ir.curve_application_count(), 1);
    let curve = ir.curve().expect("curve bound");
    assert_eq!(curve.apply_site, CurveApplySite::TargetFirBaseline);
    assert_eq!(curve.target_sha256, hex64('d'));
    assert_eq!(curve.phase_ref, CalPhaseRef::None);
    // 每个角色 placement 含 target_fir（curve 只落基线 P）。
    for role in ir.post() {
        assert!(role.placement.contains(&PostStage::TargetFir));
        assert!(role.gain_db.is_finite());
    }
    // G residual 无 curve（不双叠）。
    assert!(ir.g_residual().curve_id().is_none());
    ir.validate_curve_single_application().expect("single");
    // 曲线 fs 与 IR fs 不一致 -> 拒收。
    let mut builder = IrDraftBuilder::new(&merged, profile_412(), 0).expect("builder2");
    let curve_96k = TargetCurveRef::from_source(&mock_curve(96_000.0)).expect("96k curve");
    expect_error(
        builder.with_curve(curve_96k),
        CalibrationErrorCode::IrCurveFs,
    );
    // delay_removed 曲线保留 D 且写入 canonical。
    let mut curve = mock_curve(48_000.0);
    curve.phase_ref = Some("delay_removed".into());
    curve.target_delay_samples = Some(512);
    let curve_ref = TargetCurveRef::from_source(&curve).expect("delay_removed");
    let mut builder = IrDraftBuilder::new(&merged, profile_412(), 0).expect("builder3");
    builder.with_curve(curve_ref).expect("bind");
    let ir = builder.build().expect("build3");
    let bound = ir.curve().unwrap();
    assert_eq!(bound.phase_ref, CalPhaseRef::DelayRemoved);
    assert_eq!(bound.target_delay_samples, Some(512));
    assert!(ir.canonical_form().contains("\"target_delay_samples\":512"));
    assert!(ir
        .canonical_form()
        .contains("\"apply_site\":\"target_fir_baseline\""));
}

// ---------------------------------------------------------------------------
// 5. 与 T12 接口：P 阶段投影 / CompileOptions / CompileInput+compile
// ---------------------------------------------------------------------------

#[test]
fn calibration_p_stage_coeff_embedded() {
    let ir = ir_412_plain();
    let stage = ir.p_stage_default().expect("p stage");
    assert_eq!(stage.sample_rate_hz, 48_000);
    assert_eq!(stage.n_role, 7);
    assert_eq!(stage.l_p, 13);
    assert_eq!(stage.p.len(), 7 * 13);
    assert_eq!(stage.encoding, PostEncoding::CoeffEmbedded);
    assert!(stage.delay_embedded_in_coefficients);
    assert_eq!(stage.max_total_delay_samples, 12);
    // FL: P = [1, 0, ...]（无延迟 normal）。
    let fl = stage.p_row(0).expect("row0");
    assert_eq!(fl[0], 1.0);
    assert!(fl[1..].iter().all(|value| *value == 0.0));
    // FR: delay 2 -> 行内 index 2 = 1。
    let fr = stage.p_row(1).expect("row1");
    assert_eq!(fr[2], 1.0);
    assert_eq!(fr.iter().filter(|value| **value != 0.0).count(), 1);
    // SUB1: delay 12 -> index 12 = 1。
    let sub = stage.p_row(6).expect("row6");
    assert_eq!(sub[12], 1.0);
    // StagePlan 投影：delay 折入系数，故 delay_samples=0/polarity=+1。
    assert_eq!(stage.stage_plans.len(), 7);
    for plan in &stage.stage_plans {
        assert_eq!(plan.taps, 13);
        assert_eq!(plan.delay_samples, 0);
        assert_eq!(plan.delay_q16, 0);
        assert_eq!(plan.polarity, 1);
        assert_eq!(plan.coeffs_f32.len(), 13);
    }
    assert_eq!(stage.roles[6].total_delay_samples, 12);
    assert_eq!(stage.roles[6].delay_samples, 0);
    assert_eq!(stage.roles[6].polarity, 1);
    // 越界 role 行。
    assert!(stage.p_row(7).is_none());
    assert!(ir.p_fir_row("NOPE").is_err());
    expect_error(ir.p_fir_row("NOPE"), CalibrationErrorCode::IrRoles);
}

#[test]
fn calibration_p_stage_role_metadata_encoding() {
    let merged = merged_412();
    let builder = IrDraftBuilder::new(&merged, profile_412(), 0).expect("builder");
    let ir = builder
        .with_encoding(PostEncoding::RoleMetadata)
        .build()
        .expect("build");
    assert_eq!(ir.encoding(), PostEncoding::RoleMetadata);
    let stage = ir.p_stage_default().expect("stage");
    assert_eq!(stage.l_p, 1);
    assert_eq!(stage.p.len(), 7);
    for value in &stage.p {
        assert_eq!(*value, 1.0);
    }
    assert!(!stage.delay_embedded_in_coefficients);
    // FR 实际总延迟 = base 1 + cal 2 = 3。
    let fr_index = ir
        .post()
        .iter()
        .position(|role| role.name == "FR")
        .expect("FR");
    let fr = &stage.roles[fr_index];
    assert_eq!(fr.total_delay_samples, 3);
    assert_eq!(fr.delay_samples, 3);
    assert_eq!(fr.delay_q16, 0);
    assert_eq!(fr.polarity, 1);
    // HFR: base invert XOR cal normal = invert -> role polarity -1。
    let hfr_index = ir
        .post()
        .iter()
        .position(|role| role.name == "HFR")
        .expect("HFR");
    let hfr = &stage.roles[hfr_index];
    assert_eq!(hfr.total_delay_samples, 3);
    assert_eq!(hfr.polarity, -1);
    assert_eq!(stage.stage_plans[hfr_index].polarity, -1);
    assert_eq!(stage.stage_plans[hfr_index].delay_samples, 3);
    // 两种编码在信号层等价：同一 merged（base HFR invert）的 CoeffEmbedded IR vs RoleMetadata。
    let embedded_builder =
        IrDraftBuilder::new(&merged, profile_412(), 0).expect("embedded builder");
    let ir_embedded = embedded_builder.build().expect("embedded ir");
    let embedded_stage = ir_embedded.p_stage_default().expect("embedded stage");
    let hfr_embedded = ir
        .post()
        .iter()
        .position(|role| role.name == "HFR")
        .map(|index| embedded_stage.p_row(index).unwrap().to_vec())
        .expect("row");
    let simulated_embedded = simulate_delta(&hfr_embedded, 0, 1, 16);
    let simulated_metadata = simulate_delta(&[1.0], hfr.delay_samples, hfr.polarity, 16);
    assert_eq!(simulated_embedded, simulated_metadata);
    assert_eq!(simulated_embedded[3], -1.0);
}

fn simulate_delta(
    coefficients: &[f64],
    role_delay_samples: u32,
    role_polarity: i8,
    length: usize,
) -> Vec<f64> {
    // y[n] = sum_k h[k] x[n-k]，x=delta[0]；再施加 role 级 delay/polarity（一次）。
    let mut after_fir = vec![0.0; length];
    for (index, value) in coefficients.iter().enumerate() {
        if index < length {
            after_fir[index] += *value;
        }
    }
    let mut out = vec![0.0; length];
    for time in 0..length {
        let source = time as i64 - role_delay_samples as i64;
        if source >= 0 && (source as usize) < length {
            out[time] = role_polarity as f64 * after_fir[source as usize];
        }
    }
    out
}

#[test]
fn calibration_p_stage_budget_and_compile_options() {
    // 200 ms 上限 delay（9600）导致 P FIR 9601 taps > 常规 4096 -> INFEASIBLE，不截尾。
    let yaml = "schema: tspi-calibration/v1\nsample_rate: 48000\nreference_index: 0\nchannels:\n  - {name: FL, index: 0, delay_samples: 0, polarity: normal, filters: []}\n  - {name: SUB1, index: 7, delay_samples: 9600, polarity: normal, filters: []}\n";
    let doc = parse_calibration_yaml(yaml.as_bytes()).expect("doc");
    let base = vec![
        RenderRoleBase::uncalibrated("FL", 0),
        RenderRoleBase::uncalibrated("SUB1", 7),
    ];
    let merged = merge_calibration(&doc, None, &base, 48_000).expect("merge");
    let profile = IrProfileBinding {
        profile_id: "long-delay".into(),
        profile_sha256: sha_a(),
        stereo_strict: false,
        stereo_mask: vec!["FL".into()],
        target_profile: "target_multichannel_4_1_2".into(),
    };
    let ir = IrDraftBuilder::new(&merged, profile, 0)
        .expect("builder")
        .build()
        .expect("ir");
    expect_error(ir.p_stage_default(), CalibrationErrorCode::IrTapsBudget);
    let extended = ir
        .compile_options(128, 16_384, true, 4 * 1024 * 1024)
        .expect("extended options");
    let stage = ir.p_stage_with_options(&extended).expect("extended stage");
    assert_eq!(stage.l_p, 9_601);
    assert_eq!(stage.max_total_delay_samples, 9_600);
    // CompileOptions 非法 / fs 不一致 / 预算不足。
    expect_error(
        ir.compile_options(256, 4_096, false, 4 * 1024 * 1024),
        CalibrationErrorCode::IrCompileOptions,
    );
    expect_error(
        ir.compile_options(128, 20_000, false, 4 * 1024 * 1024),
        CalibrationErrorCode::IrCompileOptions,
    );
    let wrong_rate = CompileOptions {
        sample_rate_hz: 96_000,
        block_size: 128,
        max_taps: 16_384,
        extended_taps_admitted: true,
        coeff_budget_bytes: 4 * 1024 * 1024,
    };
    expect_error(
        ir.p_stage_with_options(&wrong_rate),
        CalibrationErrorCode::IrRateMismatch,
    );
    let tiny_budget = CompileOptions {
        sample_rate_hz: 48_000,
        block_size: 128,
        max_taps: 16_384,
        extended_taps_admitted: true,
        coeff_budget_bytes: 16,
    };
    expect_error(
        ir.p_stage_with_options(&tiny_budget),
        CalibrationErrorCode::IrCapacity,
    );
}

#[test]
fn calibration_t12_compile_integration() {
    let ir = ir_412_plain();
    let stage = ir.p_stage_default().expect("stage");
    let n_role = stage.n_role as usize;
    let src_of_role: Vec<u16> = (0..n_role as u16).collect();
    let scatter: Vec<u16> = vec![0, 1, 2, 3, 4, 5, 7];
    let a = vec![1.0f64; n_role];
    let participants = vec![0u16];
    let g = vec![0.0f64];
    let input = CompileInput {
        n_src: n_role as u16,
        n_role: n_role as u16,
        container_channels: 8,
        d_samples: 0,
        participants: &participants,
        src_of_role: &src_of_role,
        scatter: &scatter,
        a: &a,
        l_a: 1,
        g: &g,
        l_g: 1,
        p: &stage.p,
        l_p: stage.l_p,
    };
    let options = CompileOptions {
        sample_rate_hz: 48_000,
        block_size: 128,
        max_taps: 4_096,
        extended_taps_admitted: false,
        coeff_budget_bytes: 4 * 1024 * 1024,
    };
    let plans = compile(&input, &options).expect("T12 compile 必须接受校准 P 阶段");
    assert_eq!(plans.report.route_count, 7);
    assert_eq!(plans.factorized.n_role, 7);
    assert_eq!(plans.factorized.l_p, 13);
    assert_eq!(plans.factorized.p.len(), stage.p.len());
    assert_eq!(plans.factorized.p, stage.p);
    // 输出阶段 plan 投影与 IR 的逐角色 taps/delay 一致（延迟已折入系数）。
    let output_plans = plans.factorized.output_stage_plans();
    assert_eq!(output_plans.len(), 7);
    for plan in &output_plans {
        assert_eq!(plan.taps, 13);
        assert_eq!(plan.delay_samples, 0);
        assert_eq!(plan.polarity, 1);
        assert_eq!(plan.coeffs_f32.len(), 13);
    }
    // T12 design_delay_to_plan 对整数校准 delay 给出 q16=0。
    for role in ir.post() {
        let plan = tspi_dsp_core::compile::design_delay_to_plan(role.delay_samples as f64)
            .expect("delay plan");
        assert_eq!(plan.delay_samples, role.delay_samples);
        assert_eq!(plan.delay_q16, 0);
        assert_eq!(plan.total_delay_q16(), (role.delay_samples as u64) << 16);
    }
}

#[test]
fn calibration_ir_sample_report() {
    // 证据样例（--nocapture 输出）：IR 规范化文本/hash 与 P 阶段摘要。
    let ir = ir_412_plain();
    let stage = ir
        .p_stage_default()
        .expect("默认 profile 下 P 阶段准入必须成功");
    println!("T13_SAMPLE_IR_SCHEMA={}", ir.schema());
    println!("T13_SAMPLE_IR_VERSION={}", ir.ir_version());
    println!("T13_SAMPLE_IR_HASH={}", ir.ir_hash());
    println!("T13_SAMPLE_IR_CANONICAL={}", ir.canonical_form());
    println!(
        "T13_SAMPLE_P_STAGE l_p={} n_role={} max_total_delay_samples={} coeff_p={}",
        stage.l_p,
        stage.n_role,
        stage.max_total_delay_samples,
        stage.p.len()
    );
    for role in &stage.roles {
        println!(
            "T13_SAMPLE_ROLE name={} role_index={} out_channel={} total_delay_samples={} delay_field={} polarity={} gain_db={}",
            role.name,
            role.role_index,
            role.out_channel,
            role.total_delay_samples,
            role.delay_samples,
            role.polarity,
            role.gain_db
        );
    }
    for plan in &stage.stage_plans {
        println!(
            "T13_SAMPLE_STAGE_PLAN idx={} taps={} delay={} q16={} polarity={} coeff_f32={}",
            plan.index,
            plan.taps,
            plan.delay_samples,
            plan.delay_q16,
            plan.polarity,
            plan.coeffs_f32.len()
        );
    }
    assert_eq!(ir.ir_hash().len(), 64);
    assert_eq!(stage.l_p, 13);
    assert!(ir.curve().is_none());
}
