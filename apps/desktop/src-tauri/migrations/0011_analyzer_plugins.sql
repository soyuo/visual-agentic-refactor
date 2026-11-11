CREATE TABLE analyzer_plugins (
    plugin_id TEXT PRIMARY KEY NOT NULL,
    manifest_json TEXT NOT NULL,
    wasm_module BLOB NOT NULL,
    trust_state TEXT NOT NULL CHECK (trust_state IN ('awaiting_approval', 'user_approved', 'rejected')),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    installed_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);
