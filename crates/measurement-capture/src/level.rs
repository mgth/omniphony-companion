//! Comparable relative levels over one fixed band, independent of arrival validity.
//! Equal log-frequency power weighting approximates a band-limited pink stimulus.
use crate::spectral::SpectralReport;
use serde::Serialize;

pub const BAND_HZ: [f64; 2] = [500.0, 2000.0];
pub const METHOD: &str = "transfer-power-500-2000hz-log-grid-v1-uncalibrated";
#[derive(Debug, Clone, Serialize)]
pub struct BandLevel {
    pub method: &'static str,
    pub band_hz: [f64; 2],
    /// Transfer amplitude, in dB relative to a unit digital source; not SPL/dBFS.
    pub level_db: f64,
    pub minimum_snr_db: f64,
}

pub fn estimate(report: &SpectralReport) -> Result<BandLevel, String> {
    let points = &report.points;
    if points.len() < 2
        || points.len() > 128
        || points.windows(2).any(|w| {
            !w[0].frequency_hz.is_finite()
                || w[0].frequency_hz <= 0.0
                || w[0].frequency_hz >= w[1].frequency_hz
        })
        || !points.last().unwrap().frequency_hz.is_finite()
    {
        return Err("invalid level spectrum".into());
    }
    let mut power = 0.0;
    let mut snr = f64::INFINITY;
    // Exactly the same 25 frequencies and weights for every speaker and repeat.
    for step in 0..=24 {
        let hz = BAND_HZ[0] * 2_f64.powf(step as f64 / 12.0);
        let pair = points
            .windows(2)
            .find(|w| w[0].frequency_hz <= hz && w[1].frequency_hz >= hz)
            .ok_or("sweep does not cover the complete 500–2000 Hz level band")?;
        let db: Vec<_> = pair
            .iter()
            .map(|p| {
                let value = p.magnitude_db.ok_or("insufficient level-band evidence")?;
                if !value.is_finite()
                    || !(-300.0..=300.0).contains(&value)
                    || !p.estimated_snr_db.is_finite()
                    || p.estimated_snr_db < 20.0
                {
                    return Err(
                        "level band needs at least 20 dB estimated signal/noise throughout",
                    );
                }
                Ok(value)
            })
            .collect::<Result<_, _>>()?;
        snr = snr
            .min(pair[0].estimated_snr_db)
            .min(pair[1].estimated_snr_db);
        let fraction =
            (hz / pair[0].frequency_hz).ln() / (pair[1].frequency_hz / pair[0].frequency_hz).ln();
        let value = db[0] + fraction * (db[1] - db[0]);
        power += 10_f64.powf(value / 10.0) * if step == 0 || step == 24 { 0.5 } else { 1.0 };
    }
    Ok(BandLevel {
        method: METHOD,
        band_hz: BAND_HZ,
        level_db: 10.0 * (power / 24.0).log10(),
        minimum_snr_db: snr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectral;
    use omniphony_osc_contract::measurement::Sweep;
    #[test]
    fn known_gain_is_independent_of_delay_polarity_and_rate() {
        for rate in [44100.0, 48000.0] {
            let anchor = (rate * 0.1) as usize;
            let mut h = vec![0.0; (rate * 0.25) as usize + 1];
            h[anchor + 100] = -0.25;
            let mut s = spectral::analyze(
                &h,
                rate,
                anchor,
                Sweep {
                    duration_ms: 1000,
                    peak_dbfs: -30.0,
                    start_hz: 200.0,
                    end_hz: 10000.0,
                },
            )
            .unwrap();
            assert!((estimate(&s).unwrap().level_db + 12.0412).abs() < 0.001);
            let i = s
                .points
                .iter()
                .position(|p| p.frequency_hz > 1000.0)
                .unwrap();
            s.points[i].estimated_snr_db = 5.0;
            assert!(estimate(&s).is_err(), "do not silently drop weak bands");
        }
    }
}
