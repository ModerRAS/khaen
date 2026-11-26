//! 构建信息（T46）：crate 版本 / release 版本 / commit 三口径分离，外加 rustc、profile、target。
//!
//! 版本事实来源（全部真实，不编造）：
//! - crate_version：编译进二进制的 Cargo 版本（workspace version，即 /healthz 历史上错误上报的 0.1.0）；
//! - release_version：release workflow 在构建时注入的 KHAEN_BUILD_VERSION（如 v0.1.2），
//!   这是实际部署发布的版本；未注入时为 None + 原因（开发构建，不冒充 release）；
//! - commit：KHAEN_BUILD_COMMIT 或构建期 git rev-parse HEAD；拿不到时为 None + 原因。
//!
//! 构建期常量由 crates/daemon/build.rs 生成到 OUT_DIR/build_info.rs；本模块把它们包装成可在
//! 测试中注入的 BuildInfo（handler 只读 DaemonState.build_info()，不读环境变量）。

use std::collections::BTreeMap;

use crate::VERSION;

include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

/// 构建信息（默认来自 build.rs 的构建期常量；测试注入 release 版本验证三口径展示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// 编译进二进制的 crate 版本（CARGO_PKG_VERSION）。
    pub crate_version: String,
    /// 部署 release 版本（KHAEN_BUILD_VERSION；缺失为 None）。
    pub release_version: Option<String>,
    /// 构建 commit（KHAEN_BUILD_COMMIT 或构建期 git HEAD；缺失为 None）。
    pub commit: Option<String>,
    /// commit 的来源（发布流水线注入 / 本机 git），诊断用。
    pub commit_source: Option<String>,
    /// 构建用 rustc 版本。
    pub rustc: Option<String>,
    /// cargo build profile（debug / release）。
    pub build_profile: String,
    /// 交叉编译目标三元组。
    pub target: String,
    /// 字段路径 -> 缺失/回退原因（并入 /api/system/info 的 reasons）。
    pub reasons: BTreeMap<String, String>,
}

impl BuildInfo {
    /// 用 build.rs 的构建期常量构造（daemon 启动的默认值）。
    pub fn from_build() -> Self {
        let mut reasons = BTreeMap::new();
        if let Some(reason) = RELEASE_VERSION_REASON {
            reasons.insert("release_version".to_string(), reason.to_string());
        }
        if let Some(reason) = COMMIT_REASON {
            reasons.insert("commit".to_string(), reason.to_string());
        }
        if let Some(reason) = RUSTC_REASON {
            reasons.insert("rustc".to_string(), reason.to_string());
        }
        Self {
            crate_version: VERSION.to_string(),
            release_version: RELEASE_VERSION.map(str::to_string),
            commit: COMMIT.map(str::to_string),
            commit_source: COMMIT_SOURCE.map(str::to_string),
            rustc: RUSTC_VERSION.map(str::to_string),
            build_profile: BUILD_PROFILE.to_string(),
            target: TARGET.to_string(),
            reasons,
        }
    }

    /// /healthz 的 version：release 版本优先；缺失时如实回退 crate 版本（不冒充 release）。
    pub fn healthz_version(&self) -> &str {
        self.release_version
            .as_deref()
            .unwrap_or(&self.crate_version)
    }

    /// crate 与 release 是否一致；任一缺失返回 None（unknown，不编造一致）。
    pub fn versions_match(&self) -> Option<bool> {
        let release = self.release_version.as_deref()?;
        let normalize = |value: &str| value.trim().trim_start_matches(['v', 'V']).to_string();
        Some(normalize(&self.crate_version) == normalize(release))
    }
}

/// 解析 KHAEN_BUILD_VERSION 的纯函数（build.rs 的同语义实现；只用于测试与文档一致性，
/// 生产路径直接用 build.rs 生成的构建期常量，不读运行期环境变量）。
///
/// 返回 (release 版本, 缺失原因)：空/空白按缺失处理。
#[cfg(test)]
pub(crate) fn parse_release_version(raw: Option<&str>) -> (Option<String>, Option<String>) {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => (Some(value.to_string()), None),
        None => (
            None,
            Some(format!(
                "KHAEN_BUILD_VERSION is not set at build time: this is an unversioned build (crate version {VERSION})"
            )),
        ),
    }
}

/// 解析 KHAEN_BUILD_COMMIT 的纯函数：空/空白按未设置处理（由调用方回退 git）。
#[cfg(test)]
pub(crate) fn parse_commit_env(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_build_info_is_honest_about_missing_release_metadata() {
        let info = BuildInfo::from_build();
        assert_eq!(info.crate_version, VERSION);
        // build.rs 在未注入 KHAEN_BUILD_VERSION 时必须给 None + 原因，不许冒充 release。
        match RELEASE_VERSION {
            Some(version) => {
                assert_eq!(info.release_version.as_deref(), Some(version));
                assert!(!info.reasons.contains_key("release_version"));
            }
            None => {
                assert_eq!(info.release_version, None);
                let reason = info
                    .reasons
                    .get("release_version")
                    .expect("缺失 release 版本必须带原因");
                assert!(reason.contains("KHAEN_BUILD_VERSION"), "{reason}");
            }
        }
        // profile / target 恒有真实值。
        assert!(!info.build_profile.is_empty());
        assert!(!info.target.is_empty());
    }

    #[test]
    fn healthz_version_prefers_release_and_falls_back_to_crate() {
        let mut info = BuildInfo::from_build();
        info.release_version = Some("v0.1.2".to_string());
        assert_eq!(info.healthz_version(), "v0.1.2");
        info.release_version = None;
        assert_eq!(info.healthz_version(), VERSION);
    }

    #[test]
    fn versions_match_normalizes_the_leading_v_and_never_invents_equality() {
        let mut info = BuildInfo::from_build();
        info.crate_version = "0.1.2".to_string();
        info.release_version = Some("v0.1.2".to_string());
        assert_eq!(info.versions_match(), Some(true));
        info.crate_version = "0.1.0".to_string();
        assert_eq!(info.versions_match(), Some(false));
        info.release_version = None;
        assert_eq!(info.versions_match(), None);
    }

    #[test]
    fn release_and_commit_parsers_treat_blank_as_missing() {
        assert_eq!(parse_release_version(None).0, None);
        assert_eq!(parse_release_version(Some("   ")).0, None);
        let (release, reason) = parse_release_version(Some(" v0.1.2 "));
        assert_eq!(release.as_deref(), Some("v0.1.2"));
        assert_eq!(reason, None);
        assert!(parse_release_version(None).1.is_some());

        assert_eq!(parse_commit_env(None), None);
        assert_eq!(parse_commit_env(Some("")), None);
        assert_eq!(
            parse_commit_env(Some("  abc123  ")),
            Some("abc123".to_string())
        );
    }
}
