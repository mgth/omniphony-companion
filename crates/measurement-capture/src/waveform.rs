//! Signed, bounded display traces and virtual tone bursts. No new acoustic evidence.
use crate::spectral::SpectralReport;
use rustfft::{FftPlanner, num_complex::Complex};
use serde::Serialize;

const MAX_BUCKETS: usize = 1600;
const CYCLES: usize = 6;

#[derive(Debug, Clone, Serialize)]
pub struct WaveformReport {
    pub method: &'static str,
    pub impulse: TracePair,
    pub bursts: Vec<BurstResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TracePair {
    pub overview: SignedTrace,
    pub detail: SignedTrace,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedTrace {
    pub start_ms: f64,
    pub bucket_ms: f64,
    /// Signed min/max per bucket; never an absolute-value envelope or decimated sine.
    pub extrema: Vec<[f32; 2]>,
    pub peak: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BurstResponse {
    pub frequency_hz: f64,
    pub cycles: usize,
    pub duration_ms: f64,
    /// Wrapped transfer phase, with only the common correlation anchor removed.
    pub phase_deg: Option<f64>,
    pub input: TracePair,
    pub predicted: TracePair,
}

/// Convolve the measured full-band IR with a causal Hann-windowed six-cycle sine.
/// The source starts at the segment's common zero, even when the response is early.
/// The fixed response window, room and measurement chain remain in the prediction.
pub fn analyze(
    impulse: &[f64],
    rate: f64,
    anchor: usize,
    spectral: &SpectralReport,
    candidate_ms: Option<f64>,
) -> Option<WaveformReport> {
    if !rate.is_finite()
        || !(7900.0..=193000.0).contains(&rate)
        || impulse.is_empty()
        || impulse.len() > (rate * 0.3) as usize
        || anchor >= impulse.len()
        || impulse.iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let maximum = impulse.iter().map(|v| v.abs()).fold(0.0, f64::max);
    if maximum <= 1e-15 || !maximum.is_finite() {
        return None;
    }
    let peak = impulse
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?
        .0;
    let focus = candidate_ms
        .filter(|v| v.is_finite() && v.abs() <= 100.0)
        .unwrap_or((peak as f64 - anchor as f64) / rate * 1000.0);
    let impulse_traces = trace_pair(impulse, rate, anchor, [focus - 5.0, focus + 10.0]);
    let mut planner = FftPlanner::<f64>::new();
    let mut bursts = Vec::new();
    for frequency in [250.0, 500.0, 1000.0, 2000.0, 4000.0, 6000.0, 8000.0] {
        // Keep the Hann burst's main lobe inside measured excitation support.
        // Its remaining sidelobes are still limited by the measured IR bandwidth.
        if frequency * (1.0 - 2.0 / CYCLES as f64) < spectral.frequency_bounds_hz[0]
            || frequency * (1.0 + 2.0 / CYCLES as f64) > spectral.frequency_bounds_hz[1]
        {
            continue;
        }
        let len = (CYCLES as f64 * rate / frequency).round() as usize + 1;
        let output_len = impulse.len().checked_add(len)?.checked_sub(1)?;
        let n = output_len.checked_next_power_of_two()?;
        if n > 131072 {
            return None;
        }
        let forward = planner.plan_fft_forward(n);
        let inverse = planner.plan_fft_inverse(n);
        let mut h = vec![Complex::default(); n];
        let mut b = h.clone();
        let taper = rate * 0.005;
        for (i, &v) in impulse.iter().enumerate() {
            let edge = (i.min(impulse.len() - 1 - i) as f64 / taper).min(1.0);
            h[i].re = v * 0.5 * (1.0 - (std::f64::consts::PI * edge).cos());
        }
        let transfer =
            h[..impulse.len()]
                .iter()
                .enumerate()
                .fold(Complex::<f64>::default(), |sum, (i, v)| {
                    let angle =
                        -std::f64::consts::TAU * frequency * (i as f64 - anchor as f64) / rate;
                    sum + *v * Complex::from_polar(1.0, angle)
                });
        let phase = transfer.arg().to_degrees();
        let phase_reliable = spectral
            .points
            .iter()
            .min_by(|a, b| {
                (a.frequency_hz - frequency)
                    .abs()
                    .total_cmp(&(b.frequency_hz - frequency).abs())
            })
            .is_some_and(|p| {
                p.magnitude_db
                    .is_some_and(|db| transfer.norm_sqr() > 10_f64.powf(db / 10.0) * 0.01)
                    && p.estimated_snr_db >= 20.0
            });
        let mut input = vec![0.0; output_len];
        for i in 0..len {
            let weight = 0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / (len - 1) as f64).cos());
            let v = weight * (std::f64::consts::TAU * frequency * i as f64 / rate).sin();
            b[i].re = v;
            input[anchor + i] = v;
        }
        forward.process(&mut h);
        forward.process(&mut b);
        for (a, b) in h.iter_mut().zip(b) {
            *a *= b;
        }
        inverse.process(&mut h);
        let predicted: Vec<_> = h[..output_len].iter().map(|v| v.re / n as f64).collect();
        let duration_ms = (len - 1) as f64 / rate * 1000.0;
        // Detail follows the existing arrival region, never a new strongest-peak alignment.
        // Include source time zero so overlays retain their common time origin.
        let detail = [
            (focus - duration_ms).min(-duration_ms * 0.5),
            (focus + duration_ms * 2.0).max(duration_ms * 1.5),
        ];
        bursts.push(BurstResponse {
            frequency_hz: frequency,
            cycles: CYCLES,
            duration_ms,
            phase_deg: (phase_reliable && phase.is_finite()).then_some(phase),
            input: trace_pair(&input, rate, anchor, detail),
            predicted: trace_pair(&predicted, rate, anchor, detail),
        });
    }
    Some(WaveformReport {
        method: "signed-ir-and-virtual-burst-v1",
        impulse: impulse_traces,
        bursts,
    })
}

fn trace_pair(samples: &[f64], rate: f64, anchor: usize, detail: [f64; 2]) -> TracePair {
    TracePair {
        overview: trace(samples, rate, anchor, [-50.0, 100.0]),
        detail: trace(samples, rate, anchor, detail),
    }
}

fn trace(samples: &[f64], rate: f64, anchor: usize, range: [f64; 2]) -> SignedTrace {
    let frame = |ms: f64| {
        (anchor as f64 + ms * rate / 1000.0)
            .round()
            .clamp(0.0, samples.len() as f64) as usize
    };
    let start = frame(range[0]);
    let end = frame(range[1]).max(start);
    let width = (end - start).div_ceil(MAX_BUCKETS).max(1);
    let peak = samples.iter().map(|v| v.abs()).fold(0.0, f64::max);
    SignedTrace {
        start_ms: (start as f64 - anchor as f64) / rate * 1000.0,
        bucket_ms: width as f64 / rate * 1000.0,
        peak,
        extrema: samples[start..end]
            .chunks(width)
            .map(|chunk| {
                [
                    chunk.iter().copied().fold(f64::INFINITY, f64::min) as f32,
                    chunk.iter().copied().fold(f64::NEG_INFINITY, f64::max) as f32,
                ]
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omniphony_osc_contract::measurement::Sweep;

    #[test]
    fn virtual_burst_preserves_delay_polarity_phase_and_gain() {
        for rate in [16000.0, 48000.0, 96000.0] {
            for delay_ms in [-2.25, 3.25] {
                let anchor = (rate * 0.1) as usize;
                let mut h = vec![0.0; (rate * 0.25) as usize + 1];
                h[(anchor as f64 + delay_ms * rate / 1000.0) as usize] = -0.5;
                let s = crate::spectral::analyze(
                    &h,
                    rate,
                    anchor,
                    Sweep {
                        start_hz: 200.0,
                        end_hz: 7000.0,
                        duration_ms: 1000,
                        peak_dbfs: -30.0,
                    },
                )
                .unwrap();
                let r = analyze(&h, rate, anchor, &s, Some(delay_ms)).unwrap();
                let b = r.bursts.iter().find(|b| b.frequency_hz == 1000.0).unwrap();
                assert_eq!(b.cycles, 6);
                assert!((b.predicted.detail.peak / b.input.detail.peak - 0.5).abs() < 1e-6);
                assert!(b.predicted.detail.extrema.len() <= MAX_BUCKETS);
                let expected = (180.0 - 360.0 * delay_ms).to_radians();
                assert!((b.phase_deg.unwrap().to_radians() - expected).sin().abs() < 1e-6);
                assert!((b.phase_deg.unwrap().to_radians() - expected).cos() > 0.999999);
                let trace = &b.predicted.detail;
                for (i, &[lo, hi]) in trace.extrema.iter().enumerate() {
                    let width = (trace.bucket_ms * rate / 1000.0).round() as usize;
                    let values: Vec<_> = (0..width)
                        .map(|j| {
                            let t = trace.start_ms
                                + i as f64 * trace.bucket_ms
                                + j as f64 / rate * 1000.0
                                - delay_ms;
                            if (0.0..=6.0).contains(&t) {
                                -0.5 * 0.5
                                    * (1.0 - (std::f64::consts::TAU * t / 6.0).cos())
                                    * (std::f64::consts::TAU * t).sin()
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    let expected_lo = values.iter().copied().fold(f64::INFINITY, f64::min);
                    let expected_hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    assert!(
                        (f64::from(lo) - expected_lo).abs() < 1e-5,
                        "{rate} {delay_ms} {i}"
                    );
                    assert!((f64::from(hi) - expected_hi).abs() < 1e-5);
                }
            }
        }
    }

    #[test]
    fn bursts_keep_an_early_path_and_later_echo_without_creating_a_verdict() {
        let rate = 48000.0;
        let mut h = vec![0.0; 12001];
        h[4800] = 0.4;
        h[5760] = 1.0;
        let s = crate::spectral::analyze(
            &h,
            rate,
            4800,
            Sweep {
                start_hz: 200.0,
                end_hz: 10000.0,
                duration_ms: 1000,
                peak_dbfs: -30.0,
            },
        )
        .unwrap();
        let r = analyze(&h, rate, 4800, &s, Some(0.0)).unwrap();
        let b = r.bursts.iter().find(|b| b.frequency_hz == 1000.0).unwrap();
        let maximum = |lo: f64, hi: f64| {
            b.predicted
                .overview
                .extrema
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    let t =
                        b.predicted.overview.start_ms + *i as f64 * b.predicted.overview.bucket_ms;
                    t >= lo && t < hi
                })
                .map(|(_, v)| f64::from(v[0].abs().max(v[1].abs())))
                .fold(0.0, f64::max)
        };
        assert!((maximum(0.0, 6.0) / maximum(20.0, 26.0) - 0.4).abs() < 0.01);
        assert!(
            r.bursts
                .iter()
                .all(|b| b.frequency_hz != 250.0 && b.frequency_hz != 8000.0)
        );
        assert!(analyze(&[], rate, 0, &s, None).is_none());
        assert!(analyze(&vec![0.0; 12001], rate, 4800, &s, None).is_none());
        assert!(analyze(&h, f64::NAN, 4800, &s, None).is_none());
        assert!(analyze(&h, rate, h.len(), &s, None).is_none());
        let mut weak = s.clone();
        for point in &mut weak.points {
            point.estimated_snr_db = 0.0;
        }
        assert!(
            analyze(&h, rate, 4800, &weak, None)
                .unwrap()
                .bursts
                .iter()
                .all(|b| b.phase_deg.is_none())
        );
    }
}
