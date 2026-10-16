//! Experimental display quality, separate from repeated correction qualification.
use measurement_capture::{
    diagnostics::AcquisitionDiagnostics, impulse::ImpulseReport, level, timing::TimingReport,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    Unavailable,
    Poor,
    Limited,
    Good,
}
impl Grade {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "Unavailable",
            Self::Poor => "Poor",
            Self::Limited => "Limited",
            Self::Good => "Good",
        }
    }
    fn from_snr(value: f64) -> Self {
        if !value.is_finite() {
            Self::Unavailable
        } else if value >= 20.0 {
            Self::Good
        } else if value >= 15.0 {
            Self::Limited
        } else {
            Self::Poor
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Indicator {
    pub grade: Grade,
    pub details: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub method: String,
    pub acquisition: Indicator,
    pub timing: Indicator,
    pub level: Indicator,
    pub level_db: Option<f64>,
    pub level_band_snr_db: Option<f64>,
    pub minimum_frequency_snr_db: Option<f64>,
}
impl Report {
    pub fn grade(&self) -> Grade {
        self.acquisition
            .grade
            .min(self.timing.grade)
            .min(self.level.grade)
    }
}

pub fn assess(
    timing: Option<&TimingReport>,
    impulse: Option<&ImpulseReport>,
    diagnostics: Option<&AcquisitionDiagnostics>,
    hard_error: Option<&str>,
) -> Report {
    let missing = |message: &str| Indicator {
        grade: Grade::Unavailable,
        details: vec![message.into()],
    };
    let mut acquisition =
        missing("No synchronized sweep windows; inspect the acquisition diagnostics.");
    let band = timing
        .and_then(|t| t.band_noise.as_ref())
        .or_else(|| diagnostics.and_then(|d| d.band_noise.as_ref()));
    if let Some(b) = band {
        let min = b
            .signal_to_noise_db
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        acquisition = Indicator {
            grade: Grade::from_snr(min),
            details: vec![format!(
                "Minimum excited-band S/N: {min:.1} dB ({:.0}–{:.0} Hz).",
                b.band_hz[0], b.band_hz[1]
            )],
        };
    } else if let Some(segments) = diagnostics.and_then(|d| d.segments.as_ref()) {
        let min = segments
            .iter()
            .map(|s| s.signal_to_noise_db)
            .fold(f64::INFINITY, f64::min);
        acquisition = Indicator {
            grade: Grade::from_snr(min).min(Grade::Limited),
            details: vec![format!(
                "Legacy broadband S/N: {min:.1} dB. Reanalyze for an excited-band estimate."
            )],
        };
    }
    if let Some(t) = timing {
        acquisition.details.extend(t.warnings.clone());
        if (t.sweep_rms_dbfs[0] - t.sweep_rms_dbfs[2]).abs() > 1.0 {
            acquisition.grade = Grade::Poor;
        }
    }
    if diagnostics.is_some_and(|d| d.total_clipped_samples > 0) {
        acquisition.grade = Grade::Poor;
        acquisition
            .details
            .push("Clipping distorts the recording; results are for inspection only.".into());
    }
    if let Some(error) = hard_error {
        acquisition.grade = acquisition.grade.min(Grade::Poor);
        acquisition.details.push(error.into());
    }
    let mut arrival = missing("No supported arrival estimate. No delay has been invented.");
    if let Some(ir) = impulse {
        if let Some(delay) = ir.relative_arrival_ms {
            arrival = Indicator {
                grade: acquisition.grade,
                details: vec![format!(
                    "Arrival candidate: {delay:+.3} ms. Repeatability and direct-path identity still require assessment."
                )],
            };
        } else {
            arrival.grade = Grade::Poor;
            arrival.details.extend(ir.unresolved_reasons.clone());
        }
    }
    let mut level_quality = missing("No complete 500–2000 Hz transfer spectrum.");
    let mut level_db = None;
    let mut level_band_snr_db = None;
    let mut minimum_frequency_snr_db = None;
    if let Some(spectrum) = impulse.and_then(|ir| ir.segments[1].spectral.as_ref()) {
        match level::estimate(spectrum) {
            Ok(b) => {
                level_db = Some(b.level_db);
                level_band_snr_db = Some(b.band_snr_db);
                minimum_frequency_snr_db = Some(b.minimum_snr_db);
                level_quality = Indicator {
                    grade: Grade::from_snr(b.band_snr_db),
                    details: vec![format!(
                        "500–2000 Hz level: {:+.2} dB relative transfer; integrated S/N: {:.1} dB.",
                        b.level_db, b.band_snr_db
                    )],
                };
                if b.weak_grid_points > 0 {
                    level_quality.grade = level_quality.grade.min(Grade::Limited);
                    level_quality.details.push(format!("{} / 25 level-grid points touch a region below 20 dB; weakest spectral point: {:.1} dB. All frequencies keep their original weights.", b.weak_grid_points,b.minimum_snr_db));
                }
                if hard_error.is_some() || diagnostics.is_some_and(|d| d.total_clipped_samples > 0)
                {
                    level_quality.grade = Grade::Poor;
                    level_quality
                        .details
                        .push("Capture fault: this level must not authorize a correction.".into());
                }
            }
            Err(reason) => level_quality.details.push(reason),
        }
    }
    Report {
        method: "experimental-band-quality-v1".into(),
        acquisition,
        timing: arrival,
        level: level_quality,
        level_db,
        level_band_snr_db,
        minimum_frequency_snr_db,
    }
}
