//! runner 执行：core block DAG、IR/flush、identity 保护、确定性 `khaen-file-runner-report/v1` 元数据。

use khaen_file_runner::graph::GraphKind;
use khaen_file_runner::json::Json;
use khaen_file_runner::runner::{self, RunOptions};
use khaen_file_runner::session::SourceProvider;
use khaen_file_runner::sha256::sha256_hex;
use khaen_file_runner::wav::{decode_wav, encode_wav_f32};
use khaen_file_runner::{E_FILE_EXEC, FALLBACK_MUTE, REPORT_SCHEMA, TRIGGER_GRAPH_HASH_MISMATCH};

use crate::common::{
    ir_role_metadata, pcm16, protection, report_json, report_num, report_str, wav_container,
    Fixture,
};

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn output_bits(output: &[Vec<f32>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for channel in output {
        for value in channel {
            bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        }
    }
    bytes
}

#[test]
fn runner_identity_passthrough_exact() {
    let fixture = Fixture::identity(100);
    let result = fixture.run().expect("run");
    assert_eq!(result.status, "ok");
    assert!(!result.protected);
    assert_eq!(result.graph_kind, GraphKind::Normal);
    assert!(result.exec_error.is_none());
    assert_eq!(result.container_channels, 8);
    assert_eq!(result.sample_rate_hz, 48_000);
    assert_eq!(result.output_frames, 100);
    assert_eq!(result.output.len(), 8);
    // 容器 0/1 逐位等于输入；未使用容器为 +0.0。
    assert_eq!(result.output[0], fixture.input_planar[0..100].to_vec());
    assert_eq!(result.output[1], fixture.input_planar[100..200].to_vec());
    for channel in 2..8 {
        assert!(result.output[channel].iter().all(|value| *value == 0.0));
    }
    // 计数：1 块（100 <= 128），非 flush；全部有限。
    assert_eq!(result.counts.input_frames, 100);
    assert_eq!(result.counts.output_frames, 100);
    assert_eq!(result.counts.blocks_total, 1);
    assert_eq!(result.counts.flush_blocks, 0);
    assert!(result.counts.worker_tiles > 0);
    assert_eq!(result.counts.nonfinite_out, 0);
    assert_eq!(result.counts.invalid_blocks, 0);
}

#[test]
fn runner_flush_tail_exact_linear_phase() {
    let mut fixture = Fixture::identity(100);
    fixture.set_p("linear", &[0.0, 1.0, 0.0]);
    let prepared = fixture.prepare().expect("prepare");
    let graph = prepared.graph.as_ref().expect("graph");
    assert_eq!(graph.l_total, 3);
    assert_eq!(graph.flush_zeros, 2, "flush = L_F - 1");
    let result = runner::run(&prepared, &RunOptions::default()).expect("run");
    // 输出长度 = 输入 + flush。
    assert_eq!(result.output_frames, 102);
    let input = &fixture.input_planar[0..100];
    let output = &result.output[0];
    assert_eq!(output.len(), 102);
    assert_eq!(output[0], 0.0);
    assert_eq!(&output[1..101], input, "单次 1 sample 延迟（P=[0,1,0]）");
    assert_eq!(output[101], 0.0, "flush 尾零");
    // 块计数：1 块含 flush。
    assert_eq!(result.counts.blocks_total, 1);
    assert_eq!(result.counts.flush_blocks, 1);
}

#[test]
fn runner_flush_lengths_track_full_path_length() {
    for taps in [1usize, 3, 5] {
        let mut fixture = Fixture::identity(64);
        if taps > 1 {
            // min_fast 家族允许任意声明延迟；这里用终点冲激表示 (taps-1) 延迟。
            fixture.set_p("min_fast", &{
                let mut coeffs = vec![0.0f32; taps];
                coeffs[taps - 1] = 1.0;
                coeffs
            });
        }
        let prepared = fixture.prepare().expect("prepare");
        let graph = prepared.graph.as_ref().expect("graph");
        assert_eq!(graph.l_a, 1);
        assert_eq!(graph.l_g, 1);
        assert_eq!(graph.l_p, taps as u32);
        assert_eq!(graph.l_total, taps as u32);
        assert_eq!(graph.flush_zeros, taps as u32 - 1);
        let result = runner::run(&prepared, &RunOptions::default()).expect("run");
        assert_eq!(result.output_frames, 64 + taps - 1);
    }
}

#[test]
fn runner_report_is_deterministic_canonical_metadata() {
    let fixture = Fixture::identity(100);
    let first = fixture.run().expect("run 1");
    let second = fixture.run().expect("run 2");
    assert_eq!(first.metadata_json, second.metadata_json, "报告必须确定性");
    let report = report_json(&first);
    let canonical = report.to_canonical();
    assert_eq!(canonical, first.metadata_json, "报告必须是 canonical JSON");
    assert_eq!(report_str(&report, &["schema"]), REPORT_SCHEMA);
    assert_eq!(report_str(&report, &["status"]), "ok");
    assert_eq!(report_str(&report, &["precision"]), "f32-strict");
    assert_eq!(
        report_str(&report, &["graph_hash_algo"]),
        "sha256-canonical-v1"
    );
    assert_eq!(report_str(&report, &["graph_kind"]), "normal");
    assert_eq!(report_str(&report, &["runner", "name"]), "khaen-file-runner");
    assert_eq!(report_num(&report, &["engine", "abi_version"]), 1.0);
    assert_eq!(report_num(&report, &["engine", "plan_version"]), 1.0);
    assert_eq!(report_num(&report, &["engine", "block_size"]), 128.0);
    assert_eq!(report_num(&report, &["engine", "expected_workers"]), 4.0);
    assert_eq!(report_num(&report, &["lengths", "flush_zeros"]), 0.0);
    assert_eq!(report_str(&report, &["graph_id"]).len(), 64);
    assert!(report_str(&report, &["graph_id"])
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    // NOT_RUN 清单存在且如实记录，不冒充已验证。
    let not_verified = match report_json(&first)
        .as_object()
        .expect("object")
        .get("not_verified")
    {
        Some(Json::Array(entries)) => entries.clone(),
        other => panic!("not_verified: {:?}", other),
    };
    assert_eq!(not_verified.len(), khaen_file_runner::NOT_VERIFIED.len());
    let texts = not_verified
        .iter()
        .map(|entry| entry.as_str().expect("string").to_string())
        .collect::<Vec<_>>();
    assert!(texts.iter().any(|text| text.contains("board/ALSA/RT")));
    // 报告不含时间戳/耗时（确定性）。
    assert!(!canonical.contains("created_unix"));
    assert!(!canonical.contains("elapsed"));
}

#[test]
fn runner_report_records_input_output_and_fir_metadata() {
    let fixture = Fixture::identity(64);
    let result = fixture.run().expect("run");
    let report = report_json(&result);
    // 输入 SHA 与解码字段。
    let (_, source) = fixture.build();
    let wav_bytes = source.load("input.wav").expect("input wav");
    assert_eq!(
        report_str(&report, &["input", "sha256"]),
        sha256_hex(&wav_bytes)
    );
    assert_eq!(report_num(&report, &["input", "channels"]), 2.0);
    assert_eq!(report_num(&report, &["input", "frames"]), 64.0);
    assert_eq!(report_num(&report, &["input", "sample_rate_hz"]), 48_000.0);
    // 输出 SHA 是 raw f32 planar 位串的 hash。
    assert_eq!(
        report_str(&report, &["output", "sha256"]),
        sha256_hex(&output_bits(&result.output))
    );
    assert_eq!(report_num(&report, &["output", "frames"]), 64.0);
    assert_eq!(report_num(&report, &["output", "channels"]), 8.0);
    // FIR 绑定：2 A + 6 P，含文件 hash 与家族。
    let fir = match report.as_object().expect("object").get("fir") {
        Some(Json::Array(entries)) => entries.clone(),
        other => panic!("fir: {:?}", other),
    };
    assert_eq!(fir.len(), 8);
    let first = fir[0].as_object().expect("fir object");
    for key in [
        "kind",
        "file",
        "family",
        "taps",
        "fs",
        "delay_samples",
        "file_sha256",
        "manifest_sha256",
        "profile",
        "fits_budget",
    ] {
        assert!(first.contains_key(key), "fir binding must contain {key}");
    }
    // 保护未触发：engaged=false、reason=null、graph_kind=normal。
    let protection = match report.as_object().expect("object").get("protection") {
        Some(value) => value.clone(),
        None => panic!("protection missing"),
    };
    let object = protection.as_object().expect("protection object");
    assert_eq!(object.get("engaged"), Some(&Json::Bool(false)));
    assert_eq!(object.get("reason"), Some(&Json::Null));
    assert_eq!(
        object.get("graph_kind").and_then(Json::as_str),
        Some("normal")
    );
}

#[test]
fn runner_report_records_ir_binding_when_applied() {
    let mut fixture = Fixture::identity(32);
    fixture.ir = Some(ir_role_metadata(&[1, 0, 0, 0, 0, 0], &[false; 6]));
    let result = fixture.run().expect("run");
    let report = report_json(&result);
    let ir = match report.as_object().expect("object").get("ir") {
        Some(value) => value.clone(),
        None => panic!("ir missing"),
    };
    let ir_object = ir.as_object().expect("ir object");
    assert_eq!(ir_object.get("applied"), Some(&Json::Bool(true)));
    assert_eq!(
        ir_object.get("encoding").and_then(Json::as_str),
        Some("role_metadata")
    );
    assert_eq!(
        ir_object
            .get("ir_hash")
            .and_then(Json::as_str)
            .map(|s| s.len()),
        Some(64)
    );
    // 无 IR 时报告 ir = null。
    let plain = Fixture::identity(32).run().expect("run");
    let report = report_json(&plain);
    assert_eq!(
        report.as_object().expect("object").get("ir"),
        Some(&Json::Null)
    );
}

#[test]
fn runner_exec_fault_mutes_whole_output() {
    // 200 帧 + 128 块 -> 2 块；第 1 块后注入故障。
    let fixture = Fixture::identity(200);
    let options = RunOptions {
        inject_exec_fault_after_blocks: Some(1),
    };
    let result = fixture.run_with(&options).expect("muted run");
    assert_eq!(result.status, "muted");
    // 执行故障按 mute_on_exec_fault 安全静音；图本身仍是 normal（protected 只表示 prepare 期保护）。
    assert_eq!(result.graph_kind, GraphKind::Normal);
    assert!(!result.protected);
    let frames = result.output_frames;
    assert_eq!(frames, 200);
    for channel in &result.output {
        assert_eq!(channel.len(), frames);
        assert!(channel.iter().all(|value| *value == 0.0), "安全 mute 全零");
    }
    let error = result.exec_error.as_ref().expect("exec error recorded");
    assert!(error.contains(E_FILE_EXEC));
    let report = report_json(&result);
    assert_eq!(report_str(&report, &["status"]), "muted");
    assert!(report_str(&report, &["exec_error"]).contains(E_FILE_EXEC));
    assert_eq!(report_num(&report, &["counts", "nonfinite_out"]), 0.0);
    // 未注入故障时同图正常。
    let clean = fixture.run().expect("clean run");
    assert_eq!(clean.status, "ok");
}

#[test]
fn runner_exec_fault_without_mute_is_error() {
    let mut fixture = Fixture::identity(200);
    fixture.protection = Some(protection("identity_p_post", &[], false));
    let options = RunOptions {
        inject_exec_fault_after_blocks: Some(1),
    };
    let error = fixture.run_with(&options).expect_err("must error");
    assert_eq!(error.code(), E_FILE_EXEC);
}

#[test]
fn runner_mute_graph_emits_silence_with_input_frame_count() {
    let mut fixture = Fixture::identity(96);
    fixture.graph_id = Some(ZERO_HASH.to_string());
    fixture.protection = Some(protection(
        FALLBACK_MUTE,
        &[TRIGGER_GRAPH_HASH_MISMATCH],
        true,
    ));
    let result = fixture.run().expect("run muted graph");
    assert_eq!(result.graph_kind, GraphKind::ProtectionMute);
    assert_eq!(result.output_frames, 96, "mute 图无 flush 尾");
    for channel in &result.output {
        assert!(channel.iter().all(|value| *value == 0.0));
    }
    assert_eq!(result.counts.output_frames, 96);
}

#[test]
fn runner_protection_identity_p_post_is_passthrough() {
    let mut fixture = Fixture::identity(96);
    fixture.graph_id = Some(ZERO_HASH.to_string());
    let result = fixture.run().expect("protected run");
    assert_eq!(result.status, "protected");
    assert!(result.protected);
    assert_eq!(result.graph_kind, GraphKind::ProtectionIdentityPPost);
    assert_eq!(result.output_frames, 96, "identity fallback L_F=1 -> 无尾");
    assert_eq!(result.output[0], fixture.input_planar[0..96].to_vec());
    assert_eq!(result.output[1], fixture.input_planar[96..192].to_vec());
    let report = report_json(&result);
    let protection = match report.as_object().expect("object").get("protection") {
        Some(value) => value.clone(),
        None => panic!("protection missing"),
    };
    let object = protection.as_object().expect("object");
    assert_eq!(object.get("engaged"), Some(&Json::Bool(true)));
    assert_eq!(
        object.get("reason").and_then(Json::as_str),
        Some(TRIGGER_GRAPH_HASH_MISMATCH)
    );
    assert_eq!(
        object.get("fallback").and_then(Json::as_str),
        Some("identity_p_post")
    );
    assert_eq!(
        object.get("graph_kind").and_then(Json::as_str),
        Some("identity_p_post")
    );
}

#[test]
fn runner_block_size_invariance_same_output() {
    let mut base = Fixture::identity(200);
    base.set_p("linear", &[0.0, 1.0, 0.0]);
    let mut small = base.clone();
    small.block_size = 64;
    let run_base = base.run().expect("run 128");
    let run_small = small.run().expect("run 64");
    assert_eq!(
        run_base.output, run_small.output,
        "block 大小不改变 f32 结果"
    );
    // 但 block_size 进入图版本 -> graph_id 不同（版本完整）。
    assert_ne!(run_base.metadata_json, run_small.metadata_json);
    let hash_base = report_str(&report_json(&run_base), &["graph_id"]);
    let hash_small = report_str(&report_json(&run_small), &["graph_id"]);
    assert_ne!(hash_base, hash_small);
    // flush/块计数随块大小变化但输出帧数一致。
    assert_eq!(run_base.output_frames, 202);
    assert_eq!(run_small.output_frames, 202);
    assert_eq!(run_small.counts.blocks_total, 4);
}

#[test]
fn runner_output_wav_round_trip() {
    let fixture = Fixture::identity(96);
    let result = fixture.run().expect("run");
    let wav = result.output_wav_f32().expect("encode");
    let decoded = decode_wav(&wav).expect("decode");
    assert_eq!(decoded.channels(), 8);
    assert_eq!(decoded.frames(), 96);
    assert_eq!(decoded.info.sample_rate, 48_000);
    for (channel, expected) in result.output.iter().enumerate() {
        assert_eq!(
            decoded.channel(channel).expect("channel"),
            expected.as_slice()
        );
    }
    // 输出 WAV 可再次 encode 且逐字节一致（确定性）。
    let mut planar = Vec::new();
    for channel in &result.output {
        planar.extend_from_slice(channel);
    }
    let again = encode_wav_f32(&planar, 8, 48_000).expect("encode again");
    assert_eq!(wav, again);
}

#[test]
fn runner_ir_delay_and_polarity_applied() {
    let mut fixture = Fixture::identity(64);
    // FL 延迟 2、FR 延迟 1 且反相。
    fixture.ir = Some(ir_role_metadata(
        &[2, 1, 0, 0, 0, 0],
        &[false, true, false, false, false, false],
    ));
    let result = fixture.run().expect("run");
    assert_eq!(result.output_frames, 64 + 2, "L_F = 3");
    let input0 = &fixture.input_planar[0..64];
    let input1 = &fixture.input_planar[64..128];
    let fl = &result.output[0];
    let fr = &result.output[1];
    assert_eq!(fl[0], 0.0);
    assert_eq!(fl[1], 0.0);
    assert_eq!(&fl[2..66], input0);
    assert_eq!(fr[0], 0.0);
    let inverted: Vec<f32> = input1.iter().map(|value| -*value).collect();
    assert_eq!(&fr[1..65], &inverted[..]);
}

#[test]
fn runner_fixed_precision_pcm_input_bit_exact() {
    // PCM16 输入 -> 固定乘子转换为 f32，然后 identity 图逐位直通。
    let samples: Vec<i16> = vec![0, 1, -1, 16_384, -16_384, 32_767, -32_768, 123];
    let pcm_wav = wav_container(1, 16, 1, 48_000, &pcm16(&samples));
    let mut fixture = Fixture::identity(0);
    fixture.input_channels = 1;
    fixture.container_channels = 2;
    fixture.roles = vec![("FL".to_string(), Some(0), 0), ("FR".to_string(), None, 1)];
    fixture.participants = vec!["FL".to_string()];
    fixture.a = vec![(
        0,
        crate::common::nos("a0.raw").0,
        crate::common::nos("a0.raw").1,
    )];
    fixture.p = vec![
        (
            "FL".to_string(),
            crate::common::nos("p_FL.raw").0,
            crate::common::nos("p_FL.raw").1,
        ),
        (
            "FR".to_string(),
            crate::common::nos("p_FR.raw").0,
            crate::common::nos("p_FR.raw").1,
        ),
    ];
    fixture.set_frames(0);
    let (text, built) = fixture.build();
    let mut source = crate::common::MapSource::new();
    source.insert("input.wav", pcm_wav);
    for name in ["a0.raw", "p_FL.raw", "p_FR.raw"] {
        source.insert(name, built.load(name).expect("blob"));
    }
    let session = khaen_file_runner::session::parse_session(&text).expect("session");
    let prepared = khaen_file_runner::graph::prepare(session, &source).expect("prepare");
    let result = runner::run(&prepared, &RunOptions::default()).expect("run");
    // 解码后的 f32 位与输出位逐位一致（无隐式 dither/clip/归一化/格式转换）。
    let expected: Vec<f32> = samples
        .iter()
        .map(|value| *value as f32 / 32_768.0)
        .collect();
    assert_eq!(result.output[0], expected);
    assert_eq!(result.output[1], vec![0.0f32; 8]);
    // 报告 precision 锁为 f32-strict。
    let report = report_json(&result);
    assert_eq!(report_str(&report, &["precision"]), "f32-strict");
}

#[test]
fn runner_run_session_convenience_matches_prepare_plus_run() {
    let fixture = Fixture::identity(48);
    let (text, source) = fixture.build();
    let direct = runner::run_session(&text, &source, &RunOptions::default()).expect("run_session");
    let prepared = fixture.prepare().expect("prepare");
    let manual = runner::run(&prepared, &RunOptions::default()).expect("run");
    assert_eq!(direct.output, manual.output);
    assert_eq!(direct.metadata_json, manual.metadata_json);
}
