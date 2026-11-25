//! 自有 JSON 解析/规范化写作 + 自有 SHA-256 的契约测试。

use khaen_file_runner::json::Json;
use khaen_file_runner::sha256::{hex, is_hex64, sha256, sha256_hex};
use khaen_file_runner::{E_FILE_FIELD, E_FILE_JSON, MAX_JSON_DEPTH};

fn err_code(text: &str) -> &'static str {
    Json::parse(text).expect_err("must fail").code()
}

#[test]
fn json_parse_primitives_and_canonical_order() {
    let value = Json::parse(r#" { "b" : [1, 2.5, true, null, "x"], "a" : { "z" : 0 } } "#)
        .expect("valid json");
    // 键按字典序、无空白、数字最短表示。
    assert_eq!(
        value.to_canonical(),
        r#"{"a":{"z":0},"b":[1,2.5,true,null,"x"]}"#
    );
    // round-trip：canonical 文本再解析得到同值。
    let again = Json::parse(&value.to_canonical()).expect("canonical parses");
    assert_eq!(again, value);
}

#[test]
fn json_number_canonical_is_shortest_and_round_trips() {
    for (text, canonical) in [
        ("1.0", "1"),
        ("-0.0", "0"),
        ("0.5", "0.5"),
        ("-2", "-2"),
        ("1e3", "1000"),
        ("123456789", "123456789"),
    ] {
        let value = Json::parse(text).expect("number parses");
        assert_eq!(value.to_canonical(), canonical, "text {text}");
        assert_eq!(
            Json::parse(&value.to_canonical()).expect("re-parse"),
            value,
            "text {text}"
        );
    }
}

#[test]
fn json_string_escapes_round_trip() {
    let value = Json::from_entries(vec![(
        "s",
        Json::String("quote\" slash\\ tab\t nl\n cr\rctl\u{1}".to_string()),
    )]);
    let text = value.to_canonical();
    assert_eq!(Json::parse(&text).expect("escaped parses"), value);
    // 控制字符必须转义（不得输出裸 0x01）。
    assert!(!text.contains('\u{1}'));
    assert!(text.contains("\\u0001"));
}

#[test]
fn json_rejects_duplicate_keys() {
    let code = err_code(r#"{"a":1,"a":2}"#);
    assert_eq!(code, E_FILE_JSON);
}

#[test]
fn json_rejects_trailing_comma_and_comments() {
    assert_eq!(err_code(r#"{"a":1,}"#), E_FILE_JSON);
    assert_eq!(err_code("[1,2,]"), E_FILE_JSON);
    assert_eq!(err_code("{} // comment"), E_FILE_JSON);
    assert_eq!(err_code("/* c */ {}"), E_FILE_JSON);
}

#[test]
fn json_rejects_non_finite_and_bad_numbers() {
    assert_eq!(err_code("NaN"), E_FILE_JSON);
    assert_eq!(err_code("Infinity"), E_FILE_JSON);
    assert_eq!(err_code("1e999"), E_FILE_JSON);
    assert_eq!(err_code("01"), E_FILE_JSON);
    assert_eq!(err_code("-"), E_FILE_JSON);
}

#[test]
fn json_rejects_trailing_content_and_truncation() {
    assert_eq!(err_code("{} {}"), E_FILE_JSON);
    assert_eq!(err_code("{}x"), E_FILE_JSON);
    assert_eq!(err_code(r#"{"a":"#), E_FILE_JSON);
    assert_eq!(err_code(""), E_FILE_JSON);
}

#[test]
fn json_rejects_excessive_nesting() {
    let deep = format!(
        "{}{}",
        "[".repeat(MAX_JSON_DEPTH + 2),
        "]".repeat(MAX_JSON_DEPTH + 2)
    );
    assert_eq!(err_code(&deep), E_FILE_JSON);
    // 恰好在深度上限内的嵌套可解析。
    let ok = format!(
        "{}{}",
        "[".repeat(MAX_JSON_DEPTH),
        "]".repeat(MAX_JSON_DEPTH)
    );
    assert!(Json::parse(&ok).is_ok());
}

#[test]
fn json_rejects_raw_control_character_in_string() {
    assert_eq!(err_code("{\"a\":\"line\u{1}break\"}"), E_FILE_JSON);
}

#[test]
fn json_require_helpers_report_field_errors() {
    let value =
        Json::parse(r#"{"s":"x","n":3,"f":1.5,"b":true,"arr":[1],"obj":{}}"#).expect("valid");
    assert_eq!(value.require_str("s").expect("str"), "x");
    assert_eq!(value.require_u64("n").expect("u64"), 3);
    assert_eq!(value.require_f64("f").expect("f64"), 1.5);
    assert!(value.require_object("obj").is_ok());
    assert_eq!(value.require_array("arr").expect("arr").len(), 1);
    assert_eq!(
        value.require_str("n").expect_err("type mismatch").code(),
        E_FILE_FIELD
    );
    assert_eq!(
        value.require("missing").expect_err("missing").code(),
        E_FILE_FIELD
    );
}

#[test]
fn sha256_nist_vectors() {
    // FIPS 180-4 / NIST CAVP 已知向量。
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    // 1,000,000 个 'a'。
    let million = vec![b'a'; 1_000_000];
    assert_eq!(
        sha256_hex(&million),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn sha256_hex_and_is_hex64_contract() {
    let digest = sha256(b"tspi");
    assert_eq!(digest.len(), 32);
    assert_eq!(hex(&digest), sha256_hex(b"tspi"));
    assert!(is_hex64(&sha256_hex(b"tspi")));
    assert!(is_hex64(&"F".repeat(64)));
    assert!(!is_hex64(&"a".repeat(63)));
    assert!(!is_hex64(&("a".repeat(63) + "g")));
    assert!(!is_hex64(""));
}
