// T64 目标驱动 2a 恢复单测（经 src/peq_fit.rs 的 #[cfg(test)] include! 进入单测构建）。
//
// 夹具来源（真实证据、可追溯）：
//   rt-stack/evidence/calib-chain-20261121/t64-rule2a-20261122/data/reconstructed_curves.csv
//   rt-stack/evidence/calib-chain-20261121/t64-rule2a-20261122/data/bands.csv
// 其中 measured_est_db 是从两份真实 job-0001 T64 报告（flat/tilt10）的 g_residual_curve
// 独立反演的实测曲线（互验 max 8e-15 dB）；本测试只取 1/24-oct 重建的 modal 段，
// 因此 before 数值是 3.95/2.73（producer 原始 1/48 网格为 4.08/2.90，差异来自重建分辨率）。
// 本测试验证：a) 标准拟合 FAIL；b) 目标驱动恢复在**同一门代码**下两 clause≤3 才采纳；
// c) 额外段落在残差峰附近；d) tilt10 已通过时不触发；e) 无解时 applied=false 且保留 FAIL。
use crate::measured_design::{
    build_measured_design_opts, load_measured_spec, MeasuredBuildOptions,
};

fn t64_evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evidence/calib-chain-20261121/t64-rule2a-20261122")
}

fn t64_read_csv(name: &str) -> Vec<Vec<String>> {
    let path = t64_evidence_dir().join("data").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    text.lines()
        .map(|line| {
            line.split(',')
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

fn t64_reconstructed_modal_curve() -> (Vec<f64>, Vec<f64>) {
    let rows = t64_read_csv("reconstructed_curves.csv");
    let header = &rows[0];
    let col = |name: &str| {
        header
            .iter()
            .position(|cell| cell == name)
            .expect("csv column")
    };
    let c_channel = col("channel");
    let c_freq = col("freq_hz");
    let c_est = col("measured_est_db");
    let mut freqs = Vec::new();
    let mut vals = Vec::new();
    for row in &rows[1..] {
        if row.get(c_channel).map(String::as_str) != Some("FL") {
            continue;
        }
        let freq: f64 = row[c_freq].parse().expect("freq");
        if !(80.0..=250.0).contains(&freq) {
            continue;
        }
        freqs.push(freq);
        vals.push(row[c_est].parse::<f64>().expect("measured_est_db"));
    }
    assert!(freqs.len() >= 40, "reconstructed modal curve too short");
    (freqs, vals)
}

fn t64_report_bands(report: &str) -> Vec<ModalBand> {
    let rows = t64_read_csv("bands.csv");
    let header = &rows[0];
    let col = |name: &str| {
        header
            .iter()
            .position(|cell| cell == name)
            .expect("csv column")
    };
    let (c_channel, c_report, c_freq, c_q, c_gain) = (
        col("channel"),
        col("report"),
        col("freq_hz"),
        col("q"),
        col("gain_db"),
    );
    let mut bands = Vec::new();
    for row in &rows[1..] {
        if row[c_channel] != "FL" || row[c_report] != report {
            continue;
        }
        bands.push(
            ModalBand::from_params(
                BandType::Peaking,
                row[c_freq].parse().expect("freq"),
                row[c_q].parse().expect("q"),
                row[c_gain].parse().expect("gain"),
                48_000.0,
            )
            .expect("valid reported band"),
        );
    }
    assert_eq!(
        bands.len(),
        4,
        "reconstructed flat fixture must have 4 bands"
    );
    bands
}

fn t64_context(freqs: &[f64], target_db: Vec<f64>) -> FitContext {
    let fs = 48_000u32;
    FitContext {
        fs,
        fs_f: fs as f64,
        upper_hz: 250.0,
        grid: freqs.to_vec(),
        target_db: target_db.clone(),
        target: TargetShape {
            grid: freqs.to_vec(),
            values_db: target_db,
            anchor_db: 0.0,
            id: "flat".to_string(),
            sha256: "test-fixture".to_string(),
            normalization: "none".to_string(),
            reference_gain_db: 0.0,
            point_count: freqs.len(),
        },
        plan_sha256: "test-plan".to_string(),
        measurement_sha256: "reconstructed-job-0001".to_string(),
        phase_basis: "aligned_segment".to_string(),
        common_d0_samples: 0,
        channels: Vec::new(),
    }
}

fn t64_channel_input(
    name: &str,
    measured: &[f64],
    bands: Vec<ModalBand>,
) -> (ChannelInput, ChannelPeq) {
    let mut channel = ChannelPeq::new(name, 0).expect("channel");
    for band in bands {
        channel.push_band(band);
    }
    let input = ChannelInput {
        name: name.to_string(),
        index: 0,
        work_band_hz: [80.0, 250.0],
        design_positions: vec![PreparedPosition {
            label: "D1".to_string(),
            role: "design",
            weight: 1.0,
            db: measured.to_vec(),
        }],
        validation_positions: Vec::new(),
        modal_avg_db: measured.to_vec(),
        wide_avg_db: measured.to_vec(),
    };
    (input, channel)
}

#[ignore = "requires measurement evidence fixtures that are not shipped in the public repository"]
#[test]
fn t64_target_aware_recovery_flat_on_reconstructed_job0001_modal_curve() {
    let (freqs, measured) = t64_reconstructed_modal_curve();
    let ctx = t64_context(&freqs, vec![0.0; freqs.len()]);
    let (input, channel) = t64_channel_input("FL", &measured, t64_report_bands("flat"));
    let band = [80.0, 250.0];

    let (before_pass, before_peak, before_worsen) =
        aggregate_design_rule_2a(&ctx, &input, &channel, band).expect("before rules");
    assert!(!before_pass, "fixture must reproduce the real flat FAIL");
    assert!(
        (3.0..5.0).contains(&before_peak),
        "reconstructed peak_over {before_peak:.3} dB out of expected range (producer 4.079)"
    );
    assert!(
        (2.0..3.5).contains(&before_worsen),
        "reconstructed worst_worsening {before_worsen:.3} dB out of expected range (producer 2.901)"
    );

    let (recovered, report) =
        try_target_aware_recovery(&ctx, &input, &channel, band, &PeqFitOptions::default())
            .expect("recovery evaluation");
    assert!(report.triggered, "recovery must be triggered by 2a FAIL");
    assert!(
        report.applied,
        "flat recovery must find an accepted candidate within the fixed budget (note: {})",
        report.note
    );
    assert!(report.before_peak_over_db > RULE_2A_LIMIT_DB);
    assert!(
        report.after_peak_over_db <= RULE_2A_LIMIT_DB + 1e-9,
        "after peak {:.4} dB must satisfy unchanged 3 dB gate",
        report.after_peak_over_db
    );
    assert!(
        report.after_worst_worsening_db <= RULE_2A_LIMIT_DB + 1e-9,
        "after worsening {:.4} dB must satisfy unchanged 3 dB gate",
        report.after_worst_worsening_db
    );
    assert_eq!(report.added_bands.len(), 1, "exactly one recovery band");
    let added = &report.added_bands[0];
    assert!(
        (80.0..=90.0).contains(&added.freq_hz),
        "recovery band must target the 80-90 Hz blind-spot peak, got {:.2} Hz",
        added.freq_hz
    );
    assert!(added.gain_db < 0.0, "recovery band must be a cut");
    assert!(
        report.candidates_tried > 0 && report.candidates_tried <= RECOVERY_MAX_CANDIDATES,
        "candidate budget must be bounded and deterministic, got {}",
        report.candidates_tried
    );

    let recovered_channel = recovered.expect("accepted candidate");
    assert!(
        (4..=5).contains(&recovered_channel.bands.len()),
        "reported bands may shrink if the 2a repair loop removes a near-zero band; got {}",
        recovered_channel.bands.len()
    );
    assert!(
        recovered_channel
            .bands
            .iter()
            .any(|band| (band.freq_hz - added.freq_hz).abs() < 1e-6),
        "recovery band must be present in the returned channel"
    );
    eprintln!(
        "T64-FLAT-RECOVERY before_peak={:.6} before_worsen={:.6} after_peak={:.6} after_worsen={:.6} after_rms={:.6} added_freq={:.6} added_q={:.6} added_gain={:.6} candidates={}",
        report.before_peak_over_db,
        report.before_worst_worsening_db,
        report.after_peak_over_db,
        report.after_worst_worsening_db,
        report.after_rms_after_db,
        added.freq_hz,
        added.q,
        added.gain_db,
        report.candidates_tried
    );
    let (after_pass, after_peak, after_worsen) =
        aggregate_design_rule_2a(&ctx, &input, &recovered_channel, band).expect("after rules");
    assert!(after_pass);
    assert!(after_peak <= RULE_2A_LIMIT_DB + 1e-9);
    assert!(after_worsen <= RULE_2A_LIMIT_DB + 1e-9);

    // 确定性：同一输入重复调用得到完全相同的报告与候选段参数。
    let (_, report_again) =
        try_target_aware_recovery(&ctx, &input, &channel, band, &PeqFitOptions::default())
            .expect("recovery deterministic");
    assert_eq!(report, report_again, "recovery must be deterministic");
}

#[ignore = "requires measurement evidence fixtures that are not shipped in the public repository"]
#[test]
fn t64_target_aware_recovery_not_triggered_for_tilt10() {
    let (freqs, measured) = t64_reconstructed_modal_curve();
    let tilt_db: Vec<f64> = freqs
        .iter()
        .map(|freq| -(10.0 / 3.0) * (freq / 1000.0).log10())
        .collect();
    let ctx = t64_context(&freqs, tilt_db.clone());
    let (input, channel) = t64_channel_input("FL", &measured, t64_report_bands("tilt10"));
    let band = [80.0, 250.0];

    let (before_pass, peak, worsen) =
        aggregate_design_rule_2a(&ctx, &input, &channel, band).expect("tilt rules");
    assert!(
        before_pass,
        "tilt10 regression: reconstructed real curve must still pass 2a (peak {peak:.3}, worsen {worsen:.3})"
    );
    let (recovered, report) =
        try_target_aware_recovery(&ctx, &input, &channel, band, &PeqFitOptions::default())
            .expect("tilt recovery check");
    assert!(recovered.is_none());
    assert!(!report.triggered, "must not trigger when 2a already passes");
    assert!(!report.applied);
    assert_eq!(report.candidates_tried, 0);
}

#[ignore = "requires measurement evidence fixtures that are not shipped in the public repository"]
#[test]
fn t64_target_aware_recovery_unresolved_keeps_fail() {
    let (freqs, mut measured) = t64_reconstructed_modal_curve();
    // +18 dB 窄峰：单个 cut 段上限 −12 dB 无法压到 +3 dB 内 → 必须如实 unresolved。
    for (freq, value) in freqs.iter().zip(measured.iter_mut()) {
        let distance = (freq.ln() - 120.0_f64.ln()).abs() / (1.0_f64 / 12.0 * 2.0_f64.ln());
        *value += 18.0 * (-distance * distance).exp();
    }
    let ctx = t64_context(&freqs, vec![0.0; freqs.len()]);
    let (input, channel) = t64_channel_input("FL", &measured, Vec::new());
    let band = [80.0, 250.0];
    let (recovered, report) =
        try_target_aware_recovery(&ctx, &input, &channel, band, &PeqFitOptions::default())
            .expect("unresolved evaluation");
    assert!(
        recovered.is_none(),
        "unresolvable case must not be accepted"
    );
    assert!(report.triggered);
    assert!(!report.applied);
    assert!(
        report.note.contains("unresolved"),
        "note must explicitly record unresolved cause, got {}",
        report.note
    );
    assert_eq!(report.candidates_tried, RECOVERY_MAX_CANDIDATES);
    assert!(report.after_peak_over_db > RULE_2A_LIMIT_DB);
}

// ---------------------------------------------------------------------------
// 双 target measured 路径（真实 analysis JSON）回归
// ---------------------------------------------------------------------------

fn t64_repo_evidence(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("evidence")
        .join(rel)
}

fn t64_write_measured_spec(
    tag: &str,
    target: &str,
    channels: &[(String, Option<String>)],
) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&dir).expect("create target dir");
    let path = dir.join(format!("t64-dual-target-{tag}.json"));
    let mut channel_json = Vec::new();
    for (index, (analysis, diag)) in channels.iter().enumerate() {
        let name = if index == 0 { "FL" } else { "FR" };
        let analysis = analysis.replace('\\', "/");
        match diag {
            Some(diag) => channel_json.push(format!(
                "{{\"name\":\"{name}\",\"analysis\":\"{analysis}\",\"diag\":\"{}\"}}",
                diag.replace('\\', "/")
            )),
            None => channel_json.push(format!(
                "{{\"name\":\"{name}\",\"analysis\":\"{analysis}\"}}"
            )),
        }
    }
    let body = format!(
        "{{\n \"schema\": \"khaen-cal-measured-design/v1\",\n \"session_id\": \"t64-dual-{tag}\",\n \"layout\": \"stereo-2.0\",\n \"sample_rate_hz\": 48000,\n \"target\": \"{target}\",\n \"validation\": \"pilot_single_position_same_as_design_proxy\",\n \"provenance\": \"real measurement analysis json (offline regression fixture)\",\n \"design_position\": {{\"label\": \"D1\", \"mic_orientation\": \"90deg\", \"channels\": [{}]}},\n \"channel_bands\": [{{\"channel\": \"FL\", \"band_hz\": [80.0, 16000.0]}}, {{\"channel\": \"FR\", \"band_hz\": [80.0, 16000.0]}}],\n \"dense_to_dac\": [0, 1],\n \"roles\": [{{\"role_index\": 0, \"role_id\": \"FL\", \"dac\": 0, \"outputs\": [0]}}, {{\"role_index\": 1, \"role_id\": \"FR\", \"dac\": 1, \"outputs\": [1]}}],\n \"container_channels\": 8,\n \"unused_outputs\": [2, 3, 4, 5, 6, 7],\n \"compile\": {{\"block_size\": 128, \"n_in\": 2, \"container_channels\": 8, \"low_band_d\": 0, \"l_g\": 8, \"l_p\": 8}},\n \"modal_artifact\": null\n}}\n",
        channel_json.join(", ")
    );
    std::fs::write(&path, body).expect("write measured spec");
    path
}

fn t64_fit_measured_spec(
    spec_path: &std::path::Path,
    target_fir_taps: Option<usize>,
) -> PeqFitOutput {
    let spec = load_measured_spec(spec_path).expect("load measured spec");
    let built = build_measured_design_opts(&spec, &MeasuredBuildOptions::production())
        .expect("build measured design");
    let mut design = built.design;
    if let Some(taps) = target_fir_taps {
        // 与 CLI 实验路径一致：measured 实验启用 T64 target FIR（P_post 非 identity）。
        design.peq_options.target_fir.enabled = true;
        design.peq_options.target_fir.taps = taps;
    }
    let mut request = PeqFitRequest::new(&design.plan, &design.target, &design.curves)
        .with_options(design.peq_options.clone())
        .with_created_unix(design.created_unix);
    if !design.channel_bands.is_empty() {
        request = request.with_channel_bands(&design.channel_bands);
    }
    fit_peq(&request).expect("fit measured design")
}

#[ignore = "requires measurement evidence fixtures that are not shipped in the public repository"]
#[test]
fn t64_measured_dual_target_flat_and_tilt10_with_real_analysis() {
    let fl = t64_repo_evidence("board-window-20261118-calib/session-B/tool/FL-ch00.analysis.json");
    assert!(
        fl.is_file(),
        "real analysis fixture missing: {}",
        fl.display()
    );
    // session-B 的 FL analysis 复制给 FR：T65 相位一致性不在本测试范围（只测 T64 双 target
    // measured 桥接与 2a 判定）；该 fixture 是真实测量数据，不是合成。
    let channels = vec![
        (fl.to_string_lossy().to_string(), None),
        (fl.to_string_lossy().to_string(), None),
    ];
    for target in ["flat", "tilt10"] {
        let spec_path = t64_write_measured_spec(&format!("sessionB-{target}"), target, &channels);
        // 双 target 完整路径按已验证的 measured 实验配置（target FIR 512）运行；
        // 实验/NON_ACCEPTABLE 标记仍由 CLI/job 层携带。
        let out = t64_fit_measured_spec(&spec_path, Some(512));
        eprintln!(
            "T64-DUAL target={target} checks={:?} recovery={:?}",
            out.report.checks,
            out.recovery
                .iter()
                .map(|r| (
                    r.channel.clone(),
                    r.triggered,
                    r.applied,
                    r.before_peak_over_db,
                    r.before_worst_worsening_db
                ))
                .collect::<Vec<_>>()
        );
        for channel in &out.report.channels {
            eprintln!(
                "T64-DUAL  {} 2a={} peak={:.3} worsen={:.3} bands={}",
                channel.name,
                channel.rule_2a_pass,
                channel.rule_2a_peak_over_target_db,
                channel.rule_2a_worst_worsening_db,
                channel.band_count
            );
        }
        assert_eq!(out.report.target.id, target, "target must be honoured");
        assert!(out.report.checks.all_headroom_pass, "{target}: headroom");
        if target == "tilt10" {
            assert!(
                out.report.checks.all_rule_2a_pass,
                "tilt10 regression must pass on session-B real analysis"
            );
        } else if !out.report.checks.all_rule_2a_pass {
            // flat 若在该真实数据上仍失败：必须有结构化 recovery 证据（不假 PASS）。
            for channel in &out.report.channels {
                if !channel.rule_2a_pass {
                    let report = out
                        .recovery
                        .iter()
                        .find(|report| report.channel == channel.name)
                        .unwrap_or_else(|| {
                            panic!("flat FAIL without recovery evidence: {}", channel.name)
                        });
                    assert!(
                        report.triggered,
                        "recovery must be attempted for failed channel"
                    );
                    if report.applied {
                        panic!("applied recovery must imply rule_2a pass (no fake pass)");
                    }
                    assert!(
                        report.note.contains("unresolved"),
                        "unresolved reason required: {}",
                        report.note
                    );
                }
            }
        } else {
            assert!(
                out.recovery.iter().all(|report| !report.triggered),
                "recovery must stay off when rule_2a already passes"
            );
        }
    }
}

#[test]
fn t64_measured_flat_unresolved_on_t4b_real_analysis_keeps_fail() {
    let fl = t64_repo_evidence(
        "x86-dsp-host-migration-20261121/CALIB-20261121-t4b/FL-ch00.analysis.json",
    );
    let fr = t64_repo_evidence(
        "x86-dsp-host-migration-20261121/CALIB-20261121-t4b/FR-ch01.analysis.json",
    );
    let fld =
        t64_repo_evidence("x86-dsp-host-migration-20261121/CALIB-20261121-t4b/FL-ch00.diag.json");
    let frd =
        t64_repo_evidence("x86-dsp-host-migration-20261121/CALIB-20261121-t4b/FR-ch01.diag.json");
    if !(fl.is_file() && fr.is_file() && fld.is_file() && frd.is_file()) {
        return; // 证据缺失时跳过（clean checkout 仍有 evidence，正常不触发）
    }
    let channels = vec![
        (
            fl.to_string_lossy().to_string(),
            Some(fld.to_string_lossy().to_string()),
        ),
        (
            fr.to_string_lossy().to_string(),
            Some(frd.to_string_lossy().to_string()),
        ),
    ];
    let spec_path = t64_write_measured_spec("t4b-flat", "flat", &channels);
    let out = t64_fit_measured_spec(&spec_path, None);
    assert_eq!(out.report.target.id, "flat");
    if out.report.checks.all_rule_2a_pass {
        // t4b 若未来用多点/更高质量数据重测后通过，则本测试自动变为通过记录。
        return;
    }
    let fl_report = out
        .report
        .channels
        .iter()
        .find(|channel| channel.name == "FL")
        .expect("FL report");
    assert!(
        !fl_report.rule_2a_pass,
        "t4b FL must stay honest FAIL (no fake pass)"
    );
    let recovery = out
        .recovery
        .iter()
        .find(|report| report.channel == "FL")
        .expect("recovery evidence must be emitted for failed channel");
    assert!(recovery.triggered, "recovery must have been attempted");
    assert!(
        !recovery.applied,
        "single-band recovery cannot legitimately fix t4b +12 dB modal failure; \
         must remain unresolved (note: {})",
        recovery.note
    );
    assert!(
        recovery.note.contains("unresolved"),
        "unresolved reason must be explicit: {}",
        recovery.note
    );
    assert!(recovery.candidates_tried <= RECOVERY_MAX_CANDIDATES);
    assert_eq!(
        recovery.after_peak_over_db, recovery.before_peak_over_db,
        "unapplied recovery must not alter reported gate values"
    );
}
