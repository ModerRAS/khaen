//! [NONRT+RT] MIMO 12 kHz 低频修正支路 host 接线（task-253）。
//!
//! 拓扑（docs/MIMO-COEFF-PLAN.md §1.2）：
//! ~~~text
//! out_j[n] = direct_j[n-Δsys] + Σ_i (h_int * 4↑(g_ji * (h_dec * x_i)))[n]
//! ~~~
//! host plan-arena 图只承担**直通支路**：plan.bin 用 G=identity、baseline D=Δsys、
//! P=identity，图输出即 `feed_j[n-Δsys]`；本模块从图输入取 M 个 participant 馈送，
//! 跑 12 kHz 修正支路（core::mimo12k，复用 T08 UPOLS），把修正量加到对应容器输出。
//! 这等价于“G_modal 在 P_post 之前”的语义**仅当 P_post=identity**；plan 校验见
//! 调用方（rt-host main.rs 会拒绝非 identity P/G、非 Δsys baseline 的 plan）。
//!
//! 产物（离线设计输出，见 rt-stack/evidence/mimo-12k-v2/artifacts/）：
//! - `params.json`：schema/采样率/taps/D12/resamp taps/Δsys/participants/g_file/resamp_file；
//! - `g12_v2_4096.f32`：g[j][i][tap] f32le，M×M×taps（不含 δ_D baseline）；
//! - `resamp95.f32`：48 kHz 抽取低通（单位 DC 增益；插值时乘 GROUP）。
//!
//! 本模块只做装载/映射/块应用；不改变默认路径（未配置 [mimo12k] 时 host 行为完全不变）。

use std::path::Path;

use khaen_core::mimo12k::{
    Mimo12kLayout, Mimo12kRuntime, MIMO12K_GROUP, MIMO12K_LOWRATE_HZ, MIMO12K_MAX_BLOCK48,
    MIMO12K_MAX_PARTICIPANTS,
};

/// 12k 修正支路产物参数（params.json 的强类型视图）。
#[derive(Clone, Debug, PartialEq)]
pub struct Mimo12kParams {
    /// host 采样率（必须 48000；96k 走 ÷8 属后续）。
    pub fs_hz: u32,
    /// 低速率域采样率（必须 12000）。
    pub lowrate_hz: u32,
    /// 抽取/插值倍数（必须 4）。
    pub group: u32,
    /// 每路 12k G 修正 FIR 支撑。
    pub taps: usize,
    /// 设计目标时移 D12（12k 样本，M2 预延迟）。
    pub d12: usize,
    /// 抽取/插值 FIR 抽头数（奇数，两方向同一低通）。
    pub resamp_taps: usize,
    /// 直通支路应对齐的 Δsys（48k 样本）。
    pub dsys_samples: u32,
    /// participant 名字（顺序 = plan role 表顺序 = g 的 dense 下标）。
    pub participants: Vec<String>,
    /// g 系数原始 f32 文件名（相对产物目录）。
    pub g_file: String,
    /// 抽取低通 f32 文件名（相对产物目录）。
    pub resamp_file: String,
    /// g 设计产物 sha256（证据/报告用；host 只透传，不重算）。
    pub g_sha256: String,
}

/// 已装载的 12k 修正支路产物。
pub struct Mimo12kArtifact {
    /// 参数。
    pub params: Mimo12kParams,
    /// g[j][i][tap]（行主序）。
    pub g: Vec<f32>,
    /// 抽取低通（单位 DC 增益）。
    pub decim: Vec<f32>,
    /// 插值低通（= decim × GROUP，零插值补偿）。
    pub interp: Vec<f32>,
}

/// [NONRT] 装载产物目录（params.json + g + resamp）。
pub fn load_artifact(dir: &Path) -> Result<Mimo12kArtifact, String> {
    let params_path = dir.join("params.json");
    let text = std::fs::read_to_string(&params_path).map_err(|err| {
        format!(
            "读取 mimo12k 产物 params.json 失败 {}: {err}",
            params_path.display()
        )
    })?;
    let params = Mimo12kParams {
        fs_hz: json_u32(&text, "fs_hz").ok_or_else(|| "params.json 缺少 fs_hz".to_string())?,
        lowrate_hz: json_u32(&text, "lowrate_hz")
            .ok_or_else(|| "params.json 缺少 lowrate_hz".to_string())?,
        group: json_u32(&text, "group").ok_or_else(|| "params.json 缺少 group".to_string())?,
        taps: json_u32(&text, "taps").ok_or_else(|| "params.json 缺少 taps".to_string())? as usize,
        d12: json_u32(&text, "d12").ok_or_else(|| "params.json 缺少 d12".to_string())? as usize,
        resamp_taps: json_u32(&text, "resamp_taps")
            .ok_or_else(|| "params.json 缺少 resamp_taps".to_string())? as usize,
        dsys_samples: json_u32(&text, "dsys_samples")
            .ok_or_else(|| "params.json 缺少 dsys_samples".to_string())?,
        participants: json_string_array(&text, "participants")
            .ok_or_else(|| "params.json 缺少 participants".to_string())?,
        g_file: json_string(&text, "g_file").ok_or_else(|| "params.json 缺少 g_file".to_string())?,
        resamp_file: json_string(&text, "resamp_file")
            .ok_or_else(|| "params.json 缺少 resamp_file".to_string())?,
        g_sha256: json_string(&text, "g_sha256").unwrap_or_default(),
    };
    let g = read_f32_file(&dir.join(&params.g_file), "g")?;
    let decim = read_f32_file(&dir.join(&params.resamp_file), "resamp")?;
    let interp = decim.iter().map(|v| *v * MIMO12K_GROUP as f32).collect();
    Ok(Mimo12kArtifact {
        params,
        g,
        decim,
        interp,
    })
}

fn read_f32_file(path: &Path, what: &str) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path)
        .map_err(|err| format!("读取 mimo12k {what} 失败 {}: {err}", path.display()))?;
    if bytes.len() % 4 != 0 {
        return Err(format!(
            "mimo12k {what} 文件字节数 {} 不是 4 的倍数（{}）",
            bytes.len(),
            path.display()
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 极简 JSON 字段读取（只覆盖 params.json 的标量/字符串/字符串数组；不引入依赖）
// ---------------------------------------------------------------------------

fn json_slice_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let colon = text[start..].find(':')? + start;
    let rest = text[colon + 1..].trim_start();
    Some(rest)
}

fn json_u32(text: &str, key: &str) -> Option<u32> {
    let rest = json_slice_value(text, key)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn json_string(text: &str, key: &str) -> Option<String> {
    let rest = json_slice_value(text, key)?;
    if !rest.starts_with('"') {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

fn json_string_array(text: &str, key: &str) -> Option<Vec<String>> {
    let rest = json_slice_value(text, key)?;
    if !rest.starts_with('[') {
        return None;
    }
    let end = rest.find(']')?;
    let body = &rest[1..end];
    let mut out = Vec::new();
    for part in body.split(',') {
        let part = part.trim();
        if part.len() >= 2 && part.starts_with('"') && part.ends_with('"') {
            out.push(part[1..part.len() - 1].to_string());
        } else if !part.is_empty() {
            return None;
        }
    }
    Some(out)
}

/// 12k 修正支路在 host 内的接线对象（plan 直通支路之外的修正量）。
pub struct Mimo12kBranch {
    params: Mimo12kParams,
    layout: Mimo12kLayout,
    runtime: Mimo12kRuntime,
    /// (participant 序号, 输入通道, 输出容器)。
    mapping: Vec<(usize, usize, usize)>,
    feeds: Vec<f32>,
    corr: Vec<f32>,
    block: usize,
}

impl Mimo12kBranch {
    /// [NONRT] 由产物 + participant→(输入通道, 输出容器) 映射构建。
    ///
    /// 调用方（rt-host main.rs）必须先校验 plan 图：直通支路 G=identity、
    /// baseline D == params.dsys_samples、P=identity；本函数只做产物自洽与容量校验。
    pub fn prepare(
        artifact: Mimo12kArtifact,
        mapping: Vec<(usize, usize, usize)>,
        block: usize,
    ) -> Result<Self, String> {
        let p = artifact.params;
        let m = p.participants.len();
        if m == 0 || m > MIMO12K_MAX_PARTICIPANTS {
            return Err(format!(
                "mimo12k participants={m} 超出 1..={MIMO12K_MAX_PARTICIPANTS}"
            ));
        }
        if p.fs_hz != 48_000 {
            return Err(format!(
                "mimo12k params.fs_hz={} != 48000（96k ÷8 属后续，fail closed）",
                p.fs_hz
            ));
        }
        if p.lowrate_hz != MIMO12K_LOWRATE_HZ || p.group != MIMO12K_GROUP as u32 {
            return Err(format!(
                "mimo12k params lowrate={} group={} 与 core 常量 {MIMO12K_LOWRATE_HZ}/{MIMO12K_GROUP} 不一致",
                p.lowrate_hz, p.group
            ));
        }
        if block < MIMO12K_GROUP || block > MIMO12K_MAX_BLOCK48 || block % MIMO12K_GROUP != 0 {
            return Err(format!(
                "mimo12k host block_size={block} 非法（须为 4 的倍数且 <= {MIMO12K_MAX_BLOCK48}）"
            ));
        }
        if artifact.g.len() != m * m * p.taps {
            return Err(format!(
                "mimo12k g 样本数 {} != M*M*taps={}",
                artifact.g.len(),
                m * m * p.taps
            ));
        }
        if artifact.decim.len() != p.resamp_taps {
            return Err(format!(
                "mimo12k 抽取低通抽头数 {} != params.resamp_taps={}",
                artifact.decim.len(),
                p.resamp_taps
            ));
        }
        let expect_dsys =
            2 * ((p.resamp_taps - 1) / 2) as u32 + MIMO12K_GROUP as u32 * p.d12 as u32;
        if p.dsys_samples != expect_dsys {
            return Err(format!(
                "mimo12k params.dsys_samples={} != 2*({}-1)/2 + {}*d12={}（产物自洽校验失败）",
                p.dsys_samples, p.resamp_taps, MIMO12K_GROUP, expect_dsys
            ));
        }
        if mapping.len() != m {
            return Err(format!(
                "mimo12k participant 映射数 {} != participants={m}（plan role 顺序必须等于产物 participants 顺序）",
                mapping.len()
            ));
        }
        let layout = Mimo12kLayout::build(m, p.taps, block, p.resamp_taps, p.d12)
            .map_err(|status| format!("Mimo12kLayout::build 失败（status {}）", status.code()))?;
        let runtime = Mimo12kRuntime::build(layout, &artifact.g, &artifact.decim, &artifact.interp)
            .map_err(|status| format!("Mimo12kRuntime::build 失败（status {}）", status.code()))?;
        let feeds = vec![0.0f32; m * block];
        let corr = vec![0.0f32; m * block];
        Ok(Self {
            params: p,
            layout,
            runtime,
            mapping,
            feeds,
            corr,
            block,
        })
    }

    /// 产物参数。
    pub fn params(&self) -> &Mimo12kParams {
        &self.params
    }

    /// 布局（证据/报告用）。
    pub fn layout(&self) -> &Mimo12kLayout {
        &self.layout
    }

    /// [RT] 应用一个 block：从图输入取 participant 馈送，跑修正支路，加到输出容器。
    ///
    /// 调用方保证 !mute 块才调用；input/output 为 host plan-arena 的 planar 缓冲
    /// （in_stride/out_stride >= block）。不分配/不加锁。
    pub fn apply_block(
        &mut self,
        input: &[f32],
        in_stride: usize,
        output: &mut [f32],
        out_stride: usize,
    ) -> Result<(), String> {
        if in_stride < self.block || out_stride < self.block {
            return Err(format!(
                "mimo12k apply_block 步长不足：in_stride={in_stride} out_stride={out_stride} block={}",
                self.block
            ));
        }
        for (ch, &(_, in_ch, _)) in self.mapping.iter().enumerate() {
            if (in_ch + 1) * in_stride > input.len() {
                return Err(format!(
                    "mimo12k apply_block 输入越界：channel {in_ch} stride {in_stride} len {}",
                    input.len()
                ));
            }
            self.feeds[ch * self.block..(ch + 1) * self.block]
                .copy_from_slice(&input[in_ch * in_stride..in_ch * in_stride + self.block]);
        }
        let status = self.runtime.process_block_f32(
            &self.feeds,
            self.block,
            &mut self.corr,
            self.block,
            self.block,
        );
        if !status.is_ok() {
            return Err(format!(
                "mimo12k core process_block 返回 status {}",
                status.code()
            ));
        }
        for (ch, &(_, _, out_ch)) in self.mapping.iter().enumerate() {
            if (out_ch + 1) * out_stride > output.len() {
                return Err(format!(
                    "mimo12k apply_block 输出越界：container {out_ch} stride {out_stride} len {}",
                    output.len()
                ));
            }
            for n in 0..self.block {
                output[out_ch * out_stride + n] += self.corr[ch * self.block + n];
            }
        }
        Ok(())
    }

    /// 报告 JSON 片段（不含外层 key；调用方拼进 host 报告）。
    pub fn report_json(&self, blocks: u64) -> String {
        let participants = self
            .params
            .participants
            .iter()
            .map(|p| format!("\"{}\"", p))
            .collect::<Vec<_>>()
            .join(",");
        let mapping = self
            .mapping
            .iter()
            .map(|(idx, in_ch, out_ch)| {
                format!("{{\"participant\":{idx},\"in\":{in_ch},\"out\":{out_ch}}}")
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"enabled\":true,\"kernel\":\"core::mimo12k + T08 UPOLS\",\"participants\":[{participants}],\"taps\":{},\"d12\":{},\"resamp_taps\":{},\"lowrate_hz\":{},\"dsys_samples\":{},\"delay_ms\":{:.3},\"g_sha256\":\"{}\",\"block_size\":{},\"low_block\":{},\"fft_size\":{},\"partitions\":{},\"routes\":{},\"plan_samples\":{},\"state_samples\":{},\"blocks\":{},\"plan_mapping\":[{mapping}]}}",
            self.params.taps,
            self.params.d12,
            self.params.resamp_taps,
            self.params.lowrate_hz,
            self.params.dsys_samples,
            self.layout.delay_ms(),
            json_escape(&self.params.g_sha256),
            self.block,
            self.layout.low_block,
            self.layout.fft_size,
            self.layout.partitions,
            self.layout.n_routes,
            self.layout.conv_plan_samples,
            self.layout.conv_state_samples,
            blocks
        )
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}
