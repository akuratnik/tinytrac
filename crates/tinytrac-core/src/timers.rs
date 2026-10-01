use jiff::{Timestamp, Zoned};

use crate::{BreakPolicy, Session, SessionError, Status, TagId};

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

/// The open (not yet stopped) sessions, at most one per tag.
///
/// Every change goes through a method that returns the sessions it touched, so
/// callers know exactly what to persist.
#[derive(Debug, Clone, Default)]
pub struct Timers {
    open: Vec<Session>,
}

impl Timers {
    /// Wraps the open sessions. Stopped sessions are ignored.
    pub fn new(sessions: impl IntoIterator<Item = Session>) -> Self {
        let open = sessions
            .into_iter()
            .filter(|session| session.status() != Status::Stopped)
            .collect();
        Self { open }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.open.iter()
    }

    pub fn get(&self, tag: TagId) -> Option<&Session> {
        self.position(tag).map(|index| &self.open[index])
    }

    /// Starts `tag`'s timer, resuming its open session if there is one.
    /// `breaks` applies only if a new session is created.
    ///
    /// Returns every session that changed, for persisting.
    pub fn start(
        &mut self,
        tag: TagId,
        mode: StartMode,
        breaks: Option<BreakPolicy>,
        now: &Zoned,
    ) -> Vec<Session> {
        let mut changed = match mode {
            StartMode::Exclusive => self.pause_where(now.timestamp(), |s| s.tag != tag),
            StartMode::Alongside => Vec::new(),
        };
        if let Some(index) = self.position(tag) {
            let session = &mut self.open[index];
            if session.resume(now.timestamp()).is_ok() {
                changed.push(session.clone());
            }
        } else {
            let session = Session::start(tag, breaks, now);
            changed.push(session.clone());
            self.open.push(session);
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

    /// Stops `tag`'s timer and hands back the finished session.
    pub fn stop(&mut self, tag: TagId, now: Timestamp) -> Result<Session, TimerError> {
        let index = self.position(tag).ok_or(TimerError::NoTimer)?;
        let mut session = self.open.swap_remove(index);
        session.stop(now)?;
        Ok(session)
    }

    fn apply(
        &mut self,
        tag: TagId,
        action: impl FnOnce(&mut Session) -> Result<(), SessionError>,
    ) -> Result<Session, TimerError> {
        let index = self.position(tag).ok_or(TimerError::NoTimer)?;
        let session = &mut self.open[index];
        action(session)?;
        Ok(session.clone())
    }

    fn position(&self, tag: TagId) -> Option<usize> {
        self.open.iter().position(|session| session.tag == tag)
    }

    fn pause_where(&mut self, now: Timestamp, filter: impl Fn(&Session) -> bool) -> Vec<Session> {
        self.open
            .iter_mut()
            .filter(|session| filter(session))
            .filter_map(|session| session.pause(now).is_ok().then(|| session.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, tz::TimeZone};

    use super::*;
    use crate::Activity;

    fn at(minute: i64) -> Zoned {
        (Timestamp::UNIX_EPOCH + SignedDuration::from_mins(minute)).to_zoned(TimeZone::UTC)
    }

    fn running(timers: &Timers) -> Vec<TagId> {
        timers
            .iter()
            .filter(|s| s.status() == Status::Running(Activity::Work))
            .map(|s| s.tag)
            .collect()
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
        assert_eq!(running(&timers), [b, c]);
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
        assert_eq!(
            finished.logged(at(99).timestamp()),
            SignedDuration::from_mins(30)
        );
        assert!(timers.get(tag).is_none());
        assert_eq!(
            timers.stop(tag, at(40).timestamp()),
            Err(TimerError::NoTimer)
        );
    }
}
