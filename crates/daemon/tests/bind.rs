//! M0-A 监听地址解析测试：KHAEN_BIND / KHAEN_PORT / PORT 的优先级与稳定错误信息。
//!
//! 解析函数注入环境变量查找闭包，不修改进程环境（避免测试并行互相干扰）；
//! 另外直接启动真实二进制，验证"非法值启动即失败 + 稳定错误"。

use std::net::SocketAddr;
use std::process::Command;

use khaen_daemon::bind::{self, BindError, DEFAULT_BIND};

/// 假环境变量表（不碰进程环境）。
struct FakeEnv<'a>(&'a [(&'static str, &'static str)]);

impl FakeEnv<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_string())
    }
}

fn resolve(env: &FakeEnv<'_>) -> Result<SocketAddr, BindError> {
    bind::resolve(|key| env.get(key))
}

#[test]
fn default_is_loopback_8730() {
    let addr = resolve(&FakeEnv(&[])).expect("默认地址必须合法");
    assert_eq!(addr.to_string(), DEFAULT_BIND);
    assert_eq!(addr.to_string(), "127.0.0.1:8730");
    assert_eq!(addr, bind::default_addr());
    assert!(addr.ip().is_loopback(), "开发默认只绑本机");
}

#[test]
fn khaen_bind_is_a_full_address_and_wins_over_ports() {
    let env = FakeEnv(&[
        ("KHAEN_BIND", "0.0.0.0:80"),
        ("KHAEN_PORT", "9000"),
        ("PORT", "9001"),
    ]);
    assert_eq!(resolve(&env).expect("bind").to_string(), "0.0.0.0:80");

    // KHAEN_BIND 已给出时，坏掉的端口变量不影响启动。
    let env = FakeEnv(&[
        ("KHAEN_BIND", "127.0.0.1:8731"),
        ("KHAEN_PORT", "not-a-port"),
    ]);
    assert_eq!(
        resolve(&env).expect("bind wins").to_string(),
        "127.0.0.1:8731"
    );

    // IPv6 完整地址。
    let env = FakeEnv(&[("KHAEN_BIND", "[::1]:8732")]);
    assert_eq!(resolve(&env).expect("ipv6").to_string(), "[::1]:8732");
}

#[test]
fn khaen_port_and_port_bind_loopback_only() {
    let env = FakeEnv(&[("KHAEN_PORT", "9000")]);
    assert_eq!(
        resolve(&env).expect("khaen_port").to_string(),
        "127.0.0.1:9000"
    );

    let env = FakeEnv(&[("PORT", "9001")]);
    assert_eq!(resolve(&env).expect("port").to_string(), "127.0.0.1:9001");

    // 二者都给时 KHAEN_PORT 胜出（兼容旧配置）。
    let env = FakeEnv(&[("KHAEN_PORT", "9000"), ("PORT", "9001")]);
    assert_eq!(
        resolve(&env).expect("khaen_port wins").to_string(),
        "127.0.0.1:9000"
    );

    // 端口 0 = 由系统分配空闲端口（测试/嵌入式场景可用）。
    let env = FakeEnv(&[("KHAEN_PORT", "0")]);
    assert_eq!(resolve(&env).expect("port 0").to_string(), "127.0.0.1:0");
}

#[test]
fn invalid_khaen_bind_is_a_stable_error() {
    for value in [
        "",
        "127.0.0.1",
        "localhost:8730",
        "0.0.0.0",
        "http://127.0.0.1:8730",
        "999.0.0.1:80",
        "127.0.0.1:not-a-port",
    ] {
        let error = resolve(&FakeEnv(&[("KHAEN_BIND", value)])).expect_err("必须失败");
        let message = error.to_string();
        assert!(
            message.starts_with("invalid KHAEN_BIND value "),
            "{value:?} -> {message}"
        );
        assert!(
            message.contains("expected an IPv4/IPv6 socket address"),
            "{value:?} -> {message}"
        );
        match error {
            BindError::InvalidBind {
                value: reported, ..
            } => assert_eq!(reported, value),
            other => panic!("错误类型不对: {other:?}"),
        }
    }
}

#[test]
fn invalid_port_is_a_stable_error() {
    for (variable, value) in [
        ("KHAEN_PORT", "not-a-port"),
        ("KHAEN_PORT", "70000"),
        ("KHAEN_PORT", "-1"),
        ("KHAEN_PORT", ""),
        ("PORT", "http"),
    ] {
        let error = resolve(&FakeEnv(&[(variable, value)])).expect_err("必须失败");
        let message = error.to_string();
        assert!(
            message.starts_with(&format!("invalid {variable} value ")),
            "{variable}={value:?} -> {message}"
        );
        assert!(
            message.contains("expected a port number between 0 and 65535"),
            "{variable}={value:?} -> {message}"
        );
        match error {
            BindError::InvalidPort {
                variable: reported,
                value: reported_value,
            } => {
                assert_eq!(reported, variable);
                assert_eq!(reported_value, value);
            }
            other => panic!("错误类型不对: {other:?}"),
        }
    }
}

#[test]
fn daemon_binary_fails_fast_with_stable_error_on_invalid_bind() {
    let output = Command::new(env!("CARGO_BIN_EXE_khaen-daemon"))
        .env_remove("KHAEN_PORT")
        .env_remove("PORT")
        .env("KHAEN_BIND", "not-an-address")
        .output()
        .expect("启动 khaen-daemon 二进制");
    assert_eq!(
        output.status.code(),
        Some(2),
        "配置错误应以退出码 2 退出; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid KHAEN_BIND value \"not-an-address\""),
        "stderr 缺少稳定错误信息: {stderr}"
    );
    assert!(
        stderr.contains("expected an IPv4/IPv6 socket address"),
        "stderr 缺少格式提示: {stderr}"
    );
}
