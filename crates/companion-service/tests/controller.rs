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
    blocked: AtomicBool,
    requests: Mutex<Vec<MeasureRequest>>,
}
impl Bench {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            discover_fails: AtomicBool::new(false),
            blocked: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
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
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
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
            directory: request.out_dir,
            accepted: false,
            cancelled: cancel.load(Ordering::Relaxed),
            rejection_reason: Some("Test backend: no recording".into()),
            timing: None,
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
