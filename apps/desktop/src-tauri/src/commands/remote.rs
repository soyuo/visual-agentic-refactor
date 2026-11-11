use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tauri::State;

macro_rules! persist_remote_failure {
    ($persistence:expr, $request:expr, $task:expr, $remote:expr, $error:expr) => {{
        let error = $error;
        $task.orchestration.remote = Some(FixTaskRemoteOperation {
            state: "failed".into(),
            error: Some(error.message.clone()),
            completed_at_unix_ms: Some(now()),
            ..$remote.clone()
        });
        $persistence
            .database()?
            .update_fix_task_orchestration(
                &$request.project_path,
                &$request.task_id,
                &$task.orchestration,
            )
            .map_err(internal)?;
        return Err(error);
    }};
}

use crate::command_error::{CommandError, CommandErrorCode};
use crate::commands::fix_tasks::validate_stored_workspace_path;
use crate::persistence::{FixTaskRemoteOperation, PersistenceState, StoredFixTask};

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidRequest, message)
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis() as i64)
        .unwrap_or_default()
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && !value.contains("..")
        && !value.contains("//")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'/' | b'.'))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteApprovalRequest {
    pub project_path: String,
    pub task_id: String,
    pub action: String,
    pub expected_base_commit: String,
    pub branch: String,
    pub title: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteExecuteRequest {
    pub project_path: String,
    pub task_id: String,
}

fn load_ready_task(
    request_path: &str,
    task_id: &str,
    persistence: &PersistenceState,
) -> Result<StoredFixTask, CommandError> {
    let task = persistence
        .database()?
        .load_fix_task(request_path, task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("작업을 찾지 못했습니다"))?;
    let workspace = task
        .workspace_path
        .as_deref()
        .ok_or_else(|| invalid("고립 workspace가 없습니다"))?;
    if task.workspace_state != crate::persistence::FixTaskWorkspaceState::Ready
        || !Path::new(workspace).is_dir()
    {
        return Err(invalid("고립 workspace가 준비되지 않았습니다"));
    }
    validate_stored_workspace_path(Path::new(request_path), workspace, task_id)?;
    let readiness = task
        .readiness
        .as_ref()
        .ok_or_else(|| invalid("커밋 준비 검증이 없습니다"))?;
    if readiness.state != "approved" {
        return Err(invalid("커밋 준비 승인이 필요합니다"));
    }
    if readiness.base_commit.as_deref() != task.base_commit.as_deref() {
        return Err(invalid("기준 커밋 메타데이터가 일치하지 않습니다"));
    }
    Ok(task)
}

#[tauri::command]
pub fn approve_fix_task_remote_operation(
    request: RemoteApprovalRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    if !matches!(request.action.as_str(), "push" | "pull_request")
        || !valid_sha(&request.expected_base_commit)
        || !valid_branch(&request.branch)
    {
        return Err(invalid("원격 작업 승인 값이 올바르지 않습니다"));
    }
    if request.action == "pull_request"
        && (request
            .title
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            || !request
                .body
                .as_deref()
                .unwrap_or_default()
                .contains("Closes #"))
    {
        return Err(invalid("PR 제목과 Closes # 본문 승인이 필요합니다"));
    }
    let mut task = load_ready_task(&request.project_path, &request.task_id, &persistence)?;
    if task.base_commit.as_deref() != Some(request.expected_base_commit.as_str()) {
        return Err(invalid("기준 커밋이 현재 작업과 다릅니다"));
    }
    task.orchestration.remote = Some(FixTaskRemoteOperation {
        action: request.action,
        state: "approved".into(),
        expected_base_commit: request.expected_base_commit,
        branch: request.branch,
        title: request.title,
        body: request.body,
        approved_at_unix_ms: Some(now()),
        completed_at_unix_ms: None,
        result_url: None,
        error: None,
    });
    persistence
        .database()?
        .update_fix_task_orchestration(&request.project_path, &request.task_id, &task.orchestration)
        .map_err(internal)
}

#[tauri::command]
pub fn execute_fix_task_remote_operation(
    request: RemoteExecuteRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    let mut task = load_ready_task(&request.project_path, &request.task_id, &persistence)?;
    let remote = task
        .orchestration
        .remote
        .clone()
        .ok_or_else(|| invalid("원격 작업 승인이 없습니다"))?;
    if remote.state != "approved" {
        return Err(invalid("원격 작업 승인이 만료되었거나 이미 사용되었습니다"));
    }
    if task.base_commit.as_deref() != Some(remote.expected_base_commit.as_str()) {
        return Err(invalid("기준 커밋이 변경되었습니다"));
    }
    let workspace_for_head = task
        .workspace_path
        .clone()
        .ok_or_else(|| invalid("workspace missing"))?;
    let head = Command::new("git")
        .args(["-C", &workspace_for_head, "rev-parse", "HEAD"])
        .output()
        .map_err(|e| internal(e.to_string()))?;
    if !head.status.success()
        || String::from_utf8_lossy(&head.stdout).trim() != remote.expected_base_commit
    {
        return Err(invalid(
            "원격 작업 전 workspace HEAD가 승인된 기준 커밋과 다릅니다",
        ));
    }
    let preflight_status = Command::new("git")
        .args(["-C", &workspace_for_head, "status", "--porcelain"])
        .output()
        .map_err(|e| internal(e.to_string()))?;
    if !preflight_status.status.success() || !preflight_status.stdout.is_empty() {
        return Err(invalid("workspace is dirty"));
    }
    let preflight_branch = Command::new("git")
        .args(["-C", &workspace_for_head, "branch", "--show-current"])
        .output()
        .map_err(|e| internal(e.to_string()))?;
    if String::from_utf8_lossy(&preflight_branch.stdout).trim() != remote.branch {
        return Err(invalid("workspace branch mismatch"));
    }
    let mut claimed = task.orchestration.clone();
    if let Some(claimed_remote) = claimed.remote.as_mut() {
        claimed_remote.state = "executing".into();
    }
    if !persistence
        .database()?
        .claim_fix_task_remote_execution(
            &request.project_path,
            &request.task_id,
            &task.orchestration,
            &claimed,
        )
        .map_err(internal)?
    {
        return Err(invalid("원격 작업이 이미 실행 중이거나 완료되었습니다"));
    }
    task.orchestration = claimed;
    if task.workspace_path.is_none() {
        persist_remote_failure!(
            persistence,
            request,
            task,
            remote,
            invalid("workspace missing after remote execution claim")
        );
    }
    let workspace = task
        .workspace_path
        .clone()
        .ok_or_else(|| invalid("workspace가 없습니다"))?;
    let status = match Command::new("git")
        .args(["-C", &workspace, "status", "--porcelain"])
        .output()
    {
        Ok(output) => output,
        Err(error) => persist_remote_failure!(
            persistence,
            request,
            task,
            remote,
            internal(error.to_string())
        ),
    };
    if !status.status.success() || !status.stdout.is_empty() {
        let error = invalid("workspace must be clean immediately before remote execution");
        task.orchestration.remote = Some(FixTaskRemoteOperation {
            state: "failed".into(),
            error: Some(error.message.clone()),
            completed_at_unix_ms: Some(now()),
            ..remote.clone()
        });
        persistence
            .database()?
            .update_fix_task_orchestration(
                &request.project_path,
                &request.task_id,
                &task.orchestration,
            )
            .map_err(internal)?;
        return Err(error);
    }
    let branch = match Command::new("git")
        .args(["-C", &workspace, "branch", "--show-current"])
        .output()
    {
        Ok(output) => output,
        Err(error) => persist_remote_failure!(
            persistence,
            request,
            task,
            remote,
            internal(error.to_string())
        ),
    };
    if String::from_utf8_lossy(&branch.stdout).trim() != remote.branch {
        let error = invalid("workspace branch changed after approval");
        task.orchestration.remote = Some(FixTaskRemoteOperation {
            state: "failed".into(),
            error: Some(error.message.clone()),
            completed_at_unix_ms: Some(now()),
            ..remote.clone()
        });
        persistence
            .database()?
            .update_fix_task_orchestration(
                &request.project_path,
                &request.task_id,
                &task.orchestration,
            )
            .map_err(internal)?;
        return Err(error);
    }
    let head = match Command::new("git")
        .args(["-C", &workspace, "rev-parse", "HEAD"])
        .output()
    {
        Ok(output) => output,
        Err(error) => persist_remote_failure!(
            persistence,
            request,
            task,
            remote,
            internal(error.to_string())
        ),
    };
    if !head.status.success()
        || String::from_utf8_lossy(&head.stdout).trim() != remote.expected_base_commit
    {
        let error = invalid("workspace HEAD changed after approval");
        task.orchestration.remote = Some(FixTaskRemoteOperation {
            state: "failed".into(),
            error: Some(error.message.clone()),
            completed_at_unix_ms: Some(now()),
            ..remote.clone()
        });
        persistence
            .database()?
            .update_fix_task_orchestration(
                &request.project_path,
                &request.task_id,
                &task.orchestration,
            )
            .map_err(internal)?;
        return Err(error);
    }
    let execution_result: Result<std::process::Output, CommandError> = (|| {
        let output = if remote.action == "push" {
            Command::new("git")
                .args(["-C", &workspace, "push", "origin", &remote.branch])
                .output()
                .map_err(|e| internal(e.to_string()))?
        } else {
            let title = remote
                .title
                .as_deref()
                .ok_or_else(|| invalid("PR 제목이 없습니다"))?;
            let body = remote
                .body
                .as_deref()
                .ok_or_else(|| invalid("PR 본문이 없습니다"))?;
            Command::new("gh")
                .args([
                    "pr",
                    "create",
                    "--head",
                    &remote.branch,
                    "--title",
                    title,
                    "--body",
                    body,
                ])
                .current_dir(&workspace)
                .output()
                .map_err(|e| internal(e.to_string()))?
        };
        Ok(output)
    })();
    let output = match execution_result {
        Ok(output) => output,
        Err(error) => {
            task.orchestration.remote = Some(FixTaskRemoteOperation {
                state: "failed".into(),
                error: Some(error.message.clone()),
                completed_at_unix_ms: Some(now()),
                ..remote
            });
            persistence
                .database()?
                .update_fix_task_orchestration(
                    &request.project_path,
                    &request.task_id,
                    &task.orchestration,
                )
                .map_err(internal)?;
            return Err(error);
        }
    };
    if !output.status.success() {
        task.orchestration.remote = Some(FixTaskRemoteOperation {
            state: "failed".into(),
            completed_at_unix_ms: Some(now()),
            error: Some(
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(4096)
                    .collect(),
            ),
            ..remote
        });
    } else {
        let result_url = String::from_utf8_lossy(&output.stdout)
            .lines()
            .rev()
            .find(|line| line.trim().starts_with("http://") || line.trim().starts_with("https://"))
            .map(|line| line.trim().to_owned());
        task.orchestration.remote = Some(FixTaskRemoteOperation {
            state: "completed".into(),
            completed_at_unix_ms: Some(now()),
            result_url,
            ..remote
        });
    }
    persistence
        .database()?
        .update_fix_task_orchestration(&request.project_path, &request.task_id, &task.orchestration)
        .map_err(internal)
}
