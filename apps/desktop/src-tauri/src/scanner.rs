use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::time::UNIX_EPOCH;
use tauri::AppHandle;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::events::TaskStatus;
use crate::state::TaskControl;

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
    pub project_detection: crate::project_detection::ProjectDetection,
    pub persistence_error: Option<String>,
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

pub fn scan_project(
    request: ScanProjectRequest,
    app: AppHandle,
    task_control: TaskControl,
) -> Result<ScanProjectResponse, CommandError> {
    scan_project_and_process(
        request,
        app,
        task_control,
        |response: &ScanProjectResponse| {
            format!("프로젝트 스캔 완료: {}개 항목", response.summary.entries)
        },
        |response, _, _| Ok(response.clone()),
    )
}

pub fn scan_project_and_process<T>(
    request: ScanProjectRequest,
    app: AppHandle,
    task_control: TaskControl,
    success_message: impl FnOnce(&T) -> String,
    process: impl FnOnce(&ScanProjectResponse, &TaskControl, &AppHandle) -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    let mut progress = ScanProgress::new(&app, &task_control);
    progress.emit_initial()?;
    let result = scan_project_inner(request, &mut progress)
        .and_then(|response| process(&response, &task_control, &app));

    let (status, message) = match &result {
        Ok(response) => (TaskStatus::Completed, success_message(&response)),
        Err(error) if matches!(&error.code, CommandErrorCode::Cancelled) => (
            TaskStatus::Cancelled,
            "사용자가 프로젝트 스캔을 취소했습니다.".into(),
        ),
        Err(error) => (TaskStatus::Failed, error.message.clone()),
    };
    let status_result =
        task_control.emit_status(&app, status, progress.completed, None, Some(message));

    match result {
        Ok(response) => {
            status_result?;
            Ok(response)
        }
        Err(error) => {
            let _ = status_result;
            Err(error)
        }
    }
}

fn scan_project_inner(
    request: ScanProjectRequest,
    progress: &mut dyn ScanObserver,
) -> Result<ScanProjectResponse, CommandError> {
    scan_project_with_entry_limit(request, progress, MAX_SCAN_ENTRIES)
}

fn scan_project_with_entry_limit(
    request: ScanProjectRequest,
    progress: &mut dyn ScanObserver,
    max_entries: usize,
) -> Result<ScanProjectResponse, CommandError> {
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
        .clamp(1, MAX_SCAN_DEPTH);
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
        progress.check_cancelled()?;
        if entries.len() >= max_entries {
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
            progress.record_entry()?;
            if entries.len().saturating_add(children.len()) >= max_entries {
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
            progress.check_cancelled()?;
            let path = child.path();
            let Some(relative_path) = relative_path(&root, &path) else {
                warnings.push(ScanWarning {
                    relative_path: None,
                    message: "프로젝트 루트 기준 경로를 계산하지 못했습니다.".into(),
                });
                continue;
            };

            if entries.len() >= max_entries {
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

            let resolved_path = match fs::canonicalize(&path) {
                Ok(resolved_path) => resolved_path,
                Err(error) => {
                    warnings.push(read_warning(&root, &path, &error));
                    continue;
                }
            };
            if !is_within_root(&root, &resolved_path) {
                warnings.push(ScanWarning {
                    relative_path: Some(relative_path),
                    message: "프로젝트 루트 밖을 가리키는 경로를 건너뛰었습니다.".into(),
                });
                continue;
            }

            let metadata = match fs::metadata(&resolved_path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    warnings.push(read_warning(&root, &path, &error));
                    continue;
                }
            };
            if file_type.is_symlink() && metadata.is_dir() {
                warnings.push(ScanWarning {
                    relative_path: Some(relative_path),
                    message: "심볼릭 링크 디렉터리는 순환 및 중복 탐색 방지를 위해 건너뛰었습니다."
                        .into(),
                });
                continue;
            }
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
                    pending.push((resolved_path, parent_depth + 1));
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
                match contains_binary_marker(&resolved_path) {
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
    progress.check_cancelled()?;
    summary.entries = entries.len() as u64;
    summary.skipped_files = summary.files.saturating_sub(summary.eligible_files);
    let project_detection = crate::project_detection::detect_project(&root, &entries);

    Ok(ScanProjectResponse {
        root_path: root.to_string_lossy().into_owned(),
        entries,
        warnings,
        summary,
        project_detection,
        persistence_error: None,
    })
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

struct ScanProgress<'a> {
    app: &'a AppHandle,
    task_control: &'a TaskControl,
    completed: u64,
    last_emitted: Instant,
}

trait ScanObserver {
    fn record_entry(&mut self) -> Result<(), CommandError>;
    fn check_cancelled(&self) -> Result<(), CommandError>;
}

impl<'a> ScanProgress<'a> {
    fn new(app: &'a AppHandle, task_control: &'a TaskControl) -> Self {
        Self {
            app,
            task_control,
            completed: 0,
            last_emitted: Instant::now(),
        }
    }

    fn emit_initial(&mut self) -> Result<(), CommandError> {
        self.task_control.emit_progress(
            self.app,
            self.completed,
            None,
            Some("프로젝트 파일 스캔을 시작합니다.".into()),
        )?;
        self.last_emitted = Instant::now();
        Ok(())
    }

    fn record_entry(&mut self) -> Result<(), CommandError> {
        self.check_cancelled()?;
        self.completed += 1;
        if self.completed % 128 == 0 || self.last_emitted.elapsed().as_millis() >= 100 {
            self.task_control.emit_progress(
                self.app,
                self.completed,
                None,
                Some(format!(
                    "프로젝트 파일 스캔 중: {}개 항목 확인",
                    self.completed
                )),
            )?;
            self.last_emitted = Instant::now();
        }
        Ok(())
    }

    fn check_cancelled(&self) -> Result<(), CommandError> {
        if self.task_control.is_cancel_requested() {
            return Err(CommandError::new(
                CommandErrorCode::Cancelled,
                "프로젝트 파일 스캔이 취소되었습니다.",
            ));
        }
        Ok(())
    }
}

impl ScanObserver for ScanProgress<'_> {
    fn record_entry(&mut self) -> Result<(), CommandError> {
        ScanProgress::record_entry(self)
    }

    fn check_cancelled(&self) -> Result<(), CommandError> {
        ScanProgress::check_cancelled(self)
    }
}

fn is_within_root(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PROJECT_ID: AtomicU64 = AtomicU64::new(1);

    struct TempProject(PathBuf);

    impl TempProject {
        fn new() -> Self {
            let id = NEXT_PROJECT_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "visual-agentic-scanner-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("create temporary project root");
            Self(root)
        }

        fn write(&self, relative_path: &str, contents: &[u8]) {
            let path = self.0.join(relative_path);
            fs::create_dir_all(path.parent().expect("file parent"))
                .expect("create temporary project directory");
            fs::write(path, contents).expect("write temporary project file");
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct TestObserver {
        completed: u64,
        cancel_after: Option<u64>,
    }

    impl ScanObserver for TestObserver {
        fn record_entry(&mut self) -> Result<(), CommandError> {
            self.check_cancelled()?;
            self.completed += 1;
            Ok(())
        }

        fn check_cancelled(&self) -> Result<(), CommandError> {
            if self
                .cancel_after
                .is_some_and(|limit| self.completed >= limit)
            {
                return Err(CommandError::new(
                    CommandErrorCode::Cancelled,
                    "test cancellation",
                ));
            }
            Ok(())
        }
    }

    fn request(root: &Path, options: ScanOptions) -> ScanProjectRequest {
        ScanProjectRequest {
            path: root.to_string_lossy().into_owned(),
            options,
        }
    }

    fn scan(root: &Path, options: ScanOptions) -> Result<ScanProjectResponse, CommandError> {
        scan_project_inner(request(root, options), &mut TestObserver::default())
    }

    fn find_entry<'a>(response: &'a ScanProjectResponse, path: &str) -> &'a ScanEntry {
        response
            .entries
            .iter()
            .find(|entry| entry.relative_path == path)
            .unwrap_or_else(|| panic!("missing scan entry: {path}"))
    }

    #[test]
    fn scans_metadata_excludes_defaults_applies_overrides_and_preserves_files() {
        let project = TempProject::new();
        project.write("src/Main.kt", b"package sample\n");
        project.write("src/private/Secret.kt", b"private\n");
        project.write("src/large.kt", b"12345678901234567890");
        project.write("src/binary.kt", b"\0binary");
        project.write("docs/README.md", b"read me");
        project.write("docs/data.csv", b"a,b\n1,2\n");
        project.write("assets/icon.png", b"png bytes");
        project.write("node_modules/pkg/Hidden.kt", b"hidden\n");
        project.write(".gradle/cache/Hidden.kt", b"hidden\n");
        project.write("build/generated.kt", b"generated\n");
        project.write("dist/Hidden.kt", b"hidden\n");
        project.write("target/Hidden.kt", b"hidden\n");
        project.write(".venv/Hidden.kt", b"hidden\n");
        project.write(".git/objects/ab/object.kt", b"hidden\n");

        let response = scan(
            &project.0,
            ScanOptions {
                ignore_patterns: vec!["src/private/".into(), "!build/".into()],
                max_depth: None,
                max_file_size_bytes: Some(16),
            },
        )
        .expect("scan project");
        let paths = response
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"src/Main.kt"));
        assert!(paths.contains(&"build/generated.kt"));
        assert!(!paths.contains(&"src/private/Secret.kt"));
        for excluded in [
            "node_modules/pkg/Hidden.kt",
            ".gradle/cache/Hidden.kt",
            "dist/Hidden.kt",
            "target/Hidden.kt",
            ".venv/Hidden.kt",
            ".git/objects/ab/object.kt",
        ] {
            assert!(
                !paths.contains(&excluded),
                "unexpected included path: {excluded}"
            );
        }

        assert!(matches!(
            &find_entry(&response, "src/Main.kt").file_status,
            Some(ScanFileStatus::Eligible)
        ));
        assert!(matches!(
            &find_entry(&response, "src/large.kt").file_status,
            Some(ScanFileStatus::TooLarge)
        ));
        assert!(matches!(
            &find_entry(&response, "src/binary.kt").file_status,
            Some(ScanFileStatus::Binary)
        ));
        assert!(matches!(
            &find_entry(&response, "assets/icon.png").file_status,
            Some(ScanFileStatus::Binary)
        ));
        assert!(matches!(
            &find_entry(&response, "docs/data.csv").file_status,
            Some(ScanFileStatus::UnsupportedType)
        ));
        assert_eq!(response.summary.file_size_limit_bytes, 16);
        assert_eq!(find_entry(&response, "src/Main.kt").size_bytes, Some(15));
        assert!(
            find_entry(&response, "src/Main.kt")
                .modified_unix_ms
                .is_some()
        );
        assert_eq!(
            fs::read(project.0.join("src/Main.kt")).unwrap(),
            b"package sample\n"
        );
    }

    #[test]
    fn includes_static_project_detection_and_marks_config_only_as_uncertain() {
        let project = TempProject::new();
        project.write("src/Main.kt", b"package sample\n");
        project.write("src/Main.java", b"package sample;\n");
        project.write(
            "build.gradle.kts",
            b"plugins { id(\"org.springframework.boot\") version \"3.3.0\" }\n",
        );
        project.write("settings.gradle", b"rootProject.name = 'sample'\n");
        project.write(
            "src/main/resources/application.yml",
            b"spring:\n  application:\n    name: sample\n",
        );
        let kotlin_before = fs::read(project.0.join("src/Main.kt")).unwrap();
        let gradle_before = fs::read(project.0.join("build.gradle.kts")).unwrap();

        let response = scan(&project.0, ScanOptions::default()).expect("scan project");
        assert!(matches!(
            response.project_detection.kotlin.status,
            crate::project_detection::DetectionStatus::Detected
        ));
        assert!(matches!(
            response.project_detection.java.status,
            crate::project_detection::DetectionStatus::Detected
        ));
        assert!(matches!(
            response.project_detection.gradle.status,
            crate::project_detection::DetectionStatus::Detected
        ));
        assert!(matches!(
            response.project_detection.spring_boot.status,
            crate::project_detection::DetectionStatus::Detected
        ));
        assert!(
            response
                .project_detection
                .spring_boot
                .evidence
                .iter()
                .any(|evidence| evidence.relative_path == "build.gradle.kts")
        );
        assert_eq!(
            fs::read(project.0.join("src/Main.kt")).unwrap(),
            kotlin_before
        );
        assert_eq!(
            fs::read(project.0.join("build.gradle.kts")).unwrap(),
            gradle_before
        );

        let config_only_project = TempProject::new();
        config_only_project.write(
            "application.properties",
            b"spring.application.name=sample\n",
        );
        let config_only = scan(&config_only_project.0, ScanOptions::default())
            .expect("scan project with application config only");
        assert!(matches!(
            config_only.project_detection.spring_boot.status,
            crate::project_detection::DetectionStatus::Uncertain
        ));

        let many_builds_project = TempProject::new();
        for module in 0..17 {
            many_builds_project.write(
                &format!("module{module}/build.gradle"),
                b"plugins { id 'java' }\n",
            );
        }
        let bounded = scan(&many_builds_project.0, ScanOptions::default())
            .expect("scan project with many Gradle builds");
        assert!(matches!(
            bounded.project_detection.spring_boot.status,
            crate::project_detection::DetectionStatus::Uncertain
        ));
        assert!(
            bounded
                .project_detection
                .spring_boot
                .evidence
                .iter()
                .any(|evidence| evidence.description.contains("검사 한도"))
        );
    }

    #[test]
    fn enforces_depth_and_entry_limits_and_clamps_requested_depth() {
        let project = TempProject::new();
        project.write("Top.kt", b"top\n");
        project.write("level1/level2/Deep.kt", b"deep\n");
        project.write("first.kt", b"first\n");
        project.write("second.kt", b"second\n");
        project.write("third.kt", b"third\n");

        let depth_limited = scan(
            &project.0,
            ScanOptions {
                max_depth: Some(1),
                ..ScanOptions::default()
            },
        )
        .expect("depth-limited scan");
        assert!(depth_limited.summary.depth_limit_reached);
        assert!(
            !depth_limited
                .entries
                .iter()
                .any(|entry| entry.relative_path == "level1/level2/Deep.kt")
        );

        let clamped = scan(
            &project.0,
            ScanOptions {
                max_depth: Some(usize::MAX),
                max_file_size_bytes: Some(u64::MAX),
                ..ScanOptions::default()
            },
        )
        .expect("scan with requested depth above cap");
        assert_eq!(clamped.summary.depth_limit, MAX_SCAN_DEPTH as u64);
        assert_eq!(clamped.summary.file_size_limit_bytes, MAX_FILE_SIZE_BYTES);

        let minimum_depth = scan(
            &project.0,
            ScanOptions {
                max_depth: Some(0),
                ..ScanOptions::default()
            },
        )
        .expect("scan with requested depth below minimum");
        assert_eq!(minimum_depth.summary.depth_limit, 1);

        let mut observer = TestObserver::default();
        let entry_limited = scan_project_with_entry_limit(
            request(&project.0, ScanOptions::default()),
            &mut observer,
            2,
        )
        .expect("entry-limited scan");
        assert_eq!(entry_limited.summary.entries, 2);
        assert!(entry_limited.summary.entry_limit_reached);
    }

    #[test]
    fn rejects_oversized_ignore_patterns_and_cancels_cooperatively() {
        let project = TempProject::new();
        project.write("src/One.kt", b"one\n");
        project.write("src/Two.kt", b"two\n");

        let invalid = scan(
            &project.0,
            ScanOptions {
                ignore_patterns: vec!["x".repeat(MAX_IGNORE_PATTERN_BYTES + 1)],
                ..ScanOptions::default()
            },
        )
        .expect_err("oversized ignore pattern should fail");
        assert!(matches!(invalid.code, CommandErrorCode::InvalidInput));

        let mut observer = TestObserver {
            completed: 0,
            cancel_after: Some(1),
        };
        let cancelled =
            scan_project_inner(request(&project.0, ScanOptions::default()), &mut observer)
                .expect_err("scan should observe cancellation");
        assert!(matches!(cancelled.code, CommandErrorCode::Cancelled));
    }

    #[test]
    fn root_confinement_uses_path_components() {
        assert!(is_within_root(
            Path::new("project"),
            Path::new("project/src/Main.kt")
        ));
        assert!(!is_within_root(
            Path::new("project"),
            Path::new("project-sibling/secret.kt")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn confines_symlinks_and_reports_skipped_targets() {
        use std::os::unix::fs::symlink;

        let project = TempProject::new();
        let outside = TempProject::new();
        project.write("src/Internal.kt", b"inside\n");
        outside.write("Secret.kt", b"outside\n");
        fs::create_dir_all(project.0.join("links")).unwrap();
        symlink(
            project.0.join("src/Internal.kt"),
            project.0.join("links/Internal.kt"),
        )
        .unwrap();
        symlink(
            outside.0.join("Secret.kt"),
            project.0.join("links/Outside.kt"),
        )
        .unwrap();
        symlink(&project.0, project.0.join("links/Loop")).unwrap();

        let response = scan(&project.0, ScanOptions::default()).expect("scan symlinks");
        let paths = response
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"links/Internal.kt"));
        assert!(!paths.contains(&"links/Outside.kt"));
        assert!(!paths.contains(&"links/Loop"));
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| { warning.relative_path.as_deref() == Some("links/Outside.kt") })
        );
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| { warning.relative_path.as_deref() == Some("links/Loop") })
        );
        assert_eq!(fs::read(outside.0.join("Secret.kt")).unwrap(), b"outside\n");
    }
}
