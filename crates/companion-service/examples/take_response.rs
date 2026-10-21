//! Offline: the unweighted response of a saved take, as a new capture would store it.
//! Reads only the take folder and writes `response.wav` and `response.json` into another,
//! new folder; useful for takes recorded before responses were kept.
use companion_service::files::{read_json, write_json};
use measurement_capture::{calibration::Calibration, impulse, response, timing};
use omniphony_adapter::{Snapshot, Sweep};
use std::path::PathBuf;

fn main() -> companion_service::AppResult<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: take_response TAKE_FOLDER NEW_FOLDER [CALIBRATION_FILE]".into());
    }
    let take = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    let stimulus: serde_json::Value = read_json(&take.join("stimulus.json"))?;
    let report: serde_json::Value = read_json(&take.join("capture-report.json"))?;
    let snapshot: Snapshot = read_json(&take.join("snapshot.json"))?;
    let sweep: Sweep = serde_json::from_value(stimulus["signal"].clone())?;
    let start = report["analysis_start_frame"]
        .as_u64()
        .ok_or("missing analysis start")? as usize;
    let mut wav = hound::WavReader::open(take.join("recording.wav"))?;
    let rate = wav.spec().sample_rate;
    let samples = wav.samples::<f32>().collect::<Result<Vec<_>, _>>()?;
    let samples = samples
        .get(start..)
        .ok_or("analysis start past the recording")?;
    let timing = timing::inspect(samples, rate, snapshot.sample_rate, sweep)?;
    let full = impulse::full_response(samples, rate, snapshot.sample_rate, sweep, &timing)?;
    let calibration = args
        .get(2)
        .map(|path| Calibration::parse(&std::fs::read_to_string(path)?).map_err(Into::into))
        .transpose()
        .map_err(|e: Box<dyn std::error::Error>| e)?;
    let magnitude = response::magnitude(&full, sweep, calibration.as_ref())?;
    std::fs::create_dir(&out)?;
    let mut writer = hound::WavWriter::create(
        out.join("response.wav"),
        hound::WavSpec {
            channels: 3,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )?;
    for i in 0..full.segments[0].len() {
        for segment in &full.segments {
            writer.write_sample(segment[i])?;
        }
    }
    writer.finalize()?;
    write_json(
        &out.join("response.json"),
        &serde_json::json!({
            "source_take": take, "signal": sweep, "sample_rate_hz": full.rate,
            "anchor_frame": full.anchor, "anchor_offsets": full.anchor_offsets, "frames": full.segments[0].len(),
            "segment_start_frames": [timing.reference_start_frames[0], timing.target_start_frame, timing.reference_start_frames[1]],
            "magnitude": magnitude,
        }),
    )?;
    println!(
        "{}: {} frames at {:.3} Hz, anchor {}",
        out.display(),
        full.segments[0].len(),
        full.rate,
        full.anchor
    );
    Ok(())
}
