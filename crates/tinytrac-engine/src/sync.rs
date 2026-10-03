//! Heads are revisions not superseded by a known parent or merge reference.
//! The greatest head wins; remaining heads are retained as conflicts.
//! Local edits supersede all known heads, clearing their conflicts.
//! Missing ancestors are allowed, so delivery order does not affect merging.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tinytrac_core::{Session, Tag};
use uuid::Uuid;

use crate::{
    Error, Result, Settings,
    store::{Index, Kind, Stored, replace},
    transport::Transport,
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub exported: usize,
    pub imported: usize,
    pub skipped: Vec<SyncIssue>,
    /// Current losing heads, retained in the revision history.
    pub conflicts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncIssue {
    pub file: Option<String>,
    pub reason: String,
}

#[derive(Serialize, Deserialize)]
struct Batch<T> {
    version: u32,
    changes: Vec<T>,
}

#[derive(Serialize, Deserialize)]
struct Change {
    rev: String,
    record: String,
    kind: Kind,
    parent: Option<String>,
    #[serde(default)]
    merges: Vec<String>,
    device: String,
    body: Option<Box<serde_json::value::RawValue>>,
}

pub(crate) fn run(
    db: &mut Connection,
    device: &str,
    transport: &impl Transport,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    match export(db, device, transport) {
        Ok(count) => report.exported = count,
        Err(error) => report.skipped.push(SyncIssue {
            file: None,
            reason: format!("export: {error}"),
        }),
    }
    match transport.list() {
        Ok(names) => {
            for name in names {
                if let Err(error) = import_file(db, transport, &name, &mut report) {
                    report.skipped.push(SyncIssue {
                        file: Some(name),
                        reason: error.to_string(),
                    });
                }
            }
        }
        Err(error) => report.skipped.push(SyncIssue {
            file: None,
            reason: format!("list: {error}"),
        }),
    }
    report.conflicts = db.query_row("SELECT count(*) FROM conflicts", [], |row| row.get(0))?;
    Ok(report)
}

fn export(db: &mut Connection, device: &str, transport: &impl Transport) -> Result<usize> {
    // Hold the writer lock through publication: only the selected rows are marked.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (last, changes) = pending(&tx, device)?;
    let mut exported = 0;
    if !changes.is_empty() {
        let bytes = serde_json::to_vec(&Batch {
            version: 1,
            changes,
        })?;
        transport.publish(&format!("{last:020}-{}.json", Uuid::now_v7()), &bytes)?;
        exported = tx.execute(
            "UPDATE changes SET exported = 1 WHERE device = ?1 AND exported = 0 AND seq <= ?2",
            params![device, last],
        )?;
    }
    tx.commit()?;
    Ok(exported)
}

fn pending(db: &Connection, device: &str) -> Result<(i64, Vec<Change>)> {
    let mut statement = db.prepare("SELECT seq, rev, record, kind, parent, device, body, merges FROM changes WHERE device = ?1 AND exported = 0 ORDER BY seq")?;
    let rows = statement.query_map([device], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut last = 0;
    let mut changes = Vec::new();
    for row in rows {
        let (seq, rev, record, kind, parent, device, body, merges) = row?;
        changes.push(Change {
            rev,
            record,
            kind: kind.parse()?,
            parent,
            device,
            body: body
                .map(serde_json::value::RawValue::from_string)
                .transpose()?,
            merges: serde_json::from_str(&merges)?,
        });
        last = seq;
    }
    Ok((last, changes))
}

fn import_file(
    db: &mut Connection,
    transport: &impl Transport,
    name: &str,
    report: &mut SyncReport,
) -> Result<()> {
    let known: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM imported_files WHERE name = ?1)",
        [name],
        |row| row.get(0),
    )?;
    if known {
        return Ok(());
    }
    let bytes = transport.read(name)?;
    // Loose values let one malformed change fail without rejecting its siblings.
    let batch = serde_json::from_slice::<Batch<serde_json::Value>>(&bytes);
    if batch.as_ref().is_err_and(serde_json::Error::is_eof) {
        // A file still being copied may end before its JSON is complete.
        return Err(batch.err().expect("checked error").into());
    }
    let mut tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut imported = 0;
    let mut skipped = Vec::new();
    let batch = batch.map_err(Error::from).and_then(|batch| {
        if batch.version != 1 {
            return Err(Error::Sync(format!("unsupported format {}", batch.version)));
        }
        Ok(batch)
    });
    match batch {
        Ok(batch) => {
            for value in batch.changes {
                let savepoint = tx.savepoint()?;
                let result = serde_json::from_value::<Change>(value)
                    .map_err(Error::from)
                    .and_then(|change| import_change(&savepoint, &change));
                match result {
                    Ok(inserted) => {
                        savepoint.commit()?;
                        imported += usize::from(inserted);
                    }
                    Err(error) if matches!(error, Error::Data(_) | Error::Sync(_)) => {
                        savepoint.finish()?;
                        skipped.push(SyncIssue {
                            file: Some(name.to_owned()),
                            reason: error.to_string(),
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Err(error) => skipped.push(SyncIssue {
            file: Some(name.to_owned()),
            reason: error.to_string(),
        }),
    }
    tx.execute("INSERT INTO imported_files (name) VALUES (?1)", [name])?;
    tx.commit()?;
    report.imported += imported;
    report.skipped.extend(skipped);
    Ok(())
}

fn import_change(db: &Connection, change: &Change) -> Result<bool> {
    let known: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM changes WHERE rev = ?1)",
        [&change.rev],
        |row| row.get(0),
    )?;
    if known {
        return Ok(false);
    }
    validate_revision(&change.rev)?;
    for parent in change.parent.iter().chain(&change.merges) {
        validate_revision(parent)?;
        if parent == &change.rev {
            return Err(Error::Sync("revision is its own parent".into()));
        }
    }
    let _ = index(
        change.kind,
        &change.record,
        change.body.as_deref().map(serde_json::value::RawValue::get),
    )?;
    let kind: Option<String> = db
        .query_row(
            "SELECT kind FROM records WHERE id = ?1",
            [&change.record],
            |row| row.get(0),
        )
        .optional()?;
    if kind.is_some_and(|kind| kind != change.kind.to_string()) {
        return Err(Error::Sync("record changed kind".into()));
    }
    let body = change.body.as_deref().map(serde_json::value::RawValue::get);
    let merges = serde_json::to_string(&change.merges)?;
    db.execute("INSERT INTO changes (rev, record, kind, parent, device, body, merges, exported) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)", params![change.rev, change.record, change.kind, change.parent, change.device, body, merges])?;
    merge(db, &change.record)?;
    Ok(true)
}

fn validate_revision(rev: &str) -> Result<()> {
    let id = Uuid::parse_str(rev).map_err(|e| Error::Sync(e.to_string()))?;
    if id.get_version_num() != 7 || id.to_string() != rev {
        return Err(Error::Sync("revision must be a canonical UUIDv7".into()));
    }
    Ok(())
}

fn index(kind: Kind, key: &str, body: Option<&str>) -> Result<Index> {
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

/// Missing parents are allowed: an ancestor may arrive in a later batch.
fn heads(revisions: &[(String, Option<String>, Vec<String>)]) -> Vec<&str> {
    let parents: HashSet<_> = revisions
        .iter()
        .flat_map(|(_, parent, merges)| parent.iter().chain(merges).map(String::as_str))
        .collect();
    let mut heads: Vec<_> = revisions
        .iter()
        .map(|(rev, _, _)| rev.as_str())
        .filter(|rev| !parents.contains(rev))
        .collect();
    heads.sort_unstable();
    heads
}

pub(crate) fn merge(db: &Connection, record: &str) -> Result<()> {
    let mut statement = db.prepare("SELECT rev, parent, merges FROM changes WHERE record = ?1")?;
    let revisions = statement
        .query_map([record], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(rev, parent, merges)| Ok((rev, parent, serde_json::from_str(&merges)?)))
        .collect::<Result<Vec<_>>>()?;
    let heads = heads(&revisions);
    let Some((winner, losers)) = heads.split_last() else {
        return Err(Error::Sync("revision graph has no head".into()));
    };
    let current: Option<String> = db
        .query_row("SELECT rev FROM records WHERE id = ?1", [record], |row| {
            row.get(0)
        })
        .optional()?;
    if current.as_deref() != Some(winner) {
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

#[cfg(test)]
mod tests {
    use super::heads;

    #[test]
    fn ancestry_beats_timestamp_and_order() {
        let mut revisions = vec![
            ("z".into(), None, vec![]),
            ("a".into(), Some("z".into()), vec![]),
            ("b".into(), Some("missing".into()), vec![]),
        ];
        assert_eq!(heads(&revisions), ["a", "b"]);
        revisions.reverse();
        assert_eq!(heads(&revisions), ["a", "b"]);
        revisions.push(("c".into(), Some("b".into()), vec!["a".into()]));
        assert_eq!(heads(&revisions), ["c"]);
        revisions.reverse();
        assert_eq!(heads(&revisions), ["c"]);
        assert!(heads(&[]).is_empty());
    }
}
