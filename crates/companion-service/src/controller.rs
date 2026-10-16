//! Frontend-neutral state and background jobs. No device/network I/O in paint calls.
use crate::{
    measurement::{self, MeasureProgress, MeasureRequest, TakeOutcome},
    review::{self, SavedReview},
};
use measurement_capture::InputDevice;
use omniphony_adapter::{Client, Snapshot, Sweep};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub trait Backend: Send + Sync + 'static {
    fn review(&self, directory: PathBuf) -> Result<SavedReview, String> {
        let _ = directory;
        Err("Saved-take review is unavailable in this backend.".into())
    }
    fn inputs(&self) -> Result<Vec<InputDevice>, String>;
    fn discover(&self, endpoint: SocketAddr) -> Result<Snapshot, String>;
    fn measure(
        &self,
        request: MeasureRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String>;
}
pub struct LiveBackend;
impl Backend for LiveBackend {
    fn review(&self, directory: PathBuf) -> Result<SavedReview, String> {
        review::saved_take(&directory).map_err(|e| e.to_string())
    }
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
        measurement_capture::devices().map_err(|e| e.to_string())
    }
    fn discover(&self, endpoint: SocketAddr) -> Result<Snapshot, String> {
        Client::connect(endpoint, Duration::from_secs(2))
            .and_then(|mut client| client.discover())
            .map_err(|e| e.to_string())
    }
    fn measure(
        &self,
        request: MeasureRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String> {
        measurement::measure(request, cancel, progress).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub device: Option<String>,
    pub input_channel: u16,
    pub sample_rate: u32,
    pub microphone_id: String,
    pub position_id: String,
    pub reference_output: Option<String>,
    pub calibration_path: String,
    pub output_root: String,
    pub sweep: Sweep,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            device: None,
            input_channel: 1,
            sample_rate: 48000,
            microphone_id: "mic-1".into(),
            position_id: "reference-seat".into(),
            reference_output: None,
            calibration_path: String::new(),
            output_root: "sessions".into(),
            sweep: Sweep {
                duration_ms: 1000,
                peak_dbfs: -30.0,
                start_hz: 200.0,
                end_hz: 10000.0,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Review,
    Inputs,
    Discover,
    Measure { output_id: String },
}

#[derive(Debug, Clone)]
pub struct Take {
    pub output_id: String,
    pub revision: String,
    pub settings: Settings,
    pub outcome: Result<TakeOutcome, String>,
}

#[derive(Default)]
pub struct State {
    pub review: Option<SavedReview>,
    pub review_error: Option<String>,
    pub inputs: Vec<InputDevice>,
    pub input_error: Option<String>,
    pub endpoint: Option<SocketAddr>,
    pub snapshot: Option<Snapshot>,
    pub connection_error: Option<String>,
    pub job: Option<Job>,
    pub progress: Option<MeasureProgress>,
    pub stopping: bool,
    pub takes: Vec<Take>,
    pub error: Option<String>,
}
enum Event {
    Reviewed(Box<Result<SavedReview, String>>),
    Inputs(Result<Vec<InputDevice>, String>),
    Discovered(SocketAddr, Result<Snapshot, String>),
    Progress(MeasureProgress),
    Measured(Box<Take>),
}
pub struct Controller {
    pub state: State,
    pub settings: Settings,
    backend: Arc<dyn Backend>,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    cancelled: Arc<AtomicBool>,
    sequence: u64,
}
impl Default for Controller {
    fn default() -> Self {
        Self::new(Arc::new(LiveBackend))
    }
}
impl Controller {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            state: State::default(),
            settings: Settings::default(),
            backend,
            sender,
            receiver,
            cancelled: Arc::new(AtomicBool::new(false)),
            sequence: 0,
        }
    }
    pub fn busy(&self) -> bool {
        self.state.job.is_some()
    }
    pub fn measuring(&self) -> bool {
        matches!(self.state.job, Some(Job::Measure { .. }))
    }

    /// Offline analysis uses the same worker boundary as acquisition. No renderer needed.
    pub fn review_saved(&mut self, directory: &str) {
        if self.busy() {
            return;
        }
        self.state.review = None;
        self.state.review_error = None;
        if directory.trim().is_empty() {
            self.state.review_error = Some("Enter a saved take folder.".into());
            return;
        }
        self.state.job = Some(Job::Review);
        let path = PathBuf::from(directory.trim());
        let backend = self.backend.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| backend.review(path));
            let _ = sender.send(Event::Reviewed(Box::new(result)));
        });
    }

    pub fn refresh_inputs(&mut self) {
        if self.busy() {
            return;
        }
        self.state.job = Some(Job::Inputs);
        self.state.input_error = None;
        let backend = self.backend.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| backend.inputs());
            let _ = sender.send(Event::Inputs(result));
        });
    }
    /// Forget a displayed snapshot when the connection address is edited.
    pub fn disconnect(&mut self) {
        if self.busy() {
            return;
        }
        self.state.snapshot = None;
        self.state.endpoint = None;
        self.state.connection_error = None;
    }
    pub fn connect(&mut self, address: &str) {
        if self.busy() {
            return;
        }
        self.disconnect();
        let endpoint = match address.parse::<SocketAddr>() {
            Ok(endpoint) if endpoint.ip().is_loopback() => endpoint,
            _ => {
                self.state.connection_error =
                    Some("Use a local address, such as 127.0.0.1:9000.".into());
                return;
            }
        };
        self.state.job = Some(Job::Discover);
        let backend = self.backend.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| backend.discover(endpoint));
            let _ = sender.send(Event::Discovered(endpoint, result));
        });
    }
    pub fn input_channels(&self) -> u16 {
        self.selected_input()
            .into_iter()
            .flat_map(|d| &d.configurations)
            .filter(|c| {
                supported_format(&c.sample_format)
                    && c.channels <= 64
                    && c.min_sample_rate <= self.settings.sample_rate
                    && c.max_sample_rate >= self.settings.sample_rate
            })
            .map(|c| c.channels.min(64))
            .max()
            .unwrap_or(0)
    }
    pub fn selected_input(&self) -> Option<&InputDevice> {
        self.state
            .inputs
            .iter()
            .find(|d| Some(&d.name) == self.settings.device.as_ref())
    }
    pub fn can_measure(&self, output_id: &str) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the current operation to finish.".into());
        }
        let snapshot = self
            .state
            .snapshot
            .as_ref()
            .ok_or("Connect to Omniphony to load its speakers.")?;
        if self.state.endpoint.is_none() {
            return Err("Connect to Omniphony first.".into());
        }
        if let Some(reason) = &snapshot.unavailable_reason {
            return Err(reason.clone());
        }
        let device = self.selected_input().ok_or("Choose a microphone input.")?;
        if self
            .state
            .inputs
            .iter()
            .filter(|d| d.name == device.name)
            .count()
            != 1
        {
            return Err("This input name is ambiguous. Give the interface a unique name.".into());
        }
        if self.settings.input_channel == 0 || self.settings.input_channel > self.input_channels() {
            return Err(
                "Choose an input channel and sample rate supported by the interface.".into(),
            );
        }
        for id in [&self.settings.microphone_id, &self.settings.position_id] {
            if id.trim().is_empty() || id.len() > 128 {
                return Err("Microphone and position names need 1–128 bytes.".into());
            }
        }
        if self.settings.output_root.trim().is_empty() {
            return Err("Choose a folder for recordings.".into());
        }
        self.settings.sweep.validate(snapshot.sample_rate)?;
        if !(8000..=192000).contains(&self.settings.sample_rate)
            || self.settings.sweep.end_hz > self.settings.sample_rate as f32 * 0.45
        {
            return Err(
                "The sweep's upper frequency is too high for the input sample rate.".into(),
            );
        }
        let reference = self
            .settings
            .reference_output
            .as_deref()
            .ok_or("Choose a timing reference speaker.")?;
        for id in [output_id, reference] {
            let output = snapshot
                .outputs
                .iter()
                .find(|o| o.id == id)
                .ok_or("Speaker is absent from this configuration. Refresh the connection.")?;
            if output.muted {
                return Err(format!(
                    "{} is muted. Unmute it in Omniphony and refresh.",
                    output.name
                ));
            }
            if self.settings.sweep.start_hz < output.freq_low_hz.unwrap_or(20.0)
                || self.settings.sweep.end_hz > output.freq_high_hz.unwrap_or(f32::MAX)
            {
                return Err(format!(
                    "The sweep must fit {}'s frequency range.",
                    output.name
                ));
            }
            if self.settings.sweep.peak_dbfs + snapshot.master_gain_db + output.gain_db > -6.0 {
                return Err(format!(
                    "Lower the sweep level for {}'s current gain.",
                    output.name
                ));
            }
        }
        Ok(())
    }

    pub fn start(&mut self, output_id: &str) {
        if let Err(error) = self.can_measure(output_id) {
            self.state.error = Some(error);
            return;
        }
        let snapshot = self.state.snapshot.as_ref().expect("validated").clone();
        let settings = self.settings.clone();
        self.sequence += 1;
        // Output names never become paths. create_dir refuses any collision.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let out_dir =
            PathBuf::from(&settings.output_root).join(format!("take-{stamp}-{}", self.sequence));
        let request = MeasureRequest {
            endpoint: self.state.endpoint.expect("validated"),
            snapshot: snapshot.clone(),
            device: settings.device.clone().expect("validated"),
            input_channel: settings.input_channel,
            sample_rate: settings.sample_rate,
            microphone_id: settings.microphone_id.clone(),
            position_id: settings.position_id.clone(),
            output: output_id.into(),
            reference_output: settings.reference_output.clone().expect("validated"),
            calibration: (!settings.calibration_path.trim().is_empty())
                .then(|| PathBuf::from(&settings.calibration_path)),
            sweep: settings.sweep,
            out_dir,
        };
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.state.job = Some(Job::Measure {
            output_id: output_id.into(),
        });
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.state.error = None;
        let backend = self.backend.clone();
        let sender = self.sender.clone();
        let cancel = self.cancelled.clone();
        let output_id = output_id.to_owned();
        std::thread::spawn(move || {
            let result = catch_job(|| {
                backend.measure(request, &cancel, &mut |progress| {
                    let _ = sender.send(Event::Progress(progress));
                })
            });
            let _ = sender.send(Event::Measured(Box::new(Take {
                output_id,
                revision: snapshot.revision,
                settings,
                outcome: result,
            })));
        });
    }
    pub fn cancel(&mut self) {
        if self.measuring() {
            self.state.stopping = true;
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
    /// Reading events is nonblocking. A slow/minimized frontend never owns keepalives.
    pub fn poll(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Reviewed(result) => {
                    match *result {
                        Ok(review) => self.state.review = Some(review),
                        Err(error) => self.state.review_error = Some(error),
                    }
                    self.state.job = None;
                }

                Event::Inputs(result) => {
                    match result {
                        Ok(inputs) => {
                            self.state.inputs = inputs;
                            if self.selected_input().is_none() {
                                self.settings.device = None;
                            }
                        }
                        Err(error) => {
                            self.state.inputs.clear();
                            self.settings.device = None;
                            self.state.input_error = Some(error);
                        }
                    }
                    self.state.job = None;
                }
                Event::Discovered(endpoint, result) => {
                    match result {
                        Ok(snapshot) => {
                            if !snapshot.outputs.iter().any(|o| {
                                Some(&o.id) == self.settings.reference_output.as_ref() && !o.muted
                            }) {
                                self.settings.reference_output = snapshot
                                    .outputs
                                    .iter()
                                    .find(|o| {
                                        !o.muted
                                            && self.settings.sweep.start_hz
                                                >= o.freq_low_hz.unwrap_or(20.0)
                                            && self.settings.sweep.end_hz
                                                <= o.freq_high_hz.unwrap_or(f32::MAX)
                                    })
                                    .map(|o| o.id.clone());
                            }
                            self.state.endpoint = Some(endpoint);
                            self.state.snapshot = Some(snapshot);
                        }
                        Err(error) => {
                            self.state.snapshot = None;
                            self.state.connection_error = Some(error);
                        }
                    }
                    self.state.job = None;
                }
                Event::Progress(progress) => self.state.progress = Some(progress),
                Event::Measured(take) => {
                    if let Err(error) = &take.outcome {
                        self.state.error = Some(error.clone());
                    }
                    self.state.takes.push(*take);
                    if self.state.takes.len() > 100 {
                        self.state.takes.remove(0);
                    }
                    self.state.progress = None;
                    self.state.job = None;
                    self.state.stopping = false;
                }
            }
        }
    }
    pub fn current_take(&self, output_id: &str) -> Option<&Take> {
        let revision = &self.state.snapshot.as_ref()?.revision;
        self.state.takes.iter().rev().find(|t| {
            t.output_id == output_id && t.revision == *revision && t.settings == self.settings
        })
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
fn supported_format(format: &str) -> bool {
    matches!(format, "f32" | "i16" | "i32")
}
fn catch_job<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err("The background operation stopped unexpectedly. Reconnect before retrying.".into())
    })
}
