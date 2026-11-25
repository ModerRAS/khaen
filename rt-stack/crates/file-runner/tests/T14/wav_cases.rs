//! 自有 RIFF/WAVE 解析/解包/写回契约（T14 “自有 file 解析/解包” + 固定 f32 精度）。

use khaen_file_runner::wav::{
    decode_float32_samples, decode_wav, encode_wav_f32, file_sha256, info_json, parse_wav,
    WavFormat,
};
use khaen_file_runner::{E_FILE_CHANNELS, E_FILE_FORMAT, E_FILE_FRAMES, E_FILE_RATE, E_FILE_WAV};

use crate::common::{pcm16, subformat, wav_container, wav_extensible};

fn f32_bits(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in values {
        out.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    out
}

#[test]
fn wav_f32_round_trip_planar() {
    // 2 通道 4 帧 planar（encode 输入是 channel-major）。
    let planar = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.25, -0.25, 0.0];
    let bytes = encode_wav_f32(&planar, 2, 48_000).expect("encode");
    let info = parse_wav(&bytes).expect("parse");
    assert_eq!(info.format, WavFormat::Float32);
    assert_eq!(info.channels, 2);
    assert_eq!(info.frames, 4);
    assert_eq!(info.sample_rate, 48_000);
    let decoded = decode_wav(&bytes).expect("decode");
    assert_eq!(decoded.frames(), 4);
    assert_eq!(decoded.channels(), 2);
    assert_eq!(decoded.planar, planar);
    // channel 视图。
    assert_eq!(decoded.channel(0).expect("ch0"), &planar[0..4]);
    assert_eq!(decoded.channel(1).expect("ch1"), &planar[4..8]);
    assert!(decoded.channel(2).is_none());
    // info_json 字段面。
    let json = info_json(&info).to_canonical();
    assert!(json.contains("\"format\":\"f32\""));
    assert!(json.contains("\"channels\":2"));
}

#[test]
fn wav_encode_rejects_bad_shapes() {
    assert_eq!(
        encode_wav_f32(&[0.0; 3], 2, 48_000)
            .expect_err("not frame aligned")
            .code(),
        E_FILE_FRAMES
    );
    assert_eq!(
        encode_wav_f32(&[], 0, 48_000)
            .expect_err("zero channels")
            .code(),
        E_FILE_CHANNELS
    );
    assert_eq!(
        encode_wav_f32(&[], 33, 48_000)
            .expect_err("too many channels")
            .code(),
        E_FILE_CHANNELS
    );
    assert_eq!(
        encode_wav_f32(&[], 2, 0).expect_err("zero rate").code(),
        E_FILE_RATE
    );
    // 空 planar（0 帧）合法。
    let empty = encode_wav_f32(&[], 2, 48_000).expect("empty ok");
    assert_eq!(parse_wav(&empty).expect("parse").frames, 0);
}

#[test]
fn wav_parse_rejects_corrupt_containers() {
    let valid = encode_wav_f32(&[0.0, 0.0, 0.0, 0.0], 2, 48_000).expect("encode");
    // 短于 RIFF 头。
    assert_eq!(parse_wav(b"RIFF").expect_err("short").code(), E_FILE_WAV);
    // 魔数错。
    let mut bad = valid.clone();
    bad[0..4].copy_from_slice(b"RIFX");
    assert_eq!(parse_wav(&bad).expect_err("magic").code(), E_FILE_WAV);
    // RIFF size 超出文件。
    let mut bad = valid.clone();
    bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(parse_wav(&bad).expect_err("riff size").code(), E_FILE_WAV);
    // WAVE form 缺失。
    let mut bad = valid.clone();
    bad[8..12].copy_from_slice(b"AVI ");
    assert_eq!(parse_wav(&bad).expect_err("form").code(), E_FILE_WAV);
    // block_align 不一致（fmt body 起始 20；block_align 在 32..34）。
    let mut bad = valid.clone();
    bad[32..34].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(parse_wav(&bad).expect_err("block_align").code(), E_FILE_WAV);
    // byte_rate 不一致（28..32）。
    let mut bad = valid.clone();
    bad[28..32].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(parse_wav(&bad).expect_err("byte_rate").code(), E_FILE_WAV);
    // data 长度非整帧。
    let bad = wav_container(1, 16, 2, 48_000, &[0u8; 6]);
    assert_eq!(
        parse_wav(&bad).expect_err("partial frame").code(),
        E_FILE_FRAMES
    );
    // 通道 0 / 采样率 0。
    let bad = wav_container(1, 16, 0, 48_000, &[]);
    assert_eq!(
        parse_wav(&bad).expect_err("zero channels").code(),
        E_FILE_WAV
    );
    let bad = wav_container(1, 16, 2, 0, &[]);
    assert_eq!(parse_wav(&bad).expect_err("zero rate").code(), E_FILE_WAV);
    // 未知编码 / 位深。
    assert_eq!(
        parse_wav(&wav_container(6, 8, 1, 8_000, &[]))
            .expect_err("alaw")
            .code(),
        E_FILE_FORMAT
    );
    assert_eq!(
        parse_wav(&wav_container(1, 20, 1, 48_000, &[]))
            .expect_err("pcm20")
            .code(),
        E_FILE_FORMAT
    );
    assert_eq!(
        parse_wav(&wav_container(3, 16, 1, 48_000, &[]))
            .expect_err("float16")
            .code(),
        E_FILE_FORMAT
    );
}

#[test]
fn wav_decode_pcm_formats_scale_exactly() {
    // PCM16：0 -> 0，32767 -> 32767/32768，-32768 -> -1。
    let data = pcm16(&[0, 32_767, -32_768]);
    let decoded = decode_wav(&wav_container(1, 16, 1, 48_000, &data)).expect("pcm16");
    assert_eq!(decoded.planar, vec![0.0f32, 32_767.0 / 32_768.0, -1.0]);
    // PCM8 偏移二进制：0 -> -1，128 -> 0，255 -> 127/128。
    let decoded = decode_wav(&wav_container(1, 8, 1, 48_000, &[0u8, 128, 255])).expect("pcm8");
    assert_eq!(decoded.planar, vec![-1.0f32, 0.0, 127.0 / 128.0]);
    // PCM24 sign-extend：0x7FFFFF -> max，0x800000 -> -1，0x000001 -> 1/8388608。
    let data = vec![0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00];
    let decoded = decode_wav(&wav_container(1, 24, 1, 48_000, &data)).expect("pcm24");
    assert_eq!(
        decoded.planar,
        vec![8_388_607.0 / 8_388_608.0, -1.0, 1.0 / 8_388_608.0]
    );
    // PCM32 全量程。
    let data = vec![0xFF, 0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x00, 0x80];
    let decoded = decode_wav(&wav_container(1, 32, 1, 48_000, &data)).expect("pcm32");
    assert_eq!(
        decoded.planar,
        vec![2_147_483_647.0 / 2_147_483_648.0, -1.0]
    );
    // float64 单次舍入到 f32。
    let value = 0.1f64;
    let data = value.to_bits().to_le_bytes().to_vec();
    let decoded = decode_wav(&wav_container(3, 64, 1, 48_000, &data)).expect("float64");
    assert_eq!(decoded.planar, vec![value as f32]);
}

#[test]
fn wav_decode_sanitizes_nonfinite_and_clips() {
    let nan = f32::NAN.to_bits().to_le_bytes();
    let inf = f32::INFINITY.to_bits().to_le_bytes();
    let over = 2.0f32.to_bits().to_le_bytes();
    let under = (-2.0f32).to_bits().to_le_bytes();
    let mut data = Vec::new();
    data.extend_from_slice(&nan);
    data.extend_from_slice(&inf);
    data.extend_from_slice(&over);
    data.extend_from_slice(&under);
    let decoded = decode_wav(&wav_container(3, 32, 1, 48_000, &data)).expect("decode");
    assert_eq!(decoded.planar, vec![0.0f32, 0.0, 1.0, -1.0]);
    assert_eq!(decoded.stats.nonfinite_in, 2);
    assert_eq!(decoded.stats.clipped, 2);
    assert_eq!(decoded.stats.nonfinite_out, 0);
    // 合法样本不计数。
    let clean = decode_wav(&wav_container(3, 32, 1, 48_000, &f32_bits(&[0.5]))).expect("decode");
    assert_eq!(clean.stats, Default::default());
}

#[test]
fn wav_extensible_requires_known_subformat_and_matching_valid_bits() {
    // PCM16 extensible，valid_bits == bits：接受。
    let data = pcm16(&[0, 16_384]);
    let bytes = wav_extensible(0xFFFE, 16, 16, 1, 48_000, subformat("pcm"), &data);
    let info = parse_wav(&bytes).expect("valid extensible");
    assert_eq!(info.format, WavFormat::Pcm16);
    // valid_bits 24 != 容器 32（24-in-32 不允许）。
    let bytes = wav_extensible(0xFFFE, 32, 24, 1, 48_000, subformat("pcm"), &[0u8; 4]);
    assert_eq!(
        parse_wav(&bytes).expect_err("24-in-32").code(),
        E_FILE_FORMAT
    );
    // 未知 GUID。
    let mut guid = subformat("pcm");
    guid[8] = 0x81;
    let bytes = wav_extensible(0xFFFE, 16, 16, 1, 48_000, guid, &data);
    assert_eq!(parse_wav(&bytes).expect_err("guid").code(), E_FILE_FORMAT);
    // float extensible。
    let float_data = f32_bits(&[0.5, -0.5]);
    let bytes = wav_extensible(0xFFFE, 32, 32, 1, 48_000, subformat("float"), &float_data);
    let decoded = decode_wav(&bytes).expect("float extensible");
    assert_eq!(decoded.planar, vec![0.5f32, -0.5]);
}

#[test]
fn wav_decode_float32_samples_rejects_pcm_and_keeps_out_of_range() {
    let pcm = wav_container(1, 16, 1, 48_000, &pcm16(&[1_000]));
    assert_eq!(
        decode_float32_samples(&pcm).expect_err("pcm").code(),
        E_FILE_FORMAT
    );
    // 系数文件路径不做设备 clip：>1.0 原样返回。
    let data = f32_bits(&[2.5, -3.0]);
    let (info, samples) =
        decode_float32_samples(&wav_container(3, 32, 1, 48_000, &data)).expect("float");
    assert_eq!(info.format, WavFormat::Float32);
    assert_eq!(samples, vec![2.5f32, -3.0]);
}

#[test]
fn wav_file_sha256_is_content_hash() {
    let a = encode_wav_f32(&[0.0, 0.25], 1, 48_000).expect("encode");
    let b = encode_wav_f32(&[0.0, 0.5], 1, 48_000).expect("encode");
    assert_eq!(file_sha256(&a), khaen_file_runner::sha256::sha256_hex(&a));
    assert_ne!(file_sha256(&a), file_sha256(&b));
    assert_eq!(file_sha256(&a).len(), 64);
}
