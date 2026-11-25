//! 设备所有权：同一进程内一台设备同一时刻只允许一个所有者。
//!
//! 这是"单设备 owner"规则的进程内落点：重复打开返回错误而不是两份状态各说各话。
//! 注册表只在打开/关闭时触碰（非实时路径），音频线程不读取它。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

fn registry() -> &'static Mutex<HashSet<DeviceId>> {
    static REGISTRY: OnceLock<Mutex<HashSet<DeviceId>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 设备标识（例如 ALSA 设备名或 MOTU 序列号）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    /// 由字符串构造。
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// 设备名。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 取得所有权失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerError {
    /// 设备已被本进程内另一个所有者持有。
    AlreadyOwned(DeviceId),
    /// 设备名为空。
    EmptyDeviceId,
}

impl core::fmt::Display for OwnerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OwnerError::AlreadyOwned(id) => write!(f, "device already owned: {id}"),
            OwnerError::EmptyDeviceId => write!(f, "device id must not be empty"),
        }
    }
}

impl std::error::Error for OwnerError {}

/// 设备所有权守卫；丢弃即释放。
#[derive(Debug)]
pub struct DeviceGuard {
    id: DeviceId,
}

impl DeviceGuard {
    /// 取得设备所有权。
    pub fn acquire(id: impl Into<String>) -> Result<Self, OwnerError> {
        let id = DeviceId::new(id);
        if id.as_str().is_empty() {
            return Err(OwnerError::EmptyDeviceId);
        }
        let mut owned = registry()
            .lock()
            .map_err(|_| OwnerError::AlreadyOwned(id.clone()))?;
        if !owned.insert(id.clone()) {
            return Err(OwnerError::AlreadyOwned(id));
        }
        Ok(Self { id })
    }

    /// 设备标识。
    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    /// 当前进程内被占用的设备数量（仅用于自检与测试）。
    pub fn owned_count() -> usize {
        registry().lock().map(|owned| owned.len()).unwrap_or(0)
    }
}

impl Drop for DeviceGuard {
    fn drop(&mut self) {
        if let Ok(mut owned) = registry().lock() {
            owned.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceGuard, OwnerError};

    #[test]
    fn empty_device_id_is_rejected() {
        assert_eq!(
            DeviceGuard::acquire("").expect_err("empty"),
            OwnerError::EmptyDeviceId
        );
    }

    #[test]
    fn second_owner_is_rejected_then_released() {
        let first = DeviceGuard::acquire("test-owner-device").expect("first");
        assert_eq!(
            DeviceGuard::acquire("test-owner-device").expect_err("second"),
            OwnerError::AlreadyOwned(first.id().clone())
        );
        drop(first);
        let again = DeviceGuard::acquire("test-owner-device").expect("released");
        assert_eq!(again.id().as_str(), "test-owner-device");
        drop(again);
        assert_eq!(DeviceGuard::owned_count(), 0);
    }
}
