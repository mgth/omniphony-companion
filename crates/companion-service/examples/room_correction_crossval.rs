//! Offline: cross-validate the room-correction design on saved alignments.
//!
//! Reads every `alignment-*/alignment-state.json` of SESSIONS_DIR (take folders are
//! resolved inside it, whatever its own name), recomputes each take's unweighted
//! response from its recording as a new capture would store it, then for each
//! alignment in turn designs every speaker's filter from the other alignments and
//! checks it on the held-out one: RMS distance from the target before and after, over
//! the design's evaluation band. Also designs from one alignment's two passes alone
//! (what a single main-seat measurement gives) and checks it on the others.
//! Writes nothing.
use calibration_core::room_correction::{
    DesignSettings, Measurement, OutputDesign, OutputMeasurements, design, deviation,
    smoothed_response,
};
use companion_service::{alignment::Alignment, files::read_json};
use measurement_capture::{impulse, timing};
use omniphony_adapter::{Snapshot, Sweep};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn response(take: &Path, position: &str) -> companion_service::AppResult<Measurement> {
    let stimulus: serde_json::Value = read_json(&take.join("stimulus.json"))?;
    let report: serde_json::Value = read_json(&take.join("capture-report.json"))?;
    if report["accepted_for_analysis"] != serde_json::Value::Bool(true) {
        return Err("rejected take".into());
    }
    let snapshot: Snapshot = read_json(&take.join("snapshot.json"))?;
    let sweep: Sweep = serde_json::from_value(stimulus["signal"].clone())?;
    let start = report["analysis_start_frame"]
        .as_u64()
        .ok_or("missing analysis start")? as usize;
    let mut wav = hound::WavReader::open(take.join("recording.wav"))?;
    let rate = wav.spec().sample_rate;
    let samples = wav.samples::<f32>().collect::<Result<Vec<_>, _>>()?;
    let samples = samples.get(start..).ok_or("analysis start past the end")?;
    let timing = timing::inspect(samples, rate, snapshot.sample_rate, sweep)?;
    let full = impulse::full_response(samples, rate, snapshot.sample_rate, sweep, &timing)?;
    let [_, speaker, _] = full.segments;
    Ok(Measurement {
        position_id: position.into(),
        main_seat: false,
        rate: full.rate,
        samples: speaker,
        anchor: full.anchor,
        band_hz: [f64::from(sweep.start_hz), f64::from(sweep.end_hz)],
        calibration: None,
    })
}

/// The share of the evaluation band within ±2 dB of the target's shape (mean
/// difference removed), before and after the filter.
fn within_2_db(d: &OutputDesign, response: &[f64], settings: &DesignSettings) -> Option<[f64; 2]> {
    let curves = d.curves.as_ref()?;
    let band = d.evaluation_band_hz?;
    let frequencies = settings.frequencies();
    let mut differences = [vec![], vec![]];
    for (i, hz) in curves.frequency_hz.iter().enumerate() {
        let k = frequencies.iter().position(|f| f == hz)?;
        if (band[0]..=band[1]).contains(hz) && response[k].is_finite() {
            let before = response[k] - f64::from(curves.target_db[i]);
            differences[0].push(before);
            differences[1].push(before + f64::from(curves.filter_db[i]));
        }
    }
    Some(differences.map(|d| {
        let m = mean(&d);
        d.iter().filter(|v| (*v - m).abs() <= 2.0).count() as f64 / d.len().max(1) as f64 * 100.0
    }))
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

fn main() -> companion_service::AppResult<()> {
    let root = PathBuf::from(std::env::args().nth(1).ok_or("usage: SESSIONS_DIR")?);
    let mut alignments: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("alignment-state.json").is_file())
        .collect();
    alignments.sort();
    // output → alignment → takes
    let mut takes: BTreeMap<String, Vec<Vec<Measurement>>> = BTreeMap::new();
    let mut rate = 48000;
    for (index, directory) in alignments.iter().enumerate() {
        let a: Alignment = read_json(&directory.join("alignment-state.json"))?;
        rate = a.snapshot.sample_rate;
        let session = format!("session-{}", index + 1);
        for row in &a.rows {
            let mut found = vec![];
            for folder in &row.take_folders {
                // "sessions/alignment-…/pass-1/take-001" → inside `root`.
                let inside: PathBuf = folder.components().skip(1).collect();
                match response(&root.join(inside), &session) {
                    Ok(m) => found.push(m),
                    Err(e) => eprintln!("{}: {}: {e}", row.output_id, folder.display()),
                }
            }
            let per = takes.entry(row.output_id.clone()).or_default();
            per.resize_with(alignments.len(), Vec::new);
            per[index] = found;
        }
    }
    let settings = DesignSettings {
        sample_rate: rate,
        ..DesignSettings::default()
    };
    println!(
        "{} alignments; target −{} dB/decade above {} Hz, boost ≤ {} dB, cut ≤ {} dB, {} taps at {} Hz, up to {} Hz",
        alignments.len(),
        settings.tilt_db_per_decade,
        settings.tilt_start_hz,
        settings.max_boost_db,
        settings.max_cut_db,
        settings.taps,
        settings.sample_rate,
        settings.max_hz
    );
    println!(
        "{:<8} {:>22} {:>16} {:>22} {:>22} {:>10} {:>9}",
        "output",
        "held out: before→after",
        "±2 dB: share",
        "in design: pred.",
        "one session → others",
        "headroom",
        "band"
    );
    for (output, sessions) in &takes {
        let (mut before, mut after, mut predicted, mut single) = (vec![], vec![], vec![], vec![]);
        let mut within = vec![];
        let mut headroom = vec![];
        let mut band = None;
        let mut notes = vec![];
        for held in 0..sessions.len() {
            let training: Vec<Measurement> = sessions
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != held)
                .flat_map(|(_, t)| t.iter().cloned())
                .collect();
            let d = design(
                &OutputMeasurements {
                    output_id: output.clone(),
                    measurements: training,
                },
                &settings,
            )?;
            if let Some(reason) = &d.refusal {
                notes.push(format!("fold {}: refused: {reason}", held + 1));
                continue;
            }
            band = d.evaluation_band_hz;
            headroom.push(d.filter.as_ref().map_or(0.0, |f| f.headroom_db));
            predicted.push(d.deviation_db.unwrap_or([f64::NAN; 2]));
            for m in &sessions[held] {
                let r = smoothed_response(m, &settings)?;
                if let Some([b, a]) = deviation(&d, &r, &settings) {
                    before.push(b);
                    after.push(a);
                }
                within.extend(within_2_db(&d, &r, &settings));
            }
            // One session's two passes, as the main seat alone, checked on the others.
            let mut alone = sessions[held].clone();
            for m in &mut alone {
                m.main_seat = true;
            }
            let d = design(
                &OutputMeasurements {
                    output_id: output.clone(),
                    measurements: alone,
                },
                &settings,
            )?;
            if d.refusal.is_none() {
                for (i, t) in sessions.iter().enumerate() {
                    if i == held {
                        continue;
                    }
                    for m in t {
                        let r = smoothed_response(m, &settings)?;
                        if let Some(v) = deviation(&d, &r, &settings) {
                            single.push(v);
                        }
                    }
                }
            }
        }
        let pairs = |v: &[[f64; 2]]| {
            format!(
                "{:.2} → {:.2} dB",
                mean(&v.iter().map(|p| p[0]).collect::<Vec<_>>()),
                mean(&v.iter().map(|p| p[1]).collect::<Vec<_>>())
            )
        };
        println!(
            "{:<8} {:>22} {:>16} {:>22} {:>22} {:>7.2} dB {:>9}",
            output,
            format!("{:.2} → {:.2} dB", mean(&before), mean(&after)),
            format!(
                "{:.0} → {:.0} %",
                mean(&within.iter().map(|p| p[0]).collect::<Vec<_>>()),
                mean(&within.iter().map(|p| p[1]).collect::<Vec<_>>())
            ),
            pairs(&predicted),
            pairs(&single),
            headroom.iter().copied().fold(f64::MIN, f64::max),
            band.map_or("-".into(), |b| format!("{:.0}–{:.0}", b[0], b[1]))
        );
        for note in notes {
            println!("         {note}");
        }
    }
    // The full design from every alignment, for its diagnostics.
    println!("\nDesigned from all alignments:");
    for (output, sessions) in &takes {
        let d = design(
            &OutputMeasurements {
                output_id: output.clone(),
                measurements: sessions.iter().flatten().cloned().collect(),
            },
            &settings,
        )?;
        let agreement: Vec<String> = d
            .positions
            .iter()
            .map(|p| format!("{:.1}", p.deviation_db.unwrap_or(f64::NAN)))
            .collect();
        let reflections: Vec<String> = d
            .reflections
            .iter()
            .map(|r| {
                format!(
                    "{:.2} ms {:.1} dB {:.0} cm",
                    r.delay_ms, r.level_db, r.extra_path_cm
                )
            })
            .collect();
        println!(
            "{output:<8} {} | sessions vs average {} dB, spread {:?} dB | reflections: {}",
            d.refusal.as_deref().map_or_else(
                || format!(
                    "headroom {:.1} dB, deepest cut {:.1} dB",
                    d.filter.as_ref().map_or(0.0, |f| f.headroom_db),
                    d.filter.as_ref().map_or(0.0, |f| f.deepest_cut_db)
                ),
                |r| format!("refused: {r}")
            ),
            agreement.join("/"),
            d.spread_db,
            if reflections.is_empty() {
                "none above −15 dB".into()
            } else {
                reflections.join(", ")
            }
        );
    }
    Ok(())
}
