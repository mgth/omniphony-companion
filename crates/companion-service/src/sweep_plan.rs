//! Which sweep each output is measured with.
//!
//! A room-correction sweep covers each output's own range: from its declared low limit
//! (at least 20 Hz) to 20 kHz, its declared high limit, or 0.45 times the lower of the
//! render and capture rates. The renderer refuses a sweep that starts below an output's
//! `freq_low_hz`, so the range cannot reach below it without a protocol change.
//!
//! The renderer plays one sweep for the reference and every target of a sequence, so the
//! outputs are grouped by the sweep they share, one sequence per group. The reference
//! plays every group's sweep: a broadband output's range is narrowed to what the
//! reference can play too. A bass-only output (a subwoofer, whose sweep stops below
//! 5 kHz) keeps its own range; when the reference cannot play it, the output is its own
//! reference, since the alignment measures no timing or level for it anyway.
use crate::{
    batch::BatchOutcome,
    controller::{Backend, Settings},
    measurement::{MeasureProgress, MeasureRequest},
};
use measurement_capture::sequence;
use omniphony_adapter::{
    Snapshot, Sweep,
    protocol::{self, Output},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepMode {
    /// Each output over its own range, 3 s long when the sequence allows.
    #[default]
    RoomCorrection,
    /// The duration and range of [`Settings::sweep`], for every output.
    Custom,
}

/// Longest room-correction sweep; shortened when a sequence would not fit.
pub const ROOM_SWEEP_MS: u32 = 3000;
/// Shortest room-correction sweep the plan shortens to.
pub const MIN_ROOM_SWEEP_MS: u32 = 1000;
pub const ROOM_HIGH_HZ: f32 = 20000.0;
/// A sweep ending below this is bass-only: the arrival review needs 5 kHz.
pub const BROADBAND_END_HZ: f32 = 5000.0;
/// Highest playback peak the contract allows.
pub const MAX_PEAK_DBFS: f32 = -12.0;
pub const MIN_PEAK_DBFS: f32 = -60.0;
/// The renderer's ceiling on peak plus master and output gains.
const OUTPUT_CEILING_DBFS: f32 = -6.0;

/// Outputs measured in one sequence, with the sweep and the reference they share.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub sweep: Sweep,
    pub reference: String,
    pub outputs: Vec<String>,
}
impl Group {
    pub fn bass_only(&self) -> bool {
        bass_only(self.sweep)
    }
}
pub fn bass_only(sweep: Sweep) -> bool {
    sweep.end_hz < BROADBAND_END_HZ
}

/// The range a room-correction sweep covers on `output`.
pub fn range(output: &Output, render_rate: u32, capture_rate: u32) -> [f32; 2] {
    let nyquist = render_rate.min(capture_rate) as f32 * 0.45;
    [
        output.freq_low_hz.unwrap_or(20.0).max(20.0),
        ROOM_HIGH_HZ
            .min(nyquist)
            .min(output.freq_high_hz.unwrap_or(f32::MAX)),
    ]
}

/// The highest peak `output` accepts under the renderer's gain ceiling.
pub fn ceiling(snapshot: &Snapshot, output: &Output) -> f32 {
    (OUTPUT_CEILING_DBFS - snapshot.master_gain_db - output.gain_db).min(MAX_PEAK_DBFS)
}

fn find<'a>(snapshot: &'a Snapshot, id: &str) -> Result<&'a Output, String> {
    snapshot
        .outputs
        .iter()
        .find(|o| o.id == id)
        .ok_or_else(|| format!("{id} is absent from this configuration. Refresh the connection."))
}

/// Group `outputs` by the sweep they are measured with. `peaks` are the automatic
/// levels by output ID; without them, or for an output they lack, the level is
/// `settings.sweep.peak_dbfs`. A group plays at its quietest member's level, the
/// reference included, under every member's gain ceiling.
pub fn plan(
    settings: &Settings,
    snapshot: &Snapshot,
    outputs: &[String],
    peaks: Option<&BTreeMap<String, f32>>,
) -> Result<Vec<Group>, String> {
    let reference_id = settings
        .reference_output
        .as_deref()
        .ok_or("Choose a timing reference speaker.")?;
    let reference = find(snapshot, reference_id)?;
    let reference_range = range(reference, snapshot.sample_rate, settings.sample_rate);
    let mut groups: Vec<Group> = Vec::new();
    for id in outputs {
        let output = find(snapshot, id)?;
        let (band, reference) = match settings.sweep_mode {
            SweepMode::Custom => (
                [settings.sweep.start_hz, settings.sweep.end_hz],
                reference_id,
            ),
            SweepMode::RoomCorrection => {
                let own = range(output, snapshot.sample_rate, settings.sample_rate);
                if own[1] >= BROADBAND_END_HZ {
                    let shared = [
                        own[0].max(reference_range[0]),
                        own[1].min(reference_range[1]),
                    ];
                    if shared[1] < BROADBAND_END_HZ || shared[0] >= shared[1] {
                        return Err(format!(
                            "The timing reference cannot play {}'s range; choose a broadband reference.",
                            output.name
                        ));
                    }
                    (shared, reference_id)
                } else if reference_range[0] <= own[0] && reference_range[1] >= own[1] {
                    (own, reference_id)
                } else {
                    (own, id.as_str())
                }
            }
        };
        let sweep = Sweep {
            start_hz: band[0],
            end_hz: band[1],
            ..settings.sweep
        };
        match groups
            .iter_mut()
            .find(|g| g.sweep == sweep && g.reference == reference)
        {
            Some(group) => group.outputs.push(id.clone()),
            None => groups.push(Group {
                sweep,
                reference: reference.to_owned(),
                outputs: vec![id.clone()],
            }),
        }
    }
    for group in &mut groups {
        let mut members = vec![group.reference.as_str()];
        members.extend(group.outputs.iter().map(String::as_str));
        let mut peak = f32::INFINITY;
        let mut limit = f32::INFINITY;
        for id in members {
            let output = find(snapshot, id)?;
            peak = peak.min(
                peaks
                    .and_then(|p| p.get(id).copied())
                    .unwrap_or(settings.sweep.peak_dbfs),
            );
            limit = limit.min(ceiling(snapshot, output));
        }
        group.sweep.peak_dbfs = peak.min(limit).max(MIN_PEAK_DBFS);
        if settings.sweep_mode == SweepMode::RoomCorrection {
            group.sweep.duration_ms = fitting_duration(
                group.sweep,
                snapshot.sample_rate,
                settings.sample_rate,
                group.outputs.len(),
            )?;
        }
    }
    Ok(groups)
}

/// The longest duration up to [`ROOM_SWEEP_MS`], in 100 ms steps, whose sequence fits
/// the renderer's limit and the recording budget.
fn fitting_duration(
    sweep: Sweep,
    render_rate: u32,
    capture_rate: u32,
    targets: usize,
) -> Result<u32, String> {
    (MIN_ROOM_SWEEP_MS..=ROOM_SWEEP_MS)
        .rev()
        .step_by(100)
        .find(|&duration_ms| {
            let sweep = Sweep {
                duration_ms,
                ..sweep
            };
            protocol::sequence_frames(sweep, render_rate, targets).is_ok()
                && sequence::capture_budget(sweep, render_rate, capture_rate, targets).is_ok()
        })
        .ok_or_else(|| {
            "Too many speakers for one room-correction sequence; measure fewer at once.".into()
        })
}

/// The group that measures `output`.
pub fn group_of<'a>(groups: &'a [Group], output: &str) -> Option<&'a Group> {
    groups
        .iter()
        .find(|g| g.outputs.iter().any(|o| o == output))
}

/// `settings` as a take measured in `group` was: its sweep and reference. Saved take
/// identities and the workspace compare against these.
pub fn take_settings(settings: &Settings, group: &Group) -> Settings {
    Settings {
        sweep: group.sweep,
        reference_output: Some(group.reference.clone()),
        ..settings.clone()
    }
}

/// Measure every group as one continuous batch each, in order, and return their takes
/// in the order of `outputs` (the groups' outputs in order). One group measures straight
/// into `request.out_dir`; several measure into `sweep-1`, `sweep-2`… inside it. Stops at
/// the first group that fails, is cancelled or errs, with its error in the outcome; the
/// takes measured so far are kept. Only creating the folder is an error.
/// `progress` receives the index of the speaker in the concatenated output list.
pub fn measure_groups<B: Backend + ?Sized>(
    backend: &B,
    request: &MeasureRequest,
    groups: &[Group],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, MeasureProgress),
) -> Result<BatchOutcome, String> {
    if groups.len() > 1 {
        std::fs::create_dir_all(crate::files::parent(&request.out_dir))
            .map_err(|e| e.to_string())?;
        std::fs::create_dir(&request.out_dir).map_err(|e| e.to_string())?;
    }
    let mut merged = BatchOutcome {
        directory: request.out_dir.clone(),
        takes: vec![],
        cancelled: false,
        error: None,
    };
    let mut offset = 0;
    for (index, group) in groups.iter().enumerate() {
        let child = group_request(request, group, index, groups.len());
        // A group that fails to start keeps the takes of the groups before it.
        let batch =
            match backend.measure_batch(child, group.outputs.clone(), cancel, &mut |i, p| {
                progress(offset + i, p)
            }) {
                Ok(batch) => batch,
                Err(error) => {
                    merged.cancelled |= cancel.load(std::sync::atomic::Ordering::Relaxed);
                    merged.error = Some(error);
                    break;
                }
            };
        offset += group.outputs.len();
        merged.takes.extend(batch.takes);
        merged.cancelled |= batch.cancelled;
        if batch.error.is_some() || batch.cancelled {
            merged.error = batch.error;
            break;
        }
    }
    Ok(merged)
}

/// The batch request of group `index` of `count`.
pub fn group_request(
    request: &MeasureRequest,
    group: &Group,
    index: usize,
    count: usize,
) -> MeasureRequest {
    let mut child = request.clone();
    child.sweep = group.sweep;
    child.reference_output = group.reference.clone();
    child.output = group.outputs[0].clone();
    if count > 1 {
        child.out_dir = request.out_dir.join(format!("sweep-{}", index + 1));
    }
    child
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output(id: &str, low: Option<f32>, high: Option<f32>, gain_db: f32) -> Output {
        Output {
            id: id.into(),
            index: 0,
            name: id.into(),
            gain_db,
            delay_ms: 0.0,
            muted: false,
            freq_low_hz: low,
            freq_high_hz: high,
            azimuth_deg: None,
            elevation_deg: None,
            distance_m: None,
        }
    }
    fn snapshot(outputs: Vec<Output>) -> Snapshot {
        Snapshot {
            instance_id: "plan".into(),
            revision: "plan:1".into(),
            sample_rate: 48000,
            master_gain_db: 0.0,
            max_delay_ms: 100.0,
            outputs,
            unavailable_reason: None,
        }
    }
    fn settings(reference: &str) -> Settings {
        Settings {
            reference_output: Some(reference.into()),
            ..Settings::default()
        }
    }
    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn each_output_is_swept_over_its_own_range_within_the_reference() {
        let room = snapshot(vec![
            output("C", Some(80.0), None, 0.0),
            output("FL", None, None, 0.0),
            output("FR", None, None, 0.0),
            output("SL", Some(120.0), None, 0.0),
            output("LFE", None, Some(120.0), 0.0),
        ]);
        let groups = plan(
            &settings("C"),
            &room,
            &ids(&["FL", "FR", "SL", "LFE"]),
            None,
        )
        .unwrap();
        assert_eq!(groups.len(), 3);
        // Full-range mains share the reference's 80 Hz limit: the renderer refuses lower.
        assert_eq!(groups[0].outputs, ids(&["FL", "FR"]));
        assert_eq!(
            (groups[0].sweep.start_hz, groups[0].sweep.end_hz),
            (80.0, 20000.0)
        );
        assert_eq!(
            (groups[1].sweep.start_hz, groups[1].outputs.len()),
            (120.0, 1)
        );
        // The subwoofer: 20–120 Hz, which the centre cannot play, so it is its own reference.
        let sub = &groups[2];
        assert!(sub.bass_only() && !groups[0].bass_only());
        assert_eq!((sub.sweep.start_hz, sub.sweep.end_hz), (20.0, 120.0));
        assert_eq!(
            (sub.reference.as_str(), sub.outputs.clone()),
            ("LFE", ids(&["LFE"]))
        );
        for group in &groups {
            assert_eq!(group.sweep.duration_ms, ROOM_SWEEP_MS);
            assert_eq!(group.sweep.peak_dbfs, Settings::default().sweep.peak_dbfs);
        }
        assert_eq!(group_of(&groups, "SL"), Some(&groups[1]));
        // A full-range reference measures the subwoofer itself.
        let groups = plan(&settings("FL"), &room, &ids(&["LFE"]), None).unwrap();
        assert_eq!(groups[0].reference, "FL");
        // At 44.1 kHz input the sweep stops below its Nyquist limit.
        let mut low_rate = settings("FL");
        low_rate.sample_rate = 44100;
        let groups = plan(&low_rate, &room, &ids(&["FR"]), None).unwrap();
        assert_eq!(groups[0].sweep.end_hz, 44100.0 * 0.45);
        assert!(groups[0].sweep.validate(44100).is_ok());
    }

    #[test]
    fn a_group_plays_at_its_quietest_level_under_every_gain_ceiling() {
        let room = snapshot(vec![
            output("C", None, None, 0.0),
            output("FL", None, None, 0.0),
            output("FR", None, None, 8.0),
        ]);
        let peaks: BTreeMap<_, _> = [
            ("C".into(), -20.0),
            ("FL".into(), -16.0),
            ("FR".into(), -12.0),
        ]
        .into_iter()
        .collect();
        let groups = plan(&settings("C"), &room, &ids(&["FL", "FR"]), Some(&peaks)).unwrap();
        // The reference is the quietest, but FR's +8 dB gain allows at most −14 dBFS.
        assert_eq!(groups[0].sweep.peak_dbfs, -20.0);
        let peaks: BTreeMap<_, _> = [
            ("C".into(), -12.0),
            ("FL".into(), -12.0),
            ("FR".into(), -12.0),
        ]
        .into_iter()
        .collect();
        let groups = plan(&settings("C"), &room, &ids(&["FL", "FR"]), Some(&peaks)).unwrap();
        assert_eq!(groups[0].sweep.peak_dbfs, -14.0);
    }

    #[test]
    fn long_sequences_get_shorter_sweeps_and_custom_keeps_the_settings() {
        let outputs: Vec<_> = (0..24)
            .map(|i| output(&format!("S{i}"), None, None, 0.0))
            .collect();
        let room = snapshot(outputs);
        let all: Vec<_> = (1..24).map(|i| format!("S{i}")).collect();
        let groups = plan(&settings("S0"), &room, &all, None).unwrap();
        let sweep = groups[0].sweep;
        assert!(sweep.duration_ms < ROOM_SWEEP_MS && sweep.duration_ms >= MIN_ROOM_SWEEP_MS);
        assert!(protocol::sequence_frames(sweep, 48000, 23).is_ok());
        let longer = Sweep {
            duration_ms: sweep.duration_ms + 100,
            ..sweep
        };
        assert!(protocol::sequence_frames(longer, 48000, 23).is_err());
        let custom = Settings {
            sweep_mode: SweepMode::Custom,
            ..settings("S0")
        };
        let groups = plan(&custom, &room, &all, None).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].sweep, custom.sweep);
    }

    #[test]
    fn a_later_group_that_fails_keeps_the_earlier_takes() {
        use crate::{batch::BatchOutcome, measurement::TakeOutcome};
        struct Flaky;
        impl Backend for Flaky {
            fn inputs(&self) -> Result<Vec<measurement_capture::InputDevice>, String> {
                Ok(vec![])
            }
            fn discover(&self, _: std::net::SocketAddr) -> Result<Snapshot, String> {
                Err("unused".into())
            }
            fn measure(
                &self,
                _: MeasureRequest,
                _: &AtomicBool,
                _: &mut dyn FnMut(MeasureProgress),
            ) -> Result<TakeOutcome, String> {
                Err("unused".into())
            }
            fn measure_batch(
                &self,
                request: MeasureRequest,
                outputs: Vec<String>,
                _: &AtomicBool,
                _: &mut dyn FnMut(usize, MeasureProgress),
            ) -> Result<BatchOutcome, String> {
                if outputs[0] == "LFE" {
                    return Err("renderer connection lost".into());
                }
                Ok(BatchOutcome {
                    directory: request.out_dir.clone(),
                    takes: outputs
                        .into_iter()
                        .map(|id| {
                            let take = TakeOutcome {
                                directory: request.out_dir.join(&id),
                                accepted: true,
                                cancelled: false,
                                rejection_reason: None,
                                diagnostics: None,
                                timing: None,
                                impulse: None,
                                impulse_error: None,
                                deferred: false,
                            };
                            (id, take)
                        })
                        .collect(),
                    cancelled: false,
                    error: None,
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let room = snapshot(vec![
            output("C", None, None, 0.0),
            output("FL", None, None, 0.0),
            output("LFE", None, Some(120.0), 0.0),
        ]);
        let groups = plan(&settings("C"), &room, &ids(&["FL", "LFE"]), None).unwrap();
        assert_eq!(groups.len(), 2);
        let request = MeasureRequest {
            endpoint: "127.0.0.1:1".parse().unwrap(),
            snapshot: room,
            device: "test".into(),
            input_channel: 1,
            sample_rate: 48000,
            microphone_id: "mic-1".into(),
            position_id: "main-seat".into(),
            output: "FL".into(),
            reference_output: "C".into(),
            calibration: None,
            sweep: Settings::default().sweep,
            out_dir: dir.path().join("batch"),
        };
        let outcome = measure_groups(
            &Flaky,
            &request,
            &groups,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(outcome.takes.len(), 1);
        assert_eq!(outcome.takes[0].0, "FL");
        assert_eq!(outcome.error.as_deref(), Some("renderer connection lost"));
        assert!(!outcome.cancelled);
    }
}
