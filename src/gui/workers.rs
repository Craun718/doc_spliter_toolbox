use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use crate::images;
use crate::split::{self, SplitControl};

use super::App;
use super::types::{Operation, SplitMode, WorkerMsg};

impl App {
    pub fn start_split(&mut self) {
        if self.files.is_empty() || self.running {
            return;
        }

        let selected_files: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|f| f.selected)
            .map(|f| f.path.clone())
            .collect();

        if selected_files.is_empty() {
            self.logs.push(t!("gui.no_pdf_selected").to_string());
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.tx = Some(tx.clone());
        self.rx = Some(rx);
        self.running = true;
        self.paused = false;
        self.operation = Operation::Split;
        self.total_progress = 0.0;
        self.current_file_name.clear();
        self.current_file_index = 0;
        self.total_files_to_process = selected_files.len();
        self.current_file_page = 0;
        self.current_file_total_pages = 0;

        let control = Arc::new(SplitControl::new());
        self.control = Some(control.clone());

        let files = selected_files;
        let output_dir = self.output_dir.clone();
        let max_size = self.max_size_mb * 1024 * 1024;
        let pages_per_chunk = self.pages_per_chunk;
        let split_mode = self.split_mode;

        thread::spawn(move || {
            let total = files.len();
            for (idx, fpath) in files.iter().enumerate() {
                // 检查是否已停止
                if control.is_stopped() {
                    let _ = tx.send(WorkerMsg::Log(t!("gui.stopped").to_string()));
                    let _ = tx.send(WorkerMsg::Stopped);
                    return;
                }

                let fname = fpath
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let _ = tx.send(WorkerMsg::FileStarted {
                    index: idx + 1,
                    total,
                    name: fname.clone(),
                });
                let _ = tx.send(WorkerMsg::Log(t!("cli.splitting", current = idx + 1, total = total, name = fname).to_string()));

                let tx_clone = tx.clone();
                let ctrl_clone = control.clone();
                let progress_tx = tx.clone();
                let split_result = match split_mode {
                    SplitMode::BySize => split::split_by_size_with_callback(
                        fpath,
                        output_dir.as_deref(),
                        max_size,
                        &ctrl_clone,
                        move |msg| {
                            let _ = tx_clone.send(WorkerMsg::Log(msg.to_string()));
                        },
                        move |current_page, total_pages| {
                            let _ = progress_tx.send(WorkerMsg::FileProgress {
                                current_page,
                                total_pages,
                            });
                        },
                    ),
                    SplitMode::ByPages => split::split_by_page_count_with_callback(
                        fpath,
                        output_dir.as_deref(),
                        pages_per_chunk,
                        &ctrl_clone,
                        move |msg| {
                            let _ = tx_clone.send(WorkerMsg::Log(msg.to_string()));
                        },
                        move |current_page, total_pages| {
                            let _ = progress_tx.send(WorkerMsg::FileProgress {
                                current_page,
                                total_pages,
                            });
                        },
                    ),
                };
                match split_result {
                    Ok(outputs) => {
                        let _ = tx.send(WorkerMsg::Log(t!("cli.generated", count = outputs.len()).to_string()));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::Log(t!("cli.error", msg = e).to_string()));
                    }
                }

                let _ = tx.send(WorkerMsg::FileDone {
                    index: idx + 1,
                    total,
                });

                // 文件级停止检查
                if control.is_stopped() {
                    let _ = tx.send(WorkerMsg::Log(t!("gui.stopped").to_string()));
                    let _ = tx.send(WorkerMsg::Stopped);
                    return;
                }
            }

            let _ = tx.send(WorkerMsg::Log(t!("gui.all_done").to_string()));
            let _ = tx.send(WorkerMsg::Finished);
        });
    }

    pub fn start_extract_images(&mut self) {
        if self.files.is_empty() || self.running {
            return;
        }

        let selected_files: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|f| f.selected)
            .map(|f| f.path.clone())
            .collect();

        if selected_files.is_empty() {
            self.logs.push(t!("gui.no_pdf_selected").to_string());
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.tx = Some(tx.clone());
        self.rx = Some(rx);
        self.running = true;
        self.paused = false;
        self.operation = Operation::ExtractImages;
        self.total_progress = 0.0;
        self.current_file_name.clear();
        self.current_file_index = 0;
        self.total_files_to_process = selected_files.len();
        self.current_file_page = 0;
        self.current_file_total_pages = 0;

        let control = Arc::new(SplitControl::new());
        self.control = Some(control.clone());

        let files = selected_files;
        let output_dir = self.output_dir.clone();

        thread::spawn(move || {
            let total = files.len();
            for (idx, fpath) in files.iter().enumerate() {
                if control.is_stopped() {
                    let _ = tx.send(WorkerMsg::Log(t!("gui.stopped").to_string()));
                    let _ = tx.send(WorkerMsg::Stopped);
                    return;
                }

                let fname = fpath
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let _ = tx.send(WorkerMsg::FileStarted {
                    index: idx + 1,
                    total,
                    name: fname.clone(),
                });
                let _ = tx.send(WorkerMsg::Log(t!("gui.extracting_images", name = fname).to_string()));

                let tx_log = tx.clone();
                let tx_prog = tx.clone();
                let ctrl_clone = control.clone();
                let result = images::extract_images_with_callback(
                    fpath,
                    output_dir.as_deref(),
                    &ctrl_clone,
                    move |msg| {
                        let _ = tx_log.send(WorkerMsg::Log(msg.to_string()));
                    },
                    move |current_page, total_pages| {
                        let _ = tx_prog.send(WorkerMsg::FileProgress {
                            current_page,
                            total_pages,
                        });
                    },
                );
                match result {
                    Ok(outputs) => {
                        let _ = tx.send(WorkerMsg::Log(t!("gui.extracted_count", count = outputs.len()).to_string()));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::Log(t!("cli.error", msg = e).to_string()));
                    }
                }

                let _ = tx.send(WorkerMsg::FileDone {
                    index: idx + 1,
                    total,
                });

                if control.is_stopped() {
                    let _ = tx.send(WorkerMsg::Log(t!("gui.stopped").to_string()));
                    let _ = tx.send(WorkerMsg::Stopped);
                    return;
                }
            }

            let _ = tx.send(WorkerMsg::Log(t!("gui.all_done").to_string()));
            let _ = tx.send(WorkerMsg::Finished);
        });
    }
}
