use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use wasmi::{Caller, Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::analyzer::{
    AnalysisCompleteness, AnalysisOutput, AnalyzerError, AnalyzerErrorCode, AnalyzerInput,
    Confidence, RelationshipKind, SourceLanguage,
};

pub const PLUGIN_CONTRACT_VERSION: u32 = 1;
pub const MAX_PLUGIN_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MAX_FUEL: u64 = 50_000_000;
pub const MAX_PLUGIN_EXECUTION_TIME: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyzerPluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub contract_version: u32,
    pub language: SourceLanguage,
    pub capabilities: Vec<String>,
    pub module_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AnalyzerPluginTrust {
    AwaitingApproval,
    UserApproved,
    Rejected,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerPluginSummary {
    pub manifest: AnalyzerPluginManifest,
    pub enabled: bool,
    pub trust: AnalyzerPluginTrust,
}

struct PluginStoreData {
    limits: StoreLimits,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginInput<'a> {
    relative_path: &'a str,
    source: &'a str,
}

pub fn parse_manifest(json: &str) -> Result<AnalyzerPluginManifest, AnalyzerError> {
    if json.len() > MAX_MANIFEST_BYTES {
        return Err(plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Plugin manifest exceeds the 64 KiB limit.",
        ));
    }
    let manifest: AnalyzerPluginManifest = serde_json::from_str(json).map_err(|error| {
        plugin_error(
            AnalyzerErrorCode::InvalidInput,
            format!("Plugin manifest is invalid: {error}"),
        )
    })?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_manifest(manifest: &AnalyzerPluginManifest) -> Result<(), AnalyzerError> {
    if manifest.id.len() > 128
        || manifest.id.is_empty()
        || !manifest
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin id must use 1 to 128 ASCII letters, digits, dots, underscores, or hyphens.",
        ));
    }
    if manifest.name.trim().is_empty()
        || manifest.name.len() > 256
        || manifest.version.trim().is_empty()
        || manifest.version.len() > 128
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin name and version must be non-empty and within their size limits.",
        ));
    }
    if manifest.contract_version != PLUGIN_CONTRACT_VERSION {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            format!(
                "Plugin contract version {} is unsupported; this app supports version {}.",
                manifest.contract_version, PLUGIN_CONTRACT_VERSION
            ),
        ));
    }
    if manifest.capabilities.is_empty()
        || manifest.capabilities.len() > 16
        || manifest.capabilities.iter().any(|capability| {
            !matches!(
                capability.as_str(),
                "symbols" | "imports" | "relationships" | "controlFlow" | "annotations"
            )
        })
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin capabilities must be a non-empty subset of the contract capability names.",
        ));
    }
    if manifest.module_sha256.len() != 64
        || !manifest
            .module_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin moduleSha256 must be a 64-character hexadecimal SHA-256 digest.",
        ));
    }
    Ok(())
}

pub fn verify_package(manifest: &AnalyzerPluginManifest, wasm: &[u8]) -> Result<(), AnalyzerError> {
    validate_manifest(manifest)?;
    if wasm.is_empty() || wasm.len() > MAX_PLUGIN_BYTES {
        return Err(plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Plugin module must be between 1 byte and 16 MiB.",
        ));
    }
    let digest = sha2::Sha256::digest(wasm);
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !actual.eq_ignore_ascii_case(&manifest.module_sha256) {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin module digest does not match moduleSha256.",
        ));
    }
    let engine = plugin_engine();
    let module = Module::new(&engine, wasm).map_err(|error| {
        plugin_error(
            AnalyzerErrorCode::InvalidInput,
            format!("Plugin module is not valid WebAssembly: {error}"),
        )
    })?;
    if module
        .imports()
        .any(|import| import.module() != "env" || import.name() != "is_cancelled")
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin modules may only import the cooperative `env.is_cancelled` function.",
        ));
    }
    Ok(())
}

pub fn run_plugin(
    manifest: &AnalyzerPluginManifest,
    wasm: &[u8],
    input: AnalyzerInput<'_>,
) -> Result<AnalysisOutput, AnalyzerError> {
    run_plugin_cancellable(manifest, wasm, input, Arc::new(AtomicBool::new(false)))
}

pub fn run_plugin_cancellable(
    manifest: &AnalyzerPluginManifest,
    wasm: &[u8],
    input: AnalyzerInput<'_>,
    cancelled: Arc<AtomicBool>,
) -> Result<AnalysisOutput, AnalyzerError> {
    verify_package(manifest, wasm)?;
    let deadline = Instant::now() + MAX_PLUGIN_EXECUTION_TIME;
    if input.relative_path.is_empty()
        || input.relative_path.starts_with('/')
        || input.relative_path.contains('\\')
        || input
            .relative_path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin input path must be a normalized project-relative path.",
        ));
    }
    let input_json = serde_json::to_vec(&PluginInput {
        relative_path: input.relative_path,
        source: input.source,
    })
    .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    if input_json.len() > MAX_INPUT_BYTES {
        return Err(plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Plugin input exceeds the 2 MiB limit.",
        ));
    }

    let engine = plugin_engine();
    let module = Module::new(&engine, wasm)
        .map_err(|error| plugin_error(AnalyzerErrorCode::InvalidInput, error.to_string()))?;
    let mut store = Store::new(
        &engine,
        PluginStoreData {
            limits: StoreLimitsBuilder::new()
                .memory_size(MAX_MEMORY_BYTES)
                .table_elements(10_000)
                .instances(1)
                .tables(1)
                .memories(1)
                .build(),
            cancelled: Arc::clone(&cancelled),
        },
    );
    store.limiter(|data| &mut data.limits);
    store
        .set_fuel(MAX_FUEL)
        .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    let mut linker = Linker::<PluginStoreData>::new(&engine);
    linker
        .func_wrap(
            "env",
            "is_cancelled",
            move |caller: Caller<PluginStoreData>| {
                i32::from(
                    caller.data().cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline,
                )
            },
        )
        .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    let memory = instance.get_memory(&store, "memory").ok_or_else(|| {
        plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin must export linear memory named `memory`.",
        )
    })?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "alloc")
        .map_err(|error| plugin_error(AnalyzerErrorCode::InvalidInput, error.to_string()))?;
    let analyze = instance
        .get_typed_func::<(i32, i32), i64>(&store, "analyze")
        .map_err(|error| plugin_error(AnalyzerErrorCode::InvalidInput, error.to_string()))?;
    let input_len = i32::try_from(input_json.len()).map_err(|_| {
        plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Plugin input is too large for the ABI.",
        )
    })?;
    let input_ptr = alloc
        .call(&mut store, input_len)
        .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    if input_ptr < 0 {
        return Err(plugin_error(
            AnalyzerErrorCode::Internal,
            "Plugin allocator returned an invalid pointer.",
        ));
    }
    memory
        .write(&mut store, input_ptr as usize, &input_json)
        .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    let packed = match analyze.call(&mut store, (input_ptr, input_len)) {
        Ok(packed) => packed as u64,
        Err(_error) if cancelled.load(Ordering::Relaxed) => {
            return Err(plugin_error(
                AnalyzerErrorCode::Cancelled,
                "Analyzer plugin execution was cancelled.",
            ));
        }
        Err(_error) if Instant::now() >= deadline => {
            return Err(plugin_error(
                AnalyzerErrorCode::ResourceLimitExceeded,
                "Analyzer plugin exceeded its 10-second execution deadline.",
            ));
        }
        Err(error) => return Err(plugin_error(AnalyzerErrorCode::Internal, error.to_string())),
    };
    if cancelled.load(Ordering::Relaxed) {
        return Err(plugin_error(
            AnalyzerErrorCode::Cancelled,
            "Analyzer plugin execution was cancelled.",
        ));
    }
    if Instant::now() >= deadline {
        return Err(plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Analyzer plugin exceeded its 10-second execution deadline.",
        ));
    }
    let output_ptr = (packed >> 32) as u32 as usize;
    let output_len = (packed & u32::MAX as u64) as usize;
    if output_len == 0 || output_len > MAX_OUTPUT_BYTES {
        return Err(plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Plugin output must be between 1 byte and 2 MiB.",
        ));
    }
    let mut output_json = vec![0; output_len];
    memory
        .read(&store, output_ptr, &mut output_json)
        .map_err(|error| plugin_error(AnalyzerErrorCode::Internal, error.to_string()))?;
    let output: AnalysisOutput = serde_json::from_slice(&output_json).map_err(|error| {
        plugin_error(
            AnalyzerErrorCode::InvalidInput,
            format!("Plugin output does not match the analyzer contract: {error}"),
        )
    })?;
    validate_output(manifest, input.relative_path, input.source, &output)?;
    Ok(output)
}

fn plugin_engine() -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true).allow_start_fn(false);
    Engine::new(&config)
}

fn validate_output(
    manifest: &AnalyzerPluginManifest,
    relative_path: &str,
    source: &str,
    output: &AnalysisOutput,
) -> Result<(), AnalyzerError> {
    if output.language != manifest.language {
        return Err(plugin_error(
            AnalyzerErrorCode::LanguageMismatch,
            "Plugin output language does not match its manifest.",
        ));
    }
    let has_capability = |name: &str| {
        manifest
            .capabilities
            .iter()
            .any(|capability| capability == name)
    };
    let relationship_is_declared = |kind: RelationshipKind| {
        has_capability("relationships")
            || (matches!(kind, RelationshipKind::Controls | RelationshipKind::Throws)
                && has_capability("controlFlow"))
            || (kind == RelationshipKind::AnnotatedBy && has_capability("annotations"))
    };
    if (!has_capability("symbols") && !output.symbols.is_empty())
        || (!has_capability("imports") && !output.imports.is_empty())
        || output
            .relationships
            .iter()
            .any(|relationship| !relationship_is_declared(relationship.kind))
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin output contains graph features not declared by its manifest.",
        ));
    }
    if output.symbols.len() > 10_000
        || output.imports.len() > 10_000
        || output.relationships.len() > 20_000
        || output.diagnostics.len() > 2_000
    {
        return Err(plugin_error(
            AnalyzerErrorCode::ResourceLimitExceeded,
            "Plugin output exceeds the graph contract item limits.",
        ));
    }
    let mut symbol_ids = std::collections::HashSet::new();
    for symbol in &output.symbols {
        if symbol.id.0.trim().is_empty() || !symbol_ids.insert(&symbol.id) {
            return Err(plugin_error(
                AnalyzerErrorCode::InvalidInput,
                "Plugin output has an empty or duplicate symbol id.",
            ));
        }
    }
    if output.symbols.iter().any(|symbol| {
        symbol
            .parent_id
            .as_ref()
            .is_some_and(|parent_id| !symbol_ids.iter().any(|id| *id == parent_id))
    }) {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin output contains a symbol with an unknown parent id.",
        ));
    }
    for relationship in &output.relationships {
        if !symbol_ids.contains(&relationship.source_symbol_id)
            || relationship
                .target_symbol_id
                .as_ref()
                .is_some_and(|id| !symbol_ids.contains(id))
            || (relationship.target_symbol_id.is_none()
                && relationship
                    .target_name
                    .as_deref()
                    .is_none_or(str::is_empty))
        {
            return Err(plugin_error(
                AnalyzerErrorCode::InvalidInput,
                "Plugin output contains a relationship with an unknown source or target.",
            ));
        }
    }
    for range in output
        .symbols
        .iter()
        .map(|item| &item.location)
        .chain(output.imports.iter().map(|item| &item.location))
        .chain(output.relationships.iter().map(|item| &item.location))
        .chain(
            output
                .diagnostics
                .iter()
                .filter_map(|item| item.location.as_ref()),
        )
    {
        if range.relative_path != relative_path
            || range.start_byte > range.end_byte
            || range.end_byte > source.len()
            || !source.is_char_boundary(range.start_byte)
            || !source.is_char_boundary(range.end_byte)
            || range.start_line == 0
            || range.start_column == 0
            || range.end_line == 0
            || range.end_column == 0
        {
            return Err(plugin_error(
                AnalyzerErrorCode::InvalidInput,
                "Plugin output contains an invalid source location.",
            ));
        }
    }
    if output.completeness == AnalysisCompleteness::Complete
        && output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == crate::analyzer::DiagnosticSeverity::Error)
    {
        return Err(plugin_error(
            AnalyzerErrorCode::InvalidInput,
            "Plugin output cannot be complete while reporting error diagnostics.",
        ));
    }
    Ok(())
}
fn plugin_error(code: AnalyzerErrorCode, message: impl Into<String>) -> AnalyzerError {
    AnalyzerError {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_manifest(wasm: &[u8]) -> AnalyzerPluginManifest {
        AnalyzerPluginManifest {
            id: "example.test".into(),
            name: "Example analyzer".into(),
            version: "1.0.0".into(),
            contract_version: PLUGIN_CONTRACT_VERSION,
            language: SourceLanguage::Kotlin,
            capabilities: vec!["symbols".into()],
            module_sha256: sha2::Sha256::digest(wasm)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }

    #[test]
    fn manifest_requires_known_contract_and_digest_shape() {
        let mut manifest = fixture_manifest(b"wasm");
        assert!(validate_manifest(&manifest).is_ok());
        manifest.contract_version += 1;
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn package_rejects_imported_host_capabilities() {
        let wasm = wat::parse_str(r#"(module (import "env" "read_file" (func)))"#).unwrap();
        let manifest = fixture_manifest(&wasm);
        let error = verify_package(&manifest, &wasm).unwrap_err();
        assert!(error.message.contains("is_cancelled"));
    }

    #[test]
    fn run_plugin_reports_cooperative_cancellation() {
        let wasm = wat::parse_str(
            r#"(module
                (import "env" "is_cancelled" (func $is_cancelled (result i32)))
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) i32.const 256)
                (func (export "analyze") (param i32 i32) (result i64)
                    (call $is_cancelled)
                    (drop)
                    i64.const 68719476844)
                (data (i32.const 16) "{\22language\22:\22kotlin\22,\22completeness\22:\22partial\22,\22symbols\22:[],\22imports\22:[],\22relationships\22:[],\22diagnostics\22:[]}")
            )"#,
        )
        .unwrap();
        let manifest = fixture_manifest(&wasm);
        let cancelled = Arc::new(AtomicBool::new(true));
        let error = run_plugin_cancellable(
            &manifest,
            &wasm,
            AnalyzerInput {
                relative_path: "src/Main.kt",
                source: "class Main {}",
            },
            cancelled,
        )
        .unwrap_err();
        assert_eq!(error.code, AnalyzerErrorCode::Cancelled);
    }

    #[test]
    fn run_plugin_executes_the_contract_v1_abi() {
        let wasm = wat::parse_str(
            r#"(module
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) i32.const 256)
                (func (export "analyze") (param i32 i32) (result i64) i64.const 68719476844)
                (data (i32.const 16) "{\22language\22:\22kotlin\22,\22completeness\22:\22partial\22,\22symbols\22:[],\22imports\22:[],\22relationships\22:[],\22diagnostics\22:[]}")
            )"#,
        )
        .unwrap();
        let manifest = fixture_manifest(&wasm);
        let output = run_plugin(
            &manifest,
            &wasm,
            AnalyzerInput {
                relative_path: "src/Main.kt",
                source: "class Main {}",
            },
        )
        .unwrap();
        assert_eq!(output.language, SourceLanguage::Kotlin);
        assert_eq!(output.completeness, AnalysisCompleteness::Partial);
        assert!(output.symbols.is_empty());
    }

    #[test]
    fn package_rejects_digest_mismatch() {
        let wasm = wat::parse_str("(module)").unwrap();
        let mut manifest = fixture_manifest(&wasm);
        manifest.module_sha256 = "0".repeat(64);
        assert!(verify_package(&manifest, &wasm).is_err());
    }
    #[test]
    fn output_rejects_relationships_outside_declared_capabilities() {
        use crate::analyzer::{AnalyzerRelationship, SourceRange, SymbolId};

        let wasm = b"wasm";
        let mut manifest = fixture_manifest(wasm);
        manifest.capabilities = vec!["annotations".into()];
        let output = AnalysisOutput {
            language: SourceLanguage::Kotlin,
            completeness: AnalysisCompleteness::Partial,
            symbols: vec![],
            imports: vec![],
            relationships: vec![AnalyzerRelationship {
                kind: RelationshipKind::Calls,
                source_symbol_id: SymbolId("source".into()),
                target_symbol_id: None,
                target_name: Some("target".into()),
                location: SourceRange {
                    relative_path: "src/Main.kt".into(),
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 1,
                    start_column: 1,
                    end_line: 1,
                    end_column: 1,
                },
                confidence: Confidence::Confirmed,
            }],
            diagnostics: vec![],
        };

        let error = validate_output(&manifest, "src/Main.kt", "", &output).unwrap_err();
        assert!(error.message.contains("not declared"));
    }

    #[test]
    fn output_rejects_unknown_symbol_parent_ids() {
        use crate::analyzer::{AnalyzerSymbol, SourceRange, SymbolId, SymbolKind};

        let mut manifest = fixture_manifest(b"wasm");
        manifest.capabilities = vec!["symbols".into()];
        let output = AnalysisOutput {
            language: SourceLanguage::Kotlin,
            completeness: AnalysisCompleteness::Partial,
            symbols: vec![AnalyzerSymbol {
                id: SymbolId("child".into()),
                name: "Child".into(),
                kind: SymbolKind::Class,
                parent_id: Some(SymbolId("missing-parent".into())),
                location: SourceRange {
                    relative_path: "src/Main.kt".into(),
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 1,
                    start_column: 1,
                    end_line: 1,
                    end_column: 1,
                },
                confidence: Confidence::Confirmed,
            }],
            imports: vec![],
            relationships: vec![],
            diagnostics: vec![],
        };

        let error = validate_output(&manifest, "src/Main.kt", "", &output).unwrap_err();
        assert!(error.message.contains("unknown parent id"));
    }
}
