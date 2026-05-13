use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use anyhow::{bail, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::extract::{classify_pdf, estimate_avg_chars_per_page, extract_pages_to_bytes, format_size};

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
    let avg_chars = estimate_avg_chars_per_page(&doc);
    let pdf_type = classify_pdf(avg_chars);

    // Phase 1: measure cumulative page sizes (front-deletion)
    // cumulative_sizes[i] = exact byte size of pages [i..N) saved together
    // cumulative_sizes[total_pages] = 0 (empty document)
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

    let mut cumulative_sizes = vec![0u64; total_pages + 1];
    {
        let mut working = doc.clone();
        let mut buf = Vec::new();
        working.save_to(&mut buf)?;
        cumulative_sizes[0] = buf.len() as u64;

        for i in 1..total_pages {
            working.delete_pages(&[1]);
            working.prune_objects();
            let mut buf = Vec::new();
            working.save_to(&mut buf)?;
            cumulative_sizes[i] = buf.len() as u64;
            pb.inc(1);
        }
    }
    pb.set_position(total_pages as u64);

    // Phase 2: exact serialization verification
    pb.set_message(format!("保存 {}", pdf_path.file_name().unwrap_or_default().to_string_lossy()));
    pb.reset();
    pb.set_length(total_pages as u64);

    let mut start = 0;
    let mut part = 1u32;
    let mut output_files = Vec::new();

    while start < total_pages {
        // Use cumulative to estimate a generous upper bound
        let mut end = start + 1;
        while end < total_pages && cumulative_sizes[start] - cumulative_sizes[end + 1] <= max_size {
            end += 1;
        }

        let chunk: Vec<usize> = if end <= start + 1 {
            vec![start]
        } else {
            (start..end).collect()
        };

        // Verify by actual serialization; binary search if too large
        let bytes = extract_pages_to_bytes(&doc, &chunk)?;
        if bytes.len() as u64 > max_size && chunk.len() > 1 {
            let fitting_end = find_fitting_end(&doc, start, end, max_size)?;
            let chunk: Vec<usize> = (start..fitting_end).collect();
            let bytes = extract_pages_to_bytes(&doc, &chunk)?;

            let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
            std::fs::write(&out_path, &bytes)?;
            print_chunk_info(&out_path, &chunk, pdf_type, avg_chars);
            output_files.push(out_path);
            start = fitting_end;
        } else {
            let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
            std::fs::write(&out_path, &bytes)?;
            print_chunk_info(&out_path, &chunk, pdf_type, avg_chars);
            output_files.push(out_path);
            start = end;
        }
        part += 1;
        pb.set_position(start as u64);
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
    let avg_chars = estimate_avg_chars_per_page(&doc);
    let pdf_type = classify_pdf(avg_chars);

    // Phase 1: measure cumulative page sizes (front-deletion, with stop/pause)
    progress(0, total_pages);
    let mut cumulative_sizes = vec![0u64; total_pages + 1];
    {
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            return Ok(Vec::new());
        }
        let mut working = doc.clone();
        control.wait_if_paused();
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            return Ok(Vec::new());
        }

        let mut buf = Vec::new();
        working.save_to(&mut buf)?;
        cumulative_sizes[0] = buf.len() as u64;
        progress(1, total_pages);

        for i in 1..total_pages {
            if control.is_stopped() {
                log("  已停止，保留已处理结果");
                return Ok(Vec::new());
            }
            control.wait_if_paused();
            if control.is_stopped() {
                log("  已停止，保留已处理结果");
                return Ok(Vec::new());
            }

            working.delete_pages(&[1]);
            working.prune_objects();
            let mut buf = Vec::new();
            working.save_to(&mut buf)?;
            cumulative_sizes[i] = buf.len() as u64;
            progress(i + 1, total_pages);
        }
    }

    // Phase 2: exact serialization verification
    let mut start = 0;
    let mut part = 1u32;
    let mut output_files = Vec::new();

    while start < total_pages {
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }
        control.wait_if_paused();
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        // Use cumulative to estimate a generous upper bound
        let mut end = start + 1;
        while end < total_pages && cumulative_sizes[start] - cumulative_sizes[end + 1] <= max_size {
            end += 1;
        }

        let chunk: Vec<usize> = if end <= start + 1 {
            vec![start]
        } else {
            (start..end).collect()
        };

        // Verify by actual serialization; binary search if too large
        let bytes = extract_pages_to_bytes(&doc, &chunk)?;
        if bytes.len() as u64 > max_size && chunk.len() > 1 {
            let fitting_end = find_fitting_end_with_control(&doc, start, end, max_size, control)?;
            let Some(fitting_end) = fitting_end else {
                log("  已停止，保留已处理结果");
                break;
            };

            let chunk: Vec<usize> = (start..fitting_end).collect();
            let bytes = extract_pages_to_bytes(&doc, &chunk)?;

            let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
            std::fs::write(&out_path, &bytes)?;
            let info = chunk_info_str(&out_path, &chunk, pdf_type, avg_chars);
            log(&info);
            output_files.push(out_path);
            start = fitting_end;
        } else {
            let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
            std::fs::write(&out_path, &bytes)?;
            let info = chunk_info_str(&out_path, &chunk, pdf_type, avg_chars);
            log(&info);
            output_files.push(out_path);
            start = end;
        }
        part += 1;
        progress(start, total_pages);
    }

    progress(total_pages, total_pages);

    Ok(output_files)
}

/// Split a PDF into chunks with at most `pages_per_chunk` pages.
///
/// Uses a single working document that progressively shrinks — avoids O(N²)
/// clone overhead by cloning progressively smaller documents instead of
/// repeatedly cloning the full source.
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

    let total_pages = doc.get_pages().len();

    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    if pages_per_chunk == 0 {
        bail!("pages_per_chunk must be greater than 0");
    }

    let avg_chars = estimate_avg_chars_per_page(&doc);
    let pdf_type = classify_pdf(avg_chars);

    // Pre-compute chunk page index ranges
    let chunks: Vec<Vec<usize>> = {
        let mut chunks = Vec::new();
        let mut chunk: Vec<usize> = Vec::new();
        for i in 0..total_pages {
            chunk.push(i);
            if chunk.len() < pages_per_chunk && i + 1 < total_pages {
                continue;
            }
            chunks.push(std::mem::take(&mut chunk));
        }
        chunks
    };

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

    // Single working document — gets smaller as chunks are removed
    let mut working = doc.clone();
    let mut part = 1u32;
    let mut output_files = Vec::new();
    let mut pages_done = 0;

    for chunk_indices in &chunks {
        let mut chunk_doc = working.clone();

        // Delete pages after this chunk from the clone
        let current_pages: Vec<u32> = chunk_doc.get_pages().into_iter().map(|(n, _)| n).collect();
        if chunk_indices.len() < current_pages.len() {
            let to_delete: Vec<u32> = current_pages[chunk_indices.len()..].to_vec();
            chunk_doc.delete_pages(&to_delete);
            chunk_doc.prune_objects();
        }

        let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
        chunk_doc.save(&out_path)?;
        print_chunk_info(&out_path, chunk_indices, pdf_type, avg_chars);
        output_files.push(out_path);

        pages_done += chunk_indices.len();
        pb.set_position(pages_done as u64);

        // Remove this chunk's pages from working doc
        let wc_pages: Vec<u32> = working.get_pages().into_iter().map(|(n, _)| n).collect();
        working.delete_pages(&wc_pages[..chunk_indices.len()]);
        working.prune_objects();

        part += 1;
    }

    pb.finish_with_message("完成");
    Ok(output_files)
}

/// Like `split_by_page_count`, but sends log messages and page progress to callbacks (for GUI).
///
/// Uses a single working document that progressively shrinks — avoids O(N²)
/// clone overhead by cloning progressively smaller documents instead of
/// repeatedly cloning the full source.
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

    let total_pages = doc.get_pages().len();

    if total_pages == 0 {
        bail!("PDF has no pages");
    }
    if pages_per_chunk == 0 {
        bail!("pages_per_chunk must be greater than 0");
    }

    let avg_chars = estimate_avg_chars_per_page(&doc);
    let pdf_type = classify_pdf(avg_chars);

    // Pre-compute chunk page index ranges
    let chunks: Vec<Vec<usize>> = {
        let mut chunks = Vec::new();
        let mut chunk: Vec<usize> = Vec::new();
        for i in 0..total_pages {
            chunk.push(i);
            if chunk.len() < pages_per_chunk && i + 1 < total_pages {
                continue;
            }
            chunks.push(std::mem::take(&mut chunk));
        }
        chunks
    };

    progress(0, total_pages);

    if control.is_stopped() {
        log("  已停止");
        return Ok(Vec::new());
    }

    // Single working document — gets smaller as chunks are removed
    let mut working = doc.clone();
    let mut part = 1u32;
    let mut output_files = Vec::new();
    let mut pages_done = 0;

    for chunk_indices in &chunks {
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }
        control.wait_if_paused();
        if control.is_stopped() {
            log("  已停止，保留已处理结果");
            break;
        }

        let mut chunk_doc = working.clone();

        // Delete pages after this chunk from the clone
        let current_pages: Vec<u32> = chunk_doc.get_pages().into_iter().map(|(n, _)| n).collect();
        if chunk_indices.len() < current_pages.len() {
            let to_delete: Vec<u32> = current_pages[chunk_indices.len()..].to_vec();
            chunk_doc.delete_pages(&to_delete);
            chunk_doc.prune_objects();
        }

        let out_path = parent.join(format!("{}_part{}.pdf", stem, part));
        chunk_doc.save(&out_path)?;
        let info = chunk_info_str(&out_path, chunk_indices, pdf_type, avg_chars);
        log(&info);
        output_files.push(out_path);

        pages_done += chunk_indices.len();
        progress(pages_done, total_pages);

        // Remove this chunk's pages from working doc
        let wc_pages: Vec<u32> = working.get_pages().into_iter().map(|(n, _)| n).collect();
        working.delete_pages(&wc_pages[..chunk_indices.len()]);
        working.prune_objects();

        part += 1;
    }

    progress(total_pages, total_pages);
    Ok(output_files)
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

/// Binary search for the largest end > start such that the actual
/// serialized size of pages [start..end) does not exceed `max_size`.
/// The initial `hi` is an upper bound derived from cumulative estimation.
fn find_fitting_end(
    doc: &lopdf::Document,
    start: usize,
    mut hi: usize,
    max_size: u64,
) -> Result<usize> {
    let mut lo = start + 1;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let chunk: Vec<usize> = (start..mid).collect();
        let bytes = extract_pages_to_bytes(doc, &chunk)?;
        if (bytes.len() as u64) <= max_size {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(lo)
}

/// Same as `find_fitting_end`, but checks `control.is_stopped()`
/// between binary search iterations and returns `None` if stopped.
fn find_fitting_end_with_control(
    doc: &lopdf::Document,
    start: usize,
    mut hi: usize,
    max_size: u64,
    control: &SplitControl,
) -> Result<Option<usize>> {
    let mut lo = start + 1;
    while lo < hi {
        if control.is_stopped() {
            return Ok(None);
        }
        control.wait_if_paused();
        if control.is_stopped() {
            return Ok(None);
        }

        let mid = (lo + hi + 1) / 2;
        let chunk: Vec<usize> = (start..mid).collect();
        let bytes = extract_pages_to_bytes(doc, &chunk)?;
        if (bytes.len() as u64) <= max_size {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(Some(lo))
}

/// Given cumulative page sizes (cumulative[i] = exact byte size of pages
/// [i..N) saved together), partition page indices into chunks using a
/// greedy algorithm with exact size checks.
#[allow(dead_code)]
fn compute_chunks_from_cumulative(cumulative: &[u64], max_size: u64) -> Vec<Vec<usize>> {
    let total = cumulative.len() - 1;
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut start = 0;

    for end in 0..total {
        let chunk_size = cumulative[start] - cumulative[end + 1];

        if chunk_size > max_size {
            if start == end {
                // Single page exceeds limit — save as its own chunk
                chunks.push(vec![start]);
                start = end + 1;
            } else {
                // Pages [start..end) fit, but adding page 'end' would exceed limit
                let mut chunk: Vec<usize> = (start..end).collect();
                chunks.push(std::mem::take(&mut chunk));
                start = end;
            }
        }
    }

    if start < total {
        chunks.push((start..total).collect());
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract_pages_to_file;

    fn create_test_doc(num_pages: usize) -> lopdf::Document {
        let mut doc = lopdf::Document::new();
        let font_id = doc.add_object(lopdf::Dictionary::from_iter(vec![
            ("Type", lopdf::Object::Name(b"Font".to_vec())),
            ("Subtype", lopdf::Object::Name(b"Type1".to_vec())),
            ("BaseFont", lopdf::Object::Name(b"Times-Roman".to_vec())),
        ]));

        let mut page_ids = Vec::new();
        for i in 0..num_pages {
            let text = format!("Page {} content with enough text to fill varying sizes ", i + 1).repeat(if i % 2 == 0 { 10 } else { 100 });
            let content_id = doc.add_object(lopdf::Stream::new(
                lopdf::Dictionary::from_iter(vec![
                    ("Length", lopdf::Object::Integer(text.len() as i64)),
                ]),
                text.into_bytes(),
            ));
            let page_id = doc.new_object_id();
            doc.objects.insert(page_id, lopdf::Object::Dictionary(
                lopdf::Dictionary::from_iter(vec![
                    ("Type", lopdf::Object::Name(b"Page".to_vec())),
                    ("MediaBox", lopdf::Object::Array(vec![
                        lopdf::Object::Integer(0), lopdf::Object::Integer(0),
                        lopdf::Object::Integer(612), lopdf::Object::Integer(792),
                    ])),
                    ("Contents", lopdf::Object::Reference(content_id)),
                    ("Resources", lopdf::Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                        ("Font", lopdf::Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                            ("F1", lopdf::Object::Reference(font_id)),
                        ]))),
                    ]))),
                ]),
            ));
            page_ids.push(page_id);
        }

        let kids: Vec<lopdf::Object> = page_ids.iter().map(|&id| lopdf::Object::Reference(id)).collect();
        let pages_id = doc.add_object(lopdf::Dictionary::from_iter(vec![
            ("Type", lopdf::Object::Name(b"Pages".to_vec())),
            ("Kids", lopdf::Object::Array(kids)),
            ("Count", lopdf::Object::Integer(page_ids.len() as i64)),
        ]));

        for &page_id in &page_ids {
            if let Some(lopdf::Object::Dictionary(ref mut dict)) = doc.objects.get_mut(&page_id) {
                dict.set("Parent", lopdf::Object::Reference(pages_id));
            }
        }

        let catalog_id = doc.add_object(lopdf::Dictionary::from_iter(vec![
            ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
            ("Pages", lopdf::Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", lopdf::Object::Reference(catalog_id));
        doc
    }

    /// Verify that cumulative prediction + shared_overhead ≈ actual file size.
    #[test]
    fn test_cumulative_measurement_accuracy() {
        let doc = create_test_doc(10);

        // Measure cumulative sizes (same front-deletion as split_by_size)
        let total_pages = doc.get_pages().len();
        let mut cumulative = vec![0u64; total_pages + 1];
        {
            let mut working = doc.clone();
            let mut buf = Vec::new();
            working.save_to(&mut buf).unwrap();
            cumulative[0] = buf.len() as u64;

            for i in 1..total_pages {
                working.delete_pages(&[1]);
                working.prune_objects();
                let mut buf = Vec::new();
                working.save_to(&mut buf).unwrap();
                cumulative[i] = buf.len() as u64;
            }
        }

        // Measure shared_overhead: actual single-page save minus cumulative prediction
        let tmp = std::env::temp_dir().join("test_overhead_measure.pdf");
        extract_pages_to_file(&doc, &[0], &tmp).unwrap();
        let actual_single = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&tmp);
        let predicted_single = cumulative[0] - cumulative[1];
        let shared_overhead = actual_single.saturating_sub(predicted_single);
        assert!(shared_overhead > 0, "Shared overhead should be > 0, got {shared_overhead}");

        // For each page index, verify shared_overhead + (cumulative[i] - cumulative[i+1])
        // matches actual file size of extracting only that page.
        // Skip the last page because cumulative[N] = 0 does not contain shared
        // resources to cancel, so cumulative[N-1] already includes them.
        for i in 0..total_pages - 1 {
            let predicted = shared_overhead + cumulative[i] - cumulative[i + 1];
            let tmp = std::env::temp_dir().join(format!("test_page_{}.pdf", i));
            extract_pages_to_file(&doc, &[i], &tmp).unwrap();
            let actual = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            let diff = if predicted > actual { predicted - actual } else { actual - predicted };
            assert!(diff < 50,
                "Page {i}: predicted={predicted}, actual={actual}, diff={diff}");
            let _ = std::fs::remove_file(&tmp);
        }

        // Also verify the last page: cumulative[N-1] is the actual file size
        // (includes shared resources since cumulative[N]=0 has nothing to cancel)
        {
            let tmp = std::env::temp_dir().join("test_page_last.pdf");
            extract_pages_to_file(&doc, &[total_pages - 1], &tmp).unwrap();
            let actual = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            let diff = if cumulative[total_pages - 1] > actual {
                cumulative[total_pages - 1] - actual
            } else {
                actual - cumulative[total_pages - 1]
            };
            assert!(diff < 50,
                "Last page: predicted={}, actual={}, diff={}",
                cumulative[total_pages - 1], actual, diff);
            let _ = std::fs::remove_file(&tmp);
        }

        // Test page ranges: shared_overhead + (cumulative[start] - cumulative[end+1]).
        // Ranges that DON'T include the last page use the overhead formula;
        // ranges that DO include the last page use cumulative[start] directly.
        let ranges: [(usize, usize, bool); 4] = [(0, 2, false), (1, 5, false), (3, 7, false), (2, 9, true)];
        for &(start, end, ends_at_last) in &ranges {
            let predicted = if ends_at_last {
                cumulative[start]
            } else {
                shared_overhead + cumulative[start] - cumulative[end + 1]
            };
            let indices: Vec<usize> = (start..=end).collect();
            let tmp = std::env::temp_dir().join(format!("test_range_{}_{}.pdf", start, end));
            extract_pages_to_file(&doc, &indices, &tmp).unwrap();
            let actual = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            let diff = if predicted > actual { predicted - actual } else { actual - predicted };
            assert!(diff < 100,
                "Range [{start}..={end}]: predicted={predicted}, actual={actual}, diff={diff}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Full integration test: split a PDF by size and verify multi-page
    /// chunks are within the size limit.  Single-page chunks that exceed
    /// the limit are unavoidable (a page larger than max_size can't be
    /// split further).
    #[test]
    fn test_split_by_size_does_not_exceed_limit() {
        let mut doc = create_test_doc(8);
        let tmp_input = std::env::temp_dir().join("test_split_size_input.pdf");
        doc.save(&tmp_input).unwrap();

        // Doc has alternating small/large pages (~1KB / ~5.9KB).  With
        // max_size=7000 the algorithm groups them as (small,large) pairs
        // whose combined size ≈ 6.6KB < 7KB.  This yields 4 two-page chunks.
        let max_size = 7000;
        let out_dir = std::env::temp_dir().join("split_test");
        let _ = std::fs::create_dir_all(&out_dir);

        let result = split_by_size(&tmp_input, Some(&out_dir), max_size, true);
        assert!(result.is_ok(), "split_by_size failed: {:?}", result.err());
        let files = result.unwrap();

        assert_eq!(files.len(), 4,
            "Expected 4 two-page chunks, got {}", files.len());

        for (i, path) in files.iter().enumerate() {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            assert!(size <= max_size + 50,
                "Chunk {} ({}) size {} exceeds limit {}",
                i + 1, path.display(), size, max_size);
        }

        // Cleanup
        for path in &files {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_dir_all(&out_dir);
        let _ = std::fs::remove_file(&tmp_input);
    }

    /// Test compute_chunks_from_cumulative directly with known values
    #[test]
    fn test_compute_chunks_from_cumulative_unit() {
        // 5 pages of sizes [90, 110, 100, 90, 110]
        let cumulative = vec![500, 410, 300, 200, 110, 0];
        let chunks = compute_chunks_from_cumulative(&cumulative, 200);

        // Expect: [0,1], [2,3], [4]
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![0, 1]);
        assert_eq!(chunks[1], vec![2, 3]);
        assert_eq!(chunks[2], vec![4]);

        // Verify each chunk's computed size ≤ max_size
        let expected_sizes = [200u64, 190, 110];
        for (i, chunk) in chunks.iter().enumerate() {
            let chunk_size = cumulative[chunk[0]] - cumulative[chunk.last().unwrap() + 1];
            assert!(chunk_size <= 200, "Chunk {i} size {chunk_size} exceeds limit");
            assert_eq!(chunk_size, expected_sizes[i], "Chunk {i} unexpected size");
        }
    }

    /// Test with single oversized page
    #[test]
    fn test_compute_chunks_oversized_page() {
        // Page 1 is huge (500), others are small
        let cumulative = vec![800, 500, 100, 50, 0];
        let chunks = compute_chunks_from_cumulative(&cumulative, 200);

        // Page 0 (size 300) alone exceeds 200, so it gets its own chunk
        // Page 1 (size 400) also exceeds 200, own chunk
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![0]);  // size 300 > 200 → single page chunk
        assert_eq!(chunks[1], vec![1]);  // size 400 > 200 → single page chunk
        assert_eq!(chunks[2], vec![2, 3]); // sizes 50+50=100 ≤ 200
    }

    /// Test all small pages (single chunk)
    #[test]
    fn test_compute_chunks_small_pages() {
        let cumulative = vec![200, 150, 100, 50, 0];
        let chunks = compute_chunks_from_cumulative(&cumulative, 500);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], vec![0, 1, 2, 3]);
    }

    /// Test empty document edge case
    #[test]
    fn test_compute_chunks_empty() {
        let cumulative = vec![0];
        let chunks = compute_chunks_from_cumulative(&cumulative, 100);
        assert!(chunks.is_empty());
    }

    /// Test single page
    #[test]
    fn test_compute_chunks_single_page() {
        let cumulative = vec![100, 0];
        // Single page under limit
        let chunks = compute_chunks_from_cumulative(&cumulative, 200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], vec![0]);
        // Single page at limit
        let chunks = compute_chunks_from_cumulative(&cumulative, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], vec![0]);
        // Single page over limit
        let chunks = compute_chunks_from_cumulative(&cumulative, 50);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], vec![0]);
    }
}
