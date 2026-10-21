//! Local TCP measurement control. No microphone capture or acoustic timestamps.
pub use omniphony_osc_contract::measurement as protocol;
use omniphony_osc_contract::stream;
pub use protocol::{Progress, SignalState, Snapshot, Sweep};
use rosc::{OscMessage, OscPacket, OscType};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("renderer connection: {0}")]
    Io(#[from] io::Error),
    #[error("invalid renderer protocol: {0}")]
    Protocol(String),
    #[error("renderer refused the request ({code}): {message}")]
    Server { code: String, message: String },
}
pub type Result<T> = std::result::Result<T, Error>;

/// Calls are sequential. Poll status at least once a second while a session is
/// open. Timeout/malformed replies close the connection; never retry a Start
/// blindly. Dropping the client disconnects its session owner.
pub struct Client {
    stream: TcpStream,
    timeout: Duration,
    next_request: u64,
    session: Option<(String, Snapshot)>,
    progress: Option<Progress>,
    closed: bool,
    noise_progress: Option<protocol::NoiseProgress>,
    /// Protocol version of this connection: [`protocol::VERSION`], or an older
    /// supported one an older renderer asked for during discovery.
    version: u32,
    /// The older version a renderer named when it refused this one.
    offered_version: Option<u32>,
}

impl Client {
    pub fn connect(endpoint: SocketAddr, timeout: Duration) -> Result<Self> {
        if !endpoint.ip().is_loopback() || timeout.is_zero() || timeout > Duration::from_secs(2) {
            return Err(Error::Protocol(
                "use a loopback endpoint and a timeout in (0, 2s]".into(),
            ));
        }
        let stream = TcpStream::connect_timeout(&endpoint, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self {
            stream,
            timeout,
            next_request: 1,
            session: None,
            progress: None,
            closed: false,
            noise_progress: None,
            version: protocol::VERSION,
            offered_version: None,
        })
    }

    /// The protocol version this connection speaks. Output positions need 5.
    pub fn protocol_version(&self) -> u32 {
        self.version
    }

    fn fail(&mut self, message: &str) -> Error {
        self.closed = true;
        let _ = self.stream.shutdown(Shutdown::Both);
        Error::Protocol(message.into())
    }

    fn exchange(&mut self, command: protocol::Command) -> Result<protocol::Reply> {
        if self.closed {
            return Err(Error::Protocol(
                "connection is closed; reconnect and discover again".into(),
            ));
        }
        let request_id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or_else(|| self.fail("request IDs exhausted"))?;
        let result = (|| {
            let json = serde_json::to_string(&protocol::Request {
                version: self.version,
                request_id,
                command,
            })
            .map_err(|e| Error::Protocol(e.to_string()))?;
            let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage {
                addr: protocol::REQUEST_ADDRESS.into(),
                args: vec![OscType::String(json)],
            }))
            .map_err(|e| Error::Protocol(e.to_string()))?;
            if bytes.len() > protocol::MAX_PACKET_BYTES {
                return Err(Error::Protocol("request is too large".into()));
            }
            self.stream.write_all(&stream::frame(&bytes)?)?;
            let mut reader = DeadlineReader {
                stream: &self.stream,
                deadline: Instant::now() + self.timeout,
            };
            let bytes = stream::read_frame(&mut reader, protocol::MAX_PACKET_BYTES)?
                .ok_or_else(|| Error::Protocol("renderer closed the connection".into()))?;
            let (remaining, packet) =
                rosc::decoder::decode_udp(&bytes).map_err(|e| Error::Protocol(e.to_string()))?;
            if !remaining.is_empty() {
                return Err(Error::Protocol("trailing OSC bytes".into()));
            }
            let OscPacket::Message(message) = packet else {
                return Err(Error::Protocol("expected a measurement reply".into()));
            };
            if message.addr != protocol::REPLY_ADDRESS {
                return Err(Error::Protocol(
                    "unexpected OSC reply address; update Omniphony to a compatible build".into(),
                ));
            }
            let [OscType::String(json)] = message.args.as_slice() else {
                return Err(Error::Protocol("expected one JSON string".into()));
            };
            let response: protocol::Response =
                serde_json::from_str(json).map_err(|e| Error::Protocol(e.to_string()))?;
            // An older renderer refuses this version in its own: a consistent
            // reply that discovery may retry in that version.
            if response.request_id == request_id
                && response.version < self.version
                && protocol::supported(response.version)
                && let protocol::Reply::Error { code, message } = &response.reply
                && code == "unsupported_version"
            {
                self.offered_version = Some(response.version);
                return Err(Error::Server {
                    code: code.clone(),
                    message: message.clone(),
                });
            }
            if response.version != self.version || response.request_id != request_id {
                return Err(Error::Protocol(
                    "reply version or request ID does not match".into(),
                ));
            }
            match response.reply {
                protocol::Reply::Error { code, message } => Err(Error::Server { code, message }),
                reply => Ok(reply),
            }
        })();
        if result
            .as_ref()
            .is_err_and(|e| !matches!(e, Error::Server { .. }))
        {
            self.closed = true;
            let _ = self.stream.shutdown(Shutdown::Both);
        }
        result
    }

    /// Send in the current protocol version, or once more in the older version an
    /// older renderer asks for when it refuses this one. Only outside a session:
    /// a refused request changed nothing, so resending it is safe. Any first
    /// request on a connection may meet an older renderer (applying settings
    /// opens a connection without discovering first).
    fn exchange_negotiated(&mut self, command: protocol::Command) -> Result<protocol::Reply> {
        let reply = self.exchange(command.clone());
        if let Err(Error::Server { code, .. }) = &reply
            && code == "unsupported_version"
            && self.session.is_none()
            && let Some(version) = self.offered_version.take()
        {
            self.version = version;
            return self.exchange(command);
        }
        reply
    }

    /// Discover in the current protocol version, or once in the older version
    /// an older renderer asks for, whose snapshots carry no output positions.
    pub fn discover(&mut self) -> Result<Snapshot> {
        let reply = self.exchange_negotiated(protocol::Command::Discover {});
        match reply? {
            protocol::Reply::Snapshot { snapshot } => {
                if let Err(e) = validate_snapshot(&snapshot) {
                    return Err(self.fail(&e));
                }
                Ok(snapshot)
            }
            _ => Err(self.fail("expected a discovery snapshot")),
        }
    }

    /// Switch the renderer's live output from headphones to the speakers (protocol
    /// 7). Discovers first, so an older renderer is named instead of refusing an
    /// unknown command.
    pub fn use_speaker_output(&mut self) -> Result<Snapshot> {
        self.discover()?;
        if self.version < protocol::SPEAKER_OUTPUT_VERSION {
            return Err(Error::Protocol(
                "this Omniphony build cannot switch its output from the Companion; choose Speakers in Studio, or update Omniphony".into(),
            ));
        }
        match self.exchange(protocol::Command::UseSpeakerOutput {})? {
            protocol::Reply::Snapshot { snapshot } => {
                validate_snapshot(&snapshot).map_err(|e| self.fail(&e))?;
                Ok(snapshot)
            }
            _ => Err(self.fail("expected a snapshot after switching the output")),
        }
    }

    /// Absolute, revision-checked live settings. A lost acknowledgement is not success.
    pub fn set_outputs(
        &mut self,
        expected: &Snapshot,
        settings: Vec<protocol::OutputSetting>,
    ) -> Result<Snapshot> {
        if self.session.is_some() {
            return Err(Error::Protocol(
                "end measurement before applying settings".into(),
            ));
        }
        validate_snapshot(expected).map_err(Error::Protocol)?;
        let mut target = expected.clone();
        let mut seen = std::collections::HashSet::new();
        if settings.is_empty() {
            return Err(Error::Protocol("no output settings".into()));
        }
        for setting in &settings {
            if !seen.insert(&setting.output_id)
                || !setting.gain_db.is_finite()
                || !(-60.0..=24.0).contains(&setting.gain_db)
                || !setting.delay_ms.is_finite()
                || !(0.0..=expected.max_delay_ms).contains(&setting.delay_ms)
            {
                return Err(Error::Protocol("invalid gain/delay target".into()));
            }
            let output = target
                .outputs
                .iter_mut()
                .find(|o| o.id == setting.output_id)
                .ok_or_else(|| Error::Protocol("unknown output target".into()))?;
            output.gain_db = setting.gain_db;
            output.delay_ms = setting.delay_ms;
        }
        let reply = self.exchange_negotiated(protocol::Command::SetOutputs {
            expected_revision: expected.revision.clone(),
            settings,
        })?;
        let protocol::Reply::Snapshot { snapshot } = reply else {
            return Err(
                self.fail("missing settings acknowledgement; application outcome is uncertain")
            );
        };
        validate_snapshot(&snapshot).map_err(|e| self.fail(&e))?;
        if !same_configuration(&snapshot, &target) {
            return Err(self.fail("applied state differs from target; refresh before proceeding"));
        }
        Ok(snapshot)
    }

    /// Upload one room correction filter (protocol 8): `taps` at `sample_rate`, in
    /// chunks of at most [`protocol::MAX_UPLOAD_CHUNK_TAPS`]. Returns the upload id that
    /// [`Self::set_output_filters`] applies, once the renderer holds every tap with the
    /// hash of what was sent. Uploading only stores the taps: nothing plays them yet.
    pub fn upload_filter(&mut self, sample_rate: u32, taps: &[f32]) -> Result<String> {
        if self.version < protocol::FILTERS_VERSION {
            return Err(Error::Protocol(FILTERS_UNSUPPORTED.into()));
        }
        if taps.is_empty()
            || taps.iter().any(|t| !t.is_finite())
            || taps.iter().all(|&t| t == 0.0)
            || taps.len() > protocol::max_filter_taps(sample_rate)
        {
            return Err(Error::Protocol(
                "a filter needs finite taps, not all zero, at most 1.5 s long".into(),
            ));
        }
        let total_taps =
            u32::try_from(taps.len()).map_err(|_| Error::Protocol("filter is too long".into()))?;
        let hash = protocol::taps_hash_hex(taps);
        let mut upload_id: Option<String> = None;
        let mut offset = 0usize;
        loop {
            let end = (offset + protocol::MAX_UPLOAD_CHUNK_TAPS).min(taps.len());
            let reply = self.exchange(protocol::Command::UploadFilter {
                upload_id: upload_id.clone(),
                sample_rate,
                total_taps,
                offset: offset as u32,
                taps: protocol::encode_taps(&taps[offset..end]),
            })?;
            let protocol::Reply::FilterUpload { upload } = reply else {
                return Err(self.fail("expected the state of a filter upload"));
            };
            if upload.upload_id.is_empty()
                || upload.upload_id.len() > 128
                || upload_id.as_ref().is_some_and(|id| *id != upload.upload_id)
                || upload.sample_rate != sample_rate
                || upload.total_taps != total_taps
                || upload.received_taps as usize != end
            {
                return Err(self.fail("the renderer acknowledged another filter upload"));
            }
            upload_id = Some(upload.upload_id);
            offset = end;
            if offset == taps.len() {
                if upload.hash.as_deref() != Some(hash.as_str()) {
                    return Err(self.fail("the renderer holds other taps than were uploaded"));
                }
                return Ok(upload_id.expect("set above"));
            }
        }
    }

    /// Replace the filters of some outputs at once (protocol 8): an upload id from
    /// [`Self::upload_filter`], or `None` for no filter. Revision-checked and live only,
    /// like [`Self::set_outputs`]; refused during a session. A lost acknowledgement is
    /// not success.
    pub fn set_output_filters(
        &mut self,
        expected: &Snapshot,
        filters: Vec<protocol::OutputFilterSetting>,
    ) -> Result<Snapshot> {
        if self.session.is_some() {
            return Err(Error::Protocol(
                "end measurement before changing filters".into(),
            ));
        }
        if self.version < protocol::FILTERS_VERSION {
            return Err(Error::Protocol(FILTERS_UNSUPPORTED.into()));
        }
        validate_snapshot(expected).map_err(Error::Protocol)?;
        let mut seen = std::collections::HashSet::new();
        if filters.is_empty()
            || filters.iter().any(|f| {
                !seen.insert(&f.output_id) || !expected.outputs.iter().any(|o| o.id == f.output_id)
            })
        {
            return Err(Error::Protocol("invalid output filter targets".into()));
        }
        let reply = self.exchange(protocol::Command::SetOutputFilters {
            expected_revision: expected.revision.clone(),
            outputs: filters.clone(),
        })?;
        let protocol::Reply::Snapshot { snapshot } = reply else {
            return Err(self.fail("missing filter acknowledgement; the outcome is uncertain"));
        };
        validate_snapshot(&snapshot).map_err(|e| self.fail(&e))?;
        // Gains, delays and every output left alone stay as they were.
        if !same_installation(&snapshot, expected)
            || filters.iter().any(|f| {
                let output = snapshot.outputs.iter().find(|o| o.id == f.output_id);
                match (&f.filter, output.and_then(|o| o.filter.as_ref())) {
                    (None, None) => false,
                    (Some(_), Some(filter)) => filter.origin != protocol::FilterOrigin::Live,
                    _ => true,
                }
            })
        {
            return Err(
                self.fail("the renderer reads back other filters; refresh before proceeding")
            );
        }
        Ok(snapshot)
    }

    pub fn begin(&mut self, snapshot: &Snapshot) -> Result<()> {
        self.begin_with(snapshot, false)
    }

    /// Begin a session; with `bypass_output_filters` (protocol 8) every output plays
    /// without its room correction filter until the session ends, however it ends.
    pub fn begin_with(&mut self, snapshot: &Snapshot, bypass_output_filters: bool) -> Result<()> {
        if self.session.is_some() {
            return Err(Error::Protocol("a session is already open".into()));
        }
        validate_snapshot(snapshot).map_err(Error::Protocol)?;
        if let Some(reason) = &snapshot.unavailable_reason {
            return Err(Error::Protocol(reason.clone()));
        }
        if bypass_output_filters && self.version < protocol::FILTERS_VERSION {
            return Err(Error::Protocol(FILTERS_UNSUPPORTED.into()));
        }
        // The session's snapshot says whether it holds the filters off.
        let mut expected = snapshot.clone();
        expected.output_filters_bypassed = bypass_output_filters;
        match self.exchange_negotiated(protocol::Command::Begin {
            expected_revision: snapshot.revision.clone(),
            bypass_output_filters,
        })? {
            protocol::Reply::Session {
                session_id,
                snapshot: confirmed,
                lease_ms,
                signal: None,
            } if confirmed == expected
                && !session_id.is_empty()
                && session_id.len() <= 128
                && lease_ms == protocol::LEASE_MS =>
            {
                self.session = Some((session_id, confirmed));
                self.progress = None;
                Ok(())
            }
            _ => {
                Err(self
                    .fail("session acknowledgement does not match the discovered configuration"))
            }
        }
    }

    fn session_id(&self) -> Result<String> {
        self.session
            .as_ref()
            .map(|(id, _)| id.clone())
            .ok_or_else(|| Error::Protocol("begin a session first".into()))
    }

    fn accept_session(&mut self, reply: protocol::Reply) -> Result<Option<Progress>> {
        let protocol::Reply::Session {
            session_id,
            snapshot,
            lease_ms,
            signal,
        } = reply
        else {
            return Err(self.fail("expected session progress"));
        };
        if self.session.as_ref() != Some(&(session_id, snapshot.clone()))
            || lease_ms != protocol::LEASE_MS
        {
            return Err(self.fail("session identity or configuration changed"));
        }
        if let Some(progress) = &signal
            && (progress.signal.validate(snapshot.sample_rate).is_err()
                || !snapshot.outputs.iter().any(|o| o.id == progress.output_id)
                || progress.signal_id == 0
                || progress
                    .reference_output_id
                    .as_ref()
                    .is_some_and(|id| !snapshot.outputs.iter().any(|o| &o.id == id))
                || !valid_sequence(progress, &snapshot)
                || progress.rendered_frames > progress.total_frames
                || (progress.state == SignalState::Completed
                    && progress.rendered_frames != progress.total_frames))
        {
            return Err(self.fail("invalid signal progress"));
        }
        Ok(signal)
    }

    pub fn start(&mut self, signal_id: u64, output_id: &str, signal: Sweep) -> Result<Progress> {
        self.start_referenced(signal_id, output_id, None, signal)
    }

    pub fn start_referenced(
        &mut self,
        signal_id: u64,
        output_id: &str,
        reference_output_id: Option<&str>,
        signal: Sweep,
    ) -> Result<Progress> {
        self.start_sequence(signal_id, output_id, reference_output_id, &[], signal)
    }

    pub fn start_sequence(
        &mut self,
        signal_id: u64,
        output_id: &str,
        reference_output_id: Option<&str>,
        sequence_outputs: &[String],
        signal: Sweep,
    ) -> Result<Progress> {
        self.start_sequence_with(
            signal_id,
            output_id,
            reference_output_id,
            sequence_outputs,
            signal,
            false,
        )
    }

    /// [`Self::start_sequence`]; with `bass_managed` (protocol 8) each target's sweep
    /// plays as a source at its layout position, through the crossover and bass
    /// management, while the reference sweeps stay direct. A renderer whose filters are
    /// still loading or switching (`filters_settling`, protocol 8) has played nothing:
    /// the same signal is started again shortly, the session kept alive meanwhile.
    pub fn start_sequence_with(
        &mut self,
        signal_id: u64,
        output_id: &str,
        reference_output_id: Option<&str>,
        sequence_outputs: &[String],
        signal: Sweep,
        bass_managed: bool,
    ) -> Result<Progress> {
        let session_id = self.session_id()?;
        if bass_managed && self.version < protocol::FILTERS_VERSION {
            return Err(Error::Protocol(FILTERS_UNSUPPORTED.into()));
        }
        let deadline = Instant::now() + SETTLING_LIMIT;
        let mut next_status = Instant::now() + SETTLING_STATUS;
        let reply = loop {
            let reply = self.exchange(protocol::Command::Start {
                session_id: session_id.clone(),
                signal_id,
                output_id: output_id.into(),
                reference_output_id: reference_output_id.map(str::to_owned),
                sequence_outputs: sequence_outputs.to_vec(),
                signal,
                bass_managed,
            });
            match reply {
                Err(Error::Server { code, .. })
                    if code == "filters_settling" && Instant::now() < deadline =>
                {
                    std::thread::sleep(SETTLING_RETRY);
                    if Instant::now() >= next_status {
                        self.status()?;
                        next_status = Instant::now() + SETTLING_STATUS;
                    }
                }
                reply => break reply?,
            }
        };
        let Some(progress) = self.accept_session(reply)? else {
            return Err(self.fail("missing signal acknowledgement"));
        };
        if progress.signal_id != signal_id
            || progress.output_id != output_id
            || progress.sequence_outputs != sequence_outputs
            || progress.reference_output_id.as_deref() != reference_output_id
            || progress.signal != signal
            || progress.bass_managed != bass_managed
        {
            return Err(self.fail("renderer acknowledged a different signal"));
        }
        self.progress = Some(progress.clone());
        self.noise_progress = None;
        Ok(progress)
    }

    pub fn status(&mut self) -> Result<Option<Progress>> {
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::Status { session_id })?;
        self.update_progress(reply)
    }

    fn update_progress(&mut self, reply: protocol::Reply) -> Result<Option<Progress>> {
        let progress = self.accept_session(reply)?;
        match (&self.progress, &progress) {
            (Some(old), Some(new))
                if old.signal_id == new.signal_id
                    && old.output_id == new.output_id
                    && old.reference_output_id == new.reference_output_id
                    && old.sequence_outputs == new.sequence_outputs
                    && old.signal == new.signal
                    && old.bass_managed == new.bass_managed
                    && new.rendered_frames >= old.rendered_frames
                    && (matches!(old.state, SignalState::Queued | SignalState::Rendering)
                        || old.state == new.state)
                    && !(old.state == SignalState::Rendering
                        && new.state == SignalState::Queued) => {}
            (None, None) => {}
            _ => {
                return Err(
                    self.fail("signal identity changed or rendered progress went backwards")
                );
            }
        }
        self.progress = progress.clone();
        Ok(progress)
    }

    pub fn stop(&mut self) -> Result<Option<Progress>> {
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::Stop { session_id })?;
        let progress = self.update_progress(reply)?;
        if progress
            .as_ref()
            .is_some_and(|p| matches!(p.state, SignalState::Queued | SignalState::Rendering))
        {
            return Err(self.fail("stop was not acknowledged"));
        }
        Ok(progress)
    }

    /// Finite, leased physical-level noise. Older renderers reject the optional
    /// command; never fall back to the legacy unbounded speaker-test control.
    pub fn start_noise(
        &mut self,
        signal_id: u64,
        output_id: &str,
        signal: protocol::LevelNoise,
    ) -> Result<protocol::NoiseProgress> {
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::StartNoise {
            session_id,
            signal_id,
            output_id: output_id.into(),
            signal,
        }).map_err(|error| match error {
            Error::Server { code, message } if code == "invalid_request" => Error::Server {
                code,
                message: format!("{message}. Manual level noise requires a compatible Omniphony calibration build; update Omniphony."),
            },
            other => other,
        })?;
        let progress = self.accept_noise(reply)?;
        if progress.signal_id != signal_id
            || progress.output_id != output_id
            || progress.signal != signal
        {
            return Err(self.fail("renderer acknowledged different manual-level noise"));
        }
        self.noise_progress = Some(progress.clone());
        self.progress = None;
        Ok(progress)
    }

    fn accept_noise(&mut self, reply: protocol::Reply) -> Result<protocol::NoiseProgress> {
        let protocol::Reply::NoiseSession {
            session_id,
            snapshot,
            lease_ms,
            signal,
        } = reply
        else {
            return Err(self.fail("expected finite level-noise progress; update Omniphony"));
        };
        if self.session.as_ref() != Some(&(session_id, snapshot.clone()))
            || lease_ms != protocol::LEASE_MS
            || signal.signal.validate(snapshot.sample_rate).is_err()
            || signal.signal_id == 0
            || !snapshot.outputs.iter().any(|o| o.id == signal.output_id)
            || signal.total_frames != signal.signal.frames(snapshot.sample_rate)
            || signal.rendered_frames > signal.total_frames
            || (signal.state == SignalState::Completed
                && signal.rendered_frames != signal.total_frames)
        {
            return Err(self.fail("invalid manual-level noise progress or session identity"));
        }
        Ok(signal)
    }

    fn update_noise(&mut self, reply: protocol::Reply) -> Result<protocol::NoiseProgress> {
        let next = self.accept_noise(reply)?;
        let Some(previous) = &self.noise_progress else {
            return Err(self.fail("no manual-level signal started"));
        };
        if next.signal_id != previous.signal_id
            || next.output_id != previous.output_id
            || next.signal != previous.signal
            || next.rendered_frames < previous.rendered_frames
            || (!matches!(previous.state, SignalState::Queued | SignalState::Rendering)
                && previous.state != next.state)
            || (previous.state == SignalState::Rendering && next.state == SignalState::Queued)
        {
            return Err(self.fail("manual-level signal identity or progress changed"));
        }
        self.noise_progress = Some(next.clone());
        Ok(next)
    }
    pub fn noise_status(&mut self) -> Result<protocol::NoiseProgress> {
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::Status { session_id })?;
        self.update_noise(reply)
    }
    pub fn stop_noise(&mut self) -> Result<protocol::NoiseProgress> {
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::Stop { session_id })?;
        let progress = self.update_noise(reply)?;
        if matches!(progress.state, SignalState::Queued | SignalState::Rendering) {
            return Err(self.fail("manual-level noise stop was not acknowledged"));
        }
        Ok(progress)
    }

    pub fn end(&mut self) -> Result<()> {
        let session_id = self.session_id()?;
        match self.exchange(protocol::Command::End { session_id })? {
            protocol::Reply::Ended {} => {
                self.noise_progress = None;
                self.session = None;
                self.progress = None;
                Ok(())
            }
            _ => Err(self.fail("end was not acknowledged")),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

struct DeadlineReader<'a> {
    stream: &'a TcpStream,
    deadline: Instant,
}
impl Read for DeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "measurement reply deadline"))?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

const FILTERS_UNSUPPORTED: &str = "this Omniphony build cannot measure or apply room correction filters (protocol 8); update Omniphony";
/// How long a Start refused with `filters_settling` is tried again, and how often.
const SETTLING_LIMIT: Duration = Duration::from_secs(5);
const SETTLING_RETRY: Duration = Duration::from_millis(100);
/// The lease is renewed this often while a Start waits for the filters.
const SETTLING_STATUS: Duration = Duration::from_millis(500);

/// Farther than any room: a declared distance beyond it is not a speaker position.
const MAX_DISTANCE_M: f32 = 1_000.0;

fn validate_snapshot(snapshot: &Snapshot) -> std::result::Result<(), String> {
    let mut names = std::collections::HashSet::new();
    if snapshot.instance_id.is_empty()
        || snapshot.instance_id.len() > 128
        || snapshot.revision.is_empty()
        || snapshot.revision.len() > 128
        || snapshot.outputs.len() > 512
        || !snapshot.master_gain_db.is_finite()
        || !snapshot.max_delay_ms.is_finite()
        || !(0.0..=protocol::SETTLE_MS as f32).contains(&snapshot.max_delay_ms)
    {
        return Err("invalid renderer snapshot".into());
    }
    for (index, output) in snapshot.outputs.iter().enumerate() {
        if output.index as usize != index
            || output.id.is_empty()
            || output.id.len() > 128
            || !names.insert(&output.id)
            || output.name != output.id
            || !output.gain_db.is_finite()
            || !output.delay_ms.is_finite()
            || output
                .freq_low_hz
                .is_some_and(|v| !v.is_finite() || v < 0.0)
            || output
                .freq_high_hz
                .is_some_and(|v| !v.is_finite() || v <= 0.0)
            || output
                .azimuth_deg
                .is_some_and(|v| !(-180.0..=180.0).contains(&v))
            || output
                .elevation_deg
                .is_some_and(|v| !(-90.0..=90.0).contains(&v))
            || output
                .distance_m
                .is_some_and(|v| !(0.0..=MAX_DISTANCE_M).contains(&v))
        {
            return Err("invalid output in renderer snapshot".into());
        }
    }
    Ok(())
}

fn valid_sequence(p: &Progress, snapshot: &Snapshot) -> bool {
    if p.sequence_outputs.is_empty() {
        return p.total_frames
            == p.signal.frames(snapshot.sample_rate)
                * if p.reference_output_id.is_some() {
                    3
                } else {
                    1
                };
    }
    let mut seen = std::collections::HashSet::new();
    p.reference_output_id.is_some()
        && p.sequence_outputs.first() == Some(&p.output_id)
        && p.sequence_outputs
            .iter()
            .all(|id| seen.insert(id) && snapshot.outputs.iter().any(|o| &o.id == id))
        && protocol::sequence_frames(p.signal, snapshot.sample_rate, p.sequence_outputs.len())
            == Ok(p.total_frames)
}

/// Whether two snapshots describe the same installation in the same state, whichever
/// renderer process published them: the same outputs in the same order (identity, name,
/// index, mute and band), sample rate, master gain, delay capacity, and gains and delays
/// within float roundoff. The address it was reached at, the instance identity (new at
/// every start) and the revision are not part of it, nor are layout positions, which
/// describe panning rather than the acoustic path and are absent from older protocols.
pub fn same_installation(a: &Snapshot, b: &Snapshot) -> bool {
    same_settings_within(a, b, 0.0001, 0.0001)
}

/// [`same_installation`] with explicit gain and delay tolerances, for settings that
/// another program may have republished rounded (Studio keeps gains to 0.1 dB).
pub fn same_settings_within(a: &Snapshot, b: &Snapshot, gain_db: f32, delay_ms: f32) -> bool {
    a.sample_rate == b.sample_rate
        && a.master_gain_db.to_bits() == b.master_gain_db.to_bits()
        && a.max_delay_ms.to_bits() == b.max_delay_ms.to_bits()
        && a.outputs.len() == b.outputs.len()
        && a.outputs.iter().zip(&b.outputs).all(|(x, y)| {
            x.id == y.id
                && x.name == y.name
                && x.index == y.index
                && x.muted == y.muted
                && x.freq_low_hz == y.freq_low_hz
                && x.freq_high_hz == y.freq_high_hz
                && x.gain_db.is_finite()
                && x.delay_ms.is_finite()
                && (x.gain_db - y.gain_db).abs() <= gain_db
                && (x.delay_ms - y.delay_ms).abs() <= delay_ms
        })
}

/// Compare physical state while ignoring the revision assigned to its publication.
/// Gain conversion through a float linear amplitude permits a tiny roundoff only.
pub fn same_configuration(a: &Snapshot, b: &Snapshot) -> bool {
    let mut normalized = a.clone();
    normalized.revision.clone_from(&b.revision);
    if a.outputs.len() != b.outputs.len() {
        return false;
    }
    for (out, expected) in normalized.outputs.iter_mut().zip(&b.outputs) {
        if !out.gain_db.is_finite()
            || !out.delay_ms.is_finite()
            || (out.gain_db - expected.gain_db).abs() > 0.0001
            || (out.delay_ms - expected.delay_ms).abs() > 0.0001
        {
            return false;
        }
        out.gain_db = expected.gain_db;
        out.delay_ms = expected.delay_ms;
        // A filter that finished loading meanwhile is still the same filter.
        if let (Some(filter), Some(was)) = (&out.filter, &expected.filter)
            && filter.origin == was.origin
            && filter.file == was.file
            && (was.hash.is_none() || filter.hash == was.hash)
            && was.state == protocol::FilterLoadState::Pending
        {
            out.filter = expected.filter.clone();
        }
    }
    normalized == *b
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(state: protocol::FilterLoadState, hash: Option<&str>) -> Snapshot {
        Snapshot {
            instance_id: "test".into(),
            revision: "test:1".into(),
            sample_rate: 48000,
            master_gain_db: 0.0,
            max_delay_ms: 250.0,
            unavailable_reason: None,
            output_filters_bypassed: false,
            outputs: vec![protocol::Output {
                id: "L".into(),
                name: "L".into(),
                index: 0,
                gain_db: 0.0,
                delay_ms: 0.0,
                muted: false,
                freq_low_hz: None,
                freq_high_hz: None,
                azimuth_deg: None,
                elevation_deg: None,
                distance_m: None,
                filter: Some(protocol::OutputFilter {
                    origin: protocol::FilterOrigin::File,
                    file: Some("L.wav".into()),
                    state,
                    sample_rate: hash.map(|_| 48000),
                    taps: hash.map(|_| 8192),
                    hash: hash.map(Into::into),
                    error: None,
                }),
            }],
        }
    }
    #[test]
    fn a_filter_that_finished_loading_is_the_same_configuration() {
        use protocol::FilterLoadState::*;
        let pending = snapshot(Pending, None);
        let loaded = snapshot(Loaded, Some("0123456789abcdef"));
        assert!(same_configuration(&loaded, &pending));
        // Not the other way round, nor another filter.
        assert!(!same_configuration(&pending, &loaded));
        let mut other = loaded.clone();
        other.outputs[0].filter.as_mut().unwrap().file = Some("other.wav".into());
        assert!(!same_configuration(&other, &pending));
    }
}
