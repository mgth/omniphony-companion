//! Integrating the subwoofer with the speakers around their crossover.
//!
//! The alignment sets each speaker's delay and level from its arrival above 300 Hz; a
//! subwoofer gets neither, since its sweep cannot show a broadband arrival. Here every
//! speaker the subwoofer may serve (a declared low limit, `freq_low_hz`, so that bass
//! management sends its low band elsewhere) plays the renderer's bass-managed sweep
//! (protocol 8): a source at the speaker's position, split by the crossover and panned as
//! programme audio is. The renderer only accepts direct sweeps inside each output's
//! declared band, so the speaker alone and the subwoofer alone are not swept directly
//! across their crossover; they are measured through the same bass-managed sweep with the
//! other part silenced (its output at −60 dB for the sequence, put back right after):
//!
//! - **sum**: everything as it is;
//! - **speaker**: the subwoofer silenced, leaving the speaker's part;
//! - **subwoofer**: every output silenced but the subwoofer and the timing reference,
//!   leaving the subwoofer's part (the reference's own part is the sum minus its
//!   speaker part);
//! - **direct**: the subwoofer's own sweep, for its mode correction.
//!
//! Every session holds the room correction filters off, so an existing correction does
//! not colour the parts. The reference speaker's direct sweeps around each target give
//! all of a position's takes one time base ([`room_response::StoredResponse::target_offset_frames`]),
//! checked by lining up their reference sweeps.
//!
//! The subwoofer is timed against the speakers' delays as Omniphony has them: apply the
//! alignment first, as a later Apply or Undo of it makes these measurements stale. The
//! main seat comes first, then optionally the guided
//! positions of [`crate::positions::GUIDE`]. Each capture is analysed by
//! [`calibration_core::bass`] together with the subwoofer's mode correction
//! ([`calibration_core::room_correction::design_subwoofer`]); Apply sends the
//! subwoofer's delay and level (and, when it would need less than no delay, a delay for
//! every other output) with `set_outputs`, and its filter (mode correction and polarity)
//! with `set_output_filters`: live only, like every Apply, until Omniphony's Save. Undo
//! puts them back; Confirm measures the sum again and compares it with the prediction.
use crate::{
    alignment::{self, Alignment, LiveState},
    controller::{Backend, OutputFilter},
    files::write_json,
    level_probe,
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
    positions::{self, GUIDE},
    room_response,
    sweep_plan::{self, BROADBAND_END_HZ, Group},
};
use calibration_core::{
    bass::{self as core, Proposal},
    room_correction::{self as design, OutputDesign, OutputMeasurements, SubwooferSettings},
};
use omniphony_adapter::{
    Snapshot, Sweep,
    protocol::{self, Output},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// An output silenced for a part is set to this, the lowest gain the renderer accepts.
pub const SILENT_DB: f32 = -60.0;
pub const SUMMARY: &str = "bass-integration.json";
/// The bass-managed sweep ends at four times the highest crossover, within these.
const SWEEP_END_HZ: [f32; 2] = [300.0, 1000.0];
/// How long Apply waits for the renderer to load a filter.
const FILTER_LOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Reference sweeps of a position's takes must line up within this.
const MAX_REFERENCE_LAG_MS: f64 = 5.0;

/// What a sequence of the capture measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// Bass-managed, everything playing.
    Sum,
    /// The subwoofer's own sweep.
    Direct,
    /// Bass-managed, the subwoofer silenced.
    Speaker,
    /// Bass-managed, every other output silenced but the timing reference.
    Subwoofer,
}
impl Part {
    pub const ALL: [Part; 4] = [Part::Sum, Part::Direct, Part::Speaker, Part::Subwoofer];
    fn folder(self) -> &'static str {
        match self {
            Part::Sum => "sum",
            Part::Direct => "subwoofer-direct",
            Part::Speaker => "speakers-without-subwoofer",
            Part::Subwoofer => "subwoofer-without-speakers",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Part::Sum => "together",
            Part::Direct => "subwoofer's own sweep",
            Part::Speaker => "subwoofer silenced",
            Part::Subwoofer => "speakers silenced",
        }
    }
}

/// Who plays what.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub subwoofer: String,
    /// The bass-managed sequences: each speaker's sweep, split at its `freq_low_hz`,
    /// framed by its group's reference. The reference must play the whole sweep, so a
    /// speaker split lower than any other speaker can be swept is framed by the
    /// subwoofer itself; silencing the subwoofer would silence that reference, so those
    /// speakers' own parts are the sum minus the subwoofer's.
    pub groups: Vec<Group>,
    /// Speakers with a low limit left out, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<(String, String)>,
    /// The subwoofer's own sweep, with its reference.
    pub direct: Group,
}
impl Plan {
    /// Every speaker measured, group by group.
    pub fn speakers(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(|g| g.outputs.iter().cloned())
            .collect()
    }
    pub fn group_of(&self, output_id: &str) -> Option<&Group> {
        sweep_plan::group_of(&self.groups, output_id)
    }
    /// The parts that measure `output_id`: not the subwoofer silenced when it is the
    /// reference, not the speakers silenced when the speaker is.
    pub fn parts_of(&self, output_id: &str) -> Vec<Part> {
        let Some(group) = self.group_of(output_id) else {
            return vec![];
        };
        let mut parts = vec![Part::Sum];
        if group.reference != self.subwoofer {
            parts.push(Part::Speaker);
        }
        if group.reference != output_id {
            parts.push(Part::Subwoofer);
        }
        parts
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BassTake {
    pub output_id: String,
    pub part: Part,
    pub take_folder: PathBuf,
    pub accepted: bool,
    pub rejection_reason: Option<String>,
    /// Whether the take stored its response.
    pub response: bool,
}
impl BassTake {
    pub fn usable(&self) -> bool {
        self.accepted && self.response
    }
}

/// The takes of one microphone position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    /// The guide step's id; `None` at the main seat.
    pub step: Option<String>,
    pub position_id: String,
    pub directory: PathBuf,
    pub takes: Vec<BassTake>,
    /// Why it stopped before every part was measured.
    pub error: Option<String>,
}
impl Capture {
    fn take(&self, output_id: &str, part: Part) -> Option<&BassTake> {
        self.takes
            .iter()
            .find(|t| t.output_id == output_id && t.part == part)
    }
    /// Every part of every speaker, and the subwoofer's own sweep, usable.
    pub fn complete(&self, plan: &Plan) -> bool {
        self.error.is_none()
            && self
                .take(&plan.subwoofer, Part::Direct)
                .is_some_and(BassTake::usable)
            && plan.speakers().iter().all(|id| {
                plan.parts_of(id)
                    .into_iter()
                    .all(|part| self.take(id, part).is_some_and(BassTake::usable))
            })
    }
}

/// The taps Apply sends to the subwoofer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterFile {
    pub file: PathBuf,
    pub sample_rate: u32,
    pub taps: usize,
    /// [`protocol::taps_hash_hex`]: what the renderer reports once it plays them.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub directory: PathBuf,
    pub proposal: Proposal,
    /// The subwoofer's mode correction, without its taps (they are in `filter`'s file).
    pub mode_correction: Option<OutputDesign>,
    /// The mode correction with the proposed polarity; absent when the subwoofer needs
    /// neither.
    pub filter: Option<FilterFile>,
    /// Takes left out and why.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BassIntegration {
    pub plan: Plan,
    /// The renderer state every capture was measured under: the alignment applied.
    pub snapshot: Snapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_probe: Option<level_probe::Probe>,
    pub captures: Vec<Capture>,
    pub analysis: Option<Analysis>,
    pub analysis_error: Option<String>,
    /// The renderer read back after Apply.
    pub applied: Option<Snapshot>,
    pub verification: Option<core::Verification>,
    pub verification_note: Option<String>,
}
impl BassIntegration {
    /// The capture at the main seat (`None`) or at a guide step.
    pub fn capture(&self, step: Option<&str>) -> Option<&Capture> {
        self.captures.iter().find(|c| c.step.as_deref() == step)
    }
    /// The renderer with the proposal applied on top of the measured state.
    pub fn target(&self) -> Option<Snapshot> {
        let analysis = self.analysis.as_ref()?;
        let proposal = &analysis.proposal;
        if proposal.refusal.is_some() {
            return None;
        }
        let mut target = self.snapshot.clone();
        for output in &mut target.outputs {
            if output.id == self.plan.subwoofer {
                output.gain_db = proposal.subwoofer_gain_db as f32;
                output.delay_ms = proposal.subwoofer_delay_ms as f32;
            } else {
                output.delay_ms += proposal.others_delay_ms as f32;
            }
        }
        Some(target)
    }
    fn subwoofer_hash<'a>(&self, snapshot: &'a Snapshot) -> Option<&'a str> {
        snapshot
            .outputs
            .iter()
            .find(|o| o.id == self.plan.subwoofer)
            .and_then(|o| o.filter.as_ref())
            .map(|f| f.hash.as_deref().unwrap_or("unknown"))
    }
    /// Where the renderer stands: as measured, with the proposal, or neither.
    pub fn live_state(&self, live: &Snapshot) -> LiveState {
        // Bass-managed sweeps pan by the layout's positions: a moved speaker changes the
        // parts that were measured.
        if !same_layout(live, &self.snapshot) {
            return LiveState::Other;
        }
        let original = alignment::same_live_settings(live, &self.snapshot)
            && self.subwoofer_hash(live) == self.subwoofer_hash(&self.snapshot);
        let applied = self.target().is_some_and(|target| {
            alignment::same_live_settings(live, &target)
                && self.subwoofer_hash(live)
                    == self
                        .analysis
                        .as_ref()
                        .and_then(|a| a.filter.as_ref())
                        .map(|f| f.hash.as_str())
        });
        match (applied, original) {
            (true, true) if self.applied.is_some() => LiveState::Applied,
            (_, true) => LiveState::Original,
            (true, false) => LiveState::Applied,
            (false, false) => LiveState::Other,
        }
    }
    /// Whether its proposal is applied in the renderer, as last recorded.
    pub fn is_applied(&self) -> bool {
        self.applied.is_some()
    }
}

/// Whether two snapshots place every output where the other does, as the panning of a
/// bass-managed sweep sees it (positions within a thousandth of a degree or metre).
fn same_layout(a: &Snapshot, b: &Snapshot) -> bool {
    let close = |x: Option<f32>, y: Option<f32>| match (x, y) {
        (Some(x), Some(y)) => (x - y).abs() <= 1e-3,
        (None, None) => true,
        _ => false,
    };
    a.outputs.len() == b.outputs.len()
        && a.outputs.iter().zip(&b.outputs).all(|(x, y)| {
            x.id == y.id
                && close(x.azimuth_deg, y.azimuth_deg)
                && close(x.elevation_deg, y.elevation_deg)
                && close(x.distance_m, y.distance_m)
        })
}

/// The renderer's subwoofer: the one unmuted output whose range stops below 5 kHz.
pub fn subwoofer(snapshot: &Snapshot) -> Result<&Output, String> {
    let subwoofers: Vec<&Output> = snapshot
        .outputs
        .iter()
        .filter(|o| !o.muted && o.freq_high_hz.is_some_and(|h| h < BROADBAND_END_HZ))
        .collect();
    match subwoofers.as_slice() {
        [] => {
            Err("Omniphony has no subwoofer: no unmuted output's range stops below 5 kHz.".into())
        }
        [one] => Ok(one),
        many => Err(format!(
            "Several subwoofers ({}): integrating more than one is not supported yet.",
            many.iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The highest peak a bass-managed sweep may have in `snapshot`: the renderer holds it
/// under the ceiling on the loudest unmuted output.
fn bass_ceiling(snapshot: &Snapshot) -> f32 {
    let loudest = snapshot
        .outputs
        .iter()
        .filter(|o| !o.muted)
        .map(|o| o.gain_db)
        .fold(f32::NEG_INFINITY, f32::max);
    (-6.0 - snapshot.master_gain_db - loudest).min(sweep_plan::MAX_PEAK_DBFS)
}

/// Who plays what for `a`'s speakers in `live`, at the manual level until a probe sets it.
pub fn plan(a: &Alignment, live: &Snapshot) -> Result<Plan, String> {
    let sub = subwoofer(live)?;
    let rate = live.sample_rate.min(a.settings.sample_rate) as f32 * 0.45;
    let candidates: Vec<&Output> = live
        .outputs
        .iter()
        .filter(|o| {
            o.id != sub.id
                && !o.muted
                && a.rows.iter().any(|r| r.output_id == o.id)
                && o.freq_low_hz.is_some_and(|f| f > 20.0)
        })
        .collect();
    if candidates.is_empty() {
        return Err("No speaker of the alignment has a low limit (freq_low): bass management sends none of their bass to the subwoofer.".into());
    }
    let highest = candidates
        .iter()
        .filter_map(|o| o.freq_low_hz)
        .fold(0.0, f32::max);
    // The main reference plays the direct sweeps: the speaker reaching lowest, the
    // alignment's own when it reaches as low.
    let end_wanted = (4.0 * highest)
        .clamp(SWEEP_END_HZ[0], SWEEP_END_HZ[1])
        .min(rate);
    let low = |o: &Output| o.freq_low_hz.unwrap_or(20.0).max(20.0);
    let reference = live
        .outputs
        .iter()
        .filter(|o| o.id != sub.id && !o.muted && o.freq_high_hz.is_none_or(|h| h >= end_wanted))
        .min_by(|x, y| {
            low(x).total_cmp(&low(y)).then_with(|| {
                let own = |o: &Output| a.settings.reference_output.as_deref() != Some(&o.id);
                own(x).cmp(&own(y))
            })
        });
    let own = sweep_plan::range(sub, live.sample_rate, a.settings.sample_rate);
    // Each speaker needs the octave around its crossover, and a little past it.
    let fits = |fc: f32, [start, end]: [f32; 2]| {
        fc / std::f32::consts::SQRT_2 >= start * 1.03 && fc * std::f32::consts::SQRT_2 <= end / 1.03
    };
    let main_range =
        reference.map(|r| [low(r), end_wanted.min(r.freq_high_hz.unwrap_or(f32::MAX))]);
    let mut main = vec![];
    let mut by_subwoofer = vec![];
    let mut skipped = vec![];
    for o in candidates {
        let fc = o.freq_low_hz.unwrap_or(20.0);
        if main_range.is_some_and(|range| fits(fc, range)) {
            main.push(o);
        } else if fits(fc, own) {
            by_subwoofer.push(o);
        } else {
            skipped.push((
                o.id.clone(),
                format!(
                    "Its {fc:.0} Hz crossover cannot be swept an octave wide: {} and {} ({:.0}–{:.0} Hz) cannot frame it.",
                    reference.map_or("no other speaker", |r| r.name.as_str()),
                    sub.name,
                    own[0],
                    own[1]
                ),
            ));
        }
    }
    let group = |reference: &Output, members: &[&Output], [start, end]: [f32; 2]| Group {
        sweep: Sweep {
            duration_ms: sweep_plan::ROOM_SWEEP_MS,
            peak_dbfs: a.settings.sweep.peak_dbfs,
            start_hz: start,
            end_hz: members
                .iter()
                .filter_map(|o| o.freq_high_hz)
                .fold(end, f32::min),
        },
        reference: reference.id.clone(),
        outputs: members.iter().map(|o| o.id.clone()).collect(),
    };
    let mut groups = vec![];
    if let (Some(reference), Some(range)) = (reference, main_range)
        && !main.is_empty()
    {
        groups.push(group(reference, &main, range));
    }
    if !by_subwoofer.is_empty() {
        groups.push(group(sub, &by_subwoofer, own));
    }
    if groups.is_empty() {
        return Err(
            "No speaker's crossover can be swept an octave wide with the speakers and subwoofer available as timing references."
                .into(),
        );
    }
    let direct_reference = match reference {
        Some(r) if low(r) <= own[0] && r.freq_high_hz.is_none_or(|h| h >= own[1]) => r.id.clone(),
        _ => sub.id.clone(),
    };
    let direct = Group {
        sweep: Sweep {
            duration_ms: sweep_plan::ROOM_SWEEP_MS,
            peak_dbfs: a.settings.sweep.peak_dbfs,
            start_hz: own[0],
            end_hz: own[1],
        },
        reference: direct_reference,
        outputs: vec![sub.id.clone()],
    };
    Ok(Plan {
        subwoofer: sub.id.clone(),
        groups,
        skipped,
        direct,
    })
}

/// `measured` with the outputs `part` silences at [`SILENT_DB`], for a group framed by
/// `reference`.
fn state(measured: &Snapshot, plan: &Plan, part: Part, reference: &str) -> Snapshot {
    let mut s = measured.clone();
    for output in &mut s.outputs {
        let silent = match part {
            Part::Sum | Part::Direct => false,
            Part::Speaker => output.id == plan.subwoofer,
            Part::Subwoofer => output.id != plan.subwoofer && output.id != reference,
        };
        if silent {
            output.gain_db = SILENT_DB;
        }
    }
    s
}

/// What one part plays of `group`: its targets (not the reference when the speakers
/// are silenced), under the bass-managed ceiling of `measured`, the state every part
/// starts from, so each part plays it at one level. `None` when nothing is left.
fn part_group(group: &Group, part: Part, measured: &Snapshot, capture_rate: u32) -> Option<Group> {
    if part == Part::Direct {
        // A direct sweep stays under each of its outputs' own ceilings.
        let mut group = group.clone();
        group.sweep.peak_dbfs = group
            .outputs
            .iter()
            .chain(std::iter::once(&group.reference))
            .filter_map(|id| measured.outputs.iter().find(|o| &o.id == id))
            .map(|o| sweep_plan::ceiling(measured, o))
            .fold(group.sweep.peak_dbfs, f32::min);
        return Some(group);
    }
    let outputs: Vec<String> = group
        .outputs
        .iter()
        .filter(|id| !(part == Part::Subwoofer && **id == group.reference))
        .cloned()
        .collect();
    if outputs.is_empty() {
        return None;
    }
    let mut sweep = group.sweep;
    sweep.peak_dbfs = sweep.peak_dbfs.min(bass_ceiling(measured));
    sweep.duration_ms = fitting(
        sweep,
        measured.sample_rate,
        capture_rate,
        group.outputs.len(),
    );
    Some(Group {
        sweep,
        reference: group.reference.clone(),
        outputs,
    })
}

/// The longest sweep up to 3 s, in 100 ms steps, whose sequence of `targets` fits.
fn fitting(sweep: Sweep, render_rate: u32, capture_rate: u32, targets: usize) -> u32 {
    (sweep_plan::MIN_ROOM_SWEEP_MS..=sweep.duration_ms.max(sweep_plan::MIN_ROOM_SWEEP_MS))
        .rev()
        .step_by(100)
        .find(|&duration_ms| {
            let sweep = Sweep {
                duration_ms,
                ..sweep
            };
            protocol::sequence_frames(sweep, render_rate, targets).is_ok()
                && measurement_capture::sequence::capture_budget(
                    sweep,
                    render_rate,
                    capture_rate,
                    targets,
                )
                .is_ok()
        })
        .unwrap_or(sweep_plan::MIN_ROOM_SWEEP_MS)
}

/// Why the subwoofer cannot be measured at `step` (`None`: the main seat) now.
pub fn readiness(a: &Alignment, live: &Snapshot, step: Option<usize>) -> Result<(), String> {
    if a.application_uncertain {
        return Err(
            "It is not known whether the last Apply reached Omniphony. Refresh first.".into(),
        );
    }
    if a.physical_levels_changed {
        return Err(
            "Speaker volumes may have been turned since the alignment. Measure a new alignment first."
                .into(),
        );
    }
    if let Some(bass) = &a.bass
        && bass.is_applied()
    {
        return Err(
            "Undo the subwoofer integration first: the subwoofer is measured as it was.".into(),
        );
    }
    // Timed against the speakers' delays as they are: the alignment's, applied or not.
    if a.live_state(live) == LiveState::Other {
        return Err("Omniphony's settings differ from both the original and the corrected settings of this alignment. Measure a new alignment first.".into());
    }
    let sub = subwoofer(live)?;
    if let Some(filter) = &sub.filter {
        return Err(format!(
            "{} already plays a correction filter ({}): remove it in Studio first, so that Undo can put the subwoofer back exactly.",
            sub.name,
            filter.file.as_deref().unwrap_or("not saved")
        ));
    }
    if let Some(step) = step {
        let guide = GUIDE.get(step).ok_or("unknown microphone position")?;
        let bass = a
            .bass
            .as_ref()
            .filter(|b| b.capture(None).is_some())
            .ok_or("Measure the subwoofer at the main seat first.")?;
        if bass.live_state(live) != LiveState::Original {
            return Err(format!(
                "Omniphony's settings changed since the subwoofer was measured at the main seat; measure it there again before {}.",
                guide.label
            ));
        }
    }
    Ok(())
}

/// Measure every part at `step` (`None`: the main seat, which starts over) and analyse
/// everything measured so far. `progress` receives the index of the speaker in the plan.
pub fn measure<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    step: Option<usize>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, MeasureProgress),
) -> Result<(), String> {
    let live = backend.discover(a.endpoint)?;
    readiness(a, &live, step)?;
    let mut bass = match step {
        Some(_) => a.bass.clone().ok_or("no subwoofer measurement")?,
        None => BassIntegration {
            plan: plan(a, &live)?,
            snapshot: live.clone(),
            level_probe: None,
            captures: vec![],
            analysis: None,
            analysis_error: None,
            applied: None,
            verification: None,
            verification_note: None,
        },
    };
    let (label, position_id) = match step {
        None => ("main-seat".to_owned(), a.settings.position_id.clone()),
        Some(step) => (
            GUIDE[step].id.to_owned(),
            positions::position_id(&a.settings.position_id, &GUIDE[step]),
        ),
    };
    let directory = a
        .directory
        .join(format!("bass-{label}-{}", alignment::stamp()));
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let speakers = bass.plan.speakers();
    let request_for = |snapshot: &Snapshot, part: Part, group: &Group, out_dir: PathBuf| {
        let mut request = alignment::request(
            a.endpoint,
            snapshot.clone(),
            &a.settings,
            group.outputs[0].clone(),
            out_dir,
        )?;
        request.position_id = position_id.clone();
        request.calibration = request
            .calibration
            .map(|_| a.directory.join(room_response::CALIBRATION_FILE));
        request.bass_managed = part != Part::Direct;
        request.bypass_output_filters = true;
        Ok::<MeasureRequest, String>(request)
    };
    let speaker_index = |id: &str| speakers.iter().position(|s| s == id).unwrap_or(0);
    // The level, from a quiet probe at the main seat, in the state the sum plays in.
    if step.is_none() && a.settings.automatic_level {
        let mut found = vec![];
        let mut probed: Vec<Group> = bass
            .plan
            .groups
            .iter()
            .filter_map(|g| part_group(g, Part::Sum, &live, a.settings.sample_rate))
            .collect();
        probed.push(bass.plan.direct.clone());
        for (index, group) in probed.iter().enumerate() {
            let part = if index + 1 == probed.len() {
                Part::Direct
            } else {
                Part::Sum
            };
            let request = request_for(&live, part, group, directory.join("level-probe"))?;
            let probe = level_probe::run(
                backend,
                &request,
                std::slice::from_ref(group),
                &directory.join(format!("level-probe-{}", index + 1)),
                a.settings.sweep.peak_dbfs,
                cancel,
                &mut |i, p| {
                    let id = group.outputs.get(i).map_or(0, |id| speaker_index(id));
                    progress(id, p)
                },
            )?;
            let peak = probe
                .levels
                .iter()
                .filter(|l| l.output_id == group.reference || group.outputs.contains(&l.output_id))
                .map(|l| l.peak_dbfs)
                .fold(sweep_plan::MAX_PEAK_DBFS, f32::min);
            match part {
                Part::Direct => bass.plan.direct.sweep.peak_dbfs = peak,
                _ => bass.plan.groups[index].sweep.peak_dbfs = peak,
            }
            found.extend(probe.levels);
        }
        bass.level_probe = Some(level_probe::Probe {
            directory: directory.clone(),
            levels: found,
        });
    }
    // Every part of every group, in the state it is measured in.
    let mut steps: Vec<(Part, Group)> = vec![];
    for part in Part::ALL {
        if part == Part::Direct {
            steps.extend(
                part_group(
                    &bass.plan.direct,
                    part,
                    &bass.snapshot,
                    a.settings.sample_rate,
                )
                .map(|g| (part, g)),
            );
            continue;
        }
        for group in &bass.plan.groups {
            if part == Part::Speaker && group.reference == bass.plan.subwoofer {
                continue;
            }
            if let Some(g) = part_group(group, part, &bass.snapshot, a.settings.sample_rate) {
                steps.push((part, g));
            }
        }
    }
    let several = bass.plan.groups.len() > 1;
    let mut takes = vec![];
    let mut error = None;
    let mut current = live.clone();
    for (index, (part, group)) in steps.iter().enumerate() {
        let part = *part;
        if cancel.load(Ordering::Relaxed) {
            error = Some("capture cancelled by user".to_owned());
            break;
        }
        let target = state(&bass.snapshot, &bass.plan, part, &group.reference);
        current = match alignment::move_outputs(backend, a.endpoint, &current, &target) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                error = Some(format!(
                    "Silencing outputs for \"{}\" failed: {e}",
                    part.label()
                ));
                break;
            }
        };
        let folder = if several && part != Part::Direct {
            format!("{}-{}", part.folder(), index + 1)
        } else {
            part.folder().to_owned()
        };
        let request = request_for(&current, part, group, directory.join(folder))?;
        let outcome = sweep_plan::measure_groups(
            backend,
            &request,
            std::slice::from_ref(group),
            cancel,
            &mut |i, p| {
                let id = group.outputs.get(i).map_or(0, |id| speaker_index(id));
                progress(id, p)
            },
        );
        match outcome {
            Ok(batch) => {
                takes.extend(
                    batch
                        .takes
                        .iter()
                        .map(|(id, take)| bass_take(id, part, take)),
                );
                if let Some(e) = batch
                    .error
                    .or_else(|| batch.cancelled.then(|| "capture cancelled by user".into()))
                {
                    error = Some(format!("{}: {e}", part.label()));
                    break;
                }
            }
            Err(e) => {
                error = Some(format!("{}: {e}", part.label()));
                break;
            }
        }
    }
    // Stopped or failed, the outputs go back to their settings all the same, from what
    // the renderer reads back: a write whose reply was lost may have gone through.
    if !settle(backend, a, &bass.snapshot) {
        return Err(
            "The outputs silenced for the subwoofer measurement could not be put back: check their gains in Studio, then refresh."
                .into(),
        );
    }
    let capture = Capture {
        step: step.map(|s| GUIDE[s].id.to_owned()),
        position_id,
        directory,
        takes,
        error: error.clone(),
    };
    bass.captures.retain(|c| c.step != capture.step);
    bass.captures.push(capture);
    bass.captures.sort_by_key(|c| {
        c.step.as_deref().map_or(0, |s| {
            1 + GUIDE.iter().position(|g| g.id == s).unwrap_or(GUIDE.len())
        })
    });
    bass.verification = None;
    bass.verification_note = None;
    match analyze(a, &bass) {
        Ok(analysis) => {
            bass.analysis = Some(analysis);
            bass.analysis_error = None;
        }
        Err(e) => {
            bass.analysis = None;
            bass.analysis_error = Some(e);
        }
    }
    a.bass = Some(bass);
    alignment::save(a)?;
    match error {
        Some(e) if e != "capture cancelled by user" => Err(e),
        _ => Ok(()),
    }
}

fn bass_take(id: &str, part: Part, take: &TakeOutcome) -> BassTake {
    BassTake {
        output_id: id.to_owned(),
        part,
        take_folder: take.directory.clone(),
        accepted: take.accepted,
        rejection_reason: take.rejection_reason.clone(),
        response: take.directory.join(room_response::FILE).is_file(),
    }
}

/// After a failed write, bring the renderer back to `measured`, as read back. Returns
/// whether that is confirmed; otherwise the alignment is saved as uncertain, which blocks
/// every action until a refresh finds the renderer in a known state.
fn settle<B: Backend + ?Sized>(backend: &B, a: &mut Alignment, measured: &Snapshot) -> bool {
    let restored = backend.discover(a.endpoint).ok().and_then(|live| {
        if alignment::same_live_settings(&live, measured) {
            Some(live)
        } else {
            alignment::move_outputs(backend, a.endpoint, &live, measured)
                .ok()
                .or_else(|| {
                    backend
                        .discover(a.endpoint)
                        .ok()
                        .filter(|live| alignment::same_live_settings(live, measured))
                })
        }
    });
    if restored.is_none() {
        a.application_uncertain = true;
    }
    let _ = alignment::save(a);
    restored.is_some()
}

/// One take's stored speaker segment on its position's time base, with its reference
/// segments; or why it cannot be used.
struct Loaded {
    response: core::Response,
    references: [Vec<f32>; 2],
    rate: f64,
}
fn load(take: &BassTake, output_id: &str) -> Result<Loaded, String> {
    if !take.accepted {
        return Err(take
            .rejection_reason
            .clone()
            .unwrap_or_else(|| "rejected take".into()));
    }
    let folder = crate::workspace::absolute(&take.take_folder);
    let (stored, full) = room_response::load(&folder).map_err(|e| e.to_string())?;
    if stored.output_id != output_id {
        return Err(format!("stored response is of {}", stored.output_id));
    }
    if stored.clipped_samples > 0 {
        return Err("clipped take".into());
    }
    let offset = stored
        .target_offset_frames
        .ok_or("its response was stored without its time base")?;
    let [before, speaker, after] = full.segments;
    let rate = full.rate;
    let anchor = full.anchor as f64 + full.anchor_offsets[1];
    Ok(Loaded {
        response: core::Response {
            rate,
            start_s: (offset - anchor) / rate,
            anchor_s: offset / rate,
            samples: speaker,
        },
        references: [before, after],
        rate,
    })
}

/// Line `loaded` up with `base` by their reference sweeps: the correlation peak a take
/// found in its references moves its whole time base.
fn line_up(loaded: &mut Loaded, base: &Loaded) -> Result<(), String> {
    let max = (MAX_REFERENCE_LAG_MS / 1000.0 * base.rate).round() as usize;
    let lags: Vec<f64> = (0..2)
        .filter_map(|i| core::lag(&loaded.references[i], &base.references[i], max))
        .collect();
    if lags.is_empty() {
        return Err("its reference sweeps do not line up with the other takes'".into());
    }
    let lag = lags.iter().sum::<f64>() / lags.len() as f64;
    // Content lying `lag` frames later in its references means its anchors were picked
    // `lag` frames early: its time base moves back by as much.
    let shift = lag / loaded.rate;
    loaded.response.start_s -= shift;
    loaded.response.anchor_s -= shift;
    Ok(())
}

/// What the analysis reads: every speaker's parts, the subwoofer's own responses and
/// its settings, and what was left out.
struct Inputs {
    speakers: Vec<core::SpeakerResponses>,
    direct: OutputMeasurements,
    subwoofer: core::Subwoofer,
    notes: Vec<String>,
}

fn inputs(bass: &BassIntegration) -> Result<Inputs, String> {
    let plan = &bass.plan;
    let output = |id: &str| bass.snapshot.outputs.iter().find(|o| o.id == id);
    let sub = output(&plan.subwoofer).ok_or("the subwoofer is absent from the measured state")?;
    let mut notes = vec![];
    let mut speakers = vec![];
    for group in &plan.groups {
        for id in &group.outputs {
            let fc = output(id)
                .and_then(|o| o.freq_low_hz)
                .ok_or_else(|| format!("{id} has no low limit in the measured state"))?;
            let mut positions = vec![];
            for capture in &bass.captures {
                let label = capture.step.as_deref().unwrap_or("main seat");
                let read = |part: Part| -> Result<Loaded, String> {
                    let take = capture
                        .take(id, part)
                        .ok_or_else(|| format!("{} not measured", part.label()))?;
                    load(take, id).map_err(|e| format!("{}: {e}", part.label()))
                };
                let lined_up = |part: Part, sum: &Loaded| -> Result<core::Response, String> {
                    let mut loaded = read(part)?;
                    line_up(&mut loaded, sum).map_err(|e| format!("{}: {e}", part.label()))?;
                    Ok(loaded.response)
                };
                let parts = (|| -> Result<_, String> {
                    let sum = read(Part::Sum)?;
                    let parts = plan.parts_of(id);
                    let speaker = parts
                        .contains(&Part::Speaker)
                        .then(|| lined_up(Part::Speaker, &sum))
                        .transpose()?;
                    let subwoofer = parts
                        .contains(&Part::Subwoofer)
                        .then(|| lined_up(Part::Subwoofer, &sum))
                        .transpose()?;
                    Ok(core::PositionResponses {
                        position_id: capture.position_id.clone(),
                        main_seat: capture.step.is_none(),
                        speaker,
                        subwoofer,
                        sum: sum.response,
                    })
                })();
                match parts {
                    Ok(p) => positions.push(p),
                    Err(e) => notes.push(format!("{id} at {label}: {e}")),
                }
            }
            speakers.push(core::SpeakerResponses {
                output_id: id.clone(),
                crossover_hz: f64::from(fc),
                swept_hz: [
                    f64::from(group.sweep.start_hz),
                    f64::from(group.sweep.end_hz),
                ],
                positions,
            });
        }
    }
    let mut measurements = vec![];
    for capture in &bass.captures {
        let label = capture.step.as_deref().unwrap_or("main seat");
        match capture.take(&plan.subwoofer, Part::Direct) {
            Some(take) if take.accepted => match crate::room_correction::read(
                &crate::workspace::absolute(&take.take_folder),
                &plan.subwoofer,
                capture.step.is_none(),
            ) {
                Ok(m) => measurements.push(m),
                Err(e) => notes.push(format!("{} at {label}: {e}", sub.name)),
            },
            Some(take) => notes.push(format!(
                "{} at {label}: {}",
                sub.name,
                take.rejection_reason.as_deref().unwrap_or("rejected take")
            )),
            None => {}
        }
    }
    let others_max_delay_ms = bass
        .snapshot
        .outputs
        .iter()
        .filter(|o| o.id != plan.subwoofer)
        .map(|o| f64::from(o.delay_ms))
        .fold(0.0, f64::max);
    Ok(Inputs {
        speakers,
        direct: OutputMeasurements {
            output_id: plan.subwoofer.clone(),
            measurements,
        },
        subwoofer: core::Subwoofer {
            output_id: plan.subwoofer.clone(),
            delay_ms: f64::from(sub.delay_ms),
            gain_db: f64::from(sub.gain_db),
            others_max_delay_ms,
            max_delay_ms: f64::from(bass.snapshot.max_delay_ms),
        },
        notes,
    })
}

/// The subwoofer's mode correction and the taps it plays through in the prediction.
fn mode_correction(
    bass: &BassIntegration,
    direct: &OutputMeasurements,
    notes: &mut Vec<String>,
) -> Result<(Option<OutputDesign>, Option<core::Filter>), String> {
    if direct.measurements.is_empty() {
        notes.push("No usable take of the subwoofer's own sweep: no mode correction.".into());
        return Ok((None, None));
    }
    let settings = SubwooferSettings {
        sample_rate: bass.snapshot.sample_rate,
        ..SubwooferSettings::default()
    };
    let designed = design::design_subwoofer(direct, &settings).map_err(|e| e.to_string())?;
    let filter = designed.filter.as_ref().map(|f| core::Filter {
        sample_rate: f64::from(f.sample_rate),
        taps: f.taps.clone(),
    });
    if let Some(reason) = &designed.refusal {
        notes.push(format!("No mode correction: {reason}"));
    }
    Ok((Some(designed), filter))
}

/// Analyse every capture of `bass` and save the proposal and the subwoofer's filter in a
/// new folder of the alignment.
pub fn analyze(a: &Alignment, bass: &BassIntegration) -> Result<Analysis, String> {
    let Inputs {
        speakers,
        direct,
        subwoofer,
        mut notes,
    } = inputs(bass)?;
    let (mut mode, filter) = mode_correction(bass, &direct, &mut notes)?;
    let proposal = core::propose(
        &speakers,
        &subwoofer,
        filter.as_ref(),
        &core::Settings::default(),
    )
    .map_err(|e| e.to_string())?;
    let directory = a
        .directory
        .join(format!("bass-integration-{}", alignment::stamp()));
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    // What the subwoofer plays: its mode correction, with the proposed polarity.
    let sign = if proposal.setting.inverted { -1.0 } else { 1.0 };
    let taps: Option<Vec<f32>> = match (&filter, proposal.setting.inverted) {
        _ if proposal.refusal.is_some() => None,
        (Some(f), _) => Some(f.taps.iter().map(|t| t * sign).collect()),
        (None, true) => Some(vec![-1.0]),
        (None, false) => None,
    };
    let filter_file = match taps {
        Some(taps) => {
            let file = directory.join(format!("{}.wav", file_name(&bass.plan.subwoofer)));
            crate::measurement::save_wav(&file, &taps, bass.snapshot.sample_rate)
                .map_err(|e| e.to_string())?;
            Some(FilterFile {
                file,
                sample_rate: bass.snapshot.sample_rate,
                taps: taps.len(),
                hash: protocol::taps_hash_hex(&taps),
            })
        }
        None => None,
    };
    if let Some(design) = &mut mode
        && let Some(f) = &mut design.filter
    {
        f.taps.clear();
    }
    let analysis = Analysis {
        directory: directory.clone(),
        proposal,
        mode_correction: mode,
        filter: filter_file,
        notes,
    };
    write_json(&directory.join(SUMMARY), &analysis).map_err(|e| e.to_string())?;
    Ok(analysis)
}

/// An output id as a file name.
fn file_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The taps of `file`, checked against the hash the analysis recorded.
fn read_filter(file: &FilterFile) -> Result<Vec<f32>, String> {
    let mut wav = hound::WavReader::open(&file.file).map_err(|e| e.to_string())?;
    let spec = wav.spec();
    if spec.channels != 1
        || spec.sample_format != hound::SampleFormat::Float
        || spec.sample_rate != file.sample_rate
    {
        return Err("the subwoofer filter file differs from its description".into());
    }
    let taps: Vec<f32> = wav
        .samples::<f32>()
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if protocol::taps_hash_hex(&taps) != file.hash {
        return Err("the subwoofer filter file changed since it was designed".into());
    }
    Ok(taps)
}

/// Why the proposal cannot be applied (or undone, with `restore`) now.
pub fn apply_readiness(a: &Alignment, live: &Snapshot, restore: bool) -> Result<(), String> {
    let bass = a.bass.as_ref().ok_or("Measure the subwoofer first.")?;
    if a.application_uncertain {
        return Err(
            "It is not known whether the last Apply reached Omniphony. Refresh first.".into(),
        );
    }
    if restore {
        return match bass.live_state(live) {
            LiveState::Applied | LiveState::Original if bass.is_applied() => Ok(()),
            _ if !bass.is_applied() => Err("The subwoofer integration is not applied.".into()),
            _ => Err("Omniphony's settings differ from both the measured and the proposed ones; nothing to undo here.".into()),
        };
    }
    if a.physical_levels_changed {
        return Err("Speaker volumes may have been turned. Measure a new alignment first.".into());
    }
    let analysis = bass.analysis.as_ref().ok_or_else(|| {
        bass.analysis_error
            .clone()
            .unwrap_or_else(|| "Nothing analysed yet.".into())
    })?;
    if let Some(reason) = &analysis.proposal.refusal {
        return Err(reason.clone());
    }
    match bass.live_state(live) {
        LiveState::Original => Ok(()),
        LiveState::Applied => Err("Already applied.".into()),
        LiveState::Other => Err(
            "Omniphony's settings changed since the subwoofer was measured: measure it again."
                .into(),
        ),
    }
}

/// Apply the proposal (or undo it, with `restore`): the subwoofer's delay and level and
/// every other output's added delay with `set_outputs`, the subwoofer's filter with
/// `set_output_filters`. Live only, until Omniphony's Save.
pub fn apply<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    restore: bool,
) -> Result<Snapshot, String> {
    let live = backend.discover(a.endpoint)?;
    apply_readiness(a, &live, restore)?;
    let bass = a.bass.clone().ok_or("no subwoofer measurement")?;
    let target = if restore {
        bass.snapshot.clone()
    } else {
        bass.target().ok_or("nothing to apply")?
    };
    if restore && bass.live_state(&live) == LiveState::Original {
        if let Some(b) = &mut a.bass {
            b.applied = None;
            b.verification = None;
        }
        alignment::save(a)?;
        return Ok(live);
    }
    let taps = match (
        restore,
        bass.analysis.as_ref().and_then(|x| x.filter.as_ref()),
    ) {
        (false, Some(file)) => Some((file.sample_rate, read_filter(file)?)),
        _ => None,
    };
    a.application_uncertain = true;
    alignment::save(a)?;
    let result = (|| -> Result<Snapshot, String> {
        let mut current = live;
        let sub = &bass.plan.subwoofer;
        let has_filter =
            |s: &Snapshot| s.outputs.iter().any(|o| &o.id == sub && o.filter.is_some());
        // The filter first, loaded before the gains and delays move, so every write
        // compares against a settled state; undone, the gains and delays first.
        if !restore && taps.is_some() {
            current = backend.set_output_filters(
                a.endpoint,
                current,
                vec![OutputFilter {
                    output_id: sub.clone(),
                    taps,
                }],
            )?;
            current = wait_for_filter(backend, a.endpoint, sub, current)?;
        }
        current = alignment::move_outputs(backend, a.endpoint, &current, &target)?;
        if restore && has_filter(&current) {
            current = backend.set_output_filters(
                a.endpoint,
                current,
                vec![OutputFilter {
                    output_id: sub.clone(),
                    taps: None,
                }],
            )?;
        }
        Ok(current)
    })();
    let state = |s: &Snapshot| bass.live_state(s);
    match result {
        Ok(snapshot)
            if state(&snapshot)
                == if restore {
                    LiveState::Original
                } else {
                    LiveState::Applied
                } =>
        {
            a.application_uncertain = false;
            if let Some(b) = &mut a.bass {
                b.applied = (!restore).then(|| snapshot.clone());
                b.verification = None;
                b.verification_note = None;
            }
            alignment::save(a)?;
            Ok(snapshot)
        }
        outcome => {
            // Read the renderer back: a refusal or a lost reply may have left it in a
            // known state.
            if let Ok(live) = backend.discover(a.endpoint) {
                match state(&live) {
                    LiveState::Applied => {
                        a.application_uncertain = false;
                        if let Some(b) = &mut a.bass {
                            b.applied = Some(live);
                        }
                    }
                    LiveState::Original => {
                        a.application_uncertain = false;
                        if let Some(b) = &mut a.bass {
                            b.applied = None;
                        }
                    }
                    LiveState::Other => {}
                }
            }
            alignment::save(a)?;
            Err(match outcome {
                Err(e) => e,
                Ok(_) => "Omniphony reads back other settings than were sent; check the subwoofer in Studio.".into(),
            })
        }
    }
}

/// Discover until the subwoofer's filter is loaded (or it has none).
fn wait_for_filter<B: Backend + ?Sized>(
    backend: &B,
    endpoint: std::net::SocketAddr,
    sub: &str,
    mut current: Snapshot,
) -> Result<Snapshot, String> {
    let deadline = Instant::now() + FILTER_LOAD_TIMEOUT;
    loop {
        let state = current
            .outputs
            .iter()
            .find(|o| o.id == sub)
            .and_then(|o| o.filter.as_ref())
            .map(|f| (f.state, f.error.clone()));
        match state {
            None | Some((protocol::FilterLoadState::Loaded, _)) => return Ok(current),
            Some((protocol::FilterLoadState::Error, error)) => {
                return Err(format!(
                    "Omniphony could not use the subwoofer filter: {}",
                    error.unwrap_or_default()
                ));
            }
            Some((protocol::FilterLoadState::Pending, _)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                current = backend.discover(endpoint)?;
            }
            Some((protocol::FilterLoadState::Pending, _)) => {
                return Err(
                    "Omniphony is still loading the subwoofer filter; refresh in a moment.".into(),
                );
            }
        }
    }
}

/// Why the applied proposal cannot be confirmed now.
pub fn verify_readiness(a: &Alignment, live: &Snapshot) -> Result<(), String> {
    let bass = a.bass.as_ref().ok_or("Measure the subwoofer first.")?;
    if a.application_uncertain {
        return Err(
            "It is not known whether the last Apply reached Omniphony. Refresh first.".into(),
        );
    }
    if !bass.is_applied() || bass.live_state(live) != LiveState::Applied {
        return Err("Apply the subwoofer integration first.".into());
    }
    if bass.capture(None).is_none() {
        return Err("Measure the subwoofer at the main seat first.".into());
    }
    Ok(())
}

/// Measure the sum again at the main seat, with the proposal applied and the filters
/// playing, and compare it with the prediction.
pub fn verify<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, MeasureProgress),
) -> Result<(), String> {
    let live = backend.discover(a.endpoint)?;
    verify_readiness(a, &live)?;
    // An earlier confirmation is no longer the current one, whatever happens next.
    if let Some(b) = &mut a.bass {
        b.verification = None;
        b.verification_note = Some("Not confirmed: the last confirmation did not complete.".into());
    }
    alignment::save(a)?;
    let mut bass = a.bass.clone().ok_or("no subwoofer measurement")?;
    let directory = a
        .directory
        .join(format!("bass-verification-{}", alignment::stamp()));
    let Inputs {
        speakers, direct, ..
    } = inputs(&bass)?;
    let mut notes = vec![];
    let (_, filter) = mode_correction(&bass, &direct, &mut notes)?;
    let main = bass.capture(None).ok_or("no main-seat capture")?.clone();
    let analysis = bass.analysis.as_ref().ok_or("nothing analysed")?;
    // The subwoofer's own delay change moves a group it frames: its reference arrives
    // that much later, and its polarity may have turned.
    let reference_moved_s =
        (analysis.proposal.subwoofer_delay_ms - analysis.proposal.subwoofer.delay_ms) / 1000.0;
    let speaker_list = bass.plan.speakers();
    let mut measured = vec![];
    let mut warnings = vec![];
    for (index, group) in bass.plan.groups.iter().enumerate() {
        let Some(group) = part_group(group, Part::Sum, &live, a.settings.sample_rate) else {
            continue;
        };
        let mut request = alignment::request(
            a.endpoint,
            live.clone(),
            &a.settings,
            group.outputs[0].clone(),
            directory.join(format!("sum-{}", index + 1)),
        )?;
        request.calibration = request
            .calibration
            .map(|_| a.directory.join(room_response::CALIBRATION_FILE));
        request.bass_managed = true;
        let batch = sweep_plan::measure_groups(
            backend,
            &request,
            std::slice::from_ref(&group),
            cancel,
            &mut |i, p| {
                let id = group
                    .outputs
                    .get(i)
                    .and_then(|id| speaker_list.iter().position(|s| s == id))
                    .unwrap_or(0);
                progress(id, p)
            },
        )?;
        if let Some(e) = batch
            .error
            .clone()
            .or_else(|| batch.cancelled.then(|| "capture cancelled by user".into()))
        {
            bass.verification_note = Some(format!("Not confirmed: {e}"));
            a.bass = Some(bass);
            alignment::save(a)?;
            return Err(e);
        }
        let framed_by_subwoofer = group.reference == bass.plan.subwoofer;
        for (id, take) in &batch.takes {
            let take = bass_take(id, Part::Sum, take);
            let loaded = load(&take, id).and_then(|mut now| {
                if framed_by_subwoofer {
                    now.response.start_s += reference_moved_s;
                    now.response.anchor_s += reference_moved_s;
                } else {
                    let base = main
                        .take(id, Part::Sum)
                        .ok_or("not measured at the main seat")
                        .map_err(String::from)
                        .and_then(|t| load(t, id))?;
                    line_up(&mut now, &base)?;
                }
                Ok(now)
            });
            match loaded {
                Ok(now) => measured.push((id.clone(), main.position_id.clone(), now.response)),
                Err(e) => warnings.push(format!("{id}: {e}")),
            }
        }
    }
    let analysis = bass.analysis.as_ref().ok_or("nothing analysed")?;
    let mut verification = core::verify(
        &speakers,
        &analysis.proposal,
        filter.as_ref(),
        &measured,
        &core::Settings::default(),
    )
    .map_err(|e| e.to_string())?;
    verification.warnings.extend(warnings);
    write_json(&directory.join("bass-verification.json"), &verification)
        .map_err(|e| e.to_string())?;
    bass.verification = Some(verification);
    bass.verification_note = None;
    a.bass = Some(bass);
    alignment::save(a)?;
    Ok(())
}

/// The folder a bass capture or analysis lives in, relative to the alignment.
pub fn relative<'a>(a: &Alignment, path: &'a Path) -> &'a Path {
    path.strip_prefix(&a.directory).unwrap_or(path)
}
