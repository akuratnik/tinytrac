use std::path::Path;

use jiff::{SignedDuration, Zoned, civil::Date};
use rusqlite::Connection;
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
        self.write(|store| store.put(&tag))?;
        Ok(tag)
    }

    pub fn rename_tag(&mut self, id: TagId, name: &str) -> Result<Tag> {
        self.update_tag(id, |tag| Ok(tag.rename(name)?))
    }

    pub fn set_tag_archived(&mut self, id: TagId, archived: bool) -> Result<Tag> {
        self.update_tag(id, |tag| {
            tag.archived = archived;
            Ok(())
        })
    }

    // Timers

    /// The open timers. Loading them also stops duplicates sync may have brought in.
    pub fn timers(&mut self) -> Result<Timers> {
        let now = self.now();
        self.write(|store| Ok(load_timers(store, &now)?.0))
    }

    /// Starts or resumes `tag`'s timer; see [`Timers::start`].
    pub fn start(&mut self, tag: TagId, mode: StartMode) -> Result<Vec<Session>> {
        let breaks = self.read(|store| {
            require_tag(store, tag)?;
            Ok(store.settings()?.breaks())
        })?;
        self.update_timers(|timers, now| Ok(timers.start(tag, mode, breaks, now)))
    }

    pub fn pause(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, now| Ok(vec![timers.pause(tag, now.timestamp())?]))
    }

    pub fn pause_all(&mut self) -> Result<Vec<Session>> {
        self.update_timers(|timers, now| Ok(timers.pause_all(now.timestamp())))
    }

    pub fn start_break(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, now| Ok(vec![timers.start_break(tag, now.timestamp())?]))
    }

    pub fn end_break(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, now| Ok(vec![timers.end_break(tag, now.timestamp())?]))
    }

    pub fn stop(&mut self, tag: TagId) -> Result<Vec<Session>> {
        self.update_timers(|timers, now| Ok(vec![timers.stop(tag, now.timestamp())?]))
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

    /// Sets a session's elapsed time, whether it is running or finished.
    pub fn set_logged(&mut self, id: SessionId, logged: SignedDuration) -> Result<Session> {
        let now = self.now().timestamp();
        self.update_session(id, |session, _| Ok(session.set_logged(logged, now)?))
    }

    /// Moves a session to another tag.
    pub fn retag(&mut self, id: SessionId, tag: TagId) -> Result<Session> {
        self.update_session(id, |session, store| {
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

    /// Sessions that log time within `range`, ordered by day.
    pub fn sessions(&self, range: DateRange) -> Result<Vec<Session>> {
        let tz = self.now().time_zone().clone();
        self.read(|store| store.sessions_in(range, &tz))
    }

    /// Logged time per day and tag within `range`.
    pub fn totals(&self, range: DateRange) -> Result<Totals> {
        let now = self.now();
        let sessions = self.sessions(range)?;
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

    /// Saves settings. Changes to the break bank apply to timers started afterwards.
    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        self.write(|store| store.put(settings))
    }

    // Helpers

    fn read<'a, T>(&'a self, query: impl FnOnce(&Store<'a>) -> Result<T>) -> Result<T> {
        query(&Store::new(&self.db, &self.device))
    }

    fn write<T>(&mut self, command: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let tx = self.db.transaction()?;
        let output = command(&Store::new(&tx, &self.device))?;
        tx.commit()?;
        Ok(output)
    }

    /// Loads the timers, applies `action`, and persists every session it changed.
    fn update_timers(
        &mut self,
        action: impl FnOnce(&mut Timers, &Zoned) -> Result<Vec<Session>>,
    ) -> Result<Vec<Session>> {
        let now = self.now();
        self.write(|store| {
            let (mut timers, mut changed) = load_timers(store, &now)?;
            changed.extend(action(&mut timers, &now)?);
            for session in &changed {
                store.put(session)?;
            }
            Ok(changed)
        })
    }

    fn update_tag(&mut self, id: TagId, edit: impl FnOnce(&mut Tag) -> Result<()>) -> Result<Tag> {
        self.write(|store| {
            let mut tag = require_tag(store, id)?;
            edit(&mut tag)?;
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

/// Loads the open timers, persisting any duplicates [`Timers::load`] stopped.
/// Returns those stopped sessions too, as they changed.
fn load_timers(store: &Store, now: &Zoned) -> Result<(Timers, Vec<Session>)> {
    let (timers, stopped) = Timers::load(store.open_sessions()?, now.timestamp());
    for session in &stopped {
        store.put(session)?;
    }
    Ok((timers, stopped))
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

        assert_eq!(engine.timers().unwrap().iter().count(), 1);
        assert_eq!(engine.read(Store::open_sessions).unwrap().len(), 1);
    }
}
