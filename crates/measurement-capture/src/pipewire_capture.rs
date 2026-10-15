//! Native PipeWire capture. Enumeration is read-only; a target is pinned by serial.
//! The session manager must not substitute the default source after target removal.
use super::*;
use pipewire as pw;
use pw::{properties::properties, spa};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Node {
    id: u32,
    serial: String,
    name: String,
    description: String,
    monitor: bool,
}
#[derive(Clone)]
struct Port {
    node: u32,
    index: u32,
    label: String,
    channel: String,
}
struct Target {
    device: InputDevice,
    serial: String,
    positions: Vec<u32>,
}

fn targets() -> Result<Vec<Target>> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry()?;
    let nodes = Rc::new(RefCell::new(Vec::<Node>::new()));
    let ports = Rc::new(RefCell::new(Vec::<Port>::new()));
    let nodes_cb = nodes.clone();
    let ports_cb = ports.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props.as_ref() else {
                return;
            };
            if global.type_ == pw::types::ObjectType::Node {
                let class = props.get("media.class").unwrap_or("");
                let monitor = class == "Audio/Sink";
                if !monitor && class != "Audio/Source" && class != "Audio/Source/Virtual" {
                    return;
                }
                let (Some(name), Some(serial)) =
                    (props.get("node.name"), props.get("object.serial"))
                else {
                    return;
                };
                if nodes_cb.borrow().len() < 4096 {
                    nodes_cb.borrow_mut().push(Node {
                        id: global.id,
                        serial: serial.into(),
                        name: name.into(),
                        description: props
                            .get("node.description")
                            .or_else(|| props.get("node.nick"))
                            .unwrap_or(name)
                            .into(),
                        monitor,
                    });
                }
            } else if global.type_ == pw::types::ObjectType::Port
                && props.get("port.direction") == Some("out")
                && props
                    .get("format.dsp")
                    .is_some_and(|format| format.ends_with(" audio"))
            {
                let Some(node) = props.get("node.id").and_then(|n| n.parse().ok()) else {
                    return;
                };
                let Some(index) = props.get("port.id").and_then(|n| n.parse().ok()) else {
                    return;
                };
                if ports_cb.borrow().len() < 16384 {
                    ports_cb.borrow_mut().push(Port {
                        node,
                        index,
                        label: props.get("port.name").unwrap_or("Input").into(),
                        channel: props.get("audio.channel").unwrap_or("UNK").into(),
                    });
                }
            }
        })
        .register();
    let done = Rc::new(Cell::new(false));
    let error = Rc::new(RefCell::new(None));
    let sequence = core.sync(0)?;
    let done_cb = done.clone();
    let error_cb = error.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == sequence {
                done_cb.set(true);
            }
        })
        .error(move |_, _, _, message| {
            *error_cb.borrow_mut() = Some(message.to_string());
        })
        .register();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done.get() {
        if let Some(error) = error.borrow_mut().take() {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Err("PipeWire discovery timed out".into());
        }
        mainloop.loop_().iterate(Duration::from_millis(20));
    }
    Ok(assemble(&nodes.borrow(), &ports.borrow()))
}

fn assemble(nodes: &[Node], ports: &[Port]) -> Vec<Target> {
    let mut targets = Vec::new();
    for node in nodes {
        let ordered: BTreeMap<_, _> = ports
            .iter()
            .filter(|p| p.node == node.id)
            .map(|p| (p.index, p))
            .collect();
        // Ports describe the exposed channels, including virtual nodes that
        // do not advertise audio.channels. Never invent a 64-channel device.
        if ordered.is_empty() || ordered.len() > 64 {
            continue;
        }
        let channels = ordered.len() as u16;
        let kind = if node.monitor { "monitor" } else { "source" };
        targets.push(Target {
            device: InputDevice {
                name: format!("pipewire:{kind}:{}", node.name),
                description: node.description.clone(),
                channel_names: ordered.values().map(|p| p.label.clone()).collect(),
                is_monitor: node.monitor,
                configurations: vec![InputConfiguration {
                    channels,
                    min_sample_rate: 8000,
                    max_sample_rate: 192000,
                    sample_format: "f32".into(),
                }],
            },
            serial: node.serial.clone(),
            positions: ordered.values().map(|p| position(&p.channel)).collect(),
        });
    }
    targets.sort_by(|a, b| {
        (a.device.is_monitor, &a.device.description, &a.device.name).cmp(&(
            b.device.is_monitor,
            &b.device.description,
            &b.device.name,
        ))
    });
    targets
}

fn position(channel: &str) -> u32 {
    use spa::sys::*;
    match channel {
        "MONO" => SPA_AUDIO_CHANNEL_MONO,
        "FL" => SPA_AUDIO_CHANNEL_FL,
        "FR" => SPA_AUDIO_CHANNEL_FR,
        "FC" => SPA_AUDIO_CHANNEL_FC,
        "LFE" => SPA_AUDIO_CHANNEL_LFE,
        "RL" => SPA_AUDIO_CHANNEL_RL,
        "RR" => SPA_AUDIO_CHANNEL_RR,
        "SL" => SPA_AUDIO_CHANNEL_SL,
        "SR" => SPA_AUDIO_CHANNEL_SR,
        "RC" => SPA_AUDIO_CHANNEL_RC,
        "FLC" => SPA_AUDIO_CHANNEL_FLC,
        "FRC" => SPA_AUDIO_CHANNEL_FRC,
        "TC" => SPA_AUDIO_CHANNEL_TC,
        "TFL" => SPA_AUDIO_CHANNEL_TFL,
        "TFR" => SPA_AUDIO_CHANNEL_TFR,
        "TFC" => SPA_AUDIO_CHANNEL_TFC,
        "TRL" => SPA_AUDIO_CHANNEL_TRL,
        "TRR" => SPA_AUDIO_CHANNEL_TRR,
        "TRC" => SPA_AUDIO_CHANNEL_TRC,
        _ => channel
            .strip_prefix("AUX")
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| *n < 64)
            .map_or(SPA_AUDIO_CHANNEL_UNKNOWN, |n| SPA_AUDIO_CHANNEL_AUX0 + n),
    }
}

pub(super) fn devices() -> Result<Vec<InputDevice>> {
    Ok(targets()?.into_iter().map(|t| t.device).collect())
}

struct Process {
    sink: Sink,
    valid: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    previous: Option<(Instant, usize)>,
}
impl Process {
    fn accept(&mut self, bytes: &[u8], offset: usize, size: usize, stride: i32, empty: bool) {
        let frame_bytes = self.sink.channels * 4;
        let Some(end) = offset.checked_add(size) else {
            self.fail();
            return;
        };
        let Some(bytes) = bytes.get(offset..end) else {
            self.fail();
            return;
        };
        if (stride != 0 && stride != frame_bytes as i32) || !size.is_multiple_of(frame_bytes) {
            self.fail();
            return;
        }
        let frames = size / frame_bytes;
        if frames == 0 {
            return;
        }
        let now = Instant::now();
        if let Some((previous, previous_frames)) = self.previous {
            let expected = previous_frames as f64 / self.sink.rate as f64;
            // Callback cadence catches long stalls; it is not a device timestamp
            // or proof of sample continuity. Record that limitation in metadata.
            if (now.duration_since(previous).as_secs_f64() - expected).abs() > 0.1 {
                self.sink.faults.fetch_or(TIMESTAMP_GAP, Ordering::Relaxed);
            }
        }
        self.previous = Some((now, frames));
        let channel_byte = self.sink.channel * 4;
        for frame in bytes.chunks_exact(frame_bytes) {
            let sample = if empty {
                0.0
            } else {
                f32::from_le_bytes([
                    frame[channel_byte],
                    frame[channel_byte + 1],
                    frame[channel_byte + 2],
                    frame[channel_byte + 3],
                ])
            };
            if !sample.is_finite() {
                self.sink.faults.fetch_or(INVALID_SAMPLE, Ordering::Relaxed);
            }
            if self.sink.producer.push(sample).is_err() {
                self.sink.faults.fetch_or(QUEUE_OVERFLOW, Ordering::Relaxed);
            }
        }
    }
    fn fail(&self) {
        if self.active.load(Ordering::Relaxed) {
            self.sink.faults.fetch_or(STREAM_ERROR, Ordering::Relaxed);
        }
    }
}

pub(super) struct Stream {
    // Drop listeners before the objects whose callbacks they wrap.
    _listener: pw::stream::StreamListener<Process>,
    stream: pw::stream::StreamRc,
    _core_listener: pw::core::Listener,
    _core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    mainloop: pw::main_loop::MainLoopRc,
    faults: Arc<AtomicU32>,
    active: Arc<AtomicBool>,
}
impl Stream {
    pub(super) fn poll(&self) {
        self.mainloop.loop_().iterate(Duration::ZERO);
        if !matches!(self.stream.state(), pw::stream::StreamState::Streaming) {
            self.faults.fetch_or(STREAM_ERROR, Ordering::Relaxed);
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        let _ = self.stream.disconnect();
    }
}

pub(super) fn start(name: &str, channel: u16, rate: u32) -> Result<Capture> {
    let mut targets = targets()?.into_iter().filter(|t| t.device.name == name);
    let target = targets
        .next()
        .ok_or("PipeWire source disappeared; refresh inputs")?;
    if targets.next().is_some() {
        return Err("PipeWire node name is ambiguous".into());
    }
    let channels = target.device.configurations[0].channels;
    if channel == 0 || channel > channels {
        return Err("PipeWire input channel is unavailable".into());
    }

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let faults = Arc::new(AtomicU32::new(0));
    let valid = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let core_faults = faults.clone();
    let core_listener = core
        .add_listener_local()
        .error(move |_, _, _, _| {
            core_faults.fetch_or(STREAM_ERROR, Ordering::Relaxed);
        })
        .register();
    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "Omniphony Companion microphone",
        properties! {
            "media.type" => "Audio", "media.category" => "Capture", "media.role" => "Production",
            "node.name" => "omniphony-companion-capture",
            "target.object" => target.serial.as_str(),
            "stream.capture.sink" => if target.device.is_monitor { "true" } else { "false" },
            "stream.dont-remix" => "true", "node.dont-reconnect" => "true", "node.dont-fallback" => "true",
        },
    )?;
    let (producer, consumer) = RingBuffer::new(rate as usize * 4);
    let process = Process {
        sink: Sink {
            producer,
            faults: faults.clone(),
            channels: channels as usize,
            channel: channel as usize - 1,
            rate,
            previous: None,
        },
        valid: valid.clone(),
        active: active.clone(),
        previous: None,
    };
    let listener = stream
        .add_local_listener_with_user_data(process)
        .param_changed(move |_, data, id, pod| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut format = spa::param::audio::AudioInfoRaw::new();
            let correct = pod.is_some_and(|p| format.parse(p).is_ok())
                && format.channels() == u32::from(channels)
                && format.rate() == rate
                && format.format() == spa::param::audio::AudioFormat::F32LE;
            data.valid.store(correct, Ordering::Relaxed);
            if !correct {
                data.fail();
            }
        })
        .state_changed(|_, data, old, new| {
            if matches!(new, pw::stream::StreamState::Error(_))
                || (matches!(old, pw::stream::StreamState::Streaming)
                    && !matches!(new, pw::stream::StreamState::Streaming))
            {
                data.fail();
            }
        })
        .process(|stream, data| {
            if !data.valid.load(Ordering::Relaxed) {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.len() != 1 {
                data.fail();
                return;
            }
            let plane = &mut datas[0];
            let chunk = plane.chunk();
            if chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
                data.fail();
                return;
            }
            let (offset, size, stride) = (
                chunk.offset() as usize,
                chunk.size() as usize,
                chunk.stride(),
            );
            let empty = chunk.flags().bits() & spa::sys::SPA_CHUNK_FLAG_EMPTY as i32 != 0;
            match plane.data() {
                Some(bytes) => data.accept(bytes, offset, size, stride, empty),
                None => data.fail(),
            }
        })
        .register()?;
    let mut format = spa::param::audio::AudioInfoRaw::new();
    format.set_format(spa::param::audio::AudioFormat::F32LE);
    format.set_rate(rate);
    format.set_channels(u32::from(channels));
    if target
        .positions
        .iter()
        .all(|p| *p != spa::sys::SPA_AUDIO_CHANNEL_UNKNOWN)
    {
        let mut positions = [0; 64];
        positions[..target.positions.len()].copy_from_slice(&target.positions);
        format.set_position(positions);
    }
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: format.into(),
    };
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|e| format!("PipeWire format: {e:?}"))?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&bytes).ok_or("invalid PipeWire format")?;
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut [pod],
    )?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !valid.load(Ordering::Relaxed)
        || !matches!(stream.state(), pw::stream::StreamState::Streaming)
    {
        mainloop.loop_().iterate(Duration::from_millis(10));
        if faults.load(Ordering::Relaxed) != 0 {
            return Err("PipeWire refused or lost the selected source".into());
        }
        if Instant::now() >= deadline {
            return Err("PipeWire source did not start; check its profile and routing".into());
        }
    }
    let metadata = CaptureMetadata {
        host: "PipeWire".into(),
        device: name.into(),
        input_channel: channel,
        stream_channels: channels,
        sample_rate: rate,
        sample_format: "f32".into(),
        is_monitor: target.device.is_monitor,
        channel_name: target
            .device
            .channel_names
            .get(usize::from(channel - 1))
            .cloned(),
        continuity_check: "chunk flags and callback cadence; device timestamps unavailable".into(),
        frames: 0,
        fault_flags: 0,
        peak_dbfs: None,
        rms_dbfs: None,
        clipped_samples: 0,
    };
    let max_frames = rate as usize * MAX_SECONDS as usize;
    Ok(Capture {
        stream: CaptureStream::PipeWire(Box::new(Stream {
            _listener: listener,
            stream,
            _core_listener: core_listener,
            _core: core,
            _context: context,
            mainloop,
            faults: faults.clone(),
            active,
        })),
        consumer,
        faults,
        samples: Vec::with_capacity(max_frames),
        metadata,
        max_frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sources_and_sink_monitors_use_named_ordered_ports_and_stable_selectors() {
        let nodes = vec![
            Node {
                id: 10,
                serial: "100".into(),
                name: "test-input".into(),
                description: "Measurement interface".into(),
                monitor: false,
            },
            Node {
                id: 20,
                serial: "200".into(),
                name: "test-output".into(),
                description: "Test output".into(),
                monitor: true,
            },
        ];
        let ports = vec![
            Port {
                node: 10,
                index: 1,
                label: "capture_FR".into(),
                channel: "FR".into(),
            },
            Port {
                node: 20,
                index: 0,
                label: "monitor_MONO".into(),
                channel: "MONO".into(),
            },
            Port {
                node: 10,
                index: 0,
                label: "capture_FL".into(),
                channel: "FL".into(),
            },
        ];
        let targets = assemble(&nodes, &ports);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].device.name, "pipewire:source:test-input");
        assert_eq!(
            targets[0].device.channel_names,
            ["capture_FL", "capture_FR"]
        );
        assert_eq!(targets[0].device.configurations[0].channels, 2);
        assert_eq!(targets[1].device.label(), "Test output (sink monitor)");
        assert!(targets[1].device.is_monitor);
    }
    #[test]
    fn pcm_capture_selects_only_the_requested_channel_and_rejects_invalid_chunks() {
        let (producer, mut consumer) = RingBuffer::new(8);
        let faults = Arc::new(AtomicU32::new(0));
        let mut process = Process {
            sink: Sink {
                producer,
                faults: faults.clone(),
                channels: 2,
                channel: 1,
                rate: 48000,
                previous: None,
            },
            valid: Arc::new(AtomicBool::new(true)),
            active: Arc::new(AtomicBool::new(true)),
            previous: None,
        };
        let bytes: Vec<_> = [0.1_f32, 0.2, 0.3, 0.4]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        process.accept(&bytes, 0, bytes.len(), 8, false);
        assert_eq!(consumer.pop(), Ok(0.2));
        assert_eq!(consumer.pop(), Ok(0.4));
        assert_eq!(faults.load(Ordering::Relaxed), 0);
        process.accept(&bytes, 0, bytes.len(), 8, true);
        assert_eq!(consumer.pop(), Ok(0.0));
        assert_eq!(consumer.pop(), Ok(0.0));
        process.accept(&bytes, usize::MAX, 8, 8, false);
        assert_ne!(faults.load(Ordering::Relaxed) & STREAM_ERROR, 0);
    }
}
