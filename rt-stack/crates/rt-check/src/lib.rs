//! KHAEN 离线契约检查工具（crate khaen-rt-check；bin rt-check）——WBS T06。
//!
//! 子命令（core-v1 §12.2）：
//! - abi-layout：§11 结构 size/align/offset + §3 时限/容量向量（T06 已实现）
//! - forbid：扫描 RT-MODULE 文件的 F-01..F-44 禁止符号（T06 入口 + 正/负 fixture）
//! - drop-audit：§13 无 RT 析构类型清单 + RT 模块 Drop/owned 类型扫描（T06 入口 + fixture）
//! - plan-hash：NOT_IMPLEMENTED（T12/T13 实现 plan 打包/graph_id）
//! - state-machine：NOT_IMPLEMENTED（T21/T26 实现状态机模型测试）
//!
//! 工具只在控制区/离线运行，允许分配；不参与 RT 路径。

pub mod abi_layout;
pub mod drop_audit;
pub mod forbid;
/// T26 host file 端到端（WAV -> T14 plan -> 4-worker DAG -> WAV；host-file 子命令）。
pub mod host_file;

/// 工具版本（= Cargo package version）。
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 检查结果（NONRT）。
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// 扫描文件数（类型审计时为检查类型/条目数）。
    pub scanned: usize,
    /// 违规列表（空 = PASS）。
    pub violations: Vec<Violation>,
}

impl Report {
    /// 是否 PASS（无违规）。
    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }

    /// 合并另一份报告。
    pub fn merge(&mut self, other: Report) {
        self.scanned += other.scanned;
        self.violations.extend(other.violations);
    }

    /// 记录一条违规。
    pub fn push(&mut self, file: &str, line: usize, rule: &'static str, detail: String) {
        self.violations.push(Violation {
            file: file.to_string(),
            line,
            rule,
            detail,
        });
    }
}

/// 一条违规（文件/行/规则/说明）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 文件路径（类型审计时为类型名）。
    pub file: String,
    /// 1-based 行号（类型审计为 0）。
    pub line: usize,
    /// 规则 ID（F-xx / D-xx / B-02 等）。
    pub rule: &'static str,
    /// 命中的符号/原因。
    pub detail: String,
}
