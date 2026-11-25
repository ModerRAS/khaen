//! 控制面方案加载测试：prepare（sha256/解码/准入/编译）→ commit（块边界换图）→
//! abort / coeff / status 的真实行为。
//!
//! 换图语义在测试里明确断言：commit 只把新图排到下一块；**block 边界 = 下一次 `run_block`
//! 的入口**，那一块从头就用新图（`applied_block` 即该块号）；commit 后、跑块前 status 仍报旧图。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_host::control::{error, ControlState, WIRED_OPS};
use khaen_host::{sha256_hex, EngineConfig, GraphInfo};
use serde_json::{json, Value};

const SAMPLE_RATE: u32 = 96_000;
const BLOCK_FRAMES: u32 = 128;
const CHANNELS: u16 = 2;
/// +6 dB 的线性增益（10^(6/20)）；断言容差 1e-6。
const GAIN_6DB: f32 = 1.995_262;

fn engine_config() -> EngineConfig {
    EngineConfig {
        block_frames: BLOCK_FRAMES as usize,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS as usize,
    }
}

fn state() -> ControlState {
    ControlState::new(engine_config()).expect("state")
}

/// 全恒等方案（identity 判据见 graph_executor.rs）：逐样本等于直通。
fn identity_plan() -> Plan {
    let lanes = CHANNELS;
    let count = CHANNELS as usize;
    let mut matrix_coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        matrix_coeffs[lane * count + lane] = 1.0;
    }
    Plan {
        block_size: BLOCK_FRAMES,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: CHANNELS,
            out_lanes: lanes,
            folds: (0..lanes)
                .map(|lane| Fold {
                    src: lane,
                    dst: lane,
                    gain: 1.0,
                })
                .collect(),
        }),
        crossover: Some(CrossoverSection {
            lanes,
            filters: (0..lanes)
                .map(|lane| CrossoverFilter {
                    lane,
                    coeffs: vec![1.0],
                })
                .collect(),
        }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection {
            lanes,
            entries: (0..lanes)
                .map(|lane| ProtectionEntry {
                    lane,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                })
                .collect(),
        }),
        routing: Some(RoutingSection {
            dac_channels: CHANNELS,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

/// identity 方案 + protection 每 lane 加 `gain_db` dB（其余段保持恒等）。
fn gain_plan(gain_db: f32) -> Plan {
    let mut plan = identity_plan();
    if let Some(protection) = &mut plan.protection {
        for entry in &mut protection.entries {
            entry.gain_db = gain_db;
        }
    }
    plan
}

/// 准备临时方案文件；Drop 时删除。
struct TempPlan {
    path: PathBuf,
    sha256: String,
}

impl TempPlan {
    fn new(tag: &str, bytes: &[u8]) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-t25b-{}-{}-{}.plan",
            std::process::id(),
            tag,
            unique
        ));
        std::fs::write(&path, bytes).expect("write temp plan");
        Self {
            path,
            sha256: sha256_hex(bytes),
        }
    }

    fn path(&self) -> &str {
        self.path.to_str().expect("temp path is utf-8")
    }
}

impl Drop for TempPlan {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ===========================================================================
// rollback：两阶段回到历史版本（可回滚是验收项）
// ===========================================================================

fn commit_plan(state: &mut ControlState, plan: &Plan) -> String {
    let file = TempPlan::new("commit", &plan.encode());
    let prepared = prepare(state, &file);
    token_of(&prepared)
}

#[test]
fn rollback_restores_previous_plan_at_next_block_boundary() {
    let mut state = state();
    let identity = identity_plan();
    let identity_id = identity.graph_id_hex();
    let token = commit_plan(&mut state, &identity);
    commit(&mut state, &token);

    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x1111);
    let first = run_block(&mut state, &input);
    assert_bit_eq(&first, &input);

    // 第二个方案：+6 dB，生效后再回滚。
    let gain = gain_plan(6.0);
    let gain_file = TempPlan::new("gain", &gain.encode());
    let gain_prepared = prepare(&mut state, &gain_file);
    let gain_token = token_of(&gain_prepared);
    commit(&mut state, &gain_token);
    let gained = run_block(&mut state, &input);
    for (actual, sample) in gained.iter().zip(input.iter()) {
        let expected = sample * GAIN_6DB;
        assert!((actual - expected).abs() <= 1e-6, "{actual} vs {expected}");
    }

    // rollback 默认回到上一个已生效版本（这里 = v1 identity），版本号递增到 3。
    let rolled = request(&mut state, json!({ "op": "rollback" }));
    assert_eq!(rolled["ok"], Value::Bool(true));
    assert_eq!(rolled["rolled_back_to"], Value::from(1));
    assert_eq!(rolled["planned_version"], Value::from(3));
    assert_eq!(rolled["graph_id"], Value::String(identity_id.clone()));
    let rollback_token = token_of(&rolled);

    // commit 前仍跑 v2（不提前生效）。
    let before_commit = run_block(&mut state, &input);
    for (actual, sample) in before_commit.iter().zip(input.iter()) {
        let expected = sample * GAIN_6DB;
        assert!((actual - expected).abs() <= 1e-6);
    }
    assert_eq!(status(&mut state)["actual_version"], Value::from(2));

    // commit 后排到下一块边界：该块从头就是 identity。
    let committed = commit(&mut state, &rollback_token);
    assert_eq!(committed["version"], Value::from(3));
    let restored = run_block(&mut state, &input);
    assert_bit_eq(&restored, &input);
    let after = status(&mut state);
    assert_eq!(after["actual_version"], Value::from(3));
    assert_eq!(after["graph_id"], Value::String(identity_id));
    // 回滚后历史仍保留 v1/v2/v3（历史不被改写）。
    let coeff = coeff(&mut state);
    assert_eq!(coeff["available_versions"], json!([1, 2, 3]));
}

#[test]
fn rollback_without_history_reports_no_history() {
    let mut state = state();
    let response = request(&mut state, json!({ "op": "rollback" }));
    assert_eq!(response["ok"], Value::Bool(false));
    assert_eq!(response["error"], Value::String(error::NO_HISTORY.into()));
    assert!(response["message"]
        .as_str()
        .is_some_and(|message| message.contains("no committed plan history")));
}

#[test]
fn rollback_rejects_unknown_current_and_single_version_targets() {
    let mut state = state();
    let identity = identity_plan();
    let token = commit_plan(&mut state, &identity);
    commit(&mut state, &token);
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x2222);
    run_block(&mut state, &input);

    // 只有一个历史版本：默认回滚没有目标。
    let single = request(&mut state, json!({ "op": "rollback" }));
    assert_eq!(single["error"], Value::String(error::BAD_REQUEST.into()));
    assert!(single["message"]
        .as_str()
        .is_some_and(|message| message.contains("only one committed plan version")));

    // 回滚到当前版本：错误（不是"成功但什么也没做"）。
    let current = request(&mut state, json!({ "op": "rollback", "target": 1 }));
    assert_eq!(current["error"], Value::String(error::BAD_REQUEST.into()));
    assert!(current["message"]
        .as_str()
        .is_some_and(|message| message.contains("already the running graph")));

    // 历史里没有的版本：点名可用版本。
    let unknown = request(&mut state, json!({ "op": "rollback", "target": 99 }));
    assert_eq!(unknown["error"], Value::String(error::BAD_REQUEST.into()));
    assert!(unknown["message"]
        .as_str()
        .is_some_and(|message| message.contains("available: [1]")));

    // 失败的 rollback 不产生草稿：commit 必须报 token 无效。
    let commit_after = request(
        &mut state,
        json!({ "op": "commit", "engine_token": "deadbeef" }),
    );
    assert_eq!(
        commit_after["error"],
        Value::String(error::ENGINE_TOKEN_INVALID.into())
    );

    // 非法 target 类型。
    let bad_type = request(&mut state, json!({ "op": "rollback", "target": "one" }));
    assert_eq!(bad_type["error"], Value::String(error::BAD_REQUEST.into()));
}

fn request(state: &mut ControlState, request: Value) -> Value {
    serde_json::from_str(&state.handle_line(&request.to_string())).expect("response json")
}

fn prepare(state: &mut ControlState, file: &TempPlan) -> Value {
    request(
        state,
        json!({
            "op": "prepare",
            "plan_path": file.path(),
            "plan_sha256": file.sha256,
        }),
    )
}

fn commit(state: &mut ControlState, token: &str) -> Value {
    request(state, json!({ "op": "commit", "engine_token": token }))
}

fn abort(state: &mut ControlState, token: &str) -> Value {
    request(state, json!({ "op": "abort", "engine_token": token }))
}

fn status(state: &mut ControlState) -> Value {
    request(state, json!({ "op": "status" }))
}

fn coeff(state: &mut ControlState) -> Value {
    request(state, json!({ "op": "coeff" }))
}

/// 确定性伪随机测试信号：值域 (-1, 1)，只用整数 LCG 与 f32 除法，跨平台一致。
fn signal(samples: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = (state >> 8) as f32 / (1u32 << 24) as f32;
        out.push(unit * 2.0 - 1.0);
    }
    out
}

fn run_block(state: &mut ControlState, input: &[f32]) -> Vec<f32> {
    state.engine_mut().run_block(input).expect("block").to_vec()
}

fn assert_bit_eq(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "length mismatch");
    for (index, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a.to_bits(), e.to_bits(), "sample {index}: {a} vs {e}");
    }
}

fn token_of(response: &Value) -> String {
    response["engine_token"]
        .as_str()
        .expect("engine_token string")
        .to_string()
}

#[test]
fn status_before_any_plan_reports_null_with_reasons() {
    let mut state = state();
    let response = status(&mut state);
    assert_eq!(response["ok"], Value::Bool(true));
    assert_eq!(response["graph_id"], Value::Null);
    assert_eq!(response["epoch"], Value::Null);
    assert_eq!(response["actual_version"], Value::Null);
    for field in ["graph_id", "epoch", "actual_version"] {
        assert!(
            response["reasons"][field]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "字段 {field} 为 null 时必须给原因"
        );
    }
    let wired: Vec<&str> = response["wired"]
        .as_array()
        .expect("wired")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(wired, WIRED_OPS.to_vec());
    assert!(response["not_wired"]
        .as_array()
        .is_some_and(|ops| ops.is_empty()));
}

#[test]
fn prepare_reports_deterministic_token_and_admission_budget() {
    let mut state = state();
    let plan = identity_plan();
    let file = TempPlan::new("identity", &plan.encode());
    let response = prepare(&mut state, &file);
    assert_eq!(response["ok"], Value::Bool(true));
    // token = 版本号（16 hex）+ 文件 sha256 前 16 hex，确定性。
    let token = token_of(&response);
    assert_eq!(token.len(), 32);
    assert_eq!(
        token,
        format!("{}{}", "0000000000000001", &file.sha256[..16])
    );
    assert_eq!(response["planned_version"], Value::from(1));
    assert_eq!(response["graph_id"], Value::String(plan.graph_id_hex()));
    assert_eq!(response["admission"]["accepted"], Value::Bool(true));
    assert_eq!(
        response["admission"]["budget"]["lanes"],
        Value::from(CHANNELS)
    );
    assert_eq!(
        response["admission"]["budget"]["matrix_cells"],
        Value::from(4)
    );
    assert_eq!(
        response["admission"]["budget"]["dac_channels"],
        Value::from(CHANNELS)
    );

    // 幂等：同一文件（同 token）重复 prepare 返回同一响应，草稿不被替换，也没有改变音频状态。
    assert_eq!(prepare(&mut state, &file), response);
    assert!(!state.engine().switch_pending());
    assert_eq!(status(&mut state)["graph_id"], Value::Null);
}

#[test]
fn prepare_rejects_sha_mismatch_naming_the_field() {
    let mut state = state();
    let file = TempPlan::new("sha", &identity_plan().encode());
    let wrong = request(
        &mut state,
        json!({
            "op": "prepare",
            "plan_path": file.path(),
            "plan_sha256": "0".repeat(64),
        }),
    );
    assert_eq!(wrong["error"], Value::String(error::BAD_REQUEST.into()));
    let message = wrong["message"].as_str().expect("message");
    assert!(message.contains("sha256 mismatch"), "message: {message}");
    assert!(message.contains(&file.sha256), "必须点名实际 sha256");
    assert_eq!(state.engine().graph_info(), GraphInfo::default());
}

#[test]
fn prepare_rejects_undecodable_plan_files() {
    let mut state = state();
    // 长度足够长（>= header+trailer）但不是方案文件：解码报 bad magic。
    let file = TempPlan::new("garbage", &[0u8; 128]);
    let response = prepare(&mut state, &file);
    assert_eq!(response["error"], Value::String(error::BAD_REQUEST.into()));
    let message = response["message"].as_str().expect("message");
    assert!(message.contains("cannot decode plan"), "message: {message}");
    assert!(message.contains("bad magic"), "必须带上 PlanError 文案");
}

#[test]
fn prepare_rejects_admission_failures_with_rejection_code() {
    let mut state = state();
    let mut plan = identity_plan();
    plan.matrix = None;
    let file = TempPlan::new("no-matrix", &plan.encode());
    let response = prepare(&mut state, &file);
    assert_eq!(
        response["error"],
        Value::String(error::ADMISSION_REJECTED.into())
    );
    let message = response["message"].as_str().expect("message");
    assert!(message.contains("missing_matrix"), "message: {message}");
}

#[test]
fn prepare_rejects_plan_that_does_not_match_runtime_config() {
    let mut state = state();
    let mut plan = identity_plan();
    plan.block_size = 64; // 解码/准入都过，但运行时的 block_frames = 128
    let file = TempPlan::new("block64", &plan.encode());
    let response = prepare(&mut state, &file);
    assert_eq!(response["error"], Value::String(error::BAD_REQUEST.into()));
    let message = response["message"].as_str().expect("message");
    assert!(message.contains("block_size"), "message: {message}");
    assert!(message.contains("64"), "message: {message}");
}

#[test]
fn prepare_maps_structural_compile_errors_to_admission_rejected() {
    let mut state = state();
    // 非有限系数永远是结构非法（不随 T22 的抽取/插值支持而改变）：准入不查有限性，
    // 编译期 protection 段拒 → 映射为 admission_rejected。
    let mut plan = identity_plan();
    if let Some(protection) = &mut plan.protection {
        protection.entries[0].gain_db = f32::NAN;
    }
    let file = TempPlan::new("nan-gain", &plan.encode());
    let response = prepare(&mut state, &file);
    assert_eq!(
        response["error"],
        Value::String(error::ADMISSION_REJECTED.into())
    );
    let message = response["message"].as_str().expect("message");
    assert!(message.contains("non-finite"), "message: {message}");
    assert!(message.contains("protection gain_db"), "message: {message}");
}

#[test]
fn commit_and_abort_without_prepare_are_engine_token_invalid() {
    let mut state = state();
    for response in [
        commit(&mut state, "deadbeef"),
        abort(&mut state, "deadbeef"),
    ] {
        assert_eq!(
            response["error"],
            Value::String(error::ENGINE_TOKEN_INVALID.into())
        );
        assert!(response["message"]
            .as_str()
            .is_some_and(|message| message.contains("no prepared plan draft")));
    }
}

#[test]
fn wrong_token_is_rejected_and_draft_survives() {
    let mut state = state();
    let plan = identity_plan();
    let file = TempPlan::new("identity", &plan.encode());
    let prepared = prepare(&mut state, &file);
    let token = token_of(&prepared);

    let wrong = commit(&mut state, "ffffffffffffffffffffffffffffffff");
    assert_eq!(
        wrong["error"],
        Value::String(error::ENGINE_TOKEN_INVALID.into())
    );
    assert!(wrong["message"]
        .as_str()
        .is_some_and(|message| message.contains("mismatch")));

    // 错 token 不销毁草稿：正确 token 仍能提交。
    let committed = commit(&mut state, &token);
    assert_eq!(committed["ok"], Value::Bool(true));
    assert_eq!(committed["version"], Value::from(1));
    assert_eq!(committed["graph_id"], Value::String(plan.graph_id_hex()));
    assert_eq!(committed["applied_block"], Value::from(1));
    assert_eq!(committed["all_ack"], Value::Bool(true));
}

#[test]
fn commit_switches_graph_at_next_block_boundary_identity_then_gain() {
    let mut state = state();
    let identity = identity_plan();
    let identity_id = identity.graph_id_hex();
    let identity_file = TempPlan::new("identity", &identity.encode());
    let prepared = prepare(&mut state, &identity_file);
    assert_eq!(prepared["planned_version"], Value::from(1));
    let token_identity = token_of(&prepared);
    let committed = commit(&mut state, &token_identity);
    assert_eq!(committed["graph_id"], Value::String(identity_id.clone()));
    assert_eq!(committed["applied_block"], Value::from(1));
    assert_eq!(committed["all_ack"], Value::Bool(true));

    // commit 后、跑块前：换图还没发生，status 仍报告旧状态（没有图），不谎报。
    let before = status(&mut state);
    assert_eq!(before["graph_id"], Value::Null);
    assert_eq!(before["epoch"], Value::Null);
    assert!(state.engine().switch_pending());

    // 下一块入口换图：本块从头就是 identity，逐样本等于输入。
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x51ce);
    let output = run_block(&mut state, &input);
    assert_bit_eq(&output, &input);

    let after = status(&mut state);
    assert_eq!(after["graph_id"], Value::String(identity_id.clone()));
    assert_eq!(after["epoch"], Value::from(1));
    assert_eq!(after["actual_version"], Value::from(1));
    assert_eq!(after["blocks"], Value::from(1));
    // 有真实来源后，这些字段不再需要 reasons 解释。
    assert!(after["reasons"].get("graph_id").is_none());
    assert!(after["reasons"].get("epoch").is_none());
    assert!(after["reasons"].get("actual_version").is_none());

    let current = coeff(&mut state);
    assert_eq!(current["version"], Value::from(1));
    assert_eq!(current["last_good_version"], Value::from(1));
    assert_eq!(current["graph_id"], Value::String(identity_id.clone()));
    assert_eq!(current["applied_block"], Value::from(1));
    assert!(current["reasons"]["last_good_version"].is_string());

    // 第二份方案：+6 dB；planned_version 递增到 2。
    let gain = gain_plan(6.0);
    let gain_id = gain.graph_id_hex();
    let gain_file = TempPlan::new("gain6", &gain.encode());
    let prepared_gain = prepare(&mut state, &gain_file);
    assert_eq!(prepared_gain["planned_version"], Value::from(2));
    let token_gain = token_of(&prepared_gain);
    let committed_gain = commit(&mut state, &token_gain);
    assert_eq!(committed_gain["version"], Value::from(2));
    assert_eq!(committed_gain["applied_block"], Value::from(2));
    assert_eq!(committed_gain["graph_id"], Value::String(gain_id.clone()));

    // 跑块之前 status 仍报第一张图（epoch 1），下一块才是 epoch 2。
    let between = status(&mut state);
    assert_eq!(between["graph_id"], Value::String(identity_id));
    assert_eq!(between["epoch"], Value::from(1));
    assert_eq!(between["actual_version"], Value::from(1));

    let input2 = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x6a09);
    let output2 = run_block(&mut state, &input2);
    for (index, (actual, sample)) in output2.iter().zip(input2.iter()).enumerate() {
        let expected = sample * GAIN_6DB;
        assert!(
            (actual - expected).abs() <= 1e-6,
            "sample {index}: {actual} vs {expected} (tolerance 1e-6)"
        );
    }

    let after2 = status(&mut state);
    assert_eq!(after2["graph_id"], Value::String(gain_id.clone()));
    assert_eq!(after2["epoch"], Value::from(2));
    assert_eq!(after2["actual_version"], Value::from(2));
    assert_eq!(after2["blocks"], Value::from(2));

    let current2 = coeff(&mut state);
    assert_eq!(current2["version"], Value::from(2));
    assert_eq!(current2["last_good_version"], Value::from(2));
    assert_eq!(current2["graph_id"], Value::String(gain_id));
    assert_eq!(current2["applied_block"], Value::from(2));
}

#[test]
fn abort_discards_draft_and_keeps_running_graph() {
    let mut state = state();
    let identity = identity_plan();
    let identity_id = identity.graph_id_hex();
    let identity_file = TempPlan::new("identity", &identity.encode());
    let token_identity = token_of(&prepare(&mut state, &identity_file));
    commit(&mut state, &token_identity);
    let _ = run_block(
        &mut state,
        &signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0),
    );

    let gain_file = TempPlan::new("gain6", &gain_plan(6.0).encode());
    let token_gain = token_of(&prepare(&mut state, &gain_file));
    let aborted = abort(&mut state, &token_gain);
    assert_eq!(aborted["ok"], Value::Bool(true));
    assert_eq!(aborted["aborted"], Value::Bool(true));

    // 草稿已被丢弃：再用它的 token 提交 → engine_token_invalid。
    let after_abort = commit(&mut state, &token_gain);
    assert_eq!(
        after_abort["error"],
        Value::String(error::ENGINE_TOKEN_INVALID.into())
    );
    // 正在跑的图不受 abort 影响：下一块仍是 identity。
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 3);
    assert_bit_eq(&run_block(&mut state, &input), &input);
    let current = status(&mut state);
    assert_eq!(current["graph_id"], Value::String(identity_id));
    assert_eq!(current["epoch"], Value::from(1));
}

#[test]
fn prepare_replaces_previous_draft_at_same_planned_version() {
    let mut state = state();
    let identity = identity_plan();
    let identity_file = TempPlan::new("identity", &identity.encode());
    let token_identity = token_of(&prepare(&mut state, &identity_file));

    // 还没 commit，版本号仍是 1；不同内容的 prepare 覆盖草稿（替换语义）。
    let gain = gain_plan(6.0);
    let gain_id = gain.graph_id_hex();
    let gain_file = TempPlan::new("gain6", &gain.encode());
    let prepared_gain = prepare(&mut state, &gain_file);
    assert_eq!(prepared_gain["planned_version"], Value::from(1));
    let token_gain = token_of(&prepared_gain);
    assert_ne!(token_identity, token_gain);

    let stale = commit(&mut state, &token_identity);
    assert_eq!(
        stale["error"],
        Value::String(error::ENGINE_TOKEN_INVALID.into())
    );
    let committed = commit(&mut state, &token_gain);
    assert_eq!(committed["graph_id"], Value::String(gain_id));
    assert_eq!(committed["version"], Value::from(1));

    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 5);
    let output = run_block(&mut state, &input);
    for (actual, sample) in output.iter().zip(input.iter()) {
        assert!((actual - sample * GAIN_6DB).abs() <= 1e-6);
    }
}
#[test]
fn surface_volume_patches_committed_plan_two_phase() {
    let mut state = state();
    let identity = identity_plan();
    let identity_file = TempPlan::new("surface-base", &identity.encode());
    let token = token_of(&prepare(&mut state, &identity_file));
    commit(&mut state, &token);
    // 让 identity 先真正生效，成为 surface 的补丁基线。
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x5eed);
    assert_bit_eq(&run_block(&mut state, &input), &input);

    // 两阶段第一步：只准备补丁，不生效。
    let prepared = request(
        &mut state,
        json!({
            "op": "surface",
            "surface": "volume",
            "data": { "gains_db": [ {"lane": 0, "gain_db": 6.0} ] }
        }),
    );
    assert_eq!(prepared["ok"], Value::Bool(true));
    assert_eq!(prepared["surface"], Value::String("volume".into()));
    assert_eq!(prepared["planned_version"], Value::from(2));
    let patched_id = prepared["graph_id"].as_str().expect("graph id").to_string();
    assert_eq!(patched_id.len(), 64);
    assert_ne!(patched_id, identity.graph_id_hex());
    let token = token_of(&prepared);

    // commit 之前仍在跑未打补丁的 identity。
    assert_bit_eq(&run_block(&mut state, &input), &input);

    let committed = commit(&mut state, &token);
    assert_eq!(committed["version"], Value::from(2));
    let input2 = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x5eee);
    let output = run_block(&mut state, &input2);
    for (index, (actual, sample)) in output.iter().zip(input2.iter()).enumerate() {
        let expected = if index % 2 == 0 {
            sample * GAIN_6DB
        } else {
            *sample
        };
        assert!(
            (actual - expected).abs() <= 1e-6,
            "sample {index}: {actual} vs {expected}"
        );
    }
    let after = status(&mut state);
    assert_eq!(after["actual_version"], Value::from(2));
}

#[test]
fn surface_routing_swap_is_two_phase_patch() {
    let mut state = state();
    let identity = identity_plan();
    let identity_file = TempPlan::new("surface-routing", &identity.encode());
    let token = token_of(&prepare(&mut state, &identity_file));
    commit(&mut state, &token);
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x1a2b);
    assert_bit_eq(&run_block(&mut state, &input), &input);

    let prepared = request(
        &mut state,
        json!({
            "op": "surface",
            "surface": "routing",
            "data": { "routes": [[0, 1], [1, 0]] }
        }),
    );
    assert_eq!(prepared["ok"], Value::Bool(true));
    let token = token_of(&prepared);
    commit(&mut state, &token);

    let input2 = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x1a2c);
    let output = run_block(&mut state, &input2);
    for frame in 0..(input2.len() / 2) {
        let actual = &output[frame * 2..frame * 2 + 2];
        let samples = &input2[frame * 2..frame * 2 + 2];
        assert!(
            (actual[0] - samples[1]).abs() <= 1e-6,
            "frame {frame}: dac0 must carry input channel 1: {} vs {}",
            actual[0],
            samples[1]
        );
        assert!(
            (actual[1] - samples[0]).abs() <= 1e-6,
            "frame {frame}: dac1 must carry input channel 0: {} vs {}",
            actual[1],
            samples[0]
        );
    }
}

#[test]
fn surface_layout_swap_is_two_phase_patch() {
    let mut state = state();
    let identity = identity_plan();
    let identity_file = TempPlan::new("surface-layout", &identity.encode());
    let token = token_of(&prepare(&mut state, &identity_file));
    commit(&mut state, &token);
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x7a11);
    assert_bit_eq(&run_block(&mut state, &input), &input);

    let prepared = request(
        &mut state,
        json!({
            "op": "surface",
            "surface": "layout",
            "data": {
                "in_channels": 2,
                "out_lanes": 2,
                "folds": [[0, 1, 1.0], [1, 0, 1.0]]
            }
        }),
    );
    assert_eq!(prepared["ok"], Value::Bool(true));
    let token = token_of(&prepared);
    commit(&mut state, &token);

    let input2 = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x7a12);
    let output = run_block(&mut state, &input2);
    for frame in 0..(input2.len() / 2) {
        let actual = &output[frame * 2..frame * 2 + 2];
        let samples = &input2[frame * 2..frame * 2 + 2];
        assert!(
            (actual[0] - samples[1]).abs() <= 1e-6,
            "frame {frame}: lane0 must fold input 1: {} vs {}",
            actual[0],
            samples[1]
        );
        assert!(
            (actual[1] - samples[0]).abs() <= 1e-6,
            "frame {frame}: lane1 must fold input 0: {} vs {}",
            actual[1],
            samples[0]
        );
    }
}

#[test]
fn surface_honours_two_phase_errors_and_truthful_capabilities() {
    let mut state = state();
    // 没有已生效方案：surface 不能凭空打补丁。
    let missing = request(
        &mut state,
        json!({
            "op": "surface",
            "surface": "volume",
            "data": { "gains_db": [ {"lane": 0, "gain_db": 0.0} ] }
        }),
    );
    assert_eq!(missing["error"], Value::String(error::NO_BASE_PLAN.into()));

    // format / mode 未接通：unsupported + 原因，不返回假成功。
    for surface in ["format", "mode"] {
        let response = request(
            &mut state,
            json!({ "op": "surface", "surface": surface, "data": {} }),
        );
        assert_eq!(
            response["error"],
            Value::String(error::UNSUPPORTED.into()),
            "surface {surface}"
        );
        assert!(response["message"]
            .as_str()
            .is_some_and(|message| message.contains("not wired")));
    }

    // 未知界面：请求内容问题。
    let unknown = request(
        &mut state,
        json!({ "op": "surface", "surface": "gainz", "data": {} }),
    );
    assert_eq!(unknown["error"], Value::String(error::BAD_REQUEST.into()));

    // 能力上报如实：三个界面接通、两个未接通且带原因。
    let capabilities = status(&mut state);
    for surface in ["volume", "routing", "layout"] {
        assert_eq!(
            capabilities["surfaces"][surface],
            Value::Bool(true),
            "surface {surface}"
        );
    }
    for surface in ["format", "mode"] {
        assert_eq!(
            capabilities["surfaces"][surface],
            Value::Bool(false),
            "surface {surface}"
        );
        assert!(capabilities["surface_reasons"][surface]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()));
    }

    // 有基线之后，非法 payload 仍是 bad_request（空列表 / 不存在的 lane）。
    let identity = identity_plan();
    let identity_file = TempPlan::new("surface-errors", &identity.encode());
    let token = token_of(&prepare(&mut state, &identity_file));
    commit(&mut state, &token);
    let empty = request(
        &mut state,
        json!({ "op": "surface", "surface": "volume", "data": { "gains_db": [] } }),
    );
    assert_eq!(empty["error"], Value::String(error::BAD_REQUEST.into()));
    let no_lane = request(
        &mut state,
        json!({ "op": "surface", "surface": "volume", "data": { "gains_db": [ {"lane": 99, "gain_db": 0.0} ] } }),
    );
    assert_eq!(no_lane["error"], Value::String(error::BAD_REQUEST.into()));
    assert!(no_lane["message"]
        .as_str()
        .is_some_and(|message| message.contains("lane 99")));
}

// ===========================================================================
// inspect：只读试运行（不产生草稿、不改变正在跑的图）
// ===========================================================================

#[test]
fn inspect_reports_ready_plan_without_staging_a_draft() {
    let mut state = state();
    let plan = identity_plan();
    let file = TempPlan::new("inspect-identity", &plan.encode());
    let response = request(
        &mut state,
        json!({
            "op": "inspect",
            "plan_path": file.path(),
            "plan_sha256": file.sha256,
        }),
    );
    assert_eq!(response["ok"], Value::Bool(true));
    assert_eq!(response["staged"], Value::Bool(false));
    assert_eq!(response["sha256"], Value::String(file.sha256.clone()));
    assert_eq!(response["graph_id"], Value::String(plan.graph_id_hex()));
    assert_eq!(response["admission"]["accepted"], Value::Bool(true));
    assert_eq!(response["compile"]["compiled"], Value::Bool(true));
    assert_eq!(response["compile"]["latency_samples"], Value::from(0));
    assert_eq!(response["plan"]["block_size"], Value::from(BLOCK_FRAMES));
    assert_eq!(response["plan"]["sample_rate"], Value::from(SAMPLE_RATE));
    assert_eq!(response["plan"]["lanes"], Value::from(u32::from(CHANNELS)));
    assert_eq!(
        response["plan"]["dac_channels"],
        Value::from(u32::from(CHANNELS))
    );

    // 只读证明：没有草稿，commit 直接 engine_token_invalid。
    let commit = request(
        &mut state,
        json!({ "op": "commit", "engine_token": "00000000000000000000000000000000" }),
    );
    assert_eq!(
        commit["error"],
        Value::String(error::ENGINE_TOKEN_INVALID.into())
    );

    // sha256 可选：不带也能试运行（诊断用途），返回值里仍有实际 sha。
    let no_sha = request(
        &mut state,
        json!({ "op": "inspect", "plan_path": file.path() }),
    );
    assert_eq!(no_sha["ok"], Value::Bool(true));
    assert_eq!(no_sha["sha256"], Value::String(file.sha256.clone()));
    assert_eq!(no_sha["admission"]["accepted"], Value::Bool(true));
}

#[test]
fn inspect_reports_admission_rejection_as_a_result_not_an_error() {
    let mut state = state();
    // 去掉矩阵段：实时图必须有矩阵 → 准入拒绝。
    let mut plan = identity_plan();
    plan.matrix = None;
    let file = TempPlan::new("inspect-reject", &plan.encode());
    let response = request(
        &mut state,
        json!({ "op": "inspect", "plan_path": file.path() }),
    );
    assert_eq!(
        response["ok"],
        Value::Bool(true),
        "inspect 是诊断，不把拒收变成 error"
    );
    assert_eq!(response["admission"]["accepted"], Value::Bool(false));
    assert!(!response["admission"]["rejections"]
        .as_array()
        .expect("rejections")
        .is_empty());
    assert_eq!(response["compile"]["compiled"], Value::Bool(false));
    assert_eq!(
        response["compile"]["error_code"],
        Value::String(error::ADMISSION_REJECTED.into())
    );
    // 仍然没有草稿。
    let commit = request(
        &mut state,
        json!({ "op": "commit", "engine_token": "00000000000000000000000000000000" }),
    );
    assert_eq!(
        commit["error"],
        Value::String(error::ENGINE_TOKEN_INVALID.into())
    );
}

#[test]
fn inspect_bad_inputs_are_bad_request() {
    let mut state = state();
    let not_a_plan = TempPlan::new("inspect-garbage", b"this is not a plan file");
    let bad = request(
        &mut state,
        json!({ "op": "inspect", "plan_path": not_a_plan.path() }),
    );
    assert_eq!(bad["error"], Value::String(error::BAD_REQUEST.into()));

    let plan = identity_plan();
    let file = TempPlan::new("inspect-sha", &plan.encode());
    let mismatch = request(
        &mut state,
        json!({
            "op": "inspect",
            "plan_path": file.path(),
            "plan_sha256": "0".repeat(64),
        }),
    );
    assert_eq!(mismatch["error"], Value::String(error::BAD_REQUEST.into()));

    let missing = request(
        &mut state,
        json!({ "op": "inspect", "plan_path": "Z:/definitely/not/here.plan" }),
    );
    assert_eq!(missing["error"], Value::String(error::BAD_REQUEST.into()));

    let wrong_type = request(
        &mut state,
        json!({ "op": "inspect", "plan_path": file.path(), "plan_sha256": 42 }),
    );
    assert_eq!(
        wrong_type["error"],
        Value::String(error::BAD_REQUEST.into())
    );
}
