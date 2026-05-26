use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use walkdir::WalkDir;

use crate::extract::{analyze_text_stats, classify_pdf};

use super::App;
use super::types::{AnalysisCacheEntry, PdfInfo, WorkerMsg};

impl App {
    pub fn pick_pdfs(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_files()
        {
            self.source_label = t!("gui.selected_pdfs", count = paths.len()).to_string();
            if self.output_dir.is_none() {
                self.output_dir = Self::default_output_dir_from_inputs(&paths);
            }
            self.scan_paths(paths);
        }
    }

    pub fn pick_directory(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.source_label = t!("gui.directory_recursive", path = dir.display()).to_string();
            self.output_dir = Some(dir.clone());
            self.scan_paths(vec![dir]);
        }
    }

    pub fn pick_output_directory(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = &self.output_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.output_dir = Some(dir);
        }
    }

    fn default_output_dir_from_inputs(inputs: &[PathBuf]) -> Option<PathBuf> {
        let first = inputs.first()?;
        if first.is_dir() {
            Some(first.clone())
        } else {
            first.parent().map(Path::to_path_buf)
        }
    }

    pub fn scan_paths(&mut self, inputs: Vec<PathBuf>) {
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
        self.logs.push(t!("gui.scanning_input", source = self.source_label).to_string());

        let (tx, rx) = mpsc::channel();
        self.scan_tx = Some(tx.clone());
        self.scan_rx = Some(rx);
        let cache = self.analysis_cache.clone();

        thread::spawn(move || {
            let mut paths = Self::collect_pdfs_from_inputs(&inputs);
            paths.sort();
            let _ = tx.send(WorkerMsg::ScanLog(t!("gui.scan_complete", count = paths.len()).to_string()));

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
            classification: t!("gui.analyzing").to_string(),
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
            Err(_) => (t!("gui.cannot_read").to_string(), 0, 0),
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
}
