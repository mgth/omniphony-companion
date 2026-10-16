//! Restore display reports, never replay hardware operations or promote evidence.
use crate::{
    alignment,
    controller::{Settings, State, Take},
    files::read_json,
    measurement::TakeOutcome,
    review::SavedReview,
};
use omniphony_adapter::{Snapshot, Sweep};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MAX_TAKES: usize = 100;
const REPORT_BUDGET: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeReference {
    pub output_id: String,
    pub revision: String,
    pub settings: Settings,
    pub result: Result<PathBuf, String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceIndex {
    pub takes: Vec<TakeReference>,
    pub snapshot: Option<Snapshot>,
    pub alignment: Option<PathBuf>,
    pub review_file: Option<PathBuf>,
    pub error: Option<String>,
}
impl WorkspaceIndex {
    pub fn from_state(state: &State) -> Self {
        Self {
            takes: state
                .takes
                .iter()
                .rev()
                .take(MAX_TAKES)
                .rev()
                .map(|t| TakeReference {
                    output_id: t.output_id.clone(),
                    revision: t.revision.clone(),
                    settings: t.settings.clone(),
                    result: t
                        .outcome
                        .as_ref()
                        .map(|o| absolute(&o.directory))
                        .map_err(Clone::clone),
                })
                .collect(),
            snapshot: state
                .snapshot
                .clone()
                .or_else(|| state.saved_snapshot.clone()),
            alignment: state
                .alignment
                .as_ref()
                .map(|a| absolute(&a.directory))
                .or_else(|| {
                    state
                        .restored_index
                        .as_ref()
                        .and_then(|i| i.alignment.clone())
                }),
            review_file: state
                .review
                .as_ref()
                .map(|r| absolute(&r.report_file))
                .or_else(|| {
                    state
                        .restored_index
                        .as_ref()
                        .and_then(|i| i.review_file.clone())
                }),
            error: state.error.clone(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.takes.len() > MAX_TAKES
            || self
                .snapshot
                .as_ref()
                .is_some_and(|s| s.outputs.len() > 512)
        {
            return Err("saved workspace exceeds its result/layout limits".into());
        }
        let paths = self
            .takes
            .iter()
            .filter_map(|t| t.result.as_ref().ok())
            .chain(self.alignment.iter())
            .chain(self.review_file.iter());
        if paths
            .into_iter()
            .any(|p| !p.is_absolute() || p.as_os_str().len() > 4096)
        {
            return Err("workspace paths must be bounded absolute paths".into());
        }
        Ok(())
    }
}
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

#[derive(Default)]
pub struct RestoredWorkspace {
    pub index: WorkspaceIndex,
    pub takes: Vec<Take>,
    pub snapshot: Option<Snapshot>,
    pub alignment: Option<alignment::Alignment>,
    pub review: Option<SavedReview>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub imported: bool,
}

#[derive(Deserialize)]
struct Identity {
    output_id: String,
    microphone_id: String,
    position_id: String,
    reference_output_id: String,
    signal: Sweep,
}
#[derive(Deserialize)]
struct DisplayReport {
    schema_version: u32,
    accepted_for_analysis: bool,
    rejection_reason: Option<String>,
    timing: Option<measurement_capture::timing::TimingReport>,
    diagnostics: Option<measurement_capture::diagnostics::AcquisitionDiagnostics>,
    impulse: Option<measurement_capture::impulse::ImpulseReport>,
    impulse_error: Option<String>,
    #[serde(default)]
    capture: Option<CaptureDetails>,
}
#[derive(Default, Deserialize)]
struct CaptureDetails {
    device: Option<String>,
    input_channel: Option<u16>,
    sample_rate: Option<u32>,
}
#[derive(Deserialize)]
struct ImportReport {
    capture: Option<CaptureDetails>,
}
fn read_display_json<T: serde::de::DeserializeOwned>(path: &Path) -> crate::AppResult<T> {
    // Dense plots can exceed the core's 8 MiB observation limit. This reader is
    // only for display caches, with a separate cumulative startup budget.
    crate::files::read_json_limited(path, 32 * 1024 * 1024)
}

fn report_size(path: &Path, remaining: &mut u64) -> Result<(), String> {
    let bytes = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or("workspace report memory budget exceeded")?;
    Ok(())
}
fn load_take(
    reference: &TakeReference,
    folder: &Path,
    remaining: &mut u64,
) -> Result<TakeOutcome, String> {
    let path = folder.join("capture-report.json");
    report_size(&path, remaining)?;
    let mut report: DisplayReport = read_display_json(&path).map_err(|e| e.to_string())?;
    let identity: Identity = read_json(&folder.join("stimulus.json")).map_err(|e| e.to_string())?;
    let snapshot: Snapshot = read_json(&folder.join("snapshot.json")).map_err(|e| e.to_string())?;
    if !(1..=3).contains(&report.schema_version)
        || identity.output_id != reference.output_id
        || snapshot.revision != reference.revision
        || identity.microphone_id != reference.settings.microphone_id
        || identity.position_id != reference.settings.position_id
        || Some(&identity.reference_output_id) != reference.settings.reference_output.as_ref()
        || identity.signal != reference.settings.sweep
        || report.accepted_for_analysis == report.rejection_reason.is_some()
    {
        return Err("saved result identity or verdict is inconsistent".into());
    }
    if report.capture.is_some() && !folder.join("recording.wav").is_file() {
        return Err("saved recording is missing; its report was retained on disk".into());
    }
    // These are cached views. Only explicit raw-audio review can recompute them;
    // alignment application uses its own independent, revalidated session file.
    if let Some(t) = &mut report.timing {
        t.valid_for_correction = false;
    }
    if let Some(d) = &mut report.diagnostics {
        d.valid_for_correction = false;
    }
    if let Some(i) = &mut report.impulse {
        i.valid_for_correction = false;
    }
    Ok(TakeOutcome {
        directory: folder.to_owned(),
        accepted: report.accepted_for_analysis,
        cancelled: report
            .rejection_reason
            .as_deref()
            .is_some_and(|r| r.contains("cancelled")),
        rejection_reason: report.rejection_reason,
        diagnostics: report.diagnostics,
        timing: report.timing,
        impulse: report.impulse.map(Arc::new),
        impulse_error: report.impulse_error,
    })
}

/// Bounded, read-only report loading on the controller's background worker.
pub fn restore(
    index: Option<WorkspaceIndex>,
    settings: &Settings,
) -> Result<RestoredWorkspace, String> {
    let imported = index.is_none();
    let (index, warnings) = match index {
        Some(index) => (index, vec![]),
        None => import_existing(settings),
    };
    index.validate()?;
    let mut restored = RestoredWorkspace {
        index: index.clone(),
        snapshot: index.snapshot,
        error: index.error,
        imported,
        warnings,
        ..Default::default()
    };
    let mut budget = REPORT_BUDGET;
    // Newest results get priority if the bounded report budget is exhausted.
    for reference in index.takes.into_iter().rev() {
        let outcome = match reference.result.as_ref() {
            Err(error) => Err(error.clone()),
            Ok(folder) => match load_take(&reference, folder, &mut budget) {
                Ok(take) => Ok(take),
                Err(e) => {
                    let message = format!(
                        "{}: saved result unavailable: {e} ({})",
                        reference.output_id,
                        folder.display()
                    );
                    restored.warnings.push(message.clone());
                    Ok(TakeOutcome {
                        directory: folder.clone(),
                        accepted: false,
                        cancelled: false,
                        rejection_reason: Some(message),
                        diagnostics: None,
                        timing: None,
                        impulse: None,
                        impulse_error: None,
                    })
                }
            },
        };
        restored.takes.push(Take {
            output_id: reference.output_id,
            revision: reference.revision,
            settings: reference.settings,
            outcome,
        });
    }
    restored.takes.reverse();
    if let Some(path) = index.alignment {
        match alignment::load(&path) {
            Ok(a) => restored.alignment = Some(a),
            Err(e) => restored
                .warnings
                .push(format!("Alignment unavailable ({}): {e}", path.display())),
        }
    }
    if let Some(path) = index.review_file {
        let mut load = || -> Result<SavedReview, String> {
            report_size(&path, &mut budget)?;
            let mut review: SavedReview = read_display_json(&path).map_err(|e| e.to_string())?;
            if !(1..=2).contains(&review.schema_version) {
                return Err("unsupported saved review".into());
            }
            review.directory = path.parent().ok_or("missing review directory")?.to_owned();
            if !review.directory.join("recording.wav").is_file() {
                return Err("review recording is missing".into());
            }
            review.report_file = path.clone();
            review.valid_for_correction = false;
            Ok(review)
        };
        match load() {
            Ok(r) => restored.review = Some(r),
            Err(e) => restored
                .warnings
                .push(format!("Review unavailable ({}): {e}", path.display())),
        }
    }
    Ok(restored)
}

/// One-time upgrade from settings-only versions. Never recurse outside known
/// session directories or follow directory symlinks, and cap discovery work.
fn import_existing(settings: &Settings) -> (WorkspaceIndex, Vec<String>) {
    let mut warnings = vec![];
    fn visit(
        path: &Path,
        depth: usize,
        remaining: &mut usize,
        takes: &mut Vec<(std::time::SystemTime, PathBuf)>,
        alignments: &mut Vec<(std::time::SystemTime, PathBuf)>,
    ) {
        if depth > 4 || *remaining == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            if *remaining == 0 {
                break;
            }
            *remaining -= 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_file()
                && matches!(
                    name.as_ref(),
                    "capture-report.json" | "alignment-state.json"
                )
            {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if name == "capture-report.json" {
                    takes.push((modified, path.to_owned()));
                } else {
                    alignments.push((modified, path.to_owned()));
                }
            } else if kind.is_dir()
                && ["take-", "batch-", "alignment-", "pass-"]
                    .iter()
                    .any(|p| name.starts_with(p))
            {
                visit(&entry.path(), depth + 1, remaining, takes, alignments);
            }
        }
    }
    let mut takes = vec![];
    let mut alignments = vec![];
    visit(
        &absolute(Path::new(&settings.output_root)),
        0,
        &mut 4096,
        &mut takes,
        &mut alignments,
    );
    takes.sort();
    alignments.sort();
    let mut index = WorkspaceIndex {
        alignment: alignments.pop().map(|(_, p)| p),
        ..Default::default()
    };
    for (_, path) in takes
        .into_iter()
        .rev()
        .take(MAX_TAKES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let read = || -> crate::AppResult<_> {
            let identity: Identity = read_json(&path.join("stimulus.json"))?;
            let snapshot: Snapshot = read_json(&path.join("snapshot.json"))?;
            let report: ImportReport = read_display_json(&path.join("capture-report.json"))?;
            let capture = report.capture.unwrap_or_default();
            let take_settings = Settings {
                device: capture.device,
                input_channel: capture.input_channel.unwrap_or(1),
                sample_rate: capture.sample_rate.unwrap_or(48000),
                microphone_id: identity.microphone_id,
                position_id: identity.position_id,
                reference_output: Some(identity.reference_output_id),
                sweep: identity.signal,
                calibration_path: if path.join("microphone-calibration.txt").is_file() {
                    path.join("microphone-calibration.txt")
                        .display()
                        .to_string()
                } else {
                    String::new()
                },
                output_root: settings.output_root.clone(),
            };
            Ok((
                TakeReference {
                    output_id: identity.output_id,
                    revision: snapshot.revision.clone(),
                    settings: take_settings,
                    result: Ok(path.clone()),
                },
                snapshot,
            ))
        };
        match read() {
            Ok((reference, snapshot)) => {
                index.takes.push(reference);
                index.snapshot = Some(snapshot);
            }
            Err(e) => warnings.push(format!(
                "Could not import saved measurement ({}): {e}",
                path.display()
            )),
        }
    }
    (index, warnings)
}
