use super::fix_tasks::validate_stored_workspace_path;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::{
    FixTaskWorkspaceState, FixVerificationPolicy, FixVerificationRecord, FixVerificationResult,
    PersistenceState, StoredFixTask,
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tauri::State;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRequest {
    pub project_path: String,
    pub task_id: String,
    pub approved: bool,
    pub policy: Option<FixVerificationPolicy>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCandidates {
    pub task_id: String,
    pub candidates: Vec<VerificationCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCandidate {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub reason: String,
}

fn invalid(s: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidRequest, s.into())
}
fn internal(s: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, s.into())
}

#[tauri::command]
pub fn list_fix_verification_candidates(
    request: VerificationRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<VerificationCandidates, CommandError> {
    let task = load_ready(&request, &persistence)?;
    let root = Path::new(task.workspace_path.as_deref().unwrap());
    let mut candidates = Vec::new();
    if root.join("Cargo.toml").is_file() {
        candidates.push(VerificationCandidate {
            id: "cargo-check".into(),
            command: "cargo".into(),
            args: vec!["check".into(), "--offline".into()],
            reason: "Rust workspace".into(),
        });
    }
    if root.join("package.json").is_file() {
        candidates.push(VerificationCandidate {
            id: "npm-test".into(),
            command: "npm".into(),
            args: vec!["test".into(), "--".into(), "--runInBand".into()],
            reason: "package.json".into(),
        });
    }
    Ok(VerificationCandidates {
        task_id: request.task_id,
        candidates,
    })
}

#[tauri::command]
pub fn run_fix_verification(
    request: VerificationRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    if !request.approved {
        return Err(invalid("검증 실행은 명시적으로 승인해야 합니다."));
    }
    let task = load_ready(&request, &persistence)?;
    let workspace = validate_stored_workspace_path(
        Path::new(&request.project_path),
        task.workspace_path.as_deref().unwrap(),
        &request.task_id,
    )?;
    let policy = request.policy.unwrap_or(FixVerificationPolicy {
        timeout_ms: 120_000,
        output_limit_bytes: 32 * 1024,
        network_disabled: true,
    });
    if policy.timeout_ms == 0
        || policy.timeout_ms > 600_000
        || policy.output_limit_bytes == 0
        || policy.output_limit_bytes > 1024 * 1024
    {
        return Err(invalid("검증 정책 제한을 벗어났습니다."));
    }
    let candidates = candidate_list(workspace);
    let mut results = Vec::new();
    for candidate in candidates {
        let started = now();
        let mut command = Command::new(&candidate.command);
        command
            .args(&candidate.args)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if policy.network_disabled {
            command
                .env("CARGO_NET_OFFLINE", "true")
                .env("npm_config_offline", "true");
        }
        let result = match command.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let output_limit = policy.output_limit_bytes;
                let stdout_reader = thread::spawn(move || read_limited(stdout, output_limit));
                let stderr_reader = thread::spawn(move || read_limited(stderr, output_limit));
                let deadline = Instant::now() + Duration::from_millis(policy.timeout_ms);
                loop {
                    if let Some(status) = child.try_wait().map_err(|e| internal(e.to_string()))? {
                        let _ = child.wait().map_err(|e| internal(e.to_string()))?;
                        let mut text = join_output(stdout_reader, output_limit)?;
                        text.push_str(&join_output(stderr_reader, output_limit)?);
                        text.truncate(output_limit);
                        break FixVerificationResult {
                            id: candidate.id.clone(),
                            command: candidate.command.clone(),
                            args: candidate.args.clone(),
                            status: if status.success() {
                                "passed".into()
                            } else {
                                "failed".into()
                            },
                            exit_code: status.code(),
                            output_summary: text,
                            error: None,
                            started_at_unix_ms: started,
                            ended_at_unix_ms: now(),
                        };
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let mut text = join_output(stdout_reader, output_limit)?;
                        text.push_str(&join_output(stderr_reader, output_limit)?);
                        text.truncate(output_limit);
                        break FixVerificationResult {
                            id: candidate.id.clone(),
                            command: candidate.command.clone(),
                            args: candidate.args.clone(),
                            status: "cancelled".into(),
                            exit_code: None,
                            output_summary: text,
                            error: Some("시간 제한을 초과했습니다.".into()),
                            started_at_unix_ms: started,
                            ended_at_unix_ms: now(),
                        };
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            Err(error) => FixVerificationResult {
                id: candidate.id.clone(),
                command: candidate.command.clone(),
                args: candidate.args.clone(),
                status: "unavailable".into(),
                exit_code: None,
                output_summary: String::new(),
                error: Some(error.to_string()),
                started_at_unix_ms: started,
                ended_at_unix_ms: now(),
            },
        };
        results.push(result);
    }
    persistence
        .database()?
        .save_fix_verification(
            &request.project_path,
            &request.task_id,
            &FixVerificationRecord { policy, results },
        )
        .map_err(internal)
}
fn read_limited<T>(stream: Option<T>, limit: usize) -> Vec<u8>
where
    T: Read,
{
    let Some(mut stream) = stream else {
        return Vec::new();
    };
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let _ = stream
        .take((limit.saturating_add(1)) as u64)
        .read_to_end(&mut bytes);
    bytes
}

fn join_output(reader: thread::JoinHandle<Vec<u8>>, limit: usize) -> Result<String, CommandError> {
    reader
        .join()
        .map(|bytes| String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]).into_owned())
        .map_err(|_| internal("검증 출력 수집 작업이 중단되었습니다."))
}

fn load_ready(
    request: &VerificationRequest,
    persistence: &State<'_, PersistenceState>,
) -> Result<StoredFixTask, CommandError> {
    if request.project_path.trim().is_empty() || request.task_id.trim().is_empty() {
        return Err(invalid("검증 식별자가 비어 있습니다."));
    }
    let task = persistence
        .database()?
        .load_fix_task(&request.project_path, &request.task_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("수정 작업을 찾을 수 없습니다."))?;
    if task.workspace_state != FixTaskWorkspaceState::Ready || task.workspace_path.is_none() {
        return Err(invalid("격리 workspace가 준비되지 않았습니다."));
    }
    validate_stored_workspace_path(
        Path::new(&request.project_path),
        task.workspace_path.as_deref().unwrap(),
        &request.task_id,
    )?;
    Ok(task)
}
fn candidate_list(root: &Path) -> Vec<VerificationCandidate> {
    let mut v = Vec::new();
    if root.join("Cargo.toml").is_file() {
        v.push(VerificationCandidate {
            id: "cargo-check".into(),
            command: "cargo".into(),
            args: vec!["check".into(), "--offline".into()],
            reason: "Rust workspace".into(),
        });
    }
    if root.join("package.json").is_file() {
        v.push(VerificationCandidate {
            id: "npm-test".into(),
            command: "npm".into(),
            args: vec!["test".into(), "--".into(), "--runInBand".into()],
            reason: "package.json".into(),
        });
    }
    v
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
