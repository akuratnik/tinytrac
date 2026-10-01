//! Headless UI tests: drive the real `view` and `update` with simulated clicks.
//!
//! Set `TINYTRAC_SNAPSHOTS=<dir>` to also save a PNG of each screen.

use std::sync::{Arc, Mutex};

use iced::{Theme, keyboard::Modifiers};
use iced_test::{Error, Simulator};
use jiff::{SignedDuration, Zoned};
use tinytrac_engine::Engine;

use super::{App, Message, Screen};
use crate::screen::{history, reports, settings, timers};

/// An app on an in-memory database, with a clock the test moves by hand.
struct Harness {
    app: App,
    clock: Arc<Mutex<Zoned>>,
}

impl Harness {
    fn new() -> Self {
        let clock = Arc::new(Mutex::new(
            "2026-10-01T09:00[Europe/Warsaw]".parse::<Zoned>().unwrap(),
        ));
        let mut engine = Engine::open_in_memory().unwrap();
        let shared = Arc::clone(&clock);
        engine.set_clock(move || shared.lock().unwrap().clone());
        Self {
            app: App::new(engine).unwrap(),
            clock,
        }
    }

    fn advance(&mut self, minutes: i32) {
        let mut now = self.clock.lock().unwrap();
        *now = now
            .checked_add(SignedDuration::from_mins(1) * minutes)
            .unwrap();
        drop(now);
        self.app.update(Message::Tick);
    }

    fn send(&mut self, message: Message) {
        self.app.update(message);
        assert_eq!(self.app.error, None);
    }

    /// The UI at the window's real size.
    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(iced::Settings::default(), (460.0, 720.0), self.app.view())
    }

    /// Clicks the first widget showing `label`, then applies its messages.
    fn click(&mut self, label: &str) {
        let mut ui = self.ui();
        ui.click(label)
            .unwrap_or_else(|_| panic!("no {label:?} on screen"));
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            self.send(message);
        }
    }

    fn shows(&self, text: &str) -> bool {
        self.ui().find(text).is_ok()
    }

    fn snapshot(&self, name: &str) -> Result<(), Error> {
        let Some(dir) = std::env::var_os("TINYTRAC_SNAPSHOTS") else {
            return Ok(());
        };
        let path = std::path::Path::new(&dir).join(name);
        self.ui()
            .snapshot(&Theme::Light)?
            .matches_image(path)
            .map(drop)
    }

    fn create_tag(&mut self, name: &str) {
        self.send(Message::Timers(timers::Message::NewTagChanged(name.into())));
        self.send(Message::Timers(timers::Message::CreateTag));
    }
}

#[test]
fn a_day_of_tracking() -> Result<(), Error> {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.create_tag("Globex");
    h.snapshot("1-tags")?;

    // Start Acme, then ctrl+click Globex to run both.
    h.click("Start");
    h.advance(30);
    h.send(Message::ModifiersChanged(Modifiers::COMMAND));
    h.click("Start");
    h.send(Message::ModifiersChanged(Modifiers::empty()));
    h.advance(15);
    assert!(h.shows("0:45:00") && h.shows("0:15:00"), "both timers run");
    h.snapshot("2-two-timers")?;

    // Stop both; the history lists the day.
    h.click("Stop");
    h.click("Stop");
    assert!(h.shows("Today 1h 00m"));
    h.snapshot("3-history")?;

    // Edit the first entry's elapsed time.
    h.click("Edit");
    h.send(Message::History(history::Message::Elapsed("1:30".into())));
    h.snapshot("3b-editing")?;
    h.click("Save");
    assert!(h.shows("Today 1h 45m"));

    h.send(Message::Show(Screen::Reports));
    assert!(h.shows("1h 30m") && h.shows("15m"));
    h.snapshot("4-reports")?;

    h.send(Message::Show(Screen::Settings));
    h.snapshot("5-settings")
}

#[test]
fn the_break_bank_shows_its_balance_and_deducts_overdraft() -> Result<(), Error> {
    let mut h = Harness::new();
    h.send(Message::Settings(settings::Message::BreakBank(true)));
    h.create_tag("Acme");

    h.click("Start");
    h.advance(25);
    assert!(h.shows("today 25m · bank 5m"));
    h.click("Break");
    h.advance(6);
    assert!(h.shows("today 30m · bank -1m"));
    h.snapshot("6-break-overdrawn")?;
    h.click("Stop");
    assert!(h.shows("Today 30m"), "the uncovered minute is not logged");
    Ok(())
}

#[test]
fn invalid_input_is_reported_not_saved() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.click("Add time");
    h.app
        .update(Message::History(history::Message::Elapsed("soon".into())));
    h.app.update(Message::History(history::Message::Save));
    assert!(h.app.error.is_some());
    assert!(h.shows("Today 0m"));
}

#[test]
fn saving_an_untouched_edit_keeps_a_running_timer_counting() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.click("Start");
    h.advance(45);
    h.click("Edit");
    h.advance(2);
    h.click("Save");
    assert!(h.shows("0:47:00"), "no time lost");
    assert!(h.shows(" · running") && !h.shows(" · edited"));
}

#[test]
fn an_archived_tag_keeps_its_open_timer_on_screen() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.click("Start");
    let tag = h.app.data.tags[0].id;
    h.send(Message::Settings(settings::Message::Archive(tag, true)));
    assert!(h.shows("Acme") && h.shows("Stop"));
    h.click("Stop");
    assert!(
        !h.shows("Start"),
        "an archived tag without a timer is hidden"
    );
}

#[test]
fn turning_the_break_bank_on_uses_the_typed_ratio() {
    let mut h = Harness::new();
    h.send(Message::Show(Screen::Settings));
    h.send(Message::Settings(settings::Message::BreakMinutes(
        "10".into(),
    )));
    h.send(Message::Settings(settings::Message::WorkMinutes(
        "50".into(),
    )));
    h.send(Message::Settings(settings::Message::BreakBank(true)));

    let policy = h.app.data.settings.break_policy;
    assert_eq!((policy.break_minutes, policy.work_minutes.get()), (10, 50));
}

#[test]
fn a_custom_report_starts_from_the_range_on_screen() {
    let mut h = Harness::new();
    h.send(Message::Show(Screen::Reports));
    h.send(Message::Reports(reports::Message::Preset(
        reports::Preset::Week,
    )));
    h.send(Message::Reports(reports::Message::Preset(
        reports::Preset::Custom,
    )));
    assert!(h.shows("2026-09-28") && h.shows("2026-10-04"));
}

#[test]
fn an_error_stays_until_something_is_saved_or_it_is_dismissed() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.click("Add time");
    for elapsed in ["99999999999999999", "soon"] {
        h.app
            .update(Message::History(history::Message::Elapsed(elapsed.into())));
        h.app.update(Message::History(history::Message::Save));
        assert!(h.app.error.is_some(), "{elapsed} is refused");
    }
    h.app
        .update(Message::History(history::Message::Elapsed("1:".into())));
    assert!(h.app.error.is_some(), "typing keeps the error");
    h.send(Message::DismissError);
    assert!(h.shows("Today 0m"));
}
