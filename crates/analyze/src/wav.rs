//! WAV 读取（16/24/32-bit PCM 与 32-bit float，1..=8 声道混单声道）。
//!
//! 解析实现复用 khaen-calibration 的 WAV 解码器（与 khaen-band-levels /
//! khaen-closed-loop-run 同一套），这里只再导出类型与函数：同一份解码数值逻辑不在
//! 仓库里分叉。只读文件、不猜测格式、不做重采样；错误码保持 E_WAV_INPUT / E_WAV_IO。

pub use khaen_calibration::wav::{parse_wav, read_wav_file, Wav, WavError, BAD_INPUT, IO};
