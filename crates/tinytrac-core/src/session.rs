use jiff::{SignedDuration, Timestamp, Zoned, civil::Date};
use serde::{Deserialize, Serialize};

use crate::{BreakPolicy, Interval, SessionId, TagId};

/// What a running timer is counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Work,
    Break,
}

/// A stretch of uninterrupted work or break. `end` is `None` while it is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub activity: Activity,
    pub start: Timestamp,
    pub end: Option<Timestamp>,
}

impl Segment {
    /// The segment's span, treating an open segment as ending at `now`.
    pub fn interval(&self, now: Timestamp) -> Interval {
        Interval {
            start: self.start,
            end: self.end.unwrap_or(now).max(self.start),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running(Activity),
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("the timer is not running")]
    NotRunning,
    #[error("the timer is not paused")]
    NotPaused,
    #[error("the timer has been stopped")]
    Stopped,
    #[error("breaks are disabled for this timer")]
    BreaksDisabled,
    #[error("the timer is already on a break")]
    AlreadyOnBreak,
    #[error("the timer is not on a break")]
    NotOnBreak,
    #[error("logged time cannot be negative")]
    NegativeDuration,
    #[error("a correction can be at most a year")]
    TooLong,
    #[error("invalid session data: {0}")]
    Invalid(&'static str),
}

/// One timer's run for a tag, from start until stop.
///
/// A session records what actually happened as work and break segments; pauses
/// are the gaps between them. Logged time is always derived from the segments,
/// never stored, so the break-bank deduction stays recomputable and explainable.
/// Manual corrections are kept separately as an `adjustment`.
///
/// Break bank: work earns break credit at the session's [`BreakPolicy`]. Break
/// time counts as logged only while covered by credit; uncovered break time is
/// an overdraft that later work can still pay back. Credit lives and dies with
/// the session: it survives pauses and is discarded on stop.
///
/// Deserializing checks the same invariants the methods maintain, so data from
/// disk or another device can't produce a session the rules don't allow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawSession")]
pub struct Session {
    pub id: SessionId,
    pub tag: TagId,
    /// The day the session belongs to: the day it started, or the chosen day of
    /// a manual entry. Positive adjustments count towards this day.
    pub day: Date,
    /// Captured at start so later settings changes don't rewrite history.
    breaks: Option<BreakPolicy>,
    segments: Vec<Segment>,
    adjustment: SignedDuration,
    stopped: bool,
}

impl Session {
    /// Starts a new session, working, at `now`.
    pub fn start(tag: TagId, breaks: Option<BreakPolicy>, now: &Zoned) -> Self {
        Self {
            id: SessionId::generate(),
            tag,
            day: now.date(),
            breaks,
            segments: vec![Segment {
                activity: Activity::Work,
                start: now.timestamp(),
                end: None,
            }],
            adjustment: SignedDuration::ZERO,
            stopped: false,
        }
    }

    /// A finished entry added by hand: just a duration on a day.
    pub fn manual(tag: TagId, day: Date, logged: SignedDuration) -> Result<Self, SessionError> {
        if logged.is_negative() {
            return Err(SessionError::NegativeDuration);
        }
        Ok(Self {
            id: SessionId::generate(),
            tag,
            day,
            breaks: None,
            segments: Vec::new(),
            adjustment: checked_adjustment(logged)?,
            stopped: true,
        })
    }

    pub fn status(&self) -> Status {
        if self.stopped {
            return Status::Stopped;
        }
        match self.segments.last() {
            Some(segment) if segment.end.is_none() => Status::Running(segment.activity),
            _ => Status::Paused,
        }
    }

    pub fn breaks(&self) -> Option<BreakPolicy> {
        self.breaks
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Manual correction on top of the recorded time.
    pub fn adjustment(&self) -> SignedDuration {
        self.adjustment
    }

    pub fn pause(&mut self, now: Timestamp) -> Result<(), SessionError> {
        self.running()?;
        self.close(now);
        Ok(())
    }

    pub fn resume(&mut self, now: Timestamp) -> Result<(), SessionError> {
        match self.status() {
            Status::Paused => {
                self.open(Activity::Work, now);
                Ok(())
            }
            Status::Running(_) => Err(SessionError::NotPaused),
            Status::Stopped => Err(SessionError::Stopped),
        }
    }

    pub fn start_break(&mut self, now: Timestamp) -> Result<(), SessionError> {
        if self.breaks.is_none() {
            return Err(SessionError::BreaksDisabled);
        }
        match self.running()? {
            Activity::Work => self.switch(Activity::Break, now),
            Activity::Break => return Err(SessionError::AlreadyOnBreak),
        }
        Ok(())
    }

    pub fn end_break(&mut self, now: Timestamp) -> Result<(), SessionError> {
        match self.running()? {
            Activity::Break => self.switch(Activity::Work, now),
            Activity::Work => return Err(SessionError::NotOnBreak),
        }
        Ok(())
    }

    pub fn stop(&mut self, now: Timestamp) -> Result<(), SessionError> {
        if self.stopped {
            return Err(SessionError::Stopped);
        }
        self.close(now);
        self.stopped = true;
        Ok(())
    }

    /// Recorded work and break time as of `now`, before any adjustment.
    pub fn tally(&self, now: Timestamp) -> Tally {
        let (mut work, mut break_taken) = (SignedDuration::ZERO, SignedDuration::ZERO);
        for segment in &self.segments {
            let duration = segment.interval(now).duration();
            match segment.activity {
                Activity::Work => work += duration,
                Activity::Break => break_taken += duration,
            }
        }
        let break_earned = self
            .breaks
            .map_or(SignedDuration::ZERO, |policy| policy.earned(work));
        Tally {
            work,
            break_taken,
            break_earned,
        }
    }

    /// Time this session logs as of `now`, including the adjustment.
    pub fn logged(&self, now: Timestamp) -> SignedDuration {
        self.tally(now).logged() + self.adjustment
    }

    /// Corrects the session so it logs exactly `target` as of `now`. A running
    /// session keeps counting from there, including any break overdraft that
    /// later work pays back.
    pub fn set_logged(
        &mut self,
        target: SignedDuration,
        now: Timestamp,
    ) -> Result<(), SessionError> {
        if target.is_negative() {
            return Err(SessionError::NegativeDuration);
        }
        let adjustment = checked_adjustment(target - self.tally(now).logged())?;
        // Close the running segment at `now` and reopen it, so a clock that later
        // jumps backwards can't shrink the time this adjustment was measured against.
        if let Status::Running(activity) = self.status() {
            self.switch(activity, now);
        }
        self.adjustment = adjustment;
        Ok(())
    }

    /// The spans of time this session logs as of `now`: all work, plus break
    /// time up to the earned credit (applied to breaks in order), with a negative
    /// adjustment trimmed off the end. A positive adjustment has no position in
    /// time, so it is not included.
    pub fn counted(&self, now: Timestamp) -> Vec<Interval> {
        let mut allowance = self.tally(now).break_covered();
        let mut spans: Vec<Interval> = self
            .segments
            .iter()
            .map(|segment| {
                let interval = segment.interval(now);
                match segment.activity {
                    Activity::Work => interval,
                    Activity::Break => {
                        let covered = interval.duration().min(allowance);
                        allowance -= covered;
                        Interval {
                            start: interval.start,
                            end: interval.start + covered,
                        }
                    }
                }
            })
            .filter(|span| span.duration().is_positive())
            .collect();

        let mut trim = (-self.adjustment).max(SignedDuration::ZERO);
        while trim.is_positive()
            && let Some(last) = spans.last_mut()
        {
            let duration = last.duration();
            if duration > trim {
                last.end -= trim;
                break;
            }
            trim -= duration;
            spans.pop();
        }
        spans
    }

    fn running(&self) -> Result<Activity, SessionError> {
        match self.status() {
            Status::Running(activity) => Ok(activity),
            Status::Paused => Err(SessionError::NotRunning),
            Status::Stopped => Err(SessionError::Stopped),
        }
    }

    fn switch(&mut self, activity: Activity, now: Timestamp) {
        self.close(now);
        self.open(activity, now);
    }

    /// Ends the open segment, if any. Never ends before it started, so a clock
    /// that jumps backwards can't produce negative time.
    fn close(&mut self, now: Timestamp) {
        if let Some(segment) = self.segments.last_mut().filter(|s| s.end.is_none()) {
            segment.end = Some(now.max(segment.start));
        }
    }

    /// Opens a segment at `now`, but never before the previous one ended.
    fn open(&mut self, activity: Activity, now: Timestamp) {
        let start = self
            .segments
            .last()
            .and_then(|segment| segment.end)
            .map_or(now, |end| now.max(end));
        self.segments.push(Segment {
            activity,
            start,
            end: None,
        });
    }
}

/// The largest correction a session accepts. A year is far more than any real
/// edit, and keeps totals nowhere near overflow, even with corrupt data.
const MAX_ADJUSTMENT: SignedDuration = SignedDuration::from_hours(24 * 366);

fn checked_adjustment(adjustment: SignedDuration) -> Result<SignedDuration, SessionError> {
    if adjustment.abs() > MAX_ADJUSTMENT {
        return Err(SessionError::TooLong);
    }
    Ok(adjustment)
}

/// A session as stored, before its invariants are checked.
#[derive(Deserialize)]
struct RawSession {
    id: SessionId,
    tag: TagId,
    day: Date,
    breaks: Option<BreakPolicy>,
    segments: Vec<Segment>,
    adjustment: SignedDuration,
    stopped: bool,
}

impl TryFrom<RawSession> for Session {
    type Error = SessionError;

    fn try_from(raw: RawSession) -> Result<Self, Self::Error> {
        checked_adjustment(raw.adjustment)?;
        let invalid = |reason| Err(SessionError::Invalid(reason));
        let mut previous_end = None;
        for (index, segment) in raw.segments.iter().enumerate() {
            let is_last = index + 1 == raw.segments.len();
            if segment.activity == Activity::Break && raw.breaks.is_none() {
                return invalid("break segment without a break policy");
            }
            if previous_end.is_some_and(|end| segment.start < end) {
                return invalid("segments overlap or are out of order");
            }
            match segment.end {
                None if !is_last || raw.stopped => {
                    return invalid("only the last segment of an open session can be running");
                }
                Some(end) if end < segment.start => {
                    return invalid("segment ends before it starts");
                }
                _ => {}
            }
            previous_end = segment.end;
        }
        let session = Self {
            id: raw.id,
            tag: raw.tag,
            day: raw.day,
            breaks: raw.breaks,
            segments: raw.segments,
            adjustment: raw.adjustment,
            stopped: raw.stopped,
        };
        // Logged time never decreases after the last recorded instant.
        let at = session
            .segments
            .last()
            .map_or(Timestamp::MIN, |s| s.end.unwrap_or(s.start));
        if session.adjustment.is_negative() && session.logged(at).is_negative() {
            return Err(SessionError::NegativeDuration);
        }
        Ok(session)
    }
}

/// A session's recorded time, split into work and break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub work: SignedDuration,
    pub break_taken: SignedDuration,
    pub break_earned: SignedDuration,
}

impl Tally {
    /// Break time paid for by earned credit.
    pub fn break_covered(&self) -> SignedDuration {
        self.break_taken.min(self.break_earned)
    }

    /// Credit left to spend; negative when overdrawn.
    pub fn break_balance(&self) -> SignedDuration {
        self.break_earned - self.break_taken
    }

    /// Work plus covered break.
    pub fn logged(&self) -> SignedDuration {
        self.work + self.break_covered()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: SignedDuration = SignedDuration::from_mins(1);

    fn at(minute: i64) -> Timestamp {
        Timestamp::UNIX_EPOCH + MIN * i32::try_from(minute).unwrap()
    }

    fn started(breaks: Option<BreakPolicy>) -> Session {
        let zoned = at(0).to_zoned(jiff::tz::TimeZone::UTC);
        Session::start(TagId::generate(), breaks, &zoned)
    }

    /// The scenario from the original feature request.
    #[test]
    fn uncovered_break_is_deducted_until_later_work_pays_it_back() {
        let mut session = started(Some(BreakPolicy::default()));
        session.start_break(at(25)).unwrap();
        session.end_break(at(31)).unwrap();

        let tally = session.tally(at(31));
        assert_eq!(tally.break_balance(), -MIN);
        assert_eq!(
            session.logged(at(31)),
            MIN * 30,
            "stopping now logs 30, not 31"
        );
        assert_eq!(
            session.logged(at(36)),
            MIN * 36,
            "5 more minutes of work covers the overdraft"
        );
    }

    #[test]
    fn pauses_are_not_logged_and_keep_the_balance() {
        let mut session = started(Some(BreakPolicy::default()));
        session.pause(at(50)).unwrap();
        assert_eq!(session.tally(at(90)).break_balance(), MIN * 10);
        session.resume(at(90)).unwrap();
        session.start_break(at(90)).unwrap();
        session.stop(at(100)).unwrap();

        assert_eq!(session.status(), Status::Stopped);
        assert_eq!(session.logged(at(500)), MIN * 60);
    }

    #[test]
    fn breaks_require_a_policy() {
        let mut session = started(None);
        assert_eq!(
            session.start_break(at(5)),
            Err(SessionError::BreaksDisabled)
        );
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        let mut session = started(Some(BreakPolicy::default()));
        assert_eq!(session.resume(at(1)), Err(SessionError::NotPaused));
        assert_eq!(session.end_break(at(1)), Err(SessionError::NotOnBreak));
        session.pause(at(2)).unwrap();
        assert_eq!(session.pause(at(3)), Err(SessionError::NotRunning));
        assert_eq!(session.start_break(at(3)), Err(SessionError::NotRunning));
        session.stop(at(4)).unwrap();
        assert_eq!(session.resume(at(5)), Err(SessionError::Stopped));
        assert_eq!(session.stop(at(5)), Err(SessionError::Stopped));
    }

    #[test]
    fn set_logged_adjusts_and_a_running_timer_keeps_counting() {
        let mut session = started(None);
        session.set_logged(MIN * 30, at(10)).unwrap();
        assert_eq!(session.adjustment(), MIN * 20);
        assert_eq!(session.logged(at(15)), MIN * 35);
        assert_eq!(
            session.set_logged(-MIN, at(15)),
            Err(SessionError::NegativeDuration)
        );
    }

    #[test]
    fn counted_spans_include_only_covered_break() {
        let mut session = started(Some(BreakPolicy::default()));
        session.start_break(at(10)).unwrap(); // 2 min earned
        session.stop(at(15)).unwrap();

        assert_eq!(
            session.counted(at(15)),
            [
                Interval {
                    start: at(0),
                    end: at(10)
                },
                Interval {
                    start: at(10),
                    end: at(12)
                },
            ]
        );
    }

    #[test]
    fn a_clock_jumping_backwards_never_produces_negative_time() {
        let mut session = started(None);
        session.pause(at(-5)).unwrap();
        session.resume(at(-10)).unwrap();
        assert_eq!(session.logged(at(-20)), SignedDuration::ZERO);
    }

    #[test]
    fn manual_entries_are_finished_and_log_their_duration() {
        let day = jiff::civil::date(2026, 10, 1);
        let session = Session::manual(TagId::generate(), day, MIN * 90).unwrap();
        assert_eq!(session.status(), Status::Stopped);
        assert_eq!(session.logged(at(0)), MIN * 90);
    }

    #[test]
    fn an_edit_survives_the_clock_jumping_backwards() {
        let mut session = started(None);
        session.set_logged(SignedDuration::ZERO, at(60)).unwrap();
        session.stop(at(59)).unwrap();
        assert_eq!(session.logged(at(59)), SignedDuration::ZERO);
    }

    #[test]
    fn earned_break_covers_breaks_in_order() {
        let mut session = started(Some(BreakPolicy::default()));
        session.start_break(at(50)).unwrap(); // 10 min earned
        session.end_break(at(60)).unwrap();
        session.start_break(at(60)).unwrap();
        session.stop(at(65)).unwrap();

        assert_eq!(session.logged(at(65)), MIN * 60);
        assert_eq!(
            session.counted(at(65)).last(),
            Some(&Interval {
                start: at(50),
                end: at(60)
            })
        );
    }

    #[test]
    fn lowering_elapsed_time_trims_counted_time_from_the_end() {
        let mut session = started(None);
        session.pause(at(30)).unwrap();
        session.resume(at(40)).unwrap();
        session.stop(at(60)).unwrap();
        session.set_logged(MIN * 20, at(60)).unwrap();

        assert_eq!(
            session.counted(at(60)),
            [Interval {
                start: at(0),
                end: at(20)
            }]
        );
    }

    #[test]
    fn sessions_round_trip_through_serde() {
        let mut session = started(Some(BreakPolicy::default()));
        session.start_break(at(30)).unwrap();
        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session);
    }

    #[test]
    fn deserializing_rejects_sessions_the_rules_do_not_allow() {
        let mut session = started(Some(BreakPolicy::default()));
        session.start_break(at(30)).unwrap();
        session.end_break(at(35)).unwrap();
        let valid = serde_json::to_value(&session).unwrap();

        let corrupt = |edit: fn(&mut serde_json::Value)| {
            let mut json = valid.clone();
            edit(&mut json);
            serde_json::from_value::<Session>(json).is_err()
        };
        assert!(
            corrupt(|j| j["stopped"] = true.into()),
            "stopped but running"
        );
        assert!(
            corrupt(|j| j["segments"][0]["end"] = None::<()>.into()),
            "open in the middle"
        );
        assert!(
            corrupt(|j| j["breaks"] = None::<()>.into()),
            "break without policy"
        );
        assert!(
            corrupt(|j| j["segments"].as_array_mut().unwrap().reverse()),
            "out of order"
        );
    }

    #[test]
    fn corrections_beyond_a_year_are_refused() {
        let day = jiff::civil::date(2026, 10, 1);
        let year = SignedDuration::from_hours(24 * 366);
        assert!(Session::manual(TagId::generate(), day, year).is_ok());
        let too_long = year + MIN;
        assert_eq!(
            Session::manual(TagId::generate(), day, too_long),
            Err(SessionError::TooLong)
        );

        let mut session = started(None);
        assert_eq!(
            session.set_logged(too_long, at(0)),
            Err(SessionError::TooLong)
        );
        let mut json = serde_json::to_value(&session).unwrap();
        json["adjustment"] = serde_json::to_value(too_long).unwrap();
        assert!(serde_json::from_value::<Session>(json).is_err());
    }

    #[test]
    fn deserialization_rejects_negative_logged_time() {
        let tag = TagId::generate();
        let manual = Session::manual(tag, jiff::civil::date(2026, 10, 1), MIN).unwrap();
        let mut value = serde_json::to_value(manual).unwrap();
        value["adjustment"] = serde_json::to_value(-MIN).unwrap();
        assert!(serde_json::from_value::<Session>(value).is_err());

        let mut tracked = Session::start(tag, None, &at(0).to_zoned(jiff::tz::TimeZone::UTC));
        tracked.pause(at(10)).unwrap();
        let mut value = serde_json::to_value(&tracked).unwrap();
        value["adjustment"] = serde_json::to_value(-MIN * 11).unwrap();
        assert!(serde_json::from_value::<Session>(value).is_err());
        tracked.resume(at(20)).unwrap();
        tracked.set_logged(MIN * 5, at(25)).unwrap();
        assert_eq!(
            serde_json::from_str::<Session>(&serde_json::to_string(&tracked).unwrap()).unwrap(),
            tracked
        );
    }
}
