// T19（clock）离线模型测试：三域映射 / ppm+uncertainty / 唯一 servo / 合成漂移 / 失锁新 epoch。
//
// 本文件经 src/clock.rs 末尾的 cfg(test) mod clock_tests { include!(...) } 进入单测构建
// （cargo 不自动发现 tests/<子目录>；公共注册属 integrator）。运行：
//   cargo test -p khaen-rt-host clock
// 全部测试名 t19_ 前缀；模块路径 clock::clock_tests 含 clock，可按 clock 过滤。
// 离线：不打开任何声卡/PTP/AoIP 设备；只用合成时间戳模型。

use super::*;
use khaen_core::rt::RtStatus;
use khaen_core::workers::KhaenRtControlV1;

const FS: u32 = 48_000;
const M0: u64 = 1_000_000_000;
const P0: u64 = 5_000_000_000;
const FRAME_STEP: u64 = 4_096;

fn t19_mono_for_frame(fs: u32, m0: u64, frame_ppm_ppb: i64, frame: u64) -> u64 {
    let numerator = (NS_PER_SEC as i128 - frame_ppm_ppb as i128) as u128;
    let delta = (frame as u128) * numerator / fs as u128;
    m0 + delta as u64
}

fn t19_ptp_for_mono(m0: u64, p0: u64, mono_ns: u64, ptp_ppm_ppb: i64) -> u64 {
    let numerator = (NS_PER_SEC as i128 + ptp_ppm_ppb as i128) as u128;
    let delta = (mono_ns - m0) as u128 * numerator / NS_PER_SEC as u128;
    p0 + delta as u64
}

fn t19_obs(
    fs: u32,
    m0: u64,
    p0: u64,
    frame_ppm_ppb: i64,
    ptp_ppm_ppb: i64,
    index: u64,
) -> ClockObservation {
    let frame = index.saturating_mul(FRAME_STEP);
    let mono = t19_mono_for_frame(fs, m0, frame_ppm_ppb, frame);
    let ptp = t19_ptp_for_mono(m0, p0, mono, ptp_ppm_ppb);
    ClockObservation::three_domain(frame, mono, ptp)
}

fn t19_obs_with_ptp_offset(
    fs: u32,
    m0: u64,
    p0: u64,
    frame_ppm_ppb: i64,
    ptp_ppm_ppb: i64,
    index: u64,
    ptp_offset_ns: i64,
) -> ClockObservation {
    let mut obs = t19_obs(fs, m0, p0, frame_ppm_ppb, ptp_ppm_ppb, index);
    obs.ptp_ns = (obs.ptp_ns as i128 + ptp_offset_ns as i128) as u64;
    obs
}

fn t19_clean_config() -> ServoConfig {
    ServoConfig {
        lock_samples: 4,
        unlock_samples: 3,
        lock_uncertainty_ppb: 5_000,
        lock_ppm_limit_ppb: 100_000,
        sample_timeout_ns: 10_000_000,
        holdover_ns: 20_000_000,
        step_limit_ns: 1_000_000,
        step_unlock_samples: 1,
    }
}

fn t19_feed_servo(
    servo: &mut ClockServo,
    grant: ServoGrant,
    count: u64,
    frame_ppm_ppb: i64,
    ptp_ppm_ppb: i64,
) -> Option<ServoUpdate> {
    let mut last = None;
    for index in 0..count {
        let obs = t19_obs(FS, M0, P0, frame_ppm_ppb, ptp_ppm_ppb, index);
        last = Some(servo.observe(grant, obs).expect("observe"));
    }
    last
}

fn t19_locked_servo(
    owner: ServoOwner,
    frame_ppm_ppb: i64,
    ptp_ppm_ppb: i64,
) -> (ClockServo, ServoGrant, ServoUpdate) {
    let mut servo = ClockServo::new(t19_clean_config(), FS);
    let grant = servo.claim(owner).expect("claim");
    let update = t19_feed_servo(&mut servo, grant, 8, frame_ppm_ppb, ptp_ppm_ppb).expect("update");
    assert_eq!(
        update.state,
        LockState::Locked,
        "servo must lock on clean data"
    );
    (servo, grant, update)
}

fn t19_runtime_locked(
    owner: ServoOwner,
    frame_ppm_ppb: i64,
    ptp_ppm_ppb: i64,
) -> (ClockRuntime, ServoGrant, ClockUpdate) {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    let map = runtime.start_stream(M0, P0, true).expect("start");
    assert_eq!(map.epoch_id(), 1);
    let grant = runtime.claim_servo(owner).expect("claim");
    let mut last = None;
    for index in 0..8 {
        let obs = t19_obs(FS, M0, P0, frame_ppm_ppb, ptp_ppm_ppb, index);
        last = Some(runtime.observe(grant, obs).expect("observe"));
    }
    let update = last.expect("update");
    assert_eq!(update.servo.state, LockState::Locked);
    (runtime, grant, update)
}

// ---------------------------------------------------------------------------
// 常量 / 错误码 / 时间域
// ---------------------------------------------------------------------------

#[test]
fn t19_time_domain_names_are_stable() {
    assert_eq!(TimeDomain::Mono.as_str(), "mono");
    assert_eq!(TimeDomain::MotuFrame.as_str(), "motu_frame");
    assert_eq!(TimeDomain::Ptp.as_str(), "ptp");
    assert_eq!(ServoOwner::MotuFrame.as_str(), "motu_frame");
    assert_eq!(ServoOwner::Ptp.as_str(), "ptp");
    assert_eq!(LockState::Locked.as_str(), "locked");
    assert_eq!(DiscontinuityReason::PtpTimeout.as_str(), "ptp_timeout");
    assert_eq!(DiscontinuityReason::PtpStep.as_str(), "ptp_step");
    assert_eq!(DiscontinuityReason::Quality.as_str(), "quality");
    assert_eq!(DiscontinuityReason::DeviceReopen.as_str(), "device_reopen");
    assert_eq!(DiscontinuityReason::Explicit.as_str(), "explicit");
    assert_eq!(TimeDomain::Mono, TimeDomain::Mono);
    assert_ne!(TimeDomain::Mono, TimeDomain::Ptp);
}

#[test]
fn t19_error_codes_map_to_frozen_rt_status() {
    let table = [
        (ClockError::InvalidSampleRate, RtStatus::ERR_RATE),
        (ClockError::TimestampOverflow, RtStatus::ERR_BAD_LAYOUT),
        (ClockError::FrameOverflow, RtStatus::ERR_BAD_LAYOUT),
        (ClockError::TimestampBeforeEpoch, RtStatus::ERR_BAD_LAYOUT),
        (ClockError::NotStarted, RtStatus::ERR_SWITCH_BUSY),
        (ClockError::EpochAlreadyActive, RtStatus::ERR_SWITCH_BUSY),
        (ClockError::AlreadyAnchored, RtStatus::ERR_SWITCH_BUSY),
        (ClockError::EpochMismatch, RtStatus::RT_E_EPOCH_MISMATCH),
        (ClockError::StaleServoToken, RtStatus::RT_E_EPOCH_MISMATCH),
        (ClockError::ServoAlreadyOwned, RtStatus::RT_E_INVARIANT),
        (ClockError::ServoNotOwned, RtStatus::RT_E_INVARIANT),
        (ClockError::EpochIdExhausted, RtStatus::RT_E_INVARIANT),
        (ClockError::PpmOutOfRange, RtStatus::ERR_ADMISSION_CLOCK),
        (
            ClockError::InvalidUncertainty,
            RtStatus::ERR_ADMISSION_CLOCK,
        ),
        (
            ClockError::InsufficientSamples,
            RtStatus::ERR_ADMISSION_CLOCK,
        ),
        (
            ClockError::TickCalibrationFailed,
            RtStatus::ERR_ADMISSION_CLOCK,
        ),
        (ClockError::NotLocked, RtStatus::RT_E_DEVICE_LOST),
        (
            ClockError::TickBudgetExhausted,
            RtStatus::RT_E_DEADLINE_HARD,
        ),
    ];
    for (error, code) in table {
        assert_eq!(error.code(), code, "{}", error.as_str());
        assert!(!error.as_str().is_empty());
        assert_eq!(format!("{}", error), error.as_str());
    }
    assert_eq!(ClockError::InvalidSampleRate.code().code(), 104);
    assert_eq!(ClockError::NotLocked.code().code(), 407);
    assert_eq!(ClockError::TickBudgetExhausted.code().code(), 405);
}

#[test]
fn t19_tick_budget_spec_limit_is_three() {
    assert_eq!(SPEC_TICK_READS_PER_BLOCK_MAX, 3);
    assert_eq!(MIN_FIT_SAMPLES, 4);
    assert_eq!(ESTIMATOR_CAPACITY, 64);
}

// ---------------------------------------------------------------------------
// mono / tick 源
// ---------------------------------------------------------------------------

#[test]
fn t19_mono_now_is_monotonic_and_progresses() {
    let first = mono_now_ns();
    let mut previous = first;
    for _ in 0..10_000 {
        let sample = mono_now_ns();
        assert!(sample >= previous, "mono clock went backwards");
        previous = sample;
    }
    let start = mono_now_ns();
    while mono_now_ns().saturating_sub(start) < 1_000_000 {
        core::hint::spin_loop();
    }
    let end = mono_now_ns();
    assert!(end > first);
    assert!(end.saturating_sub(start) >= 1_000_000);
}

#[test]
fn t19_rt_tick_is_monotonic() {
    let first = rt_tick();
    let mut saw_change = false;
    for _ in 0..10_000 {
        let tick = rt_tick();
        assert!(tick >= first, "raw tick went backwards");
        if tick != first {
            saw_change = true;
        }
    }
    assert!(saw_change, "rt_tick did not advance");
    assert_eq!(
        uses_raw_tick_counter(),
        cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
    );
}

#[test]
fn t19_tick_budget_enforces_three_reads() {
    let ctl = KhaenRtControlV1::new();
    let mut budget = TickBudget::from_control(&ctl);
    assert_eq!(budget.max_reads(), 3);
    assert_eq!(budget.reads(), 0);
    let a = budget.read(&ctl).expect("read 1");
    let b = budget.read(&ctl).expect("read 2");
    let c = budget.read(&ctl).expect("read 3");
    assert!(b >= a && c >= b);
    assert_eq!(budget.reads(), 3);
    assert_eq!(budget.read(&ctl), Err(ClockError::TickBudgetExhausted));
    budget.reset();
    assert_eq!(budget.reads(), 0);
    assert!(budget.read(&ctl).is_ok());
    // > 3 被钳到规范上限；显式 0 也按 3。
    assert_eq!(TickBudget::new(9).max_reads(), 3);
    assert_eq!(TickBudget::new(0).max_reads(), 3);
    assert_eq!(TickBudget::new(1).max_reads(), 1);
}

#[test]
fn t19_rt_tick_budgeted_reads_ctl_budget() {
    let mut ctl = KhaenRtControlV1::new();
    assert_eq!(tick_read_budget(&ctl), 3);
    ctl.tick_read_budget = 2;
    assert_eq!(tick_read_budget(&ctl), 2);
    ctl.tick_read_budget = 7;
    assert_eq!(tick_read_budget(&ctl), 3);
    let first = rt_tick_budgeted(&ctl);
    let second = rt_tick_budgeted(&ctl);
    assert!(second >= first);
}

#[test]
fn t19_ticks_to_ns_with_freq_known_vectors() {
    assert_eq!(
        ticks_to_ns_with_freq(24_000, 24_000_000).unwrap(),
        1_000_000
    );
    assert_eq!(
        ticks_to_ns_with_freq(1_000_000_000, 1_000_000_000).unwrap(),
        NS_PER_SEC
    );
    assert_eq!(ticks_to_ns_with_freq(0, 24_000_000).unwrap(), 0);
    // floor 语义（不足 1 tick 的余数丢弃）。
    assert_eq!(ticks_to_ns_with_freq(1, 24_000_000).unwrap(), 41);
    assert_eq!(
        ticks_to_ns_with_freq(1, 0),
        Err(ClockError::TickCalibrationFailed)
    );
    assert_eq!(
        ticks_to_ns_with_freq(1, MAX_TICK_FREQ_HZ + 1),
        Err(ClockError::TickCalibrationFailed)
    );
    assert_eq!(
        ticks_to_ns_with_freq(u64::MAX, 1_000),
        Err(ClockError::TimestampOverflow)
    );
    assert_eq!(
        install_tick_frequency_hz(0),
        Err(ClockError::TickCalibrationFailed)
    );
}

#[test]
fn t19_calibrate_tick_source_installs_global_frequency() {
    let calibration =
        calibrate_tick_source(MIN_CALIBRATION_WINDOW_NS).expect("calibrate tick source");
    assert!(calibration.freq_hz >= MIN_TICK_FREQ_HZ);
    assert!(calibration.freq_hz <= MAX_TICK_FREQ_HZ);
    assert_eq!(calibration.rounds, CALIBRATION_ROUNDS as u8);
    assert!(calibration.uncertainty_ppm <= TICK_CALIBRATION_MAX_SPREAD_PPM);
    assert!(calibration.window_ns >= MIN_CALIBRATION_WINDOW_NS);
    assert_eq!(tick_frequency_hz(), Some(calibration.freq_hz));
    // 标定频率自洽：freq 个 tick 恰好 1 s。
    assert_eq!(
        ticks_to_ns_with_freq(calibration.freq_hz, calibration.freq_hz).unwrap(),
        NS_PER_SEC
    );
    // 真实交叉核对：tick 差换算的 ns 与实际 mono 差的相对误差 < 20%。
    let mono0 = mono_now_ns();
    let tick0 = rt_tick();
    spin_for_ns(5_000_000);
    let tick1 = rt_tick();
    let mono1 = mono_now_ns();
    let converted = ticks_to_ns(tick1.saturating_sub(tick0));
    let actual = mono1.saturating_sub(mono0);
    assert!(actual > 0);
    let diff = converted.abs_diff(actual);
    assert!(
        diff * 5 < actual,
        "converted={} actual={} diff={}",
        converted,
        actual,
        diff
    );
}

#[test]
fn t19_probe_clock_support_reports_mono_and_tick() {
    let support = probe_clock_support();
    assert!(support.mono_clock_ok, "mono clock probe failed");
    assert!(support.raw_tick_ok, "raw tick probe failed");
    assert_eq!(
        support.needs_calibration,
        uses_raw_tick_counter() && support.tick_freq_hz.is_none()
    );
    assert_eq!(
        support.rt_ready(),
        support.mono_clock_ok && support.raw_tick_ok && !support.needs_calibration
    );
}

// ---------------------------------------------------------------------------
// 三域映射
// ---------------------------------------------------------------------------

#[test]
fn t19_map_no_drift_known_vectors() {
    let map = ClockMap::new(1, FS, M0, P0)
        .expect("map")
        .with_ptp_locked(true);
    assert_eq!(map.epoch_id(), 1);
    assert_eq!(map.sample_rate_hz(), FS);
    assert_eq!(map.mono_of_frame(0).unwrap(), M0);
    assert_eq!(map.mono_of_frame(FS as u64).unwrap(), M0 + NS_PER_SEC);
    assert_eq!(map.frame_of_mono(M0 + NS_PER_SEC).unwrap(), FS as u64);
    assert_eq!(map.ptp_of_frame(FS as u64).unwrap(), P0 + NS_PER_SEC);
    assert_eq!(map.mono_of_ptp(P0 + NS_PER_SEC).unwrap(), M0 + NS_PER_SEC);
    assert_eq!(map.ptp_of_mono(M0 + NS_PER_SEC).unwrap(), P0 + NS_PER_SEC);
    assert_eq!(map.frame_of_ptp(P0 + NS_PER_SEC).unwrap(), FS as u64);
    assert_eq!(map.ptp_of_mono(M0).unwrap(), P0);
}

#[test]
fn t19_map_frame_rate_sign_is_explicit() {
    // ppm > 0 = frame 钟快于 mono => 每帧更短 => 同一时刻 mono 更早。
    let fast = ClockMap::new(1, FS, M0, 0)
        .unwrap()
        .with_rates(1_000, 0)
        .unwrap();
    assert_eq!(fast.mono_of_frame(FS as u64).unwrap(), M0 + 999_999_000);
    assert!(fast.mono_of_frame(FRAME_STEP).unwrap() < M0 + (FRAME_STEP * NS_PER_SEC / FS as u64));
    // ppm < 0 = frame 钟慢于 mono => 每帧更长。
    let slow = ClockMap::new(1, FS, M0, 0)
        .unwrap()
        .with_rates(-1_000, 0)
        .unwrap();
    assert_eq!(slow.mono_of_frame(FS as u64).unwrap(), M0 + 1_000_001_000);
    assert!(slow.mono_of_frame(FRAME_STEP).unwrap() > M0 + (FRAME_STEP * NS_PER_SEC / FS as u64));
}

#[test]
fn t19_map_frame_mono_roundtrip_many_points() {
    for (frame_rate, sample_rate) in [
        (0i64, 48_000u32),
        (12_500, 48_000),
        (-7_500, 48_000),
        (1_000, 96_000),
        (-1_000, 96_000),
    ] {
        let map = ClockMap::new(7, sample_rate, M0, P0)
            .unwrap()
            .with_rates(frame_rate, 0)
            .unwrap()
            .with_ptp_locked(true);
        for frame in [
            0u64,
            1,
            63,
            64,
            127,
            128,
            4_799,
            4_800,
            4_801,
            1_000_000,
            100_000_000,
        ] {
            let mono = map.mono_of_frame(frame).unwrap();
            assert_eq!(
                map.frame_of_mono(mono).unwrap(),
                frame,
                "frame={} rate={} fs={}",
                frame,
                frame_rate,
                sample_rate
            );
        }
    }
}

#[test]
fn t19_map_ptp_known_vectors_and_roundtrips() {
    let map = ClockMap::new(2, FS, M0, P0)
        .unwrap()
        .with_rates(12_500, 25_000)
        .unwrap()
        .with_ptp_locked(true);
    // mono -> ptp：1 s mono 对应 +25 ppm。
    assert_eq!(
        map.ptp_of_mono(M0 + NS_PER_SEC).unwrap(),
        P0 + 1_000_025_000
    );
    // frame -> ptp：MOTU 快 12.5 ppm、PTP 快 25 ppm => PTP 相对 frame 约 +37.5 ppm。
    let ptp_at_1s = map.ptp_of_frame(FS as u64).unwrap();
    // PTP 相对 frame 约 +37.5 ppm，但以 1 s mono/frame 为基准：+12.5 us。
    let expected = P0 + NS_PER_SEC + 12_500;
    assert!(
        ptp_at_1s.abs_diff(expected) <= 200,
        "ptp_at_1s={} expected~{}",
        ptp_at_1s,
        expected
    );
    for frame in [0u64, 1, 63, 128, 4_800, 48_000, 1_000_000] {
        let ptp = map.ptp_of_frame(frame).unwrap();
        assert_eq!(map.frame_of_ptp(ptp).unwrap(), frame);
        let mono = map.mono_of_frame(frame).unwrap();
        let ptp2 = map.ptp_of_mono(mono).unwrap();
        assert!(
            ptp2.abs_diff(ptp) <= 2,
            "frame={} ptp={} ptp2={}",
            frame,
            ptp,
            ptp2
        );
    }
    assert_eq!(map.frame_ptp_relative_ppm_ppb(), 12_500);
}

#[test]
fn t19_map_ptp_unlocked_queries_error() {
    let map = ClockMap::new(3, FS, M0, P0).unwrap();
    assert!(!map.ptp_locked());
    for result in [
        map.ptp_of_frame(0),
        map.frame_of_ptp(P0),
        map.ptp_of_mono(M0),
        map.mono_of_ptp(P0),
        map.ptp_uncertainty_ns(0),
    ] {
        assert_eq!(result, Err(ClockError::NotLocked));
        assert_eq!(result.unwrap_err().code(), RtStatus::RT_E_DEVICE_LOST);
    }
}

#[test]
fn t19_map_invalid_sample_rate_rejected() {
    assert_eq!(
        ClockMap::new(1, 44_100, M0, P0),
        Err(ClockError::InvalidSampleRate)
    );
    assert!(matches!(
        ClockRuntime::new(88_200, ServoConfig::default()),
        Err(ClockError::InvalidSampleRate)
    ));
    assert!(ClockMap::new(1, 96_000, M0, P0).is_ok());
}

#[test]
fn t19_map_rates_and_uncertainty_sanity_gates() {
    let map = ClockMap::new(1, FS, M0, P0).unwrap();
    assert_eq!(
        map.with_rates(PPM_SANITY_LIMIT_PPB + 1, 0),
        Err(ClockError::PpmOutOfRange)
    );
    assert_eq!(
        map.with_rates(0, -PPM_SANITY_LIMIT_PPB - 1),
        Err(ClockError::PpmOutOfRange)
    );
    let max_ok = ClockMap::new(1, FS, M0, P0)
        .unwrap()
        .with_rates(PPM_SANITY_LIMIT_PPB, -PPM_SANITY_LIMIT_PPB)
        .expect("boundary accepted");
    assert_eq!(max_ok.frame_ppm_ppb(), PPM_SANITY_LIMIT_PPB);
    assert_eq!(
        map.with_uncertainties(UNCERTAINTY_SANITY_LIMIT_PPB + 1, 0, 0),
        Err(ClockError::InvalidUncertainty)
    );
    assert_eq!(
        map.with_uncertainties(0, UNCERTAINTY_SANITY_LIMIT_PPB + 1, 0),
        Err(ClockError::InvalidUncertainty)
    );
}

#[test]
fn t19_map_uncertainty_grows_with_span() {
    let map = ClockMap::new(1, FS, M0, P0)
        .unwrap()
        .with_rates(0, 0)
        .unwrap()
        .with_uncertainties(500, 500, 10_000)
        .unwrap()
        .with_ptp_locked(true);
    assert_eq!(map.mono_uncertainty_ns(0).unwrap(), 10_000);
    let one_hour_frames = 48_000u64 * 3_600;
    let mono_unc = map.mono_uncertainty_ns(one_hour_frames).unwrap();
    // 1 h span * 500 ppb = 1.8 ms，加锚点 10 us。
    assert!(
        (1_700_000..=1_900_000).contains(&mono_unc),
        "mono_unc={}",
        mono_unc
    );
    let ptp_unc = map.ptp_uncertainty_ns(one_hour_frames).unwrap();
    // 1 h span * (500+500) ppb = 3.6 ms，加锚点 10 us。
    assert!(
        (3_400_000..=3_800_000).contains(&ptp_unc),
        "ptp_unc={}",
        ptp_unc
    );
    assert!(map.mono_uncertainty_ns(0).unwrap() < map.mono_uncertainty_ns(4_800).unwrap());
}

#[test]
fn t19_map_timestamp_before_epoch_rejected() {
    let map = ClockMap::new(1, FS, M0, P0).unwrap().with_ptp_locked(true);
    assert_eq!(
        map.frame_of_mono(M0 - 1),
        Err(ClockError::TimestampBeforeEpoch)
    );
    assert_eq!(
        map.mono_of_ptp(P0 - 1),
        Err(ClockError::TimestampBeforeEpoch)
    );
    assert_eq!(
        map.ptp_of_mono(M0 - 1),
        Err(ClockError::TimestampBeforeEpoch)
    );
    assert_eq!(
        map.frame_of_ptp(P0 - 1),
        Err(ClockError::TimestampBeforeEpoch)
    );
}

#[test]
fn t19_map_timestamp_overflow_rejected() {
    let map = ClockMap::new(1, FS, M0, P0).unwrap().with_ptp_locked(true);
    assert_eq!(
        map.mono_of_frame(u64::MAX),
        Err(ClockError::TimestampOverflow)
    );
    assert_eq!(
        map.ptp_of_frame(u64::MAX),
        Err(ClockError::TimestampOverflow)
    );
    assert_eq!(
        map.ptp_of_mono(u64::MAX),
        Err(ClockError::TimestampOverflow)
    );
    // 反向 mono_of_ptp(u64::MAX) 仍在 u64 范围内（PTP 起点更晚），不要求溢出。
    assert!(map.mono_of_ptp(u64::MAX).is_ok());
}

#[test]
fn t19_map_no_accumulated_rounding_drift() {
    // 1e9 帧 = 5.79 h @48k；每块先 floor 再累加的写法会累积误差，有理数映射不许。
    let frame_ppm = 12_500i64;
    let map = ClockMap::new(1, FS, M0, P0)
        .unwrap()
        .with_rates(frame_ppm, 0)
        .unwrap();
    let frame = 1_000_000_000u64;
    let exact_delta =
        (frame as u128) * (NS_PER_SEC as i128 - frame_ppm as i128) as u128 / FS as u128;
    assert_eq!(map.mono_of_frame(frame).unwrap(), M0 + exact_delta as u64);
    // 朴素 128 帧分块 floor 累加的结果显著不同（证明本实现不是逐块累加）。
    let block_frames = 128u64;
    let block_ns =
        (block_frames as u128) * (NS_PER_SEC as i128 - frame_ppm as i128) as u128 / FS as u128;
    let naive = (frame / block_frames) as u128 * block_ns + M0 as u128;
    let actual = map.mono_of_frame(frame).unwrap() as u128;
    assert!(actual > naive, "actual={} naive={}", actual, naive);
    assert!(actual - naive > 1_000_000, "drift should be > 1 ms");
}

#[test]
fn t19_epoch_info_flags_and_layout() {
    let map = ClockMap::new(9, FS, M0, P0).unwrap().with_ptp_locked(true);
    let info = map.epoch_info(3, 128, 20, 22, 0, true);
    assert_eq!(info.epoch_id, 9);
    assert_eq!(info.plan_version, 3);
    assert_eq!(info.sample_rate, FS);
    assert_eq!(info.block_size, 128);
    assert_eq!(info.n_in, 20);
    assert_eq!(info.n_out, 22);
    assert_eq!(info.mode, 0);
    assert_eq!(info.flags & 1, 1);
    assert_eq!(info.start_mono_ns, M0);
    assert_eq!(info.reserved, [0; 3]);
    assert_eq!(core::mem::size_of::<KhaenEpochInfoV1>(), 64);
    assert_eq!(core::mem::align_of::<KhaenEpochInfoV1>(), 64);
    let plain = map.epoch_info(0, 64, 0, 0, 1, false);
    assert_eq!(plain.flags, 0);
}

// ---------------------------------------------------------------------------
// ppm 估计器（合成漂移）
// ---------------------------------------------------------------------------

fn t19_fill_estimator(frame_ppm_ppb: i64, ptp_ppm_ppb: i64, count: u64) -> PpmEstimator {
    let mut estimator = PpmEstimator::new();
    for index in 0..count {
        estimator.push(t19_obs(FS, M0, P0, frame_ppm_ppb, ptp_ppm_ppb, index));
    }
    estimator
}

#[test]
fn t19_estimator_recovers_positive_drift() {
    let estimator = t19_fill_estimator(12_500, 25_000, 16);
    let fit = estimator.fit(FS).expect("fit");
    assert!(
        fit.frame_ppm_ppb.abs_diff(12_500) <= 100,
        "frame ppm={}",
        fit.frame_ppm_ppb
    );
    assert!(
        fit.ptp_ppm_ppb.abs_diff(25_000) <= 200,
        "ptp ppm={}",
        fit.ptp_ppm_ppb
    );
    assert!(fit.frame_valid);
    assert!(fit.ptp_valid);
    assert!(fit.significant);
    assert!(
        fit.frame_sigma_ppb <= 1_000,
        "sigma={}",
        fit.frame_sigma_ppb
    );
    assert_eq!(fit.samples, 16);
    assert_eq!(fit.ptp_samples, 16);
}

#[test]
fn t19_estimator_recovers_negative_drift() {
    let estimator = t19_fill_estimator(-7_500, -2_500, 16);
    let fit = estimator.fit(FS).expect("fit");
    assert!(
        fit.frame_ppm_ppb.abs_diff(-7_500) <= 100,
        "frame ppm={}",
        fit.frame_ppm_ppb
    );
    assert!(
        fit.ptp_ppm_ppb.abs_diff(-2_500) <= 200,
        "ptp ppm={}",
        fit.ptp_ppm_ppb
    );
    assert!(fit.frame_valid);
    assert!(fit.significant);
}

#[test]
fn t19_estimator_flat_clock_is_not_significant() {
    let estimator = t19_fill_estimator(0, 0, 16);
    let fit = estimator.fit(FS).expect("fit");
    assert!(
        fit.frame_ppm_ppb.abs_diff(0) <= 100,
        "ppm={}",
        fit.frame_ppm_ppb
    );
    assert!(fit.frame_valid);
    assert!(!fit.significant);
}

#[test]
fn t19_estimator_insufficient_samples_returns_none() {
    let mut estimator = PpmEstimator::new();
    for index in 0..(MIN_FIT_SAMPLES as u64 - 1) {
        estimator.push(t19_obs(FS, M0, P0, 1_000, 0, index));
    }
    assert_eq!(estimator.len(), MIN_FIT_SAMPLES - 1);
    assert!(estimator.fit(FS).is_none());
    estimator.push(t19_obs(FS, M0, P0, 1_000, 0, MIN_FIT_SAMPLES as u64 - 1));
    assert!(estimator.fit(FS).is_some());
    assert_eq!(PpmEstimator::new().fit(FS), None);
    assert!(PpmEstimator::new().is_empty());
}

#[test]
fn t19_estimator_window_capacity_and_dropped() {
    let mut estimator = PpmEstimator::new();
    for index in 0..(ESTIMATOR_CAPACITY as u64 + 6) {
        estimator.push(t19_obs(FS, M0, P0, 0, 0, index));
    }
    assert_eq!(estimator.len(), ESTIMATOR_CAPACITY);
    assert_eq!(estimator.dropped(), 6);
    let fit = estimator.fit(FS).expect("fit");
    assert_eq!(fit.samples, ESTIMATOR_CAPACITY);
    estimator.reset();
    assert_eq!(estimator.len(), 0);
    assert_eq!(estimator.dropped(), 0);
}

#[test]
fn t19_estimator_rejects_out_of_sanity_drift() {
    // 实际 ns/frame = 0.4 * 标称 => frame 快 1.5e6 ppm，远超 1000 ppm 上限。
    let mut estimator = PpmEstimator::new();
    for index in 0..16u64 {
        let frame = index * FRAME_STEP;
        let mono = M0 + (frame as u128 * NS_PER_SEC as u128 / FS as u128 * 4 / 10) as u64;
        estimator.push(ClockObservation::motu(frame, mono));
    }
    let fit = estimator.fit(FS).expect("fit");
    assert!(!fit.frame_valid, "ppm={}", fit.frame_ppm_ppb);
    assert!(!fit.significant);
    assert!(fit.frame_ppm_ppb > PPM_SANITY_LIMIT_PPB);
}

#[test]
fn t19_estimator_noise_increases_uncertainty() {
    let clean = t19_fill_estimator(5_000, 0, 24);
    let clean_fit = clean.fit(FS).expect("clean fit");
    let mut noisy = PpmEstimator::new();
    let mut seed = 0x1234_5678_9abc_def0u64;
    for index in 0..24u64 {
        let mut obs = t19_obs(FS, M0, P0, 5_000, 0, index);
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let noise = ((seed >> 40) % 200_000) as i64 - 100_000;
        obs.mono_ns = (obs.mono_ns as i128 + noise as i128) as u64;
        noisy.push(obs);
    }
    let noisy_fit = noisy.fit(FS).expect("noisy fit");
    assert!(
        noisy_fit.frame_sigma_ppb > clean_fit.frame_sigma_ppb,
        "clean sigma={} noisy sigma={}",
        clean_fit.frame_sigma_ppb,
        noisy_fit.frame_sigma_ppb
    );
    assert!(noisy_fit.frame_valid, "1ms residual limit still ok");
    assert!(noisy_fit.frame_residual_rms_ns > clean_fit.frame_residual_rms_ns);
}

#[test]
fn t19_estimator_requires_ptp_valid_for_ptp_fit() {
    let mut estimator = PpmEstimator::new();
    for index in 0..16u64 {
        estimator.push(ClockObservation::motu(
            index * FRAME_STEP,
            t19_mono_for_frame(FS, M0, 0, index * FRAME_STEP),
        ));
    }
    let fit = estimator.fit(FS).expect("fit");
    assert!(fit.frame_valid);
    assert!(!fit.ptp_valid);
    assert_eq!(fit.ptp_samples, 0);
}

// ---------------------------------------------------------------------------
// epoch 计数器
// ---------------------------------------------------------------------------

#[test]
fn t19_epoch_begin_monotonic_ids() {
    let mut epoch = EpochClock::new();
    assert_eq!(epoch.current(), None);
    assert_eq!(epoch.epochs_started(), 0);
    let first = epoch.begin(100).expect("begin");
    assert_eq!(first.epoch_id, 1);
    assert!(!first.first_after_discontinuity);
    assert_eq!(first.start_mono_ns, 100);
    assert_eq!(epoch.current(), Some(first));
    assert_eq!(epoch.begin(200), Err(ClockError::EpochAlreadyActive));
    let discontinuity = epoch
        .on_discontinuity(DiscontinuityReason::PtpTimeout, 300, Some(12), None)
        .expect("discontinuity");
    assert_eq!(discontinuity.old_epoch_id, 1);
    assert_eq!(discontinuity.new_epoch_id, 2);
    assert_eq!(discontinuity.old_ppm_ppb, Some(12));
    assert_eq!(discontinuity.new_ppm_ppb, None);
    assert_eq!(discontinuity.at_mono_ns, 300);
    let second = epoch.current().expect("current");
    assert_eq!(second.epoch_id, 2);
    assert!(second.first_after_discontinuity);
    assert_eq!(epoch.discontinuities(), 1);
    assert_eq!(epoch.epochs_started(), 1);
}

#[test]
fn t19_epoch_ids_never_reused() {
    let mut epoch = EpochClock::new();
    epoch.begin(0).expect("begin");
    let mut previous = 1;
    for _ in 0..16 {
        let discontinuity = epoch
            .on_discontinuity(DiscontinuityReason::PtpStep, 1, None, None)
            .expect("discontinuity");
        assert_eq!(discontinuity.new_epoch_id, previous + 1);
        previous = discontinuity.new_epoch_id;
        assert!(epoch.current().unwrap().first_after_discontinuity);
    }
    assert_eq!(epoch.discontinuities(), 16);
}

#[test]
fn t19_epoch_discontinuity_before_begin_rejected() {
    let mut epoch = EpochClock::new();
    assert_eq!(
        epoch.on_discontinuity(DiscontinuityReason::Explicit, 0, None, None),
        Err(ClockError::NotStarted)
    );
    assert_eq!(epoch.current(), None);
}

// ---------------------------------------------------------------------------
// 唯一 servo
// ---------------------------------------------------------------------------

#[test]
fn t19_servo_second_claim_rejected() {
    let mut servo = ClockServo::new(t19_clean_config(), FS);
    let grant = servo.claim(ServoOwner::MotuFrame).expect("claim");
    assert_eq!(servo.owner(), Some(ServoOwner::MotuFrame));
    assert_eq!(
        servo.claim(ServoOwner::Ptp),
        Err(ClockError::ServoAlreadyOwned)
    );
    assert_eq!(
        servo.claim(ServoOwner::MotuFrame).unwrap_err().code(),
        RtStatus::RT_E_INVARIANT
    );
    assert_eq!(servo.owner(), Some(ServoOwner::MotuFrame));
    assert!(servo.release(grant).is_ok());
    assert_eq!(servo.owner(), None);
    // release 后可 claim 另一 owner（generation 递增，旧 grant 失效）。
    let grant2 = servo.claim(ServoOwner::Ptp).expect("reclaim");
    assert!(grant2.generation > grant.generation);
}

#[test]
fn t19_servo_observe_requires_grant() {
    let mut servo = ClockServo::new(t19_clean_config(), FS);
    let obs = t19_obs(FS, M0, P0, 0, 0, 0);
    assert_eq!(
        servo.observe(
            ServoGrant {
                owner: ServoOwner::MotuFrame,
                generation: 1,
            },
            obs
        ),
        Err(ClockError::ServoNotOwned)
    );
    let grant = servo.claim(ServoOwner::MotuFrame).expect("claim");
    assert_eq!(
        servo.observe(
            ServoGrant {
                owner: ServoOwner::Ptp,
                generation: grant.generation,
            },
            obs
        ),
        Err(ClockError::StaleServoToken)
    );
    assert!(servo.release(grant).is_ok());
    assert_eq!(servo.observe(grant, obs), Err(ClockError::ServoNotOwned));
    assert!(servo.poll(grant, 0).is_err());
}

#[test]
fn t19_servo_stale_grant_after_reclaim() {
    let mut servo = ClockServo::new(t19_clean_config(), FS);
    let first = servo.claim(ServoOwner::MotuFrame).expect("claim");
    servo.release(first).expect("release");
    let second = servo.claim(ServoOwner::Ptp).expect("claim");
    assert_ne!(first.generation, second.generation);
    let obs = t19_obs(FS, M0, P0, 0, 0, 0);
    assert_eq!(servo.observe(first, obs), Err(ClockError::StaleServoToken));
    assert!(servo.observe(second, obs).is_ok());
}

#[test]
fn t19_servo_locks_after_configured_samples() {
    let mut servo = ClockServo::new(t19_clean_config(), FS);
    let grant = servo.claim(ServoOwner::MotuFrame).expect("claim");
    assert_eq!(servo.lock_state(), LockState::Unlocked);
    let mut acquired_at = None;
    for index in 0..12u64 {
        let obs = t19_obs(FS, M0, P0, 12_500, 0, index);
        let update = servo.observe(grant, obs).expect("observe");
        if update.lock_acquired {
            acquired_at = Some(index);
        }
    }
    assert!(acquired_at.is_some(), "servo never locked");
    assert_eq!(servo.lock_state(), LockState::Locked);
    let ppm = servo.last_ppm_ppb().expect("ppm");
    assert!(ppm.abs_diff(12_500) <= 100, "ppm={}", ppm);
    let (_, accepted, _, losses) = servo.counters();
    assert!(accepted > 0);
    assert_eq!(losses, 0);
}

#[test]
fn t19_servo_holdover_then_timeout_lock_loss() {
    let (mut servo, grant, last) = t19_locked_servo(ServoOwner::MotuFrame, 0, 0);
    let samples = 8u64;
    let last_obs = t19_obs(FS, M0, P0, 0, 0, samples - 1);
    let last_mono = last_obs.mono_ns;
    assert_eq!(last.state, LockState::Locked);
    let holdover = servo
        .poll(grant, last_mono + 10_000_001)
        .expect("poll")
        .expect("holdover update");
    assert_eq!(holdover.state, LockState::Holdover);
    assert!(holdover.lock_lost.is_none());
    let lost = servo
        .poll(grant, last_mono + 20_000_001)
        .expect("poll")
        .expect("lock lost update");
    assert_eq!(lost.state, LockState::Unlocked);
    assert_eq!(lost.lock_lost, Some(DiscontinuityReason::PtpTimeout));
    assert_eq!(servo.lock_state(), LockState::Unlocked);
    assert_eq!(servo.counters().3, 1);
}

#[test]
fn t19_servo_holdover_recovers_without_discontinuity() {
    let (mut servo, grant, _) = t19_locked_servo(ServoOwner::MotuFrame, 0, 0);
    let last_obs = t19_obs(FS, M0, P0, 0, 0, 7);
    let holdover = servo
        .poll(grant, last_obs.mono_ns + 10_000_001)
        .expect("poll")
        .expect("holdover");
    assert_eq!(holdover.state, LockState::Holdover);
    let obs = t19_obs(FS, M0, P0, 0, 0, 8);
    let recovered = servo.observe(grant, obs).expect("observe");
    assert_eq!(recovered.state, LockState::Locked);
    assert!(recovered.lock_acquired);
    assert!(recovered.lock_lost.is_none());
    assert_eq!(servo.counters().3, 0);
}

#[test]
fn t19_servo_quality_streak_lock_loss() {
    let mut config = t19_clean_config();
    config.lock_ppm_limit_ppb = 100; // 100 ppb 以上视为质量不合格
    let mut servo = ClockServo::new(config, FS);
    let grant = servo.claim(ServoOwner::MotuFrame).expect("claim");
    let mut locked = false;
    for index in 0..8u64 {
        let update = servo
            .observe(grant, t19_obs(FS, M0, P0, 0, 0, index))
            .expect("observe");
        locked |= update.state == LockState::Locked;
    }
    assert!(locked, "servo did not lock on clean data");
    let mut lost = None;
    for index in 8..16u64 {
        let update = servo
            .observe(grant, t19_obs(FS, M0, P0, 2_000, 0, index))
            .expect("observe");
        if update.lock_lost.is_some() {
            lost = update.lock_lost;
            break;
        }
    }
    assert_eq!(lost, Some(DiscontinuityReason::Quality));
    assert_eq!(servo.lock_state(), LockState::Unlocked);
}

#[test]
fn t19_servo_step_detection_lock_loss() {
    let (mut servo, grant, _) = t19_locked_servo(ServoOwner::Ptp, 0, 0);
    // 锁定后注入 +5 ms PTP step（单样本即 > step_limit=1 ms）。
    let step_obs = t19_obs_with_ptp_offset(FS, M0, P0, 0, 0, 8, 5_000_000);
    let update = servo.observe(grant, step_obs).expect("observe");
    assert_eq!(update.lock_lost, Some(DiscontinuityReason::PtpStep));
    assert_eq!(update.state, LockState::Unlocked);
    assert_eq!(servo.lock_state(), LockState::Unlocked);
}

#[test]
fn t19_servo_lock_uncertainty_gate() {
    let mut config = t19_clean_config();
    config.lock_uncertainty_ppb = 10; // 低于 sigma 下限 50 ppb => 永不锁定
    let mut servo = ClockServo::new(config, FS);
    let grant = servo.claim(ServoOwner::MotuFrame).expect("claim");
    for index in 0..16u64 {
        let update = servo
            .observe(grant, t19_obs(FS, M0, P0, 0, 0, index))
            .expect("observe");
        assert_ne!(update.state, LockState::Locked);
        assert!(!update.lock_acquired);
    }
    assert_eq!(servo.counters().1, 0);
}

#[test]
fn t19_servo_owner_measurement_domain_only() {
    // 同一批观测：MOTU 12.5 ppm、PTP 25 ppm。owner 决定采用哪个域的 ppm。
    let (motu_servo, _, motu_update) = t19_locked_servo(ServoOwner::MotuFrame, 12_500, 25_000);
    assert!(motu_update.ppm_ppb.abs_diff(12_500) <= 100);
    let estimate = motu_servo.last_estimate().expect("estimate");
    assert!(
        estimate.ptp_ppm_ppb.abs_diff(25_000) <= 200,
        "ptp measured={}",
        estimate.ptp_ppm_ppb
    );
    let (ptp_servo, _, ptp_update) = t19_locked_servo(ServoOwner::Ptp, 12_500, 25_000);
    assert!(
        ptp_update.ppm_ppb.abs_diff(25_000) <= 200,
        "owner=Ptp ppm={}",
        ptp_update.ppm_ppb
    );
    let ptp_estimate = ptp_servo.last_estimate().expect("estimate");
    assert!(ptp_estimate.frame_ppm_ppb.abs_diff(12_500) <= 100);
}

#[test]
fn t19_servo_deterministic_replay() {
    let mut first_servo = ClockServo::new(t19_clean_config(), FS);
    let first_grant = first_servo.claim(ServoOwner::MotuFrame).expect("claim");
    let mut second_servo = ClockServo::new(t19_clean_config(), FS);
    let second_grant = second_servo.claim(ServoOwner::MotuFrame).expect("claim");
    for index in 0..16u64 {
        let obs = t19_obs(FS, M0, P0, 3_000, -1_000, index);
        let a = first_servo.observe(first_grant, obs).expect("observe");
        let b = second_servo.observe(second_grant, obs).expect("observe");
        assert_eq!(a, b);
    }
    assert_eq!(first_servo.last_ppm_ppb(), second_servo.last_ppm_ppb());
    assert_eq!(first_servo.lock_state(), second_servo.lock_state());
}

// ---------------------------------------------------------------------------
// runtime 集成：epoch + mapping + 唯一 servo
// ---------------------------------------------------------------------------

#[test]
fn t19_runtime_start_epoch_and_epoch_info() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    assert_eq!(runtime.epoch(), None);
    assert_eq!(runtime.map(), None);
    assert_eq!(runtime.required_map(), Err(ClockError::NotStarted));
    assert_eq!(runtime.epoch_info(0, 128, 0, 0, 0), None);
    let map = runtime.start_stream(M0, P0, true).expect("start");
    assert_eq!(map.epoch_id(), 1);
    assert_eq!(map.mono_start_ns(), M0);
    assert_eq!(runtime.epoch().unwrap().epoch_id, 1);
    assert_eq!(
        runtime.start_stream(M0, P0, true),
        Err(ClockError::EpochAlreadyActive)
    );
    let info = runtime.epoch_info(5, 128, 20, 22, 0).expect("info");
    assert_eq!(info.epoch_id, 1);
    assert_eq!(info.plan_version, 5);
    assert_eq!(info.flags, 0);
    assert_eq!(info.start_mono_ns, M0);
}

#[test]
fn t19_runtime_unique_servo_and_stale_grant() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    runtime.start_stream(M0, P0, false).expect("start");
    let grant = runtime.claim_servo(ServoOwner::MotuFrame).expect("claim");
    assert_eq!(
        runtime.claim_servo(ServoOwner::Ptp),
        Err(ClockError::ServoAlreadyOwned)
    );
    runtime.release_servo(grant).expect("release");
    let second = runtime.claim_servo(ServoOwner::Ptp).expect("reclaim");
    let obs = t19_obs(FS, M0, P0, 0, 0, 0);
    assert_eq!(
        runtime.observe(grant, obs),
        Err(ClockError::StaleServoToken)
    );
    assert!(runtime.observe(second, obs).is_ok());
}

#[test]
fn t19_runtime_lock_applies_owner_rate_only() {
    // owner = MotuFrame：只把 frame 域 ppm 写回 mapping；PTP 域测得 25 ppm 但不被第二个闭环调整。
    let (runtime, _grant, _update) = t19_runtime_locked(ServoOwner::MotuFrame, 12_500, 25_000);
    let map = runtime.required_map().expect("map");
    assert!(
        map.frame_ppm_ppb().abs_diff(12_500) <= 100,
        "frame ppm={}",
        map.frame_ppm_ppb()
    );
    assert_eq!(map.ptp_ppm_ppb(), 0, "PTP domain must not be re-closed");
    let estimate = runtime.servo().last_estimate().expect("estimate");
    assert!(estimate.ptp_ppm_ppb.abs_diff(25_000) <= 200);
    let counters = runtime.counters();
    assert_eq!(counters.lock_acquired, 1);
    assert!(counters.updates_applied > 0);
    assert_eq!(counters.discontinuities, 0);
}

#[test]
fn t19_runtime_ptp_owner_updates_ptp_domain_only() {
    let (runtime, _grant, _update) = t19_runtime_locked(ServoOwner::Ptp, 12_500, 25_000);
    let map = runtime.required_map().expect("map");
    assert!(
        map.ptp_ppm_ppb().abs_diff(25_000) <= 200,
        "ptp ppm={}",
        map.ptp_ppm_ppb()
    );
    assert_eq!(map.frame_ppm_ppb(), 0, "MOTU frame domain kept independent");
}

#[test]
fn t19_runtime_timeout_discontinuity_new_epoch_and_reanchor() {
    let (mut runtime, grant, _update) = t19_runtime_locked(ServoOwner::MotuFrame, 0, 0);
    let old_map = *runtime.required_map().expect("map");
    let last_mono = t19_obs(FS, M0, P0, 0, 0, 7).mono_ns;
    let holdover = runtime
        .poll(grant, last_mono + 10_000_001)
        .expect("poll")
        .expect("holdover");
    assert_eq!(holdover.servo.state, LockState::Holdover);
    assert!(holdover.discontinuity.is_none());
    let lost = runtime
        .poll(grant, last_mono + 20_000_001)
        .expect("poll")
        .expect("lost");
    let discontinuity = lost.discontinuity.expect("discontinuity");
    assert_eq!(discontinuity.reason, DiscontinuityReason::PtpTimeout);
    assert_eq!(discontinuity.old_epoch_id, 1);
    assert_eq!(discontinuity.new_epoch_id, 2);
    assert_eq!(lost.epoch_id, Some(2));
    assert_eq!(runtime.map(), None);
    assert_eq!(runtime.epoch().unwrap().epoch_id, 2);
    assert!(runtime.epoch().unwrap().first_after_discontinuity);
    // 旧 mapping 快照不可跨 epoch 使用（runtime 已拒绝返回）。
    assert_eq!(old_map.epoch_id(), 1);
    // reanchor 后建立 epoch 2 mapping，epoch_info flags bit0=1。
    let now = last_mono + 20_000_002;
    let new_map = runtime
        .reanchor_after_discontinuity(now, P0 + NS_PER_SEC, true)
        .expect("reanchor");
    assert_eq!(new_map.epoch_id(), 2);
    assert_eq!(new_map.mono_start_ns(), now);
    let info = runtime.epoch_info(0, 128, 20, 22, 0).expect("info");
    assert_eq!(info.epoch_id, 2);
    assert_eq!(info.flags & 1, 1);
    assert_eq!(info.start_mono_ns, now);
    assert_eq!(
        runtime.reanchor_after_discontinuity(now, P0, true),
        Err(ClockError::AlreadyAnchored)
    );
    let counters = runtime.counters();
    assert_eq!(counters.discontinuities, 1);
    assert_eq!(counters.lock_lost, 1);
}

#[test]
fn t19_runtime_step_discontinuity_new_epoch() {
    let (mut runtime, grant, _update) = t19_runtime_locked(ServoOwner::Ptp, 0, 0);
    let step = t19_obs_with_ptp_offset(FS, M0, P0, 0, 0, 8, 5_000_000);
    let update = runtime.observe(grant, step).expect("observe");
    assert_eq!(update.servo.lock_lost, Some(DiscontinuityReason::PtpStep));
    let discontinuity = update.discontinuity.expect("discontinuity");
    assert_eq!(discontinuity.reason, DiscontinuityReason::PtpStep);
    assert_eq!(discontinuity.new_epoch_id, 2);
    assert_eq!(runtime.map(), None);
    assert_eq!(runtime.epoch().unwrap().epoch_id, 2);
}

#[test]
fn t19_runtime_observe_before_reanchor_does_not_apply() {
    let (mut runtime, grant, _update) = t19_runtime_locked(ServoOwner::Ptp, 0, 0);
    let step = t19_obs_with_ptp_offset(FS, M0, P0, 0, 0, 8, 5_000_000);
    let _ = runtime.observe(grant, step).expect("observe");
    assert_eq!(runtime.map(), None);
    assert_eq!(runtime.required_map(), Err(ClockError::NotStarted));
    let mut observed = 0;
    for index in 9..16u64 {
        let update = runtime
            .observe(grant, t19_obs(FS, M0, P0, 0, 0, index))
            .expect("observe");
        assert!(update.applied_ppm_ppb.is_none(), "no mapping => no apply");
        observed += 1;
    }
    assert_eq!(observed, 7);
    assert_eq!(runtime.map(), None);
    let map = runtime
        .reanchor_after_discontinuity(50_000_000, P0 + NS_PER_SEC, true)
        .expect("reanchor");
    assert_eq!(map.epoch_id(), runtime.epoch().unwrap().epoch_id);
}

#[test]
fn t19_runtime_explicit_device_reopen_discontinuity() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    let map = runtime.start_stream(M0, P0, true).expect("start");
    let discontinuity = runtime
        .request_discontinuity(DiscontinuityReason::DeviceReopen, 10)
        .expect("discontinuity");
    assert_eq!(discontinuity.old_epoch_id, map.epoch_id());
    assert_eq!(discontinuity.new_epoch_id, map.epoch_id() + 1);
    assert_eq!(discontinuity.reason, DiscontinuityReason::DeviceReopen);
    assert_eq!(runtime.map(), None);
    assert_eq!(runtime.counters().discontinuities, 1);
    assert_eq!(
        runtime.request_discontinuity(DiscontinuityReason::Explicit, 20),
        Ok(Discontinuity {
            old_epoch_id: 2,
            new_epoch_id: 3,
            old_ppm_ppb: None,
            new_ppm_ppb: None,
            reason: DiscontinuityReason::Explicit,
            at_mono_ns: 20,
        })
    );
}

#[test]
fn t19_runtime_request_discontinuity_before_start_rejected() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    assert_eq!(
        runtime.request_discontinuity(DiscontinuityReason::Explicit, 0),
        Err(ClockError::NotStarted)
    );
}

#[test]
fn t19_runtime_epoch_mismatch_rejected() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    let map = runtime.start_stream(M0, P0, true).expect("start");
    assert!(runtime.map_for_epoch(1).is_ok());
    assert_eq!(runtime.map_for_epoch(2), Err(ClockError::EpochMismatch));
    assert_eq!(
        runtime.map_for_epoch(2).unwrap_err().code(),
        RtStatus::RT_E_EPOCH_MISMATCH
    );
    assert_eq!(map.ensure_epoch(1), Ok(()));
    assert_eq!(map.ensure_epoch(2), Err(ClockError::EpochMismatch));
    let _ = runtime
        .request_discontinuity(DiscontinuityReason::Explicit, M0 + 1)
        .expect("discontinuity");
    assert_eq!(runtime.map_for_epoch(1), Err(ClockError::NotStarted));
    let new_map = runtime
        .reanchor_after_discontinuity(M0 + 2, P0, true)
        .expect("reanchor");
    assert_eq!(new_map.ensure_epoch(2), Ok(()));
    assert_eq!(runtime.map_for_epoch(2).unwrap().epoch_id(), 2);
}

#[test]
fn t19_runtime_no_cross_epoch_mapping_mix() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    let old_map = runtime.start_stream(M0, P0, true).expect("start");
    let _ = runtime
        .request_discontinuity(DiscontinuityReason::Explicit, M0 + 1_000_000)
        .expect("discontinuity");
    let new_map = runtime
        .reanchor_after_discontinuity(M0 + 2_000_000, P0 + 2_000_000, true)
        .expect("reanchor");
    assert_eq!(old_map.epoch_id(), 1);
    assert_eq!(new_map.epoch_id(), 2);
    assert_ne!(old_map.epoch_id(), new_map.epoch_id());
    // 旧快照自身仍自洽（只读审计），但 runtime 只返回新 epoch mapping。
    assert_eq!(old_map.mono_of_frame(0).unwrap(), M0);
    assert_eq!(new_map.mono_of_frame(0).unwrap(), M0 + 2_000_000);
    assert_eq!(runtime.required_map().unwrap().epoch_id(), 2);
    assert_eq!(runtime.epoch().unwrap().epoch_id, 2);
}

#[test]
fn t19_runtime_invalid_transitions_rejected() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    assert_eq!(
        runtime.reanchor_after_discontinuity(M0, P0, false),
        Err(ClockError::NotStarted)
    );
    runtime.start_stream(M0, P0, false).expect("start");
    assert_eq!(
        runtime.reanchor_after_discontinuity(M0, P0, false),
        Err(ClockError::AlreadyAnchored)
    );
    assert!(matches!(
        ClockRuntime::new(44_100, ServoConfig::default()),
        Err(ClockError::InvalidSampleRate)
    ));
    assert!(ClockRuntime::new(96_000, ServoConfig::default()).is_ok());
}

#[test]
fn t19_runtime_counters_accounting() {
    let (mut runtime, grant, _update) = t19_runtime_locked(ServoOwner::MotuFrame, 0, 0);
    let last_mono = t19_obs(FS, M0, P0, 0, 0, 7).mono_ns;
    let _ = runtime.poll(grant, last_mono + 10_000_001).expect("poll");
    let _ = runtime.poll(grant, last_mono + 20_000_001).expect("poll");
    let counters = runtime.counters();
    assert_eq!(counters.epochs_started, 1);
    assert_eq!(counters.discontinuities, 1);
    assert_eq!(counters.lock_lost, 1);
    assert!(counters.lock_acquired >= 1);
    assert!(counters.updates_applied >= 1);
}

#[test]
fn t19_runtime_mapping_roundtrip_after_lock() {
    let (runtime, _grant, _update) = t19_runtime_locked(ServoOwner::MotuFrame, 12_500, 0);
    let map = runtime.required_map().expect("map");
    for frame in [0u64, 1, 128, 48_000, 1_000_000] {
        let mono = map.mono_of_frame(frame).unwrap();
        assert_eq!(map.frame_of_mono(mono).unwrap(), frame);
    }
    // 锁定后 mapping 的 frame 域 ppm 已由 servo 更新（不是 0）。
    assert!(map.frame_ppm_ppb().abs() > 1_000);
}

#[test]
fn t19_runtime_servo_release_clears_ownership() {
    let mut runtime = ClockRuntime::new(FS, t19_clean_config()).expect("runtime");
    runtime.start_stream(M0, P0, false).expect("start");
    let grant = runtime.claim_servo(ServoOwner::Ptp).expect("claim");
    assert_eq!(runtime.servo().owner(), Some(ServoOwner::Ptp));
    runtime.release_servo(grant).expect("release");
    assert_eq!(runtime.servo().owner(), None);
    let obs = t19_obs(FS, M0, P0, 0, 0, 0);
    assert_eq!(runtime.observe(grant, obs), Err(ClockError::ServoNotOwned));
}
