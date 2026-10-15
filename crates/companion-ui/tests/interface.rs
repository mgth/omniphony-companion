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
        .find(|(t, _)| t == "Channel 1")
        .unwrap()
        .1
        .center();
    click(&mut app, &ctx, channel, size);
    let output = frame(&mut app, &ctx, vec![], size);
    let text = labels(&output);
    let channel = text
        .iter()
        .find(|(t, _)| t == "Channel 2")
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
