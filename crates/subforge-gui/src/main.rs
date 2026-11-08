//! SubForge — subtitle editor.
//!
//! Usage: `subforge [SUBTITLE] [--video VIDEO]`
#![allow(deprecated)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cache;
mod dialogs;
mod editor;
mod fonts;
mod fonts_dlg;
mod grid;
mod icons;
mod layout;
mod loading;
mod menus;
mod motion;
mod ocr_dlg;
mod speech_dlg;
mod spell_dlg;
mod tools_dlg;
mod tracking;
mod typeset;
mod media;
mod timeline;
mod undo;
mod video_view;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut file: Option<PathBuf> = None;
    let mut video: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--video" | "-v" => video = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                println!("SubForge {}\nUsage: subforge [SUBTITLE] [--video VIDEO]\nCommand line tools: subforge-cli --help", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {
                let p = PathBuf::from(&a);
                if media::is_media_path(&p) && video.is_none() {
                    video = Some(p);
                } else {
                    file = Some(p);
                }
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(app::APP_NAME)
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(app::APP_NAME, options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, file, video)))))
}
