//! Offline acquisition evidence. These approximate windows cannot establish
//! arrival time, speaker identity, a hardware fault or a calibration gain.
use crate::timing::{Peak, correlation_scores, peak_candidates, resample};
use omniphony_osc_contract::measurement::{SETTLE_MS, Sweep};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    Detected,
    Clipped,
    NoSweepDetected,
    LowSignalToNoise,
    UncorrelatedSignal,
}
impl SegmentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Detected => "Sweep detected",
            Self::Clipped => "Clipped",
            Self::NoSweepDetected => "No sweep detected",
            Self::LowSignalToNoise => "Low signal / noise",
            Self::UncorrelatedSignal => "Signal without a reliable sweep match",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SegmentDiagnostics {
    pub role: &'static str,
    /// Sweep-body window predicted from the two reference correlation peaks.
    /// The target does not need to be detected to locate this approximate window.
    pub body_start_frame: usize,
    pub body_end_frame: usize,
    pub rms_dbfs: f64,
    /// (Signal + noise) / noise; not a noise-subtracted acoustic measurement.
    pub signal_to_noise_db: f64,
    /// Includes 100 ms on either side of the body for delayed arrivals.
    pub peak_dbfs: f64,
    pub clipped_samples: usize,
    pub normalized_correlation: f64,
    pub status: SegmentStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcquisitionDiagnostics {
    pub method: &'static str,
    pub noise_rms_dbfs: f64,
    pub total_clipped_samples: usize,
    pub reference_start_frames: Option<[f64; 2]>,
    pub segments: Option<[SegmentDiagnostics; 3]>,
    pub messages: Vec<String>,
    pub valid_for_correction: bool,
}

/// Clipped captures are deliberately inspectable, but never made usable here.
/// Callers must independently retain acquisition faults and acceptance verdicts.
pub fn analyze(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
) -> Result<AcquisitionDiagnostics, String> {
    sweep.validate(render_rate)?;
    if !(8_000..=192_000).contains(&capture_rate)
        || samples.len() > capture_rate as usize * crate::MAX_SECONDS as usize
        || samples.iter().any(|x| !x.is_finite() || x.abs() > 16.0)
        || samples.len() < capture_rate as usize / 10
    {
        return Err(
            "invalid, out-of-range, nonfinite, short or overlong diagnostic capture".into(),
        );
    }
    let noise = rms(&samples[..capture_rate as usize / 10]);
    let mut report = AcquisitionDiagnostics {
        method: "reference-pair-acquisition-v1",
        noise_rms_dbfs: db(noise),
        total_clipped_samples: samples.iter().filter(|x| x.abs() >= 0.999).count(),
        reference_start_frames: None,
        segments: None,
        messages: Vec::new(),
        valid_for_correction: false,
    };
    if report.total_clipped_samples > 0 {
        report.messages.push("Recording clipped. Reduce playback or microphone input gain before repeating; this take cannot be used for correction.".into());
    }
    if samples.iter().all(|s| *s == 0.0) {
        report.messages.push(crate::input::NO_SIGNAL.into());
        return Ok(report);
    }
    match locate_segments(samples, capture_rate, render_rate, sweep, noise) {
        Ok((references, segments)) => {
            let target = &segments[1];
            match target.status {
                SegmentStatus::NoSweepDetected => report.messages.push("The reference sweeps were found, but no target sweep was detected above background. Check the selected speaker's output route, mute, amplifier and connection. Increasing the overall volume is not the first step.".into()),
                SegmentStatus::LowSignalToNoise => report.messages.push("The target interval has insufficient signal / noise. Check routing and input selection before adjusting levels.".into()),
                SegmentStatus::UncorrelatedSignal => report.messages.push("Sound is present during the target interval, but it does not reliably match the sweep. Check routing, interference and capture continuity.".into()),
                SegmentStatus::Clipped => report.messages.push("Clipping occurs in the target speaker interval. Repeat this speaker at a lower level.".into()),
                SegmentStatus::Detected => (),
            }
            if (segments[0].rms_dbfs - segments[2].rms_dbfs).abs() > 1.0 {
                report.messages.push("Reference levels changed by more than 1 dB; keep the microphone, gains and routing fixed.".into());
            }
            if segments[0].status == SegmentStatus::Clipped
                || segments[2].status == SegmentStatus::Clipped
            {
                report.messages.push(
                    "A reference interval clipped; lower the reference or input level and repeat."
                        .into(),
                );
            }
            report.reference_start_frames = Some(references);
            report.segments = Some(segments);
        }
        Err(reason) => report.messages.push(format!(
            "Per-speaker windows unavailable: {reason}. No target-silence conclusion can be drawn."
        )),
    }
    Ok(report)
}

fn locate_segments(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
    noise: f64,
) -> Result<([f64; 2], [SegmentDiagnostics; 3]), String> {
    let source = sweep.samples(render_rate)?;
    let nominal = capture_rate as f64 / render_rate as f64;
    let spacing = source.len() as f64 * nominal;
    if samples.len() < (3.0 * spacing) as usize {
        return Err("recording is too short for a complete sequence".into());
    }
    let first = reference_pair(
        &correlation_scores(samples, &resample(&source, nominal))?,
        spacing,
    )?;
    let ratio = (first[1].position - first[0].position) / (2.0 * source.len() as f64);
    let scores = correlation_scores(samples, &resample(&source, ratio))?;
    let pair = reference_pair(&scores, source.len() as f64 * ratio)?;
    let ratio = (pair[1].position - pair[0].position) / (2.0 * source.len() as f64);
    if (ratio / nominal - 1.0).abs() > 0.002 {
        return Err("reference spacing exceeds the clock-drift limit".into());
    }
    if pair[0].position < capture_rate as f64 * 0.1 {
        return Err("no quiet lead-in before the first reference".into());
    }
    let rate = render_rate as f64 * ratio;
    let starts = [
        pair[0].position,
        (pair[0].position + pair[1].position) * 0.5,
        pair[1].position,
    ];
    let mut segments = Vec::with_capacity(3);
    for (index, start) in starts.into_iter().enumerate() {
        let body_start_frame = (start + rate * SETTLE_MS as f64 / 1000.0).round() as usize;
        let body_end_frame =
            body_start_frame + (rate * sweep.duration_ms as f64 / 1000.0).round() as usize;
        let margin = (rate * 0.1).ceil() as usize;
        let lo = body_start_frame.saturating_sub(margin);
        let hi = body_end_frame + margin;
        if hi > samples.len() {
            return Err("truncated sweep window".into());
        }
        let rms = rms(&samples[body_start_frame..body_end_frame]);
        let window = &samples[lo..hi];
        let clipped_samples = window.iter().filter(|x| x.abs() >= 0.999).count();
        let center = start.round() as usize;
        let score_lo = center.saturating_sub(margin);
        let score_hi = (center + margin + 1).min(scores.len());
        if score_lo >= score_hi {
            return Err("incomplete correlation window".into());
        }
        let correlation = scores[score_lo..score_hi]
            .iter()
            .copied()
            .fold(0.0, f64::max);
        let snr = db(rms) - db(noise);
        let status = if clipped_samples > 0 {
            SegmentStatus::Clipped
        } else if snr < 6.0 && correlation < 0.2 {
            SegmentStatus::NoSweepDetected
        } else if snr < 20.0 {
            SegmentStatus::LowSignalToNoise
        } else if correlation < 0.2 {
            SegmentStatus::UncorrelatedSignal
        } else {
            SegmentStatus::Detected
        };
        segments.push(SegmentDiagnostics {
            role: ["Reference before", "Speaker", "Reference after"][index],
            body_start_frame,
            body_end_frame,
            rms_dbfs: db(rms),
            signal_to_noise_db: snr,
            peak_dbfs: db(window.iter().map(|x| x.abs() as f64).fold(0.0, f64::max)),
            clipped_samples,
            normalized_correlation: correlation,
            status,
        });
    }
    if segments[0].signal_to_noise_db < 20.0 || segments[2].signal_to_noise_db < 20.0 {
        return Err("reference sweeps have insufficient signal / noise".into());
    }
    let segments = segments.try_into().map_err(|_| "invalid segment count")?;
    Ok(([pair[0].position, pair[1].position], segments))
}

fn reference_pair(scores: &[f64], spacing: f64) -> Result<[Peak; 2], String> {
    let candidates = peak_candidates(scores, spacing);
    let mut pairs = Vec::new();
    for (i, &first) in candidates.iter().enumerate() {
        for &last in candidates.iter().skip(i + 1) {
            if ((last.position - first.position) / (2.0 * spacing) - 1.0).abs() <= 0.004 {
                pairs.push([first, last]);
            }
        }
    }
    let quality = |pair: &[Peak; 2]| pair[0].score.min(pair[1].score);
    pairs.sort_by(|a, b| quality(b).total_cmp(&quality(a)));
    let best = pairs
        .first()
        .copied()
        .ok_or("two reliable reference sweeps were not found")?;
    if pairs
        .get(1)
        .is_some_and(|p| quality(p) > quality(&best) * 0.8)
    {
        return Err("multiple plausible reference pairs".into());
    }
    Ok(best)
}
fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}
fn db(value: f64) -> f64 {
    20.0 * value.max(1e-10).log10()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sweep() -> Sweep {
        Sweep {
            duration_ms: 1000,
            peak_dbfs: -24.0,
            start_hz: 200.0,
            end_hz: 6000.0,
        }
    }
    fn fixture(gains: [f32; 3], rate: u32, ppm: f64) -> Vec<f32> {
        let source = sweep().samples(48000).unwrap();
        let ratio = rate as f64 / 48000.0 * (1.0 + ppm / 1e6);
        let signal = resample(&source, ratio);
        let mut samples = vec![0.0; rate as usize * 6];
        let mut rng = 42_u32;
        for sample in &mut samples {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            *sample = (rng as f64 / u32::MAX as f64 - 0.5) as f32 * 0.0002;
        }
        for (i, gain) in gains.into_iter().enumerate() {
            let start =
                (rate as f64 * 0.5 + source.len() as f64 * ratio * i as f64).round() as usize;
            for (dst, src) in samples[start..].iter_mut().zip(&signal) {
                *dst = (*dst + src * gain).clamp(-1.0, 1.0);
            }
        }
        samples
    }
    #[test]
    fn missing_target_is_distinct_from_missing_references_across_rates_and_drift() {
        for (rate, ppm) in [(48000, 0.0), (44100, 300.0)] {
            let missing = fixture([1.0, 0.0, 1.0], rate, ppm);
            assert!(crate::timing::analyze(&missing, rate, 48000, sweep()).is_err());
            let report = analyze(&missing, rate, 48000, sweep()).unwrap();
            let segments = report.segments.unwrap();
            assert_eq!(segments[0].status, SegmentStatus::Detected);
            assert_eq!(segments[1].status, SegmentStatus::NoSweepDetected);
            assert_eq!(segments[2].status, SegmentStatus::Detected);
            assert!(segments[1].signal_to_noise_db.abs() < 0.5);
            assert!(!report.valid_for_correction);
        }
        for gains in [[0.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0; 3]] {
            let report = analyze(&fixture(gains, 48000, 0.0), 48000, 48000, sweep()).unwrap();
            assert!(report.segments.is_none());
            assert!(
                report
                    .messages
                    .iter()
                    .any(|s| s.contains("No target-silence conclusion"))
            );
        }
    }
    #[test]
    fn clipped_target_keeps_reference_evidence_and_does_not_supply_timing() {
        let samples = fixture([1.0, 30.0, 1.0], 48000, 0.0);
        assert!(crate::timing::analyze(&samples, 48000, 48000, sweep()).is_err());
        let report = analyze(&samples, 48000, 48000, sweep()).unwrap();
        let segments = report.segments.unwrap();
        assert_eq!(segments[1].status, SegmentStatus::Clipped);
        assert_eq!(segments[1].clipped_samples, report.total_clipped_samples);
        assert_eq!(segments[0].clipped_samples + segments[2].clipped_samples, 0);
    }
    #[test]
    fn quiet_sweep_and_unrelated_loud_sound_are_not_called_missing() {
        let report = analyze(
            &fixture([1.0, 0.005, 1.0], 48000, 0.0),
            48000,
            48000,
            sweep(),
        )
        .unwrap();
        assert_eq!(
            report.segments.unwrap()[1].status,
            SegmentStatus::LowSignalToNoise
        );
        let mut samples = fixture([1.0, 0.0, 1.0], 48000, 0.0);
        for (i, sample) in samples[108000..156000].iter_mut().enumerate() {
            *sample += 0.1 * (i as f32 * std::f32::consts::TAU * 1000.0 / 48000.0).sin();
        }
        let report = analyze(&samples, 48000, 48000, sweep()).unwrap();
        assert_eq!(
            report.segments.unwrap()[1].status,
            SegmentStatus::UncorrelatedSignal
        );
    }
    #[test]
    fn extra_sequence_and_incomplete_recordings_do_not_get_invented_windows() {
        let source = sweep().samples(48000).unwrap();
        let mut samples = vec![0.0; 48000 * 8];
        for i in 0..4 {
            samples[24000 + i * source.len()..24000 + (i + 1) * source.len()]
                .copy_from_slice(&source);
        }
        let report = analyze(&samples, 48000, 48000, sweep()).unwrap();
        assert!(report.segments.is_none());
        assert!(report.messages[0].contains("multiple plausible reference pairs"));
        let short = analyze(&samples[..48000], 48000, 48000, sweep()).unwrap();
        assert!(short.segments.is_none());
        samples[0] = f32::NAN;
        assert!(analyze(&samples, 48000, 48000, sweep()).is_err());
        assert!(analyze(&[], 48000, 48000, sweep()).is_err());
        assert!(analyze(&[0.0; 10000], 0, 48000, sweep()).is_err());
    }
}
