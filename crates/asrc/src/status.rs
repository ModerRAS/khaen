//! ASRC 状态快照（host 挂在 input.asrc 下对外暴露）。

/// ASRC 运行状态快照（每次取 status 时新建，Copy，不分配）。
///
/// 字段含义：
/// - `enabled`：ASRC 有效启用（配置启用且未因失败被 bypass）；
/// - `ratio_ppm`：当前实际施加给重采样器的比率修正（输出/输入 − 1，ppm）；
/// - `fill_frames`：调用方传入的抖动环 fill（帧）快照；
/// - `ppm_estimate`：控制器积分项，即估计的稳态时钟偏差修正（ppm）。
///   发送端快 d ppm 时该值收敛到约 −d；
/// - `last_error`：最近一次失败的错误名（无失败为 None）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AsrcStatus {
    /// ASRC 是否有效启用（配置启用且未被 bypass）。
    pub enabled: bool,
    /// 当前施加的比率修正（ppm）。
    pub ratio_ppm: f32,
    /// 抖动环 fill 快照（帧）。
    pub fill_frames: usize,
    /// 估计的稳态时钟偏差修正（ppm，= 积分项）。
    pub ppm_estimate: f32,
    /// 最近一次失败的错误名（稳定字符串）。
    pub last_error: Option<&'static str>,
}

impl AsrcStatus {
    /// 未启用（host 未构造 ASRC 时使用）。
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            ratio_ppm: 0.0,
            fill_frames: 0,
            ppm_estimate: 0.0,
            last_error: None,
        }
    }
}

impl Default for AsrcStatus {
    fn default() -> Self {
        Self::disabled()
    }
}
