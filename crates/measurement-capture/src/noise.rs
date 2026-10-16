//! Offline noise estimates over the frequencies actually excited by the sweep.
use omniphony_osc_contract::measurement::Sweep;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandNoise {
    pub band_hz: [f64; 2],
    pub noise_rms_dbfs: f64,
    /// (Signal + noise) / noise, with identical filtering in every window.
    pub signal_to_noise_db: [f64; 3],
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}
impl Biquad {
    fn new(hz: f64, rate: f64, q: f64, high_pass: bool) -> Self {
        let omega = std::f64::consts::TAU * hz / rate;
        let c = omega.cos();
        let alpha = omega.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = if high_pass {
            [(1.0 + c) * 0.5, -(1.0 + c), (1.0 + c) * 0.5]
        } else {
            [(1.0 - c) * 0.5, 1.0 - c, (1.0 - c) * 0.5]
        };
        Self {
            b: b.map(|x| x / a0),
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Fourth-order Butterworth high/low passes. This filtered copy is used only
/// for energy ratios; correlation and impulse analysis use the original PCM.
pub(crate) fn estimate(
    samples: &[f32],
    rate: f64,
    sweep: Sweep,
    reference: f64,
    windows: [(usize, usize); 3],
) -> Result<BandNoise, String> {
    let band = [
        f64::from(sweep.start_hz),
        f64::from(sweep.end_hz).min(rate * 0.45),
    ];
    if !rate.is_finite()
        || !(7900.0..=193000.0).contains(&rate)
        || !reference.is_finite()
        || reference < 0.0
        || !band.iter().all(|v| v.is_finite())
        || band[0] < 20.0
        || band[1] <= band[0]
        || samples.len() > 192_000 * crate::MAX_SECONDS as usize
        || samples.iter().any(|v| !v.is_finite())
    {
        return Err("invalid excited-band noise input".into());
    }
    let mut filters = [
        Biquad::new(band[0], rate, 0.541196100146197, true),
        Biquad::new(band[0], rate, 1.306562964876377, true),
        Biquad::new(band[1], rate, 0.541196100146197, false),
        Biquad::new(band[1], rate, 1.306562964876377, false),
    ];
    let filtered: Vec<f32> = samples
        .iter()
        .map(|&x| {
            filters
                .iter_mut()
                .fold(f64::from(x), |v, filter| filter.process(v)) as f32
        })
        .collect();
    let noise = crate::timing::reference_noise(&filtered, reference, rate)?;
    let mut ratios = [0.0; 3];
    for (i, (start, end)) in windows.into_iter().enumerate() {
        let body = filtered
            .get(start..end)
            .filter(|s| !s.is_empty())
            .ok_or("missing excited-band window")?;
        let rms =
            (body.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / body.len() as f64).sqrt();
        ratios[i] = 20.0 * (rms.max(1e-10) / noise).log10();
    }
    Ok(BandNoise {
        band_hz: band,
        noise_rms_dbfs: 20.0 * noise.log10(),
        signal_to_noise_db: ratios,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn in_band_noise_retains_the_expected_power_ratio() {
        let rate = 48000.0;
        let windows = [(24000, 48000), (96000, 120000), (168000, 192000)];
        let mut samples = vec![0.0; 240000];
        for (i, x) in samples.iter_mut().enumerate() {
            let t = i as f64 / rate;
            *x = (0.001 * (std::f64::consts::TAU * 1500.0 * t).sin()) as f32;
            if windows.iter().any(|&(a, b)| (a..b).contains(&i)) {
                *x += (0.01 * (std::f64::consts::TAU * 1000.0 * t).sin()) as f32;
            }
        }
        let report = estimate(
            &samples,
            rate,
            Sweep {
                duration_ms: 500,
                peak_dbfs: -24.0,
                start_hz: 200.0,
                end_hz: 10000.0,
            },
            12000.0,
            windows,
        )
        .unwrap();
        let expected = 10.0 * 101_f64.log10();
        for value in report.signal_to_noise_db {
            assert!((value - expected).abs() < 0.1, "{value} vs {expected}");
        }
    }

    #[test]
    fn out_of_band_rumble_does_not_hide_an_in_band_signal() {
        let rate = 48000.0;
        let mut samples = vec![0.0; 240000];
        let windows = [(24000, 48000), (96000, 120000), (168000, 192000)];
        for (i, x) in samples.iter_mut().enumerate() {
            let t = i as f64 / rate;
            *x = (0.1 * (std::f64::consts::TAU * 50.0 * t).sin()
                + 0.0001 * (std::f64::consts::TAU * 1500.0 * t).sin()) as f32;
            if windows.iter().any(|&(a, b)| (a..b).contains(&i)) {
                *x += (0.01 * (std::f64::consts::TAU * 1000.0 * t).sin()) as f32;
            }
        }
        let report = estimate(
            &samples,
            rate,
            Sweep {
                duration_ms: 500,
                peak_dbfs: -24.0,
                start_hz: 200.0,
                end_hz: 10000.0,
            },
            12000.0,
            windows,
        )
        .unwrap();
        assert!(report.signal_to_noise_db.iter().all(|v| *v > 25.0));
        let mut invalid = windows;
        invalid[1] = (240001, 240002);
        assert!(
            estimate(
                &samples,
                rate,
                Sweep {
                    duration_ms: 500,
                    peak_dbfs: -24.0,
                    start_hz: 200.0,
                    end_hz: 10000.0
                },
                12000.0,
                invalid
            )
            .is_err()
        );
    }
}
