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
        })
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
                version: protocol::VERSION,
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
            if response.version != protocol::VERSION || response.request_id != request_id {
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

    pub fn discover(&mut self) -> Result<Snapshot> {
        match self.exchange(protocol::Command::Discover {})? {
            protocol::Reply::Snapshot { snapshot } => {
                if let Err(e) = validate_snapshot(&snapshot) {
                    return Err(self.fail(&e));
                }
                Ok(snapshot)
            }
            _ => Err(self.fail("expected a discovery snapshot")),
        }
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
                || progress.total_frames != progress.signal.frames(snapshot.sample_rate)
                || progress.rendered_frames > progress.total_frames
                || (progress.state == SignalState::Completed
                    && progress.rendered_frames != progress.total_frames))
        {
            return Err(self.fail("invalid signal progress"));
        }
        Ok(signal)
    }

    pub fn start(&mut self, signal_id: u64, output_id: &str, signal: Sweep) -> Result<Progress> {
        let session_id = self.session_id()?;
        let reply = self.exchange(protocol::Command::Start {
            session_id,
            signal_id,
            output_id: output_id.into(),
            signal,
        })?;
        let Some(progress) = self.accept_session(reply)? else {
            return Err(self.fail("missing signal acknowledgement"));
        };
        if progress.signal_id != signal_id
            || progress.output_id != output_id
            || progress.signal != signal
        {
            return Err(self.fail("renderer acknowledged a different signal"));
        }
        self.progress = Some(progress.clone());
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

    pub fn end(&mut self) -> Result<()> {
        let session_id = self.session_id()?;
        match self.exchange(protocol::Command::End { session_id })? {
            protocol::Reply::Ended {} => {
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

fn validate_snapshot(snapshot: &Snapshot) -> std::result::Result<(), String> {
    let mut names = std::collections::HashSet::new();
    if snapshot.instance_id.is_empty()
        || snapshot.instance_id.len() > 128
        || snapshot.revision.is_empty()
        || snapshot.revision.len() > 128
        || snapshot.outputs.len() > 512
        || !snapshot.master_gain_db.is_finite()
        || snapshot.max_delay_ms != protocol::MAX_DELAY_MS
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
        {
            return Err("invalid output in renderer snapshot".into());
        }
    }
    Ok(())
}
