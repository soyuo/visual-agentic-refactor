use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

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

    #[cfg(test)]
    fn schema_version(&self) -> i64 {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version query should succeed")
    }
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

    use super::ProjectDatabase;

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
}
