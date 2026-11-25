//! RT-MODULE: 负向 fixture（RT 内 drop(Box)；rt-check forbid 必须 FAIL F-27）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

pub fn bad_drop() {
    drop(Box::new(1u32));
}
