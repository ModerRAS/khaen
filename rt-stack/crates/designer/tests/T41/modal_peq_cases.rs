// T41 私有测试：10 段模态 PEQ（IIR 允许、shelf S/Q、coeff 导出/原图比对、IIR 不硬截 FIR）。
//
// 本文件由 crates/designer/src/modal_peq.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （见 tests/T41/README.md；不改 integrator 独占的 public_registration.rs）。
// 运行：cargo test -p tspi-fir-designer modal_peq

// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn approx(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[track_caller]
fn assert_approx(a: f64, b: f64, tol: f64, what: &str) {
    assert!(approx(a, b, tol), "{what}: {a} vs {b} (tol {tol})");
}

fn expect_code<T: std::fmt::Debug>(result: Result<T, PeqError>, code: &str) {
    match result {
        Ok(v) => panic!("期望错误 {code}，实际成功 {v:?}"),
        Err(e) => assert_eq!(e.code, code, "错误码不符：{}", e.message),
    }
}

fn flat_curve(lo: f64, hi: f64, points: usize, db: f64) -> Vec<CurvePoint> {
    log_frequency_grid(lo, hi, (hi / lo).log2() / (points.max(2) - 1) as f64)
        .into_iter()
        .map(|f| CurvePoint::new(f, db))
        .collect()
}

/// 确定性 LCG 均匀噪声（±amp）。
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(
            seed.wrapping_mul(2862933555777941757)
                .wrapping_add(3037000493),
        )
    }
    fn next_unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn next_bipolar(&mut self, amp: f64) -> f64 {
        (self.next_unit() * 2.0 - 1.0) * amp
    }
}

/// 合成频响：宽带倾斜（tilt10）+ 多个 RBJ peaking 正增益（模拟房间模态峰）+ 噪声。
fn synth_modal_curve(
    fs: f64,
    modes: &[(f64, f64, f64)],
    use_tilt: bool,
    noise_db: f64,
    points: usize,
    lo: f64,
    hi: f64,
    seed: u64,
) -> Vec<CurvePoint> {
    let grid = log_frequency_grid(lo, hi, (hi / lo).log2() / (points.max(2) - 1) as f64);
    let bands: Vec<Biquad> = modes
        .iter()
        .map(|&(f, q, g)| Biquad::from_params(BandType::Peaking, f, q, g, fs).unwrap())
        .collect();
    let mut rng = Lcg::new(seed);
    grid.into_iter()
        .map(|f| {
            let mut db = if use_tilt { tilt10_db(f) } else { 0.0 };
            for band in &bands {
                db += band.magnitude_db(f, fs);
            }
            db += rng.next_bipolar(noise_db);
            CurvePoint::new(f, db)
        })
        .collect()
}

fn peaking_band(f: f64, q: f64, g: f64) -> ModalBand {
    ModalBand::from_params(BandType::Peaking, f, q, g, 48000.0).unwrap()
}

fn channel_with(name: &str, bands: Vec<ModalBand>) -> ChannelPeq {
    let mut ch = ChannelPeq::new(name, 0).unwrap();
    for b in bands {
        ch.push_band(b);
    }
    ch
}

// ---------------------------------------------------------------------------
// 极简 JSON 解析（只用于校验 to_json 输出；无第三方依赖）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("不是字符串: {self:?}"),
        }
    }
    fn as_f64(&self) -> f64 {
        match self {
            Json::Num(n) => *n,
            _ => panic!("不是数字: {self:?}"),
        }
    }
    fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => panic!("不是数组: {self:?}"),
        }
    }
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
        }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            Some(' ') | Some('\n') | Some('\r') | Some('\t')
        ) {
            self.pos += 1;
        }
    }
    fn parse(mut self) -> Result<Json, String> {
        self.skip_ws();
        let v = self.value()?;
        self.skip_ws();
        if self.pos != self.chars.len() {
            return Err(format!("尾随字符 @{}", self.pos));
        }
        Ok(v)
    }
    fn value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek().ok_or("意外结束")? {
            '{' => self.object(),
            '[' => self.array(),
            '"' => Ok(Json::Str(self.string()?)),
            't' => {
                self.expect_word("true")?;
                Ok(Json::Bool(true))
            }
            'f' => {
                self.expect_word("false")?;
                Ok(Json::Bool(false))
            }
            'n' => {
                self.expect_word("null")?;
                Ok(Json::Null)
            }
            _ => self.number(),
        }
    }
    fn expect_word(&mut self, word: &str) -> Result<(), String> {
        for expect in word.chars() {
            if self.bump() != Some(expect) {
                return Err(format!("期望 {word}"));
            }
        }
        Ok(())
    }
    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let token: String = self.chars[start..self.pos].iter().collect();
        token
            .parse::<f64>()
            .map(Json::Num)
            .map_err(|e| format!("坏数字 {token:?}: {e}"))
    }
    fn string(&mut self) -> Result<String, String> {
        if self.bump() != Some('"') {
            return Err("期望字符串".into());
        }
        let mut out = String::new();
        loop {
            match self.bump().ok_or("字符串未闭合")? {
                '"' => return Ok(out),
                '\\' => match self.bump().ok_or("转义未闭合")? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000c}'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let c = self.bump().ok_or("\\u 未闭合")?;
                            code =
                                code * 16 + c.to_digit(16).ok_or_else(|| format!("坏 hex {c}"))?;
                        }
                        out.push(char::from_u32(code).ok_or("坏 unicode")?);
                    }
                    other => return Err(format!("未知转义 \\{other}")),
                },
                c => out.push(c),
            }
        }
    }
    fn array(&mut self) -> Result<Json, String> {
        self.bump();
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some(']') => return Ok(Json::Arr(items)),
                other => return Err(format!("数组分隔符错误 {other:?}")),
            }
        }
    }
    fn object(&mut self) -> Result<Json, String> {
        self.bump();
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Json::Obj(entries));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            if self.bump() != Some(':') {
                return Err("缺少冒号".into());
            }
            let value = self.value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => return Ok(Json::Obj(entries)),
                other => return Err(format!("对象分隔符错误 {other:?}")),
            }
        }
    }
}

fn parse_json(text: &str) -> Json {
    JsonParser::new(text)
        .parse()
        .unwrap_or_else(|e| panic!("JSON 解析失败: {e}\n{text}"))
}

fn biquad_from_json(band: &Json) -> Biquad {
    let b: Vec<f64> = band
        .get("b")
        .unwrap()
        .as_arr()
        .iter()
        .map(|v| v.as_f64())
        .collect();
    let a: Vec<f64> = band
        .get("a")
        .unwrap()
        .as_arr()
        .iter()
        .map(|v| v.as_f64())
        .collect();
    Biquad {
        b: [b[0], b[1], b[2]],
        a: [a[0], a[1], a[2]],
    }
}

fn wrap_deg(d: f64) -> f64 {
    let mut x = d % 360.0;
    if x > 180.0 {
        x -= 360.0;
    }
    if x < -180.0 {
        x += 360.0;
    }
    x
}

// ---------------------------------------------------------------------------
// V5/V6/V7：RBJ 解析不变量
// ---------------------------------------------------------------------------

#[test]
fn t41_peaking_invariants_v5() {
    let cases = [(1000.0_f64, 1.0, 6.0), (80.0, 1.2, -4.5)];
    for (f0, q, g) in cases {
        let bq = Biquad::from_params(BandType::Peaking, f0, q, g, 48000.0).unwrap();
        assert_approx(
            bq.magnitude_db(f0, 48000.0),
            g,
            1e-9,
            "peaking |H(f0)|=gain",
        );
    }
    // gain=0 → b=a 恒等（V5）
    let bq = Biquad::from_params(BandType::Peaking, 1000.0, 4.0, 0.0, 48000.0).unwrap();
    assert_eq!(bq.b, bq.a, "gain=0 应 b=a");
    for f in [20.0, 100.0, 1000.0, 10000.0, 20000.0] {
        assert_approx(bq.magnitude_db(f, 48000.0), 0.0, 1e-12, "gain=0 恒等响应");
    }
}

#[test]
fn t41_shelf_invariants_v6() {
    // 100 Hz / −3 dB / S=0.7 low shelf：f0 半增益，DC=g，Nyquist=0
    let low = Biquad::from_params(BandType::LowShelf, 100.0, 0.7, -3.0, 48000.0).unwrap();
    assert_approx(
        low.magnitude_db(100.0, 48000.0),
        -1.5,
        1e-9,
        "low_shelf f0 半增益",
    );
    assert_approx(low.dc_gain_db(), -3.0, 1e-9, "low_shelf DC=g");
    assert_approx(low.nyquist_gain_db(), 0.0, 1e-9, "low_shelf Nyquist=0");

    // 5 kHz / +2 dB / S=0.5 high shelf
    let high = Biquad::from_params(BandType::HighShelf, 5000.0, 0.5, 2.0, 48000.0).unwrap();
    assert_approx(
        high.magnitude_db(5000.0, 48000.0),
        1.0,
        1e-9,
        "high_shelf f0 半增益",
    );
    assert_approx(high.dc_gain_db(), 0.0, 1e-9, "high_shelf DC=0");
    assert_approx(high.nyquist_gain_db(), 2.0, 1e-9, "high_shelf Nyquist=g");

    // 80 Hz / −6 dB / S=1.0 low shelf
    let low1 = Biquad::from_params(BandType::LowShelf, 80.0, 1.0, -6.0, 48000.0).unwrap();
    assert_approx(
        low1.magnitude_db(80.0, 48000.0),
        -3.0,
        1e-9,
        "S=1 f0 半增益",
    );
    assert_approx(low1.dc_gain_db(), -6.0, 1e-9, "S=1 DC=g");
    assert_approx(low1.nyquist_gain_db(), 0.0, 1e-9, "S=1 Nyq=0");
    // S=1 时 alpha/sin(w0) = sqrt(2)/2 = 0.70710678
    let w0 = 2.0 * std::f64::consts::PI * 80.0 / 48000.0;
    let a_lin = rbj_a(-6.0);
    let ratio = shelf_alpha(w0, a_lin, 1.0) / w0.sin();
    assert_approx(ratio, 0.70710678, 1e-8, "S=1 alpha/sin");
}

#[test]
fn t41_shelf_slope_semantics() {
    // CONTRACT v1.1：shelf 的 q 字段是 RBJ slope S；CamillaDSP 映射 slope=12*S
    for s in [0.1, 0.5, 0.7, 1.0] {
        let band = ModalBand::from_params(BandType::LowShelf, 100.0, s, -3.0, 48000.0).unwrap();
        let slope_db_per_oct = 12.0 * band.q;
        assert!(slope_db_per_oct <= 12.0 + 1e-12, "slope 必须 ≤12 dB/oct");
        assert_approx(band.q, s, 0.0, "shelf q 字段必须原样是 S");
    }
    // 若把 S 当 Q 用（错误做法），f0 处不会得到半增益 → 语义不同，禁止混用
    let band = ModalBand::from_params(BandType::LowShelf, 100.0, 0.7, -3.0, 48000.0).unwrap();
    assert_approx(
        band.correction_at_center_db(48000.0),
        -1.5,
        1e-9,
        "shelf f0 修正=半增益",
    );
}

#[test]
fn t41_peaking_bandwidth_informative_v7() {
    fn half_gain_ratio(f0: f64, q: f64, gain_db: f64) -> f64 {
        let fs = 48000.0;
        let bq = Biquad::from_params(BandType::Peaking, f0, q, gain_db, fs).unwrap();
        let half = gain_db / 2.0;
        let mut lo = f0 / 2.0;
        let mut hi = f0;
        for _ in 0..200 {
            let mid = (lo * hi).sqrt();
            if bq.magnitude_db(mid, fs) > half {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let f1 = (lo * hi).sqrt();
        let mut lo2 = f0;
        let mut hi2 = f0 * 2.0;
        for _ in 0..200 {
            let mid = (lo2 * hi2).sqrt();
            if bq.magnitude_db(mid, fs) > half {
                lo2 = mid;
            } else {
                hi2 = mid;
            }
        }
        let f2 = (lo2 * hi2).sqrt();
        f0 / (f2 - f1)
    }
    // V7：f0=40,g=+12,Q=8 → 8.00004；f0=1k,g=+6,Q=1 → 1.00428（±2%，信息性）
    assert_approx(half_gain_ratio(40.0, 8.0, 12.0), 8.00004, 0.16, "V7 f0=40");
    assert_approx(
        half_gain_ratio(1000.0, 1.0, 6.0),
        1.00428,
        0.0201,
        "V7 f0=1k",
    );
}

// ---------------------------------------------------------------------------
// 类型/界限/契约
// ---------------------------------------------------------------------------

#[test]
fn t41_band_type_parse_rejects_unknown() {
    use BandType::*;
    assert_eq!(BandType::parse("peaking").unwrap(), Peaking);
    assert_eq!(BandType::parse("low_shelf").unwrap(), LowShelf);
    assert_eq!(BandType::parse("high_shelf").unwrap(), HighShelf);
    assert_eq!(BandType::parse_optional(None).unwrap(), Peaking);
    assert_eq!(BandType::parse_optional(Some("")).unwrap(), Peaking);
    // 未知 type 整份拒绝，不降级（spec §5.2）
    expect_code(BandType::parse("notch"), E_PEQ_TYPE);
    expect_code(BandType::parse("Peaking"), E_PEQ_TYPE);
    expect_code(BandType::parse_optional(Some("peak")), E_PEQ_TYPE);
    assert!(!Peaking.is_shelf());
    assert!(LowShelf.is_shelf());
    assert!(HighShelf.is_shelf());
}

#[test]
fn t41_numeric_bounds_reject() {
    expect_code(
        Biquad::from_params(BandType::Peaking, 19.9, 5.0, 0.0, 48000.0),
        E_PEQ_FREQ,
    );
    expect_code(
        Biquad::from_params(BandType::Peaking, 20001.0, 5.0, 0.0, 48000.0),
        E_PEQ_FREQ,
    );
    expect_code(
        Biquad::from_params(BandType::Peaking, 24000.0, 5.0, 0.0, 48000.0),
        E_PEQ_FREQ,
    );
    expect_code(
        Biquad::from_params(BandType::Peaking, 100.0, 5.0, 24.5, 48000.0),
        E_PEQ_GAIN,
    );
    expect_code(
        Biquad::from_params(BandType::Peaking, 100.0, 0.05, -1.0, 48000.0),
        E_PEQ_Q,
    );
    expect_code(
        Biquad::from_params(BandType::LowShelf, 100.0, 1.5, -1.0, 48000.0),
        E_PEQ_Q,
    );
    expect_code(
        Biquad::from_params(BandType::Peaking, f64::NAN, 5.0, 0.0, 48000.0),
        E_PEQ_NUMERIC,
    );
    expect_code(
        Biquad::from_params(BandType::Peaking, 100.0, 5.0, -1.0, 0.0),
        E_PEQ_FS,
    );
    expect_code(validate_sample_rate(32000), E_PEQ_FS);
}

#[test]
fn t41_fs_and_channel_name_validation() {
    for fs in [44100u32, 48000, 88200, 96000] {
        assert!(validate_sample_rate(fs).is_ok(), "fs {fs} 应合法");
        assert!(ChannelPeq::new("FL", 0).is_ok());
    }
    expect_code(validate_sample_rate(32000), E_PEQ_FS);
    expect_code(ChannelPeq::new("fl", 0), E_PEQ_NAME);
    expect_code(ChannelPeq::new("", 0), E_PEQ_NAME);
    expect_code(ChannelPeq::new("1FL", 0), E_PEQ_NAME);
    expect_code(ChannelPeq::new("F-L", 0), E_PEQ_NAME);
    // 合法角色名（CONTRACT ^[A-Z][A-Z0-9]*$）
    for name in ["FL", "FR", "SUB1", "HFL", "C"] {
        assert!(ChannelPeq::new(name, 0).is_ok(), "{name} 应合法");
    }
}

// ---------------------------------------------------------------------------
// 预测/f32/极点（§5.4、V15）
// ---------------------------------------------------------------------------

#[test]
fn t41_prediction_dc_nyquist_and_stability() {
    let mut ch = ChannelPeq::new("FL", 0).unwrap();
    ch.push_band(ModalBand::from_params(BandType::LowShelf, 100.0, 0.7, -3.0, 48000.0).unwrap());
    ch.push_band(ModalBand::from_params(BandType::HighShelf, 8000.0, 0.5, 2.0, 48000.0).unwrap());
    let pred = ch.prediction(48000.0);
    assert_approx(pred.dc_gain_db, -3.0, 1e-9, "级联 DC");
    assert_approx(pred.nyquist_gain_db, 2.0, 1e-9, "级联 Nyquist");
    assert!(
        pred.poles_radius_max <= POLE_RADIUS_MAX,
        "极点半径必须 ≤0.999999"
    );
    assert!(pred.max_a1 >= 0.0);
    assert!(ch.is_stable());
    // peaking 的 DC/Nyquist 恒 0
    let peaking = peaking_band(80.0, 12.0, -6.0);
    let pred = channel_with("FR", vec![peaking]).prediction(48000.0);
    assert_approx(pred.dc_gain_db, 0.0, 1e-9, "peaking DC=0");
    assert_approx(pred.nyquist_gain_db, 0.0, 1e-9, "peaking Nyq=0");
}

#[test]
fn t41_pole_radius_v15() {
    // V15：30 Hz / −12 dB / Q=20 → 极点半径 0.999804
    let band = ModalBand::from_params(BandType::Peaking, 30.0, 20.0, -12.0, 48000.0).unwrap();
    assert_approx(
        band.biquad().max_pole_radius(),
        0.999804,
        5e-6,
        "V15 极点半径",
    );
    assert!(band.biquad().is_stable());
}

#[test]
fn t41_f32_requires_f64_v15() {
    // V15 固定网格（2001 点）f32 量化后最大偏差：30 Hz/−12/Q20 → 0.19646（>0.1 → 必须 f64）
    // 40 Hz/−10/Q16 → 0.03974（f32 可）；25 Hz/−8/Q12 → 0.12813（>0.1）。容差 0.01 dB（本实现独立复算）。
    let cases = [
        (30.0_f64, 20.0, -12.0, 0.19646, true),
        (40.0, 16.0, -10.0, 0.03974, false),
        (25.0, 12.0, -8.0, 0.12813, true),
    ];
    for (f0, q, g, expected, requires) in cases {
        let ch = channel_with("FL", vec![peaking_band(f0, q, g)]);
        let dev = ch.f32_quantization_max_deviation_db(48000.0);
        println!("V15 f0={f0} q={q} g={g}: f32 max dev = {dev:.5} dB (spec {expected})");
        assert_approx(dev, expected, 0.02, "V15 f32 量化偏差");
        assert_eq!(
            ch.prediction(48000.0).requires_f64,
            requires,
            "requires_f64 @ f0={f0}"
        );
        assert_eq!(dev > F32_QUANTIZATION_TOLERANCE_DB, requires);
    }
}

// ---------------------------------------------------------------------------
// 导出 schema / 确定性 / 自洽（§5.5、§5.6.1）
// ---------------------------------------------------------------------------

fn sample_design() -> PeqDesign {
    let mut design = PeqDesign::new(48000, DesignMode::ModalPlusWide).unwrap();
    design.created_unix = 1_700_000_000;
    design.allow_boost = true;
    design.source.target_sha256 = Some("a".repeat(64));
    design.source.curve_id = Some("flat".to_string());
    let mut ch = ChannelPeq::new("FL", 0).unwrap();
    ch.push_band(peaking_band(60.0, 8.0, -5.0));
    ch.push_band(ModalBand::from_params(BandType::LowShelf, 120.0, 0.7, -3.0, 48000.0).unwrap());
    ch.push_band(ModalBand::from_params(BandType::HighShelf, 8000.0, 0.5, 2.0, 48000.0).unwrap());
    design.add_channel(ch).unwrap();
    design
}

#[test]
fn t41_export_schema_and_determinism() {
    let design = sample_design();
    let json = design.to_json();
    assert_eq!(json, design.to_json(), "to_json 必须确定性");
    let v = parse_json(&json);
    assert_eq!(v.get("schema").unwrap().as_str(), MODAL_PEQ_SCHEMA);
    assert_eq!(v.get("fs").unwrap().as_f64(), 48000.0);
    assert_eq!(v.get("created_unix").unwrap().as_f64(), 1_700_000_000.0);
    let tool = v.get("tool").unwrap();
    assert_eq!(tool.get("name").unwrap().as_str(), "tspi-fir-designer");
    assert!(!tool.get("version").unwrap().as_str().is_empty());
    let channels = v.get("channels").unwrap().as_arr();
    assert_eq!(channels.len(), 1);
    let ch = &channels[0];
    assert_eq!(ch.get("name").unwrap().as_str(), "FL");
    assert_eq!(ch.get("index").unwrap().as_f64(), 0.0);
    let bands = ch.get("bands").unwrap().as_arr();
    assert_eq!(bands.len(), 3);
    assert_eq!(bands[0].get("type").unwrap().as_str(), "peaking");
    assert_eq!(bands[1].get("type").unwrap().as_str(), "low_shelf");
    assert_eq!(bands[2].get("type").unwrap().as_str(), "high_shelf");
    // shelf 的 q 字段在导出中仍是 S（不是 Q）
    assert_approx(
        bands[1].get("q").unwrap().as_f64(),
        0.7,
        1e-15,
        "low_shelf 导出 q=S",
    );
    assert_approx(
        bands[2].get("q").unwrap().as_f64(),
        0.5,
        1e-15,
        "high_shelf 导出 q=S",
    );
    let predicted = ch.get("predicted").unwrap();
    for key in [
        "dc_gain_db",
        "nyquist_gain_db",
        "poles_radius_max",
        "requires_f64",
    ] {
        assert!(predicted.get(key).is_some(), "predicted 缺 {key}");
    }
    let design_block = v.get("design").unwrap();
    assert_eq!(
        design_block.get("mode").unwrap().as_str(),
        "modal_plus_wide"
    );
    assert_eq!(design_block.get("allow_boost").unwrap(), &Json::Bool(true));
    assert_eq!(design_block.get("band_hz").unwrap().as_arr().len(), 2);
    assert!(v.get("warnings").unwrap().as_arr().is_empty());
    // 数字至少 17 位有效数字（spec §5.5）；本实现用 {:.17e}（18 位）
    for num in [design.to_json().find("\"b\":").unwrap()] {
        let _ = num;
    }
    for band in bands {
        for key in ["freq", "q", "gain_db"] {
            let text = format!("{:?}", band.get(key).unwrap());
            assert!(text.contains('.'), "数字应为浮点: {text}");
        }
        for arr_key in ["b", "a"] {
            for value in band.get(arr_key).unwrap().as_arr() {
                if let Json::Num(n) = value {
                    let token = json_number(*n);
                    let mantissa = token
                        .split('e')
                        .next()
                        .unwrap()
                        .trim_start_matches('-')
                        .replace('.', "");
                    assert!(mantissa.len() >= 17, "有效数字不足 17：{token}");
                }
            }
        }
    }
    // source 字段
    let source = v.get("source").unwrap();
    assert_eq!(source.get("curve_id").unwrap().as_str(), "flat");
    assert_eq!(
        source.get("target_sha256").unwrap().as_str(),
        "a".repeat(64).as_str()
    );
}

#[test]
fn t41_self_consistency_export_vs_fields() {
    // §5.6.1：由 band 字段解析计算的级联响应 vs 由导出 b/a 系数计算的响应，
    // 20 Hz–20 kHz、1/48 oct 网格：差 ≤0.1 dB / 1°。
    let design = sample_design();
    let json = design.to_json();
    let v = parse_json(&json);
    let channel = &design.channels[0];
    let bands = v.get("channels").unwrap().as_arr()[0]
        .get("bands")
        .unwrap()
        .as_arr();
    assert_eq!(bands.len(), channel.bands.len());
    let grid = log_frequency_grid(20.0, 20000.0, SELF_CONSISTENCY_OCTAVE_RES);
    let mut max_db = 0.0_f64;
    let mut max_phase = 0.0_f64;
    for &f in &grid {
        let (mut re_f, mut im_f) = (1.0_f64, 0.0_f64);
        let (mut re_e, mut im_e) = (1.0_f64, 0.0_f64);
        for (band, band_json) in channel.bands.iter().zip(bands) {
            let (r, i) = band.biquad().complex_response(f, 48000.0);
            let nr = re_f * r - im_f * i;
            let ni = re_f * i + im_f * r;
            re_f = nr;
            im_f = ni;
            let (r, i) = biquad_from_json(band_json).complex_response(f, 48000.0);
            let nr = re_e * r - im_e * i;
            let ni = re_e * i + im_e * r;
            re_e = nr;
            im_e = ni;
            // 导出参数与字段一致
            assert_approx(
                band_json.get("freq").unwrap().as_f64(),
                band.freq_hz,
                1e-12,
                "导出 freq",
            );
            assert_approx(
                band_json.get("q").unwrap().as_f64(),
                band.q,
                1e-12,
                "导出 q/S",
            );
            assert_approx(
                band_json.get("gain_db").unwrap().as_f64(),
                band.gain_db,
                1e-12,
                "导出 gain",
            );
        }
        let db_f = 20.0 * re_f.hypot(im_f).log10();
        let db_e = 20.0 * re_e.hypot(im_e).log10();
        max_db = max_db.max((db_f - db_e).abs());
        let ph_f = im_f.atan2(re_f).to_degrees();
        let ph_e = im_e.atan2(re_e).to_degrees();
        max_phase = max_phase.max(wrap_deg(ph_f - ph_e).abs());
    }
    println!(
        "self-consistency: max |dB diff| = {max_db:.3e}, max |phase diff| = {max_phase:.3e} deg"
    );
    assert!(max_db <= 0.1, "自洽 dB 差 {max_db} > 0.1");
    assert!(max_phase <= 1.0, "自洽相位差 {max_phase} > 1°");
}

// ---------------------------------------------------------------------------
// 段数/只切/boost 策略（§5.2）
// ---------------------------------------------------------------------------

#[test]
fn t41_limits_modal_peakings_and_shelves() {
    let fs = 48000u32;
    let limits = PeqLimits::producer(DesignMode::ModalOnly, false, [20.0, 250.0]);
    let mut ch = ChannelPeq::new("FL", 0).unwrap();
    for i in 0..11 {
        ch.push_band(peaking_band(20.0 + i as f64 * 10.0, 5.0, -1.0));
    }
    expect_code(ch.validate(fs, &limits), E_PEQ_LIMIT);

    let mut ch = ChannelPeq::new("FL", 0).unwrap();
    ch.push_band(ModalBand::from_params(BandType::LowShelf, 60.0, 0.7, -2.0, 48000.0).unwrap());
    ch.push_band(ModalBand::from_params(BandType::LowShelf, 80.0, 0.7, -2.0, 48000.0).unwrap());
    expect_code(ch.validate(fs, &limits), E_PEQ_LIMIT);

    // 消费端：不假设 shelf 段数，已知 type 任意段，总数 ≤32
    let consumer = PeqLimits::consumer();
    assert!(ch.validate(fs, &consumer).is_ok(), "消费端可接受多段 shelf");
    let mut too_many = ChannelPeq::new("FL", 0).unwrap();
    for i in 0..33 {
        too_many.push_band(peaking_band(20.0 + i as f64 * 5.0, 5.0, -0.5));
    }
    expect_code(too_many.validate(fs, &consumer), E_PEQ_LIMIT);
}

#[test]
fn t41_modal_only_cut_policy() {
    let fs = 48000u32;
    let limits = PeqLimits::producer(DesignMode::ModalOnly, false, [20.0, 250.0]);
    // 模态带内 peaking 正增益 → 只切策略拒绝（allow_boost 对模态段无效）
    let ch = channel_with("FL", vec![peaking_band(80.0, 8.0, 1.0)]);
    expect_code(ch.validate(fs, &limits), E_PEQ_BOOST);
    let limits_boost = PeqLimits::producer(DesignMode::ModalOnly, true, [20.0, 250.0]);
    expect_code(ch.validate(fs, &limits_boost), E_PEQ_BOOST);
    // 负增益合法
    let ch = channel_with(
        "FL",
        vec![
            peaking_band(80.0, 8.0, -5.0),
            peaking_band(120.0, 6.0, -3.0),
        ],
    );
    assert!(ch.validate(fs, &limits).is_ok());
    // 模态带内 shelf 负增益（宽带只切）合法
    let ch = channel_with(
        "FL",
        vec![ModalBand::from_params(BandType::LowShelf, 120.0, 0.7, -3.0, 48000.0).unwrap()],
    );
    assert!(ch.validate(fs, &limits).is_ok());
}

#[test]
fn t41_wideband_boost_policy() {
    let fs = 48000u32;
    // 非模态 peaking 正增益：默认 cap 0 → 拒绝
    let ch = channel_with("FL", vec![peaking_band(1000.0, 1.0, 2.0)]);
    let limits = PeqLimits::producer(DesignMode::ModalPlusWide, false, [20.0, 250.0]);
    expect_code(ch.validate(fs, &limits), E_PEQ_BOOST);
    // allow_boost=true：+2 dB 合法（≤+3）
    let limits_boost = PeqLimits::producer(DesignMode::ModalPlusWide, true, [20.0, 250.0]);
    assert!(ch.validate(fs, &limits_boost).is_ok());
    // 总量 >+3 → 拒绝
    let ch2 = channel_with(
        "FL",
        vec![
            peaking_band(1000.0, 1.0, 2.0),
            peaking_band(2000.0, 1.0, 2.0),
        ],
    );
    expect_code(ch2.validate(fs, &limits_boost), E_PEQ_BOOST);
    // shelf 正增益也计入宽带总量
    let ch3 = channel_with(
        "FL",
        vec![ModalBand::from_params(BandType::LowShelf, 60.0, 0.7, 2.0, 48000.0).unwrap()],
    );
    expect_code(ch3.validate(fs, &limits), E_PEQ_BOOST);
    // 模态带内 peaking 正增益与 allow_boost 无关
    let ch4 = channel_with("FL", vec![peaking_band(80.0, 8.0, 1.0)]);
    expect_code(ch4.validate(fs, &limits_boost), E_PEQ_BOOST);
}

#[test]
fn t41_design_validation() {
    expect_code(PeqDesign::new(12345, DesignMode::ModalOnly), E_PEQ_FS);
    let mut design = PeqDesign::new(48000, DesignMode::ModalOnly).unwrap();
    expect_code(design.validate().map(|_| ()), E_PEQ_CHANNELS);
    design
        .add_channel(channel_with("FL", vec![peaking_band(80.0, 8.0, -5.0)]))
        .unwrap();
    expect_code(
        design.add_channel(channel_with("FL", vec![])),
        E_PEQ_CHANNELS,
    );
    let mut dup_index = channel_with("FR", vec![]);
    dup_index.index = 0;
    expect_code(design.add_channel(dup_index), E_PEQ_CHANNELS);
    design.band_hz = [30.0, 20.0];
    expect_code(design.validate().map(|_| ()), E_PEQ_BAND);
    design.band_hz = [20.0, 250.0];
    let warnings = design.validate().unwrap();
    assert!(warnings.is_empty(), "干净设计不应有 warning: {warnings:?}");
    design.allow_boost = true;
    let warnings = design.validate().unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("allow_boost")),
        "allow_boost 应产生 warning"
    );
}

// ---------------------------------------------------------------------------
// IIR 不硬截 FIR（§5.7）
// ---------------------------------------------------------------------------

#[test]
fn t41_fir_tail_required_len_examples() {
    // spec §5.7 例子：r=0.999 → 11508 samples（239.75 ms @48k）；r=0.9995 → 23021
    assert_eq!(fir_required_len(0.999), 11508);
    assert_eq!(fir_required_len(0.9995), 23021);
    assert_eq!(fir_required_len(0.5), 17); // ceil(log(1e-5)/log(0.5)) = ceil(16.61) = 17
    assert_eq!(fir_required_len(0.0), 1);
    assert_eq!(fir_required_len(1.0), usize::MAX);
}

#[test]
fn t41_fir_tail_rejects_extreme_modal_band() {
    // 30 Hz/−12/Q20（V15）极点半径 0.999804：4096 常规 FIR 远不够，必须拒绝并给出所需长度
    let ch = channel_with(
        "FL",
        vec![ModalBand::from_params(BandType::Peaking, 30.0, 20.0, -12.0, 48000.0).unwrap()],
    );
    let report = ch.fir_tail_report(4096).unwrap();
    assert!(!report.passed, "极限模态段必须不能塞进 4096");
    assert!(
        report.required_len > 40000,
        "required_len={}",
        report.required_len
    );
    assert!(report.tail_peak_db > FIR_TAIL_PEAK_LIMIT_DB);
    let err = ch.check_iir_to_fir(4096).unwrap_err();
    assert_eq!(err.code, E_PEQ_FIR_TAIL);
    assert!(
        err.message.contains("required_len"),
        "错误必须给出所需长度估计: {}",
        err.message
    );
    println!("E_PEQ_FIR_TAIL: {}", err.message);
}

#[test]
fn t41_fir_tail_mild_band_passes() {
    // 1 kHz/−6/Q1 的温和段：4096 taps 尾长满足 → 允许 FIR 近似
    let ch = channel_with("FL", vec![peaking_band(1000.0, 1.0, -6.0)]);
    let report = ch.check_iir_to_fir(4096).unwrap();
    assert!(report.passed);
    assert!(report.tail_peak_db <= FIR_TAIL_PEAK_LIMIT_DB);
    assert!(report.tail_energy_db <= FIR_TAIL_ENERGY_LIMIT_DB);
    // 级联含极限段时整体拒绝，required_len 取最大
    let mixed = channel_with(
        "FL",
        vec![
            peaking_band(1000.0, 1.0, -6.0),
            ModalBand::from_params(BandType::Peaking, 30.0, 20.0, -12.0, 48000.0).unwrap(),
        ],
    );
    let err = mixed.check_iir_to_fir(4096).unwrap_err();
    assert_eq!(err.code, E_PEQ_FIR_TAIL);
    assert!(
        err.message.contains("第 1 段"),
        "应指出最慢段: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// 原图比对（§5.6：2a/2b 分开）
// ---------------------------------------------------------------------------

fn flat_target(lo: f64, hi: f64) -> Vec<CurvePoint> {
    flat_curve(lo, hi, 800, 0.0)
}

#[test]
fn t41_compare_flat_original_passes() {
    let original = flat_curve(20.0, 20000.0, 1000, 0.0);
    let target = flat_target(20.0, 20000.0);
    let ch = ChannelPeq::new("FL", 0).unwrap();
    let cmp = compare_design_to_original(&ch, 48000, &original, &target, [20.0, 250.0]).unwrap();
    assert!(cmp.rule_2a.passed);
    assert!(cmp.rule_2b.passed);
    assert!(cmp.overall_pass());
    assert!(cmp.eval_grid_points > 400);
    assert_approx(
        cmp.modal_band_residual.rms_after_db,
        0.0,
        1e-12,
        "空设计残余=0",
    );
    assert!(cmp.band_corrections.is_empty());
    assert_approx(
        cmp.rule_2b.raw_max_deviation_db_1_24_oct,
        0.0,
        1e-12,
        "raw 1/24",
    );
}

#[test]
fn t41_compare_detected_design_improves_and_passes() {
    let fs = 48000.0;
    let curve = synth_modal_curve(fs, &[(80.0, 8.0, 6.0)], false, 0.0, 1200, 20.0, 2000.0, 7);
    let mut cfg = ModalDetectConfig::default();
    cfg.band_hz = [20.0, 250.0];
    let ch = detect_modal_peq_channel("FL", 0, &curve, &cfg).unwrap();
    assert_eq!(ch.bands.len(), 1, "应检出 1 个模态");
    let target = flat_target(20.0, 20000.0);
    let cmp = compare_design_to_original(&ch, 48000, &curve, &target, [20.0, 250.0]).unwrap();
    assert_eq!(cmp.band_corrections.len(), 1);
    let bc = cmp.band_corrections[0];
    assert_approx(
        bc.applied_correction_db,
        bc.gain_db,
        1e-6,
        "peaking 中心修正=gain",
    );
    assert!(
        bc.original_deviation_db > 4.5,
        "修正前模态峰应高出目标: {bc:?}"
    );
    assert!(
        bc.residual_db.abs() < bc.original_deviation_db,
        "修正后残余应更小: {bc:?}"
    );
    assert!(
        cmp.modal_band_residual.rms_after_db < cmp.modal_band_residual.rms_before_db,
        "模态带残余应改善 before={} after={}",
        cmp.modal_band_residual.rms_before_db,
        cmp.modal_band_residual.rms_after_db
    );
    println!(
        "detected design vs original: 2a={:?} 2b ripple={:.3} raw24={:.3}",
        cmp.rule_2a,
        cmp.rule_2b.one_third_octave_ripple_db,
        cmp.rule_2b.raw_max_deviation_db_1_24_oct
    );
    assert!(
        cmp.rule_2a.passed,
        "单频模态修正应通过 2a: {:?}",
        cmp.rule_2a
    );
    assert!(
        cmp.rule_2b.passed,
        "单频模态修正应通过 2b: {:?}",
        cmp.rule_2b
    );
}

#[test]
fn t41_compare_overcut_fails_2a() {
    let original = synth_modal_curve(
        48000.0,
        &[(80.0, 8.0, 6.0)],
        false,
        0.0,
        1200,
        20.0,
        2000.0,
        1,
    );
    let target = flat_target(20.0, 20000.0);
    // 过切 −20 dB：相对修正前恶化 >3 dB → 2a FAIL（负向对照）
    let ch = channel_with("FL", vec![peaking_band(80.0, 8.0, -20.0)]);
    let cmp = compare_design_to_original(&ch, 48000, &original, &target, [20.0, 250.0]).unwrap();
    assert!(!cmp.rule_2a.passed, "过切必须 2a FAIL: {:?}", cmp.rule_2a);
    assert!(cmp.rule_2a.worst_worsening_db > 3.0, "{:?}", cmp.rule_2a);
}

#[test]
fn t41_compare_boost_band_fails_2a() {
    let original = flat_curve(20.0, 20000.0, 1000, 0.0);
    let target = flat_target(20.0, 20000.0);
    // 平直原始曲线上加正增益段 → 模态带内峰高出目标 >+3 dB → 2a FAIL
    let ch = channel_with("FL", vec![peaking_band(80.0, 8.0, 6.0)]);
    let cmp = compare_design_to_original(&ch, 48000, &original, &target, [20.0, 250.0]).unwrap();
    assert!(!cmp.rule_2a.passed);
    assert!(cmp.rule_2a.peak_over_target_db > 3.0, "{:?}", cmp.rule_2a);
}

#[test]
fn t41_compare_new_narrow_notch_fails_2b() {
    let original = flat_curve(20.0, 20000.0, 1000, 0.0);
    let target = flat_target(20.0, 20000.0);
    // 模态带外（1 kHz）加 −8 dB 窄缺口：2a 只看模态带 → 通过；2b 新增 >6 dB 窄带缺口 → FAIL
    let ch = channel_with("FL", vec![peaking_band(1000.0, 30.0, -8.0)]);
    let cmp = compare_design_to_original(&ch, 48000, &original, &target, [20.0, 250.0]).unwrap();
    assert!(cmp.rule_2a.passed, "模态带外不应触发 2a: {:?}", cmp.rule_2a);
    assert!(
        cmp.rule_2b.new_narrow_notch_db > 6.0,
        "应检出新增窄带缺口: {:?}",
        cmp.rule_2b
    );
    assert!(!cmp.rule_2b.passed);
}

#[test]
fn t41_compare_reports_raw_1_24_oct() {
    // 1/3 oct 平滑会掩盖窄事件：必须另报 1/24 oct 原始分辨率最大偏差（§5.6.2b 第 5 条）
    let mut original = flat_curve(20.0, 20000.0, 4000, 0.0);
    let spike = Biquad::from_params(BandType::Peaking, 1000.0, 15.0, 8.0, 48000.0).unwrap();
    for p in original.iter_mut() {
        p.mag_db += spike.magnitude_db(p.freq_hz, 48000.0);
    }
    let target = flat_target(20.0, 20000.0);
    let ch = ChannelPeq::new("FL", 0).unwrap();
    let cmp = compare_design_to_original(&ch, 48000, &original, &target, [20.0, 250.0]).unwrap();
    println!(
        "raw 1/24 oct max dev = {:.3} dB; 1/3 oct smoothed max dev = {:.3} dB",
        cmp.rule_2b.raw_max_deviation_db_1_24_oct, cmp.rule_2b.one_third_octave_max_deviation_db
    );
    assert!(
        cmp.rule_2b.raw_max_deviation_db_1_24_oct > 5.5,
        "raw 应看到窄峰"
    );
    assert!(
        cmp.rule_2b.raw_max_deviation_db_1_24_oct
            > cmp.rule_2b.one_third_octave_max_deviation_db + 1.5,
        "raw 必须显著高于 1/3 oct 平滑值"
    );
}

#[test]
fn t41_compare_partial_coverage_warns() {
    let original = flat_curve(20.0, 1000.0, 400, 0.0);
    let target = flat_target(20.0, 1000.0);
    let ch = ChannelPeq::new("FL", 0).unwrap();
    let cmp = compare_design_to_original(&ch, 48000, &original, &target, [20.0, 250.0]).unwrap();
    assert!(!cmp.warnings.is_empty(), "部分覆盖必须 warning");
    assert!(cmp.warnings.iter().any(|w| w.contains("未覆盖")));
}

#[test]
fn t41_compare_invalid_curve() {
    let unsorted = vec![CurvePoint::new(100.0, 0.0), CurvePoint::new(50.0, 0.0)];
    let target = flat_target(20.0, 20000.0);
    let ch = ChannelPeq::new("FL", 0).unwrap();
    expect_code(
        compare_design_to_original(&ch, 48000, &unsorted, &target, [20.0, 250.0]).map(|_| ()),
        E_PEQ_CURVE,
    );
    // 无交集（original 20–30 Hz vs target 1–2 kHz）
    let low = flat_curve(20.0, 30.0, 20, 0.0);
    let high = flat_curve(1000.0, 2000.0, 100, 0.0);
    expect_code(
        compare_design_to_original(&ch, 48000, &low, &high, [20.0, 250.0]).map(|_| ()),
        E_PEQ_CURVE,
    );
}

// ---------------------------------------------------------------------------
// 模态检测合成夹具（spec §5.6.4；requirements 5 AC1–AC6、需求 8 AC1）
// ---------------------------------------------------------------------------

fn fixture_modes() -> Vec<(f64, f64, f64)> {
    vec![
        (30.0, 8.0, 8.0),
        (42.0, 12.0, 10.0),
        (56.0, 9.0, 7.0),
        (76.0, 18.0, 9.0),
        (103.0, 14.0, 7.0),
        (138.0, 10.0, 6.0),
        (182.0, 15.0, 8.0),
        (228.0, 20.0, 11.0),
    ]
}

#[test]
fn t41_detect_eight_modes_synthetic_fixture() {
    // 8 个已知模态 + 宽带倾斜 + 噪声；至少两个真实 Q>10（12/18/14/15/20）
    let fs = 48000.0;
    let modes = fixture_modes();
    let curve = synth_modal_curve(fs, &modes, true, 0.15, 1600, 20.0, 2000.0, 20261117);
    let mut cfg = ModalDetectConfig::default();
    cfg.band_hz = [20.0, 400.0];
    let detected = detect_modal_modes(&curve, &cfg).unwrap();
    println!("detected {} modes:", detected.len());
    for (i, m) in detected.iter().enumerate() {
        println!(
            "  #{i}: f={:.3} Hz q={:.3} gain={:.3} dB prominence={:.3} dB unresolved={} bw={:.3} Hz step={:.3} Hz",
            m.freq_hz, m.q, m.gain_db, m.prominence_db, m.q_unresolved, m.bandwidth_hz, m.grid_step_hz
        );
    }
    assert_eq!(detected.len(), modes.len(), "应检出全部 8 个模态");
    let mut matched_high_q = 0;
    for &(true_f, true_q, _true_g) in &modes {
        let nearest = detected
            .iter()
            .min_by(|a, b| {
                (a.freq_hz - true_f)
                    .abs()
                    .partial_cmp(&(b.freq_hz - true_f).abs())
                    .unwrap()
            })
            .unwrap();
        assert!(
            (nearest.freq_hz - true_f).abs() <= 3.0,
            "模态 {true_f} Hz 检出 {:.3} Hz 超 ±3 Hz",
            nearest.freq_hz
        );
        if true_q > 10.0 {
            matched_high_q += 1;
            let q_err = (nearest.q - true_q).abs() / true_q;
            assert!(
                q_err <= 0.30,
                "Q>10 模态 {true_f}: 真实 {true_q} 检出 {:.3}（误差 {:.1}%）",
                nearest.q,
                q_err * 100.0
            );
            assert!(
                nearest.q > 10.0,
                "Q>10 模态被夹到 10 或更低：{:.3}",
                nearest.q
            );
            assert!(!nearest.q_unresolved, "细网格上 Q>10 模态不应 q_unresolved");
        }
        assert!(
            nearest.gain_db <= 0.0,
            "模态增益必须 ≤ 0: {}",
            nearest.gain_db
        );
    }
    assert!(matched_high_q >= 2, "至少两个 Q>10 模态被验证");
    // 检出 Q 不被夹到 10（存在 >10 的检出段）
    assert!(detected.iter().any(|m| m.q > 10.0), "Q 不应被夹到 10");
}

#[test]
fn t41_detect_flat_and_valley_produce_no_bands() {
    let flat = flat_curve(20.0, 500.0, 600, 0.0);
    let cfg = ModalDetectConfig::default();
    assert!(
        detect_modal_modes(&flat, &cfg).unwrap().is_empty(),
        "平直曲线不应产生模态段"
    );
    // 深谷不生成滤波器（只切；谷 = 局部最小）
    let mut dip = flat_curve(20.0, 500.0, 600, 0.0);
    let notch = Biquad::from_params(BandType::Peaking, 100.0, 10.0, -12.0, 48000.0).unwrap();
    for p in dip.iter_mut() {
        p.mag_db += notch.magnitude_db(p.freq_hz, 48000.0);
    }
    assert!(
        detect_modal_modes(&dip, &cfg).unwrap().is_empty(),
        "谷不生成滤波器"
    );
}

#[test]
fn t41_detect_q_unresolved_on_coarse_grid() {
    // 粗网格（1/3 oct 中心频率）+ 窄模态 Q=30 → 网格不可分辨 → q_unresolved，Q 取网格最细值
    let fs = 48000.0;
    let freqs = [
        20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0,
    ];
    let peak = Biquad::from_params(BandType::Peaking, 100.0, 30.0, 8.0, fs).unwrap();
    let curve: Vec<CurvePoint> = freqs
        .iter()
        .map(|&f| CurvePoint::new(f, peak.magnitude_db(f, fs)))
        .collect();
    let mut cfg = ModalDetectConfig::default();
    cfg.band_hz = [20.0, 250.0];
    let detected = detect_modal_modes(&curve, &cfg).unwrap();
    assert_eq!(detected.len(), 1, "应检出 1 个窄模态: {detected:?}");
    let m = detected[0];
    println!("coarse-grid narrow mode: {m:?}");
    assert!(m.q_unresolved, "粗网格窄模态必须 q_unresolved");
    assert_approx(m.freq_hz, 100.0, 3.0, "粗网格 f0");
    // Q 取网格能支持的最细值 ≈ f0 / 局部网格步长
    let expected_q = m.freq_hz / m.grid_step_hz;
    assert_approx(
        m.q,
        expected_q,
        expected_q * 0.05,
        "q_unresolved 时 Q=网格最细值",
    );
    assert!(m.gain_db <= 0.0);

    // 较密网格（1/24 oct）上的 Q=10 峰 → 可分辨（q_unresolved=false）
    let fine_grid = log_frequency_grid(20.0, 250.0, 1.0 / 24.0);
    let narrow = Biquad::from_params(BandType::Peaking, 100.0, 10.0, 8.0, fs).unwrap();
    let curve: Vec<CurvePoint> = fine_grid
        .iter()
        .map(|&f| CurvePoint::new(f, narrow.magnitude_db(f, fs)))
        .collect();
    let detected = detect_modal_modes(&curve, &cfg).unwrap();
    assert_eq!(detected.len(), 1, "密网格 Q=10 峰应检出: {detected:?}");
    assert!(
        !detected[0].q_unresolved,
        "密网格 Q=10 峰应可分辨: {:?}",
        detected[0]
    );
    assert_approx(detected[0].q, 10.0, 3.0, "Q=10 检出误差应在 ±30%");
}

#[test]
fn t41_detect_merge_and_max_bands() {
    let fs = 48000.0;
    // 两个峰相距 < 1/6 oct → 合并为一段（requirements 5 AC6）
    let curve = synth_modal_curve(
        fs,
        &[(100.0, 8.0, 8.0), (110.0, 8.0, 8.0)],
        false,
        0.0,
        1600,
        20.0,
        500.0,
        3,
    );
    let mut cfg = ModalDetectConfig::default();
    cfg.band_hz = [20.0, 250.0];
    let detected = detect_modal_modes(&curve, &cfg).unwrap();
    assert_eq!(detected.len(), 1, "1/6 oct 内相邻峰应合并: {detected:?}");
    // max_bands 限制生效（8 个模态 + max_bands=3 → 只保留 3 个最深）
    let curve = synth_modal_curve(fs, &fixture_modes(), true, 0.15, 1600, 20.0, 2000.0, 11);
    config_max_bands(&mut cfg, 3);
    let detected = detect_modal_modes(&curve, &cfg).unwrap();
    assert_eq!(detected.len(), 3, "max_bands 应被遵守: {detected:?}");
}

fn config_max_bands(cfg: &mut ModalDetectConfig, max_bands: usize) {
    cfg.max_bands = max_bands;
    cfg.band_hz = [20.0, 400.0];
}

#[test]
fn t41_design_from_curve_json_evidence() {
    // 证据用样例：curve → 检测 → tspi-modal-peq/v1 JSON（--nocapture 日志写入 evidence/T41）
    let fs = 48000.0;
    let curve = synth_modal_curve(
        fs,
        &[(60.0, 10.0, 7.0), (110.0, 14.0, 9.0)],
        true,
        0.1,
        1200,
        20.0,
        1000.0,
        99,
    );
    let mut cfg = ModalDetectConfig::default();
    cfg.band_hz = [20.0, 250.0];
    let mut design = design_from_curve(48000, "FL", 0, &curve, &cfg).unwrap();
    design.created_unix = 1_700_000_000;
    design.warnings = design.validate().unwrap();
    let json = design.to_json();
    println!("T41_MODAL_PEQ_JSON_BEGIN");
    println!("{json}");
    println!("T41_MODAL_PEQ_JSON_END");
    let v = parse_json(&json);
    assert_eq!(v.get("schema").unwrap().as_str(), MODAL_PEQ_SCHEMA);
    assert!(!v.get("channels").unwrap().as_arr()[0]
        .get("bands")
        .unwrap()
        .as_arr()
        .is_empty());
    assert_eq!(
        v.get("design").unwrap().get("mode").unwrap().as_str(),
        "modal_only"
    );
}
