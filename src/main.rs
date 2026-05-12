mod cli;
mod extract;
mod gui;
mod split;

use clap::Parser;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // No arguments (or only program name) → launch GUI
    if args.len() <= 1 {
        launch_gui();
        return;
    }

    // Otherwise → CLI mode
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
