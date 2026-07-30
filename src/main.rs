//! TarDrop is a user-local, security-conscious installer for portable archives.
//!
//! The GUI stays deliberately small; the installer modules do all security-sensitive work.

#![forbid(unsafe_code)]

mod archive;
mod desktop;
mod icons;
mod installer;
mod security;
mod ui;
mod utils;

use ui::TarDropApp;

/// Starts the desktop application and reports failures to the terminal too.
fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([780.0, 560.0])
            .with_min_inner_size([600.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("TarDrop", options, Box::new(|cc| Ok(Box::new(TarDropApp::new(cc)))))
}
