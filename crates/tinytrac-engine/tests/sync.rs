//! Two independent databases exchange only immutable folder batches.
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use jiff::{SignedDuration, Zoned, civil::date};
use rusqlite::Connection;
use tempfile::TempDir;
use tinytrac_core::{DateRange, StartMode, Status};
use tinytrac_engine::{Engine, SessionEdit, WeekStart};

struct Pair {
    a: Engine,
    b: Engine,
    root: TempDir,
    folder: PathBuf,
    clock: Arc<Mutex<Zoned>>,
}

impl Pair {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("sync");
        let clock = Arc::new(Mutex::new(
            "2026-10-01T09:00[UTC]".parse::<Zoned>().unwrap(),
        ));
        let mut a = Engine::open(root.path().join("a.db")).unwrap();
        let mut b = Engine::open(root.path().join("b.db")).unwrap();
        for engine in [&mut a, &mut b] {
            let clock = clock.clone();
            engine.set_clock(move || clock.lock().unwrap().clone());
            engine.set_sync_folder(&folder).unwrap();
        }
        Self {
            a,
            b,
            root,
            folder,
            clock,
        }
    }

    fn exchange(&mut self) {
        for _ in 0..2 {
            self.a.sync().unwrap();
            self.b.sync().unwrap();
        }
    }

    fn count(&self, device: &str, table: &str) -> i64 {
        Connection::open(self.root.path().join(format!("{device}.db")))
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = fs::read_dir(&self.folder)
            .unwrap()
            .flat_map(|dir| fs::read_dir(dir.unwrap().path()).unwrap())
            .map(|file| file.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();
        files
    }
}

fn today() -> DateRange {
    DateRange::day(date(2026, 10, 1))
}

#[test]
fn records_edits_tombstones_and_settings_propagate_idempotently() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Acme").unwrap();
    let entry = pair
        .a
        .add_entry(tag.id, date(2026, 10, 1), SignedDuration::from_mins(30))
        .unwrap();
    pair.a
        .update_settings(|s| s.week_start = WeekStart::Sunday)
        .unwrap();
    pair.exchange();
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(
        pair.a.sessions(today()).unwrap(),
        pair.b.sessions(today()).unwrap()
    );
    assert_eq!(pair.a.settings().unwrap(), pair.b.settings().unwrap());
    pair.a.rename_tag(tag.id, "New name").unwrap();
    pair.a
        .edit_session(
            entry.id,
            SessionEdit {
                logged: Some(SignedDuration::from_mins(60)),
                ..SessionEdit::default()
            },
        )
        .unwrap();
    pair.exchange();
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(
        pair.a.sessions(today()).unwrap(),
        pair.b.sessions(today()).unwrap()
    );
    pair.a.delete_session(entry.id).unwrap();
    pair.exchange();
    assert!(pair.b.sessions(today()).unwrap().is_empty());
    let counts = [
        pair.count("a", "changes"),
        pair.count("b", "changes"),
        pair.count("b", "imported_files"),
    ];
    pair.exchange();
    assert_eq!(
        counts,
        [
            pair.count("a", "changes"),
            pair.count("b", "changes"),
            pair.count("b", "imported_files")
        ]
    );
    assert_eq!(pair.a.sync().unwrap().exported, 0);
    assert_eq!(pair.b.sync().unwrap().imported, 0);
}

#[test]
fn concurrent_heads_converge_and_conflicts_are_not_duplicated() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Initial").unwrap();
    pair.exchange();
    pair.a.rename_tag(tag.id, "Desktop").unwrap();
    pair.b.rename_tag(tag.id, "Laptop").unwrap();
    pair.exchange();
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(pair.b.tags().unwrap()[0].name(), "Laptop");
    assert_eq!(pair.a.sync().unwrap().conflicts, 1);
    assert_eq!(pair.b.sync().unwrap().conflicts, 1);
    assert_eq!(pair.count("a", "conflicts"), 1);
    pair.a.rename_tag(tag.id, "Later").unwrap();
    pair.exchange();
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(pair.a.sync().unwrap().conflicts, 0);
    assert_eq!(pair.b.sync().unwrap().conflicts, 0);
}

#[test]
fn late_ancestors_do_not_replace_their_descendants() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Initial").unwrap();
    pair.a.sync().unwrap();
    let old = pair.files()[0].clone();
    let bytes = fs::read(&old).unwrap();
    fs::remove_file(&old).unwrap();
    pair.a.rename_tag(tag.id, "New").unwrap();
    pair.a.sync().unwrap();
    assert_eq!(pair.b.sync().unwrap().imported, 1);
    fs::write(&old, bytes).unwrap();
    assert_eq!(pair.b.sync().unwrap().imported, 1);
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(pair.b.sync().unwrap().conflicts, 0);
}

#[test]
fn corrupt_files_and_invalid_changes_are_reported_once() {
    let mut pair = Pair::new();
    pair.a.create_tag("Valid").unwrap();
    pair.a.sync().unwrap();
    let path = pair.files()[0].clone();
    let good = fs::read(&path).unwrap();
    let corrupt = path.with_file_name("corrupt.json");
    fs::write(&corrupt, b"broken JSON").unwrap();
    let mut batch: serde_json::Value = serde_json::from_slice(&good).unwrap();
    batch["changes"][0]["rev"] = uuid::Uuid::now_v7().to_string().into();
    batch["changes"][0]["body"]["name"] = " ".into();
    let invalid = path.with_file_name("invalid.json");
    fs::write(&invalid, serde_json::to_vec(&batch).unwrap()).unwrap();
    fs::write(path.with_file_name("ignored.tmp"), b"not JSON").unwrap();
    let report = pair.b.sync().unwrap();
    assert_eq!(report.imported, 1);
    assert_eq!(report.skipped.len(), 2);
    assert!(pair.b.sync().unwrap().skipped.is_empty());
    fs::write(&corrupt, &good).unwrap();
    fs::write(&invalid, &good).unwrap();
    assert!(pair.b.sync().unwrap().skipped.is_empty());
    assert_eq!(pair.count("b", "changes"), 1);
    assert_eq!(pair.count("b", "imported_files"), 3);
}

#[test]
fn timers_can_move_between_devices_and_offline_duplicates_are_settled() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Timer").unwrap().id;
    pair.a.start(tag, StartMode::Exclusive).unwrap();
    pair.exchange();
    *pair.clock.lock().unwrap() = "2026-10-01T10:00[UTC]".parse().unwrap();
    pair.b.stop(tag).unwrap();
    pair.exchange();
    assert!(pair.a.timers().unwrap().get(tag).is_none());
    pair.a.start(tag, StartMode::Exclusive).unwrap();
    pair.b.start(tag, StartMode::Exclusive).unwrap();
    pair.exchange();
    pair.b.pause_all().unwrap();
    pair.exchange();
    for engine in [&pair.a, &pair.b] {
        assert_eq!(
            engine
                .sessions(today())
                .unwrap()
                .iter()
                .filter(|session| session.status() != Status::Stopped)
                .count(),
            1
        );
    }
}

#[test]
fn configuration_is_local_persistent_and_excludes_the_database() {
    let mut pair = Pair::new();
    assert!(pair.a.set_sync_folder(pair.root.path()).is_err());
    assert_eq!(pair.a.sync_folder().unwrap(), Some(pair.folder.clone()));
    let reopened = Engine::open(pair.root.path().join("a.db")).unwrap();
    assert_eq!(reopened.sync_folder().unwrap(), Some(pair.folder.clone()));
    pair.a.clear_sync_folder().unwrap();
    assert!(pair.a.sync_folder().unwrap().is_none());
    assert!(pair.b.sync_folder().unwrap().is_some());
    #[cfg(unix)]
    {
        let alias = pair.root.path().join("alias");
        std::os::unix::fs::symlink(pair.root.path(), &alias).unwrap();
        assert!(pair.a.set_sync_folder(alias).is_err());
    }
}

#[test]
fn publication_failures_never_mark_unsent_changes_and_retries_are_safe() {
    let mut pair = Pair::new();
    pair.a.create_tag("First").unwrap();
    let connection = Connection::open(pair.root.path().join("a.db")).unwrap();
    let device: String = connection
        .query_row("SELECT value FROM meta WHERE key = 'device'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let directory = pair.folder.join(device);
    fs::write(&directory, b"blocks directory creation").unwrap();
    pair.b.create_tag("Remote").unwrap();
    pair.b.sync().unwrap();
    let report = pair.a.sync().unwrap();
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.imported, 1, "export failure must not block import");
    let pending = || {
        connection
            .query_row(
                "SELECT count(*) FROM changes WHERE exported = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    assert_eq!(pending(), 1);
    fs::remove_file(&directory).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_export BEFORE UPDATE OF exported ON changes BEGIN SELECT RAISE(ABORT, 'simulated commit failure'); END;").unwrap();
    assert_eq!(pair.a.sync().unwrap().skipped.len(), 1);
    assert_eq!(
        pair.files().len(),
        2,
        "publication succeeded before database failure"
    );
    assert_eq!(pending(), 1);
    let original = fs::read(&pair.files()[0]).unwrap();
    connection
        .execute_batch("DROP TRIGGER fail_export")
        .unwrap();
    assert_eq!(pair.a.sync().unwrap().exported, 1);
    assert_eq!(fs::read(&pair.files()[0]).unwrap(), original);
    assert_eq!(pending(), 0);
    assert_eq!(pair.b.sync().unwrap().imported, 1);
    pair.a.create_tag("Second").unwrap();
    pair.exchange();
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
}

#[test]
fn invalid_ids_kinds_and_bodies_do_not_prevent_valid_siblings() {
    let mut pair = Pair::new();
    pair.a.create_tag("Existing").unwrap();
    pair.exchange();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&pair.files()[0]).unwrap()).unwrap();
    let base = &original["changes"][0];
    let mut mismatch = base.clone();
    mismatch["rev"] = uuid::Uuid::now_v7().to_string().into();
    mismatch["record"] = uuid::Uuid::now_v7().to_string().into();
    let mut changed_kind = base.clone();
    changed_kind["rev"] = uuid::Uuid::now_v7().to_string().into();
    changed_kind["kind"] = "session".into();
    changed_kind["body"] = serde_json::Value::Null;
    let mut unknown = base.clone();
    unknown["rev"] = uuid::Uuid::now_v7().to_string().into();
    unknown["kind"] = "unknown".into();
    let mut valid = base.clone();
    valid["rev"] = uuid::Uuid::now_v7().to_string().into();
    valid["parent"] = base["rev"].clone();
    valid["body"]["name"] = "Accepted sibling".into();
    let path = pair.files()[0].with_file_name("mixed.json");
    let batch =
        serde_json::json!({"version": 1, "changes": [mismatch, changed_kind, unknown, valid]});
    fs::write(path, serde_json::to_vec(&batch).unwrap()).unwrap();
    let report = pair.b.sync().unwrap();
    assert_eq!(report.skipped.len(), 3);
    assert_eq!(report.imported, 1);
    assert_eq!(pair.b.tags().unwrap()[0].name(), "Accepted sibling");
    assert_eq!(pair.b.sync().unwrap().imported, 0);
    assert!(pair.b.sync().unwrap().skipped.is_empty());
    assert_eq!(pair.count("b", "changes"), 2);
}

#[test]
fn truncated_files_are_retried_until_complete() {
    let mut pair = Pair::new();
    pair.a.create_tag("Arriving").unwrap();
    pair.a.sync().unwrap();
    let path = pair.files()[0].clone();
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(pair.b.sync().unwrap().skipped.len(), 1);
    fs::write(path, bytes).unwrap();
    assert_eq!(pair.b.sync().unwrap().imported, 1);
    assert!(pair.b.sync().unwrap().skipped.is_empty());
}

#[test]
fn restored_backup_can_publish_reused_sequences() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("sync");
    let path = root.path().join("a.db");
    let backup = root.path().join("backup.db");
    let mut a = Engine::open(&path).unwrap();
    let mut b = Engine::open(root.path().join("b.db")).unwrap();
    a.set_sync_folder(&folder).unwrap();
    b.set_sync_folder(&folder).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    a.create_tag("Before restore").unwrap();
    assert_eq!(a.sync().unwrap().exported, 1);
    b.sync().unwrap();
    drop(a);
    fs::copy(backup, &path).unwrap();
    let mut a = Engine::open(&path).unwrap();
    a.create_tag("After restore").unwrap();
    b.create_tag("Remote").unwrap();
    b.sync().unwrap();
    let report = a.sync().unwrap();
    assert_eq!(report.exported, 1);
    assert_eq!(report.imported, 1);
    assert!(report.skipped.is_empty());
    assert_eq!(b.sync().unwrap().imported, 1);
    assert_eq!(b.tags().unwrap().len(), 3);
}

#[test]
fn randomized_three_device_exchange_converges() {
    for seed in [1_u64, 17, 42, 123, 999] {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("sync");
        let paths: Vec<_> = (0..3)
            .map(|i| root.path().join(format!("{i}.db")))
            .collect();
        let mut engines: Vec<_> = paths
            .iter()
            .map(|path| {
                let mut engine = Engine::open(path).unwrap();
                engine.set_sync_folder(&folder).unwrap();
                engine
            })
            .collect();
        let tag = engines[0].create_tag("Shared").unwrap().id;
        for engine in &mut engines {
            engine.sync().unwrap();
        }
        let mut state = seed;
        let mut random = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for step in 0..60 {
            let device = usize::try_from(random() % 3).unwrap();
            let engine = &mut engines[device];
            match random() % 5 {
                0 => {
                    engine.rename_tag(tag, &format!("Name {step}")).unwrap();
                }
                1 => {
                    engine
                        .update_settings(|s| {
                            s.week_start = if step % 2 == 0 {
                                WeekStart::Sunday
                            } else {
                                WeekStart::Monday
                            }
                        })
                        .unwrap();
                }
                2 => {
                    engine
                        .add_entry(tag, date(2026, 10, 1), SignedDuration::from_mins(step + 1))
                        .unwrap();
                }
                3 => {
                    if let Some(session) = engine.sessions(today()).unwrap().first() {
                        engine.delete_session(session.id).unwrap();
                    }
                }
                _ => {
                    engine.sync().unwrap();
                }
            }
        }
        for _ in 0..3 {
            for engine in &mut engines {
                assert!(engine.sync().unwrap().skipped.is_empty());
            }
        }
        let snapshot = |path: &PathBuf, sql: &str| {
            let db = Connection::open(path).unwrap();
            db.prepare(sql)
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .into_iter()
                .map(|(id, body)| {
                    (
                        id,
                        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for sql in [
            "SELECT id, coalesce(body, 'null') FROM records ORDER BY id",
            "SELECT rev, json_quote(record) FROM conflicts ORDER BY rev",
        ] {
            assert_eq!(
                snapshot(&paths[0], sql),
                snapshot(&paths[1], sql),
                "seed {seed}"
            );
            assert_eq!(
                snapshot(&paths[0], sql),
                snapshot(&paths[2], sql),
                "seed {seed}"
            );
        }
    }
}

#[test]
fn validation_after_insert_rolls_back_only_that_change() {
    let mut pair = Pair::new();
    pair.a.create_tag("Template").unwrap();
    pair.a.sync().unwrap();
    let path = pair.files()[0].clone();
    let mut batch: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut first = batch["changes"][0].clone();
    let mut second = first.clone();
    let record = uuid::Uuid::now_v7().to_string();
    let a = uuid::Uuid::now_v7().to_string();
    let b = uuid::Uuid::now_v7().to_string();
    for change in [&mut first, &mut second] {
        change["record"] = record.clone().into();
        change["body"]["id"] = record.clone().into();
    }
    first["rev"] = a.clone().into();
    first["merges"] = serde_json::json!([b]);
    second["rev"] = b.clone().into();
    second["parent"] = a.into();
    let valid = batch["changes"][0].clone();
    batch["changes"] = serde_json::json!([first, second, valid]);
    fs::write(&path, serde_json::to_vec(&batch).unwrap()).unwrap();
    let report = pair.b.sync().unwrap();
    assert_eq!(report.imported, 2);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(pair.count("b", "changes"), 2);
    assert!(pair.b.sync().unwrap().skipped.is_empty());
}
