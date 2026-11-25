//! RT-MODULE: 正向 fixture（无析构/owned 类型；rt-check drop-audit 必须 PASS）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

use core::sync::atomic::{AtomicU32, Ordering};

pub struct Counter {
    value: AtomicU32,
}

impl Counter {
    pub const fn new() -> Self {
        Self { value: AtomicU32::new(0) }
    }

    pub fn get(&self) -> u32 {
        self.value.load(Ordering::Acquire)
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}
