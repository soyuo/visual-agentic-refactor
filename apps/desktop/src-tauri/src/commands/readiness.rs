use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::{FixCommitReadiness, PersistenceState, StoredFixTask};
use serde::Deserialize;
use std::collections::HashSet;
use tauri::State;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessRequest {
    pub project_path: String,
    pub task_id: String,
    pub expected_base_commit: Option<String>,
    pub changed_files: Option<Vec<String>>,
    pub approve: bool,
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidRequest, message.into())
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message.into())
}

#[tauri::command]
pub fn evaluate_fix_commit_readiness(
    request: ReadinessRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    let task = persistence
        .database()?
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("수정 작업을 찾을 수 없습니다."))?;
    let verification = task.verification.clone();
    let changed_files = request
        .changed_files
        .as_ref()
        .cloned()
        .unwrap_or_else(|| task.after_paths.clone());
    let allowed: HashSet<_> = task.target_paths.iter().cloned().collect();
    if !changed_files_are_in_scope(&changed_files, &allowed) {
        return save(
            &request,
            &persistence,
            FixCommitReadiness {
                state: "blocked".into(),
                finding_id: task.finding_id.clone(),
                changed_files,
                verification,
                base_commit: task.base_commit.clone(),
                approver_action: None,
                approved_at_unix_ms: None,
                evaluated_at_unix_ms: now(),
                reason: Some("변경 파일이 승인된 범위를 벗어났습니다.".into()),
            },
        );
    }
    if task.base_commit.is_none()
        || request.expected_base_commit.as_deref() != task.base_commit.as_deref()
    {
        return save(
            &request,
            &persistence,
            FixCommitReadiness {
                state: "blocked".into(),
                finding_id: task.finding_id.clone(),
                changed_files,
                verification,
                base_commit: task.base_commit.clone(),
                approver_action: None,
                approved_at_unix_ms: None,
                evaluated_at_unix_ms: now(),
                reason: Some("base commit이 없거나 일치하지 않습니다.".into()),
            },
        );
    }
    let passed = task
        .verification
        .as_ref()
        .map(|record| {
            all_verifications_passed(record.results.iter().map(|result| result.status.as_str()))
        })
        .unwrap_or(false);
    if !passed {
        return save(
            &request,
            &persistence,
            FixCommitReadiness {
                state: "blocked".into(),
                finding_id: task.finding_id.clone(),
                changed_files,
                verification,
                base_commit: task.base_commit.clone(),
                approver_action: None,
                approved_at_unix_ms: None,
                evaluated_at_unix_ms: now(),
                reason: Some("모든 필수 검증이 passed 상태가 아닙니다.".into()),
            },
        );
    }
    let readiness = FixCommitReadiness {
        state: if request.approve {
            "approved".into()
        } else {
            "ready".into()
        },
        finding_id: task.finding_id.clone(),
        changed_files,
        verification,
        base_commit: task.base_commit.clone(),
        approver_action: request.approve.then_some("approve".into()),
        approved_at_unix_ms: request.approve.then_some(now()),
        evaluated_at_unix_ms: now(),
        reason: None,
    };
    save(&request, &persistence, readiness)
}

fn save(
    request: &ReadinessRequest,
    persistence: &State<'_, PersistenceState>,
    readiness: FixCommitReadiness,
) -> Result<StoredFixTask, CommandError> {
    persistence
        .database()?
        .save_fix_readiness(&request.project_path, &request.task_id, &readiness)
        .map_err(internal)
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn changed_files_are_in_scope(changed_files: &[String], allowed: &HashSet<String>) -> bool {
    !changed_files.is_empty()
        && changed_files
            .iter()
            .all(|path| !path.trim().is_empty() && allowed.contains(path))
}

fn all_verifications_passed<'a>(statuses: impl IntoIterator<Item = &'a str>) -> bool {
    let mut saw_result = false;
    let all_passed = statuses.into_iter().all(|status| {
        saw_result = true;
        status == "passed"
    });
    saw_result && all_passed
}

#[cfg(test)]
mod tests {
    #[test]
    fn readiness_requires_at_least_one_changed_file() {
        let changed_files: Vec<String> = Vec::new();
        assert!(!super::changed_files_are_in_scope(
            &changed_files,
            &std::collections::HashSet::new()
        ));
    }

    #[test]
    fn readiness_only_accepts_all_passed_results() {
        assert!(super::all_verifications_passed(["passed", "passed"]));
        assert!(!super::all_verifications_passed(["passed", "failed"]));
        assert!(!super::all_verifications_passed(["skipped"]));
        assert!(!super::all_verifications_passed(std::iter::empty()));
    }
}
