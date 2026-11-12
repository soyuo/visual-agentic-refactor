use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analyzer::{Confidence, SourceLanguage, SourceRange};
use crate::persistence::ProjectDatabase;

const MAX_SNAPSHOT_ID_CHARS: usize = 256;
const MAX_NODE_ID_CHARS: usize = 512;
const MAX_EDGE_ID_CHARS: usize = 512;
const MAX_SNAPSHOT_NODES: usize = 100_000;
const MAX_SNAPSHOT_EDGES: usize = 250_000;
const MAX_SNAPSHOT_DIAGNOSTICS: usize = 50_000;
const MAX_SNAPSHOT_SOURCE_HASHES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraphNodeType {
    Project,
    Module,
    Directory,
    File,
    Package,
    Symbol,
    Class,
    Interface,
    Function,
    Method,
    ApiEndpoint,
    Dto,
    Dependency,
    ExternalService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraphEdgeType {
    Contains,
    Declares,
    Imports,
    DependsOn,
    Calls,
    Implements,
    Extends,
    Inherits,
    Injects,
    Exposes,
    UsesDto,
    Accesses,
    References,
    Controls,
    Throws,
    AnnotatedBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraphCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraphDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub node_id: String,
    pub node_type: GraphNodeType,
    pub name: String,
    pub qualified_name: Option<String>,
    pub file_path: Option<String>,
    pub source_evidence: Option<SourceRange>,
    pub language: Option<SourceLanguage>,
    #[serde(default)]
    pub metadata: Value,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub edge_id: String,
    pub edge_type: GraphEdgeType,
    pub source_node_id: String,
    pub target_node_id: Option<String>,
    pub target_name: Option<String>,
    pub evidence: SourceRange,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphDiagnostic {
    pub code: String,
    pub severity: GraphDiagnosticSeverity,
    pub message: String,
    pub source_evidence: Option<SourceRange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSnapshotInput {
    pub snapshot_id: String,
    pub head_sha: Option<String>,
    pub analyzer_version: String,
    pub source_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: Value,
    pub completeness: GraphCompleteness,
    pub analyzed_at_unix_ms: Option<i64>,
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub diagnostics: Vec<GraphDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSnapshotSummary {
    pub snapshot_id: String,
    pub head_sha: Option<String>,
    pub analyzer_version: String,
    pub source_hashes: BTreeMap<String, String>,
    pub settings: Value,
    pub completeness: GraphCompleteness,
    pub analyzed_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSnapshot {
    pub project_path: String,
    #[serde(flatten)]
    pub summary: GraphSnapshotSummary,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub diagnostics: Vec<GraphDiagnostic>,
}

/// Builds a deterministic ID from a node kind and a caller-supplied canonical key.
/// For example, use a normalized relative path for files and a qualified name plus
/// source path for declarations. The same key always produces the same ID.
pub fn stable_node_id(node_type: GraphNodeType, canonical_key: &str) -> Result<String, String> {
    if canonical_key.is_empty() {
        return Err("Graph node identity key must not be empty".to_owned());
    }
    let node_id = format!("{}:{canonical_key}", node_type.as_db_value());
    validate_id(&node_id, MAX_NODE_ID_CHARS, "node")?;
    Ok(node_id)
}

impl GraphSnapshotInput {
    pub fn stable_edge_id(
        source_node_id: &str,
        edge_type: GraphEdgeType,
        target_key: &str,
        evidence: &SourceRange,
    ) -> Result<String, String> {
        if source_node_id.is_empty() || target_key.is_empty() {
            return Err("Graph edge identity parts must not be empty".to_owned());
        }
        let identity = serde_json::to_string(&(
            edge_type.as_db_value(),
            source_node_id,
            target_key,
            &evidence.relative_path,
            evidence.start_byte,
        ))
        .map_err(|error| format!("Could not encode graph edge identity: {error}"))?;
        let edge_id = format!("edge:{identity}");
        validate_id(&edge_id, MAX_EDGE_ID_CHARS, "edge")?;
        Ok(edge_id)
    }
}

impl ProjectDatabase {
    /// Replaces one snapshot atomically. Other snapshots for the project are kept.
    pub fn write_graph_snapshot(
        &self,
        project_path: &str,
        snapshot: &GraphSnapshotInput,
    ) -> Result<(), String> {
        validate_snapshot(snapshot)?;
        let analyzed_at = snapshot
            .analyzed_at_unix_ms
            .unwrap_or(current_time_unix_ms()?);
        let source_hashes = serde_json::to_string(&snapshot.source_hashes)
            .map_err(|error| format!("Could not encode source hashes: {error}"))?;
        let settings = serde_json::to_string(&snapshot.settings)
            .map_err(|error| format!("Could not encode graph settings: {error}"))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "Local database lock is poisoned".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start graph transaction: {error}"))?;
        let project_id: i64 = transaction
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                [project_path],
                |row| row.get(0),
            )
            .map_err(|error| format!("Project is not registered for graph storage: {error}"))?;

        transaction
            .execute(
                "DELETE FROM graph_snapshots WHERE project_id = ?1 AND snapshot_id = ?2",
                params![project_id, snapshot.snapshot_id],
            )
            .map_err(|error| format!("Could not replace prior graph snapshot: {error}"))?;
        transaction
            .execute(
                "INSERT INTO graph_snapshots (
                    project_id, snapshot_id, head_sha, analyzer_version,
                    source_hashes_json, settings_json, completeness, analyzed_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    project_id,
                    snapshot.snapshot_id,
                    snapshot.head_sha,
                    snapshot.analyzer_version,
                    source_hashes,
                    settings,
                    snapshot.completeness.as_db_value(),
                    analyzed_at,
                ],
            )
            .map_err(|error| format!("Could not store graph snapshot metadata: {error}"))?;

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO graph_nodes (
                        project_id, snapshot_id, node_id, node_type, name, qualified_name,
                        file_path, source_evidence_json, language, metadata_json, confidence
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )
                .map_err(|error| format!("Could not prepare graph node insert: {error}"))?;
            for node in &snapshot.nodes {
                let source_evidence =
                    encode_optional_json(&node.source_evidence, "node source evidence")?;
                let metadata = serde_json::to_string(&node.metadata)
                    .map_err(|error| format!("Could not encode graph node metadata: {error}"))?;
                statement
                    .execute(params![
                        project_id,
                        snapshot.snapshot_id,
                        node.node_id,
                        node.node_type.as_db_value(),
                        node.name,
                        node.qualified_name,
                        node.file_path,
                        source_evidence,
                        node.language.map(SourceLanguage::as_db_value),
                        metadata,
                        node.confidence.as_db_value(),
                    ])
                    .map_err(|error| format!("Could not store graph node: {error}"))?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO graph_edges (
                        project_id, snapshot_id, edge_id, edge_type, source_node_id,
                        target_node_id, target_name, evidence_json, confidence
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|error| format!("Could not prepare graph edge insert: {error}"))?;
            for edge in &snapshot.edges {
                let evidence = encode_json(&edge.evidence, "edge evidence")?
                    .expect("required edge evidence serializes to JSON");
                statement
                    .execute(params![
                        project_id,
                        snapshot.snapshot_id,
                        edge.edge_id,
                        edge.edge_type.as_db_value(),
                        edge.source_node_id,
                        edge.target_node_id,
                        edge.target_name,
                        evidence,
                        edge.confidence.as_db_value(),
                    ])
                    .map_err(|error| format!("Could not store graph edge: {error}"))?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO graph_diagnostics (
                        project_id, snapshot_id, diagnostic_order, code, severity,
                        message, source_evidence_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(|error| format!("Could not prepare graph diagnostic insert: {error}"))?;
            for (order, diagnostic) in snapshot.diagnostics.iter().enumerate() {
                let source_evidence = encode_optional_json(
                    &diagnostic.source_evidence,
                    "diagnostic source evidence",
                )?;
                statement
                    .execute(params![
                        project_id,
                        snapshot.snapshot_id,
                        i64::try_from(order).map_err(|error| {
                            format!("Too many graph diagnostics to store: {error}")
                        })?,
                        diagnostic.code,
                        diagnostic.severity.as_db_value(),
                        diagnostic.message,
                        source_evidence,
                    ])
                    .map_err(|error| format!("Could not store graph diagnostic: {error}"))?;
            }
        }

        transaction
            .commit()
            .map_err(|error| format!("Could not commit graph snapshot: {error}"))
    }

    pub fn list_graph_snapshots(
        &self,
        project_path: &str,
    ) -> Result<Vec<GraphSnapshotSummary>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "Local database lock is poisoned".to_owned())?;
        let project_id: Option<i64> = connection
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                [project_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Could not locate project for graph query: {error}"))?;
        let Some(project_id) = project_id else {
            return Ok(Vec::new());
        };
        let mut statement = connection
            .prepare(
                "SELECT snapshot_id, head_sha, analyzer_version, source_hashes_json,
                        settings_json, completeness, analyzed_at_unix_ms
                 FROM graph_snapshots WHERE project_id = ?1
                 ORDER BY analyzed_at_unix_ms DESC, snapshot_id DESC",
            )
            .map_err(|error| format!("Could not prepare graph snapshot query: {error}"))?;
        statement
            .query_map([project_id], summary_from_row)
            .map_err(|error| format!("Could not query graph snapshots: {error}"))?
            .map(|row| row.map_err(|error| format!("Could not read graph snapshot: {error}")))
            .collect()
    }

    pub fn load_graph_snapshot(
        &self,
        project_path: &str,
        snapshot_id: &str,
    ) -> Result<Option<GraphSnapshot>, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "Local database lock is poisoned".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start graph query: {error}"))?;
        let project = transaction
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                [project_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("Could not locate project for graph query: {error}"))?;
        let Some(project_id) = project else {
            transaction
                .commit()
                .map_err(|error| format!("Could not finish graph query: {error}"))?;
            return Ok(None);
        };
        let summary = transaction
            .query_row(
                "SELECT snapshot_id, head_sha, analyzer_version, source_hashes_json,
                        settings_json, completeness, analyzed_at_unix_ms
                 FROM graph_snapshots WHERE project_id = ?1 AND snapshot_id = ?2",
                params![project_id, snapshot_id],
                summary_from_row,
            )
            .optional()
            .map_err(|error| format!("Could not query graph snapshot: {error}"))?;
        let Some(summary) = summary else {
            transaction
                .commit()
                .map_err(|error| format!("Could not finish graph query: {error}"))?;
            return Ok(None);
        };

        let nodes = {
            let mut statement = transaction
                .prepare(
                    "SELECT node_id, node_type, name, qualified_name, file_path,
                            source_evidence_json, language, metadata_json, confidence
                     FROM graph_nodes WHERE project_id = ?1 AND snapshot_id = ?2
                     ORDER BY node_id",
                )
                .map_err(|error| format!("Could not prepare graph node query: {error}"))?;
            statement
                .query_map(params![project_id, snapshot_id], node_from_row)
                .map_err(|error| format!("Could not query graph nodes: {error}"))?
                .map(|row| row.map_err(|error| format!("Could not read graph node: {error}")))
                .collect::<Result<Vec<_>, _>>()?
        };
        let edges = {
            let mut statement = transaction
                .prepare(
                    "SELECT edge_id, edge_type, source_node_id, target_node_id,
                            target_name, evidence_json, confidence
                     FROM graph_edges WHERE project_id = ?1 AND snapshot_id = ?2
                     ORDER BY edge_id",
                )
                .map_err(|error| format!("Could not prepare graph edge query: {error}"))?;
            statement
                .query_map(params![project_id, snapshot_id], edge_from_row)
                .map_err(|error| format!("Could not query graph edges: {error}"))?
                .map(|row| row.map_err(|error| format!("Could not read graph edge: {error}")))
                .collect::<Result<Vec<_>, _>>()?
        };
        let diagnostics = {
            let mut statement = transaction
                .prepare(
                    "SELECT code, severity, message, source_evidence_json
                     FROM graph_diagnostics WHERE project_id = ?1 AND snapshot_id = ?2
                     ORDER BY diagnostic_order",
                )
                .map_err(|error| format!("Could not prepare graph diagnostic query: {error}"))?;
            statement
                .query_map(params![project_id, snapshot_id], diagnostic_from_row)
                .map_err(|error| format!("Could not query graph diagnostics: {error}"))?
                .map(|row| row.map_err(|error| format!("Could not read graph diagnostic: {error}")))
                .collect::<Result<Vec<_>, _>>()?
        };
        transaction
            .commit()
            .map_err(|error| format!("Could not finish graph query: {error}"))?;
        Ok(Some(GraphSnapshot {
            project_path: project_path.to_owned(),
            summary,
            nodes,
            edges,
            diagnostics,
        }))
    }
}

impl GraphNodeType {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Module => "module",
            Self::Directory => "directory",
            Self::File => "file",
            Self::Package => "package",
            Self::Symbol => "symbol",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Function => "function",
            Self::Method => "method",
            Self::ApiEndpoint => "apiEndpoint",
            Self::Dto => "dto",
            Self::Dependency => "dependency",
            Self::ExternalService => "externalService",
        }
    }
}

impl GraphEdgeType {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Declares => "declares",
            Self::Imports => "imports",
            Self::DependsOn => "dependsOn",
            Self::Calls => "calls",
            Self::Implements => "implements",
            Self::Extends => "extends",
            Self::Inherits => "inherits",
            Self::Injects => "injects",
            Self::Exposes => "exposes",
            Self::UsesDto => "usesDto",
            Self::Accesses => "accesses",
            Self::References => "references",
            Self::Controls => "controls",
            Self::Throws => "throws",
            Self::AnnotatedBy => "annotatedBy",
        }
    }
}

impl GraphCompleteness {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }
}

impl GraphDiagnosticSeverity {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl Confidence {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }
}

impl SourceLanguage {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::Kotlin => "kotlin",
            Self::Java => "java",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Python => "python",
        }
    }
}

fn validate_snapshot(snapshot: &GraphSnapshotInput) -> Result<(), String> {
    validate_id(&snapshot.snapshot_id, MAX_SNAPSHOT_ID_CHARS, "snapshot")?;
    if snapshot.analyzer_version.trim().is_empty() {
        return Err("Analyzer version must not be empty".to_owned());
    }
    if snapshot.nodes.len() > MAX_SNAPSHOT_NODES {
        return Err(format!(
            "Graph snapshot exceeds the {} node limit",
            MAX_SNAPSHOT_NODES
        ));
    }
    if snapshot.edges.len() > MAX_SNAPSHOT_EDGES {
        return Err(format!(
            "Graph snapshot exceeds the {} edge limit",
            MAX_SNAPSHOT_EDGES
        ));
    }
    if snapshot.diagnostics.len() > MAX_SNAPSHOT_DIAGNOSTICS {
        return Err(format!(
            "Graph snapshot exceeds the {} diagnostic limit",
            MAX_SNAPSHOT_DIAGNOSTICS
        ));
    }
    if snapshot.source_hashes.len() > MAX_SNAPSHOT_SOURCE_HASHES {
        return Err(format!(
            "Graph snapshot exceeds the {} source hash limit",
            MAX_SNAPSHOT_SOURCE_HASHES
        ));
    }
    let mut node_ids = std::collections::HashSet::with_capacity(snapshot.nodes.len());
    for node in &snapshot.nodes {
        validate_id(&node.node_id, MAX_NODE_ID_CHARS, "node")?;
        if !node_ids.insert(node.node_id.as_str()) {
            return Err(format!("Duplicate graph node ID: {}", node.node_id));
        }
    }
    let mut edge_ids = std::collections::HashSet::with_capacity(snapshot.edges.len());
    for edge in &snapshot.edges {
        validate_id(&edge.edge_id, MAX_EDGE_ID_CHARS, "edge")?;
        if !edge_ids.insert(edge.edge_id.as_str()) {
            return Err(format!("Duplicate graph edge ID: {}", edge.edge_id));
        }
        if edge.target_node_id.is_none() && edge.target_name.is_none() {
            return Err(format!(
                "Graph edge {} must have a target ID or target name",
                edge.edge_id
            ));
        }
    }
    Ok(())
}

fn validate_id(value: &str, max_chars: usize, kind: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(format!(
            "Graph {kind} ID must contain between 1 and {max_chars} characters"
        ));
    }
    Ok(())
}

fn current_time_unix_ms() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Could not read current time: {error}"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|error| format!("Current time exceeds SQLite range: {error}"))
}

fn encode_json<T: Serialize>(value: &T, label: &str) -> Result<Option<String>, String> {
    serde_json::to_string(value)
        .map(Some)
        .map_err(|error| format!("Could not encode {label}: {error}"))
}

fn encode_optional_json<T: Serialize>(
    value: &Option<T>,
    label: &str,
) -> Result<Option<String>, String> {
    value
        .as_ref()
        .map(|value| {
            serde_json::to_string(value)
                .map_err(|error| format!("Could not encode {label}: {error}"))
        })
        .transpose()
}

fn decode_json<T: for<'de> Deserialize<'de>>(value: String, label: &str) -> rusqlite::Result<T> {
    let length = value.len();
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            length,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!("{label}: {error}"))),
        )
    })
}

fn parse_db_enum<T: for<'de> Deserialize<'de>>(value: String, label: &str) -> rusqlite::Result<T> {
    serde_json::from_value(Value::String(value)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            label.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphSnapshotSummary> {
    Ok(GraphSnapshotSummary {
        snapshot_id: row.get(0)?,
        head_sha: row.get(1)?,
        analyzer_version: row.get(2)?,
        source_hashes: decode_json(row.get(3)?, "source hashes")?,
        settings: decode_json(row.get(4)?, "graph settings")?,
        completeness: parse_db_enum(row.get(5)?, "completeness")?,
        analyzed_at_unix_ms: row.get(6)?,
    })
}

fn node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    Ok(GraphNode {
        node_id: row.get(0)?,
        node_type: parse_db_enum(row.get(1)?, "node type")?,
        name: row.get(2)?,
        qualified_name: row.get(3)?,
        file_path: row.get(4)?,
        source_evidence: row
            .get::<_, Option<String>>(5)?
            .map(|value| decode_json(value, "node source evidence"))
            .transpose()?,
        language: row
            .get::<_, Option<String>>(6)?
            .map(|value| parse_db_enum(value, "source language"))
            .transpose()?,
        metadata: decode_json(row.get(7)?, "node metadata")?,
        confidence: parse_db_enum(row.get(8)?, "node confidence")?,
    })
}

fn edge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdge> {
    Ok(GraphEdge {
        edge_id: row.get(0)?,
        edge_type: parse_db_enum(row.get(1)?, "edge type")?,
        source_node_id: row.get(2)?,
        target_node_id: row.get(3)?,
        target_name: row.get(4)?,
        evidence: decode_json(row.get(5)?, "edge evidence")?,
        confidence: parse_db_enum(row.get(6)?, "edge confidence")?,
    })
}

fn diagnostic_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphDiagnostic> {
    Ok(GraphDiagnostic {
        code: row.get(0)?,
        severity: parse_db_enum(row.get(1)?, "diagnostic severity")?,
        message: row.get(2)?,
        source_evidence: row
            .get::<_, Option<String>>(3)?
            .map(|value| decode_json(value, "diagnostic source evidence"))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::StoredProject;

    fn database() -> (ProjectDatabase, std::path::PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "visual-agentic-graph-{}-{nonce}",
            std::process::id()
        ));
        let database = ProjectDatabase::open(directory.clone()).unwrap();
        database
            .save_project(&StoredProject {
                path: "C:/workspace/sample".into(),
                name: "sample".into(),
            })
            .unwrap();
        (database, directory)
    }

    fn source_range() -> SourceRange {
        SourceRange {
            relative_path: "src/Main.kt".into(),
            start_byte: 10,
            end_byte: 18,
            start_line: 2,
            start_column: 3,
            end_line: 2,
            end_column: 11,
        }
    }

    fn snapshot(snapshot_id: &str, name: &str) -> GraphSnapshotInput {
        let node_id = stable_node_id(GraphNodeType::Symbol, "src/Main.kt:Sample.run").unwrap();
        GraphSnapshotInput {
            snapshot_id: snapshot_id.into(),
            head_sha: Some("abc123".into()),
            analyzer_version: "tree-sitter-v1".into(),
            source_hashes: BTreeMap::from([("src/Main.kt".into(), "sha256:deadbeef".into())]),
            settings: serde_json::json!({"includeTests": false}),
            completeness: GraphCompleteness::Partial,
            analyzed_at_unix_ms: Some(1234),
            nodes: vec![GraphNode {
                node_id: node_id.clone(),
                node_type: GraphNodeType::Symbol,
                name: name.into(),
                qualified_name: Some("Sample.run".into()),
                file_path: Some("src/Main.kt".into()),
                source_evidence: Some(source_range()),
                language: Some(SourceLanguage::Kotlin),
                metadata: serde_json::json!({"visibility": "public"}),
                confidence: Confidence::Unknown,
            }],
            edges: vec![GraphEdge {
                edge_id: "calls:sample-run:external-call:src/Main.kt:10".into(),
                edge_type: GraphEdgeType::Calls,
                source_node_id: node_id,
                target_node_id: None,
                target_name: Some("externalCall".into()),
                evidence: source_range(),
                confidence: Confidence::Unknown,
            }],
            diagnostics: vec![GraphDiagnostic {
                code: "partialResult".into(),
                severity: GraphDiagnosticSeverity::Warning,
                message: "Some declarations could not be resolved".into(),
                source_evidence: Some(source_range()),
            }],
        }
    }

    #[test]
    fn rejects_snapshots_over_node_limit_before_id_validation() {
        let mut input = snapshot("oversized", "run");
        input
            .nodes
            .resize(MAX_SNAPSHOT_NODES + 1, input.nodes[0].clone());
        let error = validate_snapshot(&input).unwrap_err();
        assert!(error.contains("100000 node limit"));
    }

    #[test]
    fn writes_and_reads_partial_snapshot_with_unknown_relationship_and_evidence() {
        let (database, directory) = database();
        let input = snapshot("snapshot-1", "run");
        database
            .write_graph_snapshot("C:/workspace/sample", &input)
            .unwrap();

        let summaries = database
            .list_graph_snapshots("C:/workspace/sample")
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].completeness, GraphCompleteness::Partial);
        let stored = database
            .load_graph_snapshot("C:/workspace/sample", "snapshot-1")
            .unwrap()
            .unwrap();
        assert_eq!(stored.nodes, input.nodes);
        assert_eq!(stored.edges, input.edges);
        assert_eq!(stored.diagnostics, input.diagnostics);
        assert_eq!(stored.edges[0].target_node_id, None);
        assert_eq!(stored.edges[0].target_name.as_deref(), Some("externalCall"));
        assert_eq!(stored.edges[0].confidence, Confidence::Unknown);
        drop(database);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_snapshot_replacement_keeps_previous_snapshot_intact() {
        let (database, directory) = database();
        let original = snapshot("snapshot-1", "run");
        database
            .write_graph_snapshot("C:/workspace/sample", &original)
            .unwrap();
        let mut replacement = snapshot("snapshot-1", "renamed");
        replacement.edges[0].source_node_id = "missing-node".into();
        assert!(
            database
                .write_graph_snapshot("C:/workspace/sample", &replacement)
                .is_err()
        );
        let stored = database
            .load_graph_snapshot("C:/workspace/sample", "snapshot-1")
            .unwrap()
            .unwrap();
        assert_eq!(stored.nodes, original.nodes);
        drop(database);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stable_node_ids_are_deterministic_and_bounded() {
        let first = stable_node_id(GraphNodeType::File, "src/Main.kt").unwrap();
        let second = stable_node_id(GraphNodeType::File, "src/Main.kt").unwrap();
        assert_eq!(first, second);
        assert!(stable_node_id(GraphNodeType::File, &"x".repeat(MAX_NODE_ID_CHARS)).is_err());
    }
}
