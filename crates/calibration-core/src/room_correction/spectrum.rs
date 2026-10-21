//! Frequency-dependent windowing and fractional-octave smoothing on a shared log grid.
//!
//! Everything here runs offline on stored responses; nothing is called by an audio
//! callback.
use std::f64::consts::{PI, TAU};

/// Grid points per octave, anchored at 1 kHz like the stored magnitude, so curves of
/// takes at any rate share frequencies.
pub const POINTS_PER_OCTAVE: f64 = 48.0;
/// The window rises over this span, ending this long before the direct sound's peak.
const RISE_MS: f64 = 1.0;
const LEAD_MS: f64 = 0.5;
/// The longest window: what a stored response holds after its anchor, less margin.
pub(crate) const MAX_WINDOW_SECONDS: f64 = 0.3;

/// The grid frequencies `1000 · 2^(k/48)` from `low` to `high`, both included when on
/// the grid.
pub fn grid(low: f64, high: f64) -> Vec<f64> {
    let first = (POINTS_PER_OCTAVE * (low / 1000.0).log2()).ceil() as i64;
    let last = (POINTS_PER_OCTAVE * (high / 1000.0).log2()).floor() as i64;
    (first..=last)
        .map(|k| 1000.0 * 2_f64.powf(k as f64 / POINTS_PER_OCTAVE))
        .collect()
}

/// How long the window lasts after the direct sound at `hz`: `cycles` periods above
/// the modal region, so late reflections, which a filter cannot correct for more than
/// one point, stay out; the longest window below it, so room modes are resolved. The
/// length moves on a log scale from one to the other between `modal_hz / 1.5` and
/// `modal_hz · 1.67`.
pub(crate) fn window_seconds(hz: f64, cycles: f64, modal_hz: f64, longest: f64) -> f64 {
    let (low, high) = (modal_hz / 1.5, modal_hz * 1.67);
    let short = (cycles / hz).min(longest);
    if hz <= low {
        return longest;
    }
    if hz >= high {
        return short;
    }
    let t = (hz / low).ln() / (high / low).ln();
    let blend = (longest.ln() * (1.0 - t) + (cycles / high).min(longest).ln() * t).exp();
    blend.clamp(short, longest)
}

/// Power of `samples` at each of `frequencies` in a frequency-dependent window: it rises
/// over 1 ms ending 0.5 ms before `peak` (the direct sound), stays flat for half its
/// length and falls over the other half (raised cosines). A lone impulse at `peak`
/// reads 1 (0 dB) at every frequency. `NaN` where the window does not fit.
pub(crate) fn fdw_power(
    samples: &[f32],
    rate: f64,
    peak: usize,
    frequencies: &[f64],
    cycles: f64,
    modal_hz: f64,
) -> Vec<f64> {
    let rise = ((RISE_MS / 1000.0 * rate).round() as usize).max(1);
    let lead = (LEAD_MS / 1000.0 * rate).round() as usize;
    let Some(start) = peak.checked_sub(rise + lead) else {
        return vec![f64::NAN; frequencies.len()];
    };
    let available = (samples.len().saturating_sub(peak + 1)) as f64 / rate;
    let longest = MAX_WINDOW_SECONDS.min(available);
    frequencies
        .iter()
        .map(|&hz| {
            let length = (window_seconds(hz, cycles, modal_hz, longest) * rate).round() as usize;
            if length < 4 || peak + length >= samples.len() {
                return f64::NAN;
            }
            let flat = length / 2;
            let fall = length - flat;
            let omega = TAU * hz / rate;
            // e^{−jω(n − peak)} by recurrence, renormalised now and then.
            let step = (omega.cos(), -omega.sin());
            let first = -omega * (start as f64 - peak as f64);
            let mut phasor = (first.cos(), first.sin());
            // The falling edge cos(π i / fall), by recurrence too.
            let edge = (PI / fall as f64).cos();
            let edge_sin = (PI / fall as f64).sin();
            let mut fall_phasor = (1.0, 0.0);
            let (mut re, mut im) = (0.0, 0.0);
            for (offset, &sample) in samples[start..=peak + length].iter().enumerate() {
                let n = start + offset;
                let weight = if offset < rise {
                    0.5 - 0.5 * (PI * offset as f64 / rise as f64).cos()
                } else if n <= peak + flat {
                    1.0
                } else {
                    let w = 0.5 + 0.5 * fall_phasor.0;
                    fall_phasor = (
                        fall_phasor.0 * edge - fall_phasor.1 * edge_sin,
                        fall_phasor.0 * edge_sin + fall_phasor.1 * edge,
                    );
                    w
                };
                let value = f64::from(sample) * weight;
                re += value * phasor.0;
                im += value * phasor.1;
                phasor = (
                    phasor.0 * step.0 - phasor.1 * step.1,
                    phasor.0 * step.1 + phasor.1 * step.0,
                );
                if offset % 1024 == 1023 {
                    let norm = phasor.0.hypot(phasor.1);
                    phasor = (phasor.0 / norm, phasor.1 / norm);
                }
            }
            re * re + im * im
        })
        .collect()
}

/// Power averaged over `width(hz)` octaves around each grid point (`NaN` stays `NaN`;
/// missing neighbours are left out of the average).
pub(crate) fn smooth(power: &[f64], frequencies: &[f64], width: impl Fn(f64) -> f64) -> Vec<f64> {
    let n = power.len();
    (0..n)
        .map(|i| {
            if !power[i].is_finite() {
                return f64::NAN;
            }
            let half = (width(frequencies[i]) * POINTS_PER_OCTAVE / 2.0).round() as usize;
            let (sum, count) = power[i.saturating_sub(half)..(i + half + 1).min(n)]
                .iter()
                .filter(|v| v.is_finite())
                .fold((0.0, 0usize), |(s, c), v| (s + v, c + 1));
            sum / count as f64
        })
        .collect()
}

/// The same moving average on dB values (for a correction curve): never exceeds the
/// largest or falls below the smallest of the values it averages.
pub(crate) fn smooth_db(db: &[f64], frequencies: &[f64], width: impl Fn(f64) -> f64) -> Vec<f64> {
    let n = db.len();
    (0..n)
        .map(|i| {
            let half = (width(frequencies[i]) * POINTS_PER_OCTAVE / 2.0).round() as usize;
            let slice = &db[i.saturating_sub(half)..(i + half + 1).min(n)];
            slice.iter().sum::<f64>() / slice.len() as f64
        })
        .collect()
}

/// `fine` octaves below `fine_below_hz`, `coarse` from twice that on, log-interpolated
/// between.
pub(crate) fn smoothing_width(hz: f64, fine: f64, coarse: f64, fine_below_hz: f64) -> f64 {
    if hz <= fine_below_hz {
        fine
    } else if hz >= 2.0 * fine_below_hz {
        coarse
    } else {
        let t = (hz / fine_below_hz).log2();
        (fine.ln() * (1.0 - t) + coarse.ln() * t).exp()
    }
}

pub(crate) fn decibels(power: f64) -> f64 {
    10.0 * power.max(1e-30).log10()
}

/// Mean of the finite values of `db` at frequencies within `band`.
pub(crate) fn band_mean(db: &[f64], frequencies: &[f64], band: [f64; 2]) -> Option<f64> {
    let (sum, count) = frequencies
        .iter()
        .zip(db)
        .filter(|(f, v)| (band[0]..=band[1]).contains(*f) && v.is_finite())
        .fold((0.0, 0usize), |(s, c), (_, v)| (s + v, c + 1));
    (count > 0).then(|| sum / count as f64)
}

/// Root mean square of `a − b` over `band`, where both are finite.
pub(crate) fn rms_difference(
    a: &[f64],
    b: &[f64],
    frequencies: &[f64],
    band: [f64; 2],
) -> Option<f64> {
    let (sum, count) = frequencies
        .iter()
        .zip(a.iter().zip(b))
        .filter(|(f, (x, y))| (band[0]..=band[1]).contains(*f) && x.is_finite() && y.is_finite())
        .fold((0.0, 0usize), |(s, c), (_, (x, y))| {
            (s + (x - y).powi(2), c + 1)
        });
    (count > 0).then(|| (sum / count as f64).sqrt())
}

/// Root mean square of `a − b` over `band` after removing its mean: how far apart their
/// shapes are, whatever their levels.
pub(crate) fn rms_shape_difference(
    a: &[f64],
    b: &[f64],
    frequencies: &[f64],
    band: [f64; 2],
) -> Option<f64> {
    let differences: Vec<f64> = frequencies
        .iter()
        .zip(a.iter().zip(b))
        .filter(|(f, (x, y))| (band[0]..=band[1]).contains(*f) && x.is_finite() && y.is_finite())
        .map(|(_, (x, y))| x - y)
        .collect();
    let n = differences.len() as f64;
    let mean = differences.iter().sum::<f64>() / n;
    (n > 0.0).then(|| (differences.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n).sqrt())
}

/// Linear interpolation over log frequency of a curve on `frequencies`, held at its
/// first and last values outside.
pub(crate) fn interpolate(frequencies: &[f64], values: &[f64], hz: f64) -> f64 {
    let at = frequencies.partition_point(|&f| f < hz);
    if at == 0 {
        return values[0];
    }
    if at == frequencies.len() {
        return values[at - 1];
    }
    let (f0, f1) = (frequencies[at - 1], frequencies[at]);
    let t = (hz / f0).ln() / (f1 / f0).ln();
    values[at - 1] + (values[at] - values[at - 1]) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse(rate: f64, taps: &[(f64, f64)]) -> (Vec<f32>, usize) {
        let peak = (rate * 0.1) as usize;
        let mut samples = vec![0.0_f32; peak + (rate * 0.35) as usize];
        for &(ms, gain) in taps {
            samples[peak + (rate * ms / 1000.0).round() as usize] += gain as f32;
        }
        (samples, peak)
    }

    #[test]
    fn a_lone_impulse_reads_0_db_and_the_grid_matches_the_stored_magnitude() {
        let (samples, peak) = impulse(48000.0, &[(0.0, 1.0)]);
        let frequencies = grid(20.0, 20000.0);
        assert!(frequencies.contains(&1000.0));
        assert!((frequencies[1] / frequencies[0] - 2_f64.powf(1.0 / 48.0)).abs() < 1e-12);
        let power = fdw_power(&samples, 48000.0, peak, &frequencies, 8.0, 300.0);
        for (hz, p) in frequencies.iter().zip(&power) {
            assert!(decibels(*p).abs() < 1e-6, "{hz}: {p}");
        }
    }

    #[test]
    fn a_late_reflection_is_outside_the_short_windows_but_inside_the_long_ones() {
        // 40 ms after the direct sound: inside the 300 ms bass window, outside the
        // 8-cycle window above 1 kHz (8 ms).
        let (samples, peak) = impulse(48000.0, &[(0.0, 1.0), (40.0, 0.5)]);
        let frequencies = grid(20.0, 20000.0);
        let power = fdw_power(&samples, 48000.0, peak, &frequencies, 8.0, 300.0);
        let db: Vec<f64> = power.iter().map(|&p| decibels(p)).collect();
        let high = frequencies.iter().position(|&f| f >= 2000.0).unwrap();
        assert!(db[high..].iter().all(|v| v.abs() < 0.01));
        let low: Vec<_> = (0..frequencies.len())
            .filter(|&i| frequencies[i] < 150.0)
            .map(|i| db[i])
            .collect();
        let (lo, hi) = low
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(hi > 3.0 && lo < -5.0, "comb in the bass: {lo}..{hi}");
    }

    #[test]
    fn windows_shorten_with_frequency_and_smoothing_widens() {
        let mut last = f64::MAX;
        for hz in grid(20.0, 20000.0) {
            let w = window_seconds(hz, 8.0, 300.0, 0.3);
            assert!(
                w <= last + 1e-12 && w >= (8.0 / hz).min(0.3) - 1e-12,
                "{hz}"
            );
            last = w;
        }
        assert_eq!(window_seconds(100.0, 8.0, 300.0, 0.3), 0.3);
        assert!((window_seconds(1000.0, 8.0, 300.0, 0.3) - 0.008).abs() < 1e-12);
        assert_eq!(
            smoothing_width(100.0, 1.0 / 24.0, 1.0 / 3.0, 300.0),
            1.0 / 24.0
        );
        assert_eq!(
            smoothing_width(900.0, 1.0 / 24.0, 1.0 / 3.0, 300.0),
            1.0 / 3.0
        );
        let mid = smoothing_width(424.0, 1.0 / 24.0, 1.0 / 3.0, 300.0);
        assert!(mid > 1.0 / 24.0 && mid < 1.0 / 3.0);
    }
}
