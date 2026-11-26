//! khaen 分析工具链（纯 Rust，无 numpy）：WAV 频谱 / 包络 / 相对频带电平。
//!
//! 设计目标：把真机测量后的离线分析（Welch 频谱、短时包络/掉音检测、两段录音相对频带
//! 电平）变成仓库内可复现的工具，而不是一次性 numpy 脚本。全部命令**只读 WAV 文件、
//! 只写 JSON**；不碰设备、不联网、不需要 Python 环境。
//!
//! # 口径（与既有工具一致，不复制数值逻辑）
//!
//! | 分析 | 实现来源 |
//! | --- | --- |
//! | WAV 解码（PCM 16/24/32、float32、1..=8 声道混单声道） | khaen_calibration::wav |
//! | Welch 带功率 / 相对带电平 | khaen_calibration::band_levels（与 khaen-band-levels 同一份） |
//! | 1/3 oct 默认中心频率（25 Hz-20 kHz，30 带） | khaen_calibration::DEFAULT_CENTERS_HZ |
//! | 默认窗长 4096、步长一半、Hann 窗 | khaen_calibration::band_levels::DEFAULT_WINDOW_LEN |
//!
//! 只有 **PSD 数组输出** 与 **短时包络/掉音检测** 是本 crate 的新代码：前者用 realfft
//! 按 band_levels 文档的归一化逐 bin 计算（单元测试锁定「PSD 按 bin 带宽积分 = 带功率」），
//! 后者是 2026-11-26 真机闭环复测首次基线录音中途掉音（4.3-4.9 s）催生的诊断能力。
//!
//! # 确定性
//!
//! 所有 JSON 用键字典序对象序列化（json::object）、末尾换行；同一输入两次运行逐字节
//! 相同、sha256 可复现（tests/ 有断言）。数值只来自输入 + 固定配置，不读时钟、不依赖
//! 线程/哈希/迭代顺序。
//!
//! # 错误码
//!
//! 见 codes 模块：CLI 退出码 0 = 成功、2 = 输入/用法错误、1 = 写输出失败；错误写 stderr
//! 一行 "khaen-analyze: <CODE>: <message>"。

pub mod compare;
pub mod envelope;
pub mod json;
pub mod snr;
pub mod spectrum;
pub mod wav;

/// 工具名（写入报告与 stdout 摘要）。
pub const TOOL_NAME: &str = "khaen-analyze";
/// spectrum 输出 schema。
pub const SPECTRUM_SCHEMA: &str = "khaen-analyze-spectrum/v1";
/// envelope 输出 schema。
pub const ENVELOPE_SCHEMA: &str = "khaen-analyze-envelope/v1";
/// compare 输出 schema。
pub const COMPARE_SCHEMA: &str = "khaen-analyze-compare/v1";
/// band-snr 输出 schema。
pub const BAND_SNR_SCHEMA: &str = "khaen-analyze-band-snr/v1";

/// 稳定错误码（CLI stderr / 报告可直接匹配；**不要改字符串**）。
pub mod codes {
    /// 命令行用法错误（未知子命令/参数、缺参数、非法取值）。
    pub const USAGE: &str = "E_USAGE";
    /// 写输出失败（目标已存在、父目录不可建、写盘失败）。
    pub const WRITE: &str = "E_WRITE";
    /// 包络/录音不合法（非有限样本、录音短于一个窗、窗内均方溢出）。
    pub const ANALYZE_INPUT: &str = "E_ANALYZE_INPUT";
    /// 包络配置非法（窗长、掉音阈值、最少窗数）。
    pub const ANALYZE_CONFIG: &str = "E_ANALYZE_CONFIG";
    /// 两段录音无法对齐（采样率不一致）。
    pub const ANALYZE_ALIGNMENT: &str = "E_ANALYZE_ALIGNMENT";

    // WAV 解码与 Welch 带功率的稳定码直接复用 khaen-calibration（同一字符串，不另造）。
    pub use khaen_calibration::wav::{BAD_INPUT as WAV_INPUT, IO as WAV_IO};
    pub use khaen_calibration::{BAND_LEVELS_ALIGNMENT, BAND_LEVELS_BAD_INPUT, BAND_LEVELS_CONFIG};
}

/// 分析错误：稳定码 + 人读消息（与 khaen-calibration 的错误类型同一风格）。
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyzeError {
    code: &'static str,
    message: String,
}

impl AnalyzeError {
    /// 构造错误。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码（见 codes 模块）。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for AnalyzeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AnalyzeError {}
