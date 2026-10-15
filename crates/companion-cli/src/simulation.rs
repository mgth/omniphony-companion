//! Deterministic observation producer, not a room/PCM simulator.
//! Physical path values are fixed; a second capture reads the new rig state.

use calibration_core::*;

struct SimulatedRig {
    state: InstallationSnapshot,
}

impl SimulatedRig {
    fn new() -> Self {
        let speakers = [
            ("left", SpeakerRole::FullRange, -1.0, 1.0),
            ("right", SpeakerRole::FullRange, 0.0, 0.0),
            ("center", SpeakerRole::FullRange, -2.0, 2.0),
            ("sub", SpeakerRole::Subwoofer, -3.0, 0.0),
        ]
        .into_iter()
        .map(|(id, role, gain_db, delay_ms)| Speaker {
            id: id.into(),
            label: id.into(),
            role,
            settings: OutputSettings { gain_db, delay_ms },
        })
        .collect();
        Self {
            state: InstallationSnapshot {
                installation_id: "simulated-installation".into(),
                layout_revision: "demo-layout-v1".into(),
                sample_rate_hz: 48_000,
                speakers,
            },
        }
    }

    fn capture(&self, id: &str) -> MeasurementBatch {
        let mut observations = Vec::new();
        // (speaker, reference arrival/level, side arrival/level)
        for (speaker_id, arrival, level, side_arrival, side_level) in [
            ("left", 5.0, -20.0, 4.0, -19.0),
            ("right", 8.0, -26.0, 9.0, -27.0),
            ("center", 6.0, -23.0, 7.0, -24.0),
            ("sub", 10.0, -22.0, 11.0, -20.0),
        ] {
            // IDs belong to the fixed rig, never to an imported session.
            if let Some(speaker) = self.state.speakers.iter().find(|s| s.id == speaker_id) {
                for (position_id, microphone_id, time, db) in [
                    ("reference", "mic-1", arrival, level),
                    ("side", "mic-2", side_arrival, side_level),
                ] {
                    observations.push(Observation {
                        speaker_id: speaker_id.into(),
                        microphone_id: microphone_id.into(),
                        position_id: position_id.into(),
                        arrival_ms: time + speaker.settings.delay_ms,
                        level_dbfs: db + speaker.settings.gain_db,
                        snr_db: 50.0,
                        clipped: false,
                        timing_valid: true,
                    });
                }
            }
        }
        MeasurementBatch {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            origin: ObservationOrigin::Simulated,
            timing_reference_id: "simulated-shared-clock".into(),
            level_method_id: "simulated-relative-level-v1".into(),
            snapshot: self.state.clone(),
            observations,
        }
    }
}

pub fn run() -> Result<(
    Session,
    CalibrationProfile,
    MeasurementBatch,
    VerificationReport,
)> {
    let mut rig = SimulatedRig::new();
    let session = Session {
        schema_version: SCHEMA_VERSION,
        id: "demo-session".into(),
        microphones: ["mic-1", "mic-2"]
            .into_iter()
            .map(|id| Microphone {
                id: id.into(),
                label: id.into(),
                calibration_id: Some("simulated-flat-microphone".into()),
            })
            .collect(),
        positions: ["reference", "side"]
            .into_iter()
            .map(|id| Position {
                id: id.into(),
                label: id.into(),
            })
            .collect(),
        reference_position_id: "reference".into(),
        baseline: rig.capture("baseline"),
    };
    let profile = propose(&session, &CalibrationPolicy::default())?;
    rig.state = settings_for_apply(&session, &profile, &rig.state)?;
    let measurements = rig.capture("verification");
    let report = verify(&session, &profile, &measurements)?;
    rig.state = settings_for_restore(&session, &profile, &rig.state)?;
    debug_assert_eq!(rig.state, session.baseline.snapshot);
    Ok((session, profile, measurements, report))
}
