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
}
impl PositionCapture {
    /// Every speaker of the alignment has an accepted take here.
    pub fn complete(&self, a: &Alignment) -> bool {
        self.error.is_none()
            && a.rows.iter().all(|r| {
                self.takes
                    .iter()
                    .any(|t| t.output_id == r.output_id && t.accepted)
            })
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
    if !a
        .pass_reports
        .as_ref()
        .is_some_and(|p| p.len() == 2 && p.iter().all(|pass| pass.error.is_none()))
        || a.rows.iter().any(|r| r.take_folders.len() != 2)
    {
        return Err("Complete an alignment at the main seat first.".into());
    }
    match a.live_state(live) {
        LiveState::Original => Ok(()),
        LiveState::Applied => Err("Undo the applied correction first: other positions are measured with the settings of the main seat's passes. Apply it again afterwards.".into()),
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
    let mut current = live;
    // The state the passes measured under: zeroed when they measured from zero.
    if let Some(zeroed) = &a.zeroed_snapshot {
        current = alignment::move_outputs(backend, a.endpoint, &current, zeroed).map_err(|error| {
            format!("Setting the measured speakers to 0 dB and 0 ms failed ({error}); nothing was measured. Check their gains and delays in Studio.")
        })?;
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
    let restored = alignment::put_back(backend, a, &current);
    let batch = measured?;
    restored?;
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
    };
    a.positions.retain(|p| p.step != capture.step);
    a.positions.push(capture);
    a.positions
        .sort_by_key(|p| GUIDE.iter().position(|s| s.id == p.step));
    alignment::save(a)?;
    Ok(batch.takes)
}
