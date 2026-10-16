//! Input meter presentation; device ownership and deadlines stay in the service.
use companion_service::{
    controller::{Controller, Job},
    measurement::MeasureProgress,
};
use eframe::egui::{self, RichText};

pub fn draw(ui: &mut egui::Ui, controller: &mut Controller) {
    ui.add_space(10.0);
    let checking = matches!(controller.state.job, Some(Job::InputCheck));
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !controller.busy() && controller.input_request().is_ok(),
                egui::Button::new("Check microphone"),
            )
            .clicked()
        {
            controller.check_input();
        }
        if ui
            .add_enabled(
                checking && !controller.state.stopping,
                egui::Button::new("Stop check"),
            )
            .clicked()
        {
            controller.cancel();
        }
    });
    ui.label(
        RichText::new("10-second live meter · no playback or saved audio")
            .small()
            .color(super::MUTED),
    );
    let preflight = match controller.state.progress {
        Some(MeasureProgress::CheckingInput(level)) => Some(level),
        _ => None,
    };
    let check = controller.input_check();
    let level = preflight.or_else(|| check.and_then(|c| c.level));
    let live = checking || preflight.is_some();
    let peak = level.and_then(|l| l.peak_dbfs);
    let meter = peak.map_or(0.0, |db| ((db + 90.0) / 90.0).clamp(0.0, 1.0) as f32);
    let label = format!(
        "{} · Peak {}",
        if live { "LIVE" } else { "Last check" },
        peak.map(|v| format!("{v:.1} dBFS"))
            .unwrap_or_else(|| "—".into())
    );
    ui.add(
        egui::ProgressBar::new(meter)
            .desired_width(ui.available_width())
            .desired_height(20.0)
            .text(label)
            .fill(if level.is_some_and(|l| l.clipped_samples > 0) {
                super::RED
            } else {
                super::ACCENT.gamma_multiply(0.45)
            }),
    );
    let problem = check
        .and_then(|c| c.error.as_deref())
        .or_else(|| level.and_then(|l| l.problem()));
    if let Some(problem) = problem {
        ui.label(RichText::new(problem).small().color(super::WARM));
    } else if let Some(level) = level {
        let rms = level
            .rms_dbfs
            .map(|v| format!("{v:.1} dBFS"))
            .unwrap_or_else(|| "—".into());
        ui.label(
            RichText::new(format!("Input activity · RMS {rms}"))
                .small()
                .color(super::ACCENT),
        );
        ui.label(RichText::new("Confirm the meter responds when you speak near the microphone. Activity alone does not identify the source.").small().color(super::MUTED));
    } else {
        ui.label(
            RichText::new(if checking {
                "Opening microphone…"
            } else {
                "Check that the meter responds to your microphone before measuring."
            })
            .small()
            .color(super::MUTED),
        );
    }
    if !live && level.is_some() {
        ui.label(
            RichText::new("Check stopped. Every measurement rechecks the input before playback.")
                .small()
                .color(super::MUTED),
        );
    }
}
