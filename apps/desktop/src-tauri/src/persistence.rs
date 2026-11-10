use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::context_builder::redact_secrets;
use crate::git_status::GitStatusResponse;
use crate::review_finding::{
    FindingValidationDiagnostic, FindingValidationResult, FindingValidationState, ReviewFinding,
};

use crate::command_error::{CommandError, CommandErrorCode};

const CURRENT_SCHEMA_VERSION: i64 = 8;
const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
const GRAPH_MIGRATION: &str = include_str!("../migrations/0002_knowledge_graph.sql");
const REVIEW_MIGRATION: &str = include_str!("../migrations/0003_review_persistence.sql");
const FIX_TASK_MIGRATION: &str = include_str!("../migrations/0004_fix_tasks.sql");
const FIX_TASK_GIT_SNAPSHOT_MIGRATION: &str =
    include_str!("../migrations/0005_fix_task_git_snapshot.sql");
const FIX_TASK_WORKSPACE_MIGRATION: &str =
    include_str!("../migrations/0006_fix_task_workspace.sql");
const FIX_TASK_PATCH_PLAN_MIGRATION: &str =
    include_str!("../migrations/0007_fix_task_patch_plan.sql");
const FIX_TASK_ORCHESTRATION_MIGRATION: &str =
    include_str!("../migrations/0008_fix_task_orchestration.sql");

static NEXT_REVIEW_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct ProjectDatabase {
    pub(crate) connection: Arc<Mutex<Connection>>,
}

pub struct PersistenceState {
    database: Option<ProjectDatabase>,
    initialization_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredProject {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FindingDecisionStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FixTaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FixTaskWorkspaceState {
    NotCreated,
    Ready,
    Removed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FixTaskOrchestrationState {
    Idle,
    Applying,
    Verifying,
    RetryScheduled,
    Failed,
    Cancelled,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FixTaskOrchestration {
    pub state: FixTaskOrchestrationState,
    pub retry_count: u32,
    pub max_retries: u32,
    pub retry_reason: Option<String>,
    pub failure_log: Option<String>,
}

fn fix_task_workspace_state_from_db(value: &str) -> Result<FixTaskWorkspaceState, String> {
    match value {
        "not_created" => Ok(FixTaskWorkspaceState::NotCreated),
        "ready" => Ok(FixTaskWorkspaceState::Ready),
        "removed" => Ok(FixTaskWorkspaceState::Removed),
        "error" => Ok(FixTaskWorkspaceState::Error),
        _ => Err(format!("알 수 없는 workspace 상태: {value}")),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredFixTask {
    pub task_id: String,
    pub project_path: String,
    pub session_id: String,
    pub finding_id: String,
    pub status: FixTaskStatus,
    pub target_paths: Vec<String>,
    pub original_git_status: GitStatusResponse,
    pub workspace_path: Option<String>,
    pub workspace_state: FixTaskWorkspaceState,
    pub workspace_error: Option<String>,
    pub patch_plan: Option<FixPatchPlan>,
    pub before_paths: Vec<String>,
    pub after_paths: Vec<String>,
    pub orchestration: FixTaskOrchestration,
    pub error_message: Option<String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FixPatchPlan {
    pub intent: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoredFindingDecision {
    pub status: FindingDecisionStatus,
    pub decided_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredReviewSessionSummary {
    pub session_id: String,
    pub snapshot_id: String,
    pub validation_state: FindingValidationState,
    pub created_at_unix_ms: i64,
    pub finding_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredReviewFinding {
    pub finding_id: String,
    pub finding: ReviewFinding,
    pub decision: StoredFindingDecision,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredReviewSession {
    #[serde(flatten)]
    pub summary: StoredReviewSessionSummary,
    pub project_path: String,
    pub diagnostics: Vec<FindingValidationDiagnostic>,
    pub findings: Vec<StoredReviewFinding>,
}

#[derive(Debug, Clone)]
pub struct StoredFile<'a> {
    pub relative_path: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub size_bytes: Option<u64>,
    pub modified_unix_ms: Option<u64>,
    pub extension: Option<&'a str>,
    pub file_status: Option<&'a str>,
}

impl PersistenceState {
    pub fn initialize(data_dir: Result<PathBuf, String>) -> Self {
        match data_dir.and_then(ProjectDatabase::open) {
            Ok(database) => Self {
                database: Some(database),
                initialization_error: None,
            },
            Err(error) => Self {
                database: None,
                initialization_error: Some(error),
            },
        }
    }

    pub fn database(&self) -> Result<&ProjectDatabase, CommandError> {
        self.database.as_ref().ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::Internal,
                format!(
                    "로컬 데이터베이스를 사용할 수 없습니다: {}",
                    self.initialization_error
                        .as_deref()
                        .unwrap_or("초기화 실패")
                ),
            )
        })
    }
}

impl ProjectDatabase {
    pub fn update_fix_task_workspace(
        &self,
        root_path: &str,
        task_id: &str,
        workspace_path: Option<&str>,
        workspace_state: FixTaskWorkspaceState,
        workspace_error: Option<&str>,
    ) -> Result<StoredFixTask, String> {
        let now = current_time_unix_ms()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let updated = connection
            .execute(
                "UPDATE fix_tasks SET workspace_path = ?1, workspace_state = ?2, workspace_error = ?3, updated_at_unix_ms = ?4 WHERE task_id = ?5 AND project_id = (SELECT id FROM projects WHERE root_path = ?6)",
                params![workspace_path, fix_task_workspace_state_db(workspace_state), workspace_error, now, task_id, root_path],
            )
            .map_err(|error| format!("수정 workspace 상태를 갱신하지 못했습니다: {error}"))?;
        if updated == 0 {
            return Err("수정 작업을 찾을 수 없습니다.".to_owned());
        }
        drop(connection);
        self.load_fix_task(root_path, task_id)?
            .ok_or_else(|| "갱신한 수정 작업을 다시 불러오지 못했습니다.".to_owned())
    }

    pub fn open(data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data_dir)
            .map_err(|error| format!("앱 데이터 디렉터리를 준비하지 못했습니다: {error}"))?;
        let database_path = data_dir.join("projects.sqlite3");
        Self::open_file(&database_path)
            .map_err(|error| format!("로컬 데이터베이스를 열지 못했습니다: {error}"))
    }

    fn open_file(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate(&connection)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn save_project(&self, project: &StoredProject) -> Result<(), String> {
        let now = current_time_unix_ms()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        connection
            .execute(
                "INSERT INTO projects (root_path, name, created_at_unix_ms, last_opened_at_unix_ms)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(root_path) DO UPDATE SET
                     name = excluded.name,
                     last_opened_at_unix_ms = excluded.last_opened_at_unix_ms",
                (&project.path, &project.name, now),
            )
            .map_err(|error| format!("프로젝트 정보를 저장하지 못했습니다: {error}"))?;
        Ok(())
    }

    pub fn last_opened_project(&self) -> Result<Option<StoredProject>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        connection
            .query_row(
                "SELECT root_path, name FROM projects
                 ORDER BY last_opened_at_unix_ms DESC, id DESC LIMIT 1",
                [],
                |row| {
                    Ok(StoredProject {
                        path: row.get(0)?,
                        name: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("저장된 프로젝트를 불러오지 못했습니다: {error}"))
    }

    pub fn save_review_session(
        &self,
        root_path: &str,
        result: &FindingValidationResult,
    ) -> Result<StoredReviewSession, String> {
        let snapshot_id = result
            .snapshot_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "분석 스냅샷이 없는 검토 결과는 저장할 수 없습니다.".to_owned())?;
        if result.findings.len() > 100 {
            return Err("검토 finding 수가 저장 한도를 초과했습니다.".to_owned());
        }

        let mut safe_result = result.clone();
        for finding in &mut safe_result.findings {
            finding.title = redact_secrets(&finding.title);
            finding.description = redact_secrets(&finding.description);
            finding.suggestion = redact_secrets(&finding.suggestion);
            finding.test_suggestion = redact_secrets(&finding.test_suggestion);
            for symbol in &mut finding.affected_symbols {
                *symbol = redact_secrets(symbol);
            }
            for evidence in &mut finding.evidence {
                evidence.excerpt = redact_secrets(&evidence.excerpt);
            }
        }
        for diagnostic in &mut safe_result.diagnostics {
            diagnostic.message = redact_secrets(&diagnostic.message);
        }
        let serialized_size = serde_json::to_vec(&safe_result)
            .map_err(|_| "검토 결과를 저장 형식으로 변환하지 못했습니다.".to_owned())?
            .len();
        if serialized_size > 1024 * 1024 {
            return Err("검토 결과가 저장 크기 한도를 초과했습니다.".to_owned());
        }

        let created_at_unix_ms = current_time_unix_ms()?;
        let sequence = NEXT_REVIEW_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("{created_at_unix_ms}-{}-{sequence}", std::process::id());
        let diagnostics_json = serde_json::to_string(&safe_result.diagnostics)
            .map_err(|_| "검토 진단을 저장 형식으로 변환하지 못했습니다.".to_owned())?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("검토 결과 저장을 시작하지 못했습니다: {error}"))?;
        let project_id: i64 = transaction
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                [root_path],
                |row| row.get(0),
            )
            .map_err(|_| "프로젝트를 로컬 데이터베이스에서 찾을 수 없습니다.".to_owned())?;
        transaction
            .execute(
                "INSERT INTO review_sessions
                    (project_id, session_id, snapshot_id, validation_state, diagnostics_json, created_at_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    project_id,
                    session_id,
                    snapshot_id,
                    validation_state_db(safe_result.state),
                    diagnostics_json,
                    created_at_unix_ms,
                ],
            )
            .map_err(|error| format!("검토 세션을 저장하지 못했습니다: {error}"))?;

        for (finding_order, finding) in safe_result.findings.iter().enumerate() {
            let finding_id = format!("f{finding_order:03}");
            let affected_symbols_json = serde_json::to_string(&finding.affected_symbols)
                .map_err(|_| "finding 심볼을 저장 형식으로 변환하지 못했습니다.".to_owned())?;
            transaction
                .execute(
                    "INSERT INTO review_findings
                        (project_id, session_id, finding_id, finding_order, title, description,
                         category, severity, confidence, affected_symbols_json, suggestion, test_suggestion)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        project_id,
                        session_id,
                        finding_id,
                        finding_order as i64,
                        finding.title,
                        finding.description,
                        category_db(finding.category),
                        severity_db(finding.severity),
                        confidence_db(finding.confidence),
                        affected_symbols_json,
                        finding.suggestion,
                        finding.test_suggestion,
                    ],
                )
                .map_err(|error| format!("검토 finding을 저장하지 못했습니다: {error}"))?;

            for (evidence_order, evidence) in finding.evidence.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO review_finding_evidence
                            (project_id, session_id, finding_id, evidence_order, relative_path,
                             start_line, start_column, end_line, end_column, excerpt)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            project_id,
                            session_id,
                            finding_id,
                            evidence_order as i64,
                            evidence.relative_path,
                            evidence.start_line,
                            evidence.start_column,
                            evidence.end_line,
                            evidence.end_column,
                            evidence.excerpt,
                        ],
                    )
                    .map_err(|error| format!("finding 근거를 저장하지 못했습니다: {error}"))?;
            }
            transaction
                .execute(
                    "INSERT INTO finding_decisions
                        (project_id, session_id, finding_id, status, decided_at_unix_ms)
                     VALUES (?1, ?2, ?3, 'pending', NULL)",
                    params![project_id, session_id, finding_id],
                )
                .map_err(|error| format!("finding 결정을 초기화하지 못했습니다: {error}"))?;
        }

        transaction
            .commit()
            .map_err(|error| format!("검토 결과 저장을 완료하지 못했습니다: {error}"))?;
        drop(connection);
        self.load_review_session(root_path, &session_id)?
            .ok_or_else(|| "저장한 검토 세션을 다시 불러오지 못했습니다.".to_owned())
    }

    pub fn list_review_sessions(
        &self,
        root_path: &str,
    ) -> Result<Vec<StoredReviewSessionSummary>, String> {
        let session_ids = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
            let mut statement = connection
                .prepare(
                    "SELECT s.session_id FROM review_sessions s
                     JOIN projects p ON p.id = s.project_id
                     WHERE p.root_path = ?1
                     ORDER BY s.created_at_unix_ms DESC, s.session_id DESC",
                )
                .map_err(|error| format!("검토 세션 조회를 준비하지 못했습니다: {error}"))?;
            statement
                .query_map([root_path], |row| row.get::<_, String>(0))
                .map_err(|error| format!("검토 세션을 조회하지 못했습니다: {error}"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| format!("검토 세션 결과를 읽지 못했습니다: {error}"))?
        };
        session_ids
            .iter()
            .map(|session_id| {
                self.load_review_session(root_path, session_id)?
                    .map(|session| session.summary)
                    .ok_or_else(|| "검토 세션이 조회 중 사라졌습니다.".to_owned())
            })
            .collect()
    }

    pub fn load_review_session(
        &self,
        root_path: &str,
        session_id: &str,
    ) -> Result<Option<StoredReviewSession>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let stored = connection
            .query_row(
                "SELECT p.id, p.root_path, s.snapshot_id, s.validation_state,
                        s.created_at_unix_ms, s.diagnostics_json
                 FROM review_sessions s JOIN projects p ON p.id = s.project_id
                 WHERE p.root_path = ?1 AND s.session_id = ?2",
                params![root_path, session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("검토 세션을 불러오지 못했습니다: {error}"))?;
        let Some((
            project_id,
            project_path,
            snapshot_id,
            state,
            created_at_unix_ms,
            diagnostics_json,
        )) = stored
        else {
            return Ok(None);
        };
        let diagnostics = serde_json::from_str(&diagnostics_json)
            .map_err(|_| "저장된 검토 진단을 읽을 수 없습니다.".to_owned())?;
        let mut evidence_by_finding: std::collections::BTreeMap<
            String,
            Vec<crate::review_finding::FindingEvidence>,
        > = std::collections::BTreeMap::new();
        {
            let mut statement = connection
                .prepare(
                    "SELECT finding_id, relative_path, start_line, start_column, end_line, end_column, excerpt
                     FROM review_finding_evidence WHERE project_id = ?1 AND session_id = ?2
                     ORDER BY finding_id, evidence_order",
                )
                .map_err(|error| format!("finding 근거 조회를 준비하지 못했습니다: {error}"))?;
            let rows = statement
                .query_map(params![project_id, session_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        crate::review_finding::FindingEvidence {
                            relative_path: row.get(1)?,
                            start_line: row.get(2)?,
                            start_column: row.get(3)?,
                            end_line: row.get(4)?,
                            end_column: row.get(5)?,
                            excerpt: row.get(6)?,
                        },
                    ))
                })
                .map_err(|error| format!("finding 근거를 조회하지 못했습니다: {error}"))?;
            for row in rows {
                let (finding_id, evidence) =
                    row.map_err(|error| format!("finding 근거 결과를 읽지 못했습니다: {error}"))?;
                evidence_by_finding
                    .entry(finding_id)
                    .or_default()
                    .push(evidence);
            }
        }
        let mut findings = Vec::new();
        {
            let mut statement = connection
                .prepare(
                    "SELECT f.finding_id, f.title, f.description, f.category, f.severity,
                            f.confidence, f.affected_symbols_json, f.suggestion, f.test_suggestion,
                            d.status, d.decided_at_unix_ms
                     FROM review_findings f JOIN finding_decisions d
                       ON d.project_id = f.project_id AND d.session_id = f.session_id AND d.finding_id = f.finding_id
                     WHERE f.project_id = ?1 AND f.session_id = ?2 ORDER BY f.finding_order",
                )
                .map_err(|error| format!("검토 finding 조회를 준비하지 못했습니다: {error}"))?;
            let rows = statement
                .query_map(params![project_id, session_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                    ))
                })
                .map_err(|error| format!("검토 finding을 조회하지 못했습니다: {error}"))?;
            for row in rows {
                let (
                    finding_id,
                    title,
                    description,
                    category,
                    severity,
                    confidence,
                    symbols_json,
                    suggestion,
                    test_suggestion,
                    decision_status,
                    decided_at_unix_ms,
                ) = row.map_err(|error| format!("검토 finding 결과를 읽지 못했습니다: {error}"))?;
                let affected_symbols = serde_json::from_str(&symbols_json)
                    .map_err(|_| "저장된 finding 심볼을 읽을 수 없습니다.".to_owned())?;
                let finding = ReviewFinding {
                    title,
                    description,
                    category: category_from_db(&category)?,
                    severity: severity_from_db(&severity)?,
                    confidence: confidence_from_db(&confidence)?,
                    evidence: evidence_by_finding.remove(&finding_id).unwrap_or_default(),
                    affected_symbols,
                    suggestion,
                    test_suggestion,
                };
                findings.push(StoredReviewFinding {
                    finding_id,
                    finding,
                    decision: StoredFindingDecision {
                        status: decision_status_from_db(&decision_status)?,
                        decided_at_unix_ms,
                    },
                });
            }
        }
        Ok(Some(StoredReviewSession {
            summary: StoredReviewSessionSummary {
                session_id: session_id.to_owned(),
                snapshot_id,
                validation_state: validation_state_from_db(&state)?,
                created_at_unix_ms,
                finding_count: findings.len(),
            },
            project_path,
            diagnostics,
            findings,
        }))
    }

    pub fn set_finding_decision(
        &self,
        root_path: &str,
        session_id: &str,
        finding_id: &str,
        status: FindingDecisionStatus,
    ) -> Result<StoredFindingDecision, String> {
        let decided_at_unix_ms = match status {
            FindingDecisionStatus::Pending => None,
            FindingDecisionStatus::Approved | FindingDecisionStatus::Rejected => {
                Some(current_time_unix_ms()?)
            }
        };
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("finding 결정을 갱신하지 못했습니다: {error}"))?;
        let updated = transaction
            .execute(
                "UPDATE finding_decisions SET status = ?1, decided_at_unix_ms = ?2
                 WHERE project_id = (SELECT id FROM projects WHERE root_path = ?3)
                   AND session_id = ?4 AND finding_id = ?5",
                params![
                    decision_status_db(status),
                    decided_at_unix_ms,
                    root_path,
                    session_id,
                    finding_id
                ],
            )
            .map_err(|error| format!("finding 결정을 저장하지 못했습니다: {error}"))?;
        if updated != 1 {
            return Err("검토 세션 또는 finding을 찾을 수 없습니다.".to_owned());
        }
        transaction
            .commit()
            .map_err(|error| format!("finding 결정 저장을 완료하지 못했습니다: {error}"))?;
        Ok(StoredFindingDecision {
            status,
            decided_at_unix_ms,
        })
    }

    pub fn replace_file_index<'a, Entries>(
        &self,
        root_path: &str,
        entries: Entries,
    ) -> Result<(), String>
    where
        Entries: IntoIterator<Item = StoredFile<'a>>,
    {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("파일 인덱스 갱신을 시작하지 못했습니다: {error}"))?;
        let now = current_time_unix_ms()?;
        transaction
            .execute(
                "UPDATE projects SET last_opened_at_unix_ms = ?1 WHERE root_path = ?2",
                (&now, root_path),
            )
            .map_err(|error| format!("프로젝트 접근 시각을 갱신하지 못했습니다: {error}"))?;
        let project_id: i64 = transaction
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                [root_path],
                |row| row.get(0),
            )
            .map_err(|error| format!("프로젝트 정보를 찾지 못했습니다: {error}"))?;
        transaction
            .execute("DELETE FROM files WHERE project_id = ?1", [project_id])
            .map_err(|error| format!("이전 파일 인덱스를 비우지 못했습니다: {error}"))?;

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO files (
                        project_id, relative_path, name, kind, size_bytes,
                        modified_unix_ms, extension, file_status
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|error| format!("파일 인덱스 쿼리를 준비하지 못했습니다: {error}"))?;

            for entry in entries {
                let size_bytes = checked_sqlite_integer(entry.size_bytes, "파일 크기")?;
                let modified_unix_ms = checked_sqlite_integer(entry.modified_unix_ms, "수정 시각")?;
                statement
                    .execute(params![
                        project_id,
                        entry.relative_path,
                        entry.name,
                        entry.kind,
                        size_bytes,
                        modified_unix_ms,
                        entry.extension,
                        entry.file_status,
                    ])
                    .map_err(|error| format!("파일 인덱스를 저장하지 못했습니다: {error}"))?;
            }
        }

        transaction
            .commit()
            .map_err(|error| format!("파일 인덱스 갱신을 완료하지 못했습니다: {error}"))
    }

    #[cfg(test)]
    fn file_paths(&self, root_path: &str) -> Vec<String> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let project_id: i64 = connection
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                [root_path],
                |row| row.get(0),
            )
            .expect("project should exist");
        let mut statement = connection
            .prepare("SELECT relative_path FROM files WHERE project_id = ?1 ORDER BY relative_path")
            .expect("file query should prepare");
        statement
            .query_map([project_id], |row| row.get(0))
            .expect("file query should succeed")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("file paths should decode")
    }

    #[cfg(test)]
    fn schema_version(&self) -> i64 {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version query should succeed")
    }
}

fn validation_state_db(value: FindingValidationState) -> &'static str {
    match value {
        FindingValidationState::Complete => "complete",
        FindingValidationState::Partial => "partial",
        FindingValidationState::Invalid => "invalid",
    }
}

fn validation_state_from_db(value: &str) -> Result<FindingValidationState, String> {
    match value {
        "complete" => Ok(FindingValidationState::Complete),
        "partial" => Ok(FindingValidationState::Partial),
        "invalid" => Ok(FindingValidationState::Invalid),
        _ => Err("저장된 검토 상태를 읽을 수 없습니다.".to_owned()),
    }
}

fn category_db(value: crate::review_finding::FindingCategory) -> &'static str {
    use crate::review_finding::FindingCategory as Category;
    match value {
        Category::Correctness => "correctness",
        Category::Security => "security",
        Category::Performance => "performance",
        Category::Reliability => "reliability",
        Category::Maintainability => "maintainability",
        Category::Testability => "testability",
        Category::Style => "style",
        Category::Other => "other",
    }
}

fn category_from_db(value: &str) -> Result<crate::review_finding::FindingCategory, String> {
    use crate::review_finding::FindingCategory as Category;
    match value {
        "correctness" => Ok(Category::Correctness),
        "security" => Ok(Category::Security),
        "performance" => Ok(Category::Performance),
        "reliability" => Ok(Category::Reliability),
        "maintainability" => Ok(Category::Maintainability),
        "testability" => Ok(Category::Testability),
        "style" => Ok(Category::Style),
        "other" => Ok(Category::Other),
        _ => Err("저장된 finding 분류를 읽을 수 없습니다.".to_owned()),
    }
}

fn severity_db(value: crate::review_finding::FindingSeverity) -> &'static str {
    use crate::review_finding::FindingSeverity as Severity;
    match value {
        Severity::Critical => "critical",
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
        Severity::Informational => "informational",
    }
}

fn severity_from_db(value: &str) -> Result<crate::review_finding::FindingSeverity, String> {
    use crate::review_finding::FindingSeverity as Severity;
    match value {
        "critical" => Ok(Severity::Critical),
        "high" => Ok(Severity::High),
        "medium" => Ok(Severity::Medium),
        "low" => Ok(Severity::Low),
        "informational" => Ok(Severity::Informational),
        _ => Err("저장된 finding 심각도를 읽을 수 없습니다.".to_owned()),
    }
}

fn confidence_db(value: crate::review_finding::FindingConfidence) -> &'static str {
    use crate::review_finding::FindingConfidence as Confidence;
    match value {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

fn confidence_from_db(value: &str) -> Result<crate::review_finding::FindingConfidence, String> {
    use crate::review_finding::FindingConfidence as Confidence;
    match value {
        "high" => Ok(Confidence::High),
        "medium" => Ok(Confidence::Medium),
        "low" => Ok(Confidence::Low),
        _ => Err("저장된 finding 신뢰도를 읽을 수 없습니다.".to_owned()),
    }
}

fn decision_status_db(value: FindingDecisionStatus) -> &'static str {
    match value {
        FindingDecisionStatus::Pending => "pending",
        FindingDecisionStatus::Approved => "approved",
        FindingDecisionStatus::Rejected => "rejected",
    }
}

fn decision_status_from_db(value: &str) -> Result<FindingDecisionStatus, String> {
    match value {
        "pending" => Ok(FindingDecisionStatus::Pending),
        "approved" => Ok(FindingDecisionStatus::Approved),
        "rejected" => Ok(FindingDecisionStatus::Rejected),
        _ => Err("저장된 finding 결정 상태를 읽을 수 없습니다.".to_owned()),
    }
}

fn fix_task_status_db(value: FixTaskStatus) -> &'static str {
    match value {
        FixTaskStatus::Queued => "queued",
        FixTaskStatus::Running => "running",
        FixTaskStatus::Completed => "completed",
        FixTaskStatus::Failed => "failed",
        FixTaskStatus::Cancelled => "cancelled",
    }
}

fn fix_task_workspace_state_db(value: FixTaskWorkspaceState) -> &'static str {
    match value {
        FixTaskWorkspaceState::NotCreated => "not_created",
        FixTaskWorkspaceState::Ready => "ready",
        FixTaskWorkspaceState::Removed => "removed",
        FixTaskWorkspaceState::Error => "error",
    }
}

fn fix_task_status_from_db(value: &str) -> Result<FixTaskStatus, String> {
    match value {
        "queued" => Ok(FixTaskStatus::Queued),
        "running" => Ok(FixTaskStatus::Running),
        "completed" => Ok(FixTaskStatus::Completed),
        "failed" => Ok(FixTaskStatus::Failed),
        "cancelled" => Ok(FixTaskStatus::Cancelled),
        _ => Err(format!("알 수 없는 수정 작업 상태: {value}")),
    }
}

fn current_time_unix_ms() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("현재 시각을 읽지 못했습니다: {error}"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|error| format!("현재 시각이 저장 가능한 범위를 벗어났습니다: {error}"))
}

fn checked_sqlite_integer(value: Option<u64>, label: &str) -> Result<Option<i64>, String> {
    value
        .map(|value| {
            i64::try_from(value)
                .map_err(|error| format!("{label}이 SQLite 정수 범위를 벗어났습니다: {error}"))
        })
        .transpose()
}

impl ProjectDatabase {
    pub fn create_fix_task(
        &self,
        root_path: &str,
        session_id: &str,
        finding_id: &str,
    ) -> Result<StoredFixTask, String> {
        let now = current_time_unix_ms()?;
        let original_git_status = crate::git_status::inspect_project(Path::new(root_path));
        let original_git_status_json = serde_json::to_string(&original_git_status)
            .map_err(|error| format!("원본 Git 상태를 직렬화하지 못했습니다: {error}"))?;
        let task_id = format!(
            "fix-{now}-{}",
            NEXT_REVIEW_SESSION_ID.fetch_add(1, Ordering::Relaxed)
        );
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("수정 작업을 생성하지 못했습니다: {error}"))?;
        let (project_id, target_paths_json): (i64, String) = transaction.query_row(
            "SELECT f.project_id, COALESCE(json_group_array(DISTINCT e.relative_path), json('[]'))
             FROM review_findings f
             JOIN finding_decisions d ON d.project_id = f.project_id AND d.session_id = f.session_id AND d.finding_id = f.finding_id
             LEFT JOIN review_finding_evidence e ON e.project_id = f.project_id AND e.session_id = f.session_id AND e.finding_id = f.finding_id
             WHERE f.project_id = (SELECT id FROM projects WHERE root_path = ?1) AND f.session_id = ?2 AND f.finding_id = ?3 AND d.status = 'approved'
             GROUP BY f.project_id",
            params![root_path, session_id, finding_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|_| "승인된 finding만 수정 작업으로 만들 수 있습니다.".to_owned())?;
        transaction.execute(
            "INSERT INTO fix_tasks (task_id, project_id, session_id, finding_id, status, target_paths_json, original_git_status_json, error_message, orchestration_json, created_at_unix_ms, updated_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, NULL, ?7, ?8, ?8)",
            params![task_id, project_id, session_id, finding_id, target_paths_json, original_git_status_json,
                serde_json::to_string(&FixTaskOrchestration { state: FixTaskOrchestrationState::Idle, retry_count: 0, max_retries: 2, retry_reason: None, failure_log: None }).map_err(|error| format!("orchestration 상태를 저장하지 못했습니다: {error}"))?, now],
        ).map_err(|error| format!("수정 작업을 저장하지 못했습니다: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("수정 작업 저장을 완료하지 못했습니다: {error}"))?;
        drop(connection);
        self.load_fix_task(root_path, &task_id)?
            .ok_or_else(|| "생성한 수정 작업을 다시 불러오지 못했습니다.".to_owned())
    }

    pub fn list_fix_tasks(&self, root_path: &str) -> Result<Vec<StoredFixTask>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let mut statement = connection.prepare("SELECT task_id FROM fix_tasks WHERE project_id = (SELECT id FROM projects WHERE root_path = ?1) ORDER BY updated_at_unix_ms DESC, task_id DESC").map_err(|error| format!("수정 작업 조회를 준비하지 못했습니다: {error}"))?;
        let ids = statement
            .query_map([root_path], |row| row.get::<_, String>(0))
            .map_err(|error| format!("수정 작업을 조회하지 못했습니다: {error}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("수정 작업 결과를 읽지 못했습니다: {error}"))?;
        drop(statement);
        drop(connection);
        ids.iter()
            .map(|id| {
                self.load_fix_task(root_path, id)?
                    .ok_or_else(|| "수정 작업이 조회 중 사라졌습니다.".to_owned())
            })
            .collect()
    }

    pub fn load_fix_task(
        &self,
        root_path: &str,
        task_id: &str,
    ) -> Result<Option<StoredFixTask>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        connection.query_row(
            "SELECT p.root_path, t.session_id, t.finding_id, t.status, t.target_paths_json, t.original_git_status_json, t.workspace_path, t.workspace_state, t.workspace_error, t.patch_plan_json, t.before_paths_json, t.after_paths_json, t.orchestration_json, t.error_message, t.created_at_unix_ms, t.updated_at_unix_ms
             FROM fix_tasks t JOIN projects p ON p.id = t.project_id WHERE p.root_path = ?1 AND t.task_id = ?2",
            params![root_path, task_id], |row| {
                let paths: Vec<String> = serde_json::from_str(&row.get::<_, String>(4)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
                let original_git_status: GitStatusResponse = serde_json::from_str(&row.get::<_, String>(5)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
                let status = fix_task_status_from_db(&row.get::<_, String>(3)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
                let workspace_state = fix_task_workspace_state_from_db(&row.get::<_, String>(7)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
                let patch_plan = row.get::<_, Option<String>>(9)?.map(|value| serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?;
                let before_paths = row.get::<_, Option<String>>(10)?.map(|value| serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?.unwrap_or_default();
                let after_paths = row.get::<_, Option<String>>(11)?.map(|value| serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?.unwrap_or_default();
                let orchestration = row.get::<_, Option<String>>(12)?.map(|value| serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?.unwrap_or(FixTaskOrchestration { state: FixTaskOrchestrationState::Idle, retry_count: 0, max_retries: 2, retry_reason: None, failure_log: None });
                Ok(StoredFixTask { task_id: task_id.to_owned(), project_path: row.get(0)?, session_id: row.get(1)?, finding_id: row.get(2)?, status, target_paths: paths, original_git_status, workspace_path: row.get(6)?, workspace_state, workspace_error: row.get(8)?, patch_plan, before_paths, after_paths, orchestration, error_message: row.get(13)?, created_at_unix_ms: row.get(14)?, updated_at_unix_ms: row.get(15)? })
            },
        ).optional().map_err(|error| format!("수정 작업을 불러오지 못했습니다: {error}"))
    }

    pub fn save_fix_patch_plan(
        &self,
        root_path: &str,
        task_id: &str,
        plan: &FixPatchPlan,
        before_paths: &[String],
    ) -> Result<StoredFixTask, String> {
        let plan_json = serde_json::to_string(plan)
            .map_err(|error| format!("패치 계획을 저장 형식으로 바꾸지 못했습니다: {error}"))?;
        let before_json = serde_json::to_string(before_paths)
            .map_err(|error| format!("파일 목록을 저장 형식으로 바꾸지 못했습니다: {error}"))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let updated = connection.execute("UPDATE fix_tasks SET patch_plan_json = ?1, before_paths_json = ?2, updated_at_unix_ms = ?3 WHERE task_id = ?4 AND project_id = (SELECT id FROM projects WHERE root_path = ?5)", params![plan_json, before_json, current_time_unix_ms()?, task_id, root_path]).map_err(|error| format!("패치 계획을 저장하지 못했습니다: {error}"))?;
        if updated == 0 {
            return Err("수정 작업을 찾을 수 없습니다.".to_owned());
        }
        drop(connection);
        self.load_fix_task(root_path, task_id)?
            .ok_or_else(|| "저장한 수정 작업을 다시 불러오지 못했습니다.".to_owned())
    }

    pub fn save_fix_patch_result(
        &self,
        root_path: &str,
        task_id: &str,
        after_paths: &[String],
    ) -> Result<StoredFixTask, String> {
        let after_json = serde_json::to_string(after_paths).map_err(|error| {
            format!("변경 파일 목록을 저장 형식으로 바꾸지 못했습니다: {error}")
        })?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let updated = connection.execute("UPDATE fix_tasks SET after_paths_json = ?1, updated_at_unix_ms = ?2 WHERE task_id = ?3 AND project_id = (SELECT id FROM projects WHERE root_path = ?4)", params![after_json, current_time_unix_ms()?, task_id, root_path]).map_err(|error| format!("패치 결과를 저장하지 못했습니다: {error}"))?;
        if updated == 0 {
            return Err("수정 작업을 찾을 수 없습니다.".to_owned());
        }
        drop(connection);
        self.load_fix_task(root_path, task_id)?
            .ok_or_else(|| "저장한 수정 작업을 다시 불러오지 못했습니다.".to_owned())
    }

    pub fn update_fix_task_orchestration(
        &self,
        root_path: &str,
        task_id: &str,
        orchestration: &FixTaskOrchestration,
    ) -> Result<StoredFixTask, String> {
        let json = serde_json::to_string(orchestration)
            .map_err(|error| format!("orchestration 상태를 저장하지 못했습니다: {error}"))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let updated = connection.execute(
            "UPDATE fix_tasks SET orchestration_json = ?1, updated_at_unix_ms = ?2 WHERE task_id = ?3 AND project_id = (SELECT id FROM projects WHERE root_path = ?4)",
            params![json, current_time_unix_ms()?, task_id, root_path],
        ).map_err(|error| format!("orchestration 상태를 갱신하지 못했습니다: {error}"))?;
        if updated == 0 {
            return Err("수정 작업을 찾지 못했습니다.".to_owned());
        }
        drop(connection);
        self.load_fix_task(root_path, task_id)?
            .ok_or_else(|| "수정 작업을 다시 불러오지 못했습니다.".to_owned())
    }

    pub fn update_fix_task(
        &self,
        root_path: &str,
        task_id: &str,
        status: FixTaskStatus,
        error_message: Option<&str>,
    ) -> Result<StoredFixTask, String> {
        let now = current_time_unix_ms()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let updated = connection.execute("UPDATE fix_tasks SET status = ?1, error_message = ?2, updated_at_unix_ms = ?3 WHERE task_id = ?4 AND project_id = (SELECT id FROM projects WHERE root_path = ?5)", params![fix_task_status_db(status), error_message, now, task_id, root_path]).map_err(|error| format!("수정 작업 상태를 갱신하지 못했습니다: {error}"))?;
        if updated == 0 {
            return Err("수정 작업을 찾을 수 없습니다.".to_owned());
        }
        drop(connection);
        self.load_fix_task(root_path, task_id)?
            .ok_or_else(|| "갱신한 수정 작업을 다시 불러오지 못했습니다.".to_owned())
    }
}

fn migrate(connection: &Connection) -> rusqlite::Result<()> {
    let mut schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version > CURRENT_SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }

    while schema_version < CURRENT_SCHEMA_VERSION {
        let next_version = schema_version + 1;
        let transaction = connection.unchecked_transaction()?;
        match next_version {
            1 => transaction.execute_batch(INITIAL_MIGRATION)?,
            2 => transaction.execute_batch(GRAPH_MIGRATION)?,
            3 => transaction.execute_batch(REVIEW_MIGRATION)?,
            4 => transaction.execute_batch(FIX_TASK_MIGRATION)?,
            5 => transaction.execute_batch(FIX_TASK_GIT_SNAPSHOT_MIGRATION)?,
            6 => transaction.execute_batch(FIX_TASK_WORKSPACE_MIGRATION)?,
            7 => transaction.execute_batch(FIX_TASK_PATCH_PLAN_MIGRATION)?,
            8 => transaction.execute_batch(FIX_TASK_ORCHESTRATION_MIGRATION)?,
            _ => return Err(rusqlite::Error::InvalidQuery),
        }
        transaction.pragma_update(None, "user_version", next_version)?;
        transaction.commit()?;
        schema_version = next_version;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::{Connection, OptionalExtension};

    use super::{
        FindingDecisionStatus, GRAPH_MIGRATION, INITIAL_MIGRATION, ProjectDatabase, StoredFile,
        StoredProject,
    };
    use crate::review_finding::{
        FindingCategory, FindingConfidence, FindingEvidence, FindingSeverity,
        FindingValidationCode, FindingValidationDiagnostic, FindingValidationResult,
        FindingValidationState, ReviewFinding,
    };

    fn stored_file(relative_path: &str) -> StoredFile<'_> {
        StoredFile {
            relative_path,
            name: relative_path,
            kind: "file",
            size_bytes: Some(12),
            modified_unix_ms: Some(34),
            extension: Some("kt"),
            file_status: Some("eligible"),
        }
    }

    fn temporary_database_dir() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after UNIX epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("visual-agentic-refactor-{nonce}"))
    }

    fn save_test_project_and_snapshot(database: &ProjectDatabase, path: &str) {
        database
            .save_project(&StoredProject {
                path: path.to_owned(),
                name: "sample".to_owned(),
            })
            .expect("project should be saved");
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO graph_snapshots
                    (project_id, snapshot_id, head_sha, analyzer_version, source_hashes_json,
                     settings_json, completeness, analyzed_at_unix_ms)
                 SELECT id, 'snapshot-1', NULL, 'test', '{}', '{}', 'complete', 1
                 FROM projects WHERE root_path = ?1",
                [path],
            )
            .expect("graph snapshot should be saved");
    }

    fn sample_review_result(snapshot_id: &str) -> FindingValidationResult {
        FindingValidationResult {
            state: FindingValidationState::Partial,
            snapshot_id: Some(snapshot_id.to_owned()),
            findings: vec![ReviewFinding {
                title: "검증 finding".to_owned(),
                description: "OPENAI_API_KEY=sk-test-secret-12345 standalone sk-ant-api03-testcredential1234567890".to_owned(),
                category: FindingCategory::Correctness,
                severity: FindingSeverity::High,
                confidence: FindingConfidence::Medium,
                evidence: vec![FindingEvidence {
                    relative_path: "src/Main.kt".to_owned(),
                    start_line: 1,
                    start_column: 1,
                    end_line: 1,
                    end_column: 5,
                    excerpt: "call()".to_owned(),
                }],
                affected_symbols: vec!["Sample.call".to_owned()],
                suggestion: "입력을 검증합니다.".to_owned(),
                test_suggestion: "빈 입력 테스트를 추가합니다.".to_owned(),
            }],
            diagnostics: vec![FindingValidationDiagnostic {
                finding_index: None,
                code: FindingValidationCode::SourceChanged,
                message: "소스가 변경되었습니다.".to_owned(),
            }],
        }
    }

    #[test]
    fn creates_and_reuses_initial_schema() {
        let data_dir = temporary_database_dir();

        let first = ProjectDatabase::open(data_dir.clone()).expect("database should initialize");
        assert_eq!(first.schema_version(), 3);
        drop(first);

        let second = ProjectDatabase::open(data_dir.clone()).expect("database should reopen");
        assert_eq!(second.schema_version(), 3);
        drop(second);

        std::fs::remove_dir_all(data_dir).expect("temporary database directory should be removed");
    }

    #[test]
    fn migrates_v2_graph_snapshot_without_losing_it() {
        let data_dir = temporary_database_dir();
        std::fs::create_dir_all(&data_dir).unwrap();
        let database_path = data_dir.join("projects.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(INITIAL_MIGRATION).unwrap();
        connection.execute_batch(GRAPH_MIGRATION).unwrap();
        connection
            .execute(
                "INSERT INTO projects (root_path, name, created_at_unix_ms, last_opened_at_unix_ms)
             VALUES ('C:/workspace/sample', 'sample', 10, 20)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO graph_snapshots
                (project_id, snapshot_id, head_sha, analyzer_version, source_hashes_json,
                 settings_json, completeness, analyzed_at_unix_ms)
             VALUES (1, 'snapshot-preserved', NULL, 'test', '{}', '{}', 'complete', 30)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);

        let database = ProjectDatabase::open(data_dir.clone()).unwrap();
        assert_eq!(database.schema_version(), 6);
        let connection = database.connection.lock().unwrap();
        let snapshot_id: String = connection
            .query_row("SELECT snapshot_id FROM graph_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(snapshot_id, "snapshot-preserved");
        drop(connection);
        drop(database);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn failed_review_migration_rolls_back_v3_tables() {
        let data_dir = temporary_database_dir();
        std::fs::create_dir_all(&data_dir).unwrap();
        let database_path = data_dir.join("projects.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(INITIAL_MIGRATION).unwrap();
        connection.execute_batch(GRAPH_MIGRATION).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        connection
            .execute_batch("CREATE TABLE review_findings (conflicting INTEGER);")
            .unwrap();
        drop(connection);

        assert!(ProjectDatabase::open(data_dir.clone()).is_err());
        let connection = Connection::open(&database_path).unwrap();
        let schema_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let sessions_exist: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'review_sessions'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(schema_version, 2);
        assert_eq!(sessions_exist, None);
        drop(connection);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn migrates_existing_project_and_file_rows_without_data_loss() {
        let data_dir = temporary_database_dir();
        std::fs::create_dir_all(&data_dir).unwrap();
        let database_path = data_dir.join("projects.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(INITIAL_MIGRATION).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection
            .execute(
                "INSERT INTO projects (root_path, name, created_at_unix_ms, last_opened_at_unix_ms)
                 VALUES ('C:/workspace/sample', 'sample', 10, 20)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO files (project_id, relative_path, name, kind)
                 VALUES (1, 'src/Main.kt', 'Main.kt', 'file')",
                [],
            )
            .unwrap();
        drop(connection);

        let database =
            ProjectDatabase::open(data_dir.clone()).expect("schema migrations should succeed");
        assert_eq!(database.schema_version(), 6);
        let connection = database.connection.lock().unwrap();
        let project_name: String = connection
            .query_row("SELECT name FROM projects WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let file_path: String = connection
            .query_row(
                "SELECT relative_path FROM files WHERE project_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let graph_table_exists: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'graph_snapshots'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(project_name, "sample");
        assert_eq!(file_path, "src/Main.kt");
        assert_eq!(graph_table_exists.as_deref(), Some("graph_snapshots"));
        drop(connection);
        drop(database);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn failed_graph_migration_rolls_back_all_schema_changes() {
        let data_dir = temporary_database_dir();
        std::fs::create_dir_all(&data_dir).unwrap();
        let database_path = data_dir.join("projects.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(INITIAL_MIGRATION).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection
            .execute_batch("CREATE TABLE graph_nodes (conflicting INTEGER);")
            .unwrap();
        drop(connection);

        assert!(ProjectDatabase::open(data_dir.clone()).is_err());
        let connection = Connection::open(&database_path).unwrap();
        let schema_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let graph_snapshots_exists: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'graph_snapshots'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(schema_version, 1);
        assert_eq!(graph_snapshots_exists, None);
        drop(connection);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn stores_review_findings_and_decisions_across_reopen() {
        let data_dir = temporary_database_dir();
        let project_path = "C:/workspace/sample";
        let database = ProjectDatabase::open(data_dir.clone()).unwrap();
        save_test_project_and_snapshot(&database, project_path);
        let session = database
            .save_review_session(project_path, &sample_review_result("snapshot-1"))
            .expect("review session should save");

        assert_eq!(
            session.summary.validation_state,
            FindingValidationState::Partial
        );
        assert_eq!(session.summary.finding_count, 1);
        assert_eq!(session.findings[0].finding_id, "f000");
        assert_eq!(
            session.findings[0].finding.evidence[0].relative_path,
            "src/Main.kt"
        );
        assert_eq!(
            session.findings[0].decision.status,
            FindingDecisionStatus::Pending
        );
        assert_eq!(session.findings[0].decision.decided_at_unix_ms, None);
        assert!(
            !session.findings[0]
                .finding
                .description
                .contains("sk-test-secret-12345")
        );
        assert!(
            !session.findings[0]
                .finding
                .description
                .contains("sk-ant-api03-testcredential1234567890")
        );
        assert_eq!(
            database.list_review_sessions(project_path).unwrap().len(),
            1
        );

        let session_id = session.summary.session_id;
        drop(database);
        let database = ProjectDatabase::open(data_dir.clone()).unwrap();
        let reloaded = database
            .load_review_session(project_path, &session_id)
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.findings[0].finding.title, "검증 finding");
        assert_eq!(
            reloaded.diagnostics[0].code,
            FindingValidationCode::SourceChanged
        );
        let decision = database
            .set_finding_decision(
                project_path,
                &session_id,
                "f000",
                FindingDecisionStatus::Approved,
            )
            .unwrap();
        assert_eq!(decision.status, FindingDecisionStatus::Approved);
        assert!(decision.decided_at_unix_ms.is_some());
        let saved_decision = database
            .load_review_session(project_path, &session_id)
            .unwrap()
            .unwrap()
            .findings
            .remove(0)
            .decision;
        assert_eq!(saved_decision, decision);
        drop(database);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn rolls_back_review_session_when_a_child_write_fails() {
        let data_dir = temporary_database_dir();
        let project_path = "C:/workspace/sample";
        let database = ProjectDatabase::open(data_dir.clone()).unwrap();
        save_test_project_and_snapshot(&database, project_path);
        database
            .connection
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_review_decision BEFORE INSERT ON finding_decisions
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .unwrap();

        let error = database
            .save_review_session(project_path, &sample_review_result("snapshot-1"))
            .expect_err("injected child insert should fail");
        assert!(error.contains("결정을 초기화하지 못했습니다"));
        let connection = database.connection.lock().unwrap();
        let session_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM review_sessions", [], |row| row.get(0))
            .unwrap();
        let finding_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM review_findings", [], |row| row.get(0))
            .unwrap();
        let evidence_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM review_finding_evidence", [], |row| {
                row.get(0)
            })
            .unwrap();
        let foreign_key_errors: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!((session_count, finding_count, evidence_count), (0, 0, 0));
        assert_eq!(foreign_key_errors, 0);
        drop(connection);
        drop(database);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn rejects_review_session_when_its_snapshot_is_missing() {
        let data_dir = temporary_database_dir();
        let project_path = "C:/workspace/sample";
        let database = ProjectDatabase::open(data_dir.clone()).unwrap();
        database
            .save_project(&StoredProject {
                path: project_path.to_owned(),
                name: "sample".to_owned(),
            })
            .unwrap();

        assert!(
            database
                .save_review_session(project_path, &sample_review_result("missing-snapshot"))
                .is_err()
        );
        let session_count: i64 = database
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM review_sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(session_count, 0);
        drop(database);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn saves_and_loads_the_most_recent_project() {
        let data_dir = temporary_database_dir();
        let database = ProjectDatabase::open(data_dir.clone()).expect("database should initialize");
        let project = StoredProject {
            path: "C:/workspace/sample".into(),
            name: "sample".into(),
        };

        database
            .save_project(&project)
            .expect("project should be persisted");

        assert_eq!(
            database.last_opened_project().unwrap().unwrap().path,
            project.path
        );
        drop(database);
        std::fs::remove_dir_all(data_dir).expect("temporary database directory should be removed");
    }

    #[test]
    fn replaces_file_index_atomically() {
        let data_dir = temporary_database_dir();
        let database = ProjectDatabase::open(data_dir.clone()).expect("database should initialize");
        let project = StoredProject {
            path: "C:/workspace/sample".into(),
            name: "sample".into(),
        };
        database.save_project(&project).unwrap();

        database
            .replace_file_index(&project.path, [stored_file("src/old.kt")])
            .unwrap();
        assert_eq!(database.file_paths(&project.path), ["src/old.kt"]);

        let result = database.replace_file_index(
            &project.path,
            [stored_file("src/new.kt"), stored_file("src/new.kt")],
        );
        assert!(result.is_err());
        assert_eq!(database.file_paths(&project.path), ["src/old.kt"]);

        drop(database);
        std::fs::remove_dir_all(data_dir).expect("temporary database directory should be removed");
    }
}
