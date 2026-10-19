use companion_service::{
    controller::{Backend, Controller, Job},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use measurement_capture::{InputConfiguration, InputDevice};
use omniphony_adapter::{Snapshot, protocol::Output};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Bench {
    calls: AtomicUsize,
    finished: AtomicBool,
    discover_fails: AtomicBool,
    inputs_fail: AtomicBool,
    blocked: AtomicBool,
    requests: Mutex<Vec<MeasureRequest>>,
    batch_outputs: Mutex<Vec<String>>,
}
impl Bench {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            discover_fails: AtomicBool::new(false),
            inputs_fail: AtomicBool::new(false),
            blocked: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            batch_outputs: Mutex::new(Vec::new()),
        }
    }
}
fn snapshot() -> Snapshot {
    Snapshot {
        instance_id: "test".into(),
        revision: "test:1".into(),
        sample_rate: 48000,
        master_gain_db: 0.0,
        max_delay_ms: 100.0,
        unavailable_reason: None,
        outputs: ["Left", "Right", "Sub"]
            .iter()
            .enumerate()
            .map(|(i, id)| Output {
                id: (*id).into(),
                name: (*id).into(),
                index: i as u32,
                gain_db: -2.0,
                delay_ms: i as f32,
                muted: i == 2,
                freq_low_hz: None,
                freq_high_hz: (i == 2).then_some(120.0),
            })
            .collect(),
    }
}
impl Backend for Bench {
    fn manual_levels(
        &self,
        _request: companion_service::manual_level::Request,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(companion_service::manual_level::Reading),
    ) -> Result<(), String> {
        update(companion_service::manual_level::Reading {
            phase: companion_service::manual_level::Phase::Listening,
            level: Some(calibration_core::manual_level::estimate(0.0101, 0.0001, -2.0).unwrap()),
            remaining_seconds: 55,
            captured_at: Instant::now(),
        });
        while !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        self.finished.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn check_input(
        &self,
        request: companion_service::microphone::InputRequest,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(measurement_capture::input::InputLevel),
    ) -> Result<(), String> {
        assert_eq!(request.input_channel, 2);
        update(measurement_capture::input::InputLevel::analyze(
            &[0.02, -0.02],
            0,
        ));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cancel.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        self.finished.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn measure_batch(
        &self,
        request: MeasureRequest,
        outputs: Vec<String>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, MeasureProgress),
    ) -> Result<companion_service::batch::BatchOutcome, String> {
        *self.batch_outputs.lock().unwrap() = outputs.clone();
        let outcome = self.measure(request.clone(), cancel, &mut |p| progress(0, p))?;
        Ok(companion_service::batch::BatchOutcome {
            directory: request.out_dir,
            takes: if outcome.cancelled {
                vec![]
            } else {
                outputs
                    .into_iter()
                    .map(|id| (id, outcome.clone()))
                    .collect()
            },
            cancelled: outcome.cancelled,
            error: outcome.cancelled.then(|| "cancelled".into()),
        })
    }
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
        if self.inputs_fail.load(Ordering::Relaxed) {
            return Err("interface disconnected".into());
        }
        Ok(vec![InputDevice {
            name: "Test interface".into(),
            description: "Test interface".into(),
            channel_names: Vec::new(),
            is_monitor: false,
            configurations: vec![InputConfiguration {
                channels: 2,
                min_sample_rate: 44100,
                max_sample_rate: 48000,
                sample_format: "f32".into(),
            }],
        }])
    }
    fn discover(&self, _: SocketAddr) -> Result<Snapshot, String> {
        if self.discover_fails.load(Ordering::Relaxed) {
            Err("renderer disconnected".into())
        } else {
            Ok(snapshot())
        }
    }
    fn measure(
        &self,
        request: MeasureRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.requests.lock().unwrap().push(request.clone());
        progress(MeasureProgress::OpeningInput);
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.blocked.load(Ordering::Relaxed)
            && !cancel.load(Ordering::Relaxed)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        self.finished.store(true, Ordering::Relaxed);
        Ok(TakeOutcome {
            diagnostics: None,
            directory: request.out_dir,
            accepted: false,
            cancelled: cancel.load(Ordering::Relaxed),
            rejection_reason: Some("Test backend: no recording".into()),
            timing: None,
            impulse: None,
            impulse_error: None,
        })
    }
}
fn settled(controller: &mut Controller) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while controller.busy() {
        controller.poll();
        assert!(Instant::now() < deadline, "background job did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn ready(bench: Arc<Bench>) -> Controller {
    let mut c = Controller::new(bench);
    c.refresh_inputs();
    settled(&mut c);
    c.connect("127.0.0.1:9000");
    settled(&mut c);
    c.settings.device = Some("Test interface".into());
    c.settings.input_channel = 2;
    c
}

#[test]
fn selection_and_speaker_constraints_gate_recording() {
    let b = Arc::new(Bench::new());
    let mut c = ready(b.clone());
    assert!(c.can_measure("Right").is_ok());
    assert!(c.can_measure("Sub").unwrap_err().contains("muted"));
    c.settings.input_channel = 3;
    assert!(c.can_measure("Right").is_err());
    c.start("Right");
    assert_eq!(b.calls.load(Ordering::Relaxed), 0);
    c.settings.input_channel = 2;
    c.settings.sample_rate = 96000;
    assert!(c.can_measure("Right").is_err());
    c.settings.sample_rate = 48000;
    c.settings.reference_output = Some("Sub".into());
    assert!(c.can_measure("Right").is_err());
    c.settings.reference_output = Some("Left".into());
    c.settings.device = None;
    assert!(c.can_measure("Right").is_err());
}

#[test]
fn microphone_check_needs_no_renderer_blocks_playback_cancels_and_forgets_old_source() {
    let b = Arc::new(Bench::new());
    let mut c = ready(b.clone());
    c.disconnect();
    c.check_input();
    assert_eq!(c.state.job, Some(Job::InputCheck));
    let deadline = Instant::now() + Duration::from_secs(1);
    while c.input_check().unwrap().level.is_none() {
        c.poll();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(c.input_check().unwrap().level.unwrap().peak_dbfs.is_some());
    c.start("Left");
    assert_eq!(b.calls.load(Ordering::Relaxed), 0);
    c.cancel();
    // The worker finishes without waiting for UI polling.
    let deadline = Instant::now() + Duration::from_secs(1);
    while !b.finished.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    settled(&mut c);
    assert!(c.input_check().is_some());
    c.settings.input_channel = 1;
    assert!(c.input_check().is_none());
    c.poll();
    c.settings.input_channel = 2;
    assert!(c.input_check().is_none(), "old activity must not reappear");
    assert!(c.state.takes.is_empty());
}

#[test]
fn startup_refresh_retains_missing_input_channel_and_reference_without_starting_audio() {
    let b = Arc::new(Bench::new());
    let mut c = Controller::new(b.clone());
    c.settings.device = Some("Missing interface".into());
    c.settings.input_channel = 2;
    c.settings.reference_output = Some("Missing speaker".into());
    c.initialize(Some("127.0.0.1:9003".into()));
    settled(&mut c);
    assert_eq!(c.state.endpoint, Some("127.0.0.1:9003".parse().unwrap()));
    assert_eq!(c.settings.device.as_deref(), Some("Missing interface"));
    assert_eq!(c.settings.input_channel, 2);
    assert_eq!(
        c.settings.reference_output.as_deref(),
        Some("Missing speaker")
    );
    assert!(c.can_measure("Right").unwrap_err().contains("unavailable"));
    c.settings.device = Some("Test interface".into());
    b.inputs_fail.store(true, Ordering::Relaxed);
    c.refresh_inputs();
    settled(&mut c);
    assert_eq!(c.settings.device.as_deref(), Some("Test interface"));
    assert!(c.selected_input().is_none());
    b.inputs_fail.store(false, Ordering::Relaxed);
    c.refresh_inputs();
    settled(&mut c);
    assert!(c.selected_input().is_some());
    assert!(c.can_measure("Right").unwrap_err().contains("absent"));
    c.settings.reference_output = Some("Sub".into());
    c.connect("127.0.0.1:9003");
    settled(&mut c);
    assert_eq!(c.settings.reference_output.as_deref(), Some("Sub"));
    assert!(c.can_measure("Right").unwrap_err().contains("muted"));
    assert_eq!(b.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn a_fresh_start_chooses_input_rate_reference_and_names_but_keeps_saved_choices() {
    let b = Arc::new(Bench::new());
    let mut c = Controller::new(b.clone());
    c.settings.device = None;
    c.settings.input_channel = 2;
    c.settings.sample_rate = 44100;
    c.settings.reference_output = None;
    c.settings.microphone_id.clear();
    c.settings.position_id.clear();
    c.initialize(Some("127.0.0.1:9000".into()));
    settled(&mut c);
    assert_eq!(c.settings.device.as_deref(), Some("Test interface"));
    assert_eq!(
        (c.settings.input_channel, c.settings.sample_rate),
        (1, 48000)
    );
    assert_eq!(c.settings.reference_output.as_deref(), Some("Left"));
    assert_eq!(
        (
            c.settings.microphone_id.as_str(),
            c.settings.position_id.as_str()
        ),
        ("mic-1", "main-seat")
    );
    assert!(c.state.defaults.device && c.state.defaults.reference);
    assert!(c.can_measure("Right").is_ok());
    // Choices made later, or restored from settings, survive every refresh.
    c.settings.reference_output = Some("Right".into());
    c.settings.position_id = "sofa".into();
    c.refresh_inputs();
    settled(&mut c);
    c.connect("127.0.0.1:9000");
    settled(&mut c);
    assert_eq!(c.settings.reference_output.as_deref(), Some("Right"));
    assert_eq!(c.settings.position_id, "sofa");
    let mut saved = Controller::new(b.clone());
    saved.settings.device = Some("Test interface".into());
    saved.settings.sample_rate = 44100;
    saved.initialize(Some("127.0.0.1:9000".into()));
    settled(&mut saved);
    assert_eq!(saved.settings.sample_rate, 44100);
    assert!(!saved.state.defaults.device);
    assert_eq!(b.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn estimates_follow_the_checked_speakers_and_the_running_job() {
    use companion_service::duration;
    let b = Arc::new(Bench::new());
    let mut c = ready(b);
    let sweep = c.settings.sweep;
    assert_eq!(c.batch_estimate(), Some(duration::batch(sweep, 2)));
    assert_eq!(c.alignment_estimate(), Some(duration::alignment(sweep, 2)));
    c.excluded_outputs.insert("Left".into());
    assert_eq!(c.batch_estimate(), Some(duration::batch(sweep, 1)));
    assert_eq!(c.alignment_estimate(), None, "alignment needs two speakers");
    assert_eq!(c.remaining(), None);
    c.state.job = Some(Job::Alignment {
        output_ids: vec!["Left".into(), "Right".into()],
        current: 1,
        pass: 1,
    });
    c.state.progress = Some(MeasureProgress::Analyzing);
    assert_eq!(
        c.remaining(),
        Some(duration::alignment(sweep, 2).analysis_per_take)
    );
    c.state.job = Some(Job::Measure {
        output_id: "Right".into(),
    });
    c.state.progress = Some(MeasureProgress::OpeningInput);
    assert_eq!(c.remaining(), Some(c.take_estimate().total));
    c.state.job = None;
    c.state.progress = None;
}

#[test]
fn batch_uses_checked_eligible_outputs_and_preserves_fixed_settings() {
    let b = Arc::new(Bench::new());
    let mut c = ready(b.clone());
    assert_eq!(c.batch_outputs().unwrap(), ["Left", "Right"]);
    c.excluded_outputs.insert("Left".into());
    assert_eq!(c.batch_outputs().unwrap(), ["Right"]);
    c.start_batch();
    settled(&mut c);
    assert_eq!(*b.batch_outputs.lock().unwrap(), ["Right"]);
    assert_eq!(b.requests.lock().unwrap()[0].reference_output, "Left");
    assert!(c.current_take("Right").is_some());
    assert!(c.current_take("Left").is_none());
}

#[test]
fn batch_stop_and_drop_cancel_the_worker_without_painting() {
    for drop_controller in [false, true] {
        let b = Arc::new(Bench::new());
        b.blocked.store(true, Ordering::Relaxed);
        let mut c = ready(b.clone());
        c.start_batch();
        assert!(c.measuring());
        if drop_controller {
            drop(c);
        } else {
            c.cancel();
            settled(&mut c);
            assert!(c.state.takes.is_empty());
            assert!(c.state.error.as_ref().unwrap().contains("Saved batch"));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while !b.finished.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(b.finished.load(Ordering::Relaxed));
    }
}

#[test]
fn per_speaker_measurement_is_single_owner_and_cancellation_needs_no_paint_loop() {
    let b = Arc::new(Bench::new());
    b.blocked.store(true, Ordering::Relaxed);
    let mut c = ready(b.clone());
    c.start("Right");
    assert!(matches!(&c.state.job, Some(Job::Measure { output_id }) if output_id == "Right"));
    c.start("Left");
    c.cancel();
    // The backend exits on the cancellation token even if the UI stops polling.
    std::thread::sleep(Duration::from_millis(20));
    settled(&mut c);
    assert_eq!(b.calls.load(Ordering::Relaxed), 1);
    let requests = b.requests.lock().unwrap();
    assert_eq!(requests[0].input_channel, 2);
    assert_eq!(requests[0].reference_output, "Left");
    assert_eq!(requests[0].output, "Right");
    assert_eq!(requests[0].snapshot.revision, "test:1");
    assert!(
        c.current_take("Right")
            .unwrap()
            .outcome
            .as_ref()
            .unwrap()
            .cancelled
    );
    assert!(c.current_take("Left").is_none());
}

#[test]
fn failed_refresh_discards_snapshot_and_old_results_do_not_follow_new_settings() {
    let b = Arc::new(Bench::new());
    let mut c = ready(b.clone());
    c.start("Right");
    settled(&mut c);
    assert!(c.current_take("Right").is_some());
    c.settings.position_id = "another-seat".into();
    assert!(c.current_take("Right").is_none());
    assert_eq!(c.state.takes.len(), 1);
    b.discover_fails.store(true, Ordering::Relaxed);
    c.connect("127.0.0.1:9001");
    assert!(c.state.snapshot.is_none());
    settled(&mut c);
    assert!(c.can_measure("Right").is_err());
    assert_eq!(
        c.state.connection_error.as_deref(),
        Some("renderer disconnected")
    );
}

#[test]
fn editing_endpoint_or_selecting_nonlocal_endpoint_cannot_keep_a_stale_connection() {
    let mut c = ready(Arc::new(Bench::new()));
    c.disconnect();
    assert!(c.state.snapshot.is_none());
    assert!(c.can_measure("Right").is_err());
    c.connect("192.0.2.1:9000");
    assert!(!c.busy());
    assert!(c.state.connection_error.is_some());
}

#[test]
fn take_paths_are_unique_and_do_not_use_output_names() {
    let b = Arc::new(Bench::new());
    let mut c = ready(b.clone());
    c.start("Right");
    settled(&mut c);
    c.start("Right");
    settled(&mut c);
    let requests = b.requests.lock().unwrap();
    assert_ne!(requests[0].out_dir, requests[1].out_dir);
    assert!(
        requests[0]
            .out_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("take-")
    );
}

#[test]
fn dropping_controller_cancels_its_worker() {
    let b = Arc::new(Bench::new());
    b.blocked.store(true, Ordering::Relaxed);
    let mut c = ready(b.clone());
    c.start("Right");
    drop(c);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !b.finished.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(b.finished.load(Ordering::Relaxed));
}

#[test]
fn offline_review_does_not_require_a_connection_or_trigger_capture() {
    let b = Arc::new(Bench::new());
    let mut c = Controller::new(b.clone());
    c.review_saved("synthetic-take");
    assert_eq!(c.state.job, Some(Job::Review));
    c.start("Right");
    settled(&mut c);
    assert_eq!(b.calls.load(Ordering::Relaxed), 0);
    assert!(c.state.snapshot.is_none());
    assert!(
        c.state
            .review_error
            .as_ref()
            .unwrap()
            .contains("unavailable in this backend")
    );
    c.review_saved("");
    assert!(!c.busy());
    assert!(c.state.review_error.is_some());
}

#[test]
fn manual_levels_are_cancellable_keep_reference_identity_and_never_capture_takes() {
    let b = Arc::new(Bench::new());
    let mut c = ready(b.clone());
    c.start_manual_levels("Left", -30.0);
    assert!(c.cancellable());
    let deadline = Instant::now() + Duration::from_secs(1);
    while c.state.manual_run.as_ref().unwrap().reading.is_none() {
        c.poll();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(c.can_set_manual_reference(-30.0));
    c.set_manual_reference(-30.0);
    assert!(c.manual_reference(-30.0).is_some());
    assert!(c.manual_reference(-29.0).is_none());
    let seat = c.settings.position_id.clone();
    c.settings.position_id.push_str("-moved");
    assert!(!c.can_set_manual_reference(-30.0));
    assert!(c.manual_reference(-30.0).is_none());
    c.settings.position_id = seat;
    c.state
        .manual_run
        .as_mut()
        .unwrap()
        .reading
        .as_mut()
        .unwrap()
        .captured_at = Instant::now() - Duration::from_secs(1);
    assert!(!c.can_set_manual_reference(-30.0));
    c.start("Right");
    assert_eq!(b.calls.load(Ordering::Relaxed), 0);
    c.cancel();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !b.finished.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    settled(&mut c);
    assert!(c.state.manual_run.as_ref().unwrap().completed);
    assert!(!c.can_set_manual_reference(-30.0));
    assert!(c.state.takes.is_empty());
    assert!(c.state.analyses.is_empty());
    b.finished.store(false, Ordering::Relaxed);
    c.start_manual_levels("Right", -30.0);
    assert!(c.busy());
    drop(c);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !b.finished.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Drop cancels even without a final paint/event-poll cycle.
}

#[test]
fn manual_levels_reject_monitor_bass_only_muted_missing_or_excessive_drive() {
    let mut c = ready(Arc::new(Bench::new()));
    c.start_manual_levels("Sub", -30.0);
    assert!(!c.busy());
    c.state.snapshot.as_mut().unwrap().outputs[2].muted = false;
    assert!(c.manual_context(-30.0).unwrap().output("Sub").is_err());
    c.state.inputs[0].is_monitor = true;
    assert!(c.manual_context(-30.0).is_err());
    c.state.inputs[0].is_monitor = false;
    assert!(c.manual_context(f32::NAN).is_err());
    c.state.snapshot.as_mut().unwrap().outputs[0].gain_db = 20.0;
    assert!(c.manual_context(-12.0).is_err());
    c.disconnect();
    assert!(c.manual_context(-30.0).is_err());
}
