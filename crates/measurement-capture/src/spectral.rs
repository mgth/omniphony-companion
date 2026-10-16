//! Offline frequency response, local group delay and band-energy peak timing.
//! All timing shares the segment's correlation anchor, never a per-band zero.
use omniphony_osc_contract::measurement::Sweep;
use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectralReport {
    pub method: String,
    pub frequency_bounds_hz: [f64; 2],
    pub response_window_ms: [f64; 2],
    pub smoothing_octaves: f64,
    pub peak_band_octaves: f64,
    pub points: Vec<SpectralPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectralPoint {
    pub frequency_hz: f64,
    /// Capture/source amplitude ratio, including speaker, room, microphone and gains.
    pub magnitude_db: Option<f64>,
    /// Energy-weighted phase derivative; milliseconds relative to the correlation anchor.
    pub group_delay_ms: Option<f64>,
    /// Strongest analytic-envelope peak, NOT a direct-arrival estimate.
    pub peak_time_ms: Option<f64>,
    pub ambiguous_peak: bool,
    /// Approximate filter time width, 2 / bandwidth. Not an accuracy/confidence bound.
    pub time_resolution_ms: f64,
    pub estimated_snr_db: f64,
}

/// `impulse` is the deconvolved, full-band response before the arrival detector's
/// raised-cosine bands. Its zero is `anchor`; no acoustic propagation estimate is used.
pub fn analyze(
    impulse: &[f64],
    rate: f64,
    anchor: usize,
    sweep: Sweep,
) -> Result<SpectralReport, String> {
    if !rate.is_finite()
        || !(7900.0..=193000.0).contains(&rate)
        || impulse.len() > (rate * 0.3) as usize
        || anchor < (rate * 0.08) as usize
        || anchor
            .checked_add((rate * 0.1) as usize)
            .is_none_or(|n| n >= impulse.len())
        || impulse.iter().any(|v| !v.is_finite())
    {
        return Err("invalid spectral impulse window".into());
    }
    let bounds = [
        f64::from(sweep.start_hz) * 1.1,
        (f64::from(sweep.end_hz) * 0.9).min(rate * 0.44),
    ];
    if bounds[0] < 20.0 || bounds[1] <= bounds[0] || !bounds.iter().all(|x| x.is_finite()) {
        return Err("insufficient excited frequency range".into());
    }
    let n = (impulse.len() * 2).next_power_of_two();
    if n > 131072 {
        return Err("spectral FFT exceeds the offline bound".into());
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(n);
    let inverse = planner.plan_fft_inverse(n);
    let mut h = vec![Complex::default(); n];
    let mut moment = h.clone();
    let taper = (rate * 0.005).round() as usize;
    let mut window_energy = 0.0;
    for (i, &sample) in impulse.iter().enumerate() {
        let edge = (i.min(impulse.len() - 1 - i) as f64 / taper as f64).min(1.0);
        let weight = 0.5 * (1.0 - (std::f64::consts::PI * edge).cos());
        h[i].re = sample * weight;
        moment[i].re = (i as f64 - anchor as f64) * h[i].re;
        window_energy += weight * weight;
    }
    let quiet_end = anchor - (rate * 0.06) as usize;
    // Deconvolution noise is colored. Estimate its spectrum in the quiet prefix
    // instead of applying one broadband floor to every frequency.
    let mut noise = vec![Complex::default(); n];
    let mut quiet_energy = 0.0;
    for i in 0..quiet_end {
        let weight =
            0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / (quiet_end - 1) as f64).cos());
        noise[i].re = impulse[i] * weight;
        quiet_energy += weight * weight;
    }
    forward.process(&mut noise);
    let noise_scale = window_energy / quiet_energy;
    forward.process(&mut h);
    forward.process(&mut moment);
    let maximum = h
        .iter()
        .take(n / 2)
        .map(|v| v.norm_sqr())
        .fold(0.0, f64::max);
    if maximum <= 1e-30 || !maximum.is_finite() {
        return Err("no usable energy in the deconvolved response".into());
    }
    let bin_hz = rate / n as f64;
    let mut band_spectrum = vec![Complex::default(); n];
    let mut inverse_scratch = vec![Complex::default(); inverse.get_inplace_scratch_len()];
    let mut envelope = vec![0.0; impulse.len()];
    let search_lo = anchor - (rate * 0.05) as usize;
    let search_hi = (anchor + (rate * 0.1) as usize).min(impulse.len() - 1);
    let mut points = Vec::new();
    for step in 0..128 {
        let hz = bounds[0] * 2_f64.powf(step as f64 / 12.0);
        if hz > bounds[1] {
            break;
        }
        let half_smooth = 2_f64.powf(1.0 / 24.0);
        let lo = ((hz / half_smooth / bin_hz).ceil() as usize).max(1);
        let hi = ((hz * half_smooth / bin_hz).floor() as usize).min(n / 2 - 1);
        let mut power = 0.0;
        let mut weighted_time = 0.0;
        let mut minimum_power = f64::INFINITY;
        for k in lo..=hi {
            power += h[k].norm_sqr();
            minimum_power = minimum_power.min(h[k].norm_sqr());
            weighted_time += (moment[k] * h[k].conj()).re;
        }
        let mean_power = power / (hi.saturating_sub(lo) + 1) as f64;
        // A wider noise neighborhood reduces periodogram variance without
        // assuming a white floor outside the excited band.
        let noise_lo = ((hz / 2_f64.powf(1.0 / 6.0) / bin_hz).ceil() as usize).max(1);
        let noise_hi = ((hz * 2_f64.powf(1.0 / 6.0) / bin_hz).floor() as usize).min(n / 2 - 1);
        let noise_power = if noise_lo <= noise_hi {
            noise[noise_lo..=noise_hi]
                .iter()
                .map(|v| v.norm_sqr())
                .sum::<f64>()
                / (noise_hi - noise_lo + 1) as f64
                * noise_scale
        } else {
            f64::MAX
        };
        let snr =
            (10.0 * (mean_power.max(1e-30) / noise_power.max(1e-30)).log10()).clamp(-300.0, 300.0);
        let supported = lo <= hi && hz / half_smooth >= bounds[0] && hz * half_smooth <= bounds[1];
        let strong = supported && snr >= 20.0 && mean_power >= maximum * 1e-6;
        // dH/dw = -j FFT[n h]. This is -d phase / dw, without phase-unwrapping
        // across nulls. Weighting by |H|² avoids trusting a phase at a deep notch.
        let gd = weighted_time / power.max(1e-30) / rate * 1000.0;
        let half_band = 2_f64.powf(1.0 / 6.0);
        let band = [hz / half_band, hz * half_band];
        let resolution = 2000.0 / (band[1] - band[0]);
        let mut peak_time = None;
        let mut ambiguous = false;
        if strong && band[0] >= bounds[0] && band[1] <= bounds[1] {
            band_spectrum.fill(Complex::default());
            for k in (band[0] / bin_hz).ceil() as usize..=(band[1] / bin_hz).floor() as usize {
                let phase = (k as f64 * bin_hz - band[0]) / (band[1] - band[0]);
                let weight = 0.5 * (1.0 - (std::f64::consts::TAU * phase).cos());
                band_spectrum[k] = h[k] * (2.0 * weight);
            }
            inverse.process_with_scratch(&mut band_spectrum, &mut inverse_scratch);
            for (out, bin) in envelope.iter_mut().zip(&band_spectrum) {
                *out = bin.norm() / n as f64;
            }
            let quiet = (envelope[..quiet_end].iter().map(|v| v * v).sum::<f64>()
                / quiet_end as f64)
                .sqrt();
            let peak = (search_lo..=search_hi)
                .max_by(|&a, &b| envelope[a].total_cmp(&envelope[b]))
                .unwrap();
            let guard = (resolution * rate / 1000.0).ceil() as usize;
            if peak > search_lo && peak < search_hi && envelope[peak] > quiet.max(1e-15) * 10.0 {
                let denom = envelope[peak - 1] - 2.0 * envelope[peak] + envelope[peak + 1];
                let fraction = if denom.abs() > 1e-30 {
                    (0.5 * (envelope[peak - 1] - envelope[peak + 1]) / denom).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                peak_time = Some((peak as f64 + fraction - anchor as f64) / rate * 1000.0);
                ambiguous = (search_lo + 1..search_hi).any(|i| {
                    i.abs_diff(peak) > guard
                        && envelope[i] > envelope[peak] * 0.8
                        && envelope[i] > envelope[i - 1]
                        && envelope[i] >= envelope[i + 1]
                });
            }
        }
        points.push(SpectralPoint {
            frequency_hz: hz,
            magnitude_db: (supported && snr >= 10.0 && mean_power > 1e-30)
                .then(|| 10.0 * mean_power.log10()),
            group_delay_ms: (strong
                && minimum_power > mean_power * 0.01
                && gd.is_finite()
                && gd.abs() <= 100.0)
                .then_some(gd),
            peak_time_ms: peak_time,
            ambiguous_peak: ambiguous,
            time_resolution_ms: resolution,
            estimated_snr_db: snr,
        });
    }
    Ok(SpectralReport {
        method: "windowed-transfer-and-band-energy-v1".into(),
        frequency_bounds_hz: bounds,
        response_window_ms: [
            -(anchor as f64) / rate * 1000.0,
            (impulse.len() - 1 - anchor) as f64 / rate * 1000.0,
        ],
        smoothing_octaves: 1.0 / 12.0,
        peak_band_octaves: 1.0 / 3.0,
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sweep() -> Sweep {
        Sweep {
            duration_ms: 1000,
            peak_dbfs: -24.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        }
    }
    fn near(report: &SpectralReport, hz: f64) -> &SpectralPoint {
        report
            .points
            .iter()
            .min_by(|a, b| {
                (a.frequency_hz - hz)
                    .abs()
                    .total_cmp(&(b.frequency_hz - hz).abs())
            })
            .unwrap()
    }
    #[test]
    fn flat_gain_and_signed_delays_keep_one_time_origin_across_rates() {
        for rate in [16000.0_f64, 44100.0, 48000.0, 96000.0] {
            for delay in [-0.004, 0.003] {
                let anchor = (rate * 0.1) as usize;
                let mut h = vec![0.0; (rate * 0.25) as usize + 1];
                let offset = (rate * delay).round() as isize;
                h[anchor.checked_add_signed(offset).unwrap()] = -0.5;
                let r = analyze(&h, rate, anchor, sweep()).unwrap();
                assert!(r.points.len() <= 128);
                for p in r.points.iter().filter(|p| p.peak_time_ms.is_some()) {
                    assert!((p.magnitude_db.unwrap() + 6.0206).abs() < 0.01);
                    assert!(
                        (p.group_delay_ms.unwrap() - offset as f64 / rate * 1000.0).abs() < 0.001
                    );
                    assert!((p.peak_time_ms.unwrap() - offset as f64 / rate * 1000.0).abs() < 0.05);
                    assert!(!p.ambiguous_peak);
                }
                assert!(near(&r, 3000.0).peak_time_ms.is_some());
            }
        }
    }
    #[test]
    fn first_order_lowpass_matches_analytic_magnitude_and_phase_derivative() {
        let rate = 48000.0;
        let anchor = 4800;
        let delay = 72;
        let a = (-std::f64::consts::TAU * 1200.0 / rate).exp();
        let mut h = vec![0.0; 12001];
        for (i, v) in h[anchor + delay..].iter_mut().enumerate() {
            *v = (1.0 - a) * a.powi(i as i32);
        }
        let r = analyze(&h, rate, anchor, sweep()).unwrap();
        for p in r
            .points
            .iter()
            .filter(|p| (400.0..7000.0).contains(&p.frequency_hz))
        {
            let w = std::f64::consts::TAU * p.frequency_hz / rate;
            let denominator = 1.0 - 2.0 * a * w.cos() + a * a;
            let db = 20.0 * ((1.0 - a) / denominator.sqrt()).log10();
            let gd = (delay as f64 + (a * w.cos() - a * a) / denominator) / rate * 1000.0;
            assert!((p.magnitude_db.unwrap() - db).abs() < 0.04);
            assert!((p.group_delay_ms.unwrap() - gd).abs() < 0.01);
        }
    }
    #[test]
    fn delayed_high_driver_is_visible_and_near_equal_echoes_are_flagged() {
        let rate = 48000.0;
        let anchor = 4800;
        let a = (-std::f64::consts::TAU * 1200.0 / rate).exp();
        let mut h = vec![0.0; 12001];
        for i in 0..5000 {
            let low = (1.0 - a) * a.powi(i as i32);
            h[anchor + i] += low;
            h[anchor + 192 + i] -= low;
        }
        h[anchor + 192] += 1.0;
        let r = analyze(&h, rate, anchor, sweep()).unwrap();
        let low = near(&r, 350.0).peak_time_ms.unwrap();
        let high = near(&r, 7000.0).peak_time_ms.unwrap();
        assert!(high - low > 2.0, "low={low}, high={high}");
        assert!((high - 4.0).abs() < 0.3);
        h.fill(0.0);
        h[anchor] = 1.0;
        h[anchor + 960] = 0.95;
        let echoes = analyze(&h, rate, anchor, sweep()).unwrap();
        assert!(
            echoes
                .points
                .iter()
                .filter(|p| p.frequency_hz > 3000.0)
                .any(|p| p.ambiguous_peak)
        );
        assert!(
            echoes
                .points
                .iter()
                .any(|p| p.magnitude_db.is_some() && p.group_delay_ms.is_none())
        );
    }
    #[test]
    fn colored_noise_outside_a_band_does_not_hide_a_clean_delayed_response() {
        let rate = 48000.0;
        let anchor = 4800;
        let mut h: Vec<_> = (0..12001)
            .map(|i| 0.003 * (std::f64::consts::TAU * 100.0 * i as f64 / rate).sin())
            .collect();
        h[anchor + 120] += 0.05;
        let r = analyze(&h, rate, anchor, sweep()).unwrap();
        let p = near(&r, 4000.0);
        assert!(p.estimated_snr_db > 20.0);
        assert!((p.group_delay_ms.unwrap() - 2.5).abs() < 0.03);
        assert!((p.peak_time_ms.unwrap() - 2.5).abs() < 0.05);
    }
    #[test]
    fn noise_and_invalid_windows_do_not_create_delay_curves() {
        assert!(analyze(&[], 48000.0, 0, sweep()).is_err());
        let mut h = vec![0.0; 12001];
        assert!(analyze(&h, 48000.0, 2400, sweep()).is_err());
        assert!(analyze(&h, 48000.0, 4800, sweep()).is_err());
        h[4800] = f64::NAN;
        assert!(analyze(&h, 48000.0, 4800, sweep()).is_err());
        let mut seed = 123_u32;
        for v in &mut h {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *v = (seed as f64 / u32::MAX as f64 - 0.5) * 1e-4;
        }
        let r = analyze(&h, 48000.0, 4800, sweep()).unwrap();
        assert!(
            r.points
                .iter()
                .all(|p| p.peak_time_ms.is_none() && p.group_delay_ms.is_none())
        );
    }
}
