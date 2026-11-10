use keyring::{Entry, Error as KeyringError};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::command_error::{CommandError, CommandErrorCode};

const SERVICE_NAME: &str = "visual-agentic-refactor";
const OPENAI_SUBSCRIPTION_ACCOUNT: &str = "openai-chatgpt-subscription";
const OPENAI_SUBSCRIPTION_HOST_ACCOUNT: &str = "openai-chatgpt-host-id";
const CHATGPT_DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";

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

/// OAuth material is deliberately separate from the provider API-key slot and
/// is only read by the trusted Rust subscription adapter.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenAiSubscriptionCredential {
    pub issued_client_id: String,
    pub subject: String,
    pub email: Option<String>,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at_unix: u64,
    pub scopes: Vec<String>,
}

#[derive(Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpenAiSubscriptionStatus {
    pub connected: bool,
    pub inference_enabled: bool,
    pub account_email: Option<String>,
}

fn subscription_entry() -> Result<Entry, CommandError> {
    Entry::new(SERVICE_NAME, OPENAI_SUBSCRIPTION_ACCOUNT).map_err(map_store_error)
}

pub(crate) fn save_openai_subscription(
    credential: &OpenAiSubscriptionCredential,
) -> Result<(), CommandError> {
    validate_subscription_credential(credential)?;
    let serialized = serde_json::to_string(credential).map_err(|_| {
        CommandError::new(
            CommandErrorCode::Internal,
            "구독 인증 정보를 저장하지 못했습니다.",
        )
    })?;
    subscription_entry()?
        .set_password(&serialized)
        .map_err(map_store_error)
}

pub(crate) fn read_openai_subscription()
-> Result<Option<OpenAiSubscriptionCredential>, CommandError> {
    let serialized = match subscription_entry()?.get_password() {
        Ok(value) => value,
        Err(KeyringError::NoEntry) => return Ok(None),
        Err(error) => return Err(map_store_error(error)),
    };
    let credential: OpenAiSubscriptionCredential =
        serde_json::from_str(&serialized).map_err(|_| {
            CommandError::new(
                CommandErrorCode::Internal,
                "구독 인증 정보를 읽지 못했습니다.",
            )
        })?;
    validate_subscription_credential(&credential).map_err(|_| {
        CommandError::new(
            CommandErrorCode::Internal,
            "저장된 구독 인증 정보를 확인할 수 없습니다.",
        )
    })?;
    Ok(Some(credential))
}

pub(crate) fn delete_openai_subscription() -> Result<(), CommandError> {
    match subscription_entry()?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(map_store_error(error)),
    }
}

pub(crate) fn load_or_create_openai_subscription_host_id() -> Result<String, CommandError> {
    let entry =
        Entry::new(SERVICE_NAME, OPENAI_SUBSCRIPTION_HOST_ACCOUNT).map_err(map_store_error)?;
    match entry.get_password() {
        Ok(host_id) if host_id.starts_with("urn:uuid:") => Ok(host_id),
        Ok(_) => Err(CommandError::new(
            CommandErrorCode::Internal,
            "저장된 OpenAI 호스트 식별자를 확인할 수 없습니다.",
        )),
        Err(KeyringError::NoEntry) => {
            let mut bytes = [0_u8; 16];
            rand::rng().fill_bytes(&mut bytes);
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let host_id = format!(
                "urn:uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                bytes[0],
                bytes[1],
                bytes[2],
                bytes[3],
                bytes[4],
                bytes[5],
                bytes[6],
                bytes[7],
                bytes[8],
                bytes[9],
                bytes[10],
                bytes[11],
                bytes[12],
                bytes[13],
                bytes[14],
                bytes[15]
            );
            entry.set_password(&host_id).map_err(map_store_error)?;
            Ok(host_id)
        }
        Err(error) => Err(map_store_error(error)),
    }
}

pub fn openai_subscription_status() -> Result<OpenAiSubscriptionStatus, CommandError> {
    let credential = read_openai_subscription()?;
    let connected = credential.is_some();
    let inference_enabled = credential.as_ref().is_some_and(|value| {
        value.expires_at_unix > now_unix()
            && value
                .scopes
                .iter()
                .any(|scope| scope == CHATGPT_DIRECT_SCOPE)
    });
    Ok(OpenAiSubscriptionStatus {
        connected,
        inference_enabled,
        account_email: credential.and_then(|value| value.email),
    })
}

fn validate_subscription_credential(
    credential: &OpenAiSubscriptionCredential,
) -> Result<(), CommandError> {
    if !credential.issued_client_id.starts_with("oaiapp_")
        || credential.subject.trim().is_empty()
        || credential.access_token.trim().is_empty()
        || credential.refresh_token.trim().is_empty()
        || credential.id_token.trim().is_empty()
        || credential.expires_at_unix == 0
    {
        return Err(CommandError::new(
            CommandErrorCode::InvalidInput,
            "OpenAI 구독 인증 정보를 확인할 수 없습니다.",
        ));
    }
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

    fn subscription_credential(
        scopes: &[&str],
        expires_at_unix: u64,
    ) -> OpenAiSubscriptionCredential {
        OpenAiSubscriptionCredential {
            issued_client_id: "oaiapp_issued-client".into(),
            subject: "account-subject".into(),
            email: Some("account@example.test".into()),
            access_token: "access-secret".into(),
            refresh_token: "refresh-secret".into(),
            id_token: "id-secret".into(),
            expires_at_unix,
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        }
    }

    #[test]
    fn subscription_credentials_reject_dynamic_entry_client_id() {
        let mut credential = subscription_credential(&[CHATGPT_DIRECT_SCOPE], now_unix() + 60);
        credential.issued_client_id = "dynamic_agent_client".into();
        let error = validate_subscription_credential(&credential)
            .expect_err("the issued client id is required");

        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        assert_ne!(
            OPENAI_SUBSCRIPTION_ACCOUNT,
            CredentialProvider::OpenAi.account_name()
        );
    }

    #[test]
    fn subscription_inference_requires_direct_scope_and_unexpired_access_token() {
        let evaluate = |credential: &OpenAiSubscriptionCredential| {
            credential.expires_at_unix > now_unix()
                && credential
                    .scopes
                    .iter()
                    .any(|scope| scope == CHATGPT_DIRECT_SCOPE)
        };
        assert!(evaluate(&subscription_credential(
            &[CHATGPT_DIRECT_SCOPE],
            now_unix() + 60
        )));
        assert!(!evaluate(&subscription_credential(
            &["openid", "profile"],
            now_unix() + 60
        )));
        assert!(!evaluate(&subscription_credential(
            &[CHATGPT_DIRECT_SCOPE],
            now_unix().saturating_sub(1)
        )));
    }

    #[test]
    fn subscription_tokens_are_serialized_as_one_bundle() {
        let credential = subscription_credential(&[CHATGPT_DIRECT_SCOPE], now_unix() + 60);
        let serialized = serde_json::to_string(&credential).expect("serialize token bundle");
        let restored: OpenAiSubscriptionCredential =
            serde_json::from_str(&serialized).expect("deserialize token bundle");

        assert_eq!(restored.issued_client_id, credential.issued_client_id);
        assert_eq!(restored.refresh_token, credential.refresh_token);
    }
}
