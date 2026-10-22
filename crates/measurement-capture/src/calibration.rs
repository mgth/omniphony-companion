//! Microphone calibration files. The raw recording and the stored impulse response are
//! never changed: a calibration only corrects the magnitude derived from them.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationPoint {
    pub frequency_hz: f64,
    pub correction_db: f64,
}

/// Common frequency/relative-dB text exports, with an optional third phase column
/// that is ignored. CR, CRLF and LF line endings are alike. As in REW, a line that
/// does not start with a number is not data: comments (`#`, `;`, `*`), column
/// headers such as `Hz\tdB\tPhase` and a UMIK-1 `"Sens Factor =…"` line are retained
/// in the raw file by the caller, not interpreted: levels stay relative, so a
/// sensitivity factor is not needed. A line that does start with a number must be a
/// valid point, in increasing frequency.
pub fn parse(text: &str) -> Result<Vec<CalibrationPoint>, String> {
    if text.len() > 1024 * 1024 {
        return Err("calibration file exceeds 1 MiB".into());
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut points: Vec<CalibrationPoint> = Vec::new();
    for (index, line) in lines(text).enumerate() {
        let line = line.trim();
        if !starts_with_number(line) {
            continue;
        }
        let values: Vec<_> = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|v| !v.is_empty())
            .collect();
        if !(2..=3).contains(&values.len()) {
            return Err(format!(
                "calibration needs frequency and dB on line {}",
                index + 1
            ));
        }
        let frequency_hz: f64 = values[0]
            .parse()
            .map_err(|_| format!("invalid calibration frequency on line {}", index + 1))?;
        let correction_db: f64 = values[1]
            .parse()
            .map_err(|_| format!("invalid calibration dB on line {}", index + 1))?;
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

/// Lines ended by LF, CRLF or a lone CR (classic Mac OS exports).
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
}

/// Whether `line` opens with a decimal number (`20`, `-0.5`, `+.5`, `.5`). A signed
/// line counts, so a negative frequency is rejected rather than skipped.
fn starts_with_number(line: &str) -> bool {
    let rest = line.strip_prefix(['+', '-']).unwrap_or(line);
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    rest.starts_with(|c: char| c.is_ascii_digit())
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
    fn classic_mac_export_with_a_plain_header_is_read() {
        // The shape of the generic Behringer ECM8000 file from Sonic Studio: CR-only
        // line endings, an unquoted `Hz dB Phase` header and no final line ending.
        let ecm8000 = "Hz\tdB\tPhase\r20.0000\t-0.070\t31.90\r25.1785\t0.137\t25.80\r\
            31.6978\t0.275\t20.20\r1000.0000\t0.000\t0.00\r21164.4000\t-4.460\t-16.00\r\
            21999.0000\t-4.920\t-15.00";
        let points = parse(ecm8000).unwrap();
        assert_eq!(points.len(), 6);
        assert_eq!(points[0].frequency_hz, 20.0);
        assert_eq!(points[0].correction_db, -0.07);
        assert_eq!(points[5].frequency_hz, 21999.0);
        assert_eq!(points[5].correction_db, -4.92);
    }
    #[test]
    fn umik_style_crlf_export_is_read() {
        let umik = "\u{feff}\"Sens Factor =-1.378dB, AGain =18dB, SERNO: 7000000\"\r\n\
            10.054\t-4.6501\r\n10.179\t-4.5235\r\n1000.000\t0.0000\r\n20000.000\t-0.9310\r\n";
        let points = parse(umik).unwrap();
        assert_eq!(points.len(), 4);
        assert_eq!(points[0].frequency_hz, 10.054);
        assert_eq!(points[3].correction_db, -0.931);
    }
    #[test]
    fn dayton_style_export_is_read() {
        // A sensitivity line, a column header with units, space-padded columns with
        // trailing blanks and a blank last line.
        let dayton = "*1kHz\t-41.6\r\nFreq(Hz)  SPL(dB)  Phase(degrees)\r\n\
             20.00   -0.42   12.3 \r\n 100.00  +0.10   -1.0 \r\n1000.00   0.00    0.0\r\n\
            20000.00  2.35  -45.0\r\n\r\n";
        let points = parse(dayton).unwrap();
        assert_eq!(points.len(), 4);
        assert_eq!(points[1].correction_db, 0.1);
        assert_eq!(points[3].frequency_hz, 20000.0);
    }
    #[test]
    fn malformed_or_unsorted_numeric_lines_are_rejected_with_their_line() {
        for (text, line) in [
            ("Hz\tdB\tPhase\r100\t0\t0\r20\t0\t0", 3),
            ("Hz dB\r\n20 0\r\n1k 0\r\n2000 1", 3),
            ("# header\n20 0\n100 0 x\n200 0", 3),
            ("-20 0\r100 0", 1),
            (".5x 0\r100 0", 1),
            ("Hz dB\r20\r100 0", 2),
        ] {
            let error = parse(text).unwrap_err();
            assert!(
                error.ends_with(&format!("line {line}")),
                "{text:?}: {error}"
            );
        }
        assert!(parse("Hz dB\r20 0\rend of data").is_err());
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
