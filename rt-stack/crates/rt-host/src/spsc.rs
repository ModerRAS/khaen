//! [RT] 固定容量 SPSC 环（core-v1 §6.3 / §11.7）——WBS T16 写域。
//!
//! 一个环 = `TspiSpscHeaderV1` 原子头（64 B）+ 固定 4 个 desc 槽。capture→DSP
//! （cap ring）与 DSP→playback（pb ring）都用本模块，容量固定为
//! `SPSC_CAPTURE_SLOTS == SPSC_PLAYBACK_SLOTS == 4`（P-01：禁止扩容/无界队列/链表）。
//! payload（B 帧 × N/O 个 f32）在 `RtArena` 的固定槽里，由 I/O 区在 push 之前写、
//! 由消费侧在 pop 之后只读；**环本身只存 `TspiBlockDescV1` 值，不存指针**（§6.3）。
//!
//! 发布/消费配方（§6.3 逐字实现，P-02；只用 Acquire/Release/Relaxed，禁止 SeqCst/fence）：
//!
//! ```text
//! producer(slot p)                          consumer(slot q)
//!   1. 写 payload（调用方，P-09 先预检查）     1. w = write_pos.load(Acquire)
//!   2. 构造 desc（含 slot_index = p % CAP）   2. if q == w: 空 → 预分配静音 + 计数（P-04）
//!   3. desc_slot[p % CAP] = desc（普通写）    3. desc = desc_slot[q % CAP]（已发布⇒完整，无半块）
//!   4. write_pos.store(p + 1, Release)       4. 校验 desc（§6.1/§6.2/epoch/slot）
//!                                            5. 读 payload（只读）
//!                                            6. read_pos.store(q + 1, Release)
//! ```
//!
//! 语义边界（本模块负责 / 不负责）：
//! - 负责：固定容量；单写单读的 Acquire/Release 发布；满时丢**新**块
//!   `RT_E_QUEUE_OVERRUN`（绝不覆盖消费者未读完的块）；空 `RT_E_QUEUE_UNDERRUN`；
//!   desc 结构/flag/valid_frames/frame_index/epoch/slot 校验；`TspiSpscHeaderV1`
//!   位置 u64 回绕（wrapping 算术，F-23）；`dropped` 计数；环当前 `epoch_id`；
//!   P-09 预检查（`next_slot`/`free_slots`：capture 必须在写 payload 前判断满）。
//! - 不负责：payload 内容与 finite/sanitize（T22/format）；deadline/gate/hard cutoff
//!   （T19/T21）；plan 切换/ack（T20）；事件环（T21）；静音 buffer 分配（T18）。
//!   连续 overrun/underrun 阈值（P-03 连续 ≥4 → 请求新 epoch；P-04 连续 ≥3 →
//!   请求 SafetyMute）由 I/O 侧按本模块返回值计数后执行。
//!
//! RT-MODULE: DSP 纯 worker 路径禁止析构/分配/锁/syscall/日志/panic；本文件是 RT-MODULE，
//! 禁止析构/分配/锁/syscall/日志/panic（core-v1 §10
//! F-01..F-44、§13 R-DROP-01..R-DROP-14）。无 Drop 类型、无 heap、无锁、无 syscall、
//! 无索引 panic（用 `slice::get`）、错误只用 Copy 的 `RtStatus`、计数/位置用
//! `wrapping_*`（位置 u64 实际不可回绕，§3.4 EPOCH_ID_WRAP_BOUND，仅防御性语义）。

use core::cell::UnsafeCell;
use core::sync::atomic::Ordering;

pub use tspi_dsp_core::abi::{TspiBlockDescV1, TspiSpscHeaderV1, ABI_VERSION, TSPI_RING_MAGIC};
pub use tspi_dsp_core::rt::RtStatus;
use tspi_dsp_core::rt::{
    BLOCK_SIZES, MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, SPSC_CAPTURE_SLOTS, SPSC_PLAYBACK_SLOTS,
};

/// 固定环容量：v1 为 4 槽（§3.3/§11.7；P-01 禁止扩容/无界队列）。
pub const SPSC_CAPACITY: usize = SPSC_CAPTURE_SLOTS;

/// 每槽字节数 = `size_of::<TspiBlockDescV1>()`（§11.3/§11.7 entry_bytes/entry_stride）。
pub const SPSC_ENTRY_BYTES: u16 = 64;

/// `flags` 位 0 `VALID`：块有效；=0 只允许内部占位，禁止发布（§6.2）。
pub const FLAG_VALID: u32 = 1 << 0;
/// `flags` 位 1 `DRAIN_TAIL`：停流/epoch 尾块，valid_frames 可 < B（§6.2/P-08）。
pub const FLAG_DRAIN_TAIL: u32 = 1 << 1;
/// `flags` 位 2 `EPOCH_FIRST`：新 epoch 第一块（§6.2）。
pub const FLAG_EPOCH_FIRST: u32 = 1 << 2;
/// `flags` 位 3 `DISCONTINUITY`：同一 epoch 内 I/O 侧 discontinuity（§6.2）。
pub const FLAG_DISCONTINUITY: u32 = 1 << 3;
/// `flags` 位 4 `SAFETY_SILENCE`：预分配安全静音块，payload 必须全零（§6.2）。
pub const FLAG_SAFETY_SILENCE: u32 = 1 << 4;
/// `flags` 位 5 `PLAN_SWITCH`：新 plan_version 的第一块（§6.2）。
pub const FLAG_PLAN_SWITCH: u32 = 1 << 5;
/// `flags` 位 6 `CROSSFADE_TAIL`：crossfade 尾块（§6.2）。
pub const FLAG_CROSSFADE_TAIL: u32 = 1 << 6;
/// `flags` 位 7 `INPUT_SANITIZED`：I/O 区已把 nonfinite 替换为 0（§6.2）。
pub const FLAG_INPUT_SANITIZED: u32 = 1 << 7;
/// 已知 `flags` 掩码：位 8..31 必须为 0（§6.2；未知位 = fault）。
pub const FLAG_KNOWN_MASK: u32 = 0xFF;

/// 固定容量 desc 槽（单写者按发布配方写、单读者按消费配方读）。
///
/// `UnsafeCell` 显式表达“跨线程共享可变、由 SPSC 协议串行化访问”的语义；本类型无
/// Drop、无分配、无锁。槽数组由 admission 一次性分配并 mlock（INV-L3），地址稳定到
/// 进程退出；`slot_index` 只能来自本环固定槽（P-07）。
#[repr(transparent)]
pub struct DescSlot(UnsafeCell<TspiBlockDescV1>);

impl DescSlot {
    /// 空槽（写 `TspiBlockDescV1::ZERO`；VALID=0，未发布前不可见）。
    pub const fn empty() -> Self {
        Self(UnsafeCell::new(TspiBlockDescV1::ZERO))
    }

    /// [NONRT] 由 RtArena 中连续的 desc 区建立槽数组视图（admission/T26 使用）。
    ///
    /// # Safety
    /// - `desc_base` 指向 `capacity` 个连续、按 `TspiBlockDescV1` 对齐（64 B）、
    ///   地址稳定到进程退出的 desc 槽（INV-L3：RtArena 运行期不 realloc/munmap）；
    /// - 返回借用期内不得存在与之冲突的 Rust 引用或第二个环实例（P-02/INV-L4）。
    pub unsafe fn slice_from_raw<'a>(
        desc_base: *mut TspiBlockDescV1,
        capacity: usize,
    ) -> Result<&'a [DescSlot], RtStatus> {
        if desc_base.is_null() {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if capacity != SPSC_CAPACITY {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if (desc_base as usize) % core::mem::align_of::<TspiBlockDescV1>() != 0 {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        // SAFETY: 调用者保证指针/长度/对齐/稳定性（见本函数 Safety 契约）；DescSlot 是
        // repr(transparent) 的 UnsafeCell 包装，布局与对齐同 TspiBlockDescV1。
        Ok(unsafe { core::slice::from_raw_parts(desc_base as *const DescSlot, capacity) })
    }
}

/// 校验环头（非 RT 建立路径）：magic/ABI/entry/容量固定为 §11.7 值。
fn validate_header(header: &TspiSpscHeaderV1) -> Result<(), RtStatus> {
    if header.magic != TSPI_RING_MAGIC {
        return Err(RtStatus::ERR_MAGIC);
    }
    if header.abi_version != ABI_VERSION {
        return Err(RtStatus::ERR_ABI_VERSION);
    }
    if header.entry_bytes != SPSC_ENTRY_BYTES || header.entry_stride != u32::from(SPSC_ENTRY_BYTES)
    {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if header.capacity != SPSC_CAPACITY as u32 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    Ok(())
}

/// 是否 v1 runtime 允许的块长（`BLOCK_SIZES = {64,128}`，§3.2）。
pub const fn is_runtime_block_size(block_size: u16) -> bool {
    block_size == BLOCK_SIZES[0] || block_size == BLOCK_SIZES[1]
}

/// desc 结构与块身份校验（§6.1/§6.2/P-07/P-08），与 epoch 无关的部分。
fn validate_desc_common(desc: &TspiBlockDescV1, slot_index: u32) -> RtStatus {
    if desc.abi_version != ABI_VERSION {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    if !is_runtime_block_size(desc.block_size) {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    if desc.n_in as usize > MAX_INPUT_CHANNELS || desc.n_out as usize > MAX_OUTPUT_CHANNELS {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    if desc.flags & !FLAG_KNOWN_MASK != 0 {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    if desc.flags & FLAG_VALID == 0 {
        // §6.2：VALID=0 只允许内部占位，禁止发布给 playback。
        return RtStatus::RT_E_BLOCK_INVALID;
    }
    let block = desc.block_size as u64;
    let expected_frame = match desc.block_seq.checked_mul(block) {
        Some(value) => value,
        None => return RtStatus::RT_E_ABI_RUNTIME,
    };
    if desc.frame_index != expected_frame {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    let valid = desc.valid_frames as u64;
    if valid > block {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    if valid < block && desc.flags & FLAG_DRAIN_TAIL == 0 {
        // P-08：只有 DRAIN_TAIL 允许 valid_frames < B，其余必须整块。
        return RtStatus::RT_E_BLOCK_INVALID;
    }
    if desc.slot_index != slot_index {
        // P-07：slot 只能来自本环固定槽；跨环引用 = fault（INV-L4）。
        return RtStatus::RT_E_SLOT_STATE;
    }
    RtStatus::OK
}

/// [RT] 校验一个已出环（或即将入环）的 block desc：结构/flag/frame/valid/slot +
/// 期望 epoch（`desc.epoch_id == expected_epoch`，否则 `RT_E_EPOCH_MISMATCH`）。
///
/// `slot_index` 是**本环**为该块分配的槽号；playback/DSP 必须用消费槽号
/// （`read_pos % CAP`）传入，跨环引用返回 `RT_E_SLOT_STATE`。
pub fn validate_block_desc(
    desc: &TspiBlockDescV1,
    slot_index: u32,
    expected_epoch: u64,
) -> RtStatus {
    let status = validate_desc_common(desc, slot_index);
    if !status.is_ok() {
        return status;
    }
    if desc.epoch_id != expected_epoch {
        return RtStatus::RT_E_EPOCH_MISMATCH;
    }
    RtStatus::OK
}

/// 发布前校验（§6.1/§6.2/INV-L7/INV-L8）：结构 + epoch 不得倒退（环当前 epoch 或更新）。
fn validate_desc_for_publish(desc: &TspiBlockDescV1, slot_index: u32, ring_epoch: u64) -> RtStatus {
    let status = validate_desc_common(desc, slot_index);
    if !status.is_ok() {
        return status;
    }
    if desc.epoch_id < ring_epoch {
        // INV-L7/L8：epoch 单调、不跨 epoch 复用历史。
        return RtStatus::RT_E_EPOCH_MISMATCH;
    }
    RtStatus::OK
}

/// 固定容量 SPSC 生产者（唯一写者；capture 或 DSP 的 output 侧）。
pub struct Producer<'a> {
    header: &'a TspiSpscHeaderV1,
    slots: &'a [DescSlot],
}

impl<'a> Producer<'a> {
    /// [NONRT] 绑定环头与 desc 槽数组；返回前校验 §11.7 头字段。
    ///
    /// # Safety
    /// - `header` 与本环 `slots` 同属一个 RtArena ring，地址稳定到进程退出（INV-L3）；
    /// - **全生命周期只有一个 `Producer`** 且只有一个 `Consumer`（P-02/INV-L4）；
    /// - payload 槽的写只经本生产者的发布配方（P-09：满时不得开始写 payload）。
    pub unsafe fn new(
        header: &'a TspiSpscHeaderV1,
        slots: &'a [DescSlot],
    ) -> Result<Self, RtStatus> {
        validate_header(header)?;
        if slots.len() != header.capacity as usize {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        Ok(Self { header, slots })
    }

    /// 环头（只读）。
    pub fn header(&self) -> &'a TspiSpscHeaderV1 {
        self.header
    }

    /// 固定容量（槽数）。
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// 已发布但未被消费的块数（生产端视角；水位/预检查用）。
    pub fn queued(&self) -> u64 {
        let write = self.header.write_pos.load(Ordering::Relaxed);
        let read = self.header.read_pos.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }

    /// 还空着的槽数（`next_slot` 的计数形式）。
    pub fn free_slots(&self) -> u64 {
        (self.slots.len() as u64).saturating_sub(self.queued())
    }

    /// 下一次成功 `try_push` 将使用的 desc/payload 槽号；`None` = 环满。
    ///
    /// P-09：capture 必须在**开始写 payload 之前**用本方法预检查；返回 `None` 时整块
    /// 丢弃并计数（不得先写 payload 再发现无槽，也不得留下半写槽）。
    pub fn next_slot(&self) -> Option<u32> {
        let capacity = self.slots.len() as u64;
        let write = self.header.write_pos.load(Ordering::Relaxed);
        let read = self.header.read_pos.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= capacity {
            return None;
        }
        Some((write % capacity) as u32)
    }

    /// 发布一个 block desc（§6.3 发布配方）。
    ///
    /// 调用者约定：payload 已经按 `next_slot()` 给出的槽写好（P-09），desc 的
    /// `slot_index` 必须等于 `write_pos % CAP`（P-07）。满时**丢弃本块**并递增
    /// `dropped`、返回 `RT_E_QUEUE_OVERRUN`（P-03；绝不覆盖未读块）。desc 非法时
    /// 不发布并返回对应 `RtStatus`（`RT_E_ABI_RUNTIME`/`RT_E_BLOCK_INVALID`/
    /// `RT_E_SLOT_STATE`/`RT_E_EPOCH_MISMATCH`）。
    pub fn try_push(&mut self, desc: TspiBlockDescV1) -> Result<(), RtStatus> {
        let capacity = self.slots.len() as u64;
        // 生产者是 write_pos 唯一写者，Relaxed 自读；消费者用 Release 释放 read_pos。
        let write = self.header.write_pos.load(Ordering::Relaxed);
        let read = self.header.read_pos.load(Ordering::Acquire);
        let used = write.wrapping_sub(read);
        if used > capacity {
            // 位置非法（INV-L7）：不是协议可恢复状态，RT fault。
            return Err(RtStatus::RT_E_INVARIANT);
        }
        if used >= capacity {
            // P-03：满时丢新块；不覆盖消费者尚未读完的块。
            self.header.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(RtStatus::RT_E_QUEUE_OVERRUN);
        }
        let expected_slot = (write % capacity) as u32;
        let status = validate_desc_for_publish(
            &desc,
            expected_slot,
            self.header.epoch_id.load(Ordering::Relaxed),
        );
        if !status.is_ok() {
            return Err(status);
        }
        let slot = match self.slots.get((write % capacity) as usize) {
            Some(cell) => cell,
            None => return Err(RtStatus::RT_E_INVARIANT),
        };
        // 1) 写 desc 槽（仅本写者；payload 已由 capture/DSP 先行写入）。
        // SAFETY: slot 来自 self.slots（get 已证在界内）；本实例是本环唯一写者（P-02/INV-L4），
        // 该写发生在 write_pos 的 Release 发布之前；消费者只在 Acquire 看到新 write_pos
        // 后才读该槽（P-05 无半块）。
        unsafe {
            *slot.0.get() = desc;
        }
        // 2) 环当前 epoch（Relaxed 存储被下面 write_pos 的 Release 覆盖发布；§11.7）。
        self.header.epoch_id.store(desc.epoch_id, Ordering::Relaxed);
        // 3) 单次 Release 发布（P-02：不得用 SeqCst/额外 fence 替代配方）。
        self.header
            .write_pos
            .store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

/// 固定容量 SPSC 消费者（唯一读者；DSP 或 playback 侧）。
pub struct Consumer<'a> {
    header: &'a TspiSpscHeaderV1,
    slots: &'a [DescSlot],
}

impl<'a> Consumer<'a> {
    /// [NONRT] 绑定环头与 desc 槽数组；返回前校验 §11.7 头字段。
    ///
    /// # Safety
    /// - `header` 与本环 `slots` 同属一个 RtArena ring，地址稳定到进程退出（INV-L3）；
    /// - **全生命周期只有一个 `Consumer`** 且只有一个 `Producer`（P-02/INV-L4）。
    pub unsafe fn new(
        header: &'a TspiSpscHeaderV1,
        slots: &'a [DescSlot],
    ) -> Result<Self, RtStatus> {
        validate_header(header)?;
        if slots.len() != header.capacity as usize {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        Ok(Self { header, slots })
    }

    /// 环头（只读）。
    pub fn header(&self) -> &'a TspiSpscHeaderV1 {
        self.header
    }

    /// 固定容量（槽数）。
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// 已发布但未被消费的块数（消费端视角）。
    pub fn queued(&self) -> u64 {
        let read = self.header.read_pos.load(Ordering::Relaxed);
        let write = self.header.write_pos.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }

    /// 环是否为空（P-04：空时 playback 必须补预分配静音并计数，不得忙等/重播旧块）。
    pub fn is_empty(&self) -> bool {
        self.queued() == 0
    }

    /// 尝试消费一个 block desc（§6.3 消费配方，不做字段校验）。
    ///
    /// 返回 `None` 表示环空或位置非法（`RT_E_QUEUE_UNDERRUN` 由调用方按 P-04 计数）。
    /// **生产 playback/DSP 必须用 `try_pop_checked`** 以执行 §6.3 步骤 4 的校验；
    /// 本方法只给出配方最小实现（跨模块/基准工具用）。
    pub fn try_pop(&mut self) -> Option<TspiBlockDescV1> {
        let capacity = self.slots.len() as u64;
        // 消费者是 read_pos 唯一写者，Relaxed 自读；生产者用 Release 发布 write_pos。
        let read = self.header.read_pos.load(Ordering::Relaxed);
        let write = self.header.write_pos.load(Ordering::Acquire);
        let available = write.wrapping_sub(read);
        if available == 0 || available > capacity {
            return None;
        }
        let slot = self.slots.get((read % capacity) as usize)?;
        // SAFETY: read_pos 在界内且 write_pos Acquire 已覆盖该槽 desc 的完整写入（P-02/P-05）；
        // 消费前块已发布 ⇒ desc 完整，不会读到半块。
        let desc = unsafe { *slot.0.get() };
        // 释放槽：Release 让生产者看到空间（P-02）。
        self.header
            .read_pos
            .store(read.wrapping_add(1), Ordering::Release);
        Some(desc)
    }

    /// 尝试消费并校验一个 block desc（§6.3 步骤 4；生产路径）。
    ///
    /// - 空：`Err(RT_E_QUEUE_UNDERRUN)`（playback 补静音 + 计数，P-04）；
    /// - 非法：块被丢弃（read_pos 已推进，等价“丢块 + 事件”中的丢块；事件/计数由
    ///   调用方按返回码写）并返回具体码：`RT_E_EPOCH_MISMATCH`（不是本 epoch）、
    ///   `RT_E_SLOT_STATE`（跨环槽）、`RT_E_BLOCK_INVALID`（VALID=0/valid_frames 非法）、
    ///   `RT_E_ABI_RUNTIME`（结构/flag/frame 非法）、`RT_E_INVARIANT`（位置非法）。
    pub fn try_pop_checked(&mut self, expected_epoch: u64) -> Result<TspiBlockDescV1, RtStatus> {
        let capacity = self.slots.len() as u64;
        let read = self.header.read_pos.load(Ordering::Relaxed);
        let write = self.header.write_pos.load(Ordering::Acquire);
        let available = write.wrapping_sub(read);
        if available > capacity {
            return Err(RtStatus::RT_E_INVARIANT);
        }
        if available == 0 {
            return Err(RtStatus::RT_E_QUEUE_UNDERRUN);
        }
        let read_slot = (read % capacity) as usize;
        let slot = match self.slots.get(read_slot) {
            Some(cell) => cell,
            None => return Err(RtStatus::RT_E_INVARIANT),
        };
        // SAFETY: 同 try_pop：write_pos Acquire 覆盖该槽完整 desc 写，无半块读取。
        let desc = unsafe { *slot.0.get() };
        let status = validate_block_desc(&desc, read_slot as u32, expected_epoch);
        // 非法块按 §6.3 步骤 4 丢弃：推进 read_pos（槽可回收），错误码交给调用方计数/事件。
        self.header
            .read_pos
            .store(read.wrapping_add(1), Ordering::Release);
        if !status.is_ok() {
            return Err(status);
        }
        Ok(desc)
    }
}

// SAFETY: 句柄按值移交唯一 owner 线程（capture / playback / DSP）；P-02 保证本环只有
// 一个写者与一个读者，移动不复制所有权。不实现 Sync：多个线程共享同一句柄没有合法用法。
unsafe impl<'a> Send for Producer<'a> {}
unsafe impl<'a> Send for Consumer<'a> {}

// 编译期静态断言（core-v1 §11.3/§11.7、§13.2、B-12/B-19；断言在 RT 区外执行）。
const _: () = {
    assert!(SPSC_CAPACITY == 4);
    assert!(SPSC_CAPACITY == SPSC_CAPTURE_SLOTS);
    assert!(SPSC_CAPACITY == SPSC_PLAYBACK_SLOTS);
    assert!(SPSC_ENTRY_BYTES == 64);
    assert!(core::mem::size_of::<TspiBlockDescV1>() == SPSC_ENTRY_BYTES as usize);
    assert!(core::mem::align_of::<TspiBlockDescV1>() == 64);
    assert!(core::mem::size_of::<DescSlot>() == core::mem::size_of::<TspiBlockDescV1>());
    assert!(core::mem::align_of::<DescSlot>() == core::mem::align_of::<TspiBlockDescV1>());
    assert!(!core::mem::needs_drop::<TspiBlockDescV1>());
    assert!(!core::mem::needs_drop::<DescSlot>());
    assert!(!core::mem::needs_drop::<Producer<'static>>());
    assert!(!core::mem::needs_drop::<Consumer<'static>>());
};

#[cfg(test)]
mod spsc_tests {
    include!("../tests/T16/spsc_model.rs");
}
