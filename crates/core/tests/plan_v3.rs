//! 方案文件 ABI v3 的往返、校验与确定性测试。

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    PlanError, ProtectionEntry, ProtectionSection, RoutingSection,
};

fn sample_plan() -> Plan {
    Plan {
        block_size: 256,
        sample_rate: 96_000,
        content: Some(ContentSection {
            in_channels: 8,
            out_lanes: 4,
            folds: vec![
                Fold {
                    src: 0,
                    dst: 0,
                    gain: 1.0,
                },
                Fold {
                    src: 1,
                    dst: 1,
                    gain: 1.0,
                },
                Fold {
                    src: 2,
                    dst: 0,
                    gain: 0.5,
                },
            ],
        }),
        crossover: Some(CrossoverSection {
            lanes: 4,
            filters: vec![
                CrossoverFilter {
                    lane: 0,
                    coeffs: vec![0.5, 0.25, -0.125],
                },
                CrossoverFilter {
                    lane: 3,
                    coeffs: vec![1.0],
                },
            ],
        }),
        matrix: Some(MatrixSection {
            rows: 4,
            cols: 2,
            decimation: 8,
            interpolation: 32,
            taps: 3,
            coeffs: (0..24).map(|index| index as f32 / 10.0).collect(),
        }),
        protection: Some(ProtectionSection {
            lanes: 4,
            entries: vec![
                ProtectionEntry {
                    lane: 0,
                    delay: 12,
                    polarity: 1,
                    gain_db: -1.5,
                    peq: vec![PeqSection {
                        b0: 1.0,
                        b1: -0.5,
                        b2: 0.25,
                        a1: -0.75,
                        a2: 0.125,
                    }],
                },
                ProtectionEntry {
                    lane: 2,
                    delay: 0,
                    polarity: -1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                },
            ],
        }),
        routing: Some(RoutingSection {
            dac_channels: 8,
            routes: vec![(0, 2), (1, 3), (2, 4), (3, 5)],
        }),
    }
}

#[test]
fn roundtrip_preserves_every_field() {
    let original = sample_plan();
    let bytes = original.encode();
    let decoded = Plan::decode(&bytes).expect("decode");
    assert_eq!(decoded, original);
    assert_eq!(
        decoded.encode(),
        bytes,
        "encoded bytes must be deterministic"
    );
}

#[test]
fn header_fields_are_where_the_contract_says() {
    let bytes = sample_plan().encode();
    assert_eq!(&bytes[0..8], b"KHAENPLN");
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 3);
    assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 5);
    assert_eq!(
        u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        256
    );
    assert_eq!(
        u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
        96_000
    );
    assert_eq!(
        u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]) as usize,
        bytes.len()
    );
}

#[test]
fn subset_of_sections_is_allowed() {
    let plan = Plan {
        block_size: 128,
        sample_rate: 48_000,
        content: None,
        crossover: None,
        matrix: None,
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: 2,
            routes: vec![(0, 0)],
        }),
    };
    let decoded = Plan::decode(&plan.encode()).expect("decode");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.stage_count(), 1);
}

#[test]
fn empty_plan_roundtrips() {
    let plan = Plan {
        block_size: 64,
        sample_rate: 44_100,
        ..Plan::default()
    };
    let bytes = plan.encode();
    assert_eq!(Plan::decode(&bytes).expect("decode"), plan);
}

#[test]
fn corrupted_body_is_rejected() {
    let mut bytes = sample_plan().encode();
    let index = bytes.len() / 2;
    bytes[index] ^= 0x01;
    assert!(matches!(
        Plan::decode(&bytes),
        Err(PlanError::CrcMismatch) | Err(PlanError::SectionCrc { .. })
    ));
}

#[test]
fn corrupted_trailer_crc_is_rejected() {
    let mut bytes = sample_plan().encode();
    let len = bytes.len();
    bytes[len - 36] ^= 0xFF;
    assert_eq!(Plan::decode(&bytes), Err(PlanError::CrcMismatch));
}

#[test]
fn forged_graph_id_is_rejected() {
    let mut bytes = sample_plan().encode();
    let len = bytes.len();
    let body_end = len - 36;
    // 只改 graph_id 的最后一个字节，并重算整体 CRC，使仅 graph_id 不符。
    bytes[len - 1] ^= 0xFF;
    let crc = khaen_core::crc32c::crc32c(&bytes[..body_end]);
    bytes[body_end..body_end + 4].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(Plan::decode(&bytes), Err(PlanError::GraphIdMismatch));
}

#[test]
fn bad_magic_and_abi_are_rejected() {
    let mut bytes = sample_plan().encode();
    bytes[0] = b'X';
    assert_eq!(Plan::decode(&bytes), Err(PlanError::BadMagic));

    let mut bytes = sample_plan().encode();
    bytes[8] = 9;
    assert_eq!(Plan::decode(&bytes), Err(PlanError::UnsupportedAbi(9)));
}

#[test]
fn truncated_plan_is_rejected() {
    let bytes = sample_plan().encode();
    assert_eq!(Plan::decode(&bytes[..16]), Err(PlanError::Truncated));
    assert!(Plan::decode(&bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn graph_id_is_content_addressed() {
    let plan = sample_plan();
    let first = plan.graph_id_hex();
    let second = Plan::decode(&plan.encode()).expect("decode").graph_id_hex();
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);

    let mut other = sample_plan();
    if let Some(matrix) = &mut other.matrix {
        matrix.coeffs[7] += 1.0e-6;
    }
    assert_ne!(first, other.graph_id_hex());
}

#[test]
fn graph_id_hex_is_trailer_digest_not_a_hash_of_it() {
    // CONTRACTS.md A.3：graph_id = sha256("khaen-plan/v3" || body)，trailer 直接存这 32 字节。
    // 对外十六进制 = 这 32 字节的编码；如果把它再散列一次（双重散列），外部按契约重算必然 mismatch。
    let plan = sample_plan();
    let bytes = plan.encode();
    let body_end = bytes.len() - 36;
    let trailer_digest = &bytes[body_end + 4..body_end + 36];

    let mut graph_input = Vec::new();
    graph_input.extend_from_slice(b"khaen-plan/v3");
    graph_input.extend_from_slice(&bytes[..body_end]);
    let expected = khaen_core::sha256::sha256(&graph_input);

    assert_eq!(trailer_digest, expected.as_slice());
    assert_eq!(
        plan.graph_id_hex(),
        khaen_core::sha256::hex_lower(&expected)
    );
    // 双重散列值必须**不**等于对外 graph_id（回归锁）。
    assert_ne!(
        plan.graph_id_hex(),
        khaen_core::sha256::sha256_hex(&expected)
    );
}

#[test]
fn graph_id_ignores_section_insertion_order_in_memory() {
    let mut plan = sample_plan();
    let expected = plan.graph_id_hex();
    plan.content = None;
    plan.content = Some(ContentSection {
        in_channels: 8,
        out_lanes: 4,
        folds: vec![
            Fold {
                src: 0,
                dst: 0,
                gain: 1.0,
            },
            Fold {
                src: 1,
                dst: 1,
                gain: 1.0,
            },
            Fold {
                src: 2,
                dst: 0,
                gain: 0.5,
            },
        ],
    });
    assert_eq!(plan.graph_id_hex(), expected);
}

#[test]
fn nan_coefficient_is_preserved_bit_exactly() {
    let plan = Plan {
        block_size: 128,
        sample_rate: 48_000,
        crossover: Some(CrossoverSection {
            lanes: 1,
            filters: vec![CrossoverFilter {
                lane: 0,
                coeffs: vec![f32::NAN, f32::INFINITY, -0.0],
            }],
        }),
        ..Plan::default()
    };
    let decoded = Plan::decode(&plan.encode()).expect("decode");
    let coeffs = &decoded.crossover.unwrap().filters[0].coeffs;
    assert!(coeffs[0].is_nan());
    assert!(coeffs[1].is_infinite());
    assert_eq!(coeffs[2].to_bits(), (-0.0f32).to_bits());
}
