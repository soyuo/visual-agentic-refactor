CREATE TABLE fix_tasks (
    task_id TEXT PRIMARY KEY CHECK(length(task_id) BETWEEN 1 AND 128),
    project_id INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    target_paths_json TEXT NOT NULL,
    error_message TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(project_id, session_id, finding_id)
        REFERENCES review_findings(project_id, session_id, finding_id) ON DELETE CASCADE
);

CREATE INDEX idx_fix_tasks_project_updated
    ON fix_tasks(project_id, updated_at_unix_ms DESC, task_id DESC);
