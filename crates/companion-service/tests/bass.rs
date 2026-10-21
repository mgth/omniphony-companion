//! Subwoofer integration through a simulated renderer and room: bass-managed sweeps split
//! by Linkwitz-Riley crossovers, a subwoofer that arrives late, wired inverted, 6 dB too
//! loud and with a room mode, and filters applied as the renderer applies them.
use companion_service::{
    alignment::{self, LiveState},
    bass,
    batch::BatchOutcome,
    controller::{Backend, OutputFilter, Settings},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use measurement_capture::{diagnostics, impulse, timing};
use omniphony_adapter::{
    Snapshot,
    protocol::{self, Output, OutputSetting},
};
use rustfft::{FftPlanner, num_complex::Complex};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Mutex, atomic::AtomicBool},
};

const RATE: f64 = 48000.0;
/// The microphone hears every output this much below its digital level.
const AT_MICROPHONE: f64 = 0.25;

type Biquad = [f64; 5];
fn butterworth(fc: f64, high: bool) -> Biquad {
    let k = (std::f64::consts::PI * fc / RATE).tan();
    let q = std::f64::consts::SQRT_2;
    let norm = 1.0 + k / q + k * k;
    let (a1, a2) = (2.0 * (k * k - 1.0) / norm, (1.0 - k / q + k * k) / norm);
    let b0 = if high { 1.0 / norm } else { k * k / norm };
    let b1 = if high { -2.0 * b0 } else { 2.0 * b0 };
    [b0, b1, b0, a1, a2]
}
fn peak(hz: f64, q: f64, gain_db: f64) -> Biquad {
    let a = 10_f64.powf(gain_db / 40.0);
    let w = std::f64::consts::TAU * hz / RATE;
    let alpha = w.sin() / (2.0 * q);
    let a0 = 1.0 + alpha / a;
    [
        (1.0 + alpha * a) / a0,
        -2.0 * w.cos() / a0,
        (1.0 - alpha * a) / a0,
        -2.0 * w.cos() / a0,
        (1.0 - alpha / a) / a0,
    ]
}
fn run(x: &[f64], [b0, b1, b2, a1, a2]: Biquad) -> Vec<f64> {
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    x.iter()
        .map(|&v| {
            let y = b0 * v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, v, y1, y);
            y
        })
        .collect()
}
/// `filters` in series on an impulse arriving `delay_ms` late, scaled by `gain`.
fn path(filters: &[Biquad], delay_ms: f64, gain: f64) -> Vec<f64> {
    let delay = (delay_ms * RATE / 1000.0).round() as usize;
    let mut x = vec![0.0; delay + (0.45 * RATE) as usize];
    x[delay] = gain;
    filters.iter().fold(x, |x, &f| run(&x, f))
}
fn convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
    let n = (a.len() + b.len()).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let (forward, inverse) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
    let load = |x: &[f64]| {
        let mut v: Vec<Complex<f64>> = x.iter().map(|&s| Complex::new(s, 0.0)).collect();
        v.resize(n, Complex::default());
        v
    };
    let (mut fa, mut fb) = (load(a), load(b));
    forward.process(&mut fa);
    forward.process(&mut fb);
    let mut y: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(x, y)| x * y).collect();
    inverse.process(&mut y);
    y[..a.len() + b.len() - 1]
        .iter()
        .map(|v| v.re / n as f64)
        .collect()
}
fn add_into(target: &mut [f64], at: usize, values: &[f64]) {
    for (t, v) in target[at..].iter_mut().zip(values) {
        *t += v;
    }
}

/// Where each output's sound arrives at one microphone position, in ms.
#[derive(Clone)]
struct Position {
    arrival_ms: BTreeMap<&'static str, f64>,
}

/// One batch: its outputs' gains, whether it was bass-managed and bypassed the filters.
type Batch = (BTreeMap<String, f32>, bool, bool);

struct Rig {
    snapshot: Mutex<Snapshot>,
    filters: Mutex<BTreeMap<String, Vec<f32>>>,
    position: Mutex<Position>,
    /// The subwoofer's acoustic level and polarity (wiring, amplifier).
    sub_gain: f64,
    writes: Mutex<usize>,
    batches: Mutex<Vec<Batch>>,
}

fn output(index: u32, id: &str, low: Option<f32>, high: Option<f32>, delay_ms: f32) -> Output {
    Output {
        id: id.into(),
        name: id.into(),
        index,
        gain_db: 0.0,
        delay_ms,
        muted: false,
        freq_low_hz: low,
        freq_high_hz: high,
        azimuth_deg: None,
        elevation_deg: None,
        distance_m: None,
        filter: None,
    }
}

impl Rig {
    /// As Omniphony's bench layout: no speaker plays below 40 Hz, so none can frame
    /// the 40 Hz crossovers.
    fn every_speaker_from_40_hz() -> Self {
        let rig = Self::new();
        {
            let mut state = rig.snapshot.lock().unwrap();
            for output in &mut state.outputs {
                match output.id.as_str() {
                    "L" | "FR" => output.freq_low_hz = Some(40.0),
                    _ => {}
                }
            }
        }
        rig
    }
    fn new() -> Self {
        Self {
            snapshot: Mutex::new(Snapshot {
                instance_id: "rig".into(),
                revision: "rig:0".into(),
                sample_rate: 48000,
                master_gain_db: 0.0,
                max_delay_ms: 250.0,
                unavailable_reason: None,
                output_filters_bypassed: false,
                outputs: vec![
                    output(0, "L", None, None, 1.0),
                    output(1, "FL", Some(80.0), None, 0.8),
                    output(2, "FR", Some(60.0), None, 0.5),
                    output(3, "C", Some(80.0), None, 1.2),
                    output(4, "LFE", None, Some(120.0), 20.0),
                ],
            }),
            filters: Mutex::new(BTreeMap::new()),
            position: Mutex::new(Position {
                arrival_ms: [
                    ("L", 5.0),
                    ("FL", 5.2),
                    ("FR", 5.5),
                    ("C", 4.8),
                    ("LFE", 3.0),
                ]
                .into(),
            }),
            sub_gain: -2.0,
            writes: Mutex::new(0),
            batches: Mutex::new(vec![]),
        }
    }
    fn bump(&self, state: &mut Snapshot) {
        let mut writes = self.writes.lock().unwrap();
        *writes += 1;
        state.revision = format!("rig:{writes}");
    }
    /// What the microphone hears of one segment: `id` direct, or as a positioned source.
    fn impulse_response(
        &self,
        s: &Snapshot,
        id: &str,
        bass_managed: bool,
        bypass: bool,
    ) -> Vec<f64> {
        let position = self.position.lock().unwrap().clone();
        let output = |id: &str| s.outputs.iter().find(|o| o.id == id).unwrap();
        let level = |o: &Output| AT_MICROPHONE * 10_f64.powf(f64::from(o.gain_db) / 20.0);
        let at = |o: &Output| position.arrival_ms[o.id.as_str()] + f64::from(o.delay_ms);
        let sub = output("LFE");
        let subwoofer = |mut filters: Vec<Biquad>| {
            filters.extend([
                butterworth(18.0, true),
                butterworth(150.0, false),
                peak(45.0, 5.0, 8.0),
            ]);
            let mut ir = path(&filters, at(sub), level(sub) * self.sub_gain);
            if !bypass && let Some(taps) = self.filters.lock().unwrap().get("LFE") {
                let taps: Vec<f64> = taps.iter().map(|&t| f64::from(t)).collect();
                ir = convolve(&ir, &taps);
            }
            ir
        };
        let o = output(id);
        if o.id == "LFE" {
            return subwoofer(vec![]);
        }
        match (bass_managed, o.freq_low_hz) {
            (true, Some(fc)) => {
                let fc = f64::from(fc);
                let mut ir = path(
                    &[butterworth(fc, true), butterworth(fc, true)],
                    at(o),
                    level(o),
                );
                let low = subwoofer(vec![butterworth(fc, false), butterworth(fc, false)]);
                if low.len() > ir.len() {
                    ir.resize(low.len(), 0.0);
                }
                add_into(&mut ir, 0, &low);
                ir
            }
            _ => path(&[], at(o), level(o)),
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
            assert!((-60.0..=24.0).contains(&setting.gain_db));
            assert!((0.0..=250.0).contains(&setting.delay_ms));
            o.gain_db = setting.gain_db;
            o.delay_ms = setting.delay_ms;
        }
        self.bump(&mut state);
        Ok(state.clone())
    }
    fn set_output_filters(
        &self,
        _: SocketAddr,
        expected: Snapshot,
        filters: Vec<OutputFilter>,
    ) -> Result<Snapshot, String> {
        let mut state = self.snapshot.lock().unwrap();
        if *state != expected {
            return Err("stale".into());
        }
        for filter in filters {
            let output = state
                .outputs
                .iter_mut()
                .find(|o| o.id == filter.output_id)
                .unwrap();
            match filter.taps {
                Some((rate, taps)) => {
                    output.filter = Some(protocol::OutputFilter {
                        origin: protocol::FilterOrigin::Live,
                        file: None,
                        state: protocol::FilterLoadState::Loaded,
                        sample_rate: Some(rate),
                        taps: Some(taps.len() as u32),
                        hash: Some(protocol::taps_hash_hex(&taps)),
                        error: None,
                    });
                    self.filters.lock().unwrap().insert(filter.output_id, taps);
                }
                None => {
                    output.filter = None;
                    self.filters.lock().unwrap().remove(&filter.output_id);
                }
            }
        }
        self.bump(&mut state);
        Ok(state.clone())
    }
    fn measure_batch(
        &self,
        args: MeasureRequest,
        outputs: Vec<String>,
        _: &AtomicBool,
        progress: &mut dyn FnMut(usize, MeasureProgress),
    ) -> Result<BatchOutcome, String> {
        let state = self.snapshot.lock().unwrap().clone();
        assert_eq!(state, args.snapshot);
        self.batches.lock().unwrap().push((
            state
                .outputs
                .iter()
                .map(|o| (o.id.clone(), o.gain_db))
                .collect(),
            args.bass_managed,
            args.bypass_output_filters,
        ));
        std::fs::create_dir_all(&args.out_dir).unwrap();
        let source: Vec<f64> = args
            .sweep
            .samples(48000)
            .unwrap()
            .iter()
            .map(|&v| f64::from(v))
            .collect();
        let reference = self.impulse_response(
            &state,
            &args.reference_output,
            false,
            args.bypass_output_filters,
        );
        let heard_reference = convolve(&source, &reference);
        let mut takes = vec![];
        for (index, id) in outputs.iter().enumerate() {
            progress(index, MeasureProgress::Analyzing);
            let target =
                self.impulse_response(&state, id, args.bass_managed, args.bypass_output_filters);
            let heard_target = convolve(&source, &target);
            let mut recording = vec![0.0; source.len() * 3 + 48000];
            for (segment, heard) in [&heard_reference, &heard_target, &heard_reference]
                .into_iter()
                .enumerate()
            {
                let at = 24000 + source.len() * segment;
                let end = (at + heard.len()).min(recording.len());
                add_into(&mut recording[..end], at, heard);
            }
            let mut seed = 0x9e37_79b9_u32.wrapping_add(index as u32);
            let samples: Vec<f32> = recording
                .iter()
                .map(|&v| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (v + 1e-5 * (f64::from(seed) / f64::from(u32::MAX) - 0.5)) as f32
                })
                .collect();
            assert!(samples.iter().all(|v| v.abs() < 0.99), "the rig clipped");
            let timing = timing::inspect(&samples, 48000, 48000, args.sweep).unwrap();
            let directory = args.out_dir.join(format!("take-{:03}", index + 1));
            std::fs::create_dir(&directory).unwrap();
            let request = MeasureRequest {
                out_dir: directory.clone(),
                output: id.clone(),
                ..args.clone()
            };
            let full = impulse::full_response(&samples, 48000, 48000, args.sweep, &timing).unwrap();
            companion_service::room_response::store(&request, &full, 48000, true, 0).unwrap();
            let probe = args.sweep.duration_ms == companion_service::level_probe::PROBE_MS;
            takes.push((
                id.clone(),
                TakeOutcome {
                    deferred: false,
                    directory,
                    accepted: true,
                    cancelled: false,
                    rejection_reason: None,
                    diagnostics: probe
                        .then(|| diagnostics::analyze(&samples, 48000, 48000, args.sweep).unwrap()),
                    timing: Some(timing),
                    impulse: None,
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

fn alignment_for(rig: &Rig, root: &std::path::Path) -> alignment::Alignment {
    let snapshot = rig.discover("127.0.0.1:9000".parse().unwrap()).unwrap();
    let rows: Vec<_> = ["L", "FL", "FR", "C", "LFE"]
        .iter()
        .map(|id| {
            serde_json::json!({"output_id": id, "take_folders": [], "repeats": [],
                "qualified": null, "reason": null})
        })
        .collect();
    let settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        output_root: root.display().to_string(),
        ..Settings::default()
    };
    let mut a: alignment::Alignment = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "endpoint": "127.0.0.1:9000",
        "settings": settings, "snapshot": snapshot, "rows": rows,
        "blockers": [], "session": null, "profile": null,
        "applied_snapshot": null, "restored_snapshot": null,
        "verification_note": null, "verification": null,
        "application_uncertain": false
    }))
    .unwrap();
    a.directory = root.join("alignment");
    std::fs::create_dir_all(&a.directory).unwrap();
    a
}

#[test]
fn a_late_inverted_loud_subwoofer_is_measured_integrated_applied_confirmed_and_undone() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = alignment_for(&rig, root.path());
    let original = rig.discover(a.endpoint).unwrap();
    let cancel = AtomicBool::new(false);
    let plan = bass::plan(&a, &original).unwrap();
    assert_eq!(plan.subwoofer, "LFE");
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].reference, "L");
    assert_eq!(plan.speakers(), ["FL", "FR", "C"]);
    assert_eq!(plan.groups[0].sweep.start_hz, 20.0);
    assert_eq!(plan.direct.reference, "L");

    bass::measure(&rig, &mut a, None, &cancel, &mut |_, _| {}).unwrap();
    // Every part was played in its state and the outputs were put back.
    let batches = rig.batches.lock().unwrap().clone();
    let measured: Vec<_> = batches
        .iter()
        .filter(|b| b.0["FL"] != -60.0 || b.0["LFE"] != -60.0)
        .collect();
    assert!(
        batches.iter().all(|b| b.2),
        "every capture bypasses the filters"
    );
    assert!(
        batches
            .iter()
            .any(|b| b.1 && b.0["LFE"] == -60.0 && b.0["FL"] == 0.0)
    );
    assert!(batches.iter().any(|b| b.1
        && b.0["LFE"] == 0.0
        && b.0["FL"] == -60.0
        && b.0["C"] == -60.0
        && b.0["L"] == 0.0));
    assert!(batches.iter().any(|b| !b.1));
    assert!(!measured.is_empty());
    let after = rig.discover(a.endpoint).unwrap();
    assert!(omniphony_adapter::same_installation(&after, &original));

    let b = a.bass.as_ref().unwrap();
    let capture = b.capture(None).unwrap();
    assert!(capture.complete(&b.plan), "{capture:?}");
    assert!(b.level_probe.is_some());
    let analysis = b
        .analysis
        .as_ref()
        .unwrap_or_else(|| panic!("{:?}", b.analysis_error));
    let proposal = &analysis.proposal;
    assert!(proposal.refusal.is_none(), "{proposal:?}");
    // The subwoofer arrives 17 ms after the speakers (3 + 20 against 6), inverted and
    // 6 dB loud, with a mode at 45 Hz.
    let setting = proposal.setting;
    assert!(setting.inverted, "{setting:?}");
    assert!(
        (setting.delay_change_ms + 17.0).abs() < 2.0,
        "delay change {}",
        setting.delay_change_ms
    );
    assert!(
        (-10.0..=-3.0).contains(&setting.gain_change_db),
        "gain change {}",
        setting.gain_change_db
    );
    for speaker in &proposal.speakers {
        assert!(speaker.excluded.is_none(), "{speaker:?}");
        let phase = speaker.phase_delay_ms.unwrap();
        assert!((phase - 17.0).abs() < 3.0, "{}: {phase}", speaker.output_id);
        let [measured, _, proposed] = speaker.summation_db.unwrap();
        assert!(proposed > measured + 1.0 && proposed > -1.0, "{speaker:?}");
        assert!(speaker.consistency_db.unwrap() < 0.5, "{speaker:?}");
    }
    let mode = analysis.mode_correction.as_ref().unwrap();
    assert!(
        mode.filter.as_ref().unwrap().deepest_cut_db < -3.0,
        "{mode:?}"
    );
    let filter = analysis.filter.as_ref().unwrap();
    assert!(filter.file.is_file());
    assert_eq!(filter.taps, 32768);

    // A second position, a little to the left.
    rig.position.lock().unwrap().arrival_ms = [
        ("L", 5.1),
        ("FL", 5.0),
        ("FR", 5.9),
        ("C", 4.9),
        ("LFE", 3.3),
    ]
    .into();
    bass::measure(&rig, &mut a, Some(0), &cancel, &mut |_, _| {}).unwrap();
    let b = a.bass.as_ref().unwrap();
    assert_eq!(b.captures.len(), 2);
    let proposal = &b.analysis.as_ref().unwrap().proposal;
    assert!(proposal.speakers.iter().all(|s| s.positions == 2));
    rig.position.lock().unwrap().arrival_ms = [
        ("L", 5.0),
        ("FL", 5.2),
        ("FR", 5.5),
        ("C", 4.8),
        ("LFE", 3.0),
    ]
    .into();

    // Apply: the subwoofer's delay, level and filter, live.
    let applied = bass::apply(&rig, &mut a, false).unwrap();
    let b = a.bass.as_ref().unwrap();
    let proposal = &b.analysis.as_ref().unwrap().proposal;
    let lfe = applied.outputs.iter().find(|o| o.id == "LFE").unwrap();
    assert!((f64::from(lfe.delay_ms) - proposal.subwoofer_delay_ms).abs() < 1e-3);
    assert!((f64::from(lfe.gain_db) - proposal.subwoofer_gain_db).abs() < 1e-3);
    let hash = &b.analysis.as_ref().unwrap().filter.as_ref().unwrap().hash;
    assert_eq!(lfe.filter.as_ref().unwrap().hash.as_ref(), Some(hash));
    let taps = rig.filters.lock().unwrap()["LFE"].clone();
    assert!(taps[0] < 0.0, "the filter carries the inversion");
    assert_eq!(b.live_state(&applied), LiveState::Applied);
    assert!(b.is_applied());
    // The alignment knows the state underneath and waits for the subwoofer's Undo.
    assert_eq!(a.live_state(&applied), LiveState::Original);
    assert!(alignment::apply(&rig, &mut a, false).is_err());
    assert!(bass::readiness(&a, &applied, None).is_err());

    // Confirm: the sum measured again, through the filter, matches the prediction.
    bass::verify(&rig, &mut a, &cancel, &mut |_, _| {}).unwrap();
    let batches = rig.batches.lock().unwrap().clone();
    let last = batches.last().unwrap();
    assert!(
        last.1 && !last.2,
        "the confirmation plays through the filters"
    );
    let verification = a.bass.as_ref().unwrap().verification.clone().unwrap();
    assert_eq!(verification.speakers.len(), 3, "{verification:?}");
    for check in &verification.speakers {
        let [_, predicted, measured] = check.sum_level_db;
        assert!((predicted - measured).abs() < 0.5, "{check:?}");
        // The loud subwoofer made the sum heavier below the crossover than above it.
        let [before, predicted, measured] = check.balance_db;
        assert!((predicted - measured).abs() < 0.5, "{check:?}");
        assert!(before > 3.0 && measured.abs() < 1.5, "{check:?}");
        assert!(check.deviation_db < 1.0, "{check:?}");
    }

    // Undo: back to the measured state, without the filter.
    let restored = bass::apply(&rig, &mut a, true).unwrap();
    assert!(omniphony_adapter::same_installation(&restored, &original));
    assert!(restored.outputs.iter().all(|o| o.filter.is_none()));
    assert!(!a.bass.as_ref().unwrap().is_applied());
    assert!(a.bass.as_ref().unwrap().verification.is_none());
    assert!(!a.application_uncertain);
    // Saved and reloaded as it is.
    let reloaded = alignment::load(&a.directory).unwrap();
    assert_eq!(reloaded.bass, a.bass);
}

#[test]
fn a_subwoofer_that_already_plays_a_filter_is_not_measured() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let a = alignment_for(&rig, root.path());
    let live = rig.discover(a.endpoint).unwrap();
    rig.set_output_filters(
        a.endpoint,
        live,
        vec![OutputFilter {
            output_id: "LFE".into(),
            taps: Some((48000, vec![1.0, 0.5])),
        }],
    )
    .unwrap();
    let live = rig.discover(a.endpoint).unwrap();
    let error = bass::readiness(&a, &live, None).unwrap_err();
    assert!(
        error.contains("already plays a correction filter"),
        "{error}"
    );
    // A position needs the main seat first.
    let mut no_filter = live.clone();
    no_filter.outputs[4].filter = None;
    let error = bass::readiness(&a, &no_filter, Some(0)).unwrap_err();
    assert!(error.contains("main seat first"), "{error}");
}

#[test]
fn speakers_split_below_every_other_speaker_are_framed_by_the_subwoofer() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::every_speaker_from_40_hz();
    let mut a = alignment_for(&rig, root.path());
    let original = rig.discover(a.endpoint).unwrap();
    let plan = bass::plan(&a, &original).unwrap();
    assert_eq!(plan.groups.len(), 2, "{plan:?}");
    assert_eq!(plan.groups[0].reference, "L");
    assert_eq!(plan.groups[0].outputs, ["FL", "C"]);
    assert_eq!(plan.groups[0].sweep.start_hz, 40.0);
    assert_eq!(plan.groups[1].reference, "LFE");
    assert_eq!(plan.groups[1].outputs, ["L", "FR"]);
    assert_eq!(
        [plan.groups[1].sweep.start_hz, plan.groups[1].sweep.end_hz],
        [20.0, 120.0]
    );
    assert_eq!(
        plan.parts_of("FR"),
        [bass::Part::Sum, bass::Part::Subwoofer]
    );
    let cancel = AtomicBool::new(false);
    bass::measure(&rig, &mut a, None, &cancel, &mut |_, _| {}).unwrap();
    // The subwoofer is never silenced while it frames a group.
    let batches = rig.batches.lock().unwrap().clone();
    assert!(
        batches
            .iter()
            .any(|b| b.1 && b.0["LFE"] == 0.0 && b.0["L"] == -60.0 && b.0["FR"] == -60.0)
    );
    let b = a.bass.as_ref().unwrap();
    assert!(b.capture(None).unwrap().complete(&b.plan));
    let analysis = b
        .analysis
        .as_ref()
        .unwrap_or_else(|| panic!("{:?}", b.analysis_error));
    let proposal = &analysis.proposal;
    assert!(proposal.refusal.is_none(), "{proposal:?}");
    for id in ["L", "FR"] {
        let speaker = proposal
            .speakers
            .iter()
            .find(|s| s.output_id == id)
            .unwrap();
        assert!(speaker.excluded.is_none(), "{speaker:?}");
        assert!(
            speaker
                .warnings
                .iter()
                .any(|w| w.contains("is its timing reference")),
            "{speaker:?}"
        );
        // The 45 Hz mode's own group delay is part of the subwoofer's lateness this low.
        let phase = speaker.phase_delay_ms.unwrap();
        assert!(phase > 15.0 && phase < 35.0, "{id}: {phase}");
        let [measured, _, proposed] = speaker.summation_db.unwrap();
        assert!(proposed > measured + 1.0 && proposed > -1.0, "{speaker:?}");
    }
    assert!(proposal.setting.inverted);
    assert!(
        (proposal.setting.delay_change_ms + 17.0).abs() < 2.0,
        "{proposal:?}"
    );
    // Applied and confirmed: the subwoofer-framed sums follow its own delay change.
    bass::apply(&rig, &mut a, false).unwrap();
    bass::verify(&rig, &mut a, &cancel, &mut |_, _| {}).unwrap();
    let verification = a.bass.as_ref().unwrap().verification.clone().unwrap();
    assert_eq!(verification.speakers.len(), 4, "{verification:?}");
    for check in &verification.speakers {
        assert!(check.deviation_db < 1.0, "{check:?}");
    }
    bass::apply(&rig, &mut a, true).unwrap();
}
