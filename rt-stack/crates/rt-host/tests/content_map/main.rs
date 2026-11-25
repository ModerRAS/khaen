//! 缺中置内容适配 fake 单测（task-210：rt-host 自持 content_map；task-200/task-203 行为基线）。
//!
//! 背景：task-200 在 compat 层实现、task-203 接线到 host 的缺中置下混，因 Cursor v1.1 N-10
//! 分层约束（host 不得依赖/调用 compat S1 兼容层）迁入 rt-host 自持（本 crate 的
//! `src/content_map.rs`），本测试目标随之迁入 rt-host（compat 侧原 17 条测试保留）。
//!
//! 覆盖（与 task-203 行为一致）：
//! - 5.1 输入（h16-5.1，C 在 ch2）→ 4.1.2 输出（无 C）：C 以 −3 dB 同时混入 FL/FR；
//! - 电平正确：只放 C=1.0 时 FL/FR = 10^(−3/20) ≈ 0.7079（每个目标声道 −3 dB）；
//! - 无中置/有中置分支：输出布局含 C（5.1/5.1.2 等）→ passthrough、缓冲逐位不变；
//!   输入内容无 C（pc-2ch）→ no-op；
//! - 不重复混：C 源在混后置零；重复 apply 不重复叠加中置；其余声道（SL/SR/LFE/高度）不变；
//! - headroom/削波保护：D7 口径（预测峰值 ≤−6 dBFS、headroom ≥6 dB）；默认 program peak
//!   基准 −18 dBFS 时不 trim（−3 dB 系数不缩水）；热节目（−3 dBFS）时全源等量 trim 到 −6 dBFS；
//! - 错误路径：布局未知/角色缺失/通道冲突/中置声明与布局矛盾/非有限 program peak/缓冲长度；
//! - host 自持 overlay 读取器：T57 overlay v2 的 input/output 段解析、schema/未知字段拒收、
//!   IO 错误码（与 compat::roles 的差异见模块头注释与 evidence/cursor-fix-v1-hostmap/）。
//!
//! 本文件是集成测试 target（cargo 自动发现 tests/content_map/main.rs，与 compat 的
//! tests/center_downmix/main.rs 同模式；不改 public_registration.rs）。布局输入为真实 overlay
//! `audio/overlay/etc/tspi-audio/layout-presets.yml`（include_str! 绑定），不写临时文件。

use khaen_rt_host::content_map::{
    center_downmix_gain_linear_f64, center_downmix_worst_case_added_db, CenterDownmixConfig,
    CenterDownmixPlan, CenterDownmixSource, CenterDownmixStatus, CenterDownmixTarget,
    CenterSpeakerState, ContentMapErrorCode, LayoutPresets,
    CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS, CENTER_DOWNMIX_GAIN_DB, CENTER_DOWNMIX_GAIN_LINEAR,
    CENTER_DOWNMIX_MIN_HEADROOM_DB, CENTER_DOWNMIX_PEAK_LIMIT_DBFS, CENTER_DOWNMIX_TARGET_ROLES,
    CENTER_ROLE_ID,
};

const OVERLAY: &str =
    include_str!("../../../../../audio/overlay/etc/tspi-audio/layout-presets.yml");

fn presets() -> LayoutPresets {
    LayoutPresets::parse_overlay(OVERLAY.as_bytes()).expect("overlay layout-presets 解析失败")
}

/// 构造每通道恒定值的 planar f32 缓冲（通道数 = values.len()）。
fn planar(values: &[f32], frames: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; values.len() * frames];
    for (channel, value) in values.iter().enumerate() {
        for frame in 0..frames {
            buffer[channel * frames + frame] = *value;
        }
    }
    buffer
}

/// 取某通道平面（测试断言用）。
fn channel(buffer: &[f32], frames: usize, channel: usize) -> &[f32] {
    &buffer[channel * frames..(channel + 1) * frames]
}

fn approx(left: f32, right: f32, tolerance: f32) -> bool {
    (left - right).abs() <= tolerance
}

// ---------------------------------------------------------------------------
// 常量/契约冻结
// ---------------------------------------------------------------------------

#[test]
fn center_downmix_constants_and_d7_policy_are_frozen() {
    assert_eq!(CENTER_ROLE_ID, "C");
    assert_eq!(CENTER_DOWNMIX_TARGET_ROLES, ["FL", "FR"]);
    assert_eq!(CENTER_DOWNMIX_GAIN_DB, -3.0);
    // f32 常量必须等于 f64 精确系数（10^(−3/20)）
    let exact = center_downmix_gain_linear_f64();
    assert!(
        (CENTER_DOWNMIX_GAIN_LINEAR as f64 - exact).abs() < 1e-7,
        "f32 系数 {} 与 f64 精确值 {exact} 偏差过大",
        CENTER_DOWNMIX_GAIN_LINEAR
    );
    assert!((20.0 * exact.log10() + 3.0).abs() < 1e-6, "系数应为 −3 dB");
    // D7 headroom 常量（calibration-spec/ACCEPTANCE.md:190）
    assert_eq!(CENTER_DOWNMIX_PEAK_LIMIT_DBFS, -6.0);
    assert_eq!(CENTER_DOWNMIX_MIN_HEADROOM_DB, 6.0);
    assert_eq!(CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS, -18.0);
    // 最坏相干叠加 +20·log10(1+g) ≈ +4.65 dB（大于单段 +3 dB boost 帽，但由 headroom 门约束）
    let worst_case = center_downmix_worst_case_added_db();
    assert!(
        worst_case > 4.6 && worst_case < 4.7,
        "worst_case={worst_case}"
    );
    // 状态字符串稳定（host 报告 content.status 直接消费）
    assert_eq!(CenterDownmixStatus::Applied.as_str(), "applied");
    assert_eq!(
        CenterDownmixStatus::DisabledCenterPresent.as_str(),
        "disabled_center_present"
    );
    assert_eq!(
        CenterDownmixStatus::DisabledNoCenterContent.as_str(),
        "disabled_no_center_content"
    );
    // 错误码字符串沿用 task-200/task-203 既有诊断口径（host 自持不改契约）
    assert_eq!(
        ContentMapErrorCode::LayoutUnknown.as_str(),
        "E_CHANNEL_MAP_LAYOUT_UNKNOWN"
    );
    assert_eq!(
        ContentMapErrorCode::LayoutParse.as_str(),
        "E_CONTENT_MAP_LAYOUT_PARSE"
    );
    assert_eq!(ContentMapErrorCode::ALL.len(), 9);
}

// ---------------------------------------------------------------------------
// 主路径：5.1 输入 → 4.1.2 输出（无中置音箱）
// ---------------------------------------------------------------------------

#[test]
fn center_downmix_51_to_412_is_applied_with_expected_channels() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("5.1→4.1.2 计划构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::Applied);
    assert!(plan.is_applied());
    // h16-5.1: FL0 FR1 C2 LFE3 SL4 SR5（单 flow，6ch）
    assert_eq!(plan.source.channels, 6);
    assert_eq!(plan.source.layout_id, "h16-5.1");
    assert_eq!(plan.target.layout_id, "4.1.2");
    assert!(!plan.target.has_center_role);
    assert_eq!(plan.center_channel, Some(2));
    assert_eq!(plan.left_channel, Some(0));
    assert_eq!(plan.right_channel, Some(1));
    // 默认 program peak 基准 −18 dBFS：预测峰值 −13.35 dBFS，headroom 13.35 dB → 不 trim
    assert!(plan.headroom.within_policy);
    assert_eq!(plan.headroom.trim_db, 0.0);
    assert!((plan.headroom.predicted_peak_dbfs - (-18.0 + 4.6495)).abs() < 1e-3);
    assert!(plan.headroom.headroom_after_trim_db >= 6.0);
}

#[test]
fn center_downmix_51_input_412_output_folds_center_into_lr_only() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("计划构建失败");
    let frames = 8usize;
    // FL=0.5, FR=-0.25, C=0.25, LFE=0.1, SL=0.2, SR=-0.2
    let before = planar(&[0.5, -0.25, 0.25, 0.1, 0.2, -0.2], frames);
    let mut after = before.clone();
    plan.apply_planar_f32_in_place(&mut after, frames)
        .expect("apply 失败");
    let gain = CENTER_DOWNMIX_GAIN_LINEAR;
    // FL/FR 各叠加 g·C；其余逐位不变
    for frame in 0..frames {
        assert!(approx(after[0 * frames + frame], 0.5 + gain * 0.25, 1e-6));
        assert!(approx(after[1 * frames + frame], -0.25 + gain * 0.25, 1e-6));
        assert_eq!(after[2 * frames + frame], 0.0, "C 源必须被消费（置零）");
        assert_eq!(after[3 * frames + frame], 0.1, "LFE 不得改变");
        assert_eq!(after[4 * frames + frame], 0.2, "SL 不得改变");
        assert_eq!(after[5 * frames + frame], -0.2, "SR 不得改变");
    }
    assert_eq!(channel(&after, frames, 2), vec![0.0f32; frames].as_slice());
}

#[test]
fn center_downmix_center_only_level_is_minus_3db_each_channel() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("计划构建失败");
    let frames = 4usize;
    // 只放中置：C=1.0，其余全 0
    let mut buffer = planar(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0], frames);
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("apply 失败");
    let left = channel(&buffer, frames, 0);
    let right = channel(&buffer, frames, 1);
    let expected = 10.0f32.powf(-3.0 / 20.0);
    for frame in 0..frames {
        assert!(
            approx(left[frame], expected, 1e-6),
            "FL[{frame}]={} 应 ≈{expected}",
            left[frame]
        );
        assert!(
            approx(right[frame], expected, 1e-6),
            "FR[{frame}]={} 应 ≈{expected}",
            right[frame]
        );
        // 以 dB 复核：每个目标声道 −3 dB
        let db = 20.0 * (left[frame].max(f32::MIN_POSITIVE)).log10();
        assert!((db + 3.0).abs() < 1e-4, "FL 电平 {db} dB 不是 −3 dB");
    }
    // 左/右完全等量（虚拟中置居中）
    assert_eq!(left, right);
}

#[test]
fn center_downmix_per_frame_values_are_processed_independently() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("计划构建失败");
    let frames = 4usize;
    let left_in = [0.25f32, -0.5, 0.0, 0.125];
    let right_in = [-0.25f32, 0.5, 0.0, -0.125];
    let center_in = [1.0f32, -2.0, 0.5, 0.0];
    let mut buffer = vec![0.0f32; 6 * frames];
    for frame in 0..frames {
        buffer[0 * frames + frame] = left_in[frame];
        buffer[1 * frames + frame] = right_in[frame];
        buffer[2 * frames + frame] = center_in[frame];
    }
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("apply 失败");
    let gain = CENTER_DOWNMIX_GAIN_LINEAR;
    for frame in 0..frames {
        assert!(approx(
            buffer[0 * frames + frame],
            left_in[frame] + gain * center_in[frame],
            1e-6
        ));
        assert!(approx(
            buffer[1 * frames + frame],
            right_in[frame] + gain * center_in[frame],
            1e-6
        ));
        assert_eq!(buffer[2 * frames + frame], 0.0);
    }
}

#[test]
fn center_downmix_does_not_repeat_center_on_second_apply() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("计划构建失败");
    let frames = 4usize;
    let mut buffer = planar(&[0.5, -0.25, 0.25, 0.1, 0.2, -0.2], frames);
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("第一次 apply 失败");
    let after_first = buffer.clone();
    // C 已置零 → 第二次 apply 不得再叠加中置（默认无 trim，结果逐位相同）
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("第二次 apply 失败");
    assert_eq!(buffer, after_first, "重复 apply 不得重复叠加中置");
    for frame in 0..frames {
        assert_eq!(buffer[2 * frames + frame], 0.0);
    }
}

// ---------------------------------------------------------------------------
// 分支：有中置 / 无中置内容
// ---------------------------------------------------------------------------

#[test]
fn center_downmix_with_center_layout_is_passthrough() {
    // 输出布局 5.1 含 C 角色（有中置配置）→ 不启用、缓冲逐位不变
    let plan =
        CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "5.1", &CenterDownmixConfig::auto())
            .expect("5.1 输出构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::DisabledCenterPresent);
    assert!(plan.target.has_center_role);
    assert!(!plan.is_applied());
    let frames = 4usize;
    let before = planar(&[0.5, -0.25, 0.25, 0.0, 0.0, 0.0], frames);
    let mut after = before.clone();
    plan.apply_planar_f32_in_place(&mut after, frames)
        .expect("passthrough apply 失败");
    assert_eq!(after, before, "有中置配置不得改变任何样本");

    // 其它含 C 的输出布局（5.3.2）同样是 passthrough
    let plan_532 = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "5.3.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("5.3.2 输出构建失败");
    assert_eq!(plan_532.status, CenterDownmixStatus::DisabledCenterPresent);
}

#[test]
fn center_downmix_input_without_center_is_noop() {
    // pc-2ch 输入（无 C）+ 4.1.2 输出 → 无中置内容，不启用
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "pc-2ch",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("2ch→4.1.2 计划构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::DisabledNoCenterContent);
    assert_eq!(plan.source.channels, 2);
    assert_eq!(plan.center_channel, None);
    let frames = 4usize;
    let before = planar(&[0.5, -0.25], frames);
    let mut after = before.clone();
    plan.apply_planar_f32_in_place(&mut after, frames)
        .expect("no-op apply 失败");
    assert_eq!(after, before);
}

#[test]
fn center_downmix_multistream_input_uses_flattened_center_index() {
    // h16-5.1.2 输入：streams [6,2]，C=2（stream0/ch2）、TFL=6、TFR=7；输出 4.1.2 无 C
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1.2",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("5.1.2→4.1.2 计划构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::Applied);
    assert_eq!(plan.source.channels, 8);
    assert_eq!(plan.center_channel, Some(2));
    assert_eq!(plan.source.role_channel("TFL"), Some(6));
    assert_eq!(plan.source.role_channel("TFR"), Some(7));
    let frames = 4usize;
    // FL=0.1 FR=0.2 C=0.25 LFE=0 SL=0 SR=0 TFL=0.5 TFR=-0.5（高度不得被改）
    let mut buffer = bright_planar_512(frames);
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("apply 失败");
    let gain = CENTER_DOWNMIX_GAIN_LINEAR;
    for frame in 0..frames {
        assert!(approx(buffer[0 * frames + frame], 0.1 + gain * 0.25, 1e-6));
        assert!(approx(buffer[1 * frames + frame], 0.2 + gain * 0.25, 1e-6));
        assert_eq!(buffer[2 * frames + frame], 0.0);
        assert_eq!(buffer[6 * frames + frame], 0.5, "TFL 高度不得改变");
        assert_eq!(buffer[7 * frames + frame], -0.5, "TFR 高度不得改变");
    }
}

#[test]
fn center_downmix_nsp_512_single_stream_folds_center_keeps_lfe() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "nsp-5.1.2",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("nsp-5.1.2→4.1.2 计划构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::Applied);
    assert_eq!(plan.source.channels, 8);
    assert_eq!(plan.center_channel, Some(2));
    assert_eq!(plan.source.role_channel("LFE"), Some(3));
    assert_eq!(plan.source.role_channel("TFL"), Some(6));
    assert_eq!(plan.headroom.program_peak_dbfs, -18.0);
    assert_eq!(plan.headroom.trim_linear(), 1.0, "−18 dBFS 基准不得触发 D7 trim");
    let frames = 4usize;
    let mut buffer = vec![0.0f32; 8 * frames];
    for frame in 0..frames {
        buffer[0 * frames + frame] = 0.1;
        buffer[1 * frames + frame] = 0.2;
        buffer[2 * frames + frame] = 0.25;
        buffer[3 * frames + frame] = -0.4;
        buffer[4 * frames + frame] = 0.3;
        buffer[5 * frames + frame] = -0.3;
        buffer[6 * frames + frame] = 0.5;
        buffer[7 * frames + frame] = -0.5;
    }
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("apply 失败");
    let gain = CENTER_DOWNMIX_GAIN_LINEAR;
    for frame in 0..frames {
        assert!((buffer[0 * frames + frame] - (0.1 + gain * 0.25)).abs() < 1e-6);
        assert!((buffer[1 * frames + frame] - (0.2 + gain * 0.25)).abs() < 1e-6);
        assert_eq!(buffer[2 * frames + frame], 0.0);
        assert_eq!(buffer[3 * frames + frame], -0.4, "LFE 必须逐位不变");
        assert_eq!(buffer[4 * frames + frame], 0.3);
        assert_eq!(buffer[5 * frames + frame], -0.3);
        assert_eq!(buffer[6 * frames + frame], 0.5);
        assert_eq!(buffer[7 * frames + frame], -0.5);
    }
    let once = buffer.clone();
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("第二次 apply");
    assert_eq!(buffer, once, "中置已清零，再次 apply 不得叠加");
}

fn bright_planar_512(frames: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; 8 * frames];
    for frame in 0..frames {
        buffer[0 * frames + frame] = 0.1;
        buffer[1 * frames + frame] = 0.2;
        buffer[2 * frames + frame] = 0.25;
        buffer[6 * frames + frame] = 0.5;
        buffer[7 * frames + frame] = -0.5;
    }
    buffer
}

#[test]
fn center_downmix_714_input_412_output_folds_only_center() {
    // h16-7.1.4（streams [8,4]）→ 12ch：C=2、BL=6、BR=7、TFL=8、TFR=9、TRL=10、TRR=11
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-7.1.4",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("7.1.4→4.1.2 计划构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::Applied);
    assert_eq!(plan.source.channels, 12);
    assert_eq!(plan.center_channel, Some(2));
    assert_eq!(plan.source.role_channel("BL"), Some(6));
    assert_eq!(plan.source.role_channel("BR"), Some(7));
    assert_eq!(plan.source.role_channel("TFL"), Some(8));
    assert_eq!(plan.source.role_channel("TRR"), Some(11));
    let frames = 2usize;
    let mut buffer = vec![0.0f32; 12 * frames];
    for frame in 0..frames {
        buffer[0 * frames + frame] = 0.25; // FL
        buffer[1 * frames + frame] = -0.5; // FR
        buffer[2 * frames + frame] = 1.0; // C
        buffer[6 * frames + frame] = 0.3; // BL
        buffer[7 * frames + frame] = -0.3; // BR
        buffer[8 * frames + frame] = 0.4; // TFL
        buffer[11 * frames + frame] = -0.4; // TRR
    }
    plan.apply_planar_f32_in_place(&mut buffer, frames)
        .expect("apply 失败");
    let gain = CENTER_DOWNMIX_GAIN_LINEAR;
    for frame in 0..frames {
        assert!(approx(buffer[0 * frames + frame], 0.25 + gain, 1e-6));
        assert!(approx(buffer[1 * frames + frame], -0.5 + gain, 1e-6));
        assert_eq!(buffer[2 * frames + frame], 0.0);
        // 环绕/后环绕/高度全部逐位不变
        assert_eq!(buffer[6 * frames + frame], 0.3);
        assert_eq!(buffer[7 * frames + frame], -0.3);
        assert_eq!(buffer[8 * frames + frame], 0.4);
        assert_eq!(buffer[11 * frames + frame], -0.4);
    }
}

// ---------------------------------------------------------------------------
// headroom / 削波保护（D7 口径）
// ---------------------------------------------------------------------------

#[test]
fn center_downmix_default_program_peak_is_within_d7_policy_without_trim() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("计划构建失败");
    let headroom = plan.headroom;
    let expected_predicted =
        CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS + center_downmix_worst_case_added_db();
    assert!((headroom.predicted_peak_dbfs - expected_predicted).abs() < 1e-9);
    assert!(headroom.predicted_peak_dbfs <= CENTER_DOWNMIX_PEAK_LIMIT_DBFS);
    assert!(headroom.headroom_after_trim_db >= CENTER_DOWNMIX_MIN_HEADROOM_DB);
    assert_eq!(headroom.trim_db, 0.0);
    assert_eq!(headroom.trim_linear(), 1.0);
    assert!(headroom.within_policy);
}

#[test]
fn center_downmix_hot_program_trims_to_d7_headroom() {
    // program peak −3 dBFS + 最坏相干叠加 +4.65 → 预测 +1.65 dBFS 超 −6 dBFS 门 → trim
    let config = CenterDownmixConfig::auto().with_program_peak_dbfs(-3.0);
    let plan = CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "4.1.2", &config)
        .expect("计划构建失败");
    let headroom = plan.headroom;
    assert!(
        headroom.trim_db < 0.0,
        "热节目必须衰减，实得 {}",
        headroom.trim_db
    );
    assert!(
        (headroom.predicted_peak_after_trim_dbfs - CENTER_DOWNMIX_PEAK_LIMIT_DBFS).abs() < 1e-9,
        "trim 后预测峰值应压到 −6 dBFS，实得 {}",
        headroom.predicted_peak_after_trim_dbfs
    );
    assert!((headroom.headroom_after_trim_db - CENTER_DOWNMIX_MIN_HEADROOM_DB).abs() < 1e-9);
    assert!(headroom.within_policy);
    let trim_linear = headroom.trim_linear();
    assert!(trim_linear < 1.0);
    // apply 复核：全局 trim（所有声道等量）+ C 叠加
    let frames = 2usize;
    let before = planar(&[0.5, -0.25, 1.0, 0.1, 0.2, -0.2], frames);
    let mut after = before.clone();
    plan.apply_planar_f32_in_place(&mut after, frames)
        .expect("apply 失败");
    let gain = CENTER_DOWNMIX_GAIN_LINEAR;
    for frame in 0..frames {
        assert!(approx(
            after[0 * frames + frame],
            0.5 * trim_linear + gain * 1.0 * trim_linear,
            1e-6
        ));
        assert!(approx(
            after[1 * frames + frame],
            -0.25 * trim_linear + gain * 1.0 * trim_linear,
            1e-6
        ));
        assert_eq!(after[2 * frames + frame], 0.0);
        // 其余源也等量 trim（保持声道间相对电平）
        assert!(approx(after[3 * frames + frame], 0.1 * trim_linear, 1e-6));
        assert!(approx(after[4 * frames + frame], 0.2 * trim_linear, 1e-6));
        assert!(approx(after[5 * frames + frame], -0.2 * trim_linear, 1e-6));
    }
}

#[test]
fn center_downmix_non_finite_program_peak_is_rejected() {
    let config = CenterDownmixConfig::auto().with_program_peak_dbfs(f64::NAN);
    let error = CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "4.1.2", &config)
        .expect_err("NaN program peak 必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::HeadroomConfig);
    assert_eq!(error.code_str(), "E_CHANNEL_MAP_HEADROOM_CONFIG");
}

// ---------------------------------------------------------------------------
// 配置矛盾 / 错误路径 / 缓冲校验
// ---------------------------------------------------------------------------

#[test]
fn center_downmix_center_policy_conflicts_are_rejected() {
    // 声明 Present 但输出布局无 C
    let config = CenterDownmixConfig::auto().with_center_state(CenterSpeakerState::Present);
    let error = CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "4.1.2", &config)
        .expect_err("Present + 无 C 布局必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::CenterPolicyConflict);

    // 声明 Absent 但输出布局含 C（不改有中置配置）
    let config = CenterDownmixConfig::auto().with_center_state(CenterSpeakerState::Absent);
    let error = CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "5.1", &config)
        .expect_err("Absent + 含 C 布局必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::CenterPolicyConflict);

    // 显式 Present 且布局含 C：合法，passthrough
    let config = CenterDownmixConfig::auto().with_center_state(CenterSpeakerState::Present);
    let plan = CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "5.1", &config)
        .expect("Present + 含 C 布局应合法");
    assert_eq!(plan.status, CenterDownmixStatus::DisabledCenterPresent);
}

#[test]
fn center_downmix_layout_and_source_validation() {
    let config = CenterDownmixConfig::auto();
    // 未知输入/输出布局
    let error = CenterDownmixPlan::from_layouts(&presets(), "no-such-input", "4.1.2", &config)
        .expect_err("未知输入布局必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::LayoutUnknown);
    let error = CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "no-such-output", &config)
        .expect_err("未知输出布局必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::LayoutUnknown);

    // 源域校验：通道重复 / 通道越界 / 角色表空
    let duplicate_channel = CenterDownmixSource::new(
        "explicit",
        6,
        vec![
            ("FL".to_string(), 0),
            ("FR".to_string(), 0),
            ("C".to_string(), 2),
        ],
    )
    .expect_err("通道重复必须拒收");
    assert_eq!(
        duplicate_channel.code,
        ContentMapErrorCode::RoleChannelConflict
    );
    let out_of_range = CenterDownmixSource::new(
        "explicit",
        6,
        vec![("FL".to_string(), 6), ("FR".to_string(), 1)],
    )
    .expect_err("通道越界必须拒收");
    assert_eq!(out_of_range.code, ContentMapErrorCode::LayoutChannels);
    let empty = CenterDownmixSource::new("explicit", 6, Vec::new()).expect_err("空角色表必须拒收");
    assert_eq!(empty.code, ContentMapErrorCode::RoleMissing);
}

#[test]
fn center_downmix_explicit_role_channels_path_is_supported() {
    // T43 H16 RolePlan/显式配置路径：与 layout 预设解耦
    let source = CenterDownmixSource::new(
        "h16-roleplan",
        6,
        vec![
            ("FL".to_string(), 0),
            ("FR".to_string(), 1),
            ("C".to_string(), 2),
            ("LFE".to_string(), 3),
            ("SL".to_string(), 4),
            ("SR".to_string(), 5),
        ],
    )
    .expect("显式源域构造失败");
    let target = CenterDownmixTarget::new("4.1.2-explicit", false);
    let plan = CenterDownmixPlan::build(&source, &target, &CenterDownmixConfig::auto())
        .expect("显式源域计划构建失败");
    assert_eq!(plan.status, CenterDownmixStatus::Applied);
    assert_eq!(plan.center_channel, Some(2));
}

#[test]
fn center_downmix_buffer_size_is_validated_without_panic() {
    let plan = CenterDownmixPlan::from_layouts(
        &presets(),
        "h16-5.1",
        "4.1.2",
        &CenterDownmixConfig::auto(),
    )
    .expect("计划构建失败");
    // 长度不足
    let mut short = vec![0.0f32; 6 * 4 - 1];
    let error = plan
        .apply_planar_f32_in_place(&mut short, 4)
        .expect_err("长度不符必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::BufferSize);
    // frames 与缓冲不符
    let mut wrong_frames = vec![0.0f32; 6 * 4];
    let error = plan
        .apply_planar_f32_in_place(&mut wrong_frames, 3)
        .expect_err("frames 不符必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::BufferSize);
    // frames=0
    let error = plan
        .apply_planar_f32_in_place(&mut [], 0)
        .expect_err("frames=0 必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::BufferSize);
    // 未启用计划的缓冲也校验（passthrough 不是绕过契约）
    let passthrough =
        CenterDownmixPlan::from_layouts(&presets(), "h16-5.1", "5.1", &CenterDownmixConfig::auto())
            .expect("5.1 输出计划构建失败");
    let mut short = vec![0.0f32; 3];
    let error = passthrough
        .apply_planar_f32_in_place(&mut short, 4)
        .expect_err("passthrough 也必须校验缓冲");
    assert_eq!(error.code, ContentMapErrorCode::BufferSize);
}

// ---------------------------------------------------------------------------
// host 自持 overlay 读取器（T57 layout-presets v2；input/output 段）
// ---------------------------------------------------------------------------

#[test]
fn layout_presets_reader_parses_t57_overlay_input_output_sections() {
    let presets = presets();
    assert_eq!(presets.schema(), "tspi-layout-presets/v2");
    // 输入布局：9 个（h16-5.1/7.1/5.1.2/5.1.4/7.1.2/7.1.4 + nsp-5.1.2 + pc-2ch/pc-8ch）
    assert_eq!(presets.input_layouts().len(), 9);
    let nsp512 = presets.input_layout("nsp-5.1.2").expect("nsp-5.1.2");
    assert_eq!(nsp512.channels, Some(8));
    assert_eq!(nsp512.streams, vec![8]);
    assert_eq!(nsp512.roles[2].id, "C");
    assert_eq!(nsp512.roles[2].channel, 2);
    assert_eq!(nsp512.roles[6].id, "TFL");
    assert_eq!(nsp512.roles[6].channel, 6);
    let h1651 = presets.input_layout("h16-5.1").expect("h16-5.1");
    assert_eq!(h1651.channels, None, "channels: auto = 以实测为准");
    assert_eq!(h1651.streams, vec![6]);
    assert_eq!(h1651.roles.len(), 6);
    assert_eq!(h1651.roles[2].id, "C");
    assert_eq!(h1651.roles[2].stream, 0);
    assert_eq!(h1651.roles[2].channel, 2);
    // 多 flow 输入：streams [6,2] → TFL=stream1/ch0（展平后 6）
    let h16512 = presets.input_layout("h16-5.1.2").expect("h16-5.1.2");
    assert_eq!(h16512.streams, vec![6, 2]);
    assert_eq!(
        h16512
            .roles
            .iter()
            .find(|role| role.id == "TFL")
            .map(|role| (role.stream, role.channel)),
        Some((1, 0))
    );
    // 输出布局：14 个；4.1.2 无 C 角色、SUB1 落槽位 7（板端真相）
    assert_eq!(presets.output_layouts().len(), 14);
    let layout_412 = presets.output_layout("4.1.2").expect("4.1.2");
    assert!(!layout_412.roles.iter().any(|role| role.id == "C"));
    assert_eq!(
        layout_412
            .roles
            .iter()
            .find(|role| role.id == "SUB1")
            .map(|role| role.outputs.clone()),
        Some(vec![7])
    );
    // 含 C 的输出布局（5.1/5.3.2）→ 中置存在
    assert!(presets
        .output_layout("5.1")
        .expect("5.1")
        .roles
        .iter()
        .any(|role| role.id == "C"));
    assert!(presets
        .output_layout("5.3.2")
        .expect("5.3.2")
        .roles
        .iter()
        .any(|role| role.id == "C"));
}

#[test]
fn layout_presets_reader_rejects_schema_unknown_fields_and_duplicate_slots() {
    // schema 不匹配
    let wrong_schema = OVERLAY.replace(
        "schema: tspi-layout-presets/v2",
        "schema: tspi-layout-presets/v1",
    );
    let error =
        LayoutPresets::parse_overlay(wrong_schema.as_bytes()).expect_err("schema 不匹配必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::LayoutParse);
    assert!(error.to_string().contains("schema"), "{error}");

    // 未知顶层字段
    let unknown_top = format!("{OVERLAY}\nunknown_section:\n  a: b\n");
    let error =
        LayoutPresets::parse_overlay(unknown_top.as_bytes()).expect_err("未知顶层字段必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::LayoutParse);
    assert!(error.to_string().contains("unknown_section"), "{error}");

    // 输出容器槽位重复（scatter 必须单射）
    let duplicate_slot = concat!(
        "schema: tspi-layout-presets/v2\n",
        "output_layouts:\n",
        "  dup:\n",
        "    roles:\n",
        "      - {id: FL, label: L, outputs: [0]}\n",
        "      - {id: FR, label: R, outputs: [0]}\n",
    );
    let error =
        LayoutPresets::parse_overlay(duplicate_slot.as_bytes()).expect_err("重复容器槽位必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::LayoutParse);
    assert!(error.to_string().contains("被多个角色占用"), "{error}");

    // 缺少 output_layouts 段
    let missing_outputs = "schema: tspi-layout-presets/v2\ninput_layouts: {}\n";
    let error = LayoutPresets::parse_overlay(missing_outputs.as_bytes())
        .expect_err("缺少 output_layouts 必须拒收");
    assert_eq!(error.code, ContentMapErrorCode::LayoutParse);
}

#[test]
fn layout_presets_reader_reports_io_errors_and_supports_block_style_roles() {
    // IO 失败 = host 自有 E_CONTENT_MAP_IO（启用 [content] 时缺文件一律拒绝，不降级）
    let error = LayoutPresets::load_overlay_file(std::path::Path::new(
        "no-such-dir/definitely-missing-layout-presets.yml",
    ))
    .expect_err("缺失 overlay 文件必须报 IO 错误");
    assert_eq!(error.code, ContentMapErrorCode::Io);
    assert_eq!(error.code_str(), "E_CONTENT_MAP_IO");

    // 块状多行角色条目（WebUI profile 可能使用）也能解析（最小读取器支持块映射序列条目）
    let block_style = concat!(
        "schema: tspi-layout-presets/v2\n",
        "input_layouts:\n",
        "  blk-5.1:\n",
        "    channels: 6\n",
        "    roles:\n",
        "      - id: FL\n",
        "        channel: 0\n",
        "      - id: FR\n",
        "        channel: 1\n",
        "      - id: C\n",
        "        channel: 2\n",
        "output_layouts:\n",
        "  4.1.2:\n",
        "    roles:\n",
        "      - id: FL\n",
        "        outputs: [0]\n",
        "      - id: FR\n",
        "        outputs: [1]\n",
    );
    let presets = LayoutPresets::parse_overlay(block_style.as_bytes()).expect("块状条目应解析");
    let source = CenterDownmixSource::from_layout(&presets, "blk-5.1").expect("块状输入布局");
    assert_eq!(source.channels, 6);
    assert_eq!(source.role_channel("C"), Some(2));
    let target = CenterDownmixTarget::from_layout(&presets, "4.1.2").expect("块状输出布局");
    assert!(!target.has_center_role);
}
