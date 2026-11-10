use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::ai_provider::{
    AiProvider, ProviderError, ProviderErrorCode, ProviderUsage, RetryPolicy, ReviewRequest,
    ReviewSelection, ensure_not_cancelled,
};
use crate::anthropic_provider::AnthropicProvider;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::commands::context::ReviewContextPreviewState;
use crate::commands::findings::{ValidateReviewFindingsRequest, validate_findings_request};
use crate::credentials::CredentialProvider;
use crate::events::TaskStatus;
use crate::openai_provider::OpenAiProvider;
use crate::persistence::{PersistenceState, StoredReviewSession};
use crate::state::{AppState, TaskCommitError};

const MAX_MODEL_ID_CHARS: usize = 128;
const MAX_OUTPUT_TOKENS: u32 = 4_096;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveReviewTransferRequest {
    pub project_path: String,
    pub preview_id: String,
    pub snapshot_id: String,
    pub selection: ReviewSelection,
    pub provider: CredentialProvider,
    pub model: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAiReviewRequest {
    pub project_path: String,
    pub preview_id: String,
    pub snapshot_id: String,
    pub selection: ReviewSelection,
    pub provider: CredentialProvider,
    pub model: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiReviewTaskStartedEvent {
    pub request_id: String,
    pub task_id: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiReviewResponse {
    pub session: StoredReviewSession,
    pub provider_id: String,
    pub model: String,
    pub usage: ProviderUsage,
}

/// Record a one-time approval for one provider and one registered preview scope.
#[tauri::command]
pub fn approve_review_transfer(
    request: ApproveReviewTransferRequest,
    preview_state: State<'_, ReviewContextPreviewState>,
) -> Result<(), CommandError> {
    let root = canonical_project_path(&request.project_path)?;
    let model = validated_model_id(&request.model)?;
    preview_state
        .approve_provider_transfer(
            &request.preview_id,
            &root.to_string_lossy(),
            &request.snapshot_id,
            &request.selection,
            request.provider,
            model,
        )
        .map_err(invalid)
}

/// Send only the backend-registered preview context, then validate and persist the result.
#[tauri::command]
pub async fn run_ai_review(
    request: RunAiReviewRequest,
    app: AppHandle,
    app_state: State<'_, AppState>,
    persistence: State<'_, PersistenceState>,
    preview_state: State<'_, ReviewContextPreviewState>,
) -> Result<AiReviewResponse, CommandError> {
    let root = canonical_project_path(&request.project_path)?;
    let model = validated_model_id(&request.model)?;
    let request_id = validated_request_id(&request.request_id)?;

    let (context, max_context_tokens) = preview_state
        .consume_approved_context(
            &request.preview_id,
            &root.to_string_lossy(),
            &request.snapshot_id,
            &request.selection,
            request.provider,
            model,
        )
        .map_err(invalid)?;
    if max_context_tokens == 0 {
        return Err(invalid("문맥 미리보기의 토큰 한도가 올바르지 않습니다."));
    }

    let provider: Box<dyn AiProvider> = match request.provider {
        CredentialProvider::OpenAi => {
            if crate::credentials::openai_subscription_status()
                .map_err(|_| internal("OpenAI 구독 인증 상태를 읽지 못했습니다.".to_owned()))?
                .inference_enabled
            {
                Box::new(OpenAiProvider::from_subscription().map_err(provider_error)?)
            } else {
                Box::new(OpenAiProvider::from_keyring().map_err(provider_error)?)
            }
        }
        CredentialProvider::Anthropic => {
            Box::new(AnthropicProvider::from_keyring().map_err(provider_error)?)
        }
    };
    let task = app_state.register_task()?;
    app.emit(
        "ai-review-task-started",
        AiReviewTaskStartedEvent {
            request_id: request_id.to_owned(),
            task_id: task.task_id(),
        },
    )
    .map_err(|_| internal("리뷰 작업을 시작하지 못했습니다.".to_owned()))?;
    task.emit_status(
        &app,
        TaskStatus::Running,
        0,
        None,
        Some("AI 리뷰 요청을 시작했습니다.".to_owned()),
    )?;

    let review_request = ReviewRequest {
        selection: request.selection.clone(),
        context,
        model: model.to_owned(),
        max_context_tokens,
        max_output_tokens: Some(MAX_OUTPUT_TOKENS),
    };
    let outcome: Result<AiReviewResponse, CommandError> = async {
        let provider_response = provider
            .review(&review_request, &task, RetryPolicy::default())
            .await
            .map_err(provider_error)?;
        ensure_not_cancelled(&task).map_err(provider_error)?;
        let validation_request = ValidateReviewFindingsRequest {
            project_path: root.to_string_lossy().into_owned(),
            preview_id: request.preview_id.clone(),
            snapshot_id: request.snapshot_id.clone(),
            selection: request.selection,
            structured_output: provider_response.structured_output,
        };
        let validation =
            validate_findings_request(&validation_request, &persistence, &preview_state)?;
        ensure_not_cancelled(&task).map_err(provider_error)?;
        let session = task
            .commit_if_not_cancelled(|| {
                persistence
                    .database()
                    .map_err(|error| error.message)
                    .and_then(|database| {
                        database.save_review_session(&root.to_string_lossy(), &validation)
                    })
            })
            .map_err(|error| match error {
                TaskCommitError::Cancelled => {
                    CommandError::new(CommandErrorCode::Cancelled, "검토가 취소되었습니다.")
                }
                TaskCommitError::Inactive => {
                    internal("검토 작업이 이미 종료되었습니다.".to_owned())
                }
                TaskCommitError::StateUnavailable => {
                    internal("검토 작업 상태를 확인하지 못했습니다.".to_owned())
                }
                TaskCommitError::Operation(message) => internal(message),
            })?;
        Ok(AiReviewResponse {
            session,
            provider_id: provider_response.provider_id,
            model: provider_response.model,
            usage: provider_response.usage,
        })
    }
    .await;

    match &outcome {
        Ok(_) => {
            let _ = task.emit_status(
                &app,
                TaskStatus::Completed,
                1,
                Some(1),
                Some("AI 리뷰 결과를 저장했습니다.".to_owned()),
            );
        }
        Err(error) if error.code == CommandErrorCode::Cancelled => {
            let _ = task.emit_status(
                &app,
                TaskStatus::Cancelled,
                0,
                None,
                Some(error.message.clone()),
            );
        }
        Err(error) => {
            let _ = task.emit_status(
                &app,
                TaskStatus::Failed,
                0,
                None,
                Some(error.message.clone()),
            );
        }
    }
    outcome
}

fn canonical_project_path(path: &str) -> Result<PathBuf, CommandError> {
    let requested = PathBuf::from(path);
    if !requested.is_absolute() {
        return Err(invalid("프로젝트 경로가 올바르지 않습니다."));
    }
    let root =
        fs::canonicalize(requested).map_err(|_| invalid("프로젝트 경로를 확인할 수 없습니다."))?;
    if !root.is_dir() {
        return Err(invalid("프로젝트 경로가 디렉터리가 아닙니다."));
    }
    Ok(root)
}

fn provider_error(error: ProviderError) -> CommandError {
    let code = match error.code {
        ProviderErrorCode::Cancelled => CommandErrorCode::Cancelled,
        ProviderErrorCode::InvalidRequest | ProviderErrorCode::ContextLimitExceeded => {
            CommandErrorCode::InvalidInput
        }
        _ => CommandErrorCode::Internal,
    };
    CommandError::new(code, error.message)
}

fn validated_model_id(model: &str) -> Result<&str, CommandError> {
    let model = model.trim();
    if model.is_empty() || model.chars().count() > MAX_MODEL_ID_CHARS {
        return Err(invalid("제공자 모델 ID를 입력해 주세요."));
    }
    Ok(model)
}

fn validated_request_id(request_id: &str) -> Result<&str, CommandError> {
    let request_id = request_id.trim();
    if request_id.is_empty() || request_id.len() > 128 {
        return Err(invalid("리뷰 요청 ID는 1~128자여야 합니다."));
    }
    Ok(request_id)
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}

fn internal(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
