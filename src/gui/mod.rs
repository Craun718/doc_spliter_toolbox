mod types;
mod scan;
mod workers;
mod actions;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use egui_extras::{Column, TableBuilder};

use crate::extract::format_size;
use crate::split::SplitControl;

use types::*;

pub struct App {
    pub(crate) source_label: String,
    pub(crate) output_dir: Option<PathBuf>,
    pub(crate) files: Vec<PdfInfo>,
    pub(crate) logs: Vec<String>,
    pub(crate) max_size_mb: u64,
    pub(crate) pages_per_chunk: usize,
    pub(crate) split_mode: SplitMode,
    pub(crate) running: bool,
    pub(crate) paused: bool,
    pub(crate) delete_after: bool,
    pub(crate) scanning: bool,
    pub(crate) scan_id: u64,
    pub(crate) analysis_cache: HashMap<PathBuf, AnalysisCacheEntry>,
    pub(crate) file_index: HashMap<PathBuf, usize>,
    pub(crate) analysis_total: usize,
    pub(crate) analyzed_count: usize,
    pub(crate) total_progress: f32,
    pub(crate) current_file_name: String,
    pub(crate) current_file_index: usize,
    pub(crate) total_files_to_process: usize,
    pub(crate) current_file_page: usize,
    pub(crate) current_file_total_pages: usize,

    // Scan/analysis communication
    pub(crate) scan_tx: Option<mpsc::Sender<WorkerMsg>>,
    pub(crate) scan_rx: Option<mpsc::Receiver<WorkerMsg>>,

    // Split worker communication
    pub(crate) tx: Option<mpsc::Sender<WorkerMsg>>,
    pub(crate) rx: Option<mpsc::Receiver<WorkerMsg>>,
    pub(crate) control: Option<Arc<SplitControl>>,
    pub(crate) show_delete_confirm: bool,
    pub(crate) operation: Operation,
}

impl Default for App {
    fn default() -> Self {
        Self {
            source_label: String::new(),
            output_dir: None,
            files: Vec::new(),
            logs: Vec::new(),
            max_size_mb: 50,
            pages_per_chunk: 100,
            split_mode: SplitMode::BySize,
            running: false,
            paused: false,
            delete_after: false,
            scanning: false,
            scan_id: 0,
            analysis_cache: HashMap::new(),
            file_index: HashMap::new(),
            analysis_total: 0,
            analyzed_count: 0,
            total_progress: 0.0,
            current_file_name: String::new(),
            current_file_index: 0,
            total_files_to_process: 0,
            current_file_page: 0,
            current_file_total_pages: 0,
            scan_tx: None,
            scan_rx: None,
            tx: None,
            rx: None,
            control: None,
            show_delete_confirm: false,
            operation: Operation::Split,
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        load_chinese_font(&cc.egui_ctx);
        Self::default()
    }
}

fn load_chinese_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Try to load Windows system Chinese fonts
    let font_paths: &[&str] = &[
        "C:/Windows/Fonts/msyh.ttc",    // Microsoft YaHei
        "C:/Windows/Fonts/msyhbd.ttc",  // Microsoft YaHei Bold
        "C:/Windows/Fonts/simsun.ttc",  // SimSun
    ];

    let mut loaded = false;
    for path in font_paths {
        if let Ok(data) = std::fs::read(path) {
            fonts
                .font_data
                .insert("chinese".to_owned(), egui::FontData::from_owned(data).into());
            loaded = true;
            break;
        }
    }

    if loaded {
        // Insert Chinese font at the beginning of both families
        if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            proportional.insert(0, "chinese".to_owned());
        }
        if let Some(monospace) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            monospace.insert(0, "chinese".to_owned());
        }
    }

    ctx.set_fonts(fonts);
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_messages();

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(t!("gui.menu_language"), |ui| {
                    if ui.button("English").clicked() {
                        rust_i18n::set_locale("en");
                        crate::i18n::save_locale("en");
                        ui.close_menu();
                    }
                    if ui.button("中文").clicked() {
                        rust_i18n::set_locale("zh");
                        crate::i18n::save_locale("zh");
                        ui.close_menu();
                    }
                });
            });
        });

        egui::TopBottomPanel::top("controls_panel")
            .show_separator_line(true)
            .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(t!("gui.select_pdf")).clicked() && !self.running {
                    self.pick_pdfs();
                }

                if ui.button(t!("gui.select_directory")).clicked() && !self.running {
                    self.pick_directory();
                }

                if ui.button(t!("gui.output_directory")).clicked() && !self.running {
                    self.pick_output_directory();
                }

                ui.add_enabled_ui(
                    !self.running && !self.files.is_empty() && self.selected_count() > 0,
                    |ui| {
                    if ui.button(t!("gui.start_split")).clicked() {
                        self.start_split();
                    }
                });

                ui.add_enabled_ui(
                    !self.running && !self.files.is_empty() && self.selected_count() > 0,
                    |ui| {
                    if ui.button(t!("gui.extract_images")).clicked() {
                        self.start_extract_images();
                    }
                });

                ui.separator();

                if self.running {
                    ui.spinner();
                    if self.paused {
                        ui.label(t!("gui.paused"));
                    } else {
                        ui.label(t!("gui.processing"));
                    }

                    if self.paused {
                        if ui.button(t!("gui.resume")).clicked() {
                            if let Some(ctrl) = &self.control {
                                ctrl.resume();
                                self.paused = false;
                                self.logs.push(t!("gui.resumed").to_string());
                            }
                        }
                    } else {
                        if ui.button(t!("gui.pause")).clicked() {
                            if let Some(ctrl) = &self.control {
                                ctrl.pause();
                                self.paused = true;
                                self.logs.push(t!("gui.paused_log").to_string());
                            }
                        }
                    }

                    if ui.button(t!("gui.stop")).clicked() {
                        if let Some(ctrl) = &self.control {
                            ctrl.stop();
                            self.paused = false;
                            self.logs.push(t!("gui.stopping").to_string());
                        }
                    }
                } else {
                    // 常态显示但禁用
                    ui.add_enabled_ui(false, |ui| {
                        let _ = ui.button(t!("gui.pause"));
                        let _ = ui.button(t!("gui.stop"));
                    });
                }

                ui.separator();

                ui.label(t!("gui.split_method"));
                ui.radio_value(&mut self.split_mode, SplitMode::BySize, t!("gui.by_size"));
                ui.radio_value(&mut self.split_mode, SplitMode::ByPages, t!("gui.by_pages"));

                match self.split_mode {
                    SplitMode::BySize => {
                        ui.label(t!("gui.max_chunk_mb"));
                        ui.add(egui::DragValue::new(&mut self.max_size_mb).range(1..=1000));
                    }
                    SplitMode::ByPages => {
                        ui.label(t!("gui.pages_per_chunk"));
                        ui.add(egui::DragValue::new(&mut self.pages_per_chunk).range(1..=10000));
                    }
                }

                ui.checkbox(&mut self.delete_after, t!("gui.delete_after_split"));
            });
            if self.scanning {
                ui.add_space(6.0);
                let scan_progress = if self.analysis_total > 0 {
                    self.analyzed_count as f32 / self.analysis_total as f32
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(scan_progress)
                        .show_percentage()
                        .text(t!("gui.scanning", current = self.analyzed_count, total = self.analysis_total)),
                );
            }
            if let Some(output_dir) = &self.output_dir {
                ui.add_space(6.0);
                ui.label(t!("gui.output_directory_label", path = output_dir.display()));
            }
            if self.running {
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(self.total_progress)
                        .show_percentage()
                        .text(t!("gui.status_total", current = self.current_file_index.min(self.total_files_to_process), total = self.total_files_to_process)),
                );
                if !self.current_file_name.is_empty() {
                    ui.add(
                        egui::ProgressBar::new(self.current_file_progress())
                            .show_percentage()
                            .text(if self.current_file_total_pages > 0 {
                                t!("gui.current_file_progress_detail", current = self.current_file_page, total = self.current_file_total_pages)
                            } else {
                                t!("gui.current_file_progress")
                            }),
                    );
                    ui.label(t!("gui.current_file", name = self.current_file_name));
                }
            }
            });

        egui::SidePanel::left("files_panel")
            .resizable(true)
            .default_width(360.0)
            .min_width(240.0)
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.heading(t!("gui.pdf_files", selected = self.selected_count(), total = self.files.len()));
                if !self.files.is_empty() {
                    ui.label(t!("gui.only_selected_split"));
                }
                ui.add_enabled_ui(!self.running && !self.files.is_empty(), |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(t!("gui.select_all")).clicked() {
                            self.select_all();
                        }
                        if ui.button(t!("gui.invert_selection")).clicked() {
                            self.invert_selection();
                        }
                    });
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("files")
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .striped(true)
                            .column(Column::auto().at_least(36.0))
                            .column(Column::auto().at_least(56.0))
                            .column(Column::auto().at_least(44.0))
                            .column(Column::remainder().at_least(120.0))
                            .column(Column::auto().at_least(60.0))
                            .column(Column::auto().at_least(80.0))
                            .header(20.0, |mut header| {
                                header.col(|ui| { ui.strong(t!("gui.header_select")); });
                                header.col(|ui| { ui.strong(t!("gui.header_size")); });
                                header.col(|ui| { ui.strong(t!("gui.header_pages")); });
                                header.col(|ui| { ui.strong(t!("gui.header_filename")); });
                                header.col(|ui| { ui.strong(t!("gui.header_classification")); });
                                header.col(|ui| { ui.strong(t!("gui.header_total_chars")); });
                            })
                            .body(|mut body| {
                                for f in &mut self.files {
                                    body.row(18.0, |mut row| {
                                        row.col(|ui| {
                                            ui.add_enabled(!self.running, egui::Checkbox::without_text(&mut f.selected));
                                        });
                                        row.col(|ui| {
                                            ui.label(format_size(f.file_size));
                                        });
                                        row.col(|ui| {
                                            if f.analyzed {
                                                ui.label(format!("{}", f.total_pages));
                                            } else {
                                                ui.label("...");
                                            }
                                        });
                                        row.col(|ui| {
                                            let response = ui.add(
                                                egui::Button::new(&f.display_name).selected(f.selected),
                                            );
                                            if response.clicked() && !self.running {
                                                f.selected = !f.selected;
                                            }
                                            response.on_hover_text(
                                                f.path.file_name().unwrap_or_default().to_string_lossy(),
                                            );
                                        });
                                        row.col(|ui| { ui.label(&f.classification); });
                                        row.col(|ui| {
                                            if f.analyzed {
                                                ui.label(format!("{}", f.total_chars));
                                            } else {
                                                ui.label("...");
                                            }
                                        });
                                    });
                                }
                            });
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(t!("gui.logs"));
            ui.separator();

            // 中央区独占剩余空间，窗口缩放时日志区不会被等分布局挤压。
            ui.allocate_ui_with_layout(
                ui.available_size(),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("logs")
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for log in &self.logs {
                            ui.monospace(log);
                        }
                    });
                },
            );
        });

        // Delete confirmation popup
        if self.show_delete_confirm {
            egui::Window::new(t!("gui.confirm_delete"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(t!("gui.delete_prompt"));
                    ui.horizontal(|ui| {
                        if ui.button(t!("gui.yes")).clicked() {
                            self.delete_originals();
                            self.show_delete_confirm = false;
                        }
                        if ui.button(t!("gui.no")).clicked() {
                            self.show_delete_confirm = false;
                        }
                    });
                });
        }

        // Keep requesting repaints while worker is running
        if self.running {
            ctx.request_repaint();
        }
    }
}
