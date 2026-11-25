//! 诊断用有界音频 dump（默认关闭；环形分段；写失败绝不影响播放）。
//!
//! 规则（2026-11-21 lead）：
//! - 默认不 dump，只有显式 --dump-dir / --dump-wav 才启用；
//! - 硬限额：总时长 ≤ max_seconds（默认 30 s）且总字节 ≤ max_bytes（默认 50 MB），先到为准；
//! - 环形覆盖：固定 N 段（默认 5 s/段 × 3 段 ≈ 47.5 MB，覆盖最近 ~15 s），旧段自动截断重写；
//! - 启动空间检查：目录 fs 剩余 < 1 GB 拒绝启用；
//! - 故障隔离：写/seek 失败只停 dump + 计数 + 告警，播放继续。

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub struct DumpRing {
    dir: PathBuf,
    files: Vec<Option<File>>,
    seg_bytes: Vec<u64>,
    seg_frames: u64,
    max_frames: u64,
    max_bytes: u64,
    channels: u16,
    rate: u32,
    cur: usize,
    cur_frames: u64,
    total_frames: u64,
    total_bytes: u64,
    pub write_errors: u64,
    pub truncated: bool,
    pub failed: bool,
    pub last_error: Option<String>,
}

impl DumpRing {
    pub fn segment_frames(seg_seconds: u32, rate: u32) -> u64 {
        (seg_seconds.max(1) as u64 * rate as u64).max(1)
    }

    /// 目录剩余空间（字节）；非 Linux 返回 u64::MAX。
    pub fn free_bytes(dir: &Path) -> u64 {
        #[cfg(target_os = "linux")]
        unsafe {
            let cpath = match std::ffi::CString::new(dir.to_string_lossy().as_bytes()) {
                Ok(c) => c,
                Err(_) => return 0,
            };
            let mut st: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(cpath.as_ptr(), &mut st) != 0 {
                return 0;
            }
            st.f_bavail as u64 * st.f_frsize as u64
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = dir;
            u64::MAX
        }
    }

    pub fn new(
        dir: &Path,
        channels: u16,
        rate: u32,
        seg_seconds: u32,
        max_segments: usize,
        max_seconds: u32,
        max_bytes: u64,
    ) -> Result<Self, String> {
        if max_segments == 0 || max_segments > 16 {
            return Err(format!("dump 段数非法: {max_segments}"));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 dump 目录失败: {e}"))?;
        let seg_frames = Self::segment_frames(seg_seconds, rate);
        let mut files = Vec::with_capacity(max_segments);
        let mut seg_bytes = vec![0u64; max_segments];
        for index in 0..max_segments {
            let path = dir.join(format!("seg{index}.wav"));
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .map_err(|e| format!("创建 dump 段 {path:?} 失败: {e}"))?;
            write_header(&mut file, channels, rate, 0)?;
            files.push(Some(file));
        }
        seg_bytes[0] = 0;
        Ok(Self {
            dir: dir.to_path_buf(),
            files,
            seg_bytes,
            seg_frames,
            max_frames: max_seconds.max(1) as u64 * rate as u64,
            max_bytes: max_bytes.max(1),
            channels,
            rate,
            cur: 0,
            cur_frames: 0,
            total_frames: 0,
            total_bytes: 0,
            write_errors: 0,
            truncated: false,
            failed: false,
            last_error: None,
        })
    }

    fn bytes_per_frame(&self) -> u64 {
        self.channels as u64 * 3
    }

    /// 写一块渲染后的 22ch S24_3LE 数据。返回 false 表示 dump 已停（播放不受影响）。
    pub fn write_block(&mut self, bytes: &[u8], frames: usize) -> bool {
        if self.failed {
            return false;
        }
        if self.total_frames >= self.max_frames || self.total_bytes >= self.max_bytes {
            self.truncated = true;
            self.finish();
            return false;
        }
        let expected = frames * self.bytes_per_frame() as usize;
        if bytes.len() < expected || frames == 0 {
            return false;
        }
        if self.cur_frames >= self.seg_frames && !self.rotate() {
            return false;
        }
        let cur = self.cur;
        let file = match self.files[cur].as_mut() {
            Some(f) => f,
            None => {
                self.fail("dump 段句柄缺失");
                return false;
            }
        };
        let offset = 44 + self.seg_bytes[cur];
        if let Err(e) = file.seek(SeekFrom::Start(offset)) {
            self.fail(&format!("dump seek 失败: {e}"));
            return false;
        }
        if let Err(e) = file.write_all(&bytes[..expected]) {
            self.fail(&format!("dump write 失败: {e}"));
            return false;
        }
        self.seg_bytes[cur] += expected as u64;
        self.cur_frames += frames as u64;
        self.total_frames += frames as u64;
        self.total_bytes += expected as u64;
        if self.total_frames >= self.max_frames || self.total_bytes >= self.max_bytes {
            self.truncated = true;
            self.finish();
            return false;
        }
        true
    }

    fn rotate(&mut self) -> bool {
        let cur = self.cur;
        if let Some(file) = self.files[cur].as_mut() {
            let data = self.seg_bytes[cur] as u32;
            if let Err(e) = write_header(file, self.channels, self.rate, data) {
                self.fail(&format!("dump 段头回填失败: {e}"));
                return false;
            }
            // 注意：RT 播放线程里绝不能 fsync（会阻塞数百 ms → ALSA xrun）。
            // 头回填已通过 write+seek 完成，脏页由内核异步回写。
        }
        self.cur = (self.cur + 1) % self.files.len();
        self.cur_frames = 0;
        let cur = self.cur;
        self.seg_bytes[cur] = 0;
        let file = match self.files[cur].as_mut() {
            Some(f) => f,
            None => {
                self.fail("dump 段句柄缺失");
                return false;
            }
        };
        if let Err(e) = file.set_len(44) {
            self.fail(&format!("dump 段截断失败: {e}"));
            return false;
        }
        if let Err(e) = write_header(file, self.channels, self.rate, 0) {
            self.fail(&format!("dump 段头重写失败: {e}"));
            return false;
        }
        true
    }

    fn fail(&mut self, message: &str) {
        self.write_errors += 1;
        self.last_error = Some(message.to_string());
        self.failed = true;
        eprintln!("[aes67-rx] dump 已停用（不影响播放）: {message}");
        let _ = self.finish();
    }

    /// 收尾：回填各段头实际大小并关闭。幂等。
    pub fn finish(&mut self) {
        for index in 0..self.files.len() {
            if let Some(file) = self.files[index].as_mut() {
                let data = self.seg_bytes[index] as u32;
                let _ = write_header(file, self.channels, self.rate, data);
                let _ = file.sync_data();
            }
            self.files[index] = None;
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

fn write_header(file: &mut File, channels: u16, rate: u32, data_bytes: u32) -> Result<(), String> {
    let byte_rate = rate * channels as u32 * 3;
    let block_align = channels * 3;
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&rate.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&24u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(&header).map_err(|e| e.to_string())
}
