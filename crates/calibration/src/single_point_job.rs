//! 单点 artifact 包 → 校准 job 证据链（T66v4）。
//!
//! 把 [`PlanBundle`] 的每个文件写进 job 的 artifacts 目录并登记为 [`ArtifactRecord`]
//! （不可变语义由 job 负责：同路径不同内容拒收），随后可用 [`verify_registered`]
//! 从磁盘重新校验整包（抓篡改/缺失）。
//!
//! 只做打包/登记/校验：不跑 DSP、不碰设备。真实采集与 apply 仍属 v1 遗留（NOT_RUN）。

use crate::job::{ArtifactKind, ArtifactRecord, StageCtx, StageFailure};
use crate::plan_compile::PLAN_ARTIFACT_PATH;
use crate::single_point_bundle::{
    self, PlanBundle, MANIFEST_FILE_NAME, PARAMS_FILE_NAME, REPORT_FILE_NAME,
};

/// 稳定错误码（可被 REST / manifest 直接匹配，不要改字符串）。
pub mod error {
    /// 包为空（没有可登记的文件）。
    pub const EMPTY: &str = "E_SINGLE_POINT_BUNDLE_EMPTY";
    /// 包内出现未约定路径（防止把任意路径硬塞进 job 证据链）。
    pub const PATH: &str = "E_SINGLE_POINT_BUNDLE_PATH";
    /// 磁盘校验失败（缺失/内容不符/篡改）。
    pub const VERIFY: &str = "E_SINGLE_POINT_BUNDLE_VERIFY";
}

/// 包内文件 → job artifact 类别。
///
/// 机器名是稳定契约（进 `artifacts.manifest.json`）；新增包内文件必须先在这里登记，
/// 否则登记时按 `E_SINGLE_POINT_BUNDLE_PATH` 拒绝，避免证据链里出现"无类别"文件。
pub fn artifact_kind_for(path: &str) -> Option<ArtifactKind> {
    match path {
        PARAMS_FILE_NAME => Some(ArtifactKind::Other("single_point_params".to_string())),
        PLAN_ARTIFACT_PATH => Some(ArtifactKind::CalibrationDraft),
        REPORT_FILE_NAME => Some(ArtifactKind::Other("single_point_report".to_string())),
        _ => None,
    }
}

/// 把包内文件（外加 manifest 本身）写进 job artifacts 并登记。
///
/// 返回登记记录（顺序 = 包内文件顺序，最后是 manifest）；同一 job 重跑同内容幂等，
/// 内容变化由 job 的不可变语义拒收（`E_ARTIFACT_IMMUTABLE`）。
pub fn register_bundle(
    ctx: &StageCtx<'_>,
    bundle: &PlanBundle,
    version: &str,
) -> Result<Vec<ArtifactRecord>, StageFailure> {
    if bundle.files.is_empty() {
        return Err(StageFailure::failed(
            error::EMPTY,
            "single-point bundle has no files",
        ));
    }
    let mut records = Vec::with_capacity(bundle.files.len() + 1);
    for file in &bundle.files {
        let kind = artifact_kind_for(file.path).ok_or_else(|| {
            StageFailure::failed(
                error::PATH,
                format!("single-point bundle path is not registrable: {}", file.path),
            )
        })?;
        records.push(ctx.write_artifact(kind, file.path, &file.bytes, version)?);
    }
    records.push(ctx.write_artifact(
        ArtifactKind::Other("single_point_manifest".to_string()),
        MANIFEST_FILE_NAME,
        &bundle.manifest,
        version,
    )?);
    Ok(records)
}

/// 从 job 的 artifacts 目录重新校验整包（与 [`single_point_bundle::verify_bundle`] 同一口径）。
pub fn verify_registered(ctx: &StageCtx<'_>, bundle: &PlanBundle) -> Result<(), StageFailure> {
    single_point_bundle::verify_bundle(&ctx.artifacts_dir(), bundle).map_err(|err| {
        StageFailure::failed(
            error::VERIFY,
            format!(
                "single-point bundle verify failed: {}: {}",
                err.code(),
                err.message()
            ),
        )
    })
}
