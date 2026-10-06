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
            engine
                .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&folder).into())))
                .unwrap();
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
            folder_sync(&mut self.a).unwrap();
            folder_sync(&mut self.b).unwrap();
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
    assert_eq!(folder_sync(&mut pair.a).unwrap().exported, 0);
    assert_eq!(folder_sync(&mut pair.b).unwrap().imported, 0);
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
    assert_eq!(
        {
            folder_sync(&mut pair.a).unwrap();
            pair.a.sync_diagnostics().unwrap().conflicts
        },
        1
    );
    assert_eq!(
        {
            folder_sync(&mut pair.b).unwrap();
            pair.b.sync_diagnostics().unwrap().conflicts
        },
        1
    );
    assert_eq!(pair.count("a", "conflicts"), 1);
    pair.a.rename_tag(tag.id, "Later").unwrap();
    pair.exchange();
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(
        {
            folder_sync(&mut pair.a).unwrap();
            pair.a.sync_diagnostics().unwrap().conflicts
        },
        0
    );
    assert_eq!(
        {
            folder_sync(&mut pair.b).unwrap();
            pair.b.sync_diagnostics().unwrap().conflicts
        },
        0
    );
}

#[test]
fn late_ancestors_do_not_replace_their_descendants() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Initial").unwrap();
    folder_sync(&mut pair.a).unwrap();
    let old = pair.files()[0].clone();
    let bytes = fs::read(&old).unwrap();
    fs::remove_file(&old).unwrap();
    pair.a.rename_tag(tag.id, "New").unwrap();
    folder_sync(&mut pair.a).unwrap();
    assert_eq!(folder_sync(&mut pair.b).unwrap().imported, 1);
    fs::write(&old, bytes).unwrap();
    assert_eq!(folder_sync(&mut pair.b).unwrap().imported, 1);
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
    assert_eq!(
        {
            folder_sync(&mut pair.b).unwrap();
            pair.b.sync_diagnostics().unwrap().conflicts
        },
        0
    );
}

#[test]
fn corrupt_files_and_invalid_changes_are_reported_once() {
    let mut pair = Pair::new();
    pair.a.create_tag("Valid").unwrap();
    folder_sync(&mut pair.a).unwrap();
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
    let report = folder_sync(&mut pair.b).unwrap();
    assert_eq!(report.imported, 1);
    let diagnostics = pair.b.sync_diagnostics().unwrap();
    assert_eq!(diagnostics.rejected, 2);
    let later = folder_sync(&mut pair.b).unwrap();
    assert_eq!(pair.b.sync_diagnostics().unwrap().rejected, 2);
    assert_eq!(
        pair.b.sync_diagnostics().unwrap().reason,
        diagnostics.reason
    );
    assert!(later.skipped.is_empty());
    fs::write(&corrupt, &good).unwrap();
    fs::write(&invalid, &good).unwrap();
    assert!(folder_sync(&mut pair.b).unwrap().skipped.is_empty());
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
    *pair.clock.lock().unwrap() = "2026-10-01T10:10[UTC]".parse().unwrap();
    pair.b.pause_all().unwrap();
    pair.exchange();
    *pair.clock.lock().unwrap() = "2026-10-01T12:00[UTC]".parse().unwrap();
    assert_eq!(
        pair.a.totals(today()).unwrap(),
        pair.b.totals(today()).unwrap()
    );
    assert_eq!(
        pair.a.totals(today()).unwrap().total(),
        SignedDuration::from_mins(190)
    );
    assert!(matches!(
        pair.a.timers().unwrap().get(tag).unwrap().status(),
        Status::Running(_)
    ));
    assert_eq!(
        {
            folder_sync(&mut pair.a).unwrap();
            pair.a.sync_diagnostics().unwrap().conflicts
        },
        0
    );
    assert_eq!(
        {
            folder_sync(&mut pair.b).unwrap();
            pair.b.sync_diagnostics().unwrap().conflicts
        },
        0
    );
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
    assert!(
        pair.a
            .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder(
                pair.root.path().into()
            )))
            .is_err()
    );
    assert_eq!(
        pair.a.sync_target().unwrap(),
        Some(tinytrac_engine::SyncTarget::Folder(pair.folder.clone()))
    );
    let reopened = Engine::open(pair.root.path().join("a.db")).unwrap();
    assert_eq!(
        reopened.sync_target().unwrap(),
        Some(tinytrac_engine::SyncTarget::Folder(pair.folder.clone()))
    );
    pair.a.set_sync_target(None).unwrap();
    assert!(pair.a.sync_target().unwrap().is_none());
    assert!(pair.b.sync_target().unwrap().is_some());
    #[cfg(unix)]
    {
        let alias = pair.root.path().join("alias");
        std::os::unix::fs::symlink(pair.root.path(), &alias).unwrap();
        assert!(
            pair.a
                .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder(alias)))
                .is_err()
        );
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
    folder_sync(&mut pair.b).unwrap();
    let report = folder_sync(&mut pair.a).unwrap();
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
    assert_eq!(folder_sync(&mut pair.a).unwrap().skipped.len(), 1);
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
    assert_eq!(folder_sync(&mut pair.a).unwrap().exported, 1);
    assert_eq!(fs::read(&pair.files()[0]).unwrap(), original);
    assert_eq!(pending(), 0);
    assert_eq!(folder_sync(&mut pair.b).unwrap().imported, 1);
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
    let report = folder_sync(&mut pair.b).unwrap();
    assert_eq!(pair.b.sync_diagnostics().unwrap().rejected, 3);
    assert_eq!(report.imported, 1);
    assert_eq!(pair.b.tags().unwrap()[0].name(), "Accepted sibling");
    assert_eq!(folder_sync(&mut pair.b).unwrap().imported, 0);
    assert!(folder_sync(&mut pair.b).unwrap().skipped.is_empty());
    assert_eq!(pair.count("b", "changes"), 2);
}

#[test]
fn truncated_files_are_retried_until_complete() {
    let mut pair = Pair::new();
    pair.a.create_tag("Arriving").unwrap();
    folder_sync(&mut pair.a).unwrap();
    let path = pair.files()[0].clone();
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(folder_sync(&mut pair.b).unwrap().skipped.len(), 1);
    fs::write(path, bytes).unwrap();
    assert_eq!(folder_sync(&mut pair.b).unwrap().imported, 1);
    assert!(folder_sync(&mut pair.b).unwrap().skipped.is_empty());
}

#[test]
fn restored_backup_can_publish_reused_sequences() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("sync");
    let path = root.path().join("a.db");
    let backup = root.path().join("backup.db");
    let mut a = Engine::open(&path).unwrap();
    let mut b = Engine::open(root.path().join("b.db")).unwrap();
    a.set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&folder).into())))
        .unwrap();
    b.set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&folder).into())))
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    a.create_tag("Before restore").unwrap();
    assert_eq!(folder_sync(&mut a).unwrap().exported, 1);
    folder_sync(&mut b).unwrap();
    drop(a);
    fs::copy(backup, &path).unwrap();
    let mut a = Engine::open(&path).unwrap();
    a.create_tag("After restore").unwrap();
    b.create_tag("Remote").unwrap();
    folder_sync(&mut b).unwrap();
    let report = folder_sync(&mut a).unwrap();
    assert_eq!(report.exported, 1);
    assert_eq!(report.imported, 1);
    assert!(report.skipped.is_empty());
    assert_eq!(folder_sync(&mut b).unwrap().imported, 1);
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
                engine
                    .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&folder).into())))
                    .unwrap();
                engine
            })
            .collect();
        let tag = engines[0].create_tag("Shared").unwrap().id;
        for engine in &mut engines {
            folder_sync(engine).unwrap();
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
                    folder_sync(engine).unwrap();
                }
            }
        }
        for _ in 0..3 {
            for engine in &mut engines {
                assert!(folder_sync(engine).unwrap().skipped.is_empty());
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
fn a_headless_graph_rejects_its_whole_file_but_valid_files_import() {
    let mut pair = Pair::new();
    pair.a.create_tag("Template").unwrap();
    folder_sync(&mut pair.a).unwrap();
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
    fs::write(
        path.with_file_name("valid.json"),
        serde_json::to_vec(&batch).unwrap(),
    )
    .unwrap();
    let valid = batch["changes"][0].clone();
    batch["changes"] = serde_json::json!([first, second, valid]);
    fs::write(&path, serde_json::to_vec(&batch).unwrap()).unwrap();
    let report = folder_sync(&mut pair.b).unwrap();
    assert_eq!(report.imported, 1);
    assert_eq!(pair.b.sync_diagnostics().unwrap().rejected, 1);
    assert_eq!(pair.count("b", "changes"), 1);
    assert!(folder_sync(&mut pair.b).unwrap().skipped.is_empty());
}

#[test]
#[ignore = "release-mode import benchmark"]
fn import_two_thousand_pause_resume_cycles() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Benchmark").unwrap().id;
    pair.a.start(tag, StartMode::Exclusive).unwrap();
    for _ in 0..2000 {
        pair.a.pause(tag).unwrap();
        pair.a.start(tag, StartMode::Exclusive).unwrap();
    }
    folder_sync(&mut pair.a).unwrap();
    let start = std::time::Instant::now();
    let report = folder_sync(&mut pair.b).unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "2000 pause/resume cycles: {elapsed:?} ({} revisions)",
        report.imported
    );
    assert_eq!(report.imported, 4002);
    assert!(report.skipped.is_empty());
    assert_eq!(pair.b.sync_diagnostics().unwrap().rejected, 0);
    assert_eq!(
        pair.a.sessions(today()).unwrap(),
        pair.b.sessions(today()).unwrap()
    );
}

#[test]
fn switching_folders_republishes_foreign_revisions_and_tombstones() {
    let mut pair = Pair::new();
    let tag = pair.b.create_tag("From another device").unwrap();
    let entry = pair
        .b
        .add_entry(tag.id, date(2026, 10, 1), SignedDuration::from_mins(1))
        .unwrap();
    pair.exchange();
    pair.b.delete_session(entry.id).unwrap();
    pair.exchange();
    pair.a
        .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder(
            (&pair.folder).into(),
        )))
        .unwrap();
    assert_eq!(folder_sync(&mut pair.a).unwrap().exported, 0);
    let fresh = pair.root.path().join("fresh");
    pair.a
        .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&fresh).into())))
        .unwrap();
    let expected = usize::try_from(pair.count("a", "changes")).unwrap();
    assert_eq!(folder_sync(&mut pair.a).unwrap().exported, expected);
    let mut peer = Engine::open(pair.root.path().join("fresh.db")).unwrap();
    peer.set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&fresh).into())))
        .unwrap();
    assert_eq!(folder_sync(&mut peer).unwrap().imported, expected);
    assert_eq!(peer.tags().unwrap(), pair.a.tags().unwrap());
    assert!(peer.sessions(today()).unwrap().is_empty());
    pair.a.set_sync_target(None).unwrap();
    pair.a
        .set_sync_target(Some(tinytrac_engine::SyncTarget::Folder((&fresh).into())))
        .unwrap();
    assert_eq!(folder_sync(&mut pair.a).unwrap().exported, expected);
    assert_eq!(folder_sync(&mut peer).unwrap().imported, 0);
}

#[test]
fn newer_versions_remain_retryable_and_do_not_block_other_files() {
    let mut pair = Pair::new();
    pair.a.create_tag("Readable").unwrap();
    folder_sync(&mut pair.a).unwrap();
    let file = pair.files()[0].with_file_name("future.json");
    fs::write(&file, br#"{"version":2,"changes":{"new-format":true}}"#).unwrap();
    for expected in [1, 0] {
        let report = folder_sync(&mut pair.b).unwrap();
        assert_eq!(report.imported, expected);
        assert_eq!(pair.b.sync_diagnostics().unwrap().rejected, 0);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("update this device"));
        assert_eq!(pair.count("b", "imported_files"), 1);
    }
    fs::write(file, br#"{"version":1,"changes":[]}"#).unwrap();
    assert!(folder_sync(&mut pair.b).unwrap().skipped.is_empty());
    assert_eq!(pair.count("b", "imported_files"), 2);
}

#[test]
fn rejections_remain_visible_after_reopen_and_when_sync_is_disabled() {
    let mut pair = Pair::new();
    pair.a.create_tag("Valid").unwrap();
    folder_sync(&mut pair.a).unwrap();
    fs::write(pair.files()[0].with_file_name("bad.json"), b"invalid JSON").unwrap();
    folder_sync(&mut pair.b).unwrap();
    let first = pair.b.sync_diagnostics().unwrap();
    assert_eq!(first.rejected, 1);
    assert!(first.reason.is_some());
    let mut reopened = Engine::open(pair.root.path().join("b.db")).unwrap();
    reopened.set_sync_target(None).unwrap();
    assert!(folder_sync(&mut reopened).unwrap().skipped.is_empty());
    let later = reopened.sync_diagnostics().unwrap();
    assert_eq!(later.rejected, first.rejected);
    assert_eq!(later.reason, first.reason);
}

#[test]
fn partial_three_device_repairs_converge_without_further_revisions() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Offline").unwrap().id;
    pair.exchange();
    let mut c = Engine::open(pair.root.path().join("c.db")).unwrap();
    c.set_sync_target(Some(tinytrac_engine::SyncTarget::Folder(
        (&pair.folder).into(),
    )))
    .unwrap();
    folder_sync(&mut c).unwrap();
    for (engine, time) in [
        (&mut pair.a, "09:00"),
        (&mut pair.b, "09:05"),
        (&mut c, "09:10"),
    ] {
        let now: Zoned = format!("2026-10-01T{time}[UTC]").parse().unwrap();
        engine.set_clock(move || now.clone());
        engine.start(tag, StartMode::Exclusive).unwrap();
    }
    c.pause_all().unwrap();
    // B repairs with only A known; C sees both. Repair clocks differ wildly.
    for (engine, day) in [(&mut pair.a, "02"), (&mut pair.b, "03"), (&mut c, "04")] {
        let now: Zoned = format!("2026-10-{day}T12:00[UTC]").parse().unwrap();
        engine.set_clock(move || now.clone());
        folder_sync(engine).unwrap();
    }
    for _ in 0..4 {
        pair.exchange();
        folder_sync(&mut c).unwrap();
    }
    let records = |device: &str| {
        let db = Connection::open(pair.root.path().join(format!("{device}.db"))).unwrap();
        db.prepare("SELECT id, rev, body FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(records("a"), records("b"));
    assert_eq!(records("a"), records("c"));
    for engine in [&pair.a, &pair.b, &c] {
        assert_eq!(
            engine
                .sessions(today())
                .unwrap()
                .iter()
                .filter(|s| s.status() != Status::Stopped)
                .count(),
            engine.timers().unwrap().iter().count()
        );
    }
    let before = [
        pair.count("a", "changes"),
        pair.count("b", "changes"),
        pair.count("c", "changes"),
    ];
    for engine in [&mut pair.a, &mut pair.b, &mut c] {
        let report = folder_sync(engine).unwrap();
        assert_eq!((report.imported, report.exported), (0, 0));
    }
    assert_eq!(
        before,
        [
            pair.count("a", "changes"),
            pair.count("b", "changes"),
            pair.count("c", "changes")
        ]
    );
}

#[test]
fn kind_validation_uses_unmaterialized_history() {
    let mut pair = Pair::new();
    pair.a.create_tag("New record").unwrap();
    folder_sync(&mut pair.a).unwrap();
    let file = pair.files()[0].clone();
    let mut batch: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut wrong_kind = batch["changes"][0].clone();
    wrong_kind["rev"] = uuid::Uuid::now_v7().to_string().into();
    wrong_kind["kind"] = "session".into();
    wrong_kind["body"] = serde_json::Value::Null;
    batch["changes"].as_array_mut().unwrap().push(wrong_kind);
    fs::write(file, serde_json::to_vec(&batch).unwrap()).unwrap();
    let report = folder_sync(&mut pair.b).unwrap();
    assert_eq!(
        (report.imported, pair.b.sync_diagnostics().unwrap().rejected),
        (1, 1)
    );
    assert!(
        pair.b
            .sync_diagnostics()
            .unwrap()
            .reason
            .unwrap()
            .contains("record changed kind")
    );
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
}

#[test]
fn identical_heads_are_hidden_in_counts_but_merged_by_the_next_write() {
    let mut pair = Pair::new();
    let tag = pair.a.create_tag("Original").unwrap().id;
    pair.exchange();
    pair.a.rename_tag(tag, "Identical").unwrap();
    pair.b.rename_tag(tag, "Identical").unwrap();
    pair.exchange();
    for engine in [&mut pair.a, &mut pair.b] {
        assert_eq!(
            {
                folder_sync(engine).unwrap();
                engine.sync_diagnostics().unwrap().conflicts
            },
            0
        );
    }
    assert_eq!(pair.count("a", "conflicts"), 1);
    assert_eq!(pair.count("b", "conflicts"), 1);
    pair.a.rename_tag(tag, "Resolved").unwrap();
    pair.exchange();
    assert_eq!(pair.count("a", "conflicts"), 0);
    assert_eq!(pair.count("b", "conflicts"), 0);
    assert_eq!(pair.a.tags().unwrap(), pair.b.tags().unwrap());
}

fn folder_sync(engine: &mut Engine) -> tinytrac_engine::Result<tinytrac_engine::SyncRun> {
    let Some(context) = engine.sync_context()? else {
        return Ok(tinytrac_engine::SyncRun::default());
    };
    let mut transport = tinytrac_engine::Folder::for_context(&context).unwrap();
    engine.sync(&mut transport)
}
