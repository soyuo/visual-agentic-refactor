ALTER TABLE fix_tasks ADD COLUMN patch_plan_json TEXT;
ALTER TABLE fix_tasks ADD COLUMN before_paths_json TEXT;
ALTER TABLE fix_tasks ADD COLUMN after_paths_json TEXT;
