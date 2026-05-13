use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use anyhow::{bail, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

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
///
/// Pre-measures each page's byte size (parallel), then determines chunk
/// boundaries by prefix sum — O(n) instead of O(n·chunk_size) serializations.
pub fn split_by_size(
    pdf_path: &Path,
    output_dir: Option<&Path>,
    max_size: u64,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let doc = lopdf::Document::load(pdf_path)?;
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = output_dir.unwrap_or_else(|| pdf_path.parent().unwrap_or_else(|| Path::new(".")));

    let total_pages = doc.get_pages().len();
    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    let text_stats = analyze_text_stats(&doc);
    let pdf_type = classify_pdf(text_stats.avg_chars_per_page);

    // Phase 1: measure individual page sizes (parallel)
    let pb = if !quiet {
        let pb = ProgressBar::new(total_pages as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:40.cyan/dim}] {pos}/{len} 页")
                .unwrap()
                .progress_chars("█▓░"),
        );
        pb.set_message(format!("测量 {}", pdf_path.file_name().unwrap_or_default().to_string_lossy()));
        pb
    } else {
        ProgressBar::hidden()
    };

    let page_sizes: Vec<u64> = (0..total_pages)
        .into_par_iter()
        .map(|i| {
            extract_pages_to_bytes(&doc, &[i])
                .map(|b| b.len() as u64)
                .unwrap_or(0)
        })
        .collect();

    // Phase 2: determine chunk boundaries by prefix sum
    let chunks = compute_chunks(&page_sizes, max_size);

    // Phase 3: save chunks
    pb.set_message(format!("保存 {}", pdf_path.file_name().unwrap_or_default().to_string_lossy()));
    pb.set_length(chunks.len() as u64);
    pb.reset();

    let mut part = 1u32;
    let mut output_files = Vec::new();
    for chunk in &chunks {
        let out_path = save_chunk(&doc, parent, stem, chunk, part)?;
        print_chunk_info(&out_path, chunk, pdf_type, text_stats.avg_chars_per_page);
        output_files.push(out_path);
        part += 1;
        pb.inc(1);
    }

    pb.finish_with_message("完成");
    Ok(output_files)
}

/// Like `split_by_size`, but sends log messages and page progress to callbacks (for GUI).
///
/// Pre-measures each page's byte size sequentially (with stop/pause checks),
/// then determines chunk boundaries by prefix sum.
pub fn split_by_size_with_callback<F: FnMut(&str), P: FnMut(usize, usize)>(
    pdf_path: &Path,
    output_dir: Option<&Path>,
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
    let parent = output_dir.unwrap_or_else(|| pdf_path.parent().unwrap_or_else(|| Path::new(".")));

    let total_pages = doc.get_pages().len();
    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    let text_stats = analyze_text_stats(&doc);
    let pdf_type = classify_pdf(text_stats.avg_chars_per_page);

    // Phase 1: measure page sizes sequentially (with stop/pause support)
    progress(0, total_pages);
    let mut page_sizes = Vec::with_capacity(total_pages);
    let mut stopped = false;
    for i in 0..total_pages {
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            stopped = true;
            break;
        }
        control.wait_if_paused();
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            stopped = true;
            break;
        }
        let bytes = extract_pages_to_bytes(&doc, &[i])?;
        page_sizes.push(bytes.len() as u64);
        progress(i + 1, total_pages);
    }
    if stopped {
        return Ok(Vec::new());
    }

    // Phase 2: determine chunk boundaries by prefix sum
    let chunks = compute_chunks(&page_sizes, max_size);

    // Phase 3: save chunks
    let mut part = 1u32;
    let mut output_files = Vec::new();
    for chunk in &chunks {
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }
        control.wait_if_paused();
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        let out_path = save_chunk(&doc, parent, stem, chunk, part)?;
        let info = chunk_info_str(&out_path, chunk, pdf_type, text_stats.avg_chars_per_page);
        log(&info);
        output_files.push(out_path);
        part += 1;
    }

    progress(total_pages, total_pages);

    Ok(output_files)
}

/// Split a PDF into chunks with at most `pages_per_chunk` pages.
pub fn split_by_page_count(
    pdf_path: &Path,
    output_dir: Option<&Path>,
    pages_per_chunk: usize,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let doc = lopdf::Document::load(pdf_path)?;
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = output_dir.unwrap_or_else(|| pdf_path.parent().unwrap_or_else(|| Path::new(".")));

    let pages: Vec<(u32, lopdf::ObjectId)> = doc.get_pages().into_iter().collect();
    let total_pages = pages.len();

    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    if pages_per_chunk == 0 {
        bail!("pages_per_chunk must be greater than 0");
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
        chunk.push(i);

        if chunk.len() < pages_per_chunk && i + 1 < total_pages {
            continue;
        }

        let out_path = save_chunk(&doc, parent, stem, &chunk, part)?;
        print_chunk_info(&out_path, &chunk, pdf_type, text_stats.avg_chars_per_page);
        output_files.push(out_path);
        part += 1;
        chunk.clear();
    }

    pb.finish_with_message("完成");
    Ok(output_files)
}

/// Like `split_by_page_count`, but sends log messages and page progress to callbacks (for GUI).
pub fn split_by_page_count_with_callback<F: FnMut(&str), P: FnMut(usize, usize)>(
    pdf_path: &Path,
    output_dir: Option<&Path>,
    pages_per_chunk: usize,
    control: &SplitControl,
    mut log: F,
    mut progress: P,
) -> Result<Vec<PathBuf>> {
    let doc = lopdf::Document::load(pdf_path)?;
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = output_dir.unwrap_or_else(|| pdf_path.parent().unwrap_or_else(|| Path::new(".")));

    let pages: Vec<(u32, lopdf::ObjectId)> = doc.get_pages().into_iter().collect();
    let total_pages = pages.len();

    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    if pages_per_chunk == 0 {
        bail!("pages_per_chunk must be greater than 0");
    }

    let text_stats = analyze_text_stats(&doc);
    let pdf_type = classify_pdf(text_stats.avg_chars_per_page);
    progress(0, total_pages);

    let mut chunk: Vec<usize> = Vec::new();
    let mut part = 1u32;
    let mut output_files = Vec::new();

    for i in 0..total_pages {
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        control.wait_if_paused();

        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        progress(i + 1, total_pages);
        chunk.push(i);

        if chunk.len() < pages_per_chunk && i + 1 < total_pages {
            continue;
        }

        let out_path = save_chunk(&doc, parent, stem, &chunk, part)?;
        let info = chunk_info_str(&out_path, &chunk, pdf_type, text_stats.avg_chars_per_page);
        log(&info);
        output_files.push(out_path);
        part += 1;
        chunk.clear();
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

/// Given individual page byte sizes and a max chunk size, partition page
/// indices into chunks using a greedy prefix-sum algorithm.
fn compute_chunks(page_sizes: &[u64], max_size: u64) -> Vec<Vec<usize>> {
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_size: u64 = 0;

    for (i, &size) in page_sizes.iter().enumerate() {
        if current_size + size <= max_size {
            current.push(i);
            current_size += size;
        } else {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_size = 0;
            }

            if size > max_size {
                // Single page exceeds limit — save as its own chunk
                chunks.push(vec![i]);
            } else {
                current.push(i);
                current_size = size;
            }
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}
