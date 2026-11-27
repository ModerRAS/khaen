//! 可切换执行器的实时路径零分配门禁（独立测试二进制 + 计数全局分配器）。
//!
//! 本文件只放一个测试：本二进制内没有别的测试并发跑，因此全局分配计数是干净的。
//! 计量窗口内不得有任何堆分配——输出缓冲在构造时分配，图在 prepare/compile（控制线程）分配，
//! 块边界换图只做移动。窗口覆盖两种稳态：固定处理链，以及一次 `stage_processor` 之后
//! （含"下一块入口应用 pending"的那一块）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use khaen_host::{EngineConfig, Processor, SlotEngine};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// 记录处理链：把输入乘增益，不做任何分配。
struct Gain(f32);

impl Processor for Gain {
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        for (out, sample) in output.iter_mut().zip(input.iter()) {
            *out = *sample * self.0;
        }
    }
}

fn allocated() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn slot_engine_audio_path_performs_no_heap_allocation() {
    let config = EngineConfig {
        block_frames: 128,
        sample_rate: 96_000,
        input_channels: 2,
        output_channels: 2,
    };
    let mut engine = SlotEngine::new(config, Box::new(Gain(0.5))).expect("engine");
    let input: Vec<f32> = (0..engine.output_samples_per_block())
        .map(|index| (index as f32) * 1.0e-3 - 0.25)
        .collect();

    // 预热：让首次触碰的懒惰初始化（TLS 等）发生在计量窗口之外。
    let _ = engine.run_block(&input).expect("warmup");

    let before = allocated();
    for _ in 0..300 {
        let _ = engine.run_block(&input).expect("run");
    }
    let delta = allocated() - before;
    assert_eq!(delta, 0, "SlotEngine audio path allocated {delta} times");
    assert_eq!(engine.stats().blocks, 301);

    // 换图后的稳态：stage_processor 在控制线程（Box/String 的分配在这里之前完成），
    // run_block 入口应用 pending 并继续处理不得分配。
    let processor: Box<dyn Processor + Send> = Box::new(Gain(2.0));
    let graph_id = String::from("0123456789abcdef0123456789abcdef");
    let staged_block = engine.stage_processor(processor, 1, graph_id);
    assert_eq!(staged_block, 302, "stage 返回下一块块号");

    let before_switch = allocated();
    for _ in 0..300 {
        let _ = engine.run_block(&input).expect("run after switch");
    }
    let switch_delta = allocated() - before_switch;
    assert_eq!(
        switch_delta, 0,
        "block-boundary switch path allocated {switch_delta} times"
    );
    assert_eq!(engine.graph_info().epoch, Some(1));
    assert_eq!(engine.graph_info().applied_block, Some(302));
    assert!(engine.stats().blocks > 600);
}
