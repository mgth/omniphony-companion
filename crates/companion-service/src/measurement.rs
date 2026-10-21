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
    CheckingInput(measurement_capture::input::InputLevel),
    Playing(Progress),
    Tail,
    Saving,
    Analyzing,
}

#[derive(Debug, Clone)]
pub struct TakeOutcome {
    pub directory: PathBuf,
    /// Complete valid acquisition with located sweeps; confidence is reported separately.
    pub accepted: bool,
    pub cancelled: bool,
    pub rejection_reason: Option<String>,
    pub diagnostics: Option<diagnostics::AcquisitionDiagnostics>,
    pub timing: Option<timing::TimingReport>,
    pub impulse: Option<Arc<impulse::ImpulseReport>>,
    pub impulse_error: Option<String>,
    /// Restored without reading its saved report, which loads when the take is
    /// opened ([`crate::controller::Controller::load_saved_take`]). Until then
    /// only the folder is known: no verdict, diagnostics or plots.
    pub deferred: bool,
}

impl TakeOutcome {
    pub fn quality(&self) -> crate::quality::Report {
        crate::quality::assess(
            self.timing.as_ref(),
            self.impulse.as_deref(),
            self.diagnostics.as_ref(),
            self.rejection_reason.as_deref(),
        )
    }
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
                deferred: false,
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
        crate::microphone::before_playback(
            &mut capture,
            args.sample_rate,
            cancelled,
            &mut on_progress,
        )?;
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
                next_status = Instant::now() + crate::duration::STATUS_POLL;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        on_progress(MeasureProgress::Tail);
        // Keep the lease quiet while the acoustic tail reaches the input.
        drain_for(&mut capture, crate::duration::TAIL, cancelled)?;
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
    // Analysis is inspectable even when clipping prevents qualification. Hardware
    // faults and incomplete/cancelled acquisition cannot establish timing anchors.
    let inspected = if acquisition.is_ok() && !was_cancelled && recording.metadata.fault_flags == 0
    {
        recording
            .samples
            .get(analysis_start_frame..)
            .ok_or_else(|| "missing quiet lead-in".to_owned())
            .and_then(|samples| {
                timing::inspect(samples, args.sample_rate, snapshot.sample_rate, sweep)
            })
    } else {
        Err("Incomplete or faulty acquisition; synchronized analysis unavailable.".into())
    };
    let result = match &acquisition {
        Err(error) => Err(error.to_string()),
        Ok(_) if was_cancelled => Err("capture cancelled by user".into()),
        Ok(_) if recording.metadata.fault_flags != 0 => Err(format!(
            "capture fault flags: {}",
            recording.metadata.fault_flags
        )),
        Ok(_) if recording.metadata.clipped_samples != 0 => Err("clipped recording".into()),
        Ok(_) => inspected.clone(),
    };
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
    let response = inspected.as_ref().ok().map(|timing| {
        impulse::inspect(
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
    // The unweighted response, for room correction. It never changes the verdict.
    let stored = inspected.as_ref().ok().map(|timing| {
        impulse::full_response(
            &recording.samples[analysis_start_frame..],
            args.sample_rate,
            snapshot.sample_rate,
            sweep,
            timing,
        )
        .and_then(|full| {
            crate::room_response::store(
                args,
                &full,
                args.sample_rate,
                result.is_ok(),
                recording.metadata.clipped_samples,
            )
            .map_err(|e| e.to_string())
        })
    });
    crate::files::write_json_compact(
        &args.out_dir.join("capture-report.json"),
        &serde_json::json!({
            "schema_version": 3, "origin": origin, "capture": recording.metadata,
            "renderer_progress": acquisition.as_ref().ok(),
            "analysis_start_frame": analysis_start_frame,
            "timing_frame_origin": "analysis_start_frame_in_recording_wav",
            "timing": inspected.as_ref().ok(), "rejection_reason": result.as_ref().err(),
            "quality": crate::quality::assess(inspected.as_ref().ok(), impulse.as_deref(), diagnostics.as_ref(), result.as_ref().err().map(String::as_str)),
            "impulse": impulse, "impulse_error": impulse_error, "diagnostics": diagnostics,
            "timing_warning": inspected.as_ref().ok().and_then(|report| report.timing_warning()),
            "accepted_for_analysis": result.is_ok(), "valid_for_correction": false,
            "calibration_applied": false,
            "response_file": stored.as_ref().and_then(|r| r.as_ref().ok()).map(|_| crate::room_response::FILE),
            "response_error": stored.and_then(Result::err),
        }),
    )?;
    Ok(TakeOutcome {
        directory: args.out_dir.clone(),
        accepted: result.is_ok(),
        cancelled: was_cancelled,
        rejection_reason: result.as_ref().err().cloned(),
        diagnostics,
        timing: inspected.ok(),
        impulse,
        impulse_error,
        deferred: false,
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
    use omniphony_adapter::protocol::Output;

    /// Reference, target, reference through known room responses, after a quiet lead-in.
    fn take(sweep: Sweep, paths: [&[(f64, f64)]; 3]) -> Vec<f32> {
        let source = sweep.samples(48000).unwrap();
        let mut out = vec![0.0_f32; 24000 + 3 * source.len() + 24000];
        for (segment, paths) in paths.iter().enumerate() {
            for &(ms, gain) in *paths {
                let start = 24000 + segment * source.len() + (ms * 48.0).round() as usize;
                for (dst, &src) in out[start..].iter_mut().zip(&source) {
                    *dst += src * gain as f32;
                }
            }
        }
        let mut state = 7_u32;
        for value in &mut out {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            *value += ((state as f64 / u32::MAX as f64 - 0.5) * 2e-6) as f32;
        }
        out
    }
    fn request(directory: &Path, sweep: Sweep) -> MeasureRequest {
        let output = |id: &str, index| Output {
            id: id.into(),
            index,
            name: id.into(),
            gain_db: 0.0,
            delay_ms: 0.0,
            muted: false,
            freq_low_hz: None,
            freq_high_hz: None,
            azimuth_deg: None,
            elevation_deg: None,
            distance_m: None,
        };
        MeasureRequest {
            endpoint: "127.0.0.1:1".parse().unwrap(),
            snapshot: Snapshot {
                instance_id: "test".into(),
                revision: "test:1".into(),
                sample_rate: 48000,
                master_gain_db: 0.0,
                max_delay_ms: 100.0,
                outputs: vec![output("C", 0), output("FL", 1)],
                unavailable_reason: None,
            },
            device: "test".into(),
            input_channel: 1,
            sample_rate: 48000,
            microphone_id: "mic-1".into(),
            position_id: "main-seat".into(),
            output: "FL".into(),
            reference_output: "C".into(),
            calibration: Some(PathBuf::from("/somewhere/umik-1 7101234.txt")),
            sweep,
            out_dir: directory.to_owned(),
        }
    }
    fn recording(samples: Vec<f32>) -> measurement_capture::Recording {
        let (peak_dbfs, rms_dbfs, clipped_samples) = measurement_capture::levels(&samples);
        measurement_capture::Recording {
            metadata: measurement_capture::CaptureMetadata {
                host: "test".into(),
                device: "test".into(),
                input_channel: 1,
                stream_channels: 1,
                sample_rate: 48000,
                sample_format: "f32".into(),
                is_monitor: false,
                channel_name: None,
                continuity_check: "test".into(),
                frames: samples.len(),
                fault_flags: 0,
                peak_dbfs,
                rms_dbfs,
                clipped_samples,
            },
            samples,
        }
    }
    fn progress(args: &MeasureRequest) -> Progress {
        let frames = args.sweep.frames(48000) * 3;
        Progress {
            signal_id: 1,
            output_id: args.output.clone(),
            reference_output_id: Some(args.reference_output.clone()),
            sequence_outputs: vec![],
            signal: args.sweep,
            state: SignalState::Completed,
            rendered_frames: frames,
            total_frames: frames,
        }
    }

    #[test]
    fn a_take_keeps_its_unweighted_response_and_a_calibrated_magnitude() {
        let dir = tempfile::tempdir().unwrap();
        let sweep = Sweep {
            duration_ms: 1500,
            peak_dbfs: -20.0,
            start_hz: 20.0,
            end_hz: 20000.0,
        };
        let args = request(&dir.path().join("take"), sweep);
        fs::create_dir(&args.out_dir).unwrap();
        let calibration = "\"Sens Factor =-.7742dB, SERNO: 7101234\"\n20 -4\n1000 0\n20000 3\n";
        fs::write(args.out_dir.join("microphone-calibration.txt"), calibration).unwrap();
        // The speaker 1.5 ms late, with an echo 3 ms after it at half its level.
        let target = &[(1.5, 0.5), (4.5, 0.25)][..];
        let samples = take(sweep, [&[(0.0, 0.8)], target, &[(0.0, 0.8)]]);
        let outcome = finish_take(
            &args,
            recording(samples),
            4800,
            Ok(progress(&args)),
            false,
            &mut |_| {},
        )
        .unwrap();
        assert!(outcome.accepted, "{:?}", outcome.rejection_reason);
        let report: serde_json::Value =
            crate::files::read_json(&args.out_dir.join("capture-report.json")).unwrap();
        assert_eq!(report["response_file"], "response.json");
        assert_eq!(report["calibration_applied"], false);
        let (stored, full) = crate::room_response::load(&args.out_dir).unwrap();
        assert_eq!(stored.position_id, "main-seat");
        assert_eq!((stored.output_id.as_str(), stored.signal), ("FL", sweep));
        let used = stored.calibration.as_ref().unwrap();
        assert_eq!(used.source_name.as_deref(), Some("umik-1 7101234.txt"));
        assert_eq!(
            used.sha256,
            crate::room_response::sha256(calibration.as_bytes())
        );
        assert_eq!((used.points, used.coverage_hz), (3, [20.0, 20000.0]));
        // The WAV is raw; the stored magnitude matches a fresh one from it.
        let again = measurement_capture::response::magnitude(&full, sweep, None).unwrap();
        assert_eq!(again.magnitude_db, stored.magnitude.magnitude_db);
        let curve = measurement_capture::calibration::Calibration::parse(calibration).unwrap();
        let magnitude = &stored.magnitude;
        let calibrated = stored.corrected_magnitude_db().unwrap();
        for (i, &hz) in magnitude.frequency_hz.iter().enumerate() {
            let raw = f64::from(magnitude.magnitude_db[1][i]);
            // The stored grid averages power over 1/48 octave around each point.
            let half = 2_f64.powf(0.5 / measurement_capture::response::POINTS_PER_OCTAVE);
            let power = (0..=64)
                .map(|k| {
                    let f = hz / half * half.powf(k as f64 / 32.0);
                    0.25 + 0.0625 + 0.25 * (std::f64::consts::TAU * f * 0.003).cos()
                })
                .sum::<f64>()
                / 65.0;
            let expected = 10.0 * power.log10();
            if (30.0..=15000.0).contains(&hz) && expected > -3.0 {
                assert!(
                    (raw - expected).abs() < 0.15,
                    "{hz} Hz: {raw} vs {expected}"
                );
            }
            let reference = f64::from(magnitude.magnitude_db[0][i]);
            if (30.0..=15000.0).contains(&hz) {
                assert!(
                    (reference - 20.0 * 0.8_f64.log10()).abs() < 0.1,
                    "{hz}: {reference}"
                );
            }
            let correction = f64::from(calibrated[1][i]) - raw;
            assert!((correction + curve.deviation_db(hz)).abs() < 0.002);
        }
    }

    #[test]
    fn without_a_calibration_the_response_says_so_and_a_clipped_take_keeps_its_fault() {
        let dir = tempfile::tempdir().unwrap();
        let sweep = Sweep {
            duration_ms: 1000,
            peak_dbfs: -12.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        };
        let mut args = request(&dir.path().join("take"), sweep);
        args.calibration = None;
        fs::create_dir(&args.out_dir).unwrap();
        let mut samples = take(sweep, [&[(0.0, 0.8)], &[(0.0, 4.5)], &[(0.0, 0.8)]]);
        for x in &mut samples {
            *x = x.clamp(-1.0, 1.0);
        }
        let outcome = finish_take(
            &args,
            recording(samples),
            4800,
            Ok(progress(&args)),
            false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            outcome.rejection_reason.as_deref(),
            Some("clipped recording")
        );
        let (stored, _) = crate::room_response::load(&args.out_dir).unwrap();
        assert!(!stored.accepted_for_analysis && stored.clipped_samples > 0);
        assert!(stored.calibration.is_none() && stored.corrected_magnitude_db().is_none());
    }
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
