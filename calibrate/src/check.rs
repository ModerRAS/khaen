//! 独立的契约校验与 RBJ 幅度。拟合代码不调用这里来选择滤波器。
use serde_yaml::Value;

pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub v1_readable: bool,
}

pub fn db_at(kind: &str, freq: f64, q: f64, gain_db: f64, f: f64, fs: f64) -> f64 {
    if gain_db.abs() < 1e-6 || f <= 0.0 || fs <= 0.0 {
        return 0.0;
    }
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * freq / fs;
    let (b0, b1, b2, a0, a1, a2) = if kind == "low_shelf" || kind == "high_shelf" {
        let q = q.max(0.1);
        let inside = (a + 1.0 / a) * (1.0 / q - 1.0) + 2.0;
        let alpha = w0.sin() / 2.0 * inside.max(0.0).sqrt();
        let cos = w0.cos();
        let sq = 2.0 * a.sqrt() * alpha;
        if kind == "low_shelf" {
            (
                a * ((a + 1.0) - (a - 1.0) * cos + sq),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - sq),
                (a + 1.0) + (a - 1.0) * cos + sq,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - sq,
            )
        } else {
            (
                a * ((a + 1.0) + (a - 1.0) * cos + sq),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sq),
                (a + 1.0) - (a - 1.0) * cos + sq,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sq,
            )
        }
    } else {
        let alpha = w0.sin() / (2.0 * q.max(0.1));
        (
            1.0 + alpha * a,
            -2.0 * w0.cos(),
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * w0.cos(),
            1.0 - alpha / a,
        )
    };
    let w = 2.0 * std::f64::consts::PI * f / fs;
    let z1r = w.cos();
    let z1i = -w.sin();
    let z2r = z1r * z1r - z1i * z1i;
    let z2i = 2.0 * z1r * z1i;
    let nr = b0 + b1 * z1r + b2 * z2r;
    let ni = b1 * z1i + b2 * z2i;
    let dr = a0 + a1 * z1r + a2 * z2r;
    let di = a1 * z1i + a2 * z2i;
    let den = dr * dr + di * di;
    let mag = if den < 1e-24 {
        1.0
    } else {
        ((nr * dr + ni * di).powi(2) + (ni * dr - nr * di).powi(2)).sqrt() / den
    };
    20.0 * mag.max(1e-12).log10()
}

pub fn validate_str(text: &str) -> Result<Report, String> {
    let doc: Value = serde_yaml::from_str(text).map_err(|e| format!("YAML 解析失败: {e}"))?;
    Ok(validate_doc(&doc))
}

pub fn validate_doc(doc: &Value) -> Report {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let Some(map) = doc.as_mapping() else {
        return Report {
            errors: vec!["顶层必须是 mapping".into()],
            warnings,
            v1_readable: false,
        };
    };
    let schema = map
        .get(Value::from("schema"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let v11 = schema == "tspi-calibration/v1.1";
    let v1 = schema == "tspi-calibration/v1";
    if !v1 && !v11 {
        errors.push(format!(
            "schema 必须是 tspi-calibration/v1 或 v1.1（当前 {schema:?}）"
        ));
    }
    let fs = as_int(map.get(Value::from("sample_rate"))).unwrap_or(-1);
    if fs < 0 {
        errors.push("sample_rate 必须是整数".into());
    } else if !matches!(fs, 44100 | 48000 | 88200 | 96000) {
        warnings.push(format!("sample_rate={fs} 不是部署口径"));
    }
    let fs_f = if fs > 0 { fs as f64 } else { 48_000.0 };
    let channels = map
        .get(Value::from("channels"))
        .and_then(Value::as_sequence);
    let Some(channels) = channels else {
        errors.push("channels 必须是非空数组".into());
        return Report {
            errors,
            warnings,
            v1_readable: false,
        };
    };
    if channels.is_empty() {
        errors.push("channels 必须是非空数组".into());
    }
    if channels.len() > 32 {
        errors.push("channels 超过 32 条".into());
    }
    let mut v1_ok = v1 || v11;
    let mut delays = Vec::new();
    let mut indexes = Vec::new();
    for (i, ch) in channels.iter().enumerate() {
        let Some(ch) = ch.as_mapping() else {
            errors.push(format!("channels[{i}] 必须是 mapping"));
            continue;
        };
        let p = format!("channels[{i}]");
        let name = ch
            .get(Value::from("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !is_role(name) {
            errors.push(format!("{p}.name 必须是角色 id"));
        }
        let idx = as_int(ch.get(Value::from("index"))).unwrap_or(-1);
        if idx < 0 {
            errors.push(format!("{p}.index 必须是非负整数"));
        } else if indexes.contains(&idx) {
            errors.push(format!("{p}.index 重复"));
        } else {
            indexes.push(idx);
        }
        let d = as_int(ch.get(Value::from("delay_samples"))).unwrap_or(-1);
        if d < 0 {
            errors.push(format!("{p}.delay_samples 必须是非负整数"));
        } else {
            let dmax = (0.2 * fs_f).floor() as i64;
            if d > dmax {
                errors.push(format!("{p}.delay_samples 超过 200 ms"));
            }
            delays.push(d);
        }
        match ch.get(Value::from("polarity")).and_then(Value::as_str) {
            Some("normal" | "invert") | None => {}
            Some(other) => {
                errors.push(format!("{p}.polarity 必须是 normal|invert（当前 {other}）"))
            }
        }
        if let Some(g) = as_f64(ch.get(Value::from("gain_db"))) {
            if !(-12.0..=6.0).contains(&g) {
                errors.push(format!("{p}.gain_db 必须在 [-12,+6]"));
            }
        }
        let filters = ch
            .get(Value::from("filters"))
            .and_then(Value::as_sequence)
            .cloned()
            .unwrap_or_default();
        if filters.len() > 32 {
            errors.push(format!("{p}.filters 超过 32 段"));
        }
        let mut pos = 0.0;
        let fmax = 20_000.0_f64.min(fs_f / 2.0 - 1.0);
        for (j, f) in filters.iter().enumerate() {
            let Some(f) = f.as_mapping() else {
                errors.push(format!("{p}.filters[{j}] 必须是 mapping"));
                continue;
            };
            let pp = format!("{p}.filters[{j}]");
            let freq = as_f64(f.get(Value::from("freq"))).unwrap_or(f64::NAN);
            let q = as_f64(f.get(Value::from("q"))).unwrap_or(f64::NAN);
            let gain = as_f64(f.get(Value::from("gain_db"))).unwrap_or(f64::NAN);
            if !freq.is_finite() || !(20.0..=fmax).contains(&freq) {
                errors.push(format!("{pp}.freq 超出范围"));
            }
            if !q.is_finite() || q < 0.1 {
                errors.push(format!("{pp}.q 必须是有限数且 ≥ 0.1"));
            }
            if !gain.is_finite() || !(-24.0..=24.0).contains(&gain) {
                errors.push(format!("{pp}.gain_db 必须在 [-24,+24]"));
            } else if gain > 0.0 {
                pos += gain;
            }
            let ty = f
                .get(Value::from("type"))
                .and_then(Value::as_str)
                .unwrap_or("peaking");
            if !matches!(ty, "peaking" | "low_shelf" | "high_shelf") {
                errors.push(format!("{pp}.type 不认识（当前 {ty}）"));
                v1_ok = false;
            } else if v1 && ty != "peaking" {
                errors.push(format!("{pp}.type v1 只允许 peaking"));
                v1_ok = false;
            } else if (ty == "low_shelf" || ty == "high_shelf") && q > 1.0 {
                // 裁决 5：shelf 的 q=RBJ slope S ∈[0.1,1.0]；CamillaDSP slope=12*S，
                // S>1 → slope>12 dB/oct 根本执行不了，必须拒绝而不是放行。
                errors.push(format!(
                    "{pp}.q={q}（shelf 的 slope S 必须在 [0.1,1.0]；CamillaDSP slope=12S={:.1} dB/oct >12）",
                    12.0 * q
                ));
                v1_ok = false;
            } else if v1 && q > 10.0 {
                errors.push(format!("{pp}.q v1 必须 ≤ 10"));
                v1_ok = false;
            } else if ty != "peaking" || q > 10.0 {
                v1_ok = false;
            }
        }
        if pos > 12.0 {
            errors.push(format!("{p} 正向增益总和 {pos:.1} dB > 12 dB"));
        } else if pos > 6.0 {
            warnings.push(format!("{p} 正向增益总和 {pos:.1} dB > 6 dB"));
        }
    }
    if !delays.is_empty() {
        let mn = delays.iter().copied().min().unwrap_or(0);
        if mn != 0 {
            errors.push(format!("delay_samples 未归一化：min={mn}"));
        }
    }
    if let Some(r) = as_int(map.get(Value::from("reference_index"))) {
        if !indexes.contains(&r) {
            errors.push("reference_index 不在 channels.index 中".into());
        }
    } else {
        errors.push("reference_index 必须是非负整数".into());
    }
    // P2：v1_readable 只对 v1.1 输出有意义；且存在任何 error 时不提示“v1 可读”。
    let v1_readable = v11
        && v1_ok
        && errors.is_empty()
        && errors
            .iter()
            .all(|e| !e.contains("type") && !e.contains(".q"));
    if v11 && v1_ok && errors.is_empty() {
        warnings.push("该 v1.1 文件只含 Peaking 且 Q ≤ 10，v1 消费方可读".into());
    }
    Report {
        errors,
        warnings,
        v1_readable,
    }
}

fn is_role(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {
            chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        }
        _ => false,
    }
}

fn as_int(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    if let Some(u) = v.as_u64() {
        return i64::try_from(u).ok();
    }
    let f = v.as_f64()?;
    if f.is_finite() && (f - f.round()).abs() < 1e-9 {
        Some(f.round() as i64)
    } else {
        None
    }
}

fn as_f64(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    v.as_i64().map(|i| i as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_rejects_shelf_and_high_q() {
        let text = r#"
schema: tspi-calibration/v1
sample_rate: 48000
reference_index: 0
channels:
  - name: FL
    index: 0
    delay_samples: 0
    polarity: normal
    filters:
      - freq: 50
        q: 12
        gain_db: -4
        type: peaking
"#;
        let r = validate_str(text).unwrap();
        assert!(r.errors.iter().any(|e| e.contains("q")), "{:?}", r.errors);
    }

    #[test]
    fn v11_accepts_shelf_and_high_q() {
        let text = r#"
schema: tspi-calibration/v1.1
sample_rate: 48000
reference_index: 0
channels:
  - name: FL
    index: 0
    delay_samples: 0
    polarity: normal
    filters:
      - {freq: 50, q: 16, gain_db: -4, type: peaking}
      - {freq: 4000, q: 0.7, gain_db: -2, type: high_shelf}
"#;
        let r = validate_str(text).unwrap();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!r.v1_readable);
    }

    #[test]
    fn unknown_type_fails() {
        let text = r#"
schema: tspi-calibration/v1.1
sample_rate: 48000
reference_index: 0
channels:
  - name: FL
    index: 0
    delay_samples: 0
    polarity: normal
    filters:
      - {freq: 1000, q: 1, gain_db: -1, type: notch}
"#;
        let r = validate_str(text).unwrap();
        assert!(
            r.errors.iter().any(|e| e.contains("type")),
            "{:?}",
            r.errors
        );
    }

    fn v11_doc_filter(q: &str, ty: &str) -> String {
        format!(
            r#"
schema: tspi-calibration/v1.1
sample_rate: 48000
reference_index: 0
channels:
  - name: FL
    index: 0
    delay_samples: 0
    polarity: normal
    filters:
      - freq: 4000
        q: {q}
        gain_db: -2.0
        type: {ty}
"#
        )
    }

    #[test]
    fn shelf_q_above_one_is_error() {
        // 裁决 5：shelf q=RBJ slope S ∈[0.1,1.0]；q>1 → error（不产不可应用文件）。
        let ok = validate_str(&v11_doc_filter("1.0", "high_shelf")).unwrap();
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);

        let bad = validate_str(&v11_doc_filter("1.01", "high_shelf")).unwrap();
        assert!(
            bad.errors
                .iter()
                .any(|e| e.contains("shelf") && e.contains("q")),
            "errors={:?}",
            bad.errors
        );
        assert!(!bad.v1_readable);

        // v1.1 peaking 仍无 Q 上限，q=69.7 必须放行。
        let peak = validate_str(&v11_doc_filter("69.7", "peaking")).unwrap();
        assert!(peak.errors.is_empty(), "{:?}", peak.errors);
        assert!(!peak.v1_readable, "q>10 不应给 v1 可读提示");
    }
}
