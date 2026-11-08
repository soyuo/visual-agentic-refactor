use crate::command_error::CommandError;
use crate::scanner::{ScanProjectRequest, ScanProjectResponse};
use crate::state::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn scan_project(
    request: ScanProjectRequest,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ScanProjectResponse, CommandError> {
    let task_control = state.register_task()?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::scanner::scan_project(request, app, task_control)
    })
    .await
    .map_err(|_| {
        CommandError::new(
            crate::command_error::CommandErrorCode::Internal,
            "프로젝트 파일 스캔 작업이 비정상 종료되었습니다.",
        )
    })?
}
