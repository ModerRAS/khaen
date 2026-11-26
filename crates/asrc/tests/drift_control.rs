//! 漂移控制端到端测试（T41 交付 3a/3b/3d）。
//!
//! 模型：本地 48 kHz 固定块消费，发送端时钟速率 = 本地 × (1 + d)，媒体帧按 1 ms
//! （48 帧）整包突发到达，与块边界不对齐（产生 fill 锯齿）。覆盖：
//! - (a) 输出无欠载、fill 稳定在 target±N 帧；
//! - (b) 比率估计（积分项）收敛到 ±5 ppm；
//! - (d) 与关闭 ASRC 的对照：同样漂移下原路径欠载/溢出明显上升。
mod support;

use khaen_asrc::Interpolation;
use support::*;

/// 目标水位（帧）：与 host 默认一致（2 块 = 256 帧 ≈ 5.3 ms）。
const TARGET_FILL: usize = 256;
/// 收敛仿真时长：控制器 500 ppm 阶跃约 10 s 收敛，留 3 倍以上余量。
const RUN_SECONDS: f64 = 36.0;
/// 统计窗口前预热：排除启动/收敛瞬态。
const WARMUP_SECONDS: f64 = 20.0;
/// fill 稳定容差（帧）：≥ 1 ms 包（48 帧）的锯齿幅度 + 余量。
const FILL_TOLERANCE: usize = 64;
/// 估计值收敛容差（ppm）。
const ESTIMATE_TOLERANCE_PPM: f64 = 5.0;

fn base_sim() -> SimConfig {
    SimConfig {
        run_seconds: RUN_SECONDS,
        warmup_seconds: WARMUP_SECONDS,
        collect_seconds: 0.0,
        ..SimConfig::default()
    }
}

/// 断言：给定发送端偏差下，fill 稳定、无欠载、估计值收敛到 −drift ±5 ppm。
fn assert_tracks(interp: Interpolation, drift_ppm: f64) {
    let sim = base_sim();
    let config = asrc_config(&sim, interp, TARGET_FILL);
    let outcome = run_with_asrc(&sim, &config, drift_ppm);
    assert_eq!(
        outcome.underruns, 0,
        "{interp:?} drift={drift_ppm}: 出现欠载（期望 0）"
    );
    assert!(
        outcome.fill_min >= TARGET_FILL - FILL_TOLERANCE,
        "{interp:?} drift={drift_ppm}: fill_min={} 低于目标 {}±{}",
        outcome.fill_min,
        TARGET_FILL,
        FILL_TOLERANCE
    );
    assert!(
        outcome.fill_max <= TARGET_FILL + FILL_TOLERANCE,
        "{interp:?} drift={drift_ppm}: fill_max={} 高于目标 {}±{}",
        outcome.fill_max,
        TARGET_FILL,
        FILL_TOLERANCE
    );
    let estimate = f64::from(outcome.status.ppm_estimate);
    assert!(
        (estimate + drift_ppm).abs() <= ESTIMATE_TOLERANCE_PPM,
        "{interp:?}: ppm 估计 {estimate:+.2} 未收敛到 {:+.2}±{}",
        -drift_ppm,
        ESTIMATE_TOLERANCE_PPM
    );
    let ratio = f64::from(outcome.status.ratio_ppm);
    assert!(
        (ratio + drift_ppm).abs() <= FILL_TOLERANCE as f64,
        "{interp:?}: 施加比率 {ratio:+.2} 偏离 {:+.2} 超过 {} ppm",
        -drift_ppm,
        FILL_TOLERANCE
    );
    assert!(
        outcome.status.enabled,
        "{interp:?}: status.enabled 应为 true"
    );
    assert!(
        outcome.status.last_error.is_none(),
        "{interp:?}: 不应有 ASRC 错误",
    );
}

/// 断言：|drift| = max_ppm 的边界工况下仍无欠载、fill 有界、估计值顶在限幅上。
fn assert_saturated(interp: Interpolation, drift_ppm: f64) {
    let sim = base_sim();
    let config = asrc_config(&sim, interp, TARGET_FILL);
    let outcome = run_with_asrc(&sim, &config, drift_ppm);
    assert_eq!(
        outcome.underruns, 0,
        "{interp:?} drift={drift_ppm}: 饱和工况出现欠载"
    );
    assert!(
        outcome.fill_min >= TARGET_FILL.saturating_sub(2 * FILL_TOLERANCE)
            && outcome.fill_max <= TARGET_FILL + 2 * FILL_TOLERANCE,
        "{interp:?} drift={drift_ppm}: fill[{},{}] 超出目标 {}±{}",
        outcome.fill_min,
        outcome.fill_max,
        TARGET_FILL,
        2 * FILL_TOLERANCE
    );
    let estimate = f64::from(outcome.status.ppm_estimate);
    assert!(
        (estimate + drift_ppm).abs() <= ESTIMATE_TOLERANCE_PPM,
        "{interp:?} drift={drift_ppm}: 饱和估计 {estimate:+.2} 偏离 {:+.2} 超过 {} ppm",
        -drift_ppm,
        ESTIMATE_TOLERANCE_PPM
    );
    let ratio = f64::from(outcome.status.ratio_ppm);
    assert!(
        (ratio + drift_ppm).abs() <= 15.0,
        "{interp:?} drift={drift_ppm}: 饱和比率 {ratio:+.2} 未顶到 {:+.0} ppm 附近",
        -drift_ppm
    );
}

#[test]
fn poly_ppm_estimate_converges_within_5ppm() {
    for drift_ppm in [200.0f64, -200.0] {
        assert_tracks(Interpolation::Poly, drift_ppm);
    }
}

#[test]
fn sinc_ppm_estimate_converges_within_5ppm() {
    for drift_ppm in [200.0f64, -200.0] {
        assert_tracks(Interpolation::Sinc, drift_ppm);
    }
}

#[test]
fn max_ppm_boundary_stays_bounded_without_underrun() {
    for interp in [Interpolation::Poly, Interpolation::Sinc] {
        for drift_ppm in [500.0f64, -500.0] {
            assert_saturated(interp, drift_ppm);
        }
    }
}

#[test]
fn without_asrc_same_drift_underruns_or_overruns() {
    // 对照：抖动环 + 本地时钟硬消费（现有路径），同样漂移下必然欠载/溢出。
    let sim = SimConfig {
        run_seconds: 30.0,
        warmup_seconds: 5.0,
        ..SimConfig::default()
    };
    let asrc_config = asrc_config(&sim, Interpolation::Poly, TARGET_FILL);

    // 负漂移（发送端慢）：水位被抽干 → 欠载。
    let legacy = run_without_asrc(&sim, -500.0, TARGET_FILL, 512);
    assert!(
        legacy.underruns > 0,
        "对照实验应出现欠载，实际 fill_min={} fill_end={}",
        legacy.fill_min,
        legacy.fill_end
    );
    let asrc = run_with_asrc(&sim, &asrc_config, -500.0);
    assert_eq!(asrc.underruns, 0, "ASRC 启用时不应欠载");

    // 正漂移（发送端快）：水位上涨 → 抖动环溢出丢帧。
    let legacy = run_without_asrc(&sim, 200.0, TARGET_FILL, 512);
    assert!(
        legacy.overruns > 0,
        "对照实验应出现抖动环溢出（丢帧），实际 overruns=0",
    );
    let asrc = run_with_asrc(&sim, &asrc_config, 200.0);
    assert_eq!(asrc.underruns, 0, "ASRC 启用时不应欠载");
    assert!(
        asrc.fill_max <= TARGET_FILL + FILL_TOLERANCE,
        "ASRC 启用时 fill 应稳定在目标附近，实际 fill_max={}",
        asrc.fill_max
    );
}

#[test]
fn startup_drains_overfilled_ring_back_to_target() {
    // 启动策略：预填明显高于目标（多 256 帧）时，控制器应把水位抽回目标且不欠载。
    let sim = SimConfig {
        run_seconds: 24.0,
        warmup_seconds: 12.0,
        prefill_overfill: 2 * TARGET_FILL,
        ..SimConfig::default()
    };
    let config = asrc_config(&sim, Interpolation::Poly, TARGET_FILL);
    let outcome = run_with_asrc(&sim, &config, -200.0);
    assert_eq!(outcome.underruns, 0, "启动抽水阶段不应欠载");
    assert!(
        outcome.fill_mean <= (TARGET_FILL + FILL_TOLERANCE) as f64,
        "启动过满后 fill 均值未回到目标附近：{:.1}",
        outcome.fill_mean
    );
    assert!(
        outcome.fill_min >= TARGET_FILL - 2 * FILL_TOLERANCE,
        "启动过满后 fill 下探过深：{}",
        outcome.fill_min
    );
}
