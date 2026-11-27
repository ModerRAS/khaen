//! cal_solve：用最近一次测量算单点设计（FIR），返回 design JSON + 建议 plan 摘要。
//!
//! 只接当前已有能力（不复用旧 emperor 12k 专有逻辑）：
//! 1. T62/T61 反卷积 IR 已在测量阶段产出（khaen-calibration ir_extract）；
//! 2. 逐通道目标 FIR：khaen-calibration peq_fit::fit_target_fir_only（flat / tilt10）；
//! 3. 方案编译：khaen-calibration fir_plan::compile_fir_plan（crossover 承载 FIR）；
//! 4. 准入 + 图编译校验：khaen-host admission::admit + graph::PlanGraph::compile
//!    （用正在跑的运行时格式校验，设计不可编译就如实失败，不交付假方案）。
//!
//! 关键约束：**不提交、不改正在跑的输出**。产物写到测量 session 目录下的 solve/，
//! 由 daemon/操作者显式走 T70f-a 的 prepare/commit 才可能生效。

use std::fs;
use std::path::Path;

use khaen_calibration::fir_plan::{compile_fir_plan, FirPlanConfig, LaneFir};
use khaen_calibration::peq_fit::fit_target_fir_only;
use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::control::ControlError;
use khaen_host::engine::EngineConfig;
use khaen_host::graph::PlanGraph;
use khaen_host::plan::Plan;
use serde_json::{json, Value};

use super::{error, LastMeasurement};

/// 设计结果 schema。
pub const DESIGN_SCHEMA: &str = "khaen-rt-host.cal-design/v1";

/// 支持的目标曲线（peq_fit 内置目标；未知值明确拒绝，不静默降级）。
pub const ALLOWED_TARGETS: [&str; 2] = ["flat", "tilt10"];

fn solve_error(message: impl Into<String>) -> ControlError {
    ControlError::new(error::SOLVE_FAILED, message)
}

/// 用一次测量 + 当前运行时格式算设计。返回 design JSON（含 artifact 路径/sha256）。
pub fn solve_design(
    last: &LastMeasurement,
    format: &EngineConfig,
    current_plan: Option<&Plan>,
    target: &str,
) -> Result<Value, ControlError> {
    // 运行时格式：UMIK 48k、块长 64/128、通道数可表示。
    if format.sample_rate != khaen_calibration::stage_backend::STAGE_UMIK_SAMPLE_RATE_HZ {
        return Err(ControlError::new(
            error::FORMAT_UNSUPPORTED,
            format!(
                "cal_solve needs a 48000 Hz runtime format (measured IRs are 48k); the running format is {} Hz",
                format.sample_rate
            ),
        ));
    }
    if !khaen_core::rt::BLOCK_SIZES.contains(&(format.block_frames as u16)) {
        return Err(ControlError::new(
            error::FORMAT_UNSUPPORTED,
            format!(
                "cal_solve needs a runtime block size of {:?}; the running format uses {} frames",
                khaen_core::rt::BLOCK_SIZES,
                format.block_frames
            ),
        ));
    }
    let channels = u16::try_from(format.output_channels).map_err(|_| {
        ControlError::new(
            error::FORMAT_UNSUPPORTED,
            "runtime channel count does not fit the plan ABI (u16)",
        )
    })?;
    if last.irs.is_empty() {
        return Err(solve_error(
            "the measurement has no deconvolved IRs; nothing to design from",
        ));
    }
    // 逐通道目标 FIR（目标曲线由请求字段 target 指定；未知值已被调用方拒绝）。
    let mut lanes = Vec::with_capacity(last.irs.len());
    let mut lane_reports = Vec::with_capacity(last.irs.len());
    for ir in &last.irs {
        let lane = u16::try_from(ir.playback_channel).map_err(|_| {
            solve_error(format!(
                "IR channel {} does not fit a plan lane",
                ir.playback_channel
            ))
        })?;
        if u32::from(lane) >= format.output_channels as u32 {
            return Err(solve_error(format!(
                "IR channel {} is outside the running format ({} channels)",
                ir.playback_channel, format.output_channels
            )));
        }
        let fitted = fit_target_fir_only(&ir.ir, target).map_err(|error| {
            solve_error(format!(
                "target FIR design failed for {} (channel {}): {error}",
                ir.label, ir.playback_channel
            ))
        })?;
        lane_reports.push(json!({
            "label": ir.label,
            "slot": ir.slot,
            "lane": lane,
            "playback_channel": ir.playback_channel,
            "ir_samples": ir.ir.len(),
            "ir_peak_index": ir.peak_index,
            "repeats_averaged": ir.repeats_averaged,
            "taps": fitted.coeffs.len(),
            "discarded_peq_sections": fitted.discarded_sections,
        }));
        lanes.push(LaneFir {
            lane,
            taps: fitted.coeffs,
        });
    }
    let plan_config = FirPlanConfig::new(format.block_frames as u32, format.sample_rate, channels);
    let compiled = compile_fir_plan(&lanes, &plan_config).map_err(|error| {
        solve_error(format!(
            "FIR plan compilation failed ({}: {})",
            error.code(),
            error.message()
        ))
    })?;
    let limits = AdmissionLimits::default();
    let admission = admit(compiled.plan(), &limits);
    if !admission.accepted {
        return Err(solve_error(format!(
            "designed plan was rejected by admission: {}",
            admission.summary()
        )));
    }
    // 图编译校验：用正在跑的运行时格式（通道/块长/采样率）验证设计可执行。
    PlanGraph::compile(compiled.plan(), format, &limits).map_err(|error| {
        solve_error(format!(
            "designed plan does not compile against the running format: {error}"
        ))
    })?;

    let solve_dir = last.session_dir.join("solve");
    fs::create_dir_all(&solve_dir).map_err(|error| {
        ControlError::new(
            error::ARTIFACT_IO,
            format!("create solve dir {}: {error}", solve_dir.display()),
        )
    })?;
    let plan_path = solve_dir.join("plan.bin");
    fs::write(&plan_path, compiled.bytes()).map_err(|error| {
        ControlError::new(
            error::ARTIFACT_IO,
            format!("write {}: {error}", plan_path.display()),
        )
    })?;
    let plan_report = json!({
        "sha256": compiled.sha256(),
        "graph_id": compiled.graph_id(),
        "bytes": compiled.bytes().len(),
        "block_size": format.block_frames,
        "sample_rate": format.sample_rate,
        "channels": format.output_channels,
        "admission": {
            "accepted": admission.accepted,
            "summary": admission.summary(),
            "budget": khaen_host::control::budget_json(&admission.budget),
        },
        "graph_compile": "ok",
    });
    write_json(&solve_dir, "plan-report.json", &plan_report)?;
    let design = json!({
        "schema": DESIGN_SCHEMA,
        "ok": true,
        "applied": false,
        "verdict": "PREDICTED_ONLY",
        "measure_id": last.summary.measure_id,
        "pass": last.summary.pass,
        "session_id": last.summary.session_id,
        "simulated": last.summary.simulated,
        "mode": "single_point_fir",
        "layout": last.layout,
        "channels": last.channels,
        "target": target,
        "engine": last.summary.engine,
        "measurement_quality": last.summary.to_json(),
        "lanes": lane_reports,
        "plan": {
            "path": plan_path.display().to_string(),
            "report_path": solve_dir.join("plan-report.json").display().to_string(),
            "sha256": compiled.sha256(),
            "graph_id": compiled.graph_id(),
            "bytes": compiled.bytes().len(),
            "block_size": format.block_frames,
            "sample_rate": format.sample_rate,
            "channels": format.output_channels,
        },
        "admission": {
            "accepted": admission.accepted,
            "summary": admission.summary(),
        },
        "graph_compile": "ok",
        "current_plan": match current_plan {
            Some(plan) => json!({
                "present": true,
                "block_size": plan.block_size,
                "sample_rate": plan.sample_rate,
                "routing": plan.routing.as_ref().map(|routing| json!({
                    "dac_channels": routing.dac_channels,
                    "routes": routing.routes,
                })),
                "reasons": "design lanes follow the T61 playback channel numbers of the measurement (lane = measured DAC channel); if the running plan routes lanes to different DAC channels, remap lanes before applying. T57 layout resolver and multipoint design are not wired in this task, so the plan routing is reported for the operator, not re-derived",
            }),
            None => json!({
                "present": false,
                "reasons": "no plan is applied to the running engine; lanes are the measured playback channel numbers",
            }),
        },
        "apply": {
            "supported": false,
            "reason": "cal_solve never commits: load plan.bin through the T70f-a control channel (prepare + commit) when you decide to apply it; the running graph is untouched by this call",
        },
        "artifacts_dir": solve_dir.display().to_string(),
        "reasons": {
            "predicted_only": "the design is a FIR fit from one measurement; it is not a verified room correction, and no remeasure/holdout verification was run",
            "engine": if last.summary.simulated {
                json!("measurement came from the offline synthetic engine: this design only proves the wiring, it is not usable room correction")
            } else {
                Value::Null
            },
        },
    });
    write_json(&solve_dir, "design.json", &design)?;
    Ok(design)
}

fn write_json(dir: &Path, name: &str, value: &Value) -> Result<(), ControlError> {
    let path = dir.join(name);
    let text = serde_json::to_string_pretty(value).map_err(|error| {
        ControlError::new(
            error::ARTIFACT_IO,
            format!("serialize {}: {error}", path.display()),
        )
    })?;
    fs::write(&path, text).map_err(|error| {
        ControlError::new(
            error::ARTIFACT_IO,
            format!("write {}: {error}", path.display()),
        )
    })
}
