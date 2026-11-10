use serde::Deserialize;
use tauri::State;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::{FixTaskStatus, PersistenceState, StoredFixTask};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixTaskRequest {
    pub project_path: String,
    pub session_id: String,
    pub finding_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixTaskStatusRequest {
    pub project_path: String,
    pub task_id: String,
    pub status: FixTaskStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFixTasksRequest {
    pub project_path: String,
}

#[tauri::command]
pub fn create_fix_task(
    request: FixTaskRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    validate(
        &request.project_path,
        &request.session_id,
        &request.finding_id,
    )?;
    persistence
        .database()?
        .create_fix_task(
            &request.project_path,
            &request.session_id,
            &request.finding_id,
        )
        .map_err(internal)
}

#[tauri::command]
pub fn list_fix_tasks(
    request: ListFixTasksRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<Vec<StoredFixTask>, CommandError> {
    if request.project_path.trim().is_empty() {
        return Err(invalid("Project path must not be empty"));
    }
    persistence
        .database()?
        .list_fix_tasks(&request.project_path)
        .map_err(internal)
}

#[tauri::command]
pub fn update_fix_task(
    request: FixTaskStatusRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    validate(&request.project_path, &request.task_id, "status")?;
    persistence
        .database()?
        .update_fix_task(
            &request.project_path,
            &request.task_id,
            request.status,
            request.error_message.as_deref(),
        )
        .map_err(internal)
}

fn validate(project_path: &str, first: &str, second: &str) -> Result<(), CommandError> {
    if project_path.trim().is_empty() || first.trim().is_empty() || second.trim().is_empty() {
        return Err(invalid("Fix task identifiers must not be empty"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}
fn internal(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
