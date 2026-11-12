//! Pull request maintenance: AI-assisted merge-conflict resolution and CI repair.
//!
//! Safety model:
//! - All edits happen in an isolated worktree (`<parent>/.visual-agentic-refactor-workspaces/pr-<n>`),
//!   never in the user's working tree.
//! - AI output is restricted to the conflicted files (conflicts) or to tracked files referenced by the
//!   failing CI log (CI repair); results containing conflict markers or foreign paths are rejected.
//! - Commits, pushes, and merges require an explicit `approved: true` from the UI and never force-push.
//!   Pushes and merges are pinned to the expected head SHA.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::ai_provider::{RetryPolicy, ReviewRequest, ReviewSelection, validate_request};
use crate::command_error::{CommandError, CommandErrorCode};
use crate::commands::fix_patch_agent::normalize_patch;
use crate::commands::graph_diagram::{build_provider, provider_error};
use crate::context_builder::is_sensitive_path;
use crate::credentials::CredentialProvider;
use crate::local_cli::std_command;
use crate::state::AppState;

const MAX_FILE_BYTES: usize = 200 * 1024;
const MAX_CONTEXT_BYTES: usize = 400 * 1024;
const MAX_LOG_BYTES: usize = 60 * 1024;
const MAX_CI_FILES: usize = 8;

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidRequest, message)
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && !value.contains("..")
        && !value.contains("//")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'/' | b'.'))
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn safe_relative(path: &str) -> bool {
    let candidate = Path::new(path);
    !path.is_empty()
        && !candidate.is_absolute()
        && candidate.components().all(|part| matches!(part, Component::Normal(_)))
        && !is_sensitive_path(path)
}

fn run(program: &str, cwd: &Path, args: &[&str]) -> Result<String, CommandError> {
    let output = std_command(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| internal(format!("{program} 실행 실패: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(invalid(format!(
            "{program} {} 실패: {}",
            args.first().copied().unwrap_or_default(),
            stderr.trim().chars().take(600).collect::<String>()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_allow_fail(program: &str, cwd: &Path, args: &[&str]) -> Result<(bool, String), CommandError> {
    let output = std_command(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| internal(format!("{program} 실행 실패: {error}")))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), text))
}

fn workspace_for(root: &Path, number: u64) -> Result<PathBuf, CommandError> {
    let parent = root
        .parent()
        .ok_or_else(|| invalid("프로젝트 상위 폴더를 찾을 수 없습니다."))?;
    Ok(parent
        .join(".visual-agentic-refactor-workspaces")
        .join(format!("pr-{number}")))
}

fn existing_workspace(root: &Path, number: u64) -> Result<PathBuf, CommandError> {
    let path = workspace_for(root, number)?;
    if !path.join(".git").exists() {
        return Err(invalid("PR 작업 공간을 먼저 준비해 주세요."));
    }
    Ok(path)
}

fn check_model(model: &str) -> Result<String, CommandError> {
    let model = model.trim();
    if model.is_empty() || model.len() > 128 {
        return Err(invalid("모델 ID를 입력해 주세요."));
    }
    Ok(model.to_owned())
}

// ---------- PR status ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRef {
    pub project_path: String,
    pub number: u64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCheck {
    pub name: String,
    /// `success`, `failure`, `pending`, or `skipped`.
    pub state: String,
    pub link: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStatus {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_sha: String,
    /// `MERGEABLE`, `CONFLICTING`, or `UNKNOWN`.
    pub mergeable: String,
    pub checks: Vec<PullRequestCheck>,
    /// `success`, `failure`, `pending`, or `none`.
    pub checks_state: String,
    pub workspace_ready: bool,
}

fn check_state(item: &Value) -> String {
    let status = item.get("status").and_then(Value::as_str).unwrap_or_default();
    let conclusion = item
        .get("conclusion")
        .and_then(Value::as_str)
        .or_else(|| item.get("state").and_then(Value::as_str))
        .unwrap_or_default()
        .to_ascii_uppercase();
    if !status.is_empty() && status != "COMPLETED" {
        return "pending".into();
    }
    match conclusion.as_str() {
        "SUCCESS" => "success",
        "NEUTRAL" | "SKIPPED" => "skipped",
        "PENDING" | "EXPECTED" | "" => "pending",
        _ => "failure",
    }
    .into()
}

fn summarize_checks(checks: &[PullRequestCheck]) -> String {
    if checks.is_empty() {
        "none"
    } else if checks.iter().any(|c| c.state == "failure") {
        "failure"
    } else if checks.iter().any(|c| c.state == "pending") {
        "pending"
    } else {
        "success"
    }
    .into()
}

fn load_status(root: &Path, number: u64) -> Result<PullRequestStatus, CommandError> {
    let raw = run(
        "gh",
        root,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "number,title,url,state,headRefName,baseRefName,headRefOid,mergeable,statusCheckRollup",
        ],
    )?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|_| internal("gh 응답을 해석할 수 없습니다."))?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let checks: Vec<PullRequestCheck> = value
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| PullRequestCheck {
                    name: item
                        .get("name")
                        .or_else(|| item.get("context"))
                        .and_then(Value::as_str)
                        .unwrap_or("check")
                        .to_owned(),
                    state: check_state(item),
                    link: item
                        .get("detailsUrl")
                        .or_else(|| item.get("targetUrl"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    let head_ref = text("headRefName");
    let base_ref = text("baseRefName");
    let head_sha = text("headRefOid");
    if !valid_branch(&head_ref) || !valid_branch(&base_ref) || !valid_sha(&head_sha) {
        return Err(invalid("PR 브랜치 정보가 올바르지 않습니다."));
    }
    Ok(PullRequestStatus {
        number,
        title: text("title"),
        url: text("url"),
        state: text("state"),
        head_ref,
        base_ref,
        head_sha,
        mergeable: text("mergeable"),
        checks_state: summarize_checks(&checks),
        checks,
        workspace_ready: workspace_for(root, number)?.join(".git").exists(),
    })
}

#[tauri::command]
pub fn inspect_pull_request(request: PullRequestRef) -> Result<PullRequestStatus, CommandError> {
    load_status(Path::new(&request.project_path), request.number)
}

// ---------- Workspace ----------

#[tauri::command]
pub fn prepare_pull_request_workspace(
    request: PullRequestRef,
) -> Result<PullRequestStatus, CommandError> {
    let root = Path::new(&request.project_path);
    let status = load_status(root, request.number)?;
    let workspace = workspace_for(root, request.number)?;
    run("git", root, &["fetch", "origin", &status.head_ref, &status.base_ref])?;
    if workspace.join(".git").exists() {
        // Reset the isolated worktree to the latest remote head; it only holds our own work.
        let _ = run_allow_fail("git", &workspace, &["merge", "--abort"]);
        run("git", &workspace, &["checkout", "--detach", &status.head_sha])?;
        run("git", &workspace, &["reset", "--hard", &status.head_sha])?;
        run("git", &workspace, &["clean", "-fd"])?;
    } else {
        if let Some(parent) = workspace.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| internal(format!("작업 공간 폴더 생성 실패: {error}")))?;
        }
        let path = workspace.to_string_lossy().into_owned();
        run("git", root, &["worktree", "add", "--detach", &path, &status.head_sha])?;
    }
    load_status(root, request.number)
}

#[tauri::command]
pub fn remove_pull_request_workspace(request: PullRequestRef) -> Result<(), CommandError> {
    let root = Path::new(&request.project_path);
    let workspace = workspace_for(root, request.number)?;
    if workspace.join(".git").exists() {
        let path = workspace.to_string_lossy().into_owned();
        run("git", root, &["worktree", "remove", "--force", &path])?;
    }
    Ok(())
}

// ---------- Conflicts ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictMergeResult {
    pub clean: bool,
    pub conflicted_files: Vec<String>,
}

fn conflicted_files(workspace: &Path) -> Result<Vec<String>, CommandError> {
    Ok(run("git", workspace, &["diff", "--name-only", "--diff-filter=U"])?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Starts merging `origin/<base>` into the PR head inside the isolated worktree, without committing.
#[tauri::command]
pub fn start_pull_request_merge(
    request: PullRequestRef,
) -> Result<ConflictMergeResult, CommandError> {
    let root = Path::new(&request.project_path);
    let status = load_status(root, request.number)?;
    let workspace = existing_workspace(root, request.number)?;
    let head = run("git", &workspace, &["rev-parse", "HEAD"])?;
    if head.trim() != status.head_sha {
        return Err(invalid("작업 공간이 최신 PR head가 아닙니다. 작업 공간을 다시 준비해 주세요."));
    }
    run("git", &workspace, &["fetch", "origin", &status.base_ref])?;
    let base = format!("origin/{}", status.base_ref);
    let (clean, _) = run_allow_fail("git", &workspace, &["merge", "--no-commit", "--no-ff", &base])?;
    let files = conflicted_files(&workspace)?;
    Ok(ConflictMergeResult { clean: clean && files.is_empty(), conflicted_files: files })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestAiRequest {
    pub project_path: String,
    pub number: u64,
    pub provider: CredentialProvider,
    pub model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChangeResult {
    pub summary: String,
    pub files: Vec<String>,
    pub diff: String,
    pub provider_id: String,
    pub model: String,
}

fn read_workspace_file(workspace: &Path, relative: &str) -> Result<String, CommandError> {
    if !safe_relative(relative) {
        return Err(invalid(format!("전송할 수 없는 파일입니다: {relative}")));
    }
    let bytes = std::fs::read(workspace.join(relative))
        .map_err(|error| internal(format!("{relative} 읽기 실패: {error}")))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid(format!("{relative} 파일이 너무 큽니다.")));
    }
    String::from_utf8(bytes).map_err(|_| invalid(format!("{relative}는 텍스트 파일이 아닙니다.")))
}

pub(crate) fn has_conflict_markers(content: &str) -> bool {
    content.lines().any(|line| {
        line.starts_with("<<<<<<< ") || line.starts_with(">>>>>>> ") || line == "======="
    })
}

async fn ask_ai(
    app_state: &AppState,
    provider: CredentialProvider,
    workspace: PathBuf,
    first_path: String,
    model: String,
    context: String,
    schema: Value,
) -> Result<(Value, String, String), CommandError> {
    let request = ReviewRequest {
        selection: ReviewSelection::File { relative_path: first_path, range: None },
        context,
        model,
        max_context_tokens: 128_000,
        max_output_tokens: Some(32_768),
        output_schema: Some(schema),
    };
    let provider = build_provider(provider, workspace)?;
    validate_request(&request, &provider.descriptor()).map_err(provider_error)?;
    let control = app_state.register_task()?;
    let response = provider
        .review(&request, &control, RetryPolicy::default())
        .await
        .map_err(provider_error)?;
    Ok((response.structured_output, response.provider_id, response.model))
}

/// Validates AI conflict resolutions: only conflicted paths, no leftover markers, no duplicates.
pub(crate) fn validate_resolutions(
    output: &Value,
    allowed: &BTreeSet<String>,
) -> Result<Vec<(String, String)>, CommandError> {
    let items = output
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("AI가 해결 결과를 반환하지 않았습니다."))?;
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    for item in items {
        let path = item.get("path").and_then(Value::as_str).unwrap_or_default().to_owned();
        let content = item.get("content").and_then(Value::as_str).unwrap_or_default().to_owned();
        if !allowed.contains(&path) {
            return Err(invalid(format!("AI가 충돌 파일이 아닌 경로를 수정하려 했습니다: {path}")));
        }
        if !seen.insert(path.clone()) {
            return Err(invalid(format!("AI 결과에 중복 경로가 있습니다: {path}")));
        }
        if has_conflict_markers(&content) {
            return Err(invalid(format!("AI 결과에 충돌 표시가 남아 있습니다: {path}")));
        }
        resolved.push((path, content));
    }
    if seen.len() != allowed.len() {
        return Err(invalid("AI가 모든 충돌 파일을 해결하지 않았습니다."));
    }
    Ok(resolved)
}

#[tauri::command]
pub async fn resolve_pull_request_conflicts(
    request: PullRequestAiRequest,
    app_state: State<'_, AppState>,
) -> Result<AiChangeResult, CommandError> {
    let model = check_model(&request.model)?;
    let root = PathBuf::from(&request.project_path);
    let workspace = existing_workspace(&root, request.number)?;
    let files = conflicted_files(&workspace)?;
    if files.is_empty() {
        return Err(invalid("해결할 충돌 파일이 없습니다. 먼저 base 병합을 시작해 주세요."));
    }
    let mut context = String::from(
        "아래 파일들은 git merge 충돌 상태다. 양쪽 변경 의도를 모두 보존하도록 충돌을 해결해줘.\n\
         규칙: 충돌 구간만 해결하고 나머지 코드는 그대로 둬. 충돌 표시(<<<<<<<, =======, >>>>>>>)를 남기지 마. \
         제시된 파일만, 각 파일의 전체 최종 내용을 JSON {\"summary\": string, \"files\": [{\"path\": string, \"content\": string}]}로만 반환해.\n",
    );
    for file in &files {
        let content = read_workspace_file(&workspace, file)?;
        context.push_str(&format!("\n=== FILE: {file} ===\n{content}\n=== END FILE ===\n"));
        if context.len() > MAX_CONTEXT_BYTES {
            return Err(invalid("충돌 파일이 너무 커서 AI에 보낼 수 없습니다."));
        }
    }
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["summary", "files"],
        "properties": {
            "summary": { "type": "string" },
            "files": { "type": "array", "items": {
                "type": "object", "additionalProperties": false, "required": ["path", "content"],
                "properties": { "path": { "type": "string" }, "content": { "type": "string" } }
            }}
        }
    });
    let (output, provider_id, model) = ask_ai(
        &app_state, request.provider, workspace.clone(), files[0].clone(), model, context, schema,
    )
    .await?;
    let allowed: BTreeSet<String> = files.iter().cloned().collect();
    let resolved = validate_resolutions(&output, &allowed)?;
    for (path, content) in &resolved {
        std::fs::write(workspace.join(path), content)
            .map_err(|error| internal(format!("{path} 쓰기 실패: {error}")))?;
    }
    let mut diff_args = vec!["diff", "HEAD", "--"];
    diff_args.extend(files.iter().map(String::as_str));
    let diff = run("git", &workspace, &diff_args)?;
    Ok(AiChangeResult {
        summary: output.get("summary").and_then(Value::as_str).unwrap_or_default().to_owned(),
        files,
        diff,
        provider_id,
        model,
    })
}

// ---------- CI repair ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CiFailureContext {
    pub run_id: String,
    pub log_excerpt: String,
    pub candidate_files: Vec<String>,
}

fn failed_run(root: &Path, status: &PullRequestStatus) -> Result<String, CommandError> {
    let raw = run(
        "gh",
        root,
        &[
            "run", "list", "--branch", &status.head_ref, "--limit", "20", "--json",
            "databaseId,conclusion,headSha",
        ],
    )?;
    let runs: Value = serde_json::from_str(&raw).map_err(|_| internal("gh run 응답 해석 실패"))?;
    runs.as_array()
        .into_iter()
        .flatten()
        .find(|item| {
            item.get("headSha").and_then(Value::as_str) == Some(status.head_sha.as_str())
                && matches!(
                    item.get("conclusion").and_then(Value::as_str),
                    Some("failure" | "timed_out" | "startup_failure")
                )
        })
        .and_then(|item| item.get("databaseId").and_then(Value::as_u64))
        .map(|id| id.to_string())
        .ok_or_else(|| invalid("현재 PR head에서 실패한 GitHub Actions 실행을 찾지 못했습니다."))
}

/// Keeps the tail of the log, where failures usually are, within the byte budget.
pub(crate) fn tail_log(log: &str, limit: usize) -> String {
    if log.len() <= limit {
        return log.to_owned();
    }
    let mut start = log.len() - limit;
    while !log.is_char_boundary(start) {
        start += 1;
    }
    format!("…(앞부분 생략)\n{}", &log[start..])
}

/// Picks tracked files whose path (or unique file name) appears in the CI log.
pub(crate) fn referenced_files(log: &str, tracked: &[String]) -> Vec<String> {
    let normalized = log.replace('\\', "/");
    let mut picked = Vec::new();
    for path in tracked {
        if picked.len() >= MAX_CI_FILES {
            break;
        }
        if !safe_relative(path) {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        let name_unique = tracked.iter().filter(|other| other.ends_with(&format!("/{name}")) || *other == name).count() == 1;
        if normalized.contains(path.as_str()) || (name.contains('.') && name.len() > 4 && name_unique && normalized.contains(name)) {
            picked.push(path.clone());
        }
    }
    picked
}

fn ci_context(root: &Path, workspace: &Path, status: &PullRequestStatus) -> Result<CiFailureContext, CommandError> {
    let run_id = failed_run(root, status)?;
    let (_, log) = run_allow_fail("gh", root, &["run", "view", &run_id, "--log-failed"])?;
    let excerpt = tail_log(&log, MAX_LOG_BYTES);
    let tracked: Vec<String> = run("git", workspace, &["ls-files"])?.lines().map(str::to_owned).collect();
    Ok(CiFailureContext { run_id, candidate_files: referenced_files(&excerpt, &tracked), log_excerpt: excerpt })
}

/// Returns the failing log and the files that would be sent, so the user can approve the transfer.
#[tauri::command]
pub fn preview_pull_request_ci_failure(request: PullRequestRef) -> Result<CiFailureContext, CommandError> {
    let root = Path::new(&request.project_path);
    let status = load_status(root, request.number)?;
    let workspace = existing_workspace(root, request.number)?;
    ci_context(root, &workspace, &status)
}

pub(crate) fn patch_paths(patch: &str) -> BTreeSet<String> {
    patch
        .lines()
        .filter_map(|line| line.strip_prefix("+++ ").or_else(|| line.strip_prefix("--- ")))
        .map(|path| path.split('\t').next().unwrap_or(path).trim())
        .filter(|path| *path != "/dev/null")
        .map(|path| path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path).to_owned())
        .collect()
}

#[tauri::command]
pub async fn repair_pull_request_ci(
    request: PullRequestAiRequest,
    app_state: State<'_, AppState>,
) -> Result<AiChangeResult, CommandError> {
    let model = check_model(&request.model)?;
    let root = PathBuf::from(&request.project_path);
    let status = load_status(&root, request.number)?;
    let workspace = existing_workspace(&root, request.number)?;
    let head = run("git", &workspace, &["rev-parse", "HEAD"])?;
    if head.trim() != status.head_sha {
        return Err(invalid("작업 공간이 최신 PR head가 아닙니다. 작업 공간을 다시 준비해 주세요."));
    }
    if !run("git", &workspace, &["status", "--porcelain"])?.trim().is_empty() {
        return Err(invalid("작업 공간에 커밋되지 않은 변경이 있습니다."));
    }
    let ci = ci_context(&root, &workspace, &status)?;
    if ci.candidate_files.is_empty() {
        return Err(invalid("CI 로그에서 수정 대상 파일을 찾지 못했습니다. 직접 확인이 필요합니다."));
    }
    let mut context = format!(
        "GitHub Actions CI가 실패했다. 아래 실패 로그와 관련 파일을 보고 원인을 고치는 최소 변경 패치를 만들어줘.\n\
         규칙: 허용 파일만 수정하고, 테스트를 지우거나 건너뛰게 하거나 검사를 약화하지 마. \
         git apply로 적용 가능한 unified diff(--- a/<path>, +++ b/<path>)를 만들어. \
         원인을 고칠 수 없으면 patch를 빈 문자열로 둬. JSON {{\"summary\": string, \"patch\": string}}로만 반환해.\n\
         허용 파일: {allowed}\n\n=== CI LOG (run {run}) ===\n{log}\n=== END LOG ===\n",
        allowed = ci.candidate_files.join(", "),
        run = ci.run_id,
        log = ci.log_excerpt,
    );
    for file in &ci.candidate_files {
        let content = read_workspace_file(&workspace, file)?;
        context.push_str(&format!("\n=== FILE: {file} ===\n{content}\n=== END FILE ===\n"));
        if context.len() > MAX_CONTEXT_BYTES {
            return Err(invalid("CI 관련 파일이 너무 커서 AI에 보낼 수 없습니다."));
        }
    }
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["summary", "patch"],
        "properties": { "summary": { "type": "string" }, "patch": { "type": "string" } }
    });
    let (output, provider_id, model) = ask_ai(
        &app_state, request.provider, workspace.clone(), ci.candidate_files[0].clone(), model, context, schema,
    )
    .await?;
    let summary = output.get("summary").and_then(Value::as_str).unwrap_or_default().to_owned();
    let patch = output.get("patch").and_then(Value::as_str).map(normalize_patch).unwrap_or_default();
    if patch.trim().is_empty() {
        return Err(invalid(format!("AI가 패치를 만들지 못했습니다: {summary}")));
    }
    let allowed: BTreeSet<String> = ci.candidate_files.iter().cloned().collect();
    let touched = patch_paths(&patch);
    if touched.is_empty() || !touched.is_subset(&allowed) {
        return Err(invalid("AI 패치가 허용되지 않은 파일을 수정하려 했습니다."));
    }
    apply_patch(&workspace, &patch)?;
    let diff = run("git", &workspace, &["diff", "HEAD"])?;
    Ok(AiChangeResult { summary, files: touched.into_iter().collect(), diff, provider_id, model })
}

fn apply_patch(workspace: &Path, patch: &str) -> Result<(), CommandError> {
    for check in [true, false] {
        let mut args = vec!["apply", "--whitespace=nowarn"];
        if check {
            args.push("--check");
        }
        args.push("-");
        let mut child = std_command("git")
            .args(&args)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| internal(format!("git apply 실행 실패: {error}")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| internal("git apply 입력을 열 수 없습니다."))?
            .write_all(patch.as_bytes())
            .map_err(|error| internal(format!("git apply 입력 실패: {error}")))?;
        let output = child.wait_with_output().map_err(|error| internal(error.to_string()))?;
        if !output.status.success() {
            return Err(invalid(format!(
                "AI 패치를 적용할 수 없습니다: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    Ok(())
}

// ---------- Commit / push / merge ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushPullRequestUpdate {
    pub project_path: String,
    pub number: u64,
    pub message: String,
    pub expected_head_sha: String,
    pub approved: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushedPullRequestUpdate {
    pub commit_sha: String,
}

/// Commits the reviewed workspace changes and pushes them to the PR head branch (fast-forward only).
#[tauri::command]
pub fn push_pull_request_update(
    request: PushPullRequestUpdate,
) -> Result<PushedPullRequestUpdate, CommandError> {
    if !request.approved {
        return Err(invalid("커밋과 push는 사용자 승인이 필요합니다."));
    }
    let message = request.message.trim();
    if message.is_empty() || message.len() > 500 {
        return Err(invalid("커밋 메시지를 입력해 주세요."));
    }
    let root = Path::new(&request.project_path);
    let status = load_status(root, request.number)?;
    if status.head_sha != request.expected_head_sha {
        return Err(invalid("PR head가 그 사이 바뀌었습니다. 상태를 새로 고친 뒤 다시 시도해 주세요."));
    }
    let workspace = existing_workspace(root, request.number)?;
    if !conflicted_files(&workspace)?.is_empty() {
        return Err(invalid("아직 해결되지 않은 충돌 파일이 있습니다."));
    }
    let changed = run("git", &workspace, &["status", "--porcelain"])?;
    for line in changed.lines() {
        let path = line.get(3..).unwrap_or_default().trim().trim_matches('"');
        if path.is_empty() {
            continue;
        }
        if is_sensitive_path(path) {
            return Err(invalid(format!("민감 파일 변경은 커밋할 수 없습니다: {path}")));
        }
        if let Ok(content) = std::fs::read_to_string(workspace.join(path))
            && has_conflict_markers(&content)
        {
            return Err(invalid(format!("충돌 표시가 남아 있습니다: {path}")));
        }
    }
    run("git", &workspace, &["add", "-A"])?;
    run("git", &workspace, &["commit", "-m", message])?;
    let sha = run("git", &workspace, &["rev-parse", "HEAD"])?.trim().to_owned();
    let target = format!("HEAD:refs/heads/{}", status.head_ref);
    // Plain push: git rejects it unless it fast-forwards the remote head we started from.
    run("git", &workspace, &["push", "origin", &target])?;
    Ok(PushedPullRequestUpdate { commit_sha: sha })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePullRequest {
    pub project_path: String,
    pub number: u64,
    pub expected_head_sha: String,
    /// `merge`, `squash`, or `rebase`.
    pub method: String,
    pub approved: bool,
}

#[tauri::command]
pub fn merge_pull_request(request: MergePullRequest) -> Result<PullRequestStatus, CommandError> {
    if !request.approved {
        return Err(invalid("PR 병합은 사용자 승인이 필요합니다."));
    }
    let flag = match request.method.as_str() {
        "merge" => "--merge",
        "squash" => "--squash",
        "rebase" => "--rebase",
        _ => return Err(invalid("병합 방식이 올바르지 않습니다.")),
    };
    let root = Path::new(&request.project_path);
    let status = load_status(root, request.number)?;
    if status.head_sha != request.expected_head_sha {
        return Err(invalid("PR head가 그 사이 바뀌었습니다. 상태를 새로 고쳐 주세요."));
    }
    if status.mergeable != "MERGEABLE" {
        return Err(invalid("PR이 아직 병합 가능한 상태가 아닙니다."));
    }
    if !matches!(status.checks_state.as_str(), "success" | "none") {
        return Err(invalid("CI가 모두 성공해야 병합할 수 있습니다."));
    }
    run(
        "gh",
        root,
        &["pr", "merge", &request.number.to_string(), flag, "--match-head-commit", &status.head_sha],
    )?;
    load_status(root, request.number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_markers_and_foreign_paths() {
        let allowed: BTreeSet<String> = ["a.kt".to_owned()].into();
        let ok = json!({"files": [{"path": "a.kt", "content": "fun a() = 1\n"}]});
        assert_eq!(validate_resolutions(&ok, &allowed).unwrap().len(), 1);
        let marker = json!({"files": [{"path": "a.kt", "content": "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\n"}]});
        assert!(validate_resolutions(&marker, &allowed).is_err());
        let foreign = json!({"files": [{"path": "b.kt", "content": "x"}]});
        assert!(validate_resolutions(&foreign, &allowed).is_err());
        let missing = json!({"files": []});
        assert!(validate_resolutions(&missing, &allowed).is_err());
    }

    #[test]
    fn finds_files_in_log_and_patch() {
        let tracked = vec!["src/main/App.kt".to_owned(), ".env".to_owned(), "README.md".to_owned()];
        let log = "e: file:///home/runner/work/x/src/main/App.kt:12:3 Unresolved reference";
        assert_eq!(referenced_files(log, &tracked), vec!["src/main/App.kt".to_owned()]);
        let patch = "--- a/src/main/App.kt\n+++ b/src/main/App.kt\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(patch_paths(patch), ["src/main/App.kt".to_owned()].into());
        assert!(tail_log(&"가".repeat(100), 10).len() < 100);
    }
}
