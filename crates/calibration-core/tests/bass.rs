//! Bass integration on synthetic rooms: a subwoofer and speakers split by Linkwitz-Riley
//! crossovers as Omniphony splits them, with known delays, polarities, levels and modes.
use calibration_core::{
    bass::{self, Filter, PositionResponses, Response, Settings, SpeakerResponses, Subwoofer},
    room_correction::{Measurement, OutputMeasurements, SubwooferSettings, design_subwoofer},
};

const RATE: f64 = 48000.0;
/// What a stored response keeps around its anchor.
const BEFORE_S: f64 = 0.1;
const AFTER_S: f64 = 0.35;

/// A biquad's coefficients `[b0, b1, b2, a1, a2]`.
type Biquad = [f64; 5];

/// Second-order Butterworth low- and high-pass by the bilinear transform, as the
/// renderer's crossover builds them.
fn butterworth(fc: f64, high: bool) -> Biquad {
    let k = (std::f64::consts::PI * fc / RATE).tan();
    let q = std::f64::consts::SQRT_2;
    let norm = 1.0 + k / q + k * k;
    let a1 = 2.0 * (k * k - 1.0) / norm;
    let a2 = (1.0 - k / q + k * k) / norm;
    if high {
        let b0 = 1.0 / norm;
        [b0, -2.0 * b0, b0, a1, a2]
    } else {
        let b0 = k * k / norm;
        [b0, 2.0 * b0, b0, a1, a2]
    }
}

/// A peaking resonance (a room mode) of `gain_db` at `hz`.
fn peak(hz: f64, q: f64, gain_db: f64) -> Biquad {
    let a = 10_f64.powf(gain_db / 40.0);
    let w = std::f64::consts::TAU * hz / RATE;
    let alpha = w.sin() / (2.0 * q);
    let a0 = 1.0 + alpha / a;
    [
        (1.0 + alpha * a) / a0,
        -2.0 * w.cos() / a0,
        (1.0 - alpha * a) / a0,
        -2.0 * w.cos() / a0,
        (1.0 - alpha / a) / a0,
    ]
}

fn run(x: &[f64], [b0, b1, b2, a1, a2]: Biquad) -> Vec<f64> {
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    x.iter()
        .map(|&v| {
            let y = b0 * v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, v, y1, y);
            y
        })
        .collect()
}

/// The impulse response of `filters` in series, `seconds` long.
fn impulse(filters: &[Biquad], seconds: f64) -> Vec<f64> {
    let mut x = vec![0.0; (seconds * RATE) as usize];
    x[0] = 1.0;
    filters.iter().fold(x, |x, &f| run(&x, f))
}

/// Linkwitz-Riley 4th order at `fc`: two Butterworths in series.
fn lr4(fc: f64, high: bool) -> [Biquad; 2] {
    [butterworth(fc, high), butterworth(fc, high)]
}

/// A path: its impulse response, gain and arrival on the position's time base.
#[derive(Clone)]
struct Path {
    ir: Vec<f64>,
    gain: f64,
    arrival_s: f64,
}

/// The response of `paths` together, stored around `anchor_s` as a take stores it.
fn response(paths: &[&Path], anchor_s: f64, seed: u32) -> Response {
    let start_s = anchor_s - BEFORE_S;
    let frames = ((BEFORE_S + AFTER_S) * RATE) as usize;
    let mut samples = vec![0.0_f64; frames];
    for path in paths {
        let offset = ((path.arrival_s - start_s) * RATE).round() as i64;
        for (i, &v) in path.ir.iter().enumerate() {
            let at = offset + i as i64;
            if (0..frames as i64).contains(&at) {
                samples[at as usize] += path.gain * v;
            }
        }
    }
    let mut state = seed.wrapping_mul(2_654_435_761).max(1);
    Response {
        rate: RATE,
        samples: samples
            .iter()
            .map(|&v| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (v + 1e-5 * (f64::from(state) / f64::from(u32::MAX) - 0.5)) as f32
            })
            .collect(),
        start_s,
        anchor_s,
    }
}

/// A speaker and the subwoofer's part of its bass-managed sweep at one position.
struct Room {
    speaker: Path,
    subwoofer: Path,
}
impl Room {
    /// Ideal drivers: only the crossover shapes them.
    fn ideal(fc: f64, speaker_s: f64, subwoofer_s: f64, sub_gain: f64) -> Self {
        Self {
            speaker: Path {
                ir: impulse(&lr4(fc, true), 0.3),
                gain: 0.5,
                arrival_s: speaker_s,
            },
            subwoofer: Path {
                ir: impulse(&lr4(fc, false), 0.3),
                gain: 0.5 * sub_gain,
                arrival_s: subwoofer_s,
            },
        }
    }
    fn position(&self, id: &str, main_seat: bool, seed: u32) -> PositionResponses {
        PositionResponses {
            position_id: id.into(),
            main_seat,
            speaker: response(&[&self.speaker], self.speaker.arrival_s, seed),
            subwoofer: Some(response(
                &[&self.subwoofer],
                self.subwoofer.arrival_s,
                seed + 1,
            )),
            sum: response(
                &[&self.speaker, &self.subwoofer],
                self.speaker.arrival_s,
                seed + 2,
            ),
        }
    }
}

fn speaker(id: &str, fc: f64, positions: Vec<PositionResponses>) -> SpeakerResponses {
    SpeakerResponses {
        output_id: id.into(),
        crossover_hz: fc,
        swept_hz: [20.0, 500.0],
        positions,
    }
}

fn subwoofer(delay_ms: f64) -> Subwoofer {
    Subwoofer {
        output_id: "LFE".into(),
        delay_ms,
        gain_db: 0.0,
        others_max_delay_ms: 10.0,
        max_delay_ms: 250.0,
    }
}

#[test]
fn a_late_subwoofer_is_brought_forward_and_then_sums_in_phase() {
    // The subwoofer's part arrives 6 ms after the speaker's.
    let room = Room::ideal(80.0, 0.003, 0.009, 1.0);
    let speakers = [speaker("FL", 80.0, vec![room.position("main", true, 1)])];
    let proposal = bass::propose(&speakers, &subwoofer(20.0), None, &Settings::default()).unwrap();
    assert!(proposal.refusal.is_none(), "{:?}", proposal.refusal);
    let result = &proposal.speakers[0];
    assert!(result.excluded.is_none(), "{:?}", result.excluded);
    // The phase difference names the lateness.
    let phase = result.phase_delay_ms.unwrap();
    assert!((phase - 6.0).abs() < 0.2, "phase delay {phase}");
    assert_eq!(result.phase_inverted, Some(false));
    assert!(
        (proposal.setting.delay_change_ms + 6.0).abs() < 0.1,
        "{:?}",
        proposal.setting
    );
    assert!(!proposal.setting.inverted);
    assert!((proposal.subwoofer_delay_ms - 14.0).abs() < 0.1);
    assert_eq!(proposal.others_delay_ms, 0.0);
    assert!(
        proposal.setting.gain_change_db.abs() < 0.3,
        "{:?}",
        proposal.setting
    );
    // Measured, the late subwoofer loses several dB around the crossover; aligned, the
    // Linkwitz-Riley parts add up in phase.
    let [measured, best, proposed] = result.summation_db.unwrap();
    assert!(measured < -1.5, "measured summation {measured}");
    assert!(proposed > -0.1 && best > -0.1, "{best} {proposed}");
    let [before, after] = result.sum_level_db.unwrap();
    assert!(after > before + 1.0, "{before} → {after}");
}

#[test]
fn an_inverted_subwoofer_is_inverted_back_and_its_level_matched() {
    // In time, but wired in opposite polarity and 6 dB too loud.
    let room = Room::ideal(80.0, 0.004, 0.004, -2.0);
    let speakers = [speaker("C", 80.0, vec![room.position("main", true, 7)])];
    let proposal = bass::propose(&speakers, &subwoofer(10.0), None, &Settings::default()).unwrap();
    let result = &proposal.speakers[0];
    assert_eq!(result.phase_inverted, Some(true));
    assert!(proposal.setting.inverted);
    assert!(
        proposal.setting.delay_change_ms.abs() < 0.1,
        "{:?}",
        proposal.setting
    );
    let gain = proposal.setting.gain_change_db;
    assert!((gain + 6.02).abs() < 0.3, "gain change {gain}");
    assert!((proposal.subwoofer_gain_db - gain).abs() < 1e-9);
    let [_, after] = result.balance_db.unwrap();
    assert!(after.abs() < 0.2, "balance after {after}");
}

#[test]
fn a_filter_that_inverts_is_part_of_the_prediction() {
    // Wired inverted, and the mode correction already inverts it back.
    let room = Room::ideal(80.0, 0.004, 0.004, -1.0);
    let speakers = [speaker("C", 80.0, vec![room.position("main", true, 3)])];
    let filter = Filter {
        sample_rate: RATE,
        taps: vec![-1.0],
    };
    let proposal = bass::propose(
        &speakers,
        &subwoofer(10.0),
        Some(&filter),
        &Settings::default(),
    )
    .unwrap();
    assert!(!proposal.setting.inverted, "{:?}", proposal.setting);
    assert!(proposal.mode_correction);
    assert!(proposal.speakers[0].summation_db.unwrap()[2] > -0.1);
}

#[test]
fn a_subwoofer_late_even_without_delay_delays_every_other_output() {
    let room = Room::ideal(60.0, 0.002, 0.007, 1.0);
    let speakers = [speaker("FR", 60.0, vec![room.position("main", true, 11)])];
    let proposal = bass::propose(&speakers, &subwoofer(1.0), None, &Settings::default()).unwrap();
    assert!((proposal.setting.delay_change_ms + 5.0).abs() < 0.1);
    assert_eq!(proposal.subwoofer_delay_ms, 0.0);
    assert!((proposal.others_delay_ms - 4.0).abs() < 0.1, "{proposal:?}");
    assert!(
        proposal.warnings.iter().any(|w| w.contains("latency")),
        "{:?}",
        proposal.warnings
    );
}

#[test]
fn the_timing_estimator_reads_band_limited_parts_with_a_mode() {
    // A subwoofer with its own roll-off and a mode in the crossover band: the phase
    // difference still gives its lateness within a fraction of a period.
    let fc = 70.0;
    let mut sub = lr4(fc, false).to_vec();
    sub.push(peak(55.0, 4.0, 6.0));
    let room = Room {
        speaker: Path {
            ir: impulse(&lr4(fc, true), 0.3),
            gain: 0.5,
            arrival_s: 0.002,
        },
        subwoofer: Path {
            ir: impulse(&sub, 0.3),
            gain: 0.5,
            arrival_s: 0.0115,
        },
    };
    let speakers = [speaker("BR", fc, vec![room.position("main", true, 5)])];
    let proposal = bass::propose(&speakers, &subwoofer(30.0), None, &Settings::default()).unwrap();
    let result = &proposal.speakers[0];
    let phase = result.phase_delay_ms.unwrap();
    assert!((phase - 9.5).abs() < 1.0, "phase delay {phase}");
    assert!(result.phase_fit_deg.unwrap() < 30.0);
    // The sum is still brought to nearly perfect addition.
    let [measured, _, proposed] = result.summation_db.unwrap();
    assert!(
        proposed > measured + 1.0 && proposed > -0.5,
        "{measured} → {proposed}"
    );
}

#[test]
fn positions_that_disagree_get_a_compromise_and_a_warning() {
    // At the main seat the subwoofer is 1 ms late; 30 cm away it is 3 ms early: more
    // than a quarter period of the crossover apart.
    let main = Room::ideal(80.0, 0.003, 0.004, 1.0);
    let aside = Room::ideal(80.0, 0.006, 0.003, 1.0);
    let speakers = [speaker(
        "FL",
        80.0,
        vec![
            main.position("main", true, 1),
            aside.position("main/left-30cm", false, 4),
        ],
    )];
    let proposal = bass::propose(&speakers, &subwoofer(20.0), None, &Settings::default()).unwrap();
    let result = &proposal.speakers[0];
    let best: Vec<f64> = result
        .position_best
        .iter()
        .map(|b| b.delay_change_ms)
        .collect();
    assert!(
        (best[0] + 1.0).abs() < 0.2 && (best[1] - 3.0).abs() < 0.2,
        "{best:?}"
    );
    // Between the two, nearer the main seat, which counts twice.
    let delay = proposal.setting.delay_change_ms;
    assert!(delay > -1.0 && delay < 1.0, "compromise {delay}");
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.starts_with("Its positions disagree")),
        "{:?}",
        result.warnings
    );
    assert!(proposal.warnings.iter().any(|w| w.contains("compromise")));
}

#[test]
fn speakers_with_different_crossovers_share_one_setting() {
    let front = Room::ideal(40.0, 0.003, 0.008, 1.0);
    let side = Room::ideal(80.0, 0.004, 0.009, 1.0);
    let speakers = [
        speaker("FL", 40.0, vec![front.position("main", true, 1)]),
        speaker("SL", 80.0, vec![side.position("main", true, 2)]),
    ];
    let proposal = bass::propose(&speakers, &subwoofer(20.0), None, &Settings::default()).unwrap();
    // Both are 5 ms late: one setting suits both.
    assert!((proposal.setting.delay_change_ms + 5.0).abs() < 0.2);
    for result in &proposal.speakers {
        assert!(result.summation_db.unwrap()[2] > -0.2, "{result:?}");
        assert!((result.best.unwrap().delay_change_ms + 5.0).abs() < 0.2);
    }
}

#[test]
fn a_speaker_whose_bass_goes_elsewhere_or_whose_sum_does_not_add_up_is_left_out() {
    // The subwoofer plays a twentieth of this speaker's low band.
    let quiet = Room::ideal(110.0, 0.003, 0.003, 0.05);
    let mut other_bass = quiet.speaker.clone();
    other_bass.ir = impulse(&lr4(110.0, false), 0.3);
    let mut quiet_position = quiet.position("main", true, 1);
    quiet_position.speaker = response(&[&quiet.speaker, &other_bass], 0.003, 9);
    quiet_position.sum = response(&[&quiet.speaker, &other_bass, &quiet.subwoofer], 0.003, 10);
    // This one's sum was measured with the subwoofer 4 ms later than its own part.
    let moved = Room::ideal(80.0, 0.003, 0.003, 1.0);
    let mut moved_position = moved.position("main", true, 2);
    let mut later = moved.subwoofer.clone();
    later.arrival_s += 0.004;
    moved_position.sum = response(&[&moved.speaker, &later], 0.003, 12);
    let good = Room::ideal(80.0, 0.003, 0.006, 1.0);
    let speakers = [
        speaker("TFL", 110.0, vec![quiet_position]),
        speaker("SR", 80.0, vec![moved_position]),
        speaker("FR", 80.0, vec![good.position("main", true, 3)]),
    ];
    let proposal = bass::propose(&speakers, &subwoofer(20.0), None, &Settings::default()).unwrap();
    let excluded = |id: &str| {
        proposal
            .speakers
            .iter()
            .find(|s| s.output_id == id)
            .unwrap()
            .excluded
            .clone()
    };
    assert!(excluded("TFL").unwrap().contains("other speakers"));
    assert!(excluded("SR").unwrap().contains("differs from its parts"));
    assert!(excluded("FR").is_none());
    // Only FR decides.
    assert!((proposal.setting.delay_change_ms + 3.0).abs() < 0.1);
    let fr = &proposal.speakers[2];
    assert!(fr.consistency_db.unwrap() < 0.2);
}

#[test]
fn nothing_is_proposed_without_a_usable_speaker() {
    let room = Room::ideal(80.0, 0.003, 0.003, 1.0);
    let mut narrow = speaker("FL", 80.0, vec![room.position("main", true, 1)]);
    narrow.swept_hz = [70.0, 500.0];
    let proposal = bass::propose(&[narrow], &subwoofer(0.0), None, &Settings::default()).unwrap();
    assert!(proposal.refusal.is_some());
    assert!(
        proposal.speakers[0]
            .excluded
            .as_deref()
            .unwrap()
            .contains("does not cover")
    );
    assert_eq!(proposal.setting, bass::Setting::UNCHANGED);
}

#[test]
fn the_timing_reference_uses_the_sum_minus_its_own_part() {
    let room = Room::ideal(80.0, 0.0, 0.004, 1.0);
    let mut position = room.position("main", true, 1);
    position.subwoofer = None;
    let proposal = bass::propose(
        &[speaker("C", 80.0, vec![position])],
        &subwoofer(20.0),
        None,
        &Settings::default(),
    )
    .unwrap();
    let result = &proposal.speakers[0];
    assert!(result.consistency_db.is_none());
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("timing reference"))
    );
    assert!((proposal.setting.delay_change_ms + 4.0).abs() < 0.1);
}

#[test]
fn verification_compares_the_new_sum_with_the_prediction() {
    let room = Room::ideal(80.0, 0.003, 0.009, 1.0);
    let speakers = [speaker("FL", 80.0, vec![room.position("main", true, 1)])];
    let settings = Settings::default();
    let proposal = bass::propose(&speakers, &subwoofer(20.0), None, &settings).unwrap();
    // Applied: the subwoofer now arrives with the speaker.
    let mut applied = room.subwoofer.clone();
    applied.arrival_s += proposal.setting.delay_change_ms / 1000.0;
    let now = response(&[&room.speaker, &applied], 0.003, 21);
    let check = bass::verify(
        &speakers,
        &proposal,
        None,
        &[("FL".into(), "main".into(), now)],
        &settings,
    )
    .unwrap();
    let [before, predicted, measured] = check.speakers[0].sum_level_db;
    assert!(
        (predicted - measured).abs() < 0.1,
        "{predicted} vs {measured}"
    );
    assert!(measured > before + 1.0);
    assert!(check.speakers[0].deviation_db < 0.2);
    assert!(check.warnings.is_empty(), "{:?}", check.warnings);
    // Applied wrongly (still late): the check says so.
    let wrong = response(&[&room.speaker, &room.subwoofer], 0.003, 22);
    let check = bass::verify(
        &speakers,
        &proposal,
        None,
        &[("FL".into(), "main".into(), wrong)],
        &settings,
    )
    .unwrap();
    assert!(check.speakers[0].deviation_db > 1.0);
    assert!(!check.warnings.is_empty());
}

/// A subwoofer's direct response stored as a take stores it.
fn subwoofer_take(position: &str, main_seat: bool, mode_db: f64) -> Measurement {
    let mut filters = vec![butterworth(15.0, true), butterworth(150.0, false)];
    filters.push(peak(45.0, 5.0, mode_db));
    filters.push(peak(90.0, 6.0, -4.0));
    let ir = impulse(&filters, 0.4);
    let anchor = (BEFORE_S * RATE) as usize;
    let mut samples = vec![0.0_f32; anchor];
    samples.extend(ir.iter().take((AFTER_S * RATE) as usize).map(|&v| v as f32));
    Measurement {
        position_id: position.into(),
        main_seat,
        rate: RATE,
        samples,
        anchor,
        band_hz: [20.0, 120.0],
        calibration: None,
        snr_db: None,
    }
}

#[test]
fn the_subwoofer_mode_correction_cuts_its_peak_and_boosts_nothing() {
    let output = OutputMeasurements {
        output_id: "LFE".into(),
        measurements: vec![
            subwoofer_take("main", true, 9.0),
            subwoofer_take("main/left-30cm", false, 7.0),
            subwoofer_take("main/right-30cm", false, 8.0),
        ],
    };
    let settings = SubwooferSettings::default();
    let design = design_subwoofer(&output, &settings).unwrap();
    assert!(design.refusal.is_none(), "{:?}", design.refusal);
    let filter = design.filter.as_ref().unwrap();
    assert_eq!(filter.taps.len(), 32768);
    assert!(filter.headroom_db < 0.3, "boost {}", filter.headroom_db);
    let curves = design.curves.as_ref().unwrap();
    let at = |hz: f64, values: &[f32]| {
        let i = curves
            .frequency_hz
            .iter()
            .position(|&f| (f / hz - 1.0).abs() < 0.008)
            .unwrap();
        f64::from(values[i])
    };
    // The mode stands out several dB above the level and comes down most of the way.
    let measured = at(45.0, &curves.measured_db);
    let cut = at(45.0, &curves.filter_db);
    assert!(measured > 4.0, "mode {measured} dB");
    assert!(cut < -3.0, "cut {cut} dB");
    assert!(at(45.0, &curves.predicted_db) < measured - 3.0);
    // The dip at 90 Hz is left alone.
    assert!(at(90.0, &curves.filter_db) > -0.6 && at(90.0, &curves.filter_db) < 0.3);
    let [before, after] = design.deviation_db.unwrap();
    assert!(after < before, "{before} → {after}");
}

#[test]
fn a_subwoofer_sweep_narrower_than_an_octave_is_not_corrected() {
    let mut take = subwoofer_take("main", true, 6.0);
    take.band_hz = [60.0, 100.0];
    let design = design_subwoofer(
        &OutputMeasurements {
            output_id: "LFE".into(),
            measurements: vec![take],
        },
        &SubwooferSettings::default(),
    )
    .unwrap();
    assert!(design.filter.is_none());
    assert!(design.refusal.unwrap().contains("narrower than an octave"));
}
