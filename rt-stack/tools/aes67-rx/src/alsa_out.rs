//! ALSA S24_3LE 多通道阻塞写出（MOTU UltraLite-mk5 22ch / 48k）。
//!
//! - 只在这里碰 libasound；热路径为 `write_all`（预分配 buf，无格式化、无锁）。
//! - 欠载（EPIPE/ESTRPIPE）后 prepare 重开并计数，不 panic、不退出（静音不爆音由上层保证）。
//! - 22ch 中未映射通道写 0（静音）。

#[cfg(target_os = "linux")]
mod imp {
    use alsa::pcm::{Access, Format, HwParams, PCM, State};
    use alsa::{Direction, ValueOr};

    /// ALSA 播放句柄。
    pub struct AlsaOut {
        pcm: PCM,
        pub channels: usize,
        pub rate: u32,
        pub period_frames: usize,
        pub buffer_frames: usize,
        pub written_frames: u64,
        pub recoveries: u64,
        pub xruns: u64,
        pub first_write_ns: Option<u64>,
    }

    impl AlsaOut {
        /// 打开 hw 设备并配置 S24_3LE / 48k / channels / period / buffer。
        pub fn open(
            device: &str,
            channels: usize,
            rate: u32,
            period_frames: usize,
            buffer_frames: usize,
        ) -> Result<Self, String> {
            let pcm = PCM::new(device, Direction::Playback, false)
                .map_err(|e| format!("PCM::new({device}) 失败: {e}"))?;
            {
                let hwp = HwParams::any(&pcm).map_err(|e| format!("HwParams::any: {e}"))?;
                hwp.set_access(Access::RWInterleaved)
                    .map_err(|e| format!("set_access: {e}"))?;
                hwp.set_format(Format::S243LE)
                    .map_err(|e| format!("set_format(S24_3LE): {e}"))?;
                hwp.set_channels(channels as u32)
                    .map_err(|e| format!("set_channels({channels}): {e}"))?;
                hwp.set_rate(rate, ValueOr::Nearest)
                    .map_err(|e| format!("set_rate({rate}): {e}"))?;
                hwp.set_period_size_near(period_frames as alsa::pcm::Frames, ValueOr::Nearest)
                    .map_err(|e| format!("set_period_size_near({period_frames}): {e}"))?;
                hwp.set_buffer_size_near(buffer_frames as alsa::pcm::Frames)
                    .map_err(|e| format!("set_buffer_size_near({buffer_frames}): {e}"))?;
                pcm.hw_params(&hwp)
                    .map_err(|e| format!("hw_params: {e}"))?;
            }
            pcm.prepare().map_err(|e| format!("prepare: {e}"))?;
            Ok(Self {
                pcm,
                channels,
                rate,
                period_frames,
                buffer_frames,
                written_frames: 0,
                recoveries: 0,
                xruns: 0,
                first_write_ns: None,
            })
        }

        /// 退出前 drain（把已写入的帧播完再 close，避免关设备瞬间的台阶/爆音）。
        pub fn drain(&self) -> Result<(), String> {
            self.pcm.drain().map_err(|e| format!("drain: {e}"))
        }

        /// 阻塞写整块（len 必须是 frames×channels×3 的整数倍）。
        pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
            let bpf = self.channels * 3;
            if bytes.len() % bpf != 0 {
                return Err(format!("write_all 长度 {} 不是 {bpf} 的倍数", bytes.len()));
            }
            let mut offset = 0usize;
            while offset < bytes.len() {
                let io = self.pcm.io_bytes();
                match io.writei(&bytes[offset..]) {
                    Ok(0) => return Err("snd_pcm_writei 返回 0 帧".into()),
                    Ok(frames) => {
                        let consumed = frames * bpf;
                        offset += consumed;
                        self.written_frames += frames as u64;
                        if self.first_write_ns.is_none() {
                            self.first_write_ns = Some(crate::now_unix_ns());
                        }
                    }
                    Err(error) => {
                        // 欠载/挂起：prepare 重开继续；计数用于证据，不 panic。
                        let state = self.pcm.state();
                        if matches!(state, State::XRun) {
                            self.xruns += 1;
                        }
                        self.recoveries += 1;
                        eprintln!(
                            "[aes67-rx] ALSA recover #{} state={:?} errno={} written={}",
                            self.recoveries,
                            state,
                            error.errno(),
                            self.written_frames
                        );
                        let _ = self.pcm.prepare();
                        if self.recoveries > 4096 {
                            return Err(format!("ALSA 连续恢复失败过多: {error}"));
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::*;

#[cfg(not(target_os = "linux"))]
mod imp {
    /// 非 Linux 平台 stub：只用于 `cargo test --workspace` 跨平台编译；
    /// 真实接收/播放只在 Linux 板端（ALSA S24_3LE 22ch）。
    pub struct AlsaOut {
        pub channels: usize,
        pub rate: u32,
        pub period_frames: usize,
        pub buffer_frames: usize,
        pub written_frames: u64,
        pub recoveries: u64,
        pub xruns: u64,
        pub first_write_ns: Option<u64>,
    }

    impl AlsaOut {
        pub fn open(
            _device: &str,
            _channels: usize,
            _rate: u32,
            _period_frames: usize,
            _buffer_frames: usize,
        ) -> Result<Self, String> {
            Err("ALSA 输出只在 Linux 板端构建（非 Linux stub）".to_string())
        }

        pub fn write_all(&mut self, _bytes: &[u8]) -> Result<(), String> {
            Err("ALSA 输出只在 Linux 板端构建（非 Linux stub）".to_string())
        }

        pub fn drain(&self) -> Result<(), String> {
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub use imp::*;

