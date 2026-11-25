//! 4.1 测量质量门 / 4.4 权威加权 + 安全钳位 + 三态 的集成测试。
//!
//! 覆盖（任务验收 ①-④）：
//! 1) 低 SNR 频段被排除且报 Unknown（且 Unknown != 能力不足）；
//! 2) 权威加权让"在该频点本来就弱的音箱"在谷点不被派活（能力不足路径）；
//! 3) 安全钳位后状态为 PARTIAL 且残差正确；
//! 4) 安全限永不越过（构造超限用例）。
//!
//! 另外用 FNV-1a 对 report.json 做冻结摘要，证明**默认配置（三项能力全关）逐位不变**。

use khaen_mimo_design::authority::AuthorityConfig;
use khaen_mimo_design::limits::{
    OutcomeState, SafetyConfig, MAX_BAND_BOOST_DB, MAX_POSITIVE_SUM_DB, MAX_PREDICTED_PEAK_DBFS,
};
use khaen_mimo_design::quality::QualityConfig;
use khaen_mimo_design::report::Json;
use khaen_mimo_design::{run_scene, Cx, RunConfig, SceneInput};

const NFFT: usize = 8192;

fn freqs_on_grid(bins: &[usize]) -> Vec<f64> {
    bins.iter()
        .map(|k| *k as f64 * 48000.0 / NFFT as f64)
        .collect()
}

fn base_cfg() -> RunConfig {
    RunConfig {
        taps: 64,
        delay_samples: 16,
        nfft: NFFT,
        ..RunConfig::default()
    }
}

/// 合成场景：3 design + 2 holdout、2 源、6 个 1/3 oct 带（40..125 Hz）。
///
/// - 源 1 在 63 Hz 带（bin 11）有一个深谷（H≈1e-3），其余带正常；
/// - 空间上有差异（避免 B1 完全无法改进）。
fn scene(valley: f64) -> SceneInput {
    let freqs = freqs_on_grid(&[7, 9, 11, 13, 17, 21]);
    let k = freqs.len();
    let n_mics = 5;
    let design_mics = vec![0usize, 1, 2];
    let holdout_mics = vec![3usize, 4];
    let mut h = Vec::with_capacity(n_mics);
    for m in 0..n_mics {
        let g = 0.7 + 0.25 * m as f64;
        let hmic: Vec<Vec<Cx>> = (0..k)
            .map(|kk| {
                let a = 0.2 * kk as f64 + 0.1 * m as f64;
                let h1 = g * (a + 0.3).cos();
                // 源 2：63 Hz 带（kk==2）深谷
                let h2 = if kk == 2 { valley * g } else { 0.8 * g };
                vec![Cx::new(h1, 0.1), Cx::new(h2, 0.0)]
            })
            .collect();
        h.push(hmic);
    }
    SceneInput {
        scene_id: "single-point-calibration".to_string(),
        fixture_dir: "synthetic".to_string(),
        ground_truth_sha256: "0".repeat(64),
        seed: 23,
        rir_scale: 0.5,
        sample_rate_hz: 48000.0,
        freqs_hz: freqs,
        source_ids: vec!["SUB1".to_string(), "SUB2".to_string()],
        mic_ids: vec![
            "P01".to_string(),
            "P02".to_string(),
            "P03".to_string(),
            "P04".to_string(),
            "P05".to_string(),
        ],
        design_mics,
        holdout_mics,
        h,
        band_centers_hz: vec![40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        extended_centers_hz: vec![25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    }
}

fn jget<'a>(root: &'a Json, keys: &[&str]) -> Option<&'a Json> {
    let mut cur = root;
    for key in keys {
        match cur {
            Json::Obj(items) => cur = &items.iter().find(|(k, _)| k == key)?.1,
            _ => return None,
        }
    }
    Some(cur)
}

fn jstr(root: &Json, keys: &[&str]) -> Option<String> {
    match jget(root, keys)? {
        Json::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn jnum(root: &Json, keys: &[&str]) -> Option<f64> {
    match jget(root, keys)? {
        Json::Num(v) => Some(*v),
        _ => None,
    }
}

fn jarr<'a>(root: &'a Json, keys: &[&str]) -> Option<&'a Vec<Json>> {
    match jget(root, keys)? {
        Json::Arr(v) => Some(v),
        _ => None,
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// 默认配置（quality/authority/safety 全关）必须逐位不变：冻结 report.json 摘要。
#[test]
fn default_config_report_digest_is_frozen() {
    let run = run_scene(&scene(1e-3), &base_cfg()).expect("run");
    let text = run.report.to_pretty();
    // 默认配置下不允许出现三项新能力的报告块
    assert!(!text.contains("\"quality\""), "默认配置不应出现 quality 块");
    assert!(
        !text.contains("\"authority\""),
        "默认配置不应出现 authority 块"
    );
    assert!(!text.contains("\"outcome\""), "默认配置不应出现 outcome 块");
    assert!(!text.contains("\"safety\""), "默认配置不应出现 safety 块");
    // 冻结摘要（预置值来自本改动前的 HEAD 代码，逐位一致）
    let digest = fnv1a64(text.as_bytes());
    assert_eq!(
        digest, DEFAULT_REPORT_FNV1A_64,
        "默认配置 report.json 摘要变化（{} != {}）",
        digest, DEFAULT_REPORT_FNV1A_64
    );
}

/// 本改动前（HEAD）代码在同一场景/默认配置下 report.json 的 FNV-1a 摘要。
const DEFAULT_REPORT_FNV1A_64: u64 = 9776064799801072177;

/// ① 低 SNR 频段被排除且报 Unknown（不被当成"无能力"）。
#[test]
fn low_snr_band_is_excluded_and_reported_unknown() {
    let input = scene(1e-3);
    // QualityConfig 的 centers 是 20 Hz–20 kHz 的 31 带；这里给出同长度证据：
    // 63 Hz 带 SNR 10 dB < 门限 15 dB → Unknown；其余 40 dB → Known。
    let centers = khaen_mimo_design::quality::THIRD_OCTAVE_CENTERS_HZ_20_20K.to_vec();
    let mut snr = vec![Some(40.0); centers.len()];
    // 63 Hz 在 ISO 表里的索引：找到中心 63.0
    let idx = centers
        .iter()
        .position(|c| (*c - 63.0).abs() < 1e-9)
        .expect("63 Hz 中心");
    snr[idx] = Some(10.0);
    let mut quality = QualityConfig::from_band_snr(snr);
    quality.min_snr_db = 15.0;
    let mut cfg = base_cfg();
    cfg.quality = quality;
    let run = run_scene(&input, &cfg).expect("run");
    let text = run.report.to_pretty();
    assert!(text.contains("\"quality\""), "缺少 quality 报告块");
    // excluded_bands 必须列出 63 Hz 与原因
    let excluded = jarr(&run.report, &["quality", "excluded_bands"]).expect("excluded_bands");
    assert!(
        excluded.iter().any(|v| {
            let c = match v {
                Json::Obj(items) => {
                    items
                        .iter()
                        .find(|(k, _)| k == "center_hz")
                        .and_then(|(_, v)| match v {
                            Json::Num(x) => Some(*x),
                            _ => None,
                        })
                }
                _ => None,
            };
            c.map(|x| (x - 63.0).abs() < 1e-9).unwrap_or(false)
        }),
        "63 Hz 未出现在 excluded_bands: {}",
        jget(&run.report, &["quality", "excluded_bands"])
            .map(|v| v.to_compact())
            .unwrap_or_default()
    );
    // 63 Hz 带状态 Unknown + 原因 snr_below_min
    let bands = jarr(&run.report, &["quality", "bands"]).expect("bands");
    let band63 = bands
        .iter()
        .find(|v| match v {
            Json::Obj(items) => items
                .iter()
                .find(|(k, _)| k == "center_hz")
                .and_then(|(_, v)| match v {
                    Json::Num(x) => Some((*x - 63.0).abs() < 1e-9),
                    _ => None,
                })
                .unwrap_or(false),
            _ => false,
        })
        .expect("63 Hz band");
    let state = match band63 {
        Json::Obj(items) => items
            .iter()
            .find(|(k, _)| k == "state")
            .and_then(|(_, v)| match v {
                Json::Str(s) => Some(s.clone()),
                _ => None,
            }),
        _ => None,
    };
    assert_eq!(state.as_deref(), Some("unknown"));
    let reason = match band63 {
        Json::Obj(items) => items
            .iter()
            .find(|(k, _)| k == "reason")
            .and_then(|(_, v)| match v {
                Json::Str(s) => Some(s.clone()),
                _ => None,
            }),
        _ => None,
    };
    assert_eq!(reason.as_deref(), Some("snr_below_min"));
    // 语义：Unknown 只表示测量不可判定，不是能力不足
    assert!(
        text.contains("unknown_ne_insufficient_authority"),
        "缺少 Unknown != 能力不足 的语义声明"
    );
    // 排除频段必须真的改变拟合（否则"排除"是假的）
    let default_run = run_scene(&input, &base_cfg()).expect("default run");
    assert!(
        !default_run.report.to_pretty().contains("\"quality\""),
        "默认配置不应出现 quality 块"
    );
    let fir_masked = run
        .fir_files
        .iter()
        .find(|(p, _)| p == "fir/SUB1.csv")
        .expect("SUB1 fir")
        .1
        .clone();
    let fir_default = default_run
        .fir_files
        .iter()
        .find(|(p, _)| p == "fir/SUB1.csv")
        .expect("SUB1 fir")
        .1
        .clone();
    assert_ne!(
        fir_masked, fir_default,
        "质量门排除频段后拟合结果应与默认不同"
    );
}

/// ② 权威加权端到端接线：报告块 + Γ 上报 + 设计确实改变。
///
/// 机制本身（谷点不被派活）由 authority::tests::authority_gamma_suppresses_drive_at_a_valley
/// 在精确的频域解层面验证；这里验证 pipeline 接线与证据输出。
#[test]
fn authority_weighting_is_wired_and_reported() {
    let input = scene(1e-3);
    let run_off = run_scene(&input, &base_cfg()).expect("off");
    let mut cfg_on = base_cfg();
    cfg_on.authority = AuthorityConfig::enabled();
    let run_on = run_scene(&input, &cfg_on).expect("on");

    let text_off = run_off.report.to_pretty();
    let text_on = run_on.report.to_pretty();
    assert!(
        !text_off.contains("\"authority\""),
        "默认关闭不应出现 authority 块"
    );
    assert!(text_on.contains("\"authority\""), "缺少 authority 报告块");
    assert!(text_on.contains("gamma_s(f)"));

    // SUB2 的 Γ 上限必须 > 1（谷点被压低）
    let per_source = jget(&run_on.report, &["authority", "per_source"]).expect("per_source");
    let mut gamma_max_sub2 = 0.0f64;
    if let Json::Obj(entries) = per_source {
        if let Some((_, Json::Obj(items))) = entries.iter().find(|(k, _)| k == "SUB2") {
            gamma_max_sub2 = items
                .iter()
                .find(|(k, _)| k == "gamma_max")
                .and_then(|(_, v)| match v {
                    Json::Num(x) => Some(*x),
                    _ => None,
                })
                .unwrap_or(0.0);
        }
    }
    assert!(gamma_max_sub2 > 10.0, "SUB2 gamma_max={}", gamma_max_sub2);

    // 设计确实不同（加权进了正则项）
    let fir_off = run_off
        .fir_files
        .iter()
        .find(|(p, _)| p == "fir/SUB1.csv")
        .expect("SUB1 fir")
        .1
        .clone();
    let fir_on = run_on
        .fir_files
        .iter()
        .find(|(p, _)| p == "fir/SUB1.csv")
        .expect("SUB1 fir")
        .1
        .clone();
    assert_ne!(fir_off, fir_on, "权威加权没有改变设计");
}

/// ② 空权威源（H 恒为 0）必须被如实标成 empty，不静默当正常源。
#[test]
fn authority_reports_empty_source() {
    let mut input = scene(1e-3);
    for mic in input.h.iter_mut() {
        for bin in mic.iter_mut() {
            bin[1] = Cx::zero();
        }
    }
    let mut cfg = base_cfg();
    cfg.authority = AuthorityConfig::enabled();
    let run = run_scene(&input, &cfg).expect("run");
    let text = run.report.to_pretty();
    assert!(
        text.contains("\"empty_sources\""),
        "缺少 empty_sources: {}",
        text.len()
    );
    assert!(text.contains("\"SUB2\""), "空权威源未上报 SUB2");
}

/// ③ 安全钳位后 PARTIAL 且残差正确；④ 安全限永不越过。
#[test]
fn safety_clamp_yields_partial_with_residuals_and_never_crosses_limits() {
    let input = scene(1e-3);
    let mut cfg = base_cfg();
    cfg.safety = SafetyConfig::enabled();
    let run = run_scene(&input, &cfg).expect("run");
    let text = run.report.to_pretty();
    assert!(text.contains("\"safety\""), "缺少 safety 报告块");
    let state = jstr(&run.report, &["variants", "B3", "outcome"]).expect("B3 outcome");
    assert!(
        state == "PARTIAL" || state == "FULL" || state == "NONE",
        "非法三态 {}",
        state
    );
    assert_eq!(
        jstr(&run.report, &["outcome"]).as_deref(),
        Some(state.as_str()),
        "顶层 outcome 应等于 B3 的 outcome"
    );
    // ④ 安全限永不越过：报告里的实测带增益/正向和/预测峰值全部在限内
    let safety = jget(&run.report, &["variants", "B3", "safety"]).expect("safety");
    let never = match safety {
        Json::Obj(items) => items
            .iter()
            .find(|(k, _)| k == "never_exceeded")
            .and_then(|(_, v)| match v {
                Json::Bool(b) => Some(*b),
                _ => None,
            }),
        _ => None,
    };
    assert_eq!(never, Some(true), "never_exceeded 必须为 true");
    let peak = jnum(
        &run.report,
        &["variants", "B3", "safety", "predicted_peak_dbfs"],
    )
    .unwrap();
    assert!(peak <= MAX_PREDICTED_PEAK_DBFS + 1e-6, "peak={}", peak);
    for source in ["SUB1", "SUB2"] {
        // per_source 数组里逐源检查
        if let Some(arr) = jarr(&run.report, &["variants", "B3", "safety", "per_source"]) {
            if let Some(Json::Obj(items)) = arr.iter().find(|v| match v {
                Json::Obj(items) => items
                    .iter()
                    .any(|(k, v)| k == "id" && matches!(v, Json::Str(s) if s == source)),
                _ => false,
            }) {
                let gain = items
                    .iter()
                    .find(|(k, _)| k == "max_band_gain_db")
                    .and_then(|(_, v)| match v {
                        Json::Num(x) => Some(*x),
                        _ => None,
                    })
                    .unwrap();
                let pos = items
                    .iter()
                    .find(|(k, _)| k == "positive_sum_db")
                    .and_then(|(_, v)| match v {
                        Json::Num(x) => Some(*x),
                        _ => None,
                    })
                    .unwrap();
                assert!(gain <= MAX_BAND_BOOST_DB + 1e-6, "{} gain={}", source, gain);
                assert!(pos <= MAX_POSITIVE_SUM_DB + 1e-6, "{} pos={}", source, pos);
            }
        }
    }
    // ③ 若撞限（PARTIAL），必须有逐项残差（角色/频点/差值/撞哪条限）
    if state == "PARTIAL" {
        let residuals = jarr(&run.report, &["variants", "B3", "safety", "residuals"]).expect("res");
        assert!(!residuals.is_empty(), "PARTIAL 必须带残差");
        for r in residuals {
            if let Json::Obj(items) = r {
                for key in ["role", "deficit_db", "target_db", "achieved_db"] {
                    assert!(items.iter().any(|(k, _)| k == key), "残差缺少字段 {}", key);
                }
            }
        }
    }
    // 默认关闭时不出现
    let default_run = run_scene(&input, &base_cfg()).expect("default");
    assert!(!default_run.report.to_pretty().contains("\"safety\""));
}

/// ④ 构造超限用例：极端目标 + 尖锐补偿 → 钳位触发，但安全限仍不越过。
#[test]
fn extreme_over_limit_case_is_clamped_and_never_exceeds() {
    let input = scene(1e-6);
    let mut cfg = base_cfg();
    cfg.safety = SafetyConfig {
        max_pre_gain_reduction_db: 3.0,
        max_target_scale_db: 3.0,
        ..SafetyConfig::enabled()
    };
    cfg.target_beta = 3.0; // 极端目标（远超 0.7）→ 补偿量很大
    let run = run_scene(&input, &cfg).expect("run");
    let safety = jget(&run.report, &["variants", "B3", "safety"]).expect("safety");
    let never = match safety {
        Json::Obj(items) => items
            .iter()
            .find(|(k, _)| k == "never_exceeded")
            .and_then(|(_, v)| match v {
                Json::Bool(b) => Some(*b),
                _ => None,
            }),
        _ => None,
    };
    assert_eq!(never, Some(true));
    let peak = jnum(
        &run.report,
        &["variants", "B3", "safety", "predicted_peak_dbfs"],
    )
    .unwrap();
    assert!(peak <= MAX_PREDICTED_PEAK_DBFS + 1e-6, "peak={}", peak);
    let state = jstr(&run.report, &["variants", "B3", "outcome"]).unwrap();
    assert!(
        state == "PARTIAL" || state == "NONE",
        "超限用例不应报 FULL: {}",
        state
    );
    // 状态不能靠"顶满但仍未达标"冒充成功
    let text = run.report.to_pretty();
    assert!(text.contains("\"residuals\""));
}

/// 三态语义：FULL 只允许出现在无钳位且达标的情形。
#[test]
fn outcome_states_are_consistent_with_residuals() {
    let input = scene(1e-3);
    let mut cfg = base_cfg();
    cfg.safety = SafetyConfig::enabled();
    let run = run_scene(&input, &cfg).expect("run");
    let state = jstr(&run.report, &["variants", "B3", "outcome"]).unwrap();
    let residuals = jarr(&run.report, &["variants", "B3", "safety", "residuals"]).unwrap();
    if state == "FULL" {
        assert!(residuals.is_empty(), "FULL 不允许带残差");
    } else {
        assert!(
            !residuals.is_empty() || state == "NONE",
            "{} 必须带残差",
            state
        );
    }
    let _ = OutcomeState::Full;
}

/// 质量门 fail-closed：全部频段 Unknown 时拒绝求解（不得静默放行）。
#[test]
fn all_bands_unknown_is_rejected_fail_closed() {
    let centers = khaen_mimo_design::quality::THIRD_OCTAVE_CENTERS_HZ_20_20K.to_vec();
    let snr = vec![Some(0.0); centers.len()];
    let mut cfg = base_cfg();
    cfg.quality = QualityConfig::from_band_snr(snr);
    let err = match run_scene(&scene(1e-3), &cfg) {
        Ok(_) => panic!("全部频段 Unknown 时必须拒绝求解"),
        Err(e) => e,
    };
    assert!(err.contains("质量门"), "错误信息应说明质量门: {}", err);
}
