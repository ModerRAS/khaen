//! T08 共享 FFT 分区卷积 kernel 私有测试（WBS T08；写域 crates/core/tests/T08/）。
//!
//! 自包含（无第三方依赖）JSON 读取 + T03 reference fixtures / T07 direct kernel 对拍：
//! - f64 oracle 入口：14 个 T03 夹具按 T08 冻结容差（rel L2 <= 1e-12）对拍 reference expected；
//! - f32 RT 入口：14 个夹具按 T08 f32 工程门（rel L2 <= 1e-5）对拍 reference expected；
//! - 与 T07 direct f64 同流对拍（共享 FFT 分区卷积 vs 固定缓冲直算，同 Graph 同输入）；
//! - 共享 FFT 历史 b-p：脉冲位置扫描（跨分区边界）、多分区/短尾/flush/DRAIN；
//! - B=3 与非 2 的幂 B 真实覆盖（T03 声明 B=3 不再静默过滤；K=next_pow2(2B) 零填充）；
//! - DRAIN 对称性：V=0 与 V>0 同样清历史（新 epoch），含非 identity（C04）partial 用例；
//! - 圆周混叠陷阱（C04/H11）、零分配 RT 路径（含 DRAIN 路径计数分配器）、负向参数校验；
//! - 预分配 plan/state/build_scratch 容量契约与布局公式。
//!
//! Cargo 自动把 tests/T08/main.rs 发现为集成测试 target（不需要 public_registration 注册）。
//! 运行：cargo test -p khaen-core partitioned -- --nocapture

#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use khaen_core::direct::{
    direct_block_f32, direct_block_f64, direct_state_zero_f32, direct_state_zero_f64,
    DirectCoeffsF32, DirectCoeffsF64, DirectLayout, DirectTopology, DIRECT_MAX_PARTICIPANTS,
    DIRECT_MAX_ROLES, DIRECT_NO_SOURCE,
};
use khaen_core::partitioned::{
    partitioned_block_f32, partitioned_block_f64, partitioned_state_zero_f32,
    partitioned_state_zero_f64, PartitionedLayout, PartitionedPlan, PARTITIONED_MAX_BLOCK,
    PARTITIONED_MAX_FFT, PARTITIONED_MAX_PARTICIPANTS, PARTITIONED_MAX_PARTITIONS,
    PARTITIONED_MAX_ROLES, PARTITIONED_MAX_ROUTES, PARTITIONED_MAX_TAPS,
    PARTITIONED_MAX_TAPS_EXTENDED,
};
use khaen_core::rt::RtStatus;

// ---------------------------------------------------------------------------
// T08 冻结精度门（FFT 引入舍入；T03 SPEC §3.5 逐位论证只适用于精确 MAC）
// ---------------------------------------------------------------------------
// 判据不事后放宽：f64 rel L2 <= 1e-12（binary64 eps 2^-53 的 ~1e4 倍安全余量）、
// f32 rel L2 <= 1e-5（与 T07 f32 工程门一致）。max_abs <= factor * max(1, peak_ref)。
const F64_REL_L2_MAX: f64 = 1e-12;
const F64_MAX_ABS_FACTOR: f64 = 1e-12;
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
// 测试目标内的所有测试各跑在独立线程；线程局部计数保证互不干扰。Cell<u64> 的
// const 初始化不分配，try_with 在 TLS 销毁期安全忽略。

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
// 运行器：partitioned（f64 oracle / f32 RT）与 direct（T07 对照）
// ---------------------------------------------------------------------------

fn padded_total(input_len: usize, flush: usize, block: usize) -> usize {
    let padded = input_len + flush;
    ((padded + block - 1) / block) * block
}

fn stream_from_input(input: &[Vec<f64>], flush: usize, block: usize) -> Vec<Vec<f64>> {
    let l_in = input[0].len();
    let total = padded_total(l_in, flush, block);
    let mut stream = vec![vec![0.0f64; total]; input.len()];
    for i in 0..input.len() {
        for n in 0..l_in {
            stream[i][n] = input[i][n];
        }
    }
    stream
}

fn build_plan_f64(
    topo: &DirectTopology,
    block: usize,
    a: &[f64],
    g: &[f64],
    p: &[f64],
) -> (PartitionedPlan, Vec<f64>) {
    let layout = PartitionedLayout::build(topo, block).expect("T08 layout");
    let mut plan_data = vec![0.0f64; layout.plan_samples()];
    let mut build_scratch = vec![0.0f64; layout.build_scratch_samples()];
    let coeffs = DirectCoeffsF64 { a_fir: a, g, p };
    let plan = PartitionedPlan::build_f64(topo, block, &coeffs, &mut plan_data, &mut build_scratch)
        .expect("T08 f64 plan");
    (plan, plan_data)
}

fn build_plan_f32(
    topo: &DirectTopology,
    block: usize,
    a: &[f32],
    g: &[f32],
    p: &[f32],
) -> (PartitionedPlan, Vec<f32>) {
    let layout = PartitionedLayout::build(topo, block).expect("T08 layout");
    let mut plan_data = vec![0.0f32; layout.plan_samples()];
    let mut build_scratch = vec![0.0f32; layout.build_scratch_samples()];
    let coeffs = DirectCoeffsF32 { a_fir: a, g, p };
    let plan = PartitionedPlan::build_f32(topo, block, &coeffs, &mut plan_data, &mut build_scratch)
        .expect("T08 f32 plan");
    (plan, plan_data)
}

fn run_partitioned_stream_f64(
    topo: &DirectTopology,
    plan: &PartitionedPlan,
    plan_data: &[f64],
    stream: &[Vec<f64>],
    block: usize,
) -> Vec<Vec<f64>> {
    let layout = plan.layout;
    let total = stream[0].len();
    let n_src = layout.n_src as usize;
    let containers = layout.container_channels as usize;
    let mut state = vec![0.0f64; layout.state_samples()];
    partitioned_state_zero_f64(&mut state);
    let mut out = vec![vec![0.0f64; total]; containers];
    let mut in_block = vec![0.0f64; n_src * block];
    let mut out_block = vec![0.0f64; containers * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..n_src {
            for n in 0..block {
                in_block[i * block + n] = stream[i][base + n];
            }
        }
        let status = partitioned_block_f64(
            topo,
            plan,
            plan_data,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        assert!(
            status.is_ok(),
            "partitioned f64 code {} at block {}",
            status.code(),
            base
        );
        for c in 0..containers {
            for n in 0..block {
                out[c][base + n] = out_block[c * block + n];
            }
        }
        base += block;
    }
    out
}

fn run_partitioned_stream_f32(
    topo: &DirectTopology,
    plan: &PartitionedPlan,
    plan_data: &[f32],
    stream: &[Vec<f32>],
    block: usize,
) -> Vec<Vec<f32>> {
    let layout = plan.layout;
    let total = stream[0].len();
    let n_src = layout.n_src as usize;
    let containers = layout.container_channels as usize;
    let mut state = vec![0.0f32; layout.state_samples()];
    partitioned_state_zero_f32(&mut state);
    let mut out = vec![vec![0.0f32; total]; containers];
    let mut in_block = vec![0.0f32; n_src * block];
    let mut out_block = vec![0.0f32; containers * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..n_src {
            for n in 0..block {
                in_block[i * block + n] = stream[i][base + n];
            }
        }
        let status = partitioned_block_f32(
            topo,
            plan,
            plan_data,
            &mut state,
            &in_block,
            block,
            &mut out_block,
            block,
            block,
            block,
        );
        assert!(
            status.is_ok(),
            "partitioned f32 code {} at block {}",
            status.code(),
            base
        );
        for c in 0..containers {
            for n in 0..block {
                out[c][base + n] = out_block[c * block + n];
            }
        }
        base += block;
    }
    out
}

fn run_direct_stream_f64(
    topo: &DirectTopology,
    block: usize,
    a: &[f64],
    g: &[f64],
    p: &[f64],
    stream: &[Vec<f64>],
) -> Vec<Vec<f64>> {
    let layout = DirectLayout::build(topo).expect("T07 direct layout");
    let total = stream[0].len();
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let mut state = vec![0.0f64; layout.state_samples()];
    direct_state_zero_f64(&mut state);
    let coeffs = DirectCoeffsF64 { a_fir: a, g, p };
    let mut out = vec![vec![0.0f64; total]; containers];
    let mut in_block = vec![0.0f64; n_src * block];
    let mut out_block = vec![0.0f64; containers * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..n_src {
            for n in 0..block {
                in_block[i * block + n] = stream[i][base + n];
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
        assert!(
            status.is_ok(),
            "direct f64 code {} at block {}",
            status.code(),
            base
        );
        for c in 0..containers {
            for n in 0..block {
                out[c][base + n] = out_block[c * block + n];
            }
        }
        base += block;
    }
    out
}

fn run_direct_stream_f32(
    topo: &DirectTopology,
    block: usize,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    stream: &[Vec<f32>],
) -> Vec<Vec<f32>> {
    let layout = DirectLayout::build(topo).expect("T07 direct layout");
    let total = stream[0].len();
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let mut state = vec![0.0f32; layout.state_samples()];
    direct_state_zero_f32(&mut state);
    let coeffs = DirectCoeffsF32 { a_fir: a, g, p };
    let mut out = vec![vec![0.0f32; total]; containers];
    let mut in_block = vec![0.0f32; n_src * block];
    let mut out_block = vec![0.0f32; containers * block];
    let mut base = 0usize;
    while base < total {
        for i in 0..n_src {
            for n in 0..block {
                in_block[i * block + n] = stream[i][base + n];
            }
        }
        let status = direct_block_f32(
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
        assert!(
            status.is_ok(),
            "direct f32 code {} at block {}",
            status.code(),
            base
        );
        for c in 0..containers {
            for n in 0..block {
                out[c][base + n] = out_block[c * block + n];
            }
        }
        base += block;
    }
    out
}

fn run_direct_case_f32(
    topo: &DirectTopology,
    block: usize,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    input: &[Vec<f64>],
    flush: usize,
) -> Vec<Vec<f32>> {
    let stream = stream_from_input(input, flush, block);
    let stream32: Vec<Vec<f32>> = stream
        .iter()
        .map(|ch| ch.iter().map(|v| *v as f32).collect())
        .collect();
    run_direct_stream_f32(topo, block, a, g, p, &stream32)
}

fn run_case_f64(
    topo: &DirectTopology,
    block: usize,
    a: &[f64],
    g: &[f64],
    p: &[f64],
    input: &[Vec<f64>],
    flush: usize,
) -> Vec<Vec<f64>> {
    let (plan, plan_data) = build_plan_f64(topo, block, a, g, p);
    let stream = stream_from_input(input, flush, block);
    run_partitioned_stream_f64(topo, &plan, &plan_data, &stream, block)
}

fn run_case_f32(
    topo: &DirectTopology,
    block: usize,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    input: &[Vec<f64>],
    flush: usize,
) -> Vec<Vec<f32>> {
    let (plan, plan_data) = build_plan_f32(topo, block, a, g, p);
    let stream = stream_from_input(input, flush, block);
    let stream32: Vec<Vec<f32>> = stream
        .iter()
        .map(|ch| ch.iter().map(|v| *v as f32).collect())
        .collect();
    run_partitioned_stream_f32(topo, &plan, &plan_data, &stream32, block)
}

fn run_direct_case_f64(
    topo: &DirectTopology,
    block: usize,
    a: &[f64],
    g: &[f64],
    p: &[f64],
    input: &[Vec<f64>],
    flush: usize,
) -> Vec<Vec<f64>> {
    let stream = stream_from_input(input, flush, block);
    run_direct_stream_f64(topo, block, a, g, p, &stream)
}

fn fixture_blocks(fx: &Fixture) -> Vec<usize> {
    // T08 支持 1..=128 任意 B（含 T03 声明的 B=3），不再按 2 的幂静默过滤；
    // 仅按 kernel 容量过滤，测试 partitioned_non_power_of_two_block_sizes 断言无声明块被跳过。
    fx.block_sizes
        .iter()
        .copied()
        .filter(|block| *block >= 1 && *block <= PARTITIONED_MAX_BLOCK)
        .collect()
}

fn run_fixture_f64(fx: &Fixture, block: usize) -> Vec<Vec<f64>> {
    let topo = fx.topology();
    let (a, g, p) = fx.flat_coeffs(false);
    let (plan, plan_data) = build_plan_f64(&topo, block, &a, &g, &p);
    let stream = stream_from_input(&fx.input, fx.l_f - 1, block);
    let out = run_partitioned_stream_f64(&topo, &plan, &plan_data, &stream, block);
    out.into_iter().map(|ch| ch[..fx.l_out].to_vec()).collect()
}

fn run_fixture_f32(fx: &Fixture, block: usize) -> Vec<Vec<f32>> {
    let topo = fx.topology();
    let (a, g, p) = fx.flat_coeffs(false);
    let a32: Vec<f32> = a.iter().map(|v| *v as f32).collect();
    let g32: Vec<f32> = g.iter().map(|v| *v as f32).collect();
    let p32: Vec<f32> = p.iter().map(|v| *v as f32).collect();
    let (plan, plan_data) = build_plan_f32(&topo, block, &a32, &g32, &p32);
    let stream = stream_from_input(&fx.input, fx.l_f - 1, block);
    let stream32: Vec<Vec<f32>> = stream
        .iter()
        .map(|ch| ch.iter().map(|v| *v as f32).collect())
        .collect();
    let out = run_partitioned_stream_f32(&topo, &plan, &plan_data, &stream32, block);
    out.into_iter().map(|ch| ch[..fx.l_out].to_vec()).collect()
}

fn run_fixture_direct_f64(fx: &Fixture, block: usize) -> Vec<Vec<f64>> {
    let topo = fx.topology();
    let (a, g, p) = fx.flat_coeffs(false);
    let stream = stream_from_input(&fx.input, fx.l_f - 1, block);
    let out = run_direct_stream_f64(&topo, block, &a, &g, &p, &stream);
    out.into_iter().map(|ch| ch[..fx.l_out].to_vec()).collect()
}

// ---------------------------------------------------------------------------
// 比较器（T08 冻结门；所有失败带用例/块大小/样本位置）
// ---------------------------------------------------------------------------

fn peak_of(expected: &[Vec<f64>]) -> f64 {
    let mut peak = 0.0f64;
    for channel in expected {
        for value in channel {
            let mag = value.abs();
            if mag > peak {
                peak = mag;
            }
        }
    }
    peak
}

fn check_f64_against(
    id: &str,
    expected: &[Vec<f64>],
    got: &[Vec<f64>],
    rel_max: f64,
    abs_factor: f64,
) -> (f64, f64, f64) {
    assert_eq!(expected.len(), got.len(), "{}: container count", id);
    let peak = peak_of(expected);
    let scale = if peak > 1.0 { peak } else { 1.0 };
    let mut err_sq = 0.0f64;
    let mut ref_sq = 0.0f64;
    let mut max_abs = 0.0f64;
    for c in 0..expected.len() {
        assert_eq!(expected[c].len(), got[c].len(), "{}: channel {}", id, c);
        for n in 0..expected[c].len() {
            let actual = got[c][n];
            assert!(
                actual.is_finite(),
                "{}: non-finite f64 at container {} sample {}",
                id,
                c,
                n
            );
            let expect = expected[c][n];
            let err = actual - expect;
            err_sq += err * err;
            ref_sq += expect * expect;
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
    assert!(
        rel_l2 <= rel_max,
        "{}: rel L2 {:e} > {:e}",
        id,
        rel_l2,
        rel_max
    );
    assert!(
        max_abs <= abs_factor * scale,
        "{}: max abs {:e} > {:e}",
        id,
        max_abs,
        abs_factor * scale
    );
    (rel_l2, max_abs, peak)
}

fn check_f32_against(
    id: &str,
    expected: &[Vec<f64>],
    got: &[Vec<f32>],
    rel_max: f64,
    abs_factor: f64,
) -> (f64, f64, f64) {
    assert_eq!(expected.len(), got.len(), "{}: container count", id);
    let peak = peak_of(expected);
    let scale = if peak > 1.0 { peak } else { 1.0 };
    let mut err_sq = 0.0f64;
    let mut ref_sq = 0.0f64;
    let mut max_abs = 0.0f64;
    for c in 0..expected.len() {
        assert_eq!(expected[c].len(), got[c].len(), "{}: channel {}", id, c);
        for n in 0..expected[c].len() {
            let actual = got[c][n] as f64;
            assert!(
                actual.is_finite(),
                "{}: non-finite f32 at container {} sample {}",
                id,
                c,
                n
            );
            let expect = expected[c][n];
            let err = actual - expect;
            err_sq += err * err;
            ref_sq += expect * expect;
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
    assert!(
        rel_l2 <= rel_max,
        "{}: rel L2 {:e} > {:e}",
        id,
        rel_l2,
        rel_max
    );
    assert!(
        max_abs <= abs_factor * scale,
        "{}: max abs {:e} > {:e}",
        id,
        max_abs,
        abs_factor * scale
    );
    (rel_l2, max_abs, peak)
}

// ---------------------------------------------------------------------------
// 图构造 helpers（合成拓扑/系数；不依赖 fixture JSON，交叉验证独立路径）
// ---------------------------------------------------------------------------

fn topo_1x1(d: u32, l_a: u16, l_g: u16, l_p: u16) -> DirectTopology {
    let mut topo = DirectTopology::identity();
    topo.d_samples = d;
    topo.l_a = l_a;
    topo.l_g = l_g;
    topo.l_p = l_p;
    topo
}

fn topo_4_1_2(d: u32, l_a: u16, l_g: u16, l_p: u16) -> DirectTopology {
    let mut topo = DirectTopology::identity();
    topo.n_src = 7;
    topo.n_role = 7;
    topo.container_channels = 8;
    topo.d_samples = d;
    topo.l_a = l_a;
    topo.l_g = l_g;
    topo.l_p = l_p;
    topo.m = 5;
    let mut j = 0usize;
    while j < 7 {
        topo.src_of_role[j] = j as u16;
        j += 1;
    }
    let roles = [0u16, 1, 2, 3, 6];
    let mut a = 0usize;
    while a < 5 {
        topo.participants[a] = roles[a];
        a += 1;
    }
    let scatter = [0u16, 1, 2, 3, 4, 5, 7];
    j = 0;
    while j < 7 {
        topo.scatter[j] = scatter[j];
        j += 1;
    }
    topo
}

fn topo_many(
    n_src: u16,
    n_role: u16,
    containers: u16,
    m: u16,
    l_a: u16,
    l_g: u16,
    l_p: u16,
) -> DirectTopology {
    let mut topo = DirectTopology::identity();
    topo.n_src = n_src;
    topo.n_role = n_role;
    topo.container_channels = containers;
    topo.l_a = l_a;
    topo.l_g = l_g;
    topo.l_p = l_p;
    topo.m = m;
    topo.d_samples = 0;
    let mut j = 0usize;
    while j < n_role as usize {
        topo.src_of_role[j] = j as u16;
        topo.scatter[j] = j as u16;
        j += 1;
    }
    let mut a = 0usize;
    while a < m as usize {
        topo.participants[a] = a as u16;
        a += 1;
    }
    topo
}

fn synth_a(n_src: usize, l_a: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; n_src * l_a];
    for k in 0..out.len() {
        out[k] = 0.25 * (((k * 5 + 1) % 7) as f64 - 3.0);
    }
    out
}

fn synth_g(m: usize, l_g: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; m * m * l_g];
    for value in out.iter_mut() {
        *value = 0.0;
    }
    for row in 0..m {
        for col in 0..m {
            for t in 0..l_g {
                let raw = (row * 3 + col * 5 + t * 2) % 9;
                out[(row * m + col) * l_g + t] = 0.125 * (raw as f64 - 4.0);
            }
        }
    }
    out
}

fn synth_p(n_role: usize, l_p: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; n_role * l_p];
    for k in 0..out.len() {
        let raw = (k * 7 + 3) % 11;
        out[k] = 0.25 * (raw as f64 - 5.0);
    }
    out
}

fn conv_f64(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0f64; a.len() + b.len() - 1];
    for i in 0..a.len() {
        for j in 0..b.len() {
            out[i + j] += a[i] * b[j];
        }
    }
    out
}

fn dft_forward(re: &[f64], im: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let n = re.len();
    let mut out_re = vec![0.0f64; n];
    let mut out_im = vec![0.0f64; n];
    for k in 0..n {
        let mut sum_re = 0.0f64;
        let mut sum_im = 0.0f64;
        for t in 0..n {
            let angle = -2.0 * std::f64::consts::PI * ((k * t) % n) as f64 / n as f64;
            let c = angle.cos();
            let s = angle.sin();
            sum_re += re[t] * c - im[t] * s;
            sum_im += re[t] * s + im[t] * c;
        }
        out_re[k] = sum_re;
        out_im[k] = sum_im;
    }
    (out_re, out_im)
}

fn circular_conv_f64(h: &[f64], x: &[f64], n_fft: usize) -> Vec<f64> {
    let mut h_re = vec![0.0f64; n_fft];
    let mut x_re = vec![0.0f64; n_fft];
    let zero = vec![0.0f64; n_fft];
    for (i, value) in h.iter().enumerate() {
        h_re[i] = *value;
    }
    for (i, value) in x.iter().enumerate() {
        x_re[i] = *value;
    }
    let (h_f_re, h_f_im) = dft_forward(&h_re, &zero);
    let (x_f_re, x_f_im) = dft_forward(&x_re, &zero);
    let mut y_re = vec![0.0f64; n_fft];
    let mut y_im = vec![0.0f64; n_fft];
    for k in 0..n_fft {
        y_re[k] = h_f_re[k] * x_f_re[k] - h_f_im[k] * x_f_im[k];
        y_im[k] = h_f_re[k] * x_f_im[k] + h_f_im[k] * x_f_re[k];
    }
    let mut y_conj_im = vec![0.0f64; n_fft];
    for k in 0..n_fft {
        y_conj_im[k] = -y_im[k];
    }
    let (z_re, _z_im) = dft_forward(&y_re, &y_conj_im);
    let mut out = vec![0.0f64; n_fft];
    for n in 0..n_fft {
        out[n] = z_re[n] / n_fft as f64;
    }
    out
}

// ---------------------------------------------------------------------------
// 测试 1：T03 fixture/index 绑定（输入 hash 与 SPEC §3.1 公式）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_fixture_index_binding() {
    let index = JsonReader::new(FIXTURE_INDEX).parse();
    let cases = index.get("cases").as_array();
    assert_eq!(cases.len(), FIXTURE_CASES.len());
    let mut seen = 0usize;
    for (i, text) in FIXTURE_CASES.iter().enumerate() {
        let fx = Fixture::parse(text);
        let entry = &cases[i];
        assert_eq!(entry.get("id").as_str(), fx.id.as_str());
        assert_eq!(
            entry.get("file").as_str(),
            format!("cases/{}.json", fx.id).as_str()
        );
        assert_eq!(entry.get("bytes").as_usize(), text.len(), "{}", fx.id);
        assert_eq!(entry.get("comparison").as_str(), fx.mode.as_str());
        // SPEC §3.1 声明支撑公式与流式 flush 长度。
        assert_eq!(fx.l_f, fx.l_a + fx.l_g + fx.l_p - 2, "{}", fx.id);
        assert_eq!(fx.l_out, fx.input_len() + fx.l_f - 1, "{}", fx.id);
        assert_eq!(fx.l_out, fx.expected_len, "{}", fx.id);
        assert_eq!(fx.flush_zeros, fx.l_f - 1, "{}", fx.id);
        assert!(fx.l_g >= fx.d as usize + 1, "{}: D not in L_G", fx.id);
        assert!(fx.l_f <= PARTITIONED_MAX_TAPS, "{}: taps", fx.id);
        assert!(!fx.block_sizes.is_empty(), "{}: block sizes", fx.id);
        seen += 1;
    }
    assert_eq!(seen, 14);
    println!("fixture/index binding PASS cases={}", seen);
}

// ---------------------------------------------------------------------------
// 测试 2：布局/容量契约（plan_samples / state_samples / build_scratch_samples）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_plan_and_layout_contract() {
    let topo = DirectTopology::identity();
    // 1..=128 任意整数 B（含 2 的幂与非 2 的幂）：W=2B、K=next_pow2(W)。
    for block in [1usize, 2, 3, 4, 5, 6, 8, 16, 32, 64, 100, 127, 128] {
        let layout = PartitionedLayout::build(&topo, block).expect("identity layout");
        let window = 2 * block;
        let mut k = 1usize;
        while k < window {
            k <<= 1;
        }
        assert_eq!(layout.block_size as usize, block);
        assert_eq!(layout.window_samples as usize, window);
        assert_eq!(layout.fft_size as usize, k);
        assert_eq!(layout.partitions as usize, 1);
        assert_eq!(layout.n_routes, 1);
        assert_eq!(layout.taps, 1);
        assert_eq!(layout.x_prev_off, 0);
        assert_eq!(layout.x_hist_off as usize, block);
        let hist = 1 * 1 * 2 * k;
        assert_eq!(layout.work_off as usize, block + hist);
        assert_eq!(layout.slot_off as usize, block + hist + 4 * k);
        assert_eq!(layout.state_samples(), block + hist + 4 * k + 1);
        assert_eq!(layout.plan_samples(), k + 2 * k);
        assert_eq!(layout.build_scratch_samples(), 2 + 2 * k);
    }
    // 显式拒绝：B=0 与 >128（非 2 的幂不再是拒绝理由）。
    for bad in [0usize, 129, 256, 1000] {
        assert_eq!(
            PartitionedLayout::build(&topo, bad).err().map(|s| s.code()),
            Some(RtStatus::ERR_BLOCK_SIZE.code()),
            "block {} must be rejected",
            bad
        );
    }

    // 多分区公式：L_F = 27、B=8 -> Q=ceil(27/8)=4，K=16，最后分区 3 tap（短尾）。
    let multi = topo_1x1(0, 1, 27, 1);
    let layout = PartitionedLayout::build(&multi, 8).expect("multi layout");
    assert_eq!(layout.taps, 27);
    assert_eq!(layout.partitions, 4);
    assert_eq!(layout.window_samples, 16);
    assert_eq!(layout.fft_size, 16);
    assert_eq!(layout.partitions as usize * 8 - 27, 5);

    // 非 2 的幂 B：窗口 2B 零填充到 next_pow2(2B)；Q=ceil(L_F/B)。
    // T03 声明的 B=3（C07/C10/C12）在 T08 真实支持（旧版测试曾静默过滤）。
    let c04 = topo_1x1(0, 1, 2, 3); // L_F = 1+2+3-2 = 4
    let l3 = PartitionedLayout::build(&c04, 3).expect("B=3 layout");
    assert_eq!(l3.window_samples, 6);
    assert_eq!(l3.fft_size, 8);
    assert_eq!(l3.partitions, 2);
    let l6 = PartitionedLayout::build(&c04, 6).expect("B=6 layout");
    assert_eq!(l6.window_samples, 12);
    assert_eq!(l6.fft_size, 16);
    assert_eq!(l6.partitions, 1);
    let l100 = PartitionedLayout::build(&c04, 100).expect("B=100 layout");
    assert_eq!(l100.window_samples, 200);
    assert_eq!(l100.fft_size, 256);
    assert_eq!(l100.partitions, 1);
    let l127 = PartitionedLayout::build(&c04, 127).expect("B=127 layout");
    assert_eq!(l127.window_samples, 254);
    assert_eq!(l127.fft_size, 256);

    // Q 上限：L_F=1501、B=1 -> Q=1501 > MAX_PARTITIONS=1500 -> ERR_TAPS。
    let big = topo_1x1(0, 1, 1501, 1);
    assert_eq!(
        PartitionedLayout::build(&big, 1).err().map(|s| s.code()),
        Some(RtStatus::ERR_TAPS.code())
    );
    // extended taps（L_F=4097）由 T07 拓扑 validate（direct 口径 4096）拒绝。
    let ext = topo_1x1(0, 1, 4097, 1);
    assert_eq!(
        PartitionedLayout::build(&ext, 64).err().map(|s| s.code()),
        Some(RtStatus::ERR_TAPS.code())
    );
    // 引擎相关容量：partitioned extended 口径（TAPS_EXTENDED_MAX=16384）可承载 L_F=4097；
    // direct/普通口径（build）保持旧拒绝（旧行为兼容）。
    let ext_layout = PartitionedLayout::build_extended(&ext, 64).expect("extended layout 4097");
    assert_eq!(ext_layout.taps, 4097);
    // Q = ceil(4097/64) = 65。
    assert_eq!(ext_layout.partitions, 65);
    // 生产 4096-tap P_post FIR 形状：L_F = 1 + 8 + 4096 - 2 = 4103（> 4096）。
    let p4096 = topo_1x1(0, 1, 8, 4096);
    assert_eq!(
        PartitionedLayout::build(&p4096, 128)
            .err()
            .map(|s| s.code()),
        Some(RtStatus::ERR_TAPS.code())
    );
    let p4096_layout = PartitionedLayout::build_extended(&p4096, 128).expect("extended 4103");
    assert_eq!(p4096_layout.taps, 4103);
    // Q = ceil(4103/128) = 33；K = 2B = 256。
    assert_eq!(p4096_layout.partitions, 33);
    assert_eq!(p4096_layout.fft_size, 256);
    // extended 抽头上限：L_F=16384（B=64 时 Q=256）接受，L_F=16385 拒绝。分区上限是 1500。
    let max_ext = topo_1x1(0, 1, 16384, 1);
    assert_eq!(
        max_ext
            .validate_with_fused_taps(PARTITIONED_MAX_TAPS_EXTENDED)
            .code(),
        RtStatus::OK.code()
    );
    let max_layout = PartitionedLayout::build_extended(&max_ext, 64).expect("extended max");
    assert_eq!(max_layout.partitions, 256);
    let over = topo_1x1(0, 1, 16385, 1);
    assert_eq!(
        PartitionedLayout::build_extended(&over, 128)
            .err()
            .map(|s| s.code()),
        Some(RtStatus::ERR_TAPS.code())
    );

    // 最大容量配置：N=32、O=C=22、M=8、L_A=1、L_G=4094、L_P=1 -> L_F=4094、B=64 时 Q=64。
    let max = topo_many(32, 22, 22, 8, 1, 4094, 1);
    let layout = PartitionedLayout::build(&max, 64).expect("max layout");
    assert_eq!(layout.n_routes, 78); // 22 对角 + 8*7 非对角
    assert_eq!(layout.partitions, 64);
    assert_eq!(layout.taps, 4094);
    assert_eq!(
        layout.state_samples(),
        32 * 64 + 32 * 64 * 256 + 4 * 128 + 1
    );
    assert_eq!(layout.plan_samples(), 128 + 78 * 64 * 2 * 128);
    assert_eq!(layout.build_scratch_samples(), 2 * 4094 + 2 * 128);
    println!(
        "layout PASS blocks 1..128; max state={} plan={} routes={}",
        layout.state_samples(),
        layout.plan_samples(),
        layout.n_routes
    );
}

#[test]
fn partitioned_extended_taps_4103_impulse_matches_linear_convolution() {
    // 生产 4096-tap P_post FIR 形状：A=identity、G=delta（1 tap）、P=4103-tap FIR
    // -> 融合 L_F = 1 + 1 + 4103 - 2 = 4103（> direct 4096，<= extended 16384）。
    // 脉冲输入时 y = h（线性卷积特例），同时覆盖 Q=33 分区的跨分区历史。
    let block = 128usize;
    let blocks = 8usize;
    let taps = 4103usize;
    let topo = topo_1x1(0, 1, 1, taps as u16);
    let mut h = vec![0.0f64; taps];
    h[0] = 1.0;
    for t in 1..taps {
        h[t] = 0.9f64.powi(t as i32) * 0.01;
    }
    let total = blocks * block;
    let mut input = vec![0.0f64; total];
    input[0] = 1.0;

    // direct/普通口径（build/build_f32）保持旧拒绝：L_F=4103 > 4096。
    let mut tiny_data = [0.0f64; 1];
    let mut tiny_scratch = [0.0f64; 1];
    let a64 = [1.0f64];
    let g64 = [0.0f64];
    let coeffs64 = DirectCoeffsF64 {
        a_fir: &a64,
        g: &g64,
        p: &h,
    };
    assert_eq!(
        PartitionedPlan::build_f64(&topo, block, &coeffs64, &mut tiny_data, &mut tiny_scratch)
            .err()
            .map(|s| s.code()),
        Some(RtStatus::ERR_TAPS.code())
    );

    // f64 oracle（extended 口径）：脉冲响应必须精确等于 h。
    let layout = PartitionedLayout::build_extended(&topo, block).expect("extended layout");
    assert_eq!(layout.taps, 4103);
    assert_eq!(layout.partitions, 33);
    let mut plan_data = vec![0.0f64; layout.plan_samples()];
    let mut scratch = vec![0.0f64; layout.build_scratch_samples()];
    let plan64 =
        PartitionedPlan::build_f64_extended(&topo, block, &coeffs64, &mut plan_data, &mut scratch)
            .expect("extended f64 plan");
    assert_eq!(
        plan64.fused_taps_limit as usize,
        PARTITIONED_MAX_TAPS_EXTENDED
    );
    let mut state = vec![0.0f64; layout.state_samples()];
    let mut output = vec![0.0f64; total];
    let mut block_in = vec![0.0f64; block];
    let mut block_out = vec![0.0f64; block];
    for b in 0..blocks {
        block_in.copy_from_slice(&input[b * block..(b + 1) * block]);
        block_out.iter_mut().for_each(|value| *value = 0.0);
        let status = partitioned_block_f64(
            &topo,
            &plan64,
            &plan_data,
            &mut state,
            &block_in,
            block,
            &mut block_out,
            block,
            block,
            block,
        );
        assert!(status.is_ok(), "partitioned f64 status {}", status.code());
        output[b * block..(b + 1) * block].copy_from_slice(&block_out);
    }
    for n in 0..total {
        let expect = if n < taps { h[n] } else { 0.0 };
        assert!(
            (output[n] - expect).abs() <= 1.0e-12,
            "extended f64 n={n} got {} expect {expect}",
            output[n]
        );
    }

    // f32 RT 入口（生产路径）：误差在 T08 f32 门（rel L2 <= 1e-5）。
    let h32: Vec<f32> = h.iter().map(|value| *value as f32).collect();
    let mut plan_data32 = vec![0.0f32; layout.plan_samples()];
    let mut scratch32 = vec![0.0f32; layout.build_scratch_samples()];
    let a32 = [1.0f32];
    let g32 = [0.0f32];
    let coeffs32 = DirectCoeffsF32 {
        a_fir: &a32,
        g: &g32,
        p: &h32,
    };
    let plan32 = PartitionedPlan::build_f32_extended(
        &topo,
        block,
        &coeffs32,
        &mut plan_data32,
        &mut scratch32,
    )
    .expect("extended f32 plan");
    let mut state32 = vec![0.0f32; layout.state_samples()];
    let mut output32 = vec![0.0f32; total];
    let input32: Vec<f32> = input.iter().map(|value| *value as f32).collect();
    let mut block_in32 = vec![0.0f32; block];
    let mut block_out32 = vec![0.0f32; block];
    for b in 0..blocks {
        block_in32.copy_from_slice(&input32[b * block..(b + 1) * block]);
        block_out32.iter_mut().for_each(|value| *value = 0.0);
        let status = partitioned_block_f32(
            &topo,
            &plan32,
            &plan_data32,
            &mut state32,
            &block_in32,
            block,
            &mut block_out32,
            block,
            block,
            block,
        );
        assert!(status.is_ok(), "partitioned f32 status {}", status.code());
        output32[b * block..(b + 1) * block].copy_from_slice(&block_out32);
    }
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    let mut max_abs = 0.0f64;
    for n in 0..total {
        let expect = if n < taps { h[n] } else { 0.0 };
        let diff = (output32[n] as f64 - expect).abs();
        num += diff * diff;
        den += expect * expect;
        if diff > max_abs {
            max_abs = diff;
        }
    }
    let rel_l2 = (num / den.max(1.0e-30)).sqrt();
    assert!(
        rel_l2 <= F32_REL_L2_MAX,
        "extended f32 rel_l2={rel_l2}（门 1e-5），max_abs={max_abs}"
    );
}

// ---------------------------------------------------------------------------
// 测试 3：预分配 plan/scratch 契约（精确容量可用、更大缓冲前缀一致、不足拒绝）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_plan_preallocation_contract() {
    let topo = topo_4_1_2(1, 2, 5, 2);
    let a = synth_a(7, 2);
    let g = synth_g(5, 5);
    let p = synth_p(7, 2);
    let block = 4usize;
    let (plan_a, data_a) = build_plan_f64(&topo, block, &a, &g, &p);
    let layout = plan_a.layout;
    assert_eq!(data_a.len(), layout.plan_samples());

    let mut data_extra = vec![0.0f64; layout.plan_samples() + 257];
    let mut scratch_extra = vec![0.0f64; layout.build_scratch_samples() + 13];
    let coeffs = DirectCoeffsF64 {
        a_fir: &a,
        g: &g,
        p: &p,
    };
    let plan_b =
        PartitionedPlan::build_f64(&topo, block, &coeffs, &mut data_extra, &mut scratch_extra)
            .expect("plan with extra capacity");
    assert_eq!(plan_a, plan_b);
    assert_eq!(&data_a[..], &data_extra[..layout.plan_samples()]);

    // 容量不足：plan_data 与 build_scratch 都必须拒绝（ERR_CAPACITY）。
    let mut small_data = vec![0.0f64; layout.plan_samples() - 1];
    let mut scratch = vec![0.0f64; layout.build_scratch_samples()];
    assert_eq!(
        PartitionedPlan::build_f64(&topo, block, &coeffs, &mut small_data, &mut scratch)
            .err()
            .map(|s| s.code()),
        Some(RtStatus::ERR_CAPACITY.code())
    );
    let mut data = vec![0.0f64; layout.plan_samples()];
    let mut small_scratch = vec![0.0f64; layout.build_scratch_samples() - 1];
    assert_eq!(
        PartitionedPlan::build_f64(&topo, block, &coeffs, &mut data, &mut small_scratch)
            .err()
            .map(|s| s.code()),
        Some(RtStatus::ERR_CAPACITY.code())
    );
    // 系数长度不符：ERR_PATH_TABLE。
    let bad_coeffs = DirectCoeffsF64 {
        a_fir: &a[..a.len() - 1],
        g: &g,
        p: &p,
    };
    let mut data2 = vec![0.0f64; layout.plan_samples()];
    let mut scratch2 = vec![0.0f64; layout.build_scratch_samples()];
    assert_eq!(
        PartitionedPlan::build_f64(&topo, block, &bad_coeffs, &mut data2, &mut scratch2)
            .err()
            .map(|s| s.code()),
        Some(RtStatus::ERR_PATH_TABLE.code())
    );
    println!(
        "preallocation PASS plan_samples={} state_samples={} build_scratch={}",
        layout.plan_samples(),
        layout.state_samples(),
        layout.build_scratch_samples()
    );
}

// ---------------------------------------------------------------------------
// 测试 4：HAND-CALC H1/H2/H4/H9/H13 手算向量（独立硬编码）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_hand_calc_vectors_match_t03() {
    // H1：1x1 identity D=0 -> y = x。
    let topo = topo_1x1(0, 1, 1, 1);
    let input = vec![vec![1.0, -2.0, 0.5, 0.0, 3.0]];
    let got = run_case_f64(&topo, 64, &[1.0], &[0.0], &[1.0], &input, 0);
    let expected = vec![vec![1.0, -2.0, 0.5, 0.0, 3.0]];
    check_f64_against(
        "H1",
        &expected,
        &[got[0][..5].to_vec()],
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );

    // H2：identity + 预声明公共延迟 D=3（D 计入 L_G=4）。
    let topo = topo_1x1(3, 1, 4, 1);
    let input = vec![vec![1.0, 2.0, 3.0]];
    let got = run_case_f64(&topo, 64, &[1.0], &[0.0, 0.0, 0.0, 0.0], &[1.0], &input, 3);
    let expected = vec![vec![0.0, 0.0, 0.0, 1.0, 2.0, 3.0]];
    check_f64_against(
        "H2",
        &expected,
        &[got[0][..6].to_vec()],
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );

    // H4/H9：F = P*G = [1,1,0,4]；x=[1,1] -> [1,2,1,4,4]；B=2 逐块 [1,2]/[1,4]/[4,0]。
    let topo = topo_1x1(0, 1, 2, 3);
    let g = [0.0f64, 2.0];
    let p = [1.0f64, -1.0, 2.0];
    let input = vec![vec![1.0, 1.0]];
    let got_big = run_case_f64(&topo, 64, &[1.0], &g, &p, &input, 3);
    let expected = vec![vec![1.0, 2.0, 1.0, 4.0, 4.0]];
    check_f64_against(
        "H4",
        &expected,
        &[got_big[0][..5].to_vec()],
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );
    let got_b2 = run_case_f64(&topo, 2, &[1.0], &g, &p, &input, 3);
    let expected_blocks = vec![vec![1.0, 2.0, 1.0, 4.0, 4.0, 0.0]];
    check_f64_against(
        "H9-B2",
        &expected_blocks,
        &got_b2,
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );

    // H13：4.1.2 identity graph、D=2、scatter=[0,1,2,3,4,5,7]、容器 ch6 恒 0。
    let topo = topo_4_1_2(2, 1, 3, 1);
    let input = vec![
        vec![1.0, -1.0, 0.0, 0.0, 2.0, 0.0],
        vec![2.0, -2.0, 0.0, 0.0, 4.0, 0.0],
        vec![3.0, -3.0, 0.0, 0.0, 6.0, 0.0],
        vec![4.0, -4.0, 0.0, 0.0, 8.0, 0.0],
        vec![5.0, -5.0, 0.0, 0.0, 10.0, 0.0],
        vec![6.0, -6.0, 0.0, 0.0, 12.0, 0.0],
        vec![7.0, -7.0, 0.0, 0.0, 14.0, 0.0],
    ];
    let a = vec![1.0f64; 7];
    let g = vec![0.0f64; 5 * 5 * 3];
    let p = vec![1.0f64; 7];
    let got = run_case_f64(&topo, 64, &a, &g, &p, &input, 2);
    let expected = vec![
        vec![0.0, 0.0, 1.0, -1.0, 0.0, 0.0, 2.0, 0.0],
        vec![0.0, 0.0, 2.0, -2.0, 0.0, 0.0, 4.0, 0.0],
        vec![0.0, 0.0, 3.0, -3.0, 0.0, 0.0, 6.0, 0.0],
        vec![0.0, 0.0, 4.0, -4.0, 0.0, 0.0, 8.0, 0.0],
        vec![0.0, 0.0, 5.0, -5.0, 0.0, 0.0, 10.0, 0.0],
        vec![0.0, 0.0, 6.0, -6.0, 0.0, 0.0, 12.0, 0.0],
        vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        vec![0.0, 0.0, 7.0, -7.0, 0.0, 0.0, 14.0, 0.0],
    ];
    let got_trunc: Vec<Vec<f64>> = got.iter().map(|ch| ch[..8].to_vec()).collect();
    check_f64_against(
        "H13",
        &expected,
        &got_trunc,
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );
    println!("HAND-CALC H1/H2/H4/H9/H13 PASS");
}

// ---------------------------------------------------------------------------
// 测试 4b：B=3（T03 声明）与非 2 的幂 B 真实覆盖（不得静默过滤）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_non_power_of_two_block_sizes() {
    // (1) 所有夹具声明的 block size 都必须被 fixture_blocks 保留（无静默过滤）。
    let fixtures = load_fixtures();
    for fx in &fixtures {
        let declared = fx.block_sizes.clone();
        let executed = fixture_blocks(fx);
        assert_eq!(
            declared, executed,
            "{}: declared block sizes silently filtered",
            fx.id
        );
        assert!(declared
            .iter()
            .all(|b| *b >= 1 && *b <= PARTITIONED_MAX_BLOCK));
    }
    // (2) T03 声明 B=3 的三个夹具（C07/C10/C12）在 B=3 下真实执行并对拍。
    let mut b3_cases = 0usize;
    for fx in &fixtures {
        if !fx.block_sizes.contains(&3) {
            continue;
        }
        let layout = PartitionedLayout::build(&fx.topology(), 3).expect("B=3 layout");
        assert_eq!(layout.block_size, 3);
        assert_eq!(layout.window_samples, 6);
        assert_eq!(layout.fft_size, 8);
        assert_eq!(layout.partitions as usize, (fx.l_f + 2) / 3);
        let got = run_fixture_f64(fx, 3);
        check_f64_against(
            &format!("{}/B3-f64", fx.id),
            &fx.expected,
            &got,
            F64_REL_L2_MAX,
            F64_MAX_ABS_FACTOR,
        );
        let got32 = run_fixture_f32(fx, 3);
        check_f32_against(
            &format!("{}/B3-f32", fx.id),
            &fx.expected,
            &got32,
            F32_REL_L2_MAX,
            F32_MAX_ABS_FACTOR,
        );
        b3_cases += 1;
    }
    assert_eq!(b3_cases, 3, "C07/C10/C12 must declare and execute B=3");
    // (3) C04（F=[1,1,0,4]）在 B=3/5/6/100/127 下与线性 expected 一致：
    //     非 2 的幂时窗口/分区零填充到 K=next_pow2(2B)，仍与 2B 时同一线性卷积。
    let topo = topo_1x1(0, 1, 2, 3);
    let input = vec![vec![1.0f64, 1.0]];
    let expected = vec![vec![1.0f64, 2.0, 1.0, 4.0, 4.0]];
    for block in [3usize, 5, 6, 100, 127] {
        let got = run_case_f64(
            &topo,
            block,
            &[1.0],
            &[0.0, 2.0],
            &[1.0, -1.0, 2.0],
            &input,
            3,
        );
        check_f64_against(
            &format!("C04/B{}", block),
            &expected,
            &[got[0][..5].to_vec()],
            F64_REL_L2_MAX,
            F64_MAX_ABS_FACTOR,
        );
        let layout = PartitionedLayout::build(&topo, block).expect("layout");
        let window = 2 * block;
        let mut k = 1usize;
        while k < window {
            k <<= 1;
        }
        assert_eq!(layout.window_samples as usize, window);
        assert_eq!(layout.fft_size as usize, k);
        assert_eq!(layout.partitions as usize, (4 + block - 1) / block);
    }
    // (4) B=0 与 >128 仍显式拒绝（ERR_BLOCK_SIZE，不是静默过滤）。
    for bad in [0usize, 129, 256] {
        assert_eq!(
            PartitionedLayout::build(&topo, bad).err().map(|s| s.code()),
            Some(RtStatus::ERR_BLOCK_SIZE.code()),
            "block {} must be rejected",
            bad
        );
    }
    println!(
        "non-power-of-two B PASS: fixtures with B=3 executed={}, C04 B={{3,5,6,100,127}} vs linear",
        b3_cases
    );
}

// ---------------------------------------------------------------------------
// 测试 5：C04 圆周混叠陷阱（H11）——线性结果 vs 短 FFT 圆周结果
// ---------------------------------------------------------------------------

#[test]
fn partitioned_circular_trap_rejected() {
    // H11：F=[1,1,0,4]、x=[1,1]，N=4 圆周卷积 -> [5,2,1,4]（错的），线性 -> [1,2,1,4,4]。
    let h = [1.0f64, 1.0, 0.0, 4.0];
    let x = [1.0f64, 1.0];
    let trap = circular_conv_f64(&h, &x, 4);
    let trap_expected = [5.0f64, 2.0, 1.0, 4.0];
    for n in 0..4 {
        assert!(
            (trap[n] - trap_expected[n]).abs() <= 1e-12,
            "H11 trap mismatch at {}: {}",
            n,
            trap[n]
        );
    }
    // 内核（zero-pad 分区 overlap-save）必须给出线性卷积，不得命中 trap。
    let topo = topo_1x1(0, 1, 2, 3);
    let input = vec![vec![1.0, 1.0]];
    let got = run_case_f64(&topo, 2, &[1.0], &[0.0, 2.0], &[1.0, -1.0, 2.0], &input, 3);
    let linear = [1.0f64, 2.0, 1.0, 4.0, 4.0];
    for n in 0..5 {
        assert!(
            (got[0][n] - linear[n]).abs() <= 1e-12,
            "linear mismatch at {}: {} vs {}",
            n,
            got[0][n],
            linear[n]
        );
    }
    // 前 4 个样本必须与 trap 明显不同（至少 n=0 差 4.0）。
    assert!((got[0][0] - trap[0]).abs() > 0.5);
    println!(
        "circular trap PASS: trap={:?} kernel-linear={:?}",
        trap,
        &got[0][..5]
    );
}

// ---------------------------------------------------------------------------
// 测试 6：f64 oracle vs T03 reference expected（14 夹具 × 每个声明块大小）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_f64_fixtures_vs_reference() {
    let fixtures = load_fixtures();
    assert_eq!(fixtures.len(), 14);
    let mut checks = 0usize;
    for fx in &fixtures {
        let blocks = fixture_blocks(fx);
        assert!(!blocks.is_empty(), "{}: no supported block size", fx.id);
        for block in blocks {
            let got = run_fixture_f64(fx, block);
            let id = format!("{}/B{}", fx.id, block);
            let (rel_l2, max_abs, peak) =
                check_f64_against(&id, &fx.expected, &got, F64_REL_L2_MAX, F64_MAX_ABS_FACTOR);
            println!(
                "PASS f64 reference {} rel_l2={:.3e} max_abs={:.3e} peak={:.3e}",
                id, rel_l2, max_abs, peak
            );
            checks += 1;
        }
    }
    assert!(checks >= 14 * 4);
    println!("f64 reference PASS checks={}", checks);
}
// ---------------------------------------------------------------------------
// 测试 7：f64 oracle vs T07 direct（同 Graph/同输入/同流；共享 FFT vs 全速直算）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_f64_vs_direct_agreement() {
    let fixtures = load_fixtures();
    let mut checks = 0usize;
    for fx in &fixtures {
        for block in fixture_blocks(fx) {
            let got = run_fixture_f64(fx, block);
            let direct = run_fixture_direct_f64(fx, block);
            let id = format!("{}/B{}", fx.id, block);
            let (rel_l2, max_abs, peak) =
                check_f64_against(&id, &direct, &got, F64_REL_L2_MAX, F64_MAX_ABS_FACTOR);
            println!(
                "PASS f64 vs direct {} rel_l2={:.3e} max_abs={:.3e} peak={:.3e}",
                id, rel_l2, max_abs, peak
            );
            checks += 1;
        }
    }
    assert!(checks >= 14 * 4);
    println!("f64 vs direct PASS checks={}", checks);
}

// ---------------------------------------------------------------------------
// 测试 8：f32 RT 入口 vs reference + 跨 block 大小不变性
// ---------------------------------------------------------------------------

#[test]
fn partitioned_f32_fixtures_gate_and_invariance() {
    let fixtures = load_fixtures();
    let mut checks = 0usize;
    for fx in &fixtures {
        for block in fixture_blocks(fx) {
            let got = run_fixture_f32(fx, block);
            let id = format!("{}/B{}", fx.id, block);
            let (rel_l2, max_abs, peak) =
                check_f32_against(&id, &fx.expected, &got, F32_REL_L2_MAX, F32_MAX_ABS_FACTOR);
            println!(
                "PASS f32 reference {} rel_l2={:.3e} max_abs={:.3e} peak={:.3e}",
                id, rel_l2, max_abs, peak
            );
            checks += 1;
        }
        // block-size invariance：B=64 与 B=128 在同一输入上按容差一致（FFT 长度不同，
        // 舍入不同，故不能要求逐位；f64 用 1e-12、f32 用 1e-5）。
        let f64_b64 = run_fixture_f64(fx, 64);
        let f64_b128 = run_fixture_f64(fx, 128);
        check_f64_against(
            &format!("{}/invariance64-128", fx.id),
            &f64_b64,
            &f64_b128,
            F64_REL_L2_MAX,
            F64_MAX_ABS_FACTOR,
        );
        let f32_b64 = run_fixture_f32(fx, 64);
        let f32_b128 = run_fixture_f32(fx, 128);
        check_f32_against(
            &format!("{}/invariance32-64", fx.id),
            &f64_b64,
            &f32_b64,
            F32_REL_L2_MAX,
            F32_MAX_ABS_FACTOR,
        );
        check_f32_against(
            &format!("{}/invariance32-128", fx.id),
            &f64_b128,
            &f32_b128,
            F32_REL_L2_MAX,
            F32_MAX_ABS_FACTOR,
        );
        checks += 1;
    }
    println!("f32 gate + block invariance PASS checks={}", checks);
}

// ---------------------------------------------------------------------------
// 测试 9：非零多分区 + 短尾（末分区不满 B；末 tap 必须参与）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_multipartition_and_short_tail() {
    // A：L_F=27、B=8 -> Q=4，末分区只有 3 个 tap（索引 24..27），其余分区非零。
    let topo = topo_1x1(0, 1, 27, 1);
    let mut g = vec![0.0f64; 27];
    let taps = [
        (0usize, 0.5f64),
        (5, -0.25),
        (8, 1.0),
        (9, 0.5),
        (15, -0.75),
        (16, 0.25),
        (23, 0.5),
        (24, -0.125),
        (26, 0.25),
    ];
    for (t, value) in taps {
        g[t] = value;
    }
    let layout = PartitionedLayout::build(&topo, 8).expect("multi layout");
    assert_eq!(layout.partitions, 4);
    assert_eq!(layout.taps, 27);
    assert_eq!(layout.partitions as usize * 8 - 27, 5);
    let input = vec![vec![1.0f64]];
    let got = run_case_f64(&topo, 8, &[1.0], &g, &[1.0], &input, 26);
    let mut expected_h = g.clone();
    expected_h[0] += 1.0; // delta_D baseline（D=0）
    let mut expected = vec![vec![0.0f64; 27]];
    expected[0][..27].copy_from_slice(&expected_h);
    let got27 = vec![got[0][..27].to_vec()];
    check_f64_against(
        "multipartition-impulse",
        &expected,
        &got27,
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );
    // 末分区的最后一个 tap（索引 26）必须真实参与：清零后输出必须改变。
    let mut g_tail = g.clone();
    g_tail[26] = 0.0;
    let got_tail = run_case_f64(&topo, 8, &[1.0], &g_tail, &[1.0], &input, 26);
    assert!(
        (got_tail[0][26] - got[0][26]).abs() > 0.1,
        "last-partition tap 26 not applied: {} vs {}",
        got_tail[0][26],
        got[0][26]
    );
    // 末分区整体（索引 24..27）清零也必须改变输出。
    let mut g_last = g.clone();
    for t in 24..27 {
        g_last[t] = 0.0;
    }
    let got_last = run_case_f64(&topo, 8, &[1.0], &g_last, &[1.0], &input, 26);
    let mut max_diff = 0.0f64;
    for n in 0..27 {
        let diff = (got_last[0][n] - got[0][n]).abs();
        if diff > max_diff {
            max_diff = diff;
        }
    }
    assert!(max_diff > 0.1);

    // B：带输入侧 A 与输出侧 P，L_F = 2+10+3-2 = 13，B=4 -> Q=4、末分区 1 个 tap。
    let topo_b = topo_1x1(2, 2, 10, 3);
    let a = [0.5f64, -1.0];
    let mut g = vec![0.0f64; 10];
    g[0] = 0.25;
    g[3] = -0.5;
    g[7] = 0.75;
    g[9] = 0.5;
    let p = [1.0f64, 0.25, -0.5];
    let layout_b = PartitionedLayout::build(&topo_b, 4).expect("layout b");
    assert_eq!(layout_b.partitions, 4);
    assert_eq!(layout_b.partitions as usize * 4 - 13, 3);
    let mut g_full = g.clone();
    g_full[2] += 1.0; // D=2 baseline
    let expected_b = conv_f64(&conv_f64(&g_full, &a), &p);
    assert_eq!(expected_b.len(), 13);
    let got_b = run_case_f64(&topo_b, 4, &a, &g, &p, &[vec![1.0f64]], 12);
    let got_b13 = vec![got_b[0][..13].to_vec()];
    let expected_b = vec![expected_b];
    check_f64_against(
        "multipartition-arp",
        &expected_b,
        &got_b13,
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );
    println!("multi-partition/short-tail PASS Q=4/B=8 and Q=4/B=4");
}

// ---------------------------------------------------------------------------
// 测试 10：共享 FFT 历史 b-p —— 脉冲位置扫描（跨分区边界）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_shared_fft_history_impulse_sweep() {
    let topo = topo_1x1(0, 1, 27, 1);
    let mut g = vec![0.0f64; 27];
    g[0] = 0.5;
    g[7] = -0.25;
    g[8] = 1.0;
    g[9] = 0.5;
    g[15] = -0.75;
    g[16] = 0.25;
    g[23] = 0.5;
    g[24] = -0.125;
    g[26] = 0.25;
    let mut h = g.clone();
    h[0] += 1.0;
    let block = 8usize;
    let flush = 26usize;
    let mut checks = 0usize;
    for n0 in [0usize, 1, 7, 8, 9, 15, 16, 23, 24] {
        let mut input = vec![vec![0.0f64; n0 + 1]];
        input[0][n0] = 1.0;
        let got = run_case_f64(&topo, block, &[1.0], &g, &[1.0], &input, flush);
        let l_out = input[0].len() + 27 - 1;
        let mut expected = vec![vec![0.0f64; l_out]];
        for t in 0..27 {
            expected[0][n0 + t] += h[t];
        }
        let got_trunc = vec![got[0][..l_out].to_vec()];
        let id = format!("impulse@{}", n0);
        check_f64_against(
            &id,
            &expected,
            &got_trunc,
            F64_REL_L2_MAX,
            F64_MAX_ABS_FACTOR,
        );
        checks += 1;
    }
    assert_eq!(checks, 9);
    println!(
        "shared FFT history b-p impulse sweep PASS positions={}",
        checks
    );
}

// ---------------------------------------------------------------------------
// 测试 11：flush 语义（L_F-1 个零；无 flush 则尾缺失）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_flush_tail_c04() {
    let topo = topo_1x1(0, 1, 2, 3);
    let g = [0.0f64, 2.0];
    let p = [1.0f64, -1.0, 2.0];
    let input = vec![vec![1.0f64, 1.0]];
    // H9 的 B=2 逐块表：块1=[1,2]、块2=[1,4]、块3=[4,0]（截断到 L_out=5）。
    let with_flush = run_case_f64(&topo, 2, &[1.0], &g, &p, &input, 3);
    let expected = vec![vec![1.0, 2.0, 1.0, 4.0, 4.0, 0.0]];
    check_f64_against(
        "flush-B2",
        &expected,
        &with_flush,
        F64_REL_L2_MAX,
        F64_MAX_ABS_FACTOR,
    );
    // 无 flush：只算到输入结束，尾 [1,4,4] 不会出现（流长 2、只有 1 个块）。
    let no_flush = run_case_f64(&topo, 2, &[1.0], &g, &p, &input, 0);
    assert_eq!(no_flush[0].len(), 2);
    assert!((no_flush[0][0] - 1.0).abs() <= 1e-12);
    assert!((no_flush[0][1] - 2.0).abs() <= 1e-12);
    // flush 长度 = L_F-1：F 长度 4 -> 需要 3 个零；只补 2 个零时第 5 个输出缺失。
    let short_flush = run_case_f64(&topo, 2, &[1.0], &g, &p, &input, 2);
    assert_eq!(short_flush[0].len(), 4);
    println!(
        "flush PASS: with flush len={} values={:?}; without-flush len={}",
        with_flush[0].len(),
        &with_flush[0][..5],
        no_flush[0].len()
    );
}

// ---------------------------------------------------------------------------
// 测试 12：DRAIN partial valid_frames（[V,B) 输入视作 0、输出 [V,B)=+0.0；
// V=0 与 V>0 对称：都清 x_prev/x_hist+slot（新 epoch）；含非 identity（C04）用例）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_drain_valid_frames() {
    let topo = DirectTopology::identity();
    let block = 8usize;
    let (plan, plan_data) = build_plan_f64(&topo, block, &[1.0], &[0.0], &[1.0]);
    let layout = plan.layout;
    let mut state = vec![0.0f64; layout.state_samples()];
    partitioned_state_zero_f64(&mut state);
    let mut out = vec![-1.0f64; block];
    let full: Vec<f64> = (1..=8).map(|v| v as f64).collect();
    let status = partitioned_block_f64(
        &topo, &plan, &plan_data, &mut state, &full, block, &mut out, block, block, block,
    );
    assert!(status.is_ok());
    for n in 0..block {
        assert!((out[n] - full[n]).abs() <= 1e-12);
    }
    // 流出尾块：V=3；[3,8) 的输入是垃圾但按 0 处理，输出 [0,3) 正确、[3,8) 严格 +0.0。
    let partial = vec![9.0f64, 10.0, 11.0, 999.0, 999.0, 999.0, 999.0, 999.0];
    let mut out_partial = vec![7.0f64; block];
    let status = partitioned_block_f64(
        &topo,
        &plan,
        &plan_data,
        &mut state,
        &partial,
        block,
        &mut out_partial,
        block,
        block,
        3,
    );
    assert!(status.is_ok());
    assert!((out_partial[0] - 9.0).abs() <= 1e-12);
    assert!((out_partial[1] - 10.0).abs() <= 1e-12);
    assert!((out_partial[2] - 11.0).abs() <= 1e-12);
    for n in 3..block {
        assert!(out_partial[n] == 0.0, "tail must be +0.0 at {}", n);
    }
    // 历史被清零 => 下一调用等价新 epoch（identity 图输出 = 新输入）。
    let mut out_next = vec![0.0f64; block];
    let status = partitioned_block_f64(
        &topo,
        &plan,
        &plan_data,
        &mut state,
        &full,
        block,
        &mut out_next,
        block,
        block,
        block,
    );
    assert!(status.is_ok());
    for n in 0..block {
        assert!((out_next[n] - full[n]).abs() <= 1e-12);
    }
    // V=0：与 V>0 对称的零样本 DRAIN——输出全 +0.0，x_prev/x_hist 清零 + slot=0（等价新 epoch），
    // 不是 idle no-op（契约：连续流用 full B 块；partial（含 V=0）只在流末，之后需新 epoch）。
    let mut out_zero = vec![5.0f64; block];
    let status = partitioned_block_f64(
        &topo,
        &plan,
        &plan_data,
        &mut state,
        &full,
        block,
        &mut out_zero,
        block,
        block,
        0,
    );
    assert!(status.is_ok());
    assert!(out_zero.iter().all(|v| *v == 0.0));
    let live_end = plan.layout.work_off as usize;
    assert!(
        state[..live_end].iter().all(|v| *v == 0.0),
        "V=0 must clear x_prev/x_hist like V>0 (symmetric drain)"
    );
    assert_eq!(state[plan.layout.slot_off as usize], 0.0);
    // V=0 之后按新 epoch 输出（identity 图输出 = 新输入）。
    let mut out_zero_next = vec![0.0f64; block];
    let status = partitioned_block_f64(
        &topo,
        &plan,
        &plan_data,
        &mut state,
        &full,
        block,
        &mut out_zero_next,
        block,
        block,
        block,
    );
    assert!(status.is_ok());
    for n in 0..block {
        assert!((out_zero_next[n] - full[n]).abs() <= 1e-12);
    }

    // 非 identity（C04 F=[1,1,0,4]，B=2）的 DRAIN：块1 full，块2 V=1（流样本 0，输入 [1,2) 垃圾按 0）；
    // 输出 y[2]=1（[1,2,1,4,4] 第 3 个样本）、[1,2) 写 +0.0；之后历史清零 = 新 epoch。
    let c04 = topo_1x1(0, 1, 2, 3);
    let (plan_c04, data_c04) = build_plan_f64(&c04, 2, &[1.0], &[0.0, 2.0], &[1.0, -1.0, 2.0]);
    let mut state_c04 = vec![0.0f64; plan_c04.layout.state_samples()];
    partitioned_state_zero_f64(&mut state_c04);
    let mut out_c04_1 = [0.0f64; 2];
    assert!(partitioned_block_f64(
        &c04,
        &plan_c04,
        &data_c04,
        &mut state_c04,
        &[1.0, 1.0],
        2,
        &mut out_c04_1,
        2,
        2,
        2
    )
    .is_ok());
    assert!((out_c04_1[0] - 1.0).abs() <= 1e-12 && (out_c04_1[1] - 2.0).abs() <= 1e-12);
    let mut out_c04_2 = [7.0f64; 2];
    assert!(partitioned_block_f64(
        &c04,
        &plan_c04,
        &data_c04,
        &mut state_c04,
        &[0.0, 999.0],
        2,
        &mut out_c04_2,
        2,
        2,
        1
    )
    .is_ok());
    assert!(
        (out_c04_2[0] - 1.0).abs() <= 1e-12,
        "C04 y[2]={} want 1",
        out_c04_2[0]
    );
    assert!(out_c04_2[1] == 0.0);
    let live_end_c04 = plan_c04.layout.work_off as usize;
    assert!(state_c04[..live_end_c04].iter().all(|v| *v == 0.0));
    assert_eq!(state_c04[plan_c04.layout.slot_off as usize], 0.0);
    // 新 epoch 第一个 full 块 x=[7,0] -> [7,7]（F=[1,1,0,4] 的前 2 样本）。
    let mut out_c04_3 = [0.0f64; 2];
    assert!(partitioned_block_f64(
        &c04,
        &plan_c04,
        &data_c04,
        &mut state_c04,
        &[7.0, 0.0],
        2,
        &mut out_c04_3,
        2,
        2,
        2
    )
    .is_ok());
    assert!((out_c04_3[0] - 7.0).abs() <= 1e-12 && (out_c04_3[1] - 7.0).abs() <= 1e-12);
    println!("DRAIN PASS V=8/V=3/V=0(symmetric reset)/C04 non-identity partial");
}

// ---------------------------------------------------------------------------
// 测试 13：完整 4.1.2 7 角色 5x5 多分区（A/G/P 全非平凡）vs direct
// ---------------------------------------------------------------------------

#[test]
fn partitioned_4_1_2_multipartition_vs_direct() {
    let topo = topo_4_1_2(1, 2, 5, 2); // L_F = 2+5+2-2 = 7
    let a = synth_a(7, 2);
    let g = synth_g(5, 5);
    let p = synth_p(7, 2);
    let mut input = vec![vec![0.0f64; 9]; 7];
    for i in 0..7 {
        for n in 0..9 {
            input[i][n] = (((i * 3 + n * 5) % 7) as f64 - 3.0) * 0.125;
        }
    }
    let a32: Vec<f32> = a.iter().map(|v| *v as f32).collect();
    let g32: Vec<f32> = g.iter().map(|v| *v as f32).collect();
    let p32: Vec<f32> = p.iter().map(|v| *v as f32).collect();
    let mut checks = 0usize;
    for block in [2usize, 4, 8] {
        let got = run_case_f64(&topo, block, &a, &g, &p, &input, 6);
        let direct = run_direct_case_f64(&topo, block, &a, &g, &p, &input, 6);
        let l_out = 9 + 7 - 1;
        let got_trunc: Vec<Vec<f64>> = got.iter().map(|ch| ch[..l_out].to_vec()).collect();
        let direct_trunc: Vec<Vec<f64>> = direct.iter().map(|ch| ch[..l_out].to_vec()).collect();
        let id = format!("4.1.2-multipart/B{}", block);
        let (rel_l2, max_abs, peak) = check_f64_against(
            &id,
            &direct_trunc,
            &got_trunc,
            F64_REL_L2_MAX,
            F64_MAX_ABS_FACTOR,
        );
        println!(
            "PASS {} f64 vs direct rel_l2={:.3e} max_abs={:.3e} peak={:.3e}",
            id, rel_l2, max_abs, peak
        );
        // 未使用容器 ch6 逐样本严格 +0.0。
        assert!(
            got_trunc[6].iter().all(|v| *v == 0.0),
            "{}: unused container ch6 must be +0.0",
            id
        );
        // f32 RT 入口对同一 f64 direct 参考；门为 T08 f32 工程门。
        let got32 = run_case_f32(&topo, block, &a32, &g32, &p32, &input, 6);
        let got32_trunc: Vec<Vec<f32>> = got32.iter().map(|ch| ch[..l_out].to_vec()).collect();
        let (rel32, abs32, peak32) = check_f32_against(
            &format!("{}-f32", id),
            &direct_trunc,
            &got32_trunc,
            F32_REL_L2_MAX,
            F32_MAX_ABS_FACTOR,
        );
        println!(
            "PASS {} f32 rel_l2={:.3e} max_abs={:.3e} peak={:.3e}",
            id, rel32, abs32, peak32
        );
        // f32 partitioned vs f32 direct（同精度、同 Graph；门仍用 T08 f32 工程门）。
        let direct32 = run_direct_case_f32(&topo, block, &a32, &g32, &p32, &input, 6);
        let direct32_trunc: Vec<Vec<f32>> =
            direct32.iter().map(|ch| ch[..l_out].to_vec()).collect();
        let direct32_as_f64: Vec<Vec<f64>> = direct32_trunc
            .iter()
            .map(|ch| ch.iter().map(|v| *v as f64).collect())
            .collect();
        check_f32_against(
            &format!("{}-f32-vs-direct", id),
            &direct32_as_f64,
            &got32_trunc,
            F32_REL_L2_MAX,
            F32_MAX_ABS_FACTOR,
        );
        checks += 2;
    }
    assert_eq!(checks, 6);
    println!("4.1.2 5x5 multipartition PASS checks={}", checks);
}

// ---------------------------------------------------------------------------
// 测试 14：RT 路径零分配（计数分配器；f32 生产入口与 f64 oracle 均 0 alloc）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_rt_path_zero_alloc() {
    let topo = topo_4_1_2(1, 2, 5, 2);
    let block = 4usize;
    let a = synth_a(7, 2);
    let g = synth_g(5, 5);
    let p = synth_p(7, 2);
    let a32: Vec<f32> = a.iter().map(|v| *v as f32).collect();
    let g32: Vec<f32> = g.iter().map(|v| *v as f32).collect();
    let p32: Vec<f32> = p.iter().map(|v| *v as f32).collect();
    let (plan32, data32) = build_plan_f32(&topo, block, &a32, &g32, &p32);
    let (plan64, data64) = build_plan_f64(&topo, block, &a, &g, &p);
    let containers = topo.container_channels as usize;
    let n_src = topo.n_src as usize;
    let in_block = vec![0.0f32; n_src * block];
    let mut out_block = vec![0.0f32; containers * block];
    let mut state32 = vec![0.0f32; plan32.layout.state_samples()];
    partitioned_state_zero_f32(&mut state32);
    let in_block64 = vec![0.0f64; n_src * block];
    let mut out_block64 = vec![0.0f64; containers * block];
    let mut state64 = vec![0.0f64; plan64.layout.state_samples()];
    partitioned_state_zero_f64(&mut state64);

    // 预热（TLS/首次触碰），并验证基线 OK。
    alloc_counter_warmup();
    let status = partitioned_block_f32(
        &topo,
        &plan32,
        &data32,
        &mut state32,
        &in_block,
        block,
        &mut out_block,
        block,
        block,
        block,
    );
    assert!(status.is_ok());

    let before32 = alloc_count();
    let mut calls = 0usize;
    while calls < 8 {
        let status = partitioned_block_f32(
            &topo,
            &plan32,
            &data32,
            &mut state32,
            &in_block,
            block,
            &mut out_block,
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

    let status = partitioned_block_f64(
        &topo,
        &plan64,
        &data64,
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
        let status = partitioned_block_f64(
            &topo,
            &plan64,
            &data64,
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

    // DRAIN/partial 路径（V=3 与 V=0，含历史清零）也必须 0 分配。
    let mut drain_state = vec![0.0f32; plan32.layout.state_samples()];
    partitioned_state_zero_f32(&mut drain_state);
    let before_drain = alloc_count();
    let status = partitioned_block_f32(
        &topo,
        &plan32,
        &data32,
        &mut drain_state,
        &in_block,
        block,
        &mut out_block,
        block,
        block,
        3,
    );
    assert!(status.is_ok());
    let status = partitioned_block_f32(
        &topo,
        &plan32,
        &data32,
        &mut drain_state,
        &in_block,
        block,
        &mut out_block,
        block,
        block,
        0,
    );
    assert!(status.is_ok());
    let after_drain = alloc_count();
    assert_eq!(
        after_drain,
        before_drain,
        "DRAIN path allocated {} times",
        after_drain - before_drain
    );
    println!(
        "zero-alloc PASS f32 8 blocks=0 alloc, f64 8 blocks=0 alloc, DRAIN(V=3/V=0)=0 alloc (plan_samples={} state_samples={})",
        plan32.layout.plan_samples(),
        plan32.layout.state_samples()
    );
}

// ---------------------------------------------------------------------------
// 测试 15：负向参数校验（固定错误码，不 panic）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_negative_calls_rejected() {
    let topo = DirectTopology::identity();
    let block = 8usize;
    let layout = PartitionedLayout::build(&topo, block).expect("layout");
    let (plan, plan_data) = build_plan_f64(&topo, block, &[1.0], &[0.0], &[1.0]);
    let mut state = vec![0.0f64; layout.state_samples()];
    let input = vec![0.0f64; block];
    let mut output = vec![0.0f64; block];
    assert!(partitioned_block_f64(
        &topo,
        &plan,
        &plan_data,
        &mut state,
        &input,
        block,
        &mut output,
        block,
        block,
        block
    )
    .is_ok());

    // state 太小。
    let mut tiny_state = vec![0.0f64; layout.state_samples() - 1];
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            &plan_data,
            &mut tiny_state,
            &input,
            block,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_CAPACITY.code()
    );
    // plan_data 太小。
    let short_plan = &plan_data[..plan_data.len() - 1];
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            short_plan,
            &mut state,
            &input,
            block,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_CAPACITY.code()
    );
    // stride 小于 block。
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            &plan_data,
            &mut state,
            &input,
            block - 1,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_STRIDE_ALIGN.code()
    );
    // 输入/输出缓冲不足。
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            &plan_data,
            &mut state,
            &input[..block - 1],
            block,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_CAPACITY.code()
    );
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            &plan_data,
            &mut state,
            &input,
            block,
            &mut output[..block - 1],
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_CAPACITY.code()
    );
    // block 大小：0/129 由布局拒绝（ERR_BLOCK_SIZE；B=3 已受支持，不再是错误）。
    for bad in [0usize, 129] {
        assert_eq!(
            partitioned_block_f64(
                &topo,
                &plan,
                &plan_data,
                &mut state,
                &input,
                block,
                &mut output,
                block,
                bad,
                bad
            )
            .code(),
            RtStatus::ERR_BLOCK_SIZE.code()
        );
    }
    // plan 为 B=8，调用 B=4：布局不符（ERR_BAD_LAYOUT）；valid_frames > block 同样拒绝。
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            &plan_data,
            &mut state,
            &input,
            block,
            &mut output,
            block,
            4,
            4
        )
        .code(),
        RtStatus::ERR_BAD_LAYOUT.code()
    );
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &plan,
            &plan_data,
            &mut state,
            &input,
            block,
            &mut output,
            block,
            block,
            block + 1
        )
        .code(),
        RtStatus::ERR_BAD_LAYOUT.code()
    );
    // 篡改路由表 / 布局：固定错误码。
    let mut bad_route = plan;
    bad_route.route_src[0] = 7;
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &bad_route,
            &plan_data,
            &mut state,
            &input,
            block,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_PATH_TABLE.code()
    );
    let mut bad_layout = plan;
    bad_layout.layout.taps = 3;
    assert_eq!(
        partitioned_block_f64(
            &topo,
            &bad_layout,
            &plan_data,
            &mut state,
            &input,
            block,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_BAD_LAYOUT.code()
    );
    // 非法拓扑：n_src=0 -> ERR_CHANNELS（拓扑校验优先于布局）。
    let mut bad_topo = DirectTopology::identity();
    bad_topo.n_src = 0;
    assert_eq!(
        partitioned_block_f64(
            &bad_topo,
            &plan,
            &plan_data,
            &mut state,
            &input,
            block,
            &mut output,
            block,
            block,
            block
        )
        .code(),
        RtStatus::ERR_CHANNELS.code()
    );
    println!("negative calls PASS (fixed RtStatus codes, no panic)");
}

// ---------------------------------------------------------------------------
// 测试 16：RT 可见类型 / 容量常量 / 编译期契约
// ---------------------------------------------------------------------------

#[test]
fn partitioned_rt_visible_types_and_capacity_consts() {
    assert!(!core::mem::needs_drop::<PartitionedLayout>());
    assert!(!core::mem::needs_drop::<PartitionedPlan>());
    assert!(!core::mem::needs_drop::<DirectTopology>());
    assert_eq!(PARTITIONED_MAX_BLOCK, 128);
    assert_eq!(PARTITIONED_MAX_FFT, 256);
    assert_eq!(PARTITIONED_MAX_ROLES, DIRECT_MAX_ROLES);
    assert_eq!(PARTITIONED_MAX_PARTICIPANTS, DIRECT_MAX_PARTICIPANTS);
    assert_eq!(PARTITIONED_MAX_ROUTES, khaen_core::rt::MAX_MATRIX_PATHS);
    assert_eq!(
        PARTITIONED_MAX_PARTITIONS,
        khaen_core::rt::MAX_PARTITIONS as usize
    );
    assert_eq!(
        PARTITIONED_MAX_TAPS,
        khaen_core::rt::TAPS_NORMAL_MAX as usize
    );
    assert_eq!(
        PARTITIONED_MAX_TAPS_EXTENDED,
        khaen_core::rt::TAPS_EXTENDED_MAX as usize
    );
    assert_eq!(PARTITIONED_MAX_TAPS_EXTENDED, 16_384);
    // 布局 = 8 个 u16（含 window_samples）+ 8 个 u32 = 48 B；POD、无 Drop、无引用。
    assert_eq!(core::mem::size_of::<PartitionedLayout>(), 48);
    assert!(core::mem::size_of::<PartitionedLayout>() <= 64);
    assert_eq!(core::mem::needs_drop::<PartitionedPlan>(), false);
    assert!(
        PARTITIONED_MAX_ROLES + PARTITIONED_MAX_PARTICIPANTS * (PARTITIONED_MAX_PARTICIPANTS - 1)
            <= PARTITIONED_MAX_ROUTES
    );
    println!(
        "rt types PASS size_of(layout)={} plan={}",
        core::mem::size_of::<PartitionedLayout>(),
        core::mem::size_of::<PartitionedPlan>()
    );
}

// ---------------------------------------------------------------------------
// 测试 17：RT 模块源码扫描（无分配/锁/syscall/日志/panic 记号；与 rt-check 同规则）
// ---------------------------------------------------------------------------

#[test]
fn partitioned_rt_module_source_scan() {
    let text = include_str!("../../src/partitioned.rs");
    assert!(
        text.contains("RT-MODULE:"),
        "T08 RT module must carry RT-MODULE marker"
    );
    let forbidden = [
        "Vec<",
        "Vec::",
        "vec![",
        "String",
        "Box<",
        "Box::new",
        "Arc<",
        "Rc<",
        "Weak<",
        "Mutex",
        "RwLock",
        "OnceLock",
        "Condvar",
        "parking_lot",
        "std::sync",
        "thread_local",
        "std::time",
        "Instant::now",
        "SystemTime::now",
        "std::fs",
        "std::net",
        "std::process",
        "println!",
        "eprintln!",
        "format!(",
        "to_string(",
        "panic!",
        "unwrap(",
        "expect(",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "assert!(",
        "unsafe {",
        "extern \"C\"",
        "no_mangle",
        "drop(",
        "drop_in_place",
        "dealloc(",
        "catch_unwind",
        "std::env",
        "/proc/",
        "/sys/",
        "clock_gettime",
        "libc::",
        "thread::spawn",
        "mmap(",
        "mlock(",
    ];
    let mut in_const_assert = false;
    let mut scanned_lines = 0usize;
    for (index, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.starts_with("const _: () = {") {
            in_const_assert = true;
            continue;
        }
        if in_const_assert {
            if trimmed == "};" {
                in_const_assert = false;
            }
            continue;
        }
        let code = match raw.find("//") {
            Some(pos) => &raw[..pos],
            None => raw,
        };
        if code.trim().is_empty() {
            continue;
        }
        scanned_lines += 1;
        for token in forbidden {
            assert!(
                !code.contains(token),
                "forbidden token {:?} at line {}",
                token,
                index + 1
            );
        }
    }
    assert!(
        scanned_lines > 400,
        "unexpected scan size {}",
        scanned_lines
    );
    println!(
        "RT source scan PASS lines={} tokens={} (no alloc/lock/syscall/panic)",
        scanned_lines,
        forbidden.len()
    );
}
