//! T23 nodes 私有测试（WBS T23；写域 crates/core/tests/T23/）。
//!
//! 覆盖 WBS T23 DoD：
//! - 节点链规范序（A_map → G → P_post(crossover→modal PEQ→target FIR→gain→delay→polarity)
//!   → volume ramp → limiter/clip/ramp → scatter），节点数上界；
//! - 只选择/零填/role scatter：无上混/下混/自动造声道，stereo_strict 独立受限 profile，
//!   G 不得向非允许角色生成输出（禁止 runtime mask 破坏预测）；
//! - 输出 FIR 段合成（crossover/target/phase 线性卷积、段序、支撑/延迟/预算），
//!   role gain/polarity 折叠单次应用，role delay 单一字段；
//! - 模态 IIR：RBJ 自洽、极点稳定、requires_f64（V15 0.19646/0.03974 dB）、不硬截 FIR、
//!   与目标 FIR 的 LTI 交换（kernel 单 P FIR 接口）；
//! - volume ramp 契约（-150..+50、ramp dB 线性插值、mute 与保护静音区分）；
//! - limiter/clip/ramp 保护不可绕过、P14 未定项标记；
//! - 与 T12 compile 图、T07 direct / T08 partitioned / T09 workers 内核接口一致
//!   （DirectTopology/系数/计划投影逐样本对拍）；
//! - 未按 role 名硬编码：重命名不改变图语义，高度无默认 HP/LP/人工 delay。
//!
//! 自包含（无第三方依赖）；Cargo 自动把 tests/T23/main.rs 发现为集成测试 target。
//! 运行：cargo test -p khaen-core nodes

use std::sync::atomic::AtomicU32;

use khaen_core::abi::KhaenBlockAckV1;
use khaen_core::compile::{CompileOptions, FusionForm, COMPILE_NO_SOURCE};
use khaen_core::direct::{
    direct_block_f32, direct_block_f64, DirectCoeffsF32, DirectCoeffsF64, DirectLayout,
    DirectTopology,
};
use khaen_core::nodes::{
    compose_fir_segments, crc32c, db_to_linear, modal_freq_upper_hz, scatter_zero_fill_containers,
    volume_gain_db_at_frame, volume_linear_gain_at_frame, volume_ramp_db_step, ClipGuardConfig,
    FirSegment, FirSegmentKind, GraphConfig, LimiterConfig, LowBandConfig, LoweredGraph,
    ModalBandConfig, ModalBandType, ModalIirChannel, NodeGraph, NodeKind, OutputFirChain,
    ProtectionConfig, ProtectionRampConfig, RoleConfig, RolePlan, RuntimeStage, VolumeRampConfig,
    F32_QUANTIZATION_TOL_DB, MODAL_BIQUADS_MAX, OUTPUT_FIR_SEGMENTS_MAX, POLE_RADIUS_MAX,
    ROLE_FLAG_ENABLED, ROLE_FLAG_ZERO_FILL,
};
use khaen_core::partitioned::{partitioned_block_f32, PartitionedLayout, PartitionedPlan};
use khaen_core::rt::RtStatus;
use khaen_core::workers::{
    execute_block_scheduled, WorkerDagPlan, WorkerPartials, WorkerScheduleStats,
    WORKER_ACK_FLAG_VALID,
};

const FS: u32 = 48_000;
const BLOCK: usize = 128;

// ---------------------------------------------------------------------------
// 图构造辅助
// ---------------------------------------------------------------------------

fn options() -> CompileOptions {
    CompileOptions {
        sample_rate_hz: FS,
        block_size: BLOCK as u16,
        max_taps: 4096,
        extended_taps_admitted: false,
        coeff_budget_bytes: 4 * 1024 * 1024,
    }
}

fn extended_options() -> CompileOptions {
    CompileOptions {
        sample_rate_hz: FS,
        block_size: BLOCK as u16,
        max_taps: 16_384,
        extended_taps_admitted: true,
        coeff_budget_bytes: 4 * 1024 * 1024,
    }
}

fn limiter() -> LimiterConfig {
    LimiterConfig::new(-1.0, 50.0, 6.0)
        .with_attack_ms(1.0)
        .with_lookahead_frames(0)
}

fn protection(n_role: usize) -> ProtectionConfig {
    ProtectionConfig {
        limiter: vec![limiter(); n_role],
        clip: ClipGuardConfig::new(-1.0),
        ramp: ProtectionRampConfig::new(128),
    }
}

fn volume() -> VolumeRampConfig {
    VolumeRampConfig {
        volume_db: -6.0,
        user_mute: false,
        ramp_frames: 240,
    }
}

fn role(src: u16, out_index: u16) -> RoleConfig {
    RoleConfig {
        src,
        out_index,
        gain_db: 0.0,
        trim_db: 0.0,
        delay_samples: 0.0,
        polarity: 1,
        name: None,
    }
}

fn identity_chains(n_role: usize) -> Vec<OutputFirChain> {
    vec![OutputFirChain::identity(); n_role]
}

fn empty_iir(n_role: usize) -> Vec<ModalIirChannel> {
    vec![ModalIirChannel::empty(); n_role]
}

fn empty_low_band(participants: Vec<u16>, l_g: u32, d_samples: u32) -> LowBandConfig {
    let m = participants.len();
    LowBandConfig {
        participants,
        g: vec![0.0; m * m * l_g as usize],
        l_g,
        d_samples,
    }
}

/// 2.0 stereo strict 受限 profile：8 容器 / 7 物理角色，FL/FR 有源，其余显式零填。
/// 布局取自 DSP-HOST-DECISION §1/§3（FL0 FR1 SL2 SR3 HFL4 HFR5 ch6 空 SUB1=7），但
/// 本测试只通过显式 out_index/mask 表达，不依赖名字。
fn stereo_graph() -> NodeGraph {
    let names = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];
    let out_indices = [0u16, 1, 2, 3, 4, 5, 7];
    let mut roles = Vec::new();
    for index in 0..7 {
        let mut role = role(
            if index < 2 {
                index as u16
            } else {
                COMPILE_NO_SOURCE
            },
            out_indices[index],
        );
        role.name = Some(names[index].to_string());
        roles.push(role);
    }
    let mut config = GraphConfig::new(options(), 6.0);
    config.stereo_strict = true;
    config.allowed_role_mask = 0b11;
    NodeGraph {
        n_src: 2,
        container_channels: 8,
        roles,
        a: vec![1.0, 1.0],
        l_a: 1,
        low_band: empty_low_band(vec![0, 1], 1, 0),
        output_fir: identity_chains(7),
        modal_iir: empty_iir(7),
        volume: volume(),
        protection: protection(7),
        config,
    }
}

/// 目标多声道 profile：7 角色 / 8 容器（ch6 空），全部有源，5x5 低带 G（D=4），
/// 每角色一个 8 tap 目标 FIR；FL 另加 4 tap crossover（测试段合成）。
fn multichannel_graph() -> NodeGraph {
    let out_indices = [0u16, 1, 2, 3, 4, 5, 7];
    let roles: Vec<RoleConfig> = (0..7)
        .map(|index| role(index as u16, out_indices[index]))
        .collect();
    let mut low_band = empty_low_band(vec![0, 1, 2, 3, 6], 16, 4);
    let m = 5usize;
    for row in 0..m {
        for col in 0..m {
            if (row + col) % 3 != 0 {
                low_band.g[(row * m + col) * 16] = 0.02 * (row as f64 + 1.0) - 0.01 * col as f64;
            }
        }
    }
    let target = vec![1.0, 0.1, -0.05, 0.02, 0.0, 0.0, 0.0, 0.0];
    let mut chains: Vec<OutputFirChain> = (0..7)
        .map(|_| {
            OutputFirChain::single(FirSegment::new(
                FirSegmentKind::Target,
                target.clone(),
                0,
                0,
            ))
        })
        .collect();
    chains[0] = OutputFirChain {
        segments: vec![
            FirSegment::new(FirSegmentKind::Crossover, vec![0.5, 0.3, 0.1, 0.05], 0, 0),
            FirSegment::new(FirSegmentKind::Target, target, 0, 0),
        ],
    };
    NodeGraph {
        n_src: 7,
        container_channels: 8,
        roles,
        a: vec![1.0; 7],
        l_a: 1,
        low_band,
        output_fir: chains,
        modal_iir: empty_iir(7),
        volume: VolumeRampConfig {
            volume_db: 0.0,
            user_mute: false,
            ramp_frames: 0,
        },
        protection: protection(7),
        config: GraphConfig::new(options(), 6.0),
    }
}

/// 最小 1 源 1 角色 4 容器（scatter 到容器 2）图：用于零填/延迟/volume 等小测试。
fn tiny_graph() -> NodeGraph {
    NodeGraph {
        n_src: 1,
        container_channels: 4,
        roles: vec![role(0, 2)],
        a: vec![1.0],
        l_a: 1,
        low_band: empty_low_band(vec![0], 1, 0),
        output_fir: identity_chains(1),
        modal_iir: empty_iir(1),
        volume: VolumeRampConfig {
            volume_db: 0.0,
            user_mute: false,
            ramp_frames: 0,
        },
        protection: protection(1),
        config: GraphConfig::new(options(), 6.0),
    }
}

fn lower_ok(graph: &NodeGraph) -> LoweredGraph {
    match graph.lower() {
        Ok(lowered) => lowered,
        Err(status) => panic!("expected graph to lower, got {:?}", status),
    }
}

fn lower_err(graph: &NodeGraph) -> RtStatus {
    match graph.lower() {
        Ok(_) => panic!("expected graph to fail"),
        Err(status) => status,
    }
}

fn target_chain(coefficients: Vec<f64>) -> OutputFirChain {
    OutputFirChain::single(FirSegment::new(FirSegmentKind::Target, coefficients, 0, 0))
}

fn expected_p_row(graph: &NodeGraph, role_index: usize, l_p: usize) -> Vec<f32> {
    let chain = &graph.output_fir[role_index];
    let mut row = vec![0.0f32; l_p];
    let composed = compose_fir_segments(&chain.segments).expect("compose segments");
    let gain = graph.roles[role_index].effective_gain_db();
    let linear = db_to_linear(gain);
    let polarity = graph.roles[role_index].polarity as f64;
    for (tap, &value) in composed.iter().enumerate() {
        row[tap] = (value * linear * polarity) as f32;
    }
    row
}

fn impulse_input(n_src: usize) -> Vec<f32> {
    let mut input = vec![0.0f32; n_src * BLOCK];
    for source in 0..n_src {
        input[source * BLOCK] = 1.0;
    }
    input
}

// ---------------------------------------------------------------------------
// 节点链 / 报告
// ---------------------------------------------------------------------------

#[test]
fn nodes_chain_canonical_order_and_node_budget() {
    // 规范链序：12 个 NodeKind，rank 严格递增，schema 名稳定。
    let order = NodeKind::canonical_order();
    assert_eq!(order.len(), 12);
    for (index, kind) in order.iter().enumerate() {
        assert_eq!(kind.rank(), index as u8, "rank mismatch at {}", index);
    }
    let names: Vec<&str> = order.iter().map(|kind| kind.schema_name()).collect();
    assert_eq!(
        names,
        vec![
            "input_select",
            "low_band_matrix",
            "output_fir",
            "modal_peq_iir",
            "gain",
            "delay",
            "polarity",
            "volume_ramp",
            "limiter",
            "clip_guard",
            "protection_ramp",
            "scatter",
        ]
    );
    assert!(NodeKind::Gain.is_folded());
    assert!(NodeKind::Polarity.is_folded());
    assert!(!NodeKind::OutputFir.is_folded());

    let graph = multichannel_graph();
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    assert_eq!(plan.node_count(), plan.nodes.len());
    assert!(plan.node_count() <= 256);
    // 节点链 rank 非降；scatter 最后；volume 在 limiter 之前；保护全部在 scatter 之前。
    let mut last = 0u8;
    for node in &plan.nodes {
        assert!(
            node.kind.rank() >= last,
            "node order regressed at {:?}",
            node.kind
        );
        last = node.kind.rank();
    }
    assert_eq!(
        plan.nodes.first().map(|node| node.kind),
        Some(NodeKind::InputSelect)
    );
    assert_eq!(
        plan.nodes.last().map(|node| node.kind),
        Some(NodeKind::Scatter)
    );
    let volume_index = plan
        .nodes
        .iter()
        .position(|node| node.kind == NodeKind::VolumeRamp)
        .expect("volume ramp node");
    let limiter_index = plan
        .nodes
        .iter()
        .position(|node| node.kind == NodeKind::Limiter)
        .expect("limiter node");
    assert!(volume_index < limiter_index);
    // 折叠的 gain/polarity 不得作为独立节点出现。
    assert!(plan.nodes.iter().all(|node| !node.kind.is_folded()));
    // 每角色都有三个保护节点。
    for index in 0..7u16 {
        assert!(plan
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Limiter && node.role == Some(index)));
        assert!(plan
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::ClipGuard && node.role == Some(index)));
        assert!(plan
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::ProtectionRamp && node.role == Some(index)));
    }
    // 2 全局 + 7 output FIR + 1 volume + 21 保护 + 1 scatter = 32。
    assert_eq!(plan.node_count(), 32);
}

#[test]
fn nodes_report_facts_consistent_with_plan() {
    let graph = multichannel_graph();
    let lowered = lower_ok(&graph);
    let report = &lowered.report;
    let plan = &lowered.plan;
    assert_eq!(report.node_count as usize, plan.node_count());
    assert_eq!(report.n_src, 7);
    assert_eq!(report.n_role, 7);
    assert_eq!(report.container_channels, 8);
    assert_eq!(report.zero_fill_roles, Vec::<u16>::new());
    assert_eq!(report.zero_fill_containers, vec![6u16]);
    assert_eq!(plan.zero_fill_containers, vec![6u16]);
    assert_eq!(report.l_a, 1);
    assert_eq!(report.l_g, 16);
    assert_eq!(report.l_p, 11);
    assert_eq!(report.output_fir_support[0], 11);
    for index in 1..7 {
        assert_eq!(report.output_fir_support[index], 8);
    }
    assert_eq!(report.role_delay_q16, vec![0u64; 7]);
    assert_eq!(report.total_delay_q16, vec![0u64; 7]);
    assert_eq!(report.role_gain_db, vec![0.0f64; 7]);
    assert_eq!(report.role_polarity, vec![1i8; 7]);
    assert!(report.modal_biquads.iter().all(|count| *count == 0));
    assert!(report.modal_f32_admissible);
    assert!(report.kernel_admissible);
    assert!(report.runtime_f32_admissible);
    assert!(report.kernel_error.is_none());
    assert_eq!(report.protection_roles, 7);
    assert!(!report.protection_unresolved_p14);
    assert_eq!(report.volume_db, 0.0);
    assert!(!report.user_mute);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("role gain/polarity")));
    // 系数字节：a 7 + g 5*5*16=400 + p 7*11=77 个 f32。
    assert_eq!(report.coeff_bytes_f32, (7 + 400 + 77) as u64 * 4);
    assert_eq!(lowered.selected_form(), report.compile.chosen);
}

// ---------------------------------------------------------------------------
// 选择/零填/scatter：无上混下混、无自动造声道、stereo strict
// ---------------------------------------------------------------------------

#[test]
fn nodes_selection_only_no_mixer_fanout() {
    let mut graph = tiny_graph();
    // 一源分发到两个角色 = 节目分发/上混，必须拒收。
    graph.roles = vec![role(0, 0), role(0, 1)];
    graph.container_channels = 2;
    graph.output_fir = identity_chains(2);
    graph.modal_iir = empty_iir(2);
    graph.protection = protection(2);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 两个角色映射同一容器 = 非单射 scatter，必须拒收。
    let mut graph = tiny_graph();
    graph.roles = vec![role(0, 0), role(COMPILE_NO_SOURCE, 0)];
    graph.container_channels = 2;
    graph.output_fir = identity_chains(2);
    graph.modal_iir = empty_iir(2);
    graph.protection = protection(2);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 容器数少于角色数 = 会被迫合并/丢弃通道，必须拒收。
    let mut graph = tiny_graph();
    graph.container_channels = 0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_CHANNELS);

    // 额外未被 A_map 选中的源不会自动造声道：角色数仍由配置决定。
    let mut graph = tiny_graph();
    graph.n_src = 2;
    graph.a = vec![1.0, 1.0];
    let lowered = lower_ok(&graph);
    assert_eq!(lowered.report.n_src, 2);
    assert_eq!(lowered.report.n_role, 1);
    assert_eq!(lowered.plan.topology.expect("topology").n_src, 2);
    assert_eq!(lowered.plan.topology.expect("topology").n_role, 1);
}

#[test]
fn nodes_zero_fill_roles_and_containers_are_explicit() {
    let graph = stereo_graph();
    let lowered = lower_ok(&graph);
    let report = &lowered.report;
    assert_eq!(report.zero_fill_roles, vec![2u16, 3, 4, 5, 6]);
    assert_eq!(report.zero_fill_containers, vec![6u16]);
    for index in 2..7 {
        let flags = lowered.plan.roles[index].flags;
        assert_ne!(flags & ROLE_FLAG_ENABLED, 0);
        assert_ne!(
            flags & ROLE_FLAG_ZERO_FILL,
            0,
            "role {} must be zero_fill",
            index
        );
    }
    for index in 0..2 {
        let flags = lowered.plan.roles[index].flags;
        assert_ne!(flags & ROLE_FLAG_ENABLED, 0);
        assert_eq!(flags & ROLE_FLAG_ZERO_FILL, 0);
    }
    // 零填角色 identity 输出：无默认 HP/LP/延迟/增益。
    for index in 2..7 {
        let diag = &lowered.plan.diags[index];
        assert_eq!(diag.taps, 1);
        assert_eq!(diag.coeffs_f32, vec![1.0f32]);
        assert_eq!(diag.flags, 0);
        assert_eq!(diag.delay_samples, 0);
        assert_eq!(diag.delay_q16, 0);
        assert_eq!(lowered.plan.roles[index].delay_samples, 0);
        assert_eq!(lowered.plan.roles[index].effective_gain_db, 0.0);
    }
}

#[test]
fn nodes_stereo_strict_rejects_program_outside_mask() {
    // 有源角色不在允许 mask -> 拒收。
    let mut graph = stereo_graph();
    graph.n_src = 3;
    graph.a = vec![1.0, 1.0, 1.0];
    graph.roles[2].src = 2;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 允许 mask 内角色无源 -> 拒收（allowed 必须正好是节目输出集合）。
    let mut graph = stereo_graph();
    graph.roles[1].src = COMPILE_NO_SOURCE;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 零填角色上出现非 identity 输出 FIR -> 拒收（不得用 coeff 造输出）。
    let mut graph = stereo_graph();
    graph.output_fir[3] = target_chain(vec![0.5, 0.5]);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 零填角色上出现 IIR -> 拒收。
    let mut graph = stereo_graph();
    let band = ModalBandConfig::from_params(ModalBandType::Peaking, 100.0, 2.0, -3.0, FS as f64)
        .expect("band");
    graph.modal_iir[4] = ModalIirChannel::new(vec![band]);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 零填角色上出现非零增益/延迟 -> 拒收。
    let mut graph = stereo_graph();
    graph.roles[5].gain_db = -3.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);
    let mut graph = stereo_graph();
    graph.roles[5].delay_samples = 1.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 低带 G 的 participant 不得在 mask 外（禁止 runtime mask 已有 G）。
    let mut graph = stereo_graph();
    graph.low_band = empty_low_band(vec![0, 1, 2], 1, 0);
    graph.low_band.g[2 * 3] = 0.1;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // mask 为空 -> 拒收。
    let mut graph = stereo_graph();
    graph.config.allowed_role_mask = 0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 允许的 2.0 profile 正常通过（上面 graph 本身）。
    lower_ok(&stereo_graph());
}

#[test]
fn nodes_stereo_strict_g_cannot_reach_other_outputs() {
    // participants 在 mask 内但 g 行指向非 mask 角色在结构上不可能（participants 即行索引），
    // 这里覆盖显式非默认 profile：modal_support_to_other_outputs 需要显式裁决。
    let mut graph = stereo_graph();
    graph.config.modal_support_to_other_outputs = true;
    graph.config.explicit_profile_approved = false;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    graph.config.explicit_profile_approved = true;
    graph.low_band = empty_low_band(vec![0, 1, 2], 4, 1);
    graph.low_band.g[(2 * 3 + 0) * 4] = 0.1;
    let lowered = lower_ok(&graph);
    assert!(lowered
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("modal_support_to_other_outputs")));
    assert_eq!(lowered.report.n_role, 7);
}

#[test]
fn nodes_role_name_never_affects_semantics_and_heights_are_fullrange() {
    // 同名图重命名（含把高度换成奇怪名字/去掉名字）不改变图语义，只改 name_hash。
    let mut named = multichannel_graph();
    let names = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];
    for (index, role) in named.roles.iter_mut().enumerate() {
        role.name = Some(names[index].to_string());
    }
    let mut renamed = named.clone();
    for (index, role) in renamed.roles.iter_mut().enumerate() {
        role.name = Some(format!("role_{}", index));
    }
    let mut unnamed = named.clone();
    for role in unnamed.roles.iter_mut() {
        role.name = None;
    }
    let a = lower_ok(&named).plan;
    let b = lower_ok(&renamed).plan;
    let c = lower_ok(&unnamed).plan;
    let a_hash: Vec<u32> = a.roles.iter().map(|role| role.name_hash).collect();
    let b_hash: Vec<u32> = b.roles.iter().map(|role| role.name_hash).collect();
    assert_ne!(a_hash, b_hash);
    assert!(c.roles.iter().all(|role| role.name_hash == 0));
    assert_eq!(a.diags, b.diags);
    assert_eq!(a.diags, c.diags);
    assert_eq!(a.coeffs, b.coeffs);
    assert_eq!(a.runtime_stages, b.runtime_stages);
    assert_eq!(a.topology, b.topology);
    let strip_hashes = |roles: &[RolePlan]| -> Vec<RolePlan> {
        roles
            .iter()
            .map(|role| RolePlan {
                name_hash: 0,
                ..role.clone()
            })
            .collect()
    };
    assert_eq!(strip_hashes(&a.roles), strip_hashes(&b.roles));
    assert_eq!(strip_hashes(&a.roles), strip_hashes(&c.roles));

    // 高度角色默认全频：没有显式 FIR 段时就是 identity，无 HP/LP/18ms/默认延迟。
    let mut heights = multichannel_graph();
    heights.output_fir = identity_chains(7);
    heights.roles[4].name = Some("HFL".to_string());
    heights.roles[5].name = Some("HFR".to_string());
    let height_plan = lower_ok(&heights).plan;
    for index in [4usize, 5] {
        let diag = &height_plan.diags[index];
        assert_eq!(diag.taps, 1);
        assert_eq!(diag.coeffs_f32, vec![1.0f32]);
        assert_eq!(diag.flags, 0);
        assert_eq!(diag.delay_samples, 0);
        assert_eq!(diag.delay_q16, 0);
        assert_eq!(height_plan.roles[index].delay_samples, 0);
    }
}

// ---------------------------------------------------------------------------
// 输出 FIR 段合成
// ---------------------------------------------------------------------------

#[test]
fn nodes_output_fir_segments_compose_linearly() {
    let mut graph = tiny_graph();
    let crossover = vec![0.5, 0.3, 0.1];
    let target = vec![1.0, -0.25, 0.0, 0.125];
    let phase = FirSegment::new(
        FirSegmentKind::PhaseDelayPolarity,
        vec![0.0, 0.0, 1.0],
        2,
        0,
    );
    graph.output_fir[0] = OutputFirChain {
        segments: vec![
            FirSegment::new(FirSegmentKind::Crossover, crossover.clone(), 1, 0),
            FirSegment::new(FirSegmentKind::Target, target.clone(), 0, 0),
            phase,
        ],
    };
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    // L_P = 3 + 4 + 3 - (3-1) = 8。
    assert_eq!(plan.l_p, 8);
    assert_eq!(lowered.report.l_p, 8);
    let expected = compose_fir_segments(&graph.output_fir[0].segments).expect("compose");
    assert_eq!(expected.len(), 8);
    let diag = &plan.diags[0];
    assert_eq!(diag.taps, 8);
    assert_eq!(diag.support_taps, 8);
    assert_eq!(
        diag.coeffs_f32,
        expected
            .iter()
            .map(|value| *value as f32)
            .collect::<Vec<f32>>()
    );
    // cursor/target/phase 位都在；声明延迟 = 1 + 0 + 2 = 3 samples。
    assert_ne!(diag.flags & 0x04, 0, "crossover bit");
    assert_ne!(diag.flags & 0x08, 0, "target bit");
    assert_ne!(diag.flags & 0x10, 0, "phase bit");
    assert_eq!(diag.delay_samples, 3);
    assert_eq!(diag.delay_q16, 0);
    assert_eq!(lowered.report.output_fir_delay_q16[0], 3 * 65536);
    // T12 factorized 输出阶段（P）与 diag 投影一致。
    assert_eq!(plan.output_stages[0].coeffs_f32, diag.coeffs_f32);
    assert_eq!(plan.output_stages[0].taps, 8);
}

#[test]
fn nodes_output_fir_rejects_bad_segments() {
    // 段序必须非降（target 在 crossover 之前 = 重排未证明）。
    let mut graph = tiny_graph();
    graph.output_fir[0] = OutputFirChain {
        segments: vec![
            FirSegment::new(FirSegmentKind::Target, vec![1.0], 0, 0),
            FirSegment::new(FirSegmentKind::Crossover, vec![1.0], 0, 0),
        ],
    };
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);

    // taps 与系数长度不符。
    let mut graph = tiny_graph();
    let mut segment = FirSegment::new(FirSegmentKind::Target, vec![1.0, 0.5], 0, 0);
    segment.taps = 3;
    graph.output_fir[0] = OutputFirChain::single(segment);
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);

    // 非有限系数。
    let mut graph = tiny_graph();
    graph.output_fir[0] = target_chain(vec![1.0, f64::NAN]);
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);

    // 声明延迟 >= 支撑（因果/支撑矛盾）。
    let mut graph = tiny_graph();
    graph.output_fir[0] = OutputFirChain::single(FirSegment::new(
        FirSegmentKind::Target,
        vec![1.0, 0.0],
        2,
        0,
    ));
    assert_eq!(lower_err(&graph), RtStatus::ERR_TAPS);

    // 段数超结构上限。
    let mut graph = tiny_graph();
    let many: Vec<FirSegment> = (0..=OUTPUT_FIR_SEGMENTS_MAX)
        .map(|_| FirSegment::identity(FirSegmentKind::Target))
        .collect();
    graph.output_fir[0] = OutputFirChain { segments: many };
    assert_eq!(lower_err(&graph), RtStatus::ERR_CAPACITY);

    // 支撑超 profile max_taps。
    let mut graph = tiny_graph();
    graph.output_fir[0] = target_chain(vec![1.0; 4097]);
    assert_eq!(lower_err(&graph), RtStatus::ERR_TAPS);
}

// ---------------------------------------------------------------------------
// gain / polarity / delay 单次应用
// ---------------------------------------------------------------------------

#[test]
fn nodes_gain_and_polarity_folded_single_application() {
    let mut graph = stereo_graph();
    graph.roles[0].gain_db = 6.0;
    graph.roles[1].gain_db = -6.0;
    graph.roles[1].polarity = -1;
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    let left = db_to_linear(6.0);
    let right = -db_to_linear(-6.0);
    assert!((plan.diags[0].coeffs_f32[0] as f64 - left).abs() < 1e-6);
    assert!((plan.diags[1].coeffs_f32[0] as f64 - right).abs() < 1e-6);
    // RolePlan 写 identity，避免运行时双乘。
    assert_eq!(plan.roles[0].gain_q15, i16::MAX);
    assert_eq!(plan.roles[0].polarity, 1);
    assert_eq!(plan.roles[1].gain_q15, i16::MAX);
    assert_eq!(plan.roles[1].polarity, 1);
    assert_eq!(plan.roles[0].effective_gain_db, 6.0);
    assert_eq!(plan.roles[1].effective_gain_db, -6.0);
    // 报告保留折叠前的事实。
    assert_eq!(lowered.report.role_gain_db[0], 6.0);
    assert_eq!(lowered.report.role_polarity[1], -1);
}

#[test]
fn nodes_gain_limits_and_trim_cap_rejected() {
    let mut graph = tiny_graph();
    graph.roles[0].gain_db = 13.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.roles[0].gain_db = f64::NAN;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.roles[0].trim_db = 7.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 所有增益叠加后超过设计增益帽（P04）。
    let mut graph = tiny_graph();
    graph.roles[0].gain_db = 6.0;
    graph.roles[0].trim_db = 6.0;
    graph.config.max_gain_db = 6.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);
}

#[test]
fn nodes_role_delay_single_field_application() {
    let mut graph = tiny_graph();
    graph.roles[0].delay_samples = 2.5;
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    assert_eq!(plan.roles[0].delay_samples, 2);
    assert_eq!(plan.roles[0].delay_q16, 32768);
    assert_eq!(lowered.report.role_delay_q16[0], 2 * 65536 + 32768);
    assert_eq!(lowered.report.total_delay_q16[0], 2 * 65536 + 32768);
    // FIR 不变（延迟不进 FIR，避免双扣）。
    assert_eq!(plan.diags[0].coeffs_f32, vec![1.0f32]);
    assert_eq!(plan.diags[0].delay_samples, 0);
    // 运行阶段含 RoleDelay，且在 volume 之前。
    let delay_index = plan
        .runtime_stages
        .iter()
        .position(|stage| *stage == RuntimeStage::RoleDelay(0))
        .expect("role delay stage");
    let volume_index = plan
        .runtime_stages
        .iter()
        .position(|stage| *stage == RuntimeStage::VolumeRamp)
        .expect("volume stage");
    assert!(delay_index < volume_index);

    // 200 ms 上限。
    let mut graph = tiny_graph();
    graph.roles[0].delay_samples = 200.0 * 48.0 + 1.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);
    let mut graph = tiny_graph();
    graph.roles[0].delay_samples = -1.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);
}

// ---------------------------------------------------------------------------
// 模态 IIR（RBJ 10 段级；允许 IIR，不硬截 FIR）
// ---------------------------------------------------------------------------

#[test]
fn nodes_modal_iir_requires_f64_v15() {
    // §5.4/V15：30 Hz / -12 dB / Q=20 -> f32 量化偏差 0.19646 dB -> requires_f64=true。
    let band = ModalBandConfig::from_params(ModalBandType::Peaking, 30.0, 20.0, -12.0, FS as f64)
        .expect("v15 band");
    let deviation = band.f32_quantization_max_deviation_db(FS as f64);
    assert!(
        (deviation - 0.196_46).abs() < 0.01,
        "V15 deviation {}",
        deviation
    );
    assert!(deviation > F32_QUANTIZATION_TOL_DB);
    assert!(band.requires_f64(FS as f64));
    let pole = band.max_pole_radius();
    assert!((pole - 0.999_804).abs() < 1e-4, "V15 pole radius {}", pole);
    assert!(pole <= POLE_RADIUS_MAX);

    let mut graph = tiny_graph();
    graph.modal_iir[0] = ModalIirChannel::new(vec![band]);
    let lowered = lower_ok(&graph);
    assert!(lowered.report.modal_requires_f64[0]);
    assert!(!lowered.report.modal_f32_admissible);
    assert!(!lowered.report.runtime_f32_admissible);
    assert_eq!(lowered.plan.iir.len(), 1);
    let plan_band = &lowered.plan.iir[0].bands[0];
    assert!(plan_band.requires_f64);
    assert_eq!(plan_band.b, plan_band.b);
    assert!(lowered
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("requires_f64")));
    assert_eq!(lowered.validate_runtime_f32(), Err(RtStatus::ERR_PRECISION));
}

#[test]
fn nodes_modal_iir_mild_band_f32_admissible() {
    // §5.4：40 Hz / -10 dB / Q=16 -> 0.03974 dB < 0.1 -> f32 可用。
    let band = ModalBandConfig::from_params(ModalBandType::Peaking, 40.0, 16.0, -10.0, FS as f64)
        .expect("mild band");
    let deviation = band.f32_quantization_max_deviation_db(FS as f64);
    assert!(
        (deviation - 0.039_74).abs() < 0.005,
        "mild deviation {}",
        deviation
    );
    assert!(deviation <= F32_QUANTIZATION_TOL_DB);
    let mut graph = tiny_graph();
    graph.modal_iir[0] = ModalIirChannel::new(vec![band]);
    let lowered = lower_ok(&graph);
    assert!(!lowered.report.modal_requires_f64[0]);
    assert!(lowered.report.modal_f32_admissible);
    assert!(lowered.runtime_f32_admissible());
    assert_eq!(lowered.validate_runtime_f32(), Ok(()));
    assert!(!lowered.plan.iir[0].bands[0].requires_f64);
}

#[test]
fn nodes_modal_iir_rejects_unstable_and_out_of_contract() {
    // peaking Q 下限、shelf S 范围、频率/增益范围。
    assert_eq!(
        ModalBandConfig::from_params(ModalBandType::Peaking, 1000.0, 0.05, -3.0, FS as f64),
        Err(RtStatus::ERR_PATH_TABLE)
    );
    assert_eq!(
        ModalBandConfig::from_params(ModalBandType::LowShelf, 1000.0, 1.5, 3.0, FS as f64),
        Err(RtStatus::ERR_PATH_TABLE)
    );
    assert_eq!(
        ModalBandConfig::from_params(ModalBandType::HighShelf, 19.0, 0.5, 3.0, FS as f64),
        Err(RtStatus::ERR_PATH_TABLE)
    );
    assert_eq!(
        ModalBandConfig::from_params(ModalBandType::Peaking, 20_001.0, 1.0, -3.0, FS as f64),
        Err(RtStatus::ERR_PATH_TABLE)
    );
    assert_eq!(
        ModalBandConfig::from_params(ModalBandType::Peaking, 1000.0, 1.0, 25.0, FS as f64),
        Err(RtStatus::ERR_PATH_TABLE)
    );
    // 未知 type：解析器返回 None（调用方整份拒收，不降级）。
    assert_eq!(
        ModalBandType::parse("peaking"),
        Some(ModalBandType::Peaking)
    );
    assert_eq!(
        ModalBandType::parse("low_shelf"),
        Some(ModalBandType::LowShelf)
    );
    assert_eq!(
        ModalBandType::parse("high_shelf"),
        Some(ModalBandType::HighShelf)
    );
    assert_eq!(ModalBandType::parse("notch"), None);
    assert_eq!(ModalBandType::parse(""), None);
    assert!(ModalBandType::LowShelf.is_shelf());
    assert!(!ModalBandType::Peaking.is_shelf());

    // 极点半径越界（|p| ~ 0.999999995 > 0.999999）必须拒收。
    assert_eq!(
        ModalBandConfig::from_coefficients(
            ModalBandType::Peaking,
            1000.0,
            1.0,
            -3.0,
            [1.0, 0.0, 0.0],
            [1.0, -1.999_999, 0.999_999_99],
            FS as f64,
        ),
        Err(RtStatus::ERR_PATH_TABLE)
    );
    // 字段与系数不自洽（管理机构输出被篡改）必须拒收。
    let mut tampered =
        ModalBandConfig::from_params(ModalBandType::Peaking, 1000.0, 1.0, -6.0, FS as f64)
            .expect("band");
    tampered.b[0] *= 1.5;
    assert_eq!(
        ModalBandConfig::from_coefficients(
            tampered.band_type,
            tampered.freq_hz,
            tampered.q,
            tampered.gain_db,
            tampered.b,
            tampered.a,
            FS as f64,
        ),
        Err(RtStatus::ERR_PATH_TABLE)
    );

    // 每通道 biquad 总数 <= 32。
    let one = ModalBandConfig::from_params(ModalBandType::Peaking, 1000.0, 1.0, -1.0, FS as f64)
        .expect("band");
    let mut graph = tiny_graph();
    graph.modal_iir[0] = ModalIirChannel::new(vec![one.clone(); MODAL_BIQUADS_MAX + 1]);
    assert_eq!(lower_err(&graph), RtStatus::ERR_CAPACITY);
    graph.modal_iir[0] = ModalIirChannel::new(vec![one; MODAL_BIQUADS_MAX]);
    lower_ok(&graph);
}

#[test]
fn nodes_modal_iir_rbj_invariants() {
    let fs = FS as f64;
    // peaking 在 f0 处 = gain_db（解析精确）。
    let peaking = ModalBandConfig::from_params(ModalBandType::Peaking, 1000.0, 4.0, -6.0, fs)
        .expect("peaking");
    assert!((peaking.magnitude_db(1000.0, fs) - (-6.0)).abs() < 1e-6);
    // low shelf：DC = gain，Nyquist = 0；f0 处 = gain/2。
    let low = ModalBandConfig::from_params(ModalBandType::LowShelf, 200.0, 0.7, 6.0, fs)
        .expect("low shelf");
    assert!((low.magnitude_db(1.0, fs) - 6.0).abs() < 0.05);
    assert!(low.magnitude_db(fs / 2.0, fs).abs() < 0.05);
    assert!((low.magnitude_db(200.0, fs) - 3.0).abs() < 0.15);
    // high shelf：DC = 0，Nyquist = gain。
    let high = ModalBandConfig::from_params(ModalBandType::HighShelf, 2000.0, 0.7, -6.0, fs)
        .expect("high shelf");
    assert!(high.magnitude_db(1.0, fs).abs() < 0.05);
    assert!((high.magnitude_db(fs / 2.0, fs) + 6.0).abs() < 0.05);
    // -0.1..0.1 dB 内 RBJ 自洽（字段 -> 系数）。
    let (db_dev, deg_dev) = peaking.self_consistency_deviation(fs);
    assert!(db_dev < 0.1 && deg_dev < 1.0);
    assert!(modal_freq_upper_hz(fs) <= 20_000.0);
    assert!((modal_freq_upper_hz(8_000.0) - 3_999.0).abs() < 1e-9);
}

#[test]
fn nodes_modal_iir_not_truncated_into_fir_and_commutes() {
    let fs = FS as f64;
    let band =
        ModalBandConfig::from_params(ModalBandType::Peaking, 1000.0, 1.0, -6.0, fs).expect("band");
    let mut graph = tiny_graph();
    graph.output_fir[0] = target_chain(vec![1.0, 0.5, -0.25]);
    graph.modal_iir[0] = ModalIirChannel::new(vec![band.clone()]);
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    // IIR 不硬截成 FIR：P FIR 支撑仍是 3 taps，IIR 独立保留。
    assert_eq!(plan.l_p, 3);
    assert_eq!(plan.diags[0].taps, 3);
    assert_eq!(plan.diags[0].support_taps, 3);
    assert_eq!(plan.iir[0].bands.len(), 1);
    assert!(plan.output_stages[0]
        .coeffs_f32
        .iter()
        .take(3)
        .eq([1.0f32, 0.5, -0.25].iter()));
    // 交换律：kernel P FIR 之后执行 IIR 与规范序（IIR 在 target FIR 之前）在 LTI 下精确等价。
    let fir: Vec<f64> = compose_fir_segments(&graph.output_fir[0].segments).expect("fir");
    let mut iir_state = [0.0f64; 2];
    let mut iir_ir = vec![0.0f64; 64];
    for (index, slot) in iir_ir.iter_mut().enumerate() {
        let input = if index == 0 { 1.0 } else { 0.0 };
        *slot = khaen_core::nodes::biquad_process_df2t_f64(&band.b, &band.a, &mut iir_state, input);
    }
    let kernel_then_iir = khaen_core::compile::convolve_linear(&fir, &iir_ir);
    let iir_then_kernel = khaen_core::compile::convolve_linear(&iir_ir, &fir);
    assert_eq!(kernel_then_iir.len(), iir_then_kernel.len());
    let mut max_diff: f64 = 0.0;
    for (left, right) in kernel_then_iir.iter().zip(iir_then_kernel.iter()) {
        max_diff = max_diff.max((left - right).abs());
    }
    assert!(max_diff < 1e-12, "commutation diff {}", max_diff);
    // 运行阶段：IIR 在 kernel 之后（runtime_stages 第一项），且在 volume/保护之前。
    assert_eq!(plan.runtime_stages[0], RuntimeStage::ModalIir(0));
    let volume_index = plan
        .runtime_stages
        .iter()
        .position(|stage| *stage == RuntimeStage::VolumeRamp)
        .expect("volume");
    assert!(0 < volume_index);
    assert!(lowered
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("模态 IIR")));
}

// ---------------------------------------------------------------------------
// volume ramp
// ---------------------------------------------------------------------------

#[test]
fn nodes_volume_ramp_contract() {
    // 量程 -150..+50（P07/R19/W17）。
    for bad in [-151.0, 50.5, f64::NAN, f64::INFINITY] {
        let mut graph = tiny_graph();
        graph.volume.volume_db = bad;
        assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    }
    // ramp 语义：线性 dB、单调、端点精确、无过冲。
    assert!((volume_ramp_db_step(0.0, -6.0, 240) + 0.025).abs() < 1e-12);
    assert_eq!(volume_ramp_db_step(0.0, -6.0, 0), 0.0);
    assert_eq!(volume_gain_db_at_frame(0.0, -6.0, 240, 0), 0.0);
    assert!((volume_gain_db_at_frame(0.0, -6.0, 240, 120) + 3.0).abs() < 1e-12);
    assert_eq!(volume_gain_db_at_frame(0.0, -6.0, 240, 240), -6.0);
    assert_eq!(volume_gain_db_at_frame(0.0, -6.0, 240, 10_000), -6.0);
    assert_eq!(volume_gain_db_at_frame(0.0, -6.0, 0, 1), -6.0);
    let linear = volume_linear_gain_at_frame(0.0, -6.0, 240, 240);
    assert!((linear - db_to_linear(-6.0)).abs() < 1e-12);
    let mut previous = f64::INFINITY;
    for frame in 0..=240 {
        let gain = volume_gain_db_at_frame(0.0, -6.0, 240, frame);
        assert!(gain <= previous + 1e-12);
        previous = gain;
    }
    // 计划/运行阶段：volume 在保护之前；mute 与保护静音区分。
    let mut graph = tiny_graph();
    graph.volume = VolumeRampConfig {
        volume_db: -12.5,
        user_mute: true,
        ramp_frames: 48,
    };
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    assert_eq!(plan.volume.volume_db, -12.5);
    assert!(plan.volume.user_mute);
    assert_eq!(plan.volume.target_linear_gain, 0.0);
    assert_eq!(plan.volume.ramp_frames, 48);
    assert_eq!(plan.volume.gain_db_min, -150.0);
    assert_eq!(plan.volume.gain_db_max, 50.0);
    let volume_index = plan
        .runtime_stages
        .iter()
        .position(|stage| *stage == RuntimeStage::VolumeRamp)
        .expect("volume");
    let limiter_index = plan
        .runtime_stages
        .iter()
        .position(|stage| *stage == RuntimeStage::Limiter(0))
        .expect("limiter");
    assert!(volume_index < limiter_index);
    assert!(plan.protection.ramp.enabled);
    assert!(lowered.report.user_mute);
    assert_eq!(lowered.report.volume_ramp_frames, 48);
}

// ---------------------------------------------------------------------------
// limiter / clip / ramp 保护（不可绕过；P14 未定项）
// ---------------------------------------------------------------------------

#[test]
fn nodes_protection_cannot_be_bypassed() {
    // limiter 缺项/多项。
    let mut graph = tiny_graph();
    graph.protection.limiter.clear();
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);
    let mut graph = tiny_graph();
    graph.protection.limiter.push(limiter());
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    // 任一保护 disabled = 尝试 bypass -> ERR_UNSUPPORTED。
    let mut graph = tiny_graph();
    graph.protection.limiter[0].enabled = false;
    assert_eq!(lower_err(&graph), RtStatus::ERR_UNSUPPORTED);
    let mut graph = tiny_graph();
    graph.protection.clip.enabled = false;
    assert_eq!(lower_err(&graph), RtStatus::ERR_UNSUPPORTED);
    let mut graph = tiny_graph();
    graph.protection.ramp.enabled = false;
    assert_eq!(lower_err(&graph), RtStatus::ERR_UNSUPPORTED);

    // 参数非法（结构/安全边界）。
    let mut graph = tiny_graph();
    graph.protection.limiter[0].threshold_db = 1.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    let mut graph = tiny_graph();
    graph.protection.limiter[0].threshold_db = f64::NAN;
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    let mut graph = tiny_graph();
    graph.protection.limiter[0].release_ms = 0.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    let mut graph = tiny_graph();
    graph.protection.limiter[0].attack_ms = Some(0.0);
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    let mut graph = tiny_graph();
    graph.protection.limiter[0].max_gain_db = -1.0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    let mut graph = tiny_graph();
    graph.protection.clip.ceiling_db = 0.1;
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
    let mut graph = tiny_graph();
    graph.protection.ramp.ramp_frames = 0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);
}

#[test]
fn nodes_protection_plan_per_role_and_p14_marks() {
    let graph = multichannel_graph();
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    assert_eq!(plan.protection.limiters.len(), 7);
    assert_eq!(plan.protection.insertion, "post_role_sum_pre_scatter");
    for (index, limiter_plan) in plan.protection.limiters.iter().enumerate() {
        assert_eq!(limiter_plan.role, index as u16);
        assert_eq!(limiter_plan.out_index, lowered.plan.scatter[index]);
        assert!(limiter_plan.enabled);
        assert_eq!(limiter_plan.threshold_db, -1.0);
        assert_eq!(limiter_plan.release_ms, 50.0);
        assert!(!limiter_plan.unresolved_p14);
    }
    assert!(!plan.protection.unresolved_p14);
    assert!(plan.protection.clip.enabled);
    assert_eq!(plan.protection.clip.ceiling_db, -1.0);
    assert!(plan.protection.ramp.enabled);

    // P14 未定项（attack/lookahead 未配置）必须显式标记，不发明默认值。
    let mut graph = tiny_graph();
    graph.protection.limiter[0] = LimiterConfig::new(-1.0, 40.0, 6.0);
    let lowered = lower_ok(&graph);
    assert!(lowered.plan.protection.limiters[0].unresolved_p14);
    assert!(lowered.plan.protection.unresolved_p14);
    assert!(lowered.report.protection_unresolved_p14);
    assert!(lowered
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("P14")));

    // 保护节点在 scatter 之前（角色求和后），且每个角色（含 zero-fill）都有。
    let stereo = stereo_graph();
    let lowered = lower_ok(&stereo);
    let stages = &lowered.plan.runtime_stages;
    let scatter_index = stages
        .iter()
        .position(|stage| *stage == RuntimeStage::Scatter)
        .expect("scatter");
    assert_eq!(scatter_index, stages.len() - 1);
    for index in 0..7u16 {
        let limiter_index = stages
            .iter()
            .position(|stage| *stage == RuntimeStage::Limiter(index))
            .expect("limiter stage");
        let clip_index = stages
            .iter()
            .position(|stage| *stage == RuntimeStage::ClipGuard(index))
            .expect("clip stage");
        let ramp_index = stages
            .iter()
            .position(|stage| *stage == RuntimeStage::ProtectionRamp(index))
            .expect("ramp stage");
        assert!(limiter_index < clip_index && clip_index < ramp_index);
        assert!(ramp_index < scatter_index);
    }
}

// ---------------------------------------------------------------------------
// 与 T12 compile 图 / T07 direct / T08 partitioned / T09 workers 接口一致
// ---------------------------------------------------------------------------

#[test]
fn nodes_compile_projection_matches_lowered_plan() {
    let graph = multichannel_graph();
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    let report = &lowered.report;
    // L_F = L_G + L_P - 1；完整图长 = L_A + L_G + L_P - 2。
    assert_eq!(report.compile.fused_len_gp, 16 + 11 - 1);
    assert_eq!(report.compile.full_path_len, 1 + 16 + 11 - 2);
    // 7 条对角 baseline + 非零 g cell（含全零 rgba 单元不计），与 T12 matrix_routes 同口径。
    let mut expected_routes = 7u32;
    let participants = &graph.low_band.participants;
    for (row, &out_role) in participants.iter().enumerate() {
        for (col, &in_role) in participants.iter().enumerate() {
            if out_role == in_role {
                continue;
            }
            let cell = &graph.low_band.g
                [(row * participants.len() + col) * graph.low_band.l_g as usize..]
                [..graph.low_band.l_g as usize];
            if cell.iter().any(|value| *value != 0.0) {
                expected_routes += 1;
            }
        }
    }
    assert_eq!(report.compile.route_count, expected_routes);
    assert_eq!(plan.matrix_stages.len(), expected_routes as usize);
    let expected_routes = expected_routes as usize;
    assert_eq!(plan.input_stages.len(), 7);
    for (index, stage) in plan.input_stages.iter().enumerate() {
        assert_eq!(stage.index, index as u16);
        assert_eq!(stage.taps, 1);
        assert_eq!(stage.coeffs_f32, vec![1.0f32]);
    }
    assert_eq!(plan.output_stages.len(), 7);
    for index in 0..7 {
        assert_eq!(plan.output_stages[index].taps, 11);
        assert_eq!(
            plan.output_stages[index].coeffs_f32,
            expected_p_row(&graph, index, 11)
        );
        assert_eq!(
            plan.diags[index].coeffs_f32,
            expected_p_row(&graph, index, 11)
        );
    }
    // 融合可行性与选择一致：选 fused 时必须有路径投影。
    match plan.selected {
        FusionForm::FusedGp => {
            assert!(lowered.plans.fused.is_some());
            assert_eq!(plan.fused_stages.len(), expected_routes);
            for path in &plan.fused_stages {
                assert_eq!(path.taps, 26);
                assert_eq!(path.coeffs_f32.len(), 26);
                assert_eq!(path.polarity, 1);
            }
        }
        FusionForm::Factorized => {
            assert!(plan.fused_stages.len() == expected_routes || plan.fused_stages.is_empty());
        }
    }
    assert_eq!(lowered.selected_form(), plan.selected);
    // 非 FL 角色 P 行只有 8 个实际 support，其余零填。
    for index in 1..7 {
        for value in plan.diags[index].coeffs_f32.iter().skip(8) {
            assert_eq!(*value, 0.0f32);
        }
    }
}

struct F64Coeffs {
    a: Vec<f64>,
    g: Vec<f64>,
    p: Vec<f64>,
}

impl F64Coeffs {
    fn view(&self) -> DirectCoeffsF64<'_> {
        DirectCoeffsF64 {
            a_fir: &self.a,
            g: &self.g,
            p: &self.p,
        }
    }
}

fn f64_coeffs(graph: &NodeGraph, l_p: usize) -> F64Coeffs {
    let mut p = vec![0.0f64; graph.roles.len() * l_p];
    for index in 0..graph.roles.len() {
        let composed =
            compose_fir_segments(&graph.output_fir[index].segments).expect("compose segments");
        let gain = db_to_linear(graph.roles[index].effective_gain_db());
        let polarity = graph.roles[index].polarity as f64;
        for (tap, &value) in composed.iter().enumerate() {
            p[index * l_p + tap] = value * gain * polarity;
        }
    }
    F64Coeffs {
        a: graph.a.clone(),
        g: graph.low_band.g.clone(),
        p,
    }
}

fn direct_f32_output(
    topology: &DirectTopology,
    coefficients: &DirectCoeffsF32<'_>,
    input: &[f32],
    containers: usize,
) -> Vec<f32> {
    let layout = DirectLayout::build(topology).expect("layout");
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut output = vec![0.0f32; containers * BLOCK];
    let status = direct_block_f32(
        topology,
        &layout,
        coefficients,
        &mut state,
        input,
        BLOCK,
        &mut output,
        BLOCK,
        BLOCK,
        BLOCK,
    );
    assert_eq!(status, RtStatus::OK);
    output
}

#[test]
fn nodes_kernel_direct_partitioned_workers_agree() {
    let graph = multichannel_graph();
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    let topology = plan.topology.expect("kernel topology");
    let coefficients = plan.direct_coeffs().expect("kernel coeffs");
    let input = impulse_input(7);
    let direct = direct_f32_output(&topology, &coefficients, &input, 8);

    // T07 f64 oracle：与 f32 direct 在工程容差内一致。
    let f64s = f64_coeffs(&graph, plan.l_p as usize);
    let layout64 = DirectLayout::build(&topology).expect("layout");
    let mut state64 = vec![0.0f64; layout64.state_samples()];
    let mut output64 = vec![0.0f64; 8 * BLOCK];
    let input64: Vec<f64> = input.iter().map(|value| *value as f64).collect();
    let status = direct_block_f64(
        &topology,
        &layout64,
        &f64s.view(),
        &mut state64,
        &input64,
        BLOCK,
        &mut output64,
        BLOCK,
        BLOCK,
        BLOCK,
    );
    assert_eq!(status, RtStatus::OK);
    for (left, right) in direct.iter().zip(output64.iter()) {
        assert!((*left as f64 - *right).abs() < 1e-4);
    }

    // T08 partitioned：同一图/输入，输出与 direct 一致。
    let layout = PartitionedLayout::build(&topology, BLOCK).expect("partitioned layout");
    let mut plan_data = vec![0.0f32; layout.plan_samples()];
    let mut scratch = vec![0.0f32; layout.build_scratch_samples()];
    let partitioned_plan = PartitionedPlan::build_f32(
        &topology,
        BLOCK,
        &coefficients,
        &mut plan_data,
        &mut scratch,
    )
    .expect("partitioned plan");
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut output = vec![0.0f32; 8 * BLOCK];
    let status = partitioned_block_f32(
        &topology,
        &partitioned_plan,
        &plan_data,
        &mut state,
        &input,
        BLOCK,
        &mut output,
        BLOCK,
        BLOCK,
        BLOCK,
    );
    assert_eq!(status, RtStatus::OK);
    for (left, right) in direct.iter().zip(output.iter()) {
        assert!((*left - *right).abs() < 1e-4);
    }

    // T09 workers：静态 4 worker DAG 接受本拓扑，执行结果一致（固定 reduce）。
    let dag = WorkerDagPlan::build(&topology, BLOCK as u16, 4, 1).expect("worker dag");
    assert_eq!(dag.validate(), RtStatus::OK);
    let worker_layout = DirectLayout::build(&topology).expect("layout");
    let mut worker_state = vec![0.0f32; worker_layout.state_samples()];
    let mut worker_output = vec![0.0f32; 8 * BLOCK];
    let mut partials = WorkerPartials::new();
    let mut stats = WorkerScheduleStats::default();
    let bad_output = AtomicU32::new(0);
    let mut ack = KhaenBlockAckV1::new(0, 0, dag.plan_version, 4);
    ack.flags = WORKER_ACK_FLAG_VALID;
    let status = execute_block_scheduled(
        &dag,
        &coefficients,
        &mut worker_state,
        &input,
        BLOCK,
        &mut worker_output,
        BLOCK,
        0,
        0,
        BLOCK,
        &ack,
        &mut partials,
        &bad_output,
        1_000_000,
        &mut stats,
    );
    assert_eq!(status, RtStatus::OK);
    assert_eq!(stats.executed_tiles, dag.total_tasks() as u32);
    for (left, right) in direct.iter().zip(worker_output.iter()) {
        assert!((*left - *right).abs() < 1e-4);
    }
}

#[test]
fn nodes_kernel_zero_fill_containers_written_exactly() {
    let graph = tiny_graph();
    let lowered = lower_ok(&graph);
    let plan = &lowered.plan;
    let topology = plan.topology.expect("topology");
    assert_eq!(topology.container_channels, 4);
    let coefficients = plan.direct_coeffs().expect("coeffs");
    let mut input = vec![0.0f32; BLOCK];
    input[0] = 1.0;
    let output = direct_f32_output(&topology, &coefficients, &input, 4);
    for container in [0usize, 1, 3] {
        for sample in 0..BLOCK {
            assert_eq!(output[container * BLOCK + sample], 0.0f32);
        }
    }
    assert_eq!(output[2 * BLOCK], 1.0f32);
    assert_eq!(plan.zero_fill_containers, vec![0u16, 1, 3]);
    assert_eq!(
        plan.container_zero_fill(4),
        scatter_zero_fill_containers(&plan.scatter, 4)
    );
}

#[test]
fn nodes_extended_taps_marked_not_kernel_admissible() {
    let mut graph = tiny_graph();
    let mut coefficients = vec![0.0f64; 8192];
    coefficients[0] = 1.0;
    graph.output_fir[0] = target_chain(coefficients);
    graph.config.options = extended_options();
    let lowered = lower_ok(&graph);
    assert_eq!(lowered.report.l_p, 8192);
    assert!(!lowered.report.kernel_admissible);
    assert_eq!(lowered.report.kernel_error, Some(RtStatus::ERR_TAPS));
    assert!(lowered.plan.topology.is_none());
    assert!(lowered.plan.coeffs.is_none());
    assert!(!lowered.report.runtime_f32_admissible);
    assert_eq!(lowered.validate_runtime_f32(), Err(RtStatus::ERR_TAPS));
    assert!(lowered
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("kernel")));
}

// ---------------------------------------------------------------------------
// 校验负向 / 辅助函数
// ---------------------------------------------------------------------------

#[test]
fn nodes_validation_negative_matrix() {
    let mut graph = tiny_graph();
    graph.n_src = 21;
    graph.a = vec![1.0; 21];
    assert_eq!(lower_err(&graph), RtStatus::ERR_CHANNELS);

    let mut graph = tiny_graph();
    graph.roles = vec![role(COMPILE_NO_SOURCE, 0); 17];
    graph.container_channels = 17;
    graph.output_fir = identity_chains(17);
    graph.modal_iir = empty_iir(17);
    graph.protection = protection(17);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.container_channels = 23;
    assert_eq!(lower_err(&graph), RtStatus::ERR_CHANNELS);

    let mut graph = tiny_graph();
    graph.l_a = 0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_TAPS);

    let mut graph = tiny_graph();
    graph.a = vec![1.0, 2.0];
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);

    let mut graph = tiny_graph();
    graph.a = vec![f64::INFINITY];
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);

    let mut graph = tiny_graph();
    graph.low_band.g = vec![0.1, 0.2];
    assert_eq!(lower_err(&graph), RtStatus::ERR_PATH_TABLE);

    let mut graph = tiny_graph();
    graph.low_band = empty_low_band(vec![0, 0], 1, 0);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.low_band = empty_low_band(vec![0], 2, 2);
    assert_eq!(lower_err(&graph), RtStatus::ERR_TAPS);

    let mut graph = tiny_graph();
    graph.output_fir = identity_chains(2);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.modal_iir = empty_iir(2);
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.protection.limiter.clear();
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.roles[0].polarity = 0;
    assert_eq!(lower_err(&graph), RtStatus::ERR_ROLES);

    let mut graph = tiny_graph();
    graph.config.options.sample_rate_hz = 44_100;
    assert_eq!(lower_err(&graph), RtStatus::ERR_RATE);

    let mut graph = tiny_graph();
    graph.config.options.block_size = 256;
    assert_eq!(lower_err(&graph), RtStatus::ERR_BLOCK_SIZE);

    // 正常图通过。
    lower_ok(&tiny_graph());
}

#[test]
fn nodes_helper_contracts() {
    // CRC-32C（Castagnoli）标准向量。
    assert_eq!(crc32c(b""), 0);
    assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    let role_name = Some("HFL".to_string());
    let hash = crc32c(role_name.expect("name").as_bytes());
    assert_ne!(hash, 0);

    // scatter 零填辅助。
    assert_eq!(
        scatter_zero_fill_containers(&[0, 4, 7], 8),
        vec![1u16, 2, 3, 5, 6]
    );
    assert_eq!(scatter_zero_fill_containers(&[], 3), vec![0u16, 1, 2]);
    assert_eq!(
        scatter_zero_fill_containers(&[0, 1, 2], 3),
        Vec::<u16>::new()
    );

    // FIR 段合成辅助：空段 = identity delta；段序任意时由调用方负责校验。
    assert_eq!(compose_fir_segments(&[]).expect("identity"), vec![1.0]);
    let segments = vec![
        FirSegment::new(FirSegmentKind::Crossover, vec![1.0, 1.0], 0, 0),
        FirSegment::new(FirSegmentKind::Target, vec![1.0, -1.0], 0, 0),
    ];
    let composed = compose_fir_segments(&segments).expect("compose");
    assert_eq!(composed, vec![1.0, 0.0, -1.0]);

    // 最小 identity 图：P=delta，无语义默认滤波。
    let lowered = lower_ok(&tiny_graph());
    assert_eq!(lowered.report.l_p, 1);
    assert_eq!(lowered.plan.l_p, 1);
    assert_eq!(lowered.plan.diags[0].coeffs_f32, vec![1.0f32]);
    assert_eq!(lowered.report.compile.route_count, 1);
    assert!(lowered.report.kernel_admissible);
    assert_eq!(lowered.plan.topology.expect("topology").n_role, 1);
    assert_eq!(
        lowered.plan.runtime_stages.last(),
        Some(&RuntimeStage::Scatter)
    );
}

#[test]
fn nodes_plan_node_shape_and_runtime_stage_roles() {
    let lowered = lower_ok(&multichannel_graph());
    let plan = &lowered.plan;
    for node in &plan.nodes {
        match node.kind {
            NodeKind::InputSelect
            | NodeKind::LowBandMatrix
            | NodeKind::VolumeRamp
            | NodeKind::Scatter => assert_eq!(node.role, None),
            _ => assert!(node.role.is_some()),
        }
    }
    assert_eq!(RuntimeStage::Scatter.role(), None);
    assert_eq!(RuntimeStage::VolumeRamp.role(), None);
    assert_eq!(RuntimeStage::ModalIir(3).role(), Some(3));
    assert_eq!(RuntimeStage::RoleDelay(2).role(), Some(2));
    assert_eq!(RuntimeStage::Limiter(1).role(), Some(1));
    assert_eq!(RuntimeStage::ClipGuard(4).role(), Some(4));
    assert_eq!(RuntimeStage::ProtectionRamp(5).role(), Some(5));
    assert_eq!(RuntimeStage::Limiter(1).schema_name(), "limiter");
    // diags 按 out_index 升序；roles 按 role_id 升序。
    let mut previous_out = 0u16;
    for diag in &plan.diags {
        assert!(diag.out_index >= previous_out);
        previous_out = diag.out_index;
    }
    for (index, role) in plan.roles.iter().enumerate() {
        assert_eq!(role.role_id, index as u16);
    }
}
