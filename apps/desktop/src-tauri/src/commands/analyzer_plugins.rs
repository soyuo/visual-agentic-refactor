use base64::Engine;
use serde::Deserialize;
use tauri::State;

use crate::analyzer::{AnalysisOutput, AnalyzerInput};
use crate::analyzer_plugin::{
    AnalyzerPluginSummary, AnalyzerPluginTrust, MAX_PLUGIN_BYTES, parse_manifest, run_plugin,
    verify_package,
};
use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::PersistenceState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallAnalyzerPluginRequest {
    pub manifest_json: String,
    pub wasm_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyzerPluginIdRequest {
    pub plugin_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetAnalyzerPluginApprovalRequest {
    pub plugin_id: String,
    pub approved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetAnalyzerPluginEnabledRequest {
    pub plugin_id: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunAnalyzerPluginRequest {
    pub plugin_id: String,
    pub relative_path: String,
    pub source: String,
}

#[tauri::command]
pub fn install_analyzer_plugin(
    request: InstallAnalyzerPluginRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<AnalyzerPluginSummary, CommandError> {
    if request.wasm_base64.len() > MAX_PLUGIN_BYTES.saturating_mul(4) / 3 + 8 {
        return Err(invalid(
            "Encoded plugin module exceeds the 16 MiB package limit.",
        ));
    }
    let manifest = parse_manifest(&request.manifest_json).map_err(analyzer_error)?;
    let wasm = base64::engine::general_purpose::STANDARD
        .decode(request.wasm_base64)
        .map_err(|_| invalid("Plugin module must be valid standard Base64."))?;
    verify_package(&manifest, &wasm).map_err(analyzer_error)?;
    let database = persistence.database()?;
    database
        .install_analyzer_plugin(&manifest, &wasm)
        .map_err(internal)?;
    Ok(AnalyzerPluginSummary {
        manifest,
        enabled: false,
        trust: AnalyzerPluginTrust::AwaitingApproval,
    })
}

#[tauri::command]
pub fn list_analyzer_plugins(
    persistence: State<'_, PersistenceState>,
) -> Result<Vec<AnalyzerPluginSummary>, CommandError> {
    persistence
        .database()?
        .list_analyzer_plugins()
        .map_err(internal)
}

#[tauri::command]
pub fn set_analyzer_plugin_approval(
    request: SetAnalyzerPluginApprovalRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<(), CommandError> {
    let plugin_id = validate_plugin_id(&request.plugin_id)?;
    persistence
        .database()?
        .approve_analyzer_plugin(plugin_id, request.approved)
        .map_err(internal)
}

#[tauri::command]
pub fn set_analyzer_plugin_enabled(
    request: SetAnalyzerPluginEnabledRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<(), CommandError> {
    let plugin_id = validate_plugin_id(&request.plugin_id)?;
    persistence
        .database()?
        .set_analyzer_plugin_enabled(plugin_id, request.enabled)
        .map_err(internal)
}

#[tauri::command]
pub fn remove_analyzer_plugin(
    request: AnalyzerPluginIdRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<bool, CommandError> {
    let plugin_id = validate_plugin_id(&request.plugin_id)?;
    persistence
        .database()?
        .remove_analyzer_plugin(plugin_id)
        .map_err(internal)
}

#[tauri::command]
pub fn run_analyzer_plugin(
    request: RunAnalyzerPluginRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<AnalysisOutput, CommandError> {
    let plugin_id = validate_plugin_id(&request.plugin_id)?;
    let plugin = persistence
        .database()?
        .load_enabled_analyzer_plugin(plugin_id)
        .map_err(internal)?
        .ok_or_else(|| invalid("Analyzer plugin is not approved and enabled."))?;
    run_plugin(
        &plugin.manifest,
        &plugin.wasm_module,
        AnalyzerInput {
            relative_path: &request.relative_path,
            source: &request.source,
        },
    )
    .map_err(analyzer_error)
}

fn validate_plugin_id(plugin_id: &str) -> Result<&str, CommandError> {
    if plugin_id.trim().is_empty() || plugin_id.len() > 128 {
        return Err(invalid(
            "Plugin ID must contain between 1 and 128 characters.",
        ));
    }
    Ok(plugin_id)
}

fn analyzer_error(error: crate::analyzer::AnalyzerError) -> CommandError {
    let code = match error.code {
        crate::analyzer::AnalyzerErrorCode::InvalidInput
        | crate::analyzer::AnalyzerErrorCode::LanguageMismatch
        | crate::analyzer::AnalyzerErrorCode::DuplicateLanguage => CommandErrorCode::InvalidInput,
        crate::analyzer::AnalyzerErrorCode::ResourceLimitExceeded
        | crate::analyzer::AnalyzerErrorCode::AnalyzerUnavailable
        | crate::analyzer::AnalyzerErrorCode::Internal => CommandErrorCode::Internal,
    };
    CommandError::new(code, error.message)
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}

fn internal(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
