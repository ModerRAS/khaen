//! T26 `rt-check host-file` 端到端 smoke（executed，非 RT）：真实 WAV -> T14 session/plan
//! -> 4-worker T09 DAG -> 真实 WAV，逐项断言输出帧/worker/block/报告 schema。

use tspi_rt_check::host_file::{make_sample, run_session};

#[test]
fn t26_host_file_e2e_wav_to_wav() {
    let base = std::env::temp_dir().join(format!("tspi-t26-host-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    make_sample(&base, 256).expect("sample session must build");
    let outcome = run_session(&base, "session.json", &base.join("output.wav"))
        .expect("host-file e2e must run");
    assert_eq!(outcome.workers, 4, "T26 4-core wiring");
    assert_eq!(outcome.block_size, 128);
    assert_eq!(outcome.input_frames, 256);
    assert_eq!(outcome.output_frames, 256);
    assert_eq!(outcome.decoded_frames, 256);
    assert_eq!(outcome.blocks, 2);
    assert_eq!(outcome.status, "ok");
    assert!(outcome.output_bytes > 0);
    assert!(outcome.runner_report.contains("tspi-file-runner-report/v1"));
    assert!(base.join("output.wav").is_file());
    let _ = std::fs::remove_dir_all(&base);
}
