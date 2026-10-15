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
            let (controller, _) = support::ready();
            Ok(Box::new(companion_ui::CompanionApp::preview(
                &cc.egui_ctx,
                controller,
            )))
        }),
    )
}
