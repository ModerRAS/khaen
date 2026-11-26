//! 方案库（T55，M4 后端）：内容寻址上传存储 + 已 prepare/commit 方案元数据 + 生效历史。
//!
//! 用途：
//! - POST /api/plan/upload 把原始 ABI v3 字节解码、校验、内容寻址存到
//!   `<KHAEN_STATE_DIR>/plans/<sha256>.plan`（同内容幂等），并返回 graph_id / segments / latency；
//! - GET /api/plan/history 返回 daemon 见到的已生效历史（commit 回执 + rt-host/控制面版本）；
//! - GET /api/channels 需要"当前已生效方案的 routing 段"：rt-host 控制 status 只报 graph_id，
//!   不报 routing；daemon 在 prepare / upload 时把解码出的 routing 按 graph_id 记下来，
//!   运行时按 rt-host 报的 graph_id 关联。关联不到时 /api/channels 如实 null + 原因，不编数据。
//!
//! 这里的记录全部是进程内内存（daemon 重启后清空）；重启后 /api/channels 仍可从 rt-host
//! status 拿到格式字段，但 routing 段会如实 null + 原因，直到重新 prepare / upload。
//!
//! 所有文件写都是原子的（先写唯一 tmp 再 rename），不改变既有内容（同 sha 直接复用）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::plan::Plan;
use khaen_host::sha256_hex;
use serde_json::{json, Value};

/// /api/plan/upload 响应的 schema。
pub const SCHEMA_PLAN: &str = "khaen-plan/v1";
/// /api/plan/history 响应的 schema。
pub const SCHEMA_HISTORY: &str = "khaen-plan-history/v1";
/// 方案文件大小上限（4 MiB，任务 T55 契约）。
pub const MAX_PLAN_BYTES: usize = 4 * 1024 * 1024;
/// 内存里保留的生效历史 / 上传记录条数上限。
pub const MAX_RECORDS: usize = 32;
/// history 分页默认条数。
pub const HISTORY_DEFAULT_LIMIT: usize = 20;
/// history 分页上限。
pub const HISTORY_MAX_LIMIT: usize = 100;

/// 方案进入方案库的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    /// POST /api/plan/upload 上传的原始字节。
    Upload,
    /// daemon 在 prepare 时按 plan_path 读到的本地方案文件。
    LocalFile,
}

impl PlanSource {
    /// 稳定字符串（响应里出现）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::LocalFile => "local-file",
        }
    }
}

/// 一份方案的解码元数据（不含完整系数，避免方案库把整份方案常驻内存）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanRecord {
    /// 原始字节的 sha256（内容寻址键；本地文件也重算）。
    pub sha256: String,
    /// 方案 graph_id（ABI v3 的规范化 sha256）。
    pub graph_id: String,
    /// 原始字节的落盘路径（上传一定存在；本地 prepare 记录原路径）。
    pub path: Option<PathBuf>,
    /// 原始字节数。
    pub size_bytes: usize,
    /// 方案采样率。
    pub sample_rate: u32,
    /// 方案块长。
    pub block_size: u32,
    /// 方案包含的区段名（content/crossover/matrix/protection/routing）。
    pub sections: Vec<&'static str>,
    /// 准入预算估算延迟（样本）。
    pub latency_samples: u64,
    /// 是否通过准入预算（未通过也记录真实结果，不冒充可下发）。
    pub admission_accepted: bool,
    /// 准入拒绝明细（accepted=false 时非空）。
    pub admission_rejections: Vec<String>,
    /// content.in_channels。
    pub in_channels: Option<u16>,
    /// content.out_lanes。
    pub out_lanes: Option<u16>,
    /// routing.dac_channels。
    pub dac_channels: Option<u16>,
    /// routing.routes（lane -> dac）。
    pub routes: Option<Vec<(u16, u16)>>,
    /// 来源。
    pub source: PlanSource,
    /// 记录时刻（RFC3339）。
    pub recorded_at: String,
}

impl PlanRecord {
    /// 当前方案的 routing 视图（没有 routing 段时为 None）。
    pub fn routing(&self) -> Option<RoutingView> {
        self.dac_channels.map(|dac_channels| RoutingView {
            graph_id: Some(self.graph_id.clone()),
            dac_channels,
            routes: self.routes.clone().unwrap_or_default(),
        })
    }

    /// 上传/元数据响应用的 JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "sha256": self.sha256,
            "graph_id": self.graph_id,
            "size_bytes": self.size_bytes,
            "sample_rate": self.sample_rate,
            "block_size": self.block_size,
            "segments": self.sections,
            "segment_count": self.sections.len(),
            "latency_samples": self.latency_samples,
            "routing": self.routing().map(|routing| routing.to_json()),
            "layout": {
                "in_channels": self.in_channels,
                "out_lanes": self.out_lanes,
            },
            "admission": {
                "accepted": self.admission_accepted,
                "rejections": self.admission_rejections,
            },
            "source": self.source.as_str(),
            "recorded_at": self.recorded_at,
            "stored_path": self.path.as_ref().map(|path| path.display().to_string()),
        })
    }
}

/// 当前生效方案的 routing 视图（/api/channels 的 mapping / dac_map 来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingView {
    /// 关联的 graph_id（来自 rt-host status 或 commit 回执）。
    pub graph_id: Option<String>,
    /// DAC 通道总数。
    pub dac_channels: u16,
    /// (lane, dac) 对（未列出的 DAC 静音）。
    pub routes: Vec<(u16, u16)>,
}

impl RoutingView {
    /// mapping 数组：[{"lane","dac"}]，按 lane 升序。
    pub fn mapping_json(&self) -> Value {
        let mut routes = self.routes.clone();
        routes.sort_unstable();
        Value::Array(
            routes
                .into_iter()
                .map(|(lane, dac)| json!({"lane": lane, "dac": dac}))
                .collect(),
        )
    }

    /// dac_map 数组：长度 dac_channels，第 i 项是喂给 DAC i 的 lane（静音为 null）。
    pub fn dac_map_json(&self) -> Value {
        let mut map: Vec<Value> = vec![Value::Null; usize::from(self.dac_channels)];
        for (lane, dac) in &self.routes {
            if let Some(slot) = map.get_mut(usize::from(*dac)) {
                *slot = json!(lane);
            }
        }
        Value::Array(map)
    }

    /// 响应里的 routing 对象。
    pub fn to_json(&self) -> Value {
        json!({
            "graph_id": self.graph_id,
            "dac_channels": self.dac_channels,
            "mapping": self.mapping_json(),
        })
    }
}

/// 一次已生效的 commit（/api/plan/history 的条目）。
#[derive(Debug, Clone, PartialEq)]
pub struct CommitRecord {
    /// 计划版本号（commit 回执的 version）。
    pub version: u64,
    /// graph_id（commit 回执；拿不到时 None）。
    pub graph_id: Option<String>,
    /// 生效块号（下一块边界；拿不到时 None）。
    pub applied_block: Option<u64>,
    /// 记录时刻（RFC3339）。
    pub recorded_at: String,
    /// 来源（当前恒为 "commit"；rollback 的 commit 也是新版本）。
    pub source: String,
    /// 关联方案的 sha256（按 graph_id 关联到方案库；关联不到为 None）。
    pub plan_sha256: Option<String>,
    /// 关联方案的准入延迟（样本）。
    pub latency_samples: Option<u64>,
}

impl CommitRecord {
    /// history 条目 JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "version": self.version,
            "graph_id": self.graph_id,
            "applied_block": self.applied_block,
            "recorded_at": self.recorded_at,
            "source": self.source,
            "sha256": self.plan_sha256,
            "latency_samples": self.latency_samples,
        })
    }
}

/// 一次上传记录（诊断/历史用；同一 sha 只记一次）。
#[derive(Debug, Clone, PartialEq)]
pub struct UploadRecord {
    /// 内容 sha256。
    pub sha256: String,
    /// graph_id。
    pub graph_id: String,
    /// 上传时刻（RFC3339）。
    pub uploaded_at: String,
    /// 字节数。
    pub size_bytes: usize,
    /// 准入估算延迟（样本）。
    pub latency_samples: u64,
}

/// 已 prepare 的声道映射改动（PUT /api/channels 两阶段的第一阶段产物）。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingChannels {
    /// 提交用的 token（确定性：同 mapping + 同基线得到同一 token，prepare 可重放）。
    pub token: String,
    /// 目标 (lane, dac) 列表。
    pub mapping: Vec<(u16, u16)>,
    /// prepare 时刻（RFC3339）。
    pub created_at: String,
    /// prepare 时的当前 routing（拿不到为 None，此时只做结构校验）。
    pub base: Option<RoutingView>,
}

/// peek/take 声道草稿的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTokenError {
    /// 还没有 prepare 过任何声道草稿。
    NoDraft,
    /// 有草稿但 token 不匹配。
    Mismatch,
}

impl ChannelTokenError {
    /// 稳定错误码（commit 与既有控制面 engine_token_invalid 对齐）。
    pub fn code(self) -> &'static str {
        "engine_token_invalid"
    }

    /// 人读原因。
    pub fn message(self) -> &'static str {
        match self {
            Self::NoDraft => {
                "no prepared channel mapping draft: PUT /api/channels with stage=prepare first"
            }
            Self::Mismatch => "engine_token does not match the prepared channel mapping draft",
        }
    }
}

/// 方案库错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStoreError {
    /// 不是合法的 ABI v3 方案文件。
    Decode(String),
    /// 超过 4 MiB 上限。
    TooLarge(usize),
    /// 存储 IO 失败。
    Io(String),
}

impl PlanStoreError {
    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Decode(_) => "plan_decode_failed",
            Self::TooLarge(_) => "plan_too_large",
            Self::Io(_) => "internal_error",
        }
    }

    /// 人读原因。
    pub fn message(&self) -> String {
        match self {
            Self::Decode(message) => message.clone(),
            Self::TooLarge(size) => format!(
                "plan file is {size} bytes, exceeding the {MAX_PLAN_BYTES}-byte upload limit"
            ),
            Self::Io(message) => format!("cannot store the uploaded plan: {message}"),
        }
    }
}

/// 进程内方案库状态。
#[derive(Debug, Default)]
struct Inner {
    /// sha256 -> 方案元数据。
    plans: BTreeMap<String, PlanRecord>,
    /// graph_id -> sha256（rt-host status 只报 graph_id，用它反查 routing）。
    by_graph: BTreeMap<String, String>,
    /// 已生效历史（时间顺序，最新在末尾）。
    commits: Vec<CommitRecord>,
    /// 上传记录（时间顺序）。
    uploads: Vec<UploadRecord>,
    /// 当前声道草稿（同一时刻只有一份，后到的 prepare 覆盖）。
    pending_channels: Option<PendingChannels>,
}

/// 方案库（Clone 只是共享同一份进程内状态）。
#[derive(Debug, Clone)]
pub struct PlanStore {
    inner: Arc<Mutex<Inner>>,
    dir: PathBuf,
}

impl PlanStore {
    /// 用 daemon 状态目录构造（上传落 `<state_dir>/plans/`）。
    pub fn new(state_dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            dir: state_dir.join("plans"),
        }
    }

    /// 上传目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 上传并记录方案（内容寻址；同 sha 幂等）。返回 (记录, 是否新写入)。
    pub fn store_upload(
        &self,
        bytes: &[u8],
        recorded_at: &str,
    ) -> Result<(PlanRecord, bool), PlanStoreError> {
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(PlanStoreError::TooLarge(bytes.len()));
        }
        let meta = decode_metadata(bytes)?;
        let sha256 = sha256_hex(bytes);
        {
            let inner = self.lock();
            if let Some(existing) = inner.plans.get(&sha256) {
                return Ok((existing.clone(), false));
            }
        }
        let path = self.dir.join(format!("{sha256}.plan"));
        write_atomic(&self.dir, &path, bytes).map_err(PlanStoreError::Io)?;
        let record = meta.into_record(
            sha256.clone(),
            Some(path.clone()),
            bytes.len(),
            PlanSource::Upload,
            recorded_at,
        );
        let mut inner = self.lock();
        if let Some(existing) = inner.plans.get(&sha256) {
            // 并发上传同一份方案：以先写入的记录为准。
            return Ok((existing.clone(), false));
        }
        inner
            .by_graph
            .insert(record.graph_id.clone(), sha256.clone());
        inner.plans.insert(sha256.clone(), record.clone());
        inner.uploads.push(UploadRecord {
            sha256,
            graph_id: record.graph_id.clone(),
            uploaded_at: recorded_at.to_string(),
            size_bytes: record.size_bytes,
            latency_samples: record.latency_samples,
        });
        if inner.uploads.len() > MAX_RECORDS {
            let excess = inner.uploads.len() - MAX_RECORDS;
            inner.uploads.drain(0..excess);
        }
        Ok((record, true))
    }

    /// prepare 成功后按本地 plan_path 记录方案元数据（best-effort：失败静默忽略）。
    pub fn remember_local_plan(&self, path: &Path, recorded_at: &str) -> Option<PlanRecord> {
        let metadata = std::fs::metadata(path).ok()?;
        if metadata.len() > (16 * 1024 * 1024) {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let meta = decode_metadata(&bytes).ok()?;
        let sha256 = sha256_hex(&bytes);
        let mut inner = self.lock();
        if let Some(existing) = inner.plans.get(&sha256) {
            return Some(existing.clone());
        }
        let graph_id = meta.graph_id.clone();
        let record = meta.into_record(
            sha256.clone(),
            Some(path.to_path_buf()),
            bytes.len(),
            PlanSource::LocalFile,
            recorded_at,
        );
        inner.by_graph.insert(graph_id, sha256.clone());
        inner.plans.insert(sha256, record.clone());
        Some(record)
    }

    /// commit 成功后记录已生效历史（响应里没有 graph_id 的 commit 不是方案提交，跳过）。
    pub fn record_commit(&self, response: &Value, source: &str, recorded_at: &str) {
        let Some(graph_id) = response.get("graph_id").and_then(Value::as_str) else {
            return;
        };
        let Some(version) = response.get("version").and_then(Value::as_u64) else {
            return;
        };
        let applied_block = response.get("applied_block").and_then(Value::as_u64);
        let graph_id = graph_id.to_string();
        let mut inner = self.lock();
        let plan_sha256 = inner.by_graph.get(&graph_id).cloned();
        let latency_samples = plan_sha256
            .as_ref()
            .and_then(|sha| inner.plans.get(sha))
            .map(|record| record.latency_samples);
        inner.commits.push(CommitRecord {
            version,
            graph_id: Some(graph_id),
            applied_block,
            recorded_at: recorded_at.to_string(),
            source: source.to_string(),
            plan_sha256,
            latency_samples,
        });
        if inner.commits.len() > MAX_RECORDS {
            let excess = inner.commits.len() - MAX_RECORDS;
            inner.commits.drain(0..excess);
        }
    }

    /// 按 graph_id 查方案元数据（rt-host status 的 graph_id -> routing）。
    pub fn plan_by_graph_id(&self, graph_id: &str) -> Option<PlanRecord> {
        let inner = self.lock();
        let sha = inner.by_graph.get(graph_id)?;
        inner.plans.get(sha).cloned()
    }

    /// 已生效历史（时间顺序，最新在末尾）。
    pub fn commits(&self) -> Vec<CommitRecord> {
        self.lock().commits.clone()
    }

    /// 上传记录（时间顺序）。
    pub fn uploads(&self) -> Vec<UploadRecord> {
        self.lock().uploads.clone()
    }

    /// 保存声道改动草稿（同 mapping + 同基线时幂等返回同一 token）。
    pub fn put_pending_channels(
        &self,
        mapping: Vec<(u16, u16)>,
        base: Option<RoutingView>,
        created_at: &str,
    ) -> PendingChannels {
        let token = channels_token(&mapping, base.as_ref());
        let mut inner = self.lock();
        if let Some(existing) = &inner.pending_channels {
            if existing.token == token {
                return existing.clone();
            }
        }
        let pending = PendingChannels {
            token,
            mapping,
            created_at: created_at.to_string(),
            base,
        };
        inner.pending_channels = Some(pending.clone());
        pending
    }

    /// 校验声道草稿的 token（不消费草稿：commit 未接通时允许重试）。
    pub fn peek_pending_channels(&self, token: &str) -> Result<PendingChannels, ChannelTokenError> {
        let inner = self.lock();
        let Some(pending) = &inner.pending_channels else {
            return Err(ChannelTokenError::NoDraft);
        };
        if pending.token != token {
            return Err(ChannelTokenError::Mismatch);
        }
        Ok(pending.clone())
    }
}

/// 声道 mapping 的确定性 token（mapping + 当前基线 graph_id 的 sha256 前 16 位）。
fn channels_token(mapping: &[(u16, u16)], base: Option<&RoutingView>) -> String {
    let mut canonical = format!(
        "khaen-channels/v1|{}|",
        base.and_then(|view| view.graph_id.as_deref())
            .unwrap_or("-")
    );
    for (lane, dac) in mapping {
        canonical.push_str(&format!("{lane}:{dac};"));
    }
    sha256_hex(canonical.as_bytes())[..16].to_string()
}

/// 解码方案并抽取元数据（不落盘）。
fn decode_metadata(bytes: &[u8]) -> Result<DecodedMeta, PlanStoreError> {
    let plan = Plan::decode(bytes).map_err(|error| {
        PlanStoreError::Decode(format!("plan is not a valid ABI v3 file: {error}"))
    })?;
    let admission = admit(&plan, &AdmissionLimits::default());
    let rejections = admission
        .rejections
        .iter()
        .map(|rejection| format!("{}: {}", rejection.code, rejection.detail))
        .collect();
    Ok(DecodedMeta {
        graph_id: plan.graph_id_hex(),
        sample_rate: plan.sample_rate,
        block_size: plan.block_size,
        sections: section_names(&plan),
        latency_samples: admission.budget.latency_samples,
        admission_accepted: admission.accepted,
        admission_rejections: rejections,
        in_channels: plan.content.as_ref().map(|content| content.in_channels),
        out_lanes: plan.content.as_ref().map(|content| content.out_lanes),
        dac_channels: plan.routing.as_ref().map(|routing| routing.dac_channels),
        routes: plan.routing.as_ref().map(|routing| routing.routes.clone()),
    })
}

/// 方案区段名列表（ABI v3 的五个可选区段）。
fn section_names(plan: &Plan) -> Vec<&'static str> {
    let mut names = Vec::new();
    if plan.content.is_some() {
        names.push("content");
    }
    if plan.crossover.is_some() {
        names.push("crossover");
    }
    if plan.matrix.is_some() {
        names.push("matrix");
    }
    if plan.protection.is_some() {
        names.push("protection");
    }
    if plan.routing.is_some() {
        names.push("routing");
    }
    names
}

/// 解码后的中间元数据。
struct DecodedMeta {
    graph_id: String,
    sample_rate: u32,
    block_size: u32,
    sections: Vec<&'static str>,
    latency_samples: u64,
    admission_accepted: bool,
    admission_rejections: Vec<String>,
    in_channels: Option<u16>,
    out_lanes: Option<u16>,
    dac_channels: Option<u16>,
    routes: Option<Vec<(u16, u16)>>,
}

impl DecodedMeta {
    fn into_record(
        self,
        sha256: String,
        path: Option<PathBuf>,
        size_bytes: usize,
        source: PlanSource,
        recorded_at: &str,
    ) -> PlanRecord {
        PlanRecord {
            sha256,
            graph_id: self.graph_id,
            path,
            size_bytes,
            sample_rate: self.sample_rate,
            block_size: self.block_size,
            sections: self.sections,
            latency_samples: self.latency_samples,
            admission_accepted: self.admission_accepted,
            admission_rejections: self.admission_rejections,
            in_channels: self.in_channels,
            out_lanes: self.out_lanes,
            dac_channels: self.dac_channels,
            routes: self.routes,
            source,
            recorded_at: recorded_at.to_string(),
        }
    }
}

/// 原子写：先写同目录唯一 tmp，再 rename（读方永远看不到半截文件）。
///
/// T71 起 audio-profile 的 profile 文件与生成的方案文件复用同一套原子写语义。
pub(crate) fn write_atomic(dir: &Path, destination: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temporary = dir.join(format!(
        ".{}.{}.{}.tmp",
        destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "plan".to_string()),
        std::process::id(),
        unique
    ));
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, destination).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("cannot install {}: {error}", destination.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use khaen_host::plan::{
        ContentSection, Fold, MatrixSection, ProtectionEntry, ProtectionSection, RoutingSection,
    };

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "khaen-plan-store-{}-{tag}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 两份恒等方案（routing: lane 0 -> dac 0, lane 1 -> dac 2，dac_channels=4）。
    fn plan_bytes() -> Vec<u8> {
        let lanes: u16 = 2;
        let plan = Plan {
            block_size: 128,
            sample_rate: 48_000,
            content: Some(ContentSection {
                in_channels: lanes,
                out_lanes: lanes,
                folds: (0..lanes)
                    .map(|lane| Fold {
                        src: lane,
                        dst: lane,
                        gain: 1.0,
                    })
                    .collect(),
            }),
            crossover: None,
            matrix: Some(MatrixSection {
                rows: lanes,
                cols: lanes,
                decimation: 1,
                interpolation: 1,
                taps: 1,
                coeffs: vec![1.0, 0.0, 0.0, 1.0],
            }),
            protection: Some(ProtectionSection {
                lanes,
                entries: (0..lanes)
                    .map(|lane| ProtectionEntry {
                        lane,
                        delay: 0,
                        polarity: 1,
                        gain_db: 0.0,
                        peq: Vec::new(),
                    })
                    .collect(),
            }),
            routing: Some(RoutingSection {
                dac_channels: 4,
                routes: vec![(0, 0), (1, 2)],
            }),
        };
        plan.encode_checked().expect("encode plan")
    }

    #[test]
    fn upload_is_content_addressed_idempotent_and_reports_metadata() {
        let dir = TempDir::new("upload");
        let store = PlanStore::new(&dir.0);
        let bytes = plan_bytes();
        let (record, created) = store
            .store_upload(&bytes, "2026-11-26T00:00:00Z")
            .expect("upload");
        assert!(created);
        assert_eq!(record.source, PlanSource::Upload);
        assert_eq!(record.sample_rate, 48_000);
        assert_eq!(record.block_size, 128);
        assert_eq!(
            record.sections,
            vec!["content", "matrix", "protection", "routing"]
        );
        assert_eq!(record.dac_channels, Some(4));
        assert_eq!(record.routes, Some(vec![(0, 0), (1, 2)]));
        assert!(record.admission_accepted);
        assert_eq!(record.latency_samples, 0);
        let stored = record.path.as_ref().expect("stored path");
        assert!(stored.is_file());
        assert!(stored.to_string_lossy().ends_with(".plan"));

        // 同内容幂等：created=false 且返回同一 record。
        let (again, created) = store
            .store_upload(&bytes, "2026-11-26T00:00:01Z")
            .expect("again");
        assert!(!created);
        assert_eq!(again.sha256, record.sha256);
        assert_eq!(store.uploads().len(), 1);
        assert_eq!(
            store.plan_by_graph_id(&record.graph_id).map(|r| r.sha256),
            Some(record.sha256)
        );

        // 非法字节 / 超限：稳定错误码。
        let error = store
            .store_upload(b"not a plan", "2026-11-26T00:00:02Z")
            .expect_err("garbage");
        assert_eq!(error.code(), "plan_decode_failed");
        let oversized = vec![0u8; MAX_PLAN_BYTES + 1];
        let error = store
            .store_upload(&oversized, "2026-11-26T00:00:03Z")
            .expect_err("too large");
        assert_eq!(error.code(), "plan_too_large");
    }

    #[test]
    fn local_plan_records_routing_by_graph_id() {
        let dir = TempDir::new("local");
        let store = PlanStore::new(&dir.0);
        let bytes = plan_bytes();
        let path = dir.0.join("local.plan");
        std::fs::write(&path, &bytes).expect("write plan");
        let record = store
            .remember_local_plan(&path, "2026-11-26T00:00:00Z")
            .expect("record");
        assert_eq!(record.source, PlanSource::LocalFile);
        let found = store.plan_by_graph_id(&record.graph_id).expect("by graph");
        assert_eq!(found.routes, Some(vec![(0, 0), (1, 2)]));
        // 不存在的文件：None（best-effort，不 panic）。
        assert!(store
            .remember_local_plan(&dir.0.join("missing.plan"), "now")
            .is_none());
    }

    #[test]
    fn commits_are_recorded_only_for_real_plan_versions_and_link_routing() {
        let dir = TempDir::new("commits");
        let store = PlanStore::new(&dir.0);
        let bytes = plan_bytes();
        let (record, _) = store
            .store_upload(&bytes, "2026-11-26T00:00:00Z")
            .expect("upload");
        let mut response = json!({
            "ok": true,
            "version": 7,
            "graph_id": record.graph_id,
            "applied_block": 42,
        });
        store.record_commit(&response, "commit", "2026-11-26T00:00:05Z");
        let commits = store.commits();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].version, 7);
        assert_eq!(commits[0].applied_block, Some(42));
        assert_eq!(
            commits[0].plan_sha256.as_deref(),
            Some(record.sha256.as_str())
        );

        // format/mode 草稿的 commit 没有 graph_id：不是方案版本，不记录。
        store.record_commit(
            &json!({"ok": true, "version": 8, "changed": false}),
            "commit",
            "now",
        );
        assert_eq!(store.commits().len(), 1);

        // 历史有界。
        response["version"] = json!(9);
        for _ in 0..(MAX_RECORDS + 5) {
            store.record_commit(&response, "commit", "now");
        }
        assert_eq!(store.commits().len(), MAX_RECORDS);
    }

    #[test]
    fn pending_channel_drafts_are_idempotent_and_token_checked() {
        let dir = TempDir::new("pending");
        let store = PlanStore::new(&dir.0);
        let base = RoutingView {
            graph_id: Some("g1".to_string()),
            dac_channels: 4,
            routes: vec![(0, 0), (1, 2)],
        };
        let first = store.put_pending_channels(vec![(0, 1), (1, 3)], Some(base.clone()), "now");
        let again = store.put_pending_channels(vec![(0, 1), (1, 3)], Some(base.clone()), "later");
        assert_eq!(first.token, again.token);
        assert_eq!(first.created_at, "now");

        let peeked = store
            .peek_pending_channels(&first.token)
            .expect("token matches");
        assert_eq!(peeked.mapping, vec![(0, 1), (1, 3)]);
        assert_eq!(
            store
                .peek_pending_channels("deadbeef")
                .expect_err("mismatch"),
            ChannelTokenError::Mismatch
        );

        // 不同 mapping -> 不同 token（拿到新草稿）。
        let other = store.put_pending_channels(vec![(0, 2)], None, "now");
        assert_ne!(other.token, first.token);
        assert_eq!(
            store
                .peek_pending_channels(&first.token)
                .expect_err("old draft replaced"),
            ChannelTokenError::Mismatch
        );
    }

    #[test]
    fn routing_views_serialize_mapping_and_dac_map() {
        let view = RoutingView {
            graph_id: Some("g".to_string()),
            dac_channels: 4,
            routes: vec![(1, 2), (0, 0)],
        };
        assert_eq!(
            view.mapping_json(),
            json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
        );
        assert_eq!(view.dac_map_json(), json!([0, Value::Null, 1, Value::Null]));
    }
}
