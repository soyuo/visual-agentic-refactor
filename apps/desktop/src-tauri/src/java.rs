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
const MAX_RELATIONSHIPS: usize = 50_000;

pub struct JavaAnalyzer;

#[derive(Clone)]
struct SymbolContext {
    id: SymbolId,
    name: String,
    kind: SymbolKind,
}

impl Analyzer for JavaAnalyzer {
    fn descriptor(&self) -> crate::analyzer::AnalyzerDescriptor {
        use AnalysisFeature::{Annotations, ControlFlow, Imports, Relationships, Symbols};

        crate::analyzer::AnalyzerDescriptor {
            language: SourceLanguage::Java,
            capabilities: vec![
                capability(Symbols, SupportLevel::Supported, None),
                capability(Imports, SupportLevel::Supported, None),
                capability(Annotations, SupportLevel::Supported, None),
                capability(
                    Relationships,
                    SupportLevel::Partial,
                    Some(
                        "Call and inheritance targets are syntax candidates; overload and type resolution is not performed.",
                    ),
                ),
                capability(
                    ControlFlow,
                    SupportLevel::Partial,
                    Some(
                        "Conditional, loop, and exception constructs are reported as syntax candidates; a full control-flow graph is not built.",
                    ),
                ),
            ],
        }
    }

    fn analyze(&self, input: AnalyzerInput<'_>) -> Result<AnalysisOutput, AnalyzerError> {
        if !is_java_path(input.relative_path) {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::InvalidInput,
                message: "Java analysis requires a .java relative path.".into(),
            });
        }
        if input.source.len() > MAX_SOURCE_BYTES {
            return Err(AnalyzerError {
                code: AnalyzerErrorCode::ResourceLimitExceeded,
                message: format!("Java source exceeds the {MAX_SOURCE_BYTES}-byte analysis limit."),
            });
        }

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .map_err(|error| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: format!("Unable to configure the Java parser: {error}"),
            })?;
        let tree = parser
            .parse(input.source, None)
            .ok_or_else(|| AnalyzerError {
                code: AnalyzerErrorCode::Internal,
                message: "The Java parser did not return a syntax tree.".into(),
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

fn is_java_path(relative_path: &str) -> bool {
    Path::new(relative_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
}

fn analyze_tree(input: &AnalyzerInput<'_>, tree: &Tree) -> AnalysisOutput {
    let line_index = LineIndex::new(input.source);
    let root = tree.root_node();
    let package_node = find_child_by_kind(root, "package_declaration");
    let package_context = package_node.and_then(|node| {
        let name = path_child(node, input.source)?;
        let id = symbol_id(SymbolKind::Package, node.start_byte());
        let symbol = AnalyzerSymbol {
            id: id.clone(),
            name: name.clone(),
            kind: SymbolKind::Package,
            parent_id: None,
            location: source_range(input.relative_path, input.source, &line_index, node),
            confidence: Confidence::Confirmed,
        };
        Some((
            symbol,
            SymbolContext {
                id,
                name,
                kind: SymbolKind::Package,
            },
        ))
    });

    let mut output = AnalysisOutput {
        language: SourceLanguage::Java,
        completeness: AnalysisCompleteness::Complete,
        symbols: package_context
            .as_ref()
            .map(|(symbol, _)| vec![symbol.clone()])
            .unwrap_or_default(),
        imports: Vec::new(),
        relationships: Vec::new(),
        diagnostics: Vec::new(),
    };
    let base_parent = package_context.map(|(_, context)| context);
    let mut stack = Vec::new();
    let mut cursor = root.walk();
    let root_children: Vec<_> = root.children(&mut cursor).collect();
    for child in root_children.into_iter().rev() {
        if child.kind() != "package_declaration" {
            stack.push((child, base_parent.clone()));
        }
    }

    let mut overflowed_diagnostics = false;
    let mut overflowed_relationships = false;
    while let Some((node, parent)) = stack.pop() {
        if node.kind() == "import_declaration" {
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
                        format!("Java parser expected `{}` here.", node.kind())
                    } else {
                        "Java syntax could not be parsed completely.".into()
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

        if matches!(
            node.kind(),
            "module_declaration" | "annotation_type_declaration"
        ) {
            push_diagnostic(
                &mut output,
                &mut overflowed_diagnostics,
                AnalyzerDiagnostic {
                    code: DiagnosticCode::UnsupportedSyntax,
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "Java `{}` is not represented in the current symbol model.",
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

        let symbols = make_symbols(input, &line_index, node, parent.as_ref());
        let mut child_parent = parent.clone();
        for (symbol, symbol_context) in symbols {
            add_annotations(
                input,
                &line_index,
                node,
                &symbol,
                &mut output,
                &mut overflowed_relationships,
            );
            add_type_relationships(
                input,
                &line_index,
                node,
                &symbol,
                &mut output,
                &mut overflowed_relationships,
            );
            output.symbols.push(symbol);
            if matches!(
                symbol_context.kind,
                SymbolKind::Class
                    | SymbolKind::Interface
                    | SymbolKind::Enum
                    | SymbolKind::Record
                    | SymbolKind::Constructor
                    | SymbolKind::Method
                    | SymbolKind::Function
            ) {
                child_parent = Some(symbol_context);
            }
        }

        add_call_and_flow_relationships(
            input,
            &line_index,
            node,
            parent.as_ref(),
            &mut output,
            &mut overflowed_relationships,
        );

        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, child_parent.clone()));
        }
    }

    if overflowed_relationships {
        push_diagnostic(
            &mut output,
            &mut overflowed_diagnostics,
            AnalyzerDiagnostic {
                code: DiagnosticCode::ResourceLimitReached,
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "Only the first {MAX_RELATIONSHIPS} Java relationships are reported."
                ),
                location: None,
            },
        );
    }
    if overflowed_diagnostics {
        output.diagnostics.push(AnalyzerDiagnostic {
            code: DiagnosticCode::ResourceLimitReached,
            severity: DiagnosticSeverity::Warning,
            message: format!("Only the first {MAX_DIAGNOSTICS} Java diagnostics are reported."),
            location: None,
        });
    }
    if !output.diagnostics.is_empty() {
        output.completeness = AnalysisCompleteness::Partial;
    }
    output
}

fn make_symbols(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    node: Node<'_>,
    parent: Option<&SymbolContext>,
) -> Vec<(AnalyzerSymbol, SymbolContext)> {
    let mut declarations = Vec::new();
    match node.kind() {
        "class_declaration" => push_named_declaration(
            &mut declarations,
            input,
            line_index,
            node,
            parent,
            SymbolKind::Class,
        ),
        "interface_declaration" => push_named_declaration(
            &mut declarations,
            input,
            line_index,
            node,
            parent,
            SymbolKind::Interface,
        ),
        "enum_declaration" => push_named_declaration(
            &mut declarations,
            input,
            line_index,
            node,
            parent,
            SymbolKind::Enum,
        ),
        "record_declaration" => push_named_declaration(
            &mut declarations,
            input,
            line_index,
            node,
            parent,
            SymbolKind::Record,
        ),
        "method_declaration" => {
            let kind = if parent.is_some_and(|parent| is_type_kind(parent.kind)) {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            push_field_named_declaration(
                &mut declarations,
                input,
                line_index,
                node,
                parent,
                kind,
                "name",
            );
        }
        "constructor_declaration" => {
            let name = identifier_field(node, "name", input.source)
                .or_else(|| parent.map(|parent| parent.name.clone()));
            if let Some(name) = name {
                declarations.push(make_symbol(
                    input,
                    line_index,
                    node,
                    parent,
                    SymbolKind::Constructor,
                    name,
                ));
            }
        }
        "compact_constructor_declaration" => {
            if let Some(name) = parent.map(|parent| parent.name.clone()) {
                declarations.push(make_symbol(
                    input,
                    line_index,
                    node,
                    parent,
                    SymbolKind::Constructor,
                    name,
                ));
            }
        }
        "field_declaration" => {
            let mut cursor = node.walk();
            for declarator in node
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
            {
                if let Some(name) = identifier_field(declarator, "name", input.source) {
                    declarations.push(make_symbol(
                        input,
                        line_index,
                        declarator,
                        parent,
                        SymbolKind::Field,
                        name,
                    ));
                }
            }
        }
        "enum_constant" => push_field_named_declaration(
            &mut declarations,
            input,
            line_index,
            node,
            parent,
            SymbolKind::Field,
            "name",
        ),
        "formal_parameter" if is_record_component(node) => {
            push_field_named_declaration(
                &mut declarations,
                input,
                line_index,
                node,
                parent,
                SymbolKind::Field,
                "name",
            );
        }
        _ => {}
    }
    declarations
}

fn push_named_declaration(
    output: &mut Vec<(AnalyzerSymbol, SymbolContext)>,
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    node: Node<'_>,
    parent: Option<&SymbolContext>,
    kind: SymbolKind,
) {
    push_field_named_declaration(output, input, line_index, node, parent, kind, "name");
}

fn push_field_named_declaration(
    output: &mut Vec<(AnalyzerSymbol, SymbolContext)>,
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    node: Node<'_>,
    parent: Option<&SymbolContext>,
    kind: SymbolKind,
    field: &str,
) {
    if let Some(name) = identifier_field(node, field, input.source) {
        output.push(make_symbol(input, line_index, node, parent, kind, name));
    }
}

fn make_symbol(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    node: Node<'_>,
    parent: Option<&SymbolContext>,
    kind: SymbolKind,
    name: String,
) -> (AnalyzerSymbol, SymbolContext) {
    let id = symbol_id(kind, node.start_byte());
    let symbol = AnalyzerSymbol {
        id: id.clone(),
        name: name.clone(),
        kind,
        parent_id: parent.map(|parent| parent.id.clone()),
        location: source_range(input.relative_path, input.source, line_index, node),
        confidence: Confidence::Confirmed,
    };
    (symbol, SymbolContext { id, name, kind })
}

fn add_type_relationships(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    declaration: Node<'_>,
    symbol: &AnalyzerSymbol,
    output: &mut AnalysisOutput,
    overflowed_relationships: &mut bool,
) {
    match declaration.kind() {
        "class_declaration" => {
            if let Some(superclass) = declaration.child_by_field_name("superclass") {
                push_type_relationship(
                    input,
                    line_index,
                    superclass,
                    symbol,
                    RelationshipKind::Extends,
                    output,
                    overflowed_relationships,
                );
            }
            if let Some(interfaces) = declaration.child_by_field_name("interfaces") {
                push_type_list_relationships(
                    input,
                    line_index,
                    interfaces,
                    symbol,
                    RelationshipKind::Implements,
                    output,
                    overflowed_relationships,
                );
            }
        }
        "interface_declaration" => {
            if let Some(interfaces) = find_child_by_kind(declaration, "extends_interfaces") {
                push_type_list_relationships(
                    input,
                    line_index,
                    interfaces,
                    symbol,
                    RelationshipKind::Extends,
                    output,
                    overflowed_relationships,
                );
            }
        }
        "enum_declaration" | "record_declaration" => {
            if let Some(interfaces) = declaration.child_by_field_name("interfaces") {
                push_type_list_relationships(
                    input,
                    line_index,
                    interfaces,
                    symbol,
                    RelationshipKind::Implements,
                    output,
                    overflowed_relationships,
                );
            }
        }
        _ => {}
    }
}

fn push_type_list_relationships(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    list: Node<'_>,
    symbol: &AnalyzerSymbol,
    kind: RelationshipKind,
    output: &mut AnalysisOutput,
    overflowed_relationships: &mut bool,
) {
    let type_list = if list.kind() == "type_list" {
        list
    } else {
        let Some(type_list) = find_child_by_kind(list, "type_list") else {
            return;
        };
        type_list
    };
    let mut cursor = type_list.walk();
    for type_node in type_list.named_children(&mut cursor) {
        push_type_relationship(
            input,
            line_index,
            type_node,
            symbol,
            kind,
            output,
            overflowed_relationships,
        );
    }
}

fn push_type_relationship(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    type_node: Node<'_>,
    symbol: &AnalyzerSymbol,
    kind: RelationshipKind,
    output: &mut AnalysisOutput,
    overflowed_relationships: &mut bool,
) {
    let Some(name_node) = find_type_name_node(type_node) else {
        return;
    };
    push_relationship(
        output,
        overflowed_relationships,
        AnalyzerRelationship {
            kind,
            source_symbol_id: symbol.id.clone(),
            target_symbol_id: None,
            target_name: Some(candidate_name(node_text(name_node, input.source))),
            location: source_range(input.relative_path, input.source, line_index, name_node),
            confidence: Confidence::Inferred,
        },
    );
}

fn find_type_name_node<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if matches!(current.kind(), "type_identifier" | "scoped_type_identifier") {
            return Some(current);
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

fn add_annotations(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    declaration: Node<'_>,
    symbol: &AnalyzerSymbol,
    output: &mut AnalysisOutput,
    overflowed_relationships: &mut bool,
) {
    let Some(modifiers) = find_child_by_kind(declaration, "modifiers") else {
        return;
    };
    let mut stack = vec![modifiers];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "annotation" | "marker_annotation") {
            if let Some(name) = node
                .child_by_field_name("name")
                .map(|name| node_text(name, input.source).to_owned())
            {
                push_relationship(
                    output,
                    overflowed_relationships,
                    AnalyzerRelationship {
                        kind: RelationshipKind::AnnotatedBy,
                        source_symbol_id: symbol.id.clone(),
                        target_symbol_id: None,
                        target_name: Some(candidate_name(&name)),
                        location: source_range(input.relative_path, input.source, line_index, node),
                        confidence: Confidence::Confirmed,
                    },
                );
            }
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn add_call_and_flow_relationships(
    input: &AnalyzerInput<'_>,
    line_index: &LineIndex,
    node: Node<'_>,
    scope: Option<&SymbolContext>,
    output: &mut AnalysisOutput,
    overflowed_relationships: &mut bool,
) {
    let Some(scope) = scope.filter(|scope| {
        matches!(
            scope.kind,
            SymbolKind::Method | SymbolKind::Constructor | SymbolKind::Function
        )
    }) else {
        return;
    };

    let candidate = match node.kind() {
        "method_invocation" | "object_creation_expression" | "explicit_constructor_invocation" => {
            let Some(target_name) = call_target_name(node, input.source) else {
                return;
            };
            (
                RelationshipKind::Calls,
                Some(target_name),
                Confidence::Inferred,
            )
        }
        "if_statement" => (
            RelationshipKind::Controls,
            Some("if".into()),
            Confidence::Confirmed,
        ),
        "switch_statement" | "switch_expression" => (
            RelationshipKind::Controls,
            Some("switch".into()),
            Confidence::Confirmed,
        ),
        "for_statement" => (
            RelationshipKind::Controls,
            Some("for".into()),
            Confidence::Confirmed,
        ),
        "enhanced_for_statement" => (
            RelationshipKind::Controls,
            Some("enhanced-for".into()),
            Confidence::Confirmed,
        ),
        "while_statement" => (
            RelationshipKind::Controls,
            Some("while".into()),
            Confidence::Confirmed,
        ),
        "do_statement" => (
            RelationshipKind::Controls,
            Some("do-while".into()),
            Confidence::Confirmed,
        ),
        "try_statement" => (
            RelationshipKind::Controls,
            Some("try".into()),
            Confidence::Confirmed,
        ),
        "catch_clause" => (
            RelationshipKind::Controls,
            Some("catch".into()),
            Confidence::Confirmed,
        ),
        "finally_clause" => (
            RelationshipKind::Controls,
            Some("finally".into()),
            Confidence::Confirmed,
        ),
        "synchronized_statement" => (
            RelationshipKind::Controls,
            Some("synchronized".into()),
            Confidence::Confirmed,
        ),
        "throw_statement" => (
            RelationshipKind::Throws,
            first_named_child(node)
                .map(|expression| candidate_name(node_text(expression, input.source))),
            Confidence::Inferred,
        ),
        _ => return,
    };

    push_relationship(
        output,
        overflowed_relationships,
        AnalyzerRelationship {
            kind: candidate.0,
            source_symbol_id: scope.id.clone(),
            target_symbol_id: None,
            target_name: candidate.1,
            location: source_range(input.relative_path, input.source, line_index, node),
            confidence: candidate.2,
        },
    );
}

fn call_target_name(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "method_invocation" => {
            let name = node.child_by_field_name("name")?;
            let method_name = candidate_name(node_text(name, source));
            let target_name = node
                .child_by_field_name("object")
                .map(|object| {
                    let receiver = candidate_name(node_text(object, source));
                    format!("{receiver}.{method_name}")
                })
                .unwrap_or(method_name);
            Some(candidate_name(&target_name))
        }
        "object_creation_expression" => node
            .child_by_field_name("type")
            .map(|target| candidate_name(node_text(target, source))),
        "explicit_constructor_invocation" => Some(candidate_name(
            node_text(node, source).split('(').next().unwrap_or(""),
        )),
        _ => None,
    }
}

fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

fn candidate_name(value: &str) -> String {
    let mut name = String::with_capacity(128);
    let mut previous_whitespace = false;
    for character in value.chars().take(512) {
        if character.is_whitespace() {
            previous_whitespace = true;
            continue;
        }
        let separator_length = usize::from(previous_whitespace && !name.is_empty());
        if name.len() + separator_length + character.len_utf8() > 128 {
            break;
        }
        if separator_length > 0 {
            name.push(' ');
        }
        name.push(character);
        previous_whitespace = false;
    }
    name
}

fn push_relationship(
    output: &mut AnalysisOutput,
    overflowed: &mut bool,
    relationship: AnalyzerRelationship,
) {
    if output.relationships.len() < MAX_RELATIONSHIPS {
        output.relationships.push(relationship);
    } else {
        *overflowed = true;
    }
}

fn parse_import(
    input: &AnalyzerInput<'_>,
    node: Node<'_>,
    line_index: &LineIndex,
) -> Option<AnalyzerImport> {
    let mut cursor = node.walk();
    let path = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))?;
    let name = node_text(path, input.source).to_owned();
    (!name.is_empty()).then(|| AnalyzerImport {
        name,
        alias: None,
        is_static: has_direct_token(node, "static"),
        is_wildcard: has_direct_token(node, "asterisk"),
        location: source_range(input.relative_path, input.source, line_index, node),
    })
}

fn path_child(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))
        .map(|child| node_text(child, source).to_owned())
}

fn is_record_component(node: Node<'_>) -> bool {
    let Some(parameters) = node.parent() else {
        return false;
    };
    let Some(record) = parameters.parent() else {
        return false;
    };
    parameters.kind() == "formal_parameters"
        && record.kind() == "record_declaration"
        && record
            .child_by_field_name("parameters")
            .is_some_and(|record_parameters| {
                record_parameters.start_byte() == parameters.start_byte()
            })
}

fn identifier_field(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|identifier| node_text(identifier, source).to_owned())
}

fn has_direct_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == token)
}

fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Record
    )
}

fn find_child_by_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
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

fn symbol_id(kind: SymbolKind, start_byte: usize) -> SymbolId {
    SymbolId(format!("java:{kind:?}:{start_byte}"))
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
        JavaAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "src/main/java/example/Example.java",
                source,
            })
            .unwrap()
    }

    fn has_symbol(output: &AnalysisOutput, name: &str, kind: SymbolKind) -> bool {
        output
            .symbols
            .iter()
            .any(|symbol| symbol.name == name && symbol.kind == kind)
    }

    #[test]
    fn extracts_package_imports_types_members_and_relationships() {
        let output = analyze(
            "package sample.app;\n\
             import java.util.List;\n\
             import static java.util.Collections.*;\n\
             @ServiceMarker\n\
             public class Worker extends BaseWorker implements Runnable, AutoCloseable {\n\
                 private final List<String> names = List.of();\n\
                 private int first, second;\n\
                 @Override public void run() {}\n\
                 public Worker() {}\n\
             }\n\
             interface Named extends Runnable {}\n\
             enum Status implements Runnable { READY; public void run() {} }\n\
             record Entry(String name, int count) implements Serializable {\n\
                 Entry { if (count < 0) throw new IllegalArgumentException(); }\n\
             }",
        );

        assert_eq!(output.completeness, AnalysisCompleteness::Complete);
        assert!(has_symbol(&output, "sample.app", SymbolKind::Package));
        assert_eq!(output.imports.len(), 2);
        assert_eq!(output.imports[0].name, "java.util.List");
        assert!(!output.imports[0].is_static);
        assert!(output.imports[1].is_static);
        assert!(output.imports[1].is_wildcard);
        assert!(has_symbol(&output, "Worker", SymbolKind::Class));
        assert!(has_symbol(&output, "Named", SymbolKind::Interface));
        assert!(has_symbol(&output, "Status", SymbolKind::Enum));
        assert!(has_symbol(&output, "Entry", SymbolKind::Record));
        assert!(has_symbol(&output, "names", SymbolKind::Field));
        assert!(has_symbol(&output, "first", SymbolKind::Field));
        assert!(has_symbol(&output, "second", SymbolKind::Field));
        assert!(has_symbol(&output, "READY", SymbolKind::Field));
        assert!(has_symbol(&output, "name", SymbolKind::Field));
        assert!(has_symbol(&output, "run", SymbolKind::Method));
        assert!(has_symbol(&output, "Worker", SymbolKind::Constructor));
        assert!(has_symbol(&output, "Entry", SymbolKind::Constructor));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::Extends
                && relationship.target_name.as_deref() == Some("BaseWorker")
        }));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::Implements
                && relationship.target_name.as_deref() == Some("Runnable")
        }));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::Extends
                && relationship.target_name.as_deref() == Some("Runnable")
        }));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::AnnotatedBy
                && relationship.target_name.as_deref() == Some("ServiceMarker")
        }));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::AnnotatedBy
                && relationship.target_name.as_deref() == Some("Override")
        }));
    }

    #[test]
    fn extracts_call_control_and_exception_candidates_within_scope() {
        let output = analyze(
            "class Worker {\n\
             Widget current = create();\n\
             void run(boolean flag) {\n\
                 // fakeCall()\n\
                 String text = \"fakeCall()\";\n\
                 helper();\n\
                 client.fetch(\"item\");\n\
                 new Widget();\n\
                 if (flag) helper();\n\
                 switch (state) { case READY: helper(); break; default: break; }\n\
                 for (int index = 0; index < 1; index++) helper();\n\
                 for (String item : items) helper();\n\
                 while (flag) helper();\n\
                 do { helper(); } while (flag);\n\
                 try { throw new IllegalStateException(); }\n\
                 catch (RuntimeException error) { recover(error); }\n\
                 finally { cleanup(); }\n\
                 synchronized (this) { helper(); }\n\
             }\n\
             }",
        );
        let run_id = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "run")
            .expect("method symbol")
            .id
            .clone();

        let calls: Vec<_> = output
            .relationships
            .iter()
            .filter(|relationship| relationship.kind == RelationshipKind::Calls)
            .collect();
        for target in ["helper", "client.fetch", "Widget", "IllegalStateException"] {
            assert!(
                calls.iter().any(|call| {
                    call.target_name
                        .as_deref()
                        .is_some_and(|name| name.starts_with(target))
                }),
                "missing call candidate {target}; got {:?}",
                calls
                    .iter()
                    .map(|call| call.target_name.as_deref())
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            !calls
                .iter()
                .any(|call| { matches!(call.target_name.as_deref(), Some("fakeCall" | "create")) })
        );
        assert!(calls.iter().all(|call| call.source_symbol_id == run_id));
        assert!(calls.iter().all(|call| call.target_symbol_id.is_none()));
        assert!(
            calls
                .iter()
                .all(|call| call.confidence == Confidence::Inferred)
        );

        let controls: Vec<_> = output
            .relationships
            .iter()
            .filter(|relationship| relationship.kind == RelationshipKind::Controls)
            .collect();
        for candidate in [
            "if",
            "switch",
            "for",
            "enhanced-for",
            "while",
            "do-while",
            "try",
            "catch",
            "finally",
            "synchronized",
        ] {
            assert!(
                controls
                    .iter()
                    .any(|edge| edge.target_name.as_deref() == Some(candidate)),
                "missing control candidate {candidate}"
            );
        }
        assert!(controls.iter().all(|edge| edge.source_symbol_id == run_id));
        assert!(output.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::Throws
                && relationship.source_symbol_id == run_id
                && relationship
                    .target_name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("new IllegalStateException"))
        }));
        assert_eq!(output.completeness, AnalysisCompleteness::Complete);
        let control_flow = JavaAnalyzer
            .descriptor()
            .capabilities
            .into_iter()
            .find(|capability| capability.feature == AnalysisFeature::ControlFlow)
            .expect("control-flow capability");
        assert_eq!(control_flow.support, SupportLevel::Partial);
    }

    #[test]
    fn reports_partial_parse_and_unsupported_module_declarations() {
        let output =
            analyze("module example.app { requires java.base; }\nclass Broken { void method( { }");
        assert_eq!(output.completeness, AnalysisCompleteness::Partial);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == DiagnosticCode::UnsupportedSyntax })
        );
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::ParseError)
        );
    }

    #[test]
    fn returns_partial_symbols_around_malformed_declarations() {
        let output = analyze("class First {} class Broken { void run( } class Last {}");
        assert_eq!(output.completeness, AnalysisCompleteness::Partial);
        assert!(has_symbol(&output, "First", SymbolKind::Class));
        assert!(has_symbol(&output, "Last", SymbolKind::Class));
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::ParseError)
        );
    }

    #[test]
    fn ignores_declaration_like_text_in_comments_and_strings() {
        let output = analyze(
            "// class Fake {}\nclass Real { String text = \"class Hidden {}\"; /* interface Fake {} */ }",
        );
        assert!(has_symbol(&output, "Real", SymbolKind::Class));
        assert!(!has_symbol(&output, "Fake", SymbolKind::Class));
        assert!(!has_symbol(&output, "Hidden", SymbolKind::Class));
    }

    #[test]
    fn reports_unicode_source_columns_and_rejects_wrong_extension() {
        let output = analyze("// 한글\nclass Worker {}");
        let worker = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Worker")
            .unwrap();
        assert_eq!(worker.location.start_line, 2);
        assert_eq!(worker.location.start_column, 1);

        let error = JavaAnalyzer
            .analyze(AnalyzerInput {
                relative_path: "src/Example.kt",
                source: "class Example {}",
            })
            .unwrap_err();
        assert_eq!(error.code, AnalyzerErrorCode::InvalidInput);
    }

    #[test]
    fn registry_exposes_the_java_analyzer() {
        let registry = crate::analyzer::AnalyzerRegistry::with_java();
        assert!(registry.descriptor(SourceLanguage::Java).is_some());
        let output = registry
            .analyze(
                SourceLanguage::Java,
                AnalyzerInput {
                    relative_path: "Example.java",
                    source: "class Example {}",
                },
            )
            .unwrap();
        assert!(has_symbol(&output, "Example", SymbolKind::Class));
    }
}
