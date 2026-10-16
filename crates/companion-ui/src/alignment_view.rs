//! Alignment presentation: session actions above the list, estimates in each speaker card.
use companion_service::alignment::{Alignment, Row};
use eframe::egui::{self, RichText};

pub fn session(ui: &mut egui::Ui, a: &Alignment, busy: bool) -> Option<&'static str> {
    let mut action = None;
    ui.label(RichText::new("Alignment session").strong());
    ui.label(
        RichText::new(format!(
            "{} · Reference: {} · Two independent passes",
            a.settings.position_id,
            a.settings.reference_output.as_deref().unwrap_or("—")
        ))
        .small()
        .color(super::MUTED),
    );
    if a.application_uncertain {
        ui.colored_label(super::RED, "Application outcome is uncertain. Inspect Omniphony and the saved original settings before further changes.");
    } else if a.profile.is_none() {
        ui.colored_label(super::WARM, "No correction proposal. Speaker gains and delays have not been changed by this session.");
    } else if a.applied_snapshot.is_none() {
        ui.colored_label(
            super::ACCENT,
            "Correction proposal ready · inspect the targets in each speaker row",
        );
    }
    if !a.blockers.is_empty() {
        ui.colored_label(
            super::WARM,
            format!(
                "{} session issue(s) · see session details",
                a.blockers.len()
            ),
        )
        .on_hover_text(a.blockers.join("\n"));
    }
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
    egui::CollapsingHeader::new("Alignment session details").show(ui, |ui| {
        ui.label("Two independent passes · fixed microphone · 500–2000 Hz relative levels");
        ui.label(RichText::new("Experimental arrival estimates: repeatability does not prove an unobstructed direct path. No EQ or subwoofer alignment.").small().color(super::WARM));
        ui.label(format!("Position: {} · Microphone: {} · Reference: {}", a.settings.position_id, a.settings.microphone_id, a.settings.reference_output.as_deref().unwrap_or("—")));
        pass_summary(ui, "Baseline", a.pass_reports.as_deref(), a.rows.len());
        if let Some(passes) = &a.verification_pass_reports {
            pass_summary(ui, "Verification", Some(passes), a.rows.len());
        }
        for reason in &a.blockers { ui.colored_label(super::RED, reason); }
        ui.label("Targets include existing output settings. Earlier speakers receive delay; louder speakers are attenuated. Levels are relative transfer dB, not SPL.");
        ui.label("Application changes live settings only. Save in Studio after verification if you want them in Omniphony's persistent configuration.");
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Session: {}", a.directory.display()));
            if ui.small_button("Copy session folder").clicked() {
                ui.ctx().copy_text(a.directory.display().to_string());
            }
        });
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
    action
}

/// The same qualified estimates/diagnostic fallbacks previously shown in the table.
pub fn speaker(ui: &mut egui::Ui, a: &Alignment, row: &Row, older: bool) {
    let arrival = row
        .qualified
        .as_ref()
        .map(|q| q.arrival_ms)
        .or_else(|| row.repeats.iter().rev().find_map(|r| r.arrival_ms));
    let level = row
        .qualified
        .as_ref()
        .map(|q| q.level_db)
        .or_else(|| row.repeats.last().and_then(|r| r.level_db));
    let spread = row
        .qualified
        .as_ref()
        .map(|q| (q.arrival_repeat_spread_ms, q.level_repeat_spread_db))
        .or_else(|| {
            row.repeats
                .first()
                .zip(row.repeats.get(1))
                .and_then(|(first, second)| {
                    Some((
                        (first.arrival_ms? - second.arrival_ms?).abs(),
                        (first.level_db? - second.level_db?).abs(),
                    ))
                })
        });
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("Alignment · {} / 2 passes", row.take_folders.len()))
                .small()
                .strong(),
        );
        ui.label(
            RichText::new(if row.qualified.is_some() {
                "Repeatable"
            } else {
                "Diagnostic · not qualified"
            })
            .small()
            .color(if row.qualified.is_some() {
                super::ACCENT
            } else {
                super::WARM
            }),
        );
        if older {
            ui.colored_label(
                super::WARM,
                "Previous alignment · settings or measurement changed",
            );
        }
    });
    ui.horizontal_wrapped(|ui| {
        metric(
            ui,
            "Arrival candidate",
            arrival
                .map(|v| format!("{v:+.3} ms"))
                .unwrap_or("Unavailable".into()),
        );
        metric(
            ui,
            "Level",
            level
                .map(|v| format!("{v:+.2} dB"))
                .unwrap_or("Unavailable".into()),
        );
        metric(
            ui,
            "Repeat Δt / ΔL",
            spread
                .map(|(t, l)| format!("{t:.3} ms / {l:.2} dB"))
                .unwrap_or("Unavailable".into()),
        );
        let grade = row.quality.iter().map(|q| q.grade()).min();
        ui.vertical(|ui| {
            super::caption(ui, "Quality");
            ui.label(grade.map(|g| g.label()).unwrap_or("Not rated"))
                .on_hover_text(
                    row.quality
                        .iter()
                        .flat_map(|q| {
                            q.acquisition
                                .details
                                .iter()
                                .chain(&q.timing.details)
                                .chain(&q.level.details)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
        });
    });
    if let Some(target) = a.profile.as_ref().and_then(|p| {
        p.target_snapshot
            .speakers
            .iter()
            .find(|s| s.id == row.output_id)
    }) {
        let before = a.snapshot.outputs.iter().find(|o| o.id == row.output_id);
        ui.horizontal_wrapped(|ui| {
            metric(
                ui,
                "Gain: before → target",
                before
                    .map(|o| format!("{:+.2} → {:+.2} dB", o.gain_db, target.settings.gain_db))
                    .unwrap_or_else(|| format!("{:+.2} dB", target.settings.gain_db)),
            );
            metric(
                ui,
                "Delay: before → target",
                before
                    .map(|o| format!("{:.3} → {:.3} ms", o.delay_ms, target.settings.delay_ms))
                    .unwrap_or_else(|| format!("{:.3} ms", target.settings.delay_ms)),
            );
        });
    }
    if let Some(warning) = arrival.and_then(calibration_core::long_delay_warning) {
        ui.colored_label(super::WARM, warning);
    }
    if let Some(reason) = &row.reason {
        ui.colored_label(super::WARM, reason);
    }
}

fn metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        super::caption(ui, label);
        ui.monospace(value);
    });
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
