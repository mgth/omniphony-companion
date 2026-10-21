//! Room-correction filter design from stored responses: one minimum-phase FIR per output.
//!
//! For each output, the responses measured at the main seat and the positions around it
//! are windowed with a frequency-dependent window (long below the modal region so room
//! modes are resolved, a few periods above it so late reflections stay out), corrected
//! by the microphone calibration, power-averaged over the positions (the main seat
//! counts more) and smoothed (finely below the modal region, by a third of an octave
//! above). The correction moves that average toward a target, flat with a gentle
//! downward tilt, within limits: no boost below the speaker's own −6 dB point or above
//! the correction band, little boost and deeper cuts, faded to 0 dB outside the band.
//! The filter is its minimum-phase realisation, normalised so the response keeps its
//! level over 500 Hz–2 kHz as an alignment measures it: the level alignment stays valid.
//!
//! Two things are diagnosed, never corrected: early reflections, whose comb a few
//! centimetres move, and the excess phase above the bass, which differs between
//! sessions. Outputs whose positions disagree too much are refused.
//!
//! Pure and offline: no file, device or network access, never in an audio callback.
mod filter;
mod reflections;
mod spectrum;

use crate::{Error, Result, require};
pub use reflections::{FLOOR_DB as REFLECTION_FLOOR_DB, Reflection, SPEED_OF_SOUND_M_S};
use serde::{Deserialize, Serialize};
pub use spectrum::{POINTS_PER_OCTAVE, grid};
use spectrum::{
    band_mean, decibels, interpolate, rms_difference, rms_shape_difference, smooth, smooth_db,
};

pub const METHOD: &str = "fdw-spatial-average-minimum-phase-v1";
/// The band every curve is levelled at (mean power, 0 dB), the alignment's level band.
pub const LEVEL_BAND_HZ: [f64; 2] = [500.0, 2000.0];
/// A position whose smoothed response differs from the average by more than this (RMS
/// above the modal region) is named in a warning.
pub const POSITION_WARNING_DB: f64 = 2.0;
/// When the median position differs by more than this, the output is not corrected.
pub const POSITION_REFUSAL_DB: f64 = 3.0;
/// Main-seat takes that differ by more than this are named in a warning.
pub const REPEATABILITY_WARNING_DB: f64 = 1.0;
/// Boosts are reduced where the positions' spread exceeds the first value and removed
/// where it reaches the second: a dip that moves is not filled.
pub const BOOST_SPREAD_DB: [f64; 2] = [1.5, 4.0];
/// Two or more reflections this close to the direct sound are named in a warning.
pub const STRONG_REFLECTION_DB: f64 = -3.0;
/// The correction fades to 0 dB over this many octaves outside its band.
const FADE_OCTAVES: f64 = 0.5;
/// The correction band starts and ends this far inside the measured range.
const EDGE_OCTAVES: f64 = 1.0 / 6.0;
/// Boosts ramp in over this many octaves above the speaker's −6 dB point.
const LOW_RAMP_OCTAVES: f64 = 1.0 / 3.0;
/// Smoothing used to find the speaker's −6 dB point, wide enough to ride over modes.
const LOW_LIMIT_SMOOTHING_OCTAVES: f64 = 1.0;
/// Where the search for the −6 dB point starts, going down.
const LOW_LIMIT_SEARCH_HZ: f64 = 500.0;
/// The direct sound's peak is looked for this close to the stored anchor.
const PEAK_SEARCH_MS: f64 = 2.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesignSettings {
    /// The engine's rate: the filter's taps are at this rate.
    pub sample_rate: u32,
    pub taps: usize,
    /// No correction above this frequency.
    pub max_hz: f64,
    /// The target falls by this much per decade above `tilt_start_hz`.
    pub tilt_db_per_decade: f64,
    pub tilt_start_hz: f64,
    pub max_boost_db: f64,
    /// The deepest cut, as a positive number of dB.
    pub max_cut_db: f64,
    /// The main seat's weight in the spatial average; every other position weighs 1.
    pub main_seat_weight: f64,
    /// Periods of the frequency-dependent window above the modal region.
    pub window_cycles: f64,
    /// Room modes dominate below this: long windows and fine smoothing.
    pub modal_hz: f64,
    pub fine_smoothing_octaves: f64,
    pub smoothing_octaves: f64,
}
impl Default for DesignSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            taps: 8192,
            max_hz: 10_000.0,
            tilt_db_per_decade: 3.0,
            tilt_start_hz: 1000.0,
            max_boost_db: 3.5,
            max_cut_db: 15.0,
            main_seat_weight: 2.0,
            window_cycles: 8.0,
            modal_hz: 300.0,
            fine_smoothing_octaves: 1.0 / 24.0,
            smoothing_octaves: 1.0 / 3.0,
        }
    }
}
impl DesignSettings {
    pub fn validate(&self) -> Result<()> {
        let finite = [
            self.max_hz,
            self.tilt_db_per_decade,
            self.tilt_start_hz,
            self.max_boost_db,
            self.max_cut_db,
            self.main_seat_weight,
            self.window_cycles,
            self.modal_hz,
            self.fine_smoothing_octaves,
            self.smoothing_octaves,
        ]
        .iter()
        .all(|v| v.is_finite());
        require(finite, "design settings must be finite")?;
        require(
            (8000..=384_000).contains(&self.sample_rate),
            "filter rate must be 8–384 kHz",
        )?;
        require(
            (256..=65_536).contains(&self.taps),
            "filter length must be 256–65536 taps",
        )?;
        require(
            (2000.0..=20_000.0).contains(&self.max_hz),
            "correction limit must be 2–20 kHz",
        )?;
        require(
            (0.0..=10.0).contains(&self.tilt_db_per_decade),
            "target tilt must be 0–10 dB per decade",
        )?;
        require(
            (200.0..=10_000.0).contains(&self.tilt_start_hz),
            "tilt must start at 200 Hz–10 kHz",
        )?;
        require(
            (0.0..=6.0).contains(&self.max_boost_db),
            "boost limit must be 0–6 dB",
        )?;
        require(
            (0.0..=30.0).contains(&self.max_cut_db),
            "cut limit must be 0–30 dB",
        )?;
        require(
            (1.0..=10.0).contains(&self.main_seat_weight),
            "main-seat weight must be 1–10",
        )?;
        require(
            (3.0..=30.0).contains(&self.window_cycles),
            "window must be 3–30 periods",
        )?;
        require(
            (100.0..=1000.0).contains(&self.modal_hz),
            "modal region must end at 100 Hz–1 kHz",
        )?;
        require(
            (1.0 / 48.0..=1.0).contains(&self.fine_smoothing_octaves)
                && (1.0 / 48.0..=1.0).contains(&self.smoothing_octaves),
            "smoothing must be 1/48–1 octave",
        )
    }
    /// The grid the design works on: 10 Hz to 24 kHz or 0.45 times the filter rate.
    pub fn frequencies(&self) -> Vec<f64> {
        grid(10.0, (0.45 * f64::from(self.sample_rate)).min(24_000.0))
    }
    fn smoothing(&self, hz: f64) -> f64 {
        spectrum::smoothing_width(
            hz,
            self.fine_smoothing_octaves,
            self.smoothing_octaves,
            self.modal_hz,
        )
    }
    /// The target before levelling: flat, then falling by the tilt above its start.
    fn tilt_db(&self, hz: f64) -> f64 {
        if hz <= self.tilt_start_hz {
            0.0
        } else {
            -self.tilt_db_per_decade * (hz / self.tilt_start_hz).log10()
        }
    }
}

/// A microphone's deviation from flat: subtracted from what it measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicrophoneCalibration {
    /// Tells two calibrations apart (the stored file's SHA-256).
    pub id: String,
    /// `[frequency_hz, deviation_db]`, sorted by frequency.
    pub points: Vec<[f64; 2]>,
}
impl MicrophoneCalibration {
    /// Interpolated linearly over log frequency, held outside the covered range.
    pub fn deviation_db(&self, hz: f64) -> f64 {
        let frequencies: Vec<f64> = self.points.iter().map(|p| p[0]).collect();
        let values: Vec<f64> = self.points.iter().map(|p| p[1]).collect();
        interpolate(&frequencies, &values, hz)
    }
    fn validate(&self) -> Result<()> {
        require(
            self.points.len() >= 2
                && self
                    .points
                    .iter()
                    .all(|p| p[0].is_finite() && p[0] > 0.0 && p[1].is_finite())
                && self.points.windows(2).all(|w| w[0][0] < w[1][0]),
            "microphone calibration must be at least two sorted finite points",
        )
    }
}

/// One stored response of an output: the speaker segment of a take.
#[derive(Debug, Clone)]
pub struct Measurement {
    pub position_id: String,
    pub main_seat: bool,
    /// Response frames per second (after clock compensation).
    pub rate: f64,
    pub samples: Vec<f32>,
    /// Frame of the correlation anchor; the direct sound is looked for within 2 ms.
    pub anchor: usize,
    /// The swept range: nothing outside it is used.
    pub band_hz: [f64; 2],
    pub calibration: Option<MicrophoneCalibration>,
}
impl Measurement {
    fn validate(&self) -> Result<()> {
        require(
            self.rate.is_finite() && (8000.0..=400_000.0).contains(&self.rate),
            "response rate must be 8–400 kHz",
        )?;
        require(
            self.anchor < self.samples.len() && self.samples.iter().all(|v| v.is_finite()),
            "response must be finite with its anchor inside",
        )?;
        require(
            self.band_hz.iter().all(|v| v.is_finite())
                && self.band_hz[0] > 0.0
                && self.band_hz[0] < self.band_hz[1],
            "swept band must be finite and ascending",
        )?;
        require(!self.position_id.is_empty(), "response needs a position id")?;
        self.calibration
            .as_ref()
            .map_or(Ok(()), MicrophoneCalibration::validate)
    }
    /// The direct sound: the largest sample within [`PEAK_SEARCH_MS`] of the anchor.
    fn peak(&self) -> usize {
        let span = (PEAK_SEARCH_MS / 1000.0 * self.rate).round() as usize;
        let range =
            self.anchor.saturating_sub(span)..(self.anchor + span + 1).min(self.samples.len());
        range
            .max_by(|&a, &b| self.samples[a].abs().total_cmp(&self.samples[b].abs()))
            .unwrap_or(self.anchor)
    }
    /// Calibrated power in the frequency-dependent window at each of `frequencies`;
    /// `NaN` outside the swept band.
    fn power(&self, settings: &DesignSettings, frequencies: &[f64]) -> Vec<f64> {
        let high = self.band_hz[1].min(0.45 * self.rate);
        let inside: Vec<f64> = frequencies
            .iter()
            .copied()
            .filter(|f| (self.band_hz[0]..=high).contains(f))
            .collect();
        let power = spectrum::fdw_power(
            &self.samples,
            self.rate,
            self.peak(),
            &inside,
            settings.window_cycles,
            settings.modal_hz,
        );
        let mut values = power.into_iter();
        frequencies
            .iter()
            .map(|&f| {
                if !(self.band_hz[0]..=high).contains(&f) {
                    return f64::NAN;
                }
                let p = values.next().unwrap_or(f64::NAN);
                match &self.calibration {
                    Some(c) => p * 10_f64.powf(-c.deviation_db(f) / 10.0),
                    None => p,
                }
            })
            .collect()
    }
}

/// Everything measured for one output.
#[derive(Debug, Clone)]
pub struct OutputMeasurements {
    pub output_id: String,
    pub measurements: Vec<Measurement>,
}

/// How one position's smoothed response compares with the output's average.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionAgreement {
    pub position_id: String,
    pub main_seat: bool,
    pub takes: usize,
    /// RMS difference from the average above the modal region, both levelled at
    /// 500 Hz–2 kHz.
    pub deviation_db: Option<f64>,
}

/// Curves on the design grid over the measured band, all levelled at 500 Hz–2 kHz.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    pub frequency_hz: Vec<f64>,
    /// The smoothed spatial average.
    pub measured_db: Vec<f32>,
    pub target_db: Vec<f32>,
    /// The correction asked of the filter, within its limits.
    pub correction_db: Vec<f32>,
    /// What the filter's taps realise.
    pub filter_db: Vec<f32>,
    /// `measured_db + filter_db`: the corrected response this design predicts.
    pub predicted_db: Vec<f32>,
}

/// A designed filter, ready to export or to send: mono taps at the engine's rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub sample_rate: u32,
    pub taps: Vec<f32>,
    /// The filter's largest gain at any frequency: how far a full-scale tone there would
    /// go over full scale, the largest boost.
    pub headroom_db: f64,
    /// The deepest cut within the correction band (negative).
    pub deepest_cut_db: f64,
    /// The gain applied to the minimum-phase taps so the averaged response keeps its
    /// level over 500 Hz–2 kHz, measured as an alignment measures it (mean power on a
    /// log grid): the level alignment stays valid.
    pub normalisation_db: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputDesign {
    pub output_id: String,
    /// Absent when the output is not corrected; `refusal` says why.
    pub filter: Option<Filter>,
    pub refusal: Option<String>,
    pub warnings: Vec<String>,
    /// Every measurement had a microphone calibration (the same one).
    pub calibrated: bool,
    /// Where the correction acts fully; it fades to 0 dB outside.
    pub band_hz: Option<[f64; 2]>,
    /// The speaker's own −6 dB point below 500 Hz, from the measurement: nothing is
    /// boosted below it. Absent when the measured range does not reach it.
    pub low_limit_hz: Option<f64>,
    pub positions: Vec<PositionAgreement>,
    /// RMS difference between main-seat takes (with two or more).
    pub repeatability_db: Option<f64>,
    /// Median over the band above the modal region of the positions' spread (weighted
    /// standard deviation, dB), with two or more positions.
    pub spread_db: Option<f64>,
    /// Strong early reflections at the main seat (first take), 300 Hz–8 kHz.
    pub reflections: Vec<Reflection>,
    /// The band `deviation_db` is measured over: the correction band above the low limit.
    pub evaluation_band_hz: Option<[f64; 2]>,
    /// How far from the target's shape the response is before and after (predicted):
    /// RMS of their difference over the evaluation band, its mean (a level) removed.
    pub deviation_db: Option<[f64; 2]>,
    pub curves: Option<Curves>,
}

impl OutputDesign {
    /// An output left uncorrected for `reason`, with nothing measured about it.
    pub fn refused(output_id: &str, reason: &str) -> Self {
        Self {
            output_id: output_id.into(),
            filter: None,
            refusal: Some(reason.into()),
            warnings: vec![],
            calibrated: false,
            band_hz: None,
            low_limit_hz: None,
            positions: vec![],
            repeatability_db: None,
            spread_db: None,
            reflections: vec![],
            evaluation_band_hz: None,
            deviation_db: None,
            curves: None,
        }
    }
}

/// The smoothed response of `measurement` alone on `settings.frequencies()`, levelled at
/// 500 Hz–2 kHz like the design's curves (`NaN` outside its band). For checking a design
/// against a measurement it was not designed from.
pub fn smoothed_response(measurement: &Measurement, settings: &DesignSettings) -> Result<Vec<f64>> {
    settings.validate()?;
    measurement.validate()?;
    let frequencies = settings.frequencies();
    let power = measurement.power(settings, &frequencies);
    levelled(&power, &frequencies, settings)
        .ok_or_else(|| Error::Invalid("the response does not cover 500 Hz–2 kHz".into()))
}

/// How far from `design`'s target shape `response` (from [`smoothed_response`]) is
/// before and after `design`'s filter, like [`OutputDesign::deviation_db`]: what the
/// filter would do to a measurement it was not designed from.
pub fn deviation(
    design: &OutputDesign,
    response: &[f64],
    settings: &DesignSettings,
) -> Option<[f64; 2]> {
    design.filter.as_ref()?;
    let curves = design.curves.as_ref()?;
    let filtered = on_grid(curves, settings, |i| f64::from(curves.filter_db[i]));
    let corrected: Vec<f64> = response.iter().zip(&filtered).map(|(r, f)| r + f).collect();
    Some([
        rms_from_target(design, response, settings)?,
        rms_from_target(design, &corrected, settings)?,
    ])
}

/// How far from `design`'s target shape `response` (from [`smoothed_response`]) is: RMS
/// of their difference over the evaluation band, its mean removed.
pub fn rms_from_target(
    design: &OutputDesign,
    response: &[f64],
    settings: &DesignSettings,
) -> Option<f64> {
    let curves = design.curves.as_ref()?;
    let band = design.evaluation_band_hz?;
    let target = on_grid(curves, settings, |i| f64::from(curves.target_db[i]));
    rms_shape_difference(response, &target, &settings.frequencies(), band)
}

/// A curve of `curves` spread onto the design grid, `NaN` where it has no point.
fn on_grid(curves: &Curves, settings: &DesignSettings, value: impl Fn(usize) -> f64) -> Vec<f64> {
    let frequencies = settings.frequencies();
    let mut values = vec![f64::NAN; frequencies.len()];
    let mut i = 0;
    for (k, f) in frequencies.iter().enumerate() {
        if curves.frequency_hz.get(i) == Some(f) {
            values[k] = value(i);
            i += 1;
        }
    }
    values
}

/// Smoothed, in dB, levelled at 500 Hz–2 kHz; `None` when that band is not covered.
fn levelled(power: &[f64], frequencies: &[f64], settings: &DesignSettings) -> Option<Vec<f64>> {
    let smoothed = smooth(power, frequencies, |hz| settings.smoothing(hz));
    let db: Vec<f64> = smoothed
        .iter()
        .map(|&p| if p.is_finite() { decibels(p) } else { f64::NAN })
        .collect();
    let covered = frequencies
        .iter()
        .zip(&db)
        .filter(|(f, _)| (LEVEL_BAND_HZ[0]..=LEVEL_BAND_HZ[1]).contains(*f))
        .all(|(_, v)| v.is_finite());
    let level = power_level(&db, frequencies).filter(|_| covered)?;
    Some(db.iter().map(|v| v - level).collect())
}

/// The level of a dB curve over 500 Hz–2 kHz as an alignment measures it: the mean
/// power over the log grid, in dB. A dip weighs less than a peak.
fn power_level(db: &[f64], frequencies: &[f64]) -> Option<f64> {
    let power: Vec<f64> = db.iter().map(|v| 10_f64.powf(v / 10.0)).collect();
    band_mean(&power, frequencies, LEVEL_BAND_HZ).map(decibels)
}

fn round(value: f64, decimals: i32) -> f64 {
    let scale = 10_f64.powi(decimals);
    (value * scale).round() / scale
}

fn curve(values: &[f64], indices: &[usize]) -> Vec<f32> {
    indices
        .iter()
        .map(|&i| round(values[i], 2) as f32)
        .collect()
}

/// A raised-cosine fade from 1 at 0 octaves to 0 at `width` octaves.
fn fade(octaves: f64, width: f64) -> f64 {
    let t = (octaves / width).clamp(0.0, 1.0);
    0.5 + 0.5 * (std::f64::consts::PI * t).cos()
}

/// Design the correction filter of one output.
pub fn design(output: &OutputMeasurements, settings: &DesignSettings) -> Result<OutputDesign> {
    settings.validate()?;
    require(!output.output_id.is_empty(), "an output needs an id")?;
    require(
        output.measurements.len() <= 64,
        "too many responses for one output",
    )?;
    for m in &output.measurements {
        m.validate()?;
    }
    let mut result = OutputDesign::refused(&output.output_id, "");
    result.refusal = None;
    let refuse = |mut result: OutputDesign, reason: &str| {
        result.refusal = Some(reason.into());
        Ok(result)
    };
    let measurements = &output.measurements;
    let Some(first) = measurements
        .iter()
        .find(|m| m.main_seat)
        .or(measurements.first())
    else {
        return refuse(
            result,
            "No accepted response with its stored response to design from.",
        );
    };
    let frequencies = settings.frequencies();

    // Diagnostics first: they hold whether or not a filter follows.
    let peak = first.peak();
    result.reflections = reflections::early_reflections(
        &first.samples,
        first.rate,
        peak,
        [
            first.band_hz[0].max(300.0),
            first.band_hz[1].min(8000.0).min(0.45 * first.rate),
        ],
    );
    let strong = result
        .reflections
        .iter()
        .filter(|r| r.level_db >= STRONG_REFLECTION_DB)
        .count();
    if strong >= 2 {
        result.warnings.push(format!(
            "{strong} early reflections are within {:.0} dB of the direct sound (a speaker facing away or partly hidden?): a correction holds less well.",
            -STRONG_REFLECTION_DB
        ));
    }
    let ids: Vec<Option<&str>> = measurements
        .iter()
        .map(|m| m.calibration.as_ref().map(|c| c.id.as_str()))
        .collect();
    if ids.iter().any(|id| *id != ids[0]) {
        return refuse(
            result,
            "Its responses were measured with different microphone calibrations; measure them again with one.",
        );
    }
    result.calibrated = ids[0].is_some();

    // Every take's calibrated power, then each position's (mean of its takes).
    let powers: Vec<Vec<f64>> = measurements
        .iter()
        .map(|m| m.power(settings, &frequencies))
        .collect();
    let mut positions: Vec<(String, bool, Vec<usize>)> = vec![];
    for (i, m) in measurements.iter().enumerate() {
        match positions.iter_mut().find(|p| p.0 == m.position_id) {
            Some(p) => p.2.push(i),
            None => positions.push((m.position_id.clone(), m.main_seat, vec![i])),
        }
    }
    positions.sort_by_key(|p| !p.1);
    let mean = |takes: &[usize]| -> Vec<f64> {
        (0..frequencies.len())
            .map(|k| takes.iter().map(|&i| powers[i][k]).sum::<f64>() / takes.len() as f64)
            .collect()
    };
    let position_power: Vec<Vec<f64>> = positions.iter().map(|p| mean(&p.2)).collect();
    let weights: Vec<f64> = positions
        .iter()
        .map(|p| if p.1 { settings.main_seat_weight } else { 1.0 })
        .collect();
    let total: f64 = weights.iter().sum();
    let average: Vec<f64> = (0..frequencies.len())
        .map(|k| {
            position_power
                .iter()
                .zip(&weights)
                .map(|(p, w)| p[k] * w)
                .sum::<f64>()
                / total
        })
        .collect();
    let Some(measured) = levelled(&average, &frequencies, settings) else {
        return refuse(
            result,
            "Its sweep does not cover 500 Hz–2 kHz, which every correction is levelled at: a subwoofer is left to bass integration.",
        );
    };
    let measured_indices: Vec<usize> = (0..frequencies.len())
        .filter(|&k| measured[k].is_finite())
        .collect();
    let (band_low, band_high) = (
        frequencies[measured_indices[0]],
        frequencies[*measured_indices.last().unwrap_or(&0)],
    );
    let low = band_low * 2_f64.powf(EDGE_OCTAVES);
    let high = settings.max_hz.min(band_high / 2_f64.powf(EDGE_OCTAVES));
    if high < 2000.0 || high < 4.0 * low {
        return refuse(result, "Its measured band is too narrow to correct.");
    }
    result.band_hz = Some([round(low, 1), round(high, 1)]);

    // How the positions agree.
    let curves: Vec<Vec<f64>> = position_power
        .iter()
        .map(|p| levelled(p, &frequencies, settings).unwrap_or_else(|| vec![f64::NAN; p.len()]))
        .collect();
    let agreement_band = [low.max(settings.modal_hz), high];
    for (p, curve) in positions.iter().zip(&curves) {
        result.positions.push(PositionAgreement {
            position_id: p.0.clone(),
            main_seat: p.1,
            takes: p.2.len(),
            deviation_db: rms_difference(curve, &measured, &frequencies, agreement_band)
                .map(|v| round(v, 2)),
        });
    }
    let main_takes: Vec<Vec<f64>> = positions
        .iter()
        .filter(|p| p.1)
        .flat_map(|p| p.2.iter())
        .filter_map(|&i| levelled(&powers[i], &frequencies, settings))
        .collect();
    if main_takes.len() >= 2 {
        let mut pairs = vec![];
        for i in 0..main_takes.len() {
            for j in i + 1..main_takes.len() {
                pairs.extend(rms_difference(
                    &main_takes[i],
                    &main_takes[j],
                    &frequencies,
                    agreement_band,
                ));
            }
        }
        result.repeatability_db =
            (!pairs.is_empty()).then(|| round(pairs.iter().sum::<f64>() / pairs.len() as f64, 2));
    }
    let spread: Vec<f64> = (0..frequencies.len())
        .map(|k| {
            if curves.len() < 2 {
                return f64::NAN;
            }
            let mean = curves
                .iter()
                .zip(&weights)
                .map(|(c, w)| c[k] * w)
                .sum::<f64>()
                / total;
            (curves
                .iter()
                .zip(&weights)
                .map(|(c, w)| w * (c[k] - mean).powi(2))
                .sum::<f64>()
                / total)
                .sqrt()
        })
        .collect();
    let mut above: Vec<f64> = frequencies
        .iter()
        .zip(&spread)
        .filter(|(f, s)| (agreement_band[0]..=agreement_band[1]).contains(*f) && s.is_finite())
        .map(|(_, s)| *s)
        .collect();
    if !above.is_empty() {
        above.sort_by(f64::total_cmp);
        result.spread_db = Some(round(above[above.len() / 2], 2));
    }
    if !result.calibrated {
        result.warnings.push(
            "No microphone calibration: the correction includes the microphone's own response."
                .into(),
        );
    }
    if positions.len() == 1 {
        result.warnings.push(
            "Designed from one position only: measure the other positions for a correction that holds around the seat."
                .into(),
        );
    }
    if let Some(r) = result
        .repeatability_db
        .filter(|&r| r > REPEATABILITY_WARNING_DB)
    {
        result.warnings.push(format!(
            "Its main-seat takes differ by {r:.1} dB: something moved or the room was noisy."
        ));
    }
    let mut deviations: Vec<f64> = result
        .positions
        .iter()
        .filter_map(|p| p.deviation_db)
        .collect();
    let far: Vec<String> = result
        .positions
        .iter()
        .filter(|p| p.deviation_db.is_some_and(|d| d > POSITION_WARNING_DB))
        .map(|p| {
            format!(
                "{} ({:.1} dB)",
                p.position_id,
                p.deviation_db.unwrap_or(0.0)
            )
        })
        .collect();
    if positions.len() >= 2 && !deviations.is_empty() {
        deviations.sort_by(f64::total_cmp);
        let median = deviations[deviations.len() / 2];
        if median > POSITION_REFUSAL_DB {
            return refuse(
                result,
                &format!(
                    "Its positions disagree too much to correct (median {median:.1} dB from their average above {:.0} Hz).",
                    settings.modal_hz
                ),
            );
        }
    }
    if !far.is_empty() {
        result.warnings.push(format!(
            "Positions far from the average: {}.",
            far.join(", ")
        ));
    }

    // The speaker's own −6 dB point, going down from 500 Hz on a wide smoothing.
    let wide = smooth(&average, &frequencies, |_| LOW_LIMIT_SMOOTHING_OCTAVES);
    let wide_db: Vec<f64> = wide.iter().map(|&p| decibels(p)).collect();
    let wide_level = power_level(&wide_db, &frequencies).unwrap_or(0.0);
    let start = frequencies.partition_point(|&f| f <= LOW_LIMIT_SEARCH_HZ);
    for k in (measured_indices[0]..start.min(frequencies.len())).rev() {
        let level = wide_db[k] - wide_level;
        if level < -6.0 {
            let above = wide_db[k + 1] - wide_level;
            let t = ((-6.0 - level) / (above - level)).clamp(0.0, 1.0);
            let hz = frequencies[k] * (frequencies[k + 1] / frequencies[k]).powf(t);
            result.low_limit_hz = Some(round(hz, 1));
            break;
        }
    }

    // Target, needed correction and its limits.
    let target_raw: Vec<f64> = frequencies.iter().map(|&f| settings.tilt_db(f)).collect();
    let target_level = power_level(&target_raw, &frequencies).unwrap_or(0.0);
    let target: Vec<f64> = target_raw.iter().map(|v| v - target_level).collect();
    let boost_cap = |k: usize| -> f64 {
        let f = frequencies[k];
        let low_ramp = match result.low_limit_hz {
            Some(limit) if f <= limit => 0.0,
            Some(limit) => 1.0 - fade((f / limit).log2(), LOW_RAMP_OCTAVES),
            None => 1.0,
        };
        let confidence = if spread[k].is_finite() {
            ((BOOST_SPREAD_DB[1] - spread[k]) / (BOOST_SPREAD_DB[1] - BOOST_SPREAD_DB[0]))
                .clamp(0.0, 1.0)
        } else {
            1.0
        };
        settings.max_boost_db * low_ramp * confidence
    };
    let inside = |f: f64| (low..=high).contains(&f);
    let first_inside = frequencies.iter().position(|&f| inside(f)).unwrap_or(0);
    let last_inside = frequencies.iter().rposition(|&f| inside(f)).unwrap_or(0);
    // Limits keep the corrected response from keeping its level over 500 Hz–2 kHz (a
    // capped dip there), and normalising the filter afterwards would lift every boost
    // past its limit: level the limited correction first, by an offset found in a few
    // steps.
    let mut offset = 0.0;
    let limited_with = |offset: f64, k: usize| {
        (target[k] - measured[k] + offset).clamp(-settings.max_cut_db, boost_cap(k))
    };
    for _ in 0..8 {
        let level: Vec<f64> = (0..frequencies.len())
            .map(|k| {
                if inside(frequencies[k]) {
                    measured[k] + limited_with(offset, k)
                } else {
                    f64::NAN
                }
            })
            .collect();
        let mean = power_level(&level, &frequencies).unwrap_or(0.0);
        if mean.abs() < 0.005 {
            break;
        }
        offset -= mean;
    }
    let limited = |k: usize| limited_with(offset, k);
    let (edge_low, edge_high) = (limited(first_inside), limited(last_inside));
    let mut correction: Vec<f64> = (0..frequencies.len())
        .map(|k| {
            let f = frequencies[k];
            if inside(f) {
                limited(k)
            } else if f < low {
                edge_low * fade((low / f).log2(), FADE_OCTAVES)
            } else {
                edge_high * fade((f / high).log2(), FADE_OCTAVES)
            }
        })
        .collect();
    // Round the corners the limits leave, no finer than the taps can realise, then
    // hold the limits again.
    let resolution_hz = f64::from(settings.sample_rate) / settings.taps as f64;
    correction = smooth_db(&correction, &frequencies, |hz| {
        let realisable = 2.0 * ((hz + resolution_hz / 2.0) / hz).log2();
        spectrum::smoothing_width(
            hz,
            settings.fine_smoothing_octaves,
            settings.smoothing_octaves / 2.0,
            settings.modal_hz,
        )
        .max(realisable)
    });
    for (k, value) in correction.iter_mut().enumerate() {
        let cap = if inside(frequencies[k]) {
            boost_cap(k)
        } else if frequencies[k] < low {
            boost_cap(first_inside)
        } else {
            settings.max_boost_db
        };
        *value = value.clamp(-settings.max_cut_db, cap);
    }

    // The filter, normalised so the averaged response keeps its level over
    // 500 Hz–2 kHz (it is levelled at 0 dB there).
    let rate = f64::from(settings.sample_rate);
    let mut taps = filter::minimum_phase(&frequencies, &correction, rate, settings.taps);
    let (realised, largest) = filter::realised_db(&taps, rate, &frequencies);
    let corrected: Vec<f64> = measured.iter().zip(&realised).map(|(m, r)| m + r).collect();
    let normalisation = -power_level(&corrected, &frequencies).unwrap_or(0.0);
    let scale = 10_f64.powf(normalisation / 20.0);
    for tap in &mut taps {
        *tap *= scale;
    }
    let realised: Vec<f64> = realised.iter().map(|v| v + normalisation).collect();
    let predicted: Vec<f64> = measured.iter().zip(&realised).map(|(m, r)| m + r).collect();
    let deepest = frequencies
        .iter()
        .zip(&realised)
        .filter(|(f, _)| inside(**f))
        .map(|(_, v)| *v)
        .fold(0.0, f64::min);
    let evaluation = [result.low_limit_hz.map_or(low, |l| l.max(low)), high];
    result.evaluation_band_hz = Some([round(evaluation[0], 1), round(evaluation[1], 1)]);
    result.deviation_db = rms_shape_difference(&measured, &target, &frequencies, evaluation)
        .zip(rms_shape_difference(
            &predicted,
            &target,
            &frequencies,
            evaluation,
        ))
        .map(|(b, a)| [round(b, 2), round(a, 2)]);
    result.curves = Some(Curves {
        frequency_hz: measured_indices.iter().map(|&k| frequencies[k]).collect(),
        measured_db: curve(&measured, &measured_indices),
        target_db: curve(&target, &measured_indices),
        correction_db: curve(&correction, &measured_indices),
        filter_db: curve(&realised, &measured_indices),
        predicted_db: curve(&predicted, &measured_indices),
    });
    let headroom = largest + normalisation;
    if headroom > settings.max_boost_db + 0.5 {
        result.warnings.push(format!(
            "The filter boosts up to {headroom:.1} dB, more than the {:.1} dB asked.",
            settings.max_boost_db
        ));
    }
    result.filter = Some(Filter {
        sample_rate: settings.sample_rate,
        taps: taps.iter().map(|&v| v as f32).collect(),
        headroom_db: round(headroom, 2),
        deepest_cut_db: round(deepest, 2),
        normalisation_db: round(normalisation, 2),
    });
    Ok(result)
}
