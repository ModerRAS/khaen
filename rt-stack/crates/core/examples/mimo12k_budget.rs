//! MIMO 12k 修正支路 taps 档 CPU/预算实测 harness（taps-budget）。
//!
//! 纯离线 host-only：只在开发机 CPU 上跑 Mimo12kRuntime::process_block_f32，
//! 不碰声卡/设备/板端，不 apply、不出声、不改 src、不改常量。产物标签：
//! PREDICTED_HOST_OFFLINE / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT
//! （板端、xrun、真实端到端延迟均未测）。
//!
//! 运行（release、cargo --offline）：
//!
//! ~~~text
//! cargo run --release --offline -p khaen-core --example mimo12k_budget
//! ~~~
//!
//! 固定口径（与 mimo12k 生产语义一致；只用于成本测量，不评价声学）：
//! - M=5（5x5 G12）；block48=128 → low_block=32；D12=120（10 ms @12k）；g 全零；
//! - 自生成 63-tap 对称 Blackman 窗 sinc 抽取/插值低通（48k->12k）：decim DC 归一 1、
//!   interp = decim x MIMO12K_GROUP（x4，按 mimo12k 语义），decim/interp 群延迟 31；
//! - 每档 taps 先 build layout/runtime，再 warmup 1000 块 + Instant 计时 20000 个
//!   完整 block，报 ns/block、realtime factor 与单核 CPU%（= realtime_factor x 100）；
//! - 8192/16384 预期被 MIMO12K_MAX_TAPS=4096 以 ERR_TAPS 拦截：本 harness 不绕过、
//!   不改常量，只按与 PartitionedLayout::build_with_fused_taps 相同的公式给
//!   Q/plan/state 字节与从实测点出发的 PREDICTED 线性外推。
//!
//! 输出：按 [SECTION key=value ...] 行打印，末尾 [CSV_HEADER] / [CSV_ROW] 行
//! 可直接抽取成 evidence/.../taps-budget/taps_budget.csv。

use std::time::Instant;

use khaen_core::mimo12k::{
    Mimo12kLayout, Mimo12kRuntime, MIMO12K_GROUP, MIMO12K_LOWRATE_HZ, MIMO12K_MAX_PARTICIPANTS,
    MIMO12K_MAX_TAPS,
};
use khaen_core::rt::{RtStatus, MAX_PARTITIONS, TAPS_EXTENDED_MAX};

/// participants 数（生产 MIMO 低频 5x5；g 全零只测成本）。
const M: usize = 5;
/// 48 kHz 块长（生产 B128；low_block = 32 @12k）。
const BLOCK48: usize = 128;
/// taps 档（12k 域 G 支撑）：2048/4096 可 build，8192/16384 预期 ERR_TAPS。
const TAPS_GRID: [usize; 4] = [2_048, 4_096, 8_192, 16_384];
/// 自生成抽取/插值低通抽头（奇数 63，群延迟 31 @48k）。
const RESAMP_TAPS: usize = 63;
/// M2 预延迟 D12（12k 样本 = 10 ms）。
const D12: usize = 120;
/// 计时 block 数（每档）。
const BLOCKS: usize = 20_000;
/// 预热 block 数（不计时）。
const WARMUP_BLOCKS: usize = 1_000;
const FS48: f64 = 48_000.0;
const FS12: f64 = MIMO12K_LOWRATE_HZ as f64;
/// 128 帧 @48k 的块 deadline（ns）。
const BLOCK_DEADLINE_NS: f64 = BLOCK48 as f64 / FS48 * 1e9;

// 结构自洽的编译期断言（不参与运行期）。
const _: () = assert!(M > 0 && M <= MIMO12K_MAX_PARTICIPANTS);
const _: () = assert!(RESAMP_TAPS % 2 == 1 && RESAMP_TAPS <= 255);
const _: () = assert!(BLOCK48 % MIMO12K_GROUP == 0);

fn main() {
    print_header();
    let (decim, interp) = build_resamplers();

    // ---- 1) build layout / runtime（探测常量拦截，不绕过） ------------------
    println!("\n== 1) build layout/runtime 尝试（不绕过常量、不改 src） ==");
    let mut built: Vec<Mimo12kLayout> = Vec::new();
    for taps in TAPS_GRID {
        match Mimo12kLayout::build(M, taps, BLOCK48, RESAMP_TAPS, D12) {
            Ok(layout) => {
                println!(
                    "[BUILD_ATTEMPT taps={taps}] result=OK code=0 name=OK g_coeff_bytes={}",
                    g_coeff_bytes(taps)
                );
                print_layout(&layout);
                print_budget(&layout);
                built.push(layout);
            }
            Err(status) => {
                println!(
                    "[BUILD_ATTEMPT taps={taps}] result=ERR code={} name={} note=Mimo12kLayout::build 按 MIMO12K_MAX_TAPS={MIMO12K_MAX_TAPS} fail-closed（本 harness 不绕过/不改常量）",
                    status.code(),
                    status_name(status)
                );
            }
        }
    }

    // ---- 2) 容量/字节公式（与 partitioned layout 同公式） ------------------
    println!(
        "\n== 2) 容量/Q/plan/state 公式（PartitionedLayout::build_with_fused_taps 同公式） =="
    );
    for taps in TAPS_GRID {
        let formula = conv_formula(taps);
        let verdict = capacity_verdict(taps, &formula);
        let delay48 = delay48_samples();
        println!(
            "[CAPACITY taps={taps}] rate_hz=12000 support_ms={:.3} low_block={} fft_size={} partitions={} max_partitions={} q_within_limit={} plan_samples={} plan_bytes={} state_samples={} state_bytes={} g_coeff_bytes={} delay48_samples={} delay_ms={:.3} q_vs_capacity_note={} verdict={verdict}",
            support_ms(taps),
            formula.low_block,
            formula.fft_size,
            formula.partitions,
            MAX_PARTITIONS,
            yes_no(formula.partitions <= MAX_PARTITIONS as usize),
            formula.plan_samples,
            formula.plan_samples * 4,
            formula.state_samples,
            formula.state_samples * 4,
            g_coeff_bytes(taps),
            delay48,
            delay_ms(delay48),
            q_note(&formula),
        );
        // 对能 build 的档位做公式 vs layout 一致性核对。
        if let Some(layout) = built.iter().find(|l| l.taps as usize == taps) {
            let plan_match = layout.conv_plan_samples == formula.plan_samples as u32;
            let state_match = layout.conv_state_samples == formula.state_samples as u32;
            println!(
                "[FORMULA_CHECK taps={taps}] plan_samples_formula={} plan_samples_layout={} state_samples_formula={} state_samples_layout={} match={}",
                formula.plan_samples,
                layout.conv_plan_samples,
                formula.state_samples,
                layout.conv_state_samples,
                yes_no(plan_match && state_match)
            );
        }
    }

    // ---- 3) 2048/4096 实测（20000 blocks/档） ------------------------------
    println!("\n== 3) 本机实测：20000 个完整 128 帧 block / 档（release；host 非板端） ==");
    let mut measurements: Vec<Measurement> = Vec::new();
    for (index, layout) in built.iter().enumerate() {
        if let Some(measurement) = measure(*layout, &decim, &interp, index == 0) {
            measurements.push(measurement);
        }
    }

    // ---- 4) 8192/16384：PREDICTED 线性外推（从实测点拟合 + 简单 x2/x4） ----
    println!("\n== 4) PREDICTED 线性外推（conv 主导；resampler 固定 → 外推略保守） ==");
    let predictions = extrapolate(&measurements);

    // ---- 5) 48k extended 16384：仅理论/规格对照，NOT_MEASURED --------------
    println!("\n== 5) 48k extended 16384 taps（仅理论/规格对照） ==");
    print_fullrate_48k_reference();

    // ---- 6) CSV（可直接落 taps_budget.csv） --------------------------------
    println!("\n== 6) CSV（全部 host 离线；板端/xrun/端到端延迟 NO_MEASUREMENT） ==");
    println!("[CSV_HEADER] {CSV_HEADER}");
    for taps in TAPS_GRID {
        let measurement = measurements.iter().find(|m| m.taps == taps);
        let prediction = predictions.iter().find(|p| p.taps == taps);
        println!(
            "[CSV_ROW] {}",
            csv_row(
                taps,
                measurement,
                prediction.map(|p| p.simple_ns_per_block),
                prediction.map(|p| p.fit_ns_per_block)
            )
        );
    }
    println!("\n[TAGS] PREDICTED_HOST_OFFLINE SINGLE_POINT_NOT_SPATIAL NO_MEASUREMENT(board,xrun,end_to_end_latency)");
}

// ---------------------------------------------------------------------------
// 环境/头部
// ---------------------------------------------------------------------------

fn print_header() {
    println!(
        "=== TSPI MIMO 12k 修正支路 taps 档 CPU/预算 harness（taps-budget，离线 host-only）==="
    );
    println!(
        "[SCOPE] PREDICTED_HOST_OFFLINE SINGLE_POINT_NOT_SPATIAL NO_MEASUREMENT(board,xrun,end_to_end_latency)"
    );
    println!(
        "[ENV] os={} arch={} profile={} debug_assertions={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        cfg!(debug_assertions)
    );
    for key in [
        "PROCESSOR_IDENTIFIER",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ] {
        if let Ok(value) = std::env::var(key) {
            println!("[ENV] {key}={value}");
        }
    }
    println!(
        "[LIMITS] mimo12k_max_taps={MIMO12K_MAX_TAPS} taps_extended_max={TAPS_EXTENDED_MAX} max_partitions={MAX_PARTITIONS} participants_max={MIMO12K_MAX_PARTICIPANTS}"
    );
    println!(
        "[PARAMS] m={M} block48={BLOCK48} low_block={} resamp_taps={RESAMP_TAPS} d12={D12} blocks={BLOCKS} warmup_blocks={WARMUP_BLOCKS}",
        BLOCK48 / MIMO12K_GROUP
    );
    println!("[PARAMS] g=all_zero（只测运行时成本，不评价声学）");
}

// ---------------------------------------------------------------------------
// 抽取/插值低通：63-tap 对称 Blackman 窗 sinc（48k->12k）
// ---------------------------------------------------------------------------

/// 生成 (decim, interp)：同一 63-tap 对称低通，decim 归一 DC=1，interp=decim x GROUP。
fn build_resamplers() -> (Vec<f32>, Vec<f32>) {
    let n = RESAMP_TAPS;
    let mid = (n - 1) as f64 / 2.0;
    let cutoff = 0.125_f64; // cycles/sample @48k = 6 kHz（12k Nyquist）
    let mut raw = vec![0.0f64; n];
    let mut sum = 0.0f64;
    for (k, slot) in raw.iter_mut().enumerate() {
        let t = k as f64 - mid;
        let x = 2.0 * std::f64::consts::PI * cutoff * t;
        let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
        let angle = 2.0 * std::f64::consts::PI * k as f64 / (n - 1) as f64;
        let window = 0.42 - 0.5 * angle.cos() + 0.08 * (2.0 * angle).cos();
        let value = 2.0 * cutoff * sinc * window;
        *slot = value;
        sum += value;
    }
    let decim: Vec<f32> = raw.iter().map(|v| (v / sum) as f32).collect();
    let interp: Vec<f32> = decim.iter().map(|v| v * MIMO12K_GROUP as f32).collect();
    (decim, interp)
}

// ---------------------------------------------------------------------------
// 布局/预算打印
// ---------------------------------------------------------------------------

fn print_layout(layout: &Mimo12kLayout) {
    println!(
        "[LAYOUT taps={}] m={} block48={} low_block={} resamp_taps={} decim_delay={} interp_delay={} predelay_low={} delay48_samples={} delay_ms={:.3} g_span_ms={:.3} correction_span_ms={:.3} fft_size={} partitions={} routes={} plan_samples={} plan_bytes={} state_samples={} state_bytes={}",
        layout.taps,
        layout.m,
        layout.block48,
        layout.low_block,
        layout.resamp_taps,
        layout.decim_delay,
        layout.interp_delay,
        layout.predelay_low,
        layout.delay48,
        layout.delay_ms(),
        layout.g_span_ms(),
        layout.correction_span_ms(),
        layout.fft_size,
        layout.partitions,
        layout.n_routes,
        layout.conv_plan_samples,
        layout.conv_plan_samples * 4,
        layout.conv_state_samples,
        layout.conv_state_samples * 4,
    );
}

fn print_budget(layout: &Mimo12kLayout) {
    let budget = layout.budget();
    println!(
        "[BUDGET taps={}] conv_complex_macs_per_low_block={} conv_flops_per_s={:.3e} decim_macs_per_s={:.3e} interp_macs_per_s={:.3e} total_macs_per_s={:.3e} fullrate_48k_macs_per_s={:.3e} lowrate_vs_fullrate_ratio={:.3}",
        layout.taps,
        budget.conv_complex_macs_per_low_block,
        budget.conv_flops_per_sec,
        budget.decim_macs_per_sec,
        budget.interp_macs_per_sec,
        budget.total_macs_per_sec,
        budget.fullrate_48k_macs_per_sec,
        budget.lowrate_vs_fullrate_ratio,
    );
}

// ---------------------------------------------------------------------------
// 实测
// ---------------------------------------------------------------------------

struct Measurement {
    taps: usize,
    plan_samples: u32,
    state_samples: u32,
    alloc_samples: usize,
    total_ns: u128,
    ns_per_block: f64,
    blocks_per_sec: f64,
    realtime_factor: f64,
    cpu_percent: f64,
    min_ns: u64,
    p50_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    checksum: f32,
}

fn measure(
    layout: Mimo12kLayout,
    decim: &[f32],
    interp: &[f32],
    emit_resampler: bool,
) -> Option<Measurement> {
    let taps = layout.taps as usize;
    let g = vec![0.0f32; M * M * taps];
    let mut runtime = match Mimo12kRuntime::build(layout, &g, decim, interp) {
        Ok(runtime) => runtime,
        Err(status) => {
            println!(
                "[MEASURE taps={taps}] build_runtime=ERR code={} name={}",
                status.code(),
                status_name(status)
            );
            return None;
        }
    };
    if emit_resampler {
        print_resampler(&runtime);
    }

    let feeds = make_feeds();
    let mut corr = vec![0.0f32; M * BLOCK48];
    for _ in 0..WARMUP_BLOCKS {
        let status = runtime.process_block_f32(&feeds, BLOCK48, &mut corr, BLOCK48, BLOCK48);
        if !status.is_ok() {
            println!(
                "[MEASURE taps={taps}] warmup=ERR code={} name={}",
                status.code(),
                status_name(status)
            );
            return None;
        }
    }

    let mut per_block_ns: Vec<u64> = Vec::with_capacity(BLOCKS);
    let mut checksum = 0.0f32;
    let mut bad_status = None;
    let overall = Instant::now();
    for _ in 0..BLOCKS {
        let started = Instant::now();
        let status = runtime.process_block_f32(&feeds, BLOCK48, &mut corr, BLOCK48, BLOCK48);
        per_block_ns.push(started.elapsed().as_nanos() as u64);
        if !status.is_ok() && bad_status.is_none() {
            bad_status = Some(status);
        }
        checksum += corr[0];
    }
    let total_ns = overall.elapsed().as_nanos();
    if let Some(status) = bad_status {
        println!(
            "[MEASURE taps={taps}] timed_loop=ERR code={} name={}",
            status.code(),
            status_name(status)
        );
        return None;
    }

    per_block_ns.sort_unstable();
    let len = per_block_ns.len();
    let ns_per_block = total_ns as f64 / BLOCKS as f64;
    let realtime_factor = ns_per_block / BLOCK_DEADLINE_NS;
    let measurement = Measurement {
        taps,
        plan_samples: layout.conv_plan_samples,
        state_samples: layout.conv_state_samples,
        alloc_samples: runtime.allocated_samples(),
        total_ns,
        ns_per_block,
        blocks_per_sec: 1e9 / ns_per_block,
        realtime_factor,
        cpu_percent: realtime_factor * 100.0,
        min_ns: per_block_ns[0],
        p50_ns: per_block_ns[len / 2],
        p99_ns: per_block_ns[(len * 99 / 100).min(len - 1)],
        max_ns: per_block_ns[len - 1],
        checksum,
    };
    println!(
        "[MEASURED_HOST_OFFLINE taps={taps}] blocks={BLOCKS} total_ms={:.3} ns_per_block={:.1} blocks_per_sec={:.1} realtime_factor={:.4} cpu_percent_single_core={:.2} min_ns={} p50_ns={} p99_ns={} max_ns={} checksum={:.6e}",
        measurement.total_ns as f64 / 1e6,
        measurement.ns_per_block,
        measurement.blocks_per_sec,
        measurement.realtime_factor,
        measurement.cpu_percent,
        measurement.min_ns,
        measurement.p50_ns,
        measurement.p99_ns,
        measurement.max_ns,
        measurement.checksum,
    );
    println!(
        "[ALLOC taps={taps}] plan_samples={} state_samples={} alloc_samples={} alloc_bytes={} (f32；plan+state+resampler coeff/hist/scratch)",
        measurement.plan_samples,
        measurement.state_samples,
        measurement.alloc_samples,
        measurement.alloc_samples * 4
    );
    Some(measurement)
}

fn print_resampler(runtime: &Mimo12kRuntime) {
    let decim_dc = runtime.resampler_gain_db(0.0, false);
    let combined_dc = runtime.resampler_gain_db(0.0, true);
    let combined_100hz = runtime.resampler_gain_db(100.0, true);
    println!(
        "[RESAMPLER] taps={RESAMP_TAPS} group={} window=blackman decim_dc_db={:.4} combined_dc_db={:.4} combined_100hz_db={:.4} decim_6000hz_db={:.2} decim_8000hz_db={:.2} decim_11700hz_db={:.2} decim_23500hz_db={:.2} note=63-tap 仅用于驱动运行时成本，非声学门（衰减指标不构成本证据结论）",
        MIMO12K_GROUP,
        decim_dc,
        combined_dc,
        combined_100hz,
        runtime.resampler_gain_db(6_000.0, false),
        runtime.resampler_gain_db(8_000.0, false),
        runtime.resampler_gain_db(11_700.0, false),
        runtime.resampler_gain_db(23_500.0, false),
    );
}

fn make_feeds() -> Vec<f32> {
    let mut feeds = vec![0.0f32; M * BLOCK48];
    for channel in 0..M {
        for n in 0..BLOCK48 {
            let t = n as f64 / FS48;
            let low =
                0.1 * (2.0 * std::f64::consts::PI * (100.0 + 25.0 * channel as f64) * t).sin();
            let high =
                0.05 * (2.0 * std::f64::consts::PI * (1_000.0 + 37.0 * channel as f64) * t).sin();
            feeds[channel * BLOCK48 + n] = (low + high) as f32;
        }
    }
    feeds
}

// ---------------------------------------------------------------------------
// 公式/容量判定（与 PartitionedLayout::build_with_fused_taps 同公式）
// ---------------------------------------------------------------------------

struct ConvFormula {
    low_block: usize,
    fft_size: usize,
    partitions: usize,
    plan_samples: usize,
    state_samples: usize,
}

fn conv_formula(taps: usize) -> ConvFormula {
    let low_block = BLOCK48 / MIMO12K_GROUP;
    let mut fft_size = 1usize;
    while fft_size < 2 * low_block {
        fft_size <<= 1;
    }
    let partitions = (taps + low_block - 1) / low_block;
    let n_src = M as u64;
    let n_routes = (M * M) as u64;
    let b = low_block as u64;
    let k = fft_size as u64;
    let q = partitions as u64;
    let state_samples = n_src * b + n_src * q * k * 2 + 4 * k + 1;
    let plan_samples = k + n_routes * q * k * 2;
    ConvFormula {
        low_block,
        fft_size,
        partitions,
        plan_samples: plan_samples as usize,
        state_samples: state_samples as usize,
    }
}

fn capacity_verdict(taps: usize, formula: &ConvFormula) -> &'static str {
    let buildable = taps <= MIMO12K_MAX_TAPS;
    if buildable {
        "FEASIBLE_MEASURED_HOST"
    } else if formula.partitions <= MAX_PARTITIONS as usize {
        "CAPACITY_FITS_NEEDS_SPEC_EXTENSION"
    } else {
        "NOT_FEASIBLE_CURRENT_ENGINE"
    }
}

fn q_note(formula: &ConvFormula) -> String {
    if formula.partitions < MAX_PARTITIONS as usize {
        format!(
            "Q={} < MAX_PARTITIONS={}（容量内）",
            formula.partitions, MAX_PARTITIONS
        )
    } else if formula.partitions == MAX_PARTITIONS as usize {
        format!(
            "Q={} == MAX_PARTITIONS={}（恰好上界）",
            formula.partitions, MAX_PARTITIONS
        )
    } else {
        format!(
            "Q={} > MAX_PARTITIONS={}（超容量）",
            formula.partitions, MAX_PARTITIONS
        )
    }
}

fn delay48_samples() -> u32 {
    (RESAMP_TAPS - 1) as u32 + MIMO12K_GROUP as u32 * D12 as u32
}

fn delay_ms(delay48: u32) -> f64 {
    delay48 as f64 * 1000.0 / FS48
}

fn support_ms(taps: usize) -> f64 {
    taps as f64 * 1000.0 / FS12
}

fn g_coeff_bytes(taps: usize) -> usize {
    M * M * taps * 4
}

// ---------------------------------------------------------------------------
// PREDICTED 线性外推
// ---------------------------------------------------------------------------

struct Prediction {
    taps: usize,
    simple_ns_per_block: f64,
    fit_ns_per_block: f64,
}

fn extrapolate(measurements: &[Measurement]) -> Vec<Prediction> {
    let low = measurements.iter().find(|m| m.taps == 2_048);
    let high = measurements.iter().find(|m| m.taps == 4_096);
    let (Some(low), Some(high)) = (low, high) else {
        println!("[PREDICTED_LINEAR] error=missing_measurements");
        return Vec::new();
    };
    let low_formula = conv_formula(low.taps);
    let high_formula = conv_formula(high.taps);
    let dq = (high_formula.partitions - low_formula.partitions) as f64;
    let slope = (high.ns_per_block - low.ns_per_block) / dq;
    let intercept = low.ns_per_block - slope * low_formula.partitions as f64;
    println!(
        "[PREDICTED_LINEAR_FIT] q_low={} ns_low={:.1} q_high={} ns_high={:.1} slope_ns_per_partition={:.2} intercept_ns={:.1} note=两实测点线性拟合（conv 按 Q 线性；resampler 固定项在 intercept）",
        low_formula.partitions,
        low.ns_per_block,
        high_formula.partitions,
        high.ns_per_block,
        slope,
        intercept,
    );
    let mut out = Vec::new();
    for taps in [8_192usize, 16_384] {
        let formula = conv_formula(taps);
        let factor = formula.partitions as f64 / high_formula.partitions as f64;
        let simple_ns_per_block = high.ns_per_block * factor;
        let fit_ns_per_block = intercept + slope * formula.partitions as f64;
        println!(
            "[PREDICTED_LINEAR taps={taps}] q={} factor_vs_4096={:.3} simple_ns_per_block={:.1} simple_realtime_factor={:.4} simple_cpu_percent={:.2} fit_ns_per_block={:.1} fit_realtime_factor={:.4} fit_cpu_percent={:.2} verdict={} tag=PREDICTED_LINEAR_EXTRAPOLATION",
            formula.partitions,
            factor,
            simple_ns_per_block,
            simple_ns_per_block / BLOCK_DEADLINE_NS,
            simple_ns_per_block / BLOCK_DEADLINE_NS * 100.0,
            fit_ns_per_block,
            fit_ns_per_block / BLOCK_DEADLINE_NS,
            fit_ns_per_block / BLOCK_DEADLINE_NS * 100.0,
            capacity_verdict(taps, &formula),
        );
        out.push(Prediction {
            taps,
            simple_ns_per_block,
            fit_ns_per_block,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 48k extended 16384：仅理论/规格对照（NOT_MEASURED）
// ---------------------------------------------------------------------------

fn print_fullrate_48k_reference() {
    let taps = 16_384usize;
    let block = 128usize;
    let fft_size = 2 * block; // 256
    let partitions = (taps + block - 1) / block; // 128
    let n_routes = (M * M) as u64;
    let x_hist = M as u64 * partitions as u64 * fft_size as u64 * 2;
    let state_samples = M as u64 * block as u64 + x_hist + 4 * fft_size as u64 + 1;
    let plan_samples = fft_size as u64 + n_routes * partitions as u64 * fft_size as u64 * 2;
    let macs_per_sec = n_routes as f64 * taps as f64 * FS48;
    let macs_12k_4096 = (M * M) as f64 * 4_096.0 * FS12;
    println!(
        "[48K_EXTENDED_THEORY taps={taps}] rate_hz=48000 support_ms={:.3} block48={block} fft_size={fft_size} partitions={partitions} max_partitions={MAX_PARTITIONS} q_within_limit={} plan_samples={} plan_bytes={} state_samples={} state_bytes={} conv_macs_per_s={:.3e} ratio_vs_12k_4096={:.1} tag=NOT_MEASURED_THEORY note=48k 全速 extended 仅规格对照（16384@48k = 341.3 ms 与 4096@12k 同物理支撑）；mimo12k 无 48k 全速支路、direct 引擎 4096 上限拒绝，本 harness 未跑",
        support_ms_48k(taps),
        yes_no(partitions <= MAX_PARTITIONS as usize),
        plan_samples,
        plan_samples * 4,
        state_samples,
        state_samples * 4,
        macs_per_sec,
        macs_per_sec / macs_12k_4096,
    );
}

fn support_ms_48k(taps: usize) -> f64 {
    taps as f64 * 1000.0 / FS48
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

const CSV_HEADER: &str = "taps,rate_hz,support_ms,block48,low_block,fft_size,partitions,max_partitions,routes,plan_samples,plan_bytes,state_samples,state_bytes,alloc_samples,alloc_bytes,g_coeff_bytes,delay48_samples,delay_ms,build_status,capacity_verdict,ns_per_block_measured,realtime_factor_measured,cpu_percent_measured,ns_per_block_predicted_simple,ns_per_block_predicted_fit,cpu_percent_predicted_fit,evidence_tag";

fn csv_row(
    taps: usize,
    measurement: Option<&Measurement>,
    simple_ns: Option<f64>,
    fit_ns: Option<f64>,
) -> String {
    let formula = conv_formula(taps);
    let build_status = if taps <= MIMO12K_MAX_TAPS {
        "OK"
    } else {
        "ERR_TAPS(110)"
    };
    let verdict = capacity_verdict(taps, &formula);
    let alloc_samples = measurement
        .map(|m| m.alloc_samples.to_string())
        .unwrap_or_default();
    let alloc_bytes = measurement
        .map(|m| (m.alloc_samples * 4).to_string())
        .unwrap_or_default();
    let measured_ns = measurement
        .map(|m| format!("{:.1}", m.ns_per_block))
        .unwrap_or_default();
    let measured_rf = measurement
        .map(|m| format!("{:.4}", m.realtime_factor))
        .unwrap_or_default();
    let measured_cpu = measurement
        .map(|m| format!("{:.2}", m.cpu_percent))
        .unwrap_or_default();
    let simple = simple_ns.map(|v| format!("{v:.1}")).unwrap_or_default();
    let fit = fit_ns.map(|v| format!("{v:.1}")).unwrap_or_default();
    let fit_cpu = fit_ns
        .map(|v| format!("{:.2}", v / BLOCK_DEADLINE_NS * 100.0))
        .unwrap_or_default();
    let tag = if measurement.is_some() {
        "PREDICTED_HOST_OFFLINE"
    } else {
        "PREDICTED_LINEAR_EXTRAPOLATION_OFFLINE"
    };
    let delay = delay48_samples();
    format!(
        "{taps},12000,{:.3},{BLOCK48},{},{},{},{MAX_PARTITIONS},{},{},{},{},{},{alloc_samples},{alloc_bytes},{},{delay},{:.3},{build_status},{verdict},{measured_ns},{measured_rf},{measured_cpu},{simple},{fit},{fit_cpu},{tag}",
        support_ms(taps),
        formula.low_block,
        formula.fft_size,
        formula.partitions,
        M * M,
        formula.plan_samples,
        formula.plan_samples * 4,
        formula.state_samples,
        formula.state_samples * 4,
        g_coeff_bytes(taps),
        delay_ms(delay),
    )
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn yes_no(condition: bool) -> &'static str {
    if condition {
        "1"
    } else {
        "0"
    }
}

fn status_name(status: RtStatus) -> &'static str {
    if status == RtStatus::ERR_TAPS {
        "ERR_TAPS"
    } else if status == RtStatus::ERR_BLOCK_SIZE {
        "ERR_BLOCK_SIZE"
    } else if status == RtStatus::ERR_CHANNELS {
        "ERR_CHANNELS"
    } else if status == RtStatus::ERR_CAPACITY {
        "ERR_CAPACITY"
    } else if status == RtStatus::ERR_PATH_TABLE {
        "ERR_PATH_TABLE"
    } else if status == RtStatus::ERR_STRIDE_ALIGN {
        "ERR_STRIDE_ALIGN"
    } else if status.is_ok() {
        "OK"
    } else {
        "OTHER"
    }
}
