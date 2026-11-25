//! RT-MODULE: 负向 fixture（RT 内返回带 Drop 错误；rt-check forbid 必须 FAIL F-25）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

pub fn bad_error() -> Result<(), String> {
    Err(String::new())
}
