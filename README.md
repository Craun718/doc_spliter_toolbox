# pdf-splitter — PDF Batch Splitter

Split large PDF files by size or page count into smaller PDFs. Available as both a GUI desktop app and a CLI tool.

[中文](README_CN.md)

## Features

- **Two split modes**
  - **By size**: Split PDFs so each chunk does not exceed a specified size (default 50 MB)
  - **By pages**: Split PDFs into fixed page-count chunks (default 100 pages per chunk)
- **Batch processing**: Supports single files, multiple files, or entire directories (recursive)
- **Smart analysis**: Automatically identifies electronic vs. scanned PDFs, showing page count, size, type, and character count
- **Image extraction**: In GUI mode, select files and click "Extract Images" to export embedded images in their original format. Supports /DCTDecode (.jpg), /JPXDecode (.jp2), /CCITTFaxDecode (.tif); deduplicates shared images across pages, outputs as `filename_001.jpg`.
- **GUI mode**: egui-based desktop interface with file list, selection, and progress display
- **CLI mode**: Suitable for scripting, with quiet mode, progress bar, and delete-after-split options
- **Pause/Resume/Stop**: Control split tasks in real time from the GUI
- **Performance**: Prefix-sum cumulative measurement with binary search verification avoids O(N²) serialization overhead

## Installation

### Build from source

```bash
# Debug build
cargo build

# Release build
cargo build --release
```

The release binary is at `target/release/pdf-splitter.exe`.

## Usage

### GUI mode

Run without arguments:

```bash
pdf-splitter
```

In the GUI you can:

1. Click "Select PDF" or "Select Directory" to add files
2. Check the files you want to process in the file list
3. Choose the split method (by size / by pages) and set parameters
4. Click "Start Splitting" to split, or check files and click "Extract Images" to export images

### CLI mode

```bash
# Split by size (default 50 MB)
pdf-splitter input.pdf

# Split by size, max 100 MB per chunk
pdf-splitter --max-size 100 input.pdf

# Split by pages, 50 pages per chunk
pdf-splitter --mode pages --page-count 50 input.pdf

# Batch process multiple files
pdf-splitter file1.pdf file2.pdf file3.pdf

# Process entire directory (recursive)
pdf-splitter ./pdfs/

# Delete source files after splitting
pdf-splitter --delete input.pdf

# Quiet mode (no progress bar)
pdf-splitter --quiet input.pdf
```

Full parameter reference:

| Parameter | Description |
| --------- | ----------- |
| `paths` | PDF file or directory paths (multiple allowed); omit to launch GUI |
| `-s, --max-size` | Max chunk size in MB (default 50) |
| `-p, --page-count` | Pages per chunk when splitting by pages (default 100) |
| `--mode` | Split mode: `size` (default) or `pages` |
| `-d, --delete` | Delete source files after successful split |
| `-q, --quiet` | Quiet mode |

## Output

Split files are named `original_part1.pdf`, `original_part2.pdf`, etc. Output goes to the source file's directory (or the specified output directory).

## Build with Makefile

```bash
make check       # Check compilation
make build       # Debug build
make release     # Release build
make run         # Launch GUI
make run-cli ARGS='--help'  # CLI mode
make clean       # Clean build cache
```

## Technical details

- PDF manipulation powered by [lopdf](https://github.com/J-F-Liu/lopdf)
- Size-based splitting uses prefix-sum cumulative measurement + binary search for strict size enforcement
- Page-based splitting uses progressive cloning to avoid O(N²) overhead
- Supports Unicode/Chinese filenames; system Chinese fonts are loaded automatically in the GUI
- Background analysis accelerated with Rayon parallelism

## License

MIT
