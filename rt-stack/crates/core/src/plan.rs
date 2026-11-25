//! [NONRT] plan 构建/校验/hash（core-v1 §12.3 / §11.10）。
//!
//! T06 scaffold：类型与函数签名固定；实现归 T12/T13/T66（plan 打包、graph_id、
//! stale 校验）与 T26（集成）。**本模块不是 RT 路径**：可分配、可校验、可返回
//! owned PlanBuf；不得被 [RT] 调用图引用。

use crate::abi::{KhaenDiagFirV1, KhaenPathV1, KhaenPlanHeaderV1, KhaenRoleV1};
use crate::rt::{PlanVersion, RtStatus};

/// plan 的可变构建素材（NONRT 拥有）。
#[derive(Clone, Debug, Default)]
pub struct PlanTables {
    /// 路径表（按 (out_index, in_index) 升序）。
    pub paths: Vec<KhaenPathV1>,
    /// 对角 FIR 表（按 out_index 升序）。
    pub diags: Vec<KhaenDiagFirV1>,
    /// role 表（按 role_id 升序）。
    pub roles: Vec<KhaenRoleV1>,
}

/// 拥有 plan 字节的 NONRT 句柄（Drop 只发生在 CTL）。
#[derive(Clone, Debug)]
pub struct PlanBuf {
    bytes: Vec<u8>,
}

impl PlanBuf {
    /// 由已打包字节构造（调用者先经 validate_plan 校验）。
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// 原始字节。
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 字节数。
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// 取出底层字节（放弃本句柄）。
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// 已校验 plan 的拥有型 token（仅 NONRT；不得进入 RT 签名/字段/局部）。
#[derive(Clone, Debug)]
pub struct PlanToken {
    plan: PlanBuf,
    graph_id: [u8; 32],
}

impl PlanToken {
    /// 由已校验 plan + graph_id 构造（NONRT）。
    pub fn new(plan: PlanBuf, graph_id: [u8; 32]) -> Self {
        Self { plan, graph_id }
    }

    /// plan 字节。
    pub fn plan(&self) -> &PlanBuf {
        &self.plan
    }

    /// 规范化 graph_id（SHA-256 32 B）。
    pub fn graph_id(&self) -> [u8; 32] {
        self.graph_id
    }
}

/// 已 stage 的 plan 切换 token（仅 NONRT）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageToken {
    /// 待生效 plan 版本。
    pub version: PlanVersion,
}

/// 校验 plan 字节（schema/字段/长度/taps/rate/role/精度/hash）。
///
/// T06 scaffold：返回 ERR_UNSUPPORTED；T12/T13/T66 实现。
pub fn validate_plan(bytes: &[u8]) -> Result<PlanBuf, RtStatus> {
    let _ = bytes;
    Err(RtStatus::ERR_UNSUPPORTED)
}

/// 按 core-v1 §11.10 规则重算 graph_id（SHA-256）。
///
/// T06 scaffold：返回 ERR_UNSUPPORTED；T12 实现。
pub fn recompute_graph_id(bytes: &[u8]) -> Result<[u8; 32], RtStatus> {
    let _ = bytes;
    Err(RtStatus::ERR_UNSUPPORTED)
}

/// 打包 plan 字节（header + 表 + 系数 blob）。
///
/// T06 scaffold：返回 ERR_UNSUPPORTED；T12 实现。
pub fn pack_plan(
    header: &KhaenPlanHeaderV1,
    tables: &PlanTables,
    coeffs: &[f32],
) -> Result<Vec<u8>, RtStatus> {
    let _ = (header, tables, coeffs);
    Err(RtStatus::ERR_UNSUPPORTED)
}
