//! T07 direct kernel 私有测试（WBS T07；写域 crates/core/tests/T07/）。
//!
//! 自包含（无第三方依赖）JSON 读取 + T03 reference fixtures 逐样本对拍：
//! - f64 oracle 入口：13 个 exact 夹具逐位一致 + C11 tolerance (rtol=1e-12)；
//! - f32 RT 入口：13 个 exact 夹具逐位一致 + C11 按 T07 f32 工程门；
//! - 所有夹具按 streaming.block_sizes 验证 block-size 不变性（跨 block 大小逐位一致）；
//! - identity baseline / 5x5 offdiag / 行列方向 / 陷阱 / valid_frames / stride / 负向；
//! - 计数分配器：RT f32 入口 8 block + partial/DRAIN 路径 0 分配（与 T08 同口径）。
//!
//! Cargo 自动把 tests/T07/main.rs 发现为集成测试 target（不需要 public_registration 注册）。
//! 运行：cargo test -p khaen-core direct -- --nocapture

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use khaen_core::direct::{
    direct_block_f32, direct_block_f64, direct_state_zero_f32, direct_state_zero_f64,
    DirectCoeffsF32, DirectCoeffsF64, DirectLayout, DirectTopology, DIRECT_MAX_BLOCK,
    DIRECT_MAX_CONTAINERS, DIRECT_MAX_PARTICIPANTS, DIRECT_MAX_ROLES, DIRECT_MAX_SOURCES,
    DIRECT_MAX_TAPS, DIRECT_NO_SOURCE,
};
use khaen_core::rt::RtStatus;

/// T07 冻结的 f32 工程门（T03 SPEC §4.2 + docs/E2E-ACCEPTANCE.md 待冻结默认；T07 定义并记录）。
const F32_REL_L2_MAX: f64 = 1e-5;
const F32_MAX_ABS_FACTOR: f64 = 1e-5;

const FIXTURE_CASES: [&str; 14] = [
    include_str!("../../../../reference/fixtures/cases/C01_identity_d0_1x1.json"),
    include_str!("../../../../reference/fixtures/cases/C02_identity_delay_d3.json"),
    include_str!("../../../../reference/fixtures/cases/C03_offdiag_2x2.json"),
    include_str!("../../../../reference/fixtures/cases/C04_tail_conv_1x1.json"),
    include_str!("../../../../reference/fixtures/cases/C05_d_in_support.json"),
    include_str!("../../../../reference/fixtures/cases/C06_row_col_asymmetry.json"),
    include_str!("../../../../reference/fixtures/cases/C07_factorized_fused.json"),
    include_str!("../../../../reference/fixtures/cases/C08_stereo_strict_scatter.json"),
    include_str!("../../../../reference/fixtures/cases/C09_4_1_2_5x5_embed.json"),
    include_str!("../../../../reference/fixtures/cases/C10_streaming_128.json"),
    include_str!("../../../../reference/fixtures/cases/C11_tolerance_f64.json"),
    include_str!("../../../../reference/fixtures/cases/C13_identity_graph_4_1_2_d2.json"),
    include_str!("../../../../reference/fixtures/cases/C14_input_column_fir.json"),
    include_str!("../../../../reference/fixtures/cases/C12_dense_5x5_D3_exact.json"),
];

const FIXTURE_INDEX: &str = include_str!("../../../../reference/fixtures/index.json");

// ---------------------------------------------------------------------------
// RT 路径零分配计数分配器（测试私有；只看当前测试线程的分配次数）
// ---------------------------------------------------------------------------
//
// 测试 target 内各测试各跑在独立线程；线程局部计数保证互不干扰。Cell<u64> 的 const
// 初始化不分配，try_with 在 TLS 销毁期安全忽略（与 T08 partitioned 测试同口径）。

thread_local! {
    static ALLOC_COUNT: Cell<u64> = const { Cell::new(0) };
}

struct CountingAllocator;

fn note_alloc() {
    let _ = ALLOC_COUNT.try_with(|cell| {
        cell.set(cell.get() + 1);
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            note_alloc();
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = System.realloc(ptr, layout, new_size);
        if !out.is_null() {
            note_alloc();
        }
        out
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let out = System.alloc_zeroed(layout);
        if !out.is_null() {
            note_alloc();
        }
        out
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn alloc_count() -> u64 {
    ALLOC_COUNT.with(|cell| cell.get())
}

fn alloc_counter_warmup() {
    let _ = alloc_count();
    let _ = alloc_count();
}

// ---------------------------------------------------------------------------
// 极简 JSON 读取（仅覆盖 fixture/index 用到的子集；含 UTF-8 字符串与基本转义）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Json {
    Null,
    #[allow(dead_code)]
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

struct JsonReader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonReader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn parse(mut self) -> Json {
        let value = self.value();
        self.skip_ws();
        if self.pos != self.bytes.len() {
            panic!("JSON trailing bytes at offset {}", self.pos);
        }
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
        if self.pos >= self.bytes.len() {
            panic!("JSON unexpected end at offset {}", self.pos);
        }
        self.bytes[self.pos]
    }

    fn expect(&mut self, want: u8) {
        self.skip_ws();
        let got = self.peek();
        if got != want {
            panic!(
                "JSON expected byte {} got {} at offset {}",
                want, got, self.pos
            );
        }
        self.pos += 1;
    }

    fn literal(&mut self, want: &str) {
        if !self.text[self.pos..].starts_with(want) {
            panic!("JSON literal mismatch at offset {}", self.pos);
        }
        self.pos += want.len();
    }

    fn value(&mut self) -> Json {
        self.skip_ws();
        match self.peek() {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Json::Str(self.string()),
            b't' => {
                self.literal("true");
                Json::Bool(true)
            }
            b'f' => {
                self.literal("false");
                Json::Bool(false)
            }
            b'n' => {
                self.literal("null");
                Json::Null
            }
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Json {
        self.expect(b'{');
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == b'}' {
            self.pos += 1;
            return Json::Obj(entries);
        }
        loop {
            self.skip_ws();
            let key = self.string();
            self.expect(b':');
            let value = self.value();
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Json::Obj(entries);
                }
                other => panic!(
                    "JSON object expected , or }} got {} at offset {}",
                    other, self.pos
                ),
            }
        }
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
            let value = self.value();
            items.push(value);
            self.skip_ws();
            match self.peek() {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Json::Arr(items);
                }
                other => panic!(
                    "JSON array expected , or ] got {} at offset {}",
                    other, self.pos
                ),
            }
        }
    }

    fn number(&mut self) -> Json {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9' => self.pos += 1,
                _ => break,
            }
        }
        let slice = &self.text[start..self.pos];
        match slice.parse::<f64>() {
            Ok(value) => Json::Num(value),
            Err(_) => panic!("JSON invalid number {:?}", slice),
        }
    }

    fn string(&mut self) -> String {
        self.expect(b'"');
        let mut out = String::new();
        loop {
            let byte = self.peek();
            self.pos += 1;
            match byte {
                b'"' => return out,
                b'\\' => {
                    let esc = self.peek();
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let hex = self.peek();
                                self.pos += 1;
                                let digit = match hex {
                                    b'0'..=b'9' => (hex - b'0') as u32,
                                    b'a'..=b'f' => (hex - b'a' + 10) as u32,
                                    b'A'..=b'F' => (hex - b'A' + 10) as u32,
                                    _ => panic!("JSON bad unicode escape at offset {}", self.pos),
                                };
                                code = code * 16 + digit;
                            }
                            match char::from_u32(code) {
                                Some(ch) => out.push(ch),
                                None => panic!("JSON bad unicode codepoint"),
                            }
                        }
                        _ => panic!("JSON bad escape at offset {}", self.pos),
                    }
                }
                _ if byte < 0x80 => out.push(byte as char),
                _ => {
                    let rest = &self.text[self.pos - 1..];
                    let ch = match rest.chars().next() {
                        Some(ch) => ch,
                        None => panic!("JSON bad UTF-8"),
                    };
                    out.push(ch);
                    self.pos += ch.len_utf8() - 1;
                }
            }
        }
    }
}

impl Json {
    fn as_object(&self) -> &[(String, Json)] {
        match self {
            Json::Obj(entries) => entries,
            _ => panic!("JSON expected object, got {:?}", self.kind()),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "bool",
            Json::Num(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }

    fn get(&self, key: &str) -> &Json {
        for (name, value) in self.as_object() {
            if name == key {
                return value;
            }
        }
        panic!("JSON missing key {:?}", key)
    }

    fn get_opt(&self, key: &str) -> Option<&Json> {
        for (name, value) in self.as_object() {
            if name == key {
                return Some(value);
            }
        }
        None
    }

    fn as_array(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => panic!("JSON expected array, got {:?}", self.kind()),
        }
    }

    fn as_real(&self) -> f64 {
        match self {
            Json::Num(value) => *value,
            _ => panic!("JSON expected number, got {:?}", self.kind()),
        }
    }

    fn as_usize(&self) -> usize {
        let value = self.as_real();
        if value < 0.0 {
            panic!("JSON expected non-negative integer, got {}", value);
        }
        value as usize
    }

    fn as_str(&self) -> &str {
        match self {
            Json::Str(value) => value,
            _ => panic!("JSON expected string, got {:?}", self.kind()),
        }
    }

    #[allow(dead_code)]
    fn as_bool(&self) -> bool {
        match self {
            Json::Bool(value) => *value,
            _ => panic!("JSON expected bool, got {:?}", self.kind()),
        }
    }

    fn as_real_vec(&self) -> Vec<f64> {
        self.as_array().iter().map(|item| item.as_real()).collect()
    }

    fn as_real_matrix(&self) -> Vec<Vec<f64>> {
        self.as_array()
            .iter()
            .map(|row| row.as_real_vec())
            .collect()
    }

    fn as_usize_vec(&self) -> Vec<usize> {
        self.as_array().iter().map(|item| item.as_usize()).collect()
    }
}

// ---------------------------------------------------------------------------
// Fixture 读取
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Fixture {
    id: String,
    tags: Vec<String>,
    mode: String,
    atol: f64,
    rtol: f64,
    n_src: usize,
    n_role: usize,
    container: usize,
    d: u32,
    m: usize,
    l_a: usize,
    l_g: usize,
    l_p: usize,
    l_f: usize,
    l_out: usize,
    expected_len: usize,
    participants: Vec<usize>,
    scatter: Vec<usize>,
    a_map: Vec<Vec<f64>>,
    a_fir: Vec<Vec<f64>>,
    g: Vec<Vec<Vec<f64>>>,
    p: Vec<Vec<f64>>,
    input: Vec<Vec<f64>>,
    expected: Vec<Vec<f64>>,
    block_sizes: Vec<usize>,
    flush_zeros: usize,
}

impl Fixture {
    fn parse(text: &str) -> Self {
        let root = JsonReader::new(text).parse();
        if root.get("schema").as_str() != "khaen-reference-fixture/v1" {
            panic!("unexpected fixture schema");
        }
        let graph = root.get("graph");
        let supports = root.get("supports");
        let comparison = root.get("comparison");
        let streaming = root.get("streaming");
        let mode = comparison.get("mode").as_str().to_string();
        let atol = comparison
            .get_opt("atol")
            .map(|v| v.as_real())
            .unwrap_or(0.0);
        let rtol = comparison
            .get_opt("rtol")
            .map(|v| v.as_real())
            .unwrap_or(0.0);
        let g_matrix = graph.get("g");
        let mut g = Vec::new();
        for row in g_matrix.as_array() {
            let mut cells = Vec::new();
            for cell in row.as_array() {
                cells.push(cell.as_real_vec());
            }
            g.push(cells);
        }
        Self {
            id: root.get("id").as_str().to_string(),
            tags: root
                .get("tags")
                .as_array()
                .iter()
                .map(|item| item.as_str().to_string())
                .collect(),
            mode,
            atol,
            rtol,
            n_src: graph.get("n_src").as_usize(),
            n_role: graph.get("n_out").as_usize(),
            container: graph.get("container_channels").as_usize(),
            d: graph.get("d_samples").as_usize() as u32,
            m: graph.get("participants").as_usize_vec().len(),
            l_a: supports.get("l_a").as_usize(),
            l_g: supports.get("l_g").as_usize(),
            l_p: supports.get("l_p").as_usize(),
            l_f: supports.get("l_f").as_usize(),
            l_out: supports.get("l_out").as_usize(),
            expected_len: root.get("expected_len").as_usize(),
            participants: graph.get("participants").as_usize_vec(),
            scatter: graph.get("scatter").as_usize_vec(),
            a_map: graph.get("a_map").as_real_matrix(),
            a_fir: graph.get("a_fir").as_real_matrix(),
            g,
            p: graph.get("p").as_real_matrix(),
            input: root.get("input").as_real_matrix(),
            expected: root.get("expected").as_real_matrix(),
            block_sizes: streaming.get("block_sizes").as_usize_vec(),
            flush_zeros: streaming.get("flush_zeros").as_usize(),
        }
    }

    fn input_len(&self) -> usize {
        self.input[0].len()
    }

    fn topology(&self) -> DirectTopology {
        let mut src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
        for j in 0..self.n_role {
            let mut src = DIRECT_NO_SOURCE;
            for (i, value) in self.a_map[j].iter().enumerate() {
                if *value != 0.0 {
                    src = i as u16;
                }
            }
            src_of_role[j] = src;
        }
        let mut participants = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
        for (a, role) in self.participants.iter().enumerate() {
            participants[a] = *role as u16;
        }
        let mut scatter = [0u16; DIRECT_MAX_ROLES];
        for (j, container) in self.scatter.iter().enumerate() {
            scatter[j] = *container as u16;
        }
        DirectTopology {
            n_src: self.n_src as u16,
            n_role: self.n_role as u16,
            container_channels: self.container as u16,
            d_samples: self.d,
            l_a: self.l_a as u16,
            l_g: self.l_g as u16,
            l_p: self.l_p as u16,
            m: self.m as u16,
            src_of_role,
            participants,
            scatter,
        }
    }

    fn flat_coeffs(&self, zero_g: bool) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut a = vec![0.0f64; self.n_src * self.l_a];
        for i in 0..self.n_src {
            for (t, value) in self.a_fir[i].iter().enumerate() {
                if t >= self.l_a {
                    panic!("{}: a_fir tap exceeds l_a", self.id);
                }
                a[i * self.l_a + t] = *value;
            }
        }
        let mut g = vec![0.0f64; self.m * self.m * self.l_g];
        if !zero_g {
            for row in 0..self.m {
                for col in 0..self.m {
                    for (t, value) in self.g[row][col].iter().enumerate() {
                        if t >= self.l_g {
                            panic!("{}: g tap exceeds l_g", self.id);
                        }
                        g[(row * self.m + col) * self.l_g + t] = *value;
                    }
                }
            }
        }
        let mut p = vec![0.0f64; self.n_role * self.l_p];
        for j in 0..self.n_role {
            for (t, value) in self.p[j].iter().enumerate() {
                if t >= self.l_p {
                    panic!("{}: p tap exceeds l_p", self.id);
                }
                p[j * self.l_p + t] = *value;
            }
        }
        (a, g, p)
    }
}

fn load_fixtures() -> Vec<Fixture> {
    FIXTURE_CASES
        .iter()
        .map(|text| Fixture::parse(text))
        .collect()
}

// ---------------------------------------------------------------------------
// 运行器：把 fixture 输入按 block 流式喂入 kernel，返回前 l_out 样本
// ---------------------------------------------------------------------------

fn padded_total(input_len: usize, flush: usize, block: usize) -> usize {
    let padded = input_len + flush;
    ((padded + block - 1) / block) * block
}

fn run_f64(fx: &Fixture, block: usize, zero_g: bool) -> Vec<Vec<f64>> {
    let topo = fx.topology();
    let layout = match DirectLayout::build(&topo) {
        Ok(layout) => layout,
        Err(status) => panic!("{}: layout rejected with code {}", fx.id, status.code()),
    };
    let (a, g, p) = fx.flat_coeffs(zero_g);
    let coeffs = DirectCoeffsF64 {
        a_fir: &a,
        g: &g,
        p: &p,
    };
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let total = padded_total(fx.input_len(), fx.l_f - 1, block);
    let mut out = vec![vec![0.0f64; total]; fx.container];
    let mut in_block = vec![0.0f64; fx.n_src * block];
    let mut out_block = vec![0.0f64; fx.container * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..fx.n_src {
            for k in 0..block {
                let idx = base + k;
                in_block[i * block + k] = if idx < fx.input_len() {
                    fx.input[i][idx]
                } else {
                    0.0
                };
            }
        }
        let status = direct_block_f64(
            &topo,
            &layout,
            &coeffs,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        if !status.is_ok() {
            panic!(
                "{}: direct_block_f64 code {} at block {}",
                fx.id,
                status.code(),
                base
            );
        }
        for c in 0..fx.container {
            for k in 0..block {
                out[c][base + k] = out_block[c * block + k];
            }
        }
        base += block;
    }
    out.into_iter().map(|ch| ch[..fx.l_out].to_vec()).collect()
}

fn run_f32(fx: &Fixture, block: usize, zero_g: bool) -> Vec<Vec<f32>> {
    let topo = fx.topology();
    let layout = match DirectLayout::build(&topo) {
        Ok(layout) => layout,
        Err(status) => panic!("{}: layout rejected with code {}", fx.id, status.code()),
    };
    let (a, g, p) = fx.flat_coeffs(zero_g);
    let a32: Vec<f32> = a.iter().map(|v| *v as f32).collect();
    let g32: Vec<f32> = g.iter().map(|v| *v as f32).collect();
    let p32: Vec<f32> = p.iter().map(|v| *v as f32).collect();
    let coeffs = DirectCoeffsF32 {
        a_fir: &a32,
        g: &g32,
        p: &p32,
    };
    let mut state = vec![0.0f32; layout.state_samples()];
    direct_state_zero_f32(&mut state);
    let total = padded_total(fx.input_len(), fx.l_f - 1, block);
    let mut out = vec![vec![0.0f32; total]; fx.container];
    let mut in_block = vec![0.0f32; fx.n_src * block];
    let mut out_block = vec![0.0f32; fx.container * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..fx.n_src {
            for k in 0..block {
                let idx = base + k;
                in_block[i * block + k] = if idx < fx.input_len() {
                    fx.input[i][idx] as f32
                } else {
                    0.0
                };
            }
        }
        let status = direct_block_f32(
            &topo,
            &layout,
            &coeffs,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        if !status.is_ok() {
            panic!(
                "{}: direct_block_f32 code {} at block {}",
                fx.id,
                status.code(),
                base
            );
        }
        for c in 0..fx.container {
            for k in 0..block {
                out[c][base + k] = out_block[c * block + k];
            }
        }
        base += block;
    }
    out.into_iter().map(|ch| ch[..fx.l_out].to_vec()).collect()
}

// ---------------------------------------------------------------------------
// 比较器
// ---------------------------------------------------------------------------

fn peak_expected(fx: &Fixture) -> f64 {
    let mut peak = 0.0f64;
    for channel in &fx.expected {
        for value in channel {
            let mag = value.abs();
            if mag > peak {
                peak = mag;
            }
        }
    }
    peak
}

fn check_finite_f64(id: &str, got: &[Vec<f64>]) {
    for (c, channel) in got.iter().enumerate() {
        for (n, value) in channel.iter().enumerate() {
            if !value.is_finite() {
                panic!(
                    "{}: non-finite f64 output at container {} sample {}",
                    id, c, n
                );
            }
        }
    }
}

fn compare_exact(fx: &Fixture, got: &[Vec<f64>]) -> f64 {
    check_finite_f64(&fx.id, got);
    let mut worst = 0.0f64;
    for c in 0..fx.container {
        for n in 0..fx.l_out {
            let actual = got[c][n];
            let expected = fx.expected[c][n];
            let diff = (actual - expected).abs();
            if diff > worst {
                worst = diff;
            }
            if actual != expected && !(actual == 0.0 && expected == 0.0) {
                panic!(
                    "{}: exact mismatch container {} sample {}: got {:?} expected {:?}",
                    fx.id, c, n, actual, expected
                );
            }
        }
    }
    worst
}

fn compare_tolerance(fx: &Fixture, got: &[Vec<f64>]) -> (f64, f64) {
    check_finite_f64(&fx.id, got);
    let peak = peak_expected(fx);
    let scale = if peak > 1.0 { peak } else { 1.0 };
    let mut worst = 0.0f64;
    for c in 0..fx.container {
        for n in 0..fx.l_out {
            let err = (got[c][n] - fx.expected[c][n]).abs();
            if err > worst {
                worst = err;
            }
            let limit = fx.atol + fx.rtol * scale;
            if !(err <= limit) {
                panic!(
                    "{}: tolerance mismatch container {} sample {}: err {:e} > {:e}",
                    fx.id, c, n, err, limit
                );
            }
        }
    }
    (worst, peak)
}

/// f32 工程门（T07 冻结）：返回 (相对 L2, 最大绝对误差, peak_ref)。
fn check_f32_gate(fx: &Fixture, got: &[Vec<f32>]) -> (f64, f64) {
    let peak = peak_expected(fx);
    let scale = if peak > 1.0 { peak } else { 1.0 };
    let mut err_sq = 0.0f64;
    let mut ref_sq = 0.0f64;
    let mut max_abs = 0.0f64;
    for c in 0..fx.container {
        for n in 0..fx.l_out {
            let actual = got[c][n] as f64;
            if !actual.is_finite() {
                panic!(
                    "{}: non-finite f32 output at container {} sample {}",
                    fx.id, c, n
                );
            }
            let expected = fx.expected[c][n];
            let err = actual - expected;
            err_sq += err * err;
            ref_sq += expected * expected;
            let abs = err.abs();
            if abs > max_abs {
                max_abs = abs;
            }
        }
    }
    let rel_l2 = if ref_sq > 0.0 {
        (err_sq / ref_sq).sqrt()
    } else {
        0.0
    };
    if !(rel_l2 <= F32_REL_L2_MAX) {
        panic!(
            "{}: f32 relative L2 {:e} > {:e}",
            fx.id, rel_l2, F32_REL_L2_MAX
        );
    }
    if !(max_abs <= F32_MAX_ABS_FACTOR * scale) {
        panic!(
            "{}: f32 max abs {:e} > {:e}",
            fx.id,
            max_abs,
            F32_MAX_ABS_FACTOR * scale
        );
    }
    (rel_l2, max_abs)
}

// ---------------------------------------------------------------------------
// 构造 helpers（手算向量/边界用例）
// ---------------------------------------------------------------------------

fn topo_1x1(d: u32, l_g: u16) -> DirectTopology {
    let mut topo = DirectTopology::identity();
    topo.d_samples = d;
    topo.l_g = l_g;
    topo
}

fn run_direct_f64(
    topo: &DirectTopology,
    a_fir: &[f64],
    g: &[f64],
    p: &[f64],
    input: &[f64],
    in_len: usize,
    input_len: usize,
    block: usize,
) -> Vec<f64> {
    let layout = match DirectLayout::build(topo) {
        Ok(layout) => layout,
        Err(status) => panic!("layout rejected with code {}", status.code()),
    };
    let coeffs = DirectCoeffsF64 { a_fir, g, p };
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let total = padded_total(
        input_len,
        topo.l_a as usize + topo.l_g as usize + topo.l_p as usize - 3,
        block,
    );
    let mut out = vec![0.0f64; total];
    let mut in_block = vec![0.0f64; topo.n_src as usize * block];
    let mut out_block = vec![0.0f64; topo.container_channels as usize * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..topo.n_src as usize {
            for k in 0..block {
                let idx = base + k;
                in_block[i * block + k] = if idx < input_len { input[idx] } else { 0.0 };
            }
        }
        let status = direct_block_f64(
            topo,
            &layout,
            &coeffs,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        if !status.is_ok() {
            panic!("direct_block_f64 code {} at block {}", status.code(), base);
        }
        out[base..base + block].copy_from_slice(&out_block[..block]);
        base += block;
    }
    out[..in_len].to_vec()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[test]
fn direct_fixture_index_binding() {
    let index = JsonReader::new(FIXTURE_INDEX).parse();
    let cases = index.get("cases").as_array();
    assert_eq!(cases.len(), FIXTURE_CASES.len());
    let mut total_bytes = 0usize;
    for (slot, case) in cases.iter().enumerate() {
        let text = FIXTURE_CASES[slot];
        total_bytes += text.len();
        let fx = Fixture::parse(text);
        assert_eq!(case.get("id").as_str(), fx.id);
        assert_eq!(case.get("file").as_str(), format!("cases/{}.json", fx.id));
        assert_eq!(case.get("bytes").as_usize(), text.len());
        assert_eq!(case.get("comparison").as_str(), fx.mode);
        let sha = case.get("sha256").as_str();
        assert_eq!(sha.len(), 64);
        assert!(sha.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(fx.l_f, fx.l_a + fx.l_g + fx.l_p - 2);
        assert_eq!(fx.l_out, fx.input_len() + fx.l_f - 1);
        assert_eq!(fx.l_out, fx.expected_len);
        assert_eq!(fx.flush_zeros, fx.l_f - 1);
        assert_eq!(fx.participants.len(), fx.m);
        assert_eq!(fx.expected.len(), fx.container);
        assert!(fx.a_map.len() == fx.n_role);
        assert!(fx.a_fir.len() == fx.n_src);
        assert!(fx.p.len() == fx.n_role);
        assert!(fx.g.len() == fx.m);
        assert!(fx.scatter.len() == fx.n_role);
        for row in &fx.a_fir {
            assert!(row.len() <= fx.l_a);
        }
        for row in &fx.p {
            assert!(row.len() <= fx.l_p);
        }
        for row in &fx.g {
            assert_eq!(row.len(), fx.m);
            for cell in row {
                assert!(cell.len() <= fx.l_g);
            }
        }
        for channel in &fx.expected {
            assert_eq!(channel.len(), fx.l_out);
        }
        for channel in &fx.input {
            assert_eq!(channel.len(), fx.input_len());
        }
        assert!(!fx.block_sizes.is_empty());
        for block in &fx.block_sizes {
            assert!(*block >= 1 && *block <= DIRECT_MAX_BLOCK);
        }
        println!(
            "index OK {} bytes={} mode={} blocks={:?} tags={:?}",
            fx.id,
            text.len(),
            fx.mode,
            fx.block_sizes,
            fx.tags
        );
    }
    println!("fixture total bytes = {}", total_bytes);
    let ids: std::collections::BTreeSet<&str> =
        cases.iter().map(|case| case.get("id").as_str()).collect();
    assert_eq!(ids.len(), cases.len());
}

#[test]
fn direct_topology_and_layout_contract() {
    let identity = DirectTopology::identity();
    assert!(identity.validate().is_ok());
    let layout = DirectLayout::build(&identity).expect("identity layout");
    let expected_state = (DIRECT_MAX_SOURCES * 0
        + DIRECT_MAX_ROLES * 0
        + DIRECT_MAX_ROLES * 0
        + (1 + 2) * DIRECT_MAX_BLOCK) as u32;
    assert_eq!(layout.state_samples, expected_state);
    assert_eq!(layout.state_samples(), expected_state as usize);
    assert_eq!(layout.x_past_off, 0);
    assert_eq!(layout.u_past_off, layout.x_past_off + 0);
    assert_eq!(layout.v_past_off, layout.u_past_off + 0);
    assert_eq!(layout.x_cur_off, layout.v_past_off);
    assert_eq!(layout.u_cur_off, layout.x_cur_off + DIRECT_MAX_BLOCK as u32);
    assert_eq!(layout.v_cur_off, layout.u_cur_off + DIRECT_MAX_BLOCK as u32);
    assert_eq!(
        layout.state_samples,
        layout.v_cur_off + DIRECT_MAX_BLOCK as u32
    );

    // 5x5 最大常规支撑（L_A=1、L_G=4095、L_P=1 -> L_F=4095 <= 4096）布局公式。
    let mut big = identity;
    big.n_src = 32;
    big.n_role = 22;
    big.container_channels = 22;
    big.l_a = 1;
    big.l_g = 4095;
    big.l_p = 1;
    big.m = 8;
    for a in 0..8 {
        big.participants[a] = a as u16;
    }
    for j in 0..22 {
        big.src_of_role[j] = if j < 22 {
            (j % 32) as u16
        } else {
            DIRECT_NO_SOURCE
        };
        big.scatter[j] = j as u16;
    }
    assert!(big.validate().is_ok());
    let big_layout = DirectLayout::build(&big).expect("big layout");
    let expected_big = (32 * 0 + 22 * 4094 + 22 * 0 + (32 + 2 * 22) * DIRECT_MAX_BLOCK) as u32;
    assert_eq!(big_layout.state_samples, expected_big);
    assert_eq!(big_layout.u_past_off, 0);
    assert_eq!(big_layout.v_past_off, 22 * 4094);
    assert_eq!(big_layout.x_cur_off, 22 * 4094);
    assert_eq!(
        big_layout.u_cur_off,
        22 * 4094 + 32 * DIRECT_MAX_BLOCK as u32
    );

    // 负向：拓扑校验错误码。
    let mut bad = identity;
    bad.n_src = 0;
    assert_eq!(bad.validate(), RtStatus::ERR_CHANNELS);
    let mut bad = identity;
    bad.n_role = (DIRECT_MAX_ROLES + 1) as u16;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.container_channels = 0;
    assert_eq!(bad.validate(), RtStatus::ERR_CHANNELS);
    let mut bad = identity;
    bad.l_a = 0;
    assert_eq!(bad.validate(), RtStatus::ERR_TAPS);
    let mut bad = identity;
    bad.d_samples = 4;
    bad.l_g = 4;
    assert_eq!(bad.validate(), RtStatus::ERR_TAPS);
    let mut bad = identity;
    bad.l_a = 1;
    bad.l_g = 4096;
    bad.l_p = 3;
    assert_eq!(bad.validate(), RtStatus::ERR_TAPS);
    let mut bad = identity;
    bad.m = 0;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.m = (DIRECT_MAX_PARTICIPANTS + 1) as u16;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.participants[0] = 5;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.n_role = 2;
    bad.container_channels = 2;
    bad.participants[1] = 0;
    bad.m = 2;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.n_src = 2;
    bad.n_role = 2;
    bad.container_channels = 2;
    bad.src_of_role[1] = 0;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.n_role = 2;
    bad.container_channels = 2;
    bad.scatter[1] = 0;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
    let mut bad = identity;
    bad.n_role = 2;
    bad.container_channels = 2;
    bad.scatter[1] = 2;
    assert_eq!(bad.validate(), RtStatus::ERR_ROLES);
}

#[test]
fn direct_hand_calc_vectors_match_t03() {
    // HAND-CALC H1 / C01：G=delta_0，y == x。
    let h1 = run_direct_f64(
        &topo_1x1(0, 1),
        &[1.0],
        &[0.0],
        &[1.0],
        &[1.0, -2.0, 0.5, 0.0, 3.0],
        5,
        5,
        5,
    );
    assert_eq!(h1, vec![1.0, -2.0, 0.5, 0.0, 3.0]);

    // HAND-CALC H2 / C02：D=3、G=z^-3，输出 = 输入延迟 3。
    let h2 = run_direct_f64(
        &topo_1x1(3, 4),
        &[1.0],
        &[0.0; 4],
        &[1.0],
        &[1.0, 2.0, 3.0],
        6,
        3,
        6,
    );
    assert_eq!(h2, vec![0.0, 0.0, 0.0, 1.0, 2.0, 3.0]);

    // HAND-CALC H4 / C04：G=[1,2]、P=[1,-1,2] -> F=[1,1,0,4]，x=[1,1]。
    let topo = topo_1x1(0, 2);
    let mut topo = topo;
    topo.l_p = 3;
    let h4 = run_direct_f64(
        &topo,
        &[1.0],
        &[0.0, 2.0],
        &[1.0, -1.0, 2.0],
        &[1.0, 1.0, 0.0, 0.0, 0.0],
        5,
        2,
        5,
    );
    assert_eq!(h4, vec![1.0, 2.0, 1.0, 4.0, 4.0]);

    // HAND-CALC H13 / C13：4.1.2 identity graph、D=2、scatter -> 每角色延迟 2。
    let some = |mut src: [u16; DIRECT_MAX_ROLES]| {
        for j in 0..7 {
            src[j] = j as u16;
        }
        src
    };
    let mut src_of_role = some([DIRECT_NO_SOURCE; DIRECT_MAX_ROLES]);
    src_of_role[7] = DIRECT_NO_SOURCE;
    let mut participants = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
    for a in 0..5 {
        participants[a] = [0u16, 1, 2, 3, 6][a];
    }
    let mut scatter = [0u16; DIRECT_MAX_ROLES];
    for (j, c) in [0u16, 1, 2, 3, 4, 5, 7].iter().enumerate() {
        scatter[j] = *c;
    }
    let topo13 = DirectTopology {
        n_src: 7,
        n_role: 7,
        container_channels: 8,
        d_samples: 2,
        l_a: 1,
        l_g: 3,
        l_p: 1,
        m: 5,
        src_of_role,
        participants,
        scatter,
    };
    let mut input13 = Vec::new();
    for i in 0..7 {
        let v = (i + 1) as f64;
        input13.push(vec![v, -v, 0.0, 0.0, 2.0 * v, 0.0]);
    }
    let flat: Vec<f64> = {
        let mut out = Vec::new();
        for row in &input13 {
            out.extend_from_slice(row);
        }
        out
    };
    let layout = DirectLayout::build(&topo13).expect("h13 layout");
    let coeffs = DirectCoeffsF64 {
        a_fir: &[1.0; 7],
        g: &[0.0; 5 * 5 * 3],
        p: &[1.0; 7],
    };
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let block = 3usize;
    let total = padded_total(6, 2, block);
    let mut got = vec![vec![0.0f64; total]; 8];
    let mut in_block = vec![0.0f64; 7 * block];
    let mut out_block = vec![0.0f64; 8 * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..7 {
            for k in 0..block {
                let idx = base + k;
                in_block[i * block + k] = if idx < 6 { flat[i * 6 + idx] } else { 0.0 };
            }
        }
        let status = direct_block_f64(
            &topo13,
            &layout,
            &coeffs,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        for c in 0..8 {
            for k in 0..block {
                got[c][base + k] = out_block[c * block + k];
            }
        }
        base += block;
    }
    let expected13: [[f64; 8]; 8] = [
        [0.0, 0.0, 1.0, -1.0, 0.0, 0.0, 2.0, 0.0],
        [0.0, 0.0, 2.0, -2.0, 0.0, 0.0, 4.0, 0.0],
        [0.0, 0.0, 3.0, -3.0, 0.0, 0.0, 6.0, 0.0],
        [0.0, 0.0, 4.0, -4.0, 0.0, 0.0, 8.0, 0.0],
        [0.0, 0.0, 5.0, -5.0, 0.0, 0.0, 10.0, 0.0],
        [0.0, 0.0, 6.0, -6.0, 0.0, 0.0, 12.0, 0.0],
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 7.0, -7.0, 0.0, 0.0, 14.0, 0.0],
    ];
    for c in 0..8 {
        assert_eq!(&got[c][..8], &expected13[c][..]);
    }
}

#[test]
fn direct_identity_baseline_all_roles() {
    // G=0 -> G_full = z^-D * I：identity baseline 作用于全部角色（含非 participant 的
    // HFL/HFR），scatter 只改容器位置；未用容器 ch6 逐样本严格 0（T03 SPEC §2.8）。
    let mut src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
    for j in 0..7 {
        src_of_role[j] = j as u16;
    }
    let mut participants = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
    for (a, role) in [0u16, 1, 2, 3, 6].iter().enumerate() {
        participants[a] = *role;
    }
    let mut scatter = [0u16; DIRECT_MAX_ROLES];
    for (j, c) in [0u16, 1, 2, 3, 4, 5, 7].iter().enumerate() {
        scatter[j] = *c;
    }
    let topo = DirectTopology {
        n_src: 7,
        n_role: 7,
        container_channels: 8,
        d_samples: 4,
        l_a: 1,
        l_g: 5,
        l_p: 1,
        m: 5,
        src_of_role,
        participants,
        scatter,
    };
    assert!(topo.validate().is_ok());
    // 每源在第 2*i 个样本放 (i+1) 冲激；输入长 15，flush 4 -> L_out 19。
    let mut input = vec![0.0f64; 7 * 15];
    for i in 0..7 {
        input[i * 15 + 2 * i] = (i + 1) as f64;
    }
    let layout = DirectLayout::build(&topo).expect("layout");
    let coeffs = DirectCoeffsF64 {
        a_fir: &[1.0; 7],
        g: &[0.0; 5 * 5 * 5],
        p: &[1.0; 7],
    };
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let block = 5usize;
    let total = padded_total(15, 4, block);
    let mut got = vec![vec![0.0f64; total]; 8];
    let mut in_block = vec![0.0f64; 7 * block];
    let mut out_block = vec![0.0f64; 8 * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..7 {
            for k in 0..block {
                let idx = base + k;
                in_block[i * block + k] = if idx < 15 { input[i * 15 + idx] } else { 0.0 };
            }
        }
        let status = direct_block_f64(
            &topo,
            &layout,
            &coeffs,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        for c in 0..8 {
            for k in 0..block {
                got[c][base + k] = out_block[c * block + k];
            }
        }
        base += block;
    }
    let mut expected = vec![vec![0.0f64; total]; 8];
    for j in 0..7 {
        expected[scatter[j] as usize][2 * j + 4] = (j + 1) as f64;
    }
    for c in 0..8 {
        assert_eq!(got[c], expected[c], "container {} mismatch", c);
    }
    // 非 participant 角色 HFL/HFR（role 4/5）也必须按 baseline 直通。
    assert!(got[4][12] != 0.0 && got[5][14] != 0.0);
    // 未映射容器 ch6 严格 0。
    assert!(got[6].iter().all(|value| *value == 0.0));
    println!("identity baseline all roles OK: D=4, HFL/HFR passthrough, ch6 zero");
}

#[test]
fn direct_f64_fixtures_offline_expected() {
    let fixtures = load_fixtures();
    assert_eq!(fixtures.len(), 14);
    let mut exact_count = 0usize;
    let mut tolerance_count = 0usize;
    for fx in &fixtures {
        let block = if fx.l_out < DIRECT_MAX_BLOCK {
            fx.l_out
        } else {
            DIRECT_MAX_BLOCK
        };
        let got = run_f64(fx, block, false);
        if fx.mode == "exact" {
            let worst = compare_exact(fx, &got);
            exact_count += 1;
            println!("f64 exact PASS {} block={} worst={:e}", fx.id, block, worst);
        } else if fx.mode == "tolerance" {
            let (worst, peak) = compare_tolerance(fx, &got);
            tolerance_count += 1;
            println!(
                "f64 tolerance PASS {} block={} atol={:e} rtol={:e} peak={} worst={:e}",
                fx.id, block, fx.atol, fx.rtol, peak, worst
            );
        } else {
            panic!("{}: unknown comparison mode {}", fx.id, fx.mode);
        }
    }
    assert_eq!(exact_count, 13);
    assert_eq!(tolerance_count, 1);
    println!(
        "f64 fixtures: {} exact + {} tolerance PASS",
        exact_count, tolerance_count
    );
}

#[test]
fn direct_f64_streaming_block_invariance() {
    let fixtures = load_fixtures();
    for fx in &fixtures {
        let mut reference: Option<Vec<Vec<f64>>> = None;
        for block in &fx.block_sizes {
            let got = run_f64(fx, *block, false);
            if fx.mode == "exact" {
                compare_exact(fx, &got);
            } else {
                compare_tolerance(fx, &got);
            }
            if let Some(base) = &reference {
                for c in 0..fx.container {
                    for n in 0..fx.l_out {
                        if got[c][n] != base[c][n] && !(got[c][n] == 0.0 && base[c][n] == 0.0) {
                            panic!(
                                "{}: block-size invariance broke at B={} container {} sample {}",
                                fx.id, block, c, n
                            );
                        }
                    }
                }
            } else {
                reference = Some(got);
            }
        }
        println!("f64 streaming PASS {} blocks={:?}", fx.id, fx.block_sizes);
    }
}

#[test]
fn direct_f32_fixtures_gate_and_invariance() {
    let fixtures = load_fixtures();
    for fx in &fixtures {
        let mut reference: Option<Vec<Vec<f32>>> = None;
        let mut worst_rel = 0.0f64;
        let mut worst_abs = 0.0f64;
        for block in &fx.block_sizes {
            let got = run_f32(fx, *block, false);
            if fx.mode == "exact" {
                // 13 个 exact 夹具的系数/输入/期望都是 f32 精确可表示；f32 kernel 应逐位一致。
                for c in 0..fx.container {
                    for n in 0..fx.l_out {
                        let actual = got[c][n] as f64;
                        let expected = fx.expected[c][n];
                        if actual != expected && !(actual == 0.0 && expected == 0.0) {
                            panic!(
                                "{}: f32 exact mismatch B={} container {} sample {}: got {:?} expected {:?}",
                                fx.id, block, c, n, actual, expected
                            );
                        }
                    }
                }
            }
            let (rel_l2, max_abs) = check_f32_gate(fx, &got);
            if rel_l2 > worst_rel {
                worst_rel = rel_l2;
            }
            if max_abs > worst_abs {
                worst_abs = max_abs;
            }
            if let Some(base) = &reference {
                for c in 0..fx.container {
                    for n in 0..fx.l_out {
                        if got[c][n].to_bits() != base[c][n].to_bits()
                            && !(got[c][n] == 0.0 && base[c][n] == 0.0)
                        {
                            panic!(
                                "{}: f32 block-size invariance broke at B={} container {} sample {}",
                                fx.id, block, c, n
                            );
                        }
                    }
                }
            } else {
                reference = Some(got);
            }
        }
        println!(
            "f32 gate PASS {} mode={} blocks={:?} worst_rel_l2={:e} worst_abs={:e}",
            fx.id, fx.mode, fx.block_sizes, worst_rel, worst_abs
        );
    }
}

#[test]
fn direct_five_by_five_offdiag_required() {
    // C09（4.1.2 5x5 嵌入）与 C12（dense 5x5 D3）：M=5，且 offdiag g 必须真实参与——
    // 把 g 清零（只留 identity baseline）后输出必须改变，证明不是“只有直通”。
    let fixtures = load_fixtures();
    let mut covered = 0usize;
    for fx in &fixtures {
        if fx.m != 5 || fx.n_role < 5 {
            continue;
        }
        if fx.id != "C09_4_1_2_5x5_embed" && fx.id != "C12_dense_5x5_D3_exact" {
            continue;
        }
        covered += 1;
        let block = DIRECT_MAX_BLOCK;
        let with_g = run_f64(fx, block, false);
        compare_exact(fx, &with_g);
        let without_g = run_f64(fx, block, true);
        let mut diff = 0usize;
        for c in 0..fx.container {
            for n in 0..fx.l_out {
                if with_g[c][n] != without_g[c][n] {
                    diff += 1;
                }
            }
        }
        assert!(
            diff > 0,
            "{}: 5x5 offdiag g 未参与计算（g=0 输出完全相同）",
            fx.id
        );
        println!(
            "5x5 offdiag PASS {} M={} samples_changed_by_g={} peak={}",
            fx.id,
            fx.m,
            diff,
            peak_expected(fx)
        );
    }
    assert_eq!(covered, 2, "必须覆盖 C09 与 C12 两个 5x5 用例");
}

#[test]
fn direct_row_column_orientation_and_traps() {
    let fixtures = load_fixtures();
    let find = |id: &str| {
        fixtures
            .iter()
            .find(|fx| fx.id == id)
            .unwrap_or_else(|| panic!("missing fixture {}", id))
    };

    // C06/H6：P 必须作用在输出行（y=[6,35]），不是列缩放（trap=[12,26]）。
    let c06 = find("C06_row_col_asymmetry");
    let got06 = run_f64(c06, 1, false);
    compare_exact(c06, &got06);
    let trap06 = [12.0f64, 26.0];
    let expected06 = [6.0f64, 35.0];
    for c in 0..2 {
        let actual = got06[c][0];
        assert_eq!(actual, expected06[c]);
        assert_ne!(actual, trap06[c], "C06: 输出等于 G*P 列缩放陷阱值");
    }

    // C14/H12：A_i 必须作用在输入列（正确 [0,3,0]），不是输出侧（trap ch0 [0,0,-3]）。
    let c14 = find("C14_input_column_fir");
    let got14 = run_f64(c14, 2, false);
    compare_exact(c14, &got14);
    let trap14_ch0 = [0.0f64, 0.0, -3.0];
    let trap14_ch1 = [0.0f64, 6.0, 0.0];
    assert_eq!(got14[0], vec![0.0f64, 3.0, 0.0]);
    assert_eq!(got14[1], vec![0.0f64, 3.0, 0.0]);
    assert_ne!(
        &got14[0][..],
        &trap14_ch0[..],
        "C14: ch0 等于输出侧 FIR 陷阱值"
    );
    assert_ne!(
        &got14[1][..],
        &trap14_ch1[..],
        "C14: ch1 等于输出侧 FIR 陷阱值"
    );

    // C04/H11：direct kernel 是线性卷积，不得出现 4 点圆周混叠 [5,2,1,4]。
    let c04 = find("C04_tail_conv_1x1");
    let got04 = run_f64(c04, 5, false);
    compare_exact(c04, &got04);
    let trap04 = [5.0f64, 2.0, 1.0, 4.0];
    assert_ne!(&got04[0][..4], &trap04[..], "C04: 输出等于圆周卷积陷阱值");
    println!("orientation/traps PASS C06/C14/C04");
}

#[test]
fn direct_partial_valid_frames() {
    // valid_frames < block_size：只处理前 valid 个样本，其余输出 +0.0；历史按 valid 个有效
    // 样本推进（可拼接 partial：两个 V=2 块 == 一个 V=4 块，属 direct 扩展语义；T08 的
    // partial 是终止性 DRAIN，跨内核契约见 direct.rs 文件头“统一 partial/flush 契约”）。
    let topo = topo_1x1(0, 1);
    let layout = DirectLayout::build(&topo).expect("layout");
    let coeffs = DirectCoeffsF64 {
        a_fir: &[1.0],
        g: &[0.0],
        p: &[1.0],
    };
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let block = 8usize;
    let mut out_block = vec![-1.0f64; block];
    let mut in_block = vec![0.0f64; block];
    in_block[..3].copy_from_slice(&[1.0, 2.0, 3.0]);
    in_block[3..].copy_from_slice(&[9.0, 9.0, 9.0, 9.0, 9.0]);
    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &in_block,
        block,
        &mut out_block,
        block,
        block,
        3,
    );
    assert!(status.is_ok());
    assert_eq!(out_block, vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    in_block[..3].copy_from_slice(&[4.0, 5.0, 6.0]);
    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &in_block,
        block,
        &mut out_block,
        block,
        block,
        3,
    );
    assert!(status.is_ok());
    assert_eq!(out_block, vec![4.0, 5.0, 6.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

    // valid_frames == 0：只清零输出，state 不变。
    let before = state.clone();
    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &in_block,
        block,
        &mut out_block,
        block,
        block,
        0,
    );
    assert!(status.is_ok());
    assert!(out_block.iter().all(|value| *value == 0.0));
    assert_eq!(state, before);

    // 两个 V=2 的块与一个 V=4 的块等价（state 只随有效样本推进）。
    let topo_d = topo_1x1(3, 4);
    let layout_d = DirectLayout::build(&topo_d).expect("layout d");
    let coeffs_d = DirectCoeffsF64 {
        a_fir: &[1.0],
        g: &[0.0; 4],
        p: &[1.0],
    };
    let split_two = {
        let mut st = vec![0.0f64; layout_d.state_samples()];
        direct_state_zero_f64(&mut st);
        let mut outs = Vec::new();
        for chunk in [
            [1.0f64, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [3.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ] {
            let mut ob = vec![0.0f64; 8];
            let status = direct_block_f64(
                &topo_d, &layout_d, &coeffs_d, &mut st, &chunk, 8, &mut ob, 8, 8, 2,
            );
            assert!(status.is_ok());
            outs.extend_from_slice(&ob[..2]);
        }
        outs
    };
    let whole = {
        let mut st = vec![0.0f64; layout_d.state_samples()];
        direct_state_zero_f64(&mut st);
        let mut ob = vec![0.0f64; 8];
        let input = [1.0f64, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0];
        let status = direct_block_f64(
            &topo_d, &layout_d, &coeffs_d, &mut st, &input, 8, &mut ob, 8, 8, 4,
        );
        assert!(status.is_ok());
        ob[..4].to_vec()
    };
    assert_eq!(split_two, whole);
    println!("partial valid_frames PASS");
}

#[test]
fn direct_planar_strides_preserve_guard_samples() {
    let fixtures = load_fixtures();
    let fx = fixtures
        .iter()
        .find(|fx| fx.id == "C10_streaming_128")
        .expect("C10");
    let block = 64usize;
    let in_stride = block + 5;
    let out_stride = block + 7;
    let topo = fx.topology();
    let layout = DirectLayout::build(&topo).expect("layout");
    let (a, g, p) = fx.flat_coeffs(false);
    let coeffs = DirectCoeffsF64 {
        a_fir: &a,
        g: &g,
        p: &p,
    };
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let total = padded_total(fx.input_len(), fx.l_f - 1, block);
    let mut guarded = vec![-7.0f64; fx.container * out_stride];
    let mut in_block = vec![0.0f64; fx.n_src * in_stride];
    let mut base = 0usize;
    while base < total {
        for i in 0..fx.n_src {
            for k in 0..in_stride {
                let idx = base + k;
                in_block[i * in_stride + k] = if idx < fx.input_len() {
                    fx.input[i][idx]
                } else {
                    -3.0
                };
            }
        }
        let status = direct_block_f64(
            &topo,
            &layout,
            &coeffs,
            &mut state,
            &in_block,
            in_stride,
            &mut guarded,
            out_stride,
            block,
            block,
        );
        assert!(status.is_ok());
        // 每通道 [block, out_stride) 是 guard，必须保持哨兵值不被写。
        for c in 0..fx.container {
            for k in block..out_stride {
                assert_eq!(guarded[c * out_stride + k], -7.0);
            }
            for k in 0..block {
                assert!(guarded[c * out_stride + k].is_finite());
            }
        }
        base += block;
    }
    // 与非 stride（连续）运行逐位一致。
    let contiguous = {
        let mut got = vec![vec![0.0f64; total]; fx.container];
        let mut state2 = vec![0.0f64; layout.state_samples()];
        direct_state_zero_f64(&mut state2);
        let mut in2 = vec![0.0f64; fx.n_src * block];
        let mut out2 = vec![0.0f64; fx.container * block];
        let mut pos = 0usize;
        while pos < total {
            for i in 0..fx.n_src {
                for k in 0..block {
                    let idx = pos + k;
                    in2[i * block + k] = if idx < fx.input_len() {
                        fx.input[i][idx]
                    } else {
                        0.0
                    };
                }
            }
            let status = direct_block_f64(
                &topo,
                &layout,
                &coeffs,
                &mut state2,
                &in2,
                block,
                &mut out2,
                block,
                block,
                block,
            );
            assert!(status.is_ok());
            for c in 0..fx.container {
                for k in 0..block {
                    got[c][pos + k] = out2[c * block + k];
                }
            }
            pos += block;
        }
        got
    };
    // 重新跑一遍 guarded 运行并对照 contiguous（上面的循环已消费 state）。
    let mut state3 = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state3);
    let mut pos = 0usize;
    let mut flat = vec![vec![0.0f64; total]; fx.container];
    let mut in3 = vec![0.0f64; fx.n_src * in_stride];
    let mut out3 = vec![0.0f64; fx.container * out_stride];
    while pos < total {
        for i in 0..fx.n_src {
            for k in 0..block {
                let idx = pos + k;
                in3[i * in_stride + k] = if idx < fx.input_len() {
                    fx.input[i][idx]
                } else {
                    0.0
                };
            }
        }
        let status = direct_block_f64(
            &topo,
            &layout,
            &coeffs,
            &mut state3,
            &in3,
            in_stride,
            &mut out3,
            out_stride,
            block,
            block,
        );
        assert!(status.is_ok());
        for c in 0..fx.container {
            for k in 0..block {
                flat[c][pos + k] = out3[c * out_stride + k];
            }
        }
        pos += block;
    }
    for c in 0..fx.container {
        assert_eq!(flat[c], contiguous[c]);
    }
    println!(
        "planar stride PASS in_stride={} out_stride={}",
        in_stride, out_stride
    );
}

#[test]
fn direct_negative_calls_rejected() {
    let topo = DirectTopology::identity();
    let layout = DirectLayout::build(&topo).expect("layout");
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let coeffs = DirectCoeffsF64 {
        a_fir: &[1.0],
        g: &[0.0],
        p: &[1.0],
    };
    let input = [1.0f64, 2.0, 3.0, 4.0];
    let mut output = [0.0f64; 4];
    let ok = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        4,
        4,
    );
    assert!(ok.is_ok());

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state[..layout.state_samples() - 1],
        &input,
        4,
        &mut output,
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_CAPACITY);

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input[..2],
        4,
        &mut output,
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_CAPACITY);

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output[..2],
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_CAPACITY);

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        0,
        0,
    );
    assert_eq!(status, RtStatus::ERR_BLOCK_SIZE);

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        DIRECT_MAX_BLOCK + 1,
        4,
    );
    assert_eq!(status, RtStatus::ERR_BLOCK_SIZE);

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        4,
        5,
    );
    assert_eq!(status, RtStatus::ERR_BAD_LAYOUT);

    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        2,
        &mut output,
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_STRIDE_ALIGN);

    let wrong_a = [1.0f64, 2.0];
    let bad_coeffs = DirectCoeffsF64 {
        a_fir: &wrong_a,
        g: &[],
        p: &[1.0],
    };
    let status = direct_block_f64(
        &topo,
        &layout,
        &bad_coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_PATH_TABLE);

    let mut tampered = layout;
    tampered.d_samples = 1;
    let status = direct_block_f64(
        &topo,
        &tampered,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_BAD_LAYOUT);

    let mut invalid = DirectTopology::identity();
    invalid.n_role = 0;
    let status = direct_block_f64(
        &invalid,
        &layout,
        &coeffs,
        &mut state,
        &input,
        4,
        &mut output,
        4,
        4,
        4,
    );
    assert_eq!(status, RtStatus::ERR_ROLES);
    println!("negative calls PASS");
}

#[test]
fn direct_rt_visible_types_and_capacity_consts() {
    assert!(!std::mem::needs_drop::<DirectTopology>());
    assert!(!std::mem::needs_drop::<DirectLayout>());
    assert!(!std::mem::needs_drop::<DirectCoeffsF32<'static>>());
    assert!(!std::mem::needs_drop::<DirectCoeffsF64<'static>>());
    assert_eq!(DIRECT_MAX_SOURCES, 32);
    assert_eq!(DIRECT_MAX_ROLES, 22);
    assert_eq!(DIRECT_MAX_CONTAINERS, 22);
    assert_eq!(DIRECT_MAX_PARTICIPANTS, 8);
    assert_eq!(DIRECT_MAX_TAPS, 4096);
    assert_eq!(DIRECT_MAX_BLOCK, 128);
    assert_eq!(DIRECT_NO_SOURCE, u16::MAX);
    let identity = DirectTopology::identity();
    assert!(identity.validate().is_ok());
    assert_eq!(identity.src_of_role[0], 0);
    assert_eq!(identity.participants[0], 0);
    assert_eq!(identity.scatter[0], 0);
    println!("rt-visible types / capacity consts PASS");
}

// ---------------------------------------------------------------------------
// 测试 14：RT 路径零分配（计数分配器；f32 生产入口 + partial/DRAIN 路径；f64 oracle）
// ---------------------------------------------------------------------------

#[test]
fn direct_rt_path_zero_alloc() {
    // 使用 C09（4.1.2 5x5 嵌入，N=7/O=7/C=8/M=5/D=4）的真实拓扑与系数；所有 Vec/state 预分配
    // 后，在计数区内只调用内核入口。口径与 T08 partitioned_rt_path_zero_alloc 一致。
    let fixtures = load_fixtures();
    let fx = fixtures
        .iter()
        .find(|fx| fx.id == "C09_4_1_2_5x5_embed")
        .expect("C09 fixture");
    let topo = fx.topology();
    let layout = match DirectLayout::build(&topo) {
        Ok(layout) => layout,
        Err(status) => panic!("C09 layout rejected with code {}", status.code()),
    };
    let (a, g, p) = fx.flat_coeffs(false);
    let a32: Vec<f32> = a.iter().map(|value| *value as f32).collect();
    let g32: Vec<f32> = g.iter().map(|value| *value as f32).collect();
    let p32: Vec<f32> = p.iter().map(|value| *value as f32).collect();
    let coeffs32 = DirectCoeffsF32 {
        a_fir: &a32,
        g: &g32,
        p: &p32,
    };
    let coeffs64 = DirectCoeffsF64 {
        a_fir: &a,
        g: &g,
        p: &p,
    };
    let block = 16usize;
    let n_src = fx.n_src;
    let containers = fx.container;
    let mut in_block32 = vec![0.0f32; n_src * block];
    in_block32.fill(0.25);
    let mut out_block32 = vec![0.0f32; containers * block];
    let mut state32 = vec![0.0f32; layout.state_samples()];
    direct_state_zero_f32(&mut state32);
    let mut in_block64 = vec![0.0f64; n_src * block];
    in_block64.fill(0.25);
    let mut out_block64 = vec![0.0f64; containers * block];
    let mut state64 = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state64);

    // 预热（线程局部计数/TLS 首次触碰）并验证一次基线调用 OK。
    alloc_counter_warmup();
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs32,
        &mut state32,
        &in_block32,
        block,
        &mut out_block32,
        block,
        block,
        block,
    );
    assert!(status.is_ok());

    let before32 = alloc_count();
    let mut calls = 0usize;
    while calls < 8 {
        let status = direct_block_f32(
            &topo,
            &layout,
            &coeffs32,
            &mut state32,
            &in_block32,
            block,
            &mut out_block32,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        calls += 1;
    }
    let after32 = alloc_count();
    assert_eq!(
        after32,
        before32,
        "RT f32 path allocated {} times",
        after32 - before32
    );

    // direct 扩展语义的 partial 路径（V=3 可拼接 partial；V=0 只清输出）同样必须 0 分配。
    let mut partial_state = vec![0.0f32; layout.state_samples()];
    direct_state_zero_f32(&mut partial_state);
    let before_partial = alloc_count();
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs32,
        &mut partial_state,
        &in_block32,
        block,
        &mut out_block32,
        block,
        block,
        3,
    );
    assert!(status.is_ok());
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs32,
        &mut partial_state,
        &in_block32,
        block,
        &mut out_block32,
        block,
        block,
        0,
    );
    assert!(status.is_ok());
    let after_partial = alloc_count();
    assert_eq!(
        after_partial,
        before_partial,
        "partial (V=3/V=0) path allocated {} times",
        after_partial - before_partial
    );

    // f64 oracle 入口（T03 reference 对拍用）同样 0 分配。
    let status = direct_block_f64(
        &topo,
        &layout,
        &coeffs64,
        &mut state64,
        &in_block64,
        block,
        &mut out_block64,
        block,
        block,
        block,
    );
    assert!(status.is_ok());
    let before64 = alloc_count();
    calls = 0;
    while calls < 8 {
        let status = direct_block_f64(
            &topo,
            &layout,
            &coeffs64,
            &mut state64,
            &in_block64,
            block,
            &mut out_block64,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        calls += 1;
    }
    let after64 = alloc_count();
    assert_eq!(
        after64,
        before64,
        "f64 oracle path allocated {} times",
        after64 - before64
    );

    println!(
        "zero-alloc PASS direct f32 8 blocks=0 alloc, f64 8 blocks=0 alloc, partial(V=3/V=0)=0 alloc (state_samples={})",
        layout.state_samples()
    );
}
