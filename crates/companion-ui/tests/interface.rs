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

fn scroll_results(app: &mut CompanionApp, ctx: &egui::Context, size: Vec2, delta: f32) {
    let pos = Pos2::new(size.x - 70.0, size.y - 85.0);
    frame(
        app,
        ctx,
        vec![
            Event::PointerMoved(pos),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, delta),
                modifiers: Default::default(),
            },
        ],
        size,
    );
    for _ in 0..20 {
        frame(app, ctx, vec![], size);
    }
}

fn find_result_text(app: &mut CompanionApp, ctx: &egui::Context, size: Vec2, needle: &str) -> Rect {
    scroll_results(app, ctx, size, 10000.0);
    for _ in 0..80 {
        let output = frame(app, ctx, vec![], size);
        let visible = output.shapes.iter().flat_map(|shape| {
            let mut labels = Vec::new();
            texts(&shape.shape, &mut labels);
            labels.retain(|(_, rect)| shape.clip_rect.contains(rect.center()));
            labels
        });
        if let Some((_, rect)) = visible.into_iter().find(|(t, r)| {
            (if needle == "Arrival diagnostics" {
                t == needle
            } else {
                t.contains(needle)
            }) && r.bottom() < size.y - 30.0
        }) {
            return rect;
        }
        scroll_results(app, ctx, size, -40.0);
    }
    panic!("Result text not reachable by scrolling: {needle}");
}

// Speaker/session controls scroll independently above the fixed individual-result panel.
fn scroll_speakers(app: &mut CompanionApp, ctx: &egui::Context, size: Vec2, delta: f32) {
    let text = labels(&frame(app, ctx, vec![], size));
    let lower = text
        .iter()
        .find(|(s, _)| s == "Measurement results" || s.ends_with(" · Measurement result"))
        .unwrap()
        .1
        .top();
    let pos = Pos2::new(size.x - 70.0, lower - 40.0);
    frame(
        app,
        ctx,
        vec![
            Event::PointerMoved(pos),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, delta),
                modifiers: Default::default(),
            },
        ],
        size,
    );
    for _ in 0..20 {
        frame(app, ctx, vec![], size);
    }
}

fn find_speaker_text(
    app: &mut CompanionApp,
    ctx: &egui::Context,
    size: Vec2,
    needle: &str,
) -> Rect {
    scroll_speakers(app, ctx, size, 10000.0);
    for _ in 0..80 {
        let output = frame(app, ctx, vec![], size);
        let lower = labels(&output)
            .iter()
            .find(|(s, _)| s == "Measurement results" || s.ends_with(" · Measurement result"))
            .unwrap()
            .1
            .top();
        for shape in &output.shapes {
            let mut text = vec![];
            texts(&shape.shape, &mut text);
            if let Some((_, rect)) = text.iter().find(|(s, r)| {
                (if matches!(needle, "Front left" | "Front right" | "Center" | "Measure") {
                    s == needle
                } else {
                    s.contains(needle)
                }) && r.left() > 310.0
                    && shape.clip_rect.contains(r.center())
                    && r.bottom() < lower - 12.0
            }) {
                return *rect;
            }
        }
        scroll_speakers(app, ctx, size, -40.0);
    }
    panic!("Speaker text not reachable by scrolling: {needle}");
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
fn input_meter_and_capture_choices_explain_silence_without_starting_audio() {
    use companion_service::controller::{InputCheck, Job};
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.excluded_outputs.insert("Surround right".into());
    controller.state.input_check = Some(InputCheck {
        request: controller.input_request().unwrap(),
        level: Some(measurement_capture::input::InputLevel::analyze(
            &[0.0; 100],
            0,
        )),
        error: None,
    });
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 900.0);
    frame(&mut app, &ctx, vec![], size);
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    for expected in [
        "Check microphone",
        "Measure selected speakers",
        "Align levels and delays",
        "No microphone signal received",
        "Last check",
        "Excluded: Surround right",
    ] {
        assert!(
            text.iter().any(|(t, _)| t.contains(expected)),
            "missing {expected}"
        );
    }
    app.controller.state.job = Some(Job::InputCheck);
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(text.iter().any(|(t, _)| t.contains("LIVE")));
    let measure = text
        .iter()
        .find(|(t, _)| t == "Measure selected speakers")
        .unwrap()
        .1;
    click(&mut app, &ctx, measure.center(), size);
    assert_eq!(starts.load(Ordering::Relaxed), 0);
    app.controller.state.job = None;
    app.controller.settings.input_channel = 2;
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(
        !text
            .iter()
            .any(|(t, _)| t.contains("No microphone signal received"))
    );
}

#[test]
fn restart_restores_settings_reconnects_and_never_measures_automatically() {
    use companion_service::preferences::PreferenceStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let (mut controller, _) = support::ready();
    controller.settings.output_root = dir.path().join("sessions").display().to_string();
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
    // Setup has internal scrolling; the new meter must not resize the speaker pane.
    frame(
        &mut app,
        &ctx,
        vec![
            Event::PointerMoved(Pos2::new(150.0, 500.0)),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -200.0),
                modifiers: Default::default(),
            },
        ],
        size,
    );
    for _ in 0..20 {
        frame(&mut app, &ctx, vec![], size);
    }
    let text = labels(&frame(&mut app, &ctx, vec![], size));
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
                band_noise: None,
                warnings: vec![],
                method: "synthetic-ui-fixture".into(),
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
    let row = find_speaker_text(&mut app, &ctx, size, "Front left");
    click(&mut app, &ctx, row.center(), size);
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert_eq!(text.iter().filter(|(t, _)| t == "Unresolved").count(), 2);
    assert!(text.iter().any(|(t, _)| t == "25.0 dB"));
    find_result_text(&mut app, &ctx, size, "More volume will not resolve this");
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
        find_result_text(&mut app, &ctx, size, "Impulse envelope");
        let toggle = find_result_text(&mut app, &ctx, size, "Arrival diagnostics");
        // Each size uses the same UI state; close after inspecting it below.
        click(&mut app, &ctx, toggle.center(), size);
        find_result_text(&mut app, &ctx, size, "Arrival candidate  +2.400 ms");
        let text = labels(&frame(&mut app, &ctx, vec![], size));
        assert!(text.iter().all(|(_, r)| r.is_finite()));
        let toggle = find_result_text(&mut app, &ctx, size, "Arrival diagnostics");
        click(&mut app, &ctx, toggle.center(), size);
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn frequency_and_timing_views_switch_without_playback_and_handle_missing_points() {
    use companion_ui::ResultPlot;
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.disconnect();
    controller.state.review = Some(support::arrival_review());
    let mut app = CompanionApp::preview(&ctx, controller);
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        for (mode, title) in [
            (
                ResultPlot::FrequencyResponse,
                "Frequency response · relative dB",
            ),
            (
                ResultPlot::EnergyArrival,
                "Peak-energy arrival vs frequency · ms",
            ),
            (ResultPlot::GroupDelay, "Group delay vs frequency · ms"),
        ] {
            app.result_plot = mode;
            frame(&mut app, &ctx, vec![], size);
            find_result_text(&mut app, &ctx, size, title);
            let output = frame(&mut app, &ctx, vec![], size);
            let text = labels(&output);
            assert!(text.iter().any(|(t, _)| t == title), "{mode:?}");
            assert!(text.iter().all(|(_, rect)| rect.is_finite()));
            assert_eq!(starts.load(Ordering::Relaxed), 0);
        }
    }
    let size = Vec2::new(1160.0, 800.0);
    let position = find_result_text(&mut app, &ctx, size, "Frequency response").center();
    click(&mut app, &ctx, position, size);
    assert_eq!(app.result_plot, ResultPlot::FrequencyResponse);
    let review = app.controller.state.review.as_mut().unwrap();
    let report = std::sync::Arc::make_mut(review.impulse.as_mut().unwrap());
    for point in &mut report.segments[1].spectral.as_mut().unwrap().points {
        point.magnitude_db = None;
        point.peak_time_ms = None;
        point.group_delay_ms = None;
    }
    find_result_text(&mut app, &ctx, size, "No reliable points in this view.");
    let output = frame(&mut app, &ctx, vec![], size);
    assert!(labels(&output).iter().all(|(_, r)| r.is_finite()));
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
    let toggle = find_result_text(&mut app, &ctx, size, "Arrival diagnostics");
    click(&mut app, &ctx, toggle.center(), size);
    find_result_text(
        &mut app,
        &ctx,
        size,
        "speaker: detection thresholds disagree",
    );
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(t, _)| t == "speaker: detection thresholds disagree")
    );
    assert!(!text.iter().any(|(t, _)| t.contains("Arrival candidate  ")));
}

#[test]
fn signed_and_virtual_burst_views_stay_below_speakers_and_never_play_audio() {
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let ctx = egui::Context::default();
        let (mut controller, starts) = support::ready();
        controller.state.review = Some(support::arrival_review());
        let mut app = CompanionApp::preview(&ctx, controller);
        app.result_plot = companion_ui::ResultPlot::SignedWaveform;
        frame(&mut app, &ctx, vec![], size);
        let output = frame(&mut app, &ctx, vec![], size);
        let text = labels(&output);
        let results = text
            .iter()
            .find(|(t, _)| t == "Front right · Measurement result")
            .unwrap()
            .1;
        let speakers = text.iter().find(|(t, _)| t == "Speakers").unwrap().1;
        assert!(results.top() > speakers.bottom());
        assert_eq!(
            ctx.layer_id_at(results.center()).unwrap().order,
            egui::Order::Background
        );
        find_result_text(&mut app, &ctx, size, "Signed impulse response");
        let tab = find_result_text(&mut app, &ctx, size, "Virtual burst");
        click(&mut app, &ctx, tab.center(), size);
        assert_eq!(app.result_plot, companion_ui::ResultPlot::ToneBurst);
        find_result_text(
            &mut app,
            &ctx,
            size,
            "Virtual tone burst · predicted from measured IR",
        );
        let frequency = find_result_text(&mut app, &ctx, size, "1000 Hz");
        click(&mut app, &ctx, frequency.center(), size);
        let output = frame(&mut app, &ctx, vec![], size);
        let frequency = labels(&output)
            .into_iter()
            .find(|(t, _)| t == "2000 Hz")
            .unwrap()
            .1;
        click(&mut app, &ctx, frequency.center(), size);
        find_result_text(&mut app, &ctx, size, "6 cycles · 3.00 ms");
        find_result_text(&mut app, &ctx, size, "No burst played");
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
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
        find_result_text(&mut app, &ctx, size, "No target sweep detected");
        let details = find_result_text(&mut app, &ctx, size, "Original capture details");
        if size.x == 850.0 {
            click(&mut app, &ctx, details.center(), size);
        }
        find_result_text(&mut app, &ctx, size, "Original take rejected");
        find_result_text(&mut app, &ctx, size, "Reference after");
        find_result_text(
            &mut app,
            &ctx,
            size,
            "Check the selected speaker's output route",
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
        for kind in 0..3 {
            app.controller.state.job = Some(if kind == 2 {
                Job::Alignment {
                    output_ids: vec!["Front left".into(), "Front right".into()],
                    current: 0,
                    pass: 1,
                }
            } else if kind == 1 {
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
                MeasureProgress::CheckingInput(measurement_capture::input::InputLevel::analyze(
                    &[0.0; 100],
                    0,
                )),
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

#[test]
fn alignment_session_stays_above_individual_results_and_unqualified_actions_stay_disabled() {
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let ctx = egui::Context::default();
        let (mut controller, starts) = support::ready();
        controller.state.alignment = Some(companion_service::alignment::Alignment {
            schema_version: 1,
            directory: "preview-alignment".into(),
            endpoint: "127.0.0.1:9000".parse().unwrap(),
            settings: controller.settings.clone(),
            snapshot: controller.state.snapshot.clone().unwrap(),
            rows: vec![],
            pass_reports: Some(vec![companion_service::alignment::PassReport {
                analyzed_speakers: 0,
                error: Some("No microphone signal received".into()),
            }]),
            verification_pass_reports: None,
            blockers: vec!["Two independent captures are required".into()],
            session: None,
            profile: None,
            applied_snapshot: None,
            restored_snapshot: None,
            verification: None,
            verification_note: None,
            application_uncertain: false,
            physical_levels_changed: false,
        });
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        let text = labels(&frame(&mut app, &ctx, vec![], size));
        let speakers = text.iter().find(|(t, _)| t == "Speakers").unwrap().1;
        let alignment = text
            .iter()
            .find(|(t, _)| t == "Alignment session")
            .unwrap()
            .1;
        assert!(alignment.top() > speakers.bottom());
        assert!(text.iter().all(|(_, r)| r.is_finite()));
        assert!(
            !text
                .iter()
                .any(|(s, _)| s == "Alignment" || s == "Delay and level alignment")
        );
        let lower = text
            .iter()
            .find(|(s, _)| s == "Measurement results")
            .unwrap()
            .1;
        assert!(alignment.bottom() < lower.top());
        let details = find_speaker_text(&mut app, &ctx, size, "Alignment session details");
        click(&mut app, &ctx, details.center(), size);
        find_speaker_text(&mut app, &ctx, size, "Baseline pass 1 / 2: stopped");
        find_speaker_text(&mut app, &ctx, size, "Baseline pass 2 / 2: not started");
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Correction proposal blocked by session issues",
        );
        let apply = find_speaker_text(&mut app, &ctx, size, "Apply gains and delays");
        click(&mut app, &ctx, apply.center(), size);
        assert!(!app.controller.busy());
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn alignment_estimates_and_targets_stay_in_the_speaker_rows_while_individual_results_stay_below() {
    for proposal in [false, true] {
        for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
            let ctx = egui::Context::default();
            let (mut controller, starts) = support::ready();
            controller.state.alignment = Some(support::alignment(&controller, proposal));
            controller.state.review = Some(support::arrival_review());
            let mut app = CompanionApp::preview(&ctx, controller);
            for _ in 0..3 {
                frame(&mut app, &ctx, vec![], size);
            }
            let heading =
                find_result_text(&mut app, &ctx, size, "Front right · Measurement result");
            for value in [
                "+0.125 ms",
                "-6.50 dB",
                "0.020 ms / 0.10 dB",
                "Quality",
                "Good",
            ] {
                let rect = find_speaker_text(&mut app, &ctx, size, value);
                assert!(
                    rect.bottom() < heading.top(),
                    "{value} appeared in the individual result panel"
                );
                assert!(
                    rect.left() > 310.0 && rect.right() < size.x,
                    "{value} escaped the speaker card: {rect:?}"
                );
            }
            let text = labels(&frame(&mut app, &ctx, vec![], size));
            let name = text
                .iter()
                .find(|(s, r)| s == "Front left" && r.left() > 310.0)
                .unwrap()
                .1;
            let value = text.iter().find(|(s, _)| s == "+0.125 ms").unwrap().1;
            assert!(value.top() > name.bottom());
            assert!(
                !text
                    .iter()
                    .any(|(s, _)| s == "Alignment" || s == "Delay and level alignment")
            );
            if proposal {
                for value in [
                    "Gain: before → target",
                    "-1.50 → -2.50 dB",
                    "Delay: before → target",
                    "0.000 → 1.000 ms",
                ] {
                    find_speaker_text(&mut app, &ctx, size, value);
                }
            } else {
                for value in [
                    "Excluded from correction",
                    "Weak target signal · diagnostic estimate only",
                    "Long delay:",
                ] {
                    find_speaker_text(&mut app, &ctx, size, value);
                }
            }
            let row_before = find_speaker_text(&mut app, &ctx, size, "Front left");
            app.controller.state.job = Some(companion_service::controller::Job::Alignment {
                output_ids: vec!["Front left".into(), "Front right".into()],
                current: 0,
                pass: 1,
            });
            for phase in [
                companion_service::measurement::MeasureProgress::OpeningInput,
                companion_service::measurement::MeasureProgress::Tail,
                companion_service::measurement::MeasureProgress::Analyzing,
            ] {
                app.controller.state.progress = Some(phase);
                let text = labels(&frame(&mut app, &ctx, vec![], size));
                let row_after = text
                    .iter()
                    .find(|(s, r)| s == "Front left" && r.left() > 310.0)
                    .unwrap()
                    .1;
                assert!(
                    (row_after.top() - row_before.top()).abs() < 0.1,
                    "alignment row moved during capture: {row_before:?} -> {row_after:?}"
                );
                assert!(
                    text.iter()
                        .any(|(s, _)| s == "Front right · Measurement result")
                );
            }
            app.controller.state.job = None;
            app.controller.state.progress = None;
            // A new/updated alignment cannot replace the individual selection or reopen a tab.
            app.controller.state.alignment.as_mut().unwrap().directory =
                "synthetic-updated-alignment".into();
            find_result_text(&mut app, &ctx, size, "Front right · Measurement result");
            find_result_text(
                &mut app,
                &ctx,
                size,
                "Arrival relative to Front left: +2.400 ms",
            );
            assert_eq!(starts.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn restart_restores_graph_selection_and_speaker_results_even_without_the_renderer() {
    use companion_service::{
        controller::{Backend, Controller, Take},
        files::write_json,
        measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
        preferences::PreferenceStore,
    };
    use std::{
        net::SocketAddr,
        sync::{Arc, atomic::AtomicBool},
    };
    struct Offline;
    impl Backend for Offline {
        fn inputs(&self) -> Result<Vec<measurement_capture::InputDevice>, String> {
            Ok(vec![])
        }
        fn discover(&self, _: SocketAddr) -> Result<omniphony_adapter::Snapshot, String> {
            Err("renderer is offline".into())
        }
        fn measure(
            &self,
            _: MeasureRequest,
            _: &AtomicBool,
            _: &mut dyn FnMut(MeasureProgress),
        ) -> Result<TakeOutcome, String> {
            panic!("startup must never measure")
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("take-1");
    std::fs::create_dir(&folder).unwrap();
    let path = dir.path().join("settings.json");
    let (mut controller, _) = support::ready();
    controller.settings.output_root = dir.path().display().to_string();
    let settings = controller.settings.clone();
    let snapshot = controller.state.snapshot.clone().unwrap();
    let review = support::arrival_review();
    let mut saved_review = review.clone();
    saved_review.output_id = "Front left".into();
    saved_review.microphone_id = settings.microphone_id.clone();
    saved_review.position_id = settings.position_id.clone();
    saved_review.directory = folder.clone();
    saved_review.report_file = folder.join("analysis-test.json");
    write_json(&saved_review.report_file, &saved_review).unwrap();
    let (_, unrelated) = saved_result(&controller, &dir.path().join("unrelated"), "TBR");
    controller.state.review = Some(unrelated);
    write_json(&folder.join("snapshot.json"), &snapshot).unwrap();
    write_json(&folder.join("stimulus.json"),&serde_json::json!({"output_id":"Front left","microphone_id":settings.microphone_id,"position_id":settings.position_id,"reference_output_id":"Front left","signal":settings.sweep})).unwrap();
    write_json(&folder.join("capture-report.json"),&serde_json::json!({"schema_version":3,"accepted_for_analysis":true,"rejection_reason":null,"capture":{},"impulse":review.impulse,"timing":review.timing,"diagnostics":review.diagnostics})).unwrap();
    std::fs::write(folder.join("recording.wav"), b"cached UI fixture").unwrap();
    let take = Take {
        output_id: "Front left".into(),
        revision: snapshot.revision.clone(),
        settings: settings.clone(),
        outcome: Ok(TakeOutcome {
            directory: folder.clone(),
            accepted: true,
            cancelled: false,
            rejection_reason: None,
            diagnostics: Some(review.diagnostics),
            timing: review.timing,
            impulse: review.impulse,
            impulse_error: None,
        }),
    };
    // A close during cache loading can leave the old global review alongside
    // a different selected take. The explicit take selection must win on restart.
    controller.state.takes.push(take);
    let mut store = PreferenceStore::open(path.clone()).unwrap();
    store.update_view(companion_service::preferences::ViewPreferences {
        selected_take: Some(folder.clone()),
        selected_output: Some("Front left".into()),
        saved_selected: true,
        show_alignment: true, // Legacy tab preference must not take over the individual result.
        ..Default::default()
    });
    store.update(&settings, "127.0.0.1:9000", &Default::default(), "");
    store.update_workspace(companion_service::workspace::WorkspaceIndex::from_state(
        &controller.state,
    ));
    drop(store);
    let ctx = egui::Context::default();
    let size = Vec2::new(1160.0, 1000.0);
    let mut app = CompanionApp::with_preferences(
        &ctx,
        controller,
        PreferenceStore::open(path.clone()).unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.controller.busy() {
        frame(&mut app, &ctx, vec![], size);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = frame(&mut app, &ctx, vec![], size);
    let restored_labels = labels(&output);
    assert!(
        restored_labels
            .iter()
            .any(|(s, _)| s == "Front left · Measurement result")
    );
    assert!(
        restored_labels
            .iter()
            .any(|(s, _)| s == "Latest saved analysis")
    );
    assert!(!restored_labels.iter().any(|(s, _)| s.contains("TBR")));
    let button = labels(&output)
        .into_iter()
        .find(|(s, _)| s == "Previous measurements")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, button, size);
    let output = frame(&mut app, &ctx, vec![], size);
    let item = labels(&output)
        .into_iter()
        .find(|(s, _)| s.starts_with("Front left ·") && s.ends_with("take-1"))
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, item, size);
    app.result_plot = companion_ui::ResultPlot::FrequencyResponse;
    let output = frame(&mut app, &ctx, vec![], size);
    let segment = labels(&output)
        .into_iter()
        .find(|(s, _)| s == "Speaker")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, segment, size);
    let output = frame(&mut app, &ctx, vec![], size);
    let reference = labels(&output)
        .into_iter()
        .find(|(s, _)| s == "Reference after")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, reference, size);
    frame(&mut app, &ctx, vec![], size);
    // Use eframe's real serializer, independently of the service preferences.
    #[derive(Default)]
    struct MemoryStorage(std::collections::BTreeMap<String, String>);
    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.into(), value);
        }
        fn flush(&mut self) {}
    }
    let mut storage = MemoryStorage::default();
    ctx.memory(|m| eframe::set_value(&mut storage, "memory", m));
    drop(app);

    let ctx = egui::Context::default();
    ctx.memory_mut(|m| *m = eframe::get_value(&storage, "memory").unwrap());
    let mut app = CompanionApp::with_preferences(
        &ctx,
        Controller::new(Arc::new(Offline)),
        PreferenceStore::open(path.clone()).unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.controller.busy() {
        frame(&mut app, &ctx, vec![], size);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.result_plot, companion_ui::ResultPlot::FrequencyResponse);
    assert!(app.controller.state.snapshot.is_none());
    assert!(app.controller.state.saved_snapshot.is_some());
    assert!(app.controller.current_take("Front left").is_none());
    assert!(app.controller.display_take("Front left").is_some());
    assert!(app.controller.can_measure("Front left").is_err());
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(text.iter().any(|(s, _)| s.contains("Saved speaker layout")));
    assert!(
        text.iter()
            .any(|(s, _)| s.contains("Front left · Measurement result"))
    );
    assert!(text.iter().any(|(s, _)| s.contains("Frequency response")));
    assert!(text.iter().any(|(s, _)| s == "Reference after"));
    assert!(app.controller.state.workspace_warnings.is_empty());
    assert_eq!(
        app.controller.state.review.as_ref().unwrap().report_file,
        folder.join("analysis-test.json")
    );
    assert_eq!(
        std::fs::read_dir(&folder).unwrap().count(),
        5,
        "startup must not generate analysis files"
    );
    assert_eq!(app.controller.settings, settings);
}

#[test]
fn marginal_measurements_keep_graphs_and_show_separate_quality_indicators() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    let mut review = support::arrival_review();
    review.timing.as_mut().unwrap().band_noise = Some(measurement_capture::noise::BandNoise {
        band_hz: [200.0, 10000.0],
        noise_rms_dbfs: -50.0,
        signal_to_noise_db: [18.0, 19.0, 18.0],
    });
    let ir = std::sync::Arc::make_mut(review.impulse.as_mut().unwrap());
    let points = &mut ir.segments[1].spectral.as_mut().unwrap().points;
    points
        .iter_mut()
        .find(|p| p.frequency_hz > 1000.0)
        .unwrap()
        .estimated_snr_db = 17.0;
    review.quality = Some(companion_service::quality::assess(
        review.timing.as_ref(),
        review.impulse.as_deref(),
        Some(&review.diagnostics),
        None,
    ));
    assert_eq!(
        review.quality.as_ref().unwrap().level.grade,
        companion_service::quality::Grade::Limited
    );
    controller.state.review = Some(review);
    let mut app = CompanionApp::preview(&ctx, controller);
    app.result_plot = companion_ui::ResultPlot::FrequencyResponse;
    let size = Vec2::new(1160.0, 1000.0);
    for _ in 0..3 {
        frame(&mut app, &ctx, vec![], size);
    }
    for label in [
        "Acquisition: Limited",
        "Timing: Limited",
        "Level: Limited",
        "Frequency response",
    ] {
        find_result_text(&mut app, &ctx, size, label);
    }
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

fn finish_worker(app: &mut CompanionApp, ctx: &egui::Context, size: Vec2) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        frame(app, ctx, vec![], size);
        if !app.controller.busy() {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn loaded_alignment_pass_opens_its_latest_analysis_and_labels_the_original_verdict_as_history() {
    use companion_service::{
        alignment::{Alignment, Row},
        files::write_json,
    };
    let dir = tempfile::tempdir().unwrap();
    let (mut controller, starts) = support::ready();
    let (first, _) = saved_result(&controller, &dir.path().join("pass-1"), "Front right");
    let (second, _) = saved_result(&controller, &dir.path().join("pass-2"), "Front right");
    let originals: Vec<_> = [&first, &second]
        .iter()
        .map(|t| {
            let path = t
                .outcome
                .as_ref()
                .unwrap()
                .directory
                .join("capture-report.json");
            (path.clone(), std::fs::read(path).unwrap())
        })
        .collect();
    let alignment = Alignment {
        schema_version: 1,
        directory: dir.path().to_owned(),
        endpoint: "127.0.0.1:9000".parse().unwrap(),
        settings: controller.settings.clone(),
        snapshot: controller.state.snapshot.clone().unwrap(),
        rows: vec![Row {
            output_id: "Front right".into(),
            take_folders: vec![
                first.outcome.as_ref().unwrap().directory.clone(),
                second.outcome.as_ref().unwrap().directory.clone(),
            ],
            repeats: vec![],
            quality: vec![],
            qualified: None,
            reason: None,
        }],
        pass_reports: None,
        verification_pass_reports: None,
        blockers: vec!["Diagnostic review only".into()],
        session: None,
        profile: None,
        applied_snapshot: None,
        restored_snapshot: None,
        verification: None,
        verification_note: None,
        application_uncertain: false,
        physical_levels_changed: false,
    };
    write_json(&dir.path().join("alignment-state.json"), &alignment).unwrap();
    controller.state.takes = vec![first, second];
    let mut unrelated = support::arrival_review();
    unrelated.output_id = "TBR".into();
    controller.state.review = Some(unrelated);
    controller
        .state
        .snapshot
        .as_mut()
        .unwrap()
        .outputs
        .retain(|o| o.id == "Front right");
    controller.load_alignment(dir.path().to_str().unwrap());
    let ctx = egui::Context::default();
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 1400.0);
    finish_worker(&mut app, &ctx, size);
    assert_eq!(app.controller.state.analyses.len(), 2);
    let button = find_speaker_text(&mut app, &ctx, size, "Front right");
    click(&mut app, &ctx, button.center(), size);
    finish_worker(&mut app, &ctx, size);
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    for expected in [
        "Front right · Measurement result",
        "Latest saved analysis",
        "Reanalyzed · arrival candidate",
        "Pass 2 of 2 · Alignment combines both passes",
    ] {
        assert!(
            text.iter().any(|(s, _)| s == expected),
            "missing {expected}: {text:?}"
        );
    }
    assert!(!text.iter().any(|(s, _)| s.contains("TBR")
        || s.starts_with("Take ·")
        || s.starts_with("Saved ·")
        || s.contains("Recorded · timing unavailable")
        || s.contains("Original take rejected")));
    find_result_text(
        &mut app,
        &ctx,
        size,
        "Arrival relative to Front left: +2.400 ms",
    );
    let details = find_result_text(&mut app, &ctx, size, "Original capture details");
    click(&mut app, &ctx, details.center(), size);
    find_result_text(
        &mut app,
        &ctx,
        size,
        "Original take rejected: legacy broadband signal/noise rejection",
    );
    assert_eq!(
        app.controller.state.review.as_ref().unwrap().directory,
        dir.path().join("pass-2")
    );
    assert!(
        app.controller
            .state
            .alignment
            .as_ref()
            .unwrap()
            .profile
            .is_none()
    );
    assert!(
        app.controller
            .state
            .takes
            .iter()
            .all(|t| !t.outcome.as_ref().unwrap().accepted)
    );
    for (path, bytes) in originals {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

#[test]
fn an_unrelated_review_finishing_later_cannot_steal_the_selected_speaker() {
    use companion_service::{
        analysis_cache::CachedAnalysis,
        controller::{Backend, Controller},
        review::SavedReview,
    };
    use std::sync::{Arc, Mutex, atomic::AtomicBool, mpsc};
    struct Gate {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        report: SavedReview,
    }
    impl Backend for Gate {
        fn inputs(&self) -> Result<Vec<measurement_capture::InputDevice>, String> {
            Ok(vec![])
        }
        fn discover(&self, _: std::net::SocketAddr) -> Result<omniphony_adapter::Snapshot, String> {
            Err("offline".into())
        }
        fn measure(
            &self,
            _: companion_service::measurement::MeasureRequest,
            _: &AtomicBool,
            _: &mut dyn FnMut(companion_service::measurement::MeasureProgress),
        ) -> Result<companion_service::measurement::TakeOutcome, String> {
            panic!("browsing must not measure")
        }
        fn review(&self, _: std::path::PathBuf) -> Result<SavedReview, String> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ok(self.report.clone())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut ready, _) = support::ready();
    let (take, review) = saved_result(&ready, &dir.path().join("FR"), "Front right");
    let (_, unrelated) = saved_result(&ready, &dir.path().join("TBR"), "TBR");
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let mut controller = Controller::new(Arc::new(Gate {
        entered: entered_tx,
        release: Mutex::new(release_rx),
        report: unrelated,
    }));
    controller.settings = ready.settings.clone();
    controller.state = std::mem::take(&mut ready.state);
    controller
        .state
        .snapshot
        .as_mut()
        .unwrap()
        .outputs
        .retain(|o| o.id == "Front right");
    controller.state.takes.push(take);
    controller.state.analyses.insert(
        review.directory.clone(),
        CachedAnalysis::from_review(&review),
    );
    controller.review_saved(dir.path().join("TBR").to_str().unwrap());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let ctx = egui::Context::default();
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 1000.0);
    frame(&mut app, &ctx, vec![], size);
    let button = find_speaker_text(&mut app, &ctx, size, "Front right");
    click(&mut app, &ctx, button.center(), size);
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = labels(&frame(&mut app, &ctx, vec![], size));
        assert!(
            text.iter()
                .any(|(s, _)| s == "Front right · Measurement result")
        );
        assert!(!text.iter().any(|(s, _)| s.contains("TBR")));
        if !app.controller.busy() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.controller.state.review.as_ref().unwrap().report_file,
        review.report_file
    );
    find_result_text(&mut app, &ctx, size, "Reanalyzed · arrival candidate");
}

/// Cached report fixture deliberately has unreadable PCM: browsing must not rerun DSP.
pub fn saved_result(
    controller: &companion_service::controller::Controller,
    folder: &std::path::Path,
    output: &str,
) -> (
    companion_service::controller::Take,
    companion_service::review::SavedReview,
) {
    use companion_service::files::write_json;
    std::fs::create_dir_all(folder).unwrap();
    let mut review = support::arrival_review();
    review.output_id = output.into();
    review.microphone_id = controller.settings.microphone_id.clone();
    review.position_id = controller.settings.position_id.clone();
    review.directory = folder.to_owned();
    review.report_file = folder.join("analysis-latest.json");
    review.original_accepted_for_analysis = false;
    review.original_rejection_reason = Some("legacy broadband signal/noise rejection".into());
    write_json(&review.report_file, &review).unwrap();
    write_json(
        &folder.join("snapshot.json"),
        controller.state.snapshot.as_ref().unwrap(),
    )
    .unwrap();
    write_json(&folder.join("stimulus.json"), &serde_json::json!({
        "output_id": output, "microphone_id": review.microphone_id, "position_id": review.position_id,
        "reference_output_id": review.reference_output_id, "signal": review.signal
    })).unwrap();
    write_json(&folder.join("capture-report.json"), &serde_json::json!({
        "schema_version": 3, "accepted_for_analysis": false, "rejection_reason": review.original_rejection_reason, "capture": {}
    })).unwrap();
    std::fs::write(
        folder.join("recording.wav"),
        b"cached fixture; no PCM analysis permitted",
    )
    .unwrap();
    let take = companion_service::controller::Take {
        output_id: output.into(),
        revision: controller.state.snapshot.as_ref().unwrap().revision.clone(),
        settings: controller.settings.clone(),
        outcome: Ok(companion_service::measurement::TakeOutcome {
            directory: folder.to_owned(),
            accepted: false,
            cancelled: false,
            rejection_reason: review.original_rejection_reason.clone(),
            diagnostics: None,
            timing: None,
            impulse: None,
            impulse_error: None,
        }),
    };
    (take, review)
}

fn speaker_card(output: &egui::FullOutput, name: Rect) -> (Rect, bool) {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            Shape::Rect(r)
                if r.rect.width() > 300.0
                    && r.stroke.width == 1.5
                    && r.rect.contains(name.center()) =>
            {
                Some((r.rect, r.stroke.color != egui::Color32::TRANSPARENT))
            }
            _ => None,
        })
        .expect("speaker card background")
}

#[test]
fn speaker_row_clicks_select_highlight_and_clear_results_without_activating_controls() {
    use companion_service::analysis_cache::CachedAnalysis;
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let dir = tempfile::tempdir().unwrap();
        let (mut controller, starts) = support::ready();
        controller
            .state
            .snapshot
            .as_mut()
            .unwrap()
            .outputs
            .truncate(3);
        for id in ["Front left", "Front right"] {
            let (take, review) = saved_result(&controller, &dir.path().join(id), id);
            controller.state.analyses.insert(
                review.directory.clone(),
                CachedAnalysis::from_review(&review),
            );
            controller.state.takes.push(take);
        }
        let ctx = egui::Context::default();
        let mut app = CompanionApp::preview(&ctx, controller);
        let name = find_speaker_text(&mut app, &ctx, size, "Front left");
        click(&mut app, &ctx, name.center(), size);
        finish_worker(&mut app, &ctx, size);
        find_result_text(&mut app, &ctx, size, "Front left · Measurement result");
        let name = find_speaker_text(&mut app, &ctx, size, "Front left");
        assert!(speaker_card(&frame(&mut app, &ctx, vec![], size), name).1);
        let name = find_speaker_text(&mut app, &ctx, size, "Front right");
        let (card, selected) = speaker_card(&frame(&mut app, &ctx, vec![], size), name);
        assert!(!selected);
        // The padding is selectable too, without requiring a label or dedicated button.
        click(
            &mut app,
            &ctx,
            Pos2::new(card.left() + 6.0, name.center().y),
            size,
        );
        finish_worker(&mut app, &ctx, size);
        find_result_text(&mut app, &ctx, size, "Front right · Measurement result");
        let name = find_speaker_text(&mut app, &ctx, size, "Front right");
        let text_output = frame(&mut app, &ctx, vec![], size);
        assert!(speaker_card(&text_output, name).1);
        assert!(!labels(&text_output).iter().any(|(s, _)| s == "View result"));
        let measure = find_speaker_text(&mut app, &ctx, size, "Measure");
        let text_output = frame(&mut app, &ctx, vec![], size);
        let name = labels(&text_output)
            .into_iter()
            .find(|(s, r)| s == "Front left" && r.left() > 310.0)
            .unwrap()
            .1;
        let (card, selected) = speaker_card(&text_output, name);
        assert!(!selected, "only one row should be highlighted");
        // The include checkbox and Measure button keep their own interactions.
        click(
            &mut app,
            &ctx,
            Pos2::new(card.left() + 25.0, measure.center().y),
            size,
        );
        assert!(app.controller.excluded_outputs.contains("Front left"));
        find_result_text(&mut app, &ctx, size, "Front right · Measurement result");
        assert_eq!(starts.load(Ordering::Relaxed), 0);
        let measure = find_speaker_text(&mut app, &ctx, size, "Measure");
        click(&mut app, &ctx, measure.center(), size);
        finish_worker(&mut app, &ctx, size);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        find_result_text(&mut app, &ctx, size, "Front right · Measurement result");
        let name = find_speaker_text(&mut app, &ctx, size, "Center");
        click(&mut app, &ctx, name.center(), size);
        find_result_text(&mut app, &ctx, size, "Center · Measurement result");
        find_result_text(&mut app, &ctx, size, "No measurement for Center yet");
        let name = find_speaker_text(&mut app, &ctx, size, "Center");
        assert!(speaker_card(&frame(&mut app, &ctx, vec![], size), name).1);
        app.controller.start("Center"); // The injected backend fails without opening audio.
        finish_worker(&mut app, &ctx, size);
        assert_eq!(starts.load(Ordering::Relaxed), 2);
        find_result_text(&mut app, &ctx, size, "Measurement failed");
        find_result_text(&mut app, &ctx, size, "Center · Measurement result");
    }
}

#[test]
fn quality_grades_draw_distinct_colored_symbols_with_readable_labels() {
    use companion_service::quality::{Grade, Indicator, Report};
    let size = Vec2::new(1160.0, 1000.0);
    for grade in [Grade::Good, Grade::Limited, Grade::Poor, Grade::Unavailable] {
        let (mut controller, starts) = support::ready();
        let mut review = support::arrival_review();
        let indicator = || Indicator {
            grade,
            details: vec!["Quality detail for inspection".into()],
        };
        review.quality = Some(Report {
            method: "synthetic-preview".into(),
            acquisition: indicator(),
            timing: indicator(),
            level: indicator(),
            level_db: None,
            level_band_snr_db: None,
            minimum_frequency_snr_db: None,
        });
        controller.state.review = Some(review);
        let ctx = egui::Context::default();
        let mut app = CompanionApp::preview(&ctx, controller);
        let label = find_result_text(
            &mut app,
            &ctx,
            size,
            &format!("Acquisition: {}", grade.label()),
        );
        let output = frame(&mut app, &ctx, vec![], size);
        let icon_region = Rect::from_min_max(
            Pos2::new(label.left() - 27.0, label.center().y - 10.0),
            Pos2::new(label.left() - 5.0, label.center().y + 10.0),
        );
        let symbols: Vec<_> = output
            .shapes
            .iter()
            .map(|s| &s.shape)
            .filter(|s| icon_region.contains_rect(s.visual_bounding_rect()))
            .collect();
        let expected_color = match grade {
            Grade::Good => egui::Color32::from_rgb(100, 220, 145),
            Grade::Limited => egui::Color32::from_rgb(243, 194, 123),
            Grade::Poor => egui::Color32::from_rgb(248, 136, 139),
            Grade::Unavailable => egui::Color32::from_rgb(151, 167, 184),
        };
        match grade {
            Grade::Good | Grade::Limited => assert!(symbols.iter().any(|s| matches!(s, Shape::Path(p) if p.points.len()==3 && p.closed == (grade==Grade::Limited) && p.stroke.color == egui::epaint::ColorMode::Solid(expected_color)))),
            Grade::Poor => assert_eq!(symbols.iter().filter(|s| matches!(s, Shape::LineSegment { stroke,.. } if stroke.color == expected_color)).count(), 2),
            Grade::Unavailable => assert_eq!(symbols.iter().filter(|s| matches!(s, Shape::LineSegment { stroke,.. } if stroke.color == expected_color)).count(), 1),
        }
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn manual_level_panel_saves_a_fresh_reference_shows_direction_and_stops() {
    use companion_service::{
        controller::Job,
        manual_level::{Phase, Reading, Request, Run},
    };
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    let context = controller.manual_context(-30.0).unwrap();
    let reference_level = calibration_core::manual_level::estimate(0.0101, 0.0001, 0.0).unwrap();
    controller.state.job = Some(Job::ManualLevels {
        output_id: "Front left".into(),
    });
    controller.state.manual_run = Some(Run {
        request: Request {
            context: context.clone(),
            output_id: "Front left".into(),
        },
        reading: Some(Reading {
            phase: Phase::Listening,
            level: Some(reference_level),
            remaining_seconds: 55,
            captured_at: Instant::now(),
        }),
        error: None,
        completed: false,
    });
    let mut app = CompanionApp::with_controller(&ctx, controller, false);
    let size = Vec2::new(1400.0, 1000.0);
    let output = frame(&mut app, &ctx, vec![], size);
    let header = labels(&output)
        .into_iter()
        .find(|(s, _)| s.contains("Manual speaker levels"))
        .unwrap()
        .1;
    click(&mut app, &ctx, header.center(), size);
    for _ in 0..10 {
        frame(&mut app, &ctx, vec![], size);
    }
    let output = frame(&mut app, &ctx, vec![], size);
    let save = labels(&output)
        .into_iter()
        .find(|(s, _)| s == "Use live level as reference")
        .unwrap()
        .1;
    click(&mut app, &ctx, save.center(), size);
    assert!(app.controller.manual_reference(-30.0).is_some());
    let run = app.controller.state.manual_run.as_mut().unwrap();
    run.request.output_id = "Front right".into();
    run.reading.as_mut().unwrap().level =
        Some(calibration_core::manual_level::estimate(0.0051, 0.0001, 0.0).unwrap());
    run.reading.as_mut().unwrap().captured_at = Instant::now();
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(s, _)| s.contains("-3.0 dB relative to Front left"))
    );
    assert!(
        text.iter()
            .any(|(s, _)| s == "Raise this speaker’s physical volume")
    );
    let stop = text.into_iter().find(|(s, _)| s == "Stop noise").unwrap().1;
    click(&mut app, &ctx, stop.center(), size);
    assert!(app.controller.state.stopping);
    assert!(!app.controller.can_set_manual_reference(-30.0));
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

#[test]
fn partial_alignment_names_exclusions_and_displays_only_eligible_targets() {
    use companion_service::alignment::Row;
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let ctx = egui::Context::default();
        let (mut controller, starts) = support::ready();
        let mut alignment = support::alignment(&controller, true);
        alignment.rows.push(Row {
            output_id: "Center".into(),
            take_folders: vec![],
            repeats: vec![],
            quality: vec![],
            qualified: None,
            reason: Some(
                "invalid calibration data: invalid calibration data: clipped recording".into(),
            ),
        });
        controller.state.alignment = Some(alignment);
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Correction proposal ready for 2 / 3 speakers · 1 excluded",
        );
        find_speaker_text(&mut app, &ctx, size, "Center: clipped recording");
        find_speaker_text(&mut app, &ctx, size, "Gain: before → target");
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Excluded: clipped recording · gain and delay unchanged",
        );
        let alignment = app.controller.state.alignment.as_mut().unwrap();
        alignment.profile = None;
        alignment.blockers.clear();
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "2 / 3 speakers qualify · update the proposal from saved passes",
        );
        find_speaker_text(&mut app, &ctx, size, "Update correction proposal");
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}
