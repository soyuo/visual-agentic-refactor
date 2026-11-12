use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::analyzer::{
    AnalysisCompleteness, Analyzer, AnalyzerDiagnostic, AnalyzerInput, Confidence,
    DiagnosticSeverity, RelationshipKind, SourceLanguage, SourceRange, SymbolKind,
};
use crate::java::JavaAnalyzer;
use crate::knowledge_graph::{
    GraphCompleteness, GraphDiagnostic, GraphDiagnosticSeverity, GraphEdge, GraphEdgeType,
    GraphNode, GraphNodeType, GraphSnapshotInput, stable_node_id,
};
use crate::kotlin::KotlinAnalyzer;

const ANALYZER_VERSION: &str = "spring-static-v2";
const MAX_SOURCE_FILES: usize = 10_000;
const MAX_SOURCE_BYTES: usize = 5 * 1024 * 1024;

pub struct SpringSourceFile<'a> {
    pub relative_path: &'a str,
    pub source: &'a str,
    pub language: SourceLanguage,
}

#[derive(Clone)]
struct SpringAnnotation {
    name: String,
    arguments: String,
    evidence: SourceRange,
}

struct SpringSymbolFact<'a> {
    key: String,
    node_id: String,
    parent_key: Option<String>,
    name: String,
    kind: SymbolKind,
    path: &'a str,
    source: &'a str,
    language: SourceLanguage,
    location: SourceRange,
    annotations: Vec<SpringAnnotation>,
    base_types: Vec<String>,
    calls: Vec<(String, SourceRange)>,
}

/// Analyzes Kotlin and Java source facts relevant to Spring without loading a Spring runtime.
pub fn analyze_spring_sources(
    snapshot_id: &str,
    files: &[SpringSourceFile<'_>],
) -> Result<GraphSnapshotInput, String> {
    if snapshot_id.trim().is_empty() {
        return Err("Snapshot ID must not be empty".to_owned());
    }

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut diagnostics = Vec::new();
    let mut source_hashes = BTreeMap::new();
    let mut completeness = GraphCompleteness::Complete;
    let mut seen_paths = HashSet::new();
    let mut spring_symbols = Vec::new();
    let mut pending_injections = Vec::new();

    for file in files.iter().take(MAX_SOURCE_FILES) {
        if !seen_paths.insert(file.relative_path) {
            diagnostics.push(diagnostic(
                "duplicate_source_path",
                GraphDiagnosticSeverity::Warning,
                format!("Duplicate source path was skipped: {}", file.relative_path),
                None,
            ));
            completeness = GraphCompleteness::Partial;
            continue;
        }
        if file.source.len() > MAX_SOURCE_BYTES {
            diagnostics.push(diagnostic(
                "source_file_limit",
                GraphDiagnosticSeverity::Warning,
                format!(
                    "Source file exceeds the 5 MiB analysis limit: {}",
                    file.relative_path
                ),
                None,
            ));
            completeness = GraphCompleteness::Partial;
            continue;
        }

        source_hashes.insert(
            file.relative_path.to_owned(),
            content_hash(file.source.as_bytes()),
        );
        let output = analyzer(file.language)
            .analyze(AnalyzerInput {
                relative_path: file.relative_path,
                source: file.source,
            })
            .map_err(|error| error.message)?;
        if output.language != file.language {
            return Err(format!(
                "Analyzer returned a language mismatch for {}",
                file.relative_path
            ));
        }

        let file_id = stable_node_id(GraphNodeType::File, file.relative_path)?;
        nodes.push(GraphNode {
            node_id: file_id.clone(),
            node_type: GraphNodeType::File,
            name: file
                .relative_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(file.relative_path)
                .to_owned(),
            qualified_name: None,
            file_path: Some(file.relative_path.to_owned()),
            source_evidence: None,
            language: Some(file.language),
            metadata: Value::Object(Map::new()),
            confidence: Confidence::Confirmed,
        });

        let mut symbol_ids = HashMap::new();
        let mut symbol_nodes = HashMap::new();
        for symbol in &output.symbols {
            let node_type = graph_node_type(symbol.kind);
            let key = format!(
                "{}:{}:{}:{}",
                file.relative_path, symbol.location.start_byte, symbol.kind as u8, symbol.name
            );
            let node_id = stable_node_id(node_type, &key)?;
            let graph_node = GraphNode {
                node_id: node_id.clone(),
                node_type,
                name: symbol.name.clone(),
                qualified_name: None,
                file_path: Some(file.relative_path.to_owned()),
                source_evidence: Some(symbol.location.clone()),
                language: Some(file.language),
                metadata: json!({"symbolKind": format!("{:?}", symbol.kind)}),
                confidence: symbol.confidence,
            };
            symbol_ids.insert(symbol.id.0.clone(), node_id);
            symbol_nodes.insert(symbol.id.0.clone(), graph_node.clone());
            nodes.push(graph_node);
            edges.push(make_edge(
                &file_id,
                GraphEdgeType::Declares,
                Some(symbol_node_id(&symbol_nodes, &symbol.id.0).ok_or_else(|| {
                    "Could not create a graph node for a source symbol".to_owned()
                })?),
                Some(symbol.name.clone()),
                symbol.location.clone(),
                Confidence::Confirmed,
            )?);
        }

        let role_names = [
            ("RestController", "restController"),
            ("Controller", "controller"),
            ("Service", "service"),
            ("Repository", "repository"),
            ("Component", "component"),
            ("Configuration", "configuration"),
        ];
        for relationship in &output.relationships {
            let Some(source_id) = symbol_ids.get(&relationship.source_symbol_id.0) else {
                continue;
            };
            let target_name = relationship.target_name.as_deref().unwrap_or_default();
            let short_name = short_annotation_name(target_name);
            let evidence = relationship.location.clone();
            if relationship.kind == RelationshipKind::AnnotatedBy {
                if let Some(node) = nodes.iter_mut().find(|node| node.node_id == *source_id) {
                    if matches!(
                        node.node_type,
                        GraphNodeType::Class | GraphNodeType::Interface
                    ) {
                        if let Some((_, role)) =
                            role_names.iter().find(|(name, _)| *name == short_name)
                        {
                            let metadata = node
                                .metadata
                                .as_object_mut()
                                .expect("metadata is an object");
                            let roles = metadata.entry("springRoles").or_insert_with(|| json!([]));
                            if let Some(roles) = roles.as_array_mut() {
                                if !roles.iter().any(|value| value == role) {
                                    roles.push(Value::String((*role).to_owned()));
                                }
                            }
                        }
                    }
                    if short_name == "Transactional" {
                        let attributes = annotation_attributes(file.source, &evidence);
                        node.metadata
                            .as_object_mut()
                            .expect("metadata is an object")
                            .insert("transactional".to_owned(), attributes);
                    }
                }
            }

            if let Some(edge_type) = graph_edge_type(relationship.kind) {
                let target_node_id = relationship
                    .target_symbol_id
                    .as_ref()
                    .and_then(|id| symbol_ids.get(&id.0).cloned());
                edges.push(make_edge(
                    source_id,
                    edge_type,
                    target_node_id,
                    relationship.target_name.clone(),
                    evidence,
                    relationship.confidence,
                )?);
            }
        }

        let symbols_by_id: HashMap<&str, _> = output
            .symbols
            .iter()
            .map(|symbol| (symbol.id.0.as_str(), symbol))
            .collect();
        let explicit_injection: HashSet<String> = output
            .relationships
            .iter()
            .filter(|relationship| {
                relationship.kind == RelationshipKind::AnnotatedBy
                    && matches!(
                        short_annotation_name(
                            relationship.target_name.as_deref().unwrap_or_default()
                        ),
                        "Autowired" | "Inject" | "Resource"
                    )
            })
            .map(|relationship| relationship.source_symbol_id.0.clone())
            .collect();
        let bean_classes: HashSet<String> = symbol_ids
            .iter()
            .filter_map(|(symbol_id, node_id)| {
                nodes
                    .iter()
                    .find(|node| node.node_id == *node_id)
                    .and_then(|node| node.metadata.get("springRoles"))
                    .and_then(Value::as_array)
                    .filter(|roles| !roles.is_empty())
                    .map(|_| symbol_id.clone())
            })
            .collect();
        let mut constructors_by_class: HashMap<String, Vec<&crate::analyzer::AnalyzerSymbol>> =
            HashMap::new();
        for symbol in &output.symbols {
            if symbol.kind == SymbolKind::Constructor {
                if let Some(parent_id) = &symbol.parent_id {
                    constructors_by_class
                        .entry(parent_id.0.clone())
                        .or_default()
                        .push(symbol);
                }
            }
        }
        for class_id in &bean_classes {
            if let (Some(class_node_id), Some(constructors)) = (
                symbol_ids.get(class_id),
                constructors_by_class.get(class_id),
            ) {
                if constructors.len() == 1 {
                    let constructor = constructors[0];
                    let explicitly_annotated = explicit_injection.contains(&constructor.id.0);
                    let confidence = if explicitly_annotated {
                        Confidence::Confirmed
                    } else {
                        Confidence::Inferred
                    };
                    for dependency in parameter_types(
                        file.source,
                        &constructor.location,
                        file.language,
                        &constructor.name,
                    ) {
                        pending_injections.push((
                            class_node_id.clone(),
                            dependency,
                            constructor.location.clone(),
                            confidence,
                        ));
                    }
                }
            }
        }

        for symbol_id in explicit_injection {
            let Some(symbol) = symbols_by_id.get(symbol_id.as_str()) else {
                continue;
            };
            if symbol.kind == SymbolKind::Constructor {
                continue;
            }
            let Some(parent) = symbol
                .parent_id
                .as_ref()
                .and_then(|id| symbol_ids.get(&id.0))
            else {
                continue;
            };
            let dependencies = if matches!(symbol.kind, SymbolKind::Method | SymbolKind::Function) {
                parameter_types(file.source, &symbol.location, file.language, &symbol.name)
            } else {
                declared_type(file.source, symbol, file.language)
                    .into_iter()
                    .collect()
            };
            for dependency in dependencies {
                pending_injections.push((
                    parent.clone(),
                    dependency,
                    symbol.location.clone(),
                    Confidence::Confirmed,
                ));
            }
        }

        let mut annotations_by_symbol: HashMap<String, Vec<SpringAnnotation>> = HashMap::new();
        let mut bases_by_symbol: HashMap<String, Vec<String>> = HashMap::new();
        let mut calls_by_symbol: HashMap<String, Vec<(String, SourceRange)>> = HashMap::new();
        for relationship in &output.relationships {
            match relationship.kind {
                RelationshipKind::AnnotatedBy => {
                    annotations_by_symbol
                        .entry(relationship.source_symbol_id.0.clone())
                        .or_default()
                        .push(SpringAnnotation {
                            name: relationship.target_name.clone().unwrap_or_default(),
                            arguments: annotation_arguments(file.source, &relationship.location),
                            evidence: relationship.location.clone(),
                        });
                }
                RelationshipKind::Extends | RelationshipKind::Inherits => {
                    if let Some(target_name) = relationship.target_name.as_ref() {
                        bases_by_symbol
                            .entry(relationship.source_symbol_id.0.clone())
                            .or_default()
                            .push(target_name.clone());
                    }
                }
                RelationshipKind::Calls => {
                    if let Some(target_name) = relationship.target_name.as_ref() {
                        calls_by_symbol
                            .entry(relationship.source_symbol_id.0.clone())
                            .or_default()
                            .push((target_name.clone(), relationship.location.clone()));
                    }
                }
                _ => {}
            }
        }
        for symbol in &output.symbols {
            let Some(node_id) = symbol_ids.get(&symbol.id.0) else {
                continue;
            };
            spring_symbols.push(SpringSymbolFact {
                key: format!("{}::{}", file.relative_path, symbol.id.0),
                node_id: node_id.clone(),
                parent_key: symbol
                    .parent_id
                    .as_ref()
                    .map(|parent| format!("{}::{}", file.relative_path, parent.0)),
                name: symbol.name.clone(),
                kind: symbol.kind,
                path: file.relative_path,
                source: file.source,
                language: file.language,
                location: symbol.location.clone(),
                annotations: annotations_by_symbol
                    .remove(&symbol.id.0)
                    .unwrap_or_default(),
                base_types: bases_by_symbol.remove(&symbol.id.0).unwrap_or_default(),
                calls: calls_by_symbol.remove(&symbol.id.0).unwrap_or_default(),
            });
        }

        for item in output.diagnostics {
            diagnostics.push(analyzer_diagnostic(item));
        }
        if output.completeness == AnalysisCompleteness::Partial {
            completeness = GraphCompleteness::Partial;
        }
    }

    if files.len() > MAX_SOURCE_FILES {
        diagnostics.push(diagnostic(
            "source_file_limit",
            GraphDiagnosticSeverity::Warning,
            format!("Only the first {MAX_SOURCE_FILES} source files were analyzed."),
            None,
        ));
        completeness = GraphCompleteness::Partial;
    }

    let mut project_types: HashMap<String, Vec<String>> = HashMap::new();
    for symbol in &spring_symbols {
        if matches!(
            symbol.kind,
            SymbolKind::Class
                | SymbolKind::Interface
                | SymbolKind::Enum
                | SymbolKind::Record
                | SymbolKind::Object
        ) {
            project_types
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.node_id.clone());
        }
    }
    let mut emitted_dependencies = HashSet::new();
    for (owner_node_id, dependency, evidence, confidence) in pending_injections {
        push_injection_edge(
            &mut edges,
            &mut emitted_dependencies,
            &owner_node_id,
            &dependency,
            &evidence,
            &project_types,
            confidence,
        )?;
    }

    build_api_endpoints(
        &spring_symbols,
        &mut nodes,
        &mut edges,
        &mut diagnostics,
        &mut completeness,
    )?;
    build_ktor_endpoints(
        &files[..files.len().min(MAX_SOURCE_FILES)],
        &mut nodes,
        &mut edges,
    )?;

    Ok(GraphSnapshotInput {
        snapshot_id: snapshot_id.to_owned(),
        head_sha: None,
        analyzer_version: ANALYZER_VERSION.to_owned(),
        source_hashes,
        settings: json!({"framework": "spring", "languages": ["kotlin", "java"]}),
        completeness,
        analyzed_at_unix_ms: Some(current_time_unix_ms()?),
        nodes,
        edges,
        diagnostics,
    })
}

fn build_ktor_endpoints(
    files: &[SpringSourceFile<'_>],
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
) -> Result<(), String> {
    for file in files {
        if file.language != SourceLanguage::Kotlin || !file.source.contains("io.ktor") {
            continue;
        }
        let file_id = stable_node_id(GraphNodeType::File, file.relative_path)?;
        for route in ktor_routes(file.source) {
            let evidence = byte_range(file.relative_path, file.source, route.start, route.end);
            let identity = format!("ktor:{}:{}:{}", file.relative_path, route.method, route.path);
            let endpoint_id = stable_node_id(GraphNodeType::ApiEndpoint, &identity)?;
            if nodes.iter().any(|node| node.node_id == endpoint_id) {
                continue;
            }
            let label = format!("{} {}", route.method, route.path);
            nodes.push(GraphNode {
                node_id: endpoint_id.clone(),
                node_type: GraphNodeType::ApiEndpoint,
                name: label.clone(),
                qualified_name: Some(format!("{}:{}", file.relative_path, evidence.start_line)),
                file_path: Some(file.relative_path.to_owned()),
                source_evidence: Some(evidence.clone()),
                language: Some(SourceLanguage::Kotlin),
                metadata: json!({"httpMethod": route.method, "path": route.path, "framework": "ktor"}),
                confidence: Confidence::Inferred,
            });
            edges.push(make_edge(
                &file_id,
                GraphEdgeType::Exposes,
                Some(endpoint_id),
                Some(label),
                evidence,
                Confidence::Inferred,
            )?);
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct KtorRoute {
    method: &'static str,
    path: String,
    start: usize,
    end: usize,
}

/// Finds Ktor routing DSL calls (`get("/x") {`, bare `get {`) and resolves the
/// path against enclosing `route("/prefix") { ... }` blocks by brace depth.
/// Strings and comments are skipped so braces inside them do not shift scopes.
fn ktor_routes(source: &str) -> Vec<KtorRoute> {
    const METHODS: [(&str, &str); 7] = [
        ("get", "GET"),
        ("post", "POST"),
        ("put", "PUT"),
        ("patch", "PATCH"),
        ("delete", "DELETE"),
        ("head", "HEAD"),
        ("options", "OPTIONS"),
    ];
    let bytes = source.as_bytes();
    let mut routes = Vec::new();
    let mut depth = 0usize;
    let mut scopes: Vec<(usize, String)> = Vec::new();
    let mut pending_prefix: Option<String> = None;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = source[index + 2..].find("*/").map_or(bytes.len(), |end| index + 2 + end + 2);
            continue;
        }
        if byte == b'"' {
            index = skip_string(bytes, index);
            continue;
        }
        if byte == b'\'' {
            index = skip_quoted(bytes, index, b'\'');
            continue;
        }
        if byte == b'{' {
            depth += 1;
            if let Some(prefix) = pending_prefix.take() {
                scopes.push((depth, prefix));
            }
            index += 1;
            continue;
        }
        if byte == b'}' {
            while scopes.last().is_some_and(|(scope_depth, _)| *scope_depth == depth) {
                scopes.pop();
            }
            depth = depth.saturating_sub(1);
            index += 1;
            continue;
        }
        let is_word_start = byte.is_ascii_alphabetic()
            && (index == 0 || !is_ident_byte(bytes[index - 1]) && bytes[index - 1] != b'.');
        if !is_word_start {
            index += 1;
            continue;
        }
        let word_end = (index..bytes.len())
            .find(|&position| !is_ident_byte(bytes[position]))
            .unwrap_or(bytes.len());
        let word = &source[index..word_end];
        let method = METHODS.iter().find(|(name, _)| *name == word).map(|(_, method)| *method);
        if method.is_some() || word == "route" {
            if let Some((argument, brace)) = ktor_call(source, word_end) {
                let prefix = scopes.last().map(|(_, prefix)| prefix.as_str()).unwrap_or("");
                let path = join_route_paths(prefix, argument.unwrap_or(""));
                match method {
                    Some(method) => {
                        routes.push(KtorRoute { method, path, start: index, end: brace });
                    }
                    None if argument.is_some() => pending_prefix = Some(path),
                    None => {}
                }
            }
        }
        index = word_end;
    }
    routes
}

/// Parses `("literal") {` or ` {` after a routing call name. Returns the literal
/// (if any) and the byte offset of the opening brace.
fn ktor_call(source: &str, after_name: usize) -> Option<(Option<&str>, usize)> {
    let bytes = source.as_bytes();
    let skip_ws = |mut position: usize| {
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        position
    };
    let mut position = skip_ws(after_name);
    let mut argument = None;
    if bytes.get(position) == Some(&b'(') {
        position = skip_ws(position + 1);
        if bytes.get(position) != Some(&b'"') {
            return None;
        }
        let literal_end = source[position + 1..].find('"')? + position + 1;
        let literal = &source[position + 1..literal_end];
        if literal.contains('$') || literal.contains('\n') {
            return None;
        }
        position = skip_ws(literal_end + 1);
        if bytes.get(position) != Some(&b')') {
            return None;
        }
        position = skip_ws(position + 1);
        argument = Some(literal);
    }
    (bytes.get(position) == Some(&b'{')).then_some((argument, position))
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_string(bytes: &[u8], start: usize) -> usize {
    if bytes[start..].starts_with(b"\"\"\"") {
        let mut position = start + 3;
        while position + 2 < bytes.len() {
            if &bytes[position..position + 3] == b"\"\"\"" {
                return position + 3;
            }
            position += 1;
        }
        return bytes.len();
    }
    skip_quoted(bytes, start, b'"')
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut position = start + 1;
    while position < bytes.len() {
        match bytes[position] {
            b'\\' => position += 2,
            b'\n' => return position,
            byte if byte == quote => return position + 1,
            _ => position += 1,
        }
    }
    bytes.len()
}

fn byte_range(relative_path: &str, source: &str, start: usize, end: usize) -> SourceRange {
    let position = |offset: usize| {
        let before = &source[..offset];
        let line = before.matches('\n').count() as u32 + 1;
        let column = (offset - before.rfind('\n').map_or(0, |newline| newline + 1)) as u32 + 1;
        (line, column)
    };
    let (start_line, start_column) = position(start);
    let (end_line, end_column) = position(end);
    SourceRange {
        relative_path: relative_path.to_owned(),
        start_byte: start,
        end_byte: end,
        start_line,
        start_column,
        end_line,
        end_column,
    }
}

fn analyzer(language: SourceLanguage) -> &'static dyn Analyzer {
    static JAVA: JavaAnalyzer = JavaAnalyzer;
    static KOTLIN: KotlinAnalyzer = KotlinAnalyzer;
    match language {
        SourceLanguage::Java => &JAVA,
        SourceLanguage::Kotlin => &KOTLIN,
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Python => &JAVA,
    }
}

struct MappingSpec {
    paths: Vec<Option<String>>,
    methods: Vec<String>,
    methods_unknown: bool,
}

fn build_api_endpoints(
    symbols: &[SpringSymbolFact<'_>],
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    diagnostics: &mut Vec<GraphDiagnostic>,
    completeness: &mut GraphCompleteness,
) -> Result<(), String> {
    let mut types_by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        if is_type_symbol(symbol.kind) {
            types_by_name
                .entry(symbol.name.clone())
                .or_default()
                .push(index);
        }
    }

    for (controller_index, controller) in symbols.iter().enumerate() {
        if !is_controller(controller) {
            continue;
        }
        let ancestry = class_ancestry(controller_index, symbols, &types_by_name);
        let class_paths = ancestry
            .iter()
            .fold(vec![Some(String::new())], |prefixes, index| {
                let specs: Vec<_> = symbols[*index]
                    .annotations
                    .iter()
                    .filter_map(mapping_spec)
                    .collect();
                if specs.is_empty() {
                    return prefixes;
                }
                let paths: Vec<_> = specs
                    .iter()
                    .flat_map(|spec| spec.paths.iter().cloned())
                    .collect();
                cartesian_paths(&prefixes, &paths)
            });
        let class_specs: Vec<_> = ancestry
            .iter()
            .flat_map(|index| symbols[*index].annotations.iter().filter_map(mapping_spec))
            .collect::<Vec<_>>();
        let class_methods = class_specs
            .iter()
            .flat_map(|spec| spec.methods.iter().cloned())
            .collect::<Vec<_>>();

        let ancestor_keys: HashSet<&str> = ancestry
            .iter()
            .map(|index| symbols[*index].key.as_str())
            .collect();
        let direct_method_names: HashSet<&str> = symbols
            .iter()
            .filter(|symbol| symbol.parent_key.as_deref() == Some(controller.key.as_str()))
            .filter(|symbol| is_method_symbol(symbol.kind))
            .map(|symbol| symbol.name.as_str())
            .collect();

        for handler in symbols.iter().filter(|symbol| {
            is_method_symbol(symbol.kind)
                && symbol
                    .parent_key
                    .as_deref()
                    .is_some_and(|parent| ancestor_keys.contains(parent))
        }) {
            if handler.parent_key.as_deref() != Some(controller.key.as_str())
                && direct_method_names.contains(handler.name.as_str())
            {
                continue;
            }
            let method_specs: Vec<_> = handler
                .annotations
                .iter()
                .filter_map(mapping_spec)
                .collect();
            if method_specs.is_empty() {
                continue;
            }

            let method_paths: Vec<_> = method_specs
                .iter()
                .flat_map(|spec| spec.paths.iter().cloned())
                .collect();
            let paths = cartesian_paths(&class_paths, &method_paths);
            let evidence = handler
                .annotations
                .iter()
                .find(|annotation| mapping_spec(annotation).is_some())
                .map(|annotation| annotation.evidence.clone())
                .unwrap_or_else(|| handler.location.clone());
            let method_constraints: Vec<_> = method_specs
                .iter()
                .flat_map(|spec| spec.methods.iter().cloned())
                .collect();
            let methods_unknown = class_specs.iter().any(|spec| spec.methods_unknown)
                || method_specs.iter().any(|spec| spec.methods_unknown);
            if !class_methods.is_empty()
                && !method_constraints.is_empty()
                && class_methods
                    .iter()
                    .all(|method| !method_constraints.contains(method))
            {
                *completeness = GraphCompleteness::Partial;
                diagnostics.push(diagnostic(
                    "mapping_method_conflict",
                    GraphDiagnosticSeverity::Warning,
                    format!(
                        "Class and method HTTP method constraints do not intersect for {}.{}.",
                        controller.name, handler.name
                    ),
                    Some(evidence.clone()),
                ));
                continue;
            }
            let http_methods = intersect_methods(&class_methods, &method_constraints);
            let http_methods: Vec<Option<String>> = if http_methods.is_empty() {
                vec![None]
            } else {
                http_methods.into_iter().map(Some).collect()
            };
            let parameters = method_parameters(
                handler.source,
                &handler.location,
                handler.language,
                &handler.name,
            );
            let response_type = method_return_type(
                handler.source,
                &handler.location,
                handler.language,
                &handler.name,
                handler.kind,
            );
            let auth = collect_security_annotations(handler, &ancestry, symbols);
            let transaction =
                collect_annotation_metadata("Transactional", handler, &ancestry, symbols);

            for path in &paths {
                let unresolved_path = path
                    .as_deref()
                    .is_none_or(|path| path.contains("${") || path.contains("#{"));
                if unresolved_path || methods_unknown {
                    *completeness = GraphCompleteness::Partial;
                    diagnostics.push(diagnostic(
                        if unresolved_path {
                            "unresolved_route_expression"
                        } else {
                            "unresolved_route_method"
                        },
                        GraphDiagnosticSeverity::Warning,
                        format!(
                            "A route mapping expression could not be resolved for {}.{}.",
                            controller.name, handler.name
                        ),
                        Some(evidence.clone()),
                    ));
                }
                for http_method in &http_methods {
                    let path_label = path.as_deref().unwrap_or("<unknown>");
                    let method_label = http_method.as_deref().unwrap_or("ANY");
                    let identity = format!(
                        "{}:{}:{}:{}:{}",
                        controller.path, controller.name, handler.path, handler.key, path_label
                    );
                    let endpoint_id = stable_node_id(
                        GraphNodeType::ApiEndpoint,
                        &format!(
                            "spring:{}:{}",
                            content_hash(identity.as_bytes()),
                            method_label
                        ),
                    )?;
                    let confidence = if unresolved_path || methods_unknown {
                        Confidence::Unknown
                    } else if handler.parent_key.as_deref() != Some(controller.key.as_str()) {
                        Confidence::Inferred
                    } else {
                        Confidence::Confirmed
                    };
                    nodes.push(GraphNode {
                        node_id: endpoint_id.clone(),
                        node_type: GraphNodeType::ApiEndpoint,
                        name: format!("{method_label} {path_label}"),
                        qualified_name: Some(format!("{}.{}", controller.name, handler.name)),
                        file_path: Some(handler.path.to_owned()),
                        source_evidence: Some(evidence.clone()),
                        language: Some(handler.language),
                        metadata: json!({
                            "httpMethod": http_method,
                            "path": path,
                            "handler": {
                                "controller": controller.name,
                                "method": handler.name,
                                "declaringType": handler.parent_key.as_ref().and_then(|parent| {
                                    symbols.iter().find(|symbol| &symbol.key == parent).map(|symbol| symbol.name.as_str())
                                }),
                                "inherited": handler.parent_key.as_deref() != Some(controller.key.as_str())
                            },
                            "parameters": parameters,
                            "responseType": response_type,
                            "authentication": auth,
                            "transactional": transaction
                        }),
                        confidence,
                    });
                    edges.push(make_edge(
                        &controller.node_id,
                        GraphEdgeType::Exposes,
                        Some(endpoint_id.clone()),
                        Some(format!("{method_label} {path_label}")),
                        evidence.clone(),
                        confidence,
                    )?);

                    for (direction, candidate_type) in
                        dto_candidates(&parameters, response_type.as_ref())
                    {
                        let resolved = resolve_type_fact(&candidate_type, symbols, &types_by_name);
                        let dto_key = format!(
                            "{}:{}:{}:{}",
                            endpoint_id, direction, candidate_type, handler.path
                        );
                        let dto_id = stable_node_id(
                            GraphNodeType::Dto,
                            &format!("spring:{}", content_hash(dto_key.as_bytes())),
                        )?;
                        nodes.push(GraphNode {
                            node_id: dto_id.clone(),
                            node_type: GraphNodeType::Dto,
                            name: simple_type_name(&candidate_type).to_owned(),
                            qualified_name: Some(candidate_type.clone()),
                            file_path: resolved.map(|fact| fact.path.to_owned()),
                            source_evidence: Some(handler.location.clone()),
                            language: Some(handler.language),
                            metadata: json!({
                                "candidateType": candidate_type,
                                "direction": direction,
                                "resolvedSymbolId": resolved.map(|fact| fact.node_id.as_str())
                            }),
                            confidence: if resolved.is_some() {
                                Confidence::Inferred
                            } else {
                                Confidence::Unknown
                            },
                        });
                        edges.push(make_edge(
                            &endpoint_id,
                            GraphEdgeType::UsesDto,
                            Some(dto_id),
                            Some(candidate_type),
                            evidence.clone(),
                            if resolved.is_some() {
                                Confidence::Inferred
                            } else {
                                Confidence::Unknown
                            },
                        )?);
                    }
                }
            }
        }
    }

    detect_dynamic_routes(symbols, diagnostics, completeness);
    Ok(())
}

fn is_type_symbol(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Enum
            | SymbolKind::Record
            | SymbolKind::Object
    )
}

fn is_method_symbol(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Method | SymbolKind::Function)
}

fn is_controller(symbol: &SpringSymbolFact<'_>) -> bool {
    is_type_symbol(symbol.kind)
        && symbol.annotations.iter().any(|annotation| {
            matches!(
                short_annotation_name(&annotation.name),
                "RestController" | "Controller"
            )
        })
}

fn class_ancestry(
    start: usize,
    symbols: &[SpringSymbolFact<'_>],
    types_by_name: &HashMap<String, Vec<usize>>,
) -> Vec<usize> {
    let mut chain = vec![start];
    let mut visited = HashSet::from([start]);
    let mut current = start;
    for _ in 0..16 {
        let base = symbols[current].base_types.iter().find_map(|name| {
            let simple = simple_type_name(name);
            let matches = types_by_name.get(simple)?;
            (matches.len() == 1).then_some(matches[0])
        });
        let Some(base) = base else { break };
        if !visited.insert(base) {
            break;
        }
        chain.push(base);
        current = base;
    }
    chain.reverse();
    chain
}

fn mapping_spec(annotation: &SpringAnnotation) -> Option<MappingSpec> {
    let name = short_annotation_name(&annotation.name);
    let fixed_method = match name {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "PatchMapping" => Some("PATCH"),
        "DeleteMapping" => Some("DELETE"),
        "RequestMapping" => None,
        _ => return None,
    };
    let arguments = annotation_arguments_map(&annotation.arguments);
    let path_argument = arguments.get("path").or_else(|| arguments.get("value"));
    let paths =
        path_argument.map_or_else(|| vec![Some(String::new())], |raw| literal_path_values(raw));
    let mut methods = fixed_method
        .map(|method| vec![method.to_owned()])
        .unwrap_or_default();
    let mut methods_unknown = false;
    if name == "RequestMapping" {
        if let Some(raw) = arguments.get("method") {
            methods.extend(parse_http_methods(raw));
            methods.sort();
            methods.dedup();
            methods_unknown = methods.is_empty();
        }
    }
    Some(MappingSpec {
        paths,
        methods,
        methods_unknown,
    })
}

fn annotation_arguments_map(arguments: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for (index, part) in split_top_level(arguments, ',').iter().enumerate() {
        if let Some((key, value)) = part.split_once('=') {
            values.insert(key.trim().to_owned(), value.trim().to_owned());
        } else if index == 0 && !part.trim().is_empty() {
            values.insert("value".to_owned(), part.trim().to_owned());
        }
    }
    values
}

fn literal_path_values(raw: &str) -> Vec<Option<String>> {
    let mut characters = raw.chars().peekable();
    let mut literals = Vec::new();
    while let Some(character) = characters.next() {
        if character.is_whitespace() || matches!(character, ',' | '{' | '}' | '[' | ']' | '(' | ')')
        {
            continue;
        }
        if !matches!(character, '"' | '\'') {
            return vec![None];
        }

        let quote = character;
        let mut literal = String::new();
        let mut closed = false;
        while let Some(character) = characters.next() {
            if character == '\\' {
                if let Some(escaped) = characters.next() {
                    literal.push(escaped);
                } else {
                    return vec![None];
                }
            } else if character == quote {
                closed = true;
                break;
            } else {
                literal.push(character);
            }
        }
        if !closed {
            return vec![None];
        }
        literals.push(Some(literal));
    }

    if literals.is_empty() {
        if raw.trim().is_empty() {
            vec![Some(String::new())]
        } else {
            vec![None]
        }
    } else {
        literals
    }
}

fn parse_http_methods(raw: &str) -> Vec<String> {
    [
        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
    ]
    .into_iter()
    .filter(|method| {
        raw.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|part| part == *method)
    })
    .map(str::to_owned)
    .collect()
}

fn cartesian_paths(
    prefixes: &[Option<String>],
    suffixes: &[Option<String>],
) -> Vec<Option<String>> {
    let mut paths = Vec::new();
    for prefix in prefixes {
        for suffix in suffixes {
            paths.push(match (prefix, suffix) {
                (Some(prefix), Some(suffix)) => Some(join_route_paths(prefix, suffix)),
                _ => None,
            });
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn join_route_paths(prefix: &str, suffix: &str) -> String {
    let left = prefix.trim_matches('/');
    let right = suffix.trim_matches('/');
    match (left.is_empty(), right.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{right}"),
        (false, true) => format!("/{left}"),
        (false, false) => format!("/{left}/{right}"),
    }
}

fn intersect_methods(class_methods: &[String], method_methods: &[String]) -> Vec<String> {
    let mut methods = match (class_methods.is_empty(), method_methods.is_empty()) {
        (true, true) => Vec::new(),
        (true, false) => method_methods.to_vec(),
        (false, true) => class_methods.to_vec(),
        (false, false) => class_methods
            .iter()
            .filter(|method| method_methods.contains(method))
            .cloned()
            .collect(),
    };
    methods.sort();
    methods.dedup();
    methods
}

fn method_parameter_parens(declaration: &str, method_name: Option<&str>) -> Option<(usize, usize)> {
    let named_open = method_name.and_then(|method_name| {
        declaration
            .match_indices(method_name)
            .find_map(|(start, matched)| {
                let preceding = declaration[..start].chars().next_back();
                if preceding.is_some_and(|character| !character.is_whitespace() && character != '>')
                {
                    return None;
                }
                let after_name = start + matched.len();
                let suffix = declaration.get(after_name..)?;
                let whitespace = suffix.len() - suffix.trim_start().len();
                (suffix[whitespace..].starts_with('(')).then_some(after_name + whitespace)
            })
    });
    let open = named_open.or_else(|| declaration.find('('))?;
    let close = matching_delimiter(declaration, open, '(', ')')?;
    Some((open, close))
}

fn method_parameters(
    source: &str,
    range: &SourceRange,
    language: SourceLanguage,
    method_name: &str,
) -> Vec<Value> {
    let Some(declaration) = source.get(range.start_byte..range.end_byte) else {
        return Vec::new();
    };
    let Some((open, close)) = method_parameter_parens(declaration, Some(method_name)) else {
        return Vec::new();
    };
    split_top_level(&declaration[open + 1..close], ',')
        .into_iter()
        .filter_map(|parameter| parameter_metadata(parameter, language))
        .collect()
}

fn parameter_metadata(parameter: &str, language: SourceLanguage) -> Option<Value> {
    let (name, type_name) = match language {
        SourceLanguage::Kotlin => {
            let (before_colon, after_colon) = parameter.split_once(':')?;
            let name = strip_leading_annotations(before_colon)
                .split_whitespace()
                .last()?
                .trim_start_matches("vararg ")
                .trim_start_matches("val ")
                .trim_start_matches("var ")
                .to_owned();
            let type_name = normalize_type_name(after_colon.split('=').next()?.trim())?;
            (name, type_name)
        }
        SourceLanguage::Java => {
            let stripped = strip_leading_annotations(parameter);
            let parameter = stripped.trim();
            let name_end = parameter.trim_end().len();
            let name_start = parameter[..name_end].rfind(char::is_whitespace)?;
            let name = parameter[name_start..name_end].trim().to_owned();
            let type_name = extract_trailing_type(&parameter[..name_start])?;
            (name, type_name)
        }
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Python => {
            return None;
        }
    };
    let binding = [
        "RequestBody",
        "PathVariable",
        "RequestParam",
        "RequestHeader",
        "CookieValue",
    ]
    .into_iter()
    .find(|annotation| parameter.contains(annotation));
    Some(json!({"name": name, "type": type_name, "binding": binding}))
}

fn method_return_type(
    source: &str,
    range: &SourceRange,
    language: SourceLanguage,
    method_name: &str,
    kind: SymbolKind,
) -> Option<String> {
    if kind == SymbolKind::Constructor {
        return None;
    }
    let declaration = source.get(range.start_byte..range.end_byte)?;
    let (open, close) = method_parameter_parens(declaration, Some(method_name))?;
    match language {
        SourceLanguage::Kotlin => {
            let suffix = declaration.get(close + 1..)?;
            let (_, after_colon) = suffix.split_once(':')?;
            let type_name = after_colon.split(['=', '{', '\n', ';']).next()?.trim();
            normalize_type_name(type_name)
        }
        SourceLanguage::Java => {
            let name_start = declaration[..open].rfind(method_name)?;
            extract_trailing_type(&declaration[..name_start])
        }
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Python => None,
    }
}

fn dto_candidates(
    parameters: &[Value],
    response_type: Option<&String>,
) -> Vec<(&'static str, String)> {
    let mut candidates = parameters
        .iter()
        .filter_map(|parameter| parameter.get("type").and_then(Value::as_str))
        .filter_map(candidate_dto_type)
        .map(|type_name| ("request", type_name))
        .collect::<Vec<_>>();
    if let Some(response_type) = response_type.and_then(|type_name| candidate_dto_type(type_name)) {
        candidates.push(("response", response_type));
    }
    candidates.sort_by(|left, right| left.0.cmp(right.0).then(left.1.cmp(&right.1)));
    candidates.dedup();
    candidates
}

fn candidate_dto_type(type_name: &str) -> Option<String> {
    let mut candidate = type_name.trim();
    if let (Some(open), Some(close)) = (candidate.find('<'), candidate.rfind('>')) {
        if open < close {
            let nested = split_top_level(&candidate[open + 1..close], ',');
            candidate = nested.first()?.trim();
            return candidate_dto_type(candidate);
        }
    }
    let simple = simple_type_name(candidate);
    let framework_or_scalar = matches!(
        simple,
        "void"
            | "Void"
            | "Unit"
            | "String"
            | "CharSequence"
            | "Boolean"
            | "Byte"
            | "Short"
            | "Integer"
            | "Long"
            | "Float"
            | "Double"
            | "BigDecimal"
            | "BigInteger"
            | "ResponseEntity"
            | "HttpEntity"
            | "ModelAndView"
    );
    (!framework_or_scalar && !candidate.starts_with("?")).then(|| candidate.to_owned())
}

fn simple_type_name(type_name: &str) -> &str {
    type_name
        .trim()
        .trim_end_matches('?')
        .split(['<', '['])
        .next()
        .unwrap_or(type_name)
        .trim()
        .rsplit('.')
        .next()
        .unwrap_or(type_name)
}

fn resolve_type_fact<'a>(
    type_name: &str,
    symbols: &'a [SpringSymbolFact<'a>],
    types_by_name: &HashMap<String, Vec<usize>>,
) -> Option<&'a SpringSymbolFact<'a>> {
    let simple = simple_type_name(type_name);
    let matches = types_by_name.get(simple)?;
    (matches.len() == 1).then(|| &symbols[matches[0]])
}

fn collect_security_annotations(
    handler: &SpringSymbolFact<'_>,
    ancestry: &[usize],
    symbols: &[SpringSymbolFact<'_>],
) -> Vec<Value> {
    const SECURITY_ANNOTATIONS: &[&str] = &[
        "PreAuthorize",
        "PostAuthorize",
        "Secured",
        "RolesAllowed",
        "PermitAll",
        "DenyAll",
    ];
    let mut result = Vec::new();
    for symbol in ancestry
        .iter()
        .map(|index| &symbols[*index])
        .chain(std::iter::once(handler))
    {
        for annotation in &symbol.annotations {
            let name = short_annotation_name(&annotation.name);
            if SECURITY_ANNOTATIONS.contains(&name) {
                result.push(json!({"annotation": name, "arguments": annotation.arguments}));
            }
        }
    }
    result
}

fn collect_annotation_metadata(
    expected: &str,
    handler: &SpringSymbolFact<'_>,
    ancestry: &[usize],
    symbols: &[SpringSymbolFact<'_>],
) -> Vec<Value> {
    ancestry
        .iter()
        .map(|index| &symbols[*index])
        .chain(std::iter::once(handler))
        .flat_map(|symbol| symbol.annotations.iter())
        .filter(|annotation| short_annotation_name(&annotation.name) == expected)
        .map(|annotation| json!({"annotation": annotation.name, "arguments": annotation.arguments}))
        .collect()
}

fn detect_dynamic_routes(
    symbols: &[SpringSymbolFact<'_>],
    diagnostics: &mut Vec<GraphDiagnostic>,
    completeness: &mut GraphCompleteness,
) {
    for symbol in symbols {
        for (target, evidence) in &symbol.calls {
            let short_name = target.rsplit(['.', '#']).next().unwrap_or(target);
            let dynamic_registration = short_name == "registerMapping"
                || (short_name == "route" && symbol.source.contains("RouterFunction"));
            if !dynamic_registration {
                continue;
            }
            *completeness = GraphCompleteness::Partial;
            diagnostics.push(diagnostic(
                "dynamic_route_registration_unknown",
                GraphDiagnosticSeverity::Info,
                format!(
                    "Dynamic Spring route registration is present and was not resolved: {}",
                    symbol.path
                ),
                Some(evidence.clone()),
            ));
        }
    }
}

fn graph_node_type(kind: SymbolKind) -> GraphNodeType {
    match kind {
        SymbolKind::Class | SymbolKind::Enum | SymbolKind::Record | SymbolKind::Object => {
            GraphNodeType::Class
        }
        SymbolKind::Interface => GraphNodeType::Interface,
        SymbolKind::Function => GraphNodeType::Function,
        SymbolKind::Method | SymbolKind::Constructor => GraphNodeType::Method,
        _ => GraphNodeType::Symbol,
    }
}

fn graph_edge_type(kind: RelationshipKind) -> Option<GraphEdgeType> {
    Some(match kind {
        RelationshipKind::Calls => GraphEdgeType::Calls,
        RelationshipKind::Implements => GraphEdgeType::Implements,
        RelationshipKind::Extends => GraphEdgeType::Extends,
        RelationshipKind::Inherits => GraphEdgeType::Inherits,
        RelationshipKind::Imports => GraphEdgeType::Imports,
        RelationshipKind::Injects => GraphEdgeType::Injects,
        RelationshipKind::Exposes => GraphEdgeType::Exposes,
        RelationshipKind::UsesDto => GraphEdgeType::UsesDto,
        RelationshipKind::Controls => GraphEdgeType::Controls,
        RelationshipKind::Throws => GraphEdgeType::Throws,
        RelationshipKind::AnnotatedBy => GraphEdgeType::AnnotatedBy,
    })
}

fn symbol_node_id(nodes: &HashMap<String, GraphNode>, symbol_id: &str) -> Option<String> {
    nodes.get(symbol_id).map(|node| node.node_id.clone())
}

fn make_edge(
    source_node_id: &str,
    edge_type: GraphEdgeType,
    target_node_id: Option<String>,
    target_name: Option<String>,
    evidence: SourceRange,
    confidence: Confidence,
) -> Result<GraphEdge, String> {
    let target_key = target_node_id
        .as_deref()
        .or(target_name.as_deref())
        .ok_or_else(|| "A graph edge must identify its target".to_owned())?;
    let edge_id =
        GraphSnapshotInput::stable_edge_id(source_node_id, edge_type, target_key, &evidence)?;
    Ok(GraphEdge {
        edge_id,
        edge_type,
        source_node_id: source_node_id.to_owned(),
        target_node_id,
        target_name,
        evidence,
        confidence,
    })
}

fn short_annotation_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn annotation_attributes(source: &str, range: &SourceRange) -> Value {
    let arguments = annotation_arguments(source, range);
    if arguments.is_empty() {
        Value::Object(Map::new())
    } else {
        json!({"raw": arguments})
    }
}

fn annotation_arguments(source: &str, range: &SourceRange) -> String {
    let Some(annotation) = source.get(range.start_byte..range.end_byte) else {
        return String::new();
    };
    let Some(open) = annotation.find('(') else {
        return String::new();
    };
    let Some(close) = annotation.rfind(')') else {
        return String::new();
    };
    let arguments = annotation[open + 1..close].trim();
    arguments.to_owned()
}

fn parameter_types(
    source: &str,
    range: &SourceRange,
    language: SourceLanguage,
    method_name: &str,
) -> Vec<String> {
    let Some(declaration) = source.get(range.start_byte..range.end_byte) else {
        return Vec::new();
    };
    let Some((open, close)) = method_parameter_parens(declaration, Some(method_name)) else {
        return Vec::new();
    };
    split_top_level(&declaration[open + 1..close], ',')
        .into_iter()
        .filter_map(|parameter| parameter_type(parameter, language))
        .collect()
}

fn parameter_type(parameter: &str, language: SourceLanguage) -> Option<String> {
    match language {
        SourceLanguage::Kotlin => {
            let (_, after_colon) = parameter.split_once(':')?;
            let type_name = after_colon.split('=').next()?.trim();
            normalize_type_name(type_name)
        }
        SourceLanguage::Java => {
            let stripped = strip_leading_annotations(parameter);
            let parameter = stripped.trim();
            let name_end = parameter.trim_end().len();
            let name_start = parameter[..name_end].rfind(char::is_whitespace)?;
            extract_trailing_type(&parameter[..name_start])
        }
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Python => None,
    }
}

fn declared_type(
    source: &str,
    symbol: &crate::analyzer::AnalyzerSymbol,
    language: SourceLanguage,
) -> Option<String> {
    let start = symbol.location.start_byte;
    match language {
        SourceLanguage::Kotlin => {
            let declaration = source.get(symbol.location.start_byte..symbol.location.end_byte)?;
            let (_, after_colon) = declaration.split_once(':')?;
            let type_name = after_colon.split(['=', '\n', ';']).next()?.trim();
            normalize_type_name(type_name)
        }
        SourceLanguage::Java => {
            let prefix = source.get(..start)?;
            let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
            extract_trailing_type(&prefix[line_start..])
        }
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Python => None,
    }
}

fn push_injection_edge(
    edges: &mut Vec<GraphEdge>,
    emitted: &mut HashSet<(String, String)>,
    owner_node_id: &str,
    dependency_type: &str,
    evidence: &SourceRange,
    local_types: &HashMap<String, Vec<String>>,
    confidence: Confidence,
) -> Result<(), String> {
    let dependency_name = resolve_dependency_name(dependency_type, local_types);
    if dependency_name.is_empty()
        || !emitted.insert((owner_node_id.to_owned(), dependency_name.clone()))
    {
        return Ok(());
    }
    let matching_nodes = local_types.get(&dependency_name);
    let target_node_id = matching_nodes
        .filter(|matches| matches.len() == 1)
        .and_then(|matches| matches.first().cloned());
    let confidence = if target_node_id.is_none() {
        Confidence::Unknown
    } else {
        confidence
    };
    // The edge keeps source evidence on the injection member or constructor.
    edges.push(make_edge(
        owner_node_id,
        GraphEdgeType::Injects,
        target_node_id,
        Some(dependency_name),
        evidence.clone(),
        confidence,
    )?);
    Ok(())
}

fn resolve_dependency_name(
    dependency_type: &str,
    local_types: &HashMap<String, Vec<String>>,
) -> String {
    let normalized = dependency_type.trim().trim_end_matches('?');
    if local_types.contains_key(normalized) {
        return normalized.to_owned();
    }
    let simple = normalized.rsplit('.').next().unwrap_or(normalized);
    if local_types.contains_key(simple) {
        return simple.to_owned();
    }
    if let (Some(open), Some(close)) = (normalized.find('<'), normalized.rfind('>')) {
        if open < close {
            for candidate in split_top_level(&normalized[open + 1..close], ',')
                .into_iter()
                .rev()
            {
                let simple_candidate = candidate
                    .trim()
                    .rsplit('.')
                    .next()
                    .unwrap_or(candidate.trim());
                if local_types.contains_key(simple_candidate) {
                    return simple_candidate.to_owned();
                }
            }
        }
    }
    simple.to_owned()
}

fn normalize_type_name(type_name: &str) -> Option<String> {
    let cleaned = type_name
        .trim()
        .trim_start_matches("? extends ")
        .trim_start_matches("? super ")
        .trim_end_matches('?')
        .trim_end_matches("[]")
        .trim_end_matches("...")
        .trim();
    (!cleaned.is_empty() && cleaned != "void").then(|| cleaned.to_owned())
}

fn extract_trailing_type(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let mut end = trimmed.len();
    let mut cursor = end;
    let mut generic_depth = 0_i32;
    while cursor > 0 {
        let ch = trimmed[..cursor].chars().next_back()?;
        let ch_len = ch.len_utf8();
        match ch {
            '>' => generic_depth += 1,
            '<' if generic_depth > 0 => generic_depth -= 1,
            _ => {}
        }
        cursor -= ch_len;
        if generic_depth == 0
            && !matches!(ch, '>' | '<' | '.' | '$' | '_' | '?' | '[' | ']')
            && !ch.is_alphanumeric()
        {
            break;
        }
        if generic_depth == 0 && ch.is_whitespace() {
            end = cursor + ch_len;
            break;
        }
        end = cursor;
    }
    let candidate = trimmed.get(end..trimmed.len())?.trim();
    let candidate = candidate
        .trim_start_matches("final ")
        .trim_start_matches("var ")
        .trim_start_matches("val ")
        .trim();
    normalize_type_name(candidate)
}

fn strip_leading_annotations(parameter: &str) -> String {
    let mut remaining = parameter.trim_start();
    while let Some(after_at) = remaining.strip_prefix('@') {
        let end = after_at.find(char::is_whitespace).unwrap_or(after_at.len());
        remaining = after_at[end..].trim_start();
    }
    remaining.to_owned()
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active_quote {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' | '{' | '<' => stack.push(character),
            ')' | ']' | '}' | '>' => {
                stack.pop();
            }
            _ if character == delimiter && stack.is_empty() => {
                let part = input[start..index].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    let last = input[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

fn matching_delimiter(input: &str, open_at: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    for (offset, character) in input[open_at..].char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth -= 1;
            if depth == 0 {
                return Some(open_at + offset);
            }
        }
    }
    None
}

fn analyzer_diagnostic(item: AnalyzerDiagnostic) -> GraphDiagnostic {
    let severity = match item.severity {
        DiagnosticSeverity::Info => GraphDiagnosticSeverity::Info,
        DiagnosticSeverity::Warning => GraphDiagnosticSeverity::Warning,
        DiagnosticSeverity::Error => GraphDiagnosticSeverity::Error,
    };
    GraphDiagnostic {
        code: format!("analyzer_{:?}", item.code),
        severity,
        message: item.message,
        source_evidence: item.location,
    }
}

fn diagnostic(
    code: &str,
    severity: GraphDiagnosticSeverity,
    message: String,
    source_evidence: Option<SourceRange>,
) -> GraphDiagnostic {
    GraphDiagnostic {
        code: code.to_owned(),
        severity,
        message,
        source_evidence,
    }
}

fn current_time_unix_ms() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock could not be read: {error}"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|error| format!("Current timestamp is out of range: {error}"))
}

fn content_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{SpringSourceFile, analyze_spring_sources};
    use crate::analyzer::{Confidence, SourceLanguage};
    use crate::knowledge_graph::{GraphCompleteness, GraphNodeType};

    #[test]
    fn extracts_spring_roles_and_transaction_metadata_from_java() {
        let source = r#"
package sample;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;
@RestController
class UserController {
    @Transactional(readOnly = true)
    public void update() {}
}
@Service
class UserService {}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-1",
            &[SpringSourceFile {
                relative_path: "src/main/java/sample/User.java",
                source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();

        let controller = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "UserController")
            .unwrap();
        assert_eq!(controller.node_type, GraphNodeType::Class);
        assert_eq!(controller.metadata["springRoles"][0], "restController");
        let update = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "update")
            .unwrap();
        assert_eq!(update.metadata["transactional"]["raw"], "readOnly = true");
        assert!(snapshot.nodes.iter().any(|node| {
            node.name == "UserService" && node.metadata["springRoles"][0] == "service"
        }));
        assert!(snapshot.edges.iter().any(|edge| {
            edge.target_name.as_deref() == Some("RestController")
                && edge.confidence == Confidence::Confirmed
        }));
    }

    #[test]
    fn extracts_spring_roles_from_kotlin_annotations() {
        let source = r#"
package sample
import org.springframework.stereotype.Repository
@Repository
class UserRepository
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-2",
            &[SpringSourceFile {
                relative_path: "src/main/kotlin/sample/UserRepository.kt",
                source,
                language: SourceLanguage::Kotlin,
            }],
        )
        .unwrap();
        let repository = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "UserRepository")
            .unwrap();
        assert_eq!(repository.metadata["springRoles"][0], "repository");
        assert_eq!(snapshot.completeness, GraphCompleteness::Complete);
    }

    #[test]
    fn resolves_nested_ktor_route_prefixes_and_bare_methods() {
        let source = r#"
import io.ktor.server.routing.Route
fun Route.voxRoomRoutes() {
    route("/vox/rooms") {
        get {
            val text = "not a } brace"
            call.respond(mapOf("k" to "v"))
        }
        post { }
        route("/{room-id}") {
            patch { }
            // delete("/ignored") {
            delete { }
        }
    }
    get("/") { }
    put("email", email)
    val x get() = 1
}
"#;
        let routes: Vec<_> = super::ktor_routes(source)
            .into_iter()
            .map(|route| format!("{} {}", route.method, route.path))
            .collect();
        assert_eq!(
            routes,
            [
                "GET /vox/rooms",
                "POST /vox/rooms",
                "PATCH /vox/rooms/{room-id}",
                "DELETE /vox/rooms/{room-id}",
                "GET /",
            ]
        );
    }

    #[test]
    fn extracts_ktor_route_endpoints_from_kotlin_routes() {
        let source = r#"
import io.ktor.server.routing.Route
fun Route.apiTokenRoutes(tokenService: ApiTokenService) {
    post("/token/refresh") { }
}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-ktor",
            &[SpringSourceFile {
                relative_path: "src/main/kotlin/kr/reflua/backend/auth/ApiTokenRoutes.kt",
                source,
                language: SourceLanguage::Kotlin,
            }],
        )
        .unwrap();
        let endpoint = snapshot
            .nodes
            .iter()
            .find(|node| node.node_type == GraphNodeType::ApiEndpoint)
            .expect("Ktor endpoint should be extracted");
        assert_eq!(endpoint.metadata["httpMethod"], "POST");
        assert_eq!(endpoint.metadata["path"], "/token/refresh");
        assert_eq!(endpoint.metadata["framework"], "ktor");
    }

    #[test]
    fn records_explicit_and_single_constructor_injection_with_confidence() {
        let source = r#"
package sample;
import org.springframework.stereotype.Service;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.beans.factory.annotation.Autowired;
@Service class UserService {}
@Service class AccountService {}
@RestController
class UserController {
    @Autowired private UserService userService;
    UserController(AccountService accounts) {}
}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-injection",
            &[SpringSourceFile {
                relative_path: "src/main/java/sample/UserController.java",
                source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();
        let controller = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "UserController")
            .unwrap();
        let user_service = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "UserService")
            .unwrap();
        let account_service = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "AccountService")
            .unwrap();
        assert!(snapshot.edges.iter().any(|edge| {
            edge.edge_type == crate::knowledge_graph::GraphEdgeType::Injects
                && edge.source_node_id == controller.node_id
                && edge.target_node_id.as_deref() == Some(user_service.node_id.as_str())
                && edge.confidence == Confidence::Confirmed
        }));
        assert!(snapshot.edges.iter().any(|edge| {
            edge.edge_type == crate::knowledge_graph::GraphEdgeType::Injects
                && edge.source_node_id == controller.node_id
                && edge.target_node_id.as_deref() == Some(account_service.node_id.as_str())
                && edge.confidence == Confidence::Inferred
        }));
    }

    #[test]
    fn resolves_constructor_injection_across_source_files() {
        let consumer = r#"
package sample;
import org.springframework.stereotype.Service;
@Service class AlertService {
    AlertService(AlertSender sender) {}
}
"#;
        let dependency = r#"
package sample;
import org.springframework.stereotype.Service;
@Service class AlertSender {}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-cross-file-injection",
            &[
                SpringSourceFile {
                    relative_path: "src/main/java/sample/AlertService.java",
                    source: consumer,
                    language: SourceLanguage::Java,
                },
                SpringSourceFile {
                    relative_path: "src/main/java/sample/AlertSender.java",
                    source: dependency,
                    language: SourceLanguage::Java,
                },
            ],
        )
        .unwrap();
        let service = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "AlertService")
            .unwrap();
        let sender = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "AlertSender")
            .unwrap();

        assert!(snapshot.edges.iter().any(|edge| {
            edge.edge_type == crate::knowledge_graph::GraphEdgeType::Injects
                && edge.source_node_id == service.node_id
                && edge.target_node_id.as_deref() == Some(sender.node_id.as_str())
                && edge.confidence == Confidence::Inferred
        }));
    }

    #[test]
    fn records_kotlin_primary_constructor_injection() {
        let source = r#"
package sample
import org.springframework.stereotype.Service
import org.springframework.web.bind.annotation.RestController
@Service class UserService
@RestController class UserController(private val userService: UserService)
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-kotlin-injection",
            &[SpringSourceFile {
                relative_path: "src/main/kotlin/sample/UserController.kt",
                source,
                language: SourceLanguage::Kotlin,
            }],
        )
        .unwrap();
        let controller = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "UserController")
            .unwrap();
        let user_service = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "UserService")
            .unwrap();
        assert!(snapshot.edges.iter().any(|edge| {
            edge.edge_type == crate::knowledge_graph::GraphEdgeType::Injects
                && edge.source_node_id == controller.node_id
                && edge.target_node_id.as_deref() == Some(user_service.node_id.as_str())
                && edge.confidence == Confidence::Inferred
        }));
    }

    #[test]
    fn joins_mvc_mapping_paths_and_links_request_response_dto_candidates() {
        let source = r#"
package sample;
import org.springframework.http.ResponseEntity;
import org.springframework.security.access.prepost.PreAuthorize;
import org.springframework.stereotype.Controller;
import org.springframework.web.bind.annotation.*;
@RestController
@RequestMapping(value = "/api")
@PreAuthorize("hasRole('USER')")
class UserController {
    @PostMapping(path = "/users")
    public ResponseEntity<UserDto> create(@RequestBody UserDto request) { return null; }
}
class UserDto {}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-route",
            &[SpringSourceFile {
                relative_path: "src/main/java/sample/UserController.java",
                source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();
        let endpoint = snapshot
            .nodes
            .iter()
            .find(|node| node.node_type == GraphNodeType::ApiEndpoint)
            .expect("endpoint node");
        assert_eq!(endpoint.metadata["httpMethod"], "POST");
        assert_eq!(endpoint.metadata["path"], "/api/users");
        assert_eq!(endpoint.metadata["handler"]["method"], "create");
        assert_eq!(
            endpoint.metadata["parameters"][0]["type"], "UserDto",
            "{endpoint:?}"
        );
        assert_eq!(endpoint.metadata["parameters"][0]["binding"], "RequestBody");
        assert_eq!(endpoint.metadata["responseType"], "ResponseEntity<UserDto>");
        assert_eq!(
            endpoint.metadata["authentication"][0]["annotation"],
            "PreAuthorize"
        );
        assert!(snapshot.nodes.iter().any(|node| {
            node.node_type == GraphNodeType::Dto
                && node.name == "UserDto"
                && node.metadata["direction"] == "request"
        }));
        assert!(snapshot.nodes.iter().any(|node| {
            node.node_type == GraphNodeType::Dto
                && node.name == "UserDto"
                && node.metadata["direction"] == "response"
        }));
    }

    #[test]
    fn creates_distinct_endpoints_for_overloaded_handlers() {
        let source = r#"
package sample;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
@RestController
class LookupController {
    @GetMapping("/lookup")
    public String lookup() { return ""; }

    @GetMapping("/lookup")
    public String lookup(String query) { return query; }
}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-overloaded-endpoints",
            &[SpringSourceFile {
                relative_path: "src/main/java/sample/LookupController.java",
                source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();
        let endpoints: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| node.node_type == GraphNodeType::ApiEndpoint)
            .collect();

        assert_eq!(endpoints.len(), 2);
        assert_ne!(endpoints[0].node_id, endpoints[1].node_id);
    }

    #[test]
    fn reports_inherited_controller_mapping_as_an_inferred_candidate() {
        let base = r#"
package sample;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;
@RequestMapping("/base")
class BaseController {
    @GetMapping("/items")
    public String list() { return "items"; }
}
"#;
        let controller = r#"
package sample;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;
@RestController
@RequestMapping("/api")
class ItemController extends BaseController {}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-inherited-route",
            &[
                SpringSourceFile {
                    relative_path: "src/main/java/sample/BaseController.java",
                    source: base,
                    language: SourceLanguage::Java,
                },
                SpringSourceFile {
                    relative_path: "src/main/java/sample/ItemController.java",
                    source: controller,
                    language: SourceLanguage::Java,
                },
            ],
        )
        .unwrap();
        let endpoint = snapshot
            .nodes
            .iter()
            .find(|node| node.node_type == GraphNodeType::ApiEndpoint)
            .expect("inherited endpoint");
        assert_eq!(endpoint.metadata["path"], "/base/api/items");
        assert_eq!(endpoint.metadata["handler"]["controller"], "ItemController");
        assert_eq!(endpoint.metadata["handler"]["method"], "list");
        assert_eq!(endpoint.metadata["handler"]["inherited"], true);
        assert_eq!(endpoint.confidence, Confidence::Inferred);
    }

    #[test]
    fn extracts_kotlin_mvc_mapping_and_path_parameters() {
        let source = r#"
package sample
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PathVariable
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RestController
@RestController
@RequestMapping("/api")
class KotlinController() {
    @GetMapping("/users/{id}")
    fun getUser(@PathVariable id: String): UserDto = UserDto()
}
data class UserDto(val id: String)
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-kotlin-route",
            &[SpringSourceFile {
                relative_path: "src/main/kotlin/sample/KotlinController.kt",
                source,
                language: SourceLanguage::Kotlin,
            }],
        )
        .unwrap();
        let endpoint = snapshot
            .nodes
            .iter()
            .find(|node| node.node_type == GraphNodeType::ApiEndpoint)
            .expect("Kotlin endpoint");
        let controller = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "KotlinController")
            .unwrap();
        assert_eq!(controller.metadata["springRoles"][0], "restController");
        let request_mapping = snapshot
            .edges
            .iter()
            .find(|edge| {
                edge.edge_type == crate::knowledge_graph::GraphEdgeType::AnnotatedBy
                    && edge.source_node_id == controller.node_id
                    && edge.target_name.as_deref() == Some("RequestMapping")
            })
            .unwrap();
        assert_eq!(
            source.get(request_mapping.evidence.start_byte..request_mapping.evidence.end_byte),
            Some("@RequestMapping(\"/api\")"),
            "evidence range {:?}",
            request_mapping.evidence
        );
        assert_eq!(endpoint.metadata["httpMethod"], "GET");
        assert_eq!(endpoint.metadata["path"], "/api/users/{id}");
        assert_eq!(
            endpoint.metadata["parameters"][0]["binding"],
            "PathVariable"
        );
        assert_eq!(endpoint.metadata["parameters"][0]["type"], "String");
        assert_eq!(endpoint.metadata["responseType"], "UserDto");
    }

    #[test]
    fn marks_dynamic_spring_route_registration_unknown() {
        let source = r#"
package sample;
class DynamicRoutes {
    void register(Object mappings, Object handler) {
        mappings.registerMapping("/runtime", handler);
    }
}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-dynamic-route",
            &[SpringSourceFile {
                relative_path: "src/main/java/sample/DynamicRoutes.java",
                source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();
        assert_eq!(snapshot.completeness, GraphCompleteness::Partial);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "dynamic_route_registration_unknown" })
        );
    }

    #[test]
    fn marks_mixed_literal_and_constant_routes_unknown() {
        let source = r#"
package sample;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
@RestController
class ItemController {
    static final String PATH_PREFIX = "/api";
    @GetMapping(PATH_PREFIX + "/items")
    public String list() { return "items"; }
}
"#;
        let snapshot = analyze_spring_sources(
            "snapshot-mixed-route-expression",
            &[SpringSourceFile {
                relative_path: "src/main/java/sample/ItemController.java",
                source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();
        let endpoint = snapshot
            .nodes
            .iter()
            .find(|node| node.node_type == GraphNodeType::ApiEndpoint)
            .expect("route candidate");

        assert!(endpoint.metadata["path"].is_null());
        assert_eq!(endpoint.confidence, Confidence::Unknown);
        assert_eq!(snapshot.completeness, GraphCompleteness::Partial);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "unresolved_route_expression")
        );
    }

    #[test]
    fn caps_oversized_files_and_marks_snapshot_partial() {
        let source = " ".repeat(5 * 1024 * 1024 + 1);
        let snapshot = analyze_spring_sources(
            "snapshot-3",
            &[SpringSourceFile {
                relative_path: "src/main/java/Large.java",
                source: &source,
                language: SourceLanguage::Java,
            }],
        )
        .unwrap();
        assert_eq!(snapshot.completeness, GraphCompleteness::Partial);
        assert!(snapshot.nodes.is_empty());
        assert_eq!(snapshot.diagnostics[0].code, "source_file_limit");
    }
}
