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
            // Long frames let egui's wheel smoothing settle within a few frames.
            predicted_dt: 0.1,
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
// Move first, like a real pointer: a jump and a press in one frame reads as a drag
// and scrolls the speaker area before the click lands.
fn click(app: &mut CompanionApp, ctx: &egui::Context, pos: Pos2, size: Vec2) {
    frame(app, ctx, vec![Event::PointerMoved(pos)], size);
    frame(
        app,
        ctx,
        vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        }],
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
    for _ in 0..8 {
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
    for _ in 0..8 {
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
        // Smaller than the shortest speaker viewport, so no label is skipped.
        scroll_speakers(app, ctx, size, -100.0);
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
    // The one-pass diagnostic capture is a Lab action; alignment is the main path.
    assert!(!text.iter().any(|(t, _)| t == "Measure selected speakers"));
    app.lab = true;
    frame(&mut app, &ctx, vec![], size);
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    for expected in [
        "Check microphone",
        "Measure selected speakers",
        "Align levels and delays",
        "No microphone signal received",
        "Last check",
        "Unchecked: Surround right",
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
    // The header switch turns Lab on; it is a display preference saved at once.
    let lab = text.iter().find(|(t, _)| t == "Lab").unwrap().1;
    click(&mut app, &ctx, lab.center(), size);
    assert!(app.lab);
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
    assert!(store.preferences().view.show_alignment, "Lab is restored");
    let app = CompanionApp::with_preferences(&ctx, support::ready().0, store);
    assert!(app.lab);
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
            deferred: false,
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
    app.lab = true;
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
    find_result_text(&mut app, &ctx, size, "25.0 dB");
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
        find_result_text(
            &mut app,
            &ctx,
            size,
            "Check the selected speaker's output route",
        );
        // The segment table is Lab detail; the routing explanation is not.
        find_result_text(&mut app, &ctx, size, "Turn on Lab for segment levels");
        app.lab = true;
        find_result_text(&mut app, &ctx, size, "Reference after");
        app.lab = false;
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
            blocker_codes: vec![],
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
        });
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        let output = frame(&mut app, &ctx, vec![], size);
        let text = labels(&output);
        let speakers = text.iter().find(|(t, _)| t == "Speakers").unwrap().1;
        assert!(text.iter().all(|(_, r)| r.is_finite()));
        assert!(
            !text
                .iter()
                .any(|(s, _)| s == "Alignment" || s == "Delay and level alignment")
        );
        // The speaker cards come first, the session below them, the results below both.
        let first = card_named(&output, "Front left").expect("card").0;
        assert!(first.top() > speakers.bottom());
        let alignment = find_speaker_text(&mut app, &ctx, size, "Alignment session");
        let output = frame(&mut app, &ctx, vec![], size);
        if let Some((last, _)) = card_named(&output, "Subwoofer") {
            assert!(last.bottom() < alignment.top(), "{last:?} {alignment:?}");
        }
        let lower = labels(&output)
            .into_iter()
            .find(|(s, _)| s == "Measurement results")
            .unwrap()
            .1;
        assert!(alignment.bottom() < lower.top());
        find_speaker_text(&mut app, &ctx, size, "Correction proposal blocked:");
        find_speaker_text(&mut app, &ctx, size, "Nothing to apply.");
        assert!(
            !labels(&frame(&mut app, &ctx, vec![], size))
                .iter()
                .any(|(s, _)| s == "Alignment session details"),
            "pass details are Lab-only"
        );
        app.lab = true;
        let details = find_speaker_text(&mut app, &ctx, size, "Alignment session details");
        click(&mut app, &ctx, details.center(), size);
        find_speaker_text(&mut app, &ctx, size, "Baseline pass 1 / 2: stopped");
        find_speaker_text(&mut app, &ctx, size, "Baseline pass 2 / 2: not started");
        for action in [
            "Apply to Omniphony",
            "Corrected",
            "Confirm by re-measuring",
            "Undo",
        ] {
            let button = find_speaker_text(&mut app, &ctx, size, action);
            click(&mut app, &ctx, button.center(), size);
            assert!(!app.controller.busy(), "{action} must stay disabled");
        }
        assert!(!app.controller.busy());
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

/// The card background whose name label equals `name`. The chart paints names too,
/// so the label must lie inside a card-sized rectangle with the card stroke.
fn card_named(output: &egui::FullOutput, name: &str) -> Option<(Rect, bool)> {
    let cards: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            Shape::Rect(r) if r.rect.width() > 150.0 && r.stroke.width == 1.5 => {
                Some((r.rect, r.stroke.color != egui::Color32::TRANSPARENT))
            }
            _ => None,
        })
        .collect();
    labels(output)
        .into_iter()
        .filter(|(s, _)| s == name)
        .find_map(|(_, r)| cards.iter().copied().find(|(c, _)| c.contains(r.center())))
}

#[test]
fn alignment_chart_and_cards_show_values_while_lab_keeps_raw_evidence_and_results_stay_below() {
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
            // Casual view: chart lanes, the card summary and what Apply changes.
            let mut casual = vec!["Timing", "Level", "measured · bar = spread between runs"];
            if proposal {
                casual.extend([
                    "delay +1.00 ms · gain -1.0 dB",
                    "delay unchanged · gain unchanged",
                    "+1.0 dB → gain -1.0",
                    "Apply to Omniphony",
                    "Not applied yet.",
                ]);
            } else {
                casual.extend(["Too noisy to trust.", "+160.0 ms", "No proposal yet"]);
            }
            for value in casual {
                let rect = find_speaker_text(&mut app, &ctx, size, value);
                assert!(
                    rect.bottom() < heading.top(),
                    "{value} appeared in the individual result panel"
                );
                assert!(
                    rect.left() > 310.0 && rect.right() < size.x,
                    "{value} escaped the speaker area: {rect:?}"
                );
            }
            let text = labels(&frame(&mut app, &ctx, vec![], size));
            for raw in [
                "0.020 ms / 0.10 dB",
                "Gain: before → target",
                "Excluded from correction",
            ] {
                assert!(
                    !text.iter().any(|(s, _)| s.contains(raw)),
                    "{raw} is Lab detail"
                );
            }
            assert!(
                !text
                    .iter()
                    .any(|(s, _)| s == "Alignment" || s == "Delay and level alignment")
            );
            // Lab keeps every raw number the previous rows showed.
            app.lab = true;
            for value in [
                "+0.125 ms",
                "-6.50 dB",
                "0.020 ms / 0.10 dB",
                "Quality",
                "Good",
            ] {
                let rect = find_speaker_text(&mut app, &ctx, size, value);
                assert!(rect.bottom() < heading.top());
            }
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
                    "integrated level-band signal/noise is 8.0 dB",
                    "Long delay:",
                ] {
                    find_speaker_text(&mut app, &ctx, size, value);
                }
            }
            app.lab = false;
            // The first card's button; its name label shares the line.
            find_speaker_text(&mut app, &ctx, size, "Measure");
            let before = card_named(&frame(&mut app, &ctx, vec![], size), "Front left")
                .expect("Front left card")
                .0;
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
                let output = frame(&mut app, &ctx, vec![], size);
                let after = card_named(&output, "Front left").expect("card").0;
                assert!(
                    (after.top() - before.top()).abs() < 0.1,
                    "card moved during capture: {before:?} -> {after:?}"
                );
                assert!(
                    labels(&output)
                        .iter()
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
fn starting_a_new_alignment_keeps_cards_in_place_and_chart_rows_select_speakers() {
    use companion_service::{controller::Job, measurement::MeasureProgress};
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let ctx = egui::Context::default();
        let (mut controller, starts) = support::ready();
        controller.state.alignment = Some(support::room_alignment(&controller));
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        // Human reasons, the far bracket and the knob hint, with raw text kept for Lab.
        for value in [
            "No sound picked up. Wired and unmuted?",
            "Too noisy to trust.",
            "beyond ±20 ms",
            "+176.9 ms",
            "Front left is 3.4 dB quieter than the others.",
            "Match knobs",
        ] {
            find_speaker_text(&mut app, &ctx, size, value);
        }
        // Clicking a chart row selects that speaker like its card does.
        let row = find_speaker_text(&mut app, &ctx, size, "Center");
        click(&mut app, &ctx, row.center(), size);
        find_result_text(&mut app, &ctx, size, "Center · Measurement result");
        // "Match knobs" opens the level band below the cards for the quietest speaker.
        let link = find_speaker_text(&mut app, &ctx, size, "Match knobs");
        click(&mut app, &ctx, link.center(), size);
        find_result_text(&mut app, &ctx, size, "Front left · Measurement result");
        find_speaker_text(&mut app, &ctx, size, "Speaker levels");
        find_speaker_text(&mut app, &ctx, size, "Measure");
        let output = frame(&mut app, &ctx, vec![], size);
        let cards: Vec<_> = ["Front left", "Front right"]
            .iter()
            .map(|name| card_named(&output, name).expect("card").0)
            .collect();
        // Starting a new alignment clears the old one; its place stays reserved.
        app.controller.state.alignment = None;
        app.controller.state.job = Some(Job::Alignment {
            output_ids: vec!["Front left".into(), "Front right".into()],
            current: 1,
            pass: 0,
        });
        for phase in [MeasureProgress::OpeningInput, MeasureProgress::Analyzing] {
            app.controller.state.progress = Some(phase);
            let output = frame(&mut app, &ctx, vec![], size);
            for (name, before) in ["Front left", "Front right"].iter().zip(&cards) {
                let after = card_named(&output, name).expect("card").0;
                assert!(
                    (after.top() - before.top()).abs() < 0.5,
                    "{name} moved when a new alignment started: {before:?} -> {after:?}"
                );
            }
        }
        assert_eq!(starts.load(Ordering::Relaxed), 0);
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
            deferred: false,
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
    // One weakest-grade badge by default; Lab separates the three indicators.
    for label in ["Quality: Limited", "Frequency response"] {
        find_result_text(&mut app, &ctx, size, label);
    }
    app.lab = true;
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
            reason_code: None,
            include_anyway: false,
        }],
        pass_reports: None,
        verification_pass_reports: None,
        blockers: vec!["Diagnostic review only".into()],
        blocker_codes: vec![],
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
            deferred: false,
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
                if r.rect.width() > 150.0
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
        app.lab = true;
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
            with_output_gain: false,
        },
        reading: Some(Reading {
            phase: Phase::Listening,
            output_gain_applied: Some(false),
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
    assert!(text.iter().any(|(s, _)| s == "Noise · -3.0 dB"));
    assert!(!text.iter().any(|(s, _)| s.contains("This Omniphony build")));
    // Check mode compares the speakers as corrected: Front right's -2.0 dB gain
    // against Front left's -1.5 dB adds -0.5 dB.
    let run = app.controller.state.manual_run.as_mut().unwrap();
    run.request.with_output_gain = true;
    run.reading.as_mut().unwrap().output_gain_applied = Some(true);
    run.reading.as_mut().unwrap().captured_at = Instant::now();
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(
        text.iter()
            .any(|(s, _)| s.contains("-3.5 dB relative to Front left, after correction"))
    );
    assert!(
        text.iter()
            .any(|(s, _)| s == "Still quieter after correction")
    );
    // An older renderer that cannot skip the gain is named, outside check mode.
    let run = app.controller.state.manual_run.as_mut().unwrap();
    run.request.with_output_gain = false;
    run.reading.as_mut().unwrap().captured_at = Instant::now();
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(
        text.iter()
            .any(|(s, _)| s.contains("This Omniphony build plays the noise through"))
    );
    assert!(
        text.iter()
            .any(|(s, _)| s.contains("-3.0 dB relative to Front left"))
    );
    let stop = text.into_iter().find(|(s, _)| s == "■ Stop").unwrap().1;
    click(&mut app, &ctx, stop.center(), size);
    assert!(app.controller.state.stopping);
    assert!(!app.controller.can_set_manual_reference(-30.0));
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

/// The card's Noise button inside the named speaker card.
fn noise_button(output: &egui::FullOutput, card: &str, text: &str) -> Option<Rect> {
    let (card, _) = card_named(output, card)?;
    labels(output)
        .into_iter()
        .find(|(s, r)| s == text && card.contains(r.center()))
        .map(|(_, r)| r)
}

fn frames_until(
    app: &mut CompanionApp,
    ctx: &egui::Context,
    size: Vec2,
    what: &str,
    done: impl Fn(&CompanionApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(app) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        frame(app, ctx, vec![], size);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn speaker_cards_play_noise_switch_speakers_and_the_band_below_reads_the_level() {
    use companion_service::controller::Job;
    let ctx = egui::Context::default();
    let (controller, starts) = support::ready();
    let mut app = CompanionApp::with_controller(&ctx, controller, false);
    let size = Vec2::new(1400.0, 1000.0);
    let playing = |app: &CompanionApp, id: &str| {
        matches!(&app.controller.state.job, Some(Job::ManualLevels { output_id }) if output_id == id)
            && !app.controller.state.stopping
    };
    let output = frame(&mut app, &ctx, vec![], size);
    // No band until a speaker plays; other speakers wait for a reference level.
    assert!(!labels(&output).iter().any(|(s, _)| s == "Speaker levels"));
    let right = noise_button(&output, "Front right", "▶ Noise").unwrap();
    click(&mut app, &ctx, right.center(), size);
    assert!(!app.controller.busy());

    let output = frame(&mut app, &ctx, vec![], size);
    let left = noise_button(&output, "Front left", "▶ Noise").unwrap();
    click(&mut app, &ctx, left.center(), size);
    assert!(playing(&app, "Front left"));
    frames_until(&mut app, &ctx, size, "a live reading", |app| {
        app.controller.can_set_manual_reference(-30.0)
    });
    let output = frame(&mut app, &ctx, vec![], size);
    assert!(labels(&output).iter().any(|(s, _)| s == "Speaker levels"));
    let save = labels(&output)
        .into_iter()
        .find(|(s, _)| s == "Use live level as reference")
        .unwrap()
        .1;
    click(&mut app, &ctx, save.center(), size);
    assert!(app.controller.manual_reference(-30.0).is_some());

    assert!(
        !app.controller
            .state
            .manual_run
            .as_ref()
            .unwrap()
            .request
            .with_output_gain
    );

    // Noise on another card stops this one, then plays there; the check switch
    // plays it through Omniphony's gain.
    let output = frame(&mut app, &ctx, vec![], size);
    let check = labels(&output)
        .into_iter()
        .find(|(s, _)| s == "Check with Omniphony’s level correction")
        .unwrap()
        .1;
    let center = noise_button(&output, "Center", "▶ Noise").unwrap();
    click(&mut app, &ctx, center.center(), size);
    frames_until(&mut app, &ctx, size, "the switch to Center", |app| {
        playing(app, "Center")
    });
    // Disabled while noise plays.
    click(&mut app, &ctx, check.center(), size);
    assert!(
        !app.controller
            .state
            .manual_run
            .as_ref()
            .unwrap()
            .request
            .with_output_gain
    );
    let output = frame(&mut app, &ctx, vec![], size);
    assert!(noise_button(&output, "Front left", "▶ Noise").is_some());
    let stop = noise_button(&output, "Center", "■ Stop").unwrap();
    click(&mut app, &ctx, stop.center(), size);
    finish_worker(&mut app, &ctx, size);

    // Once stopped, the check switch makes the next noise play through the gain.
    let check = find_speaker_text(
        &mut app,
        &ctx,
        size,
        "Check with Omniphony’s level correction",
    );
    click(&mut app, &ctx, check.center(), size);
    scroll_speakers(&mut app, &ctx, size, 10000.0);
    let output = frame(&mut app, &ctx, vec![], size);
    let left = noise_button(&output, "Front left", "▶ Noise").unwrap();
    click(&mut app, &ctx, left.center(), size);
    assert!(playing(&app, "Front left"));
    let run = app.controller.state.manual_run.as_ref().unwrap();
    assert!(run.request.with_output_gain);
    let output = frame(&mut app, &ctx, vec![], size);
    let stop = noise_button(&output, "Front left", "■ Stop").unwrap();
    click(&mut app, &ctx, stop.center(), size);
    finish_worker(&mut app, &ctx, size);

    // The last reading stays until the band is hidden.
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    assert!(
        text.iter()
            .any(|(s, _)| s.contains("Front left · noise stopped"))
    );
    let hide = text.into_iter().find(|(s, _)| s == "Hide").unwrap().1;
    click(&mut app, &ctx, hide.center(), size);
    let output = frame(&mut app, &ctx, vec![], size);
    assert!(!labels(&output).iter().any(|(s, _)| s == "Speaker levels"));
    assert!(app.controller.state.manual_run.is_none());
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
            reason_code: None,
            include_anyway: false,
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
        // The short reason replaces the raw one in the chart and card.
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Clipped. Lower the mic gain or test level.",
        );
        // With a known overdrive the card says how much to lower the gain.
        fn center(app: &mut CompanionApp) -> &mut Option<calibration_core::ExclusionReason> {
            let rows = &mut app.controller.state.alignment.as_mut().unwrap().rows;
            &mut rows.last_mut().unwrap().reason_code
        }
        *center(&mut app) = Some(calibration_core::ExclusionReason::Clipped {
            overdrive_db: Some(9.3),
        });
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Clipped by about 9 dB. Lower the mic gain or test level by 13 dB.",
        );
        *center(&mut app) = None;
        find_speaker_text(&mut app, &ctx, size, "delay +1.00 ms · gain -1.0 dB");
        let text = labels(&frame(&mut app, &ctx, vec![], size));
        assert!(!text.iter().any(|(s, _)| s.contains("clipped recording")));
        app.lab = true;
        find_speaker_text(&mut app, &ctx, size, "Center: clipped recording");
        find_speaker_text(&mut app, &ctx, size, "Gain: before → target");
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Excluded: clipped recording · gain and delay unchanged",
        );
        app.lab = false;
        let alignment = app.controller.state.alignment.as_mut().unwrap();
        alignment.profile = None;
        alignment.blockers.clear();
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "2 / 3 speakers qualify · update the proposal from saved passes",
        );
        find_speaker_text(&mut app, &ctx, size, "Turn on Lab to update the proposal");
        app.lab = true;
        find_speaker_text(&mut app, &ctx, size, "Update correction proposal");
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn a_late_speaker_is_set_aside_with_a_reason_and_can_be_included_from_the_chart() {
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let ctx = egui::Context::default();
        let dir = tempfile::tempdir().unwrap();
        let (mut controller, starts) = support::ready();
        controller.state.alignment = Some(support::late_device_alignment(&controller, dir.path()));
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        for value in [
            "No sound picked up. Wired and unmuted?",
            "Set aside: 60 ms later than the others.",
            "Set aside: Surround right arrives 60 ms later than the others.",
            "set aside · unchanged",
            "(1 set aside)",
        ] {
            find_speaker_text(&mut app, &ctx, size, value);
        }
        app.lab = true;
        find_speaker_text(&mut app, &ctx, size, "Set aside (1) · unchanged");
        find_speaker_text(&mut app, &ctx, size, "Repeatable · set aside");
        app.lab = false;
        let link = find_speaker_text(&mut app, &ctx, size, "Align to Surround right anyway");
        click(&mut app, &ctx, link.center(), size);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.controller.busy() {
            frame(&mut app, &ctx, vec![], size);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        frame(&mut app, &ctx, vec![], size);
        assert_eq!(app.controller.state.error, None);
        let alignment = app.controller.state.alignment.as_ref().unwrap();
        assert!(alignment.rows[4].include_anyway && alignment.rows[4].included());
        assert_eq!(
            alignment.corrected_output_ids(),
            ["Front left", "Front right", "Center", "Surround right"]
        );
        find_speaker_text(
            &mut app,
            &ctx,
            size,
            "Correction proposal ready for 4 / 5 speakers · 1 excluded",
        );
        find_speaker_text(&mut app, &ctx, size, "Set Surround right aside again");

        // The same choice sits on the speaker's card, where its reason is shown.
        let wait = |app: &mut CompanionApp| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.controller.busy() {
                frame(app, &ctx, vec![], size);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            frame(app, &ctx, vec![], size);
        };
        // Scrolls until the card's bottom row is in view; None when it has no such button.
        let card_button = |app: &mut CompanionApp, text: &str| {
            scroll_speakers(app, &ctx, size, 10000.0);
            for _ in 0..20 {
                let output = frame(app, &ctx, vec![], size);
                if let Some((card, _)) = card_named(&output, "Surround right")
                    && labels(&output)
                        .iter()
                        .any(|(s, r)| s == "▶ Noise" && card.contains(r.center()))
                {
                    return labels(&output)
                        .into_iter()
                        .find(|(s, r)| s == text && card.contains(r.center()))
                        .map(|(_, r)| r);
                }
                scroll_speakers(app, &ctx, size, -100.0);
            }
            panic!("Surround right card not reachable");
        };
        let aside = card_button(&mut app, "Set aside").expect("Set aside on the card");
        assert!(card_button(&mut app, "Align anyway").is_none());
        click(&mut app, &ctx, aside.center(), size);
        wait(&mut app);
        let alignment = app.controller.state.alignment.as_ref().unwrap();
        assert!(!alignment.rows[4].include_anyway && alignment.rows[4].set_aside());
        let anyway = card_button(&mut app, "Align anyway").expect("Align anyway on the card");
        click(&mut app, &ctx, anyway.center(), size);
        wait(&mut app);
        assert!(app.controller.state.alignment.as_ref().unwrap().rows[4].included());
        // Other cards offer nothing of the kind.
        scroll_speakers(&mut app, &ctx, size, 10000.0);
        let output = frame(&mut app, &ctx, vec![], size);
        let (front, _) = card_named(&output, "Front left").expect("card");
        assert!(
            !labels(&output)
                .iter()
                .any(|(s, r)| (s == "Align anyway" || s == "Set aside")
                    && front.contains(r.center()))
        );
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn the_setup_rail_shows_chosen_values_with_a_change_link() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller.settings.sample_rate = 48000;
    controller.state.defaults.device = true;
    controller.state.defaults.reference = true;
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    frame(&mut app, &ctx, vec![], size);
    let rail = |app: &mut CompanionApp| {
        labels(&frame(app, &ctx, vec![], size))
            .into_iter()
            .filter(|(_, r)| r.right() <= 310.0)
            .collect::<Vec<_>>()
    };
    let text = rail(&mut app);
    for value in [
        "Preview audio interface",
        "1 · Mic 1 · 48000 Hz\nmic-1 · main-seat",
        "Front left",
        "Arrivals are measured against this speaker.",
    ] {
        assert!(text.iter().any(|(t, _)| t == value), "{value}: {text:?}");
    }
    assert_eq!(
        text.iter()
            .filter(|(t, _)| t == "Chosen automatically")
            .count(),
        2
    );
    assert!(!text.iter().any(|(t, _)| t == "Input channel"));
    let changes: Vec<_> = text.iter().filter(|(t, _)| t == "change").collect();
    assert_eq!(changes.len(), 2);
    click(&mut app, &ctx, changes[0].1.center(), size);
    let text = rail(&mut app);
    assert!(text.iter().any(|(t, _)| t == "Input channel"));
    let done = text.iter().find(|(t, _)| t == "Done").unwrap().1;
    click(&mut app, &ctx, done.center(), size);
    assert!(!rail(&mut app).iter().any(|(t, _)| t == "Input channel"));
    // A choice the user made is shown without the automatic note.
    app.controller.state.defaults = Default::default();
    assert!(
        !rail(&mut app)
            .iter()
            .any(|(t, _)| t == "Chosen automatically")
    );
    // A missing reference opens its chooser instead of a summary.
    app.controller.settings.reference_output = Some("Gone".into());
    let text = rail(&mut app);
    assert!(
        text.iter()
            .any(|(t, _)| t.starts_with("Selected reference is missing"))
    );
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

#[test]
fn measure_actions_show_their_expected_duration_and_running_captures_the_time_left() {
    use companion_service::{controller::Job, measurement::MeasureProgress};
    let ctx = egui::Context::default();
    let (controller, starts) = support::ready();
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    frame(&mut app, &ctx, vec![], size);
    let expected = companion_service::duration::alignment(app.controller.settings.sweep, 5);
    let minutes = (expected.total.as_secs_f64().ceil() as u64 + 30) / 60;
    find_speaker_text(
        &mut app,
        &ctx,
        size,
        &format!("5 selected · about {minutes} min"),
    );
    app.lab = true;
    find_speaker_text(&mut app, &ctx, size, "s · One diagnostic pass.");
    app.lab = false;
    app.controller.state.job = Some(Job::Alignment {
        output_ids: vec!["Front left".into(), "Front right".into()],
        current: 1,
        pass: 1,
    });
    app.controller.state.progress = Some(MeasureProgress::Tail);
    let left = app.controller.remaining().unwrap();
    assert!(left < expected.total && !left.is_zero(), "{left:?}");
    find_speaker_text(&mut app, &ctx, size, "Pass 2 / 2 · Speaker 2 / 2 · about");
    find_speaker_text(&mut app, &ctx, size, "Acoustic tail · about");
    app.controller.state.job = None;
    app.controller.state.progress = None;
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

/// Text drawn exactly as `text`, right of the setup panel.
fn central(output: &egui::FullOutput, text: &str) -> Option<Rect> {
    labels(output)
        .into_iter()
        .find(|(s, r)| s == text && r.left() > 310.0)
        .map(|(_, r)| r)
}

/// The pulse around a chip: a chip-sized outline twice as thick as the selection's.
fn pulse_around(output: &egui::FullOutput, chip: Rect) -> bool {
    output.shapes.iter().any(|shape| match &shape.shape {
        Shape::Rect(r) => {
            r.stroke.width == 2.0 && r.rect.width() < 120.0 && r.rect.contains(chip.center())
        }
        _ => false,
    })
}

#[test]
fn room_map_sits_beside_the_cards_labels_the_reference_and_selects_speakers() {
    let ctx = egui::Context::default();
    let (controller, _) = support::ready();
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(1160.0, 800.0);
    for _ in 0..3 {
        frame(&mut app, &ctx, vec![], size);
    }
    let output = frame(&mut app, &ctx, vec![], size);
    let room = central(&output, "Room").expect("room map title");
    let (front_right, _) = card_named(&output, "Front right").expect("card");
    assert!(room.left() > front_right.right(), "map beside the cards");
    assert!((room.top() - front_right.top()).abs() < 40.0, "{room:?}");
    for chip in ["FL", "FR", "Cen", "SL", "SR", "Sub", "REF", "FRONT"] {
        assert!(central(&output, chip).is_some(), "{chip} missing");
    }
    // Front is up and the left is on the left; the reference label sits under its chip.
    let fl = central(&output, "FL").unwrap();
    let fr = central(&output, "FR").unwrap();
    let sl = central(&output, "SL").unwrap();
    assert!(fl.center().x < fr.center().x && fl.bottom() < sl.top());
    assert!(
        labels(&output).iter().any(|(s, r)| s == "REF"
            && r.top() >= fl.bottom() - 1.0
            && (r.center().x - fl.center().x).abs() < 20.0),
        "REF under the reference chip"
    );
    // Clicking a chip selects that speaker like its card does.
    let center = central(&output, "Cen").unwrap();
    click(&mut app, &ctx, center.center(), size);
    find_result_text(&mut app, &ctx, size, "Center · Measurement result");
    let output = frame(&mut app, &ctx, vec![], size);
    assert!(
        card_named(&output, "Center").expect("card").1,
        "card selected too"
    );
}

#[test]
fn capture_pulses_the_playing_chip_without_moving_the_map_chart_or_cards() {
    use companion_service::{controller::Job, measurement::MeasureProgress};
    for size in [Vec2::new(1160.0, 800.0), Vec2::new(1450.0, 1000.0)] {
        let ctx = egui::Context::default();
        let (mut controller, _) = support::ready();
        controller.state.alignment = Some(support::room_alignment(&controller));
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        // The cards and the map beside them come first, the chart below.
        find_speaker_text(&mut app, &ctx, size, "top view · front is up");
        let before = frame(&mut app, &ctx, vec![], size);
        let watched = ["Room", "FL", "FR", "Cen", "SL", "SR", "Sub"];
        let places: Vec<_> = watched
            .iter()
            .map(|t| central(&before, t).expect(t))
            .collect();
        let visible = [
            "Front right",
            "Center",
            "Surround left",
            "Surround right",
            "Subwoofer",
        ]
        .into_iter()
        .find_map(|name| card_named(&before, name).map(|(rect, _)| (name, rect)));
        // The map sits beside the cards at both widths.
        assert!(visible.is_some(), "{size:?}");
        let fl = central(&before, "FL").unwrap();
        assert!(!pulse_around(&before, fl));
        // A new alignment clears the chart and keeps its place while it captures.
        app.controller.state.alignment = None;
        app.controller.state.job = Some(Job::Alignment {
            output_ids: vec!["Front left".into(), "Front right".into()],
            current: 0,
            pass: 0,
        });
        for phase in [MeasureProgress::OpeningInput, MeasureProgress::Analyzing] {
            app.controller.state.progress = Some(phase);
            let during = frame(&mut app, &ctx, vec![], size);
            for (text, place) in watched.iter().zip(&places) {
                let now = central(&during, text).expect(text);
                assert!(
                    (now.min - place.min).length() < 0.5,
                    "{text} moved at {size:?}: {place:?} -> {now:?}"
                );
            }
            if let Some((name, card)) = visible {
                let now = card_named(&during, name).expect("card").0;
                assert!((now.min - card.min).length() < 0.5, "{name} moved");
            }
            assert!(pulse_around(&during, fl), "the playing speaker pulses");
            assert!(!pulse_around(&during, central(&during, "FR").unwrap()));
        }
    }
}

#[test]
fn the_map_never_pushes_the_cards_down_and_needs_reported_positions() {
    for size in [Vec2::new(850.0, 640.0), Vec2::new(1160.0, 800.0)] {
        let first_card = |placed: bool| {
            let ctx = egui::Context::default();
            let (mut controller, _) = support::ready();
            if !placed {
                for o in &mut controller.state.snapshot.as_mut().unwrap().outputs {
                    o.azimuth_deg = None;
                    o.elevation_deg = None;
                    o.distance_m = None;
                }
            }
            let mut app = CompanionApp::preview(&ctx, controller);
            for _ in 0..3 {
                frame(&mut app, &ctx, vec![], size);
            }
            let output = frame(&mut app, &ctx, vec![], size);
            (
                card_named(&output, "Front left").expect("card").0,
                central(&output, "Room"),
                card_named(&output, "Center").expect("card").0,
            )
        };
        let (with_map, room, _) = first_card(true);
        let (without_map, none, center) = first_card(false);
        assert_eq!(with_map.top(), without_map.top(), "{size:?}");
        // An older renderer reports no positions: no map, and the cards keep the width.
        assert!(none.is_none());
        if size.x > 1000.0 {
            assert!(room.is_some());
            assert_eq!(center.top(), without_map.top(), "three cards per row");
        } else {
            // Too narrow for two card columns beside it: the map waits below the cards.
            assert!(room.is_none_or(|r| r.top() > with_map.bottom()));
        }
    }
    let ctx = egui::Context::default();
    let (controller, _) = support::ready();
    let mut app = CompanionApp::preview(&ctx, controller);
    let size = Vec2::new(850.0, 640.0);
    find_speaker_text(&mut app, &ctx, size, "top view · front is up");
    // Beside the cards, the map card ends within the card grid.
    let size = Vec2::new(1160.0, 1000.0);
    scroll_speakers(&mut app, &ctx, size, 10000.0);
    for _ in 0..3 {
        frame(&mut app, &ctx, vec![], size);
    }
    let output = frame(&mut app, &ctx, vec![], size);
    let room = central(&output, "Room").expect("room map title");
    let map_card = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            Shape::Rect(r)
                if r.rect.contains(room.center())
                    && r.rect.width() < 300.0
                    && r.rect.height() > 150.0 =>
            {
                Some(r.rect)
            }
            _ => None,
        })
        .expect("map card");
    let last = card_named(&output, "Subwoofer").expect("card").0;
    assert!(
        map_card.bottom() <= last.bottom() + 0.5,
        "{map_card:?} {last:?}"
    );
}

/// Opacity of the text drawn exactly as `text` right of the setup panel.
fn text_alpha(output: &egui::FullOutput, text: &str) -> Option<u8> {
    fn find(shape: &Shape, text: &str) -> Option<u8> {
        match shape {
            Shape::Text(t) if t.galley.text() == text && t.pos.x > 310.0 => {
                t.galley.job.sections.first().map(|s| s.format.color.a())
            }
            Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, text)),
            _ => None,
        }
    }
    output.shapes.iter().find_map(|s| find(&s.shape, text))
}

#[test]
fn a_speaker_set_aside_by_the_policy_is_dimmed_on_the_map_until_included() {
    let ctx = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let (mut controller, _) = support::ready();
    controller.state.alignment = Some(support::late_device_alignment(&controller, dir.path()));
    let mut app = CompanionApp::preview(&ctx, controller);
    // Tall enough to show the chart, the cards and the map without scrolling.
    let size = Vec2::new(1160.0, 1800.0);
    for _ in 0..3 {
        frame(&mut app, &ctx, vec![], size);
    }
    let output = frame(&mut app, &ctx, vec![], size);
    let included = text_alpha(&output, "FL").expect("FL chip");
    assert!(text_alpha(&output, "SR").expect("SR chip") < included);
    let link = find_speaker_text(&mut app, &ctx, size, "Align to Surround right anyway");
    click(&mut app, &ctx, link.center(), size);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.controller.busy() {
        frame(&mut app, &ctx, vec![], size);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    frame(&mut app, &ctx, vec![], size);
    let output = frame(&mut app, &ctx, vec![], size);
    assert_eq!(text_alpha(&output, "SR"), Some(included));
}

#[test]
fn a_card_measures_its_speaker_again_into_the_alignment_or_takes_one_diagnostic_take() {
    use companion_service::controller::Job;
    for size in [
        Vec2::new(850.0, 640.0),
        Vec2::new(1160.0, 800.0),
        Vec2::new(1400.0, 1000.0),
    ] {
        let ctx = egui::Context::default();
        let (mut controller, starts) = support::ready();
        let mut alignment = support::alignment(&controller, true);
        alignment.profile.as_mut().unwrap().origin = calibration_core::ObservationOrigin::Measured;
        controller.state.alignment = Some(alignment);
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        // The first card's Measure opens the choice; the menu floats over the cards.
        let measure = find_speaker_text(&mut app, &ctx, size, "Measure");
        let output = frame(&mut app, &ctx, vec![], size);
        let cards: Vec<_> = ["Front left", "Front right"]
            .iter()
            .map(|name| card_named(&output, name).expect("card").0)
            .collect();
        click(&mut app, &ctx, measure.center(), size);
        let output = frame(&mut app, &ctx, vec![], size);
        let text = labels(&output);
        let again = text
            .iter()
            .find(|(s, _)| s == "Measure again into the session")
            .expect("session choice")
            .1;
        assert!(text.iter().any(|(s, _)| s == "One diagnostic take"));
        for (name, before) in ["Front left", "Front right"].iter().zip(&cards) {
            let after = card_named(&output, name).expect("card").0;
            assert!(
                (after.top() - before.top()).abs() < 0.5,
                "{name} moved when the menu opened: {before:?} -> {after:?}"
            );
        }
        click(&mut app, &ctx, again.center(), size);
        assert!(
            matches!(&app.controller.state.job, Some(Job::Remeasure { output_id, .. }) if output_id == "Front left"),
            "{:?}",
            app.controller.state.job
        );
        finish_worker(&mut app, &ctx, size);
        // The bench has no batch capture: the request reached it, no single take ran.
        assert!(
            app.controller
                .state
                .error
                .as_deref()
                .is_some_and(|e| e.contains("unavailable")),
            "{:?}",
            app.controller.state.error
        );
        assert_eq!(starts.load(Ordering::Relaxed), 0);
        assert!(app.controller.state.alignment.is_some());
        assert!(
            !labels(&frame(&mut app, &ctx, vec![], size))
                .iter()
                .any(|(s, _)| s == "One diagnostic take"),
            "the menu stayed open"
        );
        // The diagnostic take is still one choice away.
        let measure = find_speaker_text(&mut app, &ctx, size, "Measure");
        click(&mut app, &ctx, measure.center(), size);
        let take = labels(&frame(&mut app, &ctx, vec![], size))
            .into_iter()
            .find(|(s, _)| s == "One diagnostic take")
            .expect("diagnostic choice")
            .1;
        click(&mut app, &ctx, take.center(), size);
        finish_worker(&mut app, &ctx, size);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        // An applied session is not measured into: the choice stays, greyed, and says why.
        let live = app.controller.state.snapshot.clone().unwrap();
        app.controller
            .state
            .alignment
            .as_mut()
            .unwrap()
            .applied_snapshot = Some(live);
        assert!(
            app.controller
                .can_remeasure("Front left")
                .unwrap_err()
                .starts_with("Undo the applied alignment first")
        );
        let measure = find_speaker_text(&mut app, &ctx, size, "Measure");
        click(&mut app, &ctx, measure.center(), size);
        let again = labels(&frame(&mut app, &ctx, vec![], size))
            .into_iter()
            .find(|(s, _)| s == "Measure again into the session")
            .expect("greyed session choice")
            .1;
        click(&mut app, &ctx, again.center(), size);
        assert!(
            app.controller.state.job.is_none(),
            "{:?}",
            app.controller.state.job
        );
        // A click on the greyed choice only closes the menu.
        let measure = find_speaker_text(&mut app, &ctx, size, "Measure");
        click(&mut app, &ctx, measure.center(), size);
        let take = labels(&frame(&mut app, &ctx, vec![], size))
            .into_iter()
            .find(|(s, _)| s == "One diagnostic take")
            .expect("diagnostic choice")
            .1;
        click(&mut app, &ctx, take.center(), size);
        finish_worker(&mut app, &ctx, size);
        assert_eq!(starts.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn an_alignment_applied_to_another_renderer_does_not_block_a_new_one() {
    let size = Vec2::new(1160.0, 800.0);
    let ctx = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let (mut controller, starts) = support::ready();
    controller.settings.output_root = dir.path().display().to_string();
    let mut alignment = support::alignment(&controller, true);
    // Saved as applied, but this renderer has the original settings: nothing to undo.
    alignment.applied_snapshot = controller.state.snapshot.clone();
    controller.state.alignment = Some(alignment);
    assert_eq!(
        controller.alignment_live_state(),
        Some(companion_service::alignment::LiveState::Original)
    );
    assert!(controller.alignment_readiness().is_ok());
    // Another installation, or settings changed since: nothing to undo here either.
    controller.state.snapshot.as_mut().unwrap().outputs[0].gain_db -= 3.0;
    assert_eq!(
        controller.alignment_live_state(),
        Some(companion_service::alignment::LiveState::Other)
    );
    assert!(controller.alignment_readiness().is_ok());
    let mut app = CompanionApp::preview(&ctx, controller);
    let align = find_speaker_text(&mut app, &ctx, size, "Align levels and delays");
    click(&mut app, &ctx, align.center(), size);
    assert!(
        !app.controller
            .state
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Restore the active alignment")),
        "{:?}",
        app.controller.state.error
    );
    finish_worker(&mut app, &ctx, size);
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

#[test]
fn an_older_take_restored_without_its_report_loads_when_opened() {
    let ctx = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let (mut controller, starts) = support::ready();
    let (mut older, _) = saved_result(&controller, &dir.path().join("older-take"), "Front right");
    let (newer, _) = saved_result(&controller, &dir.path().join("newer-take"), "Front right");
    // As a restore leaves it: only the folder of the older take is known.
    if let Ok(outcome) = &mut older.outcome {
        outcome.deferred = true;
        outcome.rejection_reason = None;
    }
    controller.state.takes = vec![older, newer];
    let mut app = CompanionApp::with_controller(&ctx, controller, false);
    let size = Vec2::new(1400.0, 1000.0);
    frame(&mut app, &ctx, vec![], size);
    // Opened from the results' history menu.
    let menu = find_result_text(&mut app, &ctx, size, "Previous measurements");
    click(&mut app, &ctx, menu.center(), size);
    let entry = labels(&frame(&mut app, &ctx, vec![], size))
        .into_iter()
        .find(|(s, _)| s.starts_with("Front right") && s.ends_with("older-take"))
        .expect("history entry")
        .1;
    click(&mut app, &ctx, entry.center(), size);
    let deadline = Instant::now() + Duration::from_secs(5);
    let loaded = |app: &CompanionApp| {
        app.controller.state.takes[0]
            .outcome
            .as_ref()
            .is_ok_and(|o| !o.deferred)
    };
    while !loaded(&app) {
        frame(&mut app, &ctx, vec![], size);
        assert!(Instant::now() < deadline, "the older take did not load");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The result panel then shows the older take itself.
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(!text.iter().any(|(s, _)| s == "Loading saved result…"));
    assert!(
        text.iter()
            .any(|(s, _)| s == "Recorded · timing unavailable")
    );
    let take = &app.controller.state.takes[0];
    let outcome = take.outcome.as_ref().unwrap();
    assert!(!outcome.deferred);
    assert_eq!(
        outcome.rejection_reason.as_deref(),
        Some("legacy broadband signal/noise rejection")
    );
    assert!(app.controller.state.loading_takes.is_empty());
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

#[test]
fn headphone_output_is_named_and_switched_to_the_speakers_from_the_companion() {
    let ctx = egui::Context::default();
    let (mut controller, starts) = support::ready();
    controller
        .state
        .snapshot
        .as_mut()
        .unwrap()
        .unavailable_reason = Some(omniphony_adapter::protocol::SPEAKER_OUTPUT_REQUIRED.into());
    let mut app = CompanionApp::with_controller(&ctx, controller, false);
    let size = Vec2::new(1160.0, 800.0);
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(
        text.iter()
            .any(|(s, _)| s == "Omniphony is playing to headphones")
    );
    assert!(!text.iter().any(|(s, _)| s == "Measurement unavailable"));
    let switch = text
        .into_iter()
        .find(|(s, _)| s == "Switch to speakers")
        .unwrap()
        .1;
    click(&mut app, &ctx, switch.center(), size);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.controller.busy() {
        frame(&mut app, &ctx, vec![], size);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let text = labels(&frame(&mut app, &ctx, vec![], size));
    assert!(
        !text
            .iter()
            .any(|(s, _)| s == "Omniphony is playing to headphones")
    );
    assert!(!app.controller.needs_speaker_output());
    assert_eq!(app.controller.state.error, None);
    assert_eq!(starts.load(Ordering::Relaxed), 0);
}

#[test]
fn from_zero_or_refine_is_picked_before_measuring_and_reset_after() {
    use companion_service::alignment::Basis;
    let size = Vec2::new(1160.0, 800.0);
    let ctx = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let (mut controller, _) = support::ready();
    controller.settings.output_root = dir.path().display().to_string();
    // Before any alignment: from zero.
    assert_eq!(controller.alignment_basis(), Basis::FromZero);
    let mut app = CompanionApp::preview(&ctx, controller);
    let refine = find_speaker_text(&mut app, &ctx, size, "Refine");
    click(&mut app, &ctx, refine.center(), size);
    assert_eq!(app.controller.alignment_basis(), Basis::Refine);
    let from_zero = find_speaker_text(&mut app, &ctx, size, "From zero");
    click(&mut app, &ctx, from_zero.center(), size);
    assert_eq!(app.controller.alignment_basis(), Basis::FromZero);
    click(&mut app, &ctx, refine.center(), size);
    // The choice is for one alignment: starting it hands the default back.
    let align = find_speaker_text(&mut app, &ctx, size, "Align levels and delays");
    click(&mut app, &ctx, align.center(), size);
    assert!(app.controller.busy(), "{:?}", app.controller.state.error);
    assert_eq!(app.controller.chosen_alignment_basis, None);
    finish_worker(&mut app, &ctx, size);
}

#[test]
fn an_uncertain_application_blocks_a_new_baseline() {
    let (mut controller, _) = support::ready();
    let mut alignment = support::alignment(&controller, true);
    alignment.application_uncertain = true;
    controller.state.alignment = Some(alignment);
    assert!(
        controller
            .alignment_readiness()
            .is_err_and(|e| e.starts_with("The last application is uncertain"))
    );
}
