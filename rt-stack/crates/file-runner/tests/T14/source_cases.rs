//! host 固定 buffer source 接口（WBS T14：不占 ALSA，按 B×N 块供 T26 播放时轴拉取）。

use tspi_file_runner::runner;
use tspi_file_runner::source::BlockSource;
use tspi_file_runner::wav::{decode_wav, encode_wav_f32};
use tspi_file_runner::{E_FILE_CAPACITY, E_FILE_INTERNAL};

use crate::common::Fixture;

fn decoded(frames: usize) -> tspi_file_runner::wav::DecodedAudio {
    // 2 通道确定性斜坡。
    let mut planar = Vec::with_capacity(2 * frames);
    for frame in 0..frames {
        planar.push(frame as f32 / 64.0);
    }
    for frame in 0..frames {
        planar.push(-(frame as f32) / 128.0);
    }
    let wav = encode_wav_f32(&planar, 2, 48_000).expect("encode");
    decode_wav(&wav).expect("decode")
}

#[test]
fn source_blocks_exact_partition() {
    let input = decoded(100);
    let mut source = BlockSource::new(&input, 64, 0).expect("source");
    assert_eq!(source.channels(), 2);
    assert_eq!(source.block_size(), 64);
    assert_eq!(source.input_frames(), 100);
    assert_eq!(source.flush_zeros(), 0);
    assert_eq!(source.total_frames(), 100);
    assert_eq!(source.blocks_total(), 2);
    let mut block = vec![0.0f32; 2 * 64];
    // 块 0：64 帧全部来自输入。
    let chunk = source.next_block(&mut block).expect("block").expect("some");
    assert_eq!(chunk.block_index, 0);
    assert_eq!(chunk.frame_start, 0);
    assert_eq!(chunk.frames_valid, 64);
    assert_eq!(chunk.input_frames, 64);
    assert_eq!(chunk.flush_frames, 0);
    assert!(!chunk.is_flush);
    assert!(!chunk.is_last);
    for channel in 0..2 {
        for frame in 0..64 {
            assert_eq!(
                block[channel * 64 + frame],
                input.planar[channel * 100 + frame]
            );
        }
    }
    // 块 1：36 帧输入 + 尾部补零。
    let chunk = source.next_block(&mut block).expect("block").expect("some");
    assert_eq!(chunk.block_index, 1);
    assert_eq!(chunk.frames_valid, 36);
    assert_eq!(chunk.input_frames, 36);
    assert_eq!(chunk.flush_frames, 0);
    assert!(!chunk.is_flush);
    assert!(chunk.is_last);
    for channel in 0..2 {
        for frame in 0..64 {
            let expected = if frame < 36 {
                input.planar[channel * 100 + 64 + frame]
            } else {
                0.0
            };
            assert_eq!(block[channel * 64 + frame], expected);
        }
    }
    // EOF。
    assert!(source.is_exhausted());
    assert_eq!(source.blocks_remaining(), 0);
    assert!(source.next_block(&mut block).expect("eof").is_none());
}

#[test]
fn source_flush_zeros_add_tail() {
    let input = decoded(100);
    let mut source = BlockSource::new(&input, 64, 2).expect("source");
    assert_eq!(source.total_frames(), 102);
    assert_eq!(source.blocks_total(), 2);
    let mut block = vec![0.0f32; 128];
    let first = source.next_block(&mut block).expect("b").expect("some");
    assert_eq!(first.input_frames, 64);
    assert!(!first.is_flush);
    let second = source.next_block(&mut block).expect("b").expect("some");
    assert_eq!(second.block_index, 1);
    assert_eq!(second.frames_valid, 38);
    assert_eq!(second.input_frames, 36, "输入只剩 36 帧");
    assert_eq!(second.flush_frames, 2, "flush = L_F-1 = 2");
    assert!(second.is_flush);
    assert!(second.is_last);
    // 后两帧必须是 +0.0。
    assert_eq!(block[36], 0.0);
    assert_eq!(block[37], 0.0);
}

#[test]
fn source_matches_manual_padded_timeline() {
    let frames = 137;
    let input = decoded(frames);
    let flush = 5usize;
    let mut source = BlockSource::new(&input, 64, flush as u32).expect("source");
    let block_len = 2 * 64;
    let mut block = vec![0.0f32; block_len];
    let mut timeline = vec![vec![0.0f32; source.total_frames()]; 2];
    let mut position = 0usize;
    while let Some(chunk) = source.next_block(&mut block).expect("block") {
        for channel in 0..2 {
            for offset in 0..chunk.frames_valid {
                timeline[channel][position + offset] = block[channel * 64 + offset];
            }
        }
        position += chunk.frames_valid;
    }
    assert_eq!(position, source.total_frames());
    for channel in 0..2 {
        for frame in 0..frames {
            assert_eq!(
                timeline[channel][frame],
                input.planar[channel * frames + frame]
            );
        }
        for frame in frames..(frames + flush) {
            assert_eq!(timeline[channel][frame], 0.0);
        }
    }
}

#[test]
fn source_zero_frames_zero_flush_has_no_blocks() {
    let input = decoded(0);
    let mut source = BlockSource::new(&input, 128, 0).expect("source");
    assert_eq!(source.total_frames(), 0);
    assert_eq!(source.blocks_total(), 0);
    assert!(source.is_exhausted());
    let mut block = vec![0.0f32; 2 * 128];
    assert!(source.next_block(&mut block).expect("eof").is_none());
}

#[test]
fn source_zero_frames_with_flush_emits_zero_block() {
    let input = decoded(0);
    let mut source = BlockSource::new(&input, 64, 3).expect("source");
    assert_eq!(source.total_frames(), 3);
    assert_eq!(source.blocks_total(), 1);
    let mut block = vec![1.0f32; 2 * 64];
    let chunk = source.next_block(&mut block).expect("block").expect("some");
    assert_eq!(chunk.frames_valid, 3);
    assert_eq!(chunk.input_frames, 0);
    assert_eq!(chunk.flush_frames, 3);
    assert!(chunk.is_flush);
    assert!(chunk.is_last);
    assert!(block.iter().all(|value| *value == 0.0), "整块被清零");
}

#[test]
fn source_rejects_bad_arguments() {
    let input = decoded(16);
    assert_eq!(
        BlockSource::new(&input, 0, 0)
            .expect_err("zero block")
            .code(),
        E_FILE_INTERNAL
    );
    let mut source = BlockSource::new(&input, 64, 0).expect("source");
    let mut short = vec![0.0f32; 2 * 64 - 1];
    assert_eq!(
        source.next_block(&mut short).expect_err("len").code(),
        E_FILE_CAPACITY
    );
    let mut exact = vec![0.0f32; 2 * 64];
    assert!(source.next_block(&mut exact).expect("ok").is_some());
}

#[test]
fn source_reset_replays_same_blocks() {
    let input = decoded(80);
    let mut source = BlockSource::new(&input, 64, 0).expect("source");
    let mut first = vec![0.0f32; 128];
    let mut second = vec![0.0f32; 128];
    while source.next_block(&mut first).expect("b").is_some() {}
    assert!(source.is_exhausted());
    source.reset();
    assert_eq!(source.blocks_remaining(), source.blocks_total());
    let mut replay = Vec::new();
    while let Some(chunk) = source.next_block(&mut second).expect("b") {
        replay.push(chunk.block_index);
    }
    assert_eq!(replay, vec![0, 1]);
}

#[test]
fn source_from_prepared_matches_executor() {
    let mut fixture = Fixture::identity(200);
    fixture.set_p("linear", &[0.0, 1.0, 0.0]);
    let prepared = fixture.prepare().expect("prepare");
    let source = BlockSource::from_prepared(&prepared).expect("source");
    let graph = prepared.graph.as_ref().expect("graph");
    assert_eq!(source.block_size(), graph.block_size as usize);
    assert_eq!(source.flush_zeros(), graph.flush_zeros as usize);
    assert_eq!(source.total_frames(), 200 + 2);
    assert_eq!(source.blocks_total(), 2);
    // 与 runner 执行的块计数完全一致（同源切分）。
    let result = runner::run(&prepared, &Default::default()).expect("run");
    assert_eq!(result.counts.blocks_total, source.blocks_total());
    // flush 块数 = is_flush 块数。
    let mut again = BlockSource::from_prepared(&prepared).expect("source 2");
    let mut block = vec![0.0f32; source.channels() * source.block_size()];
    let mut flush_blocks = 0u64;
    while let Some(chunk) = again.next_block(&mut block).expect("b") {
        if chunk.is_flush {
            flush_blocks += 1;
        }
    }
    assert_eq!(flush_blocks, result.counts.flush_blocks);
    assert_eq!(flush_blocks, 1);
}
