//! S24_3LE 裸流读写与 aplay/arecord 测量封装。
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

pub fn read_s24(path: &Path, channels: usize) -> Result<Vec<Vec<f64>>, String> {
    let data = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bytes_per_frame = channels * 3;
    if bytes_per_frame == 0 {
        return Err("channels=0".into());
    }
    let frames = data.len() / bytes_per_frame;
    let mut out = vec![Vec::with_capacity(frames); channels];
    for f in 0..frames {
        let base = f * bytes_per_frame;
        for c in 0..channels {
            let o = base + c * 3;
            let raw = (data[o] as i32) | ((data[o + 1] as i32) << 8) | ((data[o + 2] as i32) << 16);
            let s = if raw & 0x800000 != 0 {
                raw | !0xFFFFFF
            } else {
                raw
            };
            out[c].push(s as f64 / 8_388_608.0);
        }
    }
    Ok(out)
}

pub fn write_s24(path: &Path, channels: Vec<Vec<f64>>) -> Result<(), String> {
    if channels.is_empty() {
        return Err("no channels".into());
    }
    let frames = channels[0].len();
    if channels.iter().any(|c| c.len() != frames) {
        return Err("channel length mismatch".into());
    }
    let mut data = Vec::with_capacity(frames * channels.len() * 3);
    for f in 0..frames {
        for ch in &channels {
            let v = (ch[f].clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
            data.push((v & 0xFF) as u8);
            data.push(((v >> 8) & 0xFF) as u8);
            data.push(((v >> 16) & 0xFF) as u8);
        }
    }
    fs::write(path, data).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn write_s24_single(path: &Path, samples: &[f64]) -> Result<(), String> {
    write_s24(path, vec![samples.to_vec()])
}

pub fn mono_mix(channels: &[Vec<f64>]) -> Vec<f64> {
    if channels.is_empty() {
        return Vec::new();
    }
    let n = channels[0].len();
    let mut out = vec![0.0; n];
    for ch in channels {
        for i in 0..n.min(ch.len()) {
            out[i] += ch[i];
        }
    }
    let g = 1.0 / channels.len() as f64;
    for v in &mut out {
        *v *= g;
    }
    out
}

/// 生成多声道 sweep 裸流：只有 target 通道有信号，其余静音；前后加静音保护。
pub fn write_sweep_file(
    path: &Path,
    playback_channels: usize,
    target: usize,
    sweep: &[f64],
    rate: u32,
    pre_seconds: f64,
    post_seconds: f64,
) -> Result<(), String> {
    if target >= playback_channels {
        return Err(format!(
            "target {target} >= playback channels {playback_channels}"
        ));
    }
    let pre = (pre_seconds * rate as f64).round() as usize;
    let post = (post_seconds * rate as f64).round() as usize;
    let total = pre + sweep.len() + post;
    let mut chans = vec![vec![0.0f64; total]; playback_channels];
    chans[target][pre..pre + sweep.len()].copy_from_slice(sweep);
    write_s24(path, chans)
}

pub fn run_checked(mut cmd: Command, label: &str) -> Result<(), String> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let out = cmd.output().map_err(|e| format!("spawn {label}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{label} 失败 ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// 单通道测量：先起 arecord，再 aplay sweep；两者都用 hw: 直连。
pub fn measure_channel(
    playback_device: &str,
    playback_channels: usize,
    capture_device: &str,
    capture_channels: usize,
    rate: u32,
    capture_seconds: f64,
    sweep_file: &Path,
    capture_file: &Path,
    period_size: usize,
    buffer_size: usize,
) -> Result<(), String> {
    let _ = fs::remove_file(capture_file);
    let mut arec = Command::new("arecord");
    arec.args([
        "-q",
        "-D",
        capture_device,
        "-t",
        "raw",
        "-c",
        &capture_channels.to_string(),
        "-r",
        &rate.to_string(),
        "-f",
        "S24_3LE",
        "-d",
        &capture_seconds.ceil().to_string(),
        &format!("--period-size={period_size}"),
        &format!("--buffer-size={buffer_size}"),
    ]);
    arec.arg(capture_file);
    let mut child = arec.spawn().map_err(|e| format!("spawn arecord: {e}"))?;
    sleep(Duration::from_millis(350));

    let mut aplay = Command::new("aplay");
    aplay.args([
        "-q",
        "-D",
        playback_device,
        "-t",
        "raw",
        "-c",
        &playback_channels.to_string(),
        "-r",
        &rate.to_string(),
        "-f",
        "S24_3LE",
        &format!("--period-size={period_size}"),
        &format!("--buffer-size={buffer_size}"),
    ]);
    aplay.arg(sweep_file);
    let status = aplay.status().map_err(|e| format!("spawn aplay: {e}"))?;
    if !status.success() {
        return Err(format!("aplay 失败: {status}"));
    }

    sleep(Duration::from_millis(200));
    let status = child.wait().map_err(|e| format!("wait arecord: {e}"))?;
    if !status.success() {
        return Err(format!("arecord 失败: {status}"));
    }
    Ok(())
}

fn spawn_arecord(
    device: &str,
    channels: usize,
    rate: u32,
    seconds: f64,
    period_size: usize,
    buffer_size: usize,
    capture_file: &Path,
) -> Result<std::process::Child, String> {
    let _ = fs::remove_file(capture_file);
    let mut arec = Command::new("arecord");
    arec.args([
        "-q",
        "-D",
        device,
        "-t",
        "raw",
        "-c",
        &channels.to_string(),
        "-r",
        &rate.to_string(),
        "-f",
        "S24_3LE",
        "-d",
        &seconds.ceil().to_string(),
        &format!("--period-size={period_size}"),
        &format!("--buffer-size={buffer_size}"),
    ]);
    arec.arg(capture_file);
    arec.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    arec.spawn()
        .map_err(|e| format!("spawn arecord {device}: {e}"))
}

/// 两路 arecord 同时起，等 `settle_ms` 再 aplay，两路都结束后返回。
/// 双设备时 settle 默认 1000 ms，给两路 arecord 留出稳定时间。
pub fn measure_channel_dual(
    playback_device: &str,
    playback_channels: usize,
    capture_device: &str,
    capture_channels: usize,
    capture2_device: &str,
    capture2_channels: usize,
    rate: u32,
    capture_seconds: f64,
    sweep_file: &Path,
    capture_file: &Path,
    capture2_file: &Path,
    period_size: usize,
    buffer_size: usize,
    settle_ms: u64,
) -> Result<(), String> {
    let mut first = spawn_arecord(
        capture_device,
        capture_channels,
        rate,
        capture_seconds,
        period_size,
        buffer_size,
        capture_file,
    )?;
    let second = spawn_arecord(
        capture2_device,
        capture2_channels,
        rate,
        capture_seconds,
        period_size,
        buffer_size,
        capture2_file,
    );
    let mut second = match second {
        Ok(c) => c,
        Err(e) => {
            let _ = first.kill();
            let _ = first.wait();
            return Err(e);
        }
    };
    sleep(Duration::from_millis(settle_ms.max(1)));
    let mut aplay = Command::new("aplay");
    aplay.args([
        "-q",
        "-D",
        playback_device,
        "-t",
        "raw",
        "-c",
        &playback_channels.to_string(),
        "-r",
        &rate.to_string(),
        "-f",
        "S24_3LE",
        &format!("--period-size={period_size}"),
        &format!("--buffer-size={buffer_size}"),
    ]);
    aplay.arg(sweep_file);
    let status = aplay.status();
    if status.as_ref().map(|s| !s.success()).unwrap_or(true) {
        let _ = first.kill();
        let _ = second.kill();
        let _ = first.wait();
        let _ = second.wait();
        return Err(format!(
            "aplay 失败: {}",
            status
                .map(|s| s.to_string())
                .unwrap_or_else(|e| e.to_string())
        ));
    }
    sleep(Duration::from_millis(200));
    let s1 = first.wait().map_err(|e| format!("wait arecord: {e}"))?;
    let s2 = second.wait().map_err(|e| format!("wait arecord2: {e}"))?;
    if !s1.success() {
        return Err(format!("arecord 失败: {s1}"));
    }
    if !s2.success() {
        return Err(format!("arecord2 失败: {s2}"));
    }
    Ok(())
}

/// POST /api/motu/direct-mute。只支持 http://host:port（板端局域网，header token）。
pub fn cuemix_direct_mute(base: &str, token: &str, mute: bool) -> Result<(), String> {
    use std::io::{Read, Write};
    let base = base.trim().trim_end_matches('/');
    let rest = base
        .strip_prefix("http://")
        .ok_or("cuemix-guard 只支持 http://host:port")?;
    if rest.contains('/') {
        return Err("cuemix-guard 基址不要带路径".into());
    }
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("端口非法 {p}"))?),
        None => (rest, 80u16),
    };
    if host.is_empty() {
        return Err("cuemix-guard 缺少主机名".into());
    }
    let body = format!(
        r#"{{"mute":{},"confirm":true}}"#,
        if mute { "true" } else { "false" }
    );
    let req = format!(
        "POST /api/motu/direct-mute HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Bearer {token}\r\nX-Auth-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = std::net::TcpStream::connect((host, port))
        .map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(8))).ok();
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("写入 direct-mute 失败: {e}"))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取 direct-mute 失败: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let status = text.split_whitespace().nth(1).unwrap_or("");
    if !status.starts_with('2') {
        let snippet: String = text.chars().take(240).collect();
        return Err(format!("direct-mute mute={mute} HTTP {status}: {snippet}"));
    }
    Ok(())
}
