use crate::command_error::CommandError;
use crate::github::{
    self, GitHubIssueOrPullRequest, GitHubNumberRequest, GitHubProjectRequest, GitHubRepository,
};

#[tauri::command]
pub fn get_github_repository(
    request: GitHubProjectRequest,
) -> Result<GitHubRepository, CommandError> {
    github::get_repository(request)
}

#[tauri::command]
pub fn get_github_issue_or_pull_request(
    request: GitHubNumberRequest,
) -> Result<GitHubIssueOrPullRequest, CommandError> {
    github::get_issue_or_pull_request(request)
}
