//! 实时路径零分配门禁。
//!
//! 本文件只放一个测试：本二进制内没有别的测试并发跑，因此全局分配计数是干净的。
//! 计量窗口内不得有任何堆分配——引擎的缓冲必须全部在构造时分配完毕。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use khaen_host::{channel, AudioBackend, BlockEngine, EngineConfig, NullBackend, Processor};

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

#[test]
fn audio_path_performs_no_heap_allocation() {
    let config = EngineConfig {
        block_frames: 128,
        sample_rate: 96_000,
        input_channels: 2,
        output_channels: 2,
    };
    let mut engine = BlockEngine::new(config.clone(), Gain(0.5)).expect("engine");
    let (producer, consumer) = channel::<u32>(256);
    let mut backend = NullBackend::new();
    let device = engine.device_config(khaen_host::DeviceId::new("null"));
    backend.start(&device).expect("start");

    let mut input = vec![0.0f32; engine.output_samples_per_block()];
    let mut scratch = vec![0.0f32; engine.output_samples_per_block()];

    // 预热：让首次触碰的懒惰初始化（TLS 等）发生在计量窗口之外。
    let _ = engine.run_block(&input).expect("warmup");
    let _ = producer.push(0);
    let _ = consumer.pop();
    let _ = engine
        .pump(&mut backend, &mut scratch)
        .expect("warmup pump");

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for index in 0..200u32 {
        input.fill(index as f32 * 1.0e-4);
        let _ = engine.run_block(&input).expect("run");
        let _ = engine.request_mute(index % 7 == 0);
        let _ = producer.push(index);
        let _ = consumer.pop();
        let _ = engine.pump(&mut backend, &mut scratch).expect("pump");
    }
    let delta = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(delta, 0, "audio path allocated {delta} times");
    assert!(engine.stats().blocks > 200);
}
