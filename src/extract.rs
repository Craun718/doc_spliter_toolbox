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

/// 仅抽样少量页面估算平均字数，用于切割阶段的轻量分类展示。
pub fn estimate_avg_chars_per_page(doc: &Document) -> f64 {
    let page_numbers: Vec<u32> = doc
        .get_pages()
        .into_iter()
        .map(|(page_num, _)| page_num)
        .collect();
    let total_pages = page_numbers.len();
    if total_pages == 0 {
        return 0.0;
    }

    let sample_count = total_pages.min(5);
    let sample_indices: Vec<usize> = if sample_count == 1 {
        vec![0]
    } else {
        (0..sample_count)
            .map(|k| k * (total_pages - 1) / (sample_count - 1))
            .collect()
    };
    let sample_pages: Vec<u32> = sample_indices
        .into_iter()
        .map(|idx| page_numbers[idx])
        .collect();

    let sample_chars = doc
        .extract_text(&sample_pages)
        .ok()
        .map(|text| text.chars().filter(|c| !c.is_whitespace()).count())
        .unwrap_or(0);

    sample_chars as f64 / sample_pages.len() as f64
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
#[allow(dead_code)]
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
    doc.prune_objects();

    let mut buf = Cursor::new(Vec::new());
    doc.save_to(&mut buf)?;
    Ok(buf.into_inner())
}

/// Extract specified page indices from source doc, write to file.
#[allow(dead_code)]
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
    doc.prune_objects();

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

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Document, Object, Dictionary, Stream};

    fn create_test_doc(num_pages: usize) -> Document {
        let mut doc = Document::new();
        let font_id = doc.add_object(Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Times-Roman".to_vec())),
        ]));

        let mut page_ids = Vec::new();
        for i in 0..num_pages {
            let text = format!("Page {} content", i + 1);
            let content_id = doc.add_object(Stream::new(
                Dictionary::from_iter(vec![
                    ("Length", Object::Integer(text.len() as i64)),
                ]),
                text.into_bytes(),
            ));
            let page_id = doc.new_object_id();
            doc.objects.insert(page_id, Object::Dictionary(
                Dictionary::from_iter(vec![
                    ("Type", Object::Name(b"Page".to_vec())),
                    ("MediaBox", Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(612), Object::Integer(792),
                    ])),
                    ("Contents", Object::Reference(content_id)),
                    ("Resources", Object::Dictionary(Dictionary::from_iter(vec![
                        ("Font", Object::Dictionary(Dictionary::from_iter(vec![
                            ("F1", Object::Reference(font_id)),
                        ]))),
                    ]))),
                ]),
            ));
            page_ids.push(page_id);
        }

        let kids: Vec<Object> = page_ids.iter().map(|&id| Object::Reference(id)).collect();
        let pages_id = doc.add_object(Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(kids)),
            ("Count", Object::Integer(page_ids.len() as i64)),
        ]));

        for &page_id in &page_ids {
            if let Some(Object::Dictionary(ref mut dict)) = doc.objects.get_mut(&page_id) {
                dict.set("Parent", Object::Reference(pages_id));
            }
        }

        let catalog_id = doc.add_object(Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn test_extract_pages_to_file() {
        let doc = create_test_doc(10);
        let tmp = std::env::temp_dir().join("test_extract_output.pdf");
        extract_pages_to_file(&doc, &[2, 3, 4], &tmp).unwrap();

        let loaded = Document::load(&tmp).unwrap();
        let pages = loaded.get_pages().len();
        assert_eq!(pages, 3, "Should have exactly 3 pages");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_single_page() {
        let doc = create_test_doc(10);
        let tmp = std::env::temp_dir().join("test_extract_single.pdf");
        extract_pages_to_file(&doc, &[7], &tmp).unwrap();

        let loaded = Document::load(&tmp).unwrap();
        assert_eq!(loaded.get_pages().len(), 1, "Should have 1 page");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_bytes() {
        let doc = create_test_doc(8);
        let bytes = extract_pages_to_bytes(&doc, &[0, 1]).unwrap();
        assert!(!bytes.is_empty(), "Bytes should not be empty");
        // Verify it's a valid PDF
        assert!(bytes.starts_with(b"%PDF-"), "Should be a PDF");
    }
}
