use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omniphony-companion"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn demo(cwd: &Path) {
    let result = run(&["demo", "--out-dir", "demo"], cwd);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("SIMULATION"));
}

#[test]
fn saved_demo_can_be_reloaded_replanned_and_verified() {
    let temp = tempfile::tempdir().unwrap();
    demo(temp.path());
    assert!(
        run(&["inspect", "--session", "demo/session.json"], temp.path())
            .status
            .success()
    );
    assert!(
        run(
            &[
                "plan",
                "--session",
                "demo/session.json",
                "--out",
                "replanned.json"
            ],
            temp.path()
        )
        .status
        .success()
    );
    assert!(
        run(
            &[
                "verify",
                "--session",
                "demo/session.json",
                "--profile",
                "replanned.json",
                "--measurements",
                "demo/verification-measurements.json",
                "--out",
                "checked.json"
            ],
            temp.path()
        )
        .status
        .success()
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("checked.json")).unwrap()).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["origin"], "simulated");
    assert_eq!(report["before"]["arrival_spread_ms"], 2.0);
    assert_eq!(report["before"]["level_spread_db"], 5.0);
    assert_eq!(report["after"]["arrival_spread_ms"], 0.0);
}

#[test]
fn existing_sessions_and_profiles_are_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    demo(temp.path());
    let path = temp.path().join("demo/profile.json");
    let original = std::fs::read(&path).unwrap();
    assert!(
        !run(&["demo", "--out-dir", "demo"], temp.path())
            .status
            .success()
    );
    assert!(
        !run(
            &[
                "plan",
                "--session",
                "demo/session.json",
                "--out",
                "demo/profile.json"
            ],
            temp.path()
        )
        .status
        .success()
    );
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn invalid_input_and_unsuccessful_verification_have_distinct_exit_codes() {
    let temp = tempfile::tempdir().unwrap();
    demo(temp.path());
    let path = temp.path().join("demo/verification-measurements.json");
    let mut batch: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    batch["observations"][0]["arrival_ms"] = 30.0.into();
    std::fs::write(&path, serde_json::to_vec(&batch).unwrap()).unwrap();
    let args = [
        "verify",
        "--session",
        "demo/session.json",
        "--profile",
        "demo/profile.json",
        "--measurements",
        "demo/verification-measurements.json",
        "--out",
        "failed.json",
    ];
    assert_eq!(run(&args, temp.path()).status.code(), Some(2));
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("failed.json")).unwrap()).unwrap();
    assert_eq!(report["passed"], false);
    std::fs::write(&path, b"not json").unwrap();
    assert_eq!(run(&args, temp.path()).status.code(), Some(1));
}

#[test]
fn unknown_fields_and_oversized_inputs_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    demo(temp.path());
    let path = temp.path().join("demo/session.json");
    let mut session: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    session["unrecognized_setting"] = true.into();
    std::fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();
    assert_eq!(
        run(&["inspect", "--session", "demo/session.json"], temp.path())
            .status
            .code(),
        Some(1)
    );
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(8 * 1024 * 1024 + 1).unwrap();
    assert_eq!(
        run(&["inspect", "--session", "demo/session.json"], temp.path())
            .status
            .code(),
        Some(1)
    );
}
