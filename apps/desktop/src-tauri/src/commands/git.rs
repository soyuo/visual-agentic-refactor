use crate::command_error::{CommandError, CommandErrorCode};
use crate::git_status::GitStatusResponse;

#[tauri::command]
pub async fn get_git_status(path: String) -> Result<GitStatusResponse, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::git_status::inspect_project(std::path::Path::new(&path))
    })
    .await
    .map_err(|_| {
        CommandError::new(
            CommandErrorCode::Internal,
            "Git 상태 조회 작업이 비정상 종료되었습니다.",
        )
    })
}
