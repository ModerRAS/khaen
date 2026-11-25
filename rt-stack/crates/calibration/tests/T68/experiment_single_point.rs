// T68（单点 G 实验旁路：measured 4.1.2 五执行器 + G 注入 + EXPERIMENT/NON_ACCEPTABLE 产物）私有单测。
//
// 经 src/compile_apply.rs 末尾 #[cfg(test)] mod t68_experiment_tests { include!(...) } 进入 lib 单测构建
//（cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator，勿写入 public_registration.rs）。
// 全部测试名 t68_ 前缀。运行：cargo test -p khaen-calibration t68_
//
// 覆盖（对应任务 §4 四个单测）：
//   ① t68_measured_412_five_actuators_builds_only_under_experiment
//   ② t68_experiment_g_injection_plan_paths_match_declared_g
//   ③ t68_experiment_artifact_status_still_fail_closed_without_flag
//   ④ t68_experiment_artifacts_carry_experiment_non_acceptable_markers
//   ⑤ t68_production_default_unchanged_no_experiment_fields（生产不污染）
//
// 纪律：全部离线（不打开 ALSA/CueMix、不碰板端）；产物只断言 EXPERIMENT/NON_ACCEPTABLE，
// 绝不写成 READY/PASS。

use std::fs;
use std::path::{Path, PathBuf};

use khaen_compat::ws::{json_object, JsonValue};
use khaen_modal_design::artifact::ArtifactStatus;

use crate::fit_wiring::CompileWiringConfig;
use crate::measured_design::{
    build_measured_design, build_measured_design_opts, g_paths_canonical_sha256, load_g_paths_file,
    InjectedGPaths, MeasuredBuildOptions, MeasuredChannelFile, MeasuredDesignSpec,
    MeasuredPositionSpec,
};

// 说明：本文件 include! 进 compile_apply 的 #[cfg(test)] mod（use super::*），下列名字直接来自该模块：
// FS/BLOCK 常量、GPathInput、CompileApplyRequest、CompileApplyOptions、MappingBinding、
// PhaseEvidence/PhaseChannelOp/PhaseFitChecks、HeadroomEvidence/RoleBoostRecord、
// ArtifactBinding、RoleCompileInput、RoleDacSlot、DelayPolarityEncoding、ExperimentMode、
// compile_plan、E_CA_ARTIFACT_STATUS、E_CA_EXPERIMENT、PEQ_FIT_SCHEMA、
// EXPERIMENT_* 常量，以及 p_row_chain_peak_gain_db/p_row_max_positive_db 私有 helper。

const T68_FS: u32 = 48_000;
const T68_BLOCK: u16 = 128;
const T68_L_G: u32 = 64;
const T68_L_P: u32 = 8;
const T68_ROLE_IDS: [&str; 5] = ["FL", "FR", "SL", "SR", "SUB1"];
const T68_ROLE_DACS: [u16; 5] = [0, 1, 2, 3, 7];
const T68_T64_PROGRAM_PEAK_DBFS: f64 = -18.0;

/// 测试临时目录（不用 std::env::temp_dir()：本机 TEMP 指到 R:\Temp，测试进程无写权限）。
fn t68_tmp_dir(tag: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let dir = base.join(format!("t68-tmp-{tag}-pid{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create t68 temp dir");
    dir
}

fn t68_sha_fill(seed: u8) -> String {
    let digit = std::char::from_digit((seed % 16) as u32, 16).unwrap_or('0');
    std::iter::repeat(digit).take(64).collect()
}

// ---------------------------------------------------------------------------
// 夹具 A：4.1.2 五执行器 measured spec（真实格式 analysis/diag 文件）
// ---------------------------------------------------------------------------

fn t68_write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("write fixture");
    path
}

/// 与旧工具同形的 analysis JSON（schema tspi-channel-measurement/v1；64 点 1/12 oct）。
fn t68_analysis_json(channel: u64, sr: u32, delay: f64) -> String {
    let mut fr = String::new();
    let mut index = 0;
    loop {
        let freq = 20.0 * 2f64.powf(index as f64 / 12.0);
        if freq > 20_000.0 {
            break;
        }
        let phase = -2.0 * std::f64::consts::PI * freq * 0.1 / sr as f64 * 180.0
            / std::f64::consts::PI;
        if !fr.is_empty() {
            fr.push(',');
        }
        fr.push_str(&format!(
            "{{\"freq_hz\":{freq:.6},\"db\":0.0,\"phase_deg\":{phase:.6}}}"
        ));
        index += 1;
    }
    format!(
        "{{\"schema\":\"tspi-channel-measurement/v1\",\"channel\":{channel},\"delay_samples\":{delay:.6},\"snr_band_db\":14.2,\"polarity_confidence\":\"low\",\"meta\":{{\"sample_rate\":{sr},\"sweep_start_hz\":20.0,\"sweep_end_hz\":20000.0}},\"fr\":[{fr}]}}"
    )
}

/// 4.1.2 五执行器 measured spec（FL/FR/SL/SR/SUB1；dense=[0,1,2,3,7]，unused=[4,5,6]）。
fn t68_measured_spec_412(dir: &Path) -> MeasuredDesignSpec {
    fs::create_dir_all(dir).expect("create temp dir");
    let mut channels = Vec::new();
    for (index, name) in T68_ROLE_IDS.iter().enumerate() {
        let analysis = t68_write(
            dir,
            &format!("{name}-ch{:02}.analysis.json", T68_ROLE_DACS[index]),
            &t68_analysis_json(T68_ROLE_DACS[index] as u64, T68_FS, 942.0 + index as f64),
        );
        let diag = t68_write(
            dir,
            &format!("{name}-ch{:02}.diag.json", T68_ROLE_DACS[index]),
            "{\"nominal_start\":120000,\"actual_start\":110186}",
        );
        channels.push(MeasuredChannelFile {
            name: (*name).to_string(),
            analysis,
            diag: Some(diag),
        });
    }
    let roles = T68_ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| {
            RoleDacSlot::new(
                index as u16,
                *role_id,
                T68_ROLE_DACS[index],
                vec![T68_ROLE_DACS[index]],
            )
            .expect("role fixture valid")
        })
        .collect();
    let mut compile = CompileWiringConfig::default();
    compile.block_size = T68_BLOCK;
    compile.n_in = 5;
    compile.container_channels = 8;
    compile.low_band_d = 0;
    compile.l_g = 8;
    compile.l_p = T68_L_P;
    compile.encoding = DelayPolarityEncoding::RoleMetadata;
    MeasuredDesignSpec {
        schema: crate::measured_design::MEASURED_SPEC_SCHEMA.to_string(),
        session_id: "t68-measured-412".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate_hz: T68_FS,
        target: "flat".to_string(),
        design_positions: vec![MeasuredPositionSpec {
            label: "D1_T68".to_string(),
            mic_orientation: "90deg".to_string(),
            weight: 1.0,
            channels,
        }],
        validation_positions: Vec::new(),
        validation: crate::measured_design::VALIDATION_PROXY_SAME_AS_DESIGN.to_string(),
        channel_bands: Vec::new(),
        dense_to_dac: T68_ROLE_DACS.to_vec(),
        roles,
        container_channels: 8,
        unused_outputs: vec![4, 5, 6],
        compile,
        modal_artifact: None,
        boost_cap_db: None,
        max_total_stages: None,
        low_band_trusted: Some(false),
        provenance: "t68_unit_test_fixture_only".to_string(),
        // T70 crossover 接线：MeasuredDesignSpec 新增可选 crossover 节（默认 None = 历史行为）。
        crossover: None,
    }
}

/// plan 里对角路径系数 = G_diag **+ delta_D baseline**（T03 §2.4；GPathInput.coeffs 不含 baseline）。
/// 这里按同一口径重算期望的 f32 canonical sha，用于和 compile-report 的 coeff_sha256 对拍。
fn t68_expected_plan_coeff_sha(injected: &GPathInput, d: u32) -> String {
    let mut coeffs = injected.coeffs.clone();
    if injected.diag_baseline {
        coeffs[d as usize] += 1.0;
    }
    let f32s: Vec<f32> = coeffs.iter().map(|value| *value as f32).collect();
    let mut canonical = String::from("tspi-plan-path-coeffs/v1\n");
    for value in &f32s {
        canonical.push_str(&format!("{value:e}\n"));
    }
    sha256_hex(canonical.as_bytes())
}

/// 25 条注入 G（dense 全覆盖；对角与非对角都非零，含可区分的 sha 口径）。
fn t68_injected_paths(l_g: u32) -> Vec<GPathInput> {
    let mut paths = Vec::with_capacity(25);
    for out_dense in 0..5u16 {
        for in_dense in 0..5u16 {
            let diag = out_dense == in_dense;
            let mut coeffs = vec![0.0_f64; l_g as usize];
            for (tap, value) in coeffs.iter_mut().enumerate() {
                *value = if diag {
                    if tap == 0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    0.001 * (out_dense as f64 * 5.0 + in_dense as f64 + 1.0)
                        * (0.5 + tap as f64 * 0.01)
                };
            }
            if diag && out_dense == 0 {
                coeffs[1] = 0.25;
            }
            let mut path = GPathInput::new(
                out_dense,
                in_dense,
                coeffs,
                diag,
                false,
                t68_sha_fill(0xd0 + out_dense as u8),
            )
            .expect("injected G path valid")
            .with_source(in_dense);
            path.coeff_sha256 = path.coeffs_f32_sha256().expect("f32 canonical hash");
            paths.push(path);
        }
    }
    paths
}

/// 写一个 tspi-cal-g-paths/v1 文件并用生产 loader 读回（覆盖注入文件解析路径）。
fn t68_g_paths_file(dir: &Path, l_g: u32) -> InjectedGPaths {
    let paths = t68_injected_paths(l_g);
    let entries: Vec<JsonValue> = paths
        .iter()
        .map(|path| {
            json_object(vec![
                ("out", JsonValue::UInt(path.out_dense as u64)),
                ("in", JsonValue::UInt(path.in_dense as u64)),
                (
                    "coeffs",
                    JsonValue::Array(
                        path.coeffs
                            .iter()
                            .map(|value| JsonValue::Number(*value))
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    let document = json_object(vec![
        ("schema", JsonValue::String("tspi-cal-g-paths/v1".to_string())),
        ("l_g", JsonValue::UInt(l_g as u64)),
        ("low_band_d", JsonValue::UInt(0)),
        (
            "participants",
            JsonValue::Array(
                T68_ROLE_IDS
                    .iter()
                    .map(|role| JsonValue::String((*role).to_string()))
                    .collect(),
            ),
        ),
        (
            "solver_note",
            JsonValue::String("t68 fixture probe G (NOT a deliverable solution)".to_string()),
        ),
        ("paths", JsonValue::Array(entries)),
    ]);
    let text = khaen_compat::ws::json_encode(&document).expect("encode g paths");
    let path = t68_write(dir, "g-paths.json", &text);
    load_g_paths_file(&path).expect("load g paths")
}

// ---------------------------------------------------------------------------
// 夹具 B：compile 级请求（4.1.2 五 participants；G 可注入；artifact 可 INCOMPLETE）
// ---------------------------------------------------------------------------

fn t68_mapping_412() -> MappingBinding {
    let roles = T68_ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| {
            RoleDacSlot::new(
                index as u16,
                *role_id,
                T68_ROLE_DACS[index],
                vec![T68_ROLE_DACS[index]],
            )
            .expect("role valid")
        })
        .collect();
    MappingBinding::new(
        "4.1.2",
        t68_sha_fill(0xa1),
        t68_sha_fill(0xa2),
        t68_sha_fill(0xa3),
        t68_sha_fill(0xa4),
        8,
        T68_ROLE_DACS.to_vec(),
        roles,
        vec![4, 5, 6],
        false,
        Vec::new(),
    )
    .expect("mapping 4.1.2 experimental (empty stereo_mask) valid")
}

fn t68_roles(l_g: u32) -> Vec<RoleCompileInput> {
    let delays = [1581u32, 0, 1135, 1837, 1353];
    let polarity = [1i8, 1, 1, -1, 1];
    T68_ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| {
            RoleCompileInput::new(
                index as u16,
                *role_id,
                T68_ROLE_DACS[index],
                Some(index as u16),
                delays[index],
                polarity[index],
                0.0,
                vec![1.0],
                0,
            )
            .expect("role valid")
        })
        .collect()
}

fn t68_phase() -> PhaseEvidence {
    let delays = [1581u32, 0, 1135, 1837, 1353];
    let polarity = [1i8, 1, 1, -1, 1];
    let ops: Vec<PhaseChannelOp> = T68_ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| PhaseChannelOp {
            channel: (*role_id).to_string(),
            index: Some(T68_ROLE_DACS[index] as u32),
            delay_samples: delays[index],
            delay_ms: delays[index] as f64 / T68_FS as f64 * 1000.0,
            polarity: if polarity[index] < 0 {
                PhasePolarity::Invert
            } else {
                PhasePolarity::Normal
            },
        })
        .collect();
    let checks = PhaseFitChecks {
        integer_delay_contract: true,
        min_zero: true,
        max_within_cap: true,
        reference_is_latest: true,
        fractional_residual_within_half_sample: true,
        polarity_known: true,
        fir_design_ok: true,
        fir_direction_ok: true,
        all_pass: true,
    };
    // reference 必须是最小延迟通道（min_zero 检查）：fixture 里 FR=0 最小。
    PhaseEvidence::from_ops(&ops, &checks, t68_sha_fill(0xc1), t68_sha_fill(0xc2), "FR", 0.0)
}

fn t68_t64_report_channel(name: &str, p_fir: &[f64]) -> JsonValue {
    let gain_db = p_row_chain_peak_gain_db(p_fir, T68_FS as f64);
    let max_positive_db = p_row_max_positive_db(p_fir, T68_FS as f64).max(0.0);
    let predicted = T68_T64_PROGRAM_PEAK_DBFS + gain_db;
    json_object(vec![
        ("name", JsonValue::String(name.to_string())),
        ("band_count", JsonValue::UInt(0)),
        (
            "headroom",
            json_object(vec![
                (
                    "program_peak_reference_dbfs",
                    JsonValue::Number(T68_T64_PROGRAM_PEAK_DBFS),
                ),
                (
                    "predicted_peak_before_trim_dbfs",
                    JsonValue::Number(predicted),
                ),
                ("trim_db", JsonValue::Number(0.0)),
                ("predicted_peak_dbfs", JsonValue::Number(predicted)),
                ("headroom_db", JsonValue::Number(-predicted)),
                ("pass", JsonValue::Bool(true)),
            ]),
        ),
        (
            "boost",
            json_object(vec![
                ("single_filter_max_db", JsonValue::Number(max_positive_db)),
                ("filter_positive_sum_db", JsonValue::Number(max_positive_db)),
                ("limits_pass", JsonValue::Bool(true)),
            ]),
        ),
        (
            "target_fir",
            json_object(vec![(
                "max_positive_db",
                JsonValue::Number(max_positive_db),
            )]),
        ),
    ])
}

fn t68_headroom(roles: &[RoleCompileInput]) -> HeadroomEvidence {
    let mut records = Vec::with_capacity(roles.len());
    let mut specs: Vec<JsonValue> = Vec::with_capacity(roles.len());
    for role in roles {
        let gain_db = p_row_chain_peak_gain_db(&role.p_fir, T68_FS as f64);
        let max_positive_db = p_row_max_positive_db(&role.p_fir, T68_FS as f64).max(0.0);
        let predicted_peak = T68_T64_PROGRAM_PEAK_DBFS + gain_db;
        records.push(
            RoleBoostRecord::new(
                role.role_id.clone(),
                max_positive_db,
                max_positive_db,
                predicted_peak,
                -predicted_peak,
                true,
            )
            .expect("headroom record valid"),
        );
        specs.push(t68_t64_report_channel(&role.role_id, &role.p_fir));
    }
    let document = json_object(vec![
        ("schema", JsonValue::String(PEQ_FIT_SCHEMA.to_string())),
        ("channels", JsonValue::Array(specs)),
    ]);
    let report_bytes = khaen_compat::ws::json_encode(&document)
        .expect("encode T64 report fixture")
        .into_bytes();
    let report_sha256 = sha256_hex(&report_bytes);
    HeadroomEvidence::new(PEQ_FIT_SCHEMA, report_sha256, records)
        .with_t64_report_bytes(report_bytes)
}

fn t68_artifact(status: ArtifactStatus) -> ArtifactBinding {
    ArtifactBinding::from_parts(
        t68_sha_fill(0xe1),
        t68_sha_fill(0xe2),
        t68_sha_fill(0xe3),
        t68_sha_fill(0xe4),
        t68_sha_fill(0xe5),
        t68_sha_fill(0xe6),
        t68_sha_fill(0xe7),
        status.as_str(),
    )
}

struct T68Fixture {
    mapping: MappingBinding,
    phase: PhaseEvidence,
    headroom: HeadroomEvidence,
    artifact: ArtifactBinding,
    paths: Vec<GPathInput>,
    roles: Vec<RoleCompileInput>,
    options: CompileApplyOptions,
    experiment: ExperimentMode,
    notes: Vec<String>,
    l_g: u32,
}

impl T68Fixture {
    fn new(status: ArtifactStatus, experiment: ExperimentMode, l_g: u32) -> Self {
        let roles = t68_roles(l_g);
        Self {
            mapping: t68_mapping_412(),
            phase: t68_phase(),
            headroom: t68_headroom(&roles),
            artifact: t68_artifact(status),
            paths: t68_injected_paths(l_g),
            roles,
            options: CompileApplyOptions::default(),
            experiment,
            notes: vec!["T64 rule_2a FAIL (fixture)".to_string()],
            l_g,
        }
    }

    fn request(&self) -> CompileApplyRequest<'_> {
        CompileApplyRequest::new(
            T68_FS,
            T68_BLOCK,
            5,
            8,
            0,
            self.l_g,
            T68_L_P,
            &self.mapping,
            &self.phase,
            &self.headroom,
            &self.artifact,
            &self.paths,
            &self.roles,
            DelayPolarityEncoding::RoleMetadata,
            self.options,
        )
        .with_experiment(self.experiment)
        .with_experiment_notes(self.notes.clone())
    }
}

// ---------------------------------------------------------------------------
// ① 4.1.2 五执行器 measured build：生产默认拒绝；实验模式能 build
// ---------------------------------------------------------------------------

#[test]
fn t68_measured_412_five_actuators_builds_only_under_experiment() {
    let dir = t68_tmp_dir("measured");
    let spec = t68_measured_spec_412(&dir);

    // 生产默认（experiment OFF）：非 stereo layout 仍传 participants => stereo_mask 门如实拒绝。
    let error = build_measured_design(&spec).expect_err("production 4.1.2 must stay rejected");
    assert!(
        error.contains(crate::measured_design::E_MEASURED_SPEC)
            && error.contains("stereo_mask"),
        "unexpected production error: {error}"
    );

    // 实验模式（显式 flag）：允许空 stereo_mask，4.1.2 五执行器能 build。
    let options = MeasuredBuildOptions {
        experiment: ExperimentMode::SINGLE_POINT,
        layout_override: Some("4.1.2".to_string()),
        g_paths: None,
    };
    let built = build_measured_design_opts(&spec, &options).expect("experimental 4.1.2 must build");
    assert_eq!(built.effective_layout, "4.1.2");
    assert!(built.experiment.is_active());
    assert_eq!(built.design.mapping.layout_id, "4.1.2");
    assert!(!built.design.mapping.stereo_strict);
    assert!(built.design.mapping.stereo_mask.is_empty());
    assert_eq!(built.design.mapping.dense_to_dac.len(), 5);
    assert_eq!(built.design.g_paths.len(), 25);
    assert!(!built.design.artifact.is_ready(), "单点 artifact 恒 INCOMPLETE（不伪造 READY）");
    assert_eq!(built.design.artifact.status, "INCOMPLETE");
    assert!(built.g_source.contains("identity_baseline"));
    assert!(built.summary_json.contains("EXPERIMENT"));
    assert!(built.summary_json.contains("NON_ACCEPTABLE"));
    assert!(built.summary_json.contains("SINGLE_POINT_NOT_SPATIAL"));
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// ② G 注入：plan 的 graph/taps/g_paths 与声明一致
// ---------------------------------------------------------------------------

#[test]
fn t68_experiment_g_injection_plan_paths_match_declared_g() {
    let dir = t68_tmp_dir("gpath");
    let injected = t68_g_paths_file(&dir, T68_L_G);
    assert_eq!(injected.l_g, T68_L_G);
    assert_eq!(injected.paths.len(), 25);
    assert_eq!(
        g_paths_canonical_sha256(&injected.paths),
        g_paths_canonical_sha256(&t68_injected_paths(T68_L_G)),
        "loader 往返必须保持 G 路径表逐位一致"
    );

    let fixture = T68Fixture::new(ArtifactStatus::Incomplete, ExperimentMode::SINGLE_POINT, T68_L_G);
    let compiled = compile_plan(&fixture.request()).expect("experimental compile with injected G");

    // graph：graph_id 非零 + plan 字节可被自身 verify。
    assert!(compiled.graph_id.iter().any(|byte| *byte != 0));
    compiled.verify().expect("plan self-verify");

    // taps / l_g：与声明一致。
    assert_eq!(compiled.report.l_g, T68_L_G);
    assert_eq!(compiled.report.l_p, T68_L_P);
    let mut paths = compiled.report.paths.clone();
    paths.sort_by_key(|path| (path.out_index, path.in_index));
    assert_eq!(paths.len(), 25, "25 条非零 G cell 必须全部进 plan");
    for path in &paths {
        assert_eq!(path.taps, T68_L_G, "path taps 必须 = 声明 L_G");
    }

    // g_paths：逐 cell 与注入 G 一致（对角路径按 plan 语义 = G_diag + delta_D baseline）。
    for injected_path in &injected.paths {
        let expected_sha = t68_expected_plan_coeff_sha(injected_path, 0);
        let record = paths
            .iter()
            .find(|path| {
                path.out_role == T68_ROLE_IDS[injected_path.out_dense as usize]
                    && path.in_role == T68_ROLE_IDS[injected_path.in_dense as usize]
            })
            .unwrap_or_else(|| {
                panic!(
                    "plan 缺 G cell {}/{}",
                    injected_path.out_dense, injected_path.in_dense
                )
            });
        assert_eq!(
            record.coeff_sha256, expected_sha,
            "plan path {}/{} 系数与注入 G 不一致",
            injected_path.out_dense, injected_path.in_dense
        );
        assert_eq!(
            record.diag_baseline, injected_path.diag_baseline,
            "diag_baseline 标记必须与注入 G 的 out==in 一致"
        );
    }

    // 非对角必须有内容（证明不是 identity baseline 被硬接）。
    let offdiag_nonzero = injected
        .paths
        .iter()
        .filter(|path| !path.diag_baseline)
        .filter(|path| path.coeffs.iter().any(|value| *value != 0.0))
        .count();
    assert_eq!(offdiag_nonzero, 20, "注入 G 的 20 条非对角 cell 必须非零");

    // envelope：plan sha / graph id / l_g / path 数与 plan 一致。
    let envelope = compiled.experiment_envelope_json();
    let document = khaen_compat::ws::json_parse(&envelope).expect("envelope JSON");
    assert_eq!(
        document.get("plan").and_then(|value| value.get("l_g")).and_then(|value| value.as_u64()),
        Some(T68_L_G as u64)
    );
    assert_eq!(
        document
            .get("plan")
            .and_then(|value| value.get("path_count"))
            .and_then(|value| value.as_u64()),
        Some(25)
    );
    assert_eq!(
        document
            .get("plan")
            .and_then(|value| value.get("plan_sha256"))
            .and_then(|value| value.as_str()),
        Some(compiled.plan_sha256().as_str())
    );
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// ③ 不带 allow-incomplete-artifact：仍 fail-closed（E_CA_ARTIFACT_STATUS）
// ---------------------------------------------------------------------------

#[test]
fn t68_experiment_artifact_status_still_fail_closed_without_flag() {
    // (a) 生产默认（experiment OFF）+ INCOMPLETE → E_CA_ARTIFACT_STATUS。
    let fixture = T68Fixture::new(ArtifactStatus::Incomplete, ExperimentMode::OFF, T68_L_G);
    let error = compile_plan(&fixture.request()).expect_err("production must reject INCOMPLETE");
    assert_eq!(error.code(), E_CA_ARTIFACT_STATUS);

    // (b) 只开 single_point、不开 allow_incomplete_artifact → 仍 E_CA_ARTIFACT_STATUS。
    let fixture = T68Fixture::new(
        ArtifactStatus::Incomplete,
        ExperimentMode {
            single_point: true,
            multipoint_pilot: false,
            allow_incomplete_artifact: false,
        },
        T68_L_G,
    );
    let error = compile_plan(&fixture.request()).expect_err("single_point alone must still reject");
    assert_eq!(error.code(), E_CA_ARTIFACT_STATUS);

    // (c) 组合非法（allow_incomplete 单独开启）→ E_CA_EXPERIMENT（fail-closed）。
    let fixture = T68Fixture::new(
        ArtifactStatus::Incomplete,
        ExperimentMode {
            single_point: false,
            multipoint_pilot: false,
            allow_incomplete_artifact: true,
        },
        T68_L_G,
    );
    let error = compile_plan(&fixture.request()).expect_err("allow_incomplete alone must reject");
    assert_eq!(error.code(), E_CA_EXPERIMENT);
}

// ---------------------------------------------------------------------------
// ④ 带 flag：产物字段 EXPERIMENT / NON_ACCEPTABLE / applied=false
// ---------------------------------------------------------------------------

#[test]
fn t68_experiment_artifacts_carry_experiment_non_acceptable_markers() {
    let fixture = T68Fixture::new(ArtifactStatus::Incomplete, ExperimentMode::SINGLE_POINT, T68_L_G);
    let compiled = compile_plan(&fixture.request()).expect("experimental compile");

    // compile-report.json 明确字段。
    let report = compiled.report_json();
    let document = khaen_compat::ws::json_parse(&report).expect("report JSON");
    let experiment = document
        .get("experiment")
        .expect("report must carry experiment block");
    for (key, expected) in [
        ("verdict", "EXPERIMENT"),
        ("acceptability", "NON_ACCEPTABLE"),
        ("scope", "SINGLE_POINT_NOT_SPATIAL"),
    ] {
        assert_eq!(
            experiment.get(key).and_then(|value| value.as_str()),
            Some(expected)
        );
    }
    assert_eq!(
        experiment.get("active").and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        document.get("applied").and_then(|value| value.as_bool()),
        Some(false),
        "实验报告不得写 applied=true"
    );
    assert_eq!(
        document
            .get("checks")
            .and_then(|value| value.get("artifact_ready"))
            .and_then(|value| value.as_bool()),
        Some(false),
        "artifact 非 READY 时不得写 artifact_ready=true"
    );
    assert!(
        report.contains("EXPERIMENT") && report.contains("NON_ACCEPTABLE"),
        "report JSON 缺实验标签"
    );

    // experiment envelope artifact（仅实验模式产）。
    let envelope = compiled.experiment_artifact().expect("experiment envelope artifact");
    assert_eq!(
        envelope.path,
        crate::compile_apply::EXPERIMENT_ENVELOPE_ARTIFACT_PATH
    );
    assert_eq!(envelope.sha256, sha256_hex(&envelope.bytes));
    let envelope_json =
        String::from_utf8(envelope.bytes.clone()).expect("envelope is UTF-8 JSON");
    assert!(envelope_json.contains("\"applied\": false"));
    assert!(envelope_json.contains("\"certifiable\": false"));
    assert!(envelope_json.contains("\"t64_correction_in_plan\": false"));
    assert!(envelope_json.contains("SINGLE_POINT_NOT_SPATIAL"));

    // 生产（experiment OFF）不产 envelope，也不得出现 EXPERIMENT 字段。
    let production = T68Fixture::new(ArtifactStatus::Ready, ExperimentMode::OFF, T68_L_G);
    let production_plan = compile_plan(&production.request()).expect("production compile");
    assert!(production_plan.experiment_artifact().is_none());
    let production_report = production_plan.report_json();
    assert!(!production_report.contains("EXPERIMENT"));
    assert!(!production_report.contains("NON_ACCEPTABLE"));
}
