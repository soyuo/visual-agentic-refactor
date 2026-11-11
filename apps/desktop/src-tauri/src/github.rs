use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::command_error::{CommandError, CommandErrorCode};
use crate::credentials::{self, CredentialProvider};

const MAX_RESPONSE_BYTES: usize = 1_048_576;

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
    let output = Command::new("git")
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
        .or_else(|| raw.strip_prefix("git@github.com:"))?;
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let name = parts.next()?.trim();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((owner.to_owned(), name.to_owned()))
}

fn gh_api(project: &Path, endpoint: &str) -> Result<serde_json::Value, CommandError> {
    let mut command = Command::new("gh");
    command.args(["api", endpoint]).current_dir(project);
    if let Some(token) = credentials::read_for_provider(CredentialProvider::GitHub)? {
        command.env("GH_TOKEN", token);
    }
    let output = command
        .output()
        .map_err(|_| external("GitHub CLI를 실행하지 못했습니다"))?;
    if output.stdout.len() > MAX_RESPONSE_BYTES {
        return Err(external("GitHub 응답이 너무 큽니다"));
    }
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        let message = if error.contains("rate limit") {
            "GitHub 요청 한도를 초과했습니다"
        } else if error.contains("forbidden") || error.contains("permission") {
            "GitHub 권한이 없습니다"
        } else {
            "GitHub 요청에 실패했습니다"
        };
        return Err(external(message));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| external("GitHub 응답을 해석하지 못했습니다"))
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
        assert_eq!(parse_github_remote("https://gitlab.com/acme/app"), None);
    }
}
