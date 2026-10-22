//! Room correction presentation: card lines with a small response plot, the action bar
//! below the cards and the detail in the Inspect drawer. Everything here reads the
//! alignment's [`Correction`] and subwoofer integration as companion-service produces
//! them; actions go back through [`Action`], nothing writes them.
use calibration_core::room_correction::Curves;
use companion_service::{
    alignment::Alignment,
    bass::BassIntegration,
    correction::{Correction, Live, Saved, Target},
};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};

/// What a correction control asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Apply(Target),
    Verify,
}

/// A card's small plot: before, predicted and the correction, on a log-frequency
/// axis from 0 to 1, about fifty points each.
#[derive(Debug, Clone, Default)]
pub struct CardPlot {
    pub before: Vec<[f32; 2]>,
    pub after: Vec<[f32; 2]>,
    pub correction: Vec<[f32; 2]>,
    /// Measured again through the filter, when confirmed.
    pub measured: Vec<[f32; 2]>,
}
const PLOT_POINTS: usize = 48;
/// The vertical range of a card plot, in dB around the level band.
const PLOT_DB: f32 = 12.0;

fn thin(curves: &Curves, values: &[f32]) -> Vec<[f32; 2]> {
    let f = &curves.frequency_hz;
    let (Some(&lo), Some(&hi)) = (f.first(), f.last()) else {
        return vec![];
    };
    if hi <= lo || values.len() != f.len() {
        return vec![];
    }
    let step = (f.len() / PLOT_POINTS).max(1);
    (0..f.len())
        .step_by(step)
        .filter(|&i| values[i].is_finite())
        .map(|i| [((f[i] / lo).ln() / (hi / lo).ln()) as f32, values[i]])
        .collect()
}

impl CardPlot {
    fn new(curves: &Curves, measured: Option<&[f32]>) -> Self {
        Self {
            before: thin(curves, &curves.measured_db),
            after: thin(curves, &curves.predicted_db),
            correction: thin(curves, &curves.filter_db),
            measured: measured.map(|m| thin(curves, m)).unwrap_or_default(),
        }
    }
}

/// Paint `plot` in `rect`: the measured response muted, the prediction in the accent,
/// the correction in amber, measured again in white.
pub fn paint_card_plot(painter: &egui::Painter, rect: Rect, plot: &CardPlot) {
    painter.rect_filled(rect, 3.0, super::BG.gamma_multiply(0.6));
    let map = |[x, db]: [f32; 2]| {
        Pos2::new(
            rect.left() + x * rect.width(),
            rect.center().y - (db.clamp(-PLOT_DB, PLOT_DB) / PLOT_DB) * rect.height() * 0.5,
        )
    };
    painter.line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(0.5_f32, super::MUTED.gamma_multiply(0.4)),
    );
    for (points, color, width) in [
        (&plot.before, super::MUTED, 1.0_f32),
        (&plot.correction, super::WARM, 1.0),
        (&plot.after, super::ACCENT, 1.4),
        (&plot.measured, super::TEXT, 1.0),
    ] {
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points.iter().copied().map(map).collect(),
                Stroke::new(width, color),
            ));
        }
    }
}

/// The lines a speaker card shows for the correction, when it has any.
pub fn card(a: &Alignment, id: &str) -> Option<super::alignment_view::Card> {
    let c = a.correction.as_ref();
    let bass = a.bass.as_ref();
    if let Some(bass) = bass
        && bass.plan.subwoofer == id
    {
        return subwoofer_card(bass);
    }
    let c = c?;
    let summary = c.summary_of(id)?;
    let grade = a
        .rows
        .iter()
        .find(|r| r.output_id == id)
        .and_then(|r| r.quality.iter().map(|q| q.grade()).min());
    let check = c
        .verification
        .as_ref()
        .and_then(|v| v.speakers.iter().find(|s| s.output_id == id));
    let mut details = String::new();
    let (measured, planned, plot) = match (&summary.refusal, summary.deviation_db) {
        (Some(reason), _) => (
            ("No room correction".to_owned(), super::WARM),
            Some((reason.clone(), super::MUTED)),
            None,
        ),
        (None, Some([before, after])) => {
            let measured = match check {
                Some(v) => (
                    format!(
                        "Room {} » {} dB measured",
                        super::tenths(v.deviation_db[0]),
                        super::tenths(v.deviation_db[2])
                    ),
                    if v.from_prediction_db <= 1.0 {
                        super::ACCENT
                    } else {
                        super::WARM
                    },
                ),
                None => (
                    format!(
                        "Room {} » {} dB",
                        super::tenths(before),
                        super::tenths(after)
                    ),
                    super::TEXT,
                ),
            };
            let mut planned = format!(
                "+{} / {} dB",
                super::tenths(summary.headroom_db.unwrap_or(0.0).max(0.0)),
                super::tenths(summary.deepest_cut_db.unwrap_or(0.0).min(0.0))
            );
            if !summary.reflections.is_empty() {
                planned.push_str(&format!(" · {} refl.", summary.reflections.len()));
            }
            let color = if summary.warnings.is_empty() {
                super::MUTED
            } else {
                super::WARM
            };
            details.push_str(&format!(
                "Room correction: {} dB from the target's shape before, {} dB predicted after (RMS over {}).\nLargest boost {} dB (the headroom the filter needs), deepest cut {} dB.",
                super::tenths(before),
                super::tenths(after),
                band(summary.evaluation_band_hz),
                super::tenths(summary.headroom_db.unwrap_or(0.0)),
                super::tenths(summary.deepest_cut_db.unwrap_or(0.0)),
            ));
            if let Some(v) = check {
                details.push_str(&format!(
                    "\nMeasured again: {} dB from the target's shape, {} dB from the prediction.",
                    super::tenths(v.deviation_db[2]),
                    super::tenths(v.from_prediction_db)
                ));
            }
            (
                measured,
                Some((planned, color)),
                c.curves_of(id)
                    .map(|curves| CardPlot::new(curves, check.map(|v| v.measured_db.as_slice()))),
            )
        }
        (None, None) => return None,
    };
    for warning in &summary.warnings {
        details.push('\n');
        details.push_str(warning);
    }
    if let Some(reason) = &summary.refusal {
        details.push_str(&format!("Not corrected: {reason}"));
    }
    details.push_str("\nClick the card for the curves and diagnostics.");
    Some(super::alignment_view::Card {
        grade,
        measured,
        planned,
        details,
        far: None,
        plot,
    })
}

fn subwoofer_card(bass: &BassIntegration) -> Option<super::alignment_view::Card> {
    let analysis = bass.analysis.as_ref();
    let proposal = analysis.map(|a| &a.proposal);
    let (measured, planned, details) = match proposal {
        None => (
            ("Subwoofer not integrated yet".to_owned(), super::MUTED),
            None,
            bass.analysis_error
                .clone()
                .unwrap_or_else(|| "Measure it at the main seat.".into()),
        ),
        Some(p) if p.refusal.is_some() => (
            ("Subwoofer: no proposal".to_owned(), super::WARM),
            p.refusal.clone().map(|r| (r, super::MUTED)),
            p.refusal.clone().unwrap_or_default(),
        ),
        Some(p) => {
            let modes = analysis
                .and_then(|a| a.mode_correction.as_ref())
                .and_then(|m| m.filter.as_ref())
                .map(|f| format!(" · modes {} dB", super::tenths(f.deepest_cut_db)));
            (
                (
                    format!(
                        "Delay {} » {} ms · {} dB",
                        super::tenths(p.subwoofer.delay_ms),
                        super::tenths(p.subwoofer_delay_ms),
                        super::alignment_view::signed(p.setting.gain_change_db, 1, "")
                            .trim_end()
                    ),
                    super::TEXT,
                ),
                Some((
                    format!(
                        "{}{}",
                        if p.setting.inverted {
                            "Polarity inverted"
                        } else {
                            "Polarity kept"
                        },
                        modes.unwrap_or_default()
                    ),
                    super::MUTED,
                )),
                "The subwoofer's integration: applied with the room correction. Click the card for every speaker's sum.".to_owned(),
            )
        }
    };
    Some(super::alignment_view::Card {
        grade: None,
        measured,
        planned,
        details,
        far: None,
        plot: None,
    })
}

fn band(band: Option<[f64; 2]>) -> String {
    band.map_or_else(
        || "its band".into(),
        |[lo, hi]| format!("{} – {}", hz(lo), hz(hi)),
    )
}
fn hz(value: f64) -> String {
    if value >= 1000.0 {
        format!("{:.1} kHz", value / 1000.0)
    } else {
        format!("{value:.0} Hz")
    }
}

/// What the action bar needs to know, gathered by the app.
pub struct Bar<'a> {
    pub correction: Option<&'a Correction>,
    pub live: Option<Live>,
    pub saved: Option<Saved>,
    pub apply: Result<(), String>,
    pub compare: Result<(), String>,
    pub corrected: Result<(), String>,
    pub undo: Result<(), String>,
    pub verify: Result<(), String>,
    /// Which write is running, if any.
    pub applying: Option<Target>,
    pub verifying: bool,
}

/// Apply, Original/Corrected, Confirm and Undo, with a line saying what plays and
/// whether Omniphony has saved it.
pub fn action_bar(ui: &mut egui::Ui, bar: &Bar) -> Option<Action> {
    let mut action = None;
    let applied = matches!(bar.live, Some(Live::Corrected | Live::Comparing))
        && bar.correction.is_some_and(|c| c.applied.is_some());
    let comparing = bar.live == Some(Live::Comparing);
    let hint = |r: &Result<(), String>| r.as_ref().err().cloned().unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        let enabled = bar.apply.is_ok() && !applied;
        let label = match bar.applying {
            Some(Target::Corrected) if !applied => "Applying…",
            _ => "Apply correction",
        };
        if ui
            .add_enabled(
                enabled,
                egui::Button::new(
                    RichText::new(label)
                        .strong()
                        .color(if enabled { super::BG } else { super::MUTED }),
                )
                .fill(if enabled { super::ACCENT } else { super::PANEL })
                .min_size(Vec2::new(150.0, 30.0)),
            )
            .on_hover_text("Send every speaker's filter to Omniphony in one write (and the subwoofer's delay, level and filter when it is integrated). Live only: press Save in Studio to keep them.")
            .on_disabled_hover_text(if applied { "Already applied.".into() } else { hint(&bar.apply) })
            .clicked()
        {
            action = Some(Action::Apply(Target::Corrected));
        }
        egui::Frame::new()
            .stroke(Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(egui::Margin::same(2))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    let original_selected = !applied || comparing;
                    let original = ui
                        .add_enabled(
                            original_selected || bar.compare.is_ok(),
                            egui::Button::selectable(original_selected, "Original"),
                        )
                        .on_hover_text("Listen without this correction: each speaker plays the filter it had before, or none at the same latency. The subwoofer's integration stays as applied.")
                        .on_disabled_hover_text(hint(&bar.compare));
                    if original.clicked() && applied && !comparing {
                        action = Some(Action::Apply(Target::Compare));
                    }
                    let corrected_selected = applied && !comparing;
                    let ready = if applied { &bar.corrected } else { &bar.apply };
                    let corrected = ui
                        .add_enabled(
                            corrected_selected || ready.is_ok(),
                            egui::Button::selectable(corrected_selected, "Corrected"),
                        )
                        .on_hover_text("Listen through the correction filters.")
                        .on_disabled_hover_text(hint(ready));
                    if corrected.clicked() && !corrected_selected {
                        action = Some(Action::Apply(Target::Corrected));
                    }
                });
            });
        let label = if bar.verifying {
            "Confirming…"
        } else {
            "Confirm by re-measuring"
        };
        if ui
            .add_enabled(bar.verify.is_ok(), egui::Button::new(label))
            .on_hover_text("With the microphone back at the main seat: measure every corrected speaker again through its filter and compare it with the prediction (and the subwoofer's sums when it is integrated).")
            .on_disabled_hover_text(hint(&bar.verify))
            .clicked()
        {
            action = Some(Action::Verify);
        }
        if ui
            .add_enabled(bar.undo.is_ok(), egui::Button::new("Undo"))
            .on_hover_text("Put back every speaker's filter from before this correction (none, or the file it played), and the subwoofer as it was measured.")
            .on_disabled_hover_text(hint(&bar.undo))
            .clicked()
        {
            action = Some(Action::Apply(Target::Original));
        }
    });
    let (playing, status, color) = status(bar);
    ui.horizontal_wrapped(|ui| {
        if let Some((text, color)) = playing {
            super::badge(ui, text, color);
        }
        ui.add(egui::Label::new(RichText::new(status).small().color(color)).wrap());
    });
    action
}

/// What plays, as a badge, and the status sentence.
fn status(bar: &Bar) -> (Option<(&'static str, Color32)>, String, Color32) {
    let Some(c) = bar.correction else {
        return (
            None,
            "Compute the correction in the setup rail (04 Room correction), then review it on the cards.".into(),
            super::MUTED,
        );
    };
    if c.uncertain {
        return (
            None,
            "It is not known whether the last change reached Omniphony; nothing else is sent until a refresh shows where it went.".into(),
            super::RED,
        );
    }
    let corrected = c.filters.len();
    let save = "Live only: Omniphony keeps the filters until it restarts or reloads. Press Save in Omniphony Studio to keep them; Save writes them as WAV files next to its configuration.";
    let mut confirmed = String::new();
    if let Some(v) = &c.verification {
        let close = v
            .speakers
            .iter()
            .filter(|s| s.from_prediction_db <= 1.0)
            .count();
        confirmed = format!(
            " Confirmed at the main seat: {close} of {} speakers within 1 dB of the prediction.",
            v.speakers.len()
        );
    } else if let Some(note) = &c.verification_note {
        confirmed = format!(" {note}");
    }
    match (bar.live, &bar.saved) {
        (None, _) => (
            None,
            "Connect to Omniphony to apply or compare.".into(),
            super::MUTED,
        ),
        (Some(Live::Other), _) => (
            None,
            "The speakers' filters changed in Omniphony since this correction (in Studio, or a reload): refresh, or set them back there.".into(),
            super::WARM,
        ),
        (Some(Live::Original), _) if c.applied.is_none() => (
            Some(("PLAYING · ORIGINAL", super::MUTED)),
            format!(
                "{corrected} speaker filter(s) ready, not applied. Apply sends them to Omniphony live; press Save in Studio to keep them."
            ),
            super::MUTED,
        ),
        (Some(Live::Comparing), _) => (
            Some(("PLAYING · ORIGINAL", super::WARM)),
            "Listening without the correction, at the same latency. Switch back to Corrected before confirming, and before Save in Studio (Save would keep what plays).".into(),
            super::WARM,
        ),
        (Some(Live::Corrected), Some(Saved::Saved)) => (
            Some(("PLAYING · CORRECTED", super::ACCENT)),
            format!("Saved in Omniphony's configuration: every filter plays from a file.{confirmed}"),
            super::ACCENT,
        ),
        (Some(Live::Corrected), Some(Saved::Unsaved(ids))) if ids.len() < corrected => (
            Some(("PLAYING · CORRECTED", super::ACCENT)),
            format!("{} filter(s) not saved yet. {save}{confirmed}", ids.len()),
            super::WARM,
        ),
        (Some(Live::Corrected), _) => (
            Some(("PLAYING · CORRECTED", super::ACCENT)),
            format!("{save}{confirmed}"),
            super::ACCENT,
        ),
        (Some(Live::Original), _) => (
            Some(("PLAYING · ORIGINAL", super::MUTED)),
            "Omniphony plays the original filters again.".into(),
            super::MUTED,
        ),
    }
}

/// The correction of one speaker in the Inspect drawer: its curves, limits and
/// diagnostics; for the subwoofer, its integration with every speaker.
pub fn inspect(ui: &mut egui::Ui, a: &Alignment, id: &str, name: &str, lab: bool) -> bool {
    if let Some(bass) = &a.bass
        && bass.plan.subwoofer == id
    {
        inspect_subwoofer(ui, bass, name);
        return true;
    }
    let Some(c) = &a.correction else {
        return false;
    };
    let Some(summary) = c.summary_of(id) else {
        return false;
    };
    ui.label(RichText::new(format!("Room correction · {name}")).strong());
    if let Some(reason) = &summary.refusal {
        ui.colored_label(super::WARM, format!("Not corrected: {reason}"));
        for warning in &summary.warnings {
            ui.label(RichText::new(warning).small().color(super::WARM));
        }
        ui.add_space(8.0);
        return true;
    }
    let check = c
        .verification
        .as_ref()
        .and_then(|v| v.speakers.iter().find(|s| s.output_id == id));
    if let Some(curves) = c.curves_of(id) {
        plot(ui, curves, summary, check.map(|v| v.measured_db.as_slice()));
    }
    egui::Grid::new(("correction_metrics", id))
        .num_columns(2)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(RichText::new(label).small().color(super::MUTED));
                ui.label(RichText::new(value).small());
                ui.end_row();
            };
            if let Some([before, after]) = summary.deviation_db {
                row(
                    "From the target's shape",
                    format!(
                        "{} dB » {} dB predicted, over {}",
                        super::tenths(before),
                        super::tenths(after),
                        band(summary.evaluation_band_hz)
                    ),
                );
            }
            if let Some(v) = check {
                row(
                    "Measured again",
                    format!(
                        "{} dB from the target's shape · {} dB from the prediction",
                        super::tenths(v.deviation_db[2]),
                        super::tenths(v.from_prediction_db)
                    ),
                );
            }
            row(
                "Headroom",
                format!(
                    "largest boost {} dB (limit {} dB)",
                    super::tenths(summary.headroom_db.unwrap_or(0.0)),
                    super::tenths(c.settings.max_boost_db)
                ),
            );
            row(
                "Deepest cut",
                format!(
                    "{} dB (limit -{} dB)",
                    super::tenths(summary.deepest_cut_db.unwrap_or(0.0)),
                    super::tenths(c.settings.max_cut_db)
                ),
            );
            row("Corrected over", band(summary.band_hz));
            if let Some(low) = summary.low_limit_hz {
                row("No boost below", format!("{} (its -6 dB point)", hz(low)));
            }
            if let Some(spread) = summary.spread_db {
                row("Positions spread", format!("{} dB", super::tenths(spread)));
            }
            if let Some(r) = summary.repeatability_db {
                row("Main-seat takes apart", format!("{} dB", super::tenths(r)));
            }
            if lab {
                row(
                    "Level kept",
                    format!(
                        "{} dB gain on the filter, so 500 Hz–2 kHz stays as aligned",
                        super::tenths(summary.normalisation_db.unwrap_or(0.0))
                    ),
                );
                row(
                    "Filter",
                    format!(
                        "{} taps at {} Hz, minimum phase",
                        summary.taps.unwrap_or(0),
                        summary.sample_rate.unwrap_or(0)
                    ),
                );
            }
        });
    if !summary.positions.is_empty() {
        ui.add_space(4.0);
        ui.label(
            RichText::new("Positions against their average")
                .small()
                .strong(),
        );
        for p in &summary.positions {
            let (text, color) = match p.deviation_db {
                Some(d) if d > 2.0 => (
                    format!("{} dB · low consistency", super::tenths(d)),
                    super::WARM,
                ),
                Some(d) => (format!("{} dB", super::tenths(d)), super::MUTED),
                None => ("—".into(), super::MUTED),
            };
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}{} · {} take(s)",
                        p.position_id,
                        if p.main_seat { " (main seat)" } else { "" },
                        p.takes
                    ))
                    .small(),
                );
                ui.label(RichText::new(text).small().color(color));
            });
        }
    }
    if !summary.reflections.is_empty() {
        ui.add_space(4.0);
        ui.label(
            RichText::new("Strong early reflections at the main seat (not corrected)")
                .small()
                .strong(),
        );
        for r in &summary.reflections {
            ui.label(
                RichText::new(format!(
                    "{:.2} ms after the direct sound · {} dB · {:.0} cm longer path",
                    r.delay_ms,
                    super::tenths(r.level_db),
                    r.extra_path_cm
                ))
                .small()
                .color(if r.level_db > -3.0 {
                    super::WARM
                } else {
                    super::MUTED
                }),
            );
        }
    }
    for warning in &summary.warnings {
        ui.label(RichText::new(warning).small().color(super::WARM));
    }
    if let Some(bass) = &a.bass
        && let Some(speaker) = bass
            .analysis
            .as_ref()
            .and_then(|x| x.proposal.speakers.iter().find(|s| s.output_id == id))
    {
        ui.add_space(4.0);
        ui.label(
            RichText::new(summation(speaker))
                .small()
                .color(super::MUTED),
        );
    }
    ui.add_space(8.0);
    true
}

fn summation(speaker: &calibration_core::bass::SpeakerResult) -> String {
    match (&speaker.excluded, speaker.summation_db) {
        (Some(reason), _) => format!("With the subwoofer: left out. {reason}"),
        (None, Some([before, _, after])) => format!(
            "With the subwoofer around {:.0} Hz: {} » {} dB below the two adding in phase.",
            speaker.crossover_hz,
            super::tenths(before),
            super::tenths(after)
        ),
        _ => String::new(),
    }
}

fn inspect_subwoofer(ui: &mut egui::Ui, bass: &BassIntegration, name: &str) {
    ui.label(RichText::new(format!("Subwoofer integration · {name}")).strong());
    let Some(analysis) = &bass.analysis else {
        ui.label(
            RichText::new(
                bass.analysis_error
                    .as_deref()
                    .unwrap_or("Measure the subwoofer at the main seat first."),
            )
            .small()
            .color(super::MUTED),
        );
        return;
    };
    let p = &analysis.proposal;
    if let Some(reason) = &p.refusal {
        ui.colored_label(super::WARM, reason);
    } else {
        let mut text = format!(
            "Delay {} » {} ms, level {} dB{}",
            super::tenths(p.subwoofer.delay_ms),
            super::tenths(p.subwoofer_delay_ms),
            super::alignment_view::signed(p.setting.gain_change_db, 1, "").trim_end(),
            if p.setting.inverted {
                ", polarity inverted"
            } else {
                ""
            }
        );
        if p.others_delay_ms > 0.0 {
            text.push_str(&format!(
                ", every other output +{} ms (adds latency)",
                super::tenths(p.others_delay_ms)
            ));
        }
        ui.label(RichText::new(text).small().strong());
        if let Some(mode) = &analysis.mode_correction {
            let line = match (&mode.filter, &mode.refusal) {
                (Some(f), _) => format!(
                    "Mode correction: cuts down to {} dB.",
                    super::tenths(f.deepest_cut_db)
                ),
                (None, Some(reason)) => format!("No mode correction: {reason}"),
                (None, None) => String::new(),
            };
            ui.label(RichText::new(line).small().color(super::MUTED));
        }
    }
    for speaker in &p.speakers {
        ui.label(
            RichText::new(format!("{}: {}", speaker.output_id, summation(speaker)))
                .small()
                .color(
                    if speaker.warnings.is_empty() && speaker.excluded.is_none() {
                        super::MUTED
                    } else {
                        super::WARM
                    },
                ),
        );
    }
    for warning in p.warnings.iter().chain(&analysis.notes) {
        ui.label(RichText::new(warning).small().color(super::WARM));
    }
    if let Some(v) = &bass.verification {
        for check in &v.speakers {
            ui.label(
                RichText::new(format!(
                    "{} measured again: {} dB from the predicted sum.",
                    check.output_id,
                    super::tenths(check.deviation_db)
                ))
                .small()
                .color(if check.deviation_db <= 1.5 {
                    super::ACCENT
                } else {
                    super::WARM
                }),
            );
        }
    }
    ui.add_space(8.0);
}

/// The design's curves of one speaker, levelled at 500 Hz–2 kHz: measured (spatial
/// average), target, predicted, the correction, and measured again when confirmed.
fn plot(
    ui: &mut egui::Ui,
    curves: &Curves,
    summary: &companion_service::room_correction::OutputSummary,
    measured: Option<&[f32]>,
) {
    let f = &curves.frequency_hz;
    let (Some(&lo), Some(&hi)) = (f.first(), f.last()) else {
        return;
    };
    if hi <= lo {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(160.0), 190.0),
        egui::Sense::hover(),
    );
    let area = rect.shrink2(Vec2::new(30.0, 16.0));
    let painter = ui.painter_at(rect);
    let (low_db, high_db) = (-18.0_f32, 12.0_f32);
    let x = |hz: f64| area.left() + ((hz / lo).ln() / (hi / lo).ln()) as f32 * area.width();
    let y = |db: f32| {
        area.bottom() - ((db.clamp(low_db, high_db) - low_db) / (high_db - low_db)) * area.height()
    };
    // The correction's band, and no boost below the speaker's −6 dB point.
    if let Some([a, b]) = summary.band_hz {
        let band = Rect::from_x_y_ranges(x(a.max(lo))..=x(b.min(hi)), area.y_range());
        painter.rect_filled(band, 0.0, super::ACCENT.gamma_multiply(0.05));
    }
    if let Some(low) = summary.low_limit_hz.filter(|l| (lo..=hi).contains(l)) {
        painter.line_segment(
            [
                Pos2::new(x(low), area.top()),
                Pos2::new(x(low), area.bottom()),
            ],
            Stroke::new(1.0_f32, super::WARM.gamma_multiply(0.5)),
        );
    }
    let font = FontId::proportional(10.0);
    for db in [-12.0_f32, -6.0, 0.0, 6.0] {
        painter.line_segment(
            [
                Pos2::new(area.left(), y(db)),
                Pos2::new(area.right(), y(db)),
            ],
            Stroke::new(
                if db == 0.0 { 0.8_f32 } else { 0.4 },
                super::MUTED.gamma_multiply(0.35),
            ),
        );
        painter.text(
            Pos2::new(area.left() - 4.0, y(db)),
            Align2::RIGHT_CENTER,
            format!("{db:+.0}"),
            font.clone(),
            super::MUTED,
        );
    }
    for hz_mark in [100.0, 1000.0, 10000.0] {
        if (lo..=hi).contains(&hz_mark) {
            painter.text(
                Pos2::new(x(hz_mark), area.bottom() + 2.0),
                Align2::CENTER_TOP,
                hz(hz_mark),
                font.clone(),
                super::MUTED,
            );
        }
    }
    let line = |values: &[f32], color: Color32, width: f32| {
        let points: Vec<Pos2> = f
            .iter()
            .zip(values)
            .filter(|(_, v)| v.is_finite())
            .map(|(&hz, &v)| Pos2::new(x(hz), y(v)))
            .collect();
        if points.len() > 1 {
            painter.add(egui::Shape::line(points, Stroke::new(width, color)));
        }
    };
    line(&curves.target_db, super::MUTED.gamma_multiply(0.7), 1.0);
    line(&curves.measured_db, super::MUTED, 1.2);
    line(&curves.filter_db, super::WARM, 1.2);
    line(&curves.predicted_db, super::ACCENT, 1.6);
    if let Some(m) = measured {
        line(m, super::TEXT, 1.2);
    }
    ui.horizontal_wrapped(|ui| {
        for (label, color) in [
            ("Measured (average)", super::MUTED),
            ("Correction", super::WARM),
            ("Predicted", super::ACCENT),
        ]
        .into_iter()
        .chain(measured.map(|_| ("Measured again", super::TEXT)))
        {
            let (swatch, _) = ui.allocate_exact_size(Vec2::new(16.0, 10.0), egui::Sense::hover());
            ui.painter().line_segment(
                [swatch.left_center(), swatch.right_center()],
                Stroke::new(2.0_f32, color),
            );
            ui.label(RichText::new(label).small().color(super::MUTED));
        }
    });
}
