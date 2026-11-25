//! 音频后端接口与假后端。
//!
//! 后端负责"两块缓冲的搬运"：采集一块输入、送出引擎算完的一块输出。
//! 引擎只认切片，不认设备；因此同一份引擎可在假后端上跑门禁，在 ALSA 后端上出声。

use crate::owner::DeviceId;

/// 打开后端所需的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceConfig {
    /// 设备标识。
    pub device: DeviceId,
    /// 采样率。
    pub sample_rate: u32,
    /// 每块帧数。
    pub block_frames: usize,
    /// 通道数（设备通道，不是 lane 数）。
    pub channels: usize,
}

impl DeviceConfig {
    /// 每块样本数。
    pub fn samples_per_block(&self) -> usize {
        self.block_frames * self.channels
    }
}

/// 后端错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// 参数非法。
    BadConfig(&'static str),
    /// 未启动就调用搬运接口。
    NotStarted,
    /// 切片长度与配置不符。
    WrongBlockLength { expected: usize, actual: usize },
    /// 后端内部故障。
    Fault(String),
}

impl core::fmt::Display for BackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BackendError::BadConfig(msg) => write!(f, "bad config: {msg}"),
            BackendError::NotStarted => write!(f, "backend not started"),
            BackendError::WrongBlockLength { expected, actual } => {
                write!(f, "wrong block length: expected {expected}, got {actual}")
            }
            BackendError::Fault(msg) => write!(f, "backend fault: {msg}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// 音频后端。实现方必须保证：`capture` 与 `playout` 在启动后不再分配内存。
pub trait AudioBackend {
    /// 打开设备并准备缓冲。
    fn start(&mut self, config: &DeviceConfig) -> Result<(), BackendError>;

    /// 采集一块输入样本（帧交错）。
    fn capture(&mut self, out: &mut [f32]) -> Result<(), BackendError>;

    /// 送出一块输出样本（帧交错）。
    fn playout(&mut self, block: &[f32]) -> Result<(), BackendError>;

    /// 关闭设备。
    fn stop(&mut self) -> Result<(), BackendError>;
}

/// 假后端：输入是确定性的斜坡，输出只做统计，不碰任何真实设备。
///
/// 用途：在没有声卡、没有 N150 的环境里跑通引擎与门禁。
#[derive(Debug, Default)]
pub struct NullBackend {
    config: Option<DeviceConfig>,
    blocks: u64,
    sample_counter: u64,
    played_blocks: u64,
    played_checksum: f64,
    played_peak: f32,
    echo: Vec<f32>,
}

impl NullBackend {
    /// 新建（未启动）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已搬运的块数。
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// 已送出的块数。
    pub fn played_blocks(&self) -> u64 {
        self.played_blocks
    }

    /// 输出样本和（double 累积，用于跨实现比对）。
    pub fn played_checksum(&self) -> f64 {
        self.played_checksum
    }

    /// 输出峰值绝对值。
    pub fn played_peak(&self) -> f32 {
        self.played_peak
    }

    /// 最近一块输出；未启动时为空。
    pub fn last_block(&self) -> &[f32] {
        &self.echo
    }
}

impl AudioBackend for NullBackend {
    fn start(&mut self, config: &DeviceConfig) -> Result<(), BackendError> {
        if config.block_frames == 0 {
            return Err(BackendError::BadConfig("block_frames must be positive"));
        }
        if config.channels == 0 {
            return Err(BackendError::BadConfig("channels must be positive"));
        }
        let echo = vec![0.0f32; config.samples_per_block()];
        self.echo = echo;
        self.config = Some(config.clone());
        self.blocks = 0;
        self.sample_counter = 0;
        self.played_blocks = 0;
        self.played_checksum = 0.0;
        self.played_peak = 0.0;
        Ok(())
    }

    fn capture(&mut self, out: &mut [f32]) -> Result<(), BackendError> {
        let config = self.config.as_ref().ok_or(BackendError::NotStarted)?;
        if out.len() != config.samples_per_block() {
            return Err(BackendError::WrongBlockLength {
                expected: config.samples_per_block(),
                actual: out.len(),
            });
        }
        for sample in out.iter_mut() {
            *sample = (self.sample_counter % 101) as f32 / 101.0 - 0.5;
            self.sample_counter = self.sample_counter.wrapping_add(1);
        }
        self.blocks += 1;
        Ok(())
    }

    fn playout(&mut self, block: &[f32]) -> Result<(), BackendError> {
        let config = self.config.as_ref().ok_or(BackendError::NotStarted)?;
        if block.len() != config.samples_per_block() {
            return Err(BackendError::WrongBlockLength {
                expected: config.samples_per_block(),
                actual: block.len(),
            });
        }
        self.echo.copy_from_slice(block);
        for sample in block {
            self.played_checksum += f64::from(*sample);
            self.played_peak = self.played_peak.max(sample.abs());
        }
        self.played_blocks += 1;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        self.config = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioBackend, BackendError, DeviceConfig, NullBackend};
    use crate::owner::DeviceId;

    fn config() -> DeviceConfig {
        DeviceConfig {
            device: DeviceId::new("null"),
            sample_rate: 96_000,
            block_frames: 128,
            channels: 2,
        }
    }

    #[test]
    fn starts_and_reports_blocks() {
        let mut backend = NullBackend::new();
        backend.start(&config()).expect("start");
        let mut input = vec![0.0f32; config().samples_per_block()];
        backend.capture(&mut input).expect("capture");
        backend.playout(&input).expect("playout");
        assert_eq!(backend.blocks(), 1);
        assert_eq!(backend.played_blocks(), 1);
        assert_eq!(backend.last_block(), &input[..]);
    }

    #[test]
    fn rejects_wrong_length_and_use_before_start() {
        let mut backend = NullBackend::new();
        assert_eq!(
            backend.capture(&mut [0.0; 4]).expect_err("start"),
            BackendError::NotStarted
        );
        backend.start(&config()).expect("start");
        assert!(matches!(
            backend.capture(&mut [0.0; 4]),
            Err(BackendError::WrongBlockLength { .. })
        ));
    }
}
