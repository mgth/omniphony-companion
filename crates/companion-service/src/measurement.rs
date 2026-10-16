use crate::{
    AppResult,
    files::{parent, write_json},
};
use measurement_capture::{Capture, MAX_SECONDS, calibration, diagnostics, impulse, timing};
use omniphony_adapter::{Client, Progress, SignalState, Snapshot, Sweep};
use std::{
    fs,
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct MeasureRequest {
    pub endpoint: SocketAddr,
    pub snapshot: Snapshot,
    pub device: String,
    pub input_channel: u16,
    pub sample_rate: u32,
    pub microphone_id: String,
    pub position_id: String,
    pub output: String,
    pub reference_output: String,
    pub calibration: Option<PathBuf>,
    pub sweep: Sweep,
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum MeasureProgress {
    Preparing,
    OpeningInput,
    LeadIn,
    Playing(Progress),
    Tail,
    Saving,
    Analyzing,
}

#[derive(Debug, Clone)]
pub struct TakeOutcome {
    pub directory: PathBuf,
    /// Passed acquisition checks; arrival timing may still need review.
    pub accepted: bool,
    pub cancelled: bool,
    pub rejection_reason: Option<String>,
    pub diagnostics: Option<diagnostics::AcquisitionDiagnostics>,
    pub timing: Option<timing::TimingReport>,
    pub impulse: Option<Arc<impulse::ImpulseReport>>,
    pub impulse_error: Option<String>,
}

pub(crate) fn save_wav(path: &Path, samples: &[f32], rate: u32) -> AppResult<()> {
    let mut pending = tempfile::NamedTempFile::new_in(parent(path))?;
    {
        let mut writer = hound::WavWriter::new(
            std::io::BufWriter::new(pending.as_file_mut()),
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        for &sample in samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
    }
    pending.as_file().sync_all()?;
    pending.persist_noclobber(path)?;
    Ok(())
}
pub(crate) fn read_calibration(path: &Path) -> AppResult<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    calibration::parse(&text)?;
    Ok(text)
}
pub fn measure(
    args: MeasureRequest,
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(MeasureProgress),
) -> AppResult<TakeOutcome> {
    check_cancelled(cancelled)?;
    on_progress(MeasureProgress::Preparing);
    for id in [&args.microphone_id, &args.position_id] {
        if id.trim().is_empty() || id.len() > 128 {
            return Err("microphone/position IDs need 1..128 bytes".into());
        }
    }
    let snapshot = &args.snapshot;
    let origin = if args.device.starts_with("pipewire:monitor:") {
        "sink_monitor_capture"
    } else {
        "microphone_capture"
    };
    let sweep = args.sweep;
    sweep.validate(snapshot.sample_rate)?;
    if !(8_000..=192_000).contains(&args.sample_rate)
        || sweep.end_hz > args.sample_rate as f32 * 0.45
    {
        return Err("the sweep must also fit the input sample rate".into());
    }
    let calibration = args
        .calibration
        .as_deref()
        .map(read_calibration)
        .transpose()?;
    let mut client = Client::connect(args.endpoint, Duration::from_secs(2))?;
    // Verify freshness before opening the input. Begin checks it again.
    if client.discover()? != *snapshot {
        return Err("renderer snapshot changed; discover again".into());
    }
    for id in [&args.output, &args.reference_output] {
        let output = snapshot
            .outputs
            .iter()
            .find(|o| &o.id == id)
            .ok_or("output is absent from snapshot")?;
        if output.muted {
            return Err("measurement output is muted".into());
        }
        if sweep.start_hz < output.freq_low_hz.unwrap_or(20.0)
            || sweep.end_hz > output.freq_high_hz.unwrap_or(f32::MAX)
            || sweep.peak_dbfs + snapshot.master_gain_db + output.gain_db > -6.0
        {
            return Err("sweep exceeds an output's frequency or level limits".into());
        }
    }
    fs::create_dir_all(parent(&args.out_dir))?;
    fs::create_dir(&args.out_dir)?;
    write_json(&args.out_dir.join("snapshot.json"), &snapshot)?;
    if let Some(text) = &calibration {
        let mut pending = tempfile::NamedTempFile::new_in(&args.out_dir)?;
        pending.write_all(text.as_bytes())?;
        pending.as_file().sync_all()?;
        pending.persist_noclobber(args.out_dir.join("microphone-calibration.txt"))?;
    }
    write_json(
        &args.out_dir.join("stimulus.json"),
        &serde_json::json!({
            "schema_version": 1, "protocol_version": omniphony_adapter::protocol::VERSION,
            "origin": origin, "microphone_id": args.microphone_id, "position_id": args.position_id,
            "output_id": args.output, "reference_output_id": args.reference_output, "signal": sweep,
            "segment_start_render_frames": [0, sweep.frames(snapshot.sample_rate), 2*sweep.frames(snapshot.sample_rate)],
            "calibration_file": calibration.as_ref().map(|_| "microphone-calibration.txt"),
            "calibration_applied": false,
        }),
    )?;
    check_cancelled(cancelled)?;
    on_progress(MeasureProgress::OpeningInput);
    let mut capture = match Capture::start(&args.device, args.input_channel, args.sample_rate) {
        Ok(capture) => capture,
        Err(error) => {
            write_json(
                &args.out_dir.join("capture-report.json"),
                &serde_json::json!({
                    "schema_version": 3, "origin": origin, "accepted_for_analysis": false,
                    "valid_for_correction": false, "recording_file": null, "rejection_reason": error.to_string(),
                }),
            )?;
            return Ok(TakeOutcome {
                directory: args.out_dir,
                accepted: false,
                cancelled: false,
                rejection_reason: Some(error.to_string()),
                diagnostics: None,
                timing: None,
                impulse: None,
                impulse_error: None,
            });
        }
    };
    let started = Instant::now();
    let mut began = false;
    let mut analysis_start_frame = 0;
    let acquisition: AppResult<_> = (|| {
        while capture.frames() < args.sample_rate as usize / 10 {
            check_cancelled(cancelled)?;
            capture.poll();
            if started.elapsed() > Duration::from_secs(2) || capture.faults() != 0 {
                return Err("input stream did not start cleanly".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        check_cancelled(cancelled)?;
        client.begin(snapshot)?;
        began = true;
        on_progress(MeasureProgress::LeadIn);
        capture.poll();
        // Exclude pre-session audio and allow existing output queues to drain.
        analysis_start_frame = capture.frames() + args.sample_rate as usize / 5;
        drain_for(&mut capture, Duration::from_millis(600), cancelled)?;
        let mut progress =
            client.start_referenced(1, &args.output, Some(&args.reference_output), sweep)?;
        on_progress(MeasureProgress::Playing(progress.clone()));
        let mut next_status = Instant::now();
        loop {
            check_cancelled(cancelled)?;
            capture.poll();
            if capture.faults() != 0 {
                return Err(format!("input capture fault flags: {}", capture.faults()).into());
            }
            if started.elapsed() >= Duration::from_secs(u64::from(MAX_SECONDS) - 1) {
                return Err("capture wall-clock limit reached".into());
            }
            match progress.state {
                SignalState::Completed => break,
                SignalState::Cancelled | SignalState::Expired => {
                    return Err(format!("renderer signal ended: {:?}", progress.state).into());
                }
                _ => {}
            }
            if Instant::now() >= next_status {
                progress = client
                    .status()?
                    .ok_or("renderer lost measurement progress")?;
                on_progress(MeasureProgress::Playing(progress.clone()));
                next_status = Instant::now() + Duration::from_millis(200);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        on_progress(MeasureProgress::Tail);
        // Keep the lease quiet while the acoustic tail reaches the input.
        drain_for(&mut capture, Duration::from_millis(600), cancelled)?;
        client.status()?;
        Ok(progress)
    })();
    let ended = if began {
        client.end().map_err(|e| e.to_string())
    } else {
        Ok(())
    };
    drop(client);
    let recording = capture.finish();
    let acquisition = acquisition
        .map_err(|e| e.to_string())
        .and_then(|p| ended.map(|()| p));
    finish_take(
        &args,
        recording,
        analysis_start_frame,
        acquisition,
        cancelled.load(Ordering::Relaxed),
        &mut on_progress,
    )
}

pub(crate) fn finish_take(
    args: &MeasureRequest,
    recording: measurement_capture::Recording,
    analysis_start_frame: usize,
    acquisition: Result<Progress, String>,
    was_cancelled: bool,
    on_progress: &mut dyn FnMut(MeasureProgress),
) -> AppResult<TakeOutcome> {
    let snapshot = &args.snapshot;
    let sweep = args.sweep;
    let origin = if recording.metadata.is_monitor {
        "sink_monitor_capture"
    } else {
        "microphone_capture"
    };
    on_progress(MeasureProgress::Saving);
    save_wav(
        &args.out_dir.join("recording.wav"),
        &recording.samples,
        args.sample_rate,
    )?;
    on_progress(MeasureProgress::Analyzing);
    let mut result: Result<_, String> = match &acquisition {
        Err(error) => Err(error.to_string()),
        Ok(_) if recording.metadata.fault_flags != 0 => Err(format!(
            "capture fault flags: {}",
            recording.metadata.fault_flags
        )),
        Ok(_) if recording.metadata.clipped_samples != 0 => Err("clipped recording".into()),
        Ok(_) => recording
            .samples
            .get(analysis_start_frame..)
            .ok_or_else(|| "missing quiet lead-in".to_owned())
            .and_then(|samples| {
                timing::analyze(samples, args.sample_rate, snapshot.sample_rate, sweep)
            }),
    };
    if was_cancelled {
        result = Err("capture cancelled by user".into());
    }
    let diagnostics =
        if acquisition.is_ok() && !was_cancelled && recording.metadata.fault_flags == 0 {
            recording
                .samples
                .get(analysis_start_frame..)
                .and_then(|samples| {
                    diagnostics::analyze(samples, args.sample_rate, snapshot.sample_rate, sweep)
                        .ok()
                })
        } else {
            None
        };
    let response = result.as_ref().ok().map(|timing| {
        impulse::analyze(
            &recording.samples[analysis_start_frame..],
            args.sample_rate,
            snapshot.sample_rate,
            sweep,
            timing,
        )
    });
    let impulse = response
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .cloned()
        .map(Arc::new);
    let impulse_error = response.and_then(Result::err);
    write_json(
        &args.out_dir.join("capture-report.json"),
        &serde_json::json!({
            "schema_version": 3, "origin": origin, "capture": recording.metadata,
            "renderer_progress": acquisition.as_ref().ok(),
            "analysis_start_frame": analysis_start_frame,
            "timing_frame_origin": "analysis_start_frame_in_recording_wav",
            "timing": result.as_ref().ok(), "rejection_reason": result.as_ref().err(),
            "impulse": impulse, "impulse_error": impulse_error, "diagnostics": diagnostics,
            "timing_warning": result.as_ref().ok().and_then(|report| report.timing_warning()),
            "accepted_for_analysis": result.is_ok(), "valid_for_correction": false,
            "calibration_applied": false,
        }),
    )?;
    Ok(TakeOutcome {
        directory: args.out_dir.clone(),
        accepted: result.is_ok(),
        cancelled: was_cancelled,
        rejection_reason: result.as_ref().err().cloned(),
        diagnostics,
        timing: result.ok(),
        impulse,
        impulse_error,
    })
}

pub(crate) fn check_cancelled(cancelled: &AtomicBool) -> AppResult<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err("capture cancelled by user".into())
    } else {
        Ok(())
    }
}
pub(crate) fn drain_for(
    capture: &mut Capture,
    duration: Duration,
    cancelled: &AtomicBool,
) -> AppResult<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        check_cancelled(cancelled)?;
        capture.poll();
        if capture.faults() != 0 {
            return Err(format!("capture fault flags: {}", capture.faults()).into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wav_roundtrip_retains_samples_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.wav");
        let samples = [0.0, 0.25, -0.5];
        save_wav(&path, &samples, 44100).unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 44100);
        assert_eq!(
            reader
                .samples::<f32>()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            samples
        );
        assert!(save_wav(&path, &[1.0], 48000).is_err());
    }
}
