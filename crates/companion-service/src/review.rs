//! Reanalysis never opens audio/network devices and never changes source take files.
use crate::{
    AppResult,
    files::{read_json, write_json},
};
pub use measurement_capture::diagnostics::{
    AcquisitionDiagnostics, SegmentDiagnostics, SegmentStatus,
};
pub use measurement_capture::impulse::{ImpulseReport, ImpulseSegment};
use measurement_capture::{MAX_SECONDS, diagnostics, impulse, timing};
use omniphony_adapter::{Progress, SignalState, Snapshot, Sweep, protocol};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Serialize)]
pub struct SavedReview {
    pub schema_version: u32,
    #[serde(skip)]
    pub directory: PathBuf,
    #[serde(skip)]
    pub report_file: PathBuf,
    pub origin: String,
    pub output_id: String,
    pub reference_output_id: String,
    pub microphone_id: String,
    pub position_id: String,
    pub input_channel: u16,
    pub analysis_start_frame: usize,
    pub capture_sample_rate: u32,
    pub render_sample_rate: u32,
    pub signal: Sweep,
    pub original_accepted_for_analysis: bool,
    pub original_rejection_reason: Option<String>,
    pub diagnostics: diagnostics::AcquisitionDiagnostics,
    pub timing: Option<timing::TimingReport>,
    pub analysis_error: Option<String>,
    pub impulse: Option<Arc<ImpulseReport>>,
    pub impulse_error: Option<String>,
    pub valid_for_correction: bool,
}

#[derive(Deserialize)]
struct Stimulus {
    schema_version: u32,
    protocol_version: u32,
    origin: String,
    microphone_id: String,
    position_id: String,
    output_id: String,
    reference_output_id: String,
    signal: Sweep,
    segment_start_render_frames: [u64; 3],
    calibration_applied: bool,
}
#[derive(Deserialize)]
struct CaptureReport {
    schema_version: u32,
    origin: String,
    accepted_for_analysis: bool,
    rejection_reason: Option<String>,
    capture: CaptureFacts,
    renderer_progress: Progress,
    analysis_start_frame: usize,
    timing_frame_origin: String,
}
#[derive(Deserialize)]
struct CaptureFacts {
    frames: usize,
    sample_rate: u32,
    input_channel: u16,
    stream_channels: u16,
    fault_flags: u32,
    clipped_samples: usize,
    #[serde(default)]
    is_monitor: bool,
}

/// Inspect completed captures, including rejected levels/timing. Rejected takes
/// retain their verdict and receive diagnostics only (except the documented
/// legacy correlation-ambiguity case). Capture faults are never repaired here.
pub fn saved_take(directory: &Path) -> AppResult<SavedReview> {
    let stimulus: Stimulus = read_json(&directory.join("stimulus.json"))?;
    let capture: CaptureReport = read_json(&directory.join("capture-report.json"))?;
    let snapshot: Snapshot = read_json(&directory.join("snapshot.json"))?;
    if stimulus.schema_version != 1
        || !(1..=3).contains(&capture.schema_version)
        || stimulus.protocol_version != protocol::VERSION
        || capture.timing_frame_origin != "analysis_start_frame_in_recording_wav"
    {
        return Err("unsupported take schema, protocol or timing origin".into());
    }
    if !matches!(
        stimulus.origin.as_str(),
        "microphone_capture" | "sink_monitor_capture"
    ) || capture.origin != stimulus.origin
        || capture.capture.is_monitor != (stimulus.origin == "sink_monitor_capture")
        || stimulus.calibration_applied
    {
        return Err("unsupported or inconsistent capture origin/calibration".into());
    }
    for id in [
        &stimulus.microphone_id,
        &stimulus.position_id,
        &stimulus.output_id,
        &stimulus.reference_output_id,
    ] {
        if id.trim().is_empty() || id.len() > 128 {
            return Err("invalid identity in saved take".into());
        }
    }
    for id in [&stimulus.output_id, &stimulus.reference_output_id] {
        if snapshot.outputs.iter().filter(|o| &o.id == id).count() != 1 {
            return Err("saved speaker identity is absent or ambiguous in the snapshot".into());
        }
    }
    stimulus.signal.validate(snapshot.sample_rate)?;
    stimulus.signal.validate(capture.capture.sample_rate)?;
    let frames = stimulus.signal.frames(snapshot.sample_rate);
    let progress = &capture.renderer_progress;
    if progress.state != SignalState::Completed
        || progress.rendered_frames != frames * 3
        || progress.total_frames != frames * 3
        || progress.signal != stimulus.signal
        || progress.output_id != stimulus.output_id
        || progress.reference_output_id.as_ref() != Some(&stimulus.reference_output_id)
        || stimulus.segment_start_render_frames != [0, frames, frames * 2]
        || capture.capture.fault_flags != 0
        || capture.capture.input_channel == 0
        || capture.capture.input_channel > capture.capture.stream_channels
        || capture.capture.stream_channels > 64
    {
        return Err(
            "incomplete, faulty or inconsistent acquisition; reanalysis cannot repair it".into(),
        );
    }
    let legacy_timing_rejection = capture.schema_version == 1
        && capture.rejection_reason.as_deref()
            == Some("competing correlation peaks; direct arrival is ambiguous");
    if capture.accepted_for_analysis == capture.rejection_reason.is_some() {
        return Err("inconsistent acquisition verdict".into());
    }
    let mut wav = hound::WavReader::open(directory.join("recording.wav"))?;
    let spec = wav.spec();
    let count = wav.duration() as usize;
    if spec.channels != 1
        || spec.sample_format != hound::SampleFormat::Float
        || spec.bits_per_sample != 32
        || spec.sample_rate != capture.capture.sample_rate
        || count != capture.capture.frames
        || count > spec.sample_rate as usize * MAX_SECONDS as usize
        || capture.analysis_start_frame >= count
    {
        return Err(
            "recording must be a bounded mono float WAV matching the capture metadata".into(),
        );
    }
    let samples = wav.samples::<f32>().collect::<Result<Vec<_>, _>>()?;
    if samples.len() != count || samples.iter().any(|x| !x.is_finite()) {
        return Err("recording is truncated or nonfinite".into());
    }
    let clipped = samples.iter().filter(|x| x.abs() >= 0.999).count();
    if clipped != capture.capture.clipped_samples {
        return Err("recording clipping count differs from the capture metadata".into());
    }
    let samples = &samples[capture.analysis_start_frame..];
    let diagnostics = diagnostics::analyze(
        samples,
        spec.sample_rate,
        snapshot.sample_rate,
        stimulus.signal,
    )?;
    // Recompute from audio, never trust saved peaks or previously computed estimates.
    // Diagnostic review is not permission to promote a rejected acquisition.
    let timing = if !capture.accepted_for_analysis && !legacy_timing_rejection {
        Err("Original acquisition rejected; diagnostics only. Record a new take after resolving its cause.".to_owned())
    } else if clipped != 0 {
        Err("Clipped recording; timing and impulse analysis withheld.".to_owned())
    } else {
        timing::analyze(
            samples,
            spec.sample_rate,
            snapshot.sample_rate,
            stimulus.signal,
        )
    };
    let response = timing.as_ref().ok().map(|timing| {
        impulse::analyze(
            samples,
            spec.sample_rate,
            snapshot.sample_rate,
            stimulus.signal,
            timing,
        )
    });
    let impulse = response
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .cloned()
        .map(Arc::new);
    let impulse_error = response.and_then(Result::err);
    let mut review = SavedReview {
        schema_version: 2,
        directory: directory.to_owned(),
        report_file: PathBuf::new(),
        origin: stimulus.origin,
        output_id: stimulus.output_id,
        reference_output_id: stimulus.reference_output_id,
        microphone_id: stimulus.microphone_id,
        position_id: stimulus.position_id,
        input_channel: capture.capture.input_channel,
        analysis_start_frame: capture.analysis_start_frame,
        capture_sample_rate: spec.sample_rate,
        render_sample_rate: snapshot.sample_rate,
        signal: stimulus.signal,
        original_accepted_for_analysis: capture.accepted_for_analysis,
        original_rejection_reason: capture.rejection_reason,
        diagnostics,
        analysis_error: timing.as_ref().err().cloned(),
        timing: timing.ok(),
        impulse,
        impulse_error,
        valid_for_correction: false,
    };
    // Reserve a unique name, then publish the report atomically without overwrites.
    let reservation = tempfile::Builder::new()
        .prefix("analysis-")
        .tempfile_in(directory)?;
    review.report_file = reservation.path().with_extension("json");
    write_json(&review.report_file, &review)?;
    Ok(review)
}
