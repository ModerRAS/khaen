//! 合成夹具生成器 + 测试临时目录辅助（mimo-opt CLI 集成测试共用）。
//!
//! 不依赖仓库外夹具：用 khaen-room-sim 自己的 ISM+RIR 生成 2 源 × 2 design + 2 holdout
//! 的小场景（fs=8000 / fft=4096 / RIR=4096），再走 mimo-opt 的真实夹具加载路径
//! （ground-truth.json SHA256 校验 → H(f) → 设计流水线）。

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use khaen_room_sim::fixture::SceneFixture;
use khaen_room_sim::ism::{arrivals, ArrivalPlacement};
use khaen_room_sim::rir::render;
use khaen_room_sim::room::{Position, RoomModel, SurfaceReflections};
use khaen_room_sim::sha256::file_hex;
use khaen_room_sim::wav::write_f32_mono;

pub const SAMPLE_RATE_HZ: u32 = 8000;
pub const FFT_SIZE: usize = 4096;
pub const RIR_LENGTH: usize = 4096;
pub const ORDER: u32 = 2;
pub const REFERENCE_DISTANCE_M: f64 = 0.5;
pub const WAV_SCALE: f64 = 0.5;

/// 频点必须精确落在 FFT bin 上（h_matrix 对非精确 bin 直接报错）：
/// fs=8000 / fft=4096 → 1.953125 Hz 分辨率。
pub const FREQ_GRID_HZ: [f64; 6] = [39.0625, 46.875, 62.5, 80.078125, 99.609375, 125.0];
/// 1/3 oct evaluated 频带中心（每个中心恰好覆盖上面一个频点）。
pub const BAND_CENTERS_HZ: [f64; 6] = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0];

/// 测试临时根目录：优先系统 temp，不可写时退回 current_dir（cargo 包根）。
pub struct Scratch {
    pub root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // 取证/调试时设 KHAEN_MIMO_OPT_TEST_KEEP=1 保留临时目录并打印路径。
        if std::env::var_os("KHAEN_MIMO_OPT_TEST_KEEP").is_some() {
            eprintln!("KHAEN_MIMO_OPT_TEST_KEEP: {}", self.root.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

pub fn scratch(tag: &str) -> Scratch {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let name = format!("khaen-mimo-opt-test-{tag}-{}-{stamp}", std::process::id());
    let candidates = [
        std::env::temp_dir(),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ];
    for base in candidates {
        let root = base.join(&name);
        if std::fs::create_dir_all(&root).is_ok() {
            return Scratch { root };
        }
    }
    panic!("无法创建测试临时目录: {name}");
}

fn room() -> RoomModel {
    RoomModel::new(
        4.0,
        3.0,
        2.5,
        343.0,
        SurfaceReflections::new(0.70, 0.60, 0.80, 0.55, 0.45, 0.40),
    )
}

/// (id, position, polarity)
fn sources() -> Vec<(&'static str, Position, f64)> {
    vec![
        ("SUB1", Position::new(0.8, 0.7, 0.35), 1.0),
        ("SUB2", Position::new(2.2, 1.9, 0.45), -1.0),
    ]
}

/// (id, role, position)
fn mics() -> Vec<(&'static str, &'static str, Position)> {
    vec![
        ("P01", "design", Position::new(1.5, 1.0, 1.20)),
        ("P02", "design", Position::new(2.6, 1.4, 1.25)),
        ("P03", "holdout", Position::new(3.2, 2.2, 1.30)),
        ("P04", "holdout", Position::new(1.1, 2.4, 1.15)),
    ]
}

const GROUND_TRUTH_TEMPLATE: &str = r#"{
  "format": "khaen-roomforge-mimo-fixture/v1",
  "generator": "khaen-mimo-opt CLI integration test",
  "generator_version": "0.0.0",
  "scene_id": "scene-cli-smoke",
  "seed": 20261125,
  "sample_rate_hz": 8000,
  "rir": {
    "format": "f32",
    "channels": 1,
    "length_samples": 4096,
    "fft_size": 4096,
    "scale": 0.5
  },
  "sweep": {
    "start_hz": 20,
    "end_hz": 150,
    "seconds": 0.5
  },
  "room": {
    "length_m": 4,
    "width_m": 3,
    "height_m": 2.5,
    "speed_of_sound_m_s": 343,
    "reflections": {
      "left": 0.70,
      "right": 0.60,
      "front": 0.80,
      "rear": 0.55,
      "floor": 0.45,
      "ceiling": 0.40
    },
    "sabine_t60_s": __SABINE__
  },
  "image_source": {
    "order": 2,
    "placement": "linear_interpolated",
    "reference_distance_m": 0.5
  },
  "sources": [
    {
      "id": "SUB1",
      "index": 0,
      "position_m": [0.8, 0.7, 0.35],
      "polarity": 1,
      "delay_ms": 0,
      "gain_db": 0
    },
    {
      "id": "SUB2",
      "index": 1,
      "position_m": [2.2, 1.9, 0.45],
      "polarity": -1,
      "delay_ms": 0,
      "gain_db": 0
    }
  ],
  "mics": [
    {"id": "P01", "role": "design", "position_m": [1.5, 1.0, 1.20]},
    {"id": "P02", "role": "design", "position_m": [2.6, 1.4, 1.25]},
    {"id": "P03", "role": "holdout", "position_m": [3.2, 2.2, 1.30]},
    {"id": "P04", "role": "holdout", "position_m": [1.1, 2.4, 1.15]}
  ],
  "design_mic_ids": ["P01", "P02"],
  "holdout_mic_ids": ["P03", "P04"],
  "frequency_grid_hz": [39.0625, 46.875, 62.5, 80.078125, 99.609375, 125.0],
  "bands": {
    "spec_centers_hz": [40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    "evaluated_spec_centers_hz": [40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    "na_spec_centers_hz": [],
    "extended_centers_hz": [25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    "band_edges_sixth_oct": true
  },
  "rir_files": [
    {"source": "SUB1", "mic": "P01", "path": "rir/SUB1_P01.wav", "sha256": "__SHA_0_0__", "samples": 4096},
    {"source": "SUB1", "mic": "P02", "path": "rir/SUB1_P02.wav", "sha256": "__SHA_0_1__", "samples": 4096},
    {"source": "SUB1", "mic": "P03", "path": "rir/SUB1_P03.wav", "sha256": "__SHA_0_2__", "samples": 4096},
    {"source": "SUB1", "mic": "P04", "path": "rir/SUB1_P04.wav", "sha256": "__SHA_0_3__", "samples": 4096},
    {"source": "SUB2", "mic": "P01", "path": "rir/SUB2_P01.wav", "sha256": "__SHA_1_0__", "samples": 4096},
    {"source": "SUB2", "mic": "P02", "path": "rir/SUB2_P02.wav", "sha256": "__SHA_1_1__", "samples": 4096},
    {"source": "SUB2", "mic": "P03", "path": "rir/SUB2_P03.wav", "sha256": "__SHA_1_2__", "samples": 4096},
    {"source": "SUB2", "mic": "P04", "path": "rir/SUB2_P04.wav", "sha256": "__SHA_1_3__", "samples": 4096}
  ]
}
"#;

/// 在 root 下生成一个 scene-cli-smoke 夹具目录，返回 (fixture_root, scene_dir)。
pub fn build_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let scene_dir = root.join("scene-cli-smoke");
    std::fs::create_dir_all(scene_dir.join("rir")).unwrap();

    let room = room();
    let mut hashes = vec![vec![String::new(); mics().len()]; sources().len()];
    let mut max_abs_sample: f64 = 0.0;
    for (source_index, (_source_id, source_position, polarity)) in sources().iter().enumerate() {
        for (mic_index, (_mic_id, _role, mic_position)) in mics().iter().enumerate() {
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
                .map(|sample| (WAV_SCALE * polarity * sample) as f32)
                .collect();
            for sample in &samples {
                max_abs_sample = max_abs_sample.max(sample.abs() as f64);
            }
            let relative_path = format!(
                "rir/{}_{}.wav",
                sources()[source_index].0,
                mics()[mic_index].0
            );
            let wav_path = scene_dir.join(&relative_path);
            write_f32_mono(&wav_path, &samples, SAMPLE_RATE_HZ).unwrap();
            hashes[source_index][mic_index] = file_hex(&wav_path).unwrap();
        }
    }
    assert!(
        max_abs_sample < 0.99,
        "fixture WAV would clamp: {max_abs_sample}"
    );

    let mut document = GROUND_TRUTH_TEMPLATE.to_string();
    document = document.replace("__SABINE__", &format!("{:?}", room.sabine_t60_s()));
    for source_index in 0..sources().len() {
        for mic_index in 0..mics().len() {
            document = document.replace(
                &format!("__SHA_{source_index}_{mic_index}__"),
                &hashes[source_index][mic_index],
            );
        }
    }
    std::fs::write(scene_dir.join("ground-truth.json"), document).unwrap();

    let mut sums = String::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for source_index in 0..sources().len() {
        for mic_index in 0..mics().len() {
            files.push((
                format!(
                    "rir/{}_{}.wav",
                    sources()[source_index].0,
                    mics()[mic_index].0
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

    (root.to_path_buf(), scene_dir)
}

/// 加载夹具（同时验证生成器与加载器自洽）。
pub fn load_fixture(root: &Path) -> SceneFixture {
    let (_, scene_dir) = build_fixture(root);
    SceneFixture::load(&scene_dir).expect("synthetic fixture must load")
}
/// 复制 scene-cli-smoke 为第二个场景（改 scene_id，RIR/WAV 不变），用于多场景产物布局测试。
pub fn clone_scene(root: &Path, new_scene_id: &str) -> PathBuf {
    let (_, source_dir) = build_fixture_if_missing(root);
    let target_name = format!("scene-{new_scene_id}");
    let target_dir = root.join(&target_name);
    let _ = std::fs::remove_dir_all(&target_dir);
    std::fs::create_dir_all(target_dir.join("rir")).unwrap();
    for entry in std::fs::read_dir(&source_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if path.file_name().unwrap() == "ground-truth.json" {
            let text = std::fs::read_to_string(&path).unwrap();
            let rewritten = text.replace(
                "\"scene_id\": \"scene-cli-smoke\"",
                &format!("\"scene_id\": \"{new_scene_id}\""),
            );
            assert_ne!(rewritten, text, "ground-truth.json 里应有 scene_id");
            std::fs::write(target_dir.join("ground-truth.json"), rewritten).unwrap();
        } else {
            std::fs::copy(&path, target_dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    for entry in std::fs::read_dir(source_dir.join("rir")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), target_dir.join("rir").join(entry.file_name())).unwrap();
    }
    target_dir
}

/// 如果 scene-cli-smoke 已存在则直接返回，否则生成（避免重复生成时重复写盘）。
fn build_fixture_if_missing(root: &Path) -> (PathBuf, PathBuf) {
    let existing = root.join("scene-cli-smoke");
    if existing.join("ground-truth.json").is_file() {
        return (root.to_path_buf(), existing);
    }
    build_fixture(root)
}
