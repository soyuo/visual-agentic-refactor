ALTER TABLE fix_tasks ADD COLUMN workspace_path TEXT;
ALTER TABLE fix_tasks ADD COLUMN workspace_state TEXT NOT NULL DEFAULT 'not_created'
    CHECK(workspace_state IN ('not_created', 'ready', 'removed', 'error'));
ALTER TABLE fix_tasks ADD COLUMN workspace_error TEXT;
