//! /api/channels 的 daemon 本地镜像（T61）。
//!
//! rt-host 是通道配置的事实源（`channels_status` 每次现取），本模块只存 **daemon 自己知道、
//! rt-host 不报**的两类本地状态：
//!
//! 1. **草稿侧的路由意图**：[`PendingRouting`]。PUT /api/channels 的 mapping/dac_map 是
//!    lane -> DAC 的**方案路由**（属于方案 routing 段），不是 rt-host 的通道协议字段；
//!    prepare 把它作为本地草稿意图记下来，commit/abort 时消费，用来在回执里如实说明
//!    "路由没有（也不可能）被 channels_commit 改写，需要匹配的方案重新 commit"。
//! 2. **最近一次成功 commit 的镜像**：[`LastChannelsCommit`]。GET /api/channels 的
//!    `applied` 段来自它（附记录时刻）；它是**快照**，不是实时数据源，页面仍以
//!    `channels_status` 的 current/device/probe/generation 为准。
//!
//! 另外记录一个**路由失效标记**：rt-host 在引擎格式变化时会在块边界丢弃运行中的方案图
//! （commit 回执 graph_dropped=true）。此后 daemon 记录过的方案 routing 不再代表运行中的图，
//! GET 必须把 mapping 报成 null + 原因（而不是拿旧 routing 冒充当前）。方案重新 commit
//! 成功（回执带 graph_id）后失效标记清除。
//!
//! 全部状态在进程内（daemon 重启即清空；重启后镜像为空 + 原因，不编造）。

use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};

/// 一次 prepare 记下的 lane -> DAC 路由意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRouting {
    /// rt-host channels_prepare 返回的 token（commit/abort 的权威 token）。
    pub token: String,
    /// rt-host 预计生效的代际（有就记录，不编）。
    pub planned_generation: Option<u64>,
    /// prepare 时关联到的当前方案 routing（拿不到为 None，此时无法判断 changed）。
    pub base_mapping: Option<Vec<(u16, u16)>>,
    /// 请求的目标 mapping（dac 升序）。
    pub target_mapping: Vec<(u16, u16)>,
    /// 目标是否与当前 routing 不同（base 未知时为 None）。
    pub mapping_changed: Option<bool>,
    /// prepare 记录时刻（RFC3339 UTC）。
    pub recorded_at: String,
}

impl PendingRouting {
    /// 响应里的 routing 对象。
    pub fn to_json(&self) -> Value {
        json!({
            "token": self.token,
            "planned_generation": self.planned_generation,
            "current": self.base_mapping.as_ref().map(|mapping| mapping_json(mapping)),
            "target": mapping_json(&self.target_mapping),
            "changed": self.mapping_changed,
            "recorded_at": self.recorded_at,
        })
    }
}

/// 一次成功的 channels_commit 的本地镜像（GET /api/channels 的 `applied`）。
#[derive(Debug, Clone, PartialEq)]
pub struct LastChannelsCommit {
    /// 记录时刻（RFC3339 UTC，daemon 时钟）。
    pub recorded_at: String,
    /// rt-host 报告的生效代际。
    pub generation: Option<u64>,
    /// rt-host 回执的 applied（no-op commit 为 false）。
    pub applied: bool,
    /// rt-host 回执的 changed。
    pub changed: Option<bool>,
    /// 实测空档（毫秒；no-op 为 0）。
    pub gap_ms: Option<u64>,
    /// 生效后的数据面格式（rt-host 回执原样）。
    pub format: Option<Value>,
    /// 生效后的通道视图（rt-host 回执的 channels 字段，原样）。
    pub channels: Option<Value>,
    /// 引擎格式变化导致运行中的方案图被丢弃。
    pub graph_dropped: bool,
    /// 引擎被重建（格式变化）。
    pub engine_rebuilt: bool,
    /// 回执的 rolled_back（成功回执恒 false；失败不进镜像）。
    pub rolled_back: bool,
    /// 完整回执（诊断用；GET 的 applied 不整份透传）。
    pub response: Value,
}

impl LastChannelsCommit {
    /// GET /api/channels 的 `applied` 段。
    pub fn to_json(&self) -> Value {
        json!({
            "source": "daemon-channels-commit",
            "recorded_at": self.recorded_at,
            "generation": self.generation,
            "applied": self.applied,
            "changed": self.changed,
            "gap_ms": self.gap_ms,
            "format": self.format,
            "channels": self.channels,
            "graph_dropped": self.graph_dropped,
            "engine_rebuilt": self.engine_rebuilt,
            "rolled_back": self.rolled_back,
            "note": "local mirror of the last channels commit this daemon performed; it is a snapshot, not the live data plane (channels_status is authoritative)",
        })
    }
}

/// 方案路由失效标记（运行中的方案图被 channels_commit 丢弃）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingInvalidation {
    /// rt-host 报告的发生代际（拿不到为 None）。
    pub generation: Option<u64>,
    /// 记录时刻（RFC3339 UTC）。
    pub recorded_at: String,
    /// 人读原因（GET 的 reasons["mapping"] 直接用）。
    pub reason: String,
}

#[derive(Debug, Default)]
struct Inner {
    pending: Option<PendingRouting>,
    last_commit: Option<LastChannelsCommit>,
    routing_invalidated: Option<RoutingInvalidation>,
}

/// daemon 侧的 /api/channels 本地镜像（进程内；Clone 共享同一份状态）。
#[derive(Debug, Default)]
pub struct ChannelsMirror {
    inner: Mutex<Inner>,
}

impl ChannelsMirror {
    /// 新建空镜像。
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 记下（或覆盖）当前路由草稿意图。同一时刻只有一份（后到的 prepare 覆盖）。
    pub fn put_pending(&self, pending: PendingRouting) {
        self.lock().pending = Some(pending);
    }

    /// 按 token 查草稿意图（不消费）。
    pub fn peek_pending(&self, token: &str) -> Option<PendingRouting> {
        self.lock()
            .pending
            .as_ref()
            .filter(|pending| pending.token == token)
            .cloned()
    }

    /// 按 token 取走草稿意图（commit/abort 消费；token 不符时不取）。
    pub fn take_pending(&self, token: &str) -> Option<PendingRouting> {
        let mut inner = self.lock();
        if inner
            .pending
            .as_ref()
            .is_some_and(|pending| pending.token == token)
        {
            inner.pending.take()
        } else {
            None
        }
    }

    /// 丢弃草稿意图（无论 token）。
    pub fn clear_pending(&self) {
        self.lock().pending = None;
    }

    /// 记录一次成功的 channels_commit 回执（只接受 ok=true 的回执）。
    ///
    /// 回执带 graph_dropped=true 时同时登记路由失效（见 [`Self::routing_invalidation`]）。
    /// 返回记录下来的镜像；`ok` 不是 true 或没有 `ok` 字段时返回 None（不把失败当成功）。
    pub fn record_commit(&self, response: &Value, recorded_at: &str) -> Option<LastChannelsCommit> {
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let applied = response
            .get("applied")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let graph_dropped = response
            .get("graph_dropped")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let generation = response.get("generation").and_then(Value::as_u64);
        let commit = LastChannelsCommit {
            recorded_at: recorded_at.to_string(),
            generation,
            applied,
            changed: response.get("changed").and_then(Value::as_bool),
            gap_ms: response.get("gap_ms").and_then(Value::as_u64),
            format: response.get("format").cloned(),
            channels: response.get("channels").cloned(),
            graph_dropped,
            engine_rebuilt: response
                .get("engine_rebuilt")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rolled_back: response
                .get("rolled_back")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            response: response.clone(),
        };
        let mut inner = self.lock();
        inner.last_commit = Some(commit.clone());
        inner.pending = None;
        if applied && graph_dropped {
            inner.routing_invalidated = Some(RoutingInvalidation {
                generation,
                recorded_at: recorded_at.to_string(),
                reason: format!(
                    "the last channels commit changed the engine format and rt-host dropped the running plan graph (graph_dropped at generation {}); the lane -> DAC mapping of the last applied plan is no longer running, so it is reported as unknown until a plan whose routing matches the new output format is prepared and committed again",
                    generation
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                ),
            });
        }
        Some(commit)
    }

    /// 最近一次成功的 commit 镜像。
    pub fn last_commit(&self) -> Option<LastChannelsCommit> {
        self.lock().last_commit.clone()
    }

    /// 当前方案路由失效标记（方案重新 commit 成功后清除）。
    pub fn routing_invalidation(&self) -> Option<RoutingInvalidation> {
        self.lock().routing_invalidated.clone()
    }

    /// 方案 commit 成功（回执带 graph_id）：运行中的图又有了已知 routing，清除失效标记。
    ///
    /// 回执没有 graph_id 时不是方案版本（format/mode 等草稿的 commit），不清除。
    /// 返回是否清了标记。
    pub fn note_plan_commit(&self, response: &Value) -> bool {
        if response.get("graph_id").and_then(Value::as_str).is_none() {
            return false;
        }
        self.lock().routing_invalidated.take().is_some()
    }
}

/// mapping 数组（[{"lane","dac"}]，lane 升序）。
fn mapping_json(mapping: &[(u16, u16)]) -> Value {
    let mut sorted = mapping.to_vec();
    sorted.sort_unstable_by_key(|(lane, _)| *lane);
    Value::Array(
        sorted
            .into_iter()
            .map(|(lane, dac)| json!({ "lane": lane, "dac": dac }))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(token: &str) -> PendingRouting {
        PendingRouting {
            token: token.to_string(),
            planned_generation: Some(2),
            base_mapping: Some(vec![(0, 0), (1, 2)]),
            target_mapping: vec![(0, 1), (1, 3)],
            mapping_changed: Some(true),
            recorded_at: "2026-11-26T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn pending_drafts_are_token_scoped_and_consumed_once() {
        let mirror = ChannelsMirror::new();
        assert!(mirror.peek_pending("t1").is_none());
        mirror.put_pending(pending("t1"));
        assert_eq!(mirror.peek_pending("t1"), Some(pending("t1")));
        // 错 token：不消费，也不返回。
        assert!(mirror.take_pending("t2").is_none());
        assert!(mirror.peek_pending("t1").is_some());
        assert_eq!(mirror.take_pending("t1"), Some(pending("t1")));
        assert!(mirror.peek_pending("t1").is_none(), "take 只消费一次");

        // 后到的 prepare 覆盖前一份草稿。
        mirror.put_pending(pending("t1"));
        mirror.put_pending(pending("t2"));
        assert!(mirror.peek_pending("t1").is_none());
        assert!(mirror.peek_pending("t2").is_some());
        mirror.clear_pending();
        assert!(mirror.peek_pending("t2").is_none());
    }

    #[test]
    fn pending_json_reports_current_target_and_change_flag() {
        let body = pending("t1").to_json();
        assert_eq!(body["token"], "t1");
        assert_eq!(body["planned_generation"], 2);
        assert_eq!(
            body["current"],
            json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
        );
        assert_eq!(
            body["target"],
            json!([{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}])
        );
        assert_eq!(body["changed"], json!(true));
    }

    fn commit_response() -> Value {
        json!({
            "ok": true,
            "op": "channels_commit",
            "applied": true,
            "changed": true,
            "generation": 3,
            "gap_ms": 412,
            "format": { "block_frames": 128, "sample_rate": 48000, "channels": 4 },
            "channels": { "channels": 4, "aes67_map": [0, 1, 2, 3] },
            "engine_rebuilt": true,
            "graph_dropped": true,
            "rolled_back": false,
        })
    }

    #[test]
    fn successful_commits_are_mirrored_and_dropped_graphs_invalidate_the_routing() {
        let mirror = ChannelsMirror::new();
        mirror.put_pending(pending("t1"));
        let recorded = mirror
            .record_commit(&commit_response(), "2026-11-26T00:00:01Z")
            .expect("ok commit is mirrored");
        assert_eq!(recorded.generation, Some(3));
        assert!(recorded.graph_dropped);
        assert!(mirror.peek_pending("t1").is_none(), "commit 消费草稿");
        let applied = mirror.last_commit().expect("mirror").to_json();
        assert_eq!(applied["applied"], json!(true));
        assert_eq!(applied["generation"], json!(3));
        assert_eq!(applied["gap_ms"], json!(412));
        assert_eq!(applied["format"]["channels"], json!(4));
        assert_eq!(applied["graph_dropped"], json!(true));
        assert!(applied["recorded_at"].as_str().is_some());

        let invalidation = mirror.routing_invalidation().expect("graph dropped");
        assert_eq!(invalidation.generation, Some(3));
        assert!(
            invalidation.reason.contains("graph_dropped"),
            "{}",
            invalidation.reason
        );

        // 方案重新 commit（带 graph_id）：清除失效标记。
        assert!(!mirror.note_plan_commit(&json!({"ok": true, "version": 9})));
        assert!(mirror.routing_invalidation().is_some());
        assert!(mirror.note_plan_commit(&json!({"ok": true, "version": 7, "graph_id": "g"})));
        assert!(mirror.routing_invalidation().is_none());
    }

    #[test]
    fn noop_or_failed_commits_do_not_invalidate_the_routing() {
        let mirror = ChannelsMirror::new();
        // 失败回执（ok=false）不镜像。
        assert!(mirror
            .record_commit(
                &json!({"ok": false, "error": "ERR_RECONFIGURE_DEVICE"}),
                "2026-11-26T00:00:01Z"
            )
            .is_none());
        assert!(mirror.last_commit().is_none());
        // no-op 成功（applied=false）：镜像但不失效 routing。
        let mirrored = mirror
            .record_commit(
                &json!({"ok": true, "applied": false, "changed": false, "generation": 0, "gap_ms": 0}),
                "2026-11-26T00:00:02Z",
            )
            .expect("noop mirrored");
        assert!(!mirrored.applied);
        assert!(mirror.routing_invalidation().is_none());
        assert_eq!(mirror.last_commit().expect("mirror").generation, Some(0));
    }

    #[test]
    fn missing_optional_fields_stay_null_instead_of_being_invented() {
        let mirror = ChannelsMirror::new();
        let recorded = mirror
            .record_commit(&json!({"ok": true}), "2026-11-26T00:00:03Z")
            .expect("mirrored");
        assert_eq!(recorded.generation, None);
        assert_eq!(recorded.gap_ms, None);
        assert_eq!(recorded.format, None);
        assert!(!recorded.applied);
        let applied = mirror.last_commit().expect("mirror").to_json();
        assert!(applied["generation"].is_null());
        assert!(applied["gap_ms"].is_null());
        assert!(applied["format"].is_null());
    }
}
