# khaen-room-sim 公开 API（契约 calib-chain-20261121/mimo-opt-20261122 §7）

> **SIMULATED / SYNTHETIC ONLY（UNPROVEN）**：本 crate 是 RoomForge 鞋盒镜像源法（ISM）的 Rust 移植，
> 零外部依赖（纯 `std`），只服务离线确定性仿真夹具：加载/校验、跨语言重渲染对拍、频带指标。
> 它不读音频设备、不参与实时路径；其产物一律标注 `SIMULATED`，不得当作实测。

权威参考：`C:/WorkSpace/CSharp/RoomForge/src/AudioOptimizer.Simulation/{RoomModel,ImageSourceMethod,ShoeboxRir,VirtualRoom}.cs`
与 `AudioOptimizer.Optimization/SpatialMetrics.cs`。本 crate 与 C# 实现同序同式（坐标/墙命中/反射系数幂次/
1r 标定/线性插值放置），差异仅限文档化的「固定长度渲染」与「f32 WAV 量化」。

## 0. 约定

- 坐标轴：`x = Length ∈ [0, L]`、`y = Width ∈ [0, W]`、`z = Height ∈ [0, H]`，单位 m；声速 343 m/s。
- 时间单位 s（延迟）；幅度单位是「源 1 m 处幅度为 1」的声压比（ISM 标定）。
- 夹具 WAV：f32 单声道 48 kHz，`data` 恰为 `rir.length_samples`（24000）个样本；
  **WAV 样本 = `rir.scale`（0.5）× `sources[].polarity` × ISM 声压标定值**。
  - `h_matrix()`：`p(t) = wav(t) / scale` 还原声压（**不**再乘极性；极性已烘进 WAV）。
  - `rerender_check()`：Rust 重渲染声压 `p(t)` 乘 `scale × polarity`（× `gain_db`，夹具恒 0）
    后与 WAV 样本比较；因此 challenge 的 SUB2（`polarity = -1`）能对齐。
  - 缺 `rir.scale` → 按 1.0 处理并记录到 `SceneFixture::warnings`（契约 §4）。
- JSON 数字按 `f64` 解析（生成器写最短往返格式，整数型 double 可能写成 `20` 而非 `20.0`）；
  对象成员顺序保留（`Vec<(String, Json)>`）。
- H(f)：对每个 (source n, mic m)，WAV→`/scale`→零填充到 N=`rir.fft_size`(65536)→`rfft`，
  取 `frequency_grid_hz` 明确频点 `k = round_half_up(f·N/fs)` 得 `H[m][n][k]`（f64 复数）。
  `HMatrix::h` 布局 `m·N·K + n·K + k`（N=n_sources，K=n_bins）。
- 指标输入 `P: &[Vec<Complex>]` 是 `[mic][bin]`（即 `P_m(f)`），`P_m(f) = Σ_n H[m][n][f]·G_n(f)`，基准 G=1。

## 1. `complex`

```rust
pub struct Complex { pub re: f64, pub im: f64 }
impl Complex {
    pub const ZERO: Complex;
    pub const ONE: Complex;
    pub const fn new(re: f64, im: f64) -> Complex;
    pub fn from_polar(magnitude: f64, phase_rad: f64) -> Complex;
    pub fn abs(self) -> f64;          // hypot(re, im)
    pub fn norm_sqr(self) -> f64;     // |z|² = re² + im²
    pub fn arg(self) -> f64;          // atan2(im, re)
    pub fn conj(self) -> Complex;
    pub fn scale(self, factor: f64) -> Complex;
    pub fn is_finite(self) -> bool;
}
// std::ops: Add, Sub, Neg, Mul, Div, AddAssign, SubAssign, MulAssign, DivAssign,
//           Mul<f64>/Div<f64> for Complex、Mul<Complex> for f64、From<f64>、Sum；Display
```

## 2. `room`

```rust
pub struct Position { pub x: f64, pub y: f64, pub z: f64 }
impl Position {
    pub const fn new(x: f64, y: f64, z: f64) -> Position;
    pub fn distance_to(self, other: Position) -> f64;
    pub fn to_array(self) -> [f64; 3];
}

pub struct SurfaceReflections { pub left: f64, pub right: f64, pub front: f64,
                                pub rear: f64, pub floor: f64, pub ceiling: f64 }
impl SurfaceReflections {
    pub const DEFAULT: SurfaceReflections;   // RoomForge 默认：墙 0.7×4、地/顶 0.5×2
    pub const fn new(left, right, front, rear, floor, ceiling) -> SurfaceReflections;
    pub fn validate(&self) -> Result<(), String>;   // 每个系数 ∈ [0,1]
}
impl Default for SurfaceReflections

pub struct RoomModel { pub length_m: f64, pub width_m: f64, pub height_m: f64,
                       pub speed_of_sound: f64, pub reflections: SurfaceReflections }
impl RoomModel {
    pub const fn new(length_m, width_m, height_m, speed_of_sound, reflections) -> RoomModel;
    pub fn validate(&self) -> Result<(), String>;
    pub fn volume_m3(&self) -> f64;
    pub fn surface_areas(&self) -> [f64; 6]; // 左/右=W·H、前/后=L·H、地/顶=L·W
    pub fn sabine_t60_s(&self) -> f64;       // 0.161·V / Σ(α_i·A_i)，α_i = 1 − r_i²
}
impl Default for RoomModel // RoomForge 默认 3.3 × 3.6 × 2.6 m @343 m/s
```

## 3. `ism`（与 C# `ImageSourceMethod` 同序同式）

```rust
pub struct Arrival { pub delay_seconds: f64, pub amplitude: f64 }
impl Arrival { pub fn new(delay_seconds: f64, amplitude: f64) -> Arrival; }

pub enum ArrivalPlacement { NearestSample, LinearInterpolated }   // Copy/Eq
impl ArrivalPlacement {
    pub fn from_ground_truth_str(s: &str) -> Option<ArrivalPlacement>; // "nearest_sample"/"linear_interpolated"
    pub fn as_str(self) -> &'static str;
}

pub const MIN_PATH_DISTANCE_M: f64 = 1e-12;

/// C# Coordinate：index 偶 → index·size + coordinate；奇 → (index+1)·size − coordinate。
pub fn coordinate(index: i64, size_m: f64, coordinate_m: f64) -> f64;
/// C# WallHits：(low, high)。index ≥ 0 先撞 size 墙；index < 0 先撞 0 墙。
pub fn wall_hits(index: i64) -> (u32, u32);

/// order 阶（每轴 ±order）镜像源；固定 (i,j,k) 顺序（i 外层，k 内层，与 C# 一致），
/// 距离 < 1e-12 m 的巧合路径跳过；amplitude = 反射幂次积 × refDist / distance；
/// delay = distance / speed_of_sound。参数非法时 panic（对应 C# 的 Throw）。
pub fn arrivals(room: &RoomModel, source: Position, microphone: Position,
                order: u32, reference_distance_m: f64) -> Vec<Arrival>;
```

## 4. `rir`（与 C# `ShoeboxRir` 相同的放置；固定长度写入）

```rust
pub use crate::ism::{Arrival, ArrivalPlacement};
pub const PLACEMENT_SLACK_SAMPLES: usize = 2;

/// 固定长度渲染：linear placement 的 lower = floor(pos)，lower 与 lower+1 按 fraction 分摊；
/// NearestSample 用 round-half-away-from-zero；越界丢弃。返回恰好 length_samples 长 f64。
pub fn render(arrivals: &[Arrival], sample_rate_hz: f64,
              placement: ArrivalPlacement, length_samples: usize) -> Vec<f64>;

/// C# ShoeboxRir.Render 的变长副本：长度 = ceil(maxDelay·fs) + 1 + PLACEMENT_SLACK_SAMPLES（空输入 → 空）。
pub fn render_auto(arrivals: &[Arrival], sample_rate_hz: f64,
                   placement: ArrivalPlacement) -> Vec<f64>;
pub fn rendered_length(arrivals: &[Arrival], sample_rate_hz: f64) -> usize;
```

## 5. `wav`（f32 mono 读写；44 字节标准头）

```rust
pub struct WavData { pub sample_rate_hz: u32, pub channels: u16, pub samples: Vec<f32> }

/// 读 RIFF/WAVE：容错未知 chunk（LIST/JUNK/fact…、奇数字节 word 对齐、WAVE_FORMAT_EXTENSIBLE 的
/// 额外 fmt 字节），要求 IEEE float 32-bit、单声道；失败返回 Err(String)（含截断/缺 chunk/非 f32）。
/// 多个 data chunk 时最后一个生效（与 RoomForge 读端一致）。
pub fn read_f32_mono(path: &Path) -> Result<WavData, String>;
pub fn decode_f32_mono(bytes: &[u8]) -> Result<WavData, String>;

/// 写 44 字节标准头（format=3 IEEE float，mono，bits=32）；样本 clamp 到 [-1,1]
/// （与 RoomForge WavFile.Write 的 float32 路径一致）。
pub fn write_f32_mono(path: &Path, samples: &[f32], sample_rate_hz: u32) -> Result<(), String>;
pub fn encode_f32_mono(samples: &[f32], sample_rate_hz: u32) -> Vec<u8>;
```

## 6. `sha256`

```rust
pub fn digest(data: &[u8]) -> [u8; 32];
pub fn to_hex(digest: &[u8; 32]) -> String;      // 64 位小写 hex
pub fn hex_digest(data: &[u8]) -> String;
pub fn file_digest(path: &Path) -> Result<[u8; 32], String>;
pub fn file_hex(path: &Path) -> Result<String, String>;
```

## 7. `json`（最小解析器：对象顺序保留、数字 f64、转义（含 \uXXXX 与代理对）、嵌套）

```rust
pub enum Json { Null, Bool(bool), Number(f64), String(String),
                Array(Vec<Json>), Object(Vec<(String, Json)>) }
impl Json {
    pub fn parse(text: &str) -> Result<Json, String>;        // 允许 UTF-8 BOM / 首尾空白
    pub fn parse_file(path: &Path) -> Result<Json, String>;
    pub fn get(&self, key: &str) -> Option<&Json>;
    pub fn at(&self, keys: &[&str]) -> Option<&Json>;        // 逐级 get
    pub fn as_object(&self) -> Option<&[(String, Json)]>;
    pub fn as_array(&self) -> Option<&[Json]>;
    pub fn as_str(&self) -> Option<&str>;
    pub fn as_f64(&self) -> Option<f64>;
    pub fn as_u64(&self) -> Option<u64>;                     // 整值 f64 → u64
    pub fn as_bool(&self) -> Option<bool>;
    pub fn type_name(&self) -> &'static str;                 // 错误信息用
}
```

## 8. `fft`

```rust
/// 就地 radix-2 复数 FFT（长度必须是 2 的幂；n <= 1 恒等）。
pub fn fft_in_place(data: &mut [Complex]);
pub fn fft(data: &[Complex]) -> Vec<Complex>;
/// 零填充/截断到 n（2 的幂）后 rfft，返回 n/2+1 个 bins（bin k 对应频率 k·fs/n）。
pub fn rfft(samples: &[f64], n: usize) -> Vec<Complex>;
```

## 9. `matrix`

```rust
pub struct HMatrix { pub freqs_hz: Vec<f64>, pub n_sources: usize,
                     pub n_mics: usize, pub h: Vec<Complex> }   // 索引 m·N·K + n·K + k
impl HMatrix {
    pub fn new(freqs_hz: Vec<f64>, n_sources: usize, n_mics: usize) -> HMatrix;
    pub fn n_bins(&self) -> usize;
    pub fn index(&self, mic: usize, source: usize, bin: usize) -> usize;
    pub fn at(&self, mic: usize, source: usize, bin: usize) -> Complex;
    pub fn set(&mut self, mic: usize, source: usize, bin: usize, value: Complex);
    pub fn pressure(&self, gains: &[Vec<Complex>]) -> Vec<Vec<Complex>>;   // [mic][bin]；gains[n][k]
    pub fn pressure_at(&self, gains: &[Vec<Complex>], bin: usize) -> Vec<Complex>; // [mic]
    pub fn unity_gains(&self) -> Vec<Vec<Complex>>;                        // G=1：[n][k]
}
```

## 10. `fixture`（ground-truth 加载/校验/H 矩阵/重渲染比对）

```rust
pub const FIXTURE_FORMAT_V1: &str = "tspi-roomforge-mimo-fixture/v1";
pub const RERENDER_TOLERANCE: f64 = 1e-5;

pub struct RirSpec { pub format: String, pub channels: u16, pub length_samples: usize,
                     pub fft_size: usize, pub scale: f64 }
pub struct SweepSpec { pub start_hz: f64, pub end_hz: f64, pub seconds: f64 }
pub struct ImageSourceSpec { pub order: u32, pub placement: ArrivalPlacement,
                             pub reference_distance_m: f64 }
pub struct SourceSpec { pub id: String, pub index: usize, pub position: Position,
                        pub polarity: f64, pub delay_ms: f64, pub gain_db: f64 }
pub enum MicRole { Design, Holdout }        // as_str()
pub struct MicSpec { pub id: String, pub role: MicRole, pub position: Position }
pub struct RirFileSpec { pub source: String, pub mic: String, pub path: String,
                         pub sha256: String, pub samples: usize }

pub struct SceneFixture {
    pub scene_dir: PathBuf,
    pub format: String, pub generator: String, pub generator_version: String,
    pub scene_id: String, pub seed: u64, pub sample_rate_hz: u32,
    pub rir: RirSpec, pub sweep: SweepSpec,
    pub room: RoomModel, pub sabine_t60_s: f64,
    pub image_source: ImageSourceSpec,
    pub sources: Vec<SourceSpec>, pub mics: Vec<MicSpec>,
    pub design_mic_ids: Vec<String>, pub holdout_mic_ids: Vec<String>,
    pub frequency_grid_hz: Vec<f64>,
    pub bands: BandPlan,
    pub rir_files: Vec<RirFileSpec>,
    pub rirs: Vec<Vec<f32>>,       // 与 rir_files 同序（已校验 SHA256）；索引 n·M + m
    pub warnings: Vec<String>,     // 如缺 rir.scale 按 1.0
}
impl SceneFixture {
    /// 读 <scene_dir>/ground-truth.json；逐 WAV 先校验 SHA256（不一致 → Err，不静默继续），再解析。
    /// 校验：格式版本、rir 尺寸、房间参数、placement、source index 顺序、bands 集合关系、
    /// rir_files 按 (source index, mic index) 升序且覆盖全部 pair、WAV 采样率/长度。
    pub fn load(scene_dir: &Path) -> Result<SceneFixture, String>;
    /// 零填充 fft_size 点 rfft，取 frequency_grid_hz 频点 → HMatrix（H = wav/scale 的声压传递函数）。
    pub fn h_matrix(&self) -> Result<HMatrix, String>;
    /// 同几何重跑 ISM+固定长度 RIR，乘 scale × polarity（× gain_db），与 WAV 逐样本比较。
    pub fn rerender_check(&self) -> Result<RerenderReport, String>;
    pub fn source_index(&self, id: &str) -> Option<usize>;
    pub fn mic_index(&self, id: &str) -> Option<usize>;
    pub fn design_mics(&self) -> Vec<usize>;    // 按 design_mic_ids 顺序
    pub fn holdout_mics(&self) -> Vec<usize>;   // 按 holdout_mic_ids 顺序
    pub fn rir(&self, source_index: usize, mic_index: usize) -> Option<&[f32]>;
}

pub struct RerenderPair { pub source_id: String, pub mic_id: String,
                          pub max_abs_delta: f64, pub max_abs_expected: f64,
                          pub max_relative_delta: f64, pub rms_delta: f64,
                          pub relative_rms: f64,
                          pub f32_round_max_abs_delta: f64 }
pub struct RerenderReport {
    pub pairs: Vec<RerenderPair>,
    pub max_abs_delta: f64,          // 全部 (源,测点) 的最大 |Δ|（WAV 单位；含 f32 量化）
    pub max_relative_delta: f64,     // max|Δ| / max|expected|
    pub max_relative_rms: f64,       // rms(Δ)/rms(expected)
    pub tolerance: f64,              // 1e-5
    pub passed: bool,                // max_abs_delta ≤ 1e-5 && max_relative_delta ≤ 1e-5
    pub f32_round_max_abs_delta: f64,// 把 expected 按 f32 舍入后与 WAV 的最大 |Δ|（跨语言逐位证据）
    pub f32_bit_exact: bool,         // f32_round_max_abs_delta == 0.0
    pub warnings: Vec<String>,       // 如 source 带非零 delay_ms（不烘进 WAV）
}
```

## 11. `metrics`（严格按契约 §5 / RoomForge SpatialMetrics）

```rust
pub const ENERGY_MIGRATION_RATIO: f64 = 1.05;       // R_k > 1.05 ⇒ ENERGY_MIGRATION
pub const COVERAGE_TOLERANCE_DB: f64 = 3.0;
pub const HOLDOUT_SEVERE_REGRESSION_DB: f64 = 3.0;  // 任一 holdout 点任一带 Δ > +3 dB
pub const HOLDOUT_CONSISTENCY_THRESHOLD: f64 = 0.50; // C ≥ 0.50 仅「初步空间泛化」
pub const ALPHA_CLAMP_MIN: f64 = 0.5;
pub const ALPHA_CLAMP_MAX: f64 = 2.0;
pub const NA_DENOMINATOR_FLOOR: f64 = 1e-12;

pub struct Band { pub center_hz: f64, pub low_hz: f64, pub high_hz: f64 }
impl Band {
    pub fn one_third_octave(center_hz: f64) -> Band;   // 边界 [c·2^(−1/6), c·2^(+1/6))
    pub fn contains(&self, frequency_hz: f64) -> bool; // low ≤ f < high
}

pub struct BandPlan { pub spec_centers_hz: Vec<f64>,
                      pub evaluated_spec_centers_hz: Vec<f64>,
                      pub na_spec_centers_hz: Vec<f64>,
                      pub extended_centers_hz: Vec<f64>,
                      pub band_edges_sixth_oct: bool }
impl BandPlan {
    pub fn new(spec_centers_hz, evaluated_spec_centers_hz, na_spec_centers_hz,
               extended_centers_hz, band_edges_sixth_oct: bool) -> BandPlan;
    pub fn spec_bands(&self) -> Vec<Band>;       // 8 带（含 N/A 的 160/200）
    pub fn evaluated_bands(&self) -> Vec<Band>;  // 40–125 Hz，6 带
    pub fn extended_bands(&self) -> Vec<Band>;   // 25/31.5 + evaluated
    pub fn n_evaluated(&self) -> usize;
}

/// 频带内 bins（升序）：freqs_hz[k] ∈ [band.low_hz, band.high_hz)。
pub fn bins_in_band(freqs_hz: &[f64], band: &Band) -> Vec<usize>;

/// E_k = Σ_{m∈mics} Σ_{f∈B_k} |P_m(f)|²，P[mic][k]。
pub fn band_energies(p: &[Vec<Complex>], mics: &[usize],
                     freqs_hz: &[f64], bands: &[Band]) -> Vec<f64>;
/// 每个 mic 每带的 E_m,k（返回顺序同 mics 参数、bands 参数）。
pub fn band_energies_per_mic(p: &[Vec<Complex>], mics: &[usize],
                             freqs_hz: &[f64], bands: &[Band]) -> Vec<Vec<f64>>;
/// 单个测点每带的 E_m,k。
pub fn point_band_energies(p: &[Vec<Complex>], mic: usize,
                           freqs_hz: &[f64], bands: &[Band]) -> Vec<f64>;

/// α：x_k = E_k^0/mean(E^0)；a_k = clamp(x_k, 0.5, 2.0)；α_k = a_k·K_b/Σa（Σα = K_b）。
pub fn alpha_weights(base_band_energies: &[f64]) -> Vec<f64>;

/// 带残余比 R_k = after_k / before_k；分母 < 1e-12·max(before) → None（N/A）。
pub fn residual_ratios(design_before: &[f64], design_after: &[f64]) -> Vec<Option<f64>>;

pub struct EModalReport {
    pub alpha: Vec<f64>,
    pub r_k: Vec<Option<f64>>,
    pub na_bands: Vec<usize>,
    pub e_modal: f64,          // Σ_{valid} α_k R_k
    pub e_modal_db: f64,       // 10·log10(E_modal)
    pub e_modal_eq: f64,       // Σ_{valid} R_k（等权对照）
    pub r_max: Option<f64>,
    pub argmax_k: Option<usize>,
    pub energy_migration: Vec<usize>,     // R_k > 1.05
}
pub fn emodal(design_before: &[f64], design_after: &[f64]) -> EModalReport;

/// 能量 → dB：10·log10(E)。
pub fn energy_to_db(energy: f64) -> f64;
/// Δ dB：10·log10(after/before)；before==0（after>0）→ +∞；两者皆 0 → NaN；after=0 → −∞。
pub fn delta_db(before_energy: f64, after_energy: f64) -> f64;
/// 每个（点, 带）的 Δ_mk（mics 顺序外、bands 顺序内）。
pub fn point_band_deltas_db(p_before: &[Vec<Complex>], p_after: &[Vec<Complex>],
                            mics: &[usize], freqs_hz: &[f64], bands: &[Band]) -> Vec<Vec<f64>>;

/// 最近秩百分位（无插值）：升序序列的第 ceil(p/100·N) − 1 个（clamp 到 [0,N−1]）。
pub fn percentile_nearest_rank(sorted_ascending: &[f64], percentile: f64) -> f64;
/// 中位数：奇数取中间序统计量，偶数取两中间值算术平均。
pub fn median(values: &[f64]) -> f64;

/// RoomForge 风格空间统计（每带，对给定点集）。
pub struct SpatialStats {
    pub center_hz: f64, pub n_points: usize,
    pub mean_db: f64, pub std_db: f64,        // 总体 σ（÷N）
    pub range_db: f64, pub p10_db: f64, pub p90_db: f64, pub p90_p10_db: f64,
    pub coverage_within_3db: f64,             // |level − mean| ≤ 3 dB 的比例
    pub worst_null_db: f64,                   // min_{f,m}(20log10|P_m(f)| − L̄(f))
    pub max_peak_above_mean_db: f64,          // max_{f,m}(20log10|P_m(f)| − L̄(f))
    pub interference_null_db: f64,            // 10log10(min_m E_m,k / mean_m E_m,k)
}
pub fn spatial_stats(p: &[Vec<Complex>], mics: &[usize],
                     freqs_hz: &[f64], band: &Band) -> SpatialStats;
pub fn spatial_stats_all(p: &[Vec<Complex>], mics: &[usize],
                         freqs_hz: &[f64], bands: &[Band]) -> Vec<SpatialStats>;

/// holdout 评估（holdout 不得参与任何设计选择）。
pub struct HoldoutPointResult {
    pub mic_index: usize,
    pub band_delta_db: Vec<f64>,
    pub weighted_mean_delta_db: f64,   // Σα_kΔ_k/Σα_k（仅非 NaN 的带）
    pub improved: bool,                // weighted_mean ≤ 0 dB
    pub valid: bool,                   // 至少一个带可加权
}
pub struct SevereRegression { pub mic_index: usize, pub band_index: usize, pub delta_db: f64 }
pub struct HoldoutEvaluation {
    pub points: Vec<HoldoutPointResult>,
    pub valid_points: usize,
    pub improved_points: usize,
    pub consistency: f64,              // C = improved/valid（valid=0 → NaN）
    pub consistency_pass: bool,        // C ≥ 0.50
    pub severe_regression: bool,       // 任一 holdout 点任一带 Δ > +3.0 dB
    pub severe_cases: Vec<SevereRegression>,
}
pub fn holdout_evaluate(p_before: &[Vec<Complex>], p_after: &[Vec<Complex>],
                        holdout_mics: &[usize], freqs_hz: &[f64], bands: &[Band],
                        alpha: &[f64]) -> HoldoutEvaluation;
```

## 12. 测试与验证命令

```powershell
cd rt-stack
# 单元测试（54 个）+ 自造 mini fixture 集成测试（3 个）：ISM 手算、WAV roundtrip、SHA256 向量、
# FFT vs DFT、JSON、metrics 手算小矩阵、alpha clamp、mini fixture load/h_matrix/rerender/篡改拒收。
cargo test -p khaen-room-sim --offline

# 生成夹具的 5 场景 load + h_matrix + rerender_check（默认 ignored；需要夹具已生成）。
# 注意：TSPI_ROOM_SIM_VALIDATION 是「输出 JSON 文件路径」，不是目录！
$env:TSPI_ROOM_SIM_FIXTURES   = '...\mimo-opt-20261122\fixtures'
$env:TSPI_ROOM_SIM_VALIDATION = '...\mimo-opt-20261122\validation\room-sim-render-check.json'
cargo test -p khaen-room-sim --test fixtures_validation --offline -- --ignored --nocapture
```

## 13. 跨语言验证结果（当前夹具版本）

- 5 场景全部通过：`max|Δ| ≈ 7.3e-9 – 2.12e-8`（WAV 单位，≤0.7 个 f32 ulp），
  `max relative ≤ 5.78e-8`，门限 1e-5；`f32_round_max_abs_delta = 0.0`（把 Rust 期望值按 f32 舍入后
  与 C# WAV 逐位相等，证明双精度 ISM/放置路径与 C# 一致，残差仅为 f32 量化）。
- K=69（首 19.775390625 Hz、末 146.484375 Hz）；N/M 为 2/9、3/9、4/12、3/10、2/9。
- 每个场景另做 SHA256SUMS 全量交叉校验（WAV + ground-truth.json）。
- 详细数值见 `rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/validation/room-sim-render-check.json`。
