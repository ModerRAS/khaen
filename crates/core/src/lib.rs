//! khaen 核心：方案文件格式、实时类型与卷积内核。

pub mod crc32c;
pub mod plan;
pub mod sha256;

pub use plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    PlanError, ProtectionEntry, ProtectionSection, RoutingSection, SectionKind, ABI_VERSION, MAGIC,
};
