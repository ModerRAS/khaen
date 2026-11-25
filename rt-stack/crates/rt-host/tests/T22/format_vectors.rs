// T22（format）边界参考向量与语义测试 —— WBS T22 / team task-93。
//
// 本文件经 src/format.rs 末尾的 cfg(test) mod format_tests { include!(...) } 进入
// lib 单测构建（cargo 不自动发现 tests/<子目录>；公共注册属 integrator，WBS T06 纪律）。
// 运行：cargo test -p tspi-rt-host format（或 format_tests 精确过滤）
// 全部测试名 t22_ 前缀；离线、不打开设备、不依赖货架第三方 crate。
//
// 覆盖：格式/字节映射/符号扩展/端序、归一化、round-ties-even、饱和/clip 统计、
// nonfinite sanitize、planar stride（16 样本倍数 + 64B 对齐契约）、TPDF dither
// （确定性/统计/状态/配置一致性）、T17 协商 actual（MOTU 20ch/22ch S24 48k）联动、
// 无分配/无 syscall 源码自证。

use super::*;
use crate::alsa::{sample_bytes, DeviceSelector, DuplexRequest, DuplexSession, FakeBackend};
use tspi_dsp_core::abi::{sample_format, TspiHwParamsV1, ABI_VERSION};
use tspi_dsp_core::rt::RtStatus;

const FMT_S24: u16 = sample_format::S24_3LE_INTERLEAVED;
const FMT_S32: u16 = sample_format::S32_LE_INTERLEAVED;
const FMT_S16: u16 = sample_format::S16_LE_INTERLEAVED;
const FMT_F32I: u16 = sample_format::F32_INTERLEAVED;
const FMT_F32P: u16 = sample_format::F32_PLANAR;
const FMT_F64: u16 = sample_format::F64_PLANAR_REFERENCE;

/// /proc/asound/cards 样本（T01 E1：MOTU UltraLite-mk5 = card 4）。
const CARDS_MOTU: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                       HDA Intel PCH at 0x0000 irq 130
  4 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5
                      MOTU UltraLite-mk5 at usb-0000:01:00.0-1.4, full speed
";

/// 64 B 对齐 planar 缓冲（验证 §11.9 每通道基址 64 B 对齐契约）。
#[repr(align(64))]
struct Aligned64<const N: usize>([f32; N]);

// ---------------------------------------------------------------------------
// 构造辅助
// ---------------------------------------------------------------------------

/// 构造冻结 hwparams（in/out stride = period × ch × bytes，与 T17 actual 口径一致）。
fn hw_with(
    format: u16,
    n_in: u16,
    n_out: u16,
    rate: u32,
    period: u32,
    buffer: u32,
    flags: u32,
    seed: u64,
) -> TspiHwParamsV1 {
    let bytes = device_sample_bytes(format).expect("test format must be device format") as u32;
    TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: format,
        n_in,
        n_out,
        sample_rate: rate,
        period_frames: period,
        buffer_frames: buffer,
        in_stride_bytes: n_in as u32 * period * bytes,
        out_stride_bytes: n_out as u32 * period * bytes,
        flags,
        reserved: [seed, 0, 0, 0],
    }
}

fn hw(format: u16, n_in: u16, n_out: u16) -> TspiHwParamsV1 {
    hw_with(format, n_in, n_out, 48_000, 64, 256, 0, 0)
}

fn cfg_of(hw: TspiHwParamsV1, dither: DitherCfg) -> FormatConfig {
    FormatConfig { hw, dither }
}

fn cfg(format: u16, n_in: u16, n_out: u16) -> FormatConfig {
    cfg_of(hw(format, n_in, n_out), DitherCfg::Off)
}

fn cfg_dither(hw: TspiHwParamsV1) -> FormatConfig {
    FormatConfig {
        hw,
        dither: DitherCfg::Tpdf,
    }
}

/// T01 E1 实测 MOTU 基线 hwparams（T17 校验产物）。
fn motu_hw() -> TspiHwParamsV1 {
    TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: FMT_S24,
        n_in: 20,
        n_out: 22,
        sample_rate: 48_000,
        period_frames: 128,
        buffer_frames: 1024,
        in_stride_bytes: 7680,
        out_stride_bytes: 8448,
        flags: HW_FLAG_CAPTURE_SANITIZE,
        reserved: [0; 4],
    }
}

/// 写一个 S24_3LE 码字（interleaved 样本下标，非字节下标）。
fn put_s24(src: &mut [u8], sample_index: usize, code: i32) {
    let bytes = code.to_le_bytes();
    src[sample_index * 3] = bytes[0];
    src[sample_index * 3 + 1] = bytes[1];
    src[sample_index * 3 + 2] = bytes[2];
}

/// 读一个 S24_3LE 码字（符号扩展）。
fn get_s24(dst: &[u8], sample_index: usize) -> i32 {
    let raw = (dst[sample_index * 3] as u32)
        | ((dst[sample_index * 3 + 1] as u32) << 8)
        | ((dst[sample_index * 3 + 2] as u32) << 16);
    ((raw << 8) as i32) >> 8
}

fn put_u16le(dst: &mut [u8], sample_index: usize, code: u16) {
    let bytes = code.to_le_bytes();
    dst[sample_index * 2] = bytes[0];
    dst[sample_index * 2 + 1] = bytes[1];
}

fn get_u16le(src: &[u8], sample_index: usize) -> u16 {
    (src[sample_index * 2] as u16) | ((src[sample_index * 2 + 1] as u16) << 8)
}

fn put_u32le(dst: &mut [u8], sample_index: usize, code: u32) {
    let bytes = code.to_le_bytes();
    dst[sample_index * 4] = bytes[0];
    dst[sample_index * 4 + 1] = bytes[1];
    dst[sample_index * 4 + 2] = bytes[2];
    dst[sample_index * 4 + 3] = bytes[3];
}

fn get_u32le(src: &[u8], sample_index: usize) -> u32 {
    (src[sample_index * 4] as u32)
        | ((src[sample_index * 4 + 1] as u32) << 8)
        | ((src[sample_index * 4 + 2] as u32) << 16)
        | ((src[sample_index * 4 + 3] as u32) << 24)
}

// ---------------------------------------------------------------------------
// A. 格式表 / 错误码 / 与 T17 常量一致性
// ---------------------------------------------------------------------------

#[test]
fn t22_format_table_and_sample_bytes() {
    assert_eq!(device_sample_bytes(FMT_S24), Some(3));
    assert_eq!(device_sample_bytes(FMT_S32), Some(4));
    assert_eq!(device_sample_bytes(FMT_S16), Some(2));
    assert_eq!(device_sample_bytes(FMT_F32I), Some(4));
    // 非设备格式：F32_PLANAR 仅 core 内部、F64 仅 reference、未知值一律拒绝。
    assert_eq!(device_sample_bytes(FMT_F32P), None);
    assert_eq!(device_sample_bytes(FMT_F64), None);
    assert_eq!(device_sample_bytes(6), None);
    assert_eq!(device_sample_bytes(99), None);
    // 与 T17 alsa::sample_bytes 常量表不得漂移（两处实现同一 §11.9 枚举）。
    for format in [FMT_S24, FMT_S32, FMT_S16, FMT_F32I] {
        assert_eq!(
            device_sample_bytes(format).map(|bytes| bytes as u32),
            sample_bytes(format)
        );
    }
    assert_eq!(sample_bytes(FMT_F32P), None);
    assert_eq!(sample_bytes(FMT_F64), None);
}

#[test]
fn t22_error_codes_match_core_v1_9_2() {
    // T22 只用 core-v1 §9.2 冻结码（不新增码，core-v1 §11.9/§9.2）。
    let table: [(RtStatus, u32); 7] = [
        (RtStatus::ERR_ABI_VERSION, 101),
        (RtStatus::ERR_RATE, 104),
        (RtStatus::ERR_BLOCK_SIZE, 105),
        (RtStatus::ERR_CHANNELS, 106),
        (RtStatus::ERR_STRIDE_ALIGN, 109),
        (RtStatus::ERR_DEVICE_PARAMS, 118),
        (RtStatus::ERR_UNSUPPORTED, 122),
    ];
    for (status, code) in table {
        assert_eq!(status.code(), code);
        assert!(!status.is_ok());
    }
    // 209/210 是「计数并继续」的 RT 事件，不是 format 的 Result 错误。
    assert_eq!(RtStatus::RT_E_INPUT_NONFINITE.code(), 209);
    assert_eq!(RtStatus::RT_E_INPUT_CLIP.code(), 210);
}

// ---------------------------------------------------------------------------
// B. capture 解码：端序 / 符号扩展 / 满量程归一化
// ---------------------------------------------------------------------------

#[test]
fn t22_capture_s24_sign_extension_and_full_scale() {
    let cfg = cfg(FMT_S24, 1, 1);
    let mut src = [0u8; 64 * 3];
    let codes = [
        0i32, 1, -1, 8_388_607, -8_388_608, 4_194_304, -4_194_304, 0x7F_FFFF, -0x7F_FFFF,
    ];
    for (frame, code) in codes.iter().enumerate() {
        put_s24(&mut src, frame, *code);
    }
    let mut dst = [0f32; 64];
    let stats = convert_capture(&cfg, &src, &mut dst).expect("capture");
    assert!(stats.is_clean());
    for (frame, code) in codes.iter().enumerate() {
        assert_eq!(dst[frame], *code as f32 / 8_388_608.0, "frame {}", frame);
    }
    // 经典符号扩展边界：bit23 = 1 必须解为负，不是大正数。
    assert_eq!(dst[4], -1.0);
    assert_eq!(dst[1], 1.0 / 8_388_608.0);
    assert_eq!(dst[2], -1.0 / 8_388_608.0);
    assert_eq!(dst[3], 1.0 - 1.0 / 8_388_608.0);
    // 正满量程 = 1 - 2^-23（不是 1.0）；负满量程 = -1.0。
    assert!(dst[3] < 1.0);
}

#[test]
fn t22_capture_s32_full_range_and_f32_rounding() {
    let cfg = cfg(FMT_S32, 1, 1);
    let mut src = [0u8; 64 * 4];
    let codes = [
        0i32,
        1,
        -1,
        i32::MAX,
        i32::MIN,
        1_073_741_824,
        -1_073_741_824,
        0x7FFF_FF01,
    ];
    for (frame, code) in codes.iter().enumerate() {
        put_u32le(&mut src, frame, *code as u32);
    }
    let mut dst = [0f32; 64];
    let stats = convert_capture(&cfg, &src, &mut dst).expect("capture");
    assert!(stats.is_clean());
    assert_eq!(dst[0], 0.0);
    assert_eq!(dst[1], (1.0f32) / 2_147_483_648.0);
    assert_eq!(dst[2], (-1.0f32) / 2_147_483_648.0);
    // 0x7FFFFFFF 的精确值 1-2^-31 超出 f32 24-bit 有效位 -> 最近 f32 为 1.0。
    assert_eq!(dst[3], 1.0);
    assert_eq!(dst[4], -1.0);
    assert_eq!(dst[5], 0.5);
    assert_eq!(dst[6], -0.5);
    // int -> f32 ties-to-even 参考向量：0x7FFF_FF01 = 2147483393 -> 2147483392。
    assert_eq!(dst[7], 2_147_483_392.0 / 2_147_483_648.0);
    assert!(dst[7] < 1.0);
}

#[test]
fn t22_capture_s16_full_scale() {
    let cfg = cfg(FMT_S16, 1, 1);
    let mut src = [0u8; 64 * 2];
    let codes = [0u16, 1, 0x8000, 0xFFFF, 0x7FFF, 0x4000, 0xC000];
    for (frame, code) in codes.iter().enumerate() {
        put_u16le(&mut src, frame, *code);
    }
    let mut dst = [0f32; 64];
    let stats = convert_capture(&cfg, &src, &mut dst).expect("capture");
    assert!(stats.is_clean());
    assert_eq!(dst[0], 0.0);
    assert_eq!(dst[1], 1.0 / 32_768.0);
    assert_eq!(dst[2], -1.0);
    assert_eq!(dst[3], -1.0 / 32_768.0);
    assert_eq!(dst[4], 1.0 - 1.0 / 32_768.0);
    assert_eq!(dst[5], 0.5);
    assert_eq!(dst[6], -0.5);
}

#[test]
fn t22_capture_little_endian_byte_order() {
    // S24：LSB 在 byte0。
    let cfg24 = cfg(FMT_S24, 1, 1);
    let mut src24 = [0u8; 64 * 3];
    src24[0] = 0x01;
    src24[1] = 0x00;
    src24[2] = 0x00;
    src24[3] = 0x00;
    src24[4] = 0x00;
    src24[5] = 0x80;
    src24[6] = 0x78;
    src24[7] = 0x56;
    src24[8] = 0x34;
    let mut dst24 = [0f32; 64];
    convert_capture(&cfg24, &src24, &mut dst24).expect("capture s24");
    assert_eq!(dst24[0], 1.0 / 8_388_608.0);
    assert_eq!(dst24[1], -1.0);
    assert_eq!(dst24[2], 0x34_5678 as f32 / 8_388_608.0);

    // S16：LSB 在 byte0。
    let cfg16 = cfg(FMT_S16, 1, 1);
    let mut src16 = [0u8; 64 * 2];
    src16[0] = 0x34;
    src16[1] = 0x12;
    let mut dst16 = [0f32; 64];
    convert_capture(&cfg16, &src16, &mut dst16).expect("capture s16");
    assert_eq!(dst16[0], 0x1234u16 as f32 / 32_768.0);

    // S32：LSB 在 byte0。
    let cfg32 = cfg(FMT_S32, 1, 1);
    let mut src32 = [0u8; 64 * 4];
    src32[0] = 0x78;
    src32[1] = 0x56;
    src32[2] = 0x34;
    src32[3] = 0x12;
    let mut dst32 = [0f32; 64];
    convert_capture(&cfg32, &src32, &mut dst32).expect("capture s32");
    assert_eq!(dst32[0], 0x1234_5678i32 as f32 / 2_147_483_648.0);
}

#[test]
fn t22_capture_f32_nonfinite_sanitize_and_clip() {
    let cfg = cfg(FMT_F32I, 1, 1);
    let mut src = [0u8; 64 * 4];
    let inputs = [
        0.0f32,
        -0.0f32,
        0.5,
        -1.0,
        1.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1.000_000_1,
        -1.000_000_1,
        1.0e30,
        -1.0e30,
        f32::MIN_POSITIVE,
        1.0e-45,
    ];
    for (frame, value) in inputs.iter().enumerate() {
        put_u32le(&mut src, frame, value.to_bits());
    }
    let mut dst = [0f32; 64];
    let stats = convert_capture(&cfg, &src, &mut dst).expect("capture f32");
    assert_eq!(dst[0], 0.0);
    assert_eq!(
        dst[1].to_bits(),
        (-0.0f32).to_bits(),
        "sign of -0.0 preserved"
    );
    assert_eq!(dst[2], 0.5);
    assert_eq!(dst[3], -1.0);
    assert_eq!(dst[4], 1.0);
    // NaN/±Inf -> 0.0（209：sanitize 为 0，不 clip、不 fault）。
    assert_eq!(dst[5], 0.0);
    assert_eq!(dst[6], 0.0);
    assert_eq!(dst[7], 0.0);
    // 幅度越界 -> 饱和到 ±1.0 且计入 clip（210）。
    assert_eq!(dst[8], 1.0);
    assert_eq!(dst[9], -1.0);
    assert_eq!(dst[10], 1.0);
    assert_eq!(dst[11], -1.0);
    // 次正规/最小正规值保留（不做 flush-to-zero）。
    assert_eq!(dst[12], f32::MIN_POSITIVE);
    assert_eq!(dst[13], 1.0e-45);
    assert_eq!(stats.nonfinite_in, 3);
    assert_eq!(stats.clipped, 4);
    assert_eq!(stats.nonfinite_out, 0);
}

#[test]
fn t22_capture_motu_20ch_layout_and_toslink_channels() {
    // T01 E1：capture 20ch / 128 帧 S24，period 字节 7680；Toslink L/R = ch12/13。
    let hw = motu_hw();
    assert_eq!(hw.in_stride_bytes, 7680);
    assert_eq!(hw.out_stride_bytes, 8448);
    let cfg = cfg_of(hw, DitherCfg::Off);
    let mut src = [0u8; 7680];
    let frames = 128usize;
    let channels = 20usize;
    for frame in 0..frames {
        for channel in 0..channels {
            let code = (channel as i32 - 10) * 1000 + frame as i32;
            put_s24(&mut src, frame * channels + channel, code);
        }
    }
    let mut dst = [0f32; 128 * 20];
    let stats = convert_capture(&cfg, &src, &mut dst).expect("motu capture");
    assert!(stats.is_clean());
    for frame in 0..frames {
        for channel in 0..channels {
            let code = (channel as i32 - 10) * 1000 + frame as i32;
            assert_eq!(
                dst[channel * frames + frame],
                code as f32 / 8_388_608.0,
                "frame {} channel {}",
                frame,
                channel
            );
        }
    }
    // Toslink 两通道在 planar 中是第 12/13 个通道平面（E1 映射）。
    assert_eq!(dst[12 * frames], 2000.0 / 8_388_608.0);
    assert_eq!(dst[13 * frames], 3000.0 / 8_388_608.0);
}

// ---------------------------------------------------------------------------
// C. capture planar stride / 缓冲边界
// ---------------------------------------------------------------------------

#[test]
fn t22_capture_strided_planar_padding_untouched() {
    let cfg = cfg(FMT_S16, 2, 2);
    let frames = 64usize;
    let mut src = [0u8; 64 * 2 * 2];
    for frame in 0..frames {
        put_u16le(&mut src, frame * 2, frame as u16);
        put_u16le(&mut src, frame * 2 + 1, 0x8000);
    }
    // stride = 80（16 的倍数且 >= 64）：每通道 64 有效 + 16 填充。
    let stride = 80usize;
    let mut dst = [f32::NAN; 200];
    let stats = convert_capture_strided(&cfg, &src, &mut dst, stride).expect("strided capture");
    assert!(stats.is_clean());
    for frame in 0..frames {
        assert_eq!(dst[frame], frame as f32 / 32_768.0);
        assert_eq!(dst[stride + frame], -1.0);
    }
    // 两个通道平面的 [64,80) 填充必须保持调用方内容（NaN）。
    for frame in frames..stride {
        assert!(
            dst[frame].is_nan(),
            "channel 0 padding frame {} written",
            frame
        );
        assert!(
            dst[stride + frame].is_nan(),
            "channel 1 padding frame {} written",
            frame
        );
    }
    // 通道 2 起（62 × stride + ...）未被写。
    assert!(dst[2 * stride].is_nan());
}

#[test]
fn t22_capture_rejects_bad_stride_and_short_buffers() {
    let cfg = cfg(FMT_S16, 2, 2);
    let src = [0u8; 64 * 2 * 2];
    let mut dst = [0f32; 200];
    // stride 小于 period -> ERR_STRIDE_ALIGN
    assert_eq!(
        convert_capture_strided(&cfg, &src, &mut dst, 48),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // stride 不是 16 样本倍数（72 = 4×16 + 8）-> ERR_STRIDE_ALIGN
    assert_eq!(
        convert_capture_strided(&cfg, &src, &mut dst, 72),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // stride 合法但 dst 长度不足 -> ERR_STRIDE_ALIGN
    let mut short = [0f32; 100];
    assert_eq!(
        convert_capture_strided(&cfg, &src, &mut short, 80),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // src 少于一个 period 的字节数 -> ERR_STRIDE_ALIGN
    let tiny = [0u8; 10];
    assert_eq!(
        convert_capture(&cfg, &tiny, &mut dst),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // 空缓冲不 panic，返回错误。
    assert_eq!(
        convert_capture(&cfg, &[], &mut []),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
}

// ---------------------------------------------------------------------------
// D. validate：冻结 hwparams 校验表（core-v1 §9.2 码）
// ---------------------------------------------------------------------------

#[test]
fn t22_validate_accepts_all_device_formats_and_rates() {
    for format in [FMT_S24, FMT_S32, FMT_S16, FMT_F32I] {
        let layout = validate(&cfg(format, 2, 2)).expect("device format accepted");
        assert_eq!(layout.sample_format, format);
        assert_eq!(
            layout.bytes_per_sample,
            device_sample_bytes(format).unwrap()
        );
        assert_eq!(layout.period_frames, 64);
        assert_eq!(layout.in_min_bytes, 64 * 2 * layout.bytes_per_sample);
        assert_eq!(layout.out_min_bytes, 64 * 2 * layout.bytes_per_sample);
    }
    let layout_96k = validate(&cfg_of(
        hw_with(FMT_S32, 2, 2, 96_000, 128, 1024, 0, 0),
        DitherCfg::Off,
    ))
    .expect("96k accepted");
    assert_eq!(layout_96k.period_frames, 128);
}

#[test]
fn t22_validate_rejects_bad_abi_and_format() {
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.abi_version = 2;
    assert_eq!(validate(&config), Err(RtStatus::ERR_ABI_VERSION));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.sample_format = FMT_F32P;
    assert_eq!(validate(&config), Err(RtStatus::ERR_UNSUPPORTED));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.sample_format = FMT_F64;
    assert_eq!(validate(&config), Err(RtStatus::ERR_UNSUPPORTED));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.sample_format = 7;
    assert_eq!(validate(&config), Err(RtStatus::ERR_UNSUPPORTED));
}

#[test]
fn t22_validate_rejects_bad_rate_period_buffer() {
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.sample_rate = 44_100;
    assert_eq!(validate(&config), Err(RtStatus::ERR_RATE));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.period_frames = 256;
    config.hw.buffer_frames = 1024;
    assert_eq!(validate(&config), Err(RtStatus::ERR_BLOCK_SIZE));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.period_frames = 32;
    assert_eq!(validate(&config), Err(RtStatus::ERR_BLOCK_SIZE));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.buffer_frames = 32;
    assert_eq!(validate(&config), Err(RtStatus::ERR_DEVICE_PARAMS));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.buffer_frames = 100; // 非 period 整数倍
    assert_eq!(validate(&config), Err(RtStatus::ERR_DEVICE_PARAMS));
}

#[test]
fn t22_validate_rejects_bad_channels() {
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.n_in = 0;
    assert_eq!(validate(&config), Err(RtStatus::ERR_CHANNELS));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.n_out = 0;
    assert_eq!(validate(&config), Err(RtStatus::ERR_CHANNELS));
    // 设备通道不得超 core 上界（§3.1：20 in / 22 out）。
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.n_in = 21;
    config.hw.in_stride_bytes = 21 * 64 * 3;
    assert_eq!(validate(&config), Err(RtStatus::ERR_CHANNELS));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.n_out = 23;
    config.hw.out_stride_bytes = 23 * 64 * 3;
    assert_eq!(validate(&config), Err(RtStatus::ERR_CHANNELS));
}

#[test]
fn t22_validate_rejects_bad_strides_and_flags() {
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.in_stride_bytes = 64 * 2 * 3 - 1;
    assert_eq!(validate(&config), Err(RtStatus::ERR_STRIDE_ALIGN));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.out_stride_bytes = 64 * 2 * 3 - 1;
    assert_eq!(validate(&config), Err(RtStatus::ERR_STRIDE_ALIGN));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.in_stride_bytes += 1; // 不是 bytes 整数倍
    assert_eq!(validate(&config), Err(RtStatus::ERR_STRIDE_ALIGN));
    // 未知 flags 位（bit4 起）必须拒绝（§11.9 其余位 0）。
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.flags = 1 << 5;
    assert_eq!(validate(&config), Err(RtStatus::ERR_DEVICE_PARAMS));
    // reserved[1..4] 必须为 0。
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.reserved[1] = 1;
    assert_eq!(validate(&config), Err(RtStatus::ERR_DEVICE_PARAMS));
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.reserved[3] = 7;
    assert_eq!(validate(&config), Err(RtStatus::ERR_DEVICE_PARAMS));
    // 合法 flags 位（bit0..bit3）接受。
    let mut config = cfg(FMT_S24, 2, 2);
    config.hw.flags = HW_FLAG_CAPTURE_SANITIZE | HW_FLAG_OUTPUT_CLIP_METER;
    assert!(validate(&config).is_ok());
}

// ---------------------------------------------------------------------------
// E. playback 编码：round-ties-even / 饱和 / 非有限 / 字节序
// ---------------------------------------------------------------------------

#[test]
fn t22_playback_s24_boundary_codes() {
    let cfg = cfg(FMT_S24, 1, 1);
    let mut src = [0f32; 64];
    let cases: [(f32, i32); 12] = [
        (0.0, 0),
        (1.0, 8_388_607),
        (-1.0, -8_388_608),
        (0.5, 4_194_304),
        (-0.5, -4_194_304),
        (1.0 / 8_388_608.0, 1),
        (-1.0 / 8_388_608.0, -1),
        // ties-to-even 参考向量（0.5 LSB 恰好为半码）。
        (0.5 / 8_388_608.0, 0),
        (1.5 / 8_388_608.0, 2),
        (-0.5 / 8_388_608.0, 0),
        (-1.5 / 8_388_608.0, -2),
        (2.5 / 8_388_608.0, 2),
    ];
    for (frame, (value, _)) in cases.iter().enumerate() {
        src[frame] = *value;
    }
    let mut dst = [0u8; 64 * 3];
    let stats = convert_playback(&cfg, &src, &mut dst).expect("playback s24");
    assert!(stats.is_clean());
    for (frame, (value, code)) in cases.iter().enumerate() {
        assert_eq!(
            get_s24(&dst, frame),
            *code,
            "value {} frame {}",
            value,
            frame
        );
    }
}

#[test]
fn t22_playback_s32_and_s16_boundaries() {
    // S32
    let cfg32 = cfg(FMT_S32, 1, 1);
    let mut src32 = [0f32; 64];
    let cases32: [(f32, u32); 6] = [
        (0.0, 0x0000_0000),
        (1.0, 0x7FFF_FFFF),
        (-1.0, 0x8000_0000),
        (0.5, 0x4000_0000),
        (-0.5, 0xC000_0000),
        (0.25, 0x2000_0000),
    ];
    for (frame, (value, _)) in cases32.iter().enumerate() {
        src32[frame] = *value;
    }
    let mut dst32 = [0u8; 64 * 4];
    convert_playback(&cfg32, &src32, &mut dst32).expect("playback s32");
    for (frame, (value, code)) in cases32.iter().enumerate() {
        assert_eq!(get_u32le(&dst32, frame), *code, "value {}", value);
    }

    // S16
    let cfg16 = cfg(FMT_S16, 1, 1);
    let mut src16 = [0f32; 64];
    let cases16: [(f32, u16); 6] = [
        (0.0, 0x0000),
        (1.0, 0x7FFF),
        (-1.0, 0x8000),
        (0.5, 0x4000),
        (-0.5, 0xC000),
        (0.25, 0x2000),
    ];
    for (frame, (value, _)) in cases16.iter().enumerate() {
        src16[frame] = *value;
    }
    let mut dst16 = [0u8; 64 * 2];
    convert_playback(&cfg16, &src16, &mut dst16).expect("playback s16");
    for (frame, (value, code)) in cases16.iter().enumerate() {
        assert_eq!(get_u16le(&dst16, frame), *code, "value {}", value);
    }
}

#[test]
fn t22_playback_clip_saturation_and_stats() {
    let cfg = cfg(FMT_S24, 1, 1);
    let mut src = [0f32; 64];
    let values = [
        1.000_000_1f32,
        -1.000_000_1,
        1.0e30,
        -1.0e30,
        1.0,
        -1.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        2.0,
    ];
    for (frame, value) in values.iter().enumerate() {
        src[frame] = *value;
    }
    let mut dst = [0u8; 64 * 3];
    let stats = convert_playback(&cfg, &src, &mut dst).expect("playback clip");
    // |x| > 1 的 6 个样本：1.0000001 / -1.0000001 / ±1e30 / 2.0（±inf 属 nonfinite 先 sanitize）。
    assert_eq!(stats.clipped, 5);
    assert_eq!(stats.nonfinite_in, 3);
    assert_eq!(stats.nonfinite_out, 0);
    assert_eq!(get_s24(&dst, 0), 8_388_607);
    assert_eq!(get_s24(&dst, 1), -8_388_608);
    assert_eq!(get_s24(&dst, 2), 8_388_607);
    assert_eq!(get_s24(&dst, 3), -8_388_608);
    assert_eq!(get_s24(&dst, 4), 8_388_607);
    assert_eq!(get_s24(&dst, 5), -8_388_608);
    // NaN/±Inf -> sanitize 为 0（209），不按 clip 计。
    assert_eq!(get_s24(&dst, 6), 0);
    assert_eq!(get_s24(&dst, 7), 0);
    assert_eq!(get_s24(&dst, 8), 0);
    assert_eq!(get_s24(&dst, 9), 8_388_607);
}

#[test]
fn t22_playback_f32_output_clip_and_bits() {
    let cfg = cfg(FMT_F32I, 1, 1);
    let mut src = [0f32; 64];
    src[0] = 0.5;
    src[1] = -0.25;
    src[2] = 1.5;
    src[3] = -1.5;
    src[4] = f32::NAN;
    src[5] = f32::INFINITY;
    src[6] = -0.0;
    let mut dst = [0u8; 64 * 4];
    let stats = convert_playback(&cfg, &src, &mut dst).expect("playback f32");
    assert_eq!(f32::from_bits(get_u32le(&dst, 0)), 0.5);
    assert_eq!(f32::from_bits(get_u32le(&dst, 1)), -0.25);
    assert_eq!(f32::from_bits(get_u32le(&dst, 2)), 1.0);
    assert_eq!(f32::from_bits(get_u32le(&dst, 3)), -1.0);
    assert_eq!(f32::from_bits(get_u32le(&dst, 4)), 0.0);
    assert_eq!(f32::from_bits(get_u32le(&dst, 5)), 0.0);
    assert_eq!(get_u32le(&dst, 6), (-0.0f32).to_bits());
    assert_eq!(stats.nonfinite_in, 2);
    assert_eq!(stats.clipped, 2);
    assert_eq!(stats.nonfinite_out, 0);
}

#[test]
fn t22_playback_motu_22ch_byte_layout() {
    let cfg = cfg_of(motu_hw(), DitherCfg::Off);
    let frames = 128usize;
    let channels = 22usize;
    let stride = frames;
    let mut src = [0f32; 128 * 22];
    for channel in 0..channels {
        for frame in 0..frames {
            // 已知码字 -> 精确 f32（除以 2^23）。
            let code = (channel as i32 - 11) * 100 + frame as i32;
            src[channel * stride + frame] = code as f32 / 8_388_608.0;
        }
    }
    let mut dst = [0u8; 8448];
    let stats = convert_playback(&cfg, &src, &mut dst).expect("motu playback");
    assert!(stats.is_clean());
    for frame in 0..frames {
        for channel in 0..channels {
            let code = (channel as i32 - 11) * 100 + frame as i32;
            let index = frame * channels + channel;
            assert_eq!(
                get_s24(&dst, index),
                code,
                "byte offset {} (frame {} channel {})",
                index * 3,
                frame,
                channel
            );
        }
    }
}

#[test]
fn t22_playback_strided_reads_offsets_and_preserves_tail() {
    // out_stride 大于最小 period 跨度：尾部 padding 不能被写。
    let mut config = cfg(FMT_S16, 1, 2);
    config.hw.out_stride_bytes = 64 * 2 * 2 + 16;
    let stride = 80usize;
    let mut src = [0f32; 200];
    for frame in 0..64 {
        src[frame] = 0.25;
        src[stride + frame] = -0.25;
    }
    let mut dst = [0xAAu8; 64 * 2 * 2 + 16];
    let stats =
        convert_playback_strided(&config, &src, &mut dst, stride).expect("strided playback");
    assert!(stats.is_clean());
    for frame in 0..64 {
        assert_eq!(get_u16le(&dst, frame * 2), 0x2000);
        assert_eq!(get_u16le(&dst, frame * 2 + 1), 0xE000);
    }
    // 最小跨度之外的尾部 padding 保持 0xAA。
    for byte in &dst[64 * 2 * 2..] {
        assert_eq!(*byte, 0xAA, "tail padding written");
    }
}

#[test]
fn t22_roundtrip_integer_formats() {
    // 码字 -> capture f32 -> playback 码字：整数格式逐位可逆（无 dither）。
    // 码字写在 device 的 frame 0（sample index = channel，因 interleaved 帧主序）；
    // 其余帧为静音 0。planar 必须是完整 period（stride = period_frames，§11.9）。
    for (format, codes) in [
        (
            FMT_S24,
            [0i32, 1, -1, 8_388_607, -8_388_608, 123_456].to_vec(),
        ),
        (FMT_S16, [0i32, 1, -1, 32_767, -32_768, 12_345].to_vec()),
    ] {
        let channels = codes.len() as u16;
        let config = cfg(format, channels, channels);
        let period = config.hw.period_frames as usize;
        let bytes = device_sample_bytes(format).unwrap();
        let mut device = vec![0u8; channels as usize * period * bytes];
        for (channel, code) in codes.iter().enumerate() {
            match format {
                FMT_S24 => put_s24(&mut device, channel, *code),
                FMT_S16 => put_u16le(&mut device, channel, *code as u16),
                _ => unreachable!(),
            }
        }
        let mut planar = vec![0f32; channels as usize * period];
        convert_capture(&config, &device, &mut planar).expect("roundtrip capture");
        // capture 归一化：整数码 / 2^(bits-1)（精确）。
        let scale = (1u32 << (bytes * 8 - 1)) as f32;
        for (channel, code) in codes.iter().enumerate() {
            assert_eq!(
                planar[channel * period],
                *code as f32 / scale,
                "capture channel {}",
                channel
            );
        }
        let mut back = vec![0u8; channels as usize * period * bytes];
        convert_playback(&config, &planar, &mut back).expect("roundtrip playback");
        for (channel, code) in codes.iter().enumerate() {
            match format {
                FMT_S24 => assert_eq!(get_s24(&back, channel), *code, "s24 channel {}", channel),
                FMT_S16 => assert_eq!(
                    get_u16le(&back, channel),
                    *code as u16,
                    "s16 channel {}",
                    channel
                ),
                _ => unreachable!(),
            }
        }
        // 其余帧必须仍是静音 0（roundtrip 不得污染）。
        for frame in 1..period {
            assert_eq!(
                planar[frame], 0.0,
                "planar frame {} must stay silent",
                frame
            );
            for channel in 0..channels as usize {
                let index = frame * channels as usize + channel;
                let code = match format {
                    FMT_S24 => get_s24(&back, index),
                    FMT_S16 => get_u16le(&back, index) as i32,
                    _ => unreachable!(),
                };
                assert_eq!(code, 0, "frame {} channel {}", frame, channel);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// F. dither：配置一致性 / 确定性 / 统计 / 状态
// ---------------------------------------------------------------------------

#[test]
fn t22_dither_config_consistency_matrix() {
    let enabled = HW_FLAG_DITHER_ENABLED;
    let tpdf = HW_FLAG_DITHER_TPDF;
    // 一致：Off + flags 0；Tpdf + flags 两位。
    let base = hw(FMT_S24, 2, 2);
    assert_eq!(
        effective_dither(&cfg_of(base, DitherCfg::Off)),
        Ok(DitherCfg::Off)
    );
    let mut dither_hw = base;
    dither_hw.flags = enabled | tpdf;
    assert_eq!(
        effective_dither(&cfg_of(dither_hw, DitherCfg::Tpdf)),
        Ok(DitherCfg::Tpdf)
    );
    // 不一致组合全部 ERR_DEVICE_PARAMS（118）。
    let mut only_enabled = base;
    only_enabled.flags = enabled;
    assert_eq!(
        effective_dither(&cfg_of(only_enabled, DitherCfg::Tpdf)),
        Err(RtStatus::ERR_DEVICE_PARAMS)
    );
    let mut only_tpdf = base;
    only_tpdf.flags = tpdf;
    assert_eq!(
        effective_dither(&cfg_of(only_tpdf, DitherCfg::Off)),
        Err(RtStatus::ERR_DEVICE_PARAMS)
    );
    assert_eq!(
        effective_dither(&cfg_of(dither_hw, DitherCfg::Off)),
        Err(RtStatus::ERR_DEVICE_PARAMS)
    );
    assert_eq!(
        effective_dither(&cfg_of(base, DitherCfg::Tpdf)),
        Err(RtStatus::ERR_DEVICE_PARAMS)
    );
    // F32 设备格式 + dither 无定义（f32 无量化）。
    let mut f32_dither = hw(FMT_F32I, 2, 2);
    f32_dither.flags = enabled | tpdf;
    assert_eq!(
        effective_dither(&cfg_of(f32_dither, DitherCfg::Tpdf)),
        Err(RtStatus::ERR_DEVICE_PARAMS)
    );
    // 配置错误必须在转换入口返回（不是静默忽略）。
    assert_eq!(
        convert_playback(
            &cfg_of(only_enabled, DitherCfg::Tpdf),
            &[0f32; 128],
            &mut [0u8; 384]
        ),
        Err(RtStatus::ERR_DEVICE_PARAMS)
    );
}

#[test]
fn t22_dither_off_is_bit_exact() {
    let cfg = cfg(FMT_S24, 1, 1);
    let src = [0.25f32; 64];
    let mut first = [0u8; 64 * 3];
    let mut second = [0u8; 64 * 3];
    convert_playback(&cfg, &src, &mut first).expect("first");
    convert_playback(&cfg, &src, &mut second).expect("second");
    assert_eq!(first, second);
    for frame in 0..64 {
        assert_eq!(get_s24(&first, frame), 2_097_152); // 0.25 × 2^23
    }
    // capture 方向在 dither 配置存在时仍不施加 dither（dither 只属 playback）。
    let mut capture_hw = hw(FMT_S24, 1, 1);
    capture_hw.flags = HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF;
    let capture_cfg = cfg_of(capture_hw, DitherCfg::Off);
    let mut device = [0u8; 64 * 3];
    put_s24(&mut device, 0, 1_234);
    let mut planar = [0f32; 64];
    let stats =
        convert_capture(&capture_cfg, &device, &mut planar).expect("capture ignores dither");
    assert!(stats.is_clean());
    assert_eq!(planar[0], 1_234.0 / 8_388_608.0);
}

#[test]
fn t22_dither_stateless_repeats_and_seed_sensitive() {
    let mut dither_hw = hw_with(
        FMT_S24,
        1,
        1,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        0x1234_5678_9ABC_DEF0,
    );
    let cfg = cfg_dither(dither_hw);
    let src = [0.25f32; 64];
    let mut first = [0u8; 64 * 3];
    let mut second = [0u8; 64 * 3];
    convert_playback(&cfg, &src, &mut first).expect("first");
    convert_playback(&cfg, &src, &mut second).expect("second");
    // 无状态入口同种子逐位一致（参考向量/离线工具语义）。
    assert_eq!(first, second);
    // 噪声确实改变了最低位（0.25 × 2^23 附近抖动）。
    let mut changed = 0usize;
    for frame in 0..64 {
        if get_s24(&first, frame) != 2_097_152 {
            changed += 1;
        }
    }
    assert!(changed > 0, "dither must perturb LSBs");
    // 换种子 -> 序列不同。
    dither_hw.reserved[DITHER_SEED_RESERVED_INDEX] = 0x0FED_CBA9_8765_4321;
    let mut other = [0u8; 64 * 3];
    convert_playback(&cfg_dither(dither_hw), &src, &mut other).expect("other seed");
    assert_ne!(first, other);
    // 种子 0 与 1 也必须产生不同序列（SplitMix64 去低熵）。
    let zero = cfg_dither(hw_with(
        FMT_S24,
        1,
        1,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        0,
    ));
    let one = cfg_dither(hw_with(
        FMT_S24,
        1,
        1,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        1,
    ));
    let mut a = [0u8; 64 * 3];
    let mut b = [0u8; 64 * 3];
    convert_playback(&zero, &[0f32; 64], &mut a).expect("seed 0");
    convert_playback(&one, &[0f32; 64], &mut b).expect("seed 1");
    assert_ne!(a, b);
}

#[test]
fn t22_dither_stateful_advances_across_periods() {
    let cfg = cfg_dither(hw_with(
        FMT_S24,
        1,
        1,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        42,
    ));
    let src = [0.0f32; 64];
    let mut state = DitherState::for_config(&cfg);
    assert_eq!(state.seed(), 42);
    let mut first = [0u8; 64 * 3];
    let mut second = [0u8; 64 * 3];
    convert_playback_stateful(&cfg, &mut state, &src, &mut first).expect("first period");
    convert_playback_stateful(&cfg, &mut state, &src, &mut second).expect("second period");
    // 连续流：第二个 period 的噪声序列必须继续，不是从头重复。
    assert_ne!(first, second, "stateful dither must advance across periods");
    // dither 关闭时不推进状态。
    let off_cfg = cfg_of(hw(FMT_S24, 1, 1), DitherCfg::Off);
    let mut off_state = DitherState::new(7);
    let before = off_state;
    let mut off_block = [0u8; 64 * 3];
    convert_playback_stateful(&off_cfg, &mut off_state, &[0.5f32; 64], &mut off_block)
        .expect("off");
    assert_eq!(off_state, before, "dither off must not advance state");
    assert_eq!(get_s24(&off_block, 0), 4_194_304);
    // 重新播种回到同一序列起点。
    let mut reseeded = DitherState::new(42);
    let mut third = [0u8; 64 * 3];
    convert_playback_stateful(&cfg, &mut reseeded, &src, &mut third).expect("reseed");
    assert_eq!(first, third);
    reseeded.reseed(42);
    assert_eq!(reseeded, DitherState::new(42));
}

#[test]
fn t22_dither_tpdf_statistics() {
    // 零输入、S24、n_out=2、period=64：解码整数码即 dither 的取整结果。
    let cfg = cfg_dither(hw_with(
        FMT_S24,
        1,
        2,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        0xDEAD_BEEF_0BAD_F00D,
    ));
    let src = [0.0f32; 128];
    let mut state = DitherState::for_config(&cfg);
    let mut device = [0u8; 128 * 3];
    let mut total = 0usize;
    let mut min_code = 0i32;
    let mut max_code = 0i32;
    let mut sum = 0i64;
    let mut sum_sq = 0i64;
    let mut count_minus = 0usize;
    let mut count_zero = 0usize;
    let mut count_plus = 0usize;
    for _ in 0..32 {
        let stats = convert_playback_stateful(&cfg, &mut state, &src, &mut device).expect("dither");
        assert!(stats.is_clean(), "dither must not clip or sanitize");
        for index in 0..128 {
            let code = get_s24(&device, index);
            total += 1;
            min_code = min_code.min(code);
            max_code = max_code.max(code);
            sum += code as i64;
            sum_sq += (code as i64) * (code as i64);
            match code {
                -1 => count_minus += 1,
                0 => count_zero += 1,
                1 => count_plus += 1,
                _ => panic!("TPDF rounded code out of -1/0/1: {}", code),
            }
        }
    }
    assert_eq!(total, 4096);
    // TPDF 取整后为三值抖动：|d| <= 1 LSB，无越界。
    assert_eq!(min_code, -1);
    assert_eq!(max_code, 1);
    // 一阶矩为 0（无 DC 偏置）：均值 = 0 且误差在 3σ 内（σ=0.0078）。
    let mean = sum as f64 / total as f64;
    assert!(mean.abs() < 0.03, "dither mean must be ~0, got {}", mean);
    // 二阶矩：P(±1)=1/8、P(0)=3/4 -> 方差 0.25。
    let variance = sum_sq as f64 / total as f64 - mean * mean;
    assert!((variance - 0.25).abs() < 0.03, "variance {}", variance);
    let p_minus = count_minus as f64 / total as f64;
    let p_zero = count_zero as f64 / total as f64;
    let p_plus = count_plus as f64 / total as f64;
    assert!((p_minus - 0.125).abs() < 0.02, "P(-1) {}", p_minus);
    assert!((p_zero - 0.75).abs() < 0.02, "P(0) {}", p_zero);
    assert!((p_plus - 0.125).abs() < 0.02, "P(+1) {}", p_plus);
    // 无符号偏置（正负概率对称）。
    assert!((p_minus - p_plus).abs() < 0.02);
}

#[test]
fn t22_dither_applies_to_all_integer_formats() {
    for format in [FMT_S24, FMT_S16, FMT_S32] {
        let cfg = cfg_dither(hw_with(
            format,
            1,
            1,
            48_000,
            64,
            256,
            HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
            99,
        ));
        let src = [0.0f32; 64];
        let mut off = cfg_of(hw(format, 1, 1), DitherCfg::Off);
        off.hw.flags = 0;
        let bytes = device_sample_bytes(format).unwrap();
        let mut with_dither = vec![0u8; 64 * bytes];
        let mut without = vec![0u8; 64 * bytes];
        convert_playback(&cfg, &src, &mut with_dither).expect("dither on");
        convert_playback(&off, &src, &mut without).expect("dither off");
        assert_ne!(with_dither, without, "format {} must be dithered", format);
    }
}

#[test]
fn t22_dither_s32_high_level_absorbed_by_f32_precision() {
    // S32 量化步长 2^-31 远小于 f32 在 0.5 附近的间距 2^-24：高声压时 dither 被 f32
    // 精度吸收（这是 core 只有 24-bit 有效位的物理上限，不伪造改善）。
    let cfg = cfg_dither(hw_with(
        FMT_S32,
        1,
        1,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        7,
    ));
    let mut src = [0.5f32; 64];
    // 静音段仍可抖动（v 小，f32 能表达 LSB）。
    for frame in 0..8 {
        src[frame] = 0.0;
    }
    let mut with_dither = [0u8; 64 * 4];
    let mut without = [0u8; 64 * 4];
    convert_playback(&cfg, &src, &mut with_dither).expect("dither on");
    convert_playback(
        &cfg_of(hw(FMT_S32, 1, 1), DitherCfg::Off),
        &src,
        &mut without,
    )
    .expect("off");
    // 0.5 段的 56 个样本：dither 被吸收 -> 逐位相同。
    for frame in 8..64 {
        assert_eq!(
            get_u32le(&with_dither, frame),
            get_u32le(&without, frame),
            "S32 0.5 dither must be absorbed at frame {}",
            frame
        );
    }
    // 静音段：至少一个最低位被抖动改变。
    let mut changed = 0usize;
    for frame in 0..8 {
        if get_u32le(&with_dither, frame) != get_u32le(&without, frame) {
            changed += 1;
        }
    }
    assert!(changed > 0, "S32 silence must be dithered");
}

#[test]
fn t22_dither_golden_sequence_pinned() {
    // 独立参考实现 evidence/T22/gen-dither-golden.py（numpy.float32 逐步模拟 IEEE-754
    // binary32；同一 PCG32/SplitMix64/TPDF/round-ties-even 定义）生成下列冻结向量。
    // F-43 要求同种子 + 同输入 + 同调用序逐位一致：这些常量把 T22 的 dither 序列
    // 冻结成跨版本/跨实现可核对的糖边界参考向量（若实现改动，本测试必须显式更新）。
    const GOLDEN_SEED42_S24_ZERO: [i32; 64] = [
        0, 1, 0, 0, 1, -1, 1, 0, 0, 1, -1, -1, 0, 0, -1, 0, 0, 0, 0, -1, 0, 1, 0, 0, 0, 0, -1, 0,
        0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, -1, 0,
        -1, 0, 0, 0, 0, 0,
    ];
    const GOLDEN_DEADBEEF_S24_ZERO: [i32; 64] = [
        -1, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1, 0, 0, 0, 0, 0, 1, 1,
        0, 0, 0, 1, 0, 0, 0, -1, 0, 1, -1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, -1, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 1, 0, -1, 0,
    ];
    const GOLDEN_SEED42_S24_QUARTER: [i32; 64] = [
        2_097_152, 2_097_153, 2_097_152, 2_097_152, 2_097_153, 2_097_151, 2_097_153, 2_097_152,
        2_097_152, 2_097_153, 2_097_151, 2_097_152, 2_097_152, 2_097_152, 2_097_151, 2_097_152,
        2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_153, 2_097_152, 2_097_152,
        2_097_152, 2_097_152, 2_097_151, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_153,
        2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152,
        2_097_152, 2_097_152, 2_097_152, 2_097_153, 2_097_152, 2_097_152, 2_097_152, 2_097_153,
        2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152,
        2_097_151, 2_097_152, 2_097_151, 2_097_152, 2_097_152, 2_097_152, 2_097_152, 2_097_152,
    ];
    const GOLDEN_SEED99_S16_ZERO: [i32; 32] = [
        0, 0, 0, 0, 1, 0, 0, 0, 0, -1, 0, 0, -1, 0, 0, 0, 0, 0, 0, -1, 0, 1, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0,
    ];

    let s24 = |seed: u64, src: &[f32]| -> Vec<i32> {
        let cfg = cfg_dither(hw_with(
            FMT_S24,
            1,
            1,
            48_000,
            64,
            256,
            HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
            seed,
        ));
        let mut device = [0u8; 64 * 3];
        convert_playback(&cfg, src, &mut device).expect("golden s24");
        (0..64).map(|index| get_s24(&device, index)).collect()
    };
    assert_eq!(
        s24(42, &[0.0f32; 64]).as_slice(),
        GOLDEN_SEED42_S24_ZERO,
        "seed=42 S24 0.0 golden vector"
    );
    assert_eq!(
        s24(0xDEAD_BEEF_0BAD_F00D, &[0.0f32; 64]).as_slice(),
        GOLDEN_DEADBEEF_S24_ZERO,
        "seed=0xDEADBEEF0BADF00D S24 0.0 golden vector"
    );
    assert_eq!(
        s24(42, &[0.25f32; 64]).as_slice(),
        GOLDEN_SEED42_S24_QUARTER,
        "seed=42 S24 0.25 golden vector"
    );
    // S16：同种子但量化 scale 不同（2^15）；向量同样由 Python 参考实现生成。
    let cfg16 = cfg_dither(hw_with(
        FMT_S16,
        1,
        1,
        48_000,
        64,
        256,
        HW_FLAG_DITHER_ENABLED | HW_FLAG_DITHER_TPDF,
        99,
    ));
    let mut device16 = [0u8; 64 * 2];
    convert_playback(&cfg16, &[0.0f32; 64], &mut device16).expect("golden s16");
    let got16: Vec<i32> = (0..32)
        .map(|index| get_u16le(&device16, index) as i16 as i32)
        .collect();
    assert_eq!(
        got16.as_slice(),
        GOLDEN_SEED99_S16_ZERO,
        "seed=99 S16 golden vector"
    );
}

// ---------------------------------------------------------------------------
// G. planar 缓冲契约（16 样本倍数 / 64 B 基址对齐）
// ---------------------------------------------------------------------------

#[test]
fn t22_validate_planar_buffer_alignment_contract() {
    let mut buffer = Aligned64([0f32; 512]);
    let base = buffer.0.as_ptr() as usize;
    assert_eq!(base % 64, 0, "Aligned64 base must be 64B aligned");
    // stride 64（16 的倍数）与基址 64B -> 全部通道基址 64B 对齐（64 × 4 = 256 B）。
    assert!(planar_channels_aligned(base, 64));
    assert!(planar_channels_aligned(base, 80)); // 80 × 4 = 320 = 5 × 64
    assert_eq!(validate_planar_buffer(base, 512, 4, 64, 80), Ok(()));
    // 非 16 倍数 stride -> ERR_STRIDE_ALIGN
    assert_eq!(
        validate_planar_buffer(base, 512, 4, 64, 72),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // 基址不对齐 -> ERR_STRIDE_ALIGN
    assert_eq!(
        validate_planar_buffer(base + 4, 512, 4, 64, 64),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // 长度不足 -> ERR_STRIDE_ALIGN
    assert_eq!(
        validate_planar_buffer(base, 100, 4, 64, 64),
        Err(RtStatus::ERR_STRIDE_ALIGN)
    );
    // 转换入口在 64B 对齐缓冲 + stride 80 下正常工作。
    let config = cfg(FMT_S16, 4, 4);
    let mut device = [0u8; 64 * 4 * 2];
    for frame in 0..64 {
        for channel in 0..4 {
            put_u16le(&mut device, frame * 4 + channel, (channel as u16) << 12);
        }
    }
    let stats = convert_capture_strided(&config, &device, &mut buffer.0[..], 80).expect("aligned");
    assert!(stats.is_clean());
    assert_eq!(buffer.0[0], 0.0);
    assert_eq!(buffer.0[3 * 80], 3.0 * 4096.0 / 32_768.0);
}

// ---------------------------------------------------------------------------
// H. 与 T17 协商 actual 的联动（MOTU 20ch/22ch S24 48k）
// ---------------------------------------------------------------------------

#[test]
fn t22_t17_negotiated_motu_actual_matches_t01_e1() {
    let mut session = DuplexSession::offline(
        FakeBackend::motu_ultralite_mk5(),
        DuplexRequest::motu_baseline(),
    );
    let actual = session
        .start(&DeviceSelector::by_id("UltraLitemk5", 0), CARDS_MOTU)
        .expect("T17 fake start");
    let hw = actual.hw;
    // T01 E1 实测：20ch capture / 22ch playback / S24_3LE @48k / period 128 / buffer 1024。
    assert_eq!(hw.sample_format, FMT_S24);
    assert_eq!(hw.n_in, 20);
    assert_eq!(hw.n_out, 22);
    assert_eq!(hw.sample_rate, 48_000);
    assert_eq!(hw.period_frames, 128);
    assert_eq!(hw.buffer_frames, 1024);
    assert_eq!(hw.in_stride_bytes, 128 * 20 * 3);
    assert_eq!(hw.out_stride_bytes, 128 * 22 * 3);
    assert_eq!(hw.flags, HW_FLAG_CAPTURE_SANITIZE);
    // T17 actual 必须被 T22 validate 接受（跨任务握手）。
    let layout = validate(&FormatConfig {
        hw,
        dither: DitherCfg::Off,
    })
    .expect("T22 accepts T17 actual");
    assert_eq!(layout.in_min_bytes, 7680);
    assert_eq!(layout.out_min_bytes, 8448);
    assert_eq!(layout.period_frames, 128);
    assert_eq!(layout.n_in, 20);
    assert_eq!(layout.n_out, 22);
    // 真实 period 字节 = T17 DuplexActual::period_bytes（同一口径，不漂移）。
    use crate::alsa::StreamDirection;
    assert_eq!(
        actual.period_bytes(StreamDirection::Capture).unwrap(),
        layout.in_min_bytes
    );
    assert_eq!(
        actual.period_bytes(StreamDirection::Playback).unwrap(),
        layout.out_min_bytes
    );
    // 用 T17 actual 直接做一次 capture/playback 全链。
    let config = FormatConfig {
        hw,
        dither: DitherCfg::Off,
    };
    let mut device_in = [0u8; 7680];
    put_s24(&mut device_in, 0, 0x7F_FFFF);
    // interleaved sample index = frame * 20 + channel；frame 0 / channel 12 = 12。
    put_s24(&mut device_in, 12, -1); // Toslink L 第一帧
    let mut planar = [0f32; 128 * 20];
    let stats = convert_capture(&config, &device_in, &mut planar).expect("motu capture");
    assert!(stats.is_clean());
    assert_eq!(planar[0], 1.0 - 1.0 / 8_388_608.0);
    assert_eq!(planar[12 * 128], -1.0 / 8_388_608.0);
    let mut device_out = [0u8; 8448];
    let mut planar_out = [0f32; 128 * 22];
    planar_out[0] = 1.0;
    planar_out[21 * 128 + 127] = -1.0;
    let stats = convert_playback(&config, &planar_out, &mut device_out).expect("motu playback");
    assert!(stats.is_clean());
    assert_eq!(get_s24(&device_out, 0), 8_388_607);
    assert_eq!(get_s24(&device_out, 128 * 22 - 1), -8_388_608);
}

// ---------------------------------------------------------------------------
// I. 统计累加 / 无分配与 RT 纪律自证
// ---------------------------------------------------------------------------

#[test]
fn t22_stats_merge_and_clean_flag() {
    let mut total = FormatStats::default();
    assert!(total.is_clean());
    total.merge(FormatStats {
        nonfinite_in: 2,
        clipped: 1,
        nonfinite_out: 0,
    });
    total.merge(FormatStats {
        nonfinite_in: 3,
        clipped: 4,
        nonfinite_out: 5,
    });
    assert_eq!(total.nonfinite_in, 5);
    assert_eq!(total.clipped, 5);
    assert_eq!(total.nonfinite_out, 5);
    assert!(!total.is_clean());
    // 饱和累加不溢出。
    let mut saturated = FormatStats {
        nonfinite_in: u64::MAX,
        clipped: u64::MAX,
        nonfinite_out: u64::MAX,
    };
    saturated.merge(FormatStats {
        nonfinite_in: 1,
        clipped: 1,
        nonfinite_out: 1,
    });
    assert_eq!(saturated.nonfinite_in, u64::MAX);
}

#[test]
fn t22_module_has_no_allocation_or_syscall_tokens() {
    // 源码级自证：[IO] 区实现无堆分配/锁/syscall/文件/网络/日志/unsafe/主动 panic。
    let source = include_str!("../../src/format.rs");
    let forbidden = [
        "Vec::new",
        "vec![",
        "Vec<",
        "Box<",
        "String",
        "format!(",
        "to_string",
        "std::",
        "alloc::",
        "unsafe",
        "extern \"C\"",
        "println!",
        "eprintln!",
        "thread::",
        "fs::",
        "File::",
        "Mutex",
        "RwLock",
        "HashMap",
        "Arc<",
        "Rc<",
        "Cell<",
        "panic!",
        ".unwrap()",
        ".expect(",
    ];
    for token in forbidden {
        assert!(
            !source.contains(token),
            "format.rs must not contain forbidden token {}",
            token
        );
    }
    // I/O 区标记存在；不是 DSP RT-MODULE（否则 forbid 扫描语义不同）。
    assert!(source.contains("[IO]"));
    assert!(!source.contains("RT-MODULE:"));
    // 测试注册走 cfg(test) include!（不写 public_registration.rs）。
    assert!(source.contains("include!(\"../tests/T22/format_vectors.rs\")"));
}

// ---------------------------------------------------------------------------
// J. 边界/健壮性：空缓冲、最大通道、128 帧
// ---------------------------------------------------------------------------

#[test]
fn t22_edge_buffers_do_not_panic() {
    let config = cfg(FMT_S24, 2, 2);
    // 空 src / 空 dst：返回错误而不是 panic。
    assert!(convert_capture(&config, &[], &mut [0f32; 128]).is_err());
    assert!(convert_capture(&config, &[0u8; 384], &mut []).is_err());
    assert!(convert_playback(&config, &[], &mut [0u8; 384]).is_err());
    assert!(convert_playback(&config, &[0f32; 128], &mut []).is_err());
    // 超长缓冲（> 一个 period）只消费/写一个 period，其余不动。
    let mut src = [0u8; 64 * 2 * 3 + 30];
    put_s24(&mut src, 0, 100);
    let mut planar = [f32::NAN; 128 + 8];
    convert_capture(&config, &src, &mut planar).expect("oversized src");
    assert_eq!(planar[0], 100.0 / 8_388_608.0);
    assert!(
        planar[128].is_nan(),
        "capture wrote past dense planar period"
    );
    let mut dst = [0u8; 64 * 2 * 3 + 30];
    let mut src_planar = [0f32; 128];
    src_planar[0] = 0.5;
    convert_playback(&config, &src_planar, &mut dst).expect("oversized dst");
    assert_eq!(get_s24(&dst, 0), 4_194_304);
    for byte in &dst[64 * 2 * 3..] {
        assert_eq!(*byte, 0, "playback wrote past one interleaved period");
    }
}

#[test]
fn t22_max_channels_128_frames_all_formats() {
    // 128 帧 × 22 输出 / 20 输入 × 全设备格式：不 panic、无 clip、无 nonfinite。
    for format in [FMT_S24, FMT_S32, FMT_S16, FMT_F32I] {
        let hw = hw_with(format, 20, 22, 48_000, 128, 1024, 0, 0);
        let config = cfg_of(hw, DitherCfg::Off);
        let bytes = device_sample_bytes(format).unwrap();
        let mut device_in = vec![0u8; 128 * 20 * bytes];
        // F32 默认 0.0；整数格式默认 0 即可（静音）。
        if format == FMT_F32I {
            for index in 0..128 * 20 {
                put_u32le(&mut device_in, index, 0.25f32.to_bits());
            }
        }
        let mut planar_in = vec![0f32; 128 * 20];
        let stats = convert_capture(&config, &device_in, &mut planar_in).expect("capture 128");
        assert!(
            stats.is_clean(),
            "format {} capture stats {:?}",
            format,
            stats
        );
        if format == FMT_F32I {
            assert_eq!(planar_in[0], 0.25);
        }
        let mut planar_out = vec![0f32; 128 * 22];
        for value in planar_out.iter_mut() {
            *value = -0.5;
        }
        let mut device_out = vec![0u8; 128 * 22 * bytes];
        let stats = convert_playback(&config, &planar_out, &mut device_out).expect("playback 128");
        assert!(
            stats.is_clean(),
            "format {} playback stats {:?}",
            format,
            stats
        );
    }
}
