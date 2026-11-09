use std::path::Path;

use tree_sitter::{Node, Parser, Point, Tree};

use crate::analyzer::{
    AnalysisCompleteness, AnalysisFeature, AnalysisOutput, Analyzer, AnalyzerCapability,
    AnalyzerDiagnostic, AnalyzerError, AnalyzerErrorCode, AnalyzerImport, AnalyzerInput,
    AnalyzerRelationship, AnalyzerSymbol, Confidence, DiagnosticCode, DiagnosticSeverity,
    RelationshipKind, SourceLanguage, SourceRange, SupportLevel, SymbolId, SymbolKind,
};

const MAX_SOURCE_BYTES: usize = 5 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 256;

pub struct KotlinAnalyzer;

#[derive(Clone)]
struct SymbolContext {
    id: SymbolId,
    name: String,
    kind: SymbolKind,
}

#[derive(Clone, Default)]
struct WalkContext {
    parent: Option<SymbolContext>,
    enclosing_type: Option<SymbolContext>,
}

impl Analyzer for KotlinAnalyzer {
    fn descriptor(&self) -> crate::analyzer::AnalyzerDescriptor {
        use AnalysisFeature::{Annotations, ControlFlow, Imports, Relationships, Symbols};

        crate::analyzer::AnalyzerDescriptor {
            language: SourceLanguage::Kotlin,
            capabilities: vec![
                capability(Symbols, SupportLevel::Supported, None),
                capability(Imports, SupportLevel::Supported, None),
                capability(Annotations, SupportLevel::Supported, None),
                capability(
                    Relationships,
                    SupportLevel::Partial,
                    Some(
                        "Inheritance targets are reported by syntax; class/interface resolution is not performed.",
                    ),
                ),
                capability(
                    ControlFlow,
                    SupportLevel::Unsupported,
                    Some("Function-body control-flow analysis is not implemented."),
                ),
            ],
        }
    }

    fn analyze(&self, input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError> {
        if !is_kotlin_path(input.relative_path) {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::InvalidInput,
                message: "Kotlin analysis requires a .kt or .kts relative path.".into(),
            });
        }
        if input.source.len() > MAX_SOURCE_BYTES {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::ResourceLimitExceeded,
                message: format!(
                    "Kotlin source exceeds the {MAX_SOURCE_BYTES}-byte analysis limit."
                ),
            });
        }

        let mut parser = Parser::new();
        let language = tree_sitter_kotlin_ng::LANGUAGE;
        parser
            .set_language(&language.into())
            .map_err(|error| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: format!("Unable to configure the Kotlin parser: {error}"),
            })?;
        let tree = parser
            .parse(input.source, None)
            .ok_or_else(|| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: "The Kotlin parser did not return a syntax tree.".into(),
            })?;

        Ok(analyze_tree(&input, &tree))
    }
}

fn capability(
    feature: AnalysisFeature,
    support: SupportLevel,
    limitation: Option<&str>,
) -> AnalyzerCapability {
    AnalyzerCapability {
        feature,
        support,
        limitation: limitation.map(str::to_owned),
    }
}

fn is_kotlin_path(relative_path: &str) -> bool {
    Path::new(relative_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("kt") || extension.eq_ignore_ascii_case("kts")
        })
}

fn analyze_tree(input: &AnalyzerInput<'_>, tree: &Tree) -> AnalysisOutput {
    let line_index = LineIndex::new(input.source);
    let root = tree.root_node();
    let package_node = find_child_by_kind(root, "package_header");
    let package_context = package_node.map(|node| {
        let name = node_text(node, input.source)
            .trim()
            .strip_prefix("package")
            .unwrap_or("")
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_owned();
        let id = symbol_id(SymbolKind::Package, node.start_byte());
        let symbol = AnalyzerSymbol {
            id: id.clone(),
            name: name.clone(),
            kind: SymbolKind::Package,
            parent_id: None,
            location: source_range(input.relative_path, input.source, &line_index, node),
            confidence: Confidence::Confirmed,
        };
        (
            symbol,
            SymbolContext {
                id,
                name,
                kind: SymbolKind::Package,
            },
        )
    });

    let mut output = AnalysisOutput {
        language: SourceLanguage::Kotlin,
        completeness: AnalysisCompleteness::Complete,
        symbols: package_context
            .as_ref()
            .map(|(symbol, _)| vec![symbol.clone()])
            .unwrap_or_default(),
        imports: Vec::new(),
        relationships: Vec::new(),
        diagnostics: Vec::new(),
    };
    let base_context = WalkContext {
        parent: package_context.map(|(_, context)| context),
        enclosing_type: None,
    };

    let mut stack = Vec::new();
    let mut cursor = root.walk();
    let root_children: Vec<_> = root.children(&mut cursor).collect();
    for child in root_children.into_iter().rev() {
        if child.kind() != "package_header" {
            stack.push((child, base_context.clone()));
        }
    }

    let mut overflowed_diagnostics = false;
    while let Some((node, context)) = stack.pop() {
        if node.kind() == "import" {
            if let Some(import) = parse_import(input, node, &line_index) {
                output.imports.push(import);
            }
        }

        if node.is_error() || node.is_missing() {
            push_diagnostic(
                &mut output,
                &mut overflowed_diagnostics,
                AnalyzerDiagnostic {
                    code: DiagnosticCode::ParseError,
                    severity: DiagnosticSeverity::Warning,
                    message: if node.is_missing() {
                        format!("Kotlin parser expected `{}` here.", node.kind())
                    } else {
                        "Kotlin syntax could not be parsed completely.".into()
                    },
                    location: Some(source_range(
                        input.relative_path,
                        input.source,
                        &line_index,
                        node,
                    )),
                },
            );
        }

        if matches!(node.kind(), "type_alias" | "multi_variable_declaration") {
            push_diagnostic(
                &mut output,
                &mut overflowed_diagnostics,
                AnalyzerDiagnostic {
                    code: DiagnosticCode::UnsupportedSyntax,
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "Kotlin `{}` is not represented in the current symbol model.",
                        node.kind()
                    ),
                    location: Some(source_range(
                        input.relative_path,
                        input.source,
                        &line_index,
                        node,
                    )),
                },
            );
        }

        let mut child_context = context.clone();
        if let Some((symbol, symbol_context)) = make_symbol(input, &line_index, node, &context) {
            add_annotations(input, &line_index, node, &symbol, &mut output);
            add_inheritance(input, &line_index, node, &symbol, &mut output);
            output.symbols.push(symbol);
            child_context.parent = Some(symbol_context.clone());
            if matches!(
                symbol_context.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Object | SymbolKind::Enum
            ) {
                child_context.enclosing_type = Some(symbol_context);
            }
        }

        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, child_context.clone()));
        }
    }

    if overflowed_diagnostics {
        output.diagnostics.push(AnalyzerDiagnostic {
            code: DiagnosticCode::ResourceLimitReached,
            severity: DiagnosticSeverity::Warning,
            message: format!("Only the first {MAX_DIAGNOSTICS} Kotlin diagnostics are reported."),
            location: None,
        });
    }
    if !output.diagnostics.is_empty() {
        output.completeness = AnalysisCompleteness::Partial;
    }
    output
}

fn push_diagnostic(
    output: &mut AnalysisOutput,
    overflowed: &mut bool,
    diagnostic: AnalyzerDiagnostic,
) {
    if output.diagnostics.len() < MAX_DIAGNOSTICS {
        output.diagnostics.push(diagnostic);
    } else {
        *overflowed = true;
    }
}

fn make_symbol(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    node: Node<'_>,
    context: &WalkContext,
) -> Option<(AnalyzerSymbol, SymbolContext)> {
    let (kind, name) = match node.kind() {
        "class_declaration" => {
            let kind = if has_direct_token(node, "interface") {
                SymbolKind::Interface
            } else if has_modifier(node, input.source, "enum") {
                SymbolKind::Enum
            } else {
                SymbolKind::Class
            };
            (kind, identifier_field(node, "name", input.source)?)
        }
        "object_declaration" => (
            SymbolKind::Object,
            identifier_field(node, "name", input.source).unwrap_or_else(|| "Companion".into()),
        ),
        "companion_object" => (
            SymbolKind::Object,
            identifier_field(node, "name", input.source).unwrap_or_else(|| "Companion".into()),
        ),
        "function_declaration" => {
            let is_type_member = context.parent.as_ref().is_some_and(|parent| {
                matches!(
                    parent.kind,
                    SymbolKind::Class
                        | SymbolKind::Interface
                        | SymbolKind::Object
                        | SymbolKind::Enum
                )
            });
            let kind = if is_type_member {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            (kind, identifier_field(node, "name", input.source)?)
        }
        "property_declaration" => {
            if find_descendant_by_kind(node, "multi_variable_declaration").is_some() {
                return None;
            }
            let declaration = find_child_by_kind(node, "variable_declaration")?;
            (
                SymbolKind::Property,
                first_identifier(declaration, input.source)?,
            )
        }
        "primary_constructor" | "secondary_constructor" => {
            let parent = context.enclosing_type.as_ref()?;
            (SymbolKind::Constructor, parent.name.clone())
        }
        "class_parameter" if has_direct_token(node, "val") || has_direct_token(node, "var") => {
            context.enclosing_type.as_ref()?;
            let name = find_child_by_kind(node, "identifier")
                .map(|identifier| node_text(identifier, input.source).to_owned())?;
            (SymbolKind::Property, name)
        }
        _ => return None,
    };

    let id = symbol_id(kind, node.start_byte());
    let parent = if node.kind() == "class_parameter" {
        context.enclosing_type.as_ref()
    } else {
        context.parent.as_ref()
    };
    let symbol = AnalyzerSymbol {
        id: id.clone(),
        name: name.clone(),
        kind,
        parent_id: parent.map(|parent| parent.id.clone()),
        location: source_range(input.relative_path, input.source, line_index, node),
        confidence: Confidence::Confirmed,
    };
    Some((symbol, SymbolContext { id, name, kind }))
}

fn add_annotations(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    declaration: Node<'_>,
    symbol: &AnalyzerSymbol,
    output: &mut AnalysisOutput,
) {
    let Some(modifiers) = find_child_by_kind(declaration, "modifiers") else {
        return;
    };
    let mut stack = vec![modifiers];
    while let Some(node) = stack.pop() {
        if node.kind() == "annotation" {
            if let Some(name) = annotation_name(node, input.source) {
                output.relationships.push(AnalyzerRelationship {
                    kind: RelationshipKind::AnnotatedBy,
                    source_symbol_id: symbol.id.clone(),
                    target_symbol_id: None,
                    target_name: Some(name),
                    location: source_range(input.relative_path, input.source, line_index, node),
                    confidence: Confidence::Confirmed,
                });
            }
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn add_inheritance(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    declaration: Node<'_>,
    symbol: &AnalyzerSymbol,
    output: &mut AnalysisOutput,
) {
    if !matches!(
        declaration.kind(),
        "class_declaration" | "object_declaration" | "companion_object"
    ) {
        return;
    }
    let Some(specifiers) = find_child_by_kind(declaration, "delegation_specifiers") else {
        return;
    };
    let mut stack = vec![specifiers];
    while let Some(node) = stack.pop() {
        if node.kind() == "delegation_specifier" {
            if let Some(name) = delegation_target_name(node, input.source) {
                output.relationships.push(AnalyzerRelationship {
                    kind: RelationshipKind::Inherits,
                    source_symbol_id: symbol.id.clone(),
                    target_symbol_id: None,
                    target_name: Some(name),
                    location: source_range(input.relative_path, input.source, line_index, node),
                    confidence: Confidence::Inferred,
                });
            }
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn parse_import(
    input: &AnalyzerInput<'_>,
    node: Node<'_>,
    line_index: &LineIndex,
) -> Option<AnalyzerImport> {
    let raw = node_text(node, input.source)
        .trim()
        .strip_prefix("import")?
        .trim()
        .trim_end_matches(';')
        .trim();
    let (path, alias) = raw
        .split_once(" as ")
        .map(|(path, alias)| (path.trim(), Some(alias.trim().to_owned())))
        .unwrap_or((raw, None));
    let is_wildcard = path.ends_with(".*") || path == "*";
    let name = path.strip_suffix(".*").unwrap_or(path).to_owned();
    (!name.is_empty()).then(|| AnalyzerImport {
        name,
        alias,
        is_static: false,
        is_wildcard,
        location: source_range(input.relative_path, input.source, line_index, node),
    })
}

fn annotation_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "use_site_target" {
            continue;
        }
        if current.kind() == "user_type" {
            return last_direct_identifier(current, source);
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

fn delegation_target_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "annotation" {
            continue;
        }
        if let Some(user_type) = find_descendant_by_kind(child, "user_type") {
            return last_direct_identifier(user_type, source);
        }
    }
    None
}

fn identifier_field(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|identifier| node_text(identifier, source).to_owned())
}

fn first_identifier(node: Node<'_>, source: &str) -> Option<String> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "identifier" {
            return Some(node_text(current, source).to_owned());
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

fn last_direct_identifier(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "identifier")
        .last()
        .map(|identifier| node_text(identifier, source).to_owned())
}

fn has_direct_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == token)
}

fn has_modifier(node: Node<'_>, source: &str, modifier: &str) -> bool {
    let Some(modifiers) = find_child_by_kind(node, "modifiers") else {
        return false;
    };
    let mut stack = vec![modifiers];
    while let Some(current) = stack.pop() {
        if node_text(current, source).trim() == modifier {
            return true;
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    false
}

fn find_child_by_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn find_descendant_by_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == kind {
            return Some(current);
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

fn symbol_id(kind: SymbolKind, start_byte: usize) -> SymbolId {
    SymbolId(format!("kotlin:{kind:?}:{start_byte}"))
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some((offset + 1) as u32)),
        );
        Self { starts }
    }

    fn unicode_column(&self, source: &str, point: Point) -> u32 {
        let line_start = self
            .starts
            .get(point.row)
            .copied()
            .map_or(source.len(), |offset| offset as usize);
        let column_end = line_start.saturating_add(point.column).min(source.len());
        source
            .get(line_start..column_end)
            .map_or(1, |text| text.chars().count() as u32 + 1)
    }
}

fn source_range(
    relative_path: &str,
    source: &str,
    line_index: &LineIndex,
    node: Node<'_>,
) -> SourceRange {
    SourceRange {
        relative_path: relative_path.to_owned(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row as u32 + 1,
        start_column: line_index.unicode_column(source, node.start_position()),
        end_line: node.end_position().row as u32 + 1,
        end_column: line_index.unicode_column(source, node.end_position()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(source: &str) -> AnalysisOutput {
        KotlinAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "src/main/kotlin/Example.kt",
                source,
            })
            .unwrap()
    }

    #[test]
    fn extracts_package_imports_declarations_inheritance_and_annotations() {
        let output = analyze(
            "package sample\n\
             import sample.api.Service as ApiService\n\
             import sample.model.*\n\
             @ServiceMarker\n\
             class Worker(val service: ApiService) : Runnable {\n\
                 @JvmName(\"execute\")\n\
                 fun runTask() = Unit\n\
                 val status = \"ready\"\n\
             }\n\
             interface Runnable\n\
             object Registry\n\
             fun createWorker() = Worker(Registry)",
        );

        assert_eq!(output.completeness, AnalysisCompleteness::Complete);
        assert_eq!(output.symbols[0].kind, SymbolKind::Package);
        assert_eq!(output.imports.len(), 2);
        assert_eq!(output.imports[0].name, "sample.api.Service");
        assert_eq!(output.imports[0].alias.as_deref(), Some("ApiService"));
        assert!(!output.imports[0].is_static);
        assert!(output.imports[1].is_wildcard);
        assert_eq!(output.imports[1].name, "sample.model");
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Worker" && symbol.kind == SymbolKind::Class)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Runnable" && symbol.kind == SymbolKind::Interface)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Registry" && symbol.kind == SymbolKind::Object)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "runTask" && symbol.kind == SymbolKind::Method)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "service" && symbol.kind == SymbolKind::Property)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "status" && symbol.kind == SymbolKind::Property)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "createWorker" && symbol.kind == SymbolKind::Function)
        );
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::Inherits
                && relationship.target_name.as_deref() == Some("Runnable")
        }));
        assert!(
            output
                .relationships
                .iter()
                .all(|relationship| relationship.kind != RelationshipKind::Extends)
        );
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::AnnotatedBy
                && relationship.target_name.as_deref() == Some("ServiceMarker")
        }));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::AnnotatedBy
                && relationship.target_name.as_deref() == Some("JvmName")
        }));
    }

    #[test]
    fn comments_and_strings_are_not_mistaken_for_declarations() {
        let output = analyze(
            "// class Ghost\n\
             val message = \"fun hidden() { class AlsoGhost }\"\n\
             /* object Noisy { fun nope() = Unit } */\n\
             fun visible() = Unit",
        );

        let names: Vec<_> = output
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(names, ["message", "visible"]);
    }

    #[test]
    fn malformed_source_returns_partial_symbols_and_diagnostics() {
        let output = analyze("fun valid() = Unit\nclass Broken(\nfun alsoValid() = Unit");

        assert_eq!(output.completeness, AnalysisCompleteness::Partial);
        assert!(!output.diagnostics.is_empty());
        assert!(output.symbols.iter().any(|symbol| symbol.name == "valid"));
    }

    #[test]
    fn source_ranges_use_relative_paths_and_unicode_columns() {
        let output = analyze("// 한\nfun café() = Unit");
        let function = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "café")
            .unwrap();

        assert_eq!(
            function.location.relative_path,
            "src/main/kotlin/Example.kt"
        );
        assert_eq!(function.location.start_line, 2);
        assert_eq!(function.location.start_column, 1);
        assert_eq!(function.location.end_line, 2);
        assert_eq!(function.location.end_column, 18);
        assert_eq!(function.location.start_byte, "// 한\n".len());
    }

    #[test]
    fn unsupported_syntax_marks_results_partial() {
        let output = analyze("typealias UserId = String\nfun lookup() = Unit");

        assert_eq!(output.completeness, AnalysisCompleteness::Partial);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::UnsupportedSyntax)
        );
        assert!(output.symbols.iter().any(|symbol| symbol.name == "lookup"));
    }

    #[test]
    fn rejects_non_kotlin_inputs() {
        let error = KotlinAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "src/Main.java",
                source: "class Main {}",
            })
            .unwrap_err();
        assert_eq!(error.code, AnalyzerErrorCode::InvalidInput);
    }

    #[test]
    fn extracts_secondary_constructors_companion_objects_enums_and_local_functions() {
        let output = analyze(
            "class Worker(val value: String) {\n\
                 constructor() : this(\"\")\n\
                 fun member() { fun local() = Unit }\n\
                 companion object Factory { fun make() = Worker(\"\") }\n\
             }\n\
             enum class Mode { ON, OFF }",
        );

        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Factory" && symbol.kind == SymbolKind::Object)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Mode" && symbol.kind == SymbolKind::Enum)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "local" && symbol.kind == SymbolKind::Function)
        );
        assert!(
            output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "make" && symbol.kind == SymbolKind::Method)
        );
        assert_eq!(
            output
                .symbols
                .iter()
                .filter(|symbol| symbol.kind == SymbolKind::Constructor)
                .count(),
            2
        );
    }

    #[test]
    fn registry_registers_the_kotlin_analyzer() {
        let registry = crate::analyzer::AnalyzerRegistry::with_kotlin();
        let output = registry
            .analyze(
                SourceLanguage::Kotlin,
                AnalyzerInput {
                    relative_path: "src/Main.kt",
                    source: "fun main() = Unit",
                },
            )
            .unwrap();

        assert_eq!(output.language, SourceLanguage::Kotlin);
        assert!(output.symbols.iter().any(|symbol| symbol.name == "main"));
    }
}
