//! The magnitude of the unweighted swept-sine response, kept for room-correction design.
//!
//! It is computed once after capture from [`FullResponse`], never in an audio callback.
//! Unlike the arrival detector's band-weighted response, nothing here shapes the
//! spectrum: a gentle window around the anchor, then fractional-octave power averages on
//! a fixed log grid. The microphone calibration, when there is one, is subtracted from a
//! copy; the raw magnitude and the response itself stay as measured.
use crate::{calibration::Calibration, impulse::FullResponse};
use omniphony_osc_contract::measurement::Sweep;
use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

pub const METHOD: &str = "unweighted-sweep-response-v1";
/// Grid points per octave, anchored at 1 kHz so every take shares the same frequencies.
pub const POINTS_PER_OCTAVE: f64 = 48.0;
/// The window opens this long before the anchor, over a raised-cosine rise of
/// [`RISE_MS`], and closes over [`FALL_MS`] at the end of the stored response.
const LEAD_MS: f64 = 10.0;
const RISE_MS: f64 = 5.0;
const FALL_MS: f64 = 50.0;
/// Deconvolution noise comes from the response before the anchor, ending this early.
const QUIET_GUARD_MS: f64 = 20.0;
const MAX_FFT: usize = 524_288;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagnitudeReport {
    pub method: String,
    /// The window, in ms from the correlation anchor, including its raised-cosine edges.
    pub window_ms: [f64; 2],
    /// Width of the power average around each grid point.
    pub smoothing_octaves: f64,
    pub frequency_hz: Vec<f64>,
    /// Transfer magnitude of `reference_before`, `speaker`, `reference_after`, in dB
    /// relative to a unit digital source: speaker, room and microphone together.
    pub magnitude_db: [Vec<f32>; 3],
    /// Deconvolution noise scaled to the same window, from the quiet lead before the
    /// anchor: `magnitude_db − noise_db` approximates the signal-to-noise ratio.
    pub noise_db: [Vec<f32>; 3],
    /// `magnitude_db` minus the microphone's deviation. Absent without a calibration.
    pub calibrated_magnitude_db: Option<[Vec<f32>; 3]>,
}

/// The grid frequencies `1000 · 2^(k/48)` from the sweep start to its end, below 0.45
/// times the response rate.
pub fn grid(sweep: Sweep, rate: f64) -> Vec<f64> {
    let low = f64::from(sweep.start_hz);
    let high = f64::from(sweep.end_hz).min(rate * 0.45);
    let first = (POINTS_PER_OCTAVE * (low / 1000.0).log2()).ceil() as i64;
    let last = (POINTS_PER_OCTAVE * (high / 1000.0).log2()).floor() as i64;
    (first..=last)
        .map(|k| 1000.0 * 2_f64.powf(k as f64 / POINTS_PER_OCTAVE))
        .collect()
}

pub fn magnitude(
    full: &FullResponse,
    sweep: Sweep,
    calibration: Option<&Calibration>,
) -> Result<MagnitudeReport, String> {
    let rate = full.rate;
    let len = full.segments[0].len();
    let lead = (rate * LEAD_MS / 1000.0).round() as usize;
    let rise = (rate * RISE_MS / 1000.0).round().max(1.0) as usize;
    let fall = (rate * FALL_MS / 1000.0).round().max(1.0) as usize;
    let quiet_end = full
        .anchor
        .checked_sub((rate * QUIET_GUARD_MS / 1000.0).round() as usize)
        .filter(|&end| end >= (rate * 0.02) as usize);
    let frequencies = grid(sweep, rate);
    if !rate.is_finite()
        || !(7900.0..=193000.0).contains(&rate)
        || full.segments.iter().any(|s| s.len() != len)
        || full.segments.iter().flatten().any(|v| !v.is_finite())
        || full.anchor < lead
        || full.anchor + fall >= len
        || frequencies.len() < 2
    {
        return Err("invalid response for the magnitude".into());
    }
    let quiet_end = quiet_end.ok_or("the response lacks a quiet lead before its anchor")?;
    let start = full.anchor - lead;
    let n = ((len - start) * 2).next_power_of_two();
    if n > MAX_FFT {
        return Err("response FFT exceeds the offline bound".into());
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(n);
    let bin_hz = rate / n as f64;
    let mut magnitude_db: [Vec<f32>; 3] = Default::default();
    let mut noise_db: [Vec<f32>; 3] = Default::default();
    for (index, segment) in full.segments.iter().enumerate() {
        let mut h = vec![Complex::default(); n];
        let mut window_energy = 0.0;
        for (i, &sample) in segment[start..].iter().enumerate() {
            let from_end = len - start - 1 - i;
            let weight = raised(i, rise) * raised(from_end, fall);
            h[i].re = f64::from(sample) * weight;
            window_energy += weight * weight;
        }
        let mut noise = vec![Complex::default(); n];
        let mut quiet_energy = 0.0;
        for (i, &sample) in segment[..quiet_end].iter().enumerate() {
            let weight =
                0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / (quiet_end - 1) as f64).cos());
            noise[i].re = f64::from(sample) * weight;
            quiet_energy += weight * weight;
        }
        forward.process(&mut h);
        forward.process(&mut noise);
        let noise_scale = window_energy / quiet_energy.max(1e-300);
        magnitude_db[index] = frequencies
            .iter()
            .map(|&hz| decibels(band_power(&h, hz, bin_hz)))
            .collect();
        noise_db[index] = frequencies
            .iter()
            .map(|&hz| decibels(band_power(&noise, hz, bin_hz) * noise_scale))
            .collect();
    }
    let calibrated_magnitude_db = calibration.map(|calibration| {
        magnitude_db.each_ref().map(|segment| {
            segment
                .iter()
                .zip(&frequencies)
                .map(|(&db, &hz)| (f64::from(db) - calibration.deviation_db(hz)) as f32)
                .collect()
        })
    });
    Ok(MagnitudeReport {
        method: METHOD.into(),
        window_ms: [
            -(lead as f64) / rate * 1000.0,
            (len - 1 - full.anchor) as f64 / rate * 1000.0,
        ],
        smoothing_octaves: 1.0 / POINTS_PER_OCTAVE,
        frequency_hz: frequencies,
        magnitude_db,
        noise_db,
        calibrated_magnitude_db,
    })
}

/// A raised-cosine edge: 0 at the first sample, 1 from `width` samples on.
fn raised(i: usize, width: usize) -> f64 {
    let edge = (i as f64 / width as f64).min(1.0);
    0.5 - 0.5 * (std::f64::consts::PI * edge).cos()
}

/// Mean power over `hz · 2^(±1/96)`; between two bins when the band holds none.
fn band_power(spectrum: &[Complex<f64>], hz: f64, bin_hz: f64) -> f64 {
    let half = 2_f64.powf(0.5 / POINTS_PER_OCTAVE);
    let lo = ((hz / half / bin_hz).ceil() as usize).max(1);
    let hi = ((hz * half / bin_hz).floor() as usize).min(spectrum.len() / 2 - 1);
    if lo <= hi {
        return spectrum[lo..=hi].iter().map(|v| v.norm_sqr()).sum::<f64>() / (hi - lo + 1) as f64;
    }
    let x = hz / bin_hz;
    let below = (x.floor() as usize).min(spectrum.len() / 2 - 2);
    let fraction = (x - below as f64).clamp(0.0, 1.0);
    spectrum[below].norm_sqr() * (1.0 - fraction) + spectrum[below + 1].norm_sqr() * fraction
}

/// Rounded to 0.001 dB, which keeps the stored text compact; floored at −300 dB.
fn decibels(power: f64) -> f32 {
    let db = (10.0 * power.max(1e-30).log10()).max(-300.0);
    ((db * 1000.0).round() / 1000.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sweep() -> Sweep {
        Sweep {
            duration_ms: 1000,
            peak_dbfs: -20.0,
            start_hz: 20.0,
            end_hz: 20000.0,
        }
    }
    fn response(rate: f64, taps: &[(f64, f64)]) -> FullResponse {
        let anchor = (rate * 0.1).round() as usize;
        let len = anchor + (rate * 0.35).round() as usize + 1;
        let mut segment = vec![0.0_f32; len];
        for &(ms, gain) in taps {
            segment[anchor + (rate * ms / 1000.0).round() as usize] += gain as f32;
        }
        FullResponse {
            rate,
            anchor,
            anchor_offsets: [0.0; 3],
            target_offset: 0.0,
            segments: [segment.clone(), segment.clone(), segment],
        }
    }
    #[test]
    fn a_pure_gain_reads_flat_and_a_calibration_is_subtracted_from_a_copy() {
        let full = response(48000.0, &[(0.0, 0.5)]);
        let calibration = Calibration::parse("20 -3\n1000 0\n20000 2").unwrap();
        let report = magnitude(&full, sweep(), Some(&calibration)).unwrap();
        assert_eq!(report.frequency_hz.len(), report.magnitude_db[1].len());
        assert!(report.frequency_hz[0] >= 20.0 && *report.frequency_hz.last().unwrap() <= 20000.0);
        let expected = 20.0 * 0.5_f64.log10();
        for (i, &hz) in report.frequency_hz.iter().enumerate() {
            let raw = f64::from(report.magnitude_db[1][i]);
            assert!((raw - expected).abs() < 0.01, "{hz} Hz: {raw}");
            let calibrated = f64::from(report.calibrated_magnitude_db.as_ref().unwrap()[1][i]);
            assert!((calibrated - (raw - calibration.deviation_db(hz))).abs() < 0.002);
            assert!(report.noise_db[1][i] < -250.0);
        }
        assert!(
            magnitude(&full, sweep(), None)
                .unwrap()
                .calibrated_magnitude_db
                .is_none()
        );
    }
    #[test]
    fn an_echo_shows_its_comb_and_the_grid_is_shared_between_rates() {
        // A 2 ms echo at half amplitude: peaks of +3.5 dB at multiples of 500 Hz,
        // dips of −6 dB halfway between.
        let full = response(48000.0, &[(0.0, 1.0), (2.0, 0.5)]);
        let report = magnitude(&full, sweep(), None).unwrap();
        let at = |hz: f64| {
            let i = report
                .frequency_hz
                .iter()
                .position(|&f| (f / hz - 1.0).abs() < 0.008)
                .unwrap();
            (report.frequency_hz[i], f64::from(report.magnitude_db[1][i]))
        };
        let (hz, peak) = at(1000.0);
        assert_eq!(hz, 1000.0);
        assert!((peak - 20.0 * 1.5_f64.log10()).abs() < 0.1, "{peak}");
        let (hz, dip) = at(750.0);
        let exact = 20.0
            * (1.0 + 0.5 * (std::f64::consts::TAU * hz * 0.002).cos())
                .abs()
                .log10();
        assert!(dip < -3.0 && (dip - exact).abs() < 1.0, "{dip} vs {exact}");
        let other = grid(sweep(), 44100.0);
        assert_eq!(&other[..], &report.frequency_hz[..other.len()]);
    }
    #[test]
    fn invalid_responses_are_refused() {
        let mut full = response(48000.0, &[(0.0, 1.0)]);
        full.segments[2].pop();
        assert!(magnitude(&full, sweep(), None).is_err());
        let mut full = response(48000.0, &[(0.0, 1.0)]);
        full.segments[0][3] = f32::NAN;
        assert!(magnitude(&full, sweep(), None).is_err());
        let mut full = response(48000.0, &[(0.0, 1.0)]);
        full.anchor = 100;
        assert!(magnitude(&full, sweep(), None).is_err());
    }
}
