use serde::{Deserialize, Serialize};

/// Byte and text coordinates for a source span.
///
/// Byte offsets are zero-based UTF-8 offsets with an exclusive end. Lines and
/// columns are one-based; columns count Unicode scalar values within a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub relative_path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Identifier unique within one analyzer output. Graph storage assigns its own
/// persistent identity when it stores the result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceLanguage {
    Kotlin,
    Java,
    #[serde(rename = "javascript")]
    JavaScript,
    #[serde(rename = "typescript")]
    TypeScript,
    #[serde(rename = "python")]
    Python,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisFeature {
    Symbols,
    Relationships,
    ControlFlow,
    Imports,
    Annotations,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportLevel {
    Supported,
    Partial,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerCapability {
    pub feature: AnalysisFeature,
    pub support: SupportLevel,
    pub limitation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerDescriptor {
    pub language: SourceLanguage,
    pub capabilities: Vec<AnalyzerCapability>,
}

/// Borrowed source input keeps file contents out of adapter-owned allocations.
pub struct AnalyzerInput<'a> {
    pub relative_path: &'a str,
    pub source: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    Confirmed,
    Inferred,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SymbolKind {
    Package,
    Class,
    Interface,
    Object,
    Enum,
    Record,
    Constructor,
    Function,
    Method,
    Property,
    Field,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerSymbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub parent_id: Option<SymbolId>,
    pub location: SourceRange,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerImport {
    pub name: String,
    pub alias: Option<String>,
    pub is_static: bool,
    pub is_wildcard: bool,
    pub location: SourceRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationshipKind {
    Calls,
    Implements,
    Extends,
    Inherits,
    Imports,
    Injects,
    Exposes,
    UsesDto,
    Controls,
    Throws,
    AnnotatedBy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerRelationship {
    pub kind: RelationshipKind,
    pub source_symbol_id: SymbolId,
    pub target_symbol_id: Option<SymbolId>,
    pub target_name: Option<String>,
    pub location: SourceRange,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticCode {
    ParseError,
    UnsupportedSyntax,
    PartialResult,
    ResourceLimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerDiagnostic {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub location: Option<SourceRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisOutput {
    pub language: SourceLanguage,
    pub completeness: AnalysisCompleteness,
    pub symbols: Vec<AnalyzerSymbol>,
    pub imports: Vec<AnalyzerImport>,
    pub relationships: Vec<AnalyzerRelationship>,
    pub diagnostics: Vec<AnalyzerDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalyzerErrorCode {
    InvalidInput,
    ResourceLimitExceeded,
    DuplicateLanguage,
    AnalyzerUnavailable,
    LanguageMismatch,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerError {
    pub code: AnalyzerErrorCode,
    pub message: String,
}

/// Adapter boundary for language-specific static analyzers.
pub trait Analyzer {
    fn descriptor(&self) -> AnalyzerDescriptor;

    /// Returns partial output with diagnostics for recoverable parse limits or
    /// unsupported syntax. Errors are reserved for inputs that cannot be
    /// analyzed or fatal adapter failures.
    fn analyze(&self, input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError>;
}

struct RegisteredAnalyzer {
    language: SourceLanguage,
    adapter: Box<dyn Analyzer>,
}

/// Minimal language-to-adapter registry shared by the Kotlin and Java parsers.
#[derive(Default)]
pub struct AnalyzerRegistry {
    adapters: Vec<RegisteredAnalyzer>,
}

impl AnalyzerRegistry {
    pub fn with_kotlin() -> Self {
        let mut registry = Self::default();
        registry
            .register(Box::new(crate::kotlin::KotlinAnalyzer))
            .expect("the default Kotlin analyzer should register once");
        registry
    }

    pub fn with_java() -> Self {
        let mut registry = Self::default();
        registry
            .register(Box::new(crate::java::JavaAnalyzer))
            .expect("the default Java analyzer should register once");
        registry
    }

    pub fn with_javascript_typescript() -> Self {
        let mut registry = Self::default();
        registry
            .register(Box::new(crate::javascript::JavaScriptAnalyzer))
            .expect("the JavaScript analyzer should register once");
        registry
            .register(Box::new(crate::javascript::TypeScriptAnalyzer))
            .expect("the TypeScript analyzer should register once");
        registry
    }

    pub fn with_python() -> Self {
        let mut registry = Self::default();
        registry
            .register(Box::new(crate::python::PythonAnalyzer))
            .expect("the Python analyzer should register once");
        registry
    }

    pub fn register(&mut self, adapter: Box<dyn Analyzer>) -> Result<(), AnalyzerError> {
        let language = adapter.descriptor().language;
        if self
            .adapters
            .iter()
            .any(|registered| registered.language == language)
        {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::DuplicateLanguage,
                message: format!("An analyzer for {language:?} is already registered."),
            });
        }

        self.adapters.push(RegisteredAnalyzer { language, adapter });
        Ok(())
    }

    pub fn descriptor(&self, language: SourceLanguage) -> Option<AnalyzerDescriptor> {
        self.adapters
            .iter()
            .find(|registered| registered.language == language)
            .map(|registered| registered.adapter.descriptor())
    }

    pub fn analyze(
        &self,
        language: SourceLanguage,
        input: AnalyzerInput<'_>,
    ) -> Result<AnalysisOutput, AnalyzerError> {
        let registered = self
            .adapters
            .iter()
            .find(|registered| registered.language == language)
            .ok_or_else(|| AnalyzerError {
                code: AnalyzerErrorCode::AnalyzerUnavailable,
                message: format!("No analyzer is registered for {language:?}."),
            })?;

        let output = registered.adapter.analyze(input)?;
        if output.language != language {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::LanguageMismatch,
                message: format!(
                    "The {language:?} analyzer returned output labeled {:?}.",
                    output.language
                ),
            });
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubAnalyzer;

    struct MislabeledAnalyzer;

    impl Analyzer for StubAnalyzer {
        fn descriptor(&self) -> AnalyzerDescriptor {
            AnalyzerDescriptor {
                language: SourceLanguage::Kotlin,
                capabilities: vec![AnalyzerCapability {
                    feature: AnalysisFeature::Symbols,
                    support: SupportLevel::Partial,
                    limitation: Some("Fixture analyzer".into()),
                }],
            }
        }

        fn analyze(&self, input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError> {
            assert_eq!(input.relative_path, "src/main.kt");
            assert_eq!(input.source, "fun main() = Unit");
            Ok(AnalysisOutput {
                language: SourceLanguage::Kotlin,
                completeness: AnalysisCompleteness::Complete,
                symbols: Vec::new(),
                imports: Vec::new(),
                relationships: Vec::new(),
                diagnostics: Vec::new(),
            })
        }
    }

    impl Analyzer for MislabeledAnalyzer {
        fn descriptor(&self) -> AnalyzerDescriptor {
            StubAnalyzer.descriptor()
        }

        fn analyze(&self, _input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError> {
            Ok(AnalysisOutput {
                language: SourceLanguage::Java,
                completeness: AnalysisCompleteness::Complete,
                symbols: Vec::new(),
                imports: Vec::new(),
                relationships: Vec::new(),
                diagnostics: Vec::new(),
            })
        }
    }

    #[test]
    fn analyzer_adapter_exposes_capabilities_and_accepts_borrowed_source() {
        let analyzer = StubAnalyzer;
        let descriptor = analyzer.descriptor();
        assert_eq!(descriptor.language, SourceLanguage::Kotlin);
        assert_eq!(descriptor.capabilities[0].support, SupportLevel::Partial);

        let output = analyzer
            .analyze(AnalyzerInput {
                relative_path: "src/main.kt",
                source: "fun main() = Unit",
            })
            .unwrap();
        assert_eq!(output.completeness, AnalysisCompleteness::Complete);
    }

    #[test]
    fn registry_dispatches_by_language_and_rejects_duplicate_adapters() {
        let mut registry = AnalyzerRegistry::default();
        registry.register(Box::new(StubAnalyzer)).unwrap();

        let descriptor = registry.descriptor(SourceLanguage::Kotlin).unwrap();
        assert_eq!(descriptor.capabilities.len(), 1);
        assert_eq!(
            registry
                .analyze(
                    SourceLanguage::Kotlin,
                    AnalyzerInput {
                        relative_path: "src/main.kt",
                        source: "fun main() = Unit",
                    },
                )
                .unwrap()
                .language,
            SourceLanguage::Kotlin
        );
        assert_eq!(
            registry.register(Box::new(StubAnalyzer)).unwrap_err().code,
            AnalyzerErrorCode::DuplicateLanguage
        );
        assert_eq!(
            registry
                .analyze(
                    SourceLanguage::Java,
                    AnalyzerInput {
                        relative_path: "Main.java",
                        source: "class Main {}",
                    },
                )
                .unwrap_err()
                .code,
            AnalyzerErrorCode::AnalyzerUnavailable
        );
    }

    #[test]
    fn registry_rejects_output_labeled_for_another_language() {
        let mut registry = AnalyzerRegistry::default();
        registry.register(Box::new(MislabeledAnalyzer)).unwrap();

        let error = registry
            .analyze(
                SourceLanguage::Kotlin,
                AnalyzerInput {
                    relative_path: "Main.kt",
                    source: "class Main",
                },
            )
            .unwrap_err();

        assert_eq!(error.code, AnalyzerErrorCode::LanguageMismatch);
    }

    #[test]
    fn source_ranges_and_confidence_serialize_with_stable_frontend_names() {
        let range = SourceRange {
            relative_path: "src/main.kt".into(),
            start_byte: 0,
            end_byte: 3,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 4,
        };
        let value = serde_json::to_value(range).unwrap();

        assert_eq!(value["relativePath"], "src/main.kt");
        assert_eq!(value["startByte"], 0);
        assert_eq!(value["endColumn"], 4);
        assert_eq!(
            serde_json::to_value(Confidence::Confirmed).unwrap(),
            "confirmed"
        );
        assert_eq!(
            serde_json::to_value(AnalysisCompleteness::Partial).unwrap(),
            "partial"
        );
    }

    #[test]
    fn unresolved_relationships_can_keep_a_name_without_claiming_a_target() {
        let relation = AnalyzerRelationship {
            kind: RelationshipKind::Calls,
            source_symbol_id: SymbolId("caller".into()),
            target_symbol_id: None,
            target_name: Some("possibly_dynamic".into()),
            location: SourceRange {
                relative_path: "src/main.kt".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 2,
            },
            confidence: Confidence::Unknown,
        };

        assert_eq!(relation.target_symbol_id, None);
        assert_eq!(relation.target_name.as_deref(), Some("possibly_dynamic"));
        assert_eq!(relation.confidence, Confidence::Unknown);
    }
}
