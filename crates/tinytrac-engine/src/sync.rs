//! Heads are revisions not superseded by a known parent or merge reference.
//! The greatest head wins; remaining heads are retained as conflicts.
//! Local edits supersede all known heads, clearing their conflicts.
//! Missing ancestors are allowed, so delivery order does not affect merging.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use uuid::Uuid;

use crate::{
    Error, Result,
    revisions::{Kind, index, merge},
    store::Store,
    transport::Transport,
};

/// Local destination identity. Remote labels are display-only.
///
/// The serde encoding is persisted in `meta.sync_target`; migration 004 depends on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncTarget {
    Folder(std::path::PathBuf),
    Remote { id: String, label: String },
}

impl SyncTarget {
    /// Folder paths and remote ids define identity; display labels do not.
    pub fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Folder(a), Self::Folder(b)) => a == b,
            (Self::Remote { id: a, .. }, Self::Remote { id: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// An immutable transport binding obtained from [`crate::Engine::sync_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncContext {
    pub(crate) target: SyncTarget,
    pub(crate) epoch: i64,
    pub(crate) device: String,
}

impl SyncContext {
    /// Bindings are reusable only for the same epoch, device, and destination.
    /// Display label changes do not invalidate a transport.
    pub fn compatible_with(&self, other: &Self) -> bool {
        self.epoch == other.epoch
            && self.device == other.device
            && self.target.same_identity(&other.target)
    }

    pub fn target(&self) -> &SyncTarget {
        &self.target
    }
    pub fn epoch(&self) -> i64 {
        self.epoch
    }
    pub fn device(&self) -> &str {
        &self.device
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncRun {
    pub exported: usize,
    pub imported: usize,
    /// Retryable problems from this run only.
    pub skipped: Vec<SyncIssue>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncDiagnostics {
    /// Rejected changes (or malformed files) remembered for this folder.
    pub rejected: i64,
    /// Most recently recorded permanent rejection.
    pub reason: Option<String>,
    /// Losing heads with a body different from the winner.
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
    changes: T,
}

#[derive(Serialize, Deserialize)]
struct Change<B> {
    rev: String,
    record: String,
    kind: Kind,
    parent: Option<String>,
    #[serde(default)]
    merges: Vec<String>,
    device: String,
    body: Option<B>,
}

// Borrowed RawValue needs an explicit serde borrow lifetime, unlike generic Batch.
#[derive(Deserialize)]
struct Envelope<'a> {
    version: u32,
    #[serde(borrow)]
    changes: Option<&'a RawValue>,
}

pub(crate) fn run(
    db: &mut Connection,
    device: &str,
    selected_epoch: i64,
    transport: &mut (impl Transport + ?Sized),
) -> Result<SyncRun> {
    let mut report = SyncRun::default();
    match export(db, selected_epoch, transport) {
        Ok(count) => report.exported = count,
        Err(error @ Error::AuthRequired(_)) => return Err(error),
        Err(error) => report.skipped.push(SyncIssue {
            file: None,
            reason: format!("export: {error}"),
        }),
    }
    match transport.list() {
        Ok(names) => {
            for name in names {
                if epoch(db)? != selected_epoch {
                    break;
                }
                if let Err(error) =
                    import_file(db, device, selected_epoch, transport, &name, &mut report)
                {
                    if matches!(error, Error::AuthRequired(_)) {
                        return Err(error);
                    }
                    report.skipped.push(SyncIssue {
                        file: Some(name),
                        reason: error.to_string(),
                    });
                }
            }
        }
        Err(error @ Error::AuthRequired(_)) => return Err(error),
        Err(error) => report.skipped.push(SyncIssue {
            file: None,
            reason: format!("list: {error}"),
        }),
    }
    Ok(report)
}

pub(crate) fn diagnostics(db: &Connection) -> Result<SyncDiagnostics> {
    Ok(SyncDiagnostics {
        conflicts: db.query_row("SELECT count(*) FROM conflicts c JOIN changes h ON h.rev = c.rev JOIN records r ON r.id = c.record WHERE h.body IS NOT r.body", [], |row| row.get(0))?,
        rejected: db.query_row("SELECT coalesce(sum(rejected), 0) FROM imported_files", [], |row| row.get(0))?,
        reason: db.query_row("SELECT reason FROM imported_files WHERE reason IS NOT NULL ORDER BY rowid DESC LIMIT 1", [], |row| row.get(0)).optional()?,
    })
}

pub(crate) fn epoch(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'sync_epoch'",
        [],
        |row| row.get(0),
    )?)
}

fn export(
    db: &mut Connection,
    selected_epoch: i64,
    transport: &mut (impl Transport + ?Sized),
) -> Result<usize> {
    let mut exported = 0;
    loop {
        let tx = db.transaction()?;
        if epoch(&tx)? != selected_epoch {
            return Ok(exported);
        }
        let (last, changes) = pending(&tx)?;
        tx.commit()?;
        if changes.is_empty() {
            return Ok(exported);
        }
        let bytes = serde_json::to_vec(&Batch {
            version: 1,
            changes: &changes,
        })?;
        transport.publish(&format!("{last:020}-{}.json", Uuid::now_v7()), &bytes)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if epoch(&tx)? != selected_epoch {
            return Ok(exported);
        }
        for change in &changes {
            exported += tx
                .prepare_cached("UPDATE changes SET exported = 1 WHERE rev = ?1 AND exported = 0")?
                .execute([&change.rev])?;
        }
        tx.commit()?;
    }
}

fn pending(db: &Connection) -> Result<(i64, Vec<Change<Box<RawValue>>>)> {
    let mut statement = db.prepare("SELECT seq, rev, record, kind, parent, device, body, merges FROM changes WHERE exported = 0 ORDER BY seq LIMIT 1000")?;
    let rows = statement.query_map([], |row| {
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
            body: body.map(RawValue::from_string).transpose()?,
            merges: serde_json::from_str(&merges)?,
        });
        last = seq;
    }
    Ok((last, changes))
}

fn import_file(
    db: &mut Connection,
    device: &str,
    selected_epoch: i64,
    transport: &mut (impl Transport + ?Sized),
    name: &str,
    report: &mut SyncRun,
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
    if epoch(db)? != selected_epoch {
        return Ok(());
    }
    // Inspect the version before interpreting the version-specific payload.
    let envelope = serde_json::from_slice::<Envelope>(&bytes);
    if let Ok(envelope) = &envelope
        && envelope.version != 1
    {
        return Err(Error::Sync(
            "written by a newer tinytrac; update this device".into(),
        ));
    }
    let batch = envelope.and_then(|envelope| {
        serde_json::from_str::<Vec<&RawValue>>(envelope.changes.map_or("null", RawValue::get))
    });
    if batch.as_ref().is_err_and(serde_json::Error::is_eof) {
        return Err(batch.expect_err("checked error").into());
    }
    let mut tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if epoch(&tx)? != selected_epoch {
        return Ok(());
    }
    let mut file = tx.savepoint()?;
    let mut imported = 0;
    let mut rejected = 0;
    let mut reason = None;
    let mut affected = BTreeSet::new();
    match batch {
        Ok(batch) => {
            for value in batch {
                let savepoint = file.savepoint()?;
                let result = serde_json::from_str::<Change<&RawValue>>(value.get())
                    .map_err(Error::from)
                    .and_then(|change| {
                        let inserted = import_change(&savepoint, &change)?;
                        Ok(inserted.then_some(change.record))
                    });
                match result {
                    Ok(record) => {
                        savepoint.commit()?;
                        if let Some(record) = record {
                            imported += 1;
                            affected.insert(record);
                        }
                    }
                    Err(error) if matches!(error, Error::Data(_) | Error::Sync(_)) => {
                        savepoint.finish()?;
                        rejected += 1;
                        reason = Some(error.to_string());
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Err(error) => {
            rejected += 1;
            reason = Some(error.to_string());
        }
    }
    let merged = affected.iter().try_for_each(|record| merge(&file, record));
    match merged {
        Ok(()) => file.commit()?,
        Err(Error::Sync(error)) => {
            file.finish()?;
            imported = 0;
            rejected = 1;
            reason = Some(error);
        }
        Err(error) => return Err(error),
    }
    Store::new(&tx, device).settle()?;
    tx.execute(
        "INSERT INTO imported_files (name, rejected, reason) VALUES (?1, ?2, ?3)",
        params![name, rejected, reason],
    )?;
    tx.commit()?;
    report.imported += imported;
    Ok(())
}

fn import_change(db: &Connection, change: &Change<&RawValue>) -> Result<bool> {
    let known: bool = db
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM changes WHERE rev = ?1)")?
        .query_row([&change.rev], |row| row.get(0))?;
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
    let _ = index(change.kind, &change.record, change.body.map(RawValue::get))?;
    let kind: Option<String> = db
        .prepare_cached("SELECT kind FROM changes WHERE record = ?1 LIMIT 1")?
        .query_row([&change.record], |row| row.get(0))
        .optional()?;
    if kind.is_some_and(|kind| kind != change.kind.to_string()) {
        return Err(Error::Sync("record changed kind".into()));
    }
    let body = change.body.map(RawValue::get);
    let merges = serde_json::to_string(&change.merges)?;
    db.prepare_cached("INSERT INTO changes (rev, record, kind, parent, device, body, merges, exported) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)")?.execute(params![change.rev, change.record, change.kind, change.parent, change.device, body, merges])?;
    Ok(true)
}

fn validate_revision(rev: &str) -> Result<()> {
    let id = Uuid::parse_str(rev).map_err(|e| Error::Sync(e.to_string()))?;
    if id.get_version_num() != 7 || id.to_string() != rev {
        return Err(Error::Sync("revision must be a canonical UUIDv7".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::Engine;
    use tinytrac_core::Tag;

    #[test]
    fn target_json_matches_the_persisted_migration_format() {
        for (target, json) in [
            (
                SyncTarget::Folder("sync".into()),
                serde_json::json!({"Folder": "sync"}),
            ),
            (
                SyncTarget::Remote {
                    id: "account".into(),
                    label: "Personal".into(),
                },
                serde_json::json!({"Remote": {"id": "account", "label": "Personal"}}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&target).unwrap(), json);
            assert_eq!(serde_json::from_value::<SyncTarget>(json).unwrap(), target);
        }
    }

    struct Publisher<F>(F, SyncContext);

    impl<F: Fn(&[u8])> Transport for Publisher<F> {
        fn context(&self) -> &SyncContext {
            &self.1
        }
        fn publish(&mut self, _: &str, bytes: &[u8]) -> Result<()> {
            self.0(bytes);
            Ok(())
        }
        fn list(&mut self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn read(&mut self, _: &str) -> Result<Vec<u8>> {
            unreachable!()
        }
    }

    #[test]
    fn a_stale_epoch_stops_before_reading_or_parsing_more_files() {
        struct Switching<'a> {
            context: SyncContext,
            engine: RefCell<&'a mut Engine>,
            reads: RefCell<usize>,
            during_list: bool,
        }
        impl Transport for Switching<'_> {
            fn context(&self) -> &SyncContext {
                &self.context
            }
            fn publish(&mut self, _: &str, _: &[u8]) -> Result<()> {
                Ok(())
            }
            fn list(&mut self) -> Result<Vec<String>> {
                if self.during_list {
                    self.engine.borrow_mut().set_sync_target(None)?;
                }
                Ok(vec!["first.json".into(), "second.json".into()])
            }
            fn read(&mut self, _: &str) -> Result<Vec<u8>> {
                *self.reads.borrow_mut() += 1;
                self.engine.borrow_mut().set_sync_target(None)?;
                // Parsing this would produce a retryable unsupported-version issue.
                Ok(br#"{"version":2}"#.to_vec())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let mut engine = Engine::open(&path).unwrap();
        let mut db = Connection::open(&path).unwrap();
        for during_list in [true, false] {
            engine
                .set_sync_target(Some(SyncTarget::Remote {
                    id: "test".into(),
                    label: "Test".into(),
                }))
                .unwrap();
            let selected = epoch(&db).unwrap();
            let mut transport = Switching {
                context: engine.sync_context().unwrap().unwrap(),
                engine: RefCell::new(&mut engine),
                reads: RefCell::new(0),
                during_list,
            };
            let run = run(&mut db, "test", selected, &mut transport).unwrap();
            assert!(run.skipped.is_empty());
            assert_eq!(*transport.reads.borrow(), usize::from(!during_list));
            assert_eq!(run.imported, 0);
        }
        assert_eq!(diagnostics(&db).unwrap().rejected, 0);
    }

    #[test]
    fn publication_releases_the_lock_and_a_folder_switch_preserves_pending_rows() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let mut engine = Engine::open(&path).unwrap();
        engine
            .set_sync_target(Some(SyncTarget::Folder(root.path().join("old"))))
            .unwrap();
        let mut db = Connection::open(&path).unwrap();
        let tx = db.transaction().unwrap();
        let store = Store::new(&tx, "test-device");
        for number in 0..1001 {
            store
                .put(&Tag::new(&format!("Tag {number}")).unwrap())
                .unwrap();
        }
        tx.commit().unwrap();
        let selected = epoch(&db).unwrap();
        let mut switch = Publisher(
            |_: &[u8]| {
                // A second writer can commit while publication is in progress.
                let mut other = Engine::open(&path).unwrap();
                other.create_tag("During publication").unwrap();
                other
                    .set_sync_target(Some(SyncTarget::Folder(root.path().join("new"))))
                    .unwrap();
            },
            engine.sync_context().unwrap().unwrap(),
        );
        assert_eq!(export(&mut db, selected, &mut switch).unwrap(), 0);
        let pending: i64 = db
            .query_row(
                "SELECT count(*) FROM changes WHERE exported = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1002);
        let sizes = RefCell::new(Vec::new());
        let mut publisher = Publisher(
            |bytes: &[u8]| {
                let batch: Batch<Vec<&RawValue>> = serde_json::from_slice(bytes).unwrap();
                sizes.borrow_mut().push(batch.changes.len());
            },
            engine.sync_context().unwrap().unwrap(),
        );
        assert_eq!(export(&mut db, selected, &mut publisher).unwrap(), 0);
        let selected = epoch(&db).unwrap();
        assert_eq!(export(&mut db, selected, &mut publisher).unwrap(), 1002);
        assert_eq!(*sizes.borrow(), [1000, 2]);
    }
}
