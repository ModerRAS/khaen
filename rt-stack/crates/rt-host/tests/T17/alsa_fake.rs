// T17（alsa）离线模型测试：duplex 协商 / actual 参数 / 启动恢复状态机 / owner / 硬件闸门。
//
// 本文件经 src/alsa.rs 末尾的 cfg(test) mod alsa_fake_tests { include!(...) } 进入单测
// 构建（cargo 不自动发现 tests/<子目录>；公共注册属 integrator）。运行：
//   cargo test -p khaen-rt-host alsa_fake
// 全部测试名 t17_ 前缀；模块路径含 alsa_fake_tests，可按 alsa_fake 过滤。
// 离线：只用 FakeBackend，不打开 /dev/snd，不需要声卡。

use super::*;
use khaen_core::abi::{sample_format, TspiHwParamsV1, ABI_VERSION};
use khaen_core::rt::RtStatus;

const S24: u16 = sample_format::S24_3LE_INTERLEAVED;
const S32: u16 = sample_format::S32_LE_INTERLEAVED;
const F32I: u16 = sample_format::F32_INTERLEAVED;
const F32P: u16 = sample_format::F32_PLANAR;
const F64: u16 = sample_format::F64_PLANAR_REFERENCE;

/// /proc/asound/cards 样本（MOTU = card 4，UMIK = card 3，PCH = 0）。
const CARDS_MOTU: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                      HDA Intel PCH at 0x0000 irq 130
 3 [UMIK           ]: USB-Audio - Umik-1
 4 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5
                      MOTU UltraLite-mk5 at usb-0000:01:00.0-1.4, full speed
";

fn motu_sel() -> DeviceSelector<'static> {
    DeviceSelector::by_id("UltraLitemk5", 0)
}

fn motu_caps() -> (StreamCaps, StreamCaps) {
    let fake = FakeBackend::motu_ultralite_mk5();
    (fake.capture_caps, fake.playback_caps)
}

fn caps_pair() -> (StreamCaps, StreamCaps) {
    motu_caps()
}

fn stream_actual(channels: u16, format: u16, rate: u32, period: u32, buffer: u32) -> StreamActual {
    StreamActual {
        channels,
        sample_format: format,
        sample_rate: rate,
        period_frames: period,
        buffer_frames: buffer,
    }
}

fn raw_actual(capture: StreamActual, playback: StreamActual) -> RawActual {
    RawActual { capture, playback }
}

/// T01 E1 实测 actual：cap 20ch / pb 22ch、S24_3LE、48k、period128、buffer1024。
fn motu_raw() -> RawActual {
    raw_actual(
        stream_actual(20, S24, 48_000, 128, 1024),
        stream_actual(22, S24, 48_000, 128, 1024),
    )
}

fn motu_plan() -> HwParamsPlan {
    HwParamsPlan {
        sample_rate: 48_000,
        sample_format: S24,
        period_frames: 128,
        buffer_frames: 1024,
        capture_channels: 20,
        playback_channels: 22,
    }
}

fn make_session() -> DuplexSession<FakeBackend> {
    DuplexSession::offline(
        FakeBackend::motu_ultralite_mk5(),
        DuplexRequest::motu_baseline(),
    )
}

fn started() -> (DuplexSession<FakeBackend>, DuplexActual) {
    let mut session = make_session();
    let actual = session
        .start(&motu_sel(), CARDS_MOTU)
        .expect("start should succeed on fake MOTU");
    (session, actual)
}

fn has_kind(session: &DuplexSession<FakeBackend>, kind: TransitionKind) -> bool {
    session.transitions().iter().any(|item| item.kind == kind)
}

fn count_kind(session: &DuplexSession<FakeBackend>, kind: TransitionKind) -> usize {
    session
        .transitions()
        .iter()
        .filter(|item| item.kind == kind)
        .count()
}

fn cap_buf(actual: &DuplexActual) -> Vec<u8> {
    vec![0u8; actual.period_bytes(StreamDirection::Capture).unwrap()]
}

fn pb_buf(actual: &DuplexActual) -> Vec<u8> {
    vec![0u8; actual.period_bytes(StreamDirection::Playback).unwrap()]
}

fn cal_hw() -> TspiHwParamsV1 {
    TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: S24,
        n_in: 20,
        n_out: 22,
        sample_rate: 48_000,
        period_frames: 64,
        buffer_frames: 512,
        in_stride_bytes: 20 * 64 * 3,
        out_stride_bytes: 22 * 64 * 3,
        flags: 0,
        reserved: [0; 4],
    }
}

// ---------------------------------------------------------------------------
// A. 稳定设备身份（§5.4：禁止写死 card4）
// ---------------------------------------------------------------------------

#[test]
fn t17_cards_parse_motu_entry_fields() {
    let entry = parse_card_entry(" 4 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5")
        .expect("motu card line");
    assert_eq!(entry.index, 4);
    assert_eq!(entry.id, "UltraLitemk5");
    assert_eq!(entry.driver, "USB-Audio");
    assert_eq!(entry.name, "UltraLite-mk5");
}

#[test]
fn t17_cards_parse_skips_headers_and_continuations() {
    assert!(parse_card_entry("--- no soundcards ---").is_none());
    assert!(parse_card_entry("                      MOTU UltraLite-mk5 at usb-x").is_none());
    assert!(parse_card_entry("").is_none());
    assert!(parse_card_entry(" 5 [NoDriver]").is_none());
}

#[test]
fn t17_cards_find_by_id_uses_parsed_index_not_hardcoded() {
    assert_eq!(
        find_card_index(CARDS_MOTU, CardLookup::ById("UltraLitemk5")),
        Ok(4)
    );
    // 同一 id 换一个 index 也必须按文本解析（禁止假 card4）。
    let moved = " 7 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n";
    assert_eq!(
        find_card_index(moved, CardLookup::ById("UltraLitemk5")),
        Ok(7)
    );
    assert_eq!(find_card_index(CARDS_MOTU, CardLookup::ById("UMIK")), Ok(3));
}

#[test]
fn t17_cards_find_by_name_falls_back_to_id() {
    assert_eq!(
        find_card_index(CARDS_MOTU, CardLookup::ByName("UltraLite-mk5")),
        Ok(4)
    );
    assert_eq!(
        find_card_index(CARDS_MOTU, CardLookup::ByName("UltraLitemk5")),
        Ok(4)
    );
}

#[test]
fn t17_cards_find_missing_and_ambiguous() {
    assert_eq!(
        find_card_index(CARDS_MOTU, CardLookup::ById("MOTU-NOT-THERE")),
        Err(CardLookupError::NotFound)
    );
    let duplicated = " 2 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n 5 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n";
    assert_eq!(
        find_card_index(duplicated, CardLookup::ById("UltraLitemk5")),
        Err(CardLookupError::Ambiguous { count: 2 })
    );
}

#[test]
fn t17_resolved_device_write_hw_name() {
    let device = ResolvedDevice {
        card_index: 4,
        device_index: 0,
    };
    let mut buffer = [0u8; 16];
    let len = device.write_hw_name(&mut buffer).expect("hw name");
    assert_eq!(&buffer[..len], b"hw:4,0");
    let mut tiny = [0u8; 3];
    assert_eq!(device.write_hw_name(&mut tiny), Err(AlsaError::BadLayout));
}

#[test]
fn t17_resolved_device_from_cards_errors() {
    let device = ResolvedDevice::from_cards(CARDS_MOTU, &motu_sel()).expect("resolve motu");
    assert_eq!(device.card_index, 4);
    assert_eq!(device.device_index, 0);
    assert_eq!(
        ResolvedDevice::from_cards(CARDS_MOTU, &DeviceSelector::by_id("nope", 0)),
        Err(AlsaError::CardNotFound)
    );
}

// ---------------------------------------------------------------------------
// B. duplex 协商（period/buffer/format/rate/channels）
// ---------------------------------------------------------------------------

#[test]
fn t17_negotiate_motu_baseline_matches_t01_e1() {
    let (capture, playback) = caps_pair();
    let plan = negotiate(&capture, &playback, &DuplexRequest::motu_baseline()).expect("plan");
    assert_eq!(plan.sample_rate, 48_000);
    assert_eq!(plan.sample_format, S24);
    assert_eq!(plan.period_frames, 128);
    assert_eq!(plan.buffer_frames, 1024);
    assert_eq!(plan.capture_channels, 20);
    assert_eq!(plan.playback_channels, 22);
}

#[test]
fn t17_negotiate_format_priority_fallback() {
    let (mut capture, mut playback) = caps_pair();
    capture.format_mask = FORMAT_MASK_S32_LE | FORMAT_MASK_S16_LE;
    playback.format_mask = FORMAT_MASK_S32_LE | FORMAT_MASK_S16_LE;
    let plan = negotiate(&capture, &playback, &DuplexRequest::motu_baseline()).expect("plan");
    assert_eq!(plan.sample_format, S32);
}

#[test]
fn t17_negotiate_rejects_request_period_256_without_clamp() {
    let (capture, playback) = caps_pair();
    let request = DuplexRequest::new(48_000, 256, 1024);
    assert_eq!(
        negotiate(&capture, &playback, &request),
        Err(AlsaError::BlockSize)
    );
}

#[test]
fn t17_negotiate_rejects_unsupported_rate() {
    let (capture, playback) = caps_pair();
    assert_eq!(
        negotiate(&capture, &playback, &DuplexRequest::new(44_100, 128, 1024)),
        Err(AlsaError::Rate)
    );
    // 96k 不在 MOTU 基线 caps 中。
    assert_eq!(
        negotiate(&capture, &playback, &DuplexRequest::new(96_000, 128, 1024)),
        Err(AlsaError::Rate)
    );
}

#[test]
fn t17_negotiate_period_falls_back_to_64() {
    let (mut capture, mut playback) = caps_pair();
    capture.period = ValueRange::new(64, 64, 1);
    playback.period = ValueRange::new(64, 64, 1);
    let plan = negotiate(&capture, &playback, &DuplexRequest::motu_baseline()).expect("plan");
    assert_eq!(plan.period_frames, 64);
}

#[test]
fn t17_negotiate_buffer_rounds_up_to_period_multiple() {
    let (capture, playback) = caps_pair();
    let plan =
        negotiate(&capture, &playback, &DuplexRequest::new(48_000, 128, 1000)).expect("plan");
    assert_eq!(plan.buffer_frames, 1024);
    assert_eq!(plan.buffer_frames % plan.period_frames, 0);
}

#[test]
fn t17_negotiate_buffer_clamped_within_caps() {
    let (mut capture, mut playback) = caps_pair();
    capture.buffer = ValueRange::new(256, 512, 1);
    playback.buffer = ValueRange::new(256, 512, 1);
    let plan =
        negotiate(&capture, &playback, &DuplexRequest::new(48_000, 128, 4096)).expect("plan");
    assert_eq!(plan.buffer_frames, 512);
}

#[test]
fn t17_negotiate_rejects_insufficient_channels() {
    let (mut capture, mut playback) = caps_pair();
    capture.channels = ChannelRange::new(2, 2);
    playback.channels = ChannelRange::new(2, 2);
    let mut request = DuplexRequest::motu_baseline();
    request.min_input_channels = 4;
    assert_eq!(
        negotiate(&capture, &playback, &request),
        Err(AlsaError::Channels)
    );
}

#[test]
fn t17_negotiate_rejects_disjoint_formats() {
    let (mut capture, mut playback) = caps_pair();
    capture.format_mask = FORMAT_MASK_S16_LE;
    playback.format_mask = FORMAT_MASK_S32_LE;
    assert_eq!(
        negotiate(&capture, &playback, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Unsupported)
    );
}

#[test]
fn t17_negotiate_rejects_planar_or_non_interleaved_caps() {
    let (mut capture, playback) = caps_pair();
    capture.interleaved = false;
    assert_eq!(
        negotiate(&capture, &playback, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Unsupported)
    );
}

#[test]
fn t17_negotiate_rejects_no_common_period() {
    let (mut capture, mut playback) = caps_pair();
    capture.period = ValueRange::new(32, 32, 1);
    playback.period = ValueRange::new(32, 32, 1);
    assert_eq!(
        negotiate(&capture, &playback, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_negotiate_respects_period_step() {
    let (mut capture, mut playback) = caps_pair();
    capture.period = ValueRange::new(128, 2048, 128);
    playback.period = ValueRange::new(128, 2048, 128);
    let plan = negotiate(&capture, &playback, &DuplexRequest::motu_baseline()).expect("plan");
    assert_eq!(plan.period_frames, 128);
}

// ---------------------------------------------------------------------------
// C. actual 参数校验（I11: period 严格 64/128；stride/哈希/宽度）
// ---------------------------------------------------------------------------

#[test]
fn t17_validate_actual_builds_frozen_hwparams() {
    let actual = validate_actual(&motu_plan(), &motu_raw(), &DuplexRequest::motu_baseline())
        .expect("actual");
    assert_eq!(actual.hw.abi_version, ABI_VERSION);
    assert_eq!(actual.hw.sample_format, S24);
    assert_eq!(actual.hw.n_in, 20);
    assert_eq!(actual.hw.n_out, 22);
    assert_eq!(actual.hw.sample_rate, 48_000);
    assert_eq!(actual.hw.period_frames, 128);
    assert_eq!(actual.hw.buffer_frames, 1024);
    assert_eq!(actual.hw.in_stride_bytes, 20 * 128 * 3);
    assert_eq!(actual.hw.out_stride_bytes, 22 * 128 * 3);
    assert_eq!(actual.hw.flags & (1 << 2), 1 << 2);
    assert!(!actual.period_changed && !actual.buffer_changed);
    assert!(!actual.requires_new_epoch());
    assert_ne!(actual.hash, 0);
}

#[test]
fn t17_validate_actual_rejects_actual_period_256() {
    // core-v1 §6.1 I11 负向对照：actual period 256 必须 ERR_DEVICE_PARAMS，禁止夹取。
    let mut raw = motu_raw();
    raw.capture.period_frames = 256;
    raw.capture.buffer_frames = 2048;
    raw.playback.period_frames = 256;
    raw.playback.buffer_frames = 2048;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_validate_actual_rejects_actual_period_32() {
    let mut raw = motu_raw();
    raw.capture.period_frames = 32;
    raw.capture.buffer_frames = 256;
    raw.playback.period_frames = 32;
    raw.playback.buffer_frames = 256;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_validate_actual_rejects_rate_mismatch() {
    let mut raw = motu_raw();
    raw.capture.sample_rate = 44_100;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Rate)
    );
    let mut raw = motu_raw();
    raw.playback.sample_rate = 96_000;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Rate)
    );
}

#[test]
fn t17_validate_actual_rejects_capture_playback_period_mismatch() {
    let mut raw = motu_raw();
    raw.playback.period_frames = 64;
    raw.playback.buffer_frames = 1024;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_validate_actual_rejects_capture_playback_buffer_mismatch() {
    let mut raw = motu_raw();
    raw.playback.buffer_frames = 2048;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_validate_actual_rejects_shrunken_channels() {
    let mut raw = motu_raw();
    raw.capture.channels = 16;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Channels)
    );
    let mut raw = motu_raw();
    raw.playback.channels = 2;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Channels)
    );
}

#[test]
fn t17_validate_actual_rejects_channels_over_abi_max() {
    let mut raw = motu_raw();
    raw.capture.channels = 24;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Channels)
    );
    let mut raw = motu_raw();
    raw.playback.channels = 23;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Channels)
    );
    let mut raw = motu_raw();
    raw.capture.channels = 0;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Channels)
    );
}

#[test]
fn t17_validate_actual_rejects_planar_and_reference_formats() {
    let mut raw = motu_raw();
    raw.capture.sample_format = F32P;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Unsupported)
    );
    let mut raw = motu_raw();
    raw.playback.sample_format = F64;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Unsupported)
    );
}

#[test]
fn t17_validate_actual_accepts_f32_interleaved_only_for_file_fake() {
    let mut raw = motu_raw();
    raw.capture.sample_format = F32I;
    raw.playback.sample_format = F32I;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::Unsupported)
    );
    let mut request = DuplexRequest::motu_baseline();
    request.allow_file_fake_formats = true;
    let actual = validate_actual(&motu_plan(), &raw, &request).expect("f32 fake actual");
    assert_eq!(actual.hw.sample_format, F32I);
    assert_eq!(actual.hw.in_stride_bytes, 20 * 128 * 4);
}

#[test]
fn t17_validate_actual_rejects_bad_buffer() {
    let mut raw = motu_raw();
    raw.capture.buffer_frames = 64;
    raw.playback.buffer_frames = 64;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
    let mut raw = motu_raw();
    raw.capture.buffer_frames = 1000;
    raw.playback.buffer_frames = 1000;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_validate_actual_marks_period_change_requires_new_epoch() {
    let mut raw = motu_raw();
    raw.capture.period_frames = 64;
    raw.capture.buffer_frames = 512;
    raw.playback.period_frames = 64;
    raw.playback.buffer_frames = 512;
    let actual = validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline())
        .expect("period clamped to 64 is legal but needs new epoch");
    assert_eq!(actual.hw.period_frames, 64);
    assert!(actual.period_changed);
    assert!(actual.buffer_changed);
    assert!(actual.requires_new_epoch());
}

#[test]
fn t17_validate_actual_rejects_cross_direction_format_mismatch() {
    let mut raw = motu_raw();
    raw.playback.sample_format = S32;
    assert_eq!(
        validate_actual(&motu_plan(), &raw, &DuplexRequest::motu_baseline()),
        Err(AlsaError::DeviceParams)
    );
}

#[test]
fn t17_hwparams_hash_deterministic_and_sensitive() {
    let actual = validate_actual(&motu_plan(), &motu_raw(), &DuplexRequest::motu_baseline())
        .expect("actual");
    let mut same = actual.hw;
    same.reserved = [7; 4];
    assert_eq!(hwparams_hash(&same), actual.hash);
    let mut changed = actual.hw;
    changed.period_frames = 64;
    assert_ne!(hwparams_hash(&changed), actual.hash);
    let mut changed = actual.hw;
    changed.flags = 0;
    assert_ne!(hwparams_hash(&changed), actual.hash);
}

#[test]
fn t17_period_bytes_match_motu_evidence() {
    let actual = validate_actual(&motu_plan(), &motu_raw(), &DuplexRequest::motu_baseline())
        .expect("actual");
    assert_eq!(actual.period_bytes(StreamDirection::Capture).unwrap(), 7680);
    assert_eq!(
        actual.period_bytes(StreamDirection::Playback).unwrap(),
        8448
    );
    assert_eq!(actual.bytes_per_sample().unwrap(), 3);
}

#[test]
fn t17_validate_width_toslink_412() {
    let actual = validate_actual(&motu_plan(), &motu_raw(), &DuplexRequest::motu_baseline())
        .expect("actual");
    assert_eq!(
        actual.validate_width(&RequiredDeviceWidth::toslink_412()),
        Ok(())
    );
    let mut narrow = motu_raw();
    narrow.capture.channels = 12;
    let narrow = validate_actual(&mut motu_plan(), &narrow, &DuplexRequest::motu_baseline());
    assert!(narrow.is_err(), "12ch capture cannot serve Toslink ch12/13");
}

// ---------------------------------------------------------------------------
// D. 硬件闸门（S1 运行期不打开硬件）
// ---------------------------------------------------------------------------

#[test]
fn t17_hw_gate_defaults_denied() {
    assert_eq!(HwOpenGate::denied().policy(), HwPolicy::Denied);
    for value in [None, Some("0"), Some("false"), Some("no"), Some("garbage")] {
        let gate = HwOpenGate::from_env_value(value);
        assert_eq!(gate.policy(), HwPolicy::Denied, "value={value:?}");
        assert_eq!(gate.check(), Err(AlsaError::HwOpenDenied));
    }
    assert_eq!(HwOpenGate::default().policy(), HwPolicy::Denied);
}

#[test]
fn t17_hw_gate_allows_explicit_values() {
    for value in [
        Some("1"),
        Some("true"),
        Some("TRUE"),
        Some("yes"),
        Some("allow"),
    ] {
        let gate = HwOpenGate::from_env_value(value);
        assert_eq!(gate.policy(), HwPolicy::Allowed, "value={value:?}");
        assert_eq!(gate.check(), Ok(()));
    }
    assert_eq!(HwOpenGate::allowed().check(), Ok(()));
}

#[test]
fn t17_no_hw_backend_is_unavailable_even_if_gate_allows() {
    let mut backend = NoHwBackend;
    assert!(PcmDuplexBackend::is_hardware(&backend));
    assert_eq!(
        backend.open(&ResolvedDevice {
            card_index: 4,
            device_index: 0,
        }),
        Err(AlsaError::BackendUnavailable)
    );
    assert_eq!(backend.start(), Err(AlsaError::BackendUnavailable));
    assert_eq!(
        backend.stream_caps(StreamDirection::Capture),
        Err(AlsaError::BackendUnavailable)
    );
    backend.close();
}

#[test]
fn t17_session_hardware_gate_denied_fails_before_open() {
    let mut session = DuplexSession::new(
        NoHwBackend,
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::default(),
        HwOpenGate::denied(),
    );
    assert_eq!(
        session.start(&motu_sel(), CARDS_MOTU),
        Err(AlsaError::HwOpenDenied)
    );
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert!(!session.mute_required());
}

#[test]
fn t17_session_hardware_gate_allowed_but_binding_missing() {
    let mut session = DuplexSession::new(
        NoHwBackend,
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::default(),
        HwOpenGate::allowed(),
    );
    assert_eq!(
        session.start(&motu_sel(), CARDS_MOTU),
        Err(AlsaError::BackendUnavailable)
    );
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Idle);
}

#[test]
fn t17_fake_backend_is_offline_and_ignores_hw_gate() {
    let fake = FakeBackend::motu_ultralite_mk5();
    assert!(!PcmDuplexBackend::is_hardware(&fake));
    let mut session = DuplexSession::new(
        FakeBackend::motu_ultralite_mk5(),
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::default(),
        HwOpenGate::denied(),
    );
    assert!(session.start(&motu_sel(), CARDS_MOTU).is_ok());
}

#[test]
fn t17_source_has_no_hardware_open_tokens() {
    let source = include_str!("../../src/alsa.rs");
    let code = source
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                ""
            } else {
                match line.find("//") {
                    Some(position) => &line[..position],
                    None => line,
                }
            }
        })
        .collect::<String>();
    for token in [
        "libasound",
        "snd_pcm_",
        "/dev/snd",
        "dlopen",
        "extern \"C\"",
        "Command::new",
        "std::fs::",
        "File::open",
        "OpenOptions",
    ] {
        assert!(!code.contains(token), "forbidden hardware token: {token}");
    }
    // [NONRT]/[IO] 模块：不得声明为 RT-MODULE（否则会进 DSP forbid 扫描语义）。
    assert!(!code.contains("RT-MODULE:"));
    // T17 不写 unsafe/FFI。
    assert!(!code.contains("unsafe"));
}

// ---------------------------------------------------------------------------
// E. §5.4 DeviceOwner（互斥/交接/hash/超时）
// ---------------------------------------------------------------------------

#[test]
fn t17_owner_acquire_release_records_device_owner_change() {
    let mut owner = DeviceOwner::new();
    assert_eq!(owner.state(), OwnerState::Idle);
    let change = owner.acquire(OwnerKind::Playback).expect("acquire");
    assert_eq!(change.event, EV_DEVICE_OWNER_CHANGE);
    assert_eq!(change.from, None);
    assert_eq!(change.to, Some(OwnerKind::Playback));
    assert_eq!(owner.state(), OwnerState::Playback);
    assert_eq!(owner.current(), Some(OwnerKind::Playback));
    let change = owner.release().expect("release");
    assert_eq!(change.from, Some(OwnerKind::Playback));
    assert_eq!(change.to, None);
    assert_eq!(owner.state(), OwnerState::Idle);
    assert_eq!(owner.changes().len(), 2);
    assert_eq!(owner.release(), Err(AlsaError::SwitchBusy));
}

#[test]
fn t17_owner_rejects_concurrent_playback_and_calibration() {
    let mut owner = DeviceOwner::new();
    owner.acquire(OwnerKind::Playback).expect("playback");
    assert_eq!(
        owner.acquire(OwnerKind::Calibration),
        Err(AlsaError::DeviceBusy)
    );
    owner.release().expect("release");
    owner.acquire(OwnerKind::Calibration).expect("calibration");
    assert_eq!(
        owner.acquire(OwnerKind::Playback),
        Err(AlsaError::DeviceBusy)
    );
}

#[test]
fn t17_owner_handover_records_before_after_hashes() {
    let mut owner = DeviceOwner::new();
    owner.acquire(OwnerKind::Playback).expect("playback");
    assert_eq!(
        owner.complete_handover(1, 2),
        Err(AlsaError::SwitchBusy),
        "no transition started yet"
    );
    owner.begin_handover(0, 1000, 0xAAAA).expect("begin");
    assert_eq!(owner.state(), OwnerState::Transition);
    let change = owner
        .complete_handover(500, 0xBBBB)
        .expect("complete handover");
    assert_eq!(change.event, EV_DEVICE_OWNER_CHANGE);
    assert_eq!(change.from, Some(OwnerKind::Playback));
    assert_eq!(change.to, Some(OwnerKind::Calibration));
    assert_eq!(change.hw_hash_before, 0xAAAA);
    assert_eq!(change.hw_hash_after, 0xBBBB);
    assert_eq!(owner.state(), OwnerState::Calibration);
}

#[test]
fn t17_owner_handover_timeout_stalls_and_closes() {
    let mut owner = DeviceOwner::new();
    owner.acquire(OwnerKind::Playback).expect("playback");
    owner.begin_handover(100, 50, 0x1234).expect("begin");
    assert_eq!(
        owner.complete_handover(151, 0x5678),
        Err(AlsaError::OwnerTransitionTimeout)
    );
    assert_eq!(owner.state(), OwnerState::Stalled);
    assert_eq!(owner.current(), None);
    let last = *owner.changes().last().expect("change record");
    assert_eq!(last.from, Some(OwnerKind::Playback));
    assert_eq!(last.to, None);
    assert_eq!(last.hw_hash_before, 0x1234);
    assert_eq!(last.hw_hash_after, 0);
    // Stalled 后不得直接 acquire（必须外部修复/重新准入）。
    assert_eq!(
        owner.acquire(OwnerKind::Calibration),
        Err(AlsaError::DeviceBusy)
    );
}

#[test]
fn t17_owner_begin_handover_requires_owner() {
    let mut owner = DeviceOwner::new();
    assert_eq!(owner.begin_handover(0, 10, 0), Err(AlsaError::SwitchBusy));
    owner.acquire(OwnerKind::Playback).expect("playback");
    owner.begin_handover(0, 10, 1).expect("begin");
    assert_eq!(
        owner.begin_handover(0, 10, 1),
        Err(AlsaError::SwitchBusy),
        "double begin"
    );
}

#[test]
fn t17_owner_fail_closed_records_release() {
    let mut owner = DeviceOwner::new();
    owner.acquire(OwnerKind::Calibration).expect("calibration");
    let change = owner.fail_closed();
    assert_eq!(change.to, None);
    assert_eq!(owner.state(), OwnerState::Stalled);
}

#[test]
fn t17_session_owner_is_playback_while_running() {
    let (mut session, actual) = started();
    assert_eq!(session.state(), DuplexState::Running);
    assert_eq!(session.owner_state(), OwnerState::Playback);
    assert_eq!(session.owner_kind(), Some(OwnerKind::Playback));
    assert_eq!(session.epoch_id(), 1);
    assert_eq!(session.actual(), Some(actual));
    let first = *session.owner_changes().first().expect("owner change");
    assert_eq!(first.event, EV_DEVICE_OWNER_CHANGE);
    assert_eq!(first.to, Some(OwnerKind::Playback));
    session.stop().expect("stop");
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert_eq!(session.owner_changes().len(), 2);
}

#[test]
fn t17_session_calibration_handover_roundtrip() {
    let (mut session, actual) = started();
    let before_hash = session
        .suspend_for_calibration(1_000, 5_000_000)
        .expect("suspend");
    assert_eq!(before_hash, actual.hash);
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Transition);
    assert!(!session.mute_required());
    let calibration = cal_hw();
    let change = session
        .calibration_opened(2_000, &calibration)
        .expect("calibration opened");
    assert_eq!(change.from, Some(OwnerKind::Playback));
    assert_eq!(change.to, Some(OwnerKind::Calibration));
    assert_eq!(change.hw_hash_before, before_hash);
    assert_eq!(change.hw_hash_after, hwparams_hash(&calibration));
    assert_eq!(session.owner_state(), OwnerState::Calibration);
    let actual = session
        .resume_playback(&motu_sel(), CARDS_MOTU)
        .expect("resume playback");
    assert_eq!(session.state(), DuplexState::Running);
    assert_eq!(session.owner_state(), OwnerState::Playback);
    assert_eq!(session.epoch_id(), 2);
    assert_eq!(actual.hash, before_hash);
    session.stop().expect("stop");
}

#[test]
fn t17_session_calibration_handover_timeout_mutes() {
    let (mut session, _actual) = started();
    session
        .suspend_for_calibration(0, 100)
        .expect("suspend with short window");
    assert_eq!(
        session.calibration_opened(500, &cal_hw()),
        Err(AlsaError::OwnerTransitionTimeout)
    );
    assert_eq!(session.state(), DuplexState::Muted);
    assert!(session.mute_required());
    assert_eq!(session.owner_state(), OwnerState::Stalled);
    session.stop().expect("cleanup");
}

// ---------------------------------------------------------------------------
// F. 启动状态机（协商/actual/重试/回滚/重复启动/宽度）
// ---------------------------------------------------------------------------

#[test]
fn t17_session_start_transitions_and_epoch() {
    let (session, _actual) = started();
    let kinds: Vec<TransitionKind> = session.transitions().iter().map(|item| item.kind).collect();
    assert!(kinds.contains(&TransitionKind::Open));
    assert!(kinds.contains(&TransitionKind::CapsOk));
    assert!(kinds.contains(&TransitionKind::Negotiated));
    assert!(kinds.contains(&TransitionKind::Configured));
    assert!(kinds.contains(&TransitionKind::StartOk));
    assert_eq!(count_kind(&session, TransitionKind::StartOk), 1);
    assert_eq!(session.epoch_id(), 1);
    assert_eq!(session.state(), DuplexState::Running);
    assert!(!session.mute_required());
    assert_eq!(session.last_error(), None);
    let calls = session.backend().calls();
    assert!(calls.contains(&FakeCall::Open));
    assert!(calls.contains(&FakeCall::Caps(StreamDirection::Capture)));
    assert!(calls.contains(&FakeCall::Caps(StreamDirection::Playback)));
    assert!(calls.contains(&FakeCall::Apply));
    assert!(calls.contains(&FakeCall::Start));
}

#[test]
fn t17_session_start_bad_card_does_not_open() {
    let mut session = make_session();
    assert_eq!(
        session.start(&DeviceSelector::by_id("not-a-card", 0), CARDS_MOTU),
        Err(AlsaError::CardNotFound)
    );
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert!(!session.backend().opened);
    assert!(session.backend().calls().is_empty());
}

#[test]
fn t17_session_start_retries_device_lost_then_runs() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    backend.open_failures = 1;
    let mut session = DuplexSession::new(
        backend,
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::new(2, 3),
        HwOpenGate::denied(),
    );
    let actual = session.start(&motu_sel(), CARDS_MOTU).expect("retry ok");
    assert_eq!(actual.hw.period_frames, 128);
    assert_eq!(session.state(), DuplexState::Running);
    assert!(has_kind(&session, TransitionKind::StartupRetry));
    let open_calls = session
        .backend()
        .calls()
        .iter()
        .filter(|call| **call == FakeCall::Open)
        .count();
    assert_eq!(open_calls, 2);
    assert_eq!(session.epoch_id(), 1);
}

#[test]
fn t17_session_start_rolls_back_after_retries_exhausted() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    backend.open_failures = 5;
    let mut session = DuplexSession::new(
        backend,
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::new(2, 3),
        HwOpenGate::denied(),
    );
    assert_eq!(
        session.start(&motu_sel(), CARDS_MOTU),
        Err(AlsaError::DeviceLost)
    );
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert!(has_kind(&session, TransitionKind::StartupFailed));
    assert_eq!(session.last_error(), Some(AlsaError::DeviceLost));
}

#[test]
fn t17_session_start_actual_period_bad_is_refused_not_clamped() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    backend.actual_override = Some(raw_actual(
        stream_actual(20, S24, 48_000, 256, 2048),
        stream_actual(22, S24, 48_000, 256, 2048),
    ));
    let mut session = DuplexSession::new(
        backend,
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::new(3, 3),
        HwOpenGate::denied(),
    );
    assert_eq!(
        session.start(&motu_sel(), CARDS_MOTU),
        Err(AlsaError::DeviceParams)
    );
    assert_eq!(session.state(), DuplexState::Closed);
    // 非可重试（配置类）错误只尝试一次。
    let open_calls = session
        .backend()
        .calls()
        .iter()
        .filter(|call| **call == FakeCall::Open)
        .count();
    assert_eq!(open_calls, 1);
}

#[test]
fn t17_session_start_twice_is_rejected() {
    let (mut session, _actual) = started();
    assert_eq!(
        session.start(&motu_sel(), CARDS_MOTU),
        Err(AlsaError::SwitchBusy)
    );
    assert_eq!(session.state(), DuplexState::Running);
}

#[test]
fn t17_session_start_with_width_rejects_narrow_capture() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    backend.actual_override = Some(raw_actual(
        stream_actual(12, S24, 48_000, 128, 1024),
        stream_actual(22, S24, 48_000, 128, 1024),
    ));
    let mut session = DuplexSession::new(
        backend,
        DuplexRequest::motu_baseline(),
        RecoveryPolicy::default(),
        HwOpenGate::denied(),
    );
    assert_eq!(
        session.start_with_width(&motu_sel(), CARDS_MOTU, &RequiredDeviceWidth::toslink_412()),
        Err(AlsaError::Channels)
    );
    assert_eq!(session.state(), DuplexState::Closed);
}

#[test]
fn t17_session_stop_drains_closes_and_releases() {
    let (mut session, _actual) = started();
    session.stop().expect("stop");
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert!(!session.mute_required());
    assert_eq!(session.actual(), None);
    assert_eq!(session.backend().drain_calls, 1);
    assert_eq!(session.backend().close_calls, 1);
    assert!(has_kind(&session, TransitionKind::Stopped));
    // stop 后再 stop = 非法状态。
    assert_eq!(session.stop(), Err(AlsaError::SwitchBusy));
}

#[test]
fn t17_session_start_from_muted_does_full_reacquire() {
    let (mut session, _actual) = started();
    session.backend_mut().recover_failures = 255;
    let mut capture = cap_buf(&session.actual().unwrap());
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert_eq!(session.state(), DuplexState::Recovering);
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 1
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 2
        }
    );
    assert_eq!(session.recovery_step(), RecoveryAction::Exhausted);
    assert_eq!(session.state(), DuplexState::Muted);
    assert!(session.mute_required());
    // 完整重新准入（fake 已修复）→ Running，epoch 递增。
    session.backend_mut().recover_failures = 0;
    let actual = session.start(&motu_sel(), CARDS_MOTU).expect("reacquire");
    assert_eq!(session.state(), DuplexState::Running);
    assert_eq!(session.epoch_id(), 2);
    assert_eq!(actual.hw.period_frames, 128);
    assert!(has_kind(&session, TransitionKind::Reacquire));
}

// ---------------------------------------------------------------------------
// G. 启动/恢复状态机（device lost / xrun / 有界重试 / 静音 / 新 epoch / Muted）
// ---------------------------------------------------------------------------

#[test]
fn t17_recovery_io_device_lost_enters_recovering() {
    let (mut session, actual) = started();
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert_eq!(session.io_read(&mut capture), Err(AlsaError::DeviceLost));
    assert_eq!(session.state(), DuplexState::Recovering);
    assert!(session.mute_required());
    assert_eq!(session.owner_state(), OwnerState::Playback);
    assert!(has_kind(&session, TransitionKind::IoDeviceLost));
    assert_eq!(session.last_error(), Some(AlsaError::DeviceLost));
}

#[test]
fn t17_recovery_three_stages_then_new_epoch() {
    let (mut session, actual) = started();
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert_eq!(session.epoch_id(), 1);
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 1,
            attempt: 0
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 2,
            attempt: 0
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovered { epoch_id: 2 }
    );
    assert_eq!(session.state(), DuplexState::Running);
    assert!(!session.mute_required());
    assert_eq!(session.epoch_id(), 2);
    assert_eq!(session.last_error(), None);
    assert!(has_kind(&session, TransitionKind::Recovered));
    let stages: Vec<RecoverStage> = session
        .backend()
        .calls()
        .iter()
        .filter_map(|call| match call {
            FakeCall::Recover(stage) => Some(*stage),
            _ => None,
        })
        .collect();
    assert_eq!(
        stages,
        vec![
            RecoverStage::Suspend,
            RecoverStage::Prepare,
            RecoverStage::Resume
        ]
    );
    // 恢复后可以继续 IO（新 epoch）。
    assert_eq!(session.io_read(&mut capture), Ok(capture.len()));
}

#[test]
fn t17_recovery_write_silence_only_during_mute() {
    let (mut session, actual) = started();
    assert_eq!(
        session.write_silence(),
        Err(AlsaError::SwitchBusy),
        "silence not allowed while Running"
    );
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert!(session.mute_required());
    assert_eq!(session.write_silence(), Ok(8448));
    assert_eq!(session.backend().silence_writes, 1);
    // 静音期间普通 IO 写被拒（只能写静音）。
    let playback = pb_buf(&actual);
    assert_eq!(session.io_write(&playback), Err(AlsaError::SwitchBusy));
}

#[test]
fn t17_recovery_retry_after_stage_failure_then_success() {
    let (mut session, actual) = started();
    session.backend_mut().recover_failures = 2;
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 1
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 2
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 1,
            attempt: 2
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 2,
            attempt: 2
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovered { epoch_id: 2 }
    );
    assert_eq!(session.state(), DuplexState::Running);
    assert!(has_kind(&session, TransitionKind::RecoverAttemptFailed));
}

#[test]
fn t17_recovery_exhaustion_enters_muted() {
    let (mut session, actual) = started();
    session.backend_mut().recover_failures = 255;
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 1
        }
    );
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 2
        }
    );
    assert_eq!(session.recovery_step(), RecoveryAction::Exhausted);
    assert_eq!(session.state(), DuplexState::Muted);
    assert!(session.mute_required());
    assert!(has_kind(&session, TransitionKind::RecoveryExhausted));
    // Muted 不自动恢复；静音仍可写，普通 IO 被拒。
    assert_eq!(session.recovery_step(), RecoveryAction::Exhausted);
    assert_eq!(session.write_silence(), Ok(8448));
    let playback = pb_buf(&actual);
    assert_eq!(session.io_write(&playback), Err(AlsaError::SwitchBusy));
    assert_eq!(session.epoch_id(), 1, "no new epoch without recovery");
}

#[test]
fn t17_recovery_requires_running_state() {
    let mut session = make_session();
    assert_eq!(
        session.begin_recovery(RecoveryReason::DeviceLost),
        Err(AlsaError::SwitchBusy)
    );
    assert_eq!(session.recovery_step(), RecoveryAction::None);
    // 已在 Running 再 begin 是合法的进入恢复入口（幂等语义另测）。
    let mut session = make_session();
    session.start(&motu_sel(), CARDS_MOTU).expect("start");
    assert_eq!(
        session.begin_recovery(RecoveryReason::Xrun),
        Ok(RecoveryAction::Recovering {
            stage: 0,
            attempt: 0
        })
    );
    assert_eq!(
        session.begin_recovery(RecoveryReason::Xrun),
        Ok(RecoveryAction::Recovering {
            stage: 0,
            attempt: 0
        })
    );
}

#[test]
fn t17_note_rt_status_mapping() {
    let (mut session, _actual) = started();
    assert_eq!(session.note_rt_status(RtStatus::OK), RecoveryAction::None);
    assert_eq!(
        session.note_rt_status(RtStatus::RT_E_PARTIAL_LATE),
        RecoveryAction::None
    );
    assert_eq!(
        session.note_rt_status(RtStatus::RT_E_LATE_STREAK),
        RecoveryAction::None
    );
    assert_eq!(
        session.note_rt_status(RtStatus::RT_E_DEVICE_LOST),
        RecoveryAction::Recovering {
            stage: 0,
            attempt: 0
        }
    );
    assert_eq!(session.state(), DuplexState::Recovering);

    let (mut session, _actual) = started();
    assert_eq!(
        session.note_rt_status(RtStatus::RT_E_WORKER_LOST),
        RecoveryAction::Faulted
    );
    assert_eq!(session.state(), DuplexState::Faulted);
    assert!(session.mute_required());

    let (mut session, _actual) = started();
    assert_eq!(
        session.note_rt_status(RtStatus::ERR_RATE),
        RecoveryAction::Faulted
    );
    assert_eq!(session.state(), DuplexState::Faulted);
}

#[test]
fn t17_faulted_state_blocks_io_and_allows_restart() {
    let (mut session, _actual) = started();
    assert_eq!(
        session.note_rt_status(RtStatus::RT_E_INVARIANT),
        RecoveryAction::Faulted
    );
    assert_eq!(session.state(), DuplexState::Faulted);
    assert!(session.mute_required());
    let mut capture = [0u8; 7680];
    assert_eq!(session.io_read(&mut capture), Err(AlsaError::SwitchBusy));
    assert_eq!(session.write_silence(), Ok(8448));
    let actual = session.start(&motu_sel(), CARDS_MOTU).expect("restart");
    assert_eq!(session.state(), DuplexState::Running);
    assert_eq!(session.epoch_id(), 2);
    assert_eq!(actual.hw.n_in, 20);
    assert!(has_kind(&session, TransitionKind::Reacquire));
}

#[test]
fn t17_xrun_triggers_recovery() {
    let (mut session, actual) = started();
    let playback = pb_buf(&actual);
    session.backend_mut().fail_next_write(AlsaError::Xrun);
    assert_eq!(session.io_write(&playback), Err(AlsaError::Xrun));
    assert_eq!(session.state(), DuplexState::Recovering);
    assert!(has_kind(&session, TransitionKind::IoXrun));
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovering {
            stage: 1,
            attempt: 0
        }
    );
}

#[test]
fn t17_recovery_epochs_monotonic_across_two_recoveries() {
    let (mut session, actual) = started();
    let mut capture = cap_buf(&actual);
    for expected_epoch in [2u64, 3u64] {
        session.backend_mut().fail_next_read(AlsaError::DeviceLost);
        assert!(session.io_read(&mut capture).is_err());
        assert_eq!(
            session.recovery_step(),
            RecoveryAction::Recovering {
                stage: 1,
                attempt: 0
            }
        );
        let _ = session.recovery_step();
        assert_eq!(
            session.recovery_step(),
            RecoveryAction::Recovered {
                epoch_id: expected_epoch
            }
        );
        assert_eq!(session.epoch_id(), expected_epoch);
        assert_eq!(session.state(), DuplexState::Running);
    }
    assert_eq!(session.epoch_id(), 3);
}

#[test]
fn t17_stop_during_recovering_cleans_up() {
    let (mut session, actual) = started();
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert_eq!(session.state(), DuplexState::Recovering);
    session.stop().expect("stop during recovery");
    assert_eq!(session.state(), DuplexState::Closed);
    assert!(!session.mute_required());
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert_eq!(session.actual(), None);
    assert!(has_kind(&session, TransitionKind::Stopped));
}

#[test]
fn t17_recovery_silence_length_matches_playback_period() {
    let (mut session, actual) = started();
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    // 22ch × 128 帧 × 3 B = 8448。
    assert_eq!(
        actual.period_bytes(StreamDirection::Playback).unwrap(),
        8448
    );
    assert_eq!(session.write_silence(), Ok(8448));
    let last_write = session
        .backend()
        .calls()
        .iter()
        .rev()
        .find(|call| matches!(call, FakeCall::Write(_)));
    assert_eq!(last_write, Some(&FakeCall::Write(8448)));
}

// ---------------------------------------------------------------------------
// H. fake backend 行为（调用日志/回显/计数/访问器）
// ---------------------------------------------------------------------------

#[test]
fn t17_fake_backend_call_log_and_counters() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    assert!(!PcmDuplexBackend::is_hardware(&backend));
    assert!(backend.calls().is_empty());
    backend
        .open(&ResolvedDevice {
            card_index: 4,
            device_index: 0,
        })
        .expect("open");
    assert!(backend.opened);
    let (capture_caps, playback_caps) = caps_pair();
    assert_eq!(
        backend.stream_caps(StreamDirection::Capture),
        Ok(capture_caps)
    );
    assert_eq!(
        backend.stream_caps(StreamDirection::Playback),
        Ok(playback_caps)
    );
    assert!(backend.calls().contains(&FakeCall::Open));
    assert!(backend
        .calls()
        .contains(&FakeCall::Caps(StreamDirection::Capture)));
    backend.close();
    assert!(!backend.opened);
    assert_eq!(backend.close_calls, 1);
}

#[test]
fn t17_fake_backend_apply_echoes_plan() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    let raw = backend.apply_hw_params(&motu_plan()).expect("apply");
    assert_eq!(raw, motu_raw());
    assert_eq!(backend.applied_plan, Some(motu_plan()));
    assert_eq!(backend.applied_actual, Some(motu_raw()));
}

#[test]
fn t17_fake_backend_clear_calls_keeps_counters() {
    let mut backend = FakeBackend::motu_ultralite_mk5();
    backend
        .open(&ResolvedDevice {
            card_index: 4,
            device_index: 0,
        })
        .expect("open");
    assert!(!backend.calls().is_empty());
    backend.clear_calls();
    assert!(backend.calls().is_empty());
    assert_eq!(backend.close_calls, 0);
    backend.close();
    assert_eq!(backend.close_calls, 1);
}

#[test]
fn t17_session_backend_accessors() {
    let (mut session, actual) = started();
    assert!(session.backend().opened);
    assert!(session.backend().started);
    let mut capture = cap_buf(&actual);
    session.backend_mut().fail_next_read(AlsaError::DeviceLost);
    assert!(session.io_read(&mut capture).is_err());
    assert_eq!(session.backend().read_calls, 0);
    let _ = session.recovery_step();
    let _ = session.recovery_step();
    assert_eq!(
        session.recovery_step(),
        RecoveryAction::Recovered { epoch_id: 2 }
    );
    assert!(session.backend().recover_calls >= 3);
}

// ---------------------------------------------------------------------------
// I. 全链离线模型（start → IO → xrun → recover → stop）
// ---------------------------------------------------------------------------

#[test]
fn t17_full_lifecycle_start_io_xrun_recover_stop() {
    let (mut session, actual) = started();
    let mut capture = cap_buf(&actual);
    let playback = pb_buf(&actual);
    for _ in 0..5 {
        assert_eq!(session.io_read(&mut capture), Ok(capture.len()));
        assert_eq!(session.io_write(&playback), Ok(playback.len()));
    }
    assert_eq!(session.backend().read_calls, 5);
    assert_eq!(session.backend().write_calls, 5);
    session.backend_mut().fail_next_write(AlsaError::Xrun);
    assert_eq!(session.io_write(&playback), Err(AlsaError::Xrun));
    assert_eq!(session.state(), DuplexState::Recovering);
    assert_eq!(session.write_silence(), Ok(8448));
    for expected in [
        RecoveryAction::Recovering {
            stage: 1,
            attempt: 0,
        },
        RecoveryAction::Recovering {
            stage: 2,
            attempt: 0,
        },
        RecoveryAction::Recovered { epoch_id: 2 },
    ] {
        assert_eq!(session.recovery_step(), expected);
    }
    for _ in 0..3 {
        assert_eq!(session.io_read(&mut capture), Ok(capture.len()));
        assert_eq!(session.io_write(&playback), Ok(playback.len()));
    }
    session.stop().expect("stop");
    assert_eq!(session.state(), DuplexState::Closed);
    assert_eq!(session.owner_state(), OwnerState::Idle);
    assert_eq!(session.backend().drain_calls, 1);
    assert_eq!(session.epoch_id(), 2);
    let calls = session.backend().calls();
    let open_at = calls.iter().position(|call| *call == FakeCall::Open);
    let apply_at = calls.iter().position(|call| *call == FakeCall::Apply);
    let start_at = calls.iter().position(|call| *call == FakeCall::Start);
    let recover_at = calls
        .iter()
        .position(|call| matches!(call, FakeCall::Recover(_)));
    let drain_at = calls.iter().position(|call| *call == FakeCall::Drain);
    assert!(open_at < apply_at && apply_at < start_at);
    assert!(start_at < recover_at, "recover must be after streaming");
    assert!(recover_at < drain_at, "drain after recovery");
}

#[test]
fn t17_motu_baseline_full_duplex_matches_t01_evidence() {
    let mut session = make_session();
    let actual = session
        .start_with_width(&motu_sel(), CARDS_MOTU, &RequiredDeviceWidth::toslink_412())
        .expect("width-validated start");
    let expected = TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: S24,
        n_in: 20,
        n_out: 22,
        sample_rate: 48_000,
        period_frames: 128,
        buffer_frames: 1024,
        in_stride_bytes: 7680,
        out_stride_bytes: 8448,
        flags: 1 << 2,
        reserved: [0; 4],
    };
    assert_eq!(actual.hw, expected);
    // E1 证据换算：period 128 / buffer 1024 = 8 periods；B 来源链 = actual period。
    assert_eq!(actual.hw.buffer_frames / actual.hw.period_frames, 8);
    assert!(is_runtime_period(actual.hw.period_frames));
    assert_eq!(actual.hw.n_in, MOTU_CAPTURE_CHANNELS);
    assert_eq!(actual.hw.n_out, MOTU_PLAYBACK_CHANNELS);
    assert_eq!(actual.hw.sample_rate, MOTU_BASELINE_RATE);
    assert!(!actual.requires_new_epoch());
    // 调用顺序：Caps(capture) → Caps(playback) → Apply → Start。
    let calls = session.backend().calls();
    let caps_capture = calls
        .iter()
        .position(|call| *call == FakeCall::Caps(StreamDirection::Capture))
        .expect("caps capture");
    let caps_playback = calls
        .iter()
        .position(|call| *call == FakeCall::Caps(StreamDirection::Playback))
        .expect("caps playback");
    let apply = calls
        .iter()
        .position(|call| *call == FakeCall::Apply)
        .expect("apply");
    let start = calls
        .iter()
        .position(|call| *call == FakeCall::Start)
        .expect("start");
    assert!(caps_capture < caps_playback && caps_playback < apply && apply < start);
    assert_eq!(session.epoch_id(), 1);
}

// ---------------------------------------------------------------------------
// J. 错误码映射/分类（core-v1 §9.2）
// ---------------------------------------------------------------------------

#[test]
fn t17_error_status_mapping_table() {
    let cases: [(AlsaError, u32); 17] = [
        (AlsaError::Rate, 104),
        (AlsaError::BlockSize, 105),
        (AlsaError::Channels, 106),
        (AlsaError::StrideAlign, 109),
        (AlsaError::BadLayout, 103),
        (AlsaError::DeviceParams, 118),
        (AlsaError::DeviceBusy, 117),
        (AlsaError::OwnerTransitionTimeout, 117),
        (AlsaError::SwitchBusy, 120),
        (AlsaError::CardNotFound, 118),
        (AlsaError::CardAmbiguous, 118),
        (AlsaError::Unsupported, 122),
        (AlsaError::BackendUnavailable, 122),
        (AlsaError::HwOpenDenied, 122),
        (AlsaError::Xrun, 407),
        (AlsaError::DeviceLost, 407),
        (AlsaError::RecoveryExhausted, 407),
    ];
    for (error, code) in cases {
        assert_eq!(error.status().code(), code, "{}", error.name());
    }
}

#[test]
fn t17_error_names_are_unique() {
    let errors = [
        AlsaError::Rate,
        AlsaError::BlockSize,
        AlsaError::Channels,
        AlsaError::StrideAlign,
        AlsaError::BadLayout,
        AlsaError::DeviceParams,
        AlsaError::DeviceBusy,
        AlsaError::OwnerTransitionTimeout,
        AlsaError::SwitchBusy,
        AlsaError::CardNotFound,
        AlsaError::CardAmbiguous,
        AlsaError::Unsupported,
        AlsaError::BackendUnavailable,
        AlsaError::HwOpenDenied,
        AlsaError::Xrun,
        AlsaError::DeviceLost,
        AlsaError::RecoveryExhausted,
    ];
    for (index, left) in errors.iter().enumerate() {
        for right in errors.iter().skip(index + 1) {
            assert_ne!(left.name(), right.name());
        }
    }
}

#[test]
fn t17_error_retry_classification() {
    assert!(AlsaError::Xrun.device_recoverable());
    assert!(AlsaError::DeviceLost.device_recoverable());
    assert!(AlsaError::Xrun.startup_retryable());
    assert!(AlsaError::DeviceLost.startup_retryable());
    for error in [
        AlsaError::Rate,
        AlsaError::BlockSize,
        AlsaError::Channels,
        AlsaError::DeviceParams,
        AlsaError::SwitchBusy,
        AlsaError::Unsupported,
        AlsaError::HwOpenDenied,
        AlsaError::BackendUnavailable,
        AlsaError::RecoveryExhausted,
    ] {
        assert!(!error.device_recoverable(), "{}", error.name());
        assert!(!error.startup_retryable(), "{}", error.name());
    }
}

#[test]
fn t17_stream_direction_and_owner_kind_names() {
    assert_eq!(StreamDirection::Capture.name(), "capture");
    assert_eq!(StreamDirection::Playback.name(), "playback");
    assert_eq!(OwnerKind::Playback.name(), "playback");
    assert_eq!(OwnerKind::Calibration.name(), "calibration");
    assert_eq!(RECOVER_STAGES.len(), 3);
}

// ---------------------------------------------------------------------------
// K. 平台 backend 选择 / ALSA errno 映射（跨平台纯逻辑；真实 PCM 仅 Linux 编译）
// ---------------------------------------------------------------------------

#[test]
fn t17_platform_backend_kind_matches_target() {
    #[cfg(target_os = "linux")]
    assert_eq!(platform_backend_kind(), BackendKind::LinuxAlsa);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(platform_backend_kind(), BackendKind::Fake);
    assert_eq!(BackendKind::Fake.name(), "fake");
    assert_eq!(BackendKind::LinuxAlsa.name(), "linux-alsa");
}

#[test]
fn t17_backend_kind_hardware_classification() {
    assert!(!backend_kind_is_hardware(BackendKind::Fake));
    assert!(backend_kind_is_hardware(BackendKind::LinuxAlsa));
    assert!(!BackendKind::Fake.is_hardware());
    assert!(BackendKind::LinuxAlsa.is_hardware());
}

#[test]
fn t17_hw_gate_denies_linux_alsa_kind_by_default() {
    // 编译了真实 backend 不等于能打开硬件：gate 默认 Denied 是唯一入口门。
    assert!(backend_kind_is_hardware(BackendKind::LinuxAlsa));
    assert_eq!(
        HwOpenGate::from_env_value(None).check(),
        Err(AlsaError::HwOpenDenied)
    );
    assert_eq!(HwOpenGate::denied().check(), Err(AlsaError::HwOpenDenied));
    assert_eq!(HwOpenGate::allowed().check(), Ok(()));
}

#[test]
fn t17_alsa_errno_mapping_table() {
    // POSIX errno 值（core-v1 §9.2 冻结码映射；见 alsa_errno_to_error 文档）。
    let cases: [(i32, AlsaError); 12] = [
        (16, AlsaError::DeviceBusy),   // EBUSY
        (19, AlsaError::DeviceLost),   // ENODEV
        (6, AlsaError::DeviceLost),    // ENXIO
        (5, AlsaError::DeviceLost),    // EIO
        (77, AlsaError::DeviceLost),   // EBADFD
        (22, AlsaError::DeviceParams), // EINVAL
        (12, AlsaError::BadLayout),    // ENOMEM
        (32, AlsaError::Xrun),         // EPIPE
        (86, AlsaError::DeviceLost),   // ESTRPIPE (suspend)
        (38, AlsaError::Unsupported),  // ENOSYS
        (95, AlsaError::Unsupported),  // EOPNOTSUPP
        (999, AlsaError::DeviceLost),  // unknown -> conservative device loss
    ];
    for (errno, expected) in cases {
        assert_eq!(alsa_errno_to_error(errno), expected, "errno={errno}");
    }
    // 负值与正值同等处理。
    assert_eq!(alsa_errno_to_error(-32), AlsaError::Xrun);
    assert_eq!(alsa_errno_to_error(-16), AlsaError::DeviceBusy);
    assert_eq!(alsa_errno_to_error(-22), AlsaError::DeviceParams);
    // 映射结果必须落在冻结码内（不新增码）。
    for (errno, expected) in cases {
        let status = expected.status();
        assert!(
            matches!(
                status.code(),
                103 | 104 | 105 | 106 | 109 | 117 | 118 | 120 | 122 | 407
            ),
            "errno={errno} code={}",
            status.code()
        );
    }
}

#[test]
fn t17_motu_arecord_buffer_reference_negotiates() {
    // T01 E1 采集建议：arecord --period-size=1024 --buffer-size=16384；runtime B 仍是 128，
    // 但 buffer 16384（= 128 period × 128）必须是可协商的。
    let (capture, playback) = caps_pair();
    let request = DuplexRequest::new(48_000, 128, 16_384);
    let plan = negotiate(&capture, &playback, &request).expect("E1 arecord buffer plan");
    assert_eq!(plan.period_frames, 128);
    assert_eq!(plan.buffer_frames, 16_384);
    assert_eq!(plan.buffer_frames % plan.period_frames, 0);
}

#[test]
fn t17_motu_baseline_period_buffer_constants() {
    // T01 E1：chunk256 → actual period128/buffer1024；MOTU cap20/pb22 @48k S24_3LE。
    assert_eq!(DEFAULT_PERIOD_FRAMES, 128);
    assert_eq!(DEFAULT_BUFFER_FRAMES, 1024);
    assert_eq!(MOTU_CAPTURE_CHANNELS, 20);
    assert_eq!(MOTU_PLAYBACK_CHANNELS, 22);
    assert_eq!(MOTU_BASELINE_RATE, 48_000);
    assert_eq!(MAX_PERIOD_BYTES, 11_264);
    let baseline = DuplexRequest::motu_baseline();
    assert_eq!(baseline.sample_rate, MOTU_BASELINE_RATE);
    assert_eq!(baseline.period_frames, DEFAULT_PERIOD_FRAMES);
    assert_eq!(baseline.buffer_frames, DEFAULT_BUFFER_FRAMES);
    // 板端 hw 名称示例：stable-ID 解析出的 card4/device0 → hw:4,0。
    let device = ResolvedDevice {
        card_index: 4,
        device_index: 0,
    };
    let mut name = [0u8; 16];
    let length = device.write_hw_name(&mut name).expect("hw name");
    assert_eq!(&name[..length], b"hw:4,0");
}

#[cfg(target_os = "linux")]
#[test]
fn t17_linux_backend_is_hardware_and_gated() {
    // 仅 Linux 编译（Windows 目标本测试不存在）；Linux 上只检查构造/状态，不 open 硬件。
    let backend = LinuxAlsaBackend::new();
    assert!(PcmDuplexBackend::is_hardware(&backend));
    assert!(!backend.is_open());
    assert!(backend.capture_state().is_none());
    assert!(backend.playback_state().is_none());
    let default_backend = LinuxAlsaBackend::default();
    assert!(!default_backend.is_open());
    assert_eq!(HwOpenGate::denied().check(), Err(AlsaError::HwOpenDenied));
}
