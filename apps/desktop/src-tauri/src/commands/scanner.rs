use crate::command_error::CommandError;
use crate::persistence::{PersistenceState, ProjectDatabase, StoredFile};
use crate::scanner::{
    ScanEntry, ScanEntryKind, ScanFileStatus, ScanProjectRequest, ScanProjectResponse, ScanSummary,
};
use crate::state::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn scan_project(
    request: ScanProjectRequest,
    app: AppHandle,
    state: State<'_, AppState>,
    persistence: State<'_, PersistenceState>,
) -> Result<ScanProjectResponse, CommandError> {
    let task_control = state.register_task()?;
    let database = persistence
        .database()
        .cloned()
        .map_err(|error| error.message);
    let mut response = tauri::async_runtime::spawn_blocking(move || {
        crate::scanner::scan_project(request, app, task_control)
    })
    .await
    .map_err(|_| {
        CommandError::new(
            crate::command_error::CommandErrorCode::Internal,
            "프로젝트 파일 스캔 작업이 비정상 종료되었습니다.",
        )
    })??;

    response.persistence_error = database
        .and_then(|database| {
            if should_persist_scan(&response.summary) {
                persist_scan(&database, &response)
            } else {
                Ok(())
            }
        })
        .err();
    Ok(response)
}

fn should_persist_scan(summary: &ScanSummary) -> bool {
    !summary.depth_limit_reached && !summary.entry_limit_reached
}

fn persist_scan(database: &ProjectDatabase, response: &ScanProjectResponse) -> Result<(), String> {
    database.replace_file_index(
        &response.root_path,
        response.entries.iter().map(stored_file),
    )
}

fn stored_file(entry: &ScanEntry) -> StoredFile<'_> {
    let kind = match &entry.kind {
        ScanEntryKind::Directory => "directory",
        ScanEntryKind::File => "file",
    };
    let file_status = entry.file_status.as_ref().map(|status| match status {
        ScanFileStatus::Eligible => "eligible",
        ScanFileStatus::UnsupportedType => "unsupportedType",
        ScanFileStatus::TooLarge => "tooLarge",
        ScanFileStatus::Binary => "binary",
    });

    StoredFile {
        relative_path: &entry.relative_path,
        name: &entry.name,
        kind,
        size_bytes: entry.size_bytes,
        modified_unix_ms: entry.modified_unix_ms,
        extension: entry.extension.as_deref(),
        file_status,
    }
}

#[cfg(test)]
mod tests {
    use super::should_persist_scan;
    use crate::scanner::ScanSummary;

    #[test]
    fn incomplete_scans_do_not_replace_the_persisted_index() {
        let complete = ScanSummary::default();
        assert!(should_persist_scan(&complete));

        let entry_limited = ScanSummary {
            entry_limit_reached: true,
            ..ScanSummary::default()
        };
        assert!(!should_persist_scan(&entry_limited));

        let depth_limited = ScanSummary {
            depth_limit_reached: true,
            ..ScanSummary::default()
        };
        assert!(!should_persist_scan(&depth_limited));
    }
}
