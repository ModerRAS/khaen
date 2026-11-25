//! RT-MODULE: 正向 fixture（无禁止符号；rt-check forbid 必须 PASS）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

use core::sync::atomic::{AtomicU32, Ordering};

pub fn read_counter(counter: &AtomicU32) -> u32 {
    counter.load(Ordering::Acquire)
}

pub fn write_counter(counter: &AtomicU32, value: u32) {
    counter.store(value, Ordering::Release);
}
