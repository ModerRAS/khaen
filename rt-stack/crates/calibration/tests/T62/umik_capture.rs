// T62（UMIK 独立 USB ADC 采集）私有测试：raw / repeat / timebase / 设备分离。
//
// 本文件经 crates/calibration/src/capture.rs 末尾的
//   #[cfg(test)] mod t62_tests { include!("../tests/T62/umik_capture.rs"); }
// 编进 lib 单测（cargo 不自动发现 tests/<子目录>；公共注册属 integrator，不要重复注册）。
// 运行：cargo test -p tspi-calibration umik_capture
//
// 离线：不打开 ALSA / 不碰板端；用合成 UMIK/MOTU 采集块验证 raw 解码、repeat onset、
// render frame 与 capture sample 的 clock 拟合（repeat-interval + T19 交叉复核）、
// 以及漂移与公共相位基准不混设备的正/负向规则。

use super::*;

use crate::sweep::{compile_timeline, SweepConfig, SweepOutput, SweepStimulus, SweepTimeline};
use tspi_dsp_core::abi::{sample_format, ABI_VERSION};
use tspi_rt_host::format::{DitherCfg, TspiHwParamsV1};

const FS: u32 = 48_000;
const EPOCH_START_NS: u64 = 1_000_000_000;
const CAPTURE_OFFSET_SAMPLES: f64 = 1234.75;
const T62_CAL_SHA: &str = "abababababababababababababababababababababababababababababababab";
// 合成 cal 文件名（不在测试里加载真实麦校准数据；报告只登记名字+sha256）。
const T62_CAL_FILE: &str = "calibration-spec/mics/t62-synthetic-90deg.txt";

// ---------------------------------------------------------------------------
// 合成时间轴 / 采集流
// ---------------------------------------------------------------------------

fn t62_outputs() -> Vec<SweepOutput> {
    vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)]
}

fn t62_timeline(repeats: u32, seconds: f64) -> SweepTimeline {
    let mut config = SweepConfig::acoustic("t62-session", t62_outputs());
    config.sample_rate = FS;
    config.block_frames = 128;
    config.playback_channels = 8;
    config.stimulus = SweepStimulus::log_sweep(20.0, 20_000.0, seconds, -30.0, 10.0, repeats, 0.5);
    config.pre_seconds = 0.5;
    config.post_seconds = 0.2;
    compile_timeline(&config).expect("t62 timeline")
}

fn t62_grid(timeline: &SweepTimeline) -> RepeatGrid {
    RepeatGrid::from_timeline(timeline).expect("t62 repeat grid")
}

/// onset-fix 回归夹具时间轴：2 s 前置静音（真实采集 seed≈1.19 s + pre 1.5 s 形态），
/// 保证采集开头有足够的噪声窗给 onset 包的稳健噪声底。
fn t62_onset_fix_timeline(repeats: u32) -> SweepTimeline {
    let mut config = SweepConfig::acoustic("t62-session", t62_outputs());
    config.sample_rate = FS;
    config.block_frames = 128;
    config.playback_channels = 8;
    config.stimulus = SweepStimulus::log_sweep(20.0, 20_000.0, 5.0, -30.0, 10.0, repeats, 0.5);
    config.pre_seconds = 2.0;
    config.post_seconds = 0.2;
    compile_timeline(&config).expect("t62 onset-fix timeline")
}

fn t62_umik_device(format: SampleFormat) -> UmikDeviceConfig {
    let mut device = UmikDeviceConfig::umik1(
        "hw:UMIK1,0",
        "718-7696",
        T62_CAL_FILE,
        T62_CAL_SHA,
        UMIK_ORIENTATION_VERTICAL,
    );
    device.sample_format = format;
    device
}

fn t62_umik_config(grid: &RepeatGrid, format: SampleFormat, chunk: u32) -> UmikCaptureConfig {
    let mut config = UmikCaptureConfig::new(grid.session_id.clone(), t62_umik_device(format));
    config.epoch_id = grid.epoch_id;
    config.graph_id = grid.graph_id.clone();
    config.epoch_start_mono_ns = EPOCH_START_NS;
    config.chunk_frames = chunk;
    config
}

fn t62_sweep_sample(grid: &RepeatGrid, t: f64) -> f64 {
    let sweep_frames = grid.sweep_frames as f64;
    if !(t >= 0.0) || t >= sweep_frames {
        return 0.0;
    }
    let fade = grid.fade_frames as f64;
    let duration = sweep_frames / FS as f64;
    let log_ratio = (grid.end_hz / grid.start_hz).ln();
    let time = t / FS as f64;
    let phase = 2.0 * std::f64::consts::PI * grid.start_hz * duration / log_ratio
        * ((time / duration * log_ratio).exp() - 1.0);
    let envelope = if fade > 1.0 && t < fade {
        0.5 * (1.0 - (std::f64::consts::PI * t / (fade - 1.0)).cos())
    } else if fade > 1.0 && t + fade >= sweep_frames {
        let local = t - (sweep_frames - fade);
        0.5 * (1.0 + (std::f64::consts::PI * local / (fade - 1.0)).cos())
    } else {
        1.0
    };
    grid.peak_linear * envelope * phase.sin()
}

/// 合成 UMIK 采集样本：把每个 sweep 段按 capture sample = offset + render_frame x (1+ppm/1e6)
/// 放置（子样本相位用连续 t 求值，模拟 ADC 在自己的采样格上采到延迟波形）。
fn t62_synth_samples(grid: &RepeatGrid, ppm: f64) -> (Vec<f32>, Vec<f64>) {
    let slope = 1.0 + ppm * 1.0e-6;
    let total = ((grid.total_frames as f64 * slope + CAPTURE_OFFSET_SAMPLES).ceil() as usize)
        + FS as usize
        + 2048;
    let mut samples = vec![0.0f32; total];
    let mut onsets = Vec::with_capacity(grid.repeat_count());
    for slot in &grid.slots {
        for render_frame in &slot.repeat_render_frames {
            let start = CAPTURE_OFFSET_SAMPLES + *render_frame as f64 * slope;
            onsets.push(start);
            let first = start.floor() as i64;
            for k in 0..grid.sweep_frames as i64 {
                let index = first + k;
                if index < 0 || index as usize >= total {
                    continue;
                }
                let t = index as f64 - start;
                if t < 0.0 {
                    continue;
                }
                samples[index as usize] += t62_sweep_sample(grid, t) as f32;
            }
        }
    }
    (samples, onsets)
}

fn t62_encode_umik(
    grid: &RepeatGrid,
    samples: &[f32],
    format: SampleFormat,
    chunk: u32,
) -> CapturedStream {
    let mut capture = UmikCapture::new(t62_umik_config(grid, format, chunk)).expect("UMIK capture");
    let mut frame = 0usize;
    while frame < samples.len() {
        let count = (samples.len() - frame).min(chunk as usize);
        let mut bytes = Vec::new();
        encode_mono_samples(format, &samples[frame..frame + count], &mut bytes);
        let mono_ns = EPOCH_START_NS + (frame as u128 * 1_000_000_000u128 / FS as u128) as u64;
        capture
            .push_block(mono_ns, &bytes)
            .expect("push UMIK block");
        frame += count;
    }
    capture.finish().expect("finish UMIK capture")
}

fn t62_synth_umik(
    grid: &RepeatGrid,
    ppm: f64,
    format: SampleFormat,
    chunk: u32,
) -> (CapturedStream, Vec<f64>) {
    let (samples, onsets) = t62_synth_samples(grid, ppm);
    (t62_encode_umik(grid, &samples, format, chunk), onsets)
}

fn t62_hw_format(hw_format: u16, n_in: u16, n_out: u16, period_frames: u32) -> FormatConfig {
    let bytes = match hw_format {
        sample_format::S24_3LE_INTERLEAVED => 3u32,
        sample_format::S32_LE_INTERLEAVED => 4,
        sample_format::F32_INTERLEAVED => 4,
        _ => 3,
    };
    let period = period_frames;
    let in_min = period * n_in as u32 * bytes;
    let out_min = period * n_out as u32 * bytes;
    FormatConfig {
        hw: TspiHwParamsV1 {
            abi_version: ABI_VERSION,
            sample_format: hw_format,
            n_in,
            n_out,
            sample_rate: FS,
            period_frames: period,
            buffer_frames: period * 4,
            in_stride_bytes: in_min,
            out_stride_bytes: out_min,
            flags: 0,
            reserved: [0; 4],
        },
        dither: DitherCfg::Off,
    }
}

fn t62_reference_channel() -> ElectricalReferenceChannel {
    ElectricalReferenceChannel::new("hw:motu,0", 0, SampleFormat::S24_3LE)
}

fn t62_synth_reference(
    grid: &RepeatGrid,
    ppm: f64,
    n_in: u16,
    period_frames: u32,
) -> (CapturedStream, Vec<f64>) {
    let (samples, onsets) = t62_synth_samples(grid, ppm);
    let channels = n_in as usize;
    let bytes_per_sample = 3usize;
    let mut interleaved = Vec::with_capacity(samples.len() * channels * bytes_per_sample);
    for sample in &samples {
        let value = *sample as f64 * 8_388_608.0;
        let quantized = if value >= 8_388_607.0 {
            8_388_607i32
        } else if value <= -8_388_608.0 {
            -8_388_608
        } else {
            value.round() as i32
        };
        let raw = quantized.to_le_bytes();
        interleaved.extend_from_slice(&raw[0..3]);
        for _ in 1..channels {
            interleaved.extend_from_slice(&[0u8, 0u8, 0u8]);
        }
    }
    let format = t62_hw_format(sample_format::S24_3LE_INTERLEAVED, n_in, 2, period_frames);
    let mut config =
        ElectricalReferenceConfig::new(t62_reference_channel(), format).expect("reference config");
    config.session_id = grid.session_id.clone();
    config.epoch_id = grid.epoch_id;
    config.graph_id = grid.graph_id.clone();
    config.epoch_start_mono_ns = EPOCH_START_NS;
    let mut capture = ElectricalReferenceCapture::new(config).expect("reference capture");
    let block_bytes = period_frames as usize * channels * bytes_per_sample;
    let mut frame = 0usize;
    while frame < samples.len() {
        let count = (samples.len() - frame).min(period_frames as usize);
        let start = frame * channels * bytes_per_sample;
        let end = start + count * channels * bytes_per_sample;
        // 双工设备一个 period 是固定字节数；最后一个不足 period 的块用静音补齐。
        let mut block = vec![0u8; block_bytes];
        block[..end - start].copy_from_slice(&interleaved[start..end]);
        let mono_ns = EPOCH_START_NS + (frame as u128 * 1_000_000_000u128 / FS as u128) as u64;
        capture
            .push_block(mono_ns, &block)
            .expect("push reference block");
        frame += count;
    }
    (capture.finish().expect("finish reference capture"), onsets)
}

// ---------------------------------------------------------------------------
// 1. 配置/设备纪律
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_config_rejects_bad_device_declarations() {
    let good = t62_umik_device(SampleFormat::S24_3LE);
    assert!(good.validate().is_ok());
    assert!(good.is_vertical_orientation());

    let mut stereo = good.clone();
    stereo.channels = 2;
    assert_eq!(stereo.validate().unwrap_err().code(), "E_CAPTURE_CHANNELS");

    let mut rate = good.clone();
    rate.sample_rate_hz = 96_000;
    assert_eq!(rate.validate().unwrap_err().code(), "E_CAPTURE_RATE");

    let mut domain = good.clone();
    domain.clock_domain = "motu_shared".to_string();
    assert_eq!(domain.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");

    let mut cal = good.clone();
    cal.cal_sha256 = "0".repeat(64);
    assert_eq!(cal.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");

    // 报告要求的 cal_file 必填（空/未 trim/控制字符全部拒绝）。
    for bad_cal_file in ["", "   ", " mics/a.txt", "mics/a\tbad.txt"] {
        let mut missing_cal_file = good.clone();
        missing_cal_file.cal_file = bad_cal_file.to_string();
        assert_eq!(
            missing_cal_file.validate().unwrap_err().code(),
            "E_CAPTURE_CONFIG",
            "cal_file {bad_cal_file:?} must be rejected"
        );
    }
    let mut too_long = good.clone();
    too_long.cal_file = "a".repeat(257);
    assert_eq!(too_long.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");

    let mut orientation = good.clone();
    orientation.orientation = String::new();
    assert_eq!(
        orientation.validate().unwrap_err().code(),
        "E_CAPTURE_CONFIG"
    );

    // 非 UMIK-1 的兼容麦克风可声明 96k，但仍受 runtime {48k,96k} 限制。
    let mut other = good.clone();
    other.model = "OTHER-96K".to_string();
    other.sample_rate_hz = 96_000;
    assert!(other.validate().is_ok());
    other.sample_rate_hz = 44_100;
    assert_eq!(other.validate().unwrap_err().code(), "E_CAPTURE_RATE");
}

#[test]
fn umik_capture_config_requires_session_epoch_graph_and_chunk_bounds() {
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let mut config = UmikCaptureConfig::new("t62-session", device);
    config.graph_id = "0".repeat(64);
    assert_eq!(config.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");
    config.graph_id = PLACEHOLDER_GRAPH_ID.to_string();
    config.epoch_id = 0;
    assert_eq!(config.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");
    config.epoch_id = 1;
    config.chunk_frames = 0;
    assert_eq!(config.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");
    config.chunk_frames = MAX_CHUNK_FRAMES + 1;
    assert_eq!(config.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");
    config.chunk_frames = 128;
    config.expected_total_frames = Some(0);
    assert_eq!(config.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");
}

// ---------------------------------------------------------------------------
// 2. raw 解码 / block 纪律 / 峰值削波
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_decodes_s24_s32_f32_raw_with_t22_semantics() {
    let values = [0.0f32, 0.5, -0.5, 0.25, -0.0, 0.125];
    let device = t62_umik_device(SampleFormat::S24_3LE);
    for (format, tolerance) in [
        (SampleFormat::S24_3LE, 2.0e-7f32),
        (SampleFormat::S32LE, 1.0e-9f32),
        (SampleFormat::F32LE, 0.0f32),
    ] {
        let mut config = UmikCaptureConfig::new("t62-decode", device.clone());
        config.device.sample_format = format;
        config.chunk_frames = 64;
        let mut capture = UmikCapture::new(config).expect("capture");
        let mut bytes = Vec::new();
        encode_mono_samples(format, &values, &mut bytes);
        assert_eq!(
            bytes.len(),
            values.len() * format.bytes_per_sample() as usize
        );
        capture
            .push_block(EPOCH_START_NS, &bytes)
            .expect("push decode block");
        let stream = capture.finish().expect("finish decode capture");
        assert_eq!(stream.frames, values.len() as u64);
        assert_eq!(stream.raw, bytes);
        for (decoded, expected) in stream.samples.iter().zip(values.iter()) {
            assert!(
                (decoded - expected).abs() <= tolerance,
                "format {:?}: {decoded} != {expected}",
                format
            );
        }
        assert_eq!(stream.clipped_samples, 0);
        assert_eq!(stream.nonfinite_samples, 0);
    }
}

#[test]
fn umik_capture_block_discipline_rejects_bad_bytes_stamps_and_overrun() {
    let mut capture = UmikCapture::new(t62_umik_config_for_decode()).expect("capture");
    assert_eq!(
        capture.push_block(EPOCH_START_NS, &[]).unwrap_err().code(),
        "E_CAPTURE_BLOCK"
    );
    let bad_len = [0u8; 4]; // S24: 4 不是 3 的倍数
    assert_eq!(
        capture
            .push_block(EPOCH_START_NS, &bad_len)
            .unwrap_err()
            .code(),
        "E_CAPTURE_BLOCK"
    );
    let mut good = Vec::new();
    encode_mono_samples(SampleFormat::S24_3LE, &[0.1f32, 0.2, 0.3], &mut good);
    capture
        .push_block(EPOCH_START_NS, &good)
        .expect("first block");
    assert_eq!(
        capture
            .push_block(EPOCH_START_NS, &good)
            .unwrap_err()
            .code(),
        "E_CAPTURE_STAMP"
    );

    let mut limited = UmikCaptureConfig::new("t62-limited", t62_umik_device(SampleFormat::S24_3LE));
    limited.chunk_frames = 64;
    limited.expected_total_frames = Some(3);
    let mut capture = UmikCapture::new(limited).expect("limited capture");
    let mut six = Vec::new();
    encode_mono_samples(SampleFormat::S24_3LE, &[0.1f32; 6], &mut six);
    assert_eq!(
        capture.push_block(EPOCH_START_NS, &six).unwrap_err().code(),
        "E_CAPTURE_TOO_MANY_FRAMES"
    );

    let empty = UmikCapture::new(t62_umik_config_for_decode()).expect("empty capture");
    assert_eq!(empty.finish().unwrap_err().code(), "E_CAPTURE_BLOCK");
}

fn t62_umik_config_for_decode() -> UmikCaptureConfig {
    let mut config = UmikCaptureConfig::new("t62-decode", t62_umik_device(SampleFormat::S24_3LE));
    config.chunk_frames = 64;
    config
}

#[test]
fn umik_capture_peak_and_clip_gate_follow_acceptance_clip_rule() {
    let mut config = UmikCaptureConfig::new("t62-clip", t62_umik_device(SampleFormat::F32LE));
    config.chunk_frames = 128;
    let mut capture = UmikCapture::new(config).expect("clip capture");
    let values = [2.0f32, f32::NAN, -3.0, 0.1];
    let mut bytes = Vec::new();
    encode_mono_samples(SampleFormat::F32LE, &values, &mut bytes);
    capture
        .push_block(EPOCH_START_NS, &bytes)
        .expect("push clip block");
    let stream = capture.finish().expect("finish clip capture");
    assert_eq!(stream.clipped_samples, 2);
    assert_eq!(stream.nonfinite_samples, 1);
    assert_eq!(stream.samples[1], 0.0);
    assert!((stream.capture_peak_dbfs() - 0.0).abs() < 1.0e-9);
    let summary = stream.summary();
    assert!(!summary.peak_gate_pass);
    assert_eq!(summary.peak_ceiling_dbfs, CAPTURE_PEAK_CEILING_DBFS);

    let silence = CapturedStream {
        peak_abs: 0.0,
        ..stream.clone()
    };
    let silence_summary = silence.summary();
    assert!(!silence_summary.peak_gate_pass);
    assert_eq!(silence_summary.capture_peak_dbfs, SILENCE_DBFS);
}

#[test]
fn umik_capture_capture_file_is_sha256_bound_and_path_checked() {
    let mut config = UmikCaptureConfig::new("t62-file", t62_umik_device(SampleFormat::S24_3LE));
    config.chunk_frames = 64;
    let mut capture = UmikCapture::new(config).expect("file capture");
    let mut bytes = Vec::new();
    encode_mono_samples(SampleFormat::S24_3LE, &[0.1f32; 32], &mut bytes);
    capture
        .push_block(EPOCH_START_NS, &bytes)
        .expect("push file block");
    let stream = capture.finish().expect("finish file capture");
    let file = stream.capture_file("capture.raw").expect("capture file");
    assert_eq!(file.relative_path, "capture.raw");
    assert_eq!(file.bytes, bytes.len() as u64);
    assert_eq!(file.frames, 32);
    assert_eq!(file.channels, 1);
    assert_eq!(file.sample_rate_hz, FS);
    assert_eq!(file.sample_format, SampleFormat::S24_3LE);
    assert_eq!(file.sha256.len(), 64);
    assert_eq!(file.sha256, sha256_hex(&stream.raw));
    for bad in ["/abs/capture.raw", "../capture.raw", "a//capture.raw", ""] {
        assert_eq!(
            stream.capture_file(bad).unwrap_err().code(),
            "E_CAPTURE_CONFIG",
            "path {bad:?} must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. repeat grid（T61 timeline 消费）
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_repeat_grid_matches_t61_timeline_contract() {
    let timeline = t62_timeline(3, 0.5);
    let grid = t62_grid(&timeline);
    assert_eq!(grid.repeats, 3);
    assert_eq!(grid.slots.len(), 2);
    assert_eq!(grid.repeat_count(), 6);
    assert_eq!(grid.sample_rate_hz, FS);
    assert_eq!(grid.block_frames, 128);
    assert_eq!(grid.sweep_frames, timeline.actual_stimulus.sweep_frames);
    assert_eq!(grid.gap_frames, timeline.actual_stimulus.gap_frames);
    assert_eq!(grid.first_render_frame(), Some(timeline.pre_frames));
    for (slot, timeline_slot) in grid.slots.iter().zip(timeline.slots.iter()) {
        assert_eq!(slot.container_slot, timeline_slot.slot);
        assert_eq!(slot.render_frame_index, timeline_slot.render_frame_index);
        assert_eq!(
            slot.repeat_render_frames,
            timeline_slot.repeat_render_frames
        );
        assert_eq!(slot.repeat_render_frames.len(), 3);
    }

    let mut bad = t62_timeline(3, 0.5);
    bad.block_frames = 256;
    assert_eq!(
        RepeatGrid::from_timeline(&bad).unwrap_err().code(),
        "E_CAPTURE_TIMELINE"
    );
    let mut bad = t62_timeline(3, 0.5);
    bad.stimulus.repeats = 0;
    assert_eq!(
        RepeatGrid::from_timeline(&bad).unwrap_err().code(),
        "E_CAPTURE_TIMELINE"
    );
}

// ---------------------------------------------------------------------------
// 4. onset / timebase
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_timebase_recovers_injected_ppm_and_t19_cross_check() {
    let timeline = t62_timeline(3, 5.0);
    let grid = t62_grid(&timeline);
    let injected_ppm = 12.84;
    let (stream, true_onsets) = t62_synth_umik(&grid, injected_ppm, SampleFormat::S24_3LE, 1024);
    assert_eq!(true_onsets.len(), grid.repeat_count());

    let timebase = estimate_capture_timebase(&stream, &grid, &OnsetConfig::default())
        .expect("estimate timebase");
    assert_eq!(timebase.onsets.len(), grid.repeat_count());
    assert!(timebase.all_slots_valid);
    assert_eq!(timebase.onsets_invalid_slots, 0);
    for (onset, truth) in timebase.onsets.iter().zip(true_onsets.iter()) {
        // 阈值插值法有固定系统偏置（已用同参数解析模板扣除）；扣偏置后应接近真实起点。
        assert!(
            (onset.onset_capture_sample - truth).abs() < 8.0,
            "onset {} vs truth {}",
            onset.onset_capture_sample,
            truth
        );
    }

    let ppm = timebase.clock.clock_ppm.expect("ppm");
    assert!(
        (ppm - injected_ppm).abs() <= 2.0,
        "recovered ppm {ppm} vs injected {injected_ppm}"
    );
    let sigma = timebase
        .clock
        .clock_ppm_uncertainty_ppm
        .expect("uncertainty");
    assert!(
        sigma >= PPM_SIGMA_FLOOR_PPM && sigma <= 5.0,
        "sigma {sigma}"
    );
    assert!(timebase.clock.clock_ppm_ok);
    assert!(timebase.clock.drift_significant);
    assert_eq!(timebase.clock.correction, DriftCorrection::Resample);
    let ratio = timebase.clock.samples_per_frame.expect("ratio");
    assert!((ratio - (1.0 + injected_ppm * 1.0e-6)).abs() < 2.0e-6);
    assert!(timebase.clock.residual_rms_samples < ONSET_RESIDUAL_MAX_SAMPLES);
    assert!(timebase.clock.span_seconds > 10.0);

    let mic_clock = timebase.mic_clock().expect("mic clock");
    assert_eq!(mic_clock.clock_ppm, ppm);
    assert_eq!(mic_clock.clock_ppm_ok, true);
    assert_eq!(mic_clock.correction, DriftCorrection::Resample);

    let cross = timebase
        .clock
        .t19_cross_check
        .expect("T19 PpmEstimator cross check");
    assert!(cross.frame_valid);
    assert!(cross.frame_ppm_ppb > 0);
    assert!(
        (cross.frame_ppm_ppb as f64 - injected_ppm * 1000.0).abs() <= 20_000.0,
        "T19 cross check {} ppb vs injected {} ppm",
        cross.frame_ppm_ppb,
        injected_ppm
    );
}

#[test]
fn umik_capture_zero_ppm_is_not_significant_and_claim_stays_honest() {
    let timeline = t62_timeline(3, 5.0);
    let grid = t62_grid(&timeline);
    let (stream, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 1024);
    let timebase =
        estimate_capture_timebase(&stream, &grid, &OnsetConfig::default()).expect("timebase");
    let ppm = timebase.clock.clock_ppm.expect("ppm");
    assert!(ppm.abs() <= 1.0, "zero-ppm estimate {ppm}");
    assert!(timebase.clock.clock_ppm_ok);
    assert!(!timebase.clock.drift_significant);
    assert_eq!(timebase.clock.correction, DriftCorrection::None);
    let claim = timebase.clock.residual_phase_claim;
    assert_eq!(claim.span_seconds, timebase.clock.span_seconds);
    assert_eq!(claim.required_sigma_ppm, RESIDUAL_PHASE_SIGMA_MAX_PPM);
    // 5 s x 3 遍 x 2 槽 => 跨度 > 10.4 s；干净数据 sigma 应在 0.4 ppm 内。
    assert!(
        claim.span_seconds >= RESIDUAL_PHASE_DEFAULT_SPAN_SECONDS,
        "span {}",
        claim.span_seconds
    );
    assert_eq!(claim.criterion, ResidualPhaseCriterion::DefaultSpan10Point4);
    let sigma = timebase
        .clock
        .clock_ppm_uncertainty_ppm
        .expect("uncertainty");
    assert_eq!(
        claim.applicable,
        timebase.clock.clock_ppm_ok
            && sigma <= RESIDUAL_PHASE_SIGMA_MAX_PPM
            && claim.span_seconds >= RESIDUAL_PHASE_DEFAULT_SPAN_SECONDS
    );
}

#[test]
fn umik_capture_single_repeat_is_not_estimable_and_never_resampled() {
    let timeline = t62_timeline(1, 0.5);
    let grid = t62_grid(&timeline);
    let (stream, _) = t62_synth_umik(&grid, 25.0, SampleFormat::S24_3LE, 256);
    let timebase =
        estimate_capture_timebase(&stream, &grid, &OnsetConfig::default()).expect("timebase");
    assert_eq!(timebase.onsets.len(), 2);
    assert_eq!(timebase.clock.intervals, 0);
    assert!(timebase.clock.clock_ppm.is_none());
    assert!(timebase.clock.clock_ppm_uncertainty_ppm.is_none());
    assert!(!timebase.clock.clock_ppm_ok);
    assert!(!timebase.clock.drift_significant);
    assert_eq!(timebase.clock.correction, DriftCorrection::None);
    assert!(timebase.mic_clock().is_none());
    assert_eq!(
        timebase.timebase_diags("capture.raw").unwrap_err().code(),
        "E_CAPTURE_CLOCK"
    );
}

#[test]
fn umik_capture_two_repeats_reports_uncertainty_but_cannot_prove_drift() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let (stream, _) = t62_synth_umik(&grid, 1.0, SampleFormat::S24_3LE, 256);
    let timebase =
        estimate_capture_timebase(&stream, &grid, &OnsetConfig::default()).expect("timebase");
    assert_eq!(timebase.clock.intervals, 2);
    let ppm = timebase.clock.clock_ppm.expect("ppm point estimate");
    assert!(ppm.abs() <= 10.0, "ppm {ppm}");
    assert!(timebase.clock.clock_ppm_uncertainty_ppm.is_some());
    // 2 个间隔时 sigma 通常 > |ppm|/2，因此显著性必须按 2 sigma 判 false（不硬造漂移）。
    if let Some(sigma) = timebase.clock.clock_ppm_uncertainty_ppm {
        assert_eq!(
            timebase.clock.drift_significant,
            ppm.abs() > DRIFT_SIGNIFICANCE_SIGMA * sigma
        );
    }
    assert!(timebase.clock.clock_ppm_ok);
}

#[test]
fn umik_capture_silence_and_missing_repeat_are_reported_not_hidden() {
    let timeline = t62_timeline(3, 0.5);
    let grid = t62_grid(&timeline);

    let silence = vec![0.0f32; grid.total_frames as usize + FS as usize];
    let silent_stream = t62_encode_umik(&grid, &silence, SampleFormat::S24_3LE, 512);
    assert_eq!(
        estimate_capture_timebase(&silent_stream, &grid, &OnsetConfig::default())
            .unwrap_err()
            .code(),
        "E_CAPTURE_NO_ONSETS"
    );

    let (mut samples, onsets) = t62_synth_samples(&grid, 0.0);
    let missing = onsets[1];
    let start = (missing.floor() as i64 - 256).max(0) as usize;
    let end = ((missing + grid.sweep_frames as f64 + 256.0) as usize).min(samples.len());
    for sample in &mut samples[start..end] {
        *sample = 0.0;
    }
    let stream = t62_encode_umik(&grid, &samples, SampleFormat::S24_3LE, 512);
    let timebase =
        estimate_capture_timebase(&stream, &grid, &OnsetConfig::default()).expect("timebase");
    assert_eq!(timebase.onsets.len(), grid.repeat_count() - 1);
    assert_eq!(timebase.onsets_invalid_slots, 1);
    assert!(!timebase.all_slots_valid);
    assert!(timebase.slots[0].onset_invalid);
    assert!(!timebase.slots[1].onset_invalid);
    assert_eq!(timebase.slots[0].repeat_starts.len(), 2);
}

#[test]
fn umik_capture_rejects_stream_timeline_identity_mismatch() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let (stream, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 256);
    let mut wrong_session = stream.clone();
    wrong_session.session_id = "other-session".to_string();
    assert_eq!(
        estimate_capture_timebase(&wrong_session, &grid, &OnsetConfig::default())
            .unwrap_err()
            .code(),
        "E_CAPTURE_TIMELINE"
    );
    let mut wrong_epoch = stream.clone();
    wrong_epoch.epoch_id = 99;
    assert_eq!(
        estimate_capture_timebase(&wrong_epoch, &grid, &OnsetConfig::default())
            .unwrap_err()
            .code(),
        "E_CAPTURE_TIMELINE"
    );
    let mut wrong_rate = stream;
    wrong_rate.sample_rate_hz = 96_000;
    assert_eq!(
        estimate_capture_timebase(&wrong_rate, &grid, &OnsetConfig::default())
            .unwrap_err()
            .code(),
        "E_CAPTURE_RATE"
    );
}

// ---------------------------------------------------------------------------
// 5. 设备分离规则（漂移与公共相位基准不混设备）
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_reference_plan_rejects_mixed_devices() {
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let electrical = t62_reference_channel();

    let acoustic = ReferencePlan::acoustic(&device, Some(electrical.clone())).expect("acoustic");
    assert_eq!(acoustic.primary_role(), CaptureStreamRole::UmikAdc);
    assert_eq!(acoustic.drift_source, TimebaseDriftSource::UmikRepeatGrid);
    assert_eq!(
        acoustic.phase_reference,
        CommonPhaseReference::UmikDirectOnset
    );

    let mut mixed_phase = acoustic.clone();
    mixed_phase.phase_reference = CommonPhaseReference::MotuElectricalLoopback;
    assert_eq!(
        mixed_phase.validate().unwrap_err().code(),
        "E_CAPTURE_REF_MIXED_DEVICES"
    );

    let mut mixed_drift = acoustic.clone();
    mixed_drift.drift_source = TimebaseDriftSource::MotuElectricalLoopback;
    assert_eq!(
        mixed_drift.validate().unwrap_err().code(),
        "E_CAPTURE_REF_MIXED_DEVICES"
    );

    let mut same_device = device.clone();
    same_device.device = electrical.device.clone();
    let not_independent = ReferencePlan::acoustic(&same_device, Some(electrical.clone()));
    assert_eq!(
        not_independent.unwrap_err().code(),
        "E_CAPTURE_REF_NOT_INDEPENDENT"
    );

    let loopback = ReferencePlan::electrical_loopback(electrical.clone()).expect("loopback");
    assert_eq!(loopback.primary_role(), CaptureStreamRole::MotuElectrical);

    let mut loopback_with_umik = loopback.clone();
    loopback_with_umik.umik_device = Some(device.device.clone());
    assert_eq!(
        loopback_with_umik.validate().unwrap_err().code(),
        "E_CAPTURE_REF_MIXED_DEVICES"
    );

    let missing_electrical = ReferencePlan {
        purpose: CapturePurpose::ElectricalLoopback,
        umik_device: None,
        electrical: None,
        drift_source: TimebaseDriftSource::MotuElectricalLoopback,
        phase_reference: CommonPhaseReference::MotuElectricalLoopback,
    };
    assert_eq!(
        missing_electrical.validate().unwrap_err().code(),
        "E_CAPTURE_CONFIG"
    );
}

#[test]
fn umik_capture_finalize_acoustic_keeps_aux_reference_isolated() {
    let timeline = t62_timeline(3, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let electrical = t62_reference_channel();
    let plan = ReferencePlan::acoustic(&device, Some(electrical)).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 512);
    let (auxiliary, _) = t62_synth_reference(&grid, 0.0, 2, 128);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "capture.raw",
        Some(auxiliary),
    )
    .expect("finalize acoustic");
    assert_eq!(report.purpose, CapturePurpose::AcousticRoom);
    assert_eq!(report.stream.role, CaptureStreamRole::UmikAdc);
    assert_eq!(report.plan.primary_role(), CaptureStreamRole::UmikAdc);
    let mic = report.stream.mic.as_ref().expect("mic report");
    assert_eq!(mic.info.serial, "718-7696");
    assert_eq!(mic.info.orientation, UMIK_ORIENTATION_VERTICAL);
    assert_eq!(mic.cal_file, T62_CAL_FILE);
    assert_eq!(mic.info.cal_sha256, T62_CAL_SHA);
    let auxiliary_summary = report.auxiliary_reference.as_ref().expect("aux summary");
    assert_eq!(auxiliary_summary.role, CaptureStreamRole::MotuElectrical);
    assert_eq!(auxiliary_summary.device, "hw:motu,0");
    assert_ne!(auxiliary_summary.raw_sha256, report.stream.raw_sha256);
    // UMIK 是唯一 drift/phase 基准；auxiliary 不改变 plan。
    assert_eq!(
        report.plan.phase_reference,
        CommonPhaseReference::UmikDirectOnset
    );
    assert_eq!(report.capture_file.relative_path, "capture.raw");
    assert!(report.stream.noise_floor_dbfs.expect("noise floor") <= -100.0);
    let diags = report.timebase_diags().expect("timebase diags");
    assert_eq!(diags.len(), grid.slots.len());
}

#[test]
fn umik_capture_finalize_rejects_same_device_aux_and_undeclared_aux() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let electrical = t62_reference_channel();
    let plan = ReferencePlan::acoustic(&device, Some(electrical)).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 512);
    let (auxiliary, _) = t62_synth_reference(&grid, 0.0, 2, 128);
    let mut same_device_aux = auxiliary.clone();
    same_device_aux.device = device.device.clone();
    assert_eq!(
        finalize_acoustic_capture(
            umik.clone(),
            &device,
            &grid,
            &plan,
            &OnsetConfig::default(),
            "capture.raw",
            Some(same_device_aux)
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_REF_NOT_INDEPENDENT"
    );

    let plan_without_electrical = ReferencePlan::acoustic(&device, None).expect("plan");
    assert_eq!(
        finalize_acoustic_capture(
            umik,
            &device,
            &grid,
            &plan_without_electrical,
            &OnsetConfig::default(),
            "capture.raw",
            Some(auxiliary)
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_CONFIG"
    );
}

#[test]
fn umik_capture_electrical_loopback_uses_motu_domain_only() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let (motu, _) = t62_synth_reference(&grid, 0.0, 2, 128);
    let plan = ReferencePlan::electrical_loopback(t62_reference_channel()).expect("loopback plan");
    let report =
        finalize_electrical_loopback(motu, &grid, &plan, &OnsetConfig::default(), "reference.raw")
            .expect("finalize loopback");
    assert_eq!(report.purpose, CapturePurpose::ElectricalLoopback);
    assert_eq!(report.stream.role, CaptureStreamRole::MotuElectrical);
    assert_eq!(report.stream.reference_channel, Some(0));
    assert_eq!(report.timebase.device, "hw:motu,0");
    assert_eq!(report.capture_file.relative_path, "reference.raw");
    let diags = report.timebase_diags().expect("loopback diags");
    assert_eq!(diags.len(), grid.slots.len());
    assert_eq!(diags[0].capture_file, "reference.raw");

    let (mut motu_stream, _) = t62_synth_reference(&grid, 0.0, 2, 128);
    motu_stream.reference_channel = Some(1);
    assert_eq!(
        finalize_electrical_loopback(
            motu_stream,
            &grid,
            &plan,
            &OnsetConfig::default(),
            "reference.raw"
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_CONFIG"
    );
}

#[test]
fn umik_capture_finalize_rejects_wrong_purpose_or_stream_role() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let acoustic_plan = ReferencePlan::acoustic(&device, None).expect("acoustic plan");
    let loopback_plan =
        ReferencePlan::electrical_loopback(t62_reference_channel()).expect("loopback plan");
    let (umik, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 512);
    let (motu, _) = t62_synth_reference(&grid, 0.0, 2, 128);

    assert_eq!(
        finalize_acoustic_capture(
            motu,
            &device,
            &grid,
            &acoustic_plan,
            &OnsetConfig::default(),
            "capture.raw",
            None
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_REF_MIXED_DEVICES"
    );
    assert_eq!(
        finalize_electrical_loopback(
            umik.clone(),
            &grid,
            &loopback_plan,
            &OnsetConfig::default(),
            "reference.raw"
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_REF_MIXED_DEVICES"
    );
    assert_eq!(
        finalize_acoustic_capture(
            umik,
            &device,
            &grid,
            &loopback_plan,
            &OnsetConfig::default(),
            "capture.raw",
            None
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_REF_MIXED_DEVICES"
    );
}

// ---------------------------------------------------------------------------
// 6. T48 diag / 报告 JSON / 峰值噪声底
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_timebase_diags_match_t48_ingest_schema() {
    let timeline = t62_timeline(3, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let plan = ReferencePlan::acoustic(&device, None).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 5.0, SampleFormat::S24_3LE, 512);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "capture.raw",
        None,
    )
    .expect("finalize");
    let diags = report.timebase_diags().expect("diags");
    assert_eq!(diags.len(), 2);
    let clock = report.timebase.mic_clock().expect("mic clock");
    for (index, diag) in diags.iter().enumerate() {
        let slot = &report.timebase.slots[index];
        assert_eq!(diag.session_id, grid.session_id);
        assert_eq!(diag.epoch_id, grid.epoch_id);
        assert_eq!(diag.graph_id, grid.graph_id);
        assert_eq!(diag.slot, slot.session_slot);
        assert_eq!(diag.speaker, slot.speaker);
        assert_eq!(diag.channel, slot.channel);
        assert_eq!(diag.capture_file, "capture.raw");
        assert_eq!(diag.render_frame_index, slot.render_frame_index);
        assert_eq!(diag.repeats, 3);
        assert_eq!(diag.repeat_starts.len(), 3);
        assert_eq!(diag.clock, clock);
        assert!(!diag.onset_invalid);
        assert!(diag.actual_start > 0.0);
        assert!(diag.nominal_start > 0.0);
        assert!(diag.lag_samples.is_finite());
    }
}

#[test]
fn umik_capture_report_json_is_deterministic_and_contract_shaped() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let plan = ReferencePlan::acoustic(&device, None).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 3.0, SampleFormat::S24_3LE, 512);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "capture.raw",
        None,
    )
    .expect("finalize");
    let json = report.to_json();
    assert_eq!(json, report.to_json());
    assert!(json.contains("\"schema\":\"tspi-umik-capture/v1\""));
    assert!(json.contains("\"purpose\":\"acoustic\""));
    assert!(json.contains("\"drift_source\":\"umik_repeat_grid\""));
    assert!(json.contains("\"phase_reference\":\"umik_direct_onset\""));
    assert!(json.contains("\"serial\":\"718-7696\""));
    assert!(json.contains("\"schema\":\"tspi-umik-timebase/v1\""));
    assert!(json.contains("\"clock_ppm\":"));
    assert!(json.contains("\"drift_significant\":"));
    assert!(json.contains("\"residual_phase_claim\":"));
    assert!(json.contains("\"raw_sha256\":"));
    let timebase_json = report.timebase.to_json();
    assert_eq!(timebase_json, report.timebase.to_json());
    let parsed_shape = timebase_json.starts_with('{') && timebase_json.ends_with('}');
    assert!(parsed_shape);
}

#[test]
fn umik_capture_report_records_cal_file() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let plan = ReferencePlan::acoustic(&device, None).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 512);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "capture.raw",
        None,
    )
    .expect("finalize");
    let mic = report.stream.mic.as_ref().expect("mic report");
    assert_eq!(mic.cal_file, T62_CAL_FILE);
    assert_eq!(mic.info.cal_sha256, T62_CAL_SHA);
    let json = report.to_json();
    // ACCEPTANCE §0.1.2：报告 mic{model, serial, cal_file, sha256, orientation} 五字段齐全。
    for expected in [
        "\"model\":\"UMIK-1\"",
        "\"serial\":\"718-7696\"",
        "\"cal_file\":\"calibration-spec/mics/t62-synthetic-90deg.txt\"",
        "\"cal_sha256\":\"abababababababababababababababababababababababababababababababab\"",
        "\"orientation\":\"90deg\"",
    ] {
        assert!(json.contains(expected), "missing {expected} in {json}");
    }
}

#[test]
fn umik_capture_calibration_stays_incomplete_until_t67_sign_self_test() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let plan = ReferencePlan::acoustic(&device, None).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 512);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "capture.raw",
        None,
    )
    .expect("finalize");
    // T67 符号自测未执行：如实 INCOMPLETE，不得声称 ±3 dB（calibration-spec/MICS.md §UMIK-1/§MA1）。
    assert_eq!(
        report.mic_calibration.sign_self_test,
        SignSelfTestStatus::NotRun
    );
    assert_eq!(
        report.mic_calibration.accuracy_status(),
        CAL_ACCURACY_INCOMPLETE
    );
    assert_eq!(report.calibration_accuracy_claim_db(), None);
    let json = report.to_json();
    assert!(json.contains("\"mic_calibration\":"));
    assert!(json.contains("\"sign_self_test\":\"NOT_RUN\""));
    assert!(json.contains("\"accuracy_status\":\"INCOMPLETE\""));
    assert!(json.contains("\"accuracy_db\":null"));
    assert!(
        !json.contains("\"accuracy_db\":3"),
        "must not claim ±3 dB before T67 sign self-test: {json}"
    );
}

/// 契约样例：带显著漂移与 MOTU auxiliary electrical reference 的声学报告 JSON。
///
/// 仅当 `TSPI_T62_EVIDENCE_DIR` 设置时才落盘（证据生成用）；普通 CI 无环境变量，只做离线断言、
/// 不写任何文件。样例给 T63/T64/T49/T66 看 schema（含时间轴/时钟/auxiliary 隔离字段）。
#[test]
fn umik_capture_writes_evidence_example_when_requested() {
    let timeline = t62_timeline(3, 5.0);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let plan = ReferencePlan::acoustic(&device, Some(t62_reference_channel())).expect("plan");
    let injected_ppm = 12.84;
    let (umik, _) = t62_synth_umik(&grid, injected_ppm, SampleFormat::S24_3LE, 1024);
    let (auxiliary, _) = t62_synth_reference(&grid, injected_ppm, 2, 128);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "evidence/capture-718-7696.raw",
        Some(auxiliary),
    )
    .expect("finalize example");
    let json = report.to_json();
    assert_eq!(json, report.to_json());
    assert!(json.contains("\"schema\":\"tspi-umik-capture/v1\""));
    assert!(json.contains("\"schema\":\"tspi-umik-timebase/v1\""));
    let auxiliary_summary = report.auxiliary_reference.as_ref().expect("aux summary");
    assert_eq!(auxiliary_summary.role, CaptureStreamRole::MotuElectrical);
    assert_eq!(auxiliary_summary.device, "hw:motu,0");
    assert_eq!(
        report.plan.phase_reference,
        CommonPhaseReference::UmikDirectOnset
    );
    let ppm = report.timebase.clock.clock_ppm.expect("ppm");
    assert!((ppm - injected_ppm).abs() <= 2.0, "example ppm {ppm}");
    assert!(report.timebase.clock.drift_significant);
    assert!(report.timebase.clock.residual_phase_claim.applicable);
    if let Some(dir) = std::env::var_os("TSPI_T62_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create evidence dir");
        let path = dir.join("capture-report-example.json");
        std::fs::write(&path, json.as_bytes()).expect("write capture-report-example.json");
        eprintln!("T62 evidence example written: {}", path.display());
    }
}

#[test]
fn umik_capture_peak_noise_floor_and_gate_are_reported() {
    let timeline = t62_timeline(2, 0.5);
    let grid = t62_grid(&timeline);
    let device = t62_umik_device(SampleFormat::S24_3LE);
    let plan = ReferencePlan::acoustic(&device, None).expect("plan");
    let (umik, _) = t62_synth_umik(&grid, 0.0, SampleFormat::S24_3LE, 512);
    let report = finalize_acoustic_capture(
        umik,
        &device,
        &grid,
        &plan,
        &OnsetConfig::default(),
        "capture.raw",
        None,
    )
    .expect("finalize");
    let peak = report.stream.capture_peak_dbfs;
    assert!(
        (peak - (-30.0)).abs() <= 0.5,
        "acoustic -30 dBFS peak got {peak}"
    );
    assert!(report.stream.peak_gate_pass);
    assert_eq!(report.stream.clipped_samples, 0);
    assert!(report.stream.noise_floor_dbfs.expect("noise floor") <= -100.0);
    assert_eq!(report.stream.channels, 1);
    assert_eq!(report.stream.sample_format, SampleFormat::S24_3LE);
    assert_eq!(report.stream.frames, report.capture_file.frames);
    assert_eq!(
        report.stream.seconds,
        report.stream.frames as f64 / FS as f64
    );
}

// ---------------------------------------------------------------------------
// 7. MOTU 电学 reference（T22 双工格式解码）
// ---------------------------------------------------------------------------

#[test]
fn umik_capture_electrical_reference_uses_t22_duplex_decode() {
    let format = t62_hw_format(sample_format::F32_INTERLEAVED, 2, 2, 128);
    let mut config = ElectricalReferenceConfig::new(
        ElectricalReferenceChannel::new("hw:motu,0", 0, SampleFormat::F32LE),
        format,
    )
    .expect("reference config");
    config.session_id = "t62-reference".to_string();
    config.epoch_id = 1;
    config.graph_id = PLACEHOLDER_GRAPH_ID.to_string();
    config.epoch_start_mono_ns = EPOCH_START_NS;
    let mut capture = ElectricalReferenceCapture::new(config).expect("capture");
    let frames = 128usize;
    let mut bytes = Vec::with_capacity(frames * 2 * 4);
    for frame in 0..frames {
        let value = if frame % 2 == 0 { 0.5f32 } else { -0.5 };
        bytes.extend_from_slice(&value.to_le_bytes());
        // 第二通道超界：必须由 T22 sanitize/clamp 并计入 clipped。
        bytes.extend_from_slice(&2.0f32.to_le_bytes());
    }
    capture
        .push_block(EPOCH_START_NS, &bytes)
        .expect("push duplex block");
    let stream = capture.finish().expect("finish reference capture");
    assert_eq!(stream.frames, frames as u64);
    assert_eq!(stream.reference_channel, Some(0));
    assert_eq!(stream.samples[0], 0.5);
    assert_eq!(stream.samples[1], -0.5);
    // T22 FormatStats 的 clipped 覆盖整个双工 period（含未选中的第二通道）。
    assert_eq!(stream.clipped_samples, frames as u64);
    assert_eq!(stream.raw.len(), frames * 4); // 选中通道 F32 dense raw

    // 第二个 capture 只验证短块被拒；其会话字段必须合法（new 会做严格校验）。
    let mut bad_config = ElectricalReferenceConfig::new(
        t62_reference_channel(),
        t62_hw_format(sample_format::S24_3LE_INTERLEAVED, 2, 2, 128),
    )
    .expect("config");
    bad_config.session_id = "t62-reference".to_string();
    bad_config.epoch_id = 1;
    bad_config.graph_id = PLACEHOLDER_GRAPH_ID.to_string();
    bad_config.epoch_start_mono_ns = EPOCH_START_NS;
    let mut bad = ElectricalReferenceCapture::new(bad_config).expect("capture");
    assert_eq!(
        bad.push_block(EPOCH_START_NS, &[0u8; 16])
            .unwrap_err()
            .code(),
        "E_CAPTURE_BLOCK"
    );
}

#[test]
fn umik_capture_electrical_reference_format_and_channel_are_validated() {
    let s16 = t62_hw_format(sample_format::S16_LE_INTERLEAVED, 2, 2, 64);
    assert_eq!(
        ElectricalReferenceConfig::new(t62_reference_channel(), s16)
            .unwrap_err()
            .code(),
        "E_CAPTURE_FORMAT"
    );
    let out_of_range = t62_hw_format(sample_format::S24_3LE_INTERLEAVED, 2, 2, 64);
    assert_eq!(
        ElectricalReferenceConfig::new(
            ElectricalReferenceChannel::new("hw:motu,0", 5, SampleFormat::S24_3LE),
            out_of_range
        )
        .unwrap_err()
        .code(),
        "E_CAPTURE_CONFIG"
    );
    let s32 = t62_hw_format(sample_format::S32_LE_INTERLEAVED, 2, 2, 64);
    assert_eq!(
        t62_reference_config_mismatch(s32).code(),
        "E_CAPTURE_CONFIG"
    );
}

fn t62_reference_config_mismatch(format: FormatConfig) -> CaptureError {
    ElectricalReferenceConfig::new(
        ElectricalReferenceChannel::new("hw:motu,0", 0, SampleFormat::S24_3LE),
        format,
    )
    .unwrap_err()
}

// ---------------------------------------------------------------------------
// 5. T62 onset-fix：20 Hz 起扫 + 房间低频滚降（真实 T3 NoOnsets 回归）
// ---------------------------------------------------------------------------

/// 合成房间低频滚降：二阶高通 150 Hz（12 dB/oct），复现实测房间 20–50 Hz 陡降
/// （20 Hz 段比峰值低 >20 dB，旧 0.3x 阈值必然漏检）。
const T62_ROOM_HP_HZ: f64 = 150.0;
/// 注入的额外到达延迟（样本；1152 = 24 ms @48k，对应实测 lag +0.024 s 形态）。
const T62_INJECTED_LAG_SAMPLES: f64 = 1_152.0;

fn t62_test_noise(seed: &mut u32) -> f64 {
    // 4 个均匀数近高斯（std = sqrt(4/12) = 0.577），归一化到 std=1。
    let mut sum = 0.0f64;
    for _ in 0..4 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        sum += *seed as f64 / 4_294_967_296.0;
    }
    (sum / 4.0 - 0.5) / (1.0 / (12.0 * 4.0f64).sqrt())
}

/// 合成“20 Hz 起扫 + 房间低频滚降 + 噪声”的 UMIK 采集样本：
/// sweep 放在 delay + render_frame*(1+ppm) 处，经二阶高通 100 Hz，归一化到 -39 dBFS
/// 峰值，再叠加 -75 dBFS RMS 噪声（与 14:30 实测 capture_peak/noise_floor 同量级）。
fn t62_room_shaped_samples(
    grid: &RepeatGrid,
    ppm: f64,
    extra_delay_samples: f64,
) -> (Vec<f32>, Vec<f64>) {
    let slope = 1.0 + ppm * 1.0e-6;
    let delay = CAPTURE_OFFSET_SAMPLES + extra_delay_samples;
    let last_onset = grid
        .slots
        .last()
        .and_then(|slot| slot.repeat_render_frames.last().copied())
        .unwrap_or(0) as f64;
    let total = ((last_onset * slope + delay + grid.sweep_frames as f64).ceil() as usize)
        + FS as usize
        + 2048;
    let mut samples = vec![0.0f64; total];
    let mut truth = Vec::with_capacity(grid.repeat_count());
    for slot in &grid.slots {
        for render_frame in &slot.repeat_render_frames {
            let start = delay + *render_frame as f64 * slope;
            truth.push(start);
            let first = start.floor() as i64;
            for k in 0..grid.sweep_frames as i64 {
                let index = first + k;
                if index < 0 || index as usize >= total {
                    continue;
                }
                let t = index as f64 - start;
                if t < 0.0 {
                    continue;
                }
                samples[index as usize] += t62_sweep_sample(grid, t);
            }
        }
    }
    let highpass = Biquad::highpass(T62_ROOM_HP_HZ, FS as f64);
    let mut state = BiquadState::default();
    for sample in samples.iter_mut() {
        *sample = highpass.step(&mut state, *sample);
    }
    let peak = samples.iter().fold(0.0f64, |acc, value| acc.max(value.abs()));
    let scale = 10.0f64.powf(-39.0 / 20.0) / peak.max(1.0e-12);
    // 实测（14:30 插桩采集）在扫频真实起点处有 -64.9 dBFS 的宽带起始瞬态（比整遍峰值低
    // 约 20 dB）：它才是 20 Hz 起扫可观测的绝对起点。给合成夹具补同形态 8 ms 衰减瞬态，
    // 让宽带精化有真实的起点信号；稳态 LF 仍被房间滚降压到 0.3x 峰值以下。
    // 用确定性衰减正弦（200 Hz、15 ms 时间常数、40 ms 长度）而不是白噪脉冲：包络平滑、
    // 峰值可预测（不会因噪声样本超出 2 sigma 把 early-peak 抬过 0.3x 门）。
    let burst = 0.004f64;
    let burst_tau = 0.015 * FS as f64;
    let burst_len = (0.040 * FS as f64) as usize;
    for start in &truth {
        let first = start.round() as usize;
        for offset in 0..burst_len {
            let index = first + offset;
            if index >= samples.len() {
                break;
            }
            let decay = (-(offset as f64) / burst_tau).exp();
            let phase = 2.0 * std::f64::consts::PI * 200.0 * offset as f64 / FS as f64;
            samples[index] += burst * decay * phase.sin();
        }
    }
    let noise_rms = 10.0f64.powf(-75.0 / 20.0);
    let mut seed = 0x1234_5678u32;
    let samples: Vec<f32> = samples
        .iter()
        .map(|value| (value * scale + t62_test_noise(&mut seed) * noise_rms) as f32)
        .collect();
    (samples, truth)
}

fn t62_noise_only_samples(grid: &RepeatGrid) -> Vec<f32> {
    let total = grid.total_frames as usize + FS as usize + 2048;
    let noise_rms = 10.0f64.powf(-72.0 / 20.0);
    let mut seed = 0xdead_beefu32;
    (0..total)
        .map(|_| (t62_test_noise(&mut seed) * noise_rms) as f32)
        .collect()
}

/// 夹具自证 + 旧配置复现 NoOnsets + 新默认配置检出且 lag 与注入值一致。
#[test]
fn umik_capture_onset_fix_detects_20hz_start_with_room_lf_rolloff() {
    let timeline = t62_onset_fix_timeline(3);
    let grid = t62_grid(&timeline);
    let injected_ppm = 12.84;
    let (samples, truth) = t62_room_shaped_samples(&grid, injected_ppm, T62_INJECTED_LAG_SAMPLES);
    assert_eq!(truth.len(), grid.repeat_count());

    // 夹具自证：20 Hz 起始后前 0.7 s 幅度低于整遍峰值的 0.3 倍（旧检测器失败条件）。
    let onset_index = truth[0].round() as usize;
    let early_end = (onset_index + (0.7 * FS as f64) as usize).min(samples.len());
    let sweep_end = (onset_index + grid.sweep_frames as usize).min(samples.len());
    let early_peak = samples[onset_index..early_end]
        .iter()
        .fold(0.0f64, |acc, value| acc.max(value.abs() as f64));
    let sweep_peak = samples[onset_index..sweep_end]
        .iter()
        .fold(0.0f64, |acc, value| acc.max(value.abs() as f64));
    assert!(
        early_peak < 0.3 * sweep_peak,
        "fixture must reproduce the 0.3x-peak LF ramp defect: early {early_peak} vs peak {sweep_peak}"
    );

    // 旧配置（threshold 0.3，只搜 ±0.25 s）在同一夹具上必须复现 NoOnsets。
    let legacy = OnsetConfig {
        search_forward_seconds: 0.25,
        threshold_fraction: 0.3,
        ..OnsetConfig::default()
    };
    let legacy_stream = t62_encode_umik(&grid, &samples, SampleFormat::S24_3LE, 1024);
    let legacy_error =
        estimate_capture_timebase(&legacy_stream, &grid, &legacy).expect_err("legacy must fail");
    assert_eq!(legacy_error.code(), "E_CAPTURE_NO_ONSETS");
    assert!(
        legacy_error.to_string().contains("bandlimited_lf"),
        "failure must state the detector/reason: {legacy_error}"
    );

    // 新默认配置：全部 onset 检出，带限 LF 方法 + 宽带精化，lag 与注入值一致。
    let stream = t62_encode_umik(&grid, &samples, SampleFormat::S24_3LE, 1024);
    let timebase = estimate_capture_timebase(&stream, &grid, &OnsetConfig::default())
        .expect("onset-fix timebase");
    assert_eq!(timebase.onsets.len(), grid.repeat_count());
    assert_eq!(timebase.onsets_invalid_slots, 0);
    assert!(timebase.all_slots_valid);
    for (onset, expected) in timebase.onsets.iter().zip(truth.iter()) {
        assert_eq!(onset.method, OnsetMethod::BandlimitedLfThreshold);
        assert!(
            onset.wide_refined,
            "wideband refinement must provide absolute timing"
        );
        assert!(
            (onset.onset_capture_sample - expected).abs() <= 480.0,
            "onset {} vs truth {} (must be within 10 ms)",
            onset.onset_capture_sample,
            expected
        );
    }
    let expected_lag = CAPTURE_OFFSET_SAMPLES + T62_INJECTED_LAG_SAMPLES;
    for slot in &timebase.slots {
        let lag = slot.lag_samples.expect("lag");
        assert!(
            (lag - expected_lag).abs() <= 480.0,
            "lag {lag} vs injected {expected_lag} (must be within 10 ms)"
        );
    }
    // 房间夹具的起点由宽带瞬态过阈给出，过阈延迟随各遍 LF 参考峰轻微变化（几样本）；
    // 绝对 ppm 精度由纯自由场夹具（umik_capture_timebase_recovers_injected_ppm）严格覆盖，
    // 这里只要求同一注入 ppm 下不跑飞（4 个间隔的 sigma 远大于点估计误差）。
    let ppm = timebase.clock.clock_ppm.expect("ppm");
    assert!(
        (ppm - injected_ppm).abs() <= 25.0,
        "recovered ppm {ppm} vs injected {injected_ppm}"
    );
    assert!(timebase.clock.clock_ppm_ok);
    assert!(timebase.clock.residual_rms_samples < ONSET_RESIDUAL_MAX_SAMPLES);
}

/// 负例：纯噪声 / 远失配（音频比 seed 晚 2.2 s）/ 孤立短 pop 都不得误检。
#[test]
fn umik_capture_onset_fix_rejects_noise_misalignment_and_isolated_pop() {
    let timeline = t62_onset_fix_timeline(2);
    let grid = t62_grid(&timeline);
    let config = OnsetConfig::default();

    let noise = t62_noise_only_samples(&grid);
    let noise_stream = t62_encode_umik(&grid, &noise, SampleFormat::S24_3LE, 1024);
    let noise_error =
        estimate_capture_timebase(&noise_stream, &grid, &config).expect_err("noise must be rejected");
    assert_eq!(noise_error.code(), "E_CAPTURE_NO_ONSETS");

    // 音频整体晚 2.2 s（搜索窗 +1.0 s 之外）：不得把噪声/残响当前起点。
    let (misaligned, _) = t62_room_shaped_samples(&grid, 0.0, 2.2 * FS as f64);
    let misaligned_stream = t62_encode_umik(&grid, &misaligned, SampleFormat::S24_3LE, 1024);
    let misaligned_error = estimate_capture_timebase(&misaligned_stream, &grid, &config)
        .expect_err("misaligned capture must be rejected");
    assert_eq!(misaligned_error.code(), "E_CAPTURE_NO_ONSETS");

    // 孤立 2 ms pop（不是持续扫频）：LF 包络 hold 门必须拒绝。
    let mut popped = t62_noise_only_samples(&grid);
    let pop_start = grid
        .slots
        .first()
        .and_then(|slot| slot.repeat_render_frames.first().copied())
        .unwrap_or(0) as usize;
    let pop_len = (0.002 * FS as f64) as usize;
    for (offset, sample) in popped[pop_start..pop_start + pop_len].iter_mut().enumerate() {
        let decay = 1.0 - offset as f64 / pop_len as f64;
        *sample += (0.3 * decay * (offset as f64 * 0.7).sin()) as f32;
    }
    let popped_stream = t62_encode_umik(&grid, &popped, SampleFormat::S24_3LE, 1024);
    let popped_error = estimate_capture_timebase(&popped_stream, &grid, &config)
        .expect_err("isolated pop must be rejected");
    assert_eq!(popped_error.code(), "E_CAPTURE_NO_ONSETS");
}

/// 真实失败采集离线复算（不碰设备、不发声）：TSPI_T62_REAL_RAW 指向 14:30 窗口
/// job-wrap2 的 cap-111114.raw（sha256 665522f1…；stereo S24_3LE、729600 帧）。
///
/// 该采集旧检测器在 seed≈1.19 s 时返回 None（真实 T3 NoOnsets）；新检测器必须检出 2 个
/// repeat，且起点落在插桩互相关实测的 2.717 s / 8.218 s 附近（±10 ms）。
#[test]
#[ignore = "需要真实采集夹具：TSPI_T62_REAL_RAW=<14:30 wrap2 cap-111114.raw>"]
fn umik_capture_onset_fix_real_failing_capture_offline() {
    let Some(path) = std::env::var_os("TSPI_T62_REAL_RAW") else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read real raw");
    assert_eq!(bytes.len() % 6, 0, "stereo S24_3LE interleaved (2ch)");
    let frames = bytes.len() / 6;
    let mut samples = Vec::with_capacity(frames);
    let mut peak = 0.0f64;
    for frame in 0..frames {
        let offset = frame * 6;
        let value = bytes[offset] as i32
            | ((bytes[offset + 1] as i32) << 8)
            | ((bytes[offset + 2] as i32) << 16);
        let value = if value & 0x80_0000 != 0 {
            value - (1 << 24)
        } else {
            value
        };
        let sample = value as f32 / 8_388_608.0;
        peak = peak.max(sample.abs() as f64);
        samples.push(sample);
    }
    let mut config = SweepConfig::acoustic("wrap2", vec![SweepOutput::new(0, "FL", 0)]);
    config.sample_rate = FS;
    config.block_frames = 128;
    config.playback_channels = 22;
    config.stimulus = SweepStimulus::log_sweep(20.0, 20_000.0, 5.0, -25.0, 10.0, 2, 0.5);
    config.pre_seconds = 1.5;
    config.post_seconds = 1.5;
    let timeline = compile_timeline(&config).expect("wrap2 timeline");
    let grid = RepeatGrid::from_timeline(&timeline).expect("wrap2 grid");
    let stream = CapturedStream {
        role: CaptureStreamRole::UmikAdc,
        device: "hw:0,0".to_string(),
        session_id: grid.session_id.clone(),
        epoch_id: grid.epoch_id,
        graph_id: grid.graph_id.clone(),
        epoch_start_mono_ns: 0,
        // wrap2 引擎 seed≈1.193 s（nominal onset 2.693 s，xcorr 实测 2.717 s）。
        capture_start_render_frame: Some(1.1933 * FS as f64),
        sample_format: SampleFormat::S24_3LE,
        sample_rate_hz: FS,
        chunk_frames: 128,
        raw: Vec::new(),
        samples,
        blocks: Vec::new(),
        peak_abs: peak,
        clipped_samples: 0,
        nonfinite_samples: 0,
        frames: frames as u64,
        reference_channel: None,
    };
    let timebase = estimate_capture_timebase(&stream, &grid, &OnsetConfig::default())
        .expect("onset-fix must detect the real 20 Hz-start failing capture");
    assert_eq!(timebase.onsets.len(), 2);
    assert_eq!(timebase.onsets_invalid_slots, 0);
    assert!(timebase.all_slots_valid);
    for onset in &timebase.onsets {
        assert_eq!(onset.method, OnsetMethod::BandlimitedLfThreshold);
    }
    let first = timebase.onsets[0].onset_capture_sample / FS as f64;
    let second = timebase.onsets[1].onset_capture_sample / FS as f64;
    assert!(
        (first - 2.717).abs() < 0.010,
        "real onset {first:.4}s vs xcorr 2.717s"
    );
    assert!(
        (second - 8.218).abs() < 0.010,
        "real onset {second:.4}s vs xcorr 8.218s"
    );
}

/// 配置面：threshold/forward/noise gate 可配且做严格校验；默认 0.3 -> 0.1。
#[test]
fn umik_capture_onset_fix_config_is_configurable_and_validated() {
    let config = OnsetConfig::default();
    assert_eq!(config.threshold_fraction, 0.1);
    assert_eq!(config.search_forward_seconds, 1.0);
    assert_eq!(config.noise_gate_db, 6.0);
    assert!(config.validate().is_ok());

    let relaxed = OnsetConfig {
        threshold_fraction: 0.05,
        search_forward_seconds: 1.5,
        noise_gate_db: 10.0,
        ..OnsetConfig::default()
    };
    assert!(relaxed.validate().is_ok());

    for bad in [
        OnsetConfig {
            threshold_fraction: 0.0,
            ..OnsetConfig::default()
        },
        OnsetConfig {
            search_forward_seconds: 0.0,
            ..OnsetConfig::default()
        },
        OnsetConfig {
            noise_gate_db: -1.0,
            ..OnsetConfig::default()
        },
    ] {
        assert_eq!(bad.validate().unwrap_err().code(), "E_CAPTURE_CONFIG");
    }
}
