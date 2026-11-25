//! 控制协议测试（契约 B）：能力上报、块边界静音、错误码、行循环。

use std::io::Cursor;

use khaen_host::control::serve::serve_loop;
use khaen_host::control::{error, ControlState, NOT_WIRED_OPS, WIRED_OPS};
use khaen_host::EngineConfig;
use serde_json::Value;

fn state() -> ControlState {
    ControlState::new(EngineConfig {
        block_frames: 128,
        sample_rate: 96_000,
        channels: 2,
    })
    .expect("state")
}

fn call(state: &mut ControlState, request: &str) -> Value {
    serde_json::from_str(&state.handle_line(request)).expect("response json")
}

#[test]
fn status_reports_capabilities_honestly() {
    let mut state = state();
    let response = call(&mut state, r#"{"op":"status"}"#);
    assert_eq!(response["ok"], Value::Bool(true));
    assert_eq!(response["runtime_state"], Value::String("idle".into()));
    assert_eq!(response["blocks"], Value::from(0));

    let wired: Vec<&str> = response["wired"]
        .as_array()
        .expect("wired")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(wired, WIRED_OPS.to_vec());

    let not_wired = response["not_wired"].as_array().expect("not_wired");
    assert_eq!(not_wired.len(), NOT_WIRED_OPS.len());
    for entry in not_wired {
        assert!(entry["op"].is_string());
        assert!(
            entry["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "未实现的能力必须带原因"
        );
    }

    // 没有来源的字段必须是 null 并带原因，不允许编数。
    assert_eq!(response["graph_id"], Value::Null);
    assert!(response["reasons"]["graph_id"].is_string());
    assert_eq!(response["xruns"], Value::from(0));
    assert!(response["reasons"]["xruns"].is_string());
}

#[test]
fn mute_takes_effect_at_block_boundary_and_is_idempotent() {
    let mut state = state();
    let first = call(&mut state, r#"{"op":"mute","mute":true}"#);
    assert_eq!(first["ok"], Value::Bool(true));
    assert_eq!(first["applied_block"], Value::from(1));
    assert_eq!(first["idempotent"], Value::Bool(false));

    // 生效前 status 仍报告未静音（命令只是排到下个块）。
    let status = call(&mut state, r#"{"op":"status"}"#);
    assert_eq!(status["mute"], Value::Bool(false));

    let input = vec![0.25f32; 256];
    state.engine_mut().run_block(&input).expect("block");
    let status = call(&mut state, r#"{"op":"status"}"#);
    assert_eq!(status["mute"], Value::Bool(true));
    assert_eq!(status["silent"], Value::Bool(true));
    assert_eq!(status["blocks"], Value::from(1));

    let repeat = call(&mut state, r#"{"op":"mute","mute":true}"#);
    assert_eq!(repeat["idempotent"], Value::Bool(true));
    assert_eq!(repeat["applied_block"], Value::from(1));

    let unmute = call(&mut state, r#"{"op":"mute","mute":false}"#);
    assert_eq!(unmute["idempotent"], Value::Bool(false));
    assert_eq!(unmute["applied_block"], Value::from(2));
}

#[test]
fn not_wired_ops_return_unsupported_with_reason() {
    let mut state = state();
    for (op, _) in NOT_WIRED_OPS {
        let response = call(&mut state, &format!(r#"{{"op":"{op}"}}"#));
        assert_eq!(response["ok"], Value::Bool(false), "op {op}");
        assert_eq!(response["error"], Value::String(error::UNSUPPORTED.into()));
        assert!(
            response["message"]
                .as_str()
                .is_some_and(|message| message.contains("not wired")),
            "op {op} 必须说明未实现原因"
        );
    }
}

#[test]
fn unknown_and_malformed_requests_get_stable_codes() {
    let mut state = state();
    assert_eq!(
        call(&mut state, r#"{"op":"frobnicate"}"#)["error"],
        Value::String(error::UNKNOWN_OP.into())
    );
    assert_eq!(
        call(&mut state, "not json at all")["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    assert_eq!(
        call(&mut state, "{}")["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    assert_eq!(
        call(&mut state, r#"{"op":42}"#)["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    assert_eq!(
        call(&mut state, r#"{"op":"mute"}"#)["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    assert_eq!(
        call(&mut state, r#"{"op":"mute","mute":"yes"}"#)["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    assert_eq!(
        call(&mut state, "[]")["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    assert_eq!(
        call(&mut state, "")["error"],
        Value::String(error::BAD_REQUEST.into())
    );
    let huge = format!(r#"{{"op":"status","pad":"{}"}}"#, "x".repeat(70_000));
    assert_eq!(
        call(&mut state, &huge)["error"],
        Value::String(error::BAD_REQUEST.into())
    );
}

#[test]
fn null_backend_flips_runtime_state() {
    let mut state = state();
    let status = call(&mut state, r#"{"op":"status"}"#);
    assert_eq!(status["runtime_state"], Value::String("idle".into()));
    let _backend = state.open_null_backend().expect("backend");
    let status = call(&mut state, r#"{"op":"status"}"#);
    assert_eq!(status["runtime_state"], Value::String("running".into()));
}

#[test]
fn serve_loop_answers_one_line_per_request() {
    let mut state = state();
    let request = concat!(
        "{\"op\":\"status\"}\n",
        "{\"op\":\"mute\",\"mute\":true}\n",
        "{\"op\":\"nope\"}\n",
    );
    let mut reader = Cursor::new(request.as_bytes().to_vec());
    let mut writer: Vec<u8> = Vec::new();
    serve_loop(&mut state, &mut reader, &mut writer).expect("serve");
    let text = String::from_utf8(writer).expect("utf8");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "每个请求恰好一行响应");
    for line in &lines {
        let value: Value = serde_json::from_str(line).expect("json line");
        assert!(value.get("ok").is_some());
    }
    let third: Value = serde_json::from_str(lines[2]).expect("json");
    assert_eq!(third["error"], Value::String(error::UNKNOWN_OP.into()));
}
