CREATE TABLE review_sessions (
    project_id INTEGER NOT NULL,
    session_id TEXT NOT NULL CHECK(length(session_id) BETWEEN 1 AND 128),
    snapshot_id TEXT NOT NULL,
    validation_state TEXT NOT NULL CHECK(validation_state IN ('complete', 'partial', 'invalid')),
    diagnostics_json TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY(project_id, session_id),
    FOREIGN KEY(project_id, snapshot_id)
        REFERENCES graph_snapshots(project_id, snapshot_id) ON DELETE CASCADE
);

CREATE INDEX idx_review_sessions_project_created
    ON review_sessions(project_id, created_at_unix_ms DESC, session_id DESC);

CREATE TABLE review_findings (
    project_id INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK(length(finding_id) BETWEEN 1 AND 160),
    finding_order INTEGER NOT NULL CHECK(finding_order >= 0),
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT NOT NULL CHECK(category IN (
        'correctness', 'security', 'performance', 'reliability',
        'maintainability', 'testability', 'style', 'other'
    )),
    severity TEXT NOT NULL CHECK(severity IN (
        'critical', 'high', 'medium', 'low', 'informational'
    )),
    confidence TEXT NOT NULL CHECK(confidence IN ('high', 'medium', 'low')),
    affected_symbols_json TEXT NOT NULL,
    suggestion TEXT NOT NULL,
    test_suggestion TEXT NOT NULL,
    PRIMARY KEY(project_id, session_id, finding_id),
    UNIQUE(project_id, session_id, finding_order),
    FOREIGN KEY(project_id, session_id)
        REFERENCES review_sessions(project_id, session_id) ON DELETE CASCADE
);

CREATE TABLE review_finding_evidence (
    project_id INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    evidence_order INTEGER NOT NULL CHECK(evidence_order >= 0),
    relative_path TEXT NOT NULL,
    start_line INTEGER NOT NULL CHECK(start_line >= 1),
    start_column INTEGER NOT NULL CHECK(start_column >= 1),
    end_line INTEGER NOT NULL CHECK(end_line >= start_line),
    end_column INTEGER NOT NULL CHECK(end_column >= 1),
    excerpt TEXT NOT NULL,
    PRIMARY KEY(project_id, session_id, finding_id, evidence_order),
    FOREIGN KEY(project_id, session_id, finding_id)
        REFERENCES review_findings(project_id, session_id, finding_id) ON DELETE CASCADE
);

CREATE INDEX idx_review_evidence_path
    ON review_finding_evidence(project_id, relative_path, session_id, finding_id);

CREATE TABLE finding_decisions (
    project_id INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'approved', 'rejected')),
    decided_at_unix_ms INTEGER,
    CHECK(
        (status = 'pending' AND decided_at_unix_ms IS NULL)
        OR (status IN ('approved', 'rejected') AND decided_at_unix_ms IS NOT NULL)
    ),
    PRIMARY KEY(project_id, session_id, finding_id),
    FOREIGN KEY(project_id, session_id, finding_id)
        REFERENCES review_findings(project_id, session_id, finding_id) ON DELETE CASCADE
);
