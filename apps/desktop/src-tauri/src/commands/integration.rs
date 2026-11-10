use std::path::Path;
use std::process::Command;
use tauri::State;

use crate::command_error::CommandError;
use crate::commands::fix_tasks::validate_stored_workspace_path;
use crate::persistence::{FixTaskIntegration, FixTaskOrchestration, PersistenceState};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrateFixTaskRequest {
    pub project_path: String,
    pub task_id: String,
    pub commits: Vec<String>,
    pub expected_base_commit: String,
    pub approve: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrateFixTaskResponse {
    pub task_id: String,
    pub applied_commits: Vec<String>,
    pub current_index: usize,
    pub state: String,
}

fn internal(message: impl Into<String>) -> CommandError {
    CommandError::internal(message.into())
}
fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::invalid(message.into())
}

fn git(workspace: &Path, args: &[&str]) -> Result<std::process::Output, CommandError> {
    Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|e| internal(e.to_string()))
}

fn output_text(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }
}

#[tauri::command]
pub fn integrate_fix_task_commits(
    request: IntegrateFixTaskRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<IntegrateFixTaskResponse, CommandError> {
    if !request.approve {
        return Err(invalid("커밋 통합 승인이 필요합니다."));
    }
    if request.commits.is_empty() || request.commits.len() > 128 {
        return Err(invalid("통합할 커밋 목록이 비어 있거나 너무 깁니다."));
    }
    if request.expected_base_commit.len() != 40
        || !request
            .expected_base_commit
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    {
        return Err(invalid("기준 SHA 형식이 올바르지 않습니다."));
    }
    let db = persistence.database()?;
    let task = db
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("수정 작업을 찾을 수 없습니다."))?;
    let workspace = task
        .workspace_path
        .clone()
        .ok_or_else(|| invalid("격리 workspace가 없습니다."))?;
    if task.workspace_state != crate::persistence::FixTaskWorkspaceState::Ready {
        return Err(invalid("유효한 격리 workspace가 아닙니다."));
    }
    let project_root = Path::new(&request.project_path);
    let workspace_path =
        validate_stored_workspace_path(project_root, &workspace, &request.task_id)?;
    let status = git(workspace_path, &["status", "--porcelain"])?;
    if !status.status.success() {
        return Err(internal(output_text(&status)));
    }
    if !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
        return Err(invalid(
            "workspace에 커밋되지 않은 변경이 있어 중단했습니다.",
        ));
    }
    let head = git(workspace_path, &["rev-parse", "HEAD"])?;
    if !head.status.success() {
        return Err(invalid("workspace 기준 SHA를 확인할 수 없습니다."));
    }
    for commit in &request.commits {
        if commit.len() < 7 || commit.len() > 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("커밋 SHA 형식이 올바르지 않습니다."));
        }
    }
    let mut integration = task
        .orchestration
        .integration
        .unwrap_or(FixTaskIntegration {
            commits: request.commits.clone(),
            current_index: 0,
            state: "ready".into(),
            expected_base_commit: request.expected_base_commit.clone(),
            updated_at_unix_ms: 0,
            error: None,
        });
    if integration.commits != request.commits
        || integration.expected_base_commit != request.expected_base_commit
    {
        return Err(invalid("저장된 통합 계획과 요청이 일치하지 않습니다."));
    }
    let current_head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if integration.current_index == 0 && current_head != request.expected_base_commit {
        return Err(invalid("기준 SHA가 현재 workspace와 일치하지 않습니다."));
    }
    if integration.current_index > integration.commits.len() {
        return Err(invalid("저장된 통합 진행 상태가 올바르지 않습니다."));
    }
    if integration.current_index > 0 {
        let expected_head = &integration.commits[integration.current_index - 1];
        if !current_head.starts_with(expected_head) {
            return Err(invalid(
                "저장된 통합 진행 상태와 workspace HEAD가 일치하지 않습니다.",
            ));
        }
    }
    integration.state = "applying".into();
    integration.error = None;
    db.update_fix_task_orchestration(
        &request.project_path,
        &request.task_id,
        &FixTaskOrchestration {
            integration: Some(integration.clone()),
            ..task.orchestration.clone()
        },
    )
    .map_err(internal)?;
    while integration.current_index < integration.commits.len() {
        let commit = integration.commits[integration.current_index].clone();
        let result = git(workspace_path, &["cherry-pick", &commit])?;
        if !result.status.success() {
            let _ = git(workspace_path, &["cherry-pick", "--abort"]);
            integration.state = "failed".into();
            integration.error = Some(output_text(&result));
            let _ = db.update_fix_task_orchestration(
                &request.project_path,
                &request.task_id,
                &FixTaskOrchestration {
                    integration: Some(integration.clone()),
                    ..task.orchestration.clone()
                },
            );
            return Err(internal(
                integration
                    .error
                    .clone()
                    .unwrap_or_else(|| "커밋 통합에 실패했습니다.".into()),
            ));
        }
        integration.current_index += 1;
        db.update_fix_task_orchestration(
            &request.project_path,
            &request.task_id,
            &FixTaskOrchestration {
                integration: Some(integration.clone()),
                ..task.orchestration.clone()
            },
        )
        .map_err(internal)?;
    }
    integration.state = "completed".into();
    db.update_fix_task_orchestration(
        &request.project_path,
        &request.task_id,
        &FixTaskOrchestration {
            integration: Some(integration.clone()),
            ..task.orchestration
        },
    )
    .map_err(internal)?;
    Ok(IntegrateFixTaskResponse {
        task_id: request.task_id,
        applied_commits: integration.commits,
        current_index: integration.current_index,
        state: integration.state,
    })
}
