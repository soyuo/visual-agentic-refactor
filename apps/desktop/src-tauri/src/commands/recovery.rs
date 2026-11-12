use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::State;

use crate::command_error::CommandError;
use crate::commands::fix_tasks::validate_stored_workspace_path;
use crate::persistence::{FixTaskOrchestration, FixTaskRecovery, PersistenceState};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeFixTaskRequest {
    pub project_path: String,
    pub task_id: String,
    pub expected_base_commit: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverFixTaskRequest {
    pub project_path: String,
    pub task_id: String,
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::invalid(message.into())
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::internal(message.into())
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn git(workspace: &Path, args: &[&str]) -> Result<std::process::Output, CommandError> {
    crate::local_cli::std_command("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|e| internal(e.to_string()))
}
fn text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
fn all_passed(task: &crate::persistence::StoredFixTask) -> bool {
    task.verification
        .as_ref()
        .map(|v| !v.results.is_empty() && v.results.iter().all(|r| r.status == "passed"))
        .unwrap_or(false)
}

#[tauri::command]
pub fn finalize_fix_task(
    request: FinalizeFixTaskRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<crate::persistence::StoredFixTask, CommandError> {
    let db = persistence.database()?;
    let task = db
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("수정 작업을 찾을 수 없습니다."))?;
    let integration = task
        .orchestration
        .integration
        .clone()
        .ok_or_else(|| invalid("통합 세션이 없습니다."))?;
    if integration.state != "completed" {
        return Err(invalid(
            "통합이 완료되지 않아 최종 검증을 기록할 수 없습니다.",
        ));
    }
    if task.readiness.as_ref().map(|r| r.state.as_str()) != Some("approved") {
        return Err(invalid(
            "commit readiness approval is required before finalization",
        ));
    }
    if task.base_commit.as_deref() != Some(request.expected_base_commit.as_str()) {
        return Err(invalid("기준 SHA가 일치하지 않습니다."));
    }
    let recovery = task
        .orchestration
        .recovery
        .as_ref()
        .ok_or_else(|| invalid("복구 가능한 최종 검증 결과가 없습니다."))?;
    if recovery.state != "verification_failed" {
        return Err(invalid("실패한 최종 검증 결과만 복구할 수 있습니다."));
    }
    let workspace = task
        .workspace_path
        .clone()
        .ok_or_else(|| invalid("격리 workspace가 없습니다."))?;
    let workspace = validate_stored_workspace_path(
        Path::new(&request.project_path),
        &workspace,
        &request.task_id,
    )?;
    let head = git(workspace, &["rev-parse", "HEAD"])?;
    if !head.status.success() {
        return Err(internal("workspace HEAD를 확인하지 못했습니다."));
    }
    let final_sha = text(&head);
    let names = git(
        workspace,
        &[
            "diff",
            "--name-only",
            &request.expected_base_commit,
            &final_sha,
        ],
    )?;
    if !names.status.success() {
        return Err(internal("failed to determine final changed files"));
    }
    let changed_files: Vec<String> = text(&names).lines().map(str::to_owned).collect();
    if changed_files.is_empty()
        || changed_files.iter().any(|path| {
            !task
                .target_paths
                .iter()
                .any(|allowed| path == allowed || path.starts_with(&format!("{allowed}/")))
        })
    {
        return Err(invalid(
            "final changes are empty or outside the approved target paths",
        ));
    }
    let stat = git(
        workspace,
        &["diff", "--stat", &request.expected_base_commit, &final_sha],
    )?;
    if !stat.status.success() {
        return Err(internal("failed to summarize final changes"));
    }
    let diff_summary = text(&stat);
    let passed = all_passed(&task);
    let recovery = FixTaskRecovery {
        state: if passed {
            "verified".into()
        } else {
            "verification_failed".into()
        },
        before_sha: request.expected_base_commit.clone(),
        last_good_sha: request.expected_base_commit.clone(),
        final_sha: Some(final_sha),
        changed_files,
        diff_summary,
        verification: task.verification.clone(),
        error_kind: (!passed).then_some("verification_failed".into()),
        error: (!passed).then_some("최종 검증을 통과하지 못했습니다.".into()),
        updated_at_unix_ms: now(),
    };
    let mut orchestration = task.orchestration;
    orchestration.recovery = Some(recovery);
    orchestration.state = if passed {
        crate::persistence::FixTaskOrchestrationState::Completed
    } else {
        crate::persistence::FixTaskOrchestrationState::Failed
    };
    db.update_fix_task_orchestration(&request.project_path, &request.task_id, &orchestration)
        .map_err(internal)?;
    db.load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| internal("저장된 수정 작업을 다시 읽지 못했습니다."))
}

#[tauri::command]
pub fn recover_fix_task(
    request: RecoverFixTaskRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<crate::persistence::StoredFixTask, CommandError> {
    let db = persistence.database()?;
    let task = db
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("수정 작업을 찾을 수 없습니다."))?;
    let recovery = task
        .orchestration
        .recovery
        .clone()
        .ok_or_else(|| invalid("복구 정보가 없습니다."))?;
    let workspace = task
        .workspace_path
        .clone()
        .ok_or_else(|| invalid("격리 workspace가 없습니다."))?;
    let workspace = validate_stored_workspace_path(
        Path::new(&request.project_path),
        &workspace,
        &request.task_id,
    )?;
    if task.base_commit.as_deref() != Some(recovery.before_sha.as_str()) {
        return Err(invalid(
            "persisted base SHA does not match the recovery record",
        ));
    }
    let status = git(workspace, &["status", "--porcelain"])?;
    if !status.status.success() {
        return Err(internal("failed to inspect recovery workspace"));
    }
    if !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
        return Err(invalid("recovery workspace has uncommitted changes"));
    }
    let head = git(workspace, &["rev-parse", "HEAD"])?;
    if !head.status.success() {
        return Err(internal("failed to inspect recovery workspace HEAD"));
    }
    let current_sha = text(&head);
    if recovery.final_sha.as_deref() != Some(current_sha.as_str()) {
        return Err(invalid(
            "recovery workspace HEAD does not match the persisted failed result",
        ));
    }
    let valid_last_good = git(
        workspace,
        &[
            "merge-base",
            "--is-ancestor",
            &recovery.before_sha,
            &recovery.last_good_sha,
        ],
    )?;
    if !valid_last_good.status.success() {
        return Err(invalid(
            "last good commit is not a descendant of the persisted base SHA",
        ));
    }
    let result = git(workspace, &["reset", "--hard", &recovery.last_good_sha])?;
    if !result.status.success() {
        return Err(internal("마지막 정상 상태로 복구하지 못했습니다."));
    }
    let mut orchestration = task.orchestration;
    let mut updated = recovery;
    updated.state = "recovered".into();
    updated.final_sha = Some(updated.last_good_sha.clone());
    updated.error = None;
    updated.error_kind = None;
    updated.updated_at_unix_ms = now();
    orchestration.recovery = Some(updated);
    orchestration.state = crate::persistence::FixTaskOrchestrationState::RetryScheduled;
    db.update_fix_task_orchestration(&request.project_path, &request.task_id, &orchestration)
        .map_err(internal)?;
    db.load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| internal("복구 후 수정 작업을 다시 읽지 못했습니다."))
}
