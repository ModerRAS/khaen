//! RT-MODULE: 负向 fixture（RT 内 Arc clone；rt-check forbid 必须 FAIL F-03）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

use std::sync::Arc;

pub fn bad_clone(value: &Arc<u32>) -> Arc<u32> {
    Arc::clone(value)
}
