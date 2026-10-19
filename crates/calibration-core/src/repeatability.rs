//! Qualification of independent repeated estimates; no PCM or device dependency.
use crate::{Result, require};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatEstimate {
    pub arrival_ms: Option<f64>,
    pub level_db: Option<f64>,
    pub minimum_snr_db: Option<f64>,
    pub reference_level_db: Option<f64>,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualifiedEstimate {
    pub arrival_ms: f64,
    pub level_db: f64,
    pub minimum_snr_db: f64,
    pub arrival_repeat_spread_ms: f64,
    pub level_repeat_spread_db: f64,
}
/// Low-SNR estimates remain displayable; this minimum is only for correction.
pub fn qualify_acquisition(minimum_band_snr_db: f64, reference_rms_spread_db: f64) -> Result<()> {
    require(
        minimum_band_snr_db.is_finite() && minimum_band_snr_db >= 15.0,
        "excited-band acquisition signal/noise below 15 dB; diagnostic result only",
    )?;
    require(
        reference_rms_spread_db.is_finite() && (0.0..=1.0).contains(&reference_rms_spread_db),
        "reference levels changed by more than 1 dB during the take",
    )
}

pub fn qualify(repeats: &[RepeatEstimate]) -> Result<QualifiedEstimate> {
    require(
        repeats.len() == 2,
        "two independent complete target captures are required",
    )?;
    let mut arrivals = Vec::new();
    let mut levels = Vec::new();
    let mut refs = Vec::new();
    let mut snr = f64::INFINITY;
    for r in repeats {
        require(r.reasons.is_empty(), r.reasons.join("; "))?;
        let Some((arrival, level, noise, reference)) = r
            .arrival_ms
            .zip(r.level_db)
            .zip(r.minimum_snr_db.zip(r.reference_level_db))
            .map(|((a, b), (c, d))| (a, b, c, d))
        else {
            return Err(crate::Error::Invalid(
                "arrival or level evidence is unresolved".into(),
            ));
        };
        require(
            arrival.is_finite() && arrival.abs() <= 10_000.0,
            "invalid relative arrival",
        )?;
        require(
            level.is_finite()
                && level.abs() <= 300.0
                && reference.is_finite()
                && reference.abs() <= 300.0,
            "invalid relative level",
        )?;
        require(
            noise.is_finite() && noise >= 20.0,
            format!(
                "integrated level-band signal/noise is {noise:.1} dB (minimum 20 dB for correction)"
            ),
        )?;
        arrivals.push(arrival);
        levels.push(level);
        refs.push(reference);
        snr = snr.min(noise);
    }
    let dt = (arrivals[0] - arrivals[1]).abs();
    let dl = (levels[0] - levels[1]).abs();
    require(
        dt <= 0.15,
        format!("arrival changed by {dt:.3} ms between repeats (limit 0.150 ms)"),
    )?;
    require(
        dl <= 0.5,
        format!("level changed by {dl:.2} dB between repeats (limit 0.50 dB)"),
    )?;
    require(
        (refs[0] - refs[1]).abs() <= 0.5,
        "reference level changed between repeats",
    )?;
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
        }
    }
    #[test]
    fn acquisition_confidence_only_blocks_correction_below_its_floor() {
        assert!(qualify_acquisition(15.0, 0.1).is_ok());
        assert!(qualify_acquisition(14.99, 0.1).is_err());
        assert!(qualify_acquisition(25.0, 1.01).is_err());
        assert!(qualify_acquisition(f64::NAN, 0.1).is_err());
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
        assert!(qualify(std::slice::from_ref(&a)).is_err());
        for case in 0..6 {
            let mut b = a.clone();
            match case {
                0 => b.arrival_ms = None,
                1 => b.arrival_ms = Some(-2.8),
                2 => b.level_db = Some(3.0),
                3 => b.reference_level_db = Some(0.0),
                4 => b.minimum_snr_db = Some(f64::NAN),
                _ => b.reasons.push("clipped".into()),
            }
            assert!(qualify(&[a.clone(), b]).is_err());
        }
    }
}

/// Per-speaker failures do not invalidate independent, qualified neighbours.
/// Shared reference consistency is checked only on the retained evidence.
#[derive(Debug)]
pub struct QualifiedSubset {
    pub rows: Vec<std::result::Result<QualifiedEstimate, String>>,
    pub blockers: Vec<String>,
}
pub fn qualify_subset<'a>(
    repeats: impl IntoIterator<Item = &'a [RepeatEstimate]>,
) -> QualifiedSubset {
    let mut rows = Vec::new();
    let mut references = Vec::new();
    for repeats in repeats {
        let qualified = qualify(repeats).map_err(|e| e.to_string());
        if qualified.is_ok() {
            references.extend(repeats.iter().filter_map(|r| r.reference_level_db));
        }
        rows.push(qualified);
    }
    let count = rows.iter().filter(|r| r.is_ok()).count();
    let mut blockers = Vec::new();
    if count < 2 {
        blockers.push(format!(
            "Only {count} speaker(s) qualify; relative correction requires at least two."
        ));
    }
    if !references.is_empty()
        && references.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - references.iter().copied().fold(f64::INFINITY, f64::min)
            > 0.5
    {
        blockers.push("Reference level changed across qualified speakers; keep microphone and input gain fixed.".into());
    }
    QualifiedSubset { rows, blockers }
}

#[cfg(test)]
mod subset_tests {
    use super::*;
    fn pair(reference: f64) -> Vec<RepeatEstimate> {
        vec![
            RepeatEstimate {
                arrival_ms: Some(0.0),
                level_db: Some(-6.0),
                minimum_snr_db: Some(30.0),
                reference_level_db: Some(reference),
                reasons: vec![]
            };
            2
        ]
    }
    #[test]
    fn excludes_independent_failures_without_weakening_shared_reference_checks() {
        let good = pair(-5.0);
        let mut weak = pair(-80.0);
        weak[1].minimum_snr_db = Some(18.5);
        let mut clipped = pair(0.0);
        clipped[0].reasons.push("clipped recording".into());
        let selection = qualify_subset([good.as_slice(), &weak, &good, &clipped]);
        assert!(selection.blockers.is_empty());
        assert!(selection.rows[0].is_ok() && selection.rows[2].is_ok());
        assert!(selection.rows[1].as_ref().unwrap_err().contains("18.5"));
        assert!(selection.rows[3].as_ref().unwrap_err().contains("clipped"));
        assert!(qualify_subset([good.as_slice(), &weak]).blockers[0].contains("at least two"));
        assert!(
            qualify_subset([good.as_slice(), &pair(-4.0)]).blockers[0]
                .contains("Reference level changed")
        );
    }
}
