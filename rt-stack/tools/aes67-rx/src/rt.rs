//! Linux 实时性原语：mlockall / SCHED_FIFO / CPU 亲和 / SIGINT 标志。
//!
//! 失败不阻塞主流程（报告里如实记录），但默认在启动期要求成功一次以便尽早暴露权限问题。
//! 全部热路径之外调用。

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    static SIGINT: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_sigint(_sig: libc::c_int) {
        SIGINT.store(true, Ordering::Release);
    }

    /// 安装 SIGINT/SIGTERM 处理（优雅停止，仍写报告）。
    pub fn install_signal_handlers() -> Result<(), String> {
        unsafe {
            let h = on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t;
            if libc::signal(libc::SIGINT, h) == libc::SIG_ERR {
                return Err("signal(SIGINT) failed".into());
            }
            if libc::signal(libc::SIGTERM, h) == libc::SIG_ERR {
                return Err("signal(SIGTERM) failed".into());
            }
        }
        Ok(())
    }

    /// 是否收到 SIGINT/SIGTERM。
    pub fn stop_requested() -> bool {
        SIGINT.load(Ordering::Acquire)
    }

    /// 锁定当前与未来内存页（失败只在报告里记录）。
    pub fn mlock_all() -> Result<(), String> {
        let rc = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
        if rc != 0 {
            return Err(format!(
                "mlockall failed errno={}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// 本线程切 SCHED_FIFO（需 CAP_SYS_NICE / root）。
    pub fn set_realtime(priority: i32) -> Result<(), String> {
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        param.sched_priority = priority;
        let rc = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) };
        if rc != 0 {
            return Err(format!(
                "sched_setscheduler(SCHED_FIFO,{priority}) failed errno={}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// 本线程绑定到指定 CPU（cpu_set_t 是位图，按字节置位避免依赖宏绑定）。
    pub fn set_affinity_cpu(cpu: usize) -> Result<(), String> {
        if cpu >= 4096 {
            return Err(format!("cpu {cpu} out of range"));
        }
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let bytes = &mut set as *mut libc::cpu_set_t as *mut u8;
        unsafe {
            *bytes.add(cpu / 8) |= 1u8 << (cpu % 8);
            let rc = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            if rc != 0 {
                return Err(format!(
                    "sched_setaffinity(cpu={cpu}) failed errno={}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(())
    }

    /// 当前线程的调度策略名称（证据用）。
    pub fn scheduler_name() -> &'static str {
        let policy = unsafe { libc::sched_getscheduler(0) };
        match policy {
            libc::SCHED_FIFO => "SCHED_FIFO",
            libc::SCHED_RR => "SCHED_RR",
            libc::SCHED_OTHER => "SCHED_OTHER",
            libc::SCHED_BATCH => "SCHED_BATCH",
            libc::SCHED_IDLE => "SCHED_IDLE",
            _ => "UNKNOWN",
        }
    }

    /// 当前线程最近一次运行的 CPU（证据用；读 /proc/thread-self/stat 的 processor 字段，
    /// 比 sched_getcpu 在该 glibc/rseq 配置下更可靠）。
    pub fn current_cpu() -> Option<usize> {
        let text = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
        // comm 可能含空格：取最后一个 ')' 之后的字段；processor 是整行第 39 个字段（1-based），
        // ')' 之后从 state（第 3 字段）开始，故 processor 偏移 = 39 - 3。
        let tail = text.rsplit_once(')')?.1;
        let fields: Vec<&str> = tail.split_whitespace().collect();
        fields.get(36)?.parse::<usize>().ok()
    }
}

#[cfg(target_os = "linux")]
pub use imp::*;

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn install_signal_handlers() -> Result<(), String> {
        Err("non-linux".into())
    }
    pub fn stop_requested() -> bool {
        false
    }
    pub fn mlock_all() -> Result<(), String> {
        Err("non-linux".into())
    }
    pub fn set_realtime(_priority: i32) -> Result<(), String> {
        Err("non-linux".into())
    }
    pub fn set_affinity_cpu(_cpu: usize) -> Result<(), String> {
        Err("non-linux".into())
    }
    pub fn scheduler_name() -> &'static str {
        "UNKNOWN"
    }
    pub fn current_cpu() -> Option<usize> {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub use imp::*;
