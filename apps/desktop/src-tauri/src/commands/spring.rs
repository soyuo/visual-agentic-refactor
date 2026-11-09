use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::analyzer::SourceLanguage;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::knowledge_graph::{GraphCompleteness, GraphDiagnostic, GraphDiagnosticSeverity};
use crate::persistence::{PersistenceState, ProjectDatabase};
use crate::scanner::{ScanEntry, ScanEntryKind, ScanFileStatus, ScanOptions, ScanProjectRequest};
use crate::spring::{SpringSourceFile, analyze_spring_sources};
use crate::state::{AppState, TaskControl};

const MAX_SOURCE_FILES: usize = 10_000;
const MAX_SOURCE_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeSpringProjectRequest {
    pub path: String,
    #[serde(default)]
    pub options: ScanOptions,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeSpringProjectResponse {
    pub project_path: String,
    pub snapshot_id: String,
    pub completeness: GraphCompleteness,
    pub source_file_count: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub diagnostic_count: usize,
}

#[tauri::command]
pub async fn analyze_spring_project(
    request: AnalyzeSpringProjectRequest,
    app: AppHandle,
    state: State<'_, AppState>,
    persistence: State<'_, PersistenceState>,
) -> Result<AnalyzeSpringProjectResponse, CommandError> {
    let task_control = state.register_task()?;
    let database = persistence.database()?.clone();
    let scan_request = ScanProjectRequest {
        path: request.path,
        options: request.options,
    };

    tauri::async_runtime::spawn_blocking(move || {
        crate::scanner::scan_project_and_process(
            scan_request,
            app,
            task_control,
            |response: &AnalyzeSpringProjectResponse| {
                format!("Spring 정적 분석 완료: {}개 노드", response.node_count)
            },
            |scan, task, app| analyze_and_persist(scan, task, app, &database),
        )
    })
    .await
    .map_err(|_| {
        CommandError::new(
            CommandErrorCode::Internal,
            "Spring 정적 분석 작업을 완료하지 못했습니다.",
        )
    })?
}

fn analyze_and_persist(
    scan: &crate::scanner::ScanProjectResponse,
    task: &TaskControl,
    app: &AppHandle,
    database: &ProjectDatabase,
) -> Result<AnalyzeSpringProjectResponse, CommandError> {
    let root = PathBuf::from(&scan.root_path);
    let candidates: Vec<_> = scan
        .entries
        .iter()
        .filter(|entry| is_spring_source(entry))
        .collect();
    let candidate_count = candidates.len();
    let mut source_paths = Vec::new();
    let mut source_texts = Vec::new();
    let mut source_extensions = Vec::new();
    let mut source_bytes = 0u64;
    let mut diagnostics = Vec::new();

    if candidate_count > MAX_SOURCE_FILES {
        diagnostics.push(partial_diagnostic(
            "spring_source_file_limit_reached",
            format!(
                "Spring analysis is limited to {MAX_SOURCE_FILES} Kotlin and Java files; additional files were skipped."
            ),
        ));
    }

    for (index, entry) in candidates.iter().take(MAX_SOURCE_FILES).enumerate() {
        if task.is_cancel_requested() {
            return Err(CommandError::new(
                CommandErrorCode::Cancelled,
                "Spring 정적 분석이 취소되었습니다.",
            ));
        }
        if source_bytes >= MAX_TOTAL_SOURCE_BYTES {
            diagnostics.push(partial_diagnostic(
                "spring_source_byte_limit_reached",
                format!(
                    "Spring analysis is limited to {MAX_TOTAL_SOURCE_BYTES} source bytes; additional files were skipped."
                ),
            ));
            break;
        }

        let relative_path = Path::new(&entry.relative_path);
        let file_path = root.join(relative_path);
        let canonical_path = match fs::canonicalize(&file_path) {
            Ok(path) if path.starts_with(&root) => path,
            Ok(_) => {
                diagnostics.push(source_read_diagnostic(
                    entry,
                    "The resolved source path escapes the selected project root.",
                ));
                continue;
            }
            Err(error) => {
                diagnostics.push(source_read_diagnostic(
                    entry,
                    &format!("Source file could not be resolved: {error}"),
                ));
                continue;
            }
        };
        let metadata = match fs::metadata(&canonical_path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                diagnostics.push(source_read_diagnostic(
                    entry,
                    "Source path is no longer a regular file.",
                ));
                continue;
            }
            Err(error) => {
                diagnostics.push(source_read_diagnostic(
                    entry,
                    &format!("Source metadata could not be read: {error}"),
                ));
                continue;
            }
        };
        if metadata.len() > MAX_SOURCE_FILE_BYTES {
            diagnostics.push(source_read_diagnostic(
                entry,
                "Source file exceeded the 5 MiB analysis limit and was skipped.",
            ));
            continue;
        }
        if source_bytes.saturating_add(metadata.len()) > MAX_TOTAL_SOURCE_BYTES {
            diagnostics.push(partial_diagnostic(
                "spring_source_byte_limit_reached",
                format!(
                    "Spring analysis is limited to {MAX_TOTAL_SOURCE_BYTES} source bytes; additional files were skipped."
                ),
            ));
            break;
        }
        let mut source = String::new();
        match fs::File::open(&canonical_path).and_then(|file| {
            file.take(MAX_SOURCE_FILE_BYTES + 1)
                .read_to_string(&mut source)
        }) {
            Ok(_) if source.len() as u64 <= MAX_SOURCE_FILE_BYTES => {}
            Ok(_) => {
                diagnostics.push(source_read_diagnostic(
                    entry,
                    "Source file grew beyond the 5 MiB analysis limit and was skipped.",
                ));
                continue;
            }
            Err(error) => {
                diagnostics.push(source_read_diagnostic(
                    entry,
                    &format!("Source text could not be read: {error}"),
                ));
                continue;
            }
        }
        if source_bytes.saturating_add(source.len() as u64) > MAX_TOTAL_SOURCE_BYTES {
            diagnostics.push(partial_diagnostic(
                "spring_source_byte_limit_reached",
                format!(
                    "Spring analysis is limited to {MAX_TOTAL_SOURCE_BYTES} source bytes; additional files were skipped."
                ),
            ));
            break;
        }
        source_bytes = source_bytes.saturating_add(source.len() as u64);
        source_paths.push(entry.relative_path.clone());
        source_texts.push(source);
        source_extensions.push(entry.extension.clone().unwrap_or_default());
        task.emit_progress(
            app,
            (index + 1) as u64,
            Some(candidate_count as u64),
            Some("Spring 소스 분석 준비 중".to_owned()),
        )?;
    }

    if task.is_cancel_requested() {
        return Err(CommandError::new(
            CommandErrorCode::Cancelled,
            "Spring 정적 분석이 취소되었습니다.",
        ));
    }

    let source_files: Vec<_> = source_paths
        .iter()
        .zip(&source_texts)
        .zip(&source_extensions)
        .map(|((relative_path, source), extension)| SpringSourceFile {
            relative_path,
            source,
            language: if matches!(extension.as_str(), "kt" | "kts") {
                SourceLanguage::Kotlin
            } else {
                SourceLanguage::Java
            },
        })
        .collect();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CommandError::new(CommandErrorCode::Internal, error.to_string()))?;
    let analyzed_at_unix_ms = timestamp.as_millis().min(i64::MAX as u128) as i64;
    let snapshot_id = format!("spring-{}", timestamp.as_nanos());
    let mut snapshot = analyze_spring_sources(&snapshot_id, &source_files)
        .map_err(|error| CommandError::new(CommandErrorCode::Internal, error))?;
    snapshot.analyzed_at_unix_ms = Some(analyzed_at_unix_ms);

    if scan.summary.depth_limit_reached || scan.summary.entry_limit_reached {
        snapshot.completeness = GraphCompleteness::Partial;
        snapshot.diagnostics.push(partial_diagnostic(
            "project_scan_limit_reached",
            "Project scanning reached its depth or entry limit; the graph may omit source files.",
        ));
    }
    if !scan.warnings.is_empty() {
        snapshot.completeness = GraphCompleteness::Partial;
        snapshot.diagnostics.push(partial_diagnostic(
            "project_scan_warnings",
            format!(
                "Project scanning reported {} warning(s); the graph may omit inaccessible files.",
                scan.warnings.len()
            ),
        ));
    }
    if !diagnostics.is_empty() {
        snapshot.completeness = GraphCompleteness::Partial;
        snapshot.diagnostics.extend(diagnostics);
    }

    if task.is_cancel_requested() {
        return Err(CommandError::new(
            CommandErrorCode::Cancelled,
            "Spring 정적 분석이 취소되었습니다.",
        ));
    }
    database
        .write_graph_snapshot(&scan.root_path, &snapshot)
        .map_err(|error| CommandError::new(CommandErrorCode::Internal, error))?;

    Ok(AnalyzeSpringProjectResponse {
        project_path: scan.root_path.clone(),
        snapshot_id,
        completeness: snapshot.completeness,
        source_file_count: source_files.len(),
        node_count: snapshot.nodes.len(),
        edge_count: snapshot.edges.len(),
        diagnostic_count: snapshot.diagnostics.len(),
    })
}

fn is_spring_source(entry: &ScanEntry) -> bool {
    matches!(&entry.kind, ScanEntryKind::File)
        && matches!(&entry.file_status, Some(ScanFileStatus::Eligible))
        && matches!(entry.extension.as_deref(), Some("java" | "kt" | "kts"))
}

fn partial_diagnostic(code: &str, message: impl Into<String>) -> GraphDiagnostic {
    GraphDiagnostic {
        code: code.to_owned(),
        severity: GraphDiagnosticSeverity::Warning,
        message: message.into(),
        source_evidence: None,
    }
}

fn source_read_diagnostic(entry: &ScanEntry, reason: &str) -> GraphDiagnostic {
    partial_diagnostic(
        "spring_source_read_failed",
        format!("Skipped {}: {reason}", entry.relative_path),
    )
}
