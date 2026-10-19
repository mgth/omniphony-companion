//! Reanalysis never opens audio/network devices and never changes source take files.
use crate::{
    AppResult,
    files::{read_json, write_json},
};
pub use measurement_capture::clipping;
pub use measurement_capture::diagnostics::{
    AcquisitionDiagnostics, SegmentDiagnostics, SegmentStatus,
};
pub use measurement_capture::impulse::{ImpulseReport, ImpulseSegment};
pub use measurement_capture::spectral::{SpectralPoint, SpectralReport};
pub use measurement_capture::waveform::{BurstResponse, SignedTrace, TracePair, WaveformReport};
use measurement_capture::{MAX_SECONDS, diagnostics, impulse, timing};
use omniphony_adapter::{Progress, SignalState, Snapshot, Sweep, protocol};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Includes the lead-in before the analysis slice.
    #[serde(default)]
    pub capture_clipped_samples: usize,
    pub render_sample_rate: u32,
    pub signal: Sweep,
    pub sequence_window: Option<crate::batch::SequenceWindow>,
    pub original_accepted_for_analysis: bool,
    pub original_rejection_reason: Option<String>,
    pub diagnostics: diagnostics::AcquisitionDiagnostics,
    pub timing: Option<timing::TimingReport>,
    pub analysis_error: Option<String>,
    pub impulse: Option<Arc<ImpulseReport>>,
    pub impulse_error: Option<String>,
    #[serde(default)]
    pub quality: Option<crate::quality::Report>,
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
    #[serde(default)]
    sequence_window: Option<crate::batch::SequenceWindow>,
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

/// Inspect completed captures, including rejected levels/timing. Quality-only
/// rejections can receive new diagnostic responses while retaining their original
/// verdict. Capture faults and identity inconsistencies are never repaired here.
pub fn saved_take(directory: &Path) -> AppResult<SavedReview> {
    let stimulus: Stimulus = read_json(&directory.join("stimulus.json"))?;
    let capture: CaptureReport =
        crate::files::read_json_limited(&directory.join("capture-report.json"), 32 * 1024 * 1024)?;
    let snapshot: Snapshot = read_json(&directory.join("snapshot.json"))?;
    if !matches!(stimulus.schema_version, 1 | 2)
        || !(1..=3).contains(&capture.schema_version)
        || !(2..=protocol::VERSION).contains(&stimulus.protocol_version)
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
    let expected_frames = if let Some(window) = &stimulus.sequence_window {
        if stimulus.schema_version != 2
            || !(3..=protocol::VERSION).contains(&stimulus.protocol_version)
            || progress.sequence_outputs.get(window.target_index) != Some(&stimulus.output_id)
            || window
                .source_end_frame
                .checked_sub(window.source_start_frame)
                != Some(capture.capture.frames)
            || capture.analysis_start_frame != 0
        {
            return Err("inconsistent sequence window".into());
        }
        let mut seen = std::collections::HashSet::new();
        if progress.sequence_outputs.first() != Some(&progress.output_id)
            || progress
                .sequence_outputs
                .iter()
                .any(|id| !seen.insert(id) || !snapshot.outputs.iter().any(|o| &o.id == id))
        {
            return Err("invalid sequence output identities".into());
        }
        let batch: serde_json::Value = read_json(
            &directory
                .parent()
                .ok_or("missing batch folder")?
                .join("batch-report.json"),
        )?;
        if batch["renderer_progress"] != serde_json::to_value(progress)?
            || batch["error"] != serde_json::Value::Null
            || batch["cancelled"] != false
            || batch["capture"]["frames"]
                .as_u64()
                .is_none_or(|n| n < window.source_end_frame as u64)
        {
            return Err("sequence source report differs from the derived take".into());
        }
        protocol::sequence_frames(
            stimulus.signal,
            snapshot.sample_rate,
            progress.sequence_outputs.len(),
        )?
    } else {
        if stimulus.schema_version != 1
            || !progress.sequence_outputs.is_empty()
            || progress.output_id != stimulus.output_id
        {
            return Err("inconsistent single-speaker take".into());
        }
        frames * 3
    };
    let segment_offset = stimulus
        .sequence_window
        .as_ref()
        .map_or(0, |w| w.target_index as u64 * 2 * frames);
    if progress.state != SignalState::Completed
        || progress.rendered_frames != expected_frames
        || progress.total_frames != expected_frames
        || progress.signal != stimulus.signal
        || progress.reference_output_id.as_ref() != Some(&stimulus.reference_output_id)
        || stimulus.segment_start_render_frames
            != [
                segment_offset,
                segment_offset + frames,
                segment_offset + frames * 2,
            ]
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
    if let Some(window) = &stimulus.sequence_window {
        let mut source = hound::WavReader::open(
            directory
                .parent()
                .ok_or("missing batch folder")?
                .join("recording.wav"),
        )?;
        if source.spec() != spec
            || source.duration() as usize > measurement_capture::sequence::MAX_BATCH_FRAMES
            || window.source_end_frame > source.duration() as usize
        {
            return Err("invalid source sequence recording".into());
        }
        source.seek(window.source_start_frame as u32)?;
        let mut raw = source.samples::<f32>();
        for sample in &samples {
            if raw
                .next()
                .transpose()?
                .is_none_or(|v| v.to_bits() != sample.to_bits())
            {
                return Err("derived take differs from its raw sequence window".into());
            }
        }
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
    // Recompute from audio, preserving the original verdict. Quality-only
    // rejection no longer suppresses a diagnostic response; transport/identity
    // failures still cannot establish a synchronized measurement.
    let quality_rejection = capture.rejection_reason.as_deref().is_some_and(|r| {
        r.contains("signal-to-noise ratio")
            || r.contains("reference levels changed")
            || r == "clipped recording"
    });
    let inspectable = capture.accepted_for_analysis || legacy_timing_rejection || quality_rejection;
    let timing = if inspectable {
        timing::inspect(
            samples,
            spec.sample_rate,
            snapshot.sample_rate,
            stimulus.signal,
        )
    } else {
        Err(
            "Original acquisition failed outside analysis; synchronized results unavailable."
                .to_owned(),
        )
    };
    let response = timing.as_ref().ok().map(|timing| {
        impulse::inspect(
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
    let quality = crate::quality::assess(
        timing.as_ref().ok(),
        impulse.as_deref(),
        Some(&diagnostics),
        if clipped > 0 {
            Some("Clipped recording: diagnostic results cannot qualify a correction.")
        } else {
            (!inspectable).then_some(
                capture
                    .rejection_reason
                    .as_deref()
                    .unwrap_or("Original capture failed"),
            )
        },
    );
    let mut review = SavedReview {
        schema_version: 2,
        quality: Some(quality),
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
        capture_clipped_samples: clipped,
        render_sample_rate: snapshot.sample_rate,
        signal: stimulus.signal,
        sequence_window: stimulus.sequence_window,
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
