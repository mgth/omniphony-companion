//! Locate the outer references of a continuous batch, then retain overlapping
//! raw windows. Local timing/arrival gates still run independently on each take.
use crate::timing::{Peak, correlation_scores, peak_candidates_bounded, resample};
use omniphony_osc_contract::measurement::{Sweep, sequence_frames};
use std::ops::Range;

/// About 146 s at 48 kHz: the renderer's 120 s sequence limit with 3 s room-correction
/// sweeps. With its correlation template it stays inside the 8 Mi-point FFT bound of
/// [`crate::timing`]; the recording itself is 28 MB of float samples.
pub const MAX_BATCH_FRAMES: usize = 7_000_000;

pub fn capture_budget(
    sweep: Sweep,
    render_rate: u32,
    capture_rate: u32,
    targets: usize,
) -> Result<usize, String> {
    if !(8000..=192000).contains(&capture_rate) {
        return Err("invalid input sample rate".into());
    }
    let frames = sequence_frames(sweep, render_rate, targets)?;
    let budget = (frames as f64 / render_rate as f64 * capture_rate as f64 * 1.002).ceil() as usize
        + capture_rate as usize * 4;
    if budget > MAX_BATCH_FRAMES {
        return Err("batch exceeds the recording budget; use fewer speakers, a shorter sweep or a lower sample rate".into());
    }
    Ok(budget)
}

/// Ranges are absolute in `samples`. Identical shared references remain identical
/// raw samples, with no resampling, stitching, or invented silent intervals.
pub fn windows(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
    targets: usize,
) -> Result<Vec<Range<usize>>, String> {
    capture_budget(sweep, render_rate, capture_rate, targets)?;
    if samples.len() > MAX_BATCH_FRAMES || samples.iter().any(|v| !v.is_finite() || v.abs() > 16.0)
    {
        return Err("invalid sequence recording".into());
    }
    let source = sweep.samples(render_rate)?;
    if samples.iter().all(|s| *s == 0.0) {
        return Err(crate::input::NO_SIGNAL.into());
    }
    let nominal = capture_rate as f64 / render_rate as f64;
    let (first, _) = endpoints(
        samples,
        &resample(&source, nominal),
        source.len() as f64 * nominal,
        targets,
    )?;
    let ratio = (first[1].position - first[0].position) / (2 * targets * source.len()) as f64;
    let (pair, scores) = endpoints(
        samples,
        &resample(&source, ratio),
        source.len() as f64 * ratio,
        targets,
    )?;
    let ratio = (pair[1].position - pair[0].position) / (2 * targets * source.len()) as f64;
    if (ratio / nominal - 1.0).abs() > 0.002 {
        return Err("sequence reference spacing exceeds the clock-drift limit".into());
    }
    let spacing = source.len() as f64 * ratio;
    // Endpoint drift cannot explain a dropped block or a route latency change.
    // Inspect every shared reference before assigning target windows.
    for i in 1..targets {
        let expected = pair[0].position + i as f64 * 2.0 * spacing;
        let radius = spacing * 0.15;
        let lo = (expected - radius).floor().max(0.0) as usize;
        let hi = ((expected + radius).ceil() as usize).min(scores.len());
        let (offset, &score) = scores[lo..hi]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .ok_or("missing shared reference")?;
        let residual_ms = (lo as f64 + offset as f64 - expected) / capture_rate as f64 * 1000.0;
        if score < 0.2 || residual_ms.abs() > 2.0 {
            return Err(format!(
                "shared reference {} has inconsistent timing ({residual_ms:+.2} ms from the clock model); raw batch retained. Check playback/capture continuity and repeat",
                i + 1
            ));
        }
    }
    // Extra real audio beyond the 150 ms IR tail tolerates local anchor
    // rounding and small differences between local/global clock estimates.
    let margin = capture_rate as f64 * 0.20;
    let mut ranges = Vec::with_capacity(targets);
    for i in 0..targets {
        let start = pair[0].position + i as f64 * 2.0 * spacing - margin;
        let end = pair[0].position + (i * 2 + 3) as f64 * spacing + margin;
        if start < 0.0 || end.ceil() as usize > samples.len() {
            return Err("sequence lacks complete reference windows and quiet margins".into());
        }
        ranges.push(start.floor() as usize..end.ceil() as usize);
    }
    Ok(ranges)
}

fn endpoints(
    samples: &[f32],
    template: &[f32],
    spacing: f64,
    targets: usize,
) -> Result<([Peak; 2], Vec<f64>), String> {
    let scores = correlation_scores(samples, template)?;
    let peaks = peak_candidates_bounded(&scores, spacing, targets * 2 + 5);
    let distance = targets as f64 * 2.0 * spacing;
    let mut pairs = Vec::new();
    for (i, &a) in peaks.iter().enumerate() {
        for &b in peaks.iter().skip(i + 1) {
            if ((b.position - a.position) / distance - 1.0).abs() <= 0.004 {
                pairs.push([a, b]);
            }
        }
    }
    let quality = |p: &[Peak; 2]| p[0].score.min(p[1].score);
    pairs.sort_by(|a, b| quality(b).total_cmp(&quality(a)));
    let best = *pairs
        .first()
        .ok_or("outer sequence references were not found; raw batch retained")?;
    if pairs
        .get(1)
        .is_some_and(|p| quality(p) > quality(&best) * 0.8)
    {
        return Err("ambiguous outer sequence references; raw batch retained".into());
    }
    Ok((best, scores))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_windows_keep_delayed_targets_quiet_noise_and_complete_ir_tails() {
        let sweep = Sweep {
            duration_ms: 1000,
            peak_dbfs: -24.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        };
        let source = sweep.samples(48000).unwrap();
        let mut audio = vec![0.0; source.len() * 7 + 48000];
        for i in 0..7 {
            // Tiny local reference changes exercise rounding at the IR tail.
            let shift = if i % 2 == 1 {
                7680
            } else if i == 2 {
                2
            } else {
                0
            };
            let start = 24000 + i * source.len() + shift;
            for (dst, src) in audio[start..].iter_mut().zip(&source) {
                *dst += src;
            }
        }
        let ranges = windows(&audio, 48000, 48000, sweep, 3).unwrap();
        for range in ranges {
            let samples = &audio[range];
            let timing = crate::timing::analyze(samples, 48000, 48000, sweep).unwrap();
            assert!((timing.relative_peak_delay_ms.unwrap() - 160.0).abs() < 0.1);
            let ir = crate::impulse::analyze(samples, 48000, 48000, sweep, &timing).unwrap();
            assert!(
                ir.relative_arrival_ms.is_some(),
                "{:?}",
                ir.unresolved_reasons
            );
            let diagnostics = crate::diagnostics::analyze(samples, 48000, 48000, sweep).unwrap();
            assert_eq!(
                diagnostics.segments.unwrap()[1].status,
                crate::diagnostics::SegmentStatus::Detected
            );
        }
        // A dropped capture block near the last target must not become linear drift.
        let mut audio = vec![0.0; source.len() * 13 + 48000];
        for i in 0..13 {
            let start = 24000 + i * source.len();
            for (dst, src) in audio[start..].iter_mut().zip(&source) {
                *dst += src;
            }
        }
        let at = 24000 + 11 * source.len();
        audio.drain(at..at + 1024);
        assert!(
            windows(&audio, 48000, 48000, sweep, 6)
                .unwrap_err()
                .contains("inconsistent timing")
        );
    }
    #[test]
    fn bass_only_sequences_keep_their_windows_and_responses() {
        // A subwoofer measured against itself: the arrival review refuses the band,
        // but the windows, the timing anchors and the stored response remain.
        let sweep = Sweep {
            duration_ms: 3000,
            peak_dbfs: -20.0,
            start_hz: 20.0,
            end_hz: 120.0,
        };
        let source = sweep.samples(48000).unwrap();
        let mut audio = vec![0.0; source.len() * 5 + 48000];
        for i in 0..5 {
            let start = 24000 + i * source.len();
            for (dst, src) in audio[start..].iter_mut().zip(&source) {
                *dst += src * 0.5;
            }
        }
        let ranges = windows(&audio, 48000, 48000, sweep, 2).unwrap();
        for range in ranges {
            let samples = &audio[range];
            let timing = crate::timing::inspect(samples, 48000, 48000, sweep).unwrap();
            assert!(crate::impulse::analyze(samples, 48000, 48000, sweep, &timing).is_err());
            let full =
                crate::impulse::full_response(samples, 48000, 48000, sweep, &timing).unwrap();
            let magnitude = crate::response::magnitude(&full, sweep, None).unwrap();
            for (i, &hz) in magnitude.frequency_hz.iter().enumerate() {
                if (25.0..=100.0).contains(&hz) {
                    let db = f64::from(magnitude.magnitude_db[1][i]);
                    assert!((db - 20.0 * 0.5_f64.log10()).abs() < 0.2, "{hz}: {db}");
                }
            }
            let band = timing.band_noise.unwrap();
            assert_eq!(band.band_hz, [20.0, 120.0]);
        }
    }
    #[test]
    fn a_treble_only_sweep_keeps_its_timing_anchors_and_response() {
        let sweep = Sweep {
            duration_ms: 1000,
            peak_dbfs: -20.0,
            start_hz: 12000.0,
            end_hz: 18000.0,
        };
        let source = sweep.samples(48000).unwrap();
        let mut audio = vec![0.0; source.len() * 3 + 48000];
        for i in 0..3 {
            let start = 24000 + i * source.len();
            for (dst, src) in audio[start..].iter_mut().zip(&source) {
                *dst += src * 0.5;
            }
        }
        let timing = crate::timing::inspect(&audio, 48000, 48000, sweep).unwrap();
        assert_eq!(
            timing.band_noise.as_ref().unwrap().band_hz,
            [12000.0, 18000.0]
        );
        assert!(crate::impulse::full_response(&audio, 48000, 48000, sweep, &timing).is_ok());
    }
    #[test]
    fn zero_batch_is_reported_as_missing_input_not_missing_references() {
        let sweep = Sweep {
            duration_ms: 1000,
            peak_dbfs: -12.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        };
        let samples = vec![0.0; 2_013_184];
        assert_eq!(
            windows(&samples, 48000, 48000, sweep, 13).unwrap_err(),
            crate::input::NO_SIGNAL
        );
        assert_eq!(
            crate::timing::analyze(&samples[..240000], 48000, 48000, sweep).unwrap_err(),
            crate::input::NO_SIGNAL
        );
        let diagnostics =
            crate::diagnostics::analyze(&samples[..240000], 48000, 48000, sweep).unwrap();
        assert_eq!(diagnostics.messages, [crate::input::NO_SIGNAL]);
        assert!(diagnostics.segments.is_none());
    }
    #[test]
    fn overlapping_windows_retain_shared_reference_and_local_delays_with_drift() {
        let sweep = Sweep {
            duration_ms: 500,
            peak_dbfs: -24.0,
            start_hz: 200.0,
            end_hz: 6000.0,
        };
        let source = sweep.samples(48000).unwrap();
        let ratio = 16000.0 / 48000.0 * 1.0003;
        let signal = resample(&source, ratio);
        let mut audio = vec![0.0; 16000 * 9];
        for i in 0..7 {
            let start = (8000.0 + i as f64 * source.len() as f64 * ratio).round() as usize
                + if i % 2 == 1 { 80 } else { 0 };
            for (dst, src) in audio[start..].iter_mut().zip(&signal) {
                *dst += src;
            }
        }
        let ranges = windows(&audio, 16000, 48000, sweep, 3).unwrap();
        for range in &ranges {
            let report =
                crate::timing::analyze(&audio[range.clone()], 16000, 48000, sweep).unwrap();
            assert!((report.relative_peak_delay_ms.unwrap() - 5.0).abs() < 0.1);
        }
        let a = &ranges[0];
        let b = &ranges[1];
        assert!(a.end > b.start + signal.len());
        assert_eq!(
            &audio[a.clone()][b.start - a.start..],
            &audio[b.clone()][..a.end - b.start]
        );
        // Missing outer reference cannot silently shift speaker identities.
        audio[8000..8000 + signal.len()].fill(0.0);
        assert!(windows(&audio, 16000, 48000, sweep, 3).is_err());
        assert!(capture_budget(sweep, 48000, 192000, 32).is_err());
    }
}
