//! Locate the outer references of a continuous batch, then retain overlapping
//! raw windows. Local timing/arrival gates still run independently on each take.
use crate::timing::{Peak, correlation_scores, peak_candidates_bounded, resample};
use omniphony_osc_contract::measurement::{Sweep, sequence_frames};
use std::ops::Range;

pub const MAX_BATCH_FRAMES: usize = 4_000_000;

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
    let nominal = capture_rate as f64 / render_rate as f64;
    let first = endpoints(
        samples,
        &resample(&source, nominal),
        source.len() as f64 * nominal,
        targets,
    )?;
    let ratio = (first[1].position - first[0].position) / (2 * targets * source.len()) as f64;
    let pair = endpoints(
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
    let margin = capture_rate as f64 * 0.15;
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
) -> Result<[Peak; 2], String> {
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
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
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
