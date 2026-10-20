use omniphony_adapter::{Client, Error, protocol::*};
use omniphony_osc_contract::stream::{frame, read_frame};
use rosc::{OscMessage, OscPacket, OscType};
use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

fn server(
    work: impl FnOnce(TcpStream) + Send + 'static,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        work(stream);
    });
    (endpoint, handle)
}
fn receive(stream: &mut TcpStream) -> Request {
    let bytes = read_frame(stream, MAX_PACKET_BYTES).unwrap().unwrap();
    let (_, OscPacket::Message(msg)) = rosc::decoder::decode_udp(&bytes).unwrap() else {
        panic!("request")
    };
    assert_eq!(msg.addr, REQUEST_ADDRESS);
    let [OscType::String(json)] = msg.args.as_slice() else {
        panic!("JSON")
    };
    serde_json::from_str(json).unwrap()
}
fn send(stream: &mut TcpStream, id: u64, reply: Reply) {
    send_version(stream, VERSION, id, reply);
}
fn send_version(stream: &mut TcpStream, version: u32, id: u64, reply: Reply) {
    let json = serde_json::to_string(&Response {
        version,
        request_id: id,
        reply,
    })
    .unwrap();
    let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage {
        addr: REPLY_ADDRESS.into(),
        args: vec![OscType::String(json)],
    }))
    .unwrap();
    stream.write_all(&frame(&bytes).unwrap()).unwrap();
}
fn snapshot() -> Snapshot {
    Snapshot {
        instance_id: "test".into(),
        revision: "test:1".into(),
        sample_rate: 48000,
        master_gain_db: 0.0,
        max_delay_ms: MAX_DELAY_MS,
        unavailable_reason: None,
        outputs: vec![Output {
            id: "L".into(),
            name: "L".into(),
            index: 0,
            gain_db: -2.0,
            delay_ms: 0.123,
            muted: false,
            freq_low_hz: None,
            freq_high_hz: None,
            azimuth_deg: None,
            elevation_deg: None,
            distance_m: None,
        }],
    }
}
fn session(signal: Option<Progress>) -> Reply {
    Reply::Session {
        session_id: "session:1".into(),
        snapshot: snapshot(),
        lease_ms: LEASE_MS,
        signal,
    }
}
fn spec() -> Sweep {
    Sweep {
        duration_ms: 100,
        peak_dbfs: -30.0,
        start_hz: 200.0,
        end_hz: 10000.0,
    }
}
fn progress(state: SignalState, frames: u64) -> Progress {
    Progress {
        sequence_outputs: Vec::new(),
        reference_output_id: None,
        signal_id: 1,
        output_id: "L".into(),
        signal: spec(),
        state,
        rendered_frames: frames,
        total_frames: spec().frames(48000),
    }
}

#[test]
fn shared_reference_sequence_preserves_identity_and_rejects_later_plan_changes() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send(&mut stream, r.request_id, session(None));
        let r = receive(&mut stream);
        let Command::Start {
            sequence_outputs,
            reference_output_id,
            ..
        } = r.command
        else {
            panic!("start")
        };
        assert_eq!(sequence_outputs, ["L"]);
        assert_eq!(reference_output_id.as_deref(), Some("L"));
        let mut p = progress(SignalState::Queued, 0);
        p.reference_output_id = reference_output_id;
        p.sequence_outputs = sequence_outputs;
        p.total_frames *= 3;
        send(&mut stream, r.request_id, session(Some(p.clone())));
        let r = receive(&mut stream);
        p.sequence_outputs.clear();
        send(&mut stream, r.request_id, session(Some(p)));
    });
    let mut c = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    c.begin(&snapshot()).unwrap();
    let p = c
        .start_sequence(1, "L", Some("L"), &["L".into()], spec())
        .unwrap();
    assert_eq!(p.total_frames, spec().frames(48000) * 3);
    assert!(
        c.status()
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    handle.join().unwrap();
}

#[test]
fn discovery_begin_render_progress_and_end_round_trip() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        assert_eq!(r.command, Command::Discover {});
        send(
            &mut stream,
            r.request_id,
            Reply::Snapshot {
                snapshot: snapshot(),
            },
        );
        let r = receive(&mut stream);
        assert_eq!(
            r.command,
            Command::Begin {
                expected_revision: "test:1".into()
            }
        );
        send(&mut stream, r.request_id, session(None));
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::Start { signal_id: 1, .. }));
        send(
            &mut stream,
            r.request_id,
            session(Some(progress(SignalState::Queued, 0))),
        );
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::Status { .. }));
        send(
            &mut stream,
            r.request_id,
            session(Some(progress(SignalState::Completed, spec().frames(48000)))),
        );
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::End { .. }));
        send(&mut stream, r.request_id, Reply::Ended {});
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
    let snapshot = client.discover().unwrap();
    client.begin(&snapshot).unwrap();
    assert_eq!(
        client.start(1, "L", spec()).unwrap().state,
        SignalState::Queued
    );
    assert_eq!(
        client.status().unwrap().unwrap().state,
        SignalState::Completed
    );
    client.end().unwrap();
    handle.join().unwrap();
}

#[test]
fn explicit_stop_is_confirmed_and_dropping_the_owner_closes_tcp() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send(&mut stream, r.request_id, session(None));
        let r = receive(&mut stream);
        send(
            &mut stream,
            r.request_id,
            session(Some(progress(SignalState::Rendering, 100))),
        );
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::Stop { .. }));
        send(
            &mut stream,
            r.request_id,
            session(Some(progress(SignalState::Cancelled, 100))),
        );
        assert!(read_frame(&mut stream, MAX_PACKET_BYTES).unwrap().is_none());
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
    client.begin(&snapshot()).unwrap();
    client.start(1, "L", spec()).unwrap();
    assert_eq!(
        client.stop().unwrap().unwrap().state,
        SignalState::Cancelled
    );
    drop(client);
    handle.join().unwrap();
}

#[test]
fn mismatched_reply_ids_poison_the_connection() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send(
            &mut stream,
            r.request_id + 1,
            Reply::Snapshot {
                snapshot: snapshot(),
            },
        );
        assert!(read_frame(&mut stream, MAX_PACKET_BYTES).unwrap().is_none());
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
    assert!(matches!(client.discover(), Err(Error::Protocol(_))));
    assert!(matches!(client.discover(), Err(Error::Protocol(_))));
    handle.join().unwrap();
}

#[test]
fn changed_configuration_cannot_be_accepted_as_the_original_session() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        let mut reply = session(None);
        if let Reply::Session { snapshot, .. } = &mut reply {
            snapshot.outputs[0].delay_ms = 2.0;
        }
        send(&mut stream, r.request_id, reply);
        assert!(read_frame(&mut stream, MAX_PACKET_BYTES).unwrap().is_none());
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
    assert!(client.begin(&snapshot()).is_err());
    handle.join().unwrap();
}

#[test]
fn backwards_progress_is_rejected_and_disconnects_the_session() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send(&mut stream, r.request_id, session(None));
        let r = receive(&mut stream);
        send(
            &mut stream,
            r.request_id,
            session(Some(progress(SignalState::Rendering, 100))),
        );
        let r = receive(&mut stream);
        send(
            &mut stream,
            r.request_id,
            session(Some(progress(SignalState::Rendering, 50))),
        );
        assert!(read_frame(&mut stream, MAX_PACKET_BYTES).unwrap().is_none());
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
    client.begin(&snapshot()).unwrap();
    client.start(1, "L", spec()).unwrap();
    assert!(client.status().is_err());
    handle.join().unwrap();
}

#[test]
fn oversized_and_partial_frames_cannot_hang_or_resume_a_connection() {
    for oversized in [true, false] {
        let (endpoint, handle) = server(move |mut stream| {
            receive(&mut stream);
            let size = if oversized {
                MAX_PACKET_BYTES as u32 + 1
            } else {
                100
            };
            stream.write_all(&size.to_be_bytes()).unwrap();
            if !oversized {
                thread::sleep(Duration::from_millis(100));
            }
        });
        let mut client = Client::connect(endpoint, Duration::from_millis(30)).unwrap();
        assert!(client.discover().is_err());
        assert!(client.discover().is_err());
        handle.join().unwrap();
    }
}

#[test]
fn nonlocal_endpoints_and_invalid_timeouts_are_rejected_before_connecting() {
    assert!(Client::connect("192.0.2.1:9000".parse().unwrap(), Duration::from_secs(1)).is_err());
    assert!(Client::connect("127.0.0.1:9000".parse().unwrap(), Duration::ZERO).is_err());
}

#[test]
fn refusal_is_returned_without_replaying_the_signal() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send(&mut stream, r.request_id, session(None));
        let r = receive(&mut stream);
        send(
            &mut stream,
            r.request_id,
            Reply::Error {
                code: "invalid_signal".into(),
                message: "outside output band".into(),
            },
        );
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::End { .. }));
        send(&mut stream, r.request_id, Reply::Ended {});
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
    client.begin(&snapshot()).unwrap();
    assert!(matches!(
        client.start(1, "L", spec()),
        Err(Error::Server { .. })
    ));
    client.end().unwrap();
    handle.join().unwrap();
}

#[test]
fn referenced_sequence_requires_matching_identity_and_total_frames() {
    for incorrect in [false, true] {
        let (endpoint, handle) = server(move |mut stream| {
            let r = receive(&mut stream);
            send(&mut stream, r.request_id, session(None));
            let r = receive(&mut stream);
            assert!(
                matches!(r.command, Command::Start { reference_output_id: Some(ref id), .. } if id == "L")
            );
            let mut p = progress(SignalState::Queued, 0);
            p.reference_output_id = Some("L".into());
            if !incorrect {
                p.total_frames *= 3;
            }
            send(&mut stream, r.request_id, session(Some(p)));
        });
        let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
        client.begin(&snapshot()).unwrap();
        assert_eq!(
            client.start_referenced(1, "L", Some("L"), spec()).is_err(),
            incorrect
        );
        handle.join().unwrap();
    }
}

#[test]
fn application_requires_complete_readback_and_never_replays_an_uncertain_write() {
    for corrupt in [false, true] {
        let (endpoint, handle) = server(move |mut stream| {
            let request = receive(&mut stream);
            let Command::SetOutputs {
                expected_revision,
                settings,
            } = request.command
            else {
                panic!("wrong command")
            };
            assert_eq!(expected_revision, "test:1");
            assert_eq!(settings.len(), 1);
            let mut applied = snapshot();
            applied.revision = "test:2".into();
            applied.outputs[0].gain_db = settings[0].gain_db;
            applied.outputs[0].delay_ms = settings[0].delay_ms;
            if corrupt {
                applied.master_gain_db = 2.0;
            }
            send(
                &mut stream,
                request.request_id,
                Reply::Snapshot { snapshot: applied },
            );
            if corrupt {
                assert!(read_frame(&mut stream, MAX_PACKET_BYTES).unwrap().is_none());
            }
        });
        let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
        let result = client.set_outputs(
            &snapshot(),
            vec![OutputSetting {
                output_id: "L".into(),
                gain_db: -4.0,
                delay_ms: 2.5,
            }],
        );
        if corrupt {
            assert!(result.is_err());
            assert!(client.discover().is_err());
        } else {
            let s = result.unwrap();
            assert_eq!(s.outputs[0].delay_ms, 2.5);
            assert_eq!(s.revision, "test:2");
        }
        drop(client);
        handle.join().unwrap();
    }
}

#[test]
fn discovery_uses_advertised_delay_capacity_and_refuses_unsettled_capacities() {
    for capacity in [100.0, 250.0, -1.0, 251.0] {
        let (endpoint, join) = server(move |mut stream| {
            let request = receive(&mut stream);
            let mut snapshot = snapshot();
            snapshot.max_delay_ms = capacity;
            send(
                &mut stream,
                request.request_id,
                Reply::Snapshot { snapshot },
            );
        });
        let mut client = Client::connect(endpoint, Duration::from_secs(2)).unwrap();
        assert_eq!(client.discover().is_ok(), (0.0..=250.0).contains(&capacity));
        join.join().unwrap();
    }
}

fn noise_progress(state: SignalState, frames: u64) -> NoiseProgress {
    let signal = LevelNoise {
        duration_seconds: 60,
        peak_dbfs: -30.0,
        bypass_output_gain: false,
    };
    NoiseProgress {
        signal_id: 1,
        output_id: "L".into(),
        signal,
        state,
        rendered_frames: frames,
        total_frames: signal.frames(48000),
    }
}
fn noise_session(signal: NoiseProgress) -> Reply {
    Reply::NoiseSession {
        session_id: "session:1".into(),
        snapshot: snapshot(),
        lease_ms: LEASE_MS,
        signal,
    }
}
#[test]
fn finite_noise_validates_identity_progress_stop_and_has_no_legacy_fallback() {
    for invalid in 0..5 {
        let (endpoint, handle) = server(move |mut stream| {
            let r = receive(&mut stream);
            send(&mut stream, r.request_id, session(None));
            let r = receive(&mut stream);
            assert!(
                matches!(r.command, Command::StartNoise { signal_id: 1, ref output_id, .. } if output_id == "L")
            );
            send(
                &mut stream,
                r.request_id,
                noise_session(noise_progress(SignalState::Queued, 0)),
            );
            let r = receive(&mut stream);
            assert!(matches!(r.command, Command::Status { .. }));
            let mut p = noise_progress(SignalState::Rendering, 1000);
            match invalid {
                1 => p.signal.peak_dbfs = -29.0,
                2 => p.total_frames += 1,
                3 => p.output_id = "missing".into(),
                4 => p.state = SignalState::Completed,
                _ => {}
            }
            send(&mut stream, r.request_id, noise_session(p));
            if invalid == 0 {
                let r = receive(&mut stream);
                assert!(matches!(r.command, Command::Stop { .. }));
                send(
                    &mut stream,
                    r.request_id,
                    noise_session(noise_progress(SignalState::Cancelled, 1000)),
                );
                let r = receive(&mut stream);
                assert!(matches!(r.command, Command::End { .. }));
                send(&mut stream, r.request_id, Reply::Ended {});
            }
        });
        let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
        client.begin(&snapshot()).unwrap();
        client
            .start_noise(1, "L", noise_progress(SignalState::Queued, 0).signal)
            .unwrap();
        if invalid == 0 {
            assert_eq!(client.noise_status().unwrap().rendered_frames, 1000);
            assert_eq!(client.stop_noise().unwrap().state, SignalState::Cancelled);
            client.end().unwrap();
        } else {
            assert!(client.noise_status().is_err());
        }
        handle.join().unwrap();
    }
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send(&mut stream, r.request_id, session(None));
        let r = receive(&mut stream);
        send(
            &mut stream,
            r.request_id,
            Reply::Error {
                code: "invalid_request".into(),
                message: "unknown command".into(),
            },
        );
        // The only follow-up is the caller's explicit End; never legacy test playback.
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::End { .. }));
        send(&mut stream, r.request_id, Reply::Ended {});
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    client.begin(&snapshot()).unwrap();
    assert!(
        client
            .start_noise(1, "L", noise_progress(SignalState::Queued, 0).signal)
            .unwrap_err()
            .to_string()
            .contains("update Omniphony")
    );
    client.end().unwrap();
    handle.join().unwrap();
}

fn placed(azimuth: f32, elevation: f32, distance: f32) -> Snapshot {
    let mut s = snapshot();
    s.outputs[0].azimuth_deg = Some(azimuth);
    s.outputs[0].elevation_deg = Some(elevation);
    s.outputs[0].distance_m = Some(distance);
    s
}

#[test]
fn discovery_reads_output_positions_and_refuses_impossible_ones() {
    for (snapshot, valid) in [
        (placed(-30.0, 0.0, 2.5), true),
        (placed(181.0, 0.0, 2.5), false),
        (placed(0.0, -91.0, 2.5), false),
        (placed(0.0, 0.0, -0.1), false),
    ] {
        let reply = snapshot.clone();
        let (endpoint, handle) = server(move |mut stream| {
            let r = receive(&mut stream);
            assert_eq!(r.version, VERSION);
            send(
                &mut stream,
                r.request_id,
                Reply::Snapshot { snapshot: reply },
            );
        });
        let mut c = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
        match c.discover() {
            Ok(found) => {
                assert!(valid);
                assert_eq!(found, snapshot);
                assert_eq!(found.outputs[0].azimuth_deg, Some(-30.0));
            }
            Err(Error::Protocol(message)) => {
                assert!(!valid);
                assert!(message.contains("invalid output"), "{message}");
            }
            Err(e) => panic!("{e}"),
        }
        handle.join().unwrap();
    }
}

#[test]
fn an_older_renderer_is_discovered_again_in_its_version_without_positions() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        assert_eq!(r.version, VERSION);
        assert!(matches!(r.command, Command::Discover {}));
        send_version(
            &mut stream,
            4,
            r.request_id,
            Reply::Error {
                code: "unsupported_version".into(),
                message: "unsupported measurement protocol version".into(),
            },
        );
        let r = receive(&mut stream);
        assert_eq!(r.version, 4);
        assert!(matches!(r.command, Command::Discover {}));
        send_version(
            &mut stream,
            4,
            r.request_id,
            Reply::Snapshot {
                snapshot: snapshot(),
            },
        );
        // The whole connection then stays in version 4.
        let r = receive(&mut stream);
        assert_eq!(r.version, 4);
        assert!(matches!(r.command, Command::Begin { .. }));
        send_version(&mut stream, 4, r.request_id, session(None));
        let r = receive(&mut stream);
        send_version(&mut stream, 4, r.request_id, Reply::Ended {});
    });
    let mut c = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    assert_eq!(c.protocol_version(), VERSION);
    let found = c.discover().unwrap();
    assert_eq!(c.protocol_version(), 4);
    assert!(found.outputs.iter().all(|o| o.azimuth_deg.is_none()));
    c.begin(&found).unwrap();
    c.end().unwrap();
    handle.join().unwrap();
}

#[test]
fn a_version_5_renderer_keeps_positions_but_cannot_skip_the_output_gain() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        assert_eq!(r.version, VERSION);
        send_version(
            &mut stream,
            5,
            r.request_id,
            Reply::Error {
                code: "unsupported_version".into(),
                message: "unsupported measurement protocol version".into(),
            },
        );
        let r = receive(&mut stream);
        assert_eq!(r.version, 5);
        send_version(
            &mut stream,
            5,
            r.request_id,
            Reply::Snapshot {
                snapshot: snapshot(),
            },
        );
    });
    let mut c = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    c.discover().unwrap();
    assert_eq!(c.protocol_version(), 5);
    // The manual level worker then plays the noise through the output gain.
    assert!(c.protocol_version() < NOISE_GAINS_VERSION);
    handle.join().unwrap();
}

#[test]
fn applying_settings_to_an_older_renderer_retries_in_its_version() {
    // Apply opens a connection without discovering first: its first request
    // meets the older renderer's refusal and is sent again in that version.
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        assert_eq!(r.version, VERSION);
        assert!(matches!(r.command, Command::SetOutputs { .. }));
        send_version(
            &mut stream,
            5,
            r.request_id,
            Reply::Error {
                code: "unsupported_version".into(),
                message: "unsupported measurement protocol version; this renderer answers versions 4 to 5".into(),
            },
        );
        let r = receive(&mut stream);
        assert_eq!(r.version, 5);
        let Command::SetOutputs { settings, .. } = r.command else {
            panic!("expected the same settings again")
        };
        let mut applied = snapshot();
        applied.revision = "test:2".into();
        applied.outputs[0].gain_db = settings[0].gain_db;
        applied.outputs[0].delay_ms = settings[0].delay_ms;
        send_version(
            &mut stream,
            5,
            r.request_id,
            Reply::Snapshot { snapshot: applied },
        );
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    let applied = client
        .set_outputs(
            &snapshot(),
            vec![OutputSetting {
                output_id: "L".into(),
                gain_db: -4.0,
                delay_ms: 2.5,
            }],
        )
        .unwrap();
    assert_eq!(applied.revision, "test:2");
    assert_eq!(client.protocol_version(), 5);
    handle.join().unwrap();
}

#[test]
fn a_version_refusal_outside_the_supported_range_poisons_the_connection() {
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send_version(
            &mut stream,
            3,
            r.request_id,
            Reply::Error {
                code: "unsupported_version".into(),
                message: "unsupported measurement protocol version".into(),
            },
        );
    });
    let mut c = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    assert!(matches!(c.discover(), Err(Error::Protocol(_))));
    assert_eq!(c.protocol_version(), VERSION);
    assert!(c.discover().unwrap_err().to_string().contains("closed"));
    handle.join().unwrap();
}

#[test]
fn speaker_output_is_requested_only_from_a_renderer_that_knows_the_command() {
    // Version 7: discover, then the switch, whose snapshot comes back.
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::Discover {}));
        let mut headphones = snapshot();
        headphones.unavailable_reason = Some(SPEAKER_OUTPUT_REQUIRED.into());
        send(
            &mut stream,
            r.request_id,
            Reply::Snapshot {
                snapshot: headphones,
            },
        );
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::UseSpeakerOutput {}));
        send(
            &mut stream,
            r.request_id,
            Reply::Snapshot {
                snapshot: snapshot(),
            },
        );
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    let switched = client.use_speaker_output().unwrap();
    assert_eq!(switched.unavailable_reason, None);
    handle.join().unwrap();

    // Version 6: named as too old; the unknown command is never sent.
    let (endpoint, handle) = server(|mut stream| {
        let r = receive(&mut stream);
        send_version(
            &mut stream,
            6,
            r.request_id,
            Reply::Error {
                code: "unsupported_version".into(),
                message: "this renderer answers versions 4 to 6".into(),
            },
        );
        let r = receive(&mut stream);
        assert!(matches!(r.command, Command::Discover {}));
        send_version(
            &mut stream,
            6,
            r.request_id,
            Reply::Snapshot {
                snapshot: snapshot(),
            },
        );
        assert!(read_frame(&mut stream, MAX_PACKET_BYTES).unwrap().is_none());
    });
    let mut client = Client::connect(endpoint, Duration::from_secs(1)).unwrap();
    let error = client.use_speaker_output().unwrap_err().to_string();
    assert!(error.contains("choose Speakers in Studio"), "{error}");
    drop(client);
    handle.join().unwrap();
}
