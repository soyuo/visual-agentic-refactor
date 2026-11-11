use serde::Deserialize;
use tauri::State;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::knowledge_graph::{GraphSnapshot, GraphSnapshotInput, GraphSnapshotSummary};
use crate::persistence::PersistenceState;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveGraphSnapshotRequest {
    pub project_path: String,
    pub snapshot: GraphSnapshotInput,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGraphRequest {
    pub project_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetGraphSnapshotRequest {
    pub project_path: String,
    pub snapshot_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareGraphSnapshotRequest {
    pub project_path: String,
    pub analyzer_version: String,
    pub source_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalAnalysisPlan {
    pub reusable_snapshot_id: Option<String>,
    pub reusable: bool,
    pub changed_paths: Vec<String>,
    pub unchanged_paths: Vec<String>,
    pub added_paths: Vec<String>,
    pub removed_paths: Vec<String>,
    pub reason: Option<String>,
}

#[tauri::command]
pub fn compare_graph_snapshot(
    request: CompareGraphSnapshotRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<IncrementalAnalysisPlan, CommandError> {
    validate_project_path(&request.project_path)?;
    if request.analyzer_version.trim().is_empty() {
        return Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "Analyzer version must not be empty",
        ));
    }
    let summaries = persistence
        .database()?
        .list_graph_snapshots(&request.project_path)
        .map_err(internal_error)?;
    let Some(previous) = summaries.into_iter().next() else {
        let mut added: Vec<_> = request.source_hashes.keys().cloned().collect();
        added.sort();
        return Ok(IncrementalAnalysisPlan {
            reusable_snapshot_id: None,
            reusable: false,
            changed_paths: Vec::new(),
            unchanged_paths: Vec::new(),
            added_paths: added,
            removed_paths: Vec::new(),
            reason: Some("no_previous_snapshot".into()),
        });
    };
    if previous.analyzer_version != request.analyzer_version
        || previous.settings != request.settings
    {
        let mut changed: Vec<_> = request.source_hashes.keys().cloned().collect();
        changed.sort();
        return Ok(IncrementalAnalysisPlan {
            reusable_snapshot_id: Some(previous.snapshot_id),
            reusable: false,
            changed_paths: changed,
            unchanged_paths: Vec::new(),
            added_paths: Vec::new(),
            removed_paths: previous.source_hashes.keys().cloned().collect(),
            reason: Some("analysis_contract_changed".into()),
        });
    }
    let mut changed = Vec::new();
    let mut unchanged = Vec::new();
    let mut added = Vec::new();
    for (path, hash) in &request.source_hashes {
        match previous.source_hashes.get(path) {
            Some(old) if old == hash => unchanged.push(path.clone()),
            Some(_) => changed.push(path.clone()),
            None => added.push(path.clone()),
        }
    }
    let mut removed: Vec<_> = previous
        .source_hashes
        .keys()
        .filter(|path| !request.source_hashes.contains_key(*path))
        .cloned()
        .collect();
    changed.sort();
    unchanged.sort();
    added.sort();
    removed.sort();
    Ok(IncrementalAnalysisPlan {
        reusable_snapshot_id: Some(previous.snapshot_id),
        reusable: true,
        changed_paths: changed,
        unchanged_paths: unchanged,
        added_paths: added,
        removed_paths: removed,
        reason: None,
    })
}

#[tauri::command]
pub fn save_graph_snapshot(
    request: SaveGraphSnapshotRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<(), CommandError> {
    validate_project_path(&request.project_path)?;
    persistence
        .database()?
        .write_graph_snapshot(&request.project_path, &request.snapshot)
        .map_err(internal_error)
}

#[tauri::command]
pub fn list_graph_snapshots(
    request: ProjectGraphRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<Vec<GraphSnapshotSummary>, CommandError> {
    validate_project_path(&request.project_path)?;
    persistence
        .database()?
        .list_graph_snapshots(&request.project_path)
        .map_err(internal_error)
}

#[tauri::command]
pub fn get_graph_snapshot(
    request: GetGraphSnapshotRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<Option<GraphSnapshot>, CommandError> {
    validate_project_path(&request.project_path)?;
    if request.snapshot_id.trim().is_empty() {
        return Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "Snapshot ID must not be empty",
        ));
    }
    persistence
        .database()?
        .load_graph_snapshot(&request.project_path, &request.snapshot_id)
        .map_err(internal_error)
}

fn validate_project_path(project_path: &str) -> Result<(), CommandError> {
    if project_path.trim().is_empty() {
        return Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "Project path must not be empty",
        ));
    }
    Ok(())
}

fn internal_error(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
