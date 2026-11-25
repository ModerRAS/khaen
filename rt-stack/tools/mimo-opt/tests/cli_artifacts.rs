//! mimo-opt CLI 计划 4.5 验收测试：
//! ① 同输入两次运行 sha256 完全一致；② 改任一影响结果的参数 result_sha256 必变；
//! ③ --verify 对篡改能报错并指出文件；④ 产物不含绝对路径/时间戳；
//! ⑤ CLI 冒烟（合成夹具，覆盖 quality/authority/safety 三组新参数）；
//! 另加：默认配置（不给任何新参数）的旧布局输出与改动前 HEAD 逐字节一致（摘要冻结）。

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCENE: &str = "scene-cli-smoke";
const MANIFEST_SCHEMA: &str = "khaen-mimo-artifact-manifest/v1";

/// 改动前 HEAD（cfec183）二进制在同一合成夹具上的旧布局输出（字节数 + sha256）。
/// 用于证明"未给任何新参数时行为逐位一致"。
const FROZEN_REPORT: (u64, &str) = (
    48760,
    "cb9eb83f694586696b7ba8bffc30a089d149a710c84a7ab31e4d296411febb19",
);
const FROZEN_METRICS: (u64, &str) = (
    8732,
    "6d74c28eeadb7c70f2ab6d3c9ef88f7064f035807ade4bca93d809d002b00cbe",
);
const FROZEN_B2_SUB1: (u64, &str) = (
    47386,
    "bbeaeb7689ba157c9d0858d36116b2083b4caef2b9da786f5d71a18da7c25502",
);
const FROZEN_B2_SUB2: (u64, &str) = (
    47327,
    "f67f6c5cf07de7088e7ca0360634325060594671139cf719b8fb2b778d7b37b3",
);
const FROZEN_SUB1: (u64, &str) = FROZEN_B2_SUB1;
const FROZEN_SUB2: (u64, &str) = FROZEN_B2_SUB2;

// ---------- 运行 / 读产物辅助 ----------

fn run_cli(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mimo-opt"))
        .args(args)
        .output()
        .expect("spawn mimo-opt")
}

fn base_args(fixtures: &Path, out: &Path) -> Vec<String> {
    vec![
        "--fixtures".to_string(),
        fixtures.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
        "--scene".to_string(),
        SCENE.to_string(),
    ]
}

fn push_extra(args: &mut Vec<String>, extra: &[&str]) {
    for item in extra {
        args.push((*item).to_string());
    }
}

fn run_ok(args: &[String], tag: &str) {
    let output = run_cli(args);
    assert!(
        output.status.success(),
        "[{}] mimo-opt 失败: status={:?} stderr={}",
        tag,
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 找到 <out>/run-<hash> 产物目录（一次运行只允许一个）。
fn find_run_dir(out: &Path) -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(out)
        .unwrap_or_else(|error| panic!("读取输出目录 {} 失败: {}", out.display(), error))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            if name.starts_with("run-") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    dirs.sort();
    assert_eq!(
        dirs.len(),
        1,
        "{} 下应恰好有一个 run-<hash> 产物目录，实际 {:?}",
        out.display(),
        dirs
    );
    dirs.pop().unwrap()
}

fn read_manifest(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("manifest.json"))
        .unwrap_or_else(|error| panic!("读取 {} 失败: {}", dir.display(), error))
}

/// 取 manifest 里某个字符串字段（manifest 是本工具 canonical JSON，格式固定）。
fn manifest_str(manifest: &str, key: &str) -> String {
    let needle = format!("\"{}\": \"", key);
    let start = manifest
        .find(&needle)
        .unwrap_or_else(|| panic!("manifest 缺少字段 {}", key))
        + needle.len();
    let rest = &manifest[start..];
    let end = rest.find('"').expect("字段值未闭合");
    rest[..end].to_string()
}

/// 目录下所有文件：相对路径（'/' 分隔）→ (sha256, bytes)。
fn collect_files(root: &Path) -> BTreeMap<String, (String, u64)> {
    fn walk(root: &Path, current: &Path, out: &mut BTreeMap<String, (String, u64)>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path).unwrap();
                out.insert(
                    relative,
                    (
                        khaen_room_sim::sha256::hex_digest(&bytes),
                        bytes.len() as u64,
                    ),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn artifact_manifest_files(manifest: &str) -> Vec<(String, String, u64)> {
    // 简单解析 files[]：本工具写出的 manifest 每个条目固定四行（path/role/sha256/bytes 排序后
    // 实际顺序为 bytes/path/role/sha256）。这里按行状态机提取，避免给测试加 JSON 依赖。
    let mut files = Vec::new();
    let mut path: Option<String> = None;
    let mut hash: Option<String> = None;
    let mut bytes: Option<u64> = None;
    for line in manifest.lines() {
        // canonical JSON：对象内字段按字典序，每个字段一行，字段间以 ',' 结尾（最后一行没有）
        let trimmed = line.trim().trim_end_matches(',');
        if let Some(rest) = trimmed.strip_prefix("\"path\": \"") {
            path = rest.strip_suffix('"').map(str::to_string);
        } else if let Some(rest) = trimmed.strip_prefix("\"sha256\": \"") {
            hash = rest.strip_suffix('"').map(str::to_string);
        } else if let Some(rest) = trimmed.strip_prefix("\"bytes\": ") {
            bytes = rest.parse::<u64>().ok();
        }
        if let (Some(path_ref), Some(hash_ref), Some(size)) = (path.as_ref(), hash.as_ref(), bytes)
        {
            files.push((path_ref.clone(), hash_ref.clone(), size));
            path = None;
            hash = None;
            bytes = None;
        }
    }
    files
}

// ---------- ① 确定性 ----------

#[test]
fn identical_inputs_and_params_produce_identical_sha256() {
    let scratch = common::scratch("determinism");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    let extras = ["--min-snr-db", "15", "--authority", "--safety"];

    let mut snapshots: Vec<(String, BTreeMap<String, (String, u64)>)> = Vec::new();
    let mut directed: Vec<PathBuf> = Vec::new();
    for tag in ["run-a", "run-b"] {
        let out = scratch.root.join(tag);
        let mut args = base_args(&fixtures, &out);
        push_extra(&mut args, &extras);
        run_ok(&args, tag);
        let dir = find_run_dir(&out);
        snapshots.push((
            dir.file_name().unwrap().to_string_lossy().to_string(),
            collect_files(&dir),
        ));
        directed.push(dir);
    }

    // 目录名（params_sha256 派生的 run-<hash>）必须相同，且不依赖输出目录 --out
    assert_eq!(
        snapshots[0].0, snapshots[1].0,
        "两次运行的 run-<hash> 目录名必须相同"
    );
    // 所有文件内容 sha256 完全相同
    assert_eq!(
        snapshots[0].1, snapshots[1].1,
        "同一输入 + 同一参数，两次运行的产物 sha256 必须完全一致"
    );
    // manifest.json 本身逐字节相同（不含时间戳/主机名/PID）
    let manifest_a = std::fs::read(directed[0].join("manifest.json")).unwrap();
    let manifest_b = std::fs::read(directed[1].join("manifest.json")).unwrap();
    assert_eq!(manifest_a, manifest_b, "manifest.json 必须逐字节相同");
    assert!(!snapshots[0].1.is_empty());

    // 产物语义自检：manifest schema + 至少一个 report + plan.present=false（本路径无 plan.bin）
    let manifest = read_manifest(&directed[0]);
    assert!(manifest.contains(&format!("\"schema\": \"{}\"", MANIFEST_SCHEMA)));
    assert!(
        manifest_str(&manifest, "params_sha256").len() == 64
            && manifest_str(&manifest, "params_sha256")
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "params_sha256 必须是 64 位十六进制"
    );
    assert!(
        manifest_str(&manifest, "result_sha256").len() == 64,
        "result_sha256 必须是 64 位十六进制"
    );
    assert!(
        manifest.contains("\"present\": false"),
        "本路径不应伪造 plan.bin"
    );
}

// ---------- ② 参数敏感性 ----------

#[test]
fn changing_result_affecting_parameter_changes_result_sha256() {
    let scratch = common::scratch("param-sensitivity");
    let (fixtures, _) = common::build_fixture(&scratch.root);

    let run = |tag: &str, extra: &[&str]| -> (String, String) {
        let out = scratch.root.join(tag);
        let mut args = base_args(&fixtures, &out);
        push_extra(&mut args, extra);
        run_ok(&args, tag);
        let manifest = read_manifest(&find_run_dir(&out));
        (
            manifest_str(&manifest, "params_sha256"),
            manifest_str(&manifest, "result_sha256"),
        )
    };

    let base = run("out-base", &[]);
    // 参数变（质量门门限）→ params_sha256 与 result_sha256 都必须变
    let quality = run("out-quality", &["--min-snr-db", "14"]);
    // 结果内容变（安全钳位给报告加 outcome/safety 块）→ result_sha256 必变
    let safety = run("out-safety", &["--safety"]);
    // 权威加权变 → 变
    let authority = run("out-authority", &["--authority"]);
    // 目标 beta 变 → 变（影响目标函数/报告数值）
    let beta = run("out-beta", &["--beta", "0.6"]);

    for (label, pair) in [
        ("quality", &quality),
        ("safety", &safety),
        ("authority", &authority),
        ("beta", &beta),
    ] {
        assert_ne!(
            base.1, pair.1,
            "参数/结果变化后 result_sha256 必须改变（{}）",
            label
        );
    }
    assert_ne!(
        base.0, quality.0,
        "质量门门限进了 params 快照，params_sha256 必须变"
    );
    assert_ne!(
        base.0, beta.0,
        "beta 进了 params 快照，params_sha256 必须变"
    );
}

// ---------- ③ --verify 篡改检测 ----------

#[test]
fn verify_accepts_valid_artifact_and_rejects_tampering() {
    let scratch = common::scratch("verify");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    let out = scratch.root.join("out");
    let mut args = base_args(&fixtures, &out);
    push_extra(&mut args, &["--safety"]);
    run_ok(&args, "verify-base");
    let dir = find_run_dir(&out);
    let dir_text = dir.display().to_string();

    // 正常校验
    let ok = run_cli(&["--verify".to_string(), dir_text.clone()]);
    assert!(
        ok.status.success(),
        "verify 合法产物必须通过: stderr={}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let stdout = String::from_utf8_lossy(&ok.stdout);
    assert!(stdout.contains("verify OK"), "stdout={}", stdout);

    // 篡改 report.json → 必须失败并点名 report.json
    let report_path = dir.join("report.json");
    let report_original = std::fs::read(&report_path).unwrap();
    let mut tampered = report_original.clone();
    tampered[0] = b'[';
    std::fs::write(&report_path, &tampered).unwrap();
    let bad = run_cli(&["--verify".to_string(), dir_text.clone()]);
    assert!(!bad.status.success(), "篡改 report.json 后 verify 必须失败");
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("report.json"),
        "失败信息必须指出哪个文件不符: stderr={}",
        stderr
    );
    std::fs::write(&report_path, &report_original).unwrap();

    // 篡改 manifest 的 result_sha256 → 必须失败并点名 result_sha256
    let manifest_path = dir.join("manifest.json");
    let manifest_original = std::fs::read_to_string(&manifest_path).unwrap();
    let result_sha = manifest_str(&manifest_original, "result_sha256");
    let manifest_tampered = manifest_original.replace(&result_sha, &"0".repeat(64));
    assert_ne!(manifest_tampered, manifest_original);
    std::fs::write(&manifest_path, &manifest_tampered).unwrap();
    let bad = run_cli(&["--verify".to_string(), dir_text.clone()]);
    assert!(!bad.status.success());
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("result_sha256"),
        "失败信息必须指出 result_sha256: stderr={}",
        stderr
    );
    std::fs::write(&manifest_path, &manifest_original).unwrap();

    // 多余文件（manifest 未声明）→ 必须失败并点名该文件
    let extra_path = dir.join("extra.txt");
    std::fs::write(&extra_path, b"tamper").unwrap();
    let bad = run_cli(&["--verify".to_string(), dir_text.clone()]);
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("extra.txt"),
        "stderr={}",
        String::from_utf8_lossy(&bad.stderr)
    );
    std::fs::remove_file(&extra_path).unwrap();

    // 缺文件 → 必须失败并点名该文件
    let fir_path = dir.join("fir").join("SUB1.csv");
    let fir_original = std::fs::read(&fir_path).unwrap();
    std::fs::remove_file(&fir_path).unwrap();
    let bad = run_cli(&["--verify".to_string(), dir_text.clone()]);
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("fir/SUB1.csv"),
        "stderr={}",
        String::from_utf8_lossy(&bad.stderr)
    );
    std::fs::write(&fir_path, &fir_original).unwrap();

    // 恢复后重新通过
    let ok = run_cli(&["--verify".to_string(), dir_text.clone()]);
    assert!(ok.status.success());
}

#[test]
fn verify_rejects_missing_manifest_and_conflicting_arguments() {
    let scratch = common::scratch("verify-errors");
    let missing = scratch.root.join("does-not-exist");
    let output = run_cli(&["--verify".to_string(), missing.display().to_string()]);
    assert!(!output.status.success(), "不存在的产物目录必须失败");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("manifest.json"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // --verify 与设计参数混用时 fail-closed
    let output = run_cli(&[
        "--verify".to_string(),
        scratch.root.display().to_string(),
        "--fixtures".to_string(),
        scratch.root.display().to_string(),
        "--out".to_string(),
        scratch.root.display().to_string(),
    ]);
    assert!(!output.status.success(), "--verify 不应接受设计参数");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--verify"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------- ④ 不含绝对路径 / 时间戳 ----------

fn contains_windows_absolute_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    for index in 0..bytes.len().saturating_sub(2) {
        if bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && (bytes[index + 2] == b'\\' || bytes[index + 2] == b'/')
        {
            return true;
        }
    }
    false
}

fn contains_unix_absolute_path(text: &str) -> bool {
    ["/home/", "/tmp/", "/Users/", "/root/", "/var/tmp/", "/opt/"]
        .iter()
        .any(|needle| text.contains(needle))
}

fn contains_iso_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    for index in 0..bytes.len().saturating_sub(16) {
        if bytes[index..index + 4]
            .iter()
            .all(|byte| byte.is_ascii_digit())
            && bytes[index + 4] == b'-'
            && bytes[index + 5..index + 7]
                .iter()
                .all(|byte| byte.is_ascii_digit())
            && bytes[index + 7] == b'-'
            && bytes[index + 8..index + 10]
                .iter()
                .all(|byte| byte.is_ascii_digit())
            && (bytes[index + 10] == b'T' || bytes[index + 10] == b' ')
            && bytes[index + 11..index + 13]
                .iter()
                .all(|byte| byte.is_ascii_digit())
            && bytes[index + 13] == b':'
        {
            return true;
        }
    }
    false
}

#[test]
fn artifacts_contain_no_absolute_paths_or_timestamps() {
    let scratch = common::scratch("no-abs-path");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    let out = scratch.root.join("out");
    let mut args = base_args(&fixtures, &out);
    push_extra(
        &mut args,
        &["--min-snr-db", "15", "--authority", "--safety"],
    );
    run_ok(&args, "no-abs-path");
    let dir = find_run_dir(&out);

    let scratch_text = scratch.root.to_string_lossy().to_string();
    let files = collect_files(&dir);
    assert!(
        files.len() >= 7,
        "产物文件数异常: {:?}",
        files.keys().collect::<Vec<_>>()
    );
    for relative in files.keys() {
        let path = dir.join(relative);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("产物 {} 不是 UTF-8 文本: {}", relative, error));
        assert!(
            !contains_windows_absolute_path(&text),
            "{} 含 Windows 绝对路径",
            relative
        );
        assert!(
            !contains_unix_absolute_path(&text),
            "{} 含 Unix 绝对路径",
            relative
        );
        assert!(!contains_iso_timestamp(&text), "{} 含 ISO 时间戳", relative);
        for forbidden in [
            "\"timestamp\"",
            "\"created_at\"",
            "\"generated_at\"",
            "\"hostname\"",
            "\"pid\"",
            "\"elapsed\"",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} 含禁止字段 {}",
                relative,
                forbidden
            );
        }
        // 运行时目录（含随机 scratch 名）绝不能进入产物
        if !scratch_text.is_empty() {
            assert!(
                !text.contains(&scratch_text),
                "{} 含运行时绝对路径 {}",
                relative,
                scratch_text
            );
            assert!(
                !text.contains(&scratch_text.replace('\\', "\\\\")),
                "{} 含运行时绝对路径（JSON 转义形式）",
                relative
            );
        }
    }
    // 时间戳/绝对路径之外，再确认 manifest 的目录都是相对路径
    let manifest = read_manifest(&dir);
    for (path, _, _) in artifact_manifest_files(&manifest) {
        assert!(
            !path.starts_with('/')
                && !path.contains('\\')
                && !path.contains(':')
                && !path.split('/').any(|segment| segment == ".."),
            "manifest 里的路径必须是相对安全路径: {:?}",
            path
        );
    }
}

// ---------- ⑤ 默认配置逐位一致（与改动前 HEAD 对比） ----------

#[test]
fn default_config_output_matches_pre_change_head_byte_for_byte() {
    let scratch = common::scratch("default-bytes");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    let out = scratch.root.join("out");
    let args = base_args(&fixtures, &out);
    run_ok(&args, "default");
    let scene_out = out.join(SCENE);

    for (relative, (expected_bytes, expected_sha)) in [
        ("report.json", FROZEN_REPORT),
        ("metrics.csv", FROZEN_METRICS),
        ("fir/B2_SUB1.csv", FROZEN_B2_SUB1),
        ("fir/B2_SUB2.csv", FROZEN_B2_SUB2),
        ("fir/SUB1.csv", FROZEN_SUB1),
        ("fir/SUB2.csv", FROZEN_SUB2),
    ] {
        let path = scene_out.join(relative);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("旧布局缺少 {}: {}", relative, error));
        assert_eq!(
            bytes.len() as u64,
            expected_bytes,
            "{} 字节数与改动前 HEAD 不一致（默认配置必须逐位一致）",
            relative
        );
        assert_eq!(
            khaen_room_sim::sha256::hex_digest(&bytes),
            expected_sha,
            "{} sha256 与改动前 HEAD 不一致（默认配置必须逐位一致）",
            relative
        );
    }

    // 旧布局的 report.json 与产物目录里的 report.json 必须逐字节相同（"现有报告保持兼容"）
    let dir = find_run_dir(&out);
    assert_eq!(
        std::fs::read(dir.join("report.json")).unwrap(),
        std::fs::read(scene_out.join("report.json")).unwrap(),
        "产物目录的 report.json 必须仍是现有报告的原始字节"
    );
    // 默认配置不出现 4.1/4.4 报告块（字段语义不变）
    let report_text = std::fs::read_to_string(scene_out.join("report.json")).unwrap();
    for top_level in ["\n  \"quality\"", "\n  \"authority\"", "\n  \"outcome\""] {
        assert!(
            !report_text.contains(top_level),
            "默认配置报告不应出现字段 {}",
            top_level
        );
    }
}

// ---------- ⑤ 冒烟：新参数全开 + 显式 --out-dir + 多场景布局 ----------

#[test]
fn full_feature_smoke_emits_self_consistent_artifact() {
    let scratch = common::scratch("smoke");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    let out = scratch.root.join("out");
    let artifacts = scratch.root.join("artifacts").join("explicit-run");
    let mut args = base_args(&fixtures, &out);
    push_extra(
        &mut args,
        &[
            "--min-snr-db",
            "15",
            "--authority",
            "--authority-eps-rel",
            "1e-6",
            "--authority-max-gamma",
            "100",
            "--safety",
            "--max-band-boost-db",
            "3",
            "--max-positive-sum-db",
            "6",
            "--max-predicted-peak-dbfs",
            "-1",
            "--out-dir",
            artifacts.to_str().unwrap(),
        ],
    );
    run_ok(&args, "smoke");

    // 三种新能力都在报告里留下块（且报告仍是原有 schema）
    let report_text = std::fs::read_to_string(out.join(SCENE).join("report.json")).unwrap();
    assert!(report_text.contains("khaen-mimo-sim-report/v1"));
    for block in ["\n  \"quality\"", "\n  \"authority\"", "\n  \"outcome\""] {
        assert!(
            report_text.contains(block),
            "启用 4.1/4.4 后报告应包含 {block}"
        );
    }
    assert!(report_text.contains("\"min_snr_db\""));
    assert!(report_text.contains("\"gamma_max\""));

    // 显式 --out-dir 生效：产物落在指定目录且可校验
    assert!(artifacts.join("manifest.json").is_file());
    assert!(artifacts.join("params.json").is_file());
    assert!(artifacts.join("report.json").is_file());
    // params.json 是完整输入快照：质量门证据、authority/safety 配置、派生输入都在里面
    let params_text = std::fs::read_to_string(artifacts.join("params.json")).expect("params.json");
    assert!(params_text
        .contains("\"quality_evidence_mode\": \"design_mic_ir_1/3_oct_snr_min_over_sources\""));
    assert!(params_text.contains("\"quality_evidence\""));
    assert!(params_text.contains("\"band_snr_db\""));
    assert!(params_text.contains("\"authority\""));
    assert!(params_text.contains("\"safety\""));
    assert!(params_text.contains("\"ground_truth_sha256\""));
    let manifest = read_manifest(&artifacts);
    let files = artifact_manifest_files(&manifest);
    // params + report + metrics + 4 个 FIR = 7
    assert_eq!(files.len(), 7, "files={:?}", files);
    for (path, hash, bytes) in &files {
        let actual = std::fs::read(artifacts.join(path)).unwrap();
        assert_eq!(actual.len() as u64, *bytes, "{} 字节数", path);
        assert_eq!(
            khaen_room_sim::sha256::hex_digest(&actual),
            *hash,
            "{} sha256",
            path
        );
    }
    let verify = run_cli(&["--verify".to_string(), artifacts.display().to_string()]);
    assert!(
        verify.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

#[test]
fn multi_scene_run_nests_reports_under_scenes_dir() {
    let scratch = common::scratch("multi-scene");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    common::clone_scene(&fixtures, "cli-second");
    let out = scratch.root.join("out");
    // 不传 --scene → 跑两个场景
    let args = vec![
        "--fixtures".to_string(),
        fixtures.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    run_ok(&args, "multi-scene");
    let dir = find_run_dir(&out);

    // 多场景：报告放在 scenes/<场景目录名>/ 下，根目录不再有 report.json
    assert!(!dir.join("report.json").exists());
    for scene in [SCENE, "scene-cli-second"] {
        assert!(
            dir.join("scenes").join(scene).join("report.json").is_file(),
            "缺少 scenes/{}/report.json",
            scene
        );
        assert!(dir.join("scenes").join(scene).join("metrics.csv").is_file());
        assert!(dir
            .join("scenes")
            .join(scene)
            .join("fir")
            .join("SUB1.csv")
            .is_file());
        // 旧布局仍然写 <out>/<scene>/
        assert!(out.join(scene).join("report.json").is_file());
    }
    // result 描述里两个报告都被绑定
    let manifest = read_manifest(&dir);
    assert!(manifest.contains("\"scene\": \"scene-cli-smoke\""));
    assert!(manifest.contains("\"scene\": \"scene-cli-second\""));
    let verify = run_cli(&["--verify".to_string(), dir.display().to_string()]);
    assert!(
        verify.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&verify.stderr)
    );
}
