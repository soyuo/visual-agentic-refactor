use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::command_error::{CommandError, CommandErrorCode};

const DEFAULT_SCAN_DEPTH: usize = 32;
const MAX_SCAN_DEPTH: usize = 64;
const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_SCAN_ENTRIES: usize = 100_000;
const BINARY_PROBE_BYTES: usize = 8 * 1024;
const MAX_IGNORE_PATTERN_COUNT: usize = 256;
const MAX_IGNORE_PATTERN_BYTES: usize = 1024;

const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    ".git/objects/",
    "node_modules/",
    ".gradle/",
    "build/",
    "dist/",
    "target/",
    ".venv/",
];

const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "avi", "bmp", "class", "dylib", "dll", "ear", "exe", "gif", "gz", "ico", "jar", "jpeg",
    "jpg", "mov", "mp3", "mp4", "otf", "pdf", "png", "rar", "so", "tar", "ttf", "war", "webp",
    "woff", "woff2", "zip",
];

const SUPPORTED_TEXT_EXTENSIONS: &[&str] = &[
    "css",
    "gradle",
    "html",
    "java",
    "json",
    "kt",
    "kts",
    "md",
    "properties",
    "sql",
    "toml",
    "ts",
    "tsx",
    "txt",
    "xml",
    "yaml",
    "yml",
];

/// Per-project rules supplied by the UI for one scan.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    /// Gitignore-style patterns. A leading `!` can re-include a default exclusion.
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Requested traversal depth. Values above the hard limit are clamped.
    pub max_depth: Option<usize>,
    /// Requested maximum file size. Values above the hard limit are clamped.
    pub max_file_size_bytes: Option<u64>,
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
    pub depth_limit: u64,
    pub file_size_limit_bytes: u64,
    pub depth_limit_reached: bool,
    pub entry_limit_reached: bool,
}

pub fn scan_project(request: ScanProjectRequest) -> Result<ScanProjectResponse, CommandError> {
    let ScanProjectRequest { path, options } = request;
    if path.trim().is_empty() {
        return Err(invalid_scan_path("프로젝트 경로가 비어 있습니다."));
    }

    let selected_path = PathBuf::from(path);
    if !selected_path.is_absolute() {
        return Err(invalid_scan_path("프로젝트 경로는 절대 경로여야 합니다."));
    }

    let root = fs::canonicalize(&selected_path).map_err(|error| {
        invalid_scan_path(match error.kind() {
            std::io::ErrorKind::NotFound => "선택한 프로젝트 경로가 존재하지 않습니다.",
            std::io::ErrorKind::PermissionDenied => "선택한 프로젝트 경로에 접근할 수 없습니다.",
            _ => "프로젝트 경로를 확인할 수 없습니다.",
        })
    })?;
    let metadata = fs::metadata(&root)
        .map_err(|_| invalid_scan_path("프로젝트 경로 정보를 읽을 수 없습니다."))?;
    if !metadata.is_dir() {
        return Err(invalid_scan_path("선택한 경로가 디렉터리가 아닙니다."));
    }
    let ignore_matcher = build_ignore_matcher(&root, &options.ignore_patterns)?;

    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let max_depth = options
        .max_depth
        .unwrap_or(DEFAULT_SCAN_DEPTH)
        .min(MAX_SCAN_DEPTH);
    let max_file_size_bytes = options
        .max_file_size_bytes
        .unwrap_or(DEFAULT_MAX_FILE_SIZE_BYTES)
        .min(MAX_FILE_SIZE_BYTES);
    let mut summary = ScanSummary {
        depth_limit: max_depth as u64,
        file_size_limit_bytes: max_file_size_bytes,
        ..ScanSummary::default()
    };
    let mut pending = vec![(root.clone(), 0usize)];

    'walk: while let Some((directory, parent_depth)) = pending.pop() {
        if entries.len() >= MAX_SCAN_ENTRIES {
            summary.entry_limit_reached = true;
            warnings.push(entry_limit_warning());
            break;
        }

        let read_dir = match fs::read_dir(&directory) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                warnings.push(read_warning(&root, &directory, &error));
                continue;
            }
        };

        let mut children = Vec::new();
        let mut hit_entry_limit = false;
        for result in read_dir {
            if entries.len().saturating_add(children.len()) >= MAX_SCAN_ENTRIES {
                hit_entry_limit = true;
                warnings.push(entry_limit_warning());
                break;
            }
            match result {
                Ok(entry) => children.push(entry),
                Err(error) => warnings.push(read_warning(&root, &directory, &error)),
            }
        }
        children.sort_by_key(|entry| entry.file_name());

        for child in children {
            let path = child.path();
            let Some(relative_path) = relative_path(&root, &path) else {
                warnings.push(ScanWarning {
                    relative_path: None,
                    message: "프로젝트 루트 기준 경로를 계산하지 못했습니다.".into(),
                });
                continue;
            };

            if entries.len() >= MAX_SCAN_ENTRIES {
                summary.entry_limit_reached = true;
                warnings.push(entry_limit_warning());
                break 'walk;
            }

            let file_type = match child.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    warnings.push(read_warning(&root, &path, &error));
                    continue;
                }
            };

            if ignore_matcher
                .matched(Path::new(&relative_path), file_type.is_dir())
                .is_ignore()
            {
                continue;
            }

            if file_type.is_symlink() {
                warnings.push(ScanWarning {
                    relative_path: Some(relative_path),
                    message: "심볼릭 링크는 경계 검사를 적용한 뒤 처리합니다.".into(),
                });
                continue;
            }

            let metadata = match child.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    warnings.push(read_warning(&root, &path, &error));
                    continue;
                }
            };
            let name = child.file_name().to_string_lossy().into_owned();
            let modified_unix_ms = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .and_then(|duration| u64::try_from(duration.as_millis()).ok());

            if metadata.is_dir() {
                entries.push(ScanEntry {
                    relative_path: relative_path.clone(),
                    name,
                    kind: ScanEntryKind::Directory,
                    size_bytes: None,
                    modified_unix_ms,
                    extension: None,
                    file_status: None,
                });
                summary.directories += 1;
                if parent_depth + 1 < max_depth {
                    pending.push((path, parent_depth + 1));
                } else {
                    summary.depth_limit_reached = true;
                    warnings.push(ScanWarning {
                        relative_path: Some(relative_path.clone()),
                        message: "스캔 깊이 제한에 도달해 하위 항목을 건너뛰었습니다.".into(),
                    });
                }
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
            let size_bytes = metadata.len();
            let file_status = if size_bytes > max_file_size_bytes {
                ScanFileStatus::TooLarge
            } else if is_binary_extension(extension.as_deref()) {
                ScanFileStatus::Binary
            } else if !is_supported_text_file(&name, extension.as_deref()) {
                ScanFileStatus::UnsupportedType
            } else {
                match contains_binary_marker(&path) {
                    Ok(true) => ScanFileStatus::Binary,
                    Ok(false) => ScanFileStatus::Eligible,
                    Err(error) => {
                        warnings.push(read_warning(&root, &path, &error));
                        ScanFileStatus::Binary
                    }
                }
            };
            entries.push(ScanEntry {
                relative_path,
                name,
                kind: ScanEntryKind::File,
                size_bytes: Some(size_bytes),
                modified_unix_ms,
                extension,
                file_status: Some(file_status.clone()),
            });
            summary.files += 1;
            if matches!(file_status, ScanFileStatus::Eligible) {
                summary.eligible_files += 1;
            }
        }

        if hit_entry_limit {
            summary.entry_limit_reached = true;
            break 'walk;
        }
    }

    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    summary.entries = entries.len() as u64;
    summary.skipped_files = summary.files.saturating_sub(summary.eligible_files);

    Ok(ScanProjectResponse {
        root_path: root.to_string_lossy().into_owned(),
        entries,
        warnings,
        summary,
    })
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn build_ignore_matcher(root: &Path, patterns: &[String]) -> Result<Gitignore, CommandError> {
    if patterns.len() > MAX_IGNORE_PATTERN_COUNT {
        return Err(invalid_scan_path(
            "제외 규칙 수가 최대 허용량을 초과했습니다.",
        ));
    }

    let mut builder = GitignoreBuilder::new(root);
    for pattern in DEFAULT_IGNORE_PATTERNS {
        builder
            .add_line(None, pattern)
            .map_err(|_| invalid_scan_path("기본 제외 규칙을 준비하지 못했습니다."))?;
    }

    for pattern in patterns {
        if pattern.len() > MAX_IGNORE_PATTERN_BYTES {
            return Err(invalid_scan_path("제외 규칙이 최대 길이를 초과했습니다."));
        }
        builder
            .add_line(None, pattern)
            .map_err(|_| invalid_scan_path("제외 규칙의 형식이 올바르지 않습니다."))?;
    }

    builder
        .build()
        .map_err(|_| invalid_scan_path("제외 규칙을 적용할 수 없습니다."))
}

fn is_binary_extension(extension: Option<&str>) -> bool {
    extension.is_some_and(|extension| BINARY_EXTENSIONS.contains(&extension))
}

fn is_supported_text_file(name: &str, extension: Option<&str>) -> bool {
    matches!(
        name,
        "Dockerfile" | "Makefile" | ".editorconfig" | ".gitignore"
    ) || extension.is_some_and(|extension| SUPPORTED_TEXT_EXTENSIONS.contains(&extension))
}

fn contains_binary_marker(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;

    let file = fs::File::open(path)?;
    let mut buffer = Vec::with_capacity(BINARY_PROBE_BYTES);
    file.take(BINARY_PROBE_BYTES as u64)
        .read_to_end(&mut buffer)?;
    if buffer.starts_with(&[0xFF, 0xFE]) || buffer.starts_with(&[0xFE, 0xFF]) {
        return Ok(false);
    }
    Ok(buffer.contains(&0))
}

fn read_warning(root: &Path, path: &Path, error: &std::io::Error) -> ScanWarning {
    let reason = match error.kind() {
        std::io::ErrorKind::PermissionDenied => "접근이 거부되었습니다.",
        std::io::ErrorKind::NotFound => "탐색 중 항목이 사라졌습니다.",
        _ => "항목을 읽지 못했습니다.",
    };
    ScanWarning {
        relative_path: relative_path(root, path),
        message: reason.into(),
    }
}

fn entry_limit_warning() -> ScanWarning {
    ScanWarning {
        relative_path: None,
        message: "최대 스캔 항목 수에 도달해 나머지 항목을 건너뛰었습니다.".into(),
    }
}

fn invalid_scan_path(message: &str) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}
