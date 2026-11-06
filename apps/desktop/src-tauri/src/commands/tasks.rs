use serde::{Deserialize, Serialize};
use tauri::State;

use crate::command_error::CommandError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelTaskRequest {
    pub task_id: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelTaskResponse {
    pub task_id: u64,
    pub cancellation_requested: bool,
}

#[tauri::command]
pub fn cancel_task(
    request: CancelTaskRequest,
    state: State<'_, AppState>,
) -> Result<CancelTaskResponse, CommandError> {
    state.request_cancel(request.task_id)?;

    Ok(CancelTaskResponse {
        task_id: request.task_id,
        cancellation_requested: true,
    })
}
