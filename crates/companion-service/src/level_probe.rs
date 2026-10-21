//! Automatic sweep level: before an alignment's passes, every output plays a short,
//! quiet probe sweep, and its measurement level is set so the microphone would peak near
//! [`TARGET_PEAK_DBFS`]. Clipping the microphone input is what most often costs a retake.
//!
//! The probe uses the passes' groups (same ranges, references and renderer state), one
//! sequence each, with 1 s sweeps at [`PROBE_PEAK_DBFS`]. A group nothing was heard from
//! is probed once more [`LOUDER_DB`] louder. The microphone peak of each interval comes
//! from the acquisition diagnostics; a clipped interval's overdrive estimate says how far
//! over full scale it went. Levels scale linearly, so the chosen peak is the probe's plus
//! the distance from the heard peak to the target, within the contract's −60..−12 dBFS
//! and each output's gain ceiling. A longer sweep lets room modes build up a little more
//! than the probe does; the 6 dB left below full scale covers that.
use crate::{
    controller::Backend,
    measurement::{MeasureProgress, MeasureRequest},
    review::SegmentDiagnostics,
    sweep_plan::{self, Group, MAX_PEAK_DBFS, MIN_PEAK_DBFS},
};
use omniphony_adapter::Sweep;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub const PROBE_PEAK_DBFS: f32 = -40.0;
pub const LOUDER_DB: f32 = 18.0;
pub const PROBE_MS: u32 = 1000;
pub const TARGET_PEAK_DBFS: f64 = -6.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub output_id: String,
    /// The probe's playback peak.
    pub probe_peak_dbfs: f32,
    /// The microphone's peak while it played; absent when it was not heard or clipped.
    pub microphone_peak_dbfs: Option<f64>,
    /// How far over full scale a clipped probe went, when that could be estimated.
    pub overdrive_db: Option<f64>,
    pub clipped: bool,
    /// The playback peak chosen for the measurement.
    pub peak_dbfs: f32,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub directory: PathBuf,
    pub levels: Vec<Level>,
}
impl Probe {
    pub fn peaks(&self) -> BTreeMap<String, f32> {
        self.levels
            .iter()
            .map(|l| (l.output_id.clone(), l.peak_dbfs))
            .collect()
    }
    pub fn level(&self, output_id: &str) -> Option<&Level> {
        self.levels.iter().find(|l| l.output_id == output_id)
    }
}

/// What the microphone picked up of one output during a probe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Heard {
    Peak(f64),
    Clipped(Option<f64>),
    Nothing,
}
impl Heard {
    fn of(segment: &SegmentDiagnostics) -> Self {
        if segment.clipped_samples > 0 {
            Self::Clipped(segment.overdrive_db)
        } else if segment.status == crate::review::SegmentStatus::NoSweepDetected {
            Self::Nothing
        } else {
            Self::Peak(segment.peak_dbfs)
        }
    }
    /// The louder of two observations of the same output (the reference plays twice
    /// in every take of its group).
    fn louder(self, other: Self) -> Self {
        use Heard::*;
        match (self, other) {
            (Clipped(a), Clipped(b)) => Clipped(match (a, b) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            }),
            (Clipped(a), _) | (_, Clipped(a)) => Clipped(a),
            (Peak(a), Peak(b)) => Peak(a.max(b)),
            (Peak(a), Nothing) | (Nothing, Peak(a)) => Peak(a),
            (Nothing, Nothing) => Nothing,
        }
    }
}

/// The measurement peak for an output that sounded `heard` at `probe_peak`, and why it
/// is not simply that, if it is not.
pub fn choose(probe_peak: f32, heard: Heard, fallback: f32) -> (f32, Option<String>) {
    let wanted = match heard {
        Heard::Peak(peak) => f64::from(probe_peak) + TARGET_PEAK_DBFS - peak,
        Heard::Clipped(Some(over)) => f64::from(probe_peak) + TARGET_PEAK_DBFS - over,
        Heard::Clipped(None) => {
            return (
                (probe_peak - 20.0).max(MIN_PEAK_DBFS),
                Some(
                    "Clipped during the level probe by an unknown amount; measured 20 dB lower."
                        .into(),
                ),
            );
        }
        Heard::Nothing => {
            return (
                fallback,
                Some("Not heard during the level probe; the manual level is used.".into()),
            );
        }
    } as f32;
    if wanted > MAX_PEAK_DBFS {
        let peak_at_max = TARGET_PEAK_DBFS - f64::from(wanted - MAX_PEAK_DBFS);
        (
            MAX_PEAK_DBFS,
            Some(format!(
                "Quiet: even at the highest level the microphone peaks near {peak_at_max:.0} dBFS. Raise the input gain for more signal/noise."
            )),
        )
    } else if wanted < MIN_PEAK_DBFS {
        (
            MIN_PEAK_DBFS,
            Some(
                "Loud: the microphone would clip even at the lowest level. Lower the input gain."
                    .into(),
            ),
        )
    } else {
        ((wanted * 10.0).round() / 10.0, None)
    }
}

/// Probe every output of `groups` (the passes' groups) and choose its level. `request`
/// carries the input, identities and renderer state; probes are saved under `directory`.
/// `fallback` is the level of an output nothing was heard from. Only a cancellation or a
/// failure to start the capture is an error; anything else falls back with a note.
pub fn run<B: Backend + ?Sized>(
    backend: &B,
    request: &MeasureRequest,
    groups: &[Group],
    directory: &Path,
    fallback: f32,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, MeasureProgress),
) -> Result<Probe, String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut heard: BTreeMap<String, (f32, Heard)> = BTreeMap::new();
    let mut offset = 0;
    for (index, group) in groups.iter().enumerate() {
        let limit = group
            .outputs
            .iter()
            .chain(std::iter::once(&group.reference))
            .filter_map(|id| request.snapshot.outputs.iter().find(|o| &o.id == id))
            .map(|o| sweep_plan::ceiling(&request.snapshot, o))
            .fold(MAX_PEAK_DBFS, f32::min);
        let mut peak = PROBE_PEAK_DBFS.min(limit);
        for attempt in 0..2 {
            if cancel.load(Ordering::Relaxed) {
                return Err("capture cancelled by user".into());
            }
            let probe = Group {
                sweep: Sweep {
                    duration_ms: PROBE_MS,
                    peak_dbfs: peak,
                    ..group.sweep
                },
                ..group.clone()
            };
            let mut child = sweep_plan::group_request(request, &probe, index, 1);
            child.out_dir = directory.join(format!(
                "sweep-{}{}",
                index + 1,
                if attempt == 0 { "" } else { "-louder" }
            ));
            let batch =
                backend.measure_batch(child, probe.outputs.clone(), cancel, &mut |i, p| {
                    progress(offset + i, p)
                })?;
            if batch.cancelled {
                return Err("capture cancelled by user".into());
            }
            let mut found: BTreeMap<String, Heard> = BTreeMap::new();
            for (id, take) in &batch.takes {
                let Some(segments) = take.diagnostics.as_ref().and_then(|d| d.segments.as_ref())
                else {
                    continue;
                };
                let mut add = |id: &str, h: Heard| {
                    let entry = found.entry(id.to_owned()).or_insert(Heard::Nothing);
                    *entry = entry.louder(h);
                };
                add(id, Heard::of(&segments[1]));
                add(&probe.reference, Heard::of(&segments[0]));
                add(&probe.reference, Heard::of(&segments[2]));
            }
            let any = found.values().any(|h| *h != Heard::Nothing);
            let louder = (peak + LOUDER_DB).min(limit);
            if any || attempt == 1 || louder <= peak {
                for (id, h) in found {
                    let entry = heard.entry(id).or_insert((peak, Heard::Nothing));
                    // A reference probed in several groups keeps its loudest reading.
                    if entry.1 == Heard::Nothing || h.louder(entry.1) == h {
                        *entry = (peak, h);
                    }
                }
                for id in probe
                    .outputs
                    .iter()
                    .chain(std::iter::once(&probe.reference))
                {
                    heard.entry(id.clone()).or_insert((peak, Heard::Nothing));
                }
                break;
            }
            peak = louder;
        }
        offset += group.outputs.len();
    }
    let levels = heard
        .into_iter()
        .map(|(output_id, (probe_peak, h))| {
            let (peak_dbfs, note) = choose(probe_peak, h, fallback);
            Level {
                output_id,
                probe_peak_dbfs: probe_peak,
                microphone_peak_dbfs: match h {
                    Heard::Peak(p) => Some(p),
                    _ => None,
                },
                overdrive_db: match h {
                    Heard::Clipped(o) => o,
                    _ => None,
                },
                clipped: matches!(h, Heard::Clipped(_)),
                peak_dbfs,
                note,
            }
        })
        .collect();
    let probe = Probe {
        directory: directory.to_owned(),
        levels,
    };
    crate::files::write_json(&directory.join("level-probe.json"), &probe)
        .map_err(|e| e.to_string())?;
    Ok(probe)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_chosen_peak_puts_the_microphone_near_the_target() {
        // Heard at −28 dBFS with a −40 dBFS probe: 22 dB louder would reach −6 dBFS.
        assert_eq!(choose(-40.0, Heard::Peak(-28.0), -30.0), (-18.0, None));
        assert_eq!(choose(-40.0, Heard::Peak(-33.25), -30.0), (-12.8, None));
        // Clipped 4 dB over full scale at −40: 10 dB lower.
        assert_eq!(
            choose(-40.0, Heard::Clipped(Some(4.0)), -30.0),
            (-50.0, None)
        );
        let (peak, note) = choose(-40.0, Heard::Clipped(None), -30.0);
        assert!(peak == -60.0 && note.unwrap().contains("unknown amount"));
        // A quiet microphone gets the contract's highest level and a note.
        let (peak, note) = choose(-40.0, Heard::Peak(-50.0), -30.0);
        assert_eq!(peak, MAX_PEAK_DBFS);
        assert!(note.unwrap().contains("near -22 dBFS"));
        let (peak, note) = choose(-40.0, Heard::Clipped(Some(20.0)), -30.0);
        assert!(peak == MIN_PEAK_DBFS && note.unwrap().contains("Lower the input gain"));
        let (peak, note) = choose(-40.0, Heard::Nothing, -30.0);
        assert!(peak == -30.0 && note.unwrap().contains("manual level"));
    }
    #[test]
    fn the_louder_reading_wins() {
        use Heard::*;
        assert_eq!(Peak(-20.0).louder(Peak(-18.0)), Peak(-18.0));
        assert_eq!(Peak(-20.0).louder(Clipped(None)), Clipped(None));
        assert_eq!(Nothing.louder(Peak(-30.0)), Peak(-30.0));
        assert_eq!(
            Clipped(Some(2.0)).louder(Clipped(Some(5.0))),
            Clipped(Some(5.0))
        );
    }
}
