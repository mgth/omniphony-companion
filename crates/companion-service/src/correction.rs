//! The guided room correction: every speaker's correction filter
//! ([`crate::room_correction`]) with the subwoofer's integration ([`crate::bass`])
//! folded in, applied to Omniphony live through measurement protocol 8, compared by
//! ear, confirmed by measuring again and undone.
//!
//! **Apply** records what each corrected output plays first (no filter, or a filter
//! whose taps the Companion can hold: a file Omniphony reads, read back and checked
//! against its hash, or one of the Companion's own designs), keeps a copy, uploads every
//! filter and replaces them in one compare-and-swap `set_output_filters`; with the
//! subwoofer, its filter goes in the same write and its delay and level follow with
//! `set_outputs`. **Undo** puts the recorded filters back the same way.
//!
//! **Original / Corrected** switches the speakers' filters for listening: Original
//! plays each speaker's previous filter, or a one-tap identity where it had none, so
//! the correction stage stays in the path and the latency does not change. The
//! subwoofer's integration stays as applied. A measurement session's
//! `bypass_output_filters` cannot do this: a session silences programme audio.
//!
//! Every write is live only: Omniphony keeps it until Studio's Save, which writes each
//! filter as a file. The snapshot tells which ([`Correction::saved`]).
use crate::{
    alignment::{self, Alignment, LiveState},
    controller::{Backend, OutputFilter},
    files::write_json,
    measurement::MeasureProgress,
    positions, room_correction, room_response,
    sweep_plan::{self, Group},
};
use calibration_core::room_correction::{self as design, Curves, DesignSettings, OutputDesign};
use omniphony_adapter::{
    Snapshot,
    protocol::{self, FilterLoadState, FilterOrigin},
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

/// What the Original side of the comparison plays where an output had no filter.
pub const IDENTITY: [f32; 1] = [1.0];
/// Results of each confirmation, in its folder.
pub const VERIFICATION: &str = "room-correction-verification.json";
/// How long a write waits for Omniphony to load the filters it sent.
const FILTER_LOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The content hash of [`IDENTITY`], as the renderer reports it.
pub fn identity_hash() -> String {
    protocol::taps_hash_hex(&IDENTITY)
}

/// The settings a user may change before computing, under Lab. The defaults need no
/// touching.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Choices {
    pub tilt_db_per_decade: f64,
    /// No correction above this.
    pub max_hz: f64,
    pub max_boost_db: f64,
    /// Fold the subwoofer's integration (delay, level, polarity, mode correction) into
    /// Apply and Undo when it was measured.
    pub with_subwoofer: bool,
}
impl Default for Choices {
    fn default() -> Self {
        let d = DesignSettings::default();
        Self {
            tilt_db_per_decade: d.tilt_db_per_decade,
            max_hz: d.max_hz,
            max_boost_db: d.max_boost_db,
            with_subwoofer: true,
        }
    }
}
impl Choices {
    /// The design settings for `a`: the defaults at its renderer's rate, with these.
    pub fn design_settings(&self, a: &Alignment) -> DesignSettings {
        DesignSettings {
            tilt_db_per_decade: self.tilt_db_per_decade,
            max_hz: self.max_hz,
            max_boost_db: self.max_boost_db,
            ..room_correction::settings(a)
        }
    }
}

/// Taps saved as a mono 32-bit float WAV file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Taps {
    pub file: PathBuf,
    pub sample_rate: u32,
    pub taps: usize,
    /// [`protocol::taps_hash_hex`]: what the renderer reports while it plays them.
    pub hash: String,
}

/// One corrected speaker's filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerFilter {
    pub output_id: String,
    #[serde(flatten)]
    pub taps: Taps,
}

/// What an output played before Apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Previous {
    pub output_id: String,
    /// `None`: no filter.
    pub filter: Option<PreviousFilter>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviousFilter {
    /// A copy of its taps in the correction's folder.
    #[serde(flatten)]
    pub taps: Taps,
    pub origin: FilterOrigin,
    /// The file Omniphony read it from, for a file filter.
    pub source: Option<String>,
}

/// One speaker measured again through its filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerCheck {
    pub output_id: String,
    /// Distance from the target's shape at the main seat (RMS, its mean removed): as
    /// first measured, as predicted with the filter, as measured now.
    pub deviation_db: [f64; 3],
    /// RMS difference between the new measurement and the prediction, mean removed.
    pub from_prediction_db: f64,
    /// The new measurement on the design's curve grid (`NaN` where it has no point).
    pub measured_db: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    pub directory: PathBuf,
    pub speakers: Vec<SpeakerCheck>,
    pub warnings: Vec<String>,
}

/// The computed correction of an alignment and where Omniphony stands with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    pub choices: Choices,
    pub settings: DesignSettings,
    /// The design's export: one WAV per corrected speaker, its summary and curves.
    pub directory: PathBuf,
    pub summary: room_correction::Summary,
    pub filters: Vec<SpeakerFilter>,
    /// What the corrected speakers played before Apply; recorded by Apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<Previous>,
    /// The renderer read back after Apply, while applied.
    pub applied: Option<Snapshot>,
    /// Set before a write; cleared once the renderer shows where it went.
    #[serde(default)]
    pub uncertain: bool,
    /// Apply also applied the subwoofer's integration (or its filter alone, when the
    /// write of its delay failed), which Undo then undoes.
    #[serde(default)]
    pub subwoofer: bool,
    /// What the last write asked for, kept while it is uncertain: a readback that
    /// matches it settles it even where the hashes alone are ambiguous (every speaker
    /// had a filter, so the original and the comparison play the same).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Target>,
    pub verification: Option<Verification>,
    pub verification_note: Option<String>,
    /// Per speaker, the design's curves, read from the export.
    #[serde(skip)]
    pub curves: Vec<(String, Curves)>,
}

/// Where the renderer stands with a correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    /// The speakers play what they played before Apply.
    Original,
    /// Every corrected speaker plays its correction filter.
    Corrected,
    /// Applied, listening to the original: the previous filters, identity where none.
    Comparing,
    /// Neither: the filters changed elsewhere, or the layout did.
    Other,
}

/// What Apply is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Every correction filter (and the subwoofer's integration when chosen).
    Corrected,
    /// The original filters at the corrected latency, for listening.
    Compare,
    /// Undo: the original filters and subwoofer settings.
    Original,
}

/// Whether Omniphony has saved what plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    /// Nothing of this correction plays.
    NotApplied,
    /// Live only: these outputs' filters reach the configuration only through Save.
    Unsaved(Vec<String>),
    /// Every corrected output plays its filter from a file the configuration names.
    Saved,
}

fn hash_of<'a>(live: &'a Snapshot, id: &str) -> Option<Option<&'a str>> {
    let output = live.outputs.iter().find(|o| o.id == id)?;
    Some(
        output
            .filter
            .as_ref()
            .map(|f| f.hash.as_deref().unwrap_or("")),
    )
}

impl Correction {
    pub fn curves_of(&self, output_id: &str) -> Option<&Curves> {
        self.curves
            .iter()
            .find(|(id, _)| id == output_id)
            .map(|(_, c)| c)
    }
    pub fn filter_of(&self, output_id: &str) -> Option<&SpeakerFilter> {
        self.filters.iter().find(|f| f.output_id == output_id)
    }
    pub fn summary_of(&self, output_id: &str) -> Option<&room_correction::OutputSummary> {
        self.summary
            .outputs
            .iter()
            .find(|o| o.output_id == output_id)
    }
    fn previous_of(&self, output_id: &str) -> Option<&Previous> {
        self.previous.iter().find(|p| p.output_id == output_id)
    }
    /// Where `live` stands with this correction.
    pub fn live_state(&self, live: &Snapshot) -> Live {
        let mut corrected = true;
        let mut original = true;
        let mut listening = true;
        let identity = identity_hash();
        for filter in &self.filters {
            let Some(now) = hash_of(live, &filter.output_id) else {
                return Live::Other;
            };
            corrected &= now == Some(filter.taps.hash.as_str());
            match self.previous_of(&filter.output_id) {
                // Not applied yet: anything but the correction is the original.
                None => {
                    listening = false;
                    original &= self.previous.is_empty();
                }
                Some(previous) => {
                    let before = previous.filter.as_ref().map(|f| f.taps.hash.as_str());
                    original &= now == before;
                    listening &= now == before.or(Some(identity.as_str()));
                }
            }
        }
        if self.filters.is_empty() {
            return Live::Other;
        }
        match (corrected, listening, original) {
            (true, ..) => Live::Corrected,
            (_, true, _) if self.applied.is_some() => Live::Comparing,
            (_, _, true) => Live::Original,
            (_, true, _) => Live::Comparing,
            // Not applied: whatever plays is the original, which the next Apply records.
            _ if self.applied.is_none() && !self.uncertain => Live::Original,
            _ => Live::Other,
        }
    }
    /// Whether `live` plays exactly what `to` asks for: every design hash, the recorded
    /// filters with an identity where none was, or the recorded filters exactly.
    pub fn matches(&self, live: &Snapshot, to: Target) -> bool {
        if self.filters.is_empty() {
            return false;
        }
        let identity = identity_hash();
        self.filters.iter().all(|filter| {
            let Some(now) = hash_of(live, &filter.output_id) else {
                return false;
            };
            if to == Target::Corrected {
                return now == Some(filter.taps.hash.as_str());
            }
            let Some(previous) = self.previous_of(&filter.output_id) else {
                return false;
            };
            let before = previous.filter.as_ref().map(|f| f.taps.hash.as_str());
            match to {
                Target::Compare => now == before.or(Some(identity.as_str())),
                _ => now == before,
            }
        })
    }
    /// Whether Omniphony's configuration holds what plays of this correction.
    pub fn saved(&self, live: &Snapshot) -> Saved {
        if self.live_state(live) != Live::Corrected {
            return Saved::NotApplied;
        }
        let unsaved: Vec<String> = self
            .filters
            .iter()
            .filter(|f| {
                live.outputs
                    .iter()
                    .find(|o| o.id == f.output_id)
                    .and_then(|o| o.filter.as_ref())
                    .is_none_or(|filter| filter.origin != FilterOrigin::File)
            })
            .map(|f| f.output_id.clone())
            .collect();
        if unsaved.is_empty() {
            Saved::Saved
        } else {
            Saved::Unsaved(unsaved)
        }
    }
    /// The design of one speaker as the core describes it, for checking a new
    /// measurement against it. `None` when it is not corrected or its curves are gone.
    fn output_design(&self, output_id: &str) -> Option<OutputDesign> {
        let summary = self.summary_of(output_id)?;
        let filter = self.filter_of(output_id)?;
        let mut d = OutputDesign::refused(output_id, "");
        d.refusal = None;
        d.filter = Some(design::Filter {
            sample_rate: filter.taps.sample_rate,
            taps: vec![],
            headroom_db: summary.headroom_db.unwrap_or(0.0),
            deepest_cut_db: summary.deepest_cut_db.unwrap_or(0.0),
            normalisation_db: summary.normalisation_db.unwrap_or(0.0),
        });
        d.band_hz = summary.band_hz;
        d.low_limit_hz = summary.low_limit_hz;
        d.evaluation_band_hz = summary.evaluation_band_hz;
        d.deviation_db = summary.deviation_db;
        d.curves = Some(self.curves_of(output_id)?.clone());
        Some(d)
    }
}

/// Read the curves of `c`'s export back, after loading an alignment.
pub fn attach_curves(c: &mut Correction) {
    let path = crate::workspace::absolute(&c.directory).join(room_correction::CURVES);
    #[derive(Deserialize)]
    struct Entry {
        output_id: String,
        curves: Curves,
    }
    if let Ok(entries) = crate::files::read_json::<Vec<Entry>>(&path) {
        c.curves = entries
            .into_iter()
            .map(|e| (e.output_id, e.curves))
            .collect();
    }
}

/// What may stop a computation, and what it should be told about first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Readiness {
    /// Why it cannot run now.
    pub blocker: Option<String>,
    /// What makes the result weaker, in plain words.
    pub warnings: Vec<String>,
}

/// Whether `a`'s correction can be computed now, from its stored responses only. A
/// correction that plays must be undone first: the new one would otherwise record the
/// old filters as the ones Undo puts back.
pub fn readiness(a: &Alignment, live: Option<&Snapshot>, version: Option<u32>) -> Readiness {
    let mut r = Readiness {
        blocker: room_correction::readiness(a).err(),
        ..Default::default()
    };
    if let Some(c) = &a.correction
        && r.blocker.is_none()
    {
        if c.uncertain {
            r.blocker = Some(
                "It is not known whether the last change reached Omniphony. Refresh first.".into(),
            );
        } else if c.applied.is_some() || live.is_some_and(|l| c.live_state(l) == Live::Comparing) {
            r.blocker = Some("Undo the applied correction first, then compute it again.".into());
        }
    }
    let calibrated = a
        .rows
        .iter()
        .flat_map(|row| row.take_folders.first())
        .any(|f| {
            crate::workspace::absolute(f)
                .join(room_response::CALIBRATION_FILE)
                .is_file()
        });
    if !calibrated {
        r.warnings.push("No microphone calibration: the treble is corrected toward the microphone's own response. Choose its calibration file and measure again for a trustworthy result.".into());
    }
    let complete = a.positions.iter().filter(|p| p.complete(a)).count();
    let partial = a.positions.len() - complete;
    match complete {
        0 => r.warnings.push(format!("Main seat only: measure the {} positions around it, so a dip that moves with the microphone is not filled.", positions::GUIDE.len())),
        n if n < 3 => r.warnings.push(format!("{n} of {} positions measured: more of them make the correction hold around the seat.", positions::GUIDE.len())),
        _ => {}
    }
    if partial > 0 {
        r.warnings.push(format!(
            "{partial} position(s) measured only in part: their missing speakers use the other positions."
        ));
    }
    if version.is_some_and(|v| v < protocol::FILTERS_VERSION) {
        r.warnings.push("This Omniphony build cannot take correction filters from the Companion (measurement protocol 8 is needed): the filters will be saved as WAV files to load in Studio. Update Omniphony to apply them from here.".into());
    }
    r
}

/// Design `a`'s correction with `choices`, export it in a new folder of the alignment
/// and describe it. Nothing is sent to Omniphony.
pub fn compute(a: &Alignment, choices: Choices) -> Result<Correction, String> {
    if let Some(blocker) = readiness(a, None, None).blocker {
        return Err(blocker);
    }
    let settings = choices.design_settings(a);
    let designed = room_correction::design(a, &settings)?;
    if designed.designed().next().is_none() {
        let reasons: Vec<String> = designed
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
    let (directory, summary) = room_correction::export(a, &designed).map_err(|e| e.to_string())?;
    let filters = summary
        .outputs
        .iter()
        .zip(&designed.outputs)
        .filter_map(|(s, d)| {
            let file = s.file.as_ref()?;
            let filter = d.filter.as_ref()?;
            Some(SpeakerFilter {
                output_id: s.output_id.clone(),
                taps: Taps {
                    file: directory.join(file),
                    sample_rate: filter.sample_rate,
                    taps: filter.taps.len(),
                    hash: protocol::taps_hash_hex(&filter.taps),
                },
            })
        })
        .collect();
    let curves = designed
        .outputs
        .iter()
        .filter_map(|o| o.curves.clone().map(|c| (o.output_id.clone(), c)))
        .collect();
    Ok(Correction {
        choices,
        settings,
        directory,
        summary,
        filters,
        previous: vec![],
        applied: None,
        uncertain: false,
        subwoofer: false,
        pending: None,
        verification: None,
        verification_note: None,
        curves,
    })
}

/// The taps of a mono WAV file and its rate: floats as they are, integers scaled to
/// ±1 as Omniphony reads them.
pub fn read_taps(path: &Path) -> Result<(u32, Vec<f32>), String> {
    let mut wav = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = wav.spec();
    if spec.channels != 1 {
        return Err(format!("{} is not mono", path.display()));
    }
    let taps: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => wav.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1_i64 << (spec.bits_per_sample - 1)) as f32;
            wav.samples::<i32>()
                .map(|s| s.map(|s| s as f32 * scale))
                .collect::<Result<_, _>>()
        }
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((spec.sample_rate, taps))
}

/// Whether the subwoofer's integration goes with this Apply: chosen, analysed with a
/// proposal, not applied yet.
fn with_subwoofer(a: &Alignment, c: &Correction) -> bool {
    c.choices.with_subwoofer
        && a.bass.as_ref().is_some_and(|b| {
            !b.is_applied()
                && b.analysis
                    .as_ref()
                    .is_some_and(|x| x.proposal.refusal.is_none())
        })
}

/// What each corrected output plays now, as Undo will need it; copies of the taps go
/// into `directory`. Refused for a filter whose taps cannot be held: one still loading,
/// unusable, or uploaded by another client and not saved.
fn record_previous(
    a: &Alignment,
    c: &Correction,
    live: &Snapshot,
) -> Result<Vec<Previous>, String> {
    let directory = crate::workspace::absolute(&c.directory).join("previous");
    let mut previous = vec![];
    for filter in &c.filters {
        let output = live
            .outputs
            .iter()
            .find(|o| o.id == filter.output_id)
            .ok_or_else(|| format!("{} is no longer in Omniphony's layout.", filter.output_id))?;
        let Some(now) = &output.filter else {
            previous.push(Previous {
                output_id: output.id.clone(),
                filter: None,
            });
            continue;
        };
        let name = &output.name;
        if now.state != FilterLoadState::Loaded {
            return Err(match now.state {
                FilterLoadState::Pending => {
                    format!("{name}'s current filter is still loading; refresh in a moment.")
                }
                _ => format!(
                    "{name}'s current filter cannot be used by Omniphony ({}): fix or remove it in Studio first.",
                    now.error.as_deref().unwrap_or("unknown error")
                ),
            });
        }
        let hash = now.hash.clone().unwrap_or_default();
        let taps = match (now.origin, &now.file) {
            (FilterOrigin::File, Some(file)) => {
                let (rate, taps) = read_taps(Path::new(file)).map_err(|e| {
                    format!("{name} plays the filter file {file}, which the Companion cannot read ({e}): Undo could not put it back. Remove it in Studio, or run the Companion where Omniphony runs.")
                })?;
                (protocol::taps_hash_hex(&taps) == hash)
                    .then_some((rate, taps))
                    .ok_or_else(|| format!("{name}'s filter file {file} changed since Omniphony read it: Reload in Studio first."))?
            }
            _ => known_taps(a, &hash).ok_or_else(|| {
                format!("{name} plays a measured filter that is not saved, so the Companion cannot put it back after Undo: press Save in Studio first, or remove it.")
            })?,
        };
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let file = directory.join(room_correction::file_name(&output.id));
        if !file.is_file() {
            room_correction::write_wav(&file, &taps.1, taps.0).map_err(|e| e.to_string())?;
        }
        let (rate, written) = read_taps(&file)?;
        if protocol::taps_hash_hex(&written) != hash || rate != taps.0 {
            return Err(format!(
                "A copy of {name}'s current filter could not be kept in {}.",
                directory.display()
            ));
        }
        previous.push(Previous {
            output_id: output.id.clone(),
            filter: Some(PreviousFilter {
                taps: Taps {
                    file,
                    sample_rate: rate,
                    taps: written.len(),
                    hash,
                },
                origin: now.origin,
                source: now.file.clone(),
            }),
        });
    }
    Ok(previous)
}

/// The recorded filters of `a` the Companion holds the taps of: this correction's
/// filters, the originals it recorded, the subwoofer's filter. Metadata only.
fn known(a: &Alignment) -> Vec<&Taps> {
    let mut known: Vec<&Taps> = vec![];
    if let Some(c) = &a.correction {
        known.extend(c.filters.iter().map(|f| &f.taps));
        known.extend(
            c.previous
                .iter()
                .filter_map(|p| p.filter.as_ref().map(|f| &f.taps)),
        );
    }
    known
}

/// Whether a live filter with `hash` is one the Companion can put back, without reading
/// any file (readiness runs at every frame).
fn is_known(a: &Alignment, hash: &str) -> bool {
    known(a).iter().any(|t| t.hash == hash)
        || a.bass
            .as_ref()
            .and_then(|b| b.analysis.as_ref())
            .and_then(|x| x.filter.as_ref())
            .is_some_and(|f| f.hash == hash)
}

/// The taps with `hash` among [`is_known`]'s, read and checked.
fn known_taps(a: &Alignment, hash: &str) -> Option<(u32, Vec<f32>)> {
    if let Some(taps) = known(a).into_iter().find(|t| t.hash == hash) {
        return load(taps).ok();
    }
    let file = a
        .bass
        .as_ref()
        .and_then(|b| b.analysis.as_ref())
        .and_then(|x| x.filter.as_ref())
        .filter(|f| f.hash == hash)?;
    crate::bass::read_filter(file)
        .ok()
        .map(|taps| (file.sample_rate, taps))
}

/// The taps of `taps`' file, checked against its hash.
fn load(taps: &Taps) -> Result<(u32, Vec<f32>), String> {
    let (rate, read) = read_taps(&crate::workspace::absolute(&taps.file))?;
    if rate != taps.sample_rate || protocol::taps_hash_hex(&read) != taps.hash {
        return Err(format!(
            "{} changed since it was designed: compute the correction again.",
            taps.file.display()
        ));
    }
    Ok((rate, read))
}

/// Why `a`'s correction cannot be taken to `to` now, with `live` the renderer speaking
/// measurement protocol `version`.
pub fn apply_readiness(
    a: &Alignment,
    live: &Snapshot,
    version: u32,
    to: Target,
) -> Result<(), String> {
    let c = a
        .correction
        .as_ref()
        .ok_or("Compute the correction first.")?;
    if version < protocol::FILTERS_VERSION {
        return Err(format!(
            "This Omniphony build cannot take correction filters from the Companion (measurement protocol 8 is needed); update Omniphony, or load the WAV files of {} in Studio.",
            c.directory.display()
        ));
    }
    if c.uncertain || a.application_uncertain {
        return Err(
            "It is not known whether the last change reached Omniphony. Refresh first.".into(),
        );
    }
    if live.output_filters_bypassed {
        return Err("A measurement session holds the filters off: wait for it to end.".into());
    }
    if live.sample_rate != c.settings.sample_rate {
        return Err(format!(
            "Omniphony now runs at {} Hz and the filters were designed at {} Hz: compute the correction again.",
            live.sample_rate, c.settings.sample_rate
        ));
    }
    // The filters were designed for the speakers as the alignment measured them: a
    // speaker routed elsewhere, split at another crossover or retuned in Studio under the
    // same name is no longer that speaker. Undo stays possible.
    if to != Target::Original && a.live_state(live) == LiveState::Other {
        return Err("Omniphony's speakers changed since this alignment was measured (routing, crossover, mute, gain or delay): measure a new alignment, then compute the correction again.".into());
    }
    let state = c.live_state(live);
    match to {
        Target::Corrected => match state {
            Live::Corrected => Err("Already applied.".into()),
            Live::Other => Err("The speakers' filters changed in Omniphony since this correction was applied: Undo cannot tell what to put back. Refresh, or set them back in Studio.".into()),
            Live::Comparing => Ok(()),
            Live::Original => {
                if with_subwoofer(a, c) {
                    crate::bass::apply_readiness(a, live, false).map_err(|e| {
                        format!("{e} (or turn the subwoofer off in the correction settings)")
                    })?;
                }
                record_previous_check(a, c, live)
            }
        },
        Target::Compare => match state {
            Live::Corrected | Live::Comparing if !c.previous.is_empty() => Ok(()),
            _ => Err("Apply the correction first.".into()),
        },
        Target::Original => match state {
            Live::Corrected | Live::Comparing if !c.previous.is_empty() => Ok(()),
            Live::Original => Err("The correction is not applied.".into()),
            _ => Err("The speakers' filters changed in Omniphony since this correction was applied; nothing to undo here.".into()),
        },
    }
}

/// [`record_previous`]'s refusals without writing copies.
fn record_previous_check(a: &Alignment, c: &Correction, live: &Snapshot) -> Result<(), String> {
    for filter in &c.filters {
        let output = live
            .outputs
            .iter()
            .find(|o| o.id == filter.output_id)
            .ok_or_else(|| format!("{} is no longer in Omniphony's layout.", filter.output_id))?;
        if let Some(now) = &output.filter {
            let name = &output.name;
            match now.state {
                FilterLoadState::Loaded => {}
                FilterLoadState::Pending => {
                    return Err(format!(
                        "{name}'s current filter is still loading; refresh in a moment."
                    ));
                }
                FilterLoadState::Error => {
                    return Err(format!(
                        "{name}'s current filter cannot be used by Omniphony: fix or remove it in Studio first."
                    ));
                }
            }
            if now.origin == FilterOrigin::Live && !is_known(a, now.hash.as_deref().unwrap_or("")) {
                return Err(format!(
                    "{name} plays a measured filter that is not saved, so the Companion cannot put it back after Undo: press Save in Studio first, or remove it."
                ));
            }
        }
    }
    Ok(())
}

/// Discover until every output in `ids` with a filter has loaded it.
fn wait_for_filters<B: Backend + ?Sized>(
    backend: &B,
    endpoint: SocketAddr,
    ids: &[String],
    mut current: Snapshot,
) -> Result<Snapshot, String> {
    let deadline = Instant::now() + FILTER_LOAD_TIMEOUT;
    loop {
        let mut pending = false;
        for output in current.outputs.iter().filter(|o| ids.contains(&o.id)) {
            match output.filter.as_ref().map(|f| f.state) {
                Some(FilterLoadState::Error) => {
                    return Err(format!(
                        "Omniphony could not use {}'s filter: {}",
                        output.name,
                        output
                            .filter
                            .as_ref()
                            .and_then(|f| f.error.as_deref())
                            .unwrap_or_default()
                    ));
                }
                Some(FilterLoadState::Pending) => pending = true,
                _ => {}
            }
        }
        if !pending {
            return Ok(current);
        }
        if Instant::now() >= deadline {
            return Err("Omniphony is still loading the filters; refresh in a moment.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
        current = backend.discover(endpoint)?;
    }
}

/// Take `a`'s correction to `to`: Apply, the comparison, or Undo. Revision-checked and
/// live only, until Studio's Save. A failed or unacknowledged write is settled by
/// reading the renderer back.
pub fn apply<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    to: Target,
) -> Result<Snapshot, String> {
    let (live, version) = backend.discover_versioned(a.endpoint)?;
    apply_readiness(a, &live, version, to)?;
    let mut c = a.correction.clone().ok_or("no correction")?;
    let state = c.live_state(&live);
    let first = to == Target::Corrected && state == Live::Original;
    if first {
        c.previous = record_previous(a, &c, &live)?;
        c.subwoofer = false;
    }
    let subwoofer = match to {
        Target::Corrected => first && with_subwoofer(a, &c),
        // Whatever of the subwoofer this correction changed, a partial Apply included.
        Target::Original => c.subwoofer && a.bass.is_some(),
        Target::Compare => false,
    };
    let bass = a.bass.clone();
    // The speakers' filters, and the subwoofer's with its integration.
    let mut filters = vec![];
    for filter in &c.filters {
        let taps = match to {
            Target::Corrected => Some(load(&filter.taps)?),
            _ => {
                let previous = c
                    .previous_of(&filter.output_id)
                    .ok_or("the original filters were not recorded")?;
                match &previous.filter {
                    Some(p) => Some(load(&p.taps)?),
                    None if to == Target::Compare => {
                        Some((c.settings.sample_rate, IDENTITY.to_vec()))
                    }
                    None => None,
                }
            }
        };
        filters.push(OutputFilter {
            output_id: filter.output_id.clone(),
            taps,
        });
    }
    let sub = bass.as_ref().map(|b| b.plan.subwoofer.clone());
    if subwoofer && let (Some(b), Some(sub)) = (&bass, &sub) {
        let taps = match (to, b.analysis.as_ref().and_then(|x| x.filter.as_ref())) {
            (Target::Corrected, Some(file)) => {
                Some((file.sample_rate, crate::bass::read_filter(file)?))
            }
            _ => None,
        };
        let has_filter = live
            .outputs
            .iter()
            .any(|o| &o.id == sub && o.filter.is_some());
        if taps.is_some() || has_filter {
            filters.push(OutputFilter {
                output_id: sub.clone(),
                taps,
            });
        }
    }
    let ids: Vec<String> = filters.iter().map(|f| f.output_id.clone()).collect();
    c.uncertain = true;
    c.pending = Some(to);
    if subwoofer {
        a.application_uncertain = true;
        // Persisted before the write: whatever reaches the subwoofer, Undo owes it back,
        // even when the outcome is only read after a reconnect.
        if to == Target::Corrected {
            c.subwoofer = true;
        }
    }
    a.correction = Some(c.clone());
    alignment::save(a)?;
    let endpoint = a.endpoint;
    let result = (|| -> Result<Snapshot, String> {
        let mut current = live.clone();
        // Applied, the filters first and loaded before the subwoofer's delay moves, so
        // every write compares against a settled state; undone, the delay first.
        if to == Target::Original
            && subwoofer
            && let Some(b) = &bass
        {
            current = alignment::move_outputs(backend, endpoint, &current, &b.snapshot)?;
        }
        current = backend.set_output_filters(endpoint, current, filters)?;
        current = wait_for_filters(backend, endpoint, &ids, current)?;
        if to == Target::Corrected
            && subwoofer
            && let Some(target) = bass.as_ref().and_then(|b| b.target())
        {
            current = alignment::move_outputs(backend, endpoint, &current, &target)?;
        }
        Ok(current)
    })();
    let bass_expected = match to {
        Target::Corrected => LiveState::Applied,
        _ => LiveState::Original,
    };
    let reached = |s: &Snapshot| {
        c.matches(s, to)
            && (!subwoofer
                || bass
                    .as_ref()
                    .is_some_and(|b| b.live_state(s) == bass_expected))
    };
    match result {
        Ok(snapshot) if reached(&snapshot) => {
            let mut c = c;
            c.uncertain = false;
            c.pending = None;
            match to {
                Target::Corrected if first => {
                    c.applied = Some(snapshot.clone());
                    c.subwoofer = subwoofer;
                    c.verification = None;
                    c.verification_note = None;
                }
                Target::Corrected | Target::Compare => c.applied = Some(snapshot.clone()),
                Target::Original => {
                    c.applied = None;
                    c.subwoofer = false;
                    c.verification = None;
                    c.verification_note = None;
                }
            }
            if subwoofer {
                a.application_uncertain = false;
                if let Some(b) = &mut a.bass {
                    b.applied = (to == Target::Corrected).then(|| snapshot.clone());
                    b.verification = None;
                    b.verification_note = None;
                }
            }
            a.correction = Some(c);
            alignment::save(a)?;
            Ok(snapshot)
        }
        outcome => {
            // Read the renderer back: a refusal or a lost reply may have left it in a
            // known state, which settles the uncertainty.
            if let Ok(live) = backend.discover(endpoint) {
                if subwoofer && to == Target::Corrected {
                    let _ = settle_partial_subwoofer(a, &live);
                }
                let _ = alignment::reconcile(a, &live);
            }
            alignment::save(a)?;
            Err(match outcome {
                Err(e) => e,
                Ok(_) => "Omniphony reads back other filters than were sent; check the speakers in Studio.".into(),
            })
        }
    }
}

/// After a failed Apply with the subwoofer, the speakers' filters may have gone
/// through without the subwoofer's delay: record that Undo still owes the subwoofer
/// its filter back.
fn settle_partial_subwoofer(a: &mut Alignment, live: &Snapshot) -> Result<(), String> {
    if let Some(c) = &mut a.correction
        && c.live_state(live) == Live::Corrected
    {
        c.subwoofer = true;
    }
    Ok(())
}

/// Record where `live` stands with `a`'s correction, settling an uncertain write when
/// the renderer shows which way it went. Returns whether that changed the record.
pub fn reconcile(a: &mut Alignment, live: &Snapshot) -> bool {
    // The subwoofer still owes Undo something: its integration applied, or its filter
    // left playing by an Apply whose delay write failed.
    let subwoofer_owed = a.bass.as_ref().is_some_and(|b| {
        b.is_applied()
            || b.analysis
                .as_ref()
                .and_then(|x| x.filter.as_ref())
                .is_some_and(|f| hash_of(live, &b.plan.subwoofer) == Some(Some(f.hash.as_str())))
    });
    let Some(c) = &mut a.correction else {
        return false;
    };
    let before = c.clone();
    // An uncertain write that reached what it asked for, ambiguous hashes included.
    if c.uncertain
        && let Some(to) = c.pending
        && c.matches(live, to)
    {
        c.uncertain = false;
        c.pending = None;
        if to == Target::Original {
            c.applied = None;
            c.verification = None;
            c.verification_note = None;
            c.subwoofer &= subwoofer_owed;
        } else {
            c.applied = Some(live.clone());
        }
        return *c != before;
    }
    match c.live_state(live) {
        Live::Corrected | Live::Comparing if !c.previous.is_empty() => {
            c.uncertain = false;
            if c.applied.as_ref() != Some(live) {
                c.applied = Some(live.clone());
            }
        }
        Live::Original => {
            c.uncertain = false;
            if c.applied.is_some() {
                c.applied = None;
                c.verification = None;
                c.verification_note = None;
            }
            c.subwoofer &= subwoofer_owed;
        }
        _ => {}
    }
    if !c.uncertain {
        c.pending = None;
    }
    *c != before
}

/// Why the applied correction cannot be confirmed now.
pub fn verify_readiness(a: &Alignment, live: &Snapshot) -> Result<(), String> {
    let c = a
        .correction
        .as_ref()
        .ok_or("Compute the correction first.")?;
    if c.uncertain || a.application_uncertain {
        return Err(
            "It is not known whether the last change reached Omniphony. Refresh first.".into(),
        );
    }
    match c.live_state(live) {
        Live::Corrected if c.applied.is_some() => Ok(()),
        Live::Comparing => Err(
            "Switch back to Corrected first: the confirmation measures through the filters.".into(),
        ),
        _ => Err("Apply the correction first.".into()),
    }
}

/// The groups of `a`'s passes reduced to the corrected speakers.
fn groups(a: &Alignment, c: &Correction) -> Vec<Group> {
    positions::groups(a)
        .into_iter()
        .filter_map(|mut g| {
            g.outputs.retain(|id| c.filter_of(id).is_some());
            (!g.outputs.is_empty()).then_some(g)
        })
        .collect()
}

/// The corrected speakers a confirmation measures, in order.
pub fn verified_outputs(a: &Alignment) -> Vec<String> {
    a.correction
        .as_ref()
        .map(|c| groups(a, c).into_iter().flat_map(|g| g.outputs).collect())
        .unwrap_or_default()
}

/// RMS of `a − b` over `band`, their mean removed, on matching points.
fn shape_difference(a: &[f64], b: &[f64], frequencies: &[f64], band: [f64; 2]) -> Option<f64> {
    let d: Vec<f64> = a
        .iter()
        .zip(b)
        .zip(frequencies)
        .filter(|(_, f)| (band[0]..=band[1]).contains(*f))
        .map(|((x, y), _)| x - y)
        .filter(|v| v.is_finite())
        .collect();
    if d.len() < 4 {
        return None;
    }
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    Some((d.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / d.len() as f64).sqrt())
}

/// Measure every corrected speaker again at the main seat through its filter, as the
/// renderer plays now, and compare it with the design: as first measured, predicted and
/// now. With the subwoofer applied by this correction, its sums are confirmed too.
pub fn verify<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, MeasureProgress),
) -> Result<(), String> {
    let live = backend.discover(a.endpoint)?;
    verify_readiness(a, &live)?;
    let mut c = a.correction.clone().ok_or("no correction")?;
    c.verification = None;
    c.verification_note = Some("Not confirmed: the last confirmation did not complete.".into());
    a.correction = Some(c.clone());
    alignment::save(a)?;
    let groups = groups(a, &c);
    let order: Vec<String> = groups.iter().flat_map(|g| g.outputs.clone()).collect();
    let first = order.first().ok_or("nothing is corrected")?.clone();
    let directory = crate::workspace::absolute(&c.directory)
        .join(format!("verification-{}", alignment::stamp()));
    let mut request = alignment::request(
        a.endpoint,
        live.clone(),
        &a.settings,
        first,
        directory.clone(),
    )?;
    // Through the filters, as programme audio plays.
    request.bypass_output_filters = false;
    request.calibration = request
        .calibration
        .map(|_| a.directory.join(room_response::CALIBRATION_FILE));
    let batch = sweep_plan::measure_groups(backend, &request, &groups, cancel, progress)?;
    if let Some(e) = batch
        .error
        .clone()
        .or_else(|| batch.cancelled.then(|| "capture cancelled by user".into()))
    {
        c.verification_note = Some(format!("Not confirmed: {e}"));
        a.correction = Some(c);
        alignment::save(a)?;
        return Err(e);
    }
    let frequencies = c.settings.frequencies();
    let mut speakers = vec![];
    let mut warnings = vec![];
    for (id, take) in &batch.takes {
        let checked = (|| -> Result<SpeakerCheck, String> {
            if !take.accepted {
                return Err(take
                    .rejection_reason
                    .clone()
                    .unwrap_or_else(|| "rejected take".into()));
            }
            let d = c
                .output_design(id)
                .ok_or("its design's curves are missing")?;
            let now = room_correction::read(&take.directory, id, true)?;
            let now = design::smoothed_response(&now, &c.settings).map_err(|e| e.to_string())?;
            let row = a
                .rows
                .iter()
                .find(|r| &r.output_id == id)
                .ok_or("not in the alignment")?;
            let main = row
                .take_folders
                .first()
                .ok_or("not measured at the main seat")?;
            let before = room_correction::read(&crate::workspace::absolute(main), id, true)?;
            let before =
                design::smoothed_response(&before, &c.settings).map_err(|e| e.to_string())?;
            let [first, predicted] =
                design::deviation(&d, &before, &c.settings).ok_or("outside its band")?;
            let measured =
                design::rms_from_target(&d, &now, &c.settings).ok_or("outside its band")?;
            let curves = d.curves.as_ref().ok_or("no curves")?;
            // The prediction at the main seat on the design grid: its first measurement
            // through the filter.
            let mut predicted_db = vec![f64::NAN; frequencies.len()];
            let mut measured_db = vec![f32::NAN; curves.frequency_hz.len()];
            for (i, f) in curves.frequency_hz.iter().enumerate() {
                if let Some(k) = frequencies.iter().position(|g| g == f) {
                    predicted_db[k] = before[k] + f64::from(curves.filter_db[i]);
                    measured_db[i] = now[k] as f32;
                }
            }
            let band = d.evaluation_band_hz.ok_or("no band")?;
            let from_prediction = shape_difference(&now, &predicted_db, &frequencies, band)
                .ok_or("outside its band")?;
            Ok(SpeakerCheck {
                output_id: id.clone(),
                deviation_db: [first, predicted, measured],
                from_prediction_db: from_prediction,
                measured_db,
            })
        })();
        match checked {
            Ok(check) => speakers.push(check),
            Err(e) => warnings.push(format!("{id}: {e}")),
        }
    }
    let verification = Verification {
        directory: directory.clone(),
        speakers,
        warnings,
    };
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    write_json(&directory.join(VERIFICATION), &verification).map_err(|e| e.to_string())?;
    c.verification = Some(verification);
    c.verification_note = None;
    a.correction = Some(c.clone());
    alignment::save(a)?;
    if c.subwoofer
        && let Some(b) = &a.bass
        && b.is_applied()
    {
        let offset = order.len();
        crate::bass::verify(backend, a, cancel, &mut |i, p| progress(offset + i, p))?;
    }
    Ok(())
}
