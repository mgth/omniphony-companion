//! Bounded, offline swept-sine deconvolution and conservative arrival candidates.
//! An early, repeatable peak is evidence, not proof of an unobstructed direct path.
use crate::timing::{TimingReport, resample};
use omniphony_osc_contract::measurement::{SETTLE_MS, Sweep};
use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

pub const PRE_SECONDS: f64 = 0.1;
const POST_SECONDS: f64 = 0.15;
/// The unweighted response kept for room correction runs this long after the
/// correlation anchor. The extraction window ends 150 ms after the segment's 250 ms
/// trailing silence, so every swept frequency is fully supported up to 400 ms.
pub const FULL_POST_SECONDS: f64 = 0.35;
const REGULARIZATION: f64 = 1e-5;
const MAX_FFT: usize = 4_194_304;
const MAX_BAND_SPREAD_MS: f64 = 0.6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpulseReport {
    pub method: String,
    pub bands_hz: [[f64; 2]; 3],
    pub segments: [ImpulseSegment; 3],
    /// Signed waveform correlation after aligning the same reference speaker.
    pub reference_similarity: Option<f64>,
    /// Difference between waveform alignment and the candidate arrival alignment.
    pub reference_alignment_residual_ms: Option<f64>,
    /// Candidate timing includes the renderer's existing output delay settings.
    pub relative_arrival_ms: Option<f64>,
    pub clock_drift_ppm: Option<f64>,
    /// Heuristic sensitivity bound, not a statistical confidence interval.
    pub uncertainty_ms: Option<f64>,
    pub unresolved_reasons: Vec<String>,
    pub valid_for_correction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpulseSegment {
    pub role: String,
    /// Relative to the analysis start in the recording, as in TimingReport.
    pub correlation_start_frame: f64,
    pub candidate_start_frame: Option<f64>,
    /// Offsets from this segment's correlation peak, not absolute propagation times.
    pub band_candidate_offset_ms: [Option<f64>; 3],
    /// Per band, candidate offsets at 20%, 30% and 40% of the peak envelope.
    pub threshold_candidate_offset_ms: [[Option<f64>; 3]; 3],
    pub candidate_offset_ms: Option<f64>,
    pub sensitivity_ms: Option<f64>,
    pub noise_db_relative_peak: f64,
    pub response_start_ms: f64,
    pub response_step_ms: f64,
    /// Band-limited impulse samples; not microphone-calibrated or SPL-normalized.
    pub response: Vec<f32>,
    /// Peak-preserving envelope buckets for bounded-cost display (at most 1600).
    pub envelope_db: Vec<f32>,
    pub envelope_step_ms: f64,
    pub spectral: Option<crate::spectral::SpectralReport>,
    pub spectral_error: Option<String>,
    pub waveforms: Option<crate::waveform::WaveformReport>,
}

/// The deconvolved response of each segment with no band weighting, kept apart from
/// [`ImpulseReport`] so reports held for display stay compact. Not microphone-calibrated.
#[derive(Debug, Clone)]
pub struct FullResponse {
    /// Capture frames per renderer second: the response's time base after clock
    /// compensation. A file header can only hold the nominal integer capture rate.
    pub rate: f64,
    /// Index of the segment's correlation anchor, [`PRE_SECONDS`] into each response.
    pub anchor: usize,
    /// How far, in frames, each segment's correlation peak lies after its anchor frame:
    /// the window starts on a whole frame. Within ±0.5.
    pub anchor_offsets: [f64; 3],
    /// How far, in frames, the speaker segment's correlation peak lies after the
    /// midpoint of the two reference peaks: where it would be with the reference's own
    /// acoustic delay. Responses measured against the same reference at one position
    /// share this time base, whichever sequence played them.
    pub target_offset: f64,
    /// `reference_before`, `speaker`, `reference_after`, of equal length.
    pub segments: [Vec<f32>; 3],
}

/// The timing report must come from `timing::analyze` on these same samples.
/// All calculations run after capture; nothing here is called by an audio callback.
pub fn analyze(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
    timing: &TimingReport,
) -> Result<ImpulseReport, String> {
    analyze_inner(samples, capture_rate, render_rate, sweep, timing, false)
}

/// Distorted clipped traces may be inspected, but callers must retain their fault.
pub fn inspect(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
    timing: &TimingReport,
) -> Result<ImpulseReport, String> {
    analyze_inner(samples, capture_rate, render_rate, sweep, timing, true)
}

/// The unweighted response of every segment, for room correction. Unlike the arrival
/// review it accepts any valid sweep, bass-only ones included, and inspects clipped
/// recordings: callers keep their fault. The template is the sweep evaluated at the
/// clock-compensated rate rather than interpolated, so the treble keeps its level when
/// the capture and render clocks differ.
pub fn full_response(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
    timing: &TimingReport,
) -> Result<FullResponse, String> {
    sweep.validate(render_rate)?;
    sweep.validate(capture_rate)?;
    if samples.is_empty()
        || samples.len() > capture_rate as usize * crate::MAX_SECONDS as usize
        || samples.iter().any(|x| !x.is_finite() || x.abs() > 16.0)
    {
        return Err("invalid response input".into());
    }
    let peaks = [
        timing.reference_start_frames[0],
        timing.target_start_frame,
        timing.reference_start_frames[1],
    ];
    if peaks
        .iter()
        .any(|p| !p.is_finite() || *p < 0.0 || *p >= samples.len() as f64)
        || !(peaks[0] < peaks[1] && peaks[1] < peaks[2])
    {
        return Err("invalid sequence anchors for the response".into());
    }
    let frames = sweep.frames(render_rate) as f64;
    let ratio = (peaks[2] - peaks[0]) / (2.0 * frames);
    let nominal = capture_rate as f64 / render_rate as f64;
    if (ratio / nominal - 1.0).abs() > 0.002 {
        return Err("clock ratio exceeds the response analysis limit".into());
    }
    let rate = render_rate as f64 * ratio;
    let template = analytic_template(sweep, render_rate, ratio);
    let pre = (PRE_SECONDS * rate).round() as usize;
    let post = (POST_SECONDS * rate).round() as usize;
    let len = pre + (FULL_POST_SECONDS * rate).round() as usize + 1;
    let window_len = template.len() + pre + post;
    let fft_len = (window_len + template.len()).next_power_of_two();
    if fft_len > MAX_FFT {
        return Err("response FFT exceeds the offline memory bound".into());
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut x = vec![Complex::default(); fft_len];
    for (dest, &sample) in x.iter_mut().zip(&template) {
        dest.re = sample;
    }
    forward.process(&mut x);
    let power = x.iter().map(|v| v.norm_sqr()).fold(0.0, f64::max);
    if power <= 0.0 || !power.is_finite() {
        return Err("empty deconvolution template".into());
    }
    for bin in &mut x {
        *bin = bin.conj() / (bin.norm_sqr() + power * REGULARIZATION);
    }
    let mut segments: [Vec<f32>; 3] = Default::default();
    let mut y = vec![Complex::default(); fft_len];
    for (segment, &peak) in segments.iter_mut().zip(&peaks) {
        let start = (peak.round() as usize)
            .checked_sub(pre)
            .ok_or("not enough pre-arrival recording")?;
        let window = samples
            .get(start..start + window_len)
            .ok_or("not enough acoustic tail for the response")?;
        y.fill(Complex::default());
        for (dest, &sample) in y.iter_mut().zip(window) {
            dest.re = f64::from(sample);
        }
        forward.process(&mut y);
        for (bin, inverse_bin) in y.iter_mut().zip(&x) {
            *bin *= inverse_bin;
        }
        inverse.process(&mut y);
        *segment = y[..len]
            .iter()
            .map(|v| (v.re / fft_len as f64) as f32)
            .collect();
    }
    Ok(FullResponse {
        rate,
        anchor: pre,
        anchor_offsets: peaks.map(|p| p - p.round()),
        target_offset: peaks[1] - 0.5 * (peaks[0] + peaks[2]),
        segments,
    })
}

/// `Sweep::samples`, evaluated at `ratio` capture frames per renderer frame: the
/// continuous sweep the converters reproduce, with the same padding and fades.
fn analytic_template(sweep: Sweep, render_rate: u32, ratio: f64) -> Vec<f64> {
    let rate = f64::from(render_rate);
    let frames = sweep.frames(render_rate) as usize;
    let padding = (render_rate as usize * SETTLE_MS as usize / 1000) as f64;
    let sweep_frames = frames as f64 - 2.0 * padding;
    let duration = sweep_frames / rate;
    let log_ratio = (f64::from(sweep.end_hz) / f64::from(sweep.start_hz)).ln();
    let phase_scale = std::f64::consts::TAU * f64::from(sweep.start_hz) * duration / log_ratio;
    let peak = 10_f64.powf(f64::from(sweep.peak_dbfs) / 20.0);
    let fade = (rate * 0.005).round();
    (0..(frames as f64 * ratio).round() as usize)
        .map(|j| {
            // Renderer frames since the sweep body started.
            let i = j as f64 / ratio - padding;
            if i < 0.0 || i > sweep_frames - 1.0 {
                return 0.0;
            }
            let edge = i.min(sweep_frames - 1.0 - i).min(fade) / fade;
            let envelope = 0.5 - 0.5 * (std::f64::consts::PI * edge).cos();
            peak * envelope * (phase_scale * (log_ratio * i / rate / duration).exp_m1()).sin()
        })
        .collect()
}
fn analyze_inner(
    samples: &[f32],
    capture_rate: u32,
    render_rate: u32,
    sweep: Sweep,
    timing: &TimingReport,
    inspect_clipping: bool,
) -> Result<ImpulseReport, String> {
    sweep.validate(render_rate)?;
    sweep.validate(capture_rate)?;
    if samples.is_empty()
        || samples.len() > capture_rate as usize * crate::MAX_SECONDS as usize
        || samples
            .iter()
            .any(|x| !x.is_finite() || x.abs() > 16.0 || (!inspect_clipping && x.abs() >= 0.999))
    {
        return Err("invalid impulse-response input".into());
    }
    let peaks = [
        timing.reference_start_frames[0],
        timing.target_start_frame,
        timing.reference_start_frames[1],
    ];
    if peaks
        .iter()
        .any(|p| !p.is_finite() || *p < 0.0 || *p >= samples.len() as f64)
        || !(peaks[0] < peaks[1] && peaks[1] < peaks[2])
    {
        return Err("invalid sequence anchors for impulse-response analysis".into());
    }
    let source = sweep.samples(render_rate)?;
    let ratio = (peaks[2] - peaks[0]) / (2.0 * source.len() as f64);
    let nominal = capture_rate as f64 / render_rate as f64;
    if (ratio / nominal - 1.0).abs() > 0.002 {
        return Err("clock ratio exceeds the impulse-response analysis limit".into());
    }
    let rate = render_rate as f64 * ratio;
    let low = (sweep.start_hz as f64 * 1.25).max(300.0);
    let high = (sweep.end_hz as f64 * 0.9).min(9000.0);
    let split = 2500.0;
    if low > 1200.0 || high < 4500.0 {
        return Err("arrival review needs a broadband sweep starting at or below 960 Hz and ending at or above 5000 Hz; bass-only timing is unsupported".into());
    }
    let bands = [[low, high], [low, split], [split, high]];
    let template = resample(&source, ratio);
    let pre = (PRE_SECONDS * rate).round() as usize;
    let post = (POST_SECONDS * rate).round() as usize;
    let response_len = pre + post + 1;
    let window_len = template.len() + pre + post;
    let fft_len = (window_len + template.len()).next_power_of_two();
    if fft_len > MAX_FFT {
        return Err("impulse-response FFT exceeds the offline memory bound".into());
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut x = vec![Complex::default(); fft_len];
    for (dest, sample) in x.iter_mut().zip(&template) {
        dest.re = f64::from(*sample);
    }
    forward.process(&mut x);
    let power = x.iter().map(|v| v.norm_sqr()).fold(0.0, f64::max);
    if power <= 0.0 || !power.is_finite() {
        return Err("empty deconvolution template".into());
    }
    // The inverse is bounded where the finite, faded sweep carries little energy.
    for bin in &mut x {
        *bin = bin.conj() / (bin.norm_sqr() + power * REGULARIZATION);
    }
    let mut segments = Vec::with_capacity(3);
    let mut analysis_responses = Vec::with_capacity(3);
    let mut reasons = Vec::new();
    for (index, &peak) in peaks.iter().enumerate() {
        let center = peak.round() as usize;
        let start = center
            .checked_sub(pre)
            .ok_or("not enough pre-arrival recording")?;
        let end = start
            .checked_add(window_len)
            .ok_or("impulse window overflow")?;
        let window = samples
            .get(start..end)
            .ok_or("not enough acoustic tail for impulse-response analysis")?;
        let mut y = vec![Complex::default(); fft_len];
        for (dest, &sample) in y.iter_mut().zip(window) {
            dest.re = f64::from(sample);
        }
        forward.process(&mut y);
        for (bin, inverse_bin) in y.iter_mut().zip(&x) {
            *bin *= inverse_bin;
        }
        // Preserve the full excited band for frequency plots, before the arrival
        // detector's three raised-cosine bands alter its spectral shape.
        let mut full = y.clone();
        inverse.process(&mut full);
        let full_response: Vec<_> = full[..response_len]
            .iter()
            .map(|v| v.re / fft_len as f64)
            .collect();
        drop(full);
        let spectral = crate::spectral::analyze(&full_response, rate, pre, sweep);
        let mut candidates = [None; 3];
        let mut threshold_candidates = [[None; 3]; 3];
        let mut sensitivity: f64 = 0.0;
        let mut wide = Vec::new();
        let mut wide_envelope = Vec::new();
        let mut noise_db = 0.0;
        for (band_index, &band) in bands.iter().enumerate() {
            // Analytic (positive-frequency) impulse gives a polarity-independent envelope.
            let mut spectrum = vec![Complex::default(); fft_len];
            for k in 1..fft_len / 2 {
                let hz = k as f64 * rate / fft_len as f64;
                spectrum[k] = y[k] * (2.0 * band_weight(hz, band));
            }
            inverse.process(&mut spectrum);
            let response: Vec<f64> = spectrum[..response_len]
                .iter()
                .map(|v| v.re / fft_len as f64)
                .collect();
            let envelope: Vec<f64> = spectrum[..response_len]
                .iter()
                .map(|v| v.norm() / fft_len as f64)
                .collect();
            let quiet_end = (0.025 * rate) as usize;
            let noise = (envelope[..quiet_end].iter().map(|v| v * v).sum::<f64>()
                / quiet_end as f64)
                .sqrt();
            let search_lo = (0.05 * rate) as usize;
            let search_hi = (0.2 * rate) as usize;
            let maximum = envelope[search_lo..search_hi]
                .iter()
                .copied()
                .fold(0.0, f64::max);
            let strongest = noise.max(1e-15);
            let arrivals = [0.20, 0.30, 0.40].map(|fraction| {
                earliest_peak(
                    &envelope,
                    search_lo,
                    search_hi,
                    (maximum * fraction).max(strongest * 10.0),
                )
            });
            threshold_candidates[band_index] =
                arrivals.map(|a| a.map(|a| (a - pre as f64) / rate * 1000.0));
            if let [Some(a), Some(b), Some(c)] = arrivals {
                // Independent thresholds expose a weak early peak competing with a stronger echo.
                sensitivity = sensitivity.max((a.max(b).max(c) - a.min(b).min(c)) / rate * 1000.0);
                candidates[band_index] = Some((b - pre as f64) / rate * 1000.0);
            }
            if band_index == 0 {
                noise_db = 20.0 * (strongest / maximum.max(strongest)).log10();
                wide = response;
                wide_envelope = envelope;
            }
        }
        let offsets: Vec<f64> = candidates.iter().flatten().copied().collect();
        let candidate = if offsets.len() == 3 {
            let spread = offsets.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - offsets.iter().copied().fold(f64::INFINITY, f64::min);
            sensitivity = sensitivity
                .max(spread)
                .max(2.0 / (high - low) * 1000.0)
                .max(1000.0 / rate);
            if sensitivity <= MAX_BAND_SPREAD_MS {
                candidates[0]
            } else {
                None
            }
        } else {
            None
        };
        let role = ["reference_before", "speaker", "reference_after"][index];
        if candidate.is_none() {
            reasons.push(format!(
                "{role}: early peaks disagree across frequency bands or detection thresholds"
            ));
        }
        let bucket = response_len.div_ceil(1600);
        let peak_envelope = wide_envelope.iter().copied().fold(0.0, f64::max).max(1e-15);
        let envelope_db = wide_envelope
            .chunks(bucket)
            .map(|chunk| {
                (20.0
                    * (chunk.iter().copied().fold(0.0, f64::max) / peak_envelope)
                        .max(1e-4)
                        .log10()) as f32
            })
            .collect();
        let waveforms = spectral.as_ref().ok().and_then(|s| {
            crate::waveform::analyze(&full_response, rate, pre, s, candidate.or(candidates[0]))
        });
        segments.push(ImpulseSegment {
            role: role.into(),
            correlation_start_frame: peak,
            // The extraction window is anchored on the rounded sample, not peak's interpolated fraction.
            candidate_start_frame: candidate.map(|offset| center as f64 + offset / 1000.0 * rate),
            band_candidate_offset_ms: candidates,
            threshold_candidate_offset_ms: threshold_candidates,
            candidate_offset_ms: candidate,
            sensitivity_ms: (offsets.len() == 3).then_some(sensitivity),
            noise_db_relative_peak: noise_db,
            response_start_ms: -(pre as f64) / rate * 1000.0,
            response_step_ms: 1000.0 / rate,
            response: wide.iter().map(|v| *v as f32).collect(),
            envelope_db,
            envelope_step_ms: bucket as f64 / rate * 1000.0,
            spectral: spectral.as_ref().ok().cloned(),
            spectral_error: spectral.err(),
            waveforms,
        });
        analysis_responses.push(wide);
    }
    let segments: [ImpulseSegment; 3] = segments.try_into().expect("three segments");
    let mut similarity = None;
    let mut residual = None;
    let mut relative = None;
    let mut drift = None;
    let mut uncertainty = None;
    if let [Some(a), _, Some(c)] = segments.each_ref().map(|s| s.candidate_start_frame) {
        let candidate_shift = (c - peaks[2].round()) - (a - peaks[0].round());
        let (agreement, lag) = reference_agreement(
            &analysis_responses[0],
            &analysis_responses[2],
            pre,
            rate,
            candidate_shift,
        );
        similarity = Some(agreement);
        residual = Some((lag - candidate_shift) / rate * 1000.0);
        let candidate_ratio = (c - a) / (2.0 * source.len() as f64);
        let ppm = (candidate_ratio / nominal - 1.0) * 1e6;
        if agreement < 0.90 {
            reasons.push("repeated reference impulse shapes are not sufficiently similar".into());
        }
        if residual.unwrap().abs() > 0.2 {
            reasons
                .push("reference waveform alignment disagrees with the arrival candidates".into());
        }
        if (candidate_ratio / ratio - 1.0).abs() > 100e-6 || ppm.abs() > 2000.0 {
            reasons.push(
                "arrival candidates do not support the template's constant clock ratio".into(),
            );
        }
        if let Some(b) = segments[1].candidate_start_frame {
            let delay = (b - (a + c) * 0.5) / (render_rate as f64 * candidate_ratio) * 1000.0;
            if reasons.is_empty() {
                relative = Some(delay);
                drift = Some(ppm);
                uncertainty = Some(
                    segments[1].sensitivity_ms.unwrap()
                        + (segments[0].sensitivity_ms.unwrap()
                            + segments[2].sensitivity_ms.unwrap())
                            * 0.5
                        + residual.unwrap().abs(),
                );
            }
        }
    }
    Ok(ImpulseReport {
        method: "regularized-sweep-arrival-review-v2".into(),
        bands_hz: bands,
        segments,
        reference_similarity: similarity,
        reference_alignment_residual_ms: residual,
        relative_arrival_ms: relative,
        clock_drift_ppm: drift,
        uncertainty_ms: uncertainty,
        unresolved_reasons: reasons,
        valid_for_correction: false,
    })
}

fn band_weight(hz: f64, [low, high]: [f64; 2]) -> f64 {
    if hz <= low || hz >= high {
        return 0.0;
    }
    // A full raised-cosine band avoids the strong pre-ringing sidelobes of
    // a flat passband, which otherwise resemble earlier arrival candidates.
    let fade = (high - low) * 0.5;
    let edge = ((hz - low).min(high - hz) / fade).min(1.0);
    0.5 - 0.5 * (std::f64::consts::PI * edge).cos()
}
fn earliest_peak(values: &[f64], start: usize, end: usize, threshold: f64) -> Option<f64> {
    (start + 1..end - 1)
        .find(|&i| {
            values[i] >= threshold && values[i] > values[i - 1] && values[i] >= values[i + 1]
        })
        .map(|i| {
            let denom = values[i - 1] - 2.0 * values[i] + values[i + 1];
            i as f64
                + if denom.abs() > 1e-15 {
                    (0.5 * (values[i - 1] - values[i + 1]) / denom).clamp(-0.5, 0.5)
                } else {
                    0.0
                }
        })
}
fn reference_agreement(
    a: &[f64],
    b: &[f64],
    anchor: usize,
    rate: f64,
    expected_shift: f64,
) -> (f64, f64) {
    let search = (rate * 0.001).ceil() as isize;
    let lo = anchor - (rate * 0.04) as usize;
    let hi = anchor + (rate * 0.06) as usize;
    let mut best = (0.0, 0.0);
    let center = expected_shift.round() as isize;
    for lag in center - search..=center + search {
        let mut xy = 0.0;
        let mut xx = 0.0;
        let mut yy = 0.0;
        for (i, &x) in a.iter().enumerate().take(hi).skip(lo) {
            if let Some(j) = i.checked_add_signed(lag).filter(|&j| j < b.len()) {
                let y = b[j];
                xy += x * y;
                xx += x * x;
                yy += y * y;
            }
        }
        let score = xy / (xx * yy).sqrt().max(1e-30);
        if score > best.0 {
            best = (score, lag as f64);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing;
    fn sweep() -> Sweep {
        Sweep {
            duration_ms: 1000,
            peak_dbfs: -24.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        }
    }
    // Independent continuous-time generator, including fractional capture positions.
    fn recording(rate: u32, ppm: f64, delay_ms: f64, paths: [&[(f64, f64)]; 3]) -> Vec<f32> {
        let spec = sweep();
        let ratio = 1.0 + ppm * 1e-6;
        let mut out = vec![0.0; (rate as f64 * 5.7) as usize];
        for (segment, paths) in paths.iter().enumerate() {
            for &(path_ms, gain) in *paths {
                for (i, dst) in out.iter_mut().enumerate() {
                    let t = i as f64 / rate as f64 / ratio
                        - 0.5
                        - segment as f64 * 1.5
                        - 0.25
                        - path_ms / 1000.0
                        - if segment == 1 { delay_ms / 1000.0 } else { 0.0 };
                    if !(0.0..1.0).contains(&t) {
                        continue;
                    }
                    let log = (spec.end_hz as f64 / spec.start_hz as f64).ln();
                    let phase =
                        std::f64::consts::TAU * spec.start_hz as f64 / log * (log * t).exp_m1();
                    let edge = t.min(1.0 - t).min(0.005) / 0.005;
                    *dst += (10_f64.powf(spec.peak_dbfs as f64 / 20.0)
                        * gain
                        * 0.5
                        * (1.0 - (std::f64::consts::PI * edge).cos())
                        * phase.sin()) as f32;
                }
            }
        }
        let mut state = 123_u32;
        for value in &mut out {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            *value += ((state as f64 / u32::MAX as f64 - 0.5) * 1e-5) as f32;
        }
        out
    }
    fn review(samples: &[f32], rate: u32) -> ImpulseReport {
        let timing = timing::analyze(samples, rate, 48000, sweep()).unwrap();
        analyze(samples, rate, 48000, sweep(), &timing).unwrap()
    }
    #[test]
    fn fractional_arrivals_survive_polarity_rate_and_clock_changes() {
        for (rate, ppm, delay) in [
            (48000, 0.0, 7.35),
            (44100, 300.0, -3.7),
            (48000, -250.0, 12.1),
            (48000, 0.0, 160.0),
            (44100, 300.0, -160.0),
        ] {
            let samples = recording(
                rate,
                ppm,
                delay,
                [&[(0.0, 0.8)], &[(0.0, -0.6)], &[(0.0, 0.8)]],
            );
            let report = review(&samples, rate);
            assert!(
                report.unresolved_reasons.is_empty(),
                "{:?}",
                report.unresolved_reasons
            );
            assert!(
                (report.relative_arrival_ms.unwrap() - delay).abs() < 0.08,
                "{}",
                report.relative_arrival_ms.unwrap()
            );
            assert!((report.clock_drift_ppm.unwrap() - ppm).abs() < 30.0);
            assert!(report.reference_similarity.unwrap() > 0.95);
            assert!(!report.valid_for_correction);
            assert!(
                report.segments.iter().all(
                    |s| s.envelope_db.len() <= 1600 && s.response.iter().all(|x| x.is_finite())
                )
            );
        }
    }
    #[test]
    fn the_unweighted_response_recovers_a_known_room_response() {
        // A direct path and a 2 ms echo at half its level, on every segment.
        let paths = &[(0.0, 0.8), (2.0, 0.4)][..];
        let samples = recording(48000, 120.0, 3.1, [paths; 3]);
        let timing = timing::analyze(&samples, 48000, 48000, sweep()).unwrap();
        let full = full_response(&samples, 48000, 48000, sweep(), &timing).unwrap();
        assert!((full.rate / 48000.0 - 1.0 - 120e-6).abs() < 30e-6);
        assert_eq!(full.anchor, (PRE_SECONDS * full.rate).round() as usize);
        for segment in &full.segments {
            assert_eq!(
                segment.len(),
                full.anchor + (FULL_POST_SECONDS * full.rate).round() as usize + 1
            );
            // The direct path is at the anchor, its echo 2 ms later.
            let strongest = |range: std::ops::Range<usize>| {
                range
                    .max_by(|&a, &b| segment[a].abs().total_cmp(&segment[b].abs()))
                    .unwrap()
            };
            let direct = strongest(full.anchor - 48..full.anchor + 48);
            let echo = strongest(direct + 48..direct + 144);
            assert!(direct.abs_diff(full.anchor) <= 2, "{direct}");
            assert!((echo - direct).abs_diff(96) <= 1, "{}", echo - direct);
            assert!((segment[echo] / segment[direct] - 0.5).abs() < 0.08);
        }
        let magnitude = crate::response::magnitude(&full, sweep(), None).unwrap();
        for (i, &hz) in magnitude.frequency_hz.iter().enumerate() {
            if !(300.0..=9000.0).contains(&hz) {
                continue;
            }
            // The same 1/48-octave power average of |0.8 + 0.4·e^(−jω·2 ms)|².
            let half = 2_f64.powf(0.5 / crate::response::POINTS_PER_OCTAVE);
            let power = (0..=64)
                .map(|k| {
                    let f = hz / half * half.powf(k as f64 / 32.0);
                    0.8 * 0.8 + 0.4 * 0.4 + 0.64 * (std::f64::consts::TAU * f * 0.002).cos()
                })
                .sum::<f64>()
                / 65.0;
            let expected = 10.0 * power.log10();
            let tolerance = if expected < -3.0 { 0.6 } else { 0.08 };
            for segment in &magnitude.magnitude_db {
                let db = f64::from(segment[i]);
                assert!(
                    (db - expected).abs() < tolerance,
                    "{hz} Hz: {db} vs {expected}"
                );
            }
        }
    }
    #[test]
    fn the_analytic_template_is_the_played_sweep() {
        for sweep in [
            sweep(),
            Sweep {
                duration_ms: 3000,
                peak_dbfs: -12.0,
                start_hz: 20.0,
                end_hz: 20000.0,
            },
        ] {
            let played = sweep.samples(48000).unwrap();
            let template = analytic_template(sweep, 48000, 1.0);
            assert_eq!(template.len(), played.len());
            for (a, b) in template.iter().zip(&played) {
                assert!((a - f64::from(*b)).abs() < 1e-6);
            }
        }
    }
    #[test]
    fn early_candidate_is_not_replaced_by_a_stronger_late_reflection() {
        let paths = &[(0.0, 0.55), (12.0, 1.0)][..];
        let samples = recording(48000, 0.0, 4.25, [paths; 3]);
        let report = review(&samples, 48000);
        assert!(
            report.unresolved_reasons.is_empty(),
            "{:?}",
            report.unresolved_reasons
        );
        assert!((report.relative_arrival_ms.unwrap() - 4.25).abs() < 0.08);
        for segment in &report.segments {
            assert!((segment.candidate_offset_ms.unwrap() + 12.0).abs() < 0.1);
        }
    }
    #[test]
    fn threshold_sensitive_weak_arrivals_are_withheld() {
        let paths = &[(0.0, 0.3), (12.0, 1.0)][..];
        let samples = recording(48000, 0.0, 4.25, [paths; 3]);
        let report = review(&samples, 48000);
        assert!(report.relative_arrival_ms.is_none());
        assert!(report.clock_drift_ppm.is_none());
        assert!(report.uncertainty_ms.is_none());
        assert!(!report.unresolved_reasons.is_empty());
    }
    #[test]
    fn changed_reference_shape_does_not_become_a_delay() {
        let first = &[(0.0, 0.8), (8.0, 0.6)][..];
        let last = &[(0.0, 1.0)][..];
        let samples = recording(48000, 0.0, 3.0, [first, first, last]);
        let report = review(&samples, 48000);
        assert!(report.relative_arrival_ms.is_none());
        assert!(
            report
                .unresolved_reasons
                .iter()
                .any(|s| s.contains("reference impulse shapes"))
        );
    }
    #[test]
    fn a_reference_polarity_change_is_not_repeatability() {
        let samples = recording(
            48000,
            0.0,
            0.0,
            [&[(0.0, 0.7)], &[(0.0, -0.7)], &[(0.0, -0.7)]],
        );
        let report = review(&samples, 48000);
        assert!(report.relative_arrival_ms.is_none());
        assert!(report.reference_similarity.is_some_and(|v| v < 0.90));
    }

    #[test]
    fn frequency_dependent_arrivals_are_not_flattened_into_one_delay() {
        let dry = recording(48000, 0.0, 2.0, [&[(0.0, 0.7)]; 3]);
        let alpha = 1.0 - (-std::f64::consts::TAU * 1500.0 / 48000.0).exp();
        let mut low = 0.0;
        let mut filtered = vec![0.0; dry.len()];
        for (i, &sample) in dry.iter().enumerate() {
            low += alpha * (f64::from(sample) - low);
            filtered[i] += low as f32;
            if i + 144 < filtered.len() {
                filtered[i + 144] += sample - low as f32;
            }
        }
        let report = review(&filtered, 48000);
        assert!(report.relative_arrival_ms.is_none());
        assert!(
            report
                .unresolved_reasons
                .iter()
                .any(|reason| reason.contains("frequency bands"))
        );
        assert!(
            report
                .segments
                .iter()
                .any(|s| s.sensitivity_ms.is_some_and(|ms| ms > 1.0))
        );
    }

    #[test]
    fn invalid_anchors_clipping_truncation_and_bass_only_are_refused() {
        let mut samples = recording(48000, 0.0, 0.0, [&[(0.0, 0.7)]; 3]);
        let mut timing = timing::analyze(&samples, 48000, 48000, sweep()).unwrap();
        let mut bass = sweep();
        bass.start_hz = 20.0;
        bass.end_hz = 100.0;
        assert!(analyze(&samples, 48000, 48000, bass, &timing).is_err());
        assert!(analyze(&samples[..220000], 48000, 48000, sweep(), &timing).is_err());
        samples[0] = 1.0;
        assert!(analyze(&samples, 48000, 48000, sweep(), &timing).is_err());
        samples[0] = 0.0;
        timing.target_start_frame = f64::NAN;
        assert!(analyze(&samples, 48000, 48000, sweep(), &timing).is_err());
    }
}
