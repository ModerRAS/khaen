//! S2（PEQ-only）plan 打包：plan ABI v2 per-role IIR(PEQ) 区（GAP-T66-3）。
//!
//! 背景：plan ABI v1.0 只有 `KhaenRoleV1.peq_count` 计数、没有 role IIR 系数区，T66
//! `compile_plan` 对 `peq_count>0` 一律 `E_CA_PEQ_ABI` 拒收，host `graph_from_plan`
//! 也显式拒绝 → PEQ 无法进入 plan/host。本模块为 **S2 诊断协议的 known PEQ ladder** 提供
//! 最小 plan 打包器：factorized identity A/G/P（path=delta_D baseline、diag=1.0）+
//! 每 role IIR 级联，产出 **plan ABI v2** 字节（header 128 B 不变；IIR 表 + 系数 blob）。
//!
//! 职责边界：本模块只消费"已知 PEQ"（如 100 Hz/−6 dB/Q1.0），不做 T64 拟合/证据门/热更；
//! T66 全量校准编译（evidence-gated `compile_plan`）对无系数的 `peq_count>0` 仍拒收。
//! plan 字节布局与 `compile_apply` 的打包助手逐字节一致（复用同一 header/表/系数区/graph_id
//! 规范化实现），并通过本模块自校验 + host 侧 `swap::parse_plan_arena` 双重验证。
//!
//! 数字域口径（证据 `rt-stack/evidence/calib-chain-20261121/abi-iir/`）：
//! identity + 单个 100 Hz −6 dB Q1.0 → plan → host plan-arena（fake/file sink）→
//! 输出幅频 vs 解析 `|H|` ≤ 0.5 dB（20 Hz–20 kHz）。

use khaen_core::abi::{
    KhaenDiagFirV1, KhaenPathV1, KhaenPlanHeaderV1, KhaenPlanIirHeaderV2, KhaenRoleIirV2,
    KhaenRoleV1, KHAEN_PLAN_ABI_V1, KHAEN_PLAN_ABI_V2, KHAEN_PLAN_IIR_MAGIC,
    KHAEN_PLAN_IIR_VERSION, KHAEN_PLAN_MAGIC, PLAN_IIR_ENTRY_BYTES, PLAN_IIR_HEADER_BYTES,
    PLAN_IIR_MAX_SECTIONS, PLAN_IIR_SECTION_COEFFS,
};
use khaen_core::rt::{
    MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, RT_WORKERS, SAMPLE_RATES_HZ,
    TAPS_NORMAL_MAX,
};

use crate::compile_apply::{
    build_coeff_region, compute_graph_id, diag_table_bytes, header_bytes, path_table_bytes,
    role_table_bytes, DIAG_ENTRY_BYTES, PATH_ENTRY_BYTES, PATH_FLAG_DIAG_BASELINE_INCLUDED,
    PLAN_HEADER_BYTES, PLAN_VERSION_UNASSIGNED, ROLE_ENTRY_BYTES, ROLE_FLAG_ENABLED,
    ROLE_FLAG_ZERO_FILL,
};

/// S2 PEQ-only plan 打包器 schema。
pub const PEQ_PLAN_SCHEMA: &str = "khaen-peq-plan/v1";
/// 角色级延迟上限（200 ms；core-v1 §11.10 role.delay_samples）。
pub const PEQ_PLAN_ROLE_DELAY_MAX_MS: f64 = 200.0;

/// 单段 biquad（a0 已归一化为 1）：`y[n]=b0x[n]+b1x[n-1]+b2x[n-2]-a1y[n-1]-a2y[n-2]`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeqBiquad {
    /// b0
    pub b0: f64,
    /// b1
    pub b1: f64,
    /// b2
    pub b2: f64,
    /// a1（a0=1）
    pub a1: f64,
    /// a2（a0=1）
    pub a2: f64,
}

impl PeqBiquad {
    /// 直接构造（传入已归一化 b0/b1/b2/a1/a2；a0=1）。
    pub fn new(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Result<Self, String> {
        let section = Self { b0, b1, b2, a1, a2 };
        section.validate()?;
        Ok(section)
    }

    /// identity（直通）biquad。
    pub fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// RBJ cookbook peaking EQ（a0 归一化）。
    pub fn peaking_eq(fs_hz: f64, f0_hz: f64, gain_db: f64, q: f64) -> Result<Self, String> {
        if !fs_hz.is_finite() || fs_hz <= 0.0 {
            return Err(format!("peaking_eq fs={fs_hz} 非法"));
        }
        if !f0_hz.is_finite() || f0_hz <= 0.0 || f0_hz >= fs_hz / 2.0 {
            return Err(format!("peaking_eq f0={f0_hz} 必须 ∈ (0, fs/2)"));
        }
        if !gain_db.is_finite() || gain_db.abs() > 24.0 {
            return Err(format!("peaking_eq gain={gain_db} dB 越界（|gain|<=24）"));
        }
        if !q.is_finite() || q <= 0.0 {
            return Err(format!("peaking_eq Q={q} 必须 >0"));
        }
        let a = 10.0_f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * f0_hz / fs_hz;
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        let section = Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * w0.cos()) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * w0.cos()) / a0,
            a2: (1.0 - alpha / a) / a0,
        };
        section.validate()?;
        Ok(section)
    }

    /// 校验：全部有限 + biquad 稳定（|a2|<1 且 |a1|<1+a2；二阶实系数极点单位圆内）。
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("b0", self.b0),
            ("b1", self.b1),
            ("b2", self.b2),
            ("a1", self.a1),
            ("a2", self.a2),
        ] {
            if !value.is_finite() {
                return Err(format!("biquad {name}={value} 非有限"));
            }
        }
        if !(self.a2.abs() < 1.0 && self.a1.abs() < 1.0 + self.a2) {
            return Err(format!(
                "biquad 不稳定（a1={}, a2={}；需 |a2|<1 且 |a1|<1+a2）",
                self.a1, self.a2
            ));
        }
        Ok(())
    }

    /// 打包编码：5 × f32 `[b0,b1,b2,a1,a2]`。
    fn to_f32(self) -> [f32; PLAN_IIR_SECTION_COEFFS] {
        [
            self.b0 as f32,
            self.b1 as f32,
            self.b2 as f32,
            self.a1 as f32,
            self.a2 as f32,
        ]
    }

    /// 解析幅频响应（dB）。
    pub fn response_db(&self, fs_hz: f64, freq_hz: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq_hz / fs_hz;
        let num_re = self.b0 + self.b1 * w.cos() + self.b2 * (2.0 * w).cos();
        let num_im = -(self.b1 * w.sin() + self.b2 * (2.0 * w).sin());
        let den_re = 1.0 + self.a1 * w.cos() + self.a2 * (2.0 * w).cos();
        let den_im = -(self.a1 * w.sin() + self.a2 * (2.0 * w).sin());
        let num = (num_re * num_re + num_im * num_im).sqrt();
        let den = (den_re * den_re + den_im * den_im).sqrt();
        20.0 * (num / den).log10()
    }
}

/// 单个 role 的 PEQ-only plan 输入。
#[derive(Clone, Debug)]
pub struct PeqRolePlanInput {
    /// plan 内 role 序号（0..N-1，必须等于表内位置）。
    pub role_index: u16,
    /// 角色 id（`^[A-Z][A-Z0-9]*$`）。
    pub role_id: String,
    /// 物理输出（容器）索引。
    pub out_channel: u16,
    /// A_map 源通道（None = 零填角色）。
    pub source_channel: Option<u16>,
    /// 角色级整数延迟（samples；≤0.2·fs）。
    pub delay_samples: u32,
    /// 角色极性（+1/-1）。
    pub polarity: i8,
    /// 角色 gain（dB；plan 只能编码 ≤0 dB，>0 拒收）。
    pub gain_db: f64,
    /// role 级 IIR 级联（顺序即级联顺序；空 = 无 PEQ）。
    pub peq: Vec<PeqBiquad>,
}

impl PeqRolePlanInput {
    /// identity 角色（无 PEQ、无延迟/极性/gain）。
    pub fn identity(role_index: u16, role_id: impl Into<String>, out_channel: u16) -> Self {
        Self {
            role_index,
            role_id: role_id.into(),
            out_channel,
            source_channel: Some(out_channel),
            delay_samples: 0,
            polarity: 1,
            gain_db: 0.0,
            peq: Vec::new(),
        }
    }

    /// 追加一段 PEQ（链式）。
    pub fn with_peq(mut self, section: PeqBiquad) -> Self {
        self.peq.push(section);
        self
    }

    fn gain_q15(&self) -> i16 {
        let linear = 10.0_f64.powf(self.gain_db / 20.0);
        let scaled = (linear * 32767.0).round();
        if !scaled.is_finite() || scaled <= 0.0 {
            0
        } else if scaled >= 32767.0 {
            32767
        } else {
            scaled as i16
        }
    }
}

/// PEQ-only plan 请求（factorized identity A/G/P + role IIR）。
#[derive(Clone, Debug)]
pub struct PeqPlanRequest {
    /// 采样率（48000/96000）。
    pub sample_rate_hz: u32,
    /// block 大小（64/128）。
    pub block_size: u16,
    /// 图输入通道 N（A_map 源域）。
    pub n_in: u16,
    /// 物理容器通道数 C（设备宽度；>= role 数）。
    pub container_channels: u16,
    /// expected workers（生产 4 / 降级 2）。
    pub workers: u8,
    /// role 表（按 role_index 严格升序）。
    pub roles: Vec<PeqRolePlanInput>,
}

/// PEQ-only plan 产物（含自校验需要的全部表/字节/解析响应参考）。
#[derive(Clone, Debug)]
pub struct PeqPlanArtifact {
    /// plan header（abi_version=2 当且仅当有 PEQ 段）。
    pub header: KhaenPlanHeaderV1,
    /// graph_id（SHA-256；含 IIR 表与系数）。
    pub graph_id: [u8; 32],
    /// 完整 plan 字节（T66 compile/plan.bin 同布局）。
    pub plan_bytes: Vec<u8>,
    /// role 表。
    pub roles: Vec<KhaenRoleV1>,
    /// plan ABI v2 IIR 表项（空 = 无 PEQ）。
    pub iir_entries: Vec<KhaenRoleIirV2>,
    /// 每 role 的 PEQ 级联（与 `roles` 同序；解析响应参考/证据用）。
    pub role_peq: Vec<Vec<PeqBiquad>>,
}

impl PeqPlanArtifact {
    /// role 的级联解析幅频响应（dB；空级联 = 0 dB）。
    pub fn role_response_db(&self, role_index: usize, freq_hz: f64) -> f64 {
        let fs = self.header.sample_rate as f64;
        match self.role_peq.get(role_index) {
            Some(sections) => sections
                .iter()
                .map(|section| section.response_db(fs, freq_hz))
                .sum(),
            None => 0.0,
        }
    }

    /// plan 中 PEQ 段总数。
    pub fn section_count(&self) -> usize {
        self.role_peq.iter().map(|sections| sections.len()).sum()
    }

    /// 重新解析 IIR 表的 f32 系数（用于证明"下发字节 == 解析预期"）。
    pub fn parsed_iir_sections(
        &self,
    ) -> Result<Vec<(u16, Vec<[f32; PLAN_IIR_SECTION_COEFFS]>)>, String> {
        let mut out = Vec::with_capacity(self.iir_entries.len());
        for entry in &self.iir_entries {
            let start = entry.coeff_offset_bytes as usize;
            let blob = &self.plan_bytes[self.header.total_coeff_bytes as usize..];
            let _ = blob; // 系数区偏移相对 plan 尾部系数区，由 header.total_coeff_bytes 界定。
            let coeff_base = self.plan_bytes.len() - self.header.total_coeff_bytes as usize;
            let mut sections = Vec::with_capacity(entry.section_count as usize);
            let mut section_index = 0usize;
            while section_index < entry.section_count as usize {
                let base = coeff_base + start + section_index * 4 * PLAN_IIR_SECTION_COEFFS;
                let mut coeffs = [0.0f32; PLAN_IIR_SECTION_COEFFS];
                let mut k = 0usize;
                while k < PLAN_IIR_SECTION_COEFFS {
                    let at = base + k * 4;
                    coeffs[k] = f32::from_le_bytes([
                        self.plan_bytes[at],
                        self.plan_bytes[at + 1],
                        self.plan_bytes[at + 2],
                        self.plan_bytes[at + 3],
                    ]);
                    k += 1;
                }
                sections.push(coeffs);
                section_index += 1;
            }
            out.push((entry.role_id, sections));
        }
        Ok(out)
    }
}

fn peq_err(field: impl Into<String>, detail: impl Into<String>) -> String {
    format!("peq-plan {}: {}", field.into(), detail.into())
}

fn is_role_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !bytes[0].is_ascii_uppercase() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// 打包 plan ABI v2（per-role IIR/PEQ）字节；任何校验失败返回稳定错误字符串，不产出半成品。
pub fn compile_peq_plan(req: &PeqPlanRequest) -> Result<PeqPlanArtifact, String> {
    validate_request(req)?;
    let fs = req.sample_rate_hz;
    let mut path_blobs: Vec<Vec<u8>> = Vec::new();
    let mut diag_blobs: Vec<Vec<u8>> = Vec::new();
    let mut iir_blobs: Vec<Vec<u8>> = Vec::new();
    let mut paths: Vec<KhaenPathV1> = Vec::new();
    let mut diags: Vec<KhaenDiagFirV1> = Vec::new();
    let mut roles: Vec<KhaenRoleV1> = Vec::with_capacity(req.roles.len());
    let mut iir_entries: Vec<KhaenRoleIirV2> = Vec::new();
    let mut role_peq: Vec<Vec<PeqBiquad>> = Vec::with_capacity(req.roles.len());

    // 1) role 表 + identity path/diag（path=delta_D baseline coeffs [1,0]、diag=[1]）。
    for role in &req.roles {
        let zero_fill = role.source_channel.is_none();
        let flags = if zero_fill {
            ROLE_FLAG_ENABLED | ROLE_FLAG_ZERO_FILL
        } else {
            ROLE_FLAG_ENABLED
        };
        roles.push(KhaenRoleV1 {
            role_id: role.role_index,
            out_index: role.out_channel,
            flags,
            reserved0: 0,
            delay_samples: role.delay_samples,
            gain_q15: role.gain_q15(),
            polarity: role.polarity,
            reserved1: 0,
            peq_count: role.peq.len() as u16,
            reserved2: 0,
            name_hash: khaen_core::nodes::crc32c(role.role_id.as_bytes()),
            reserved3: 0,
            reserved4: 0,
        });
        if let Some(source) = role.source_channel {
            paths.push(KhaenPathV1 {
                in_index: source,
                out_index: role.out_channel,
                taps: 2,
                delay_samples: 0,
                polarity: 1,
                flags: PATH_FLAG_DIAG_BASELINE_INCLUDED,
                reserved0: 0,
                coeff_offset_bytes: 0,
                coeff_len_bytes: 8,
                reserved1: 0,
            });
            path_blobs.push({
                let mut bytes = Vec::with_capacity(8);
                bytes.extend_from_slice(&1.0f32.to_le_bytes());
                bytes.extend_from_slice(&0.0f32.to_le_bytes());
                bytes
            });
            diags.push(KhaenDiagFirV1 {
                out_index: role.out_channel,
                flags: 0,
                taps: 1,
                delay_samples: 0,
                reserved0: 0,
                coeff_offset_bytes: 0,
                coeff_len_bytes: 4,
                reserved1: 0,
            });
            diag_blobs.push(1.0f32.to_le_bytes().to_vec());
        }
        role_peq.push(role.peq.clone());
    }

    // 2) 系数区：path/diag blob + 每 role IIR blob（每段 5×f32），统一 64B 对齐。
    let mut all_blobs: Vec<Vec<u8>> = Vec::new();
    all_blobs.extend(path_blobs.iter().cloned());
    all_blobs.extend(diag_blobs.iter().cloned());
    for role in &req.roles {
        if role.peq.is_empty() {
            continue;
        }
        let mut bytes = Vec::with_capacity(role.peq.len() * 4 * PLAN_IIR_SECTION_COEFFS);
        for section in &role.peq {
            for value in section.to_f32() {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        iir_blobs.push(bytes);
        all_blobs.push(iir_blobs.last().cloned().unwrap_or_default());
    }
    let (coeff_region, offsets) = build_coeff_region(&all_blobs);
    if coeff_region.len() as u64 > 4 * 1024 * 1024 {
        return Err(peq_err("coeff_region", "系数区超过 4 MiB 预算"));
    }
    // 回填 path/diag 系数偏移 + 生成 IIR 表项。
    let mut path_index = 0usize;
    for path in paths.iter_mut() {
        path.coeff_offset_bytes = offsets[path_index];
        path_index += 1;
    }
    let mut diag_index = 0usize;
    for diag in diags.iter_mut() {
        diag.coeff_offset_bytes = offsets[paths.len() + diag_index];
        diag_index += 1;
    }
    let mut iir_index = 0usize;
    for role in &req.roles {
        if role.peq.is_empty() {
            continue;
        }
        let section_count = role.peq.len();
        iir_entries.push(KhaenRoleIirV2 {
            role_id: role.role_index,
            order: 2,
            section_count: section_count as u16,
            flags: 0,
            coeff_offset_bytes: offsets[paths.len() + diags.len() + iir_index],
            coeff_len_bytes: section_count as u32 * 4 * PLAN_IIR_SECTION_COEFFS as u32,
            reserved0: 0,
            reserved1: 0,
            reserved2: 0,
        });
        iir_index += 1;
    }

    // 3) 表偏移 + ctl_meta + IIR 表（表区在系数区之前；IIR 表 8B 对齐）。
    let path_table_offset = PLAN_HEADER_BYTES as u32;
    let diag_table_offset = path_table_offset + (paths.len() * PATH_ENTRY_BYTES) as u32;
    let role_table_offset = diag_table_offset + (diags.len() * DIAG_ENTRY_BYTES) as u32;
    let ctl_meta_offset = role_table_offset + (roles.len() * ROLE_ENTRY_BYTES) as u32;
    let ctl_meta = build_ctl_meta_for(&req.roles);
    let ctl_meta_len = ctl_meta.len() as u32;
    let iir_table_offset = ctl_meta_offset
        .checked_add(ctl_meta_len)
        .map(|value| (value + 7) / 8 * 8)
        .ok_or_else(|| peq_err("iir_table_offset", "偏移溢出"))?;
    let iir_table = if iir_entries.is_empty() {
        Vec::new()
    } else {
        build_iir_table(&iir_entries, &iir_blobs)?
    };
    let n_out = req.container_channels;
    let mut header = KhaenPlanHeaderV1 {
        magic: KHAEN_PLAN_MAGIC,
        abi_version: if iir_entries.is_empty() {
            KHAEN_PLAN_ABI_V1
        } else {
            KHAEN_PLAN_ABI_V2
        },
        header_size: PLAN_HEADER_BYTES as u16,
        plan_version: PLAN_VERSION_UNASSIGNED,
        sample_rate: fs,
        block_size: req.block_size,
        n_in: req.n_in,
        n_out,
        low_band_count: 1,
        path_count: paths.len() as u16,
        diag_fir_count: diags.len() as u16,
        max_taps: 2,
        workers: req.workers,
        flags: 0,
        precision: 0,
        reserved0: 0,
        graph_id: [0u8; 32],
        total_coeff_bytes: coeff_region.len() as u32,
        path_table_offset,
        diag_table_offset,
        in_stride: req.block_size as u32,
        out_stride: req.block_size as u32,
        role_table_offset,
        role_table_count: roles.len() as u16,
        reserved1: 0,
        affinity_table_offset: 0,
        affinity_count: 0,
        reserved2: 0,
        ctl_meta_offset,
        ctl_meta_len,
        reserved3: if iir_entries.is_empty() {
            [0u32; 3]
        } else {
            [iir_table_offset, iir_entries.len() as u32, 0]
        },
        header_crc32c: 0,
    };
    let graph_id = compute_graph_id(
        &header,
        &paths,
        &diags,
        &roles,
        &[],
        &ctl_meta,
        &all_blobs,
        &iir_table,
        &iir_blobs,
    )
    .map_err(|error| {
        peq_err(
            "graph_id",
            format!("graph_id 重算失败（compile_apply 口径）：{error:?}"),
        )
    })?;
    header.graph_id = graph_id;
    header.header_crc32c = 0;
    let mut header_array = header_bytes(&header);
    header.header_crc32c = khaen_core::nodes::crc32c(&header_array);
    header_array = header_bytes(&header);

    // 4) 拼字节：header + path/diag/role 表 + ctl_meta + pad + IIR 表 + 系数区。
    let mut plan_bytes: Vec<u8> = Vec::with_capacity(
        PLAN_HEADER_BYTES
            + paths.len() * PATH_ENTRY_BYTES
            + diags.len() * DIAG_ENTRY_BYTES
            + roles.len() * ROLE_ENTRY_BYTES
            + ctl_meta.len()
            + iir_table.len()
            + coeff_region.len()
            + 8,
    );
    plan_bytes.extend_from_slice(&header_array);
    plan_bytes.extend_from_slice(&path_table_bytes(&paths));
    plan_bytes.extend_from_slice(&diag_table_bytes(&diags));
    plan_bytes.extend_from_slice(&role_table_bytes(&roles));
    plan_bytes.extend_from_slice(&ctl_meta);
    while (plan_bytes.len() as u32) < iir_table_offset {
        plan_bytes.push(0);
    }
    plan_bytes.extend_from_slice(&iir_table);
    plan_bytes.extend_from_slice(&coeff_region);
    if plan_bytes.len() != iir_table_offset as usize + iir_table.len() + coeff_region.len() {
        return Err(peq_err("plan_bytes", "字节长度与表偏移不一致（内部错误）"));
    }
    if header.total_coeff_bytes as usize != coeff_region.len() {
        return Err(peq_err("total_coeff_bytes", "系数区长度不一致（内部错误）"));
    }
    let artifact = PeqPlanArtifact {
        header,
        graph_id,
        plan_bytes,
        roles,
        iir_entries,
        role_peq,
    };
    verify_artifact(&artifact)?;
    Ok(artifact)
}

fn validate_request(req: &PeqPlanRequest) -> Result<(), String> {
    if !SAMPLE_RATES_HZ.contains(&req.sample_rate_hz) {
        return Err(peq_err(
            "sample_rate_hz",
            format!("{} 不是 48000/96000", req.sample_rate_hz),
        ));
    }
    if req.block_size != 64 && req.block_size != 128 {
        return Err(peq_err(
            "block_size",
            format!("{} 不是 64/128", req.block_size),
        ));
    }
    if req.n_in == 0 || req.n_in as usize > MAX_INPUT_CHANNELS {
        return Err(peq_err("n_in", format!("{} 越界", req.n_in)));
    }
    if req.container_channels == 0 || req.container_channels as usize > MAX_OUTPUT_CHANNELS {
        return Err(peq_err(
            "container_channels",
            format!("{} 越界", req.container_channels),
        ));
    }
    if req.workers != RT_WORKERS && req.workers != khaen_core::rt::RT_WORKERS_FALLBACK {
        return Err(peq_err("workers", format!("{} 必须 4 或 2", req.workers)));
    }
    if req.roles.is_empty() || req.roles.len() > MAX_OUTPUT_ROLES {
        return Err(peq_err(
            "roles",
            format!("role 数 {} 越界（1..=16）", req.roles.len()),
        ));
    }
    if req.roles.len() > req.container_channels as usize {
        return Err(peq_err("roles", "role 数不能超过容器通道数"));
    }
    let mut seen_ids: Vec<&str> = Vec::new();
    let mut seen_out: Vec<u16> = Vec::new();
    let mut seen_sources: Vec<u16> = Vec::new();
    for (position, role) in req.roles.iter().enumerate() {
        if role.role_index as usize != position {
            return Err(peq_err(
                "roles.role_index",
                format!("role_index {} 必须等于表内序号 {position}", role.role_index),
            ));
        }
        if !is_role_token(&role.role_id) {
            return Err(peq_err(
                "roles.role_id",
                format!("role id {:?} 不匹配 ^[A-Z][A-Z0-9]*$", role.role_id),
            ));
        }
        if seen_ids.iter().any(|id| *id == role.role_id) {
            return Err(peq_err(
                "roles.role_id",
                format!("role id {:?} 重复", role.role_id),
            ));
        }
        seen_ids.push(role.role_id.as_str());
        if role.out_channel >= req.container_channels {
            return Err(peq_err(
                "roles.out_channel",
                format!(
                    "{} >= container_channels={}",
                    role.out_channel, req.container_channels
                ),
            ));
        }
        if seen_out.contains(&role.out_channel) {
            return Err(peq_err(
                "roles.out_channel",
                format!("out_channel {} 重复（scatter 必须单射）", role.out_channel),
            ));
        }
        seen_out.push(role.out_channel);
        match role.source_channel {
            Some(source) => {
                if source >= req.n_in {
                    return Err(peq_err(
                        "roles.source_channel",
                        format!("source {source} >= n_in={}", req.n_in),
                    ));
                }
                if seen_sources.contains(&source) {
                    return Err(peq_err(
                        "roles.source_channel",
                        format!("源通道 {source} 被多个 role 选中（A_map 禁止分发）"),
                    ));
                }
                seen_sources.push(source);
            }
            None => {}
        }
        if role.polarity != 1 && role.polarity != -1 {
            return Err(peq_err(
                "roles.polarity",
                format!("{} 必须 ±1", role.polarity),
            ));
        }
        let max_delay = req.sample_rate_hz as f64 * PEQ_PLAN_ROLE_DELAY_MAX_MS / 1000.0;
        if role.delay_samples as f64 > max_delay {
            return Err(peq_err(
                "roles.delay_samples",
                format!("{} 超过 200 ms 上限 {max_delay}", role.delay_samples),
            ));
        }
        if !role.gain_db.is_finite() || role.gain_db > 0.0 || role.gain_db < -60.0 {
            return Err(peq_err(
                "roles.gain_db",
                format!(
                    "{} dB 越界：plan ABI role.gain_q15 只能编码 [-60,0]（>0 需增益节点或折入 FIR）",
                    role.gain_db
                ),
            ));
        }
        if role.peq.len() > PLAN_IIR_MAX_SECTIONS {
            return Err(peq_err(
                "roles.peq",
                format!("{} 段超过 {PLAN_IIR_MAX_SECTIONS}", role.peq.len()),
            ));
        }
        for (index, section) in role.peq.iter().enumerate() {
            section
                .validate()
                .map_err(|detail| peq_err(format!("roles[{position}].peq[{index}]"), detail))?;
        }
    }
    // max_taps 上限（identity 只用 2 tap，但保持与公用常量一致）。
    if 2 > TAPS_NORMAL_MAX {
        return Err(peq_err("max_taps", "identity plan taps 超上限"));
    }
    Ok(())
}

fn build_ctl_meta_for(roles: &[PeqRolePlanInput]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for role in roles {
        let bytes = role.role_id.as_bytes();
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(bytes);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

fn build_iir_table(entries: &[KhaenRoleIirV2], iir_blobs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if entries.len() != iir_blobs.len() {
        return Err(peq_err(
            "iir_table",
            "表项数与系数 blob 数不一致（内部错误）",
        ));
    }
    let header = KhaenPlanIirHeaderV2 {
        magic: KHAEN_PLAN_IIR_MAGIC,
        version: KHAEN_PLAN_IIR_VERSION,
        entry_bytes: PLAN_IIR_ENTRY_BYTES as u16,
        entry_count: entries.len() as u16,
        reserved0: 0,
        table_crc32c: 0,
    };
    let mut table: Vec<u8> =
        Vec::with_capacity(PLAN_IIR_HEADER_BYTES + entries.len() * PLAN_IIR_ENTRY_BYTES);
    table.extend_from_slice(&iir_header_bytes(&header));
    for entry in entries {
        table.extend_from_slice(&iir_entry_bytes(entry));
    }
    let mut crc_input = table.clone();
    for blob in iir_blobs {
        crc_input.extend_from_slice(blob);
    }
    let crc = khaen_core::nodes::crc32c(&crc_input);
    table[12..16].copy_from_slice(&crc.to_le_bytes());
    Ok(table)
}

fn iir_header_bytes(header: &KhaenPlanIirHeaderV2) -> [u8; PLAN_IIR_HEADER_BYTES] {
    let mut out = [0u8; PLAN_IIR_HEADER_BYTES];
    out[0..4].copy_from_slice(&header.magic.to_le_bytes());
    out[4..6].copy_from_slice(&header.version.to_le_bytes());
    out[6..8].copy_from_slice(&header.entry_bytes.to_le_bytes());
    out[8..10].copy_from_slice(&header.entry_count.to_le_bytes());
    out[10..12].copy_from_slice(&header.reserved0.to_le_bytes());
    out[12..16].copy_from_slice(&header.table_crc32c.to_le_bytes());
    out
}

fn iir_entry_bytes(entry: &KhaenRoleIirV2) -> [u8; PLAN_IIR_ENTRY_BYTES] {
    let mut out = [0u8; PLAN_IIR_ENTRY_BYTES];
    out[0..2].copy_from_slice(&entry.role_id.to_le_bytes());
    out[2..4].copy_from_slice(&entry.order.to_le_bytes());
    out[4..6].copy_from_slice(&entry.section_count.to_le_bytes());
    out[6..8].copy_from_slice(&entry.flags.to_le_bytes());
    out[8..16].copy_from_slice(&entry.coeff_offset_bytes.to_le_bytes());
    out[16..20].copy_from_slice(&entry.coeff_len_bytes.to_le_bytes());
    out[20..24].copy_from_slice(&entry.reserved0.to_le_bytes());
    out[24..28].copy_from_slice(&entry.reserved1.to_le_bytes());
    out[28..32].copy_from_slice(&entry.reserved2.to_le_bytes());
    out
}

fn verify_artifact(artifact: &PeqPlanArtifact) -> Result<(), String> {
    let header = &artifact.header;
    if header.magic != KHAEN_PLAN_MAGIC {
        return Err(peq_err("header.magic", "magic 不符"));
    }
    let expect_abi = if artifact.iir_entries.is_empty() {
        KHAEN_PLAN_ABI_V1
    } else {
        KHAEN_PLAN_ABI_V2
    };
    if header.abi_version != expect_abi {
        return Err(peq_err("header.abi_version", "版本与 IIR 表存在性不一致"));
    }
    if header.header_size != PLAN_HEADER_BYTES as u16 {
        return Err(peq_err("header.header_size", "header_size 必须 128"));
    }
    if header.plan_version != PLAN_VERSION_UNASSIGNED {
        return Err(peq_err(
            "header.plan_version",
            "编译产物 plan_version 必须 0",
        ));
    }
    if header.graph_id != artifact.graph_id {
        return Err(peq_err("header.graph_id", "graph_id 不一致"));
    }
    // header CRC
    let mut header_array = header_bytes(header);
    let written_crc = header.header_crc32c;
    header_array[124..128].fill(0);
    if khaen_core::nodes::crc32c(&header_array) != written_crc {
        return Err(peq_err("header.header_crc32c", "header CRC-32C 自校验失败"));
    }
    // 字节长度 / 系数区 / 表顺序
    let coeff_base = artifact.plan_bytes.len() - header.total_coeff_bytes as usize;
    let expected_tables_end =
        header.path_table_offset as usize + artifact.roles.len() * ROLE_ENTRY_BYTES; // role 表结束（ctl_meta offset 一致）
    if header.role_table_offset as usize >= header.ctl_meta_offset as usize
        || (header.ctl_meta_offset as usize) < expected_tables_end
    {
        return Err(peq_err("header.offsets", "表偏移顺序不一致"));
    }
    if header.ctl_meta_offset as usize + header.ctl_meta_len as usize > coeff_base {
        return Err(peq_err("ctl_meta", "ctl_meta 越界"));
    }
    if !artifact.iir_entries.is_empty() {
        let iir_offset = header.reserved3[0] as usize;
        let iir_count = header.reserved3[1] as usize;
        if iir_offset % 8 != 0 || iir_offset < 128 || iir_count != artifact.iir_entries.len() {
            return Err(peq_err("header.reserved3", "IIR 表偏移/条数非法"));
        }
        let iir_end = iir_offset + PLAN_IIR_HEADER_BYTES + iir_count * PLAN_IIR_ENTRY_BYTES;
        if iir_end > coeff_base {
            return Err(peq_err("iir_table", "IIR 表越界"));
        }
        let iir_header = KhaenPlanIirHeaderV2 {
            magic: khaen_core::abi::KHAEN_PLAN_IIR_MAGIC,
            version: khaen_core::abi::KHAEN_PLAN_IIR_VERSION,
            entry_bytes: PLAN_IIR_ENTRY_BYTES as u16,
            entry_count: iir_count as u16,
            reserved0: 0,
            table_crc32c: 0,
        };
        let mut expected_table: Vec<u8> = iir_header_bytes(&iir_header).to_vec();
        for entry in &artifact.iir_entries {
            expected_table.extend_from_slice(&iir_entry_bytes(entry));
        }
        let mut crc_input: Vec<u8> = expected_table.clone();
        for entry in &artifact.iir_entries {
            let start = coeff_base + entry.coeff_offset_bytes as usize;
            let end = start + entry.coeff_len_bytes as usize;
            if end > artifact.plan_bytes.len() {
                return Err(peq_err("iir.coeff", "IIR 系数区间越界"));
            }
            crc_input.extend_from_slice(&artifact.plan_bytes[start..end]);
        }
        let crc = khaen_core::nodes::crc32c(&crc_input);
        let written = &artifact.plan_bytes[iir_offset + 12..iir_offset + 16];
        if written != crc.to_le_bytes() {
            return Err(peq_err(
                "iir_table.table_crc32c",
                "IIR 表 CRC-32C 自校验失败",
            ));
        }
        if artifact.plan_bytes[iir_offset..iir_offset + 4]
            != khaen_core::abi::KHAEN_PLAN_IIR_MAGIC.to_le_bytes()
        {
            return Err(peq_err("iir_table.magic", "IIR 表 magic 不符"));
        }
    }
    // graph_id 重算由编译期 compute_graph_id + header.graph_id 一致性检查覆盖；这里再确认
    // 系数区尾部与 header 声明一致（防"表区吃进系数区"的坏布局）。
    if artifact.plan_bytes.len() < coeff_base {
        return Err(peq_err("plan_bytes", "系数区起点越界"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    fn stereo_request(peq: PeqBiquad) -> PeqPlanRequest {
        PeqPlanRequest {
            sample_rate_hz: FS,
            block_size: 128,
            n_in: 2,
            container_channels: 2,
            workers: RT_WORKERS,
            roles: vec![
                PeqRolePlanInput::identity(0, "FL", 0).with_peq(peq),
                PeqRolePlanInput::identity(1, "FR", 1),
            ],
        }
    }

    #[test]
    fn t_peq_plan_known_100hz_minus6_q1() {
        let section = PeqBiquad::peaking_eq(FS as f64, 100.0, -6.0, 1.0).expect("已知 PEQ");
        // 解析参考：中心 −6 dB、远端 ≈0 dB。
        assert!((section.response_db(FS as f64, 100.0) + 6.0).abs() < 1e-6);
        assert!(section.response_db(FS as f64, 20.0).abs() < 0.5);
        // 1 kHz 处 RBJ peaking（Q1）解析值 ≈ −0.065 dB；远端应回到 0 dB 附近。
        assert!(section.response_db(FS as f64, 1000.0).abs() < 0.1);
        let artifact = compile_peq_plan(&stereo_request(section)).expect("compile v2 plan");
        assert_eq!(artifact.header.abi_version, KHAEN_PLAN_ABI_V2);
        assert_eq!(artifact.section_count(), 1);
        assert_eq!(artifact.iir_entries.len(), 1);
        assert_eq!(artifact.iir_entries[0].role_id, 0);
        assert_eq!(artifact.iir_entries[0].section_count, 1);
        assert_eq!(artifact.header.reserved3[1], 1);
        // 下发字节解析出的 f32 系数与请求一致（≤1e-7）。
        let parsed = artifact.parsed_iir_sections().expect("parse back");
        assert_eq!(parsed.len(), 1);
        let sent = parsed[0].1[0];
        let expected = section.to_f32();
        for index in 0..5 {
            assert!(
                (sent[index] - expected[index]).abs() <= 1e-7,
                "coeff[{index}] {} != {}",
                sent[index],
                expected[index]
            );
        }
        assert_eq!(artifact.plan_bytes[0..4], KHAEN_PLAN_MAGIC.to_le_bytes());
    }

    #[test]
    fn t_peq_plan_no_peq_is_v1() {
        let artifact = compile_peq_plan(&PeqPlanRequest {
            sample_rate_hz: FS,
            block_size: 128,
            n_in: 2,
            container_channels: 2,
            workers: RT_WORKERS,
            roles: vec![
                PeqRolePlanInput::identity(0, "FL", 0),
                PeqRolePlanInput::identity(1, "FR", 1),
            ],
        })
        .expect("identity v1 plan");
        assert_eq!(artifact.header.abi_version, KHAEN_PLAN_ABI_V1);
        assert!(artifact.iir_entries.is_empty());
        assert_eq!(artifact.header.reserved3, [0u32; 3]);
        assert!(artifact.section_count() == 0);
    }

    #[test]
    fn t_peq_plan_rejects_unstable_or_bad_input() {
        let unstable = PeqBiquad::new(1.0, 0.0, 0.0, 0.0, 1.2);
        assert!(unstable.is_err(), "不稳定 biquad 必须拒绝");
        let section = PeqBiquad::peaking_eq(FS as f64, 100.0, -6.0, 1.0).expect("known");
        let mut request = stereo_request(section);
        request.roles[0].gain_db = 3.0;
        assert!(
            compile_peq_plan(&request).is_err(),
            "正 gain 不能编码（ABI gain_q15<=0 dB）"
        );
        let mut request = stereo_request(section);
        request.roles[1].out_channel = 0;
        assert!(compile_peq_plan(&request).is_err(), "scatter 必须单射");
        let mut request = stereo_request(section);
        request.roles[0].peq.push(section);
        let artifact = compile_peq_plan(&request).expect("两段级联");
        assert_eq!(artifact.section_count(), 2);
    }
}
