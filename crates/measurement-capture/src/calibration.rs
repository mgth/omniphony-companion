//! Preserve calibration data without implying that it has been applied.
use serde::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize)]
pub struct CalibrationPoint {
    pub frequency_hz: f64,
    pub correction_db: f64,
}

/// Common two-column frequency/relative-dB text exports. Comments beginning
/// with #, ; or * are retained in the raw file by the caller, not interpreted.
pub fn parse(text: &str) -> Result<Vec<CalibrationPoint>, String> {
    if text.len() > 1024 * 1024 {
        return Err("calibration file exceeds 1 MiB".into());
    }
    let mut points: Vec<CalibrationPoint> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';', '*']) {
            continue;
        }
        let values: Vec<_> = line.split_whitespace().collect();
        if values.len() != 2 {
            return Err(format!(
                "calibration line {} needs frequency and dB",
                index + 1
            ));
        }
        let frequency_hz: f64 = values[0]
            .parse()
            .map_err(|_| "invalid calibration frequency")?;
        let correction_db: f64 = values[1].parse().map_err(|_| "invalid calibration dB")?;
        if !frequency_hz.is_finite()
            || !(1.0..=192_000.0).contains(&frequency_hz)
            || !correction_db.is_finite()
            || correction_db.abs() > 60.0
            || points
                .last()
                .is_some_and(|p| p.frequency_hz >= frequency_hz)
        {
            return Err(format!(
                "invalid or unsorted calibration point on line {}",
                index + 1
            ));
        }
        points.push(CalibrationPoint {
            frequency_hz,
            correction_db,
        });
        if points.len() > 20_000 {
            return Err("too many calibration points".into());
        }
    }
    if points.len() < 2 {
        return Err("calibration requires at least two frequency points".into());
    }
    Ok(points)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_export_and_reject_unknown_or_nonfinite_data() {
        assert_eq!(
            parse("* sensitivity metadata\n20 -0.5\n1000 0\n20000 1.2")
                .unwrap()
                .len(),
            3
        );
        for text in [
            "20 0\n20 1",
            "20 NaN\n100 0",
            "frequency gain\n20 0",
            "20 1",
        ] {
            assert!(parse(text).is_err());
        }
    }
}
