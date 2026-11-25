//! khaen-plan-levels CLI 集成测试：预测电平、确定性、错误码与退出码。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use khaen_core::plan::{MatrixSection, Plan, ProtectionEntry, ProtectionSection};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-plan-levels");
const RATE: u32 = 48_000;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-plan-levels-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn constant_gain_section(gain_db: f64) -> khaen_core::plan::PeqSection {
    khaen_core::plan::PeqSection {
        b0: 10.0f64.powf(gain_db / 20.0) as f32,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

fn entry(lane: u16, gain_db: f32, peq: Vec<khaen_core::plan::PeqSection>) -> ProtectionEntry {
    ProtectionEntry {
        lane,
        delay: 0,
        polarity: 1,
        gain_db,
        peq,
    }
}

fn fixture_plan() -> Plan {
    PlanBuilder::new(PlanConfig::new(128, RATE, 2))
        .set_content_identity(2)
        .set_crossover_identity(2)
        .set_matrix_identity(2)
        .set_protection(ProtectionSection {
            lanes: 2,
            entries: vec![
                entry(0, 0.0, vec![constant_gain_section(4.0)]),
                entry(1, -2.0, vec![]),
            ],
        })
        .set_routing_identity(2)
        .build()
        .expect("fixture plan")
}

fn write_plan(root: &Path) -> PathBuf {
    let path = root.join("plan.bin");
    let bytes = fixture_plan().encode_checked().expect("encode plan");
    std::fs::write(&path, bytes).expect("write plan");
    path
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let output = Command::new(BIN)
        .args(args)
        .output()
        .expect("spawn khaen-plan-levels");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn writes_deterministic_levels_for_every_lane() {
    let root = temp_dir("ok");
    let plan = write_plan(&root);
    let plan_path = plan.to_str().unwrap().to_string();

    let out1 = root.join("levels1.json");
    let first = run(&[
        "--plan",
        &plan_path,
        "--out",
        out1.to_str().unwrap(),
        "--centers",
        "40,50,63",
    ]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("\"ok\":true"));
    assert!(first.stdout.contains("\"tool\":\"khaen-plan-levels\""));
    assert!(first.stdout.contains("\"points\":2"));
    assert!(first.stdout.contains("\"bands\":3"));
    let text = std::fs::read_to_string(&out1).expect("levels file");
    assert!(text.contains("\"schema\":\"khaen-closed-loop-levels/v1\""));
    assert!(text.contains("\"name\":\"L00\""));
    assert!(text.contains("\"name\":\"L01\""));
    assert!(text.ends_with('\n'));

    // 同输入到另一个输出文件：逐字节相同（可复现 sha）。
    let out2 = root.join("levels2.json");
    let second = run(&[
        "--plan",
        &plan_path,
        "--out",
        out2.to_str().unwrap(),
        "--centers",
        "40,50,63",
    ]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        std::fs::read(&out1).expect("first"),
        std::fs::read(&out2).expect("second")
    );
    let sha_from = |stdout: &str| -> String {
        stdout
            .split("\"levels_sha256\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("sha")
            .to_string()
    };
    assert_eq!(sha_from(&first.stdout), sha_from(&second.stdout));
    assert_eq!(sha_from(&first.stdout).len(), 64);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn default_centers_cover_thirty_bands() {
    let root = temp_dir("defaults");
    let plan = write_plan(&root);
    let out = root.join("levels.json");
    let result = run(&[
        "--plan",
        plan.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("\"bands\":30"));
    let text = std::fs::read_to_string(&out).expect("levels");
    assert_eq!(text.matches("\"center_hz\"").count(), 30 * 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn usage_and_input_errors_exit_two() {
    assert_eq!(run(&[]).code, 2);
    assert!(run(&[]).stderr.contains("E_USAGE"));
    assert_eq!(run(&["--plan", "only-plan"]).code, 2, "--out is required");
    let root = temp_dir("errors");
    let plan = write_plan(&root);
    let plan_path = plan.to_str().unwrap().to_string();

    let missing_plan = run(&[
        "--plan",
        root.join("nope.bin").to_str().unwrap(),
        "--out",
        root.join("out.json").to_str().unwrap(),
    ]);
    assert_eq!(missing_plan.code, 2);
    assert!(missing_plan.stderr.contains("E_IO"));

    let garbage = root.join("garbage.bin");
    std::fs::write(&garbage, b"not a plan").expect("write garbage");
    let bad_plan = run(&[
        "--plan",
        garbage.to_str().unwrap(),
        "--out",
        root.join("out2.json").to_str().unwrap(),
    ]);
    assert_eq!(bad_plan.code, 2);
    assert!(bad_plan.stderr.contains("E_PLAN"));

    let bad_centers = run(&[
        "--plan",
        &plan_path,
        "--out",
        root.join("out3.json").to_str().unwrap(),
        "--centers",
        "40,abc",
    ]);
    assert_eq!(bad_centers.code, 2);
    assert!(bad_centers.stderr.contains("E_USAGE"));

    let zero_center = run(&[
        "--plan",
        &plan_path,
        "--out",
        root.join("out4.json").to_str().unwrap(),
        "--centers",
        "0",
    ]);
    assert_eq!(zero_center.code, 2);
    assert!(zero_center.stderr.contains("E_PLAN_LEVELS_INPUT"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn crossover_fir_plan_is_accepted_and_contributes() {
    let root = temp_dir("crossover");
    let plan = PlanBuilder::new(PlanConfig::new(128, RATE, 1))
        .set_content_identity(1)
        .set_crossover_filters(
            1,
            vec![khaen_core::plan::CrossoverFilter {
                lane: 0,
                coeffs: vec![2.0],
            }],
        )
        .set_matrix_identity(1)
        .set_protection(ProtectionSection {
            lanes: 1,
            entries: vec![entry(0, 0.0, vec![])],
        })
        .set_routing_identity(1)
        .build()
        .expect("plan");
    let path = root.join("plan.bin");
    std::fs::write(&path, plan.encode_checked().expect("encode")).expect("write");
    let out = root.join("levels.json");
    let result = run(&[
        "--plan",
        path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--centers",
        "100,1000",
    ]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let text = std::fs::read_to_string(&out).expect("levels");
    assert_eq!(text.matches("6.020").count(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn non_identity_matrix_plan_is_rejected() {
    let root = temp_dir("identity");
    let plan = PlanBuilder::new(PlanConfig::new(128, RATE, 1))
        .set_content_identity(1)
        .set_crossover_identity(1)
        .set_matrix(MatrixSection {
            rows: 1,
            cols: 1,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: vec![0.5],
        })
        .set_protection(ProtectionSection {
            lanes: 1,
            entries: vec![entry(0, 0.0, vec![])],
        })
        .set_routing_identity(1)
        .build()
        .expect("plan");
    let path = root.join("plan.bin");
    std::fs::write(&path, plan.encode_checked().expect("encode")).expect("write");
    let result = run(&[
        "--plan",
        path.to_str().unwrap(),
        "--out",
        root.join("levels.json").to_str().unwrap(),
    ]);
    assert_eq!(result.code, 2);
    assert!(result.stderr.contains("E_PLAN_LEVELS_PLAN"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn existing_output_file_is_refused() {
    let root = temp_dir("write");
    let plan = write_plan(&root);
    let out = root.join("levels.json");
    std::fs::write(&out, "old").expect("old file");
    let result = run(&[
        "--plan",
        plan.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 1);
    assert!(result.stderr.contains("E_WRITE"));
    let _ = std::fs::remove_dir_all(&root);
}
