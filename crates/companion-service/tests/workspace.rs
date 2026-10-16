use companion_service::{
    controller::{Settings, State},
    files::write_json,
    preferences::{PreferenceStore, ResultPlot, ViewPreferences},
    workspace::{self, TakeReference, WorkspaceIndex},
};
use omniphony_adapter::{Snapshot, protocol::Output};
use std::{collections::BTreeSet, path::Path};

fn fixture(root: &Path, name: &str, output: &str, accepted: bool) -> TakeReference {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let settings = Settings {
        device: Some("test input".into()),
        reference_output: Some("L".into()),
        output_root: root.display().to_string(),
        ..Default::default()
    };
    let snapshot = Snapshot {
        instance_id: "test".into(),
        revision: "test:1".into(),
        sample_rate: 48000,
        master_gain_db: 0.0,
        max_delay_ms: 250.0,
        unavailable_reason: None,
        outputs: ["L", "R"]
            .iter()
            .enumerate()
            .map(|(i, id)| Output {
                id: (*id).into(),
                name: (*id).into(),
                index: i as u32,
                gain_db: 0.0,
                delay_ms: 0.0,
                muted: false,
                freq_low_hz: None,
                freq_high_hz: None,
            })
            .collect(),
    };
    write_json(&folder.join("snapshot.json"), &snapshot).unwrap();
    write_json(&folder.join("stimulus.json"), &serde_json::json!({"output_id":output,"microphone_id":settings.microphone_id,"position_id":settings.position_id,"reference_output_id":"L","signal":settings.sweep})).unwrap();
    write_json(&folder.join("capture-report.json"), &serde_json::json!({"schema_version":3,"accepted_for_analysis":accepted,"rejection_reason":if accepted {None} else {Some("capture cancelled by user")},"capture":{"device":"test input","input_channel":1,"sample_rate":48000}})).unwrap();
    // Startup displays the retained report without reading or reanalyzing PCM.
    std::fs::write(folder.join("recording.wav"), b"display-only test fixture").unwrap();
    TakeReference {
        output_id: output.into(),
        revision: snapshot.revision,
        settings,
        result: Ok(folder),
    }
}

#[test]
fn restart_keeps_results_verdicts_absolute_paths_and_view_choices_without_reanalysis() {
    let dir = tempfile::tempdir().unwrap();
    let a = fixture(dir.path(), "take-1", "L", true);
    let b = fixture(dir.path(), "batch-2/take-001", "R", false);
    let source = std::fs::read(b.result.as_ref().unwrap().join("capture-report.json")).unwrap();
    let index = WorkspaceIndex {
        takes: vec![a.clone(), b.clone()],
        ..Default::default()
    };
    let path = dir.path().join("settings.json");
    let mut store = PreferenceStore::open(path.clone()).unwrap();
    store.update_workspace(index.clone());
    let view = ViewPreferences {
        plot: ResultPlot::GroupDelay,
        selected_take: Some(a.result.as_ref().unwrap().clone()),
        selected_output: Some("L".into()),
        burst_frequency: 2000.0,
        burst_overview: true,
        ..Default::default()
    };
    store.update_view(view.clone());
    store.update(&a.settings, "127.0.0.1:9007", &BTreeSet::new(), "");
    drop(store); // Flush an immediate close, without another frame.
    let store = PreferenceStore::open(path).unwrap();
    assert_eq!(store.preferences().view, view);
    assert_eq!(store.preferences().workspace.as_ref(), Some(&index));
    let result = workspace::restore(store.preferences().workspace.clone(), &a.settings).unwrap();
    assert_eq!(result.takes.len(), 2);
    assert!(result.takes[0].outcome.as_ref().unwrap().accepted);
    assert!(result.takes[1].outcome.as_ref().unwrap().cancelled);
    assert!(!result.takes[1].outcome.as_ref().unwrap().accepted);
    assert_eq!(
        source,
        std::fs::read(b.result.as_ref().unwrap().join("capture-report.json")).unwrap()
    );
    assert!(!b.result.as_ref().unwrap().join("analysis.json").exists());
}

#[test]
fn missing_corrupt_and_mismatched_results_do_not_discard_healthy_speakers_or_paths() {
    let dir = tempfile::tempdir().unwrap();
    let a = fixture(dir.path(), "take-1", "L", true);
    for mode in 0..3 {
        let b = fixture(dir.path(), &format!("take-bad-{mode}"), "R", true);
        if mode == 0 {
            std::fs::remove_file(b.result.as_ref().unwrap().join("recording.wav")).unwrap();
        }
        if mode == 1 {
            std::fs::write(
                b.result.as_ref().unwrap().join("capture-report.json"),
                b"{broken",
            )
            .unwrap();
        }
        let mut bad = b.clone();
        if mode == 2 {
            bad.output_id = "wrong output".into();
        }
        let index = WorkspaceIndex {
            takes: vec![a.clone(), bad],
            ..Default::default()
        };
        let result = workspace::restore(Some(index), &a.settings).unwrap();
        assert!(result.takes[0].outcome.as_ref().unwrap().accepted);
        assert!(!result.takes[1].outcome.as_ref().unwrap().accepted);
        assert_eq!(result.warnings.len(), 1);
        let state = State {
            takes: result.takes,
            ..Default::default()
        };
        assert_eq!(WorkspaceIndex::from_state(&state).takes[1].result, b.result);
    }
}

#[test]
fn settings_only_upgrade_discovers_single_batch_and_alignment_children_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = fixture(dir.path(), "take-1", "L", true);
    fixture(dir.path(), "batch-2/take-001", "R", false);
    fixture(dir.path(), "alignment-3/pass-1/take-001", "L", true);
    fixture(dir.path(), "unrelated/take-4", "R", true);
    let result = workspace::restore(None, &a.settings).unwrap();
    assert!(result.imported);
    assert_eq!(result.takes.len(), 3);
    assert!(result.snapshot.is_some());
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    // An existing empty index is intentional; don't rescan and resurrect old takes.
    assert!(
        workspace::restore(Some(WorkspaceIndex::default()), &a.settings)
            .unwrap()
            .takes
            .is_empty()
    );
}

#[test]
fn corrupt_alignment_and_review_references_survive_for_later_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let index = WorkspaceIndex {
        alignment: Some(dir.path().join("moved-alignment")),
        review_file: Some(dir.path().join("moved-review.json")),
        ..Default::default()
    };
    let result = workspace::restore(Some(index.clone()), &Settings::default()).unwrap();
    assert_eq!(result.warnings.len(), 2);
    let state = State {
        restored_index: Some(result.index),
        ..Default::default()
    };
    assert_eq!(WorkspaceIndex::from_state(&state), index);
    let oversized = WorkspaceIndex {
        alignment: Some("relative/path".into()),
        ..Default::default()
    };
    assert!(workspace::restore(Some(oversized), &Settings::default()).is_err());
}

#[test]
fn display_reports_larger_than_the_core_limit_reload_without_changing_observation_limits() {
    let dir = tempfile::tempdir().unwrap();
    let reference = fixture(dir.path(), "take-large", "L", true);
    let path = reference
        .result
        .as_ref()
        .unwrap()
        .join("capture-report.json");
    let mut bytes = vec![b' '; 8 * 1024 * 1024];
    bytes.extend(std::fs::read(&path).unwrap());
    std::fs::write(&path, &bytes).unwrap();
    assert!(companion_service::files::read_json::<serde_json::Value>(&path).is_err());
    let index = WorkspaceIndex {
        takes: vec![reference.clone()],
        ..Default::default()
    };
    let result = workspace::restore(Some(index), &reference.settings).unwrap();
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(result.takes[0].outcome.as_ref().unwrap().accepted);
}
