//! RT-MODULE: 负向 fixture（RT 内 heap 分配；rt-check forbid 必须 FAIL F-01）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

pub fn bad_alloc() -> Vec<f32> {
    Vec::new()
}
