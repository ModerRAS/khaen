//! ground-truth.json 加载（SHA256 校验每个 WAV）+ H(f) 矩阵 + 同几何重渲染对拍。
//!
//! 幅度口径（契约 §4）：WAV 样本 = rir.scale（0.5）× polarity × ISM 声压标定值；
//!   - h_matrix：p(t) = wav(t) / scale，零填充 fft_size 点 rfft，取 frequency_grid_hz 频点；
//!   - rerender_check：Rust ISM 重渲染得声压 p(t)，乘 scale × polarity（× gain_db，夹具恒 0）后与 WAV 逐样本比较。
//! 读 WAV 前必须校验 SHA256，不一致 → Err（不静默继续）；缺 rir.scale → 按 1.0 并记录警告。

use std::path::{Path, PathBuf};

use crate::fft::rfft;
use crate::ism::{arrivals, ArrivalPlacement};
use crate::json::Json;
use crate::matrix::HMatrix;
use crate::metrics::BandPlan;
use crate::rir::render;
use crate::room::{Position, RoomModel, SurfaceReflections};
use crate::sha256::{digest, to_hex};
use crate::wav::decode_f32_mono;

/// 支持的夹具格式版本。
pub const FIXTURE_FORMAT_V1: &str = "tspi-roomforge-mimo-fixture/v1";
/// rerender 对拍阈值（max|Δ| 与相对误差，WAV 单位）。
pub const RERENDER_TOLERANCE: f64 = 1e-5;

/// ground-truth.json 的 rir 对象。
#[derive(Clone, Debug, PartialEq)]
pub struct RirSpec {
    pub format: String,
    pub channels: u16,
    pub length_samples: usize,
    pub fft_size: usize,
    pub scale: f64,
}

/// ground-truth.json 的 sweep 对象。
#[derive(Clone, Debug, PartialEq)]
pub struct SweepSpec {
    pub start_hz: f64,
    pub end_hz: f64,
    pub seconds: f64,
}

/// ground-truth.json 的 image_source 对象。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageSourceSpec {
    pub order: u32,
    pub placement: ArrivalPlacement,
    pub reference_distance_m: f64,
}

/// 一个源（sub）。
#[derive(Clone, Debug, PartialEq)]
pub struct SourceSpec {
    pub id: String,
    pub index: usize,
    pub position: Position,
    pub polarity: f64,
    pub delay_ms: f64,
    pub gain_db: f64,
}

/// 测点的 design/holdout 角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicRole {
    Design,
    Holdout,
}

impl MicRole {
    pub fn as_str(self) -> &'static str {
        match self {
            MicRole::Design => "design",
            MicRole::Holdout => "holdout",
        }
    }
}

/// 一个测点。
#[derive(Clone, Debug, PartialEq)]
pub struct MicSpec {
    pub id: String,
    pub role: MicRole,
    pub position: Position,
}

/// ground-truth.json 的 rir_files 条目（路径相对 scene 目录）。
#[derive(Clone, Debug, PartialEq)]
pub struct RirFileSpec {
    pub source: String,
    pub mic: String,
    pub path: String,
    pub sha256: String,
    pub samples: usize,
}

/// 一个场景的完整夹具（ground-truth + 已校验的 RIR 样本）。
#[derive(Clone, Debug)]
pub struct SceneFixture {
    pub scene_dir: PathBuf,
    pub format: String,
    pub generator: String,
    pub generator_version: String,
    pub scene_id: String,
    pub seed: u64,
    pub sample_rate_hz: u32,
    pub rir: RirSpec,
    pub sweep: SweepSpec,
    pub room: RoomModel,
    pub sabine_t60_s: f64,
    pub image_source: ImageSourceSpec,
    pub sources: Vec<SourceSpec>,
    pub mics: Vec<MicSpec>,
    pub design_mic_ids: Vec<String>,
    pub holdout_mic_ids: Vec<String>,
    pub frequency_grid_hz: Vec<f64>,
    pub bands: BandPlan,
    pub rir_files: Vec<RirFileSpec>,
    /// 与 rir_files 同序的样本（已校验 SHA256）。索引 = source_index·n_mics + mic_index。
    pub rirs: Vec<Vec<f32>>,
    pub warnings: Vec<String>,
}

/// 单个 (源, 测点) 的重渲染比对结果（WAV 单位）。
#[derive(Clone, Debug, PartialEq)]
pub struct RerenderPair {
    pub source_id: String,
    pub mic_id: String,
    pub max_abs_delta: f64,
    pub max_abs_expected: f64,
    pub max_relative_delta: f64,
    pub rms_delta: f64,
    pub relative_rms: f64,
    /// 把 Rust 期望值按 f32 舍入（与 WAV 写入的 (float) cast 同规则）后与 WAV 的最大 |Δ|。
    pub f32_round_max_abs_delta: f64,
}

/// 一个场景的重渲染比对结果。
#[derive(Clone, Debug, PartialEq)]
pub struct RerenderReport {
    pub pairs: Vec<RerenderPair>,
    pub max_abs_delta: f64,
    pub max_relative_delta: f64,
    pub max_relative_rms: f64,
    pub tolerance: f64,
    pub passed: bool,
    /// 全部 pair 的「期望值按 f32 舍入后 vs WAV」最大 |Δ|；0 表示跨语言双精度路径在 f32 分辨率下逐位一致。
    pub f32_round_max_abs_delta: f64,
    pub f32_bit_exact: bool,
    pub warnings: Vec<String>,
}

impl SceneFixture {
    /// 读 <scene_dir>/ground-truth.json，逐 WAV 校验 SHA256 后解析。任何不一致 → Err。
    pub fn load(scene_dir: &Path) -> Result<SceneFixture, String> {
        let mut warnings: Vec<String> = Vec::new();
        let document_path = scene_dir.join("ground-truth.json");
        let document = Json::parse_file(&document_path)?;
        let context = "ground-truth.json";

        let format = str_member(&document, "format", context)?.to_string();
        if format != FIXTURE_FORMAT_V1 {
            return Err(format!(
                "unsupported fixture format '{format}' in {} (expected {FIXTURE_FORMAT_V1})",
                document_path.display()
            ));
        }
        let generator = str_member(&document, "generator", context)?.to_string();
        let generator_version = str_member(&document, "generator_version", context)?.to_string();
        let scene_id = str_member(&document, "scene_id", context)?.to_string();
        let seed = u64_member(&document, "seed", context)?;
        let sample_rate_hz = u32_member(&document, "sample_rate_hz", context)?;
        if sample_rate_hz == 0 {
            return Err(format!("{context}: sample_rate_hz must be > 0"));
        }

        // ---- rir ----
        let rir_json = object_member(&document, "rir", context)?;
        let rir = RirSpec {
            format: str_member(rir_json, "format", "rir")?.to_string(),
            channels: u16_member(rir_json, "channels", "rir")?,
            length_samples: usize_member(rir_json, "length_samples", "rir")?,
            fft_size: usize_member(rir_json, "fft_size", "rir")?,
            scale: match rir_json.get("scale") {
                Some(value) => {
                    let scale = value.as_f64().ok_or_else(|| {
                        format!(
                            "'scale' in rir must be a number, found {}",
                            value.type_name()
                        )
                    })?;
                    if !(scale > 0.0 && scale.is_finite()) {
                        return Err(format!("rir.scale must be > 0 and finite, got {scale}"));
                    }
                    scale
                }
                None => {
                    warnings.push(
                        "rir.scale missing from ground-truth.json; assuming 1.0 (contract §4)"
                            .to_string(),
                    );
                    1.0
                }
            },
        };
        if rir.length_samples == 0 {
            return Err(format!("rir.length_samples must be > 0"));
        }
        if !rir.fft_size.is_power_of_two() || rir.fft_size < 2 {
            return Err(format!(
                "rir.fft_size must be a power of two >= 2, got {}",
                rir.fft_size
            ));
        }
        if rir.channels != 1 {
            return Err(format!(
                "rir.channels must be 1 (f32 mono fixtures), got {}",
                rir.channels
            ));
        }
        if rir.format != "f32" {
            warnings.push(format!(
                "rir.format is '{}' but this crate only reads f32 WAV",
                rir.format
            ));
        }

        // ---- sweep ----
        let sweep_json = object_member(&document, "sweep", context)?;
        let sweep = SweepSpec {
            start_hz: f64_member(sweep_json, "start_hz", "sweep")?,
            end_hz: f64_member(sweep_json, "end_hz", "sweep")?,
            seconds: f64_member(sweep_json, "seconds", "sweep")?,
        };

        // ---- room ----
        let room_json = object_member(&document, "room", context)?;
        let reflections_json = object_member(room_json, "reflections", "room")?;
        let reflections = SurfaceReflections::new(
            f64_member(reflections_json, "left", "room.reflections")?,
            f64_member(reflections_json, "right", "room.reflections")?,
            f64_member(reflections_json, "front", "room.reflections")?,
            f64_member(reflections_json, "rear", "room.reflections")?,
            f64_member(reflections_json, "floor", "room.reflections")?,
            f64_member(reflections_json, "ceiling", "room.reflections")?,
        );
        let room = RoomModel::new(
            f64_member(room_json, "length_m", "room")?,
            f64_member(room_json, "width_m", "room")?,
            f64_member(room_json, "height_m", "room")?,
            f64_member(room_json, "speed_of_sound_m_s", "room")?,
            reflections,
        );
        room.validate()
            .map_err(|error| format!("{context}: invalid room: {error}"))?;
        let sabine_t60_s = f64_member(room_json, "sabine_t60_s", "room")?;
        let computed_t60 = room.sabine_t60_s();
        if (computed_t60 - sabine_t60_s).abs() > 1e-6 * sabine_t60_s.abs().max(1.0) {
            warnings.push(format!(
                "room.sabine_t60_s = {sabine_t60_s} but the ported Sabine formula gives {computed_t60}"
            ));
        }

        // ---- image_source ----
        let image_source_json = object_member(&document, "image_source", context)?;
        let placement_str = str_member(image_source_json, "placement", "image_source")?;
        let placement = ArrivalPlacement::from_ground_truth_str(placement_str).ok_or_else(|| {
            format!("unsupported image_source.placement '{placement_str}' (expected nearest_sample|linear_interpolated)")
        })?;
        let reference_distance_m =
            f64_member(image_source_json, "reference_distance_m", "image_source")?;
        if !(reference_distance_m > 0.0) {
            return Err(format!(
                "image_source.reference_distance_m must be > 0 m, got {reference_distance_m}"
            ));
        }
        let image_source = ImageSourceSpec {
            order: u32_member(image_source_json, "order", "image_source")?,
            placement,
            reference_distance_m,
        };

        // ---- sources ----
        let sources_json = array_member(&document, "sources", context)?;
        if sources_json.is_empty() {
            return Err(format!("{context}: sources must not be empty"));
        }
        let mut sources = Vec::with_capacity(sources_json.len());
        for (position, source_json) in sources_json.iter().enumerate() {
            let source_context = format!("sources[{position}]");
            let index = usize_member(source_json, "index", &source_context)?;
            if index != position {
                return Err(format!(
                    "{source_context}: index {index} does not match its position in the array (expected {position})"
                ));
            }
            let polarity = optional_f64(source_json, "polarity", 1.0)?;
            if polarity != 1.0 && polarity != -1.0 {
                warnings.push(format!(
                    "{}: polarity {polarity} is not +1/-1",
                    source_context
                ));
            }
            let source = SourceSpec {
                id: str_member(source_json, "id", &source_context)?.to_string(),
                index,
                position: position_member(source_json, "position_m", &source_context)?,
                polarity,
                delay_ms: optional_f64(source_json, "delay_ms", 0.0)?,
                gain_db: optional_f64(source_json, "gain_db", 0.0)?,
            };
            if sources
                .iter()
                .any(|existing: &SourceSpec| existing.id == source.id)
            {
                return Err(format!("{context}: duplicate source id '{}'", source.id));
            }
            sources.push(source);
        }

        // ---- mics ----
        let mics_json = array_member(&document, "mics", context)?;
        if mics_json.is_empty() {
            return Err(format!("{context}: mics must not be empty"));
        }
        let mut mics = Vec::with_capacity(mics_json.len());
        for (position, mic_json) in mics_json.iter().enumerate() {
            let mic_context = format!("mics[{position}]");
            let role_str = str_member(mic_json, "role", &mic_context)?;
            let role = match role_str {
                "design" => MicRole::Design,
                "holdout" => MicRole::Holdout,
                other => {
                    return Err(format!(
                        "{mic_context}: role must be 'design' or 'holdout', got '{other}'"
                    ))
                }
            };
            let mic = MicSpec {
                id: str_member(mic_json, "id", &mic_context)?.to_string(),
                role,
                position: position_member(mic_json, "position_m", &mic_context)?,
            };
            if mics.iter().any(|existing: &MicSpec| existing.id == mic.id) {
                return Err(format!("{context}: duplicate mic id '{}'", mic.id));
            }
            mics.push(mic);
        }

        // ---- design / holdout id 列表 ----
        let design_mic_ids = string_array_member(&document, "design_mic_ids", context)?;
        let holdout_mic_ids = string_array_member(&document, "holdout_mic_ids", context)?;
        for id in design_mic_ids.iter().chain(holdout_mic_ids.iter()) {
            if !mics.iter().any(|mic| mic.id == *id) {
                return Err(format!(
                    "{context}: mic id '{id}' in design/holdout list is not in mics"
                ));
            }
        }
        for mic in &mics {
            let in_design = design_mic_ids.iter().any(|id| *id == mic.id);
            let in_holdout = holdout_mic_ids.iter().any(|id| *id == mic.id);
            let role_matches = match mic.role {
                MicRole::Design => in_design && !in_holdout,
                MicRole::Holdout => in_holdout && !in_design,
            };
            if !role_matches {
                warnings.push(format!(
                    "mic '{}' role {} disagrees with design/holdout id lists",
                    mic.id,
                    mic.role.as_str()
                ));
            }
        }

        // ---- frequency grid ----
        let frequency_grid_hz = f64_array_member(&document, "frequency_grid_hz", context)?;
        if frequency_grid_hz.is_empty() {
            return Err(format!("{context}: frequency_grid_hz must not be empty"));
        }
        let nyquist = sample_rate_hz as f64 / 2.0;
        let mut previous = f64::NEG_INFINITY;
        for (bin, frequency) in frequency_grid_hz.iter().enumerate() {
            if !(*frequency > 0.0 && *frequency <= nyquist) {
                return Err(format!(
                    "frequency_grid_hz[{bin}] = {frequency} is outside (0, {nyquist}]"
                ));
            }
            if *frequency <= previous {
                return Err(format!(
                    "frequency_grid_hz must be strictly ascending; index {bin} = {frequency} follows {previous}"
                ));
            }
            previous = *frequency;
            let exact_bin = frequency * rir.fft_size as f64 / sample_rate_hz as f64;
            let rounded = (exact_bin + 0.5).floor();
            if (exact_bin - rounded).abs() > 1e-6 {
                warnings.push(format!(
                    "frequency_grid_hz[{bin}] = {frequency} is not exactly an FFT bin (k = {exact_bin})"
                ));
            }
            if rounded > (rir.fft_size / 2) as f64 {
                return Err(format!(
                    "frequency_grid_hz[{bin}] = {frequency} maps above Nyquist bin"
                ));
            }
        }

        // ---- bands ----
        let bands_json = object_member(&document, "bands", context)?;
        let spec_centers_hz = f64_array_member(bands_json, "spec_centers_hz", "bands")?;
        let evaluated_spec_centers_hz =
            f64_array_member(bands_json, "evaluated_spec_centers_hz", "bands")?;
        let na_spec_centers_hz = f64_array_member(bands_json, "na_spec_centers_hz", "bands")?;
        let extended_centers_hz = f64_array_member(bands_json, "extended_centers_hz", "bands")?;
        let band_edges_sixth_oct = match bands_json.get("band_edges_sixth_oct") {
            Some(value) => value.as_bool().ok_or_else(|| {
                format!(
                    "bands.band_edges_sixth_oct must be bool, found {}",
                    value.type_name()
                )
            })?,
            None => true,
        };
        if !band_edges_sixth_oct {
            warnings.push(
                "bands.band_edges_sixth_oct is false; this crate always uses 1/3-octave c·2^(±1/6) edges"
                    .to_string(),
            );
        }
        if spec_centers_hz.is_empty() || evaluated_spec_centers_hz.is_empty() {
            return Err(format!(
                "{context}: bands.spec/evaluated centre lists must not be empty"
            ));
        }
        for center in &evaluated_spec_centers_hz {
            if !spec_centers_hz
                .iter()
                .any(|spec| (spec - center).abs() < 1e-9)
            {
                return Err(format!(
                    "{context}: evaluated band centre {center} Hz is not in spec_centers_hz"
                ));
            }
        }
        for center in &na_spec_centers_hz {
            if !spec_centers_hz
                .iter()
                .any(|spec| (spec - center).abs() < 1e-9)
            {
                return Err(format!(
                    "{context}: N/A band centre {center} Hz is not in spec_centers_hz"
                ));
            }
        }
        let mut reconstructed = evaluated_spec_centers_hz.clone();
        reconstructed.extend(na_spec_centers_hz.iter().copied());
        if reconstructed.len() != spec_centers_hz.len()
            || spec_centers_hz.iter().any(|center| {
                !reconstructed
                    .iter()
                    .any(|value| (value - center).abs() < 1e-9)
            })
        {
            return Err(format!(
                "{context}: spec_centers_hz must equal evaluated ∪ N/A centre sets"
            ));
        }
        let bands = BandPlan::new(
            spec_centers_hz,
            evaluated_spec_centers_hz,
            na_spec_centers_hz,
            extended_centers_hz,
            band_edges_sixth_oct,
        );

        // ---- rir_files + WAV 校验 ----
        let rir_files_json = array_member(&document, "rir_files", context)?;
        let expected_files = sources.len() * mics.len();
        if rir_files_json.len() != expected_files {
            return Err(format!(
                "{context}: rir_files has {} entries, expected N·M = {}·{} = {expected_files}",
                rir_files_json.len(),
                sources.len(),
                mics.len()
            ));
        }
        let mut rir_files = Vec::with_capacity(rir_files_json.len());
        let mut rirs: Vec<Vec<f32>> = Vec::with_capacity(expected_files);
        let mut seen = vec![false; expected_files];
        for (index, file_json) in rir_files_json.iter().enumerate() {
            let file_context = format!("rir_files[{index}]");
            let source_id = str_member(file_json, "source", &file_context)?.to_string();
            let mic_id = str_member(file_json, "mic", &file_context)?.to_string();
            let path = str_member(file_json, "path", &file_context)?.to_string();
            let sha256 = str_member(file_json, "sha256", &file_context)?.to_string();
            let samples = usize_member(file_json, "samples", &file_context)?;

            let source_index = sources
                .iter()
                .position(|source| source.id == source_id)
                .ok_or_else(|| format!("{file_context}: unknown source '{source_id}'"))?;
            let mic_index = mics
                .iter()
                .position(|mic| mic.id == mic_id)
                .ok_or_else(|| format!("{file_context}: unknown mic '{mic_id}'"))?;
            let expected_index = source_index * mics.len() + mic_index;
            if index != expected_index {
                return Err(format!(
                    "{file_context}: rir_files must be ordered by (source index, mic index); expected ({source_id},{mic_id}) at index {expected_index}, found at {index}"
                ));
            }
            if samples != rir.length_samples {
                return Err(format!(
                    "{file_context}: samples = {samples} but rir.length_samples = {}",
                    rir.length_samples
                ));
            }
            validate_relative_path(&path)?;

            let wav_path = scene_dir.join(&path);
            let bytes = std::fs::read(&wav_path)
                .map_err(|error| format!("cannot read RIR file {}: {error}", wav_path.display()))?;
            let actual_sha = to_hex(&digest(&bytes));
            if !actual_sha.eq_ignore_ascii_case(&sha256) {
                return Err(format!(
                    "SHA256 mismatch for {}: ground-truth says {sha256}, file is {actual_sha}",
                    wav_path.display()
                ));
            }
            let wav = decode_f32_mono(&bytes)
                .map_err(|error| format!("invalid RIR WAV {}: {error}", wav_path.display()))?;
            if wav.sample_rate_hz != sample_rate_hz {
                return Err(format!(
                    "{}: sample rate {} Hz but ground-truth says {sample_rate_hz} Hz",
                    wav_path.display(),
                    wav.sample_rate_hz
                ));
            }
            if wav.samples.len() != rir.length_samples {
                return Err(format!(
                    "{}: {} samples but rir.length_samples = {}",
                    wav_path.display(),
                    wav.samples.len(),
                    rir.length_samples
                ));
            }
            if seen[expected_index] {
                return Err(format!(
                    "{file_context}: duplicate (source, mic) pair ({source_id}, {mic_id})"
                ));
            }
            seen[expected_index] = true;

            let mut max_abs_sample: f32 = 0.0;
            for sample in &wav.samples {
                max_abs_sample = max_abs_sample.max(sample.abs());
            }
            if max_abs_sample >= 0.999_999 {
                warnings.push(format!(
                    "{}: WAV sample reaches {max_abs_sample} (possible clamp saturation)",
                    wav_path.display()
                ));
            }

            rir_files.push(RirFileSpec {
                source: source_id,
                mic: mic_id,
                path,
                sha256: actual_sha,
                samples,
            });
            rirs.push(wav.samples);
        }
        if seen.iter().any(|value| !value) {
            return Err(format!(
                "{context}: rir_files does not cover every (source, mic) pair"
            ));
        }

        if sources
            .iter()
            .any(|source| source.delay_ms != 0.0 || source.gain_db != 0.0)
        {
            warnings.push(
                "sources carry non-zero delay_ms/gain_db; fixture generator bakes only polarity into the WAV"
                    .to_string(),
            );
        }

        Ok(SceneFixture {
            scene_dir: scene_dir.to_path_buf(),
            format,
            generator,
            generator_version,
            scene_id,
            seed,
            sample_rate_hz,
            rir,
            sweep,
            room,
            sabine_t60_s,
            image_source,
            sources,
            mics,
            design_mic_ids,
            holdout_mic_ids,
            frequency_grid_hz,
            bands,
            rir_files,
            rirs,
            warnings,
        })
    }

    /// source id → 数组下标。
    pub fn source_index(&self, id: &str) -> Option<usize> {
        self.sources.iter().position(|source| source.id == id)
    }

    /// mic id → 数组下标。
    pub fn mic_index(&self, id: &str) -> Option<usize> {
        self.mics.iter().position(|mic| mic.id == id)
    }

    /// design 测点的下标（按 design_mic_ids 顺序）。
    pub fn design_mics(&self) -> Vec<usize> {
        self.design_mic_ids
            .iter()
            .filter_map(|id| self.mic_index(id))
            .collect()
    }

    /// holdout 测点的下标（按 holdout_mic_ids 顺序）。
    pub fn holdout_mics(&self) -> Vec<usize> {
        self.holdout_mic_ids
            .iter()
            .filter_map(|id| self.mic_index(id))
            .collect()
    }

    /// (source_index, mic_index) 的 RIR 样本（已校验 SHA256）。
    pub fn rir(&self, source_index: usize, mic_index: usize) -> Option<&[f32]> {
        if source_index >= self.sources.len() || mic_index >= self.mics.len() {
            return None;
        }
        self.rirs
            .get(source_index * self.mics.len() + mic_index)
            .map(|samples| samples.as_slice())
    }

    /// 零填充 fft_size 点 rfft，取 frequency_grid_hz 精确频点得 H[m][n][k]。
    /// H 是 wav/scale 还原后的声压传递函数（不含任何校正滤波）。
    pub fn h_matrix(&self) -> Result<HMatrix, String> {
        let n_sources = self.sources.len();
        let n_mics = self.mics.len();
        let n_bins = self.frequency_grid_hz.len();
        let fft_size = self.rir.fft_size;
        let sample_rate = self.sample_rate_hz as f64;

        let mut bins = Vec::with_capacity(n_bins);
        for (bin, frequency) in self.frequency_grid_hz.iter().enumerate() {
            let exact_bin = frequency * fft_size as f64 / sample_rate;
            let rounded = (exact_bin + 0.5).floor();
            if !(rounded >= 0.0 && rounded <= (fft_size / 2) as f64) {
                return Err(format!(
                    "frequency_grid_hz[{bin}] = {frequency} maps to bin {rounded}, outside [0, {}]",
                    fft_size / 2
                ));
            }
            if (exact_bin - rounded).abs() > 1e-6 {
                return Err(format!(
                    "frequency_grid_hz[{bin}] = {frequency} is not an exact FFT bin (k = {exact_bin})"
                ));
            }
            bins.push(rounded as usize);
        }

        let mut matrix = HMatrix::new(self.frequency_grid_hz.clone(), n_sources, n_mics);
        let scale = self.rir.scale;
        for source_index in 0..n_sources {
            for mic_index in 0..n_mics {
                let samples = self
                    .rirs
                    .get(source_index * n_mics + mic_index)
                    .ok_or_else(|| {
                        format!("missing RIR for source {source_index}, mic {mic_index}")
                    })?;
                let pressure: Vec<f64> = samples
                    .iter()
                    .map(|sample| *sample as f64 / scale)
                    .collect();
                let spectrum = rfft(&pressure, fft_size);
                for (bin_index, &bin) in bins.iter().enumerate() {
                    matrix.set(mic_index, source_index, bin_index, spectrum[bin]);
                }
            }
        }
        Ok(matrix)
    }

    /// 用同一几何重跑 ISM + 固定长度 RIR，乘 scale × polarity（× gain_db，夹具恒 0）后与 WAV 逐样本比较。
    pub fn rerender_check(&self) -> Result<RerenderReport, String> {
        let mut warnings = Vec::new();
        let n_mics = self.mics.len();
        let mut pairs = Vec::with_capacity(self.sources.len() * n_mics);
        let mut max_abs_delta: f64 = 0.0;
        let mut max_relative_delta: f64 = 0.0;
        let mut max_relative_rms: f64 = 0.0;
        let mut f32_round_max_abs_delta: f64 = 0.0;

        for source in &self.sources {
            if source.delay_ms != 0.0 {
                warnings.push(format!(
                    "source {} has delay_ms = {}; rerender applies gain/polarity but not the delay setting",
                    source.id, source.delay_ms
                ));
            }
            let gain = 10f64.powf(source.gain_db / 20.0) * source.polarity;
            for (mic_index, mic) in self.mics.iter().enumerate() {
                let paths = arrivals(
                    &self.room,
                    source.position,
                    mic.position,
                    self.image_source.order,
                    self.image_source.reference_distance_m,
                );
                let pressure = render(
                    &paths,
                    self.sample_rate_hz as f64,
                    self.image_source.placement,
                    self.rir.length_samples,
                );
                let samples = self
                    .rirs
                    .get(source.index * n_mics + mic_index)
                    .ok_or_else(|| {
                        format!("missing RIR for source {}, mic {}", source.id, mic.id)
                    })?;

                let mut pair_max_abs_delta: f64 = 0.0;
                let mut pair_max_abs_expected: f64 = 0.0;
                let mut pair_f32_round_max_abs_delta: f64 = 0.0;
                let mut sum_delta_sq: f64 = 0.0;
                let mut sum_expected_sq: f64 = 0.0;
                for (sample_index, &wav_sample) in samples.iter().enumerate() {
                    let expected = self.rir.scale * gain * pressure[sample_index];
                    let delta = wav_sample as f64 - expected;
                    pair_max_abs_delta = pair_max_abs_delta.max(delta.abs());
                    pair_max_abs_expected = pair_max_abs_expected.max(expected.abs());
                    // generator 写 WAV 时是 (float)expected；按同一规则舍入后应逐位相等（f32 分辨率）。
                    let rounded = expected as f32 as f64;
                    let delta_f32 = wav_sample as f64 - rounded;
                    pair_f32_round_max_abs_delta =
                        pair_f32_round_max_abs_delta.max(delta_f32.abs());
                    sum_delta_sq += delta * delta;
                    sum_expected_sq += expected * expected;
                }
                let pair_relative_delta = if pair_max_abs_expected > 0.0 {
                    pair_max_abs_delta / pair_max_abs_expected
                } else if pair_max_abs_delta > 0.0 {
                    f64::INFINITY
                } else {
                    0.0
                };
                let pair_rms_delta = (sum_delta_sq / samples.len() as f64).sqrt();
                let pair_relative_rms = if sum_expected_sq > 0.0 {
                    (sum_delta_sq / sum_expected_sq).sqrt()
                } else {
                    0.0
                };

                max_abs_delta = max_abs_delta.max(pair_max_abs_delta);
                max_relative_delta = max_relative_delta.max(pair_relative_delta);
                max_relative_rms = max_relative_rms.max(pair_relative_rms);
                f32_round_max_abs_delta = f32_round_max_abs_delta.max(pair_f32_round_max_abs_delta);

                pairs.push(RerenderPair {
                    source_id: source.id.clone(),
                    mic_id: mic.id.clone(),
                    max_abs_delta: pair_max_abs_delta,
                    max_abs_expected: pair_max_abs_expected,
                    max_relative_delta: pair_relative_delta,
                    rms_delta: pair_rms_delta,
                    relative_rms: pair_relative_rms,
                    f32_round_max_abs_delta: pair_f32_round_max_abs_delta,
                });
            }
        }

        Ok(RerenderReport {
            pairs,
            max_abs_delta,
            max_relative_delta,
            max_relative_rms,
            tolerance: RERENDER_TOLERANCE,
            passed: max_abs_delta <= RERENDER_TOLERANCE && max_relative_delta <= RERENDER_TOLERANCE,
            f32_round_max_abs_delta,
            f32_bit_exact: f32_round_max_abs_delta == 0.0,
            warnings,
        })
    }
}

// ---- JSON 读取辅助（错误信息带路径上下文） ----

fn object_member<'a>(parent: &'a Json, key: &str, context: &str) -> Result<&'a Json, String> {
    let value = member(parent, key, context)?;
    if value.as_object().is_none() {
        return Err(format!(
            "'{key}' in {context} must be an object, found {}",
            value.type_name()
        ));
    }
    Ok(value)
}

fn member<'a>(parent: &'a Json, key: &str, context: &str) -> Result<&'a Json, String> {
    parent
        .get(key)
        .ok_or_else(|| format!("missing '{key}' in {context}"))
}

fn str_member<'a>(parent: &'a Json, key: &str, context: &str) -> Result<&'a str, String> {
    let value = member(parent, key, context)?;
    value.as_str().ok_or_else(|| {
        format!(
            "'{key}' in {context} must be a string, found {}",
            value.type_name()
        )
    })
}

fn f64_member(parent: &Json, key: &str, context: &str) -> Result<f64, String> {
    let value = member(parent, key, context)?;
    let number = value.as_f64().ok_or_else(|| {
        format!(
            "'{key}' in {context} must be a number, found {}",
            value.type_name()
        )
    })?;
    if !number.is_finite() {
        return Err(format!("'{key}' in {context} must be finite, got {number}"));
    }
    Ok(number)
}

fn optional_f64(parent: &Json, key: &str, default: f64) -> Result<f64, String> {
    match parent.get(key) {
        None | Some(Json::Null) => Ok(default),
        Some(value) => {
            let number = value
                .as_f64()
                .ok_or_else(|| format!("'{key}' must be a number, found {}", value.type_name()))?;
            if !number.is_finite() {
                return Err(format!("'{key}' must be finite, got {number}"));
            }
            Ok(number)
        }
    }
}

fn u64_member(parent: &Json, key: &str, context: &str) -> Result<u64, String> {
    let value = member(parent, key, context)?;
    value
        .as_u64()
        .ok_or_else(|| format!("'{key}' in {context} must be a non-negative integer"))
}

fn usize_member(parent: &Json, key: &str, context: &str) -> Result<usize, String> {
    let value = u64_member(parent, key, context)?;
    usize::try_from(value).map_err(|_| format!("'{key}' in {context} does not fit in usize"))
}

fn u32_member(parent: &Json, key: &str, context: &str) -> Result<u32, String> {
    let value = u64_member(parent, key, context)?;
    u32::try_from(value).map_err(|_| format!("'{key}' in {context} does not fit in u32"))
}

fn u16_member(parent: &Json, key: &str, context: &str) -> Result<u16, String> {
    let value = u64_member(parent, key, context)?;
    u16::try_from(value).map_err(|_| format!("'{key}' in {context} does not fit in u16"))
}

fn array_member<'a>(parent: &'a Json, key: &str, context: &str) -> Result<&'a [Json], String> {
    let value = member(parent, key, context)?;
    value.as_array().ok_or_else(|| {
        format!(
            "'{key}' in {context} must be an array, found {}",
            value.type_name()
        )
    })
}

fn f64_array_member(parent: &Json, key: &str, context: &str) -> Result<Vec<f64>, String> {
    let values = array_member(parent, key, context)?;
    let mut result = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let number = value.as_f64().ok_or_else(|| {
            format!(
                "'{key}[{index}]' in {context} must be a number, found {}",
                value.type_name()
            )
        })?;
        if !number.is_finite() {
            return Err(format!(
                "'{key}[{index}]' in {context} must be finite, got {number}"
            ));
        }
        result.push(number);
    }
    Ok(result)
}

fn string_array_member(parent: &Json, key: &str, context: &str) -> Result<Vec<String>, String> {
    let values = array_member(parent, key, context)?;
    let mut result = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let text = value.as_str().ok_or_else(|| {
            format!(
                "'{key}[{index}]' in {context} must be a string, found {}",
                value.type_name()
            )
        })?;
        result.push(text.to_string());
    }
    Ok(result)
}

fn position_member(parent: &Json, key: &str, context: &str) -> Result<Position, String> {
    let values = array_member(parent, key, context)?;
    if values.len() != 3 {
        return Err(format!(
            "'{key}' in {context} must have 3 coordinates, found {}",
            values.len()
        ));
    }
    let mut coordinates = [0.0f64; 3];
    for (index, value) in values.iter().enumerate() {
        let number = value.as_f64().ok_or_else(|| {
            format!(
                "'{key}[{index}]' in {context} must be a number, found {}",
                value.type_name()
            )
        })?;
        if !number.is_finite() {
            return Err(format!("'{key}[{index}]' in {context} must be finite"));
        }
        coordinates[index] = number;
    }
    Ok(Position::new(
        coordinates[0],
        coordinates[1],
        coordinates[2],
    ))
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!(
            "fixture path '{path}' must be relative to the scene directory"
        ));
    }
    for component in candidate.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(format!(
                "fixture path '{path}' must be a plain relative path (no '.', '..', root or prefix)"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_guard() {
        assert!(validate_relative_path("rir/SUB1_P01.wav").is_ok());
        assert!(validate_relative_path("ground-truth.json").is_ok());
        assert!(validate_relative_path("../secret.wav").is_err());
        assert!(validate_relative_path("/abs/secret.wav").is_err());
        assert!(validate_relative_path("rir/../secret.wav").is_err());
    }

    #[test]
    fn missing_fixture_directory_is_an_error() {
        assert!(SceneFixture::load(Path::new("definitely-missing-fixture-dir")).is_err());
    }
}
