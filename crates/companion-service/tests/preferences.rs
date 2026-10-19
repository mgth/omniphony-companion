use companion_service::{
    controller::Settings,
    preferences::{PreferenceStore, Preferences},
};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};

fn wait_for(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !check() {
        assert!(Instant::now() < deadline, "settings worker did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn read(path: &Path) -> Preferences {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn autosave_and_immediate_exit_restore_all_choices_and_the_latest_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config/settings.json");
    let mut store = PreferenceStore::open(path.clone()).unwrap();
    assert!(!store.loaded());
    assert!(!path.exists());
    let mut settings = Settings {
        device: Some("pipewire:source:measurement-interface".into()),
        input_channel: 2,
        sample_rate: 96000,
        microphone_id: "calibrated-mic".into(),
        position_id: "seat-two".into(),
        reference_output: Some("Right".into()),
        calibration_path: "calibration.txt".into(),
        output_root: "takes".into(),
        sweep: omniphony_adapter::Sweep {
            duration_ms: 2000,
            peak_dbfs: -24.0,
            start_hz: 120.0,
            end_hz: 18000.0,
        },
        retake_clipped: false,
    };
    let excluded = BTreeSet::from(["Rear".into(), "Height".into()]);
    store.update(&settings, "127.0.0.1:9003", &excluded, "takes/previous");
    wait_for(|| path.exists());
    assert_eq!(read(&path).settings, settings);
    for i in 0..100 {
        settings.position_id = format!("seat-{i}");
        store.update(&settings, "127.0.0.1:9003", &excluded, "takes/previous");
    }
    drop(store); // Flushes without waiting for another UI paint or debounce interval.
    let store = PreferenceStore::open(path.clone()).unwrap();
    assert!(store.loaded());
    let saved = store.preferences();
    assert_eq!(saved.settings, settings);
    assert_eq!(saved.excluded_outputs, excluded);
    assert_eq!(saved.endpoint, "127.0.0.1:9003");
    assert_eq!(saved.review_path, "takes/previous");
    assert_eq!(saved.schema_version, 1);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn bad_and_future_files_are_not_overwritten_and_missing_fields_get_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    for bytes in [
        b"broken {".as_slice(),
        br#"{"schema_version":2}"#,
        &vec![b' '; 1024 * 1024 + 1],
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(PreferenceStore::open(path.clone()).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::write(
        &path,
        br#"{"schema_version":1,"settings":{"input_channel":2}}"#,
    )
    .unwrap();
    let store = PreferenceStore::open(path).unwrap();
    assert_eq!(store.preferences().settings.input_channel, 2);
    assert_eq!(store.preferences().settings.sample_rate, 48000);
    // Settings saved before the option existed measure clipped speakers again.
    assert!(store.preferences().settings.retake_clipped);
}

#[test]
fn failed_atomic_save_reports_an_error_and_retry_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut store = PreferenceStore::open(path.clone()).unwrap();
    fs::create_dir(&path).unwrap(); // Destination becomes unwritable as a file.
    let settings = Settings::default();
    store.update(&settings, "127.0.0.1:9003", &BTreeSet::new(), "");
    wait_for(|| store.error().is_some());
    assert!(path.is_dir());
    fs::remove_dir(&path).unwrap();
    store.retry();
    wait_for(|| path.is_file() && store.error().is_none());
    assert_eq!(read(&path).endpoint, "127.0.0.1:9003");
}
