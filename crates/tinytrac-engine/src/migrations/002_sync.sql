-- Remember individual files: Syncthing can deliver them in any order.
CREATE TABLE imported_files (name TEXT PRIMARY KEY) STRICT;
CREATE TABLE conflicts (
    rev TEXT PRIMARY KEY,
    record TEXT NOT NULL
) STRICT;
ALTER TABLE changes ADD COLUMN merges TEXT NOT NULL DEFAULT '[]';
CREATE INDEX changes_by_device_rev ON changes (device, rev);
