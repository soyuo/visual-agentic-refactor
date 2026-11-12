use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::ai_provider::{
    AiProvider, ProviderCapabilities, ProviderDescriptor, ProviderError, ProviderErrorCode,
    ProviderResponse, ProviderUsage, RetryPolicy, ReviewRequest, ensure_not_cancelled,
    validate_request,
};
use crate::local_cli::resolve_command;
use crate::state::TaskControl;

pub struct CodexProvider {
    project_root: PathBuf,
}

impl CodexProvider {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

#[async_trait::async_trait]
impl AiProvider for CodexProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "codex".into(),
            display_name: "Codex (공식 앱/CLI)".into(),
            capabilities: ProviderCapabilities {
                context_window_tokens: None,
                supports_structured_output: true,
                supports_cancellation: false,
            },
        }
    }

    async fn review(
        &self,
        request: &ReviewRequest,
        task: &TaskControl,
        _retry_policy: RetryPolicy,
    ) -> Result<ProviderResponse, ProviderError> {
        let descriptor = self.descriptor();
        validate_request(request, &descriptor)?;
        ensure_not_cancelled(task)?;
        let executable = resolve_command("codex").ok_or_else(cli_unavailable)?;
        let mut child = Command::new(executable)
            .current_dir(&self.project_root)
            .args([
                "exec",
                "--json",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--model",
                request.model.as_str(),
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| cli_unavailable())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.context.as_bytes())
                .await
                .map_err(|_| cli_unavailable())?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|_| cli_unavailable())?;
        ensure_not_cancelled(task)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            if stderr.contains("login")
                || stderr.contains("authenticate")
                || stderr.contains("unauthorized")
            {
                launch_login(&self.project_root);
                return Err(ProviderError::new(
                    ProviderErrorCode::Authentication,
                    "Codex 로그인 창을 열었습니다. Codex 앱에서 ChatGPT 계정 로그인을 완료한 뒤 다시 시도해 주세요.",
                ));
            }
            return Err(ProviderError::new(
                ProviderErrorCode::ProviderUnavailable,
                "공식 Codex CLI 요청에 실패했습니다.",
            ));
        }
        parse_output(&output.stdout, &request.model)
    }
}

pub fn is_logged_in() -> bool {
    let Some(executable) = resolve_command("codex") else {
        return false;
    };
    std::process::Command::new(executable)
        .args(["login", "status"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn launch_login(project_root: &PathBuf) {
    let Some(executable) = resolve_command("codex") else {
        return;
    };
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .current_dir(project_root)
            .args([
                "/C",
                "start",
                "Codex login",
                executable.to_string_lossy().as_ref(),
                "login",
            ])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new(executable)
            .current_dir(project_root)
            .args(["login"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn();
    }
}

fn parse_output(bytes: &[u8], model: &str) -> Result<ProviderResponse, ProviderError> {
    let mut message = None;
    for line in String::from_utf8_lossy(bytes).lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if let Some(text) = find_agent_text(&value) {
                message = Some(text);
            }
        }
    }
    let text = message.ok_or_else(|| {
        ProviderError::new(
            ProviderErrorCode::InvalidResponse,
            "Codex가 구조화된 결과를 반환하지 않았습니다.",
        )
    })?;
    let structured_output = serde_json::from_str::<Value>(&text).map_err(|_| {
        ProviderError::new(
            ProviderErrorCode::InvalidResponse,
            "Codex 응답 JSON을 해석할 수 없습니다.",
        )
    })?;
    Ok(ProviderResponse {
        provider_id: "codex".into(),
        model: model.into(),
        structured_output,
        usage: ProviderUsage {
            input_tokens: None,
            output_tokens: None,
        },
    })
}

fn find_agent_text(value: &Value) -> Option<String> {
    if value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("agent_message")
    {
        if let Some(text) = value
            .get("item")
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        {
            return Some(text.to_owned());
        }
    }
    match value {
        Value::Object(map) => map.values().find_map(find_agent_text),
        Value::Array(values) => values.iter().find_map(find_agent_text),
        _ => None,
    }
}

fn cli_unavailable() -> ProviderError {
    ProviderError::new(
        ProviderErrorCode::ProviderUnavailable,
        "공식 Codex CLI를 찾을 수 없습니다.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_message_json_without_credentials() {
        let output = br#"{"type":"item.completed","item":{"type":"agent_message","text":"{\"title\":\"Architecture\",\"nodes\":[],\"edges\":[]}"}}"#;
        let response = parse_output(output, "gpt-5-codex").expect("valid Codex JSON event");
        assert_eq!(response.provider_id, "codex");
        assert_eq!(response.model, "gpt-5-codex");
        assert_eq!(response.structured_output["title"], "Architecture");
    }
}
