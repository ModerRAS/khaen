//! forbid：RT 模块禁止行为扫描（core-v1 §10 F-01..F-44 子集 + §13 R-DROP 规则）。
//!
//! 扫描范围：文件内出现 RT-MODULE 标记的 .rs 文件（DSP 纯 RT 模块）。混合模块
//! （同时含 [RT] 与 [NONRT] 代码，如 rt-host::clock）由代码评审覆盖，不在此文件级扫描。
//! RT 内禁止格式化/分配/锁/syscall；规则 ID 与 core-v1 F 表对应。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Report, Violation};

/// RT 纯模块标记（放在文件头注释中；见 core-v1 R-DROP-14 文件头要求）。
pub const RT_MODULE_MARKER: &str = "RT-MODULE:";

/// 一条禁止符号规则（对应 core-v1 F 表条目）。
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// 规则 ID（F-01..F-44 子集）。
    pub id: &'static str,
    /// 命中符号（子串匹配，注释已剥离）。
    pub needles: &'static [&'static str],
    /// 禁止原因/允许替代。
    pub why: &'static str,
}

/// T06 首批可执行规则（core-v1 §10/§13；完整 F-01..F-44 审查仍由 T07..T21 任务负责）。
pub const RULES: &[Rule] = &[
    Rule {
        id: "F-01",
        needles: &[
            "Vec<",
            "Vec::",
            "vec![",
            "String::",
            "String,",
            "String>",
            "format!(",
            "to_string(",
            "Box::new",
            "HashMap",
            "BTreeMap",
            "Cow<",
        ],
        why: "heap 分配/owned 容器进 RT（固定 arena + Copy 值）",
    },
    Rule {
        id: "F-03",
        needles: &["Arc<", "Arc::", "Rc<", "Rc::", "Weak<"],
        why: "引用计数进 RT（PlanRef/raw pointer+version）",
    },
    Rule {
        id: "F-04",
        needles: &["Box<dyn", "&dyn ", "dyn Trait"],
        why: "trait object 动态分派进 RT",
    },
    Rule {
        id: "F-05",
        needles: &["PathBuf", "CString", "OsString", "HashSet", "BTreeSet"],
        why: "路径/字符串/容器类型进 RT",
    },
    Rule {
        id: "F-07",
        needles: &["FftPlanner", "rustfft", "realfft"],
        why: "RT 内创建/销毁 FFT plan（CTL 预建）",
    },
    Rule {
        id: "F-08",
        needles: &["mmap(", "munmap(", "mlock(", "mprotect("],
        why: "RT 内内存映射（admission 前完成）",
    },
    Rule {
        id: "F-09",
        needles: &["Mutex", "RwLock", "OnceLock", "Condvar", "parking_lot"],
        why: "RT 内锁（无锁 SPSC/原子 ack）",
    },
    Rule {
        id: "F-10",
        needles: &["thread::sleep", "yield_now", "futex", ".await"],
        why: "RT 内阻塞等待（有界 spin + cutoff）",
    },
    Rule {
        id: "F-12",
        needles: &["mpsc", "crossbeam"],
        why: "分配式 channel 进 RT（固定 SPSC ring）",
    },
    Rule {
        id: "F-13",
        needles: &[
            "std::fs::",
            "std::net::",
            "std::process::",
            "libc::",
            "clock_gettime",
        ],
        why: "RT 内 syscall（I/O 区/CTL 完成）",
    },
    Rule {
        id: "F-14",
        needles: &["Instant::now", "SystemTime::now", "rdtsc", "CNTVCT"],
        // core-v1 v1.1.2 errata E2：F-14 预算 ≤3 次/block（B1 1 + 可选 cutoff 1 + B3 1；
        // 正常路径 2 次；纯 worker 0 次）。
        why: "纯 worker 读时钟（仅 master 每 block ≤3 次预算 tick；正常路径 2 次）",
    },
    Rule {
        id: "F-16",
        needles: &["println!", "eprintln!", "log::", "tracing::"],
        why: "RT 内日志/打印（事件环 + 计数）",
    },
    Rule {
        id: "F-17",
        needles: &["thread::spawn", "thread::Builder", "JoinHandle"],
        why: "RT 内创建/join 线程",
    },
    Rule {
        id: "F-18",
        needles: &["thread_local!", "lazy_static!"],
        why: "TLS/懒初始化进 RT",
    },
    Rule {
        id: "F-20",
        needles: &["std::env", "/proc/", "/sys/"],
        why: "RT 读环境/系统文件",
    },
    Rule {
        id: "F-21",
        needles: &[
            "panic!",
            "unwrap(",
            "expect(",
            "unreachable!",
            "todo!",
            "unimplemented!",
            "assert!(",
        ],
        why: "RT 内 panic 类宏（RtStatus + 显式分支）",
    },
    Rule {
        id: "F-24",
        needles: &["catch_unwind"],
        why: "RT 内 catch_unwind",
    },
    Rule {
        id: "F-25",
        needles: &["Result<(), String>", "Box<dyn Error", "std::error::Error"],
        why: "带 Drop 的错误类型跨 RT（RtStatus）",
    },
    Rule {
        id: "F-26",
        needles: &["process::abort", "process::exit", "Command::new"],
        why: "RT 内 abort/exit/子进程",
    },
    Rule {
        id: "F-27",
        needles: &["drop(", "drop_in_place", "dealloc("],
        why: "RT 内析构/释放（R-DROP-01）",
    },
    Rule {
        id: "F-28",
        needles: &["mem::forget", "ManuallyDrop"],
        why: "用 forget/ManuallyDrop 规避析构",
    },
    Rule {
        id: "F-36",
        needles: &["rayon", "work_steal", "thread_pool"],
        why: "RT 内 work stealing/动态线程池",
    },
    Rule {
        id: "F-38",
        needles: &["extern \"C\"", "#[no_mangle]"],
        why: "RT 内 FFI（v1 无 FFI）",
    },
    Rule {
        id: "F-40",
        needles: &[
            "is_x86_feature_detected",
            "is_aarch64_feature_detected",
            "cpuid",
        ],
        why: "RT 内运行期 CPU feature 探测",
    },
];

/// 扫描目录/文件中所有 RT-MODULE 的 .rs 文件。
pub fn scan_path(root: &Path) -> io::Result<Report> {
    let mut files = Vec::new();
    collect_rs_files(root, &mut files)?;
    let mut report = Report::default();
    for file in files {
        scan_file(&file, &mut report)?;
    }
    Ok(report)
}

/// 扫描单个 .rs 文件（非 RT 模块则只计数不检查）。
pub fn scan_file(path: &Path, report: &mut Report) -> io::Result<()> {
    let text = fs::read_to_string(path)?;
    report.scanned += 1;
    let name = path.display().to_string();
    report.violations.extend(scan_text(&name, &text));
    Ok(())
}

/// 文本是否声明为 RT 纯模块：仅文件头注释行内的 RT-MODULE 标记有效（不接受字符串字面量）。
pub fn is_rt_module(text: &str) -> bool {
    text.lines().take(64).any(|line| {
        let trimmed = line.trim_start();
        (trimmed.starts_with("//!") || trimmed.starts_with("//"))
            && trimmed.contains(RT_MODULE_MARKER)
    })
}

/// 纯函数扫描入口（供 fixture 测试直接调用）。
pub fn scan_text(name: &str, text: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    if !is_rt_module(text) {
        return out;
    }
    let mut in_const_assert = false;
    for (index, raw) in text.lines().enumerate() {
        // 编译期静态断言（const _: () = { ... }）不在 RT 运行期执行，允许出现在 RT 模块头。
        let trimmed = raw.trim();
        if trimmed.starts_with("const _: () = {") {
            in_const_assert = true;
            continue;
        }
        if in_const_assert {
            if trimmed == "};" {
                in_const_assert = false;
            }
            continue;
        }
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        for rule in RULES {
            let hit = rule.needles.iter().find(|needle| line.contains(**needle));
            if let Some(needle) = hit {
                out.push(Violation {
                    file: name.to_string(),
                    line: index + 1,
                    rule: rule.id,
                    detail: format!("{}（{}）", needle, rule.why),
                });
            }
        }
    }
    out
}

fn strip_comment(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return "";
    }
    match line.find("//") {
        Some(pos) => &line[..pos],
        None => line,
    }
}

fn collect_rs_files(path: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            collect_rs_files(&child, out)?;
        } else if child.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(child);
        }
    }
    Ok(())
}
