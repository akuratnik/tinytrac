//! Typed access to the SQLite database.
//!
//! Records are stored whole, as JSON, in one table, with a few lookup columns
//! derived from them. Every write also appends a revision to `changes`, which is
//! what sync exchanges between devices.

use std::{fmt, str::FromStr};

use rusqlite::{Connection, OptionalExtension, Params, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tinytrac_core::{DateRange, Session, Status, Tag, TimeZone, Timestamp};
use uuid::Uuid;

use crate::{Error, Result, Settings};

const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_initial.sql"),
    include_str!("migrations/002_sync.sql"),
];

/// Brings the schema up to date, tracking progress in `user_version`. Refuses a
/// database written by a newer version, which this one might damage.
pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    let applied: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let supported = u32::try_from(MIGRATIONS.len()).expect("a handful of migrations");
    if applied > supported {
        return Err(Error::NewerDatabase(applied));
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Tag,
    Session,
    Settings,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tag => "tag",
            Self::Session => "session",
            Self::Settings => "settings",
        })
    }
}

impl FromStr for Kind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "tag" => Ok(Self::Tag),
            "session" => Ok(Self::Session),
            "settings" => Ok(Self::Settings),
            _ => Err(Error::Sync(format!("unknown kind: {value}"))),
        }
    }
}

impl rusqlite::ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

/// A kind of record kept in the `records` table.
pub(crate) trait Stored: Serialize + DeserializeOwned {
    const KIND: Kind;

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
    const KIND: Kind = Kind::Tag;

    fn key(&self) -> String {
        self.id.to_string()
    }
}

impl Stored for Session {
    const KIND: Kind = Kind::Session;

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
    const KIND: Kind = Kind::Settings;

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
                   ORDER BY day, span_start, id";
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
    fn write(&self, kind: Kind, key: &str, body: Option<&str>, index: &Index) -> Result<()> {
        let parent: Option<String> = self
            .db
            .prepare_cached("SELECT rev FROM records WHERE id = ?1")?
            .query_row([key], |row| row.get(0))
            .optional()?;
        let latest: Option<String> = self.db.query_row(
            "SELECT max(rev) FROM changes WHERE device = ?1",
            [self.device],
            |row| row.get(0),
        )?;
        let rev = next_revision(Uuid::now_v7(), latest.as_deref())?;
        let merges = self
            .db
            .prepare_cached("SELECT rev FROM conflicts WHERE record = ?1 ORDER BY rev")?
            .query_map([key], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let merges = serde_json::to_string(&merges)?;
        self.db
            .prepare_cached(
                "INSERT INTO changes (rev, record, kind, parent, device, body, merges)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![rev, key, kind, parent, self.device, body, merges])?;
        replace(self.db, kind, key, &rev, body, index)?;
        crate::sync::merge(self.db, key)?;
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

pub(crate) fn replace(
    db: &Connection,
    kind: Kind,
    key: &str,
    rev: &str,
    body: Option<&str>,
    index: &Index,
) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO records (id, kind, rev, body, day, span_start, span_end, is_open)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (id) DO UPDATE SET
                    rev = excluded.rev, body = excluded.body, day = excluded.day,
                    span_start = excluded.span_start, span_end = excluded.span_end,
                    is_open = excluded.is_open",
    )?
    .execute(params![
        key,
        kind,
        rev,
        body,
        index.day,
        index.span_start,
        index.span_end,
        index.is_open
    ])?;
    Ok(())
}

/// Advance the timestamp if the wall clock stepped back, preserving `UUIDv7` bits.
fn next_revision(candidate: Uuid, latest: Option<&str>) -> Result<String> {
    let Some(latest) = latest.filter(|latest| *latest >= candidate.to_string().as_str()) else {
        return Ok(candidate.to_string());
    };
    let previous = Uuid::parse_str(latest).map_err(|e| Error::Sync(e.to_string()))?;
    let mut timestamp = [0; 8];
    timestamp[2..].copy_from_slice(&previous.as_bytes()[..6]);
    let next = u64::from_be_bytes(timestamp) + 1;
    if next >= 1 << 48 {
        return Err(Error::Sync("revision timestamp exhausted".into()));
    }
    let mut bytes = *candidate.as_bytes();
    bytes[..6].copy_from_slice(&next.to_be_bytes()[2..]);
    Ok(Uuid::from_bytes(bytes).to_string())
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
    fn revisions_advance_after_clock_rollback() {
        let db = database();
        let future = "ffffffff-ff00-7fff-bfff-ffffffffffff";
        db.execute("INSERT INTO changes (rev, record, kind, device) VALUES (?1, 'old', 'tag', 'this-device')", [future]).unwrap();
        for _ in 0..2 {
            Store::new(&db, "this-device")
                .put(&Tag::new("New").unwrap())
                .unwrap();
        }
        let revisions = db
            .prepare("SELECT rev FROM changes ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            revisions
                .iter()
                .all(|rev| Uuid::parse_str(rev).unwrap().get_version_num() == 7)
        );
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
    fn a_database_from_a_newer_version_is_refused() {
        let mut db = database();
        db.pragma_update(None, "user_version", 99).unwrap();
        assert!(matches!(migrate(&mut db), Err(Error::NewerDatabase(99))));
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
            .prepare("SELECT rev, parent, body IS NULL FROM changes ORDER BY seq")
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
