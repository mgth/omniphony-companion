//! Continuous shared-reference acquisition, owned by a service worker.
use crate::{
    AppResult,
    files::write_json,
    measurement::{
        MeasureProgress, MeasureRequest, TakeOutcome, check_cancelled, drain_for, finish_take,
        read_calibration, save_wav,
    },
};
use measurement_capture::{Capture, Recording, levels, sequence};
use omniphony_adapter::{Client, Progress, SignalState};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceWindow {
    pub target_index: usize,
    pub source_start_frame: usize,
    pub source_end_frame: usize,
}
#[derive(Debug, Clone)]
pub struct BatchOutcome {
    pub directory: PathBuf,
    pub takes: Vec<(String, TakeOutcome)>,
    pub cancelled: bool,
    pub error: Option<String>,
}

pub fn active_target(progress: &Progress) -> usize {
    let segments = progress.sequence_outputs.len() * 2 + 1;
    let segment_frames = progress.total_frames / segments.max(1) as u64;
    ((progress.rendered_frames / segment_frames.max(1)).saturating_sub(1) / 2) as usize
}

pub fn measure(
    args: MeasureRequest,
    outputs: Vec<String>,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(usize, MeasureProgress),
) -> AppResult<BatchOutcome> {
    check_cancelled(cancelled)?;
    let budget = sequence::capture_budget(
        args.sweep,
        args.snapshot.sample_rate,
        args.sample_rate,
        outputs.len(),
    )?;
    if outputs.first() != Some(&args.output)
        || args.microphone_id.trim().is_empty()
        || args.microphone_id.len() > 128
        || args.position_id.trim().is_empty()
        || args.position_id.len() > 128
    {
        return Err("invalid batch identity".into());
    }
    args.sweep.validate(args.sample_rate)?;
    let mut seen = std::collections::HashSet::new();
    for id in outputs
        .iter()
        .chain(std::iter::once(&args.reference_output))
    {
        let output = args
            .snapshot
            .outputs
            .iter()
            .find(|o| &o.id == id)
            .ok_or("batch output absent from snapshot")?;
        if output.muted
            || args.sweep.start_hz < output.freq_low_hz.unwrap_or(20.0)
            || args.sweep.end_hz > output.freq_high_hz.unwrap_or(f32::MAX)
            || args.sweep.peak_dbfs + args.snapshot.master_gain_db + output.gain_db > -6.0
        {
            return Err("batch output is muted or exceeds frequency/level limits".into());
        }
    }
    if outputs.iter().any(|id| !seen.insert(id)) {
        return Err("duplicate batch target".into());
    }
    let calibration = args
        .calibration
        .as_deref()
        .map(read_calibration)
        .transpose()?;
    progress(0, MeasureProgress::Preparing);
    let mut client = Client::connect(args.endpoint, Duration::from_secs(2))?;
    if client.discover()? != args.snapshot {
        return Err("renderer snapshot changed; refresh before measuring".into());
    }
    fs::create_dir_all(crate::files::parent(&args.out_dir))?;
    fs::create_dir(&args.out_dir)?;
    write_json(&args.out_dir.join("snapshot.json"), &args.snapshot)?;
    write_json(
        &args.out_dir.join("batch-stimulus.json"),
        &serde_json::json!({"schema_version":1,"protocol_version":omniphony_adapter::protocol::VERSION,"outputs":outputs,"reference_output_id":args.reference_output,"signal":args.sweep,"microphone_id":args.microphone_id,"position_id":args.position_id}),
    )?;
    progress(0, MeasureProgress::OpeningInput);
    let mut capture =
        Capture::start_bounded(&args.device, args.input_channel, args.sample_rate, budget)?;
    let started = Instant::now();
    let mut began = false;
    let mut analysis_start_frame = 0;
    let acquisition: Result<Progress, String> = (|| -> AppResult<Progress> {
        while capture.frames() < args.sample_rate as usize / 10 {
            check_cancelled(cancelled)?;
            capture.poll();
            if started.elapsed() > Duration::from_secs(2) || capture.faults() != 0 {
                return Err("input stream did not start cleanly".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        client.begin(&args.snapshot)?;
        began = true;
        capture.poll();
        analysis_start_frame = capture.frames() + args.sample_rate as usize / 5;
        progress(0, MeasureProgress::LeadIn);
        crate::microphone::before_playback(&mut capture, args.sample_rate, cancelled, &mut |p| {
            progress(0, p)
        })?;
        let mut state = client.start_sequence(
            1,
            &args.output,
            Some(&args.reference_output),
            &outputs,
            args.sweep,
        )?;
        let mut next = Instant::now();
        loop {
            check_cancelled(cancelled)?;
            capture.poll();
            if capture.faults() != 0 {
                return Err("input capture fault during batch".into());
            }
            if started.elapsed().as_secs_f64() > budget as f64 / args.sample_rate as f64 - 0.2 {
                return Err("batch recording limit reached".into());
            }
            if state.state == SignalState::Completed {
                break;
            }
            if matches!(state.state, SignalState::Cancelled | SignalState::Expired) {
                return Err("renderer sequence stopped before completion".into());
            }
            if Instant::now() >= next {
                state = client.status()?.ok_or("renderer lost sequence progress")?;
                progress(
                    active_target(&state).min(outputs.len() - 1),
                    MeasureProgress::Playing(state.clone()),
                );
                next = Instant::now() + Duration::from_millis(200);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        progress(outputs.len() - 1, MeasureProgress::Tail);
        drain_for(&mut capture, Duration::from_millis(600), cancelled)?;
        client.status()?;
        Ok(state)
    })()
    .map_err(|e| e.to_string());
    let ended = if began {
        client.end().map_err(|e| e.to_string())
    } else {
        Ok(())
    };
    drop(client);
    let recording = capture.finish();
    progress(outputs.len() - 1, MeasureProgress::Saving);
    save_wav(
        &args.out_dir.join("recording.wav"),
        &recording.samples,
        args.sample_rate,
    )?;
    let was_cancelled = cancelled.load(Ordering::Relaxed);
    let acquisition = acquisition.and_then(|p| ended.map(|()| p));
    let error = acquisition
        .as_ref()
        .err()
        .cloned()
        .or_else(|| {
            (recording.metadata.fault_flags != 0).then(|| "capture fault flags in batch".into())
        })
        .or_else(|| was_cancelled.then(|| "batch cancelled by user".into()));
    write_json(
        &args.out_dir.join("batch-report.json"),
        &serde_json::json!({"schema_version":1,"capture":recording.metadata,"renderer_progress":acquisition.as_ref().ok(),"analysis_start_frame":analysis_start_frame,"cancelled":was_cancelled,"error":error,"valid_for_correction":false}),
    )?;
    let mut outcome = BatchOutcome {
        directory: args.out_dir.clone(),
        takes: Vec::new(),
        cancelled: was_cancelled,
        error,
    };
    if outcome.error.is_some() {
        save_analysis(&outcome)?;
        return Ok(outcome);
    }
    progress(0, MeasureProgress::Analyzing);
    let ranges = match sequence::windows(
        &recording.samples[analysis_start_frame..],
        args.sample_rate,
        args.snapshot.sample_rate,
        args.sweep,
        outputs.len(),
    ) {
        Ok(r) => r,
        Err(error) => {
            outcome.error = Some(error);
            save_analysis(&outcome)?;
            return Ok(outcome);
        }
    };
    let state = acquisition.expect("checked acquisition");
    for (index, (output, range)) in outputs.iter().zip(ranges).enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            outcome.error = Some("batch analysis cancelled; full raw recording retained".into());
            break;
        }
        let start = analysis_start_frame + range.start;
        let end = analysis_start_frame + range.end;
        let mut child = args.clone();
        child.output = output.clone();
        child.out_dir = args.out_dir.join(format!("take-{:03}", index + 1));
        fs::create_dir(&child.out_dir)?;
        write_json(&child.out_dir.join("snapshot.json"), &args.snapshot)?;
        if let Some(text) = &calibration {
            fs::write(child.out_dir.join("microphone-calibration.txt"), text)?;
        }
        let origin = if recording.metadata.is_monitor {
            "sink_monitor_capture"
        } else {
            "microphone_capture"
        };
        let frames = args.sweep.frames(args.snapshot.sample_rate);
        write_json(
            &child.out_dir.join("stimulus.json"),
            &serde_json::json!({
                "schema_version":2,"protocol_version":omniphony_adapter::protocol::VERSION,"origin":origin,
                "microphone_id":args.microphone_id,"position_id":args.position_id,"output_id":output,"reference_output_id":args.reference_output,"signal":args.sweep,
                "segment_start_render_frames":[index as u64*2*frames,(index as u64*2+1)*frames,(index as u64*2+2)*frames],
                "sequence_window":SequenceWindow {target_index:index,source_start_frame:start,source_end_frame:end},
                "source_recording":"../recording.wav","calibration_applied":false,"calibration_file":calibration.as_ref().map(|_|"microphone-calibration.txt")
            }),
        )?;
        let samples = recording.samples[start..end].to_vec();
        let mut metadata = recording.metadata.clone();
        metadata.frames = samples.len();
        (
            metadata.peak_dbfs,
            metadata.rms_dbfs,
            metadata.clipped_samples,
        ) = levels(&samples);
        let take = finish_take(
            &child,
            Recording { metadata, samples },
            0,
            Ok(state.clone()),
            false,
            &mut |p| progress(index, p),
        )?;
        outcome.takes.push((output.clone(), take));
    }
    save_analysis(&outcome)?;
    Ok(outcome)
}

fn save_analysis(outcome: &BatchOutcome) -> AppResult<()> {
    write_json(
        &outcome.directory.join("batch-analysis.json"),
        &serde_json::json!({"schema_version":1,"takes":outcome.takes.iter().map(|(id,t)|serde_json::json!({"output_id":id,"folder":t.directory.file_name(),"accepted_for_analysis":t.accepted,"rejection_reason":t.rejection_reason})).collect::<Vec<_>>(),"cancelled":outcome.cancelled,"error":outcome.error,"valid_for_correction":false}),
    )
}
