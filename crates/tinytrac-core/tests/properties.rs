//! Invariants of the break bank and reporting, checked over random timer usage.

use jiff::ToSpan;
use proptest::prelude::*;
use tinytrac_core::{
    BreakPolicy, DateRange, Interval, Session, SignedDuration, TagId, TimeZone, Timestamp, Totals,
};

#[derive(Debug, Clone, Copy)]
enum Action {
    Pause,
    Resume,
    StartBreak,
    EndBreak,
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        Just(Action::Pause),
        Just(Action::Resume),
        Just(Action::StartBreak),
        Just(Action::EndBreak),
    ]
}

/// A session driven by random actions, each after a random gap in minutes.
/// Invalid actions are ignored, as the UI would never offer them.
fn session() -> impl Strategy<Value = (Session, Vec<Timestamp>)> {
    prop::collection::vec((action(), 0..120_i64), 0..30).prop_map(|steps| {
        let start = "2026-10-01T08:00:00+02:00".parse::<Timestamp>().unwrap();
        let mut session = Session::start(
            TagId::generate(),
            Some(BreakPolicy::default()),
            &start.to_zoned(TimeZone::UTC),
        );
        let mut now = start;
        let mut checkpoints = vec![now];
        for (action, gap) in steps {
            now += SignedDuration::from_mins(gap);
            let _ = match action {
                Action::Pause => session.pause(now),
                Action::Resume => session.resume(now),
                Action::StartBreak => session.start_break(now),
                Action::EndBreak => session.end_break(now),
            };
            checkpoints.push(now);
        }
        (session, checkpoints)
    })
}

fn total(intervals: impl Iterator<Item = Interval>) -> SignedDuration {
    intervals.map(|i| i.duration()).sum()
}

proptest! {
    #[test]
    fn logged_time_never_decreases((session, checkpoints) in session()) {
        for pair in checkpoints.windows(2) {
            prop_assert!(session.logged(pair[0]) <= session.logged(pair[1]));
        }
    }

    #[test]
    fn logged_time_is_work_plus_covered_break((session, checkpoints) in session()) {
        let now = *checkpoints.last().unwrap();
        let tally = session.tally(now);
        prop_assert!(tally.logged() <= tally.work + tally.break_taken);
        prop_assert!(tally.break_covered() <= tally.break_earned);
        prop_assert_eq!(total(session.counted(now)), session.logged(now));
    }

    #[test]
    fn report_totals_add_up_to_logged_time((session, checkpoints) in session()) {
        let now = *checkpoints.last().unwrap();
        let tz = TimeZone::get("America/New_York").unwrap();
        let range = DateRange::new(session.day - 1.day(), session.day + 30.days());
        let totals = Totals::compute([&session], range, &tz, now);
        prop_assert_eq!(totals.total(), session.logged(now));
    }
}
