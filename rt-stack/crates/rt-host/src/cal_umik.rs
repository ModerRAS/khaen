//! Linux 上由 host 打开 UMIK。只在校准模式的非实时线程里使用。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

static STOP: AtomicBool = AtomicBool::new(false);
static HOLD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

pub fn open_session() -> Result<(), String> {
    close_session();
    STOP.store(false, Ordering::Release);
    let handle = std::thread::Builder::new()
        .name("khaen-umik".to_string())
        .spawn(session)
        .map_err(|error| format!("UMIK 线程启动失败：{error}"))?;
    if let Ok(mut slot) = HOLD.lock() {
        *slot = Some(handle);
    }
    for _ in 0..50 {
        if OPENED.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(error) = take_error() {
            close_session();
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    close_session();
    Err("打开 UMIK hw:0,0 超时".to_string())
}

pub fn close_session() {
    STOP.store(true, Ordering::Release);
    OPENED.store(false, Ordering::Release);
    if let Ok(mut slot) = HOLD.lock() {
        if let Some(handle) = slot.take() {
            let _ = handle.join();
        }
    }
}

static OPENED: AtomicBool = AtomicBool::new(false);
static ERROR: Mutex<Option<String>> = Mutex::new(None);

fn take_error() -> Option<String> {
    ERROR.lock().ok().and_then(|mut slot| slot.take())
}

fn session() {
    if let Err(error) = run_capture() {
        if let Ok(mut slot) = ERROR.lock() {
            *slot = Some(error);
        }
    }
    OPENED.store(false, Ordering::Release);
}

fn run_capture() -> Result<(), String> {
    use alsa::pcm::{Access, Format, HwParams, PCM};
    use alsa::ValueOr;
    let pcm = PCM::new("hw:0,0", alsa::Direction::Capture, false)
        .map_err(|error| format!("打开 UMIK hw:0,0 失败：{error}"))?;
    {
        let params = HwParams::any(&pcm).map_err(|error| format!("UMIK hwparams 失败：{error}"))?;
        params
            .set_access(Access::RWInterleaved)
            .map_err(|error| format!("UMIK access 失败：{error}"))?;
        params
            .set_format(Format::S243LE)
            .map_err(|error| format!("UMIK S24_3LE 失败：{error}"))?;
        params
            .set_channels(2)
            .map_err(|error| format!("UMIK 通道数失败：{error}"))?;
        params
            .set_rate(48_000, ValueOr::Nearest)
            .map_err(|error| format!("UMIK 采样率失败：{error}"))?;
        params
            .set_period_size(128, ValueOr::Nearest)
            .map_err(|error| format!("UMIK 周期失败：{error}"))?;
        params
            .set_buffer_size(512)
            .map_err(|error| format!("UMIK 缓冲失败：{error}"))?;
        pcm.hw_params(&params)
            .map_err(|error| format!("UMIK 提交 hwparams 失败：{error}"))?;
    }
    pcm.start()
        .map_err(|error| format!("UMIK start 失败：{error}"))?;
    OPENED.store(true, Ordering::Release);
    let io = pcm.io_bytes();
    let mut buf = vec![0u8; 128 * 2 * 3];
    while !STOP.load(Ordering::Acquire) {
        match io.readi(&mut buf) {
            Ok(frames) => {
                let frames = frames as usize;
                let mut mono = Vec::with_capacity(frames);
                for index in 0..frames {
                    let byte = index * 6;
                    if byte + 3 > buf.len() {
                        break;
                    }
                    mono.push(s24(&buf[byte..byte + 3]));
                }
                super::shared().push_mic(&mono);
            }
            Err(error) => {
                if STOP.load(Ordering::Acquire) {
                    break;
                }
                return Err(format!("UMIK 读取失败：{error}"));
            }
        }
    }
    Ok(())
}

fn s24(bytes: &[u8]) -> f64 {
    let mut value = bytes[0] as i32 | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16);
    if value & 0x0080_0000 != 0 {
        value |= !0x00FF_FFFF;
    }
    f64::from(value) / 8_388_608.0
}
