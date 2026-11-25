//! `khaen-file-runner/v1` session manifest 解析/校验 + blob 源（MapSource/FsSource）。

use khaen_file_runner::ir::StaleReason;
use khaen_file_runner::json::Json;
use khaen_file_runner::session::{
    parse_session, parse_session_json, FsSource, MapSource, SourceProvider, MAX_CONTAINER_CHANNELS,
    MAX_INPUT_CHANNELS, MAX_PARTICIPANTS, MAX_ROLES, RUNTIME_BLOCK_SIZES, RUNTIME_SAMPLE_RATES,
};
use khaen_file_runner::{
    E_FILE_BLOB, E_FILE_CHANNELS, E_FILE_FIELD, E_FILE_GRAPH, E_FILE_IR, E_FILE_PATH_TABLE,
    E_FILE_PRECISION, E_FILE_PROFILE, E_FILE_RATE, E_FILE_ROLES, E_FILE_SCHEMA, E_FILE_VERSION,
};

use crate::common::{
    ir_role_metadata, map_mut, nested_mut, protection, protection_none, Fixture, HASH_A, HASH_B,
};

fn parse(fixture: &Fixture) -> Json {
    Json::parse(&fixture.session_text()).expect("fixture session parses as JSON")
}

fn with_root(mut json: Json, mutate: impl FnOnce(&mut Json)) -> Json {
    mutate(&mut json);
    json
}

fn err(fixture: &Fixture, mutate: impl FnOnce(&mut Json)) -> &'static str {
    let json = with_root(parse(fixture), mutate);
    parse_session_json(&json)
        .expect_err("must be rejected")
        .code()
}

#[test]
fn session_parse_valid_minimal() {
    let fixture = Fixture::identity(64);
    let session = parse_session(&fixture.session_text()).expect("valid session");
    assert_eq!(session.schema, "khaen-file-runner/v1");
    assert_eq!(session.precision, "f32-strict");
    assert_eq!(session.audio.sample_rate_hz, 48_000);
    assert_eq!(session.audio.container_channels, 8);
    assert_eq!(session.audio.input_channels, 2);
    assert_eq!(session.audio.layout_id, "4.1.2");
    assert_eq!(session.audio.input_file, "input.wav");
    assert_eq!(session.engine.abi_version, 1);
    assert_eq!(session.engine.plan_version, 1);
    assert_eq!(session.engine.block_size, 128);
    assert_eq!(session.engine.expected_workers, 4);
    assert_eq!(session.engine.profile, "regular");
    assert_eq!(session.engine.max_taps, 4096);
    assert!(!session.engine.extended_taps_admitted);
    assert_eq!(session.roles.len(), 6);
    assert_eq!(session.roles[0].name, "FL");
    assert_eq!(session.roles[0].src, Some(0));
    assert_eq!(session.roles[2].src, None);
    assert_eq!(session.participants.len(), 3);
    assert_eq!(session.d_samples, 0);
    assert_eq!(session.coefficients.len(), 2 + 0 + 6);
    assert!(session.ir.is_none());
    assert!(session.output.is_some());
    assert_eq!(
        session.output.as_ref().map(|o| o.format.as_str()),
        Some("wav_f32")
    );
    assert!(session.warnings.is_empty());
    // 运行时允许集合与 spec 常量一致。
    assert_eq!(RUNTIME_SAMPLE_RATES, [48_000, 96_000]);
    assert_eq!(RUNTIME_BLOCK_SIZES, [64, 128]);
    assert_eq!(MAX_CONTAINER_CHANNELS, 22);
    assert_eq!(MAX_INPUT_CHANNELS, 20);
    assert_eq!(MAX_ROLES, 16);
    assert_eq!(MAX_PARTICIPANTS, 8);
}

#[test]
fn session_rejects_schema_precision_and_versions() {
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert("schema".to_string(), Json::String("v2".to_string()));
        }),
        E_FILE_SCHEMA
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert("precision".to_string(), Json::String("f64".to_string()));
        }),
        E_FILE_PRECISION
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["engine"]))
                .insert("abi_version".to_string(), Json::Number(2.0));
        }),
        E_FILE_VERSION
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["engine"]))
                .insert("plan_version".to_string(), Json::Number(0.0));
        }),
        E_FILE_VERSION
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["engine"]))
                .insert("block_size".to_string(), Json::Number(32.0));
        }),
        E_FILE_VERSION
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["engine"]))
                .insert("expected_workers".to_string(), Json::Number(3.0));
        }),
        E_FILE_VERSION
    );
}

#[test]
fn session_unknown_fields_require_x_prefix() {
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert("bogus".to_string(), Json::Bool(true));
        }),
        E_FILE_SCHEMA
    );
    let json = with_root(parse(&fixture), |json| {
        map_mut(json).insert("x_vendor".to_string(), Json::String("v".to_string()));
    });
    let session = parse_session_json(&json).expect("x_ allowed");
    assert_eq!(session.extensions, vec!["x_vendor".to_string()]);
}

#[test]
fn session_rejects_audio_bounds() {
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"]))
                .insert("sample_rate_hz".to_string(), Json::Number(44_100.0));
        }),
        E_FILE_RATE
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"]))
                .insert("container_channels".to_string(), Json::Number(0.0));
        }),
        E_FILE_CHANNELS
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"]))
                .insert("container_channels".to_string(), Json::Number(23.0));
        }),
        E_FILE_CHANNELS
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"]))
                .insert("input_channels".to_string(), Json::Number(21.0));
        }),
        E_FILE_CHANNELS
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"]))
                .insert("layout_id".to_string(), Json::String(String::new()));
        }),
        E_FILE_SCHEMA
    );
}

#[test]
fn session_rejects_bad_role_table() {
    let fixture = Fixture::identity(16);
    // role index 必须等于数组位置（dense）。
    assert_eq!(
        err(&fixture, |json| {
            let roles = match nested_mut(json, &["roles"]) {
                Json::Array(entries) => entries,
                other => panic!("roles not array: {:?}", other),
            };
            map_mut(&mut roles[1]).insert("index".to_string(), Json::Number(0.0));
        }),
        E_FILE_ROLES
    );
    // scatter 必须单射。
    assert_eq!(
        err(&fixture, |json| {
            let roles = match nested_mut(json, &["roles"]) {
                Json::Array(entries) => entries,
                other => panic!("roles not array: {:?}", other),
            };
            map_mut(&mut roles[1]).insert("scatter".to_string(), Json::Number(0.0));
        }),
        E_FILE_ROLES
    );
    // scatter 必须在容器范围内。
    assert_eq!(
        err(&fixture, |json| {
            let roles = match nested_mut(json, &["roles"]) {
                Json::Array(entries) => entries,
                other => panic!("roles not array: {:?}", other),
            };
            map_mut(&mut roles[0]).insert("scatter".to_string(), Json::Number(8.0));
        }),
        E_FILE_ROLES
    );
    // A_map 一源只能被一个 role 选中。
    assert_eq!(
        err(&fixture, |json| {
            let roles = match nested_mut(json, &["roles"]) {
                Json::Array(entries) => entries,
                other => panic!("roles not array: {:?}", other),
            };
            map_mut(&mut roles[1]).insert("src".to_string(), Json::Number(0.0));
        }),
        E_FILE_ROLES
    );
    // src 超范围。
    assert_eq!(
        err(&fixture, |json| {
            let roles = match nested_mut(json, &["roles"]) {
                Json::Array(entries) => entries,
                other => panic!("roles not array: {:?}", other),
            };
            map_mut(&mut roles[0]).insert("src".to_string(), Json::Number(2.0));
        }),
        E_FILE_ROLES
    );
    // participants 必须在 roles 内。
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert(
                "participants".to_string(),
                Json::Array(vec![Json::String("XX".to_string())]),
            );
        }),
        E_FILE_ROLES
    );
}

#[test]
fn session_rejects_incomplete_coefficient_tables() {
    let fixture = Fixture::identity(16);
    // 少一个 A 源。
    assert_eq!(
        err(&fixture, |json| {
            let a = match nested_mut(json, &["coefficients", "a"]) {
                Json::Array(entries) => entries,
                other => panic!("a not array: {:?}", other),
            };
            a.pop();
        }),
        E_FILE_PATH_TABLE
    );
    // 少一个 P role。
    assert_eq!(
        err(&fixture, |json| {
            let p = match nested_mut(json, &["coefficients", "p"]) {
                Json::Array(entries) => entries,
                other => panic!("p not array: {:?}", other),
            };
            p.pop();
        }),
        E_FILE_PATH_TABLE
    );
    // G cell 必须在 participants 方阵内。
    assert_eq!(
        err(&fixture, |json| {
            let g = match nested_mut(json, &["coefficients", "g"]) {
                Json::Array(entries) => entries,
                other => panic!("g not array: {:?}", other),
            };
            g.push(Json::from_entries(vec![
                ("row", Json::Number(3.0)),
                ("col", Json::Number(0.0)),
                ("file", Json::String("g.raw".to_string())),
                ("manifest", Json::object()),
            ]));
        }),
        E_FILE_PATH_TABLE
    );
    // 槽位必须恰有一个 manifest / manifest_file。
    assert_eq!(
        err(&fixture, |json| {
            let a = match nested_mut(json, &["coefficients", "a"]) {
                Json::Array(entries) => entries,
                other => panic!("a not array: {:?}", other),
            };
            map_mut(&mut a[0]).insert(
                "manifest_file".to_string(),
                Json::String("m.json".to_string()),
            );
        }),
        E_FILE_FIELD
    );
    assert_eq!(
        err(&fixture, |json| {
            let a = match nested_mut(json, &["coefficients", "a"]) {
                Json::Array(entries) => entries,
                other => panic!("a not array: {:?}", other),
            };
            map_mut(&mut a[0]).remove("manifest");
        }),
        E_FILE_FIELD
    );
}

#[test]
fn session_rejects_bad_protection_and_output() {
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert(
                "protection".to_string(),
                protection("garbage", &["graph_hash_mismatch"], true),
            );
        }),
        E_FILE_SCHEMA
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert(
                "protection".to_string(),
                protection("identity_p_post", &["bogus"], true),
            );
        }),
        E_FILE_SCHEMA
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert(
                "protection".to_string(),
                Json::from_entries(vec![
                    ("fallback", Json::String("mute".to_string())),
                    ("mute_on_exec_fault", Json::String("yes".to_string())),
                ]),
            );
        }),
        E_FILE_SCHEMA
    );
    // 重复 trigger 去重。
    let json = with_root(parse(&fixture), |json| {
        map_mut(json).insert(
            "protection".to_string(),
            protection("mute", &["ir_stale", "ir_stale"], false),
        );
    });
    let session = parse_session_json(&json).expect("protection");
    let policy = session.protection;
    assert_eq!(policy.fallback, "mute");
    assert!(!policy.mute_on_exec_fault);
    assert_eq!(policy.triggers, vec!["ir_stale".to_string()]);
    // 输出格式只接受 wav_f32。
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["output"]))
                .insert("format".to_string(), Json::String("pcm_s16".to_string()));
        }),
        E_FILE_SCHEMA
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["output"])).remove("format");
        }),
        E_FILE_FIELD
    );
}

#[test]
fn session_rejects_bad_ir_object() {
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert(
                "ir".to_string(),
                Json::from_entries(vec![("schema", Json::String("other".to_string()))]),
            );
        }),
        E_FILE_IR
    );
    // 合法 IR 草案可解析进 session。
    let json = with_root(parse(&fixture), |json| {
        map_mut(json).insert("ir".to_string(), ir_role_metadata(&[0; 6], &[false; 6]));
    });
    let session = parse_session_json(&json).expect("ir ok");
    assert!(session.ir.is_some());
}

#[test]
fn session_rejects_bad_graph_id_and_binding() {
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["engine"]))
                .insert("graph_id".to_string(), Json::String("zz".to_string()));
        }),
        E_FILE_GRAPH
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"])).insert(
                "binding".to_string(),
                Json::from_entries(vec![
                    ("profile_sha256", Json::String("zz".to_string())),
                    ("calibration_sha256", Json::String(HASH_B.to_string())),
                ]),
            );
        }),
        E_FILE_GRAPH
    );
    assert_eq!(
        err(&fixture, |json| {
            map_mut(nested_mut(json, &["audio"])).insert(
                "binding".to_string(),
                Json::from_entries(vec![("profile_sha256", Json::String(HASH_A.to_string()))]),
            );
        }),
        E_FILE_FIELD
    );
    // profile/calibration/target 完整 binding 可解析。
    let json = with_root(parse(&fixture), |json| {
        map_mut(nested_mut(json, &["audio"])).insert(
            "binding".to_string(),
            Json::from_entries(vec![
                ("profile_sha256", Json::String(HASH_A.to_string())),
                ("calibration_sha256", Json::String(HASH_B.to_string())),
                ("target_sha256", Json::Null),
            ]),
        );
    });
    let session = parse_session_json(&json).expect("binding ok");
    assert!(session.audio.binding_present);
    assert_eq!(session.audio.profile_sha256.as_deref(), Some(HASH_A));
    assert_eq!(session.audio.calibration_sha256.as_deref(), Some(HASH_B));
    assert!(session.audio.target_sha256.is_none());
}

#[test]
fn session_ir_current_and_stale_when_binding_present() {
    let mut fixture = Fixture::identity(16);
    fixture.ir = Some(ir_role_metadata(&[0; 6], &[false; 6]));
    // 无 binding：只比较 fs/layout/roles；ir_current 需要 binding 才构造上下文。
    let session = parse_session(&fixture.session_text()).expect("session");
    assert!(session.ir_current().is_none());
    assert!(session.ir_stale_reasons().is_empty());
    // 提供 binding 后 profile/target/calibration 参与判定。
    fixture.binding = Some(Json::from_entries(vec![
        ("profile_sha256", Json::String(HASH_A.to_string())),
        ("calibration_sha256", Json::String(HASH_B.to_string())),
        ("target_sha256", Json::Null),
    ]));
    let session = parse_session(&fixture.session_text()).expect("session");
    let current = session.ir_current().expect("current");
    assert_eq!(current.fs_hz, 48_000);
    assert_eq!(current.layout_id.as_deref(), Some("4.1.2"));
    assert_eq!(current.profile_sha256, HASH_A);
    assert_eq!(current.calibration_sha256, HASH_B);
    assert!(session.ir_stale_reasons().is_empty());
    // 改 layout -> STALE_LAYOUT。
    fixture.binding = None;
    fixture.layout_id = "5.1".to_string();
    let session = parse_session(&fixture.session_text()).expect("session");
    assert_eq!(session.ir_stale_reasons(), vec![StaleReason::Layout]);
    fixture.layout_id = "4.1.2".to_string();
    // 改 profile hash -> STALE_PROFILE。
    fixture.binding = Some(Json::from_entries(vec![
        ("profile_sha256", Json::String(HASH_B.to_string())),
        ("calibration_sha256", Json::String(HASH_B.to_string())),
        ("target_sha256", Json::Null),
    ]));
    let session = parse_session(&fixture.session_text()).expect("session");
    assert_eq!(session.ir_stale_reasons(), vec![StaleReason::Profile]);
    // 改 target -> STALE_TARGET。
    fixture.binding = Some(Json::from_entries(vec![
        ("profile_sha256", Json::String(HASH_A.to_string())),
        ("calibration_sha256", Json::String(HASH_B.to_string())),
        ("target_sha256", Json::String(HASH_A.to_string())),
    ]));
    let session = parse_session(&fixture.session_text()).expect("session");
    assert_eq!(session.ir_stale_reasons(), vec![StaleReason::Target]);
    // 改 calibration -> STALE_CALIBRATION。
    fixture.binding = Some(Json::from_entries(vec![
        ("profile_sha256", Json::String(HASH_A.to_string())),
        ("calibration_sha256", Json::String(HASH_A.to_string())),
        ("target_sha256", Json::Null),
    ]));
    let session = parse_session(&fixture.session_text()).expect("session");
    assert_eq!(session.ir_stale_reasons(), vec![StaleReason::Calibration]);
    // 改 fs -> STALE_FS。
    fixture.fs = 96_000;
    fixture.set_frames(16);
    let mut session = parse_session(&fixture.session_text()).expect("session");
    assert!(session.ir_stale_reasons().contains(&StaleReason::Fs));
    let _ = &mut session;
}

#[test]
fn blob_sources_map_and_fs_contracts() {
    // MapSource：缺失引用 -> E_FILE_BLOB；覆盖写。
    let mut map = MapSource::new();
    assert!(map.is_empty());
    map.insert("a", vec![1u8]);
    map.insert("a", vec![2u8]);
    assert_eq!(map.len(), 1);
    assert_eq!(map.load("a").expect("load"), vec![2u8]);
    assert_eq!(
        map.load("missing").expect_err("missing").code(),
        E_FILE_BLOB
    );
    // FsSource：绝对路径/.. 逃逸拒绝；正常文件读取。
    // 用 cargo 提供的 target 临时目录（R:\Temp 在部分沙箱下不可写）。
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "khaen-t14-fs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("blob.bin"), b"payload").expect("write");
    let source = FsSource::new(&root);
    assert_eq!(source.load("blob.bin").expect("read"), b"payload".to_vec());
    assert_eq!(
        source
            .load("C:\\Windows\\win.ini")
            .expect_err("absolute")
            .code(),
        E_FILE_BLOB
    );
    assert_eq!(
        source.load("../escape").expect_err("dotdot").code(),
        E_FILE_BLOB
    );
    assert_eq!(
        source.load("missing.bin").expect_err("missing").code(),
        E_FILE_BLOB
    );
    std::fs::remove_dir_all(&root).expect("cleanup");
    // preparation 阶段缺失 manifest blob -> E_FILE_BLOB。
    let mut fixture = Fixture::identity(16);
    let (_, mut map_source) = fixture.build();
    fixture.a[0].1 = Json::object();
    let (text, _) = fixture.build();
    map_source.insert("input.wav", Vec::new());
    assert!(parse_session(&text).is_ok());
    // 缺少 A manifest blob 引用：把 inline manifest 改为 manifest_file 后 prepare。
    let mut fixture = Fixture::identity(16);
    fixture.a[0].1 = Json::object();
    let json = with_root(parse(&fixture), |json| {
        let a = match nested_mut(json, &["coefficients", "a"]) {
            Json::Array(entries) => entries,
            other => panic!("a not array: {:?}", other),
        };
        map_mut(&mut a[0]).remove("manifest");
        map_mut(&mut a[0]).insert(
            "manifest_file".to_string(),
            Json::String("missing_manifest.json".to_string()),
        );
    });
    let session = parse_session_json(&json).expect("session");
    let source = MapSource::new();
    let error = khaen_file_runner::graph::prepare(session, &source).expect_err("missing blob");
    assert_eq!(error.code(), E_FILE_BLOB);
}

#[test]
fn session_profile_and_taps_admission_rules() {
    let mut fixture = Fixture::identity(16);
    // regular 不能声明 >4096 taps。
    fixture.max_taps = 5000;
    assert_eq!(err(&fixture, |_json| {}), E_FILE_PROFILE);
    // extended profile + 显式准入才能 >4096 taps。
    fixture.extended_admitted = true;
    fixture.profile = "extended".to_string();
    let session = parse_session(&fixture.session_text()).expect("admitted");
    assert!(session.engine.extended_taps_admitted);
    assert_eq!(session.engine.max_taps, 5000);
    // profile 非法。
    let mut fixture = Fixture::identity(16);
    fixture.profile = "offline_only".to_string();
    assert_eq!(err(&fixture, |_json| {}), E_FILE_PROFILE);
    // expected_workers=1 仅测试/benchmark -> warning。
    let mut fixture = Fixture::identity(16);
    fixture.workers = 1;
    let session = parse_session(&fixture.session_text()).expect("workers 1");
    assert!(session
        .warnings
        .iter()
        .any(|warning| warning.contains("test/benchmark")));
    // d_samples 必须是 u32。
    let fixture = Fixture::identity(16);
    assert_eq!(
        err(&fixture, |json| {
            map_mut(json).insert("d_samples".to_string(), Json::Number(u32::MAX as f64 + 1.0));
        }),
        E_FILE_SCHEMA
    );
    // 保护策略全关的 session 可解析（负向保护测试用）。
    let json = with_root(parse(&Fixture::identity(16)), |json| {
        map_mut(json).insert("protection".to_string(), protection_none());
    });
    let session = parse_session_json(&json).expect("no protection");
    assert!(session.protection.triggers.is_empty());
}
