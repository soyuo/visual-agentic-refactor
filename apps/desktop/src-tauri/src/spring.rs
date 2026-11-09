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

const ANALYZER_VERSION: &str = "spring-static-v1";
const MAX_SOURCE_FILES: usize = 10_000;
const MAX_SOURCE_BYTES: usize = 5 * 1024 * 1024;

pub struct SpringSourceFile<'a> {
    pub relative_path: &'a str,
    pub source: &'a str,
    pub language: SourceLanguage,
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
        let mut local_types: HashMap<String, Vec<String>> = HashMap::new();
        for symbol in &output.symbols {
            if matches!(
                symbol.kind,
                SymbolKind::Class
                    | SymbolKind::Interface
                    | SymbolKind::Enum
                    | SymbolKind::Record
                    | SymbolKind::Object
            ) {
                if let Some(node_id) = symbol_ids.get(&symbol.id.0) {
                    local_types
                        .entry(symbol.name.clone())
                        .or_default()
                        .push(node_id.clone());
                }
            }
        }

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
        let mut emitted_dependencies = HashSet::new();
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
                    for dependency in
                        parameter_types(file.source, &constructor.location, file.language)
                    {
                        push_injection_edge(
                            &mut edges,
                            &mut emitted_dependencies,
                            class_node_id,
                            &dependency,
                            &constructor.location,
                            &local_types,
                            confidence,
                        )?;
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
                parameter_types(file.source, &symbol.location, file.language)
            } else {
                declared_type(file.source, symbol, file.language)
                    .into_iter()
                    .collect()
            };
            for dependency in dependencies {
                push_injection_edge(
                    &mut edges,
                    &mut emitted_dependencies,
                    parent,
                    &dependency,
                    &symbol.location,
                    &local_types,
                    Confidence::Confirmed,
                )?;
            }
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

fn analyzer(language: SourceLanguage) -> &'static dyn Analyzer {
    static JAVA: JavaAnalyzer = JavaAnalyzer;
    static KOTLIN: KotlinAnalyzer = KotlinAnalyzer;
    match language {
        SourceLanguage::Java => &JAVA,
        SourceLanguage::Kotlin => &KOTLIN,
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
    let Some(annotation) = source.get(range.start_byte..range.end_byte) else {
        return Value::Object(Map::new());
    };
    let Some(open) = annotation.find('(') else {
        return Value::Object(Map::new());
    };
    let Some(close) = annotation.rfind(')') else {
        return Value::Object(Map::new());
    };
    let arguments = annotation[open + 1..close].trim();
    if arguments.is_empty() {
        return Value::Object(Map::new());
    }
    json!({"raw": arguments})
}

fn parameter_types(source: &str, range: &SourceRange, language: SourceLanguage) -> Vec<String> {
    let Some(declaration) = source.get(range.start_byte..range.end_byte) else {
        return Vec::new();
    };
    let Some(open) = declaration.find('(') else {
        return Vec::new();
    };
    let Some(close) = matching_delimiter(declaration, open, '(', ')') else {
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
