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
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
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
    /// The next write that silences an output goes through but its reply is lost.
    lose_silencing_ack: AtomicBool,
    /// Captures fail, as when the microphone input disappears.
    fail_captures: AtomicBool,
    /// The next gain/delay write is refused before it changes anything.
    refuse_next_outputs: AtomicBool,
    /// After that refusal, the renderer cannot be read until reconnected.
    blind_after_refusal: AtomicBool,
    unreadable: AtomicBool,
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
            lose_silencing_ack: AtomicBool::new(false),
            fail_captures: AtomicBool::new(false),
            refuse_next_outputs: AtomicBool::new(false),
            blind_after_refusal: AtomicBool::new(false),
            unreadable: AtomicBool::new(false),
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
        if self.unreadable.load(Ordering::Relaxed) {
            return Err("renderer unreachable".into());
        }
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
        if self.refuse_next_outputs.swap(false, Ordering::Relaxed) {
            if self.blind_after_refusal.load(Ordering::Relaxed) {
                self.unreadable.store(true, Ordering::Relaxed);
            }
            return Err("renderer refused the request (busy)".into());
        }
        let mut state = self.snapshot.lock().unwrap();
        if *state != expected {
            return Err("stale".into());
        }
        let silencing = settings.iter().any(|s| s.gain_db == -60.0);
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
        if silencing && self.lose_silencing_ack.swap(false, Ordering::Relaxed) {
            return Err("lost acknowledgement".into());
        }
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
        if self.fail_captures.load(Ordering::Relaxed) {
            return Err("input stream did not start cleanly".into());
        }
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

#[test]
fn a_silencing_write_whose_reply_was_lost_is_put_back_from_what_the_renderer_reads() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = alignment_for(&rig, root.path());
    let original = rig.discover(a.endpoint).unwrap();
    rig.lose_silencing_ack.store(true, Ordering::Relaxed);
    let error =
        bass::measure(&rig, &mut a, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap_err();
    assert!(error.contains("lost acknowledgement"), "{error}");
    let live = rig.discover(a.endpoint).unwrap();
    assert!(omniphony_adapter::same_installation(&live, &original));
    assert!(!a.application_uncertain);
}

#[test]
fn a_moved_speaker_makes_the_subwoofer_measurement_stale() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = alignment_for(&rig, root.path());
    bass::measure(&rig, &mut a, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let live = rig.discover(a.endpoint).unwrap();
    assert!(bass::apply_readiness(&a, &live, false).is_ok());
    let mut moved = live.clone();
    moved.outputs[1].azimuth_deg = Some(150.0);
    let b = a.bass.as_ref().unwrap();
    assert_eq!(b.live_state(&moved), LiveState::Other);
    assert!(bass::apply_readiness(&a, &moved, false).is_err());
    assert!(bass::readiness(&a, &moved, Some(0)).is_err());
}

#[test]
fn a_confirmation_that_fails_leaves_no_earlier_result_current() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = alignment_for(&rig, root.path());
    let cancel = AtomicBool::new(false);
    bass::measure(&rig, &mut a, None, &cancel, &mut |_, _| {}).unwrap();
    bass::apply(&rig, &mut a, false).unwrap();
    bass::verify(&rig, &mut a, &cancel, &mut |_, _| {}).unwrap();
    assert!(a.bass.as_ref().unwrap().verification.is_some());
    rig.fail_captures.store(true, Ordering::Relaxed);
    assert!(bass::verify(&rig, &mut a, &cancel, &mut |_, _| {}).is_err());
    let b = a.bass.as_ref().unwrap();
    assert!(b.verification.is_none());
    assert!(
        b.verification_note
            .as_deref()
            .unwrap()
            .starts_with("Not confirmed")
    );
    let reloaded = alignment::load(&a.directory).unwrap();
    assert!(reloaded.bass.unwrap().verification.is_none());
}

#[test]
fn the_room_correction_applies_and_undoes_the_subwoofer_with_the_speakers_filters() {
    use companion_service::{
        correction::{self, Correction, Live, SpeakerFilter, Taps, Target},
        room_correction::{OutputSummary, Summary},
    };
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let mut a = alignment_for(&rig, root.path());
    let original = rig.discover(a.endpoint).unwrap();
    bass::measure(&rig, &mut a, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let proposal = a.bass.as_ref().unwrap().analysis.clone().unwrap();
    let sub_hash = proposal.filter.as_ref().unwrap().hash.clone();
    // FL's room correction filter, as a computation exports it.
    let directory = a.directory.join("room-correction-test");
    std::fs::create_dir_all(&directory).unwrap();
    let taps = vec![0.9_f32, 0.05, -0.01];
    let file = directory.join("FL.wav");
    let mut w = hound::WavWriter::create(
        &file,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for &t in &taps {
        w.write_sample(t).unwrap();
    }
    w.finalize().unwrap();
    let summary: OutputSummary = serde_json::from_value(serde_json::json!({
        "output_id": "FL", "file": "FL.wav", "refusal": null, "warnings": [],
        "calibrated": true, "taps": 3, "sample_rate": 48000, "headroom_db": 0.0,
        "deepest_cut_db": -1.0, "normalisation_db": 0.0, "band_hz": null,
        "low_limit_hz": null, "evaluation_band_hz": null, "deviation_db": [2.0, 1.0],
        "repeatability_db": null, "spread_db": null, "positions": [], "reflections": []
    }))
    .unwrap();
    a.correction = Some(Correction {
        choices: Default::default(),
        settings: Default::default(),
        directory: directory.clone(),
        summary: Summary {
            schema_version: 1,
            method: "test".into(),
            settings: Default::default(),
            outputs: vec![summary],
        },
        filters: vec![SpeakerFilter {
            output_id: "FL".into(),
            taps: Taps {
                file,
                sample_rate: 48000,
                taps: taps.len(),
                hash: protocol::taps_hash_hex(&taps),
            },
        }],
        previous: vec![],
        applied: None,
        uncertain: false,
        subwoofer: false,
        pending: None,
        verification: None,
        verification_note: None,
        curves: vec![],
    });
    let endpoint = a.endpoint;
    let hash = |id: &str| {
        rig.discover(endpoint)
            .unwrap()
            .outputs
            .iter()
            .find(|o| o.id == id)
            .unwrap()
            .filter
            .as_ref()
            .and_then(|f| f.hash.clone())
    };
    let lfe = |s: &Snapshot| {
        let o = s.outputs.iter().find(|o| o.id == "LFE").unwrap();
        (f64::from(o.gain_db), f64::from(o.delay_ms))
    };

    // Apply: FL's filter and the subwoofer's in one write, then its delay and level.
    let applied = correction::apply(&rig, &mut a, Target::Corrected).unwrap();
    assert_eq!(hash("FL"), Some(protocol::taps_hash_hex(&taps)));
    assert_eq!(hash("LFE"), Some(sub_hash.clone()));
    let (gain, delay) = lfe(&applied);
    assert!((delay - proposal.proposal.subwoofer_delay_ms).abs() < 1e-3);
    assert!((gain - proposal.proposal.subwoofer_gain_db).abs() < 1e-3);
    assert!(a.correction.as_ref().unwrap().subwoofer);
    assert!(a.bass.as_ref().unwrap().is_applied());
    assert!(!a.application_uncertain);
    assert!(!alignment::reconcile(&mut a, &applied).unwrap());

    // Listening to the original switches FL only: the subwoofer stays integrated.
    correction::apply(&rig, &mut a, Target::Compare).unwrap();
    assert_eq!(hash("FL"), Some(correction::identity_hash()));
    assert_eq!(hash("LFE"), Some(sub_hash));
    assert_eq!(lfe(&rig.discover(a.endpoint).unwrap()), (gain, delay));
    correction::apply(&rig, &mut a, Target::Corrected).unwrap();

    // Undo: the subwoofer as measured, without a filter, and FL without one.
    correction::apply(&rig, &mut a, Target::Original).unwrap();
    let after = rig.discover(a.endpoint).unwrap();
    assert_eq!(hash("FL"), None);
    assert_eq!(hash("LFE"), None);
    assert!(omniphony_adapter::same_installation(&after, &original));
    assert!(!a.bass.as_ref().unwrap().is_applied());
    assert_eq!(
        a.correction.as_ref().unwrap().live_state(&after),
        Live::Original
    );

    // The filters went through but the subwoofer's delay did not: Undo still owes the
    // subwoofer its filter back, and takes it.
    rig.refuse_next_outputs.store(true, Ordering::Relaxed);
    assert!(correction::apply(&rig, &mut a, Target::Corrected).is_err());
    assert_eq!(
        hash("LFE"),
        Some(proposal.filter.as_ref().unwrap().hash.clone())
    );
    assert!(a.correction.as_ref().unwrap().subwoofer);
    assert!(!a.bass.as_ref().unwrap().is_applied());
    correction::apply(&rig, &mut a, Target::Original).unwrap();
    assert_eq!(hash("FL"), None);
    assert_eq!(hash("LFE"), None);
    assert!(omniphony_adapter::same_installation(
        &rig.discover(endpoint).unwrap(),
        &original
    ));
    assert!(!a.correction.as_ref().unwrap().subwoofer);

    // The same, with the renderer unreadable until a reconnect: the alignment is
    // reloaded, reconciled, and Undo still takes the subwoofer's filter back.
    rig.refuse_next_outputs.store(true, Ordering::Relaxed);
    rig.blind_after_refusal.store(true, Ordering::Relaxed);
    assert!(correction::apply(&rig, &mut a, Target::Corrected).is_err());
    let mut a = alignment::load(&a.directory).unwrap();
    assert!(a.correction.as_ref().unwrap().uncertain);
    rig.unreadable.store(false, Ordering::Relaxed);
    rig.blind_after_refusal.store(false, Ordering::Relaxed);
    alignment::reconcile(&mut a, &rig.discover(endpoint).unwrap()).unwrap();
    let c = a.correction.as_ref().unwrap();
    assert!(!c.uncertain && c.subwoofer, "{c:?}");
    assert!(!a.application_uncertain);
    correction::apply(&rig, &mut a, Target::Original).unwrap();
    assert_eq!(hash("FL"), None);
    assert_eq!(hash("LFE"), None);
    assert!(omniphony_adapter::same_installation(
        &rig.discover(endpoint).unwrap(),
        &original
    ));
}
