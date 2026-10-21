//! Microphone calibration files. The raw recording and the stored impulse response are
//! never changed: a calibration only corrects the magnitude derived from them.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationPoint {
    pub frequency_hz: f64,
    pub correction_db: f64,
}

/// Common frequency/relative-dB text exports, with an optional third phase column
/// that is ignored. Comments beginning with #, ; or * and quoted header lines such as
/// a UMIK-1 `"Sens Factor =…"` are retained in the raw file by the caller, not
/// interpreted: levels stay relative, so a sensitivity factor is not needed.
pub fn parse(text: &str) -> Result<Vec<CalibrationPoint>, String> {
    if text.len() > 1024 * 1024 {
        return Err("calibration file exceeds 1 MiB".into());
    }
    let mut points: Vec<CalibrationPoint> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';', '*', '"']) {
            continue;
        }
        let values: Vec<_> = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|v| !v.is_empty())
            .collect();
        if !(2..=3).contains(&values.len()) {
            return Err(format!(
                "calibration line {} needs frequency and dB",
                index + 1
            ));
        }
        let frequency_hz: f64 = values[0]
            .parse()
            .map_err(|_| "invalid calibration frequency")?;
        let correction_db: f64 = values[1].parse().map_err(|_| "invalid calibration dB")?;
        if values
            .get(2)
            .is_some_and(|phase| phase.parse::<f64>().is_err())
        {
            return Err(format!("invalid calibration phase on line {}", index + 1));
        }
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

/// A parsed calibration: the microphone's deviation from flat, as the file states it.
/// Correcting a measured magnitude subtracts it (`calibrated = raw − deviation`), the
/// convention of UMIK-1, Dayton Audio and REW calibration files.
#[derive(Debug, Clone)]
pub struct Calibration {
    points: Vec<CalibrationPoint>,
}
impl Calibration {
    pub fn parse(text: &str) -> Result<Self, String> {
        parse(text).map(|points| Self { points })
    }
    pub fn points(&self) -> &[CalibrationPoint] {
        &self.points
    }
    /// The first and last frequency the file covers.
    pub fn coverage_hz(&self) -> [f64; 2] {
        [
            self.points[0].frequency_hz,
            self.points[self.points.len() - 1].frequency_hz,
        ]
    }
    /// The deviation at `hz`, interpolated linearly over log frequency and held at the
    /// first or last value outside the covered range.
    pub fn deviation_db(&self, hz: f64) -> f64 {
        let points = &self.points;
        let at = points.partition_point(|p| p.frequency_hz < hz);
        if at == 0 {
            return points[0].correction_db;
        }
        if at == points.len() {
            return points[at - 1].correction_db;
        }
        let (a, b) = (&points[at - 1], &points[at]);
        let fraction = (hz / a.frequency_hz).ln() / (b.frequency_hz / a.frequency_hz).ln();
        a.correction_db + fraction * (b.correction_db - a.correction_db)
    }
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
            "20 0 phase\n100 0 0",
            "20 0 0 0\n100 0",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }
    #[test]
    fn umik_and_phase_column_exports_are_read() {
        let umik = "\"Sens Factor =-.7742dB, AGain =18dB, SERNO: 7101234\"\n10.054\t-2.8\n1000.0\t0.0\n20000\t-1.5\n";
        assert_eq!(parse(umik).unwrap().len(), 3);
        assert_eq!(parse("20,-1.0,12.5\n200,0.5,-3").unwrap().len(), 2);
    }
    #[test]
    fn deviation_interpolates_over_log_frequency_and_holds_the_ends() {
        let calibration = Calibration::parse("100 -2\n1000 0\n10000 4").unwrap();
        assert_eq!(calibration.coverage_hz(), [100.0, 10000.0]);
        assert_eq!(calibration.deviation_db(20.0), -2.0);
        assert_eq!(calibration.deviation_db(20000.0), 4.0);
        assert!((calibration.deviation_db(1000.0)).abs() < 1e-12);
        assert!((calibration.deviation_db(10_f64.powf(2.5)) + 1.0).abs() < 1e-12);
        assert!((calibration.deviation_db(10_f64.powf(3.25)) - 1.0).abs() < 1e-12);
    }
}
