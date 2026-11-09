use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use tauri::State;

use crate::ai_provider::ReviewSelection;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::commands::context::ReviewContextPreviewState;
use crate::persistence::PersistenceState;
use crate::review_finding::{FindingValidationResult, validate_provider_output};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateReviewFindingsRequest {
    pub project_path: String,
    pub preview_id: String,
    pub snapshot_id: String,
    pub selection: ReviewSelection,
    pub structured_output: Value,
}

/// Verify untrusted structured provider output before any finding is shown as actionable.
#[tauri::command]
pub fn validate_review_findings(
    request: ValidateReviewFindingsRequest,
    persistence: State<'_, PersistenceState>,
    preview_state: State<'_, ReviewContextPreviewState>,
) -> Result<FindingValidationResult, CommandError> {
    validate_findings_request(&request, &persistence, &preview_state)
}

pub(crate) fn validate_findings_request(
    request: &ValidateReviewFindingsRequest,
    persistence: &PersistenceState,
    preview_state: &ReviewContextPreviewState,
) -> Result<FindingValidationResult, CommandError> {
    if request.snapshot_id.trim().is_empty() {
        return Err(invalid("분석 스냅샷을 지정해야 합니다."));
    }
    let requested_root = PathBuf::from(&request.project_path);
    if !requested_root.is_absolute() {
        return Err(invalid("프로젝트 경로가 올바르지 않습니다."));
    }
    let root = fs::canonicalize(requested_root)
        .map_err(|_| invalid("프로젝트 경로를 확인할 수 없습니다."))?;
    if !root.is_dir() {
        return Err(invalid("프로젝트 경로가 디렉터리가 아닙니다."));
    }

    let (_, _, included_files, included_source_spans) = preview_state
        .validated_context(
            &request.preview_id,
            &root.to_string_lossy(),
            &request.snapshot_id,
            &request.selection,
        )
        .map_err(invalid)?;
    if included_files.len() > 25 {
        return Err(invalid("컨텍스트 포함 파일 수가 검증 한도를 초과했습니다."));
    }

    let database = persistence.database()?;
    let snapshot = database
        .load_graph_snapshot(&root.to_string_lossy(), &request.snapshot_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("지정한 분석 스냅샷을 찾을 수 없습니다."))?;

    Ok(validate_provider_output(
        &request.structured_output,
        &root,
        &snapshot,
        &request.snapshot_id,
        &request.selection,
        &included_files,
        &included_source_spans,
    ))
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}

fn internal(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
