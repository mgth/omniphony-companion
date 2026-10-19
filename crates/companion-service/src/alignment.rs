//! Repeated measurement, proposal and explicit live gain/delay application.
use crate::{
    controller::{Backend, Settings},
    files::{read_json, write_json},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use calibration_core::{
    self as core, Blocker, ExclusionReason,
    reason::Interval,
    repeatability::{QualifiedEstimate, RepeatEstimate},
};
use measurement_capture::{clipping, level};
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
    #[serde(default)]
    pub quality: Vec<crate::quality::Report>,
    pub qualified: Option<QualifiedEstimate>,
    /// Technical text of a failed qualification, kept for older readers and older
    /// sessions. Never set for a repeatable speaker, even when it is set aside.
    pub reason: Option<String>,
    /// Why this speaker receives no target: a failed qualification, or a repeatable
    /// speaker set aside by the proposal policy. Absent on sessions before reason codes;
    /// use [`Row::exclusion`], which also covers those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<ExclusionReason>,
    /// The user chose to align to this speaker although the policy sets it aside.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_anyway: bool,
}
impl Row {
    fn new(output_id: String) -> Self {
        Self {
            output_id,
            take_folders: vec![],
            repeats: vec![],
            quality: vec![],
            qualified: None,
            reason: None,
            reason_code: None,
            include_anyway: false,
        }
    }
    /// Why this speaker receives no target, typed also for sessions saved as text.
    pub fn exclusion(&self) -> Option<ExclusionReason> {
        if let Some(code) = &self.reason_code {
            return Some(code.clone());
        }
        if self.qualified.is_some() {
            return None;
        }
        Some(self.reason.as_deref().map_or(
            ExclusionReason::NotMeasuredTwice,
            ExclusionReason::from_message,
        ))
    }
    /// Repeatable and part of the corrected subset.
    pub fn included(&self) -> bool {
        self.qualified.is_some() && self.reason_code.is_none()
    }
    /// Repeatable, but left out of the correction until the user includes it.
    pub fn set_aside(&self) -> bool {
        self.qualified.is_some()
            && self
                .reason_code
                .as_ref()
                .is_some_and(ExclusionReason::is_set_aside)
    }
    fn exclude(&mut self, reason: ExclusionReason) {
        self.qualified = None;
        self.reason = Some(reason.to_string());
        self.reason_code = Some(reason);
    }
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
    /// The address the renderer was last reached at. Not an identity: actions use the
    /// connected address, and the renderer is recognised by [`Alignment::live_state`].
    pub endpoint: SocketAddr,
    pub settings: Settings,
    pub snapshot: Snapshot,
    pub rows: Vec<Row>,
    /// Absent on older sessions; progress is descriptive, never eligibility evidence.
    #[serde(default)]
    pub pass_reports: Option<Vec<PassReport>>,
    #[serde(default)]
    pub verification_pass_reports: Option<Vec<PassReport>>,
    /// Technical text of the session blockers; use [`Alignment::blocker_list`].
    pub blockers: Vec<String>,
    /// Typed `blockers`, in the same order. Empty on sessions saved before reason codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocker_codes: Vec<Blocker>,
    pub session: Option<core::Session>,
    pub profile: Option<core::CalibrationProfile>,
    pub applied_snapshot: Option<Snapshot>,
    /// The renderer in the original configuration under a newer revision: after undoing
    /// an application, or restoring the gains lowered to measure a clipped speaker again.
    pub restored_snapshot: Option<Snapshot>,
    pub verification_note: Option<String>,
    pub verification: Option<core::VerificationReport>,
    /// Set before sending any mutation. Remains true if acknowledgement/storage fails.
    pub application_uncertain: bool,
    #[serde(default)]
    pub physical_levels_changed: bool,
    /// Renderer states measured under besides `snapshot`: each retake's lowered gains and
    /// the restoration after it. Reanalysis accepts takes from these states only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retake_snapshots: Vec<Snapshot>,
}
#[derive(Debug, Clone)]
pub struct Outcome {
    pub alignment: Alignment,
    pub takes: Vec<(String, TakeOutcome)>,
    /// Clipped speakers measured again at a lower output gain; their takes replace the
    /// clipped ones in the alignment.
    pub retakes: Vec<Retake>,
}
#[derive(Debug, Clone)]
pub struct Retake {
    pub output_id: String,
    /// The renderer revision with the lowered gain, never the current one.
    pub revision: String,
    pub gain_offset_db: f64,
    pub take: TakeOutcome,
}
/// The renderer refuses output gains below this.
const MIN_OUTPUT_GAIN_DB: f32 = -60.0;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Reasons are typed where they are found. Text from the acquisition and analysis
/// crates is classified once; consequences of an earlier failure (no timing or impulse
/// after a rejected capture) are not repeated as separate causes. `gain_offset_db` is how
/// far the speaker's output gain was lowered for the take: it is added back to its level
/// and to a clipping overdrive, so both read at the speaker's original setting.
fn estimate(take: &TakeOutcome, gain_offset_db: f64) -> RepeatEstimate {
    let mut result = RepeatEstimate {
        arrival_ms: None,
        level_db: None,
        minimum_snr_db: None,
        reference_level_db: None,
        reasons: vec![],
        codes: vec![],
        output_gain_offset_db: gain_offset_db,
        minimum_snr_interval: None,
    };
    if take.cancelled {
        result.reject(ExclusionReason::Cancelled);
    } else if !take.accepted {
        let mut reason = take.rejection_reason.as_deref().map_or_else(
            || ExclusionReason::CaptureFailed {
                detail: "capture did not complete".into(),
            },
            ExclusionReason::from_message,
        );
        // The rejection text only says "clipped"; the acquisition diagnostics say by how much.
        if let ExclusionReason::Clipped { overdrive_db } = &mut reason {
            *overdrive_db = overdrive_db.or_else(|| {
                take.diagnostics
                    .as_ref()
                    .and_then(|d| d.overdrive_db())
                    .map(|db| (db + gain_offset_db).clamp(0.0, core::reason::MAX_OVERDRIVE_DB))
            });
        }
        result.reject(reason);
    }
    if let Some(t) = &take.timing {
        let snr = t
            .band_noise
            .as_ref()
            .map(|b| &b.signal_to_noise_db)
            .unwrap_or(&t.signal_to_noise_db);
        if let Err(reason) = core::repeatability::qualify_acquisition(
            *snr,
            (t.sweep_rms_dbfs[0] - t.sweep_rms_dbfs[2]).abs(),
        ) {
            result.reject(reason);
        }
    } else if !result.is_rejected() {
        result.reject(ExclusionReason::MissingTimingReferences);
    }
    let Some(ir) = &take.impulse else {
        if !result.is_rejected() {
            result.reject(take.impulse_error.as_deref().map_or_else(
                || ExclusionReason::NoClearArrival {
                    detail: "missing impulse response".into(),
                },
                ExclusionReason::from_message,
            ));
        }
        return result;
    };
    result.arrival_ms = ir.relative_arrival_ms;
    for reason in &ir.unresolved_reasons {
        result.reject(ExclusionReason::from_message(reason));
    }
    if result.arrival_ms.is_none() && !result.is_rejected() {
        result.reject(ExclusionReason::NoClearArrival {
            detail: "arrival unresolved".into(),
        });
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
            result.level_db = Some(bands[1].level_db + gain_offset_db);
            let (interval, minimum) =
                core::repeatability::weakest_interval([0, 1, 2].map(|i| bands[i].band_snr_db));
            result.minimum_snr_db = Some(minimum);
            result.minimum_snr_interval = Some(interval);
            result.reference_level_db = Some((bands[0].level_db + bands[2].level_db) * 0.5);
            if (bands[0].level_db - bands[2].level_db).abs() > 0.5 {
                result.reject(ExclusionReason::UnstableReference);
            }
        }
        Err(e) => result.reject(ExclusionReason::from_message(&e)),
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
        rows: outputs.iter().cloned().map(Row::new).collect(),
        blockers: vec![],
        blocker_codes: vec![],
        pass_reports: Some(vec![]),
        verification_pass_reports: None,
        session: None,
        profile: None,
        applied_snapshot: None,
        restored_snapshot: None,
        verification_note: None,
        verification: None,
        application_uncertain: false,
        physical_levels_changed: false,
        retake_snapshots: vec![],
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
    let mut retakes = vec![];
    // A retake leaves the renderer in the original configuration under a newer revision.
    let mut current = request.snapshot.clone();
    for pass in 0..2 {
        if cancel.load(Ordering::Relaxed) {
            alignment.block(Blocker::Cancelled);
            break;
        }
        let mut child = request.clone();
        child.snapshot = current.clone();
        child.output = outputs[0].clone();
        child.out_dir = request.out_dir.join(format!("pass-{}", pass + 1));
        child.calibration = calibration
            .as_ref()
            .map(|_| request.out_dir.join("microphone-calibration.txt"));
        let batch =
            match backend.measure_batch(child.clone(), outputs.clone(), cancel, &mut |i, p| {
                progress(pass, i, p)
            }) {
                Ok(batch) => batch,
                Err(error) => {
                    alignment.pass_reports.as_mut().unwrap().push(PassReport {
                        analyzed_speakers: 0,
                        error: Some(error.clone()),
                    });
                    alignment.block(Blocker::Capture { detail: error });
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
            .map(|detail| Blocker::Capture { detail })
            .or_else(|| batch.cancelled.then_some(Blocker::Cancelled))
            .or_else(|| incomplete.then_some(Blocker::IncompleteBatch));
        alignment.pass_reports.as_mut().unwrap().push(PassReport {
            analyzed_speakers: batch.takes.len(),
            error: pass_error.as_ref().map(ToString::to_string),
        });
        if let Some(error) = pass_error {
            alignment.block(error);
        }
        if !incomplete {
            for (row, (_, take)) in alignment.rows.iter_mut().zip(&batch.takes) {
                row.take_folders.push(take.directory.clone());
                row.repeats.push(estimate(take, 0.0));
                row.quality.push(take.quality());
            }
            if alignment.blockers.is_empty() {
                let lower_gain = alignment.settings.retake_clipped;
                retakes.extend(retake(
                    backend,
                    &child,
                    &mut alignment,
                    &mut current,
                    pass,
                    &batch.takes,
                    lower_gain,
                    cancel,
                    progress,
                ));
            }
        }
        takes.extend(batch.takes);
        if !alignment.blockers.is_empty() {
            break;
        }
    }
    if cancel.load(Ordering::Relaxed) {
        alignment.block(Blocker::Cancelled);
    }
    build_proposal(&mut alignment);
    write_json(&request.out_dir.join("alignment-plan.json"), &alignment).map_err(err)?;
    save(&alignment)?;
    Ok(Outcome {
        alignment,
        takes,
        retakes,
    })
}

/// Measure again, in the same pass, the speakers a retake can fix, and keep those takes:
/// - with `lower_gain`, a speaker whose own interval clipped by a known amount, with its
///   output gain lowered by that overdrive plus margin. Not the reference speaker, nor
///   one whose reference intervals clipped too: lowering one output cannot fix those.
/// - a speaker excluded only by a reference sweep below its signal/noise floor, while
///   another take of the pass has clean reference sweeps: a passing disturbance in the
///   room, measured again at its own gain. A reference weak on every take is left alone.
///
/// Lowered gains are restored right after the retake. A speaker whose retake fails to
/// capture keeps its first take; a completed retake replaces it, failed or not.
#[allow(clippy::too_many_arguments)]
fn retake<B: Backend + ?Sized>(
    backend: &B,
    pass_request: &MeasureRequest,
    a: &mut Alignment,
    current: &mut Snapshot,
    pass: usize,
    batch: &[(String, TakeOutcome)],
    lower_gain: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize, MeasureProgress),
) -> Vec<Retake> {
    let clean_references: Vec<_> = batch.iter().map(|(_, t)| references_pass(t)).collect();
    let mut rows = vec![];
    let mut settings = vec![];
    let mut original = vec![];
    for (index, (id, take)) in batch.iter().enumerate() {
        let Some(output) = current.outputs.iter().find(|o| &o.id == id) else {
            continue;
        };
        // The batch may be a subset of the rows (one speaker measured again).
        let Some(row) = a.rows.iter().position(|r| &r.output_id == id) else {
            continue;
        };
        let lowered = take
            .diagnostics
            .as_ref()
            .and_then(|d| d.segments.as_ref())
            .filter(|s| {
                lower_gain
                    && *id != pass_request.reference_output
                    && s[0].clipped_samples + s[2].clipped_samples == 0
            })
            .and_then(|s| s[1].overdrive_db)
            .map(|db| output.gain_db - clipping::reduction_db(db) as f32)
            .filter(|gain| *gain >= MIN_OUTPUT_GAIN_DB);
        let disturbed = a.rows[row]
            .repeats
            .get(pass)
            .is_some_and(reference_shortfall)
            && clean_references
                .iter()
                .enumerate()
                .any(|(other, clean)| other != index && *clean);
        let gain = match lowered {
            Some(gain) => gain,
            None if disturbed => output.gain_db,
            None => continue,
        };
        rows.push(row);
        settings.push(OutputSetting {
            output_id: id.clone(),
            gain_db: gain,
            delay_ms: output.delay_ms,
        });
        original.push(OutputSetting {
            output_id: id.clone(),
            gain_db: output.gain_db,
            delay_ms: output.delay_ms,
        });
    }
    if rows.is_empty() || cancel.load(Ordering::Relaxed) {
        return vec![];
    }
    let (lowering, restoring): (Vec<_>, Vec<_>) = settings
        .iter()
        .zip(&original)
        .filter(|(low, high)| low.gain_db != high.gain_db)
        .map(|(low, high)| (low.clone(), high.clone()))
        .unzip();
    let renderer = if lowering.is_empty() {
        current.clone()
    } else {
        match backend.set_outputs(a.endpoint, current.clone(), lowering.clone()) {
            Ok(renderer) => {
                a.retake_snapshots.push(renderer.clone());
                renderer
            }
            Err(error) => {
                // Refused before any change: keep the first takes and carry on.
                match backend.discover(a.endpoint) {
                    Ok(live) if omniphony_adapter::same_configuration(&live, current) => {
                        *current = live;
                    }
                    _ => a.block(Blocker::Capture {
                        detail: format!("Lowering the output gain of clipped speakers failed ({error}); check the renderer's output gains."),
                    }),
                }
                return vec![];
            }
        }
    };
    let mut expected = current.clone();
    for setting in &lowering {
        if let Some(output) = expected
            .outputs
            .iter_mut()
            .find(|o| o.id == setting.output_id)
        {
            output.gain_db = setting.gain_db;
        }
    }
    let ids: Vec<_> = settings.iter().map(|s| s.output_id.clone()).collect();
    // The offsets are the requested ones: measure only if the renderer reads them back.
    let result = omniphony_adapter::same_configuration(&renderer, &expected).then(|| {
        let mut retake = pass_request.clone();
        retake.snapshot = renderer.clone();
        retake.output = ids[0].clone();
        retake.out_dir = a.directory.join(format!("pass-{}-retake", pass + 1));
        backend.measure_batch(retake, ids.clone(), cancel, &mut |i, p| {
            progress(pass, rows[i.min(rows.len() - 1)], p)
        })
    });
    if !lowering.is_empty() {
        match backend.set_outputs(a.endpoint, renderer.clone(), restoring) {
            Ok(restored) if omniphony_adapter::same_installation(&restored, &a.snapshot) => {
                a.retake_snapshots.push(restored.clone());
                *current = restored.clone();
                a.restored_snapshot = Some(restored);
            }
            restored => {
                let lowered: Vec<_> = lowering.iter().map(|s| s.output_id.as_str()).collect();
                a.block(Blocker::Capture {
                    detail: format!(
                        "The output gain of {} was lowered for a retake and could not be restored{}; set it back before measuring again.",
                        lowered.join(", "),
                        restored.err().map(|e| format!(" ({e})")).unwrap_or_default()
                    ),
                });
                return vec![];
            }
        }
    }
    let Some(Ok(batch)) = result else {
        return vec![];
    };
    if batch.cancelled {
        a.block(Blocker::Cancelled);
    }
    let mut retakes = vec![];
    // An incomplete retake returns the takes of its first speakers, in order.
    for (((&row, low), high), (id, take)) in
        rows.iter().zip(&settings).zip(&original).zip(batch.takes)
    {
        if id != low.output_id {
            break;
        }
        let gain_offset_db = (high.gain_db - low.gain_db) as f64;
        let row = &mut a.rows[row];
        row.take_folders[pass] = take.directory.clone();
        row.repeats[pass] = estimate(&take, gain_offset_db);
        row.quality[pass] = take.quality();
        retakes.push(Retake {
            output_id: id,
            revision: renderer.revision.clone(),
            gain_offset_db,
            take,
        });
    }
    retakes
}

/// Whether a take's only shortfall is a reference sweep below a signal/noise floor.
fn reference_shortfall(r: &RepeatEstimate) -> bool {
    let reference = |interval: Option<Interval>| {
        matches!(
            interval,
            Some(Interval::ReferenceBefore | Interval::ReferenceAfter)
        )
    };
    let noisy_reference = |c: &ExclusionReason| matches!(c, ExclusionReason::LowSnr { interval, .. } if reference(*interval));
    let level = r
        .minimum_snr_db
        .is_some_and(|db| db.is_nan() || db < core::reason::SnrBand::Level.minimum_db())
        && reference(r.minimum_snr_interval);
    (level || r.codes.iter().any(noisy_reference)) && r.codes.iter().all(noisy_reference)
}

/// Whether both reference sweeps of a take meet the acquisition and level-band floors.
fn references_pass(take: &TakeOutcome) -> bool {
    use core::reason::SnrBand;
    let acquisition = take.timing.as_ref().is_some_and(|t| {
        let snr = t
            .band_noise
            .as_ref()
            .map_or(&t.signal_to_noise_db, |b| &b.signal_to_noise_db);
        [snr[0], snr[2]]
            .iter()
            .all(|db| *db >= SnrBand::Acquisition.minimum_db())
    });
    let level = take.impulse.as_ref().is_some_and(|ir| {
        [0, 2].iter().all(|&i| {
            ir.segments
                .get(i)
                .and_then(|s| s.spectral.as_ref())
                .and_then(|s| level::estimate(s).ok())
                .is_some_and(|b| b.band_snr_db >= SnrBand::Level.minimum_db())
        })
    });
    acquisition && level
}

/// How far `output_id`'s gain was lowered in the renderer state a take was captured
/// under: zero in the alignment's own installation state (whichever process or port
/// published it) or after a retake's restoration, the retake's offset in its lowered
/// state. None for a state the alignment did not record.
fn gain_offset(a: &Alignment, take: &Snapshot, output_id: &str) -> Option<f64> {
    if !omniphony_adapter::same_installation(take, &a.snapshot)
        && !a.retake_snapshots.contains(take)
    {
        return None;
    }
    let gain = |s: &Snapshot| Some(s.outputs.iter().find(|o| o.id == output_id)?.gain_db);
    Some(((gain(&a.snapshot)? - gain(take)?) as f64).max(0.0))
}

/// Measure one speaker of a saved alignment again and replace its two repeats: one take
/// per pass, in the session's reference–speaker–reference sequence, under its settings
/// and the renderer state it expects (`restored_snapshot` after a retake, else
/// `snapshot`). The other speakers' takes stay; the proposal is rebuilt and the session
/// saved. A clipped take is measured again at a lower gain as in a full alignment; a
/// disturbed reference cannot be told from a weak one with no other take in the batch.
/// Refused, with the reason, when the session is applied, uncertain or incomplete, when
/// `settings` differ from the session's, or when the renderer is not in that state. A
/// stopped or failed re-measure keeps the speaker's previous takes; the renderer states
/// it passed through for a retake are kept, so the next action still finds the renderer.
pub fn remeasure<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    output_id: &str,
    settings: &Settings,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize, MeasureProgress),
) -> Result<Outcome, String> {
    a.refreshable()?;
    let row = a
        .rows
        .iter()
        .position(|r| r.output_id == output_id)
        .ok_or("This speaker is not part of the alignment.")?;
    if a.rows[row].take_folders.len() != 2 || a.rows[row].repeats.len() != 2 {
        return Err("unexpected repeat count".into());
    }
    if *settings != a.settings {
        return Err(
            "Restore the measurement settings this alignment was captured with before measuring into it.".into(),
        );
    }
    let live = backend.discover(a.endpoint)?;
    if a.live_state(&live) != LiveState::Original {
        return Err("Omniphony's settings changed since this alignment was measured. Refresh, then measure a new alignment.".into());
    }
    let mut request = request(
        a.endpoint,
        live.clone(),
        &a.settings,
        output_id.to_owned(),
        a.directory.clone(),
    )?;
    // The session's copy of the calibration file, as its own passes used it.
    request.calibration = request
        .calibration
        .map(|_| a.directory.join("microphone-calibration.txt"));
    let mut updated = a.clone();
    let mut current = live;
    let outputs = vec![output_id.to_owned()];
    let stamp = stamp();
    let mut takes = vec![];
    let mut retakes = vec![];
    let stopped =
        || format!("Measuring {output_id} again was stopped; its previous takes are kept.");
    let mut passes = || -> Result<(), String> {
        for pass in 0..2 {
            if cancel.load(Ordering::Relaxed) {
                return Err(stopped());
            }
            let mut child = request.clone();
            child.snapshot = current.clone();
            child.out_dir = a.directory.join(format!("pass-{}-again-{stamp}", pass + 1));
            let batch =
                backend.measure_batch(child.clone(), outputs.clone(), cancel, &mut |_, p| {
                    progress(pass, row, p)
                })?;
            if batch.cancelled {
                return Err(stopped());
            }
            if let Some(error) = batch.error {
                return Err(error);
            }
            let Some((_, take)) = batch.takes.first().filter(|(id, _)| id == output_id) else {
                return Err(format!("The capture produced no take for {output_id}."));
            };
            let r = &mut updated.rows[row];
            r.take_folders[pass] = take.directory.clone();
            r.repeats[pass] = estimate(take, 0.0);
            r.quality[pass] = take.quality();
            let lower_gain = updated.settings.retake_clipped;
            retakes.extend(retake(
                backend,
                &child,
                &mut updated,
                &mut current,
                pass,
                &batch.takes,
                lower_gain,
                cancel,
                progress,
            ));
            takes.extend(batch.takes);
            // The session had no acquisition blocker; any now is this re-measure's.
            let blockers: Vec<_> = updated
                .blocker_list()
                .into_iter()
                .filter(|b| !b.is_derived())
                .collect();
            if let Some(blocker) = blockers.first() {
                return Err(if *blocker == Blocker::Cancelled {
                    stopped()
                } else {
                    blocker.to_string()
                });
            }
        }
        build_proposal(&mut updated);
        if updated.profile.is_some() {
            updated.validate_evidence()?;
        }
        save(&updated)
    };
    match passes() {
        Ok(()) => {
            *a = updated.clone();
            Ok(Outcome {
                alignment: updated,
                takes,
                retakes,
            })
        }
        Err(error) => {
            if updated.retake_snapshots.len() != a.retake_snapshots.len() {
                a.retake_snapshots = updated.retake_snapshots;
                a.restored_snapshot = updated.restored_snapshot;
                save(a)?;
            }
            Err(error)
        }
    }
}
/// Recompute membership from all repeated evidence; never drop rejected rows
/// from the saved report or let them influence the corrected subset's targets.
/// Repeatable speakers far from the group are set aside unless the user included them;
/// they keep their settings and do not move the others' targets.
fn build_proposal(a: &mut Alignment) {
    a.session = None;
    a.profile = None;
    // Blockers derived from the evidence are recomputed; acquisition blockers stay.
    let kept: Vec<_> = a
        .blocker_list()
        .into_iter()
        .filter(|b| !b.is_derived())
        .collect();
    a.blockers.clear();
    a.blocker_codes.clear();
    for blocker in kept {
        a.block(blocker);
    }
    let subset = core::repeatability::qualify_subset(
        a.rows
            .iter()
            .map(|r| (r.repeats.as_slice(), r.include_anyway)),
    );
    for ((row, result), set_aside) in a.rows.iter_mut().zip(subset.rows).zip(subset.set_aside) {
        match result {
            Ok(q) => {
                row.qualified = Some(q);
                row.reason = None;
                row.reason_code = set_aside;
            }
            Err(reason) => row.exclude(reason),
        }
    }
    if a.blockers.is_empty() {
        for blocker in subset.blockers {
            a.block(blocker);
        }
    }
    if a.blockers.is_empty() {
        match make_session(a).and_then(|session| {
            core::propose(
                &session,
                &core::CalibrationPolicy {
                    maximum_delay_ms: a.snapshot.max_delay_ms as f64,
                    ..Default::default()
                },
            )
            .map(|profile| (session, profile))
            .map_err(|e| match e {
                core::Error::Invalid(detail) => detail,
                other => other.to_string(),
            })
        }) {
            Ok((session, profile)) => {
                a.session = Some(session);
                a.profile = Some(profile);
            }
            Err(detail) => a.block(Blocker::Proposal { detail }),
        }
    }
}

/// Explicitly update an original, complete saved acquisition. Diagnostic-only
/// reanalysis and stale physical setups keep their blockers and cannot be promoted.
pub fn refresh_proposal(original: &Alignment) -> Result<Alignment, String> {
    original.refreshable()?;
    let mut updated = original.clone();
    build_proposal(&mut updated);
    if updated.profile.is_some() {
        updated.validate_evidence()?;
    }
    save(&updated)?;
    Ok(updated)
}

/// Align to a set-aside speaker anyway, or set it aside again, and update the proposal
/// from the saved passes. No playback and no renderer change; the choice is saved.
pub fn include_anyway(
    original: &Alignment,
    output_id: &str,
    include: bool,
) -> Result<Alignment, String> {
    let index = original
        .rows
        .iter()
        .position(|r| r.output_id == output_id)
        .ok_or("This speaker is not part of the alignment.")?;
    if original.rows[index].qualified.is_none() {
        return Err("Only speakers that measured reliably can be included.".into());
    }
    let mut changed = original.clone();
    changed.rows[index].include_anyway = include;
    refresh_proposal(&changed)
}

/// Revisit retained raw audio without changing original verdicts or any renderer.
/// The derived alignment is diagnostic only, even if its estimates qualify.
pub fn reanalyze(original: &Alignment) -> Result<Alignment, String> {
    if original.applied_snapshot.is_some() || original.application_uncertain {
        return Err(
            "Keep the applied alignment available for restoration; reanalyze after restoring it."
                .into(),
        );
    }
    if original.rows.len() > 32 {
        return Err("too many saved speakers".into());
    }
    let mut reviewed = original.clone();
    reviewed.directory = crate::files::parent(&crate::workspace::absolute(&original.directory))
        .join(format!("alignment-review-{}", stamp()));
    reviewed.session = None;
    reviewed.profile = None;
    reviewed.verification = None;
    reviewed.verification_note = None;
    reviewed.verification_pass_reports = None;
    reviewed.blockers.clear();
    reviewed.blocker_codes.clear();
    reviewed.block(Blocker::Diagnostic);
    for row in &mut reviewed.rows {
        row.repeats.clear();
        row.quality.clear();
        row.qualified = None;
        row.reason = None;
        row.reason_code = None;
        if row.take_folders.len() > 2 {
            return Err("unexpected repeat count".into());
        }
        for folder in &mut row.take_folders {
            *folder = crate::workspace::absolute(folder);
            let analyze = || -> Result<(crate::review::SavedReview, f64), String> {
                let snapshot: Snapshot = read_json(&folder.join("snapshot.json")).map_err(err)?;
                let gain_offset_db = gain_offset(original, &snapshot, &row.output_id)
                    .ok_or("saved take belongs to a different renderer configuration")?;
                let review = crate::review::saved_take(folder).map_err(err)?;
                if review.output_id != row.output_id
                    || review.microphone_id != original.settings.microphone_id
                    || review.position_id != original.settings.position_id
                    || review.signal != original.settings.sweep
                    || Some(&review.reference_output_id)
                        != original.settings.reference_output.as_ref()
                {
                    return Err("saved take identity differs from this alignment".into());
                }
                Ok((review, gain_offset_db))
            };
            match analyze() {
                Ok((review, gain_offset_db)) => {
                    let valid_analysis =
                        review.analysis_error.is_none() && review.capture_clipped_samples == 0;
                    let take = TakeOutcome {
                        directory: folder.clone(),
                        accepted: valid_analysis,
                        cancelled: false,
                        rejection_reason: review.analysis_error.clone().or_else(|| {
                            (review.capture_clipped_samples > 0).then(|| "clipped recording".into())
                        }),
                        diagnostics: Some(review.diagnostics),
                        timing: review.timing,
                        impulse: review.impulse,
                        impulse_error: review.impulse_error,
                    };
                    row.repeats.push(estimate(&take, gain_offset_db));
                    let mut quality = take.quality();
                    if let Some(reason) = review.original_rejection_reason {
                        quality
                            .acquisition
                            .details
                            .push(format!("Original verdict retained: {reason}"));
                    }
                    row.quality.push(quality);
                }
                Err(e) => {
                    row.quality
                        .push(crate::quality::assess(None, None, None, Some(&e)));
                    let mut repeat = RepeatEstimate {
                        arrival_ms: None,
                        level_db: None,
                        minimum_snr_db: None,
                        reference_level_db: None,
                        reasons: vec![],
                        codes: vec![],
                        output_gain_offset_db: 0.0,
                        minimum_snr_interval: None,
                    };
                    repeat.reject(ExclusionReason::from_message(&e));
                    row.repeats.push(repeat);
                }
            }
        }
        match core::repeatability::qualify(&row.repeats) {
            Ok(q) => row.qualified = Some(q),
            Err(reason) => row.exclude(reason),
        }
    }
    // Show which repeatable speakers the policy would set aside, for diagnosis only.
    let arrivals: Vec<_> = reviewed
        .rows
        .iter()
        .map(|r| r.qualified.as_ref().map(|q| q.arrival_ms))
        .collect();
    let include: Vec<_> = reviewed.rows.iter().map(|r| r.include_anyway).collect();
    for (row, set_aside) in
        reviewed
            .rows
            .iter_mut()
            .zip(core::repeatability::set_aside_far_arrivals(
                &arrivals, &include,
            ))
    {
        if row.qualified.is_some() {
            row.reason_code = set_aside;
        }
    }
    std::fs::create_dir(&reviewed.directory).map_err(err)?;
    write_json(&reviewed.directory.join("alignment-plan.json"), &reviewed).map_err(err)?;
    save(&reviewed)?;
    Ok(reviewed)
}

fn make_session(a: &Alignment) -> Result<core::Session, String> {
    let mut speakers = vec![];
    let mut observations = vec![];
    for row in a.rows.iter().filter(|r| r.included()) {
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
    pub fn corrected_output_ids(&self) -> Vec<String> {
        self.profile
            .as_ref()
            .map(|p| {
                p.target_snapshot
                    .speakers
                    .iter()
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Speakers that measured repeatably, whether included or set aside.
    pub fn qualified_count(&self) -> usize {
        self.rows.iter().filter(|r| r.qualified.is_some()).count()
    }
    /// Repeatable speakers that are part of the corrected subset.
    pub fn included_count(&self) -> usize {
        self.rows.iter().filter(|r| r.included()).count()
    }
    pub fn set_aside_count(&self) -> usize {
        self.rows.iter().filter(|r| r.set_aside()).count()
    }
    /// Record a blocker once, as text for older readers and as a code.
    pub fn block(&mut self, blocker: Blocker) {
        let text = blocker.to_string();
        if !self.blockers.contains(&text) {
            self.blockers.push(text);
            self.blocker_codes.push(blocker);
        }
    }
    /// The session blockers, typed also for sessions saved as text (as `Other`).
    pub fn blocker_list(&self) -> Vec<Blocker> {
        self.blockers
            .iter()
            .enumerate()
            .map(|(i, text)| match self.blocker_codes.get(i) {
                Some(code) if code.to_string() == *text => code.clone(),
                _ => Blocker::Other {
                    detail: core::reason::without_error_prefix(text).to_owned(),
                },
            })
            .collect()
    }
    /// Whether the proposal may be recomputed from the saved passes (an explicit update or
    /// an "include anyway" choice). The error says why not.
    pub fn refreshable(&self) -> Result<(), String> {
        if self.applied_snapshot.is_some()
            || self.application_uncertain
            || self.physical_levels_changed
        {
            return Err("Restore any active alignment and capture fresh evidence after physical adjustments before updating a proposal.".into());
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.baseline.origin != core::ObservationOrigin::Measured)
            || self
                .profile
                .as_ref()
                .is_some_and(|p| p.origin != core::ObservationOrigin::Measured)
        {
            return Err(
                "Simulated observations cannot become a measured correction proposal.".into(),
            );
        }
        let acquisition: Vec<_> = self
            .blocker_list()
            .into_iter()
            .filter(|b| !b.is_derived())
            .map(|b| b.to_string())
            .collect();
        if !acquisition.is_empty() {
            return Err(format!("Proposal blocked: {}", acquisition.join("; ")));
        }
        if !self.pass_reports.as_ref().is_some_and(|p| {
            p.len() == 2
                && p.iter()
                    .all(|pass| pass.error.is_none() && pass.analyzed_speakers == self.rows.len())
        }) {
            return Err(
                "Two complete original acquisition passes are required; capture a new alignment."
                    .into(),
            );
        }
        Ok(())
    }
    /// Sessions saved before far arrivals were set aside may already correct such a
    /// speaker. Its proposal stays valid and restorable: a far speaker that the saved
    /// profile corrects counts as included anyway.
    fn include_corrected_far_speakers(&mut self) {
        let Some(profile) = &self.profile else {
            return;
        };
        let arrivals: Vec<_> = self
            .rows
            .iter()
            .map(|r| r.qualified.as_ref().map(|q| q.arrival_ms))
            .collect();
        let include: Vec<_> = self.rows.iter().map(|r| r.include_anyway).collect();
        let set_aside = core::repeatability::set_aside_far_arrivals(&arrivals, &include);
        for (row, set_aside) in self.rows.iter_mut().zip(set_aside) {
            if set_aside.is_some()
                && row.reason_code.is_none()
                && profile
                    .target_snapshot
                    .speakers
                    .iter()
                    .any(|s| s.id == row.output_id)
            {
                row.include_anyway = true;
            }
        }
    }
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
        let qualified = core::repeatability::qualify_subset(
            self.rows
                .iter()
                .map(|r| (r.repeats.as_slice(), r.include_anyway)),
        );
        if !qualified.blockers.is_empty() {
            return Err(qualified
                .blockers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "));
        }
        let mut ids = std::collections::HashSet::new();
        for ((row, qualification), set_aside) in self
            .rows
            .iter()
            .zip(qualified.rows)
            .zip(qualified.set_aside)
        {
            if !ids.insert(&row.output_id)
                || !self.snapshot.outputs.iter().any(|o| o.id == row.output_id)
            {
                return Err("Alignment contains duplicate or unknown speaker IDs.".into());
            }
            // Membership and estimates are evidence; an exclusion's wording is not.
            match qualification {
                Ok(q)
                    if row.qualified.as_ref() == Some(&q)
                        && row.reason.is_none()
                        && row.reason_code == set_aside
                        && row.take_folders.len() == 2
                        && row.take_folders[0] != row.take_folders[1] => {}
                Err(_) if row.qualified.is_none() => {}
                _ => {
                    return Err(
                        "Alignment repeat evidence or subset membership is inconsistent.".into(),
                    );
                }
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

/// Where the connected renderer stands for this alignment. A renderer is recognised by
/// its installation and settings ([`omniphony_adapter::same_installation`]), never by
/// the address it is reached at or the identity of its process, so a session survives a
/// new port or a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    /// The installation the alignment measured, with its original gains and delays.
    Original,
    /// The same installation with the proposal's gains and delays.
    Applied,
    /// Another installation, or settings changed since: nothing to apply or undo here.
    Other,
}
impl Alignment {
    pub fn live_state(&self, live: &Snapshot) -> LiveState {
        let applied = self
            .profile
            .as_ref()
            .and_then(|_| self.expected_target().ok())
            .is_some_and(|target| omniphony_adapter::same_installation(live, &target));
        let original = omniphony_adapter::same_installation(live, &self.snapshot);
        match (applied, original) {
            // A proposal that changes nothing reads as both; the saved state decides.
            (true, true) if self.applied_snapshot.is_some() => LiveState::Applied,
            (_, true) => LiveState::Original,
            (true, false) => LiveState::Applied,
            (false, false) => LiveState::Other,
        }
    }
}

/// Record where the connected renderer stands: applied when it has the proposal's
/// settings, not applied when it has the original ones, whichever process or port
/// carries them. Saves the session when that changes what it records. A renderer with
/// other settings, or an uncertain application, leaves the session as it is.
pub fn reconcile(a: &mut Alignment, live: &Snapshot) -> Result<bool, String> {
    if a.application_uncertain {
        return Ok(false);
    }
    let changed = match a.live_state(live) {
        LiveState::Applied if a.applied_snapshot.as_ref() != Some(live) => {
            a.applied_snapshot = Some(live.clone());
            true
        }
        LiveState::Original if a.applied_snapshot.is_some() => {
            // The corrections are not in this renderer: what was verified no longer is.
            a.applied_snapshot = None;
            a.restored_snapshot = (live != &a.snapshot).then(|| live.clone());
            a.verification = None;
            a.verification_pass_reports = None;
            a.verification_note = None;
            true
        }
        LiveState::Original
            if live != &a.snapshot && a.restored_snapshot.as_ref() != Some(live) =>
        {
            a.restored_snapshot = Some(live.clone());
            true
        }
        _ => false,
    };
    if changed {
        save(a)?;
    }
    Ok(changed)
}

pub fn apply<B: Backend + ?Sized>(
    backend: &B,
    alignment: &mut Alignment,
    restore: bool,
) -> Result<Snapshot, String> {
    if !restore && alignment.physical_levels_changed {
        return Err(
            "Physical speaker levels may have changed. Measure a new alignment before applying."
                .into(),
        );
    }

    if alignment.application_uncertain {
        return Err("Previous application is uncertain. Inspect the renderer and saved original settings before further changes.".into());
    }
    let targets = alignment.targets(restore)?;
    if let Some(restored) = &alignment.restored_snapshot
        && !omniphony_adapter::same_installation(restored, &alignment.snapshot)
    {
        return Err("Saved restoration state differs from the original settings.".into());
    }
    if let Some(applied) = &alignment.applied_snapshot
        && !omniphony_adapter::same_installation(applied, &alignment.expected_target()?)
    {
        return Err("Saved application state differs from the proposal.".into());
    }
    let current = backend.discover(alignment.endpoint)?;
    // Already where it was asked to go: record it, write nothing.
    match (alignment.live_state(&current), restore) {
        (LiveState::Other, _) => {
            return Err("Omniphony's settings differ from both the original and the corrected settings of this alignment. No settings were changed; measure a new alignment.".into());
        }
        (LiveState::Original, true) | (LiveState::Applied, false) => {
            reconcile(alignment, &current)?;
            return Ok(current);
        }
        _ => {}
    }
    alignment.application_uncertain = true;
    save(alignment)?;
    let result = backend.set_outputs(alignment.endpoint, current, targets)?;
    let target = if restore {
        alignment.snapshot.clone()
    } else {
        alignment.expected_target()?
    };
    if !omniphony_adapter::same_installation(&result, &target) {
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
    if a.physical_levels_changed {
        return Err(
            "Physical speaker levels may have changed. Measure a new alignment before verifying."
                .into(),
        );
    }

    if a.application_uncertain {
        return Err("Application state is uncertain.".into());
    }
    a.targets(false)?;
    a.verification = None;
    a.verification_pass_reports = Some(vec![]);
    a.verification_note = Some("Verification has not completed.".into());
    save(a)?;
    if a.applied_snapshot.is_none() {
        return Err("Apply the proposal before verification.".into());
    }
    let snapshot = backend.discover(a.endpoint)?;
    if a.live_state(&snapshot) != LiveState::Applied {
        return Err("Renderer changed after application; verification refused.".into());
    }
    a.applied_snapshot = Some(snapshot.clone());
    let outputs = a.corrected_output_ids();
    let request = request(
        a.endpoint,
        snapshot,
        &a.settings,
        outputs[0].clone(),
        a.directory.join(format!("verification-{}", stamp())),
    )?;
    // Verification measures the applied state as it is: no gain is lowered for a retake.
    let outcome = capture(
        backend,
        request,
        Settings {
            retake_clipped: false,
            ..a.settings.clone()
        },
        outputs,
        cancel,
        progress,
    )?;
    a.verification_pass_reports = outcome.alignment.pass_reports.clone();
    let measured = outcome.alignment.session.as_ref();
    if outcome.alignment.corrected_output_ids() != a.corrected_output_ids() || measured.is_none() {
        let mut reasons = outcome.alignment.blockers.clone();
        reasons.extend(outcome.alignment.rows.iter().filter_map(|r| {
            r.exclusion()
                .map(|reason| format!("{}: {reason}", r.output_id))
        }));
        a.verification_note = Some(format!(
            "Verification incomplete for the corrected subset: {}. Original targets are unchanged. Review {}",
            reasons.join("; "),
            outcome.alignment.directory.display()
        ));
        save(a)?;
        return Ok(outcome);
    }
    let measured = measured.ok_or("missing verification evidence")?;
    let session = a.session.as_ref().ok_or("missing baseline")?;
    let profile = a.profile.as_ref().ok_or("missing profile")?;
    if !omniphony_adapter::same_installation(&outcome.alignment.snapshot, &a.expected_target()?) {
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
    a.include_corrected_far_speakers();
    if a.profile.is_some() {
        a.targets(false)?;
    }
    a.directory = directory.to_owned();
    Ok(a)
}

/// Preserve restoration data while preventing stale proposals after knob changes.
pub fn invalidate_physical_levels(alignment: &mut Alignment) -> Result<(), String> {
    if !alignment.physical_levels_changed {
        alignment.physical_levels_changed = true;
        alignment.verification = None;
        alignment.verification_note =
            Some("Physical speaker levels may have changed. Measure a new alignment.".into());
        save(alignment)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_are_accepted_only_from_the_renderer_states_the_alignment_recorded() {
        let a: Alignment = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "endpoint": "127.0.0.1:9000",
            "settings": {},
            "snapshot": {
                "instance_id": "renderer", "revision": "r1", "sample_rate": 48000,
                "master_gain_db": 0.0, "max_delay_ms": 100.0, "unavailable_reason": null,
                "outputs": [
                    {"id": "L", "index": 0, "name": "L", "gain_db": 0.0, "delay_ms": 0.0,
                     "muted": false, "freq_low_hz": null, "freq_high_hz": null},
                    {"id": "R", "index": 1, "name": "R", "gain_db": -3.0, "delay_ms": 0.0,
                     "muted": false, "freq_low_hz": null, "freq_high_hz": null}
                ]
            },
            "rows": [], "blockers": [], "session": null, "profile": null,
            "applied_snapshot": null, "restored_snapshot": null,
            "verification_note": null, "verification": null,
            "application_uncertain": false
        }))
        .unwrap();
        let state = |revision: &str, gain: f32| {
            let mut s = a.snapshot.clone();
            s.revision = revision.into();
            s.outputs[1].gain_db = gain;
            s
        };
        let mut a = a.clone();
        assert_eq!(gain_offset(&a, &a.snapshot, "R"), Some(0.0));
        // The same installation state under a new revision (a restart) is the same.
        assert_eq!(gain_offset(&a, &state("r2", -3.0), "R"), Some(0.0));
        assert_eq!(gain_offset(&a, &state("r2", -16.0), "R"), None);
        a.retake_snapshots = vec![state("r2", -16.0), state("r3", -3.0)];
        assert_eq!(gain_offset(&a, &state("r2", -16.0), "R"), Some(13.0));
        assert_eq!(gain_offset(&a, &state("r2", -16.0), "L"), Some(0.0));
        assert_eq!(gain_offset(&a, &state("r3", -3.0), "R"), Some(0.0));
        assert_eq!(gain_offset(&a, &state("r4", -16.0), "R"), None);
        assert_eq!(gain_offset(&a, &a.snapshot, "Unknown"), None);
    }
}
