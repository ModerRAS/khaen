//! 自造 mini fixture 的 roundtrip 测试（不依赖 C# 生成器）：
//! 用 crate 自己的 ISM+RIR 生成 2 源 × 2 测点小场景 WAV + ground-truth.json（含 SHA256），
//! 再走 SceneFixture::load → h_matrix → rerender_check，验证加载/校验/重渲染全链路。
//! 另一变体故意去掉 rir.scale，验证按 1.0 处理并记录警告。

use std::path::{Path, PathBuf};

use khaen_room_sim::fft::rfft;
use khaen_room_sim::fixture::SceneFixture;
use khaen_room_sim::ism::{arrivals, ArrivalPlacement};
use khaen_room_sim::rir::render;
use khaen_room_sim::room::{Position, RoomModel, SurfaceReflections};
use khaen_room_sim::sha256::file_hex;
use khaen_room_sim::wav::{read_f32_mono, write_f32_mono};

const SAMPLE_RATE_HZ: u32 = 8000;
const FFT_SIZE: usize = 256;
const RIR_LENGTH: usize = 256;
const ORDER: u32 = 4;
const REFERENCE_DISTANCE_M: f64 = 0.5;
const WAV_SCALE: f64 = 0.5;

fn scratch_root(tag: &str) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let root = base.join(format!(
        "khaen-room-sim-mini-{tag}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn mini_room() -> RoomModel {
    RoomModel::new(
        3.0,
        2.0,
        2.5,
        343.0,
        SurfaceReflections::new(0.8, 0.7, 0.9, 0.6, 0.5, 0.4),
    )
}

fn mini_sources() -> Vec<(&'static str, Position, f64)> {
    vec![
        ("SUB1", Position::new(0.55, 0.6, 0.3), 1.0),
        ("SUB2", Position::new(1.4, 1.3, 0.4), -1.0),
    ]
}

fn mini_mics() -> Vec<(&'static str, Position, &'static str)> {
    vec![
        ("P01", Position::new(1.1, 0.8, 1.2), "design"),
        ("P02", Position::new(2.0, 1.5, 1.4), "holdout"),
    ]
}

const GROUND_TRUTH_TEMPLATE: &str = r#"{
  "format": "khaen-roomforge-mimo-fixture/v1",
  "generator": "khaen-room-sim mini fixture test",
  "generator_version": "0.0.0",
  "scene_id": "scene-mini",
  "seed": 42,
  "sample_rate_hz": 8000,
  "rir": {
    "format": "f32",
    "channels": 1,
    "length_samples": 256,
    "fft_size": 256,
    "scale": 0.5
  },
  "sweep": {
    "start_hz": 20,
    "end_hz": 150,
    "seconds": 1
  },
  "room": {
    "length_m": 3,
    "width_m": 2,
    "height_m": 2.5,
    "speed_of_sound_m_s": 343,
    "reflections": {
      "left": 0.8,
      "right": 0.7,
      "front": 0.9,
      "rear": 0.6,
      "floor": 0.5,
      "ceiling": 0.4
    },
    "sabine_t60_s": __SABINE__
  },
  "image_source": {
    "order": 4,
    "placement": "linear_interpolated",
    "reference_distance_m": 0.5
  },
  "sources": [
    {
      "id": "SUB1",
      "index": 0,
      "position_m": [0.55, 0.6, 0.3],
      "polarity": 1,
      "delay_ms": 0,
      "gain_db": 0
    },
    {
      "id": "SUB2",
      "index": 1,
      "position_m": [1.4, 1.3, 0.4],
      "polarity": -1,
      "delay_ms": 0,
      "gain_db": 0
    }
  ],
  "mics": [
    {
      "id": "P01",
      "role": "design",
      "position_m": [1.1, 0.8, 1.2]
    },
    {
      "id": "P02",
      "role": "holdout",
      "position_m": [2.0, 1.5, 1.4]
    }
  ],
  "design_mic_ids": ["P01"],
  "holdout_mic_ids": ["P02"],
  "frequency_grid_hz": [1000.0, 1250.0, 1500.0],
  "bands": {
    "spec_centers_hz": [1000, 1250],
    "evaluated_spec_centers_hz": [1000],
    "na_spec_centers_hz": [1250],
    "extended_centers_hz": [1000],
    "band_edges_sixth_oct": true
  },
  "rir_files": [
    {"source": "SUB1", "mic": "P01", "path": "rir/SUB1_P01.wav", "sha256": "__SHA_0_0__", "samples": 256},
    {"source": "SUB1", "mic": "P02", "path": "rir/SUB1_P02.wav", "sha256": "__SHA_0_1__", "samples": 256},
    {"source": "SUB2", "mic": "P01", "path": "rir/SUB2_P01.wav", "sha256": "__SHA_1_0__", "samples": 256},
    {"source": "SUB2", "mic": "P02", "path": "rir/SUB2_P02.wav", "sha256": "__SHA_1_1__", "samples": 256}
  ]
}
"#;

/// 生成 mini 夹具；include_scale=false 时去掉 rir.scale 且 WAV 按 scale=1.0 写。
fn build_mini_fixture(root: &Path, include_scale: bool) -> PathBuf {
    let scene_dir = root.join(if include_scale {
        "scene-mini"
    } else {
        "scene-mini-noscale"
    });
    std::fs::create_dir_all(scene_dir.join("rir")).unwrap();

    let room = mini_room();
    let wav_scale = if include_scale { WAV_SCALE } else { 1.0 };
    let mut hashes = vec![vec![String::new(); 2]; 2];
    let mut max_abs_sample: f64 = 0.0;

    for (source_index, (_source_id, source_position, polarity)) in mini_sources().iter().enumerate()
    {
        for (mic_index, (_mic_id, mic_position, _role)) in mini_mics().iter().enumerate() {
            let paths = arrivals(
                &room,
                *source_position,
                *mic_position,
                ORDER,
                REFERENCE_DISTANCE_M,
            );
            let pressure = render(
                &paths,
                SAMPLE_RATE_HZ as f64,
                ArrivalPlacement::LinearInterpolated,
                RIR_LENGTH,
            );
            let samples: Vec<f32> = pressure
                .iter()
                .map(|sample| (wav_scale * polarity * sample) as f32)
                .collect();
            for sample in &samples {
                max_abs_sample = max_abs_sample.max(sample.abs() as f64);
            }
            let relative_path = format!(
                "rir/{}_{}.wav",
                mini_sources()[source_index].0,
                mini_mics()[mic_index].0
            );
            let wav_path = scene_dir.join(&relative_path);
            write_f32_mono(&wav_path, &samples, SAMPLE_RATE_HZ).unwrap();
            hashes[source_index][mic_index] = file_hex(&wav_path).unwrap();
        }
    }
    assert!(
        max_abs_sample < 0.99,
        "mini fixture WAV would clamp: max |sample| = {max_abs_sample}"
    );

    let mut document = GROUND_TRUTH_TEMPLATE.to_string();
    document = document.replace("__SABINE__", &format!("{:?}", mini_room().sabine_t60_s()));
    for source_index in 0..2 {
        for mic_index in 0..2 {
            document = document.replace(
                &format!("__SHA_{source_index}_{mic_index}__"),
                &hashes[source_index][mic_index],
            );
        }
    }
    if !include_scale {
        // 用另一个键名替换，等价于 rir.scale 缺失（加载器只读已知键）。
        document = document.replace(r#""scale": 0.5"#, r#""scale_removed_for_test": true"#);
    }
    std::fs::write(scene_dir.join("ground-truth.json"), document).unwrap();

    // SHA256SUMS 也写一份，模拟真实夹具布局（load 不读它，但保持形状一致）。
    let mut sums = String::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for source_index in 0..2 {
        for mic_index in 0..2 {
            files.push((
                format!(
                    "rir/{}_{}.wav",
                    mini_sources()[source_index].0,
                    mini_mics()[mic_index].0
                ),
                hashes[source_index][mic_index].clone(),
            ));
        }
    }
    files.push((
        "ground-truth.json".to_string(),
        file_hex(&scene_dir.join("ground-truth.json")).unwrap(),
    ));
    files.sort();
    for (path, hash) in files {
        sums.push_str(&format!("{hash}  {path}\n"));
    }
    std::fs::write(scene_dir.join("SHA256SUMS"), sums).unwrap();

    scene_dir
}

#[test]
fn mini_fixture_load_h_matrix_and_rerender_roundtrip() {
    let root = scratch_root("with-scale");
    let scene_dir = build_mini_fixture(&root, true);

    let fixture = SceneFixture::load(&scene_dir).expect("mini fixture must load");
    assert_eq!(fixture.scene_id, "scene-mini");
    assert_eq!(fixture.sample_rate_hz, SAMPLE_RATE_HZ);
    assert_eq!(fixture.sources.len(), 2);
    assert_eq!(fixture.mics.len(), 2);
    assert_eq!(fixture.design_mics(), vec![0]);
    assert_eq!(fixture.holdout_mics(), vec![1]);
    assert_eq!(fixture.rir.scale, WAV_SCALE);
    assert_eq!(fixture.rir.length_samples, RIR_LENGTH);
    assert_eq!(fixture.rir.fft_size, FFT_SIZE);
    assert_eq!(fixture.frequency_grid_hz, vec![1000.0, 1250.0, 1500.0]);
    assert_eq!(fixture.bands.spec_bands().len(), 2);
    assert_eq!(fixture.bands.evaluated_bands().len(), 1);
    assert_eq!(fixture.bands.extended_bands().len(), 1);
    assert!(
        fixture.warnings.is_empty(),
        "unexpected fixture warnings: {:?}",
        fixture.warnings
    );

    // H(m,n,k)：对 (mic=1, source=1) 的 bin 48（1500 Hz）与独立重算比对。
    let matrix = fixture.h_matrix().expect("h_matrix");
    assert_eq!(matrix.n_sources, 2);
    assert_eq!(matrix.n_mics, 2);
    assert_eq!(matrix.n_bins(), 3);
    assert_eq!(matrix.freqs_hz, vec![1000.0, 1250.0, 1500.0]);
    let decoded = read_f32_mono(&scene_dir.join("rir/SUB2_P02.wav")).unwrap();
    let pressure: Vec<f64> = decoded
        .samples
        .iter()
        .map(|sample| *sample as f64 / WAV_SCALE)
        .collect();
    let spectrum = rfft(&pressure, FFT_SIZE);
    let expected = spectrum[48];
    let actual = matrix.at(1, 1, 2);
    assert!(
        (actual - expected).abs() < 1e-12,
        "H mismatch: {actual} vs {expected}"
    );

    let report = fixture.rerender_check().expect("rerender_check");
    assert_eq!(report.pairs.len(), 4);
    assert!(report.passed, "rerender failed: {report:?}");
    assert!(
        report.max_abs_delta < 1e-6,
        "max |delta| = {}",
        report.max_abs_delta
    );
    assert!(
        report.max_relative_delta < 1e-6,
        "max relative delta = {}",
        report.max_relative_delta
    );
    // WAV 由本 crate 以 (float) 写回；期望值按 f32 舍入后应逐位相等。
    assert!(
        report.f32_bit_exact,
        "expected f32 bit-exactness, max f32-rounded delta = {}",
        report.f32_round_max_abs_delta
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mini_fixture_without_scale_defaults_to_one_with_warning() {
    let root = scratch_root("without-scale");
    let scene_dir = build_mini_fixture(&root, false);

    let fixture = SceneFixture::load(&scene_dir).expect("mini fixture without scale must load");
    assert_eq!(fixture.rir.scale, 1.0);
    assert!(
        fixture
            .warnings
            .iter()
            .any(|warning| warning.contains("rir.scale missing")),
        "expected missing-scale warning, got {:?}",
        fixture.warnings
    );
    let report = fixture.rerender_check().expect("rerender_check");
    assert!(report.passed, "rerender failed: {report:?}");
    assert!(report.max_abs_delta < 1e-6);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mini_fixture_rejects_tampered_wav() {
    let root = scratch_root("tampered");
    let scene_dir = build_mini_fixture(&root, true);

    let wav_path = scene_dir.join("rir/SUB1_P01.wav");
    let mut bytes = std::fs::read(&wav_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&wav_path, bytes).unwrap();

    let error = SceneFixture::load(&scene_dir).expect_err("tampered WAV must be rejected");
    assert!(
        error.contains("SHA256 mismatch"),
        "unexpected error: {error}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
