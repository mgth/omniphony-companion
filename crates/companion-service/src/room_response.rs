//! The unweighted response of a take, kept next to it for room-correction design.
//!
//! `response.wav` holds the deconvolved response of the three segments as measured: no
//! band weighting and no microphone calibration. `response.json` says how to read it and
//! carries its magnitude, raw and, with a calibration file, calibrated. The arrival
//! review keeps its own band-weighted response in the capture report; nothing here
//! changes timing, levels or the take's verdict.
use crate::{
    AppResult,
    files::{read_json, write_json_compact},
    measurement::MeasureRequest,
};
use measurement_capture::{
    calibration::Calibration,
    impulse::{FullResponse, PRE_SECONDS},
    response::{self, MagnitudeReport},
};
use omniphony_adapter::Sweep;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const FILE: &str = "response.json";
pub const WAV: &str = "response.wav";
/// The copy of the chosen calibration file every take folder keeps.
pub const CALIBRATION_FILE: &str = "microphone-calibration.txt";
pub const SEGMENTS: [&str; 3] = ["reference_before", "speaker", "reference_after"];
/// Shown wherever a response has no calibration; room correction will require one.
pub const NO_CALIBRATION: &str =
    "No microphone calibration: the magnitude includes the microphone's own response.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationUse {
    /// The copy in the take folder, [`CALIBRATION_FILE`].
    pub file: String,
    /// The name of the chosen file, without its folder.
    pub source_name: Option<String>,
    /// Of the copied text, so a design can tell two calibrations apart.
    pub sha256: String,
    pub points: usize,
    pub coverage_hz: [f64; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    pub schema_version: u32,
    pub method: String,
    /// One float channel per segment, in [`SEGMENTS`] order; raw and unweighted.
    pub response_file: String,
    pub segments: [String; 3],
    /// Response frames per renderer second after clock compensation. The WAV header can
    /// only hold the nominal capture rate, `file_sample_rate`.
    pub sample_rate_hz: f64,
    pub file_sample_rate: u32,
    /// Frame of the segment's correlation anchor in each channel.
    pub anchor_frame: usize,
    /// Where each segment's correlation peak lies after `anchor_frame`, in frames.
    pub anchor_offsets: [f64; 3],
    /// How far the speaker segment's correlation peak lies after the midpoint of the
    /// reference peaks, in frames: the speaker's time base relative to the reference's
    /// arrival. Absent in responses stored before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_offset_frames: Option<f64>,
    pub frames: usize,
    pub output_id: String,
    pub reference_output_id: String,
    pub microphone_id: String,
    pub position_id: String,
    pub signal: Sweep,
    /// The take's verdict: a rejected or clipped take keeps its response for
    /// inspection, never as design evidence.
    pub accepted_for_analysis: bool,
    pub clipped_samples: usize,
    pub calibration: Option<CalibrationUse>,
    pub magnitude: MagnitudeReport,
}

impl StoredResponse {
    /// The magnitude a correction should use: calibrated when a calibration was applied.
    pub fn corrected_magnitude_db(&self) -> Option<&[Vec<f32>; 3]> {
        self.magnitude.calibrated_magnitude_db.as_ref()
    }
}

/// The calibration copied into `directory`, if any, with what identifies it.
pub fn calibration(
    directory: &Path,
    source: Option<&Path>,
) -> AppResult<Option<(Calibration, CalibrationUse)>> {
    let path = directory.join(CALIBRATION_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = crate::measurement::read_calibration(&path)?;
    let calibration = Calibration::parse(&text)?;
    let used = CalibrationUse {
        file: CALIBRATION_FILE.into(),
        source_name: source
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned()),
        sha256: sha256(text.as_bytes()),
        points: calibration.points().len(),
        coverage_hz: calibration.coverage_hz(),
    };
    Ok(Some((calibration, used)))
}

/// Read and identify the calibration file at `path`, as a take would use it.
pub fn summarize(path: &Path) -> Result<CalibrationUse, String> {
    let text = crate::measurement::read_calibration(path).map_err(|e| e.to_string())?;
    let calibration = Calibration::parse(&text)?;
    Ok(CalibrationUse {
        file: CALIBRATION_FILE.into(),
        source_name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        sha256: sha256(text.as_bytes()),
        points: calibration.points().len(),
        coverage_hz: calibration.coverage_hz(),
    })
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Write the response and its description into the take folder `args.out_dir`.
pub fn store(
    args: &MeasureRequest,
    full: &FullResponse,
    capture_rate: u32,
    accepted_for_analysis: bool,
    clipped_samples: usize,
) -> AppResult<StoredResponse> {
    let directory = &args.out_dir;
    let calibration = calibration(directory, args.calibration.as_deref())?;
    let magnitude = response::magnitude(full, args.sweep, calibration.as_ref().map(|(c, _)| c))?;
    let frames = full.segments[0].len();
    write_wav(&directory.join(WAV), &full.segments, capture_rate)?;
    let stored = StoredResponse {
        schema_version: 1,
        method: format!(
            "{}; analytic-template deconvolution, {:.0} ms before to {:.0} ms after the anchor",
            response::METHOD,
            PRE_SECONDS * 1000.0,
            (frames - 1 - full.anchor) as f64 / full.rate * 1000.0
        ),
        response_file: WAV.into(),
        segments: SEGMENTS.map(String::from),
        sample_rate_hz: full.rate,
        file_sample_rate: capture_rate,
        anchor_frame: full.anchor,
        anchor_offsets: full.anchor_offsets,
        target_offset_frames: Some(full.target_offset),
        frames,
        output_id: args.output.clone(),
        reference_output_id: args.reference_output.clone(),
        microphone_id: args.microphone_id.clone(),
        position_id: args.position_id.clone(),
        signal: args.sweep,
        accepted_for_analysis,
        clipped_samples,
        calibration: calibration.map(|(_, used)| used),
        magnitude,
    };
    write_json_compact(&directory.join(FILE), &stored)?;
    Ok(stored)
}

/// Read a stored response back, checking the WAV against its description.
pub fn load(directory: &Path) -> AppResult<(StoredResponse, FullResponse)> {
    let stored: StoredResponse = read_json(&directory.join(FILE))?;
    if stored.schema_version != 1 || stored.segments != SEGMENTS.map(String::from) {
        return Err("unsupported stored response".into());
    }
    let mut wav = hound::WavReader::open(directory.join(&stored.response_file))?;
    let spec = wav.spec();
    if spec.channels != 3
        || spec.sample_format != hound::SampleFormat::Float
        || spec.bits_per_sample != 32
        || spec.sample_rate != stored.file_sample_rate
        || wav.duration() as usize != stored.frames
        || stored.anchor_frame >= stored.frames
    {
        return Err("stored response WAV differs from its description".into());
    }
    let mut segments: [Vec<f32>; 3] = Default::default();
    for (i, sample) in wav.samples::<f32>().enumerate() {
        let sample = sample?;
        if !sample.is_finite() {
            return Err("nonfinite stored response".into());
        }
        segments[i % 3].push(sample);
    }
    let full = FullResponse {
        rate: stored.sample_rate_hz,
        anchor: stored.anchor_frame,
        anchor_offsets: stored.anchor_offsets,
        target_offset: stored.target_offset_frames.unwrap_or(0.0),
        segments,
    };
    Ok((stored, full))
}

fn write_wav(path: &Path, segments: &[Vec<f32>; 3], rate: u32) -> AppResult<()> {
    let mut pending = tempfile::NamedTempFile::new_in(crate::files::parent(path))?;
    {
        let mut writer = hound::WavWriter::new(
            std::io::BufWriter::new(pending.as_file_mut()),
            hound::WavSpec {
                channels: 3,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        for i in 0..segments[0].len() {
            for segment in segments {
                writer.write_sample(segment[i])?;
            }
        }
        writer.finalize()?;
    }
    pending.as_file().sync_all()?;
    pending.persist_noclobber(path)?;
    Ok(())
}
