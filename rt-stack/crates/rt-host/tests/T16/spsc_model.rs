// T16（spsc）模型测试：单写单读、FIFO、wrap、overrun/underrun、epoch/frame、
// 无半块读取、cap→DSP→pb 双层模型、无锁/无分配、多线程 Acquire/Release 压测。
//
// 本文件经 src/spsc.rs 末尾的 cfg(test) mod spsc_tests { include!(...) } 进入
// 单测构建（cargo 不会自动发现 tests/<子目录>/*.rs；公共注册属 integrator）。
// 运行：cargo test -p khaen-rt-host spsc
//
// 命名：全部 t16_ 前缀，便于 cargo test 按任务关键字过滤。

use super::*;
use core::sync::atomic::Ordering;

const B64: u16 = 64;
const B128: u16 = 128;

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

/// 生产可用的 block desc（flags 自动补 VALID；slot_index/frame_index 由调用方定）。
fn mk_desc(
    epoch_id: u64,
    block_seq: u64,
    block_size: u16,
    slot_index: u32,
    flags: u32,
) -> KhaenBlockDescV1 {
    KhaenBlockDescV1 {
        abi_version: ABI_VERSION,
        block_size,
        n_in: 2,
        n_out: 2,
        epoch_id,
        block_seq,
        frame_index: block_seq * block_size as u64,
        plan_version: 1,
        flags: flags | FLAG_VALID,
        valid_frames: block_size as u32,
        slot_index,
        mono_publish_ns: 0x1000 + block_seq,
        play_deadline_ns: 0x2000 + block_seq,
    }
}

/// 一个环的固定存储：64 B 原子头 + 4 个 desc 槽（地址稳定即生产等价）。
struct Fixture {
    header: KhaenSpscHeaderV1,
    slots: [DescSlot; SPSC_CAPACITY],
}

impl Fixture {
    fn new() -> Self {
        Self {
            header: KhaenSpscHeaderV1::new(SPSC_CAPACITY as u32, SPSC_ENTRY_BYTES),
            slots: core::array::from_fn(|_| DescSlot::empty()),
        }
    }

    fn slots(&self) -> &[DescSlot] {
        &self.slots
    }
}

fn make_producer<'a>(fx: &'a Fixture) -> Producer<'a> {
    match unsafe { Producer::new(&fx.header, fx.slots()) } {
        Ok(producer) => producer,
        Err(status) => panic!("Producer::new failed: {:?}", status),
    }
}

fn make_consumer<'a>(fx: &'a Fixture) -> Consumer<'a> {
    match unsafe { Consumer::new(&fx.header, fx.slots()) } {
        Ok(consumer) => consumer,
        Err(status) => panic!("Consumer::new failed: {:?}", status),
    }
}

/// 直接写 desc 槽（模拟生产者写入 / 制造损坏），不走发布配方。
fn poke_slot(fx: &Fixture, index: usize, desc: KhaenBlockDescV1) {
    // SAFETY: 测试单线程持有该环，poke 用于复现“未发布即被读”的负向场景（P-05）与损坏块。
    unsafe {
        *fx.slots[index].0.get() = desc;
    }
}

fn read_pos(fx: &Fixture) -> u64 {
    fx.header.read_pos.load(Ordering::Relaxed)
}

fn write_pos(fx: &Fixture) -> u64 {
    fx.header.write_pos.load(Ordering::Relaxed)
}

fn dropped(fx: &Fixture) -> u64 {
    fx.header.dropped.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// A. 环头/构造/视图建立
// ---------------------------------------------------------------------------

#[test]
fn t16_new_valid_ring_reports_fixed_capacity() {
    let fx = Fixture::new();
    let producer = make_producer(&fx);
    let consumer = make_consumer(&fx);
    assert_eq!(producer.capacity(), 4);
    assert_eq!(consumer.capacity(), 4);
    assert_eq!(producer.header().magic, KHAEN_RING_MAGIC);
    assert_eq!(producer.header().abi_version, ABI_VERSION);
    assert_eq!(producer.header().entry_bytes, SPSC_ENTRY_BYTES);
    assert_eq!(producer.header().entry_stride, u32::from(SPSC_ENTRY_BYTES));
    assert_eq!(write_pos(&fx), 0);
    assert_eq!(read_pos(&fx), 0);
    assert!(consumer.is_empty());
    assert_eq!(consumer.queued(), 0);
    assert_eq!(producer.free_slots(), 4);
    assert_eq!(producer.next_slot(), Some(0));
}

#[test]
fn t16_new_rejects_bad_magic() {
    let mut fx = Fixture::new();
    fx.header.magic = 0;
    assert_eq!(
        unsafe { Producer::new(&fx.header, fx.slots()) }.err(),
        Some(RtStatus::ERR_MAGIC)
    );
    assert_eq!(
        unsafe { Consumer::new(&fx.header, fx.slots()) }.err(),
        Some(RtStatus::ERR_MAGIC)
    );
}

#[test]
fn t16_new_rejects_bad_abi_version() {
    let mut fx = Fixture::new();
    fx.header.abi_version = ABI_VERSION + 1;
    assert_eq!(
        unsafe { Producer::new(&fx.header, fx.slots()) }.err(),
        Some(RtStatus::ERR_ABI_VERSION)
    );
}

#[test]
fn t16_new_rejects_bad_entry_layout() {
    let mut fx = Fixture::new();
    fx.header.entry_bytes = 0;
    assert_eq!(
        unsafe { Producer::new(&fx.header, fx.slots()) }.err(),
        Some(RtStatus::ERR_BAD_LAYOUT)
    );
    let mut fx = Fixture::new();
    fx.header.entry_stride = 128;
    assert_eq!(
        unsafe { Consumer::new(&fx.header, fx.slots()) }.err(),
        Some(RtStatus::ERR_BAD_LAYOUT)
    );
}

#[test]
fn t16_new_rejects_bad_capacity_and_slot_len() {
    let mut fx = Fixture::new();
    fx.header.capacity = 2;
    assert_eq!(
        unsafe { Producer::new(&fx.header, fx.slots()) }.err(),
        Some(RtStatus::ERR_BAD_LAYOUT)
    );
    // 头容量 4 但只给 3 个槽（跨环/截断）= 布局不符
    let fx = Fixture::new();
    assert_eq!(
        unsafe { Producer::new(&fx.header, &fx.slots[..3]) }.err(),
        Some(RtStatus::ERR_BAD_LAYOUT)
    );
}

#[test]
fn t16_desc_slots_from_raw_checks_null_align_capacity() {
    let null = unsafe { DescSlot::slice_from_raw(core::ptr::null_mut(), SPSC_CAPACITY) };
    assert_eq!(null.err(), Some(RtStatus::ERR_BAD_LAYOUT));

    let mut store = [KhaenBlockDescV1::ZERO; SPSC_CAPACITY];
    let base = store.as_mut_ptr();
    let misaligned = unsafe { base.cast::<u8>().add(1).cast::<KhaenBlockDescV1>() };
    let bad = unsafe { DescSlot::slice_from_raw(misaligned, SPSC_CAPACITY) };
    assert_eq!(bad.err(), Some(RtStatus::ERR_BAD_LAYOUT));

    let wrong_len = unsafe { DescSlot::slice_from_raw(base, SPSC_CAPACITY - 1) };
    assert_eq!(wrong_len.err(), Some(RtStatus::ERR_BAD_LAYOUT));

    let ok = unsafe { DescSlot::slice_from_raw(base, SPSC_CAPACITY) };
    assert!(ok.is_ok());
    assert_eq!(ok.map(|slots| slots.len()).unwrap_or(0), SPSC_CAPACITY);
}

// ---------------------------------------------------------------------------
// B. 发布 / 消费基本语义（P-02 配方）
// ---------------------------------------------------------------------------

#[test]
fn t16_push_pop_roundtrip_single_desc() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    let desc = mk_desc(1, 0, B64, 0, 0);
    assert!(producer.try_push(desc).is_ok());
    assert_eq!(write_pos(&fx), 1);
    assert_eq!(consumer.queued(), 1);
    assert_eq!(
        fx.header.epoch_id.load(Ordering::Relaxed),
        desc.epoch_id,
        "环当前 epoch 随发布更新（§11.7）"
    );
    let got = consumer.try_pop_checked(1).expect("pop");
    assert_eq!(got, desc, "desc 必须逐字段原样返回（无半块/无丢失）");
    assert_eq!(read_pos(&fx), 1);
    assert!(consumer.is_empty());
    assert_eq!(
        consumer.try_pop_checked(1).err(),
        Some(RtStatus::RT_E_QUEUE_UNDERRUN)
    );
}

#[test]
fn t16_fifo_order_preserved() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    for seq in 0..3u64 {
        let slot = producer.next_slot().expect("slot");
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
    }
    for seq in 0..3u64 {
        let got = consumer.try_pop_checked(1).expect("pop");
        assert_eq!(got.block_seq, seq, "FIFO 顺序");
        assert_eq!(got.frame_index, seq * B64 as u64);
    }
}

#[test]
fn t16_pop_empty_returns_none_and_keeps_positions() {
    let fx = Fixture::new();
    let mut consumer = make_consumer(&fx);
    assert!(consumer.try_pop().is_none());
    assert_eq!(
        consumer.try_pop_checked(1).err(),
        Some(RtStatus::RT_E_QUEUE_UNDERRUN)
    );
    assert_eq!(read_pos(&fx), 0);
    assert_eq!(write_pos(&fx), 0);
}

#[test]
fn t16_checked_pop_reports_underrun_while_unchecked_is_none() {
    let fx = Fixture::new();
    let mut consumer = make_consumer(&fx);
    assert!(consumer.try_pop().is_none());
    assert_eq!(
        consumer.try_pop_checked(1).err(),
        Some(RtStatus::RT_E_QUEUE_UNDERRUN)
    );
}

#[test]
fn t16_frame_index_matches_block_seq_times_block_size() {
    for block_size in [B64, B128] {
        let fx = Fixture::new();
        let mut producer = make_producer(&fx);
        let mut consumer = make_consumer(&fx);
        for seq in [0u64, 1, 5, 17] {
            let slot = producer.next_slot().expect("slot");
            let desc = mk_desc(1, seq, block_size, slot, 0);
            assert_eq!(desc.frame_index, seq * block_size as u64);
            assert!(producer.try_push(desc).is_ok());
            let got = consumer.try_pop_checked(1).expect("pop");
            assert_eq!(got.block_seq, seq);
            assert_eq!(got.frame_index, seq * block_size as u64);
        }
    }
}

#[test]
fn t16_drain_tail_allows_partial_valid_frames() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    let mut desc = mk_desc(1, 0, B128, 0, FLAG_DRAIN_TAIL);
    desc.valid_frames = 37;
    assert!(producer.try_push(desc).is_ok());
    let got = consumer.try_pop_checked(1).expect("pop");
    assert_eq!(got.flags & FLAG_DRAIN_TAIL, FLAG_DRAIN_TAIL);
    assert_eq!(got.valid_frames, 37);
    assert_eq!(got.block_size, B128);
}

// ---------------------------------------------------------------------------
// C. overrun / underrun / 槽回收（P-03/P-04/P-09）
// ---------------------------------------------------------------------------

#[test]
fn t16_overrun_drops_new_block_and_preserves_unread() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    for seq in 0..4u64 {
        let slot = producer.next_slot().expect("slot");
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
    }
    assert_eq!(producer.next_slot(), None);
    assert_eq!(producer.free_slots(), 0);
    // P-03：满时丢“新”块，绝不覆盖消费者未读完的 slot 0。
    assert_eq!(
        producer.try_push(mk_desc(1, 4, B64, 0, 0)).err(),
        Some(RtStatus::RT_E_QUEUE_OVERRUN)
    );
    assert_eq!(dropped(&fx), 1);
    assert_eq!(write_pos(&fx), 4, "overrun 不得推进 write_pos");
    for seq in 0..4u64 {
        let got = consumer.try_pop_checked(1).expect("pop");
        assert_eq!(got.block_seq, seq, "未读块未被覆盖");
    }
    assert!(consumer.is_empty());
}

#[test]
fn t16_push_after_consume_reuses_freed_slot() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    for seq in 0..4u64 {
        let slot = producer.next_slot().expect("slot");
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
    }
    assert_eq!(
        producer.try_push(mk_desc(1, 4, B64, 0, 0)).err(),
        Some(RtStatus::RT_E_QUEUE_OVERRUN)
    );
    assert_eq!(dropped(&fx), 1);
    let first = consumer.try_pop_checked(1).expect("pop");
    assert_eq!(first.block_seq, 0);
    let slot = producer.next_slot().expect("slot after consume");
    assert_eq!(slot, 0, "write_pos=4 回绕到槽 0");
    assert!(producer.try_push(mk_desc(1, 4, B64, slot, 0)).is_ok());
    assert_eq!(dropped(&fx), 1, "成功 push 不再计 dropped");
    let mut order = Vec::new();
    while let Ok(desc) = consumer.try_pop_checked(1) {
        order.push(desc.block_seq);
    }
    assert_eq!(order, [1u64, 2, 3, 4]);
}

#[test]
fn t16_invalid_desc_push_does_not_count_as_overrun() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut bad = mk_desc(1, 0, B64, 0, 0);
    bad.flags = 0; // VALID=0
    assert_eq!(
        producer.try_push(bad).err(),
        Some(RtStatus::RT_E_BLOCK_INVALID)
    );
    assert_eq!(dropped(&fx), 0, "非法 desc 拒绝不等于环满 overrun");
    assert_eq!(write_pos(&fx), 0);
}

#[test]
fn t16_next_slot_precheck_prevents_half_filled_payload() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut payload = [0u64; SPSC_CAPACITY];
    for seq in 0..4u64 {
        // P-09：先预检查槽号，再写 payload，最后发布。
        let slot = producer.next_slot().expect("space");
        payload[slot as usize] = 0xA000 + seq;
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
    }
    // 环满：capture 必须在写 payload 之前失败，payload 保持第 4 块内容。
    match producer.next_slot() {
        Some(_) => panic!("满环不得给出槽号"),
        None => {}
    }
    assert_eq!(payload, [0xA000, 0xA001, 0xA002, 0xA003]);
    assert_eq!(
        producer.try_push(mk_desc(1, 4, B64, 0, 0)).err(),
        Some(RtStatus::RT_E_QUEUE_OVERRUN)
    );
}

#[test]
fn t16_queued_and_free_slots_track_capacity() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    for step in 0..4u64 {
        assert_eq!(producer.queued(), step);
        assert_eq!(producer.free_slots(), 4 - step);
        let slot = producer.next_slot().expect("slot");
        assert!(producer.try_push(mk_desc(1, step, B64, slot, 0)).is_ok());
    }
    assert_eq!(producer.queued(), 4);
    assert_eq!(producer.free_slots(), 0);
    assert_eq!(consumer.queued(), 4);
    assert!(!consumer.is_empty());
    consumer.try_pop_checked(1).expect("pop");
    assert_eq!(producer.queued(), 3);
    assert_eq!(producer.free_slots(), 1);
}

// ---------------------------------------------------------------------------
// D. 发布前校验（§6.1/§6.2/P-07/INV-L7）
// ---------------------------------------------------------------------------

#[test]
fn t16_push_rejects_unknown_flag_bits() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut desc = mk_desc(1, 0, B64, 0, 0);
    desc.flags |= 1 << 8; // 位 8..31 必须为 0
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_ABI_RUNTIME)
    );
    assert_eq!(write_pos(&fx), 0, "非法 desc 不得发布");
}

#[test]
fn t16_push_rejects_placeholder_block() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut desc = mk_desc(1, 0, B64, 0, 0);
    desc.flags = FLAG_DRAIN_TAIL; // VALID=0：只允许内部占位
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_BLOCK_INVALID)
    );
}

#[test]
fn t16_push_requires_drain_tail_for_partial_block() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut desc = mk_desc(1, 0, B128, 0, 0);
    desc.valid_frames = 64; // < B 且无 DRAIN_TAIL
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_BLOCK_INVALID)
    );
    let mut desc = mk_desc(1, 0, B128, 0, FLAG_DRAIN_TAIL);
    desc.valid_frames = 64;
    assert!(producer.try_push(desc).is_ok());
}

#[test]
fn t16_push_rejects_slot_index_mismatch() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let desc = mk_desc(1, 0, B64, 1, 0); // write_pos=0 → 槽 0
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_SLOT_STATE)
    );
    assert_eq!(write_pos(&fx), 0);
}

#[test]
fn t16_push_rejects_epoch_regression() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    assert!(producer.try_push(mk_desc(5, 0, B64, 0, 0)).is_ok());
    let stale = mk_desc(4, 1, B64, 1, 0);
    assert_eq!(
        producer.try_push(stale).err(),
        Some(RtStatus::RT_E_EPOCH_MISMATCH)
    );
    assert_eq!(write_pos(&fx), 1, "旧 epoch 不得入环（INV-L7/L8）");
    assert!(producer.try_push(mk_desc(6, 1, B64, 1, 0)).is_ok());
}

#[test]
fn t16_push_rejects_frame_index_mismatch() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut desc = mk_desc(1, 2, B64, 0, 0);
    desc.frame_index = 0;
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_ABI_RUNTIME)
    );
}

#[test]
fn t16_push_rejects_bad_block_size_and_channels() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut desc = mk_desc(1, 0, B64, 0, 0);
    desc.block_size = 32;
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_ABI_RUNTIME)
    );
    let mut desc = mk_desc(1, 0, B64, 0, 0);
    desc.n_in = 21;
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_ABI_RUNTIME)
    );
    let mut desc = mk_desc(1, 0, B64, 0, 0);
    desc.n_out = 23;
    assert_eq!(
        producer.try_push(desc).err(),
        Some(RtStatus::RT_E_ABI_RUNTIME)
    );
}

#[test]
fn t16_validate_block_desc_codes() {
    let base = mk_desc(9, 3, B64, 2, 0);
    assert_eq!(validate_block_desc(&base, 2, 9), RtStatus::OK);
    assert!(
        is_runtime_block_size(B64) && is_runtime_block_size(B128),
        "64/128 是唯一 runtime 块长"
    );
    assert!(!is_runtime_block_size(32) && !is_runtime_block_size(256));

    let mut desc = base;
    desc.abi_version = ABI_VERSION + 1;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::RT_E_ABI_RUNTIME);

    let mut desc = base;
    desc.block_size = 256;
    desc.frame_index = 0;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::RT_E_ABI_RUNTIME);

    let mut desc = base;
    desc.flags |= 1 << 31;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::RT_E_ABI_RUNTIME);

    let mut desc = base;
    desc.flags = 0;
    assert_eq!(
        validate_block_desc(&desc, 2, 9),
        RtStatus::RT_E_BLOCK_INVALID
    );

    let mut desc = base;
    desc.frame_index = 1;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::RT_E_ABI_RUNTIME);

    let mut desc = base;
    desc.valid_frames = 65;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::RT_E_ABI_RUNTIME);

    let mut desc = base;
    desc.valid_frames = 32;
    assert_eq!(
        validate_block_desc(&desc, 2, 9),
        RtStatus::RT_E_BLOCK_INVALID
    );

    let mut desc = base;
    desc.valid_frames = 32;
    desc.flags |= FLAG_DRAIN_TAIL;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::OK);

    let mut desc = base;
    desc.slot_index = 1;
    assert_eq!(validate_block_desc(&desc, 2, 9), RtStatus::RT_E_SLOT_STATE);

    let mut desc = base;
    desc.epoch_id = 8;
    assert_eq!(
        validate_block_desc(&desc, 2, 9),
        RtStatus::RT_E_EPOCH_MISMATCH
    );

    let mut desc = base;
    desc.block_seq = u64::MAX;
    desc.frame_index = u64::MAX;
    assert_eq!(
        validate_block_desc(&desc, 2, 9),
        RtStatus::RT_E_ABI_RUNTIME,
        "block_seq * B 溢出必须显式拒绝（F-23）"
    );
}

#[test]
fn t16_ring_epoch_follows_published_blocks() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    assert_eq!(fx.header.epoch_id.load(Ordering::Relaxed), 0);
    let first = mk_desc(3, 0, B64, 0, 0);
    assert!(producer.try_push(first).is_ok());
    assert_eq!(fx.header.epoch_id.load(Ordering::Relaxed), 3);
    assert_eq!(consumer.try_pop_checked(3).expect("pop"), first);
    let next = mk_desc(4, 1, B64, 1, FLAG_EPOCH_FIRST);
    assert!(producer.try_push(next).is_ok());
    assert_eq!(fx.header.epoch_id.load(Ordering::Relaxed), 4);
    // 新 epoch 必须由 playback 重建 epoch 后消费：旧 epoch 期望值 = 丢弃（不跨 epoch 拼接）。
    assert_eq!(
        consumer.try_pop_checked(3).err(),
        Some(RtStatus::RT_E_EPOCH_MISMATCH)
    );
    assert_eq!(read_pos(&fx), 2, "非法块按 §6.3 步骤 4 丢弃并推进 read_pos");
}

#[test]
fn t16_checked_pop_drops_corrupt_desc_and_continues() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    let good = mk_desc(1, 0, B64, 0, 0);
    assert!(producer.try_push(good).is_ok());
    // 槽 0 被写坏（slot_index=3 指向别的环）
    poke_slot(&fx, 0, mk_desc(1, 0, B64, 3, 0));
    assert_eq!(
        consumer.try_pop_checked(1).err(),
        Some(RtStatus::RT_E_SLOT_STATE)
    );
    assert_eq!(read_pos(&fx), 1);
    // 后续合法块仍可消费
    let next = mk_desc(1, 1, B64, 1, 0);
    assert!(producer.try_push(next).is_ok());
    assert_eq!(consumer.try_pop_checked(1).expect("pop"), next);
}

// ---------------------------------------------------------------------------
// E. 无半块读取 / 未发布槽不可见（P-05/P-09）
// ---------------------------------------------------------------------------

#[test]
fn t16_unpublished_torn_desc_is_invisible_until_publish() {
    let fx = Fixture::new();
    let mut consumer = make_consumer(&fx);
    // 模拟 capture 已经写完 desc 槽、但还没 Release 发布 write_pos。
    let desc = mk_desc(1, 0, B64, 0, 0);
    poke_slot(&fx, 0, desc);
    assert!(consumer.try_pop().is_none(), "未发布槽不得被消费（P-05）");
    assert_eq!(
        consumer.try_pop_checked(1).err(),
        Some(RtStatus::RT_E_QUEUE_UNDERRUN)
    );
    assert_eq!(read_pos(&fx), 0);
    let mut producer = make_producer(&fx);
    assert!(producer.try_push(desc).is_ok());
    assert_eq!(consumer.try_pop_checked(1).expect("pop"), desc);
}

#[test]
fn t16_consumer_reads_only_published_slots() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    let desc = mk_desc(1, 0, B64, 0, 0);
    assert!(producer.try_push(desc).is_ok());
    // slot 1 未发布；即使槽里有残留/垃圾也不能被读。
    poke_slot(&fx, 1, mk_desc(77, 99, B128, 1, FLAG_EPOCH_FIRST));
    assert_eq!(consumer.try_pop_checked(1).expect("pop"), desc);
    assert!(consumer.is_empty());
    assert_eq!(
        consumer.try_pop_checked(1).err(),
        Some(RtStatus::RT_E_QUEUE_UNDERRUN)
    );
}

// ---------------------------------------------------------------------------
// F. wrap / 位置回绕（u64 单调，wrapping 语义，F-23）
// ---------------------------------------------------------------------------

#[test]
fn t16_positions_wrap_at_u64_boundary() {
    let fx = Fixture::new();
    // 预置：write_pos = MAX-1、read_pos = MAX-3 → 已排队 2 块，槽 0/1（(MAX-3)%4=0）。
    poke_slot(&fx, 0, mk_desc(1, 10, B64, 0, 0));
    poke_slot(&fx, 1, mk_desc(1, 11, B64, 1, 0));
    fx.header.write_pos.store(u64::MAX - 1, Ordering::Release);
    fx.header.read_pos.store(u64::MAX - 3, Ordering::Release);
    fx.header.epoch_id.store(1, Ordering::Release);
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    assert_eq!(consumer.queued(), 2);
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 10);
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 11);
    assert_eq!(read_pos(&fx), u64::MAX - 1);
    // write_pos = MAX-1 → 槽 2；发布后 write_pos = MAX。
    assert_eq!(producer.next_slot(), Some(2));
    assert!(producer.try_push(mk_desc(1, 12, B64, 2, 0)).is_ok());
    assert_eq!(write_pos(&fx), u64::MAX);
    // write_pos = MAX → 槽 3；发布后回绕到 0（不得 panic/不得丢语义）。
    assert_eq!(producer.next_slot(), Some(3));
    assert!(producer.try_push(mk_desc(1, 13, B64, 3, 0)).is_ok());
    assert_eq!(write_pos(&fx), 0);
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 12);
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 13);
    assert_eq!(read_pos(&fx), 0);
    assert!(consumer.is_empty());
}

#[test]
fn t16_slot_rotation_wraps_over_many_blocks() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    for seq in 0..1000u64 {
        let slot = producer.next_slot().expect("slot");
        assert_eq!(slot, (seq % 4) as u32, "槽号按 write_pos % 4 轮转");
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
        let got = consumer.try_pop_checked(1).expect("pop");
        assert_eq!(got.block_seq, seq);
        assert_eq!(got.slot_index, slot);
        assert_eq!(got.frame_index, seq * B64 as u64);
    }
    assert_eq!(write_pos(&fx), 1000);
    assert_eq!(read_pos(&fx), 1000);
}

// ---------------------------------------------------------------------------
// G. cap → DSP → pb 双层模型（WBS T16 “固定容量 cap→DSP→pb ring”）
// ---------------------------------------------------------------------------

#[test]
fn t16_pipeline_capture_dsp_playback_delivers_in_order() {
    let cap_fx = Fixture::new();
    let pb_fx = Fixture::new();
    let mut cap_producer = make_producer(&cap_fx);
    let mut cap_consumer = make_consumer(&cap_fx);
    let mut pb_producer = make_producer(&pb_fx);
    let mut pb_consumer = make_consumer(&pb_fx);
    let mut cap_payload = [0u64; SPSC_CAPACITY];
    let mut pb_payload = [0u64; SPSC_CAPACITY];
    const BLOCKS: u64 = 8;
    for step in 0..BLOCKS {
        // capture：预检查 → 写 payload → 发布。
        let cap_slot = cap_producer.next_slot().expect("cap space");
        cap_payload[cap_slot as usize] = 0xA000 + step;
        let mut flags = 0u32;
        if step == 0 {
            flags |= FLAG_EPOCH_FIRST;
        }
        assert!(cap_producer
            .try_push(mk_desc(1, step, B64, cap_slot, flags))
            .is_ok());
        // DSP：消费 → 读 payload → 处理 → 发布到 pb ring。
        let din = cap_consumer.try_pop_checked(1).expect("dsp pop");
        assert_eq!(din.block_seq, step);
        assert_eq!(din.frame_index, step * B64 as u64);
        assert_eq!(validate_block_desc(&din, din.slot_index, 1), RtStatus::OK);
        assert_eq!(cap_payload[din.slot_index as usize], 0xA000 + step);
        let pb_slot = pb_producer.next_slot().expect("pb space");
        pb_payload[pb_slot as usize] = cap_payload[din.slot_index as usize] + 1;
        assert!(pb_producer
            .try_push(mk_desc(
                din.epoch_id,
                din.block_seq,
                din.block_size,
                pb_slot,
                0
            ))
            .is_ok());
        // playback：消费 → 校验 payload 与身份连续性。
        let dout = pb_consumer.try_pop_checked(1).expect("playback pop");
        assert_eq!(dout.epoch_id, 1);
        assert_eq!(dout.block_seq, step);
        assert_eq!(dout.frame_index, step * B64 as u64);
        assert_eq!(dout.slot_index, pb_slot);
        assert_eq!(pb_payload[dout.slot_index as usize], 0xA001 + step);
    }
    assert_eq!(cap_producer.queued(), 0);
    assert_eq!(pb_producer.queued(), 0);
    assert_eq!(dropped(&cap_fx), 0);
    assert_eq!(dropped(&pb_fx), 0);
}

#[test]
fn t16_pipeline_capture_overrun_when_dsp_stalled_then_recovers() {
    let cap_fx = Fixture::new();
    let mut producer = make_producer(&cap_fx);
    let mut consumer = make_consumer(&cap_fx);
    // capture 连续推 4 块（DSP 停顿，未消费）。
    for seq in 0..4u64 {
        let slot = producer.next_slot().expect("space");
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
    }
    // P-09 预检查失败 → capture 丢块（不写 payload、不覆盖未读块）。
    assert_eq!(producer.next_slot(), None);
    assert_eq!(
        producer.try_push(mk_desc(1, 4, B64, 0, 0)).err(),
        Some(RtStatus::RT_E_QUEUE_OVERRUN)
    );
    assert_eq!(dropped(&cap_fx), 1);
    // DSP 恢复：消费 2 块 → capture 继续推 seq 5（seq 4 已被丢弃 → 可见 gap）。
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 0);
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 1);
    let slot = producer.next_slot().expect("space after drain");
    assert!(producer.try_push(mk_desc(1, 5, B64, slot, 0)).is_ok());
    let mut seen = Vec::new();
    while let Ok(desc) = consumer.try_pop_checked(1) {
        seen.push(desc.block_seq);
    }
    assert_eq!(
        seen,
        [2u64, 3, 5],
        "被丢弃块表现为 block_seq gap，不得伪装连续"
    );
    assert_eq!(dropped(&cap_fx), 1);
}

#[test]
fn t16_pipeline_playback_overrun_drops_dsp_output_then_recovers() {
    let pb_fx = Fixture::new();
    let mut producer = make_producer(&pb_fx);
    let mut consumer = make_consumer(&pb_fx);
    for seq in 0..4u64 {
        let slot = producer.next_slot().expect("pb space");
        assert!(producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok());
    }
    // playback 满：DSP 输出必须丢弃（P-03），不覆盖未读块。
    assert_eq!(producer.next_slot(), None);
    assert_eq!(
        producer.try_push(mk_desc(1, 4, B64, 0, 0)).err(),
        Some(RtStatus::RT_E_QUEUE_OVERRUN)
    );
    assert_eq!(dropped(&pb_fx), 1);
    assert_eq!(consumer.try_pop_checked(1).expect("pop").block_seq, 0);
    let slot = producer.next_slot().expect("pb space after pop");
    assert!(producer.try_push(mk_desc(1, 5, B64, slot, 0)).is_ok());
    let mut seen = Vec::new();
    while let Ok(desc) = consumer.try_pop_checked(1) {
        seen.push(desc.block_seq);
    }
    assert_eq!(seen, [1u64, 2, 3, 5], "未读块完整，仅丢弃新块");
}

// ---------------------------------------------------------------------------
// H. 多线程单写单读（真实 Acquire/Release，desc 完整性压测）
// ---------------------------------------------------------------------------

#[test]
fn t16_single_writer_single_reader_thread_stress() {
    let fixture: &'static Fixture = Box::leak(Box::new(Fixture::new()));
    let producer = make_producer(fixture);
    let consumer = make_consumer(fixture);
    const TOTAL: u64 = 20_000;
    let (sent, full_polls, received) = std::thread::scope(|scope| {
        let producer_task = scope.spawn(move || {
            let mut producer = producer;
            let mut seq = 0u64;
            let mut full_polls = 0u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while seq < TOTAL {
                assert!(
                    std::time::Instant::now() < deadline,
                    "producer 无进展（消费者线程可能已失败）"
                );
                match producer.next_slot() {
                    Some(slot) => {
                        let desc = mk_desc(7, seq, B64, slot, 0);
                        match producer.try_push(desc) {
                            Ok(()) => seq += 1,
                            Err(status) => {
                                panic!("已观察空间，push 必须成功：{:?}", status)
                            }
                        }
                    }
                    None => {
                        // 满：单写者语义下等待消费者释放槽（并发压测不在此断言 overrun，
                        // overrun 语义由单线程确定性测试覆盖）。
                        full_polls += 1;
                        core::hint::spin_loop();
                    }
                }
            }
            (seq, full_polls)
        });
        let consumer_task = scope.spawn(move || {
            let mut consumer = consumer;
            let mut received = 0u64;
            let mut expected_seq = 0u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while received < TOTAL {
                assert!(
                    std::time::Instant::now() < deadline,
                    "consumer 无进展（生产者线程可能已失败）"
                );
                match consumer.try_pop_checked(7) {
                    Ok(desc) => {
                        // 完整性：desc 逐字段一致（Release/Acquire 下不得读到撕裂值）。
                        assert_eq!(desc.epoch_id, 7);
                        assert_eq!(desc.abi_version, ABI_VERSION);
                        assert_eq!(desc.block_size, B64);
                        assert_eq!(desc.block_seq, expected_seq);
                        assert_eq!(desc.frame_index, expected_seq * B64 as u64);
                        assert_eq!(desc.slot_index, (expected_seq % 4) as u32);
                        assert_eq!(desc.mono_publish_ns, 0x1000 + expected_seq);
                        assert_eq!(desc.play_deadline_ns, 0x2000 + expected_seq);
                        assert_eq!(desc.n_in, 2);
                        assert_eq!(desc.n_out, 2);
                        assert_eq!(desc.plan_version, 1);
                        assert_ne!(desc.flags & FLAG_VALID, 0);
                        assert_eq!(desc.valid_frames, B64 as u32);
                        expected_seq += 1;
                        received += 1;
                    }
                    Err(status) => {
                        assert_eq!(status, RtStatus::RT_E_QUEUE_UNDERRUN);
                        core::hint::spin_loop();
                    }
                }
            }
            received
        });
        let sent = producer_task.join().expect("producer thread");
        let received = consumer_task.join().expect("consumer thread");
        (sent.0, sent.1, received)
    });
    assert_eq!(sent, TOTAL);
    assert_eq!(received, TOTAL);
    let _ = full_polls;
    assert_eq!(write_pos(fixture), TOTAL);
    assert_eq!(read_pos(fixture), TOTAL);
    assert_eq!(
        dropped(fixture),
        0,
        "本压测不 push 满环，环内 dropped 不应增长"
    );
}

// ---------------------------------------------------------------------------
// I. 无锁 / 无分配 / RT 类型纪律（模型测试）
// ---------------------------------------------------------------------------

mod alloc_probe {
    //! 线程本地计数分配器（仅测试）：证明 try_push/try_pop 不分配（F-01/R-DROP-03）。
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::Cell;
    use std::alloc::System;

    thread_local! {
        static COUNT: Cell<u64> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNT.with(|count| count.set(count.get().wrapping_add(1)));
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            COUNT.with(|count| count.set(count.get().wrapping_add(1)));
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            COUNT.with(|count| count.set(count.get().wrapping_add(1)));
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            COUNT.with(|count| count.set(count.get().wrapping_add(1)));
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    pub fn begin() {
        COUNT.with(|count| count.set(0));
    }

    pub fn count() -> u64 {
        COUNT.with(|count| count.get())
    }
}

#[test]
fn t16_push_pop_do_not_allocate() {
    let fx = Fixture::new();
    let mut producer = make_producer(&fx);
    let mut consumer = make_consumer(&fx);
    let mut seq = 0u64;
    // 预热 TLS 计数器（首次访问不计入测量窗口）。
    alloc_probe::begin();
    let _ = producer.next_slot();
    let _ = consumer.is_empty();
    alloc_probe::begin();
    let mut round = 0u64;
    while round < 4_000 {
        match producer.next_slot() {
            Some(slot) => {
                let pushed = producer.try_push(mk_desc(1, seq, B64, slot, 0)).is_ok();
                if pushed {
                    seq += 1;
                }
            }
            None => {
                let _ = consumer.try_pop_checked(1);
            }
        }
        round += 1;
    }
    let allocations = alloc_probe::count();
    assert_eq!(allocations, 0, "RT 发布/消费配方不得分配（F-01）");
    assert!(seq > 0);
}

#[test]
fn t16_module_source_has_no_lock_alloc_or_syscall_tokens() {
    // 只扫描可执行代码行（去注释）；静态断言块里的 assert! 属编译期，不在 RT 运行期。
    let source = include_str!("../../src/spsc.rs");
    let mut in_const_assert = false;
    let mut code_lines: Vec<&str> = Vec::new();
    for raw_line in source.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("const _: () = {") {
            in_const_assert = true;
            continue;
        }
        if in_const_assert {
            if trimmed == "};" {
                in_const_assert = false;
            }
            continue;
        }
        let comment_start = raw_line.find("//");
        let code = match comment_start {
            Some(index) => &raw_line[..index],
            None => raw_line,
        };
        if !code.trim().is_empty() {
            code_lines.push(code);
        }
    }
    let forbidden = [
        "Mutex",
        "RwLock",
        "OnceLock",
        "Condvar",
        "parking_lot",
        "Box<",
        "Vec<",
        "String",
        "format!",
        "println!",
        "eprintln!",
        "std::fs",
        "std::net",
        "std::process",
        "std::thread",
        "std::time",
        "libc::",
        "clock_gettime",
        "Instant::now",
        "SystemTime",
        "Arc<",
        "Rc<",
        "HashMap",
        "BTreeMap",
        "mpsc",
        "crossbeam",
        "rayon",
        "unwrap(",
        "expect(",
        "panic!",
        "todo!",
        "unimplemented!",
        "unreachable!",
        "catch_unwind",
        "impl Drop",
        "ManuallyDrop",
        "mem::forget",
        "SeqCst",
        "thread_local",
        "extern ",
        "#[no_mangle]",
    ];
    for token in forbidden {
        for code in &code_lines {
            assert!(
                !code.contains(token),
                "RT 源文件出现禁止符号 {:?}：{}",
                token,
                code.trim()
            );
        }
    }
    assert!(source.contains("RT-MODULE:"), "R-DROP-14 文件头标记缺失");
    assert!(
        source.contains("§10") && source.contains("§13"),
        "R-DROP-14 规则引用缺失"
    );
    assert!(
        source.contains("Acquire") && source.contains("Release"),
        "发布配方必须显式使用 Acquire/Release（P-02）"
    );
}

#[test]
fn t16_rt_visible_types_have_no_drop_and_fixed_layout() {
    assert!(!core::mem::needs_drop::<DescSlot>());
    assert!(!core::mem::needs_drop::<Producer<'_>>());
    assert!(!core::mem::needs_drop::<Consumer<'_>>());
    assert!(!core::mem::needs_drop::<KhaenSpscHeaderV1>());
    assert!(!core::mem::needs_drop::<KhaenBlockDescV1>());
    assert_eq!(core::mem::size_of::<DescSlot>(), 64);
    assert_eq!(core::mem::align_of::<DescSlot>(), 64);
    assert_eq!(core::mem::size_of::<KhaenSpscHeaderV1>(), 64);
    assert_eq!(SPSC_CAPACITY, 4);
    assert_eq!(SPSC_ENTRY_BYTES, 64);
}
