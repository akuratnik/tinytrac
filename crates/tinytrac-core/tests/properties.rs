//! Invariants of the break bank and reporting, checked over random timer usage.

use jiff::ToSpan;
use proptest::prelude::*;
use tinytrac_core::{
    BreakPolicy, DateRange, Session, SignedDuration, TagId, TimeZone, Timestamp, Totals,
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

/// A session driven by random actions, each after a random gap in minutes, then
/// stopped. Invalid actions are ignored, as the UI would never offer them.
///
/// Alongside the session, returns the logged time observed just before and just
/// after every action, in order.
fn session() -> impl Strategy<Value = (Session, Vec<SignedDuration>)> {
    prop::collection::vec((action(), 0..120_i64), 0..30).prop_map(|steps| {
        let mut now = "2026-10-01T08:00:00+02:00".parse::<Timestamp>().unwrap();
        let mut session = Session::start(
            TagId::generate(),
            Some(BreakPolicy::default()),
            &now.to_zoned(TimeZone::UTC),
        );
        let mut observed = Vec::new();
        for (action, gap) in steps {
            now += SignedDuration::from_mins(gap);
            observed.push(session.logged(now));
            let _ = match action {
                Action::Pause => session.pause(now),
                Action::Resume => session.resume(now),
                Action::StartBreak => session.start_break(now),
                Action::EndBreak => session.end_break(now),
            };
            observed.push(session.logged(now));
        }
        session.stop(now).unwrap();
        observed.push(session.logged(now));
        (session, observed)
    })
}

fn last_change(session: &Session) -> Timestamp {
    session
        .segments()
        .last()
        .and_then(|segment| segment.end)
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

proptest! {
    #[test]
    fn logged_time_never_decreases((_, observed) in session()) {
        for pair in observed.windows(2) {
            prop_assert!(pair[0] <= pair[1], "{} then {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn logged_time_is_work_plus_covered_break((session, _) in session()) {
        let tally = session.tally(last_change(&session));
        prop_assert!(tally.logged() <= tally.work + tally.break_taken);
        prop_assert!(tally.break_covered() <= tally.break_earned);
    }

    #[test]
    fn reports_add_up_and_no_day_goes_negative(
        (mut session, _) in session(),
        edited_minutes in prop::option::of(0..600_i64),
    ) {
        let now = last_change(&session);
        if let Some(minutes) = edited_minutes {
            session.set_logged(SignedDuration::from_mins(minutes), now).unwrap();
        }
        let tz = TimeZone::get("America/New_York").unwrap();
        let range = DateRange::new(session.day - 1.day(), session.day + 30.days());
        let totals = Totals::compute([&session], range, &tz, now);

        prop_assert_eq!(totals.total(), session.logged(now));
        for (day, tags) in totals.days() {
            for duration in tags.values() {
                prop_assert!(!duration.is_negative(), "{day}: {duration}");
            }
        }
    }
}
