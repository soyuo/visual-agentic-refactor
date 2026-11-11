use tree_sitter::{Node, Parser, Point, Tree};

use crate::analyzer::{
    AnalysisCompleteness, AnalysisFeature, AnalysisOutput, Analyzer, AnalyzerCapability,
    AnalyzerDiagnostic, AnalyzerError, AnalyzerErrorCode, AnalyzerImport, AnalyzerInput,
    AnalyzerRelationship, AnalyzerSymbol, Confidence, DiagnosticCode, DiagnosticSeverity,
    RelationshipKind, SourceLanguage, SourceRange, SupportLevel, SymbolId, SymbolKind,
};

const MAX_SOURCE_BYTES: usize = 5 * 1024 * 1024;
const MAX_ITEMS: usize = 50_000;

pub struct JavaScriptTypeScriptAnalyzer;

impl Analyzer for JavaScriptTypeScriptAnalyzer {
    fn descriptor(&self) -> crate::analyzer::AnalyzerDescriptor {
        crate::analyzer::AnalyzerDescriptor {
            language: SourceLanguage::JavaScript,
            capabilities: vec![
                cap(AnalysisFeature::Symbols, SupportLevel::Supported),
                cap(AnalysisFeature::Imports, SupportLevel::Supported),
                cap(AnalysisFeature::Relationships, SupportLevel::Partial),
                cap(AnalysisFeature::ControlFlow, SupportLevel::Partial),
            ],
        }
    }

    fn analyze(&self, input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError> {
        let language = language_for(input.relative_path).ok_or_else(|| AnalyzerError {
            code: AnalyzerErrorCode::InvalidInput,
            message: "JavaScript/TypeScript analysis requires a supported source extension.".into(),
        })?;
        if input.source.len() > MAX_SOURCE_BYTES {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::ResourceLimitExceeded,
                message: format!("Source exceeds the {MAX_SOURCE_BYTES}-byte analysis limit."),
            });
        }
        let mut parser = Parser::new();
        let grammar = match language {
            SourceLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            _ => unreachable!(),
        };
        parser.set_language(&grammar).map_err(|e| AnalyzerError {
            code: AnalyzerErrorCode::Internal,
            message: format!("Unable to configure parser: {e}"),
        })?;
        let tree = parser
            .parse(input.source, None)
            .ok_or_else(|| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: "Parser did not return a syntax tree.".into(),
            })?;
        Ok(analyze_tree(input, language, &tree))
    }
}

fn cap(feature: AnalysisFeature, support: SupportLevel) -> AnalyzerCapability {
    AnalyzerCapability {
        feature,
        support,
        limitation: None,
    }
}

fn language_for(path: &str) -> Option<SourceLanguage> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "js" | "jsx" | "mjs" | "cjs" => Some(SourceLanguage::JavaScript),
        "ts" | "tsx" | "mts" | "cts" => Some(SourceLanguage::TypeScript),
        _ => None,
    }
}

#[derive(Clone)]
struct Context {
    id: SymbolId,
}

fn analyze_tree(
    input: &AnalyzerInput<'_>,
    language: SourceLanguage,
    tree: &Tree,
) -> AnalysisOutput {
    let mut out = AnalysisOutput {
        language,
        completeness: AnalysisCompleteness::Complete,
        symbols: Vec::new(),
        imports: Vec::new(),
        relationships: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut stack = Vec::new();
    walk(input, tree.root_node(), &mut out, &mut stack);
    if tree.root_node().has_error() {
        out.completeness = AnalysisCompleteness::Partial;
        out.diagnostics.push(AnalyzerDiagnostic {
            code: DiagnosticCode::ParseError,
            severity: DiagnosticSeverity::Warning,
            message: "Syntax errors limited JavaScript/TypeScript analysis.".into(),
            location: None,
        });
    }
    out
}

fn walk(
    input: &AnalyzerInput<'_>,
    node: Node<'_>,
    out: &mut AnalysisOutput,
    stack: &mut Vec<Context>,
) {
    if out.symbols.len() + out.relationships.len() >= MAX_ITEMS {
        return;
    }
    let kind = node.kind();
    if kind == "import_statement" {
        out.imports.push(AnalyzerImport {
            name: node
                .utf8_text(input.source.as_bytes())
                .unwrap_or_default()
                .to_owned(),
            alias: None,
            is_static: false,
            is_wildcard: false,
            location: range(input, node),
        });
    }
    let mut pushed = false;
    if let Some((name, symbol_kind)) = declaration(node, input.source) {
        let id = SymbolId(format!("js:{}:{}", name, node.start_byte()));
        out.symbols.push(AnalyzerSymbol {
            id: id.clone(),
            name,
            kind: symbol_kind,
            parent_id: stack.last().map(|c| c.id.clone()),
            location: range(input, node),
            confidence: Confidence::Confirmed,
        });
        stack.push(Context { id });
        pushed = true;
    }
    if let Some(ctx) = stack.last() {
        if matches!(kind, "call_expression" | "new_expression" | "call") {
            let target = node
                .child_by_field_name("function")
                .or_else(|| node.child(0))
                .and_then(|n| n.utf8_text(input.source.as_bytes()).ok())
                .map(str::to_owned);
            out.relationships.push(AnalyzerRelationship {
                kind: RelationshipKind::Calls,
                source_symbol_id: ctx.id.clone(),
                target_symbol_id: None,
                target_name: target,
                location: range(input, node),
                confidence: Confidence::Inferred,
            });
        } else if matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "while_statement"
                | "switch_statement"
                | "try_statement"
        ) {
            out.relationships.push(AnalyzerRelationship {
                kind: RelationshipKind::Controls,
                source_symbol_id: ctx.id.clone(),
                target_symbol_id: None,
                target_name: Some(kind.to_owned()),
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

fn declaration(node: Node<'_>, source: &str) -> Option<(String, SymbolKind)> {
    let kind = match node.kind() {
        "class_declaration" | "class" => SymbolKind::Class,
        "interface_declaration" => SymbolKind::Interface,
        "enum_declaration" => SymbolKind::Enum,
        "function_declaration" | "function" | "arrow_function" => SymbolKind::Function,
        "method_definition" | "method_signature" => SymbolKind::Method,
        "type_alias_declaration" => SymbolKind::Class,
        "lexical_declaration" | "variable_declaration" => SymbolKind::Property,
        _ => return None,
    };
    let name = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(str::to_owned)
        .or_else(|| {
            node.child(0)
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(str::to_owned)
        })?;
    Some((name, kind))
}

fn range(input: &AnalyzerInput<'_>, node: Node<'_>) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        relative_path: input.relative_path.to_owned(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: start.row as u32 + 1,
        start_column: start.column as u32 + 1,
        end_line: end.row as u32 + 1,
        end_column: end.column as u32 + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::Analyzer;
    #[test]
    fn extracts_types_imports_calls_and_control_flow() {
        let source = "import x from 'x'; class A { run() { if (x) foo(); } }";
        let out = JavaScriptTypeScriptAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "a.js",
                source,
            })
            .unwrap();
        assert!(!out.symbols.is_empty());
        assert_eq!(out.imports.len(), 1);
        assert!(
            out.relationships
                .iter()
                .any(|r| r.kind == RelationshipKind::Calls)
        );
        assert!(
            out.relationships
                .iter()
                .any(|r| r.kind == RelationshipKind::Controls)
        );
    }
}
