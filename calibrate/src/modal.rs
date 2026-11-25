//! 模态峰：只切。Q 来自 −3 dB 带宽，不夹到 10。
use crate::model::FrPoint;

#[derive(Clone, Debug)]
pub struct Mode {
    pub freq: f64,
    pub q: f64,
    pub gain_db: f64,
    pub q_unresolved: bool,
    pub prominence: f64,
}

pub fn detect(fr: &[FrPoint], band: (f64, f64), positions: &[Vec<FrPoint>]) -> Vec<Mode> {
    if fr.len() < 5 {
        return Vec::new();
    }
    let smooth = smooth_oct(fr, 1.0 / 24.0);
    let mut modes = Vec::new();
    for i in 1..smooth.len() - 1 {
        let f = smooth[i].freq_hz;
        if f < band.0 || f > band.1 {
            continue;
        }
        if smooth[i].db < smooth[i - 1].db || smooth[i].db <= smooth[i + 1].db {
            continue;
        }
        let prom = prominence(&smooth, i);
        if prom < 3.0 {
            continue;
        }
        if positions.len() >= 2 {
            let same = positions.iter().filter(|p| same_direction(p, f)).count();
            if prom < 2.0 || same < 2 {
                continue;
            }
        }
        let (q, unresolved) = q_from_bandwidth(&smooth, i);
        let gain = -((prom - 1.0).clamp(0.0, 12.0));
        if gain >= -0.05 {
            continue;
        }
        modes.push(Mode {
            freq: f,
            q,
            gain_db: gain,
            q_unresolved: unresolved,
            prominence: prom,
        });
    }
    merge_close(&mut modes);
    modes.sort_by(|a, b| {
        a.gain_db
            .partial_cmp(&b.gain_db)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    modes.truncate(10);
    modes.sort_by(|a, b| {
        a.freq
            .partial_cmp(&b.freq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    modes
}

fn same_direction(fr: &[FrPoint], freq: f64) -> bool {
    if fr.len() < 5 {
        return false;
    }
    let smooth = smooth_oct(fr, 1.0 / 24.0);
    let mut best_i = 0usize;
    let mut best_d = f64::MAX;
    for (i, p) in smooth.iter().enumerate() {
        let d = (p.freq_hz.ln() - freq.ln()).abs();
        if d < best_d {
            best_d = d;
            best_i = i;
        }
    }
    // 1/6 oct 以内才算同一处
    if best_d / std::f64::consts::LN_2 > 1.0 / 6.0 {
        return false;
    }
    if best_i == 0 || best_i + 1 >= smooth.len() {
        return false;
    }
    prominence(&smooth, best_i) >= 2.0
}

pub fn smooth_oct(fr: &[FrPoint], octaves: f64) -> Vec<FrPoint> {
    let half = octaves / 2.0;
    let ln2 = std::f64::consts::LN_2;
    let mut out = Vec::with_capacity(fr.len());
    for (i, p) in fr.iter().enumerate() {
        let mut acc = 0.0;
        let mut n = 0.0;
        for q in fr {
            let dist = (q.freq_hz.ln() - p.freq_hz.ln()).abs() / ln2;
            if dist <= half {
                acc += q.db;
                n += 1.0;
            }
        }
        let _ = i;
        out.push(FrPoint {
            freq_hz: p.freq_hz,
            db: if n > 0.0 { acc / n } else { p.db },
            phase_deg: p.phase_deg,
        });
    }
    out
}

/// 峰高减去 ±1/3 oct 内左右两侧各自最低点里较高的那个。
/// 宽峰的肩不能拿来当本底，否则 Q≈4 的峰再高也过不了 3 dB。
fn prominence(fr: &[FrPoint], i: usize) -> f64 {
    let f = fr[i].freq_hz;
    let ln2 = std::f64::consts::LN_2;
    let mut left_min = f64::INFINITY;
    let mut right_min = f64::INFINITY;
    for (j, p) in fr.iter().enumerate() {
        if j == i {
            continue;
        }
        let dist = (p.freq_hz.ln() - f.ln()) / ln2;
        if (-1.0 / 3.0..=0.0).contains(&dist) {
            left_min = left_min.min(p.db);
        } else if (0.0..=1.0 / 3.0).contains(&dist) {
            right_min = right_min.min(p.db);
        }
    }
    if !left_min.is_finite() || !right_min.is_finite() {
        return 0.0;
    }
    fr[i].db - left_min.max(right_min)
}

fn q_from_bandwidth(fr: &[FrPoint], i: usize) -> (f64, bool) {
    let peak = fr[i].db;
    let f0 = fr[i].freq_hz;
    let level = peak - 3.0;
    let f_lo = cross(fr, i, level, -1).unwrap_or(fr[0].freq_hz);
    let f_hi = cross(fr, i, level, 1).unwrap_or(fr[fr.len() - 1].freq_hz);
    let width = (f_hi - f_lo).max(1e-6);
    let df = if i > 0 {
        f0 - fr[i - 1].freq_hz
    } else {
        fr[1].freq_hz - f0
    }
    .abs()
    .max(1e-6);
    if width < df * 1.5 {
        return ((f0 / df).max(0.1), true);
    }
    ((f0 / width).max(0.1), false)
}

fn cross(fr: &[FrPoint], i: usize, level: f64, dir: isize) -> Option<f64> {
    let mut k = i as isize;
    loop {
        let n = k + dir;
        if n < 0 || n >= fr.len() as isize {
            return None;
        }
        let a = &fr[k as usize];
        let b = &fr[n as usize];
        if (a.db - level) * (b.db - level) <= 0.0 && (a.db - b.db).abs() > 1e-9 {
            let t = (level - a.db) / (b.db - a.db);
            return Some(a.freq_hz + t * (b.freq_hz - a.freq_hz));
        }
        if dir < 0 && b.db < level {
            let t = (level - a.db) / (b.db - a.db);
            return Some(a.freq_hz + t * (b.freq_hz - a.freq_hz));
        }
        if dir > 0 && b.db < level {
            let t = (a.db - level) / (a.db - b.db);
            return Some(a.freq_hz + t * (b.freq_hz - a.freq_hz));
        }
        k = n;
    }
}

fn merge_close(modes: &mut Vec<Mode>) {
    if modes.is_empty() {
        return;
    }
    modes.sort_by(|a, b| {
        a.freq
            .partial_cmp(&b.freq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out: Vec<Mode> = Vec::new();
    for m in modes.drain(..) {
        if let Some(prev) = out.last_mut() {
            let dist = (m.freq / prev.freq).ln().abs() / std::f64::consts::LN_2;
            if dist < 1.0 / 6.0 {
                if m.gain_db < prev.gain_db {
                    prev.freq = m.freq;
                    prev.gain_db = m.gain_db;
                    prev.prominence = m.prominence;
                }
                if m.q > prev.q {
                    prev.q = m.q;
                }
                prev.q_unresolved |= m.q_unresolved;
                continue;
            }
        }
        out.push(m);
    }
    *modes = out;
}

pub fn log_grid(f0: f64, f1: f64, per_oct: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if f1 <= f0 || per_oct <= 0.0 {
        return out;
    }
    let step = 2f64.powf(1.0 / per_oct);
    let mut f = f0;
    while f <= f1 * 1.0000001 {
        out.push(f);
        f *= step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump(f: f64, f0: f64, q: f64, peak: f64) -> f64 {
        let half = f0 / q / 2.0;
        let k = 10f64.powf(0.3) - 1.0;
        let atten = 10.0 * (1.0 + (f - f0).powi(2) / half.powi(2) * k).log10();
        (peak - atten).max(0.0)
    }

    #[test]
    fn finds_known_modes_and_skips_dip() {
        let grid = log_grid(20.0, 250.0, 48.0);
        let fr: Vec<FrPoint> = grid
            .iter()
            .map(|&f| {
                let mut db = bump(f, 50.0, 16.0, 8.0) + bump(f, 120.0, 4.0, 6.0);
                if (f - 80.0).abs() < 8.0 {
                    db -= 12.0;
                }
                FrPoint {
                    freq_hz: f,
                    db,
                    phase_deg: 0.0,
                }
            })
            .collect();
        let modes = detect(&fr, (20.0, 250.0), &[]);
        assert!(modes.iter().all(|m| m.gain_db <= 0.0));
        let m50 = modes.iter().find(|m| (m.freq - 50.0).abs() < 3.0);
        let m120 = modes.iter().find(|m| (m.freq - 120.0).abs() < 3.0);
        assert!(m50.is_some(), "modes={modes:?}");
        assert!(m120.is_some(), "modes={modes:?}");
        let q50 = m50.unwrap().q;
        assert!((q50 - 16.0).abs() / 16.0 < 0.30, "q50={q50}");
        let q120 = m120.unwrap().q;
        assert!((q120 - 4.0).abs() / 4.0 < 0.30, "q120={q120}");
        assert!(q50 > 10.0, "narrow mode must not clamp to 10, q={q50}");
    }

    #[test]
    fn coarse_grid_marks_unresolved() {
        let fr = vec![
            FrPoint {
                freq_hz: 40.0,
                db: 0.0,
                phase_deg: 0.0,
            },
            FrPoint {
                freq_hz: 50.0,
                db: 8.0,
                phase_deg: 0.0,
            },
            FrPoint {
                freq_hz: 62.0,
                db: 0.0,
                phase_deg: 0.0,
            },
            FrPoint {
                freq_hz: 80.0,
                db: 0.0,
                phase_deg: 0.0,
            },
            FrPoint {
                freq_hz: 100.0,
                db: 0.0,
                phase_deg: 0.0,
            },
        ];
        let modes = detect(&fr, (20.0, 250.0), &[]);
        assert!(modes.iter().any(|m| m.q_unresolved), "modes={modes:?}");
    }
}
