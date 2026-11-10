use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::State;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::{FixTaskStatus, FixTaskWorkspaceState, PersistenceState, StoredFixTask};

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareFixTaskWorkspaceRequest {
    pub project_path: String,
    pub task_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFixTaskWorkspaceRequest {
    pub project_path: String,
    pub task_id: String,
}

#[tauri::command]
pub fn prepare_fix_task_workspace(
    request: PrepareFixTaskWorkspaceRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    validate(&request.project_path, &request.task_id, "workspace")?;
    let root = Path::new(&request.project_path);
    if !root.is_dir() {
        return Err(invalid("Project path must be an existing directory"));
    }
    let task = persistence
        .database()?
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("Fix task was not found"))?;
    if let (Some(workspace_path), FixTaskWorkspaceState::Ready) =
        (task.workspace_path.as_deref(), task.workspace_state)
    {
        if let Ok(workspace) =
            validate_stored_workspace_path(root, workspace_path, &request.task_id)
        {
            if workspace.is_dir() {
                return Ok(task);
            }
        }
    }
    let workspace_root = workspace_path_for(root, &request.task_id)?;
    if workspace_root.exists() {
        return Err(internal(
            "기존 workspace 경로가 남아 있어 먼저 삭제하거나 복구해야 합니다.".to_owned(),
        ));
    }
    fs::create_dir_all(workspace_root.parent().expect("workspace has a parent"))
        .map_err(|error| internal(format!("workspace 디렉터리를 만들지 못했습니다: {error}")))?;
    let result = Command::new("git")
        .current_dir(root)
        .args(["worktree", "add", "--detach"])
        .arg(&workspace_root)
        .arg("HEAD")
        .output()
        .map_err(|error| internal(error.to_string()))?;
    if !result.status.success() {
        let message = String::from_utf8_lossy(&result.stderr).trim().to_owned();
        let _ = persistence.database()?.update_fix_task_workspace(
            &request.project_path,
            &request.task_id,
            Some(&workspace_root.to_string_lossy()),
            FixTaskWorkspaceState::Error,
            Some(&message),
        );
        return Err(internal(if message.is_empty() {
            "격리 workspace를 만들지 못했습니다.".to_owned()
        } else {
            message
        }));
    }
    persistence
        .database()?
        .update_fix_task_workspace(
            &request.project_path,
            &request.task_id,
            Some(&workspace_root.to_string_lossy()),
            FixTaskWorkspaceState::Ready,
            None,
        )
        .map_err(internal)
}

#[tauri::command]
pub fn remove_fix_task_workspace(
    request: RemoveFixTaskWorkspaceRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    validate(&request.project_path, &request.task_id, "workspace")?;
    let task = persistence
        .database()?
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("Fix task was not found"))?;
    let Some(workspace_path) = task.workspace_path.as_deref() else {
        return persistence
            .database()?
            .update_fix_task_workspace(
                &request.project_path,
                &request.task_id,
                None,
                FixTaskWorkspaceState::Removed,
                None,
            )
            .map_err(internal);
    };
    let workspace = validate_stored_workspace_path(
        Path::new(&request.project_path),
        workspace_path,
        &request.task_id,
    )?;
    let result = Command::new("git")
        .current_dir(&request.project_path)
        .args(["worktree", "remove", "--force"])
        .arg(workspace)
        .output()
        .map_err(|error| internal(error.to_string()))?;
    if !result.status.success() {
        let message = String::from_utf8_lossy(&result.stderr).trim().to_owned();
        return persistence
            .database()?
            .update_fix_task_workspace(
                &request.project_path,
                &request.task_id,
                Some(workspace_path),
                FixTaskWorkspaceState::Error,
                Some(&message),
            )
            .map_err(internal)
            .and_then(|_| {
                Err(internal(if message.is_empty() {
                    "격리 workspace를 삭제하지 못했습니다.".to_owned()
                } else {
                    message
                }))
            });
    }
    persistence
        .database()?
        .update_fix_task_workspace(
            &request.project_path,
            &request.task_id,
            Some(workspace_path),
            FixTaskWorkspaceState::Removed,
            None,
        )
        .map_err(internal)
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

fn workspace_path_for(root: &Path, task_id: &str) -> Result<PathBuf, CommandError> {
    if task_id.contains('/') || task_id.contains('\\') || task_id == "." || task_id == ".." {
        return Err(invalid(
            "Fix task identifier contains an invalid path component",
        ));
    }
    let parent = root
        .parent()
        .unwrap_or(root)
        .join(".visual-agentic-refactor-workspaces");
    if parent.exists() {
        if fs::symlink_metadata(&parent)
            .map_err(|error| internal(error.to_string()))?
            .file_type()
            .is_symlink()
        {
            return Err(invalid("Workspace directory must not be a symlink"));
        }
        let canonical_parent =
            fs::canonicalize(&parent).map_err(|error| internal(error.to_string()))?;
        let canonical_root_parent = fs::canonicalize(root.parent().unwrap_or(root))
            .map_err(|error| internal(error.to_string()))?;
        if canonical_parent.parent() != Some(canonical_root_parent.as_path()) {
            return Err(invalid(
                "Workspace directory must remain under the project parent",
            ));
        }
    }
    Ok(parent.join(task_id))
}

fn validate_stored_workspace_path(
    root: &Path,
    workspace_path: &str,
    task_id: &str,
) -> Result<&Path, CommandError> {
    let workspace = Path::new(workspace_path);
    let expected_parent = root
        .parent()
        .unwrap_or(root)
        .join(".visual-agentic-refactor-workspaces");
    if workspace.parent() != Some(expected_parent.as_path()) {
        return Err(invalid(
            "Stored workspace path is outside the project workspace directory",
        ));
    }
    if workspace.file_name().is_none() || workspace.file_name() == Some(std::ffi::OsStr::new(".")) {
        return Err(invalid("Stored workspace path is invalid"));
    }
    if workspace.file_name() != Some(std::ffi::OsStr::new(task_id)) {
        return Err(invalid("Stored workspace path does not match the task"));
    }
    if expected_parent.exists()
        && fs::symlink_metadata(&expected_parent)
            .map_err(|error| internal(error.to_string()))?
            .file_type()
            .is_symlink()
    {
        return Err(invalid("Workspace directory must not be a symlink"));
    }
    if workspace.exists()
        && fs::symlink_metadata(workspace)
            .map_err(|error| internal(error.to_string()))?
            .file_type()
            .is_symlink()
    {
        return Err(invalid("Stored workspace path must not be a symlink"));
    }
    if workspace.exists() && expected_parent.exists() {
        let canonical_workspace =
            fs::canonicalize(workspace).map_err(|error| internal(error.to_string()))?;
        let canonical_parent =
            fs::canonicalize(&expected_parent).map_err(|error| internal(error.to_string()))?;
        if canonical_workspace != canonical_parent.join(task_id) {
            return Err(invalid(
                "Stored workspace path must remain under the project workspace directory",
            ));
        }
    }
    Ok(workspace)
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}
fn internal(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::{validate_stored_workspace_path, workspace_path_for};
    use std::path::Path;

    #[test]
    fn rejects_task_ids_that_escape_workspace_parent() {
        let root = Path::new("C:/projects/demo");
        assert!(workspace_path_for(root, "../outside").is_err());
        assert!(workspace_path_for(root, "nested/task").is_err());
    }

    #[test]
    fn accepts_workspace_path_directly_under_expected_parent() {
        let root = Path::new("C:/projects/demo");
        assert!(
            validate_stored_workspace_path(
                root,
                "C:/projects/.visual-agentic-refactor-workspaces/task-1",
                "task-1"
            )
            .is_ok()
        );
        assert!(
            validate_stored_workspace_path(root, "C:/projects/outside/task-1", "task-1").is_err()
        );
    }
}
