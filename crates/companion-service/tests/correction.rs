//! The guided room correction through a simulated renderer that keeps filters as
//! protocol 8 does: uploads replaced together under a revision check, live until a
//! Save turns them into files, a file filter read back by its hash, and every take
//! played through the filters unless its session holds them off.
use companion_service::{
    alignment,
    batch::BatchOutcome,
    controller::{Backend, OutputFilter, Settings},
    correction::{self, Choices, Live, Saved, Target},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
    positions,
};
use measurement_capture::{impulse, timing};
use omniphony_adapter::{
    Snapshot,
    protocol::{self, FilterLoadState, FilterOrigin, Output, OutputSetting},
};
use rustfft::{FftPlanner, num_complex::Complex};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
};

type Biquad = [f64; 5];
fn peak(hz: f64, q: f64, gain_db: f64) -> Biquad {
    let a = 10_f64.powf(gain_db / 40.0);
    let w = std::f64::consts::TAU * hz / 48000.0;
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
fn convolve(a: &[f64], b: &[f32]) -> Vec<f64> {
    let n = (a.len() + b.len()).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let (forward, inverse) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
    let load = |x: &mut dyn Iterator<Item = f64>| {
        let mut v: Vec<Complex<f64>> = x.map(|s| Complex::new(s, 0.0)).collect();
        v.resize(n, Complex::default());
        v
    };
    let (mut fa, mut fb) = (
        load(&mut a.iter().copied()),
        load(&mut b.iter().map(|&t| f64::from(t))),
    );
    forward.process(&mut fa);
    forward.process(&mut fb);
    let mut y: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(x, y)| x * y).collect();
    inverse.process(&mut y);
    y[..a.len()].iter().map(|v| v.re / n as f64).collect()
}

fn write_wav(path: &Path, taps: &[f32]) {
    let mut w = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for &t in taps {
        w.write_sample(t).unwrap();
    }
    w.finalize().unwrap();
}

struct Rig {
    snapshot: Mutex<Snapshot>,
    /// What each output plays through, as the renderer holds it.
    filters: Mutex<BTreeMap<String, Vec<f32>>>,
    version: AtomicU32,
    writes: AtomicUsize,
    filter_writes: AtomicUsize,
    /// The next filter write goes through but its reply is lost.
    lose_filter_ack: AtomicBool,
    /// After a lost reply, the renderer cannot be read back either.
    blind_after_lost_ack: AtomicBool,
    unreadable: AtomicBool,
    /// Another client changes the renderer just before the next filter write.
    interleave: AtomicBool,
    /// Whether each batch held the filters off.
    bypassed: Mutex<Vec<bool>>,
    root: PathBuf,
}
fn output(index: u32, id: &str) -> Output {
    Output {
        id: id.into(),
        name: id.into(),
        index,
        gain_db: 0.0,
        delay_ms: 0.0,
        muted: false,
        freq_low_hz: None,
        freq_high_hz: None,
        azimuth_deg: None,
        elevation_deg: None,
        distance_m: None,
        filter: None,
    }
}
impl Rig {
    fn new(root: &Path) -> Self {
        Self {
            snapshot: Mutex::new(Snapshot {
                instance_id: "rig".into(),
                revision: "rig:0".into(),
                sample_rate: 48000,
                master_gain_db: 0.0,
                max_delay_ms: 100.0,
                unavailable_reason: None,
                output_filters_bypassed: false,
                outputs: vec![output(0, "L"), output(1, "R")],
            }),
            filters: Mutex::new(BTreeMap::new()),
            version: AtomicU32::new(protocol::VERSION),
            writes: AtomicUsize::new(0),
            filter_writes: AtomicUsize::new(0),
            lose_filter_ack: AtomicBool::new(false),
            blind_after_lost_ack: AtomicBool::new(false),
            unreadable: AtomicBool::new(false),
            interleave: AtomicBool::new(false),
            bypassed: Mutex::new(vec![]),
            root: root.to_owned(),
        }
    }
    fn bump(&self, state: &mut Snapshot) {
        let n = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        state.revision = format!("rig:{n}");
    }
    fn hash(&self, id: &str) -> Option<String> {
        self.snapshot
            .lock()
            .unwrap()
            .outputs
            .iter()
            .find(|o| o.id == id)?
            .filter
            .as_ref()?
            .hash
            .clone()
    }
    fn origin(&self, id: &str) -> Option<FilterOrigin> {
        Some(
            self.snapshot
                .lock()
                .unwrap()
                .outputs
                .iter()
                .find(|o| o.id == id)?
                .filter
                .as_ref()?
                .origin,
        )
    }
    /// The layout names a filter file for `id`, as Studio's speaker editor sets it.
    fn load_file(&self, id: &str, path: &Path, taps: &[f32]) {
        write_wav(path, taps);
        let mut state = self.snapshot.lock().unwrap();
        let o = state.outputs.iter_mut().find(|o| o.id == id).unwrap();
        o.filter = Some(protocol::OutputFilter {
            origin: FilterOrigin::File,
            file: Some(path.display().to_string()),
            state: FilterLoadState::Loaded,
            sample_rate: Some(48000),
            taps: Some(taps.len() as u32),
            hash: Some(protocol::taps_hash_hex(taps)),
            error: None,
        });
        self.filters
            .lock()
            .unwrap()
            .insert(id.into(), taps.to_vec());
        self.bump(&mut state);
    }
    /// Studio's Save: every live filter becomes a file with the same taps.
    fn save(&self) {
        let mut state = self.snapshot.lock().unwrap();
        let filters = self.filters.lock().unwrap();
        for o in &mut state.outputs {
            if let Some(f) = &mut o.filter
                && f.origin == FilterOrigin::Live
            {
                let path =
                    self.root
                        .join(format!("saved-{}-{}.wav", o.id, f.hash.as_deref().unwrap()));
                write_wav(&path, &filters[&o.id]);
                f.origin = FilterOrigin::File;
                f.file = Some(path.display().to_string());
            }
        }
        self.bump(&mut state);
    }
    /// What the microphone hears of `id` playing `source`: L flat, R with a 1 kHz
    /// peak; each through its filter unless the session holds the filters off.
    fn heard(&self, id: &str, source: &[f64], bypass: bool) -> Vec<f64> {
        let mut x: Vec<f64> = source.to_vec();
        if id == "R" {
            x = run(&x, peak(1000.0, 1.4, 6.0));
            x = run(&x, peak(3000.0, 2.0, -5.0));
        }
        if !bypass && let Some(taps) = self.filters.lock().unwrap().get(id) {
            x = convolve(&x, taps);
        }
        x
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
        let mut state = self.snapshot.lock().unwrap();
        let s = state.clone();
        // Uploaded filters load in the background: pending once, then loaded.
        for o in &mut state.outputs {
            if let Some(f) = &mut o.filter {
                f.state = FilterLoadState::Loaded;
            }
        }
        Ok(s)
    }
    fn discover_versioned(&self, endpoint: SocketAddr) -> Result<(Snapshot, u32), String> {
        Ok((
            self.discover(endpoint)?,
            self.version.load(Ordering::Relaxed),
        ))
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
        if state.revision != expected.revision {
            return Err("renderer refused the request (stale_revision)".into());
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
        self.bump(&mut state);
        Ok(state.clone())
    }
    fn set_output_filters(
        &self,
        _: SocketAddr,
        expected: Snapshot,
        filters: Vec<OutputFilter>,
    ) -> Result<Snapshot, String> {
        if self.version.load(Ordering::Relaxed) < protocol::FILTERS_VERSION {
            return Err("this Omniphony build cannot measure or apply room correction filters (protocol 8); update Omniphony".into());
        }
        let mut state = self.snapshot.lock().unwrap();
        if self.interleave.swap(false, Ordering::Relaxed) {
            // Another client's write lands first.
            self.bump(&mut state);
        }
        if state.revision != expected.revision {
            return Err(
                "renderer refused the request (stale_revision): the revision changed".into(),
            );
        }
        self.filter_writes.fetch_add(1, Ordering::Relaxed);
        for filter in filters {
            let o = state
                .outputs
                .iter_mut()
                .find(|o| o.id == filter.output_id)
                .unwrap();
            match filter.taps {
                Some((rate, taps)) => {
                    o.filter = Some(protocol::OutputFilter {
                        origin: FilterOrigin::Live,
                        file: None,
                        state: FilterLoadState::Pending,
                        sample_rate: Some(rate),
                        taps: Some(taps.len() as u32),
                        hash: Some(protocol::taps_hash_hex(&taps)),
                        error: None,
                    });
                    self.filters.lock().unwrap().insert(filter.output_id, taps);
                }
                None => {
                    o.filter = None;
                    self.filters.lock().unwrap().remove(&filter.output_id);
                }
            }
        }
        self.bump(&mut state);
        if self.lose_filter_ack.swap(false, Ordering::Relaxed) {
            if self.blind_after_lost_ack.load(Ordering::Relaxed) {
                self.unreadable.store(true, Ordering::Relaxed);
            }
            return Err("missing filter acknowledgement; the outcome is uncertain".into());
        }
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
        assert_eq!(state.revision, args.snapshot.revision);
        self.bypassed
            .lock()
            .unwrap()
            .push(args.bypass_output_filters);
        std::fs::create_dir_all(&args.out_dir).unwrap();
        let source: Vec<f64> = args
            .sweep
            .samples(48000)
            .unwrap()
            .iter()
            .map(|&v| f64::from(v))
            .collect();
        let mut takes = vec![];
        for (index, id) in outputs.iter().enumerate() {
            progress(index, MeasureProgress::Analyzing);
            let mut samples = vec![0.0_f64; source.len() * 3 + 48000];
            for (segment, output_id) in ["L", id.as_str(), "L"].iter().enumerate() {
                let o = state.outputs.iter().find(|o| o.id == *output_id).unwrap();
                let delay = if *output_id == "L" { 144 } else { 288 }
                    + (f64::from(o.delay_ms) * 48.0).round() as usize;
                let gain = if *output_id == "L" { 0.5 } else { 0.25 };
                let gain = gain * 10_f64.powf(f64::from(o.gain_db) / 20.0);
                let heard = self.heard(output_id, &source, args.bypass_output_filters);
                let start = 24000 + source.len() * segment + delay;
                for (j, x) in heard.iter().enumerate() {
                    if let Some(s) = samples.get_mut(start + j) {
                        *s += gain * x;
                    }
                }
            }
            let samples: Vec<f32> = samples.iter().map(|&v| v as f32).collect();
            let timing = timing::inspect(&samples, 48000, 48000, args.sweep).unwrap();
            let response = impulse::analyze(&samples, 48000, 48000, args.sweep, &timing).ok();
            let directory = args.out_dir.join(id);
            std::fs::create_dir(&directory).unwrap();
            let request = MeasureRequest {
                out_dir: directory.clone(),
                output: id.clone(),
                ..args.clone()
            };
            let full = impulse::full_response(&samples, 48000, 48000, args.sweep, &timing).unwrap();
            companion_service::room_response::store(&request, &full, 48000, true, 0).unwrap();
            takes.push((
                id.clone(),
                TakeOutcome {
                    deferred: false,
                    directory,
                    accepted: true,
                    cancelled: false,
                    rejection_reason: None,
                    diagnostics: None,
                    timing: Some(timing),
                    impulse: response.map(Arc::new),
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

fn acquire(rig: &Rig, root: &Path) -> alignment::Alignment {
    let mut settings = Settings {
        device: Some("test microphone".into()),
        reference_output: Some("L".into()),
        output_root: root.display().to_string(),
        sweep_mode: companion_service::sweep_plan::SweepMode::Custom,
        automatic_level: false,
        ..Settings::default()
    };
    settings.sweep.duration_ms = 400;
    settings.sweep.start_hz = 100.0;
    settings.sweep.end_hz = 16000.0;
    let endpoint = "127.0.0.1:9000".parse().unwrap();
    let request = alignment::request(
        endpoint,
        rig.discover(endpoint).unwrap(),
        &settings,
        "L".into(),
        root.join(format!("alignment-{}", alignment_count(root))),
    )
    .unwrap();
    alignment::capture(
        rig,
        request,
        settings,
        vec!["L".into(), "R".into()],
        alignment::Basis::Refine,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    )
    .unwrap()
    .alignment
}
fn alignment_count(root: &Path) -> usize {
    std::fs::read_dir(root).map_or(0, |d| d.count())
}

/// An alignment, one guided position and its computed correction.
fn computed(rig: &Rig, root: &Path) -> alignment::Alignment {
    let mut a = acquire(rig, root);
    positions::measure(rig, &mut a, 0, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let c = correction::compute(&a, Choices::default()).unwrap();
    a.correction = Some(c);
    alignment::save(&a).unwrap();
    a
}

fn live(rig: &Rig) -> Snapshot {
    rig.discover("127.0.0.1:9000".parse().unwrap()).unwrap()
}
fn state(rig: &Rig, a: &alignment::Alignment) -> Live {
    a.correction.as_ref().unwrap().live_state(&live(rig))
}

#[test]
fn a_correction_is_applied_compared_confirmed_and_undone_at_constant_latency() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    let c = a.correction.clone().unwrap();
    assert_eq!(c.filters.len(), 2, "{:?}", c.summary);
    assert_eq!(c.curves.len(), 2);
    assert_eq!(state(&rig, &a), Live::Original);
    // Computing never writes to the renderer.
    assert_eq!(rig.filter_writes.load(Ordering::Relaxed), 0);

    correction::apply(&rig, &mut a, Target::Corrected).unwrap();
    let c = a.correction.clone().unwrap();
    assert!(!c.uncertain && c.applied.is_some());
    // One write for every filter at once.
    assert_eq!(rig.filter_writes.load(Ordering::Relaxed), 1);
    for f in &c.filters {
        assert_eq!(
            rig.hash(&f.output_id).as_deref(),
            Some(f.taps.hash.as_str())
        );
    }
    assert_eq!(c.previous.iter().filter(|p| p.filter.is_none()).count(), 2);
    assert_eq!(state(&rig, &a), Live::Corrected);
    assert_eq!(
        c.saved(&live(&rig)),
        Saved::Unsaved(vec!["L".into(), "R".into()])
    );
    // Applying twice changes nothing.
    assert!(correction::apply(&rig, &mut a, Target::Corrected).is_err());
    assert_eq!(rig.filter_writes.load(Ordering::Relaxed), 1);

    // Original keeps a filter on every output (an identity where none was): the
    // correction stage, and its latency, stay.
    correction::apply(&rig, &mut a, Target::Compare).unwrap();
    assert_eq!(state(&rig, &a), Live::Comparing);
    for id in ["L", "R"] {
        assert_eq!(rig.hash(id), Some(correction::identity_hash()));
    }
    let comparing = a.clone();
    assert!(correction::verify_readiness(&comparing, &live(&rig)).is_err());
    correction::apply(&rig, &mut a, Target::Corrected).unwrap();
    assert_eq!(state(&rig, &a), Live::Corrected);
    // The original filters recorded at the first Apply are still the ones to restore.
    assert_eq!(a.correction.as_ref().unwrap().previous, c.previous);

    // Measured again through the filters: as predicted, and closer to the target.
    correction::verify(&rig, &mut a, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    let v = a.correction.as_ref().unwrap().verification.clone().unwrap();
    assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    assert_eq!(v.speakers.len(), 2);
    let r = v.speakers.iter().find(|s| s.output_id == "R").unwrap();
    let [first, predicted, measured] = r.deviation_db;
    assert!(first > 1.5, "{first}");
    assert!(predicted < first / 2.0, "{predicted} vs {first}");
    assert!(measured < first / 2.0, "{measured} vs {first}");
    assert!(r.from_prediction_db < 0.5, "{}", r.from_prediction_db);
    // The confirmation played through the filters; every capture before held them off
    // only when there were filters to hold off.
    assert_eq!(*rig.bypassed.lock().unwrap(), [false, false, false, false]);

    correction::apply(&rig, &mut a, Target::Original).unwrap();
    assert_eq!(state(&rig, &a), Live::Original);
    for id in ["L", "R"] {
        assert_eq!(rig.hash(id), None);
    }
    let c = a.correction.as_ref().unwrap();
    assert!(c.applied.is_none() && c.verification.is_none() && !c.uncertain);
    // Saved with the alignment and read back, curves included.
    let loaded = alignment::load(&a.directory).unwrap();
    let c = loaded.correction.unwrap();
    assert_eq!(c.curves.len(), 2);
    assert_eq!(c.live_state(&live(&rig)), Live::Original);
}

#[test]
fn undo_puts_a_file_filter_back_and_a_save_reads_as_saved() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    // L plays a file chosen in Studio: its taps are read back and kept.
    let file = root.path().join("studio-L.wav");
    let taps = vec![0.9_f32, 0.05, -0.02];
    rig.load_file("L", &file, &taps);
    let file_hash = protocol::taps_hash_hex(&taps);
    correction::apply(&rig, &mut a, Target::Corrected).unwrap();
    let c = a.correction.clone().unwrap();
    let l = c.previous.iter().find(|p| p.output_id == "L").unwrap();
    let kept = l.filter.as_ref().unwrap();
    assert_eq!(kept.origin, FilterOrigin::File);
    assert_eq!(kept.taps.hash, file_hash);
    assert_eq!(
        kept.source.as_deref(),
        Some(file.display().to_string().as_str())
    );
    assert!(kept.taps.file.starts_with(&c.directory));

    // Compared, L plays its file's taps again, R an identity.
    correction::apply(&rig, &mut a, Target::Compare).unwrap();
    assert_eq!(rig.hash("L"), Some(file_hash.clone()));
    assert_eq!(rig.hash("R"), Some(correction::identity_hash()));
    correction::apply(&rig, &mut a, Target::Corrected).unwrap();

    // Studio's Save writes every filter as a file with the same taps.
    rig.save();
    let now = live(&rig);
    assert_eq!(c.live_state(&now), Live::Corrected);
    assert_eq!(c.saved(&now), Saved::Saved);
    assert_eq!(rig.origin("R"), Some(FilterOrigin::File));
    alignment::reconcile(&mut a, &now).unwrap();

    // Undone even after the Save: L gets the file's taps back, R none.
    correction::apply(&rig, &mut a, Target::Original).unwrap();
    assert_eq!(rig.hash("L"), Some(file_hash));
    assert_eq!(rig.hash("R"), None);
    assert_eq!(state(&rig, &a), Live::Original);
    assert_eq!(
        a.correction.as_ref().unwrap().saved(&live(&rig)),
        Saved::NotApplied
    );
}

#[test]
fn an_unsaved_filter_from_elsewhere_or_a_changed_file_cannot_be_put_back() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    // Another client's upload: the Companion does not hold its taps.
    rig.set_output_filters(
        a.endpoint,
        live(&rig),
        vec![OutputFilter {
            output_id: "R".into(),
            taps: Some((48000, vec![0.5, 0.25])),
        }],
    )
    .unwrap();
    // Loaded by the next discovery.
    live(&rig);
    let writes = rig.filter_writes.load(Ordering::Relaxed);
    let e = correction::apply(&rig, &mut a, Target::Corrected).unwrap_err();
    assert!(e.contains("press Save in Studio first"), "{e}");
    assert_eq!(rig.filter_writes.load(Ordering::Relaxed), writes);
    assert!(!a.correction.as_ref().unwrap().uncertain);
    // A file that changed since Omniphony read it would not restore what plays.
    let file = root.path().join("changed.wav");
    rig.load_file("R", &file, &[0.7, 0.1]);
    write_wav(&file, &[0.6, 0.1]);
    let e = correction::apply(&rig, &mut a, Target::Corrected).unwrap_err();
    assert!(e.contains("changed since Omniphony read it"), "{e}");
    assert_eq!(state(&rig, &a), Live::Original);
}

#[test]
fn a_write_another_client_overtook_changes_nothing_and_is_not_uncertain() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    rig.interleave.store(true, Ordering::Relaxed);
    let e = correction::apply(&rig, &mut a, Target::Corrected).unwrap_err();
    assert!(e.contains("stale_revision"), "{e}");
    assert_eq!(rig.filter_writes.load(Ordering::Relaxed), 0);
    // Read back: still the original, so nothing is uncertain and it can be tried again.
    let c = a.correction.as_ref().unwrap();
    assert!(!c.uncertain && c.applied.is_none());
    assert_eq!(state(&rig, &a), Live::Original);
    correction::apply(&rig, &mut a, Target::Corrected).unwrap();
    assert_eq!(state(&rig, &a), Live::Corrected);
}

#[test]
fn an_unacknowledged_write_is_settled_by_reading_the_renderer_back() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    // The filters went through, the reply did not: read back, it is applied.
    rig.lose_filter_ack.store(true, Ordering::Relaxed);
    assert!(correction::apply(&rig, &mut a, Target::Corrected).is_err());
    let c = a.correction.as_ref().unwrap();
    assert!(!c.uncertain, "settled by the readback");
    assert!(c.applied.is_some());
    assert_eq!(state(&rig, &a), Live::Corrected);

    // This time the renderer cannot be read back either: uncertain, and nothing
    // else is written until a refresh shows where it went.
    rig.lose_filter_ack.store(true, Ordering::Relaxed);
    rig.blind_after_lost_ack.store(true, Ordering::Relaxed);
    assert!(correction::apply(&rig, &mut a, Target::Original).is_err());
    assert!(a.correction.as_ref().unwrap().uncertain);
    let saved = alignment::load(&a.directory).unwrap();
    assert!(saved.correction.as_ref().unwrap().uncertain, "persisted");
    rig.unreadable.store(false, Ordering::Relaxed);
    rig.blind_after_lost_ack.store(false, Ordering::Relaxed);
    for target in [Target::Corrected, Target::Compare, Target::Original] {
        let e =
            correction::apply_readiness(&a, &live(&rig), protocol::VERSION, target).unwrap_err();
        assert!(e.contains("Refresh"), "{e}");
    }
    // The refresh: the Undo had gone through.
    assert!(alignment::reconcile(&mut a, &live(&rig)).unwrap());
    let c = a.correction.as_ref().unwrap();
    assert!(!c.uncertain && c.applied.is_none());
    assert_eq!(state(&rig, &a), Live::Original);
}

#[test]
fn an_older_renderer_is_refused_before_anything_is_written() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    rig.version.store(7, Ordering::Relaxed);
    let e = correction::apply(&rig, &mut a, Target::Corrected).unwrap_err();
    assert!(e.contains("protocol 8"), "{e}");
    assert_eq!(rig.filter_writes.load(Ordering::Relaxed), 0);
    assert!(!a.correction.as_ref().unwrap().uncertain);
    // Computing still works and says the filters can only be loaded in Studio.
    a.correction = None;
    let r = correction::readiness(&a, Some(&live(&rig)), Some(7));
    assert!(r.blocker.is_none());
    assert!(
        r.warnings.iter().any(|w| w.contains("protocol 8")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn speakers_with_filters_are_measured_without_them_and_filtered_takes_are_left_out() {
    let root = tempfile::tempdir().unwrap();
    let rig = Rig::new(root.path());
    let mut a = computed(&rig, root.path());
    correction::apply(&rig, &mut a, Target::Corrected).unwrap();
    // A position measured while the correction plays holds the filters off.
    positions::measure(&rig, &mut a, 1, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!(rig.bypassed.lock().unwrap().last(), Some(&true));
    assert_eq!(a.positions[1].filtered_outputs, Some(vec![]));
    // So does a new alignment.
    let b = acquire(&rig, root.path());
    assert_eq!(b.filtered_outputs, Some(vec![]));
    assert!(
        rig.bypassed
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(2)
            .all(|b| *b)
    );
    // A session from before, measured through the filters, designs nothing from them.
    let mut old = b.clone();
    old.filtered_outputs = None;
    old.snapshot = live(&rig);
    let measured = companion_service::room_correction::measurements(&old);
    for (output, left_out) in measured {
        assert!(output.measurements.is_empty());
        assert!(
            left_out[0].contains("through its correction filter"),
            "{left_out:?}"
        );
    }
    // Computing again while applied would record the correction as the original.
    let r = correction::readiness(&a, Some(&live(&rig)), Some(protocol::VERSION));
    assert!(
        r.blocker
            .unwrap()
            .contains("Undo the applied correction first")
    );
}
