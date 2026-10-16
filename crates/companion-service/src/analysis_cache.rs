//! Bounded discovery of saved analyses. Cache reads never rerun DSP or authorize correction.
use crate::{
    controller::Take,
    files, quality,
    review::SavedReview,
    workspace::{self, TakeReference},
};
use omniphony_adapter::Sweep;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub type Index = BTreeMap<PathBuf, CachedAnalysis>;
const FILE_LIMIT: u64 = 32 * 1024 * 1024;
const SCAN_BUDGET: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
struct Arrival {
    relative_arrival_ms: Option<f64>,
}
/// Only small presentation fields are retained; dense curves load on selection.
#[derive(Debug, Clone, Deserialize)]
pub struct CachedAnalysis {
    #[serde(skip)]
    pub report_file: PathBuf,
    #[serde(skip)]
    pub directory: PathBuf,
    schema_version: u32,
    pub output_id: String,
    microphone_id: String,
    position_id: String,
    reference_output_id: String,
    input_channel: u16,
    capture_sample_rate: u32,
    signal: Sweep,
    pub quality: Option<quality::Report>,
    pub analysis_error: Option<String>,
    pub impulse_error: Option<String>,
    impulse: Option<Arrival>,
}
impl CachedAnalysis {
    pub fn from_review(r: &SavedReview) -> Self {
        Self {
            report_file: workspace::absolute(&r.report_file),
            directory: workspace::absolute(&r.directory),
            schema_version: r.schema_version,
            output_id: r.output_id.clone(),
            microphone_id: r.microphone_id.clone(),
            position_id: r.position_id.clone(),
            reference_output_id: r.reference_output_id.clone(),
            input_channel: r.input_channel,
            capture_sample_rate: r.capture_sample_rate,
            signal: r.signal,
            quality: r.quality.clone(),
            analysis_error: r.analysis_error.clone(),
            impulse_error: r.impulse_error.clone(),
            impulse: r.impulse.as_ref().map(|i| Arrival {
                relative_arrival_ms: i.relative_arrival_ms,
            }),
        }
    }
    pub fn arrival_ms(&self) -> Option<f64> {
        self.impulse.as_ref()?.relative_arrival_ms
    }
    pub fn has_response(&self) -> bool {
        self.impulse.is_some()
    }
    fn matches(&self, t: &TakeReference) -> bool {
        matches!(self.schema_version, 1 | 2)
            && t.result
                .as_ref()
                .is_ok_and(|d| workspace::absolute(d) == self.directory)
            && self.matches_settings(&t.output_id, &t.settings)
    }
    fn matches_settings(&self, output_id: &str, settings: &crate::controller::Settings) -> bool {
        self.output_id == output_id
            && self.microphone_id == settings.microphone_id
            && self.position_id == settings.position_id
            && Some(&self.reference_output_id) == settings.reference_output.as_ref()
            && self.input_channel == settings.input_channel
            && self.capture_sample_rate == settings.sample_rate
            && self.signal == settings.sweep
    }
    pub fn matches_take(&self, take: &Take) -> bool {
        self.matches_settings(&take.output_id, &take.settings)
            && take
                .outcome
                .as_ref()
                .is_ok_and(|o| workspace::absolute(&o.directory) == self.directory)
    }
}

fn validate(summary: &CachedAnalysis, reference: &TakeReference) -> Result<(), String> {
    if !summary.matches(reference) {
        return Err("saved analysis belongs to a different measurement".into());
    }
    let snapshot: omniphony_adapter::Snapshot =
        files::read_json(&summary.directory.join("snapshot.json")).map_err(|e| e.to_string())?;
    if snapshot.revision != reference.revision || !summary.directory.join("recording.wav").is_file()
    {
        return Err("saved analysis source recording or renderer identity is unavailable".into());
    }
    Ok(())
}

/// Inspect only known take folders, with bounded directory traversal and JSON reads.
/// Legacy alignments already wrote analysis-*.json; no migration or new measurement is needed.
pub fn scan(references: &[TakeReference]) -> (Index, Vec<String>) {
    let mut index = Index::new();
    let mut warnings = vec![];
    let mut budget = SCAN_BUDGET;
    for reference in references.iter().rev().take(workspace::MAX_TAKES) {
        let Ok(directory) = &reference.result else {
            continue;
        };
        let directory = workspace::absolute(directory);
        if index.contains_key(&directory) {
            continue;
        }
        let read = || -> Result<Option<PathBuf>, String> {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                return Ok(None);
            };
            let mut newest = None;
            for (n, entry) in entries.enumerate() {
                if n >= 512 {
                    return Err("saved analysis directory exceeds the scan limit".into());
                }
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("analysis-")
                    || !name.ends_with(".json")
                    || !entry.file_type().map_err(|e| e.to_string())?.is_file()
                {
                    continue;
                }
                let stamp = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map_err(|e| e.to_string())?;
                let candidate = (stamp, entry.path());
                if newest.as_ref().is_none_or(|old| &candidate > old) {
                    newest = Some(candidate);
                }
            }
            Ok(newest.map(|(_, p)| p))
        };
        let load = |path: PathBuf, budget: &mut u64| -> Result<CachedAnalysis, String> {
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            *budget = budget
                .checked_sub(size)
                .ok_or("saved analysis scan budget exceeded")?;
            let mut summary: CachedAnalysis =
                files::read_json_limited(&path, FILE_LIMIT).map_err(|e| e.to_string())?;
            summary.report_file = path;
            summary.directory = directory.clone();
            validate(&summary, reference)?;
            Ok(summary)
        };
        match read().and_then(|p| p.map(|p| load(p, &mut budget)).transpose()) {
            Ok(Some(summary)) => {
                index.insert(directory, summary);
            }
            Ok(None) => (),
            Err(e) => warnings.push(format!(
                "{}: latest saved analysis unavailable: {e}",
                reference.output_id
            )),
        }
    }
    (index, warnings)
}

/// Read one explicit cached report. No audio, FFT, network calls or source writes.
pub fn load(path: &Path, take: &Take) -> Result<SavedReview, String> {
    let path = workspace::absolute(path);
    let mut review: SavedReview =
        files::read_json_limited(&path, FILE_LIMIT).map_err(|e| e.to_string())?;
    review.directory = path.parent().ok_or("missing analysis folder")?.to_owned();
    review.report_file = path;
    validate(
        &CachedAnalysis::from_review(&review),
        &TakeReference::from(take),
    )?;
    review.valid_for_correction = false;
    if let Some(timing) = &mut review.timing {
        timing.valid_for_correction = false;
    }
    review.diagnostics.valid_for_correction = false;
    if let Some(ir) = &mut review.impulse {
        std::sync::Arc::make_mut(ir).valid_for_correction = false;
    }
    Ok(review)
}
