//! 写入前快照。真机落盘到配置目录并只留最近 20 份；mock 不使用这个目录。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::motu::codec::{export_values, import_values, Datastore};
use crate::util;

pub const SNAPSHOT_KEEP: usize = 20;

static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn new_snapshot_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}-{seq:04}", utc_compact(ms))
}

pub fn snapshot_document(id: &str, store: &Datastore) -> Value {
    json!({
        "id": id,
        "saved_unix": util::now_unix(),
        "tool": "khaen-audio-web",
        "values": export_values(store),
    })
}

pub fn datastore_from_document(value: &Value) -> Result<Datastore, String> {
    let values = value.get("values").ok_or("快照缺少 values")?;
    import_values(values)
}

pub struct FileSnapshotStore {
    dir: PathBuf,
}

impl FileSnapshotStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub async fn save(&self, store: &Datastore) -> AppResult<String> {
        let id = new_snapshot_id();
        if !safe_id(&id) {
            return Err(AppError::internal("生成的快照 id 非法"));
        }
        let path = self.dir.join(format!("{id}.json"));
        let text = serde_json::to_string_pretty(&snapshot_document(&id, store))?;
        util::write_atomic(&path, &text).await?;
        if let Err(err) = self.prune().await {
            eprintln!("motu: 清理旧快照失败: {err}");
        }
        Ok(id)
    }

    pub async fn load(&self, id: Option<&str>) -> AppResult<(String, Datastore)> {
        let id = match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => {
                let id = id.strip_suffix(".json").unwrap_or(id);
                if !safe_id(id) {
                    return Err(AppError::bad_request("snapshot_id 含有非法字符"));
                }
                id.to_string()
            }
            None => self
                .latest_id()
                .await?
                .ok_or_else(|| AppError::bad_request("没有可恢复的快照"))?,
        };
        let path = self.dir.join(format!("{id}.json"));
        if !path.is_file() {
            return Err(AppError::not_found(format!("找不到快照 {id}")));
        }
        let text = tokio::fs::read_to_string(&path).await?;
        let doc: Value = serde_json::from_str(&text)?;
        let store = datastore_from_document(&doc).map_err(AppError::internal)?;
        Ok((id, store))
    }

    pub async fn latest_id(&self) -> AppResult<Option<String>> {
        let mut names = self.list_ids().await?;
        Ok(names.pop())
    }

    async fn list_ids(&self) -> AppResult<Vec<String>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                if safe_id(id) {
                    names.push(id.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    async fn prune(&self) -> AppResult<()> {
        let names = self.list_ids().await?;
        if names.len() <= SNAPSHOT_KEEP {
            return Ok(());
        }
        let drop_n = names.len() - SNAPSHOT_KEEP;
        for id in names.into_iter().take(drop_n) {
            let path = self.dir.join(format!("{id}.json"));
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }
}

pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

fn utc_compact(unix_ms: u128) -> String {
    let secs = (unix_ms / 1000) as u64;
    let ms = (unix_ms % 1000) as u32;
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let ss = tod % 60;
    format!("{year:04}{month:02}{day:02}T{hh:02}{mm:02}{ss:02}{ms:03}Z")
}

/// Howard Hinnant civil_from_days。`days` 是从 1970-01-01 起的天数。
fn civil_from_days(days: u64) -> (i32, u32, u32) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (if month <= 2 { y + 1 } else { y }) as i32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motu::codec::PropValue;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_773), (2026, 11, 16));
        assert!(safe_id("20261116T000000000Z-0001"));
        assert!(!safe_id("../etc/passwd"));
        assert!(!safe_id("a/b"));
    }

    #[tokio::test]
    async fn file_store_roundtrip_and_prune() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "motu-snap-{}-{}",
                std::process::id(),
                SEQ.load(Ordering::Relaxed)
            ));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let store = FileSnapshotStore::new(&dir);
        let mut body = Datastore::default();
        body.set("kMainTrim", 0, PropValue::B(28));
        let mut last = String::new();
        for n in 0..21u8 {
            body.set("kMainTrim", 0, PropValue::B(n));
            last = store.save(&body).await.unwrap();
        }
        let names = store.list_ids().await.unwrap();
        assert_eq!(names.len(), SNAPSHOT_KEEP);
        assert_eq!(names.last().unwrap(), &last);
        let (id, loaded) = store.load(None).await.unwrap();
        assert_eq!(id, last);
        assert_eq!(loaded.get("kMainTrim", 0), Some(PropValue::B(20)));
        let err = store.load(Some("../x")).await.unwrap_err();
        assert_eq!(err.code(), "bad_request");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
