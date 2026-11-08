CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    root_path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    last_opened_at_unix_ms INTEGER NOT NULL
);

CREATE INDEX idx_projects_last_opened
    ON projects(last_opened_at_unix_ms DESC, id DESC);

CREATE TABLE files (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('directory', 'file')),
    size_bytes INTEGER,
    modified_unix_ms INTEGER,
    extension TEXT,
    file_status TEXT CHECK(
        file_status IS NULL OR file_status IN ('eligible', 'unsupportedType', 'tooLarge', 'binary')
    ),
    PRIMARY KEY(project_id, relative_path)
);
