//! MOTU UltraLite-mk5 / CueMix 控制。
//!
//! 协议见 `codec`（对齐 `tools/motu/cuemix_client.py`）。`MockMotu` 在 `TSPI_MOCK=1`
//! 下提供内存设备，不打开套接字、不执行系统命令；`RealMotu` 只在真机模式构造。

mod codec;
mod real;
mod store;

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};
use crate::util;

pub use codec::{trim_to_db, MAIN_TRIM_MAX};
pub use real::RealMotu;

use codec::{
    active_points, analog_alias, apply_ops, bus_name, buses, format_mac, format_uid,
    host_faders_changed, input_name, is_direct_input, is_host_input, is_protected_fader_op,
    mismatches, mix_index, plan_monitor, plan_restore, plan_restore_direct, plan_zero_direct,
    rollback_ops, Datastore, Mismatch, PropValue, WriteOp, FINAL_INSERT_CELLS, NUM_IN,
};
use store::{new_snapshot_id, SNAPSHOT_KEEP};

const RAW_LIMIT: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Default)]
pub struct DiscoveryInfo {
    pub uid: Option<String>,
    pub name: Option<String>,
    pub ip: Option<String>,
    pub ipv6: Option<String>,
    pub model: Option<String>,
    pub version: Option<String>,
    pub iface: Option<String>,
    pub interval: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DeviceInfo {
    pub model: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub api_version: Option<u16>,
    pub sample_rate: Option<i32>,
    pub uid: Option<String>,
    pub mac: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhantomView {
    pub index: u16,
    pub name: String,
    pub on: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectRoute {
    pub bus: u16,
    pub bus_name: String,
    pub input: u16,
    pub input_name: String,
    pub index: u16,
    pub fader: f64,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Crosspoint {
    pub bus: u16,
    pub bus_name: String,
    pub input: u16,
    pub input_name: String,
    pub index: u16,
    pub fader: f64,
    pub mute: bool,
    pub solo: bool,
    pub pan: Option<f64>,
    pub direct: bool,
    pub host: bool,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelName {
    pub index: u16,
    pub name: String,
    pub jack: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MotuStatus {
    pub online: bool,
    pub mock: bool,
    pub reason: Option<String>,
    pub discovery: DiscoveryInfo,
    pub device: DeviceInfo,
    pub last_read_unix: Option<u64>,
    pub main_trim: Option<u8>,
    pub main_db: Option<f64>,
    pub direct_count: usize,
    pub direct: Vec<DirectRoute>,
    pub phantom: Vec<PhantomView>,
    pub last_error: Option<String>,
    pub latest_snapshot_id: Option<String>,
    pub message_count: u32,
    pub cache_age_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotView {
    pub online: bool,
    pub reason: Option<String>,
    pub device: DeviceInfo,
    pub buses: Vec<ChannelName>,
    pub inputs: Vec<ChannelName>,
    pub direct: Vec<DirectRoute>,
    pub nonzero: Vec<Crosspoint>,
    pub raw: Option<Value>,
    pub raw_omitted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteReport {
    pub ok: bool,
    pub changed: usize,
    pub written: usize,
    pub verified: bool,
    pub rollback: bool,
    pub rollback_verified: bool,
    pub snapshot_id: Option<String>,
    pub candidates: usize,
    pub host_inputs_untouched: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WriteError>,
    pub mismatches: Vec<Mismatch>,
}

impl WriteReport {
    fn noop(action: &str, candidates: usize) -> Self {
        Self {
            ok: true,
            changed: 0,
            written: 0,
            verified: true,
            rollback: false,
            rollback_verified: false,
            snapshot_id: None,
            candidates,
            host_inputs_untouched: true,
            message: format!("{action}：已是目标状态，未写设备"),
            error: None,
            mismatches: Vec::new(),
        }
    }

    fn applied(
        action: &str,
        changed: usize,
        candidates: usize,
        snapshot_id: String,
        host_inputs_untouched: bool,
    ) -> Self {
        Self {
            ok: true,
            changed,
            written: changed,
            verified: true,
            rollback: false,
            rollback_verified: false,
            snapshot_id: Some(snapshot_id),
            candidates,
            host_inputs_untouched,
            message: format!("{action}：已写入 {changed} 条差异并读回校验通过"),
            error: None,
            mismatches: Vec::new(),
        }
    }

    fn rolled_back(
        changed: usize,
        written: usize,
        candidates: usize,
        snapshot_id: &str,
        rollback_verified: bool,
        note: &str,
        mismatches: Vec<Mismatch>,
    ) -> Self {
        let message = format!("写入失败，已回滚：{note}");
        Self {
            ok: false,
            changed,
            written,
            verified: false,
            rollback: true,
            rollback_verified,
            snapshot_id: Some(snapshot_id.to_string()),
            candidates,
            host_inputs_untouched: true,
            error: Some(WriteError {
                code: "internal_error".into(),
                message: message.clone(),
            }),
            message,
            mismatches,
        }
    }
}

#[async_trait]
pub trait MotuBackend: Send + Sync {
    async fn status(&self) -> MotuStatus;
    async fn snapshot(&self, raw: bool) -> AppResult<SnapshotView>;
    async fn set_monitor(&self, trim: u8) -> AppResult<WriteReport>;
    async fn direct_mute(&self, mute: bool) -> AppResult<WriteReport>;
    async fn restore(&self, snapshot_id: Option<String>) -> AppResult<WriteReport>;
}

pub fn parse_trim_db(value: f64) -> AppResult<u8> {
    if !value.is_finite()
        || value < 0.0
        || value > f64::from(MAIN_TRIM_MAX)
        || (value - value.round()).abs() > 1e-6
    {
        return Err(AppError::bad_request(format!(
            "trim_db 必须是 0..={MAIN_TRIM_MAX} 的整数（n 表示 -n dB，0 表示 0 dB）"
        )));
    }
    Ok(value.round() as u8)
}

fn device_info(store: &Datastore) -> DeviceInfo {
    DeviceInfo {
        model: store.string_at("kModelID"),
        name: store.string_at("kDevName"),
        version: store.string_at("kDevVersion"),
        api_version: match store.get("kAPIVersion", 0) {
            Some(PropValue::I16(v)) => Some(v),
            _ => None,
        },
        sample_rate: match store.get("kSampleRate", 0) {
            Some(PropValue::I(v)) => Some(v),
            _ => None,
        },
        uid: match store.get("kDevUID", 0) {
            Some(PropValue::A(bytes)) => Some(format_uid(&bytes)),
            _ => None,
        },
        mac: match store.get("kDeviceMAC", 0) {
            Some(PropValue::A(bytes)) => Some(format_mac(&bytes)),
            _ => None,
        },
    }
}

fn main_trim(store: &Datastore) -> Option<u8> {
    match store.get("kMainTrim", 0) {
        Some(PropValue::B(v)) => Some(v),
        _ => None,
    }
}

fn phantom(store: &Datastore) -> Vec<PhantomView> {
    (0..2)
        .map(|index| PhantomView {
            index,
            name: input_name(index).to_string(),
            on: match store.get("k48V", index as usize) {
                Some(PropValue::B(v)) => Some(v != 0),
                _ => None,
            },
        })
        .collect()
}

fn direct_routes(store: &Datastore) -> Vec<DirectRoute> {
    active_points(store)
        .into_iter()
        .filter(|point| is_direct_input(point.input))
        .map(|point| DirectRoute {
            bus: point.bus,
            bus_name: bus_name(point.bus).to_string(),
            input: point.input,
            input_name: input_name(point.input).to_string(),
            index: point.index,
            fader: point.fader,
            alias: analog_alias(point.bus, point.input),
        })
        .collect()
}

fn nonzero(store: &Datastore) -> Vec<Crosspoint> {
    active_points(store)
        .into_iter()
        .map(|point| Crosspoint {
            bus: point.bus,
            bus_name: bus_name(point.bus).to_string(),
            input: point.input,
            input_name: input_name(point.input).to_string(),
            index: point.index,
            fader: point.fader,
            mute: point.mute,
            solo: point.solo,
            pan: point.pan,
            direct: is_direct_input(point.input),
            host: is_host_input(point.input),
            alias: analog_alias(point.bus, point.input),
        })
        .collect()
}

pub(crate) fn build_status(
    store: &Datastore,
    mock: bool,
    online: bool,
    reason: Option<String>,
    discovery: DiscoveryInfo,
    last_read_unix: Option<u64>,
    last_error: Option<String>,
    latest_snapshot_id: Option<String>,
    cache_age_ms: u64,
) -> MotuStatus {
    let direct = if online {
        direct_routes(store)
    } else {
        Vec::new()
    };
    let trim = if online { main_trim(store) } else { None };
    MotuStatus {
        online,
        mock,
        reason,
        discovery,
        device: if online {
            device_info(store)
        } else {
            DeviceInfo::default()
        },
        last_read_unix,
        main_db: trim.map(trim_to_db),
        main_trim: trim,
        direct_count: direct.len(),
        direct,
        phantom: if online { phantom(store) } else { Vec::new() },
        last_error,
        latest_snapshot_id,
        message_count: if online { store.message_count } else { 0 },
        cache_age_ms,
    }
}

pub(crate) fn build_snapshot(
    store: &Datastore,
    online: bool,
    reason: Option<String>,
    _discovery: DiscoveryInfo,
    raw: bool,
) -> SnapshotView {
    let (raw_value, raw_omitted) = if raw && online {
        let values = codec::export_values(store);
        let text = values.to_string();
        if text.len() > RAW_LIMIT {
            (None, true)
        } else {
            (Some(json!({ "values": values })), false)
        }
    } else {
        (None, false)
    };
    SnapshotView {
        online,
        reason,
        device: if online {
            device_info(store)
        } else {
            DeviceInfo::default()
        },
        buses: buses()
            .iter()
            .map(|bus| ChannelName {
                index: bus.index,
                name: bus.name.to_string(),
                jack: Some(bus.jack.to_string()),
            })
            .collect(),
        inputs: (0..NUM_IN)
            .map(|index| ChannelName {
                index,
                name: input_name(index).to_string(),
                jack: None,
            })
            .collect(),
        direct: if online {
            direct_routes(store)
        } else {
            Vec::new()
        },
        nonzero: if online { nonzero(store) } else { Vec::new() },
        raw: raw_value,
        raw_omitted,
    }
}

fn demo_discovery() -> DiscoveryInfo {
    DiscoveryInfo {
        uid: Some("UL5LFE7E6E".into()),
        name: Some("UltraLite-mk5".into()),
        ip: Some("169.254.111.126".into()),
        ipv6: Some("fe80::201:f2ff:fefe:7e6e".into()),
        model: Some("UltraLite-mk5".into()),
        version: Some("2.1.2+3352".into()),
        iface: Some("enx0001f2fe7e6f".into()),
        interval: Some(1),
    }
}

fn demo_datastore() -> Datastore {
    let mut store = Datastore::default();
    store.set("kModelID", 0, PropValue::S("UL5L".into()));
    store.set("kDevName", 0, PropValue::S("UltraLite-mk5".into()));
    store.set("kDevVersion", 0, PropValue::S("2.1.2+3352".into()));
    store.set("kAPIVersion", 0, PropValue::I16(10));
    store.set("kSampleRate", 0, PropValue::I(48_000));
    store.set("kDevUID", 0, PropValue::A(b"UL5LFE7E6E".to_vec()));
    store.set(
        "kDeviceMAC",
        0,
        PropValue::A(vec![0x00, 0x01, 0xf2, 0xfe, 0x7e, 0x6e]),
    );
    store.set("kMainTrim", 0, PropValue::B(28));
    store.set("k48V", 0, PropValue::B(0));
    store.set("k48V", 1, PropValue::B(0));
    let physical = [
        (0u16, 0u16, 1.0f64),
        (0, 1, 1.0),
        (0, 10, 0.935),
        (2, 2, 0.973),
        (4, 4, 0.954),
        (6, 6, 0.935),
        (7, 7, 1.0),
    ];
    for (bus, input, fader) in physical {
        store.set(
            "kiMixFader",
            mix_index(bus, input) as usize,
            PropValue::F(fader),
        );
    }
    let usb = [
        (0u16, 18u16),
        (1, 19),
        (2, 20),
        (3, 21),
        (4, 22),
        (5, 23),
        (6, 24),
        (7, 25),
        (8, 26),
        (9, 27),
        (10, 28),
        (11, 29),
    ];
    for (bus, input) in usb {
        store.set(
            "kiMixFader",
            mix_index(bus, input) as usize,
            PropValue::F(1.0),
        );
    }
    store.message_count = store
        .values
        .values()
        .map(|arr| arr.iter().flatten().count() as u32)
        .sum();
    store
}

struct SavedSnap {
    id: String,
    store: Datastore,
}

struct MockInner {
    store: Datastore,
    snapshots: Vec<SavedSnap>,
    last_error: Option<String>,
    last_read_unix: u64,
    latest_snapshot_id: Option<String>,
}

impl MockInner {
    fn status(&self) -> MotuStatus {
        build_status(
            &self.store,
            true,
            true,
            None,
            demo_discovery(),
            Some(self.last_read_unix),
            self.last_error.clone(),
            self.latest_snapshot_id.clone(),
            0,
        )
    }

    fn remember(&mut self, store: Datastore) -> String {
        let id = new_snapshot_id();
        self.snapshots.push(SavedSnap {
            id: id.clone(),
            store,
        });
        if self.snapshots.len() > SNAPSHOT_KEEP {
            let extra = self.snapshots.len() - SNAPSHOT_KEEP;
            self.snapshots.drain(0..extra);
        }
        self.latest_snapshot_id = Some(id.clone());
        id
    }

    fn snapshot_by_id(&self, id: Option<&str>) -> AppResult<Datastore> {
        match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => self
                .snapshots
                .iter()
                .rev()
                .find(|snap| snap.id == id)
                .map(|snap| snap.store.clone())
                .ok_or_else(|| AppError::not_found(format!("找不到快照 {id}"))),
            None => self
                .snapshots
                .last()
                .map(|snap| snap.store.clone())
                .ok_or_else(|| AppError::bad_request("没有可恢复的快照")),
        }
    }
}

pub struct MockMotu {
    inner: Mutex<MockInner>,
    apply_fails: bool,
}

impl MockMotu {
    pub fn new(apply_fails: bool) -> Self {
        Self {
            inner: Mutex::new(MockInner {
                store: demo_datastore(),
                snapshots: Vec::new(),
                last_error: None,
                last_read_unix: util::now_unix(),
                latest_snapshot_id: None,
            }),
            apply_fails,
        }
    }

    async fn transact(
        &self,
        ops: Vec<WriteOp>,
        candidates: usize,
        action: &str,
        guard_host: bool,
    ) -> AppResult<WriteReport> {
        if guard_host && ops.iter().any(is_protected_fader_op) {
            return Err(AppError::internal("拒绝写入：计划包含 USB 播放路径 18–29"));
        }
        let mut guard = self.inner.lock().await;
        if ops.is_empty() {
            return Ok(WriteReport::noop(action, candidates));
        }
        let before = guard.store.clone();
        let snapshot_id = guard.remember(before.clone());
        if self.apply_fails {
            guard.last_error = Some("mock 注入写入失败，已按快照回滚".into());
            guard.last_read_unix = util::now_unix();
            return Ok(WriteReport::rolled_back(
                ops.len(),
                0,
                candidates,
                &snapshot_id,
                true,
                "mock 注入写入失败，设备状态未改变",
                Vec::new(),
            ));
        }
        apply_ops(&mut guard.store, &ops);
        let bad = mismatches(&guard.store, &ops);
        let host_bad = guard_host && host_faders_changed(&before, &guard.store);
        if bad.is_empty() && !host_bad {
            guard.last_error = None;
            guard.last_read_unix = util::now_unix();
            return Ok(WriteReport::applied(
                action,
                ops.len(),
                candidates,
                snapshot_id,
                !host_faders_changed(&before, &guard.store),
            ));
        }
        let rb = rollback_ops(&before, &ops);
        apply_ops(&mut guard.store, &rb);
        let rollback_verified = mismatches(&guard.store, &rb).is_empty();
        guard.last_error = Some("读回不一致，已回滚".into());
        guard.last_read_unix = util::now_unix();
        let note = if host_bad {
            "写入触到了 USB 播放路径，已回滚".to_string()
        } else {
            format!("读回校验不一致（{} 处），已回滚", bad.len())
        };
        Ok(WriteReport::rolled_back(
            ops.len(),
            ops.len(),
            candidates,
            &snapshot_id,
            rollback_verified,
            &note,
            bad,
        ))
    }
}

#[async_trait]
impl MotuBackend for MockMotu {
    async fn status(&self) -> MotuStatus {
        self.inner.lock().await.status()
    }

    async fn snapshot(&self, raw: bool) -> AppResult<SnapshotView> {
        let guard = self.inner.lock().await;
        Ok(build_snapshot(
            &guard.store,
            true,
            None,
            demo_discovery(),
            raw,
        ))
    }

    async fn set_monitor(&self, trim: u8) -> AppResult<WriteReport> {
        let ops = {
            let guard = self.inner.lock().await;
            plan_monitor(&guard.store, trim)
        };
        self.transact(ops, 1, "设置 Main Out 监听衰减", false).await
    }

    async fn direct_mute(&self, mute: bool) -> AppResult<WriteReport> {
        let ops = {
            let guard = self.inner.lock().await;
            if mute {
                plan_zero_direct(&guard.store)
            } else {
                let snap = guard.snapshot_by_id(None)?;
                plan_restore_direct(&guard.store, &snap)
            }
        };
        let action = if mute {
            "关闭输入直通"
        } else {
            "按快照恢复直通"
        };
        self.transact(ops, FINAL_INSERT_CELLS, action, true).await
    }

    async fn restore(&self, snapshot_id: Option<String>) -> AppResult<WriteReport> {
        let (candidates, ops) = {
            let guard = self.inner.lock().await;
            let snap = guard.snapshot_by_id(snapshot_id.as_deref())?;
            plan_restore(&guard.store, &snap)
        };
        self.transact(ops, candidates, "恢复快照", false).await
    }
}

/// 装配用的句柄。测试通过 `MockMotu` 的失败开关覆盖回滚。
pub fn mock_backend(apply_fails: bool) -> Arc<dyn MotuBackend> {
    Arc::new(MockMotu::new(apply_fails))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_contains_in7_in8_and_spares_usb() {
        let store = demo_datastore();
        let direct = direct_routes(&store);
        assert_eq!(direct.len(), 7);
        assert!(direct
            .iter()
            .any(|row| row.alias.as_deref() == Some("in7→out7")));
        assert!(direct
            .iter()
            .any(|row| row.alias.as_deref() == Some("in8→out8")));
        assert!((store.fader(mix_index(0, 18)) - 1.0).abs() < 1e-6);
        let ops = plan_zero_direct(&store);
        assert_eq!(ops.len(), 7);
        assert!(ops.iter().all(|op| !is_protected_fader_op(op)));
        assert_eq!(main_trim(&store), Some(28));
        assert_eq!(trim_to_db(28), -28.0);
    }

    #[tokio::test]
    async fn mock_mute_restore_and_fail_injection() {
        let motu = MockMotu::new(false);
        let before = motu.status().await;
        assert!(before.online);
        assert_eq!(before.main_trim, Some(28));
        assert_eq!(before.direct_count, 7);
        assert!(before.discovery.ip.as_deref() == Some("169.254.111.126"));

        let muted = motu.direct_mute(true).await.unwrap();
        assert!(muted.ok && muted.verified && !muted.rollback);
        assert_eq!(muted.written, 7);
        assert_eq!(muted.candidates, 280);
        assert!(muted.host_inputs_untouched);
        assert_eq!(motu.status().await.direct_count, 0);
        let snap = motu.snapshot(false).await.unwrap();
        assert!(snap
            .nonzero
            .iter()
            .any(|row| row.host && (row.fader - 1.0).abs() < 1e-6));
        assert!(snap.nonzero.iter().all(|row| !row.direct));

        let restored = motu.direct_mute(false).await.unwrap();
        assert!(restored.ok && restored.verified);
        assert_eq!(motu.status().await.direct_count, 7);

        let monitor = motu.set_monitor(10).await.unwrap();
        assert!(monitor.ok && monitor.verified);
        assert_eq!(motu.status().await.main_trim, Some(10));
        assert_eq!(motu.status().await.main_db, Some(-10.0));

        let back = motu
            .restore(Some(muted.snapshot_id.unwrap()))
            .await
            .unwrap();
        assert!(back.ok);
        assert_eq!(motu.status().await.direct_count, 7);
        assert_eq!(motu.status().await.main_trim, Some(10));

        let failing = MockMotu::new(true);
        let failed = failing.direct_mute(true).await.unwrap();
        assert!(!failed.ok);
        assert!(failed.rollback && failed.rollback_verified);
        assert!(!failed.verified);
        assert_eq!(failed.written, 0);
        assert_eq!(failing.status().await.direct_count, 7);
        assert!(failing.status().await.last_error.is_some());
    }

    #[tokio::test]
    async fn mock_restore_without_snapshot_is_rejected() {
        let motu = MockMotu::new(false);
        let err = motu.restore(None).await.unwrap_err();
        assert_eq!(err.code(), "bad_request");
        let err = motu.direct_mute(false).await.unwrap_err();
        assert_eq!(err.code(), "bad_request");
    }
}
