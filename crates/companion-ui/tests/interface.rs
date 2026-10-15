mod support;
use companion_ui::CompanionApp;
use eframe::egui::{self, Event, PointerButton, Pos2, Rect, Vec2, epaint::Shape};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn frame(
    app: &mut CompanionApp,
    ctx: &egui::Context,
    events: Vec<Event>,
    size: Vec2,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    )
}
fn texts(shape: &Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        Shape::Text(t) => out.push((
            t.galley.text().to_owned(),
            t.galley.rect.translate(t.pos.to_vec2()),
        )),
        Shape::Vec(shapes) => {
            for s in shapes {
                texts(s, out);
            }
        }
        _ => {}
    }
}
fn labels(output: &egui::FullOutput) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    for shape in &output.shapes {
        texts(&shape.shape, &mut out);
    }
    out
}
fn click(app: &mut CompanionApp, ctx: &egui::Context, pos: Pos2, size: Vec2) {
    frame(
        app,
        ctx,
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
        size,
    );
    frame(
        app,
        ctx,
        vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
        size,
    );
}

#[test]
fn speaker_rows_and_controls_fit_the_minimum_window_and_measure_dispatches() {
    let ctx = egui::Context::default();
    let (controller, starts) = support::ready();
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(850.0, 640.0);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(text.iter().any(|(t, _)| t == "Front left"));
    assert!(text.iter().any(|(t, _)| t.contains("NO AUDIO")));
    let (_, button) = text
        .iter()
        .find(|(t, _)| t == "Measure")
        .expect("per-speaker button");
    assert!(
        button.right() <= size.x && button.left() > 310.0,
        "{button:?}"
    );
    click(&mut app, &ctx, button.center(), size);
    let deadline = Instant::now() + Duration::from_secs(2);
    while starts.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    assert_eq!(
        app.controller.state.takes.len(),
        0,
        "worker is separate from UI polling"
    );
    app.controller.poll();
}

#[test]
fn missing_microphone_disables_measure_and_disconnected_state_has_no_invented_speakers() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.settings.device = None;
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    let (_, button) = text.iter().find(|(t, _)| t == "Measure").unwrap();
    click(&mut app, &ctx, button.center(), size);
    assert_eq!(starts.load(Ordering::Relaxed), 0);
    app.controller.disconnect();
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(t, _)| t == "Your speakers will appear here")
    );
    assert!(!text.iter().any(|(t, _)| t == "Measure"));
}

#[test]
fn microphone_and_channel_can_be_selected_in_the_interface() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.settings.device = None;
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    let choice = text
        .iter()
        .find(|(t, _)| t == "Choose an input…")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, choice, size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    let choice = text
        .iter()
        .find(|(t, _)| t == "Preview audio interface")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, choice, size);
    assert_eq!(
        app.controller.settings.device.as_deref(),
        Some("Preview audio interface")
    );
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    let channel = text
        .iter()
        .find(|(t, _)| t == "1 · Mic 1")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, channel, size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    let channel = text
        .iter()
        .find(|(t, _)| t == "2 · Mic 2")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, channel, size);
    assert_eq!(app.controller.settings.input_channel, 2);
    assert_eq!(
        starts.load(Ordering::Relaxed),
        0,
        "selection must not start recording"
    );
}

#[test]
fn restart_restores_settings_reconnects_and_never_measures_automatically() {
    use companion_service::preferences::PreferenceStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let (mut controller, _) = support::ready();
    controller.settings.input_channel = 2;
    controller.settings.reference_output = Some("Front right".into());
    controller.excluded_outputs.insert("Center".into());
    let settings = controller.settings.clone();
    let mut store = PreferenceStore::open(path.clone()).unwrap();
    store.update(
        &settings,
        "127.0.0.1:9003",
        &controller.excluded_outputs,
        "sessions/previous",
    );
    drop(store);
    let ctx = egui::Context::default();
    let (controller, starts) = support::ready();
    let mut app = CompanionApp::with_preferences(
        &ctx,
        controller,
        PreferenceStore::open(path.clone()).unwrap(),
    );
    let size = Vec2::new(1160.0, 800.0);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.controller.busy() {
        frame(&mut app, &ctx, vec![], size);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.controller.settings, settings);
    assert!(app.controller.excluded_outputs.contains("Center"));
    assert_eq!(
        app.controller.state.endpoint,
        Some("127.0.0.1:9003".parse().unwrap())
    );
    assert_eq!(starts.load(Ordering::Relaxed), 0);
    app.controller.settings.device = Some("Missing interface".into());
    app.controller.settings.reference_output = Some("Missing speaker".into());
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(t, _)| t.contains("Selected input unavailable"))
    );
    assert!(
        text.iter()
            .any(|(t, _)| t.contains("Selected reference is missing"))
    );
    // Exit immediately after an edit, without another draw.
    app.controller.settings.position_id = "new-position".into();
    drop(app);
    let store = PreferenceStore::open(path).unwrap();
    assert_eq!(store.preferences().settings.position_id, "new-position");
    assert_eq!(
        store.preferences().settings.device.as_deref(),
        Some("Missing interface")
    );
    assert_eq!(store.preferences().review_path, "sessions/previous");
}

#[test]
fn active_measurement_progress_stays_finite_across_frames_and_window_sizes() {
    use companion_service::{controller::Job, measurement::MeasureProgress};
    let ctx = egui::Context::default();
    let (mut controller, _) = support::ready();
    controller.state.job = Some(Job::Measure {
        output_id: "Front left".into(),
    });
    let mut app = CompanionApp::preview(&ctx, controller);
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        for progress in [
            MeasureProgress::Preparing,
            MeasureProgress::LeadIn,
            MeasureProgress::Saving,
            MeasureProgress::Analyzing,
        ] {
            app.controller.state.progress = Some(progress);
            for _ in 0..3 {
                let output = frame(&mut app, &ctx, vec![], size);
                let text = labels(&output);
                assert!(text.iter().any(|(s, _)| s == "Stop"));
                assert!(text.iter().all(|(_, rect)| rect.is_finite()));
            }
        }
    }
}

#[test]
fn captured_sweeps_with_ambiguous_arrivals_show_levels_and_withhold_timing() {
    use companion_service::{controller::Take, measurement::TakeOutcome};
    use measurement_capture::timing::TimingReport;
    let ctx = egui::Context::default();
    let (mut controller, _) = support::ready();
    controller.state.takes.push(Take {
        output_id: "Front left".into(),
        revision: "ui-preview:1".into(),
        settings: controller.settings.clone(),
        outcome: Ok(TakeOutcome {
            directory: "sessions/synthetic-review".into(),
            accepted: true,
            cancelled: false,
            rejection_reason: None,
            diagnostics: None,
            impulse: None,
            impulse_error: None,
            timing: Some(TimingReport {
                method: "synthetic-ui-fixture",
                reference_start_frames: [24000.0, 168000.0],
                target_start_frame: 96000.0,
                clock_drift_ppm: None,
                relative_peak_delay_ms: None,
                ambiguous_peaks: true,
                competing_peak_ratio: [0.9; 3],
                normalized_correlation: [0.3; 3],
                signal_to_noise_db: [25.0; 3],
                sweep_rms_dbfs: [-26.0; 3],
                noise_rms_dbfs: -51.0,
                valid_for_correction: false,
            }),
        }),
    });
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(t, _)| t == "Captured · timing unresolved")
    );
    let (_, button) = text.iter().find(|(t, _)| t == "View result").unwrap();
    click(&mut app, &ctx, button.center(), size);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert_eq!(text.iter().filter(|(t, _)| t == "Unresolved").count(), 2);
    assert!(text.iter().any(|(t, _)| t == "25.0 dB"));
    assert!(
        text.iter()
            .any(|(t, _)| t.contains("More volume will not resolve this"))
    );
    assert!(!text.iter().any(|(t, _)| t.contains("Take rejected")));
    assert!(text.iter().all(|(_, rect)| rect.is_finite()));
}

#[test]
fn offline_arrival_review_renders_without_a_renderer_and_keeps_geometry_finite() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.disconnect();
    controller.state.review = Some(support::arrival_review());
    let mut app = CompanionApp::preview(&ctx, controller);
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        let output = frame(&mut app, &ctx, vec![], size);
        let text = labels(&output);
        assert!(
            text.iter()
                .any(|(t, _)| t.contains("Arrival candidate  +2.400 ms"))
        );
        assert!(text.iter().any(|(t, _)| t.contains("Impulse envelope")));
        assert!(text.iter().all(|(_, r)| r.is_finite()));
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn unresolved_impulse_review_shows_reasons_instead_of_a_delay() {
    let ctx = egui::Context::default();
    let (mut controller, _) = support::ready();
    let mut review = support::arrival_review();
    let report = std::sync::Arc::make_mut(review.impulse.as_mut().unwrap());
    report.relative_arrival_ms = None;
    report.clock_drift_ppm = None;
    report.uncertainty_ms = None;
    report.unresolved_reasons = vec!["speaker: detection thresholds disagree".into()];
    controller.state.review = Some(review);
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    frame(&mut app, &ctx, vec![], size);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(t, _)| t == "speaker: detection thresholds disagree")
    );
    assert!(!text.iter().any(|(t, _)| t.contains("Arrival candidate  ")));
}

#[test]
fn rejected_target_review_explains_routing_evidence_without_fabricating_a_delay() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.disconnect();
    controller.state.review = Some(support::missing_target_review());
    let mut app = CompanionApp::preview(&ctx, controller);
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        let output = frame(&mut app, &ctx, vec![], size);
        let text = labels(&output);
        assert!(text.iter().any(|(t, _)| t == "No target sweep detected"));
        assert!(
            text.iter()
                .any(|(t, _)| t.contains("Original take rejected"))
        );
        assert!(text.iter().any(|(t, _)| t == "Reference after"));
        assert!(
            text.iter()
                .any(|(t, _)| t.contains("Check the selected speaker's output route"))
        );
        assert!(!text.iter().any(|(t, _)| t.contains("Arrival candidate  ")));
        assert!(text.iter().all(|(_, r)| r.is_finite()));
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn capture_progress_keeps_speaker_rows_in_place_for_single_and_batch_capture() {
    use companion_service::{controller::Job, measurement::MeasureProgress};
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let ctx = egui::Context::default();
        let (controller, _) = support::ready();
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        let baseline = labels(&frame(&mut app, &ctx, vec![], size));
        let rows: Vec<_> = baseline
            .iter()
            .filter(|(s, r)| r.left() > 310.0 && (s == "Front left" || s == "Front right"))
            .cloned()
            .collect();
        for batch in [false, true] {
            app.controller.state.job = Some(if batch {
                Job::Batch {
                    output_ids: vec!["Front left".into(), "Front right".into()],
                    current: 0,
                }
            } else {
                Job::Measure {
                    output_id: "Front left".into(),
                }
            });
            for phase in [
                MeasureProgress::OpeningInput,
                MeasureProgress::LeadIn,
                MeasureProgress::Analyzing,
            ] {
                app.controller.state.progress = Some(phase);
                let text = labels(&frame(&mut app, &ctx, vec![], size));
                for (name, rect) in &rows {
                    let now = text
                        .iter()
                        .find(|(s, r)| s == name && r.left() > 310.0)
                        .unwrap()
                        .1;
                    assert!(
                        (now.top() - rect.top()).abs() < 0.1,
                        "row {name} moved: {rect:?} -> {now:?}"
                    );
                }
                assert!(text.iter().any(|(s, _)| s == "Stop"));
            }
        }
    }
}
