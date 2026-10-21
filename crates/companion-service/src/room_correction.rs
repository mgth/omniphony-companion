//! Room-correction filters from an alignment's stored responses, and their export.
//!
//! The main seat's takes (both passes) and every usable take of the guided positions
//! ([`crate::positions`]) are read back with their microphone calibration, then
//! [`calibration_core::room_correction`] designs one filter per output. The export
//! writes each filter as a mono 32-bit float WAV at the engine's rate, named after its
//! output, which Omniphony's speaker editor loads with "Choose filter…", next to a
//! summary of how it was designed. Nothing is sent to Omniphony here.
use crate::{
    AppResult,
    alignment::{self, Alignment},
    files::write_json,
    room_response,
};
use calibration_core::room_correction::{
    self as design, DesignSettings, Measurement, MicrophoneCalibration, OutputDesign,
    OutputMeasurements, PositionAgreement, Reflection,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SUMMARY: &str = "room-correction.json";
pub const CURVES: &str = "room-correction-curves.json";
/// Exports are saved in a folder of the alignment named after this and a stamp.
pub const FOLDER_PREFIX: &str = "room-correction-";

/// One design of every output of an alignment: what phase 6 will send.
#[derive(Debug, Clone)]
pub struct Design {
    pub settings: DesignSettings,
    pub outputs: Vec<OutputDesign>,
}
impl Design {
    pub fn designed(&self) -> impl Iterator<Item = &OutputDesign> {
        self.outputs.iter().filter(|o| o.filter.is_some())
    }
}

/// Design settings for `a`: the defaults at the rate its renderer ran at.
pub fn settings(a: &Alignment) -> DesignSettings {
    DesignSettings {
        sample_rate: a.snapshot.sample_rate,
        ..DesignSettings::default()
    }
}

/// Why an alignment has nothing to design from yet.
pub fn readiness(a: &Alignment) -> Result<(), String> {
    let stored = |folder: &Path| folder.join(room_response::FILE).is_file();
    let any = a.rows.iter().any(|r| {
        r.take_folders
            .iter()
            .any(|f| stored(&crate::workspace::absolute(f)))
    });
    if any {
        Ok(())
    } else {
        Err("This alignment's takes have no stored response: measure a new alignment to design correction filters.".into())
    }
}

/// The stored responses of every speaker of `a`, and for each speaker the takes left
/// out and why.
pub fn measurements(a: &Alignment) -> Vec<(OutputMeasurements, Vec<String>)> {
    a.rows
        .iter()
        .map(|row| {
            let mut sources: Vec<(PathBuf, bool, String)> = row
                .take_folders
                .iter()
                .enumerate()
                .map(|(pass, f)| (f.clone(), true, format!("pass {}", pass + 1)))
                .collect();
            for capture in &a.positions {
                for take in capture
                    .takes
                    .iter()
                    .filter(|t| t.output_id == row.output_id)
                {
                    if take.usable() {
                        sources.push((take.take_folder.clone(), false, capture.step.clone()));
                    }
                }
            }
            let mut measurements = vec![];
            let mut left_out = vec![];
            for (folder, main_seat, label) in sources {
                match read(
                    &crate::workspace::absolute(&folder),
                    &row.output_id,
                    main_seat,
                ) {
                    Ok(m) => measurements.push(m),
                    Err(reason) => left_out.push(format!("{label}: {reason}")),
                }
            }
            (
                OutputMeasurements {
                    output_id: row.output_id.clone(),
                    measurements,
                },
                left_out,
            )
        })
        .collect()
}

/// One take's stored response as design evidence, or why it is not.
fn read(folder: &Path, output_id: &str, main_seat: bool) -> Result<Measurement, String> {
    if !folder.join(room_response::FILE).is_file() {
        return Err("no stored response (measured before responses were kept)".into());
    }
    let (stored, full) = room_response::load(folder).map_err(|e| e.to_string())?;
    if stored.output_id != output_id {
        return Err(format!("stored response is of {}", stored.output_id));
    }
    if !stored.accepted_for_analysis {
        return Err("rejected take".into());
    }
    if stored.clipped_samples > 0 {
        return Err("clipped take".into());
    }
    let calibration = room_response::calibration(folder, None)
        .map_err(|e| e.to_string())?
        .map(|(calibration, used)| MicrophoneCalibration {
            id: used.sha256,
            points: calibration
                .points()
                .iter()
                .map(|p| [p.frequency_hz, p.correction_db])
                .collect(),
        });
    // The stored magnitude over its deconvolution noise, on the shared log grid.
    let magnitude = &stored.magnitude;
    let snr_db = magnitude
        .frequency_hz
        .iter()
        .zip(magnitude.magnitude_db[1].iter().zip(&magnitude.noise_db[1]))
        .map(|(&hz, (&signal, &noise))| [hz, f64::from(signal) - f64::from(noise)])
        .collect();
    let [_, speaker, _] = full.segments;
    Ok(Measurement {
        position_id: stored.position_id,
        main_seat,
        rate: full.rate,
        samples: speaker,
        anchor: full.anchor,
        band_hz: [
            f64::from(stored.signal.start_hz),
            f64::from(stored.signal.end_hz),
        ],
        calibration,
        snr_db: Some(snr_db),
    })
}

/// Design one filter per speaker of `a`. Subwoofers are left to bass integration.
pub fn design(a: &Alignment, settings: &DesignSettings) -> Result<Design, String> {
    settings.validate().map_err(|e| e.to_string())?;
    readiness(a)?;
    let outputs = measurements(a)
        .into_iter()
        .zip(&a.rows)
        .map(|((output, left_out), row)| {
            let mut result = if row.bass_only(&a.settings) {
                OutputDesign::refused(
                    &output.output_id,
                    "A subwoofer or LFE: left to bass integration.",
                )
            } else {
                design::design(&output, settings).map_err(|e| e.to_string())?
            };
            if !left_out.is_empty() {
                result
                    .warnings
                    .push(format!("Takes left out: {}.", left_out.join("; ")));
            }
            Ok(result)
        })
        .collect::<Result<_, String>>()?;
    Ok(Design {
        settings: settings.clone(),
        outputs,
    })
}

/// What the export says about one output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSummary {
    pub output_id: String,
    /// The filter's WAV in the export folder; absent when not corrected.
    pub file: Option<String>,
    pub refusal: Option<String>,
    pub warnings: Vec<String>,
    pub calibrated: bool,
    pub taps: Option<usize>,
    pub sample_rate: Option<u32>,
    /// The filter's largest gain: the headroom it needs.
    pub headroom_db: Option<f64>,
    pub deepest_cut_db: Option<f64>,
    pub normalisation_db: Option<f64>,
    pub band_hz: Option<[f64; 2]>,
    pub low_limit_hz: Option<f64>,
    pub evaluation_band_hz: Option<[f64; 2]>,
    /// RMS distance from the target before and after (predicted).
    pub deviation_db: Option<[f64; 2]>,
    pub repeatability_db: Option<f64>,
    pub spread_db: Option<f64>,
    pub positions: Vec<PositionAgreement>,
    pub reflections: Vec<Reflection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub schema_version: u32,
    pub method: String,
    /// Target, limits, taps and rate.
    pub settings: DesignSettings,
    pub outputs: Vec<OutputSummary>,
}

/// A file name for `output_id`: its letters, digits, `-`, `_` and `.`, others as `_`.
pub fn file_name(output_id: &str) -> String {
    let stem: String = output_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.wav", stem.trim_start_matches('.'))
}

/// Write `design`'s filters, its summary and its curves into a new folder of `a`.
pub fn export(a: &Alignment, design: &Design) -> AppResult<(PathBuf, Summary)> {
    let directory = crate::workspace::absolute(&a.directory)
        .join(format!("{FOLDER_PREFIX}{}", alignment::stamp()));
    std::fs::create_dir(&directory)?;
    let mut outputs = vec![];
    let mut used = std::collections::BTreeSet::new();
    for output in &design.outputs {
        let file = match &output.filter {
            Some(filter) => {
                let mut name = file_name(&output.output_id);
                while !used.insert(name.clone()) {
                    name.insert(name.len() - 4, '_');
                }
                write_wav(&directory.join(&name), &filter.taps, filter.sample_rate)?;
                Some(name)
            }
            None => None,
        };
        let filter = output.filter.as_ref();
        outputs.push(OutputSummary {
            output_id: output.output_id.clone(),
            file,
            refusal: output.refusal.clone(),
            warnings: output.warnings.clone(),
            calibrated: output.calibrated,
            taps: filter.map(|f| f.taps.len()),
            sample_rate: filter.map(|f| f.sample_rate),
            headroom_db: filter.map(|f| f.headroom_db),
            deepest_cut_db: filter.map(|f| f.deepest_cut_db),
            normalisation_db: filter.map(|f| f.normalisation_db),
            band_hz: output.band_hz,
            low_limit_hz: output.low_limit_hz,
            evaluation_band_hz: output.evaluation_band_hz,
            deviation_db: output.deviation_db,
            repeatability_db: output.repeatability_db,
            spread_db: output.spread_db,
            positions: output.positions.clone(),
            reflections: output.reflections.clone(),
        });
    }
    let summary = Summary {
        schema_version: 1,
        method: design::METHOD.into(),
        settings: design.settings.clone(),
        outputs,
    };
    write_json(&directory.join(SUMMARY), &summary)?;
    let curves: Vec<_> = design
        .outputs
        .iter()
        .filter_map(|o| {
            o.curves
                .as_ref()
                .map(|c| serde_json::json!({ "output_id": o.output_id, "curves": c }))
        })
        .collect();
    crate::files::write_json_compact(&directory.join(CURVES), &curves)?;
    Ok((directory, summary))
}

fn write_wav(path: &Path, taps: &[f32], rate: u32) -> AppResult<()> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )?;
    for &tap in taps {
        writer.write_sample(tap)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Design and export in one go, for the alignment's own settings.
pub fn design_and_export(a: &Alignment) -> Result<(PathBuf, Summary), String> {
    let design = design(a, &settings(a))?;
    if design.designed().next().is_none() {
        let reasons: Vec<String> = design
            .outputs
            .iter()
            .map(|o| {
                format!(
                    "{}: {}",
                    o.output_id,
                    o.refusal.as_deref().unwrap_or("not designed")
                )
            })
            .collect();
        return Err(format!(
            "No filter could be designed. {}",
            reasons.join(" ")
        ));
    }
    export(a, &design).map_err(|e| e.to_string())
}
