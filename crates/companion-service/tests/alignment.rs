use calibration_core::{Blocker, ExclusionReason, reason::SnrBand};
use companion_service::{
    alignment,
    batch::BatchOutcome,
    controller::{Backend, Settings},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use measurement_capture::{diagnostics, impulse, timing};
use omniphony_adapter::{
    Snapshot,
    protocol::{Output, OutputSetting},
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Rig {
    snapshot: Mutex<Snapshot>,
    slow_output: bool,
    writes: AtomicUsize,
    lose_ack: AtomicBool,
    bad_repeat: AtomicBool,
    silent_batch: AtomicBool,
    captures: AtomicUsize,
    rejected: Mutex<std::collections::BTreeMap<String, String>>,
    /// Outputs this much louder at the microphone; a capture that reaches full scale
    /// clips and is rejected as the measurement would.
    loud: Mutex<std::collections::BTreeMap<String, f32>>,
    /// Outputs whose next take gets noise of this amplitude during the reference sweep
    /// after them, once: a passing disturbance in the room.
    disturb: Mutex<std::collections::BTreeMap<String, f32>>,
    batches: Mutex<Vec<Vec<String>>>,
    /// Addresses the renderer was written to or played through.
    endpoints: Mutex<Vec<SocketAddr>>,
}
impl Rig {
    fn new() -> Self {
        Self {
            slow_output: false,
            snapshot: Mutex::new(Snapshot {
                instance_id: "rig".into(),
                revision: "rig:0".into(),
                sample_rate: 48000,
                master_gain_db: 0.0,
                max_delay_ms: 100.0,
                unavailable_reason: None,
                outputs: ["L", "R", "Unused"]
                    .iter()
                    .enumerate()
                    .map(|(index, id)| Output {
                        id: (*id).into(),
                        name: (*id).into(),
                        index: index as u32,
                        gain_db: 0.0,
                        delay_ms: 0.0,
                        muted: false,
                        freq_low_hz: None,
                        freq_high_hz: None,
                        azimuth_deg: None,
                        elevation_deg: None,
                        distance_m: None,
                    })
                    .collect(),
            }),
            writes: AtomicUsize::new(0),
            lose_ack: AtomicBool::new(false),
            bad_repeat: AtomicBool::new(false),
            silent_batch: AtomicBool::new(false),
            captures: AtomicUsize::new(0),
            rejected: Mutex::new(Default::default()),
            loud: Mutex::new(Default::default()),
            disturb: Mutex::new(Default::default()),
            batches: Mutex::new(vec![]),
            endpoints: Mutex::new(vec![]),
        }
    }
}
impl Backend for Rig {
    fn inputs(&self) -> Result<Vec<measurement_capture::InputDevice>, String> {
        Ok(vec![])
    }
    fn discover(&self, _: SocketAddr) -> Result<Snapshot, String> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
    fn measure(
        &self,
        _: MeasureRequest,
        _: &AtomicBool,
        _: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String> {
        unreachable!()
    }
    fn set_outputs(
        &self,
        endpoint: SocketAddr,
        expected: Snapshot,
        settings: Vec<OutputSetting>,
    ) -> Result<Snapshot, String> {
        self.endpoints.lock().unwrap().push(endpoint);
        let mut state = self.snapshot.lock().unwrap();
        if *state != expected {
            return Err("stale".into());
        }
        for setting in settings {
            let o = state
                .outputs
                .iter_mut()
                .find(|o| o.id == setting.output_id)
                .unwrap();
            o.gain_db = setting.gain_db;
            o.delay_ms = setting.delay_ms;
        }
        let n = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        state.revision = format!("rig:{n}");
        if self.lose_ack.load(Ordering::Relaxed) {
            return Err("lost acknowledgement".into());
        }
        Ok(state.clone())
    }
    fn measure_batch(
        &self,
        args: MeasureRequest,
        outputs: Vec<String>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, MeasureProgress),
    ) -> Result<BatchOutcome, String> {
        assert_eq!(*self.snapshot.lock().unwrap(), args.snapshot);
        self.endpoints.lock().unwrap().push(args.endpoint);
        self.batches.lock().unwrap().push(outputs.clone());
        let pass = self.captures.fetch_add(1, Ordering::Relaxed);
        if self.silent_batch.load(Ordering::Relaxed) {
            return Ok(BatchOutcome {
                directory: args.out_dir,
                takes: vec![],
                cancelled: false,
                error: Some(measurement_capture::input::NO_SIGNAL.into()),
            });
        }
        std::fs::create_dir_all(&args.out_dir).unwrap();
        let mut takes = vec![];
        for (index, id) in outputs.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            progress(index, MeasureProgress::Analyzing);
            let source = args.sweep.samples(48000).unwrap();
            let mut samples = vec![0.0; source.len() * 3 + 48000];
            for (segment, output_id) in ["L", id.as_str(), "L"].iter().enumerate() {
                let o = args
                    .snapshot
                    .outputs
                    .iter()
                    .find(|o| o.id == *output_id)
                    .unwrap();
                let physical_delay = if *output_id == "L" {
                    144
                } else if self.slow_output {
                    7824
                } else {
                    288
                };
                let delay = physical_delay + (o.delay_ms as f64 * 48.0).round() as usize;
                let gain = if *output_id == "L" { 0.5 } else { 0.25 };
                let loud = self.loud.lock().unwrap().get(*output_id).copied();
                let gain = gain * 10_f32.powf(o.gain_db / 20.0) * loud.unwrap_or(1.0);
                let start = 24000 + source.len() * segment + delay;
                for (j, x) in source.iter().enumerate() {
                    samples[start + j] += gain * x;
                }
            }
            if let Some(noise) = self.disturb.lock().unwrap().remove(id) {
                let padding = 12000;
                let body = args.sweep.duration_ms as usize * 48;
                let start = 24000 + source.len() * 2 + 144 + padding;
                let mut seed = 7_u32;
                for x in &mut samples[start..start + body] {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    *x += noise * (seed as f32 / u32::MAX as f32 - 0.5) * 2.0;
                }
            }
            let clipped = samples.iter().any(|x| x.abs() >= 0.999);
            for x in &mut samples {
                *x = x.clamp(-1.0, 1.0);
            }
            let (timing, mut response) = if clipped {
                let timing = timing::inspect(&samples, 48000, 48000, args.sweep).unwrap();
                let response =
                    impulse::inspect(&samples, 48000, 48000, args.sweep, &timing).unwrap();
                (timing, response)
            } else {
                let timing = timing::analyze(&samples, 48000, 48000, args.sweep).unwrap();
                let response =
                    impulse::analyze(&samples, 48000, 48000, args.sweep, &timing).unwrap();
                (timing, response)
            };
            if self.bad_repeat.load(Ordering::Relaxed) && pass % 2 == 1 && id == "R" {
                response.relative_arrival_ms = response.relative_arrival_ms.map(|v| v + 2.0);
            }
            let directory = args.out_dir.join(id);
            std::fs::create_dir(&directory).unwrap();
            let rejection = self.rejected.lock().unwrap().get(id).cloned();
            let rejection = rejection.or_else(|| clipped.then(|| "clipped recording".into()));
            let diagnostics =
                clipped.then(|| diagnostics::analyze(&samples, 48000, 48000, args.sweep).unwrap());
            takes.push((
                id.clone(),
                TakeOutcome {
                    deferred: false,
                    directory,
                    accepted: rejection.is_none(),
                    cancelled: false,
                    rejection_reason: rejection,
                    diagnostics,
                    timing: Some(timing),
                    impulse: Some(Arc::new(response)),
                    impulse_error: None,
                },
            ));
        }
        Ok(BatchOutcome {
            directory: args.out_dir,
            takes,
            cancelled: false,
            error: None,
        })
    }
}
fn acquire(rig: &Rig, root: &std::path::Path) -> alignment::Alignment {
    let settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        output_root: root.display().to_string(),
        ..Settings::default()
    };
    let endpoint = "127.0.0.1:9000".parse().unwrap();
    let mut sweep = settings.sweep;
    sweep.duration_ms = 200;
    let settings = Settings { sweep, ..settings };
    let request = alignment::request(
        endpoint,
        rig.discover(endpoint).unwrap(),
        &settings,
        "L".into(),
        root.join("alignment"),
    )
    .unwrap();
    alignment::capture(
        rig,
        request,
        settings,
        vec!["L".into(), "R".into()],
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap()
    .alignment
}
#[test]
fn measured_pipeline_proposes_applies_verifies_reloads_and_restores_without_accumulation() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    assert!(a.blockers.is_empty(), "{:?}", a.blockers);
    assert!(a.profile.is_some(), "{:?}", a.rows);
    assert_eq!(a.pass_reports.as_ref().unwrap().len(), 2);
    let targets = a.targets(false).unwrap();
    assert!((targets[0].gain_db + 6.0206).abs() < 0.05);
    assert!((targets[0].delay_ms - 3.0).abs() < 0.05);
    let original = a.snapshot.clone();
    for case in 0..3 {
        let mut altered = a.clone();
        match case {
            0 => {
                altered.session.as_mut().unwrap().baseline.origin =
                    calibration_core::ObservationOrigin::Simulated
            }
            1 => {
                altered.profile.as_mut().unwrap().target_snapshot.speakers[0]
                    .settings
                    .gain_db += 1.0
            }
            _ => altered.rows[0].repeats[0].arrival_ms = Some(99.0),
        }
        assert!(alignment::apply(&rig, &mut altered, false).is_err());
    }
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    alignment::apply(&rig, &mut a, false).unwrap();
    alignment::apply(&rig, &mut a, false).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
    let restored = companion_service::workspace::restore(
        Some(companion_service::workspace::WorkspaceIndex {
            alignment: Some(a.directory.clone()),
            ..Default::default()
        }),
        &a.settings,
    )
    .unwrap();
    assert!(restored.warnings.is_empty());
    assert_eq!(
        restored.alignment.unwrap().applied_snapshot,
        a.applied_snapshot
    );
    assert_eq!(
        rig.writes.load(Ordering::Relaxed),
        1,
        "restore must not reapply settings"
    );
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(
        a.verification.as_ref().unwrap().passed,
        "{:?}",
        a.verification
    );
    alignment::verify(&rig, &mut a, &AtomicBool::new(true), &mut |_, _, _| {}).unwrap();
    assert!(a.verification.is_none());
    assert!(a.verification_note.is_some());
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(a.verification.as_ref().unwrap().passed);
    let mut loaded = alignment::load(&a.directory).unwrap();
    assert!(loaded.verification.as_ref().unwrap().passed);
    alignment::apply(&rig, &mut loaded, true).unwrap();
    assert_eq!(rig.snapshot.lock().unwrap().outputs, original.outputs);
    alignment::apply(&rig, &mut loaded, true).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 2);
    alignment::apply(&rig, &mut loaded, false).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 3);
    // Someone edits another output in the renderer: restoration refuses to overwrite it.
    {
        let mut live = rig.snapshot.lock().unwrap();
        live.revision = "external-change".into();
        live.outputs[2].gain_db = -4.0;
    }
    assert!(alignment::apply(&rig, &mut loaded, true).is_err());
    assert_eq!(rig.writes.load(Ordering::Relaxed), 3);
}

#[test]
fn failed_input_records_first_pass_failure_without_starting_second_pass_or_proposing() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    rig.silent_batch.store(true, Ordering::Relaxed);
    let a = acquire(&rig, root.path());
    assert_eq!(rig.captures.load(Ordering::Relaxed), 1);
    assert!(a.profile.is_none());
    assert!(a.applied_snapshot.is_none());
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    let loaded = alignment::load(&a.directory).unwrap();
    let reports = loaded.pass_reports.unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].analyzed_speakers, 0);
    assert_eq!(
        reports[0].error.as_deref(),
        Some(measurement_capture::input::NO_SIGNAL)
    );
    assert_eq!(a.blockers, [measurement_capture::input::NO_SIGNAL]);
    // Older saved sessions remain readable and do not invent pass history.
    let file = a.directory.join("alignment-state.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("pass_reports");
    json.as_object_mut()
        .unwrap()
        .remove("verification_pass_reports");
    std::fs::write(file, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(
        alignment::load(&a.directory)
            .unwrap()
            .pass_reports
            .is_none()
    );
}
#[test]
fn uncertain_mutation_is_persisted_and_never_retried() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    rig.lose_ack.store(true, Ordering::Relaxed);
    assert!(alignment::apply(&rig, &mut a, false).is_err());
    assert!(a.application_uncertain);
    let mut loaded = alignment::load(&a.directory).unwrap();
    assert!(loaded.application_uncertain);
    let restored = companion_service::workspace::restore(
        Some(companion_service::workspace::WorkspaceIndex {
            alignment: Some(a.directory.clone()),
            ..Default::default()
        }),
        &a.settings,
    )
    .unwrap();
    assert!(restored.alignment.unwrap().application_uncertain);
    assert!(alignment::apply(&rig, &mut loaded, false).is_err());
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
}
#[test]
fn unstable_target_retains_level_evidence_but_cannot_make_or_apply_a_profile() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    rig.bad_repeat.store(true, Ordering::Relaxed);
    let mut a = acquire(&rig, root.path());
    assert!(a.profile.is_none());
    assert!(
        a.rows[1]
            .reason
            .as_ref()
            .unwrap()
            .contains("between repeats")
    );
    assert!(a.rows[1].repeats[0].level_db.is_some());
    assert!(alignment::apply(&rig, &mut a, false).is_err());
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
}

#[test]
fn stable_projector_latency_is_set_aside_until_included_then_aligns_within_capacity() {
    let root = tempfile::tempdir().unwrap();
    let mut rig = Rig::new();
    rig.slow_output = true;
    rig.snapshot.lock().unwrap().max_delay_ms = 250.0;
    let a = acquire(&rig, root.path());
    // R is repeatable but about 160 ms late: set aside by default, which leaves only L.
    assert!(a.rows.iter().all(|r| r.qualified.is_some()));
    assert!(a.rows[1].set_aside() && !a.rows[1].included());
    assert!(
        matches!(a.rows[1].exclusion(), Some(ExclusionReason::FarArrival { ms }) if (ms - 160.0).abs() < 1.0),
        "{:?}",
        a.rows[1].reason_code
    );
    assert!(
        a.rows[1].reason.is_none(),
        "set-aside speakers keep no failure text"
    );
    assert!(a.profile.is_none());
    assert_eq!(
        a.blocker_list(),
        [Blocker::TooFewQualified {
            count: 1,
            set_aside: 1
        }]
    );
    assert!(
        calibration_core::long_delay_warning(a.rows[1].qualified.as_ref().unwrap().arrival_ms)
            .is_some()
    );
    assert!(alignment::include_anyway(&a, "Unknown", true).is_err());
    // Including it is saved and builds the proposal from the saved passes.
    let mut a = alignment::include_anyway(&a, "R", true).unwrap();
    assert!(a.rows[1].include_anyway && a.rows[1].included());
    assert!(a.blockers.is_empty(), "{:?}", a.blockers);
    assert_eq!(a.profile.as_ref().unwrap().policy.maximum_delay_ms, 250.0);
    assert!((a.targets(false).unwrap()[0].delay_ms - 160.0).abs() < 0.05);
    assert!(alignment::load(&a.directory).unwrap().rows[1].include_anyway);
    alignment::apply(&rig, &mut a, false).unwrap();
    assert!(
        alignment::include_anyway(&a, "R", false).is_err(),
        "applied proposals stay fixed"
    );
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(
        a.verification.as_ref().unwrap().passed,
        "{:?}",
        a.verification_note
    );
    // A session applied by the former policy has no include flag; it stays valid and
    // restorable because its saved profile already corrects the far speaker.
    let file = a.directory.join("alignment-state.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    for row in json["rows"].as_array_mut().unwrap() {
        let row = row.as_object_mut().unwrap();
        row.remove("include_anyway");
        row.remove("reason_code");
    }
    std::fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
    let mut old = alignment::load(&a.directory).unwrap();
    assert!(old.rows[1].include_anyway);
    alignment::apply(&rig, &mut old, true).unwrap();
    assert!(omniphony_adapter::same_configuration(
        &rig.snapshot.lock().unwrap(),
        &old.snapshot
    ));

    let old_root = tempfile::tempdir().unwrap();
    rig.snapshot.lock().unwrap().max_delay_ms = 100.0;
    let a = acquire(&rig, old_root.path());
    assert!(
        a.rows.iter().all(|r| r.qualified.is_some()),
        "long latency remains valid evidence"
    );
    let a = alignment::include_anyway(&a, "R", true).unwrap();
    assert!(
        a.profile.is_none(),
        "cannot exceed the older renderer's real capacity"
    );
    assert!(matches!(a.blocker_list()[..], [Blocker::Proposal { .. }]));
    // Setting it aside again recomputes the derived blocker instead of keeping it.
    let a = alignment::include_anyway(&a, "R", false).unwrap();
    assert_eq!(
        a.blocker_list(),
        [Blocker::TooFewQualified {
            count: 1,
            set_aside: 1
        }]
    );
}

#[test]
fn reason_codes_round_trip_and_text_only_sessions_still_load() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut snapshot = rig.snapshot.lock().unwrap();
    let mut fourth = snapshot.outputs[2].clone();
    fourth.id = "Clipped".into();
    fourth.name = fourth.id.clone();
    fourth.index = 3;
    snapshot.outputs.push(fourth);
    drop(snapshot);
    rig.rejected.lock().unwrap().insert(
        "Unused".into(),
        "integrated level-band signal/noise is 18.5 dB (minimum 20 dB for correction)".into(),
    );
    rig.rejected
        .lock()
        .unwrap()
        .insert("Clipped".into(), "clipped recording".into());
    let settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        sweep: omniphony_adapter::Sweep {
            duration_ms: 200,
            ..Settings::default().sweep
        },
        ..Settings::default()
    };
    let request = alignment::request(
        "127.0.0.1:9000".parse().unwrap(),
        rig.discover("127.0.0.1:9000".parse().unwrap()).unwrap(),
        &settings,
        "L".into(),
        root.path().join("codes"),
    )
    .unwrap();
    let a = alignment::capture(
        &rig,
        request,
        settings,
        vec!["L".into(), "R".into(), "Unused".into(), "Clipped".into()],
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap()
    .alignment;
    let low = ExclusionReason::LowSnr {
        band: SnrBand::Level,
        db: Some(18.5),
        interval: None,
    };
    let codes = |a: &alignment::Alignment| a.rows.iter().map(|r| r.exclusion()).collect::<Vec<_>>();
    let expected = vec![
        None,
        None,
        Some(low.clone()),
        Some(ExclusionReason::Clipped { overdrive_db: None }),
    ];
    assert_eq!(codes(&a), expected);
    assert_eq!(a.rows[2].reason.as_deref(), Some(low.to_string().as_str()));
    assert_eq!(a.rows[2].repeats[0].codes, std::slice::from_ref(&low));
    assert_eq!(
        a.rows[3].exclusion().unwrap().human(),
        "Clipped. Lower the mic gain or test level."
    );
    assert!(a.profile.is_some());
    // Codes survive the saved file exactly.
    let loaded = alignment::load(&a.directory).unwrap();
    assert_eq!(codes(&loaded), expected);
    assert_eq!(loaded.rows[2].reason_code, Some(low.clone()));
    // A session saved before reason codes carries text only: it loads, keeps its
    // proposal and reads the same codes back from the text.
    let file = a.directory.join("alignment-state.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("blocker_codes");
    for row in json["rows"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("reason_code");
        for repeat in row["repeats"].as_array_mut().unwrap() {
            repeat.as_object_mut().unwrap().remove("codes");
        }
    }
    let text = serde_json::to_string(&json).unwrap();
    assert!(!text.contains("reason_code") && !text.contains("\"codes\""));
    std::fs::write(&file, text).unwrap();
    let old = alignment::load(&a.directory).unwrap();
    assert!(old.rows.iter().all(|r| r.reason_code.is_none()));
    assert_eq!(codes(&old), expected);
    assert_eq!(old.targets(false).unwrap(), a.targets(false).unwrap());
    // Updating the old proposal writes the codes alongside the text.
    let updated = alignment::refresh_proposal(&old).unwrap();
    assert_eq!(
        updated.rows[3].reason_code,
        Some(ExclusionReason::Clipped { overdrive_db: None })
    );
    assert_eq!(updated.corrected_output_ids(), ["L", "R"]);
    // Old text blockers are kept as they were and still block an update.
    let mut blocked = old.clone();
    blocked.profile = None;
    blocked.session = None;
    blocked.blockers.push("Alignment cancelled.".into());
    assert_eq!(
        blocked.blocker_list(),
        [Blocker::Other {
            detail: "Alignment cancelled.".into()
        }]
    );
    assert!(alignment::refresh_proposal(&blocked).is_err());
}

/// The rig with a fourth speaker, "Loud", whose take peaks `over_db` above full scale at
/// its own gain: the rig's target is 0.25 times the -30 dBFS sweep.
fn loud_rig(over_db: f64) -> (Rig, f64) {
    let rig = Rig::new();
    let mut snapshot = rig.snapshot.lock().unwrap();
    let mut loud = snapshot.outputs[2].clone();
    loud.id = "Loud".into();
    loud.name = loud.id.clone();
    loud.index = 3;
    snapshot.outputs.push(loud);
    drop(snapshot);
    let factor = 10_f64.powf((over_db + 30.0) / 20.0) / 0.25;
    rig.loud
        .lock()
        .unwrap()
        .insert("Loud".into(), factor as f32);
    (rig, 20.0 * factor.log10())
}
fn capture_loud(rig: &Rig, root: &std::path::Path, retake_clipped: bool) -> alignment::Outcome {
    let settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        sweep: omniphony_adapter::Sweep {
            duration_ms: 200,
            ..Settings::default().sweep
        },
        retake_clipped,
        ..Settings::default()
    };
    let request = alignment::request(
        "127.0.0.1:9000".parse().unwrap(),
        rig.discover("127.0.0.1:9000".parse().unwrap()).unwrap(),
        &settings,
        "L".into(),
        root.join("loud"),
    )
    .unwrap();
    alignment::capture(
        rig,
        request,
        settings,
        vec!["L".into(), "R".into(), "Loud".into()],
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap()
}

#[test]
fn a_clipped_speaker_says_how_much_to_lower_the_gain() {
    let root = tempfile::tempdir().unwrap();
    let (rig, _) = loud_rig(9.3);
    let a = capture_loud(&rig, root.path(), false).alignment;
    let Some(ExclusionReason::Clipped {
        overdrive_db: Some(db),
    }) = a.rows[2].exclusion()
    else {
        panic!("{:?}", a.rows[2].exclusion());
    };
    assert!((db - 9.3).abs() < 0.1, "{db}");
    assert_eq!(
        a.rows[2].exclusion().unwrap().human(),
        "Clipped by about 9 dB. Lower the mic gain or test level by 13 dB."
    );
    assert!(
        a.rows[2]
            .reason
            .as_ref()
            .unwrap()
            .starts_with("clipped recording, 9.")
    );
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    assert!(a.restored_snapshot.is_none());
    let loaded = alignment::load(&a.directory).unwrap();
    assert_eq!(loaded.rows[2].exclusion(), a.rows[2].exclusion());
}

#[test]
fn a_clipped_speaker_is_measured_again_at_a_lower_gain_and_counted_back() {
    assert!(Settings::default().retake_clipped);
    let root = tempfile::tempdir().unwrap();
    let (rig, louder_db) = loud_rig(9.3);
    let original = rig.snapshot.lock().unwrap().clone();
    let outcome = capture_loud(&rig, root.path(), true);
    let mut a = outcome.alignment;
    // Each pass measured Loud again 13 dB lower (9.3 dB over plus 3 dB, rounded up).
    assert_eq!(
        *rig.batches.lock().unwrap(),
        [
            vec!["L", "R", "Loud"],
            vec!["Loud"],
            vec!["L", "R", "Loud"],
            vec!["Loud"]
        ]
    );
    let loud = &a.rows[2];
    assert!(loud.qualified.is_some(), "{:?}", loud.exclusion());
    assert!(loud.repeats.iter().all(|r| r.output_gain_offset_db == 13.0));
    assert!(
        loud.take_folders
            .iter()
            .all(|f| f.parent().unwrap().ends_with("pass-1-retake")
                || f.parent().unwrap().ends_with("pass-2-retake"))
    );
    assert_eq!(outcome.retakes.len(), 2);
    assert!(outcome.retakes.iter().all(|r| r.gain_offset_db == 13.0));
    // The level counts the lowered gain back: Loud reads as loud as it is.
    let level = |i: usize| a.rows[i].qualified.as_ref().unwrap().level_db;
    assert!(
        (level(2) - level(1) - louder_db).abs() < 0.1,
        "{} vs {louder_db}",
        level(2) - level(1)
    );
    // The gains are back; the alignment follows the renderer's newer revision.
    let live = rig.snapshot.lock().unwrap().clone();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 4);
    assert_ne!(live.revision, original.revision);
    assert!(omniphony_adapter::same_configuration(&live, &original));
    assert_eq!(a.restored_snapshot.as_ref(), Some(&live));
    assert_eq!(a.snapshot, original);
    // Lowered and restored states of both passes, for reanalysis of these takes.
    assert_eq!(a.retake_snapshots.len(), 4);
    assert_eq!(a.retake_snapshots[3], live);
    // The proposal corrects Loud from its own gain, and applies to the live renderer.
    assert_eq!(a.corrected_output_ids(), ["L", "R", "Loud"]);
    let targets = a.targets(false).unwrap();
    let gain = |id: &str| targets.iter().find(|t| t.output_id == id).unwrap().gain_db as f64;
    assert!((gain("Loud") - gain("R") + louder_db).abs() < 0.1);
    alignment::apply(&rig, &mut a, false).unwrap();
    let applied = rig.snapshot.lock().unwrap().clone();
    let loud_gain = applied
        .outputs
        .iter()
        .find(|o| o.id == "Loud")
        .unwrap()
        .gain_db as f64;
    assert!((loud_gain - gain("Loud")).abs() < 0.001);
    let loaded = alignment::load(&a.directory).unwrap();
    assert_eq!(loaded.rows[2].repeats, a.rows[2].repeats);
}

/// L and R with the default settings, keeping the retakes.
fn capture_pair(rig: &Rig, root: &std::path::Path) -> alignment::Outcome {
    let mut settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        output_root: root.display().to_string(),
        ..Settings::default()
    };
    settings.sweep.duration_ms = 200;
    let request = alignment::request(
        "127.0.0.1:9000".parse().unwrap(),
        rig.discover("127.0.0.1:9000".parse().unwrap()).unwrap(),
        &settings,
        "L".into(),
        root.join("pair"),
    )
    .unwrap();
    alignment::capture(
        rig,
        request,
        settings,
        vec!["L".into(), "R".into()],
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap()
}

#[test]
fn a_disturbed_reference_sweep_is_measured_again_at_once() {
    // Noise in the room during the reference sweep after R, in pass 1 only: R's own
    // sweep is clean and L's take shows the reference passes.
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let original = rig.snapshot.lock().unwrap().clone();
    rig.disturb.lock().unwrap().insert("R".into(), 0.008);
    let outcome = capture_pair(&rig, root.path());
    let a = &outcome.alignment;
    assert_eq!(
        *rig.batches.lock().unwrap(),
        [vec!["L", "R"], vec!["R"], vec!["L", "R"]]
    );
    let r = &a.rows[1];
    assert!(r.qualified.is_some(), "{:?}", r.exclusion());
    assert!(
        r.take_folders[0]
            .parent()
            .unwrap()
            .ends_with("pass-1-retake")
    );
    assert_eq!(r.repeats[0].output_gain_offset_db, 0.0);
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    // Measured again at its own gain: the renderer was never touched.
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    assert!(a.restored_snapshot.is_none() && a.retake_snapshots.is_empty());
    assert_eq!(*rig.snapshot.lock().unwrap(), original);
    assert_eq!(outcome.retakes.len(), 1);
    assert_eq!(outcome.retakes[0].revision, original.revision);
    assert_eq!(outcome.retakes[0].gain_offset_db, 0.0);
}

#[test]
fn a_reference_disturbed_on_every_take_is_named_and_not_measured_again() {
    // With no take of the pass to show the reference passes, a retake would not help.
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    for id in ["L", "R"] {
        rig.disturb.lock().unwrap().insert(id.into(), 0.008);
    }
    let a = capture_pair(&rig, root.path()).alignment;
    assert_eq!(rig.batches.lock().unwrap().len(), 2);
    let reason = a.rows[1].exclusion().unwrap();
    assert!(
        matches!(
            reason,
            ExclusionReason::LowSnr {
                band: SnrBand::Level,
                interval: Some(calibration_core::reason::Interval::ReferenceAfter),
                ..
            }
        ),
        "{reason:?}"
    );
    assert_eq!(
        reason.human(),
        "Reference sweep after it was too noisy. Measure again."
    );
    assert!(
        a.rows[1]
            .reason
            .as_deref()
            .unwrap()
            .contains("in the reference sweep after the speaker")
    );
}

#[test]
fn a_clipping_reference_speaker_is_not_measured_again() {
    // Lowering the reference would change every take; only the test level can help.
    let root = tempfile::tempdir().unwrap();
    let (rig, _) = loud_rig(9.3);
    rig.loud.lock().unwrap().insert("L".into(), 400.0);
    let a = capture_loud(&rig, root.path(), true).alignment;
    assert_eq!(rig.batches.lock().unwrap().len(), 2);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    assert!(a.rows.iter().all(|r| r.qualified.is_none()));
}

#[test]
fn the_16_october_session_sets_aside_the_late_device_and_keeps_its_text_reasons() {
    // Saved before reason codes and set-aside existed (device and instance IDs replaced).
    let root = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/alignment-2026-10-16.json"
        ),
        root.path().join("alignment-state.json"),
    )
    .unwrap();
    let saved = alignment::load(root.path()).unwrap();
    let by_id = |a: &alignment::Alignment, id: &str| {
        a.rows.iter().find(|r| r.output_id == id).unwrap().clone()
    };
    for (id, code, human) in [
        (
            "BL",
            ExclusionReason::MissingTimingReferences,
            "No sound picked up. Wired and unmuted?",
        ),
        (
            "TBL",
            ExclusionReason::Clipped { overdrive_db: None },
            "Clipped. Lower the mic gain or test level.",
        ),
        (
            "TBR",
            ExclusionReason::LowSnr {
                band: SnrBand::Acquisition,
                db: None,
                interval: None,
            },
            "Too noisy to trust.",
        ),
    ] {
        let reason = by_id(&saved, id).exclusion().unwrap();
        assert_eq!((&reason, reason.human().as_str()), (&code, human), "{id}");
    }
    let a = alignment::refresh_proposal(&saved).unwrap();
    assert!(a.blockers.is_empty(), "{:?}", a.blockers);
    assert_eq!(a.corrected_output_ids(), ["FL", "FR", "C", "BC"]);
    for id in ["spk-12", "spk-13"] {
        let row = by_id(&a, id);
        assert!(row.set_aside(), "{id}: {:?}", row.reason_code);
        let Some(ExclusionReason::FarArrival { ms }) = row.reason_code else {
            unreachable!()
        };
        assert!((176.7..177.0).contains(&ms), "{id}: {ms}");
        assert_eq!(
            row.exclusion().unwrap().human(),
            "Set aside: 177 ms later than the others."
        );
    }
    // The four others align among themselves: nobody is delayed by 177 ms.
    let targets = a.targets(false).unwrap();
    assert!(targets.iter().all(|t| t.delay_ms < 7.0), "{targets:?}");
    let bc = targets.iter().find(|t| t.output_id == "BC").unwrap();
    assert!((bc.delay_ms - 6.31).abs() < 0.01, "{bc:?}");
    // Aligning to the late device is an explicit, saved choice.
    let a = alignment::include_anyway(&a, "spk-12", true).unwrap();
    assert_eq!(a.corrected_output_ids(), ["FL", "FR", "C", "BC", "spk-12"]);
    assert!(by_id(&a, "spk-13").set_aside());
    let fl = a.targets(false).unwrap()[0].clone();
    assert!((fl.delay_ms - 176.9).abs() < 0.05, "{fl:?}");
    let reloaded = alignment::load(root.path()).unwrap();
    assert!(by_id(&reloaded, "spk-12").include_anyway);
    assert_eq!(reloaded.targets(false).unwrap(), a.targets(false).unwrap());
}

#[test]
fn physical_level_adjustment_persists_staleness_but_keeps_digital_restoration() {
    let rig = Rig::new();
    let root = tempfile::tempdir().unwrap();
    let mut a = acquire(&rig, root.path());
    assert!(a.profile.is_some());
    alignment::apply(&rig, &mut a, false).unwrap();
    let original = a.snapshot.clone();
    alignment::invalidate_physical_levels(&mut a).unwrap();
    let mut a = alignment::load(&a.directory).unwrap();
    assert!(a.physical_levels_changed);
    assert!(
        alignment::apply(&rig, &mut a, false)
            .unwrap_err()
            .contains("Physical")
    );
    assert!(
        alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {})
            .unwrap_err()
            .contains("Physical")
    );
    alignment::apply(&rig, &mut a, true).unwrap();
    assert!(omniphony_adapter::same_configuration(
        &rig.snapshot.lock().unwrap(),
        &original
    ));
    assert!(
        alignment::load(&a.directory)
            .unwrap()
            .physical_levels_changed
    );
}

#[test]
fn partial_proposal_preserves_excluded_outputs_through_apply_verify_reload_and_restore() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut snapshot = rig.snapshot.lock().unwrap();
    let mut fourth = snapshot.outputs[2].clone();
    fourth.id = "Clipped".into();
    fourth.name = fourth.id.clone();
    fourth.index = 3;
    snapshot.outputs.push(fourth);
    snapshot.outputs[2].gain_db = -7.0;
    snapshot.outputs[2].delay_ms = 12.0;
    snapshot.outputs[3].gain_db = -4.0;
    snapshot.outputs[3].delay_ms = 21.0;
    let original = snapshot.clone();
    drop(snapshot);
    rig.rejected.lock().unwrap().insert(
        "Unused".into(),
        "integrated level-band signal/noise is 18.5 dB (minimum 20 dB for correction)".into(),
    );
    rig.rejected
        .lock()
        .unwrap()
        .insert("Clipped".into(), "clipped recording".into());
    let settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        sweep: omniphony_adapter::Sweep {
            duration_ms: 200,
            ..Settings::default().sweep
        },
        ..Settings::default()
    };
    let request = alignment::request(
        "127.0.0.1:9000".parse().unwrap(),
        original.clone(),
        &settings,
        "L".into(),
        root.path().join("partial"),
    )
    .unwrap();
    let mut a = alignment::capture(
        &rig,
        request,
        settings,
        vec!["L".into(), "R".into(), "Unused".into(), "Clipped".into()],
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap()
    .alignment;
    assert!(a.profile.is_some(), "{:?}", a.blockers);
    assert!(a.blockers.is_empty());
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    assert_eq!(a.rows.len(), 4);
    assert!(a.rows[2].reason.as_ref().unwrap().contains("18.5"));
    assert!(a.rows[3].reason.as_ref().unwrap().contains("clipped"));
    assert_eq!(a.targets(false).unwrap().len(), 2);
    let plan = std::fs::read(a.directory.join("alignment-plan.json")).unwrap();
    // Upgrade a complete saved session made by the former all-or-nothing policy.
    a.session = None;
    a.profile = None;
    alignment::save(&a).unwrap();
    a = alignment::refresh_proposal(&alignment::load(&a.directory).unwrap()).unwrap();
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    assert_eq!(
        std::fs::read(a.directory.join("alignment-plan.json")).unwrap(),
        plan
    );
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    alignment::apply(&rig, &mut a, false).unwrap();
    assert_eq!(
        rig.snapshot.lock().unwrap().outputs[2..],
        original.outputs[2..]
    );
    assert!(alignment::refresh_proposal(&a).is_err());
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(
        a.verification.as_ref().unwrap().passed,
        "{:?}",
        a.verification_note
    );
    assert_eq!(*rig.batches.lock().unwrap().last().unwrap(), ["L", "R"]);
    // If one corrected speaker fails verification, do not verify a smaller set.
    rig.rejected
        .lock()
        .unwrap()
        .insert("R".into(), "clipped recording".into());
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(a.verification.is_none());
    assert!(
        a.verification_note
            .as_ref()
            .unwrap()
            .contains("R: clipped recording")
    );
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    let mut a = alignment::load(&a.directory).unwrap();
    // Neither forging an exclusion nor editing the proposed membership is accepted.
    let mut forged = a.clone();
    forged.rows[0].qualified = None;
    forged.rows[0].reason = Some("excluded".into());
    assert!(forged.targets(false).is_err());
    let mut forged = a.clone();
    forged
        .profile
        .as_mut()
        .unwrap()
        .target_snapshot
        .speakers
        .push(a.profile.as_ref().unwrap().target_snapshot.speakers[0].clone());
    assert!(forged.targets(false).is_err());
    alignment::apply(&rig, &mut a, true).unwrap();
    assert_eq!(rig.snapshot.lock().unwrap().outputs, original.outputs);
    let mut diagnostic = a.clone();
    diagnostic.profile = None;
    diagnostic.session = None;
    diagnostic
        .blockers
        .push("Offline diagnostic reanalysis".into());
    assert!(alignment::refresh_proposal(&diagnostic).is_err());
    diagnostic.blockers.clear();
    diagnostic.physical_levels_changed = true;
    assert!(alignment::refresh_proposal(&diagnostic).is_err());
}

#[test]
fn one_speaker_is_measured_again_into_the_session_and_the_proposal_rebuilt() {
    // R repeats badly in the first alignment, so the session has no proposal. Measured
    // again on its own, with the room quiet, it qualifies and the proposal appears.
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    rig.bad_repeat.store(true, Ordering::Relaxed);
    let mut a = acquire(&rig, root.path());
    assert!(a.profile.is_none());
    assert!(a.rows[1].qualified.is_none());
    let left = a.rows[0].clone();
    let old_right = a.rows[1].take_folders.clone();
    rig.bad_repeat.store(false, Ordering::Relaxed);
    let settings = a.settings.clone();
    let mut passes = vec![];
    let outcome = alignment::remeasure(
        &rig,
        &mut a,
        "R",
        &settings,
        &AtomicBool::new(false),
        &mut |pass, row, _| passes.push((pass, row)),
    )
    .unwrap();
    assert_eq!(
        *rig.batches.lock().unwrap(),
        [vec!["L", "R"], vec!["L", "R"], vec!["R"], vec!["R"]]
    );
    assert!(
        passes.contains(&(0, 1)) && passes.contains(&(1, 1)),
        "{passes:?}"
    );
    // R has two fresh takes, one per pass; L keeps its own.
    let right = &a.rows[1];
    assert!(right.qualified.is_some(), "{:?}", right.exclusion());
    assert_eq!(right.take_folders.len(), 2);
    assert_ne!(right.take_folders, old_right);
    for (pass, folder) in right.take_folders.iter().enumerate() {
        let parent = folder
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            parent.starts_with(&format!("pass-{}-again-", pass + 1)),
            "{parent}"
        );
        assert!(folder.join("snapshot.json").exists() || folder.exists());
    }
    assert_eq!(a.rows[0].take_folders, left.take_folders);
    assert_eq!(a.rows[0].repeats, left.repeats);
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    assert!(
        a.profile.is_some() && a.blockers.is_empty(),
        "{:?}",
        a.blockers
    );
    assert_eq!(outcome.takes.len(), 2);
    assert!(outcome.takes.iter().all(|(id, _)| id == "R"));
    assert!(outcome.retakes.is_empty());
    assert_eq!(outcome.alignment.rows[1].take_folders, right.take_folders);
    // Measured at its own gain: the renderer was never touched.
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
    // The session on disk is the updated one, and still applies.
    let loaded = alignment::load(&a.directory).unwrap();
    assert_eq!(loaded.rows[1].take_folders, right.take_folders);
    assert_eq!(loaded.rows[1].repeats, right.repeats);
    assert_eq!(loaded.corrected_output_ids(), ["L", "R"]);
    alignment::apply(&rig, &mut a, false).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn measuring_again_refuses_other_settings_an_applied_session_and_a_changed_renderer() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    let before = a.clone();
    let settings = a.settings.clone();
    let no_cancel = AtomicBool::new(false);
    let mut quiet = |_, _, _| {};
    let unknown = alignment::remeasure(&rig, &mut a, "Unused", &settings, &no_cancel, &mut quiet);
    assert_eq!(
        unknown.unwrap_err(),
        "This speaker is not part of the alignment."
    );
    let other = Settings {
        position_id: "another-seat".into(),
        ..settings.clone()
    };
    let changed = alignment::remeasure(&rig, &mut a, "R", &other, &no_cancel, &mut quiet);
    assert!(
        changed.as_ref().unwrap_err().contains("settings"),
        "{changed:?}"
    );
    // The renderer's settings moved on since the session: refused before any playback.
    let live = rig.snapshot.lock().unwrap().clone();
    {
        let mut edited = rig.snapshot.lock().unwrap();
        edited.revision = "rig:edited".into();
        edited.outputs[2].gain_db = -4.0;
    }
    let moved = alignment::remeasure(&rig, &mut a, "R", &settings, &no_cancel, &mut quiet);
    assert!(moved.as_ref().unwrap_err().contains("changed"), "{moved:?}");
    *rig.snapshot.lock().unwrap() = live;
    assert_eq!(rig.batches.lock().unwrap().len(), 2);
    assert_eq!(rows_json(&a), rows_json(&before));
    // An applied session keeps its evidence for restoration.
    alignment::apply(&rig, &mut a, false).unwrap();
    let applied = alignment::remeasure(&rig, &mut a, "R", &settings, &no_cancel, &mut quiet);
    assert!(
        applied.as_ref().unwrap_err().contains("Restore"),
        "{applied:?}"
    );
    assert_eq!(rig.batches.lock().unwrap().len(), 2);
    // Undone, the renderer is back in the original configuration under a newer
    // revision: that is the state the session expects now.
    alignment::apply(&rig, &mut a, true).unwrap();
    assert!(a.restored_snapshot.is_some());
    alignment::remeasure(&rig, &mut a, "R", &settings, &no_cancel, &mut quiet).unwrap();
    assert_eq!(rig.batches.lock().unwrap().len(), 4);
    assert_ne!(a.rows[1].take_folders, before.rows[1].take_folders);
    assert_eq!(a.rows[0].take_folders, before.rows[0].take_folders);
    assert_eq!(a.corrected_output_ids(), ["L", "R"]);
    assert_eq!(rig.writes.load(Ordering::Relaxed), 2);
}

#[test]
fn a_stopped_re_measure_keeps_the_previous_takes() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    let before = a.clone();
    let settings = a.settings.clone();
    // Stopped before it starts: nothing is played.
    let stopped = alignment::remeasure(
        &rig,
        &mut a,
        "R",
        &settings,
        &AtomicBool::new(true),
        &mut |_, _, _| {},
    );
    assert!(
        stopped.as_ref().unwrap_err().contains("stopped"),
        "{stopped:?}"
    );
    assert_eq!(rig.batches.lock().unwrap().len(), 2);
    // Stopped after the first pass: that take is not kept either.
    let cancel = AtomicBool::new(false);
    let stopped = alignment::remeasure(&rig, &mut a, "R", &settings, &cancel, &mut |pass, _, _| {
        if pass == 0 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(
        stopped.as_ref().unwrap_err().contains("stopped"),
        "{stopped:?}"
    );
    assert_eq!(rig.batches.lock().unwrap().len(), 3);
    assert_eq!(rows_json(&a), rows_json(&before));
    assert_eq!(
        serde_json::to_value(&a.profile).unwrap(),
        serde_json::to_value(&before.profile).unwrap()
    );
    let loaded = alignment::load(&a.directory).unwrap();
    assert_eq!(rows_json(&loaded), rows_json(&before));
    assert_eq!(rig.writes.load(Ordering::Relaxed), 0);
}

/// The rows as saved, for comparing whole sessions.
fn rows_json(a: &alignment::Alignment) -> serde_json::Value {
    serde_json::to_value(&a.rows).unwrap()
}

/// The renderer restarts, as it does to change port: a new process identity and
/// revision, with `gains` and `delays` for L and R (the same settings when `None`).
fn restart(rig: &Rig, name: &str, settings: Option<[(f32, f32); 2]>) -> Snapshot {
    let mut live = rig.snapshot.lock().unwrap();
    live.instance_id = name.into();
    live.revision = format!("{name}:0");
    if let Some(settings) = settings {
        for (output, (gain_db, delay_ms)) in live.outputs.iter_mut().zip(settings) {
            output.gain_db = gain_db;
            output.delay_ms = delay_ms;
        }
    }
    live.clone()
}

#[test]
fn a_new_port_or_a_restart_keeps_the_session_usable() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    let original = a.snapshot.clone();
    // Restarted on another port with the same settings: still the measured installation.
    let live = restart(&rig, "rig-on-9007", None);
    a.endpoint = "127.0.0.1:9007".parse().unwrap();
    assert_eq!(a.live_state(&live), alignment::LiveState::Original);
    assert!(alignment::reconcile(&mut a, &live).unwrap());
    assert_eq!(a.restored_snapshot.as_ref(), Some(&live));
    assert_eq!(a.snapshot, original, "the measured baseline is kept");
    // A speaker is measured again into it, and the proposal applies, on the new port.
    let settings = a.settings.clone();
    alignment::remeasure(
        &rig,
        &mut a,
        "R",
        &settings,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap();
    alignment::apply(&rig, &mut a, false).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
    let new_port: SocketAddr = "127.0.0.1:9007".parse().unwrap();
    let endpoints = rig.endpoints.lock().unwrap().clone();
    assert_eq!(endpoints[endpoints.len() - 3..], [new_port; 3]);
    let corrected = rig.snapshot.lock().unwrap().clone();
    let corrected = [0, 1].map(|i| (corrected.outputs[i].gain_db, corrected.outputs[i].delay_ms));
    // Restarted again with the corrected settings saved: applied, and Undo restores.
    let live = restart(&rig, "rig-on-9010", Some(corrected));
    a.endpoint = "127.0.0.1:9010".parse().unwrap();
    a.applied_snapshot = None;
    assert_eq!(a.live_state(&live), alignment::LiveState::Applied);
    assert!(alignment::reconcile(&mut a, &live).unwrap());
    assert_eq!(a.applied_snapshot.as_ref(), Some(&live));
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(
        a.verification.as_ref().unwrap().passed,
        "{:?}",
        a.verification
    );
    alignment::apply(&rig, &mut a, true).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 2);
    assert_eq!(rig.snapshot.lock().unwrap().outputs, original.outputs);
    assert_eq!(
        *rig.endpoints.lock().unwrap().last().unwrap(),
        "127.0.0.1:9010".parse::<SocketAddr>().unwrap()
    );
    // What was saved is what the next start reads.
    let loaded = alignment::load(&a.directory).unwrap();
    assert!(loaded.applied_snapshot.is_none());
    assert_eq!(loaded.endpoint, a.endpoint);
}

#[test]
fn an_alignment_applied_before_a_restart_to_the_original_settings_is_no_longer_applied() {
    // The case of 19 October: applied on one renderer, which then stopped; another
    // renderer, on another port, has the original settings.
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    alignment::apply(&rig, &mut a, false).unwrap();
    alignment::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert!(a.verification.is_some());
    let live = restart(&rig, "other-workflow", Some([(0.0, 0.0); 2]));
    assert_eq!(a.live_state(&live), alignment::LiveState::Original);
    // Undo has nothing to do, and writes nothing.
    let mut undone = a.clone();
    alignment::apply(&rig, &mut undone, true).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
    assert!(undone.applied_snapshot.is_none());
    // Connecting records it: not applied, nothing verified, and it can be applied again.
    assert!(alignment::reconcile(&mut a, &live).unwrap());
    assert!(a.applied_snapshot.is_none());
    assert!(a.verification.is_none() && a.verification_note.is_none());
    assert!(
        !alignment::reconcile(&mut a, &live).unwrap(),
        "nothing left to record"
    );
    assert!(
        alignment::load(&a.directory)
            .unwrap()
            .applied_snapshot
            .is_none()
    );
    alignment::apply(&rig, &mut a, false).unwrap();
    assert_eq!(rig.writes.load(Ordering::Relaxed), 2);
}

#[test]
fn a_renderer_with_other_settings_is_neither_applied_nor_undone() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = acquire(&rig, root.path());
    alignment::apply(&rig, &mut a, false).unwrap();
    let before = a.applied_snapshot.clone();
    let live = restart(&rig, "elsewhere", Some([(-2.0, 0.0), (0.0, 1.0)]));
    assert_eq!(a.live_state(&live), alignment::LiveState::Other);
    assert!(!alignment::reconcile(&mut a, &live).unwrap());
    assert_eq!(
        a.applied_snapshot, before,
        "the saved restoration context stays"
    );
    for restore in [true, false] {
        let refused = alignment::apply(&rig, &mut a, restore);
        assert!(
            refused.as_ref().unwrap_err().contains("differ from both"),
            "{refused:?}"
        );
    }
    let settings = a.settings.clone();
    assert!(
        alignment::remeasure(
            &rig,
            &mut a,
            "R",
            &settings,
            &AtomicBool::new(false),
            &mut |_, _, _| {}
        )
        .is_err()
    );
    assert_eq!(rig.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn connecting_on_another_port_records_where_the_renderer_stands() {
    use companion_service::controller::Controller;
    let root = tempfile::tempdir().unwrap();
    let rig = Arc::new(Rig::new());
    let mut a = acquire(&rig, root.path());
    alignment::apply(&*rig, &mut a, false).unwrap();
    restart(&rig, "other-workflow", Some([(0.0, 0.0); 2]));
    let mut controller = Controller::new(rig.clone());
    controller.state.alignment = Some(a);
    controller.connect("127.0.0.1:9007");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while controller.busy() {
        assert!(
            std::time::Instant::now() < deadline,
            "discovery did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        controller.poll();
    }
    let a = controller.state.alignment.as_ref().unwrap();
    assert!(a.applied_snapshot.is_none(), "{:?}", controller.state.error);
    assert_eq!(a.endpoint, "127.0.0.1:9007".parse::<SocketAddr>().unwrap());
    assert_eq!(
        controller.alignment_live_state(),
        Some(alignment::LiveState::Original)
    );
    assert_eq!(
        rig.writes.load(Ordering::Relaxed),
        1,
        "connecting writes nothing"
    );
}
