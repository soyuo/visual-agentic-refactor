use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::command_error::{CommandError, CommandErrorCode};

const CURRENT_SCHEMA_VERSION: i64 = 1;
const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");

#[derive(Clone)]
pub struct ProjectDatabase {
    connection: Arc<Mutex<Connection>>,
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

#[derive(Debug, Clone)]
pub struct StoredFile {
    pub relative_path: String,
    pub name: String,
    pub kind: String,
    pub size_bytes: Option<u64>,
    pub modified_unix_ms: Option<u64>,
    pub extension: Option<String>,
    pub file_status: Option<String>,
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

    pub fn replace_file_index(
        &self,
        root_path: &str,
        entries: &[StoredFile],
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "로컬 데이터베이스 잠금에 실패했습니다.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("파일 인덱스 갱신을 시작하지 못했습니다: {error}"))?;
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

fn migrate(connection: &Connection) -> rusqlite::Result<()> {
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version > CURRENT_SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }

    if schema_version == 0 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(INITIAL_MIGRATION)?;
        transaction.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ProjectDatabase, StoredFile, StoredProject};

    fn temporary_database_dir() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after UNIX epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("visual-agentic-refactor-{nonce}"))
    }

    #[test]
    fn creates_and_reuses_initial_schema() {
        let data_dir = temporary_database_dir();

        let first = ProjectDatabase::open(data_dir.clone()).expect("database should initialize");
        assert_eq!(first.schema_version(), 1);
        drop(first);

        let second = ProjectDatabase::open(data_dir.clone()).expect("database should reopen");
        assert_eq!(second.schema_version(), 1);
        drop(second);

        std::fs::remove_dir_all(data_dir).expect("temporary database directory should be removed");
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

        let stored_file = |relative_path: &str| StoredFile {
            relative_path: relative_path.into(),
            name: relative_path.into(),
            kind: "file".into(),
            size_bytes: Some(12),
            modified_unix_ms: Some(34),
            extension: Some("kt".into()),
            file_status: Some("eligible".into()),
        };
        database
            .replace_file_index(&project.path, &[stored_file("src/old.kt")])
            .unwrap();
        assert_eq!(database.file_paths(&project.path), ["src/old.kt"]);

        let result = database.replace_file_index(
            &project.path,
            &[stored_file("src/new.kt"), stored_file("src/new.kt")],
        );
        assert!(result.is_err());
        assert_eq!(database.file_paths(&project.path), ["src/old.kt"]);

        drop(database);
        std::fs::remove_dir_all(data_dir).expect("temporary database directory should be removed");
    }
}
