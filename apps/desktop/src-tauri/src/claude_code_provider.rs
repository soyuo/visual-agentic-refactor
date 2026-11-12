use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::ai_provider::{
    AiProvider, ProviderCapabilities, ProviderDescriptor, ProviderError, ProviderErrorCode,
    ProviderResponse, ProviderUsage, RetryPolicy, ReviewRequest, ensure_not_cancelled,
    validate_request,
};
use crate::local_cli::resolve_command;
use crate::state::TaskControl;

const PACKAGE: &str = "@anthropic-ai/claude-code";

pub struct ClaudeCodeProvider {
    project_root: PathBuf,
}

impl ClaudeCodeProvider {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

#[async_trait::async_trait]
impl AiProvider for ClaudeCodeProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "claude-code".into(),
            display_name: "Claude Code (공식 앱)".into(),
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
        ensure_cli_available().await?;
        let executable = resolve_command("claude").ok_or_else(cli_unavailable_error)?;
        let mut child = crate::local_cli::tokio_command(executable)
            .current_dir(&self.project_root)
            .args([
                "-p",
                "--output-format",
                "json",
                "--input-format",
                "text",
                "--max-turns",
                "1",
                "--permission-mode",
                "plan",
                "--disallowedTools",
                "Edit",
                "Write",
                "Bash",
                "--model",
                request.model.as_str(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| cli_unavailable_error())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.context.as_bytes())
                .await
                .map_err(|_| cli_unavailable_error())?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|_| cli_unavailable_error())?;
        ensure_not_cancelled(task)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            if stderr.contains("login")
                || stderr.contains("authenticate")
                || stderr.contains("not logged")
                || stderr.contains("unauthorized")
            {
                launch_interactive_cli(&self.project_root);
                return Err(ProviderError::new(
                    ProviderErrorCode::Authentication,
                    "Claude Code 로그인 창을 열었습니다. Claude 앱의 Pro/Max 구독 로그인을 완료한 뒤 다시 시도해 주세요.",
                ));
            }
            return Err(ProviderError::new(
                ProviderErrorCode::ProviderUnavailable,
                "공식 Claude Code 실행에 실패했습니다.",
            ));
        }
        parse_output(&output.stdout, &request.model)
    }
}

fn launch_interactive_cli(project_root: &PathBuf) {
    let Some(executable) = resolve_command("claude") else {
        return;
    };
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .current_dir(project_root)
            .args([
                "/C",
                "start",
                "Claude Code 로그인",
                executable.to_string_lossy().as_ref(),
            ])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new(executable)
            .current_dir(project_root)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn();
    }
}

async fn ensure_cli_available() -> Result<(), ProviderError> {
    if let Some(executable) = resolve_command("claude") {
        if crate::local_cli::tokio_command(executable)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
        {
            return Ok(());
        }
    }
    let npm = resolve_command("npm").ok_or_else(cli_unavailable_error)?;
    let status = crate::local_cli::tokio_command(npm)
        .args(["install", "-g", PACKAGE])
        .status()
        .await
        .map_err(|_| cli_unavailable_error())?;
    if status.success() {
        Ok(())
    } else {
        Err(cli_unavailable_error())
    }
}

fn parse_output(bytes: &[u8], requested_model: &str) -> Result<ProviderResponse, ProviderError> {
    let body: Value = serde_json::from_slice(bytes).map_err(|_| invalid_response())?;
    let text = body
        .get("result")
        .and_then(Value::as_str)
        .or_else(|| body.as_str())
        .ok_or_else(invalid_response)?;
    let structured_output = serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(invalid_response)?;
    let usage = body.get("usage");
    Ok(ProviderResponse {
        provider_id: "claude-code".into(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(requested_model)
            .into(),
        structured_output,
        usage: ProviderUsage {
            input_tokens: usage
                .and_then(|value| value.get("input_tokens"))
                .and_then(Value::as_u64),
            output_tokens: usage
                .and_then(|value| value.get("output_tokens"))
                .and_then(Value::as_u64),
        },
    })
}

fn invalid_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorCode::InvalidResponse,
        "Claude Code가 구조화된 JSON 결과를 반환하지 않았습니다.",
    )
}
fn cli_unavailable_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorCode::ProviderUnavailable,
        "공식 Claude Code를 실행할 수 없습니다. Node.js/npm 설치와 PATH를 확인해 주세요.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_print_mode_json_result_without_secret_material() {
        let output = serde_json::json!({"type":"result","model":"sonnet","result":"{\"findings\":[]}","usage":{"input_tokens":12,"output_tokens":3}});
        let response = parse_output(output.to_string().as_bytes(), "default").unwrap();
        assert_eq!(response.provider_id, "claude-code");
        assert_eq!(
            response.structured_output,
            serde_json::json!({"findings": []})
        );
        assert_eq!(response.usage.input_tokens, Some(12));
    }
}
