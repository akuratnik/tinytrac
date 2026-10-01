-- Local, unsynced values such as this device's id.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- The current revision of every record: tags, sessions and settings.
-- `body` is the record as JSON, or NULL once deleted (a tombstone, kept for sync).
CREATE TABLE records (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    rev        TEXT NOT NULL,
    body       TEXT,
    -- Session lookup columns, derived from `body`.
    day        TEXT,     -- YYYY-MM-DD the session belongs to
    span_start INTEGER,  -- Unix seconds: start of the first segment
    span_end   INTEGER,  -- Unix seconds: end of the last segment; NULL while running
    is_open    INTEGER   -- 1 until the session is stopped
) STRICT;

CREATE INDEX records_by_day ON records (kind, day);
CREATE INDEX records_by_span ON records (kind, span_start);
CREATE INDEX records_open ON records (kind, is_open);

-- Every revision written on this device or imported from another: the history
-- that sync exchanges. `parent` is the revision it replaced.
CREATE TABLE changes (
    rev      TEXT PRIMARY KEY,
    record   TEXT NOT NULL,
    kind     TEXT NOT NULL,
    parent   TEXT,
    device   TEXT NOT NULL,
    body     TEXT,
    exported INTEGER NOT NULL DEFAULT 0
) STRICT;
