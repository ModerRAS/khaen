//! 配置校验 / 通道与块语义 / 重置语义（T41 交付 3f）。

use khaen_asrc::{Asrc, AsrcConfig, AsrcError, DriftController, Interpolation};

fn base_config() -> AsrcConfig {
    AsrcConfig::new(2, 128, 48_000)
}

/// 断言配置非法：validate 与 Asrc::new 都必须拒绝（fail-closed）。
fn assert_invalid(name: &str, mutate: fn(&mut AsrcConfig)) {
    let mut config = base_config();
    mutate(&mut config);
    assert!(config.validate().is_err(), "配置 {name} 应被 validate 拒绝");
    assert!(
        matches!(Asrc::new(config), Err(AsrcError::BadConfig(_))),
        "配置 {name} 应被 Asrc::new 拒绝为 BadConfig"
    );
}

#[test]
fn invalid_configs_are_rejected() {
    assert_invalid("channels=0", |config| config.channels = 0);
    assert_invalid("channels=65", |config| config.channels = 65);
    assert_invalid("block_frames=0", |config| config.block_frames = 0);
    assert_invalid("sample_rate=0", |config| config.sample_rate = 0);
    assert_invalid("target_fill < block", |config| {
        config.target_fill_frames = config.block_frames - 1;
    });
    assert_invalid("max_ppm=0", |config| config.max_ppm = 0.0);
    assert_invalid("max_ppm<0", |config| config.max_ppm = -1.0);
    assert_invalid("max_ppm=NaN", |config| config.max_ppm = f64::NAN);
    assert_invalid("max_ppm too big", |config| {
        config.max_ppm = khaen_asrc::MAX_CONFIGURABLE_PPM + 1.0;
    });
    assert_invalid("kp<0", |config| config.kp_ppm_per_frame = -1.0);
    assert_invalid("ki=NaN", |config| {
        config.ki_ppm_per_frame_sec = f64::NAN;
    });
    assert_invalid("both gains zero", |config| {
        config.kp_ppm_per_frame = 0.0;
        config.ki_ppm_per_frame_sec = 0.0;
    });
    assert_invalid("fill_lpf=0", |config| config.fill_lpf_sec = 0.0);
    assert_invalid("sinc_len too small", |config| {
        config.interp = Interpolation::Sinc;
        config.sinc_len = 4;
    });
    assert_invalid("sinc_oversampling=0", |config| {
        config.interp = Interpolation::Sinc;
        config.sinc_oversampling = 0;
    });
}

#[test]
fn default_and_explicit_configs_are_valid() {
    let default = AsrcConfig::default();
    default.validate().expect("默认配置必须合法");
    base_config().validate().expect("2ch/128/48k 必须合法");
    let poly = Asrc::new(base_config()).expect("poly 构造");
    assert_eq!(poly.config().interp, Interpolation::Poly);
    assert_eq!(poly.output_frames(), 128);
    let mut sinc_config = base_config();
    sinc_config.interp = Interpolation::Sinc;
    let sinc = Asrc::new(sinc_config).expect("sinc 构造");
    assert_eq!(sinc.config().interp, Interpolation::Sinc);
}

#[test]
fn interpolation_parsing_is_strict_but_case_insensitive() {
    assert_eq!(Interpolation::parse("poly"), Some(Interpolation::Poly));
    assert_eq!(Interpolation::parse(" SINC "), Some(Interpolation::Sinc));
    assert_eq!(Interpolation::parse("Poly"), Some(Interpolation::Poly));
    assert_eq!(Interpolation::parse("off"), None);
    assert_eq!(Interpolation::parse(""), None);
    assert_eq!(Interpolation::parse("linear"), None);
    assert_eq!(Interpolation::name(Interpolation::Poly), "poly");
    assert_eq!(Interpolation::name(Interpolation::Sinc), "sinc");
}

#[test]
fn planar_buffers_and_process_contract() {
    for channels in [1usize, 2, 8] {
        let mut config = AsrcConfig::new(channels, 128, 48_000);
        config.interp = Interpolation::Poly;
        let mut asrc = Asrc::new(config).expect("asrc");
        assert_eq!(asrc.input_mut().channels(), channels);
        assert_eq!(asrc.output().channels(), channels);
        assert_eq!(asrc.output_frames(), 128);
        assert_eq!(asrc.output().frames(), 128);
        let need = asrc.input_frames_next();
        assert!(
            need <= asrc.input_mut().frames(),
            "input_frames_next={need} 超过预分配输入缓冲容量"
        );
        {
            let input = asrc.input_mut();
            for channel in 0..channels {
                for frame in 0..need {
                    input.channel_mut(channel)[frame] = 0.25;
                }
            }
        }
        let outcome = asrc.process().expect("process");
        assert_eq!(outcome.input_frames, need);
        assert_eq!(outcome.output_frames, 128);
        assert_eq!(asrc.blocks(), 1);
        // 输出必须落在预分配 planar 缓冲里（长度 = block_frames）。
        for channel in 0..channels {
            assert_eq!(asrc.output().channel(channel).len(), 128);
        }
    }
}

#[test]
fn channel_count_is_fixed_per_instance() {
    // 声道数是构造期固定量：换声道数必须重建实例（host 换流只 reset、不换声道；
    // 若 AES67 流的 RTP 声道数变化，host 重建 Aes67Input）。这里验证两个实例互不影响。
    let mut one_ch = Asrc::new({
        let mut config = AsrcConfig::new(1, 128, 48_000);
        config.interp = Interpolation::Poly;
        config
    })
    .expect("1ch asrc");
    let mut two_ch = Asrc::new({
        let mut config = AsrcConfig::new(2, 128, 48_000);
        config.interp = Interpolation::Poly;
        config
    })
    .expect("2ch asrc");
    assert_eq!(one_ch.input_mut().channels(), 1);
    assert_eq!(two_ch.input_mut().channels(), 2);
    for asrc in [&mut one_ch, &mut two_ch] {
        let need = asrc.input_frames_next();
        let channels = asrc.input_mut().channels();
        {
            let input = asrc.input_mut();
            for channel in 0..channels {
                for frame in 0..need {
                    input.channel_mut(channel)[frame] = 0.0;
                }
            }
        }
        let outcome = asrc.process().expect("process");
        assert_eq!(outcome.output_frames, 128);
        assert_eq!(asrc.output().channels(), channels);
    }
}

#[test]
fn reset_clears_ratio_estimate_and_bypass() {
    let mut config = base_config();
    config.interp = Interpolation::Poly;
    let mut asrc = Asrc::new(config).expect("asrc");
    // 用极端 fill 把控制器推到限幅，制造非零比率/估计。
    for _ in 0..600 {
        let need = asrc.input_frames_next();
        {
            let input = asrc.input_mut();
            for channel in 0..2 {
                for frame in 0..need {
                    input.channel_mut(channel)[frame] = 0.0;
                }
            }
        }
        asrc.process().expect("process");
        asrc.update_fill(0).expect("update");
    }
    let before = asrc.status(0);
    assert!(before.ratio_ppm.abs() > 1.0, "极端 fill 下比率应有明显修正");
    assert!(before.ppm_estimate.abs() > 1.0, "极端 fill 下估计应为非零");
    asrc.note_bypass(AsrcError::Process("test"));
    assert!(
        !asrc.status(0).enabled,
        "bypass 后 status.enabled 应为 false"
    );

    asrc.reset();
    let after = asrc.status(0);
    assert_eq!(after.ratio_ppm, 0.0, "reset 后比率修正应清零");
    assert_eq!(after.ppm_estimate, 0.0, "reset 后积分估计应清零");
    assert!(after.enabled, "reset 应清除 bypass");
    assert!(after.last_error.is_none(), "reset 应清除 last_error");
    assert_eq!(asrc.blocks(), 0);
    assert!(!asrc.is_bypassed());
}

#[test]
fn controller_output_is_clamped_to_max_ppm() {
    let mut config = base_config();
    config.kp_ppm_per_frame = 100.0;
    config.ki_ppm_per_frame_sec = 100.0;
    let mut controller = DriftController::new(&config).expect("controller");
    // fill 远低于目标：需要正 ppm（少消费），应被限幅在 +max_ppm。
    for _ in 0..1000 {
        let ppm = controller.update(0);
        assert!(ppm <= config.max_ppm, "正修正未被限幅：{ppm}");
    }
    assert_eq!(controller.ppm(), config.max_ppm);
    // fill 远高于目标：需要负 ppm，应被限幅在 -max_ppm。
    for _ in 0..4000 {
        let ppm = controller.update(64_000);
        assert!(ppm >= -config.max_ppm, "负修正未被限幅：{ppm}");
    }
    assert_eq!(controller.ppm(), -config.max_ppm);
    // 估计（积分）也被限幅。
    assert!(controller.estimate_ppm().abs() <= config.max_ppm);
}

#[test]
fn status_reports_fill_and_effective_enabled() {
    let mut config = base_config();
    config.interp = Interpolation::Poly;
    let mut asrc = Asrc::new(config).expect("asrc");
    let status = asrc.status(123);
    assert_eq!(status.fill_frames, 123);
    assert!(status.enabled);
    let need = asrc.input_frames_next();
    {
        let input = asrc.input_mut();
        for channel in 0..2 {
            for frame in 0..need {
                input.channel_mut(channel)[frame] = 0.0;
            }
        }
    }
    asrc.process().expect("process");
    let _ = asrc.update_fill(123).expect("update");
    let status = asrc.status(123);
    assert_eq!(status.fill_frames, 123);
    assert!(status.ratio_ppm.is_finite());
    assert!(status.ppm_estimate.is_finite());
    // 未启用的默认状态。
    let disabled = khaen_asrc::AsrcStatus::disabled();
    assert!(!disabled.enabled);
    assert_eq!(disabled.ratio_ppm, 0.0);
    assert_eq!(disabled.fill_frames, 0);
    assert!(disabled.last_error.is_none());
}
