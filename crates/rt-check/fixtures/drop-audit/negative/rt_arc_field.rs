//! RT-MODULE: 负向 fixture（RT 结构含 Arc；rt-check drop-audit 必须 FAIL D-01）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

use std::sync::Arc;

pub struct Bad {
    shared: Arc<u32>,
}
