//! Patch agent: asks the selected provider for a unified diff that resolves an
//! approved finding, limited to the files in the task's patch plan. The diff is
//! returned for human review only; `apply_fix_patch` still validates and applies it.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tauri::State;

use crate::ai_provider::{RetryPolicy, ReviewRequest, ReviewSelection, validate_request};
use crate::command_error::{CommandError, CommandErrorCode};
use crate::commands::fix_tasks::validate_stored_workspace_path;
use crate::commands::graph_diagram::{build_provider, provider_error};
use crate::credentials::CredentialProvider;
use crate::persistence::{FixTaskWorkspaceState, PersistenceState};
use crate::state::AppState;

const MAX_FILE_BYTES: usize = 200 * 1024;
const MAX_CONTEXT_BYTES: usize = 400 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateFixPatchRequest {
    pub project_path: String,
    pub task_id: String,
    pub provider: CredentialProvider,
    pub model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedFixPatch {
    pub task_id: String,
    pub patch: String,
    pub provider_id: String,
    pub model: String,
}

#[tauri::command]
pub async fn generate_fix_patch(
    request: GenerateFixPatchRequest,
    app_state: State<'_, AppState>,
    persistence: State<'_, PersistenceState>,
) -> Result<GeneratedFixPatch, CommandError> {
    let model = request.model.trim().to_owned();
    if model.is_empty() || model.len() > 128 {
        return Err(invalid("모델 ID를 입력해 주세요."));
    }
    let (plan, workspace, finding) = {
        let database = persistence.database()?;
        let task = database
            .load_fix_task(&request.project_path, &request.task_id)
            .map_err(internal)?
            .ok_or_else(|| invalid("수정 작업을 찾을 수 없습니다."))?;
        let plan = task
            .patch_plan
            .clone()
            .ok_or_else(|| invalid("패치 계획을 먼저 만들어 주세요."))?;
        if task.workspace_state != FixTaskWorkspaceState::Ready {
            return Err(invalid("격리 작업 공간이 준비되지 않았습니다."));
        }
        let workspace_path = task
            .workspace_path
            .as_deref()
            .ok_or_else(|| invalid("격리 작업 공간이 준비되지 않았습니다."))?;
        let root = Path::new(&request.project_path);
        let workspace =
            validate_stored_workspace_path(root, workspace_path, &request.task_id)?.to_path_buf();
        let session = database
            .load_review_session(&request.project_path, &task.session_id)
            .map_err(internal)?
            .ok_or_else(|| invalid("리뷰 세션을 찾을 수 없습니다."))?;
        let finding = session
            .findings
            .into_iter()
            .find(|item| item.finding_id == task.finding_id)
            .ok_or_else(|| invalid("finding을 찾을 수 없습니다."))?;
        (plan, workspace, finding)
    };

    let mut files = String::new();
    for relative in &plan.files {
        let content = read_plan_file(&workspace, relative)?;
        files.push_str(&format!(
            "\n=== FILE: {relative} ===\n{content}\n=== END FILE ===\n"
        ));
        if files.len() > MAX_CONTEXT_BYTES {
            return Err(invalid("패치 대상 파일이 너무 커서 AI에 보낼 수 없습니다."));
        }
    }
    let finding_json = serde_json::to_string_pretty(&finding.finding)
        .map_err(|_| internal("finding을 직렬화할 수 없습니다."))?;
    let context = format!(
        "아래 승인된 리뷰 finding을 해결하는 최소 변경 패치를 만들어줘.\n\
         규칙: 허용된 파일만 수정하고, git apply로 적용 가능한 unified diff(--- a/<path>, +++ b/<path>, 정확한 @@ hunk와 문맥 줄)를 만들어. \
         파일 경로는 저장소 기준 상대 경로를 그대로 사용해. 설명이나 코드 펜스 없이 JSON {{\"patch\": string}} 형식으로만 반환해.\n\
         수정 의도: {intent}\n허용 파일: {allowed}\nFinding:\n{finding_json}\n현재 파일 내용:{files}",
        intent = plan.intent,
        allowed = plan.files.join(", "),
    );
    let review_request = ReviewRequest {
        selection: ReviewSelection::File {
            relative_path: plan.files.first().cloned().unwrap_or_default(),
            range: None,
        },
        context,
        model: model.clone(),
        max_context_tokens: 128_000,
        max_output_tokens: Some(16_384),
        output_schema: Some(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["patch"],
            "properties": { "patch": { "type": "string" } }
        })),
    };
    let provider = build_provider(request.provider, workspace)?;
    validate_request(&review_request, &provider.descriptor()).map_err(provider_error)?;
    let control = app_state.register_task()?;
    let response = provider
        .review(&review_request, &control, RetryPolicy::default())
        .await
        .map_err(provider_error)?;
    let patch = response
        .structured_output
        .get("patch")
        .and_then(|value| value.as_str())
        .map(normalize_patch)
        .filter(|patch| !patch.trim().is_empty())
        .ok_or_else(|| invalid("AI가 패치를 반환하지 않았습니다."))?;
    Ok(GeneratedFixPatch {
        task_id: request.task_id,
        patch,
        provider_id: response.provider_id,
        model: response.model,
    })
}

fn read_plan_file(workspace: &Path, relative: &str) -> Result<String, CommandError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(invalid("패치 계획에 잘못된 파일 경로가 있습니다."));
    }
    let path = workspace.join(relative_path);
    if !path.exists() {
        return Ok(String::new());
    }
    let bytes = std::fs::read(&path).map_err(|error| internal(error.to_string()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid(format!("{relative} 파일이 너무 큽니다.")));
    }
    String::from_utf8(bytes).map_err(|_| invalid(format!("{relative} 파일이 텍스트가 아닙니다.")))
}

/// Strips an optional Markdown fence and ensures a trailing newline for `git apply`.
fn normalize_patch(raw: &str) -> String {
    let trimmed = raw.trim();
    let body = ["```diff", "```patch", "```"]
        .iter()
        .find_map(|fence| trimmed.strip_prefix(fence))
        .map(|rest| rest.trim_end().trim_end_matches("```"))
        .unwrap_or(trimmed)
        .trim_matches('\n');
    format!("{body}\n")
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}

fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::normalize_patch;

    #[test]
    fn normalize_patch_strips_fence_and_adds_newline() {
        let patch = normalize_patch("```diff\n--- a/x\n+++ b/x\n```");
        assert_eq!(patch, "--- a/x\n+++ b/x\n");
    }

    #[test]
    fn normalize_patch_keeps_plain_diff() {
        assert_eq!(normalize_patch("--- a/x\n+++ b/x"), "--- a/x\n+++ b/x\n");
    }
}
