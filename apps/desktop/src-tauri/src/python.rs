use crate::analyzer::*;
use tree_sitter::{Node, Parser, Tree};

pub struct PythonAnalyzer;
impl Analyzer for PythonAnalyzer {
    fn descriptor(&self) -> AnalyzerDescriptor {
        AnalyzerDescriptor {
            language: SourceLanguage::Python,
            capabilities: vec![
                AnalyzerCapability {
                    feature: AnalysisFeature::Symbols,
                    support: SupportLevel::Supported,
                    limitation: None,
                },
                AnalyzerCapability {
                    feature: AnalysisFeature::Imports,
                    support: SupportLevel::Supported,
                    limitation: None,
                },
                AnalyzerCapability {
                    feature: AnalysisFeature::Relationships,
                    support: SupportLevel::Partial,
                    limitation: None,
                },
                AnalyzerCapability {
                    feature: AnalysisFeature::ControlFlow,
                    support: SupportLevel::Partial,
                    limitation: None,
                },
            ],
        }
    }
    fn analyze(&self, input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError> {
        if !input.relative_path.ends_with(".py") {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::InvalidInput,
                message: "Python analyzer requires .py input".into(),
            });
        }
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .map_err(|e| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: e.to_string(),
            })?;
        let tree = parser
            .parse(input.source, None)
            .ok_or_else(|| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: "Python parser returned no tree".into(),
            })?;
        Ok(analyze_tree(&input, &tree))
    }
}
fn analyze_tree(input: &AnalyzerInput<'_>, tree: &Tree) -> AnalysisOutput {
    let mut out = AnalysisOutput {
        language: SourceLanguage::Python,
        completeness: AnalysisCompleteness::Complete,
        symbols: vec![],
        imports: vec![],
        relationships: vec![],
        diagnostics: vec![],
    };
    walk(input, tree.root_node(), &mut out, &mut vec![]);
    if tree.root_node().has_error() {
        out.completeness = AnalysisCompleteness::Partial;
        out.diagnostics.push(AnalyzerDiagnostic {
            code: DiagnosticCode::ParseError,
            severity: DiagnosticSeverity::Warning,
            message: "Python syntax errors limited analysis".into(),
            location: None,
        });
    }
    out
}
fn walk(
    input: &AnalyzerInput<'_>,
    node: Node<'_>,
    out: &mut AnalysisOutput,
    stack: &mut Vec<SymbolId>,
) {
    let kind = node.kind();
    if kind == "import_statement" || kind == "import_from_statement" {
        out.imports.push(AnalyzerImport {
            name: text(node, input),
            alias: None,
            is_static: false,
            is_wildcard: kind == "import_from_statement",
            location: range(input, node),
        });
    }
    let symbol_kind = match kind {
        "class_definition" => Some(SymbolKind::Class),
        "function_definition" | "async_function_definition" => Some(if stack.is_empty() {
            SymbolKind::Function
        } else {
            SymbolKind::Method
        }),
        _ => None,
    };
    let pushed = if let Some(sk) = symbol_kind {
        let name = node
            .child_by_field_name("name")
            .map(|n| text(n, input))
            .unwrap_or_else(|| kind.into());
        let id = SymbolId(format!("python:{}:{}", name, node.start_byte()));
        out.symbols.push(AnalyzerSymbol {
            id: id.clone(),
            name,
            kind: sk,
            parent_id: stack.last().cloned(),
            location: range(input, node),
            confidence: Confidence::Confirmed,
        });
        stack.push(id);
        true
    } else {
        false
    };
    if let Some(source) = stack.last().cloned() {
        if kind == "call" {
            out.relationships.push(AnalyzerRelationship {
                kind: RelationshipKind::Calls,
                source_symbol_id: source.clone(),
                target_symbol_id: None,
                target_name: node.child_by_field_name("function").map(|n| text(n, input)),
                location: range(input, node),
                confidence: Confidence::Inferred,
            });
        }
        if matches!(
            kind,
            "if_statement" | "for_statement" | "while_statement" | "try_statement"
        ) {
            out.relationships.push(AnalyzerRelationship {
                kind: RelationshipKind::Controls,
                source_symbol_id: source,
                target_symbol_id: None,
                target_name: Some(kind.into()),
                location: range(input, node),
                confidence: Confidence::Confirmed,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(input, child, out, stack);
    }
    if pushed {
        stack.pop();
    }
}
fn text(node: Node<'_>, input: &AnalyzerInput<'_>) -> String {
    node.utf8_text(input.source.as_bytes())
        .unwrap_or_default()
        .into()
}
fn range(input: &AnalyzerInput<'_>, node: Node<'_>) -> SourceRange {
    let s = node.start_position();
    let e = node.end_position();
    SourceRange {
        relative_path: input.relative_path.into(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: s.row as u32 + 1,
        start_column: s.column as u32 + 1,
        end_line: e.row as u32 + 1,
        end_column: e.column as u32 + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{Analyzer, AnalyzerInput, RelationshipKind, SymbolKind};

    #[test]
    fn extracts_python_symbols_imports_calls_and_control_flow() {
        let source = "import os\n\nclass Runner:\n    def run(self):\n        if os.path.exists('x'):\n            print('ok')\n";
        let output = PythonAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "src/runner.py",
                source,
            })
            .expect("python analysis should succeed");
        assert!(
            output
                .symbols
                .iter()
                .any(|s| s.name == "Runner" && s.kind == SymbolKind::Class)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|s| s.name == "run" && s.kind == SymbolKind::Method)
        );
        assert_eq!(output.imports.len(), 1);
        assert!(
            output
                .relationships
                .iter()
                .any(|r| r.kind == RelationshipKind::Calls)
        );
        assert!(
            output
                .relationships
                .iter()
                .any(|r| r.kind == RelationshipKind::Controls)
        );
    }

    #[test]
    fn rejects_non_python_input() {
        let error = PythonAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "src/runner.txt",
                source: "class Runner: pass",
            })
            .expect_err("non-python input should be rejected");
        assert_eq!(error.code, AnalyzerErrorCode::InvalidInput);
    }
}
