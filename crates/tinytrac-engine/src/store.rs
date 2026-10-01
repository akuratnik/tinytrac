//! Typed access to the SQLite database.
//!
//! Records are stored whole, as JSON, in one table, with a few lookup columns
//! derived from them. Every write also appends a revision to `changes`, which is
//! what sync exchanges between devices.

use rusqlite::{Connection, OptionalExtension, Params, params};
use serde::{Serialize, de::DeserializeOwned};
use tinytrac_core::{DateRange, Session, Status, Tag, TimeZone, Timestamp};
use uuid::Uuid;

use crate::{Result, Settings};

const MIGRATIONS: &[&str] = &[include_str!("migrations/001_initial.sql")];

/// Brings the schema up to date, tracking progress in `user_version`.
pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    let applied: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let tx = db.transaction()?;
    for (sql, version) in MIGRATIONS.iter().zip(1_u32..) {
        if version > applied {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// This device's id, created on first use.
pub(crate) fn device_id(db: &Connection) -> Result<String> {
    let existing = db
        .query_row("SELECT value FROM meta WHERE key = 'device'", [], |row| {
            row.get(0)
        })
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = Uuid::now_v7().to_string();
    db.execute("INSERT INTO meta (key, value) VALUES ('device', ?1)", [&id])?;
    Ok(id)
}

/// A kind of record kept in the `records` table.
pub(crate) trait Stored: Serialize + DeserializeOwned {
    const KIND: &'static str;

    fn key(&self) -> String;

    /// Lookup columns; only sessions have any.
    fn index(&self) -> Index {
        Index::default()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Index {
    day: Option<String>,
    span_start: Option<i64>,
    span_end: Option<i64>,
    is_open: Option<bool>,
}

impl Stored for Tag {
    const KIND: &'static str = "tag";

    fn key(&self) -> String {
        self.id.to_string()
    }
}

impl Stored for Session {
    const KIND: &'static str = "session";

    fn key(&self) -> String {
        self.id.to_string()
    }

    fn index(&self) -> Index {
        let segments = self.segments();
        Index {
            day: Some(self.day.to_string()),
            span_start: segments.first().map(|s| s.start.as_second()),
            span_end: segments
                .last()
                .and_then(|s| s.end)
                .map(Timestamp::as_second),
            is_open: Some(self.status() != Status::Stopped),
        }
    }
}

/// The key of the single settings record.
pub(crate) const SETTINGS: &str = "settings";

impl Stored for Settings {
    const KIND: &'static str = "settings";

    fn key(&self) -> String {
        SETTINGS.to_owned()
    }
}

/// Reads and writes records on one connection or transaction.
#[derive(Debug)]
pub(crate) struct Store<'a> {
    db: &'a Connection,
    device: &'a str,
}

impl<'a> Store<'a> {
    pub fn new(db: &'a Connection, device: &'a str) -> Self {
        Self { db, device }
    }

    pub fn get<T: Stored>(&self, key: &str) -> Result<Option<T>> {
        let sql = "SELECT body FROM records WHERE kind = ?1 AND id = ?2 AND body IS NOT NULL";
        Ok(self.query(sql, params![T::KIND, key])?.pop())
    }

    pub fn all<T: Stored>(&self) -> Result<Vec<T>> {
        let sql = "SELECT body FROM records WHERE kind = ?1 AND body IS NOT NULL";
        self.query(sql, [T::KIND])
    }

    pub fn put<T: Stored>(&self, record: &T) -> Result<()> {
        let body = serde_json::to_string(record)?;
        self.write(T::KIND, &record.key(), Some(&body), &record.index())
    }

    /// Replaces the record with a tombstone, so the deletion can sync.
    pub fn delete<T: Stored>(&self, key: &str) -> Result<()> {
        self.write(T::KIND, key, None, &Index::default())
    }

    pub fn settings(&self) -> Result<Settings> {
        Ok(self.get(SETTINGS)?.unwrap_or_default())
    }

    pub fn open_sessions(&self) -> Result<Vec<Session>> {
        let sql = "SELECT body FROM records
                   WHERE kind = 'session' AND body IS NOT NULL AND is_open = 1";
        self.query(sql, [])
    }

    /// Sessions that may log time within `range`: open ones, ones belonging to
    /// a day in the range, and ones whose segments overlap it.
    pub fn sessions_in(&self, range: DateRange, tz: &TimeZone) -> Result<Vec<Session>> {
        let bounds = range.bounds(tz);
        let sql = "SELECT body FROM records
                   WHERE kind = 'session' AND body IS NOT NULL
                     AND (is_open = 1
                          OR day BETWEEN ?1 AND ?2
                          OR (span_start < ?4 AND span_end >= ?3))
                   ORDER BY day, span_start";
        self.query(
            sql,
            params![
                range.first().to_string(),
                range.last().to_string(),
                bounds.start.as_second(),
                bounds.end.as_second(),
            ],
        )
    }

    /// Writes a new revision of a record and logs it in `changes`.
    fn write(&self, kind: &str, key: &str, body: Option<&str>, index: &Index) -> Result<()> {
        let parent: Option<String> = self
            .db
            .query_row("SELECT rev FROM records WHERE id = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        let rev = Uuid::now_v7().to_string();
        self.db.execute(
            "INSERT INTO changes (rev, record, kind, parent, device, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![rev, key, kind, parent, self.device, body],
        )?;
        self.db.execute(
            "INSERT INTO records (id, kind, rev, body, day, span_start, span_end, is_open)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (id) DO UPDATE SET
                rev = excluded.rev, body = excluded.body, day = excluded.day,
                span_start = excluded.span_start, span_end = excluded.span_end,
                is_open = excluded.is_open",
            params![
                key,
                kind,
                rev,
                body,
                index.day,
                index.span_start,
                index.span_end,
                index.is_open
            ],
        )?;
        Ok(())
    }

    /// Runs a query selecting one `body` column and decodes each row.
    fn query<T: DeserializeOwned>(&self, sql: &str, params: impl Params) -> Result<Vec<T>> {
        let mut statement = self.db.prepare_cached(sql)?;
        let bodies = statement.query_map(params, |row| row.get::<_, String>(0))?;
        bodies
            .map(|body| Ok(serde_json::from_str(&body?)?))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        migrate(&mut db).unwrap();
        db
    }

    #[test]
    fn migrating_again_changes_nothing() {
        let mut db = database();
        migrate(&mut db).unwrap();
        let version: u32 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
    }

    #[test]
    fn the_device_id_is_created_once() {
        let db = database();
        assert_eq!(device_id(&db).unwrap(), device_id(&db).unwrap());
    }

    #[test]
    fn every_write_appends_a_revision_linked_to_the_last() {
        let db = database();
        let store = Store::new(&db, "this-device");
        let mut tag = Tag::new("Acme").unwrap();
        store.put(&tag).unwrap();
        tag.rename("Acme Inc").unwrap();
        store.put(&tag).unwrap();
        store.delete::<Tag>(&tag.key()).unwrap();

        let mut statement = db
            .prepare("SELECT rev, parent, body IS NULL FROM changes ORDER BY rowid")
            .unwrap();
        let changes: Vec<(String, Option<String>, bool)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].1, None);
        assert_eq!(changes[1].1.as_ref(), Some(&changes[0].0));
        assert_eq!(changes[2].1.as_ref(), Some(&changes[1].0));
        assert!(changes[2].2, "deletion leaves a tombstone");
        assert!(store.get::<Tag>(&tag.key()).unwrap().is_none());
    }
}
