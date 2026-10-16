//! Native presentation only. Workers and measurement ownership live in companion-service.
use companion_service::{
    controller::{Controller, Job, Take},
    measurement::MeasureProgress,
    review::ImpulseReport,
};
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use std::time::Duration;

const BG: Color32 = Color32::from_rgb(15, 20, 27);
const PANEL: Color32 = Color32::from_rgb(22, 29, 38);
const CARD: Color32 = Color32::from_rgb(29, 38, 49);
const TEXT: Color32 = Color32::from_rgb(233, 240, 247);
const MUTED: Color32 = Color32::from_rgb(151, 167, 184);
const ACCENT: Color32 = Color32::from_rgb(113, 219, 201);
const WARM: Color32 = Color32::from_rgb(243, 194, 123);
const RED: Color32 = Color32::from_rgb(248, 136, 139);

pub struct CompanionApp {
    pub controller: Controller,
    endpoint: String,
    closing: bool,
    selected_result: Option<Take>,
    preview: bool,
    review_path: String,
}

impl CompanionApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_controller(&cc.egui_ctx, Controller::default(), true)
    }
    /// Inject a service for deterministic UI tests; the live app never installs fake devices.
    pub fn with_controller(
        ctx: &egui::Context,
        mut controller: Controller,
        refresh_inputs: bool,
    ) -> Self {
        configure_style(ctx);
        if refresh_inputs {
            controller.refresh_inputs();
        }
        Self {
            controller,
            endpoint: "127.0.0.1:9000".into(),
            closing: false,
            selected_result: None,
            preview: false,
            review_path: String::new(),
        }
    }

    /// Label a fixture-driven window explicitly; production uses LiveBackend.
    pub fn preview(ctx: &egui::Context, controller: Controller) -> Self {
        let mut app = Self::with_controller(ctx, controller, false);
        app.preview = true;
        ctx.style_mut(|style| style.animation_time = 0.0);
        app
    }

    pub fn draw(&mut self, ctx: &egui::Context) {
        self.controller.poll();
        if ctx.input(|i| i.viewport().close_requested()) && self.controller.measuring() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            self.controller.cancel();
        }
        if self.closing && !self.controller.measuring() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(if self.controller.busy() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(300)
        });

        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(42.0, 42.0), egui::Sense::hover());
                    for (i, height) in [12.0, 25.0, 36.0, 25.0, 12.0].iter().enumerate() {
                        let x = rect.left() + 5.0 + i as f32 * 7.0;
                        ui.painter().line_segment(
                            [
                                egui::pos2(x, rect.center().y - height / 2.0),
                                egui::pos2(x, rect.center().y + height / 2.0),
                            ],
                            Stroke::new(3.0_f32, ACCENT),
                        );
                    }
                    ui.add_space(9.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("OMNIPHONY  /  COMPANION")
                                .size(11.0)
                                .color(ACCENT)
                                .strong(),
                        );
                        ui.label(RichText::new("Room measurements").size(27.0).strong());
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        badge(
                            ui,
                            if self.preview {
                                "UI PREVIEW · NO AUDIO"
                            } else {
                                "ACQUISITION PREVIEW"
                            },
                            WARM,
                        );
                    });
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Renderer").color(MUTED));
                    let changed = ui
                        .add_enabled(
                            !self.controller.busy(),
                            egui::TextEdit::singleline(&mut self.endpoint)
                                .desired_width(163.0)
                                .font(egui::TextStyle::Monospace),
                        )
                        .changed();
                    if changed {
                        self.controller.disconnect();
                    }
                    let label = if matches!(self.controller.state.job, Some(Job::Discover)) {
                        "Connecting…"
                    } else if self.controller.state.snapshot.is_some() {
                        "Refresh speakers"
                    } else {
                        "Connect"
                    };
                    if ui
                        .add_enabled(!self.controller.busy(), egui::Button::new(label))
                        .clicked()
                    {
                        self.controller.connect(&self.endpoint);
                    }
                    if let Some(snapshot) = &self.controller.state.snapshot {
                        ui.add_space(8.0);
                        badge(ui, "CONNECTED", ACCENT);
                        ui.label(
                            RichText::new(format!(
                                "{:.1} kHz · {} speakers",
                                snapshot.sample_rate as f32 / 1000.0,
                                snapshot.outputs.len()
                            ))
                            .small()
                            .color(MUTED),
                        );
                    } else {
                        ui.label(RichText::new("Disconnected").small().color(MUTED));
                    }
                });
                if let Some(error) = &self.controller.state.connection_error {
                    ui.label(RichText::new(error).small().color(RED));
                }
            });
        egui::TopBottomPanel::bottom("footer")
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(24, 10)))
            .show(ctx, |ui| {
                ui.label(RichText::new("Capture and timing diagnostics · No speaker settings are changed · Automatic correction comes later")
                    .size(12.0).color(MUTED));
            });
        egui::SidePanel::left("setup")
            .resizable(false)
            .exact_width(310.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(20, 20)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("setup_scroll")
                    .show(ui, |ui| {
                        self.setup(ui);
                    });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| self.speakers(ui));
        self.result_window(ctx);
        self.review_window(ctx);
    }

    fn setup(&mut self, ui: &mut egui::Ui) {
        section(ui, "01", "Microphone");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            caption(ui, "Microphone / capture source");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.controller.busy(),
                        egui::Button::new("Refresh").small(),
                    )
                    .clicked()
                {
                    self.controller.refresh_inputs();
                }
            });
        });
        if matches!(self.controller.state.job, Some(Job::Inputs)) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Finding inputs…").small().color(MUTED));
            });
        }
        let busy = self.controller.busy();
        ui.add_enabled_ui(!busy, |ui| {
            let selected = self
                .controller
                .selected_input()
                .map(|input| input.label())
                .unwrap_or_else(|| "Choose an input…".into());
            egui::ComboBox::from_id_salt("device")
                .width(ui.available_width())
                .truncate()
                .selected_text(truncate(&selected, 31))
                .show_ui(ui, |ui| {
                    for device in &self.controller.state.inputs {
                        let usable = !device.configurations.is_empty();
                        ui.add_enabled_ui(usable, |ui| {
                            if ui
                                .selectable_label(
                                    self.controller.settings.device.as_ref() == Some(&device.name),
                                    device.label(),
                                )
                                .on_hover_text(&device.name)
                                .clicked()
                            {
                                self.controller.settings.device = Some(device.name.clone());
                                self.controller.settings.input_channel = 1;
                            }
                        });
                    }
                })
                .response
                .on_hover_text(&selected);
            if self.controller.selected_input().is_some_and(|input| input.is_monitor) {
                ui.label(RichText::new("Sink monitor: records a digital output. Choose a microphone source to measure sound in the room.").small().color(WARM));
            }
            if self.controller.state.inputs.is_empty()
                && self.controller.state.input_error.is_none()
                && !busy
            {
                ui.label(
                    RichText::new("No input found. Connect your interface, then refresh.")
                        .small()
                        .color(MUTED),
                );
            }
            let channels = self.controller.input_channels();
            ui.add_space(10.0);
            ui.columns(2, |columns| {
                caption(&mut columns[0], "Input channel");
                egui::ComboBox::from_id_salt("channel")
                    .width(110.0)
                    .truncate()
                    .selected_text(self.controller.selected_input().map_or_else(
                        || format!("Channel {}", self.controller.settings.input_channel),
                        |input| input.channel_label(self.controller.settings.input_channel)))
                    .show_ui(&mut columns[0], |ui| {
                        for channel in 1..=channels {
                            let label = self.controller.selected_input().map_or_else(
                                || format!("Channel {channel}"), |input| input.channel_label(channel));
                            ui.selectable_value(
                                &mut self.controller.settings.input_channel,
                                channel,
                                label,
                            );
                        }
                    });
                caption(&mut columns[1], "Sample rate");
                egui::ComboBox::from_id_salt("sample_rate")
                    .width(110.0)
                    .selected_text(format!("{} Hz", self.controller.settings.sample_rate))
                    .show_ui(&mut columns[1], |ui| {
                        for rate in [
                            8000, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000,
                        ] {
                            let supported = self.controller.selected_input().is_some_and(|input| {
                                input.configurations.iter().any(|c| {
                                    c.min_sample_rate <= rate
                                        && c.max_sample_rate >= rate
                                        && matches!(c.sample_format.as_str(), "f32" | "i16" | "i32")
                                })
                            });
                            ui.add_enabled_ui(supported, |ui| {
                                ui.selectable_value(
                                    &mut self.controller.settings.sample_rate,
                                    rate,
                                    format!("{rate} Hz"),
                                );
                            });
                        }
                    });
            });
            ui.add_space(10.0);
            caption(ui, "Microphone name");
            ui.add(
                egui::TextEdit::singleline(&mut self.controller.settings.microphone_id)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            caption(ui, "Listening position");
            ui.add(
                egui::TextEdit::singleline(&mut self.controller.settings.position_id)
                    .desired_width(f32::INFINITY),
            );
        });
        if let Some(error) = &self.controller.state.input_error {
            ui.label(RichText::new(error).small().color(RED));
        }
        ui.add_space(23.0);
        section(ui, "02", "Timing reference");
        ui.add_space(10.0);
        ui.add_enabled_ui(!busy, |ui| {
            let selected = self
                .controller
                .settings
                .reference_output
                .clone()
                .unwrap_or_else(|| "Connect to load speakers…".into());
            egui::ComboBox::from_id_salt("reference")
                .width(ui.available_width())
                .selected_text(truncate(&selected, 31))
                .show_ui(ui, |ui| {
                    if let Some(snapshot) = &self.controller.state.snapshot {
                        for output in &snapshot.outputs {
                            ui.add_enabled_ui(!output.muted, |ui| {
                                ui.selectable_value(
                                    &mut self.controller.settings.reference_output,
                                    Some(output.id.clone()),
                                    &output.name,
                                );
                            });
                        }
                    }
                });
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Keep this speaker and the microphone position fixed across measurements.",
            )
            .small()
            .color(MUTED),
        );
        ui.add_space(23.0);
        section(ui, "03", "Recordings");
        ui.add_space(10.0);
        ui.add_enabled_ui(!busy, |ui| {
            caption(ui, "Save folder");
            ui.add(
                egui::TextEdit::singleline(&mut self.controller.settings.output_root)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("Each take saves a WAV and its measurement report in a new folder.")
                    .small()
                    .color(MUTED),
            );
            ui.add_space(14.0);
            egui::CollapsingHeader::new("Measurement settings")
                .id_salt("advanced")
                .show(ui, |ui| {
                    caption(ui, "Sweep duration");
                    ui.add(
                        egui::DragValue::new(&mut self.controller.settings.sweep.duration_ms)
                            .range(100..=5000)
                            .suffix(" ms"),
                    );
                    caption(ui, "Playback level");
                    ui.add(
                        egui::DragValue::new(&mut self.controller.settings.sweep.peak_dbfs)
                            .range(-60.0..=-12.0)
                            .speed(0.5)
                            .suffix(" dBFS"),
                    );
                    caption(ui, "Frequency range");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.controller.settings.sweep.start_hz)
                                .range(20.0..=80000.0)
                                .speed(10.0)
                                .suffix(" Hz"),
                        );
                        ui.label("to");
                        ui.add(
                            egui::DragValue::new(&mut self.controller.settings.sweep.end_hz)
                                .range(20.0..=86000.0)
                                .speed(50.0)
                                .suffix(" Hz"),
                        );
                    });
                    caption(ui, "Calibration file (optional)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.controller.settings.calibration_path)
                            .desired_width(f32::INFINITY)
                            .hint_text("Path to microphone calibration"),
                    );
                    ui.label(
                        RichText::new(
                            "Stored with the take; frequency corrections are not applied yet.",
                        )
                        .small()
                        .color(MUTED),
                    );
                });
        });
    }

    fn speakers(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Review a saved take · no playback").show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.review_path)
                    .desired_width(ui.available_width())
                    .hint_text("Path to a saved take folder"),
            );
            if ui
                .add_enabled(
                    !self.controller.busy(),
                    egui::Button::new("Analyze saved take"),
                )
                .clicked()
            {
                self.controller.review_saved(&self.review_path);
            }
            ui.label(
                RichText::new("Creates a new analysis report. The original take is preserved.")
                    .small()
                    .color(MUTED),
            );
        });
        if matches!(self.controller.state.job, Some(Job::Review)) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Analyzing recording…");
            });
        }
        if let Some(error) = &self.controller.state.review_error {
            ui.label(RichText::new(error).color(WARM));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Speakers").size(23.0).strong());
            if let Some(snapshot) = &self.controller.state.snapshot {
                ui.add_space(4.0);
                badge(ui, &format!("{}", snapshot.outputs.len()), MUTED);
            }
        });
        ui.label(
            RichText::new(
                "Measure individually or capture all checked speakers with shared references.",
            )
            .color(MUTED),
        );
        ui.label(
            RichText::new(
                "Choose the speaker playback sink in Studio / Audio Output / Output device.",
            )
            .small()
            .color(MUTED),
        );
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            let readiness = self.controller.batch_outputs();
            if ui
                .add_enabled(
                    readiness.is_ok(),
                    egui::Button::new("Measure all checked").min_size(Vec2::new(155.0, 30.0)),
                )
                .on_disabled_hover_text(readiness.as_ref().err().map(String::as_str).unwrap_or(""))
                .clicked()
            {
                self.controller.start_batch();
            }
            if ui
                .add_enabled(
                    self.controller.measuring() && !self.controller.state.stopping,
                    egui::Button::new(RichText::new("Stop").color(RED))
                        .min_size(Vec2::new(60.0, 30.0)),
                )
                .clicked()
            {
                self.controller.cancel();
            }
            let text = match &self.controller.state.job {
                Some(Job::Batch {
                    output_ids,
                    current,
                }) => format!("Speaker {} / {}", current + 1, output_ids.len()),
                _ => readiness
                    .as_ref()
                    .map(|o| format!("{} selected", o.len()))
                    .unwrap_or_default(),
            };
            ui.add(egui::Label::new(RichText::new(text).small().color(MUTED)).truncate());
        });
        ui.add_space(12.0);
        if let Some(error) = &self.controller.state.error {
            card().show(ui, |ui| {
                ui.label(RichText::new(error).color(RED));
            });
            ui.add_space(10.0);
        }
        if let Some(reason) = self
            .controller
            .state
            .snapshot
            .as_ref()
            .and_then(|s| s.unavailable_reason.as_ref())
        {
            card().show(ui, |ui| {
                ui.label(
                    RichText::new("Measurement unavailable")
                        .strong()
                        .color(WARM),
                );
                ui.label(reason);
            });
            ui.add_space(10.0);
        }
        let outputs = self
            .controller
            .state
            .snapshot
            .as_ref()
            .map(|s| s.outputs.clone());
        egui::ScrollArea::vertical().id_salt("speakers_scroll").auto_shrink([false, false]).show(ui, |ui| {
            match outputs {
                None => {
                    card().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("Your speakers will appear here").size(20.0).strong());
                            ui.add_space(10.0);
                            ui.label(RichText::new("Start Omniphony, then connect using its local address above.").color(MUTED));
                            ui.label(RichText::new("Choose your microphone input while you get connected.").small().color(MUTED));
                        });
                        ui.add_space(30.0);
                    });
                }
                Some(outputs) if outputs.is_empty() => {
                    ui.label("Omniphony reported no physical outputs. Check its speaker layout, then refresh.");
                }
                Some(outputs) => {
                    for output in outputs {
                        let readiness = self.controller.can_measure(&output.id);
                        let take = self.controller.current_take(&output.id).cloned();
                        let active = self.controller.active_output() == Some(output.id.as_str());
                        card().show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let mut included=!self.controller.excluded_outputs.contains(&output.id) && self.controller.output_readiness(&output.id).is_ok();
                                if ui.add_enabled(!self.controller.busy() && readiness.is_ok(),egui::Checkbox::without_text(&mut included)).on_hover_text("Include in whole-system capture. Uncheck unwired speakers.").changed() {
                                    if included {self.controller.excluded_outputs.remove(&output.id);} else {self.controller.excluded_outputs.insert(output.id.clone());}
                                }
                                let (icon, _) = ui.allocate_exact_size(Vec2::new(24.0, 44.0), egui::Sense::hover());
                                let ink = if output.muted { MUTED } else { ACCENT };
                                ui.painter().rect_stroke(icon.shrink2(Vec2::new(6.0, 4.0)), 4.0, Stroke::new(1.3_f32, ink), egui::StrokeKind::Inside);
                                ui.painter().circle_stroke(icon.center()+Vec2::new(0.0, 6.0), 5.0, Stroke::new(1.3_f32, ink));
                                ui.painter().circle_filled(icon.center()+Vec2::new(0.0,-8.0), 2.0, ink);
                                let name_width = (ui.available_width()-272.0).max(100.0);
                                ui.allocate_ui_with_layout(Vec2::new(name_width, 48.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.set_min_width(name_width);
                                    ui.add(egui::Label::new(RichText::new(&output.name).size(18.0).strong()).truncate()).on_hover_text(&output.name);
                                    let band = match (output.freq_low_hz, output.freq_high_hz) {
                                        (Some(lo), Some(hi)) => format!("{lo:.0}–{hi:.0} Hz"),
                                        (Some(lo), None) => format!("From {lo:.0} Hz"),
                                        (None, Some(hi)) => format!("Up to {hi:.0} Hz"),
                                        _ => "No band restriction".into(),
                                    };
                                    ui.label(RichText::new(format!("Output {} · {band}", output.index + 1)).size(12.0).color(MUTED));
                                });
                                ui.allocate_ui_with_layout(Vec2::new(72.0, 45.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.set_min_width(72.0);
                                    caption(ui, "GAIN");
                                    ui.label(RichText::new(format!("{:+.1} dB", output.gain_db)).monospace());
                                });
                                ui.allocate_ui_with_layout(Vec2::new(77.0, 45.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.set_min_width(77.0);
                                    caption(ui, "DELAY");
                                    ui.label(RichText::new(format!("{:.2} ms", output.delay_ms)).monospace());
                                });
                                let button = ui.add_enabled(if active { !self.controller.state.stopping } else { readiness.is_ok() }, egui::Button::new(
                                    RichText::new(if active { "Stop" } else { "Measure" }).color(if readiness.is_ok() { BG } else { MUTED }))
                                    .fill(if readiness.is_ok() { ACCENT } else { PANEL })
                                    .min_size(Vec2::new(92.0, 34.0)));
                                if button.clicked() { if active {self.controller.cancel();} else { self.controller.start(&output.id); } }
                                if let Err(reason) = &readiness { button.on_disabled_hover_text(reason); }
                            });
                            ui.add_space(5.0);
                            let (status_rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 25.0), egui::Sense::hover());
                            ui.scope_builder(egui::UiBuilder::new().max_rect(status_rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                                if output.muted { badge(ui, "MUTED", WARM); }
                                if self.controller.settings.reference_output.as_ref() == Some(&output.id) {
                                    badge(ui, "TIMING REFERENCE", ACCENT);
                                }
                                if active { self.row_progress(ui); }
                                else if let Some(take) = &take {
                                    let (text, color) = take_status(take);
                                    ui.add(egui::Label::new(RichText::new(text).small().color(color)).truncate());
                                    if ui.small_button("View result").clicked() {
                                        self.selected_result = Some(take.clone());
                                    }
                                } else if let Err(reason) = &readiness {
                                    ui.add(egui::Label::new(RichText::new(reason).small().color(MUTED)).truncate()).on_hover_text(reason);
                                } else { ui.label(RichText::new("Ready for a first take").small().color(MUTED)); }
                            });
                        });
                        ui.add_space(10.0);
                    }
                }
            }
            if !self.controller.state.takes.is_empty() {
                ui.add_space(8.0);
                egui::CollapsingHeader::new(format!("Recent takes ({})", self.controller.state.takes.len())).show(ui, |ui| {
                    for take in self.controller.state.takes.iter().rev() {
                        let (status, color) = take_status(take);
                        if ui.button(RichText::new(format!("{} · {status}", take.output_id)).color(color)).clicked() {
                            self.selected_result = Some(take.clone());
                        }
                    }
                });
            }
        });
    }

    fn row_progress(&self, ui: &mut egui::Ui) {
        let (phase, fraction) = match &self.controller.state.progress {
            Some(MeasureProgress::Preparing) => ("Checking configuration", 0.0),
            Some(MeasureProgress::OpeningInput) => ("Opening microphone", 0.0),
            Some(MeasureProgress::LeadIn) => ("Quiet lead-in", 0.0),
            Some(MeasureProgress::Playing(p)) => {
                if p.sequence_outputs.is_empty() {
                    (
                        "Reference / speaker / reference",
                        p.rendered_frames as f32 / p.total_frames.max(1) as f32,
                    )
                } else {
                    let count = p.sequence_outputs.len();
                    let segment = p.total_frames / (count as u64 * 2 + 1);
                    let index = companion_service::batch::active_target(p).min(count - 1);
                    let local = p.rendered_frames.saturating_sub(index as u64 * 2 * segment);
                    (
                        if (p.rendered_frames / segment.max(1)).is_multiple_of(2) {
                            "Reference sweep"
                        } else {
                            "Speaker sweep"
                        },
                        (local as f32 / (segment * 3).max(1) as f32).clamp(0.0, 1.0),
                    )
                }
            }
            Some(MeasureProgress::Tail) => ("Acoustic tail", 1.0),
            Some(MeasureProgress::Saving) => ("Saving recording", 1.0),
            Some(MeasureProgress::Analyzing) => ("Analyzing recording", 1.0),
            None => ("Preparing", 0.0),
        };
        let label = if self.controller.state.stopping {
            "Stopping and saving…"
        } else {
            phase
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(ui.available_width())
                .desired_height(20.0)
                .text(label)
                .fill(ACCENT.gamma_multiply(0.45)),
        );
    }

    fn result_window(&mut self, ctx: &egui::Context) {
        let Some(take) = &self.selected_result else {
            return;
        };
        let mut open = true;
        let mut review_requested = None;
        egui::Window::new(format!("Measurement · {}", take.output_id)).open(&mut open).default_width(600.0)
            .vscroll(true).default_height(600.0).max_height(600.0).resizable(true).show(ctx, |ui| {
                let (status, color) = take_status(take);
                ui.label(RichText::new(status).size(20.0).color(color).strong());
                ui.add_space(10.0);
                ui.label(format!("Microphone: {} · Channel {}", take.settings.microphone_id, take.settings.input_channel));
                ui.label(format!("Position: {}", take.settings.position_id));
                ui.label(format!("Reference: {}", take.settings.reference_output.as_deref().unwrap_or("—")));
                match &take.outcome {
                    Ok(outcome) => {
                        if take.settings.device.as_deref().is_some_and(|s| s.starts_with("pipewire:monitor:")) { ui.label(RichText::new("Sink monitor · not an acoustic microphone measurement").color(WARM)); }
                        if let Some(report) = &outcome.diagnostics { draw_diagnostics(ui, report); }
                        if let Some(impulse) = &outcome.impulse { draw_impulse(ui, impulse); }
                        if let Some(error) = &outcome.impulse_error { ui.label(RichText::new(error).color(WARM)); }
                        if let Some(timing) = &outcome.timing {
                            ui.add_space(12.0);
                            egui::Grid::new("timing_values").spacing([32.0, 10.0]).show(ui, |ui| {
                                if outcome.impulse.is_none() {
                                ui.label("Relative peak delay"); ui.monospace(timing.relative_peak_delay_ms.map_or_else(|| "Unresolved".into(), |v| format!("{v:+.3} ms"))); ui.end_row();
                                ui.label("Clock drift"); ui.monospace(timing.clock_drift_ppm.map_or_else(|| "Unresolved".into(), |v| format!("{v:+.1} ppm"))); ui.end_row();
                                }
                                ui.label("Speaker signal / noise"); ui.monospace(format!("{:.1} dB", timing.signal_to_noise_db[1])); ui.end_row();
                                ui.label("Speaker sweep RMS"); ui.monospace(format!("{:.1} dBFS", timing.sweep_rms_dbfs[1])); ui.end_row();
                                ui.label("Background RMS"); ui.monospace(format!("{:.1} dBFS", timing.noise_rms_dbfs)); ui.end_row();
                            });
                            if let Some(warning) = timing.timing_warning().filter(|_| outcome.impulse.is_none()) {
                                ui.add_space(10.0);
                                ui.label(RichText::new(warning).color(WARM));
                            }
                        }
                        if let Some(reason) = &outcome.rejection_reason { ui.label(RichText::new(reason).color(WARM)); }
                        ui.add_space(14.0);
                        caption(ui, "Saved take");
                        let path = outcome.directory.display().to_string();
                        ui.label(RichText::new(&path).monospace().small());
                        if ui.button("Copy folder path").clicked() { ui.ctx().copy_text(path.clone()); }
                        if ui.add_enabled(!self.controller.busy(), egui::Button::new("Reanalyze this take")).clicked() { review_requested = Some(path); }
                    }
                    Err(error) => { ui.label(RichText::new(error).color(RED)); }
                }
                ui.add_space(14.0);
                ui.label(RichText::new("Arrival candidates are experimental. An early peak is not proof of the direct path. Automatic gain/delay correction remains disabled.").small().color(MUTED));
            });
        if let Some(path) = review_requested {
            self.review_path = path;
            self.controller.review_saved(&self.review_path);
            open = false;
        }
        if !open {
            self.selected_result = None;
        }
    }
    fn review_window(&mut self, ctx: &egui::Context) {
        let Some(review) = &self.controller.state.review else {
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Saved take · {}", review.output_id))
            .id(egui::Id::new("saved_take_review"))
            .open(&mut open)
            .default_width(620.0)
            .default_height(650.0)
            .max_height(650.0)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Offline measurement review")
                        .size(20.0)
                        .strong(),
                );
                ui.label(format!(
                    "{} · {} · Channel {}",
                    review.microphone_id, review.position_id, review.input_channel
                ));
                ui.label(format!(
                    "Speaker: {} · Reference: {}",
                    review.output_id, review.reference_output_id
                ));
                if review.origin == "sink_monitor_capture" {
                    ui.label(
                        RichText::new("Sink monitor · not an acoustic microphone measurement")
                            .color(WARM),
                    );
                }
                if let Some(reason) = &review.original_rejection_reason {
                    ui.label(
                        RichText::new(format!("Original take rejected: {reason}")).color(WARM),
                    );
                }
                draw_diagnostics(ui, &review.diagnostics);
                if let Some(error) = &review.analysis_error {
                    ui.label(RichText::new(error).color(WARM));
                }
                if let Some(impulse) = &review.impulse {
                    draw_impulse(ui, impulse);
                }
                if let Some(error) = &review.impulse_error {
                    ui.label(RichText::new(error).color(WARM));
                }
                ui.separator();

                ui.label(RichText::new("New analysis report").small().color(MUTED));
                ui.label(
                    RichText::new(review.report_file.display().to_string())
                        .small()
                        .monospace(),
                );
                if ui.button("Copy analysis path").clicked() {
                    ui.ctx().copy_text(review.report_file.display().to_string());
                }
                ui.label(
                    RichText::new(
                        "Original files preserved · No audio played · No correction applied",
                    )
                    .small()
                    .color(MUTED),
                );
            });
        if !open {
            self.controller.state.review = None;
        }
    }
}

impl eframe::App for CompanionApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.draw(ctx);
    }
}
fn configure_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = PANEL;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = BG;
    style.visuals.faint_bg_color = CARD;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.3);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.inactive.bg_fill = CARD;
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(46, 63, 76);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(43, 83, 80);
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.interact_size.y = 31.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(12.0));
    ctx.set_style(style);
}
fn section(ui: &mut egui::Ui, number: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).size(12.0).monospace().color(ACCENT));
        ui.add_space(3.0);
        ui.label(RichText::new(title).size(17.0).strong());
    });
}
fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(MUTED));
}
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.0).strong().color(color));
        });
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(8)
        .inner_margin(egui::Margin::same(14))
}
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.into()
    } else {
        format!("{}…", text.chars().take(limit - 1).collect::<String>())
    }
}
fn take_status(take: &Take) -> (&str, Color32) {
    match &take.outcome {
        Ok(outcome) if outcome.cancelled => ("Cancelled · recording retained", WARM),
        Ok(outcome)
            if outcome.accepted
                && outcome
                    .impulse
                    .as_ref()
                    .is_some_and(|r| r.relative_arrival_ms.is_some()) =>
        {
            ("Captured · arrival candidate", ACCENT)
        }
        Ok(outcome) if outcome.accepted && outcome.impulse.is_some() => {
            ("Captured · arrival needs review", WARM)
        }
        Ok(outcome)
            if outcome.accepted
                && outcome
                    .timing
                    .as_ref()
                    .is_some_and(|report| report.ambiguous_peaks) =>
        {
            ("Captured · timing unresolved", WARM)
        }
        Ok(outcome) if outcome.accepted => ("Recorded · diagnostics available", ACCENT),
        Ok(_) => ("Take rejected · review report", WARM),
        Err(_) => ("Measurement failed", RED),
    }
}

fn draw_diagnostics(ui: &mut egui::Ui, report: &companion_service::review::AcquisitionDiagnostics) {
    use companion_service::review::SegmentStatus;
    ui.add_space(10.0);
    let target = report.segments.as_ref().map(|s| s[1].status);
    let title = match target {
        Some(SegmentStatus::NoSweepDetected) => "No target sweep detected",
        Some(SegmentStatus::Clipped) => "Target interval clipped",
        _ => "Acquisition levels",
    };
    ui.label(RichText::new(title).strong());
    ui.label(format!(
        "Background: {:.1} dBFS · Analysis clipped samples: {}",
        report.noise_rms_dbfs, report.total_clipped_samples
    ));
    if let Some(segments) = &report.segments {
        egui::Grid::new("segment_diagnostics")
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                for label in ["Interval", "RMS", "Signal / noise", "Peak", "Clipped"] {
                    ui.label(RichText::new(label).small().color(MUTED));
                }
                ui.end_row();
                for segment in segments {
                    ui.label(segment.role);
                    ui.monospace(format!("{:.1}", segment.rms_dbfs));
                    ui.monospace(format!("{:.1} dB", segment.signal_to_noise_db));
                    ui.monospace(format!("{:.1}", segment.peak_dbfs));
                    ui.monospace(segment.clipped_samples.to_string());
                    ui.end_row();
                }
            });
        ui.label(
            RichText::new("RMS and peak in dBFS · Approximate windows from the reference pair")
                .small()
                .color(MUTED),
        );
        ui.label(format!("Speaker: {}", segments[1].status.label()));
    }
    for message in &report.messages {
        ui.label(RichText::new(message).color(WARM));
    }
    ui.label(
        RichText::new("These levels diagnose acquisition; they are not speaker gain corrections.")
            .small()
            .color(MUTED),
    );
    ui.separator();
}

fn draw_impulse(ui: &mut egui::Ui, report: &ImpulseReport) {
    ui.add_space(12.0);
    if let (Some(delay), Some(bound)) = (report.relative_arrival_ms, report.uncertainty_ms) {
        ui.label(
            RichText::new(format!("Arrival candidate  {delay:+.3} ms"))
                .size(20.0)
                .strong()
                .color(ACCENT),
        );
        ui.label(format!(
            "Sensitivity ±{bound:.3} ms · not a calibrated accuracy bound"
        ));
        if let Some(drift) = report.clock_drift_ppm {
            ui.label(format!("Reference clock estimate: {drift:+.1} ppm"));
        }
    } else {
        ui.label(
            RichText::new("Impulse response available · arrival unresolved")
                .strong()
                .color(WARM),
        );
        for reason in &report.unresolved_reasons {
            ui.label(RichText::new(reason).small().color(WARM));
        }
    }
    if let Some(similarity) = report.reference_similarity {
        ui.label(format!(
            "Repeated reference waveform agreement: {:.1}%",
            similarity * 100.0
        ));
    }
    ui.label(RichText::new("Experimental arrival evidence; direct path and automatic correction are not validated.").small().color(MUTED));
    let id = ui.id().with("impulse_segment");
    let mut selected: usize = ui.data_mut(|d| d.get_persisted(id)).unwrap_or(1);
    selected = selected.min(2);
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text(["Reference before", "Speaker", "Reference after"][selected])
            .show_ui(ui, |ui| {
                for (i, label) in ["Reference before", "Speaker", "Reference after"]
                    .iter()
                    .enumerate()
                {
                    ui.selectable_value(&mut selected, i, *label);
                }
            });
    });
    ui.data_mut(|d| d.insert_persisted(id, selected));
    let segment = &report.segments[selected];
    ui.label(
        RichText::new("Impulse envelope · dB relative to peak")
            .small()
            .color(MUTED),
    );
    let focus = segment
        .candidate_offset_ms
        .or(segment.band_candidate_offset_ms[0])
        .unwrap_or(0.0);
    let left_ms = focus - 12.0;
    let right_ms = focus + 35.0;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(100.0), 160.0),
        egui::Sense::hover(),
    );
    let plot = rect.shrink2(Vec2::new(30.0, 18.0));
    let painter = ui.painter_at(rect);
    let map = |ms: f64, db: f64| {
        egui::pos2(
            plot.left() + ((ms - left_ms) / (right_ms - left_ms)) as f32 * plot.width(),
            plot.bottom() - ((db.clamp(-80.0, 0.0) + 80.0) / 80.0) as f32 * plot.height(),
        )
    };
    for db in [-80.0, -40.0, 0.0] {
        let y = map(left_ms, db).y;
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            Stroke::new(1.0_f32, MUTED.gamma_multiply(0.2)),
        );
        painter.text(
            egui::pos2(plot.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{db:.0}"),
            FontId::proportional(10.0),
            MUTED,
        );
    }
    let points: Vec<_> = segment
        .envelope_db
        .iter()
        .enumerate()
        .filter_map(|(i, &db)| {
            let ms = segment.response_start_ms + (i as f64 + 0.5) * segment.envelope_step_ms;
            (ms >= left_ms && ms <= right_ms).then(|| map(ms, f64::from(db)))
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(1.3_f32, ACCENT)));
    // Mark each band's evidence even when disagreement prevents a final candidate.
    for (band, color) in [ACCENT, WARM, Color32::from_rgb(157, 171, 255)]
        .into_iter()
        .enumerate()
    {
        if let Some(ms) =
            segment.band_candidate_offset_ms[band].filter(|ms| *ms >= left_ms && *ms <= right_ms)
        {
            let x = map(ms, 0.0).x;
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                Stroke::new(1.0_f32, color),
            );
        }
    }
    for ms in [left_ms, focus, right_ms] {
        painter.text(
            egui::pos2(map(ms, -80.0).x, plot.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            format!("{ms:+.1}"),
            FontId::proportional(10.0),
            MUTED,
        );
    }
    if let Some(position) = response.hover_pos().filter(|p| plot.contains(*p)) {
        let ms =
            left_ms + f64::from((position.x - plot.left()) / plot.width()) * (right_ms - left_ms);
        response.on_hover_text(format!("{ms:+.3} ms relative to the correlation peak"));
    }
    ui.label(
        RichText::new("Time in ms relative to the correlation peak; markers show band candidates.")
            .small()
            .color(MUTED),
    );
    egui::Grid::new("arrival_bands")
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for (band, candidate) in report.bands_hz.iter().zip(segment.band_candidate_offset_ms) {
                ui.label(format!("{:.0}–{:.0} Hz", band[0], band[1]));
                ui.monospace(
                    candidate.map_or_else(|| "Unresolved".into(), |v| format!("{v:+.3} ms")),
                );
                ui.end_row();
            }
        });
    egui::CollapsingHeader::new("Threshold checks").show(ui, |ui| {
        egui::Grid::new("threshold_checks")
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                ui.label("Band");
                for label in ["20%", "30%", "40%"] {
                    ui.label(label);
                }
                ui.end_row();
                for (band, candidates) in report
                    .bands_hz
                    .iter()
                    .zip(segment.threshold_candidate_offset_ms)
                {
                    ui.label(format!("{:.0}–{:.0} Hz", band[0], band[1]));
                    for candidate in candidates {
                        ui.monospace(candidate.map_or_else(|| "—".into(), |v| format!("{v:+.3}")));
                    }
                    ui.end_row();
                }
            });
    });
    if let Some(spread) = segment.sensitivity_ms {
        ui.label(format!("Band / threshold sensitivity: {spread:.3} ms"));
    }
}
