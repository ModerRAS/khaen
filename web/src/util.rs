//! 通用文件/时间工具：原子写、备份、名称校验。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 备份文件序号：同一秒内多次备份也不覆盖。
static BACKUP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 读取文本文件，超限截断。返回 (内容, 是否截断)。
pub async fn read_text_limited(path: &Path, limit: usize) -> std::io::Result<(String, bool)> {
    let bytes = tokio::fs::read(path).await?;
    let truncated = bytes.len() > limit;
    let slice = if truncated {
        &bytes[..limit]
    } else {
        &bytes[..]
    };
    Ok((String::from_utf8_lossy(slice).to_string(), truncated))
}

/// 原子写：同目录临时文件 + rename；Windows 上目标已存在时先删除。
pub async fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    tokio::fs::write(&tmp, content).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows: rename 不能覆盖已存在文件
            let _ = tokio::fs::remove_file(path).await;
            tokio::fs::rename(&tmp, path).await
        }
    }
}

/// 备份文件到 \`<path>.bak.<unix秒>\`；原文件不存在时返回 None。
pub async fn backup_file(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(None);
    }
    let seq = BACKUP_SEQ.fetch_add(1, Ordering::Relaxed);
    let backup = PathBuf::from(format!("{}.bak.{}.{}", path.display(), now_unix(), seq));
    tokio::fs::copy(path, &backup).await?;
    Ok(Some(backup))
}

/// 只保留最近 keep 个 \`<path>.bak.*\` 备份。
pub async fn prune_backups(path: &Path, keep: usize) {
    let Some(parent) = path.parent() else { return };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return;
    };
    let prefix = format!("{name}.bak.");
    let Ok(mut dir) = tokio::fs::read_dir(parent).await else {
        return;
    };
    let mut backups: Vec<((u64, u64), PathBuf)> = Vec::new();
    while let Ok(Some(entry)) = dir.next_entry().await {
        let fname = entry.file_name().to_string_lossy().to_string();
        if let Some(suffix) = fname.strip_prefix(&prefix) {
            let mut parts = suffix.split('.');
            let secs = parts.next().and_then(|v| v.parse::<u64>().ok());
            let seq = parts.next().and_then(|v| v.parse::<u64>().ok());
            if let (Some(secs), Some(seq)) = (secs, seq) {
                backups.push(((secs, seq), entry.path()));
            }
        }
    }
    backups.sort_by_key(|(key, _)| *key);
    let excess = backups.len().saturating_sub(keep);
    for (_, p) in backups.into_iter().take(excess) {
        let _ = tokio::fs::remove_file(p).await;
    }
}

/// 校验配置文件名：只允许单层文件名 + .yml/.yaml 后缀。
pub fn validate_yaml_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 128 {
        return Err("配置文件名长度非法".into());
    }
    if n.contains('/') || n.contains('\\') || n.contains("..") || n.starts_with('.') {
        return Err("配置文件名不得包含路径分隔符或 ..".into());
    }
    let lower = n.to_ascii_lowercase();
    if !lower.ends_with(".yml") && !lower.ends_with(".yaml") {
        return Err("配置文件名必须以 .yml 或 .yaml 结尾".into());
    }
    Ok(())
}

pub fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

/// 测试用临时目录：优先建在 crate 目录（C:），避免本机 TEMP 指向内存盘 R: 时的权限问题。
#[cfg(test)]
pub fn test_tempdir() -> tempfile::TempDir {
    let base = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    tempfile::Builder::new()
        .prefix(".khaen-test-")
        .tempdir_in(base)
        .expect("create test tempdir")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_name_validation() {
        assert!(validate_yaml_name("aes67.yml").is_ok());
        assert!(validate_yaml_name("dante.yaml").is_ok());
        assert!(validate_yaml_name("../etc/passwd.yml").is_err());
        assert!(validate_yaml_name("sub/dir.yml").is_err());
        assert!(validate_yaml_name("aes67.json").is_err());
        assert!(validate_yaml_name(".hidden.yml").is_err());
        assert!(validate_yaml_name("").is_err());
    }

    #[test]
    fn backup_prune_limit() {
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    #[tokio::test]
    async fn atomic_write_and_backup_roundtrip() {
        let dir = test_tempdir();
        let path = dir.path().join("configs").join("a.yml");
        write_atomic(&path, "a: 1\n").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "a: 1\n");
        let backup = backup_file(&path).await.unwrap().unwrap();
        assert!(tokio::fs::try_exists(&backup).await.unwrap());
        // 覆盖写
        write_atomic(&path, "a: 2\n").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "a: 2\n");
        // 备份裁剪
        for _ in 0..3 {
            let _ = backup_file(&path).await.unwrap();
        }
        prune_backups(&path, 2).await;
        let mut count = 0;
        let mut dir = tokio::fs::read_dir(dir.path().join("configs"))
            .await
            .unwrap();
        while let Some(e) = dir.next_entry().await.unwrap() {
            if e.file_name().to_string_lossy().contains(".bak.") {
                count += 1;
            }
        }
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn read_limited_truncates() {
        let dir = test_tempdir();
        let p = dir.path().join("big.txt");
        tokio::fs::write(&p, "x".repeat(100)).await.unwrap();
        let (text, truncated) = read_text_limited(&p, 10).await.unwrap();
        assert!(truncated);
        assert_eq!(text.len(), 10);
    }
}
