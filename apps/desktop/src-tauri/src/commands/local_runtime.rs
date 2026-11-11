use crate::command_error::CommandError;
use crate::local_runtime::{self, LocalLlmRequest, LocalLlmResponse};

#[tauri::command]
pub fn run_local_llm(request: LocalLlmRequest) -> Result<LocalLlmResponse, CommandError> {
    local_runtime::run(request)
}
