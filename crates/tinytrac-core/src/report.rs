use std::collections::BTreeMap;

use jiff::{
    SignedDuration, Timestamp, ToSpan,
    civil::{Date, Weekday},
    tz::TimeZone,
};

use crate::{Interval, Session, TagId};

/// An inclusive range of calendar days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    first: Date,
    last: Date,
}

impl DateRange {
    /// The days from `a` to `b`, in either order.
    pub fn new(a: Date, b: Date) -> Self {
        Self {
            first: a.min(b),
            last: a.max(b),
        }
    }

    pub fn day(date: Date) -> Self {
        Self::new(date, date)
    }

    /// The week containing `date`, starting on `week_start`.
    pub fn week_of(date: Date, week_start: Weekday) -> Self {
        let first = date - i64::from(date.weekday().since(week_start)).days();
        Self::new(first, first + 6.days())
    }

    pub fn month_of(date: Date) -> Self {
        Self::new(date.first_of_month(), date.last_of_month())
    }

    /// The month before the one containing `date`.
    pub fn month_before(date: Date) -> Self {
        Self::month_of(date.first_of_month() - 1.day())
    }

    pub fn first(&self) -> Date {
        self.first
    }

    pub fn last(&self) -> Date {
        self.last
    }

    pub fn contains(&self, date: Date) -> bool {
        (self.first..=self.last).contains(&date)
    }

    /// The range as a span of time in `tz`, from the first day's midnight to
    /// the midnight after the last day.
    pub fn bounds(self, tz: &TimeZone) -> Interval {
        Interval {
            start: midnight(self.first, tz),
            end: next_midnight(self.last, tz),
        }
    }
}

/// Logged time per day and tag over a [`DateRange`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    days: BTreeMap<Date, BTreeMap<TagId, SignedDuration>>,
}

impl Totals {
    /// Sums the logged time of `sessions` that falls within `range`.
    ///
    /// Pass every session whose segments overlap the range's
    /// [`bounds`](DateRange::bounds) or whose `day` is in the range; others are
    /// ignored anyway.
    ///
    /// Counted time is split at midnight in `tz`, so a session running past
    /// midnight counts towards both days. A positive adjustment has no position
    /// in time and counts towards the session's own day; a negative one is
    /// already trimmed from the counted time, so no day goes below zero.
    pub fn compute<'a>(
        sessions: impl IntoIterator<Item = &'a Session>,
        range: DateRange,
        tz: &TimeZone,
        now: Timestamp,
    ) -> Self {
        let bounds = range.bounds(tz);
        let mut totals = Self::default();
        for session in sessions {
            let counted = session.counted(now);
            for interval in counted.iter().filter_map(|i| i.intersect(&bounds)) {
                for (date, duration) in split_by_day(interval, tz) {
                    totals.add(date, session.tag, duration);
                }
            }
            if range.contains(session.day) && session.adjustment().is_positive() {
                totals.add(session.day, session.tag, session.adjustment());
            }
        }
        totals
    }

    /// Per-tag totals for each day that has any logged time, in date order.
    pub fn days(&self) -> impl Iterator<Item = (Date, &BTreeMap<TagId, SignedDuration>)> {
        self.days.iter().map(|(date, tags)| (*date, tags))
    }

    /// Totals per tag across the whole range.
    pub fn by_tag(&self) -> BTreeMap<TagId, SignedDuration> {
        let mut by_tag = BTreeMap::new();
        for (tag, duration) in self.days.values().flatten() {
            *by_tag.entry(*tag).or_default() += *duration;
        }
        by_tag
    }

    pub fn total(&self) -> SignedDuration {
        self.days.values().flat_map(BTreeMap::values).copied().sum()
    }

    fn add(&mut self, date: Date, tag: TagId, duration: SignedDuration) {
        *self.days.entry(date).or_default().entry(tag).or_default() += duration;
    }
}

/// Cuts `interval` at each midnight in `tz`, yielding each day's share.
///
/// Days are treated as contiguous spans from one midnight to the next. Where
/// clocks fall back across midnight, the repeated hour counts towards the
/// later day.
fn split_by_day(interval: Interval, tz: &TimeZone) -> impl Iterator<Item = (Date, SignedDuration)> {
    let mut cursor = interval.start;
    std::iter::from_fn(move || {
        if cursor >= interval.end {
            return None;
        }
        let date = cursor.to_zoned(tz.clone()).date();
        let boundary = next_midnight(date, tz);
        let next = if boundary > cursor {
            boundary.min(interval.end)
        } else {
            interval.end
        };
        let share = next.duration_since(cursor);
        cursor = next;
        Some((date, share))
    })
}

fn next_midnight(date: Date, tz: &TimeZone) -> Timestamp {
    date.checked_add(1.day())
        .map_or(Timestamp::MAX, |next| midnight(next, tz))
}

/// The first instant of `date` in `tz` (not always 00:00, around DST changes).
fn midnight(date: Date, tz: &TimeZone) -> Timestamp {
    date.to_zoned(tz.clone()).map_or_else(
        |_| {
            if date.year() < 0 {
                Timestamp::MIN
            } else {
                Timestamp::MAX
            }
        },
        |zoned| zoned.timestamp(),
    )
}

#[cfg(test)]
mod tests {
    use jiff::{Zoned, civil::date};

    use super::*;
    use crate::BreakPolicy;

    const MIN: SignedDuration = SignedDuration::from_mins(1);
    /// Every session in these tests is stopped, so the current time doesn't matter.
    const NOW: Timestamp = Timestamp::UNIX_EPOCH;

    fn warsaw() -> TimeZone {
        TimeZone::get("Europe/Warsaw").unwrap()
    }

    fn zoned(s: &str) -> Zoned {
        format!("{s}[Europe/Warsaw]").parse().unwrap()
    }

    #[test]
    fn ranges_cover_whole_weeks_and_months() {
        let today = date(2026, 10, 1); // a Thursday
        assert_eq!(
            DateRange::week_of(today, Weekday::Monday),
            DateRange::new(date(2026, 9, 28), date(2026, 10, 4))
        );
        assert_eq!(
            DateRange::month_of(today),
            DateRange::new(date(2026, 10, 1), date(2026, 10, 31))
        );
        assert_eq!(
            DateRange::month_before(date(2026, 3, 15)),
            DateRange::new(date(2026, 2, 1), date(2026, 2, 28))
        );
    }

    #[test]
    fn sessions_are_split_at_local_midnight() {
        let tag = TagId::generate();
        let mut session = Session::start(tag, None, &zoned("2026-10-01T23:00"));
        session.stop(zoned("2026-10-02T01:30").timestamp()).unwrap();

        let range = DateRange::new(date(2026, 10, 1), date(2026, 10, 2));
        let totals = Totals::compute([&session], range, &warsaw(), NOW);
        let days: Vec<_> = totals.days().map(|(day, tags)| (day, tags[&tag])).collect();
        assert_eq!(
            days,
            [(date(2026, 10, 1), MIN * 60), (date(2026, 10, 2), MIN * 90)]
        );
    }

    #[test]
    fn only_time_inside_the_range_counts() {
        let tag = TagId::generate();
        let mut session = Session::start(tag, None, &zoned("2026-10-01T23:00"));
        session.stop(zoned("2026-10-02T01:30").timestamp()).unwrap();

        let totals = Totals::compute(
            [&session],
            DateRange::day(date(2026, 10, 2)),
            &warsaw(),
            NOW,
        );
        assert_eq!(totals.total(), MIN * 90);
    }

    #[test]
    fn a_dst_day_has_25_hours() {
        let tag = TagId::generate();
        let mut session = Session::start(tag, None, &zoned("2026-10-25T00:00"));
        session.stop(zoned("2026-10-26T00:00").timestamp()).unwrap();

        let totals = Totals::compute(
            [&session],
            DateRange::day(date(2026, 10, 25)),
            &warsaw(),
            NOW,
        );
        assert_eq!(totals.total(), SignedDuration::from_hours(25));
    }

    #[test]
    fn adjustments_and_manual_entries_count_on_their_day() {
        let (a, b) = (TagId::generate(), TagId::generate());
        let mut tracked =
            Session::start(a, Some(BreakPolicy::default()), &zoned("2026-10-01T09:00"));
        let stop = zoned("2026-10-01T10:00").timestamp();
        tracked.stop(stop).unwrap();
        tracked.set_logged(MIN * 45, stop).unwrap();
        let manual = Session::manual(b, date(2026, 10, 1), MIN * 30).unwrap();

        let totals = Totals::compute(
            [&tracked, &manual],
            DateRange::month_of(date(2026, 10, 1)),
            &warsaw(),
            NOW,
        );
        assert_eq!(
            totals.by_tag(),
            BTreeMap::from([(a, MIN * 45), (b, MIN * 30)])
        );
        assert_eq!(totals.total(), MIN * 75);
    }

    #[test]
    fn shortening_a_session_across_midnight_keeps_both_days_positive() {
        let tag = TagId::generate();
        let mut session = Session::start(tag, None, &zoned("2026-10-01T23:00"));
        let stop = zoned("2026-10-02T01:00").timestamp();
        session.stop(stop).unwrap();
        session.set_logged(MIN * 30, stop).unwrap();

        let range = DateRange::new(date(2026, 10, 1), date(2026, 10, 2));
        let totals = Totals::compute([&session], range, &warsaw(), NOW);
        let days: Vec<_> = totals.days().map(|(day, tags)| (day, tags[&tag])).collect();
        assert_eq!(days, [(date(2026, 10, 1), MIN * 30)]);
    }

    #[test]
    fn civil_extremes_are_clamped_in_every_offset() {
        for tz in [
            TimeZone::UTC,
            TimeZone::fixed(jiff::tz::offset(14)),
            TimeZone::fixed(jiff::tz::offset(-12)),
        ] {
            for date in [Date::MIN, Date::MAX] {
                let range = DateRange::day(date);
                let bounds = range.bounds(&tz);
                assert!(bounds.start <= bounds.end);
                let manual = Session::manual(TagId::generate(), date, MIN).unwrap();
                assert_eq!(Totals::compute([&manual], range, &tz, NOW).total(), MIN);
            }
            for (start, end) in [
                (Timestamp::MIN, Timestamp::MIN + MIN * 60),
                (Timestamp::MAX - MIN * 60, Timestamp::MAX),
            ] {
                let mut session =
                    Session::start(TagId::generate(), None, &start.to_zoned(tz.clone()));
                session.stop(end).unwrap();
                let range = DateRange::new(
                    start.to_zoned(tz.clone()).date(),
                    end.to_zoned(tz.clone()).date(),
                );
                assert_eq!(
                    Totals::compute([&session], range, &tz, end).total(),
                    MIN * 60
                );
            }
        }
    }
}
