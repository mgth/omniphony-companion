//! Minimum-phase FIR from a magnitude curve, and the response a set of taps realises.
use super::spectrum::interpolate;
use rustfft::{FftPlanner, num_complex::Complex};

/// The cepstrum's FFT is this many times the tap count (at least [`MIN_FFT`]), so the
/// minimum-phase response it folds has decayed long before it wraps around.
const OVERSAMPLING: usize = 8;
const MIN_FFT: usize = 65_536;
/// The last part of the taps fades out over a raised cosine.
const FADE_FRACTION: f64 = 0.25;

fn fft_size(taps: usize) -> usize {
    (taps * OVERSAMPLING).next_power_of_two().max(MIN_FFT)
}

/// Minimum-phase taps whose magnitude follows `gain_db` (given on the log grid
/// `frequencies`, held outside it, 0 dB at DC), by the real cepstrum: the log
/// magnitude's inverse transform, folded onto positive quefrencies, exponentiated back.
/// The taps are truncated to `taps` with a raised-cosine fade over their last quarter.
pub(crate) fn minimum_phase(
    frequencies: &[f64],
    gain_db: &[f64],
    rate: f64,
    taps: usize,
) -> Vec<f64> {
    let n = fft_size(taps);
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(n);
    let inverse = planner.plan_fft_inverse(n);
    let bin_hz = rate / n as f64;
    let mut spectrum: Vec<Complex<f64>> = (0..n)
        .map(|k| {
            let bin = k.min(n - k);
            let db = if bin == 0 {
                0.0
            } else {
                interpolate(frequencies, gain_db, bin as f64 * bin_hz)
            };
            Complex::new(db / 20.0 * std::f64::consts::LN_10, 0.0)
        })
        .collect();
    inverse.process(&mut spectrum);
    // Real cepstrum (scaled by n); fold the anticausal half onto the causal one.
    for (k, value) in spectrum.iter_mut().enumerate() {
        let fold = match k {
            0 => 1.0,
            k if k < n / 2 => 2.0,
            k if k == n / 2 => 1.0,
            _ => 0.0,
        };
        *value = Complex::new(value.re * fold / n as f64, 0.0);
    }
    forward.process(&mut spectrum);
    for value in &mut spectrum {
        *value = value.exp();
    }
    inverse.process(&mut spectrum);
    let fade = ((taps as f64 * FADE_FRACTION) as usize).max(1);
    (0..taps)
        .map(|i| {
            let from_end = taps - 1 - i;
            let weight = if from_end < fade {
                0.5 - 0.5 * (std::f64::consts::PI * (from_end as f64 + 0.5) / fade as f64).cos()
            } else {
                1.0
            };
            spectrum[i].re / n as f64 * weight
        })
        .collect()
}

/// What `taps` realise: their gain in dB at each of `frequencies`, and the largest gain
/// at any frequency up to Nyquist.
pub(crate) fn realised_db(taps: &[f64], rate: f64, frequencies: &[f64]) -> (Vec<f64>, f64) {
    let n = fft_size(taps.len());
    let mut spectrum: Vec<Complex<f64>> = (0..n)
        .map(|i| Complex::new(taps.get(i).copied().unwrap_or(0.0), 0.0))
        .collect();
    FftPlanner::<f64>::new()
        .plan_fft_forward(n)
        .process(&mut spectrum);
    let power: Vec<f64> = spectrum[..=n / 2].iter().map(|v| v.norm_sqr()).collect();
    let bin_hz = rate / n as f64;
    let curve = frequencies
        .iter()
        .map(|&hz| {
            let x = hz / bin_hz;
            let below = (x.floor() as usize).min(power.len() - 2);
            let t = (x - below as f64).clamp(0.0, 1.0);
            super::spectrum::decibels(power[below] * (1.0 - t) + power[below + 1] * t)
        })
        .collect();
    let largest = power.iter().copied().fold(0.0, f64::max);
    (curve, super::spectrum::decibels(largest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room_correction::spectrum::grid;

    #[test]
    fn a_flat_curve_gives_a_unit_impulse() {
        let frequencies = grid(10.0, 20000.0);
        let taps = minimum_phase(&frequencies, &vec![0.0; frequencies.len()], 48000.0, 1024);
        assert!((taps[0] - 1.0).abs() < 1e-9);
        assert!(taps[1..].iter().all(|v| v.abs() < 1e-9));
    }

    #[test]
    fn a_shelf_is_realised_with_its_energy_up_front() {
        // −6 dB above 2 kHz, in a smooth step.
        let frequencies = grid(10.0, 20000.0);
        let curve: Vec<f64> = frequencies
            .iter()
            .map(|&f| -6.0 / (1.0 + (2000.0 / f).powi(4)))
            .collect();
        let taps = minimum_phase(&frequencies, &curve, 48000.0, 2048);
        let (realised, largest) = realised_db(&taps, 48000.0, &frequencies);
        for ((f, want), got) in frequencies.iter().zip(&curve).zip(&realised) {
            assert!((want - got).abs() < 0.05, "{f}: {want} vs {got}");
        }
        assert!(largest < 0.05);
        let energy: f64 = taps.iter().map(|v| v * v).sum();
        let early: f64 = taps[..64].iter().map(|v| v * v).sum();
        assert!(early / energy > 0.999);
    }
}
