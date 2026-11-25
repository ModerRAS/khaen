// T66（cal-compiler：P/G 每 path FIR 编译 -> prepare/apply ack -> last-good 回滚）私有单测。
//
// 本文件经 src/compile_apply.rs 末尾的 #[cfg(test)] mod t66_tests { include!(...) } 进入
// lib 单测构建（cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator，勿写入
// public_registration.rs）。全部测试名 t66_ 前缀。
// 运行：cargo test -p khaen-calibration compile_apply
// 离线：不打开 ALSA/CueMix，不碰板端/S1，不启动 Camilla；admission 用 MockMemory 注入。

use khaen_modal_design::artifact::encode_wav_f32_mono;
use khaen_rt_host::admission::MockMemory;

// ---------------------------------------------------------------------------
// 夹具：4.1.2 七角色 / 五 participants（FL FR SL SR SUB1）+ stereo strict 两角色
// ---------------------------------------------------------------------------

const FS: u32 = 48_000;
const BLOCK: u16 = 128;
const L_G: u32 = 16;
const L_P: u32 = 16;
const D_BASE: u32 = 4;
const ROLE_IDS: [&str; 7] = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];
const ROLE_DACS: [u16; 7] = [0, 1, 2, 3, 4, 5, 7];
/// participants 的 role 序号（dense 顺序 = G 行列顺序）。
const PARTICIPANT_ROLES: [u16; 5] = [0, 1, 2, 3, 6];
/// participants 的源通道（fixture 里 role i 的 source = i）。
const PARTICIPANT_SOURCES: [u16; 5] = [0, 1, 2, 3, 6];

fn sha_fill(seed: u8) -> String {
    let digit = std::char::from_digit((seed % 16) as u32, 16).unwrap_or('0');
    std::iter::repeat(digit).take(64).collect()
}

fn g_paths_412() -> Vec<GPathInput> {
    let mut paths = Vec::with_capacity(25);
    for out_dense in 0..5u16 {
        for in_dense in 0..5u16 {
            let diag = out_dense == in_dense;
            let mut coeffs = vec![0.0_f64; L_G as usize];
            if diag {
                coeffs[0] = 1.0;
                coeffs[2] = 0.05;
            } else {
                coeffs[1] = 0.01 * ((out_dense * 5 + in_dense) as f64 + 1.0);
            }
            let path = GPathInput::new(
                out_dense,
                in_dense,
                coeffs,
                diag,
                false,
                sha_fill(0xb0 + out_dense as u8),
            )
            .expect("G path fixture must be valid");
            paths.push(path.with_source(PARTICIPANT_SOURCES[in_dense as usize]));
        }
    }
    paths
}

fn roles_412(encoding: DelayPolarityEncoding) -> Vec<RoleCompileInput> {
    ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| {
            let delay = match index {
                0 => 0,
                1 => 2,
                2 => 1,
                3 => 3,
                4 => 0,
                5 => 2,
                _ => 4,
            };
            let polarity = if index == 3 { -1 } else { 1 };
            let p_fir = if index == 0 {
                vec![1.0, 0.1, -0.05]
            } else if encoding == DelayPolarityEncoding::CoeffEmbedded {
                vec![1.0]
            } else {
                vec![1.0]
            };
            RoleCompileInput::new(
                index as u16,
                *role_id,
                ROLE_DACS[index],
                Some(index as u16),
                delay,
                polarity,
                -1.5,
                p_fir,
                0,
            )
            .expect("role fixture must be valid")
        })
        .collect()
}

fn phase_412() -> PhaseEvidence {
    let delays = [0u32, 2, 1, 3, 0, 2, 4];
    let ops: Vec<PhaseChannelOp> = ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| PhaseChannelOp {
            channel: (*role_id).to_string(),
            index: Some(ROLE_DACS[index] as u32),
            delay_samples: delays[index],
            delay_ms: delays[index] as f64 / FS as f64 * 1000.0,
            polarity: if index == 3 {
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
    PhaseEvidence::from_ops(&ops, &checks, sha_fill(0xc1), sha_fill(0xc2), "FL", 0.0)
}

/// F-35：headroom 证据必须绑定 T64 报告字节；夹具由 roles 的 P FIR 生成同形 T64 报告并
/// 用其 sha256 绑定（helper 见文件末尾 cursor-overflow/F-35 段）。
fn headroom_412(roles: &[RoleCompileInput]) -> HeadroomEvidence {
    headroom_with_bound_t64_report(roles, T64_REPORT_PROGRAM_PEAK_DBFS)
}

fn artifact_binding() -> ArtifactBinding {
    ArtifactBinding::from_parts(
        sha_fill(0xe1),
        sha_fill(0xe2),
        sha_fill(0xe3),
        sha_fill(0xe4),
        sha_fill(0xe5),
        sha_fill(0xe6),
        sha_fill(0xe7),
        ArtifactStatus::Ready.as_str(),
    )
}

fn mapping_412() -> MappingBinding {
    let roles: Vec<RoleDacSlot> = ROLE_IDS
        .iter()
        .enumerate()
        .map(|(index, role_id)| {
            RoleDacSlot::new(
                index as u16,
                *role_id,
                ROLE_DACS[index],
                vec![ROLE_DACS[index]],
            )
            .expect("role dac fixture must be valid")
        })
        .collect();
    MappingBinding::new(
        "4.1.2",
        sha_fill(0xa1),
        sha_fill(0xa2),
        sha_fill(0xa3),
        sha_fill(0xa4),
        8,
        vec![0, 1, 2, 3, 7],
        roles,
        vec![6],
        false,
        Vec::new(),
    )
    .expect("mapping 4.1.2 fixture must be valid")
}

struct Fixture {
    mapping: MappingBinding,
    phase: PhaseEvidence,
    headroom: HeadroomEvidence,
    artifact: ArtifactBinding,
    paths: Vec<GPathInput>,
    roles: Vec<RoleCompileInput>,
    encoding: DelayPolarityEncoding,
    options: CompileApplyOptions,
    n_in: u16,
    container: u16,
    l_g: u32,
    l_p: u32,
    d: u32,
}

impl Fixture {
    fn multichannel(encoding: DelayPolarityEncoding) -> Self {
        let roles = roles_412(encoding);
        Self {
            mapping: mapping_412(),
            phase: phase_412(),
            headroom: headroom_412(&roles),
            artifact: artifact_binding(),
            paths: g_paths_412(),
            roles,
            encoding,
            options: CompileApplyOptions::default(),
            n_in: 7,
            container: 8,
            l_g: L_G,
            l_p: L_P,
            d: D_BASE,
        }
    }

    fn stereo(encoding: DelayPolarityEncoding) -> Self {
        let roles = vec![
            RoleCompileInput::new(0, "FL", 0, Some(0), 0, 1, -1.0, vec![1.0, 0.05], 0)
                .expect("stereo FL role"),
            RoleCompileInput::new(1, "FR", 1, Some(1), 0, 1, -1.0, vec![1.0, -0.05], 0)
                .expect("stereo FR role"),
        ];
        let mapping_roles = vec![
            RoleDacSlot::new(0, "FL", 0, vec![0]).expect("stereo FL dac"),
            RoleDacSlot::new(1, "FR", 1, vec![1]).expect("stereo FR dac"),
        ];
        let mapping = MappingBinding::new(
            "stereo-2.0",
            sha_fill(0xa5),
            sha_fill(0xa6),
            sha_fill(0xa7),
            sha_fill(0xa8),
            8,
            vec![0, 1],
            mapping_roles,
            vec![2, 3, 4, 5, 6, 7],
            true,
            vec!["FL".to_string(), "FR".to_string()],
        )
        .expect("stereo mapping fixture");
        let mut paths = Vec::new();
        for out_dense in 0..2u16 {
            for in_dense in 0..2u16 {
                let diag = out_dense == in_dense;
                let mut coeffs = vec![0.0_f64; 8];
                if diag {
                    coeffs[0] = 1.0;
                } else {
                    coeffs[1] = 0.01;
                }
                paths.push(
                    GPathInput::new(
                        out_dense,
                        in_dense,
                        coeffs,
                        diag,
                        false,
                        sha_fill(0xb8 + out_dense as u8),
                    )
                    .expect("stereo G path")
                    .with_source(in_dense),
                );
            }
        }
        let ops: Vec<PhaseChannelOp> = ["FL", "FR"]
            .iter()
            .enumerate()
            .map(|(index, role_id)| PhaseChannelOp {
                channel: (*role_id).to_string(),
                index: Some(index as u32),
                delay_samples: 0,
                delay_ms: 0.0,
                polarity: PhasePolarity::Normal,
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
        let phase =
            PhaseEvidence::from_ops(&ops, &checks, sha_fill(0xc3), sha_fill(0xc4), "FL", 0.0);
        // F-35：headroom 证据绑定 T64 报告字节（由 P FIR 生成同形报告 + sha256）。
        let headroom = headroom_with_bound_t64_report(&roles, T64_REPORT_PROGRAM_PEAK_DBFS);
        Self {
            mapping,
            phase,
            headroom,
            artifact: artifact_binding(),
            paths,
            roles,
            encoding,
            options: CompileApplyOptions::default(),
            n_in: 2,
            container: 8,
            l_g: 8,
            l_p: 8,
            d: 0,
        }
    }

    fn request(&self) -> CompileApplyRequest<'_> {
        CompileApplyRequest::new(
            FS,
            BLOCK,
            self.n_in,
            self.container,
            self.d,
            self.l_g,
            self.l_p,
            &self.mapping,
            &self.phase,
            &self.headroom,
            &self.artifact,
            &self.paths,
            &self.roles,
            self.encoding,
            self.options,
        )
    }
}

fn env_4w() -> AdmissionEnv {
    let mut env = AdmissionEnv::new();
    env.sample_rate_hz = FS;
    env.block_size = BLOCK;
    env.rt_workers = RT_WORKERS;
    env.rt_privilege_ok = true;
    env.clock_source_ok = true;
    env.cpu_set_ok = true;
    env
}

fn assert_code(error: &CompileApplyError, code: &str) {
    assert_eq!(error.code(), code, "unexpected error: {error}");
}

fn compile_ok(fixture: &Fixture) -> CompiledPlan {
    compile_plan(&fixture.request()).expect("compile must succeed")
}

// ---------------------------------------------------------------------------
// 编译：P/G 每 path FIR、form、hash、确定性
// ---------------------------------------------------------------------------

#[test]
fn t66_compile_multichannel_paths_diags_and_roles() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    // 5x5 participants 非零 offdiag + 7 条对角 baseline = 27 path；P 行 7 条 diag。
    assert_eq!(plan.paths.len(), 27, "path 表条数");
    assert_eq!(plan.diags.len(), 7, "diag 表条数");
    assert_eq!(plan.roles.len(), 7, "role 表条数");
    assert_eq!(plan.header.low_band_count, 5);
    assert_eq!(plan.header.n_out, 8);
    assert_eq!(plan.header.n_in, 7);
    assert_eq!(plan.header.plan_version, PLAN_VERSION_UNASSIGNED);
    assert!(plan.header.flags & PLAN_FLAG_STEREO_STRICT == 0);
    // 对角 path 含 baseline D；非对角 delay=0。
    for path in &plan.paths {
        if path.flags & PATH_FLAG_DIAG_BASELINE_INCLUDED != 0 {
            assert_eq!(path.delay_samples, D_BASE);
        } else {
            assert_eq!(path.delay_samples, 0);
        }
    }
    // role 表按 role_id 升序、DAC 与 overlay 一致。
    for (index, role) in plan.roles.iter().enumerate() {
        assert_eq!(role.role_id, index as u16);
        assert_eq!(role.out_index, ROLE_DACS[index]);
        assert_eq!(role.peq_count, 0);
        assert_eq!(role.polarity, if index == 3 { -1 } else { 1 });
        assert_eq!(role.delay_samples, fixture.roles[index].delay_samples);
    }
    plan.verify().expect("compiled plan must self-verify");
}

#[test]
fn t66_compile_graph_id_deterministic_and_content_sensitive() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let first = compile_ok(&fixture);
    let second = compile_ok(&fixture);
    assert_eq!(first.graph_id, second.graph_id, "同输入 graph_id 必须一致");
    assert_eq!(first.plan_bytes, second.plan_bytes, "同输入 plan 字节确定");
    assert_eq!(first.report_json(), second.report_json());

    let mut changed = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    changed.paths[0].coeffs[1] += 0.25;
    let third = compile_plan(&changed.request()).expect("changed g must compile");
    assert_ne!(
        first.graph_id, third.graph_id,
        "g 系数变化必须改变 graph_id"
    );

    let mut retimed = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    retimed.d = D_BASE + 1;
    let fourth = compile_plan(&retimed.request()).expect("changed D must compile");
    assert_ne!(
        first.graph_id, fourth.graph_id,
        "baseline D 变化必须改变 graph_id"
    );
}

#[test]
fn t66_compile_report_binds_mapping_phase_artifact_hashes() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let report = &plan.report;
    assert_eq!(report.mapping_hash, fixture.mapping.mapping_hash());
    assert_eq!(report.phase_hash, fixture.phase.phase_hash());
    assert_eq!(report.headroom_hash, fixture.headroom.headroom_hash());
    assert_eq!(report.artifact_hash, fixture.artifact.artifact_hash);
    assert_eq!(report.host_graph_sha256, plan.graph_id_hex());
    assert_eq!(report.plan_sha256, plan.plan_sha256());
    assert_eq!(report.paths.len(), plan.paths.len());
    assert_eq!(report.roles.len(), plan.roles.len());
    assert!(is_sha256_hex(&report.mapping_hash));
    assert!(is_sha256_hex(&report.phase_hash));
    assert!(is_sha256_hex(&report.artifact_manifest_sha256));
    let json = plan.report_json();
    assert!(json.contains("\"compiled\": true"));
    assert!(json.contains("\"applied\": false"));
    assert!(json.contains("\"requires_post_apply_remeasurement\": true"));
    assert!(json.contains("\"camilla_involved\": false"));
    assert!(json.contains(&plan.graph_id_hex()));
}

#[test]
fn t66_compile_artifacts_are_stable_and_path_registered() {
    let fixture = Fixture::stereo(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let artifacts = plan.artifacts();
    assert_eq!(artifacts.plan.path, PLAN_ARTIFACT_PATH);
    assert_eq!(artifacts.report.path, COMPILE_REPORT_ARTIFACT_PATH);
    assert_eq!(artifacts.plan.bytes, plan.plan_bytes);
    assert_eq!(artifacts.plan.sha256, plan.plan_sha256());
    assert_eq!(artifacts.report.sha256, sha256_hex(&artifacts.report.bytes));
}

#[test]
fn t66_compile_plan_roundtrips_through_t20_registry() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let mut registry = PlanRegistry::new();
    let mut memory = MockMemory::new();
    let version = registry
        .admit_with(plan.token(), &env_4w(), &mut memory)
        .expect("T20 registry must accept compiled plan bytes");
    assert_eq!(version, 1);
    let view = registry.plan_view(version).expect("admitted plan view");
    assert_eq!(view.paths.len(), plan.paths.len());
    assert_eq!(view.diags.len(), plan.diags.len());
    assert_eq!(view.roles.len(), plan.roles.len());
    assert_eq!(view.header.graph_id, plan.graph_id);
    assert_eq!(view.coeffs.len(), plan.coeffs.len());
}

#[test]
fn t66_compile_stereo_strict_flags_and_zero_fill() {
    let fixture = Fixture::stereo(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    assert!(plan.header.flags & PLAN_FLAG_STEREO_STRICT != 0);
    assert_eq!(plan.paths.len(), 4, "stereo 2x2 path");
    assert_eq!(plan.roles.len(), 2);
    assert_eq!(
        plan.report.stereo_mask,
        vec!["FL".to_string(), "FR".to_string()]
    );
}

// ---------------------------------------------------------------------------
// 编译拒绝：坏图 / stale / hash / D7 / ABI 缺口
// ---------------------------------------------------------------------------

#[test]
fn t66_reject_artifact_incomplete_or_stale() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.artifact.status = "INCOMPLETE".to_string();
    let error = compile_plan(&fixture.request()).expect_err("INCOMPLETE artifact must be rejected");
    assert_code(&error, E_CA_ARTIFACT_STATUS);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.artifact.stale_reasons = vec!["STALE_SOLUTION"];
    let error = compile_plan(&fixture.request()).expect_err("stale artifact must be rejected");
    assert_code(&error, E_CA_ARTIFACT_STALE);
}

#[test]
fn t66_reject_missing_mapping_hash() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.mapping.binding_hash = String::new();
    let error =
        compile_plan(&fixture.request()).expect_err("missing mapping hash must be rejected");
    assert_code(&error, E_CA_MAPPING_HASH);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.mapping.role_hash = "deadbeef".to_string();
    let error = compile_plan(&fixture.request()).expect_err("short role hash must be rejected");
    assert_code(&error, E_CA_MAPPING_HASH);
}

#[test]
fn t66_reject_mapping_role_dac_drift() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[0].out_channel = 3;
    let error = compile_plan(&fixture.request()).expect_err("role/dac drift must be rejected");
    assert_code(&error, E_CA_MAPPING);
}

#[test]
fn t66_reject_phase_direction_wrong() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.phase.direction_ok = false;
    let error =
        compile_plan(&fixture.request()).expect_err("wrong phase direction must be rejected");
    assert_code(&error, E_CA_PHASE_DIRECTION);
}

#[test]
fn t66_reject_phase_residual_over_tolerance() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.phase.residual_max_samples = 0.75;
    let error = compile_plan(&fixture.request()).expect_err("residual over 0.5 must be rejected");
    assert_code(&error, E_CA_PHASE_RESIDUAL);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.phase.tolerance_samples = 0.75;
    let error = compile_plan(&fixture.request()).expect_err("tolerance over 0.5 must be rejected");
    assert_code(&error, E_CA_PHASE);
}

#[test]
fn t66_reject_phase_role_ops_mismatch() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[1].delay_samples += 1;
    let error =
        compile_plan(&fixture.request()).expect_err("role/T65 delay mismatch must be rejected");
    assert_code(&error, E_CA_PHASE);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[1].polarity = -1;
    let error =
        compile_plan(&fixture.request()).expect_err("role/T65 polarity mismatch must be rejected");
    assert_code(&error, E_CA_PHASE);
}

#[test]
fn t66_reject_phase_hash_missing() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.phase.ops_sha256 = "not-a-hash".to_string();
    let error = compile_plan(&fixture.request()).expect_err("missing phase hash must be rejected");
    assert_code(&error, E_CA_PHASE_HASH);
}

#[test]
fn t66_reject_d7_boost_positive_sum_peak() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom.roles[0].single_boost_db = 3.5;
    let error = compile_plan(&fixture.request()).expect_err("boost > +3 dB must be rejected");
    assert_code(&error, E_CA_D7_BOOST);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom.roles[0].positive_sum_db = 13.0;
    let error =
        compile_plan(&fixture.request()).expect_err("positive sum > 12 dB must be rejected");
    assert_code(&error, E_CA_D7_POSITIVE_SUM);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom.roles[0].predicted_peak_dbfs = -4.0;
    let error =
        compile_plan(&fixture.request()).expect_err("predicted peak > -6 dBFS must be rejected");
    assert_code(&error, E_CA_D7_PEAK);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom.roles[0].headroom_db = 3.0;
    let error = compile_plan(&fixture.request()).expect_err("headroom < 6 dB must be rejected");
    assert_code(&error, E_CA_D7_PEAK);
}

#[test]
fn t66_accept_d7_positive_sum_warning() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom.roles[0].positive_sum_db = 7.0;
    let plan =
        compile_plan(&fixture.request()).expect("7 dB positive sum is a warning, not reject");
    assert!(plan.d7_warnings.contains(&W_D7_POSITIVE_SUM_WARN));
    assert!(plan.report.d7_warnings.contains(&W_D7_POSITIVE_SUM_WARN));
}

#[test]
fn t66_reject_plan_abi_gaps() {
    // GAP-T66-3：plan ABI v1.0 无 role IIR 系数区 -> peq_count>0 拒收，禁止静默丢弃 PEQ。
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[0].peq_count = 1;
    let error = compile_plan(&fixture.request()).expect_err("peq_count>0 must be rejected");
    assert_code(&error, E_CA_PEQ_ABI);

    // GAP-T66-4：role.gain_q15 只能表示 <=1.0 线性增益（0 dB 最大）。
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[0].gain_db = 1.0;
    let error = compile_plan(&fixture.request()).expect_err("positive role gain must be rejected");
    assert_code(&error, E_CA_GAIN_RANGE);
}

#[test]
fn t66_reject_non_finite_and_length_errors() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.paths[0].coeffs[3] = f64::NAN;
    let error = compile_plan(&fixture.request()).expect_err("NaN coefficient must be rejected");
    assert_code(&error, E_CA_NUMERIC);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.paths[0].coeffs.pop();
    let error = compile_plan(&fixture.request()).expect_err("L_G mismatch must be rejected");
    assert_code(&error, E_CA_PATH_TABLE);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.l_g = 0;
    let error = compile_plan(&fixture.request()).expect_err("L_G=0 must be rejected");
    assert_code(&error, E_CA_TAPS);
}

#[test]
fn t66_reject_options_extended_without_admission() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.options.max_taps = 8192;
    let error = compile_plan(&fixture.request()).expect_err("extended taps must be gated");
    assert_code(&error, E_CA_OPTIONS);

    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.options.workers = 3;
    let error = compile_plan(&fixture.request()).expect_err("workers=3 must be rejected");
    assert_code(&error, E_CA_OPTIONS);
}

// ---------------------------------------------------------------------------
// T54 artifact path 适配（WAV 逐字节校验 / delay_q16 缺口）
// ---------------------------------------------------------------------------

fn artifact_path_fixture(
    out_dense: u16,
    in_dense: u16,
    coeffs: &[f32],
    delay_q16: u32,
) -> (ArtifactPath, Vec<u8>) {
    let wav = encode_wav_f32_mono(FS, coeffs).expect("wav encode");
    let peak = coeffs
        .iter()
        .fold(0.0_f32, |acc, value| acc.max(value.abs())) as f64;
    let path = ArtifactPath {
        out_dense,
        in_dense,
        out_role: "SUB1".to_string(),
        in_role: "FL".to_string(),
        out_dac: 7,
        in_dac: 0,
        file: format!("coeff/p_{in_dense}__{out_dense}.wav"),
        format: "wav_f32_mono",
        fs: FS,
        taps: coeffs.len() as u32,
        delay_samples: if out_dense == in_dense { 0 } else { 0 },
        delay_q16,
        diag_baseline: out_dense == in_dense,
        zero: false,
        band_hz: (63.0, 250.0),
        bytes: wav.len() as u64,
        sha256: sha256_hex(&wav),
        f64_sha256: sha_fill(0x99),
        peak_abs: peak,
    };
    (path, wav)
}

#[test]
fn t66_artifact_path_adapter_verifies_wav_bytes() {
    let coeffs = [1.0_f32, 0.25, -0.125, 0.0];
    let (path, wav) = artifact_path_fixture(0, 0, &coeffs, 0);
    let input = GPathInput::from_artifact_path(&path, &wav).expect("valid artifact path");
    assert!(input.diag_baseline);
    assert_eq!(input.coeffs.len(), coeffs.len());
    assert_eq!(input.coeff_sha256, path.sha256);

    let mut bad = path.clone();
    bad.sha256 = sha_fill(0x77);
    let error =
        GPathInput::from_artifact_path(&bad, &wav).expect_err("wav hash mismatch must reject");
    assert_code(&error, E_CA_ARTIFACT_HASH);

    let (path_q16, wav_q16) = artifact_path_fixture(1, 0, &coeffs, 1);
    let error = GPathInput::from_artifact_path(&path_q16, &wav_q16)
        .expect_err("delay_q16 != 0 must reject (ABI v1.0)");
    assert_code(&error, E_CA_DELAY_Q16_ABI);
}

// ---------------------------------------------------------------------------
// delay/polarity 编码 + phase 方向（T65 §4 / T40 因果 delta）
// ---------------------------------------------------------------------------

fn pure_delay_fixture(encoding: DelayPolarityEncoding) -> Fixture {
    let mut fixture = Fixture::multichannel(encoding);
    for role in &mut fixture.roles {
        role.p_fir = vec![1.0];
    }
    // F-35：p_fir 改写后重新绑定 T64 报告字节（申报数字/报告与系数一致）。
    fixture.headroom = headroom_with_bound_t64_report(&fixture.roles, T64_REPORT_PROGRAM_PEAK_DBFS);
    fixture
}

#[test]
fn t66_role_metadata_encoding_keeps_delay_in_role_table() {
    let fixture = pure_delay_fixture(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    for (index, role) in plan.roles.iter().enumerate() {
        let expected_delay = fixture.roles[index].delay_samples;
        assert_eq!(
            role.delay_samples, expected_delay,
            "RoleMetadata 延迟必须在 role 表"
        );
        assert_eq!(role.polarity, fixture.roles[index].polarity);
    }
    for diag in &plan.diags {
        assert_eq!(diag.delay_samples, 0, "RoleMetadata P FIR 不得自带延迟");
        assert_eq!(diag.taps, 1, "identity 目标 FIR 支撑必须为 1");
        assert_eq!(diag.flags & DIAG_FLAG_PHASE, 0);
    }
}

#[test]
fn t66_coeff_embedded_matches_t40_causal_delta_and_direction() {
    let fixture = pure_delay_fixture(DelayPolarityEncoding::CoeffEmbedded);
    let plan = compile_ok(&fixture);
    // role 表不再携带 delay/polarity（已折入系数，防双扣）。
    for role in &plan.roles {
        assert_eq!(role.delay_samples, 0);
        assert_eq!(role.polarity, 1);
    }
    // 每角色 diag 行必须等于 T40 design_integer 的 delta(D)*polarity（H=e^-jwD，因果）。
    for role in &fixture.roles {
        let diag = plan
            .diags
            .iter()
            .find(|diag| diag.out_index == role.out_channel)
            .expect("diag per role");
        let spec = IntegerDelaySpec::new(FS as f64, role.delay_samples as u64).with_polarity(
            if role.polarity >= 0 {
                PhasePolarity::Normal
            } else {
                PhasePolarity::Invert
            },
        );
        let design = design_integer(&spec).expect("T40 integer delay");
        assert_eq!(diag.taps as usize, design.coefficients.len());
        let bytes = &plan.coeffs[diag.coeff_offset_bytes as usize
            ..diag.coeff_offset_bytes as usize + diag.coeff_len_bytes as usize];
        for (index, expected) in design.coefficients.iter().enumerate() {
            let value = f32::from_le_bytes([
                bytes[index * 4],
                bytes[index * 4 + 1],
                bytes[index * 4 + 2],
                bytes[index * 4 + 3],
            ]) as f64;
            assert!(
                (value - expected).abs() <= 1e-6,
                "role {} tap {index}: {value} != {expected}",
                role.role_id
            );
        }
        // 因果方向：D 之前的 tap 全 0（延迟而不是提前）。
        for index in 0..role.delay_samples as usize {
            assert_eq!(bytes[index * 4], 0, "causal tap must be zero before D");
        }
        assert!(diag.flags & DIAG_FLAG_PHASE != 0 || role.delay_samples == 0);
    }
}

#[test]
fn t66_reject_double_delay_encoding() {
    // RoleMetadata 下把 delay 再编码成 delta(D) -> 双扣，必须拒收。
    let mut fixture = pure_delay_fixture(DelayPolarityEncoding::RoleMetadata);
    let delay = fixture.roles[1].delay_samples as usize;
    let mut fir = vec![0.0_f64; delay + 1];
    fir[delay] = 1.0;
    fixture.roles[1].p_fir = fir;
    let error =
        compile_plan(&fixture.request()).expect_err("double delay encoding must be rejected");
    assert_code(&error, E_CA_DOUBLE_COUNT);
}

// ---------------------------------------------------------------------------
// F-16：线性相位 P FIR 固有群延迟 GD_p=(taps-1)/2 已含在系数里；GD_p≠0 与 role
// delay_samples D_i≠0 并存 = 双扣（总延迟 = GD_p + D_i），定义落地前一律拒收。
// 覆盖 taps 奇/偶、D_i=0/非 0、单点脉冲/多抽头、RoleMetadata/CoeffEmbedded 两种编码。
// ---------------------------------------------------------------------------

const ENCODINGS: [DelayPolarityEncoding; 2] = [
    DelayPolarityEncoding::RoleMetadata,
    DelayPolarityEncoding::CoeffEmbedded,
];

/// 3 抽头对称 FIR：线性相位，群延迟 (3-1)/2 = 1 samples（奇 taps）。
fn linear_phase_fir_odd() -> Vec<f64> {
    vec![0.25, 0.5, 0.25]
}

/// 4 抽头对称 FIR：线性相位，群延迟 (4-1)/2 = 1.5 samples（偶 taps，半整数）。
fn linear_phase_fir_even() -> Vec<f64> {
    vec![0.2, 0.5, 0.5, 0.2]
}

#[test]
fn t66_reject_linear_phase_fir_with_role_delay() {
    for encoding in ENCODINGS {
        for (case, p_fir) in [
            ("odd", linear_phase_fir_odd()),
            ("even", linear_phase_fir_even()),
        ] {
            let mut fixture = Fixture::multichannel(encoding);
            assert!(fixture.roles[1].delay_samples > 0, "夹具 role 1 必须带 D_i");
            fixture.roles[1].p_fir = p_fir;
            let error = compile_plan(&fixture.request())
                .expect_err(&format!("{encoding:?}/{case}: GD_p 与 D_i 并存必须拒收"));
            assert_code(&error, E_CA_DOUBLE_COUNT);
        }
    }
}

#[test]
fn t66_reject_single_impulse_delay_with_role_delay() {
    // 单点脉冲等价纯延迟 FIR：GD_p = 脉冲下标。旧实现只抓 RoleMetadata 且只抓
    // index == D_i；这里补 CoeffEmbedded 与「index != D_i 仍会叠加总延迟」两个漏网。
    for encoding in ENCODINGS {
        // index == D_i。
        let mut fixture = Fixture::multichannel(encoding);
        let delay = fixture.roles[1].delay_samples as usize;
        let mut fir = vec![0.0_f64; delay + 1];
        fir[delay] = 1.0;
        fixture.roles[1].p_fir = fir;
        let error =
            compile_plan(&fixture.request()).expect_err("single impulse at D 与 D_i 并存必须拒收");
        assert_code(&error, E_CA_DOUBLE_COUNT);

        // index != D_i：P FIR 固有延迟仍与 role 延迟相加，一律拒收。
        let mut fixture = Fixture::multichannel(encoding);
        let mut fir = vec![0.0_f64; 2];
        fir[1] = 1.0;
        fixture.roles[1].p_fir = fir;
        let error =
            compile_plan(&fixture.request()).expect_err("single impulse 与 D_i 并存必须拒收");
        assert_code(&error, E_CA_DOUBLE_COUNT);
    }
}

#[test]
fn t66_accept_linear_phase_fir_when_role_delay_is_zero() {
    // D_i=0：P FIR 自带群延迟允许（固有 latency 只由系数承载，不是 plan 延迟字段）。
    for encoding in ENCODINGS {
        for p_fir in [linear_phase_fir_odd(), linear_phase_fir_even()] {
            let mut fixture = Fixture::multichannel(encoding);
            assert_eq!(fixture.roles[0].delay_samples, 0);
            fixture.roles[0].p_fir = p_fir;
            // F-35：p_fir 改写后重新绑定 T64 报告字节（否则报告与系数不一致会被拒）。
            fixture.headroom =
                headroom_with_bound_t64_report(&fixture.roles, T64_REPORT_PROGRAM_PEAK_DBFS);
            compile_ok(&fixture);
        }
    }
}

#[test]
fn t66_force_fused_form_has_no_diag_rows() {
    let mut fixture = pure_delay_fixture(DelayPolarityEncoding::RoleMetadata);
    fixture.options.force_form = Some(FusionForm::FusedGp);
    let plan = compile_ok(&fixture);
    assert_eq!(
        plan.diags.len(),
        0,
        "fused 形态 P 已折入路径，不得再留 diag 行"
    );
    assert!(plan
        .paths
        .iter()
        .all(|path| path.flags & PATH_FLAG_FUSED != 0));
    assert_eq!(plan.report.facts.chosen_form, "fused_gp");
    assert_eq!(plan.report.facts.selection_reason, "forced_by_caller");
    assert_eq!(
        plan.report.facts.fused_len_gp,
        fixture.l_g + fixture.l_p - 1
    );
}

// ---------------------------------------------------------------------------
// 拒坏图：自校验 + T20 parse 二次校验
// ---------------------------------------------------------------------------

#[test]
fn t66_verify_rejects_tampered_plan_bytes() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let mut tampered = plan.clone();
    tampered.plan_bytes[0] ^= 0xFF;
    let error = tampered
        .verify()
        .expect_err("tampered bytes must fail self-verify");
    assert_code(&error, E_CA_PLAN_VERIFY);

    let mut tampered_header = plan.clone();
    tampered_header.header.max_taps += 1;
    let error = tampered_header
        .verify()
        .expect_err("tampered header must fail self-verify");
    assert_code(&error, E_CA_PLAN_VERIFY);
}

fn registry_reject(bytes: Vec<u8>, graph_id: [u8; 32]) -> RtStatus {
    let mut registry = PlanRegistry::new();
    let token = PlanToken::new(PlanBuf::new(bytes), graph_id);
    registry
        .admit_with(token, &env_4w(), &mut MockMemory::new())
        .expect_err("corrupt plan must be rejected by T20 registry")
}

#[test]
fn t66_registry_rejects_corrupt_magic_and_hash() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);

    let mut bad_magic = plan.plan_bytes.clone();
    bad_magic[0] ^= 0xFF;
    assert_eq!(registry_reject(bad_magic, plan.graph_id).code(), 102);

    let mut bad_hash = plan.plan_bytes.clone();
    bad_hash[36] ^= 0xFF; // header.graph_id
    assert_eq!(registry_reject(bad_hash, plan.graph_id).code(), 112);
}

#[test]
fn t66_registry_rejects_over_taps_and_role_delay() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);

    // path 表 taps（path 0 位于 offset 128，taps 在 +4）。
    let mut bad_taps = plan.plan_bytes.clone();
    bad_taps[128 + 4..128 + 8].copy_from_slice(&20_000u32.to_le_bytes());
    assert_eq!(registry_reject(bad_taps, plan.graph_id).code(), 110);

    // role 表 delay_samples（role 项 +8；表起点 = 128 + paths*32 + diags*32）。
    let role_offset = 128 + plan.paths.len() * 32 + plan.diags.len() * 32;
    let mut bad_role = plan.plan_bytes.clone();
    bad_role[role_offset + 8..role_offset + 12].copy_from_slice(&20_000u32.to_le_bytes());
    assert_eq!(registry_reject(bad_role, plan.graph_id).code(), 107);
}

// ---------------------------------------------------------------------------
// prepare 资源准入
// ---------------------------------------------------------------------------

#[test]
fn t66_prepare_rejects_admission_failures() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);

    let mut session = ApplySession::new(FS, BLOCK);
    let mut env = env_4w();
    env.preallocated_bytes = 1;
    let error = session
        .prepare_with(&plan, &env, &mut MockMemory::new())
        .expect_err("declared budget too small must reject");
    assert_code(&error, E_CA_ADMISSION);
    assert_eq!(session.state(), APPLY_STATE_IDLE);

    let mut session = ApplySession::new(FS, BLOCK);
    let mut memory = MockMemory::new();
    memory = memory.failing_mlock(RtStatus::ERR_ADMISSION_MEMORY.code());
    let error = session
        .prepare_with(&plan, &env_4w(), &mut memory)
        .expect_err("mlock failure must reject (no warn-and-RT)");
    assert_code(&error, E_CA_ADMISSION);
    assert_eq!(session.state(), APPLY_STATE_IDLE);
}

#[test]
fn t66_prepare_rejects_stale_env_and_workers() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.options.workers = 2;
    let plan = compile_ok(&fixture);
    let mut session = ApplySession::new(FS, BLOCK);
    let error = session
        .prepare_with(&plan, &env_4w(), &mut MockMemory::new())
        .expect_err("plan workers=2 vs 4-core env must reject");
    assert_code(&error, E_CA_ADMISSION);

    let mut env = env_4w();
    env.sample_rate_hz = 96_000;
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan4 = compile_ok(&fixture);
    let mut session = ApplySession::new(FS, BLOCK);
    let error = session
        .prepare_with(&plan4, &env, &mut MockMemory::new())
        .expect_err("fs mismatch must reject (stale)");
    assert_code(&error, E_CA_ADMISSION);
}

// ---------------------------------------------------------------------------
// version apply ack（§5.3：未 all-ack 不算 applied）
// ---------------------------------------------------------------------------

#[test]
fn t66_prepare_commit_ack_flow() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let mut session = ApplySession::new(FS, BLOCK);
    let env = env_4w();
    let prepared = session
        .prepare_with(&plan, &env, &mut MockMemory::new())
        .expect("prepare must succeed");
    assert_eq!(prepared.plan_version, 1);
    assert_eq!(prepared.expected_workers, RT_WORKERS);
    assert_eq!(prepared.ack_mask, 0xF);
    assert_eq!(prepared.path_count, plan.paths.len() as u32);
    assert_eq!(session.state(), APPLY_STATE_PREPARED);
    assert!(session.require_apply_ack().is_err());

    let commit = session.commit().expect("commit must succeed");
    assert_eq!(commit.pending_version, 1);
    assert!(commit.awaiting_ack);
    assert!(session.require_apply_ack().is_err(), "commit != applied");

    let block = session.synthetic_all_ack_block(commit.pending_version, RT_WORKERS);
    let ack = session
        .note_block_ack(&block)
        .expect("all-ack block must be accepted")
        .expect("must return ack when retire ready");
    assert!(ack.applied);
    assert_eq!(ack.ack_mask, 0xF);
    assert_eq!(ack.plan_version, 1);
    assert_eq!(ack.graph_id_hex, plan.graph_id_hex());
    assert_eq!(session.state(), APPLY_STATE_APPLIED);
    assert_eq!(session.overall_status(), OVERALL_APPLIED_NOT_CONFIRMED);
    assert_eq!(session.apply_ack().map(|ack| ack.plan_version), Some(1));
    assert!(session.apply_ack_json().is_some());
    let artifact = session.apply_ack_artifact().expect("ack artifact");
    assert_eq!(artifact.path, APPLY_ACK_ARTIFACT_PATH);
    assert_eq!(artifact.sha256, sha256_hex(&artifact.bytes));
}

#[test]
fn t66_apply_ack_requires_new_version_all_ack() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let mut session = ApplySession::new(FS, BLOCK);
    session
        .prepare_with(&plan, &env_4w(), &mut MockMemory::new())
        .expect("prepare");
    let commit = session.commit().expect("commit");

    // cutoff block（invalid）→ 不得 applied。
    let block = session.synthetic_all_ack_block(commit.pending_version, RT_WORKERS);
    block.cutoff.store(1, Ordering::Release);
    let error = session
        .note_block_ack(&block)
        .expect_err("cutoff block must not ack");
    assert_eq!(error.status().map(|status| status.code()), Some(201));
    assert!(session.require_apply_ack().is_err());
    assert_eq!(session.effective().map(|plan| plan.plan_version), None);

    // 错误版本 block → 拒绝。
    let wrong = session.synthetic_all_ack_block(commit.pending_version + 1, RT_WORKERS);
    let error = session
        .note_block_ack(&wrong)
        .expect_err("wrong version must be rejected");
    assert_code(&error, E_CA_COMMIT);
}

#[test]
fn t66_apply_two_worker_profile_ack_mask() {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.options.workers = 2;
    let plan = compile_ok(&fixture);
    let mut env = env_4w();
    env.rt_workers = 2;
    let mut session = ApplySession::new(FS, BLOCK);
    let prepared = session
        .prepare_with(&plan, &env, &mut MockMemory::new())
        .expect("2-worker prepare");
    assert_eq!(prepared.ack_mask, 0x3);
    let commit = session.commit().expect("2-worker commit");
    let block = session.synthetic_all_ack_block(commit.pending_version, 2);
    let ack = session
        .note_block_ack(&block)
        .expect("2-worker all-ack")
        .expect("ack");
    assert_eq!(ack.ack_mask, 0x3);
    assert_eq!(ack.expected_workers, 2);
}

// ---------------------------------------------------------------------------
// T3 应用后复测（K4：缺 measured = INCOMPLETE，禁止 PASS_OPEN_LOOP）
// ---------------------------------------------------------------------------

fn t3_ok(version: PlanVersion, graph_hex: &str) -> T3RetestEvidence {
    let mut evidence = T3RetestEvidence::new("run-t3-1", version, graph_hex);
    evidence.measured = true;
    evidence.residual_delay_max_samples = 0.2;
    evidence.polarity_match_all = true;
    evidence.fr_deviation_max_db = 0.5;
    evidence.target_deviation_max_db = 2.0;
    evidence.peq_implementation_max_db = 0.3;
    evidence.measured_peak_dbfs = Some(-6.0);
    evidence.holdout_present = true;
    evidence.raw_evidence_sha256 = vec![sha_fill(0xf1), sha_fill(0xf2)];
    evidence
}

fn applied_session(plan: &CompiledPlan) -> (ApplySession, PlanVersion) {
    let mut session = ApplySession::new(FS, BLOCK);
    session
        .prepare_with(plan, &env_4w(), &mut MockMemory::new())
        .expect("prepare");
    let commit = session.commit().expect("commit");
    let block = session.synthetic_all_ack_block(commit.pending_version, RT_WORKERS);
    let ack = session
        .note_block_ack(&block)
        .expect("all-ack")
        .expect("ack");
    (session, ack.plan_version)
}

#[test]
fn t66_t3_retest_confirms_or_lists_failures() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);

    // 未做 T3：overall 不得 CONFIRMED/PASS。
    let (mut session, version) = applied_session(&plan);
    assert_eq!(session.overall_status(), OVERALL_APPLIED_NOT_CONFIRMED);

    // 全过 -> CONFIRMED。
    let evidence = t3_ok(version, &plan.graph_id_hex());
    let receipt = session
        .record_t3_retest(&evidence)
        .expect("t3 evidence must validate");
    assert!(receipt.passed, "failures: {:?}", receipt.failures);
    assert_eq!(receipt.state, APPLY_STATE_CONFIRMED);
    assert_eq!(session.overall_status(), OVERALL_CONFIRMED);

    // 但 confirmed 后不得再记录冲突证据？状态机只允许 applied -> confirmed；
    // 新会话演示未 measured/版本不符/超门都列 fail 码。
    let (mut session, version) = applied_session(&plan);
    let mut bad = t3_ok(version, &plan.graph_id_hex());
    bad.measured = false;
    bad.residual_delay_max_samples = 0.8;
    bad.polarity_match_all = false;
    bad.fr_deviation_max_db = 2.0;
    bad.target_deviation_max_db = 5.0;
    bad.peq_implementation_max_db = 1.5;
    bad.measured_peak_dbfs = Some(-0.5);
    bad.holdout_present = false;
    bad.applied_version = version + 7;
    let receipt = session
        .record_t3_retest(&bad)
        .expect("evidence shape valid");
    assert!(!receipt.passed);
    assert_eq!(session.state(), APPLY_STATE_APPLIED);
    assert!(receipt.failures.contains(&T3_NOT_MEASURED));
    assert!(receipt.failures.contains(&T3_RESIDUAL_DELAY));
    assert!(receipt.failures.contains(&T3_POLARITY));
    assert!(receipt.failures.contains(&T3_FR_DEVIATION));
    assert!(receipt.failures.contains(&T3_TARGET_DEVIATION));
    assert!(receipt.failures.contains(&T3_PEQ_IMPLEMENTATION));
    assert!(receipt.failures.contains(&T3_PEAK));
    assert!(receipt.failures.contains(&T3_HOLDOUT_MISSING));
    assert!(receipt.failures.contains(&T3_VERSION_MISMATCH));
}

// ---------------------------------------------------------------------------
// 热更 last-good 回滚（失败只自研 last-good 或 SafetyMute，不回旧 C）
// ---------------------------------------------------------------------------

fn plan_second_variant() -> CompiledPlan {
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.paths[0].coeffs[1] += 0.5;
    compile_plan(&fixture.request()).expect("second plan must compile")
}

#[test]
fn t66_rollback_before_commit_keeps_effective_plan() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let (mut session, version) = applied_session(&plan);
    assert_eq!(version, 1);

    let second = plan_second_variant();
    session
        .prepare_with(&second, &env_4w(), &mut MockMemory::new())
        .expect("prepare second plan");
    assert_eq!(session.state(), APPLY_STATE_PREPARED);
    let receipt = session
        .rollback_with(RollbackReason::OperatorRequest, &mut MockMemory::new())
        .expect("abort rollback");
    assert_eq!(receipt.mode, ROLLBACK_MODE_ABORTED);
    assert!(receipt.rolled_back);
    assert!(!receipt.safety_mute);
    assert_eq!(receipt.effective_version, Some(1));
    assert_eq!(session.state(), APPLY_STATE_ROLLED_BACK);
    assert_eq!(
        session.effective().map(|plan| plan.graph_id),
        Some(plan.graph_id)
    );
    assert!(session
        .events()
        .iter()
        .any(|event| event.kind == EV_PLAN_ABORTED));
}

#[test]
fn t66_rollback_after_t3_failure_reactivates_last_good() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan_v1 = compile_ok(&fixture);
    let plan_v2 = plan_second_variant();
    assert_ne!(plan_v1.graph_id, plan_v2.graph_id);

    let (mut session, version_v1) = applied_session(&plan_v1);
    session
        .prepare_with(&plan_v2, &env_4w(), &mut MockMemory::new())
        .expect("prepare v2");
    let commit = session.commit().expect("commit v2");
    let block = session.synthetic_all_ack_block(commit.pending_version, RT_WORKERS);
    let ack = session
        .note_block_ack(&block)
        .expect("ack v2")
        .expect("applied v2");
    assert_eq!(ack.plan_version, 2);
    assert_eq!(
        session.last_good().map(|plan| plan.plan_version),
        Some(version_v1)
    );

    // T3 实测失败 -> 回滚 last-good（v1 字节，经 SafetyMute + 完整 re-admission）。
    let mut bad = t3_ok(ack.plan_version, &plan_v2.graph_id_hex());
    bad.residual_delay_max_samples = 1.2;
    let receipt = session.record_t3_retest(&bad).expect("fail evidence shape");
    assert!(!receipt.passed);
    let rollback = session
        .rollback_with(RollbackReason::T3Failed, &mut MockMemory::new())
        .expect("rollback to last-good");
    assert_eq!(rollback.mode, ROLLBACK_MODE_READMIT_LAST_GOOD);
    assert!(rollback.rolled_back);
    assert!(!rollback.safety_mute);
    assert_eq!(rollback.safety_mute_cycles, 1);
    assert!(rollback.version_epoch_reset);
    assert_eq!(
        rollback.last_good_graph_id_hex.as_deref(),
        Some(plan_v1.graph_id_hex().as_str())
    );
    assert_eq!(session.state(), APPLY_STATE_ROLLED_BACK);
    let effective = session.effective().expect("effective after rollback");
    assert_eq!(effective.graph_id, plan_v1.graph_id);
    assert_eq!(effective.plan_version, 1, "fresh runtime session version");
    assert!(session
        .events()
        .iter()
        .any(|event| event.kind == EV_SAFETY_MUTE_ENTER));
    assert!(session
        .events()
        .iter()
        .any(|event| event.kind == EV_ROLLBACK));
    assert!(session
        .events()
        .iter()
        .any(|event| event.kind == EV_SAFETY_MUTE_EXIT));
}

#[test]
fn t66_rollback_without_last_good_stays_safety_mute() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let (mut session, version) = applied_session(&plan);
    let mut bad = t3_ok(version, &plan.graph_id_hex());
    bad.measured = false;
    assert!(!session.record_t3_retest(&bad).expect("shape").passed);
    let receipt = session
        .rollback_with(RollbackReason::T3Failed, &mut MockMemory::new())
        .expect("mute rollback");
    assert_eq!(receipt.mode, ROLLBACK_MODE_SAFETY_MUTE);
    assert!(receipt.rolled_back);
    assert!(receipt.safety_mute);
    assert_eq!(receipt.effective_version, None);
    assert_eq!(session.state(), APPLY_STATE_SAFETY_MUTE);
    assert_eq!(session.overall_status(), OVERALL_SAFETY_MUTE);
    assert!(session.effective().is_none(), "no partial graph output");
}

#[test]
fn t66_apply_events_and_summary_are_exported() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let plan = compile_ok(&fixture);
    let (session, _version) = applied_session(&plan);
    let kinds: Vec<u16> = session.events().iter().map(|event| event.kind).collect();
    for expected in [
        EV_PLAN_PREPARED,
        EV_PLAN_ADMITTED,
        EV_PLAN_STAGED,
        EV_PLAN_SWAPPED,
        EV_SWAP_ACK,
    ] {
        assert!(kinds.contains(&expected), "missing event kind {expected}");
    }
    let lines = session.summary_lines();
    assert!(lines
        .iter()
        .any(|line| line.starts_with("effective version=1")));
    assert_eq!(
        session.events()[0].label(),
        event_name(EV_PLAN_PREPARED),
        "event label must use core-v1 names"
    );
}

// ---------------------------------------------------------------------------
// cursor-overflow/F-35：D7 证据绑定（T64 报告字节 sha256 + 编码后 P 行重算）
//
// 添加于 2026-11-17 终审修复组B（cal-apply/task-2）；既有 39 个 t66_* 测试全部保留，本段只追加。
// ---------------------------------------------------------------------------

/// T64 报告夹具的 program peak 基准（= PeqFitOptions::default 的 program_peak_reference_dbfs）。
const T64_REPORT_PROGRAM_PEAK_DBFS: f64 = -18.0;

/// 由 P FIR 生成一条与 T64 `tspi-peq-fit/v1` 同形的 channel 报告（D7 字段子集）。
fn t64_report_channel(
    name: &str,
    p_fir: &[f64],
    program_peak_dbfs: f64,
) -> khaen_compat::ws::JsonValue {
    use khaen_compat::ws::{json_object, JsonValue};
    let gain_db = super::p_row_chain_peak_gain_db(p_fir, FS as f64);
    let max_positive_db = super::p_row_max_positive_db(p_fir, FS as f64).max(0.0);
    let predicted = program_peak_dbfs + gain_db;
    json_object(vec![
        ("name", JsonValue::String(name.to_string())),
        ("band_count", JsonValue::UInt(0)),
        (
            "headroom",
            json_object(vec![
                (
                    "program_peak_reference_dbfs",
                    JsonValue::Number(program_peak_dbfs),
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

/// T64 报告字节（compat ws 编码；生产侧 `parse_t64_fit_report` 解析同一 schema）。
fn t64_report_bytes(channels: &[(&str, &[f64], f64)]) -> Vec<u8> {
    use khaen_compat::ws::{json_encode, json_object, JsonValue};
    let document = json_object(vec![
        ("schema", JsonValue::String(PEQ_FIT_SCHEMA.to_string())),
        (
            "channels",
            JsonValue::Array(
                channels
                    .iter()
                    .map(|(name, p_fir, program_peak_dbfs)| {
                        t64_report_channel(name, p_fir, *program_peak_dbfs)
                    })
                    .collect(),
            ),
        ),
    ]);
    json_encode(&document)
        .expect("T64 report fixture must encode")
        .into_bytes()
}

/// F-35 夹具：由 roles 的 P FIR 生成 T64 报告字节 + 申报记录，并绑定 report sha256。
fn headroom_with_bound_t64_report(
    roles: &[RoleCompileInput],
    program_peak_dbfs: f64,
) -> HeadroomEvidence {
    let mut records = Vec::with_capacity(roles.len());
    let mut specs: Vec<(&str, &[f64], f64)> = Vec::with_capacity(roles.len());
    for role in roles {
        let gain_db = super::p_row_chain_peak_gain_db(&role.p_fir, FS as f64);
        let max_positive_db = super::p_row_max_positive_db(&role.p_fir, FS as f64).max(0.0);
        let predicted_peak = program_peak_dbfs + gain_db;
        records.push(
            RoleBoostRecord::new(
                role.role_id.clone(),
                max_positive_db,
                max_positive_db,
                predicted_peak,
                -predicted_peak,
                true,
            )
            .expect("D7 fixture record must be valid"),
        );
        specs.push((
            role.role_id.as_str(),
            role.p_fir.as_slice(),
            program_peak_dbfs,
        ));
    }
    let report_bytes = t64_report_bytes(&specs);
    let report_sha256 = sha256_hex(&report_bytes);
    HeadroomEvidence::new(PEQ_FIT_SCHEMA, report_sha256, records)
        .with_t64_report_bytes(report_bytes)
}

#[test]
fn t66_f35_t64_report_bytes_bound_and_d7_recomputed() {
    // (i) 合法 T64 报告（sha256 与字节一致）→ 通过；报告 hash 进编译报告。
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let report_bytes = fixture
        .headroom
        .t64_report_bytes()
        .expect("F-35 fixture must bind T64 report bytes")
        .to_vec();
    assert_eq!(
        sha256_hex(&report_bytes),
        fixture.headroom.report_sha256,
        "report_sha256 必须等于 T64 报告字节 sha256"
    );
    let plan = compile_ok(&fixture);
    assert_eq!(
        plan.report.headroom_report_sha256, fixture.headroom.report_sha256,
        "编译报告必须回写绑定的 T64 报告 sha256"
    );
    assert!(plan.d7_warnings.is_empty(), "合法报告不应有 D7 warning");
}

#[test]
fn t66_f35_rejects_hotter_p_fir_than_declared_report() {
    // (ii-a) 申报数字/报告都合格，但真正下发的 P 行被换成正峰 +6 dB → 单段帽从系数本身拒收。
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[0].p_fir = vec![2.0];
    let error = compile_plan(&fixture.request()).expect_err("hotter P row must be rejected (F-35)");
    assert_code(&error, E_CA_D7_BOOST);

    // (ii-b) P 行幅度链相对 1 kHz 参考更热（1 kHz 处深陷、绝对正峰仍 ≤ +3 dB）→ D7 峰值重算拒收。
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[0].p_fir = vec![0.5, -0.5];
    let error =
        compile_plan(&fixture.request()).expect_err("hotter chain gain must be rejected (F-35)");
    assert_code(&error, E_CA_D7_PEAK);
}

#[test]
fn t66_f35_rejects_report_sha256_not_matching_bytes() {
    // (iii-a) report_sha256 与绑定字节不符 → 拒收。
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom.report_sha256 = sha_fill(0xd1);
    let error = compile_plan(&fixture.request())
        .expect_err("report sha256 != bytes sha256 must be rejected (F-35)");
    assert_code(&error, E_CA_ARTIFACT_HASH);

    // (iii-b) 只申报 64-hex hash、不给 T64 报告字节 → 不可判定，拒收。
    let mut fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    fixture.headroom = HeadroomEvidence::new(
        PEQ_FIT_SCHEMA,
        sha_fill(0xd2),
        fixture.headroom.roles.clone(),
    );
    let error = compile_plan(&fixture.request())
        .expect_err("missing T64 report bytes must be rejected (F-35)");
    assert_code(&error, E_CA_EVIDENCE);
}

// ---------------------------------------------------------------------------
// cursor-overflow/F-38：verify 与入口同一 peq_count 规则
// ---------------------------------------------------------------------------

#[test]
fn t66_f38_verify_rejects_peq_count_nonzero_same_rule_as_entry() {
    let fixture = Fixture::multichannel(DelayPolarityEncoding::RoleMetadata);
    let mut plan = compile_ok(&fixture);
    plan.roles[0].peq_count = 1;
    let error = plan
        .verify()
        .expect_err("verify must reject peq_count != 0 (F-38)");
    assert_code(&error, E_CA_PLAN_VERIFY);
    assert!(
        error.field().contains("peq_count"),
        "unexpected field: {}",
        error.field()
    );
    let clean = compile_ok(&fixture);
    clean.verify().expect("peq_count == 0 must verify");
}
