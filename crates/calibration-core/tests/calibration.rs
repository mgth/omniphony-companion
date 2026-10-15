use calibration_core::*;

fn fixture() -> Session {
    Session {
        schema_version: SCHEMA_VERSION,
        id: "session-1".into(),
        microphones: vec![Microphone {
            id: "mic".into(),
            label: "Microphone".into(),
            calibration_id: None,
        }],
        positions: vec![Position {
            id: "seat".into(),
            label: "Reference seat".into(),
        }],
        reference_position_id: "seat".into(),
        baseline: MeasurementBatch {
            schema_version: SCHEMA_VERSION,
            id: "before".into(),
            origin: ObservationOrigin::Simulated,
            timing_reference_id: "shared-clock".into(),
            level_method_id: "reference-band-v1".into(),
            snapshot: InstallationSnapshot {
                installation_id: "room-1".into(),
                layout_revision: "layout-1".into(),
                sample_rate_hz: 48_000,
                speakers: vec![
                    Speaker {
                        id: "a".into(),
                        label: "A".into(),
                        role: SpeakerRole::FullRange,
                        settings: OutputSettings {
                            gain_db: -1.0,
                            delay_ms: 1.0,
                        },
                    },
                    Speaker {
                        id: "b".into(),
                        label: "B".into(),
                        role: SpeakerRole::FullRange,
                        settings: OutputSettings {
                            gain_db: -3.0,
                            delay_ms: 2.0,
                        },
                    },
                ],
            },
            observations: vec![
                Observation {
                    speaker_id: "a".into(),
                    microphone_id: "mic".into(),
                    position_id: "seat".into(),
                    arrival_ms: 5.0,
                    level_dbfs: -20.0,
                    snr_db: 40.0,
                    clipped: false,
                    timing_valid: true,
                },
                Observation {
                    speaker_id: "b".into(),
                    microphone_id: "mic".into(),
                    position_id: "seat".into(),
                    arrival_ms: 8.0,
                    level_dbfs: -24.0,
                    snr_db: 40.0,
                    clipped: false,
                    timing_valid: true,
                },
            ],
        },
    }
}

#[test]
fn absolute_targets_include_existing_offsets_without_boosting() {
    let session = fixture();
    let profile = propose(&session, &CalibrationPolicy::default()).unwrap();
    assert_eq!(
        profile.target_snapshot.speakers[0].settings,
        OutputSettings {
            gain_db: -5.0,
            delay_ms: 4.0
        }
    );
    assert_eq!(
        profile.target_snapshot.speakers[1].settings,
        OutputSettings {
            gain_db: -3.0,
            delay_ms: 2.0
        }
    );
    assert_eq!(session.baseline.snapshot.speakers[0].settings.gain_db, -1.0);
}

#[test]
fn apply_and_restore_are_idempotent_and_refuse_external_edits() {
    let session = fixture();
    let profile = propose(&session, &CalibrationPolicy::default()).unwrap();
    let once = settings_for_apply(&session, &profile, &session.baseline.snapshot).unwrap();
    assert_eq!(once, settings_for_apply(&session, &profile, &once).unwrap());
    let restored = settings_for_restore(&session, &profile, &once).unwrap();
    assert_eq!(restored, session.baseline.snapshot);
    assert_eq!(
        restored,
        settings_for_restore(&session, &profile, &restored).unwrap()
    );
    let mut changed = once;
    changed.speakers[0].settings.gain_db += 1.0;
    assert!(matches!(
        settings_for_apply(&session, &profile, &changed),
        Err(Error::StaleInstallation)
    ));
    assert!(matches!(
        settings_for_restore(&session, &profile, &changed),
        Err(Error::StaleInstallation)
    ));
}

#[test]
fn rejects_bad_quality_or_non_finite_observations() {
    for bad in 0..7 {
        let mut session = fixture();
        let observation = &mut session.baseline.observations[0];
        match bad {
            0 => observation.clipped = true,
            1 => observation.timing_valid = false,
            2 => observation.snr_db = 5.0,
            3 => observation.arrival_ms = f64::NAN,
            4 => observation.level_dbfs = f64::INFINITY,
            5 => observation.snr_db = f64::NEG_INFINITY,
            _ => observation.arrival_ms = -1.0,
        }
        assert!(
            propose(&session, &CalibrationPolicy::default()).is_err(),
            "case {bad}"
        );
    }
}

#[test]
fn rejects_missing_duplicate_and_unknown_identities() {
    let mut missing = fixture();
    missing.baseline.observations.pop();
    assert!(propose(&missing, &CalibrationPolicy::default()).is_err());
    let mut duplicate = fixture();
    duplicate
        .baseline
        .observations
        .push(duplicate.baseline.observations[0].clone());
    assert!(duplicate.validate().is_err());
    for field in 0..3 {
        let mut unknown = fixture();
        let observation = &mut unknown.baseline.observations[0];
        match field {
            0 => observation.speaker_id = "absent".into(),
            1 => observation.microphone_id = "absent".into(),
            _ => observation.position_id = "absent".into(),
        }
        assert!(unknown.validate().is_err());
    }
}

#[test]
fn rejects_excessive_targets_instead_of_clamping_and_claiming_alignment() {
    let session = fixture();
    let mut policy = CalibrationPolicy {
        maximum_delay_ms: 3.0,
        ..Default::default()
    };
    assert!(propose(&session, &policy).is_err());
    policy.maximum_delay_ms = 100.0;
    policy.minimum_gain_db = -4.0;
    assert!(propose(&session, &policy).is_err());
    policy.minimum_gain_db = -60.0;
    policy.arrival_tolerance_ms = f64::NAN;
    assert!(propose(&session, &policy).is_err());
}

#[test]
fn subwoofer_settings_are_preserved_without_peak_alignment() {
    let mut session = fixture();
    session.baseline.snapshot.speakers.push(Speaker {
        id: "sub".into(),
        label: "Subwoofer".into(),
        role: SpeakerRole::Subwoofer,
        settings: OutputSettings {
            gain_db: -4.0,
            delay_ms: 7.0,
        },
    });
    let profile = propose(&session, &CalibrationPolicy::default()).unwrap();
    assert_eq!(
        profile.target_snapshot.speakers[2],
        session.baseline.snapshot.speakers[2]
    );
}

fn verification_fixture() -> (Session, CalibrationProfile, MeasurementBatch) {
    let session = fixture();
    let profile = propose(&session, &CalibrationPolicy::default()).unwrap();
    let mut measured = session.baseline.clone();
    measured.id = "after".into();
    measured.snapshot = profile.target_snapshot.clone();
    // Physical paths: (4 ms, -19 dB) and (6 ms, -21 dB), with measurement noise.
    measured.observations[0].arrival_ms = 8.02;
    measured.observations[0].level_dbfs = -24.1;
    measured.observations[1].arrival_ms = 8.0;
    measured.observations[1].level_dbfs = -24.0;
    (session, profile, measured)
}

#[test]
fn verification_checks_observations_and_reports_failure_not_prediction() {
    let (session, profile, mut measured) = verification_fixture();
    let good = verify(&session, &profile, &measured).unwrap();
    assert!(good.passed);
    assert_eq!(good.before.arrival_spread_ms, 3.0);
    assert_eq!(good.before.level_spread_db, 4.0);
    measured.observations[0].arrival_ms += 2.0;
    assert!(!verify(&session, &profile, &measured).unwrap().passed);
    measured.observations[0].clipped = true;
    assert!(verify(&session, &profile, &measured).is_err());
}

#[test]
fn verification_refuses_incomparable_acquisitions() {
    for change in 0..6 {
        let (session, profile, mut measured) = verification_fixture();
        match change {
            0 => measured.id = session.baseline.id.clone(),
            1 => measured.snapshot.sample_rate_hz = 96_000,
            2 => measured.origin = ObservationOrigin::Imported,
            3 => measured.timing_reference_id = "another-clock".into(),
            4 => measured.level_method_id = "another-band".into(),
            _ => measured.snapshot.speakers[0].settings.gain_db = 0.0,
        }
        assert!(
            verify(&session, &profile, &measured).is_err(),
            "change {change}"
        );
    }
}

#[test]
fn profile_is_bound_to_session_reference_policy_and_original_snapshot() {
    let session = fixture();
    for change in 0..5 {
        let mut profile = propose(&session, &CalibrationPolicy::default()).unwrap();
        match change {
            0 => profile.source_session_id = "other".into(),
            1 => profile.target_snapshot.speakers[0].settings.gain_db = 0.0,
            2 => profile.base_snapshot.layout_revision = "other".into(),
            3 => profile.schema_version = 99,
            _ => profile.reference_position_id = "other".into(),
        }
        assert!(settings_for_apply(&session, &profile, &session.baseline.snapshot).is_err());
    }
}

#[test]
fn additional_positions_do_not_change_reference_alignment() {
    let mut session = fixture();
    let expected = propose(&session, &CalibrationPolicy::default()).unwrap();
    session.positions.push(Position {
        id: "side".into(),
        label: "Side seat".into(),
    });
    let mut other = session.baseline.observations[0].clone();
    other.position_id = "side".into();
    other.arrival_ms = 30.0;
    other.level_dbfs = -60.0;
    session.baseline.observations.push(other);
    assert_eq!(
        propose(&session, &CalibrationPolicy::default()).unwrap(),
        expected
    );
}

#[test]
fn rejects_switching_microphones_within_reference_measurements() {
    let mut session = fixture();
    session.microphones.push(Microphone {
        id: "mic-2".into(),
        label: "Second".into(),
        calibration_id: None,
    });
    session.baseline.observations[1].microphone_id = "mic-2".into();
    assert!(propose(&session, &CalibrationPolicy::default()).is_err());
}

#[test]
fn serialization_preserves_session_and_refuses_future_schema() {
    let session = fixture();
    let json = serde_json::to_string(&session).unwrap();
    let mut reloaded: Session = serde_json::from_str(&json).unwrap();
    reloaded.validate().unwrap();
    assert_eq!(reloaded, session);
    reloaded.schema_version = 99;
    assert!(reloaded.validate().is_err());
}

#[test]
fn fractional_measurements_survive_saved_profile_roundtrip() {
    for seed in 1..100 {
        let mut session = fixture();
        session.baseline.observations[0].arrival_ms = 1.0 + f64::from(seed) / 137.0;
        session.baseline.observations[0].level_dbfs = -19.0 - f64::from(seed) / 193.0;
        let profile = propose(&session, &CalibrationPolicy::default()).unwrap();
        let loaded_session: Session =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        let loaded_profile: CalibrationProfile =
            serde_json::from_str(&serde_json::to_string(&profile).unwrap()).unwrap();
        settings_for_apply(
            &loaded_session,
            &loaded_profile,
            &loaded_session.baseline.snapshot,
        )
        .unwrap();
    }
}
