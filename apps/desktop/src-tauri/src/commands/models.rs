use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::credentials::CredentialProvider;
use crate::local_cli::resolve_command;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModelOption {
    pub id: String,
    pub display_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAiModelsRequest {
    pub provider: CredentialProvider,
}

#[tauri::command]
pub async fn list_ai_models(
    request: ListAiModelsRequest,
) -> Result<Vec<AiModelOption>, CommandError> {
    match request.provider {
        CredentialProvider::Codex => list_codex_models().await,
        CredentialProvider::ClaudeCode => Ok([
            ("sonnet", "Claude Sonnet"),
            ("opus", "Claude Opus"),
            ("haiku", "Claude Haiku"),
        ]
        .into_iter()
        .map(|(id, display_name)| AiModelOption {
            id: id.into(),
            display_name: display_name.into(),
        })
        .collect()),
        CredentialProvider::OpenAi | CredentialProvider::Anthropic => Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "API 키 provider는 API 키 설정 후 모델 목록 조회를 지원하도록 준비 중입니다.",
        )),
        CredentialProvider::GitHub => Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "GitHub는 AI 모델 provider가 아닙니다.",
        )),
    }
}

async fn list_codex_models() -> Result<Vec<AiModelOption>, CommandError> {
    let executable = resolve_command("codex").ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::InvalidInput,
            "공식 Codex CLI를 찾을 수 없습니다.",
        )
    })?;
    let mut child = crate::local_cli::tokio_command(executable)
        .args(["app-server", "--listen", "stdio://"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| {
            internal(format!(
                "Codex 모델 목록 조회를 시작할 수 없습니다: {error}"
            ))
        })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| internal("Codex 입력 채널을 열 수 없습니다."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| internal("Codex 출력 채널을 열 수 없습니다."))?;
    let initialize = serde_json::json!({
        "method": "initialize",
        "id": 1,
        "params": {
            "clientInfo": {
                "name": "visual-agentic-refactor",
                "title": "Visual Agentic Refactor",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    let initialized = serde_json::json!({ "method": "initialized", "params": {} });
    let model_list = serde_json::json!({ "method": "model/list", "id": 2, "params": {} });
    stdin
        .write_all(format!("{}\n{}\n{}\n", initialize, initialized, model_list).as_bytes())
        .await
        .map_err(|error| internal(format!("Codex 모델 목록 요청을 보낼 수 없습니다: {error}")))?;
    stdin.flush().await.map_err(|error| {
        internal(format!(
            "Codex 모델 목록 요청을 완료할 수 없습니다: {error}"
        ))
    })?;

    let mut lines = BufReader::new(stdout).lines();
    let response = timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await.map_err(|error| error.to_string())? {
            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id") == Some(&serde_json::Value::from(2)) {
                return Ok::<_, String>(value);
            }
        }
        Err("Codex 모델 목록 응답이 없습니다.".into())
    })
    .await
    .map_err(|_| internal("Codex 모델 목록 조회 시간이 초과되었습니다."))?
    .map_err(internal)?;
    let _ = child.kill().await;

    let models = response
        .get("result")
        .and_then(|result| result.get("data"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| internal("Codex 모델 목록 형식을 해석할 수 없습니다."))?;
    let options = models
        .iter()
        .filter(|model| {
            !model
                .get("hidden")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|model| {
            let id = model.get("model").or_else(|| model.get("id"))?.as_str()?;
            let display_name = model
                .get("displayName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(id);
            Some(AiModelOption {
                id: id.into(),
                display_name: display_name.into(),
            })
        })
        .collect::<Vec<_>>();
    if options.is_empty() {
        return Err(internal("Codex에서 사용할 수 있는 모델을 찾지 못했습니다."));
    }
    Ok(options)
}

fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
