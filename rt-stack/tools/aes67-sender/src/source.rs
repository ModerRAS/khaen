//! Audio sources: synthetic sine, WAV file, raw PCM file.
//! (WASAPI loopback sources live in `wasapi.rs`, Windows only.)

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::pcm::{i16_to_f32, l24_to_f32};
use crate::resample::LinearResampler;
use crate::wav::WavReader;
use crate::AES67_RATE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadStatus {
    pub requested_frames: usize,
    pub produced_frames: usize,
    pub underrun_frames: usize,
    pub eof: bool,
}

pub trait Source {
    fn channels(&self) -> usize;
    fn description(&self) -> String;
    /// Fill `out[..frames * channels]` with interleaved f32 @ 48 kHz.
    /// Missing frames must be zero-filled and reported via ReadStatus.
    fn read_into(&mut self, out: &mut [f32], frames: usize) -> Result<ReadStatus>;
}

// ---------------------------------------------------------------------------
// Sine generator
// ---------------------------------------------------------------------------

pub struct SineSource {
    channels: usize,
    freqs: Vec<f64>,
    amp: f64,
    frame_index: u64,
    total_frames: Option<u64>,
}

impl SineSource {
    pub fn new(
        channels: usize,
        freq_l: f64,
        freq_r: f64,
        amp: f64,
        duration_s: Option<f64>,
    ) -> Result<Self> {
        anyhow::ensure!((1..=8).contains(&channels), "channels must be 1..=8");
        anyhow::ensure!(amp > 0.0 && amp <= 1.0, "amp must be in (0, 1]");
        let mut freqs = Vec::with_capacity(channels);
        for c in 0..channels {
            let f = match c {
                0 => freq_l,
                1 => freq_r,
                _ => freq_l,
            };
            anyhow::ensure!(
                f > 0.0 && f < AES67_RATE as f64 / 2.0,
                "tone frequency {f} must be in (0, 24000)"
            );
            freqs.push(f);
        }
        if let Some(d) = duration_s {
            anyhow::ensure!(d > 0.0, "duration must be > 0");
        }
        let total_frames = duration_s.map(|d| (d * AES67_RATE as f64).round() as u64);
        Ok(Self {
            channels,
            freqs,
            amp,
            frame_index: 0,
            total_frames,
        })
    }
}

impl Source for SineSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn description(&self) -> String {
        format!(
            "sine channels={} freqs={:?} amp={} frames={:?}",
            self.channels, self.freqs, self.amp, self.total_frames
        )
    }

    fn read_into(&mut self, out: &mut [f32], frames: usize) -> Result<ReadStatus> {
        let ch = self.channels;
        let mut produced = 0usize;
        let mut eof = false;
        for f in 0..frames {
            if let Some(total) = self.total_frames {
                if self.frame_index >= total {
                    eof = true;
                    break;
                }
            }
            let t = self.frame_index as f64 / AES67_RATE as f64;
            for c in 0..ch {
                let phase = 2.0 * std::f64::consts::PI * self.freqs[c] * t;
                out[f * ch + c] = (phase.sin() * self.amp) as f32;
            }
            self.frame_index += 1;
            produced += 1;
        }
        for v in out
            .iter_mut()
            .skip(produced * ch)
            .take((frames - produced) * ch)
        {
            *v = 0.0;
        }
        Ok(ReadStatus {
            requested_frames: frames,
            produced_frames: produced,
            underrun_frames: frames - produced,
            eof,
        })
    }
}

// ---------------------------------------------------------------------------
// WAV file source (streaming, optional linear resample to 48 kHz, optional loop)
// ---------------------------------------------------------------------------

const FILE_CHUNK_FRAMES: usize = 1024;

pub struct WavFileSource {
    reader: WavReader,
    channels: usize,
    resampler: Option<LinearResampler>,
    pending: Vec<f32>,
    loop_file: bool,
    eof: bool,
    frames_emitted: u64,
}

impl WavFileSource {
    pub fn open<P: AsRef<Path>>(path: P, loop_file: bool) -> Result<Self> {
        let reader = WavReader::open(path)?;
        let channels = reader.spec.channels as usize;
        anyhow::ensure!(
            (1..=8).contains(&channels),
            "WAV has {channels} channels; AES67 flow supports 1..=8"
        );
        anyhow::ensure!(reader.total_frames() > 0, "WAV data chunk has zero frames");
        if reader.spec.sample_rate != AES67_RATE {
            eprintln!(
                "note: WAV is {} Hz; using built-in linear resampler to {} Hz (prototype quality)",
                reader.spec.sample_rate, AES67_RATE
            );
        }
        let resampler = if reader.spec.sample_rate != AES67_RATE {
            Some(LinearResampler::new(
                reader.spec.sample_rate,
                AES67_RATE,
                channels,
            )?)
        } else {
            None
        };
        Ok(Self {
            reader,
            channels,
            resampler,
            pending: Vec::new(),
            loop_file,
            eof: false,
            frames_emitted: 0,
        })
    }

    fn refill(&mut self) -> Result<()> {
        while self.pending.len() < FILE_CHUNK_FRAMES * self.channels && !self.eof {
            let (chunk, eof) = self
                .reader
                .read_frames(FILE_CHUNK_FRAMES)
                .context("WAV read failed")?;
            if let Some(rs) = self.resampler.as_mut() {
                self.pending.extend(rs.process(&chunk));
            } else {
                self.pending.extend(chunk);
            }
            if eof {
                if self.loop_file {
                    self.reader.rewind();
                } else {
                    self.eof = true;
                }
            }
        }
        Ok(())
    }
}

impl Source for WavFileSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn description(&self) -> String {
        format!(
            "wav channels={} format={} rate={} pending={} loop={}",
            self.channels,
            self.reader.spec.format.name(),
            self.reader.spec.sample_rate,
            self.pending.len() / self.channels,
            self.loop_file
        )
    }

    fn read_into(&mut self, out: &mut [f32], frames: usize) -> Result<ReadStatus> {
        let ch = self.channels;
        let need = frames * ch;
        self.refill()?;
        let take = need.min(self.pending.len());
        out[..take].copy_from_slice(&self.pending[..take]);
        self.pending.drain(..take);
        for v in out.iter_mut().skip(take).take(need - take) {
            *v = 0.0;
        }
        let produced = take / ch;
        self.frames_emitted += produced as u64;
        Ok(ReadStatus {
            requested_frames: frames,
            produced_frames: produced,
            underrun_frames: frames - produced,
            eof: self.eof && produced == 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Raw PCM file source (48 kHz, interleaved; little-endian file bytes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFormat {
    L16Le,
    L24Le,
    L32Le,
    F32Le,
}

impl RawFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "l16" | "l16le" | "s16" | "s16le" => Ok(Self::L16Le),
            "l24" | "l24le" | "s24" | "s24le" => Ok(Self::L24Le),
            "l32" | "l32le" | "s32" | "s32le" => Ok(Self::L32Le),
            "f32" | "f32le" | "float" => Ok(Self::F32Le),
            other => bail!("unsupported raw format {other:?} (l16/l24/l32/f32, little-endian)"),
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            RawFormat::L16Le => 2,
            RawFormat::L24Le => 3,
            RawFormat::L32Le | RawFormat::F32Le => 4,
        }
    }
}

pub struct RawFileSource {
    data: Vec<u8>,
    channels: usize,
    format: RawFormat,
    cursor: usize,
    loop_file: bool,
}

impl RawFileSource {
    pub fn open<P: AsRef<Path>>(
        path: P,
        channels: usize,
        format: RawFormat,
        loop_file: bool,
    ) -> Result<Self> {
        anyhow::ensure!((1..=8).contains(&channels), "channels must be 1..=8");
        let data = std::fs::read(path.as_ref())
            .with_context(|| format!("failed to read raw PCM {:?}", path.as_ref()))?;
        let frame_bytes = channels * format.bytes();
        anyhow::ensure!(
            data.len() >= frame_bytes,
            "raw PCM file too short ({} bytes, {frame_bytes} B/frame)",
            data.len()
        );
        anyhow::ensure!(
            data.len() % frame_bytes == 0,
            "raw PCM size {} is not a multiple of {} B/frame",
            data.len(),
            frame_bytes
        );
        Ok(Self {
            data,
            channels,
            format,
            cursor: 0,
            loop_file,
        })
    }
}

impl Source for RawFileSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn description(&self) -> String {
        format!(
            "raw channels={} format={:?} frames={} loop={}",
            self.channels,
            self.format,
            self.data.len() / (self.channels * self.format.bytes()),
            self.loop_file
        )
    }

    fn read_into(&mut self, out: &mut [f32], frames: usize) -> Result<ReadStatus> {
        let ch = self.channels;
        let fb = ch * self.format.bytes();
        let need = frames * ch;
        let avail = self.data.len().saturating_sub(self.cursor) / fb;
        let n = frames.min(avail);
        let mut produced = 0usize;
        for f in 0..n {
            for c in 0..ch {
                let off = self.cursor + f * fb + c * self.format.bytes();
                let v = match self.format {
                    RawFormat::L16Le => {
                        i16_to_f32(i16::from_le_bytes([self.data[off], self.data[off + 1]]))
                    }
                    RawFormat::L24Le => {
                        let u = (self.data[off] as u32)
                            | ((self.data[off + 1] as u32) << 8)
                            | ((self.data[off + 2] as u32) << 16);
                        let v = if u & 0x0080_0000 != 0 {
                            (u | 0xFF00_0000) as i32
                        } else {
                            u as i32
                        };
                        l24_to_f32(v)
                    }
                    RawFormat::L32Le => {
                        i32::from_le_bytes([
                            self.data[off],
                            self.data[off + 1],
                            self.data[off + 2],
                            self.data[off + 3],
                        ]) as f32
                            / 2_147_483_648.0
                    }
                    RawFormat::F32Le => f32::from_le_bytes([
                        self.data[off],
                        self.data[off + 1],
                        self.data[off + 2],
                        self.data[off + 3],
                    ]),
                };
                out[f * ch + c] = v;
            }
            produced += 1;
        }
        self.cursor += produced * fb;
        let eof = self.cursor >= self.data.len();
        if eof && self.loop_file {
            self.cursor = 0;
        }
        for v in out
            .iter_mut()
            .skip(produced * ch)
            .take(need - produced * ch)
        {
            *v = 0.0;
        }
        Ok(ReadStatus {
            requested_frames: frames,
            produced_frames: produced,
            underrun_frames: frames - produced,
            eof: eof && !self.loop_file && produced == 0,
        })
    }
}

/// Open a WAV file, rejecting unsupported formats early (used by CLI validation).
pub fn describe_wav(path: &Path) -> Result<String> {
    let r = WavReader::open(path)?;
    Ok(format!(
        "channels={} rate={} format={} frames={} size={}",
        r.spec.channels,
        r.spec.sample_rate,
        r.spec.format.name(),
        r.total_frames(),
        r.file_size()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::{write_wav_file, WavWriteSpec};

    #[test]
    fn sine_source_respects_duration() {
        let mut s = SineSource::new(2, 1000.0, 2000.0, 0.5, Some(0.001)).unwrap();
        let mut buf = vec![0.0f32; 48 * 2];
        let st = s.read_into(&mut buf, 48).unwrap();
        assert_eq!(st.produced_frames, 48);
        assert!(!st.eof);
        let st = s.read_into(&mut buf, 48).unwrap();
        assert_eq!(st.produced_frames, 0);
        assert!(st.eof);
        assert_eq!(st.underrun_frames, 48);
    }

    #[test]
    fn sine_left_and_right_differ() {
        let mut s = SineSource::new(2, 1000.0, 2000.0, 0.5, None).unwrap();
        let mut buf = vec![0.0f32; 480 * 2];
        s.read_into(&mut buf, 480).unwrap();
        let left: Vec<f32> = buf.chunks(2).map(|c| c[0]).collect();
        let right: Vec<f32> = buf.chunks(2).map(|c| c[1]).collect();
        let fl = crate::analysis::estimate_frequency(&left, 48_000.0);
        let fr = crate::analysis::estimate_frequency(&right, 48_000.0);
        assert!((fl - 1000.0).abs() < 20.0, "left freq {fl}");
        assert!((fr - 2000.0).abs() < 40.0, "right freq {fr}");
    }

    /// Test scratch dir: %TEMP% may point at the RAM disk R:\Temp which the
    /// cargo-test child can be denied write access to; use the crate target dir.
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn wav_source_streams_and_loops() {
        let dir = test_dir("wav-source");
        let path = dir.join("tone.wav");
        let spec = WavWriteSpec::l24_48k(2);
        let n = 240usize;
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = (i as f32 / 240.0 * 2.0 * std::f32::consts::PI).sin();
            samples.push(v);
            samples.push(v);
        }
        write_wav_file(&path, spec, &samples).unwrap();

        let mut src = WavFileSource::open(&path, false).unwrap();
        assert_eq!(src.channels(), 2);
        let mut buf = vec![0.0f32; 100 * 2];
        let st = src.read_into(&mut buf, 100).unwrap();
        assert_eq!(st.produced_frames, 100);
        let mut total = 100u64;
        loop {
            let st = src.read_into(&mut buf, 100).unwrap();
            total += st.produced_frames as u64;
            if st.eof {
                break;
            }
        }
        assert_eq!(total, n as u64);

        let mut looped = WavFileSource::open(&path, true).unwrap();
        let mut buf = vec![0.0f32; 100 * 2];
        let mut total = 0u64;
        for _ in 0..5 {
            let st = looped.read_into(&mut buf, 100).unwrap();
            total += st.produced_frames as u64;
        }
        assert_eq!(total, 500);
    }

    #[test]
    fn raw_source_reads_l24() {
        let dir = test_dir("raw-source");
        let mut data = Vec::new();
        for i in 0..100 {
            let v = ((i as f32 / 100.0) * 8_000_000.0) as i32;
            let u = (v & 0x00FF_FFFF) as u32;
            data.push(u as u8);
            data.push((u >> 8) as u8);
            data.push((u >> 16) as u8);
            data.extend_from_slice(&[0, 0, 0]); // second channel zero
        }
        let path = dir.join("tone.raw");
        std::fs::write(&path, &data).unwrap();
        let mut src = RawFileSource::open(&path, 2, RawFormat::L24Le, false).unwrap();
        let mut buf = vec![0.0f32; 50 * 2];
        let st = src.read_into(&mut buf, 50).unwrap();
        assert_eq!(st.produced_frames, 50);
        assert!(buf[0] >= 0.0);
        assert_eq!(buf[1], 0.0);
    }
}
