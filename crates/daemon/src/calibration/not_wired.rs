//! 默认校准服务实现：后端未接线（NotWired）。
//!
//! daemon 的 M2 形态没有校准 job 执行后端（crates/calibration 的生产 backend 由并行任务
//! T70h-b 实现中）。本实现的语义只有一条：**如实报告未接线**——
//!
//! - GET /api/calibration/jobs 返回空列表（200），REST 层补 available=false + reason，
//!   前端显示 NOT_AVAILABLE，而不是收到一个假 job 或 500；
//! - 任何写操作 / 单 job 查询 / 产物下载 / 应用 / 回滚返回 503 `not_wired`；
//! - /api/status 的 calibration 三字段保持 null，并在 reasons/not_wired 里给原因。
//!
//! 上传（uploads）与向导配置（wizard）不依赖 job 后端，照常可用。

use super::service::{
    AppliedVersion, ApplyPlan, ArtifactInfo, CalibrationError, CalibrationService,
    CalibrationStatus, JobPage, JobQuery, JobRecord, NewJob, RollbackTarget,
};

/// 未接线原因（稳定文案；/api/status 的 reasons、503 错误体、meta 的 not_wired 共用）。
pub const NOT_WIRED_REASON: &str = "calibration job backend is not wired: crates/calibration has no production job backend in this daemon build yet (T70h-b); uploads and wizard configuration work, but jobs cannot start and no job state exists";

/// 默认（未接线）校准服务。
#[derive(Debug, Default, Clone, Copy)]
pub struct NotWiredCalibration;

impl NotWiredCalibration {
    /// 新建。
    pub fn new() -> Self {
        Self
    }

    fn not_wired<T>(&self) -> Result<T, CalibrationError> {
        Err(CalibrationError::not_wired(NOT_WIRED_REASON))
    }
}

impl CalibrationService for NotWiredCalibration {
    fn status(&self) -> Result<CalibrationStatus, CalibrationError> {
        Ok(CalibrationStatus {
            wired: false,
            engine: None,
            active_job: None,
            last_job_id: None,
            last_verdict: None,
            reason: Some(NOT_WIRED_REASON.to_string()),
        })
    }

    fn start_job(&self, _request: &NewJob) -> Result<JobRecord, CalibrationError> {
        self.not_wired()
    }

    fn list_jobs(&self, _query: &JobQuery) -> Result<JobPage, CalibrationError> {
        // 空列表不是“有 0 个 job”，而是“不知道有没有”的唯一诚实表达；REST 层会带
        // available=false + reason 让前端渲染 NOT_AVAILABLE。
        Ok(JobPage {
            jobs: Vec::new(),
            total: 0,
        })
    }

    fn job(&self, _job_id: &str) -> Result<JobRecord, CalibrationError> {
        self.not_wired()
    }

    fn cancel(&self, _job_id: &str) -> Result<JobRecord, CalibrationError> {
        self.not_wired()
    }

    fn artifacts(&self, _job_id: &str) -> Result<Vec<ArtifactInfo>, CalibrationError> {
        self.not_wired()
    }

    fn read_artifact(&self, _job_id: &str, _name: &str) -> Result<Vec<u8>, CalibrationError> {
        self.not_wired()
    }

    fn apply(&self, _job_id: &str) -> Result<ApplyPlan, CalibrationError> {
        self.not_wired()
    }

    fn apply_committed(
        &self,
        _job_id: &str,
        _applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError> {
        self.not_wired()
    }

    fn rollback(&self, _job_id: &str) -> Result<RollbackTarget, CalibrationError> {
        self.not_wired()
    }

    fn rollback_committed(
        &self,
        _job_id: &str,
        _applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError> {
        self.not_wired()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::service::{code, JOB_KIND_SINGLE_POINT_OFFLINE};
    use std::path::PathBuf;

    fn new_job() -> NewJob {
        NewJob {
            kind: JOB_KIND_SINGLE_POINT_OFFLINE.to_string(),
            inputs: crate::calibration::service::JobInputs {
                plan_sha: "0".repeat(64),
                baseline_upload_id: "1".repeat(64),
                applied_upload_id: "2".repeat(64),
                centers_hz: vec![100.0],
                tolerance_db: 2.0,
                min_bands: 1,
            },
            baseline: crate::calibration::service::UploadRef {
                upload_id: "1".repeat(64),
                sha256: "1".repeat(64),
                bytes: 1,
                path: PathBuf::from("a"),
            },
            applied: crate::calibration::service::UploadRef {
                upload_id: "2".repeat(64),
                sha256: "2".repeat(64),
                bytes: 2,
                path: PathBuf::from("b"),
            },
        }
    }

    #[test]
    fn status_is_unwired_with_reason_and_no_invented_values() {
        let service = NotWiredCalibration::new();
        let status = service.status().expect("status");
        assert!(!status.wired);
        assert!(status.active_job.is_none());
        assert!(status.last_job_id.is_none());
        assert!(status.last_verdict.is_none());
        assert!(status.engine.is_none());
        assert_eq!(status.reason.as_deref(), Some(NOT_WIRED_REASON));
        assert!(!service.wired());
    }

    #[test]
    fn list_is_empty_but_writes_are_503_not_wired() {
        let service = NotWiredCalibration::new();
        let page = service
            .list_jobs(&JobQuery {
                limit: 20,
                offset: 0,
            })
            .expect("list");
        assert!(page.jobs.is_empty());
        assert_eq!(page.total, 0);
        let cases: Vec<CalibrationError> = vec![
            service.start_job(&new_job()).expect_err("start"),
            service.job("job-0001").expect_err("job"),
            service.cancel("job-0001").expect_err("cancel"),
            service.artifacts("job-0001").expect_err("artifacts"),
            service
                .read_artifact("job-0001", "report.json")
                .expect_err("read"),
            service.apply("job-0001").expect_err("apply"),
            service.rollback("job-0001").expect_err("rollback"),
        ];
        for error in cases {
            assert_eq!(error.code(), code::NOT_WIRED);
            assert_eq!(error.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        }
    }
}
