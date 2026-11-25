//! T48 private integration tests — solver ingest（raw/source/diag/position）.
//!
//! 运行（在 rt-stack/ 下）：cargo test -p khaen-modal-design ingest
//! 文件位置约定：crates/modal-design/tests/T48/main.rs（leaf 私有测试；由 integrator 决定公共注册）。
//!
//! 覆盖：合法 session 全链路、reference 可选通道、以及 session/position/diag/analysis/raw/hash
//! 六层拒收路径（schema、坏 JSON、重复 key、缺时轴、跨 position epoch 不一致、slot 覆盖/单调、
//! 缺文件、hash 不符、raw 大小、采样率/格式/1ch、权重/角色/label、时钟矛盾、onset 无效等）。
//! 本文件只构造临时输入目录并只读 ingest；不采音、不写项目文件、不碰板端。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use khaen_modal_design::ingest::{
    read_bundle, sha256_hex, IngestBundle, IngestError, IngestErrorKind, PositionRole,
    CAPTURE_FILE, POSITION_FILE, REFERENCE_FILE, SESSION_FILE, SHA256SUMS_FILE,
};

const SAMPLE_RATE: u32 = 48_000;
const FRAMES: u64 = 24_000;
const SESSION_ID: &str = "mimo-4.1.2-20261120-a";
const EPOCH_ID: u64 = 7;
const GRAPH_ID: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const SWEEP_SECONDS: f64 = 0.05;
const GAP_SECONDS: f64 = 0.005;
const PRE_SECONDS: f64 = 0.005;
const POST_SECONDS: f64 = 0.005;
const PLAYBACK_DEVICE: &str = "hw:motu,0";
const CAPTURE_DEVICE: &str = "hw:umik1,0";
/// participants（顺序即 dense 索引 0..M-1；4.1.2 例：FL0 FR1 SUB1=7）。
const PARTICIPANTS: [(&str, u32); 3] = [("FL", 0), ("FR", 1), ("SUB1", 7)];

const TIMEOUT_HINT: &str = "fixture must not exceed test budget";

fn interval_frames() -> f64 {
    (SWEEP_SECONDS + GAP_SECONDS) * SAMPLE_RATE as f64
}

fn slot_start(slot: u32) -> f64 {
    PRE_SECONDS * SAMPLE_RATE as f64 + 100.0 + slot as f64 * interval_frames()
}

fn render_frame(slot: u32) -> u64 {
    1_000_000 + slot as u64 * (interval_frames() as u64) + 17
}

// ===========================================================================
// 最小 JSON 值类型（测试专用；确定性序列化，便于可变构造负向 fixture）
// ===========================================================================

#[derive(Clone, Debug)]
enum J {
    Num(f64),
    Str(String),
    Bool(bool),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn n(value: f64) -> J {
        J::Num(value)
    }

    fn s(value: &str) -> J {
        J::Str(value.to_string())
    }

    fn b(value: bool) -> J {
        J::Bool(value)
    }

    fn arr(items: Vec<J>) -> J {
        J::Arr(items)
    }

    fn obj(items: Vec<(&str, J)>) -> J {
        J::Obj(
            items
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn set(&mut self, key: &str, value: J) {
        match self {
            J::Obj(entries) => {
                if let Some(slot) = entries.iter_mut().find(|(name, _)| name == key) {
                    slot.1 = value;
                } else {
                    entries.push((key.to_string(), value));
                }
            }
            _ => panic!("set() requires a JSON object"),
        }
    }

    fn remove(&mut self, key: &str) {
        match self {
            J::Obj(entries) => entries.retain(|(name, _)| name != key),
            _ => panic!("remove() requires a JSON object"),
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut J> {
        match self {
            J::Obj(entries) => entries
                .iter_mut()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Num(value) => {
                if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e15 {
                    out.push_str(&format!("{}", *value as i64));
                } else {
                    out.push_str(&format!("{value}"));
                }
            }
            J::Str(value) => {
                out.push('"');
                for ch in value.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
                        ch => out.push(ch),
                    }
                }
                out.push('"');
            }
            J::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            J::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            J::Obj(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    out.push_str(key);
                    out.push_str("\":");
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn set_path(root: &mut J, path: &[&str], value: J) {
    assert!(!path.is_empty(), "set_path requires a non-empty path");
    let mut current = root;
    for key in &path[..path.len() - 1] {
        current = current
            .get_mut(key)
            .unwrap_or_else(|| panic!("set_path: missing intermediate key {key}"));
    }
    current.set(path[path.len() - 1], value);
}

// ===========================================================================
// fixture 构造
// ===========================================================================

struct MeasurementFixture {
    speaker: &'static str,
    out_index: u32,
    slot: u32,
    diag: J,
    analysis: J,
}

struct PositionFixture {
    dir: &'static str,
    meta: J,
    capture: Vec<u8>,
    reference: Option<Vec<u8>>,
    measurements: Vec<MeasurementFixture>,
}

struct Fixture {
    root: PathBuf,
    session: J,
    positions: Vec<PositionFixture>,
    overrides: BTreeMap<String, Vec<u8>>,
    skip: BTreeSet<String>,
    exclude_from_sums: BTreeSet<String>,
    extra_sums_lines: Vec<String>,
    materialized: bool,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        // 不写系统 TEMP（本机 TEMP 指向内存盘，部分进程会被拒绝）；
        // fixture 放在测试二进制所在的构建目录（可重建、不在仓库内）。
        let base = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .map(|parent| parent.join("t48-ingest-fixtures"))
            })
            .unwrap_or_else(|| std::env::temp_dir().join("t48-ingest-fixtures"));
        let root = base.join(format!(
            "tspi-t48-{}-{}-{}",
            std::process::id(),
            unique,
            tag
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect(TIMEOUT_HINT);
        Fixture {
            root,
            session: session_json(),
            positions: vec![
                position_fixture("p01-mlp", "P01", "design", 1.0),
                position_fixture("p02-sofa", "P02", "validation", 0.6),
            ],
            overrides: BTreeMap::new(),
            skip: BTreeSet::new(),
            exclude_from_sums: BTreeSet::new(),
            extra_sums_lines: Vec::new(),
            materialized: false,
        }
    }

    fn override_file(&mut self, relative: &str, content: &str) {
        self.overrides
            .insert(relative.to_string(), content.as_bytes().to_vec());
    }

    fn skip_file(&mut self, relative: &str) {
        self.skip.insert(relative.to_string());
    }

    fn exclude_from_manifest(&mut self, relative: &str) {
        self.exclude_from_sums.insert(relative.to_string());
    }

    fn add_manifest_line(&mut self, line: &str) {
        self.extra_sums_lines.push(line.to_string());
    }

    fn enable_reference(&mut self) {
        self.session.set(
            "reference",
            J::obj(vec![
                ("device", J::s(PLAYBACK_DEVICE)),
                ("channels", J::n(2.0)),
                ("sample_format", J::s("S32_LE")),
                ("independent", J::b(true)),
            ]),
        );
        for position in &mut self.positions {
            position.reference = Some(vec![0u8; FRAMES as usize * 2 * 4]);
        }
    }

    fn set_participant_repeats(&mut self, repeats: u32) {
        if let J::Obj(entries) = &mut self.session {
            for (name, value) in entries.iter_mut() {
                if name == "stimulus" {
                    if let J::Obj(stimulus) = value {
                        for (field, field_value) in stimulus.iter_mut() {
                            if field == "repeats" {
                                *field_value = J::n(repeats as f64);
                            }
                        }
                    }
                }
            }
        }
        for position in &mut self.positions {
            for measurement in &mut position.measurements {
                measurement.diag.set("repeats", J::n(repeats as f64));
                let starts = (0..repeats)
                    .map(|index| {
                        J::n(slot_start(measurement.slot) + index as f64 * interval_frames())
                    })
                    .collect();
                measurement.diag.set("repeat_starts", J::arr(starts));
                if let J::Obj(entries) = &mut measurement.analysis {
                    for (name, value) in entries.iter_mut() {
                        if name == "repeats" {
                            *value = J::obj(vec![("count", J::n(repeats as f64))]);
                        }
                    }
                }
            }
        }
    }

    fn plan(&self) -> BTreeMap<String, Vec<u8>> {
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        files.insert(SESSION_FILE.to_string(), self.session.text().into_bytes());
        for position in &self.positions {
            let base = format!("positions/{}", position.dir);
            files.insert(
                format!("{base}/{POSITION_FILE}"),
                position.meta.text().into_bytes(),
            );
            files.insert(format!("{base}/{CAPTURE_FILE}"), position.capture.clone());
            if let Some(reference) = &position.reference {
                files.insert(format!("{base}/{REFERENCE_FILE}"), reference.clone());
            }
            for measurement in &position.measurements {
                let stem = format!("{}-ch{:02}", measurement.speaker, measurement.out_index);
                files.insert(
                    format!("{base}/{stem}.diag.json"),
                    measurement.diag.text().into_bytes(),
                );
                files.insert(
                    format!("{base}/{stem}.analysis.json"),
                    measurement.analysis.text().into_bytes(),
                );
            }
        }
        for (relative, content) in &self.overrides {
            files.insert(relative.clone(), content.clone());
        }
        for relative in &self.skip {
            files.remove(relative);
        }
        files
    }

    fn materialize(&mut self) -> PathBuf {
        let files = self.plan();
        for (relative, content) in &files {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect(TIMEOUT_HINT);
            }
            fs::write(&path, content).expect(TIMEOUT_HINT);
        }
        let mut manifest = String::new();
        for (relative, content) in &files {
            if self.exclude_from_sums.contains(relative) {
                continue;
            }
            manifest.push_str(&format!("{}  {}\n", sha256_hex(content), relative));
        }
        for line in &self.extra_sums_lines {
            manifest.push_str(line);
            manifest.push('\n');
        }
        fs::write(self.root.join(SHA256SUMS_FILE), manifest).expect(TIMEOUT_HINT);
        self.materialized = true;
        self.root.clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn session_json() -> J {
    J::obj(vec![
        ("schema", J::s("tspi-mimo-measurement/v1")),
        ("session_id", J::s(SESSION_ID)),
        ("layout", J::s("4.1.2")),
        ("sample_rate_hz", J::n(SAMPLE_RATE as f64)),
        ("block_frames", J::n(128.0)),
        (
            "participants",
            J::arr(PARTICIPANTS.iter().map(|(name, _)| J::s(name)).collect()),
        ),
        ("speakers", speakers_json()),
        ("mic", mic_json()),
        ("playback", playback_json()),
        ("capture", capture_json()),
        ("stimulus", stimulus_json()),
        ("room_temperature_c", J::n(22.0)),
        (
            "tool",
            J::obj(vec![
                ("name", J::s("tspi-audio-daemon")),
                ("version", J::s("0.1.0")),
            ]),
        ),
        ("created_unix", J::n(1_794_725_000.0)),
        ("claim", J::s("T48 integration fixture")),
    ])
}

fn speakers_json() -> J {
    J::arr(vec![
        J::obj(vec![
            ("name", J::s("FL")),
            ("out_index", J::n(0.0)),
            ("kind", J::s("main")),
            ("allowed_band_hz", J::arr(vec![J::n(80.0), J::n(300.0)])),
        ]),
        J::obj(vec![
            ("name", J::s("FR")),
            ("out_index", J::n(1.0)),
            ("kind", J::s("main")),
            ("allowed_band_hz", J::arr(vec![J::n(80.0), J::n(300.0)])),
        ]),
        J::obj(vec![
            ("name", J::s("HFL")),
            ("out_index", J::n(4.0)),
            ("kind", J::s("height")),
        ]),
        J::obj(vec![
            ("name", J::s("SUB1")),
            ("out_index", J::n(7.0)),
            ("kind", J::s("subwoofer")),
            ("allowed_band_hz", J::arr(vec![J::n(20.0), J::n(300.0)])),
        ]),
    ])
}

fn mic_json() -> J {
    J::obj(vec![
        ("model", J::s("UMIK-1")),
        ("serial", J::s("718-7696")),
        (
            "cal_sha256",
            J::s("aa00bb11cc22dd33ee44ff5566778899aabbccddeeff00112233445566778899"),
        ),
        ("orientation", J::s("90deg")),
        ("clock_domain", J::s("usb")),
    ])
}

fn playback_json() -> J {
    J::obj(vec![
        ("device", J::s(PLAYBACK_DEVICE)),
        ("channels", J::n(22.0)),
        ("sample_format", J::s("S32_LE")),
    ])
}

fn capture_json() -> J {
    J::obj(vec![
        ("device", J::s(CAPTURE_DEVICE)),
        ("channels", J::n(1.0)),
        ("sample_format", J::s("S24_3LE")),
        ("pre_seconds", J::n(PRE_SECONDS)),
        ("post_seconds", J::n(POST_SECONDS)),
    ])
}

fn stimulus_json() -> J {
    J::obj(vec![
        ("kind", J::s("log_sweep")),
        ("start_hz", J::n(15.0)),
        ("end_hz", J::n(600.0)),
        ("seconds", J::n(SWEEP_SECONDS)),
        ("level_dbfs", J::n(-30.0)),
        ("fade_ms", J::n(10.0)),
        ("repeats", J::n(3.0)),
        ("repeat_gap_seconds", J::n(GAP_SECONDS)),
    ])
}

fn position_fixture(dir: &'static str, name: &str, role: &str, weight: f64) -> PositionFixture {
    let measurements = PARTICIPANTS
        .iter()
        .enumerate()
        .map(|(slot, (speaker, out_index))| MeasurementFixture {
            speaker,
            out_index: *out_index,
            slot: slot as u32,
            diag: diag_json(speaker, *out_index, slot as u32),
            analysis: analysis_json(name, weight, *out_index),
        })
        .collect();
    PositionFixture {
        dir,
        meta: J::obj(vec![
            ("schema", J::s("tspi-mimo-position/v1")),
            ("name", J::s(name)),
            ("role", J::s(role)),
            ("weight", J::n(weight)),
            (
                "coordinates_m",
                J::arr(vec![J::n(0.0), J::n(0.0), J::n(1.2)]),
            ),
            ("mic_orientation", J::s("90deg")),
            ("created_unix", J::n(1_794_725_100.0)),
        ]),
        capture: vec![0u8; FRAMES as usize * 3],
        reference: None,
        measurements,
    }
}

fn diag_json(speaker: &str, out_index: u32, slot: u32) -> J {
    let actual = slot_start(slot);
    J::obj(vec![
        ("schema", J::s("tspi-mimo-diag/v1")),
        ("session_id", J::s(SESSION_ID)),
        ("epoch_id", J::n(EPOCH_ID as f64)),
        ("graph_id", J::s(GRAPH_ID)),
        ("speaker", J::s(speaker)),
        ("channel", J::n(out_index as f64)),
        ("slot", J::n(slot as f64)),
        ("capture_file", J::s(CAPTURE_FILE)),
        ("render_frame_index", J::n(render_frame(slot) as f64)),
        ("capture_sample_index", J::n(actual)),
        ("nominal_start", J::n(actual - 2.0)),
        ("actual_start", J::n(actual)),
        ("lag_samples", J::n(2.0)),
        (
            "repeat_starts",
            J::arr(
                (0..3)
                    .map(|index| J::n(actual + index as f64 * interval_frames()))
                    .collect(),
            ),
        ),
        ("repeats", J::n(3.0)),
        (
            "clock",
            J::obj(vec![
                ("clock_ppm", J::n(1.0)),
                ("clock_ppm_uncertainty_ppm", J::n(1.0)),
                ("drift_significant", J::b(false)),
                ("clock_ppm_ok", J::b(true)),
                ("correction", J::s("none")),
            ]),
        ),
        ("onset_invalid", J::b(false)),
    ])
}

fn analysis_json(position: &str, weight: f64, out_index: u32) -> J {
    J::obj(vec![
        ("schema", J::s("tspi-channel-measurement/v1")),
        ("channel", J::n(out_index as f64)),
        ("delay_samples", J::n(12.5)),
        ("delay_ms", J::n(12.5 / SAMPLE_RATE as f64 * 1000.0)),
        ("delay_coarse_samples", J::n(12.0)),
        ("polarity", J::n(1.0)),
        ("peak_db", J::n(-22.0)),
        ("phase_1k_deg", J::n(7.5)),
        ("snr_db", J::n(35.0)),
        ("phase_ref", J::s("aligned_segment(已去粗延迟)")),
        ("fr", fr_json()),
        ("fr_modal", fr_json()),
        (
            "position",
            J::obj(vec![("name", J::s(position)), ("weight", J::n(weight))]),
        ),
        ("repeats", J::obj(vec![("count", J::n(3.0))])),
        ("meta", meta_json()),
    ])
}

fn fr_json() -> J {
    let frequencies = [20.0, 40.0, 80.0, 160.0, 320.0, 640.0, 1280.0, 2560.0];
    J::arr(
        frequencies
            .iter()
            .enumerate()
            .map(|(index, freq)| {
                J::obj(vec![
                    ("freq_hz", J::n(*freq)),
                    ("db", J::n(-1.0 - index as f64 * 0.1)),
                    ("phase_deg", J::n(index as f64 * 2.0)),
                ])
            })
            .collect(),
    )
}

fn meta_json() -> J {
    J::obj(vec![
        ("schema", J::s("khaen-calibration-meta/v1")),
        ("sample_rate", J::n(SAMPLE_RATE as f64)),
        ("channels", J::n(22.0)),
        ("sweep_start_hz", J::n(15.0)),
        ("sweep_end_hz", J::n(600.0)),
        ("sweep_seconds", J::n(SWEEP_SECONDS)),
        ("level_db", J::n(-30.0)),
        ("pre_seconds", J::n(PRE_SECONDS)),
        ("post_seconds", J::n(POST_SECONDS)),
        ("playback_device", J::s(PLAYBACK_DEVICE)),
        ("playback_channels", J::n(22.0)),
        ("capture_device", J::s(CAPTURE_DEVICE)),
        ("capture_channels", J::n(1.0)),
        ("created_unix", J::n(1_794_725_050.0)),
    ])
}

// ===========================================================================
// 断言辅助
// ===========================================================================

fn expect_ok(root: &Path) -> IngestBundle {
    read_bundle(root).unwrap_or_else(|error| panic!("expected ingest to succeed: {error}"))
}

fn expect_err(root: &Path, kind: IngestErrorKind) -> IngestError {
    match read_bundle(root) {
        Ok(_) => panic!("expected {kind:?} but ingest succeeded"),
        Err(error) => {
            assert_eq!(error.kind(), kind, "unexpected error: {error}");
            error
        }
    }
}

// ===========================================================================
// 正向：合法 bundle
// ===========================================================================

#[test]
fn t48_ingest_reads_valid_bundle() {
    let mut fixture = Fixture::new("happy");
    let root = fixture.materialize();
    let bundle = expect_ok(&root);
    assert_eq!(bundle.session.session_id, SESSION_ID);
    assert_eq!(bundle.session.layout, "4.1.2");
    assert_eq!(bundle.session.sample_rate_hz, SAMPLE_RATE);
    assert_eq!(bundle.session.block_frames, Some(128));
    assert_eq!(
        bundle.session.participants,
        vec!["FL".to_string(), "FR".to_string(), "SUB1".to_string()]
    );
    assert_eq!(bundle.dense_to_out_index(), vec![0, 1, 7]);
    assert!(bundle.session.reference.is_none());
    assert_eq!(bundle.session.mic.model, "UMIK-1");
    assert_eq!(bundle.session.mic.clock_domain, "usb");
    assert_eq!(bundle.session.stimulus.repeats, 3);
    assert_eq!(bundle.positions.len(), 2);
    assert_eq!(bundle.design_count(), 1);
    assert_eq!(bundle.validation_count(), 1);
    assert_eq!(bundle.design_measurement_count(), 3);
    assert_eq!(
        bundle
            .design_positions()
            .map(|p| p.meta.name.as_str())
            .collect::<Vec<_>>(),
        vec!["P01"]
    );
    assert_eq!(
        bundle
            .validation_positions()
            .map(|p| p.meta.name.as_str())
            .collect::<Vec<_>>(),
        vec!["P02"]
    );
    let position = bundle.position("P01").expect("P01");
    assert_eq!(position.meta.role, PositionRole::Design);
    assert_eq!(position.meta.weight, 1.0);
    assert_eq!(position.meta.coordinates_m, Some([0.0, 0.0, 1.2]));
    assert_eq!(position.participant_names(), vec!["FL", "FR", "SUB1"]);
    assert_eq!(position.capture.frames, FRAMES);
    assert_eq!(position.capture.channels, 1);
    assert_eq!(position.capture.bytes, FRAMES * 3);
    assert_eq!(position.capture.sample_format.as_str(), "S24_3LE");
    assert!(position.is_design());
    assert!(!position.is_validation());
    let measurement = bundle.measurement("P01", "SUB1").expect("SUB1 measurement");
    assert_eq!(measurement.out_index, 7);
    assert_eq!(measurement.slot, 2);
    assert_eq!(measurement.analysis.channel, 7);
    assert_eq!(measurement.analysis.fr.len(), 8);
    assert_eq!(measurement.analysis.fr[0].freq_hz, 20.0);
    assert!(
        measurement.analysis.phase_ref.contains("absolute")
            || !measurement.analysis.phase_ref.is_empty()
    );
    assert_eq!(measurement.diag.epoch_id, EPOCH_ID);
    assert_eq!(measurement.diag.graph_id, GRAPH_ID);
    assert!(measurement.diag.clock.clock_ppm_ok);
    assert!(!measurement.diag.clock.drift_significant);
    assert_eq!(measurement.diag.repeats, 3);
    assert_eq!(measurement.diag.repeat_starts.len(), 3);
    assert_eq!(bundle.session_sha256.len(), 64);
    assert_eq!(bundle.files.len(), 17);
    assert!(
        bundle
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("analys.json"))
            == false
    );
    assert!(bundle
        .files
        .iter()
        .any(|file| file.relative_path == "positions/p01-mlp/capture.raw"));
    assert!(
        bundle.warnings.is_empty(),
        "unexpected warnings: {:?}",
        bundle.warnings
    );
}

#[test]
fn t48_ingest_reads_bundle_with_independent_reference() {
    let mut fixture = Fixture::new("reference");
    fixture.enable_reference();
    let root = fixture.materialize();
    let bundle = expect_ok(&root);
    let reference = bundle
        .session
        .reference
        .as_ref()
        .expect("reference declared");
    assert_eq!(reference.device, PLAYBACK_DEVICE);
    assert_eq!(reference.channels, 2);
    assert!(reference.independent);
    for position in &bundle.positions {
        let reference = position.reference.as_ref().expect("reference.raw");
        assert_eq!(reference.frames, FRAMES);
        assert_eq!(reference.channels, 2);
        assert_eq!(reference.bytes, FRAMES * 2 * 4);
        assert_eq!(reference.sample_format.as_str(), "S32_LE");
    }
}

// ===========================================================================
// 负向：清单 / JSON / schema
// ===========================================================================

#[test]
fn t48_ingest_rejects_missing_sha256sums() {
    let mut fixture = Fixture::new("no-sums");
    let root = fixture.materialize();
    fs::remove_file(root.join(SHA256SUMS_FILE)).expect("remove sums");
    let error = expect_err(&root, IngestErrorKind::Missing);
    assert_eq!(error.code(), "E_INGEST_MISSING");
}

#[test]
fn t48_ingest_rejects_session_schema_mismatch() {
    let mut fixture = Fixture::new("schema");
    fixture
        .session
        .set("schema", J::s("tspi-mimo-measurement/v0"));
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Schema);
    assert_eq!(error.code(), "E_INGEST_SCHEMA");
}

#[test]
fn t48_ingest_rejects_malformed_json() {
    let mut fixture = Fixture::new("bad-json");
    fixture.override_file(SESSION_FILE, "{");
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Json);
}

#[test]
fn t48_ingest_rejects_duplicate_json_keys() {
    let mut fixture = Fixture::new("dup-key");
    fixture.override_file(SESSION_FILE, "{\"schema\":\"a\",\"schema\":\"b\"}");
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Json);
}

#[test]
fn t48_ingest_rejects_placeholder_graph_id() {
    let mut fixture = Fixture::new("graph-id");
    fixture.positions[0].measurements[0]
        .diag
        .set("graph_id", J::s(&"0".repeat(64)));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Schema);
}

#[test]
fn t48_ingest_rejects_duplicate_participant() {
    let mut fixture = Fixture::new("dup-participant");
    fixture.session.set(
        "participants",
        J::arr(vec![J::s("FL"), J::s("FL"), J::s("SUB1")]),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Duplicate);
}

#[test]
fn t48_ingest_rejects_participant_missing_speaker() {
    let mut fixture = Fixture::new("unknown-participant");
    fixture.session.set(
        "participants",
        J::arr(vec![J::s("FL"), J::s("FR"), J::s("SR")]),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Schema);
}

#[test]
fn t48_ingest_rejects_unsupported_sample_rate() {
    let mut fixture = Fixture::new("rate");
    fixture.session.set("sample_rate_hz", J::n(44_100.0));
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Unsupported);
    assert_eq!(error.code(), "E_INGEST_UNSUPPORTED");
}

#[test]
fn t48_ingest_rejects_capture_channels_not_one() {
    let mut fixture = Fixture::new("capture-channels");
    set_path(&mut fixture.session, &["capture", "channels"], J::n(2.0));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Unsupported);
}

#[test]
fn t48_ingest_rejects_reference_sharing_capture_device() {
    let mut fixture = Fixture::new("reference-device");
    fixture.session.set(
        "reference",
        J::obj(vec![
            ("device", J::s(CAPTURE_DEVICE)),
            ("channels", J::n(2.0)),
            ("sample_format", J::s("S32_LE")),
            ("independent", J::b(true)),
        ]),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Unsupported);
}

// ===========================================================================
// 负向：公共时轴 / slot / mic clock
// ===========================================================================

#[test]
fn t48_ingest_rejects_missing_time_axis_field() {
    let mut fixture = Fixture::new("missing-timebase");
    fixture.positions[0].measurements[0]
        .diag
        .remove("render_frame_index");
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Timebase);
    assert_eq!(error.code(), "E_INGEST_TIMEBASE");
}

#[test]
fn t48_ingest_rejects_missing_clock_block() {
    let mut fixture = Fixture::new("missing-clock");
    fixture.positions[0].measurements[0].diag.remove("clock");
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_diag_session_id_mismatch() {
    let mut fixture = Fixture::new("diag-session");
    fixture.positions[0].measurements[0]
        .diag
        .set("session_id", J::s("other-session"));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_epoch_mismatch_across_positions() {
    let mut fixture = Fixture::new("epoch");
    fixture.positions[1].measurements[0]
        .diag
        .set("epoch_id", J::n((EPOCH_ID + 1) as f64));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_slot_coverage_gap() {
    let mut fixture = Fixture::new("slot-gap");
    fixture.positions[0].measurements[1]
        .diag
        .set("slot", J::n(2.0));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_non_increasing_render_frames() {
    let mut fixture = Fixture::new("render-order");
    fixture.positions[0].measurements[1]
        .diag
        .set("render_frame_index", J::n(render_frame(0) as f64));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_clock_significance_contradiction() {
    let mut fixture = Fixture::new("clock-2sigma");
    set_path(
        &mut fixture.positions[0].measurements[0].diag,
        &["clock", "clock_ppm"],
        J::n(5.0),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_significant_drift_without_correction() {
    let mut fixture = Fixture::new("clock-correction");
    set_path(
        &mut fixture.positions[0].measurements[0].diag,
        &["clock", "clock_ppm"],
        J::n(5.0),
    );
    set_path(
        &mut fixture.positions[0].measurements[0].diag,
        &["clock", "drift_significant"],
        J::b(true),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_invalid_onset_flag() {
    let mut fixture = Fixture::new("onset");
    fixture.positions[0].measurements[0]
        .diag
        .set("onset_invalid", J::b(true));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_repeat_starts_length_mismatch() {
    let mut fixture = Fixture::new("repeat-length");
    fixture.positions[0].measurements[0].diag.set(
        "repeat_starts",
        J::arr(vec![
            J::n(slot_start(0)),
            J::n(slot_start(0) + interval_frames()),
        ]),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_capture_shorter_than_sweep_window() {
    let mut fixture = Fixture::new("short-capture");
    fixture.positions[0].capture = vec![0u8; 6_000 * 3];
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

// ===========================================================================
// 负向：position / analysis / raw / hash
// ===========================================================================

#[test]
fn t48_ingest_rejects_missing_analysis_file() {
    let mut fixture = Fixture::new("missing-analysis");
    fixture.skip_file("positions/p01-mlp/FL-ch00.analysis.json");
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Missing);
}

#[test]
fn t48_ingest_rejects_missing_diag_file() {
    let mut fixture = Fixture::new("missing-diag");
    fixture.skip_file("positions/p01-mlp/FR-ch01.diag.json");
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Missing);
}

#[test]
fn t48_ingest_rejects_file_missing_from_manifest() {
    let mut fixture = Fixture::new("not-covered");
    fixture.exclude_from_manifest("positions/p01-mlp/capture.raw");
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Missing);
}

#[test]
fn t48_ingest_rejects_listed_missing_file() {
    let mut fixture = Fixture::new("listed-missing");
    fixture.add_manifest_line(&format!("{}  notes/missing.txt", sha256_hex(b"missing")));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Missing);
}

#[test]
fn t48_ingest_rejects_unsafe_manifest_path() {
    let mut fixture = Fixture::new("unsafe-path");
    fixture.add_manifest_line(&format!("{}  ../escape.raw", sha256_hex(b"escape")));
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Unsupported);
    assert_eq!(error.code(), "E_INGEST_UNSUPPORTED");
}

#[test]
fn t48_ingest_rejects_hash_mismatch() {
    let mut fixture = Fixture::new("hash-mismatch");
    let root = fixture.materialize();
    fs::write(
        root.join("positions/p01-mlp/capture.raw"),
        vec![1u8; FRAMES as usize * 3],
    )
    .expect("tamper capture");
    let error = expect_err(&root, IngestErrorKind::Hash);
    assert_eq!(error.code(), "E_INGEST_HASH");
}

#[test]
fn t48_ingest_rejects_capture_not_frame_aligned() {
    let mut fixture = Fixture::new("frame-aligned");
    fixture.positions[0].capture.push(0);
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Audio);
    assert_eq!(error.code(), "E_INGEST_AUDIO");
}

#[test]
fn t48_ingest_rejects_analysis_sample_rate_mismatch() {
    let mut fixture = Fixture::new("analysis-rate");
    set_path(
        &mut fixture.positions[0].measurements[0].analysis,
        &["meta", "sample_rate"],
        J::n(96_000.0),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Schema);
}

#[test]
fn t48_ingest_rejects_analysis_channel_mismatch() {
    let mut fixture = Fixture::new("analysis-channel");
    fixture.positions[0].measurements[0]
        .analysis
        .set("channel", J::n(2.0));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Schema);
}

#[test]
fn t48_ingest_rejects_analysis_position_label_mismatch() {
    let mut fixture = Fixture::new("analysis-position");
    set_path(
        &mut fixture.positions[0].measurements[0].analysis,
        &["position", "name"],
        J::s("OTHER"),
    );
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Schema);
}

#[test]
fn t48_ingest_rejects_analysis_invalid_clock() {
    let mut fixture = Fixture::new("analysis-clock");
    fixture.positions[0].measurements[0]
        .analysis
        .set("clock_ppm_ok", J::b(false));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Timebase);
}

#[test]
fn t48_ingest_rejects_analysis_measurement_role_reference() {
    let mut fixture = Fixture::new("analysis-reference");
    fixture.positions[0].measurements[0]
        .analysis
        .set("measurement_role", J::s("reference"));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Unsupported);
}

#[test]
fn t48_ingest_rejects_position_weight_zero() {
    let mut fixture = Fixture::new("weight");
    fixture.positions[0].meta.set("weight", J::n(0.0));
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Range);
    assert_eq!(error.code(), "E_INGEST_RANGE");
}

#[test]
fn t48_ingest_rejects_invalid_position_role() {
    let mut fixture = Fixture::new("role");
    fixture.positions[0].meta.set("role", J::s("pilot"));
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Schema);
    assert_eq!(error.code(), "E_INGEST_SCHEMA");
}

#[test]
fn t48_ingest_rejects_duplicate_position_label() {
    let mut fixture = Fixture::new("dup-label");
    fixture.positions[1].meta.set("name", J::s("P01"));
    let root = fixture.materialize();
    let error = expect_err(&root, IngestErrorKind::Duplicate);
    assert_eq!(error.code(), "E_INGEST_DUPLICATE");
}

#[test]
fn t48_ingest_rejects_bundle_without_design_position() {
    let mut fixture = Fixture::new("no-design");
    fixture.positions[0].meta.set("role", J::s("validation"));
    fixture.positions[1].meta.set("role", J::s("validation"));
    let root = fixture.materialize();
    expect_err(&root, IngestErrorKind::Missing);
}

#[test]
fn t48_ingest_rejects_missing_positions_dir() {
    let mut fixture = Fixture::new("no-positions-dir");
    let root = fixture.materialize();
    fs::remove_dir_all(root.join("positions")).expect("remove positions");
    expect_err(&root, IngestErrorKind::Missing);
}

// ===========================================================================
// warning（非致命）
// ===========================================================================

#[test]
fn t48_ingest_warns_low_repeats_and_unknown_fields() {
    let mut fixture = Fixture::new("warnings");
    fixture
        .session
        .set("future_field", J::s("kept for forward compatibility"));
    fixture.set_participant_repeats(2);
    let root = fixture.materialize();
    let bundle = expect_ok(&root);
    assert_eq!(bundle.session.stimulus.repeats, 2);
    let codes: BTreeSet<&str> = bundle.warnings.iter().map(|warning| warning.code).collect();
    assert!(
        codes.contains("W_INGEST_UNKNOWN_FIELD"),
        "warnings: {:?}",
        bundle.warnings
    );
    assert!(
        codes.contains("W_INGEST_LOW_REPEATS"),
        "warnings: {:?}",
        bundle.warnings
    );
    assert_eq!(
        bundle.positions[0].measurements[0].diag.repeat_starts.len(),
        2
    );
}
