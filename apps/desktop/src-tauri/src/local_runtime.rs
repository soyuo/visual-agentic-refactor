use crate::command_error::{CommandError, CommandErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

const MAX_TIMEOUT_MS: u64 = 600_000;
const MAX_OUTPUT_BYTES: usize = 1_048_576;
const MAX_ARGS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLlmRequest {
    pub runtime: LocalRuntimeKind,
    pub executable_path: String,
    pub model_path: String,
    pub prompt: String,
    pub timeout_ms: Option<u64>,
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalRuntimeKind {
    LlamaCpp,
    Ollama,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLlmResponse {
    pub text: String,
    pub structured: Value,
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message.into())
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message.into())
}

pub fn run(request: LocalLlmRequest) -> Result<LocalLlmResponse, CommandError> {
    if request.prompt.trim().is_empty() {
        return Err(invalid("프롬프트가 비어 있습니다."));
    }
    if request.prompt.chars().count() > 200_000 {
        return Err(invalid("프롬프트가 너무 깁니다."));
    }
    let timeout = request.timeout_ms.unwrap_or(120_000);
    if timeout == 0 || timeout > MAX_TIMEOUT_MS {
        return Err(invalid("실행 시간 제한이 올바르지 않습니다."));
    }
    let args = request.args.unwrap_or_default();
    if args.len() > MAX_ARGS
        || args.iter().any(|arg| arg.len() > 4096)
        || args.iter().any(|arg| {
            matches!(arg.as_str(), "--model" | "--prompt" | "-m")
                || arg.starts_with("--model=")
                || arg.starts_with("--prompt=")
        })
    {
        return Err(invalid("실행 인자가 너무 많거나 깁니다."));
    }
    let executable = validate_file(&request.executable_path, "실행 파일")?;
    let model = validate_file(&request.model_path, "모델 파일")?;

    let mut command = crate::local_cli::std_command(executable);
    command
        .args(args)
        .arg("--model")
        .arg(model)
        .arg("--prompt")
        .arg(&request.prompt)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if matches!(request.runtime, LocalRuntimeKind::Ollama) {
        return Err(invalid("Ollama 런타임은 아직 지원되지 않습니다."));
    }
    let mut child = command
        .spawn()
        .map_err(|e| internal(format!("로컬 런타임을 시작하지 못했습니다: {e}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = thread::spawn(move || read_limited(stdout));
    let err_reader = thread::spawn(move || read_limited(stderr));
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| internal(e.to_string()))? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CommandError::new(
                CommandErrorCode::Internal,
                "로컬 런타임 실행 시간이 초과되었습니다.",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader
        .join()
        .map_err(|_| internal("로컬 런타임 출력을 읽지 못했습니다."))?;
    let stderr = err_reader
        .join()
        .map_err(|_| internal("로컬 런타임 오류 출력을 읽지 못했습니다."))?;
    let text = String::from_utf8_lossy(&stdout).trim().to_owned();
    if !status.success() {
        return Err(internal(format!(
            "로컬 런타임이 실패했습니다 ({}): {}",
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let structured = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(LocalLlmResponse { text, structured })
}

fn validate_file(raw: &str, label: &str) -> Result<std::path::PathBuf, CommandError> {
    if raw.trim().is_empty() {
        return Err(invalid(format!("{label} 경로가 비어 있습니다.")));
    }
    let path = fs::canonicalize(Path::new(raw))
        .map_err(|_| invalid(format!("{label} 경로를 확인할 수 없습니다.")))?;
    if !path.is_file() {
        return Err(invalid(format!("{label}이 파일이 아닙니다.")));
    }
    Ok(path)
}

fn read_limited<T: Read>(stream: Option<T>) -> Vec<u8> {
    let Some(mut stream) = stream else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    let _ = stream
        .take((MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes);
    bytes.truncate(MAX_OUTPUT_BYTES);
    bytes
}
