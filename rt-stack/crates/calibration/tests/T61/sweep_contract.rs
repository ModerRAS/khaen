//! T61 私有集成测试（crate 公共 API 契约） —— WBS T61 写域：crates/calibration/tests/T61/。
//!
//! 注册纪律（rt-stack/README.md §5）：leaf 测试放 tests/<Txx>/，由 integrator 在
//! tests/public_registration.rs 加 #[path = "T61/sweep_contract.rs"] mod sweep_contract; 后由 cargo 执行。
//! 本文件只用 tspi_calibration::sweep 的公开 API（模拟 T62/T66 的最小消费方式），因此也可用
//! rustc --test 直接对 build 出的 rlib 做独立编译/执行验证（见 evidence/T61/MANIFEST.md）。
//!
//! 覆盖：WBS T61 DoD 的公共契约面 —— 逐输出扫频、公共 render frame/slot、T1 −12 dBFS/kMainTrim=0、
//! 声学 −30 dBFS 与 ≥0.5 s 通道静音、trim 补偿不得破 −1 dBFS 峰值门、cancel、clip 保护停止、
//! fake backend 逐段样本验证、确定性、block 描述连续/标记。

use tspi_calibration::job::CancelToken;
use tspi_calibration::sweep::{
    compile_timeline, FakeSweepSink, SweepBlockInfo, SweepConfig, SweepError, SweepOutput,
    SweepPlaybackSink, SweepPlayer, SweepSegmentKind, SweepSinkAck, SweepStimulus, SweepStopReason,
    SweepTimeline, TrimPolicy, ACOUSTIC_LEVEL_DBFS, MEASUREMENT_PEAK_CEILING_DBFS,
    T1_LOOPBACK_LEVEL_DBFS,
};

fn outputs(count: usize) -> Vec<SweepOutput> {
    (0..count)
        .map(|index| SweepOutput::new(index as u32, format!("CH{index}"), index as u32))
        .collect()
}

fn short_config(output_count: usize, repeats: u32) -> SweepConfig {
    let mut config = SweepConfig::loopback("t61-session", outputs(output_count));
    config.block_frames = 64;
    config.stimulus = SweepStimulus::log_sweep(100.0, 1_000.0, 0.05, -12.0, 2.0, repeats, 0.2);
    config.pre_seconds = 0.2;
    config.post_seconds = 0.2;
    config
}

fn long_config() -> SweepConfig {
    let mut config = short_config(1, 1);
    config.stimulus.seconds = 0.5;
    config
}

fn run_complete(
    config: SweepConfig,
) -> (
    tspi_calibration::sweep::SweepRunReport,
    FakeSweepSink,
    SweepTimeline,
) {
    let timeline = compile_timeline(&config).expect("timeline");
    let mut player = SweepPlayer::new(config).expect("player");
    let mut sink = FakeSweepSink::new(
        timeline.playback_channels as usize,
        timeline.block_frames as usize,
    );
    let cancel = CancelToken::new();
    let report = player.run(&mut sink, &cancel).expect("run");
    (report, sink, timeline)
}

#[test]
fn public_api_loopback_sweep_matches_t1_contract() {
    let config = short_config(2, 1);
    assert_eq!(config.k_main_trim_db, 0.0);
    assert_eq!(config.trim_policy, TrimPolicy::RecordOnly);
    let timeline = compile_timeline(&config).expect("timeline");
    assert_eq!(timeline.level.drive_level_dbfs, T1_LOOPBACK_LEVEL_DBFS);
    assert_eq!(timeline.level.k_main_trim_db, 0.0);
    assert_eq!(
        timeline.protection.peak_ceiling_dbfs,
        MEASUREMENT_PEAK_CEILING_DBFS
    );
    assert_eq!(
        timeline.first_sweep_render_frame(),
        Some(timeline.pre_frames)
    );
    let json = timeline.to_json();
    assert!(json.contains("\"drive_level_dbfs\":-12"));
    assert!(json.contains("\"k_main_trim_db\":0"));
    assert!(json.contains("\"schema\":\"tspi-sweep-timeline/v1\""));

    let (report, sink, timeline) = run_complete(config);
    assert!(report.outcome.is_completed());
    assert!(sink.finished);
    assert!(sink.aborted.is_none());
    assert_eq!(sink.frame_count(), timeline.total_frames as usize);
    assert!(sink.blocks().last().unwrap().info.is_drain_tail());
    for segment in &timeline.segments {
        let start = segment.start_frame as usize;
        let end = segment.end_frame() as usize;
        if segment.kind == SweepSegmentKind::Sweep {
            let output = segment.output_index.expect("output") as usize;
            let channel = timeline.outputs[output].channel as usize;
            let samples = &sink.channel(channel)[start..end];
            assert_eq!(samples[0], 0.0);
            assert_eq!(samples[samples.len() - 1], 0.0);
            let peak = samples
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.9 * timeline.level.peak_linear as f32);
        } else {
            for channel in 0..sink.channels().len() {
                assert!(
                    sink.channel(channel)[start..end]
                        .iter()
                        .all(|sample| *sample == 0.0),
                    "silence segment must be all zero"
                );
            }
        }
    }
}

#[test]
fn public_api_timeline_matches_t48_t49_render_frame_contract() {
    let mut config = short_config(2, 3);
    config.stimulus = SweepStimulus::log_sweep(20.0, 20_000.0, 0.05, -12.0, 2.0, 3, 0.2);
    let timeline = compile_timeline(&config).expect("timeline");
    let sweep = timeline.actual_stimulus.sweep_frames;
    let gap = timeline.actual_stimulus.gap_frames;
    let expected_interval = 3 * (sweep + gap);
    assert_eq!(timeline.expected_slot_interval_frames(), expected_interval);
    assert_eq!(
        timeline.slots[1].render_frame_index - timeline.slots[0].render_frame_index,
        expected_interval
    );
    for slot in &timeline.slots {
        assert!(slot.render_frame_index > 0);
        assert_eq!(slot.repeat_render_frames.len(), 3);
        for window in slot.repeat_render_frames.windows(2) {
            assert_eq!(window[1] - window[0], sweep + gap);
        }
    }
    assert_eq!(
        timeline.total_blocks * timeline.block_frames as u64,
        timeline.total_frames
    );
    for segment in &timeline.segments {
        assert_eq!(segment.start_frame % timeline.block_frames as u64, 0);
        assert_eq!(
            segment.start_block_seq * timeline.block_frames as u64,
            segment.start_frame
        );
    }
    assert_eq!(timeline.epoch_id, 1);
    assert_eq!(timeline.graph_id.len(), 64);
}

#[test]
fn public_api_acoustic_defaults_and_gap_rule() {
    let mut acoustic = SweepConfig::acoustic("t61-acoustic", outputs(1));
    acoustic.block_frames = 64;
    acoustic.stimulus = SweepStimulus::log_sweep(100.0, 1_000.0, 0.05, -30.0, 2.0, 1, 0.5);
    acoustic.pre_seconds = 0.2;
    acoustic.post_seconds = 0.2;
    let timeline = compile_timeline(&acoustic).expect("acoustic timeline");
    assert_eq!(timeline.level.drive_level_dbfs, ACOUSTIC_LEVEL_DBFS);
    assert_eq!(timeline.actual_stimulus.repeat_gap_seconds, 0.5);

    let mut too_short = acoustic.clone();
    too_short.stimulus.repeat_gap_seconds = 0.2;
    let error = compile_timeline(&too_short).expect_err("acoustic gap must be >= 0.5 s");
    assert!(error.to_string().contains("repeat_gap_seconds"), "{error}");
}

#[test]
fn public_api_trim_compensation_is_explicit_and_bounded() {
    let mut acoustic = SweepConfig::acoustic("t61-trim", outputs(1));
    acoustic.block_frames = 64;
    acoustic.stimulus = SweepStimulus::log_sweep(100.0, 1_000.0, 0.05, -30.0, 2.0, 1, 0.5);
    acoustic.pre_seconds = 0.2;
    acoustic.post_seconds = 0.2;
    acoustic.k_main_trim_db = -28.0;
    acoustic.trim_policy = TrimPolicy::Compensate;
    let timeline = compile_timeline(&acoustic).expect("legal compensation");
    assert!((timeline.level.effective_level_dbfs - (-2.0)).abs() < 1.0e-12);
    assert!(timeline.level.peak_linear <= 10f64.powf(-1.0 / 20.0));
    let json = timeline.to_json();
    assert!(json.contains("\"trim_policy\":\"compensate\""));
    assert!(json.contains("\"k_main_trim_db\":-28"));

    let mut over = short_config(1, 1);
    over.k_main_trim_db = -28.0;
    over.trim_policy = TrimPolicy::Compensate;
    let error = compile_timeline(&over).expect_err("must not exceed peak ceiling");
    assert!(error.to_string().contains("ceiling"), "{error}");
}

#[test]
fn public_api_validation_rejects_invalid_sessions() {
    let good = short_config(2, 1);
    assert!(good.validate().is_ok());

    let mut bad = good.clone();
    bad.sample_rate = 44_100;
    assert!(bad.validate().is_err());

    let mut bad = good.clone();
    bad.block_frames = 256;
    assert!(bad.validate().is_err());

    let mut bad = good.clone();
    bad.epoch_id = 0;
    assert!(bad.validate().is_err());

    let mut bad = good.clone();
    bad.graph_id = "0".repeat(64);
    assert!(bad.validate().is_err());

    let mut bad = good.clone();
    bad.stimulus.end_hz = 25_000.0;
    assert!(bad.validate().is_err());

    let mut bad = good.clone();
    bad.outputs[1].channel = 0;
    assert!(bad.validate().is_err());
}

#[test]
fn public_api_cancel_and_clip_stop_are_safe_and_reported() {
    struct CancelAfterSink {
        inner: FakeSweepSink,
        cancel: CancelToken,
        after_blocks: u64,
    }
    impl SweepPlaybackSink for CancelAfterSink {
        fn write_block(
            &mut self,
            block: &SweepBlockInfo,
            planar: &[f32],
        ) -> Result<SweepSinkAck, SweepError> {
            let ack = self.inner.write_block(block, planar)?;
            if self.inner.blocks().len() as u64 >= self.after_blocks {
                self.cancel.request();
            }
            Ok(ack)
        }
        fn finish(&mut self) -> Result<SweepSinkAck, SweepError> {
            self.inner.finish()
        }
        fn abort(&mut self, reason: SweepStopReason) -> Result<(), SweepError> {
            self.inner.abort(reason)
        }
    }

    let config = long_config();
    let timeline = compile_timeline(&config).expect("timeline");
    let mut player = SweepPlayer::new(config).expect("player");
    let cancel = CancelToken::new();
    let mut sink = CancelAfterSink {
        inner: FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        ),
        cancel: cancel.clone(),
        after_blocks: 155,
    };
    let error = player.run(&mut sink, &cancel).expect_err("cancel");
    assert!(error.is_cancelled(), "{error}");
    let report = error.report().expect("report");
    assert_eq!(report.stop_block_seq, Some(155));
    assert_eq!(report.cancel_fade_blocks, 2);
    assert_eq!(report.blocks_written, 155 + report.cancel_fade_blocks);
    assert!(report.frames_written < report.planned_frames);
    assert_eq!(sink.inner.aborted, Some(SweepStopReason::Cancelled));
    assert!(sink.inner.blocks().last().unwrap().info.is_drain_tail());
    assert!(error.stage_failure().cancelled);

    let config = long_config();
    let timeline = compile_timeline(&config).expect("timeline");
    let mut player = SweepPlayer::new(config).expect("player");
    let mut sink = FakeSweepSink::new(
        timeline.playback_channels as usize,
        timeline.block_frames as usize,
    );
    sink.inject_clipped_at_block = Some(155);
    let cancel = CancelToken::new();
    let error = player.run(&mut sink, &cancel).expect_err("clip stop");
    assert!(error.is_clip_stopped(), "{error}");
    let report = error.report().expect("report");
    assert_eq!(report.clipped_samples, 1);
    assert_eq!(report.stop_block_seq, Some(156));
    assert_eq!(report.cancel_fade_blocks, 2);
    assert_eq!(sink.aborted, Some(SweepStopReason::ClipDetected));
    let failure = error.stage_failure();
    assert!(!failure.cancelled);
    assert_eq!(failure.code, "E_SWEEP_CLIP");
}

#[test]
fn public_api_per_output_sweep_is_isolated() {
    let mut config = short_config(2, 1);
    config.stimulus.repeats = 1;
    let (report, sink, timeline) = run_complete(config);
    assert!(report.outcome.is_completed());
    let mut sweep_segments = 0;
    for segment in &timeline.segments {
        if segment.kind != SweepSegmentKind::Sweep {
            continue;
        }
        sweep_segments += 1;
        let output = segment.output_index.expect("output") as usize;
        let active = timeline.outputs[output].channel as usize;
        let start = segment.start_frame as usize;
        let end = segment.end_frame() as usize;
        for channel in 0..sink.channels().len() {
            let samples = &sink.channel(channel)[start..end];
            if channel == active {
                assert!(samples.iter().any(|sample| sample.abs() > 0.0));
            } else {
                assert!(samples.iter().all(|sample| *sample == 0.0));
            }
        }
    }
    assert_eq!(sweep_segments, timeline.outputs.len());
}

#[test]
fn public_api_runs_are_deterministic() {
    let config = short_config(2, 1);
    let (report_a, sink_a, _timeline) = run_complete(config.clone());
    let (report_b, sink_b, _timeline_b) = run_complete(config);
    assert_eq!(report_a.peak, report_b.peak);
    for channel in 0..sink_a.channels().len() {
        assert_eq!(sink_a.channel(channel), sink_b.channel(channel));
    }
}

#[test]
fn public_api_render_block_sequence_is_continuous() {
    let config = short_config(2, 1);
    let timeline = compile_timeline(&config).expect("timeline");
    let player = SweepPlayer::new(config).expect("player");
    let mut buffer =
        vec![0.0f32; timeline.playback_channels as usize * timeline.block_frames as usize];
    let block = timeline.block_frames as u64;
    for block_seq in 0..timeline.total_blocks {
        let info = player.render_block(block_seq, &mut buffer).expect("block");
        assert_eq!(info.desc.block_seq, block_seq);
        assert_eq!(info.desc.frame_index, block_seq * block);
        assert_eq!(info.desc.slot_index, (block_seq % 4) as u32);
        assert_eq!(info.desc.valid_frames, timeline.block_frames as u32);
        if block_seq == 0 {
            assert_ne!(info.desc.flags & 1, 0);
            assert_ne!(info.desc.flags & 4, 0);
        }
        if block_seq + 1 == timeline.total_blocks {
            assert_ne!(info.desc.flags & 2, 0);
        }
    }
    let error = player
        .render_block(timeline.total_blocks, &mut buffer)
        .expect_err("out of range");
    assert!(matches!(error, SweepError::InvalidBlockSeq { .. }));
}

#[test]
fn public_api_block_metadata_tracks_segments() {
    let config = short_config(2, 1);
    let (report, sink, timeline) = run_complete(config);
    assert!(report.outcome.is_completed());
    assert_eq!(report.segments_completed, report.total_segments);
    for record in sink.blocks() {
        let segment = &timeline.segments[record.info.segment_index as usize];
        assert_eq!(record.info.segment_kind, segment.kind);
        assert!(record.info.desc.frame_index >= segment.start_frame);
        assert!(record.info.desc.frame_index < segment.end_frame());
        assert_eq!(record.info.output_index, segment.output_index);
        assert_eq!(record.info.repeat, segment.repeat);
    }
}
