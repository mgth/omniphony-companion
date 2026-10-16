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
    for _ in 0..30 {
        let output = frame(app, ctx, vec![], size);
        if let Some((_, rect)) = labels(&output).into_iter().find(|(t, r)| {
            (if needle == "Arrival diagnostics" {
                t == needle
            } else {
                t.contains(needle)
            }) && r.bottom() < size.y - 30.0
        }) {
            return rect;
        }
        scroll_results(app, ctx, size, -100.0);
    }
    panic!("Result text not reachable by scrolling: {needle}");
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
            let output = frame(&mut app, &ctx, vec![], size);
            let text = labels(&output);
            assert!(text.iter().any(|(t, _)| t == title), "{mode:?}");
            assert!(text.iter().all(|(_, rect)| rect.is_finite()));
            assert_eq!(starts.load(Ordering::Relaxed), 0);
        }
    }
    let size = Vec2::new(1160.0, 800.0);
    let output = frame(&mut app, &ctx, vec![], size);
    let position = labels(&output)
        .iter()
        .find(|(t, _)| t == "Frequency response")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, position, size);
    assert_eq!(app.result_plot, ResultPlot::FrequencyResponse);
    let review = app.controller.state.review.as_mut().unwrap();
    let report = std::sync::Arc::make_mut(review.impulse.as_mut().unwrap());
    for point in &mut report.segments[1].spectral.as_mut().unwrap().points {
        point.magnitude_db = None;
        point.peak_time_ms = None;
        point.group_delay_ms = None;
    }
    let output = frame(&mut app, &ctx, vec![], size);
    assert!(
        labels(&output)
            .iter()
            .any(|(t, _)| t == "No reliable points in this view.")
    );
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
        let results = text.iter().find(|(t, _)| t == "Results").unwrap().1;
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
fn alignment_is_inline_and_unqualified_results_never_apply_or_play() {
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
        });
        let mut app = CompanionApp::preview(&ctx, controller);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], size);
        }
        let text = labels(&frame(&mut app, &ctx, vec![], size));
        let speakers = text.iter().find(|(t, _)| t == "Speakers").unwrap().1;
        let alignment = text
            .iter()
            .find(|(t, _)| t == "Delay and level alignment")
            .unwrap()
            .1;
        assert!(alignment.top() > speakers.bottom());
        assert!(text.iter().all(|(_, r)| r.is_finite()));
        find_result_text(&mut app, &ctx, size, "Baseline pass 1 / 2: stopped");
        find_result_text(&mut app, &ctx, size, "Baseline pass 2 / 2: not started");
        find_result_text(&mut app, &ctx, size, "No correction proposal");
        let apply = find_result_text(&mut app, &ctx, size, "Apply gains and delays");
        click(&mut app, &ctx, apply.center(), size);
        assert!(!app.controller.busy());
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
    saved_review.directory = folder.clone();
    saved_review.report_file = folder.join("analysis-test.json");
    write_json(&saved_review.report_file, &saved_review).unwrap();
    controller.state.review = Some(saved_review);
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
    // Seed the index as a completed session, then make a real view choice in egui.
    controller.state.takes.push(take);
    let mut store = PreferenceStore::open(path.clone()).unwrap();
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
    assert!(text.iter().any(|(s, _)| s.contains("Take · Front left")));
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
