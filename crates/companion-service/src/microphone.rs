//! Explicit, finite microphone monitoring and a fresh check before each playback.
use crate::{
    AppResult,
    measurement::{MeasureProgress, check_cancelled},
};
use measurement_capture::{Capture, input::InputLevel};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const CHECK_SECONDS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequest {
    pub device: String,
    pub input_channel: u16,
    pub sample_rate: u32,
}

/// Does not contact the renderer or save audio. A worker owns the deadline even
/// if the frontend is minimized or stops polling. Samples remain memory-bounded.
pub fn monitor(
    args: InputRequest,
    cancel: &AtomicBool,
    update: &mut dyn FnMut(InputLevel),
) -> AppResult<()> {
    check_cancelled(cancel)?;
    if !(8000..=192000).contains(&args.sample_rate) || args.input_channel == 0 {
        return Err("Choose a valid microphone channel and sample rate.".into());
    }
    let mut capture = Capture::start_bounded(
        &args.device,
        args.input_channel,
        args.sample_rate,
        args.sample_rate as usize * (CHECK_SECONDS as usize + 2),
    )?;
    let start = Instant::now();
    let mut next = start;
    let mut last_frames = 0;
    let mut last_audio = start;
    while start.elapsed() < Duration::from_secs(CHECK_SECONDS.into()) {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        capture.poll();
        if capture.frames() > last_frames {
            last_frames = capture.frames();
            last_audio = Instant::now();
        }
        if Instant::now() >= next {
            update(capture.input_level(args.sample_rate as usize / 10));
            next = Instant::now() + Duration::from_millis(100);
        }
        if capture.faults() != 0 {
            return Err("Microphone capture fault. Check the interface connection.".into());
        }
        if last_audio.elapsed() > Duration::from_secs(1) {
            return Err(
                "No microphone frames received for one second. Check the input connection.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    struct Node(Child);
    impl Drop for Node {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "requires PipeWire and pw-cli; creates only a temporary silent virtual source"]
    fn live_silent_source_stops_before_playback_and_meter_can_be_cancelled() {
        let name = format!("companion-preflight-test-{}", std::process::id());
        let props = format!(
            "{{ factory.name = support.null-audio-sink node.name = {name} node.description = \"Companion silent test source\" media.class = Audio/Source/Virtual audio.position = [ MONO ] node.virtual = true }}"
        );
        let mut node = Node(
            Command::new("pw-cli")
                .args(["-m", "create-node", "adapter", &props])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let selector = format!("pipewire:source:{name}");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(
                node.0.try_wait().unwrap().is_none(),
                "virtual source exited"
            );
            if measurement_capture::devices()
                .unwrap()
                .iter()
                .any(|d| d.name == selector)
            {
                break;
            }
            assert!(Instant::now() < deadline, "virtual source not enumerated");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut capture = Capture::start(&selector, 1, 48000).unwrap();
        assert!(!capture.is_monitor());
        let mut updates = 0;
        let error = before_playback(&mut capture, 48000, &AtomicBool::new(false), &mut |_| {
            updates += 1
        })
        .unwrap_err();
        assert_eq!(error.to_string(), measurement_capture::input::NO_SIGNAL);
        assert!(updates >= 2);
        let recording = capture.finish();
        assert_eq!(recording.metadata.fault_flags, 0);
        assert!(recording.samples.len() >= 16000);
        assert!(recording.samples.iter().all(|s| *s == 0.0));
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        monitor(
            InputRequest {
                device: selector,
                input_channel: 1,
                sample_rate: 48000,
            },
            &cancel,
            &mut |level| {
                if level.frames > 0 {
                    assert_eq!(level.problem(), Some(measurement_capture::input::NO_SIGNAL));
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

/// Replace the existing quiet lead-in with a metered lead-in of the same length.
/// Only recent frames qualify: earlier activity cannot hide a silent/stalled input.
pub(crate) fn before_playback(
    capture: &mut Capture,
    rate: u32,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(MeasureProgress),
) -> AppResult<()> {
    let start = Instant::now();
    let initial_frames = capture.frames();
    let mut next = start;
    while start.elapsed() < crate::duration::LEAD_IN {
        check_cancelled(cancel)?;
        capture.poll();
        if Instant::now() >= next {
            progress(MeasureProgress::CheckingInput(
                capture.input_level(rate as usize / 5),
            ));
            next = Instant::now() + Duration::from_millis(100);
        }
        if capture.faults() != 0 {
            return Err("Microphone capture fault before playback; no sweep was started.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    check_cancelled(cancel)?;
    if capture.frames().saturating_sub(initial_frames) < rate as usize / 3 {
        return Err("Not enough fresh microphone frames received; no sweep was started. Check the input connection.".into());
    }
    let level = capture.input_level(rate as usize / 3);
    progress(MeasureProgress::CheckingInput(level));
    if let Some(reason) = level.problem() {
        // A digital sink monitor is normally silent until the renderer plays.
        // These diagnostics are explicitly not acoustic alignment evidence.
        if !(capture.is_monitor() && reason == measurement_capture::input::NO_SIGNAL) {
            return Err(reason.into());
        }
    }
    Ok(())
}
