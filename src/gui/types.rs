use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SplitMode {
    BySize,
    ByPages,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Split,
    ExtractImages,
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
pub struct AnalysisCacheEntry {
    pub modified: Option<SystemTime>,
    pub classification: String,
    pub total_chars: usize,
    pub total_pages: usize,
}

/// Messages sent from worker thread to GUI
pub enum WorkerMsg {
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
