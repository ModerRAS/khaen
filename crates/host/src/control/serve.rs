//! 行协议循环：不关心底层是 Unix socket、管道还是测试用的内存缓冲。

use std::io::{BufRead, Write};

use super::ControlState;

/// 逐行读请求、逐行写响应，直到对端关闭。
///
/// 单行异常长时回 `bad_request` 并继续读下一行，不因为一个坏请求中断服务。
/// 这里假定控制 socket 是本地可信连接；长度检查是"拒绝处理"而不是内存防护。
pub fn serve_loop<R: BufRead, W: Write>(
    state: &mut ControlState,
    reader: &mut R,
    writer: &mut W,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        let line = String::from_utf8_lossy(&buffer);
        let response = state.handle_line(line.trim_end_matches(['\r', '\n']));
        writeln!(writer, "{response}")?;
        writer.flush()?;
    }
}
