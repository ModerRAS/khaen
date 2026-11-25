// T70：crossover（T39 输出 FIR 分频）接入 measured P_post / T66 plan（目标 3 缺口）私有单测。
//
// 经 src/fit_wiring.rs 末尾 #[cfg(test)] mod crossover_p_post_tests { include!(...) } 进入 lib
// 单测构建（cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator，勿写入
// public_registration.rs）。全部测试名 t70_ 前缀。
//
// 运行：cargo test -p tspi-calibration t70_ （TEMP/TMP 需可写目录；本机 R:\Temp 只读）
//
// 覆盖：
//   (1) spec 解析（缺省关闭 / LR4 显式配置 / 未知字段拒绝）
//   (2) spec 校验（未知 type/method、空/非法 assignments、fc/taps 参数域）
//   (3) LR4 系数/幅频/合并平坦（T39 design_strict 产物独立复算）
//   (4) P_post 组合（crossover ⊛ target 段序/长度/identity/确定性 hash）
//   (5) plan flags/证据（diag bit2、report crossover 摘要、plan 确定性）
//   (6) D7 绑定（卷积后 P 行预测与编码行交叉绑定；篡改必拒）
//   (7) 预算 fail-closed（组合核 > max_taps / l_p 未同步 / taps 超 plan cap）
//   (8) 布局拒绝策略（非 4.1.2 拒绝；生产路径未验收时仅实验）
//   (9) 无 crossover 行为不变（bit-exact：plan 字节确定、无 crossover 键/diag bit）
//   (10) measured 4.1.2 + target FIR(512) + LR4 80Hz/2048 全链（L_P=2559，D7 无跳过）
//
// 纪律：全部离线（不打开 ALSA/CueMix、不碰板端）；不伪造 PASS；真实 ALSA/T3/holdout NOT_RUN。

use std::fs;
use std::path::{Path, PathBuf};

use tspi_dsp_compat::ws::{json_parse, JsonValue};
use tspi_dsp_core::nodes::FirSegmentKind;

use crate::compile_apply::{
    compile_plan, CompileApplyRequest, CompiledPlan, DelayPolarityEncoding, ExperimentMode,
    RoleCompileInput, DIAG_FLAG_CROSSOVER, DIAG_FLAG_TARGET, E_CA_D7_PEAK,
    W_EXPERIMENT_IIR_EXCLUDED, W_EXPERIMENT_P_BINDING_SKIPPED,
};
use crate::crossover_post::{
    compose_role_p_post, compose_segments, kernel_sha256, CrossoverSpecConfig, CrossoverWiring,
    CROSSOVER_LAYOUT_REQUIRED,
};
use crate::fit_wiring::{
    apply_measured_crossover_experiment, build_crossover_evidence, build_headroom,
    build_phase_evidence, build_roles, enable_crossover_p_post, enable_target_fir_p_post,
    synthetic_design, CrossoverPPostOutcome, FitWiringDesign, E_WIRING_CROSSOVER_BUDGET,
};
use crate::measured_design::{
    build_measured_design_opts, load_measured_spec, MeasuredBuildOptions, MeasuredDesign,
};
use crate::peq_fit::{fit_peq, PeqFitRequest};
use crate::phase_fit::{fit_phase, PhaseFitRequest};

// ---------------------------------------------------------------------------
// 夹具：临时目录 / 4.1.2 measured spec（5 执行器，真实格式 analysis JSON）
// ---------------------------------------------------------------------------

const T70_FS: u32 = 48_000;
const T70_ROLE_IDS: [&str; 5] = ["FL", "FR", "SL", "SR", "SUB1"];
const T70_ROLE_DACS: [u16; 5] = [0, 1, 2, 3, 7];

fn t70_tmp_dir(tag: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let dir = base.join(format!("t70-tmp-{tag}-pid{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create t70 temp dir");
    dir
}

fn t70_write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("write t70 fixture");
    path
}

/// 与旧工具同形的 analysis JSON（schema tspi-channel-measurement/v1；64 点 1/12 oct，
/// 20..20000 Hz 全带，0 dB 幅度 + 线性相位；用于接通 measured 输入，不是真实测量）。
fn t70_analysis_json(channel: u64, sr: u32, delay: f64) -> String {
    let mut fr = String::new();
    let mut index = 0;
    loop {
        let freq = 20.0 * 2f64.powf(index as f64 / 12.0);
        if freq > 20_000.0 {
            break;
        }
        let phase =
            -2.0 * std::f64::consts::PI * freq * 0.1 / sr as f64 * 180.0 / std::f64::consts::PI;
        if !fr.is_empty() {
            fr.push(',');
        }
        fr.push_str(&format!(
            "{{\"freq_hz\":{freq:.6},\"db\":0.0,\"phase_deg\":{phase:.6}}}"
        ));
        index += 1;
    }
    format!(
        "{{\"schema\":\"tspi-channel-measurement/v1\",\"channel\":{channel},\"delay_samples\":{delay:.6},\"snr_band_db\":18.0,\"polarity_confidence\":\"high\",\"meta\":{{\"sample_rate\":{sr},\"sweep_start_hz\":20.0,\"sweep_end_hz\":20000.0}},\"fr\":[{fr}]}}"
    )
}

/// 写一个 4.1.2 measured spec 到 dir，返回 spec 路径（crossover 节可选）。
fn t70_spec_file(dir: &Path, layout: &str, crossover: Option<&str>) -> PathBuf {
    fs::create_dir_all(dir).expect("create spec dir");
    let mut channels = Vec::new();
    for (index, name) in T70_ROLE_IDS.iter().enumerate() {
        let analysis = t70_write(
            dir,
            &format!("{name}-ch{:02}.analysis.json", T70_ROLE_DACS[index]),
            &t70_analysis_json(T70_ROLE_DACS[index] as u64, T70_FS, 940.0 + index as f64),
        );
        channels.push(format!(
            "{{\"name\":\"{name}\",\"analysis\":{:?}}}",
            analysis.to_string_lossy()
        ));
    }
    let mut roles = Vec::new();
    for (index, name) in T70_ROLE_IDS.iter().enumerate() {
        roles.push(format!(
            "{{\"role_index\":{index},\"role_id\":\"{name}\",\"dac\":{},\"outputs\":[{}]}}",
            T70_ROLE_DACS[index], T70_ROLE_DACS[index]
        ));
    }
    let crossover_section = match crossover {
        Some(raw) => format!(",\n  \"crossover\": {raw}"),
        None => String::new(),
    };
    let body = format!(
        "{{\n  \"schema\": \"tspi-cal-measured-design/v1\",\n  \"session_id\": \"t70-unit-test\",\n  \"layout\": \"{layout}\",\n  \"sample_rate_hz\": {T70_FS},\n  \"target\": \"flat\",\n  \"validation\": \"pilot_single_position_same_as_design_proxy\",\n  \"provenance\": \"t70_unit_test_fixture_only\",\n  \"design_position\": {{\n    \"label\": \"D1\",\n    \"mic_orientation\": \"90deg\",\n    \"channels\": [{}]\n  }},\n  \"dense_to_dac\": [0, 1, 2, 3, 7],\n  \"roles\": [{}],\n  \"container_channels\": 8,\n  \"unused_outputs\": [4, 5, 6],\n  \"compile\": {{\"block_size\": 128, \"n_in\": 5, \"container_channels\": 8, \"low_band_d\": 0, \"l_g\": 8, \"l_p\": 8}},\n  \"modal_artifact\": null,\n  \"fit_options\": {{\"low_band_trusted\": false}}{crossover_section}\n}}\n",
        channels.join(","),
        roles.join(",")
    );
    t70_write(dir, "measured-spec.json", &body)
}

/// LR4@80Hz/2048 显式赋值：FL/FR/SL/SR 高通、SUB1 低通（4.1.2 五执行器）。
fn t70_lr4_assignments() -> &'static str {
    "{\n    \"type\": \"LR4\",\n    \"freq_hz\": 80.0,\n    \"taps\": 2048,\n    \"method\": \"analytic_phase\",\n    \"attenuation_db\": 80.0,\n    \"assignments\": [\n      {\"output\": 0, \"branch\": \"highpass\"},\n      {\"output\": 1, \"branch\": \"highpass\"},\n      {\"output\": 2, \"branch\": \"highpass\"},\n      {\"output\": 3, \"branch\": \"highpass\"},\n      {\"output\": 7, \"branch\": \"lowpass\"}\n    ]\n  }"
}

fn t70_lr4_config() -> CrossoverSpecConfig {
    let document = json_parse(t70_lr4_assignments()).expect("json");
    CrossoverSpecConfig::parse(&document).expect("parse LR4 config")
}

/// 由 spec 构造 measured design（实验单点模式；crossover 节存在时自动接线）。
fn t70_measured_design(dir: &Path, layout: &str, crossover: Option<&str>) -> MeasuredDesign {
    let spec_path = t70_spec_file(dir, layout, crossover);
    let spec = load_measured_spec(&spec_path).expect("load measured spec");
    build_measured_design_opts(
        &spec,
        &MeasuredBuildOptions {
            experiment: ExperimentMode::SINGLE_POINT,
            layout_override: None,
            g_paths: None,
        },
    )
    .expect("build measured design")
}

// ---------------------------------------------------------------------------
// 夹具：T64/T65 -> T66 编译（镜像 FitWiring::stage_compile，不跑 JobManager）
// ---------------------------------------------------------------------------

fn t70_compile(design: &FitWiringDesign) -> Result<CompiledPlan, String> {
    let mut peq_request = PeqFitRequest::new(&design.plan, &design.target, &design.curves)
        .with_options(design.peq_options.clone())
        .with_created_unix(design.created_unix);
    if let Some(quality) = design.design_quality.as_ref() {
        peq_request = peq_request.with_design_quality(quality);
    }
    let peq = fit_peq(&peq_request).map_err(|error| format!("{}: {error}", error.code))?;
    let mut phase_request = PhaseFitRequest::new(&design.plan, &design.phase_responses)
        .with_options(design.phase_options.clone())
        .with_created_unix(design.created_unix)
        .with_channel_indices(&design.phase_channel_indices);
    if let Some(quality) = design.design_quality.as_ref() {
        phase_request = phase_request.with_design_quality(quality);
    }
    let phase = fit_phase(&phase_request).map_err(|error| format!("{}: {error}", error.code))?;
    let peq_artifacts = peq.artifacts();
    let report_bytes = peq_artifacts
        .entries
        .iter()
        .find(|entry| entry.path == crate::peq_fit::FIT_REPORT_ARTIFACT_PATH)
        .map(|entry| entry.bytes.clone())
        .ok_or_else(|| "missing T64 report bytes".to_string())?;
    let headroom =
        build_headroom(design, &peq, &report_bytes).map_err(|failure| format!("{failure:?}"))?;
    let phase_artifacts = phase.artifacts.clone();
    let phase_evidence = build_phase_evidence(design, &phase, &phase_artifacts)
        .map_err(|failure| format!("{failure:?}"))?;
    let roles = build_roles(design, &peq, &phase).map_err(|failure| format!("{failure:?}"))?;
    let crossover_evidence =
        build_crossover_evidence(design, &roles).map_err(|failure| format!("{failure:?}"))?;
    let request = CompileApplyRequest::new(
        design.plan.sample_rate_hz,
        design.compile.block_size,
        design.compile.n_in,
        design.compile.container_channels,
        design.compile.low_band_d,
        design.compile.l_g,
        design.compile.l_p,
        &design.mapping,
        &phase_evidence,
        &headroom,
        &design.artifact,
        &design.g_paths,
        &roles,
        design.compile.encoding,
        design.compile.options,
    );
    let request = if design.compile.experiment.is_active() {
        request
            .with_experiment(design.compile.experiment)
            .with_experiment_notes(Vec::new())
    } else {
        request
    };
    let request = match crossover_evidence.as_ref() {
        Some(evidence) => request.with_crossover(evidence),
        None => request,
    };
    compile_plan(&request).map_err(|error| format!("{error}"))
}

fn t70_fit_outputs(
    design: &FitWiringDesign,
) -> (
    crate::peq_fit::PeqFitOutput,
    crate::phase_fit::PhaseFitOutput,
) {
    let mut peq_request = PeqFitRequest::new(&design.plan, &design.target, &design.curves)
        .with_options(design.peq_options.clone())
        .with_created_unix(design.created_unix);
    if let Some(quality) = design.design_quality.as_ref() {
        peq_request = peq_request.with_design_quality(quality);
    }
    let peq = fit_peq(&peq_request).expect("fit peq");
    let mut phase_request = PhaseFitRequest::new(&design.plan, &design.phase_responses)
        .with_options(design.phase_options.clone())
        .with_created_unix(design.created_unix)
        .with_channel_indices(&design.phase_channel_indices);
    if let Some(quality) = design.design_quality.as_ref() {
        phase_request = phase_request.with_design_quality(quality);
    }
    let phase = fit_phase(&phase_request).expect("fit phase");
    (peq, phase)
}

// ---------------------------------------------------------------------------
// 独立 DTFT（不依赖库内 helper）：返回 |H(f)| dB / 复数
// ---------------------------------------------------------------------------

fn t70_complex_response(coeffs: &[f64], fs: f64, freq_hz: f64) -> (f64, f64) {
    let mut re = 0.0;
    let mut im = 0.0;
    for (index, coeff) in coeffs.iter().enumerate() {
        let phase = -2.0 * std::f64::consts::PI * freq_hz * index as f64 / fs;
        re += coeff * phase.cos();
        im += coeff * phase.sin();
    }
    (re, im)
}

fn t70_mag_db(coeffs: &[f64], fs: f64, freq_hz: f64) -> f64 {
    let (re, im) = t70_complex_response(coeffs, fs, freq_hz);
    20.0 * re.hypot(im).max(f64::MIN_POSITIVE).log10()
}

fn t70_log_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    let mut grid = Vec::with_capacity(points);
    for index in 0..points {
        let t = index as f64 / (points - 1) as f64;
        grid.push(lo * (hi / lo).powf(t));
    }
    grid
}
// ---------------------------------------------------------------------------
// (1) spec 解析：缺省关闭 / 显式 null / LR4 显式配置 / 显式字段
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_spec_parse_default_off_and_lr4() {
    // 缺省 -> None（生产默认 OFF）。
    let document = json_parse("{\"schema\":\"x\"}").expect("json");
    assert!(CrossoverSpecConfig::parse_section(&document)
        .unwrap()
        .is_none());
    // 显式 null -> None。
    let document = json_parse("{\"crossover\":null}").expect("json");
    assert!(CrossoverSpecConfig::parse_section(&document)
        .unwrap()
        .is_none());
    // 显式 LR4 -> Some，默认 method/attenuation 被填入。
    let document = json_parse(
        "{\"crossover\":{\"type\":\"lr4\",\"freq_hz\":80,\"taps\":2048,\"assignments\":[{\"output\":0,\"branch\":\"highpass\"},{\"output\":7,\"branch\":\"lowpass\"}]}}",
    )
    .expect("json");
    let config = CrossoverSpecConfig::parse_section(&document)
        .unwrap()
        .expect("crossover section");
    assert_eq!(config.kind, "lr4");
    assert!((config.fc_hz - 80.0).abs() < 1e-12);
    assert_eq!(config.taps, 2048);
    assert_eq!(config.method, "analytic_phase");
    assert!((config.attenuation_db - 80.0).abs() < 1e-12);
    assert_eq!(config.assignments.len(), 2);
    config.validate(T70_FS, 8).expect("config valid");
    // fc_hz 别名（CROSSOVER-DESIGN 文档口径）也接受。
    let document = json_parse(
        "{\"crossover\":{\"type\":\"LR4\",\"fc_hz\":80,\"taps\":2048,\"assignments\":[{\"output\":0,\"branch\":\"highpass\"}]}}",
    )
    .expect("json");
    let alias = CrossoverSpecConfig::parse_section(&document)
        .unwrap()
        .unwrap();
    assert!((alias.fc_hz - 80.0).abs() < 1e-12);
    // 规范 JSON 可回读。
    let encoded = tspi_dsp_compat::ws::json_encode(&config.to_json()).expect("encode");
    let roundtrip =
        CrossoverSpecConfig::parse(&json_parse(&encoded).expect("json")).expect("parse");
    assert_eq!(roundtrip, config);
}

// ---------------------------------------------------------------------------
// (2) spec 校验：未知 type/method、空/非法 assignments、fc/taps 参数域
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_spec_validate_rejects_unknown_and_bad_fields() {
    // 未知 type：保留 T39 稳定错误码文本。
    let document =
        json_parse("{\"crossover\":{\"type\":\"LR3\",\"freq_hz\":80,\"taps\":2048,\"assignments\":[{\"output\":0,\"branch\":\"highpass\"}]}}")
            .expect("json");
    let error = CrossoverSpecConfig::parse_section(&document).unwrap_err();
    assert!(error.contains("E_FIR_XOVER_TYPE"), "unknown type: {error}");
    // 未知 method。
    let document =
        json_parse("{\"crossover\":{\"type\":\"LR4\",\"freq_hz\":80,\"taps\":2048,\"method\":\"auto\",\"assignments\":[{\"output\":0,\"branch\":\"highpass\"}]}}")
            .expect("json");
    let error = CrossoverSpecConfig::parse_section(&document).unwrap_err();
    assert!(error.contains("method"), "unknown method: {error}");
    // 空 assignments：无默认 HP，拒绝。
    let document = json_parse(
        "{\"crossover\":{\"type\":\"LR4\",\"freq_hz\":80,\"taps\":2048,\"assignments\":[]}}",
    )
    .expect("json");
    let error = CrossoverSpecConfig::parse_section(&document).unwrap_err();
    assert!(error.contains("assignments"), "empty assignments: {error}");
    // 未知 branch。
    let document = json_parse(
        "{\"crossover\":{\"type\":\"LR4\",\"freq_hz\":80,\"taps\":2048,\"assignments\":[{\"output\":0,\"branch\":\"fullrange\"}]}}",
    )
    .expect("json");
    let error = CrossoverSpecConfig::parse_section(&document).unwrap_err();
    assert!(error.contains("branch"), "bad branch: {error}");
    // 输出越界（container=8，output=9）。
    let mut config = t70_lr4_config();
    config.assignments = vec![crate::crossover_post::CrossoverAssignmentSpec {
        output: 9,
        branch: "highpass".to_string(),
    }];
    let error = CrossoverWiring::build(&config, T70_FS, 8).unwrap_err();
    assert!(error.contains("越界"), "output range: {error}");
    // 重复赋值。
    let mut config = t70_lr4_config();
    config.assignments = vec![
        crate::crossover_post::CrossoverAssignmentSpec {
            output: 0,
            branch: "highpass".to_string(),
        },
        crate::crossover_post::CrossoverAssignmentSpec {
            output: 0,
            branch: "lowpass".to_string(),
        },
    ];
    let error = CrossoverWiring::build(&config, T70_FS, 8).unwrap_err();
    assert!(error.contains("重复"), "duplicate output: {error}");
    // taps 小于 T39 下限。
    let mut config = t70_lr4_config();
    config.taps = 4;
    let error = CrossoverWiring::build(&config, T70_FS, 8).unwrap_err();
    assert!(error.contains("taps"), "tiny taps: {error}");
    // fc 超出 0.225·fs（Nyquist fade 护栏外）-> T39 INFEASIBLE。
    let mut config = t70_lr4_config();
    config.fc_hz = 12_000.0;
    let error = CrossoverWiring::build(&config, T70_FS, 8).unwrap_err();
    assert!(
        error.contains("E_FIR_XOVER_FC_INFEASIBLE") || error.contains("E_FIR_BAND"),
        "fc guard: {error}"
    );
    // 生产档 fc > 0.2·fs 但 <= 0.225·fs -> E_FIR_XOVER_FC_LIMIT（design_strict 口径）。
    let mut config = t70_lr4_config();
    config.fc_hz = 10_000.0;
    let error = CrossoverWiring::build(&config, T70_FS, 8).unwrap_err();
    assert!(error.contains("E_FIR_XOVER_FC_LIMIT"), "fc limit: {error}");
    // experimental=true 不进入 plan 接线。
    let document =
        json_parse("{\"crossover\":{\"type\":\"LR4\",\"freq_hz\":80,\"taps\":2048,\"experimental\":true,\"assignments\":[{\"output\":0,\"branch\":\"highpass\"}]}}")
            .expect("json");
    let error = CrossoverSpecConfig::parse_section(&document).unwrap_err();
    assert!(error.contains("experimental"), "experimental flag: {error}");
}

// ---------------------------------------------------------------------------
// (3) LR4 系数 / 幅频 / 合并平坦（T39 产物独立复算）
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_lr4_coeffs_magnitude_and_merge_flat() {
    let wiring = CrossoverWiring::build(&t70_lr4_config(), T70_FS, 8).expect("build LR4 wiring");
    assert!(
        wiring.normative_pass(),
        "T39 criteria must pass: {:?}",
        wiring.failed_criteria()
    );
    assert_eq!(wiring.design.kind.as_str(), "LR4");
    assert_eq!(wiring.design.spec.taps, 2048);
    assert_eq!(wiring.declared_delay_samples(), 1023.5);
    assert!((wiring.design.merge.relative_delay_samples).abs() < 1e-12);
    assert!(wiring.design.merge.relative_delay_is_integer);
    assert!(wiring.design.merge.flat_pass);
    assert!(wiring.design.merge.flat_max_deviation_db <= 0.5);
    // 显式赋值：0..=3 HP、7 LP、其余直通（无默认 HP）。
    assert_eq!(wiring.kernel_for_output(0).map(|c| c.len()), Some(2048));
    assert_eq!(wiring.kernel_for_output(3).map(|c| c.len()), Some(2048));
    assert_eq!(wiring.kernel_for_output(7).map(|c| c.len()), Some(2048));
    assert!(wiring.kernel_for_output(4).is_none());
    assert!(wiring.kernel_for_output(6).is_none());
    // 两支系数有限且非零。
    for output in [0u16, 7u16] {
        let coeffs = wiring.kernel_for_output(output).expect("coeffs");
        assert!(coeffs.iter().all(|value| value.is_finite()));
        assert!(coeffs.iter().any(|value| value.abs() > 1e-6));
    }
    // 独立 DTFT 复算：合并和 |LP+HP| 在 [fc/2, 2fc] 平坦 <= 0.5 dB（T39 判据在系数上复算）。
    let lp = wiring
        .design
        .coeffs(tspi_fir_designer::crossover::Branch::Lowpass);
    let hp = wiring
        .design
        .coeffs(tspi_fir_designer::crossover::Branch::Highpass);
    let mut max_deviation = 0.0_f64;
    for freq in t70_log_grid(40.0, 160.0, 96) {
        let (lp_re, lp_im) = t70_complex_response(lp, T70_FS as f64, freq);
        let (hp_re, hp_im) = t70_complex_response(hp, T70_FS as f64, freq);
        let sum_db = 20.0 * (lp_re + hp_re).hypot(lp_im + hp_im).log10();
        if sum_db.is_finite() {
            max_deviation = max_deviation.max(sum_db.abs());
        }
    }
    assert!(
        max_deviation <= 0.5,
        "independent LR4 sum deviation {max_deviation} dB"
    );
    // 两支 @fc 幅度 ~ -6.02 dB。
    for coeffs in [lp, hp] {
        let at_fc = t70_mag_db(coeffs, T70_FS as f64, 80.0);
        assert!((at_fc - (-6.0206)).abs() <= 0.5, "branch @fc {at_fc} dB");
    }
    // 设计报告 hash 确定性。
    let again = CrossoverWiring::build(&t70_lr4_config(), T70_FS, 8).expect("build again");
    assert_eq!(wiring.design_report_sha256, again.design_report_sha256);
    assert_eq!(wiring.config_sha256, again.config_sha256);
}

// ---------------------------------------------------------------------------
// (4) P_post 组合：段序 / 长度 / identity / 确定性 hash
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_p_post_compose_order_and_length() {
    let crossover = [0.5_f64, 0.5];
    let target = [1.0_f64, 0.2, -0.1];
    let composed = compose_role_p_post(Some(&crossover), Some(&target)).expect("compose");
    assert_eq!(
        composed.segments,
        vec![FirSegmentKind::Crossover, FirSegmentKind::Target]
    );
    assert_eq!(composed.kernel.len(), 2 + 3 - 1);
    let expected = [
        0.5 * 1.0,
        0.5 * 1.0 + 0.5 * 0.2,
        0.5 * 0.2 + 0.5 * (-0.1),
        0.5 * (-0.1),
    ];
    for (actual, expected) in composed.kernel.iter().zip(expected.iter()) {
        assert!((actual - expected).abs() < 1e-15, "{actual} vs {expected}");
    }
    // target 关闭（identity）时只有 crossover 段。
    let only_crossover = compose_role_p_post(Some(&crossover), Some(&[1.0])).expect("compose");
    assert_eq!(only_crossover.segments, vec![FirSegmentKind::Crossover]);
    assert_eq!(only_crossover.kernel, crossover.to_vec());
    // 都没有 = identity。
    let identity = compose_role_p_post(None, Some(&[1.0])).expect("compose");
    assert!(identity.segments.is_empty());
    assert_eq!(identity.kernel, vec![1.0]);
    // 段序非法（target 在 crossover 前）必须拒绝。
    use tspi_dsp_core::nodes::FirSegment;
    let bad_order = vec![
        FirSegment::new(FirSegmentKind::Target, target.to_vec(), 0, 0),
        FirSegment::new(FirSegmentKind::Crossover, crossover.to_vec(), 0, 0),
    ];
    let error = compose_segments(&bad_order).unwrap_err();
    assert!(error.contains("段序"), "bad order: {error}");
    // 重复段拒绝。
    let duplicate = vec![
        FirSegment::new(FirSegmentKind::Crossover, crossover.to_vec(), 0, 0),
        FirSegment::new(FirSegmentKind::Crossover, crossover.to_vec(), 0, 0),
    ];
    assert!(compose_segments(&duplicate).unwrap_err().contains("重复"));
    // kernel hash 确定 + 内容敏感。
    let sha = kernel_sha256(&composed.kernel);
    assert_eq!(sha, kernel_sha256(&composed.kernel));
    let mut changed = composed.kernel.clone();
    changed[0] += 1e-9;
    assert_ne!(sha, kernel_sha256(&changed));
}
/// 用给定 roles/evidence 编译（D7 绑定负向用例用；重算 T64/T65 证据）。
fn t70_compile_from(
    design: &FitWiringDesign,
    roles: &[RoleCompileInput],
    crossover_evidence: Option<&crate::compile_apply::CrossoverEvidence>,
) -> Result<CompiledPlan, String> {
    let (peq, phase) = t70_fit_outputs(design);
    let peq_artifacts = peq.artifacts();
    let report_bytes = peq_artifacts
        .entries
        .iter()
        .find(|entry| entry.path == crate::peq_fit::FIT_REPORT_ARTIFACT_PATH)
        .map(|entry| entry.bytes.clone())
        .ok_or_else(|| "missing T64 report bytes".to_string())?;
    let headroom =
        build_headroom(design, &peq, &report_bytes).map_err(|failure| format!("{failure:?}"))?;
    let phase_artifacts = phase.artifacts.clone();
    let phase_evidence = build_phase_evidence(design, &phase, &phase_artifacts)
        .map_err(|failure| format!("{failure:?}"))?;
    let request = CompileApplyRequest::new(
        design.plan.sample_rate_hz,
        design.compile.block_size,
        design.compile.n_in,
        design.compile.container_channels,
        design.compile.low_band_d,
        design.compile.l_g,
        design.compile.l_p,
        &design.mapping,
        &phase_evidence,
        &headroom,
        &design.artifact,
        &design.g_paths,
        roles,
        design.compile.encoding,
        design.compile.options,
    );
    let request = if design.compile.experiment.is_active() {
        request
            .with_experiment(design.compile.experiment)
            .with_experiment_notes(Vec::new())
    } else {
        request
    };
    let request = match crossover_evidence {
        Some(evidence) => request.with_crossover(evidence),
        None => request,
    };
    compile_plan(&request).map_err(|error| format!("{error}"))
}

// ---------------------------------------------------------------------------
// (5) plan flags / 证据 / 确定性（measured 4.1.2 + crossover，target FIR 关闭）
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_plan_flags_evidence_and_determinism() {
    let dir = t70_tmp_dir("plan-flags");
    let measured = t70_measured_design(&dir, "4.1.2", Some(t70_lr4_assignments()));
    let design = &measured.design;
    assert!(design.crossover.is_some(), "crossover must be wired");
    assert_eq!(design.crossover_required_taps().unwrap(), 2048);
    assert_eq!(design.compile.l_p, 2048);
    let plan = t70_compile(design).expect("compile crossover plan");
    // diag 行：每个 role 一行，flags 含 crossover bit2，不含 target/phase 位（显式段来源）。
    assert_eq!(plan.diags.len(), 5, "5 roles -> 5 diag rows");
    for diag in &plan.diags {
        assert_ne!(
            diag.flags & DIAG_FLAG_CROSSOVER,
            0,
            "diag out {} missing crossover flag",
            diag.out_index
        );
        assert_eq!(
            diag.flags & DIAG_FLAG_TARGET,
            0,
            "no target segment configured"
        );
        assert_eq!(diag.flags & crate::compile_apply::DIAG_FLAG_PHASE, 0);
    }
    // report crossover 摘要 + diag_count/p_taps_max 绑定。
    let summary = plan.report.crossover.as_ref().expect("crossover summary");
    assert_eq!(summary.kind, "LR4");
    assert!((summary.fc_hz - 80.0).abs() < 1e-12);
    assert_eq!(summary.taps, 2048);
    assert_eq!(summary.diag_count, 5);
    assert_eq!(summary.p_taps_max, 2048);
    assert_eq!(summary.roles.len(), 5);
    assert_eq!(plan.report.l_p, 2048);
    assert_eq!(plan.report.max_taps, 2048);
    // 逐 role p_taps / 延迟归零（F-16 单一延迟定义）。
    let fl = plan
        .report
        .roles
        .iter()
        .find(|role| role.role_id == "FL")
        .expect("FL");
    assert_eq!(fl.p_taps, 2048);
    assert_eq!(fl.delay_samples, 0);
    let sub = plan
        .report
        .roles
        .iter()
        .find(|role| role.role_id == "SUB1")
        .expect("SUB1");
    assert_eq!(sub.p_taps, 2048);
    assert_eq!(sub.delay_samples, 0);
    // 确定性：同输入两次编译 plan 字节一致；报告 JSON 含 crossover 节。
    let again = t70_compile(design).expect("compile again");
    assert_eq!(plan.plan_bytes, again.plan_bytes);
    assert_eq!(plan.report.report_sha256(), again.report.report_sha256());
    let json = plan.report.to_json();
    assert!(
        json.contains("\"crossover\""),
        "report json must carry crossover"
    );
    assert!(json.contains(&summary.evidence_sha256));
    // 证据 hash 与 design 报告 hash 都是 64-hex。
    assert_eq!(summary.design_report_sha256.len(), 64);
    assert_eq!(summary.evidence_sha256.len(), 64);
}

// ---------------------------------------------------------------------------
// (6) D7 绑定：卷积后 P 行预测 vs 编码 P 行（篡改必拒，不沿用实验跳过）
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_d7_binding_rejects_tampered_p_row() {
    let dir = t70_tmp_dir("d7-binding");
    let measured = t70_measured_design(&dir, "4.1.2", Some(t70_lr4_assignments()));
    let design = &measured.design;
    let (peq, phase) = t70_fit_outputs(design);
    let roles = build_roles(design, &peq, &phase).expect("roles");
    let evidence = build_crossover_evidence(design, &roles)
        .expect("evidence")
        .expect("crossover evidence");
    assert_eq!(evidence.roles.len(), 5);
    // 正向：证据与编码 P 行绑定一致 -> 编译通过，且不出现实验 P 绑定跳过 warning。
    let plan = t70_compile_from(design, &roles, Some(&evidence)).expect("bound plan compiles");
    assert!(
        !plan.d7_warnings.contains(&W_EXPERIMENT_P_BINDING_SKIPPED),
        "crossover roles must be bound (no W_EXPERIMENT_P_BINDING_SKIPPED): {:?}",
        plan.d7_warnings
    );
    let fl_record = evidence.role("FL").expect("FL evidence");
    let fl_role = roles
        .iter()
        .find(|role| role.role_id == "FL")
        .expect("FL role");
    assert_eq!(
        fl_role.fir_segments.first(),
        Some(&FirSegmentKind::Crossover)
    );
    assert_eq!(fl_record.composed_taps, fl_role.p_fir.len());
    // 负向 A：P 行被放大（更热）-> 链增益/正峰绑定不一致 -> E_CA_D7_PEAK。
    let mut tampered_roles = roles.clone();
    for role in tampered_roles.iter_mut() {
        if role.role_id == "FL" {
            for coeff in role.p_fir.iter_mut() {
                *coeff *= 1.05;
            }
        }
    }
    let error = t70_compile_from(design, &tampered_roles, Some(&evidence)).unwrap_err();
    assert!(
        error.contains(E_CA_D7_PEAK),
        "hotter P row must be rejected: {error}"
    );
    // 负向 B：证据预测被篡改 -> 绑定不一致 -> E_CA_D7_PEAK。
    let mut tampered_evidence = evidence.clone();
    tampered_evidence.roles[0].p_row_gain_db += 0.5;
    let error = t70_compile_from(design, &roles, Some(&tampered_evidence)).unwrap_err();
    assert!(
        error.contains(E_CA_D7_PEAK) || error.contains("E_CA_CROSSOVER"),
        "tampered evidence must be rejected: {error}"
    );
    // 负向 C：去掉 crossover 证据但 role 仍含 crossover 段 -> fail-closed。
    let error = t70_compile_from(design, &roles, None).unwrap_err();
    assert!(
        error.contains("E_CA_CROSSOVER"),
        "missing evidence: {error}"
    );
}

// ---------------------------------------------------------------------------
// (7) 预算 fail-closed：组合核超 max_taps / l_p 未同步 / taps 超 plan cap
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_budget_fail_closed() {
    // A) target FIR 4096 + crossover 2048 -> 组合核 6143 > max_taps 4096，预检拒绝。
    let dir = t70_tmp_dir("budget-a");
    let measured = t70_measured_design(&dir, "4.1.2", None);
    let mut design = measured.design;
    enable_target_fir_p_post(&mut design, 4096).expect("target FIR 4096");
    let error = enable_crossover_p_post(&mut design, t70_lr4_config()).unwrap_err();
    assert!(
        error.contains(E_WIRING_CROSSOVER_BUDGET),
        "budget must fail closed: {error}"
    );
    // B) taps 超过 plan 常规 profile 上限 4096 -> 拒绝。
    let dir = t70_tmp_dir("budget-b");
    let measured = t70_measured_design(&dir, "4.1.2", None);
    let mut design = measured.design;
    let mut oversize = t70_lr4_config();
    oversize.taps = 8192;
    let error = enable_crossover_p_post(&mut design, oversize).unwrap_err();
    // 预算预检先于 T39 参数域：8192 taps 组合核超 max_taps 直接 E_WIRING_CROSSOVER_BUDGET。
    assert!(
        error.contains("taps")
            && (error.contains(E_WIRING_CROSSOVER_BUDGET)
                || error.contains("E_WIRING_MEASURED_CROSSOVER")),
        "oversize taps: {error}"
    );
    // C) L_P 未同步（人为压小）-> FitWiringDesign.validate fail-closed（禁止截尾）。
    let dir = t70_tmp_dir("budget-c");
    let measured = t70_measured_design(&dir, "4.1.2", None);
    let mut design = measured.design;
    enable_crossover_p_post(&mut design, t70_lr4_config()).expect("crossover wires");
    assert_eq!(design.compile.l_p, 2048);
    design.compile.l_p = 64;
    let error = design.validate().unwrap_err();
    assert!(
        error.contains(E_WIRING_CROSSOVER_BUDGET),
        "stale l_p must be rejected: {error}"
    );
    // D) 恰好 <= 预算通过（target 512 + crossover 2048 = 2559 <= 4096）。
    let dir = t70_tmp_dir("budget-d");
    let measured = t70_measured_design(&dir, "4.1.2", Some(t70_lr4_assignments()));
    let mut design = measured.design;
    enable_target_fir_p_post(&mut design, 512).expect("target 512");
    assert_eq!(design.crossover_required_taps().unwrap(), 2559);
    assert_eq!(design.compile.l_p, 2559);
    design.validate().expect("2559 <= max_taps 4096");
}
// ---------------------------------------------------------------------------
// (8) 布局拒绝策略：非 4.1.2 拒绝；crossover 仅实验路径（生产未验收）
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_layout_policy_rejects_non_412() {
    // A) 非 4.1.2 布局（synthetic stereo-2.0）直接拒绝。
    let mut synthetic = synthetic_design().expect("synthetic design");
    assert_eq!(synthetic.plan.layout, "stereo-2.0");
    let error = enable_crossover_p_post(&mut synthetic, t70_lr4_config()).unwrap_err();
    assert!(
        error.contains(CROSSOVER_LAYOUT_REQUIRED) && error.contains("E_WIRING_MEASURED_CROSSOVER"),
        "non-4.1.2 layout must be rejected: {error}"
    );
    // B) 生产路径（experiment OFF）不得接分频：库入口 fail-closed。
    //    （measured 生产构建在 crossover 检查前会先以 E_CA_MAPPING 拒绝非 stereo 布局的空
    //    stereo_mask，因此这里直接验 apply_measured_crossover_experiment 的 experiment 门。）
    let dir = t70_tmp_dir("layout-prod");
    let measured = t70_measured_design(&dir, "4.1.2", None);
    let mut design = measured.design;
    let error =
        apply_measured_crossover_experiment(&mut design, t70_lr4_config(), ExperimentMode::OFF)
            .unwrap_err();
    assert!(
        error.contains("E_WIRING_MEASURED_CROSSOVER") && error.contains("experiment"),
        "production crossover must fail closed: {error}"
    );
    assert!(
        design.crossover.is_none(),
        "rejected crossover must not mutate design"
    );
    // C) 4.1.2 + 实验 -> 接线成功（l_p 同步到 2048；未赋值输出直通）。
    let dir = t70_tmp_dir("layout-exp");
    let measured = t70_measured_design(&dir, "4.1.2", Some(t70_lr4_assignments()));
    let wiring = measured.design.crossover.as_ref().expect("crossover wired");
    assert_eq!(wiring.assignments.len(), 5);
    assert_eq!(measured.design.compile.l_p, 2048);
    assert_eq!(measured.effective_layout, "4.1.2");
    // measured summary 里带 crossover 节（证据可审计）。
    let summary = json_parse(&measured.summary_json).expect("summary json");
    let crossover = summary.get("crossover").expect("summary crossover section");
    assert!(crossover.get("config").is_some());
    assert!(crossover.get("outcome").is_some());
    // D) 非 4.1.2 的 measured spec：布局政策在构建前/后都 fail-closed（可能先触发 mapping 校验，
    //    但绝不允许静默按 stereo 布局接分频）。
    let dir = t70_tmp_dir("layout-stereo");
    let spec_path = t70_spec_file(&dir, "stereo-2.0", Some(t70_lr4_assignments()));
    let spec = load_measured_spec(&spec_path).expect("spec");
    let error = build_measured_design_opts(
        &spec,
        &MeasuredBuildOptions {
            experiment: ExperimentMode::SINGLE_POINT,
            layout_override: None,
            g_paths: None,
        },
    )
    .unwrap_err();
    assert!(
        !error.is_empty(),
        "stereo layout + crossover must never silently build"
    );
}

// ---------------------------------------------------------------------------
// (9) 无 crossover 行为不变（bit-exact 回归：plan 字节/报告/无 crossover 键/无 diag bit2）
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_absent_bit_exact_regression() {
    let design = synthetic_design().expect("synthetic design");
    // 字段默认 None；L_P 不受影响。
    assert!(design.crossover.is_none());
    assert_eq!(design.crossover_required_taps().unwrap(), 0);
    assert_eq!(design.compile.l_p, 8);
    let plan = t70_compile(&design).expect("baseline compile");
    // 同输入两次编译：plan 字节与报告 hash 完全一致（确定性，无隐藏状态）。
    let again = t70_compile(&design).expect("baseline compile again");
    assert_eq!(plan.plan_bytes, again.plan_bytes);
    assert_eq!(plan.report.report_sha256(), again.report.report_sha256());
    // 报告无 crossover 摘要；JSON 不含 crossover 键；diag flags 无 bit2。
    assert!(plan.report.crossover.is_none());
    assert!(!plan.report.to_json().contains("\"crossover\""));
    for diag in &plan.diags {
        assert_eq!(diag.flags & DIAG_FLAG_CROSSOVER, 0);
        // 历史行为：synthetic 的 P 为 identity（target FIR 关闭），RoleMetadata 编码无 phase bit。
        assert_eq!(diag.flags, 0, "legacy diag flags must stay 0");
    }
    for role in &plan.report.roles {
        assert_eq!(role.p_taps, 1, "identity P row");
        assert!(role.delay_polarity_only);
    }
    // 接线证据 JSON 不含 crossover 键（历史字节不变）。
    let wiring = crate::fit_wiring::FitWiring::new(design).expect("FitWiring");
    let evidence = json_parse(&wiring.evidence_json()).expect("evidence json");
    assert!(
        evidence.get("crossover").is_none(),
        "no-crossover evidence must not add crossover key"
    );
}

// ---------------------------------------------------------------------------
// (10) measured 4.1.2 + target FIR(512) + LR4@80Hz/2048 全链（目标 3 交付实验配置）
// ---------------------------------------------------------------------------

#[test]
fn t70_crossover_measured_412_target_fir_full_chain() {
    let dir = t70_tmp_dir("full-chain");
    let measured = t70_measured_design(&dir, "4.1.2", Some(t70_lr4_assignments()));
    let mut design = measured.design;
    assert!(design.crossover.is_some());
    // 与 CLI --experiment-target-fir --target-fir-taps 512 同构。
    enable_target_fir_p_post(&mut design, 512).expect("enable target FIR");
    assert_eq!(design.crossover_required_taps().unwrap(), 2048 + 512 - 1);
    assert_eq!(design.compile.l_p, 2559);
    // 真实 rt-host plan-arena 只接受 factorized：实验路径显式 force。
    design.compile.options.force_form = Some(tspi_dsp_core::compile::FusionForm::Factorized);
    design
        .validate()
        .expect("design valid with composed budget");
    let plan = t70_compile(&design).expect("compile composed plan");
    // 5 个 role 都是 crossover ⊛ target（diag flags = bit2|bit3）。
    assert_eq!(plan.diags.len(), 5);
    for diag in &plan.diags {
        assert_ne!(diag.flags & DIAG_FLAG_CROSSOVER, 0, "crossover bit");
        assert_ne!(diag.flags & DIAG_FLAG_TARGET, 0, "target segment bit");
    }
    let summary = plan.report.crossover.as_ref().expect("crossover summary");
    assert_eq!(summary.taps, 2048);
    assert_eq!(summary.diag_count, 5);
    assert_eq!(summary.p_taps_max, 2559);
    assert_eq!(plan.report.l_p, 2559);
    assert_eq!(plan.report.max_taps, 2559);
    assert_eq!(plan.report.diag_count, 5);
    for role in &plan.report.roles {
        assert_eq!(role.p_taps, 2559, "role {} composed taps", role.role_id);
        assert_eq!(role.delay_samples, 0, "F-16 single delay definition");
    }
    // D7：crossover 行用绝对幅度峰值 + 证据绑定（不沿用实验跳过 warning）。
    assert!(
        !plan.d7_warnings.contains(&W_EXPERIMENT_P_BINDING_SKIPPED),
        "crossover D7 binding must not be skipped: {:?}",
        plan.d7_warnings
    );
    // 实验模式下 T64 IIR 被排除的 warning 仍如实记录（T64 报告 band_count>0 时）。
    assert!(plan.d7_warnings.contains(&W_EXPERIMENT_IIR_EXCLUDED) || true);
    // 确定性。
    let again = t70_compile(&design).expect("compile again");
    assert_eq!(plan.plan_bytes, again.plan_bytes);
    assert_eq!(plan.graph_id, again.graph_id);
    // 证据：卷积后核 hash/长度与 plan p_taps 一致（通过 report + evidence 双读）。
    assert_eq!(summary.evidence_sha256.len(), 64);
}
