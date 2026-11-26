//! RT 零分配门禁（T41 交付 3e）：ASRC process/update_fill/status 路径不分配堆内存。
//!
//! 沿用 host 测试的线程局部计数分配器模式：只统计当前测试线程的分配，
//! 其它测试线程/运行时的分配不会污染计量窗口。
#![allow(clippy::missing_const_for_thread_local)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use khaen_asrc::{Asrc, AsrcConfig, Interpolation};

/// 统计分配次数的全局分配器：只累加当前线程的分配计数（无锁、不分配）。
struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// 当前测试线程累计分配次数。
fn allocation_count() -> usize {
    ALLOCATIONS.with(Cell::get)
}

/// 清零当前测试线程的分配计数。
fn reset_allocation_count() {
    ALLOCATIONS.with(|count| count.set(0));
}

/// 实时路径：填 planar 输入 → process → update_fill → status（全部应零分配）。
fn drive_rt_path(asrc: &mut Asrc, blocks: usize, fill_frames: usize) {
    for block in 0..blocks {
        let need = asrc.input_frames_next();
        {
            let input = asrc.input_mut();
            for channel in 0..input.channels() {
                for frame in 0..need {
                    // 简单确定性信号，避免测试自身分配。
                    input.channel_mut(channel)[frame] = 0.01 * (block as f32 + 1.0);
                }
            }
        }
        let outcome = asrc.process().expect("process");
        assert_eq!(outcome.output_frames, asrc.output_frames());
        let _ = asrc.update_fill(fill_frames).expect("update fill");
        let status = asrc.status(fill_frames);
        let _ = (status.enabled, status.ratio_ppm, status.ppm_estimate);
    }
}

#[test]
fn process_path_is_allocation_free() {
    for interp in [Interpolation::Poly, Interpolation::Sinc] {
        let mut config = AsrcConfig::new(2, 128, 48_000);
        config.interp = interp;
        let mut asrc = Asrc::new(config).expect("construct asrc");
        // 预热：让 rubato 内部状态/比率进入稳态（分配只允许发生在构造期）。
        drive_rt_path(&mut asrc, 64, 256);
        reset_allocation_count();
        drive_rt_path(&mut asrc, 240, 256);
        assert_eq!(
            allocation_count(),
            0,
            "{interp:?}: RT 路径发生堆分配（process/update_fill/status 必须零分配）"
        );
    }
}

#[test]
fn controller_update_is_allocation_free() {
    let mut config = AsrcConfig::new(2, 128, 48_000);
    config.interp = Interpolation::Poly;
    let mut controller = khaen_asrc::DriftController::new(&config).expect("controller");
    for _ in 0..64 {
        let _ = controller.update(256);
    }
    reset_allocation_count();
    let mut ppm = 0.0;
    for index in 0..10_000 {
        ppm += controller.update(index % 512);
    }
    assert_eq!(allocation_count(), 0, "控制器 update 发生堆分配");
    assert!(ppm.is_finite());
}
