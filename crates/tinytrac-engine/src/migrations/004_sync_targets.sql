-- Preserve progress: only the local target representation changes.
INSERT INTO meta (key, value)
SELECT 'sync_target', json_object('Folder', value) FROM meta WHERE key = 'sync_folder';
DELETE FROM meta WHERE key = 'sync_folder';
