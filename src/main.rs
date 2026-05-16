#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod cli;
mod extract;
mod gui;
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
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        // CLI mode: re-attach to parent terminal so eprintln! works
        #[cfg(target_os = "windows")]
        unsafe {
            win32::AttachConsole(win32::ATTACH_PARENT_PROCESS);
        }
    } else {
        // No arguments → launch GUI (Windows subsystem, no console window)
        launch_gui();
        return;
    }

    let cli_args = cli::Cli::parse();
    if let Err(e) = cli::run(&cli_args) {
        eprintln!("错误: {}", e);
        std::process::exit(1);
    }
}

fn launch_gui() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 500.0])
            .with_title("PDF 批量切割工具"),
        ..Default::default()
    };

    let result = eframe::run_native(
        "pdf-splitter",
        options,
        Box::new(|cc| Ok(Box::new(gui::App::new(cc)))),
    );

    if let Err(e) = result {
        eprintln!("GUI 启动失败: {}", e);
        std::process::exit(1);
    }
}
