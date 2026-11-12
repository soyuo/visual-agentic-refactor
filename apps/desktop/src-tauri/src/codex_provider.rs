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
        let output = run_exec(
            &executable,
            &request.context,
            &request.model,
            &self.project_root,
            true,
        )
        .await?;
        let output = if !output.status.success() && is_unsupported_model(&output) {
            run_exec(
                &executable,
                &request.context,
                &request.model,
                &self.project_root,
                false,
            )
            .await?
        } else {
            output
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            if stderr.contains("login")
                || stderr.contains("authenticate")
                || stderr.contains("unauthorized")
                || stdout.contains("login")
                || stdout.contains("authenticate")
                || stdout.contains("unauthorized")
            {
                launch_login(&self.project_root);
                return Err(ProviderError::new(
                    ProviderErrorCode::Authentication,
                    "Codex 로그인 창을 열었습니다. Codex 앱에서 ChatGPT 계정 로그인을 완료한 뒤 다시 시도해 주세요.",
                ));
            }
            let detail = extract_error_detail(&output).unwrap_or_else(|| "알 수 없는 오류".into());
            return Err(ProviderError::new(
                ProviderErrorCode::ProviderUnavailable,
                format!("Codex 요청에 실패했습니다: {detail}"),
            ));
        }
        parse_output(&output.stdout, &request.model)
    }
}

async fn run_exec(
    executable: &std::path::Path,
    context: &str,
    model: &str,
    project_root: &PathBuf,
    include_model: bool,
) -> Result<std::process::Output, ProviderError> {
    let mut command = crate::local_cli::tokio_command(executable);
    command.current_dir(project_root).args([
        "exec",
        "--json",
        "--ephemeral",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
    ]);
    if include_model {
        command.args(["--model", model]);
    }
    command.arg("-");
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| cli_unavailable())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(context.as_bytes())
            .await
            .map_err(|_| cli_unavailable())?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|_| cli_unavailable())?;
    Ok(output)
}

fn is_unsupported_model(output: &std::process::Output) -> bool {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    text.contains("model") && (text.contains("not supported") || text.contains("unsupported"))
}

fn extract_error_detail(output: &std::process::Output) -> Option<String> {
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if let Some(message) = value.get("message").and_then(Value::as_str) {
                return Some(message.to_owned());
            }
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                return Some(message.to_owned());
            }
        }
    }
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .map(str::to_owned)
}

pub fn is_logged_in() -> bool {
    let Some(executable) = resolve_command("codex") else {
        return false;
    };
    crate::local_cli::std_command(executable)
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

    #[test]
    fn detects_chatgpt_plan_model_rejection_for_fallback() {
        use std::os::windows::process::ExitStatusExt;

        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: br#"{"type":"error","message":"The 'gpt-5-codex' model is not supported when using Codex with a ChatGPT account."}"#.to_vec(),
            stderr: Vec::new(),
        };
        assert!(is_unsupported_model(&output));
        assert_eq!(
            extract_error_detail(&output).as_deref(),
            Some(
                "The 'gpt-5-codex' model is not supported when using Codex with a ChatGPT account."
            )
        );
    }
}
