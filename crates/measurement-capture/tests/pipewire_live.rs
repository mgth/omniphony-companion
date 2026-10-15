//! Explicit local bench. Creates a temporary virtual sink, never opens hardware.
#![cfg(target_os = "linux")]
use measurement_capture::{Capture, STREAM_ERROR};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
#[ignore = "requires a running PipeWire session plus pw-cli and pw-play; virtual audio only"]
fn named_monitor_keeps_channels_separate_and_rejects_disconnection() {
    let name = format!("companion-capture-test-{}", std::process::id());
    let props = format!(
        "{{ factory.name = support.null-audio-sink node.name = {name} node.description = \"Companion test sink\" media.class = Audio/Sink audio.position = [ FL FR ] node.virtual = true }}"
    );
    let mut node = Process(
        Command::new("pw-cli")
            .args(["-m", "create-node", "adapter", &props])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let selector = format!("pipewire:monitor:{name}");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(
            node.0.try_wait().unwrap().is_none(),
            "virtual sink process exited"
        );
        if measurement_capture::devices()
            .unwrap()
            .iter()
            .any(|d| d.name == selector)
        {
            break;
        }
        assert!(Instant::now() < deadline, "virtual sink was not enumerated");
        std::thread::sleep(Duration::from_millis(20));
    }
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("channels.raw");
    let mut bytes = Vec::new();
    for _ in 0..48000 * 3 {
        bytes.extend(0.75_f32.to_le_bytes());
        bytes.extend(0.125_f32.to_le_bytes());
    }
    std::fs::write(&raw, bytes).unwrap();
    let mut capture = Capture::start(&selector, 2, 48000).unwrap();
    let playback = Process(
        Command::new("pw-play")
            .args([
                "--target",
                &name,
                "--raw",
                "--rate",
                "48000",
                "--channels",
                "2",
                "--channel-map",
                "FL,FR",
                "--format",
                "f32",
            ])
            .arg(&raw)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        capture.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        capture.faults(),
        0,
        "stream failed before intentional shutdown"
    );
    let recording = capture.finish();
    drop(playback);
    let _ = std::fs::remove_file(&raw);
    assert!(recording.metadata.is_monitor);
    assert_eq!(
        recording.metadata.channel_name.as_deref(),
        Some("monitor_FR")
    );
    assert_eq!(recording.metadata.stream_channels, 2);
    assert_eq!(
        recording.metadata.fault_flags, 0,
        "{:#?}",
        recording.metadata
    );
    assert!(recording.samples.len() > 4800);
    assert!(
        recording.samples.iter().any(|s| (*s - 0.125).abs() < 0.001),
        "right channel was not captured"
    );
    assert!(
        recording.samples.iter().all(|s| s.abs() < 0.126),
        "channels were mixed or swapped"
    );
    let mut capture = Capture::start(&selector, 1, 48000).unwrap();
    drop(node);
    let deadline = Instant::now() + Duration::from_secs(2);
    while capture.faults() == 0 && Instant::now() < deadline {
        capture.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(
        capture.faults() & STREAM_ERROR,
        0,
        "source loss silently followed another node"
    );
    drop(capture);
    assert!(
        Capture::start(&selector, 1, 48000).is_err(),
        "disappeared target must be refused"
    );
}
