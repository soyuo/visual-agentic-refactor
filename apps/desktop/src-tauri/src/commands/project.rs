use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command_error::{CommandError, CommandErrorCode};
use crate::persistence::{PersistenceState, StoredProject};
use tauri::State;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenProjectRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenProjectResponse {
    pub path: String,
    pub name: String,
    pub persistence_error: Option<String>,
}

#[tauri::command]
pub fn open_project(
    request: OpenProjectRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<OpenProjectResponse, CommandError> {
    if request.path.trim().is_empty() {
        return Err(invalid_project_path("프로젝트 경로가 비어 있습니다."));
    }

    let selected_path = PathBuf::from(request.path);
    if !selected_path.is_absolute() {
        return Err(invalid_project_path(
            "프로젝트 경로는 절대 경로여야 합니다.",
        ));
    }

    let canonical_path = fs::canonicalize(&selected_path).map_err(|error| {
        invalid_project_path(match error.kind() {
            std::io::ErrorKind::NotFound => "선택한 경로가 존재하지 않습니다.",
            std::io::ErrorKind::PermissionDenied => "선택한 경로에 접근할 수 없습니다.",
            _ => "프로젝트 경로를 확인할 수 없습니다.",
        })
    })?;

    let metadata = fs::metadata(&canonical_path).map_err(|error| {
        invalid_project_path(match error.kind() {
            std::io::ErrorKind::NotFound => "선택한 경로가 존재하지 않습니다.",
            std::io::ErrorKind::PermissionDenied => "선택한 경로에 접근할 수 없습니다.",
            _ => "프로젝트 경로 정보를 읽을 수 없습니다.",
        })
    })?;

    if !metadata.is_dir() {
        return Err(invalid_project_path("선택한 경로가 디렉터리가 아닙니다."));
    }

    let mut entries = fs::read_dir(&canonical_path).map_err(|error| {
        invalid_project_path(match error.kind() {
            std::io::ErrorKind::PermissionDenied => "선택한 디렉터리를 읽을 수 없습니다.",
            _ => "선택한 디렉터리를 확인할 수 없습니다.",
        })
    })?;

    if let Some(entry) = entries.next() {
        entry.map_err(|error| {
            invalid_project_path(match error.kind() {
                std::io::ErrorKind::PermissionDenied => "선택한 디렉터리를 읽을 수 없습니다.",
                _ => "선택한 디렉터리를 확인할 수 없습니다.",
            })
        })?;
    }

    let mut response = project_response(&canonical_path);
    response.persistence_error = persistence
        .database()
        .map_err(|error| error.message)
        .and_then(|database| {
            database.save_project(&StoredProject {
                path: response.path.clone(),
                name: response.name.clone(),
            })
        })
        .err();
    Ok(response)
}

#[tauri::command]
pub fn get_last_project(
    persistence: State<'_, PersistenceState>,
) -> Result<Option<OpenProjectResponse>, CommandError> {
    let project = persistence
        .database()?
        .last_opened_project()
        .map_err(|error| CommandError::new(CommandErrorCode::Internal, error))?;

    Ok(project.map(|project| OpenProjectResponse {
        path: project.path,
        name: project.name,
        persistence_error: None,
    }))
}

fn project_response(path: &Path) -> OpenProjectResponse {
    let display_path = path.to_string_lossy().into_owned();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| display_path.clone());

    OpenProjectResponse {
        path: display_path,
        name,
        persistence_error: None,
    }
}

fn invalid_project_path(message: &str) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}
