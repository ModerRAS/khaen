//! profile 持久化 + 两阶段草稿（docs/API.md §12.2 PUT /api/audio-profile）。
//!
//! - 已保存 profile 落 daemon 状态目录的 audio-profile.json（原子写：同目录唯一 tmp + rename）；
//!   首次运行没有文件 -> 返回 §12.6 的默认 2.0 profile（不自动写盘）。
//! - prepare 只把规范化 profile + diff 记进进程内单草稿槽并签发一次性 token（不碰引擎、不写盘）；
//!   commit 校验 token 后原子落盘并 revision+1；abort 丢弃草稿。
//! - 草稿 TTL 120 s（响应里如实给 expires_in_s）；token 是不透明 16 hex。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::model::{AudioProfile, ProfileDiff};

/// 状态目录下的 profile 文件名。
pub const STORE_FILE: &str = "audio-profile.json";
/// 落盘外壳 schema（包 revision / saved_at，profile 本体 schema 仍是 khaen-audio-profile/v1）。
pub const STORE_SCHEMA: &str = "khaen-audio-profile-store/v1";
/// 草稿有效期（秒）。
pub const PREPARE_TTL_SECS: u64 = 120;
/// 草稿有效期。
pub const PREPARE_TTL: Duration = Duration::from_secs(PREPARE_TTL_SECS);

/// 已保存 profile（含 revision）。
#[derive(Clone, Debug, PartialEq)]
pub struct SavedProfile {
    /// profile 本体。
    pub profile: AudioProfile,
    /// 保存版本（首次 commit = 1）。
    pub revision: u64,
    /// 保存时刻（RFC3339；旧文件缺失时为 None）。
    pub saved_at: Option<String>,
}

/// 当前生效 profile 的三种状态（GET 永远 200，按状态如实给 null + 原因）。
#[derive(Clone, Debug, PartialEq)]
pub enum CurrentProfile {
    /// 还没有保存过：首次运行默认 2.0。
    Default {
        /// 版本号（0 = 还没保存过）。
        revision: u64,
    },
    /// 已保存。
    Saved(SavedProfile),
    /// 文件存在但读不出来（损坏/权限）：不编造 profile。
    Unreadable {
        /// 人读原因。
        reason: String,
    },
}

impl CurrentProfile {
    /// 可用 profile（Unreadable 为 None）。
    pub fn profile(&self) -> Option<&AudioProfile> {
        match self {
            Self::Default { .. } => None,
            Self::Saved(saved) => Some(&saved.profile),
            Self::Unreadable { .. } => None,
        }
    }

    /// revision（Unreadable 为 None）。
    pub fn revision(&self) -> Option<u64> {
        match self {
            Self::Default { revision } => Some(*revision),
            Self::Saved(saved) => Some(saved.revision),
            Self::Unreadable { .. } => None,
        }
    }

    /// 是否来自已保存文件。
    pub fn saved(&self) -> bool {
        matches!(self, Self::Saved(_))
    }

    /// 不可读原因。
    pub fn unreadable_reason(&self) -> Option<&str> {
        match self {
            Self::Unreadable { reason } => Some(reason),
            _ => None,
        }
    }
}

/// prepare 产出的 profile 草稿。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingProfileDraft {
    /// 一次性 token（16 hex）。
    pub token: String,
    /// 规范化 profile。
    pub profile: AudioProfile,
    /// 相对当前已保存 profile 的 diff。
    pub diff: ProfileDiff,
    /// 创建时刻（单调时钟，只看过期）。
    pub created: Instant,
    /// 有效期（秒；回执里如实给）。
    pub expires_in_s: u64,
}

impl PendingProfileDraft {
    /// 是否过期。
    pub fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created) > PREPARE_TTL
    }
}

/// 草稿 token 校验失败（映射 409 engine_token_invalid）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftTokenError {
    /// 没有草稿（未 prepare 或已被 commit/abort 消费）。
    NoDraft,
    /// token 与当前草稿不符（可能被新的 prepare 覆盖）。
    Mismatch,
    /// 草稿过期。
    Expired,
}

impl DraftTokenError {
    /// 稳定错误码。
    pub fn code(self) -> &'static str {
        khaen_host::control::error::ENGINE_TOKEN_INVALID
    }

    /// 人读原因。
    pub fn message(self) -> &'static str {
        match self {
            Self::NoDraft => {
                "no audio profile draft is pending: call stage=prepare first (a commit/abort consumes the draft)"
            }
            Self::Mismatch => {
                "token does not match the pending audio profile draft (another prepare replaced it)"
            }
            Self::Expired => "the audio profile draft expired: call stage=prepare again",
        }
    }
}

/// profile 存储 + 单草稿槽（进程内互斥；控制面路径，非实时路径）。
pub struct AudioProfileStore {
    state_dir: PathBuf,
    pending: Mutex<Option<PendingProfileDraft>>,
    write_guard: Mutex<()>,
}

impl std::fmt::Debug for AudioProfileStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioProfileStore")
            .field("state_dir", &self.state_dir)
            .field("file", &self.file_path())
            .finish()
    }
}

impl AudioProfileStore {
    /// 用 daemon 状态目录构造（profile 落 <state_dir>/audio-profile.json）。
    pub fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            pending: Mutex::new(None),
            write_guard: Mutex::new(()),
        }
    }

    /// 状态目录。
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// profile 文件路径。
    pub fn file_path(&self) -> PathBuf {
        self.state_dir.join(STORE_FILE)
    }

    fn lock_pending(&self) -> MutexGuard<'_, Option<PendingProfileDraft>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_write(&self) -> MutexGuard<'_, ()> {
        self.write_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 读已保存 profile（文件不存在 -> Ok(None)；文件损坏 -> Err(原因)，不编数据）。
    pub fn load(&self) -> Result<Option<SavedProfile>, String> {
        let path = self.file_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "cannot read the saved audio profile {}: {error}",
                    path.display()
                ))
            }
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "the saved audio profile {} is not valid JSON: {error}",
                path.display()
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            format!(
                "the saved audio profile {} must be a JSON object",
                path.display()
            )
        })?;
        if let Some(schema) = object.get("schema") {
            if schema.as_str() != Some(STORE_SCHEMA)
                && schema.as_str() != Some(super::model::SCHEMA)
            {
                return Err(format!(
                    "the saved audio profile {} has an unsupported schema {schema}",
                    path.display()
                ));
            }
        }
        // 兼容两种外壳：{schema:store, revision, saved_at, profile} 或裸 profile。
        let (profile_value, revision, saved_at) = match object.get("profile") {
            Some(profile) => (
                profile.clone(),
                object.get("revision").and_then(Value::as_u64).unwrap_or(0),
                object
                    .get("saved_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ),
            None => (value.clone(), 0, None),
        };
        let profile = AudioProfile::parse(&profile_value)
            .map_err(|error| format!("the saved audio profile is invalid: {error}"))?;
        Ok(Some(SavedProfile {
            profile,
            revision,
            saved_at,
        }))
    }

    /// 当前 profile 状态（GET/plan 用；文件不可读 -> Unreadable + 原因）。
    pub fn current(&self) -> CurrentProfile {
        match self.load() {
            Ok(Some(saved)) => CurrentProfile::Saved(saved),
            Ok(None) => CurrentProfile::Default { revision: 0 },
            Err(reason) => CurrentProfile::Unreadable { reason },
        }
    }

    /// 保存草稿：不同 profile 覆盖旧草稿（旧 token 随即失效）；相同 profile 复用未过期草稿的 token。
    pub fn put_draft(
        &self,
        profile: AudioProfile,
        diff: ProfileDiff,
        now: Instant,
        now_rfc: &str,
    ) -> PendingProfileDraft {
        let mut pending = self.lock_pending();
        if let Some(existing) = pending.as_ref() {
            if !existing.is_expired(now) && existing.profile == profile {
                return existing.clone();
            }
        }
        let draft = PendingProfileDraft {
            token: next_token(&profile, now_rfc),
            profile,
            diff,
            created: now,
            expires_in_s: PREPARE_TTL_SECS,
        };
        *pending = Some(draft.clone());
        draft
    }

    /// 取走草稿（一次性；token 不符/过期都如实报错）。
    pub fn take_draft(
        &self,
        token: &str,
        now: Instant,
    ) -> Result<PendingProfileDraft, DraftTokenError> {
        let mut pending = self.lock_pending();
        let Some(draft) = pending.clone() else {
            return Err(DraftTokenError::NoDraft);
        };
        if draft.token != token {
            return Err(DraftTokenError::Mismatch);
        }
        if draft.is_expired(now) {
            *pending = None;
            return Err(DraftTokenError::Expired);
        }
        *pending = None;
        Ok(draft)
    }

    /// 原子保存 profile 并 revision+1（读当前文件 -> 写唯一 tmp -> rename）。
    pub fn save(&self, profile: AudioProfile, now_rfc: &str) -> Result<SavedProfile, String> {
        let _guard = self.lock_write();
        let revision = match self.load() {
            Ok(Some(saved)) => saved.revision.saturating_add(1),
            _ => 1,
        };
        let document = json!({
            "schema": STORE_SCHEMA,
            "revision": revision,
            "saved_at": now_rfc,
            "profile": profile.to_json(),
        });
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| format!("cannot encode the audio profile: {error}"))?;
        std::fs::create_dir_all(&self.state_dir).map_err(|error| {
            format!(
                "cannot create the daemon state directory {}: {error}",
                self.state_dir.display()
            )
        })?;
        let path = self.file_path();
        crate::plan_store::write_atomic(&self.state_dir, &path, &bytes)
            .map_err(|error| format!("cannot persist the audio profile: {error}"))?;
        Ok(SavedProfile {
            profile,
            revision,
            saved_at: Some(now_rfc.to_string()),
        })
    }
}

/// 草稿 token：sha256(canonical profile + 创建时刻 + 进程内计数器) 前 16 位（每次新草稿都不同）。
fn next_token(profile: &AudioProfile, now_rfc: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let canonical = format!(
        "khaen-audio-profile-draft/v1|{now_rfc}|{counter}|{}",
        profile.to_json()
    );
    let digest = khaen_host::sha256_hex(canonical.as_bytes());
    digest[..16].to_string()
}
