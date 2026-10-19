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

    /// Discover in the current protocol version, or once in the older version
    /// an older renderer asks for, whose snapshots carry no output positions.
    pub fn discover(&mut self) -> Result<Snapshot> {
        let mut reply = self.exchange(protocol::Command::Discover {});
        if let Err(Error::Server { code, .. }) = &reply
            && code == "unsupported_version"
            && self.session.is_none()
            && let Some(version) = self.offered_version.take()
        {
            self.version = version;
            reply = self.exchange(protocol::Command::Discover {});
        }
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
        let reply = self.exchange(protocol::Command::SetOutputs {
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

    pub fn begin(&mut self, snapshot: &Snapshot) -> Result<()> {
        if self.session.is_some() {
            return Err(Error::Protocol("a session is already open".into()));
        }
        validate_snapshot(snapshot).map_err(Error::Protocol)?;
        if let Some(reason) = &snapshot.unavailable_reason {
            return Err(Error::Protocol(reason.clone()));
        }
        match self.exchange(protocol::Command::Begin {
            expected_revision: snapshot.revision.clone(),
        })? {
            protocol::Reply::Session {
                session_id,
                snapshot: confirmed,
                lease_ms,
                signal: None,
            } if confirmed == *snapshot
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
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::Start {
            session_id,
            signal_id,
            output_id: output_id.into(),
            reference_output_id: reference_output_id.map(str::to_owned),
            sequence_outputs: sequence_outputs.to_vec(),
            signal,
        })?;
        let Some(progress) = self.accept_session(reply)? else {
            return Err(self.fail("missing signal acknowledgement"));
        };
        if progress.signal_id != signal_id
            || progress.output_id != output_id
            || progress.sequence_outputs != sequence_outputs
            || progress.reference_output_id.as_deref() != reference_output_id
            || progress.signal != signal
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
    }
    normalized == *b
}
