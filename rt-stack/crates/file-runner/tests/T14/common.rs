//! T14 测试公共夹具：合成 WAV / `tspi-fir/v1` manifest / `tspi-cal-ir-draft/v1` / session。
//!
//! 全部夹具在内存里构造（`MapSource`），不依赖板端/ALSA/网络/本地文件；确定性（无时间戳/随机）。

#![allow(dead_code)]

use khaen_file_runner::fir::{build_fir_manifest, magnitude_db_at, FirManifestSpec};
use khaen_file_runner::graph::{self, PreparedSession};
use khaen_file_runner::json::Json;
use khaen_file_runner::runner::{self, RunOptions, RunResult};
pub use khaen_file_runner::session::MapSource;
use khaen_file_runner::sha256::sha256_hex;
use khaen_file_runner::wav::encode_wav_f32;
use khaen_file_runner::RunnerResult;

/// 夹具采样率。
pub const FS: u32 = 48_000;
/// 角色名（2.0 -> 4.1.2：FL/FR/C/SL/SR/SUB1）。
pub const ROLE_NAMES: [&str; 6] = ["FL", "FR", "C", "SL", "SR", "SUB1"];
/// 角色 -> 播放容器（WBS §3 唯一映射子集：SUB1=7，ch6 空）。
pub const ROLE_SCATTER: [u16; 6] = [0, 1, 2, 3, 4, 7];
/// 低带 participants（dense 顺序）。
pub const PARTICIPANTS: [&str; 3] = ["FL", "FR", "SUB1"];
/// 合法 64 hex（64 个 'a'）。
pub const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// 合法 64 hex（64 个 'b'）。
pub const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// f32 系数 -> raw_f32le 字节。
pub fn raw_f32le(coeffs: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(coeffs.len() * 4);
    for value in coeffs {
        out.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    out
}

/// 系数是否纯延迟（仅一个非零且 = 1.0）。
fn pure_delay(coeffs: &[f32]) -> bool {
    let mut nonzeros = 0;
    let mut unit = false;
    for &value in coeffs {
        if value != 0.0 {
            nonzeros += 1;
            unit = value == 1.0;
        }
    }
    nonzeros == 1 && unit
}

/// 端点 JSON（measured）。
pub fn endpoint_measured(freq_hz: f64, mag_db: f64) -> Json {
    Json::from_entries(vec![
        ("status", Json::String("measured".to_string())),
        ("freq_hz", Json::Number(freq_hz)),
        ("mag_db", Json::Number(mag_db)),
        ("phase_deg", Json::Number(0.0)),
    ])
}

/// 端点 JSON（not_applicable_at_fs）。
pub fn endpoint_na() -> Json {
    Json::from_entries(vec![
        ("status", Json::String("not_applicable_at_fs".to_string())),
        ("reason", Json::String("nyquist below endpoint".to_string())),
    ])
}

/// 构造 `tspi-fir/v1` manifest JSON + raw_f32le 系数文件字节（默认 fs=48k）。
pub fn fir(family: &str, coeffs: &[f32], file: &str) -> (Json, Vec<u8>) {
    fir_custom(family, coeffs, file, "regular", 4096)
}

/// 指定采样率的 manifest 构造（96k 负向/版本敏感性用例）。
pub fn fir_fs(family: &str, coeffs: &[f32], file: &str, fs: u32) -> (Json, Vec<u8>) {
    fir_custom_fs(family, coeffs, file, "regular", 4096, fs)
}

/// 定制 profile / route_taps_cap / fs 的 manifest 构造。
pub fn fir_custom_fs(
    family: &str,
    coeffs: &[f32],
    file: &str,
    profile: &str,
    taps_cap: u32,
    fs: u32,
) -> (Json, Vec<u8>) {
    let bytes = raw_f32le(coeffs);
    let sha = sha256_hex(&bytes);
    let taps = coeffs.len() as u32;
    let delay = match family {
        "linear" | "apodizing" | "brickwall" => (taps as f64 - 1.0) / 2.0,
        _ => 0.0,
    };
    let gain = if family == "nos" || pure_delay(coeffs) {
        0.0
    } else {
        magnitude_db_at(coeffs, fs as f64, 1000.0)
    };
    let manifest = build_fir_manifest(&FirManifestSpec {
        family,
        fs,
        taps,
        delay_samples: delay,
        tool_name: "t14-fixture",
        tool_version: "1.0.0",
        coefficients_file: file,
        coefficients_format: "raw_f32le",
        coefficients_bytes: bytes.len() as u64,
        coefficients_sha256: &sha,
        profile,
        route_taps_cap: taps_cap,
        ram_bytes_f32: taps as u64 * 4,
        fits_budget: true,
        reference_hz: 1000.0,
        reference_gain_db: gain,
        measured_gain_db: None,
        endpoint_f20: endpoint_measured(20.0, magnitude_db_at(coeffs, fs as f64, 20.0)),
        endpoint_f20k: endpoint_measured(20_000.0, magnitude_db_at(coeffs, fs as f64, 20_000.0)),
        design: None,
        source: None,
        verification_method: "t14-fixture/analytic",
        warnings: Vec::new(),
    })
    .expect("fixture manifest must build");
    (manifest, bytes)
}

/// 定制 profile / route_taps_cap 的 manifest 构造。
pub fn fir_custom(
    family: &str,
    coeffs: &[f32],
    file: &str,
    profile: &str,
    taps_cap: u32,
) -> (Json, Vec<u8>) {
    fir_custom_fs(family, coeffs, file, profile, taps_cap, FS)
}

/// 单位冲激（nos 家族）：h=[1.0]，delay=0。
pub fn nos(file: &str) -> (Json, Vec<u8>) {
    fir("nos", &[1.0], file)
}

/// taps=3、delay=1 的线性相位纯延迟 h=[0,1,0]（L_P = 3）。
pub fn linear_delay_one(file: &str) -> (Json, Vec<u8>) {
    fir("linear", &[0.0, 1.0, 0.0], file)
}

/// taps=ts、终点冲激 h=[0,…,0,1]（min_fast，声明 delay=0，L=ts）。
pub fn min_fast_delay(file: &str, taps: usize) -> (Json, Vec<u8>) {
    let mut coeffs = vec![0.0f32; taps];
    coeffs[taps - 1] = 1.0;
    fir("min_fast", &coeffs, file)
}

/// 深拷贝 JSON 对象并设置字段（夹具变异用）。
pub fn set_field(value: &mut Json, key: &str, field: Json) {
    match value {
        Json::Object(map) => {
            map.insert(key.to_string(), field);
        }
        other => panic!("set_field on non-object: {:?}", other),
    }
}

/// 深拷贝 JSON 对象并删除字段。
pub fn remove_field(value: &mut Json, key: &str) {
    if let Json::Object(map) = value {
        map.remove(key);
    }
}

/// 可变对象视图（夹具变异用）。
pub fn map_mut(value: &mut Json) -> &mut std::collections::BTreeMap<String, Json> {
    match value {
        Json::Object(map) => map,
        other => panic!("map_mut on non-object: {:?}", other),
    }
}

/// 按路径取可变子对象（夹具变异用）。
pub fn nested_mut<'a>(value: &'a mut Json, keys: &[&str]) -> &'a mut Json {
    let mut current = value;
    for key in keys {
        current = match current {
            Json::Object(map) => map.get_mut(*key).expect("fixture nested field must exist"),
            other => panic!("nested_mut on non-object: {:?}", other),
        };
    }
    current
}

/// 取对象字段（克隆）。
pub fn field(value: &Json, key: &str) -> Json {
    match value {
        Json::Object(map) => map.get(key).cloned().expect("fixture field must exist"),
        other => panic!("field on non-object: {:?}", other),
    }
}

/// 手写 RIFF/WAVE 容器（非 f32 格式的负向/正向用例）。
pub fn wav_container(format: u16, bits: u16, channels: u16, fs: u32, data: &[u8]) -> Vec<u8> {
    let block_align = channels * (bits / 8);
    let byte_rate = fs * block_align as u32;
    let pad = (data.len() & 1) as usize;
    let riff_payload = 4 + (8 + 16) + (8 + data.len() + pad);
    let mut out = Vec::with_capacity(8 + riff_payload);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_payload as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&format.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&fs.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    for _ in 0..pad {
        out.push(0);
    }
    out
}

/// 手写 WAVE_FORMAT_EXTENSIBLE 容器（valid_bits + SubFormat GUID）。
pub fn wav_extensible(
    format: u16,
    bits: u16,
    valid_bits: u16,
    channels: u16,
    fs: u32,
    subformat: [u8; 16],
    data: &[u8],
) -> Vec<u8> {
    let block_align = channels * (bits / 8);
    let byte_rate = fs * block_align as u32;
    let pad = (data.len() & 1) as usize;
    let riff_payload = 4 + (8 + 40) + (8 + data.len() + pad);
    let mut out = Vec::with_capacity(8 + riff_payload);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_payload as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&0xFFFEu16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&fs.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    let mut extension = [0u8; 24];
    extension[0..2].copy_from_slice(&22u16.to_le_bytes());
    extension[2..4].copy_from_slice(&valid_bits.to_le_bytes());
    extension[4..8].copy_from_slice(&0u32.to_le_bytes());
    extension[8..24].copy_from_slice(&subformat);
    out.extend_from_slice(&extension);
    let _ = format;
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    for _ in 0..pad {
        out.push(0);
    }
    out
}

/// PCM / IEEE float 的 KSDATAFORMAT_SUBTYPE 字节。
pub fn subformat(kind: &str) -> [u8; 16] {
    let first: u16 = match kind {
        "pcm" => 1,
        "float" => 3,
        other => panic!("unknown subformat {}", other),
    };
    let mut out = [0u8; 16];
    out[0..2].copy_from_slice(&first.to_le_bytes());
    out[2..4].copy_from_slice(&0u16.to_le_bytes());
    out[4..8].copy_from_slice(&0x0010_0000u32.to_le_bytes());
    out[8..].copy_from_slice(&[0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
    out
}

/// i16 PCM 样本 -> 字节。
pub fn pcm16(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for value in samples {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// 会话夹具（`khaen-file-runner/v1`）。
#[derive(Clone, Debug)]
pub struct Fixture {
    pub fs: u32,
    pub container_channels: u16,
    pub input_channels: u16,
    pub block_size: u16,
    pub workers: u8,
    pub plan_version: u32,
    pub profile: String,
    pub max_taps: u32,
    pub extended_admitted: bool,
    pub d_samples: u32,
    pub layout_id: String,
    pub roles: Vec<(String, Option<u16>, u16)>,
    pub participants: Vec<String>,
    pub a: Vec<(u16, Json, Vec<u8>)>,
    pub g: Vec<(u16, u16, Json, Vec<u8>)>,
    pub p: Vec<(String, Json, Vec<u8>)>,
    pub ir: Option<Json>,
    pub protection: Option<Json>,
    pub graph_id: Option<String>,
    pub input_planar: Vec<f32>,
    pub with_output: bool,
    pub binding: Option<Json>,
}

impl Fixture {
    /// 默认图：2.0 -> 4.1.2，A/G/P 全单位冲激（L_F=1、flush=0）。
    pub fn identity(frames: usize) -> Self {
        let mut roles = Vec::new();
        let mut p = Vec::new();
        for (index, name) in ROLE_NAMES.iter().enumerate() {
            let src = if index == 0 {
                Some(0u16)
            } else if index == 1 {
                Some(1u16)
            } else {
                None
            };
            roles.push((name.to_string(), src, ROLE_SCATTER[index]));
            let (manifest, bytes) = nos(&format!("p_{}.raw", name));
            p.push((name.to_string(), manifest, bytes));
        }
        let a = vec![
            (0u16, nos("a0.raw").0, nos("a0.raw").1),
            (1u16, nos("a1.raw").0, nos("a1.raw").1),
        ];
        let mut fixture = Self {
            fs: FS,
            container_channels: 8,
            input_channels: 2,
            block_size: 128,
            workers: 4,
            plan_version: 1,
            profile: "regular".to_string(),
            max_taps: 4096,
            extended_admitted: false,
            d_samples: 0,
            layout_id: "4.1.2".to_string(),
            roles,
            participants: PARTICIPANTS.iter().map(|name| name.to_string()).collect(),
            a,
            g: Vec::new(),
            p,
            ir: None,
            protection: None,
            graph_id: None,
            input_planar: Vec::new(),
            with_output: true,
            binding: None,
        };
        fixture.set_frames(frames);
        fixture
    }

    /// 替换输入样本（确定性周期斜坡；恒落在 [-0.5, 0.5]，不触发 decode clip）。
    pub fn set_frames(&mut self, frames: usize) {
        let mut planar = Vec::with_capacity(2 * frames);
        for frame in 0..frames {
            planar.push((frame % 64) as f32 / 64.0 - 0.5);
        }
        for frame in 0..frames {
            planar.push(0.5 - (frame % 32) as f32 / 64.0);
        }
        self.input_planar = planar;
    }

    /// 输入帧数。
    pub fn input_frames(&self) -> usize {
        self.input_planar.len() / self.input_channels as usize
    }

    /// 替换所有 P 系数（同家族同 taps）。
    pub fn set_p(&mut self, family: &str, coeffs: &[f32]) {
        for (name, manifest, bytes) in self.p.iter_mut() {
            let (new_manifest, new_bytes) = fir(family, coeffs, &format!("p_{}.raw", name));
            *manifest = new_manifest;
            *bytes = new_bytes;
        }
    }

    /// 替换所有 P 系数（自定义 profile / taps cap）。
    pub fn set_p_custom(&mut self, family: &str, coeffs: &[f32], profile: &str, cap: u32) {
        for (name, manifest, bytes) in self.p.iter_mut() {
            let (new_manifest, new_bytes) =
                fir_custom(family, coeffs, &format!("p_{}.raw", name), profile, cap);
            *manifest = new_manifest;
            *bytes = new_bytes;
        }
    }

    /// 改变采样率并同步重建全部系数 manifest（96k 版本敏感性/负向用例）。
    pub fn set_fs(&mut self, fs: u32) {
        self.fs = fs;
        for (src, manifest, bytes) in self.a.iter_mut() {
            let (new_manifest, new_bytes) = fir_fs("nos", &[1.0], &format!("a{}.raw", src), fs);
            *manifest = new_manifest;
            *bytes = new_bytes;
        }
        for (name, manifest, bytes) in self.p.iter_mut() {
            let (new_manifest, new_bytes) = fir_fs("nos", &[1.0], &format!("p_{}.raw", name), fs);
            *manifest = new_manifest;
            *bytes = new_bytes;
        }
        self.set_frames(self.input_frames());
    }

    /// 设置角色 scatter。
    pub fn set_scatter(&mut self, role: &str, scatter: u16) {
        for (name, _, current) in self.roles.iter_mut() {
            if name == role {
                *current = scatter;
            }
        }
    }

    /// 增加 insulation G cell（residual）。
    pub fn set_g(&mut self, row: u16, col: u16, family: &str, coeffs: &[f32]) {
        let file = format!("g_{}_{}.raw", row, col);
        let (manifest, bytes) = fir(family, coeffs, &file);
        self.g.push((row, col, manifest, bytes));
    }

    /// 构造 session JSON 文本 + blob 源。
    pub fn build(&self) -> (String, MapSource) {
        let mut source = MapSource::new();
        let wav = encode_wav_f32(&self.input_planar, self.input_channels, self.fs)
            .expect("fixture input wav must encode");
        source.insert("input.wav", wav);

        let mut a_entries = Vec::new();
        for (src, manifest, bytes) in &self.a {
            let file = format!("a{}.raw", src);
            source.insert(file.clone(), bytes.clone());
            a_entries.push(Json::from_entries(vec![
                ("src", Json::Number(*src as f64)),
                ("file", Json::String(file)),
                ("manifest", manifest.clone()),
            ]));
        }
        let mut g_entries = Vec::new();
        for (row, col, manifest, bytes) in &self.g {
            let file = format!("g_{}_{}.raw", row, col);
            source.insert(file.clone(), bytes.clone());
            g_entries.push(Json::from_entries(vec![
                ("row", Json::Number(*row as f64)),
                ("col", Json::Number(*col as f64)),
                ("file", Json::String(file)),
                ("manifest", manifest.clone()),
            ]));
        }
        let mut p_entries = Vec::new();
        for (role, manifest, bytes) in &self.p {
            let file = format!("p_{}.raw", role);
            source.insert(file.clone(), bytes.clone());
            p_entries.push(Json::from_entries(vec![
                ("role", Json::String(role.clone())),
                ("file", Json::String(file)),
                ("manifest", manifest.clone()),
            ]));
        }

        let mut audio_entries = vec![
            ("sample_rate_hz", Json::Number(self.fs as f64)),
            (
                "container_channels",
                Json::Number(self.container_channels as f64),
            ),
            ("input_channels", Json::Number(self.input_channels as f64)),
            ("layout_id", Json::String(self.layout_id.clone())),
            (
                "input",
                Json::from_entries(vec![("file", Json::String("input.wav".to_string()))]),
            ),
        ];
        if let Some(binding) = &self.binding {
            audio_entries.push(("binding", binding.clone()));
        }

        let mut engine_entries = vec![
            ("abi_version", Json::Number(1.0)),
            ("plan_version", Json::Number(self.plan_version as f64)),
            ("block_size", Json::Number(self.block_size as f64)),
            ("expected_workers", Json::Number(self.workers as f64)),
            ("profile", Json::String(self.profile.clone())),
            ("max_taps", Json::Number(self.max_taps as f64)),
            ("extended_taps_admitted", Json::Bool(self.extended_admitted)),
        ];
        if let Some(graph_id) = &self.graph_id {
            engine_entries.push(("graph_id", Json::String(graph_id.clone())));
        }

        let mut role_entries = Vec::new();
        for (position, (name, src, scatter)) in self.roles.iter().enumerate() {
            role_entries.push(Json::from_entries(vec![
                ("name", Json::String(name.clone())),
                ("index", Json::Number(position as f64)),
                (
                    "src",
                    match src {
                        Some(value) => Json::Number(*value as f64),
                        None => Json::Null,
                    },
                ),
                ("scatter", Json::Number(*scatter as f64)),
            ]));
        }

        let mut root = vec![
            ("schema", Json::String("khaen-file-runner/v1".to_string())),
            ("precision", Json::String("f32-strict".to_string())),
            ("audio", Json::from_entries(audio_entries)),
            ("engine", Json::from_entries(engine_entries)),
            ("roles", Json::Array(role_entries)),
            (
                "participants",
                Json::Array(
                    self.participants
                        .iter()
                        .map(|name| Json::String(name.clone()))
                        .collect(),
                ),
            ),
            ("d_samples", Json::Number(self.d_samples as f64)),
            (
                "coefficients",
                Json::from_entries(vec![
                    ("a", Json::Array(a_entries)),
                    ("g", Json::Array(g_entries)),
                    ("p", Json::Array(p_entries)),
                ]),
            ),
        ];
        if let Some(ir) = &self.ir {
            root.push(("ir", ir.clone()));
        }
        if let Some(protection) = &self.protection {
            root.push(("protection", protection.clone()));
        }
        if self.with_output {
            root.push((
                "output",
                Json::from_entries(vec![("format", Json::String("wav_f32".to_string()))]),
            ));
        }
        (Json::from_entries(root).to_canonical(), source)
    }

    /// 解析 session（不 prepare）。
    pub fn session_text(&self) -> String {
        self.build().0
    }

    /// prepare（加载 blob + 编译 + 图版本 + 保护）。
    pub fn prepare(&self) -> RunnerResult<PreparedSession> {
        let (text, source) = self.build();
        graph::prepare_from_json(&text, &source)
    }

    /// prepare 并执行（默认选项）。
    pub fn run(&self) -> RunnerResult<RunResult> {
        self.run_with(&RunOptions::default())
    }

    /// prepare 并按给定选项执行。
    pub fn run_with(&self, options: &RunOptions) -> RunnerResult<RunResult> {
        let prepared = self.prepare()?;
        runner::run(&prepared, options)
    }
}

/// 期望 identity 直通输出（容器通道 -> 输入样本；角色无源/未映射容器为 0）。
pub fn expected_identity_output(fixture: &Fixture) -> Vec<Vec<f32>> {
    let frames = fixture.input_frames();
    let n_src = fixture.input_channels as usize;
    let containers = fixture.container_channels as usize;
    let mut output = vec![vec![0.0f32; frames]; containers];
    for (position, (_, src, scatter)) in fixture.roles.iter().enumerate() {
        let _ = position;
        if let Some(src) = src {
            let channel = *scatter as usize;
            let base = (*src as usize) * frames;
            output[channel].copy_from_slice(&fixture.input_planar[base..base + frames]);
        }
    }
    let _ = n_src;
    output
}

/// 保护策略 JSON（默认触发集合可裁剪）。
pub fn protection(fallback: &str, triggers: &[&str], mute_on_exec_fault: bool) -> Json {
    Json::from_entries(vec![
        ("fallback", Json::String(fallback.to_string())),
        (
            "triggers",
            Json::Array(
                triggers
                    .iter()
                    .map(|trigger| Json::String((*trigger).to_string()))
                    .collect(),
            ),
        ),
        ("mute_on_exec_fault", Json::Bool(mute_on_exec_fault)),
    ])
}

/// 不触发任何保护（negative 测试用）。
pub fn protection_none() -> Json {
    protection("identity_p_post", &[], true)
}

/// `tspi-cal-ir-draft/v1` role_metadata 草案（P FIR=[1]；runner 应用 D/polarity）。
pub fn ir_role_metadata(delays: &[u32; 6], invert: &[bool; 6]) -> Json {
    ir_draft("role_metadata", 48_000, "4.1.2", delays, invert, None)
}

/// 定制 IR 草案。
pub fn ir_draft(
    encoding: &str,
    source_sample_rate: u32,
    layout: &str,
    delays: &[u32; 6],
    invert: &[bool; 6],
    curve: Option<Json>,
) -> Json {
    let mut role_entries = Vec::new();
    let mut post_entries = Vec::new();
    for (index, name) in ROLE_NAMES.iter().enumerate() {
        role_entries.push(Json::from_entries(vec![
            ("name", Json::String((*name).to_string())),
            ("index", Json::Number(ROLE_SCATTER[index] as f64)),
        ]));
        post_entries.push(Json::from_entries(vec![
            ("name", Json::String((*name).to_string())),
            ("index", Json::Number(ROLE_SCATTER[index] as f64)),
            ("delay_samples", Json::Number(delays[index] as f64)),
            ("delay_q16", Json::Number(0.0)),
            (
                "polarity",
                Json::String(if invert[index] { "invert" } else { "normal" }.to_string()),
            ),
            ("gain_db", Json::Number(0.0)),
            ("peq", Json::Array(Vec::new())),
            ("placement", Json::Array(Vec::new())),
            (
                "p_taps",
                Json::Number(if encoding == "coeff_embedded" {
                    delays[index] as f64 + 1.0
                } else {
                    1.0
                }),
            ),
        ]));
    }
    let mut root = vec![
        ("schema", Json::String("tspi-cal-ir-draft/v1".to_string())),
        ("ir_version", Json::Number(1.0)),
        ("ir_hash", Json::String(HASH_A.to_string())),
        (
            "hash_algo",
            Json::String("tspi-cal-ir-canonical-v1".to_string()),
        ),
        (
            "source",
            Json::from_entries(vec![
                ("schema", Json::String("khaen-calibration/v1".to_string())),
                ("source_sha256", Json::String(HASH_B.to_string())),
                ("sample_rate", Json::Number(source_sample_rate as f64)),
            ]),
        ),
        ("layout", Json::String(layout.to_string())),
        (
            "profile",
            Json::from_entries(vec![
                ("profile_id", Json::String("t14-profile".to_string())),
                ("profile_sha256", Json::String(HASH_A.to_string())),
                ("stereo_strict", Json::Bool(false)),
                (
                    "stereo_mask",
                    Json::Array(vec![
                        Json::String("FL".to_string()),
                        Json::String("FR".to_string()),
                    ]),
                ),
                ("target_profile", Json::String("4.1.2".to_string())),
            ]),
        ),
        ("roles", Json::Array(role_entries)),
        ("post", Json::Array(post_entries)),
        ("encoding", Json::String(encoding.to_string())),
    ];
    match curve {
        Some(value) => root.push(("curve", value)),
        None => root.push(("curve", Json::Null)),
    }
    root.push((
        "g_residual",
        Json::from_entries(vec![
            ("rule", Json::String("modal_residual_only".to_string())),
            ("participants", Json::Array(Vec::new())),
        ]),
    ));
    Json::from_entries(root)
}

/// 报告 JSON（canonical 文本 -> 解析值）。
pub fn report_json(result: &RunResult) -> Json {
    Json::parse(&result.metadata_json).expect("report must be valid JSON")
}

/// 取报告字符串字段。
pub fn report_str(report: &Json, path: &[&str]) -> String {
    let mut current = report;
    for key in path {
        current = match current {
            Json::Object(map) => map.get(*key).expect("report field must exist"),
            other => panic!("report path on non-object: {:?}", other),
        };
    }
    match current {
        Json::String(text) => text.clone(),
        other => panic!("report field is not string: {:?}", other),
    }
}

/// 取报告数字字段（f64）。
pub fn report_num(report: &Json, path: &[&str]) -> f64 {
    let mut current = report;
    for key in path {
        current = match current {
            Json::Object(map) => map.get(*key).expect("report field must exist"),
            other => panic!("report path on non-object: {:?}", other),
        };
    }
    match current {
        Json::Number(value) => *value,
        other => panic!("report field is not number: {:?}", other),
    }
}
