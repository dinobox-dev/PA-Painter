#![windows_subsystem = "windows"]

mod gui;

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::init();

    let icon = {
        #[cfg(target_os = "macos")]
        let bytes = include_bytes!("../assets/icon_macos.png");
        #[cfg(not(target_os = "macos"))]
        let bytes = include_bytes!("../assets/icon_default.png");

        let img = image::load_from_memory(bytes)
            .expect("Failed to decode app icon")
            .into_rgba8();
        let (w, h) = img.dimensions();
        egui::IconData {
            rgba: img.into_raw(),
            width: w,
            height: h,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("PA Painter")
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "PA Painter",
        options,
        Box::new(|cc| Ok(Box::new(gui::PainterApp::new(cc)))),
    )
}
