use companion_service::{files::write_json, review::saved_take};
use omniphony_adapter::{Sweep, protocol};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn fixture(path: &Path) {
    let sweep = Sweep {
        duration_ms: 1000,
        peak_dbfs: -24.0,
        start_hz: 200.0,
        end_hz: 10000.0,
    };
    let template = sweep.samples(48000).unwrap();
    let mut samples = vec![0.0_f32; 270000];
    for segment in 0..3 {
        let start = 24000 + segment * template.len() + if segment == 1 { 240 } else { 0 };
        for (dst, src) in samples[start..].iter_mut().zip(&template) {
            *dst += src * 0.7;
        }
    }
    let mut wav = hound::WavWriter::create(
        path.join("recording.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for x in samples {
        wav.write_sample(x).unwrap();
    }
    wav.finalize().unwrap();
    write_json(&path.join("stimulus.json"), &json!({
        "schema_version":1,"protocol_version":protocol::VERSION,"origin":"microphone_capture",
        "microphone_id":"synthetic-mic","position_id":"synthetic-seat","output_id":"R","reference_output_id":"L",
        "signal":sweep,"segment_start_render_frames":[0,72000,144000],"calibration_applied":false
    })).unwrap();
    write_json(&path.join("snapshot.json"), &json!({
        "instance_id":"synthetic","revision":"synthetic:1","sample_rate":48000,"master_gain_db":0.0,"max_delay_ms":100.0,"unavailable_reason":null,
        "outputs":(["L","R"].iter().enumerate().map(|(i,id)| json!({"id":id,"name":id,"index":i,"gain_db":0.0,"delay_ms":0.0,"muted":false,"freq_low_hz":null,"freq_high_hz":null})).collect::<Vec<_>>())
    })).unwrap();
    write_json(&path.join("capture-report.json"), &json!({
        "schema_version":2,"origin":"microphone_capture","accepted_for_analysis":true,"rejection_reason":null,
        "capture":{"sample_rate":48000,"frames":270000,"input_channel":1,"stream_channels":2,"fault_flags":0,"clipped_samples":0,"is_monitor":false},
        "renderer_progress":{"signal_id":1,"state":"completed","output_id":"R","reference_output_id":"L","signal":sweep,"rendered_frames":216000,"total_frames":216000},
        "analysis_start_frame":0,"timing_frame_origin":"analysis_start_frame_in_recording_wav",
        "timing":{"target_start_frame":999999999,"relative_peak_delay_ms":9999}
    })).unwrap();
}
fn change(path: &Path, file: &str, f: impl FnOnce(&mut Value)) {
    let path = path.join(file);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    f(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
#[test]
fn offline_reanalysis_uses_wav_not_old_estimates_and_preserves_every_source_file() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let originals: Vec<_> = [
        "recording.wav",
        "capture-report.json",
        "stimulus.json",
        "snapshot.json",
    ]
    .map(|name| (name, fs::read(dir.path().join(name)).unwrap()))
    .into();
    let a = saved_take(dir.path()).unwrap();
    let b = saved_take(dir.path()).unwrap();
    assert_ne!(a.report_file, b.report_file);
    assert!((a.impulse.as_ref().unwrap().relative_arrival_ms.unwrap() - 5.0).abs() < 0.05);
    for (name, bytes) in originals {
        assert_eq!(bytes, fs::read(dir.path().join(name)).unwrap());
    }
    let json: Value = serde_json::from_slice(&fs::read(&a.report_file).unwrap()).unwrap();
    assert_eq!(json["valid_for_correction"], false);
    assert_eq!(json["impulse"]["valid_for_correction"], false);
    assert_eq!(json["origin"], "microphone_capture");
    assert_eq!(json["microphone_id"], "synthetic-mic");
}
#[test]
fn cancelled_faulty_or_inconsistent_takes_cannot_be_promoted() {
    for mode in 0..9 {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        change(dir.path(), "capture-report.json", |r| match mode {
            0 => r["capture"]["fault_flags"] = json!(8),
            1 => r["renderer_progress"]["state"] = json!("cancelled"),
            2 => r["renderer_progress"]["output_id"] = json!("L"),
            3 => r["capture"]["sample_rate"] = json!(44100),
            4 => r["capture"]["frames"] = json!(270001),
            5 => r["schema_version"] = json!(99),
            6 => r["capture"]["is_monitor"] = json!(true),
            7 => {
                r["accepted_for_analysis"] = json!(false);
                r["rejection_reason"] = json!("renderer configuration changed");
            }
            _ => r["analysis_start_frame"] = json!(usize::MAX),
        });
        assert!(saved_take(dir.path()).is_err(), "case {mode}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
    }
}
#[test]
fn legacy_correlation_rejection_is_reviewable_but_keeps_its_original_verdict() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    change(dir.path(), "capture-report.json", |r| {
        r["schema_version"] = json!(1);
        r["accepted_for_analysis"] = json!(false);
        r["rejection_reason"] = json!("competing correlation peaks; direct arrival is ambiguous");
    });
    let original = fs::read(dir.path().join("capture-report.json")).unwrap();
    assert!(saved_take(dir.path()).unwrap().impulse.is_some());
    assert_eq!(
        original,
        fs::read(dir.path().join("capture-report.json")).unwrap()
    );
}
#[test]
fn sink_monitor_origin_survives_review() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    change(dir.path(), "stimulus.json", |r| {
        r["origin"] = json!("sink_monitor_capture")
    });
    change(dir.path(), "capture-report.json", |r| {
        r["origin"] = json!("sink_monitor_capture");
        r["capture"]["is_monitor"] = json!(true);
    });
    let report = saved_take(dir.path()).unwrap();
    assert_eq!(report.origin, "sink_monitor_capture");
    assert!(!report.valid_for_correction);
}
