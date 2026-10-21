//! Room-correction design on synthetic rooms: a known speaker, a comb from a floor
//! reflection and a room mode, at a few positions.
use calibration_core::room_correction::{
    DesignSettings, Measurement, MicrophoneCalibration, OutputDesign, OutputMeasurements, design,
    deviation, rms_from_target, smoothed_response,
};
use std::f64::consts::TAU;

const RATE: f64 = 48000.0;
/// Like a stored response: 100 ms before the anchor, 350 ms after it.
const ANCHOR: usize = 4800;
const LENGTH: usize = ANCHOR + 16800 + 1;

#[derive(Clone, Copy)]
enum Biquad {
    HighPass { hz: f64, q: f64 },
    Peak { hz: f64, q: f64, db: f64 },
}

/// RBJ cookbook biquads, run over `x` in place.
fn run(x: &mut [f64], filter: Biquad) {
    let (b, a) = match filter {
        Biquad::HighPass { hz, q } => {
            let w = TAU * hz / RATE;
            let alpha = w.sin() / (2.0 * q);
            let c = w.cos();
            (
                [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
                [1.0 + alpha, -2.0 * c, 1.0 - alpha],
            )
        }
        Biquad::Peak { hz, q, db } => {
            let g = 10_f64.powf(db / 40.0);
            let w = TAU * hz / RATE;
            let alpha = w.sin() / (2.0 * q);
            let c = w.cos();
            (
                [1.0 + alpha * g, -2.0 * c, 1.0 - alpha * g],
                [1.0 + alpha / g, -2.0 * c, 1.0 - alpha / g],
            )
        }
    };
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for v in x.iter_mut() {
        let y = (b[0] * *v + b[1] * x1 + b[2] * x2 - a[1] * y1 - a[2] * y2) / a[0];
        (x2, x1, y2, y1) = (x1, *v, y1, y);
        *v = y;
    }
}

struct Room {
    speaker: Vec<Biquad>,
    /// (delay ms, gain) after the direct sound.
    reflections: Vec<(f64, f64)>,
    /// (Hz, decay time constant s, amplitude per sample).
    mode: Option<(f64, f64, f64)>,
}
impl Room {
    fn response(&self) -> Vec<f32> {
        let mut x = vec![0.0; LENGTH];
        x[ANCHOR] = 1.0;
        for &(ms, gain) in &self.reflections {
            x[ANCHOR + (ms / 1000.0 * RATE).round() as usize] += gain;
        }
        if let Some((hz, tau, amplitude)) = self.mode {
            for (i, v) in x[ANCHOR..].iter_mut().enumerate() {
                let t = i as f64 / RATE;
                *v += amplitude * (-t / tau).exp() * (TAU * hz * t).cos();
            }
        }
        for &f in &self.speaker {
            run(&mut x, f);
        }
        x.iter().map(|&v| v as f32).collect()
    }
}

fn measurement(position: &str, main: bool, samples: Vec<f32>, band: [f64; 2]) -> Measurement {
    Measurement {
        position_id: position.into(),
        main_seat: main,
        rate: RATE,
        samples,
        anchor: ANCHOR,
        band_hz: band,
        calibration: None,
    }
}

/// A two-way speaker with a few faults, 2nd-order roll-off at 60 Hz.
fn speaker() -> Vec<Biquad> {
    vec![
        Biquad::HighPass { hz: 60.0, q: 0.707 },
        Biquad::Peak {
            hz: 250.0,
            q: 2.0,
            db: 5.0,
        },
        Biquad::Peak {
            hz: 3000.0,
            q: 1.5,
            db: -2.5,
        },
        Biquad::Peak {
            hz: 6000.0,
            q: 2.0,
            db: -7.0,
        },
    ]
}

fn room(reflection_ms: f64, level: f64) -> Room {
    Room {
        speaker: speaker()
            .into_iter()
            .chain([Biquad::Peak {
                hz: 1000.0,
                q: 0.1,
                db: level,
            }])
            .collect(),
        reflections: vec![(reflection_ms, 0.5)],
        // About +9 dB at 90 Hz, 5 Hz wide.
        mode: Some((90.0, 0.06, 0.0013)),
    }
}

/// The main seat twice, then four positions where the floor reflection arrives a
/// little earlier or later.
fn positions() -> OutputMeasurements {
    let band = [20.0, 20000.0];
    let mut measurements = vec![
        measurement("seat", true, room(2.3, 0.0).response(), band),
        measurement("seat", true, room(2.3, 0.0).response(), band),
    ];
    for (i, (ms, level)) in [(2.1, 0.3), (2.5, -0.3), (2.2, 0.0), (2.4, 0.2)]
        .into_iter()
        .enumerate()
    {
        measurements.push(measurement(
            &format!("seat/{i}"),
            false,
            room(ms, level).response(),
            band,
        ));
    }
    OutputMeasurements {
        output_id: "FL".into(),
        measurements,
    }
}

/// `samples` through the filter's taps, cut back to the stored length.
fn filtered(samples: &[f32], taps: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0_f64; samples.len()];
    for (i, &x) in samples.iter().enumerate() {
        if x == 0.0 {
            continue;
        }
        for (j, &h) in taps.iter().enumerate().take(samples.len() - i) {
            out[i + j] += f64::from(x) * f64::from(h);
        }
    }
    out.iter().map(|&v| v as f32).collect()
}

fn at(curve: &[f64], frequencies: &[f64], hz: f64) -> f64 {
    let i = frequencies
        .iter()
        .position(|&f| f >= hz)
        .expect("frequency on the grid");
    curve[i]
}

fn filter_db(d: &OutputDesign, hz: f64) -> f64 {
    let c = d.curves.as_ref().unwrap();
    let i = c.frequency_hz.iter().position(|&f| f >= hz).unwrap();
    f64::from(c.filter_db[i])
}

#[test]
fn the_filter_flattens_a_synthetic_room_within_its_limits() {
    let settings = DesignSettings::default();
    let output = positions();
    let d = design(&output, &settings).unwrap();
    assert!(d.refusal.is_none(), "{:?}", d.refusal);
    let filter = d.filter.as_ref().unwrap();
    assert_eq!(filter.taps.len(), 8192);
    assert_eq!(filter.sample_rate, 48000);
    // The main seat's own response, through the filter, measured as the design does.
    let main = &output.measurements[0];
    let before = smoothed_response(main, &settings).unwrap();
    let corrected = measurement(
        "seat",
        true,
        filtered(&main.samples, &filter.taps),
        main.band_hz,
    );
    let after = smoothed_response(&corrected, &settings).unwrap();
    let frequencies = settings.frequencies();
    let curves = d.curves.as_ref().unwrap();
    let target = |hz: f64| {
        let i = curves.frequency_hz.iter().position(|&f| f >= hz).unwrap();
        f64::from(curves.target_db[i])
    };
    // Faults within the limits are corrected: the 250 Hz peak (+5 dB), the 3 kHz dip
    // (−2.5 dB) and the room mode at 90 Hz (+8 dB, a cut, whose narrow top is finer
    // than 8192 taps resolve at 48 kHz).
    for (hz, within) in [(250.0, 1.0), (3000.0, 1.0), (90.0, 1.5)] {
        let (b, a) = (
            at(&before, &frequencies, hz) - target(hz),
            at(&after, &frequencies, hz) - target(hz),
        );
        assert!(
            a.abs() < within && a.abs() < b.abs() / 2.0,
            "{hz} Hz: {b:.2} -> {a:.2}"
        );
    }
    // The 6 kHz dip wants more than the boost limit: the filter stops at it.
    assert!(filter_db(&d, 6000.0) > 3.0);
    assert!(
        filter.headroom_db <= settings.max_boost_db + 0.2,
        "{filter:?}"
    );
    assert!(filter.deepest_cut_db >= -settings.max_cut_db - 0.5);
    // Overall, much closer to the target.
    let [b, a] = d.deviation_db.unwrap();
    assert!(a < 0.5 * b, "predicted {b} -> {a}");
    // Applied to the main seat's own response, it does about what it predicts.
    let [b, a] = deviation(&d, &before, &settings).unwrap();
    let applied = rms_from_target(&d, &after, &settings).unwrap();
    assert!(
        applied < 0.5 * b && (applied - a).abs() < 0.3,
        "{b} -> {a}, applied {applied}"
    );
    // Diagnostics: the floor reflection is listed, not corrected.
    assert!(
        d.reflections
            .iter()
            .any(|r| (2.2..2.4).contains(&r.delay_ms) && (r.extra_path_cm - 79.0).abs() < 4.0),
        "{:?}",
        d.reflections
    );
    assert_eq!(d.positions.len(), 5);
    assert!(d.positions[0].main_seat && d.positions[0].takes == 2);
    assert_eq!(d.repeatability_db, Some(0.0));
    assert!(
        d.positions.iter().all(|p| p.deviation_db.unwrap() < 1.5),
        "{:?}",
        d.positions
    );
    assert!(!d.warnings.iter().any(|w| w.contains("Positions")));
}

#[test]
fn the_filter_is_minimum_phase_and_keeps_the_level_at_500_hz_to_2_khz() {
    let settings = DesignSettings::default();
    let d = design(&positions(), &settings).unwrap();
    let filter = d.filter.as_ref().unwrap();
    let c = d.curves.as_ref().unwrap();
    // The level an alignment measures: mean power over 500 Hz–2 kHz on a log grid.
    let level = |curve: &[f32]| {
        let power: Vec<f64> = c
            .frequency_hz
            .iter()
            .zip(curve)
            .filter(|(f, _)| (500.0..=2000.0).contains(*f))
            .map(|(_, &v)| 10_f64.powf(f64::from(v) / 10.0))
            .collect();
        10.0 * (power.iter().sum::<f64>() / power.len() as f64).log10()
    };
    assert!(level(&c.measured_db).abs() < 0.01);
    assert!(
        level(&c.predicted_db).abs() < 0.02,
        "{}",
        level(&c.predicted_db)
    );
    // Of every filter with this magnitude, a minimum-phase one gathers its energy
    // fastest: faster than the same taps reversed, which have the same magnitude.
    let energy = |taps: &mut dyn Iterator<Item = f32>| -> Vec<f64> {
        let mut sum = 0.0;
        taps.map(|v| {
            sum += f64::from(v).powi(2);
            sum
        })
        .collect()
    };
    let forward = energy(&mut filter.taps.iter().copied());
    let backward = energy(&mut filter.taps.iter().rev().copied());
    let total = *forward.last().unwrap();
    for (n, (f, b)) in forward.iter().zip(&backward).enumerate() {
        assert!(*f >= *b - 1e-9 * total, "tap {n}");
    }
    assert!(forward[480] / total > 0.95, "{}", forward[480] / total);
}

#[test]
fn nothing_is_boosted_where_the_speaker_cannot_play_and_the_band_edges_fade_out() {
    // A small speaker, 4th order at 80 Hz: −6 dB at 69.8 Hz. Its response wants
    // boosting all the way down; the design must not.
    let small = Room {
        speaker: vec![
            Biquad::HighPass { hz: 80.0, q: 0.541 },
            Biquad::HighPass { hz: 80.0, q: 1.307 },
            Biquad::Peak {
                hz: 9000.0,
                q: 1.0,
                db: 4.0,
            },
        ],
        reflections: vec![],
        mode: None,
    };
    let output = OutputMeasurements {
        output_id: "TBL".into(),
        measurements: vec![measurement("seat", true, small.response(), [20.0, 20000.0])],
    };
    let settings = DesignSettings::default();
    let d = design(&output, &settings).unwrap();
    let limit = d.low_limit_hz.unwrap();
    assert!((limit / 69.8 - 1.0).abs() < 0.1, "{limit}");
    let c = d.curves.as_ref().unwrap();
    for (f, v) in c.frequency_hz.iter().zip(&c.filter_db) {
        if *f <= limit {
            assert!(*v <= 0.1, "{f} Hz boosted by {v}");
        }
        // Outside the band (10 kHz plus half an octave fade), nothing.
        if *f >= 10_000.0 * 2_f64.sqrt() * 1.02 || *f < 20.0 {
            assert!(v.abs() < 0.25, "{f} Hz: {v}");
        }
    }
    // The 9 kHz peak is cut; one position is a warning, not a refusal.
    assert!(filter_db(&d, 9000.0) < -2.5);
    assert!(d.refusal.is_none());
    assert!(d.warnings.iter().any(|w| w.contains("one position")));
    assert!(d.warnings.iter().any(|w| w.contains("calibration")));
}

#[test]
fn positions_that_disagree_are_refused_and_a_subwoofer_is_left_alone() {
    let band = [20.0, 20000.0];
    let mut measurements = vec![measurement("seat", true, room(2.3, 0.0).response(), band)];
    for (i, hz) in [400.0, 2500.0, 7000.0].into_iter().enumerate() {
        let mut r = room(2.3, 0.0);
        r.speaker.push(Biquad::Peak {
            hz,
            q: 0.5,
            db: if i % 2 == 0 { 12.0 } else { -12.0 },
        });
        measurements.push(measurement(&format!("seat/{i}"), false, r.response(), band));
    }
    let output = OutputMeasurements {
        output_id: "C".into(),
        measurements,
    };
    let d = design(&output, &DesignSettings::default()).unwrap();
    assert!(d.filter.is_none());
    assert!(d.refusal.as_deref().unwrap().contains("disagree"), "{d:?}");
    assert_eq!(d.positions.len(), 4);

    let sub = OutputMeasurements {
        output_id: "LFE".into(),
        measurements: vec![measurement(
            "seat",
            true,
            room(2.3, 0.0).response(),
            [20.0, 120.0],
        )],
    };
    let d = design(&sub, &DesignSettings::default()).unwrap();
    assert!(d.filter.is_none());
    assert!(d.refusal.as_deref().unwrap().contains("500 Hz–2 kHz"));
}

#[test]
fn the_microphone_calibration_is_subtracted_and_must_be_the_same_everywhere() {
    let flat = Room {
        speaker: vec![Biquad::HighPass { hz: 40.0, q: 0.707 }],
        reflections: vec![],
        mode: None,
    };
    // The microphone reads 3 dB high above 6 kHz: the speaker is 3 dB quieter there.
    let calibration = MicrophoneCalibration {
        id: "a".into(),
        points: vec![[20.0, 0.0], [4000.0, 0.0], [6000.0, 3.0], [20000.0, 3.0]],
    };
    let mut m = measurement("seat", true, flat.response(), [20.0, 20000.0]);
    m.calibration = Some(calibration.clone());
    let settings = DesignSettings::default();
    let d = design(
        &OutputMeasurements {
            output_id: "FR".into(),
            measurements: vec![m.clone()],
        },
        &settings,
    )
    .unwrap();
    assert!(d.calibrated);
    assert!(!d.warnings.iter().any(|w| w.contains("calibration")));
    let c = d.curves.as_ref().unwrap();
    let i = c.frequency_hz.iter().position(|&f| f >= 8000.0).unwrap();
    assert!(
        (f64::from(c.measured_db[i]) + 3.0).abs() < 0.2,
        "{}",
        c.measured_db[i]
    );
    // Target at 8 kHz: −3 dB/decade above 1 kHz, levelled → about −2.6 dB; needs a
    // small boost.
    assert!(f64::from(c.filter_db[i]) > 0.0);

    let mut other = m.clone();
    other.calibration = Some(MicrophoneCalibration {
        id: "b".into(),
        ..calibration
    });
    let d = design(
        &OutputMeasurements {
            output_id: "FR".into(),
            measurements: vec![m, other],
        },
        &settings,
    )
    .unwrap();
    assert!(d.refusal.as_deref().unwrap().contains("calibrations"));
}

#[test]
fn settings_and_responses_are_validated() {
    let short = DesignSettings {
        taps: 100,
        ..Default::default()
    };
    assert!(design(&positions(), &short).is_err());
    let nonfinite = DesignSettings {
        max_boost_db: f64::NAN,
        ..Default::default()
    };
    assert!(nonfinite.validate().is_err());
    let mut output = positions();
    output.measurements[1].samples[10] = f32::NAN;
    assert!(design(&output, &DesignSettings::default()).is_err());
    let empty = OutputMeasurements {
        output_id: "FL".into(),
        measurements: vec![],
    };
    assert!(
        design(&empty, &DesignSettings::default())
            .unwrap()
            .refusal
            .is_some()
    );
    // Other rates and lengths.
    let settings = DesignSettings {
        sample_rate: 96000,
        taps: 4096,
        ..Default::default()
    };
    let d = design(&positions(), &settings).unwrap();
    assert_eq!(d.filter.as_ref().unwrap().taps.len(), 4096);
    assert_eq!(d.filter.as_ref().unwrap().sample_rate, 96000);
}
