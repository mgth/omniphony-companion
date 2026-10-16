//! Physical level matching controls. Signal ownership and meter policy are services.
use companion_service::{
    controller::{Controller, Job},
    manual_level::Phase,
};
use eframe::egui::{self, RichText};

pub fn draw(
    ui: &mut egui::Ui,
    controller: &mut Controller,
    selected: Option<&str>,
    peak: &mut f32,
) {
    egui::CollapsingHeader::new("Manual speaker levels · physical volume controls")
        .id_salt("manual_speaker_levels")
        .show(ui, |ui| contents(ui, controller, selected, peak));
}

fn contents(
    ui: &mut egui::Ui,
    controller: &mut Controller,
    selected: Option<&str>,
    peak: &mut f32,
) {
    ui.label("Match active speakers using their physical volume controls. Keep the microphone position and interface input gain unchanged.");
    ui.label(RichText::new("1. Play the reference and save its level.  2. Select a speaker row below and play it.  3. Adjust its volume until the difference is near 0 dB.").small().color(super::MUTED));
    let active = matches!(controller.state.job, Some(Job::ManualLevels { .. }));
    let context = controller.manual_context(*peak);
    let reference_id = controller.settings.reference_output.clone();
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled(
            !controller.busy(),
            egui::Slider::new(peak, -60.0..=-12.0).text("Test peak · dBFS"),
        );
        ui.label(
            RichText::new("500–2000 Hz pink noise · stops after 60 s")
                .small()
                .color(super::MUTED),
        );
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !controller.busy() && context.is_ok(),
                egui::Button::new(format!(
                    "Play reference {}",
                    reference_id.as_deref().unwrap_or("—")
                )),
            )
            .clicked()
            && let Some(id) = &reference_id
        {
            controller.start_manual_levels(id, *peak);
        }
        if ui
            .add_enabled(
                controller.can_set_manual_reference(*peak),
                egui::Button::new("Use live level as reference"),
            )
            .clicked()
        {
            controller.set_manual_reference(*peak);
        }
        let valid = context
            .as_ref()
            .ok()
            .and_then(|c| selected.map(|id| c.output(id).is_ok()))
            .unwrap_or(false);
        if ui
            .add_enabled(
                !controller.busy() && valid && controller.manual_reference(*peak).is_some(),
                egui::Button::new(format!("Play selected {}", selected.unwrap_or("—"))),
            )
            .clicked()
            && let Some(id) = selected
        {
            controller.start_manual_levels(id, *peak);
        }
        if ui
            .add_enabled(
                active && !controller.state.stopping,
                egui::Button::new(RichText::new("Stop noise").color(super::RED)),
            )
            .clicked()
        {
            controller.cancel();
        }
    });
    if let Err(reason) = &context {
        ui.colored_label(super::WARM, reason);
    }
    if let Ok(context) = &context
        && let Some(id) = selected
        && let Err(reason) = context.output(id)
    {
        ui.colored_label(super::WARM, reason);
    }
    let target = controller.manual_reference(*peak);
    if let Some(reference) = target {
        ui.label(
            RichText::new(format!(
                "Reference saved: {} · fixed microphone and test level",
                reference.context.reference_output
            ))
            .color(super::ACCENT),
        );
    } else {
        ui.label(RichText::new("No reference saved for this setup. Play the reference speaker first; wait for the live meter, then save its level.").color(super::WARM));
    }
    if let Some(run) = &controller.state.manual_run {
        let current = context.as_ref() == Ok(&run.request.context);
        let fresh = active
            && !controller.state.stopping
            && current
            && run
                .reading
                .as_ref()
                .is_some_and(|r| r.captured_at.elapsed().as_millis() < 500);
        let phase = run.reading.as_ref().map(|r| r.phase);
        let label = if controller.state.stopping {
            "Stopping noise…"
        } else if !active {
            "Noise stopped · last reading"
        } else if !fresh {
            "Waiting for microphone / renderer…"
        } else {
            match phase {
                Some(Phase::CheckingInput) => "Opening microphone · no noise",
                Some(Phase::Background) => "Measuring background · no noise",
                Some(Phase::Settling) => "Noise playing · settling",
                Some(Phase::Listening) => "LIVE · adjust the physical volume control",
                None => "Preparing…",
            }
        };
        ui.label(RichText::new(format!("{} · {label}", run.request.output_id)).strong());
        let level = if current {
            run.reading.as_ref().and_then(|r| r.level)
        } else {
            None
        };
        let delta = if fresh {
            level.and_then(|l| target.and_then(|r| l.difference(&r.level)))
        } else {
            None
        };
        let color = delta.map_or(super::MUTED, |d| {
            if d.abs() <= 0.5 {
                super::ACCENT
            } else {
                super::WARM
            }
        });
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 5.0, super::CARD);
        let center = rect.center().x;
        let range = rect.width() * 0.45;
        let green_width = range * 0.5 / 12.0;
        painter.rect_filled(
            egui::Rect::from_x_y_ranges(
                center - green_width..=center + green_width,
                rect.top()..=rect.bottom(),
            ),
            0.0,
            super::ACCENT.gamma_multiply(0.2),
        );
        painter.line_segment(
            [
                egui::pos2(center, rect.top()),
                egui::pos2(center, rect.bottom()),
            ],
            egui::Stroke::new(1.0_f32, super::MUTED),
        );
        if let Some(delta) = delta {
            let x = center + range * (delta as f32 / 12.0).clamp(-1.0, 1.0);
            painter.circle_filled(egui::pos2(x, rect.center().y), 6.0, color);
        }
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                color,
                delta
                    .map(|d| {
                        format!(
                            "{d:+.1} dB relative to {}",
                            reference_id.as_deref().unwrap_or("reference")
                        )
                    })
                    .unwrap_or_else(|| "Difference unavailable".into()),
            );
            if let Some(delta) = delta {
                ui.label(if delta.abs() <= 0.5 {
                    "Matched · within ±0.5 dB"
                } else if delta > 0.0 {
                    "Lower this speaker’s physical volume"
                } else {
                    "Raise this speaker’s physical volume"
                });
            }
        });
        if let Some(level) = level {
            ui.label(format!(
                "Microphone band RMS: {:.1} dBFS · signal / background: {:.1} dB",
                level.microphone_dbfs, level.snr_db
            ));
            if !level.usable() {
                ui.colored_label(super::WARM, "Signal is too close to background for reliable matching. Check the routing and microphone; the reference cannot be saved yet.");
            }
        } else if fresh && phase == Some(Phase::Listening) {
            ui.colored_label(super::WARM, "No clear signal above background. Check that the selected speaker is connected and playing.");
        }
        if active && let Some(reading) = &run.reading {
            ui.label(format!("Auto-stop in {} s", reading.remaining_seconds));
        }
        if let Some(error) = &run.error {
            ui.colored_label(super::RED, error);
        }
        if !current {
            ui.colored_label(
                super::WARM,
                "Setup changed; start a new run to refresh the meter.",
            );
        }
    }
    ui.label(RichText::new("Relative matching, not calibrated SPL. The comparison removes existing digital trims without changing them. Start quietly; stop noise before switching speakers. Remeasure alignment after adjusting any physical volume.").small().color(super::MUTED));
    ui.label(RichText::new("Reference readings are valid only for this input, microphone position, renderer setup and test level; capture a new reference after restarting.").small().color(super::MUTED));
}
