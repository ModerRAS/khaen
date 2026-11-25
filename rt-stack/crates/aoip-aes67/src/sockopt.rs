//! 最小跨平台 socket 选项 FFI（std 未暴露 `setsockopt`）。
//!
//! 只做接收端需要的两件事：**SO_RCVBUF 调优**（AES67 1 ms 包突发，默认缓冲常太小）
//! 与其回读（证据）。仅本模块有 `extern`，其它模块保持纯 safe std。
//!
//! 平台：
//! - Windows：`ws2_32!setsockopt`（SOL_SOCKET=0xffff / SO_RCVBUF=0x1002）；
//! - Linux/macOS：`libc::setsockopt`（SOL_SOCKET=1 / SO_RCVBUF=8；Linux 内核对请求值
//!   会加倍并受 net.core.rmem_max 限制，回读值为实际生效值）；
//! - 其它平台：返回 `Unsupported`，接收端仍可工作（使用系统默认缓冲）。

use std::io;
use std::net::UdpSocket;

/// 设置接收缓冲（`SO_RCVBUF`）。
///
/// `bytes` 为请求值；返回值 = 内核实际生效值（回读失败返回请求值）。
/// 失败不致命（例如超出 `rmem_max` 权限），调用方记录后继续。
pub fn set_receive_buffer(socket: &UdpSocket, bytes: usize) -> io::Result<usize> {
    platform::set_receive_buffer(socket, bytes)?;
    match platform::receive_buffer(socket) {
        Ok(applied) if applied > 0 => Ok(applied),
        _ => Ok(bytes),
    }
}

/// 回读接收缓冲（`SO_RCVBUF`；证据/测试用，生产路径由 `set_receive_buffer` 内部回读）。
#[allow(dead_code)]
pub fn receive_buffer(socket: &UdpSocket) -> io::Result<usize> {
    platform::receive_buffer(socket)
}

/// 组播"只接受本 socket 已 join 的组"（Linux `IP_MULTICAST_ALL=0`）。
///
/// 返回 `Ok(true)` = 已限制；`Ok(false)` = 本平台无需该选项（Windows 只交付本 socket
/// join 的组）；`Err` = 设置失败（调用方决定是否 fail closed）。
///
/// 背景（task-320）：Linux 默认 `IP_MULTICAST_ALL=1`，socket 会收到**本机任意进程**
/// join 过的组播组流量（正是"0.0.0.0 来什么收什么"的一半原因）；`aes67_group_allow`
/// 非空时必须关掉它，才能保证"只 join/接受列出的组"。
pub fn exclude_unjoined_multicast(socket: &UdpSocket) -> io::Result<bool> {
    platform::exclude_unjoined_multicast(socket)
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::net::UdpSocket;
    use std::os::windows::io::AsRawSocket;

    const SOL_SOCKET: i32 = 0xffff;
    const SO_RCVBUF: i32 = 0x1002;

    #[link(name = "ws2_32")]
    extern "system" {
        fn setsockopt(
            socket: usize,
            level: i32,
            optname: i32,
            optval: *const u8,
            optlen: i32,
        ) -> i32;
        fn getsockopt(
            socket: usize,
            level: i32,
            optname: i32,
            optval: *mut u8,
            optlen: *mut i32,
        ) -> i32;
    }

    pub fn set_receive_buffer(socket: &UdpSocket, bytes: usize) -> io::Result<()> {
        let value: i32 = bytes.min(i32::MAX as usize) as i32;
        let result = unsafe {
            setsockopt(
                socket.as_raw_socket() as usize,
                SOL_SOCKET,
                SO_RCVBUF,
                &value as *const i32 as *const u8,
                std::mem::size_of::<i32>() as i32,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn receive_buffer(socket: &UdpSocket) -> io::Result<usize> {
        let mut value: i32 = 0;
        let mut len: i32 = std::mem::size_of::<i32>() as i32;
        let result = unsafe {
            getsockopt(
                socket.as_raw_socket() as usize,
                SOL_SOCKET,
                SO_RCVBUF,
                &mut value as *mut i32 as *mut u8,
                &mut len as *mut i32,
            )
        };
        if result == 0 && value > 0 {
            Ok(value as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Windows 无 `IP_MULTICAST_ALL` 等价项：内核只把本 socket 已 join 的组交给它，
    /// 因此返回 `Ok(false)`（无需设置）。
    pub fn exclude_unjoined_multicast(_socket: &UdpSocket) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::net::UdpSocket;
    use std::os::unix::io::AsRawFd;

    const SOL_SOCKET: i32 = 1;
    const SO_RCVBUF: i32 = 8;

    #[cfg(target_os = "linux")]
    extern "C" {
        fn setsockopt(
            fd: i32,
            level: i32,
            optname: i32,
            optval: *const core::ffi::c_void,
            optlen: u32,
        ) -> i32;
        fn getsockopt(
            fd: i32,
            level: i32,
            optname: i32,
            optval: *mut core::ffi::c_void,
            optlen: *mut u32,
        ) -> i32;
    }

    #[cfg(not(target_os = "linux"))]
    extern "C" {
        fn setsockopt(
            fd: i32,
            level: i32,
            optname: i32,
            optval: *const core::ffi::c_void,
            optlen: u32,
        ) -> i32;
        fn getsockopt(
            fd: i32,
            level: i32,
            optname: i32,
            optval: *mut core::ffi::c_void,
            optlen: *mut u32,
        ) -> i32;
    }

    pub fn set_receive_buffer(socket: &UdpSocket, bytes: usize) -> io::Result<()> {
        let value: i32 = bytes.min(i32::MAX as usize) as i32;
        let result = unsafe {
            setsockopt(
                socket.as_raw_fd(),
                SOL_SOCKET,
                SO_RCVBUF,
                &value as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn receive_buffer(socket: &UdpSocket) -> io::Result<usize> {
        let mut value: i32 = 0;
        let mut len: u32 = std::mem::size_of::<i32>() as u32;
        let result = unsafe {
            getsockopt(
                socket.as_raw_fd(),
                SOL_SOCKET,
                SO_RCVBUF,
                &mut value as *mut i32 as *mut core::ffi::c_void,
                &mut len as *mut u32,
            )
        };
        if result == 0 && value > 0 {
            Ok(value as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// `IP_MULTICAST_ALL`（Linux 仅）：非 0 = 接收本机任意 socket join 的组（默认 1）。
    /// 置 0 后只交付本 socket 自己 join 的组。macOS 无此选项（本身即按 socket 成员关系交付）。
    #[cfg(target_os = "linux")]
    pub fn exclude_unjoined_multicast(socket: &UdpSocket) -> io::Result<bool> {
        const IPPROTO_IP: i32 = 0;
        const IP_MULTICAST_ALL: i32 = 49;
        let value: i32 = 0;
        // SAFETY: setsockopt 读取 &value（4 字节）；fd 来自有效 socket。
        let result = unsafe {
            setsockopt(
                socket.as_raw_fd(),
                IPPROTO_IP,
                IP_MULTICAST_ALL,
                &value as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            )
        };
        if result == 0 {
            Ok(true)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// 非 Linux unix（macOS）：无 IP_MULTICAST_ALL，内核按 socket 成员关系交付 → 无需设置。
    #[cfg(not(target_os = "linux"))]
    pub fn exclude_unjoined_multicast(_socket: &UdpSocket) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use std::io;
    use std::net::UdpSocket;

    pub fn set_receive_buffer(_socket: &UdpSocket, _bytes: usize) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SO_RCVBUF tuning not wired on this platform",
        ))
    }

    pub fn receive_buffer(_socket: &UdpSocket) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SO_RCVBUF readback not wired on this platform",
        ))
    }

    pub fn exclude_unjoined_multicast(_socket: &UdpSocket) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_buffer_can_be_tuned_and_read_back() {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let applied = set_receive_buffer(&socket, 1 << 20).expect("setsockopt");
        assert!(applied > 0);
        let readback = receive_buffer(&socket).expect("getsockopt");
        assert!(readback > 0);
        // 1 MiB 请求在 Windows/Linux 默认权限下都会生效（Linux 内核常给 2×）。
        assert!(applied >= 1 << 15, "applied={applied}");
    }
}
