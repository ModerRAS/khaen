//! 目标曲线：只用于拟合和报告，不写入校准文件的应用字段。
use serde::Deserialize;

const EMBEDDED: &str = include_str!("../target-curves.yml");

#[derive(Clone, Debug)]
pub struct Curve {
    pub id: String,
    pub points: Vec<(f64, f64)>,
}

#[derive(Deserialize)]
struct FileDoc {
    schema: String,
    default: String,
    curves: std::collections::BTreeMap<String, CurveBody>,
}

#[derive(Deserialize)]
struct CurveBody {
    points: Vec<(f64, f64)>,
}

impl Curve {
    pub fn flat() -> Self {
        Self {
            id: "flat".into(),
            points: vec![(20.0, 0.0), (1000.0, 0.0), (20_000.0, 0.0)],
        }
    }

    pub fn db_at(&self, freq_hz: f64) -> f64 {
        let f = freq_hz.max(1e-9);
        let pts = &self.points;
        if pts.is_empty() {
            return 0.0;
        }
        if f <= pts[0].0 {
            return pts[0].1;
        }
        if f >= pts[pts.len() - 1].0 {
            return pts[pts.len() - 1].1;
        }
        let mut i = 1;
        while i < pts.len() && pts[i].0 < f {
            i += 1;
        }
        let (f0, d0) = pts[i - 1];
        let (f1, d1) = pts[i];
        let t = ((f.ln() - f0.ln()) / (f1.ln() - f0.ln())).clamp(0.0, 1.0);
        d0 + t * (d1 - d0)
    }
}

pub fn resolve(spec: Option<&str>) -> Result<Curve, String> {
    let spec = spec.unwrap_or("flat").trim();
    if spec.is_empty() || spec == "flat" {
        return lookup_embedded("flat");
    }
    if let Some(rest) = spec.strip_prefix('@') {
        let (path, id) = rest.split_once('#').unwrap_or((rest, ""));
        return load_file(std::path::Path::new(path), id);
    }
    lookup_embedded(spec)
}

fn lookup_embedded(id: &str) -> Result<Curve, String> {
    let doc = parse_doc(EMBEDDED)?;
    take(&doc, id)
}

fn load_file(path: &std::path::Path, id: &str) -> Result<Curve, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取目标曲线 {}: {e}", path.display()))?;
    let doc = parse_doc(&text)?;
    let id = if id.is_empty() {
        doc.default.as_str()
    } else {
        id
    };
    take(&doc, id)
}

fn parse_doc(text: &str) -> Result<FileDoc, String> {
    let doc: FileDoc = serde_yaml::from_str(text).map_err(|e| format!("目标曲线格式错误: {e}"))?;
    if doc.schema != "tspi-target-curve/v1" {
        return Err(format!(
            "目标曲线 schema 必须是 tspi-target-curve/v1（当前 {}）",
            doc.schema
        ));
    }
    for (id, body) in &doc.curves {
        check_points(id, &body.points)?;
    }
    Ok(doc)
}

fn take(doc: &FileDoc, id: &str) -> Result<Curve, String> {
    let body = doc
        .curves
        .get(id)
        .ok_or_else(|| format!("目标曲线不存在: {id}"))?;
    check_points(id, &body.points)?;
    Ok(Curve {
        id: id.into(),
        points: body.points.clone(),
    })
}

fn check_points(id: &str, points: &[(f64, f64)]) -> Result<(), String> {
    if points.len() < 2 {
        return Err(format!("曲线 {id} 至少需要两个点"));
    }
    for w in points.windows(2) {
        if !(w[1].0 > w[0].0) {
            return Err(format!("曲线 {id} 的频率必须严格递增"));
        }
        if !w[0].1.is_finite() || !w[1].1.is_finite() {
            return Err(format!("曲线 {id} 的 dB 必须是有限数"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_is_zero() {
        let c = resolve(None).unwrap();
        assert!((c.db_at(50.0)).abs() < 1e-9);
        assert!((c.db_at(10_000.0)).abs() < 1e-9);
    }

    #[test]
    fn log_interp_and_hold() {
        let c = Curve {
            id: "t".into(),
            points: vec![(100.0, 0.0), (10_000.0, -2.0)],
        };
        let mid = c.db_at(1000.0);
        assert!((mid + 1.0).abs() < 1e-9, "mid={mid}");
        assert!((c.db_at(10.0) - 0.0).abs() < 1e-12);
        assert!((c.db_at(30_000.0) + 2.0).abs() < 1e-12);
    }

    #[test]
    fn tilt10_preset_loads() {
        let c = resolve(Some("tilt10")).unwrap();
        assert!(c.db_at(20.0).abs() < 1e-9, "{} @20Hz", c.db_at(20.0));
        assert!((c.db_at(20_000.0) + 10.0).abs() < 1e-9);
        assert!(
            (c.db_at(1000.0) + 5.663).abs() < 0.01,
            "1k={}",
            c.db_at(1000.0)
        );
        // log-f 常数斜率：每十倍频程 -1.0034 dB/oct * 3.3219 oct ≈ -3.334 dB
        let slope = (c.db_at(20_000.0) - c.db_at(20.0)) / 1000.0f64.log2();
        assert!((slope + 1.0034).abs() < 0.01, "slope={slope}");
    }

    #[test]
    fn old_tilt_is_removed() {
        assert!(
            resolve(Some("tilt")).is_err(),
            "旧 -3dB@16k tilt 必须 superseded"
        );
    }
}
