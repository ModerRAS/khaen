//! 校准 REST 的存储层（T36）：上传文件存储、原子写、产物白名单与媒体类型。
//!
//! # 上传
//!
//! - 上传内容按内容寻址存到状态目录（环境变量 KHAEN_STATE_DIR，见 auth.rs AuthConfig）下的
//!   `uploads/<sha256>`：文件名**只用** sha256 小写十六进制，因此不可能出现路径穿越
//!   （id 校验在 [`is_upload_id`] 里，目录名另行拼接后仍会复核 parent）；
//! - 原子写：同目录 `.tmp-*` → `write_all + sync_all` → `rename`，中途失败不会留下半截
//!   文件；Unix 上 0600（与 auth.json 同权限档）；
//! - 幂等：同名（= 同内容）文件已存在时直接复用，不重写。
//!
//! # 产物
//!
//! REST 只按白名单暴露 job 产物（[`ARTIFACT_WHITELIST`]），未知名字一律 404，避免把 backend
//! 目录里的任意文件（checkpoint、日志、输入 WAV）暴露出去。白名单同时决定 Content-Type；
//! 字节的 sha256 校验在 api 层做（与 artifact manifest 记录比对）。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::HeaderMap;

use khaen_core::sha256::sha256_hex;

use super::service::{CalibrationError, UploadRef};

/// 上传目录名（状态目录下）。
pub const UPLOADS_DIR: &str = "uploads";
/// 单个上传的大小上限：64 MiB（PLAN §4.7 的原始 WAV/JSON 上传；足够两段 96 kHz 测量 WAV）。
pub const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
/// 上传接受的 Content-Type（去掉参数后的小写 essence）。
pub const UPLOAD_CONTENT_TYPES: [&str; 2] = ["application/octet-stream", "audio/wav"];
/// 产物白名单：REST 只暴露这些名字（其余 404）。
pub const ARTIFACT_WHITELIST: [&str; 10] = [
    "report.json",
    "artifacts.manifest.json",
    "plan.bin",
    "levels.json",
    "predicted.json",
    "observed.json",
    // C3b1：rt-host 真实测量 job 的证据产物（offline job 不会产生这些名字）。
    "measurement.json",
    "quality.json",
    // C3b2a：rt-host cal_solve 的单点设计回执（predicted-only，不是可应用 plan）。
    "design.json",
    // C3b2b：rt-host 真实测量 job 的 compile 产物（plan 字节由 daemon 校验后复制进 job 目录；
    // 编译报告记录它的 sha256/graph_id/verdict=PREDICTED_ONLY）。
    "compile-report.json",
];

/// 上传 id / 内容 sha256 是否是合法的 64 位小写十六进制。
///
/// 上传 id 与磁盘文件名同源，所以只接受规范化小写（大小写混写会造成同内容两个文件，
/// 也让 Linux/Windows 行为不一致）。
pub fn is_upload_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// 是否是 64 位十六进制（不区分大小写；用于 plan_sha 等外部 sha 字段，统一转小写）。
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 去参数后的 Content-Type essence（小写）。
pub fn content_type_essence(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::CONTENT_TYPE)?
        .to_str()
        .ok()?;
    let essence = value.split(';').next().unwrap_or(value).trim();
    if essence.is_empty() {
        None
    } else {
        Some(essence.to_ascii_lowercase())
    }
}

/// 上传媒体类型是否被接受。
pub fn is_supported_upload_content_type(essence: &str) -> bool {
    UPLOAD_CONTENT_TYPES.contains(&essence)
}

/// 产物名是否可安全作为文件名（bare filename；无分隔符、无 ..、长度受限）。
pub fn is_safe_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 产物名是否在白名单内（可下载）。
pub fn is_whitelisted_artifact(name: &str) -> bool {
    ARTIFACT_WHITELIST.contains(&name)
}

/// 产物名的 Content-Type（按扩展名；未知按 octet-stream）。
pub fn artifact_content_type(name: &str) -> &'static str {
    match name.rsplit_once('.') {
        Some((_, "json")) => "application/json",
        Some((_, "wav")) => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// 上传记录（磁盘上的真实文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRecord {
    /// 上传 id（= 内容 sha256）。
    pub upload_id: String,
    /// 内容 sha256。
    pub sha256: String,
    /// 字节数。
    pub bytes: u64,
    /// 磁盘路径。
    pub path: PathBuf,
}

impl UploadRecord {
    /// 转成服务层引用。
    pub fn as_ref_record(&self) -> UploadRef {
        UploadRef {
            upload_id: self.upload_id.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
            path: self.path.clone(),
        }
    }
}

/// 上传存储：状态目录下的 `uploads/`。
#[derive(Debug, Clone)]
pub struct UploadStore {
    root: PathBuf,
}

impl UploadStore {
    /// 状态目录 → `<state_dir>/uploads`。
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join(UPLOADS_DIR),
        }
    }

    /// 上传目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 校验上传 id 并给出磁盘路径（id 非法 → 400 invalid_upload）。
    pub fn path_for_id(&self, upload_id: &str) -> Result<PathBuf, CalibrationError> {
        if !is_upload_id(upload_id) {
            return Err(CalibrationError::invalid_upload(format!(
                "upload id must be 64 lowercase hex characters (the sha256 of the uploaded bytes), got {upload_id:?}"
            )));
        }
        let path = self.root.join(upload_id);
        // 双保险：即使有人改了 id 规则，也只允许直接子文件。
        if path.parent() != Some(self.root.as_path()) {
            return Err(CalibrationError::invalid_upload(
                "upload id must name a file directly inside the uploads directory",
            ));
        }
        Ok(path)
    }

    /// 查找已存在上传（不存在返回 None；id 非法仍是 400）。
    pub fn lookup(&self, upload_id: &str) -> Result<Option<UploadRecord>, CalibrationError> {
        let path = self.path_for_id(upload_id)?;
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(CalibrationError::internal(format!(
                    "cannot stat upload {}: {error}",
                    path.display()
                )))
            }
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        Ok(Some(UploadRecord {
            upload_id: upload_id.to_string(),
            sha256: upload_id.to_string(),
            bytes: metadata.len(),
            path,
        }))
    }

    /// 存一段上传字节（阻塞 IO；调用方用 spawn_blocking）。
    ///
    /// 返回 `(record, created)`：已存在同 sha 文件时 created=false（幂等）。
    pub fn save(&self, bytes: &[u8]) -> Result<(UploadRecord, bool), CalibrationError> {
        let sha256 = sha256_hex(bytes);
        std::fs::create_dir_all(&self.root).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot create uploads directory {}: {error}",
                self.root.display()
            ))
        })?;
        let path = self.path_for_id(&sha256)?;
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.is_file() {
                return Ok((
                    UploadRecord {
                        upload_id: sha256.clone(),
                        sha256,
                        bytes: metadata.len(),
                        path,
                    },
                    false,
                ));
            }
        }
        write_atomic(&path, bytes, 0o600).map_err(|error| {
            CalibrationError::internal(format!("cannot store upload {}: {error}", path.display()))
        })?;
        Ok((
            UploadRecord {
                upload_id: sha256.clone(),
                sha256,
                bytes: bytes.len() as u64,
                path,
            },
            true,
        ))
    }
}

/// 原子写：同目录临时文件 → 写满 + fsync → rename 覆盖目标。
///
/// Unix 上把临时文件权限设为 `mode`（上传/wizard.json 用 0600）；Windows 忽略 mode。
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write;

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".tmp-{}-{unique}", std::process::id()));
    let result = (|| -> io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
            options.open(&tmp)?
        };
        #[cfg(not(unix))]
        let file = {
            let _ = mode;
            options.open(&tmp)?
        };
        let mut file = file;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-cal-store-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn upload_ids_are_strict_lowercase_sha256() {
        assert!(is_upload_id(&"a".repeat(64)));
        assert!(is_upload_id(&"0".repeat(64)));
        assert!(!is_upload_id(&"A".repeat(64)));
        assert!(!is_upload_id(&"a".repeat(63)));
        assert!(!is_upload_id("../../etc/passwd"));
        assert!(!is_upload_id(""));
        assert!(is_sha256_hex(&"A".repeat(64)));
        assert!(!is_sha256_hex(&"g".repeat(64)));
    }

    #[test]
    fn content_type_essence_strips_parameters_and_lowercases() {
        let mut headers = HeaderMap::new();
        assert_eq!(content_type_essence(&headers), None);
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            "Audio/WAV; codecs=1".parse().expect("header"),
        );
        assert_eq!(content_type_essence(&headers).as_deref(), Some("audio/wav"));
        assert!(is_supported_upload_content_type("audio/wav"));
        assert!(is_supported_upload_content_type("application/octet-stream"));
        assert!(!is_supported_upload_content_type("text/plain"));
    }

    #[test]
    fn artifact_names_are_whitelisted_and_content_typed() {
        for name in ARTIFACT_WHITELIST {
            assert!(is_safe_artifact_name(name), "{name}");
            assert!(is_whitelisted_artifact(name), "{name}");
        }
        assert!(!is_whitelisted_artifact("../../auth.json"));
        assert!(!is_safe_artifact_name("../report.json"));
        assert!(!is_safe_artifact_name("sub/report.json"));
        // C3b2a：cal_solve 的设计证据可下载（offline job 不会产生这个名字）。
        assert!(is_whitelisted_artifact("design.json"));
        assert_eq!(artifact_content_type("design.json"), "application/json");
        // C3b2b：rt-host job 的 compile 证据可下载（variant/JSON 分类沿用扩展名）。
        assert!(is_whitelisted_artifact("compile-report.json"));
        assert_eq!(
            artifact_content_type("compile-report.json"),
            "application/json"
        );
        assert_eq!(artifact_content_type("report.json"), "application/json");
        assert_eq!(
            artifact_content_type("plan.bin"),
            "application/octet-stream"
        );
        assert_eq!(artifact_content_type("x.wav"), "audio/wav");
    }

    #[test]
    fn save_is_content_addressed_and_idempotent() {
        let dir = temp_dir("save");
        let store = UploadStore::new(&dir);
        let (first, created) = store.save(b"baseline-wav-bytes").expect("save");
        assert!(created);
        assert_eq!(first.bytes, 18);
        assert_eq!(first.upload_id, sha256_hex(b"baseline-wav-bytes"));
        assert_eq!(
            std::fs::read(&first.path).expect("read"),
            b"baseline-wav-bytes"
        );
        let (second, created) = store.save(b"baseline-wav-bytes").expect("save again");
        assert!(!created);
        assert_eq!(second, first);
        let (third, created) = store.save(b"other").expect("save other");
        assert!(created);
        assert_ne!(third.upload_id, first.upload_id);
        assert_eq!(store.lookup(&first.upload_id).expect("lookup"), Some(first));
        assert_eq!(store.lookup(&"f".repeat(64)).expect("lookup"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn traversal_ids_are_rejected_before_touching_disk() {
        let dir = temp_dir("traversal");
        let store = UploadStore::new(&dir);
        for id in ["../auth.json", "..", "/etc/passwd", "ab/cd", ""] {
            let error = store.path_for_id(id).expect_err("must be rejected");
            assert_eq!(
                error.code(),
                super::super::service::code::INVALID_UPLOAD,
                "{id:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn write_atomic_replaces_existing_file_and_leaves_no_temp() {
        let dir = temp_dir("atomic");
        let path = dir.join("wizard.json");
        write_atomic(&path, b"{\"a\":1}", 0o600).expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"{\"a\":1}");
        write_atomic(&path, b"{\"a\":2}", 0o600).expect("rewrite");
        assert_eq!(std::fs::read(&path).expect("read"), b"{\"a\":2}");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
