//! cfg(unix) 传输：Unix domain socket，每连接一个处理线程，操作共享的 ControlCore。
//!
//! - 监听线程：非阻塞 accept + 2 ms 轮询停止标志（Drop 时 join，不会挂住）；
//! - 每连接线程：阻塞式逐行处理（transport::serve_connection），对端关闭或服务端退出时结束；
//! - 连接线程是分离的：句柄 Drop 只停止接受新连接并移除 socket 文件，已有连接随客户端关闭退出。
//!
//! 安全边界：这是本机可信连接（daemon 与 rt-host 同机），协议层不做认证；socket 文件权限
//! 由部署（systemd RuntimeDirectory / umask）决定。跨机/多用户部署需要另外的鉴权层，
//! 不在本任务（如实标注）。

use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::core::ControlCore;
use super::transport::serve_connection;

/// accept 轮询间隔（也决定 Drop -> join 的最坏延迟）。
const ACCEPT_POLL: Duration = Duration::from_millis(2);

/// Unix socket 控制服务端（内部句柄；对外由 control::ControlServer 包装）。
#[derive(Debug)]
pub(super) struct UnixControlServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl UnixControlServer {
    /// 绑定路径并启动 accept 线程。
    ///
    /// 路径已存在时：能连上说明有活着的 rt-host 在监听（拒绝启动，避免互相抢控制面）；
    /// 连不上视为陈旧 socket，删除后重建。
    pub(super) fn start(path: &Path, core: Arc<Mutex<ControlCore>>) -> io::Result<Self> {
        // systemd 部署通常由 RuntimeDirectory 建目录；独立运行时 best-effort 补建（失败不阻断）。
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let _ = std::fs::create_dir_all(parent);
        }
        if path.exists() {
            match UnixStream::connect(path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "control socket {} is already in use (another rt-host is listening)",
                            path.display()
                        ),
                    ));
                }
                Err(_) => match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                },
            }
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_path = path.to_path_buf();
        let join = match thread::Builder::new()
            .name("khaen-rt-control".to_string())
            .spawn(move || accept_loop(listener, core, thread_stop, thread_path))
        {
            Ok(join) => join,
            Err(err) => {
                let _ = std::fs::remove_file(path);
                return Err(err);
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            stop,
            join: Some(join),
        })
    }

    /// 监听路径。
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for UnixControlServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 监听线程：accept -> 每连接一个处理线程；停止标志置位后退出并删除 socket 文件。
fn accept_loop(
    listener: UnixListener,
    core: Arc<Mutex<ControlCore>>,
    stop: Arc<AtomicBool>,
    path: PathBuf,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _address)) => {
                let connection_core = Arc::clone(&core);
                let spawn = thread::Builder::new()
                    .name("khaen-rt-control-conn".to_string())
                    .spawn(move || {
                        let _ = serve_stream(stream, &connection_core);
                    });
                if let Err(err) = spawn {
                    eprintln!("khaen-rt-host: control connection thread spawn failed: {err}");
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                eprintln!("khaen-rt-host: control accept failed: {err}");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    // Drop 的 join 保证 accept 线程已退出，这里删除 socket 文件是安全的。
    let _ = std::fs::remove_file(&path);
}

/// 一条连接：阻塞式逐行处理（每个请求一行响应）。
fn serve_stream(stream: UnixStream, core: &Mutex<ControlCore>) -> io::Result<()> {
    let writer_stream = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut writer = BufWriter::new(writer_stream);
    serve_connection(&mut reader, &mut writer, core)?;
    writer.flush()
}
