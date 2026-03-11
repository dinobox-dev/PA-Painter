mod gui;

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("PA Painter"),
        ..Default::default()
    };
    eframe::run_native(
        "PA Painter",
        options,
        Box::new(|cc| Ok(Box::new(gui::PainterApp::new(cc)))),
    )
}
