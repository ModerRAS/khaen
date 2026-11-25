//! Streaming WAV reader/writer (PCM 8/16/24/32 + IEEE float 32/64).
//!
//! The reader keeps the file bytes in memory and decodes frame-by-frame, so it
//! can feed the RTP pacer without whole-file f32 expansion. The writer buffers
//! the produced WAV (used by the self-check receiver and the tone generator).

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::pcm;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    U8,
    I16,
    I24,
    I32,
    F32,
    F64,
}

impl SampleFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::I32 | SampleFormat::F32 => 4,
            SampleFormat::F64 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::U8 => "u8",
            SampleFormat::I16 => "pcm_s16le",
            SampleFormat::I24 => "pcm_s24le",
            SampleFormat::I32 => "pcm_s32le",
            SampleFormat::F32 => "f32le",
            SampleFormat::F64 => "f64le",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavSpec {
    pub channels: u16,
    pub sample_rate: u32,
    pub format: SampleFormat,
}

impl WavSpec {
    pub fn block_align(&self) -> u16 {
        self.channels as u16 * self.format.bytes_per_sample() as u16
    }
}

pub struct WavReader {
    pub spec: WavSpec,
    data: Vec<u8>,
    data_offset: usize,
    data_len: usize,
    cursor: usize,
    file_size: u64,
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn le_i24(b: &[u8]) -> i32 {
    let u = (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16);
    if u & 0x0080_0000 != 0 {
        (u | 0xFF00_0000) as i32
    } else {
        u as i32
    }
}

impl WavReader {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let bytes = fs::read(path.as_ref())
            .with_context(|| format!("failed to read WAV {:?}", path.as_ref()))?;
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < 12 {
            bail!("WAV too short ({} bytes)", bytes.len());
        }
        if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            bail!("not a RIFF/WAVE file");
        }
        let file_size = le32(&bytes[4..8]) as u64 + 8;
        let mut pos = 12usize;
        let mut fmt: Option<(u16, u16, u32, u16, u16)> = None; // tag, ch, rate, block_align, bits
        let mut data_offset = 0usize;
        let mut data_len = 0usize;
        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = le32(&bytes[pos + 4..pos + 8]) as usize;
            let body = pos + 8;
            if body + size > bytes.len() {
                bail!("truncated WAV chunk {:?} (size {} at {})", id, size, pos);
            }
            if id == b"fmt " {
                if size < 16 {
                    bail!("fmt chunk too small: {size}");
                }
                let tag = le16(&bytes[body..body + 2]);
                let channels = le16(&bytes[body + 2..body + 4]);
                let rate = le32(&bytes[body + 4..body + 8]);
                let block_align = le16(&bytes[body + 12..body + 14]);
                let bits = le16(&bytes[body + 14..body + 16]);
                let mut actual_tag = tag;
                if tag == 0xFFFE {
                    if size < 40 {
                        bail!("WAVE_FORMAT_EXTENSIBLE fmt chunk too small: {size}");
                    }
                    // subformat GUID starts at body+24; first 2 bytes are the real format tag
                    actual_tag = le16(&bytes[body + 24..body + 26]);
                }
                fmt = Some((actual_tag, channels, rate, block_align, bits));
            } else if id == b"data" {
                data_offset = body;
                data_len = size;
            }
            pos = body + size + (size & 1); // chunks are word-aligned
        }
        let (tag, channels, rate, _block_align, bits) = fmt.context("WAV missing fmt chunk")?;
        anyhow::ensure!(channels > 0, "WAV has zero channels");
        anyhow::ensure!(rate > 0, "WAV sample rate is zero");
        let format = match (tag, bits) {
            (1, 8) => SampleFormat::U8,
            (1, 16) => SampleFormat::I16,
            (1, 24) => SampleFormat::I24,
            (1, 32) => SampleFormat::I32,
            (3, 32) => SampleFormat::F32,
            (3, 64) => SampleFormat::F64,
            (1, _) | (3, _) => {
                bail!("unsupported WAV PCM tag={tag} bits={bits} (support PCM 8/16/24/32, float 32/64)")
            }
            (other, _) => bail!("unsupported WAV format tag {other} (only PCM/float)"),
        };
        if data_len == 0 {
            bail!("WAV has no data chunk");
        }
        Ok(Self {
            spec: WavSpec {
                channels,
                sample_rate: rate,
                format,
            },
            data: bytes,
            data_offset,
            data_len,
            cursor: 0,
            file_size,
        })
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn total_frames(&self) -> u64 {
        (self.data_len / self.spec.block_align().max(1) as usize) as u64
    }

    pub fn frames_remaining(&self) -> u64 {
        let bps = self.spec.fetch_bytes_per_frame();
        let left = self.data_len.saturating_sub(self.cursor);
        (left / bps.max(1)) as u64
    }

    /// Read up to `frames` interleaved f32 frames. Returns (samples, eof).
    pub fn read_frames(&mut self, frames: usize) -> Result<(Vec<f32>, bool)> {
        let ch = self.spec.channels as usize;
        let bps = self.spec.block_align() as usize;
        let avail_bytes = self.data_len.saturating_sub(self.cursor);
        let avail_frames = avail_bytes / bps.max(1);
        let n = frames.min(avail_frames);
        let mut out = Vec::with_capacity(n * ch);
        let base = self.data_offset + self.cursor;
        match self.spec.format {
            SampleFormat::U8 => {
                for i in 0..n * ch {
                    out.push((self.data[base + i] as f32 - 128.0) / 128.0);
                }
            }
            SampleFormat::I16 => {
                for i in 0..n * ch {
                    let p = base + i * 2;
                    out.push(pcm::i16_to_f32(le16(&self.data[p..p + 2]) as i16));
                }
            }
            SampleFormat::I24 => {
                for i in 0..n * ch {
                    let p = base + i * 3;
                    out.push(pcm::l24_to_f32(le_i24(&self.data[p..p + 3])));
                }
            }
            SampleFormat::I32 => {
                for i in 0..n * ch {
                    let p = base + i * 4;
                    out.push(le32(&self.data[p..p + 4]) as i32 as f32 / 2_147_483_648.0);
                }
            }
            SampleFormat::F32 => {
                for i in 0..n * ch {
                    let p = base + i * 4;
                    out.push(f32::from_le_bytes([
                        self.data[p],
                        self.data[p + 1],
                        self.data[p + 2],
                        self.data[p + 3],
                    ]));
                }
            }
            SampleFormat::F64 => {
                for i in 0..n * ch {
                    let p = base + i * 8;
                    let v = f64::from_le_bytes(self.data[p..p + 8].try_into().unwrap());
                    out.push(v as f32);
                }
            }
        }
        self.cursor += n * bps;
        let eof = self.cursor >= self.data_len;
        Ok((out, eof))
    }

    pub fn rewind(&mut self) {
        self.cursor = 0;
    }
}

impl WavSpec {
    pub fn fetch_bytes_per_frame(&self) -> usize {
        self.channels as usize * self.format.bytes_per_sample()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WavWriteSpec {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits: u16,
    pub float: bool,
}

impl WavWriteSpec {
    pub fn l24_48k(channels: u16) -> Self {
        Self {
            channels,
            sample_rate: 48_000,
            bits: 24,
            float: false,
        }
    }

    fn block_align(&self) -> u16 {
        self.channels * ((self.bits as u16 + 7) / 8)
    }
}

pub struct WavWriter {
    spec: WavWriteSpec,
    data: Vec<u8>,
    frames: u64,
}

impl WavWriter {
    pub fn new(spec: WavWriteSpec) -> Result<Self> {
        anyhow::ensure!(
            matches!(spec.bits, 16 | 24 | 32),
            "WAV bits must be 16/24/32"
        );
        anyhow::ensure!(
            spec.float == (spec.bits == 32) || !spec.float,
            "float requires 32 bits"
        );
        anyhow::ensure!(spec.channels > 0, "channels must be > 0");
        Ok(Self {
            spec,
            data: Vec::new(),
            frames: 0,
        })
    }

    pub fn write_frames(&mut self, interleaved: &[f32]) -> Result<()> {
        let ch = self.spec.channels as usize;
        anyhow::ensure!(
            interleaved.len() % ch == 0,
            "interleaved sample count {} not a multiple of channels {ch}",
            interleaved.len()
        );
        match (self.spec.float, self.spec.bits) {
            (true, 32) => {
                for s in interleaved {
                    self.data.extend_from_slice(&s.to_le_bytes());
                }
            }
            (false, 16) => {
                for s in interleaved {
                    self.data
                        .extend_from_slice(&pcm::f32_to_i16(*s).to_le_bytes());
                }
            }
            (false, 24) => {
                for s in interleaved {
                    let v = pcm::f32_to_l24(*s);
                    let u = (v & 0x00FF_FFFF) as u32;
                    self.data.push(u as u8);
                    self.data.push((u >> 8) as u8);
                    self.data.push((u >> 16) as u8);
                }
            }
            (false, 32) => {
                for s in interleaved {
                    let v = (s.clamp(-1.0, 1.0) * 2_147_483_647.0).round() as i32;
                    self.data.extend_from_slice(&v.to_le_bytes());
                }
            }
            _ => bail!("unsupported WAV write format bits={}", self.spec.bits),
        }
        self.frames += (interleaved.len() / ch) as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn finish(self) -> Vec<u8> {
        let data_len = self.data.len() as u32;
        let block_align = self.spec.block_align();
        let byte_rate = self.spec.sample_rate * block_align as u32;
        let fmt_tag: u16 = if self.spec.float { 3 } else { 1 };
        let mut out = Vec::with_capacity(44 + self.data.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&fmt_tag.to_le_bytes());
        out.extend_from_slice(&self.spec.channels.to_le_bytes());
        out.extend_from_slice(&self.spec.sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&self.spec.bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }
}

pub fn write_wav_file<P: AsRef<Path>>(path: P, spec: WavWriteSpec, frames: &[f32]) -> Result<()> {
    let mut w = WavWriter::new(spec)?;
    w.write_frames(frames)?;
    fs::write(path.as_ref(), w.finish())
        .with_context(|| format!("failed to write WAV {:?}", path.as_ref()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm;

    fn sine(n: usize, freq: f32, rate: f32, ch: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * ch);
        for i in 0..n {
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin() * 0.5;
            for _ in 0..ch {
                v.push(s);
            }
        }
        v
    }

    #[test]
    fn writer_reader_roundtrip_l24_stereo() {
        let spec = WavWriteSpec::l24_48k(2);
        let samples = sine(480, 1000.0, 48_000.0, 2);
        let mut w = WavWriter::new(spec).unwrap();
        w.write_frames(&samples).unwrap();
        assert_eq!(w.frames(), 480);
        let bytes = w.finish();
        let mut r = WavReader::from_bytes(bytes).unwrap();
        assert_eq!(r.spec.channels, 2);
        assert_eq!(r.spec.sample_rate, 48_000);
        assert_eq!(r.spec.format, SampleFormat::I24);
        assert_eq!(r.total_frames(), 480);
        let (back, eof) = r.read_frames(1000).unwrap();
        assert!(eof);
        assert_eq!(back.len(), 960);
        for (a, b) in samples.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1.0 / 4_000_000.0);
        }
        assert_eq!(r.frames_remaining(), 0);
    }

    #[test]
    fn writer_reader_roundtrip_f32_and_i16() {
        let samples: Vec<f32> = (0..200)
            .map(|i| ((i as f32) / 200.0 * 2.0 - 1.0) * 0.7)
            .collect();
        for (bits, float, tol) in [(16u16, false, 1.0 / 10_000.0), (32u16, true, 1e-6)] {
            let spec = WavWriteSpec {
                channels: 1,
                sample_rate: 48_000,
                bits,
                float,
            };
            let mut w = WavWriter::new(spec).unwrap();
            w.write_frames(&samples).unwrap();
            let mut r = WavReader::from_bytes(w.finish()).unwrap();
            let (back, eof) = r.read_frames(1000).unwrap();
            assert!(eof);
            for (a, b) in samples.iter().zip(back.iter()) {
                assert!(
                    (a - b).abs() < tol,
                    "{a} vs {b} (bits={bits} float={float})"
                );
            }
        }
    }

    #[test]
    fn reader_handles_extensible_and_odd_chunks() {
        // Build a minimal extensible PCM16 WAV by hand.
        let frames = 120u32;
        let ch = 2u16;
        let rate = 48_000u32;
        let block = ch * 2;
        let data_len = frames * block as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(4 + (8 + 40) + (8 + data_len) + 1).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"JUNK");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        out.push(0); // pad
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&0xFFFEu16.to_le_bytes());
        out.extend_from_slice(&ch.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * block as u32).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        out.extend_from_slice(&16u16.to_le_bytes()); // valid bits
        out.extend_from_slice(&3u32.to_le_bytes()); // channel mask
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM subformat
        out.extend_from_slice(&[0u8; 14]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for _ in 0..frames as usize * ch as usize {
            out.extend_from_slice(&pcm::f32_to_i16(0.25).to_le_bytes());
        }
        let mut r = WavReader::from_bytes(out).unwrap();
        assert_eq!(r.spec.format, SampleFormat::I16);
        assert_eq!(r.total_frames(), frames as u64);
        let (s, _) = r.read_frames(10).unwrap();
        assert!((s[0] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn reader_rejects_garbage() {
        assert!(WavReader::from_bytes(vec![0u8; 4]).is_err());
        assert!(WavReader::from_bytes(b"RIFF\x00\x00\x00\x00WAVE".to_vec()).is_err());
    }
}
