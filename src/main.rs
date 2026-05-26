#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[macro_use]
extern crate rust_i18n;

i18n!("locales", fallback = "zh");

mod cli;
mod extract;
mod gui;
mod i18n;
mod images;
mod split;

use clap::Parser;

#[cfg(target_os = "windows")]
mod win32 {
    pub const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn AttachConsole(dwProcessId: u32) -> i32;
    }
}

fn main() {
    // Check for --lang flag before init_locale for early language override
    let lang_override = parse_lang_arg();
    if let Some(lang) = lang_override {
        rust_i18n::set_locale(lang);
    } else {
        i18n::init_locale();
    }

    // Count non-lang args to decide CLI vs GUI mode
    let non_lang_arg_count = count_non_lang_args();
    if non_lang_arg_count > 1 {
        // CLI mode: re-attach to parent terminal so eprintln! works
        #[cfg(target_os = "windows")]
        unsafe {
            win32::AttachConsole(win32::ATTACH_PARENT_PROCESS);
        }
    } else {
        // No non-lang arguments → launch GUI (Windows subsystem, no console window)
        launch_gui();
        return;
    }

    let cli_args = cli::Cli::parse();
    if let Err(e) = cli::run(&cli_args) {
        eprintln!("{}", t!("app.error", msg = e));
        std::process::exit(1);
    }
}

fn parse_lang_arg() -> Option<&'static str> {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == "--lang" || args[i] == "-l" {
            if let Some(val) = args.get(i + 1) {
                match val.as_str() {
                    "zh" | "cn" | "chinese" => return Some("zh"),
                    "en" | "english" => return Some("en"),
                    _ => {}
                }
            }
        }
    }
    None
}

fn count_non_lang_args() -> usize {
    let args: Vec<String> = std::env::args().collect();
    let mut count = 0;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--lang" || args[i] == "-l" {
            i += 2; // skip flag and its value
        } else {
            count += 1;
            i += 1;
        }
    }
    count
}

fn launch_gui() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 500.0])
            .with_title(t!("app.title")),
        ..Default::default()
    };

    let result = eframe::run_native(
        "pdf-splitter",
        options,
        Box::new(|cc| Ok(Box::new(gui::App::new(cc)))),
    );

    if let Err(e) = result {
        eprintln!("{}", t!("app.gui_launch_failed", error = e));
        std::process::exit(1);
    }
}
