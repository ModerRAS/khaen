//! rubato `Async` 重采样器封装：固定输出块、planar 预分配、process 路径零分配。
//!
//! - 固定输出（[rubato::FixedAsync::Output]）：每次 `process` 输出恒为 `block_frames` 帧，
//!   消耗的输入帧数由当前比率决定（`input_frames_next()`）；
//! - 输入/输出缓冲都在构造期按 `input_frames_max()` / `block_frames` 分配为 planar
//!   (Vec<Vec<f32>>)，运行期只做拷贝/索引，不分配；
//! - 缓冲区通过本模块内的 Adapter/AdapterMut 实现交给 rubato（unsafe trait 只在实现处，
//!   推导依赖构造期不变式：每声道长度 ≥ 报告给 rubato 的帧数）。

use audioadapter::{Adapter, AdapterMut};
use rubato::{
    Adjustable, Async, FixedAsync, PolynomialDegree, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

use crate::config::{AsrcConfig, Interpolation};
use crate::error::{map_construction_error, map_resample_error, AsrcError};

/// 预分配 planar 缓冲（每个声道一段连续 f32）。
#[derive(Clone, Debug, PartialEq)]
pub struct PlanarBuffers {
    frames: usize,
    data: Vec<Vec<f32>>,
}

impl PlanarBuffers {
    /// 分配 channels × frames 的零初始化 planar 缓冲（构造期一次完成）。
    pub fn new(channels: usize, frames: usize) -> Self {
        Self {
            frames,
            data: vec![vec![0.0f32; frames]; channels],
        }
    }

    /// 声道数。
    pub fn channels(&self) -> usize {
        self.data.len()
    }

    /// 每声道帧数（容量）。
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// 只读声道切片（长度 = [frames](Self::frames)）。
    pub fn channel(&self, channel: usize) -> &[f32] {
        &self.data[channel]
    }

    /// 可写声道切片（长度 = [frames](Self::frames)）。
    pub fn channel_mut(&mut self, channel: usize) -> &mut [f32] {
        &mut self.data[channel]
    }

    /// 把某声道 [from, to) 清零（缺帧补 0 用）。
    pub fn clear_range(&mut self, channel: usize, from: usize, to: usize) {
        let to = to.min(self.frames);
        if from < to {
            self.data[channel][from..to].fill(0.0);
        }
    }

    /// 全部清零。
    pub fn clear(&mut self) {
        for channel in &mut self.data {
            channel.fill(0.0);
        }
    }
}

/// 只读 planar 视图 → rubato `Adapter<f32>`。
struct PlanarRef<'a> {
    data: &'a [Vec<f32>],
    frames: usize,
}

// SAFETY: 本类型只读地借用一个 planar 缓冲，且构造前由 [PlanarRef::new] 校验
// 每个声道长度 ≥ 向 rubato 报告的 frames；channels()/frames() 在借用期间不变化，
// read_sample_unchecked 使用带边界检查的安全索引（不产生 UB，违反不变式只会 panic）。
unsafe impl<'a> Adapter<f32> for PlanarRef<'a> {
    unsafe fn read_sample_unchecked(&self, channel: usize, frame: usize) -> f32 {
        self.data[channel][frame]
    }

    fn channels(&self) -> usize {
        self.data.len()
    }

    fn frames(&self) -> usize {
        self.frames
    }
}

impl<'a> PlanarRef<'a> {
    fn new(data: &'a [Vec<f32>], frames: usize) -> Option<Self> {
        if data.iter().all(|channel| channel.len() >= frames) {
            Some(Self { data, frames })
        } else {
            None
        }
    }
}

/// 可写 planar 视图 → rubato `Adapter<f32>` + `AdapterMut<f32>`。
struct PlanarMut<'a> {
    data: &'a mut [Vec<f32>],
    frames: usize,
}

// SAFETY: 同 [PlanarRef]：独占借用、构造时校验每声道长度 ≥ 报告的 frames，
// channels()/frames() 在借用期间不变；读写都用安全索引。
unsafe impl<'a> Adapter<f32> for PlanarMut<'a> {
    unsafe fn read_sample_unchecked(&self, channel: usize, frame: usize) -> f32 {
        self.data[channel][frame]
    }

    fn channels(&self) -> usize {
        self.data.len()
    }

    fn frames(&self) -> usize {
        self.frames
    }
}

// SAFETY: 与上面的 Adapter 实现同一不变式；写入同样用安全索引。
unsafe impl<'a> AdapterMut<f32> for PlanarMut<'a> {
    unsafe fn write_sample_unchecked(&mut self, channel: usize, frame: usize, value: &f32) -> bool {
        self.data[channel][frame] = *value;
        false
    }
}

impl<'a> PlanarMut<'a> {
    fn new(data: &'a mut [Vec<f32>], frames: usize) -> Option<Self> {
        if data.iter().all(|channel| channel.len() >= frames) {
            Some(Self { data, frames })
        } else {
            None
        }
    }
}

/// 一次 `process` 的实际消耗/产出帧数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// 从输入缓冲消耗的帧数（= 调用前 `input_frames_next()`）。
    pub input_frames: usize,
    /// 写入输出缓冲的帧数（固定 = `block_frames`）。
    pub output_frames: usize,
}

/// rubato `Async` 的封装：固定输出块、planar 预分配、比率可逐块调整。
pub struct AsyncResampler {
    config: AsrcConfig,
    inner: Async<f32>,
    input: PlanarBuffers,
    output: PlanarBuffers,
    ratio_ppm: f64,
    last_error: Option<&'static str>,
}

impl std::fmt::Debug for AsyncResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncResampler")
            .field("interp", &self.config.interp)
            .field("channels", &self.config.channels)
            .field("block_frames", &self.config.block_frames)
            .field("ratio_ppm", &self.ratio_ppm)
            .field("last_error", &self.last_error)
            .finish_non_exhaustive()
    }
}

impl AsyncResampler {
    /// 按配置构造（rubato 内部分配 sinc 表/内部缓冲；之后 process 不再分配）。
    pub fn new(config: &AsrcConfig) -> Result<Self, AsrcError> {
        config.validate()?;
        // 构造时给 rubato 的比率范围比控制器限幅留 1 ppm 余量：控制器被 clamp 在
        // ±max_ppm，浮点换算后不会顶到 rubato 的端点而报 RatioOutOfBounds。
        let max_relative = 1.0 + (config.max_ppm + 1.0) * 1e-6;
        let inner = match config.interp {
            Interpolation::Poly => Async::<f32>::new_poly(
                1.0,
                max_relative,
                PolynomialDegree::Cubic,
                config.block_frames,
                config.channels,
                FixedAsync::Output,
            ),
            Interpolation::Sinc => Async::<f32>::new_sinc(
                1.0,
                max_relative,
                &sinc_parameters(config),
                config.block_frames,
                config.channels,
                FixedAsync::Output,
            ),
        }
        .map_err(map_construction_error)?;
        let input_frames = inner
            .input_frames_max()
            .max(inner.input_frames_next())
            .saturating_add(8);
        let input = PlanarBuffers::new(config.channels, input_frames);
        let output = PlanarBuffers::new(config.channels, config.block_frames);
        Ok(Self {
            config: *config,
            inner,
            input,
            output,
            ratio_ppm: 0.0,
            last_error: None,
        })
    }

    /// 配置视图。
    pub fn config(&self) -> &AsrcConfig {
        &self.config
    }

    /// 下一次 `process` 需要的输入帧数（随当前比率变化）。
    pub fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    /// 输入缓冲可容纳的最大帧数（构造期分配）。
    pub fn input_frames_max(&self) -> usize {
        self.input.frames()
    }

    /// 每次 `process` 的固定输出帧数（= block_frames）。
    pub fn output_frames(&self) -> usize {
        self.config.block_frames
    }

    /// 重采样器群延迟（输出帧；sinc 明显大于 poly）。
    pub fn output_delay(&self) -> usize {
        self.inner.output_delay()
    }

    /// 当前施加的比率修正（ppm）。
    pub fn ratio_ppm(&self) -> f64 {
        self.ratio_ppm
    }

    /// 最近一次失败的错误名。
    pub fn last_error(&self) -> Option<&'static str> {
        self.last_error
    }

    /// 输入 planar 缓冲（调用方按 `input_frames_next()` 填帧，缺帧必须写 0）。
    pub fn input_mut(&mut self) -> &mut PlanarBuffers {
        &mut self.input
    }

    /// 输出 planar 缓冲（最近一次 `process` 的结果，长度 = block_frames）。
    pub fn output(&self) -> &PlanarBuffers {
        &self.output
    }

    /// 设置比率修正（ppm，相对 1.0；内部按 ramp=true 在下一块内线性过渡）。
    pub fn set_ratio_ppm(&mut self, ppm: f64) -> Result<(), AsrcError> {
        let ppm = ppm.clamp(-self.config.max_ppm, self.config.max_ppm);
        let relative = 1.0 + ppm * 1e-6;
        match self.inner.set_resample_ratio_relative(relative, true) {
            Ok(()) => {
                self.ratio_ppm = ppm;
                Ok(())
            }
            Err(error) => {
                let error = map_resample_error(error);
                self.last_error = Some(error.name());
                Err(error)
            }
        }
    }

    /// 处理一块：输入缓冲须已填好 `input_frames_next()` 帧；输出 `block_frames` 帧。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不 panic（尺寸不满足时返回 Err）。
    pub fn process(&mut self) -> Result<ProcessOutcome, AsrcError> {
        let needed_input = self.inner.input_frames_next();
        if needed_input > self.input.frames() {
            self.last_error = Some("ERR_ASRC_PROCESS");
            return Err(AsrcError::Process("input buffer smaller than next chunk"));
        }
        let needed_output = self.inner.output_frames_next();
        if needed_output > self.output.frames() {
            self.last_error = Some("ERR_ASRC_PROCESS");
            return Err(AsrcError::Process("output buffer smaller than next chunk"));
        }
        let Some(input) = PlanarRef::new(&self.input.data, needed_input) else {
            self.last_error = Some("ERR_ASRC_PROCESS");
            return Err(AsrcError::Process("input planar buffer too short"));
        };
        let Some(mut output) = PlanarMut::new(&mut self.output.data, needed_output) else {
            self.last_error = Some("ERR_ASRC_PROCESS");
            return Err(AsrcError::Process("output planar buffer too short"));
        };
        match self.inner.process_into_buffer(&input, &mut output, None) {
            Ok((input_frames, output_frames)) => Ok(ProcessOutcome {
                input_frames,
                output_frames,
            }),
            Err(error) => {
                let error = map_resample_error(error);
                self.last_error = Some(error.name());
                Err(error)
            }
        }
    }

    /// 复位 rubato 内部状态（换流/失效时调用）：内部比率回到 1.0，缓冲清零。
    pub fn reset(&mut self) {
        self.inner.reset();
        self.input.clear();
        self.output.clear();
        self.ratio_ppm = 0.0;
        self.last_error = None;
    }
}

/// 由配置构造 sinc 参数（BlackmanHarris2 窗 + 自动 cutoff + cubic 插值）。
fn sinc_parameters(config: &AsrcConfig) -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: config.sinc_len,
        f_cutoff: None,
        oversampling_factor: config.sinc_oversampling,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::BlackmanHarris2,
    }
}
