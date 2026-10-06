//! Revision ancestry, validation, and materialized winners.

use std::{fmt, str::FromStr};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tinytrac_core::{Session, Status, Tag, Timestamp};
use uuid::Uuid;

use crate::{Error, Result, Settings};

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

pub(crate) fn index(kind: Kind, key: &str, body: Option<&str>) -> Result<Index> {
    match kind {
        Kind::Tag => typed_index::<Tag>(key, body),
        Kind::Session => typed_index::<Session>(key, body),
        Kind::Settings => typed_index::<Settings>(key, body),
    }
}

fn typed_index<T: Stored>(key: &str, body: Option<&str>) -> Result<Index> {
    if (T::KIND == Kind::Settings && key != "settings")
        || (T::KIND != Kind::Settings && Uuid::parse_str(key).is_err())
    {
        return Err(Error::Sync("invalid record id".into()));
    }
    let Some(body) = body else {
        return Ok(Index::default());
    };
    let record: T = serde_json::from_str(body)?;
    if record.key() != key {
        return Err(Error::Sync("body id does not match record".into()));
    }
    Ok(record.index())
}

/// A head has no known descendant naming it as a parent or merge ancestor.
pub(crate) fn heads(db: &Connection, record: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare_cached(
        "SELECT c.rev FROM changes c
        WHERE c.record = ?1
          AND NOT EXISTS (SELECT 1 FROM changes p WHERE p.record = ?1 AND p.parent = c.rev)
          AND c.rev NOT IN (SELECT j.value FROM changes m, json_each(m.merges) j
                           WHERE m.record = ?1 AND m.merges != '[]')
        ORDER BY c.rev",
    )?;
    Ok(statement
        .query_map([record], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Materialize a record once after all revisions in a batch have arrived.
pub(crate) fn merge(db: &Connection, record: &str) -> Result<()> {
    let heads = heads(db, record)?;
    let Some((winner, losers)) = heads.split_last() else {
        return Err(Error::Sync("revision graph has no head".into()));
    };
    let current: Option<String> = db
        .query_row("SELECT rev FROM records WHERE id = ?1", [record], |row| {
            row.get(0)
        })
        .optional()?;
    if current.as_deref() != Some(winner.as_str()) {
        let (kind, body): (String, Option<String>) = db.query_row(
            "SELECT kind, body FROM changes WHERE rev = ?1",
            [winner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let kind = kind.parse()?;
        replace(
            db,
            kind,
            record,
            winner,
            body.as_deref(),
            &index(kind, record, body.as_deref())?,
        )?;
    }
    db.execute("DELETE FROM conflicts WHERE record = ?1", [record])?;
    for loser in losers {
        db.execute(
            "INSERT INTO conflicts (rev, record) VALUES (?1, ?2)",
            params![loser, record],
        )?;
    }
    Ok(())
}
