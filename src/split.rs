use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use anyhow::{bail, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::extract::{analyze_text_stats, classify_pdf, extract_pages_to_bytes, extract_pages_to_file, format_size};

/// 控制信号：用于暂停/恢复/停止切割任务
pub struct SplitControl {
    stopped: AtomicBool,
    paused: Mutex<bool>,
    pause_condvar: Condvar,
}

impl SplitControl {
    pub fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            paused: Mutex::new(false),
            pause_condvar: Condvar::new(),
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        // 如果在暂停中，也要唤醒等待线程让它退出
        let mut paused = self.paused.lock().unwrap();
        *paused = false;
        self.pause_condvar.notify_all();
    }

    pub fn pause(&self) {
        *self.paused.lock().unwrap() = true;
    }

    pub fn resume(&self) {
        let mut paused = self.paused.lock().unwrap();
        *paused = false;
        self.pause_condvar.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// 如果处于暂停状态，则阻塞直到恢复或停止
    pub fn wait_if_paused(&self) {
        let mut paused = self.paused.lock().unwrap();
        while *paused && !self.stopped.load(Ordering::Relaxed) {
            paused = self.pause_condvar.wait(paused).unwrap();
        }
    }
}

/// Split a PDF into chunks no larger than `max_size` bytes.
/// Returns the list of output file paths.
pub fn split_by_size(
    pdf_path: &Path,
    max_size: u64,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let doc = lopdf::Document::load(pdf_path)?;
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = pdf_path.parent().unwrap_or_else(|| Path::new("."));

    let pages: Vec<(u32, lopdf::ObjectId)> = doc.get_pages().into_iter().collect();
    let total_pages = pages.len();

    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    let text_stats = analyze_text_stats(&doc);
    let pdf_type = classify_pdf(text_stats.avg_chars_per_page);

    let pb = if !quiet {
        let pb = ProgressBar::new(total_pages as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:40.cyan/dim}] {pos}/{len} 页")
                .unwrap()
                .progress_chars("█▓░"),
        );
        pb.set_message(format!("切割 {}", pdf_path.file_name().unwrap_or_default().to_string_lossy()));
        pb
    } else {
        ProgressBar::hidden()
    };

    let mut chunk: Vec<usize> = Vec::new();
    let mut part = 1u32;
    let mut output_files = Vec::new();

    for i in 0..total_pages {
        pb.inc(1);

        let chunk_was_empty = chunk.is_empty();
        chunk.push(i);
        let candidate_size = extract_pages_to_bytes(&doc, &chunk)?.len() as u64;

        if candidate_size <= max_size {
            continue;
        }

        chunk.pop();

        // Current chunk is full; flush it
        if !chunk.is_empty() {
            let out_path = save_chunk(&doc, parent, stem, &chunk, part)?;
            print_chunk_info(&out_path, &chunk, pdf_type, text_stats.avg_chars_per_page);
            output_files.push(out_path);
            part += 1;
        }

        // Handle current page
        let single_size = if chunk_was_empty {
            candidate_size
        } else {
            extract_pages_to_bytes(&doc, &[i])?.len() as u64
        };
        if single_size > max_size {
            // Single page exceeds limit — save as-is
            let out_path = save_chunk(&doc, parent, stem, &[i], part)?;
            print_chunk_info(&out_path, &[i], pdf_type, text_stats.avg_chars_per_page);
            output_files.push(out_path);
            part += 1;
            chunk.clear();
        } else {
            chunk.clear();
            chunk.push(i);
        }
    }

    // Flush final chunk
    if !chunk.is_empty() {
        let out_path = save_chunk(&doc, parent, stem, &chunk, part)?;
        print_chunk_info(&out_path, &chunk, pdf_type, text_stats.avg_chars_per_page);
        output_files.push(out_path);
    }

    pb.finish_with_message("完成");
    Ok(output_files)
}

/// Like `split_by_size`, but sends log messages and page progress to callbacks (for GUI).
pub fn split_by_size_with_callback<F: FnMut(&str), P: FnMut(usize, usize)>(
    pdf_path: &Path,
    max_size: u64,
    control: &SplitControl,
    mut log: F,
    mut progress: P,
) -> Result<Vec<PathBuf>> {
    let doc = lopdf::Document::load(pdf_path)?;
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = pdf_path.parent().unwrap_or_else(|| Path::new("."));

    let pages: Vec<(u32, lopdf::ObjectId)> = doc.get_pages().into_iter().collect();
    let total_pages = pages.len();

    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    let text_stats = analyze_text_stats(&doc);
    let pdf_type = classify_pdf(text_stats.avg_chars_per_page);
    progress(0, total_pages);

    let mut chunk: Vec<usize> = Vec::new();
    let mut part = 1u32;
    let mut output_files = Vec::new();

    for i in 0..total_pages {
        // 检查停止信号
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        // 检查暂停信号，阻塞直到恢复
        control.wait_if_paused();

        // 再次检查停止（暂停恢复后可能立即停止）
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        progress(i + 1, total_pages);

        let chunk_was_empty = chunk.is_empty();
        chunk.push(i);
        let candidate_size = extract_pages_to_bytes(&doc, &chunk)?.len() as u64;

        if candidate_size <= max_size {
            continue;
        }

        chunk.pop();

        if !chunk.is_empty() {
            let out_path = save_chunk(&doc, parent, stem, &chunk, part)?;
            let info = chunk_info_str(&out_path, &chunk, pdf_type, text_stats.avg_chars_per_page);
            log(&info);
            output_files.push(out_path);
            part += 1;
        }

        let single_size = if chunk_was_empty {
            candidate_size
        } else {
            extract_pages_to_bytes(&doc, &[i])?.len() as u64
        };
        if single_size > max_size {
            let out_path = save_chunk(&doc, parent, stem, &[i], part)?;
            let info = chunk_info_str(&out_path, &[i], pdf_type, text_stats.avg_chars_per_page);
            log(&info);
            output_files.push(out_path);
            part += 1;
            chunk.clear();
        } else {
            chunk.clear();
            chunk.push(i);
        }
    }

    if !chunk.is_empty() {
        let out_path = save_chunk(&doc, parent, stem, &chunk, part)?;
        let info = chunk_info_str(&out_path, &chunk, pdf_type, text_stats.avg_chars_per_page);
        log(&info);
        output_files.push(out_path);
    }

    progress(total_pages, total_pages);

    Ok(output_files)
}

fn save_chunk(
    doc: &lopdf::Document,
    parent: &Path,
    stem: &str,
    chunk: &[usize],
    part: u32,
) -> Result<PathBuf> {
    let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
    extract_pages_to_file(doc, chunk, &out_path)?;
    Ok(out_path)
}

fn print_chunk_info(out_path: &Path, chunk: &[usize], pdf_type: &str, avg_chars: f64) {
    let size = std::fs::metadata(out_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let page_range = if chunk.len() > 1 {
        format!("第 {}-{} 页", chunk[0] + 1, chunk.last().unwrap() + 1)
    } else {
        format!("第 {} 页", chunk[0] + 1)
    };
    eprintln!(
        "  {} ({}, {}, {}, 平均{:.0}字/页)",
        out_path.file_name().unwrap_or_default().to_string_lossy(),
        page_range,
        format_size(size),
        pdf_type,
        avg_chars
    );
}

fn chunk_info_str(out_path: &Path, chunk: &[usize], pdf_type: &str, avg_chars: f64) -> String {
    let size = std::fs::metadata(out_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let page_range = if chunk.len() > 1 {
        format!("第 {}-{} 页", chunk[0] + 1, chunk.last().unwrap() + 1)
    } else {
        format!("第 {} 页", chunk[0] + 1)
    };
    format!(
        "  {} ({}, {}, {}, 平均{:.0}字/页)",
        out_path.file_name().unwrap_or_default().to_string_lossy(),
        page_range,
        format_size(size),
        pdf_type,
        avg_chars
    )
}
