//! Native presentation only. Workers and measurement ownership live in companion-service.
use companion_service::{
    controller::{Controller, Job, Take},
    measurement::MeasureProgress,
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
        }
    }

    /// Label a fixture-driven window explicitly; production uses LiveBackend.
    pub fn preview(ctx: &egui::Context, controller: Controller) -> Self {
        let mut app = Self::with_controller(ctx, controller, false);
        app.preview = true;
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
        ui.horizontal(|ui| {
            ui.label(RichText::new("Speakers").size(23.0).strong());
            if let Some(snapshot) = &self.controller.state.snapshot {
                ui.add_space(4.0);
                badge(ui, &format!("{}", snapshot.outputs.len()), MUTED);
            }
        });
        ui.label(
            RichText::new("Measure one speaker at a time from your listening position.")
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
        if self.controller.measuring() {
            self.progress(ui);
            ui.add_space(12.0);
        }
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
                        let active = matches!(&self.controller.state.job, Some(Job::Measure { output_id }) if output_id == &output.id);
                        card().show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let (icon, _) = ui.allocate_exact_size(Vec2::new(33.0, 44.0), egui::Sense::hover());
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
                                let button = ui.add_enabled(readiness.is_ok(), egui::Button::new(
                                    RichText::new(if active { "Recording…" } else { "Measure" }).color(if readiness.is_ok() { BG } else { MUTED }))
                                    .fill(if readiness.is_ok() { ACCENT } else { PANEL })
                                    .min_size(Vec2::new(92.0, 34.0)));
                                if button.clicked() { self.controller.start(&output.id); }
                                if let Err(reason) = &readiness { button.on_disabled_hover_text(reason); }
                            });
                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                if output.muted { badge(ui, "MUTED", WARM); }
                                if self.controller.settings.reference_output.as_ref() == Some(&output.id) {
                                    badge(ui, "TIMING REFERENCE", ACCENT);
                                }
                                if active { ui.spinner(); ui.label(RichText::new("Measurement in progress").small().color(ACCENT)); }
                                else if let Some(take) = &take {
                                    let (text, color) = take_status(take);
                                    ui.label(RichText::new(text).small().color(color));
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

    fn progress(&mut self, ui: &mut egui::Ui) {
        card()
            .stroke(Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.4)))
            .show(ui, |ui| {
                let output = match &self.controller.state.job {
                    Some(Job::Measure { output_id }) => output_id.clone(),
                    _ => String::new(),
                };
                let (phase, fraction) = match &self.controller.state.progress {
                    Some(MeasureProgress::Preparing) => {
                        ("Checking the renderer configuration", 0.0)
                    }
                    Some(MeasureProgress::OpeningInput) => ("Opening microphone input", 0.0),
                    Some(MeasureProgress::LeadIn) => ("Recording a quiet lead-in", 0.0),
                    Some(MeasureProgress::Playing(p)) => (
                        "Playing reference, speaker, then reference",
                        p.rendered_frames as f32 / p.total_frames.max(1) as f32,
                    ),
                    Some(MeasureProgress::Tail) => ("Recording the acoustic tail", 1.0),
                    Some(MeasureProgress::Saving) => ("Saving the recording", 1.0),
                    Some(MeasureProgress::Analyzing) => ("Checking quality and timing", 1.0),
                    None => ("Preparing measurement", 0.0),
                };
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("Measuring {output}")).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.controller.state.stopping,
                                egui::Button::new(RichText::new("Stop measurement").color(RED)),
                            )
                            .clicked()
                        {
                            self.controller.cancel();
                        }
                    });
                });
                ui.label(
                    RichText::new(if self.closing {
                        "Stopping and saving the take before closing…"
                    } else if self.controller.state.stopping {
                        "Stopping and saving the take…"
                    } else {
                        phase
                    })
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .fill(ACCENT)
                        .desired_width(ui.available_width()),
                );
            });
    }

    fn result_window(&mut self, ctx: &egui::Context) {
        let Some(take) = &self.selected_result else {
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Measurement · {}", take.output_id)).open(&mut open).default_width(490.0)
            .resizable(true).show(ctx, |ui| {
                let (status, color) = take_status(take);
                ui.label(RichText::new(status).size(20.0).color(color).strong());
                ui.add_space(10.0);
                ui.label(format!("Microphone: {} · Channel {}", take.settings.microphone_id, take.settings.input_channel));
                ui.label(format!("Position: {}", take.settings.position_id));
                ui.label(format!("Reference: {}", take.settings.reference_output.as_deref().unwrap_or("—")));
                match &take.outcome {
                    Ok(outcome) => {
                        if let Some(timing) = &outcome.timing {
                            ui.add_space(12.0);
                            egui::Grid::new("timing_values").spacing([32.0, 10.0]).show(ui, |ui| {
                                ui.label("Relative peak delay"); ui.monospace(format!("{:+.3} ms", timing.relative_peak_delay_ms)); ui.end_row();
                                ui.label("Clock drift"); ui.monospace(format!("{:+.1} ppm", timing.clock_drift_ppm)); ui.end_row();
                                ui.label("Speaker signal / noise"); ui.monospace(format!("{:.1} dB", timing.signal_to_noise_db[1])); ui.end_row();
                            });
                        }
                        if let Some(reason) = &outcome.rejection_reason { ui.label(RichText::new(reason).color(WARM)); }
                        ui.add_space(14.0);
                        caption(ui, "Saved take");
                        let path = outcome.directory.display().to_string();
                        ui.label(RichText::new(&path).monospace().small());
                        if ui.button("Copy folder path").clicked() { ui.ctx().copy_text(path); }
                    }
                    Err(error) => { ui.label(RichText::new(error).color(RED)); }
                }
                ui.add_space(14.0);
                ui.label(RichText::new("These are acquisition diagnostics. Direct-arrival analysis and automatic gain/delay correction are not enabled.").small().color(MUTED));
            });
        if !open {
            self.selected_result = None;
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
        Ok(outcome) if outcome.accepted => ("Recorded · diagnostics available", ACCENT),
        Ok(_) => ("Take rejected · review report", WARM),
        Err(_) => ("Measurement failed", RED),
    }
}
