use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

use crate::command_error::{CommandError, CommandErrorCode};

const SERVICE_NAME: &str = "visual-agentic-refactor";

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum CredentialProvider {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
}

impl CredentialProvider {
    fn account_name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCredentialRequest {
    pub provider: CredentialProvider,
    pub credential: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialRequest {
    pub provider: CredentialProvider,
}

#[derive(Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialStatus {
    pub provider: CredentialProvider,
    pub display_name: &'static str,
    pub configured: bool,
}

fn entry(provider: CredentialProvider) -> Result<Entry, CommandError> {
    Entry::new(SERVICE_NAME, provider.account_name()).map_err(|error| map_store_error(error))
}

fn map_store_error(_error: KeyringError) -> CommandError {
    CommandError::new(
        CommandErrorCode::Internal,
        "OS 자격 증명 저장소에 접근할 수 없습니다. 잠금 상태와 시스템 설정을 확인해 주세요.",
    )
}

fn validate_credential(credential: &str) -> Result<(), CommandError> {
    if credential.trim().is_empty() {
        return Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "자격 증명을 입력해 주세요.",
        ));
    }
    Ok(())
}

pub fn save(provider: CredentialProvider, credential: &str) -> Result<(), CommandError> {
    validate_credential(credential)?;
    entry(provider)?
        .set_password(credential)
        .map_err(map_store_error)
}

pub fn is_configured(provider: CredentialProvider) -> Result<bool, CommandError> {
    match entry(provider)?.get_password() {
        Ok(_credential) => Ok(true),
        Err(KeyringError::NoEntry) => Ok(false),
        Err(error) => Err(map_store_error(error)),
    }
}

/// Reads a credential only for trusted Rust provider adapters. This value must
/// never cross the Tauri command boundary or be included in an error message.
pub(crate) fn read_for_provider(
    provider: CredentialProvider,
) -> Result<Option<String>, CommandError> {
    match entry(provider)?.get_password() {
        Ok(credential) => Ok(Some(credential)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(map_store_error(error)),
    }
}

pub fn delete(provider: CredentialProvider) -> Result<(), CommandError> {
    match entry(provider)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(map_store_error(error)),
    }
}

pub fn statuses() -> Result<Vec<ProviderCredentialStatus>, CommandError> {
    [CredentialProvider::OpenAi, CredentialProvider::Anthropic]
        .into_iter()
        .map(|provider| {
            Ok(ProviderCredentialStatus {
                provider,
                display_name: provider.display_name(),
                configured: is_configured(provider)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_credentials_are_rejected_without_store_access() {
        let error = validate_credential(" \t\n").expect_err("blank credential is invalid");

        assert!(error.message.contains("자격 증명"));
        assert_eq!(error.code, CommandErrorCode::InvalidInput);
    }

    #[test]
    fn provider_names_are_stable_and_secret_free() {
        assert_eq!(CredentialProvider::OpenAi.account_name(), "openai");
        assert_eq!(CredentialProvider::Anthropic.account_name(), "anthropic");
        assert_eq!(CredentialProvider::OpenAi.display_name(), "OpenAI");
        assert_eq!(CredentialProvider::Anthropic.display_name(), "Anthropic");
    }

    #[test]
    fn credential_store_errors_are_sanitized() {
        let error = map_store_error(KeyringError::Invalid(
            "api-key-secret".to_owned(),
            "api-key-secret".to_owned(),
        ));

        assert!(!error.message.contains("api-key-secret"));
        assert!(error.message.contains("OS 자격 증명 저장소"));
    }
}
