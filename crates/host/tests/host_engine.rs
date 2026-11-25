//! 块引擎行为测试：配置门禁、直通、块边界静音、假后端往返。

use khaen_host::{
    AudioBackend, BlockEngine, DeviceConfig, DeviceId, EngineConfig, EngineError, NullBackend,
    OwnerError, Passthrough, Silent,
};

fn config() -> EngineConfig {
    EngineConfig {
        block_frames: 128,
        sample_rate: 96_000,
        channels: 2,
    }
}

#[test]
fn config_validation_names_the_problem() {
    assert_eq!(
        EngineConfig {
            block_frames: 0,
            ..config()
        }
        .validate(),
        Err(EngineError::BadConfig("block_frames out of range"))
    );
    assert_eq!(
        EngineConfig {
            channels: 0,
            ..config()
        }
        .validate(),
        Err(EngineError::BadConfig("channels must be positive"))
    );
    assert!(EngineConfig {
        block_frames: 8193,
        ..config()
    }
    .validate()
    .is_err());
    // 速率必须是运行时能力集合的成员（core::rt::SAMPLE_RATES_HZ），不能只校验非零。
    assert_eq!(
        EngineConfig {
            sample_rate: 44_100,
            ..config()
        }
        .validate(),
        Err(EngineError::BadConfig(
            "sample_rate is not in khaen_core::rt::SAMPLE_RATES_HZ (supported: 48000, 96000)"
        ))
    );
    assert!(config().validate().is_ok());
}

#[test]
fn passthrough_reproduces_input_and_counts_blocks() {
    let mut engine = BlockEngine::new(config(), Passthrough).expect("engine");
    let input: Vec<f32> = (0..engine.samples_per_block())
        .map(|index| (index as f32) * 1.0e-3 - 0.25)
        .collect();
    for _ in 0..5 {
        let output = engine.run_block(&input).expect("run");
        assert_eq!(output, &input[..]);
    }
    assert_eq!(engine.stats().blocks, 5);
    assert!(!engine.stats().mute);
}

#[test]
fn wrong_block_length_is_rejected() {
    let mut engine = BlockEngine::new(config(), Passthrough).expect("engine");
    match engine.run_block(&[0.0f32; 7]) {
        Err(EngineError::WrongBlockLength { expected, actual }) => {
            assert_eq!(expected, 256);
            assert_eq!(actual, 7);
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(engine.stats().blocks, 0);
}

#[test]
fn mute_takes_effect_at_the_next_block_boundary() {
    let mut engine = BlockEngine::new(config(), Passthrough).expect("engine");
    let input = vec![0.25f32; engine.samples_per_block()];

    let receipt = engine.request_mute(true);
    assert!(!receipt.idempotent);
    assert_eq!(receipt.applied_block, 1);

    let output = engine.run_block(&input).expect("run");
    assert!(output.iter().all(|sample| *sample == 0.0));
    let stats = engine.stats();
    assert!(stats.mute);
    assert_eq!(stats.mute_applied_block, 1);

    let again = engine.request_mute(true);
    assert!(again.idempotent, "same value must be idempotent");
    assert_eq!(again.applied_block, 1);

    let unmute = engine.request_mute(false);
    assert!(!unmute.idempotent);
    assert_eq!(unmute.applied_block, 2);
    let output = engine.run_block(&input).expect("run");
    assert_eq!(output, &input[..]);
    assert!(!engine.stats().mute);
}

#[test]
fn queued_mute_can_be_cancelled_before_the_boundary() {
    let mut engine = BlockEngine::new(config(), Silent).expect("engine");
    let input = vec![0.5f32; engine.samples_per_block()];

    engine.request_mute(true);
    let cancel = engine.request_mute(false);
    assert!(!cancel.idempotent);
    let output = engine.run_block(&input).expect("run");
    assert!(
        output.iter().all(|sample| *sample == 0.0),
        "Silent chain still zeroes"
    );
    assert!(
        !engine.stats().mute,
        "cancelled request must not latch mute"
    );
}

#[test]
fn null_backend_round_trip_matches_input_sum() {
    let mut engine = BlockEngine::new(config(), Passthrough).expect("engine");
    let mut backend = NullBackend::new();
    let device = engine.device_config(DeviceId::new("null-device"));
    backend.start(&device).expect("start");

    let mut scratch = vec![0.0f32; engine.samples_per_block()];
    let blocks: u64 = 10;
    for _ in 0..blocks {
        engine.pump(&mut backend, &mut scratch).expect("pump");
    }
    assert_eq!(engine.stats().blocks, blocks);
    assert_eq!(backend.blocks(), blocks);
    assert_eq!(backend.played_blocks(), blocks);

    // 假后端输入是 mod 101 的确定性斜坡；直通输出的和应等于同长度斜坡之和。
    let total_samples = blocks * engine.samples_per_block() as u64;
    let expected_sum: f64 = (0..total_samples)
        .map(|index| ((index % 101) as f32 / 101.0 - 0.5) as f64)
        .sum();
    let actual = backend.played_checksum();
    assert!(
        (actual - expected_sum).abs() < 1.0e-3,
        "checksum {actual} vs {expected_sum}"
    );
    backend.stop().expect("stop");
}

#[test]
fn single_device_owner_is_enforced() {
    let guard = khaen_host::DeviceGuard::acquire("motu-lane7").expect("acquire");
    assert_eq!(guard.id().as_str(), "motu-lane7");
    assert_eq!(
        khaen_host::DeviceGuard::acquire("motu-lane7").expect_err("second"),
        OwnerError::AlreadyOwned(guard.id().clone())
    );
    drop(guard);
    assert!(khaen_host::DeviceGuard::acquire("motu-lane7").is_ok());
}

#[test]
fn device_config_matches_engine() {
    let engine = BlockEngine::new(config(), Passthrough).expect("engine");
    let device: DeviceConfig = engine.device_config(DeviceId::new("dev"));
    assert_eq!(device.block_frames, 128);
    assert_eq!(device.channels, 2);
    assert_eq!(device.sample_rate, 96_000);
    assert_eq!(device.samples_per_block(), engine.samples_per_block());
}
