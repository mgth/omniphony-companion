use companion_service::{
    alignment,
    batch::BatchOutcome,
    controller::{Backend, Settings},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use measurement_capture::{impulse, timing};
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
    writes: AtomicUsize,
    lose_ack: AtomicBool,
    bad_repeat: AtomicBool,
    silent_batch: AtomicBool,
    captures: AtomicUsize,
}
impl Rig {
    fn new() -> Self {
        Self {
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
                    })
                    .collect(),
            }),
            writes: AtomicUsize::new(0),
            lose_ack: AtomicBool::new(false),
            bad_repeat: AtomicBool::new(false),
            silent_batch: AtomicBool::new(false),
            captures: AtomicUsize::new(0),
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
        _: SocketAddr,
        expected: Snapshot,
        settings: Vec<OutputSetting>,
    ) -> Result<Snapshot, String> {
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
                let physical_delay = if *output_id == "L" { 144 } else { 288 };
                let delay = physical_delay + (o.delay_ms as f64 * 48.0).round() as usize;
                let gain = if *output_id == "L" { 0.5 } else { 0.25 };
                let gain = gain * 10_f32.powf(o.gain_db / 20.0);
                let start = 24000 + source.len() * segment + delay;
                for (j, x) in source.iter().enumerate() {
                    samples[start + j] += gain * x;
                }
            }
            let timing = timing::analyze(&samples, 48000, 48000, args.sweep).unwrap();
            let mut response =
                impulse::analyze(&samples, 48000, 48000, args.sweep, &timing).unwrap();
            if self.bad_repeat.load(Ordering::Relaxed) && pass % 2 == 1 && id == "R" {
                response.relative_arrival_ms = response.relative_arrival_ms.map(|v| v + 2.0);
            }
            let directory = args.out_dir.join(id);
            std::fs::create_dir(&directory).unwrap();
            takes.push((
                id.clone(),
                TakeOutcome {
                    directory,
                    accepted: true,
                    cancelled: false,
                    rejection_reason: None,
                    diagnostics: None,
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
    rig.snapshot.lock().unwrap().revision = "external-change".into();
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
