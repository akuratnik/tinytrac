use rusqlite::Connection;
use tinytrac_engine::{Engine, Error, Folder, Result, SyncContext, SyncTarget, Transport};

struct Memory {
    context: SyncContext,
    calls: Vec<&'static str>,
    auth: Option<&'static str>,
}

impl Memory {
    fn new(context: SyncContext) -> Self {
        Self {
            context,
            calls: Vec::new(),
            auth: None,
        }
    }
    fn call(&mut self, operation: &'static str) -> Result<()> {
        self.calls.push(operation);
        if self.auth == Some(operation) {
            return Err(Error::AuthRequired("sign in again".into()));
        }
        Ok(())
    }
}

impl Transport for Memory {
    fn context(&self) -> &SyncContext {
        &self.context
    }
    fn publish(&mut self, _: &str, _: &[u8]) -> Result<()> {
        self.call("publish")
    }
    fn list(&mut self) -> Result<Vec<String>> {
        self.call("list")?;
        Ok(vec!["opaque remote id".into()])
    }
    fn read(&mut self, key: &str) -> Result<Vec<u8>> {
        assert_eq!(key, "opaque remote id");
        self.call("read")?;
        Ok(br#"{"version":1,"changes":[]}"#.to_vec())
    }
}

fn remote(id: &str, label: &str) -> SyncTarget {
    SyncTarget::Remote {
        id: id.into(),
        label: label.into(),
    }
}

#[test]
fn stale_contexts_do_no_transport_work_even_after_returning_to_a() {
    let mut engine = Engine::open_in_memory().unwrap();
    engine.set_sync_target(Some(remote("a", "A"))).unwrap();
    let context = engine.sync_context().unwrap().unwrap();
    let mut transport = Memory::new(context.clone());
    for target in [Some(remote("b", "B")), Some(remote("a", "A")), None] {
        engine.set_sync_target(target).unwrap();
        assert!(matches!(
            engine.sync(&mut transport),
            Err(Error::StaleSyncContext)
        ));
        assert!(transport.calls.is_empty());
    }
}

#[test]
fn label_changes_preserve_export_and_import_progress_and_accept_old_context() {
    let mut engine = Engine::open_in_memory().unwrap();
    engine
        .set_sync_target(Some(remote("a", "Old label")))
        .unwrap();
    engine.create_tag("Work").unwrap();
    let context = engine.sync_context().unwrap().unwrap();
    let mut transport = Memory::new(context.clone());
    assert_eq!(engine.sync(&mut transport).unwrap().exported, 1);
    engine
        .set_sync_target(Some(remote("a", "New label")))
        .unwrap();
    let current = engine.sync_context().unwrap().unwrap();
    assert_eq!(current.epoch(), context.epoch());
    assert_eq!(current.target(), &remote("a", "New label"));
    transport.calls.clear();
    let dynamic: &mut dyn Transport = &mut transport;
    assert_eq!(engine.sync(dynamic).unwrap().exported, 0);
    assert_eq!(transport.calls, ["list"]);
}

#[test]
fn authentication_aborts_at_each_transport_operation_and_read_is_retried() {
    for operation in ["publish", "list", "read"] {
        let mut engine = Engine::open_in_memory().unwrap();
        engine.set_sync_target(Some(remote("a", "A"))).unwrap();
        if operation == "publish" {
            engine.create_tag("Work").unwrap();
        }
        let context = engine.sync_context().unwrap().unwrap();
        let mut transport = Memory {
            auth: Some(operation),
            ..Memory::new(context.clone())
        };
        assert!(
            matches!(engine.sync(&mut transport), Err(Error::AuthRequired(message)) if message == "sign in again")
        );
        assert_eq!(transport.calls.last(), Some(&operation));
        assert_eq!(engine.sync_diagnostics().unwrap().rejected, 0);
        transport.auth = None;
        transport.calls.clear();
        let run = engine.sync(&mut transport).unwrap();
        assert_eq!(run.exported, usize::from(operation == "publish"));
        assert!(transport.calls.contains(&"read"));
    }
}

#[test]
fn migration_from_three_preserves_folder_progress() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    let folder = root.path().join("sync");
    std::fs::create_dir(&folder).unwrap();
    let folder = folder.canonicalize().unwrap();
    let db = Connection::open(&path).unwrap();
    for sql in [
        include_str!("../src/migrations/001_initial.sql"),
        include_str!("../src/migrations/002_sync.sql"),
        include_str!("../src/migrations/003_sync_repairs.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    db.pragma_update(None, "user_version", 3).unwrap();
    db.execute(
        "INSERT INTO meta (key, value) VALUES ('sync_folder', ?1)",
        [folder.to_str().unwrap()],
    )
    .unwrap();
    db.execute_batch("UPDATE meta SET value = '17' WHERE key = 'sync_epoch';
        INSERT INTO changes (rev, record, kind, device, exported) VALUES ('old', 'old', 'tag', 'peer', 1);
        INSERT INTO imported_files (name, rejected, reason) VALUES ('peer/old.json', 2, 'old rejection');").unwrap();
    let mut engine = Engine::open(&path).unwrap();
    engine
        .set_sync_target(Some(SyncTarget::Folder(folder.clone())))
        .unwrap();
    let context = engine.sync_context().unwrap().unwrap();
    assert_eq!(context.target(), &SyncTarget::Folder(folder));
    assert_eq!(context.epoch(), 17);
    let mut transport = Folder::for_context(&context).unwrap();
    assert_eq!(engine.sync(&mut transport).unwrap().exported, 0);
    assert_eq!(engine.sync_diagnostics().unwrap().rejected, 2);
    assert_eq!(
        db.query_row("SELECT name FROM imported_files", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "peer/old.json"
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[cfg(unix)]
#[test]
fn folder_is_rechecked_before_each_run() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("sync");
    let mut engine = Engine::open(root.path().join("db")).unwrap();
    engine
        .set_sync_target(Some(SyncTarget::Folder(folder.clone())))
        .unwrap();
    let context = engine.sync_context().unwrap().unwrap();
    std::fs::remove_dir(&folder).unwrap();
    std::os::unix::fs::symlink(root.path(), &folder).unwrap();
    let mut transport = Memory::new(context.clone());
    assert!(matches!(
        engine.sync(&mut transport),
        Err(Error::SyncFolderMoved)
    ));
    assert!(transport.calls.is_empty());
}

#[test]
fn missing_folder_fails_before_transport_work() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("sync");
    let mut engine = Engine::open_in_memory().unwrap();
    engine
        .set_sync_target(Some(SyncTarget::Folder(folder.clone())))
        .unwrap();
    let mut transport = Memory::new(engine.sync_context().unwrap().unwrap());
    std::fs::remove_dir(&folder).unwrap();
    assert!(matches!(engine.sync(&mut transport), Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::NotFound));
    assert!(transport.calls.is_empty());
    assert!(!folder.exists());
}

#[test]
fn empty_remote_ids_leave_the_current_target_and_epoch_unchanged() {
    let mut engine = Engine::open_in_memory().unwrap();
    engine
        .set_sync_target(Some(remote("valid", "Account")))
        .unwrap();
    let before = engine.sync_context().unwrap();
    for id in ["", " ", "\t\n", "\u{2003}"] {
        assert!(matches!(
            engine.set_sync_target(Some(remote(id, "Invalid"))),
            Err(Error::Sync(_))
        ));
        assert_eq!(engine.sync_context().unwrap(), before);
    }
}

#[test]
fn migration_four_without_a_folder_preserves_all_metadata() {
    let db = Connection::open_in_memory().unwrap();
    for sql in [
        include_str!("../src/migrations/001_initial.sql"),
        include_str!("../src/migrations/002_sync.sql"),
        include_str!("../src/migrations/003_sync_repairs.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    let metadata = || {
        db.prepare("SELECT key, value FROM meta ORDER BY key")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = metadata();
    db.execute_batch(include_str!("../src/migrations/004_sync_targets.sql"))
        .unwrap();
    assert_eq!(metadata(), before);
}

#[test]
fn authentication_on_the_second_key_keeps_the_first_import_and_retries_only_the_second() {
    struct TwoKeys {
        context: SyncContext,
        batches: Vec<Vec<u8>>,
        reads: Vec<String>,
        auth: bool,
        collecting: bool,
    }
    impl Transport for TwoKeys {
        fn context(&self) -> &SyncContext {
            &self.context
        }
        fn publish(&mut self, _: &str, bytes: &[u8]) -> Result<()> {
            self.batches.push(bytes.to_vec());
            Ok(())
        }
        fn list(&mut self) -> Result<Vec<String>> {
            Ok(if self.collecting {
                Vec::new()
            } else {
                vec!["0".into(), "1".into()]
            })
        }
        fn read(&mut self, key: &str) -> Result<Vec<u8>> {
            self.reads.push(key.into());
            if key == "1" && self.auth {
                return Err(Error::AuthRequired("sign in".into()));
            }
            Ok(self.batches[key.parse::<usize>().unwrap()].clone())
        }
    }
    let mut source = Engine::open_in_memory().unwrap();
    source.set_sync_target(Some(remote("a", "A"))).unwrap();
    let mut transport = TwoKeys {
        context: source.sync_context().unwrap().unwrap(),
        batches: Vec::new(),
        reads: Vec::new(),
        auth: true,
        collecting: true,
    };
    // Capture two real batches without importing them into the source.
    for name in ["First", "Second"] {
        source.create_tag(name).unwrap();
        source.sync(&mut transport).unwrap();
    }
    let mut engine = Engine::open_in_memory().unwrap();
    engine.set_sync_target(Some(remote("a", "A"))).unwrap();
    transport.collecting = false;
    transport.context = engine.sync_context().unwrap().unwrap();
    transport.reads.clear();
    assert!(matches!(
        engine.sync(&mut transport),
        Err(Error::AuthRequired(_))
    ));
    assert_eq!(engine.tags().unwrap().len(), 1);
    assert_eq!(engine.tags().unwrap()[0].name(), "First");
    assert_eq!(transport.reads, ["0", "1"]);
    transport.auth = false;
    transport.reads.clear();
    assert_eq!(engine.sync(&mut transport).unwrap().imported, 1);
    assert_eq!(transport.reads, ["1"]);
    assert_eq!(engine.tags().unwrap().len(), 2);
}
