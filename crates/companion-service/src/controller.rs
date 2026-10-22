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
    /// Switch the renderer's live output to the speakers (protocol 7).
    fn use_speaker_output(&self, endpoint: SocketAddr) -> Result<Snapshot, String> {
        let _ = endpoint;
        Err("Switching the output is unavailable in this backend.".into())
    }
    /// Replace the room correction filters of some outputs at once (protocol 8),
    /// revision-checked and live only: each named output gets its taps, or no filter.
    fn set_output_filters(
        &self,
        endpoint: SocketAddr,
        expected: Snapshot,
        filters: Vec<OutputFilter>,
    ) -> Result<Snapshot, String> {
        let _ = (endpoint, expected, filters);
        Err("Sending correction filters is unavailable in this backend.".into())
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
    /// [`Self::discover`] with the measurement protocol version the renderer speaks: an
    /// older renderer negotiates down and cannot take correction filters.
    fn discover_versioned(&self, endpoint: SocketAddr) -> Result<(Snapshot, u32), String> {
        self.discover(endpoint)
            .map(|s| (s, omniphony_adapter::protocol::VERSION))
    }
    fn measure(
        &self,
        request: MeasureRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String>;
}
/// One output's room correction filter to send: its taps at their rate, or none.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputFilter {
    pub output_id: String,
    pub taps: Option<(u32, Vec<f32>)>,
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
    fn discover_versioned(&self, endpoint: SocketAddr) -> Result<(Snapshot, u32), String> {
        Client::connect(endpoint, Duration::from_secs(2))
            .and_then(|mut client| {
                let snapshot = client.discover()?;
                Ok((snapshot, client.protocol_version()))
            })
            .map_err(|e| e.to_string())
    }
    fn use_speaker_output(&self, endpoint: SocketAddr) -> Result<Snapshot, String> {
        Client::connect(endpoint, Duration::from_secs(2))
            .and_then(|mut client| client.use_speaker_output())
            .map_err(|e| e.to_string())
    }
    fn set_output_filters(
        &self,
        endpoint: SocketAddr,
        expected: Snapshot,
        filters: Vec<OutputFilter>,
    ) -> Result<Snapshot, String> {
        let mut client =
            Client::connect(endpoint, Duration::from_secs(2)).map_err(|e| e.to_string())?;
        // Discovery agrees on the protocol version before anything is uploaded.
        client.discover().map_err(|e| e.to_string())?;
        let mut settings = Vec::with_capacity(filters.len());
        for filter in filters {
            let upload = match &filter.taps {
                Some((rate, taps)) => Some(
                    client
                        .upload_filter(*rate, taps)
                        .map_err(|e| e.to_string())?,
                ),
                None => None,
            };
            settings.push(omniphony_adapter::protocol::OutputFilterSetting {
                output_id: filter.output_id,
                filter: upload,
            });
        }
        client
            .set_output_filters(&expected, settings)
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
    /// The manual sweep: its level is used without [`Settings::automatic_level`], and
    /// its duration and range in [`crate::sweep_plan::SweepMode::Custom`].
    pub sweep: Sweep,
    /// How each output's sweep is chosen; see [`crate::sweep_plan`].
    pub sweep_mode: crate::sweep_plan::SweepMode,
    /// Before an alignment, probe each speaker at a low level and play it so that the
    /// microphone peaks near −6 dBFS; see [`crate::level_probe`].
    pub automatic_level: bool,
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
            sweep_mode: Default::default(),
            automatic_level: true,
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
    /// The current alignment's speakers measured once at guide step `step`
    /// ([`crate::positions::GUIDE`]), in the alignment's row order.
    Position {
        step: usize,
        output_ids: Vec<String>,
        current: usize,
    },
    /// Designing the current alignment's room correction and exporting its filters.
    RoomCorrection,
    /// The room correction applied, compared, or undone.
    ApplyCorrection {
        target: crate::correction::Target,
    },
    /// The applied room correction measured again at the main seat, then the
    /// subwoofer's sums when it was applied with it.
    VerifyCorrection {
        output_ids: Vec<String>,
        current: usize,
    },
    /// The subwoofer's integration measured at the main seat (`step` `None`) or at a
    /// guide step, the speakers of its plan in order.
    Bass {
        step: Option<usize>,
        output_ids: Vec<String>,
        current: usize,
    },
    /// The subwoofer integration applied, or undone with `restore`.
    ApplyBass {
        restore: bool,
    },
    /// The applied subwoofer integration measured again at the main seat.
    VerifyBass {
        output_ids: Vec<String>,
        current: usize,
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

/// A take with the settings it was measured with and its speaker.
type RecordedTake = (Settings, String, TakeOutcome);

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
    /// The measurement protocol version the connected renderer speaks.
    pub protocol_version: Option<u32>,
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
    /// Folders of deferred takes whose saved report is being read.
    pub loading_takes: std::collections::HashSet<PathBuf>,
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
        Result<(Snapshot, Vec<RecordedTake>), String>,
    ),
    AlignmentLoaded(Box<Result<LoadedAlignment, String>>),
    /// A guided position measured; the renderer as read back after it.
    PositionMeasured(
        Box<alignment::Alignment>,
        Result<(), String>,
        Option<Snapshot>,
    ),
    /// A subwoofer measurement, Apply, Undo or Confirm finished; the renderer as read
    /// back after an Apply or Undo.
    BassUpdated(
        Box<alignment::Alignment>,
        Result<Option<Snapshot>, String>,
        Option<Snapshot>,
    ),
    /// A room correction computed, applied, compared, undone or confirmed; the renderer
    /// as read back after it.
    CorrectionUpdated(
        Box<alignment::Alignment>,
        Result<(), String>,
        Option<Snapshot>,
    ),
    Reviewed(Box<Result<SavedReview, String>>),
    SavedTakeLoaded(PathBuf, Box<Result<TakeOutcome, String>>),
    OutputSwitched(Result<Snapshot, String>),
    Inputs(Result<Vec<InputDevice>, String>),
    Discovered(
        SocketAddr,
        Result<(Snapshot, u32), String>,
        Option<Box<Result<alignment::Alignment, String>>>,
    ),
    Progress(MeasureProgress),
    Measured(Box<Take>),
    BatchProgress(usize, MeasureProgress),
    Batched(
        Vec<(String, Settings)>,
        String,
        Result<BatchOutcome, String>,
    ),
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
    /// The basis the user chose for the next alignment; `None` follows
    /// [`Controller::default_alignment_basis`].
    pub chosen_alignment_basis: Option<alignment::Basis>,
    /// The calibration file last read, by path: re-read only when the path changes.
    calibration: Option<(String, Result<crate::room_response::CalibrationUse, String>)>,
    /// The settings the next room correction is computed with.
    pub correction_choices: crate::correction::Choices,
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
            chosen_alignment_basis: None,
            calibration: None,
            correction_choices: Default::default(),
        }
    }
    /// The microphone calibration the next takes will use: `None` without a file,
    /// else what identifies it or why it cannot be read. Takes measure either way;
    /// a calibration only corrects the magnitude stored for room correction.
    pub fn calibration_status(
        &mut self,
    ) -> Option<&Result<crate::room_response::CalibrationUse, String>> {
        let path = self.settings.calibration_path.trim();
        if path.is_empty() {
            self.calibration = None;
            return None;
        }
        if self.calibration.as_ref().is_none_or(|(p, _)| p != path) {
            let status = crate::room_response::summarize(std::path::Path::new(path));
            self.calibration = Some((path.to_owned(), status));
        }
        self.calibration.as_ref().map(|(_, status)| status)
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
                    | Job::Position { .. }
                    | Job::Bass { .. }
                    | Job::VerifyBass { .. }
                    | Job::VerifyCorrection { .. }
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
    /// Play the level noise on one output: without its gain by default, or with it
    /// (`with_output_gain`) to check the corrected result.
    pub fn start_manual_levels(&mut self, output_id: &str, peak_dbfs: f32, with_output_gain: bool) {
        if self.busy() {
            return;
        }
        let request = match self.manual_context(peak_dbfs).and_then(|context| {
            context.playable(output_id, with_output_gain)?;
            Ok(manual_level::Request {
                context,
                output_id: output_id.into(),
                with_output_gain,
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
    /// Forget the last reading of a finished noise run; a running one is kept.
    pub fn dismiss_manual_run(&mut self) {
        if !matches!(self.state.job, Some(Job::ManualLevels { .. })) {
            self.state.manual_run = None;
        }
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

    /// Read the saved report of a take restored without it, off the UI thread. It
    /// does not block other work; asking again while it loads does nothing.
    pub fn load_saved_take(&mut self, directory: &std::path::Path) {
        let Some(take) = self.state.takes.iter().find(|t| {
            t.outcome
                .as_ref()
                .is_ok_and(|o| o.deferred && o.directory == directory)
        }) else {
            return;
        };
        if !self.state.loading_takes.insert(directory.to_owned()) {
            return;
        }
        let reference = crate::workspace::TakeReference::from(take);
        let (sender, folder) = (self.sender.clone(), directory.to_owned());
        std::thread::spawn(move || {
            let result = catch_job(|| crate::workspace::load_saved(&reference));
            let _ = sender.send(Event::SavedTakeLoaded(folder, Box::new(result)));
        });
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
        self.state.protocol_version = None;
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
        let alignment = self.state.alignment.clone();
        std::thread::spawn(move || {
            let result = catch_job(|| backend.discover_versioned(endpoint));
            // Whichever port or process it is, record whether this renderer has the
            // alignment's original or corrected settings.
            let reconciled = match (&result, alignment) {
                (Ok((live, _)), Some(mut a)) => Some(Box::new(catch_job(|| {
                    a.endpoint = endpoint;
                    alignment::reconcile(&mut a, live).map(|_| a)
                }))),
                _ => None,
            };
            let _ = sender.send(Event::Discovered(endpoint, result, reconciled));
        });
    }
    /// Whether the renderer cannot measure because it plays to headphones, which
    /// [`Self::use_speaker_output`] resolves.
    pub fn needs_speaker_output(&self) -> bool {
        self.state
            .snapshot
            .as_ref()
            .and_then(|s| s.unavailable_reason.as_deref())
            == Some(omniphony_adapter::protocol::SPEAKER_OUTPUT_REQUIRED)
    }
    /// Switch the renderer from headphones to the speakers, live only, then show
    /// its new snapshot. The saved configuration stays Studio's to change.
    pub fn use_speaker_output(&mut self) {
        if self.busy() {
            return;
        }
        let Some(endpoint) = self.state.endpoint else {
            return;
        };
        self.state.job = Some(Job::Discover);
        self.state.error = None;
        let (backend, sender) = (self.backend.clone(), self.sender.clone());
        std::thread::spawn(move || {
            let result = catch_job(|| backend.use_speaker_output(endpoint));
            let _ = sender.send(Event::OutputSwitched(result));
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
        }
        let groups = crate::sweep_plan::plan(&self.settings, snapshot, &[output_id.into()], None)?;
        let group = &groups[0];
        let sweep = group.sweep;
        sweep.validate(snapshot.sample_rate)?;
        if !(8000..=192000).contains(&self.settings.sample_rate)
            || sweep.end_hz > self.settings.sample_rate as f32 * 0.45
        {
            return Err(
                "The sweep's upper frequency is too high for the input sample rate.".into(),
            );
        }
        for id in [output_id, group.reference.as_str()] {
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
            if sweep.start_hz < output.freq_low_hz.unwrap_or(20.0)
                || sweep.end_hz > output.freq_high_hz.unwrap_or(f32::MAX)
            {
                return Err(format!(
                    "The sweep must fit {}'s frequency range.",
                    output.name
                ));
            }
            if sweep.peak_dbfs + snapshot.master_gain_db + output.gain_db > -6.0 {
                return Err(format!(
                    "Lower the sweep level for {}'s current gain.",
                    output.name
                ));
            }
        }
        Ok(())
    }

    /// The groups the next capture of `outputs` measures, with the manual level.
    pub fn plan(&self, outputs: &[String]) -> Result<Vec<crate::sweep_plan::Group>, String> {
        let snapshot = self
            .state
            .snapshot
            .as_ref()
            .ok_or("Connect to Omniphony first.")?;
        crate::sweep_plan::plan(&self.settings, snapshot, outputs, None)
    }

    pub fn start(&mut self, output_id: &str) {
        if let Err(error) = self.can_measure(output_id) {
            self.state.error = Some(error);
            return;
        }
        let snapshot = self.state.snapshot.as_ref().expect("validated").clone();
        let groups = self.plan(&[output_id.into()]).expect("validated");
        let settings = crate::sweep_plan::take_settings(&self.settings, &groups[0]);
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
            bass_managed: false,
            bypass_output_filters: false,
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
    /// Where the connected renderer stands for the current alignment, if both exist.
    pub fn alignment_live_state(&self) -> Option<alignment::LiveState> {
        Some(
            self.state
                .alignment
                .as_ref()?
                .live_state(self.state.snapshot.as_ref()?),
        )
    }
    /// Whether the current alignment's corrections must be undone before another
    /// baseline: only while this renderer has them, or after an uncertain application.
    /// An alignment applied to another installation or under other settings has nothing
    /// to undo here; it keeps its saved original settings on disk.
    fn active_alignment_blocks(&self) -> bool {
        self.state.alignment.as_ref().is_some_and(|a| {
            a.application_uncertain
                || a.bass.as_ref().is_some_and(|b| b.is_applied())
                || a.correction
                    .as_ref()
                    .is_some_and(|c| c.uncertain || c.applied.is_some())
                || a.applied_snapshot.is_some()
                    && self.alignment_live_state() == Some(alignment::LiveState::Applied)
        })
    }
    pub fn alignment_readiness(&self) -> Result<Vec<String>, String> {
        let outputs = self.batch_outputs()?;
        if outputs.len() < 2 {
            return Err("Select at least two broadband speakers.".into());
        }
        if self.selected_input().is_some_and(|i| i.is_monitor) {
            return Err("Choose an acoustic microphone, not a sink monitor.".into());
        }
        let groups = self.plan(&outputs)?;
        if groups
            .iter()
            .any(|g| !g.bass_only() && (g.sweep.start_hz > 450.0 || g.sweep.end_hz < 5000.0))
        {
            return Err(
                "Alignment needs broadband arrival analysis and the full 500–2000 Hz level band."
                    .into(),
            );
        }
        if groups
            .iter()
            .filter(|g| !g.bass_only())
            .map(|g| g.outputs.len())
            .sum::<usize>()
            < 2
        {
            return Err("Select at least two broadband speakers.".into());
        }
        // An applied alignment can be refined: the next one starts from its corrections.
        if self
            .state
            .alignment
            .as_ref()
            .is_some_and(|a| a.application_uncertain)
        {
            return Err(
                "The last application is uncertain. Refresh before starting another baseline."
                    .into(),
            );
        }
        Ok(outputs)
    }
    /// Refine once the renderer has the corrections of an alignment applied to it, from
    /// zero otherwise: before any alignment, after an Undo, or once its settings changed.
    pub fn default_alignment_basis(&self) -> alignment::Basis {
        let applied = self.state.alignment.as_ref().is_some_and(|a| {
            a.applied_snapshot.is_some()
                && self.alignment_live_state() == Some(alignment::LiveState::Applied)
        });
        if applied {
            alignment::Basis::Refine
        } else {
            alignment::Basis::FromZero
        }
    }
    /// The basis the next alignment measures with: the user's choice, else the default.
    pub fn alignment_basis(&self) -> alignment::Basis {
        self.chosen_alignment_basis
            .unwrap_or_else(|| self.default_alignment_basis())
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
        // Chosen while the previous alignment is still known: it decides the default.
        let basis = self.alignment_basis();
        // A choice is for one alignment; the next follows the default again.
        self.chosen_alignment_basis = None;
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
                    basis,
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
        let Some(endpoint) = self.state.endpoint else {
            self.state.error = Some("Connect to Omniphony first.".into());
            return;
        };
        a.endpoint = endpoint;
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
                    let measured = &outcome.alignment;
                    Ok((
                        a.applied_snapshot.clone().ok_or("missing applied state")?,
                        outcome
                            .takes
                            .into_iter()
                            .map(|(id, take)| (measured.take_settings(&id), id, take))
                            .collect(),
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
        if self.state.endpoint.is_none() {
            return Err("Connect to Omniphony first.".into());
        }
        let live = self
            .alignment_live_state()
            .ok_or("Connect to Omniphony first.")?;
        if live == alignment::LiveState::Other {
            return Err("Omniphony's settings changed since this alignment was measured. Refresh, then measure a new alignment.".into());
        }
        if live == alignment::LiveState::Applied
            || a.applied_snapshot.is_some()
            || a.application_uncertain
        {
            return Err(
                "Undo the applied alignment first: a speaker is measured again under the original settings.".into(),
            );
        }
        a.refreshable()?;
        if self.settings != a.settings {
            return Err(
                "Restore the measurement settings this alignment was captured with before measuring into it.".into(),
            );
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
        a.endpoint = self.state.endpoint.expect("checked by can_remeasure");
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
    /// Whether the current alignment's speakers can be measured at another microphone
    /// position now; the error says why not.
    pub fn position_readiness(&self) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for the current operation to finish.".into());
        }
        let a = self
            .state
            .alignment
            .as_ref()
            .ok_or("Align levels and delays at the main seat first.")?;
        let live = self
            .state
            .snapshot
            .as_ref()
            .ok_or("Connect to Omniphony first.")?;
        if self.state.endpoint.is_none() {
            return Err("Connect to Omniphony first.".into());
        }
        crate::positions::readiness(a, live)?;
        if self.settings != a.settings {
            return Err(
                "Restore the measurement settings this alignment was captured with.".into(),
            );
        }
        let first = a.rows.first().ok_or("The alignment has no speakers.")?;
        self.can_measure(&first.output_id)
    }
    /// Whether the subwoofer's integration is measured at guide step `step` with the
    /// speakers: chosen, and measured at the main seat first.
    pub fn position_includes_subwoofer(&self, step: usize) -> bool {
        self.correction_choices.with_subwoofer
            && self
                .state
                .alignment
                .as_ref()
                .and_then(|a| a.bass.as_ref())
                .is_some_and(|b| b.capture(None).is_some())
            && self.bass_readiness(Some(step)).is_ok()
    }
    /// Measure the current alignment's speakers at guide step `step`, and the
    /// subwoofer's integration there when [`Self::position_includes_subwoofer`].
    pub fn measure_position(&mut self, step: usize) {
        if let Err(error) = self.position_readiness() {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        a.endpoint = self.state.endpoint.expect("checked by position_readiness");
        let subwoofer = self.position_includes_subwoofer(step);
        let mut output_ids: Vec<String> = a.rows.iter().map(|r| r.output_id.clone()).collect();
        let speakers = output_ids.len();
        if subwoofer && let Some(b) = &a.bass {
            output_ids.extend(b.plan.speakers());
        }
        self.state.job = Some(Job::Position {
            step,
            output_ids,
            current: 0,
        });
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.state.error = None;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                crate::positions::measure(backend.as_ref(), &mut a, step, &cancel, &mut |i, p| {
                    let _ = sender.send(Event::BatchProgress(i, p));
                })?;
                if subwoofer && !cancel.load(Ordering::Relaxed) {
                    crate::bass::measure(
                        backend.as_ref(),
                        &mut a,
                        Some(step),
                        &cancel,
                        &mut |i, p| {
                            let _ = sender.send(Event::BatchProgress(speakers + i, p));
                        },
                    )?;
                }
                Ok(())
            });
            // Moving outputs for the position and back gives the renderer a newer
            // revision: read it again, whatever happened.
            let live = backend.discover(a.endpoint).ok();
            let _ = sender.send(Event::PositionMeasured(Box::new(a), result, live));
        });
    }
    /// The current alignment, the connected renderer and its address, when the
    /// renderer can be reached.
    fn connected_alignment(&self) -> Result<(&alignment::Alignment, &Snapshot), String> {
        if self.busy() {
            return Err("Wait for the current operation to finish.".into());
        }
        let a = self
            .state
            .alignment
            .as_ref()
            .ok_or("Align levels and delays at the main seat first.")?;
        let live = self
            .state
            .snapshot
            .as_ref()
            .ok_or("Connect to Omniphony first.")?;
        if self.state.endpoint.is_none() {
            return Err("Connect to Omniphony first.".into());
        }
        Ok((a, live))
    }
    /// Whether the subwoofer can be measured now at the main seat (`step` `None`) or at
    /// a guide step; the error says why not.
    pub fn bass_readiness(&self, step: Option<usize>) -> Result<(), String> {
        let (a, live) = self.connected_alignment()?;
        crate::bass::readiness(a, live, step)?;
        if self.settings != a.settings {
            return Err(
                "Restore the measurement settings this alignment was captured with.".into(),
            );
        }
        if self.selected_input().is_some_and(|i| i.is_monitor) {
            return Err("Choose an acoustic microphone, not a sink monitor.".into());
        }
        let plan = match (&a.bass, step) {
            (Some(bass), Some(_)) => bass.plan.clone(),
            _ => crate::bass::plan(a, live)?,
        };
        self.output_readiness(&plan.groups[0].reference)
    }
    /// Measure the subwoofer's integration at the main seat (`None`, which starts over)
    /// or at guide step `step`.
    pub fn measure_bass(&mut self, step: Option<usize>) {
        if let Err(error) = self.bass_readiness(step) {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        a.endpoint = self.state.endpoint.expect("checked by bass_readiness");
        let output_ids = match (&a.bass, step, &self.state.snapshot) {
            (Some(bass), Some(_), _) => bass.plan.speakers(),
            (_, _, Some(live)) => crate::bass::plan(&a, live)
                .map(|p| p.speakers())
                .unwrap_or_default(),
            _ => vec![],
        };
        self.state.job = Some(Job::Bass {
            step,
            output_ids,
            current: 0,
        });
        self.start_bass_job(a, move |backend, a, cancel, progress| {
            crate::bass::measure(backend, a, step, cancel, progress).map(|()| None)
        });
    }
    /// Whether the subwoofer integration can be applied (or undone, with `restore`) now.
    pub fn bass_apply_readiness(&self, restore: bool) -> Result<(), String> {
        let (a, live) = self.connected_alignment()?;
        crate::bass::apply_readiness(a, live, restore)
    }
    /// Apply the subwoofer integration, or undo it with `restore`.
    pub fn apply_bass(&mut self, restore: bool) {
        if let Err(error) = self.bass_apply_readiness(restore) {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        a.endpoint = self
            .state
            .endpoint
            .expect("checked by bass_apply_readiness");
        self.state.job = Some(Job::ApplyBass { restore });
        self.start_bass_job(a, move |backend, a, _, _| {
            crate::bass::apply(backend, a, restore).map(Some)
        });
    }
    /// Whether the applied subwoofer integration can be confirmed now.
    pub fn bass_verify_readiness(&self) -> Result<(), String> {
        let (a, live) = self.connected_alignment()?;
        crate::bass::verify_readiness(a, live)?;
        if self.settings != a.settings {
            return Err(
                "Restore the measurement settings this alignment was captured with.".into(),
            );
        }
        let reference = a
            .bass
            .as_ref()
            .and_then(|b| b.plan.groups.first())
            .map(|g| g.reference.clone());
        self.output_readiness(reference.as_deref().unwrap_or_default())
    }
    /// Measure the applied subwoofer integration again at the main seat.
    pub fn verify_bass(&mut self) {
        if let Err(error) = self.bass_verify_readiness() {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        a.endpoint = self
            .state
            .endpoint
            .expect("checked by bass_verify_readiness");
        let output_ids = a
            .bass
            .as_ref()
            .map(|b| b.plan.speakers())
            .unwrap_or_default();
        for b in [self.state.alignment.as_mut(), Some(&mut a)]
            .into_iter()
            .flatten()
            .filter_map(|a| a.bass.as_mut())
        {
            b.verification = None;
            b.verification_note = Some("Confirmation in progress.".into());
        }
        self.state.job = Some(Job::VerifyBass {
            output_ids,
            current: 0,
        });
        self.start_bass_job(a, |backend, a, cancel, progress| {
            crate::bass::verify(backend, a, cancel, progress).map(|()| None)
        });
    }
    fn start_bass_job(
        &mut self,
        mut a: alignment::Alignment,
        job: impl FnOnce(
            &dyn Backend,
            &mut alignment::Alignment,
            &AtomicBool,
            &mut dyn FnMut(usize, MeasureProgress),
        ) -> Result<Option<Snapshot>, String>
        + Send
        + 'static,
    ) {
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.state.error = None;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                job(backend.as_ref(), &mut a, &cancel, &mut |i, p| {
                    let _ = sender.send(Event::BatchProgress(i, p));
                })
            });
            // Silencing outputs and putting them back gives the renderer a newer
            // revision: read it again, whatever happened.
            let live = match &result {
                Ok(Some(snapshot)) => Some(snapshot.clone()),
                _ => backend.discover(a.endpoint).ok(),
            };
            let _ = sender.send(Event::BassUpdated(Box::new(a), result, live));
        });
    }
    /// Whether the current alignment's room correction can be computed now, and what
    /// would make it weaker. No renderer is needed: only stored responses are read.
    pub fn correction_readiness(&self) -> crate::correction::Readiness {
        if self.busy() {
            return crate::correction::Readiness {
                blocker: Some("Wait for the current operation to finish.".into()),
                ..Default::default()
            };
        }
        let Some(a) = self.state.alignment.as_ref() else {
            return crate::correction::Readiness {
                blocker: Some("Align levels and delays at the main seat first.".into()),
                ..Default::default()
            };
        };
        crate::correction::readiness(a, self.state.snapshot.as_ref(), self.state.protocol_version)
    }
    /// Design the current alignment's room correction, export its filters as WAV files
    /// in a new folder of the alignment and record it there. Nothing is sent to
    /// Omniphony.
    pub fn compute_correction(&mut self) {
        if let Some(error) = self.correction_readiness().blocker {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        self.state.job = Some(Job::RoomCorrection);
        self.state.error = None;
        let (sender, choices) = (self.sender.clone(), self.correction_choices);
        std::thread::spawn(move || {
            let result = catch_job(|| {
                let correction = crate::correction::compute(&a, choices)?;
                a.correction = Some(correction);
                alignment::save(&a)
            });
            let _ = sender.send(Event::CorrectionUpdated(Box::new(a), result, None));
        });
    }
    /// Where the connected renderer stands with the current alignment's correction.
    pub fn correction_live_state(&self) -> Option<crate::correction::Live> {
        let c = self.state.alignment.as_ref()?.correction.as_ref()?;
        Some(c.live_state(self.state.snapshot.as_ref()?))
    }
    /// Whether the correction can be taken to `target` now; the error says why not.
    pub fn correction_apply_readiness(
        &self,
        target: crate::correction::Target,
    ) -> Result<(), String> {
        let (a, live) = self.connected_alignment()?;
        crate::correction::apply_readiness(
            a,
            live,
            self.state.protocol_version.unwrap_or(0),
            target,
        )
    }
    /// Apply the correction, switch it for listening, or undo it.
    pub fn apply_correction(&mut self, target: crate::correction::Target) {
        if let Err(error) = self.correction_apply_readiness(target) {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        a.endpoint = self.state.endpoint.expect("checked by the readiness");
        self.state.job = Some(Job::ApplyCorrection { target });
        self.start_correction_job(a, move |backend, a, _, _| {
            crate::correction::apply(backend, a, target).map(Some)
        });
    }
    /// Whether the applied correction can be confirmed now.
    pub fn correction_verify_readiness(&self) -> Result<(), String> {
        let (a, live) = self.connected_alignment()?;
        crate::correction::verify_readiness(a, live)?;
        if self.settings != a.settings {
            return Err(
                "Restore the measurement settings this alignment was captured with.".into(),
            );
        }
        if self.selected_input().is_some_and(|i| i.is_monitor) {
            return Err("Choose an acoustic microphone, not a sink monitor.".into());
        }
        let first = crate::correction::verified_outputs(a);
        self.output_readiness(first.first().map_or("", String::as_str))
    }
    /// Measure the corrected speakers again at the main seat, through their filters.
    pub fn verify_correction(&mut self) {
        if let Err(error) = self.correction_verify_readiness() {
            self.state.error = Some(error);
            return;
        }
        let Some(mut a) = self.state.alignment.clone() else {
            return;
        };
        a.endpoint = self.state.endpoint.expect("checked by the readiness");
        let mut output_ids = crate::correction::verified_outputs(&a);
        if a.correction.as_ref().is_some_and(|c| c.subwoofer)
            && let Some(b) = a.bass.as_ref()
        {
            output_ids.extend(b.plan.speakers());
        }
        for c in [self.state.alignment.as_mut(), Some(&mut a)]
            .into_iter()
            .flatten()
            .filter_map(|a| a.correction.as_mut())
        {
            c.verification = None;
            c.verification_note = Some("Confirmation in progress.".into());
        }
        self.state.job = Some(Job::VerifyCorrection {
            output_ids,
            current: 0,
        });
        self.start_correction_job(a, |backend, a, cancel, progress| {
            crate::correction::verify(backend, a, cancel, progress).map(|()| None)
        });
    }
    fn start_correction_job(
        &mut self,
        mut a: alignment::Alignment,
        job: impl FnOnce(
            &dyn Backend,
            &mut alignment::Alignment,
            &AtomicBool,
            &mut dyn FnMut(usize, MeasureProgress),
        ) -> Result<Option<Snapshot>, String>
        + Send
        + 'static,
    ) {
        self.state.progress = Some(MeasureProgress::Preparing);
        self.state.stopping = false;
        self.state.error = None;
        self.cancelled = Arc::new(AtomicBool::new(false));
        let (backend, sender, cancel) = (
            self.backend.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
        );
        std::thread::spawn(move || {
            let result = catch_job(|| {
                job(backend.as_ref(), &mut a, &cancel, &mut |i, p| {
                    let _ = sender.send(Event::BatchProgress(i, p));
                })
            });
            // Every write gives the renderer a newer revision: read it again, whatever
            // happened.
            let live = match &result {
                Ok(Some(snapshot)) => Some(snapshot.clone()),
                _ => backend.discover(a.endpoint).ok(),
            };
            let _ = sender.send(Event::CorrectionUpdated(
                Box::new(a),
                result.map(|_| ()),
                live,
            ));
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
        if self.active_alignment_blocks() {
            self.state.error = Some("Restore the current alignment before loading another.".into());
            return;
        }
        self.state.job = Some(Job::LoadAlignment);
        self.state.error = None;
        let path = PathBuf::from(directory.trim());
        let sender = self.sender.clone();
        let live = self.state.endpoint.zip(self.state.snapshot.clone());
        std::thread::spawn(move || {
            let result = catch_job(|| {
                let mut a = alignment::load(&path)?;
                if let Some((endpoint, live)) = &live {
                    a.endpoint = *endpoint;
                    alignment::reconcile(&mut a, live)?;
                }
                LoadedAlignment::read(a)
            });
            let _ = sender.send(Event::AlignmentLoaded(Box::new(result)));
        });
    }
    /// Takes of `a`, each recorded with the sweep and reference it was measured with.
    fn append_alignment_takes(
        &mut self,
        a: &alignment::Alignment,
        revision: String,
        takes: Vec<(String, TakeOutcome)>,
    ) {
        self.append_takes(
            revision,
            takes
                .into_iter()
                .map(|(id, take)| (a.take_settings(&id), id, take))
                .collect(),
        );
    }
    fn append_takes(&mut self, revision: String, takes: Vec<RecordedTake>) {
        for (settings, output_id, outcome) in takes {
            self.state.takes.push(Take {
                output_id,
                revision: revision.clone(),
                settings,
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
            })
            | Some(Job::Position {
                output_ids,
                current,
                ..
            })
            | Some(Job::Bass {
                output_ids,
                current,
                ..
            })
            | Some(Job::VerifyBass {
                output_ids,
                current,
            })
            | Some(Job::VerifyCorrection {
                output_ids,
                current,
            }) => output_ids.get(*current).map(String::as_str),
            _ => None,
        }
    }
    /// Expected duration of an alignment over the checked speakers, when one can start.
    pub fn alignment_estimate(&self) -> Option<crate::duration::Estimate> {
        let outputs = self.alignment_readiness().ok()?;
        let groups = self.plan(&outputs).ok()?;
        Some(crate::duration::planned(
            &groups,
            2,
            self.settings.automatic_level,
        ))
    }
    /// Expected duration of one diagnostic pass over the checked speakers.
    pub fn batch_estimate(&self) -> Option<crate::duration::Estimate> {
        let outputs = self.batch_outputs().ok()?;
        Some(crate::duration::planned(
            &self.plan(&outputs).ok()?,
            1,
            false,
        ))
    }
    /// Expected duration of one take of a single speaker.
    pub fn take_estimate(&self, output_id: &str) -> crate::duration::Estimate {
        self.plan(&[output_id.into()]).map_or_else(
            |_| crate::duration::batch(self.settings.sweep, 1),
            |groups| crate::duration::planned(&groups, 1, false),
        )
    }
    /// Time left in the running capture, from its job and latest progress.
    pub fn remaining(&self) -> Option<Duration> {
        let planned = |ids: &[String], passes, probe| {
            self.plan(ids).map_or_else(
                |_| crate::duration::estimate(self.settings.sweep, ids.len(), passes),
                |groups| crate::duration::planned(&groups, passes, probe),
            )
        };
        let (estimate, pass, target) = match &self.state.job {
            Some(Job::Measure { output_id }) => {
                (planned(std::slice::from_ref(output_id), 1, false), 0, 0)
            }
            Some(Job::Remeasure { output_id, pass }) => {
                (planned(std::slice::from_ref(output_id), 2, false), *pass, 0)
            }
            Some(Job::Batch {
                output_ids,
                current,
            }) => (planned(output_ids, 1, false), 0, *current),
            Some(Job::Alignment {
                output_ids,
                current,
                pass,
            }) => (planned(output_ids, 2, false), *pass, *current),
            Some(Job::Position {
                output_ids,
                current,
                ..
            }) => (planned(output_ids, 1, false), 0, *current),
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
        for group in self.plan(&outputs)? {
            measurement_capture::sequence::capture_budget(
                group.sweep,
                snapshot.sample_rate,
                self.settings.sample_rate,
                group.outputs.len(),
            )?;
        }
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
            bass_managed: false,
            bypass_output_filters: false,
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
        let groups = self.plan(&outputs).expect("validated");
        std::thread::spawn(move || {
            let result = catch_job(|| {
                crate::sweep_plan::measure_groups(
                    &*backend,
                    &request,
                    &groups,
                    &cancel,
                    &mut |i, p| {
                        let _ = sender.send(Event::BatchProgress(i, p));
                    },
                )
            });
            let settings: Vec<_> = groups
                .iter()
                .flat_map(|g| {
                    g.outputs
                        .iter()
                        .map(|id| (id.clone(), crate::sweep_plan::take_settings(&settings, g)))
                })
                .collect();
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
                            self.append_alignment_takes(&a, revision, takes);
                            for retake in retakes {
                                self.append_alignment_takes(
                                    &a,
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
                            self.append_alignment_takes(
                                &outcome.alignment,
                                outcome.alignment.snapshot.revision.clone(),
                                outcome.takes,
                            );
                            // Takes at a lowered gain stay previous measurements.
                            for retake in outcome.retakes {
                                self.append_alignment_takes(
                                    &outcome.alignment,
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
                            self.append_takes(snapshot.revision.clone(), takes);
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
                    if let Some(
                        Job::Batch {
                            output_ids,
                            current,
                        }
                        | Job::Position {
                            output_ids,
                            current,
                            ..
                        }
                        | Job::Bass {
                            output_ids,
                            current,
                            ..
                        }
                        | Job::VerifyBass {
                            output_ids,
                            current,
                        }
                        | Job::VerifyCorrection {
                            output_ids,
                            current,
                        },
                    ) = &mut self.state.job
                    {
                        *current = index.min(output_ids.len().saturating_sub(1));
                    }
                    self.state.progress = Some(progress);
                }
                Event::CorrectionUpdated(a, result, live) => {
                    if let Err(e) = result {
                        self.state.error = Some(e);
                    }
                    if let Some(snapshot) = live {
                        self.state.saved_snapshot = Some(snapshot.clone());
                        self.state.snapshot = Some(snapshot);
                    }
                    self.state.alignment = Some(*a);
                    self.state.job = None;
                    self.state.progress = None;
                    self.state.stopping = false;
                }
                Event::BassUpdated(a, result, live) => {
                    if let Err(e) = result {
                        self.state.error = Some(e);
                    }
                    if let Some(snapshot) = live {
                        self.state.saved_snapshot = Some(snapshot.clone());
                        self.state.snapshot = Some(snapshot);
                    }
                    self.state.alignment = Some(*a);
                    self.state.job = None;
                    self.state.progress = None;
                    self.state.stopping = false;
                }
                Event::PositionMeasured(a, result, live) => {
                    if let Err(e) = result {
                        self.state.error = Some(e);
                    }
                    if let Some(live) = live {
                        self.state.saved_snapshot = Some(live.clone());
                        self.state.snapshot = Some(live);
                    } else if let Some(live) = &a.restored_snapshot
                        && self
                            .state
                            .alignment
                            .as_ref()
                            .is_none_or(|old| old.restored_snapshot.as_ref() != Some(live))
                    {
                        self.state.saved_snapshot = Some(live.clone());
                        self.state.snapshot = Some(live.clone());
                    }
                    self.state.alignment = Some(*a);
                    self.state.job = None;
                    self.state.progress = None;
                    self.state.stopping = false;
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
                                let settings = settings
                                    .iter()
                                    .find(|(id, _)| *id == output_id)
                                    .map_or_else(|| self.settings.clone(), |(_, s)| s.clone());
                                self.state.takes.push(Take {
                                    output_id,
                                    revision: revision.clone(),
                                    settings,
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
                Event::OutputSwitched(result) => {
                    match result {
                        Ok(snapshot) => {
                            self.state.saved_snapshot = Some(snapshot.clone());
                            self.state.snapshot = Some(snapshot);
                        }
                        Err(error) => self.state.error = Some(error),
                    }
                    self.state.job = None;
                }
                Event::SavedTakeLoaded(folder, result) => {
                    self.state.loading_takes.remove(&folder);
                    let outcome = match *result {
                        Ok(outcome) => outcome,
                        Err(e) => TakeOutcome {
                            directory: folder.clone(),
                            accepted: false,
                            cancelled: false,
                            rejection_reason: Some(format!("saved result unavailable: {e}")),
                            diagnostics: None,
                            timing: None,
                            impulse: None,
                            impulse_error: None,
                            deferred: false,
                        },
                    };
                    if let Some(take) = self.state.takes.iter_mut().find(|t| {
                        t.outcome
                            .as_ref()
                            .is_ok_and(|o| o.deferred && o.directory == folder)
                    }) {
                        take.outcome = Ok(outcome);
                    }
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
                Event::Discovered(endpoint, result, reconciled) => {
                    // Reaching the renderer replaces an older error, such as one a
                    // restart restored from a renderer that has since been replaced.
                    if result.is_ok() {
                        self.state.error = None;
                    }
                    match reconciled.map(|r| *r) {
                        Some(Ok(a)) => self.state.alignment = Some(a),
                        Some(Err(e)) => self.state.error = Some(e),
                        None => {}
                    }
                    match result {
                        Ok((snapshot, version)) => {
                            self.state.protocol_version = Some(version);
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
                            self.state.protocol_version = None;
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
        self.state
            .takes
            .iter()
            .rev()
            .find(|t| t.output_id == output_id && t.revision == *revision && self.measures_like(t))
    }
    /// The settings a take of `output_id` records now: its planned sweep and reference.
    pub fn take_settings(&self, output_id: &str) -> Settings {
        self.plan(&[output_id.into()]).map_or_else(
            |_| self.settings.clone(),
            |groups| crate::sweep_plan::take_settings(&self.settings, &groups[0]),
        )
    }
    /// Whether the current settings would measure `take`'s speaker as it was: the same
    /// settings, with the sweep and reference the plan gives it now. With automatic
    /// levels its playback level is the probe's, not a setting.
    pub fn measures_like(&self, take: &Take) -> bool {
        let mut expected = self.take_settings(&take.output_id);
        if self.settings.automatic_level {
            expected.sweep.peak_dbfs = take.settings.sweep.peak_dbfs;
        }
        take.settings == expected
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
