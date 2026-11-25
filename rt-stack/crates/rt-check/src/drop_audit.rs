//! drop-audit：§13 无 RT 析构专项（类型清单 + RT 模块静态扫描）。
//!
//! 类型清单对照 core-v1 §13.2：RT 可见类型必须 core::mem::needs_drop == false。
//! 静态扫描对象：RT-MODULE .rs 文件中的 impl Drop / owned 容器 / 引用计数 / 手动释放。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Report, Violation};

/// RT 模块内禁止的析构/owned 类型符号（core-v1 §13 R-DROP-01..14 子集）。
pub const DROP_NEEDLES: &[(&str, &str)] = &[
    ("impl Drop", "RT 模块内 impl Drop（R-DROP-02）"),
    ("drop_in_place", "RT 内显式 drop_in_place（R-DROP-01）"),
    ("Arc<", "RT 内 Arc（R-DROP-03/05）"),
    ("Rc<", "RT 内 Rc（R-DROP-03）"),
    ("Weak<", "RT 内 Weak（R-DROP-03）"),
    ("Box<", "RT 内 Box owned 类型（R-DROP-03）"),
    ("Vec<", "RT 内 Vec（R-DROP-03）"),
    ("String", "RT 内 String（R-DROP-03）"),
    ("HashMap", "RT 内 HashMap（R-DROP-03）"),
    ("BTreeMap", "RT 内 BTreeMap（R-DROP-03）"),
    ("ManuallyDrop", "用 ManuallyDrop 规避析构（R-DROP-01/F-28）"),
    ("mem::forget", "用 forget 规避析构（F-28）"),
];

/// 检查 §13.2 的 RT 可见类型清单：全部 needs_drop == false。
pub fn audit_types() -> Report {
    let mut report = Report::default();
    macro_rules! no_drop {
        ($name:literal, $ty:ty) => {{
            report.scanned += 1;
            if core::mem::needs_drop::<$ty>() {
                report.push("<type>", 0, "D-01", format!("{} needs_drop == true", $name));
            }
        }};
    }

    use tspi_dsp_core::abi::{
        TspiBlockAckV1, TspiBlockDescV1, TspiDiagFirV1, TspiEpochInfoV1, TspiEventV1,
        TspiHwParamsV1, TspiPathV1, TspiPlanHeaderV1, TspiRoleV1, TspiSpscHeaderV1,
        TspiWorkerStatsV1,
    };
    use tspi_dsp_core::rt::{BlockIo, PlanRef, PlanView, RtControl, RtStatus, WorkerRtCtx};
    use tspi_rt_host::fault::{EventRingLane, SafetyAction, WatchdogAction};
    use tspi_rt_host::format::{DitherCfg, FormatStats};

    no_drop!("RtStatus", RtStatus);
    no_drop!("PlanRef", PlanRef);
    no_drop!("PlanView", PlanView<'static>);
    no_drop!("BlockIo", BlockIo<'static>);
    no_drop!("WorkerRtCtx", WorkerRtCtx<'static>);
    no_drop!("TspiBlockDescV1", TspiBlockDescV1);
    no_drop!("TspiEpochInfoV1", TspiEpochInfoV1);
    no_drop!("TspiEventV1", TspiEventV1);
    no_drop!("TspiHwParamsV1", TspiHwParamsV1);
    no_drop!("TspiPlanHeaderV1", TspiPlanHeaderV1);
    no_drop!("TspiBlockAckV1", TspiBlockAckV1);
    no_drop!("TspiSpscHeaderV1", TspiSpscHeaderV1);
    no_drop!("TspiWorkerStatsV1", TspiWorkerStatsV1);
    no_drop!("TspiPathV1", TspiPathV1);
    no_drop!("TspiDiagFirV1", TspiDiagFirV1);
    no_drop!("TspiRoleV1", TspiRoleV1);
    no_drop!("RtControl", RtControl);
    no_drop!("EventRingLane", EventRingLane);
    no_drop!("SafetyAction", SafetyAction);
    no_drop!("WatchdogAction", WatchdogAction);
    no_drop!("DitherCfg", DitherCfg);
    no_drop!("FormatStats", FormatStats);

    report
}

/// 扫描目录/文件中 RT-MODULE 的 .rs 文件（析构/owned 类型规则）。
pub fn scan_path(root: &Path) -> io::Result<Report> {
    let mut files = Vec::new();
    collect_rs_files(root, &mut files)?;
    let mut report = Report::default();
    for file in files {
        let text = fs::read_to_string(&file)?;
        report.scanned += 1;
        let name = file.display().to_string();
        report.violations.extend(scan_text(&name, &text));
    }
    Ok(report)
}

/// 纯函数扫描入口（供 fixture 测试直接调用）。
pub fn scan_text(name: &str, text: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    if !crate::forbid::is_rt_module(text) {
        return out;
    }
    let mut in_const_assert = false;
    for (index, raw) in text.lines().enumerate() {
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
        for (needle, why) in DROP_NEEDLES {
            if line.contains(needle) {
                out.push(Violation {
                    file: name.to_string(),
                    line: index + 1,
                    rule: "D-01",
                    detail: format!("{}（{}）", needle, why),
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
