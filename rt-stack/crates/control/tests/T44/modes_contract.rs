// T44 modes 私有契约测试（由 src/modes.rs 末尾 #[cfg(test)] include! 进 lib 单测）。
//
// 覆盖：A01-A22 / K39-K44（互斥、唯一 servo/epoch、schema/hash 原子切换、失败 off 回退、
// 模拟服务状态、禁调用 C、CueMix 不变、A03 退出码、T42/T43/T19 接口对齐）。
// 运行：cargo test -p khaen-control modes

// ---------------------------------------------------------------------------
// 测试助手
// ---------------------------------------------------------------------------

fn controller() -> ModeController {
    ModeController::with_default_target(Box::new(SimulatedServices::new())).expect("controller")
}

fn controller_with(services: SimulatedServices) -> ModeController {
    ModeController::with_default_target(Box::new(services)).expect("controller")
}

fn switch(controller: &mut ModeController, mode: ControlMode) -> Result<ModeState, ModeError> {
    controller.switch(&ModeRequest::new(mode))
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn calls_len(controller: &ModeController) -> usize {
    controller
        .services()
        .as_simulated()
        .expect("simulated services")
        .calls()
        .len()
}

fn step_position(report: &SwitchReport, name: &str) -> Option<usize> {
    report.steps.iter().position(|step| step.step == name)
}

// ---------------------------------------------------------------------------
// A01/K39：模式集合、互斥、profile 完整性
// ---------------------------------------------------------------------------

#[test]
fn modes_all_five_modes_parse_case_insensitive_and_unknown_rejected() {
    for name in ["off", "toslink", "target_multichannel", "aes67", "dante"] {
        assert!(ControlMode::parse(name).is_some(), "canonical name {name}");
    }
    assert_eq!(ControlMode::parse("TOSLINK"), Some(ControlMode::Toslink));
    assert_eq!(
        ControlMode::parse("Target_MultiChannel"),
        Some(ControlMode::TargetMultichannel)
    );
    assert_eq!(ControlMode::parse("AES67"), Some(ControlMode::Aes67));
    assert_eq!(ControlMode::parse("Dante"), Some(ControlMode::Dante));
    for unknown in ["stereo", "upmix", "4.1.2", ""] {
        assert!(ControlMode::parse(unknown).is_none(), "unknown {unknown}");
    }
    assert_eq!(SELECTABLE_MODES.len(), 5);
    for mode in SELECTABLE_MODES {
        assert_eq!(ControlMode::parse(mode.as_str()), Some(mode));
    }
    let names = allowed_mode_names();
    assert_eq!(names.len(), 5);
    for (index, name) in names.iter().enumerate() {
        assert_eq!(*name, ALLOWED_MODES[index]);
    }
}

#[test]
fn modes_profiles_are_complete_and_playback_names_unique() {
    let target = TargetSpec::p1_412();
    let mut playback_names = Vec::new();
    for mode in SELECTABLE_MODES {
        let profile = ModeProfile::for_mode(mode, &target).expect("profile");
        assert_eq!(profile.mode, mode);
        assert!(SAMPLE_RATES_HZ.contains(&profile.sample_rate));
        assert!(BLOCK_SIZES.contains(&profile.chunksize));
        for unit in profile.units() {
            assert!(!unit.is_forbidden());
        }
        if mode == ControlMode::Off {
            assert!(profile.units().is_empty());
            assert!(profile.servo_owner.is_none());
            assert!(profile.ptp_source.is_none());
            assert!(!profile.requires_ptp_lock);
        } else {
            assert!(profile.servo_owner.is_some());
            assert!(profile.channels > 0);
            assert!(profile.is_playback());
            playback_names.push(profile.name);
        }
    }
    playback_names.sort_unstable();
    playback_names.dedup();
    assert_eq!(playback_names.len(), 4);
}

#[test]
fn modes_clock_owner_is_unique_per_mode() {
    let table = mode_clock_ownership();
    assert_eq!(table.len(), 5);
    for (mode, ptp_source, servo_owner) in table {
        match mode {
            ControlMode::Off => {
                assert!(ptp_source.is_none());
                assert!(servo_owner.is_none());
            }
            ControlMode::Toslink | ControlMode::TargetMultichannel => {
                assert!(ptp_source.is_none());
                assert_eq!(servo_owner, Some(ServoOwner::MotuFrame));
            }
            ControlMode::Aes67 => {
                assert_eq!(ptp_source, Some(AoipProtocol::Aes67.expected_ptp_source()));
                assert_eq!(servo_owner, Some(ServoOwner::Ptp));
            }
            ControlMode::Dante => {
                assert_eq!(
                    ptp_source,
                    Some(AoipProtocol::Inferno.expected_ptp_source())
                );
                assert_eq!(servo_owner, Some(ServoOwner::Ptp));
            }
        }
    }
}

#[test]
fn modes_no_camilla_unit_or_marker_in_any_profile() {
    assert!(!FORBIDDEN_SERVICE_MARKERS.is_empty());
    let names = service_unit_names();
    assert_eq!(names.len(), 5);
    for name in &names {
        for marker in FORBIDDEN_SERVICE_MARKERS {
            assert!(!name.contains(marker), "{name} contains {marker}");
        }
    }
    for unit in ServiceUnit::ALL {
        assert!(!unit.is_forbidden());
    }
    let target = TargetSpec::p1_412();
    for mode in SELECTABLE_MODES {
        let profile = ModeProfile::for_mode(mode, &target).expect("profile");
        assert!(profile.forbidden_units().is_empty());
        for unit in profile.units() {
            for marker in FORBIDDEN_SERVICE_MARKERS {
                assert!(!unit.as_str().contains(marker));
            }
        }
    }
}

#[test]
fn modes_b2_ptp4l_and_phc2sys_not_started_by_p1_profiles() {
    let target = TargetSpec::p1_412();
    for mode in SELECTABLE_MODES {
        let profile = ModeProfile::for_mode(mode, &target).expect("profile");
        assert!(!profile.units().contains(&ServiceUnit::Ptp4lMaster));
        assert!(!profile.units().contains(&ServiceUnit::Phc2sys));
    }
    let dante = ModeProfile::for_mode(ControlMode::Dante, &target).expect("dante");
    assert_eq!(dante.ptp_units, &[ServiceUnit::StatimeInferno]);
    assert!(dante.data_units.is_empty());
}

// ---------------------------------------------------------------------------
// 初始状态 / 基本切换 / 状态 JSON
// ---------------------------------------------------------------------------

#[test]
fn modes_initial_state_is_off_idle_without_servo() {
    let controller = controller();
    let state = controller.current();
    assert_eq!(state.schema, MODE_STATE_SCHEMA);
    assert_eq!(state.mode, ControlMode::Off);
    assert_eq!(state.phase, ModeStatePhase::Idle);
    assert_eq!(state.device_owner, DeviceOwner::Idle);
    assert_eq!(state.epoch_id, 0);
    assert!(state.servo_owner.is_none());
    assert!(state.profile.is_none());
    assert_eq!(state.last_exit_code, 0);
    assert!(state.last_error.is_none());
    assert!(is_hex64(&state.target_hash));
    assert!(is_hex64(&state.config_hash));
    assert_eq!(state.target_hash, TargetSpec::p1_412().fingerprint());
    assert_eq!(state.config_hash, state.runtime_config().fingerprint());
    assert!(state.services.is_empty());
    assert_eq!(controller.counters(), (0, 0));
    assert_eq!(controller.exit_code(), 0);
}

#[test]
fn modes_switch_toslink_reports_actual_profile_and_fields() {
    let mut controller = controller();
    let state = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    assert_eq!(state.mode, ControlMode::Toslink);
    assert_eq!(state.phase, ModeStatePhase::Idle);
    assert_eq!(state.profile_name(), Some(PROFILE_TOSLINK));
    assert_eq!(state.device_owner, DeviceOwner::Playback);
    assert_eq!(state.sample_rate, MODE_PLAYBACK_RATE_HZ);
    assert_eq!(state.chunksize, MODE_DEFAULT_CHUNKSIZE);
    assert_eq!(state.channels, TOSLINK_CHANNELS);
    assert_eq!(state.format, "S24_3LE");
    assert_eq!(state.layout, "2.0");
    assert_eq!(state.servo_owner, Some(ServoOwner::MotuFrame));
    assert_eq!(state.servo_claims, 1);
    assert!(state.epoch_id >= 1);
    assert!(state.ptp_source.is_none());
    assert!(state.last_error.is_none());
    assert_eq!(state.last_exit_code, 0);
    assert_eq!(state.fallback, FallbackAction::None);
    assert!(state.services.iter().all(|status| !status.state.is_busy()));
    let report = controller.last_report().expect("report");
    assert!(report.ok);
    assert_eq!(report.seq, 1);
    assert!(report.started_units.is_empty());
    assert!(step_position(report, "publish").is_some());
    assert_eq!(controller.current().mode, ControlMode::Toslink);
}

#[test]
fn modes_switch_back_to_off_returns_idle_and_keeps_last_good() {
    let mut controller = controller();
    let toslink = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let off = switch(&mut controller, ControlMode::Off).expect("off");
    assert_eq!(off.mode, ControlMode::Off);
    assert_eq!(off.phase, ModeStatePhase::Idle);
    assert_eq!(off.device_owner, DeviceOwner::Idle);
    assert!(off.profile.is_none());
    assert!(off.servo_owner.is_none());
    assert_eq!(off.channels, 0);
    assert_eq!(off.format, "-");
    assert!(off.epoch_id > toslink.epoch_id);
    let last_good = controller.last_good().expect("last good");
    assert_eq!(last_good.mode, ControlMode::Toslink);
}

#[test]
fn modes_all_modes_round_trip_keeps_single_active_mode() {
    let mut controller = controller();
    let mut epochs = Vec::new();
    for mode in [
        ControlMode::Toslink,
        ControlMode::TargetMultichannel,
        ControlMode::Aes67,
        ControlMode::Dante,
        ControlMode::Off,
    ] {
        let state = switch(&mut controller, mode).expect("round trip");
        assert_eq!(state.mode, mode);
        epochs.push(state.epoch_id);
        let busy = ServiceUnit::ALL
            .iter()
            .filter(|unit| controller.services().state(**unit).is_busy())
            .count();
        let expected_busy = match mode {
            ControlMode::Aes67 => 2,
            ControlMode::Dante => 1,
            _ => 0,
        };
        assert_eq!(busy, expected_busy, "mode {mode:?} busy units");
        if mode.is_playback() {
            assert!(state.servo_owner.is_some());
        } else {
            assert!(state.servo_owner.is_none());
        }
    }
    for window in epochs.windows(2) {
        assert!(window[1] > window[0], "epochs must be strictly increasing");
    }
}

#[test]
fn modes_status_json_schema_hash_and_round_trip() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    let json = controller.status_json();
    let expected_schema = JsonValue::str(MODE_STATE_SCHEMA);
    assert_eq!(json.get("schema"), Some(&expected_schema));
    let expected_idle = JsonValue::str("idle");
    assert_eq!(json.get("state"), Some(&expected_idle));
    assert!(json.get("config_hash").is_some());
    assert!(json.get("epoch").is_some());
    assert!(json.get("services").is_some());
    let text = json.to_json();
    let parsed = JsonValue::parse(&text).expect("status json parses");
    assert_eq!(parsed, json);
    let rest = controller.current().rest_status_json();
    let rest_schema = JsonValue::str(MODE_STATUS_SCHEMA);
    assert_eq!(rest.get("schema"), Some(&rest_schema));
    assert_eq!(rest.get("mock"), Some(&JsonValue::Bool(true)));
    assert!(is_hex64(controller.current().config_hash.as_str()));
}

#[test]
fn modes_legacy_status_has_a04_fields_and_null_camilladsp() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::TargetMultichannel).expect("target");
    let legacy = controller.legacy_status_json();
    let legacy_schema = JsonValue::str(LEGACY_STATUS_SCHEMA);
    assert_eq!(legacy.get("schema"), Some(&legacy_schema));
    for key in [
        "schema",
        "ts",
        "mode",
        "state",
        "layout",
        "outputs",
        "ptp_role",
        "ptp_lock",
        "pid",
        "ptp",
        "input_content",
        "aoip",
        "camilladsp",
        "alsa",
        "last_error",
    ] {
        assert!(legacy.get(key).is_some(), "legacy status missing {key}");
    }
    assert_eq!(legacy.get("camilladsp"), Some(&JsonValue::Null));
    assert_eq!(legacy.get("input_content"), Some(&JsonValue::Null));
    assert_eq!(legacy.get("pid"), Some(&JsonValue::Null));
    assert_eq!(legacy.get("ptp_role"), Some(&JsonValue::Null));
    match legacy.get("outputs") {
        Some(JsonValue::Arr(items)) => assert_eq!(items.len(), 7),
        other => panic!("outputs must be array, got {other:?}"),
    }
}

#[test]
fn modes_switch_mode_returns_t42_compatible_mode_ack() {
    let mut controller = controller();
    let ack = controller.switch_mode(ControlMode::Toslink).expect("ack");
    assert_eq!(ack.mode, ControlMode::Toslink);
    assert_eq!(ack.switch_state, "switched");
    assert_eq!(ack.profile.as_deref(), Some(PROFILE_TOSLINK));
    assert!(SAMPLE_RATES_HZ.contains(&ack.sample_rate));
    assert!(BLOCK_SIZES.contains(&ack.chunksize));
    assert!(!ack.target_hash.trim().is_empty());
    assert!(ack.target_hash.len() <= 64);
    let off = controller.switch_mode(ControlMode::Off).expect("off ack");
    assert_eq!(off.mode, ControlMode::Off);
    assert!(off.profile.is_none());
    assert_eq!(off.switch_state, "switched");
}

#[test]
fn modes_dry_run_and_preview_have_no_side_effects() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let revision = controller.current().revision;
    let calls = calls_len(&controller);
    let dry = controller
        .switch(&ModeRequest::new(ControlMode::Dante).with_dry_run(true))
        .expect("dry run");
    assert_eq!(dry.mode, ControlMode::Dante);
    assert_eq!(dry.phase, ModeStatePhase::Idle);
    assert_eq!(dry.channels, DANTE_CHANNELS);
    assert_eq!(controller.current().revision, revision);
    assert_eq!(controller.current().mode, ControlMode::Toslink);
    assert_eq!(calls_len(&controller), calls);
    let preview = controller.preview(ControlMode::Aes67).expect("preview");
    assert_eq!(preview.mode, ControlMode::Aes67);
    assert!(is_hex64(&preview.config_hash));
    assert_eq!(calls_len(&controller), calls);
}

#[test]
fn modes_same_mode_is_idempotent_unless_forced() {
    let mut controller = controller();
    let first = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let claims = first.servo_claims;
    let calls = calls_len(&controller);
    let again = switch(&mut controller, ControlMode::Toslink).expect("idempotent");
    assert_eq!(again.servo_claims, claims);
    assert_eq!(calls_len(&controller), calls);
    let forced = controller
        .switch(&ModeRequest::new(ControlMode::Toslink).with_force(true))
        .expect("forced");
    assert!(forced.servo_claims > claims);
    assert!(forced.epoch_id > first.epoch_id);
    assert_eq!(calls_len(&controller), calls); // toslink has no overlay units
    let _ = switch(&mut controller, ControlMode::Dante).expect("dante");
    let dante_calls = calls_len(&controller);
    let _ = controller
        .switch(&ModeRequest::new(ControlMode::Dante).with_force(true))
        .expect("force dante");
    assert!(calls_len(&controller) > dante_calls);
}

// ---------------------------------------------------------------------------
// A06/A07/K41：串行唯一 owner、calibration 独占
// ---------------------------------------------------------------------------

#[test]
fn modes_switch_busy_is_rejected_with_exit_10_and_rt_120() {
    let mut controller = controller();
    controller.switch_busy = true;
    let error = switch(&mut controller, ControlMode::Toslink).expect_err("busy");
    assert_eq!(error.kind, ModeErrorKind::SwitchBusy);
    assert_eq!(error.exit_code(), 10);
    assert_eq!(error.rt_status(), RtStatus::ERR_SWITCH_BUSY);
    assert!(!error.rolled_back);
    assert_eq!(controller.counters().1, 1);
    controller.switch_busy = false;
    assert!(switch(&mut controller, ControlMode::Toslink).is_ok());
}

#[test]
fn modes_calibration_job_is_exclusive_with_playback_modes() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let calibration = controller.begin_calibration().expect("calibration");
    assert_eq!(calibration.mode, ControlMode::Off);
    assert_eq!(calibration.device_owner, DeviceOwner::Calibration);
    assert!(calibration.servo_owner.is_none());
    assert!(!calibration.is_playback());
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
    let error = switch(&mut controller, ControlMode::Toslink).expect_err("device busy");
    assert_eq!(error.kind, ModeErrorKind::DeviceBusy);
    assert_eq!(error.rt_status(), RtStatus::ERR_DEVICE_BUSY);
    assert_eq!(error.exit_code(), 4);
    assert_eq!(controller.current().device_owner, DeviceOwner::Calibration);
    let ended = controller.end_calibration().expect("release");
    assert_eq!(ended.device_owner, DeviceOwner::Idle);
    assert!(switch(&mut controller, ControlMode::Toslink).is_ok());
}

#[test]
fn modes_calibration_requires_playback_release_first() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    let calibration = controller.begin_calibration().expect("calibration");
    assert_eq!(calibration.mode, ControlMode::Off);
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
}

#[test]
fn modes_end_calibration_without_owner_is_argument_error() {
    let mut controller = controller();
    let error = controller.end_calibration().expect_err("no owner");
    assert_eq!(error.kind, ModeErrorKind::InvalidArgument);
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn modes_error_state_recovers_on_next_successful_switch() {
    let services = SimulatedServices::new().with_lock_after_start(LockState::Locking);
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Dante).expect_err("no lock");
    assert_eq!(error.kind, ModeErrorKind::PtpNotLocked);
    assert_eq!(controller.current().phase, ModeStatePhase::Error);
    let state = switch(&mut controller, ControlMode::Toslink).expect("recover");
    assert_eq!(state.phase, ModeStatePhase::Idle);
    assert_eq!(state.mode, ControlMode::Toslink);
    assert!(state.last_error.is_none());
}

// ---------------------------------------------------------------------------
// 唯一 servo / epoch（A15/K44；core-v1 §5.2）
// ---------------------------------------------------------------------------

#[test]
fn modes_servo_owner_and_claims_are_unique_and_monotonic() {
    let mut controller = controller();
    let toslink = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    assert_eq!(toslink.servo_owner, Some(ServoOwner::MotuFrame));
    assert_eq!(toslink.servo_claims, 1);
    let dante = switch(&mut controller, ControlMode::Dante).expect("dante");
    assert_eq!(dante.servo_owner, Some(ServoOwner::Ptp));
    assert_eq!(dante.servo_claims, 2);
    let off = switch(&mut controller, ControlMode::Off).expect("off");
    assert!(off.servo_owner.is_none());
    assert_eq!(off.servo_claims, 2);
    let aes67 = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    assert_eq!(aes67.servo_owner, Some(ServoOwner::Ptp));
    assert_eq!(aes67.servo_claims, 3);
}

#[test]
fn modes_each_switch_creates_new_epoch_even_on_failure() {
    let mut services = SimulatedServices::new();
    services.fail_next_start(ServiceUnit::StatimeInferno);
    let mut controller = controller_with(services);
    let toslink = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let error = switch(&mut controller, ControlMode::Dante).expect_err("dante failure");
    assert_eq!(error.kind, ModeErrorKind::SelfCheckFailed);
    let after_failure = controller.current().epoch_id;
    assert!(after_failure > toslink.epoch_id);
    let again = switch(&mut controller, ControlMode::Toslink).expect("toslink again");
    assert!(again.epoch_id > after_failure);
}

#[test]
fn modes_servo_grant_transfer_keeps_single_owner_and_invalidates_old_grant() {
    let target = TargetSpec::p1_412();
    let mut clock = ModeClock::new();
    let toslink_profile = ModeProfile::for_mode(ControlMode::Toslink, &target).expect("toslink");
    let first_epoch = clock
        .enter(&toslink_profile, 1_000_000)
        .expect("first epoch");
    let first_grant = clock.grant.expect("grant");
    assert_eq!(clock.owner(), Some(ServoOwner::MotuFrame));
    assert_eq!(clock.claims(), 1);
    assert!(clock
        .servo
        .as_mut()
        .expect("servo")
        .claim(ServoOwner::MotuFrame)
        .is_err());
    let dante_profile = ModeProfile::for_mode(ControlMode::Dante, &target).expect("dante");
    let second_epoch = clock
        .enter(&dante_profile, 2_000_000)
        .expect("second epoch");
    assert!(second_epoch > first_epoch);
    assert_eq!(clock.owner(), Some(ServoOwner::Ptp));
    assert_eq!(clock.claims(), 2);
    assert!(clock
        .servo
        .as_mut()
        .expect("servo")
        .release(first_grant)
        .is_err());
    assert!(clock
        .servo
        .as_mut()
        .expect("servo")
        .claim(ServoOwner::Ptp)
        .is_err());
    clock.release();
    assert!(clock.owner().is_none());
}

#[test]
fn modes_ptp_source_matches_t43_protocol_ownership() {
    assert!(aes67_plan_is_valid());
    assert!(dante_env_is_valid());
    let target = TargetSpec::p1_412();
    let aes67 = ModeProfile::for_mode(ControlMode::Aes67, &target).expect("aes67");
    let dante = ModeProfile::for_mode(ControlMode::Dante, &target).expect("dante");
    assert_eq!(
        aes67.ptp_source,
        Some(AoipProtocol::Aes67.expected_ptp_source())
    );
    assert_eq!(
        dante.ptp_source,
        Some(AoipProtocol::Inferno.expected_ptp_source())
    );
    assert_eq!(aes67.input, ModeInput::Aes67Network);
    assert_eq!(dante.input, ModeInput::DanteNetwork);
    assert!(aes67.input.is_network());
    assert!(dante.input.is_network());
    assert_eq!(aes67.channels, AES67_LOGIC_CHANNELS);
    assert_eq!(dante.channels, DANTE_CHANNELS);
    assert_eq!(dante.servo_owner, Some(ServoOwner::Ptp));
}

// ---------------------------------------------------------------------------
// schema/hash 原子切换
// ---------------------------------------------------------------------------

#[test]
fn modes_config_hash_changes_per_mode_and_matches_t42_fingerprint() {
    let mut controller = controller();
    let mut hashes = vec![controller.current().config_hash.clone()];
    for mode in [
        ControlMode::Toslink,
        ControlMode::TargetMultichannel,
        ControlMode::Aes67,
        ControlMode::Dante,
    ] {
        let state = switch(&mut controller, mode).expect("switch");
        hashes.push(state.config_hash.clone());
    }
    for hash in &hashes {
        assert!(is_hex64(hash), "config hash {hash}");
    }
    let mut deduped = hashes.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        hashes.len(),
        "config hash must be mode bound"
    );
    let expected = RuntimeConfig {
        mode: ControlMode::Dante,
        sample_rate: DANTE_RATE_HZ,
        chunksize: MODE_DEFAULT_CHUNKSIZE,
        target: TargetSpec::p1_412(),
        coeff: None,
    }
    .fingerprint();
    assert_eq!(controller.current().config_hash, expected);
}

#[test]
fn modes_stale_expected_hash_is_rejected_atomically() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let revision = controller.current().revision;
    let calls = calls_len(&controller);
    let error = controller
        .switch(&ModeRequest::new(ControlMode::Off).with_expected_config_hash("0".repeat(64)))
        .expect_err("stale");
    assert_eq!(error.kind, ModeErrorKind::Stale);
    assert_eq!(error.exit_code(), 2);
    assert_eq!(error.rt_status(), RtStatus::ERR_PLAN_STALE);
    assert!(!error.rolled_back);
    assert_eq!(controller.current().revision, revision);
    assert_eq!(controller.current().mode, ControlMode::Toslink);
    assert_eq!(calls_len(&controller), calls);
    assert_eq!(controller.counters().0, 1);
    let current_hash = controller.current().config_hash.clone();
    let off = controller
        .switch(&ModeRequest::new(ControlMode::Off).with_expected_config_hash(current_hash))
        .expect("fresh hash accepted");
    assert_eq!(off.mode, ControlMode::Off);
}

#[test]
fn modes_failure_publishes_one_consistent_off_state() {
    let services =
        SimulatedServices::new().with_aes67_after_start(Aes67ServiceState::new(false, 0, 0));
    let mut controller = controller_with(services);
    let before = controller.current().revision;
    let error = switch(&mut controller, ControlMode::Aes67).expect_err("not ready");
    assert_eq!(error.kind, ModeErrorKind::AoipNotReady);
    let state = controller.current();
    assert_eq!(state.revision, before + 1);
    assert_eq!(state.mode, ControlMode::Off);
    assert_eq!(state.phase, ModeStatePhase::Error);
    assert!(state.profile.is_none());
    assert!(state.servo_owner.is_none());
    assert!(state.epoch_id >= 1);
    let off_config = RuntimeConfig {
        mode: ControlMode::Off,
        sample_rate: MODE_PLAYBACK_RATE_HZ,
        chunksize: MODE_DEFAULT_CHUNKSIZE,
        target: TargetSpec::p1_412(),
        coeff: None,
    };
    assert_eq!(state.config_hash, off_config.fingerprint());
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
}

#[test]
fn modes_schema_constants_are_bound_to_published_status() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let state = controller.current();
    assert_eq!(state.schema, MODE_STATE_SCHEMA);
    assert_eq!(
        controller.status_json().get("schema"),
        Some(&JsonValue::str(MODE_STATE_SCHEMA))
    );
    assert_eq!(
        controller.legacy_status_json().get("schema"),
        Some(&JsonValue::str(LEGACY_STATUS_SCHEMA))
    );
    assert!(MODE_STATE_SCHEMA.ends_with("/v1"));
    assert!(LEGACY_STATUS_SCHEMA.ends_with("/v1"));
    assert!(MODE_STATUS_SCHEMA.ends_with("/v1"));
}

#[test]
fn modes_target_hash_binds_resolved_target() {
    let mut controller = controller();
    let expected = TargetSpec::p1_412().fingerprint();
    for mode in SELECTABLE_MODES {
        let state = switch(&mut controller, mode).expect("switch");
        assert_eq!(state.target_hash, expected);
        assert_eq!(state.target, TargetSpec::p1_412());
    }
    let target_state = switch(&mut controller, ControlMode::TargetMultichannel).expect("target");
    assert_eq!(target_state.layout, "4.1.2");
    assert_eq!(target_state.channels, TARGET_CHANNELS);
}

#[test]
fn modes_preview_hash_matches_real_switch() {
    let mut controller = controller();
    let preview = controller.preview(ControlMode::Dante).expect("preview");
    let state = switch(&mut controller, ControlMode::Dante).expect("dante");
    assert_eq!(preview.mode, state.mode);
    assert_eq!(preview.config_hash, state.config_hash);
    assert_eq!(preview.sample_rate, state.sample_rate);
    assert_eq!(preview.chunksize, state.chunksize);
    assert_eq!(preview.channels, state.channels);
}

// ---------------------------------------------------------------------------
// 失败回退（A16/A03）与模拟服务状态
// ---------------------------------------------------------------------------

#[test]
fn modes_aes67_daemon_start_failure_maps_exit_8_and_rolls_back_off() {
    let mut services = SimulatedServices::new();
    services.fail_next_start(ServiceUnit::Aes67Daemon);
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Aes67).expect_err("daemon fail");
    assert_eq!(error.kind, ModeErrorKind::SelfCheckFailed);
    assert_eq!(error.exit_code(), 8);
    assert!(error.rolled_back);
    let state = controller.current();
    assert_eq!(state.mode, ControlMode::Off);
    assert_eq!(state.phase, ModeStatePhase::Error);
    assert_eq!(state.fallback, FallbackAction::Off);
    assert_eq!(state.last_exit_code, 9);
    assert_eq!(
        state.last_error.as_ref().expect("last error").kind,
        ModeErrorKind::SelfCheckFailed
    );
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
}

#[test]
fn modes_ptp_lock_gate_fails_dante_with_exit_5() {
    let services = SimulatedServices::new().with_lock_after_start(LockState::Locking);
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Dante).expect_err("no lock");
    assert_eq!(error.kind, ModeErrorKind::PtpNotLocked);
    assert_eq!(error.exit_code(), 5);
    assert_eq!(error.rt_status(), RtStatus::ERR_ADMISSION_CLOCK);
    assert!(error.rolled_back);
    let state = controller.current();
    assert_eq!(state.mode, ControlMode::Off);
    assert_eq!(state.device_owner, DeviceOwner::Idle);
    assert!(state.servo_owner.is_none());
    assert_ne!(state.ptp_lock, LockState::Locked);
    assert_eq!(state.last_exit_code, 9);
    let report = controller.last_report().expect("report");
    assert!(!report.ok);
    assert!(report
        .steps
        .iter()
        .any(|step| step.step == "wait_ptp_lock" && !step.ok));
    assert!(report
        .steps
        .iter()
        .any(|step| step.step == "cleanup" && step.ok));
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
}

#[test]
fn modes_allow_unlocked_ptp_switches_with_warning() {
    let services = SimulatedServices::new().with_lock_after_start(LockState::Locking);
    let mut controller = controller_with(services);
    let state = controller
        .switch(&ModeRequest::new(ControlMode::Dante).with_allow_unlocked_ptp(true))
        .expect("allow unlocked");
    assert_eq!(state.mode, ControlMode::Dante);
    assert_eq!(state.ptp_lock, LockState::Locking);
    assert!(state
        .warnings
        .iter()
        .any(|warning| warning.contains("allow_unlocked_ptp")));
    assert_eq!(state.servo_owner, Some(ServoOwner::Ptp));
}

#[test]
fn modes_aes67_service_not_ready_maps_exit_6() {
    let services =
        SimulatedServices::new().with_aes67_after_start(Aes67ServiceState::new(false, 0, 0));
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Aes67).expect_err("not ready");
    assert_eq!(error.kind, ModeErrorKind::AoipNotReady);
    assert_eq!(error.exit_code(), 6);
    assert_eq!(error.rt_status(), RtStatus::RT_E_DEVICE_LOST);
    assert_eq!(controller.current().mode, ControlMode::Off);
}

#[test]
fn modes_aes67_service_rate_mismatch_is_self_check_failure() {
    let services = SimulatedServices::new().with_aes67_after_start(Aes67ServiceState::new(
        true,
        44_100,
        AES67_PTP_DOMAIN,
    ));
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Aes67).expect_err("rate mismatch");
    assert_eq!(error.kind, ModeErrorKind::SelfCheckFailed);
    assert_eq!(error.exit_code(), 8);
}

#[test]
fn modes_missing_ravenna_lkm_maps_prerequisite_exit_3() {
    let mut services = SimulatedServices::new();
    services.mark_missing(ServiceUnit::RavennaLkm);
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Aes67).expect_err("missing lkm");
    assert_eq!(error.kind, ModeErrorKind::PrerequisiteMissing);
    assert_eq!(error.exit_code(), 3);
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
}

#[test]
fn modes_stop_failure_falls_back_off() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    controller
        .services_mut()
        .as_simulated_mut()
        .expect("simulated")
        .fail_next_stop(ServiceUnit::Aes67Daemon);
    let error = switch(&mut controller, ControlMode::Off).expect_err("stop failure");
    assert_eq!(error.kind, ModeErrorKind::DeviceUnavailable);
    assert_eq!(error.exit_code(), 4);
    let state = controller.current();
    assert_eq!(state.mode, ControlMode::Off);
    assert_eq!(state.phase, ModeStatePhase::Error);
    assert!(controller
        .services()
        .as_simulated()
        .expect("simulated")
        .active_units()
        .is_empty());
}

#[test]
fn modes_failure_keeps_cue_mix_untouched_and_never_mentions_camilla() {
    let mut services = SimulatedServices::new();
    services.fail_next_start(ServiceUnit::StatimeInferno);
    let mut controller = controller_with(services);
    let before = controller.cue_mix().clone();
    // F-29：没有真实 CueMix 读取面 -> 如实 NOT_RUN；不得拿内存模型自比当「板端 diff=0」。
    assert_eq!(controller.last_cue_mix(), &CueMixCheck::not_run());
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let not_run = controller.last_cue_mix();
    assert_eq!(not_run.source, "not_run");
    assert!(!not_run.verified);
    assert!(not_run.diff_count.is_none());
    let _ = switch(&mut controller, ControlMode::Dante).expect_err("dante failure");
    assert_eq!(controller.last_cue_mix(), &CueMixCheck::not_run());
    // 内存模型本身未被切换路径写入（这是模型断言，不是设备证据）。
    assert_eq!(before.diff_count(controller.cue_mix()), 0);
    assert_eq!(board_cue_mix_baseline().diff_count(&before), 0);
    let status_text = controller.status_json().to_json();
    for marker in FORBIDDEN_SERVICE_MARKERS {
        assert!(!status_text.contains(marker));
    }
    // A04 合同要求 legacy status 有 camilladsp 键但值必须 null（G-no-C：无 C 进程/无 C 单元）。
    assert_eq!(
        controller.legacy_status_json().get("camilladsp"),
        Some(&JsonValue::Null)
    );
    for name in service_unit_names() {
        for marker in FORBIDDEN_SERVICE_MARKERS {
            assert!(!name.contains(marker));
        }
    }
}

#[test]
fn modes_cue_mix_diff_uses_injected_real_reader() {
    // F-29：注入读取器后，diff 来自切换前后两次读取（不是进程内模型自比）。
    struct FakeCueMixSource {
        snapshots: std::sync::Mutex<std::collections::VecDeque<Option<CueMixRouting>>>,
    }
    impl FakeCueMixSource {
        fn new(snapshots: Vec<Option<CueMixRouting>>) -> Self {
            FakeCueMixSource {
                snapshots: std::sync::Mutex::new(snapshots.into()),
            }
        }
    }
    impl CueMixSource for FakeCueMixSource {
        fn read_snapshot(&self) -> Option<CueMixRouting> {
            self.snapshots
                .lock()
                .expect("snapshots")
                .pop_front()
                .flatten()
        }
    }
    let baseline = board_cue_mix_baseline();
    let controller = ModeController::with_default_target(Box::new(SimulatedServices::new()))
        .expect("controller")
        .with_cue_mix_source(Box::new(FakeCueMixSource::new(vec![
            Some(baseline.clone()),
            Some(baseline.clone()),
        ])));
    let mut controller = controller;
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let check = controller.last_cue_mix().clone();
    assert_eq!(check.source, "board_read");
    assert!(check.verified);
    assert_eq!(check.diff_count, Some(0));
    assert_eq!(check.before.as_deref(), Some(baseline.canonical().as_str()));
    assert_eq!(check.after.as_deref(), Some(baseline.canonical().as_str()));
    // 切换路径不写进程内模型。
    assert_eq!(controller.cue_mix(), &baseline);
    // 读取器返回漂移快照时 diff 必须 > 0（证明比较的是读取结果）。
    let mut drifted = baseline.clone();
    drifted.routes[0].enabled = false;
    let mut controller = ModeController::with_default_target(Box::new(SimulatedServices::new()))
        .expect("controller")
        .with_cue_mix_source(Box::new(FakeCueMixSource::new(vec![
            Some(baseline.clone()),
            Some(drifted),
        ])));
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let check = controller.last_cue_mix().clone();
    assert_eq!(check.source, "board_read");
    assert!(check.verified);
    assert!(check.diff_count.unwrap_or(0) > 0);
    // 读取器不可用（None）-> NOT_RUN，不伪造 0。
    let mut controller = ModeController::with_default_target(Box::new(SimulatedServices::new()))
        .expect("controller")
        .with_cue_mix_source(Box::new(FakeCueMixSource::new(vec![None, None])));
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    assert_eq!(controller.last_cue_mix(), &CueMixCheck::not_run());
}

#[test]
fn modes_failure_preserves_last_good_under_off_policy() {
    let mut services = SimulatedServices::new();
    services.fail_next_start(ServiceUnit::StatimeInferno);
    let mut controller = controller_with(services);
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let _ = switch(&mut controller, ControlMode::Dante).expect_err("dante failure");
    let last_good = controller.last_good().expect("last good");
    assert_eq!(last_good.mode, ControlMode::Toslink);
    assert_eq!(controller.current().mode, ControlMode::Off);
    assert_eq!(controller.current().fallback, FallbackAction::Off);
}

#[test]
fn modes_last_good_policy_restores_playback_after_failure() {
    let mut services = SimulatedServices::new();
    services.fail_next_start(ServiceUnit::StatimeInferno);
    let mut controller = controller_with(services).with_fallback_policy(FallbackPolicy::LastGood);
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let error = switch(&mut controller, ControlMode::Dante).expect_err("dante failure");
    assert_eq!(error.kind, ModeErrorKind::SelfCheckFailed);
    let state = controller.current();
    assert_eq!(state.mode, ControlMode::Toslink);
    assert_eq!(state.phase, ModeStatePhase::Idle);
    assert_eq!(state.fallback, FallbackAction::LastGood);
    assert_eq!(state.servo_owner, Some(ServoOwner::MotuFrame));
    assert_eq!(state.device_owner, DeviceOwner::Playback);
    assert_eq!(state.last_exit_code, 9);
    assert_eq!(
        state.last_error.as_ref().expect("last error").kind,
        ModeErrorKind::SelfCheckFailed
    );
}

#[test]
fn modes_last_good_restore_failure_falls_back_off_with_warning() {
    let mut controller =
        controller_with(SimulatedServices::new()).with_fallback_policy(FallbackPolicy::LastGood);
    let _ = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    {
        let services = controller
            .services_mut()
            .as_simulated_mut()
            .expect("simulated");
        services.mark_missing(ServiceUnit::Aes67Daemon);
        services.fail_next_start(ServiceUnit::StatimeInferno);
    }
    let error = switch(&mut controller, ControlMode::Dante).expect_err("dante failure");
    assert_eq!(error.kind, ModeErrorKind::SelfCheckFailed);
    let state = controller.current();
    assert_eq!(state.mode, ControlMode::Off);
    assert_eq!(state.phase, ModeStatePhase::Error);
    assert_eq!(state.fallback, FallbackAction::Off);
    let report = controller.last_report().expect("report");
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("last-good restore failed")));
}

#[test]
fn modes_failure_rolls_back_with_exit_9_but_reports_cause() {
    let services = SimulatedServices::new().with_lock_after_start(LockState::Locking);
    let mut controller = controller_with(services);
    let error = switch(&mut controller, ControlMode::Dante).expect_err("no lock");
    assert_eq!(error.exit_code(), 5);
    assert!(error.rolled_back);
    assert_eq!(controller.exit_code(), 9);
    assert_eq!(ModeExitCode::RolledBack.as_u8(), 9);
    assert_eq!(
        ModeExitCode::from_kind(ModeErrorKind::RolledBack).as_u8(),
        9
    );
}

// ---------------------------------------------------------------------------
// 模拟服务状态与编排顺序（A07/A11）
// ---------------------------------------------------------------------------

#[test]
fn modes_aes67_start_order_ptp_then_lock_then_data() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    let report = controller.last_report().expect("report").clone();
    let start_positions: Vec<usize> = report
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.step == "start")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(start_positions.len(), 2);
    let lock_position = step_position(&report, "wait_ptp_lock").expect("lock step");
    assert!(start_positions[0] < lock_position);
    assert!(lock_position < start_positions[1]);
    let self_check_position = step_position(&report, "self_check").expect("self check");
    assert!(lock_position < self_check_position);
    assert!(step_position(&report, "new_epoch").is_some());
    assert!(step_position(&report, "claim_servo").is_some());
    assert_eq!(
        report.started_units,
        vec![ServiceUnit::Aes67Daemon, ServiceUnit::RavennaLkm]
    );
}

#[test]
fn modes_stop_previous_services_in_reverse_order() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    controller
        .services_mut()
        .as_simulated_mut()
        .expect("simulated")
        .clear_calls();
    let _ = switch(&mut controller, ControlMode::Dante).expect("dante");
    let calls = controller
        .services()
        .as_simulated()
        .expect("simulated")
        .calls()
        .to_vec();
    assert_eq!(
        calls,
        vec![
            ServiceCall::Stop(ServiceUnit::RavennaLkm),
            ServiceCall::Stop(ServiceUnit::Aes67Daemon),
            ServiceCall::Start(ServiceUnit::StatimeInferno),
        ]
    );
}

#[test]
fn modes_state_services_projection_matches_profile() {
    let mut controller = controller();
    let aes67 = switch(&mut controller, ControlMode::Aes67).expect("aes67");
    assert_eq!(aes67.services.len(), 2);
    assert!(aes67
        .services
        .iter()
        .all(|status| status.state == ServiceRunState::Active));
    let dante = switch(&mut controller, ControlMode::Dante).expect("dante");
    assert_eq!(dante.services.len(), 1);
    assert_eq!(dante.services[0].unit, ServiceUnit::StatimeInferno);
    let off = switch(&mut controller, ControlMode::Off).expect("off");
    assert!(off.services.is_empty());
    for unit in ServiceUnit::ALL {
        assert_eq!(
            controller.services().state(unit),
            ServiceRunState::Inactive,
            "unit {unit:?} must be inactive after off"
        );
    }
}

#[test]
fn modes_simulated_services_state_machine_is_testable() {
    let mut services = SimulatedServices::new();
    assert_eq!(
        services.state(ServiceUnit::Aes67Daemon),
        ServiceRunState::Inactive
    );
    services.start(ServiceUnit::Aes67Daemon).expect("start");
    assert_eq!(
        services.state(ServiceUnit::Aes67Daemon),
        ServiceRunState::Active
    );
    assert_eq!(services.lock(), LockState::Locked);
    assert!(services.aes67_service().daemon_ready);
    assert!(services.active_units().contains(&ServiceUnit::Aes67Daemon));
    services.stop(ServiceUnit::Aes67Daemon).expect("stop");
    assert_eq!(
        services.state(ServiceUnit::Aes67Daemon),
        ServiceRunState::Inactive
    );
    assert_eq!(services.lock(), LockState::Unlocked);
    assert!(!services.aes67_service().daemon_ready);
    services.fail_next_start(ServiceUnit::RavennaLkm);
    assert!(services.start(ServiceUnit::RavennaLkm).is_err());
    assert_eq!(
        services.state(ServiceUnit::RavennaLkm),
        ServiceRunState::Failed
    );
    services.mark_missing(ServiceUnit::StatimeInferno);
    assert!(matches!(
        services.start(ServiceUnit::StatimeInferno),
        Err(ServiceError::NotPresent(ServiceUnit::StatimeInferno))
    ));
    assert!(!services.calls().is_empty());
    services.clear_calls();
    assert!(services.calls().is_empty());
}

#[test]
fn modes_report_sequence_and_last_report_are_available() {
    let mut controller = controller();
    let _ = switch(&mut controller, ControlMode::Toslink).expect("toslink");
    let first = controller.last_report().expect("first report");
    assert_eq!(first.seq, 1);
    assert!(first.ok);
    let _ = switch(&mut controller, ControlMode::Off).expect("off");
    let second = controller.last_report().expect("second report");
    assert_eq!(second.seq, 2);
    assert_eq!(controller.current().switch_seq, 2);
    let json = second.to_json();
    assert!(json.get("steps").is_some());
    assert_eq!(json.get("ok"), Some(&JsonValue::Bool(true)));
}

// ---------------------------------------------------------------------------
// A03/K39 退出码与 RtStatus 映射
// ---------------------------------------------------------------------------

#[test]
fn modes_exit_code_enum_covers_all_contract_values() {
    for value in 0u8..=10u8 {
        let code = ModeExitCode::from_u8(value).expect("exit code");
        assert_eq!(code.as_u8(), value);
        assert!(!code.as_str().is_empty());
    }
    assert!(ModeExitCode::from_u8(11).is_none());
    let expected = [
        (ModeErrorKind::UnknownMode, 1u8),
        (ModeErrorKind::InvalidArgument, 2),
        (ModeErrorKind::Stale, 2),
        (ModeErrorKind::PrerequisiteMissing, 3),
        (ModeErrorKind::DeviceUnavailable, 4),
        (ModeErrorKind::DeviceBusy, 4),
        (ModeErrorKind::PtpNotLocked, 5),
        (ModeErrorKind::AoipNotReady, 6),
        (ModeErrorKind::DspStartFailed, 7),
        (ModeErrorKind::SelfCheckFailed, 8),
        (ModeErrorKind::RolledBack, 9),
        (ModeErrorKind::SwitchBusy, 10),
    ];
    for (kind, code) in expected {
        assert_eq!(kind.exit_code(), code, "kind {kind:?}");
        assert_eq!(ModeExitCode::from_kind(kind).as_u8(), code);
    }
}

#[test]
fn modes_error_rt_status_uses_frozen_core_codes() {
    assert_eq!(
        ModeErrorKind::UnknownMode.rt_status(),
        RtStatus::ERR_UNSUPPORTED
    );
    assert_eq!(
        ModeErrorKind::InvalidArgument.rt_status(),
        RtStatus::ERR_UNSUPPORTED
    );
    assert_eq!(ModeErrorKind::Stale.rt_status(), RtStatus::ERR_PLAN_STALE);
    assert_eq!(
        ModeErrorKind::PrerequisiteMissing.rt_status(),
        RtStatus::ERR_UNSUPPORTED
    );
    assert_eq!(
        ModeErrorKind::DeviceUnavailable.rt_status(),
        RtStatus::ERR_DEVICE_PARAMS
    );
    assert_eq!(
        ModeErrorKind::DeviceBusy.rt_status(),
        RtStatus::ERR_DEVICE_BUSY
    );
    assert_eq!(
        ModeErrorKind::PtpNotLocked.rt_status(),
        RtStatus::ERR_ADMISSION_CLOCK
    );
    assert_eq!(
        ModeErrorKind::AoipNotReady.rt_status(),
        RtStatus::RT_E_DEVICE_LOST
    );
    assert_eq!(
        ModeErrorKind::DspStartFailed.rt_status(),
        RtStatus::ERR_ADMISSION_MEMORY
    );
    assert_eq!(
        ModeErrorKind::SelfCheckFailed.rt_status(),
        RtStatus::ERR_HASH
    );
    // F-30：回滚不复用 119（119 只表示 plan stale），用 control 层扩展码 124。
    use khaen_compat::ws;
    assert_eq!(
        ModeErrorKind::RolledBack.rt_status().code(),
        ws::RT_STATUS_MODE_ROLLED_BACK
    );
    assert_ne!(
        ModeErrorKind::RolledBack.rt_status(),
        RtStatus::ERR_PLAN_STALE
    );
    assert_eq!(
        ModeErrorKind::SwitchBusy.rt_status(),
        RtStatus::ERR_SWITCH_BUSY
    );
    let error = ModeError::new(ModeErrorKind::PtpNotLocked, "x");
    let json = error.to_json();
    assert_eq!(
        json.get("kind"),
        Some(&JsonValue::str(ModeErrorKind::PtpNotLocked.as_str()))
    );
    assert_eq!(json.get("exit_code"), Some(&JsonValue::from(5u8)));
    assert_eq!(json.get("rt_status"), Some(&JsonValue::from(116u32)));
}

// ---------------------------------------------------------------------------
// T42 接口：RuntimeConfig 有效性 / EngineSnapshot 字段
// ---------------------------------------------------------------------------

#[test]
fn modes_runtime_configs_validate_for_every_mode() {
    let target = TargetSpec::p1_412();
    for mode in SELECTABLE_MODES {
        let profile = ModeProfile::for_mode(mode, &target).expect("profile");
        let config = RuntimeConfig {
            mode,
            sample_rate: profile.sample_rate,
            chunksize: profile.chunksize,
            target: target.clone(),
            coeff: None,
        };
        config.validate().expect("runtime config validates");
        assert!(SAMPLE_RATES_HZ.contains(&config.sample_rate));
        assert!(BLOCK_SIZES.contains(&config.chunksize));
    }
}

#[test]
fn modes_engine_fields_mirror_current_state() {
    let mut controller = controller();
    let state = switch(&mut controller, ControlMode::Dante).expect("dante");
    let fields = controller.engine_fields();
    assert_eq!(fields.mode, ControlMode::Dante);
    assert_eq!(fields.mode_profile.as_deref(), Some(PROFILE_DANTE));
    assert_eq!(fields.target_hash, state.target_hash);
    assert_eq!(fields.sample_rate, DANTE_RATE_HZ);
    assert_eq!(fields.chunksize, MODE_DEFAULT_CHUNKSIZE);
    assert_eq!(fields.epoch, state.epoch_id);
    assert_eq!(fields.device_owner, "playback");
    assert!(!fields.calibration_owner);
    assert_eq!(fields.config_hash, state.config_hash);
    assert_eq!(fields.last_exit_code, 0);
    let _ = controller.begin_calibration().expect("calibration");
    assert!(controller.engine_fields().calibration_owner);
    assert_eq!(controller.engine_fields().device_owner, "calibration");
}

#[test]
fn modes_status_sample_evidence() {
    // --nocapture 证据样例：每个模式的状态 JSON + 一次失败回退状态（供 evidence/T44 摘录）。
    let mut controller = controller();
    for mode in SELECTABLE_MODES {
        let state = switch(&mut controller, mode).expect("switch");
        println!("SAMPLE_MODE {}", state.to_json().to_json());
        println!("SAMPLE_LEGACY {}", state.legacy_status_json().to_json());
    }
    let services = SimulatedServices::new().with_lock_after_start(LockState::Locking);
    let mut failing = controller_with(services);
    let error = switch(&mut failing, ControlMode::Dante).expect_err("no lock");
    println!(
        "SAMPLE_FAILURE err={} state={}",
        error.to_json().to_json(),
        failing.status_json().to_json()
    );
    println!(
        "SAMPLE_REPORT {}",
        failing.last_report().expect("report").to_json().to_json()
    );
}

#[test]
fn modes_target_and_service_helpers_are_consistent() {
    assert_eq!(board_cue_mix_baseline().routes.len(), 5);
    assert_eq!(service_unit_names().len(), ServiceUnit::ALL.len());
    let table = mode_clock_ownership();
    assert_eq!(table.len(), SELECTABLE_MODES.len());
    let target = TargetSpec::p1_412();
    for mode in SELECTABLE_MODES {
        let profile = ModeProfile::for_mode(mode, &target).expect("profile");
        let ownership = table
            .iter()
            .find(|(candidate, _, _)| *candidate == mode)
            .expect("ownership row");
        assert_eq!(ownership.1, profile.ptp_source);
        assert_eq!(ownership.2, profile.servo_owner);
    }
}
