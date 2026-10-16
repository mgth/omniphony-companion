use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Result, require};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerRole {
    FullRange,
    Subwoofer,
}

/// Absolute renderer output settings, not increments to add at every apply.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSettings {
    pub gain_db: f64,
    pub delay_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Speaker {
    pub id: String,
    pub label: String,
    pub role: SpeakerRole,
    pub settings: OutputSettings,
}

/// Snapshot of the settings actually used to acquire a batch.
/// The future adapter must also revision any relevant routing/processing change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationSnapshot {
    pub installation_id: String,
    pub layout_revision: String,
    pub sample_rate_hz: u32,
    pub speakers: Vec<Speaker>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Microphone {
    pub id: String,
    pub label: String,
    /// Identifies the calibration already used by the observation producer.
    /// This field does not load or apply a microphone calibration file.
    pub calibration_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOrigin {
    Simulated,
    Imported,
    Measured,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub speaker_id: String,
    pub microphone_id: String,
    pub position_id: String,
    /// Arrival relative to a shared acoustic/hardware reference, not OSC time.
    pub arrival_ms: f64,
    /// Comparable relative levels using the same stimulus, calibration and band.
    /// This is not an absolute SPL measurement.
    // Legacy field name: measured transfer levels are relative dB, not SPL or PCM RMS.
    pub level_dbfs: f64,
    pub snr_db: f64,
    pub clipped: bool,
    pub timing_valid: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementBatch {
    pub schema_version: u32,
    pub id: String,
    pub origin: ObservationOrigin,
    pub timing_reference_id: String,
    pub level_method_id: String,
    pub snapshot: InstallationSnapshot,
    /// One observation per speaker/position. Repeats belong in separate batches.
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub schema_version: u32,
    pub id: String,
    pub microphones: Vec<Microphone>,
    pub positions: Vec<Position>,
    pub reference_position_id: String,
    pub baseline: MeasurementBatch,
}

fn identifier(value: &str, kind: &str) -> Result<()> {
    require(
        !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control),
        format!("{kind} must be a nonempty identifier of at most 128 bytes"),
    )
}

pub(crate) fn finite_range(value: f64, min: f64, max: f64, name: &str) -> Result<()> {
    require(
        value.is_finite() && (min..=max).contains(&value),
        format!("{name} must be finite and within {min}..={max}"),
    )
}

fn unique_ids<'a>(ids: impl Iterator<Item = &'a str>, kind: &str) -> Result<HashSet<&'a str>> {
    let mut unique = HashSet::new();
    for id in ids {
        identifier(id, kind)?;
        require(unique.insert(id), format!("duplicate {kind}: {id}"))?;
    }
    Ok(unique)
}

impl InstallationSnapshot {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.installation_id, "installation id")?;
        identifier(&self.layout_revision, "layout revision")?;
        require(
            (8_000..=384_000).contains(&self.sample_rate_hz),
            "unsupported sample rate",
        )?;
        require(
            (1..=256).contains(&self.speakers.len()),
            "expected 1..=256 speakers",
        )?;
        unique_ids(self.speakers.iter().map(|s| s.id.as_str()), "speaker id")?;
        for speaker in &self.speakers {
            finite_range(speaker.settings.gain_db, -120.0, 60.0, "output gain")?;
            finite_range(speaker.settings.delay_ms, 0.0, 10_000.0, "output delay")?;
        }
        Ok(())
    }
}

impl Session {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema_version == SCHEMA_VERSION,
            "unsupported session schema version",
        )?;
        identifier(&self.id, "session id")?;
        require(
            (1..=64).contains(&self.microphones.len()),
            "expected 1..=64 microphones",
        )?;
        require(
            (1..=128).contains(&self.positions.len()),
            "expected 1..=128 positions",
        )?;
        unique_ids(
            self.microphones.iter().map(|m| m.id.as_str()),
            "microphone id",
        )?;
        let positions = unique_ids(self.positions.iter().map(|p| p.id.as_str()), "position id")?;
        require(
            positions.contains(self.reference_position_id.as_str()),
            "unknown reference position",
        )?;
        for microphone in &self.microphones {
            if let Some(calibration) = &microphone.calibration_id {
                identifier(calibration, "microphone calibration id")?;
            }
        }
        self.validate_batch(&self.baseline)
    }

    pub fn validate_batch(&self, batch: &MeasurementBatch) -> Result<()> {
        require(
            batch.schema_version == SCHEMA_VERSION,
            "unsupported measurement schema version",
        )?;
        identifier(&batch.id, "batch id")?;
        identifier(&batch.timing_reference_id, "timing reference id")?;
        identifier(&batch.level_method_id, "level method id")?;
        batch.snapshot.validate()?;
        require(
            (1..=32_768).contains(&batch.observations.len()),
            "expected 1..=32768 observations",
        )?;
        let mut pairs = HashSet::new();
        for observation in &batch.observations {
            require(
                batch
                    .snapshot
                    .speakers
                    .iter()
                    .any(|s| s.id == observation.speaker_id),
                "observation refers to an unknown speaker",
            )?;
            require(
                self.microphones
                    .iter()
                    .any(|m| m.id == observation.microphone_id),
                "observation refers to an unknown microphone",
            )?;
            require(
                self.positions
                    .iter()
                    .any(|p| p.id == observation.position_id),
                "observation refers to an unknown position",
            )?;
            require(
                pairs.insert((&observation.speaker_id, &observation.position_id)),
                "duplicate speaker/position observation; put repeats in separate batches",
            )?;
            finite_range(observation.arrival_ms, -10_000.0, 10_000.0, "arrival")?;
            finite_range(observation.level_dbfs, -300.0, 300.0, "relative level")?;
            finite_range(observation.snr_db, -100.0, 200.0, "signal-to-noise ratio")?;
        }
        Ok(())
    }
}
