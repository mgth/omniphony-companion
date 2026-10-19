//! Native presentation only. Workers and measurement ownership live in companion-service.
mod alignment_view;
mod manual_level_view;
mod microphone_view;
mod quality_view;
mod room_map;
mod spectral_plot;
mod waveform_plot;
use companion_service::{
    controller::{Controller, Job, Take},
    measurement::MeasureProgress,
    preferences::{self, PreferenceStore},
    review::ImpulseReport,
};
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use std::time::Duration;

const BG: Color32 = Color32::from_rgb(15, 20, 27);
const PANEL: Color32 = Color32::from_rgb(22, 29, 38);
const CARD: Color32 = Color32::from_rgb(29, 38, 49);
const SELECTED_CARD: Color32 = Color32::from_rgb(30, 57, 62);
const TEXT: Color32 = Color32::from_rgb(233, 240, 247);
const MUTED: Color32 = Color32::from_rgb(151, 167, 184);
const ACCENT: Color32 = Color32::from_rgb(113, 219, 201);
const WARM: Color32 = Color32::from_rgb(243, 194, 123);
const RED: Color32 = Color32::from_rgb(248, 136, 139);

const CARD_MIN_WIDTH: f32 = 210.0;
const CARD_HEIGHT: f32 = 112.0;
const CARD_GAP: f32 = 8.0;
/// Narrowest alignment chart drawn beside the room map, so the far-arrival bracket keeps
/// its values readable; a narrower chart spans the full width above the map and cards.
const CHART_MIN_WIDTH: f32 = 700.0;

pub use companion_service::preferences::ResultPlot;
use companion_service::preferences::ViewPreferences;

/// The renderer output fields a speaker card shows.
struct CardOutput<'a> {
    id: &'a str,
    name: &'a str,
    muted: bool,
    layout: String,
    now: String,
}

pub struct CompanionApp {
    pub controller: Controller,
    pending_view: Option<ViewPreferences>,
    last_workspace_revision: u64,
    pub result_plot: ResultPlot,
    waveform_view: waveform_plot::State,
    manual_noise_dbfs: f32,
    saved_selected: bool,
    pending_review_folder: Option<std::path::PathBuf>,
    attempted_analysis: Option<std::path::PathBuf>,
    endpoint: String,
    closing: bool,
    selected_result: Option<Take>,
    selected_speaker: Option<String>,
    preview: bool,
    review_path: String,
    preferences: Option<PreferenceStore>,
    preferences_error: Option<String>,
    /// Lab shows every diagnostic; off keeps the chart, cards and one action bar.
    pub lab: bool,
    show_manual: bool,
    open_manual: bool,
    /// Last drawn alignment stage height, reserved while a new alignment is captured
    /// so the speaker cards below never move during capture.
    stage_height: f32,
    /// The setup rail shows chosen values; these open their editors.
    edit_microphone: bool,
    edit_reference: bool,
}

impl CompanionApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        match preferences::default_path().and_then(PreferenceStore::open) {
            Ok(store) => Self::with_preferences(&cc.egui_ctx, Controller::default(), store),
            Err(error) => {
                let mut app = Self::with_controller(&cc.egui_ctx, Controller::default(), true);
                app.preferences_error = Some(format!(
                    "Settings could not be restored; automatic saving is disabled and the existing file was kept: {error}"
                ));
                app
            }
        }
    }

    /// Restore preferences before device enumeration or renderer discovery.
    pub fn with_preferences(
        ctx: &egui::Context,
        mut controller: Controller,
        store: PreferenceStore,
    ) -> Self {
        let prefs = store.preferences().clone();
        let reconnect = store.loaded();
        controller.settings = prefs.settings;
        controller.excluded_outputs = prefs.excluded_outputs;
        let mut app = Self::with_controller(ctx, controller, false);
        app.endpoint = prefs.endpoint;
        app.review_path = prefs.review_path;
        app.pending_view = Some(prefs.view.clone());
        app.result_plot = prefs.view.plot;
        app.waveform_view.frequency = prefs.view.burst_frequency;
        app.waveform_view.overview = prefs.view.burst_overview;
        app.waveform_view.overlay = prefs.view.burst_overlay;
        app.saved_selected = prefs.view.saved_selected;
        app.lab = prefs.view.show_alignment;
        app.manual_noise_dbfs = if prefs.view.manual_noise_dbfs.is_finite() {
            prefs.view.manual_noise_dbfs.clamp(-60.0, -12.0)
        } else {
            -30.0
        };
        app.preferences = Some(store);
        let index = if reconnect {
            prefs.workspace
        } else {
            Some(Default::default())
        };
        app.controller
            .restore_workspace(index, reconnect.then(|| app.endpoint.clone()));
        app
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
            saved_selected: controller.state.review.is_some(),
            selected_speaker: controller
                .state
                .review
                .as_ref()
                .map(|r| r.output_id.clone()),
            controller,
            pending_view: None,
            last_workspace_revision: u64::MAX,
            result_plot: ResultPlot::default(),
            waveform_view: waveform_plot::State::default(),
            manual_noise_dbfs: -30.0,
            pending_review_folder: None,
            attempted_analysis: None,
            endpoint: "127.0.0.1:9000".into(),
            closing: false,
            selected_result: None,
            preview: false,
            review_path: String::new(),
            preferences: None,
            preferences_error: None,
            lab: false,
            show_manual: false,
            open_manual: false,
            stage_height: 0.0,
            edit_microphone: false,
            edit_reference: false,
        }
    }

    /// Label a fixture-driven window explicitly; production uses LiveBackend.
    pub fn preview(ctx: &egui::Context, controller: Controller) -> Self {
        let mut app = Self::with_controller(ctx, controller, false);
        app.preview = true;
        ctx.style_mut(|style| {
            style.animation_time = 0.0;
            style.scroll_animation = egui::style::ScrollAnimation::none();
        });
        app
    }

    pub fn draw(&mut self, ctx: &egui::Context) {
        self.controller.poll();
        if self.controller.state.workspace_ready
            && let Some(view) = self.pending_view.take()
        {
            self.selected_result = self
                .controller
                .state
                .takes
                .iter()
                .rev()
                .find(|t| {
                    if let Some(path) = &view.selected_take {
                        t.outcome.as_ref().is_ok_and(|o| {
                            companion_service::workspace::absolute(&o.directory) == *path
                        })
                    } else {
                        view.selected_output.as_ref() == Some(&t.output_id)
                    }
                })
                .cloned();
            if self.controller.state.workspace_imported && self.selected_result.is_none() {
                self.selected_result = self.controller.state.takes.last().cloned();
            }
            self.selected_speaker = self
                .selected_result
                .as_ref()
                .map(|t| t.output_id.clone())
                .or(view.selected_output)
                .or_else(|| {
                    self.saved_selected
                        .then(|| {
                            self.controller
                                .state
                                .review
                                .as_ref()
                                .map(|r| r.output_id.clone())
                        })
                        .flatten()
                });
            self.saved_selected &= self.controller.state.review.is_some();
        }
        self.sync_selected_analysis();
        if ctx.input(|i| i.viewport().close_requested())
            && (self.controller.cancellable()
                || matches!(self.controller.state.job, Some(Job::ApplyAlignment)))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            self.controller.cancel();
        }
        if self.closing && !self.controller.busy() {
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
                        ui.add_space(12.0);
                        lab_switch(ui, &mut self.lab);
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
                if matches!(self.controller.state.job, Some(Job::RestoreWorkspace)) {
                    ui.label(RichText::new("Restoring previous measurements…").color(MUTED));
                }
                if !self.controller.state.workspace_warnings.is_empty() {
                    egui::CollapsingHeader::new("Some saved results could not be restored").show(
                        ui,
                        |ui| {
                            for warning in &self.controller.state.workspace_warnings {
                                ui.colored_label(WARM, warning);
                            }
                        },
                    );
                }
                let save_error = self.preferences.as_ref().and_then(PreferenceStore::error);
                if let Some(error) = self.preferences_error.as_ref().or(save_error.as_ref()) {
                    ui.label(RichText::new(error).small().color(WARM));
                    if save_error.is_some()
                        && ui.small_button("Retry saving settings").clicked()
                        && let Some(store) = &self.preferences
                    {
                        store.retry();
                    }
                }
            });
        egui::TopBottomPanel::bottom("footer")
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(24, 10)))
            .show(ctx, |ui| {
                ui.label(RichText::new("Measurement and experimental alignment · Settings change only through Apply or Restore")
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
            .show(ctx, |ui| {
                let height = ui.available_height();
                let has_result = self.selected_speaker.is_some()
                    || self.selected_result.is_some()
                    || self.saved_selected
                    || self.controller.state.review.is_some();
                let reserved = 190.0;
                let panel = if has_result {
                    egui::TopBottomPanel::bottom("results_panel")
                        .resizable(true)
                        .default_height((height * 0.5).min((height - reserved).max(140.0)))
                        .height_range(140.0..=(height - reserved).max(140.0))
                } else {
                    egui::TopBottomPanel::bottom("empty_results_panel").exact_height(75.0)
                };
                panel
                    .frame(
                        egui::Frame::new()
                            .fill(PANEL)
                            .inner_margin(egui::Margin::same(10)),
                    )
                    .show_inside(ui, |ui| {
                        self.results(ui);
                    });
                ui.set_clip_rect(ui.available_rect_before_wrap().intersect(ui.clip_rect()));
                if !matches!(self.controller.state.job, Some(Job::RestoreWorkspace)) {
                    egui::ScrollArea::vertical()
                        .id_salt("speakers_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.speakers(ui));
                }
            });
        self.persist_preferences();
    }

    fn persist_preferences(&mut self) {
        if let Some(store) = &mut self.preferences {
            store.update(
                &self.controller.settings,
                &self.endpoint,
                &self.controller.excluded_outputs,
                &self.review_path,
            );
            if self.pending_view.is_none() {
                if self.last_workspace_revision != self.controller.state.workspace_revision
                    && !self.controller.busy()
                {
                    store.update_workspace(
                        companion_service::workspace::WorkspaceIndex::from_state(
                            &self.controller.state,
                        ),
                    );
                    self.last_workspace_revision = self.controller.state.workspace_revision;
                }
                store.update_view(ViewPreferences {
                    plot: self.result_plot,
                    show_alignment: self.lab,
                    saved_selected: self.saved_selected,
                    selected_take: self
                        .selected_result
                        .as_ref()
                        .and_then(|t| t.outcome.as_ref().ok())
                        .map(|o| companion_service::workspace::absolute(&o.directory)),
                    selected_output: self.selected_speaker.clone(),
                    burst_frequency: self.waveform_view.frequency,
                    burst_overview: self.waveform_view.overview,
                    burst_overlay: self.waveform_view.overlay,
                    manual_noise_dbfs: self.manual_noise_dbfs,
                });
            }
        }
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
        let automatic = self.controller.state.defaults;
        // A missing or unavailable choice keeps the editor open until Done.
        if self.controller.selected_input().is_none()
            || self.controller.input_channels() == 0
            || self.controller.settings.microphone_id.trim().is_empty()
            || self.controller.settings.position_id.trim().is_empty()
        {
            self.edit_microphone = true;
        }
        if !self.edit_microphone
            && let Some(input) = self.controller.selected_input()
        {
            let settings = &self.controller.settings;
            let detail = format!(
                "{} · {} Hz\n{} · {}",
                input.channel_label(settings.input_channel),
                settings.sample_rate,
                settings.microphone_id,
                settings.position_id
            );
            if chosen(
                ui,
                &input.label(),
                &detail,
                (automatic.device || automatic.microphone_id || automatic.position_id).then_some(
                    "Chosen automatically: the first microphone input at its preferred rate.",
                ),
                !busy,
            ) {
                self.edit_microphone = true;
            }
        }
        ui.add_enabled_ui(!busy, |ui| {
            if !self.edit_microphone {
                return;
            }
            let selected = self
                .controller
                .selected_input()
                .map(|input| input.label())
                .unwrap_or_else(|| self.controller.settings.device.clone()
                    .unwrap_or_else(|| "Choose an input…".into()));
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
            if self.controller.settings.device.is_some() && self.controller.selected_input().is_none() && !busy {
                ui.label(RichText::new("Selected input unavailable. Reconnect it and refresh, or choose another input.").small().color(WARM));
            }
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
            let complete = self.controller.selected_input().is_some()
                && self.controller.input_channels() > 0
                && !self.controller.settings.microphone_id.trim().is_empty()
                && !self.controller.settings.position_id.trim().is_empty();
            ui.add_space(4.0);
            if ui
                .add_enabled(complete, egui::Link::new("Done"))
                .on_disabled_hover_text("Choose an input, then name the microphone and position.")
                .clicked()
            {
                self.edit_microphone = false;
            }
        });
        if let Some(error) = &self.controller.state.input_error {
            ui.label(RichText::new(error).small().color(RED));
        }
        ui.label(
            RichText::new(
                "Input port names identify interface channels, not the speaker being measured.",
            )
            .small()
            .color(MUTED),
        );
        microphone_view::draw(ui, &mut self.controller);
        ui.add_space(23.0);
        section(ui, "02", "Timing reference");
        ui.add_space(10.0);
        let reference = self
            .controller
            .settings
            .reference_output
            .as_deref()
            .and_then(|id| {
                match &self.controller.state.snapshot {
                    // Before connecting, the saved choice is shown as saved.
                    None => Some(id.to_owned()),
                    Some(snapshot) => snapshot
                        .outputs
                        .iter()
                        .find(|o| o.id == id && !o.muted)
                        .map(|o| o.name.clone()),
                }
            });
        if reference.is_none() {
            self.edit_reference = true;
        }
        if !self.edit_reference
            && let Some(name) = &reference
            && chosen(
                ui,
                name,
                if self.controller.state.snapshot.is_some() {
                    "Arrivals are measured against this speaker."
                } else {
                    "Saved choice · connect to check it."
                },
                automatic
                    .reference
                    .then_some("Chosen automatically: the centre speaker, else the front left."),
                !busy,
            )
        {
            self.edit_reference = true;
        }
        ui.add_enabled_ui(!busy, |ui| {
            if !self.edit_reference {
                return;
            }
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
            if let (Some(id), Some(snapshot)) = (&self.controller.settings.reference_output, &self.controller.state.snapshot)
                && !snapshot.outputs.iter().any(|o| &o.id == id && !o.muted) {
                ui.label(RichText::new("Selected reference is missing or muted. Restore it or choose another speaker.").small().color(WARM));
            }
            if ui
                .add_enabled(reference.is_some(), egui::Link::new("Done"))
                .on_disabled_hover_text("Choose an available speaker.")
                .clicked()
            {
                self.edit_reference = false;
            }
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
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.controller.settings.retake_clipped,
                        "Re-measure clipped speakers at a lower gain",
                    );
                    ui.label(
                        RichText::new(
                            "Lowers that speaker's output gain by its overdrive plus 3 dB for one more take, then restores it. Its level counts the change back.",
                        )
                        .small()
                        .color(MUTED),
                    );
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
            ui.label(RichText::new("Speakers").size(23.0).strong()).on_hover_text("Measure individually or capture checked speakers. Choose the playback sink in Studio / Audio Output / Output device.");
            if let Some(snapshot) = &self.controller.state.snapshot {
                ui.add_space(4.0);
                badge(ui, &format!("{}", snapshot.outputs.len()), MUTED);
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let ready = self.controller.alignment_readiness();
            let enabled = ready.is_ok();
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(
                        RichText::new("Align levels and delays")
                            .strong()
                            .color(if enabled { BG } else { MUTED }),
                    )
                    .fill(if enabled { ACCENT } else { PANEL })
                    .min_size(Vec2::new(170.0, 30.0)),
                )
                .on_hover_text("Measure every checked speaker twice, then propose gains and delays. Nothing changes until Apply.")
                .on_disabled_hover_text(ready.as_ref().err().map(String::as_str).unwrap_or(""))
                .clicked()
            {
                self.controller.start_alignment();
            }
            if ui
                .add_enabled(
                    self.controller.cancellable() && !self.controller.state.stopping,
                    egui::Button::new(RichText::new("Stop").color(RED))
                        .min_size(Vec2::new(60.0, 30.0)),
                )
                .clicked()
            {
                self.controller.cancel();
            }
            let left = self
                .controller
                .remaining()
                .map(|d| format!(" · {}", time_left(d)))
                .unwrap_or_default();
            let text = match &self.controller.state.job {
                Some(Job::Alignment {
                    output_ids,
                    current,
                    pass,
                }) => format!(
                    "Pass {} / 2 · Speaker {} / {}{left}",
                    pass + 1,
                    current + 1,
                    output_ids.len()
                ),
                Some(Job::Remeasure { output_id, pass }) => format!(
                    "{} again · Pass {} / 2{left}",
                    self.controller
                        .state
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.outputs.iter().find(|o| o.id == *output_id))
                        .map_or(output_id.as_str(), |o| o.name.as_str()),
                    pass + 1
                ),
                Some(Job::Batch {
                    output_ids,
                    current,
                }) => format!("Speaker {} / {}{left}", current + 1, output_ids.len()),
                _ => self
                    .controller
                    .batch_outputs()
                    .map(|o| {
                        format!(
                            "{} selected{}",
                            o.len(),
                            self.controller
                                .alignment_estimate()
                                .map(|e| format!(" · {}", about(e.total)))
                                .unwrap_or_default()
                        )
                    })
                    .unwrap_or_default(),
            };
            ui.add(egui::Label::new(RichText::new(text).small().color(MUTED)).truncate());
        });
        ui.label(
            RichText::new("Measures each checked speaker twice, then charts what Apply would change. Keep the microphone and input gain fixed.")
                .small()
                .color(MUTED),
        );
        if self.lab {
            ui.horizontal_wrapped(|ui| {
                let readiness = self.controller.batch_outputs();
                if ui
                    .add_enabled(
                        readiness.is_ok(),
                        egui::Button::new("Measure selected speakers"),
                    )
                    .on_disabled_hover_text(
                        readiness.as_ref().err().map(String::as_str).unwrap_or(""),
                    )
                    .clicked()
                {
                    self.controller.start_batch();
                }
                ui.label(
                    RichText::new(format!(
                        "{}One diagnostic pass. Does not propose or apply corrections.",
                        self.controller
                            .batch_estimate()
                            .map(|e| format!("{} · ", capitalized(&about(e.total))))
                            .unwrap_or_default()
                    ))
                    .small()
                    .color(MUTED),
                );
            });
        }
        if let Some(snapshot) = &self.controller.state.snapshot {
            let excluded: Vec<_> = snapshot
                .outputs
                .iter()
                .filter(|o| self.controller.excluded_outputs.contains(&o.id))
                .map(|o| o.name.as_str())
                .collect();
            let unavailable: Vec<_> = snapshot
                .outputs
                .iter()
                .filter(|o| {
                    !self.controller.excluded_outputs.contains(&o.id)
                        && self.controller.output_readiness(&o.id).is_err()
                })
                .map(|o| o.name.as_str())
                .collect();
            let summary = format!(
                "Unchecked: {} · Unavailable: {}",
                if excluded.is_empty() {
                    "none".into()
                } else {
                    excluded.join(", ")
                },
                if unavailable.is_empty() {
                    "none".into()
                } else {
                    unavailable.join(", ")
                }
            );
            ui.add(egui::Label::new(RichText::new(&summary).small().color(MUTED)).truncate())
                .on_hover_text(&summary);
        }
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
            .or(self.controller.state.saved_snapshot.as_ref())
            .map(|s| s.outputs.clone());
        let revision = self
            .controller
            .state
            .snapshot
            .as_ref()
            .map(|s| s.revision.clone());
        let revision = revision.as_deref();
        let busy = self.controller.busy();
        let mut stage = alignment_view::StageResponse::default();
        let mut cards: Vec<Option<alignment_view::Card>> = Vec::new();
        let mut alignment_action = None;
        let summary = self
            .controller
            .state
            .alignment
            .as_ref()
            .map(alignment_view::Summary::new);
        // The chart, or the previous chart's place while a new alignment is captured.
        let chart = summary.is_some()
            || matches!(self.controller.state.job, Some(Job::Alignment { .. }))
                && self.stage_height > 0.0;
        let chips = outputs
            .as_deref()
            .map(|o| self.map_chips(o, summary.as_ref()))
            .unwrap_or_default();
        // The room map takes a column on the right, beside the speaker cards, and beside
        // the chart too when the chart keeps a readable width; a window too narrow for two
        // card columns beside it shows the map below the cards. Nothing above the cards
        // moves for it, and no placement or size depends on capture state.
        let width = ui.available_width();
        let count = outputs.as_ref().map_or(0, Vec::len);
        let map = room_map::available(&chips)
            .then(|| room_map::placement(width, count, CARD_MIN_WIDTH, CARD_HEIGHT, CARD_GAP));
        let map_width = match map {
            Some(room_map::Placement::Beside { side }) => Some(side + room_map::CARD_MARGINS),
            _ => None,
        };
        let chart_in_column =
            map_width.is_some_and(|map| width - map - CARD_GAP >= CHART_MIN_WIDTH);
        if let (Some(summary), Some(outputs)) = (&summary, &outputs) {
            cards = outputs
                .iter()
                .map(|o| summary.speaker(&o.id).map(alignment_view::card))
                .collect();
        }
        let mut map_click = None;
        let reserved = self.stage_height;
        if chart && !chart_in_column {
            let height = chart_card(
                ui,
                summary.as_ref(),
                reserved,
                self.selected_speaker.as_deref(),
                busy,
                &mut stage,
            );
            if summary.is_some() {
                self.stage_height = height;
            }
            ui.add_space(10.0);
        }
        let top = ui.cursor().min;
        let column_width = width - map_width.map_or(0.0, |map| map + CARD_GAP);
        // Explicit child columns rather than closures: the cards below borrow `self`.
        let outer = ui;
        let mut left = outer.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_size(
                    top,
                    Vec2::new(column_width, f32::INFINITY),
                ))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        left.set_width(column_width);
        if chart && chart_in_column {
            let height = chart_card(
                &mut left,
                summary.as_ref(),
                reserved,
                self.selected_speaker.as_deref(),
                busy,
                &mut stage,
            );
            if summary.is_some() {
                self.stage_height = height;
            }
            left.add_space(10.0);
        }
        let ui = &mut left;
        if self.controller.state.snapshot.is_none()
            && self.controller.state.saved_snapshot.is_some()
        {
            ui.colored_label(
                WARM,
                "Saved speaker layout · connect to refresh live settings",
            );
        }
        match &outputs {
            None => {
                card().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Your speakers will appear here")
                                .size(20.0)
                                .strong(),
                        );
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(
                                "Start Omniphony, then connect using its local address above.",
                            )
                            .color(MUTED),
                        );
                        ui.label(
                            RichText::new("Choose your microphone input while you get connected.")
                                .small()
                                .color(MUTED),
                        );
                    });
                    ui.add_space(30.0);
                });
            }
            Some(outputs) if outputs.is_empty() => {
                ui.label("Omniphony reported no physical outputs. Check its speaker layout, then refresh.");
            }
            Some(outputs) => {
                // Card positions depend only on the speaker count and width, never on
                // capture state, so progress cannot move the grid.
                let width = ui.available_width();
                let columns =
                    (((width + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as usize).max(1);
                let card_width = (width - CARD_GAP * (columns - 1) as f32) / columns as f32;
                let origin = ui.cursor().min;
                for (i, output) in outputs.iter().enumerate() {
                    let rect = egui::Rect::from_min_size(
                        origin
                            + Vec2::new(
                                (i % columns) as f32 * (card_width + CARD_GAP),
                                (i / columns) as f32 * (CARD_HEIGHT + CARD_GAP),
                            ),
                        Vec2::new(card_width, CARD_HEIGHT),
                    );
                    let band = match (output.freq_low_hz, output.freq_high_hz) {
                        (Some(lo), Some(hi)) => format!("{lo:.0}–{hi:.0} Hz"),
                        (Some(lo), None) => format!("From {lo:.0} Hz"),
                        (None, Some(hi)) => format!("Up to {hi:.0} Hz"),
                        _ => "No band restriction".into(),
                    };
                    let output = CardOutput {
                        id: &output.id,
                        name: &output.name,
                        muted: output.muted,
                        layout: format!("Output {} · {band}", output.index + 1),
                        now: format!(
                            "Now: gain {:+.1} dB · delay {:.2} ms",
                            output.gain_db, output.delay_ms
                        ),
                    };
                    self.speaker_card(ui, &output, rect, cards.get(i).and_then(Option::as_ref));
                }
                ui.add_space(4.0);
            }
        }
        if let Some(room_map::Placement::Below { side }) = map {
            ui.add_space(6.0);
            card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                map_click = room_map::wide(ui, &chips, side);
            });
        }
        let mut used = left.min_rect();
        if let (Some(map), Some(room_map::Placement::Beside { side })) = (map_width, map) {
            let mut right = outer.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(
                        egui::pos2(top.x + column_width + CARD_GAP, top.y),
                        Vec2::new(map, f32::INFINITY),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            card().show(&mut right, |ui| {
                map_click = room_map::compact(ui, &chips, side);
            });
            used = used.union(right.min_rect());
        }
        outer.advance_cursor_after_rect(used);
        let ui = outer;
        if let Some(alignment) = &self.controller.state.alignment {
            ui.add_space(6.0);
            card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                alignment_action = alignment_view::action_bar(
                    ui,
                    alignment,
                    alignment_view::live(alignment, revision),
                    busy,
                    matches!(self.controller.state.job, Some(Job::UpdateProposal)),
                );
            });
            if self.lab {
                ui.add_space(10.0);
                let summary = alignment_view::Summary::new(alignment);
                let revision_changed =
                    revision.is_some() && !alignment_view::live(alignment, revision);
                let older = |row: &companion_service::alignment::Row| {
                    alignment.settings != self.controller.settings
                        || revision_changed
                        || self
                            .controller
                            .display_take(&row.output_id)
                            .and_then(|t| t.outcome.as_ref().ok())
                            .is_some_and(|o| {
                                !row.take_folders.iter().any(|p| {
                                    companion_service::workspace::absolute(p)
                                        == companion_service::workspace::absolute(&o.directory)
                                })
                            })
                };
                card().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if let Some(action) = alignment_view::lab(ui, &summary, busy, &older) {
                        alignment_action = Some(action);
                    }
                });
            }
        }
        if let Some(id) = stage.select.or(map_click) {
            self.select_speaker(&id);
        }
        if let Some(id) = stage.match_knobs {
            self.show_manual = true;
            self.open_manual = true;
            self.select_speaker(&id);
        }
        if let Some((id, include)) = stage.include {
            self.controller.include_anyway(&id, include);
        }
        if let Some(action) = alignment_action {
            if action == "reanalyze" {
                self.controller.reanalyze_alignment();
            } else if action == "proposal" {
                self.controller.refresh_alignment_proposal();
            } else {
                self.controller.alignment_action(action);
            }
        }
        ui.add_space(10.0);
        if self.lab || self.show_manual || self.controller.state.manual_run.is_some() {
            let open = std::mem::take(&mut self.open_manual);
            let header = manual_level_view::draw(
                ui,
                &mut self.controller,
                self.selected_speaker.as_deref(),
                &mut self.manual_noise_dbfs,
                open,
            );
            if open {
                header.scroll_to_me(Some(egui::Align::TOP));
            }
        }
        if self.lab {
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
                    self.analyze_selected_folder(self.review_path.clone());
                }
                if ui
                    .add_enabled(
                        !self.controller.busy(),
                        egui::Button::new("Load alignment session"),
                    )
                    .clicked()
                {
                    self.controller.load_alignment(&self.review_path);
                }
                ui.label(
                    RichText::new("Creates a new analysis report. The original take is preserved.")
                        .small()
                        .color(MUTED),
                );
            });
        }
        if matches!(self.controller.state.job, Some(Job::Review)) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Analyzing recording…");
            });
        }
        if let Some(error) = &self.controller.state.review_error {
            ui.label(RichText::new(error).color(WARM));
        }
        if self.lab && !self.controller.state.takes.is_empty() {
            ui.add_space(8.0);
            egui::CollapsingHeader::new(format!(
                "Recent takes ({})",
                self.controller.state.takes.len()
            ))
            .show(ui, |ui| {
                let mut selected = None;
                for take in self.controller.state.takes.iter().rev() {
                    let (status, color) = self.measurement_status(take);
                    if ui
                        .button(
                            RichText::new(format!("{} · {status}", take.output_id)).color(color),
                        )
                        .clicked()
                    {
                        selected = Some(take.clone());
                    }
                }
                if let Some(take) = selected {
                    self.select_take(take);
                }
            });
        }
    }

    /// The room map's view of each output: its layout position, the card's quality
    /// grade, selection and role. Set aside means unchecked, muted or outside the sweep
    /// band: speaker-specific facts, so an unfinished setup does not dim every speaker.
    fn map_chips<'a>(
        &self,
        outputs: &'a [omniphony_adapter::protocol::Output],
        summary: Option<&alignment_view::Summary>,
    ) -> Vec<room_map::Chip<'a>> {
        let sweep = self.controller.settings.sweep;
        outputs
            .iter()
            .map(|o| {
                let speaker = summary.and_then(|s| s.speaker(&o.id));
                let dimmed = if self.controller.excluded_outputs.contains(&o.id) {
                    Some("Set aside: unchecked for capture".to_owned())
                } else if o.muted {
                    Some("Set aside: muted in Omniphony".to_owned())
                } else if sweep.start_hz < o.freq_low_hz.unwrap_or(20.0)
                    || sweep.end_hz > o.freq_high_hz.unwrap_or(f32::MAX)
                {
                    Some("Set aside: the sweep does not fit its frequency range".to_owned())
                } else {
                    speaker.and_then(|sp| sp.reason.as_ref()).map(|reason| {
                        if reason.is_set_aside() {
                            format!("Set aside from correction: {}", reason.human())
                        } else {
                            format!("Excluded from correction: {}", reason.human())
                        }
                    })
                };
                room_map::Chip {
                    id: &o.id,
                    name: &o.name,
                    position: room_map::Position::from_output(o),
                    grade: speaker.and_then(|sp| sp.grade).or_else(|| {
                        self.controller
                            .display_take(&o.id)
                            .and_then(|t| t.outcome.as_ref().ok())
                            .map(|outcome| outcome.quality().grade())
                    }),
                    selected: self.selected_speaker.as_deref() == Some(o.id.as_str()),
                    active: self.controller.active_output() == Some(o.id.as_str()),
                    reference: self.controller.settings.reference_output.as_deref()
                        == Some(o.id.as_str()),
                    dimmed,
                }
            })
            .collect()
    }

    /// One fixed-size speaker card: name, quality, measured/planned values and progress.
    fn speaker_card(
        &mut self,
        ui: &mut egui::Ui,
        output: &CardOutput,
        rect: egui::Rect,
        text: Option<&alignment_view::Card>,
    ) {
        let readiness = self.controller.can_measure(output.id);
        let take = self.controller.display_take(output.id).cloned();
        let active = self.controller.active_output() == Some(output.id);
        let selected = self.selected_speaker.as_deref() == Some(output.id);
        let reference = self.controller.settings.reference_output.as_deref() == Some(output.id);
        let mut hover = format!("{}\n{}\n{}", output.name, output.layout, output.now);
        if let Some(text) = text {
            hover = format!("{hover}\n\n{}", text.details);
        }
        let grade = text.and_then(|t| t.grade).or_else(|| {
            take.as_ref()
                .and_then(|t| t.outcome.as_ref().ok())
                .map(|o| o.quality().grade())
        });
        let mut controls = [egui::Rect::NOTHING; 2];
        let card = ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .id_salt(("speaker_card", output.id))
                .sense(egui::Sense::click()),
            |ui| {
                ui.set_min_size(rect.size());
                ui.style_mut().interaction.selectable_labels = false;
                ui.painter().rect(
                    rect,
                    8.0,
                    if selected { SELECTED_CARD } else { CARD },
                    Stroke::new(
                        1.5_f32,
                        if selected {
                            ACCENT
                        } else {
                            Color32::TRANSPARENT
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                let inner = rect.shrink2(Vec2::new(10.0, 8.0));
                let line = |top: f32, height: f32| {
                    egui::Rect::from_min_size(
                        inner.min + Vec2::new(0.0, top),
                        Vec2::new(inner.width(), height),
                    )
                };
                let row =
                    |ui: &mut egui::Ui, rect: egui::Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                            |ui| add(ui),
                        );
                    };
                row(ui, line(0.0, 28.0), &mut |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let mut included = !self.controller.excluded_outputs.contains(output.id)
                        && self.controller.output_readiness(output.id).is_ok();
                    let include = ui
                        .add_enabled(
                            !self.controller.busy() && readiness.is_ok(),
                            egui::Checkbox::without_text(&mut included),
                        )
                        .on_hover_text(
                            "Include in whole-system capture. Uncheck unwired speakers.",
                        );
                    controls[0] = include.rect;
                    if include.changed() {
                        if included {
                            self.controller.excluded_outputs.remove(output.id);
                        } else {
                            self.controller
                                .excluded_outputs
                                .insert(output.id.to_owned());
                        }
                    }
                    quality_view::dot(ui, grade);
                    let name_width =
                        (ui.available_width() - 76.0 - if reference { 34.0 } else { 0.0 })
                            .max(30.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(name_width, 24.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(output.name).size(15.0).strong())
                                    .truncate(),
                            )
                            .on_hover_text(&hover);
                        },
                    );
                    if reference {
                        ui.label(RichText::new("REF").size(10.0).strong().color(ACCENT))
                            .on_hover_text(
                                "Timing reference: arrivals are measured against this speaker.",
                            );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // With an alignment that is not applied, Measure offers to measure
                        // this speaker again into it; the diagnostic take stays a choice.
                        let session = !active
                            && self.controller.state.alignment.as_ref().is_some_and(|a| {
                                a.applied_snapshot.is_none()
                                    && !a.application_uncertain
                                    && a.rows.iter().any(|r| r.output_id == output.id)
                            });
                        let enabled = if active {
                            !self.controller.state.stopping
                        } else {
                            readiness.is_ok()
                        };
                        let button = egui::Button::new(
                            RichText::new(if active { "Stop" } else { "Measure" })
                                .color(if readiness.is_ok() { BG } else { MUTED }),
                        )
                        .fill(if readiness.is_ok() { ACCENT } else { PANEL })
                        .min_size(Vec2::new(70.0, 26.0));
                        let take_hint = format!(
                            "One diagnostic take of this speaker, {}. Does not change any setting.",
                            about(self.controller.take_estimate().total)
                        );
                        let button = if session {
                            let again = self.controller.can_remeasure(output.id);
                            let again_hint = format!(
                                "Two takes of this speaker, one per pass, replace its takes in the alignment and the proposal is rebuilt; the other speakers keep theirs. {}. Does not change any setting.",
                                capitalized(&about(
                                    companion_service::duration::alignment(
                                        self.controller.settings.sweep,
                                        1
                                    )
                                    .total
                                ))
                            );
                            let (button, _) = ui
                                .add_enabled_ui(enabled, |ui| {
                                    egui::containers::menu::MenuButton::from_button(button).ui(
                                        ui,
                                        |ui| {
                                            let mut entry = ui
                                                .add_enabled(
                                                    again.is_ok(),
                                                    egui::Button::new(
                                                        "Measure again into the session",
                                                    ),
                                                )
                                                .on_hover_text(&again_hint);
                                            if let Err(reason) = &again {
                                                entry = entry.on_disabled_hover_text(reason);
                                            }
                                            if entry.clicked() {
                                                self.controller.remeasure(output.id);
                                                ui.close();
                                            }
                                            if ui
                                                .button("One diagnostic take")
                                                .on_hover_text(&take_hint)
                                                .clicked()
                                            {
                                                self.controller.start(output.id);
                                                ui.close();
                                            }
                                        },
                                    )
                                })
                                .inner;
                            button.on_hover_text(
                                "Measure this speaker again into the alignment, or take one diagnostic take.",
                            )
                        } else {
                            let button = ui.add_enabled(enabled, button);
                            if button.clicked() {
                                if active {
                                    self.controller.cancel();
                                } else {
                                    self.controller.start(output.id);
                                }
                            }
                            button.on_hover_text(take_hint)
                        };
                        controls[1] = button.rect;
                        if let Err(reason) = &readiness {
                            button.on_disabled_hover_text(reason);
                        }
                    });
                });
                let (measured, planned) = match text {
                    Some(text) => (&text.measured, text.planned.as_ref()),
                    None => (
                        &(output.now.clone(), MUTED),
                        Some(&(output.layout.clone(), MUTED)),
                    ),
                };
                match planned {
                    Some(planned) => {
                        card_line(ui, line(32.0, 18.0), measured, 1);
                        card_line(ui, line(52.0, 18.0), planned, 1);
                    }
                    None => card_line(ui, line(32.0, 38.0), measured, 2),
                }
                row(ui, line(inner.height() - 22.0, 22.0), &mut |ui| {
                    if output.muted {
                        badge(ui, "MUTED", WARM);
                    }
                    if active {
                        self.row_progress(ui);
                    } else if let Some(take) = &take {
                        let (text, color) = self.measurement_status(take);
                        ui.add(
                            egui::Label::new(RichText::new(text).small().color(color)).truncate(),
                        );
                    } else if let Err(reason) = &readiness {
                        ui.add(
                            egui::Label::new(RichText::new(reason).small().color(MUTED)).truncate(),
                        )
                        .on_hover_text(reason);
                    } else {
                        ui.label(RichText::new("Ready for a first take").small().color(MUTED));
                    }
                });
            },
        );
        card.response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                selected,
                output.name,
            )
        });
        let over_control = card
            .response
            .interact_pointer_pos()
            .is_some_and(|pos| controls.iter().any(|rect| rect.contains(pos)));
        if card
            .response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
            && !over_control
        {
            self.select_speaker(output.id);
        }
    }

    fn row_progress(&self, ui: &mut egui::Ui) {
        let (phase, fraction) = match &self.controller.state.progress {
            Some(MeasureProgress::Preparing) => ("Checking configuration", 0.0),
            Some(MeasureProgress::OpeningInput) => ("Opening microphone", 0.0),
            Some(MeasureProgress::LeadIn) => ("Quiet lead-in", 0.0),
            Some(MeasureProgress::CheckingInput(_)) => ("Checking microphone · no playback", 0.0),
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
            "Stopping and saving…".to_owned()
        } else {
            match self.controller.remaining() {
                Some(left) => format!("{phase} · {}", time_left(left)),
                None => phase.to_owned(),
            }
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(ui.available_width())
                .desired_height(20.0)
                .text(label)
                .fill(ACCENT.gamma_multiply(0.45)),
        );
    }

    fn select_speaker(&mut self, output_id: &str) {
        if let Some(take) = self.controller.display_take(output_id).cloned() {
            self.select_take(take);
        } else {
            self.selected_speaker = Some(output_id.to_owned());
            self.selected_result = None;
            self.saved_selected = false;
            self.pending_review_folder = None;
            self.attempted_analysis = None;
        }
    }

    fn select_take(&mut self, take: Take) {
        self.selected_speaker = Some(take.output_id.clone());
        self.selected_result = Some(take);
        self.saved_selected = false;
        self.pending_review_folder = None;
        self.attempted_analysis = None;
        self.sync_selected_analysis();
    }

    fn analyze_selected_folder(&mut self, path: String) {
        self.review_path = path.trim().to_owned();
        self.selected_speaker = None;
        self.selected_result = None;
        self.saved_selected = true;
        self.pending_review_folder = Some(companion_service::workspace::absolute(
            std::path::Path::new(&self.review_path),
        ));
        self.attempted_analysis = None;
        self.controller.review_saved(&self.review_path);
    }

    // Selection owns the visible report. A worker finishing for another speaker cannot
    // change it, and a failed cache load is not retried on every UI frame.
    fn sync_selected_analysis(&mut self) {
        if self.selected_result.is_none() && !self.saved_selected {
            self.selected_result = self
                .selected_speaker
                .as_deref()
                .and_then(|id| self.controller.display_take(id))
                .cloned();
        }
        if self.selected_result.is_none()
            && let Some(review) = self.active_review()
        {
            let folder = companion_service::workspace::absolute(&review.directory);
            self.selected_speaker = Some(review.output_id.clone());
            self.selected_result = self
                .controller
                .state
                .takes
                .iter()
                .rev()
                .find(|t| {
                    t.outcome.as_ref().is_ok_and(|o| {
                        companion_service::workspace::absolute(&o.directory) == folder
                    })
                })
                .cloned();
        }
        let Some(take) = self.selected_result.as_ref() else {
            return;
        };
        let Some(analysis) = self.controller.analysis_for(take) else {
            self.saved_selected = false;
            return;
        };
        self.saved_selected = true;
        if self.active_review().is_none()
            && !self.controller.busy()
            && self.attempted_analysis.as_ref() != Some(&analysis.report_file)
        {
            self.attempted_analysis = Some(analysis.report_file.clone());
            let take = take.clone();
            self.controller.load_analysis(&take);
        }
    }

    fn active_review(&self) -> Option<&companion_service::review::SavedReview> {
        if !self.saved_selected {
            return None;
        }
        let review = self.controller.state.review.as_ref()?;
        let folder = companion_service::workspace::absolute(&review.directory);
        if let Some(take) = &self.selected_result {
            let analysis = self.controller.analysis_for(take)?;
            if folder != analysis.directory
                || companion_service::workspace::absolute(&review.report_file)
                    != analysis.report_file
            {
                return None;
            }
        } else if self
            .pending_review_folder
            .as_ref()
            .is_some_and(|p| *p != folder)
        {
            return None;
        }
        Some(review)
    }

    fn measurement_status(&self, take: &Take) -> (&'static str, Color32) {
        self.controller
            .analysis_for(take)
            .map(analysis_status)
            .unwrap_or_else(|| take_status(take))
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        let output = self.selected_speaker.as_deref().or_else(|| {
            self.selected_result
                .as_ref()
                .map(|t| t.output_id.as_str())
                .or_else(|| self.active_review().map(|r| r.output_id.as_str()))
        });
        let heading = output.map_or_else(
            || "Measurement results".into(),
            |id| format!("{id} · Measurement result"),
        );
        let mut selected = None;
        let mut select_manual = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(heading).size(18.0).strong());
            egui::ComboBox::from_id_salt("measurement_history")
                .selected_text("Previous measurements")
                .show_ui(ui, |ui| {
                    for take in self.controller.state.takes.iter().rev() {
                        let folder = take
                            .outcome
                            .as_ref()
                            .ok()
                            .map(|o| {
                                let folder = companion_service::workspace::absolute(&o.directory);
                                let root = companion_service::workspace::absolute(
                                    std::path::Path::new(&take.settings.output_root),
                                );
                                folder
                                    .strip_prefix(root)
                                    .unwrap_or(&folder)
                                    .display()
                                    .to_string()
                            })
                            .unwrap_or_else(|| "failed capture".into());
                        if ui
                            .selectable_label(
                                false,
                                format!(
                                    "{} · {} · {}",
                                    take.output_id, take.settings.position_id, folder
                                ),
                            )
                            .clicked()
                        {
                            selected = Some(take.clone());
                        }
                    }
                    if let Some(review) = &self.controller.state.review {
                        select_manual = ui
                            .selectable_label(
                                false,
                                format!(
                                    "{} · Saved analysis · {}",
                                    review.output_id,
                                    review.directory.display()
                                ),
                            )
                            .clicked();
                    }
                });
        });
        if let Some(take) = selected {
            self.select_take(take);
        }
        if select_manual {
            self.selected_speaker = self
                .controller
                .state
                .review
                .as_ref()
                .map(|r| r.output_id.clone());
            self.selected_result = None;
            self.saved_selected = true;
            self.pending_review_folder = self
                .controller
                .state
                .review
                .as_ref()
                .map(|r| companion_service::workspace::absolute(&r.directory));
        }
        let folder = self
            .selected_result
            .as_ref()
            .and_then(|t| t.outcome.as_ref().ok())
            .map(|o| companion_service::workspace::absolute(&o.directory))
            .or_else(|| {
                self.active_review()
                    .map(|r| companion_service::workspace::absolute(&r.directory))
            })
            .or_else(|| self.pending_review_folder.clone());
        if self.saved_selected || self.selected_result.is_some() {
            ui.label(
                RichText::new(if self.saved_selected {
                    "Latest saved analysis"
                } else {
                    "Original capture · no saved reanalysis for this take"
                })
                .small()
                .color(MUTED),
            );
        }
        if let Some(folder) = &folder
            && let Some(alignment) = &self.controller.state.alignment
            && let Some((pass, count)) = alignment.rows.iter().find_map(|row| {
                row.take_folders
                    .iter()
                    .position(|p| companion_service::workspace::absolute(p) == *folder)
                    .map(|i| (i + 1, row.take_folders.len()))
            })
        {
            ui.label(
                RichText::new(format!(
                    "Pass {pass} of {count} · Alignment combines both passes"
                ))
                .small()
                .color(MUTED),
            );
        }
        let scroll_id = egui::Id::new((
            "result_scroll",
            &self.selected_speaker,
            folder,
            self.active_review()
                .map(|r| companion_service::workspace::absolute(&r.report_file)),
        ));
        egui::ScrollArea::vertical().id_salt(scroll_id).auto_shrink([false, false]).show(ui, |ui| {
            if self.active_review().is_some() {
                self.review_panel(ui);
            } else if self.saved_selected {
                if self.controller.busy() {
                    ui.horizontal(|ui| { ui.spinner(); ui.label("Loading selected measurement analysis…"); });
                } else if let Some(error) = &self.controller.state.review_error {
                    ui.colored_label(WARM, format!("Selected analysis unavailable: {error}"));
                } else {
                    ui.label("Selected analysis unavailable. Choose a measurement or analyze its saved folder again.");
                }
            } else if self.selected_result.is_some() {
                self.result_panel(ui);
            } else if let Some(output) = &self.selected_speaker {
                    let message = if self.controller.active_output() == Some(output.as_str()) {
                        format!("Measuring {output}… Results will be available when capture finishes.")
                    } else { format!("No measurement for {output} yet. Use Measure to capture it.") };
                    ui.label(RichText::new(message).color(MUTED));
            } else {
                ui.label(RichText::new("Click a speaker card or chart row to inspect its result.").color(MUTED));
            }
        });
    }

    fn result_panel(&mut self, ui: &mut egui::Ui) {
        let Some(take) = &self.selected_result else {
            return;
        };
        let mut review_requested = None;
        let lab = self.lab;
        ui.push_id("live_result", |ui| {
                let (status, color) = take_status(take);
                ui.label(RichText::new(status).size(20.0).color(color).strong());
                if self.controller.state.snapshot.as_ref().is_none_or(|s| s.revision != take.revision) || self.controller.settings != take.settings {
                    ui.colored_label(WARM, "Previous measurement · current renderer or capture settings differ");
                }
                ui.label(RichText::new(format!("{} · {} · Channel {} · Reference: {}", take.settings.microphone_id, take.settings.position_id, take.settings.input_channel, take.settings.reference_output.as_deref().unwrap_or("—"))).small().color(MUTED));
                match &take.outcome {
                    Ok(outcome) => {
                        draw_quality(ui, &outcome.quality(), lab);
                        if take.settings.device.as_deref().is_some_and(|s| s.starts_with("pipewire:monitor:")) { ui.label(RichText::new("Sink monitor · not an acoustic microphone measurement").color(WARM)); }
                        if let Some(impulse) = &outcome.impulse { draw_impulse(ui, impulse, &mut self.result_plot, &mut self.waveform_view, lab); }
                        if let Some(error) = &outcome.impulse_error { ui.label(RichText::new(error).color(WARM)); }
                        if let Some(report) = &outcome.diagnostics {
                            egui::CollapsingHeader::new("Acquisition diagnostics").default_open(outcome.impulse.is_none()).show(ui, |ui| draw_diagnostics(ui, report, lab));
                        }
                        if let Some(timing) = &outcome.timing && (lab || outcome.impulse.is_none()) {
                            ui.add_space(12.0);
                            egui::Grid::new("timing_values").spacing([32.0, 10.0]).show(ui, |ui| {
                                if outcome.impulse.is_none() {
                                ui.label("Relative peak delay"); ui.monospace(timing.relative_peak_delay_ms.map_or_else(|| "Unresolved".into(), |v| format!("{v:+.3} ms"))); ui.end_row();
                                ui.label("Clock drift"); ui.monospace(timing.clock_drift_ppm.map_or_else(|| "Unresolved".into(), |v| format!("{v:+.1} ppm"))); ui.end_row();
                                }
                                if lab {
                                ui.label("Speaker signal / noise (broadband)"); ui.monospace(format!("{:.1} dB", timing.signal_to_noise_db[1])); ui.end_row();
                                ui.label("Speaker sweep RMS"); ui.monospace(format!("{:.1} dBFS", timing.sweep_rms_dbfs[1])); ui.end_row();
                                ui.label("Background RMS"); ui.monospace(format!("{:.1} dBFS", timing.noise_rms_dbfs)); ui.end_row();
                                }
                            });
                            if let Some(warning) = timing.relative_peak_delay_ms.and_then(calibration_core::long_delay_warning).filter(|_| outcome.impulse.is_none()) {
                                ui.colored_label(WARM, warning);
                            }
                            if let Some(warning) = timing.timing_warning().filter(|_| outcome.impulse.is_none()) {
                                ui.add_space(10.0);
                                ui.label(RichText::new(warning).color(WARM));
                            }
                        }
                        if let Some(reason) = &outcome.rejection_reason { ui.label(RichText::new(reason).color(WARM)); }
                        ui.add_space(14.0);
                        let path = outcome.directory.display().to_string();
                        if lab {
                            caption(ui, "Saved take");
                            ui.label(RichText::new(&path).monospace().small());
                            if ui.button("Copy folder path").clicked() { ui.ctx().copy_text(path.clone()); }
                        }
                        if ui.add_enabled(!self.controller.busy(), egui::Button::new("Reanalyze this take")).clicked() { review_requested = Some(path); }
                    }
                    Err(error) => { ui.label(RichText::new(error).color(RED)); }
                }
                if lab {
                    ui.add_space(14.0);
                    ui.label(RichText::new("Arrival candidates are experimental. An early peak is not proof of the direct path. Automatic gain/delay correction remains disabled.").small().color(MUTED));
                }
            });
        if let Some(path) = review_requested {
            self.analyze_selected_folder(path);
        }
    }
    fn review_panel(&mut self, ui: &mut egui::Ui) {
        let Some(review) = self.active_review().cloned() else {
            return;
        };
        let lab = self.lab;
        ui.push_id("saved_result", |ui| {
            let (status, color) = analysis_status(
                &companion_service::analysis_cache::CachedAnalysis::from_review(&review),
            );
            ui.label(RichText::new(status).size(20.0).color(color).strong());
            if self.selected_result.as_ref().is_some_and(|take| {
                self.controller
                    .state
                    .snapshot
                    .as_ref()
                    .is_none_or(|s| s.revision != take.revision)
                    || self.controller.settings != take.settings
            }) {
                ui.colored_label(
                    WARM,
                    "Previous measurement · current renderer or capture settings differ",
                );
            }
            if let Some(delay) = review.impulse.as_ref().and_then(|r| r.relative_arrival_ms) {
                ui.label(format!(
                    "Arrival relative to {}: {delay:+.3} ms",
                    review.reference_output_id
                ));
            }
            ui.label(
                RichText::new(format!(
                    "{} · {} · Channel {} · Reference: {}",
                    review.microphone_id,
                    review.position_id,
                    review.input_channel,
                    review.reference_output_id
                ))
                .small()
                .color(MUTED),
            );
            if review.origin == "sink_monitor_capture" {
                ui.label(
                    RichText::new("Sink monitor · not an acoustic microphone measurement")
                        .color(WARM),
                );
            }
            if let Some(error) = &review.analysis_error {
                ui.label(RichText::new(error).color(WARM));
            }
            let quality = review.quality.clone().unwrap_or_else(|| {
                companion_service::quality::assess(
                    review.timing.as_ref(),
                    review.impulse.as_deref(),
                    Some(&review.diagnostics),
                    review.analysis_error.as_deref(),
                )
            });
            draw_quality(ui, &quality, lab);
            if let Some(impulse) = &review.impulse {
                draw_impulse(
                    ui,
                    impulse,
                    &mut self.result_plot,
                    &mut self.waveform_view,
                    lab,
                );
            }
            if let Some(error) = &review.impulse_error {
                ui.label(RichText::new(error).color(WARM));
            }
            egui::CollapsingHeader::new("Acquisition diagnostics")
                .default_open(review.impulse.is_none())
                .show(ui, |ui| draw_diagnostics(ui, &review.diagnostics, lab));
            ui.separator();

            egui::CollapsingHeader::new("Original capture details").show(ui, |ui| {
                ui.label("Historical capture verdict, before this saved reanalysis:");
                if let Some(reason) = &review.original_rejection_reason {
                    ui.colored_label(WARM, format!("Original take rejected: {reason}"));
                } else {
                    ui.label(if review.original_accepted_for_analysis {
                        "Original take accepted"
                    } else {
                        "Original take not accepted"
                    });
                }
                ui.label(
                    RichText::new(review.directory.display().to_string())
                        .small()
                        .monospace(),
                );
            });
            if lab {
                ui.label(RichText::new("Saved analysis report").small().color(MUTED));
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
            }
        });
    }
}

impl Drop for CompanionApp {
    fn drop(&mut self) {
        self.persist_preferences();
    }
}

impl eframe::App for CompanionApp {
    fn persist_egui_memory(&self) -> bool {
        !self.preview
    }
    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(2)
    }
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
/// "about 45 s", "about 2 min".
fn about(duration: Duration) -> String {
    let seconds = duration.as_secs_f64().ceil() as u64;
    if seconds < 60 {
        format!("about {seconds} s")
    } else {
        format!("about {} min", (seconds + 30) / 60)
    }
}
/// The remaining-time hint of a running capture.
fn time_left(duration: Duration) -> String {
    if duration.is_zero() {
        "finishing".into()
    } else {
        format!("{} left", about(duration))
    }
}
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A chosen setup value with a "change" link; returns whether the link was clicked.
fn chosen(
    ui: &mut egui::Ui,
    value: &str,
    detail: &str,
    automatic: Option<&str>,
    enabled: bool,
) -> bool {
    let mut change = false;
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width() - 56.0, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.add(egui::Label::new(RichText::new(value).strong()).truncate())
                            .on_hover_text(value);
                        ui.label(RichText::new(detail).small().color(MUTED));
                        if let Some(note) = automatic {
                            ui.label(RichText::new("Chosen automatically").small().color(ACCENT))
                                .on_hover_text(note);
                        }
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    change = ui
                        .add_enabled(enabled, egui::Link::new("change"))
                        .on_disabled_hover_text("Wait for the current operation to finish.")
                        .clicked();
                });
            });
        });
    change
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
/// Fixed-height card text: wraps up to `rows` lines, then ends with an ellipsis.
fn card_line(ui: &egui::Ui, rect: egui::Rect, (text, color): &(String, Color32), rows: usize) {
    let mut job = egui::text::LayoutJob::single_section(
        text.clone(),
        egui::TextFormat::simple(FontId::proportional(12.5), *color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: rect.width(),
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let top = rect.top() + ((rect.height() - galley.size().y) / 2.0).max(0.0);
    ui.painter()
        .galley(egui::pos2(rect.left(), top), galley, *color);
}

/// Persisted Lab toggle, drawn as a switch rather than a checkbox.
fn lab_switch(ui: &mut egui::Ui, on: &mut bool) {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(70.0, 24.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, "Lab"));
    let track = egui::Rect::from_min_size(
        rect.left_center() - Vec2::new(0.0, 10.0),
        Vec2::new(36.0, 20.0),
    );
    let painter = ui.painter();
    painter.rect_filled(
        track,
        10.0,
        if *on {
            ACCENT
        } else {
            Color32::from_rgb(42, 53, 66)
        },
    );
    let knob = if *on {
        track.right_center() - Vec2::new(10.0, 0.0)
    } else {
        track.left_center() + Vec2::new(10.0, 0.0)
    };
    painter.circle_filled(knob, 7.0, if *on { BG } else { TEXT });
    painter.text(
        track.right_center() + Vec2::new(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Lab",
        FontId::proportional(14.0),
        if *on { TEXT } else { MUTED },
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Lab shows every diagnostic: segment S/N tables, band candidates, threshold checks, saved-take review, proposal maintenance and the full caveats.");
}
/// The alignment chart card, or its placeholder at the reserved height; returns its height.
fn chart_card(
    ui: &mut egui::Ui,
    summary: Option<&alignment_view::Summary>,
    reserved: f32,
    selected: Option<&str>,
    busy: bool,
    stage: &mut alignment_view::StageResponse,
) -> f32 {
    card()
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            match summary {
                Some(summary) => *stage = alignment_view::stage(ui, summary, selected, busy),
                None => alignment_view::placeholder(ui, reserved - 28.0),
            }
        })
        .response
        .rect
        .height()
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
fn analysis_status(
    analysis: &companion_service::analysis_cache::CachedAnalysis,
) -> (&'static str, Color32) {
    if analysis
        .quality
        .as_ref()
        .is_some_and(|q| q.acquisition.grade == companion_service::quality::Grade::Poor)
    {
        ("Reanalyzed · quality warning", WARM)
    } else if analysis.arrival_ms().is_some() {
        ("Reanalyzed · arrival candidate", ACCENT)
    } else if analysis.has_response() {
        ("Reanalyzed · arrival unresolved", WARM)
    } else {
        ("Reanalyzed · timing unavailable", WARM)
    }
}
fn take_status(take: &Take) -> (&'static str, Color32) {
    match &take.outcome {
        Ok(o) if !o.accepted && o.timing.is_some() => ("Results available · capture fault", WARM),
        Ok(o) if o.accepted && o.timing.as_ref().is_some_and(|t| !t.warnings.is_empty()) => {
            ("Results available · quality warning", WARM)
        }
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
            ("Captured · arrival unresolved", WARM)
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
        Ok(_) => ("Recorded · timing unavailable", WARM),
        Err(_) => ("Measurement failed", RED),
    }
}

fn draw_quality(ui: &mut egui::Ui, report: &companion_service::quality::Report, lab: bool) {
    if !lab {
        let details = [&report.acquisition, &report.timing, &report.level]
            .iter()
            .flat_map(|i| i.details.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        let grade = report.grade();
        quality_view::badge(
            ui,
            Some(grade),
            &format!("Quality: {}", grade.label()),
            &details,
        );
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (name, indicator) in [
            ("Acquisition", &report.acquisition),
            ("Timing", &report.timing),
            ("Level", &report.level),
        ] {
            quality_view::badge(
                ui,
                Some(indicator.grade),
                &format!("{name}: {}", indicator.grade.label()),
                &indicator.details.join("\n"),
            );
        }
    });
    if let Some(level) = report.level_db {
        ui.label(format!(
            "500–2000 Hz level: {level:+.2} dB relative · Band S/N: {:.1} dB",
            report.level_band_snr_db.unwrap_or_default()
        ));
    }
    egui::CollapsingHeader::new("Measurement quality details").show(ui, |ui| {
        ui.label("Experimental quality grades, not probabilities or calibrated accuracy. Correction requires two consistent passes.");
        for (name, indicator) in [("Acquisition", &report.acquisition), ("Timing", &report.timing), ("Level", &report.level)] {
            ui.strong(name);
            for detail in &indicator.details { ui.label(detail); }
        }
    });
}

fn draw_diagnostics(
    ui: &mut egui::Ui,
    report: &companion_service::review::AcquisitionDiagnostics,
    lab: bool,
) {
    use companion_service::review::SegmentStatus;
    ui.add_space(10.0);
    let target = report.segments.as_ref().map(|s| s[1].status);
    let title = match target {
        Some(SegmentStatus::NoSweepDetected) => "No target sweep detected",
        Some(SegmentStatus::Clipped) => "Target interval clipped",
        _ => "Acquisition levels",
    };
    ui.label(RichText::new(title).strong());
    if !lab {
        for message in &report.messages {
            ui.label(RichText::new(message).color(WARM));
        }
        ui.label(
            RichText::new("Turn on Lab for segment levels and signal/noise.")
                .small()
                .color(MUTED),
        );
        ui.separator();
        return;
    }
    ui.label(format!(
        "Background: {:.1} dBFS · Analysis clipped samples: {}",
        report.noise_rms_dbfs, report.total_clipped_samples
    ));
    if let Some(band) = &report.band_noise {
        ui.label(format!("Excited-band S/N ({:.0}–{:.0} Hz): reference {:.1} / speaker {:.1} / reference {:.1} dB", band.band_hz[0], band.band_hz[1], band.signal_to_noise_db[0], band.signal_to_noise_db[1], band.signal_to_noise_db[2]));
    }
    if let Some(segments) = &report.segments {
        egui::Grid::new("segment_diagnostics")
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                for label in ["Interval", "RMS", "Broadband S/N", "Peak", "Clipped"] {
                    ui.label(RichText::new(label).small().color(MUTED));
                }
                ui.end_row();
                for segment in segments {
                    ui.label(&segment.role);
                    ui.monospace(format!("{:.1}", segment.rms_dbfs));
                    ui.monospace(format!("{:.1} dB", segment.signal_to_noise_db));
                    ui.monospace(format!("{:.1}", segment.peak_dbfs));
                    ui.monospace(match segment.overdrive_db {
                        Some(db) => format!(
                            "{} · {} over",
                            segment.clipped_samples,
                            companion_service::review::clipping::label(db)
                        ),
                        None => segment.clipped_samples.to_string(),
                    });
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

fn draw_arrival_summary(ui: &mut egui::Ui, report: &ImpulseReport, lab: bool) {
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
    if lab {
        ui.label(RichText::new("Experimental arrival evidence; direct path and automatic correction are not validated.").small().color(MUTED));
    }
}

fn draw_impulse(
    ui: &mut egui::Ui,
    report: &ImpulseReport,
    plot: &mut ResultPlot,
    waveform: &mut waveform_plot::State,
    lab: bool,
) {
    if let Some(warning) = report
        .relative_arrival_ms
        .and_then(calibration_core::long_delay_warning)
    {
        ui.colored_label(WARM, warning);
    }
    draw_impulse_plot(ui, report, plot, waveform, lab);
    if report.relative_arrival_ms.is_none() {
        ui.label(
            RichText::new(
                "Arrival unresolved · Inspect Arrival diagnostics; no manual approval is required.",
            )
            .small()
            .color(WARM),
        );
    }
    egui::CollapsingHeader::new("Arrival diagnostics")
        .show(ui, |ui| draw_arrival_summary(ui, report, lab));
}

fn draw_impulse_plot(
    ui: &mut egui::Ui,
    report: &ImpulseReport,
    plot: &mut ResultPlot,
    waveform: &mut waveform_plot::State,
    lab: bool,
) {
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
    ui.horizontal_wrapped(|ui| {
        for (mode, label) in [
            (ResultPlot::Impulse, "Impulse"),
            (ResultPlot::FrequencyResponse, "Frequency response"),
            (ResultPlot::EnergyArrival, "Arrival / frequency"),
            (ResultPlot::GroupDelay, "Group delay"),
            (ResultPlot::SignedWaveform, "Signed waveform"),
            (ResultPlot::ToneBurst, "Virtual burst"),
        ] {
            ui.selectable_value(plot, mode, label);
        }
    });
    if matches!(*plot, ResultPlot::SignedWaveform | ResultPlot::ToneBurst) {
        if let Some(report) = &segment.waveforms {
            waveform_plot::draw(ui, report, *plot, waveform);
        } else {
            ui.label(
                RichText::new(
                    "Reanalyze this take to generate signed waveforms and virtual bursts.",
                )
                .color(WARM),
            );
        }
        return;
    }
    if *plot != ResultPlot::Impulse {
        if let Some(spectral) = &segment.spectral {
            spectral_plot::draw(ui, spectral, *plot);
        } else {
            ui.label(
                RichText::new(
                    segment
                        .spectral_error
                        .as_deref()
                        .unwrap_or("Reanalyze this take to generate frequency plots."),
                )
                .color(WARM),
            );
        }
        return;
    }
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
    if !lab {
        return;
    }
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
