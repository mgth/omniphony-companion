//! Frontend-neutral state and background jobs. No device/network I/O in paint calls.
use crate::{
    alignment,
    batch::{self, BatchOutcome},
    manual_level,
    measurement::{self, MeasureProgress, MeasureRequest, TakeOutcome},
    microphone::{self, InputRequest},
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
    fn manual_levels(
        &self,
        request: manual_level::Request,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(manual_level::Reading),
    ) -> Result<(), String> {
        let _ = (request, cancel, update);
        Err("Manual speaker levels are unavailable in this backend.".into())
    }

    fn check_input(
        &self,
        request: InputRequest,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(measurement_capture::input::InputLevel),
    ) -> Result<(), String> {
        let _ = (request, cancel, update);
        Err("Microphone monitoring is unavailable in this backend.".into())
    }
    fn set_outputs(
        &self,
        endpoint: SocketAddr,
        expected: Snapshot,
        settings: Vec<omniphony_adapter::protocol::OutputSetting>,
    ) -> Result<Snapshot, String> {
        let _ = (endpoint, expected, settings);
        Err("Profile application is unavailable in this backend.".into())
    }
    fn measure_batch(
        &self,
        request: MeasureRequest,
        outputs: Vec<String>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, MeasureProgress),
    ) -> Result<BatchOutcome, String> {
        let _ = (request, outputs, cancel, progress);
        Err("Continuous batch capture is unavailable in this backend.".into())
    }
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
    fn manual_levels(
        &self,
        request: manual_level::Request,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(manual_level::Reading),
    ) -> Result<(), String> {
        manual_level::run(request, cancel, update).map_err(|e| e.to_string())
    }

    fn check_input(
        &self,
        request: InputRequest,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(measurement_capture::input::InputLevel),
    ) -> Result<(), String> {
        microphone::monitor(request, cancel, update).map_err(|e| e.to_string())
    }
    fn set_outputs(
        &self,
        endpoint: SocketAddr,
        expected: Snapshot,
        settings: Vec<omniphony_adapter::protocol::OutputSetting>,
    ) -> Result<Snapshot, String> {
        Client::connect(endpoint, Duration::from_secs(2))
            .and_then(|mut c| c.set_outputs(&expected, settings))
            .map_err(|e| e.to_string())
    }
    fn measure_batch(
        &self,
        request: MeasureRequest,
        outputs: Vec<String>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, MeasureProgress),
    ) -> Result<BatchOutcome, String> {
        batch::measure(request, outputs, cancel, progress).map_err(|e| e.to_string())
    }
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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// During an alignment, measure a speaker whose own interval clipped again in the
    /// same pass, with its output gain lowered by the overdrive plus margin; its level
    /// adds that gain back. The gain is restored right after.
    pub retake_clipped: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            device: None,
            input_channel: 1,
            sample_rate: 48000,
            microphone_id: crate::defaults::MICROPHONE_ID.into(),
            position_id: crate::defaults::POSITION_ID.into(),
            reference_output: None,
            calibration_path: String::new(),
            output_root: "sessions".into(),
            sweep: Sweep {
                duration_ms: 1000,
                peak_dbfs: -30.0,
                start_hz: 200.0,
                end_hz: 10000.0,
            },
            retake_clipped: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    UpdateProposal,
    ManualLevels {
        output_id: String,
    },
    InputCheck,
    Alignment {
        output_ids: Vec<String>,
        current: usize,
        pass: usize,
    },
    /// One speaker measured again into the current alignment, one take per pass.
    Remeasure {
        output_id: String,
        pass: usize,
    },
    ApplyAlignment,
    LoadAlignment,
    RestoreWorkspace,
    Review,
    Inputs,
    Discover,
    Measure {
        output_id: String,
    },
    Batch {
        output_ids: Vec<String>,
        current: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Take {
    pub output_id: String,
    pub revision: String,
    pub settings: Settings,
    pub outcome: Result<TakeOutcome, String>,
}

#[derive(Debug, Clone)]
pub struct InputCheck {
    pub request: InputRequest,
    pub level: Option<measurement_capture::input::InputLevel>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct State {
    pub analyses: crate::analysis_cache::Index,
    pub saved_snapshot: Option<Snapshot>,
    pub restored_index: Option<crate::workspace::WorkspaceIndex>,
    pub workspace_warnings: Vec<String>,
    pub workspace_ready: bool,
    pub workspace_imported: bool,
    pub workspace_revision: u64,
    pub input_check: Option<InputCheck>,
    pub manual_run: Option<manual_level::Run>,
    pub manual_reference: Option<manual_level::Reference>,
    pub alignment: Option<alignment::Alignment>,
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
    /// Setup values chosen automatically because none was saved.
    pub defaults: crate::defaults::Applied,
}
struct LoadedAlignment {
    alignment: alignment::Alignment,
    workspace: crate::workspace::RestoredWorkspace,
}

impl LoadedAlignment {
    fn read(alignment: alignment::Alignment) -> Result<Self, String> {
        let references = alignment
            .rows
            .iter()
            .flat_map(|r| {
                r.take_folders
                    .iter()
                    .map(|p| crate::workspace::TakeReference {
                        output_id: r.output_id.clone(),
                        revision: alignment.snapshot.revision.clone(),
                        settings: alignment.settings.clone(),
                        result: Ok(crate::workspace::absolute(p)),
                    })
            })
            .collect();
        let workspace = crate::workspace::restore(
            Some(crate::workspace::WorkspaceIndex {
                takes: references,
                ..Default::default()
            }),
            &alignment.settings,
        )?;
        Ok(Self {
            alignment,
            workspace,
        })
    }
}

/// The takes of a re-measure, and its retakes at a lowered gain.
type NewTakes = (Vec<(String, TakeOutcome)>, Vec<alignment::Retake>);
enum Event {
    ProposalUpdated(Box<Result<alignment::Alignment, String>>),
    ManualReading(manual_level::Reading),
    ManualFinished(Result<(), String>),
    PhysicalLevelsInvalidated(Box<alignment::Alignment>),
    WorkspaceRestored(Box<Result<crate::workspace::RestoredWorkspace, String>>),
    InputLevel(measurement_capture::input::InputLevel),
    InputChecked(Result<(), String>),
    AlignmentProgress(usize, usize, MeasureProgress),
    Aligned(Box<Result<alignment::Outcome, String>>),
    Remeasured(Box<alignment::Alignment>, Result<NewTakes, String>),
    AlignmentUpdated(
        Box<alignment::Alignment>,
        Result<(Snapshot, Vec<(String, TakeOutcome)>), String>,
    ),
    AlignmentLoaded(Box<Result<LoadedAlignment, String>>),
    Reviewed(Box<Result<SavedReview, String>>),
    Inputs(Result<Vec<InputDevice>, String>),
    Discovered(SocketAddr, Result<Snapshot, String>),
    Progress(MeasureProgress),
    Measured(Box<Take>),
    BatchProgress(usize, MeasureProgress),
    Batched(Settings, String, Result<BatchOutcome, String>),
}
pub struct Controller {
    pub state: State,
    pub settings: Settings,
    pub excluded_outputs: std::collections::BTreeSet<String>,
    backend: Arc<dyn Backend>,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    cancelled: Arc<AtomicBool>,
    sequence: u64,
    startup_endpoint: Option<String>,
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
            excluded_outputs: Default::default(),
            backend,
            sender,
            receiver,
            cancelled: Arc::new(AtomicBool::new(false)),
            sequence: 0,
            startup_endpoint: None,
        }
    }
    pub fn busy(&self) -> bool {
        self.state.job.is_some()
    }
    pub fn measuring(&self) -> bool {
        matches!(
            self.state.job,
            Some(
                Job::Measure { .. }
                    | Job::Batch { .. }
                    | Job::Alignment { .. }
                    | Job::Remeasure { .. }
            )
        )
    }
    pub fn cancellable(&self) -> bool {
        self.measuring()
            || matches!(
                self.state.job,
                Some(Job::InputCheck | Job::ManualLevels { .. })
            )
    }

    pub fn input_request(&self) -> Result<InputRequest, String> {
        let device = self
            .selected_input()
            .ok_or("Choose an available microphone input.")?;
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
        if self.settings.input_channel == 0
            || self.settings.input_channel > self.input_channels()
            || !(8000..=192000).contains(&self.settings.sample_rate)
        {
            return Err(
                "Choose an input channel and sample rate supported by the interface.".into(),
            );
        }
        Ok(InputRequest {
            device: device.name.clone(),
            input_channel: self.settings.input_channel,
            sample_rate: self.settings.sample_rate,
        })
    }

    /// Hide old readings immediately after a source/channel/rate change.
    pub fn input_check(&self) -> Option<&InputCheck> {
        self.state
            .input_check
            .as_ref()
            .filter(|c| self.input_request().as_ref() == Ok(&c.request))
    }

    pub fn check_input(&mut self) {
        if self.busy() {
            return;
        }
        let request = match self.input_request() {
            Ok(r) => r,
            Err(e) => {
                self.state.input_error = Some(e);
                return;
            }
        };
        self.state.input_check = Some(InputCheck {
            request: request.clone(),
            level: None,
            error: None,
        });
        self.state.input_error = None;
        self.state.job = Some(Job::InputCheck);
        self.state.stopping = false;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                backend.check_input(request, &cancel, &mut |level| {
                    let _ = sender.send(Event::InputLevel(level));
                })
            });
            let _ = sender.send(Event::InputChecked(result));
        });
    }

    pub fn manual_context(&self, peak_dbfs: f32) -> Result<manual_level::Context, String> {
        let input = self.input_request()?;
        if self.selected_input().is_some_and(|d| d.is_monitor) {
            return Err("Select a physical microphone, not a sink monitor.".into());
        }
        let context = manual_level::Context {
            endpoint: self.state.endpoint.ok_or("Connect to Omniphony first.")?,
            snapshot: self
                .state
                .snapshot
                .clone()
                .ok_or("Connect to Omniphony first.")?,
            input,
            microphone_id: self.settings.microphone_id.clone(),
            position_id: self.settings.position_id.clone(),
            reference_output: self
                .settings
                .reference_output
                .clone()
                .ok_or("Choose a reference speaker in the microphone settings.")?,
            peak_dbfs,
        };
        context.output(&context.reference_output)?;
        Ok(context)
    }
    pub fn manual_reference(&self, peak_dbfs: f32) -> Option<&manual_level::Reference> {
        let context = self.manual_context(peak_dbfs).ok()?;
        self.state
            .manual_reference
            .as_ref()
            .filter(|r| r.context == context)
    }
    pub fn start_manual_levels(&mut self, output_id: &str, peak_dbfs: f32) {
        if self.busy() {
            return;
        }
        let request = match self.manual_context(peak_dbfs).and_then(|context| {
            context.output(output_id)?;
            Ok(manual_level::Request {
                context,
                output_id: output_id.into(),
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                self.state.error = Some(e);
                return;
            }
        };
        if request.output_id != request.context.reference_output
            && self.manual_reference(peak_dbfs).is_none()
        {
            self.state.error = Some("Play and save the reference speaker level first.".into());
            return;
        }
        if self
            .state
            .manual_reference
            .as_ref()
            .is_some_and(|r| r.context != request.context)
        {
            self.state.manual_reference = None;
        }
        self.state.manual_run = Some(manual_level::Run {
            request: request.clone(),
            reading: None,
            error: None,
            completed: false,
        });
        self.state.job = Some(Job::ManualLevels {
            output_id: output_id.into(),
        });
        self.state.stopping = false;
        self.state.error = None;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel, alignment) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
            self.state.alignment.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                // Physical changes cannot be detected in the renderer snapshot.
                // Conservatively retire old proposals before enabling adjustment.
                if let Some(mut alignment) = alignment {
                    crate::alignment::invalidate_physical_levels(&mut alignment)?;
                    let _ = sender.send(Event::PhysicalLevelsInvalidated(Box::new(alignment)));
                }
                backend.manual_levels(request, &cancel, &mut |reading| {
                    let _ = sender.send(Event::ManualReading(reading));
                })
            });
            let _ = sender.send(Event::ManualFinished(result));
        });
    }
    pub fn can_set_manual_reference(&self, peak_dbfs: f32) -> bool {
        let Some(run) = &self.state.manual_run else {
            return false;
        };
        matches!(self.state.job, Some(Job::ManualLevels { .. }))
            && !self.state.stopping
            && self.manual_context(peak_dbfs).as_ref() == Ok(&run.request.context)
            && run.request.output_id == run.request.context.reference_output
            && run.reading.as_ref().is_some_and(|r| {
                r.phase == manual_level::Phase::Listening
                    && r.captured_at.elapsed() < Duration::from_millis(500)
                    && r.level.is_some_and(|l| l.usable())
            })
    }
    pub fn set_manual_reference(&mut self, peak_dbfs: f32) {
        if !self.can_set_manual_reference(peak_dbfs) {
            return;
        }
        if let Some(run) = &self.state.manual_run
            && let Some(level) = run.reading.as_ref().and_then(|r| r.level)
        {
            self.state.manual_reference = Some(manual_level::Reference {
                context: run.request.context.clone(),
                level,
            });
        }
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

    pub fn analysis_for(&self, take: &Take) -> Option<&crate::analysis_cache::CachedAnalysis> {
        let folder = crate::workspace::absolute(&take.outcome.as_ref().ok()?.directory);
        self.state
            .analyses
            .get(&folder)
            .filter(|a| a.matches_take(take))
    }

    pub fn load_analysis(&mut self, take: &Take) {
        if self.busy() {
            return;
        }
        let Some(analysis) = self.analysis_for(take) else {
            return;
        };
        let path = analysis.report_file.clone();
        self.state.review_error = None;
        self.state.job = Some(Job::Review);
        let sender = self.sender.clone();
        let take = take.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| crate::analysis_cache::load(&path, &take));
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
    pub fn restore_workspace(
        &mut self,
        index: Option<crate::workspace::WorkspaceIndex>,
        endpoint: Option<String>,
    ) {
        if self.busy() {
            return;
        }
        self.startup_endpoint = endpoint;
        self.state.job = Some(Job::RestoreWorkspace);
        let settings = self.settings.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| crate::workspace::restore(index, &settings));
            let _ = sender.send(Event::WorkspaceRestored(Box::new(result)));
        });
    }

    /// Enumerate first, then discover the saved renderer without starting audio.
    pub fn initialize(&mut self, endpoint: Option<String>) {
        if !self.busy() {
            self.startup_endpoint = endpoint;
            self.refresh_inputs();
        }
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
        self.output_readiness(output_id)
    }
    /// Shared eligibility checks, independent of an operation currently running.
    pub fn output_readiness(&self, output_id: &str) -> Result<(), String> {
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
        let device = self.selected_input().ok_or_else(|| {
            if self.settings.device.is_some() {
                "Selected input is unavailable. Reconnect it and refresh, or choose another input."
            } else {
                "Choose a microphone input."
            }
        })?;
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
    pub fn alignment_readiness(&self) -> Result<Vec<String>, String> {
        let outputs = self.batch_outputs()?;
        if outputs.len() < 2 {
            return Err("Select at least two broadband speakers.".into());
        }
        if self.selected_input().is_some_and(|i| i.is_monitor) {
            return Err("Choose an acoustic microphone, not a sink monitor.".into());
        }
        if self.settings.sweep.start_hz > 450.0 || self.settings.sweep.end_hz < 5000.0 {
            return Err(
                "Alignment needs broadband arrival analysis and the full 500–2000 Hz level band."
                    .into(),
            );
        }
        if self
            .state
            .alignment
            .as_ref()
            .is_some_and(|a| a.applied_snapshot.is_some() || a.application_uncertain)
        {
            return Err("Restore the active alignment before starting another baseline.".into());
        }
        Ok(outputs)
    }
    pub fn start_alignment(&mut self) {
        let outputs = match self.alignment_readiness() {
            Ok(o) => o,
            Err(e) => {
                self.state.error = Some(e);
                return;
            }
        };
        let snapshot = self.state.snapshot.clone().expect("validated");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let request = match alignment::request(
            self.state.endpoint.expect("validated"),
            snapshot,
            &self.settings,
            outputs[0].clone(),
            PathBuf::from(&self.settings.output_root).join(format!("alignment-{stamp}")),
        ) {
            Ok(r) => r,
            Err(e) => {
                self.state.error = Some(e);
                return;
            }
        };
        self.state.alignment = None;
        self.state.job = Some(Job::Alignment {
            output_ids: outputs.clone(),
            current: 0,
            pass: 0,
        });
        self.state.error = None;
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel, settings) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
            self.settings.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                alignment::capture(
                    backend.as_ref(),
                    request,
                    settings,
                    outputs,
                    &cancel,
                    &mut |pass, i, p| {
                        let _ = sender.send(Event::AlignmentProgress(pass, i, p));
                    },
                )
            });
            let _ = sender.send(Event::Aligned(Box::new(result)));
        });
    }
    pub fn alignment_action(&mut self, action: &str) {
        if self.busy() {
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        if self.state.endpoint != Some(a.endpoint) {
            self.state.error =
                Some("Connect to the renderer used for this alignment first.".into());
            return;
        }
        if action != "restore" && self.settings != a.settings {
            self.state.error=Some("Restore the original measurement settings before applying or verifying this alignment.".into());
            return;
        }
        if !matches!(action, "apply" | "restore" | "verify") {
            return;
        }
        let action = action.to_owned();
        if action == "verify"
            && let Some(current) = &mut self.state.alignment
        {
            current.verification = None;
            current.verification_note = Some("Verification in progress.".into());
        }
        self.state.job = if action == "verify" {
            Some(Job::Alignment {
                output_ids: a.corrected_output_ids(),
                current: 0,
                pass: 0,
            })
        } else {
            Some(Job::ApplyAlignment)
        };
        self.state.error = None;
        self.state.stopping = false;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                if action == "verify" {
                    let outcome =
                        alignment::verify(backend.as_ref(), &mut a, &cancel, &mut |pass, i, p| {
                            let _ = sender.send(Event::AlignmentProgress(pass, i, p));
                        })?;
                    Ok((
                        a.applied_snapshot.clone().ok_or("missing applied state")?,
                        outcome.takes,
                    ))
                } else {
                    alignment::apply(backend.as_ref(), &mut a, action == "restore")
                        .map(|s| (s, vec![]))
                }
            });
            let _ = sender.send(Event::AlignmentUpdated(Box::new(a), result));
        });
    }
    /// Whether `output_id` can be measured again into the current alignment now: the
    /// session is complete and not applied, it was captured with the current settings on
    /// the connected renderer, whose settings are still those it expects, and the speaker
    /// is ready for a take. The error says why not.
    pub fn can_remeasure(&self, output_id: &str) -> Result<(), String> {
        let a = self
            .state
            .alignment
            .as_ref()
            .ok_or("Align levels and delays first.")?;
        if !a.rows.iter().any(|r| r.output_id == output_id) {
            return Err("This speaker is not part of the alignment.".into());
        }
        if a.applied_snapshot.is_some() || a.application_uncertain {
            return Err(
                "Undo the applied alignment first: a speaker is measured again under the original settings.".into(),
            );
        }
        a.refreshable()?;
        if self.state.endpoint != Some(a.endpoint) {
            return Err("Connect to the renderer used for this alignment first.".into());
        }
        if self.settings != a.settings {
            return Err(
                "Restore the measurement settings this alignment was captured with before measuring into it.".into(),
            );
        }
        let expected = a.restored_snapshot.as_ref().unwrap_or(&a.snapshot);
        if self.state.snapshot.as_ref().map(|s| &s.revision) != Some(&expected.revision) {
            return Err("Omniphony's settings changed since this alignment was measured. Refresh, then measure a new alignment.".into());
        }
        self.can_measure(output_id)
    }
    /// Measure one speaker again, one take per pass, into the current alignment. Its
    /// repeats are replaced, the proposal rebuilt and the session saved; the other
    /// speakers keep their takes. Stop keeps the previous takes.
    pub fn remeasure(&mut self, output_id: &str) {
        if let Err(error) = self.can_remeasure(output_id) {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        self.state.job = Some(Job::Remeasure {
            output_id: output_id.into(),
            pass: 0,
        });
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.state.error = None;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel, settings, output_id) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
            self.settings.clone(),
            output_id.to_owned(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                alignment::remeasure(
                    backend.as_ref(),
                    &mut a,
                    &output_id,
                    &settings,
                    &cancel,
                    &mut |pass, i, p| {
                        let _ = sender.send(Event::AlignmentProgress(pass, i, p));
                    },
                )
                .map(|outcome| (outcome.takes, outcome.retakes))
            });
            let _ = sender.send(Event::Remeasured(Box::new(a), result));
        });
    }
    pub fn refresh_alignment_proposal(&mut self) {
        if self.busy() {
            return;
        }
        let Some(alignment) = self.state.alignment.clone() else {
            return;
        };
        self.state.job = Some(Job::UpdateProposal);
        self.state.error = None;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| crate::alignment::refresh_proposal(&alignment));
            let _ = sender.send(Event::ProposalUpdated(Box::new(result)));
        });
    }

    /// Align to a set-aside speaker anyway (or set it aside again) from the saved passes.
    pub fn include_anyway(&mut self, output_id: &str, include: bool) {
        if self.busy() {
            return;
        }
        let Some(alignment) = self.state.alignment.clone() else {
            return;
        };
        self.state.job = Some(Job::UpdateProposal);
        self.state.error = None;
        let (sender, output_id) = (self.sender.clone(), output_id.to_owned());
        std::thread::spawn(move || {
            let result =
                catch_job(|| crate::alignment::include_anyway(&alignment, &output_id, include));
            let _ = sender.send(Event::ProposalUpdated(Box::new(result)));
        });
    }

    pub fn reanalyze_alignment(&mut self) {
        if self.busy() {
            return;
        }
        let Some(alignment) = self.state.alignment.clone() else {
            return;
        };
        self.state.job = Some(Job::Review);
        self.state.error = None;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result =
                catch_job(|| alignment::reanalyze(&alignment).and_then(LoadedAlignment::read));
            let _ = sender.send(Event::AlignmentLoaded(Box::new(result)));
        });
    }
    pub fn load_alignment(&mut self, directory: &str) {
        if self.busy() {
            return;
        }
        if self
            .state
            .alignment
            .as_ref()
            .is_some_and(|a| a.applied_snapshot.is_some() || a.application_uncertain)
        {
            self.state.error = Some("Restore the current alignment before loading another.".into());
            return;
        }
        self.state.job = Some(Job::LoadAlignment);
        self.state.error = None;
        let path = PathBuf::from(directory.trim());
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| alignment::load(&path).and_then(LoadedAlignment::read));
            let _ = sender.send(Event::AlignmentLoaded(Box::new(result)));
        });
    }
    fn append_alignment_takes(
        &mut self,
        settings: Settings,
        revision: String,
        takes: Vec<(String, TakeOutcome)>,
    ) {
        for (output_id, outcome) in takes {
            self.state.takes.push(Take {
                output_id,
                revision: revision.clone(),
                settings: settings.clone(),
                outcome: Ok(outcome),
            });
        }
        if self.state.takes.len() > 100 {
            self.state.takes.drain(..self.state.takes.len() - 100);
        }
    }
    pub fn active_output(&self) -> Option<&str> {
        match &self.state.job {
            Some(Job::Measure { output_id } | Job::Remeasure { output_id, .. }) => Some(output_id),
            Some(Job::Batch {
                output_ids,
                current,
            })
            | Some(Job::Alignment {
                output_ids,
                current,
                ..
            }) => output_ids.get(*current).map(String::as_str),
            _ => None,
        }
    }
    /// Expected duration of an alignment over the checked speakers, when one can start.
    pub fn alignment_estimate(&self) -> Option<crate::duration::Estimate> {
        let outputs = self.alignment_readiness().ok()?;
        Some(crate::duration::alignment(
            self.settings.sweep,
            outputs.len(),
        ))
    }
    /// Expected duration of one diagnostic pass over the checked speakers.
    pub fn batch_estimate(&self) -> Option<crate::duration::Estimate> {
        let outputs = self.batch_outputs().ok()?;
        Some(crate::duration::batch(self.settings.sweep, outputs.len()))
    }
    /// Expected duration of one take of a single speaker.
    pub fn take_estimate(&self) -> crate::duration::Estimate {
        crate::duration::batch(self.settings.sweep, 1)
    }
    /// Time left in the running capture, from its job and latest progress.
    pub fn remaining(&self) -> Option<Duration> {
        let sweep = self.settings.sweep;
        let (estimate, pass, target) = match &self.state.job {
            Some(Job::Measure { .. }) => (crate::duration::batch(sweep, 1), 0, 0),
            Some(Job::Remeasure { pass, .. }) => (crate::duration::alignment(sweep, 1), *pass, 0),
            Some(Job::Batch {
                output_ids,
                current,
            }) => (crate::duration::batch(sweep, output_ids.len()), 0, *current),
            Some(Job::Alignment {
                output_ids,
                current,
                pass,
            }) => (
                crate::duration::alignment(sweep, output_ids.len()),
                *pass,
                *current,
            ),
            _ => return None,
        };
        Some(estimate.remaining(pass, target, self.state.progress.as_ref()))
    }
    pub fn batch_outputs(&self) -> Result<Vec<String>, String> {
        if self.busy() {
            return Err("Wait for the current operation to finish.".into());
        }
        let snapshot = self
            .state
            .snapshot
            .as_ref()
            .ok_or("Connect to Omniphony first.")?;
        let outputs: Vec<_> = snapshot
            .outputs
            .iter()
            .filter(|o| !self.excluded_outputs.contains(&o.id) && self.can_measure(&o.id).is_ok())
            .map(|o| o.id.clone())
            .collect();
        if outputs.is_empty() {
            return Err("Check at least one speaker with compatible measurement settings.".into());
        }
        measurement_capture::sequence::capture_budget(
            self.settings.sweep,
            snapshot.sample_rate,
            self.settings.sample_rate,
            outputs.len(),
        )?;
        Ok(outputs)
    }
    pub fn start_batch(&mut self) {
        let outputs = match self.batch_outputs() {
            Ok(o) => o,
            Err(e) => {
                self.state.error = Some(e);
                return;
            }
        };
        let snapshot = self.state.snapshot.as_ref().expect("validated").clone();
        let settings = self.settings.clone();
        self.sequence += 1;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let request = MeasureRequest {
            endpoint: self.state.endpoint.expect("validated"),
            snapshot: snapshot.clone(),
            device: settings.device.clone().expect("validated"),
            input_channel: settings.input_channel,
            sample_rate: settings.sample_rate,
            microphone_id: settings.microphone_id.clone(),
            position_id: settings.position_id.clone(),
            output: outputs[0].clone(),
            reference_output: settings.reference_output.clone().expect("validated"),
            calibration: (!settings.calibration_path.trim().is_empty())
                .then(|| PathBuf::from(&settings.calibration_path)),
            sweep: settings.sweep,
            out_dir: PathBuf::from(&settings.output_root)
                .join(format!("batch-{stamp}-{}", self.sequence)),
        };
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.state.job = Some(Job::Batch {
            output_ids: outputs.clone(),
            current: 0,
        });
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.state.error = None;
        let backend = self.backend.clone();
        let sender = self.sender.clone();
        let cancel = self.cancelled.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| {
                backend.measure_batch(request, outputs, &cancel, &mut |i, p| {
                    let _ = sender.send(Event::BatchProgress(i, p));
                })
            });
            let _ = sender.send(Event::Batched(settings, snapshot.revision, result));
        });
    }
    pub fn cancel(&mut self) {
        if self.cancellable() {
            self.state.stopping = true;
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
    /// Reading events is nonblocking. A slow/minimized frontend never owns keepalives.
    pub fn poll(&mut self) {
        let previous_revision = self.state.workspace_revision;
        while let Ok(event) = self.receiver.try_recv() {
            if !matches!(
                &event,
                Event::ManualReading(_)
                    | Event::InputLevel(_)
                    | Event::AlignmentProgress(..)
                    | Event::BatchProgress(..)
                    | Event::Progress(_)
            ) {
                self.state.workspace_revision = self.state.workspace_revision.wrapping_add(1);
            }
            match event {
                Event::ProposalUpdated(result) => {
                    match *result {
                        Ok(a) => self.state.alignment = Some(a),
                        Err(e) => self.state.error = Some(e),
                    }
                    self.state.job = None;
                }

                Event::ManualReading(reading) => {
                    if let Some(run) = &mut self.state.manual_run {
                        run.reading = Some(reading);
                    }
                }
                Event::PhysicalLevelsInvalidated(alignment) => {
                    self.state.alignment = Some(*alignment);
                }
                Event::ManualFinished(result) => {
                    if let Some(run) = &mut self.state.manual_run {
                        run.error = result.err();
                        run.completed = true;
                    }
                    self.state.job = None;
                    self.state.stopping = false;
                }

                Event::WorkspaceRestored(result) => {
                    match *result {
                        Ok(saved) => {
                            self.state.restored_index = Some(saved.index);
                            self.state.takes = saved.takes;
                            self.state.analyses = saved.analyses;
                            self.state.saved_snapshot = saved.snapshot;
                            self.state.alignment = saved.alignment;
                            self.state.review = saved.review;
                            self.state.error = saved.error;
                            self.state.workspace_warnings = saved.warnings;
                            self.state.workspace_imported = saved.imported;
                        }
                        Err(e) => self
                            .state
                            .workspace_warnings
                            .push(format!("Workspace could not be restored: {e}")),
                    }
                    self.state.workspace_ready = true;
                    self.state.job = None;
                    self.refresh_inputs();
                }
                Event::InputLevel(level) => {
                    if let Some(check) = &mut self.state.input_check {
                        check.level = Some(level);
                    }
                }
                Event::InputChecked(result) => {
                    if let Some(check) = &mut self.state.input_check {
                        check.error = result.err();
                    }
                    self.state.job = None;
                    self.state.stopping = false;
                }
                Event::AlignmentProgress(pass, index, p) => {
                    match &mut self.state.job {
                        Some(Job::Alignment {
                            output_ids,
                            current,
                            pass: active_pass,
                        }) => {
                            *current = index.min(output_ids.len().saturating_sub(1));
                            *active_pass = pass;
                        }
                        Some(Job::Remeasure {
                            pass: active_pass, ..
                        }) => *active_pass = pass,
                        _ => {}
                    }
                    self.state.progress = Some(p);
                }
                Event::Remeasured(a, result) => {
                    match result {
                        Ok((takes, retakes)) => {
                            let revision = a
                                .restored_snapshot
                                .as_ref()
                                .unwrap_or(&a.snapshot)
                                .revision
                                .clone();
                            self.append_alignment_takes(a.settings.clone(), revision, takes);
                            for retake in retakes {
                                self.append_alignment_takes(
                                    a.settings.clone(),
                                    retake.revision,
                                    vec![(retake.output_id, retake.take)],
                                );
                            }
                        }
                        Err(e) => self.state.error = Some(e),
                    }
                    // A retake lowered a gain and restored it: the renderer is at the
                    // last state the session recorded, under a newer revision.
                    let recorded = |a: &alignment::Alignment| a.retake_snapshots.len();
                    if self.state.alignment.as_ref().map(recorded) != Some(recorded(&a))
                        && let Some(live) = a.retake_snapshots.last()
                    {
                        self.state.saved_snapshot = Some(live.clone());
                        self.state.snapshot = Some(live.clone());
                    }
                    self.state.alignment = Some(*a);
                    self.state.job = None;
                    self.state.progress = None;
                    self.state.stopping = false;
                }
                Event::Aligned(result) => {
                    match *result {
                        Ok(outcome) => {
                            let settings = outcome.alignment.settings.clone();
                            self.append_alignment_takes(
                                settings.clone(),
                                outcome.alignment.snapshot.revision.clone(),
                                outcome.takes,
                            );
                            // Takes at a lowered gain stay previous measurements.
                            for retake in outcome.retakes {
                                self.append_alignment_takes(
                                    settings.clone(),
                                    retake.revision,
                                    vec![(retake.output_id, retake.take)],
                                );
                            }
                            // Restoring lowered gains gave the renderer a newer revision.
                            if let Some(live) = &outcome.alignment.restored_snapshot {
                                self.state.saved_snapshot = Some(live.clone());
                                self.state.snapshot = Some(live.clone());
                            }
                            self.state.alignment = Some(outcome.alignment);
                        }
                        Err(e) => self.state.error = Some(e),
                    }
                    self.state.job = None;
                    self.state.progress = None;
                    self.state.stopping = false;
                }
                Event::AlignmentUpdated(a, result) => {
                    match result {
                        Ok((snapshot, takes)) => {
                            self.append_alignment_takes(
                                a.settings.clone(),
                                snapshot.revision.clone(),
                                takes,
                            );
                            self.state.saved_snapshot = Some(snapshot.clone());
                            self.state.snapshot = Some(snapshot);
                        }
                        Err(e) => self.state.error = Some(e),
                    }
                    self.state.alignment = Some(*a);
                    self.state.job = None;
                    self.state.progress = None;
                    self.state.stopping = false;
                }
                Event::AlignmentLoaded(result) => {
                    match *result {
                        Ok(loaded) => {
                            let mut imported = Vec::new();
                            for take in loaded.workspace.takes {
                                let folder = take
                                    .outcome
                                    .as_ref()
                                    .ok()
                                    .map(|o| crate::workspace::absolute(&o.directory));
                                if let Some(old) = self.state.takes.iter_mut().find(|t| {
                                    folder.is_some()
                                        && t.outcome
                                            .as_ref()
                                            .ok()
                                            .map(|o| crate::workspace::absolute(&o.directory))
                                            == folder
                                }) {
                                    *old = take;
                                } else {
                                    imported.push(take);
                                }
                                if let Some(folder) = folder {
                                    self.state.analyses.remove(&folder);
                                }
                            }
                            // Browsing an older alignment must not displace newer captures
                            // as the default result for an existing speaker.
                            self.state.takes.splice(0..0, imported);
                            self.state.analyses.extend(loaded.workspace.analyses);
                            self.state
                                .workspace_warnings
                                .extend(loaded.workspace.warnings);
                            if self.state.takes.len() > 100 {
                                self.state.takes.drain(..self.state.takes.len() - 100);
                            }
                            self.state.alignment = Some(loaded.alignment);
                        }
                        Err(e) => self.state.error = Some(e),
                    }
                    self.state.job = None;
                }
                Event::BatchProgress(index, progress) => {
                    if let Some(Job::Batch {
                        output_ids,
                        current,
                    }) = &mut self.state.job
                    {
                        *current = index.min(output_ids.len().saturating_sub(1));
                    }
                    self.state.progress = Some(progress);
                }
                Event::Batched(settings, revision, result) => {
                    match result {
                        Ok(batch) => {
                            if let Some(error) = batch.error {
                                self.state.error = Some(format!(
                                    "{error}. Saved batch: {}",
                                    batch.directory.display()
                                ));
                            }
                            for (output_id, outcome) in batch.takes {
                                self.state.takes.push(Take {
                                    output_id,
                                    revision: revision.clone(),
                                    settings: settings.clone(),
                                    outcome: Ok(outcome),
                                });
                            }
                            if self.state.takes.len() > 100 {
                                self.state.takes.drain(..self.state.takes.len() - 100);
                            }
                        }
                        Err(error) => self.state.error = Some(error),
                    }
                    self.state.progress = None;
                    self.state.job = None;
                    self.state.stopping = false;
                }
                Event::Reviewed(result) => {
                    match *result {
                        Ok(review) => {
                            let summary =
                                crate::analysis_cache::CachedAnalysis::from_review(&review);
                            self.state
                                .analyses
                                .insert(summary.directory.clone(), summary);
                            self.state.review = Some(review);
                        }
                        Err(error) => self.state.review_error = Some(error),
                    }
                    self.state.job = None;
                }

                Event::Inputs(result) => {
                    match result {
                        Ok(inputs) => {
                            self.state.inputs = inputs;
                            crate::defaults::input_settings(
                                &mut self.settings,
                                &self.state.inputs,
                                &mut self.state.defaults,
                            );
                        }
                        Err(error) => {
                            self.state.inputs.clear();
                            self.state.input_error = Some(error);
                        }
                    }
                    crate::defaults::identities(&mut self.settings, &mut self.state.defaults);
                    self.state.job = None;
                    if let Some(endpoint) = self.startup_endpoint.take() {
                        self.connect(&endpoint);
                    }
                }
                Event::Discovered(endpoint, result) => {
                    match result {
                        Ok(snapshot) => {
                            crate::defaults::reference_settings(
                                &mut self.settings,
                                &snapshot,
                                &mut self.state.defaults,
                            );
                            self.state.endpoint = Some(endpoint);
                            self.state.saved_snapshot = Some(snapshot.clone());
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
        if self.state.workspace_revision != previous_revision {
            let folders: std::collections::BTreeSet<_> = self
                .state
                .takes
                .iter()
                .filter_map(|t| {
                    t.outcome
                        .as_ref()
                        .ok()
                        .map(|o| crate::workspace::absolute(&o.directory))
                })
                .chain(
                    self.state
                        .review
                        .as_ref()
                        .map(|r| crate::workspace::absolute(&r.directory)),
                )
                .collect();
            self.state
                .analyses
                .retain(|folder, _| folders.contains(folder));
        }
        if self.input_check().is_none() {
            self.state.input_check = None;
        }
    }
    /// Historic results remain viewable after a renderer restart or settings edit.
    /// They do not become current evidence or enable acquisition without discovery.
    pub fn display_take(&self, output_id: &str) -> Option<&Take> {
        self.current_take(output_id).or_else(|| {
            self.state
                .takes
                .iter()
                .rev()
                .find(|t| t.output_id == output_id)
        })
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
use crate::defaults::supported_format;
fn catch_job<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err("The background operation stopped unexpectedly. Reconnect before retrying.".into())
    })
}
