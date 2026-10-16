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
            "insufficient signal/noise",
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
