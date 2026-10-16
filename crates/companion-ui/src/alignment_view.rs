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
    pass_summary(ui, "Baseline", a.pass_reports.as_deref(), a.rows.len());
    if let Some(passes) = &a.verification_pass_reports {
        pass_summary(ui, "Verification", Some(passes), a.rows.len());
    }
    if a.profile.is_none() && !a.application_uncertain {
        ui.colored_label(super::WARM, "No correction proposal. Speaker gains and delays have not been changed by this session.");
    }
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
                        "Arrival candidate",
                        "Level",
                        "Repeat Δt / ΔL",
                        "Quality",
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
                            ui.label(row.repeats.iter().rev().find_map(|r| r.arrival_ms)
                                .map(|v| format!("{v:+.3} ms" )).unwrap_or("Unavailable".into()))
                                .on_hover_text("Latest available diagnostic estimate; this row has not qualified for correction.");
                            ui.label(
                                row.repeats
                                    .last()
                                    .and_then(|r| r.level_db)
                                    .map(|v| format!("{v:+.2} dB"))
                                    .unwrap_or("—".into()),
                            );
                            let spread = row.repeats.first().zip(row.repeats.get(1)).and_then(|(a,b)| Some(((a.arrival_ms? - b.arrival_ms?).abs(), (a.level_db? - b.level_db?).abs())));
                            ui.label(spread.map(|(t,l)| format!("{t:.3} ms / {l:.2} dB")).unwrap_or("—".into()));
                        }
                        let grade = row.quality.iter().map(|q| q.grade()).min();
                        ui.label(grade.map(|g| g.label()).unwrap_or("Not rated"))
                            .on_hover_text(row.quality.iter().flat_map(|q| q.acquisition.details.iter().chain(&q.timing.details).chain(&q.level.details)).cloned().collect::<Vec<_>>().join("\n"));
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
                        if let Some(warning) = row
                            .qualified
                            .as_ref()
                            .map(|q| q.arrival_ms)
                            .or_else(|| row.repeats.last().and_then(|r| r.arrival_ms))
                            .and_then(calibration_core::long_delay_warning)
                        {
                            ui.label("");
                            ui.colored_label(super::WARM, warning);
                            ui.end_row();
                        }
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
        if ui.add_enabled(!busy && a.applied_snapshot.is_none() && !a.application_uncertain, egui::Button::new("Reanalyze saved passes"))
            .on_hover_text("Recompute diagnostic results and quality from the original audio. Creates a separate report; no playback or correction.").clicked() {
            action = Some("reanalyze");
        }
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

fn pass_summary(
    ui: &mut egui::Ui,
    label: &str,
    passes: Option<&[companion_service::alignment::PassReport]>,
    speakers: usize,
) {
    let Some(passes) = passes else {
        ui.label(format!(
            "{label}: pass completion was not recorded by this older session."
        ));
        return;
    };
    for pass in 0..2 {
        let status = match passes.get(pass) {
            None => "not started".to_owned(),
            Some(p) => format!(
                "{} · {} / {} speakers analyzed",
                if p.error.is_some() {
                    "stopped"
                } else {
                    "complete"
                },
                p.analyzed_speakers,
                speakers
            ),
        };
        ui.label(format!("{label} pass {} / 2: {status}", pass + 1));
    }
}
