CREATE TABLE graph_snapshots (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    snapshot_id TEXT NOT NULL CHECK(length(snapshot_id) BETWEEN 1 AND 256),
    head_sha TEXT,
    analyzer_version TEXT NOT NULL,
    source_hashes_json TEXT NOT NULL,
    settings_json TEXT NOT NULL,
    completeness TEXT NOT NULL CHECK(completeness IN ('complete', 'partial')),
    analyzed_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY(project_id, snapshot_id)
);

CREATE INDEX idx_graph_snapshots_analyzed
    ON graph_snapshots(project_id, analyzed_at_unix_ms DESC, snapshot_id DESC);

CREATE TABLE graph_nodes (
    project_id INTEGER NOT NULL,
    snapshot_id TEXT NOT NULL,
    node_id TEXT NOT NULL CHECK(length(node_id) BETWEEN 1 AND 512),
    node_type TEXT NOT NULL CHECK(node_type IN (
        'project', 'module', 'directory', 'file', 'package', 'symbol', 'class', 'interface',
        'function', 'method', 'apiEndpoint', 'dto', 'dependency', 'externalService'
    )),
    name TEXT NOT NULL,
    qualified_name TEXT,
    file_path TEXT,
    source_evidence_json TEXT,
    language TEXT CHECK(language IS NULL OR language IN ('kotlin', 'java')),
    metadata_json TEXT NOT NULL,
    confidence TEXT NOT NULL CHECK(confidence IN ('confirmed', 'inferred', 'unknown')),
    PRIMARY KEY(project_id, snapshot_id, node_id),
    FOREIGN KEY(project_id, snapshot_id)
        REFERENCES graph_snapshots(project_id, snapshot_id) ON DELETE CASCADE
);

CREATE INDEX idx_graph_nodes_snapshot_path
    ON graph_nodes(project_id, snapshot_id, file_path, node_id);

CREATE TABLE graph_edges (
    project_id INTEGER NOT NULL,
    snapshot_id TEXT NOT NULL,
    edge_id TEXT NOT NULL CHECK(length(edge_id) BETWEEN 1 AND 512),
    edge_type TEXT NOT NULL CHECK(edge_type IN (
        'contains', 'declares', 'imports', 'dependsOn', 'calls', 'implements',
        'extends', 'inherits', 'injects', 'exposes', 'usesDto', 'accesses',
        'references', 'controls', 'throws', 'annotatedBy'
    )),
    source_node_id TEXT NOT NULL,
    target_node_id TEXT,
    target_name TEXT,
    evidence_json TEXT NOT NULL,
    confidence TEXT NOT NULL CHECK(confidence IN ('confirmed', 'inferred', 'unknown')),
    CHECK(target_node_id IS NOT NULL OR target_name IS NOT NULL),
    PRIMARY KEY(project_id, snapshot_id, edge_id),
    FOREIGN KEY(project_id, snapshot_id)
        REFERENCES graph_snapshots(project_id, snapshot_id) ON DELETE CASCADE,
    FOREIGN KEY(project_id, snapshot_id, source_node_id)
        REFERENCES graph_nodes(project_id, snapshot_id, node_id) ON DELETE CASCADE,
    FOREIGN KEY(project_id, snapshot_id, target_node_id)
        REFERENCES graph_nodes(project_id, snapshot_id, node_id) ON DELETE CASCADE
);

CREATE INDEX idx_graph_edges_snapshot_source
    ON graph_edges(project_id, snapshot_id, source_node_id, edge_id);

CREATE INDEX idx_graph_edges_snapshot_target
    ON graph_edges(project_id, snapshot_id, target_node_id, edge_id);

CREATE TABLE graph_diagnostics (
    project_id INTEGER NOT NULL,
    snapshot_id TEXT NOT NULL,
    diagnostic_order INTEGER NOT NULL CHECK(diagnostic_order >= 0),
    code TEXT NOT NULL,
    severity TEXT NOT NULL CHECK(severity IN ('info', 'warning', 'error')),
    message TEXT NOT NULL,
    source_evidence_json TEXT,
    PRIMARY KEY(project_id, snapshot_id, diagnostic_order),
    FOREIGN KEY(project_id, snapshot_id)
        REFERENCES graph_snapshots(project_id, snapshot_id) ON DELETE CASCADE
);
