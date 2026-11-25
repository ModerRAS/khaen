//! 方案文件 ABI v3（见 docs/CONTRACTS.md §A）。
//!
//! 布局：Header(32B) | Sections | SectionTable | Trailer(36B)
//!
//! - 解析必须校验每段 CRC-32C 与整体 CRC，任一不符拒绝加载（fail-closed）。
//! - graph_id = sha256("khaen-plan/v3" || 除 trailer 外的全部字节)。
//! - 未知区段类型拒绝加载，不静默跳过。

use crate::crc32c::crc32c;
use crate::sha256::sha256;

/// 文件魔数。
pub const MAGIC: [u8; 8] = *b"KHAENPLN";
/// 当前 ABI 版本。
pub const ABI_VERSION: u16 = 3;
/// graph_id 的域分隔串。
pub const GRAPH_ID_DOMAIN: &[u8] = b"khaen-plan/v3";

const HEADER_BYTES: usize = 32;
/// 表项：kind(2) + offset(8) + bytes(8) + crc(4)。
const TABLE_ENTRY_BYTES: usize = 22;
const TRAILER_BYTES: usize = 4 + 32;

/// 方案文件错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// 魔数不符。
    BadMagic,
    /// ABI 版本不受支持。
    UnsupportedAbi(u16),
    /// 长度不足或偏移越界。
    Truncated,
    /// 头部声明的总长与实际不符。
    BadTotalBytes { declared: u32, actual: usize },
    /// 区段 CRC 不符。
    SectionCrc { kind: u16 },
    /// 整体 CRC 不符。
    CrcMismatch,
    /// graph_id 与内容不符。
    GraphIdMismatch,
    /// 未知区段类型。
    UnknownSection(u16),
    /// 区段内容非法。
    BadPayload(&'static str),
    /// 必需区段缺失。
    MissingSection(&'static str),
}

impl core::fmt::Display for PlanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PlanError::BadMagic => write!(f, "bad magic"),
            PlanError::UnsupportedAbi(v) => write!(f, "unsupported abi version {v}"),
            PlanError::Truncated => write!(f, "truncated plan"),
            PlanError::BadTotalBytes { declared, actual } => {
                write!(f, "total_bytes declared {declared} but actual {actual}")
            }
            PlanError::SectionCrc { kind } => write!(f, "section crc mismatch (kind {kind})"),
            PlanError::CrcMismatch => write!(f, "plan crc mismatch"),
            PlanError::GraphIdMismatch => write!(f, "graph_id mismatch"),
            PlanError::UnknownSection(kind) => write!(f, "unknown section kind {kind}"),
            PlanError::BadPayload(msg) => write!(f, "bad payload: {msg}"),
            PlanError::MissingSection(name) => write!(f, "missing section {name}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// 区段类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionKind {
    /// 内容布局与中置折叠。
    Content = 1,
    /// 每 lane 的分频滤波器组。
    Crossover = 2,
    /// 跨 lane 满矩阵。
    Matrix = 3,
    /// 每 lane 的保护（延迟/极性/增益/PEQ）。
    Protection = 4,
    /// lane 到 DAC 通道的路由。
    Routing = 5,
}

impl SectionKind {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            1 => Some(SectionKind::Content),
            2 => Some(SectionKind::Crossover),
            3 => Some(SectionKind::Matrix),
            4 => Some(SectionKind::Protection),
            5 => Some(SectionKind::Routing),
            _ => None,
        }
    }
}

/// 内容折叠项：把源通道按增益并入目标 lane。
#[derive(Debug, Clone, PartialEq)]
pub struct Fold {
    /// 源容器通道。
    pub src: u16,
    /// 目标 lane。
    pub dst: u16,
    /// 线性增益。
    pub gain: f32,
}

/// 内容布局区段。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContentSection {
    /// 输入容器通道数。
    pub in_channels: u16,
    /// 输出 lane 数。
    pub out_lanes: u16,
    /// 折叠表。
    pub folds: Vec<Fold>,
}

/// 单 lane 分频滤波器。
#[derive(Debug, Clone, PartialEq)]
pub struct CrossoverFilter {
    /// 目标 lane。
    pub lane: u16,
    /// 系数。
    pub coeffs: Vec<f32>,
}

/// 分频区段。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CrossoverSection {
    /// lane 数。
    pub lanes: u16,
    /// 滤波器组。
    pub filters: Vec<CrossoverFilter>,
}

/// 满矩阵区段。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatrixSection {
    /// 行数（输出 lane）。
    pub rows: u16,
    /// 列数（输入 lane）。
    pub cols: u16,
    /// 抽取因子。
    pub decimation: u8,
    /// 插值因子。
    pub interpolation: u8,
    /// 每格抽头数。
    pub taps: u32,
    /// 系数，行优先，长度 rows*cols*taps。
    pub coeffs: Vec<f32>,
}

/// 单段 PEQ（biquad）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeqSection {
    /// b0
    pub b0: f32,
    /// b1
    pub b1: f32,
    /// b2
    pub b2: f32,
    /// a1
    pub a1: f32,
    /// a2
    pub a2: f32,
}

/// 单 lane 保护项。
#[derive(Debug, Clone, PartialEq)]
pub struct ProtectionEntry {
    /// 目标 lane。
    pub lane: u16,
    /// 延迟（样本）。
    pub delay: u32,
    /// 极性，+1 或 -1。
    pub polarity: i8,
    /// 增益（dB）。
    pub gain_db: f32,
    /// PEQ 段。
    pub peq: Vec<PeqSection>,
}

/// 保护区段。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProtectionSection {
    /// lane 数。
    pub lanes: u16,
    /// 保护项。
    pub entries: Vec<ProtectionEntry>,
}

/// 路由区段。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoutingSection {
    /// DAC 通道总数。
    pub dac_channels: u16,
    /// (lane, dac) 对；未列出的 DAC 通道输出静音。
    pub routes: Vec<(u16, u16)>,
}

/// 一份完整方案。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// 块大小（帧）。
    pub block_size: u32,
    /// 采样率。
    pub sample_rate: u32,
    /// 内容布局。
    pub content: Option<ContentSection>,
    /// 分频。
    pub crossover: Option<CrossoverSection>,
    /// 矩阵。
    pub matrix: Option<MatrixSection>,
    /// 保护。
    pub protection: Option<ProtectionSection>,
    /// 路由。
    pub routing: Option<RoutingSection>,
}

impl Plan {
    /// 区段数量。
    pub fn stage_count(&self) -> u16 {
        [
            self.content.is_some(),
            self.crossover.is_some(),
            self.matrix.is_some(),
            self.protection.is_some(),
            self.routing.is_some(),
        ]
        .iter()
        .filter(|present| **present)
        .count() as u16
    }

    /// 编码为字节。
    pub fn encode(&self) -> Vec<u8> {
        let mut sections: Vec<(SectionKind, Vec<u8>)> = Vec::new();
        if let Some(section) = &self.content {
            sections.push((SectionKind::Content, encode_content(section)));
        }
        if let Some(section) = &self.crossover {
            sections.push((SectionKind::Crossover, encode_crossover(section)));
        }
        if let Some(section) = &self.matrix {
            sections.push((SectionKind::Matrix, encode_matrix(section)));
        }
        if let Some(section) = &self.protection {
            sections.push((SectionKind::Protection, encode_protection(section)));
        }
        if let Some(section) = &self.routing {
            sections.push((SectionKind::Routing, encode_routing(section)));
        }
        sections.sort_by_key(|(kind, _)| *kind);

        let mut body = vec![0u8; HEADER_BYTES];
        let mut table = Vec::with_capacity(2 + sections.len() * TABLE_ENTRY_BYTES);
        put_u16(&mut table, sections.len() as u16);
        for (kind, payload) in &sections {
            let offset = body.len() as u64;
            body.extend_from_slice(payload);
            put_u16(&mut table, *kind as u16);
            put_u64(&mut table, offset);
            put_u64(&mut table, payload.len() as u64);
            put_u32(&mut table, crc32c(payload));
        }
        let table_offset = body.len() as u64;
        body.extend_from_slice(&table);
        let total = (body.len() + TRAILER_BYTES) as u32;

        body[0..8].copy_from_slice(&MAGIC);
        body[8..10].copy_from_slice(&ABI_VERSION.to_le_bytes());
        body[10..12].copy_from_slice(&self.stage_count().to_le_bytes());
        body[12..16].copy_from_slice(&self.block_size.to_le_bytes());
        body[16..20].copy_from_slice(&self.sample_rate.to_le_bytes());
        body[20..28].copy_from_slice(&table_offset.to_le_bytes());
        body[28..32].copy_from_slice(&total.to_le_bytes());

        let mut graph_input = Vec::with_capacity(GRAPH_ID_DOMAIN.len() + body.len());
        graph_input.extend_from_slice(GRAPH_ID_DOMAIN);
        graph_input.extend_from_slice(&body);
        let graph_id = sha256(&graph_input);

        let crc = crc32c(&body);
        body.extend_from_slice(&crc.to_le_bytes());
        body.extend_from_slice(&graph_id);
        body
    }

    /// 从字节解码并做全部校验。
    pub fn decode(data: &[u8]) -> Result<Plan, PlanError> {
        if data.len() < HEADER_BYTES + TRAILER_BYTES {
            return Err(PlanError::Truncated);
        }
        if data[0..8] != MAGIC {
            return Err(PlanError::BadMagic);
        }
        let abi = u16::from_le_bytes([data[8], data[9]]);
        if abi != ABI_VERSION {
            return Err(PlanError::UnsupportedAbi(abi));
        }
        let stage_count = u16::from_le_bytes([data[10], data[11]]);
        let block_size = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let sample_rate = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let table_offset = u64::from_le_bytes([
            data[20], data[21], data[22], data[23], data[24], data[25], data[26], data[27],
        ]) as usize;
        let total = u32::from_le_bytes([data[28], data[29], data[30], data[31]]) as usize;
        if total != data.len() {
            return Err(PlanError::BadTotalBytes {
                declared: total as u32,
                actual: data.len(),
            });
        }
        let body_end = data.len() - TRAILER_BYTES;
        let expected_crc = u32::from_le_bytes([
            data[body_end],
            data[body_end + 1],
            data[body_end + 2],
            data[body_end + 3],
        ]);
        if crc32c(&data[..body_end]) != expected_crc {
            return Err(PlanError::CrcMismatch);
        }
        let mut graph_input = Vec::with_capacity(GRAPH_ID_DOMAIN.len() + body_end);
        graph_input.extend_from_slice(GRAPH_ID_DOMAIN);
        graph_input.extend_from_slice(&data[..body_end]);
        let expected_id = sha256(&graph_input);
        if data[body_end + 4..body_end + 36] != expected_id {
            return Err(PlanError::GraphIdMismatch);
        }
        if table_offset < HEADER_BYTES || table_offset > body_end {
            return Err(PlanError::Truncated);
        }
        let table = &data[table_offset..body_end];
        if table.len() < 2 {
            return Err(PlanError::Truncated);
        }
        let count = u16::from_le_bytes([table[0], table[1]]) as usize;
        if count != stage_count as usize {
            return Err(PlanError::BadPayload("stage_count mismatch"));
        }
        if table.len() != 2 + count * TABLE_ENTRY_BYTES {
            return Err(PlanError::Truncated);
        }
        let mut plan = Plan {
            block_size,
            sample_rate,
            ..Plan::default()
        };
        for index in 0..count {
            let base = 2 + index * TABLE_ENTRY_BYTES;
            let kind_raw = u16::from_le_bytes([table[base], table[base + 1]]);
            let offset = u64::from_le_bytes([
                table[base + 2],
                table[base + 3],
                table[base + 4],
                table[base + 5],
                table[base + 6],
                table[base + 7],
                table[base + 8],
                table[base + 9],
            ]) as usize;
            let bytes = u64::from_le_bytes([
                table[base + 10],
                table[base + 11],
                table[base + 12],
                table[base + 13],
                table[base + 14],
                table[base + 15],
                table[base + 16],
                table[base + 17],
            ]) as usize;
            let crc = u32::from_le_bytes([
                table[base + 18],
                table[base + 19],
                table[base + 20],
                table[base + 21],
            ]);
            let kind =
                SectionKind::from_u16(kind_raw).ok_or(PlanError::UnknownSection(kind_raw))?;
            let end = offset.checked_add(bytes).ok_or(PlanError::Truncated)?;
            if offset < HEADER_BYTES || end > table_offset {
                return Err(PlanError::Truncated);
            }
            let payload = &data[offset..end];
            if crc32c(payload) != crc {
                return Err(PlanError::SectionCrc { kind: kind_raw });
            }
            match kind {
                SectionKind::Content => plan.content = Some(decode_content(payload)?),
                SectionKind::Crossover => plan.crossover = Some(decode_crossover(payload)?),
                SectionKind::Matrix => plan.matrix = Some(decode_matrix(payload)?),
                SectionKind::Protection => plan.protection = Some(decode_protection(payload)?),
                SectionKind::Routing => plan.routing = Some(decode_routing(payload)?),
            }
        }
        Ok(plan)
    }

    /// graph_id（十六进制）。
    pub fn graph_id_hex(&self) -> String {
        let bytes = self.encode();
        let body_end = bytes.len() - TRAILER_BYTES;
        crate::sha256::sha256_hex(&bytes[body_end + 4..body_end + 36])
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], PlanError> {
        let end = self.pos.checked_add(n).ok_or(PlanError::Truncated)?;
        if end > self.data.len() {
            return Err(PlanError::Truncated);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, PlanError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PlanError> {
        let slice = self.take(2)?;
        Ok(u16::from_le_bytes([slice[0], slice[1]]))
    }

    fn u32(&mut self) -> Result<u32, PlanError> {
        let slice = self.take(4)?;
        Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn i8(&mut self) -> Result<i8, PlanError> {
        Ok(self.u8()? as i8)
    }

    fn f32(&mut self) -> Result<f32, PlanError> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn finish(&self) -> Result<(), PlanError> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(PlanError::BadPayload("trailing bytes"))
        }
    }
}

fn encode_content(section: &ContentSection) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + section.folds.len() * 8);
    put_u16(&mut out, section.in_channels);
    put_u16(&mut out, section.out_lanes);
    put_u16(&mut out, section.folds.len() as u16);
    for fold in &section.folds {
        put_u16(&mut out, fold.src);
        put_u16(&mut out, fold.dst);
        put_f32(&mut out, fold.gain);
    }
    out
}

fn decode_content(data: &[u8]) -> Result<ContentSection, PlanError> {
    let mut reader = Reader::new(data);
    let in_channels = reader.u16()?;
    let out_lanes = reader.u16()?;
    let count = reader.u16()? as usize;
    let mut folds = Vec::with_capacity(count);
    for _ in 0..count {
        folds.push(Fold {
            src: reader.u16()?,
            dst: reader.u16()?,
            gain: reader.f32()?,
        });
    }
    reader.finish()?;
    Ok(ContentSection {
        in_channels,
        out_lanes,
        folds,
    })
}

fn encode_crossover(section: &CrossoverSection) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(&mut out, section.lanes);
    put_u16(&mut out, section.filters.len() as u16);
    for filter in &section.filters {
        put_u16(&mut out, filter.lane);
        put_u32(&mut out, filter.coeffs.len() as u32);
        for coeff in &filter.coeffs {
            put_f32(&mut out, *coeff);
        }
    }
    out
}

fn decode_crossover(data: &[u8]) -> Result<CrossoverSection, PlanError> {
    let mut reader = Reader::new(data);
    let lanes = reader.u16()?;
    let count = reader.u16()? as usize;
    let mut filters = Vec::with_capacity(count);
    for _ in 0..count {
        let lane = reader.u16()?;
        let taps = reader.u32()? as usize;
        if taps > 1 << 20 {
            return Err(PlanError::BadPayload("crossover taps out of range"));
        }
        let mut coeffs = Vec::with_capacity(taps);
        for _ in 0..taps {
            coeffs.push(reader.f32()?);
        }
        filters.push(CrossoverFilter { lane, coeffs });
    }
    reader.finish()?;
    Ok(CrossoverSection { lanes, filters })
}

fn encode_matrix(section: &MatrixSection) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + section.coeffs.len() * 4);
    put_u16(&mut out, section.rows);
    put_u16(&mut out, section.cols);
    out.push(section.decimation);
    out.push(section.interpolation);
    put_u32(&mut out, section.taps);
    put_u32(&mut out, section.coeffs.len() as u32);
    for coeff in &section.coeffs {
        put_f32(&mut out, *coeff);
    }
    out
}

fn decode_matrix(data: &[u8]) -> Result<MatrixSection, PlanError> {
    let mut reader = Reader::new(data);
    let rows = reader.u16()?;
    let cols = reader.u16()?;
    let decimation = reader.u8()?;
    let interpolation = reader.u8()?;
    let taps = reader.u32()?;
    let len = reader.u32()? as usize;
    let expected = rows as usize * cols as usize * taps as usize;
    if len != expected || len > 1 << 22 {
        return Err(PlanError::BadPayload("matrix coefficient count mismatch"));
    }
    let mut coeffs = Vec::with_capacity(len);
    for _ in 0..len {
        coeffs.push(reader.f32()?);
    }
    reader.finish()?;
    Ok(MatrixSection {
        rows,
        cols,
        decimation,
        interpolation,
        taps,
        coeffs,
    })
}

fn encode_protection(section: &ProtectionSection) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(&mut out, section.lanes);
    put_u16(&mut out, section.entries.len() as u16);
    for entry in &section.entries {
        put_u16(&mut out, entry.lane);
        put_u32(&mut out, entry.delay);
        out.push(entry.polarity as u8);
        put_f32(&mut out, entry.gain_db);
        put_u16(&mut out, entry.peq.len() as u16);
        for section in &entry.peq {
            for value in [section.b0, section.b1, section.b2, section.a1, section.a2] {
                put_f32(&mut out, value);
            }
        }
    }
    out
}

fn decode_protection(data: &[u8]) -> Result<ProtectionSection, PlanError> {
    let mut reader = Reader::new(data);
    let lanes = reader.u16()?;
    let count = reader.u16()? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let lane = reader.u16()?;
        let delay = reader.u32()?;
        let polarity = reader.i8()?;
        if polarity != 1 && polarity != -1 {
            return Err(PlanError::BadPayload("polarity must be +1 or -1"));
        }
        let gain_db = reader.f32()?;
        let peq_count = reader.u16()? as usize;
        let mut peq = Vec::with_capacity(peq_count);
        for _ in 0..peq_count {
            let b0 = reader.f32()?;
            let b1 = reader.f32()?;
            let b2 = reader.f32()?;
            let a1 = reader.f32()?;
            let a2 = reader.f32()?;
            peq.push(PeqSection { b0, b1, b2, a1, a2 });
        }
        entries.push(ProtectionEntry {
            lane,
            delay,
            polarity,
            gain_db,
            peq,
        });
    }
    reader.finish()?;
    Ok(ProtectionSection { lanes, entries })
}

fn encode_routing(section: &RoutingSection) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(&mut out, section.dac_channels);
    put_u16(&mut out, section.routes.len() as u16);
    for (lane, dac) in &section.routes {
        put_u16(&mut out, *lane);
        put_u16(&mut out, *dac);
    }
    out
}

fn decode_routing(data: &[u8]) -> Result<RoutingSection, PlanError> {
    let mut reader = Reader::new(data);
    let dac_channels = reader.u16()?;
    let count = reader.u16()? as usize;
    let mut routes = Vec::with_capacity(count);
    for _ in 0..count {
        routes.push((reader.u16()?, reader.u16()?));
    }
    reader.finish()?;
    Ok(RoutingSection {
        dac_channels,
        routes,
    })
}
