//! Headless UI tests: drive the real `view` and `update` with simulated clicks.
//!
//! Set `TINYTRAC_SNAPSHOTS=<dir>` to also save a PNG of each screen.

use std::sync::{Arc, Mutex};

use iced::{Theme, keyboard::Modifiers};
use iced_test::{Error, simulator};
use jiff::{SignedDuration, Zoned};
use tinytrac_engine::Engine;

use super::{App, Message, Screen};
use crate::screen::{history, settings, timers};

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

    /// Clicks the first widget showing `label`, then applies its messages.
    fn click(&mut self, label: &str) {
        let mut ui = simulator(self.app.view());
        ui.click(label)
            .unwrap_or_else(|_| panic!("no {label:?} on screen"));
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            self.send(message);
        }
    }

    fn shows(&self, text: &str) -> bool {
        simulator(self.app.view()).find(text).is_ok()
    }

    fn snapshot(&self, name: &str) -> Result<(), Error> {
        let Some(dir) = std::env::var_os("TINYTRAC_SNAPSHOTS") else {
            return Ok(());
        };
        let path = std::path::Path::new(&dir).join(name);
        simulator(self.app.view())
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
    h.send(Message::ModifiersChanged(Modifiers::CTRL));
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
    assert!(h.shows("bank 5m"));
    h.click("Break");
    h.advance(6);
    assert!(h.shows("bank -1m"));
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
