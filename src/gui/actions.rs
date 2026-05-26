use super::App;
use super::types::{Operation, WorkerMsg};

impl App {
    pub fn process_messages(&mut self) {
        let mut finished = false;
        let mut stopped = false;
        let mut close_scan_channel = false;
        if let Some(rx) = &self.scan_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerMsg::ScanLog(text) => self.logs.push(text),
                    WorkerMsg::ScanFinished { scan_id, files } => {
                        if scan_id != self.scan_id {
                            continue;
                        }
                        let total = files.len();
                        self.file_index = files.iter().enumerate().map(|(i, f)| (f.path.clone(), i)).collect();
                        self.files = files;
                        self.analysis_total = total;
                        self.analyzed_count = 0;
                        self.scanning = total > 0;
                        self.logs.push(t!("gui.scan_count", source = self.source_label, total = total).to_string());
                        if total == 0 {
                            self.scanning = false;
                        }
                    }
                    WorkerMsg::AnalysisUpdated {
                        scan_id,
                        path,
                        modified,
                        classification,
                        total_chars,
                        total_pages,
                    } => {
                        if scan_id != self.scan_id {
                            continue;
                        }
                        if let Some(&idx) = self.file_index.get(&path) {
                            if let Some(file) = self.files.get_mut(idx) {
                                let was_analyzed = file.analyzed;
                                file.classification = classification.clone();
                                file.total_chars = total_chars;
                                file.total_pages = total_pages;
                                file.analyzed = true;
                                self.analysis_cache.insert(
                                    path.clone(),
                                    super::types::AnalysisCacheEntry {
                                        modified,
                                        classification,
                                        total_chars,
                                        total_pages,
                                    },
                                );
                                if !was_analyzed {
                                    self.analyzed_count += 1;
                                    if self.analyzed_count >= self.analysis_total {
                                        self.scanning = false;
                                        self.logs.push(t!("gui.analysis_complete").to_string());
                                        close_scan_channel = true;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if close_scan_channel {
            self.scan_tx = None;
            self.scan_rx = None;
        }
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerMsg::Log(text) => self.logs.push(text),
                    WorkerMsg::FileStarted { index, total, name } => {
                        self.current_file_index = index;
                        self.total_files_to_process = total;
                        self.current_file_name = name;
                        self.current_file_page = 0;
                        self.current_file_total_pages = 0;
                    }
                    WorkerMsg::FileProgress {
                        current_page,
                        total_pages,
                    } => {
                        self.current_file_page = current_page;
                        self.current_file_total_pages = total_pages;
                        let done_files = self.current_file_index.saturating_sub(1) as f32;
                        let file_fraction = if total_pages == 0 {
                            0.0
                        } else {
                            current_page as f32 / total_pages as f32
                        };
                        let total_files = self.total_files_to_process.max(1) as f32;
                        self.total_progress = ((done_files + file_fraction) / total_files).clamp(0.0, 1.0);
                    }
                    WorkerMsg::FileDone { index, total } => {
                        self.current_file_index = index;
                        self.total_files_to_process = total;
                        self.current_file_page = self.current_file_total_pages;
                        self.total_progress = (index as f32 / total.max(1) as f32).clamp(0.0, 1.0);
                    }
                    WorkerMsg::Finished => {
                        finished = true;
                    }
                    WorkerMsg::Stopped => {
                        stopped = true;
                    }
                    WorkerMsg::ScanLog(_)
                    | WorkerMsg::ScanFinished { .. }
                    | WorkerMsg::AnalysisUpdated { .. } => {}
                }
            }
        }
        if finished {
            self.running = false;
            self.paused = false;
            self.control = None;
            self.total_progress = 1.0;
            self.show_delete_confirm = self.operation == Operation::Split
                && self.delete_after
                && self.selected_count() > 0;
        }
        if stopped {
            self.running = false;
            self.paused = false;
            self.control = None;
        }
    }

    pub fn delete_originals(&mut self) {
        let mut deleted = 0;
        for f in self.files.iter().filter(|f| f.selected) {
            if f.path.exists() {
                if let Err(e) = std::fs::remove_file(&f.path) {
                    self.logs.push(t!("gui.delete_failed", path = f.path.display(), error = e).to_string());
                } else {
                    self.logs.push(t!("gui.deleted_file", path = f.path.display()).to_string());
                    deleted += 1;
                }
            }
        }
        self.logs.push(t!("gui.total_deleted", count = deleted).to_string());
        self.files.retain(|f| !f.selected);
        self.file_index = self.files.iter().enumerate().map(|(i, f)| (f.path.clone(), i)).collect();
    }

    pub fn selected_count(&self) -> usize {
        self.files.iter().filter(|f| f.selected).count()
    }

    pub fn select_all(&mut self) {
        for f in &mut self.files {
            f.selected = true;
        }
    }

    pub fn invert_selection(&mut self) {
        for f in &mut self.files {
            f.selected = !f.selected;
        }
    }

    pub fn current_file_progress(&self) -> f32 {
        if self.current_file_total_pages == 0 {
            0.0
        } else {
            (self.current_file_page as f32 / self.current_file_total_pages as f32).clamp(0.0, 1.0)
        }
    }
}
