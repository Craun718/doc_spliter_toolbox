use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use flate2::read::ZlibDecoder;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::split::SplitControl;

/// Extract all supported embedded images from a PDF.
///
/// Output naming: `<output_dir>/<pdf_stem>_<seq:03>.<ext>` where ext is `jpg`/`jp2`/`tif`
/// according to the stream's original PDF filter. Images shared across pages are
/// deduplicated by ObjectId.
///
/// Supported filters:
/// - `/DCTDecode`      → stream bytes are JPEG, written as-is to `.jpg`
/// - `/JPXDecode`      → stream bytes are JPEG2000, written as-is to `.jp2`
/// - `/CCITTFaxDecode` → raw CCITT bytes wrapped in a minimal TIFF header (G4 only)
/// - `/FlateDecode`    → decoded and re-encoded to JPEG via the `image` crate
pub fn extract_images_with_callback<F: FnMut(&str), P: FnMut(usize, usize)>(
    pdf_path: &Path,
    output_dir: Option<&Path>,
    control: &SplitControl,
    mut log: F,
    mut progress: P,
) -> Result<Vec<PathBuf>> {
    let doc = Document::load(pdf_path)?;
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output")
        .to_string();
    let parent = output_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            pdf_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        });

    if !parent.exists() {
        fs::create_dir_all(&parent)?;
    }

    let pages: Vec<(u32, ObjectId)> = doc.get_pages().into_iter().collect();
    let total_pages = pages.len();
    if total_pages == 0 {
        bail!("PDF has no pages");
    }

    progress(0, total_pages);

    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut outputs: Vec<PathBuf> = Vec::new();
    let mut counter: usize = 0;

    for (i, (_page_num, page_id)) in pages.iter().enumerate() {
        if control.is_stopped() {
            log(&t!("images.stopped_preserving"));
            return Ok(outputs);
        }
        control.wait_if_paused();
        if control.is_stopped() {
            log(&t!("images.stopped_preserving"));
            return Ok(outputs);
        }

        let page_dict = match doc.get_dictionary(*page_id) {
            Ok(d) => d,
            Err(_) => {
                progress(i + 1, total_pages);
                continue;
            }
        };

        let resources = match dict_entry_as_dict(&doc, page_dict, b"Resources") {
            Some(d) => d,
            None => {
                progress(i + 1, total_pages);
                continue;
            }
        };

        let xobjects = match dict_entry_as_dict(&doc, resources, b"XObject") {
            Some(d) => d,
            None => {
                progress(i + 1, total_pages);
                continue;
            }
        };

        for (_name, val) in xobjects.iter() {
            let xobj_id = match val {
                Object::Reference(id) => *id,
                _ => continue,
            };

            if !seen.insert(xobj_id) {
                continue;
            }

            let stream = match doc.get_object(xobj_id).and_then(|o| o.as_stream()) {
                Ok(s) => s,
                Err(_) => continue,
            };

            if !is_image_xobject(&stream.dict) {
                continue;
            }

            let filter_chain = read_filter_chain(&stream.dict);

            // Check for FlateDecode-then-DCTDecode: decompress Flate, then write raw JPEG
            if filter_chain.len() == 2
                && filter_chain[0] == b"FlateDecode"
                && filter_chain[1] == b"DCTDecode"
            {
                let mut decoder = ZlibDecoder::new(stream.content.as_slice());
                let mut jpeg_bytes = Vec::new();
                if decoder.read_to_end(&mut jpeg_bytes).is_ok() && !jpeg_bytes.is_empty() {
                    counter += 1;
                    let path = parent.join(format!("{}_{:03}.jpg", stem, counter));
                    fs::write(&path, &jpeg_bytes)?;
                    outputs.push(path);
                    continue;
                }
            }

            match filter_chain.len() {
                0 => {
                    log(&t!("images.skip_no_filter", id1 = xobj_id.0, id2 = xobj_id.1));
                    continue;
                }
                1 => {
                    let filter_name = &filter_chain[0];
                    match filter_name.as_slice() {
                        b"DCTDecode" => {
                            counter += 1;
                            let path = parent.join(format!("{}_{:03}.jpg", stem, counter));
                            fs::write(&path, &stream.content)?;
                            outputs.push(path);
                        }
                        b"JPXDecode" => {
                            counter += 1;
                            let path = parent.join(format!("{}_{:03}.jp2", stem, counter));
                            fs::write(&path, &stream.content)?;
                            outputs.push(path);
                        }
                        b"CCITTFaxDecode" => {
                            match wrap_ccitt_in_tiff(&doc, stream) {
                                Ok(tiff_bytes) => {
                                    counter += 1;
                                    let path = parent.join(format!("{}_{:03}.tif", stem, counter));
                                    fs::write(&path, &tiff_bytes)?;
                                    outputs.push(path);
                                }
                                Err(e) => {
                                    log(&t!("images.ccitt_wrap_failed", id1 = xobj_id.0, id2 = xobj_id.1, error = e));
                                }
                            }
                        }
                        b"FlateDecode" => {
                            match decode_flate_to_jpeg(&doc, stream) {
                                Ok(jpeg_bytes) => {
                                    counter += 1;
                                    let path = parent.join(format!("{}_{:03}.jpg", stem, counter));
                                    fs::write(&path, &jpeg_bytes)?;
                                    outputs.push(path);
                                }
                                Err(e) => {
                                    log(&t!("images.flate_decode_failed", id1 = xobj_id.0, id2 = xobj_id.1, error = e));
                                }
                            }
                        }
                        other => {
                            log(&t!("images.unsupported_filter", id1 = xobj_id.0, id2 = xobj_id.1, filter = String::from_utf8_lossy(other)));
                        }
                    }
                }
                _ => {
                    let names: Vec<String> = filter_chain
                        .iter()
                        .map(|f| String::from_utf8_lossy(f).to_string())
                        .collect();
                    log(&t!("images.unsupported_filter_combo", id1 = xobj_id.0, id2 = xobj_id.1, filters = names.join(", ")));
                }
            }

        }

        progress(i + 1, total_pages);
    }

    Ok(outputs)
}

/// Resolve a dictionary entry that may be either an inline Dictionary or a Reference.
fn dict_entry_as_dict<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Dictionary> {
    let obj = dict.get(key).ok()?;
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

fn is_image_xobject(dict: &Dictionary) -> bool {
    match dict.get(b"Subtype") {
        Ok(Object::Name(n)) => n.as_slice() == b"Image",
        _ => false,
    }
}

/// Return the filter names in order. Handles single Name, single or multi-element Array,
/// and missing filter (returns empty vec).
fn read_filter_chain(dict: &Dictionary) -> Vec<Vec<u8>> {
    let Ok(filter) = dict.get(b"Filter") else {
        return Vec::new();
    };
    match filter {
        Object::Name(n) => vec![n.clone()],
        Object::Array(arr) => {
            arr.iter().filter_map(|o| {
                if let Object::Name(n) = o { Some(n.clone()) } else { None }
            }).collect()
        }
        _ => Vec::new(),
    }
}

/// Wrap CCITT G4-encoded data in a minimal baseline-TIFF container so common viewers
/// (Windows Photos, IrfanView, libtiff-based tools) can open it directly.
fn wrap_ccitt_in_tiff(doc: &Document, stream: &Stream) -> Result<Vec<u8>> {
    let width = stream
        .dict
        .get(b"Width")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .ok_or_else(|| anyhow::anyhow!("CCITT image missing /Width"))? as u32;
    let height = stream
        .dict
        .get(b"Height")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .ok_or_else(|| anyhow::anyhow!("CCITT image missing /Height"))? as u32;

    let (k, black_is_1) = read_ccitt_decode_params(doc, &stream.dict);

    // Map PDF /DecodeParms.K to TIFF Compression:
    //   K < 0 → G4 (T6), Compression = 4
    //   K ≥ 0 → G3 (T4), Compression = 3
    let compression: u16 = if k < 0 { 4 } else { 3 };

    // PDF default: BlackIs1 = false → 0=black, 1=white → TIFF PhotometricInterpretation 1 (BlackIsZero)
    // PDF BlackIs1 = true → 0=white, 1=black → TIFF PhotometricInterpretation 0 (WhiteIsZero)
    let photometric: u16 = if black_is_1 { 0 } else { 1 };

    let num_entries: u16 = 9;
    let ifd_size: usize = 2 + (num_entries as usize) * 12 + 4;
    let image_offset: u32 = (8 + ifd_size) as u32;

    let mut buf: Vec<u8> = Vec::with_capacity(image_offset as usize + stream.content.len());

    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    buf.extend_from_slice(&8u32.to_le_bytes());

    buf.extend_from_slice(&num_entries.to_le_bytes());

    // IFD entries must be in ascending tag order.
    push_ifd_entry(&mut buf, 0x0100, 4, 1, width);                       // ImageWidth (LONG)
    push_ifd_entry(&mut buf, 0x0101, 4, 1, height);                      // ImageLength (LONG)
    push_ifd_entry(&mut buf, 0x0102, 3, 1, 1);                           // BitsPerSample = 1
    push_ifd_entry(&mut buf, 0x0103, 3, 1, compression as u32);          // Compression
    push_ifd_entry(&mut buf, 0x0106, 3, 1, photometric as u32);          // PhotometricInterpretation
    push_ifd_entry(&mut buf, 0x0111, 4, 1, image_offset);                // StripOffsets
    push_ifd_entry(&mut buf, 0x0115, 3, 1, 1);                           // SamplesPerPixel = 1
    push_ifd_entry(&mut buf, 0x0116, 4, 1, height);                      // RowsPerStrip = full height
    push_ifd_entry(&mut buf, 0x0117, 4, 1, stream.content.len() as u32); // StripByteCounts

    buf.extend_from_slice(&0u32.to_le_bytes()); // next IFD = none

    buf.extend_from_slice(&stream.content);

    Ok(buf)
}

fn push_ifd_entry(buf: &mut Vec<u8>, tag: u16, ftype: u16, count: u32, value: u32) {
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(&ftype.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&value.to_le_bytes());
}

fn read_ccitt_decode_params(doc: &Document, image_dict: &Dictionary) -> (i64, bool) {
    let Some(dp) = image_dict.get(b"DecodeParms").ok() else {
        return (0, false);
    };

    let dp_dict = match dp {
        Object::Dictionary(d) => d,
        Object::Reference(id) => match doc.get_dictionary(*id) {
            Ok(d) => d,
            Err(_) => return (0, false),
        },
        // DecodeParms can also be an array of dicts (one per filter); use the first.
        Object::Array(arr) => match arr.first() {
            Some(Object::Dictionary(d)) => d,
            Some(Object::Reference(id)) => match doc.get_dictionary(*id) {
                Ok(d) => d,
                Err(_) => return (0, false),
            },
            _ => return (0, false),
        },
        _ => return (0, false),
    };

    let k = dp_dict
        .get(b"K")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let black_is_1 = match dp_dict.get(b"BlackIs1").ok() {
        Some(Object::Boolean(b)) => *b,
        _ => false,
    };
    (k, black_is_1)
}

/// Decode a /FlateDecode stream and re-encode as JPEG via the `image` crate.
///
/// Supports /ColorSpace = DeviceGray or DeviceRGB (both 8-bit).
/// CMYK or indexed colorspaces are reported as errors (CMYK needs ICC conversion).
fn decode_flate_to_jpeg(doc: &Document, stream: &Stream) -> Result<Vec<u8>> {
    let width = stream
        .dict
        .get(b"Width")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .ok_or_else(|| anyhow::anyhow!("Flate image missing /Width"))? as u32;
    let height = stream
        .dict
        .get(b"Height")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .ok_or_else(|| anyhow::anyhow!("Flate image missing /Height"))? as u32;
    let bpc = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(8) as u8;
    let colorspace_name = read_colorspace_name(doc, &stream.dict)?;

    let (pixel_format, components) = match colorspace_name.as_slice() {
        b"DeviceGray" => (image::ColorType::L8, 1),
        b"DeviceRGB" => (image::ColorType::Rgb8, 3),
        b"DeviceCMYK" => {
            bail!("CMYK not supported");
        }
        _ => {
            bail!(
                "unsupported colorspace {}",
                String::from_utf8_lossy(&colorspace_name)
            );
        }
    };

    if bpc != 8 {
        bail!("unsupported BitsPerComponent={}", bpc);
    }

    // Decompress the zlib/Deflate stream
    let mut decoder = ZlibDecoder::new(stream.content.as_slice());
    let mut raw = Vec::new();
    decoder.read_to_end(&mut raw)?;

    let expected = (width as usize) * (height as usize) * components;
    if raw.len() < expected {
        bail!(
            "decompressed data too short: {} < {}",
            raw.len(),
            expected
        );
    }

    // Build image from raw RGB bytes and encode to JPEG
    let img = image::ImageBuffer::<image::Rgb<u8>, _>::from_raw(width, height, raw)
        .ok_or_else(|| anyhow::anyhow!("failed to construct image buffer"))?;
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
    encoder.encode(img.as_raw(), width, height, pixel_format.into())?;
    Ok(out)
}

/// Read the /ColorSpace name from an image dictionary.
///
/// Handles:
/// - `/DeviceRGB` / `DeviceGray` — direct Name
/// - `5 0 R` → Name — Reference to Name
/// - `[/ICCBased 10 0 R]` — ICCBased, reads `/N` from profile dict
/// - `[...?]` — any unrecognised array form → fallback to `DeviceRGB`
fn read_colorspace_name(doc: &Document, image_dict: &Dictionary) -> Result<Vec<u8>> {
    let cs = image_dict
        .get(b"ColorSpace")
        .map_err(|_| anyhow::anyhow!("missing /ColorSpace"))?;

    // Direct Name — simplest case.
    if let Object::Name(n) = cs {
        return Ok(n.clone());
    }

    // Reference to another object.
    let obj = if let Object::Reference(id) = cs {
        doc.get_object(*id)
    } else {
        // Might be an Array — handled below.
        Ok(cs)
    };

    match obj {
        Ok(Object::Name(n)) => Ok(n.clone()),
        Ok(Object::Array(arr)) => {
            let first = match arr.first() {
                Some(Object::Name(n)) => n.clone(),
                Some(Object::Reference(id)) => match doc.get_object(*id) {
                    Ok(Object::Name(n)) => n.clone(),
                    _ => return Ok(b"DeviceRGB".to_vec()),
                },
                // First element is not a Name or Reference (e.g. Integer, Null)
                // — fallback to RGB, which is the most common scan case.
                _ => return Ok(b"DeviceRGB".to_vec()),
            };

            // ICCBased: read component count from the referenced stream's /N
            if first == b"ICCBased" && arr.len() >= 2 {
                if let Some(Object::Reference(id)) = arr.get(1) {
                    if let Ok(stream) = doc.get_object(*id).and_then(|o| o.as_stream()) {
                        let n = stream
                            .dict
                            .get(b"N")
                            .ok()
                            .and_then(|o| o.as_i64().ok())
                            .unwrap_or(3);
                        return Ok(if n == 1 {
                            b"DeviceGray".to_vec()
                        } else {
                            b"DeviceRGB".to_vec()
                        });
                    }
                }
            }

            Ok(first)
        }
        _ => Ok(b"DeviceRGB".to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiff_header_has_correct_magic_and_byte_order() {
        let mut doc = Document::new();
        let dict = Dictionary::from_iter(vec![
            ("Width", Object::Integer(100)),
            ("Height", Object::Integer(50)),
        ]);
        let stream = Stream::new(dict, vec![0xFFu8; 64]);
        let tiff = wrap_ccitt_in_tiff(&doc, &stream).unwrap();
        let _ = &mut doc;

        assert_eq!(&tiff[0..2], b"II", "Little-endian byte order");
        assert_eq!(&tiff[2..4], &42u16.to_le_bytes(), "TIFF magic");
        // Image data should appear right after the header+IFD.
        let expected_offset = 8 + 2 + 9 * 12 + 4;
        assert_eq!(&tiff[expected_offset..], &[0xFFu8; 64][..]);
    }

    #[test]
    fn tiff_header_defaults_to_g3_when_no_decode_params() {
        let mut doc = Document::new();
        let dict = Dictionary::from_iter(vec![
            ("Width", Object::Integer(8)),
            ("Height", Object::Integer(4)),
        ]);
        let stream = Stream::new(dict, vec![0u8; 4]);
        let tiff = wrap_ccitt_in_tiff(&doc, &stream).unwrap();
        let _ = &mut doc;

        // IFD layout: header 8 bytes, IFD count 2 bytes, then 12-byte entries.
        // Compression is the 4th entry (index 3). Entry start = 8 + 2 + 3*12 = 46.
        // Value field is at entry offset 8 → absolute offset 54. SHORT lives in low 2 bytes.
        let compression_value = u16::from_le_bytes([tiff[54], tiff[55]]);
        assert_eq!(compression_value, 3, "K defaults to 0 → G3");
    }

    #[test]
    fn read_filter_chain_handles_single_and_multi() {
        let mut d = Dictionary::new();
        d.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        assert_eq!(read_filter_chain(&d), vec![b"DCTDecode".to_vec()]);

        let mut d2 = Dictionary::new();
        d2.set(
            "Filter",
            Object::Array(vec![Object::Name(b"JPXDecode".to_vec())]),
        );
        assert_eq!(read_filter_chain(&d2), vec![b"JPXDecode".to_vec()]);

        let mut d3 = Dictionary::new();
        d3.set(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        assert_eq!(
            read_filter_chain(&d3),
            vec![b"ASCII85Decode".to_vec(), b"FlateDecode".to_vec()],
            "Multi-filter chain returned"
        );
    }
}
