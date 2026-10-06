-- Reinspect files that older clients could have swallowed as unsupported.
DELETE FROM imported_files;
ALTER TABLE imported_files ADD COLUMN rejected INTEGER NOT NULL DEFAULT 0;
ALTER TABLE imported_files ADD COLUMN reason TEXT;
INSERT INTO meta (key, value) VALUES ('sync_epoch', '0');

CREATE INDEX changes_by_parent ON changes (record, parent);
CREATE INDEX conflicts_by_record ON conflicts (record);
