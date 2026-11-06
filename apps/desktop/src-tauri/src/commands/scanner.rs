use crate::command_error::CommandError;
use crate::scanner::{ScanProjectRequest, ScanProjectResponse};

#[tauri::command]
pub fn scan_project(request: ScanProjectRequest) -> Result<ScanProjectResponse, CommandError> {
    crate::scanner::scan_project(request)
}
