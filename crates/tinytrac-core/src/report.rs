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
    fn interval(self, tz: &TimeZone) -> Interval {
        Interval {
            start: midnight(self.first, tz),
            end: midnight(self.last + 1.day(), tz),
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
    /// Recorded time is split at midnight in `tz`, so a session running past
    /// midnight counts towards both days. Adjustments have no position in time
    /// and count towards the session's own day.
    pub fn compute<'a>(
        sessions: impl IntoIterator<Item = &'a Session>,
        range: DateRange,
        tz: &TimeZone,
        now: Timestamp,
    ) -> Self {
        let bounds = range.interval(tz);
        let mut totals = Self::default();
        for session in sessions {
            for interval in session.counted(now).filter_map(|i| i.intersect(&bounds)) {
                for (date, duration) in split_by_day(interval, tz) {
                    totals.add(date, session.tag, duration);
                }
            }
            if range.contains(session.day) && !session.adjustment().is_zero() {
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
fn split_by_day(interval: Interval, tz: &TimeZone) -> impl Iterator<Item = (Date, SignedDuration)> {
    let mut cursor = interval.start;
    std::iter::from_fn(move || {
        if cursor >= interval.end {
            return None;
        }
        let date = cursor.to_zoned(tz.clone()).date();
        let next = midnight(date + 1.day(), tz).min(interval.end);
        let share = next.duration_since(cursor);
        cursor = next;
        Some((date, share))
    })
}

/// The first instant of `date` in `tz` (not always 00:00, around DST changes).
fn midnight(date: Date, tz: &TimeZone) -> Timestamp {
    date.to_zoned(tz.clone())
        .expect("calendar dates in use are within jiff's supported range")
        .timestamp()
}

#[cfg(test)]
mod tests {
    use jiff::{Zoned, civil::date};

    use super::*;
    use crate::BreakPolicy;

    const MIN: SignedDuration = SignedDuration::from_mins(1);

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
        let totals = Totals::compute([&session], range, &warsaw(), Timestamp::now());
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
            Timestamp::now(),
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
            Timestamp::now(),
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
            Timestamp::now(),
        );
        assert_eq!(
            totals.by_tag(),
            BTreeMap::from([(a, MIN * 45), (b, MIN * 30)])
        );
        assert_eq!(totals.total(), MIN * 75);
    }
}
