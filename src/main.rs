// No console window for release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod week;
mod config;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("CleanWeather")
            .with_inner_size([460.0, 860.0])
            .with_min_inner_size([360.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("CleanWeather", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
