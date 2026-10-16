//! Presentation only: actions are executed by the shared controller.
use companion_service::alignment::Alignment;
use eframe::egui::{self, RichText};

pub fn draw(ui: &mut egui::Ui, a: &Alignment, busy: bool) -> Option<&'static str> {
    let mut action = None;
    ui.heading("Delay and level alignment");
    ui.label("Two independent passes · fixed microphone · 500–2000 Hz relative levels");
    ui.label(RichText::new("Experimental arrival estimates: repeatability does not prove an unobstructed direct path. No EQ or subwoofer alignment.").small().color(super::WARM));
    ui.label(format!(
        "Position: {} · Microphone: {} · Reference: {}",
        a.settings.position_id,
        a.settings.microphone_id,
        a.settings.reference_output.as_deref().unwrap_or("—")
    ));
    for reason in &a.blockers {
        ui.colored_label(super::RED, reason);
    }
    if a.application_uncertain {
        ui.colored_label(super::RED,"Application outcome is uncertain. Inspect Omniphony and the saved original settings before further changes.");
    }
    ui.add_space(8.0);
    egui::ScrollArea::horizontal()
        .id_salt("alignment_columns")
        .show(ui, |ui| {
            egui::Grid::new("alignment_table")
                .striped(true)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    for title in [
                        "Speaker",
                        "Arrival",
                        "Level",
                        "Repeat Δt / ΔL",
                        "Gain: before → target",
                        "Delay: before → target",
                    ] {
                        ui.strong(title);
                    }
                    ui.end_row();
                    for row in &a.rows {
                        ui.label(&row.output_id);
                        if let Some(q) = &row.qualified {
                            ui.label(format!("{:+.3} ms", q.arrival_ms));
                            ui.label(format!("{:+.2} dB", q.level_db));
                            ui.label(format!(
                                "{:.3} ms / {:.2} dB",
                                q.arrival_repeat_spread_ms, q.level_repeat_spread_db
                            ));
                        } else {
                            ui.label("Unresolved");
                            ui.label(
                                row.repeats
                                    .last()
                                    .and_then(|r| r.level_db)
                                    .map(|v| format!("{v:+.2} dB"))
                                    .unwrap_or("—".into()),
                            );
                            ui.label("—");
                        }
                        if let Some(speaker) = a.profile.as_ref().and_then(|p| {
                            p.target_snapshot
                                .speakers
                                .iter()
                                .find(|s| s.id == row.output_id)
                        }) {
                            let before = a.snapshot.outputs.iter().find(|o| o.id == row.output_id);
                            ui.label(
                                before
                                    .map(|o| {
                                        format!(
                                            "{:+.2} → {:+.2} dB",
                                            o.gain_db, speaker.settings.gain_db
                                        )
                                    })
                                    .unwrap_or_else(|| {
                                        format!("{:+.2} dB", speaker.settings.gain_db)
                                    }),
                            );
                            ui.label(
                                before
                                    .map(|o| {
                                        format!(
                                            "{:.3} → {:.3} ms",
                                            o.delay_ms, speaker.settings.delay_ms
                                        )
                                    })
                                    .unwrap_or_else(|| {
                                        format!("{:.3} ms", speaker.settings.delay_ms)
                                    }),
                            );
                        } else {
                            ui.label("—");
                            ui.label("—");
                        }
                        ui.end_row();
                        if let Some(reason) = &row.reason {
                            ui.label("");
                            ui.colored_label(super::WARM, reason);
                            ui.end_row();
                        }
                    }
                });
        });
    ui.add_space(8.0);
    ui.label("Targets include existing output settings. Earlier speakers receive delay; louder speakers are attenuated. Levels are relative transfer dB, not SPL.");
    ui.horizontal_wrapped(|ui| {
        let ready = !busy && !a.application_uncertain && a.profile.is_some();
        if ui
            .add_enabled(
                ready && a.applied_snapshot.is_none(),
                egui::Button::new("Apply gains and delays"),
            )
            .clicked()
        {
            action = Some("apply");
        }
        if ui
            .add_enabled(
                ready && a.applied_snapshot.is_some(),
                egui::Button::new("Verify · 2 passes"),
            )
            .clicked()
        {
            action = Some("verify");
        }
        if ui
            .add_enabled(
                ready && a.applied_snapshot.is_some(),
                egui::Button::new("Restore original settings"),
            )
            .clicked()
        {
            action = Some("restore");
        }
    });
    if let Some(note) = &a.verification_note {
        ui.colored_label(super::WARM, note);
    }
    if let Some(v) = &a.verification {
        ui.colored_label(
            if v.passed { super::ACCENT } else { super::WARM },
            if v.passed {
                "Verification passed at this microphone position"
            } else {
                "Verification outside tolerance — review or restore"
            },
        );
        ui.label(format!(
            "Arrival spread: {:.3} → {:.3} ms · Level spread: {:.2} → {:.2} dB",
            v.before.arrival_spread_ms,
            v.after.arrival_spread_ms,
            v.before.level_spread_db,
            v.after.level_spread_db
        ));
        ui.label("Prototype tolerances: 0.10 ms / 0.50 dB. These are comparison thresholds, not a claim of hardware accuracy.");
    } else if a.applied_snapshot.is_some() {
        ui.colored_label(
            super::WARM,
            "Settings applied and read back; acoustic verification still required.",
        );
    }
    ui.label("Application changes live settings only. Save in Studio after verification if you want them in Omniphony's persistent configuration.");
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Session: {}", a.directory.display()));
        if ui.small_button("Copy session folder").clicked() {
            ui.ctx().copy_text(a.directory.display().to_string());
        }
    });
    action
}
