CREATE TABLE graph_diagrams (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    snapshot_id TEXT NOT NULL,
    provider_id TEXT NOT NULL CHECK(length(provider_id) BETWEEN 1 AND 128),
    model TEXT NOT NULL CHECK(length(model) BETWEEN 1 AND 256),
    diagram_json TEXT NOT NULL,
    saved_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY(project_id, snapshot_id, provider_id, model),
    FOREIGN KEY(project_id, snapshot_id)
        REFERENCES graph_snapshots(project_id, snapshot_id) ON DELETE CASCADE
);

CREATE INDEX idx_graph_diagrams_recent
    ON graph_diagrams(project_id, snapshot_id, saved_at_unix_ms DESC);
