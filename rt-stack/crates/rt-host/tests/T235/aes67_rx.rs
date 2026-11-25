// T235（自研用户态 AES67 接收端 → host capture 适配）离线测试集。
//
// 本文件经 src/aes67_rx.rs 末尾的 cfg(test) mod aes67_rx_proto_tests { include!(...) }
// 进入单测构建（cargo 不自动发现 tests/<子目录>）。运行：
//   cargo test -p khaen-rt-host aes67_rx_proto
// 全部测试名 t235_ 前缀；离线：不打开 ALSA/设备、不连板端；UDP 仅本机回环（bind smoke）。

use super::*;
use khaen_aoip_aes67::clock::frames_to_ns_ceil;
use khaen_aoip_aes67::jitter::{self, JitterConfig};
use khaen_aoip_aes67::rtp::Header;

/// 单 flow 2ch 自定义计划（H16 slot 0/1 = FL/FR）。
fn stereo_plan() -> Aes67RxFlowPlan {
    Aes67RxFlowPlan::Custom(H16FlowConfig::try_from_slots(&[&[0u8, 1u8]]).expect("stereo slots"))
}

/// 1 ms 包（48 帧）RTP 头。
fn packet_header(sequence: u16, timestamp: u32, channels: u16) -> Header {
    Header {
        version: 2,
        padding: false,
        extension: false,
        csrc_count: 0,
        marker: false,
        payload_type: 96 + channels as u8,
        sequence,
        timestamp,
        ssrc: 0x2350_0001,
    }
}

fn jitter_config(channels: u16) -> JitterConfig {
    JitterConfig::new(channels, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000).expect("jitter config")
}

/// 推 4 包（48 帧/包 = 192 帧）交错样本。
fn push_packets(
    writer: &jitter::JitterWriter,
    now_ns: u64,
    channels: u16,
    channels_fn: impl Fn(usize, usize) -> i32,
    packets: usize,
) {
    let frames = 48usize;
    for packet in 0..packets {
        let mut samples = vec![0i32; frames * channels as usize];
        for frame in 0..frames {
            for channel in 0..channels as usize {
                samples[frame * channels as usize + channel] = channels_fn(frame, channel);
            }
        }
        let result = writer.push_interleaved(
            now_ns,
            &packet_header(packet as u16, packet as u32 * frames as u32, channels),
            &samples,
        );
        assert!(
            matches!(result, jitter::PushResult::Accepted { .. }),
            "{result:?}"
        );
    }
}

fn block_now(origin: u64, latency_frames: u64, index: u64) -> u64 {
    origin
        + frames_to_ns_ceil(
            latency_frames + index * AES67_RX_BLOCK_FRAMES as u64,
            48_000,
        )
}

#[test]
fn t235_aes67rx_stereo_single_flow_maps_fl_fr_planar() {
    let config = jitter_config(2);
    let (writer, reader) = jitter::channel(config).expect("channel");
    let adapter = Aes67RxAdapter::new(stereo_plan(), 2).expect("adapter");
    assert_eq!(adapter.flow_count(), 1);
    assert_eq!(adapter.flow_channels(0), Some(2));
    assert_eq!(adapter.container_channels().unwrap(), 2);
    let mut capture = Aes67RxCapture::from_readers(vec![reader], adapter).expect("capture");
    assert_eq!(capture.layout(), AES67_RX_LAYOUT_STEREO);
    assert_eq!(capture.capture_index(), &[Some(0), Some(1)]);

    let origin = 1_000u64;
    // ch0 = +0.125 FS，ch1 = -0.0625 FS（满量程 i32 精确值）。
    push_packets(
        &writer,
        origin,
        2,
        |_frame, channel| {
            if channel == 0 {
                0x1000_0000
            } else {
                -0x0800_0000
            }
        },
        4,
    );
    let stride = AES67_RX_BLOCK_FRAMES;
    let mut planar = vec![9.0f32; 2 * stride];
    let now = block_now(origin, capture_latency(&capture), 0);
    let status = capture
        .fill_block(&mut planar, stride, AES67_RX_BLOCK_FRAMES, now)
        .expect("fill block");
    assert_eq!(status, BlockStatus::Ok);
    assert_eq!(planar[0], 0x1000_0000u32 as i32 as f32 / 2_147_483_648.0);
    assert_eq!(planar[1 * stride], (-0x0800_0000) as f32 / 2_147_483_648.0);
    assert_eq!(capture.counters().ok_blocks, 1);
    assert_eq!(
        capture.counters().frames_delivered,
        AES67_RX_BLOCK_FRAMES as u64
    );
    let report = capture.report_json();
    assert!(report.contains("\"layout\":\"2.0-strict\""));
    assert!(report.contains("\"capture_index\":[0,1]"));
}

/// 读取 adapter 的 latency（CaptureBlockSource reader 延迟；测试辅助）。
fn capture_latency(capture: &Aes67RxCapture) -> u64 {
    capture.sources[0].latency_frames()
}

#[test]
fn t235_aes67rx_logical12_8ch_412_role_map_and_empty_slot() {
    let flow0_config = jitter_config(8);
    let flow1_config = jitter_config(4);
    let (writer0, reader0) = jitter::channel(flow0_config).expect("flow0");
    let (writer1, reader1) = jitter::channel(flow1_config).expect("flow1");
    let adapter = Aes67RxAdapter::new(Aes67RxFlowPlan::Logical12, 8).expect("adapter logical12");
    assert_eq!(adapter.flow_count(), 2);
    assert_eq!(adapter.flow_channels(0), Some(8));
    assert_eq!(adapter.flow_channels(1), Some(4));
    assert_eq!(adapter.container_channels().unwrap(), 12);
    let mut capture =
        Aes67RxCapture::from_readers(vec![reader0, reader1], adapter).expect("capture");
    assert_eq!(capture.layout(), AES67_RX_LAYOUT_412);
    // 4.1.2 strict：FL FR SL SR TFL TFR 空 LFE → 容器 capture 0/1/4/5/8/9/None/3。
    assert_eq!(
        capture.capture_index(),
        &[
            Some(0),
            Some(1),
            Some(4),
            Some(5),
            Some(8),
            Some(9),
            None,
            Some(3)
        ]
    );

    let origin = 2_000u64;
    // 容器通道值 = capture 通道 index + 1（小整数在 f32 中精确）。
    push_packets(&writer0, origin, 8, |_frame, channel| channel as i32 + 1, 4);
    push_packets(&writer1, origin, 4, |_frame, channel| 9 + channel as i32, 4);
    let stride = AES67_RX_BLOCK_FRAMES;
    let mut planar = vec![0.0f32; 8 * stride];
    let now = block_now(origin, capture.sources[0].latency_frames(), 0);
    let status = capture
        .fill_block(&mut planar, stride, AES67_RX_BLOCK_FRAMES, now)
        .expect("fill");
    assert_eq!(status, BlockStatus::Ok);
    let full_scale = 2_147_483_648.0f32;
    for (slot, expected) in [
        (0usize, 1.0f32),
        (1, 2.0),
        (2, 5.0),  // SL = 容器 4 → 值 5
        (3, 6.0),  // SR = 容器 5 → 值 6
        (4, 9.0),  // TFL = 容器 8 → 值 9
        (5, 10.0), // TFR = 容器 9 → 值 10
        (6, 0.0),  // strict 空槽显式 0
        (7, 4.0),  // LFE = 容器 3 → 值 4
    ] {
        assert_eq!(planar[slot * stride], expected / full_scale, "slot {slot}");
    }
}

#[test]
fn t235_aes67rx_any_flow_missing_yields_whole_block_silence() {
    let (writer0, reader0) = jitter::channel(jitter_config(8)).expect("flow0");
    let (_writer1, reader1) = jitter::channel(jitter_config(4)).expect("flow1");
    let adapter = Aes67RxAdapter::new(Aes67RxFlowPlan::Logical12, 8).expect("adapter logical12");
    let mut capture =
        Aes67RxCapture::from_readers(vec![reader0, reader1], adapter).expect("capture");
    let origin = 3_000u64;
    push_packets(&writer0, origin, 8, |_frame, _channel| 42, 4);
    // flow1 从未锚定 → 整块 Warmup 静音。
    let stride = AES67_RX_BLOCK_FRAMES;
    let mut planar = vec![7.0f32; 8 * stride];
    let now = block_now(origin, capture.sources[0].latency_frames(), 0);
    let status = capture
        .fill_block(&mut planar, stride, AES67_RX_BLOCK_FRAMES, now)
        .expect("fill");
    assert_eq!(status, BlockStatus::Warmup);
    assert!(planar.iter().all(|&sample| sample == 0.0));
    assert_eq!(capture.counters().warmup_blocks, 1);
    assert_eq!(capture.counters().ok_blocks, 0);
    let jitter_stats = capture.jitter_stats();
    assert_eq!(jitter_stats[0].packets, 4);
    assert_eq!(jitter_stats[1].packets, 0);
}

#[test]
fn t235_aes67rx_config_plan_and_block_contract_errors() {
    // 图宽度 3 → 布局错误。
    let error = Aes67RxAdapter::new(stereo_plan(), 3).unwrap_err();
    assert!(error.starts_with("ERR_AES67RX_LAYOUT"), "{error}");
    // 角色缺失（2ch 图 + 容器只有 1 个 LFE 角色）→ 角色错误。
    let lfe_only =
        Aes67RxFlowPlan::Custom(H16FlowConfig::try_from_slots(&[&[3u8]]).expect("lfe slots"));
    let error = Aes67RxAdapter::new(lfe_only, 2).unwrap_err();
    assert!(error.starts_with("ERR_AES67RX_ROLES"), "{error}");
    // flow 数不符 → 计划错误。
    let (writer, reader) = jitter::channel(jitter_config(2)).expect("channel");
    let adapter = Aes67RxAdapter::new(stereo_plan(), 2).expect("adapter");
    let error = Aes67RxCapture::from_readers(vec![reader.clone(), reader], adapter).unwrap_err();
    assert!(error.starts_with("ERR_AES67RX_PLAN"), "{error}");
    // 块长非 128 / 缓冲不足 → 契约错误。
    let (_writer, reader) = jitter::channel(jitter_config(2)).expect("channel2");
    let adapter = Aes67RxAdapter::new(stereo_plan(), 2).expect("adapter2");
    let mut capture = Aes67RxCapture::from_readers(vec![reader], adapter).expect("capture");
    let mut planar = vec![0.0f32; 2 * AES67_RX_BLOCK_FRAMES];
    let error = capture
        .fill_block(&mut planar, AES67_RX_BLOCK_FRAMES, 64, 1_000)
        .unwrap_err();
    assert!(error.starts_with("ERR_AES67RX_BLOCK"), "{error}");
    let error = capture
        .fill_block(
            &mut planar[..10],
            AES67_RX_BLOCK_FRAMES,
            AES67_RX_BLOCK_FRAMES,
            1_000,
        )
        .unwrap_err();
    assert!(error.starts_with("ERR_AES67RX_BLOCK"), "{error}");
    let _ = writer;
}

#[test]
fn t235_aes67rx_bind_two_unicast_flows_smoke() {
    use std::net::{Ipv4Addr, SocketAddr};
    use khaen_aoip_aes67::receiver::{ReceiverClock, ReceiverConfig};

    fn unicast(port: u16, channels: u16) -> ReceiverConfig {
        ReceiverConfig {
            media: Ipv4Addr::LOCALHOST,
            port,
            multicast: false,
            interfaces: khaen_aoip_aes67::listen::ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: khaen_aoip_aes67::listen::GroupAllow::EMPTY,
            source_allow: khaen_aoip_aes67::listen::SourceAllow::EMPTY,
            channels,
            rcvbuf_bytes: 1 << 20,
            recv_timeout_ms: 10,
            io_priority: None,
            clock: ReceiverClock::Usrv,
            jitter: jitter_config(channels),
            ssrc_filter: None,
        }
    }
    let adapter = Aes67RxAdapter::new(Aes67RxFlowPlan::Logical12, 8).expect("adapter logical12");
    let flows = [unicast(0, 8), unicast(0, 4)];
    let mut capture = Aes67RxCapture::bind(&flows, adapter).expect("bind");
    assert_eq!(capture.local_addrs().len(), 2);
    assert!(capture
        .local_addrs()
        .iter()
        .all(|address| !address.ip().is_unspecified()));
    assert_eq!(capture.flow_info().len(), 2);
    assert_eq!(capture.flow_info()[0].channels, 8);
    assert_eq!(capture.flow_info()[1].channels, 4);
    assert_eq!(capture.receiver_stats().len(), 2);
    let report = capture.report_json();
    assert!(report.contains("\"flow_count\":2"));
    assert!(report.contains("\"layout\":\"4.1.2-strict\""));
    capture.stop();
    // 未投递任何 RTP → Warmup 整块静音（不是 Ok）。
    let mut planar = vec![0.0f32; 8 * AES67_RX_BLOCK_FRAMES];
    let status = capture
        .fill_block(
            &mut planar,
            AES67_RX_BLOCK_FRAMES,
            AES67_RX_BLOCK_FRAMES,
            1_000,
        )
        .expect("fill");
    assert_eq!(status, BlockStatus::Warmup);
    let _ = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
}

#[test]
fn t235_aes67rx_container16_non_logical_slots_explicitly_ignored() {
    let adapter =
        Aes67RxAdapter::new(Aes67RxFlowPlan::Container16, 8).expect("adapter container16");
    assert_eq!(adapter.container_channels().unwrap(), 16);
    let readers = (0..2)
        .map(|_| jitter::channel(jitter_config(8)).expect("flow channel").1)
        .collect::<Vec<_>>();
    let capture = Aes67RxCapture::from_readers(readers, adapter).expect("capture");
    // 16ch 容器：capture 8/9/12/13 非逻辑（Lw/Rw/空/空）；4.1.2 选择 FL/FR/SL/SR/TFL/TFR/LFE。
    let index = capture.capture_index();
    assert_eq!(index.len(), 8);
    assert_eq!(index[0], Some(0)); // FL
    assert_eq!(index[1], Some(1)); // FR
    assert_eq!(index[2], Some(4)); // SL
    assert_eq!(index[3], Some(5)); // SR
    assert_eq!(index[4], Some(10)); // TFL（TX11）
    assert_eq!(index[5], Some(11)); // TFR（TX12）
    assert_eq!(index[6], None); // strict 空槽
    assert_eq!(index[7], Some(3)); // LFE
}

/// 连续推包（起始 seq/时间戳可指定；1ms/48 帧）——合成媒体时钟测试用。
fn push_stream_packets(
    writer: &jitter::JitterWriter,
    now_ns: u64,
    channels: u16,
    start_packet: u16,
    packets: usize,
    start_timestamp: u32,
) {
    for index in 0..packets {
        let sequence = start_packet.wrapping_add(index as u16);
        let timestamp = start_timestamp.wrapping_add(index as u32 * 48);
        let mut samples = vec![0i32; 48 * channels as usize];
        for frame in 0..48usize {
            for channel in 0..channels as usize {
                samples[frame * channels as usize + channel] = channel as i32 + 1;
            }
        }
        let result = writer.push_interleaved(
            now_ns,
            &packet_header(sequence, timestamp, channels),
            &samples,
        );
        assert!(
            matches!(
                result,
                jitter::PushResult::Accepted { .. } | jitter::PushResult::Restarted { .. }
            ),
            "{result:?}"
        );
    }
}

#[test]
fn t235_aes67rx_paced_clock_no_notdue_under_irregular_calls() {
    // T239-crackle 回归：播放路径用合成媒体时钟（origin+latency+delivered），
    // 调用间隔不规则（ALSA duplex 2ms/3ms 交替、甚至背靠背）时 not_due 必须恒为 0。
    let (writer, reader) = jitter::channel(jitter_config(2)).expect("channel");
    let adapter = Aes67RxAdapter::new(stereo_plan(), 2).expect("adapter");
    let mut capture = Aes67RxCapture::from_readers(vec![reader], adapter).expect("capture");
    let origin = 20_000u64;
    let stride = AES67_RX_BLOCK_FRAMES;
    let mut planar = vec![0.0f32; 2 * stride];
    // 4 包 = 192 帧 < 4ms 目标延迟（256 帧）→ 预滚静音，不消费媒体帧。
    push_stream_packets(&writer, origin, 2, 0, 4, 0);
    let status = capture
        .fill_block_paced(&mut planar, stride, stride)
        .expect("paced");
    assert_eq!(status, BlockStatus::Warmup);
    assert_eq!(capture.counters().preroll_blocks, 1);
    assert_eq!(capture.counters().frames_delivered, 0);
    // 补推到 384 帧 ≥ 预滚门限 → 预滚完成，开始 Ok。
    push_stream_packets(&writer, origin, 2, 4, 4, 192);
    assert_eq!(
        capture
            .fill_block_paced(&mut planar, stride, stride)
            .expect("paced"),
        BlockStatus::Ok
    );
    // 再推 80 包（30 块）数据；不规则/背靠背调用全部必须 Ok。
    push_stream_packets(&writer, origin, 2, 8, 80, 384);
    for index in 0..30 {
        assert_eq!(
            capture
                .fill_block_paced(&mut planar, stride, stride)
                .expect("paced"),
            BlockStatus::Ok,
            "第 {index} 块：合成时钟下不得 NotDue"
        );
    }
    // 背靠背两次（同一墙钟窗口；旧"墙钟 target"实现第二块会 NotDue 静音）。
    push_stream_packets(&writer, origin, 2, 88, 4, 384 + 80 * 48);
    for _ in 0..2 {
        assert_eq!(
            capture
                .fill_block_paced(&mut planar, stride, stride)
                .expect("paced"),
            BlockStatus::Ok
        );
    }
    let counters = capture.counters();
    assert_eq!(counters.not_due_blocks, 0, "合成时钟下 not_due 必须为 0");
    assert_eq!(counters.underrun_blocks, 0);
    assert_eq!(counters.ok_blocks, 33);
    assert_eq!(counters.frames_delivered, 33 * stride as u64);
    assert_eq!(counters.preroll_blocks, 1);
    assert!(counters.paced_blocks >= 34);
    let report = capture.report_json();
    assert!(report.contains("\"paced_blocks\":"), "{report}");
    assert!(report.contains("\"clock_resets\":0"), "{report}");
}

#[test]
fn t235_aes67rx_paced_clock_resets_on_internal_reanchor() {
    // 内部重锚（新会话 ts 复位）：origin/epoch 变化 → 合成时钟重置、重新预滚，
    // 且不得产生 NotDue（旧实现会 NotDue 静音 + resync 丢块）。
    let (writer, reader) = jitter::channel(jitter_config(2)).expect("channel");
    let adapter = Aes67RxAdapter::new(stereo_plan(), 2).expect("adapter");
    let mut capture = Aes67RxCapture::from_readers(vec![reader], adapter).expect("capture");
    let stride = AES67_RX_BLOCK_FRAMES;
    let mut planar = vec![0.0f32; 2 * stride];
    push_stream_packets(&writer, 30_000, 2, 0, 16, 0);
    assert_eq!(
        capture
            .fill_block_paced(&mut planar, stride, stride)
            .expect("paced"),
        BlockStatus::Ok
    );
    assert_eq!(capture.counters().frames_delivered, stride as u64);
    assert_eq!(capture.counters().clock_resets, 0);
    // 请求内部重锚 + 新会话首包（时间戳复位、SSRC 不变）：writer 在下一包重建锚点。
    writer.request_reanchor();
    push_stream_packets(&writer, 50_000, 2, 0, 8, 0);
    assert_eq!(
        capture
            .fill_block_paced(&mut planar, stride, stride)
            .expect("paced"),
        BlockStatus::Ok
    );
    let counters = capture.counters();
    assert_eq!(counters.clock_resets, 1, "origin/epoch 变化必须重置合成时钟");
    assert_eq!(counters.not_due_blocks, 0, "重锚后也不得 NotDue 静音");
    assert_eq!(counters.preroll_blocks, 0, "新锚点 384 帧 ≥ 预滚门限，同块即完成");
}

#[test]
fn t235_aes67rx_container16_dual_flow_maps_16_graph_channels() {
    // NSP 16ch Phase 1：Container16 = 2 条 8+8 flow；图输入 16ch = H16 容器 capture 顺序。
    // 12 个逻辑角色映射到图通道；Lw/Rw/Aux12/Aux13（容器 slot 8/9/12/13）显式零填。
    let (writer0, reader0) = jitter::channel(jitter_config(8)).expect("flow0");
    let (writer1, reader1) = jitter::channel(jitter_config(8)).expect("flow1");
    let adapter = Aes67RxAdapter::new(Aes67RxFlowPlan::Container16, 16).expect("adapter 16ch");
    assert_eq!(adapter.flow_count(), 2);
    assert_eq!(adapter.flow_channels(0), Some(8));
    assert_eq!(adapter.flow_channels(1), Some(8));
    assert_eq!(adapter.container_channels().unwrap(), 16);
    let mut capture =
        Aes67RxCapture::from_readers(vec![reader0, reader1], adapter).expect("capture");
    assert_eq!(capture.layout(), AES67_RX_LAYOUT_16);
    // 16ch full-container layout: graph channel k == container capture k (identity),
    // so the four non-logical container channels are delivered as aux graph inputs
    // (consumed by aux plan roles), never silently dropped.
    assert_eq!(
        capture.capture_index(),
        &[
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(7),
            Some(8),
            Some(9),
            Some(10),
            Some(11),
            Some(12),
            Some(13),
            Some(14),
            Some(15),
        ]
    );

    let origin = 4_000u64;
    // 容器通道值 = capture 通道 index + 1（小整数在 f32 中精确）；flow1 从 9 起。
    push_packets(&writer0, origin, 8, |_frame, channel| channel as i32 + 1, 4);
    push_packets(&writer1, origin, 8, |_frame, channel| 9 + channel as i32, 4);
    let stride = AES67_RX_BLOCK_FRAMES;
    let mut planar = vec![-1.0f32; 16 * stride];
    let now = block_now(origin, capture.sources[0].latency_frames(), 0);
    let status = capture
        .fill_block(&mut planar, stride, AES67_RX_BLOCK_FRAMES, now)
        .expect("fill");
    assert_eq!(status, BlockStatus::Ok);
    let full_scale = 2_147_483_648.0f32;
    for slot in 0..16usize {
        // Container channel value = capture index + 1 for both flows (flow1 starts at 9).
        let expected = (slot + 1) as f32 / full_scale;
        assert_eq!(planar[slot * stride], expected, "graph slot {slot}");
    }
    // 电平表（NONRT 证据）：16 个图通道都有独立 RMS；4 个非逻辑通道恒 0。
    let report = capture.report_json();
    assert!(report.contains("\"graph_channels\":16"), "{report}");
    assert!(report.contains("\"container_channels\":16"), "{report}");
    assert!(report.contains("\"channel_rms\":["), "{report}");
    assert!(report.contains("\"channel_peak\":["), "{report}");
    assert!(report.contains("\"meter_frames\":128"), "{report}");
    let counters = capture.counters();
    assert_eq!(counters.ok_blocks, 1);
    assert_eq!(counters.frames_delivered, AES67_RX_BLOCK_FRAMES as u64);
}
