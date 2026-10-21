//! Strong early reflections, reported for diagnosis only: a filter designed at one point
//! cannot cancel them elsewhere (a few centimetres move their comb), so the design never
//! tries.
use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

/// Speed of sound for the extra path length, at about 20 °C.
pub const SPEED_OF_SOUND_M_S: f64 = 343.0;
/// Reflections are looked for in this span after the direct sound.
const FIRST_MS: f64 = 0.7;
const LAST_MS: f64 = 25.0;
/// Weaker reflections are not listed.
pub const FLOOR_DB: f64 = -15.0;
/// A peak must stand this far above the envelope on both sides within [`SIDE_MS`].
const PROMINENCE_DB: f64 = 4.0;
const SIDE_MS: f64 = 0.7;
/// Two listed reflections are at least this far apart; the stronger one is kept.
const SEPARATION_MS: f64 = 0.8;
const MAX_LISTED: usize = 6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reflection {
    /// After the direct sound.
    pub delay_ms: f64,
    /// Envelope peak relative to the direct sound's, in the analysis band.
    pub level_db: f64,
    /// How much longer its path is than the direct sound's.
    pub extra_path_cm: f64,
}

/// The envelope of `samples` band-limited to `band` (raised-cosine edges over a third
/// of an octave), from 10 ms before `peak` to [`LAST_MS`] after it, and the frame of
/// `peak` in it.
fn envelope(samples: &[f32], rate: f64, peak: usize, band: [f64; 2]) -> Option<(Vec<f64>, usize)> {
    let before = (0.01 * rate) as usize;
    let start = peak.checked_sub(before)?;
    let end = (peak + ((LAST_MS + 5.0) / 1000.0 * rate) as usize).min(samples.len());
    let length = end - start;
    let n = (length * 2).next_power_of_two();
    let mut spectrum: Vec<Complex<f64>> = (0..n)
        .map(|i| {
            Complex::new(
                if i < length {
                    f64::from(samples[start + i])
                } else {
                    0.0
                },
                0.0,
            )
        })
        .collect();
    let mut planner = FftPlanner::<f64>::new();
    planner.plan_fft_forward(n).process(&mut spectrum);
    let edge = 2_f64.powf(1.0 / 3.0);
    for (k, value) in spectrum.iter_mut().enumerate() {
        // Analytic signal: negative frequencies removed, positive ones doubled.
        let hz = k as f64 * rate / n as f64;
        let gain = if k == 0 || k >= n / 2 || hz < band[0] / edge || hz > band[1] * edge {
            0.0
        } else if hz < band[0] {
            let t = (hz * edge / band[0]).log2() / edge.log2();
            0.5 - 0.5 * (std::f64::consts::PI * t).cos()
        } else if hz > band[1] {
            let t = (band[1] * edge / hz).log2() / edge.log2();
            0.5 - 0.5 * (std::f64::consts::PI * t).cos()
        } else {
            1.0
        };
        *value *= 2.0 * gain;
    }
    planner.plan_fft_inverse(n).process(&mut spectrum);
    Some((
        spectrum[..length]
            .iter()
            .map(|v| v.norm() / n as f64)
            .collect(),
        before,
    ))
}

/// Strong early reflections of one response in `band`: delay, level and extra path,
/// at most six, in order of delay.
pub fn early_reflections(
    samples: &[f32],
    rate: f64,
    peak: usize,
    band: [f64; 2],
) -> Vec<Reflection> {
    let Some((envelope, at)) = envelope(samples, rate, peak, band) else {
        return vec![];
    };
    let ms = |m: f64| (m / 1000.0 * rate).round() as usize;
    // The direct sound: the envelope's peak within 1 ms of the broadband peak.
    let direct = (at.saturating_sub(ms(1.0))..(at + ms(1.0)).min(envelope.len()))
        .max_by(|&a, &b| envelope[a].total_cmp(&envelope[b]))
        .unwrap_or(at);
    let reference = envelope[direct].max(1e-30);
    let db: Vec<f64> = envelope
        .iter()
        .map(|v| 20.0 * (v / reference).max(1e-9).log10())
        .collect();
    let side = ms(SIDE_MS).max(1);
    let mut found: Vec<(usize, f64)> = vec![];
    for i in (direct + ms(FIRST_MS))..(direct + ms(LAST_MS)).min(db.len() - side - 1) {
        if db[i] < FLOOR_DB || db[i] < db[i - 1] || db[i] < db[i + 1] {
            continue;
        }
        let left = db[i - side..i].iter().copied().fold(f64::MAX, f64::min);
        let right = db[i + 1..=i + side]
            .iter()
            .copied()
            .fold(f64::MAX, f64::min);
        if db[i] - left.max(right) >= PROMINENCE_DB {
            found.push((i, db[i]));
        }
    }
    // Keep the strongest of peaks closer than the separation.
    found.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut kept: Vec<(usize, f64)> = vec![];
    for (i, level) in found {
        if kept
            .iter()
            .all(|&(k, _)| (k as f64 - i as f64).abs() >= SEPARATION_MS / 1000.0 * rate)
        {
            kept.push((i, level));
        }
        if kept.len() == MAX_LISTED {
            break;
        }
    }
    kept.sort_by_key(|&(i, _)| i);
    kept.into_iter()
        .map(|(i, level)| {
            let delay = (i - direct) as f64 / rate;
            Reflection {
                delay_ms: round(delay * 1000.0, 2),
                level_db: round(level, 1),
                extra_path_cm: round(delay * SPEED_OF_SOUND_M_S * 100.0, 0),
            }
        })
        .collect()
}

fn round(value: f64, decimals: i32) -> f64 {
    let scale = 10_f64.powi(decimals);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reflection_is_found_with_its_delay_level_and_path() {
        let rate: f64 = 48000.0;
        let peak = 4800;
        let mut samples = vec![0.0_f32; peak + 16000];
        samples[peak] = 1.0;
        samples[peak + (0.0023 * rate).round() as usize] = 0.5;
        samples[peak + (0.0110 * rate).round() as usize] = -0.1;
        let found = early_reflections(&samples, rate, peak, [300.0, 8000.0]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!((found[0].delay_ms - 2.29).abs() < 0.05);
        assert!((found[0].level_db + 6.0).abs() < 0.5, "{found:?}");
        assert!((found[0].extra_path_cm - 79.0).abs() < 2.0);
    }
}
