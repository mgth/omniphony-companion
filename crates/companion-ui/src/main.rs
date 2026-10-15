fn main() -> eframe::Result {
    eframe::run_native(
        "Omniphony Companion",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1160.0, 800.0])
                .with_min_inner_size([850.0, 640.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(companion_ui::CompanionApp::new(cc)))),
    )
}
