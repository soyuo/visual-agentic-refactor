use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use tauri::State;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::commands::context::ReviewContextPreviewState;
use crate::commands::findings::{ValidateReviewFindingsRequest, validate_findings_request};
use crate::persistence::{
    FindingDecisionStatus, PersistenceState, StoredFindingDecision, StoredReviewSession,
    StoredReviewSessionSummary,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReviewSessionsRequest {
    pub project_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionRequest {
    pub project_path: String,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFindingDecisionRequest {
    pub project_path: String,
    pub session_id: String,
    pub finding_id: String,
    pub status: FindingDecisionStatus,
}

#[tauri::command]
pub fn save_review_session(
    request: ValidateReviewFindingsRequest,
    persistence: State<'_, PersistenceState>,
    preview_state: State<'_, ReviewContextPreviewState>,
) -> Result<StoredReviewSession, CommandError> {
    let requested_root = PathBuf::from(&request.project_path);
    if !requested_root.is_absolute() {
        return Err(invalid("프로젝트 경로가 올바르지 않습니다."));
    }
    let root = fs::canonicalize(requested_root)
        .map_err(|_| invalid("프로젝트 경로를 확인할 수 없습니다."))?;
    if !root.is_dir() {
        return Err(invalid("프로젝트 경로가 디렉터리가 아닙니다."));
    }
    let result = validate_findings_request(&request, &persistence, &preview_state)?;
    let project_path = root.to_string_lossy();
    persistence
        .database()?
        .save_review_session(&project_path, &result)
        .map_err(internal_error)
}

#[tauri::command]
pub fn list_review_sessions(
    request: ProjectReviewSessionsRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<Vec<StoredReviewSessionSummary>, CommandError> {
    validate_project_path(&request.project_path)?;
    persistence
        .database()?
        .list_review_sessions(&request.project_path)
        .map_err(internal_error)
}

#[tauri::command]
pub fn load_review_session(
    request: ReviewSessionRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<Option<StoredReviewSession>, CommandError> {
    validate_project_path(&request.project_path)?;
    validate_identifier(&request.session_id, "Session ID")?;
    persistence
        .database()?
        .load_review_session(&request.project_path, &request.session_id)
        .map_err(internal_error)
}

#[tauri::command]
pub fn set_finding_decision(
    request: SetFindingDecisionRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFindingDecision, CommandError> {
    validate_project_path(&request.project_path)?;
    validate_identifier(&request.session_id, "Session ID")?;
    validate_identifier(&request.finding_id, "Finding ID")?;
    persistence
        .database()?
        .set_finding_decision(
            &request.project_path,
            &request.session_id,
            &request.finding_id,
            request.status,
        )
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

fn validate_identifier(value: &str, label: &str) -> Result<(), CommandError> {
    if value.trim().is_empty() {
        return Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            format!("{label} must not be empty"),
        ));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}

fn internal_error(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
