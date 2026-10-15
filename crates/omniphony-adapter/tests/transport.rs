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
    let json = serde_json::to_string(&Response {
        version: VERSION,
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
