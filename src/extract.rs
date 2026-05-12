use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;

use anyhow::Result;
use lopdf::{Document, ObjectId};

pub struct PdfTextStats {
    pub total_chars: usize,
    pub avg_chars_per_page: f64,
}

/// 单次遍历页面，完成总字数与平均每页字数统计。
/// 先等间隔抽样 5 页做快速检测，扫描版 PDF 提前退出，避免无谓的全量提取。
pub fn analyze_text_stats(doc: &Document) -> PdfTextStats {
    let page_numbers: Vec<u32> = doc
        .get_pages()
        .into_iter()
        .map(|(page_num, _)| page_num)
        .collect();
    let total_pages = page_numbers.len();
    if total_pages == 0 {
        return PdfTextStats {
            total_chars: 0,
            avg_chars_per_page: 0.0,
        };
    }

    // Evenly spaced sample to detect scanned PDFs early
    let sample_count = total_pages.min(5);
    let step = total_pages / sample_count;
    let sample_pages: Vec<u32> = (0..sample_count)
        .map(|k| page_numbers[k * step])
        .collect();

    let sample_chars = doc
        .extract_text(&sample_pages)
        .ok()
        .map(|text| text.chars().filter(|c| !c.is_whitespace()).count())
        .unwrap_or(0);

    if sample_chars == 0 {
        return PdfTextStats {
            total_chars: 0,
            avg_chars_per_page: 0.0,
        };
    }

    let total_chars = doc
        .extract_text(&page_numbers)
        .ok()
        .map(|text| text.chars().filter(|c| !c.is_whitespace()).count())
        .unwrap_or(0);

    PdfTextStats {
        total_chars,
        avg_chars_per_page: total_chars as f64 / total_pages as f64,
    }
}

/// 根据平均每页字数判断PDF类型
pub fn classify_pdf(avg_chars: f64) -> &'static str {
    if avg_chars >= 100.0 {
        "电子版"
    } else {
        "扫描版"
    }
}

/// Extract specified page indices from source doc, serialize to bytes (for size measurement).
pub fn extract_pages_to_bytes(
    source: &Document,
    page_indices: &[usize],
) -> Result<Vec<u8>> {
    let mut doc = source.clone();
    let pages: Vec<(u32, ObjectId)> = doc.get_pages().into_iter().collect();

    let keep: HashSet<u32> = page_indices
        .iter()
        .map(|&idx| pages[idx].0)
        .collect();

    let to_delete: Vec<u32> = pages
        .into_iter()
        .filter(|(num, _)| !keep.contains(num))
        .map(|(num, _)| num)
        .collect();

    if !to_delete.is_empty() {
        doc.delete_pages(&to_delete);
    }

    let mut buf = Cursor::new(Vec::new());
    doc.save_to(&mut buf)?;
    Ok(buf.into_inner())
}

/// Extract specified page indices from source doc, write to file.
pub fn extract_pages_to_file(
    source: &Document,
    page_indices: &[usize],
    out_path: &Path,
) -> Result<()> {
    let mut doc = source.clone();
    let pages: Vec<(u32, ObjectId)> = doc.get_pages().into_iter().collect();

    let keep: HashSet<u32> = page_indices
        .iter()
        .map(|&idx| pages[idx].0)
        .collect();

    let to_delete: Vec<u32> = pages
        .into_iter()
        .filter(|(num, _)| !keep.contains(num))
        .map(|(num, _)| num)
        .collect();

    if !to_delete.is_empty() {
        doc.delete_pages(&to_delete);
    }

    doc.save(out_path)?;
    Ok(())
}

pub fn format_size(n: u64) -> String {
    let mut size = n as f64;
    for unit in ["B", "KB", "MB"] {
        if size < 1024.0 {
            return format!("{:.1} {}", size, unit);
        }
        size /= 1024.0;
    }
    format!("{:.1} GB", size)
}
