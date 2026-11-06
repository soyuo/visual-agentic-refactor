use serde::{Deserialize, Serialize};

/// Per-project rules supplied by the UI for one scan.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    /// Gitignore-style patterns. A leading `!` can re-include a default exclusion.
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProjectRequest {
    pub path: String,
    #[serde(default)]
    pub options: ScanOptions,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProjectResponse {
    pub root_path: String,
    pub entries: Vec<ScanEntry>,
    pub warnings: Vec<ScanWarning>,
    pub summary: ScanSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub relative_path: String,
    pub name: String,
    pub kind: ScanEntryKind,
    pub size_bytes: Option<u64>,
    pub modified_unix_ms: Option<u64>,
    pub extension: Option<String>,
    pub file_status: Option<ScanFileStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanEntryKind {
    Directory,
    File,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanFileStatus {
    Eligible,
    UnsupportedType,
    TooLarge,
    Binary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanWarning {
    pub relative_path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub entries: u64,
    pub files: u64,
    pub directories: u64,
    pub eligible_files: u64,
    pub skipped_files: u64,
}
