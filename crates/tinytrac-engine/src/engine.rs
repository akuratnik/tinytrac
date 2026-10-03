use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use jiff::{SignedDuration, Zoned, civil::Date};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use tinytrac_core::{DateRange, Session, SessionId, StartMode, Status, Tag, TagId, Timers, Totals};

use crate::{
    Error, Result, Settings,
    store::{self, Store},
};

/// The application's single entry point: every command and query the UI needs.
///
/// Commands run in one transaction each and return the records they changed,
/// so a UI can update its view without reloading everything.
pub struct Engine {
    db: Connection,
    device: String,
    clock: Box<dyn Fn() -> Zoned + Send>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Opens (or creates) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_connection(Connection::open(path)?)
    }

    /// A throwaway database, for tests and demos.
    pub fn open_in_memory() -> Result<Self> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(mut db: Connection) -> Result<Self> {
        db.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        // Another process may hold the write lock briefly; wait for it, but not
        // long enough to freeze a UI.
        db.busy_timeout(Duration::from_secs(2))?;
        store::migrate(&mut db)?;
        let device = store::device_id(&db)?;
        Ok(Self {
            db,
            device,
            clock: Box::new(Zoned::now),
        })
    }

    /// Replaces the system clock and time zone, e.g. in tests.
    pub fn set_clock(&mut self, clock: impl Fn() -> Zoned + Send + 'static) {
        self.clock = Box::new(clock);
    }

    pub fn now(&self) -> Zoned {
        (self.clock)()
    }

    // Tags

    /// All tags, archived ones included, sorted by name.
    pub fn tags(&self) -> Result<Vec<Tag>> {
        let mut tags: Vec<Tag> = self.read(Store::all)?;
        tags.sort_by_cached_key(|tag| tag.name().to_lowercase());
        Ok(tags)
    }

    pub fn create_tag(&mut self, name: &str) -> Result<Tag> {
        let tag = Tag::new(name)?;
        self.write(|store| {
            ensure_unique_name(store, &tag)?;
            store.put(&tag)
        })?;
        Ok(tag)
    }

    pub fn rename_tag(&mut self, id: TagId, name: &str) -> Result<Tag> {
        self.update_tag(id, |tag, store| {
            tag.rename(name)?;
            ensure_unique_name(store, tag)
        })
    }

    pub fn set_tag_archived(&mut self, id: TagId, archived: bool) -> Result<Tag> {
        self.update_tag(id, |tag, _| {
            tag.archived = archived;
            Ok(())
        })
    }

    // Timers

    /// The open timers. Duplicates that sync may bring in show as one and are
    /// resolved by the next timer command; see [`Timers::load`].
    pub fn timers(&self) -> Result<Timers> {
        let now = self.now().timestamp();
        Ok(Timers::load(self.read(Store::open_sessions)?, now).0)
    }

    /// Starts or resumes `tag`'s timer; see [`Timers::start`].
    pub fn start(&mut self, tag: TagId, mode: StartMode) -> Result<Vec<Session>> {
        self.update_timers(|timers, store, now| {
            require_tag(store, tag)?;
            let breaks = store.settings()?.breaks();
            Ok(timers.start(tag, mode, breaks, now))
        })
    }

    pub fn pause(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, _, now| Ok(vec![timers.pause(tag, now.timestamp())?]))
    }

    pub fn pause_all(&mut self) -> Result<Vec<Session>> {
        self.update_timers(|timers, _, now| Ok(timers.pause_all(now.timestamp())))
    }

    pub fn start_break(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, _, now| Ok(vec![timers.start_break(tag, now.timestamp())?]))
    }

    pub fn end_break(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, _, now| Ok(vec![timers.end_break(tag, now.timestamp())?]))
    }

    pub fn stop(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, _, now| Ok(vec![timers.stop(tag, now.timestamp())?]))
    }

    // Entries

    /// Adds time by hand: `logged` on `day` for `tag`.
    pub fn add_entry(&mut self, tag: TagId, day: Date, logged: SignedDuration) -> Result<Session> {
        let session = Session::manual(tag, day, logged)?;
        self.write(|store| {
            require_tag(store, tag)?;
            store.put(&session)
        })?;
        Ok(session)
    }

    /// Applies `edit` to a session, running or finished, in one transaction.
    pub fn edit_session(&mut self, id: SessionId, edit: SessionEdit) -> Result<Session> {
        let now = self.now().timestamp();
        self.update_session(id, |session, store| {
            if let Some(tag) = edit.tag.filter(|tag| *tag != session.tag) {
                require_tag(store, tag)?;
                let busy = session.status() != Status::Stopped
                    && store
                        .open_sessions()?
                        .iter()
                        .any(|open| open.tag == tag && open.id != id);
                if busy {
                    return Err(Error::TagBusy);
                }
                session.tag = tag;
            }
            if let Some(logged) = edit.logged {
                session.set_logged(logged, now)?;
            }
            Ok(())
        })
    }

    pub fn delete_session(&mut self, id: SessionId) -> Result<()> {
        self.write(|store| {
            require_session(store, id)?;
            store.delete::<Session>(&id.to_string())
        })
    }

    // Reports

    /// Every session that may log time within `range`: what
    /// [`Totals::compute`] needs. That includes all open timers and sessions
    /// that merely touch the range, so filter by `day` to list a day's entries.
    pub fn sessions(&self, range: DateRange) -> Result<Vec<Session>> {
        let now = self.now();
        self.read(|store| store.sessions_in(range, now.time_zone()))
    }

    /// Logged time per day and tag within `range`.
    pub fn totals(&self, range: DateRange) -> Result<Totals> {
        let now = self.now();
        let sessions = self.read(|store| store.sessions_in(range, now.time_zone()))?;
        Ok(Totals::compute(
            &sessions,
            range,
            now.time_zone(),
            now.timestamp(),
        ))
    }

    // Settings

    pub fn settings(&self) -> Result<Settings> {
        self.read(Store::settings)
    }

    /// Changes the stored settings in place, so a concurrent change to another
    /// field survives. Break bank changes apply to timers started afterwards.
    pub fn update_settings(&mut self, change: impl FnOnce(&mut Settings)) -> Result<Settings> {
        self.write(|store| {
            let mut settings = store.settings()?;
            change(&mut settings);
            store.put(&settings)?;
            Ok(settings)
        })
    }

    /// The folder is local to this database; it is never part of synced settings.
    pub fn sync_folder(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .db
            .query_row(
                "SELECT value FROM meta WHERE key = 'sync_folder'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(PathBuf::from))
    }

    pub fn set_sync_folder(&mut self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::create_dir_all(path.as_ref())?;
        let path = std::fs::canonicalize(path)?;
        self.check_sync_folder(&path)?;
        let value = path
            .to_str()
            .ok_or_else(|| Error::Sync("sync folder must be UTF-8".into()))?;
        self.db.execute("INSERT INTO meta (key, value) VALUES ('sync_folder', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [value])?;
        Ok(())
    }

    pub fn clear_sync_folder(&mut self) -> Result<()> {
        self.db
            .execute("DELETE FROM meta WHERE key = 'sync_folder'", [])?;
        Ok(())
    }

    /// Publishes local changes before importing other devices' immutable batches.
    pub fn sync(&mut self) -> Result<crate::SyncReport> {
        let Some(root) = self.sync_folder()? else {
            return Ok(crate::SyncReport::default());
        };
        let root = std::fs::canonicalize(root)?;
        self.check_sync_folder(&root)?;
        let folder = crate::transport::Folder {
            root,
            device: self.device.clone(),
        };
        crate::sync::run(&mut self.db, &self.device, &folder)
    }

    fn check_sync_folder(&self, folder: &Path) -> Result<()> {
        if let Some(path) = self.db.path().filter(|path| !path.is_empty()) {
            let database = std::fs::canonicalize(path)?;
            if database.starts_with(folder) {
                return Err(Error::Sync(
                    "sync folder must not contain the database".into(),
                ));
            }
        }
        Ok(())
    }

    // Helpers

    fn read<'a, T>(&'a self, query: impl FnOnce(&Store<'a>) -> Result<T>) -> Result<T> {
        query(&Store::new(&self.db, &self.device))
    }

    /// Runs `command` in a transaction that takes the write lock up front, so it
    /// can't fail halfway when another connection writes.
    fn write<T>(&mut self, command: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let output = command(&Store::new(&tx, &self.device))?;
        tx.commit()?;
        Ok(output)
    }

    /// Loads the timers, applies `action`, and persists every session that
    /// changed, including duplicates that loading stopped.
    fn update_timers(
        &mut self,
        action: impl FnOnce(&mut Timers, &Store, &Zoned) -> Result<Vec<Session>>,
    ) -> Result<Vec<Session>> {
        let now = self.now();
        self.write(|store| {
            let (mut timers, mut changed) = Timers::load(store.open_sessions()?, now.timestamp());
            changed.extend(action(&mut timers, store, &now)?);
            for session in &changed {
                store.put(session)?;
            }
            Ok(changed)
        })
    }

    fn update_tag(
        &mut self,
        id: TagId,
        edit: impl FnOnce(&mut Tag, &Store) -> Result<()>,
    ) -> Result<Tag> {
        self.write(|store| {
            let mut tag = require_tag(store, id)?;
            edit(&mut tag, store)?;
            store.put(&tag)?;
            Ok(tag)
        })
    }

    fn update_session(
        &mut self,
        id: SessionId,
        edit: impl FnOnce(&mut Session, &Store) -> Result<()>,
    ) -> Result<Session> {
        self.write(|store| {
            let mut session = require_session(store, id)?;
            edit(&mut session, store)?;
            store.put(&session)?;
            Ok(session)
        })
    }
}

/// Changes to a session, applied together by [`Engine::edit_session`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionEdit {
    /// Move the session to this tag.
    pub tag: Option<TagId>,
    /// Set the session's elapsed time; see [`Session::set_logged`].
    pub logged: Option<SignedDuration>,
}

/// Tag names are unique, ignoring case, so reports never show two alike.
fn ensure_unique_name(store: &Store, tag: &Tag) -> Result<()> {
    let name = tag.name().to_lowercase();
    let taken = store
        .all::<Tag>()?
        .iter()
        .any(|other| other.id != tag.id && other.name().to_lowercase() == name);
    if taken {
        return Err(Error::DuplicateTag);
    }
    Ok(())
}

fn require_tag(store: &Store, id: TagId) -> Result<Tag> {
    store.get(&id.to_string())?.ok_or(Error::NotFound("tag"))
}

fn require_session(store: &Store, id: SessionId) -> Result<Session> {
    store
        .get(&id.to_string())?
        .ok_or(Error::NotFound("session"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_open_timers_from_sync_are_settled_and_saved() {
        let mut engine = Engine::open_in_memory().unwrap();
        let tag = engine.create_tag("Acme").unwrap().id;
        let now = engine.now();
        let (desktop, laptop) = (
            Session::start(tag, None, &now),
            Session::start(tag, None, &now),
        );
        engine
            .write(|store| {
                store.put(&desktop)?;
                store.put(&laptop)
            })
            .unwrap();

        let revisions = |engine: &Engine| -> i64 {
            engine
                .db
                .query_row("SELECT count(*) FROM changes", [], |row| row.get(0))
                .unwrap()
        };
        let before = revisions(&engine);
        let changed = engine.pause_all().unwrap();

        assert_eq!(changed.len(), 2, "the duplicate stopped, the other paused");
        assert_eq!(revisions(&engine) - before, 2, "one revision per change");
        assert_eq!(engine.read(Store::open_sessions).unwrap().len(), 1);
    }
}
