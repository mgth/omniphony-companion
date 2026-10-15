//! Offline correlation diagnostics, not a direct-sound or correction estimator.
//! Repeated reference sweeps remove capture offset and estimate linear clock drift.
use omniphony_osc_contract::measurement::{SETTLE_MS, Sweep};
use rustfft::{FftPlanner, num_complex::Complex};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct TimingReport {
    pub method: &'static str,
    /// Candidate correlation peaks, not verified direct arrivals. Their timing
    /// relationship is withheld below when competing peaks are present.
    pub reference_start_frames: [f64; 2],
    pub target_start_frame: f64,
    /// Withheld when competing peaks make their correspondence uncertain.
    pub clock_drift_ppm: Option<f64>,
    /// Includes existing speaker delay settings. Relative to the reference.
    pub relative_peak_delay_ms: Option<f64>,
    /// A complete, usable acquisition can still have unresolved arrival timing.
    pub ambiguous_peaks: bool,
    /// Strongest competing peak / selected peak, more than 2 ms away and
    /// within 100 ms, for reference, target, reference respectively.
    pub competing_peak_ratio: [f64; 3],
    pub normalized_correlation: [f64; 3],
    pub signal_to_noise_db: [f64; 3],
    pub sweep_rms_dbfs: [f64; 3],
    pub noise_rms_dbfs: f64,
    pub valid_for_correction: bool,
}
#[derive(Clone, Copy)]
struct Peak {
    position: f64,
    score: f64,
}

struct Located {
    peaks: [Peak; 3],
    competing_peak_ratio: [f64; 3],
}

impl TimingReport {
    pub fn timing_warning(&self) -> Option<&'static str> {
        self.ambiguous_peaks.then_some(
            "The sweeps were captured, but competing arrival peaks make timing uncertain. \
             Delay and clock drift are withheld. More volume will not resolve this; \
             impulse-response and direct-arrival analysis are needed.",
        )
    }
}

/// Capture rate may differ from renderer rate. This estimates constant drift;
/// Discontinuities and non-linear drift still require validation. Competing
/// arrival peaks retain level diagnostics but withhold timing estimates until
/// impulse-response/direct-arrival analysis is available.
pub fn analyze(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
) -> Result<TimingReport, String> {
    sweep.validate(render_rate)?;
    if !(8_000..=192_000).contains(&capture_rate)
        || samples.len() > capture_rate as usize * super::MAX_SECONDS as usize
        || samples.iter().any(|x| !x.is_finite() || x.abs() >= 0.999)
    {
        return Err("invalid, clipped, nonfinite or overlong capture".into());
    }
    let source = sweep.samples(render_rate)?;
    let nominal = capture_rate as f64 / render_rate as f64;
    let spacing = source.len() as f64 * nominal;
    if samples.len() < (3.0 * spacing) as usize {
        return Err("recording is too short for three complete segments".into());
    }
    // A first pass finds the clock ratio. A second pass compensates the
    // template's time scale before estimating the relative peak difference.
    let first = locate(
        samples,
        &resample(&source, nominal),
        spacing,
        capture_rate as f64,
    )?;
    let ratio = (first.peaks[2].position - first.peaks[0].position) / (2.0 * source.len() as f64);
    let located = locate(
        samples,
        &resample(&source, ratio),
        source.len() as f64 * ratio,
        render_rate as f64 * ratio,
    )?;
    let peaks = located.peaks;
    let ambiguous_peaks = located
        .competing_peak_ratio
        .iter()
        .any(|&ratio| ratio > 0.8);
    let ratio = (peaks[2].position - peaks[0].position) / (2.0 * source.len() as f64);
    let ppm = (ratio / nominal - 1.0) * 1e6;
    if ppm.abs() > 2_000.0 {
        return Err("clock drift exceeds the 2000 ppm prototype limit".into());
    }
    let effective_rate = render_rate as f64 * ratio;
    let relative_ms = (peaks[1].position - (peaks[0].position + peaks[2].position) * 0.5)
        / effective_rate
        * 1000.0;
    if relative_ms.abs() > 100.0 {
        return Err("relative peak delay exceeds 100 ms".into());
    }
    let quiet_len = (capture_rate / 10) as usize;
    if peaks[0].position < quiet_len as f64 {
        return Err("capture lacks a quiet lead-in for the noise check".into());
    }
    let noise = rms(&samples[..quiet_len]).max(1e-10);
    let padding = effective_rate * SETTLE_MS as f64 / 1000.0;
    let body = effective_rate * sweep.duration_ms as f64 / 1000.0;
    let mut snr = [0.0; 3];
    let mut sweep_rms_dbfs = [0.0; 3];
    for (i, p) in peaks.iter().enumerate() {
        let start = (p.position + padding).round() as usize;
        let end = start + body.round() as usize;
        if end > samples.len() {
            return Err("truncated sweep".into());
        }
        let signal_rms = rms(&samples[start..end]).max(1e-10);
        sweep_rms_dbfs[i] = 20.0 * signal_rms.log10();
        snr[i] = 20.0 * (signal_rms / noise).log10();
        if snr[i] < 20.0 {
            return Err(format!(
                "sweep {} signal-to-noise ratio is {:.1} dB, below the 20 dB acquisition limit",
                i + 1,
                snr[i]
            ));
        }
    }
    if (sweep_rms_dbfs[0] - sweep_rms_dbfs[2]).abs() > 1.0 {
        return Err("reference levels changed by more than 1 dB during the take".into());
    }
    Ok(TimingReport {
        method: "repeated-reference-correlation-v2",
        reference_start_frames: [peaks[0].position, peaks[2].position],
        target_start_frame: peaks[1].position,
        clock_drift_ppm: (!ambiguous_peaks).then_some(ppm),
        relative_peak_delay_ms: (!ambiguous_peaks).then_some(relative_ms),
        ambiguous_peaks,
        competing_peak_ratio: located.competing_peak_ratio,
        normalized_correlation: peaks.map(|p| p.score),
        signal_to_noise_db: snr,
        sweep_rms_dbfs,
        noise_rms_dbfs: 20.0 * noise.log10(),
        // Peak correlation is not a validated direct-arrival estimate.
        valid_for_correction: false,
    })
}
fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}
pub(crate) fn resample(source: &[f32], ratio: f64) -> Vec<f32> {
    let len = (source.len() as f64 * ratio).round() as usize;
    (0..len)
        .map(|i| {
            let x = i as f64 / ratio;
            let left = x.floor() as usize;
            let fraction = (x - left as f64) as f32;
            let a = source.get(left).copied().unwrap_or(0.0);
            let b = source.get(left + 1).copied().unwrap_or(0.0);
            a + (b - a) * fraction
        })
        .collect()
}
fn locate(samples: &[f32], template: &[f32], spacing: f64, rate: f64) -> Result<Located, String> {
    let count = samples
        .len()
        .checked_add(template.len())
        .ok_or("recording is too large")?;
    let size = count.next_power_of_two();
    if size > 8_388_608 {
        return Err("recording exceeds the offline FFT memory bound".into());
    }
    let mut a = vec![Complex::new(0.0_f32, 0.0); size];
    let mut b = vec![Complex::new(0.0_f32, 0.0); size];
    for (dst, &src) in a.iter_mut().zip(samples) {
        dst.re = src;
    }
    for (dst, &src) in b.iter_mut().zip(template.iter().rev()) {
        dst.re = src;
    }
    let mut planner = FftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(size);
    forward.process(&mut a);
    forward.process(&mut b);
    for (x, y) in a.iter_mut().zip(b) {
        *x *= y;
    }
    planner.plan_fft_inverse(size).process(&mut a);
    let template_energy = template.iter().map(|&x| (x as f64).powi(2)).sum::<f64>();
    let mut energy = samples[..template.len()]
        .iter()
        .map(|&x| (x as f64).powi(2))
        .sum::<f64>();
    let mut scores = Vec::with_capacity(samples.len() - template.len() + 1);
    for i in 0..=samples.len() - template.len() {
        if i > 0 {
            energy +=
                (samples[i + template.len() - 1] as f64).powi(2) - (samples[i - 1] as f64).powi(2);
        }
        let corr = a[i + template.len() - 1].re.abs() as f64 / size as f64;
        let score = if energy > 1e-12 {
            corr / (energy.max(0.0) * template_energy).sqrt()
        } else {
            0.0
        };
        scores.push(score.min(1.0));
    }
    // Find a unique sequence independently of whether the room response has
    // competing arrivals inside each sweep. Arrival ambiguity is reported after
    // clock compensation; it does not mean the sweeps were not recorded.
    let guard = (spacing * 0.4) as usize;
    let mut candidates = Vec::new();
    let mut masked = scores.clone();
    for _ in 0..12 {
        let Some((i, &score)) = masked.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) else {
            break;
        };
        if score < 0.2 {
            break;
        }
        let fraction = if i > 0 && i + 1 < scores.len() {
            let (left, center, right) = (scores[i - 1], scores[i], scores[i + 1]);
            let denom = left - 2.0 * center + right;
            if denom.abs() > 1e-12 {
                (0.5 * (left - right) / denom).clamp(-0.5, 0.5)
            } else {
                0.0
            }
        } else {
            0.0
        };
        candidates.push(Peak {
            position: i as f64 + fraction,
            score,
        });
        let end = (i + guard + 1).min(masked.len());
        masked[i.saturating_sub(guard)..end].fill(0.0);
    }
    candidates.sort_by(|a, b| a.position.total_cmp(&b.position));
    let mut triples = Vec::new();
    for (i, &first) in candidates.iter().enumerate() {
        for (j, &middle) in candidates.iter().enumerate().skip(i + 1) {
            for &last in candidates.iter().skip(j + 1) {
                if ((last.position - first.position) / (2.0 * spacing) - 1.0).abs() > 0.004
                    || (middle.position - (first.position + last.position) * 0.5).abs()
                        > spacing * 0.2
                {
                    continue;
                }
                triples.push([first, middle, last]);
            }
        }
    }
    triples.sort_by(|a, b| quality(b).total_cmp(&quality(a)));
    let best = triples
        .first()
        .copied()
        .ok_or("three reliable timing references were not found")?;
    if triples
        .get(1)
        .is_some_and(|next| quality(next) > quality(&best) * 0.8)
    {
        return Err("ambiguous repeated reference sequence".into());
    }
    let exclusion = (rate * 0.002).ceil() as usize;
    let neighborhood = (rate * 0.1).ceil() as usize;
    let competing_peak_ratio = best.map(|peak| {
        let center = peak.position.round() as usize;
        let lo = center.saturating_sub(neighborhood);
        let hi = (center + neighborhood + 1).min(scores.len());
        scores[lo..hi]
            .iter()
            .enumerate()
            .filter(|(i, _)| (i + lo).abs_diff(center) > exclusion)
            .map(|(_, &score)| score / peak.score)
            .fold(0.0, f64::max)
    });
    Ok(Located {
        peaks: best,
        competing_peak_ratio,
    })
}
fn quality(peaks: &[Peak; 3]) -> f64 {
    peaks.iter().map(|p| p.score).fold(1.0, f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> Sweep {
        Sweep {
            duration_ms: 1000,
            peak_dbfs: -20.0,
            start_hz: 200.0,
            end_hz: 6000.0,
        }
    }
    fn fixture(rate: u32, ppm: f64, delay_ms: f64) -> Vec<f32> {
        let source = spec().samples(48000).unwrap();
        let ratio = rate as f64 / 48000.0 * (1.0 + ppm / 1e6);
        let template = resample(&source, ratio);
        let lead = rate as usize / 2;
        let spacing = source.len() as f64 * ratio;
        let mut samples = vec![0.0; lead + template.len() * 3 + rate as usize / 2];
        for segment in 0..3 {
            let start = (lead as f64
                + spacing * segment as f64
                + if segment == 1 {
                    delay_ms / 1000.0 * 48000.0 * ratio
                } else {
                    0.0
                })
            .round() as usize;
            let gain = if segment == 1 { -0.65 } else { 0.8 };
            for (dst, &x) in samples[start..].iter_mut().zip(&template) {
                *dst += x * gain;
            }
        }
        // Deterministic low background noise.
        for (i, x) in samples.iter_mut().enumerate() {
            *x += ((i * 13 % 101) as f32 - 50.0) * 0.0000001;
        }
        samples
    }
    #[test]
    fn independent_rate_and_clock_drift_preserve_relative_arrival() {
        for (rate, ppm, delay) in [
            (48000, 0.0, 7.5),
            (44100, 300.0, -4.0),
            (48000, -250.0, 12.0),
        ] {
            let report = analyze(&fixture(rate, ppm, delay), rate, 48000, spec()).unwrap();
            assert!(
                (report.relative_peak_delay_ms.unwrap() - delay).abs() < 0.06,
                "{report:?}"
            );
            assert!(
                (report.clock_drift_ppm.unwrap() - ppm).abs() < 35.0,
                "{report:?}"
            );
            assert!(!report.ambiguous_peaks);
            assert!(!report.valid_for_correction);
        }
    }
    #[test]
    fn strong_reflections_preserve_levels_but_withhold_timing() {
        let dry = fixture(48000, 0.0, 5.0);
        let mut echo = dry.clone();
        for i in 960..echo.len() {
            echo[i] += dry[i - 960] * 0.95;
        }
        let report = analyze(&echo, 48000, 48000, spec()).unwrap();
        assert!(report.ambiguous_peaks);
        assert!(report.competing_peak_ratio.iter().any(|&ratio| ratio > 0.8));
        assert!(report.relative_peak_delay_ms.is_none());
        assert!(report.clock_drift_ppm.is_none());
        assert!(report.timing_warning().is_some());
        assert!(report.signal_to_noise_db.iter().all(|&snr| snr > 20.0));
        assert!(!report.valid_for_correction);
    }
    #[test]
    fn noise_is_still_rejected_when_arrivals_are_ambiguous() {
        let dry = fixture(48000, 0.0, 5.0);
        let mut noisy = dry.clone();
        for i in 960..noisy.len() {
            noisy[i] += dry[i - 960] * 0.95;
        }
        let mut state = 42_u32;
        for x in &mut noisy {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            *x += ((state as f64 / u32::MAX as f64 - 0.5) * 0.05) as f32;
        }
        assert!(analyze(&noisy, 48000, 48000, spec()).is_err());
    }
    #[test]
    fn ambiguous_arrivals_do_not_bypass_reference_level_checks() {
        let dry = fixture(48000, 0.0, 5.0);
        let mut echo = dry.clone();
        for i in 960..echo.len() {
            echo[i] += dry[i - 960] * 0.95;
        }
        for x in &mut echo[168000..] {
            *x *= 1.4;
        }
        assert!(
            analyze(&echo, 48000, 48000, spec())
                .unwrap_err()
                .contains("reference levels changed")
        );
    }
    #[test]
    fn multiple_plausible_sequences_are_still_rejected() {
        let capture = fixture(48000, 0.0, 0.0);
        let mut repeated = capture.clone();
        repeated.extend(capture);
        assert!(
            analyze(&repeated, 48000, 48000, spec())
                .unwrap_err()
                .contains("ambiguous repeated reference sequence")
        );
    }
    #[test]
    fn silence_clipping_missing_reference_and_excess_drift_are_rejected() {
        assert!(analyze(&vec![0.0; 192000], 48000, 48000, spec()).is_err());
        let mut samples = fixture(48000, 0.0, 5.0);
        samples[100] = 1.0;
        assert!(analyze(&samples, 48000, 48000, spec()).is_err());
        let mut samples = fixture(48000, 0.0, 5.0);
        samples[100000..].fill(0.0);
        assert!(analyze(&samples, 48000, 48000, spec()).is_err());
        assert!(analyze(&fixture(48000, 4000.0, 5.0), 48000, 48000, spec()).is_err());
    }
}
