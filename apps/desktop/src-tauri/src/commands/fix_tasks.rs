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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFixPatchPlanRequest {
    pub project_path: String,
    pub task_id: String,
    pub intent: String,
    pub files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyFixPatchRequest {
    pub project_path: String,
    pub task_id: String,
    pub patch: String,
}

#[tauri::command]
pub fn create_fix_patch_plan(
    request: CreateFixPatchPlanRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    validate(&request.project_path, &request.task_id, "patch plan")?;
    if request.intent.trim().is_empty() || request.intent.len() > 4096 {
        return Err(invalid("Patch intent must be between 1 and 4096 bytes"));
    }
    let task = persistence
        .database()?
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("Fix task was not found"))?;
    if request.files.is_empty() || request.files.len() > 64 {
        return Err(invalid("Patch plan must contain 1 to 64 files"));
    }
    let root = Path::new(&request.project_path);
    for path in &request.files {
        validate_patch_path(root, path, &task.target_paths)?;
    }
    let before = request.files.clone();
    persistence
        .database()?
        .save_fix_patch_plan(
            &request.project_path,
            &request.task_id,
            &crate::persistence::FixPatchPlan {
                intent: request.intent,
                files: request.files,
            },
            &before,
        )
        .map_err(internal)
}

#[tauri::command]
pub fn apply_fix_patch(
    request: ApplyFixPatchRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    validate(&request.project_path, &request.task_id, "patch")?;
    if request.patch.trim().is_empty() || request.patch.len() > 1024 * 1024 {
        return Err(invalid("Patch output is empty or exceeds the size limit"));
    }
    let root = Path::new(&request.project_path);
    let task = persistence
        .database()?
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("Fix task was not found"))?;
    let Some(plan) = task.patch_plan.as_ref() else {
        return Err(invalid(
            "Patch plan must be created before applying a patch",
        ));
    };
    let workspace_path = task
        .workspace_path
        .as_deref()
        .ok_or_else(|| invalid("Patch workspace is not ready"))?;
    let workspace = validate_stored_workspace_path(root, workspace_path, &request.task_id)?;
    if task.workspace_state != FixTaskWorkspaceState::Ready {
        return Err(invalid("Patch workspace is not ready"));
    }
    let paths = patch_paths(&request.patch)?;
    if paths.is_empty()
        || paths
            .iter()
            .any(|path| !plan.files.iter().any(|allowed| allowed == path))
    {
        return Err(invalid("Patch contains a file outside the approved plan"));
    }
    let check = run_git_apply(workspace, &request.patch, true)?;
    if !check.status.success() {
        return Err(invalid(
            String::from_utf8_lossy(&check.stderr).trim().to_owned(),
        ));
    }
    let output = run_git_apply(workspace, &request.patch, false)?;
    if !output.status.success() {
        return Err(invalid(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let after = changed_paths(workspace)?;
    persistence
        .database()?
        .save_fix_patch_result(&request.project_path, &request.task_id, &after)
        .map_err(internal)
}

fn validate_patch_path(
    root: &Path,
    value: &str,
    target_paths: &[String],
) -> Result<(), CommandError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || value.contains('\\')
        || value.split('/').any(|part| part == ".." || part.is_empty())
        || value.starts_with(".git/")
        || value.contains("/target/")
        || value.ends_with(".generated")
    {
        return Err(invalid("Patch path is outside the project or is generated"));
    }
    if !target_paths.iter().any(|target| target == value) {
        return Err(invalid("Patch path is not covered by the finding"));
    }
    if !root.join(path).is_file() {
        return Err(invalid("Patch target file does not exist"));
    }
    Ok(())
}

fn patch_paths(patch: &str) -> Result<Vec<String>, CommandError> {
    let mut paths = Vec::new();
    let mut headers = patch.lines().filter(|line| line.starts_with("--- "));
    while let Some(old_header) = headers.next() {
        let Some(new_header) = headers.next() else {
            return Err(invalid("Malformed patch output"));
        };
        if !new_header.starts_with("+++ ") {
            return Err(invalid("Malformed patch file header"));
        }
        let old_path = old_header[4..].split('\t').next().unwrap_or("");
        let new_path = new_header[4..].split('\t').next().unwrap_or("");
        if !old_path.starts_with("a/") || !new_path.starts_with("b/") {
            return Err(invalid("Patch must use repository-relative file headers"));
        }
        let old_path = &old_path[2..];
        let new_path = &new_path[2..];
        if old_path.is_empty() || new_path.is_empty() || old_path != new_path {
            return Err(invalid("Patch cannot rename or delete files"));
        }
        let candidate = Path::new(new_path);
        if candidate.is_absolute()
            || new_path.contains('\\')
            || new_path
                .split('/')
                .any(|part| part == ".." || part.is_empty())
            || new_path.starts_with(".git/")
            || new_path.contains("/target/")
            || new_path.ends_with(".generated")
        {
            return Err(invalid("Patch path is outside the project or is generated"));
        }
        if !paths.iter().any(|item| item == new_path) {
            paths.push(new_path.to_owned());
        }
    }
    if paths.is_empty() {
        return Err(invalid("Malformed patch output"));
    }
    Ok(paths)
}

fn run_git_apply(
    workspace: &Path,
    patch: &str,
    check_only: bool,
) -> Result<std::process::Output, CommandError> {
    use std::io::Write;
    let mut command = Command::new("git");
    command
        .current_dir(workspace)
        .args(["apply", "--whitespace=nowarn"]);
    if check_only {
        command.arg("--check");
    }
    let mut child = command
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| internal(error.to_string()))?;
    child
        .stdin
        .take()
        .ok_or_else(|| internal("패치 입력 스트림을 열지 못했습니다.".to_owned()))?
        .write_all(patch.as_bytes())
        .map_err(|error| internal(error.to_string()))?;
    child
        .wait_with_output()
        .map_err(|error| internal(error.to_string()))
}

fn changed_paths(workspace: &Path) -> Result<Vec<String>, CommandError> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["diff", "--name-only"])
        .output()
        .map_err(|error| internal(error.to_string()))?;
    if !output.status.success() {
        return Err(internal(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
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
    use super::{patch_paths, validate_stored_workspace_path, workspace_path_for};
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

    #[test]
    fn rejects_patch_with_mismatched_source_and_destination_headers() {
        let patch = "--- a/src/Other.kt\n+++ b/src/Main.kt\n@@ -1 +1 @@\n-old\n+new\n";
        assert!(patch_paths(patch).is_err());
    }

    #[test]
    fn extracts_matching_repository_relative_patch_path() {
        let patch = "--- a/src/Main.kt\told\n+++ b/src/Main.kt\tnew\n@@ -1 +1 @@\n-old\n+new\n";
        assert_eq!(patch_paths(patch).unwrap(), vec!["src/Main.kt"]);
    }
}
