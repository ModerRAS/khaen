//! RT-MODULE: 负向 fixture（RT 内 impl Drop；rt-check drop-audit 必须 FAIL D-01）。
//!
//! 仅用于 T06 lint 自证，不参与生产构建。

pub struct Bad {
    value: u32,
}

impl Drop for Bad {
    fn drop(&mut self) {
        self.value = 0;
    }
}
