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
    let spectral = a.impulse.as_ref().unwrap().segments[1]
        .spectral
        .as_ref()
        .unwrap();
    let point = spectral
        .points
        .iter()
        .find(|p| p.frequency_hz >= 1000.0)
        .unwrap();
    assert!((point.magnitude_db.unwrap() - 20.0_f64 * 0.7_f64.log10()).abs() < 0.15);
    assert!(
        point.group_delay_ms.unwrap().abs() < 0.05,
        "bulk extraction offset must be removed"
    );
    assert!(point.peak_time_ms.unwrap().abs() < 0.1);
    let waveforms = a.impulse.as_ref().unwrap().segments[1]
        .waveforms
        .as_ref()
        .unwrap();
    let burst = waveforms
        .bursts
        .iter()
        .find(|b| b.frequency_hz == 1000.0)
        .unwrap();
    assert!(
        burst.phase_deg.unwrap().abs() < 2.0,
        "phase uses the same extraction zero"
    );
    assert!((burst.predicted.detail.peak / burst.input.detail.peak - 0.7).abs() < 0.01);
    for (name, bytes) in originals {
        assert_eq!(bytes, fs::read(dir.path().join(name)).unwrap());
    }
    let json: Value = serde_json::from_slice(&fs::read(&a.report_file).unwrap()).unwrap();
    assert_eq!(json["valid_for_correction"], false);
    assert_eq!(json["impulse"]["valid_for_correction"], false);
    assert_eq!(json["origin"], "microphone_capture");
    assert_eq!(json["microphone_id"], "synthetic-mic");
    assert_eq!(
        json["impulse"]["segments"][1]["waveforms"]["method"],
        "signed-ir-and-virtual-burst-v1"
    );
}
#[test]
fn takes_stay_reviewable_under_every_protocol_up_to_the_current_one() {
    // A take keeps the protocol it was recorded with. Takes at protocol 4 were refused
    // once the Companion spoke 5, which made their sessions impossible to reanalyze.
    for version in 1..=protocol::VERSION + 1 {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        change(dir.path(), "stimulus.json", |s| {
            s["protocol_version"] = json!(version)
        });
        let supported = (2..=protocol::VERSION).contains(&version);
        assert_eq!(
            saved_take(dir.path()).is_ok(),
            supported,
            "protocol {version}"
        );
    }
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
        if mode == 7 {
            let review = saved_take(dir.path()).unwrap();
            assert!(!review.original_accepted_for_analysis);
            assert!(review.timing.is_none() && review.impulse.is_none());
            assert!(review.analysis_error.is_some());
        } else {
            assert!(saved_take(dir.path()).is_err(), "case {mode}");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
        }
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

#[test]
fn sequence_child_checks_parent_progress_and_keeps_local_timing() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("take-001");
    fs::create_dir(&child).unwrap();
    fixture(&child);
    change(&child, "stimulus.json", |s| {
        s["schema_version"] = json!(2);
        s["sequence_window"] =
            json!({"target_index":0,"source_start_frame":0,"source_end_frame":270000});
    });
    change(&child, "capture-report.json", |r| {
        r["renderer_progress"]["sequence_outputs"] = json!(["R"])
    });
    let capture: Value =
        serde_json::from_slice(&fs::read(child.join("capture-report.json")).unwrap()).unwrap();
    write_json(&root.path().join("batch-report.json"),&json!({"renderer_progress":capture["renderer_progress"],"capture":{"frames":270000},"error":null,"cancelled":false})).unwrap();
    fs::copy(
        child.join("recording.wav"),
        root.path().join("recording.wav"),
    )
    .unwrap();
    let review = saved_take(&child).unwrap();
    assert!((review.timing.unwrap().relative_peak_delay_ms.unwrap() - 5.0).abs() < 0.1);
    // A sequence child keeps the protocol it was recorded with, from 3 to the current one.
    for version in 2..=protocol::VERSION {
        change(&child, "stimulus.json", |s| {
            s["protocol_version"] = json!(version)
        });
        assert_eq!(
            saved_take(&child).is_ok(),
            version >= 3,
            "protocol {version}"
        );
    }
    change(root.path(), "batch-report.json", |r| {
        r["renderer_progress"]["sequence_outputs"] = json!(["L"])
    });
    assert!(saved_take(&child).is_err());
}

#[test]
fn rejected_missing_and_clipped_target_takes_are_diagnosed_without_promotion() {
    use measurement_capture::diagnostics::SegmentStatus;
    for clipped in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let wav_path = dir.path().join("recording.wav");
        let mut reader = hound::WavReader::open(&wav_path).unwrap();
        let spec = reader.spec();
        let mut samples: Vec<f32> = reader.samples().map(Result::unwrap).collect();
        drop(reader);
        for x in &mut samples[96000..168000] {
            *x = if clipped {
                (*x * 50.0).clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        let count = samples.iter().filter(|x| x.abs() >= 0.999).count();
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for x in samples {
            writer.write_sample(x).unwrap();
        }
        writer.finalize().unwrap();
        change(dir.path(), "capture-report.json", |r| {
            r["accepted_for_analysis"] = json!(false);
            r["rejection_reason"] = json!(if clipped {
                "clipped recording"
            } else {
                "three reliable timing references were not found"
            });
            r["capture"]["clipped_samples"] = json!(count);
        });
        let original = fs::read(dir.path().join("capture-report.json")).unwrap();
        let audio = fs::read(&wav_path).unwrap();
        let review = saved_take(dir.path()).unwrap();
        assert_eq!(
            review.diagnostics.segments.as_ref().unwrap()[1].status,
            if clipped {
                SegmentStatus::Clipped
            } else {
                SegmentStatus::NoSweepDetected
            }
        );
        assert!(!review.original_accepted_for_analysis && !review.valid_for_correction);
        if clipped {
            assert!(review.timing.is_some() && review.impulse.is_some());
            let target = &review.diagnostics.segments.as_ref().unwrap()[1];
            assert!(target.overdrive_db.is_some_and(|db| db > 0.0));
            assert!(
                review
                    .diagnostics
                    .messages
                    .iter()
                    .any(|m| m.contains("dB over full scale"))
            );
            assert_eq!(
                review.quality.as_ref().unwrap().acquisition.grade,
                companion_service::quality::Grade::Poor
            );
        } else {
            assert!(review.timing.is_none() && review.impulse.is_none());
        }
        assert_eq!(
            original,
            fs::read(dir.path().join("capture-report.json")).unwrap()
        );
        assert_eq!(audio, fs::read(&wav_path).unwrap());
        let report: Value = serde_json::from_slice(&fs::read(review.report_file).unwrap()).unwrap();
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["timing"].is_null(), !clipped);
        if clipped {
            change(dir.path(), "capture-report.json", |r| {
                r["capture"]["clipped_samples"] = json!(0)
            });
            assert!(saved_take(dir.path()).is_err());
        }
    }
}

#[test]
fn old_snr_rejections_get_new_diagnostic_results_without_rewriting_the_original() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    change(dir.path(), "capture-report.json", |r| {
        r["accepted_for_analysis"] = json!(false);
        r["rejection_reason"] =
            json!("sweep 1 signal-to-noise ratio is 19.88 dB, below the 20 dB acquisition limit");
    });
    let original = fs::read(dir.path().join("capture-report.json")).unwrap();
    let review = saved_take(dir.path()).unwrap();
    assert!(!review.original_accepted_for_analysis && !review.valid_for_correction);
    assert!(review.timing.is_some() && review.impulse.is_some());
    assert!(review.quality.as_ref().unwrap().level_db.is_some());
    assert_eq!(
        original,
        fs::read(dir.path().join("capture-report.json")).unwrap()
    );
}

#[test]
fn whole_alignment_reanalysis_retains_originals_and_cannot_apply_a_profile() {
    use companion_service::{
        alignment::{self, Alignment, Row},
        controller::Settings,
    };
    let root = tempfile::tempdir().unwrap();
    let original_dir = root.path().join("alignment-original");
    fs::create_dir(&original_dir).unwrap();
    let mut folders = vec![];
    for pass in 1..=2 {
        let path = original_dir.join(format!("pass-{pass}"));
        fs::create_dir(&path).unwrap();
        fixture(&path);
        change(&path, "capture-report.json", |r| {
            r.as_object_mut().unwrap().remove("timing");
            r["accepted_for_analysis"] = json!(false);
            r["rejection_reason"] = json!(
                "sweep 1 signal-to-noise ratio is 19.88 dB, below the 20 dB acquisition limit"
            );
        });
        folders.push(path);
    }
    let snapshot = companion_service::files::read_json(&folders[0].join("snapshot.json")).unwrap();
    let signal = companion_service::files::read_json::<Value>(&folders[0].join("stimulus.json"))
        .unwrap()["signal"]
        .clone();
    let original = Alignment {
        level_probe: None,
        positions: vec![],
        schema_version: 1,
        directory: original_dir,
        endpoint: "127.0.0.1:9000".parse().unwrap(),
        settings: Settings {
            microphone_id: "synthetic-mic".into(),
            position_id: "synthetic-seat".into(),
            reference_output: Some("L".into()),
            sweep: serde_json::from_value(signal).unwrap(),
            ..Default::default()
        },
        snapshot,
        rows: vec![Row {
            sweep: None,
            reference_output: None,
            output_id: "R".into(),
            take_folders: folders.clone(),
            repeats: vec![],
            quality: vec![],
            qualified: None,
            reason: None,
            reason_code: None,
            include_anyway: false,
        }],
        blockers: vec![],
        blocker_codes: vec![],
        pass_reports: None,
        verification_pass_reports: None,
        session: None,
        profile: None,
        applied_snapshot: None,
        restored_snapshot: None,
        verification: None,
        verification_note: None,
        application_uncertain: false,
        physical_levels_changed: false,
        retake_snapshots: vec![],
        zeroed_snapshot: None,
        bass: None,
    };
    let before = fs::read(folders[0].join("capture-report.json")).unwrap();
    struct Offline;
    impl companion_service::controller::Backend for Offline {
        fn inputs(&self) -> Result<Vec<measurement_capture::InputDevice>, String> {
            panic!("offline review must not enumerate devices")
        }
        fn discover(&self, _: std::net::SocketAddr) -> Result<omniphony_adapter::Snapshot, String> {
            panic!("offline review must not connect")
        }
        fn measure(
            &self,
            _: companion_service::measurement::MeasureRequest,
            _: &std::sync::atomic::AtomicBool,
            _: &mut dyn FnMut(companion_service::measurement::MeasureProgress),
        ) -> Result<companion_service::measurement::TakeOutcome, String> {
            panic!("offline review must not measure")
        }
    }
    let mut controller =
        companion_service::controller::Controller::new(std::sync::Arc::new(Offline));
    controller.state.alignment = Some(original.clone());
    controller.reanalyze_alignment();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while controller.busy() {
        controller.poll();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        controller.state.error.is_none(),
        "{:?}",
        controller.state.error
    );
    assert!(
        controller.state.workspace_warnings.is_empty(),
        "{:?}",
        controller.state.workspace_warnings
    );
    assert_eq!(controller.state.takes.len(), 2);
    assert_eq!(controller.state.analyses.len(), 2);
    for take in &controller.state.takes {
        let analysis = controller.analysis_for(take).unwrap();
        assert!((analysis.arrival_ms().unwrap() - 5.0).abs() < 0.05);
        assert!(!take.outcome.as_ref().unwrap().accepted);
    }
    let review = controller.state.alignment.clone().unwrap();
    assert_ne!(review.directory, original.directory);
    assert!(review.rows[0].qualified.is_some());
    assert_eq!(review.rows[0].quality.len(), 2);
    assert!(review.profile.is_none() && review.session.is_none());
    assert!(review.targets(false).is_err());
    assert!(alignment::load(&review.directory).is_ok());
    assert_eq!(
        before,
        fs::read(folders[0].join("capture-report.json")).unwrap()
    );
    // Clipping outside the analysis slice remains a capture fault, even when
    // the retained sweep response itself is clean and repeatable.
    let wav_path = folders[0].join("recording.wav");
    let mut reader = hound::WavReader::open(&wav_path).unwrap();
    let spec = reader.spec();
    let mut samples: Vec<f32> = reader.samples().map(Result::unwrap).collect();
    drop(reader);
    samples[0] = 1.0;
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for sample in samples {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
    change(&folders[0], "capture-report.json", |r| {
        r["rejection_reason"] = json!("clipped recording");
        r["capture"]["clipped_samples"] = json!(1);
        r["analysis_start_frame"] = json!(4800);
    });
    let clipped = saved_take(&folders[0]).unwrap();
    assert_eq!(clipped.capture_clipped_samples, 1);
    assert_eq!(clipped.diagnostics.total_clipped_samples, 0);
    assert!(clipped.impulse.is_some());
    assert_eq!(
        clipped.quality.unwrap().acquisition.grade,
        companion_service::quality::Grade::Poor
    );
    let clipped_alignment = alignment::reanalyze(&original).unwrap();
    assert!(clipped_alignment.rows[0].qualified.is_none());
    assert!(
        clipped_alignment.rows[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("clipped recording")
    );
    assert_eq!(
        clipped_alignment.rows[0].exclusion(),
        Some(calibration_core::ExclusionReason::Clipped { overdrive_db: None })
    );
    // Restore the synthetic fixture for the independent identity check below.
    fs::remove_dir_all(&folders[0]).unwrap();
    fs::create_dir(&folders[0]).unwrap();
    fixture(&folders[0]);
    let mut applied = original.clone();
    applied.applied_snapshot = Some(applied.snapshot.clone());
    assert!(alignment::reanalyze(&applied).is_err());
    // Captured under other settings (a revision alone would be the same installation).
    change(&folders[1], "snapshot.json", |r| {
        r["revision"] = json!("different");
        r["outputs"][0]["gain_db"] = json!(-7.0);
    });
    let mismatched = alignment::reanalyze(&original).unwrap();
    assert!(mismatched.rows[0].qualified.is_none());
    assert!(
        mismatched.rows[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("different renderer configuration")
    );
    assert!(matches!(
        mismatched.rows[0].reason_code,
        Some(calibration_core::ExclusionReason::SessionMismatch { .. })
    ));
}
