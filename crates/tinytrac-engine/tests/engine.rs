//! The engine's commands and queries, driven through its public API with a
//! controllable clock.

use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Zoned, civil::date};
use tinytrac_core::{Activity, DateRange, StartMode, Status, TagId};
use tinytrac_engine::{Engine, Error, Settings};

const MIN: SignedDuration = SignedDuration::from_mins(1);

/// A clock the test moves forward by hand.
#[derive(Clone)]
struct Clock(Arc<Mutex<Zoned>>);

impl Clock {
    fn at(time: &str) -> Self {
        Self(Arc::new(Mutex::new(
            format!("{time}[Europe/Warsaw]").parse().unwrap(),
        )))
    }

    fn advance(&self, minutes: i32) {
        let mut now = self.0.lock().unwrap();
        *now = now.checked_add(MIN * minutes).unwrap();
    }

    fn attach(&self, engine: &mut Engine) {
        let clock = self.clone();
        engine.set_clock(move || clock.0.lock().unwrap().clone());
    }
}

fn engine_at(time: &str) -> (Engine, Clock) {
    let mut engine = Engine::open_in_memory().unwrap();
    let clock = Clock::at(time);
    clock.attach(&mut engine);
    (engine, clock)
}

fn today() -> DateRange {
    DateRange::day(date(2026, 10, 1))
}

#[test]
fn tags_are_sorted_and_editable() {
    let (mut engine, _) = engine_at("2026-10-01T09:00");
    let beta = engine.create_tag("beta").unwrap();
    engine.create_tag("Alpha").unwrap();

    let names = |engine: &Engine| -> Vec<String> {
        engine
            .tags()
            .unwrap()
            .iter()
            .map(|t| t.name().to_owned())
            .collect()
    };
    assert_eq!(names(&engine), ["Alpha", "beta"]);

    engine.rename_tag(beta.id, "Gamma").unwrap();
    assert!(engine.set_tag_archived(beta.id, true).unwrap().archived);
    assert_eq!(names(&engine), ["Alpha", "Gamma"]);
    assert!(matches!(
        engine.rename_tag(beta.id, " "),
        Err(Error::Tag(_))
    ));
}

#[test]
fn timers_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tinytrac.db");
    let clock = Clock::at("2026-10-01T09:00");

    let mut engine = Engine::open(&path).unwrap();
    clock.attach(&mut engine);
    let tag = engine.create_tag("Acme").unwrap().id;
    engine.start(tag, StartMode::Exclusive).unwrap();
    drop(engine);

    clock.advance(30);
    let mut engine = Engine::open(&path).unwrap();
    clock.attach(&mut engine);
    let timers = engine.timers().unwrap();
    assert_eq!(
        timers.get(tag).unwrap().status(),
        Status::Running(Activity::Work)
    );
    assert_eq!(engine.totals(today()).unwrap().total(), MIN * 30);
}

#[test]
fn starting_a_timer_pauses_the_running_one() {
    let (mut engine, clock) = engine_at("2026-10-01T09:00");
    let a = engine.create_tag("A").unwrap().id;
    let b = engine.create_tag("B").unwrap().id;

    engine.start(a, StartMode::Exclusive).unwrap();
    clock.advance(10);
    engine.start(b, StartMode::Exclusive).unwrap();
    clock.advance(5);
    engine.start(a, StartMode::Alongside).unwrap();

    let timers = engine.timers().unwrap();
    let running = |tag| timers.get(tag).unwrap().status() == Status::Running(Activity::Work);
    assert!(running(a) && running(b));
    assert_eq!(engine.totals(today()).unwrap().by_tag()[&a], MIN * 10);
}

#[test]
fn the_break_bank_follows_settings() {
    let (mut engine, clock) = engine_at("2026-10-01T09:00");
    let tag = engine.create_tag("Acme").unwrap().id;
    let settings = Settings {
        break_bank: true,
        ..Settings::default()
    };
    engine.save_settings(&settings).unwrap();
    assert_eq!(engine.settings().unwrap(), settings);

    engine.start(tag, StartMode::Exclusive).unwrap();
    clock.advance(25);
    engine.start_break(tag).unwrap();
    clock.advance(6);
    engine.stop(tag).unwrap();

    assert_eq!(engine.totals(today()).unwrap().total(), MIN * 30);
}

#[test]
fn breaks_are_refused_while_the_break_bank_is_off() {
    let (mut engine, _) = engine_at("2026-10-01T09:00");
    let tag = engine.create_tag("Acme").unwrap().id;
    engine.start(tag, StartMode::Exclusive).unwrap();
    assert!(matches!(engine.start_break(tag), Err(Error::Timer(_))));
}

#[test]
fn entries_can_be_added_edited_moved_and_deleted() {
    let (mut engine, _) = engine_at("2026-10-01T09:00");
    let a = engine.create_tag("A").unwrap().id;
    let b = engine.create_tag("B").unwrap().id;

    let entry = engine.add_entry(a, date(2026, 10, 1), MIN * 90).unwrap();
    engine.set_logged(entry.id, MIN * 60).unwrap();
    engine.retag(entry.id, b).unwrap();
    assert_eq!(engine.totals(today()).unwrap().by_tag()[&b], MIN * 60);

    engine.delete_session(entry.id).unwrap();
    assert_eq!(
        engine.totals(today()).unwrap().total(),
        SignedDuration::ZERO
    );
    assert!(matches!(
        engine.delete_session(entry.id),
        Err(Error::NotFound("session"))
    ));
}

#[test]
fn a_running_timer_cannot_move_to_a_tag_with_its_own_timer() {
    let (mut engine, _) = engine_at("2026-10-01T09:00");
    let a = engine.create_tag("A").unwrap().id;
    let b = engine.create_tag("B").unwrap().id;
    let session = engine.start(a, StartMode::Exclusive).unwrap()[0].id;
    engine.start(b, StartMode::Alongside).unwrap();

    assert!(matches!(engine.retag(session, b), Err(Error::TagBusy)));
}

#[test]
fn ranges_include_sessions_crossing_midnight_and_manual_entries() {
    let (mut engine, clock) = engine_at("2026-09-30T23:00");
    let tag = engine.create_tag("Acme").unwrap().id;
    engine.start(tag, StartMode::Exclusive).unwrap();
    clock.advance(120);
    engine.stop(tag).unwrap();
    engine.add_entry(tag, date(2026, 10, 1), MIN * 15).unwrap();
    engine.add_entry(tag, date(2026, 10, 2), MIN * 45).unwrap();

    assert_eq!(engine.sessions(today()).unwrap().len(), 2);
    assert_eq!(engine.totals(today()).unwrap().total(), MIN * 75);
    let month = DateRange::month_of(date(2026, 10, 1));
    assert_eq!(engine.totals(month).unwrap().total(), MIN * 120);
}

#[test]
fn unknown_records_are_reported() {
    let (mut engine, _) = engine_at("2026-10-01T09:00");
    let stranger = TagId::generate();
    assert!(matches!(
        engine.start(stranger, StartMode::Exclusive),
        Err(Error::NotFound("tag"))
    ));
    assert!(matches!(
        engine.add_entry(stranger, date(2026, 10, 1), MIN),
        Err(Error::NotFound("tag"))
    ));
}

#[test]
fn a_running_timer_can_be_edited_and_deleted() {
    let (mut engine, clock) = engine_at("2026-10-01T09:00");
    let tag = engine.create_tag("Acme").unwrap().id;
    let session = engine.start(tag, StartMode::Exclusive).unwrap()[0].id;
    clock.advance(60);
    engine.set_logged(session, MIN * 30).unwrap();
    clock.advance(10);
    assert_eq!(engine.totals(today()).unwrap().total(), MIN * 40);

    engine.delete_session(session).unwrap();
    assert!(engine.timers().unwrap().get(tag).is_none());
    assert_eq!(
        engine.totals(today()).unwrap().total(),
        SignedDuration::ZERO
    );
}

#[test]
fn pausing_and_breaks_round_trip_through_storage() {
    let (mut engine, clock) = engine_at("2026-10-01T09:00");
    let settings = Settings {
        break_bank: true,
        ..Settings::default()
    };
    engine.save_settings(&settings).unwrap();
    let tag = engine.create_tag("Acme").unwrap().id;

    engine.start(tag, StartMode::Exclusive).unwrap();
    clock.advance(50);
    engine.start_break(tag).unwrap();
    clock.advance(5);
    engine.end_break(tag).unwrap();
    clock.advance(5);
    engine.pause(tag).unwrap();
    clock.advance(30);

    let timers = engine.timers().unwrap();
    assert_eq!(timers.get(tag).unwrap().status(), Status::Paused);
    assert_eq!(engine.totals(today()).unwrap().total(), MIN * 60);
}
