// T66 plan ABI v2（per-role IIR/PEQ 区）接线私有单测（GAP-T66-3）。
//
// 背景：T64 PEQ 拟合段此前只存在于 T64 报告/PeqDesign，T66 compile 产物是 ABI v1 plan，
// 声学校正不完整（F-37：残差 FIR 不能单独代表 P，H_eff≠Hraw·P）。本组验证：
// 1) 无 PEQ 时 plan 字节/graph_id/大小与改动前 bit-exact（冻结基线）；
// 2) T64 PEQ 段 → plan ABI v2 IIR 区（表/CRC/role 绑定/f32 系数编码），且被 T20
//    plan-arena **真实解析/准入**接受并暴露 IIR 表项（host 消费，离线静音）；
// 3) fail-closed：段数/预算/系数/报告绑定任一不一致都拒收，绝不静默丢弃 PEQ；
// 4) D7 证据：plan IIR 段逐段绑定 T64 报告 bands[] b/a，编码链峰值含 IIR 级联；
// 5) 实验 envelope 如实写 t64_correction_in_plan（有 PEQ=true，无 PEQ 仍 false/旧字节）。
//
// 全部离线：MockMemory 注入，不开 ALSA/设备/板端（出声验证 NOT_RUN）。

use crate::peq_fit::{FIT_FREQ_MIN_HZ, REFERENCE_FREQ_HZ, REPORT_GRID_OCTAVE};
use crate::peq_plan::PeqBiquad;
use khaen_core::abi::{
    PLAN_IIR_ENTRY_BYTES, PLAN_IIR_HEADER_BYTES, PLAN_IIR_SECTION_COEFFS, KHAEN_PLAN_ABI_V1,
    KHAEN_PLAN_ABI_V2,
};

// ---------------------------------------------------------------------------
// T64 报告夹具（带 bands[] b/a；D7 链峰值与编译期同口径）
// ---------------------------------------------------------------------------

fn peq_section(freq_hz: f64, gain_db: f64, q: f64) -> PeqBiquad {
    PeqBiquad::peaking_eq(FS as f64, freq_hz, gain_db, q).expect("valid peq section")
}

fn section_coeffs(section: &PeqBiquad) -> [f64; PLAN_IIR_SECTION_COEFFS] {
    [
        section.b0,
        section.b1,
        section.b2,
        section.a1,
        section.a2,
    ]
}

fn band_json(section: &PeqBiquad) -> khaen_compat::ws::JsonValue {
    use khaen_compat::ws::{json_object, JsonValue};
    let coeffs = section_coeffs(section);
    json_object(vec![
        (
            "b",
            JsonValue::Array(vec![
                JsonValue::Number(coeffs[0]),
                JsonValue::Number(coeffs[1]),
                JsonValue::Number(coeffs[2]),
            ]),
        ),
        (
            "a",
            JsonValue::Array(vec![
                JsonValue::Number(1.0),
                JsonValue::Number(coeffs[3]),
                JsonValue::Number(coeffs[4]),
            ]),
        ),
    ])
}

fn peq_sum_db(sections: &[PeqBiquad], freq_hz: f64) -> f64 {
    sections
        .iter()
        .map(|section| section.response_db(FS as f64, freq_hz))
        .sum()
}

/// 测得链路（P FIR ⊛ PEQ 级联）相对 1 kHz 的链峰值增益（= T64 peq_fit 的 D7 口径）。
fn combined_chain_peak_gain_db(p_fir: &[f64], sections: &[PeqBiquad]) -> f64 {
    let grid = khaen_designer::modal_peq::log_frequency_grid(
        FIT_FREQ_MIN_HZ,
        khaen_designer::modal_peq::freq_upper_hz(FS as f64),
        REPORT_GRID_OCTAVE,
    );
    let response: Vec<f64> = grid
        .iter()
        .map(|freq| super::fir_magnitude_db_at(p_fir, FS as f64, *freq) + peq_sum_db(sections, *freq))
        .collect();
    let reference = super::sample_curve_db(&grid, &response, REFERENCE_FREQ_HZ);
    response
        .iter()
        .map(|value| value - reference)
        .fold(f64::NEG_INFINITY, f64::max)
}

/// 单段 PEQ 的绝对幅度正峰（dB；T64 单段 boost 帽口径）。
fn section_abs_peak_db(section: &PeqBiquad) -> f64 {
    let grid = khaen_designer::modal_peq::log_frequency_grid(
        FIT_FREQ_MIN_HZ,
        khaen_designer::modal_peq::freq_upper_hz(FS as f64),
        REPORT_GRID_OCTAVE,
    );
    grid.iter()
        .map(|freq| section.response_db(FS as f64, *freq))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// T64 报告 channel（字段子集 + bands[]；与 khaen-peq-fit/v1 同形）。
fn peq_report_channel(
    name: &str,
    p_fir: &[f64],
    sections: &[PeqBiquad],
    program_peak_dbfs: f64,
) -> khaen_compat::ws::JsonValue {
    use khaen_compat::ws::{json_object, JsonValue};
    let gain_db = combined_chain_peak_gain_db(p_fir, sections);
    let max_positive_db = super::p_row_max_positive_db(p_fir, FS as f64).max(0.0);
    let single_boost_db = sections
        .iter()
        .map(section_abs_peak_db)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.0);
    let predicted = program_peak_dbfs + gain_db;
    json_object(vec![
        ("name", JsonValue::String(name.to_string())),
        ("band_count", JsonValue::UInt(sections.len() as u64)),
        (
            "bands",
            JsonValue::Array(sections.iter().map(band_json).collect()),
        ),
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
                ("single_filter_max_db", JsonValue::Number(single_boost_db)),
                ("filter_positive_sum_db", JsonValue::Number(single_boost_db)),
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

fn t64_report_bytes_peq(channels: &[(&str, &[f64], &[PeqBiquad], f64)]) -> Vec<u8> {
    use khaen_compat::ws::{json_encode, json_object, JsonValue};
    let document = json_object(vec![
        ("schema", JsonValue::String(PEQ_FIT_SCHEMA.to_string())),
        (
            "channels",
            JsonValue::Array(
                channels
                    .iter()
                    .map(|(name, p_fir, sections, program_peak_dbfs)| {
                        peq_report_channel(name, p_fir, sections, *program_peak_dbfs)
                    })
                    .collect(),
            ),
        ),
    ]);
    json_encode(&document)
        .expect("T64 report fixture must encode")
        .into_bytes()
}

/// F-35 夹具：按 roles 的 P FIR + PEQ 级联生成 T64 报告字节并绑定 sha256。
///
/// `report_bands=false` 时报告不写 bands[]（band_count=0），用于验证「plan 有 IIR 但报告
/// 无段」= 拒收；`report_bands=true` 时报告 bands 与 plan 段同序同系数。
fn headroom_with_bound_peq_report(
    roles: &[RoleCompileInput],
    program_peak_dbfs: f64,
    report_bands: bool,
) -> HeadroomEvidence {
    let mut records = Vec::with_capacity(roles.len());
    let mut specs: Vec<(&str, &[f64], &[PeqBiquad], f64)> = Vec::with_capacity(roles.len());
    let empty: &[PeqBiquad] = &[];
    for role in roles {
        let gain_db = combined_chain_peak_gain_db(&role.p_fir, &role.peq_sections);
        let max_positive_db = super::p_row_max_positive_db(&role.p_fir, FS as f64).max(0.0);
        let single_boost_db = role
            .peq_sections
            .iter()
            .map(section_abs_peak_db)
            .fold(f64::NEG_INFINITY, f64::max)
            .max(max_positive_db)
            .max(0.0);
        let predicted_peak = program_peak_dbfs + gain_db;
        records.push(
            RoleBoostRecord::new(
                role.role_id.clone(),
                single_boost_db,
                single_boost_db,
                predicted_peak,
                -predicted_peak,
                true,
            )
            .expect("D7 fixture record must be valid"),
        );
        let sections_for_report: &[PeqBiquad] = if report_bands {
            role.peq_sections.as_slice()
        } else {
            empty
        };
        specs.push((
            role.role_id.as_str(),
            role.p_fir.as_slice(),
            sections_for_report,
            program_peak_dbfs,
        ));
    }
    let report_bytes = t64_report_bytes_peq(&specs);
    let report_sha256 = crate::job::sha256_hex(&report_bytes);
    HeadroomEvidence::new(PEQ_FIT_SCHEMA, report_sha256, records)
        .with_t64_report_bytes(report_bytes)
}

/// measured 路径 fixture：FL 带 T64 PEQ 级联、FR 无 PEQ（P FIR 与 stereo 基线一致）。
fn peq_fixture(sections: Vec<PeqBiquad>) -> Fixture {
    let mut fixture = Fixture::stereo(DelayPolarityEncoding::RoleMetadata);
    fixture.roles[0] = RoleCompileInput::new(0, "FL", 0, Some(0), 0, 1, -1.0, vec![1.0, 0.05], 0)
        .expect("FL role")
        .with_peq_sections(sections)
        .expect("FL peq sections");
    fixture.headroom =
        headroom_with_bound_peq_report(&fixture.roles, T64_REPORT_PROGRAM_PEAK_DBFS, true);
    fixture
}

// ---------------------------------------------------------------------------
// 1) 无 PEQ：plan 字节 bit-exact 冻结基线（改动前实测值）
// ---------------------------------------------------------------------------

/// 改动前（ABI v2 PEQ 接线前）实测的 plan sha256/graph_id/字节数冻结基线。
///
/// 该断言是「无 PEQ 时输出与历史逐字节一致」的硬证据：任何布局/pad/graph_id 规范化
/// 漂移都会让这些常量失败。
#[test]
fn t66_peq_v1_bit_exact_baseline_without_peq() {
    // renamed tspi -> khaen (2026-11-25): GRAPH_HASH_DOMAIN b"tspi-plan/v1\0" ->
    // b"khaen-plan/v1\0" (+1 B of hash domain), so graph_id and the plan sha256 that
    // commits to it change. plan byte lengths are unchanged (the domain is not stored).
    let cases = [
        (
            DelayPolarityEncoding::RoleMetadata,
            true,
            "8101e1aab7976c4d5ed0b7b17a9e74872c5d40f30f6df69670fd19a58109b71e",
            "86333bcc7e20fe4b67c10fb81016059489e6cec48f3842924c08a5b897a74506",
            3596usize,
        ),
        (
            DelayPolarityEncoding::RoleMetadata,
            false,
            "d230b51d809592019474145243f63255bd5473d2b8bff248932340cf963a6897",
            "1668d3af9a10fa4927026476cf6d57cd254aca2ac6555068624065e5e06ae145",
            720usize,
        ),
        (
            DelayPolarityEncoding::CoeffEmbedded,
            true,
            "539b63157cc4db4f12cb63f369110fa961a0990e223aa493ad9d7b03182d4a2f",
            "9cbd1544de061d522f987a2c6b55cde507325824716b757bb158eb8795f729c4",
            3612usize,
        ),
        (
            DelayPolarityEncoding::CoeffEmbedded,
            false,
            "d230b51d809592019474145243f63255bd5473d2b8bff248932340cf963a6897",
            "1668d3af9a10fa4927026476cf6d57cd254aca2ac6555068624065e5e06ae145",
            720usize,
        ),
    ];
    for (encoding, multichannel, plan_sha, graph_id, bytes) in cases {
        let fixture = if multichannel {
            Fixture::multichannel(encoding)
        } else {
            Fixture::stereo(encoding)
        };
        let plan = compile_ok(&fixture);
        assert_eq!(plan.plan_sha256(), plan_sha, "plan sha256 ({encoding:?})");
        assert_eq!(plan.graph_id_hex(), graph_id, "graph_id ({encoding:?})");
        assert_eq!(plan.plan_bytes.len(), bytes, "plan bytes ({encoding:?})");
        assert_eq!(plan.header.abi_version, KHAEN_PLAN_ABI_V1);
        assert_eq!(plan.header.reserved3, [0u32; 3]);
        assert!(plan.iir_entries.is_empty());
        assert!(plan.iir_table.is_empty());
        assert!(plan.iir_blobs.is_empty());
        assert!(plan.report.iir.is_none());
        // 无 PEQ 的 compile-report canonical/JSON 不得出现 IIR 段（字节与历史一致）。
        assert!(!plan.report.canonical().contains("iir "));
        assert!(!plan.report.to_json().contains("\"iir\""));
    }
}

// ---------------------------------------------------------------------------
// 2) T64 PEQ → plan ABI v2 IIR 区；T20 plan-arena（host 消费）解析准入
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_t64_sections_written_as_plan_abi_v2_iir_and_host_parsed() {
    let sections = vec![peq_section(100.0, -6.0, 1.0), peq_section(125.0, -10.0, 8.0)];
    let fixture = peq_fixture(sections.clone());
    let plan = compile_ok(&fixture);

    // 头部/表：ABI v2，reserved3 = [IIR 表偏移, 表项数, 0]。
    assert_eq!(plan.header.abi_version, KHAEN_PLAN_ABI_V2);
    assert_eq!(plan.header.reserved3[1], 1, "1 个 en-IIR role");
    assert_eq!(plan.header.reserved3[2], 0);
    let iir_offset = plan.header.reserved3[0] as usize;
    assert_eq!(iir_offset % 8, 0, "IIR 表 8B 对齐");
    assert_eq!(
        plan.iir_table.len(),
        PLAN_IIR_HEADER_BYTES + PLAN_IIR_ENTRY_BYTES,
        "16B 表头 + 1×32B 表项"
    );
    assert_eq!(
        iir_offset + plan.iir_table.len(),
        plan.plan_bytes.len() - plan.coeffs.len(),
        "IIR 表紧接系数区之前"
    );
    assert_eq!(
        &plan.plan_bytes[iir_offset..iir_offset + plan.iir_table.len()],
        plan.iir_table.as_slice(),
        "下发字节 == 自校验表字节"
    );

    // role 表绑定：FL.peq_count == 2；FR == 0。
    let fl = plan.roles.iter().find(|role| role.role_id == 0).unwrap();
    let fr = plan.roles.iter().find(|role| role.role_id == 1).unwrap();
    assert_eq!(fl.peq_count, 2);
    assert_eq!(fr.peq_count, 0);
    assert_eq!(plan.iir_entries.len(), 1);
    let entry = plan.iir_entries[0];
    assert_eq!(entry.role_id, 0);
    assert_eq!(entry.order, 2);
    assert_eq!(entry.section_count, 2);
    assert_eq!(
        entry.coeff_len_bytes,
        (2 * PLAN_IIR_SECTION_COEFFS * 4) as u32
    );
    assert_eq!(entry.coeff_offset_bytes % 64, 0, "IIR 系数 64B 对齐");

    // 下发 f32 系数 == 请求段（逐段逐系数）。
    let parsed = super::parse_encoded_peq_sections(
        super::E_CA_PLAN_VERIFY,
        &plan.coeffs,
        entry.coeff_offset_bytes,
        entry.coeff_len_bytes,
    )
    .expect("plan IIR 系数可解析（有限 + 稳定）");
    assert_eq!(parsed.len(), 2);
    for (index, section) in sections.iter().enumerate() {
        assert_eq!(parsed[index], super::peq_section_coeffs_f32(section));
    }

    // 无 PEQ 对照：同 P FIR 的 stereo fixture 仍是 v1（plan 只多出 IIR 区）。
    let baseline = compile_ok(&Fixture::stereo(DelayPolarityEncoding::RoleMetadata));
    // renamed tspi -> khaen (2026-11-25); same property (no-PEQ baseline plan sha256).
    assert_eq!(baseline.plan_sha256(), "d230b51d809592019474145243f63255bd5473d2b8bff248932340cf963a6897");
    let extra = plan.coeffs.len() - baseline.coeffs.len();
    assert!(
        (40..40 + 64).contains(&extra),
        "IIR blob 需 40B + <64B 对齐 pad，实际 {extra}"
    );

    // host 消费（真实 T20 plan-arena 解析/准入路径，离线静音）：接受 ABI v2 并暴露 IIR 表。
    let mut registry = PlanRegistry::new();
    let mut memory = khaen_rt_host::admission::MockMemory::new();
    let version = registry
        .admit_with(plan.token(), &env_4w(), &mut memory)
        .expect("T20 plan-arena 必须接受 T66 ABI v2 plan");
    let view = registry.plan_view(version).expect("admitted plan view");
    assert_eq!(view.header.abi_version, KHAEN_PLAN_ABI_V2);
    assert_eq!(view.header.graph_id, plan.graph_id);
    assert_eq!(view.roles[0].peq_count, 2);
    let host_iir = registry
        .iir_entries(version)
        .expect("host 解析出 IIR 表项")
        .to_vec();
    assert_eq!(host_iir.len(), 1);
    assert_eq!(host_iir[0].role_id, 0);
    assert_eq!(host_iir[0].section_count, 2);
    assert_eq!(host_iir[0].coeff_len_bytes, entry.coeff_len_bytes);
    assert_eq!(host_iir[0].coeff_offset_bytes, entry.coeff_offset_bytes);

    // 可选：落盘供离线 host CLI（plan-arena + backend=fake）静音验证（证据脚本用）。
    if let Ok(out_dir) = std::env::var("T66_PEQ_PLAN_OUT") {
        let dir = std::path::Path::new(&out_dir);
        std::fs::create_dir_all(dir).expect("create T66_PEQ_PLAN_OUT");
        std::fs::write(dir.join("plan.bin"), &plan.plan_bytes).expect("write plan.bin");
        std::fs::write(dir.join("plan.sha256"), plan.plan_sha256()).expect("write plan sha256");
        std::fs::write(dir.join("graph_id.txt"), plan.graph_id_hex()).expect("write graph id");
        std::fs::write(
            dir.join("peq-sections.txt"),
            sections
                .iter()
                .map(|section| {
                    section_coeffs(section)
                        .iter()
                        .map(|value| format!("{value:.17e}"))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("write peq sections");
    }
}

// ---------------------------------------------------------------------------
// 3) IIR 表 CRC / graph_id 绑定（篡改必拒）
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_iir_table_crc_and_graph_id_bind_iir_content() {
    let plan = compile_ok(&peq_fixture(vec![peq_section(100.0, -6.0, 1.0)]));
    let changed_gain = compile_ok(&peq_fixture(vec![peq_section(100.0, -8.0, 1.0)]));
    assert_ne!(plan.graph_id, changed_gain.graph_id, "PEQ 影响 graph_id");
    assert_ne!(
        plan.plan_sha256(),
        changed_gain.plan_sha256(),
        "PEQ 影响 plan 字节"
    );

    // (a) 篡改 IIR 表 CRC 字段 → 自校验拒收（host 侧同口径 ERR_HASH）。
    let mut tampered = plan.clone();
    let mut bytes = plan.plan_bytes.clone();
    let iir_offset = plan.header.reserved3[0] as usize;
    bytes[iir_offset + 12] ^= 0xFF;
    tampered.plan_bytes = bytes;
    let error = tampered
        .verify()
        .expect_err("tampered IIR table CRC must be rejected");
    assert_code(&error, E_CA_PLAN_VERIFY);

    // (b) 篡改 IIR 系数（表 CRC 未同步）→ 表 CRC/系数绑定失败。
    let mut tampered = plan.clone();
    let mut bytes = plan.plan_bytes.clone();
    let coeff_base = plan.plan_bytes.len() - plan.coeffs.len();
    let at = coeff_base + plan.iir_entries[0].coeff_offset_bytes as usize;
    bytes[at] ^= 0x01;
    tampered.plan_bytes = bytes;
    let error = tampered
        .verify()
        .expect_err("tampered IIR coefficient must be rejected");
    assert_code(&error, E_CA_PLAN_VERIFY);

    // (c) role.peq_count 与 IIR 表项段数不一致 → verify 拒收坏图。
    let mut tampered = plan.clone();
    tampered.roles[0].peq_count = 3;
    let error = tampered
        .verify()
        .expect_err("role.peq_count != IIR section_count must be rejected");
    assert_code(&error, E_CA_PLAN_VERIFY);
    assert!(error.field().contains("peq_count"), "{}", error.field());
}

// ---------------------------------------------------------------------------
// 4) 段数声明 / 稳定性 / 32 段上限
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_section_count_and_stability_fail_closed() {
    let section = peq_section(100.0, -6.0, 1.0);

    // 无系数的 peq_count>0（旧 v1 守卫）仍拒收。
    let error = RoleCompileInput::new(0, "FL", 0, Some(0), 0, 1, -1.0, vec![1.0], 1)
        .expect_err("peq_count>0 without sections must be rejected");
    assert_code(&error, E_CA_PEQ_ABI);

    // 声明段数 != 提供段数 → 拒收（with_peq_sections 交叉绑定）。
    let mut role = RoleCompileInput::new(0, "FL", 0, Some(0), 0, 1, -1.0, vec![1.0], 0)
        .expect("role without peq");
    role.peq_count = 3;
    let error = role
        .with_peq_sections(vec![section, section])
        .expect_err("declared peq_count mismatch must be rejected");
    assert_code(&error, E_CA_PEQ_ABI);

    // 不稳定 biquad 拒收（|a2|>=1）。
    assert!(
        PeqBiquad::new(1.0, 0.0, 0.0, 0.0, 1.2).is_err(),
        "unstable biquad must be rejected"
    );
    let unstable_role = RoleCompileInput::new(0, "FL", 0, Some(0), 0, 1, -1.0, vec![1.0], 0)
        .expect("role without peq")
        .with_peq_sections(vec![PeqBiquad {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 1.2,
        }]);
    assert!(unstable_role.is_err(), "unstable section must be rejected");

    // 33 段 > PLAN_IIR_MAX_SECTIONS(32) 拒收；32 段接受。
    let too_many = vec![section; 33];
    let role = RoleCompileInput::new(0, "FL", 0, Some(0), 0, 1, -1.0, vec![1.0], 0)
        .expect("role without peq");
    let error = role
        .with_peq_sections(too_many)
        .expect_err("33 sections must be rejected");
    assert_code(&error, E_CA_PEQ_ABI);

    let plan = compile_ok(&peq_fixture(vec![peq_section(1000.0, -0.1, 2.0); 32]));
    assert_eq!(plan.header.abi_version, KHAEN_PLAN_ABI_V2);
    assert_eq!(plan.iir_entries[0].section_count, 32);
    assert_eq!(plan.roles[0].peq_count, 32);
    plan.verify().expect("32-section plan must verify");
}

// ---------------------------------------------------------------------------
// 5) 系数预算 fail-closed（IIR 系数计入 plan 预算）
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_coeff_budget_includes_iir_and_fails_closed() {
    let baseline = compile_ok(&Fixture::stereo(DelayPolarityEncoding::RoleMetadata));
    let base_len = baseline.coeffs.len();

    let mut fixture = peq_fixture(vec![peq_section(100.0, -6.0, 1.0)]);
    fixture.options.coeff_budget_bytes = base_len as u64 + 32;
    let error = compile_plan(&fixture.request())
        .expect_err("IIR coeffs must count against plan coeff budget");
    assert_code(&error, E_CA_CAPACITY);

    fixture.options.coeff_budget_bytes = base_len as u64 + 4096;
    let plan = compile_plan(&fixture.request()).expect("budget with IIR must compile");
    assert_eq!(plan.header.abi_version, KHAEN_PLAN_ABI_V2);
    assert!(plan.coeffs.len() > base_len);
}

// ---------------------------------------------------------------------------
// 6) T64 报告绑定：band_count / 逐段 b/a / bands 缺失 全部 fail-closed
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_report_band_count_must_match_plan_sections() {
    let sections = vec![peq_section(100.0, -6.0, 1.0)];
    let fixture = peq_fixture(sections);
    // 报告 band_count=0（不写 bands）但 plan 带 1 段 → 段数绑定失败。
    let mut hidden = peq_fixture(vec![peq_section(100.0, -6.0, 1.0)]);
    hidden.headroom =
        headroom_with_bound_peq_report(&hidden.roles, T64_REPORT_PROGRAM_PEAK_DBFS, false);
    let error = compile_plan(&hidden.request())
        .expect_err("report band_count=0 vs plan IIR sections must be rejected");
    assert_code(&error, E_CA_PEQ_ABI);

    // 报告 band_count 与 plan 段数一致但系数不同 → 逐段 b/a 绑定失败。
    let mut mismatch = peq_fixture(vec![peq_section(100.0, -8.0, 1.0)]);
    mismatch.headroom = fixture.headroom.clone();
    let error = compile_plan(&mismatch.request())
        .expect_err("plan IIR coeffs != report bands must be rejected");
    assert_code(&error, E_CA_PEQ_ABI);

    // bands[] 字段被裁掉（band_count>0 但无系数）→ 证据不可判定，拒收。
    let mut missing_bands = peq_fixture(vec![peq_section(100.0, -6.0, 1.0)]);
    missing_bands.headroom = headroom_with_bound_peq_report(
        &missing_bands.roles,
        T64_REPORT_PROGRAM_PEAK_DBFS,
        true,
    );
    // 人工删掉报告 bytes 中的 bands 数组不可行；改为 band_count/bands 不一致的构造：
    let bytes = missing_bands
        .headroom
        .t64_report_bytes()
        .expect("bound report bytes")
        .to_vec();
    let text = String::from_utf8(bytes).expect("report json utf8");
    let patched = text.replace("\"bands\":[", "\"bands_removed\":[");
    let patched_bytes = patched.into_bytes();
    missing_bands.headroom = HeadroomEvidence::new(
        PEQ_FIT_SCHEMA,
        crate::job::sha256_hex(&patched_bytes),
        missing_bands.headroom.roles.clone(),
    )
    .with_t64_report_bytes(patched_bytes);
    let error = compile_plan(&missing_bands.request())
        .expect_err("report bands missing must be rejected (cannot bind IIR coeffs)");
    assert_code(&error, E_CA_EVIDENCE);
}

// ---------------------------------------------------------------------------
// 7) plan 未承载 IIR 但 T64 报告有段：生产拒收（禁止静默丢弃）；实验旁路如实标记
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_missing_iir_sections_is_rejected_unless_experiment() {
    // 报告有 FL IIR 段，但 plan roles 不带 sections（旧行为）→ 生产拒收。
    let mut fixture = Fixture::stereo(DelayPolarityEncoding::RoleMetadata);
    let peq = peq_fixture(vec![peq_section(100.0, -6.0, 1.0)]);
    fixture.headroom = peq.headroom.clone();
    let error = compile_plan(&fixture.request())
        .expect_err("T64 IIR bands without plan IIR must be rejected (F-37)");
    assert_code(&error, E_CA_PEQ_ABI);

    // 显式实验旁路：不拒收，但记 W_EXPERIMENT_IIR_EXCLUDED，plan 仍 v1、envelope 如实 false。
    let request = fixture.request().with_experiment(ExperimentMode::SINGLE_POINT);
    let plan = compile_plan(&request).expect("experiment bypass must compile");
    assert_eq!(plan.header.abi_version, KHAEN_PLAN_ABI_V1);
    assert!(plan.d7_warnings.contains(&W_EXPERIMENT_IIR_EXCLUDED));
    let envelope = plan.experiment_envelope_json();
    assert!(envelope.contains("\"t64_correction_in_plan\": false"));
    assert!(!envelope.contains("\"plan_abi\""));
}

// ---------------------------------------------------------------------------
// 8) 实验 envelope：有 PEQ 时 t64_correction_in_plan=true 且写 plan_abi/iir_sections
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_experiment_envelope_reports_t64_correction_in_plan() {
    let fixture = peq_fixture(vec![peq_section(100.0, -6.0, 1.0)]);
    let request = fixture.request().with_experiment(ExperimentMode::SINGLE_POINT);
    let plan = compile_plan(&request).expect("experiment plan with PEQ must compile");
    assert_eq!(plan.header.abi_version, KHAEN_PLAN_ABI_V2);
    assert!(!plan
        .d7_warnings
        .contains(&W_EXPERIMENT_IIR_EXCLUDED));
    let envelope = plan.experiment_envelope_json();
    assert!(envelope.contains("\"t64_correction_in_plan\": true"));
    assert!(envelope.contains("\"plan_abi\": 2"));
    assert!(envelope.contains("\"iir_sections\": 1"));
    assert!(envelope.contains("EXPERIMENT"));
    assert!(envelope.contains("NON_ACCEPTABLE"));
    // 无 PEQ 实验（历史路径）仍写 false，且报告字节不含 IIR 字段（旧字节不变）。
    let mut plain = Fixture::stereo(DelayPolarityEncoding::RoleMetadata);
    plain.headroom = headroom_with_bound_peq_report(
        &plain.roles,
        T64_REPORT_PROGRAM_PEAK_DBFS,
        true,
    );
    let plain_plan = compile_plan(&plain.request().with_experiment(ExperimentMode::SINGLE_POINT))
        .expect("plain experiment plan");
    let plain_envelope = plain_plan.experiment_envelope_json();
    assert!(plain_envelope.contains("\"t64_correction_in_plan\": false"));
    assert!(!plain_envelope.contains("\"plan_abi\""));
}

// ---------------------------------------------------------------------------
// 9) compile-report IIR 摘要与 plan 一致（无 IIR 不出现）
// ---------------------------------------------------------------------------

#[test]
fn t66_peq_plan_compile_report_iir_summary_matches_plan() {
    let sections = vec![peq_section(100.0, -6.0, 1.0), peq_section(200.0, -3.0, 2.0)];
    let plan = compile_ok(&peq_fixture(sections));
    let summary = plan.report.iir.as_ref().expect("report must carry IIR summary");
    assert_eq!(summary.abi_version, KHAEN_PLAN_ABI_V2);
    assert_eq!(summary.role_count, 1);
    assert_eq!(summary.section_count, 2);
    assert_eq!(summary.roles, vec![("FL".to_string(), 2u16)]);
    assert_eq!(summary.table_offset, plan.header.reserved3[0]);
    assert_eq!(
        summary.table_sha256,
        crate::job::sha256_hex(&plan.iir_table)
    );
    assert!(plan.report.canonical().contains("iir abi=2"));
    assert!(plan.report.to_json().contains("\"iir\": {"));
}
