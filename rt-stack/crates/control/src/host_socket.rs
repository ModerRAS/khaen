//! daemon 把校准 REST 收成对 `tspi-rt-host` 的一行 Unix socket 请求。
//!
//! daemon 不打开声卡。socket 不在时返回 503，不造一条测量记录。

use std::time::{SystemTime, UNIX_EPOCH};

use tspi_dsp_core::rt::RtStatus;

use crate::rest::calibration_contract::host_stage_request;
use crate::rest::{
    CalEngineInfo, CalJobView, CalPortError, CalStartRequest, CalibrationPort, CAL_EXTRA_STAGES,
};

const JOB_ID: &str = "job-emperor";

/// 转发到 host socket 的校准 port。
pub struct HostSocketPort {
    sock: String,
    exchange_fn: fn(&str, &str) -> Result<String, String>,
    job: Option<CalJobView>,
}

impl HostSocketPort {
    pub fn new(sock: impl Into<String>) -> Self {
        Self {
            sock: sock.into(),
            exchange_fn: tspi_rt_host::cal_control::exchange,
            job: None,
        }
    }

    /// 单测替换连接函数。生产路径用 `new`。
    pub fn with_exchange(
        sock: impl Into<String>,
        exchange_fn: fn(&str, &str) -> Result<String, String>,
    ) -> Self {
        Self {
            sock: sock.into(),
            exchange_fn,
            job: None,
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }

    fn send(&self, request: &str) -> Result<String, CalPortError> {
        (self.exchange_fn)(&self.sock, request).map_err(CalPortError::Unavailable)
    }

    fn view(&self, state: &str, stage: Option<&str>, progress: f64) -> CalJobView {
        let now = Self::now();
        CalJobView {
            job_id: JOB_ID.to_string(),
            kind: "emperor".to_string(),
            state: state.to_string(),
            stage: stage.map(str::to_string),
            progress,
            applied_version: None,
            last_good_version: None,
            failure: None,
            created_unix: self.job.as_ref().map(|job| job.created_unix).unwrap_or(now),
            updated_unix: now,
        }
    }
}

impl CalibrationPort for HostSocketPort {
    fn start(&mut self, request: &CalStartRequest) -> Result<CalJobView, CalPortError> {
        if request.kind != "emperor" {
            return Err(CalPortError::Rejected(RtStatus::ERR_DEVICE_PARAMS));
        }
        let op = request.stage.as_deref().unwrap_or("enter");
        let extra_stage = CAL_EXTRA_STAGES.contains(&op);
        let response = if extra_stage {
            // level/delay/bass：唯一契约构造器（含 mic/cal_file/bass 字段）；host 侧结果
            // 还没最终落地 -> 只有回执带 lanes 才接受，否则明确 501 pending_host_field。
            let request_json = host_stage_request(
                op,
                request.audible_sweep_authorized,
                request.f_hi.unwrap_or(150.0),
                request.target.as_deref().unwrap_or("flat"),
                &request.supports,
                request.settings.as_ref(),
                &request.lanes,
                request.reference_lane.as_deref(),
                request.target_dbfs,
                Some(JOB_ID),
            );
            let response = self.send(&request_json)?;
            if !response.contains("\"ok\":true") || !response.contains("\"lanes\"") {
                return Err(CalPortError::PendingHostField(format!(
                    "host accepted no {op} per-lane result yet (pending host fields mic.device/mic.gain_db/{op}.lanes); refusing to fake progress"
                )));
            }
            response
        } else {
            let audible = if request.audible_sweep_authorized {
                "true"
            } else {
                "false"
            };
            let f_hi = request.f_hi.unwrap_or(150.0);
            let target = request.target.as_deref().unwrap_or("flat");
            let supports = if request.supports.is_empty() {
                "[]".to_string()
            } else {
                format!(
                    "[{}]",
                    request
                        .supports
                        .iter()
                        .map(|name| format!("\"{name}\""))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            let response = self.send(&format!(
                r#"{{"op":"{op}","audible":{audible},"f_hi":{f_hi},"target":"{target}","supports":{supports}}}"#
            ))?;
            if !response.contains("\"ok\":true") {
                return Err(CalPortError::Rejected(RtStatus::ERR_DEVICE_PARAMS));
            }
            response
        };
        let _ = response;
        let (state, stage) = match op {
            "measure" => ("sweep_capture", "sweep_capture"),
            "solve" => ("solve_g", "solve_g"),
            "remeasure" => ("remeasure", "remeasure"),
            "level" | "delay" | "bass" => ("sweep_capture", op),
            _ => ("preflight", "preflight"),
        };
        let job = self.view(state, Some(stage), 0.2);
        self.job = Some(job.clone());
        Ok(job)
    }

    fn get(&mut self, job_id: &str) -> Option<CalJobView> {
        self.job
            .as_ref()
            .filter(|job| job.job_id == job_id)
            .cloned()
    }

    fn list(&mut self) -> Vec<CalJobView> {
        self.job.iter().cloned().collect()
    }

    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if self.job.as_ref().map(|job| job.job_id.as_str()) != Some(job_id) {
            return Err(CalPortError::NotFound);
        }
        self.send(r#"{"op":"leave","audible":false}"#)?;
        let job = self.view("cancelled", None, 1.0);
        self.job = Some(job.clone());
        Ok(job)
    }

    fn apply(&mut self, _job_id: &str) -> Result<CalJobView, CalPortError> {
        Err(CalPortError::Unavailable(
            "this round does not apply a plan through the daemon; leave restores the program input"
                .to_string(),
        ))
    }

    fn retest(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if self.job.as_ref().map(|job| job.job_id.as_str()) != Some(job_id) {
            return Err(CalPortError::NotFound);
        }
        let response = self.send(
            r#"{"op":"remeasure","audible":true,"f_hi":150,"target":"flat","supports":[]}"#,
        )?;
        if !response.contains("\"ok\":true") {
            return Err(CalPortError::Rejected(RtStatus::ERR_DEVICE_PARAMS));
        }
        let job = self.view("remeasure", Some("remeasure"), 0.8);
        self.job = Some(job.clone());
        Ok(job)
    }

    fn rollback(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if self.job.as_ref().map(|job| job.job_id.as_str()) != Some(job_id) {
            return Err(CalPortError::NotFound);
        }
        self.send(r#"{"op":"leave","audible":false}"#)?;
        let mut job = self.view("rolled_back", None, 1.0);
        job.last_good_version = Some("previous-capture".to_string());
        self.job = Some(job.clone());
        Ok(job)
    }

    fn engine_info(&self) -> CalEngineInfo {
        CalEngineInfo {
            engine: "host-socket",
            synthetic: true,
            backend: "unix-socket",
            real_audio: false,
            note: "requests go to tspi-rt-host; synthetic stays true until a recorded room measurement exists",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refuse_connect(_: &str, _: &str) -> Result<String, String> {
        Err("socket down".to_string())
    }

    fn accept_enter(_: &str, request: &str) -> Result<String, String> {
        if request.contains("\"op\":\"enter\"") {
            Ok(r#"{"ok":true,"verdict":"PREDICTED_ONLY","synthetic":true}"#.to_string())
        } else {
            Err("unexpected".to_string())
        }
    }

    fn emperor(audible: bool) -> CalStartRequest {
        CalStartRequest {
            kind: "emperor".to_string(),
            label: None,
            auto_apply: false,
            audible_sweep_authorized: audible,
            stage: None,
            f_hi: None,
            supports: Vec::new(),
            target: None,
            settings: None,
            lanes: Vec::new(),
            reference_lane: None,
            target_dbfs: None,
        }
    }

    #[test]
    fn missing_socket_does_not_invent_a_job() {
        let mut port = HostSocketPort::with_exchange("/tmp/tspi-host-missing.sock", refuse_connect);
        let error = port.start(&emperor(true)).expect_err("socket down");
        assert!(matches!(error, CalPortError::Unavailable(_)), "{error:?}");
        assert!(port.list().is_empty());
    }

    #[test]
    fn full_kind_is_rejected_before_a_socket_call() {
        let mut port = HostSocketPort::with_exchange("/tmp/tspi-host-missing.sock", accept_enter);
        let error = port
            .start(&CalStartRequest {
                kind: "full".to_string(),
                label: None,
                auto_apply: false,
                audible_sweep_authorized: true,
                stage: None,
                f_hi: None,
                supports: Vec::new(),
                target: None,
                settings: None,
                lanes: Vec::new(),
                reference_lane: None,
                target_dbfs: None,
            })
            .expect_err("full is not this round");
        assert!(matches!(error, CalPortError::Rejected(_)), "{error:?}");
        assert!(port.list().is_empty());
    }

    #[test]
    fn emperor_enter_uses_the_socket_response() {
        let mut port = HostSocketPort::with_exchange("/tmp/tspi-host.sock", accept_enter);
        let job = port.start(&emperor(true)).expect("enter");
        assert_eq!(job.kind, "emperor");
        assert_eq!(job.state, "preflight");
        assert!(port.engine_info().synthetic);
    }
}
