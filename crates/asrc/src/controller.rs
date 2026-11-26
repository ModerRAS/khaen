//! 漂移控制器：抖动环 fill 误差 → 比率修正 ppm。
//!
//! # 环路
//!
//! 抖动环 fill 的物理模型（本地输出固定速率、媒体按发送端时钟到达）：
//!
//! ~~~text
//! d(fill)/dt = fs·[(1 + d) − 1/ratio] ≈ fs·(d + ppm)
//! ~~~
//!
//! 其中 d 是发送端相对本地时钟的偏差（ppm），ppm = ratio − 1 是施加的比率修正
//! （ratio = 输出/输入采样率；插值器固定输出块，比率越小时每块消耗的输入帧越少）。
//! 因此：fill 低于目标 → 需要正 ppm（每块少消费输入）把水位拉回；fill 高于目标 → 负 ppm。
//!
//! 控制律 = fill 一阶低通 + PI + 积分限幅抗饱和 + 启动冻结：
//! - 低通（[AsrcConfig::fill_lpf_sec]，默认 1 s）滤掉收包节拍造成的锯齿
//!   （AES67 1 ms 包 + 2.67 ms 块会拍出 ~2.3 Hz 的 fill 锯齿，见 README 误差预算）；
//! - P 项（[AsrcConfig::kp_ppm_per_frame]）提供阻尼，I 项（[AsrcConfig::ki_ppm_per_frame_sec]）
//!   消除稳态误差，最终收敛到 I ≈ −d（[Asrc::ppm_estimate] 即 I）；
//! - 输出与积分分别限幅 ±[AsrcConfig::max_ppm]：积分在饱和时仍可缓慢爬升，
//!   保证"发送端偏差 = max_ppm"的边界工况也能把 fill 拉回目标（没有余量时冻结积分会留下永久偏置）；
//! - 启动 [AsrcConfig::startup_blocks] 块内只更新低通、输出 0，避免预填水位/初始延迟
//!   被积分成假偏差。
//!
//! # 失效/重置
//!
//! 换流（SSRC 变化/时间戳跳变）或 ASRC 失败 bypass 时由调用方调用 [DriftController::reset]，
//! 清零积分与低通、回到启动阶段；不保留旧流的偏差估计。

use crate::config::AsrcConfig;
use crate::error::AsrcError;

/// fill 误差在进入 P/I 前的限幅（相对目标水位的倍数）：防止异常 fill 造成数值爆掉。
const ERROR_LIMIT_RATIO: f64 = 4.0;

/// 抖动环 fill → 比率修正的 PI 控制器（构造期固定增益，RT 路径只做算术）。
#[derive(Clone, Copy, Debug)]
pub struct DriftController {
    target_fill: f64,
    max_ppm: f64,
    kp: f64,
    ki: f64,
    dt: f64,
    lpf_alpha: f64,
    error_limit: f64,
    fill_filter: f64,
    integral: f64,
    ratio_ppm: f64,
    last_error: f64,
    warmup_left: u32,
    startup_blocks: u32,
    updates: u64,
}

impl DriftController {
    /// 按配置构造控制器（先校验配置）。
    pub fn new(config: &AsrcConfig) -> Result<Self, AsrcError> {
        config.validate()?;
        let dt = config.block_seconds();
        let target_fill = config.target_fill_frames as f64;
        Ok(Self {
            target_fill,
            max_ppm: config.max_ppm,
            kp: config.kp_ppm_per_frame,
            ki: config.ki_ppm_per_frame_sec,
            dt,
            // 一阶低通：alpha = dt / (tau + dt)。
            lpf_alpha: dt / (config.fill_lpf_sec + dt),
            error_limit: target_fill * ERROR_LIMIT_RATIO,
            fill_filter: target_fill,
            integral: 0.0,
            ratio_ppm: 0.0,
            last_error: 0.0,
            warmup_left: config.startup_blocks,
            startup_blocks: config.startup_blocks,
            updates: 0,
        })
    }

    /// 输入一个 fill 快照（帧），返回本次应施加的比率修正（ppm，已限幅）。
    ///
    /// 实时约束：纯算术，无分配/锁/IO/panic。
    pub fn update(&mut self, fill_frames: usize) -> f64 {
        let fill = fill_frames as f64;
        self.fill_filter += self.lpf_alpha * (fill - self.fill_filter);
        self.updates = self.updates.saturating_add(1);
        let error =
            (self.target_fill - self.fill_filter).clamp(-self.error_limit, self.error_limit);
        self.last_error = error;
        if self.warmup_left > 0 {
            self.warmup_left -= 1;
            self.ratio_ppm = 0.0;
            return 0.0;
        }
        // 积分项独立限幅（anti-windup）：即使 P 项瞬时饱和，积分仍能慢慢爬到真实偏差，
        // 这样"发送端偏差 = max_ppm"这种边界工况也能把 fill 拉回目标（窗口没有余量时
        // 冻结积分会让 fill 永久偏置）。
        let integral =
            (self.integral + self.ki * error * self.dt).clamp(-self.max_ppm, self.max_ppm);
        self.integral = integral;
        let ppm = (self.kp * error + integral).clamp(-self.max_ppm, self.max_ppm);
        self.ratio_ppm = ppm;
        ppm
    }

    /// 回到启动状态（换流/失效时调用）。
    pub fn reset(&mut self) {
        self.fill_filter = self.target_fill;
        self.integral = 0.0;
        self.ratio_ppm = 0.0;
        self.last_error = 0.0;
        self.warmup_left = self.startup_blocks;
        self.updates = 0;
    }

    /// 当前施加的比率修正（ppm）。
    pub fn ppm(&self) -> f64 {
        self.ratio_ppm
    }

    /// 积分项 = 估计的稳态时钟偏差修正（ppm）。
    pub fn estimate_ppm(&self) -> f64 {
        self.integral
    }

    /// 最近一次进入 PI 的 fill 误差（帧，目标 − 低通后的 fill）。
    pub fn last_error_frames(&self) -> f64 {
        self.last_error
    }

    /// 已更新次数（诊断用）。
    pub fn updates(&self) -> u64 {
        self.updates
    }
}
