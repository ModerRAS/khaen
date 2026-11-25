//! KHAEN 离线契约检查工具（crate khaen-rt-check；bin rt-check）——WBS T06。
//!
//! 子命令（core-v1 §12.2）：
//! - abi-layout：§11 结构 size/align/offset + §3 时限/容量向量（T06 已实现）
//! - forbid：扫描 RT-MODULE 文件的 F-01..F-44 禁止符号（T06 入口 + 正/负 fixture）
//! - drop-audit：§13 无 RT 析构类型清单 + RT 模块 Drop/owned 类型扫描（T06 入口 + fixture）
//! - plan-hash：NOT_IMPLEMENTED（v2 方案文件 ABI v3 的 plan 打包/graph_id 属于 core T10）
//! - state-machine：NOT_IMPLEMENTED（T21/T26 实现状态机模型测试）
//!
//! v2 迁移说明：
//! - BlockDesc 与通道/采样率等冻结契约常量直接引用 `khaen-core`（唯一来源，不在本 crate 镜像）；
//!   v1 plan 头 / ring / worker 统计 / rt-host 类型尚未进入 v2 公共 API，abi-layout / drop-audit
//!   暂时对 [`contract`] 里的 core-v1 v1.1.2 冻结镜像运行（TODO(core)：v2 对应类型落地后替换，
//!   预期值按 v2 契约重写）；
//! - legacy 的 `host-file`（T26 WAV -> session/plan -> 4-worker DAG -> WAV）子命令依赖
//!   khaen-file-runner，该 crate 不在 v2 workspace 内，故本 crate 不迁入该子命令。
//!
//! 工具只在控制区/离线运行，允许分配；不参与 RT 路径。

pub mod abi_layout;
/// v1.1.2 ABI/RT 契约镜像（TODO(core)：core 提供 ABI/RT 类型后删除）。
pub mod contract;
pub mod drop_audit;
pub mod forbid;

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
