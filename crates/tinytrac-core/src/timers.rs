use std::collections::{BTreeMap, btree_map::Entry};

use jiff::{SignedDuration, Timestamp, Zoned};

use crate::{Activity, BreakPolicy, Session, SessionError, SessionId, Status, TagId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimerError {
    #[error("there is no open timer for this tag")]
    NoTimer,
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// How starting a timer affects the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// Pause every other running timer (the default click).
    Exclusive,
    /// Leave other timers running (ctrl+click).
    Alongside,
}

/// The open (not yet stopped) sessions, one per tag.
///
/// Every change goes through a method that returns the sessions it touched, so
/// callers know exactly what to persist.
#[derive(Debug, Clone, Default)]
pub struct Timers {
    open: BTreeMap<TagId, Session>,
}

impl Timers {
    /// Builds the timers from stored sessions, skipping stopped ones.
    ///
    /// Two devices that were offline can each open a session for the same tag.
    /// The most recently active one is kept and the others are stopped at `now`,
    /// keeping their recorded time. Returns those stopped sessions for persisting.
    pub fn load(
        sessions: impl IntoIterator<Item = Session>,
        now: Timestamp,
    ) -> (Self, Vec<Session>) {
        let mut open = BTreeMap::new();
        let mut stopped = Vec::new();
        for session in sessions {
            if session.status() == Status::Stopped {
                continue;
            }
            let mut older = match open.entry(session.tag) {
                Entry::Vacant(entry) => {
                    entry.insert(session);
                    continue;
                }
                Entry::Occupied(mut entry) if last_active(&session) > last_active(entry.get()) => {
                    entry.insert(session)
                }
                Entry::Occupied(_) => session,
            };
            if older.stop(now).is_ok() {
                stopped.push(older);
            }
        }
        (Self { open }, stopped)
    }

    /// The open sessions, ordered by tag.
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.open.values()
    }

    pub fn get(&self, tag: TagId) -> Option<&Session> {
        self.open.get(&tag)
    }

    /// Starts working on `tag`: resumes its paused session, ends its break, or
    /// opens a new session with `breaks`.
    ///
    /// Returns every session that changed, for persisting.
    pub fn start(
        &mut self,
        tag: TagId,
        mode: StartMode,
        breaks: Option<BreakPolicy>,
        now: &Zoned,
    ) -> Vec<Session> {
        let at = now.timestamp();
        let mut changed = match mode {
            StartMode::Exclusive => self.pause_where(at, |other| other != tag),
            StartMode::Alongside => Vec::new(),
        };
        if let Some(session) = self.open.get_mut(&tag) {
            let started = match session.status() {
                Status::Running(Activity::Break) => session.end_break(at),
                _ => session.resume(at),
            };
            if started.is_ok() {
                changed.push(session.clone());
            }
        } else {
            let session = Session::start(tag, breaks, now);
            changed.push(session.clone());
            self.open.insert(tag, session);
        }
        changed
    }

    /// Pauses every running timer. Returns the sessions that changed.
    pub fn pause_all(&mut self, now: Timestamp) -> Vec<Session> {
        self.pause_where(now, |_| true)
    }

    pub fn pause(&mut self, tag: TagId, now: Timestamp) -> Result<Session, TimerError> {
        self.apply(tag, |session| session.pause(now))
    }

    pub fn start_break(&mut self, tag: TagId, now: Timestamp) -> Result<Session, TimerError> {
        self.apply(tag, |session| session.start_break(now))
    }

    pub fn end_break(&mut self, tag: TagId, now: Timestamp) -> Result<Session, TimerError> {
        self.apply(tag, |session| session.end_break(now))
    }

    /// Sets the elapsed time of `tag`'s open session; see [`Session::set_logged`].
    pub fn set_logged(
        &mut self,
        tag: TagId,
        target: SignedDuration,
        now: Timestamp,
    ) -> Result<Session, TimerError> {
        self.apply(tag, |session| session.set_logged(target, now))
    }

    /// Stops `tag`'s timer and hands back the finished session.
    pub fn stop(&mut self, tag: TagId, now: Timestamp) -> Result<Session, TimerError> {
        let Entry::Occupied(mut entry) = self.open.entry(tag) else {
            return Err(TimerError::NoTimer);
        };
        entry.get_mut().stop(now)?;
        Ok(entry.remove())
    }

    fn apply(
        &mut self,
        tag: TagId,
        action: impl FnOnce(&mut Session) -> Result<(), SessionError>,
    ) -> Result<Session, TimerError> {
        let session = self.open.get_mut(&tag).ok_or(TimerError::NoTimer)?;
        action(session)?;
        Ok(session.clone())
    }

    fn pause_where(&mut self, now: Timestamp, filter: impl Fn(TagId) -> bool) -> Vec<Session> {
        self.open
            .iter_mut()
            .filter(|(tag, _)| filter(**tag))
            .filter_map(|(_, session)| session.pause(now).is_ok().then(|| session.clone()))
            .collect()
    }
}

/// When a session last changed state, with the id as a deterministic tiebreak.
fn last_active(session: &Session) -> (Option<Timestamp>, SessionId) {
    let last = session.segments().last();
    (
        last.map(|segment| segment.end.unwrap_or(segment.start)),
        session.id,
    )
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;

    use super::*;

    const MIN: SignedDuration = SignedDuration::from_mins(1);

    fn at(minute: i64) -> Zoned {
        (Timestamp::UNIX_EPOCH + MIN * i32::try_from(minute).unwrap()).to_zoned(TimeZone::UTC)
    }

    fn running(timers: &Timers) -> Vec<TagId> {
        let mut tags: Vec<_> = timers
            .iter()
            .filter(|s| s.status() == Status::Running(Activity::Work))
            .map(|s| s.tag)
            .collect();
        tags.sort();
        tags
    }

    #[test]
    fn starting_pauses_the_others_unless_alongside() {
        let (a, b, c) = (TagId::generate(), TagId::generate(), TagId::generate());
        let mut timers = Timers::default();

        timers.start(a, StartMode::Exclusive, None, &at(0));
        let changed = timers.start(b, StartMode::Exclusive, None, &at(10));
        assert_eq!(changed.len(), 2, "a paused, b started");
        assert_eq!(running(&timers), [b]);

        timers.start(c, StartMode::Alongside, None, &at(20));
        let mut both = [b, c];
        both.sort();
        assert_eq!(running(&timers), both);
    }

    #[test]
    fn starting_an_open_tag_resumes_its_session() {
        let tag = TagId::generate();
        let mut timers = Timers::default();
        let first = timers.start(tag, StartMode::Exclusive, None, &at(0))[0].id;
        timers.pause_all(at(10).timestamp());

        let resumed = timers.start(tag, StartMode::Exclusive, None, &at(20));
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].id, first);
        assert_eq!(timers.iter().count(), 1);
    }

    #[test]
    fn starting_a_timer_on_a_break_ends_the_break() {
        let tag = TagId::generate();
        let mut timers = Timers::default();
        timers.start(
            tag,
            StartMode::Exclusive,
            Some(BreakPolicy::default()),
            &at(0),
        );
        timers.start_break(tag, at(25).timestamp()).unwrap();

        let changed = timers.start(tag, StartMode::Exclusive, None, &at(30));
        assert_eq!(changed[0].status(), Status::Running(Activity::Work));
    }

    #[test]
    fn set_logged_edits_an_open_timer() {
        let tag = TagId::generate();
        let mut timers = Timers::default();
        timers.start(tag, StartMode::Exclusive, None, &at(0));
        timers
            .set_logged(tag, MIN * 30, at(60).timestamp())
            .unwrap();

        let paused = timers.pause(tag, at(70).timestamp()).unwrap();
        assert_eq!(paused.logged(at(70).timestamp()), MIN * 40);
    }

    #[test]
    fn actions_on_a_tag_without_a_timer_fail() {
        let mut timers = Timers::default();
        let tag = TagId::generate();
        assert_eq!(
            timers.pause(tag, at(0).timestamp()),
            Err(TimerError::NoTimer)
        );
    }

    #[test]
    fn stop_removes_the_timer() {
        let tag = TagId::generate();
        let mut timers = Timers::default();
        timers.start(tag, StartMode::Exclusive, None, &at(0));

        let finished = timers.stop(tag, at(30).timestamp()).unwrap();
        assert_eq!(finished.logged(at(99).timestamp()), MIN * 30);
        assert!(timers.get(tag).is_none());
        assert_eq!(
            timers.stop(tag, at(40).timestamp()),
            Err(TimerError::NoTimer)
        );
    }

    #[test]
    fn loading_two_open_sessions_for_one_tag_keeps_the_latest() {
        let tag = TagId::generate();
        let desktop = Session::start(tag, None, &at(0));
        let laptop = Session::start(tag, None, &at(5));

        let (timers, stopped) = Timers::load([laptop.clone(), desktop.clone()], at(10).timestamp());
        assert_eq!(timers.get(tag).map(|s| s.id), Some(laptop.id));
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].id, desktop.id);
        assert_eq!(stopped[0].logged(at(99).timestamp()), MIN * 10);
    }
}
