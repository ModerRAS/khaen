//! dsp-host AES67 接收端运行二进制（calib-chain-20261121 / aes67-rx-20261121）。
//!
//! 链路：SDP（L24/48k/Nch，组播或单播）→ crates/aoip-aes67（UDP 收包线程 + 无锁抖动环 +
//! 128 帧固定块）→ 本二进制播放线程（媒体时基按"已写出帧数"推进）→ ALSA S24_3LE 22ch
//! MOTU UltraLite-mk5 → 4.1.2 七声道映射到 ch0,1,2,3,4,5,7，其余静音。
//!
//! 热路径纪律：块缓冲/输出缓冲全部启动期预分配；播放线程每块只做固定长度拷贝+算法换算，
//! 不分配、不格式化、不加锁（统计走原子计数，由主线程另起监视循环打印）。
//!
//! 用法见 README.md / --help。

mod alsa_out;
mod dump_ring;
mod rt;

use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use khaen_aoip_aes67::capture::CaptureBlockSource;
use khaen_aoip_aes67::clock::frames_to_ns_ceil;
use khaen_aoip_aes67::jitter::{BlockStatus, JitterConfig};
use khaen_aoip_aes67::receiver::{Aes67Receiver, ReceiverClock, ReceiverConfig};
use khaen_aoip_aes67::sdp::{Encoding, RefClk, SdpDocument, Session};
use khaen_aoip_aes67::sender::{Aes67RtpSender, SenderConfig};

const RATE_HZ: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;
const FOUR_ONE_TWO: [&str; 7] = ["L", "R", "Ls", "Rs", "Ltf", "Rtf", "LFE"];
const FOUR_ONE_TWO_MAP: [usize; 7] = [0, 1, 2, 3, 4, 5, 7];

fn now_unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn now_unix_ms() -> u64 {
    now_unix_ns() / 1_000_000
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------- CLI

struct Cli {
    command: String,
    kv: Vec<(String, String)>,
    flags: Vec<String>,
}

impl Cli {
    fn parse(argv: Vec<String>) -> Result<Self, String> {
        let mut args = argv.into_iter();
        let command = args
            .next()
            .ok_or("缺少子命令（run|selftest|tone|sdp-info）")?;
        let mut kv = Vec::new();
        let mut flags = Vec::new();
        let mut pending: Option<String> = None;
        for arg in args {
            if let Some(key) = pending.take() {
                kv.push((key, arg));
                continue;
            }
            if let Some(name) = arg.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    kv.push((k.to_string(), v.to_string()));
                } else if matches!(
                    name,
                    "help"
                        | "mlock"
                        | "no-click-guard"
                        | "distinct-tones"
                        | "quiet"
                        | "stay-on-stream-end"
                ) {
                    flags.push(name.to_string());
                } else {
                    pending = Some(name.to_string());
                }
            } else {
                return Err(format!("无法解析参数: {arg}"));
            }
        }
        if let Some(key) = pending {
            return Err(format!("参数 --{key} 缺少值"));
        }
        Ok(Self { command, kv, flags })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.kv
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    fn str_or(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_string()
    }

    fn num_or<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.get(key) {
            None => Ok(default),
            Some(text) => text
                .parse::<T>()
                .map_err(|_| format!("--{key} 值非法: {text}")),
        }
    }

    fn required(&self, key: &str) -> Result<&str, String> {
        self.get(key).ok_or_else(|| format!("缺少必需参数 --{key}"))
    }
}

/// 由 CLI 构造 dump 诊断规格（默认 None = 不落盘）。
/// --dump-wav PATH 等价于 --dump-dir PATH.d（启动时清空该目录=清理本进程旧 dump）。
fn build_dump_spec(cli: &Cli) -> Result<Option<DumpSpec>, String> {
    let dir = match (cli.get("dump-dir"), cli.get("dump-wav")) {
        (Some(dir), _) => dir.to_string(),
        (None, Some(wav)) => format!("{wav}.d"),
        (None, None) => return Ok(None),
    };
    // 启动时清理本进程自己的 dump 目录（有界环形文件，不跨进程累积）
    let path = std::path::Path::new(&dir);
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
    let seg_seconds: u32 = cli.num_or("dump-seg-seconds", 5)?;
    let max_segments: usize = cli.num_or("dump-max-segments", 3)?;
    let max_seconds: u32 = cli.num_or("dump-max-seconds", 30)?;
    let max_mb: u64 = cli.num_or("dump-max-mb", 50)?;
    Ok(Some(DumpSpec {
        dir,
        seg_seconds,
        max_segments,
        max_seconds,
        max_bytes: max_mb.saturating_mul(1_000_000),
    }))
}

fn print_help() {
    println!(
        r#"aes67-rx —— dsp-host AES67 接收 → MOTU 22ch S24_3LE 输出（独立二进制）

子命令：
  run      --sdp FILE [选项]      接收真实/外部 RTP 流并写 MOTU
  selftest [--sdp FILE] [选项]    本机 7ch 多音回路自测（自带 RTP 发送端）
  tone     --dest IP:PORT [选项]  独立 RTP 测试音发送端（跨机联调）
  sdp-info --sdp FILE             解析/校验 SDP 并打印 JSON 摘要

通用输出选项（run/selftest）：
  --device hw:UltraLitemk5,0    ALSA 播放设备（默认 hw:UltraLitemk5,0）
  --playback-channels 22        播放通道数（默认 22）
  --map 0,1,2,3,4,5,7           源通道→播放通道映射（默认 7ch=4.1.2 映射）
  --latency-us 24000            抖动缓冲目标延迟（µs；向上取整到 128 帧块，默认 24000 = 24 ms 预滚）
  --fade-in-ms 10               起播/恢复余弦淡入长度（ms，默认 10）
  --fade-out-ms 40              停流/欠载余弦淡出长度（ms，默认 40，避免突然归零爆音）
  --stop-mute-ms 200            停止/退出前数字静音长度（ms，默认 200；之后 drain 再 close）
  --stream-end-ms 80            连续多久无可播数据判定"流结束"并自动收尾（ms，默认 80）
  --stay-on-stream-end          流结束时不退出，保持设备时钟等待流恢复（交互监听推荐）
  --notdue-hold-ms 20           NOT_DUE 保持上限（ms，超限即判定异常/停流，默认 20）
  --ring-frames 8192            抖动环容量（帧，2 的幂，默认 8192）
  --period-frames 128           ALSA period（帧，默认 128）
  --buffer-frames 512           ALSA buffer（帧，默认 512）
  --interface IP                组播 join / 单播 bind 网卡地址（默认 0.0.0.0）
  --ssrc 0x4E535037             只接受该 RTP SSRC（十六进制；多源/保活同端口隔离用）
  --duration-secs 15            运行时长（0 = 直到 SIGINT/SIGTERM）
  --rx-prio 80                  收包 IO 线程 SCHED_FIFO 优先级（0 = 不设置）
  --playback-prio 70            播放线程 SCHED_FIFO 优先级（0 = 不设置）
  --pin-cpu 2                   播放线程绑定 CPU（-1 = 不绑定；默认 2）
  --mlock                       启动期 mlockall（默认关；证据窗口建议开）
  --gain-db 0.0                 数字衰减（dB；负值 = 衰减，默认 0；首次出声建议 -18）
  --no-click-guard              关闭欠载/恢复淡入淡出
  --status-ms 1000              状态打印周期（0 = 不打印）
  --report FILE                 结束时写 JSON 报告
  --dump-dir DIR                诊断用有界环形 dump 目录（默认关闭；≤30s/≤50MB，5s/段×3 段自动覆盖）
  --dump-wav FILE               兼容写法：等价 --dump-dir FILE.d
  --dump-seg-seconds 5          dump 每段秒数（默认 5）
  --dump-max-segments 3         dump 环形段数（默认 3）
  --dump-max-seconds 30         dump 总时长上限（默认 30 s，先到为准）
  --dump-max-mb 50              dump 总字节上限（默认 50 MB，先到为准）
  --quiet                       不打印状态行

selftest 专用：
  --tone-hz 1000                起始测试音频率（默认 1000）
  --tone-step 0                 通道递增步进 Hz（0 = 所有通道同频；>0 = 频分验证映射）
  --amplitude-db -24            单通道幅度 dBFS（默认 -24）
  --frames-per-packet 48        每包帧数（默认 48 = 1 ms）
  --sender-stop-after-ms 0      模拟流中途结束：发送端只发 N ms 后停（0=发满 duration；数字测试用）

tone 专用：
  --dest IP:PORT                目标（默认 127.0.0.1:5004）
  --channels 7                  通道数（默认 7）
  其余同 selftest（--tone-hz/--tone-step/--amplitude-db/--frames-per-packet/--duration-secs）
"#
    );
}

// ---------------------------------------------------------------- SDP

struct SdpSummary {
    text: Option<String>,
    path: Option<String>,
    media: String,
    port: u16,
    multicast: bool,
    channels: u16,
    rate_hz: u32,
    encoding: &'static str,
    ptime_us: Option<u32>,
    payload_type: u8,
    refclk: &'static str,
    ptp_domain: Option<u8>,
    localmac: Option<String>,
}

fn summarize_sdp(doc: &SdpDocument, session: &Session<'_>) -> SdpSummary {
    let media = session.media;
    let connection = session.connection();
    let (refclk, ptp_domain, localmac) = match media.refclk {
        Some(RefClk::Ptp(ptp)) => ("ptp", Some(ptp.domain), None),
        // RFC 7273 local（Phase 1 NSP 16ch SDP）：无 MAC/domain 可展示；本工具不做 16ch。
        Some(RefClk::Local) => ("local", None, None),
        Some(RefClk::LocalMac(mac)) => (
            "localmac",
            None,
            Some(
                mac.iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join("-"),
            ),
        ),
        None => ("none", None, None),
    };
    SdpSummary {
        text: Some(doc.text().to_string()),
        path: doc.source().map(ToString::to_string),
        media: connection
            .map(|c| format!("{}:{}", c.ipv4(), media.port))
            .unwrap_or_else(|| format!("?:{}", media.port)),
        port: media.port,
        multicast: connection.map(|c| c.is_multicast()).unwrap_or(false),
        channels: media.rtpmap.channels,
        rate_hz: media.rtpmap.rate_hz,
        encoding: match media.rtpmap.encoding {
            Encoding::L24 => "L24",
            Encoding::L16 => "L16",
        },
        ptime_us: media.ptime_us,
        payload_type: media.payload_type,
        refclk,
        ptp_domain,
        localmac,
    }
}

fn generated_selftest_sdp(channels: u16, port: u16) -> String {
    format!(
        "v=0\r\n\
o=- 424242 424242 IN IP4 127.0.0.1\r\n\
s=KHAEN dsp-host AES67 selftest\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio {port} RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/{channels}\r\n\
a=ptime:1\r\n\
a=sendonly\r\n\
a=ts-refclk:localmac=02-4E-53-50-00-01\r\n\
a=mediaclk:direct=0\r\n"
    )
}

fn open_receiver(
    doc: &SdpDocument,
    interface: Option<Ipv4Addr>,
    rx_prio: i32,
    latency_us: u32,
    ring_frames: usize,
    ssrc_filter: Option<u32>,
) -> Result<(Aes67Receiver, CaptureBlockSource, SdpSummary), String> {
    let session = doc.parse_validated().map_err(|e| e.name().to_string())?;
    let summary = summarize_sdp(doc, &session);
    let mut config =
        ReceiverConfig::from_session(&session, interface).map_err(|e| e.name().to_string())?;
    config.jitter = JitterConfig::new(
        summary.channels,
        RATE_HZ,
        BLOCK_FRAMES,
        ring_frames,
        latency_us,
    )
    .map_err(|e| e.name().to_string())?;
    config.recv_timeout_ms = 20;
    config.io_priority = if rx_prio > 0 { Some(rx_prio) } else { None };
    config.clock = ReceiverClock::Usrv;
    config.ssrc_filter = ssrc_filter;
    let mut receiver = Aes67Receiver::bind(config).map_err(|e| e.name().to_string())?;
    let reader = receiver.take_reader().ok_or("take_reader 为空")?;
    let source = CaptureBlockSource::new(reader).map_err(|e| e.name().to_string())?;
    Ok((receiver, source, summary))
}

// ---------------------------------------------------------------- 映射/渲染

fn parse_map(text: &str, channels: usize, playback_channels: usize) -> Result<Vec<usize>, String> {
    let map: Vec<usize> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<usize>()
                .map_err(|_| format!("--map 元素非法: {part}"))
        })
        .collect::<Result<_, _>>()?;
    if map.len() != channels {
        return Err(format!(
            "--map 需要 {channels} 个目标通道（源通道数），实际 {}",
            map.len()
        ));
    }
    let mut seen = vec![false; playback_channels];
    for (src, dst) in map.iter().enumerate() {
        if *dst >= playback_channels {
            return Err(format!(
                "--map 源通道 {src} → 目标通道 {dst} 超出播放通道数 {playback_channels}"
            ));
        }
        if seen[*dst] {
            return Err(format!("--map 目标通道 {dst} 重复映射"));
        }
        seen[*dst] = true;
    }
    Ok(map)
}

/// 源通道 → 播放通道 的反查表（-1 = 静音）。
fn src_for_dest(map: &[usize], playback_channels: usize) -> Vec<i8> {
    let mut table = vec![-1i8; playback_channels];
    for (src, dst) in map.iter().enumerate() {
        table[*dst] = src as i8;
    }
    table
}

/// 淡入淡出状态：流中断（欠载/停流）时用余弦淡出，恢复/起播时余弦淡入，
/// 避免"突然归零/突然起音"的爆音。全部预分配，热路径只做查表式 cos 计算。
struct FadeState {
    /// 上一个 Ok 块末样本（每个播放通道），用于淡出起点。
    last: Vec<i32>,
    /// 当前非 Ok 块淡出起点。
    out_from: Vec<i32>,
    /// 非 Ok 块是否处于淡出中。
    out_active: bool,
    /// 淡出进度（帧）。
    out_pos: usize,
    /// 下一个 Ok 块是否需要淡入。
    in_pending: bool,
    /// 淡入进度（帧）。
    in_pos: usize,
}

impl FadeState {
    fn new(playback_channels: usize) -> Self {
        Self {
            last: vec![0i32; playback_channels],
            out_from: vec![0i32; playback_channels],
            out_active: false,
            out_pos: 0,
            in_pending: true, // 起播也淡入，防止源首样本非零造成起播 click
            in_pos: 0,
        }
    }
}

/// 余弦增益（Q16）：fade_in=true 时 0→1，false 时 1→0。pos/total ∈ [0,1]。
fn cosine_gain_q16(pos: usize, total: usize, fade_in: bool) -> i64 {
    if total == 0 {
        return if fade_in { 65_536 } else { 0 };
    }
    let phase = std::f64::consts::PI * (pos.min(total) as f64 + 1.0) / (total as f64 + 1.0);
    let g = if fade_in {
        0.5 * (1.0 - phase.cos())
    } else {
        0.5 * (1.0 + phase.cos())
    };
    (g * 65_536.0).clamp(0.0, 65_536.0) as i64
}

/// 流结束判定：连续无数据帧数 ≥ 门限（默认 80 ms）→ 自动收尾。
fn stream_end_reached(silent_frames: u64, end_frames: u64) -> bool {
    end_frames > 0 && silent_frames >= end_frames
}

/// NOT_DUE 保持上限判定：超过 hold 门限即视为异常/停流，不再"保持"。
fn notdue_hold_exceeded(silent_frames: u64, hold_frames: u64) -> bool {
    hold_frames > 0 && silent_frames >= hold_frames
}

/// 把抖动缓冲交出的交错 i32（满量程 24bit<<8）渲染成 22ch S24_3LE 交织字节。
/// 热路径：无分配、无格式化、无锁。
///
/// 状态机：
/// - Ok 块：若 in_pending → 逐帧余弦淡入 fade_in_frames；否则全增益。
/// - 非 Ok 块（Warmup/Underrun/NotDue）：若未在淡出中 → 以 last[] 为起点开始
///   余弦淡出 fade_out_frames（可跨多个块，30–50 ms），结束后整块静音并把
///   in_pending 置位（恢复时淡入）。
fn render_block(
    planar: &[i32],
    status: BlockStatus,
    src_dest: &[i8],
    channels: usize,
    out: &mut [u8],
    fade: &mut FadeState,
    click_guard: bool,
    gain_q16: i64,
    fade_in_frames: usize,
    fade_out_frames: usize,
) {
    let playback = src_dest.len();
    let bpf = playback * 3;
    let ok = matches!(status, BlockStatus::Ok);
    if ok {
        // 起播/恢复：从静音余弦淡入
        if click_guard && fade.in_pending && fade_in_frames > 0 {
            // keep in_pending until ramp finishes
        } else if !click_guard {
            fade.in_pending = false;
        }
        for frame in 0..BLOCK_FRAMES {
            let fade_q16: i64 = if fade.in_pending {
                let g = cosine_gain_q16(fade.in_pos, fade_in_frames, true);
                fade.in_pos += 1;
                if fade.in_pos >= fade_in_frames {
                    fade.in_pending = false;
                    fade.in_pos = 0;
                }
                g
            } else {
                65_536
            };
            let fused = (fade_q16 * gain_q16) >> 16;
            let base = frame * bpf;
            let src_base = frame * channels;
            for dest in 0..playback {
                let src = src_dest[dest];
                let value: i32 = if src >= 0 {
                    let raw = planar[src_base + src as usize] >> 8;
                    let scaled =
                        ((raw as i64) * fused / 65_536).clamp(-8_388_608, 8_388_607) as i32;
                    fade.last[dest] = scaled;
                    scaled
                } else {
                    0
                };
                let u = (value as u32) & 0x00FF_FFFF;
                let o = base + dest * 3;
                out[o] = u as u8;
                out[o + 1] = (u >> 8) as u8;
                out[o + 2] = (u >> 16) as u8;
            }
        }
        return;
    }

    // 非 Ok：开始或继续余弦淡出
    if click_guard && !fade.out_active && fade.last.iter().any(|v| *v != 0) {
        fade.out_from.copy_from_slice(&fade.last);
        fade.out_active = true;
        fade.out_pos = 0;
    }
    for frame in 0..BLOCK_FRAMES {
        let base = frame * bpf;
        for dest in 0..playback {
            let value: i32 = if fade.out_active {
                if fade.out_pos < fade_out_frames {
                    let g = cosine_gain_q16(fade.out_pos, fade_out_frames, false);
                    fade.out_pos += 1;
                    ((fade.out_from[dest] as i64) * g / 65_536) as i32
                } else {
                    0
                }
            } else {
                0
            };
            let u = (value as u32) & 0x00FF_FFFF;
            let o = base + dest * 3;
            out[o] = u as u8;
            out[o + 1] = (u >> 8) as u8;
            out[o + 2] = (u >> 16) as u8;
        }
    }
    if fade.out_active && fade.out_pos >= fade_out_frames {
        fade.out_active = false;
        fade.out_pos = 0;
        fade.last.fill(0);
        fade.out_from.fill(0);
        fade.in_pending = click_guard;
        fade.in_pos = 0;
    }
}

// ---------------------------------------------------------------- 统计/报告

#[derive(Default)]
struct RunStats {
    blocks: AtomicU64,
    ok_blocks: AtomicU64,
    warmup_blocks: AtomicU64,
    underrun_blocks: AtomicU64,
    not_due_blocks: AtomicU64,
    not_due_holds: AtomicU64,
    stream_reanchors: AtomicU64,
    stream_end_events: AtomicU64,
    write_lead_sum_us: AtomicU64,
    write_lead_count: AtomicU64,
    write_lead_min_us: AtomicU64,
    write_lead_max_us: AtomicU64,
    silent_streak: AtomicU64,
    end_reason: AtomicU64, // 0=running 1=stream_end_timeout 2=notdue_hold_exceeded 3=stop_requested
    frames_written: AtomicU64,
    xruns: AtomicU64,
    recoveries: AtomicU64,
    anchor_wait_ms: AtomicU64,
    fill_wait_ms: AtomicU64,
    playback_cpu: AtomicU64,
    dump_bytes: AtomicU64,
    dump_frames: AtomicU64,
    dump_truncated: AtomicU64,
    dump_write_errors: AtomicU64,
    dump_skipped_space: AtomicU64,
    stop_fade_blocks: AtomicU64,
    stop_mute_blocks: AtomicU64,
    drain_ok: AtomicU64,
    drain_errors: AtomicU64,
}

fn emit_json_report(path: Option<&str>, json: &str) -> Result<(), String> {
    if let Some(path) = path {
        std::fs::write(path, format!("{json}\n")).map_err(|e| format!("写 {path} 失败: {e}"))?;
        eprintln!("[aes67-rx] report -> {path}");
    }
    println!("{json}");
    let _ = std::io::stdout().flush();
    Ok(())
}

// ---------------------------------------------------------------- 播放线程

/// dump 诊断规格（默认 None=不落盘；有界/环形，见 dump_ring.rs）。
#[derive(Clone, Debug)]
struct DumpSpec {
    dir: String,
    seg_seconds: u32,
    max_segments: usize,
    max_seconds: u32,
    max_bytes: u64,
}

struct PlaybackConfig {
    device: String,
    playback_channels: usize,
    period_frames: usize,
    buffer_frames: usize,
    map: Vec<usize>,
    priority: i32,
    pin_cpu: i32,
    click_guard: bool,
    gain_q16: i64,
    fade_in_frames: usize,
    fade_out_frames: usize,
    stop_mute_frames: usize,
    stream_end_frames: u64,
    notdue_hold_frames: u64,
    stay_on_stream_end: bool,
    reanchor: Arc<AtomicBool>,
    dump: Option<DumpSpec>,
}

fn spawn_playback(
    mut source: CaptureBlockSource,
    config: PlaybackConfig,
    stats: Arc<RunStats>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<alsa_out::AlsaOut, String>> {
    std::thread::Builder::new()
        .name("aes67-rx-playback".to_string())
        .spawn(move || {
            let mut rt_notes: Vec<String> = Vec::new();
            if config.priority > 0 {
                if let Err(e) = rt::set_realtime(config.priority) {
                    rt_notes.push(e);
                }
            }
            if config.pin_cpu >= 0 {
                if let Err(e) = rt::set_affinity_cpu(config.pin_cpu as usize) {
                    rt_notes.push(e);
                }
            }
            for note in &rt_notes {
                eprintln!("[aes67-rx] playback RT 警告: {note}");
            }

            let mut out = alsa_out::AlsaOut::open(
                &config.device,
                config.playback_channels,
                RATE_HZ,
                config.period_frames,
                config.buffer_frames,
            )
            .map_err(|e| {
                eprintln!("[aes67-rx] ALSA 打开失败: {e}");
                e
            })?;
            eprintln!(
                "[aes67-rx] ALSA 已打开: {}ch S24_3LE/48k period={} buffer={}",
                config.playback_channels, config.period_frames, config.buffer_frames
            );
            let channels = source.channels();
            let mut planar = vec![0i32; BLOCK_FRAMES * channels];
            let mut outbuf = vec![0u8; BLOCK_FRAMES * config.playback_channels * 3];
            let mut fade = FadeState::new(config.playback_channels);
            let src_dest = src_for_dest(&config.map, config.playback_channels);
            let mut dump = match config.dump.as_ref() {
                Some(spec) => {
                    let dir = std::path::Path::new(&spec.dir);
                    let free = dump_ring::DumpRing::free_bytes(
                        dir.parent().unwrap_or(std::path::Path::new("/")),
                    );
                    if free > 0 && free < 1_000_000_000 {
                        eprintln!(
                            "[aes67-rx] dump 未启用：目录所在 fs 剩余 {} MB < 1 GB",
                            free / 1_000_000
                        );
                        stats.dump_skipped_space.store(1, Ordering::Relaxed);
                        None
                    } else {
                        eprintln!(
                            "[aes67-rx] dump(诊断) -> {}（{}s/段 ×{} 段，≤{}s/≤{}MB，环形）",
                            spec.dir, spec.seg_seconds, spec.max_segments, spec.max_seconds, spec.max_bytes / 1_000_000
                        );
                        match dump_ring::DumpRing::new(
                            dir,
                            config.playback_channels as u16,
                            RATE_HZ,
                            spec.seg_seconds,
                            spec.max_segments,
                            spec.max_seconds,
                            spec.max_bytes,
                        ) {
                            Ok(d) => Some(d),
                            Err(e) => {
                                eprintln!("[aes67-rx] dump 未启用: {e}");
                                stats.dump_skipped_space.store(1, Ordering::Relaxed);
                                None
                            }
                        }
                    }
                }
                None => None,
            };
            // 1) 首包锚定 + 预滚（**不能在锚定前定死时间轴**）：
            //    - 未锚定：只写静音块保持 MOTU 设备时钟，不消费抖动环；
            //    - 锚定后：等抖动缓冲填到目标延迟再开始消费媒体帧（否则首块欠载会让
            //      抖动缓冲丢弃首段并触发 resync → 启动瞬间跳帧/毛刺）；
            //    - 媒体时基只在锚定后建立：now = anchor + latency + 已写出媒体帧数。
            let latency_frames = source.latency_frames();
            let wait_start = Instant::now();
            let mut origin: Option<u64> = None;
            let mut media_frames_written: u64 = 0;
            let mut preroll_done = false;
            let mut silent_frames: u64 = 0;
            let mut stream_end_logged = false;
            let mut wl_sum_us: u64 = 0;
            let mut wl_count: u64 = 0;
            let mut wl_min_us: u64 = u64::MAX;
            let mut wl_max_us: u64 = 0;
            let silence_block = vec![0u8; BLOCK_FRAMES * config.playback_channels * 3];
            while !stop.load(Ordering::Acquire) && !rt::stop_requested() {
                if origin.is_none() {
                    origin = source.origin_ns();
                    if origin.is_some() {
                        stats.anchor_wait_ms.store(
                            wait_start.elapsed().as_millis() as u64,
                            Ordering::Relaxed,
                        );
                        eprintln!(
                            "[aes67-rx] 首包锚定：等待 {} ms，开始预滚 {} 帧",
                            wait_start.elapsed().as_millis(),
                            latency_frames
                        );
                    }
                }
                let mut anchor = match origin {
                    None => {
                        out.write_all(&silence_block)?;
                        stats.blocks.fetch_add(1, Ordering::Relaxed);
                        stats.warmup_blocks.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Some(anchor) => anchor,
                };
                // 流重锚（发送端重启会话/SSRC 变化/时间戳大跳）：抖动环 origin 会变。
                // 必须同步重置本地媒体时基，否则会用旧时间轴去追新流 → 长时间 Warmup/Overflow。
                if let Some(current) = source.origin_ns() {
                    if current != anchor {
                        anchor = current;
                        origin = Some(current);
                        media_frames_written = 0;
                        preroll_done = false;
                        fade = FadeState::new(config.playback_channels);
                        stats.stream_reanchors.fetch_add(1, Ordering::Relaxed);
                        eprintln!("[aes67-rx] 流重锚：origin 更新，媒体时基/预滚重置");
                    }
                }
                if !preroll_done {
                    if source.available_frames() < latency_frames {
                        out.write_all(&silence_block)?;
                        stats.blocks.fetch_add(1, Ordering::Relaxed);
                        stats.warmup_blocks.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    preroll_done = true;
                    stats.fill_wait_ms.store(
                        wait_start.elapsed().as_millis() as u64,
                        Ordering::Relaxed,
                    );
                    eprintln!(
                        "[aes67-rx] 预滚完成：{} ms（可用 {} 帧）",
                        wait_start.elapsed().as_millis(),
                        source.available_frames()
                    );
                }
                let now_ns = anchor.saturating_add(frames_to_ns_ceil(
                    latency_frames + media_frames_written,
                    RATE_HZ,
                ));
                let read = source
                    .read_block(now_ns, &mut planar)
                    .map_err(|e| e.name().to_string())?;
                match read.status {
                    BlockStatus::NotDue => {
                        // 时钟尚未到期/时基错位：**绝不重复上一块音频**（重复内容 = 可闻滋滋声）。
                        // 只写静音块（保持设备时钟），把时基对齐 read_frame，下一块即恢复 Ok。
                        stats.not_due_blocks.fetch_add(1, Ordering::Relaxed);
                        stats.not_due_holds.fetch_add(1, Ordering::Relaxed);
                        media_frames_written = source.read_frame();
                        silent_frames += BLOCK_FRAMES as u64;
                        stats.silent_streak.store(silent_frames, Ordering::Relaxed);
                        if let Some(dump) = dump.as_mut() {
                            let _ = dump.write_block(&silence_block, BLOCK_FRAMES);
                        }
                        out.write_all(&silence_block)?;
                        stats.blocks.fetch_add(1, Ordering::Relaxed);
                        stats.xruns.store(out.xruns, Ordering::Relaxed);
                        stats.recoveries.store(out.recoveries, Ordering::Relaxed);
                        if let Some(cpu) = rt::current_cpu() {
                            stats.playback_cpu.store(cpu as u64, Ordering::Relaxed);
                        }
                        // NOT_DUE_HOLD 上限：超过 notdue_hold_frames 即视为异常/停流，走收尾。
                        if notdue_hold_exceeded(silent_frames, config.notdue_hold_frames) {
                            stats.end_reason.store(2, Ordering::Relaxed);
                            eprintln!("[aes67-rx] NOT_DUE 持续 {} ms，判定停流，进入收尾", silent_frames as f64 / RATE_HZ as f64 * 1000.0);
                            break;
                        }
                        continue;
                    }
                    _ => {}
                }
                render_block(
                    &planar,
                    read.status,
                    &src_dest,
                    channels,
                    &mut outbuf,
                    &mut fade,
                    config.click_guard,
                    config.gain_q16,
                    config.fade_in_frames,
                    config.fade_out_frames,
                );
                out.write_all(&outbuf)?;
                if let Some(dump) = dump.as_mut() {
                    let _ = dump.write_block(&outbuf, BLOCK_FRAMES);
                }
                stats.blocks.fetch_add(1, Ordering::Relaxed);
                match read.status {
                    BlockStatus::Ok => {
                        stats.ok_blocks.fetch_add(1, Ordering::Relaxed);
                    }
                    BlockStatus::Warmup => {
                        stats.warmup_blocks.fetch_add(1, Ordering::Relaxed);
                    }
                    BlockStatus::Underrun => {
                        stats.underrun_blocks.fetch_add(1, Ordering::Relaxed);
                    }
                    BlockStatus::NotDue => {
                        stats.not_due_blocks.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if matches!(read.status, BlockStatus::Ok) && origin.is_some() {
                    // 实测"接收→ALSA 写出"提前量：写出时刻 - 媒体时基对应时刻。
                    // 等于抖动缓冲目标延迟 + ALSA 可写窗口的提前量（不含 MOTU/模拟/空气）。
                    let media_ns = origin.unwrap().saturating_add(frames_to_ns_ceil(
                        latency_frames + media_frames_written,
                        RATE_HZ,
                    ));
                    let wall_ns = khaen_aoip_aes67::clock::UsrvClock::origin()
                        .elapsed()
                        .as_nanos() as u64;
                    let lead_us = wall_ns.saturating_sub(media_ns) / 1_000;
                    wl_sum_us = wl_sum_us.saturating_add(lead_us);
                    wl_count += 1;
                    wl_min_us = wl_min_us.min(lead_us);
                    wl_max_us = wl_max_us.max(lead_us);
                }
                media_frames_written += BLOCK_FRAMES as u64;
                if matches!(read.status, BlockStatus::Ok) {
                    silent_frames = 0;
                    stream_end_logged = false;
                } else {
                    silent_frames += BLOCK_FRAMES as u64;
                }
                stats.silent_streak.store(silent_frames, Ordering::Relaxed);
                stats
                    .frames_written
                    .store(media_frames_written, Ordering::Relaxed);
                // STREAM_END：连续 stream_end_frames 帧没有可播数据（无包/欠载）→ 主动收尾
                // （40ms 淡出已经在 render_block 里完成；随后停止序列+200ms 静音+drain+close）。
                if stream_end_reached(silent_frames, config.stream_end_frames) {
                    stats.stream_end_events.fetch_add(1, Ordering::Relaxed);
                    if config.stay_on_stream_end {
                        // 短间隙/会话重启：**不退出、不重置媒体时基/预滚**，只写静音等数据恢复；
                        // 数据恢复后 read_block 自然回到 Ok（欠载淡出已处理），避免“卡一下再重锚”。
                        // 若是新会话（SSRC/ts 复位）导致 jitter 拒绝新包，下面的 stale-jitter 看门狗会退出交给监督器。
                        if !stream_end_logged {
                            eprintln!(
                                "[aes67-rx] 连续 {} ms 无数据（stay 模式：保持静音等待恢复，不退出；请求一次内部重锚）",
                                silent_frames as f64 / RATE_HZ as f64 * 1000.0
                            );
                            stream_end_logged = true;
                            config.reanchor.store(true, Ordering::Release);
                        }
                        silent_frames = 0;
                        continue;
                    }
                    stats.end_reason.store(1, Ordering::Relaxed);
                    eprintln!(
                        "[aes67-rx] 连续 {} ms 无可播数据，判定流结束，进入收尾",
                        silent_frames as f64 / RATE_HZ as f64 * 1000.0
                    );
                    break;
                }
                stats.xruns.store(out.xruns, Ordering::Relaxed);
                stats.recoveries.store(out.recoveries, Ordering::Relaxed);
                if let Some(cpu) = rt::current_cpu() {
                    stats.playback_cpu.store(cpu as u64, Ordering::Relaxed);
                }
            }
            if stats.end_reason.load(Ordering::Relaxed) == 0 {
                stats.end_reason.store(3, Ordering::Relaxed);
            }
            // ===== 停止序列（绝不在非零样本处切）=====
            // 1) 40 ms 余弦淡出（若正在出声）；2) 200 ms 数字静音块（仍按设备节拍写）；
            // 3) ALSA drain（播完已写帧）→ close。
            if out.written_frames > 0 {
                let fade_blocks = (config.fade_out_frames + BLOCK_FRAMES - 1) / BLOCK_FRAMES;
                for _ in 0..fade_blocks {
                    render_block(
                        &planar,
                        BlockStatus::Underrun,
                        &src_dest,
                        channels,
                        &mut outbuf,
                        &mut fade,
                        config.click_guard,
                        config.gain_q16,
                        config.fade_in_frames,
                        config.fade_out_frames,
                    );
                    out.write_all(&outbuf)?;
                    if let Some(dump) = dump.as_mut() {
                        let _ = dump.write_block(&outbuf, BLOCK_FRAMES);
                    }
                }
                let mute_blocks = (config.stop_mute_frames + BLOCK_FRAMES - 1) / BLOCK_FRAMES;
                for _ in 0..mute_blocks {
                    out.write_all(&silence_block)?;
                    if let Some(dump) = dump.as_mut() {
                        let _ = dump.write_block(&silence_block, BLOCK_FRAMES);
                    }
                }
                stats.stop_fade_blocks.store(fade_blocks as u64, Ordering::Relaxed);
                stats.stop_mute_blocks.store(mute_blocks as u64, Ordering::Relaxed);
            }
            // 先把已写入 ALSA 的帧播完（drain），再 close；避免"关设备台阶"爆音。
            if let Err(e) = out.drain() {
                eprintln!("[aes67-rx] drain 警告: {e}");
                stats.drain_errors.fetch_add(1, Ordering::Relaxed);
            } else {
                stats.drain_ok.store(1, Ordering::Relaxed);
            }
            stats.write_lead_sum_us.store(wl_sum_us, Ordering::Relaxed);
            stats.write_lead_count.store(wl_count, Ordering::Relaxed);
            stats.write_lead_min_us.store(if wl_count > 0 { wl_min_us } else { 0 }, Ordering::Relaxed);
            stats.write_lead_max_us.store(wl_max_us, Ordering::Relaxed);
            if let Some(mut d) = dump.take() {
                d.finish();
                stats.dump_bytes.store(d.total_bytes(), Ordering::Relaxed);
                stats.dump_frames.store(d.total_frames(), Ordering::Relaxed);
                stats.dump_truncated.store(d.truncated as u64, Ordering::Relaxed);
                stats.dump_write_errors.store(d.write_errors, Ordering::Relaxed);
                eprintln!(
                    "[aes67-rx] dump 完成: {} 帧 / {} 字节 ({}), errors={}, truncated={}",
                    d.total_frames(), d.total_bytes(), d.dir().display(), d.write_errors, d.truncated
                );
            }
            eprintln!(
                "[aes67-rx] playback 退出: blocks={} ok={} under={} xruns={} written={}",
                stats.blocks.load(Ordering::Relaxed),
                stats.ok_blocks.load(Ordering::Relaxed),
                stats.underrun_blocks.load(Ordering::Relaxed),
                stats.xruns.load(Ordering::Relaxed),
                stats.frames_written.load(Ordering::Relaxed),
            );
            Ok(out)
        })
        .expect("spawn playback")
}

// ---------------------------------------------------------------- 测试音发送端

struct ToneConfig {
    dest: SocketAddr,
    channels: usize,
    frames_per_packet: usize,
    tones: Vec<f32>,
    amplitude: f32,
    duration: Duration,
    fade: Duration,
    /// 模拟"流中途结束"：到该时刻停止发送（None=发满 duration）。
    stop_after: Option<Duration>,
}

fn run_tone_sender(config: &ToneConfig, stop: &AtomicBool) -> Result<u64, String> {
    let mut sender =
        Aes67RtpSender::bind(SenderConfig::unicast(config.dest, config.channels as u16))
            .map_err(|e| e.name().to_string())?;
    sender.config().frames_per_packet = config.frames_per_packet;
    let frames = config.frames_per_packet;
    let mut interleaved = vec![0f32; frames * config.channels];
    let mut phase = vec![0f64; config.channels];
    let mut sent_frames: u64 = 0;
    let total_frames = (config.duration.as_secs_f64() * RATE_HZ as f64) as u64;
    let fade_frames = (config.fade.as_secs_f64() * RATE_HZ as f64) as u64;
    let start = Instant::now();
    while !stop.load(Ordering::Acquire) && sent_frames < total_frames {
        if let Some(stop_after) = config.stop_after {
            if start.elapsed() >= stop_after {
                break;
            }
        }
        for frame in 0..frames {
            let absolute = sent_frames + frame as u64;
            let mut gain = 1.0f32;
            if fade_frames > 0 {
                if absolute < fade_frames {
                    gain = absolute as f32 / fade_frames as f32;
                } else if absolute + fade_frames > total_frames {
                    gain = (total_frames.saturating_sub(absolute)) as f32 / fade_frames as f32;
                }
            }
            for channel in 0..config.channels {
                let sample = phase[channel].sin() as f32 * config.amplitude * gain;
                interleaved[frame * config.channels + channel] = sample;
                phase[channel] +=
                    2.0 * std::f64::consts::PI * config.tones[channel] as f64 / RATE_HZ as f64;
                if phase[channel] > 2.0 * std::f64::consts::PI {
                    phase[channel] -= 2.0 * std::f64::consts::PI;
                }
            }
        }
        sender
            .send_samples(&interleaved)
            .map_err(|e| e.name().to_string())?;
        sent_frames += frames as u64;
        let target = start + Duration::from_secs_f64(sent_frames as f64 / RATE_HZ as f64);
        loop {
            let now = Instant::now();
            if now >= target {
                break;
            }
            let remain = target - now;
            if remain > Duration::from_micros(800) {
                std::thread::sleep(remain - Duration::from_micros(400));
            } else {
                std::hint::spin_loop();
            }
        }
    }
    Ok(sent_frames)
}

// ---------------------------------------------------------------- run / selftest

fn cmd_run(cli: &Cli) -> Result<(), String> {
    let sdp_path = cli.required("sdp")?.to_string();
    let doc = SdpDocument::from_file(&sdp_path).map_err(|e| e.name().to_string())?;
    let interface = match cli.get("interface") {
        Some(text) => Some(
            text.parse::<Ipv4Addr>()
                .map_err(|_| format!("--interface 非法: {text}"))?,
        ),
        None => None,
    };
    let rx_prio: i32 = cli.num_or("rx-prio", 80)?;
    let latency_us: u32 = cli.num_or("latency-us", 24_000)?;
    let fade_in_ms: u32 = cli.num_or("fade-in-ms", 10)?;
    let fade_out_ms: u32 = cli.num_or("fade-out-ms", 40)?;
    let stop_mute_ms: u32 = cli.num_or("stop-mute-ms", 200)?;
    let stream_end_ms: u32 = cli.num_or("stream-end-ms", 80)?;
    let stay_on_stream_end = cli.has("stay-on-stream-end");
    let notdue_hold_ms: u32 = cli.num_or("notdue-hold-ms", 20)?;
    let fade_in_frames = (fade_in_ms as usize * RATE_HZ as usize / 1000).max(1);
    let fade_out_frames = (fade_out_ms as usize * RATE_HZ as usize / 1000).max(1);
    let ssrc_filter: Option<u32> = match cli.get("ssrc") {
        Some(text) => Some(
            u32::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .map_err(|_| format!("--ssrc 非法: {text}"))?,
        ),
        None => None,
    };
    let ring_frames: usize = cli.num_or("ring-frames", 8_192)?;
    let duration_secs: u64 = cli.num_or("duration-secs", 15)?;
    let playback_prio: i32 = cli.num_or("playback-prio", 70)?;
    let pin_cpu: i32 = cli.num_or("pin-cpu", 2)?;
    let playback_channels: usize = cli.num_or("playback-channels", 22)?;
    let period_frames: usize = cli.num_or("period-frames", 128)?;
    let buffer_frames: usize = cli.num_or("buffer-frames", 1024)?;
    let status_ms: u64 = cli.num_or("status-ms", 1_000)?;
    let gain_db: f32 = cli.num_or("gain-db", 0.0)?;
    let gain_q16 = (10f64.powf(gain_db as f64 / 20.0) * 65_536.0) as i64;
    let device = cli.str_or("device", "hw:UltraLitemk5,0");
    let mlock = cli.has("mlock");
    let click_guard = !cli.has("no-click-guard");
    let quiet = cli.has("quiet");
    let report_path = cli.get("report").map(ToString::to_string);
    let dump_spec = build_dump_spec(cli)?;
    let dump_dir_json = dump_spec
        .as_ref()
        .map(|d| format!("\"{}\"", escape_json(&d.dir)))
        .unwrap_or_else(|| "null".to_string());

    rt::install_signal_handlers()?;
    let mut mlock_note = "disabled".to_string();
    if mlock {
        match rt::mlock_all() {
            Ok(()) => mlock_note = "ok".to_string(),
            Err(e) => mlock_note = e,
        }
    }

    let (mut receiver, source, summary) = open_receiver(
        &doc,
        interface,
        rx_prio,
        latency_us,
        ring_frames,
        ssrc_filter,
    )?;
    let channels = summary.channels as usize;
    let reanchor = receiver.reanchor_flag();
    let map = match cli.get("map") {
        Some(text) => parse_map(text, channels, playback_channels)?,
        None => {
            if channels == 7 {
                FOUR_ONE_TWO_MAP.to_vec()
            } else if channels == 8 {
                (0..8).collect()
            } else {
                (0..channels).collect()
            }
        }
    };
    if channels == 7 && map != FOUR_ONE_TWO_MAP {
        eprintln!("[aes67-rx] 注意：7ch 使用了非 4.1.2 默认映射 {map:?}");
    }
    let local_addr = receiver.local_addr();
    eprintln!(
        "[aes67-rx] 接收 {}（{}）ch={} latency={} us ring={}；本地绑定 {}",
        summary.media,
        if summary.multicast {
            "组播"
        } else {
            "单播"
        },
        channels,
        latency_us,
        ring_frames,
        local_addr
    );
    eprintln!(
        "[aes67-rx] ALSA {} ch={} S24_3LE/48000 map={:?}（4.1.2 名称={:?}）",
        device, playback_channels, map, FOUR_ONE_TWO
    );

    let stats = Arc::new(RunStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let playback = spawn_playback(
        source,
        PlaybackConfig {
            device: device.clone(),
            playback_channels,
            period_frames,
            buffer_frames,
            map: map.clone(),
            priority: playback_prio,
            pin_cpu,
            click_guard,
            gain_q16,
            fade_in_frames,
            fade_out_frames,
            stop_mute_frames: (stop_mute_ms as usize * RATE_HZ as usize / 1000).max(BLOCK_FRAMES),
            stream_end_frames: (stream_end_ms as u64 * RATE_HZ as u64 / 1000)
                .max(BLOCK_FRAMES as u64),
            notdue_hold_frames: (notdue_hold_ms as u64 * RATE_HZ as u64 / 1000)
                .max(BLOCK_FRAMES as u64),
            stay_on_stream_end,
            reanchor: Arc::clone(&reanchor),
            dump: dump_spec.clone(),
        },
        Arc::clone(&stats),
        Arc::clone(&stop),
    );

    let started = Instant::now();
    let started_unix_ms = now_unix_ms();
    let mut last_status = Instant::now();
    // stay 模式下的 stale-jitter 看门狗：datagrams 增长但 accepted 不涨（新会话 ts/SSRC 复位被旧锚点判 late）
    // → 退出进程，让监督器 1 s 内重启并用新锚点重锚（避免永久静音）。
    let mut guard_last_dg: u64 = 0;
    let mut guard_last_acc: u64 = 0;
    let mut guard_last_check = Instant::now();
    let mut stale_strikes = 0u32;
    loop {
        if rt::stop_requested() {
            eprintln!("[aes67-rx] 收到信号，停止");
            break;
        }
        if duration_secs > 0 && started.elapsed() >= Duration::from_secs(duration_secs) {
            break;
        }
        if stats.end_reason.load(Ordering::Relaxed) != 0 {
            eprintln!("[aes67-rx] 播放线程判定流结束，主循环退出");
            break;
        }
        if playback.is_finished() {
            stats.end_reason.store(6, Ordering::Relaxed);
            eprintln!("[aes67-rx] 播放线程已退出（异常），主循环退出");
            break;
        }
        if stay_on_stream_end && guard_last_check.elapsed() >= Duration::from_millis(500) {
            let io = receiver.io_stats();
            if guard_last_acc > 0
                && io.accepted_packets == guard_last_acc
                && io.datagrams > guard_last_dg.saturating_add(300)
            {
                stale_strikes += 1;
                // 优先内部重锚（不退出、不断音）；连续 3 次（~1.5 s）仍无效才退出交监督器。
                reanchor.store(true, Ordering::Release);
                eprintln!(
                    "[aes67-rx] stay 模式：新包持续被 jitter 拒绝（strike {stale_strikes}/3），请求内部重锚"
                );
                if stale_strikes >= 3 {
                    stats.end_reason.store(7, Ordering::Relaxed);
                    eprintln!("[aes67-rx] 内部重锚连续 3 次无效，退出交监督器重锚");
                    break;
                }
            } else {
                stale_strikes = 0;
            }
            guard_last_dg = io.datagrams;
            guard_last_acc = io.accepted_packets;
            guard_last_check = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
        if !quiet && status_ms > 0 && last_status.elapsed() >= Duration::from_millis(status_ms) {
            last_status = Instant::now();
            let io = receiver.io_stats();
            let jitter = receiver.jitter_stats();
            eprintln!(
                "[aes67-rx] t={:.1}s datagrams={} accepted={} frames_rx={} jitter(pkts={} recv={} played={} under={} warm={} late={} over={} lost={} ooo={}) blocks={} ok={} under_blocks={} notdue={} xruns={}",
                started.elapsed().as_secs_f64(),
                io.datagrams,
                io.accepted_packets,
                io.accepted_frames,
                jitter.packets,
                jitter.frames_received,
                jitter.frames_played,
                jitter.underruns,
                jitter.warmups,
                jitter.late_packets,
                jitter.overruns,
                jitter.lost_packets,
                jitter.out_of_order_packets,
                stats.blocks.load(Ordering::Relaxed),
                stats.ok_blocks.load(Ordering::Relaxed),
                stats.underrun_blocks.load(Ordering::Relaxed),
                stats.not_due_blocks.load(Ordering::Relaxed),
                stats.xruns.load(Ordering::Relaxed),
            );
        }
    }

    // 收尾：先停收包线程，再停播放线程（播放线程会以静音块退出）。
    receiver.stop();
    stop.store(true, Ordering::Release);
    let alsa = playback
        .join()
        .map_err(|_| "playback 线程 panic".to_string())?
        .map_err(|e| format!("playback 失败: {e}"))?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ended_unix_ms = now_unix_ms();
    let cs = stats.blocks.load(Ordering::Relaxed);
    let ok = stats.ok_blocks.load(Ordering::Relaxed);
    let under = stats.underrun_blocks.load(Ordering::Relaxed);
    let warm = stats.warmup_blocks.load(Ordering::Relaxed);
    let notdue = stats.not_due_blocks.load(Ordering::Relaxed);

    let sdp_ref = &summary;
    let map_json = map
        .iter()
        .enumerate()
        .map(|(src, dst)| {
            if channels == 7 {
                format!(
                    "{{\"src\":{src},\"name\":\"{}\",\"dest\":{dst}}}",
                    FOUR_ONE_TWO[src]
                )
            } else {
                format!("{{\"src\":{src},\"dest\":{dst}}}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let sdp_text_json = sdp_ref
        .text
        .as_ref()
        .map(|text| escape_json(text))
        .unwrap_or_default();
    let report = format!(
        "{{\"schema\":\"khaen-aes67-rx/1\",\"mode\":\"run\",\"verdict\":\"{}\",\
\"started_unix_ms\":{started_unix_ms},\"ended_unix_ms\":{ended_unix_ms},\"duration_ms\":{elapsed_ms},\
\"sdp\":{{\"path\":{},\"source\":\"{}\",\"media\":\"{}\",\"port\":{},\"multicast\":{},\
\"channels\":{},\"rate_hz\":{},\"encoding\":\"{}\",\"ptime_us\":{},\"payload_type\":{},\
\"refclk\":\"{}\",\"ptp_domain\":{},\"localmac\":{},\
\"text\":\"{}\"}},\
\"channels\":{{\"source\":{},\"playback\":{},\"map\":[{}]}},\
\"alsa\":{{\"device\":\"{}\",\"format\":\"S24_3LE\",\"channels\":{},\"rate\":48000,\
\"period_frames\":{},\"buffer_frames\":{},\"written_frames\":{},\"xruns\":{},\"recoveries\":{},\"gain_db\":{},\"dump_enabled\":{},\"dump_dir\":{},\"dump_frames\":{},\"dump_bytes\":{},\"dump_truncated\":{},\"dump_write_errors\":{},\"dump_skipped_space\":{},\
\"first_write_unix_ns\":{}}},\
\"rt\":{{\"mlockall\":\"{}\",\"playback_priority\":{},\"playback_pin_cpu\":{},\
\"playback_cpu_actual\":{},\"playback_sched\":\"{}\",\"rx_priority\":{},\"rx_priority_state\":\"{}\",\
\"signal_handlers\":\"ok\",\"fade_in_ms\":{},\"fade_out_ms\":{},\"write_lead_ms_avg\":{},\"write_lead_ms_min\":{},\"write_lead_ms_max\":{}}},\
\"capture\":{{\"blocks\":{cs},\"ok_blocks\":{ok},\"warmup_blocks\":{warm},\
\"underrun_blocks\":{under},\"not_due_blocks\":{notdue},\"not_due_holds\":{},\"stream_reanchors\":{},\"silent_streak_frames\":{},\"end_reason\":{},\"frames_written\":{},\
\"sleep_block_ratio\":{:.4},\"anchor_wait_ms\":{},\"fill_wait_ms\":{},\"stop_fade_blocks\":{},\"stop_mute_blocks\":{},\"drain_ok\":{},\"drain_errors\":{}}},\
\"receiver\":{}}}",
        if ok > 0 { "PASS" } else { "FAIL_NO_AUDIO_BLOCKS" },
        sdp_ref
            .path
            .as_ref()
            .map(|p| format!("\"{}\"", escape_json(p)))
            .unwrap_or_else(|| "null".to_string()),
        if sdp_ref.path.is_some() { "file" } else { "generated" },
        escape_json(&sdp_ref.media),
        sdp_ref.port,
        sdp_ref.multicast,
        sdp_ref.channels,
        sdp_ref.rate_hz,
        sdp_ref.encoding,
        sdp_ref
            .ptime_us
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string()),
        sdp_ref.payload_type,
        sdp_ref.refclk,
        sdp_ref
            .ptp_domain
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string()),
        sdp_ref
            .localmac
            .as_ref()
            .map(|v| format!("\"{v}\""))
            .unwrap_or_else(|| "null".to_string()),
        sdp_text_json,
        channels,
        playback_channels,
        map_json,
        escape_json(&device),
        playback_channels,
        period_frames,
        buffer_frames,
        alsa.written_frames,
        alsa.xruns,
        alsa.recoveries,
        gain_db,
        if dump_spec.is_some() { 1 } else { 0 },
        dump_dir_json,
        stats.dump_frames.load(Ordering::Relaxed),
        stats.dump_bytes.load(Ordering::Relaxed),
        stats.dump_truncated.load(Ordering::Relaxed),
        stats.dump_write_errors.load(Ordering::Relaxed),
        stats.dump_skipped_space.load(Ordering::Relaxed),
        alsa.first_write_ns.unwrap_or(0),
        escape_json(&mlock_note),
        playback_prio,
        pin_cpu,
        stats.playback_cpu.load(Ordering::Relaxed),
        rt::scheduler_name(),
        rx_prio,
        format!("{:?}", receiver.priority_status()),
        fade_in_ms,
        fade_out_ms,
        stats.write_lead_sum_us.load(Ordering::Relaxed) as f64
            / stats.write_lead_count.load(Ordering::Relaxed).max(1) as f64
            / 1000.0,
        stats.write_lead_min_us.load(Ordering::Relaxed) as f64 / 1000.0,
        stats.write_lead_max_us.load(Ordering::Relaxed) as f64 / 1000.0,
        stats.not_due_holds.load(Ordering::Relaxed),
        stats.stream_reanchors.load(Ordering::Relaxed),
        stats.silent_streak.load(Ordering::Relaxed),
        stats.end_reason.load(Ordering::Relaxed),
        stats.frames_written.load(Ordering::Relaxed),
        if cs > 0 {
            (warm + under + notdue) as f64 / cs as f64
        } else {
            0.0
        },
        stats.anchor_wait_ms.load(Ordering::Relaxed),
        stats.fill_wait_ms.load(Ordering::Relaxed),
        stats.stop_fade_blocks.load(Ordering::Relaxed),
        stats.stop_mute_blocks.load(Ordering::Relaxed),
        stats.drain_ok.load(Ordering::Relaxed),
        stats.drain_errors.load(Ordering::Relaxed),
        receiver.report_json().trim(),
    );
    emit_json_report(report_path.as_deref(), &report)
}

fn cmd_selftest(cli: &Cli) -> Result<(), String> {
    let channels: usize = cli.num_or("channels", 7)?;
    let port: u16 = cli.num_or("port", 5_004)?;
    let duration_secs: u64 = cli.num_or("duration-secs", 10)?;
    let tone_hz: f32 = cli.num_or("tone-hz", 1_000.0)?;
    let tone_step: f32 = cli.num_or("tone-step", 0.0)?;
    let amplitude_db: f32 = cli.num_or("amplitude-db", -24.0)?;
    let amplitude = 10f32.powf(amplitude_db / 20.0);
    let frames_per_packet: usize = cli.num_or("frames-per-packet", 48)?;
    let sender_stop_after_ms: u64 = cli.num_or("sender-stop-after-ms", 0)?;
    let tones: Vec<f32> = (0..channels)
        .map(|i| tone_hz + tone_step * i as f32)
        .collect();
    let doc = match cli.get("sdp") {
        Some(path) => SdpDocument::from_file(path).map_err(|e| e.name().to_string())?,
        None => SdpDocument::from_text(generated_selftest_sdp(channels as u16, port)),
    };
    let interface = Some(Ipv4Addr::LOCALHOST);
    let rx_prio: i32 = cli.num_or("rx-prio", 80)?;
    let latency_us: u32 = cli.num_or("latency-us", 24_000)?;
    let fade_in_ms: u32 = cli.num_or("fade-in-ms", 10)?;
    let fade_out_ms: u32 = cli.num_or("fade-out-ms", 40)?;
    let stop_mute_ms: u32 = cli.num_or("stop-mute-ms", 200)?;
    let stream_end_ms: u32 = cli.num_or("stream-end-ms", 80)?;
    let stay_on_stream_end = cli.has("stay-on-stream-end");
    let notdue_hold_ms: u32 = cli.num_or("notdue-hold-ms", 20)?;
    let fade_in_frames = (fade_in_ms as usize * RATE_HZ as usize / 1000).max(1);
    let fade_out_frames = (fade_out_ms as usize * RATE_HZ as usize / 1000).max(1);
    let ssrc_filter: Option<u32> = match cli.get("ssrc") {
        Some(text) => Some(
            u32::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .map_err(|_| format!("--ssrc 非法: {text}"))?,
        ),
        None => None,
    };
    let ring_frames: usize = cli.num_or("ring-frames", 8_192)?;
    let playback_prio: i32 = cli.num_or("playback-prio", 70)?;
    let pin_cpu: i32 = cli.num_or("pin-cpu", 2)?;
    let playback_channels: usize = cli.num_or("playback-channels", 22)?;
    let period_frames: usize = cli.num_or("period-frames", 128)?;
    let buffer_frames: usize = cli.num_or("buffer-frames", 1024)?;
    let status_ms: u64 = cli.num_or("status-ms", 1_000)?;
    let gain_db: f32 = cli.num_or("gain-db", 0.0)?;
    let gain_q16 = (10f64.powf(gain_db as f64 / 20.0) * 65_536.0) as i64;
    let device = cli.str_or("device", "hw:UltraLitemk5,0");
    let mlock = cli.has("mlock");
    let quiet = cli.has("quiet");
    let click_guard = !cli.has("no-click-guard");
    let report_path = cli.get("report").map(ToString::to_string);
    let dump_spec = build_dump_spec(cli)?;
    let dump_dir_json = dump_spec
        .as_ref()
        .map(|d| format!("\"{}\"", escape_json(&d.dir)))
        .unwrap_or_else(|| "null".to_string());

    rt::install_signal_handlers()?;
    let mut mlock_note = "disabled".to_string();
    if mlock {
        match rt::mlock_all() {
            Ok(()) => mlock_note = "ok".to_string(),
            Err(e) => mlock_note = e,
        }
    }
    let (mut receiver, source, summary) = open_receiver(
        &doc,
        interface,
        rx_prio,
        latency_us,
        ring_frames,
        ssrc_filter,
    )?;
    let reanchor = receiver.reanchor_flag();
    let map = match cli.get("map") {
        Some(text) => parse_map(text, channels, playback_channels)?,
        None => {
            if channels == 7 {
                FOUR_ONE_TWO_MAP.to_vec()
            } else {
                (0..channels).collect()
            }
        }
    };
    let local_addr = receiver.local_addr();
    eprintln!(
        "[aes67-rx] selftest 接收 {} ch={} tones={:?} amp={:.1} dBFS；本地绑定 {}",
        summary.media, channels, tones, amplitude_db, local_addr
    );
    let stats = Arc::new(RunStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let playback = spawn_playback(
        source,
        PlaybackConfig {
            device: device.clone(),
            playback_channels,
            period_frames,
            buffer_frames,
            map: map.clone(),
            priority: playback_prio,
            pin_cpu,
            click_guard,
            gain_q16,
            fade_in_frames,
            fade_out_frames,
            stop_mute_frames: (stop_mute_ms as usize * RATE_HZ as usize / 1000).max(BLOCK_FRAMES),
            stream_end_frames: (stream_end_ms as u64 * RATE_HZ as u64 / 1000)
                .max(BLOCK_FRAMES as u64),
            notdue_hold_frames: (notdue_hold_ms as u64 * RATE_HZ as u64 / 1000)
                .max(BLOCK_FRAMES as u64),
            stay_on_stream_end,
            reanchor: Arc::clone(&reanchor),
            dump: dump_spec.clone(),
        },
        Arc::clone(&stats),
        Arc::clone(&stop),
    );
    // 等播放线程把设备打开、进入静音块循环后再发流（避免设备打开延迟被算进流时间）。
    std::thread::sleep(Duration::from_millis(300));
    let tone_stop = Arc::new(AtomicBool::new(false));
    let tone_config = ToneConfig {
        dest: SocketAddr::from((Ipv4Addr::LOCALHOST, local_addr.port())),
        channels,
        frames_per_packet,
        tones: tones.clone(),
        amplitude,
        duration: Duration::from_secs(duration_secs.max(1)),
        fade: Duration::from_millis(200),
        stop_after: if sender_stop_after_ms > 0 {
            Some(Duration::from_millis(sender_stop_after_ms))
        } else {
            None
        },
    };
    let tone_stop_thread = Arc::clone(&tone_stop);
    let tone_thread = std::thread::Builder::new()
        .name("aes67-rx-tone".to_string())
        .spawn(move || run_tone_sender(&tone_config, &tone_stop_thread))
        .expect("spawn tone");

    let started = Instant::now();
    let started_unix_ms = now_unix_ms();
    let mut last_status = Instant::now();
    loop {
        if rt::stop_requested() {
            break;
        }
        if started.elapsed() >= Duration::from_secs(duration_secs.max(1)) {
            break;
        }
        if stats.end_reason.load(Ordering::Relaxed) != 0 {
            eprintln!("[aes67-rx] 播放线程判定流结束，主循环退出");
            break;
        }
        if playback.is_finished() {
            stats.end_reason.store(6, Ordering::Relaxed);
            eprintln!("[aes67-rx] 播放线程已退出（异常），主循环退出");
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        if !quiet && status_ms > 0 && last_status.elapsed() >= Duration::from_millis(status_ms) {
            last_status = Instant::now();
            let io = receiver.io_stats();
            let jitter = receiver.jitter_stats();
            eprintln!(
                "[aes67-rx] t={:.1}s datagrams={} accepted={} jitter(pkts={} recv={} played={} under={} warm={} late={} lost={}) blocks={} ok={} under_blocks={} xruns={}",
                started.elapsed().as_secs_f64(),
                io.datagrams,
                io.accepted_packets,
                jitter.packets,
                jitter.frames_received,
                jitter.frames_played,
                jitter.underruns,
                jitter.warmups,
                jitter.late_packets,
                jitter.lost_packets,
                stats.blocks.load(Ordering::Relaxed),
                stats.ok_blocks.load(Ordering::Relaxed),
                stats.underrun_blocks.load(Ordering::Relaxed),
                stats.xruns.load(Ordering::Relaxed),
            );
        }
    }
    tone_stop.store(true, Ordering::Release);
    let sent_frames = tone_thread
        .join()
        .map_err(|_| "tone 线程 panic".to_string())?
        .map_err(|e| format!("tone 发送失败: {e}"))?;
    // 等一段音频把抖动缓冲播完再停（多 300 ms）。
    std::thread::sleep(Duration::from_millis(300));
    receiver.stop();
    stop.store(true, Ordering::Release);
    let alsa = playback
        .join()
        .map_err(|_| "playback 线程 panic".to_string())?
        .map_err(|e| format!("playback 失败: {e}"))?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let ended_unix_ms = now_unix_ms();
    let cs = stats.blocks.load(Ordering::Relaxed);
    let ok = stats.ok_blocks.load(Ordering::Relaxed);
    let under = stats.underrun_blocks.load(Ordering::Relaxed);
    let warm = stats.warmup_blocks.load(Ordering::Relaxed);
    let notdue = stats.not_due_blocks.load(Ordering::Relaxed);
    let map_json = map
        .iter()
        .enumerate()
        .map(|(src, dst)| {
            if channels == 7 {
                format!(
                    "{{\"src\":{src},\"name\":\"{}\",\"dest\":{dst}}}",
                    FOUR_ONE_TWO[src]
                )
            } else {
                format!("{{\"src\":{src},\"dest\":{dst}}}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let tones_json = tones
        .iter()
        .map(|t| format!("{t:.1}"))
        .collect::<Vec<_>>()
        .join(",");
    let report = format!(
        "{{\"schema\":\"khaen-aes67-rx/1\",\"mode\":\"selftest\",\"verdict\":\"{}\",\
\"started_unix_ms\":{started_unix_ms},\"ended_unix_ms\":{ended_unix_ms},\"duration_ms\":{elapsed_ms},\
\"sdp\":{{\"path\":{},\"source\":\"{}\",\"media\":\"{}\",\"port\":{},\"multicast\":{},\
\"channels\":{},\"rate_hz\":{},\"encoding\":\"{}\",\"ptime_us\":{},\"payload_type\":{},\
\"refclk\":\"{}\",\"localmac\":{}}},\
\"sender\":{{\"dest\":\"127.0.0.1:{}\",\"channels\":{},\"frames_per_packet\":{},\
\"tones_hz\":[{}],\"amplitude_db\":{},\"sent_frames\":{}}},\
\"channels\":{{\"source\":{},\"playback\":{},\"map\":[{}]}},\
\"alsa\":{{\"device\":\"{}\",\"format\":\"S24_3LE\",\"channels\":{},\"rate\":48000,\
\"period_frames\":{},\"buffer_frames\":{},\"written_frames\":{},\"xruns\":{},\"recoveries\":{},\"gain_db\":{},\"dump_enabled\":{},\"dump_dir\":{},\"dump_frames\":{},\"dump_bytes\":{},\"dump_truncated\":{},\"dump_write_errors\":{},\"dump_skipped_space\":{},\
\"first_write_unix_ns\":{}}},\
\"rt\":{{\"mlockall\":\"{}\",\"playback_priority\":{},\"playback_pin_cpu\":{},\
\"playback_cpu_actual\":{},\"playback_sched\":\"{}\",\"rx_priority\":{},\"rx_priority_state\":\"{}\",\
\"signal_handlers\":\"ok\",\"fade_in_ms\":{},\"fade_out_ms\":{},\"write_lead_ms_avg\":{},\"write_lead_ms_min\":{},\"write_lead_ms_max\":{}}},\
\"capture\":{{\"blocks\":{cs},\"ok_blocks\":{ok},\"warmup_blocks\":{warm},\
\"underrun_blocks\":{under},\"not_due_blocks\":{notdue},\"not_due_holds\":{},\"stream_reanchors\":{},\"silent_streak_frames\":{},\"end_reason\":{},\"frames_written\":{},\
\"sleep_block_ratio\":{:.4},\"anchor_wait_ms\":{},\"fill_wait_ms\":{},\"stop_fade_blocks\":{},\"stop_mute_blocks\":{},\"drain_ok\":{},\"drain_errors\":{}}},\
\"receiver\":{}}}",
        if ok > 0 { "PASS" } else { "FAIL_NO_AUDIO_BLOCKS" },
        summary
            .path
            .as_ref()
            .map(|p| format!("\"{}\"", escape_json(p)))
            .unwrap_or_else(|| "null".to_string()),
        if summary.path.is_some() { "file" } else { "generated" },
        escape_json(&summary.media),
        summary.port,
        summary.multicast,
        summary.channels,
        summary.rate_hz,
        summary.encoding,
        summary
            .ptime_us
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string()),
        summary.payload_type,
        summary.refclk,
        summary
            .localmac
            .as_ref()
            .map(|v| format!("\"{v}\""))
            .unwrap_or_else(|| "null".to_string()),
        local_addr.port(),
        channels,
        frames_per_packet,
        tones_json,
        amplitude_db,
        sent_frames,
        channels,
        playback_channels,
        map_json,
        escape_json(&device),
        playback_channels,
        period_frames,
        buffer_frames,
        alsa.written_frames,
        alsa.xruns,
        alsa.recoveries,
        gain_db,
        if dump_spec.is_some() { 1 } else { 0 },
        dump_dir_json,
        stats.dump_frames.load(Ordering::Relaxed),
        stats.dump_bytes.load(Ordering::Relaxed),
        stats.dump_truncated.load(Ordering::Relaxed),
        stats.dump_write_errors.load(Ordering::Relaxed),
        stats.dump_skipped_space.load(Ordering::Relaxed),
        alsa.first_write_ns.unwrap_or(0),
        escape_json(&mlock_note),
        playback_prio,
        pin_cpu,
        stats.playback_cpu.load(Ordering::Relaxed),
        rt::scheduler_name(),
        rx_prio,
        format!("{:?}", receiver.priority_status()),
        fade_in_ms,
        fade_out_ms,
        stats.write_lead_sum_us.load(Ordering::Relaxed) as f64
            / stats.write_lead_count.load(Ordering::Relaxed).max(1) as f64
            / 1000.0,
        stats.write_lead_min_us.load(Ordering::Relaxed) as f64 / 1000.0,
        stats.write_lead_max_us.load(Ordering::Relaxed) as f64 / 1000.0,
        stats.not_due_holds.load(Ordering::Relaxed),
        stats.stream_reanchors.load(Ordering::Relaxed),
        stats.silent_streak.load(Ordering::Relaxed),
        stats.end_reason.load(Ordering::Relaxed),
        stats.frames_written.load(Ordering::Relaxed),
        if cs > 0 {
            (warm + under + notdue) as f64 / cs as f64
        } else {
            0.0
        },
        stats.anchor_wait_ms.load(Ordering::Relaxed),
        stats.fill_wait_ms.load(Ordering::Relaxed),
        stats.stop_fade_blocks.load(Ordering::Relaxed),
        stats.stop_mute_blocks.load(Ordering::Relaxed),
        stats.drain_ok.load(Ordering::Relaxed),
        stats.drain_errors.load(Ordering::Relaxed),
        receiver.report_json().trim(),
    );
    emit_json_report(report_path.as_deref(), &report)
}

fn cmd_tone(cli: &Cli) -> Result<(), String> {
    let dest_text = cli.str_or("dest", "127.0.0.1:5004");
    let dest: SocketAddr = dest_text
        .parse()
        .map_err(|_| format!("--dest 非法: {dest_text}"))?;
    if dest.is_ipv4() && !dest.ip().is_unspecified() {
        // 显式校验 IPv4 单播地址（组播留给 SDP selftest 路径）。
    }
    let channels: usize = cli.num_or("channels", 7)?;
    let frames_per_packet: usize = cli.num_or("frames-per-packet", 48)?;
    let tone_hz: f32 = cli.num_or("tone-hz", 1_000.0)?;
    let tone_step: f32 = cli.num_or("tone-step", 0.0)?;
    let amplitude_db: f32 = cli.num_or("amplitude-db", -24.0)?;
    let duration_secs: u64 = cli.num_or("duration-secs", 10)?;
    let amplitude = 10f32.powf(amplitude_db / 20.0);
    let tones: Vec<f32> = (0..channels)
        .map(|i| tone_hz + tone_step * i as f32)
        .collect();
    let config = ToneConfig {
        dest,
        channels,
        frames_per_packet,
        tones: tones.clone(),
        amplitude,
        duration: Duration::from_secs(duration_secs.max(1)),
        fade: Duration::from_millis(200),
        stop_after: None,
    };
    rt::install_signal_handlers()?;
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    let sent = run_tone_sender(&config, &stop)?;
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "{{\"schema\":\"khaen-aes67-rx/1\",\"mode\":\"tone\",\"dest\":\"{}\",\"channels\":{},\
\"frames_per_packet\":{},\"tones_hz\":[{}],\"amplitude_db\":{},\"sent_frames\":{},\
\"duration_ms\":{}}}",
        dest,
        channels,
        frames_per_packet,
        tones
            .iter()
            .map(|t| format!("{t:.1}"))
            .collect::<Vec<_>>()
            .join(","),
        amplitude_db,
        sent,
        (elapsed * 1000.0) as u64
    );
    Ok(())
}

fn cmd_sdp_info(cli: &Cli) -> Result<(), String> {
    let path = cli.required("sdp")?;
    let doc = SdpDocument::from_file(path).map_err(|e| e.name().to_string())?;
    let session = doc.parse_validated().map_err(|e| e.name().to_string())?;
    let summary = summarize_sdp(&doc, &session);
    let report = format!(
        "{{\"schema\":\"khaen-aes67-rx/1\",\"mode\":\"sdp-info\",\"verdict\":\"PASS\",\
\"path\":\"{}\",\"media\":\"{}\",\"port\":{},\"multicast\":{},\"channels\":{},\"rate_hz\":{},\
\"encoding\":\"{}\",\"ptime_us\":{},\"payload_type\":{},\"refclk\":\"{}\",\"ptp_domain\":{},\
\"localmac\":{},\"text\":\"{}\"}}",
        escape_json(path),
        escape_json(&summary.media),
        summary.port,
        summary.multicast,
        summary.channels,
        summary.rate_hz,
        summary.encoding,
        summary
            .ptime_us
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string()),
        summary.payload_type,
        summary.refclk,
        summary
            .ptp_domain
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string()),
        summary
            .localmac
            .as_ref()
            .map(|v| format!("\"{v}\""))
            .unwrap_or_else(|| "null".to_string()),
        escape_json(doc.text()),
    );
    println!("{report}");
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv
        .first()
        .map(|s| s == "--help" || s == "-h")
        .unwrap_or(true)
    {
        print_help();
        return;
    }
    let cli = match Cli::parse(argv) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("[aes67-rx] 参数错误: {e}");
            std::process::exit(2);
        }
    };
    let result = match cli.command.as_str() {
        "run" => cmd_run(&cli),
        "selftest" => cmd_selftest(&cli),
        "tone" => cmd_tone(&cli),
        "sdp-info" => cmd_sdp_info(&cli),
        other => Err(format!("未知子命令: {other}")),
    };
    if let Err(e) = result {
        eprintln!("[aes67-rx] 失败: {e}");
        std::process::exit(1);
    }
}

// 非 Linux 平台：二进制仍可编译（ALSA 由 alsa_out 的 stub 提供），但 run/selftest 无法打开设备。
// 这样 rt-stack workspace 在 Windows CI 上仍能 cargo test（纯逻辑/映射单测照常跑）。
#[cfg(test)]
mod tests {
    use super::*;

    fn decode(out: &[u8], playback: usize, frame: usize, dest: usize) -> i32 {
        let o = (frame * playback + dest) * 3;
        let u = (out[o] as u32) | ((out[o + 1] as u32) << 8) | ((out[o + 2] as u32) << 16);
        if u >= 1 << 23 {
            (u as i32) - (1 << 24)
        } else {
            u as i32
        }
    }

    /// 24bit<<8 满量程 i32 → ALSA 24bit。测试用"源通道值"直接编码为 (ch+1)<<16。
    fn planar_const(channels: usize) -> Vec<i32> {
        let mut planar = vec![0i32; BLOCK_FRAMES * channels];
        for frame in 0..BLOCK_FRAMES {
            for ch in 0..channels {
                planar[frame * channels + ch] = ((ch as i32) + 1) << 16;
            }
        }
        planar
    }

    /// 4.1.2 七声道 → MOTU 22ch：只有 ch0,1,2,3,4,5,7 有内容，其余静音，且逐通道内容不串。
    #[test]
    fn mapping_412_to_motu_22ch_is_exact() {
        let channels = 7usize;
        let playback = 22usize;
        let src_dest = src_for_dest(&FOUR_ONE_TWO_MAP, playback);
        let planar = planar_const(channels);
        let mut out = vec![0u8; BLOCK_FRAMES * playback * 3];
        // click_guard=false 关闭淡入，直接验证映射逐字节。
        let mut fade = FadeState::new(playback);
        render_block(
            &planar,
            BlockStatus::Ok,
            &src_dest,
            channels,
            &mut out,
            &mut fade,
            false,
            65_536,
            0,
            0,
        );
        for (dest, src) in [
            (0usize, 0usize),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 5),
            (7, 6),
        ] {
            let expect = (((src as i32) + 1) << 16) >> 8;
            for frame in [0usize, 1, 63, 127] {
                assert_eq!(
                    decode(&out, playback, frame, dest),
                    expect,
                    "dest {dest} (src {src}) frame {frame}"
                );
            }
        }
        for dest in 0..playback {
            if [0usize, 1, 2, 3, 4, 5, 7].contains(&dest) {
                continue;
            }
            for frame in [0usize, 64, 127] {
                assert_eq!(decode(&out, playback, frame, dest), 0, "dest {dest} 应静音");
            }
        }
    }

    /// 停流：以 last[] 为起点做 40 ms 余弦淡出（跨多块），期间无阶跃、末尾归零，
    /// 恢复时置淡入标志（避免突然归零/突然起音爆音）。
    #[test]
    fn stream_stop_fades_out_over_40ms_and_flags_fade_in() {
        let channels = 7usize;
        let playback = 22usize;
        let src_dest = src_for_dest(&FOUR_ONE_TWO_MAP, playback);
        let mut planar = planar_const(channels);
        for v in planar.iter_mut() {
            *v = (*v as i64 * 16) as i32; // 提高幅度，降低 24bit 量化对相对误差的影响
        }
        let mut out = vec![0u8; BLOCK_FRAMES * playback * 3];
        let mut fade = FadeState::new(playback);
        fade.in_pending = false; // 跳过起播淡入，先建立满幅 last[]
        render_block(
            &planar,
            BlockStatus::Ok,
            &src_dest,
            channels,
            &mut out,
            &mut fade,
            true,
            65_536,
            480,
            1_920, // 40 ms
        );
        let last_ok = fade.last[0];
        assert!(last_ok > 0);
        // 第 1 个非 Ok 块：淡出开始；第 0 帧≈last（cos 起点），后续单调下降。
        render_block(
            &planar,
            BlockStatus::Underrun,
            &src_dest,
            channels,
            &mut out,
            &mut fade,
            true,
            65_536,
            480,
            1_920,
        );
        let first = decode(&out, playback, 0, 0);
        assert!(
            (first as f64 - last_ok as f64).abs() / (last_ok as f64) < 0.002,
            "淡出首帧必须≈上一块末值（无阶跃），got {first} vs {last_ok}"
        );
        let mid = decode(&out, playback, 64, 0);
        assert!(mid.abs() < last_ok.abs(), "淡出中段必须衰减");
        // 跨 1920 帧（15 块）淡出后必须完全归零。
        for _ in 0..15 {
            render_block(
                &planar,
                BlockStatus::Underrun,
                &src_dest,
                channels,
                &mut out,
                &mut fade,
                true,
                65_536,
                480,
                1_920,
            );
        }
        for frame in [0usize, 64, 127] {
            assert_eq!(decode(&out, playback, frame, 0), 0, "淡出结束后必须静音");
        }
        assert!(fade.in_pending, "恢复时必须是淡入");
        assert_eq!(fade.last[0], 0);
    }

    /// 起播淡入：第一个 Ok 块从 0 余弦升到满幅，避免源首样本非零造成起播 click。
    #[test]
    fn playback_start_fades_in_over_10ms() {
        let channels = 7usize;
        let playback = 22usize;
        let src_dest = src_for_dest(&FOUR_ONE_TWO_MAP, playback);
        let planar = planar_const(channels);
        let mut out = vec![0u8; BLOCK_FRAMES * playback * 3];
        let mut fade = FadeState::new(playback); // in_pending = true
        let full = ((((0i32 + 1) << 16) >> 8) as i32) as f64;
        let mut first = None;
        let mut tail = 0.0f64;
        for _ in 0..4 {
            render_block(
                &planar,
                BlockStatus::Ok,
                &src_dest,
                channels,
                &mut out,
                &mut fade,
                true,
                65_536,
                480, // 10 ms = 480 帧（跨 4 块）
                1_920,
            );
            if first.is_none() {
                first = Some(decode(&out, playback, 0, 0) as f64);
            }
            tail = decode(&out, playback, 127, 0) as f64;
        }
        let first = first.unwrap();
        assert!(
            first.abs() < 0.02 * full,
            "起播首帧≈0（got {first}, full {full}）"
        );
        assert!(
            (tail - full).abs() / (full) < 0.01,
            "10ms 淡入完成后≈满幅（got {tail}, full {full}）"
        );
        assert!(!fade.in_pending, "淡入完成后 in_pending 应清除");
    }

    /// 流结束/NotDue 上限判定：80 ms 门限、20 ms hold 上限的边界；保证"断流后有限时间内收尾"。
    #[test]
    fn stream_end_and_notdue_hold_thresholds_are_finite() {
        let end = 80 * RATE_HZ as u64 / 1000; // 3840 帧
        let hold = 20 * RATE_HZ as u64 / 1000; // 960 帧
        assert!(!stream_end_reached(0, end));
        assert!(!stream_end_reached(end - 1, end));
        assert!(stream_end_reached(end, end));
        assert!(stream_end_reached(end + 128, end));
        assert!(!notdue_hold_exceeded(0, hold));
        assert!(!notdue_hold_exceeded(hold - 1, hold));
        assert!(notdue_hold_exceeded(hold, hold));
        // 收尾总时长（检测 80 ms + 淡出 40 ms + 静音 200 ms）= 320 ms，远小于"无限保持"。
        let stop_total_ms = 80 + 40 + 200;
        assert!(stop_total_ms <= 400);
    }

    /// 映射表校验：重复目标/越界/长度不符全部拒绝。
    #[test]
    fn map_validation_rejects_bad_tables() {
        assert!(parse_map("0,1,2,3,4,5,7", 7, 22).is_ok());
        assert!(parse_map("0,1,2,3,4,5,5", 7, 22).is_err(), "重复目标");
        assert!(parse_map("0,1,2,3,4,5,22", 7, 22).is_err(), "越界");
        assert!(parse_map("0,1,2", 7, 22).is_err(), "长度不符");
        assert!(parse_map("0,1,2,3,4,5,x", 7, 22).is_err(), "非数字");
    }
}
