//! How far a clipped capture overshoots digital full scale.
//!
//! At any instant the sound of the sweep at the microphone is a sinusoid whose
//! frequency the sweep definition and its match position give. Two unclipped samples
//! `x_a`, `x_b`, `k` samples apart, of a sinusoid advancing `ω` radians per sample fix
//! its amplitude: `A² = (x_a² + x_b² − 2·x_a·x_b·cos kω) / sin² kω`. Each plateau is
//! measured from the unclipped samples just before it, so the estimate follows the
//! level at the microphone along the sweep, including the beating of room echoes. When
//! the overdrive leaves too few of them, the `r` unclipped samples between opposite
//! plateaus still bound it: crossing from −c to +c within `r + 1` samples needs
//! `A ≥ c / sin((r + 1)·ω/2)`. A running median of five plateaus rejects outliers such
//! as a knock during the sweep. Only digital full-scale clipping is visible: an analog
//! preamplifier saturating below full scale leaves no plateau, is not detected and
//! gets no number.
use crate::level::BAND_HZ;
use omniphony_osc_contract::measurement::Sweep;
use std::f64::consts::{FRAC_PI_2, TAU};

/// A sample at or above this magnitude counts as clipped.
pub const CLIP_LEVEL: f32 = 0.999;
/// Estimates saturate here; the advice no longer depends on the exact value.
pub const MAX_OVERDRIVE_DB: f64 = 20.0;
/// Headroom added to the overdrive in the suggested gain reduction.
pub const MARGIN_DB: f64 = 3.0;
/// Preferred phase between the two samples: wide enough that noise does not dominate,
/// short enough to stay inside the unclipped run before a plateau.
const LAG_PHASE: f64 = 0.5;
/// Fits whose two samples are closer than this in phase are skipped: `sin kω` near
/// zero would turn noise into an outlier. Only a steep crossing at a low frequency,
/// where the gap bound applies, gets there.
const MIN_SIN: f64 = 0.02;
const MEDIAN: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overdrive {
    /// Largest overshoot along the sweep, in dB above full scale.
    pub max_db: f64,
    /// Largest overshoot inside the 500–2000 Hz level band.
    pub band_db: Option<f64>,
}

/// `sweep_start` is the capture frame where the sweep body starts and `rate` the capture
/// frames per renderer second. None when no plateau along the sweep can be measured;
/// clipping outside the sweep (a knock, a late echo past its end) is not a sinusoid.
pub fn estimate(samples: &[f32], sweep_start: f64, rate: f64, sweep: Sweep) -> Option<Overdrive> {
    let duration = sweep.duration_ms as f64 / 1000.0;
    let start_hz = sweep.start_hz as f64;
    let log_ratio = (sweep.end_hz as f64 / start_hz).ln();
    if !(sweep_start.is_finite() && rate.is_finite() && rate > 0.0 && log_ratio > 0.0) {
        return None;
    }
    let frame = |t: f64| ((sweep_start + t * rate).round().max(0.0) as usize).min(samples.len());
    // The instantaneous frequency of `Sweep::samples`, at a capture frame.
    let hz = |i: usize| start_hz * (log_ratio * (i as f64 - sweep_start) / rate / duration).exp();
    let mut recent = [(0.0, 0.0); MEDIAN];
    let mut count = 0;
    let mut result: Option<Overdrive> = None;
    let mut keep = |recent: &[(f64, f64)]| {
        let mut sorted = [(0.0, 0.0); MEDIAN];
        let sorted = &mut sorted[..recent.len()];
        sorted.copy_from_slice(recent);
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (db, at_hz) = sorted[sorted.len() / 2];
        let r = result.get_or_insert(Overdrive {
            max_db: db,
            band_db: None,
        });
        r.max_db = r.max_db.max(db);
        if (BAND_HZ[0]..=BAND_HZ[1]).contains(&at_hz) {
            r.band_db = Some(r.band_db.map_or(db, |b| b.max(db)));
        }
    };
    let (lo, hi) = (frame(0.0), frame(duration));
    let clip = CLIP_LEVEL as f64;
    // Unclipped samples since the last clipped one, and that one's sign (0 before any).
    let mut run = 0;
    let mut last_sign = 0.0;
    for i in lo..hi {
        if samples[i].abs() < CLIP_LEVEL {
            run += 1;
            continue;
        }
        let sign = samples[i].signum();
        let (gap, opposite, onset) = (run, last_sign == -sign, run > 0 || sign != last_sign);
        run = 0;
        last_sign = sign;
        if !onset {
            continue;
        }
        let f = hz(i);
        let omega = TAU * f / rate;
        let mut db = f64::NEG_INFINITY;
        if opposite {
            let half = ((gap + 1) as f64 * omega * 0.5).min(FRAC_PI_2);
            db = 20.0 * (clip / half.sin()).log10();
        }
        if gap >= 2 {
            let k = ((LAG_PHASE / omega).round() as usize).clamp(1, gap - 1);
            let (sin, cos) = (k as f64 * omega).sin_cos();
            if sin >= MIN_SIN {
                let (a, b) = (samples[i - 1 - k] as f64, samples[i - 1] as f64);
                let power = (a * a + b * b - 2.0 * a * b * cos) / (sin * sin);
                db = db.max(10.0 * power.log10());
            }
        }
        if db == f64::NEG_INFINITY {
            continue;
        }
        recent[count % MEDIAN] = (db.clamp(0.0, MAX_OVERDRIVE_DB), f);
        count += 1;
        if count >= MEDIAN {
            keep(&recent);
        }
    }
    // Too few plateaus for a full median: use those there are.
    if (1..MEDIAN).contains(&count) {
        keep(&recent[..count]);
    }
    result
}

/// The overdrive as acquisition messages write it, e.g. "about 4 dB over full scale".
pub fn describe(db: f64) -> String {
    if db >= MAX_OVERDRIVE_DB {
        format!("more than {MAX_OVERDRIVE_DB:.0} dB over full scale")
    } else {
        format!("about {:.0} dB over full scale", db.max(1.0))
    }
}

/// The overdrive for reports and tables, e.g. "4.2 dB" or "> 20 dB".
pub fn label(db: f64) -> String {
    if db >= MAX_OVERDRIVE_DB {
        format!("> {MAX_OVERDRIVE_DB:.0} dB")
    } else {
        format!("{db:.1} dB")
    }
}

/// Whole decibels to lower the input gain or test level by: overdrive plus margin.
pub fn reduction_db(db: f64) -> f64 {
    (db + MARGIN_DB).ceil()
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

    #[test]
    fn wording_and_suggested_reduction() {
        assert_eq!(describe(4.4), "about 4 dB over full scale");
        assert_eq!(describe(0.3), "about 1 dB over full scale");
        assert_eq!(describe(20.0), "more than 20 dB over full scale");
        assert_eq!(label(4.24), "4.2 dB");
        assert_eq!(label(20.0), "> 20 dB");
        assert_eq!(reduction_db(4.0), 7.0);
        assert_eq!(reduction_db(4.2), 8.0);
    }

    #[test]
    fn unclipped_or_off_sweep_clipping_has_no_estimate() {
        let source = sweep().samples(48000).unwrap();
        let start = 12000.0;
        assert_eq!(estimate(&source, start, 48000.0, sweep()), None);
        let mut knock = source.clone();
        knock[100] = 1.0;
        assert_eq!(estimate(&knock, start, 48000.0, sweep()), None);
        assert_eq!(estimate(&[], start, 48000.0, sweep()), None);
    }

    #[test]
    fn steep_plateaus_are_bounded_rather_than_divided_through() {
        // At 20 Hz and 192 kHz, plateaus two samples apart leave the fit no phase between
        // its samples: it is skipped, and the gap bounds the level instead.
        let low = Sweep {
            start_hz: 20.0,
            end_hz: 40.0,
            ..sweep()
        };
        let mut samples = vec![0.0; 192000];
        for (half, chunk) in samples[..2040].chunks_mut(102).enumerate() {
            let sign = if half % 2 == 0 { 1.0 } else { -1.0 };
            chunk.fill(sign);
            chunk[0] = -0.0005 * sign;
            chunk[1] = 0.0005 * sign;
        }
        let steep = estimate(&samples, 0.0, 192000.0, low).unwrap();
        assert_eq!(steep.max_db, MAX_OVERDRIVE_DB);
        // One clean plateau gives its own reading.
        let amplitude = 2.0_f64;
        let omega = TAU * 20.0 / 192000.0;
        samples.fill(0.0);
        for (i, x) in samples[..2000].iter_mut().enumerate() {
            *x = (amplitude * (omega * i as f64).sin()).clamp(-1.0, 1.0) as f32;
        }
        let one = estimate(&samples, 0.0, 192000.0, low).unwrap();
        assert!(
            (one.max_db - 20.0 * amplitude.log10()).abs() < 0.1,
            "{one:?}"
        );
        assert_eq!(one.band_db, None);
    }
}
