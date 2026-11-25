//! khaen 核心：方案文件格式、实时类型与卷积内核。

pub mod crc32c;
pub mod kernels;
pub mod plan;
pub mod rt;
pub mod sha256;

pub use kernels::resample::{RationalResampler, ResamplerError};
pub use plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    PlanError, ProtectionEntry, ProtectionSection, RoutingSection, SectionKind, ABI_VERSION, MAGIC,
};
pub use rt::{
    BlockDesc, BLOCK_ABI_VERSION, BLOCK_SIZES, DEFAULT_BLOCK_SIZE, MAX_INPUT_CHANNELS,
    MAX_INPUT_ROLES, MAX_LOW_BAND_PARTICIPANTS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES,
    SAMPLE_RATES_HZ, TAPS_NORMAL_MAX,
};
