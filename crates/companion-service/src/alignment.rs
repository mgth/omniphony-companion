//! Repeated measurement, proposal and explicit live gain/delay application.
use crate::{
    controller::{Backend, Settings},
    files::{read_json, write_json},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use calibration_core::{
    self as core,
    repeatability::{QualifiedEstimate, RepeatEstimate},
};
use measurement_capture::level;
use omniphony_adapter::{Snapshot, protocol::OutputSetting};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub output_id: String,
    pub take_folders: Vec<PathBuf>,
    pub repeats: Vec<RepeatEstimate>,
    pub qualified: Option<QualifiedEstimate>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassReport {
    pub analyzed_speakers: usize,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alignment {
    pub schema_version: u32,
    #[serde(skip)]
    pub directory: PathBuf,
    pub endpoint: SocketAddr,
    pub settings: Settings,
    pub snapshot: Snapshot,
    pub rows: Vec<Row>,
    /// Absent on older sessions; progress is descriptive, never eligibility evidence.
    #[serde(default)]
    pub pass_reports: Option<Vec<PassReport>>,
    #[serde(default)]
    pub verification_pass_reports: Option<Vec<PassReport>>,
    pub blockers: Vec<String>,
    pub session: Option<core::Session>,
    pub profile: Option<core::CalibrationProfile>,
    pub applied_snapshot: Option<Snapshot>,
    pub restored_snapshot: Option<Snapshot>,
    pub verification_note: Option<String>,
    pub verification: Option<core::VerificationReport>,
    /// Set before sending any mutation. Remains true if acknowledgement/storage fails.
    pub application_uncertain: bool,
}
#[derive(Debug, Clone)]
pub struct Outcome {
    pub alignment: Alignment,
    pub takes: Vec<(String, TakeOutcome)>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn estimate(take: &TakeOutcome) -> RepeatEstimate {
    let mut result = RepeatEstimate {
        arrival_ms: None,
        level_db: None,
        minimum_snr_db: None,
        reference_level_db: None,
        reasons: vec![],
    };
    if !take.accepted || take.cancelled {
        result.reasons.push(
            take.rejection_reason
                .clone()
                .unwrap_or("capture did not complete".into()),
        );
        return result;
    }
    let Some(ir) = &take.impulse else {
        result.reasons.push(
            take.impulse_error
                .clone()
                .unwrap_or("missing impulse response".into()),
        );
        return result;
    };
    result.arrival_ms = ir.relative_arrival_ms;
    result.reasons.extend(ir.unresolved_reasons.clone());
    if result.arrival_ms.is_none() && result.reasons.is_empty() {
        result.reasons.push("arrival unresolved".into());
    }
    let bands: Result<Vec<_>, _> = ir
        .segments
        .iter()
        .map(|s| {
            s.spectral
                .as_ref()
                .ok_or("missing spectrum".to_owned())
                .and_then(level::estimate)
        })
        .collect();
    match bands {
        Ok(bands) => {
            result.level_db = Some(bands[1].level_db);
            result.minimum_snr_db = Some(
                bands
                    .iter()
                    .map(|b| b.minimum_snr_db)
                    .fold(f64::INFINITY, f64::min),
            );
            result.reference_level_db = Some((bands[0].level_db + bands[2].level_db) * 0.5);
            if (bands[0].level_db - bands[2].level_db).abs() > 0.5 {
                result
                    .reasons
                    .push("reference level changed within the take".into());
            }
        }
        Err(e) => result.reasons.push(e),
    }
    result
}

pub fn capture<B: Backend + ?Sized>(
    backend: &B,
    request: MeasureRequest,
    settings: Settings,
    outputs: Vec<String>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize, MeasureProgress),
) -> Result<Outcome, String> {
    if outputs.len() < 2
        || outputs.len() > 32
        || outputs
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != outputs.len()
    {
        return Err("Select 2–32 broadband speakers for alignment.".into());
    }
    if request.device.starts_with("pipewire:monitor:") {
        return Err("Alignment requires an acoustic microphone, not a sink monitor.".into());
    }
    if !request.endpoint.ip().is_loopback() {
        return Err("Alignment requires a local renderer.".into());
    }
    if request.sweep.start_hz > 450.0 || request.sweep.end_hz < 5000.0 {
        return Err("Alignment requires a sweep covering 500–2000 Hz levels and broadband arrival analysis.".into());
    }
    std::fs::create_dir_all(crate::files::parent(&request.out_dir)).map_err(err)?;
    std::fs::create_dir(&request.out_dir).map_err(err)?;
    let mut alignment = Alignment {
        schema_version: 1,
        directory: request.out_dir.clone(),
        endpoint: request.endpoint,
        settings,
        snapshot: request.snapshot.clone(),
        rows: outputs
            .iter()
            .map(|id| Row {
                output_id: id.clone(),
                take_folders: vec![],
                repeats: vec![],
                qualified: None,
                reason: None,
            })
            .collect(),
        blockers: vec![],
        pass_reports: Some(vec![]),
        verification_pass_reports: None,
        session: None,
        profile: None,
        applied_snapshot: None,
        restored_snapshot: None,
        verification_note: None,
        verification: None,
        application_uncertain: false,
    };
    let mut takes = vec![];
    // Preserve one calibration file for both passes, even if its source is edited.
    // This relative-level method intentionally applies no capsule correction.
    let calibration = request
        .calibration
        .as_ref()
        .map(|p| crate::measurement::read_calibration(p))
        .transpose()
        .map_err(err)?;
    if let Some(text) = &calibration {
        std::fs::write(request.out_dir.join("microphone-calibration.txt"), text).map_err(err)?;
    }
    for pass in 0..2 {
        if cancel.load(Ordering::Relaxed) {
            alignment.blockers.push(
                "Alignment cancelled; incomplete measurements cannot make a proposal.".into(),
            );
            break;
        }
        let mut child = request.clone();
        child.output = outputs[0].clone();
        child.out_dir = request.out_dir.join(format!("pass-{}", pass + 1));
        child.calibration = calibration
            .as_ref()
            .map(|_| request.out_dir.join("microphone-calibration.txt"));
        let batch = match backend.measure_batch(child, outputs.clone(), cancel, &mut |i, p| {
            progress(pass, i, p)
        }) {
            Ok(batch) => batch,
            Err(error) => {
                alignment.pass_reports.as_mut().unwrap().push(PassReport {
                    analyzed_speakers: 0,
                    error: Some(error.clone()),
                });
                alignment.blockers.push(error);
                break;
            }
        };
        let incomplete = batch.takes.len() != outputs.len()
            || batch
                .takes
                .iter()
                .zip(&outputs)
                .any(|((id, _), expected)| id != expected);
        let pass_error = batch
            .error
            .clone()
            .or_else(|| batch.cancelled.then(|| "Alignment cancelled.".into()))
            .or_else(|| {
                incomplete
                    .then(|| "The batch did not return every selected speaker in order.".into())
            });
        alignment.pass_reports.as_mut().unwrap().push(PassReport {
            analyzed_speakers: batch.takes.len(),
            error: pass_error.clone(),
        });
        if let Some(error) = pass_error {
            alignment.blockers.push(error);
        }
        if !incomplete {
            for (row, (_, take)) in alignment.rows.iter_mut().zip(&batch.takes) {
                row.take_folders.push(take.directory.clone());
                row.repeats.push(estimate(take));
            }
        }
        takes.extend(batch.takes);
        if !alignment.blockers.is_empty() {
            break;
        }
    }
    if cancel.load(Ordering::Relaxed) {
        alignment.blockers.push("Alignment cancelled.".into());
    }
    for row in &mut alignment.rows {
        match core::repeatability::qualify(&row.repeats) {
            Ok(q) => row.qualified = Some(q),
            Err(e) => row.reason = Some(e.to_string()),
        }
    }
    let refs: Vec<_> = alignment
        .rows
        .iter()
        .flat_map(|r| &r.repeats)
        .filter_map(|r| r.reference_level_db)
        .collect();
    if !refs.is_empty()
        && refs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - refs.iter().copied().fold(f64::INFINITY, f64::min)
            > 0.5
    {
        alignment.blockers.push(
            "Reference level changed across speakers; keep microphone and input gain fixed.".into(),
        );
    }
    if alignment.blockers.is_empty() && alignment.rows.iter().all(|r| r.qualified.is_some()) {
        match make_session(&alignment).and_then(|s| {
            core::propose(
                &s,
                &core::CalibrationPolicy {
                    maximum_delay_ms: alignment.snapshot.max_delay_ms as f64,
                    ..Default::default()
                },
            )
            .map(|p| (s, p))
            .map_err(err)
        }) {
            Ok((s, p)) => {
                alignment.session = Some(s);
                alignment.profile = Some(p);
            }
            Err(e) => alignment.blockers.push(e),
        }
    }
    write_json(&request.out_dir.join("alignment-plan.json"), &alignment).map_err(err)?;
    save(&alignment)?;
    Ok(Outcome { alignment, takes })
}
fn make_session(a: &Alignment) -> Result<core::Session, String> {
    let mut speakers = vec![];
    let mut observations = vec![];
    for row in &a.rows {
        let q = row.qualified.as_ref().ok_or("unqualified speaker")?;
        let output = a
            .snapshot
            .outputs
            .iter()
            .find(|o| o.id == row.output_id)
            .ok_or("unknown speaker")?;
        speakers.push(core::Speaker {
            id: output.id.clone(),
            label: output.name.clone(),
            role: core::SpeakerRole::FullRange,
            settings: core::OutputSettings {
                gain_db: output.gain_db as f64,
                delay_ms: output.delay_ms as f64,
            },
        });
        observations.push(core::Observation {
            speaker_id: output.id.clone(),
            microphone_id: a.settings.microphone_id.clone(),
            position_id: a.settings.position_id.clone(),
            arrival_ms: q.arrival_ms,
            level_dbfs: q.level_db,
            snr_db: q.minimum_snr_db,
            clipped: false,
            timing_valid: true,
        });
    }
    Ok(core::Session {
        schema_version: core::SCHEMA_VERSION,
        id: format!("alignment-{}", stamp()),
        microphones: vec![core::Microphone {
            id: a.settings.microphone_id.clone(),
            label: a.settings.microphone_id.clone(),
            calibration_id: None,
        }],
        positions: vec![core::Position {
            id: a.settings.position_id.clone(),
            label: a.settings.position_id.clone(),
        }],
        reference_position_id: a.settings.position_id.clone(),
        baseline: core::MeasurementBatch {
            schema_version: core::SCHEMA_VERSION,
            id: format!("measured-{}", stamp()),
            origin: core::ObservationOrigin::Measured,
            timing_reference_id: a
                .settings
                .reference_output
                .clone()
                .ok_or("missing reference")?,
            level_method_id: level::METHOD.into(),
            snapshot: core::InstallationSnapshot {
                installation_id: a.snapshot.instance_id.clone(),
                layout_revision: a.snapshot.revision.clone(),
                sample_rate_hz: a.snapshot.sample_rate,
                speakers,
            },
            observations,
        },
    })
}
impl Alignment {
    fn validate_evidence(&self) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .ok_or("No qualified measurement session.")?;
        if !self.blockers.is_empty()
            || self.rows.len() < 2
            || self.rows.len() > 32
            || session.baseline.origin != core::ObservationOrigin::Measured
            || self
                .settings
                .device
                .as_deref()
                .is_none_or(|d| d.starts_with("pipewire:monitor:"))
        {
            return Err("Alignment does not contain eligible microphone observations.".into());
        }
        for row in &self.rows {
            let q = core::repeatability::qualify(&row.repeats).map_err(err)?;
            if row.qualified.as_ref() != Some(&q)
                || row.reason.is_some()
                || row.take_folders.len() != 2
                || row.take_folders[0] == row.take_folders[1]
            {
                return Err("Alignment repeat evidence is inconsistent.".into());
            }
        }
        let expected = make_session(self)?;
        if session.microphones != expected.microphones
            || session.positions != expected.positions
            || session.reference_position_id != expected.reference_position_id
            || session.baseline.snapshot != expected.baseline.snapshot
            || session.baseline.observations != expected.baseline.observations
            || session.baseline.timing_reference_id != expected.baseline.timing_reference_id
            || session.baseline.level_method_id != expected.baseline.level_method_id
        {
            return Err("Alignment observations differ from the measured evidence.".into());
        }
        let refs: Vec<_> = self
            .rows
            .iter()
            .flat_map(|r| &r.repeats)
            .filter_map(|r| r.reference_level_db)
            .collect();
        if refs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - refs.iter().copied().fold(f64::INFINITY, f64::min)
            > 0.5
        {
            return Err("Reference level changed across the alignment.".into());
        }
        Ok(())
    }
    pub fn targets(&self, restore: bool) -> Result<Vec<OutputSetting>, String> {
        self.validate_evidence()?;
        let session = self
            .session
            .as_ref()
            .ok_or("No qualified measurement session.")?;
        let profile = self.profile.as_ref().ok_or("No qualified proposal.")?;
        // Recompute rather than trusting edited/deserialized target values.
        core::settings_for_apply(session, profile, &profile.base_snapshot).map_err(err)?;
        let snapshot = if restore {
            &profile.base_snapshot
        } else {
            &profile.target_snapshot
        };
        Ok(snapshot
            .speakers
            .iter()
            .map(|s| OutputSetting {
                output_id: s.id.clone(),
                gain_db: s.settings.gain_db as f32,
                delay_ms: s.settings.delay_ms as f32,
            })
            .collect())
    }
    pub fn expected_target(&self) -> Result<Snapshot, String> {
        let mut snapshot = self.snapshot.clone();
        for setting in self.targets(false)? {
            let output = snapshot
                .outputs
                .iter_mut()
                .find(|o| o.id == setting.output_id)
                .ok_or("unknown target")?;
            output.gain_db = setting.gain_db;
            output.delay_ms = setting.delay_ms;
        }
        Ok(snapshot)
    }
}

pub fn apply<B: Backend + ?Sized>(
    backend: &B,
    alignment: &mut Alignment,
    restore: bool,
) -> Result<Snapshot, String> {
    if alignment.application_uncertain {
        return Err("Previous application is uncertain. Inspect the renderer and saved original settings before further changes.".into());
    }
    let targets = alignment.targets(restore)?;
    if let Some(restored) = &alignment.restored_snapshot
        && !omniphony_adapter::same_configuration(restored, &alignment.snapshot)
    {
        return Err("Saved restoration state differs from the original settings.".into());
    }
    if let Some(applied) = &alignment.applied_snapshot
        && !omniphony_adapter::same_configuration(applied, &alignment.expected_target()?)
    {
        return Err("Saved application state differs from the proposal.".into());
    }
    let current = backend.discover(alignment.endpoint)?;
    let expected = alignment
        .applied_snapshot
        .as_ref()
        .or(alignment.restored_snapshot.as_ref())
        .unwrap_or(&alignment.snapshot);
    if current != *expected {
        return Err("Renderer changed since measurement/application. Refresh and measure again; no settings were changed.".into());
    }
    if restore && alignment.applied_snapshot.is_none() {
        return Ok(current);
    }
    if !restore && alignment.applied_snapshot.is_some() {
        return Ok(current);
    }
    alignment.application_uncertain = true;
    save(alignment)?;
    let result = backend.set_outputs(alignment.endpoint, current, targets)?;
    let target = if restore {
        alignment.snapshot.clone()
    } else {
        alignment.expected_target()?
    };
    if !omniphony_adapter::same_configuration(&result, &target) {
        return Err(
            "Renderer readback differs from requested settings; application is uncertain.".into(),
        );
    }
    alignment.applied_snapshot = if restore { None } else { Some(result.clone()) };
    if restore {
        alignment.restored_snapshot = Some(result.clone());
    }
    alignment.verification = None;
    alignment.verification_pass_reports = None;
    alignment.verification_note = None;
    alignment.application_uncertain = false;
    save(alignment)?;
    Ok(result)
}

pub fn verify<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize, MeasureProgress),
) -> Result<Outcome, String> {
    if a.application_uncertain {
        return Err("Application state is uncertain.".into());
    }
    a.targets(false)?;
    a.verification = None;
    a.verification_pass_reports = Some(vec![]);
    a.verification_note = Some("Verification has not completed.".into());
    save(a)?;
    let snapshot = a
        .applied_snapshot
        .clone()
        .ok_or("Apply the proposal before verification.")?;
    if backend.discover(a.endpoint)? != snapshot {
        return Err("Renderer changed after application; verification refused.".into());
    }
    let outputs = a
        .rows
        .iter()
        .map(|r| r.output_id.clone())
        .collect::<Vec<_>>();
    let request = request(
        a.endpoint,
        snapshot,
        &a.settings,
        outputs[0].clone(),
        a.directory.join(format!("verification-{}", stamp())),
    )?;
    let outcome = capture(
        backend,
        request,
        a.settings.clone(),
        outputs,
        cancel,
        progress,
    )?;
    a.verification_pass_reports = outcome.alignment.pass_reports.clone();
    let Some(measured) = outcome.alignment.session.as_ref() else {
        a.verification_note = Some(format!(
            "Verification evidence is unresolved. Review {}",
            outcome.alignment.directory.display()
        ));
        save(a)?;
        return Ok(outcome);
    };
    let session = a.session.as_ref().ok_or("missing baseline")?;
    let profile = a.profile.as_ref().ok_or("missing profile")?;
    if !omniphony_adapter::same_configuration(&outcome.alignment.snapshot, &a.expected_target()?) {
        return Err("Verification was not captured with the proposed state.".into());
    }
    let mut batch = measured.baseline.clone();
    // Renderer revision changes on application; physical values were compared above.
    batch.snapshot = profile.target_snapshot.clone();
    a.verification = Some(core::verify(session, profile, &batch).map_err(err)?);
    a.verification_note = None;
    save(a)?;
    Ok(outcome)
}

pub fn request(
    endpoint: SocketAddr,
    snapshot: Snapshot,
    s: &Settings,
    output: String,
    out_dir: PathBuf,
) -> Result<MeasureRequest, String> {
    Ok(MeasureRequest {
        endpoint,
        snapshot,
        device: s.device.clone().ok_or("choose microphone")?,
        input_channel: s.input_channel,
        sample_rate: s.sample_rate,
        microphone_id: s.microphone_id.clone(),
        position_id: s.position_id.clone(),
        output,
        reference_output: s.reference_output.clone().ok_or("choose reference")?,
        calibration: (!s.calibration_path.trim().is_empty())
            .then(|| PathBuf::from(&s.calibration_path)),
        sweep: s.sweep,
        out_dir,
    })
}
pub fn save(a: &Alignment) -> Result<(), String> {
    let mut pending = tempfile::NamedTempFile::new_in(&a.directory).map_err(err)?;
    serde_json::to_writer_pretty(pending.as_file_mut(), a).map_err(err)?;
    pending.as_file().sync_all().map_err(err)?;
    pending
        .persist(a.directory.join("alignment-state.json"))
        .map_err(err)?;
    Ok(())
}
pub fn load(directory: &Path) -> Result<Alignment, String> {
    let mut a: Alignment = read_json(&directory.join("alignment-state.json")).map_err(err)?;
    if a.schema_version != 1 || !a.endpoint.ip().is_loopback() || a.rows.len() > 32 {
        return Err("unsupported alignment file".into());
    }
    if a.profile.is_some() {
        a.targets(false)?;
    }
    a.directory = directory.to_owned();
    Ok(a)
}
