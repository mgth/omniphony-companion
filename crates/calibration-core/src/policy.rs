use serde::{Deserialize, Serialize};

use crate::model::finite_range;
use crate::{
    Error, InstallationSnapshot, MeasurementBatch, Observation, ObservationOrigin, Result,
    SCHEMA_VERSION, Session, SpeakerRole, require,
};

/// Prototype policy. Acoustic tolerances must be validated on real hardware.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationPolicy {
    pub minimum_snr_db: f64,
    pub minimum_gain_db: f64,
    pub maximum_gain_db: f64,
    pub maximum_delay_ms: f64,
    pub arrival_tolerance_ms: f64,
    pub level_tolerance_db: f64,
}

impl Default for CalibrationPolicy {
    fn default() -> Self {
        Self {
            minimum_snr_db: 20.0,
            minimum_gain_db: -60.0,
            maximum_gain_db: 24.0,
            maximum_delay_ms: 100.0,
            arrival_tolerance_ms: 0.1,
            level_tolerance_db: 0.5,
        }
    }
}

impl CalibrationPolicy {
    pub fn validate(&self) -> Result<()> {
        finite_range(self.minimum_snr_db, 0.0, 200.0, "minimum SNR")?;
        finite_range(self.minimum_gain_db, -120.0, 60.0, "minimum gain")?;
        finite_range(
            self.maximum_gain_db,
            self.minimum_gain_db,
            60.0,
            "maximum gain",
        )?;
        finite_range(self.maximum_delay_ms, 0.0, 10_000.0, "maximum delay")?;
        finite_range(self.arrival_tolerance_ms, 0.0, 100.0, "arrival tolerance")?;
        finite_range(self.level_tolerance_db, 0.0, 20.0, "level tolerance")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationProfile {
    pub origin: ObservationOrigin,
    pub schema_version: u32,
    pub source_session_id: String,
    pub source_batch_id: String,
    pub reference_position_id: String,
    pub policy: CalibrationPolicy,
    pub base_snapshot: InstallationSnapshot,
    pub target_snapshot: InstallationSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpreadMetrics {
    pub speaker_count: usize,
    pub arrival_spread_ms: f64,
    pub level_spread_db: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    pub schema_version: u32,
    pub origin: ObservationOrigin,
    pub source_session_id: String,
    pub verification_batch_id: String,
    pub reference_position_id: String,
    pub before: SpreadMetrics,
    pub after: SpreadMetrics,
    pub passed: bool,
}

fn reference_observations<'a>(
    session: &Session,
    batch: &'a MeasurementBatch,
    policy: &CalibrationPolicy,
) -> Result<Vec<&'a Observation>> {
    let mut observations = Vec::new();
    for speaker in batch
        .snapshot
        .speakers
        .iter()
        .filter(|s| s.role == SpeakerRole::FullRange)
    {
        let observation = batch
            .observations
            .iter()
            .find(|o| o.speaker_id == speaker.id && o.position_id == session.reference_position_id)
            .ok_or_else(|| {
                Error::Invalid(format!("missing reference observation for {}", speaker.id))
            })?;
        require(
            !observation.clipped,
            format!("clipped observation for {}", speaker.id),
        )?;
        require(
            observation.timing_valid,
            format!("invalid timing for {}", speaker.id),
        )?;
        require(
            observation.snr_db >= policy.minimum_snr_db,
            format!("insufficient SNR for {}", speaker.id),
        )?;
        observations.push(observation);
    }
    require(
        observations.len() >= 2,
        "relative alignment requires at least two full-range speakers",
    )?;
    // This first policy assumes one calibrated microphone at the reference point.
    // Additional microphones/positions are retained but are not averaged blindly.
    require(
        observations
            .iter()
            .all(|o| o.microphone_id == observations[0].microphone_id),
        "use the same microphone for all reference-position observations",
    )?;
    Ok(observations)
}

fn metrics(observations: &[&Observation]) -> SpreadMetrics {
    let bounds = |value: fn(&Observation) -> f64| {
        let min = observations
            .iter()
            .map(|o| value(o))
            .fold(f64::INFINITY, f64::min);
        let max = observations
            .iter()
            .map(|o| value(o))
            .fold(f64::NEG_INFINITY, f64::max);
        max - min
    };
    SpreadMetrics {
        speaker_count: observations.len(),
        arrival_spread_ms: bounds(|o| o.arrival_ms),
        level_spread_db: bounds(|o| o.level_dbfs),
    }
}

/// Delay earlier arrivals to the latest and attenuate louder speakers to the
/// quietest. Existing gains/delays are included in the measurements and targets.
/// Subwoofers are preserved without automatic alignment in mission 1.
pub fn propose(session: &Session, policy: &CalibrationPolicy) -> Result<CalibrationProfile> {
    session.validate()?;
    policy.validate()?;
    let observations = reference_observations(session, &session.baseline, policy)?;
    let latest = observations
        .iter()
        .map(|o| o.arrival_ms)
        .fold(0.0, f64::max);
    let quietest = observations
        .iter()
        .map(|o| o.level_dbfs)
        .fold(0.0, f64::min);
    let mut target = session.baseline.snapshot.clone();
    for speaker in &mut target.speakers {
        if speaker.role == SpeakerRole::Subwoofer {
            continue;
        }
        // Coverage was checked above, but keep the lookup fallible as this is
        // a boundary accepting imported data, not a reason to panic.
        let observation = observations
            .iter()
            .find(|o| o.speaker_id == speaker.id)
            .ok_or_else(|| Error::Invalid(format!("missing observation for {}", speaker.id)))?;
        speaker.settings.delay_ms += latest - observation.arrival_ms;
        speaker.settings.gain_db += quietest - observation.level_dbfs;
        finite_range(
            speaker.settings.delay_ms,
            0.0,
            policy.maximum_delay_ms,
            "proposed output delay",
        )?;
        finite_range(
            speaker.settings.gain_db,
            policy.minimum_gain_db,
            policy.maximum_gain_db,
            "proposed output gain",
        )?;
    }
    target.validate()?;
    Ok(CalibrationProfile {
        origin: session.baseline.origin,
        schema_version: SCHEMA_VERSION,
        source_session_id: session.id.clone(),
        source_batch_id: session.baseline.id.clone(),
        reference_position_id: session.reference_position_id.clone(),
        policy: policy.clone(),
        base_snapshot: session.baseline.snapshot.clone(),
        target_snapshot: target,
    })
}

fn validate_profile(session: &Session, profile: &CalibrationProfile) -> Result<()> {
    let expected = propose(session, &profile.policy)?;
    if expected != *profile {
        return Err(Error::ProfileMismatch);
    }
    Ok(())
}

/// Return absolute settings after a compare-before-apply check.
/// A live adapter will also need an atomic revision check on the renderer.
pub fn settings_for_apply(
    session: &Session,
    profile: &CalibrationProfile,
    current: &InstallationSnapshot,
) -> Result<InstallationSnapshot> {
    validate_profile(session, profile)?;
    current.validate()?;
    if current != &profile.base_snapshot && current != &profile.target_snapshot {
        return Err(Error::StaleInstallation);
    }
    Ok(profile.target_snapshot.clone())
}

/// Restoration also refuses to overwrite edits made after applying the profile.
pub fn settings_for_restore(
    session: &Session,
    profile: &CalibrationProfile,
    current: &InstallationSnapshot,
) -> Result<InstallationSnapshot> {
    settings_for_apply(session, profile, current)?;
    Ok(profile.base_snapshot.clone())
}

/// Compare a new batch acquired with exactly the proposed output settings.
/// `passed` describes reference-position spread only, never the entire room.
pub fn verify(
    session: &Session,
    profile: &CalibrationProfile,
    measured: &MeasurementBatch,
) -> Result<VerificationReport> {
    validate_profile(session, profile)?;
    session.validate_batch(measured)?;
    require(
        measured.id != session.baseline.id,
        "verification requires a distinct acquisition batch",
    )?;
    require(
        measured.snapshot == profile.target_snapshot,
        "verification settings differ from the proposed profile",
    )?;
    require(
        measured.origin == session.baseline.origin,
        "cannot mix simulated and imported observations",
    )?;
    require(
        measured.timing_reference_id == session.baseline.timing_reference_id,
        "verification timing reference changed",
    )?;
    require(
        measured.level_method_id == session.baseline.level_method_id,
        "verification level method changed",
    )?;
    let before_observations = reference_observations(session, &session.baseline, &profile.policy)?;
    let after_observations = reference_observations(session, measured, &profile.policy)?;
    require(
        before_observations[0].microphone_id == after_observations[0].microphone_id,
        "verification reference microphone changed",
    )?;
    let before = metrics(&before_observations);
    let after = metrics(&after_observations);
    let passed = after.arrival_spread_ms <= profile.policy.arrival_tolerance_ms
        && after.level_spread_db <= profile.policy.level_tolerance_db;
    Ok(VerificationReport {
        schema_version: SCHEMA_VERSION,
        origin: measured.origin,
        source_session_id: session.id.clone(),
        verification_batch_id: measured.id.clone(),
        reference_position_id: session.reference_position_id.clone(),
        before,
        after,
        passed,
    })
}
