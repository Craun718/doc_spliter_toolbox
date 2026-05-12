use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::SystemTime;

use egui_extras::{Column, TableBuilder};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use walkdir::WalkDir;

use crate::extract::{analyze_text_stats, classify_pdf, format_size};
use crate::split::{self, SplitControl};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitMode {
    BySize,
    ByPages,
}

pub struct PdfInfo {
    pub path: PathBuf,
    pub display_name: String,
    pub classification: String,
    pub total_chars: usize,
    pub total_pages: usize,
    pub file_size: u64,
    pub selected: bool,
    pub analyzed: bool,
}

#[derive(Clone)]
struct AnalysisCacheEntry {
    modified: Option<SystemTime>,
    classification: String,
    total_chars: usize,
    total_pages: usize,
}

/// Messages sent from worker thread to GUI
enum WorkerMsg {
    Log(String),
    ScanLog(String),
    ScanFinished {
        scan_id: u64,
        files: Vec<PdfInfo>,
    },
    AnalysisUpdated {
        scan_id: u64,
        path: PathBuf,
        modified: Option<SystemTime>,
        classification: String,
        total_chars: usize,
        total_pages: usize,
    },
    FileStarted {
        index: usize,
        total: usize,
        name: String,
    },
    FileProgress {
        current_page: usize,
        total_pages: usize,
    },
    FileDone {
        index: usize,
        total: usize,
    },
    Finished,
    Stopped,
}

pub struct App {
    source_label: String,
    files: Vec<PdfInfo>,
    logs: Vec<String>,
    max_size_mb: u64,
    pages_per_chunk: usize,
    split_mode: SplitMode,
    running: bool,
    paused: bool,
    delete_after: bool,
    scanning: bool,
    scan_id: u64,
    analysis_cache: HashMap<PathBuf, AnalysisCacheEntry>,
    file_index: HashMap<PathBuf, usize>,
    analysis_total: usize,
    analyzed_count: usize,
    total_progress: f32,
    current_file_name: String,
    current_file_index: usize,
    total_files_to_process: usize,
    current_file_page: usize,
    current_file_total_pages: usize,

    // Scan/analysis communication
    scan_tx: Option<mpsc::Sender<WorkerMsg>>,
    scan_rx: Option<mpsc::Receiver<WorkerMsg>>,

    // Split worker communication
    tx: Option<mpsc::Sender<WorkerMsg>>,
    rx: Option<mpsc::Receiver<WorkerMsg>>,
    control: Option<Arc<SplitControl>>,
    show_delete_confirm: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            source_label: String::new(),
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
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        load_chinese_font(&cc.egui_ctx);
        Self::default()
    }

    fn pick_pdfs(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_files()
        {
            self.source_label = format!("已选择 {} 个 PDF 文件", paths.len());
            self.scan_paths(paths);
        }
    }

    fn pick_directory(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.source_label = format!("目录: {}（递归）", dir.display());
            self.scan_paths(vec![dir]);
        }
    }

    fn scan_paths(&mut self, inputs: Vec<PathBuf>) {
        if inputs.is_empty() {
            return;
        }

        self.files.clear();
        self.logs.clear();
        self.scanning = true;
        self.scan_id = self.scan_id.wrapping_add(1);
        let scan_id = self.scan_id;
        self.analysis_total = 0;
        self.analyzed_count = 0;
        self.logs.push(format!(
            "正在扫描输入: {}",
            self.source_label
        ));

        let (tx, rx) = mpsc::channel();
        self.scan_tx = Some(tx.clone());
        self.scan_rx = Some(rx);
        let cache = self.analysis_cache.clone();

        thread::spawn(move || {
            let mut paths = Self::collect_pdfs_from_inputs(&inputs);
            paths.sort();
            let _ = tx.send(WorkerMsg::ScanLog(format!("扫描完成，找到 {} 个 PDF", paths.len())));

            let files: Vec<PdfInfo> = paths.iter().cloned().map(Self::build_pdf_stub).collect();
            let _ = tx.send(WorkerMsg::ScanFinished { scan_id, files });

            let (cached_paths, uncached_paths): (Vec<_>, Vec<_>) = paths
                .into_iter()
                .partition(|path| Self::cached_analysis(path, &cache).is_some());

            for path in cached_paths {
                if let Some((modified, classification, total_chars, total_pages)) = Self::cached_analysis(&path, &cache) {
                    let _ = tx.send(WorkerMsg::AnalysisUpdated {
                        scan_id,
                        path,
                        modified,
                        classification,
                        total_chars,
                        total_pages,
                    });
                }
            }

            let threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(4)
                .max(1);
            let pool = ThreadPoolBuilder::new().num_threads(threads).build();
            match pool {
                Ok(pool) => {
                    pool.install(|| {
                        uncached_paths.into_par_iter().for_each_with(tx.clone(), |tx, path| {
                            let info = Self::analyze_pdf(path);
                            let modified = Self::file_modified(&info.path);
                            let _ = tx.send(WorkerMsg::AnalysisUpdated {
                                scan_id,
                                path: info.path,
                                modified,
                                classification: info.classification,
                                total_chars: info.total_chars,
                                total_pages: info.total_pages,
                            });
                        });
                    });
                }
                Err(_) => {
                    for path in uncached_paths {
                        let info = Self::analyze_pdf(path);
                        let modified = Self::file_modified(&info.path);
                        let _ = tx.send(WorkerMsg::AnalysisUpdated {
                            scan_id,
                            path: info.path,
                            modified,
                            classification: info.classification,
                            total_chars: info.total_chars,
                            total_pages: info.total_pages,
                        });
                    }
                }
            }
        });
    }

    fn collect_pdfs_from_inputs(inputs: &[PathBuf]) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for input in inputs {
            if input.is_file() {
                if Self::is_pdf_candidate(input) {
                    files.push(input.clone());
                }
                continue;
            }
            if input.is_dir() {
                files.extend(
                    WalkDir::new(input)
                        .into_iter()
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_file())
                        .map(|entry| entry.into_path())
                        .filter(Self::is_pdf_candidate),
                );
            }
        }
        files.sort();
        files.dedup();
        files
    }

    fn is_pdf_candidate(path: &PathBuf) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false)
            && !path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .contains("_part")
    }

    fn build_pdf_stub(path: PathBuf) -> PdfInfo {
        let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        PdfInfo {
            display_name: Self::truncate_file_name(&path, 24),
            path,
            classification: "分析中...".to_string(),
            total_chars: 0,
            total_pages: 0,
            file_size,
            selected: true,
            analyzed: false,
        }
    }

    fn analyze_pdf(path: PathBuf) -> PdfInfo {
        let mut info = Self::build_pdf_stub(path.clone());
        let (classification, total_chars, total_pages) = match lopdf::Document::load(&path) {
            Ok(doc) => {
                let page_count = doc.get_pages().len();
                let stats = analyze_text_stats(&doc);
                (
                    classify_pdf(stats.avg_chars_per_page).to_string(),
                    stats.total_chars,
                    page_count,
                )
            }
            Err(_) => ("无法读取".to_string(), 0, 0),
        };

        info.classification = classification;
        info.total_chars = total_chars;
        info.total_pages = total_pages;
        info.analyzed = true;
        info
    }

    fn file_modified(path: &PathBuf) -> Option<SystemTime> {
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
    }

    fn truncate_file_name(path: &Path, max_stem_chars: usize) -> String {
        let full_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| !ext.is_empty());

        if stem.chars().count() <= max_stem_chars {
            return full_name;
        }

        let truncated_stem = stem.chars().take(max_stem_chars).collect::<String>();
        match ext {
            Some(ext) => format!("{}....{}", truncated_stem, ext),
            None => format!("{}...", truncated_stem),
        }
    }

    fn cached_analysis(
        path: &PathBuf,
        cache: &HashMap<PathBuf, AnalysisCacheEntry>,
    ) -> Option<(Option<SystemTime>, String, usize, usize)> {
        let modified = Self::file_modified(path);
        let entry = cache.get(path)?;
        if entry.modified == modified {
            Some((modified, entry.classification.clone(), entry.total_chars, entry.total_pages))
        } else {
            None
        }
    }

    fn start_split(&mut self) {
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
            self.logs.push("未选择任何 PDF 文件".to_string());
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.tx = Some(tx.clone());
        self.rx = Some(rx);
        self.running = true;
        self.paused = false;
        self.total_progress = 0.0;
        self.current_file_name.clear();
        self.current_file_index = 0;
        self.total_files_to_process = selected_files.len();
        self.current_file_page = 0;
        self.current_file_total_pages = 0;

        let control = Arc::new(SplitControl::new());
        self.control = Some(control.clone());

        let files = selected_files;
        let max_size = self.max_size_mb * 1024 * 1024;
        let pages_per_chunk = self.pages_per_chunk;
        let split_mode = self.split_mode;

        thread::spawn(move || {
            let total = files.len();
            for (idx, fpath) in files.iter().enumerate() {
                // 检查是否已停止
                if control.is_stopped() {
                    let _ = tx.send(WorkerMsg::Log("处理已停止".to_string()));
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
                let _ = tx.send(WorkerMsg::Log(format!("[{}/{}] 正在切割: {}", idx + 1, total, fname)));

                let tx_clone = tx.clone();
                let ctrl_clone = control.clone();
                let progress_tx = tx.clone();
                let split_result = match split_mode {
                    SplitMode::BySize => split::split_by_size_with_callback(
                        fpath,
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
                        let _ = tx.send(WorkerMsg::Log(format!("  → 生成 {} 个文件", outputs.len())));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::Log(format!("  错误: {}", e)));
                    }
                }

                let _ = tx.send(WorkerMsg::FileDone {
                    index: idx + 1,
                    total,
                });

                // 文件级停止检查
                if control.is_stopped() {
                    let _ = tx.send(WorkerMsg::Log("处理已停止".to_string()));
                    let _ = tx.send(WorkerMsg::Stopped);
                    return;
                }
            }

            let _ = tx.send(WorkerMsg::Log("全部完成！".to_string()));
            let _ = tx.send(WorkerMsg::Finished);
        });
    }

    fn process_messages(&mut self) {
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
                        self.logs.push(format!(
                            "{}，共 {} 个 PDF 文件",
                            self.source_label,
                            total
                        ));
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
                                    AnalysisCacheEntry {
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
                                        self.logs.push("PDF 分析完成".to_string());
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
            self.show_delete_confirm = self.delete_after && self.selected_count() > 0;
        }
        if stopped {
            self.running = false;
            self.paused = false;
            self.control = None;
        }
    }

    fn delete_originals(&mut self) {
        let mut deleted = 0;
        for f in self.files.iter().filter(|f| f.selected) {
            if f.path.exists() {
                if let Err(e) = std::fs::remove_file(&f.path) {
                    self.logs.push(format!("删除失败 {}: {}", f.path.display(), e));
                } else {
                    self.logs.push(format!("已删除: {}", f.path.display()));
                    deleted += 1;
                }
            }
        }
        self.logs.push(format!("共删除 {} 个原文件", deleted));
        self.files.retain(|f| !f.selected);
        self.file_index = self.files.iter().enumerate().map(|(i, f)| (f.path.clone(), i)).collect();
    }

    fn selected_count(&self) -> usize {
        self.files.iter().filter(|f| f.selected).count()
    }

    fn select_all(&mut self) {
        for f in &mut self.files {
            f.selected = true;
        }
    }

    fn invert_selection(&mut self) {
        for f in &mut self.files {
            f.selected = !f.selected;
        }
    }

    fn current_file_progress(&self) -> f32 {
        if self.current_file_total_pages == 0 {
            0.0
        } else {
            (self.current_file_page as f32 / self.current_file_total_pages as f32).clamp(0.0, 1.0)
        }
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

        egui::TopBottomPanel::top("controls_panel")
            .show_separator_line(true)
            .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("选择 PDF").clicked() && !self.running {
                    self.pick_pdfs();
                }

                if ui.button("选择目录").clicked() && !self.running {
                    self.pick_directory();
                }

                ui.add_enabled_ui(
                    !self.running && !self.files.is_empty() && self.selected_count() > 0,
                    |ui| {
                    if ui.button("开始切割").clicked() {
                        self.start_split();
                    }
                });

                ui.separator();

                if self.running {
                    ui.spinner();
                    if self.paused {
                        ui.label("已暂停");
                    } else {
                        ui.label("处理中");
                    }

                    if self.paused {
                        if ui.button("恢复").clicked() {
                            if let Some(ctrl) = &self.control {
                                ctrl.resume();
                                self.paused = false;
                                self.logs.push("已恢复处理".to_string());
                            }
                        }
                    } else {
                        if ui.button("暂停").clicked() {
                            if let Some(ctrl) = &self.control {
                                ctrl.pause();
                                self.paused = true;
                                self.logs.push("已暂停处理".to_string());
                            }
                        }
                    }

                    if ui.button("停止").clicked() {
                        if let Some(ctrl) = &self.control {
                            ctrl.stop();
                            self.paused = false;
                            self.logs.push("正在停止...".to_string());
                        }
                    }
                } else {
                    // 常态显示但禁用
                    ui.add_enabled_ui(false, |ui| {
                        let _ = ui.button("暂停");
                        let _ = ui.button("停止");
                    });
                }

                ui.separator();

                ui.label("切分方式:");
                ui.radio_value(&mut self.split_mode, SplitMode::BySize, "按大小");
                ui.radio_value(&mut self.split_mode, SplitMode::ByPages, "按页数");

                match self.split_mode {
                    SplitMode::BySize => {
                        ui.label("每块上限 (MB):");
                        ui.add(egui::DragValue::new(&mut self.max_size_mb).range(1..=1000));
                    }
                    SplitMode::ByPages => {
                        ui.label("每块页数:");
                        ui.add(egui::DragValue::new(&mut self.pages_per_chunk).range(1..=10000));
                    }
                }

                ui.checkbox(&mut self.delete_after, "切割后删除源文件");
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
                        .text(format!(
                            "扫描和分析中... {} / {}",
                            self.analyzed_count,
                            self.analysis_total
                        )),
                );
            }
            if self.running {
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(self.total_progress)
                        .show_percentage()
                        .text(format!(
                            "总进度 {}/{}",
                            self.current_file_index.min(self.total_files_to_process),
                            self.total_files_to_process
                        )),
                );
                if !self.current_file_name.is_empty() {
                    ui.add(
                        egui::ProgressBar::new(self.current_file_progress())
                            .show_percentage()
                            .text(if self.current_file_total_pages > 0 {
                                format!(
                                    "当前文件进度 {}/{}",
                                    self.current_file_page,
                                    self.current_file_total_pages
                                )
                            } else {
                                "当前文件进度".to_string()
                            }),
                    );
                    ui.label(format!("当前文件: {}", self.current_file_name));
                }
            }
            });

        egui::SidePanel::left("files_panel")
            .resizable(true)
            .default_width(360.0)
            .min_width(240.0)
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.heading(format!(
                    "PDF 文件 (已选 {} / 共 {})",
                    self.selected_count(),
                    self.files.len()
                ));
                if !self.files.is_empty() {
                    ui.label("只会切割勾选的文件");
                }
                ui.add_enabled_ui(!self.running && !self.files.is_empty(), |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("全选").clicked() {
                            self.select_all();
                        }
                        if ui.button("反选").clicked() {
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
                                header.col(|ui| { ui.strong("选择"); });
                                header.col(|ui| { ui.strong("大小"); });
                                header.col(|ui| { ui.strong("页数"); });
                                header.col(|ui| { ui.strong("文件名（含后缀）"); });
                                header.col(|ui| { ui.strong("分类"); });
                                header.col(|ui| { ui.strong("总字数"); });
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
            ui.heading("日志");
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
            egui::Window::new("确认删除")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("切割完成！是否删除源 PDF 文件？");
                    ui.horizontal(|ui| {
                        if ui.button("是").clicked() {
                            self.delete_originals();
                            self.show_delete_confirm = false;
                        }
                        if ui.button("否").clicked() {
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
