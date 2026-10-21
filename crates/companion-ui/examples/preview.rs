//! Synthetic UI fixture. No live discovery, microphone, playback or session writes.
#[path = "../tests/support/mod.rs"]
mod support;

fn main() -> eframe::Result {
    // `--size=1160x1500` opens a taller window, for screenshots of the whole rail.
    let size = std::env::args()
        .find_map(|a| {
            let (w, h) = a.strip_prefix("--size=")?.split_once('x')?;
            Some([w.parse().ok()?, h.parse().ok()?])
        })
        .unwrap_or([1160.0, 800.0]);
    eframe::run_native(
        "Omniphony Companion · UI preview · No audio",
        eframe::NativeOptions {
            persist_window: false,
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size(size)
                .with_min_inner_size([850.0, 640.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            let (mut controller, _) = support::ready();
            if std::env::args().any(|a| {
                [
                    "--arrival-review",
                    "--frequency-response",
                    "--energy-arrival",
                    "--group-delay",
                    "--signed-waveform",
                    "--tone-burst",
                ]
                .contains(&a.as_str())
            }) {
                controller.state.review = Some(support::arrival_review());
            }
            if std::env::args().any(|a| a == "--alignment") {
                controller.state.alignment = Some(support::room_alignment(&controller));
            }
            if std::env::args().any(|a| a == "--positions") {
                // Two guided positions measured around the main seat, one of them partly.
                let mut a = support::room_alignment(&controller);
                for (index, measured) in [(0, a.rows.len()), (1, a.rows.len() - 1)] {
                    let step = companion_service::positions::GUIDE[index];
                    a.positions
                        .push(companion_service::positions::PositionCapture {
                            step: step.id.into(),
                            position_id: companion_service::positions::position_id(
                                &a.settings.position_id,
                                &step,
                            ),
                            instruction: step.instruction.into(),
                            directory: format!("synthetic-position-{}", step.id).into(),
                            takes: a
                                .rows
                                .iter()
                                .enumerate()
                                .map(|(i, r)| companion_service::positions::PositionTake {
                                    output_id: r.output_id.clone(),
                                    take_folder: format!("synthetic-position-{}/{i}", step.id)
                                        .into(),
                                    accepted: i < measured,
                                    rejection_reason: (i >= measured)
                                        .then(|| "clipped recording".into()),
                                    response: true,
                                })
                                .collect(),
                            error: None,
                        });
                }
                controller.state.alignment = Some(a);
            }
            for arg in std::env::args() {
                if let Some(path) = arg.strip_prefix("--calibration=") {
                    controller.settings.calibration_path = path.into();
                }
            }
            if std::env::args().any(|a| a == "--bench") {
                controller.state.alignment = Some(support::bench_alignment(&mut controller));
            }
            if std::env::args().any(|a| a == "--late-device") {
                // A folder that does not exist: including the speaker cannot write a session.
                controller.state.alignment = Some(support::late_device_alignment(
                    &controller,
                    std::path::Path::new("synthetic-late-device-alignment"),
                ));
            }
            if std::env::args().any(|a| a == "--blocked-alignment") {
                controller.state.alignment = Some(support::alignment(&controller, false));
            }
            if std::env::args().any(|a| a == "--missing-target-review") {
                controller.state.review = Some(support::missing_target_review());
            }
            if std::env::args().any(|a| a == "--batch-progress") {
                let outputs: Vec<_> = controller
                    .state
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .outputs
                    .iter()
                    .filter(|o| !o.muted)
                    .map(|o| o.id.clone())
                    .collect();
                let signal = controller.settings.sweep;
                controller.state.job = Some(companion_service::controller::Job::Batch {
                    output_ids: outputs.clone(),
                    current: 0,
                });
                controller.state.progress =
                    Some(companion_service::measurement::MeasureProgress::Playing(
                        omniphony_adapter::Progress {
                            signal_id: 1,
                            output_id: outputs[0].clone(),
                            reference_output_id: Some(outputs[0].clone()),
                            sequence_outputs: outputs.clone(),
                            signal,
                            state: omniphony_adapter::SignalState::Rendering,
                            rendered_frames: 180000,
                            total_frames: signal.frames(48000) * (outputs.len() as u64 * 2 + 1),
                        },
                    ));
            }
            if std::env::args().any(|a| a == "--level-matching") {
                // A saved reference, then synthetic noise on Center: the level band below
                // the cards moves with the preview meter.
                let context = controller.manual_context(-30.0).unwrap();
                controller.state.manual_reference =
                    Some(companion_service::manual_level::Reference {
                        context,
                        level: calibration_core::manual_level::estimate(0.0101, 0.0001, 0.0)
                            .unwrap(),
                    });
                controller.start_manual_levels("Center", -30.0, false);
            }
            let mut app = companion_ui::CompanionApp::preview(&cc.egui_ctx, controller);
            app.lab = std::env::args().any(|a| a == "--lab");
            for arg in std::env::args() {
                app.result_plot = match arg.as_str() {
                    "--frequency-response" => companion_ui::ResultPlot::FrequencyResponse,
                    "--energy-arrival" => companion_ui::ResultPlot::EnergyArrival,
                    "--group-delay" => companion_ui::ResultPlot::GroupDelay,
                    "--signed-waveform" => companion_ui::ResultPlot::SignedWaveform,
                    "--tone-burst" => companion_ui::ResultPlot::ToneBurst,
                    _ => app.result_plot,
                };
            }
            Ok(Box::new(app))
        }),
    )
}
