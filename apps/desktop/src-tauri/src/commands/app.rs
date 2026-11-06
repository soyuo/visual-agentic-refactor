use serde::Serialize;

use crate::command_error::CommandError;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfoResponse {
    pub app_name: &'static str,
    pub version: &'static str,
}

#[tauri::command]
pub fn get_app_info() -> Result<AppInfoResponse, CommandError> {
    Ok(AppInfoResponse {
        app_name: "Visual Agentic Refactor",
        version: env!("CARGO_PKG_VERSION"),
    })
}
