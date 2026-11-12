use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tauri::State;

use crate::ai_provider::ReviewSelection;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::context_builder::{
    ContextInputs, RenderedSourceSpan, ReviewContextPreview, ReviewContextScope,
    build_review_context, is_sensitive_path,
};
use crate::credentials::CredentialProvider;
use crate::knowledge_graph::GraphSnapshot;
use crate::persistence::PersistenceState;

const MAX_SELECTED_SOURCE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RELATED_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_RELATED_FILES: usize = 24;
const MAX_TOTAL_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RULE_BYTES: u64 = 32 * 1024;
const MAX_REGISTERED_PREVIEWS: usize = 32;

#[derive(Default)]
pub struct ReviewContextPreviewState {
    previews: Mutex<BTreeMap<String, PreviewClaim>>,
}

#[derive(Clone)]
struct PreviewClaim {
    project_path: String,
    snapshot_id: Option<String>,
    scope: ReviewContextScope,
    context: String,
    token_budget: u32,
    included_files: Vec<String>,
    included_source_spans: Vec<RenderedSourceSpan>,
    approved_transfer: Option<(CredentialProvider, String)>,
}

impl ReviewContextPreviewState {
    fn register(
        &self,
        project_path: String,
        scope: ReviewContextScope,
        preview: &mut ReviewContextPreview,
    ) -> Result<(), String> {
        let preview_id = new_preview_id();
        let claim = PreviewClaim {
            project_path,
            snapshot_id: preview.graph_snapshot_id.clone(),
            scope,
            context: preview.context.clone(),
            token_budget: preview.token_budget,
            included_files: preview.included_files.clone(),
            included_source_spans: preview.included_source_spans.clone(),
            approved_transfer: None,
        };
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "문맥 미리보기 상태를 확인할 수 없습니다.".to_owned())?;
        if previews.len() >= MAX_REGISTERED_PREVIEWS {
            previews.clear();
        }
        previews.insert(preview_id.clone(), claim);
        preview.preview_id = preview_id;
        Ok(())
    }

    pub(crate) fn validated_context(
        &self,
        preview_id: &str,
        project_path: &str,
        snapshot_id: &str,
        selection: &ReviewSelection,
    ) -> Result<(String, u32, Vec<String>, Vec<RenderedSourceSpan>), String> {
        let previews = self
            .previews
            .lock()
            .map_err(|_| "문맥 미리보기 상태를 확인할 수 없습니다.".to_owned())?;
        let claim = previews.get(preview_id).ok_or_else(|| {
            "문맥 미리보기가 만료되었습니다. 다시 미리보기를 생성하세요.".to_owned()
        })?;
        if claim.project_path != project_path
            || claim.snapshot_id.as_deref() != Some(snapshot_id)
            || !selection_matches_scope(&claim.scope, selection)
        {
            return Err(
                "문맥 미리보기의 프로젝트, 스냅샷 또는 선택 범위가 일치하지 않습니다.".to_owned(),
            );
        }
        Ok((
            claim.context.clone(),
            claim.token_budget,
            claim.included_files.clone(),
            claim.included_source_spans.clone(),
        ))
    }

    pub(crate) fn approve_provider_transfer(
        &self,
        preview_id: &str,
        project_path: &str,
        snapshot_id: &str,
        selection: &ReviewSelection,
        provider: CredentialProvider,
        model: &str,
    ) -> Result<(), String> {
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "문맥 미리보기 상태를 확인할 수 없습니다.".to_owned())?;
        let claim = previews.get_mut(preview_id).ok_or_else(|| {
            "문맥 미리보기가 만료되었습니다. 다시 미리보기를 생성하세요.".to_owned()
        })?;
        if claim.project_path != project_path
            || claim.snapshot_id.as_deref() != Some(snapshot_id)
            || !selection_matches_scope(&claim.scope, selection)
        {
            return Err(
                "문맥 미리보기의 프로젝트, 스냅샷 또는 선택 범위가 일치하지 않습니다.".to_owned(),
            );
        }
        claim.approved_transfer = Some((provider, model.to_owned()));
        Ok(())
    }

    pub(crate) fn consume_approved_context(
        &self,
        preview_id: &str,
        project_path: &str,
        snapshot_id: &str,
        selection: &ReviewSelection,
        provider: CredentialProvider,
        model: &str,
    ) -> Result<(String, u32), String> {
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "문맥 미리보기 상태를 확인할 수 없습니다.".to_owned())?;
        let claim = previews.get_mut(preview_id).ok_or_else(|| {
            "문맥 미리보기가 만료되었습니다. 다시 미리보기를 생성하세요.".to_owned()
        })?;
        if claim.project_path != project_path
            || claim.snapshot_id.as_deref() != Some(snapshot_id)
            || !selection_matches_scope(&claim.scope, selection)
        {
            return Err(
                "문맥 미리보기의 프로젝트, 스냅샷 또는 선택 범위가 일치하지 않습니다.".to_owned(),
            );
        }
        if !matches!(
            claim.approved_transfer.as_ref(),
            Some((approved_provider, approved_model))
                if *approved_provider == provider && approved_model == model
        ) {
            return Err("제공자, 모델, 문맥 범위에 대한 일회성 전송 동의가 필요합니다.".to_owned());
        }
        claim.approved_transfer = None;
        Ok((claim.context.clone(), claim.token_budget))
    }
}

static NEXT_PREVIEW_ID: AtomicU64 = AtomicU64::new(1);

fn new_preview_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{timestamp:032x}-{:016x}",
        NEXT_PREVIEW_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn selection_matches_scope(scope: &ReviewContextScope, selection: &ReviewSelection) -> bool {
    match (scope, selection) {
        (
            ReviewContextScope::File {
                relative_path: scope_path,
                range: scope_range,
            },
            ReviewSelection::File {
                relative_path: selected_path,
                range: selected_range,
            },
        ) => scope_path == selected_path && scope_range == selected_range,
        (
            ReviewContextScope::GraphNode {
                node_id: scope_node,
            },
            ReviewSelection::GraphNode {
                node_id: selected_node,
                ..
            },
        ) => scope_node == selected_node,
        _ => false,
    }
}

#[cfg(test)]
mod preview_state_tests {
    use super::*;

    fn preview() -> ReviewContextPreview {
        ReviewContextPreview {
            preview_id: String::new(),
            context: "visible source".to_owned(),
            estimated_tokens: 13,
            token_budget: 128,
            included_files: vec!["src/Main.kt".to_owned()],
            included_source_spans: vec![RenderedSourceSpan {
                relative_path: "src/Main.kt".to_owned(),
                start_line: 1,
                end_line: 1,
                visible_text: "fun main() {}".to_owned(),
            }],
            omitted_items: Vec::new(),
            graph_snapshot_id: Some("snapshot-1".to_owned()),
            graph_completeness: Some("Complete".to_owned()),
            token_estimate_note: "test".to_owned(),
        }
    }

    #[test]
    fn preview_claim_binds_project_snapshot_scope_and_rendered_spans() {
        let state = ReviewContextPreviewState::default();
        let scope = ReviewContextScope::File {
            relative_path: "src/Main.kt".to_owned(),
            range: None,
        };
        let mut preview = preview();
        state
            .register("C:/work/sample".to_owned(), scope, &mut preview)
            .expect("register preview");

        let (context, token_budget, files, spans) = state
            .validated_context(
                &preview.preview_id,
                "C:/work/sample",
                "snapshot-1",
                &ReviewSelection::File {
                    relative_path: "src/Main.kt".to_owned(),
                    range: None,
                },
            )
            .expect("validated preview");
        assert_eq!(files, preview.included_files);
        assert_eq!(spans, preview.included_source_spans);
        assert_eq!(context, preview.context);
        assert_eq!(token_budget, preview.token_budget);
        assert!(!preview.preview_id.is_empty());
    }

    #[test]
    fn preview_claim_rejects_a_forged_or_stale_scope() {
        let state = ReviewContextPreviewState::default();
        let mut preview = preview();
        state
            .register(
                "C:/work/sample".to_owned(),
                ReviewContextScope::File {
                    relative_path: "src/Main.kt".to_owned(),
                    range: None,
                },
                &mut preview,
            )
            .expect("register preview");

        let result = state.validated_context(
            &preview.preview_id,
            "C:/work/other",
            "snapshot-1",
            &ReviewSelection::File {
                relative_path: "src/Other.kt".to_owned(),
                range: None,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn provider_transfer_approval_is_scope_bound_and_one_time() {
        let state = ReviewContextPreviewState::default();
        let scope = ReviewContextScope::File {
            relative_path: "src/Main.kt".to_owned(),
            range: None,
        };
        let mut preview = preview();
        state
            .register("C:/work/sample".to_owned(), scope, &mut preview)
            .expect("register preview");
        let selection = ReviewSelection::File {
            relative_path: "src/Main.kt".to_owned(),
            range: None,
        };

        assert!(
            state
                .consume_approved_context(
                    &preview.preview_id,
                    "C:/work/sample",
                    "snapshot-1",
                    &selection,
                    CredentialProvider::OpenAi,
                    "model-a",
                )
                .is_err()
        );
        state
            .approve_provider_transfer(
                &preview.preview_id,
                "C:/work/sample",
                "snapshot-1",
                &selection,
                CredentialProvider::Anthropic,
                "model-a",
            )
            .expect("approve transfer");
        assert!(
            state
                .consume_approved_context(
                    &preview.preview_id,
                    "C:/work/sample",
                    "snapshot-1",
                    &selection,
                    CredentialProvider::OpenAi,
                    "model-a",
                )
                .is_err()
        );
        assert!(
            state
                .consume_approved_context(
                    &preview.preview_id,
                    "C:/work/sample",
                    "snapshot-1",
                    &selection,
                    CredentialProvider::Anthropic,
                    "model-b",
                )
                .is_err()
        );
        assert_eq!(
            state
                .consume_approved_context(
                    &preview.preview_id,
                    "C:/work/sample",
                    "snapshot-1",
                    &selection,
                    CredentialProvider::Anthropic,
                    "model-a",
                )
                .expect("consume approved transfer"),
            (preview.context.clone(), preview.token_budget)
        );
        assert!(
            state
                .consume_approved_context(
                    &preview.preview_id,
                    "C:/work/sample",
                    "snapshot-1",
                    &selection,
                    CredentialProvider::Anthropic,
                    "model-a",
                )
                .is_err()
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReviewContextRequest {
    pub project_path: String,
    pub scope: ReviewContextScope,
    #[serde(default)]
    pub max_context_tokens: u32,
}

#[tauri::command]
pub fn preview_review_context(
    request: PreviewReviewContextRequest,
    persistence: State<'_, PersistenceState>,
    preview_state: State<'_, ReviewContextPreviewState>,
) -> Result<ReviewContextPreview, CommandError> {
    let project_path = PathBuf::from(&request.project_path);
    if !project_path.is_absolute() {
        return Err(invalid("프로젝트 경로가 올바르지 않습니다."));
    }
    let root = fs::canonicalize(&project_path)
        .map_err(|_| invalid("프로젝트 경로를 확인할 수 없습니다."))?;
    if !root.is_dir() {
        return Err(invalid("선택한 프로젝트 경로가 디렉터리가 아닙니다."));
    }

    let database = persistence.database()?;
    let graph = latest_graph(&database, &root)?;
    let selected_path = match &request.scope {
        ReviewContextScope::File { relative_path, .. } => normalize_relative(relative_path)?,
        ReviewContextScope::GraphNode { node_id } => {
            let graph = graph
                .as_ref()
                .ok_or_else(|| invalid("저장된 분석 그래프가 없습니다."))?;
            let node = graph
                .nodes
                .iter()
                .find(|node| node.node_id == *node_id)
                .ok_or_else(|| invalid("선택한 그래프 항목이 최신 분석 그래프에 없습니다."))?;
            node.file_path
                .clone()
                .or_else(|| {
                    node.source_evidence
                        .as_ref()
                        .map(|range| range.relative_path.clone())
                })
                .map(|path| normalize_relative(&path))
                .transpose()?
                .ok_or_else(|| invalid("선택한 그래프 항목에 소스 파일 위치가 없습니다."))?
        }
    };
    if is_sensitive_path(&selected_path) {
        return Err(invalid(
            "민감 정보 가능성이 있는 파일은 문맥에 포함할 수 없습니다.",
        ));
    }

    let mut sources = BTreeMap::new();
    let mut source_priorities = BTreeMap::new();
    let mut omissions = Vec::new();
    let selected_source = read_source(&root, &selected_path, MAX_SELECTED_SOURCE_BYTES)
        .map_err(|_| invalid("선택한 소스 파일을 안전하게 읽을 수 없습니다."))?;
    let mut total_bytes = selected_source.len() as u64;
    sources.insert(selected_path.clone(), selected_source);

    let related_paths = related_source_paths(&request.scope, &selected_path, graph.as_ref());
    if related_paths.len() > MAX_RELATED_FILES {
        omissions.push(format!(
            "관련 소스 파일 {}개가 파일 수 제한 때문에 생략되었습니다.",
            related_paths.len() - MAX_RELATED_FILES
        ));
    }
    for (path, priority) in related_paths.into_iter().take(MAX_RELATED_FILES) {
        if is_sensitive_path(&path) {
            omissions.push("민감 정보 가능성이 있는 관련 파일을 제외했습니다.".to_owned());
            continue;
        }
        match read_source(&root, &path, MAX_RELATED_SOURCE_BYTES) {
            Ok(source)
                if total_bytes.saturating_add(source.len() as u64) <= MAX_TOTAL_SOURCE_BYTES =>
            {
                total_bytes += source.len() as u64;
                sources.insert(path.clone(), source);
                source_priorities.insert(path, priority);
            }
            Ok(_) => omissions.push(format!("전체 소스 읽기 제한 때문에 {path}를 생략했습니다.")),
            Err(reason) => omissions.push(format!("{path}: {reason}")),
        }
    }

    let rules = read_applicable_rules(&root, &selected_path, &mut omissions);
    let project_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let project_stack = graph.as_ref().map(graph_languages).unwrap_or_default();
    let mut preview = build_review_context(ContextInputs {
        project_name,
        scope: &request.scope,
        graph: graph.as_ref(),
        sources: &sources,
        source_priorities: &source_priorities,
        rules: &rules,
        project_stack: &project_stack,
        source_omissions: &omissions,
        max_context_tokens: request.max_context_tokens,
    })
    .map_err(invalid)?;
    preview_state
        .register(
            root.to_string_lossy().into_owned(),
            request.scope,
            &mut preview,
        )
        .map_err(internal)?;
    Ok(preview)
}

fn latest_graph(
    database: &crate::persistence::ProjectDatabase,
    root: &Path,
) -> Result<Option<GraphSnapshot>, CommandError> {
    let summaries = database
        .list_graph_snapshots(&root.to_string_lossy())
        .map_err(internal)?;
    let Some(summary) = summaries.first() else {
        return Ok(None);
    };
    database
        .load_graph_snapshot(&root.to_string_lossy(), &summary.snapshot_id)
        .map_err(internal)
}

fn related_source_paths(
    scope: &ReviewContextScope,
    selected_path: &str,
    graph: Option<&GraphSnapshot>,
) -> Vec<(String, u8)> {
    let Some(graph) = graph else {
        return Vec::new();
    };
    let selected_node_id = match scope {
        ReviewContextScope::GraphNode { node_id } => Some(node_id.as_str()),
        ReviewContextScope::File { .. } => None,
    };
    let seeds: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|node| {
            selected_node_id == Some(node.node_id.as_str())
                || node
                    .file_path
                    .as_deref()
                    .is_some_and(|path| normalize_graph_path(path) == selected_path)
                || node.source_evidence.as_ref().is_some_and(|range| {
                    normalize_graph_path(&range.relative_path) == selected_path
                })
        })
        .map(|node| node.node_id.as_str())
        .collect();
    let mut seeds = seeds;
    if selected_node_id.is_some() {
        // Include every descendant reachable through containment edges so a
        // selected module/package/class reviews its child code too.
        let mut frontier: Vec<&str> = seeds.iter().copied().collect();
        while let Some(parent) = frontier.pop() {
            for edge in &graph.edges {
                if edge.edge_type == crate::knowledge_graph::GraphEdgeType::Contains
                    && edge.source_node_id == parent
                    && let Some(child) = edge.target_node_id.as_deref()
                    && seeds.insert(child)
                {
                    frontier.push(child);
                }
            }
        }
    }
    let mut ids = seeds.clone();
    for edge in &graph.edges {
        if seeds.contains(edge.source_node_id.as_str())
            || edge
                .target_node_id
                .as_deref()
                .is_some_and(|id| seeds.contains(id))
        {
            ids.insert(edge.source_node_id.as_str());
            if let Some(target) = edge.target_node_id.as_deref() {
                ids.insert(target);
            }
        }
    }
    let mut paths = BTreeMap::<String, u8>::new();
    for node in graph
        .nodes
        .iter()
        .filter(|node| ids.contains(node.node_id.as_str()))
    {
        let score = if seeds.contains(node.node_id.as_str()) {
            1
        } else {
            3
        };
        if let Some(path) = node.file_path.as_deref() {
            add_ranked_path(&mut paths, path, score);
        }
        if let Some(range) = &node.source_evidence {
            add_ranked_path(&mut paths, &range.relative_path, score);
        }
    }
    for edge in &graph.edges {
        if seeds.contains(edge.source_node_id.as_str())
            || edge
                .target_node_id
                .as_deref()
                .is_some_and(|id| seeds.contains(id))
        {
            let score = match edge.edge_type {
                crate::knowledge_graph::GraphEdgeType::Calls
                | crate::knowledge_graph::GraphEdgeType::Injects
                | crate::knowledge_graph::GraphEdgeType::Exposes
                | crate::knowledge_graph::GraphEdgeType::UsesDto
                | crate::knowledge_graph::GraphEdgeType::Accesses
                | crate::knowledge_graph::GraphEdgeType::Controls
                | crate::knowledge_graph::GraphEdgeType::Throws => 5,
                crate::knowledge_graph::GraphEdgeType::References
                | crate::knowledge_graph::GraphEdgeType::Imports
                | crate::knowledge_graph::GraphEdgeType::Implements
                | crate::knowledge_graph::GraphEdgeType::Extends
                | crate::knowledge_graph::GraphEdgeType::Inherits => 4,
                _ => 2,
            };
            add_ranked_path(&mut paths, &edge.evidence.relative_path, score);
        }
    }
    paths.remove(selected_path);
    let mut ranked: Vec<_> = paths.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked
}

fn add_ranked_path(paths: &mut BTreeMap<String, u8>, path: &str, score: u8) {
    let path = normalize_graph_path(path);
    paths
        .entry(path)
        .and_modify(|current| *current = (*current).max(score))
        .or_insert(score);
}

fn read_source(root: &Path, relative: &str, max_bytes: u64) -> Result<String, String> {
    let relative = normalize_relative(relative).map_err(|_| "잘못된 상대 경로".to_owned())?;
    if is_sensitive_path(&relative) {
        return Err("민감 정보 파일을 제외했습니다.".to_owned());
    }
    let candidate = root.join(&relative);
    let canonical = fs::canonicalize(candidate)
        .map_err(|_| "파일을 찾을 수 없거나 읽을 수 없습니다.".to_owned())?;
    if !canonical.starts_with(root) {
        return Err("프로젝트 외부 경로는 제외했습니다.".to_owned());
    }
    let metadata =
        fs::metadata(&canonical).map_err(|_| "파일 정보를 읽을 수 없습니다.".to_owned())?;
    if !metadata.is_file() {
        return Err("일반 파일이 아닌 경로는 제외했습니다.".to_owned());
    }
    if metadata.len() > max_bytes {
        return Err("파일별 크기 제한을 초과했습니다.".to_owned());
    }
    let mut source = String::new();
    fs::File::open(canonical)
        .and_then(|file| file.take(max_bytes + 1).read_to_string(&mut source))
        .map_err(|_| "UTF-8 텍스트로 읽을 수 없습니다.".to_owned())?;
    if source.len() as u64 > max_bytes {
        return Err("읽는 동안 파일 크기가 제한을 초과했습니다.".to_owned());
    }
    Ok(source)
}

fn read_applicable_rules(
    root: &Path,
    selected_path: &str,
    omissions: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let mut rules = BTreeMap::new();
    let mut directories = vec![root.to_path_buf()];
    let relative = Path::new(selected_path);
    let mut parent = relative.parent();
    let mut nested = Vec::new();
    while let Some(path) = parent {
        if path.as_os_str().is_empty() {
            break;
        }
        nested.push(root.join(path));
        parent = path.parent();
    }
    nested.reverse();
    directories.extend(nested);
    for directory in directories {
        let path = directory.join("AGENTS.md");
        if !path.exists() {
            continue;
        }
        let Ok(canonical) = fs::canonicalize(&path) else {
            omissions.push("일부 AGENTS.md 규칙 파일을 읽을 수 없습니다.".to_owned());
            continue;
        };
        if !canonical.starts_with(root) {
            omissions
                .push("프로젝트 외부를 가리키는 AGENTS.md 규칙 파일을 제외했습니다.".to_owned());
            continue;
        }
        let relative = canonical
            .strip_prefix(root)
            .ok()
            .map(|path| normalize_graph_path(&path.to_string_lossy()))
            .unwrap_or_default();
        match fs::metadata(&canonical) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_RULE_BYTES => {
                let mut rule = String::new();
                let read_result = fs::File::open(canonical)
                    .and_then(|file| file.take(MAX_RULE_BYTES + 1).read_to_string(&mut rule));
                match read_result {
                    Ok(_) if rule.len() as u64 <= MAX_RULE_BYTES => {
                        rules.insert(relative, rule);
                    }
                    Ok(_) => omissions.push(format!(
                        "{relative}: 규칙 파일 크기 제한을 초과하여 생략했습니다."
                    )),
                    Err(_) => omissions.push(format!(
                        "{relative}: 규칙 파일을 UTF-8 텍스트로 읽을 수 없습니다."
                    )),
                }
            }
            Ok(_) => omissions.push(format!(
                "{relative}: 규칙 파일 크기 또는 종류 제한으로 생략했습니다."
            )),
            Err(_) => {
                omissions.push("일부 AGENTS.md 규칙 파일의 정보를 읽을 수 없습니다.".to_owned())
            }
        }
    }
    rules
}

fn graph_languages(graph: &GraphSnapshot) -> Vec<String> {
    let mut languages = BTreeSet::new();
    for node in &graph.nodes {
        if let Some(language) = node.language {
            languages.insert(format!("{:?}", language));
        }
    }
    languages.into_iter().collect()
}

fn normalize_relative(path: &str) -> Result<String, CommandError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(
            "선택 범위에는 프로젝트 내부의 상대 경로가 필요합니다.",
        ));
    }
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn normalize_graph_path(path: &str) -> String {
    path.replace('\\', "/")
}
fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}
fn internal(message: String) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
