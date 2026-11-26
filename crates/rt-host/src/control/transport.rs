//! 行协议传输：一行 JSON 请求 / 一行 JSON 响应。
//!
//! 与具体 socket 无关：[serve_connection] 接任意 BufRead + Write，Unix socket 只是把连接
//! 塞进来（unix.rs）。内存双工/回环夹具因此也能跑完整协议测试。
//!
//! 语义与 host 的 control::serve::serve_loop 一致（同一份契约）：
//! - 逐行读请求、逐行写响应；对端关闭（EOF）结束；
//! - 单行坏请求不中断连接，回 bad_request 后继续读下一行；
//! - 单行超过 MAX_REQUEST_BYTES 时回 bad_request 并**丢弃到下一个换行**（保持同步），
//!   不会为超长行无限分配内存；
//! - 最后一行没有换行符就 EOF：按一行处理（与 read_until 的行为一致）。

use std::io::{self, BufRead, Write};
use std::sync::Mutex;

use khaen_host::control::{error, ControlError, MAX_REQUEST_BYTES};

use super::core::ControlCore;

/// 一行读取的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRead {
    /// 对端关闭（没有更多请求）。
    Eof,
    /// 读到完整一行（不含换行符，最多 MAX_REQUEST_BYTES 字节）。
    Line,
    /// 超长行：已丢弃到下一个换行，调用方应回 bad_request。
    TooLong,
}

/// 读一行（不含结尾换行），超过上限时丢弃剩余部分并返回 TooLong。
///
/// 用 fill_buf/consume 手动组行：阻塞式读取下与 read_until 等价，但能限制单行内存，
/// 并在超限后把连接重新同步到下一个换行。
pub fn read_line_limited<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> io::Result<LineRead> {
    line.clear();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if line.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&chunk[..position]);
            reader.consume(position + 1);
            return Ok(if line.len() > MAX_REQUEST_BYTES {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
        line.extend_from_slice(chunk);
        let consumed = chunk.len();
        reader.consume(consumed);
        if line.len() > MAX_REQUEST_BYTES {
            // 超限：丢弃到下一个换行（不累积），下次读请求仍从行边界开始。
            loop {
                let chunk = reader.fill_buf()?;
                if chunk.is_empty() {
                    return Ok(LineRead::TooLong);
                }
                if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
                    reader.consume(position + 1);
                    return Ok(LineRead::TooLong);
                }
                let consumed = chunk.len();
                reader.consume(consumed);
            }
        }
    }
}

/// 超长请求的稳定响应（与 ControlCore 的错误形状一致）。
fn too_long_response() -> String {
    ControlError::new(
        error::BAD_REQUEST,
        format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
    )
    .to_json()
    .to_string()
}

/// 处理一条连接：逐行分发到 ControlCore，逐行写响应，直到 EOF。
///
/// 多连接共享同一把 ControlCore 锁：协议状态（草稿/历史/模式/格式）被串行化，
/// 不会出现两个 commit 同时生效。
pub fn serve_connection<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    core: &Mutex<ControlCore>,
) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        match read_line_limited(reader, &mut line)? {
            LineRead::Eof => return Ok(()),
            LineRead::TooLong => write_response(writer, &too_long_response())?,
            LineRead::Line => {
                let text = String::from_utf8_lossy(&line);
                let request = text.trim_end_matches(['\r', '\n']);
                let response = {
                    let mut guard = core.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.handle_line(request)
                };
                write_response(writer, &response)?;
            }
        }
    }
}

/// 写一行响应（JSON + 换行）并 flush；写入失败（对端已关）向上抛给调用方结束连接。
fn write_response<W: Write>(writer: &mut W, response: &str) -> io::Result<()> {
    writer.write_all(response.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}
