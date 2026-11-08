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
        return if has_git_marker_in_ancestors(&root) {
            error_status("Git 저장소 여부를 확인하지 못했습니다.")
        } else {
            not_repository_status()
        };
    }
    if String::from_utf8_lossy(&repository_check.stdout).trim() != "true" {
        return not_repository_status();
    }

    let branch_output = match run_git(Some(&root), &["symbolic-ref", "--quiet", "--short", "HEAD"])
    {
        Ok(output) => output,
        Err(_) => return error_status("현재 브랜치 정보를 읽지 못했습니다."),
    };
    let branch = if branch_output.status.success() {
        Some(
            String::from_utf8_lossy(&branch_output.stdout)
                .trim()
                .to_owned(),
        )
    } else if branch_output.status.code() == Some(1) && branch_output.stderr.is_empty() {
        None
    } else {
        return error_status("현재 브랜치 정보를 읽지 못했습니다.");
    };
    let branch = branch.filter(|branch| !branch.is_empty());

    let head_output = match run_git(Some(&root), &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        Ok(output) => output,
        Err(_) => return error_status("HEAD 커밋 정보를 읽지 못했습니다."),
    };
    let head = if head_output.status.success() {
        Some(
            String::from_utf8_lossy(&head_output.stdout)
                .trim()
                .to_owned(),
        )
    } else if head_output.status.code() == Some(1) && head_output.stderr.is_empty() {
        None
    } else {
        return error_status("HEAD 커밋 정보를 읽지 못했습니다.");
    };
    let head = head.filter(|head| !head.is_empty());

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

fn has_git_marker_in_ancestors(root: &Path) -> bool {
    root.ancestors()
        .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
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
    use super::{
        GitFileChangeKind, GitStatusState, MAX_DISPLAYED_CHANGES, inspect_project,
        parse_porcelain_status,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "visual-agentic-refactor-git-status-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("temporary directory should be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn run_git(root: &Path, args: &[&str]) -> Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("empty-git-config"))
            .output()
            .expect("Git is available for integration tests");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git_is_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

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

    #[test]
    fn inspects_repository_state_without_changing_project_files_or_index() {
        if !git_is_available() {
            return;
        }

        let temp = TempDirectory::new();
        let root = temp.path();
        let hooks = root.join("empty-hooks");
        fs::create_dir(&hooks).expect("hook directory should be created");
        fs::write(root.join("tracked.txt"), b"initial\n").expect("sample file should be written");
        run_git(root, &["init", "--quiet", "--template="]);
        run_git(
            root,
            &[
                "-c",
                "core.hooksPath=empty-hooks",
                "config",
                "user.name",
                "Test User",
            ],
        );
        run_git(
            root,
            &[
                "-c",
                "core.hooksPath=empty-hooks",
                "config",
                "user.email",
                "test@example.invalid",
            ],
        );
        run_git(
            root,
            &["-c", "core.hooksPath=empty-hooks", "add", "tracked.txt"],
        );
        run_git(
            root,
            &[
                "-c",
                "core.hooksPath=empty-hooks",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        fs::write(root.join("tracked.txt"), b"changed\n").expect("tracked file should change");
        fs::write(root.join("new.txt"), b"untracked\n").expect("untracked file should be written");
        let original_tracked_contents = fs::read(root.join("tracked.txt")).unwrap();
        let original_untracked_contents = fs::read(root.join("new.txt")).unwrap();
        let index_path = root.join(".git").join("index");
        let original_index = fs::read(&index_path).expect("Git index should exist after commit");

        let status = inspect_project(root);

        assert_eq!(status.state, GitStatusState::Available);
        assert!(
            status
                .branch
                .as_ref()
                .is_some_and(|branch| !branch.is_empty())
        );
        assert!(status.head.as_ref().is_some_and(|head| !head.is_empty()));
        assert_eq!(status.tracked_count, 1);
        assert_eq!(status.modified_count, 1);
        assert_eq!(status.untracked_count, 1);
        assert!(
            status
                .changes
                .iter()
                .any(|change| change.path == "tracked.txt")
        );
        assert!(status.changes.iter().any(|change| change.path == "new.txt"));
        assert_eq!(
            fs::read(root.join("tracked.txt")).unwrap(),
            original_tracked_contents
        );
        assert_eq!(
            fs::read(root.join("new.txt")).unwrap(),
            original_untracked_contents
        );
        assert_eq!(fs::read(&index_path).unwrap(), original_index);
    }

    #[test]
    fn reports_non_repository_without_blocking_folder_access() {
        if !git_is_available() {
            return;
        }

        let temp = TempDirectory::new();
        fs::write(temp.path().join("sample.txt"), b"readable\n").unwrap();

        let status = inspect_project(temp.path());

        assert_eq!(status.state, GitStatusState::NotRepository);
        assert_eq!(
            fs::read(temp.path().join("sample.txt")).unwrap(),
            b"readable\n"
        );
    }

    #[test]
    fn reports_corrupt_git_marker_as_error_instead_of_non_repository() {
        if !git_is_available() {
            return;
        }

        let temp = TempDirectory::new();
        fs::create_dir(temp.path().join(".git")).expect("Git marker should be created");

        let status = inspect_project(temp.path());

        assert_eq!(status.state, GitStatusState::Error);
    }

    #[test]
    fn reports_branch_without_head_for_an_initialized_empty_repository() {
        if !git_is_available() {
            return;
        }

        let temp = TempDirectory::new();
        run_git(temp.path(), &["init", "--quiet", "--template="]);

        let status = inspect_project(temp.path());

        assert_eq!(status.state, GitStatusState::Available);
        assert!(
            status
                .branch
                .as_ref()
                .is_some_and(|branch| !branch.is_empty())
        );
        assert_eq!(status.head, None);
        assert_eq!(status.tracked_count, 0);
        assert_eq!(status.modified_count, 0);
        assert_eq!(status.untracked_count, 0);
    }
}
