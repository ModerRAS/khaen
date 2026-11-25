//! 生成夹具的 5 场景 load + h_matrix + rerender_check 验证（默认 ignored）。
//!
//! 运行：
//!   $env:TSPI_ROOM_SIM_FIXTURES='...\fixtures'                              # 夹具根目录（含 scene-*/）
//!   $env:TSPI_ROOM_SIM_VALIDATION='...\validation\room-sim-render-check.json'  # 输出 JSON 文件路径（不是目录！）
//!   cargo test -p khaen-room-sim --test fixtures_validation --offline -- --ignored --nocapture
//!
//! 注意：TSPI_ROOM_SIM_VALIDATION 必须是**文件路径**；误传目录会 os error 5（本测试对目录形式
//! 会补默认文件名，但脚本调用请直接给文件路径）。
//!
//! 产出 UTF-8 JSON：每场景 K/N/M、max|Δ|（WAV 单位）、相对误差、最坏 pair、SHA256SUMS 交叉校验。

use std::path::{Path, PathBuf};

use khaen_room_sim::fixture::SceneFixture;
use khaen_room_sim::sha256::file_hex;

fn json_string(value: &str) -> String {
    format!("{value:?}")
}

fn json_string_array(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|value| json_string(value)).collect();
    format!("[{}]", items.join(", "))
}

fn directory_scenes(fixtures_dir: &Path) -> Vec<PathBuf> {
    let mut scenes: Vec<PathBuf> = std::fs::read_dir(fixtures_dir)
        .unwrap_or_else(|error| {
            panic!(
                "cannot read fixtures dir {}: {error}",
                fixtures_dir.display()
            )
        })
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.is_dir() && path.join("ground-truth.json").is_file() {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    scenes.sort();
    scenes
}

fn verify_sha256sums(scene_dir: &Path, scene_id: &str, expected_entries: usize) -> (usize, String) {
    let path = scene_dir.join("SHA256SUMS");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{scene_id}: cannot read {}: {error}", path.display()));
    let mut checked = 0usize;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (expected_hash, relative_path) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("{scene_id}: malformed SHA256SUMS line '{line}'"));
        assert_eq!(
            expected_hash.len(),
            64,
            "{scene_id}: SHA256SUMS hash '{expected_hash}' is not 64 hex chars"
        );
        let file_path = scene_dir.join(relative_path);
        let actual_hash = file_hex(&file_path)
            .unwrap_or_else(|error| panic!("{scene_id}: {relative_path}: {error}"));
        assert_eq!(
            actual_hash, expected_hash,
            "{scene_id}: SHA256SUMS mismatch for {relative_path}"
        );
        checked += 1;
    }
    assert_eq!(
        checked, expected_entries,
        "{scene_id}: SHA256SUMS covers {checked} files, expected {expected_entries} (WAVs + ground-truth.json)"
    );
    (
        checked,
        file_hex(&scene_dir.join("ground-truth.json")).unwrap(),
    )
}

#[test]
#[ignore = "needs generated fixtures: set TSPI_ROOM_SIM_FIXTURES (and optionally TSPI_ROOM_SIM_VALIDATION)"]
fn generated_fixtures_render_check() {
    let fixtures_dir = match std::env::var("TSPI_ROOM_SIM_FIXTURES") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            eprintln!("TSPI_ROOM_SIM_FIXTURES not set; skipping generated-fixture validation");
            return;
        }
    };
    let validation_path = std::env::var("TSPI_ROOM_SIM_VALIDATION")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    let scenes = directory_scenes(&fixtures_dir);
    assert_eq!(
        scenes.len(),
        5,
        "expected 5 generated scenes under {}, found {}: {:?}",
        fixtures_dir.display(),
        scenes.len(),
        scenes
    );

    let mut scene_json = Vec::new();
    let mut overall_max_abs_delta: f64 = 0.0;
    let mut overall_max_relative_delta: f64 = 0.0;
    let mut overall_max_relative_rms: f64 = 0.0;
    let mut overall_f32_round_max_abs_delta: f64 = 0.0;
    let mut scene_count = 0usize;

    for scene_dir in &scenes {
        let fixture = SceneFixture::load(scene_dir)
            .unwrap_or_else(|error| panic!("load {} failed: {error}", scene_dir.display()));

        // 契约 §2/§3 的固定量（每个场景都应成立）。
        assert_eq!(
            fixture.frequency_grid_hz.len(),
            69,
            "{}: expected K = 69, got {}",
            fixture.scene_id,
            fixture.frequency_grid_hz.len()
        );
        assert!(
            (fixture.frequency_grid_hz[0] - 19.775_390_625).abs() < 1e-9,
            "{}: expected first grid point 19.775390625 Hz, got {}",
            fixture.scene_id,
            fixture.frequency_grid_hz[0]
        );
        let last_grid = *fixture.frequency_grid_hz.last().unwrap();
        assert!(
            (last_grid - 146.484_375).abs() < 1e-9,
            "{}: expected last grid point 146.484375 Hz, got {last_grid}",
            fixture.scene_id
        );
        assert_eq!(
            fixture.rir.scale, 0.5,
            "{}: expected rir.scale 0.5, got {}",
            fixture.scene_id, fixture.rir.scale
        );
        assert_eq!(
            fixture.rir.length_samples, 24_000,
            "{}: expected 24000-sample RIR",
            fixture.scene_id
        );
        assert_eq!(
            fixture.rir.fft_size, 65_536,
            "{}: expected 65536-point FFT",
            fixture.scene_id
        );
        assert_eq!(
            fixture.sample_rate_hz, 48_000,
            "{}: expected 48 kHz",
            fixture.scene_id
        );
        assert_eq!(
            fixture.image_source.order, 12,
            "{}: expected ISM order 12",
            fixture.scene_id
        );
        assert_eq!(
            fixture.image_source.placement,
            khaen_room_sim::ism::ArrivalPlacement::LinearInterpolated,
            "{}: expected linear_interpolated placement",
            fixture.scene_id
        );
        assert_eq!(
            fixture.sources.len() * fixture.mics.len(),
            fixture.rirs.len()
        );

        let (sha256sums_entries, ground_truth_sha256) =
            verify_sha256sums(scene_dir, &fixture.scene_id, fixture.rirs.len() + 1);

        let matrix = fixture
            .h_matrix()
            .unwrap_or_else(|error| panic!("{}: h_matrix failed: {error}", fixture.scene_id));
        assert_eq!(matrix.n_sources, fixture.sources.len());
        assert_eq!(matrix.n_mics, fixture.mics.len());
        assert_eq!(matrix.n_bins(), fixture.frequency_grid_hz.len());
        assert!(
            matrix
                .h
                .iter()
                .all(|value| value.re.is_finite() && value.im.is_finite()),
            "{}: non-finite H values",
            fixture.scene_id
        );
        let max_abs_h = matrix
            .h
            .iter()
            .fold(0.0f64, |best, value| best.max(value.abs()));

        let report = fixture
            .rerender_check()
            .unwrap_or_else(|error| panic!("{}: rerender_check failed: {error}", fixture.scene_id));
        assert_eq!(report.pairs.len(), fixture.rirs.len());
        assert!(
            report.passed,
            "{}: rerender check failed: max|delta| = {:?}, max relative = {:?}, tolerance = {:?}",
            fixture.scene_id, report.max_abs_delta, report.max_relative_delta, report.tolerance
        );
        assert_eq!(
            report.f32_round_max_abs_delta, 0.0,
            "{}: expected f32-quantised bit-exactness, max f32-rounded |delta| = {:?}",
            fixture.scene_id, report.f32_round_max_abs_delta
        );
        let worst_pair = report
            .pairs
            .iter()
            .max_by(|left, right| {
                left.max_abs_delta
                    .partial_cmp(&right.max_abs_delta)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();

        eprintln!(
            "{}: K={} N={} M={} max|delta|={:.3e} max_rel={:.3e} max_rel_rms={:.3e} f32_round_max|delta|={:.1e} f32_bit_exact={} max|H|={:.6} passed={}",
            fixture.scene_id,
            matrix.n_bins(),
            matrix.n_sources,
            matrix.n_mics,
            report.max_abs_delta,
            report.max_relative_delta,
            report.max_relative_rms,
            report.f32_round_max_abs_delta,
            report.f32_bit_exact,
            max_abs_h,
            report.passed
        );

        overall_max_abs_delta = overall_max_abs_delta.max(report.max_abs_delta);
        overall_max_relative_delta = overall_max_relative_delta.max(report.max_relative_delta);
        overall_max_relative_rms = overall_max_relative_rms.max(report.max_relative_rms);
        overall_f32_round_max_abs_delta =
            overall_f32_round_max_abs_delta.max(report.f32_round_max_abs_delta);
        scene_count += 1;

        scene_json.push(format!(
            r#"    {{
      "scene_id": {scene_id},
      "n_sources": {n_sources},
      "n_mics": {n_mics},
      "k_bins": {k_bins},
      "fft_size": {fft_size},
      "sample_rate_hz": {sample_rate_hz},
      "rir_length_samples": {rir_length_samples},
      "scale": {scale:?},
      "rir_files": {rir_files},
      "sha256sums_entries": {sha256sums_entries},
      "ground_truth_sha256": {ground_truth_sha256},
      "max_abs_delta": {max_abs_delta:?},
      "max_relative_delta": {max_relative_delta:?},
      "max_relative_rms": {max_relative_rms:?},
      "f32_round_max_abs_delta": {f32_round_max_abs_delta:?},
      "f32_bit_exact": {f32_bit_exact},
      "worst_pair": {{"source": {worst_source}, "mic": {worst_mic}, "max_abs_delta": {worst_delta:?}, "max_abs_expected": {worst_expected:?}, "max_relative_delta": {worst_relative:?}}},
      "max_abs_h": {max_abs_h:?},
      "passed": true,
      "warnings": {warnings}
    }}"#,
            scene_id = json_string(&fixture.scene_id),
            n_sources = matrix.n_sources,
            n_mics = matrix.n_mics,
            k_bins = matrix.n_bins(),
            fft_size = fixture.rir.fft_size,
            sample_rate_hz = fixture.sample_rate_hz,
            rir_length_samples = fixture.rir.length_samples,
            scale = fixture.rir.scale,
            rir_files = fixture.rirs.len(),
            sha256sums_entries = sha256sums_entries,
            ground_truth_sha256 = json_string(&ground_truth_sha256),
            max_abs_delta = report.max_abs_delta,
            max_relative_delta = report.max_relative_delta,
            max_relative_rms = report.max_relative_rms,
            f32_round_max_abs_delta = report.f32_round_max_abs_delta,
            f32_bit_exact = report.f32_bit_exact,
            worst_source = json_string(&worst_pair.source_id),
            worst_mic = json_string(&worst_pair.mic_id),
            worst_delta = worst_pair.max_abs_delta,
            worst_expected = worst_pair.max_abs_expected,
            worst_relative = worst_pair.max_relative_delta,
            max_abs_h = max_abs_h,
            warnings = json_string_array(&report.warnings),
        ));
    }

    let document = format!(
        r#"{{
  "tool": "khaen-room-sim",
  "kind": "SIMULATED / UNPROVEN",
  "contract": "rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/CONTRACT.md",
  "command": "cargo test -p khaen-room-sim --test fixtures_validation --offline -- --ignored generated_fixtures_render_check",
  "fixtures_dir": {fixtures_dir},
  "tolerance": {tolerance:?},
  "scenes": [
{scenes}
  ],
  "overall": {{
    "scene_count": {scene_count},
    "max_abs_delta": {max_abs_delta:?},
    "max_relative_delta": {max_relative_delta:?},
    "max_relative_rms": {max_relative_rms:?},
    "f32_round_max_abs_delta": {f32_round_max_abs_delta:?},
    "f32_bit_exact": {f32_bit_exact},
    "passed": true
  }},
  "notes": [
    "WAV sample = 0.5 * polarity * p_ISM (contract 4); H uses p = wav / 0.5.",
    "rerender compares rust_p * scale * polarity against the f32 WAV; the residual is f32 quantization + libm pow rounding.",
    "H(f) = rfft(wav/scale zero padded to fft_size) at the exact frequency_grid_hz bins.",
    "All numbers are SIMULATED; they are cross-language porting checks, not measurements."
  ]
}}
"#,
        fixtures_dir = json_string(&fixtures_dir.display().to_string()),
        tolerance = khaen_room_sim::fixture::RERENDER_TOLERANCE,
        scenes = scene_json.join(",\n"),
        scene_count = scene_count,
        max_abs_delta = overall_max_abs_delta,
        max_relative_delta = overall_max_relative_delta,
        max_relative_rms = overall_max_relative_rms,
        f32_round_max_abs_delta = overall_f32_round_max_abs_delta,
        f32_bit_exact = overall_f32_round_max_abs_delta == 0.0,
    );

    if let Some(raw_path) = &validation_path {
        let path = resolve_validation_path(raw_path);
        write_validation(&path, &document).unwrap_or_else(|error| {
            panic!(
                "cannot write {}: {error} (hint: TSPI_ROOM_SIM_VALIDATION may point at a directory or a read-only file)",
                path.display()
            )
        });
        eprintln!("wrote validation JSON: {}", path.display());
    } else {
        eprintln!("TSPI_ROOM_SIM_VALIDATION not set; validation JSON not written");
        eprintln!("{document}");
    }
}

/// TSPI_ROOM_SIM_VALIDATION 允许直接给 JSON 文件路径；给目录（含尾部反斜杠）时自动补默认文件名。
fn resolve_validation_path(raw: &Path) -> PathBuf {
    if raw.is_dir() {
        return raw.join("room-sim-render-check.json");
    }
    match raw.file_name() {
        Some(name) if name.to_string_lossy().contains('.') => raw.to_path_buf(),
        _ => raw.join("room-sim-render-check.json"),
    }
}

/// 创建父目录后写文件；若目标已存在且只读/被占用，先删除再写（Windows 上只读属性会拒绝覆盖）。
fn write_validation(path: &Path, document: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    match std::fs::write(path, document) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            if path.exists() {
                std::fs::remove_file(path)?;
                std::fs::write(path, document)
            } else {
                Err(first_error)
            }
        }
    }
}
