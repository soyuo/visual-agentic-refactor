use serde::Deserialize;
use tauri::State;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::knowledge_graph::{GraphSnapshot, GraphSnapshotInput, GraphSnapshotSummary};
use crate::persistence::PersistenceState;

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
