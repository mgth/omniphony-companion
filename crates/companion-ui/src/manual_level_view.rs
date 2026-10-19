//! Physical level matching controls. Signal ownership and meter policy are services.
//!
//! Each speaker card carries a Noise button; the band below the cards shows the live
//! level of the speaker playing against the reference, with the test options under it.
use companion_service::{
    controller::{Controller, Job},
    manual_level::{Phase, RUN_SECONDS},
};
use eframe::egui::{self, RichText, Vec2};

/// Within this difference a speaker counts as matched.
const MATCHED_DB: f64 = 0.5;
/// The meter spans ± this many dB around the reference.
const METER_RANGE_DB: f64 = 12.0;

/// What a card's Noise button asked for.
pub enum NoiseAction {
    Start,
    Stop,
}

/// The speaker whose noise is playing or about to play (a switch waits for the
/// previous noise to stop).
pub fn noise_output<'a>(controller: &'a Controller, pending: Option<&'a str>) -> Option<&'a str> {
    match &controller.state.job {
        Some(Job::ManualLevels { output_id }) if !controller.state.stopping => Some(output_id),
        _ => pending,
    }
}

/// Whether this speaker's noise can start now, or why not.
pub fn readiness(controller: &Controller, id: &str, peak: f32) -> Result<(), String> {
    if controller.busy() && !matches!(controller.state.job, Some(Job::ManualLevels { .. })) {
        return Err("Wait for the current measurement to finish.".into());
    }
    let context = controller.manual_context(peak)?;
    context.output(id)?;
    if id != context.reference_output && controller.manual_reference(peak).is_none() {
        return Err(format!(
            "Play noise on the reference speaker {} first and save its level.",
            context.reference_output
        ));
    }
    Ok(())
}

/// The card's Noise control: Noise starts the test signal on this speaker, Stop ends it.
pub fn card_button(
    ui: &mut egui::Ui,
    controller: &Controller,
    id: &str,
    pending: Option<&str>,
    peak: f32,
) -> (egui::Response, Option<NoiseAction>) {
    let playing = noise_output(controller, pending) == Some(id);
    let ready = readiness(controller, id, peak);
    let (text, enabled) = if playing {
        (RichText::new("■ Stop").color(super::RED), true)
    } else {
        (
            RichText::new("▶ Noise").color(if ready.is_ok() {
                super::TEXT
            } else {
                super::MUTED
            }),
            ready.is_ok(),
        )
    };
    let mut button = ui
        .add_enabled(
            enabled,
            egui::Button::new(text.size(11.5))
                .fill(if playing {
                    super::RED.gamma_multiply(0.12)
                } else {
                    super::PANEL
                })
                .min_size(Vec2::new(62.0, 20.0)),
        )
        .on_hover_text(if playing {
            "Stop the noise on this speaker.".to_owned()
        } else {
            format!(
                "Play 500–2000 Hz pink noise on this speaker at {peak:.0} dBFS (stops after {RUN_SECONDS} s) and compare its level with the reference below. Turn its physical volume until the difference is near 0 dB."
            )
        });
    if let Err(reason) = &ready {
        button = button.on_disabled_hover_text(reason);
    }
    let action = button.clicked().then_some(if playing {
        NoiseAction::Stop
    } else {
        NoiseAction::Start
    });
    (button, action)
}

/// One short status for the card of the speaker playing noise.
pub fn card_status(controller: &Controller, peak: f32) -> (String, egui::Color32) {
    let reading = live(controller, peak);
    match reading.delta {
        Some(delta) => (format!("Noise · {delta:+.1} dB"), color(Some(delta))),
        None => (
            format!("Noise · {}", short_phase(reading.phase)),
            super::ACCENT,
        ),
    }
}

/// What the band reads from the current run.
#[derive(Default)]
struct Live {
    active: bool,
    current: bool,
    fresh: bool,
    phase: Option<Phase>,
    level: Option<calibration_core::manual_level::Level>,
    delta: Option<f64>,
}

fn live(controller: &Controller, peak: f32) -> Live {
    let Some(run) = &controller.state.manual_run else {
        return Live::default();
    };
    let active = matches!(controller.state.job, Some(Job::ManualLevels { .. }));
    let current = controller.manual_context(peak).as_ref() == Ok(&run.request.context);
    let fresh = active
        && !controller.state.stopping
        && current
        && run
            .reading
            .as_ref()
            .is_some_and(|r| r.captured_at.elapsed().as_millis() < 500);
    let level = if current {
        run.reading.as_ref().and_then(|r| r.level)
    } else {
        None
    };
    let delta = if fresh {
        level.and_then(|l| {
            controller
                .manual_reference(peak)
                .and_then(|r| l.difference(&r.level))
        })
    } else {
        None
    };
    Live {
        active,
        current,
        fresh,
        phase: run.reading.as_ref().map(|r| r.phase),
        level,
        delta,
    }
}

fn short_phase(phase: Option<Phase>) -> &'static str {
    match phase {
        Some(Phase::CheckingInput) => "opening microphone",
        Some(Phase::Background) => "measuring background",
        Some(Phase::Settling) => "settling",
        Some(Phase::Listening) => "live",
        None => "starting",
    }
}

fn color(delta: Option<f64>) -> egui::Color32 {
    delta.map_or(super::MUTED, |d| {
        if d.abs() <= MATCHED_DB {
            super::ACCENT
        } else {
            super::WARM
        }
    })
}

/// What the band asked for.
#[derive(Default)]
pub struct BandResponse {
    pub close: bool,
}

/// The level band below the speaker cards: the live meter of the speaker playing, then
/// the test options.
pub fn band(
    ui: &mut egui::Ui,
    controller: &mut Controller,
    pending: Option<&str>,
    peak: &mut f32,
) -> BandResponse {
    let mut response = BandResponse::default();
    let reference_id = controller.settings.reference_output.clone();
    let reference_name = reference_id.as_deref().unwrap_or("the reference speaker");
    let target = controller.manual_reference(*peak).is_some();
    let reading = live(controller, *peak);
    let playing = noise_output(controller, pending).map(str::to_owned);
    let run = controller.state.manual_run.as_ref();

    let status = if controller.state.stopping {
        "Stopping noise…".to_owned()
    } else if let (Some(id), true) = (&playing, reading.active) {
        let what = if !reading.fresh {
            "waiting for microphone / renderer…"
        } else {
            match reading.phase {
                Some(Phase::CheckingInput) => "opening microphone · no noise yet",
                Some(Phase::Background) => "measuring background · no noise yet",
                Some(Phase::Settling) => "noise playing · settling",
                Some(Phase::Listening) => "LIVE · turn its volume knob",
                None => "starting…",
            }
        };
        format!("{id} · {what}")
    } else if let Some(id) = &playing {
        format!("{id} · starting…")
    } else if let Some(run) = run {
        format!("{} · noise stopped · last reading", run.request.output_id)
    } else if target {
        format!("Press ▶ Noise on a speaker card to compare it with {reference_name}.")
    } else {
        format!("Start with ▶ Noise on {reference_name}, then save its level as the reference.")
    };
    ui.horizontal(|ui| {
        // The controls on the right first, so the status truncates before them.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !reading.active && playing.is_none(),
                    egui::Button::new("Hide"),
                )
                .on_hover_text("Hide the level band.")
                .on_disabled_hover_text("Stop the noise first.")
                .clicked()
            {
                response.close = true;
            }
            if reading.active
                && let Some(reading) = run.and_then(|r| r.reading.as_ref())
            {
                ui.label(
                    RichText::new(format!("Auto-stop in {} s", reading.remaining_seconds))
                        .small()
                        .color(super::MUTED),
                );
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(RichText::new("Speaker levels").strong());
                ui.add(
                    egui::Label::new(RichText::new(status).color(if reading.active {
                        super::TEXT
                    } else {
                        super::MUTED
                    }))
                    .truncate(),
                );
            });
        });
    });
    ui.add_space(4.0);
    meter(ui, reading.delta);
    ui.horizontal_wrapped(|ui| {
        let color = color(reading.delta);
        match reading.delta {
            Some(delta) => {
                ui.colored_label(
                    color,
                    RichText::new(format!("{delta:+.1} dB relative to {reference_name}")).strong(),
                );
                ui.label(if delta.abs() <= MATCHED_DB {
                    "Matched · within ±0.5 dB"
                } else if delta > 0.0 {
                    "Lower this speaker’s physical volume"
                } else {
                    "Raise this speaker’s physical volume"
                });
            }
            None if run
                .is_some_and(|r| r.request.output_id == r.request.context.reference_output)
                && !target =>
            {
                ui.colored_label(
                    super::MUTED,
                    "This is the reference speaker: save its level once the meter is live.",
                );
            }
            None => {
                ui.colored_label(super::MUTED, "Difference unavailable");
            }
        }
    });
    if let Some(level) = reading.level {
        ui.label(
            RichText::new(format!(
                "Microphone band RMS {:.1} dBFS · signal / background {:.1} dB",
                level.microphone_dbfs, level.snr_db
            ))
            .small()
            .color(super::MUTED),
        );
        if !level.usable() {
            ui.colored_label(super::WARM, "Signal is too close to background for reliable matching. Check the routing and microphone; the reference cannot be saved yet.");
        }
    } else if reading.fresh && reading.phase == Some(Phase::Listening) {
        ui.colored_label(
            super::WARM,
            "No clear signal above background. Check that this speaker is connected and playing.",
        );
    }
    if let Some(error) = run.and_then(|r| r.error.as_ref()) {
        ui.colored_label(super::RED, error);
    }
    if run.is_some() && !reading.current {
        ui.colored_label(
            super::WARM,
            "Setup changed; press ▶ Noise again to refresh the meter.",
        );
    }

    ui.add_space(6.0);
    ui.separator();
    options(ui, controller, peak, reference_name);
    response
}

/// The meter: a dot at the difference from the reference, a green zone around 0 dB.
fn meter(ui: &mut egui::Ui, delta: Option<f64>) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 5.0, super::BG);
    let center = rect.center().x;
    let range = rect.width() * 0.45;
    let x = |db: f64| center + range * (db / METER_RANGE_DB).clamp(-1.0, 1.0) as f32;
    painter.rect_filled(
        egui::Rect::from_x_y_ranges(x(-MATCHED_DB)..=x(MATCHED_DB), rect.y_range()),
        0.0,
        super::ACCENT.gamma_multiply(0.2),
    );
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let major = db == 0.0;
        painter.line_segment(
            [
                egui::pos2(x(db), rect.top() + if major { 0.0 } else { 26.0 }),
                egui::pos2(x(db), rect.bottom()),
            ],
            egui::Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.6)),
        );
        if db.abs() == 6.0 {
            painter.text(
                egui::pos2(x(db), rect.top() + 4.0),
                egui::Align2::CENTER_TOP,
                format!("{db:+.0}"),
                egui::FontId::proportional(10.0),
                super::MUTED,
            );
        }
    }
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.top() + 4.0),
        egui::Align2::LEFT_TOP,
        "quieter",
        egui::FontId::proportional(10.5),
        super::MUTED,
    );
    painter.text(
        egui::pos2(rect.right() - 8.0, rect.top() + 4.0),
        egui::Align2::RIGHT_TOP,
        "louder",
        egui::FontId::proportional(10.5),
        super::MUTED,
    );
    if let Some(delta) = delta {
        painter.circle_filled(
            egui::pos2(x(delta), rect.center().y),
            7.0,
            color(Some(delta)),
        );
    }
}

/// Test level and reference, under the meter.
fn options(ui: &mut egui::Ui, controller: &mut Controller, peak: &mut f32, reference_name: &str) {
    ui.scope(|ui| {
        // The rail would otherwise share the card's colour.
        ui.visuals_mut().widgets.inactive.bg_fill = super::BG;
        ui.visuals_mut().slider_trailing_fill = true;
        ui.add_enabled(
            !controller.busy(),
            egui::Slider::new(peak, -60.0..=-12.0).text("Test level · dBFS peak"),
        )
        .on_disabled_hover_text("Stop the noise to change the test level.");
    });
    ui.label(
        RichText::new(format!(
            "500–2000 Hz pink noise · stops after {RUN_SECONDS} s · start quietly"
        ))
        .small()
        .color(super::MUTED),
    );
    ui.horizontal_wrapped(|ui| {
        let can_save = controller.can_set_manual_reference(*peak);
        let saved = controller.manual_reference(*peak).is_some();
        let label = if saved {
            "Save again as reference"
        } else {
            "Use live level as reference"
        };
        let button = egui::Button::new(RichText::new(label).color(if can_save {
            super::BG
        } else {
            super::MUTED
        }))
        .fill(if can_save {
            super::ACCENT
        } else {
            super::PANEL
        });
        if ui
            .add_enabled(can_save, button)
            .on_disabled_hover_text(format!(
                "Play noise on {reference_name} and wait for a live, clear reading."
            ))
            .clicked()
        {
            controller.set_manual_reference(*peak);
        }
        if let Some(reference) = controller.manual_reference(*peak) {
            ui.colored_label(
                super::ACCENT,
                format!(
                    "Reference saved: {} · {:.1} dBFS",
                    reference.context.reference_output, reference.level.microphone_dbfs
                ),
            );
        } else {
            ui.colored_label(
                super::WARM,
                format!("No reference for this setup and test level yet ({reference_name})."),
            );
        }
    });
    if let Err(reason) = controller.manual_context(*peak) {
        ui.colored_label(super::WARM, reason);
    }
    ui.label(RichText::new("Relative matching, not calibrated SPL; existing digital trims are removed from the comparison without being changed. Keep the microphone and interface gain where they are. A reference holds only for this input, microphone position, renderer setup and test level. Measure the alignment again after turning any physical volume.").small().color(super::MUTED));
}
