use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::command_error::{CommandError, CommandErrorCode};
use crate::credentials::{self, CredentialProvider};

const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_ERROR_BYTES: usize = 64 * 1024;

fn read_bounded<R: Read>(mut reader: R, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(limit.saturating_add(1));
    let mut buffer = [0_u8; 8192];
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    bytes
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRepository {
    pub owner: String,
    pub name: String,
    pub html_url: String,
    pub default_branch: String,
    pub private: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubIssueOrPullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub is_pull_request: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubProjectRequest {
    pub project_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubNumberRequest {
    pub project_path: String,
    pub number: u64,
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}

fn external(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}

fn project_dir(path: &str) -> Result<PathBuf, CommandError> {
    let value = Path::new(path);
    if path.trim().is_empty() || !value.is_dir() {
        return Err(invalid("프로젝트 경로가 올바르지 않습니다"));
    }
    value
        .canonicalize()
        .map_err(|_| invalid("프로젝트 경로를 확인하지 못했습니다"))
}

fn repository_slug(project: &Path) -> Result<(String, String), CommandError> {
    let output = crate::local_cli::std_command("git")
        .arg("-C")
        .arg(project)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|_| external("Git 원격 저장소를 확인하지 못했습니다"))?;
    if !output.status.success() {
        return Err(external("origin 원격 저장소가 없습니다"));
    }
    parse_github_remote(String::from_utf8_lossy(&output.stdout).trim())
        .ok_or_else(|| invalid("origin은 GitHub 저장소여야 합니다"))
}

fn parse_github_remote(value: &str) -> Option<(String, String)> {
    let raw = value.strip_suffix(".git").unwrap_or(value);
    let path = raw
        .strip_prefix("https://github.com/")
        .or_else(|| raw.strip_prefix("http://github.com/"))
        .or_else(|| raw.strip_prefix("git@github.com:"))
        .or_else(|| raw.strip_prefix("ssh://git@github.com/"))?;
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let name = parts.next()?.trim();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((owner.to_owned(), name.to_owned()))
}

fn gh_api(project: &Path, endpoint: &str) -> Result<serde_json::Value, CommandError> {
    let mut command = crate::local_cli::std_command("gh");
    command
        .args(["api", endpoint])
        .current_dir(project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(token) = credentials::read_for_provider(CredentialProvider::GitHub)? {
        command.env("GH_TOKEN", token);
    }
    let mut child = command
        .spawn()
        .map_err(|_| external("GitHub CLI를 실행하지 못했습니다"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| external("GitHub CLI 출력을 읽지 못했습니다"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| external("GitHub CLI 오류 출력을 읽지 못했습니다"))?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, MAX_RESPONSE_BYTES));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, MAX_ERROR_BYTES));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child
            .try_wait()
            .map_err(|_| external("GitHub CLI 상태를 확인하지 못했습니다"))?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(external("GitHub 요청 시간이 초과했습니다"));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let status = child
        .try_wait()
        .map_err(|_| external("GitHub CLI 상태를 확인하지 못했습니다"))?
        .ok_or_else(|| external("GitHub CLI가 아직 종료되지 않았습니다"))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| external("GitHub CLI 출력을 읽지 못했습니다"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| external("GitHub CLI 오류 출력을 읽지 못했습니다"))?;
    if stdout.len() > MAX_RESPONSE_BYTES {
        return Err(external("GitHub 응답이 너무 큽니다"));
    }
    if !status.success() {
        let error = String::from_utf8_lossy(&stderr).to_ascii_lowercase();
        let message = if error.contains("rate limit") {
            "GitHub 요청 한도를 초과했습니다"
        } else if error.contains("forbidden") || error.contains("permission") {
            "GitHub 권한이 없습니다"
        } else {
            "GitHub 요청에 실패했습니다"
        };
        return Err(external(message));
    }
    serde_json::from_slice(&stdout).map_err(|_| external("GitHub 응답을 해석하지 못했습니다"))
}

pub fn get_repository(request: GitHubProjectRequest) -> Result<GitHubRepository, CommandError> {
    let project = project_dir(&request.project_path)?;
    let (owner, name) = repository_slug(&project)?;
    let value = gh_api(&project, &format!("repos/{owner}/{name}"))?;
    Ok(GitHubRepository {
        owner,
        name,
        html_url: value["html_url"].as_str().unwrap_or_default().to_owned(),
        default_branch: value["default_branch"]
            .as_str()
            .unwrap_or("main")
            .to_owned(),
        private: value["private"].as_bool().unwrap_or(false),
    })
}

pub fn get_issue_or_pull_request(
    request: GitHubNumberRequest,
) -> Result<GitHubIssueOrPullRequest, CommandError> {
    if request.number == 0 {
        return Err(invalid("이슈 또는 PR 번호가 올바르지 않습니다"));
    }
    let project = project_dir(&request.project_path)?;
    let (owner, name) = repository_slug(&project)?;
    let value = gh_api(
        &project,
        &format!("repos/{owner}/{name}/issues/{}", request.number),
    )?;
    Ok(GitHubIssueOrPullRequest {
        number: value["number"].as_u64().unwrap_or(request.number),
        title: value["title"].as_str().unwrap_or_default().to_owned(),
        state: value["state"].as_str().unwrap_or_default().to_owned(),
        html_url: value["html_url"].as_str().unwrap_or_default().to_owned(),
        is_pull_request: value.get("pull_request").is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_github_remote;

    #[test]
    fn parses_only_github_remotes() {
        assert_eq!(
            parse_github_remote("https://github.com/acme/app.git"),
            Some(("acme".into(), "app".into()))
        );
        assert_eq!(
            parse_github_remote("git@github.com:acme/app"),
            Some(("acme".into(), "app".into()))
        );
        assert_eq!(
            parse_github_remote("ssh://git@github.com/acme/app.git"),
            Some(("acme".into(), "app".into()))
        );
        assert_eq!(parse_github_remote("https://gitlab.com/acme/app"), None);
    }
}
