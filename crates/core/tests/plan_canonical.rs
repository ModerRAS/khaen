//! `Plan::decode` 的规范形式（canonical form）测试（T10d）。
//!
//! 规范形式 = `Plan::encode_checked()` 的输出，也是 decode 唯一接受的字节串：
//! 区段表按 kind 严格升序、每个 kind 至多一次；payload 从 header 之后连续排布
//! （无重叠 / 间隙）；区段表紧跟最后一个 payload。
//!
//! 这里的手搓文件都是「先 encode 出合法文件，再改区段表 / 偏移，最后重算整体 CRC 与
//! graph_id（reseal）」得到的，因此一定会走到新增的规范化检查，而不是先被 CRC 拦下。

use khaen_core::plan::{
    ContentSection, Fold, MatrixSection, Plan, PlanError, RoutingSection, GRAPH_ID_DOMAIN,
};

const HEADER_BYTES: usize = 32;
const TABLE_ENTRY_BYTES: usize = 22;
const TRAILER_BYTES: usize = 36;

fn content_only_plan() -> Plan {
    Plan {
        block_size: 128,
        sample_rate: 48_000,
        content: Some(ContentSection {
            in_channels: 2,
            out_lanes: 1,
            folds: vec![
                Fold {
                    src: 0,
                    dst: 0,
                    gain: 1.0,
                },
                Fold {
                    src: 1,
                    dst: 0,
                    gain: 0.5,
                },
            ],
        }),
        ..Plan::default()
    }
}

fn content_routing_plan() -> Plan {
    Plan {
        routing: Some(RoutingSection {
            dac_channels: 1,
            routes: vec![(0, 0), (0, 1)],
        }),
        ..content_only_plan()
    }
}

/// 头部里的 section_table_offset。
fn table_offset(bytes: &[u8]) -> usize {
    usize::try_from(u64::from_le_bytes(
        bytes[20..28]
            .try_into()
            .expect("header table_offset is 8 bytes"),
    ))
    .expect("fits usize")
}

/// 第 index 个表项的起点（表项：kind 2 + offset 8 + bytes 8 + crc 4）。
fn entry_base(bytes: &[u8], index: usize) -> usize {
    table_offset(bytes) + 2 + index * TABLE_ENTRY_BYTES
}

fn set_entry_kind(bytes: &mut [u8], index: usize, kind: u16) {
    let base = entry_base(bytes, index);
    bytes[base..base + 2].copy_from_slice(&kind.to_le_bytes());
}

fn set_entry_offset(bytes: &mut [u8], index: usize, offset: u64) {
    let base = entry_base(bytes, index);
    bytes[base + 2..base + 10].copy_from_slice(&offset.to_le_bytes());
}

fn set_entry_bytes(bytes: &mut [u8], index: usize, size: u64) {
    let base = entry_base(bytes, index);
    bytes[base + 10..base + 18].copy_from_slice(&size.to_le_bytes());
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// 第 index 个表项指向的 payload 区间（offset, bytes）。
fn entry_payload_range(bytes: &[u8], index: usize) -> (usize, usize) {
    let base = entry_base(bytes, index);
    let offset = usize::try_from(read_u64(bytes, base + 2)).expect("fits usize");
    let size = usize::try_from(read_u64(bytes, base + 10)).expect("fits usize");
    (offset, size)
}

/// 改 payload 后重算表项里存的区段 crc32c，否则会先被 SectionCrc 拦下。
fn fix_entry_crc(bytes: &mut [u8], index: usize) {
    let base = entry_base(bytes, index);
    let (offset, size) = entry_payload_range(bytes, index);
    let crc = khaen_core::crc32c::crc32c(&bytes[offset..offset + size]);
    bytes[base + 18..base + 22].copy_from_slice(&crc.to_le_bytes());
}

/// 改完表后重算整体 crc32c 与 graph_id，让文件能穿过 CRC / graph_id 校验，
/// 从而真正检验规范化检查。
fn reseal(bytes: &mut [u8]) {
    let body_end = bytes.len() - TRAILER_BYTES;
    let crc = khaen_core::crc32c::crc32c(&bytes[..body_end]);
    bytes[body_end..body_end + 4].copy_from_slice(&crc.to_le_bytes());
    let mut input = Vec::with_capacity(GRAPH_ID_DOMAIN.len() + body_end);
    input.extend_from_slice(GRAPH_ID_DOMAIN);
    input.extend_from_slice(&bytes[..body_end]);
    let graph_id = khaen_core::sha256::sha256(&input);
    bytes[body_end + 4..body_end + 36].copy_from_slice(&graph_id);
}

#[test]
fn accepted_files_are_exactly_the_canonical_bytes() {
    // 空方案 / 单区段 / 多区段：decode 只接受 encode 的字节串，
    // 且 decode → encode 必须逐字节还原（同语义只有一种字节串）。
    let plans = [
        Plan {
            block_size: 64,
            sample_rate: 96_000,
            ..Plan::default()
        },
        content_only_plan(),
        content_routing_plan(),
    ];
    for plan in plans {
        let bytes = plan.encode();
        let decoded = Plan::decode(&bytes).expect("canonical bytes must decode");
        assert_eq!(decoded, plan);
        assert_eq!(
            decoded.encode(),
            bytes,
            "decode 只接受 encode 的输出（canonical form）"
        );
    }
}

#[test]
fn duplicate_section_kind_is_rejected() {
    let mut bytes = content_routing_plan().encode();
    // 第 1 项 routing(5) 改成 content(1)，与第 0 项重复。
    set_entry_kind(&mut bytes, 1, 1);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::DuplicateSection { kind: 1 })
    );
}

#[test]
fn unsorted_section_table_is_rejected() {
    let mut bytes = content_routing_plan().encode();
    // 表项 kind 变成 5, 1（倒序），payload 布局保持连续。
    set_entry_kind(&mut bytes, 0, 5);
    set_entry_kind(&mut bytes, 1, 1);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::SectionOrder {
            previous: 5,
            current: 1,
        })
    );
}

#[test]
fn overlapping_payload_offsets_are_rejected() {
    let mut bytes = content_routing_plan().encode();
    // 第 1 项偏移指回第 0 项 payload（区间重叠 / 被包含）。
    set_entry_offset(&mut bytes, 1, 32);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::SectionLayout {
            kind: 5,
            expected_offset: 54,
            actual_offset: 32,
        })
    );
}

#[test]
fn payload_starting_inside_previous_payload_is_rejected() {
    let mut bytes = content_routing_plan().encode();
    // 指到 content payload 内部（40），既不连续也不越界。
    set_entry_offset(&mut bytes, 1, 40);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::SectionLayout {
            kind: 5,
            expected_offset: 54,
            actual_offset: 40,
        })
    );
}

#[test]
fn first_payload_must_start_at_header_end() {
    let mut bytes = content_only_plan().encode();
    // 第一个 payload 前留 2 字节间隙：规范形式要求 offset == 32。
    set_entry_offset(&mut bytes, 0, 34);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::SectionLayout {
            kind: 1,
            expected_offset: 32,
            actual_offset: 34,
        })
    );
}

#[test]
fn gap_before_section_table_is_rejected() {
    let mut bytes = content_only_plan().encode();
    // content payload 实际 22 字节，声明成 20：表前留 2 字节间隙。
    set_entry_bytes(&mut bytes, 0, 20);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::SectionTableOffset {
            expected: 52,
            actual: 54,
        })
    );
}

#[test]
fn payload_running_into_section_table_is_rejected() {
    let mut bytes = content_only_plan().encode();
    // content payload 声明成 40 字节：越过表起点（54）。
    set_entry_bytes(&mut bytes, 0, 40);
    reseal(&mut bytes);
    assert_eq!(
        Plan::decode(&bytes),
        Err(PlanError::SectionTableOffset {
            expected: 72,
            actual: 54,
        })
    );
}

#[test]
fn unknown_section_kind_is_still_rejected() {
    // 既有行为回归：未知 kind 报 UnknownSection（先于顺序/布局检查）。
    let mut bytes = content_routing_plan().encode();
    set_entry_kind(&mut bytes, 0, 9);
    reseal(&mut bytes);
    assert_eq!(Plan::decode(&bytes), Err(PlanError::UnknownSection(9)));
}

#[test]
fn canonical_form_does_not_change_semantics() {
    // 规范文件里 header/表/trailer 全部字段都参与内容寻址：改任一字节都会破坏 CRC 或 graph_id，
    // 因此不存在「同一语义两串字节」。
    let bytes = content_routing_plan().encode();
    assert_eq!(table_offset(&bytes), 66);
    assert_eq!(
        bytes.len(),
        HEADER_BYTES + 22 + 12 + (2 + 2 * TABLE_ENTRY_BYTES) + TRAILER_BYTES
    );

    let mut mutated = bytes.clone();
    mutated[30] ^= 0x01; // header total_bytes 内
    assert_ne!(
        Plan::decode(&mutated),
        Plan::decode(&bytes),
        "改动 header 后不可能仍是同一份可接受文件"
    );
}

#[test]
fn malicious_matrix_dimensions_are_rejected_without_panic() {
    // decode 不得在恶意 payload 上 panic：matrix 的 rows * cols * taps 走 checked 乘法
    // （64 位 usize 上该乘积上界 < 2^64 不会溢出，32 位会，因此期望 BadPayload 家族而不是 panic）。
    let plan = Plan {
        block_size: 128,
        sample_rate: 48_000,
        matrix: Some(MatrixSection {
            rows: 1,
            cols: 1,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: vec![1.0],
        }),
        ..Plan::default()
    };
    let mut bytes = plan.encode();
    let (payload, size) = entry_payload_range(&bytes, 0);
    assert_eq!(size, 18, "matrix payload = 10 字节头 + 1 个 f32");
    // matrix payload 布局：rows(2) cols(2) decimation(1) interpolation(1) taps(4) len(4) coeffs。
    bytes[payload..payload + 2].copy_from_slice(&u16::MAX.to_le_bytes());
    bytes[payload + 2..payload + 4].copy_from_slice(&u16::MAX.to_le_bytes());
    bytes[payload + 6..payload + 10].copy_from_slice(&u32::MAX.to_le_bytes());
    fix_entry_crc(&mut bytes, 0);
    reseal(&mut bytes);
    let error = Plan::decode(&bytes).expect_err("恶意矩阵维度必须被拒绝，而不是 panic");
    assert!(
        matches!(&error, PlanError::BadPayload(message) if message.contains("matrix coefficient count")),
        "unexpected error: {error:?}"
    );
}
