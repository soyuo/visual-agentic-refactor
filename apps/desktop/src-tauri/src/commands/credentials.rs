use crate::command_error::CommandError;
use crate::credentials::{
    self, ProviderCredentialRequest, ProviderCredentialStatus, SaveCredentialRequest,
};

#[tauri::command]
pub fn list_provider_credentials() -> Result<Vec<ProviderCredentialStatus>, CommandError> {
    credentials::statuses()
}

#[tauri::command]
pub fn save_provider_credential(request: SaveCredentialRequest) -> Result<(), CommandError> {
    credentials::save(request.provider, &request.credential)
}

#[tauri::command]
pub fn delete_provider_credential(request: ProviderCredentialRequest) -> Result<(), CommandError> {
    credentials::delete(request.provider)
}
