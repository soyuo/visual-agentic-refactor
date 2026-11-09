use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::analyzer::SourceRange;
use crate::knowledge_graph::{GraphEdge, GraphNode, GraphSnapshot};

const MAX_CONTEXT_TOKENS: u32 = 32_000;
const MIN_CONTEXT_TOKENS: u32 = 128;
const ESTIMATED_BYTES_PER_TOKEN: usize = 1;
const CONTEXT_HEADER_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ReviewContextScope {
    File {
        relative_path: String,
        range: Option<SourceRange>,
    },
    GraphNode {
        node_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewContextPreview {
    pub context: String,
    pub estimated_tokens: u32,
    pub token_budget: u32,
    pub included_files: Vec<String>,
    pub omitted_items: Vec<String>,
    pub graph_snapshot_id: Option<String>,
    pub graph_completeness: Option<String>,
    pub token_estimate_note: String,
}

#[derive(Debug, Clone)]
pub struct ContextInputs<'a> {
    pub project_name: &'a str,
    pub scope: &'a ReviewContextScope,
    pub graph: Option<&'a GraphSnapshot>,
    /// Source text already read and confined by the command layer.
    pub sources: &'a BTreeMap<String, String>,
    /// Relative-source relevance scores calculated from the selected graph neighborhood.
    pub source_priorities: &'a BTreeMap<String, u8>,
    /// Applicable AGENTS.md rules, keyed by project-relative path.
    pub rules: &'a BTreeMap<String, String>,
    pub project_stack: &'a [String],
    pub source_omissions: &'a [String],
    pub max_context_tokens: u32,
}

#[derive(Debug, Clone)]
struct Section {
    priority: u8,
    key: String,
    title: String,
    path: Option<String>,
    content: String,
}

pub fn build_review_context(inputs: ContextInputs<'_>) -> Result<ReviewContextPreview, String> {
    let token_budget = inputs
        .max_context_tokens
        .clamp(MIN_CONTEXT_TOKENS, MAX_CONTEXT_TOKENS);
    let byte_budget = (token_budget as usize).saturating_mul(ESTIMATED_BYTES_PER_TOKEN);
    let (scope_path, scope_range, scope_node) = resolve_scope(inputs.scope, inputs.graph)?;
    let mut sections = Vec::new();

    let selected_source = inputs
        .sources
        .get(&scope_path)
        .ok_or_else(|| "선택한 소스 파일을 읽을 수 없습니다.".to_owned())?;
    sections.push(Section {
        priority: 0,
        key: format!("selected:{scope_path}"),
        title: format!("선택 범위: {scope_path}"),
        path: Some(scope_path.clone()),
        content: render_selected_source(selected_source, scope_range.as_ref()),
    });

    for (path, rule) in inputs.rules {
        if is_sensitive_path(path) {
            continue;
        }
        sections.push(Section {
            priority: 1,
            key: format!("rule:{path}"),
            title: format!("프로젝트 규칙: {path}"),
            path: Some(path.clone()),
            content: rule.clone(),
        });
    }

    let related = related_graph_context(scope_node, &scope_path, inputs.graph);
    if !related.summary.is_empty() {
        sections.push(Section {
            priority: 2,
            key: "graph-summary".to_owned(),
            title: "관련 심볼·관계·API 정보".to_owned(),
            path: None,
            content: related.summary,
        });
    }

    let mut source_ranges: BTreeMap<String, Vec<SourceRange>> = BTreeMap::new();
    for range in related.ranges {
        if range.relative_path != scope_path && !is_sensitive_path(&range.relative_path) {
            source_ranges
                .entry(range.relative_path.clone())
                .or_default()
                .push(range);
        }
    }
    for (path, ranges) in source_ranges {
        if let Some(source) = inputs.sources.get(&path) {
            sections.push(Section {
                priority: 3
                    + (5u8.saturating_sub(*inputs.source_priorities.get(&path).unwrap_or(&0))),
                key: format!("evidence:{path}"),
                title: format!("관련 소스 근거: {path}"),
                path: Some(path),
                content: render_evidence(source, &ranges),
            });
        }
    }

    if !inputs.project_stack.is_empty() {
        sections.push(Section {
            priority: 8,
            key: "project-stack".to_owned(),
            title: "저장된 그래프 분석 언어".to_owned(),
            path: None,
            content: inputs.project_stack.join("\n"),
        });
    }
    if !inputs.source_omissions.is_empty() {
        sections.push(Section {
            priority: 9,
            key: "source-omissions".to_owned(),
            title: "수집하지 못한 근거".to_owned(),
            path: None,
            content: inputs.source_omissions.join("\n"),
        });
    }

    sections.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.key.cmp(&right.key))
    });

    let header = format!(
        "프로젝트: {}\n선택 범위: {}\n문맥은 로컬 인덱스와 선택된 소스에서 구성했습니다.\n",
        safe_label(inputs.project_name),
        scope_title(inputs.scope, &scope_path, scope_node)
    );
    let header = if let Some(graph) = inputs.graph {
        format!(
            "{header}{}",
            format!(
                "그래프 스냅샷: {} ({:?})\n",
                graph.summary.snapshot_id, graph.summary.completeness
            )
        )
    } else {
        format!("{header}관련 분석 그래프가 없어 선택한 소스와 프로젝트 규칙만 포함합니다.\n")
    };
    let mut context = truncate_utf8(&header, byte_budget);

    let mut included_files = BTreeSet::new();
    let mut omitted_items = inputs.source_omissions.to_vec();
    let mut seen = HashSet::new();
    for section in sections {
        if !seen.insert(section.key.clone()) {
            omitted_items.push(format!("중복 항목 생략: {}", section.title));
            continue;
        }
        let rendered = format!(
            "\n## {}\n{}\n",
            section.title,
            redact_secrets(&section.content)
        );
        let remaining = byte_budget.saturating_sub(context.len());
        if remaining < CONTEXT_HEADER_BYTES {
            omitted_items.push(format!("문맥 예산 초과로 생략: {}", section.title));
            continue;
        }
        if let Some(path) = section.path {
            included_files.insert(path);
        }
        if rendered.len() > remaining {
            let truncated = truncate_utf8(&rendered, remaining);
            context.push_str(&truncated);
            omitted_items.push(format!(
                "파일별 또는 전체 문맥 예산에 맞춰 일부 내용 생략: {}",
                section.title
            ));
            continue;
        }
        context.push_str(&rendered);
    }

    if !omitted_items.is_empty() {
        let note = format!("\n## 생략 항목\n{}\n", omitted_items.join("\n"));
        if context.len().saturating_add(note.len()) <= byte_budget {
            context.push_str(&note);
        }
    }

    let estimated_tokens = context.len().div_ceil(ESTIMATED_BYTES_PER_TOKEN) as u32;
    Ok(ReviewContextPreview {
        context,
        estimated_tokens,
        token_budget,
        included_files: included_files.into_iter().collect(),
        omitted_items,
        graph_snapshot_id: inputs.graph.map(|graph| graph.summary.snapshot_id.clone()),
        graph_completeness: inputs.graph.map(|graph| format!("{:?}", graph.summary.completeness)),
        token_estimate_note: "UTF-8 바이트 수를 token 상한으로 사용합니다. 실제 provider token 수보다 크게 계산될 수 있습니다.".to_owned(),
    })
}

struct RelatedGraphContext {
    summary: String,
    ranges: Vec<SourceRange>,
}

fn resolve_scope<'a>(
    scope: &ReviewContextScope,
    graph: Option<&'a GraphSnapshot>,
) -> Result<(String, Option<SourceRange>, Option<&'a GraphNode>), String> {
    match scope {
        ReviewContextScope::File {
            relative_path,
            range,
        } => {
            validate_relative_path(relative_path)?;
            if let Some(range) = range {
                if range.relative_path != *relative_path || range.start_byte > range.end_byte {
                    return Err("선택한 소스 범위가 파일과 일치하지 않습니다.".to_owned());
                }
            }
            Ok((relative_path.replace('\\', "/"), range.clone(), None))
        }
        ReviewContextScope::GraphNode { node_id } => {
            let graph = graph.ok_or_else(|| "저장된 분석 그래프가 없습니다.".to_owned())?;
            let node = graph
                .nodes
                .iter()
                .find(|node| node.node_id == *node_id)
                .ok_or_else(|| "선택한 그래프 항목이 최신 분석 그래프에 없습니다.".to_owned())?;
            let range = node.source_evidence.clone();
            let path = node
                .file_path
                .clone()
                .or_else(|| range.as_ref().map(|range| range.relative_path.clone()))
                .ok_or_else(|| "선택한 그래프 항목에 소스 파일 위치가 없습니다.".to_owned())?;
            validate_relative_path(&path)?;
            if range.as_ref().is_some_and(|range| {
                range.relative_path.replace('\\', "/") != path.replace('\\', "/")
            }) {
                return Err("선택한 그래프 항목의 소스 범위가 파일과 일치하지 않습니다.".to_owned());
            }
            Ok((path.replace('\\', "/"), range, Some(node)))
        }
    }
}

fn related_graph_context(
    selected_node: Option<&GraphNode>,
    scope_path: &str,
    graph: Option<&GraphSnapshot>,
) -> RelatedGraphContext {
    let Some(graph) = graph else {
        return RelatedGraphContext {
            summary: String::new(),
            ranges: Vec::new(),
        };
    };
    let selected_id = selected_node.map(|node| node.node_id.as_str());
    let seed_ids: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|node| {
            node.file_path.as_deref() == Some(scope_path)
                || node
                    .source_evidence
                    .as_ref()
                    .is_some_and(|range| range.relative_path == scope_path)
                || selected_id == Some(node.node_id.as_str())
        })
        .map(|node| node.node_id.as_str())
        .collect();

    let related_edges: Vec<&GraphEdge> = graph
        .edges
        .iter()
        .filter(|edge| {
            seed_ids.contains(edge.source_node_id.as_str())
                || edge
                    .target_node_id
                    .as_deref()
                    .is_some_and(|target| seed_ids.contains(target))
        })
        .filter(|edge| !is_sensitive_path(&edge.evidence.relative_path))
        .take(256)
        .collect();
    let mut related_ids = seed_ids.clone();
    for edge in &related_edges {
        related_ids.insert(edge.source_node_id.as_str());
        if let Some(target) = edge.target_node_id.as_deref() {
            related_ids.insert(target);
        }
    }
    let nodes: Vec<&GraphNode> = graph
        .nodes
        .iter()
        .filter(|node| related_ids.contains(node.node_id.as_str()))
        .take(256)
        .collect();
    let by_id: BTreeMap<&str, &GraphNode> = nodes
        .iter()
        .map(|node| (node.node_id.as_str(), *node))
        .collect();
    let mut lines = Vec::new();
    let mut ranges = Vec::new();
    for node in nodes {
        if node.file_path.as_deref().is_some_and(is_sensitive_path)
            || node
                .source_evidence
                .as_ref()
                .is_some_and(|range| is_sensitive_path(&range.relative_path))
        {
            continue;
        }
        lines.push(format!(
            "{} `{}`{}{}",
            format!("{:?}", node.node_type),
            safe_label(
                &node
                    .qualified_name
                    .clone()
                    .unwrap_or_else(|| node.name.clone())
            ),
            node.file_path
                .as_ref()
                .map(|path| format!(" — {path}"))
                .unwrap_or_default(),
            format!(" — confidence: {:?}", node.confidence)
        ));
        if let Some(range) = &node.source_evidence {
            ranges.push(range.clone());
        }
    }
    for edge in related_edges {
        if is_sensitive_path(&edge.evidence.relative_path) {
            continue;
        }
        let source = by_id
            .get(edge.source_node_id.as_str())
            .map(|node| safe_label(&node.name))
            .unwrap_or_else(|| edge.source_node_id.clone());
        let target = edge
            .target_node_id
            .as_deref()
            .and_then(|id| by_id.get(id))
            .map(|node| safe_label(&node.name))
            .or_else(|| edge.target_name.as_deref().map(safe_label))
            .unwrap_or_else(|| "unresolved".to_owned());
        lines.push(format!(
            "{:?}: {source} → {target} — {}:{} — confidence: {:?}",
            edge.edge_type, edge.evidence.relative_path, edge.evidence.start_line, edge.confidence
        ));
        ranges.push(edge.evidence.clone());
    }
    lines.sort();
    lines.dedup();
    ranges.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then(left.start_byte.cmp(&right.start_byte))
    });
    ranges.dedup_by(|left, right| {
        left.relative_path == right.relative_path
            && left.start_byte == right.start_byte
            && left.end_byte == right.end_byte
    });
    RelatedGraphContext {
        summary: lines.join("\n"),
        ranges,
    }
}

fn render_selected_source(source: &str, range: Option<&SourceRange>) -> String {
    match range {
        Some(range) => render_lines(source, range.start_line, range.end_line, 5),
        None => source.to_owned(),
    }
}

fn render_evidence(source: &str, ranges: &[SourceRange]) -> String {
    let mut chunks = Vec::new();
    for range in ranges.iter().take(16) {
        let chunk = render_lines(source, range.start_line, range.end_line, 2);
        if !chunk.is_empty() && !chunks.contains(&chunk) {
            chunks.push(chunk);
        }
    }
    chunks.join("\n…\n")
}

fn render_lines(source: &str, start_line: u32, end_line: u32, padding: u32) -> String {
    let start = start_line.saturating_sub(padding).max(1) as usize;
    let end = end_line.saturating_add(padding).max(start_line) as usize;
    source
        .lines()
        .enumerate()
        .filter(|(index, _)| index + 1 >= start && index + 1 <= end)
        .map(|(index, line)| format!("{}: {}", index + 1, redact_secrets(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let path = std::path::Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("선택 범위에는 프로젝트 내부의 상대 경로가 필요합니다.".to_owned());
    }
    if is_sensitive_path(&path.to_string_lossy()) {
        return Err("민감 정보 가능성이 있는 경로는 문맥에 포함할 수 없습니다.".to_owned());
    }
    Ok(())
}

pub fn is_sensitive_path(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|segment| {
        let segment = segment.to_ascii_lowercase();
        segment == ".env"
            || segment.starts_with(".env.")
            || segment == ".npmrc"
            || segment == ".netrc"
            || segment.contains("secret")
            || segment.contains("credential")
            || segment.contains("password")
            || segment.contains("token")
            || segment.contains("private-key")
            || segment.contains("private_key")
            || segment == "id_rsa"
            || segment == "id_ed25519"
            || [
                ".pem",
                ".key",
                ".p12",
                ".pfx",
                ".jks",
                ".keystore",
                ".crt",
                ".cer",
                ".der",
            ]
            .iter()
            .any(|suffix| segment.ends_with(suffix))
    })
}

fn redact_secrets(text: &str) -> String {
    let mut in_private_key = false;
    text.lines()
        .map(|line| {
            if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
                in_private_key = true;
                return "[REDACTED PRIVATE KEY]".to_owned();
            }
            if in_private_key {
                if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                    in_private_key = false;
                }
                return "[REDACTED PRIVATE KEY]".to_owned();
            }
            redact_secret_line(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_secret_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let assignment = trimmed.split_once('=').or_else(|| trimmed.split_once(':'));
    if let Some((key, _)) = assignment {
        let key = key
            .trim()
            .trim_matches(['"', '\''])
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if [
            "apikey",
            "accesskey",
            "secret",
            "clientsecret",
            "password",
            "passwd",
            "token",
            "authorization",
            "privatekey",
        ]
        .iter()
        .any(|marker| key.contains(marker))
        {
            let value_separator = line
                .find('=')
                .or_else(|| line.find(':'))
                .unwrap_or(line.len());
            let mut value_start = value_separator.saturating_add(1);
            while line
                .as_bytes()
                .get(value_start)
                .is_some_and(u8::is_ascii_whitespace)
            {
                value_start += 1;
            }
            return format!("{}[REDACTED]", &line[..value_start]);
        }
    }
    let lower = line.to_ascii_lowercase();
    if let Some(index) = lower.find("bearer ") {
        return format!("{}Bearer [REDACTED]", &line[..index]);
    }
    line.to_owned()
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let marker = "\n[… 일부 생략]";
    if max_bytes <= marker.len() {
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        return text[..end].to_owned();
    }
    let mut end = max_bytes.saturating_sub(marker.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &text[..end], marker)
}

fn scope_title(scope: &ReviewContextScope, path: &str, node: Option<&GraphNode>) -> String {
    match scope {
        ReviewContextScope::File { .. } => format!("file {path}"),
        ReviewContextScope::GraphNode { .. } => node
            .map(|node| format!("graph node {} ({path})", safe_label(&node.name)))
            .unwrap_or_else(|| format!("graph node ({path})")),
    }
}

fn safe_label(value: &str) -> String {
    redact_secrets(value).replace(['\r', '\0'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{Confidence, SourceLanguage};
    use crate::knowledge_graph::{GraphCompleteness, GraphNodeType, GraphSnapshotSummary};
    use serde_json::json;

    fn sample_graph() -> GraphSnapshot {
        let evidence = SourceRange {
            relative_path: "src/Main.kt".to_owned(),
            start_byte: 0,
            end_byte: 16,
            start_line: 1,
            start_column: 0,
            end_line: 1,
            end_column: 16,
        };
        GraphSnapshot {
            project_path: "C:/work/sample".to_owned(),
            summary: GraphSnapshotSummary {
                snapshot_id: "snapshot-1".to_owned(),
                head_sha: None,
                analyzer_version: "test".to_owned(),
                source_hashes: BTreeMap::new(),
                settings: json!({}),
                completeness: GraphCompleteness::Partial,
                analyzed_at_unix_ms: 1,
            },
            nodes: vec![
                GraphNode {
                    node_id: "method:main".to_owned(),
                    node_type: GraphNodeType::Method,
                    name: "main".to_owned(),
                    qualified_name: Some("sample.main".to_owned()),
                    file_path: Some("src/Main.kt".to_owned()),
                    source_evidence: Some(evidence.clone()),
                    language: Some(SourceLanguage::Kotlin),
                    metadata: json!({}),
                    confidence: Confidence::Confirmed,
                },
                GraphNode {
                    node_id: "api:items".to_owned(),
                    node_type: GraphNodeType::ApiEndpoint,
                    name: "items".to_owned(),
                    qualified_name: Some("GET /items".to_owned()),
                    file_path: Some("src/Controller.kt".to_owned()),
                    source_evidence: Some(SourceRange {
                        relative_path: "src/Controller.kt".to_owned(),
                        start_byte: 0,
                        end_byte: 24,
                        start_line: 1,
                        start_column: 0,
                        end_line: 1,
                        end_column: 24,
                    }),
                    language: Some(SourceLanguage::Kotlin),
                    metadata: json!({}),
                    confidence: Confidence::Inferred,
                },
            ],
            edges: vec![GraphEdge {
                edge_id: "edge-1".to_owned(),
                edge_type: crate::knowledge_graph::GraphEdgeType::Calls,
                source_node_id: "method:main".to_owned(),
                target_node_id: Some("api:items".to_owned()),
                target_name: None,
                evidence: SourceRange {
                    relative_path: "src/Main.kt".to_owned(),
                    start_byte: 10,
                    end_byte: 16,
                    start_line: 1,
                    start_column: 10,
                    end_line: 1,
                    end_column: 16,
                },
                confidence: Confidence::Inferred,
            }],
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn builds_selected_context_with_related_api_rules_and_deduplication() {
        let graph = sample_graph();
        let sources = BTreeMap::from([
            ("src/Main.kt".to_owned(), "fun main() { call() }".to_owned()),
            (
                "src/Controller.kt".to_owned(),
                "@GetMapping(\"/items\")\nfun items() {}".to_owned(),
            ),
        ]);
        let rules = BTreeMap::from([(
            "AGENTS.md".to_owned(),
            "Do not change generated files.".to_owned(),
        )]);
        let stack = vec!["Kotlin".to_owned(), "Spring Boot".to_owned()];
        let preview = build_review_context(ContextInputs {
            project_name: "sample",
            scope: &ReviewContextScope::File {
                relative_path: "src/Main.kt".to_owned(),
                range: None,
            },
            graph: Some(&graph),
            sources: &sources,
            source_priorities: &BTreeMap::new(),
            rules: &rules,
            project_stack: &stack,
            source_omissions: &[],
            max_context_tokens: 2_000,
        })
        .expect("context");

        assert!(preview.context.contains("fun main()"));
        assert!(preview.context.contains("GET /items"));
        assert!(preview.context.contains("Do not change generated files."));
        assert!(preview.context.contains("Kotlin"));
        assert_eq!(
            preview
                .included_files
                .iter()
                .filter(|path| *path == "src/Main.kt")
                .count(),
            1
        );
        assert_eq!(preview.graph_snapshot_id.as_deref(), Some("snapshot-1"));
        assert_eq!(preview.graph_completeness.as_deref(), Some("Partial"));
    }

    #[test]
    fn graph_node_scope_uses_its_source_evidence_and_related_context() {
        let graph = sample_graph();
        let sources = BTreeMap::from([
            ("src/Main.kt".to_owned(), "fun main() { call() }".to_owned()),
            (
                "src/Controller.kt".to_owned(),
                "@GetMapping(\"/items\")\nfun items() {}".to_owned(),
            ),
        ]);
        let preview = build_review_context(ContextInputs {
            project_name: "sample",
            scope: &ReviewContextScope::GraphNode {
                node_id: "method:main".to_owned(),
            },
            graph: Some(&graph),
            sources: &sources,
            source_priorities: &BTreeMap::new(),
            rules: &BTreeMap::new(),
            project_stack: &[],
            source_omissions: &[],
            max_context_tokens: 2_000,
        })
        .expect("context");
        assert!(preview.context.contains("graph node main"));
        assert!(preview.context.contains("GET /items"));
        assert!(preview.context.contains("fun main()"));
    }

    #[test]
    fn context_budget_is_bounded_and_omissions_are_reported() {
        let source = "x".repeat(4_000);
        let sources = BTreeMap::from([("src/Main.kt".to_owned(), source)]);
        let preview = build_review_context(ContextInputs {
            project_name: "sample",
            scope: &ReviewContextScope::File {
                relative_path: "src/Main.kt".to_owned(),
                range: None,
            },
            graph: None,
            sources: &sources,
            source_priorities: &BTreeMap::new(),
            rules: &BTreeMap::new(),
            project_stack: &[],
            source_omissions: &["src/Related.kt: UTF-8 텍스트로 읽을 수 없습니다.".to_owned()],
            max_context_tokens: 128,
        })
        .expect("context");
        assert!(preview.context.len() <= 128 * ESTIMATED_BYTES_PER_TOKEN);
        assert!(!preview.omitted_items.is_empty());
        assert!(
            preview
                .omitted_items
                .iter()
                .any(|item| item.contains("src/Related.kt"))
        );
        assert!(preview.estimated_tokens <= preview.token_budget);
    }

    #[test]
    fn graph_node_scope_rejects_source_evidence_from_another_file() {
        let mut graph = sample_graph();
        graph.nodes[0]
            .source_evidence
            .as_mut()
            .expect("source evidence")
            .relative_path = "src/Other.kt".to_owned();
        let sources = BTreeMap::from([("src/Main.kt".to_owned(), "fun main() {}".to_owned())]);
        let result = build_review_context(ContextInputs {
            project_name: "sample",
            scope: &ReviewContextScope::GraphNode {
                node_id: "method:main".to_owned(),
            },
            graph: Some(&graph),
            sources: &sources,
            source_priorities: &BTreeMap::new(),
            rules: &BTreeMap::new(),
            project_stack: &[],
            source_omissions: &[],
            max_context_tokens: 2_000,
        });
        assert!(result.is_err());
    }

    #[test]
    fn graph_context_excludes_relationships_with_sensitive_evidence_paths() {
        let mut graph = sample_graph();
        graph.edges.push(GraphEdge {
            edge_id: "edge-secret".to_owned(),
            edge_type: crate::knowledge_graph::GraphEdgeType::References,
            source_node_id: "method:main".to_owned(),
            target_node_id: Some("api:items".to_owned()),
            target_name: None,
            evidence: SourceRange {
                relative_path: "config/.env.production".to_owned(),
                start_byte: 0,
                end_byte: 10,
                start_line: 1,
                start_column: 0,
                end_line: 1,
                end_column: 10,
            },
            confidence: Confidence::Inferred,
        });
        let sources = BTreeMap::from([
            ("src/Main.kt".to_owned(), "fun main() { call() }".to_owned()),
            (
                "src/Controller.kt".to_owned(),
                "@GetMapping(\"/items\")\nfun items() {}".to_owned(),
            ),
        ]);
        let preview = build_review_context(ContextInputs {
            project_name: "sample",
            scope: &ReviewContextScope::File {
                relative_path: "src/Main.kt".to_owned(),
                range: None,
            },
            graph: Some(&graph),
            sources: &sources,
            source_priorities: &BTreeMap::new(),
            rules: &BTreeMap::new(),
            project_stack: &[],
            source_omissions: &[],
            max_context_tokens: 2_000,
        })
        .expect("context");
        assert!(!preview.context.contains(".env.production"));
        assert!(
            !preview
                .included_files
                .iter()
                .any(|path| path.contains(".env"))
        );
    }

    #[test]
    fn sensitive_paths_and_assignment_values_are_excluded_or_redacted() {
        assert!(is_sensitive_path("config/.env.local"));
        assert!(is_sensitive_path("keys/signing.p12"));
        assert_eq!(
            redact_secrets("OPENAI_API_KEY=sk-secret"),
            "OPENAI_API_KEY=[REDACTED]"
        );
        assert_eq!(
            redact_secrets("Authorization: Bearer abc123"),
            "Authorization: [REDACTED]"
        );
        assert_eq!(
            redact_secrets("const apiKey = \"sk-secret\";"),
            "const apiKey = [REDACTED]"
        );
        assert_eq!(
            redact_secrets(
                "-----BEGIN PRIVATE KEY-----\nprivate-material\n-----END PRIVATE KEY-----"
            ),
            "[REDACTED PRIVATE KEY]\n[REDACTED PRIVATE KEY]\n[REDACTED PRIVATE KEY]"
        );
    }
}
