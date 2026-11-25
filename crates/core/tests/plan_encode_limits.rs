//! Plan::encode 的 fail-closed 边界测试（T10c）。
//!
//! 覆盖两件事：
//! 1. 合法方案 `encode_checked()` 成功，与 `encode()` 字节一致，且 decode 回原方案；
//! 2. 每个 ABI v3 字段域超限路径都返回稳定错误码，而不是静默截断 / panic。
//!
//! 总长超过 u32（`PlanError::EncodedTooLarge`）无法用真实 Plan 构造（需要 > 4 GiB 内存），
//! 该守卫在 `crates/core/src/plan.rs` 的单元测试里直接断言。

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    PlanError, ProtectionEntry, ProtectionSection, RoutingSection, MAX_CROSSOVER_TAPS,
    MAX_MATRIX_COEFFICIENTS, MAX_SECTION_ENTRIES,
};

fn base_plan() -> Plan {
    Plan {
        block_size: 128,
        sample_rate: 48_000,
        ..Plan::default()
    }
}

fn full_plan() -> Plan {
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

fn peq() -> PeqSection {
    PeqSection {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

fn entry() -> ProtectionEntry {
    ProtectionEntry {
        lane: 0,
        delay: 0,
        polarity: 1,
        gain_db: 0.0,
        peq: Vec::new(),
    }
}

#[test]
fn valid_plan_encodes_through_checked_path() {
    let plan = full_plan();
    let checked = plan.encode_checked().expect("合法方案必须能编码");
    assert_eq!(
        checked,
        plan.encode(),
        "encode() 必须是 encode_checked() 的便捷包装（字节一致）"
    );
    assert_eq!(Plan::decode(&checked).expect("decode"), plan);
}

#[test]
fn fold_count_at_the_u16_limit_is_accepted() {
    // 恰好 65535 条：字段域上限内，encode 必须成功且 decode 回来还是 65535 条。
    let folds = vec![
        Fold {
            src: 0,
            dst: 0,
            gain: 1.0,
        };
        MAX_SECTION_ENTRIES
    ];
    let plan = Plan {
        content: Some(ContentSection {
            in_channels: 1,
            out_lanes: 1,
            folds,
        }),
        ..base_plan()
    };
    let bytes = plan.encode_checked().expect("65535 条折叠必须在 u16 域内");
    let decoded = Plan::decode(&bytes).expect("decode");
    assert_eq!(
        decoded.content.expect("content").folds.len(),
        MAX_SECTION_ENTRIES
    );
}

#[test]
fn too_many_folds_is_rejected() {
    let plan = Plan {
        content: Some(ContentSection {
            in_channels: 1,
            out_lanes: 1,
            folds: vec![
                Fold {
                    src: 0,
                    dst: 0,
                    gain: 1.0,
                };
                MAX_SECTION_ENTRIES + 1
            ],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::TooManyFolds {
            count: MAX_SECTION_ENTRIES + 1,
            limit: MAX_SECTION_ENTRIES,
        })
    );
}

#[test]
fn too_many_crossover_filters_is_rejected() {
    let plan = Plan {
        crossover: Some(CrossoverSection {
            lanes: 1,
            filters: vec![
                CrossoverFilter {
                    lane: 0,
                    coeffs: vec![1.0],
                };
                MAX_SECTION_ENTRIES + 1
            ],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::TooManyCrossoverFilters {
            count: MAX_SECTION_ENTRIES + 1,
            limit: MAX_SECTION_ENTRIES,
        })
    );
}

#[test]
fn crossover_taps_beyond_decoder_limit_are_rejected() {
    // 抽头数超过解码器防护（1 << 20）的方案 encode 得出来、decode 不回去，必须拒绝。
    let taps = usize::try_from(MAX_CROSSOVER_TAPS).expect("u32 taps fits usize") + 1;
    let plan = Plan {
        crossover: Some(CrossoverSection {
            lanes: 1,
            filters: vec![CrossoverFilter {
                lane: 0,
                coeffs: vec![0.0; taps],
            }],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::CrossoverTapsTooLong {
            lane: 0,
            taps,
            limit: MAX_CROSSOVER_TAPS,
        })
    );
}

#[test]
fn matrix_coefficients_beyond_decoder_limit_are_rejected() {
    let count = usize::try_from(MAX_MATRIX_COEFFICIENTS).expect("u32 count fits usize") + 1;
    let plan = Plan {
        matrix: Some(MatrixSection {
            rows: 1,
            cols: 1,
            decimation: 1,
            interpolation: 1,
            taps: u32::try_from(count).expect("count fits u32"),
            coeffs: vec![0.0; count],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::MatrixCoefficientsTooMany {
            count,
            limit: MAX_MATRIX_COEFFICIENTS,
        })
    );
}

#[test]
fn too_many_protection_entries_is_rejected() {
    let plan = Plan {
        protection: Some(ProtectionSection {
            lanes: 1,
            entries: vec![entry(); MAX_SECTION_ENTRIES + 1],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::TooManyProtectionEntries {
            count: MAX_SECTION_ENTRIES + 1,
            limit: MAX_SECTION_ENTRIES,
        })
    );
}

#[test]
fn too_many_peq_sections_is_rejected() {
    let mut single = entry();
    single.peq = vec![peq(); MAX_SECTION_ENTRIES + 1];
    let plan = Plan {
        protection: Some(ProtectionSection {
            lanes: 1,
            entries: vec![single],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::TooManyPeqSections {
            lane: 0,
            count: MAX_SECTION_ENTRIES + 1,
            limit: MAX_SECTION_ENTRIES,
        })
    );
}

#[test]
fn too_many_routes_is_rejected() {
    let plan = Plan {
        routing: Some(RoutingSection {
            dac_channels: 1,
            routes: vec![(0, 0); MAX_SECTION_ENTRIES + 1],
        }),
        ..base_plan()
    };
    assert_eq!(
        plan.encode_checked(),
        Err(PlanError::TooManyRoutes {
            count: MAX_SECTION_ENTRIES + 1,
            limit: MAX_SECTION_ENTRIES,
        })
    );
}

#[test]
fn limit_errors_all_have_display_messages() {
    let errors = [
        PlanError::TooManyFolds {
            count: 65_536,
            limit: MAX_SECTION_ENTRIES,
        },
        PlanError::TooManyCrossoverFilters {
            count: 65_536,
            limit: MAX_SECTION_ENTRIES,
        },
        PlanError::CrossoverTapsTooLong {
            lane: 3,
            taps: 1 << 21,
            limit: MAX_CROSSOVER_TAPS,
        },
        PlanError::MatrixCoefficientsTooMany {
            count: 1 << 23,
            limit: MAX_MATRIX_COEFFICIENTS,
        },
        PlanError::TooManyProtectionEntries {
            count: 65_536,
            limit: MAX_SECTION_ENTRIES,
        },
        PlanError::TooManyPeqSections {
            lane: 7,
            count: 65_536,
            limit: MAX_SECTION_ENTRIES,
        },
        PlanError::TooManyRoutes {
            count: 65_536,
            limit: MAX_SECTION_ENTRIES,
        },
        PlanError::EncodedTooLarge { bytes: 1 << 33 },
    ];
    let mut seen = Vec::new();
    for error in errors {
        let text = error.to_string();
        assert!(!text.is_empty(), "Display 不得为空: {error:?}");
        assert!(!seen.contains(&text), "每条超限错误应有独立文案: {text}");
        seen.push(text);
    }
}

#[test]
fn semantic_inconsistency_stays_outside_the_encoder() {
    // encode_checked 只守 ABI 字段域：矩阵系数个数与 rows*cols*taps 是否一致属于
    // khaen_host::admission / khaen_plan_builder 的职责。host 的准入路径会先 encode
    // 取长度再逐项报告拒绝原因，因此这里必须保持容忍（否则准入会变成 panic）。
    let mut plan = full_plan();
    plan.matrix.as_mut().expect("matrix").coeffs.pop();
    let bytes = plan
        .encode_checked()
        .expect("字段域可表示就必须编码成功（语义校验不在这里）");
    assert!(
        Plan::decode(&bytes).is_err(),
        "语义不自洽的方案字节本身 decode 不回去，这正是准入层要拒绝的"
    );
}
