//! 防漂移测试：Web 内嵌 layout-presets 必须与板端部署文件一致。
//!
//! - Web 内嵌副本：web/assets/layout-presets.yml（编译期 include_str! 进二进制，WebUI 读它）。
//! - 板端部署真相：audio/overlay/etc/tspi-audio/layout-presets.yml，安装为 /etc/tspi-audio/layout-presets.yml，
//!   由板端 Python tspi-channelmap 渲染器读取；两边 role→outputs 不一致会造成音源接错箱（如 4.1.2 的
//!   SUB1=7 / HFL=4 / HFR=5；旧表 SUB1=4 会把低频打进高度箱且 SUB 静音）。
//!
//! 默认按仓库相对路径找部署文件（web crate 的 ../audio/...）；只打包 web/ 的场景可用
//! TSPI_DEPLOY_LAYOUT_PRESETS=/path/to/layout-presets.yml 覆盖。文件缺失/解析失败/任一映射不一致
//! 都直接 panic（fail closed），保证 cargo test 拦住漂移。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_yaml::Value;

const DEPLOY_REL: &str = "../audio/overlay/etc/tspi-audio/layout-presets.yml";
const PRESETS_SCHEMA: &str = "tspi-layout-presets/v2";

fn deploy_path() -> PathBuf {
    match std::env::var("TSPI_DEPLOY_LAYOUT_PRESETS") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEPLOY_REL),
    }
}

fn web_asset_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/layout-presets.yml")
}

fn load(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "读取 layout-presets 失败：{}（{e}）。防漂移测试需要仓库完整布局；\
             如只打包 web/，请设置 TSPI_DEPLOY_LAYOUT_PRESETS 指向板端部署文件。",
            path.display()
        )
    });
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("YAML 解析失败 {}：{e}", path.display()))
}

fn map_get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_mapping().and_then(|m| m.get(Value::from(key)))
}

fn as_str(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn as_u64s(v: Option<&Value>) -> Vec<u64> {
    match v {
        Some(Value::Sequence(seq)) => seq.iter().filter_map(Value::as_u64).collect(),
        Some(Value::Number(n)) => n.as_u64().into_iter().collect(),
        _ => Vec::new(),
    }
}

fn entries(v: Option<&Value>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if let Some(Value::Mapping(m)) = v {
        for (k, val) in m {
            if let Some(k) = k.as_str() {
                out.insert(k.to_string(), val.clone());
            }
        }
    }
    out
}

/// 输出布局：id=outputs 按 roles 顺序拼接。
fn output_snapshot(doc: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (layout, def) in entries(map_get(doc, "output_layouts")) {
        let roles = map_get(&def, "roles")
            .and_then(Value::as_sequence)
            .map(|seq| {
                seq.iter()
                    .map(|r| {
                        let id = as_str(map_get(r, "id"));
                        let outs = as_u64s(map_get(r, "outputs"));
                        let joined = outs
                            .iter()
                            .map(u64::to_string)
                            .collect::<Vec<_>>()
                            .join(",");
                        format!("{id}={joined}")
                    })
                    .collect::<Vec<_>>()
                    .join(";")
            })
            .unwrap_or_default();
        out.insert(format!("output:{layout}"), roles);
    }
    out
}

/// 输入布局：channels/streams + id@channel/stream（按 roles 顺序）。
fn input_snapshot(doc: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (layout, def) in entries(map_get(doc, "input_layouts")) {
        let channels = as_str(map_get(&def, "channels"));
        let streams = as_u64s(map_get(&def, "streams"))
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let roles = map_get(&def, "roles")
            .and_then(Value::as_sequence)
            .map(|seq| {
                seq.iter()
                    .map(|r| {
                        let id = as_str(map_get(r, "id"));
                        let ch = map_get(r, "channel")
                            .and_then(Value::as_u64)
                            .unwrap_or_default();
                        let st = map_get(r, "stream")
                            .and_then(Value::as_u64)
                            .unwrap_or_default();
                        format!("{id}@{ch}/{st}")
                    })
                    .collect::<Vec<_>>()
                    .join(";")
            })
            .unwrap_or_default();
        out.insert(
            format!("input:{layout}"),
            format!("channels={channels};streams={streams};roles={roles}"),
        );
    }
    out
}

/// remap preset：键 + 规范化 JSON（input/output/routes 等任何字段差异都算漂移）。
fn remap_snapshot(doc: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (key, def) in entries(map_get(doc, "remap_presets")) {
        let json = serde_json::to_value(&def).unwrap_or(serde_json::Value::Null);
        out.insert(
            format!("remap:{key}"),
            serde_json::to_string(&json).unwrap_or_default(),
        );
    }
    out
}

fn snapshot(doc: &Value) -> BTreeMap<String, String> {
    let mut out = output_snapshot(doc);
    out.extend(input_snapshot(doc));
    out.extend(remap_snapshot(doc));
    out
}

fn schema_of(doc: &Value) -> String {
    as_str(map_get(doc, "schema"))
}

#[test]
fn web_layout_presets_match_deployed_authoritative_file() {
    let web = load(&web_asset_path());
    let deploy = load(&deploy_path());
    assert_eq!(
        schema_of(&web),
        PRESETS_SCHEMA,
        "web/assets/layout-presets.yml schema"
    );
    assert_eq!(
        schema_of(&deploy),
        PRESETS_SCHEMA,
        "板端部署 layout-presets schema"
    );

    let a = snapshot(&web);
    let b = snapshot(&deploy);
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut diffs = Vec::new();
    for key in keys {
        let wa = a.get(key);
        let wb = b.get(key);
        if wa != wb {
            diffs.push(format!(
                "- {key}\n    web   : {}\n    deploy: {}",
                wa.map(String::as_str).unwrap_or("<缺失>"),
                wb.map(String::as_str).unwrap_or("<缺失>")
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "web/assets/layout-presets.yml 与板端部署文件（{}）漂移（{} 处）：\n{}\n\
         修复：以 audio/overlay/etc/tspi-audio/layout-presets.yml 为准更新 web 内嵌副本，\
         或在 web/docs 说明有意的差异。",
        deploy_path().display(),
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn four_one_two_mapping_is_board_truth() {
    // 板端 MOTU 22ch（48k）映射：FL/FR=1-2、SL/SR=3-4、HFL/HFR=5-6、SUB1=8（即 ch0/1/2/3/4/5/7，ch6 空）。
    // 旧表 SUB1=4/HFL=5/HFR=6 会把 20–300 Hz 打进 8010 高度箱、SUB 静音。
    let expected: Vec<(String, Vec<u64>)> = vec![
        ("FL".into(), vec![0]),
        ("FR".into(), vec![1]),
        ("SL".into(), vec![2]),
        ("SR".into(), vec![3]),
        ("HFL".into(), vec![4]),
        ("HFR".into(), vec![5]),
        ("SUB1".into(), vec![7]),
    ];
    for (name, path) in [("web", web_asset_path()), ("deploy", deploy_path())] {
        let doc = load(&path);
        let def = map_get(&doc, "output_layouts")
            .and_then(|m| map_get(m, "4.1.2"))
            .unwrap_or_else(|| panic!("{name} 缺少 output_layouts 的 4.1.2"));
        let roles = map_get(def, "roles")
            .and_then(Value::as_sequence)
            .unwrap_or_else(|| panic!("{name} 4.1.2.roles 缺失"));
        let got: Vec<(String, Vec<u64>)> = roles
            .iter()
            .map(|r| (as_str(map_get(r, "id")), as_u64s(map_get(r, "outputs"))))
            .collect();
        assert_eq!(
            got, expected,
            "{name} 的 4.1.2 role→outputs 必须为板端 MOTU 真相"
        );
        assert!(
            !got.iter().any(|(_, outs)| outs.contains(&6)),
            "{name} 的 4.1.2 不应把任何角色映射到 ch6（空闲）"
        );
    }
}
