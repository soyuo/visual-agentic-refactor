use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai_provider::ReviewSelection;
use crate::analyzer::SourceRange;
use crate::context_builder::RenderedSourceSpan;
use crate::context_builder::is_sensitive_path;
use crate::knowledge_graph::GraphSnapshot;

/// Provider-neutral finding categories accepted from structured AI responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FindingCategory {
    Correctness,
    Security,
    Performance,
    Reliability,
    Maintainability,
    Testability,
    Style,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Informational,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingConfidence {
    High,
    Medium,
    Low,
}

/// Provider-neutral shape decoded before source and snapshot validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewFinding {
    pub title: String,
    pub description: String,
    pub category: FindingCategory,
    pub severity: FindingSeverity,
    pub confidence: FindingConfidence,
    pub evidence: Vec<FindingEvidence>,
    pub affected_symbols: Vec<String>,
    pub suggestion: String,
    pub test_suggestion: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewFindings {
    pub findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindingEvidence {
    pub relative_path: String,
    /// One-based Unicode-scalar position; the end position is exclusive.
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// Exact source text between the claimed start and end positions.
    pub excerpt: String,
}

const MAX_FINDINGS: usize = 100;
const MAX_EVIDENCE_PER_FINDING: usize = 8;
const MAX_SYMBOLS_PER_FINDING: usize = 16;
const MAX_SOURCE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INCLUDED_FILES: usize = 25;
const MAX_INCLUDED_SOURCE_SPANS: usize = 400;
const MAX_RENDERED_CONTEXT_BYTES: usize = 32_000;
const MAX_EXCERPT_CHARS: usize = 16_000;
const MAX_SYMBOL_CHARS: usize = 512;
const MAX_PATH_CHARS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FindingValidationState {
    Complete,
    Partial,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FindingValidationCode {
    InvalidStructuredOutput,
    TooManyFindings,
    InvalidFindingSchema,
    MissingEvidence,
    InvalidPath,
    OutsideSelectedContext,
    MissingSnapshotEvidence,
    SourceUnavailable,
    SourceChanged,
    SourceLimitExceeded,
    InvalidRange,
    ExcerptMismatch,
    SymbolNotFound,
    InvalidSelection,
    SnapshotMismatch,
    ProjectMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingValidationDiagnostic {
    pub finding_index: Option<usize>,
    pub code: FindingValidationCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingValidationResult {
    pub state: FindingValidationState,
    pub snapshot_id: Option<String>,
    /// Only findings whose complete evidence passed validation are returned.
    pub findings: Vec<ReviewFinding>,
    pub diagnostics: Vec<FindingValidationDiagnostic>,
}

/// Validate provider output against the exact context preview, graph snapshot,
/// and current source files. Invalid findings are returned only as safe diagnostics.
pub fn validate_provider_output(
    structured_output: &Value,
    project_root: &Path,
    snapshot: &GraphSnapshot,
    expected_snapshot_id: &str,
    selection: &ReviewSelection,
    included_files: &[String],
    included_source_spans: &[RenderedSourceSpan],
) -> FindingValidationResult {
    let mut findings = Vec::new();
    let mut diagnostics = Vec::new();
    let root = match fs::canonicalize(project_root) {
        Ok(path) if path.is_dir() => path,
        _ => {
            return invalid_result(
                None,
                FindingValidationCode::ProjectMismatch,
                "프로젝트 경로를 확인할 수 없습니다.",
            );
        }
    };
    let graph_root = match fs::canonicalize(&snapshot.project_path) {
        Ok(path) => path,
        Err(_) => {
            return invalid_result(
                None,
                FindingValidationCode::ProjectMismatch,
                "분석 스냅샷의 프로젝트 경로를 확인할 수 없습니다.",
            );
        }
    };
    if root != graph_root {
        return invalid_result(
            None,
            FindingValidationCode::ProjectMismatch,
            "분석 스냅샷이 현재 프로젝트에 속하지 않습니다.",
        );
    }
    if snapshot.summary.snapshot_id != expected_snapshot_id {
        return invalid_result(
            None,
            FindingValidationCode::SnapshotMismatch,
            "컨텍스트 미리보기와 분석 스냅샷이 일치하지 않습니다.",
        );
    }
    if json_value_size(structured_output, 0) > 1024 * 1024 {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::InvalidStructuredOutput,
            "구조화 응답이 검증 크기 한도를 초과했습니다.",
        );
    }

    let included: BTreeSet<String> = included_files
        .iter()
        .filter_map(|path| normalize_relative_path(path))
        .collect();
    if included.len() > MAX_INCLUDED_FILES {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::SourceLimitExceeded,
            "컨텍스트 포함 파일 수가 검증 한도를 초과했습니다.",
        );
    }
    if included_source_spans.len() > MAX_INCLUDED_SOURCE_SPANS {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::SourceLimitExceeded,
            "문맥에 포함된 소스 범위 수가 검증 한도를 초과했습니다.",
        );
    }
    let mut rendered_text_bytes = 0usize;
    let mut rendered_spans: BTreeMap<String, Vec<(u32, u32, String)>> = BTreeMap::new();
    for span in included_source_spans {
        let Some(path) = normalize_relative_path(&span.relative_path) else {
            return invalid_result(
                Some(snapshot.summary.snapshot_id.clone()),
                FindingValidationCode::InvalidPath,
                "문맥에 포함된 소스 범위의 경로가 올바르지 않습니다.",
            );
        };
        if span.start_line == 0 || span.end_line < span.start_line || !included.contains(&path) {
            return invalid_result(
                Some(snapshot.summary.snapshot_id.clone()),
                FindingValidationCode::InvalidRange,
                "문맥에 포함된 소스 범위가 올바르지 않습니다.",
            );
        }
        rendered_text_bytes = rendered_text_bytes.saturating_add(span.visible_text.len());
        if rendered_text_bytes > MAX_RENDERED_CONTEXT_BYTES {
            return invalid_result(
                Some(snapshot.summary.snapshot_id.clone()),
                FindingValidationCode::SourceLimitExceeded,
                "렌더링된 소스 문맥이 검증 한도를 초과했습니다.",
            );
        }
        rendered_spans.entry(path).or_default().push((
            span.start_line,
            span.end_line,
            span.visible_text.clone(),
        ));
    }
    let selected = match selection_path(selection, snapshot) {
        Ok(path) => path,
        Err(code) => {
            return invalid_result(
                Some(snapshot.summary.snapshot_id.clone()),
                code,
                "선택 범위를 최신 분석 스냅샷에서 확인할 수 없습니다.",
            );
        }
    };
    if !included.contains(&selected) {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::InvalidSelection,
            "선택한 소스가 컨텍스트 미리보기에 포함되지 않았습니다.",
        );
    }
    let selected_source = match read_verified_source(&root, &selected, snapshot) {
        Ok(source) => source,
        Err(code) => {
            return invalid_result(
                Some(snapshot.summary.snapshot_id.clone()),
                code,
                "선택한 소스를 스냅샷과 대조할 수 없습니다.",
            );
        }
    };
    let selected_range = match selection_range(selection, &selected_source, &selected, snapshot) {
        Ok(range) => range,
        Err(()) => {
            return invalid_result(
                Some(snapshot.summary.snapshot_id.clone()),
                FindingValidationCode::InvalidRange,
                "선택 범위가 현재 소스와 일치하지 않습니다.",
            );
        }
    };
    let mut source_cache = BTreeMap::from([(selected.clone(), selected_source.clone())]);
    let mut total_source_bytes = selected_source.len() as u64;

    let Some(output) = structured_output.as_object() else {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::InvalidStructuredOutput,
            "응답이 finding 객체 형식이 아닙니다.",
        );
    };
    let Some(items) = output.get("findings").and_then(Value::as_array) else {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::InvalidStructuredOutput,
            "응답에 findings 배열이 없습니다.",
        );
    };
    if output.len() != 1 {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::InvalidStructuredOutput,
            "응답에 허용되지 않은 최상위 필드가 있습니다.",
        );
    }
    if items.len() > MAX_FINDINGS {
        return invalid_result(
            Some(snapshot.summary.snapshot_id.clone()),
            FindingValidationCode::TooManyFindings,
            "응답 finding 수가 허용 한도를 초과했습니다.",
        );
    }

    for (index, item) in items.iter().enumerate() {
        let finding = match serde_json::from_value::<ReviewFinding>(item.clone()) {
            Ok(finding) => finding,
            Err(_) => {
                diagnostics.push(diagnostic(
                    Some(index),
                    FindingValidationCode::InvalidFindingSchema,
                    "finding 필드가 공통 형식과 일치하지 않습니다.",
                ));
                continue;
            }
        };
        let code = validate_finding(
            &finding,
            &root,
            snapshot,
            &included,
            &rendered_spans,
            &selected,
            selected_range,
            &mut source_cache,
            &mut total_source_bytes,
        );
        if let Some(code) = code {
            diagnostics.push(diagnostic(Some(index), code, message_for(code)));
        } else {
            findings.push(finding);
        }
    }

    let state = if diagnostics.is_empty() {
        FindingValidationState::Complete
    } else if findings.is_empty() {
        FindingValidationState::Invalid
    } else {
        FindingValidationState::Partial
    };
    FindingValidationResult {
        state,
        snapshot_id: Some(snapshot.summary.snapshot_id.clone()),
        findings,
        diagnostics,
    }
}

fn validate_finding(
    finding: &ReviewFinding,
    root: &Path,
    snapshot: &GraphSnapshot,
    included: &BTreeSet<String>,
    rendered_spans: &BTreeMap<String, Vec<(u32, u32, String)>>,
    selected_path: &str,
    selected_range: Option<(usize, usize)>,
    source_cache: &mut BTreeMap<String, String>,
    total_source_bytes: &mut u64,
) -> Option<FindingValidationCode> {
    if finding.title.chars().count() > 200
        || finding.title.trim().is_empty()
        || finding.description.chars().count() > 8_000
        || finding.description.trim().is_empty()
        || finding.suggestion.chars().count() > 4_000
        || finding.test_suggestion.chars().count() > 4_000
        || finding.evidence.len() > MAX_EVIDENCE_PER_FINDING
        || finding.affected_symbols.len() > MAX_SYMBOLS_PER_FINDING
        || finding
            .affected_symbols
            .iter()
            .any(|symbol| symbol.is_empty() || symbol.chars().count() > MAX_SYMBOL_CHARS)
    {
        return Some(FindingValidationCode::InvalidFindingSchema);
    }
    if finding.evidence.is_empty() {
        return Some(FindingValidationCode::MissingEvidence);
    }

    let mut evidence_paths = BTreeSet::new();
    for evidence in &finding.evidence {
        if evidence.relative_path.chars().count() > MAX_PATH_CHARS
            || evidence.excerpt.chars().count() > MAX_EXCERPT_CHARS
        {
            return Some(FindingValidationCode::InvalidFindingSchema);
        }
        let Some(path) = normalize_relative_path(&evidence.relative_path) else {
            return Some(FindingValidationCode::InvalidPath);
        };
        if is_sensitive_path(&path) {
            return Some(FindingValidationCode::OutsideSelectedContext);
        }
        if !included.contains(&path) {
            return Some(FindingValidationCode::OutsideSelectedContext);
        }
        if !source_cache.contains_key(&path) {
            let source = match read_verified_source(root, &path, snapshot) {
                Ok(source) => source,
                Err(code) => return Some(code),
            };
            *total_source_bytes = total_source_bytes.saturating_add(source.len() as u64);
            if *total_source_bytes > MAX_TOTAL_SOURCE_BYTES {
                return Some(FindingValidationCode::SourceLimitExceeded);
            }
            source_cache.insert(path.clone(), source);
        }
        let Some(source) = source_cache.get(&path) else {
            return Some(FindingValidationCode::SourceUnavailable);
        };
        let Some(start) =
            line_column_to_offset(&source, evidence.start_line, evidence.start_column)
        else {
            return Some(FindingValidationCode::InvalidRange);
        };
        let Some(end) = line_column_to_offset(&source, evidence.end_line, evidence.end_column)
        else {
            return Some(FindingValidationCode::InvalidRange);
        };
        if end <= start {
            return Some(FindingValidationCode::InvalidRange);
        }
        if source.get(start..end) != Some(evidence.excerpt.as_str()) {
            return Some(FindingValidationCode::ExcerptMismatch);
        }
        let last_evidence_line =
            if evidence.end_column == 1 && evidence.end_line > evidence.start_line {
                evidence.end_line - 1
            } else {
                evidence.end_line
            };
        let was_rendered = rendered_spans.get(&path).is_some_and(|spans| {
            spans.iter().any(|(first, last, visible_text)| {
                evidence.start_line >= *first
                    && last_evidence_line <= *last
                    && excerpt_is_rendered(
                        evidence.start_line,
                        last_evidence_line,
                        &evidence.excerpt,
                        *first,
                        visible_text,
                    )
            })
        });
        if !was_rendered {
            return Some(FindingValidationCode::OutsideSelectedContext);
        }
        if let Some((scope_start, scope_end)) = selected_range {
            if path != selected_path || start < scope_start || end > scope_end {
                return Some(FindingValidationCode::OutsideSelectedContext);
            }
        }
        evidence_paths.insert(path);
    }

    for symbol in &finding.affected_symbols {
        let found = snapshot.nodes.iter().any(|node| {
            let node_path = node
                .file_path
                .as_deref()
                .or_else(|| {
                    node.source_evidence
                        .as_ref()
                        .map(|range| range.relative_path.as_str())
                })
                .and_then(normalize_relative_path);
            node_path.is_some_and(|path| evidence_paths.contains(&path))
                && (node.name == *symbol || node.qualified_name.as_deref() == Some(symbol))
        });
        if !found {
            return Some(FindingValidationCode::SymbolNotFound);
        }
    }

    None
}

fn excerpt_is_rendered(
    start_line: u32,
    end_line: u32,
    excerpt: &str,
    span_start_line: u32,
    visible_text: &str,
) -> bool {
    let visible_lines = visible_text.split('\n').collect::<Vec<_>>();
    let excerpt_lines = excerpt.split('\n').collect::<Vec<_>>();
    (start_line..=end_line).all(|line| {
        let Some(visible_line) = visible_lines.get((line - span_start_line) as usize) else {
            return false;
        };
        let Some(excerpt_line) = excerpt_lines.get((line - start_line) as usize) else {
            return false;
        };
        let excerpt_line = excerpt_line.strip_suffix('\r').unwrap_or(excerpt_line);
        visible_line.contains(excerpt_line)
    })
}

fn selection_path(
    selection: &ReviewSelection,
    snapshot: &GraphSnapshot,
) -> Result<String, FindingValidationCode> {
    match selection {
        ReviewSelection::File { relative_path, .. } => {
            normalize_relative_path(relative_path).ok_or(FindingValidationCode::InvalidSelection)
        }
        ReviewSelection::GraphNode { node_id, .. } => {
            let node = snapshot
                .nodes
                .iter()
                .find(|node| node.node_id == *node_id)
                .ok_or(FindingValidationCode::InvalidSelection)?;
            let file_path = node
                .file_path
                .as_deref()
                .or_else(|| {
                    node.source_evidence
                        .as_ref()
                        .map(|range| range.relative_path.as_str())
                })
                .ok_or(FindingValidationCode::InvalidSelection)?;
            normalize_relative_path(file_path).ok_or(FindingValidationCode::InvalidPath)
        }
    }
}

fn selection_range(
    selection: &ReviewSelection,
    selected_source: &str,
    selected_path: &str,
    snapshot: &GraphSnapshot,
) -> Result<Option<(usize, usize)>, ()> {
    match selection {
        ReviewSelection::File { range, .. } => range
            .as_ref()
            .map(|range| source_range_offsets(range, selected_source, selected_path))
            .transpose(),
        ReviewSelection::GraphNode { node_id, range, .. } => {
            let node = snapshot
                .nodes
                .iter()
                .find(|node| node.node_id == *node_id)
                .ok_or(())?;
            let expected = node.source_evidence.as_ref().ok_or(())?;
            let expected_offsets = source_range_offsets(expected, selected_source, selected_path)?;
            if let Some(range) = range {
                let provided_offsets = source_range_offsets(range, selected_source, selected_path)?;
                if provided_offsets != expected_offsets {
                    return Err(());
                }
            }
            Ok(Some(expected_offsets))
        }
    }
}

fn source_range_offsets(
    range: &SourceRange,
    source: &str,
    selected_path: &str,
) -> Result<(usize, usize), ()> {
    if normalize_relative_path(&range.relative_path).as_deref() != Some(selected_path) {
        return Err(());
    }
    let start = line_column_to_offset(source, range.start_line, range.start_column).ok_or(())?;
    let end = line_column_to_offset(source, range.end_line, range.end_column).ok_or(())?;
    if start >= end || start != range.start_byte || end != range.end_byte {
        return Err(());
    }
    Ok((start, end))
}

fn read_verified_source(
    root: &Path,
    relative: &str,
    snapshot: &GraphSnapshot,
) -> Result<String, FindingValidationCode> {
    let relative = normalize_relative_path(relative).ok_or(FindingValidationCode::InvalidPath)?;
    if is_sensitive_path(&relative) {
        return Err(FindingValidationCode::OutsideSelectedContext);
    }
    let expected_hash = snapshot
        .summary
        .source_hashes
        .iter()
        .find_map(|(path, hash)| {
            (normalize_relative_path(path).as_deref() == Some(relative.as_str())).then_some(hash)
        })
        .ok_or(FindingValidationCode::MissingSnapshotEvidence)?;
    let canonical = fs::canonicalize(root.join(PathBuf::from(&relative)))
        .map_err(|_| FindingValidationCode::SourceUnavailable)?;
    if !canonical.starts_with(root) {
        return Err(FindingValidationCode::InvalidPath);
    }
    let metadata =
        fs::metadata(&canonical).map_err(|_| FindingValidationCode::SourceUnavailable)?;
    if !metadata.is_file() {
        return Err(FindingValidationCode::SourceUnavailable);
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(FindingValidationCode::SourceLimitExceeded);
    }
    let file = File::open(canonical).map_err(|_| FindingValidationCode::SourceUnavailable)?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FindingValidationCode::SourceUnavailable)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(FindingValidationCode::SourceLimitExceeded);
    }
    if content_hash(&bytes) != *expected_hash {
        return Err(FindingValidationCode::SourceChanged);
    }
    String::from_utf8(bytes).map_err(|_| FindingValidationCode::SourceUnavailable)
}

fn normalize_relative_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.contains(':')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    if Path::new(&normalized)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(normalized)
}

fn line_column_to_offset(source: &str, line: u32, column: u32) -> Option<usize> {
    if line == 0 || column == 0 {
        return None;
    }
    let mut line_start = 0usize;
    for _ in 1..line {
        let newline = source.get(line_start..)?.find('\n')?;
        line_start = line_start.checked_add(newline + 1)?;
    }
    let remaining = source.get(line_start..)?;
    let line_len = remaining.find('\n').unwrap_or(remaining.len());
    let text = remaining.get(..line_len)?;
    let character_count = text.chars().count();
    let column_offset = usize::try_from(column - 1).ok()?;
    let byte_offset = if column_offset == character_count {
        text.len()
    } else {
        text.char_indices().nth(column_offset)?.0
    };
    line_start.checked_add(byte_offset)
}

fn content_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn json_value_size(value: &Value, accumulated: usize) -> usize {
    if accumulated > 1024 * 1024 {
        return accumulated;
    }
    match value {
        Value::Null => accumulated.saturating_add(4),
        Value::Bool(_) => accumulated.saturating_add(5),
        Value::Number(number) => accumulated.saturating_add(number.to_string().len()),
        Value::String(text) => accumulated.saturating_add(text.len()),
        Value::Array(values) => values
            .iter()
            .fold(accumulated, |size, item| json_value_size(item, size)),
        Value::Object(fields) => fields.iter().fold(accumulated, |size, (key, item)| {
            json_value_size(item, size.saturating_add(key.len()))
        }),
    }
}

fn invalid_result(
    snapshot_id: Option<String>,
    code: FindingValidationCode,
    message: &str,
) -> FindingValidationResult {
    FindingValidationResult {
        state: FindingValidationState::Invalid,
        snapshot_id,
        findings: Vec::new(),
        diagnostics: vec![diagnostic(None, code, message)],
    }
}

fn diagnostic(
    finding_index: Option<usize>,
    code: FindingValidationCode,
    message: &str,
) -> FindingValidationDiagnostic {
    FindingValidationDiagnostic {
        finding_index,
        code,
        message: message.to_owned(),
    }
}

fn message_for(code: FindingValidationCode) -> &'static str {
    match code {
        FindingValidationCode::InvalidStructuredOutput => "응답 구조를 확인할 수 없습니다.",
        FindingValidationCode::TooManyFindings => "응답 finding 수가 허용 한도를 초과했습니다.",
        FindingValidationCode::InvalidFindingSchema => {
            "finding 필드가 공통 형식과 일치하지 않습니다."
        }
        FindingValidationCode::MissingEvidence => "finding에 검증 가능한 소스 근거가 없습니다.",
        FindingValidationCode::InvalidPath => "근거 경로가 프로젝트 내 상대 경로가 아닙니다.",
        FindingValidationCode::OutsideSelectedContext => {
            "근거가 선택한 문맥 범위에 포함되지 않습니다."
        }
        FindingValidationCode::MissingSnapshotEvidence => "근거 파일이 분석 스냅샷에 없습니다.",
        FindingValidationCode::SourceUnavailable => "근거 파일을 안전한 텍스트로 읽을 수 없습니다.",
        FindingValidationCode::SourceChanged => "현재 파일 내용이 분석 스냅샷과 다릅니다.",
        FindingValidationCode::SourceLimitExceeded => {
            "컨텍스트 소스 크기가 검증 한도를 초과했습니다."
        }
        FindingValidationCode::InvalidRange => "근거 위치가 소스 범위를 벗어났습니다.",
        FindingValidationCode::ExcerptMismatch => {
            "인용한 근거가 해당 소스 위치와 일치하지 않습니다."
        }
        FindingValidationCode::SymbolNotFound => "영향 심볼이 근거 파일의 분석 스냅샷에 없습니다.",
        FindingValidationCode::InvalidSelection => {
            "선택 범위를 최신 분석 스냅샷에서 찾을 수 없습니다."
        }
        FindingValidationCode::SnapshotMismatch => "컨텍스트와 분석 스냅샷이 일치하지 않습니다.",
        FindingValidationCode::ProjectMismatch => {
            "분석 스냅샷이 현재 프로젝트와 일치하지 않습니다."
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        FindingCategory, FindingConfidence, FindingSeverity, FindingValidationCode,
        FindingValidationState, ReviewFindings, content_hash, validate_provider_output,
    };
    use crate::ai_provider::ReviewSelection;
    use crate::context_builder::RenderedSourceSpan;
    use crate::knowledge_graph::GraphSnapshot;
    use serde_json::json;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    struct ProjectFixture {
        root: PathBuf,
        snapshot: GraphSnapshot,
    }

    impl ProjectFixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "visual-agentic-finding-validation-{}-{id}",
                std::process::id()
            ));
            let source_dir = root.join("src");
            fs::create_dir_all(&source_dir).expect("fixture directory should be created");
            let cart = "class Cart {\n  val total = 1\n}\n";
            let other = "class Other {\n  val value = 2\n}\n";
            fs::write(source_dir.join("Cart.kt"), cart).expect("Cart fixture should be written");
            fs::write(source_dir.join("Other.kt"), other).expect("Other fixture should be written");
            let cart_start = cart.find("total").expect("Cart symbol should exist");
            let other_start = other.find("value").expect("Other symbol should exist");
            let snapshot = serde_json::from_value(json!({
                "projectPath": root.to_string_lossy(),
                "snapshotId": "snapshot-1",
                "headSha": null,
                "analyzerVersion": "fixture",
                "sourceHashes": {
                    "src/Cart.kt": content_hash(cart.as_bytes()),
                    "src/Other.kt": content_hash(other.as_bytes())
                },
                "settings": {},
                "completeness": "complete",
                "analyzedAtUnixMs": 1,
                "nodes": [
                    {
                        "nodeId": "symbol:Cart.total",
                        "nodeType": "symbol",
                        "name": "total",
                        "qualifiedName": "Cart.total",
                        "filePath": "src/Cart.kt",
                        "sourceEvidence": {
                            "relativePath": "src/Cart.kt",
                            "startByte": cart_start,
                            "endByte": cart_start + "total".len(),
                            "startLine": 2,
                            "startColumn": 7,
                            "endLine": 2,
                            "endColumn": 12
                        },
                        "language": "kotlin",
                        "metadata": {},
                        "confidence": "confirmed"
                    },
                    {
                        "nodeId": "symbol:Other.value",
                        "nodeType": "symbol",
                        "name": "value",
                        "qualifiedName": "Other.value",
                        "filePath": "src/Other.kt",
                        "sourceEvidence": {
                            "relativePath": "src/Other.kt",
                            "startByte": other_start,
                            "endByte": other_start + "value".len(),
                            "startLine": 2,
                            "startColumn": 7,
                            "endLine": 2,
                            "endColumn": 12
                        },
                        "language": "kotlin",
                        "metadata": {},
                        "confidence": "confirmed"
                    }
                ],
                "edges": [],
                "diagnostics": []
            }))
            .expect("fixture graph snapshot should deserialize");
            Self { root, snapshot }
        }

        fn validate(
            &self,
            output: serde_json::Value,
            included: &[&str],
        ) -> super::FindingValidationResult {
            let selection = ReviewSelection::File {
                relative_path: "src/Cart.kt".to_owned(),
                range: None,
            };
            self.validate_with_selection(output, included, selection)
        }

        fn validate_with_selection(
            &self,
            output: serde_json::Value,
            included: &[&str],
            selection: ReviewSelection,
        ) -> super::FindingValidationResult {
            let included: Vec<String> = included.iter().map(|path| (*path).to_owned()).collect();
            let spans = included
                .iter()
                .map(|relative_path| RenderedSourceSpan {
                    relative_path: relative_path.clone(),
                    start_line: 1,
                    end_line: 100,
                    visible_text: match relative_path.as_str() {
                        "src/Cart.kt" => "class Cart {\n  val total = 1\n}".to_owned(),
                        "src/Other.kt" => "class Other {\n  val value = 2\n}".to_owned(),
                        _ => String::new(),
                    },
                })
                .collect::<Vec<_>>();
            self.validate_with_spans(output, included, spans, selection)
        }

        fn validate_with_spans(
            &self,
            output: serde_json::Value,
            included: Vec<String>,
            spans: Vec<RenderedSourceSpan>,
            selection: ReviewSelection,
        ) -> super::FindingValidationResult {
            validate_provider_output(
                &output,
                &self.root,
                &self.snapshot,
                "snapshot-1",
                &selection,
                &included,
                &spans,
            )
        }
    }

    impl Drop for ProjectFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn finding(path: &str, symbol: &str, excerpt: &str) -> serde_json::Value {
        json!({
            "title": "Review finding",
            "description": "A verified source claim.",
            "category": "correctness",
            "severity": "medium",
            "confidence": "high",
            "evidence": [{
                "relativePath": path,
                "startLine": 2,
                "startColumn": 7,
                "endLine": 2,
                "endColumn": 12,
                "excerpt": excerpt
            }],
            "affectedSymbols": [symbol],
            "suggestion": "Review the source behavior.",
            "testSuggestion": "Add a focused test."
        })
    }

    #[test]
    fn deserializes_provider_neutral_finding_contract() {
        let response = serde_json::from_value::<ReviewFindings>(json!({
            "findings": [{
                "title": "Check null handling",
                "description": "The value is dereferenced without a guard.",
                "category": "correctness",
                "severity": "high",
                "confidence": "medium",
                "evidence": [{
                    "relativePath": "src/Cart.kt",
                    "startLine": 3,
                    "startColumn": 5,
                    "endLine": 3,
                    "endColumn": 16,
                    "excerpt": "item.price"
                }],
                "affectedSymbols": ["Cart.total"],
                "suggestion": "Guard the value before dereferencing it.",
                "testSuggestion": "Add a test for a missing item."
            }]
        }))
        .expect("provider contract fixture should deserialize");

        let finding = &response.findings[0];
        assert_eq!(finding.category, FindingCategory::Correctness);
        assert_eq!(finding.severity, FindingSeverity::High);
        assert_eq!(finding.confidence, FindingConfidence::Medium);
        assert_eq!(finding.evidence[0].start_column, 5);
        assert_eq!(finding.evidence[0].end_column, 16);
    }

    #[test]
    fn accepts_finding_with_current_source_and_snapshot_evidence() {
        let fixture = ProjectFixture::new();
        let result = fixture.validate(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "total")]}),
            &["src/Cart.kt"],
        );

        assert_eq!(result.state, FindingValidationState::Complete);
        assert_eq!(result.findings.len(), 1);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.snapshot_id.as_deref(), Some("snapshot-1"));
    }

    #[test]
    fn rejects_invalid_and_out_of_scope_paths() {
        let fixture = ProjectFixture::new();
        let invalid_path = fixture.validate(
            json!({"findings": [finding("../src/Cart.kt", "Cart.total", "total")]}),
            &["src/Cart.kt"],
        );
        assert_eq!(invalid_path.state, FindingValidationState::Invalid);
        assert_eq!(
            invalid_path.diagnostics[0].code,
            FindingValidationCode::InvalidPath
        );

        let outside_context = fixture.validate(
            json!({"findings": [finding("src/Other.kt", "Other.value", "value")]}),
            &["src/Cart.kt"],
        );
        assert_eq!(outside_context.state, FindingValidationState::Invalid);
        assert_eq!(
            outside_context.diagnostics[0].code,
            FindingValidationCode::OutsideSelectedContext
        );
    }

    #[test]
    fn rejects_evidence_outside_the_rendered_source_spans() {
        let fixture = ProjectFixture::new();
        let selection = ReviewSelection::File {
            relative_path: "src/Cart.kt".to_owned(),
            range: None,
        };
        let result = fixture.validate_with_spans(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "total")]}),
            vec!["src/Cart.kt".to_owned()],
            vec![RenderedSourceSpan {
                relative_path: "src/Cart.kt".to_owned(),
                start_line: 1,
                end_line: 1,
                visible_text: "class Cart {".to_owned(),
            }],
            selection,
        );

        assert_eq!(result.state, FindingValidationState::Invalid);
        assert_eq!(
            result.diagnostics[0].code,
            FindingValidationCode::OutsideSelectedContext
        );
    }

    #[test]
    fn rejects_evidence_text_missing_from_its_rendered_line() {
        let fixture = ProjectFixture::new();
        let selection = ReviewSelection::File {
            relative_path: "src/Cart.kt".to_owned(),
            range: None,
        };
        let result = fixture.validate_with_spans(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "total")]}),
            vec!["src/Cart.kt".to_owned()],
            vec![RenderedSourceSpan {
                relative_path: "src/Cart.kt".to_owned(),
                start_line: 1,
                end_line: 2,
                visible_text: "  val total = 1\nclass Cart {".to_owned(),
            }],
            selection,
        );

        assert_eq!(result.state, FindingValidationState::Invalid);
        assert_eq!(
            result.diagnostics[0].code,
            FindingValidationCode::OutsideSelectedContext
        );
    }

    #[test]
    fn graph_node_without_a_range_uses_snapshot_source_evidence() {
        let fixture = ProjectFixture::new();
        let selection = ReviewSelection::GraphNode {
            node_id: "symbol:Cart.total".to_owned(),
            label: "Cart.total".to_owned(),
            range: None,
        };
        let valid = fixture.validate_with_selection(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "total")]}),
            &["src/Cart.kt"],
            selection.clone(),
        );
        assert_eq!(valid.state, FindingValidationState::Complete);

        let mut outside_node_finding = finding("src/Cart.kt", "Cart.total", "Cart");
        outside_node_finding["evidence"][0]["startLine"] = json!(1);
        outside_node_finding["evidence"][0]["startColumn"] = json!(7);
        outside_node_finding["evidence"][0]["endLine"] = json!(1);
        outside_node_finding["evidence"][0]["endColumn"] = json!(11);
        let outside_node_range = fixture.validate_with_selection(
            json!({"findings": [outside_node_finding]}),
            &["src/Cart.kt"],
            selection,
        );
        assert_eq!(outside_node_range.state, FindingValidationState::Invalid);
        assert_eq!(
            outside_node_range.diagnostics[0].code,
            FindingValidationCode::OutsideSelectedContext
        );
    }

    #[test]
    fn rejects_nonexistent_affected_symbol() {
        let fixture = ProjectFixture::new();
        let result = fixture.validate(
            json!({"findings": [finding("src/Cart.kt", "Cart.missing", "total")]}),
            &["src/Cart.kt"],
        );

        assert_eq!(result.state, FindingValidationState::Invalid);
        assert_eq!(result.findings.len(), 0);
        assert_eq!(
            result.diagnostics[0].code,
            FindingValidationCode::SymbolNotFound
        );
    }

    #[test]
    fn validates_exact_range_against_selection_and_source_excerpt() {
        let fixture = ProjectFixture::new();
        let selected_range = crate::analyzer::SourceRange {
            relative_path: "src/Cart.kt".to_owned(),
            start_byte: 19,
            end_byte: 24,
            start_line: 2,
            start_column: 7,
            end_line: 2,
            end_column: 12,
        };
        let selection = ReviewSelection::File {
            relative_path: "src/Cart.kt".to_owned(),
            range: Some(selected_range),
        };
        let valid = fixture.validate_with_selection(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "total")]}),
            &["src/Cart.kt"],
            selection.clone(),
        );
        assert_eq!(valid.state, FindingValidationState::Complete);

        let mismatch = fixture.validate_with_selection(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "wrong")]}),
            &["src/Cart.kt"],
            selection,
        );
        assert_eq!(mismatch.state, FindingValidationState::Invalid);
        assert_eq!(
            mismatch.diagnostics[0].code,
            FindingValidationCode::ExcerptMismatch
        );
    }

    #[test]
    fn rejects_source_changed_after_snapshot() {
        let fixture = ProjectFixture::new();
        fs::write(
            fixture.root.join("src/Cart.kt"),
            "class Cart {\n  val total = 2\n}\n",
        )
        .expect("mutated fixture should be written");
        let result = fixture.validate(
            json!({"findings": [finding("src/Cart.kt", "Cart.total", "total")]}),
            &["src/Cart.kt"],
        );

        assert_eq!(result.state, FindingValidationState::Invalid);
        assert_eq!(
            result.diagnostics[0].code,
            FindingValidationCode::SourceChanged
        );
    }

    #[test]
    fn rejects_platform_independent_path_traversal_forms() {
        assert!(super::normalize_relative_path("..\\src\\Cart.kt").is_none());
        assert!(super::normalize_relative_path("C:/private/Cart.kt").is_none());
        assert!(super::normalize_relative_path("/src/Cart.kt").is_none());
        assert_eq!(
            super::normalize_relative_path("src\\Cart.kt").as_deref(),
            Some("src/Cart.kt")
        );
    }

    #[test]
    fn distinguishes_partial_findings_from_wholly_invalid_output() {
        let fixture = ProjectFixture::new();
        let partial = fixture.validate(
            json!({"findings": [
                finding("src/Cart.kt", "Cart.total", "total"),
                {"title": "Malformed finding"}
            ]}),
            &["src/Cart.kt"],
        );
        assert_eq!(partial.state, FindingValidationState::Partial);
        assert_eq!(partial.findings.len(), 1);
        assert_eq!(partial.diagnostics[0].finding_index, Some(1));

        let invalid = fixture.validate(json!({"notFindings": []}), &["src/Cart.kt"]);
        assert_eq!(invalid.state, FindingValidationState::Invalid);
        assert!(invalid.findings.is_empty());
        assert_eq!(
            invalid.diagnostics[0].code,
            FindingValidationCode::InvalidStructuredOutput
        );
    }
}
