//! A subwoofer's mode correction: cuts on the peaks of its spatially averaged response.
//!
//! The speakers' design ([`super::design`]) is levelled at 500 Hz–2 kHz, which a
//! subwoofer never plays. Here the level is the subwoofer's own over its band (mean power
//! on the log grid), the target is flat at that level, and the correction cuts what
//! stands above it, little or nothing is boosted: a modal dip moves with the listener and
//! costs power to fill. The responses are windowed as the speakers' are below their modal
//! region (300 ms), power-averaged over the positions with the main seat weighing more,
//! and smoothed finely. The filter is minimum phase, as the speakers' are; its level is
//! left as it is, cuts only, so the level is set afterwards by bass integration
//! ([`crate::bass`]) through the output's gain.
//!
//! Pure and offline: no file, device or network access, never in an audio callback.
use super::{
    DesignSettings, FADE_OCTAVES, OutputDesign, OutputMeasurements, PositionAgreement, curve, fade,
    filter, round,
    spectrum::{self, band_mean, decibels, rms_difference, rms_shape_difference, smooth},
};
use crate::{Result, require};
use serde::{Deserialize, Serialize};

pub const SUBWOOFER_METHOD: &str = "fdw-spatial-average-minimum-phase-cut-only-v1";
/// The correction band starts and ends this far inside the measured range.
const EDGE_OCTAVES: f64 = 1.0 / 12.0;
/// A take whose median signal-to-noise ratio over its band is below this is left out.
const MIN_SNR_DB: f64 = 20.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubwooferSettings {
    /// The engine's rate: the filter's taps are at this rate.
    pub sample_rate: u32,
    /// Long enough to resolve modes a few hertz apart: 32768 taps resolve 1.5 Hz at 48 kHz.
    pub taps: usize,
    /// The deepest cut, as a positive number of dB.
    pub max_cut_db: f64,
    /// The largest boost; 0 cuts only.
    pub max_boost_db: f64,
    /// The main seat's weight in the spatial average; every other position weighs 1.
    pub main_seat_weight: f64,
    /// Smoothing of the averaged response, in octaves.
    pub smoothing_octaves: f64,
}
impl Default for SubwooferSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            taps: 32768,
            max_cut_db: 12.0,
            max_boost_db: 0.0,
            main_seat_weight: 2.0,
            smoothing_octaves: 1.0 / 24.0,
        }
    }
}
impl SubwooferSettings {
    pub fn validate(&self) -> Result<()> {
        require(
            [
                self.max_cut_db,
                self.max_boost_db,
                self.main_seat_weight,
                self.smoothing_octaves,
            ]
            .iter()
            .all(|v| v.is_finite()),
            "subwoofer settings must be finite",
        )?;
        require(
            (8000..=384_000).contains(&self.sample_rate),
            "filter rate must be 8–384 kHz",
        )?;
        require(
            (1024..=65_536).contains(&self.taps)
                && self.taps as f64 <= 1.5 * f64::from(self.sample_rate),
            "subwoofer filter length must be 1024–65536 taps and at most 1.5 s",
        )?;
        require(
            (0.0..=30.0).contains(&self.max_cut_db),
            "cut limit must be 0–30 dB",
        )?;
        require(
            (0.0..=3.0).contains(&self.max_boost_db),
            "subwoofer boost limit must be 0–3 dB",
        )?;
        require(
            (1.0..=10.0).contains(&self.main_seat_weight),
            "main-seat weight must be 1–10",
        )?;
        require(
            (1.0 / 48.0..=1.0 / 3.0).contains(&self.smoothing_octaves),
            "subwoofer smoothing must be 1/48–1/3 octave",
        )
    }
    /// The speakers' design settings that drive the shared windowing: the whole band is
    /// modal, so the window is the longest at every frequency.
    fn windowing(&self) -> DesignSettings {
        DesignSettings {
            sample_rate: self.sample_rate,
            taps: self.taps,
            modal_hz: 1000.0,
            ..DesignSettings::default()
        }
    }
}

/// Design the mode correction of a subwoofer from its direct responses: the takes of its
/// own sweep at the main seat and the positions around it.
pub fn design_subwoofer(
    output: &OutputMeasurements,
    settings: &SubwooferSettings,
) -> Result<OutputDesign> {
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
    let windowing = settings.windowing();
    let mut noisy = vec![];
    let measurements: Vec<_> = output
        .measurements
        .iter()
        .filter(|m| match m.median_snr_db(m.band_hz) {
            Some(snr) if snr < MIN_SNR_DB => {
                noisy.push(format!("{} ({snr:.0} dB)", m.position_id));
                false
            }
            _ => true,
        })
        .collect();
    if !noisy.is_empty() {
        result.warnings.push(format!(
            "Takes too noisy to design from (signal/noise over the subwoofer's band below {MIN_SNR_DB:.0} dB) left out: {}.",
            noisy.join(", ")
        ));
    }
    if measurements.is_empty() {
        return refuse(
            result,
            if noisy.is_empty() {
                "No accepted subwoofer response to design from."
            } else {
                "Every subwoofer take is too noisy to design from: measure it again louder."
            },
        );
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
    let frequencies = windowing.frequencies();
    let powers: Vec<Vec<f64>> = measurements
        .iter()
        .map(|m| {
            let centre = (m.band_hz[0] * m.band_hz[1]).sqrt();
            let band = m.usable_band_around(centre);
            m.power_in(&windowing, &frequencies, band)
        })
        .collect();
    let mut positions: Vec<(String, bool, Vec<usize>)> = vec![];
    for (i, m) in measurements.iter().enumerate() {
        match positions.iter_mut().find(|p| p.0 == m.position_id) {
            Some(p) => p.2.push(i),
            None => positions.push((m.position_id.clone(), m.main_seat, vec![i])),
        }
    }
    positions.sort_by_key(|p| !p.1);
    let position_power: Vec<Vec<f64>> = positions
        .iter()
        .map(|p| {
            (0..frequencies.len())
                .map(|k| p.2.iter().map(|&i| powers[i][k]).sum::<f64>() / p.2.len() as f64)
                .collect()
        })
        .collect();
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
    let smoothed = smooth(&average, &frequencies, |_| settings.smoothing_octaves);
    let measured_indices: Vec<usize> = (0..frequencies.len())
        .filter(|&k| smoothed[k].is_finite())
        .collect();
    let Some((&first, &last)) = measured_indices.first().zip(measured_indices.last()) else {
        return refuse(result, "Its responses share no measured band.");
    };
    let low = frequencies[first] * 2_f64.powf(EDGE_OCTAVES);
    let high = frequencies[last] / 2_f64.powf(EDGE_OCTAVES);
    if high < 2.0 * low {
        return refuse(
            result,
            "Its measured band is narrower than an octave: too narrow to correct.",
        );
    }
    result.band_hz = Some([round(low, 1), round(high, 1)]);
    let band = [low, high];
    let raw_db: Vec<f64> = smoothed
        .iter()
        .map(|&p| if p.is_finite() { decibels(p) } else { f64::NAN })
        .collect();
    let power: Vec<f64> = raw_db.iter().map(|v| 10_f64.powf(v / 10.0)).collect();
    let Some(level) = band_mean(&power, &frequencies, band).map(decibels) else {
        return refuse(result, "Its measured band is empty.");
    };
    let measured: Vec<f64> = raw_db.iter().map(|v| v - level).collect();

    // How the positions agree, over the band.
    let curves: Vec<Vec<f64>> = position_power
        .iter()
        .map(|p| {
            let smoothed = smooth(p, &frequencies, |_| settings.smoothing_octaves);
            let db: Vec<f64> = smoothed.iter().map(|&v| decibels(v)).collect();
            let power: Vec<f64> = db.iter().map(|v| 10_f64.powf(v / 10.0)).collect();
            let level = band_mean(&power, &frequencies, band).map_or(0.0, decibels);
            db.iter().map(|v| v - level).collect()
        })
        .collect();
    for (p, curve) in positions.iter().zip(&curves) {
        result.positions.push(PositionAgreement {
            position_id: p.0.clone(),
            main_seat: p.1,
            takes: p.2.len(),
            deviation_db: rms_difference(curve, &measured, &frequencies, band).map(|v| round(v, 2)),
        });
    }
    if !result.calibrated {
        result.warnings.push(
            "No microphone calibration: the correction includes the microphone's own response."
                .into(),
        );
    }
    if positions.len() == 1 {
        result.warnings.push(
            "Designed from one position only: measure the other positions for cuts that hold around the seat."
                .into(),
        );
    }

    // Cut what stands above the level, little or no boost, faded to 0 dB outside.
    let inside = |f: f64| (low..=high).contains(&f);
    let limited = |k: usize| (-measured[k]).clamp(-settings.max_cut_db, settings.max_boost_db);
    let first_inside = frequencies.iter().position(|&f| inside(f)).unwrap_or(0);
    let last_inside = frequencies.iter().rposition(|&f| inside(f)).unwrap_or(0);
    let (edge_low, edge_high) = (limited(first_inside), limited(last_inside));
    let rate = f64::from(settings.sample_rate);
    let resolution_hz = rate / settings.taps as f64;
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
    correction = spectrum::smooth_db(&correction, &frequencies, |hz| {
        (2.0 * ((hz + resolution_hz / 2.0) / hz).log2()).max(settings.smoothing_octaves)
    });
    for value in &mut correction {
        *value = value.clamp(-settings.max_cut_db, settings.max_boost_db);
    }
    let taps = filter::minimum_phase(&frequencies, &correction, rate, settings.taps);
    let (realised, largest) = filter::realised_db(&taps, rate, &frequencies);
    let predicted: Vec<f64> = measured.iter().zip(&realised).map(|(m, r)| m + r).collect();
    let deepest = frequencies
        .iter()
        .zip(&realised)
        .filter(|(f, _)| inside(**f))
        .map(|(_, v)| *v)
        .fold(0.0, f64::min);
    let flat = vec![0.0; frequencies.len()];
    result.evaluation_band_hz = result.band_hz;
    result.deviation_db = rms_shape_difference(&measured, &flat, &frequencies, band)
        .zip(rms_shape_difference(&predicted, &flat, &frequencies, band))
        .map(|(b, a)| [round(b, 2), round(a, 2)]);
    result.curves = Some(super::Curves {
        frequency_hz: measured_indices.iter().map(|&k| frequencies[k]).collect(),
        measured_db: curve(&measured, &measured_indices),
        target_db: curve(&flat, &measured_indices),
        correction_db: curve(&correction, &measured_indices),
        filter_db: curve(&realised, &measured_indices),
        predicted_db: curve(&predicted, &measured_indices),
    });
    if largest > settings.max_boost_db + 0.5 {
        result.warnings.push(format!(
            "The filter boosts up to {largest:.1} dB, more than the {:.1} dB asked.",
            settings.max_boost_db
        ));
    }
    result.filter = Some(super::Filter {
        sample_rate: settings.sample_rate,
        taps: taps.iter().map(|&v| v as f32).collect(),
        headroom_db: round(largest, 2),
        deepest_cut_db: round(deepest, 2),
        normalisation_db: 0.0,
    });
    Ok(result)
}
