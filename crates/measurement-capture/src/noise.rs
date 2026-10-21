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

pub(crate) struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}
impl Biquad {
    pub(crate) fn new(hz: f64, rate: f64, q: f64, high_pass: bool) -> Self {
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
    pub(crate) fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Broadband sweeps are graded over this band, where the arrival and level analyses
/// work: a full-range room-correction sweep is then graded like the 200 Hz–10 kHz
/// alignment sweep, not on the room's rumble below 200 Hz or the hiss above 10 kHz.
pub const BROADBAND_HZ: [f64; 2] = [200.0, 10000.0];

/// The band an acquisition's signal/noise is graded over, and the part of the sweep
/// body, as fractions of its duration, during which the sweep is inside it. A sweep
/// that does not reach 5 kHz, such as a subwoofer's, is graded over its whole range.
pub fn graded_band(sweep: Sweep, rate: f64) -> ([f64; 2], [f64; 2]) {
    let (start, end) = (
        f64::from(sweep.start_hz),
        f64::from(sweep.end_hz).min(rate * 0.45),
    );
    let band = if end >= 5000.0 {
        [start.max(BROADBAND_HZ[0]), end.min(BROADBAND_HZ[1])]
    } else {
        [start, end]
    };
    // An exponential sweep is at `f` after ln(f / start) / ln(end / start) of its body.
    let span = (f64::from(sweep.end_hz) / start).ln();
    let at = |hz: f64| ((hz / start).ln() / span).clamp(0.0, 1.0);
    (band, [at(band[0]), at(band[1])])
}

/// Fourth-order Butterworth high/low passes. This filtered copy is used only
/// for energy ratios; correlation and impulse analysis use the original PCM.
/// `windows` are the sweep bodies; only their part inside the graded band counts.
pub(crate) fn estimate(
    samples: &[f32],
    rate: f64,
    sweep: Sweep,
    reference: f64,
    windows: [(usize, usize); 3],
) -> Result<BandNoise, String> {
    let (band, part) = graded_band(sweep, rate);
    let windows = windows.map(|(start, end)| {
        let length = end.saturating_sub(start) as f64;
        (
            start + (length * part[0]).round() as usize,
            start + (length * part[1]).round() as usize,
        )
    });
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
    fn a_full_range_sweep_is_graded_like_the_alignment_sweep() {
        // The same rumble and hiss under a 1 s 200 Hz–10 kHz sweep and a 3 s full-range
        // one at the same peak: the graded band and its part of the body make them agree.
        let grade = |sweep: Sweep| {
            let source = sweep.samples(48000).unwrap();
            let lead = 24000;
            let mut samples = vec![0.0_f32; lead + 3 * source.len() + 24000];
            for segment in 0..3 {
                for (dst, &src) in samples[lead + segment * source.len()..]
                    .iter_mut()
                    .zip(&source)
                {
                    *dst += src;
                }
            }
            let mut state = 9_u32;
            for (i, x) in samples.iter_mut().enumerate() {
                let t = i as f64 / 48000.0;
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                *x += (0.05 * (std::f64::consts::TAU * 35.0 * t).sin()
                    + (state as f64 / u32::MAX as f64 - 0.5) * 2e-4) as f32;
            }
            let padding = 12000;
            let body = source.len() - 2 * padding;
            let windows = [0, 1, 2].map(|k| {
                let start = lead + k * source.len() + padding;
                (start, start + body)
            });
            estimate(&samples, 48000.0, sweep, lead as f64, windows).unwrap()
        };
        let alignment = grade(Sweep {
            duration_ms: 1000,
            peak_dbfs: -20.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        });
        let full = grade(Sweep {
            duration_ms: 3000,
            peak_dbfs: -20.0,
            start_hz: 20.0,
            end_hz: 20000.0,
        });
        assert_eq!(full.band_hz, [200.0, 10000.0]);
        for (a, b) in alignment
            .signal_to_noise_db
            .iter()
            .zip(full.signal_to_noise_db)
        {
            assert!(*a > 20.0 && (a - b).abs() < 0.5, "{a} vs {b}");
        }
        // A subwoofer's sweep is graded over its own range.
        let sub = Sweep {
            duration_ms: 3000,
            peak_dbfs: -20.0,
            start_hz: 20.0,
            end_hz: 120.0,
        };
        assert_eq!(graded_band(sub, 48000.0), ([20.0, 120.0], [0.0, 1.0]));
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
