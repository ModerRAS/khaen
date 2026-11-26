//! ASRC 错误类型（Copy；稳定错误名供 status/日志/证据使用，不参与 RT 决策）。

use core::fmt;

use rubato::{ResampleError, ResamplerConstructionError};

/// ASRC 错误。
///
/// RT 路径只把错误名放进 [crate::AsrcStatus::last_error]，不格式化、不分配。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsrcError {
    /// 配置非法（见 [crate::AsrcConfig::validate]）。
    BadConfig(&'static str),
    /// rubato 构造失败（参数/CPU 特性）。
    Construction(&'static str),
    /// 期望的比率超出 rubato 构造时允许的相对范围（防御性错误）。
    RatioOutOfBounds,
    /// 处理失败（缓冲尺寸/声道数不匹配等；正常配置下不应发生）。
    Process(&'static str),
}

impl AsrcError {
    /// 稳定错误名（证据/JSON/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            AsrcError::BadConfig(_) => "ERR_ASRC_CONFIG",
            AsrcError::Construction(_) => "ERR_ASRC_CONSTRUCTION",
            AsrcError::RatioOutOfBounds => "ERR_ASRC_RATIO_OUT_OF_BOUNDS",
            AsrcError::Process(_) => "ERR_ASRC_PROCESS",
        }
    }
}

impl fmt::Display for AsrcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsrcError::BadConfig(reason) => write!(f, "bad config: {reason}"),
            AsrcError::Construction(reason) => write!(f, "construction failed: {reason}"),
            AsrcError::RatioOutOfBounds => f.write_str("resample ratio out of bounds"),
            AsrcError::Process(reason) => write!(f, "process failed: {reason}"),
        }
    }
}

impl std::error::Error for AsrcError {}

/// rubato 构造错误 → 本 crate 错误（只保留稳定原因，不格式化浮点值）。
pub(crate) fn map_construction_error(error: ResamplerConstructionError) -> AsrcError {
    match error {
        ResamplerConstructionError::InvalidRatio(_) => {
            AsrcError::BadConfig("resample_ratio must be finite and > 0")
        }
        ResamplerConstructionError::InvalidRelativeRatio(_) => {
            AsrcError::BadConfig("max_resample_ratio_relative must be finite and >= 1")
        }
        ResamplerConstructionError::InvalidChunkSize(_) => {
            AsrcError::BadConfig("chunk_size must be >= 1")
        }
        ResamplerConstructionError::InvalidSampleRate { .. } => {
            AsrcError::BadConfig("sample rates must both be > 0")
        }
        _ => AsrcError::Construction("rubato rejected its parameters"),
    }
}

/// rubato 处理错误 → 本 crate 错误（稳定原因）。
pub(crate) fn map_resample_error(error: ResampleError) -> AsrcError {
    match error {
        ResampleError::RatioOutOfBounds { .. } | ResampleError::RatioOutsideRange { .. } => {
            AsrcError::RatioOutOfBounds
        }
        ResampleError::WrongNumberOfInputChannels { .. }
        | ResampleError::WrongNumberOfOutputChannels { .. }
        | ResampleError::WrongNumberOfMaskChannels { .. } => {
            AsrcError::Process("channel count mismatch")
        }
        ResampleError::InsufficientInputBufferSize { .. } => {
            AsrcError::Process("input buffer too small")
        }
        ResampleError::InsufficientOutputBufferSize { .. } => {
            AsrcError::Process("output buffer too small")
        }
        ResampleError::InvalidChunkSize { .. } => AsrcError::Process("invalid chunk size"),
        _ => AsrcError::Process("resample failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_names_are_stable() {
        assert_eq!(AsrcError::BadConfig("x").name(), "ERR_ASRC_CONFIG");
        assert_eq!(AsrcError::Construction("x").name(), "ERR_ASRC_CONSTRUCTION");
        assert_eq!(
            AsrcError::RatioOutOfBounds.name(),
            "ERR_ASRC_RATIO_OUT_OF_BOUNDS"
        );
        assert_eq!(AsrcError::Process("x").name(), "ERR_ASRC_PROCESS");
    }
}
