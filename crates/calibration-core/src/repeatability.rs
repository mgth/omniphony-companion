//! Qualification of independent repeated estimates; no PCM or device dependency.
use crate::reason::{
    ARRIVAL_REPEAT_LIMIT_MS, Blocker, ExclusionReason, FAR_ARRIVAL_MS, Interval,
    LEVEL_REPEAT_LIMIT_DB, SnrBand,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatEstimate {
    pub arrival_ms: Option<f64>,
    pub level_db: Option<f64>,
    pub minimum_snr_db: Option<f64>,
    pub reference_level_db: Option<f64>,
    /// Technical text of `codes`, kept so older readers still see the reasons.
    pub reasons: Vec<String>,
    /// Typed `reasons`, in the same order. Empty on sessions saved before reason codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<ExclusionReason>,
    /// How far the speaker's output gain was lowered for this take, after it clipped at
    /// its own setting. `level_db` already adds it back, so repeats compare at the
    /// original gain. Zero for a regular take.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_gain_offset_db: f64,
    /// Which sweep of the take had `minimum_snr_db`. Absent in older sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_snr_interval: Option<Interval>,
}
fn is_zero(value: &f64) -> bool {
    *value == 0.0
}
impl RepeatEstimate {
    pub fn reject(&mut self, code: ExclusionReason) {
        self.reasons.push(code.to_string());
        self.codes.push(code);
    }
    pub fn is_rejected(&self) -> bool {
        !self.codes.is_empty() || !self.reasons.is_empty()
    }
    /// The first reason this estimate cannot be used, typed also for older sessions.
    pub fn first_exclusion(&self) -> Option<ExclusionReason> {
        self.codes.first().cloned().or_else(|| {
            self.reasons
                .first()
                .map(|r| ExclusionReason::from_message(r))
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualifiedEstimate {
    pub arrival_ms: f64,
    pub level_db: f64,
    pub minimum_snr_db: f64,
    pub arrival_repeat_spread_ms: f64,
    pub level_repeat_spread_db: f64,
}
/// The weakest of a take's three sweeps (reference, speaker, reference): an invalid
/// value first, otherwise the lowest.
pub fn weakest_interval(snr_db: [f64; 3]) -> (Interval, f64) {
    let index = (0..3)
        .find(|&i| !snr_db[i].is_finite())
        .or_else(|| (0..3).min_by(|&a, &b| snr_db[a].total_cmp(&snr_db[b])))
        .unwrap_or(0);
    (
        Interval::of_segment(index).unwrap_or(Interval::Speaker),
        snr_db[index],
    )
}

/// Low-SNR estimates remain displayable; this minimum is only for correction. The
/// excited-band S/N of each sweep of the take: reference, speaker, reference.
pub fn qualify_acquisition(
    band_snr_db: [f64; 3],
    reference_rms_spread_db: f64,
) -> Result<(), ExclusionReason> {
    let (interval, minimum) = weakest_interval(band_snr_db);
    if !(minimum.is_finite() && minimum >= SnrBand::Acquisition.minimum_db()) {
        return Err(ExclusionReason::LowSnr {
            band: SnrBand::Acquisition,
            db: minimum.is_finite().then_some(minimum),
            interval: Some(interval),
        });
    }
    if !(reference_rms_spread_db.is_finite() && (0.0..=1.0).contains(&reference_rms_spread_db)) {
        return Err(ExclusionReason::UnstableReference);
    }
    Ok(())
}

pub fn qualify(repeats: &[RepeatEstimate]) -> Result<QualifiedEstimate, ExclusionReason> {
    if repeats.len() != 2 {
        return Err(ExclusionReason::NotMeasuredTwice);
    }
    let mut arrivals = Vec::new();
    let mut levels = Vec::new();
    let mut refs = Vec::new();
    let mut snr = f64::INFINITY;
    for r in repeats {
        if let Some(reason) = r.first_exclusion() {
            return Err(reason);
        }
        let Some((arrival, level, noise, reference)) = r
            .arrival_ms
            .zip(r.level_db)
            .zip(r.minimum_snr_db.zip(r.reference_level_db))
            .map(|((a, b), (c, d))| (a, b, c, d))
        else {
            return Err(ExclusionReason::NoClearArrival {
                detail: "arrival or level evidence is unresolved".into(),
            });
        };
        if !(arrival.is_finite() && arrival.abs() <= 10_000.0) {
            return Err(ExclusionReason::Other {
                detail: "invalid relative arrival".into(),
            });
        }
        if !(level.is_finite()
            && level.abs() <= 300.0
            && reference.is_finite()
            && reference.abs() <= 300.0)
        {
            return Err(ExclusionReason::Other {
                detail: "invalid relative level".into(),
            });
        }
        if !(noise.is_finite() && noise >= SnrBand::Level.minimum_db()) {
            return Err(ExclusionReason::LowSnr {
                band: SnrBand::Level,
                db: noise.is_finite().then_some(noise),
                interval: r.minimum_snr_interval,
            });
        }
        arrivals.push(arrival);
        levels.push(level);
        refs.push(reference);
        snr = snr.min(noise);
    }
    let dt = (arrivals[0] - arrivals[1]).abs();
    let dl = (levels[0] - levels[1]).abs();
    if dt > ARRIVAL_REPEAT_LIMIT_MS || dl > LEVEL_REPEAT_LIMIT_DB {
        return Err(ExclusionReason::RepeatSpread { ms: dt, db: dl });
    }
    if (refs[0] - refs[1]).abs() > 0.5 {
        return Err(ExclusionReason::UnstableReference);
    }
    Ok(QualifiedEstimate {
        arrival_ms: (arrivals[0] + arrivals[1]) * 0.5,
        level_db: 10.0
            * ((10_f64.powf(levels[0] / 10.0) + 10_f64.powf(levels[1] / 10.0)) * 0.5).log10(),
        minimum_snr_db: snr,
        arrival_repeat_spread_ms: dt,
        level_repeat_spread_db: dl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn estimate() -> RepeatEstimate {
        RepeatEstimate {
            arrival_ms: Some(-3.0),
            level_db: Some(2.0),
            minimum_snr_db: Some(30.0),
            reference_level_db: Some(-1.0),
            reasons: vec![],
            codes: vec![],
            output_gain_offset_db: 0.0,
            minimum_snr_interval: None,
        }
    }
    #[test]
    fn acquisition_confidence_only_blocks_correction_below_its_floor() {
        assert!(qualify_acquisition([15.0, 30.0, 15.0], 0.1).is_ok());
        assert_eq!(
            qualify_acquisition([30.0, 14.99, 30.0], 0.1),
            Err(ExclusionReason::LowSnr {
                band: SnrBand::Acquisition,
                db: Some(14.99),
                interval: Some(Interval::Speaker),
            })
        );
        // A disturbed reference sweep is named, not blamed on the speaker.
        assert_eq!(
            qualify_acquisition([28.0, 30.0, 14.4], 0.1),
            Err(ExclusionReason::LowSnr {
                band: SnrBand::Acquisition,
                db: Some(14.4),
                interval: Some(Interval::ReferenceAfter),
            })
        );
        assert_eq!(
            qualify_acquisition([25.0; 3], 1.01),
            Err(ExclusionReason::UnstableReference)
        );
        assert_eq!(
            qualify_acquisition([25.0, 25.0, f64::NAN], 0.1),
            Err(ExclusionReason::LowSnr {
                band: SnrBand::Acquisition,
                db: None,
                interval: Some(Interval::ReferenceAfter),
            })
        );
    }
    #[test]
    fn long_signed_delays_are_advisory_but_changes_still_fail() {
        for delay in [-160.0, 160.0] {
            let mut a = estimate();
            a.arrival_ms = Some(delay);
            assert_eq!(qualify(&[a.clone(), a.clone()]).unwrap().arrival_ms, delay);
            assert!(crate::long_delay_warning(delay).is_some());
            let mut b = a.clone();
            b.arrival_ms = Some(delay + 21.0);
            assert!(qualify(&[a, b]).is_err());
        }
        assert!(crate::long_delay_warning(100.0).is_none());
        assert!(crate::long_delay_warning(f64::NAN).is_none());
    }
    #[test]
    fn independent_repeat_gates_reject_ambiguous_unstable_and_nonfinite_evidence() {
        let a = estimate();
        assert_eq!(qualify(&[a.clone(), a.clone()]).unwrap().arrival_ms, -3.0);
        assert_eq!(
            qualify(std::slice::from_ref(&a)),
            Err(ExclusionReason::NotMeasuredTwice)
        );
        for case in 0..7 {
            let mut b = a.clone();
            let expected = match case {
                0 => {
                    b.arrival_ms = None;
                    "no_clear_arrival"
                }
                1 => {
                    b.arrival_ms = Some(-2.8);
                    "repeat_spread"
                }
                2 => {
                    b.level_db = Some(3.0);
                    "repeat_spread"
                }
                3 => {
                    b.reference_level_db = Some(0.0);
                    "unstable_reference"
                }
                4 => {
                    b.minimum_snr_db = Some(f64::NAN);
                    "low_snr"
                }
                5 => {
                    b.reject(ExclusionReason::Clipped { overdrive_db: None });
                    "clipped"
                }
                // A session saved before reason codes carries text only.
                _ => {
                    b.reasons.push("clipped".into());
                    "clipped"
                }
            };
            let reason = qualify(&[a.clone(), b]).unwrap_err();
            let json = serde_json::to_value(&reason).unwrap();
            assert_eq!(json["code"], expected, "case {case}: {reason}");
        }
    }
}

/// Per-speaker failures do not invalidate independent, qualified neighbours.
/// Shared reference consistency is checked only on the evidence used for correction.
#[derive(Debug)]
pub struct QualifiedSubset {
    /// Per speaker: its repeatable estimate, or why there is none.
    pub rows: Vec<Result<QualifiedEstimate, ExclusionReason>>,
    /// Per speaker: why a repeatable speaker is left out of the correction.
    pub set_aside: Vec<Option<ExclusionReason>>,
    pub blockers: Vec<Blocker>,
}
impl QualifiedSubset {
    /// Whether speaker `index` receives a correction target.
    pub fn included(&self, index: usize) -> bool {
        self.rows.get(index).is_some_and(Result::is_ok)
            && self.set_aside.get(index).is_some_and(Option::is_none)
    }
}

/// Each item is one speaker's repeats and whether the user asked to include it even
/// if its arrival is far from the others.
pub fn qualify_subset<'a>(
    speakers: impl IntoIterator<Item = (&'a [RepeatEstimate], bool)>,
) -> QualifiedSubset {
    let mut rows = Vec::new();
    let mut include = Vec::new();
    let mut references = Vec::new();
    for (repeats, included) in speakers {
        rows.push(qualify(repeats));
        include.push(included);
        references.push(repeats);
    }
    let arrivals: Vec<_> = rows
        .iter()
        .map(|r| r.as_ref().ok().map(|q| q.arrival_ms))
        .collect();
    let set_aside = set_aside_far_arrivals(&arrivals, &include);
    let mut subset = QualifiedSubset {
        rows,
        set_aside,
        blockers: Vec::new(),
    };
    let count = (0..subset.rows.len())
        .filter(|&i| subset.included(i))
        .count();
    if count < 2 {
        subset.blockers.push(Blocker::TooFewQualified {
            count,
            set_aside: subset.set_aside.iter().flatten().count(),
        });
    }
    let (low, high) = (0..subset.rows.len())
        .filter(|&i| subset.included(i))
        .flat_map(|i| references[i].iter().filter_map(|r| r.reference_level_db))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    if high - low > 0.5 {
        subset.blockers.push(Blocker::ReferenceDrift);
    }
    subset
}

/// Set aside repeatable arrivals more than [`FAR_ARRIVAL_MS`] from the group, unless the
/// user includes them. Matching a speaker that is much later (another output device, a
/// projector path) delays every other speaker by as much, so it must be a choice.
/// `arrivals` holds `None` for speakers that did not qualify.
pub fn set_aside_far_arrivals(
    arrivals: &[Option<f64>],
    include: &[bool],
) -> Vec<Option<ExclusionReason>> {
    let mut sorted: Vec<f64> = arrivals
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    sorted.sort_by(f64::total_cmp);
    let Some(center) = group_center(&sorted) else {
        return vec![None; arrivals.len()];
    };
    arrivals
        .iter()
        .enumerate()
        .map(|(i, arrival)| {
            let offset = (*arrival)? - center;
            (offset.abs() > FAR_ARRIVAL_MS && !include.get(i).copied().unwrap_or(false))
                .then_some(ExclusionReason::FarArrival { ms: offset })
        })
        .collect()
}

/// The median of sorted arrivals. For an even count it is the middle arrival nearer the
/// timing reference (0 ms), not their mean, so a room split into two equal halves keeps
/// the reference's half instead of setting everyone aside.
pub fn group_center(sorted: &[f64]) -> Option<f64> {
    match sorted.len() {
        0 => None,
        n if n % 2 == 1 => Some(sorted[n / 2]),
        n => {
            let (low, high) = (sorted[n / 2 - 1], sorted[n / 2]);
            Some(if high.abs() < low.abs() { high } else { low })
        }
    }
}

#[cfg(test)]
mod subset_tests {
    use super::*;
    fn pair(arrival: f64, reference: f64) -> Vec<RepeatEstimate> {
        vec![
            RepeatEstimate {
                arrival_ms: Some(arrival),
                level_db: Some(-6.0),
                minimum_snr_db: Some(30.0),
                reference_level_db: Some(reference),
                reasons: vec![],
                codes: vec![],
                output_gain_offset_db: 0.0,
                minimum_snr_interval: None,
            };
            2
        ]
    }
    fn subset(speakers: &[(&[RepeatEstimate], bool)]) -> QualifiedSubset {
        qualify_subset(speakers.iter().copied())
    }
    #[test]
    fn excludes_independent_failures_without_weakening_shared_reference_checks() {
        let good = pair(0.0, -5.0);
        let mut weak = pair(0.0, -80.0);
        weak[1].minimum_snr_db = Some(18.5);
        let mut clipped = pair(0.0, 0.0);
        clipped[0].reject(ExclusionReason::Clipped { overdrive_db: None });
        let selection = subset(&[
            (&good, false),
            (&weak, false),
            (&good, false),
            (&clipped, false),
        ]);
        assert!(selection.blockers.is_empty());
        assert!(selection.included(0) && selection.included(2));
        assert_eq!(
            selection.rows[1].as_ref().unwrap_err(),
            &ExclusionReason::LowSnr {
                band: SnrBand::Level,
                db: Some(18.5),
                interval: None,
            }
        );
        assert_eq!(
            selection.rows[3].as_ref().unwrap_err(),
            &ExclusionReason::Clipped { overdrive_db: None }
        );
        assert_eq!(
            subset(&[(&good, false), (&weak, false)]).blockers,
            [Blocker::TooFewQualified {
                count: 1,
                set_aside: 0
            }]
        );
        assert_eq!(
            subset(&[(&good, false), (&pair(0.0, -4.0), false)]).blockers,
            [Blocker::ReferenceDrift]
        );
    }

    #[test]
    fn far_arrivals_are_set_aside_from_the_median_unless_included() {
        // Repeatable arrivals of the 16 Oct 2026 session (FL, FR, C, BC, spk-12, spk-13).
        let arrivals = [-0.0326, 0.3173, -0.0200, -5.9947, 176.8552, 176.7789];
        let rows: Vec<_> = arrivals.iter().map(|a| pair(*a, -5.0)).collect();
        let speakers: Vec<_> = rows.iter().map(|r| (r.as_slice(), false)).collect();
        let selection = subset(&speakers);
        assert!(selection.blockers.is_empty(), "{:?}", selection.blockers);
        assert!((0..4).all(|i| selection.included(i)));
        for i in [4, 5] {
            assert!(
                selection.rows[i].is_ok(),
                "set-aside speakers stay repeatable"
            );
            let Some(ExclusionReason::FarArrival { ms }) = selection.set_aside[i] else {
                panic!("{:?}", selection.set_aside[i]);
            };
            assert!((ms - (arrivals[i] + 0.0200)).abs() < 1e-9, "{ms}");
        }
        // Including one keeps the median and the others' verdicts.
        let mut speakers = speakers;
        speakers[4].1 = true;
        let selection = subset(&speakers);
        assert!(selection.included(4) && !selection.included(5));
        // A split room keeps the reference's half; everything set aside is reported.
        let near = pair(0.1, -5.0);
        let far = pair(177.0, -5.0);
        let selection = subset(&[(&near, false), (&near, false), (&far, false), (&far, false)]);
        assert!(selection.blockers.is_empty());
        assert!(selection.included(0) && !selection.included(2));
        let selection = subset(&[(&near, false), (&far, false)]);
        assert_eq!(
            selection.blockers,
            [Blocker::TooFewQualified {
                count: 1,
                set_aside: 1
            }]
        );
        let selection = subset(&[(&near, false), (&far, true)]);
        assert!(selection.blockers.is_empty());
        // A far speaker's reference level cannot block the others' correction.
        let drifted = pair(177.0, 0.0);
        assert!(
            subset(&[(&near, false), (&near, false), (&drifted, false)])
                .blockers
                .is_empty()
        );
    }

    #[test]
    fn the_group_center_is_the_median_nearer_the_reference() {
        assert_eq!(group_center(&[]), None);
        assert_eq!(group_center(&[3.0]), Some(3.0));
        assert_eq!(group_center(&[-1.0, 2.0, 50.0]), Some(2.0));
        assert_eq!(group_center(&[0.2, 177.0]), Some(0.2));
        assert_eq!(group_center(&[-177.0, -0.5]), Some(-0.5));
    }
}
