//! T12 compile 私有测试（WBS T12；写域 crates/core/tests/T12/）。
//!
//! 覆盖 WBS T12 DoD：
//! - factorized 与 fused `F = P * (delta_D I + g)` 逐样本等价（含 `delta_D` 对角 baseline）；
//! - 长度加法 `L_F = L_G + L_P - 1` / 完整图长 `L_A + L_G + L_P - 2`；
//! - 行列 delay/polarity 方向（输入作用列、输出作用行，P/G 不交换）；
//! - zero-padding 线性卷积等价（短线 FFT 循环卷积混叠反断言）；
//! - 融合 cost 计划、选择与非假合并省算（direct 成本对拍 planner 数字）；
//! - 对拍 T07 direct kernel（factorized）与 T03 reference fixtures C07/C09（fused 同 expected）；
//! - 输入校验负向/half-integer D 的 delay_q16 拆分。
//!
//! 自包含（无第三方依赖）：极简 JSON 读取 T03 fixtures C07/C09（include_str 绑定只读参考文件）。
//! Cargo 自动把 tests/T12/main.rs 发现为集成测试 target；运行：
//! `cargo test -p khaen-core fusion -- --nocapture`
//!
//! T03 fixtures 是只读输入；本测试不改它们，只读取 graph/input/expected/supports。

use khaen_core::compile::{
    compile, convolve_linear, delay_plan_residual_samples, design_delay_to_plan, next_pow2,
    oracle_spectral_convolve_circular, oracle_spectral_convolve_zero_padded, CompileInput,
    CompileOptions, CompiledPlans, FusionForm, SelectedPlan, SelectionReason, COMPILE_NO_SOURCE,
    PATH_FLAG_DIAG_BASELINE_INCLUDED, PATH_FLAG_FUSED, PATH_FLAG_ZERO_PAD_LINEAR_CONV,
};
use khaen_core::direct::{
    direct_block_f64, DirectCoeffsF64, DirectLayout, DirectTopology, DIRECT_MAX_CONTAINERS,
    DIRECT_MAX_PARTICIPANTS, DIRECT_MAX_ROLES, DIRECT_MAX_SOURCES, DIRECT_NO_SOURCE,
};
use khaen_core::rt::RtStatus;

const C07_JSON: &str =
    include_str!("../../../../reference/fixtures/cases/C07_factorized_fused.json");
const C09_JSON: &str =
    include_str!("../../../../reference/fixtures/cases/C09_4_1_2_5x5_embed.json");

// ---------------------------------------------------------------------------
// 极简 JSON 读取（仅覆盖 fixture 子集：number/string/array/object）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Json {
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(items) => items
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("JSON missing key {key}")),
            _ => panic!("JSON value for {key} is not object"),
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            Json::Num(v) => *v,
            _ => panic!("JSON value is not number"),
        }
    }

    fn as_usize(&self) -> usize {
        let v = self.as_f64();
        assert!(v >= 0.0 && v.fract() == 0.0, "not a non-negative integer");
        v as usize
    }

    fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => panic!("JSON value is not array"),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("JSON value is not string"),
        }
    }

    fn nums(&self) -> Vec<f64> {
        self.as_arr().iter().map(|v| v.as_f64()).collect()
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn parse(text: &'a str) -> Json {
        let mut parser = Self {
            bytes: text.as_bytes(),
            pos: 0,
        };
        let value = parser.value();
        parser.skip_ws();
        assert_eq!(parser.pos, parser.bytes.len(), "JSON trailing bytes");
        value
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> u8 {
        assert!(self.pos < self.bytes.len(), "JSON unexpected end");
        self.bytes[self.pos]
    }

    fn expect(&mut self, want: u8) {
        self.skip_ws();
        let got = self.peek();
        assert_eq!(got, want, "JSON expected {want} got {got}");
        self.pos += 1;
    }

    fn value(&mut self) -> Json {
        self.skip_ws();
        match self.peek() {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Json::Str(self.string()),
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Json {
        self.expect(b'{');
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == b'}' {
            self.pos += 1;
            return Json::Obj(items);
        }
        loop {
            self.skip_ws();
            let key = self.string();
            self.expect(b':');
            let value = self.value();
            items.push((key, value));
            self.skip_ws();
            match self.peek() {
                b',' => {
                    self.pos += 1;
                }
                b'}' => {
                    self.pos += 1;
                    break;
                }
                other => panic!("JSON object expected , or }} got {other}"),
            }
        }
        Json::Obj(items)
    }

    fn array(&mut self) -> Json {
        self.expect(b'[');
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == b']' {
            self.pos += 1;
            return Json::Arr(items);
        }
        loop {
            items.push(self.value());
            self.skip_ws();
            match self.peek() {
                b',' => {
                    self.pos += 1;
                }
                b']' => {
                    self.pos += 1;
                    break;
                }
                other => panic!("JSON array expected , or ] got {other}"),
            }
        }
        Json::Arr(items)
    }

    fn string(&mut self) -> String {
        self.expect(b'"');
        let mut out = String::new();
        loop {
            let byte = self.peek();
            self.pos += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escaped = self.peek();
                    self.pos += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let digit = self.peek();
                                self.pos += 1;
                                let value = match digit {
                                    b'0'..=b'9' => (digit - b'0') as u32,
                                    b'a'..=b'f' => (digit - b'a' + 10) as u32,
                                    b'A'..=b'F' => (digit - b'A' + 10) as u32,
                                    _ => panic!("JSON bad \\u escape"),
                                };
                                code = code * 16 + value;
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => panic!("JSON bad escape {}", other as char),
                    }
                }
                _ => out.push(byte as char),
            }
        }
        out
    }

    fn number(&mut self) -> Json {
        let start = self.pos;
        if self.peek() == b'-' {
            self.pos += 1;
        }
        while self.pos < self.bytes.len()
            && matches!(
                self.bytes[self.pos],
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
            )
        {
            self.pos += 1;
        }
        let text = core::str::from_utf8(&self.bytes[start..self.pos]).expect("utf8 number");
        Json::Num(text.parse::<f64>().expect("JSON number parse"))
    }
}

// ---------------------------------------------------------------------------
// T03 fixture 读取与编译输入准备
// ---------------------------------------------------------------------------

struct Fixture {
    id: String,
    n_src: usize,
    n_role: usize,
    container_channels: usize,
    d_samples: u32,
    a_map: Vec<Vec<f64>>,
    a_fir: Vec<Vec<f64>>,
    participants: Vec<u16>,
    g: Vec<Vec<Vec<f64>>>,
    p: Vec<Vec<f64>>,
    scatter: Vec<u16>,
    l_a: u32,
    l_g: u32,
    l_p: u32,
    input: Vec<Vec<f64>>,
    expected: Vec<Vec<f64>>,
}

fn load_fixture(text: &str) -> Fixture {
    let root = JsonParser::parse(text);
    let graph = root.get("graph");
    let supports = root.get("supports");
    let a_map: Vec<Vec<f64>> = graph
        .get("a_map")
        .as_arr()
        .iter()
        .map(|row| row.nums())
        .collect();
    let a_fir: Vec<Vec<f64>> = graph
        .get("a_fir")
        .as_arr()
        .iter()
        .map(|row| row.nums())
        .collect();
    let participants: Vec<u16> = graph
        .get("participants")
        .as_arr()
        .iter()
        .map(|v| v.as_usize() as u16)
        .collect();
    let g: Vec<Vec<Vec<f64>>> = graph
        .get("g")
        .as_arr()
        .iter()
        .map(|row| row.as_arr().iter().map(|cell| cell.nums()).collect())
        .collect();
    let p: Vec<Vec<f64>> = graph
        .get("p")
        .as_arr()
        .iter()
        .map(|row| row.nums())
        .collect();
    let scatter: Vec<u16> = graph
        .get("scatter")
        .as_arr()
        .iter()
        .map(|v| v.as_usize() as u16)
        .collect();
    Fixture {
        id: root.get("id").as_str().to_string(),
        n_src: graph.get("n_src").as_usize(),
        n_role: graph.get("n_out").as_usize(),
        container_channels: graph.get("container_channels").as_usize(),
        d_samples: graph.get("d_samples").as_usize() as u32,
        a_map,
        a_fir,
        participants,
        g,
        p,
        scatter,
        l_a: supports.get("l_a").as_usize() as u32,
        l_g: supports.get("l_g").as_usize() as u32,
        l_p: supports.get("l_p").as_usize() as u32,
        input: root
            .get("input")
            .as_arr()
            .iter()
            .map(|row| row.nums())
            .collect(),
        expected: root
            .get("expected")
            .as_arr()
            .iter()
            .map(|row| row.nums())
            .collect(),
    }
}

/// 编译输入 + 其 owned 数组（借用生命周期自持，避免测试内重复造轮子）。
struct Prepared {
    n_src: u16,
    n_role: u16,
    container_channels: u16,
    d_samples: u32,
    participants: Vec<u16>,
    src_of_role: Vec<u16>,
    scatter: Vec<u16>,
    a: Vec<f64>,
    l_a: u32,
    g: Vec<f64>,
    l_g: u32,
    p: Vec<f64>,
    l_p: u32,
}

impl Prepared {
    fn input(&self) -> CompileInput<'_> {
        CompileInput {
            n_src: self.n_src,
            n_role: self.n_role,
            container_channels: self.container_channels,
            d_samples: self.d_samples,
            participants: &self.participants,
            src_of_role: &self.src_of_role,
            scatter: &self.scatter,
            a: &self.a,
            l_a: self.l_a,
            g: &self.g,
            l_g: self.l_g,
            p: &self.p,
            l_p: self.l_p,
        }
    }
}

fn max_len(rows: &[Vec<f64>]) -> usize {
    rows.iter().map(|row| row.len()).max().unwrap_or(1).max(1)
}

fn flatten_padded(rows: &[Vec<f64>], width: usize) -> Vec<f64> {
    let mut out = vec![0.0; rows.len() * width];
    for (r, row) in rows.iter().enumerate() {
        assert!(row.len() <= width, "row wider than declared support");
        out[r * width..r * width + row.len()].copy_from_slice(row);
    }
    out
}

fn src_of_role_from_map(a_map: &[Vec<f64>], n_src: usize) -> Vec<u16> {
    let mut out = vec![COMPILE_NO_SOURCE; a_map.len()];
    for (role, row) in a_map.iter().enumerate() {
        assert_eq!(row.len(), n_src, "a_map row width");
        let ones: Vec<usize> = row
            .iter()
            .enumerate()
            .filter(|(_, v)| **v == 1.0)
            .map(|(i, _)| i)
            .collect();
        assert!(ones.len() <= 1, "A_map row must be selection/zero");
        if let Some(src) = ones.first() {
            out[role] = *src as u16;
        }
    }
    out
}

fn prepare_fixture(fixture: &Fixture) -> Prepared {
    assert_eq!(fixture.a_fir.len(), fixture.n_src, "a_fir count");
    assert_eq!(fixture.p.len(), fixture.n_role, "p count");
    assert_eq!(fixture.g.len(), fixture.participants.len(), "g rows");
    for row in &fixture.g {
        assert_eq!(row.len(), fixture.participants.len(), "g cols");
    }
    let l_a = fixture.l_a;
    let l_p = fixture.l_p;
    let g_max = fixture
        .g
        .iter()
        .flat_map(|row| row.iter())
        .map(|cell| cell.len())
        .max()
        .unwrap_or(0) as u32;
    let l_g = fixture.l_g;
    assert_eq!(
        l_g,
        (fixture.d_samples + 1).max(g_max),
        "L_G support formula"
    );
    assert_eq!(l_a as usize, max_len(&fixture.a_fir), "L_A declared");
    assert_eq!(l_p as usize, max_len(&fixture.p), "L_P declared");
    let a = flatten_padded(&fixture.a_fir, l_a as usize);
    let mut g = Vec::new();
    for row in &fixture.g {
        for cell in row {
            let padded = flatten_padded(&[cell.clone()], l_g as usize);
            g.extend_from_slice(&padded);
        }
    }
    let p = flatten_padded(&fixture.p, l_p as usize);
    Prepared {
        n_src: fixture.n_src as u16,
        n_role: fixture.n_role as u16,
        container_channels: fixture.container_channels as u16,
        d_samples: fixture.d_samples,
        participants: fixture.participants.clone(),
        src_of_role: src_of_role_from_map(&fixture.a_map, fixture.n_src),
        scatter: fixture.scatter.clone(),
        a,
        l_a,
        g,
        l_g,
        p,
        l_p,
    }
}

/// 合成 2x2 基础图：N=O=C=2、participants=[0,1]、g=0、D=0、L_A=L_G=L_P=2（便于改 g）。
fn prepared_2x2() -> Prepared {
    Prepared {
        n_src: 2,
        n_role: 2,
        container_channels: 2,
        d_samples: 0,
        participants: vec![0, 1],
        src_of_role: vec![0, 1],
        scatter: vec![0, 1],
        a: vec![1.0, 0.0, 1.0, 0.0],
        l_a: 2,
        g: vec![0.0; 2 * 2 * 2],
        l_g: 2,
        p: vec![1.0, 0.0, 1.0, 0.0],
        l_p: 2,
    }
}

fn set_a_row(prep: &mut Prepared, src: usize, values: &[f64]) {
    let width = prep.l_a as usize;
    assert!(values.len() <= width);
    let base = src * width;
    for (t, v) in values.iter().enumerate() {
        prep.a[base + t] = *v;
    }
}

fn set_p_row(prep: &mut Prepared, role: usize, values: &[f64]) {
    let width = prep.l_p as usize;
    assert!(values.len() <= width);
    let base = role * width;
    for (t, v) in values.iter().enumerate() {
        prep.p[base + t] = *v;
    }
}

fn set_g_cell(prep: &mut Prepared, row: usize, col: usize, values: &[f64]) {
    let m = prep.participants.len();
    let width = prep.l_g as usize;
    assert!(values.len() <= width);
    let base = (row * m + col) * width;
    for (t, v) in values.iter().enumerate() {
        prep.g[base + t] = *v;
    }
}

// ---------------------------------------------------------------------------
// 求值器：factorized 走 T07 direct kernel；fused 走离线线性卷积
// ---------------------------------------------------------------------------

fn direct_topology(prep: &Prepared) -> DirectTopology {
    assert!(prep.n_src as usize <= DIRECT_MAX_SOURCES);
    assert!(prep.n_role as usize <= DIRECT_MAX_ROLES);
    assert!(prep.container_channels as usize <= DIRECT_MAX_CONTAINERS);
    assert!(prep.participants.len() <= DIRECT_MAX_PARTICIPANTS);
    let mut topo = DirectTopology::identity();
    topo.n_src = prep.n_src;
    topo.n_role = prep.n_role;
    topo.container_channels = prep.container_channels;
    topo.d_samples = prep.d_samples;
    topo.l_a = prep.l_a as u16;
    topo.l_g = prep.l_g as u16;
    topo.l_p = prep.l_p as u16;
    topo.m = prep.participants.len() as u16;
    topo.src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
    for (role, &src) in prep.src_of_role.iter().enumerate() {
        topo.src_of_role[role] = src;
    }
    topo.participants = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
    for (pos, &role) in prep.participants.iter().enumerate() {
        topo.participants[pos] = role;
    }
    topo.scatter = [0u16; DIRECT_MAX_ROLES];
    for (role, &container) in prep.scatter.iter().enumerate() {
        topo.scatter[role] = container;
    }
    topo
}

/// 用 T07 direct kernel（f64 oracle 入口）跑 factorized 图，返回 container x L_out。
fn eval_factorized_direct(prep: &Prepared, input: &[Vec<f64>], block: usize) -> Vec<Vec<f64>> {
    let topo = direct_topology(prep);
    assert!(topo.validate().is_ok(), "direct topology validate failed");
    let layout = DirectLayout::build(&topo).expect("direct layout");
    let n_src = prep.n_src as usize;
    let n_role = prep.n_role as usize;
    let containers = prep.container_channels as usize;
    let l_in = input[0].len();
    for row in input {
        assert_eq!(row.len(), l_in, "input rows must be equal length");
    }
    let full_len = (prep.l_a + prep.l_g + prep.l_p - 2) as usize;
    let l_out = l_in + full_len - 1;
    let total = l_out;
    let blocks = (total + block - 1) / block;
    let padded = blocks * block;
    let mut out = vec![vec![0.0; padded]; containers];
    let mut state = vec![0.0; layout.state_samples()];
    let coeffs = DirectCoeffsF64 {
        a_fir: &prep.a,
        g: &prep.g,
        p: &prep.p,
    };
    let _ = n_role;
    for b in 0..blocks {
        let mut block_in = vec![0.0; n_src * block];
        for ch in 0..n_src {
            for t in 0..block {
                let index = b * block + t;
                if index < l_in {
                    block_in[ch * block + t] = input[ch][index];
                }
            }
        }
        let mut block_out = vec![0.0; containers * block];
        let status = direct_block_f64(
            &topo,
            &layout,
            &coeffs,
            &mut state,
            &block_in,
            block,
            &mut block_out,
            block,
            block,
            block,
        );
        assert!(status.is_ok(), "direct_block_f64 status {status:?}");
        for ch in 0..containers {
            for t in 0..block {
                out[ch][b * block + t] = block_out[ch * block + t];
            }
        }
    }
    for row in out.iter_mut() {
        row.truncate(l_out);
    }
    out
}

/// 用 fused 图的 F 路径（A 前置 + 线性卷积）跑整图，返回 container x L_out。
fn eval_fused(prep: &Prepared, plans: &CompiledPlans, input: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let fused = plans.fused.as_ref().expect("fused graph expected");
    let n_src = prep.n_src as usize;
    let l_a = prep.l_a as usize;
    let l_in = input[0].len();
    let full_len = (prep.l_a + prep.l_g + prep.l_p - 2) as usize;
    let l_out = l_in + full_len - 1;
    // A 阶段（输入侧 FIR，作用列）。
    let u: Vec<Vec<f64>> = (0..n_src)
        .map(|src| convolve_linear(&prep.a[src * l_a..(src + 1) * l_a], &input[src]))
        .collect();
    let mut role_out = vec![vec![0.0; l_out]; prep.n_role as usize];
    for path in &fused.paths {
        let contrib = convolve_linear(&path.coeffs, &u[path.in_index as usize]);
        let row = &mut role_out[path.out_index as usize];
        for (n, value) in contrib.iter().enumerate() {
            if n < l_out {
                row[n] += value;
            }
        }
    }
    let mut out = vec![vec![0.0; l_out]; prep.container_channels as usize];
    for (role, row) in role_out.iter().enumerate() {
        let container = prep.scatter[role] as usize;
        out[container] = row.clone();
    }
    out
}

// ---------------------------------------------------------------------------
// 比较辅助
// ---------------------------------------------------------------------------

fn assert_shape(label: &str, got: &[Vec<f64>], expected: &[Vec<f64>]) {
    assert_eq!(got.len(), expected.len(), "{label}: channel count");
    for (ch, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
        assert_eq!(g.len(), e.len(), "{label}: length ch{ch}");
    }
}

fn max_abs_diff(got: &[Vec<f64>], expected: &[Vec<f64>]) -> f64 {
    let mut worst = 0.0f64;
    for (g, e) in got.iter().zip(expected.iter()) {
        for (a, b) in g.iter().zip(e.iter()) {
            let diff = (a - b).abs();
            if diff > worst {
                worst = diff;
            }
            assert!(a.is_finite() && b.is_finite(), "non-finite sample");
        }
    }
    worst
}

fn assert_exact_2d(label: &str, got: &[Vec<f64>], expected: &[Vec<f64>]) {
    assert_shape(label, got, expected);
    for (ch, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
        for (n, (a, b)) in g.iter().zip(e.iter()).enumerate() {
            assert!(
                *a == *b || (*a == 0.0 && *b == 0.0),
                "{label}: mismatch at ch{ch} n{n}: got {a} expected {b}"
            );
        }
    }
}

fn assert_close_2d(label: &str, got: &[Vec<f64>], expected: &[Vec<f64>], tol: f64) {
    assert_shape(label, got, expected);
    let worst = max_abs_diff(got, expected);
    assert!(worst <= tol, "{label}: max abs diff {worst} > {tol}");
}

fn compile_ok(prep: &Prepared, options: &CompileOptions) -> CompiledPlans {
    compile(&prep.input(), options).expect("compile should succeed")
}

fn compile_err(prep: &Prepared, options: &CompileOptions) -> RtStatus {
    compile(&prep.input(), options).expect_err("compile should fail")
}

fn selected_form(plans: &CompiledPlans) -> FusionForm {
    match plans.selected() {
        SelectedPlan::Factorized(_) => FusionForm::Factorized,
        SelectedPlan::Fused(_) => FusionForm::FusedGp,
    }
}

// ---------------------------------------------------------------------------
// T12 测试
// ---------------------------------------------------------------------------

#[test]
fn fusion_delay_q16_split_contract() {
    // core-v1 §3.4/§11.10/§12.3：半整数/分数 D -> (delay_samples, delay_q16)，残差 < 1/65536。
    let half_cases: [(f64, u32, u32); 4] = [
        (0.0, 0, 0),
        (0.5, 0, 32_768),
        (2.5, 2, 32_768),
        (4095.5, 4095, 32_768),
    ];
    for (total, want_int, want_q16) in half_cases {
        let plan = design_delay_to_plan(total).expect("half-integer delay split");
        assert_eq!(plan.delay_samples, want_int, "int part of {total}");
        assert_eq!(plan.delay_q16, want_q16, "q16 part of {total}");
        assert!(plan.delay_q16 < 65_536, "delay_q16 must be < 65536");
        assert_eq!(
            plan.total_delay_q16(),
            ((want_int as u64) << 16) | want_q16 as u64,
            "total_delay_q16 layout"
        );
        assert!(delay_plan_residual_samples(plan, total) < 1.0 / 65_536.0);
    }

    // 一般分数：残差上界 + Q16 单调性。
    for step in 0..200 {
        let total = step as f64 / 7.3 + 0.125;
        let plan = design_delay_to_plan(total).expect("fractional delay split");
        assert!(plan.delay_q16 < 65_536);
        let residual = delay_plan_residual_samples(plan, total);
        assert!(
            residual < 1.0 / 65_536.0,
            "residual {residual} >= 1/65536 for {total}"
        );
        assert!((plan.delay_samples as f64) <= total + 1.0);
    }

    // round 进位到整数：0.99999999 -> (1, 0)，残差 < 1/65536。
    let carried = design_delay_to_plan(0.999_999_99).expect("carry split");
    assert_eq!(carried.delay_samples, 1);
    assert_eq!(carried.delay_q16, 0);
    assert!(delay_plan_residual_samples(carried, 0.999_999_99) < 1.0 / 65_536.0);

    // 1/3 sample：round(1/3 * 65536) = 21845。
    let third = design_delay_to_plan(1.0 / 3.0).expect("third sample");
    assert_eq!(third.delay_samples, 0);
    assert_eq!(third.delay_q16, 21_845);
    assert!(delay_plan_residual_samples(third, 1.0 / 3.0) < 1.0 / 65_536.0);

    // 拒绝：负数/NaN/Inf/超出 u32 样本。
    assert_eq!(
        design_delay_to_plan(-1.0),
        Err(RtStatus::ERR_TAPS),
        "negative delay must be rejected"
    );
    assert_eq!(design_delay_to_plan(f64::NAN), Err(RtStatus::ERR_TAPS));
    assert_eq!(design_delay_to_plan(f64::INFINITY), Err(RtStatus::ERR_TAPS));
    assert_eq!(
        design_delay_to_plan(1.0e12),
        Err(RtStatus::ERR_TAPS),
        "out of u32 sample range"
    );
}

#[test]
fn fusion_validation_negative() {
    let options = CompileOptions::default();
    let valid = prepared_2x2();
    // D=0、L_G=2、a/p 全零尾 -> 合法。
    compile_ok(&valid, &options);

    // 通道/容器。
    let mut prep = prepared_2x2();
    prep.n_src = 0;
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_CHANNELS);
    let mut prep = prepared_2x2();
    prep.container_channels = 1;
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_CHANNELS);

    // role / participants / A_map / scatter。
    let mut prep = prepared_2x2();
    prep.n_role = 0;
    prep.src_of_role.clear();
    prep.scatter.clear();
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);
    let mut prep = prepared_2x2();
    prep.participants = vec![0, 0];
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);
    let mut prep = prepared_2x2();
    prep.participants = vec![0, 5];
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);
    let mut prep = prepared_2x2();
    prep.src_of_role = vec![0, 0];
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);
    let mut prep = prepared_2x2();
    prep.src_of_role = vec![0, 5];
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);
    let mut prep = prepared_2x2();
    prep.scatter = vec![0, 0];
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);
    let mut prep = prepared_2x2();
    prep.scatter = vec![0, 2];
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_ROLES);

    // taps / D / 数组长度 / 非有限系数。
    let mut prep = prepared_2x2();
    prep.l_a = 0;
    prep.a.clear();
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_TAPS);
    let mut prep = prepared_2x2();
    prep.d_samples = 2; // L_G=2 -> 需要 L_G >= D+1 = 3
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_TAPS);
    let mut prep = prepared_2x2();
    prep.a.pop();
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_PATH_TABLE);
    let mut prep = prepared_2x2();
    prep.g.pop();
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_PATH_TABLE);
    let mut prep = prepared_2x2();
    prep.p.push(0.0);
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_PATH_TABLE);
    let mut prep = prepared_2x2();
    prep.p[0] = f64::NAN;
    assert_eq!(compile_err(&prep, &options), RtStatus::ERR_PATH_TABLE);

    // options。
    let mut opts = CompileOptions::default();
    opts.sample_rate_hz = 44_100;
    assert_eq!(compile_err(&valid, &opts), RtStatus::ERR_RATE);
    let mut opts = CompileOptions::default();
    opts.block_size = 256;
    assert_eq!(compile_err(&valid, &opts), RtStatus::ERR_BLOCK_SIZE);
    let mut opts = CompileOptions::default();
    opts.max_taps = 0;
    assert_eq!(compile_err(&valid, &opts), RtStatus::ERR_TAPS);
    let mut opts = CompileOptions::default();
    opts.max_taps = 8192; // extended 未准入
    assert_eq!(compile_err(&valid, &opts), RtStatus::ERR_TAPS);
    let mut opts = CompileOptions::default();
    opts.coeff_budget_bytes = 0;
    assert_eq!(compile_err(&valid, &opts), RtStatus::ERR_CAPACITY);

    // factorized 峰值超 profile：L_G=4096 > max_taps=2048 -> INFEASIBLE，不截断。
    let mut prep = prepared_2x2();
    prep.l_g = 4096;
    prep.g = vec![0.0; 2 * 2 * 4096];
    let mut opts = CompileOptions::default();
    opts.max_taps = 2048;
    assert_eq!(compile_err(&prep, &opts), RtStatus::ERR_TAPS);
}

#[test]
fn fusion_length_additivity_and_limits() {
    // L_F = L_G + L_P - 1；完整图长 = L_A + L_G + L_P - 2（T03 §3.1 / T04 V14）。
    let cases: [(u32, u32, u32); 4] = [
        (2, 2, 3),
        (4096, 1024, 5119),
        (8192, 4096, 12287),
        (2048, 2048, 4095),
    ];
    for (l_g, l_p, want_l_f) in cases {
        let mut prep = prepared_2x2();
        prep.l_g = l_g;
        prep.g = vec![0.0; 2 * 2 * l_g as usize];
        prep.l_p = l_p;
        prep.p = vec![0.0; 2 * l_p as usize];
        set_p_row(&mut prep, 0, &[1.0]);
        set_p_row(&mut prep, 1, &[1.0]);
        let mut opts = CompileOptions::default();
        opts.max_taps = 16_384;
        opts.extended_taps_admitted = true;
        let plans = compile_ok(&prep, &opts);
        assert_eq!(plans.report.fused_len_gp, want_l_f, "L_F for {l_g}+{l_p}");
        assert_eq!(
            plans.report.full_path_len,
            prep.l_a + l_g + l_p - 2,
            "L_total for {l_g}+{l_p}"
        );
        let fused = plans.fused.as_ref().expect("fused feasible");
        for path in &fused.paths {
            assert_eq!(path.taps, want_l_f, "fused path taps");
            assert_eq!(path.coeffs.len() as u32, want_l_f, "fused coeffs len");
        }
    }

    // A 非 delta 时完整图长多出 L_A-1；G/P 融合长度不变（T03 口径 vs T04 口径）。
    let mut prep = prepared_2x2();
    prep.l_a = 3;
    prep.a = vec![0.0; 2 * 3];
    set_a_row(&mut prep, 0, &[1.0]);
    set_a_row(&mut prep, 1, &[1.0]);
    let plans = compile_ok(&prep, &CompileOptions::default());
    assert_eq!(plans.report.fused_len_gp, 2 + 2 - 1);
    assert_eq!(plans.report.full_path_len, 3 + 2 + 2 - 2);
    assert_eq!(plans.factorized.full_path_len(), 5);
}

#[test]
fn fusion_cost_plan_selection_no_fake_saving() {
    // planner 示例（DSP-HOST-DECISION §3 / T04 §7.1）：G=4096 + P=1024，25 条 G/P 路径。
    // factorized direct = 25*4096 + 5*1024 = 107,520 MAC/sample -> 10.32192 GF/s @48k；
    // fused        direct = 25*5119              = 127,975 MAC/sample -> 12.2856 GF/s。
    // 融合反而更贵 -> 必须报告并保留 factorized，不得假合并省算。
    fn prepared_planner_example() -> Prepared {
        let n = 5usize;
        let l_g = 4096usize;
        let l_p = 1024usize;
        let mut g = vec![0.0; n * n * l_g];
        for cell in 0..n * n {
            g[cell * l_g] = 1.0; // 每个 cell 至少一个非零 tap（25 条路径全非零）
        }
        let mut p = vec![0.0; n * l_p];
        for role in 0..n {
            p[role * l_p] = 1.0; // 声明支撑 1024；len>1 故不是 identity（按声明成本计）
        }
        Prepared {
            n_src: n as u16,
            n_role: n as u16,
            container_channels: n as u16,
            d_samples: 0,
            participants: (0..n as u16).collect(),
            src_of_role: (0..n as u16).collect(),
            scatter: (0..n as u16).collect(),
            a: vec![1.0; n],
            l_a: 1,
            g,
            l_g: l_g as u32,
            p,
            l_p: l_p as u32,
        }
    }

    let prep = prepared_planner_example();
    let mut opts = CompileOptions::default();
    opts.max_taps = 16_384;
    opts.extended_taps_admitted = true;
    let plans = compile_ok(&prep, &opts);
    let report = &plans.report;

    assert_eq!(report.route_count, 25, "5x5 dense G -> 25 routes");
    assert_eq!(report.fused_len_gp, 5119);
    assert_eq!(report.full_path_len, 5119);
    assert_eq!(report.factorized.input_stage_taps, 0, "identity A elided");
    assert_eq!(report.factorized.matrix_stage_taps, 25 * 4096);
    assert_eq!(report.factorized.output_stage_taps, 5 * 1024);
    assert_eq!(report.factorized.macs_per_sample, 107_520);
    assert_eq!(report.fused.matrix_stage_taps, 25 * 5119);
    assert_eq!(report.fused.macs_per_sample, 127_975);
    assert!((report.factorized.flops_per_second - 10_321_920_000.0).abs() < 1e-3);
    assert!((report.fused.flops_per_second - 12_285_600_000.0).abs() < 1e-3);
    assert_eq!(report.chosen, FusionForm::Factorized);
    assert_eq!(report.reason, SelectionReason::FusedNotCheaper);
    assert_eq!(report.saved_macs_per_sample, -20_455);
    assert!(report.saved_flops_per_second < 0.0);
    assert!(report.fused_feasible && report.fused_built);
    assert_eq!(selected_form(&plans), FusionForm::Factorized);

    // normal profile（L_F=5119 > 4096）：保留 factorized、报告超限，不静默截尾。
    let plans_norm = compile_ok(&prep, &CompileOptions::default());
    assert_eq!(plans_norm.report.reason, SelectionReason::FusedOverTapLimit);
    assert_eq!(plans_norm.report.chosen, FusionForm::Factorized);
    assert!(!plans_norm.report.fused_feasible);
    assert!(!plans_norm.report.fused_built);
    assert!(plans_norm.fused.is_none());
    assert_eq!(
        plans_norm.report.fused_len_gp, 5119,
        "still reported, no truncation"
    );
    assert_eq!(plans_norm.report.fused.macs_per_sample, 127_975);

    // 预算不足：factorized 系数区也超预算 -> INFEASIBLE，不静默降级。
    let mut low_budget = opts;
    low_budget.coeff_budget_bytes = 100_000;
    assert_eq!(compile_err(&prep, &low_budget), RtStatus::ERR_CAPACITY);

    // 对角-only（g=0, L_G=1, L_P=2）：fused 5 路 -> 真正更省，选 fused 并报告正节省。
    fn prepared_cheap_fused(l_g: u32, d: u32) -> Prepared {
        let mut prep = Prepared {
            n_src: 2,
            n_role: 2,
            container_channels: 2,
            d_samples: d,
            participants: vec![0],
            src_of_role: vec![0, 1],
            scatter: vec![0, 1],
            a: vec![1.0, 1.0],
            l_a: 1,
            g: vec![0.0; 1 * 1 * l_g as usize],
            l_g,
            p: vec![1.0, 0.0, 1.0, 0.0],
            l_p: 2,
        };
        set_p_row(&mut prep, 0, &[1.0]);
        set_p_row(&mut prep, 1, &[1.0]);
        prep
    }
    let prep_cheap = prepared_cheap_fused(1, 0);
    let plans_cheap = compile_ok(&prep_cheap, &CompileOptions::default());
    let report_cheap = &plans_cheap.report;
    // A=identity 0；routes = 2 条对角；factor = 2*1 + 2*2 = 6；fused = 2*(1+2-1) = 4。
    assert_eq!(report_cheap.route_count, 2);
    assert_eq!(report_cheap.factorized.macs_per_sample, 6);
    assert_eq!(report_cheap.fused.macs_per_sample, 4);
    assert_eq!(report_cheap.chosen, FusionForm::FusedGp);
    assert_eq!(report_cheap.reason, SelectionReason::FusedCheaper);
    assert_eq!(report_cheap.saved_macs_per_sample, 2);
    assert_eq!(selected_form(&plans_cheap), FusionForm::FusedGp);

    // fused 更省但 L_F > normal max_taps：保留 factorized（不截尾）；extended 准入后选 fused。
    let prep_over = prepared_cheap_fused(4096, 0);
    let plans_over = compile_ok(&prep_over, &CompileOptions::default());
    assert_eq!(plans_over.report.fused_len_gp, 4097);
    assert_eq!(plans_over.report.reason, SelectionReason::FusedOverTapLimit);
    assert!(!plans_over.report.fused_built);
    assert_eq!(
        plans_over.report.factorized.macs_per_sample,
        2 * 4096 + 2 * 2
    );
    assert_eq!(plans_over.report.fused.macs_per_sample, 2 * 4097);
    assert!(plans_over.report.fused.macs_per_sample < plans_over.report.factorized.macs_per_sample);
    assert_eq!(plans_over.report.chosen, FusionForm::Factorized);

    let mut ext = CompileOptions::default();
    ext.max_taps = 8192;
    ext.extended_taps_admitted = true;
    let plans_ext = compile_ok(&prep_over, &ext);
    assert_eq!(plans_ext.report.reason, SelectionReason::FusedCheaper);
    assert_eq!(plans_ext.report.chosen, FusionForm::FusedGp);
    assert!(plans_ext.report.fused_built);
}

#[test]
fn fusion_c07_factorized_fused_reference() {
    // T03 reference fixture C07（只读 include_str 绑定）：factorized 与 fused 输出一致，
    // 且都等于 fixture expected（G=[[z^-1,0.5],[0.25,z^-1]]，P 行不同，含 tail/D）。
    let fixture = load_fixture(C07_JSON);
    assert!(fixture.id.contains("C07"), "fixture id {}", fixture.id);
    assert_eq!(fixture.expected.len(), fixture.container_channels);
    let prep = prepare_fixture(&fixture);
    let plans = compile_ok(&prep, &CompileOptions::default());

    let direct = eval_factorized_direct(&prep, &fixture.input, 64);
    assert_exact_2d(
        "C07 factorized vs reference expected",
        &direct,
        &fixture.expected,
    );

    let fused_graph = plans.fused.as_ref().expect("C07 fused feasible");
    assert_eq!(fused_graph.paths.len(), 4, "2 diag + 2 offdiag routes");
    // plan 投影：对角路径带 baseline D=1，offdiag 路径不得带 baseline delay。
    let plan_paths = fused_graph.plan_paths();
    let diag = plan_paths
        .iter()
        .find(|path| path.out_index == 0 && path.in_index == 0)
        .expect("diag (out0,in0)");
    assert_eq!(diag.delay_samples, 1);
    assert!(diag.flags & PATH_FLAG_DIAG_BASELINE_INCLUDED != 0);
    let offdiag = plan_paths
        .iter()
        .find(|path| path.out_index == 0 && path.in_index == 1)
        .expect("offdiag (out0,in1)");
    assert_eq!(offdiag.delay_samples, 0, "offdiag has no delta_D baseline");
    assert!(offdiag.flags & PATH_FLAG_DIAG_BASELINE_INCLUDED == 0);
    let fused = eval_fused(&prep, &plans, &fixture.input);
    assert_close_2d(
        "C07 fused vs reference expected",
        &fused,
        &fixture.expected,
        1e-12,
    );
    assert_close_2d("C07 fused vs factorized(direct)", &fused, &direct, 1e-12);

    // 长度：L_A=1、L_G=2、L_P=2 -> L_F=3、full=3。
    assert_eq!(plans.report.fused_len_gp, 3);
    assert_eq!(plans.report.full_path_len, 3);
    // 成本相等（12 vs 12）-> 保留 factorized，不假称融合省算。
    assert_eq!(plans.report.factorized.macs_per_sample, 12);
    assert_eq!(plans.report.fused.macs_per_sample, 12);
    assert_eq!(plans.report.chosen, FusionForm::Factorized);
    assert_eq!(plans.report.reason, SelectionReason::FusedNotCheaper);
    assert_eq!(selected_form(&plans), FusionForm::Factorized);

    // factorized direct 的 block-size 不变性（T03 §3.3）。
    for block in [1usize, 2, 3, 64] {
        let again = eval_factorized_direct(&prep, &fixture.input, block);
        assert_exact_2d(
            &format!("C07 factorized block={block}"),
            &again,
            &fixture.expected,
        );
    }
}

#[test]
fn fusion_c09_5x5_embed_reference() {
    // T03 reference fixture C09：4.1.2 七角色、5x5 低带嵌入、D=4、scatter [0,1,2,3,4,5,7]。
    let fixture = load_fixture(C09_JSON);
    assert!(fixture.id.contains("C09"));
    let prep = prepare_fixture(&fixture);
    let plans = compile_ok(&prep, &CompileOptions::default());

    let direct = eval_factorized_direct(&prep, &fixture.input, 64);
    assert_exact_2d(
        "C09 factorized vs reference expected",
        &direct,
        &fixture.expected,
    );

    let fused_graph = plans.fused.as_ref().expect("C09 fused feasible");
    // 7 条对角 baseline + 4 条非零 offdiag cell（(0,1),(1,3),(2,0),(4,1)）= 11 条 F 路径。
    assert_eq!(fused_graph.paths.len(), 11);
    let fused = eval_fused(&prep, &plans, &fixture.input);
    assert_close_2d(
        "C09 fused vs reference expected",
        &fused,
        &fixture.expected,
        1e-12,
    );
    assert_close_2d("C09 fused vs factorized(direct)", &fused, &direct, 1e-12);

    assert_eq!(plans.report.route_count, 11);
    assert_eq!(plans.report.fused_len_gp, 6);
    assert_eq!(plans.report.full_path_len, 6);
    // factor = 11*5 + 7*2 = 69；fused = 11*6 = 66 -> fused 真省 3 MAC/sample。
    assert_eq!(plans.report.factorized.macs_per_sample, 69);
    assert_eq!(plans.report.fused.macs_per_sample, 66);
    assert_eq!(plans.report.chosen, FusionForm::FusedGp);
    assert_eq!(plans.report.reason, SelectionReason::FusedCheaper);
    assert_eq!(plans.report.saved_macs_per_sample, 3);
    assert_eq!(selected_form(&plans), FusionForm::FusedGp);
    assert_eq!(plans.report.fused_len_gp, 6);

    // fused 路径必须区分对角 baseline 与非对角 g 路径（flags 与 diag_baseline）。
    let diag_count = fused_graph
        .paths
        .iter()
        .filter(|path| path.diag_baseline)
        .count();
    assert_eq!(diag_count, 7);
    let plan_paths = fused_graph.plan_paths();
    assert_eq!(
        plan_paths
            .iter()
            .find(|path| path.out_index == 0 && path.in_index == 0)
            .expect("diag (out0,in0)")
            .delay_samples,
        4
    );
    assert_eq!(
        plan_paths
            .iter()
            .find(|path| path.out_index == 1 && path.in_index == 3)
            .expect("offdiag (out1,in3)")
            .delay_samples,
        0
    );

    // 容器 ch6 未使用 -> 严格零；scatter 映射到 ch7 的 SUB1 有非零输出。
    let ch6 = &fused[6];
    assert!(
        ch6.iter().all(|v| *v == 0.0),
        "unused container ch6 must be +0.0"
    );
    let ch7 = &fused[7];
    assert!(
        ch7.iter().any(|v| *v != 0.0),
        "SUB1 on container ch7 must be nonzero"
    );
}

#[test]
fn fusion_row_column_delay_polarity() {
    // 行列方向：输入侧 delay/polarity 作用列（A_i），输出侧作用行（p_j）；P·G != G·P。
    fn prepared_rowcol() -> Prepared {
        let mut prep = prepared_2x2();
        prep.g = vec![0.0; 2 * 2 * 2];
        set_g_cell(&mut prep, 0, 1, &[0.5]);
        set_g_cell(&mut prep, 1, 0, &[0.25]);
        prep
    }
    let x = vec![vec![1.0, 0.0], vec![0.0, 1.0]];

    // 输入侧 delay：a0 = z^-1（源 0 延迟 1），A 作用列。
    let mut input_delay = prepared_rowcol();
    set_a_row(&mut input_delay, 0, &[0.0, 1.0]);
    // 输出侧 delay：p0 = z^-1（角色 0 延迟 1），P 作用行。
    let mut output_delay = prepared_rowcol();
    set_p_row(&mut output_delay, 0, &[0.0, 1.0]);

    let plans_in = compile_ok(&input_delay, &CompileOptions::default());
    let plans_out = compile_ok(&output_delay, &CompileOptions::default());
    let direct_in = eval_factorized_direct(&input_delay, &x, 64);
    let direct_out = eval_factorized_direct(&output_delay, &x, 64);
    let fused_in = eval_fused(&input_delay, &plans_in, &x);
    let fused_out = eval_fused(&output_delay, &plans_out, &x);
    assert_close_2d("input-delay fused vs direct", &fused_in, &direct_in, 1e-12);
    assert_close_2d(
        "output-delay fused vs direct",
        &fused_out,
        &direct_out,
        1e-12,
    );

    // 手算（G 对角 baseline + 0.5/0.25 offdiag、P 声明支撑 2）：
    // 输入延迟：u0=u1=δ_1 -> y0[1]=1.5、y1[1]=1.25；
    // 输出延迟：y0[1]=1、y0[2]=0.5；y1[0]=0.25、y1[1]=1。
    assert!((direct_in[0][1] - 1.5).abs() < 1e-12);
    assert!((direct_in[1][1] - 1.25).abs() < 1e-12);
    assert!((direct_out[0][1] - 1.0).abs() < 1e-12);
    assert!((direct_out[0][2] - 0.5).abs() < 1e-12);
    assert!((direct_out[1][0] - 0.25).abs() < 1e-12);
    assert!((direct_out[1][1] - 1.0).abs() < 1e-12);

    // 行/列不可交换：把同一 delay 从列搬到行，输出不同。
    let mut differs = false;
    for ch in 0..2 {
        for n in 0..direct_in[ch].len().min(direct_out[ch].len()) {
            if (direct_in[ch][n] - direct_out[ch][n]).abs() > 1e-12 {
                differs = true;
            }
        }
    }
    assert!(
        differs,
        "column delay vs row delay must differ for offdiag G"
    );

    // 极性：输入侧 -1 翻一列；输出侧 -1 翻一行（有 offdiag 时结果不同）。
    let mut input_pol = prepared_rowcol();
    set_a_row(&mut input_pol, 0, &[-1.0, 0.0]);
    let mut output_pol = prepared_rowcol();
    set_p_row(&mut output_pol, 0, &[-1.0, 0.0]);
    let plans_ip = compile_ok(&input_pol, &CompileOptions::default());
    let plans_op = compile_ok(&output_pol, &CompileOptions::default());
    let direct_ip = eval_factorized_direct(&input_pol, &x, 64);
    let direct_op = eval_factorized_direct(&output_pol, &x, 64);
    let fused_ip = eval_fused(&input_pol, &plans_ip, &x);
    let fused_op = eval_fused(&output_pol, &plans_op, &x);
    assert_close_2d(
        "input-polarity fused vs direct",
        &fused_ip,
        &direct_ip,
        1e-12,
    );
    assert_close_2d(
        "output-polarity fused vs direct",
        &fused_op,
        &direct_op,
        1e-12,
    );
    // 输入极性：y0=[-1,0.5,0,...]、y1=[-0.25,1,...]；输出极性：y0=[-1,-0.5,0,...]、y1=[0.25,1,...]。
    assert!((direct_ip[0][0] + 1.0).abs() < 1e-12);
    assert!((direct_ip[0][1] - 0.5).abs() < 1e-12);
    assert!((direct_ip[1][0] + 0.25).abs() < 1e-12);
    assert!((direct_op[0][0] + 1.0).abs() < 1e-12);
    assert!((direct_op[0][1] + 0.5).abs() < 1e-12);
    assert!((direct_op[1][0] - 0.25).abs() < 1e-12);
    let mut pol_differs = false;
    for ch in 0..2 {
        for n in 0..direct_ip[ch].len() {
            if (direct_ip[ch][n] - direct_op[ch][n]).abs() > 1e-12 {
                pol_differs = true;
            }
        }
    }
    assert!(
        pol_differs,
        "input vs output polarity must differ for offdiag G"
    );
}

#[test]
fn fusion_zero_padding_linear_vs_circular() {
    // T04 V10 / T03 H11：h=[1,2,3,2,1]/9，B=8，x 在 n=0 与 n=7 各一个冲激。
    // 短 FFT 循环卷积 y[0]=1/3；正确线性卷积 y[0]=1/9（差 2/9）。
    let h = vec![1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];
    let mut x = vec![0.0; 8];
    x[0] = 1.0;
    x[7] = 1.0;
    let linear = convolve_linear(&h, &x);
    assert!((linear[0] - 1.0 / 9.0).abs() < 1e-15);
    let circular = oracle_spectral_convolve_circular(&h, &x, 8);
    assert!((circular[0] - 1.0 / 3.0).abs() < 1e-12);
    assert!((circular[0] - linear[0] - 2.0 / 9.0).abs() < 1e-12);
    assert!(
        (circular[0] - linear[0]).abs() > 0.2,
        "short circular FFT must alias (trap)"
    );

    // zero-padded 频谱卷积 == 精确线性卷积（全序列 tolerance 1e-12）。
    assert_eq!(next_pow2(12), 16);
    assert_eq!(next_pow2(1), 1);
    assert_eq!(next_pow2(16), 16);
    assert_eq!(next_pow2(17), 32);
    let spectral = oracle_spectral_convolve_zero_padded(&h, &x);
    assert_eq!(spectral.len(), linear.len());
    for (a, b) in spectral.iter().zip(linear.iter()) {
        assert!((a - b).abs() < 1e-12, "zero-padded spectral conv mismatch");
    }

    // fused 系数必须等于 zero-padded 频谱卷积（不能是短循环卷积）。
    let mut prep = prepared_2x2();
    prep.l_g = 3;
    prep.d_samples = 1;
    prep.g = vec![0.0; 2 * 2 * 3];
    set_g_cell(&mut prep, 1, 0, &[0.25, -0.5, 0.125]);
    prep.l_p = 3;
    prep.p = vec![0.0; 2 * 3];
    set_p_row(&mut prep, 1, &[1.0, -1.0, 0.5]);
    set_p_row(&mut prep, 0, &[1.0]);
    let plans = compile_ok(&prep, &CompileOptions::default());
    let fused = plans.fused.as_ref().expect("fused feasible");
    let offdiag = fused
        .paths
        .iter()
        .find(|path| path.out_index == 1 && path.in_index == 0)
        .expect("offdiag path (out1,in0)");
    let g_route = vec![0.25, -0.5, 0.125];
    let p_row = vec![1.0, -1.0, 0.5];
    let spectral = oracle_spectral_convolve_zero_padded(&p_row, &g_route);
    assert_eq!(offdiag.coeffs.len(), 5);
    for (a, b) in offdiag.coeffs.iter().zip(spectral.iter()) {
        assert!(
            (a - b).abs() < 1e-12,
            "fused coeffs must be zero-padded spectral conv"
        );
    }
    // n=4 的短循环卷积会把 p2*g2=0.0625 折到 index 0 -> 与线性结果不同（trap）。
    let circular = oracle_spectral_convolve_circular(&p_row, &g_route, 4);
    assert!((circular[0] - spectral[0] - 0.0625).abs() < 1e-12);
    assert!((circular[0] - spectral[0]).abs() > 0.01);
}

#[test]
fn fusion_plan_projection_metadata() {
    // plan 投影：fused 路径按 (out_index, in_index) 升序；flags/delay/polarity 口径固定。
    let mut prep = Prepared {
        n_src: 2,
        n_role: 2,
        container_channels: 2,
        d_samples: 0,
        participants: vec![0],
        src_of_role: vec![0, 1],
        scatter: vec![0, 1],
        a: vec![1.0, 1.0],
        l_a: 1,
        g: vec![0.0; 1],
        l_g: 1,
        p: vec![1.0, 0.0, 1.0, 0.0],
        l_p: 2,
    };
    set_p_row(&mut prep, 0, &[1.0]);
    set_p_row(&mut prep, 1, &[1.0]);
    let plans = compile_ok(&prep, &CompileOptions::default());
    assert_eq!(plans.report.chosen, FusionForm::FusedGp);
    let fused = plans.fused.as_ref().expect("fused feasible");
    let paths = fused.plan_paths();
    assert_eq!(paths.len(), 2);
    assert_eq!((paths[0].out_index, paths[0].in_index), (0, 0));
    assert_eq!((paths[1].out_index, paths[1].in_index), (1, 1));
    for path in &paths {
        assert_eq!(path.taps, 2, "L_F = 1 + 2 - 1");
        assert_eq!(path.delay_samples, 0);
        assert_eq!(path.delay_q16, 0);
        assert_eq!(path.polarity, 1, "polarity folded into coefficients");
        assert_eq!(
            path.flags,
            PATH_FLAG_FUSED | PATH_FLAG_ZERO_PAD_LINEAR_CONV | PATH_FLAG_DIAG_BASELINE_INCLUDED
        );
        assert_eq!(path.coeffs_f32, vec![1.0, 0.0]);
    }

    // factorized stage plan：A 每源一条、G 每条 route 一条（含 baseline）、P 每角色一条。
    let stage_in = plans.factorized.input_stage_plans();
    assert_eq!(stage_in.len(), 2);
    assert_eq!(stage_in[0].taps, 1);
    assert_eq!(stage_in[0].coeffs_f32, vec![1.0]);
    let stage_matrix = plans.factorized.matrix_stage_plans();
    assert_eq!(stage_matrix.len() as u32, plans.report.route_count);
    assert!(stage_matrix.iter().all(|route| route.taps == 1));
    assert!(stage_matrix.iter().all(|route| route.delay_samples == 0));
    assert!(stage_matrix.iter().any(|route| route.diag_baseline));
    let stage_out = plans.factorized.output_stage_plans();
    assert_eq!(stage_out.len(), 2);
    assert!(stage_out.iter().all(|stage| stage.taps == 2));
}

#[test]
fn fusion_selected_view_and_cost_consistency() {
    // report.chosen 与 selected() 一致；成本估计自洽（macs = 各阶段之和；routes 对齐）。
    let mut prep = prepared_2x2();
    set_g_cell(&mut prep, 0, 1, &[0.5]);
    set_g_cell(&mut prep, 1, 0, &[0.25]);
    let plans = compile_ok(&prep, &CompileOptions::default());
    let report = &plans.report;
    for cost in [report.factorized, report.fused] {
        assert_eq!(
            cost.macs_per_sample,
            cost.input_stage_taps + cost.matrix_stage_taps + cost.output_stage_taps
        );
        assert!(cost.routes as usize <= 2 * prep.n_role as usize);
        assert!(cost.coeff_bytes_f32 > 0);
        assert!(cost.peak_taps >= 1);
        assert!(cost.partitioned.fft_size == 256);
        assert!(cost.partitioned.transforms_per_block >= 2);
        assert!(cost.partitioned.max_partitions >= 1);
        assert!(cost.partitioned.real_flops_per_second >= 0.0);
    }
    match plans.selected() {
        SelectedPlan::Factorized(graph) => {
            assert_eq!(report.chosen, FusionForm::Factorized);
            assert_eq!(graph.l_g, prep.l_g);
        }
        SelectedPlan::Fused(graph) => {
            assert_eq!(report.chosen, FusionForm::FusedGp);
            assert_eq!(graph.l_f, report.fused_len_gp);
        }
    }
}
