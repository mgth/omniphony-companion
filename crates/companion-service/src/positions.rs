//! Guided microphone positions around the main seat, for room correction.
//!
//! An alignment measures delays and levels at one seat, its `position_id`. A correction
//! filter is better designed from several nearby points, so the main seat's responses
//! can be complemented by one pass at each of [`GUIDE`]'s positions. Each pass plays
//! every speaker of the alignment with the sweep, level and reference its rows were
//! measured with, in the renderer state of the alignment's own passes (its outputs at
//! 0 dB and 0 ms when it measured from zero, put back afterwards), so every position is
//! directly comparable with the main seat. Takes keep their stored responses
//! ([`crate::room_response`]) under the position's own `position_id`.
//!
//! These takes never change the alignment's rows, proposal, delays or levels.
use crate::{
    alignment::{self, Alignment, LiveState},
    controller::Backend,
    measurement::{MeasureProgress, TakeOutcome},
    sweep_plan::{self, Group},
};
use omniphony_adapter::Snapshot;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::atomic::AtomicBool};

/// One step of the guided capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Appended to the main seat's `position_id`: `main-seat/left-30cm`.
    pub id: &'static str,
    /// A few words for a list: "30 cm left".
    pub label: &'static str,
    pub instruction: &'static str,
}

/// Five positions around the main seat, each from the main seat itself.
pub const GUIDE: [Step; 5] = [
    Step {
        id: "left-30cm",
        label: "30 cm left",
        instruction: "Move the microphone 30 cm to the left of the main seat.",
    },
    Step {
        id: "right-30cm",
        label: "30 cm right",
        instruction: "Move the microphone 30 cm to the right of the main seat.",
    },
    Step {
        id: "front-30cm",
        label: "30 cm forward",
        instruction: "Move the microphone 30 cm forward from the main seat, toward the screen.",
    },
    Step {
        id: "back-30cm",
        label: "30 cm back",
        instruction: "Move the microphone 30 cm back from the main seat.",
    },
    Step {
        id: "up-15cm",
        label: "15 cm higher",
        instruction: "Put the microphone back above the main seat, 15 cm higher.",
    },
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionTake {
    pub output_id: String,
    pub take_folder: PathBuf,
    pub accepted: bool,
    pub rejection_reason: Option<String>,
    /// Whether the take stored its unweighted response.
    pub response: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionCapture {
    /// The guide step's `id`.
    pub step: String,
    /// The takes' `position_id`: the main seat's, then `/` and the step.
    pub position_id: String,
    pub instruction: String,
    pub directory: PathBuf,
    pub takes: Vec<PositionTake>,
    /// Why the pass stopped before every speaker was measured.
    pub error: Option<String>,
    /// The outputs whose correction filter played: empty when the pass held the filters
    /// off. `None` on captures from before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filtered_outputs: Option<Vec<String>>,
}
impl PositionCapture {
    /// Every speaker of the alignment has an accepted take here.
    pub fn complete(&self, a: &Alignment) -> bool {
        self.error.is_none()
            && a.rows.iter().all(|r| {
                self.takes
                    .iter()
                    .any(|t| t.output_id == r.output_id && t.usable())
            })
    }
    /// Speakers with a usable take here.
    pub fn usable_count(&self) -> usize {
        self.takes.iter().filter(|t| t.usable()).count()
    }
}
impl PositionTake {
    /// Accepted and with its response stored: what the design step needs.
    pub fn usable(&self) -> bool {
        self.accepted && self.response
    }
}

/// The position id of `step` for an alignment measured at `main`.
pub fn position_id(main: &str, step: &Step) -> String {
    format!("{main}/{}", step.id)
}

/// The capture of `step`, if it was measured for `a`.
pub fn captured<'a>(a: &'a Alignment, step: &Step) -> Option<&'a PositionCapture> {
    a.positions.iter().find(|p| p.step == step.id)
}

/// The first step not yet measured completely, if any.
pub fn next_step(a: &Alignment) -> Option<usize> {
    GUIDE
        .iter()
        .position(|step| captured(a, step).is_none_or(|p| !p.complete(a)))
}

/// The alignment's speakers grouped as its passes measured them.
pub fn groups(a: &Alignment) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for row in &a.rows {
        let sweep = row.sweep(&a.settings);
        let Some(reference) = row.reference(&a.settings) else {
            continue;
        };
        match groups
            .iter_mut()
            .find(|g| g.sweep == sweep && g.reference == reference)
        {
            Some(group) => group.outputs.push(row.output_id.clone()),
            None => groups.push(Group {
                sweep,
                reference: reference.to_owned(),
                outputs: vec![row.output_id.clone()],
            }),
        }
    }
    groups
}

/// Why `a` cannot be measured at more positions now, with `live` the renderer.
pub fn readiness(a: &Alignment, live: &omniphony_adapter::Snapshot) -> Result<(), String> {
    if a.application_uncertain {
        return Err(
            "It is not known whether the last Apply reached Omniphony. Refresh first.".into(),
        );
    }
    if a.physical_levels_changed {
        return Err(
            "Speaker volumes may have been turned since these passes. Measure a new alignment at the main seat first."
                .into(),
        );
    }
    if !a
        .pass_reports
        .as_ref()
        .is_some_and(|p| p.len() == 2 && p.iter().all(|pass| pass.error.is_none()))
        || a.rows.iter().any(|r| r.take_folders.len() != 2)
    {
        return Err("Complete an alignment at the main seat first.".into());
    }
    // Applied, the speakers are put in the passes' state for the position and back.
    match a.live_state(live) {
        LiveState::Original | LiveState::Applied => Ok(()),
        LiveState::Other => Err("Omniphony's settings changed since this alignment was measured. Measure a new alignment first.".into()),
    }
}

/// Measure `a`'s speakers once at guide step `step` and record the capture in `a`,
/// replacing an earlier one of that step (its folder stays on disk). `progress`
/// receives the index of the speaker in `a.rows`. Returns the takes.
pub fn measure<B: Backend + ?Sized>(
    backend: &B,
    a: &mut Alignment,
    step: usize,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, MeasureProgress),
) -> Result<Vec<(String, TakeOutcome)>, String> {
    let guide = GUIDE.get(step).ok_or("unknown microphone position")?;
    let live = backend.discover(a.endpoint)?;
    readiness(a, &live)?;
    let groups = groups(a);
    let first = groups
        .first()
        .and_then(|g| g.outputs.first())
        .ok_or("the alignment has no speakers")?
        .clone();
    let order: Vec<_> = groups
        .iter()
        .flat_map(|g| g.outputs.iter())
        .map(|id| a.rows.iter().position(|r| &r.output_id == id).unwrap_or(0))
        .collect();
    // The state the passes measured under: zeroed when they measured from zero, the
    // original settings when the alignment is applied now. Applied, the speakers go
    // back to the state found; otherwise to their original settings.
    let applied = a.live_state(&live) == LiveState::Applied;
    let found = live.clone();
    let back = if applied {
        found.clone()
    } else {
        a.snapshot.clone()
    };
    let mut current = live;
    if applied || a.zeroed_snapshot.is_some() {
        let measured = a.measured_state().clone();
        current = match alignment::move_outputs(backend, a.endpoint, &current, &measured) {
            Ok(current) => current,
            Err(error) => {
                // The write may have gone through (a lost reply): put the settings back.
                let settled = settle(backend, a, &back);
                return Err(format!(
                    "Setting the measured speakers to 0 dB and 0 ms failed ({error}); nothing was measured.{}",
                    if settled {
                        ""
                    } else {
                        " Their settings could not be confirmed: check their gains and delays in Studio, then refresh."
                    }
                ));
            }
        };
    }
    let directory = a
        .directory
        .join(format!("position-{}-{}", guide.id, alignment::stamp()));
    let mut request = alignment::request(
        a.endpoint,
        current.clone(),
        &a.settings,
        first,
        directory.clone(),
    )?;
    request.position_id = position_id(&a.settings.position_id, guide);
    request.calibration = request
        .calibration
        .map(|_| a.directory.join(crate::room_response::CALIBRATION_FILE));
    let measured = sweep_plan::measure_groups(backend, &request, &groups, cancel, &mut |i, p| {
        progress(order[i.min(order.len() - 1)], p)
    });
    // Stopped or failed, the outputs go back to their settings all the same.
    let restored = if applied {
        alignment::move_outputs(backend, a.endpoint, &current, &found)
            .map(|live| {
                let _ = alignment::reconcile(a, &live);
            })
            .map_err(|error| format!("The speakers were set to the alignment's original settings for this position and could not be put back to the corrected ones ({error})."))
    } else {
        alignment::put_back(backend, a, &current)
    };
    if let Err(error) = restored {
        // Another attempt from whatever the renderer reads back; else say it is unknown.
        if !settle(backend, a, &back) {
            return Err(format!(
                "{error} Their settings could not be confirmed: check them in Studio, then refresh."
            ));
        }
    }
    let batch = measured?;
    let capture = PositionCapture {
        step: guide.id.into(),
        position_id: request.position_id.clone(),
        instruction: guide.instruction.into(),
        directory,
        takes: batch
            .takes
            .iter()
            .map(|(id, take)| PositionTake {
                output_id: id.clone(),
                take_folder: take.directory.clone(),
                accepted: take.accepted,
                rejection_reason: take.rejection_reason.clone(),
                response: take.directory.join(crate::room_response::FILE).is_file(),
            })
            .collect(),
        error: batch
            .error
            .clone()
            .or_else(|| batch.cancelled.then(|| "capture cancelled by user".into())),
        filtered_outputs: Some(alignment::filtered_outputs(&request)),
    };
    a.positions.retain(|p| p.step != capture.step);
    a.positions.push(capture);
    a.positions
        .sort_by_key(|p| GUIDE.iter().position(|s| s.id == p.step));
    alignment::save(a)?;
    Ok(batch.takes)
}

/// After a failed write around a position, bring the renderer back to `back` (the
/// alignment's original settings, or the corrected ones it was found with), as read
/// back. Returns whether that is confirmed; otherwise the alignment is saved as
/// uncertain, which blocks every action until a refresh finds the renderer in a known
/// state.
fn settle<B: Backend + ?Sized>(backend: &B, a: &mut Alignment, back: &Snapshot) -> bool {
    let restored = backend.discover(a.endpoint).ok().and_then(|live| {
        if omniphony_adapter::same_installation(&live, back) {
            Some(live)
        } else {
            alignment::move_outputs(backend, a.endpoint, &live, back)
                .ok()
                // A lost reply: the write may have gone through all the same.
                .or_else(|| {
                    backend
                        .discover(a.endpoint)
                        .ok()
                        .filter(|live| omniphony_adapter::same_installation(live, back))
                })
        }
    });
    match restored {
        Some(live) => {
            if omniphony_adapter::same_installation(back, &a.snapshot) {
                if live.revision != a.snapshot.revision {
                    a.restored_snapshot = Some(live);
                }
            } else {
                let _ = alignment::reconcile(a, &live);
            }
            let _ = alignment::save(a);
            true
        }
        None => {
            a.application_uncertain = true;
            let _ = alignment::save(a);
            false
        }
    }
}
