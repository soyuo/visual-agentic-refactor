use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const MAX_DISPLAYED_CHANGES: usize = 100;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusResponse {
    pub state: GitStatusState,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub tracked_count: u64,
    pub modified_count: u64,
    pub untracked_count: u64,
    pub changes: Vec<GitFileChange>,
    pub omitted_change_count: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitStatusState {
    Available,
    GitUnavailable,
    NotRepository,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitFileChangeKind {
    Tracked,
    Untracked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub path: String,
    pub kind: GitFileChangeKind,
    pub status_code: String,
}

pub fn inspect_project(path: &Path) -> GitStatusResponse {
    let root = match fs::canonicalize(path) {
        Ok(root) if root.is_dir() => root,
        _ => return error_status("선택한 프로젝트 경로를 확인할 수 없습니다."),
    };

    match run_git(None, &["--version"]) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable_status();
        }
        Err(_) => return error_status("Git 실행 파일을 확인하지 못했습니다."),
        Ok(output) if !output.status.success() => {
            return error_status("Git 실행 파일을 확인하지 못했습니다.");
        }
        Ok(_) => {}
    }

    let repository_check = match run_git(Some(&root), &["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) => output,
        Err(_) => return error_status("Git 저장소 여부를 확인하지 못했습니다."),
    };
    if !repository_check.status.success() {
        let message = String::from_utf8_lossy(&repository_check.stderr).to_ascii_lowercase();
        return if message.contains("not a git repository") {
            not_repository_status()
        } else {
            error_status("Git 저장소 여부를 확인하지 못했습니다.")
        };
    }
    if String::from_utf8_lossy(&repository_check.stdout).trim() != "true" {
        return not_repository_status();
    }

    let branch = run_git(Some(&root), &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|branch| !branch.is_empty());
    let head = run_git(Some(&root), &["rev-parse", "--verify", "HEAD"])
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|head| !head.is_empty());

    let tracked_output = match run_git(Some(&root), &["ls-files", "--cached", "-z"]) {
        Ok(output) if output.status.success() => output.stdout,
        _ => return error_status("추적 파일 목록을 읽지 못했습니다."),
    };
    let tracked_count = tracked_output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .count() as u64;

    let status_output = match run_git(
        Some(&root),
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        Ok(output) if output.status.success() => output.stdout,
        _ => return error_status("Git 파일 변경 상태를 읽지 못했습니다."),
    };
    let (modified_count, untracked_count, changes, omitted_change_count) =
        parse_porcelain_status(&status_output);

    GitStatusResponse {
        state: GitStatusState::Available,
        branch,
        head,
        tracked_count,
        modified_count,
        untracked_count,
        changes,
        omitted_change_count,
        message: "Git 저장소 상태를 읽었습니다.".into(),
    }
}

fn run_git(root: Option<&Path>, args: &[&str]) -> std::io::Result<Output> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.untrackedCache=false")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_PREFIX");
    if let Some(root) = root {
        command.arg("-C").arg(root);
    }
    command.args(args).output()
}

fn parse_porcelain_status(output: &[u8]) -> (u64, u64, Vec<GitFileChange>, u64) {
    let mut modified_count = 0_u64;
    let mut untracked_count = 0_u64;
    let mut changes = Vec::new();
    let mut omitted_change_count = 0_u64;
    let mut records = output.split(|byte| *byte == 0).peekable();

    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            continue;
        }

        let index_status = record[0] as char;
        let worktree_status = record[1] as char;
        let is_untracked = index_status == '?' && worktree_status == '?';
        if is_untracked {
            untracked_count = untracked_count.saturating_add(1);
        } else if index_status != ' ' || worktree_status != ' ' {
            modified_count = modified_count.saturating_add(1);
        }

        if is_untracked || index_status != ' ' || worktree_status != ' ' {
            if changes.len() < MAX_DISPLAYED_CHANGES {
                changes.push(GitFileChange {
                    path: String::from_utf8_lossy(&record[3..]).into_owned(),
                    kind: if is_untracked {
                        GitFileChangeKind::Untracked
                    } else {
                        GitFileChangeKind::Tracked
                    },
                    status_code: String::from_utf8_lossy(&record[..2]).into_owned(),
                });
            } else {
                omitted_change_count = omitted_change_count.saturating_add(1);
            }
        }

        if index_status == 'R'
            || index_status == 'C'
            || worktree_status == 'R'
            || worktree_status == 'C'
        {
            records.next();
        }
    }

    (
        modified_count,
        untracked_count,
        changes,
        omitted_change_count,
    )
}

fn unavailable_status() -> GitStatusResponse {
    empty_status(
        GitStatusState::GitUnavailable,
        "시스템 Git을 찾을 수 없습니다. 파일 탐색은 계속 사용할 수 있습니다.",
    )
}

fn not_repository_status() -> GitStatusResponse {
    empty_status(
        GitStatusState::NotRepository,
        "선택한 프로젝트는 Git 작업 트리가 아닙니다. 파일 탐색은 계속 사용할 수 있습니다.",
    )
}

fn error_status(message: &str) -> GitStatusResponse {
    empty_status(GitStatusState::Error, message)
}

fn empty_status(state: GitStatusState, message: &str) -> GitStatusResponse {
    GitStatusResponse {
        state,
        branch: None,
        head: None,
        tracked_count: 0,
        modified_count: 0,
        untracked_count: 0,
        changes: Vec::new(),
        omitted_change_count: 0,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{GitFileChangeKind, MAX_DISPLAYED_CHANGES, parse_porcelain_status};

    #[test]
    fn parses_changed_and_untracked_paths_from_nul_delimited_status() {
        let status = b" M src/Main.kt\0A  staged.kt\0?? new file.txt\0D  deleted.java\0";
        let (modified, untracked, changes, omitted) = parse_porcelain_status(status);

        assert_eq!(modified, 3);
        assert_eq!(untracked, 1);
        assert_eq!(omitted, 0);
        assert_eq!(changes[0].path, "src/Main.kt");
        assert_eq!(changes[0].status_code, " M");
        assert_eq!(changes[0].kind, GitFileChangeKind::Tracked);
        assert_eq!(changes[2].path, "new file.txt");
        assert_eq!(changes[2].kind, GitFileChangeKind::Untracked);
    }

    #[test]
    fn skips_the_second_path_in_nul_delimited_rename_records() {
        let status = b"R  new name.kt\0old name.kt\0?? untracked\0";
        let (modified, untracked, changes, omitted) = parse_porcelain_status(status);

        assert_eq!(modified, 1);
        assert_eq!(untracked, 1);
        assert_eq!(omitted, 0);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].path, "new name.kt");
        assert_eq!(changes[1].path, "untracked");
    }

    #[test]
    fn caps_displayed_changes_and_counts_the_omitted_remainder() {
        let mut status = Vec::new();
        for index in 0..(MAX_DISPLAYED_CHANGES + 7) {
            status.extend_from_slice(format!("?? file-{index}\0").as_bytes());
        }
        let (modified, untracked, changes, omitted) = parse_porcelain_status(&status);

        assert_eq!(modified, 0);
        assert_eq!(untracked, (MAX_DISPLAYED_CHANGES + 7) as u64);
        assert_eq!(changes.len(), MAX_DISPLAYED_CHANGES);
        assert_eq!(omitted, 7);
    }
}
