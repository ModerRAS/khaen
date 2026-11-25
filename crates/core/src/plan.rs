//! 方案文件 ABI v3（见 docs/CONTRACTS.md §A）。
//!
//! 布局：Header(32B) | Sections | SectionTable | Trailer(36B)
//!
//! - 解析必须校验每段 CRC-32C 与整体 CRC，任一不符拒绝加载（fail-closed）。
//! - graph_id = sha256("khaen-plan/v3" || 除 trailer 外的全部字节)。
//! - 未知区段类型拒绝加载，不静默跳过。
//! - 解码只接受**规范形式**（= encode 的输出）：区段表按 kind 升序无重复、payload 连续
//!   不重叠、表紧跟最后一个 payload；违反即拒绝，保证 encode(decode(data)) == data。

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

/// 单个区段的条目数上限：ABI v3 里 folds / filters / entries / PEQ / routes 的计数域是 u16。
pub const MAX_SECTION_ENTRIES: usize = u16::MAX as usize;
/// 单个分频滤波器的抽头数上限：与解码器防护一致，保证 encode 出来的文件 decode 得回去。
pub const MAX_CROSSOVER_TAPS: u32 = 1 << 20;
/// 矩阵系数总数上限：与解码器防护一致。
pub const MAX_MATRIX_COEFFICIENTS: u32 = 1 << 22;

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
    /// 编码时折叠表条目数超过 u16 计数域。
    TooManyFolds { count: usize, limit: usize },
    /// 编码时分频滤波器数量超过 u16 计数域。
    TooManyCrossoverFilters { count: usize, limit: usize },
    /// 编码时单个分频滤波器的抽头数超过 ABI v3 解码器上限。
    CrossoverTapsTooLong { lane: u16, taps: usize, limit: u32 },
    /// 编码时矩阵系数个数超过 ABI v3 解码器上限。
    MatrixCoefficientsTooMany { count: usize, limit: u32 },
    /// 编码时保护项数量超过 u16 计数域。
    TooManyProtectionEntries { count: usize, limit: usize },
    /// 编码时单条保护项的 PEQ 段数超过 u16 计数域。
    TooManyPeqSections {
        lane: u16,
        count: usize,
        limit: usize,
    },
    /// 编码时路由表项数超过 u16 计数域。
    TooManyRoutes { count: usize, limit: usize },
    /// 编码后的总字节数超过头部 `total_bytes` 的 u32 域。
    EncodedTooLarge { bytes: u64 },
    /// 解码时同一区段 kind 在区段表里出现多次（规范形式要求每个 kind 至多一次）。
    DuplicateSection { kind: u16 },
    /// 解码时区段表未按规范顺序（kind 严格升序）排列。
    SectionOrder { previous: u16, current: u16 },
    /// 解码时区段 payload 布局非规范：未紧跟上一个 payload（重叠 / 间隙 / 起点不对）。
    SectionLayout {
        kind: u16,
        expected_offset: u64,
        actual_offset: u64,
    },
    /// 解码时区段表位置非规范：表必须紧跟在最后一个 payload 之后。
    SectionTableOffset { expected: u64, actual: u64 },
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
            PlanError::TooManyFolds { count, limit } => {
                write!(f, "cannot encode {count} folds (ABI v3 limit {limit})")
            }
            PlanError::TooManyCrossoverFilters { count, limit } => {
                write!(f, "cannot encode {count} crossover filters (ABI v3 limit {limit})")
            }
            PlanError::CrossoverTapsTooLong { lane, taps, limit } => write!(
                f,
                "cannot encode crossover lane {lane} with {taps} taps (ABI v3 limit {limit})"
            ),
            PlanError::MatrixCoefficientsTooMany { count, limit } => write!(
                f,
                "cannot encode {count} matrix coefficients (ABI v3 limit {limit})"
            ),
            PlanError::TooManyProtectionEntries { count, limit } => {
                write!(f, "cannot encode {count} protection entries (ABI v3 limit {limit})")
            }
            PlanError::TooManyPeqSections { lane, count, limit } => write!(
                f,
                "cannot encode protection lane {lane} with {count} PEQ sections (ABI v3 limit {limit})"
            ),
            PlanError::TooManyRoutes { count, limit } => {
                write!(f, "cannot encode {count} routes (ABI v3 limit {limit})")
            }
            PlanError::EncodedTooLarge { bytes } => {
                write!(f, "encoded plan is {bytes} bytes (ABI v3 total_bytes is u32)")
            }
            PlanError::DuplicateSection { kind } => {
                write!(f, "duplicate section kind {kind} in section table")
            }
            PlanError::SectionOrder { previous, current } => write!(
                f,
                "section table not in canonical order: kind {current} after {previous} (must be strictly ascending)"
            ),
            PlanError::SectionLayout {
                kind,
                expected_offset,
                actual_offset,
            } => write!(
                f,
                "section {kind} payload at offset {actual_offset}, canonical layout expects {expected_offset}"
            ),
            PlanError::SectionTableOffset { expected, actual } => write!(
                f,
                "section table at offset {actual}, canonical layout expects {expected} (immediately after the last payload)"
            ),
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

    /// 编码为字节；任何 ABI v3 字段域放不下的计数 / 长度都返回错误（fail-closed）。
    ///
    /// 只做**字段域**检查：各区段条目计数是 u16、单个分频滤波器抽头数与矩阵系数总数有
    /// 解码器上限、文件总长是 u32；超限一律返回 PlanError，绝不静默截断成
    /// "encode 得出来、decode 不回去"的文件。
    ///
    /// 不做区段语义校验：矩阵系数个数与 rows * cols * taps 是否一致、lane / DAC 引用
    /// 是否越界等属于 khaen_host::admission 与 khaen_plan_builder 的职责，因此语义不
    /// 自洽但字段可表示的方案在这里仍然编码成功（保持既有调用方的容忍度）。
    pub fn encode_checked(&self) -> Result<Vec<u8>, PlanError> {
        let mut sections: Vec<(SectionKind, Vec<u8>)> = Vec::new();
        if let Some(section) = &self.content {
            sections.push((SectionKind::Content, encode_content(section)?));
        }
        if let Some(section) = &self.crossover {
            sections.push((SectionKind::Crossover, encode_crossover(section)?));
        }
        if let Some(section) = &self.matrix {
            sections.push((SectionKind::Matrix, encode_matrix(section)?));
        }
        if let Some(section) = &self.protection {
            sections.push((SectionKind::Protection, encode_protection(section)?));
        }
        if let Some(section) = &self.routing {
            sections.push((SectionKind::Routing, encode_routing(section)?));
        }
        sections.sort_by_key(|(kind, _)| *kind);

        // stage_count 由五个固定 Optional 区段计数得到，最小上界是 5，u16 域必然放得下。
        let stage_count = self.stage_count();
        debug_assert_eq!(sections.len(), usize::from(stage_count));

        let mut body = vec![0u8; HEADER_BYTES];
        let mut table = Vec::with_capacity(2 + sections.len() * TABLE_ENTRY_BYTES);
        put_u16(&mut table, stage_count);
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
        // 总长超 u32 时 fail-closed（不截断成看似合法的小文件）。
        let total = check_total_bytes(body.len() + TRAILER_BYTES)?;

        body[0..8].copy_from_slice(&MAGIC);
        body[8..10].copy_from_slice(&ABI_VERSION.to_le_bytes());
        body[10..12].copy_from_slice(&stage_count.to_le_bytes());
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
        Ok(body)
    }

    /// 编码为字节；方案超出 ABI v3 字段域时 **panic**（fail-closed）。
    ///
    /// 便捷包装，等价于 encode_checked().unwrap_or_else(|error| panic!(...))；
    /// 需要错误返回值时用 Plan::encode_checked。保持不可失败签名是为了不破坏既有
    /// 调用方：host 准入只取长度（admit），plan-builder 的 build_bytes 返回已校验
    /// 方案的字节，两者都在本 crate 的写范围之外。
    ///
    /// # Panics
    ///
    /// 方案的折叠 / 滤波器 / 保护 / 路由计数超过 u16 域、分频抽头或矩阵系数超过解码器
    /// 上限、编码后总长超过 u32 时 panic。能由 Plan::decode 得到或经
    /// khaen_plan_builder 校验的方案不会触发。
    pub fn encode(&self) -> Vec<u8> {
        self.encode_checked()
            .unwrap_or_else(|error| panic!("plan exceeds ABI v3 encoding limits: {error}"))
    }

    /// 从字节解码并做全部校验。
    ///
    /// 只接受**规范形式**（[Plan::encode_checked] 的输出）：区段表按 kind 严格升序、
    /// 每个 kind 至多一次，payload 从 header 之后连续排布（无重叠 / 间隙），区段表紧跟
    /// 最后一个 payload。违反任一条都 fail-closed 拒绝（[PlanError::DuplicateSection] /
    /// [PlanError::SectionOrder] / [PlanError::SectionLayout] /
    /// [PlanError::SectionTableOffset]），不静默重排或去重。因此任何被接受的字节串都满足
    /// encode(decode(data)) == data，同一语义方案只有一种字节串。
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
        let table_offset = usize::try_from(u64::from_le_bytes([
            data[20], data[21], data[22], data[23], data[24], data[25], data[26], data[27],
        ]))
        .map_err(|_| PlanError::Truncated)?;
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
        // ---- 规范形式校验（fail-closed）：区段表按 kind 严格升序、每个 kind 至多一次；
        // payload 从 header 之后连续排布（互不重叠、无间隙），表紧跟最后一个 payload。
        // 先整表校验完再解析 payload：非规范文件不会先"部分解出"再随机报别的错。
        let mut entries: Vec<TableEntry> = Vec::with_capacity(count);
        let mut cursor = HEADER_BYTES;
        let mut previous_kind: Option<u16> = None;
        for index in 0..count {
            let entry = parse_table_entry(table, index)?;
            let kind_raw = entry.kind as u16;
            if let Some(previous) = previous_kind {
                if kind_raw == previous {
                    return Err(PlanError::DuplicateSection { kind: kind_raw });
                }
                if kind_raw < previous {
                    return Err(PlanError::SectionOrder {
                        previous,
                        current: kind_raw,
                    });
                }
            }
            if entry.offset != cursor {
                return Err(PlanError::SectionLayout {
                    kind: kind_raw,
                    expected_offset: cursor as u64,
                    actual_offset: entry.offset as u64,
                });
            }
            if entry.end > table_offset {
                return Err(PlanError::SectionTableOffset {
                    expected: entry.end as u64,
                    actual: table_offset as u64,
                });
            }
            previous_kind = Some(kind_raw);
            cursor = entry.end;
            entries.push(entry);
        }
        if cursor != table_offset {
            return Err(PlanError::SectionTableOffset {
                expected: cursor as u64,
                actual: table_offset as u64,
            });
        }

        let mut plan = Plan {
            block_size,
            sample_rate,
            ..Plan::default()
        };
        for entry in entries {
            let payload = &data[entry.offset..entry.end];
            if crc32c(payload) != entry.crc {
                return Err(PlanError::SectionCrc {
                    kind: entry.kind as u16,
                });
            }
            match entry.kind {
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
    ///
    /// trailer 里存的就是 32 字节摘要 `sha256("khaen-plan/v3" || body)`；这里只做十六进制
    /// 编码，**不再**对它散列一次（双重散列会与 CONTRACTS.md A.3 的 graph_id 定义不符）。
    pub fn graph_id_hex(&self) -> String {
        let bytes = self.encode();
        let body_end = bytes.len() - TRAILER_BYTES;
        crate::sha256::hex_lower(&bytes[body_end + 4..body_end + 36])
    }
}

/// 区段表项（22 字节）：kind(2) + offset(8) + bytes(8) + crc32c(4)。
struct TableEntry {
    /// 已解析的区段类型（未知类型在 parse 阶段即拒绝）。
    kind: SectionKind,
    /// payload 起点。
    offset: usize,
    /// payload 终点（不含）。
    end: usize,
    /// payload 的 crc32c。
    crc: u32,
}

/// 解析第 index 个区段表项；长度域一律显式转换，不在 32 位平台上静默截断。
fn parse_table_entry(table: &[u8], index: usize) -> Result<TableEntry, PlanError> {
    let base = 2 + index * TABLE_ENTRY_BYTES;
    let kind_raw = u16::from_le_bytes([table[base], table[base + 1]]);
    let offset = usize::try_from(u64::from_le_bytes([
        table[base + 2],
        table[base + 3],
        table[base + 4],
        table[base + 5],
        table[base + 6],
        table[base + 7],
        table[base + 8],
        table[base + 9],
    ]))
    .map_err(|_| PlanError::Truncated)?;
    let bytes = usize::try_from(u64::from_le_bytes([
        table[base + 10],
        table[base + 11],
        table[base + 12],
        table[base + 13],
        table[base + 14],
        table[base + 15],
        table[base + 16],
        table[base + 17],
    ]))
    .map_err(|_| PlanError::Truncated)?;
    let crc = u32::from_le_bytes([
        table[base + 18],
        table[base + 19],
        table[base + 20],
        table[base + 21],
    ]);
    let kind = SectionKind::from_u16(kind_raw).ok_or(PlanError::UnknownSection(kind_raw))?;
    let end = offset.checked_add(bytes).ok_or(PlanError::Truncated)?;
    Ok(TableEntry {
        kind,
        offset,
        end,
        crc,
    })
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

fn encode_content(section: &ContentSection) -> Result<Vec<u8>, PlanError> {
    let fold_count = u16::try_from(section.folds.len()).map_err(|_| PlanError::TooManyFolds {
        count: section.folds.len(),
        limit: MAX_SECTION_ENTRIES,
    })?;
    let mut out = Vec::with_capacity(6 + section.folds.len() * 8);
    put_u16(&mut out, section.in_channels);
    put_u16(&mut out, section.out_lanes);
    put_u16(&mut out, fold_count);
    for fold in &section.folds {
        put_u16(&mut out, fold.src);
        put_u16(&mut out, fold.dst);
        put_f32(&mut out, fold.gain);
    }
    Ok(out)
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

fn encode_crossover(section: &CrossoverSection) -> Result<Vec<u8>, PlanError> {
    let filter_count =
        u16::try_from(section.filters.len()).map_err(|_| PlanError::TooManyCrossoverFilters {
            count: section.filters.len(),
            limit: MAX_SECTION_ENTRIES,
        })?;
    let mut out = Vec::new();
    put_u16(&mut out, section.lanes);
    put_u16(&mut out, filter_count);
    for filter in &section.filters {
        let taps =
            u32::try_from(filter.coeffs.len()).map_err(|_| PlanError::CrossoverTapsTooLong {
                lane: filter.lane,
                taps: filter.coeffs.len(),
                limit: MAX_CROSSOVER_TAPS,
            })?;
        if taps > MAX_CROSSOVER_TAPS {
            return Err(PlanError::CrossoverTapsTooLong {
                lane: filter.lane,
                taps: filter.coeffs.len(),
                limit: MAX_CROSSOVER_TAPS,
            });
        }
        put_u16(&mut out, filter.lane);
        put_u32(&mut out, taps);
        for coeff in &filter.coeffs {
            put_f32(&mut out, *coeff);
        }
    }
    Ok(out)
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

fn encode_matrix(section: &MatrixSection) -> Result<Vec<u8>, PlanError> {
    let coefficient_count =
        u32::try_from(section.coeffs.len()).map_err(|_| PlanError::MatrixCoefficientsTooMany {
            count: section.coeffs.len(),
            limit: MAX_MATRIX_COEFFICIENTS,
        })?;
    if coefficient_count > MAX_MATRIX_COEFFICIENTS {
        return Err(PlanError::MatrixCoefficientsTooMany {
            count: section.coeffs.len(),
            limit: MAX_MATRIX_COEFFICIENTS,
        });
    }
    let mut out = Vec::with_capacity(10 + section.coeffs.len() * 4);
    put_u16(&mut out, section.rows);
    put_u16(&mut out, section.cols);
    out.push(section.decimation);
    out.push(section.interpolation);
    put_u32(&mut out, section.taps);
    put_u32(&mut out, coefficient_count);
    for coeff in &section.coeffs {
        put_f32(&mut out, *coeff);
    }
    Ok(out)
}

fn decode_matrix(data: &[u8]) -> Result<MatrixSection, PlanError> {
    let mut reader = Reader::new(data);
    let rows = reader.u16()?;
    let cols = reader.u16()?;
    let decimation = reader.u8()?;
    let interpolation = reader.u8()?;
    let taps = reader.u32()?;
    let len = reader.u32()? as usize;
    // rows*cols*taps 的代数上界 < 2^64，在 64 位 usize 上不会溢出，但在 32 位平台会；
    // 用 checked 乘法保证任何平台上恶意维度都返回 BadPayload 而不是 panic（解码不 panic 是契约）。
    let expected = (rows as usize)
        .checked_mul(cols as usize)
        .and_then(|cells| cells.checked_mul(taps as usize))
        .ok_or(PlanError::BadPayload("matrix coefficient count overflow"))?;
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

fn encode_protection(section: &ProtectionSection) -> Result<Vec<u8>, PlanError> {
    let entry_count =
        u16::try_from(section.entries.len()).map_err(|_| PlanError::TooManyProtectionEntries {
            count: section.entries.len(),
            limit: MAX_SECTION_ENTRIES,
        })?;
    let mut out = Vec::new();
    put_u16(&mut out, section.lanes);
    put_u16(&mut out, entry_count);
    for entry in &section.entries {
        let peq_count =
            u16::try_from(entry.peq.len()).map_err(|_| PlanError::TooManyPeqSections {
                lane: entry.lane,
                count: entry.peq.len(),
                limit: MAX_SECTION_ENTRIES,
            })?;
        put_u16(&mut out, entry.lane);
        put_u32(&mut out, entry.delay);
        out.push(entry.polarity as u8);
        put_f32(&mut out, entry.gain_db);
        put_u16(&mut out, peq_count);
        for section in &entry.peq {
            for value in [section.b0, section.b1, section.b2, section.a1, section.a2] {
                put_f32(&mut out, value);
            }
        }
    }
    Ok(out)
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

fn encode_routing(section: &RoutingSection) -> Result<Vec<u8>, PlanError> {
    let route_count =
        u16::try_from(section.routes.len()).map_err(|_| PlanError::TooManyRoutes {
            count: section.routes.len(),
            limit: MAX_SECTION_ENTRIES,
        })?;
    let mut out = Vec::new();
    put_u16(&mut out, section.dac_channels);
    put_u16(&mut out, route_count);
    for (lane, dac) in &section.routes {
        put_u16(&mut out, *lane);
        put_u16(&mut out, *dac);
    }
    Ok(out)
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

/// 把编码后的总字节数换算成头部 total_bytes 字段（u32）；超限 fail-closed。
///
/// 单独抽出来是为了能被单元测试直接覆盖：真实构造 > 4 GiB 的方案在测试里不现实，
/// 因此这条守卫在守卫函数上验证（合法值 + 恰好溢出），而不是造假方案。
fn check_total_bytes(total: usize) -> Result<u32, PlanError> {
    u32::try_from(total).map_err(|_| PlanError::EncodedTooLarge {
        bytes: u64::try_from(total).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::{check_total_bytes, PlanError};

    #[test]
    fn total_bytes_guard_is_exact_and_fail_closed() {
        assert_eq!(check_total_bytes(0), Ok(0));
        assert_eq!(check_total_bytes(u32::MAX as usize), Ok(u32::MAX));

        let error = PlanError::EncodedTooLarge {
            bytes: u64::from(u32::MAX) + 1,
        };
        let text = error.to_string();
        assert!(text.contains("4294967296"), "{text}");
        assert!(text.contains("u32"), "{text}");

        // 32 位平台上 usize 放不下 u32::MAX + 1；该守卫只在 64 位平台可被触发。
        #[cfg(target_pointer_width = "64")]
        {
            let too_large = u32::MAX as usize + 1;
            assert_eq!(
                check_total_bytes(too_large),
                Err(PlanError::EncodedTooLarge {
                    bytes: u64::try_from(too_large).unwrap_or(u64::MAX),
                })
            );
        }
    }
}
