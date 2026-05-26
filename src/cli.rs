use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use walkdir::WalkDir;

use crate::split;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitModeArg {
    Size,
    Pages,
}

#[derive(Parser)]
#[command(
    name = "pdf-splitter",
    about = "PDF Splitter / PDF 切割工具 — Launch GUI without arguments / 无参数启动 GUI",
    version
)]
pub struct Cli {
    /// PDF file or directory paths (multiple allowed, omit to launch GUI)
    /// PDF 文件或目录路径 (支持多个, 省略时启动 GUI)
    #[arg()]
    pub paths: Vec<PathBuf>,

    /// Max chunk size in MB
    /// 单个分块最大大小 (MB)
    #[arg(short = 's', long, default_value_t = 50)]
    pub max_size: u64,

    /// Pages per chunk when splitting by page count
    /// 按页数切分时每块页数
    #[arg(short = 'p', long, default_value_t = 100)]
    pub page_count: usize,

    /// Split mode: size or pages
    /// 切分模式: size 或 pages
    #[arg(long, value_enum, default_value_t = SplitModeArg::Size)]
    pub mode: SplitModeArg,

    /// Delete source files after successful split
    /// 切割成功后删除源文件
    #[arg(short = 'd', long)]
    pub delete: bool,

    /// Quiet mode (no progress bar)
    /// 静默模式 (无进度条)
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

pub fn run(cli: &Cli) -> Result<()> {
    let max_size = cli.max_size * 1024 * 1024;
    let files = collect_pdf_files(&cli.paths)?;

    if files.is_empty() {
        eprintln!("{}", t!("cli.no_pdf_found"));
        std::process::exit(1);
    }

    match cli.mode {
        SplitModeArg::Size => {
            eprintln!("{}", t!("cli.found_by_size", count = files.len(), size = cli.max_size));
        }
        SplitModeArg::Pages => {
            eprintln!("{}", t!("cli.found_by_pages", count = files.len(), pages = cli.page_count));
        }
    }

    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut files_to_delete: Vec<PathBuf> = Vec::new();

    for (idx, fpath) in files.iter().enumerate() {
        eprintln!("{}", t!("cli.splitting", current = idx + 1, total = files.len(), name = fpath.display()));
        let result = match cli.mode {
            SplitModeArg::Size => split::split_by_size(fpath, None, max_size, cli.quiet),
            SplitModeArg::Pages => split::split_by_page_count(fpath, None, cli.page_count, cli.quiet),
        };
        match result {
            Ok(outputs) => {
                eprintln!("{}", t!("cli.generated", count = outputs.len()));
                succeeded += 1;
                if cli.delete {
                    files_to_delete.push(fpath.clone());
                }
            }
            Err(e) => {
                eprintln!("{}", t!("cli.error", msg = e));
                failed += 1;
            }
        }
    }

    eprintln!("{}", t!("cli.done", succeeded = succeeded, failed = failed));

    if cli.delete && !files_to_delete.is_empty() {
        for f in &files_to_delete {
            if let Err(e) = std::fs::remove_file(f) {
                eprintln!("{}", t!("cli.delete_failed", path = f.display(), error = e));
            } else {
                eprintln!("{}", t!("cli.deleted", path = f.display()));
            }
        }
    }

    Ok(())
}

fn collect_pdf_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for path in paths {
        if path.is_file() {
            if is_pdf(path) {
                files.push(path.clone());
            }
        } else if path.is_dir() {
            collect_dir(path, &mut files)?;
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() && is_pdf(path) {
            out.push(path.to_path_buf());
        }
    }
    Ok(())
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}
