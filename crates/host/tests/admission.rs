//! 准入测试：预算估算、超限 fail-closed、内部不自洽拒绝。

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    ProtectionEntry, ProtectionSection, RoutingSection,
};
use khaen_host::admission::{admit, AdmissionLimits};

fn base_plan() -> Plan {
    Plan {
        block_size: 128,
        sample_rate: 96_000,
        content: Some(ContentSection {
            in_channels: 2,
            out_lanes: 2,
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
            ],
        }),
        crossover: Some(CrossoverSection {
            lanes: 2,
            filters: vec![
                CrossoverFilter {
                    lane: 0,
                    coeffs: vec![0.5, 0.5],
                },
                CrossoverFilter {
                    lane: 1,
                    coeffs: vec![0.5, 0.5],
                },
            ],
        }),
        matrix: Some(MatrixSection {
            rows: 2,
            cols: 2,
            decimation: 8,
            interpolation: 32,
            taps: 4,
            coeffs: vec![1.0; 16],
        }),
        protection: Some(ProtectionSection {
            lanes: 2,
            entries: vec![ProtectionEntry {
                lane: 0,
                delay: 12,
                polarity: 1,
                gain_db: 0.0,
                peq: vec![PeqSection {
                    b0: 1.0,
                    b1: 0.0,
                    b2: 0.0,
                    a1: 0.0,
                    a2: 0.0,
                }],
            }],
        }),
        routing: Some(RoutingSection {
            dac_channels: 2,
            routes: vec![(0, 0), (1, 1)],
        }),
    }
}

fn codes(rejections: &[khaen_host::admission::Rejection]) -> Vec<&'static str> {
    rejections.iter().map(|rejection| rejection.code).collect()
}

#[test]
fn reasonable_plan_is_admitted_with_budget() {
    let admission = admit(&base_plan(), &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());
    assert!(admission.rejections.is_empty());

    let budget = &admission.budget;
    assert_eq!(budget.matrix_cells, 4);
    assert_eq!(budget.matrix_taps_per_cell, 4);
    // 分频 4 + 矩阵 16 + PEQ 5 = 25
    assert_eq!(budget.coefficients, 25);
    assert_eq!(budget.lanes, 2);
    assert_eq!(budget.dac_channels, 2);
    // 分频 GD 0 + 矩阵 (4/2)*8 = 16 + 保护延迟 12
    assert_eq!(budget.latency_samples, 28);
    assert!(budget.state_samples > 0);
    assert!(budget.memory_bytes > budget.plan_bytes);
}

#[test]
fn coefficient_budget_is_enforced() {
    let limits = AdmissionLimits {
        max_coefficients: 10,
        ..AdmissionLimits::default()
    };
    let admission = admit(&base_plan(), &limits);
    assert!(!admission.accepted);
    assert!(codes(&admission.rejections).contains(&"coefficients"));
}

#[test]
fn latency_budget_is_enforced() {
    let limits = AdmissionLimits {
        max_latency_samples: 8,
        ..AdmissionLimits::default()
    };
    let admission = admit(&base_plan(), &limits);
    assert!(!admission.accepted);
    let latency = admission
        .rejections
        .iter()
        .find(|rejection| rejection.code == "latency")
        .expect("latency rejection");
    assert!(latency.detail.contains("28"), "{}", latency.detail);
}

#[test]
fn plan_bytes_budget_is_enforced() {
    let limits = AdmissionLimits {
        max_plan_bytes: 16,
        ..AdmissionLimits::default()
    };
    let admission = admit(&base_plan(), &limits);
    assert!(!admission.accepted);
    assert!(codes(&admission.rejections).contains(&"plan_bytes"));
}

#[test]
fn missing_matrix_is_rejected() {
    let mut plan = base_plan();
    plan.matrix = None;
    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(!admission.accepted);
    assert!(codes(&admission.rejections).contains(&"missing_matrix"));
}

#[test]
fn inconsistent_matrix_is_rejected() {
    let mut plan = base_plan();
    if let Some(matrix) = &mut plan.matrix {
        matrix.coeffs.pop();
    }
    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(!admission.accepted);
    let rejection = admission
        .rejections
        .iter()
        .find(|rejection| rejection.code == "matrix_coefficients")
        .expect("matrix coefficient rejection");
    assert!(rejection.detail.contains("15"));
}

#[test]
fn unsupported_sample_rate_is_rejected() {
    let mut plan = base_plan();
    plan.sample_rate = 44_100;
    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(!admission.accepted);
    let rejection = admission
        .rejections
        .iter()
        .find(|rejection| rejection.code == "sample_rate")
        .expect("sample_rate rejection");
    assert!(rejection.detail.contains("48000"), "{}", rejection.detail);
    assert!(rejection.detail.contains("44100"), "{}", rejection.detail);
}

#[test]
fn over_abi_limit_plan_is_rejected_not_panicked() {
    // 手工构造 65536 个 fold（ABI 上限 65535）：准入必须给 plan_encoding rejection，
    // 而不是触发 panic 版 encode（decode/plan-builder 产物不会到这一步）。
    let mut plan = base_plan();
    let folds: Vec<Fold> = (0..65_536u32)
        .map(|index| Fold {
            src: (index % 2) as u16,
            dst: (index % 2) as u16,
            gain: 1.0,
        })
        .collect();
    if let Some(content) = &mut plan.content {
        content.folds = folds;
    }
    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(!admission.accepted);
    let rejection = admission
        .rejections
        .iter()
        .find(|rejection| rejection.code == "plan_encoding")
        .expect("plan_encoding rejection");
    assert!(rejection.detail.contains("65535"), "{}", rejection.detail);
}

#[test]
fn out_of_range_references_are_rejected() {
    let mut plan = base_plan();
    plan.content = Some(ContentSection {
        in_channels: 2,
        out_lanes: 2,
        folds: vec![Fold {
            src: 5,
            dst: 0,
            gain: 1.0,
        }],
    });
    if let Some(routing) = &mut plan.routing {
        routing.routes.push((3, 0));
        routing.routes.push((0, 9));
    }
    let admission = admit(&plan, &AdmissionLimits::default());
    let found = codes(&admission.rejections);
    assert!(found.contains(&"content_fold_range"), "{found:?}");
    assert!(found.contains(&"routing_lane_range"), "{found:?}");
    assert!(found.contains(&"routing_dac_range"), "{found:?}");
}

#[test]
fn lane_and_dac_limits_are_enforced() {
    let limits = AdmissionLimits {
        max_lanes: 1,
        max_dac_channels: 1,
        ..AdmissionLimits::default()
    };
    let admission = admit(&base_plan(), &limits);
    let found = codes(&admission.rejections);
    assert!(found.contains(&"lanes"), "{found:?}");
    assert!(found.contains(&"dac_channels"), "{found:?}");
}

#[test]
fn zero_block_size_and_bad_polarity_are_rejected() {
    let mut plan = base_plan();
    plan.block_size = 0;
    if let Some(protection) = &mut plan.protection {
        protection.entries[0].polarity = 0;
    }
    let admission = admit(&plan, &AdmissionLimits::default());
    let found = codes(&admission.rejections);
    assert!(found.contains(&"block_size"), "{found:?}");
    assert!(found.contains(&"protection_polarity"), "{found:?}");
}
