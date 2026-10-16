//! Synthetic UI fixture. No live discovery, microphone, playback or file writes.
#[path = "../tests/support/mod.rs"]
mod support;

fn main() -> eframe::Result {
    eframe::run_native(
        "Omniphony Companion · UI preview · No audio",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1160.0, 800.0])
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
                ]
                .contains(&a.as_str())
            }) {
                controller.state.review = Some(support::arrival_review());
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
            let mut app = companion_ui::CompanionApp::preview(&cc.egui_ctx, controller);
            for arg in std::env::args() {
                app.result_plot = match arg.as_str() {
                    "--frequency-response" => companion_ui::ResultPlot::FrequencyResponse,
                    "--energy-arrival" => companion_ui::ResultPlot::EnergyArrival,
                    "--group-delay" => companion_ui::ResultPlot::GroupDelay,
                    _ => app.result_plot,
                };
            }
            Ok(Box::new(app))
        }),
    )
}
