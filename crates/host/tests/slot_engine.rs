//! SlotEngine 行为测试：块边界换图、换图后的图信息（epoch/version/applied_block）、
//! 与 BlockEngine 等价的静音语义、错误路径不吞掉待生效命令。

use khaen_host::{EngineConfig, EngineError, GraphInfo, Processor, SlotEngine};

/// 可区分新旧图的处理链：输出 = 输入 × 增益。
struct Gain(f32);

impl Processor for Gain {
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        for (out, sample) in output.iter_mut().zip(input.iter()) {
            *out = *sample * self.0;
        }
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        block_frames: 8,
        sample_rate: 96_000,
        input_channels: 2,
        output_channels: 2,
    }
}

/// 固定测试信号：每块 8 帧 × 2 通道。
fn input(block: u64) -> Vec<f32> {
    (0..16)
        .map(|index| (index as f32 + block as f32 * 0.5) * 1.0e-2 - 0.1)
        .collect()
}

fn engine(gain: f32) -> SlotEngine {
    SlotEngine::new(config(), Box::new(Gain(gain))).expect("engine")
}

fn run(engine: &mut SlotEngine, block: u64) -> Vec<f32> {
    engine.run_block(&input(block)).expect("block").to_vec()
}

#[test]
fn initial_processor_runs_without_graph_info() {
    let mut slot = engine(1.0);
    assert_eq!(slot.output_samples_per_block(), 16);
    assert_eq!(slot.graph_info(), GraphInfo::default());
    assert!(!slot.switch_pending());

    let expected = input(0);
    let out = run(&mut slot, 0);
    assert_eq!(out, expected);
    assert_eq!(slot.stats().blocks, 1);
    assert_eq!(slot.graph_info(), GraphInfo::default());
}

#[test]
fn switch_takes_effect_at_next_block_boundary() {
    let mut slot = engine(1.0);
    let first = input(0);
    assert_eq!(run(&mut slot, 0), first);

    // stage 只登记：返回"下一块"块号，此时状态快照仍是旧图（未加载方案 = None），不谎报。
    let staged_block = slot.stage_processor(Box::new(Gain(2.0)), 7, "graph-7".to_string());
    assert_eq!(staged_block, 2);
    assert!(slot.switch_pending());
    assert_eq!(slot.graph_info(), GraphInfo::default());

    // 下一块入口换图：本块从头就用新图处理。
    let out = run(&mut slot, 1);
    for (index, (actual, sample)) in out.iter().zip(input(1).iter()).enumerate() {
        assert_eq!(*actual, sample * 2.0, "sample {index}");
    }
    assert!(!slot.switch_pending());
    assert_eq!(
        slot.graph_info(),
        GraphInfo {
            graph_id: Some("graph-7".to_string()),
            epoch: Some(1),
            version: Some(7),
            applied_block: Some(2),
        }
    );
}

#[test]
fn every_applied_switch_increments_epoch() {
    let mut slot = engine(1.0);
    let _ = run(&mut slot, 0);
    let _ = run(&mut slot, 1);
    slot.stage_processor(Box::new(Gain(0.5)), 8, "graph-8".to_string());
    let _ = run(&mut slot, 2);
    let info = slot.graph_info();
    assert_eq!(info.epoch, Some(1));
    assert_eq!(info.version, Some(8));
    assert_eq!(info.applied_block, Some(3));

    // 模拟控制线程在下一次 stage 时释放退役的旧图，不影响语义。
    slot.stage_processor(Box::new(Gain(4.0)), 9, "graph-9".to_string());
    let out = run(&mut slot, 3);
    for (actual, sample) in out.iter().zip(input(3).iter()) {
        assert_eq!(*actual, sample * 4.0);
    }
    let info = slot.graph_info();
    assert_eq!(info.epoch, Some(2));
    assert_eq!(info.version, Some(9));
    assert_eq!(info.applied_block, Some(4));
    assert_eq!(info.graph_id.as_deref(), Some("graph-9"));
}

#[test]
fn last_stage_wins_when_no_block_runs_between() {
    let mut slot = engine(1.0);
    let _ = run(&mut slot, 0);
    slot.stage_processor(Box::new(Gain(2.0)), 1, "graph-a".to_string());
    slot.stage_processor(Box::new(Gain(3.0)), 2, "graph-b".to_string());
    let out = run(&mut slot, 1);
    for (actual, sample) in out.iter().zip(input(1).iter()) {
        assert_eq!(*actual, sample * 3.0);
    }
    let info = slot.graph_info();
    assert_eq!(info.epoch, Some(1), "两张未生效的图只算一次换图");
    assert_eq!(info.version, Some(2));
    assert_eq!(info.graph_id.as_deref(), Some("graph-b"));
}

#[test]
fn mute_is_block_boundary_and_idempotent_like_block_engine() {
    let mut slot = engine(1.0);
    let receipt = slot.request_mute(true);
    assert!(receipt.mute);
    assert_eq!(receipt.applied_block, 1);
    assert!(!receipt.idempotent);
    assert!(!slot.stats().mute, "命令只是排到下个块");
    let out = run(&mut slot, 0);
    assert!(out.iter().all(|sample| *sample == 0.0));
    assert!(slot.stats().mute);
    assert_eq!(slot.stats().mute_applied_block, 1);

    let repeat = slot.request_mute(true);
    assert!(repeat.idempotent);
    assert_eq!(repeat.applied_block, 1);

    let unmute = slot.request_mute(false);
    assert!(!unmute.idempotent);
    assert_eq!(unmute.applied_block, 2);
    assert_eq!(run(&mut slot, 1), input(1));
}

#[test]
fn wrong_block_length_keeps_pending_switch_for_next_valid_block() {
    let mut slot = engine(1.0);
    slot.stage_processor(Box::new(Gain(2.0)), 1, "graph-1".to_string());
    match slot.run_block(&[0.0f32; 7]) {
        Err(EngineError::WrongBlockLength { expected, actual }) => {
            assert_eq!(expected, 16);
            assert_eq!(actual, 7);
        }
        other => panic!("expected WrongBlockLength, got {other:?}"),
    }
    // 块没有开始：待生效切换仍在，图信息未变。
    assert!(slot.switch_pending());
    assert_eq!(slot.graph_info(), GraphInfo::default());
    assert_eq!(slot.stats().blocks, 0);
    let out = run(&mut slot, 0);
    assert_eq!(slot.graph_info().applied_block, Some(1));
    for (actual, sample) in out.iter().zip(input(0).iter()) {
        assert_eq!(*actual, sample * 2.0);
    }
}

#[test]
fn graph_switch_and_mute_can_apply_in_same_block() {
    let mut slot = engine(1.0);
    slot.stage_processor(Box::new(Gain(2.0)), 1, "graph-1".to_string());
    let receipt = slot.request_mute(true);
    assert_eq!(receipt.applied_block, 1);
    let out = run(&mut slot, 0);
    // 静音优先：新图已换上，但本块输出被静音清零。
    assert!(out.iter().all(|sample| *sample == 0.0));
    assert_eq!(slot.graph_info().epoch, Some(1));
    assert_eq!(slot.graph_info().applied_block, Some(1));
    assert!(slot.stats().mute);
    assert!(slot.stats().pending_commands >= 2);
}
