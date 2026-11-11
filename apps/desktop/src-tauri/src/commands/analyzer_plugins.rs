use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool};
use tauri::State;

use crate::analyzer::{
    AnalysisCompleteness, AnalysisOutput, AnalyzerDiagnostic, AnalyzerError, AnalyzerErrorCode,
    AnalyzerInput, AnalyzerRegistry, DiagnosticCode, DiagnosticSeverity, SourceLanguage,
};
use crate::analyzer_plugin::{
    AnalyzerPluginSummary, AnalyzerPluginTrust, MAX_PLUGIN_BYTES, MAX_PLUGIN_EXECUTION_TIME,
    parse_manifest, run_plugin_cancellable, verify_package,
};
use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::PersistenceState;

static ACTIVE_PLUGIN_RUNS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

struct ActivePluginRunGuard(String);

impl Drop for ActivePluginRunGuard {
    fn drop(&mut self) {
        if let Some(runs) = ACTIVE_PLUGIN_RUNS.get() {
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&self.0);
            }
        }
    }
}

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
    pub run_id: String,
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
pub async fn run_analyzer_plugin(
    request: RunAnalyzerPluginRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<AnalysisOutput, CommandError> {
    let plugin_id = validate_plugin_id(&request.plugin_id)?.to_owned();
    let run_id = validate_plugin_id(&request.run_id)?.to_owned();
    validate_plugin_relative_path(&request.relative_path)?;
    let relative_path = request.relative_path;
    let source = request.source;
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let runs = ACTIVE_PLUGIN_RUNS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut runs = runs
            .lock()
            .map_err(|_| internal("plugin run registry lock failed".to_owned()))?;
        if runs.contains_key(&run_id) {
            return Err(invalid("A plugin run with this runId is already active."));
        }
        if !runs.is_empty() {
            return Err(invalid("Only one analyzer plugin may run at a time."));
        }
        runs.insert(run_id.clone(), Arc::clone(&cancelled));
    }

    let prepared: Result<_, CommandError> = (|| {
        let plugin = persistence
            .database()?
            .load_enabled_analyzer_plugin(&plugin_id)
            .map_err(internal)?
            .ok_or_else(|| invalid("Analyzer plugin is not approved and enabled."))?;
        Ok((plugin.manifest, plugin.wasm_module))
    })();
    match prepared {
        Ok((manifest, wasm_module)) => {
            let task_cancelled = Arc::clone(&cancelled);
            let fallback_language = manifest.language;
            let fallback_path = relative_path.clone();
            let fallback_source = source.clone();
            let task_run_id = run_id.clone();
            let task = tauri::async_runtime::spawn_blocking(move || {
                let _active_run = ActivePluginRunGuard(task_run_id);
                let plugin_result = run_plugin_cancellable(
                    &manifest,
                    &wasm_module,
                    AnalyzerInput {
                        relative_path: &relative_path,
                        source: &source,
                    },
                    task_cancelled,
                );
                match plugin_result {
                    Ok(output) => Ok(output),
                    Err(error) if error.code == AnalyzerErrorCode::Cancelled => Err(error),
                    Err(error) => fallback_analyze(
                        manifest.language,
                        AnalyzerInput {
                            relative_path: &relative_path,
                            source: &source,
                        },
                        &error,
                    ),
                }
            });
            match tokio::time::timeout(MAX_PLUGIN_EXECUTION_TIME, task).await {
                Ok(Ok(result)) => result.map_err(analyzer_error),
                Ok(Err(error)) => Err(internal(format!("plugin execution task failed: {error}"))),
                Err(_) => {
                    cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                    let timeout_error = AnalyzerError {
                        code: AnalyzerErrorCode::ResourceLimitExceeded,
                        message: "Analyzer plugin exceeded its 10-second execution deadline; cooperative cancellation was requested.".to_owned(),
                    };
                    let fallback_task = tauri::async_runtime::spawn_blocking(move || {
                        fallback_analyze(
                            fallback_language,
                            AnalyzerInput {
                                relative_path: &fallback_path,
                                source: &fallback_source,
                            },
                            &timeout_error,
                        )
                    });
                    match fallback_task.await {
                        Ok(result) => result.map_err(analyzer_error),
                        Err(error) => Err(internal(format!(
                            "built-in analyzer fallback task failed: {error}"
                        ))),
                    }
                }
            }
        }
        Err(error) => {
            if let Some(runs) = ACTIVE_PLUGIN_RUNS.get() {
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&run_id);
                }
            }
            Err(error)
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelAnalyzerPluginRunRequest {
    pub run_id: String,
}

#[tauri::command]
pub fn cancel_analyzer_plugin_run(
    request: CancelAnalyzerPluginRunRequest,
) -> Result<bool, CommandError> {
    let run_id = validate_plugin_id(&request.run_id)?;
    let runs = ACTIVE_PLUGIN_RUNS.get_or_init(|| Mutex::new(HashMap::new()));
    let runs = runs
        .lock()
        .map_err(|_| internal("plugin run registry lock failed".to_owned()))?;
    if let Some(cancelled) = runs.get(run_id) {
        cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(true)
    } else {
        Ok(false)
    }
}
fn validate_plugin_relative_path(path: &str) -> Result<(), CommandError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(invalid(
            "Plugin input path must be a normalized project-relative path.",
        ));
    }
    Ok(())
}
fn fallback_analyze(
    language: SourceLanguage,
    input: AnalyzerInput<'_>,
    plugin_error: &AnalyzerError,
) -> Result<AnalysisOutput, AnalyzerError> {
    let registry = match language {
        SourceLanguage::Kotlin => AnalyzerRegistry::with_kotlin(),
        SourceLanguage::Java => AnalyzerRegistry::with_java(),
        SourceLanguage::JavaScript | SourceLanguage::TypeScript => {
            AnalyzerRegistry::with_javascript_typescript()
        }
        SourceLanguage::Python => AnalyzerRegistry::with_python(),
    };
    let mut output = registry.analyze(language, input)?;
    output.completeness = AnalysisCompleteness::Partial;
    output.diagnostics.push(AnalyzerDiagnostic {
        code: DiagnosticCode::PartialResult,
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "Analyzer plugin failed; built-in analyzer fallback was used: {}",
            plugin_error.message
        ),
        location: None,
    });
    Ok(output)
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
        crate::analyzer::AnalyzerErrorCode::Cancelled => CommandErrorCode::Cancelled,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_failure_uses_builtin_analyzer_fallback() {
        let error = AnalyzerError {
            code: AnalyzerErrorCode::Internal,
            message: "plugin trapped".to_owned(),
        };
        let output = fallback_analyze(
            SourceLanguage::Kotlin,
            AnalyzerInput {
                relative_path: "src/Test.kt",
                source: "package sample\nclass Test",
            },
            &error,
        )
        .expect("the built-in Kotlin analyzer should provide fallback output");

        assert_eq!(output.completeness, AnalysisCompleteness::Partial);
        assert_eq!(
            output.diagnostics.last().unwrap().code,
            DiagnosticCode::PartialResult
        );
        assert!(
            output
                .diagnostics
                .last()
                .unwrap()
                .message
                .contains("plugin trapped")
        );
    }
}
