// T43（aoip）离线模型测试：AES67/Inferno 端点接入 / SDP / H16 逻辑12-容器16 /
// format-rate / 失流 fake 状态机。
//
// 本文件经 src/aoip.rs 末尾的 cfg(test) mod aoip_fake_tests { include!(...) } 进入单测
// 构建（cargo 不自动发现 tests/<子目录>；公共注册属 integrator）。运行：
//   cargo test -p tspi-rt-host aoip_fake
// 全部测试名 t43_ 前缀；模块路径含 aoip_fake_tests，可按 aoip_fake 过滤。
// 离线：不打开 ALSA/pipe、不启动 aes67-daemon/statime、不连板端、不需要声卡/网络。

use super::*;

/// /proc/asound/cards 样本：RAVENNA（AES67 LKM）在 card 6；另有 MOTU/UMIK 不被匹配。
const CARDS_RAVENNA: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
 4 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5
 6 [RAVENNA        ]: RAVENNA - RAVENNA
                      RAVENNA at 0x0 irq 0
";

/// RAVENNA card 重名（拒绝猜测）。
const CARDS_RAVENNA_AMBIGUOUS: &str = " 6 [RAVENNA        ]: RAVENNA - RAVENNA
 9 [RAVENNA        ]: RAVENNA - RAVENNA
";

fn sdp_text(
    address: &str,
    port: u16,
    payload: u8,
    encoding: &str,
    rate: u32,
    channels: u16,
    ptime: Option<&str>,
    ptp_domain: Option<&str>,
) -> String {
    let mut text = format!(
        "v=0\r\n\
         o=- 0 0 IN IP4 127.0.0.1\r\n\
         s=NuPrime-a227-5240:16\r\n\
         c=IN IP4 {address}/32\r\n\
         t=0 0\r\n\
         m=audio {port} RTP/AVP {payload}\r\n\
         a=rtpmap:{payload} {encoding}/{rate}/{channels}\r\n"
    );
    if let Some(value) = ptime {
        text.push_str(&format!("a=ptime:{value}\r\n"));
    }
    if let Some(domain) = ptp_domain {
        text.push_str(&format!(
            "a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:{domain}\r\n"
        ));
    }
    text.push_str("a=mediaclk:direct=0\r\n");
    text
}

fn h16_sdp_pair(channels_second: u16, address: &str, encoding: &str) -> (String, String) {
    (
        sdp_text(
            "239.69.16.146",
            5_004,
            103,
            encoding,
            48_000,
            8,
            Some("1"),
            Some("0"),
        ),
        sdp_text(
            address,
            5_004,
            103,
            encoding,
            48_000,
            channels_second,
            Some("1"),
            Some("0"),
        ),
    )
}

fn plan_16ch() -> Aes67CapturePlan {
    let (first, second) = h16_sdp_pair(8, "239.69.16.147", "L24");
    let flow0 = parse_aes67_sdp(&first).expect("flow0 sdp");
    let flow1 = parse_aes67_sdp(&second).expect("flow1 sdp");
    let set = Aes67FlowSet::new(&[flow0, flow1]).expect("flow set");
    validate_aes67_capture(&set, &H16FlowConfig::container16(), 0).expect("16ch plan")
}

fn plan_12ch() -> Aes67CapturePlan {
    let (first, second) = h16_sdp_pair(4, "239.69.16.147", "L24");
    let flow0 = parse_aes67_sdp(&first).expect("flow0 sdp");
    let flow1 = parse_aes67_sdp(&second).expect("flow1 sdp");
    let set = Aes67FlowSet::new(&[flow0, flow1]).expect("flow set");
    validate_aes67_capture(&set, &H16FlowConfig::logical12(), 0).expect("12ch plan")
}

fn ravenna_device() -> ResolvedDevice {
    plan_16ch()
        .resolve_alsa_device(CARDS_RAVENNA, AES67_CARD_ID_DEFAULT)
        .expect("ravenna resolved")
}

fn inferno_env_16() -> InfernoEnv<'static> {
    InfernoEnv::new(
        "00000a00020f0001",
        INFERNO_CLOCK_PATH_DEFAULT,
        "end0",
        INFERNO_RATE_HZ,
        16,
    )
}

fn endpoint_inferno_16() -> AoipEndpoint<'static> {
    let spec = AoipEndpointSpec::inferno_alsa(H16Container::container16());
    AoipEndpoint::new(spec).expect("inferno endpoint spec")
}

fn monitor_48k() -> StreamMonitor {
    StreamMonitor::new(128, 48_000, DEFAULT_MAX_STALL_PERIODS).expect("monitor")
}

// ---------------------------------------------------------------------------
// 常量 / profile / 冻结错误码
// ---------------------------------------------------------------------------

#[test]
fn t43_constants_and_profiles() {
    assert_eq!(AES67_RATE_HZ, 48_000);
    assert_eq!(INFERNO_RATE_HZ, 96_000);
    assert_eq!(H16_LOGICAL_CHANNELS, 12);
    assert_eq!(H16_CONTAINER_CHANNELS, 16);
    assert_eq!(AES67_MAX_CHANNELS_PER_FLOW, 8);
    assert_eq!(H16_LOGICAL_SLOTS, [0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 14, 15]);
    assert_eq!(H16_NON_LOGICAL_SLOTS, [8, 9, 12, 13]);
    assert_eq!(DEFAULT_PERIOD_FRAMES, 128);
    assert_eq!(DEFAULT_BUFFER_FRAMES, 1_024);
    assert_eq!(DEFAULT_MAX_STALL_PERIODS, 3);
    assert_eq!(STREAM_LOST_MULTIPLIER, 2);
    assert_eq!(MAX_PIPE_CHUNK_BYTES, 8_192);
    assert_eq!(AES67_PTP_DOMAIN_DEFAULT, 0);
    assert_eq!(AES67_RTP_PORT_DEFAULT, 5_004);
    assert_eq!(AES67_CARD_ID_DEFAULT, "RAVENNA");
    assert_eq!(INFERNO_ALSA_DEVICE, "inferno");
    assert_eq!(INFERNO_CLOCK_PATH_DEFAULT, "/tmp/ptp-usrvclock");
    assert_eq!(INFERNO_DEVICE_ID_HEX_LEN, 16);
}

#[test]
fn t43_protocol_profiles_and_ptp_sources() {
    assert_eq!(AoipProtocol::Aes67.as_str(), "aes67");
    assert_eq!(AoipProtocol::Inferno.as_str(), "inferno");
    assert_eq!(AoipProtocol::Aes67.profile_rate_hz(), AES67_RATE_HZ);
    assert_eq!(AoipProtocol::Inferno.profile_rate_hz(), INFERNO_RATE_HZ);
    assert_eq!(
        AoipProtocol::Aes67.device_format(),
        sample_format::S32_LE_INTERLEAVED
    );
    assert_eq!(
        AoipProtocol::Aes67.expected_ptp_source(),
        PtpSource::Aes67DaemonBuiltinPtpV2
    );
    assert_eq!(
        AoipProtocol::Inferno.expected_ptp_source(),
        PtpSource::StatimePtpV1VirtualClock
    );
    assert_eq!(AoipTransport::Alsa.as_str(), "alsa");
    assert_eq!(AoipTransport::Pipe.as_str(), "pipe");
    assert_eq!(
        PtpSource::Aes67DaemonBuiltinPtpV2.as_str(),
        "aes67-daemon-ptpv2-slave"
    );
    assert_eq!(
        PtpSource::StatimePtpV1VirtualClock.as_str(),
        "statime-ptpv1-virtual-clock"
    );
}

#[test]
fn t43_error_codes_map_to_frozen_status() {
    assert_eq!(AoipError::Rate.status(), RtStatus::ERR_RATE);
    assert_eq!(AoipError::BlockSize.status(), RtStatus::ERR_BLOCK_SIZE);
    assert_eq!(AoipError::Channels.status(), RtStatus::ERR_CHANNELS);
    assert_eq!(AoipError::Roles.status(), RtStatus::ERR_ROLES);
    assert_eq!(AoipError::BadLayout.status(), RtStatus::ERR_BAD_LAYOUT);
    assert_eq!(
        AoipError::DeviceParams.status(),
        RtStatus::ERR_DEVICE_PARAMS
    );
    assert_eq!(AoipError::DeviceBusy.status(), RtStatus::ERR_DEVICE_BUSY);
    assert_eq!(AoipError::SwitchBusy.status(), RtStatus::ERR_SWITCH_BUSY);
    assert_eq!(
        AoipError::EpochMismatch.status(),
        RtStatus::RT_E_EPOCH_MISMATCH
    );
    assert_eq!(
        AoipError::PtpUnavailable.status(),
        RtStatus::ERR_ADMISSION_CLOCK
    );
    assert_eq!(AoipError::Unsupported.status(), RtStatus::ERR_UNSUPPORTED);
    assert_eq!(AoipError::StreamLost.status(), RtStatus::RT_E_DEVICE_LOST);
    assert_eq!(AoipError::Rate.name(), "ERR_RATE");
    assert_eq!(AoipError::StreamLost.name(), "RT_E_DEVICE_LOST");
    assert!(AoipError::PtpUnavailable.retryable());
    assert!(AoipError::StreamLost.retryable());
    assert!(!AoipError::Roles.retryable());
}

#[test]
fn t43_error_from_alsa_mapping() {
    assert_eq!(AoipError::from(AlsaError::Rate), AoipError::Rate);
    assert_eq!(AoipError::from(AlsaError::BlockSize), AoipError::BlockSize);
    assert_eq!(AoipError::from(AlsaError::Channels), AoipError::Channels);
    assert_eq!(
        AoipError::from(AlsaError::CardNotFound),
        AoipError::DeviceParams
    );
    assert_eq!(
        AoipError::from(AlsaError::CardAmbiguous),
        AoipError::DeviceParams
    );
    assert_eq!(
        AoipError::from(AlsaError::OwnerTransitionTimeout),
        AoipError::DeviceBusy
    );
    assert_eq!(
        AoipError::from(AlsaError::HwOpenDenied),
        AoipError::Unsupported
    );
    assert_eq!(AoipError::from(AlsaError::Xrun), AoipError::StreamLost);
    assert_eq!(
        AoipError::from(AlsaError::RecoveryExhausted),
        AoipError::StreamLost
    );
}

// ---------------------------------------------------------------------------
// H16 逻辑 12 / 容器 16
// ---------------------------------------------------------------------------

#[test]
fn t43_h16_role_slots_match_official_container() {
    for (index, role) in H16Role::ALL.iter().enumerate() {
        assert_eq!(role.index(), index);
        assert_eq!(role.container_slot(), H16_LOGICAL_SLOTS[index]);
        assert_eq!(
            H16Role::from_container_slot(role.container_slot()),
            Some(*role)
        );
    }
    assert_eq!(H16Role::FrontLeft.as_str(), "FL");
    assert_eq!(H16Role::TopRearRight.as_str(), "TRR");
    assert!(H16Role::slot_is_logical(0));
    assert!(H16Role::slot_is_logical(15));
    assert!(!H16Role::slot_is_logical(8));
    assert!(!H16Role::slot_is_logical(9));
    assert!(!H16Role::slot_is_logical(12));
    assert!(!H16Role::slot_is_logical(13));
    assert_eq!(H16Role::from_container_slot(16), None);
}

#[test]
fn t43_h16_logical12_container_mapping() {
    let container = H16Container::logical12();
    assert_eq!(container.capture_channels(), 12);
    assert_eq!(container.flow_count(), 2);
    assert_eq!(container.non_logical_mask(), 0);
    assert_eq!(container.non_logical_count(), 0);
    assert_eq!(container.uncovered_roles_mask(), 0);
    for index in 0..12u16 {
        assert_eq!(
            container.container_slot(index),
            Some(H16_LOGICAL_SLOTS[index as usize])
        );
        assert_eq!(container.role_of(index), Some(H16Role::ALL[index as usize]));
    }
    assert_eq!(container.container_slot(12), None);
    // 12ch 容器里 8/9/12/13 容器 slot 不在 capture 列表：capture 8..11 = TX11/12/15/16。
    assert_eq!(container.container_slot(8), Some(10));
    assert_eq!(container.container_slot(9), Some(11));
    assert_eq!(container.container_slot(10), Some(14));
    assert_eq!(container.container_slot(11), Some(15));
    assert_eq!(container.capture_index_of(H16Role::TopFrontLeft), Some(8));
    assert_eq!(container.capture_index_of(H16Role::TopRearRight), Some(11));
}

#[test]
fn t43_h16_container16_mapping_non_logical() {
    let container = H16Container::container16();
    assert_eq!(container.capture_channels(), 16);
    assert_eq!(container.flow_count(), 2);
    // 非逻辑 capture = 8/9/12/13。
    assert_eq!(container.non_logical_mask(), 0x3300);
    assert_eq!(container.non_logical_count(), 4);
    assert_eq!(container.uncovered_roles_mask(), 0);
    for slot in 0..16u8 {
        let capture = slot as u16;
        assert_eq!(container.container_slot(capture), Some(slot));
        match H16Role::from_container_slot(slot) {
            Some(role) => assert_eq!(container.role_of(capture), Some(role)),
            None => assert_eq!(container.role_of(capture), None),
        }
    }
    assert_eq!(container.role_of(8), None);
    assert_eq!(container.role_of(9), None);
    assert_eq!(container.role_of(12), None);
    assert_eq!(container.role_of(13), None);
    assert_eq!(container.role_of(10), Some(H16Role::TopFrontLeft));
    assert_eq!(container.role_of(15), Some(H16Role::TopRearRight));
    assert_eq!(container.capture_index_of(H16Role::Lfe), Some(3));
}

#[test]
fn t43_h16_flow_config_rejects_invalid() {
    assert_eq!(
        H16FlowConfig::try_from_slots(&[]),
        Err(AoipError::DeviceParams)
    );
    // 重复 slot（跨 flow）。
    assert_eq!(
        H16FlowConfig::try_from_slots(&[&[0, 1, 2], &[2, 3]]),
        Err(AoipError::DeviceParams)
    );
    // slot 越界（16）。
    assert_eq!(
        H16FlowConfig::try_from_slots(&[&[0, 1, 16]]),
        Err(AoipError::BadLayout)
    );
    // 单 flow 超 8 通道。
    assert_eq!(
        H16FlowConfig::try_from_slots(&[&[0, 1, 2, 3, 4, 5, 6, 7, 8]]),
        Err(AoipError::DeviceParams)
    );
    // 超过 4 条 flow。
    assert_eq!(
        H16FlowConfig::try_from_slots(&[&[0], &[1], &[2], &[3], &[4],]),
        Err(AoipError::DeviceParams)
    );
    let config = H16FlowConfig::try_from_slots(&[&[0, 1, 2, 3]]).expect("4ch config");
    assert_eq!(config.flow_count(), 1);
    assert_eq!(config.flow_len(0), Some(4));
    assert_eq!(config.flow_slots(0), Some(&[0u8, 1, 2, 3][..]));
    assert_eq!(config.total_channels(), 4);
    config.validate().expect("valid");
    assert_eq!(H16FlowConfig::logical12().total_channels(), 12);
    assert_eq!(H16FlowConfig::container16().total_channels(), 16);
}

#[test]
fn t43_h16_container_custom_8ch_uncovered_roles() {
    let config = H16FlowConfig::try_from_slots(&[&[0, 1, 2, 3, 4, 5, 6, 7]]).expect("8ch cfg");
    let container = H16Container::from_flow_config(&config).expect("container");
    assert_eq!(container.capture_channels(), 8);
    assert_eq!(container.flow_count(), 1);
    // 高度 4 角色（TFL/TFR/TRL/TRR）未被订阅 → uncovered = bit8..11。
    assert_eq!(container.uncovered_roles_mask(), 0x0f00);
    assert_eq!(container.capture_index_of(H16Role::TopFrontLeft), None);
    assert_eq!(container.capture_index_of(H16Role::BackRight), Some(7));
    assert_eq!(container.role_of(7), Some(H16Role::BackRight));
}

#[test]
fn t43_role_plan_all_selected_explicit_non_logical() {
    let container = H16Container::container16();
    let plan = plan_role_selection(
        &container,
        RoleSelection::ALL,
        RoleSelection::NONE,
        NonLogicalPolicy::ExplicitlyIgnore,
    )
    .expect("all roles explicit ignore");
    assert_eq!(plan.selected, RoleSelection::ALL.mask());
    assert_eq!(plan.ignored, 0);
    assert_eq!(plan.non_logical_ignored, 4);
    assert_eq!(plan.non_logical_policy, NonLogicalPolicy::ExplicitlyIgnore);
    assert_eq!(plan.capture_index(H16Role::FrontLeft), Some(0));
    assert_eq!(plan.capture_index(H16Role::Lfe), Some(3));
    assert_eq!(plan.capture_index(H16Role::TopFrontLeft), Some(10));
    assert_eq!(plan.capture_index(H16Role::TopRearRight), Some(15));
    // 非逻辑 capture 不映射到任何角色。
    for capture in [8u16, 9, 12, 13] {
        assert_eq!(container.role_of(capture), None);
    }
}

#[test]
fn t43_role_plan_requires_explicit_ignore() {
    let container = H16Container::logical12();
    // 已收 LFE 既未选择也未 ignore → 拒收（禁止静默丢弃）。
    let selected = RoleSelection::ALL.without(H16Role::Lfe);
    assert_eq!(
        plan_role_selection(
            &container,
            selected,
            RoleSelection::NONE,
            NonLogicalPolicy::Reject
        ),
        Err(AoipError::Roles)
    );
    let plan = plan_role_selection(
        &container,
        selected,
        RoleSelection::NONE.with(H16Role::Lfe),
        NonLogicalPolicy::Reject,
    )
    .expect("explicit ignore LFE");
    assert_eq!(plan.ignored, 1 << H16Role::Lfe.index());
    assert_eq!(plan.capture_index(H16Role::Lfe), None);
    assert_eq!(plan.capture_index(H16Role::FrontLeft), Some(0));
}

#[test]
fn t43_role_plan_rejects_missing_role() {
    let config = H16FlowConfig::try_from_slots(&[&[0, 1, 2, 3, 4, 5, 6, 7]]).expect("8ch");
    let container = H16Container::from_flow_config(&config).expect("container");
    // 选择未订阅的高度角色 → 不允许静默零填。
    assert_eq!(
        plan_role_selection(
            &container,
            RoleSelection::ALL,
            RoleSelection::NONE,
            NonLogicalPolicy::Reject
        ),
        Err(AoipError::Roles)
    );
    let front = RoleSelection::from_mask(0x00ff);
    let heights = RoleSelection::from_mask(0x0f00);
    let plan = plan_role_selection(&container, front, heights, NonLogicalPolicy::Reject)
        .expect("front selected + heights explicit ignore");
    assert_eq!(plan.capture_index(H16Role::BackRight), Some(7));
    assert_eq!(plan.capture_index(H16Role::TopFrontLeft), None);
    assert_eq!(plan.ignored, 0x0f00);
}

#[test]
fn t43_role_plan_rejects_overlap_unknown_and_policy() {
    let container = H16Container::logical12();
    assert_eq!(
        plan_role_selection(
            &container,
            RoleSelection::ALL,
            RoleSelection::ALL,
            NonLogicalPolicy::Reject
        ),
        Err(AoipError::Roles)
    );
    assert_eq!(
        plan_role_selection(
            &container,
            RoleSelection::from_mask(1 << 15),
            RoleSelection::NONE,
            NonLogicalPolicy::Reject
        ),
        Err(AoipError::Roles)
    );
    let sixteen = H16Container::container16();
    assert_eq!(
        plan_role_selection(
            &sixteen,
            RoleSelection::ALL,
            RoleSelection::NONE,
            NonLogicalPolicy::Reject
        ),
        Err(AoipError::Roles)
    );
    assert!(RoleSelection::from_mask(1 << 15).has_unknown_bits());
    assert!(!RoleSelection::ALL.has_unknown_bits());
    assert_eq!(RoleSelection::ALL.count(), 12);
    assert!(RoleSelection::ALL.contains(H16Role::TopRearRight));
}

// ---------------------------------------------------------------------------
// AES67 SDP 解析/校验
// ---------------------------------------------------------------------------

#[test]
fn t43_sdp_parse_h16_golden() {
    let text = sdp_text(
        "239.69.16.146",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("0"),
    );
    let flow = parse_aes67_sdp(&text).expect("h16 sdp");
    assert_eq!(flow.session_name, Some("NuPrime-a227-5240:16"));
    assert_eq!(flow.connection_addr, "239.69.16.146");
    assert_eq!(flow.connection_ttl, Some(32));
    assert_eq!(flow.port, 5_004);
    assert_eq!(flow.payload_type, 103);
    assert_eq!(flow.encoding, Aes67Encoding::L24);
    assert_eq!(flow.encoding.as_str(), "L24");
    assert_eq!(flow.sample_rate_hz, 48_000);
    assert_eq!(flow.channels, 8);
    assert_eq!(flow.ptime_us, Some(1_000));
    let refclk = flow.ts_refclk.expect("ptp refclk");
    assert_eq!(refclk.domain, 0);
    assert!(refclk.grandmaster_present);
    assert!(flow.mediaclk_direct);
}

#[test]
fn t43_sdp_parses_l16_and_fractional_ptime() {
    let text = sdp_text(
        "239.69.16.148",
        5_004,
        96,
        "L16",
        48_000,
        4,
        Some("0.125"),
        Some("1"),
    );
    let flow = parse_aes67_sdp(&text).expect("l16 sdp");
    assert_eq!(flow.encoding, Aes67Encoding::L16);
    assert_eq!(flow.channels, 4);
    assert_eq!(flow.ptime_us, Some(125));
    assert_eq!(flow.ts_refclk.expect("refclk").domain, 1);
}

#[test]
fn t43_sdp_rejects_wrong_rate() {
    for rate in [96_000u32, 44_100, 32_000] {
        let text = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            "L24",
            rate,
            8,
            Some("1"),
            Some("0"),
        );
        assert_eq!(parse_aes67_sdp(&text), Err(AoipError::Rate));
    }
}

#[test]
fn t43_sdp_rejects_unsupported_encoding() {
    for encoding in ["L20", "OPUS", "L24x"] {
        let text = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            encoding,
            48_000,
            8,
            Some("1"),
            Some("0"),
        );
        assert_eq!(parse_aes67_sdp(&text), Err(AoipError::Unsupported));
    }
}

#[test]
fn t43_sdp_rejects_channel_width() {
    for channels in [0u16, 9, 16] {
        let text = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            "L24",
            48_000,
            channels,
            Some("1"),
            Some("0"),
        );
        assert_eq!(parse_aes67_sdp(&text), Err(AoipError::Channels));
    }
}

#[test]
fn t43_sdp_rejects_non_audio_and_multi_session() {
    let video = "v=0\r\n\
                 s=x\r\n\
                 c=IN IP4 239.69.16.146/32\r\n\
                 m=video 5004 RTP/AVP 96\r\n\
                 a=rtpmap:96 L24/48000/8\r\n";
    assert_eq!(parse_aes67_sdp(video), Err(AoipError::Unsupported));
    let multi = sdp_text(
        "239.69.16.146",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("0"),
    ) + "m=audio 5006 RTP/AVP 104\r\n";
    assert_eq!(parse_aes67_sdp(&multi), Err(AoipError::Unsupported));
}

#[test]
fn t43_sdp_rejects_connection_problems() {
    // 单播地址。
    let unicast = sdp_text(
        "192.168.1.10",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("0"),
    );
    assert_eq!(parse_aes67_sdp(&unicast), Err(AoipError::Unsupported));
    // IPv6。
    let ipv6 = "v=0\r\n\
                s=x\r\n\
                c=IN IP6 ff02::1\r\n\
                m=audio 5004 RTP/AVP 103\r\n\
                a=rtpmap:103 L24/48000/8\r\n\
                a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n";
    assert_eq!(parse_aes67_sdp(ipv6), Err(AoipError::Unsupported));
    // 缺 c=。
    let missing = "v=0\r\n\
                   s=x\r\n\
                   m=audio 5004 RTP/AVP 103\r\n\
                   a=rtpmap:103 L24/48000/8\r\n\
                   a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n";
    assert_eq!(parse_aes67_sdp(missing), Err(AoipError::DeviceParams));
    // TTL 越界。
    for ttl in ["0", "300"] {
        let bad = format!(
            "v=0\r\ns=x\r\nc=IN IP4 239.69.16.146/{ttl}\r\nm=audio 5004 RTP/AVP 103\r\n\
             a=rtpmap:103 L24/48000/8\r\n\
             a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n"
        );
        assert_eq!(parse_aes67_sdp(&bad), Err(AoipError::DeviceParams));
    }
}

#[test]
fn t43_sdp_rejects_bad_ptime() {
    // 合法数值但超出 AES67 ≤1ms 媒体面 → Unsupported。
    for ptime in ["2", "1001", "0"] {
        let text = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            "L24",
            48_000,
            8,
            Some(ptime),
            Some("0"),
        );
        assert_eq!(parse_aes67_sdp(&text), Err(AoipError::Unsupported));
    }
    // 畸形文本（>3 位小数/非数字）→ DeviceParams。
    for ptime in ["1.2345", "abc", "-1"] {
        let malformed = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            "L24",
            48_000,
            8,
            Some(ptime),
            Some("0"),
        );
        assert_eq!(parse_aes67_sdp(&malformed), Err(AoipError::DeviceParams));
    }
}

#[test]
fn t43_sdp_rejects_missing_or_bad_ts_refclk() {
    let missing = sdp_text(
        "239.69.16.146",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        None,
    );
    assert_eq!(parse_aes67_sdp(&missing), Err(AoipError::Unsupported));
    for line in [
        "a=ts-refclk:local\r\n",
        "a=ts-refclk:ptp=IEEE802.1AS:00-11-22-33-44-55-66-77:0\r\n",
    ] {
        let text = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            "L24",
            48_000,
            8,
            Some("1"),
            None,
        ) + line;
        assert_eq!(parse_aes67_sdp(&text), Err(AoipError::Unsupported));
    }
    for bad_ref in [
        "a=ts-refclk:ptp=IEEE1588-2008:zz-11-22-33-44-55-66-77:0\r\n",
        "a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:128\r\n",
    ] {
        let text = sdp_text(
            "239.69.16.146",
            5_004,
            103,
            "L24",
            48_000,
            8,
            Some("1"),
            None,
        ) + bad_ref;
        assert_eq!(parse_aes67_sdp(&text), Err(AoipError::DeviceParams));
    }
}

#[test]
fn t43_sdp_rejects_static_payload_type() {
    let text = sdp_text(
        "239.69.16.146",
        5_004,
        0,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("0"),
    );
    assert_eq!(parse_aes67_sdp(&text), Err(AoipError::Unsupported));
}

// ---------------------------------------------------------------------------
// SDP ↔ 显式 flow 配置交叉校验
// ---------------------------------------------------------------------------

#[test]
fn t43_capture_plan_16ch_two_flows() {
    let plan = plan_16ch();
    assert_eq!(plan.flow_count, 2);
    assert_eq!(plan.total_channels, 16);
    assert_eq!(plan.encoding, Aes67Encoding::L24);
    assert_eq!(plan.sample_rate_hz, AES67_RATE_HZ);
    assert_eq!(plan.ptp_domain, 0);
    assert_eq!(plan.media_len, 2);
    assert_eq!(plan.ptime_us_max, 1_000);
    assert_eq!(plan.container, H16Container::container16());
    assert_eq!(plan.media[0].destination, [239, 69, 16, 146]);
    assert_eq!(plan.media[0].port, 5_004);
    assert_eq!(plan.media[0].payload_type, 103);
    assert_eq!(plan.media[0].channels, 8);
    assert_eq!(plan.media[1].destination, [239, 69, 16, 147]);
    assert_eq!(plan.media[1].channels, 8);
}

#[test]
fn t43_capture_plan_12ch_two_flows() {
    let plan = plan_12ch();
    assert_eq!(plan.flow_count, 2);
    assert_eq!(plan.total_channels, 12);
    assert_eq!(plan.container, H16Container::logical12());
    assert_eq!(plan.media[1].channels, 4);
}

#[test]
fn t43_capture_plan_rejects_flow_count_or_channel_mismatch() {
    let (first, second) = h16_sdp_pair(8, "239.69.16.147", "L24");
    let flow0 = parse_aes67_sdp(&first).expect("flow0");
    let flow1 = parse_aes67_sdp(&second).expect("flow1");
    let two = Aes67FlowSet::new(&[flow0, flow1]).expect("two flows");
    // SDP 2 条 vs 配置 1 条。
    let single = H16FlowConfig::try_from_slots(&[&[0, 1, 2, 3, 4, 5, 6, 7]]).expect("8ch");
    assert_eq!(
        validate_aes67_capture(&two, &single, 0),
        Err(AoipError::DeviceParams)
    );
    // 通道数配置（8+4）与 SDP（8+8）不符。
    assert_eq!(
        validate_aes67_capture(&two, &H16FlowConfig::logical12(), 0),
        Err(AoipError::DeviceParams)
    );
    // 单条 flow SDP vs 2 条配置。
    let one = Aes67FlowSet::new(&[flow0]).expect("one flow");
    assert_eq!(
        validate_aes67_capture(&one, &H16FlowConfig::container16(), 0),
        Err(AoipError::DeviceParams)
    );
}

#[test]
fn t43_capture_plan_rejects_mixed_encoding_and_ptp_domain() {
    let first = sdp_text(
        "239.69.16.146",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("0"),
    );
    let second = sdp_text(
        "239.69.16.147",
        5_004,
        103,
        "L16",
        48_000,
        8,
        Some("1"),
        Some("0"),
    );
    let flow0 = parse_aes67_sdp(&first).expect("flow0");
    let flow1 = parse_aes67_sdp(&second).expect("flow1");
    let mixed = Aes67FlowSet::new(&[flow0, flow1]).expect("set");
    assert_eq!(
        validate_aes67_capture(&mixed, &H16FlowConfig::container16(), 0),
        Err(AoipError::Unsupported)
    );

    let domain_one = sdp_text(
        "239.69.16.147",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("1"),
    );
    let flow1b = parse_aes67_sdp(&domain_one).expect("flow1b");
    let wrong_domain = Aes67FlowSet::new(&[flow0, flow1b]).expect("set");
    assert_eq!(
        validate_aes67_capture(&wrong_domain, &H16FlowConfig::container16(), 0),
        Err(AoipError::Unsupported)
    );
}

#[test]
fn t43_capture_plan_rejects_duplicate_media() {
    let text = sdp_text(
        "239.69.16.146",
        5_004,
        103,
        "L24",
        48_000,
        8,
        Some("1"),
        Some("0"),
    );
    let flow0 = parse_aes67_sdp(&text).expect("flow0");
    let flow1 = parse_aes67_sdp(&text).expect("flow1");
    let duplicate = Aes67FlowSet::new(&[flow0, flow1]).expect("set");
    assert_eq!(
        validate_aes67_capture(&duplicate, &H16FlowConfig::container16(), 0),
        Err(AoipError::DeviceParams)
    );
}

#[test]
fn t43_capture_plan_requires_ptp_refclk() {
    let (first, second) = h16_sdp_pair(8, "239.69.16.147", "L24");
    let flow0 = parse_aes67_sdp(&first).expect("flow0");
    let flow1 = parse_aes67_sdp(&second).expect("flow1");
    let no_refclk = Aes67FlowSdp {
        ts_refclk: None,
        ..flow1
    };
    // flow 级校验就应拒收缺 ts-refclk 的 flow。
    assert_eq!(no_refclk.validate(), Err(AoipError::Unsupported));
    assert_eq!(
        Aes67FlowSet::new(&[flow0, no_refclk]),
        Err(AoipError::Unsupported)
    );
}

#[test]
fn t43_flow_set_limits() {
    assert_eq!(Aes67FlowSet::new(&[]), Err(AoipError::DeviceParams));
    let texts: Vec<String> = (0..4u8)
        .map(|index| {
            sdp_text(
                &format!("239.1.0.{}", index + 1),
                5_004,
                103,
                "L24",
                48_000,
                8,
                Some("1"),
                Some("0"),
            )
        })
        .collect();
    let flows: Vec<Aes67FlowSdp<'_>> = texts
        .iter()
        .map(|text| parse_aes67_sdp(text).expect("flow"))
        .collect();
    assert_eq!(flows.len(), 4);
    // 4 条 8ch flow 的通道总数 32 > 16 容器上限 → 拒收。
    assert_eq!(Aes67FlowSet::new(&flows), Err(AoipError::Channels));
    let texts5: Vec<String> = (0..5u8)
        .map(|index| {
            sdp_text(
                &format!("239.2.0.{}", index + 1),
                5_004,
                103,
                "L24",
                48_000,
                8,
                Some("1"),
                Some("0"),
            )
        })
        .collect();
    let flows5: Vec<Aes67FlowSdp<'_>> = texts5
        .iter()
        .map(|text| parse_aes67_sdp(text).expect("flow"))
        .collect();
    assert_eq!(Aes67FlowSet::new(&flows5), Err(AoipError::DeviceParams));
}

// ---------------------------------------------------------------------------
// 端点规格 / ALSA 稳定 ID / pipe / Inferno 环境
// ---------------------------------------------------------------------------

#[test]
fn t43_aes67_alsa_endpoint_resolves_ravenna_card() {
    let plan = plan_16ch();
    let device = plan
        .resolve_alsa_device(CARDS_RAVENNA, AES67_CARD_ID_DEFAULT)
        .expect("resolved");
    assert_eq!(device.card_index, 6);
    assert_eq!(device.device_index, 0);
    let mut name = [0u8; 16];
    let length = device.write_hw_name(&mut name).expect("hw name");
    assert_eq!(&name[..length], b"hw:6,0");
    let spec = plan
        .alsa_endpoint(CARDS_RAVENNA, AES67_CARD_ID_DEFAULT)
        .expect("alsa endpoint");
    assert_eq!(spec.protocol, AoipProtocol::Aes67);
    assert_eq!(spec.transport, AoipTransport::Alsa);
    assert_eq!(spec.alsa_device, Some(AlsaDevice::Hw(device)));
    assert_eq!(spec.sample_rate_hz, AES67_RATE_HZ);
    assert_eq!(spec.device_format, sample_format::S32_LE_INTERLEAVED);
    assert_eq!(spec.ptp_source, PtpSource::Aes67DaemonBuiltinPtpV2);
    assert_eq!(spec.container.capture_channels(), 16);
    assert!(spec.ptp_source_matches());
    spec.validate().expect("spec valid");
}

#[test]
fn t43_aes67_alsa_endpoint_rejects_missing_or_ambiguous_card() {
    let plan = plan_16ch();
    assert_eq!(
        plan.resolve_alsa_device(CARDS_RAVENNA_AMBIGUOUS, AES67_CARD_ID_DEFAULT),
        Err(AoipError::DeviceParams)
    );
    assert_eq!(
        plan.resolve_alsa_device(CARDS_RAVENNA, "NOT_THERE"),
        Err(AoipError::DeviceParams)
    );
    assert_eq!(
        plan.alsa_endpoint(CARDS_RAVENNA_AMBIGUOUS, AES67_CARD_ID_DEFAULT),
        Err(AoipError::DeviceParams)
    );
}

#[test]
fn t43_endpoint_spec_rejects_rate_format_period_buffer() {
    let device = ravenna_device();
    let container = H16Container::logical12();
    let mut spec = AoipEndpointSpec::aes67_alsa(device, container);
    spec.validate().expect("baseline valid");

    spec.sample_rate_hz = INFERNO_RATE_HZ;
    assert_eq!(spec.validate(), Err(AoipError::Rate));
    spec.sample_rate_hz = AES67_RATE_HZ;

    spec.device_format = sample_format::S24_3LE_INTERLEAVED;
    assert_eq!(spec.validate(), Err(AoipError::Unsupported));
    spec.device_format = sample_format::S32_LE_INTERLEAVED;

    spec.period_frames = 32;
    assert_eq!(spec.validate(), Err(AoipError::BlockSize));
    spec.period_frames = 64;
    spec.buffer_frames = 100;
    assert_eq!(spec.validate(), Err(AoipError::DeviceParams));
    spec.buffer_frames = 512;
    spec.validate().expect("64/512 valid");

    spec.max_stall_periods = 0;
    assert_eq!(spec.validate(), Err(AoipError::DeviceParams));
    spec.max_stall_periods = DEFAULT_MAX_STALL_PERIODS;
    spec.container = H16FlowConfig::try_from_slots(&[&[0, 1]])
        .map(|config| H16Container::from_flow_config(&config).expect("container"))
        .expect("2ch config");
    spec.validate().expect("2ch container valid");
}

#[test]
fn t43_endpoint_spec_rejects_ptp_source_mismatch() {
    let device = ravenna_device();
    let mut spec = AoipEndpointSpec::aes67_alsa(device, H16Container::logical12());
    spec.ptp_source = PtpSource::StatimePtpV1VirtualClock;
    assert!(!spec.ptp_source_matches());
    assert_eq!(spec.validate(), Err(AoipError::Unsupported));
}

#[test]
fn t43_endpoint_spec_rejects_alsa_device_mismatch() {
    let container = H16Container::logical12();
    let hw = ravenna_device();
    let mut aes67_named = AoipEndpointSpec::aes67_alsa(hw, container);
    aes67_named.alsa_device = Some(AlsaDevice::Named(INFERNO_ALSA_DEVICE));
    assert_eq!(aes67_named.validate(), Err(AoipError::DeviceParams));

    let mut aes67_device_one = AoipEndpointSpec::aes67_alsa(hw, container);
    aes67_device_one.alsa_device = Some(AlsaDevice::Hw(ResolvedDevice {
        card_index: 6,
        device_index: 1,
    }));
    assert_eq!(aes67_device_one.validate(), Err(AoipError::DeviceParams));

    let mut inferno_hw = AoipEndpointSpec::inferno_alsa(container);
    inferno_hw.alsa_device = Some(AlsaDevice::Hw(hw));
    assert_eq!(inferno_hw.validate(), Err(AoipError::Unsupported));

    let mut inferno_default = AoipEndpointSpec::inferno_alsa(container);
    inferno_default.alsa_device = Some(AlsaDevice::Named("default"));
    assert_eq!(inferno_default.validate(), Err(AoipError::DeviceParams));

    let mut inferno_none = AoipEndpointSpec::inferno_alsa(container);
    inferno_none.alsa_device = None;
    assert_eq!(inferno_none.validate(), Err(AoipError::Unsupported));
}

#[test]
fn t43_aes67_pipe_endpoint_and_frame_math() {
    let plan = plan_16ch();
    let spec = plan
        .pipe_endpoint("/run/tspi/aes67.raw")
        .expect("pipe spec");
    assert_eq!(spec.transport, AoipTransport::Pipe);
    assert_eq!(spec.pipe_path, Some("/run/tspi/aes67.raw"));
    assert!(spec.alsa_device.is_none());
    assert_eq!(spec.frame_bytes().expect("frame bytes"), 16 * 4);
    assert_eq!(spec.validate_pipe_chunk(64 * 128).expect("chunk"), 128);
    assert_eq!(spec.validate_pipe_chunk(0).expect("zero"), 0);
    assert_eq!(spec.validate_pipe_chunk(48), Err(AoipError::DeviceParams));
    assert_eq!(
        spec.validate_pipe_chunk(MAX_PIPE_CHUNK_BYTES + 64),
        Err(AoipError::BadLayout)
    );
    // pipe chunk 校验只对 pipe 端点有意义。
    let mut alsa = AoipEndpointSpec::aes67_alsa(ravenna_device(), H16Container::logical12());
    alsa = alsa.with_period_buffer(128, 1_024);
    assert_eq!(alsa.validate_pipe_chunk(64), Err(AoipError::Unsupported));
}

#[test]
fn t43_pipe_endpoint_rejects_bad_path() {
    let plan = plan_12ch();
    assert_eq!(plan.pipe_endpoint(""), Err(AoipError::DeviceParams));
    let mut spec = plan.pipe_endpoint("/run/x.raw").expect("pipe");
    spec.alsa_device = Some(AlsaDevice::Named("inferno"));
    assert_eq!(spec.validate(), Err(AoipError::DeviceParams));
    spec.alsa_device = None;
    spec.pipe_path = None;
    assert_eq!(spec.validate(), Err(AoipError::DeviceParams));
}

#[test]
fn t43_inferno_env_validation() {
    let container = H16Container::container16();
    inferno_env_16().validate(&container).expect("env valid");
    InfernoEnv::new(
        "auto",
        INFERNO_CLOCK_PATH_DEFAULT,
        "end0",
        INFERNO_RATE_HZ,
        16,
    )
    .validate(&container)
    .expect("auto id valid");
    assert!(inferno_device_id_valid("00000A00020F0001"));
    assert!(!inferno_device_id_valid("0000"));
    assert!(!inferno_device_id_valid("zzzzzzzzzzzzzzzz"));

    let wrong_rate = InfernoEnv::new(
        "auto",
        INFERNO_CLOCK_PATH_DEFAULT,
        "end0",
        AES67_RATE_HZ,
        16,
    );
    assert_eq!(wrong_rate.validate(&container), Err(AoipError::Rate));
    let wrong_channels = InfernoEnv::new(
        "auto",
        INFERNO_CLOCK_PATH_DEFAULT,
        "end0",
        INFERNO_RATE_HZ,
        12,
    );
    assert_eq!(
        wrong_channels.validate(&container),
        Err(AoipError::Channels)
    );
    let wrong_clock = InfernoEnv::new("auto", "tmp/ptp", "end0", INFERNO_RATE_HZ, 16);
    assert_eq!(
        wrong_clock.validate(&container),
        Err(AoipError::DeviceParams)
    );
    let empty_iface = InfernoEnv::new("auto", INFERNO_CLOCK_PATH_DEFAULT, "", INFERNO_RATE_HZ, 16);
    assert_eq!(
        empty_iface.validate(&container),
        Err(AoipError::DeviceParams)
    );
    let bad_id = InfernoEnv::new(
        "xyz",
        INFERNO_CLOCK_PATH_DEFAULT,
        "end0",
        INFERNO_RATE_HZ,
        16,
    );
    assert_eq!(bad_id.validate(&container), Err(AoipError::DeviceParams));
}

#[test]
fn t43_inferno_alas_and_pipe_endpoints() {
    let container = H16Container::container16();
    let spec = inferno_alsa_endpoint(&inferno_env_16(), container).expect("inferno alsa");
    assert_eq!(spec.protocol, AoipProtocol::Inferno);
    assert_eq!(spec.transport, AoipTransport::Alsa);
    assert_eq!(
        spec.alsa_device,
        Some(AlsaDevice::Named(INFERNO_ALSA_DEVICE))
    );
    assert_eq!(spec.sample_rate_hz, INFERNO_RATE_HZ);
    assert_eq!(spec.ptp_source, PtpSource::StatimePtpV1VirtualClock);
    spec.validate().expect("inferno spec valid");

    let pipe = inferno_pipe_endpoint(&inferno_env_16(), container, "/run/tspi/inferno.raw")
        .expect("inferno pipe");
    assert_eq!(pipe.transport, AoipTransport::Pipe);
    assert_eq!(pipe.frame_bytes().expect("frame"), 64);
    assert_eq!(pipe.validate_pipe_chunk(64 * 64).expect("frames"), 64);

    let bad_env = InfernoEnv::new("auto", INFERNO_CLOCK_PATH_DEFAULT, "end0", 48_000, 16);
    assert_eq!(
        inferno_alsa_endpoint(&bad_env, container),
        Err(AoipError::Rate)
    );
}

#[test]
fn t43_aes67_service_and_preflight() {
    let plan = plan_16ch();
    let ready = Aes67ServiceState::new(true, AES67_RATE_HZ, 0);
    validate_aes67_service(&ready, &plan).expect("ready");
    let not_ready = Aes67ServiceState::new(false, AES67_RATE_HZ, 0);
    assert_eq!(
        validate_aes67_service(&not_ready, &plan),
        Err(AoipError::StreamLost)
    );
    let wrong_rate = Aes67ServiceState::new(true, INFERNO_RATE_HZ, 0);
    assert_eq!(
        validate_aes67_service(&wrong_rate, &plan),
        Err(AoipError::Rate)
    );
    let wrong_domain = Aes67ServiceState::new(true, AES67_RATE_HZ, 1);
    assert_eq!(
        validate_aes67_service(&wrong_domain, &plan),
        Err(AoipError::Unsupported)
    );

    let spec = plan
        .alsa_endpoint(CARDS_RAVENNA, AES67_CARD_ID_DEFAULT)
        .expect("spec");
    preflight(&spec, LockState::Locked).expect("preflight locked");
    assert_eq!(
        preflight(&spec, LockState::Locking),
        Err(AoipError::PtpUnavailable)
    );
    assert_eq!(
        preflight(&spec, LockState::Unlocked),
        Err(AoipError::PtpUnavailable)
    );
    assert_eq!(
        preflight(&spec, LockState::Holdover),
        Err(AoipError::PtpUnavailable)
    );
    let mut bad = spec;
    bad.period_frames = 32;
    assert_eq!(
        preflight(&bad, LockState::Locked),
        Err(AoipError::BlockSize)
    );
}

// ---------------------------------------------------------------------------
// 失流监视器（fake nanos/帧数）
// ---------------------------------------------------------------------------

#[test]
fn t43_monitor_first_data_and_flow() {
    let mut monitor = monitor_48k();
    assert_eq!(monitor.state(), MonitorState::Idle);
    assert_eq!(monitor.state().as_str(), "idle");
    assert_eq!(monitor.t_block_ns(), 2_666_667);
    assert_eq!(monitor.stall_threshold_ns(), 8_000_001);
    assert_eq!(monitor.lost_threshold_ns(), 16_000_002);
    monitor.start(1_000);
    assert_eq!(monitor.state(), MonitorState::WaitingFirstData);
    assert_eq!(monitor.poll(1_000), MonitorEvent::None);
    assert_eq!(monitor.on_data(2_000, 128), Ok(MonitorEvent::Started));
    assert_eq!(monitor.state(), MonitorState::Flowing);
    assert_eq!(monitor.on_data(3_000, 128), Ok(MonitorEvent::None));
    assert_eq!(monitor.frames_total(), 256);
    assert_eq!(monitor.data_events(), 2);
    assert_eq!(monitor.poll(4_000), MonitorEvent::None);
}

#[test]
fn t43_monitor_stall_then_resume_same_epoch() {
    let mut monitor = monitor_48k();
    monitor.start(0);
    monitor.on_data(0, 128).expect("started");
    let stall_at = monitor.stall_threshold_ns() + 1;
    assert_eq!(
        monitor.poll(stall_at),
        MonitorEvent::Stalled { silent_periods: 3 }
    );
    assert_eq!(monitor.state(), MonitorState::Stalled);
    assert_eq!(monitor.stall_events(), 1);
    // 未到持续失流门限前数据恢复 → 同 epoch Resumed。
    assert_eq!(
        monitor.on_data(stall_at + 1, 128),
        Ok(MonitorEvent::Resumed { silent_periods: 3 })
    );
    assert_eq!(monitor.state(), MonitorState::Flowing);
    assert_eq!(monitor.resume_events(), 1);
    assert_eq!(monitor.loss_events(), 0);
}

#[test]
fn t43_monitor_stall_escalates_to_lost() {
    let mut monitor = monitor_48k();
    monitor.start(0);
    monitor.on_data(0, 128).expect("started");
    let stall_at = monitor.stall_threshold_ns() + 1;
    assert!(matches!(
        monitor.poll(stall_at),
        MonitorEvent::Stalled { .. }
    ));
    let lost_at = monitor.lost_threshold_ns() + 1;
    match monitor.poll(lost_at) {
        MonitorEvent::Lost { silent_periods } => assert!(silent_periods >= 3),
        other => panic!("expected Lost, got {:?}", other),
    }
    assert_eq!(monitor.state(), MonitorState::Lost);
    assert_eq!(monitor.loss_events(), 1);
    // 幂等：重复 poll 不再产生事件/计数。
    assert_eq!(monitor.poll(lost_at + 1), MonitorEvent::None);
    assert_eq!(monitor.loss_events(), 1);
    // Lost 后数据必须先 reopen 新 epoch。
    assert_eq!(
        monitor.on_data(lost_at + 2, 128),
        Err(AoipError::EpochMismatch)
    );
}

#[test]
fn t43_monitor_threshold_boundary_is_strict() {
    let mut monitor = monitor_48k();
    monitor.start(0);
    monitor.on_data(0, 128).expect("started");
    let threshold = monitor.stall_threshold_ns();
    // 恰好等于门限：尚未停滞（严格大于才触发）。
    assert_eq!(monitor.poll(threshold), MonitorEvent::None);
    assert_eq!(
        monitor.poll(threshold + 1),
        MonitorEvent::Stalled { silent_periods: 3 }
    );
}

#[test]
fn t43_monitor_waiting_first_data_timeout() {
    let mut monitor = monitor_48k();
    monitor.start(0);
    assert_eq!(
        monitor.poll(monitor.stall_threshold_ns()),
        MonitorEvent::None
    );
    assert!(matches!(
        monitor.poll(monitor.stall_threshold_ns() + 1),
        MonitorEvent::Stalled { .. }
    ));
    assert!(matches!(
        monitor.poll(monitor.lost_threshold_ns() + 1),
        MonitorEvent::Lost { .. }
    ));
    // 96k profile 门限不同（ceil T_block = 1333334 ns）。
    let monitor_96k = StreamMonitor::new(128, 96_000, 3).expect("96k monitor");
    assert_eq!(monitor_96k.t_block_ns(), 1_333_334);
    assert_eq!(monitor_96k.stall_threshold_ns(), 4_000_002);
}

#[test]
fn t43_monitor_rejects_bad_construction_and_zero_data() {
    assert_eq!(StreamMonitor::new(32, 48_000, 3), Err(AoipError::BlockSize));
    assert_eq!(StreamMonitor::new(128, 44_100, 3), Err(AoipError::Rate));
    assert_eq!(
        StreamMonitor::new(128, 48_000, 0),
        Err(AoipError::DeviceParams)
    );
    let mut monitor = monitor_48k();
    monitor.start(0);
    assert_eq!(monitor.on_data(0, 0), Err(AoipError::DeviceParams));
    // Idle 状态的数据没有 start。
    let mut idle = monitor_48k();
    assert_eq!(idle.on_data(0, 128), Err(AoipError::SwitchBusy));
}

// ---------------------------------------------------------------------------
// 端点状态机
// ---------------------------------------------------------------------------

#[test]
fn t43_endpoint_start_waits_for_ptp() {
    let mut endpoint = endpoint_inferno_16();
    assert_eq!(endpoint.state(), AoipState::Closed);
    assert!(endpoint.mute_required());
    assert_eq!(endpoint.ptp_source(), PtpSource::StatimePtpV1VirtualClock);
    assert_eq!(
        endpoint.start(1, LockState::Unlocked, 0),
        Err(AoipError::PtpUnavailable)
    );
    assert_eq!(endpoint.state(), AoipState::WaitingPtp);
    assert_eq!(endpoint.epoch_id(), 1);
    assert_eq!(endpoint.on_data(0, 128), Err(AoipError::SwitchBusy));
    assert_eq!(
        endpoint.on_ptp(LockState::Locked, 1_000),
        Ok(StreamEvent::Started)
    );
    assert_eq!(endpoint.state(), AoipState::Streaming);
    assert!(!endpoint.mute_required());
    assert_eq!(endpoint.on_data(2_000, 128), Ok(StreamEvent::Started));
    assert_eq!(endpoint.poll(3_000), StreamEvent::None);
    assert_eq!(endpoint.counters().frames, 128);
    assert_eq!(endpoint.counters().data_events, 1);
}

#[test]
fn t43_endpoint_start_rejects_zero_epoch_and_double_start() {
    let spec = AoipEndpointSpec::inferno_alsa(H16Container::container16());
    let mut endpoint = AoipEndpoint::new(spec).expect("endpoint");
    assert_eq!(
        endpoint.start(0, LockState::Locked, 0),
        Err(AoipError::BadLayout)
    );
    assert_eq!(endpoint.state(), AoipState::Closed);
    assert_eq!(
        endpoint.start(1, LockState::Locked, 0),
        Ok(StreamEvent::Started)
    );
    assert_eq!(
        endpoint.start(2, LockState::Locked, 1_000),
        Err(AoipError::DeviceBusy)
    );
    assert_eq!(endpoint.counters().rejected_calls, 2);
}

#[test]
fn t43_endpoint_stall_mutes_then_resumes() {
    let mut endpoint = endpoint_inferno_16();
    endpoint.start(1, LockState::Locked, 0).expect("started");
    endpoint.on_data(0, 128).expect("first data");
    let stall_at = endpoint.spec().period_frames;
    let threshold = StreamMonitor::new(128, 96_000, DEFAULT_MAX_STALL_PERIODS)
        .expect("monitor")
        .stall_threshold_ns();
    assert_eq!(stall_at, 128);
    let event = endpoint.poll(threshold + 1);
    match event {
        StreamEvent::Stalled { silent_periods } => assert_eq!(silent_periods, 3),
        other => panic!("expected Stalled, got {:?}", other),
    }
    assert_eq!(endpoint.state(), AoipState::Muted);
    assert!(endpoint.mute_required());
    assert_eq!(endpoint.counters().stall_events, 1);
    assert_eq!(endpoint.counters().mute_events, 1);
    let resumed = endpoint.on_data(threshold + 2, 128).expect("resume data");
    assert_eq!(resumed, StreamEvent::Resumed { silent_periods: 3 });
    assert_eq!(endpoint.state(), AoipState::Streaming);
    assert!(!endpoint.mute_required());
    assert_eq!(endpoint.counters().resume_events, 1);
}

#[test]
fn t43_endpoint_lost_requires_new_epoch() {
    let mut endpoint = endpoint_inferno_16();
    endpoint.start(1, LockState::Locked, 0).expect("started");
    endpoint.on_data(0, 128).expect("data");
    let monitor = StreamMonitor::new(128, 96_000, DEFAULT_MAX_STALL_PERIODS).expect("monitor");
    let lost_at = monitor.lost_threshold_ns() + 1;
    match endpoint.poll(lost_at) {
        StreamEvent::Lost {
            discontinuity,
            silent_periods,
        } => {
            assert_eq!(discontinuity, DiscontinuityReason::Explicit);
            assert!(silent_periods >= 6);
        }
        other => panic!("expected Lost, got {:?}", other),
    }
    assert_eq!(endpoint.state(), AoipState::Lost);
    assert!(endpoint.needs_new_epoch());
    assert!(endpoint.mute_required());
    assert_eq!(
        endpoint.on_data(lost_at + 1, 128),
        Err(AoipError::EpochMismatch)
    );
    // 同一 epoch / 更旧 epoch 都不能恢复。
    assert_eq!(
        endpoint.reopen(1, LockState::Locked, lost_at + 2),
        Err(AoipError::EpochMismatch)
    );
    // PTP 未锁也不能恢复。
    assert_eq!(
        endpoint.reopen(2, LockState::Unlocked, lost_at + 2),
        Err(AoipError::PtpUnavailable)
    );
    assert_eq!(
        endpoint.reopen(2, LockState::Locked, lost_at + 2),
        Ok(StreamEvent::Reopened { epoch_id: 2 })
    );
    assert_eq!(endpoint.state(), AoipState::Streaming);
    assert!(!endpoint.needs_new_epoch());
    assert_eq!(endpoint.on_data(lost_at + 3, 128), Ok(StreamEvent::Started));
    assert_eq!(endpoint.counters().epoch_reopens, 1);
    assert_eq!(endpoint.counters().loss_events, 1);
}

#[test]
fn t43_endpoint_reopen_rejected_when_not_lost() {
    let mut endpoint = endpoint_inferno_16();
    endpoint.start(1, LockState::Locked, 0).expect("started");
    assert_eq!(
        endpoint.reopen(2, LockState::Locked, 1),
        Err(AoipError::SwitchBusy)
    );
    let mut closed = endpoint_inferno_16();
    assert_eq!(
        closed.reopen(2, LockState::Locked, 1),
        Err(AoipError::SwitchBusy)
    );
}

#[test]
fn t43_endpoint_ptp_unlock_causes_lost() {
    let mut endpoint = endpoint_inferno_16();
    endpoint.start(1, LockState::Locked, 0).expect("started");
    endpoint.on_data(0, 128).expect("data");
    match endpoint.on_ptp(LockState::Holdover, 1_000) {
        Ok(StreamEvent::PtpLost { discontinuity }) => {
            assert_eq!(discontinuity, DiscontinuityReason::PtpTimeout)
        }
        other => panic!("expected PtpLost, got {:?}", other),
    }
    assert_eq!(endpoint.state(), AoipState::Lost);
    assert!(endpoint.needs_new_epoch());
    assert_eq!(endpoint.counters().ptp_unlock_events, 1);
    // 失流后 PTP 復锁不自动恢复（必须 reopen 新 epoch）。
    assert_eq!(
        endpoint.on_ptp(LockState::Locked, 2_000),
        Ok(StreamEvent::None)
    );
    assert_eq!(endpoint.state(), AoipState::Lost);
    // Closed 状态下 PTP 回调是非法迁移。
    let mut closed = endpoint_inferno_16();
    assert_eq!(
        closed.on_ptp(LockState::Locked, 0),
        Err(AoipError::SwitchBusy)
    );
}

#[test]
fn t43_endpoint_eof_is_lost_once() {
    let mut endpoint = endpoint_inferno_16();
    endpoint.start(1, LockState::Locked, 0).expect("started");
    endpoint.on_data(0, 128).expect("data");
    match endpoint.on_source_eof(1_000) {
        StreamEvent::Lost { discontinuity, .. } => {
            assert_eq!(discontinuity, DiscontinuityReason::Explicit)
        }
        other => panic!("expected Lost, got {:?}", other),
    }
    assert_eq!(endpoint.state(), AoipState::Lost);
    assert!(endpoint.needs_new_epoch());
    assert_eq!(endpoint.on_source_eof(2_000), StreamEvent::None);
    assert_eq!(endpoint.counters().loss_events, 1);
}

#[test]
fn t43_endpoint_stop_and_restart() {
    let mut endpoint = endpoint_inferno_16();
    endpoint.start(1, LockState::Locked, 0).expect("started");
    endpoint.on_data(0, 128).expect("data");
    endpoint.stop();
    assert_eq!(endpoint.state(), AoipState::Closed);
    assert_eq!(endpoint.epoch_id(), 0);
    assert!(endpoint.mute_required());
    assert_eq!(endpoint.counters().frames, 128);
    assert_eq!(
        endpoint.start(3, LockState::Locked, 10_000),
        Ok(StreamEvent::Started)
    );
    assert_eq!(endpoint.epoch_id(), 3);
    assert_eq!(endpoint.state(), AoipState::Streaming);
}

#[test]
fn t43_epoch_clock_integration() {
    use crate::clock::EpochClock;

    let mut epochs = EpochClock::new();
    let first = epochs.begin(0).expect("epoch begin");
    let mut endpoint = endpoint_inferno_16();
    endpoint
        .start(first.epoch_id, LockState::Locked, 0)
        .expect("started");
    endpoint.on_data(0, 128).expect("data");
    let monitor = StreamMonitor::new(128, 96_000, DEFAULT_MAX_STALL_PERIODS).expect("monitor");
    let lost_at = monitor.lost_threshold_ns() + 1;
    assert!(matches!(endpoint.poll(lost_at), StreamEvent::Lost { .. }));
    let discontinuity = epochs
        .on_discontinuity(DiscontinuityReason::Explicit, lost_at + 1, None, None)
        .expect("discontinuity");
    assert_eq!(discontinuity.old_epoch_id, first.epoch_id);
    assert_eq!(discontinuity.new_epoch_id, first.epoch_id + 1);
    // 旧 epoch 被拒，新 epoch 接受。
    assert_eq!(
        endpoint.reopen(discontinuity.old_epoch_id, LockState::Locked, lost_at + 2),
        Err(AoipError::EpochMismatch)
    );
    assert_eq!(
        endpoint.reopen(discontinuity.new_epoch_id, LockState::Locked, lost_at + 2),
        Ok(StreamEvent::Reopened {
            epoch_id: discontinuity.new_epoch_id
        })
    );
    assert_eq!(endpoint.epoch_id(), discontinuity.new_epoch_id);
}

// ---------------------------------------------------------------------------
// 交付纪律：无硬件/无协议写路径；类型 POD；源码扫描
// ---------------------------------------------------------------------------

#[test]
fn t43_types_are_pod_no_drop() {
    fn no_drop<T>() {
        assert!(!core::mem::needs_drop::<T>());
    }
    no_drop::<AoipError>();
    no_drop::<AoipProtocol>();
    no_drop::<AoipTransport>();
    no_drop::<PtpSource>();
    no_drop::<H16Role>();
    no_drop::<H16FlowConfig>();
    no_drop::<H16Container>();
    no_drop::<RoleSelection>();
    no_drop::<NonLogicalPolicy>();
    no_drop::<RolePlan>();
    no_drop::<Aes67Encoding>();
    no_drop::<PtpRefClock>();
    no_drop::<Aes67FlowSdp<'static>>();
    no_drop::<Aes67FlowSet<'static>>();
    no_drop::<Aes67FlowMedia>();
    no_drop::<Aes67CapturePlan>();
    no_drop::<Aes67ServiceState>();
    no_drop::<InfernoEnv<'static>>();
    no_drop::<AlsaDevice<'static>>();
    no_drop::<AoipEndpointSpec<'static>>();
    no_drop::<MonitorState>();
    no_drop::<MonitorEvent>();
    no_drop::<StreamMonitor>();
    no_drop::<AoipState>();
    no_drop::<StreamEvent>();
    no_drop::<AoipCounters>();
    no_drop::<AoipEndpoint<'static>>();
}

#[test]
fn t43_source_has_no_hardware_or_protocol_write_path() {
    let source = include_str!("../../src/aoip.rs");
    // 不打开硬件/不写文件/不起进程/不实现协议栈。
    for forbidden in [
        "unsafe ",
        "std::fs",
        "std::process",
        "Command::new",
        "TcpStream",
        "UdpSocket",
        "std::net",
        "extern \"C\"",
        "libc::",
    ] {
        assert!(
            !source.contains(forbidden),
            "aoip.rs must not contain {}",
            forbidden
        );
    }
    // 声明为 NONRT/IO 模块，且测试 include! 走 leaf 私有注册。
    assert!(source.contains("[NONRT]/[IO]"));
    assert!(source.contains("include!(\"../tests/T43/aoip_fake.rs\")"));
}
