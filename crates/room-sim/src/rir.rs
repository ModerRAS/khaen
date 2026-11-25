//! RIR 渲染：把 arrival 列表写成定长样本缓冲。放置规则与 RoomForge ShoeboxRir.cs 的
//! ArrivalPlacement 完全一致（linear: lower=floor(pos)，分给 lower/lower+1；nearest: half away from zero）。
//! 与 C# 的差异只有一处、且是契约要求的：本移植写入调用方给定的固定长度缓冲，越界丢弃
//! （C# Render 返回 ceil(maxDelay·fs)+1+2 的变长数组）。

pub use crate::ism::{Arrival, ArrivalPlacement};

/// 变长渲染（render_auto）在最后一个 arrival 之后保留的样本数，用于容纳分数延迟的另一半。
pub const PLACEMENT_SLACK_SAMPLES: usize = 2;

/// 固定长度渲染：返回恰好 length_samples 个 f64（不足补 0），越界 arrival 丢弃。
///
/// # Panics
/// sample_rate_hz 非正或非有限时 panic。
pub fn render(
    arrivals: &[Arrival],
    sample_rate_hz: f64,
    placement: ArrivalPlacement,
    length_samples: usize,
) -> Vec<f64> {
    assert!(
        sample_rate_hz > 0.0 && sample_rate_hz.is_finite(),
        "RIR: sample rate must be > 0 Hz and finite, got {sample_rate_hz}"
    );
    let mut response = vec![0.0; length_samples];
    for arrival in arrivals {
        place(&mut response, arrival, sample_rate_hz, placement);
    }
    response
}

/// C# ShoeboxRir.Render 的变长副本：长度 = ceil(maxDelay·fs) + 1 + PLACEMENT_SLACK_SAMPLES；
/// 空输入 → 空（接收者与源之间没有路径是静音，不是异常）。
pub fn render_auto(
    arrivals: &[Arrival],
    sample_rate_hz: f64,
    placement: ArrivalPlacement,
) -> Vec<f64> {
    assert!(
        sample_rate_hz > 0.0 && sample_rate_hz.is_finite(),
        "RIR: sample rate must be > 0 Hz and finite, got {sample_rate_hz}"
    );
    if arrivals.is_empty() {
        return Vec::new();
    }
    let length = rendered_length(arrivals, sample_rate_hz);
    render(arrivals, sample_rate_hz, placement, length)
}

/// C# ShoeboxRir.Render 的缓冲长度公式。
pub fn rendered_length(arrivals: &[Arrival], sample_rate_hz: f64) -> usize {
    let mut max_delay: f64 = 0.0;
    for arrival in arrivals {
        if arrival.delay_seconds > max_delay {
            max_delay = arrival.delay_seconds;
        }
    }
    let sample_count = (max_delay * sample_rate_hz).ceil();
    let sample_count = if sample_count.is_finite() && sample_count > 0.0 {
        sample_count as usize
    } else {
        0
    };
    sample_count + 1 + PLACEMENT_SLACK_SAMPLES
}

fn place(
    response: &mut [f64],
    arrival: &Arrival,
    sample_rate_hz: f64,
    placement: ArrivalPlacement,
) {
    let position = arrival.delay_seconds * sample_rate_hz;
    if !position.is_finite() {
        return;
    }
    match placement {
        ArrivalPlacement::NearestSample => {
            // C#: (int)Math.Round(position, MidpointRounding.AwayFromZero)
            let index = position.round();
            if index >= 0.0 && index < response.len() as f64 {
                response[index as usize] += arrival.amplitude;
            }
        }
        ArrivalPlacement::LinearInterpolated => {
            let lower = position.floor();
            let fraction = position - lower;
            let lower_index = lower as i64;
            if lower_index >= 0 && (lower_index as usize) < response.len() {
                response[lower_index as usize] += arrival.amplitude * (1.0 - fraction);
            }
            let upper_index = lower_index + 1;
            if upper_index >= 0 && (upper_index as usize) < response.len() {
                response[upper_index as usize] += arrival.amplitude * fraction;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ism::arrivals;
    use crate::room::{Position, RoomModel, SurfaceReflections};

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual} (tolerance {tolerance})"
        );
    }

    #[test]
    fn linear_placement_splits_a_fractional_arrival() {
        let mut list = [Arrival::new(10.5 / 48_000.0, 1.0)];
        let response = render(
            &mut list,
            48_000.0,
            ArrivalPlacement::LinearInterpolated,
            16,
        );
        assert_eq!(response.len(), 16);
        close(response[10], 0.5, 1e-15);
        close(response[11], 0.5, 1e-15);
        for (index, value) in response.iter().enumerate() {
            if index != 10 && index != 11 {
                assert_eq!(*value, 0.0);
            }
        }
    }

    #[test]
    fn nearest_placement_rounds_away_from_midpoint() {
        let list = [
            Arrival::new(10.5 / 48_000.0, 1.0),
            Arrival::new(10.49 / 48_000.0, 2.0),
        ];
        let response = render(&list, 48_000.0, ArrivalPlacement::NearestSample, 16);
        close(response[11], 1.0, 1e-15);
        close(response[10], 2.0, 1e-15);
    }

    #[test]
    fn out_of_range_arrivals_are_dropped() {
        let list = [Arrival::new(100.0 / 48_000.0, 1.0)];
        let response = render(&list, 48_000.0, ArrivalPlacement::LinearInterpolated, 8);
        assert_eq!(response.len(), 8);
        assert!(response.iter().all(|value| *value == 0.0));
        // 负数位置 floor = -1、fraction = 0.75：lower 越界丢弃，upper = 0 得到 0.75 份额。
        let negative = [Arrival::new(-0.25 / 48_000.0, 0.5)];
        let response = render(&negative, 48_000.0, ArrivalPlacement::LinearInterpolated, 8);
        close(response[0], 0.5 * 0.75, 1e-15);
    }

    #[test]
    fn auto_length_matches_csharp_formula() {
        let list = [Arrival::new(0.25, 1.0), Arrival::new(0.125, 0.5)];
        assert_eq!(
            rendered_length(&list, 48_000.0),
            12_000 + 1 + PLACEMENT_SLACK_SAMPLES
        );
        let auto = render_auto(&list, 48_000.0, ArrivalPlacement::LinearInterpolated);
        assert_eq!(auto.len(), 12_003);
        let fixed = render(
            &list,
            48_000.0,
            ArrivalPlacement::LinearInterpolated,
            auto.len(),
        );
        assert_eq!(auto, fixed);
        assert!(render_auto(&[], 48_000.0, ArrivalPlacement::NearestSample).is_empty());
    }

    #[test]
    fn reference_room_a_pair_peak_and_delay() {
        // 与 Python 独立参考（contract §4 校准口径）对照：room 4.10×5.20×2.70、r=0.866、order=12、
        // SUB1(0.55,1.40,0.30) → P01(1.15,1.40,1.20)。期望 max delay 0.25219 s、渲染峰值 0.58236。
        let room = RoomModel::new(
            4.10,
            5.20,
            2.70,
            343.0,
            SurfaceReflections::new(0.866, 0.866, 0.866, 0.866, 0.866, 0.866),
        );
        let paths = arrivals(
            &room,
            Position::new(0.55, 1.40, 0.30),
            Position::new(1.15, 1.40, 1.20),
            12,
            1.0,
        );
        assert_eq!(paths.len(), 15_625);
        let max_delay = paths
            .iter()
            .map(|arrival| arrival.delay_seconds)
            .fold(0.0f64, f64::max);
        close(max_delay, 0.25219400383443513, 1e-12);
        let response = render(
            &paths,
            48_000.0,
            ArrivalPlacement::LinearInterpolated,
            24_000,
        );
        let peak = response
            .iter()
            .fold(0.0f64, |best, value| best.max(value.abs()));
        close(peak, 0.5823587482922874, 1e-9);
    }

    #[test]
    fn challenge_scene_peak_and_delay() {
        // 非对称反射房间 5.00×6.00×2.80，SUB3(4.45,5.15,0.30) → P10(4.55,5.20,1.20)：
        // Python 参考峰值 1.00959；该对最大到达延迟 0.29145 s（整场最大延迟 0.30788 s 出现在另一对）。
        let room = RoomModel::new(
            5.00,
            6.00,
            2.80,
            343.0,
            SurfaceReflections::new(0.92, 0.70, 0.92, 0.60, 0.60, 0.80),
        );
        let paths = arrivals(
            &room,
            Position::new(4.45, 5.15, 0.30),
            Position::new(4.55, 5.20, 1.20),
            12,
            1.0,
        );
        let max_delay = paths
            .iter()
            .map(|arrival| arrival.delay_seconds)
            .fold(0.0f64, f64::max);
        close(max_delay, 0.2914498750900575, 1e-12);
        let response = render(
            &paths,
            48_000.0,
            ArrivalPlacement::LinearInterpolated,
            24_000,
        );
        let peak = response
            .iter()
            .fold(0.0f64, |best, value| best.max(value.abs()));
        close(peak, 1.0095936642663423, 1e-9);
    }
}
