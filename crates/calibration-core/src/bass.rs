//! Integrating a subwoofer with the speakers around their crossover.
//!
//! Omniphony's bass management splits every source into bands before panning: a band
//! below a speaker's `freq_low` goes to the outputs whose range covers it, usually the
//! subwoofer. Around that crossover the speaker and the subwoofer play together, so their
//! arrival times, polarities and levels decide whether they add up or cancel.
//!
//! Each speaker is measured three times with the renderer's bass-managed sweep (a source
//! at the speaker's position, split and panned as programme audio is): with the subwoofer
//! silenced (the speaker's part), with every other output silenced (the subwoofer's part)
//! and with everything playing (the sum), at the main seat and the positions around it.
//! The direct sweeps of a reference speaker frame each sweep, so the three share one time
//! base at each position: their phases can be compared and added.
//!
//! From those parts this module estimates, per speaker, how much later the subwoofer
//! arrives (from the slope of their phase difference around the crossover) and the
//! subwoofer delay and polarity that make them add up best; then one setting for every
//! speaker the subwoofer serves: the delay and polarity that maximise their summed level
//! over half to twice each crossover, averaged over the positions (main seat counting
//! more), and the subwoofer level that makes each sum as loud half an octave below its
//! crossover as half an octave above. The measured sum checks that the parts add up as
//! measured. A verification compares a sum measured after applying with the prediction.
//!
//! Pure and offline: no file, device or network access, never in an audio callback.
use crate::{
    Error, Result, require,
    room_correction::{
        grid,
        spectrum::{decibels, smooth},
    },
};
use serde::{Deserialize, Serialize};
use std::f64::consts::{PI, TAU};

pub const METHOD: &str = "bass-managed-parts-on-reference-time-base-v1";
/// The window rises over this span from its start and falls over the last
/// [`FALL_SECONDS`] of the time the responses share.
const RISE_SECONDS: f64 = 0.005;
const FALL_SECONDS: f64 = 0.05;
/// Every response must start at least this long before its own anchor, inside the
/// shared window: otherwise the window cuts its arrival.
const LEAD_SECONDS: f64 = 0.02;
/// The shortest shared window: 4 Hz resolution.
const MIN_WINDOW_SECONDS: f64 = 0.25;
/// The coarse step of the delay search; the best candidates are refined to
/// [`Settings::delay_step_ms`].
const COARSE_STEP_MS: f64 = 0.25;
/// Candidates within this of the best sum are equivalent: the one closest to the timing
/// the phase difference gives (the least extra delay) wins.
const TIE_DB: f64 = 0.05;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The main seat's weight when averaging positions; every other position weighs 1.
    pub main_seat_weight: f64,
    /// Smoothing of the summed power before it is compared, in octaves.
    pub smoothing_octaves: f64,
    /// The subwoofer level moves by at most this much.
    pub max_gain_change_db: f64,
    /// The resolution of the proposed delay.
    pub delay_step_ms: f64,
    /// The delay search covers this many periods of the lowest crossover around the
    /// timing the phase difference gives.
    pub search_periods: f64,
    /// A speaker whose low band the subwoofer plays less than this much of (its part
    /// relative to the speaker's own part, half an octave below the crossover) is left out.
    pub min_share_db: f64,
    /// The measured sum and the sum of the parts may differ by this much (RMS over the
    /// band) before a warning, and by the second value before the speaker is left out.
    pub consistency_db: [f64; 2],
    /// A speaker whose sum at the shared setting is this much lower than at its own
    /// best setting is named in a warning.
    pub compromise_warning_db: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            main_seat_weight: 2.0,
            smoothing_octaves: 1.0 / 12.0,
            max_gain_change_db: 12.0,
            delay_step_ms: 0.02,
            search_periods: 1.5,
            min_share_db: -6.0,
            consistency_db: [1.5, 3.0],
            compromise_warning_db: 1.0,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        require(
            [
                self.main_seat_weight,
                self.smoothing_octaves,
                self.max_gain_change_db,
                self.delay_step_ms,
                self.search_periods,
                self.min_share_db,
                self.consistency_db[0],
                self.consistency_db[1],
                self.compromise_warning_db,
            ]
            .iter()
            .all(|v| v.is_finite()),
            "bass settings must be finite",
        )?;
        require(
            (1.0..=10.0).contains(&self.main_seat_weight),
            "main-seat weight must be 1–10",
        )?;
        require(
            (1.0 / 48.0..=1.0 / 3.0).contains(&self.smoothing_octaves),
            "smoothing must be 1/48–1/3 octave",
        )?;
        require(
            (0.0..=24.0).contains(&self.max_gain_change_db),
            "level change limit must be 0–24 dB",
        )?;
        require(
            (0.005..=1.0).contains(&self.delay_step_ms),
            "delay step must be 0.005–1 ms",
        )?;
        require(
            (0.5..=4.0).contains(&self.search_periods),
            "delay search must cover 0.5–4 periods",
        )?;
        require(
            self.consistency_db[0] > 0.0 && self.consistency_db[0] <= self.consistency_db[1],
            "consistency limits must be positive and ascending",
        )
    }
}

/// One stored response on the time base of its position: zero is the reference
/// speaker's arrival, which every response measured at that position shares.
#[derive(Debug, Clone)]
pub struct Response {
    /// Frames per second, after clock compensation.
    pub rate: f64,
    pub samples: Vec<f32>,
    /// When `samples[0]` was, in seconds on the position's time base.
    pub start_s: f64,
    /// When its correlation anchor was: its sound arrives around it.
    pub anchor_s: f64,
}
impl Response {
    fn end_s(&self) -> f64 {
        self.start_s + (self.samples.len().saturating_sub(1)) as f64 / self.rate
    }
    fn validate(&self) -> Result<()> {
        require(
            self.rate.is_finite() && (8000.0..=400_000.0).contains(&self.rate),
            "response rate must be 8–400 kHz",
        )?;
        require(
            self.start_s.is_finite()
                && self.anchor_s.is_finite()
                && self.samples.len() >= 2
                && self.samples.iter().all(|v| v.is_finite())
                && (self.start_s..=self.end_s()).contains(&self.anchor_s),
            "response must be finite with its anchor inside",
        )
    }
}

/// A speaker's three bass-managed responses at one microphone position.
#[derive(Debug, Clone)]
pub struct PositionResponses {
    pub position_id: String,
    pub main_seat: bool,
    /// With the subwoofer silenced: the speaker and any other output its low band reaches.
    pub speaker: Response,
    /// With every other output silenced: the subwoofer's part. `None` when it could not
    /// be measured apart (the timing reference keeps playing its own sweep): the sum
    /// minus the speaker's part stands for it.
    pub subwoofer: Option<Response>,
    /// With everything playing.
    pub sum: Response,
}

/// Everything measured for one speaker the subwoofer may serve.
#[derive(Debug, Clone)]
pub struct SpeakerResponses {
    pub output_id: String,
    /// Where bass management splits this speaker: its declared low limit.
    pub crossover_hz: f64,
    /// The swept range.
    pub swept_hz: [f64; 2],
    pub positions: Vec<PositionResponses>,
}

/// The subwoofer's settings as measured and how far its delay can move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subwoofer {
    pub output_id: String,
    pub delay_ms: f64,
    pub gain_db: f64,
    /// The latest delay among every other output: the renderer's limit minus it is how
    /// much delay can be added to all of them when the subwoofer would need less than
    /// none.
    pub others_max_delay_ms: f64,
    pub max_delay_ms: f64,
}

/// Taps the subwoofer will play through (its mode correction), at their rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub sample_rate: f64,
    pub taps: Vec<f32>,
}

/// A change of the subwoofer relative to every other output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    /// How much later the subwoofer plays relative to the other outputs.
    pub delay_change_ms: f64,
    /// Its polarity inverted.
    pub inverted: bool,
    pub gain_change_db: f64,
}
impl Setting {
    pub const UNCHANGED: Self = Self {
        delay_change_ms: 0.0,
        inverted: false,
        gain_change_db: 0.0,
    };
}

/// The best delay and polarity for one position on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionBest {
    pub position_id: String,
    pub delay_change_ms: f64,
    pub inverted: bool,
}

/// Spatially averaged curves over a speaker's band, 1/48-octave grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    pub frequency_hz: Vec<f64>,
    pub speaker_db: Vec<f32>,
    pub subwoofer_db: Vec<f32>,
    /// The sum as measured.
    pub measured_sum_db: Vec<f32>,
    /// The parts added with the proposal (delay, polarity, level, mode correction).
    pub predicted_sum_db: Vec<f32>,
    /// What they would give adding perfectly in phase, at the proposed level.
    pub coherent_db: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerResult {
    pub output_id: String,
    pub crossover_hz: f64,
    /// Where the sums are compared: half to twice the crossover, within the sweep.
    pub band_hz: Option<[f64; 2]>,
    pub positions: usize,
    /// Why the shared setting does not count this speaker.
    pub excluded: Option<String>,
    /// How much later the subwoofer arrives than the speaker, from the slope of their
    /// phase difference over the octave around the crossover (weighted mean of the
    /// positions), and whether their phases are opposed once that delay is taken out.
    pub phase_delay_ms: Option<f64>,
    pub phase_inverted: Option<bool>,
    /// RMS of the phase difference around its straight-line fit, in degrees: how far
    /// it is from a pure delay.
    pub phase_fit_deg: Option<f64>,
    /// How loud the subwoofer's part is against the speaker's own part half an octave
    /// below the crossover.
    pub subwoofer_share_db: Option<f64>,
    /// RMS difference between the measured sum and the parts added, over the band.
    pub consistency_db: Option<f64>,
    /// This speaker's own best delay and polarity, and the level it asks for.
    pub best: Option<Setting>,
    /// Per position, the best delay and polarity of that position alone.
    pub position_best: Vec<PositionBest>,
    /// How far the sum falls below the parts adding perfectly in phase (mean over the
    /// band, 0 dB is perfect), as measured, at its own best and with the proposal.
    pub summation_db: Option<[f64; 3]>,
    /// The sum's mean level over the band, as measured and with the proposal (dB, the
    /// spatial average): what the proposal gains here.
    pub sum_level_db: Option<[f64; 2]>,
    /// How much louder the sum is half an octave below the crossover than half an octave
    /// above, as measured and with the proposal.
    pub balance_db: Option<[f64; 2]>,
    pub warnings: Vec<String>,
    pub curves: Option<Curves>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub method: String,
    pub subwoofer: Subwoofer,
    /// The shared setting, relative to the measured state.
    pub setting: Setting,
    /// The subwoofer's absolute delay and gain with the proposal.
    pub subwoofer_delay_ms: f64,
    pub subwoofer_gain_db: f64,
    /// Delay added to every other output when the subwoofer would need less than none.
    pub others_delay_ms: f64,
    /// Whether the mode correction is part of the prediction.
    pub mode_correction: bool,
    pub speakers: Vec<SpeakerResult>,
    pub warnings: Vec<String>,
    /// Why nothing is proposed.
    pub refusal: Option<String>,
}
impl Proposal {
    /// Speakers the shared setting counts.
    pub fn included(&self) -> impl Iterator<Item = &SpeakerResult> {
        self.speakers.iter().filter(|s| s.excluded.is_none())
    }
}

/// One speaker's sum measured again after applying the proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerCheck {
    pub output_id: String,
    pub position_id: String,
    /// Mean level of the sum over the band: measured before (the analysis at that
    /// position), predicted, and measured now.
    pub sum_level_db: [f64; 3],
    /// How much louder the sum is half an octave below the crossover than half an
    /// octave above: measured before, predicted, and measured now.
    pub balance_db: [f64; 3],
    /// RMS difference between the predicted and the measured sum over the band.
    pub deviation_db: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    pub speakers: Vec<SpeakerCheck>,
    pub warnings: Vec<String>,
}

/// A speaker's responses at one position as spectra on its grid, on the shared window.
struct Parts {
    weight: f64,
    position_id: String,
    speaker: Vec<(f64, f64)>,
    subwoofer: Vec<(f64, f64)>,
    sum: Vec<(f64, f64)>,
    measured_apart: bool,
    window: [f64; 2],
}

/// A speaker ready to evaluate: its grid, bands and parts at every usable position.
struct Prepared {
    crossover_hz: f64,
    frequencies: Vec<f64>,
    /// Indices of `frequencies` inside the compared band, below and above the crossover.
    band: Vec<usize>,
    low: Vec<usize>,
    high: Vec<usize>,
    band_hz: [f64; 2],
    parts: Vec<Parts>,
    /// The subwoofer filter's response on `frequencies`.
    filter: Vec<(f64, f64)>,
}

fn mul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}
fn add(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 + b.0, a.1 + b.1)
}
fn norm_sqr(a: (f64, f64)) -> f64 {
    a.0 * a.0 + a.1 * a.1
}
fn round(value: f64, decimals: i32) -> f64 {
    let scale = 10_f64.powi(decimals);
    (value * scale).round() / scale
}

/// The window over the shared time `[from, to]`: a raised-cosine rise, flat, then a
/// raised-cosine fall over the last [`FALL_SECONDS`].
fn window(t: f64, [from, to]: [f64; 2]) -> f64 {
    if t < from || t > to {
        0.0
    } else if t < from + RISE_SECONDS {
        0.5 - 0.5 * (PI * (t - from) / RISE_SECONDS).cos()
    } else if t > to - FALL_SECONDS {
        0.5 - 0.5 * (PI * (to - t) / FALL_SECONDS).cos()
    } else {
        1.0
    }
}

/// The spectrum of `response` at each of `frequencies`, windowed over `span` on the
/// shared time base: `Σ w(t) x(t) e^{−j2πft}`, so responses of one position keep their
/// relative phase.
fn spectrum(response: &Response, frequencies: &[f64], span: [f64; 2]) -> Vec<(f64, f64)> {
    let first = (((span[0] - response.start_s) * response.rate)
        .floor()
        .max(0.0)) as usize;
    let last = ((((span[1] - response.start_s) * response.rate).ceil()) as usize)
        .min(response.samples.len() - 1);
    let weighted: Vec<(f64, f64)> = (first..=last)
        .map(|k| {
            let t = response.start_s + k as f64 / response.rate;
            (t, f64::from(response.samples[k]) * window(t, span))
        })
        .collect();
    frequencies
        .iter()
        .map(|&hz| {
            let omega = TAU * hz;
            let Some(&(t0, _)) = weighted.first() else {
                return (0.0, 0.0);
            };
            let step = -omega / response.rate;
            let (step_cos, step_sin) = (step.cos(), step.sin());
            let mut phasor = ((-omega * t0).cos(), (-omega * t0).sin());
            let (mut re, mut im) = (0.0, 0.0);
            for (i, &(_, value)) in weighted.iter().enumerate() {
                re += value * phasor.0;
                im += value * phasor.1;
                phasor = (
                    phasor.0 * step_cos - phasor.1 * step_sin,
                    phasor.0 * step_sin + phasor.1 * step_cos,
                );
                if i % 1024 == 1023 {
                    let norm = phasor.0.hypot(phasor.1);
                    phasor = (phasor.0 / norm, phasor.1 / norm);
                }
            }
            (re, im)
        })
        .collect()
}

/// The frequency response of `filter` at each of `frequencies` (identity without one).
fn filter_response(filter: Option<&Filter>, frequencies: &[f64]) -> Vec<(f64, f64)> {
    let Some(filter) = filter else {
        return vec![(1.0, 0.0); frequencies.len()];
    };
    frequencies
        .iter()
        .map(|&hz| {
            let step = -TAU * hz / filter.sample_rate;
            let (step_cos, step_sin) = (step.cos(), step.sin());
            let mut phasor = (1.0, 0.0);
            let (mut re, mut im) = (0.0, 0.0);
            for (i, &tap) in filter.taps.iter().enumerate() {
                re += f64::from(tap) * phasor.0;
                im += f64::from(tap) * phasor.1;
                phasor = (
                    phasor.0 * step_cos - phasor.1 * step_sin,
                    phasor.0 * step_sin + phasor.1 * step_cos,
                );
                if i % 1024 == 1023 {
                    let norm = phasor.0.hypot(phasor.1);
                    phasor = (phasor.0 / norm, phasor.1 / norm);
                }
            }
            (re, im)
        })
        .collect()
}

impl Prepared {
    /// `extra`, a response measured later at the (one) position, shares the window.
    fn new(
        speaker: &SpeakerResponses,
        filter: Option<&Filter>,
        extra: Option<&Response>,
        settings: &Settings,
    ) -> std::result::Result<(Self, Vec<String>), String> {
        let fc = speaker.crossover_hz;
        let [swept_low, swept_high] = speaker.swept_hz;
        // Spectra a little past the compared band, so smoothing has neighbours there.
        let margin = 2_f64.powf(settings.smoothing_octaves);
        let frequencies = grid(
            (fc / 2.0 / margin).max(swept_low),
            (fc * 2.0 * margin).min(swept_high),
        );
        let band_hz = [
            (fc / 2.0).max(swept_low * 2_f64.powf(1.0 / 24.0)),
            (fc * 2.0).min(swept_high / 2_f64.powf(1.0 / 24.0)),
        ];
        if band_hz[0] > fc / 2_f64.sqrt() || band_hz[1] < fc * 2_f64.sqrt() {
            return Err(format!(
                "The sweep ({:.0}–{:.0} Hz) does not cover the octave around its {fc:.0} Hz crossover.",
                swept_low, swept_high
            ));
        }
        let pick = |range: [f64; 2]| -> Vec<usize> {
            (0..frequencies.len())
                .filter(|&k| (range[0]..=range[1]).contains(&frequencies[k]))
                .collect()
        };
        let band = pick(band_hz);
        let low = pick([band_hz[0], fc / 2_f64.sqrt()]);
        let high = pick([fc * 2_f64.sqrt(), band_hz[1]]);
        if low.len() < 4 || high.len() < 4 {
            return Err(format!(
                "The sweep ({swept_low:.0}–{swept_high:.0} Hz) reaches too little beyond the octave around its {fc:.0} Hz crossover."
            ));
        }
        let mut warnings = vec![];
        let mut parts = vec![];
        for p in &speaker.positions {
            let mut responses = vec![&p.speaker, &p.sum];
            responses.extend(p.subwoofer.as_ref());
            responses.extend(extra);
            let from = responses
                .iter()
                .map(|r| r.start_s)
                .fold(f64::NEG_INFINITY, f64::max);
            let to = responses
                .iter()
                .map(|r| r.end_s())
                .fold(f64::INFINITY, f64::min);
            if responses.iter().any(|r| r.anchor_s - from < LEAD_SECONDS)
                || to - from < MIN_WINDOW_SECONDS
            {
                warnings.push(format!(
                    "{}: its parts arrive too far apart to share a window; left out.",
                    p.position_id
                ));
                continue;
            }
            let span = [from, to];
            let spk = spectrum(&p.speaker, &frequencies, span);
            let sum = spectrum(&p.sum, &frequencies, span);
            let (sub, measured_apart) = match &p.subwoofer {
                Some(r) => (spectrum(r, &frequencies, span), true),
                None => (
                    sum.iter()
                        .zip(&spk)
                        .map(|(s, a)| (s.0 - a.0, s.1 - a.1))
                        .collect(),
                    false,
                ),
            };
            parts.push(Parts {
                weight: if p.main_seat {
                    settings.main_seat_weight
                } else {
                    1.0
                },
                position_id: p.position_id.clone(),
                speaker: spk,
                subwoofer: sub,
                sum,
                measured_apart,
                window: span,
            });
        }
        if parts.is_empty() {
            return Err("No position with usable parts.".into());
        }
        let filter = filter_response(filter, &frequencies);
        Ok((
            Self {
                crossover_hz: fc,
                frequencies,
                band,
                low,
                high,
                band_hz,
                parts,
                filter,
            },
            warnings,
        ))
    }

    fn total_weight(&self) -> f64 {
        self.parts.iter().map(|p| p.weight).sum()
    }

    /// The predicted sum's power at one position, smoothed.
    fn sum_power(
        &self,
        parts: &Parts,
        setting: &Setting,
        filtered: bool,
        settings: &Settings,
    ) -> Vec<f64> {
        let gain =
            10_f64.powf(setting.gain_change_db / 20.0) * if setting.inverted { -1.0 } else { 1.0 };
        let delay = setting.delay_change_ms / 1000.0;
        let power: Vec<f64> = (0..self.frequencies.len())
            .map(|k| {
                let angle = -TAU * self.frequencies[k] * delay;
                let mut sub = mul(parts.subwoofer[k], (gain * angle.cos(), gain * angle.sin()));
                if filtered {
                    sub = mul(sub, self.filter[k]);
                }
                norm_sqr(add(parts.speaker[k], sub))
            })
            .collect();
        smooth(&power, &self.frequencies, |_| settings.smoothing_octaves)
    }

    /// Smoothed power of a spectrum.
    fn power(&self, values: &[(f64, f64)], settings: &Settings) -> Vec<f64> {
        let power: Vec<f64> = values.iter().map(|&v| norm_sqr(v)).collect();
        smooth(&power, &self.frequencies, |_| settings.smoothing_octaves)
    }

    /// The parts adding perfectly in phase at one position, smoothed.
    fn coherent_power(
        &self,
        parts: &Parts,
        setting: &Setting,
        filtered: bool,
        settings: &Settings,
    ) -> Vec<f64> {
        let gain = 10_f64.powf(setting.gain_change_db / 20.0);
        let power: Vec<f64> = (0..self.frequencies.len())
            .map(|k| {
                let filter = if filtered {
                    norm_sqr(self.filter[k]).sqrt()
                } else {
                    1.0
                };
                (norm_sqr(parts.speaker[k]).sqrt()
                    + gain * filter * norm_sqr(parts.subwoofer[k]).sqrt())
                .powi(2)
            })
            .collect();
        smooth(&power, &self.frequencies, |_| settings.smoothing_octaves)
    }

    /// Mean dB of `power` over `indices`.
    fn mean_db(&self, power: &[f64], indices: &[usize]) -> f64 {
        indices.iter().map(|&k| decibels(power[k])).sum::<f64>() / indices.len().max(1) as f64
    }

    /// The sum's mean level over the band, weighted over the positions (or one).
    fn level(&self, setting: &Setting, filtered: bool, only: Option<usize>, s: &Settings) -> f64 {
        let mut total = 0.0;
        let mut weights = 0.0;
        for (i, parts) in self.parts.iter().enumerate() {
            if only.is_some_and(|o| o != i) {
                continue;
            }
            let power = self.sum_power(parts, setting, filtered, s);
            total += parts.weight * self.mean_db(&power, &self.band);
            weights += parts.weight;
        }
        total / weights.max(f64::MIN_POSITIVE)
    }

    /// How much louder the sum is half an octave below the crossover than half an octave
    /// above, from the positions' weighted mean power.
    fn balance(&self, setting: &Setting, filtered: bool, s: &Settings) -> f64 {
        let n = self.frequencies.len();
        let mut average = vec![0.0; n];
        for parts in &self.parts {
            let power = self.sum_power(parts, setting, filtered, s);
            for (a, p) in average.iter_mut().zip(&power) {
                *a += parts.weight * p;
            }
        }
        self.mean_db(&average, &self.low) - self.mean_db(&average, &self.high)
    }

    /// The parts' summation loss (dB below perfectly in phase), weighted over positions.
    fn summation(&self, setting: &Setting, filtered: bool, s: &Settings) -> f64 {
        let mut total = 0.0;
        for parts in &self.parts {
            let sum = self.sum_power(parts, setting, filtered, s);
            let coherent = self.coherent_power(parts, setting, filtered, s);
            let loss: f64 = self
                .band
                .iter()
                .map(|&k| decibels(sum[k]) - decibels(coherent[k]))
                .sum::<f64>()
                / self.band.len() as f64;
            total += parts.weight * loss;
        }
        total / self.total_weight()
    }

    /// The level change that balances the sum below and above the crossover with
    /// `setting`'s delay and polarity, within the limit.
    fn balancing_gain(&self, setting: &Setting, filtered: bool, s: &Settings) -> (f64, bool) {
        let at = |gain: f64| {
            self.balance(
                &Setting {
                    gain_change_db: gain,
                    ..*setting
                },
                filtered,
                s,
            )
        };
        let (mut low, mut high) = (-s.max_gain_change_db, s.max_gain_change_db);
        let (below, above) = (at(low), at(high));
        if below >= 0.0 {
            return (low, false);
        }
        if above <= 0.0 {
            return (high, false);
        }
        for _ in 0..40 {
            let middle = 0.5 * (low + high);
            if at(middle) < 0.0 {
                low = middle;
            } else {
                high = middle;
            }
        }
        (0.5 * (low + high), true)
    }

    /// Per position, the slope and offset of the phase difference between the
    /// subwoofer's part and the speaker's over the octave around the crossover: the
    /// subwoofer's lateness (s), its phase offset (rad) and the fit's RMS residual (rad).
    fn phase_fit(&self, parts: &Parts, settings: &Settings) -> Option<(f64, f64, f64)> {
        let range = [
            self.crossover_hz / 2_f64.sqrt(),
            self.crossover_hz * 2_f64.sqrt(),
        ];
        let indices: Vec<usize> = (0..self.frequencies.len())
            .filter(|&k| (range[0]..=range[1]).contains(&self.frequencies[k]))
            .collect();
        if indices.len() < 8 {
            return None;
        }
        // The cross-spectrum, smoothed as a complex value so modes average out.
        let cross: Vec<(f64, f64)> = (0..self.frequencies.len())
            .map(|k| {
                let a = parts.speaker[k];
                mul(parts.subwoofer[k], (a.0, -a.1))
            })
            .collect();
        let half = (settings.smoothing_octaves * crate::room_correction::POINTS_PER_OCTAVE / 2.0)
            .round() as usize;
        let smoothed: Vec<(f64, f64)> = (0..cross.len())
            .map(|i| {
                cross[i.saturating_sub(half)..(i + half + 1).min(cross.len())]
                    .iter()
                    .fold((0.0, 0.0), |acc, &v| add(acc, v))
            })
            .collect();
        // Unwrapped from the crossover outward.
        let centre = indices[indices.len() / 2];
        let mut phase = vec![0.0; self.frequencies.len()];
        phase[centre] = smoothed[centre].1.atan2(smoothed[centre].0);
        for k in centre + 1..=*indices.last()? {
            let raw = smoothed[k].1.atan2(smoothed[k].0);
            phase[k] = raw + TAU * ((phase[k - 1] - raw) / TAU).round();
        }
        for k in (indices[0]..centre).rev() {
            let raw = smoothed[k].1.atan2(smoothed[k].0);
            phase[k] = raw + TAU * ((phase[k + 1] - raw) / TAU).round();
        }
        // Weighted straight line φ = a + b·f.
        let weights: Vec<f64> = indices
            .iter()
            .map(|&k| norm_sqr(smoothed[k]).sqrt())
            .collect();
        let total: f64 = weights.iter().sum();
        if total <= 0.0 || !total.is_finite() {
            return None;
        }
        let mean_f = indices
            .iter()
            .zip(&weights)
            .map(|(&k, w)| w * self.frequencies[k])
            .sum::<f64>()
            / total;
        let mean_p = indices
            .iter()
            .zip(&weights)
            .map(|(&k, w)| w * phase[k])
            .sum::<f64>()
            / total;
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for (&k, w) in indices.iter().zip(&weights) {
            let df = self.frequencies[k] - mean_f;
            sxy += w * df * (phase[k] - mean_p);
            sxx += w * df * df;
        }
        if sxx <= 0.0 {
            return None;
        }
        let slope = sxy / sxx;
        let intercept = mean_p - slope * mean_f;
        let residual = (indices
            .iter()
            .zip(&weights)
            .map(|(&k, w)| w * (phase[k] - intercept - slope * self.frequencies[k]).powi(2))
            .sum::<f64>()
            / total)
            .sqrt();
        // φ_sub − φ_speaker = intercept − 2π f τ.
        Some((-slope / TAU, intercept, residual))
    }
}

/// Wrap an angle to (−π, π].
fn wrap(angle: f64) -> f64 {
    let wrapped = angle - TAU * (angle / TAU).round();
    if wrapped <= -PI {
        wrapped + TAU
    } else {
        wrapped
    }
}

/// The best delay and polarity for `score` over `[from, to]` ms, the candidate nearest
/// `centre` among those within [`TIE_DB`] of the best.
fn search(
    from: f64,
    to: f64,
    centre: f64,
    step: f64,
    score: &dyn Fn(f64, bool) -> f64,
) -> (f64, bool, f64) {
    let mut candidates: Vec<(f64, bool, f64)> = vec![];
    let coarse = ((to - from) / COARSE_STEP_MS).ceil().max(1.0) as usize;
    for inverted in [false, true] {
        for i in 0..=coarse {
            let delay = (from + i as f64 * COARSE_STEP_MS).min(to);
            candidates.push((delay, inverted, score(delay, inverted)));
        }
    }
    let best = candidates
        .iter()
        .map(|c| c.2)
        .fold(f64::NEG_INFINITY, f64::max);
    // Refine every coarse peak near the best: local maxima can be a period apart.
    let mut refined: Vec<(f64, bool, f64)> = vec![];
    for (i, c) in candidates.iter().enumerate() {
        let neighbours = [i.checked_sub(1), Some(i + 1)];
        let peak = neighbours
            .iter()
            .flatten()
            .all(|&j| candidates.get(j).is_none_or(|n| n.1 != c.1 || n.2 <= c.2));
        if !peak || c.2 < best - 1.0 {
            continue;
        }
        let fine = (2.0 * COARSE_STEP_MS / step).round() as usize;
        let mut top = *c;
        for j in 0..=fine {
            let delay = (c.0 - COARSE_STEP_MS + j as f64 * step).clamp(from, to);
            let value = score(delay, c.1);
            if value > top.2 {
                top = (delay, c.1, value);
            }
        }
        refined.push(top);
    }
    let best = refined
        .iter()
        .map(|c| c.2)
        .fold(f64::NEG_INFINITY, f64::max);
    refined
        .into_iter()
        .filter(|c| c.2 >= best - TIE_DB)
        .min_by(|a, b| (a.0 - centre).abs().total_cmp(&(b.0 - centre).abs()))
        .map(|(d, i, v)| ((d / step).round() * step, i, v))
        .unwrap_or((0.0, false, f64::NEG_INFINITY))
}

fn curve(values: &[f64], indices: &[usize]) -> Vec<f32> {
    indices
        .iter()
        .map(|&k| round(values[k], 2) as f32)
        .collect()
}

/// Analyse the speakers' parts and propose one subwoofer setting for all of them.
/// `filter` is the subwoofer's planned mode correction, part of every prediction.
pub fn propose(
    speakers: &[SpeakerResponses],
    subwoofer: &Subwoofer,
    filter: Option<&Filter>,
    settings: &Settings,
) -> Result<Proposal> {
    settings.validate()?;
    require(
        [
            subwoofer.delay_ms,
            subwoofer.gain_db,
            subwoofer.others_max_delay_ms,
            subwoofer.max_delay_ms,
        ]
        .iter()
        .all(|v| v.is_finite())
            && subwoofer.max_delay_ms > 0.0
            && (0.0..=subwoofer.max_delay_ms).contains(&subwoofer.delay_ms)
            && (0.0..=subwoofer.max_delay_ms).contains(&subwoofer.others_max_delay_ms),
        "subwoofer settings must be finite and within the delay limit",
    )?;
    require(!subwoofer.output_id.is_empty(), "the subwoofer needs an id")?;
    require(speakers.len() <= 64, "too many speakers")?;
    for speaker in speakers {
        require(
            speaker.crossover_hz.is_finite()
                && (20.0..=1000.0).contains(&speaker.crossover_hz)
                && speaker.swept_hz.iter().all(|v| v.is_finite() && *v > 0.0)
                && speaker.swept_hz[0] < speaker.swept_hz[1]
                && !speaker.output_id.is_empty()
                && speaker.positions.len() <= 64,
            "speaker crossover and sweep must be finite and ascending",
        )?;
        for p in &speaker.positions {
            p.speaker.validate()?;
            p.sum.validate()?;
            if let Some(s) = &p.subwoofer {
                s.validate()?;
            }
        }
    }
    if let Some(f) = filter {
        require(
            f.sample_rate.is_finite()
                && f.sample_rate > 0.0
                && !f.taps.is_empty()
                && f.taps.iter().all(|v| v.is_finite()),
            "the subwoofer filter must be finite",
        )?;
    }
    let mut proposal = Proposal {
        method: METHOD.into(),
        subwoofer: subwoofer.clone(),
        setting: Setting::UNCHANGED,
        subwoofer_delay_ms: subwoofer.delay_ms,
        subwoofer_gain_db: subwoofer.gain_db,
        others_delay_ms: 0.0,
        mode_correction: filter.is_some(),
        speakers: vec![],
        warnings: vec![],
        refusal: None,
    };
    let mut prepared: Vec<Option<Prepared>> = vec![];
    for speaker in speakers {
        let mut result = SpeakerResult {
            output_id: speaker.output_id.clone(),
            crossover_hz: speaker.crossover_hz,
            band_hz: None,
            positions: 0,
            excluded: None,
            phase_delay_ms: None,
            phase_inverted: None,
            phase_fit_deg: None,
            subwoofer_share_db: None,
            consistency_db: None,
            best: None,
            position_best: vec![],
            summation_db: None,
            sum_level_db: None,
            balance_db: None,
            warnings: vec![],
            curves: None,
        };
        match Prepared::new(speaker, filter, None, settings) {
            Err(reason) => {
                result.excluded = Some(reason);
                prepared.push(None);
            }
            Ok((p, warnings)) => {
                result.warnings = warnings;
                result.band_hz = Some([round(p.band_hz[0], 1), round(p.band_hz[1], 1)]);
                result.positions = p.parts.len();
                prepared.push(Some(p));
            }
        }
        proposal.speakers.push(result);
    }

    // Per speaker: timing from the phase, share, consistency.
    for (result, p) in proposal.speakers.iter_mut().zip(&prepared) {
        let Some(p) = p else { continue };
        let total = p.total_weight();
        let fits: Vec<(f64, (f64, f64, f64))> = p
            .parts
            .iter()
            .filter_map(|parts| p.phase_fit(parts, settings).map(|f| (parts.weight, f)))
            .collect();
        let fit_weight: f64 = fits.iter().map(|f| f.0).sum();
        if fit_weight > 0.0 {
            let delay = fits.iter().map(|(w, f)| w * f.0).sum::<f64>() / fit_weight;
            // The offset left once that delay is taken out, averaged as a direction.
            let (c, s) = fits.iter().fold((0.0, 0.0), |(c, s), (w, f)| {
                let offset = wrap(f.1);
                (c + w * offset.cos(), s + w * offset.sin())
            });
            let offset = s.atan2(c);
            let residual = fits.iter().map(|(w, f)| w * f.2).sum::<f64>() / fit_weight;
            result.phase_delay_ms = Some(round(delay * 1000.0, 2));
            result.phase_inverted = Some(offset.abs() > PI / 2.0);
            result.phase_fit_deg = Some(round(residual.to_degrees(), 1));
        }
        let mut share = 0.0;
        let mut consistency = (0.0, 0.0);
        for parts in &p.parts {
            let speaker = p.power(&parts.speaker, settings);
            let sub = p.power(&parts.subwoofer, settings);
            share += parts.weight * (p.mean_db(&sub, &p.low) - p.mean_db(&speaker, &p.low));
            if parts.measured_apart {
                let measured = p.power(&parts.sum, settings);
                let added = p.sum_power(parts, &Setting::UNCHANGED, false, settings);
                let rms = (p
                    .band
                    .iter()
                    .map(|&k| (decibels(measured[k]) - decibels(added[k])).powi(2))
                    .sum::<f64>()
                    / p.band.len() as f64)
                    .sqrt();
                consistency.0 += parts.weight * rms;
                consistency.1 += parts.weight;
            }
        }
        let share = share / total;
        result.subwoofer_share_db = Some(round(share, 1));
        if consistency.1 > 0.0 {
            let rms = consistency.0 / consistency.1;
            result.consistency_db = Some(round(rms, 2));
            if rms > settings.consistency_db[1] {
                result.excluded = Some(format!(
                    "The sum measured differs from its parts added by {rms:.1} dB: something changed between the measurements. Measure it again."
                ));
            } else if rms > settings.consistency_db[0] {
                result.warnings.push(format!(
                    "The sum measured differs from its parts added by {rms:.1} dB."
                ));
            }
        }
        if result.excluded.is_none() && share < settings.min_share_db {
            result.excluded = Some(format!(
                "The subwoofer plays little of its low band ({share:.0} dB against the speaker's own part): its bass goes to other speakers."
            ));
        }
        if p.parts.iter().any(|parts| !parts.measured_apart) {
            result.warnings.push(
                "The timing reference keeps playing: its subwoofer part is the sum minus its own part."
                    .into(),
            );
        }
    }
    let included: Vec<usize> = (0..prepared.len())
        .filter(|&i| prepared[i].is_some() && proposal.speakers[i].excluded.is_none())
        .collect();
    if included.is_empty() {
        proposal.refusal = Some(if proposal.speakers.is_empty() {
            "No speaker with a crossover to the subwoofer was measured.".into()
        } else {
            "No measured speaker can be integrated with the subwoofer.".into()
        });
        return Ok(proposal);
    }

    // The delay range: the subwoofer's delay within the limit, or the others delayed.
    let headroom = subwoofer.max_delay_ms - subwoofer.others_max_delay_ms;
    let (from_limit, to_limit) = (
        -(subwoofer.delay_ms + headroom),
        subwoofer.max_delay_ms - subwoofer.delay_ms,
    );
    let lowest_fc = included
        .iter()
        .filter_map(|&i| prepared[i].as_ref())
        .map(|p| p.crossover_hz)
        .fold(f64::INFINITY, f64::min);
    let span_ms = settings.search_periods * 1000.0 / lowest_fc;
    let phase_centres: Vec<f64> = included
        .iter()
        .filter_map(|&i| proposal.speakers[i].phase_delay_ms)
        .collect();
    // Undoing the measured lateness is where the phases line up.
    let centre = if phase_centres.is_empty() {
        0.0
    } else {
        -phase_centres.iter().sum::<f64>() / phase_centres.len() as f64
    }
    .clamp(from_limit, to_limit);
    let (from, to) = (
        (centre - span_ms).max(from_limit),
        (centre + span_ms).min(to_limit),
    );
    let speakers_of = |indices: &[usize]| -> Vec<&Prepared> {
        indices
            .iter()
            .filter_map(|&i| prepared[i].as_ref())
            .collect()
    };
    let shared = speakers_of(&included);
    let shared_level = |gain: f64, delay: f64, inverted: bool| {
        let setting = Setting {
            delay_change_ms: delay,
            inverted,
            gain_change_db: gain,
        };
        shared
            .iter()
            .map(|p| p.level(&setting, true, None, settings))
            .sum::<f64>()
            / shared.len() as f64
    };
    // Delay and polarity at the measured level, then the level, then both again.
    let mut setting = Setting::UNCHANGED;
    for _ in 0..2 {
        let gain = setting.gain_change_db;
        let (delay, inverted, _) = search(from, to, centre, settings.delay_step_ms, &|d, i| {
            shared_level(gain, d, i)
        });
        setting.delay_change_ms = delay;
        setting.inverted = inverted;
        let gains: Vec<(f64, bool)> = shared
            .iter()
            .map(|p| p.balancing_gain(&setting, true, settings))
            .collect();
        setting.gain_change_db = gains.iter().map(|g| g.0).sum::<f64>() / gains.len() as f64;
    }
    let gain = setting.gain_change_db;
    let (delay, inverted, _) = search(from, to, centre, settings.delay_step_ms, &|d, i| {
        shared_level(gain, d, i)
    });
    setting.delay_change_ms = round(delay, 3);
    setting.inverted = inverted;
    setting.gain_change_db = round(setting.gain_change_db, 1);
    let new_gain = (subwoofer.gain_db + setting.gain_change_db).clamp(-60.0, 24.0);
    setting.gain_change_db = round(new_gain - subwoofer.gain_db, 2);
    proposal.setting = setting;
    let absolute = subwoofer.delay_ms + setting.delay_change_ms;
    if absolute >= 0.0 {
        proposal.subwoofer_delay_ms = round(absolute, 3);
        proposal.others_delay_ms = 0.0;
    } else {
        proposal.subwoofer_delay_ms = 0.0;
        proposal.others_delay_ms = round(-absolute, 3);
        proposal.warnings.push(format!(
            "The subwoofer arrives too late even without delay: every other output is delayed by {:.2} ms instead, which adds that much latency.",
            -absolute
        ));
    }
    proposal.subwoofer_gain_db = round(new_gain, 2);
    if setting.gain_change_db.abs() >= settings.max_gain_change_db - 0.05 {
        proposal.warnings.push(format!(
            "The level change reached its {:.0} dB limit: check the subwoofer's own volume.",
            settings.max_gain_change_db
        ));
    }

    // Per speaker: its own best, per position, and how the shared setting serves it.
    for (i, p) in prepared.iter().enumerate() {
        let Some(p) = p else { continue };
        let result = &mut proposal.speakers[i];
        let own_score = |delay: f64, inverted: bool| {
            p.level(
                &Setting {
                    delay_change_ms: delay,
                    inverted,
                    gain_change_db: setting.gain_change_db,
                },
                true,
                None,
                settings,
            )
        };
        let local_centre = result.phase_delay_ms.map_or(centre, |d| -d);
        let local = (
            (local_centre - span_ms).max(from_limit),
            (local_centre + span_ms).min(to_limit),
        );
        let (delay, inverted, _) = search(
            local.0,
            local.1,
            local_centre,
            settings.delay_step_ms,
            &own_score,
        );
        let own = Setting {
            delay_change_ms: round(delay, 3),
            inverted,
            gain_change_db: setting.gain_change_db,
        };
        let (gain, _) = p.balancing_gain(&own, true, settings);
        result.best = Some(Setting {
            gain_change_db: round(gain, 2),
            ..own
        });
        for (k, parts) in p.parts.iter().enumerate() {
            let score = |delay: f64, inverted: bool| {
                p.level(
                    &Setting {
                        delay_change_ms: delay,
                        inverted,
                        gain_change_db: setting.gain_change_db,
                    },
                    true,
                    Some(k),
                    settings,
                )
            };
            let (delay, inverted, _) = search(
                local.0,
                local.1,
                local_centre,
                settings.delay_step_ms,
                &score,
            );
            result.position_best.push(PositionBest {
                position_id: parts.position_id.clone(),
                delay_change_ms: round(delay, 2),
                inverted,
            });
        }
        let measured = Setting::UNCHANGED;
        result.summation_db = Some([
            round(p.summation(&measured, false, settings), 2),
            round(p.summation(&own, true, settings), 2),
            round(p.summation(&setting, true, settings), 2),
        ]);
        let before = p.level(&measured, false, None, settings);
        let after = p.level(&setting, true, None, settings);
        let own_level = p.level(&own, true, None, settings);
        result.sum_level_db = Some([round(before, 2), round(after, 2)]);
        result.balance_db = Some([
            round(p.balance(&measured, false, settings), 2),
            round(p.balance(&setting, true, settings), 2),
        ]);
        if result.excluded.is_none() && own_level - after > settings.compromise_warning_db {
            result.warnings.push(format!(
                "The shared setting costs it {:.1} dB around its crossover against its own best ({:+.2} ms{}).",
                own_level - after,
                own.delay_change_ms,
                if own.inverted { ", inverted" } else { "" }
            ));
        }
        // Positions that disagree: their best delays spread over a quarter period.
        let quarter_ms = 250.0 / p.crossover_hz;
        let delays: Vec<f64> = result
            .position_best
            .iter()
            .filter(|b| b.inverted == own.inverted)
            .map(|b| b.delay_change_ms)
            .collect();
        let spread = delays.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - delays.iter().copied().fold(f64::INFINITY, f64::min);
        let polarities_differ = result
            .position_best
            .iter()
            .any(|b| b.inverted != own.inverted);
        if result.position_best.len() > 1 && (spread > quarter_ms || polarities_differ) {
            result.warnings.push(format!(
                "Its positions disagree on the best timing ({}): the setting is a compromise.",
                result
                    .position_best
                    .iter()
                    .map(|b| format!(
                        "{} {:+.1} ms{}",
                        b.position_id,
                        b.delay_change_ms,
                        if b.inverted { " inverted" } else { "" }
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // Spatially averaged curves.
        let n = p.frequencies.len();
        let mut curves = [
            vec![0.0; n],
            vec![0.0; n],
            vec![0.0; n],
            vec![0.0; n],
            vec![0.0; n],
        ];
        for parts in &p.parts {
            let sub: Vec<(f64, f64)> = parts
                .subwoofer
                .iter()
                .zip(&p.filter)
                .map(|(&b, &f)| {
                    let g = 10_f64.powf(setting.gain_change_db / 20.0);
                    let v = mul(b, f);
                    (v.0 * g, v.1 * g)
                })
                .collect();
            let values = [
                p.power(&parts.speaker, settings),
                p.power(&sub, settings),
                p.power(&parts.sum, settings),
                p.sum_power(parts, &setting, true, settings),
                p.coherent_power(parts, &setting, true, settings),
            ];
            for (curve, value) in curves.iter_mut().zip(&values) {
                for (c, v) in curve.iter_mut().zip(value) {
                    *c += parts.weight * v;
                }
            }
        }
        let total = p.total_weight();
        let db =
            |values: &[f64]| -> Vec<f64> { values.iter().map(|v| decibels(v / total)).collect() };
        result.curves = Some(Curves {
            frequency_hz: p.band.iter().map(|&k| round(p.frequencies[k], 3)).collect(),
            speaker_db: curve(&db(&curves[0]), &p.band),
            subwoofer_db: curve(&db(&curves[1]), &p.band),
            measured_sum_db: curve(&db(&curves[2]), &p.band),
            predicted_sum_db: curve(&db(&curves[3]), &p.band),
            coherent_db: curve(&db(&curves[4]), &p.band),
        });
    }
    let costly: Vec<&str> = proposal
        .included()
        .filter(|s| {
            s.warnings
                .iter()
                .any(|w| w.starts_with("The shared setting costs"))
        })
        .map(|s| s.output_id.as_str())
        .collect();
    if !costly.is_empty() {
        proposal.warnings.push(format!(
            "One setting cannot suit every speaker: {} would sum better on its own.",
            costly.join(", ")
        ));
    }
    if proposal.included().any(|s| {
        s.warnings
            .iter()
            .any(|w| w.starts_with("Its positions disagree"))
    }) {
        proposal.warnings.push(
            "The positions disagree on the timing for some speakers: the proposal is a compromise around the seat."
                .into(),
        );
    }
    Ok(proposal)
}

/// Compare sums measured after applying `proposal` with what it predicted, at the
/// position each was measured at. `measured` holds, per speaker, the new sum on the same
/// time base as the analysis' takes at that position.
pub fn verify(
    speakers: &[SpeakerResponses],
    proposal: &Proposal,
    filter: Option<&Filter>,
    measured: &[(String, String, Response)],
    settings: &Settings,
) -> Result<Verification> {
    settings.validate()?;
    let mut verification = Verification {
        speakers: vec![],
        warnings: vec![],
    };
    for (output_id, position_id, response) in measured {
        response.validate()?;
        let Some(speaker) = speakers.iter().find(|s| &s.output_id == output_id) else {
            return Err(Error::Invalid(format!("{output_id} was not analysed")));
        };
        let positions: Vec<PositionResponses> = speaker
            .positions
            .iter()
            .filter(|p| &p.position_id == position_id)
            .cloned()
            .collect();
        if positions.is_empty() {
            verification.warnings.push(format!(
                "{output_id}: {position_id} was not measured before; nothing to compare with."
            ));
            continue;
        }
        let one = SpeakerResponses {
            positions,
            ..speaker.clone()
        };
        let prepared = match Prepared::new(&one, filter, Some(response), settings) {
            Ok((p, _)) if !p.parts.is_empty() => p,
            Ok(_) => continue,
            Err(reason) => {
                verification.warnings.push(format!(
                    "{output_id}: {reason} The new sum cannot be compared; measure it again."
                ));
                continue;
            }
        };
        let parts = &prepared.parts[0];
        let now = spectrum(response, &prepared.frequencies, parts.window);
        let now = prepared.power(&now, settings);
        let before_power = prepared.power(&parts.sum, settings);
        let before = prepared.mean_db(&before_power, &prepared.band);
        let balance = |power: &[f64]| {
            prepared.mean_db(power, &prepared.low) - prepared.mean_db(power, &prepared.high)
        };
        let predicted_power = prepared.sum_power(parts, &proposal.setting, true, settings);
        let predicted = prepared.mean_db(&predicted_power, &prepared.band);
        let measured_level = prepared.mean_db(&now, &prepared.band);
        let deviation = (prepared
            .band
            .iter()
            .map(|&k| (decibels(now[k]) - decibels(predicted_power[k])).powi(2))
            .sum::<f64>()
            / prepared.band.len() as f64)
            .sqrt();
        if deviation > settings.consistency_db[0] {
            verification.warnings.push(format!(
                "{output_id}: the sum measured now differs from the prediction by {deviation:.1} dB."
            ));
        }
        verification.speakers.push(SpeakerCheck {
            output_id: output_id.clone(),
            position_id: position_id.clone(),
            sum_level_db: [
                round(before, 2),
                round(predicted, 2),
                round(measured_level, 2),
            ],
            balance_db: [
                round(balance(&before_power), 2),
                round(balance(&predicted_power), 2),
                round(balance(&now), 2),
            ],
            deviation_db: round(deviation, 2),
        });
    }
    Ok(verification)
}

/// How far, in frames, `a`'s content lies after `b`'s: the lag that best lines up two
/// recordings of the same sound (the reference sweep of two takes cut around their own
/// correlation peaks), within `max_frames`, refined between frames. `None` when the best
/// lag is at the limit or either is silent.
pub fn lag(a: &[f32], b: &[f32], max_frames: usize) -> Option<f64> {
    use rustfft::{FftPlanner, num_complex::Complex};
    let len = a.len().max(b.len());
    if a.is_empty() || b.is_empty() || len > 1 << 22 {
        return None;
    }
    let n = (2 * len).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(n);
    let inverse = planner.plan_fft_inverse(n);
    let load = |x: &[f32]| {
        let mut v: Vec<Complex<f64>> = x.iter().map(|&s| Complex::new(f64::from(s), 0.0)).collect();
        v.resize(n, Complex::default());
        v
    };
    let (mut fa, mut fb) = (load(a), load(b));
    forward.process(&mut fa);
    forward.process(&mut fb);
    // r(l) = Σ a[k] b[k − l]: peaks where a's content lies l frames after b's.
    let mut r: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(x, y)| x * y.conj()).collect();
    inverse.process(&mut r);
    let at = |l: i64| r[l.rem_euclid(n as i64) as usize].re;
    let max = max_frames.min(len - 1) as i64;
    let (best, value) = (-max..=max)
        .map(|l| (l, at(l)))
        .max_by(|x, y| x.1.total_cmp(&y.1))?;
    if value <= 0.0 || best.abs() == max {
        return None;
    }
    let (left, right) = (at(best - 1), at(best + 1));
    let curvature = left - 2.0 * value + right;
    let shift = if curvature < 0.0 {
        0.5 * (left - right) / curvature
    } else {
        0.0
    };
    Some(best as f64 + shift.clamp(-0.5, 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_lag_lines_up_a_shifted_copy() {
        let sound: Vec<f32> = (0..4000)
            .map(|i| {
                let t = i as f32 / 48000.0;
                (std::f32::consts::TAU * 60.0 * t * (1.0 + 20.0 * t)).sin()
                    * (-(t - 0.04).powi(2) / 0.0004).exp()
            })
            .collect();
        let mut later = vec![0.0; 7];
        later.extend_from_slice(&sound[..sound.len() - 7]);
        let lag = lag(&later, &sound, 96).unwrap();
        assert!((lag - 7.0).abs() < 0.05, "{lag}");
        assert!((super::lag(&sound, &later, 96).unwrap() + 7.0).abs() < 0.05);
        assert!(super::lag(&later, &sound, 7).is_none());
        assert!(super::lag(&vec![0.0; 100], &sound, 10).is_none());
    }
}
