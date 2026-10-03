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
        let _ = self.app.update(Message::Tick);
    }

    fn send(&mut self, message: Message) {
        let _ = self.app.update(message);
        assert_eq!(self.app.error, None);
    }

    /// The UI at the window's real size.
    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(
            iced::Settings::default(),
            (460.0, 720.0),
            self.app.view(iced::window::Id::unique()),
        )
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
    let _ = h
        .app
        .update(Message::History(history::Message::Elapsed("soon".into())));
    let _ = h.app.update(Message::History(history::Message::Save));
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
        let _ = h
            .app
            .update(Message::History(history::Message::Elapsed(elapsed.into())));
        let _ = h.app.update(Message::History(history::Message::Save));
        assert!(h.app.error.is_some(), "{elapsed} is refused");
    }
    let _ = h
        .app
        .update(Message::History(history::Message::Elapsed("1:".into())));
    assert!(h.app.error.is_some(), "typing keeps the error");
    h.send(Message::DismissError);
    assert!(h.shows("Today 0m"));
}

#[test]
fn pause_all_uses_the_same_save_and_reload_path() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.create_tag("Globex");
    let tags: Vec<_> = h.app.data.tags.iter().map(|tag| tag.id).collect();
    for tag in tags {
        h.send(Message::Timers(timers::Message::Start(
            tag,
            tinytrac_core::StartMode::Alongside,
        )));
    }
    h.advance(15);
    h.send(Message::Timers(timers::Message::PauseAll));
    assert!(!h.app.data.any_running());
    assert!(h.shows("Today 30m"));
}

#[test]
fn reopening_a_window_keeps_the_engine_and_forms() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    h.click("Start");
    h.click("Add time");
    h.send(Message::History(history::Message::Elapsed("12m".into())));
    let _ = h.app.update(Message::ShowWindow);
    let first = h.app.window.unwrap();
    let _ = h.app.update(Message::ShowWindow);
    assert_eq!(h.app.window, Some(first), "Show reuses an existing window");
    // The close task is deliberately not executed by this headless harness.
    let _ = h.app.update(Message::CloseWindow(first));
    assert!(h.app.window.is_none());
    let _ = h.app.update(Message::ShowWindow);
    assert_ne!(h.app.window, Some(first));
    assert!(h.app.data.any_running());
    h.click("Save");
    assert!(h.shows("Today 12m"), "the edit buffer survived");
}

#[test]
fn losing_the_tray_host_restores_a_closed_window() {
    let mut h = Harness::new();
    assert!(h.app.window.is_none());
    let _ = h.app.update(Message::TrayHost(false));
    assert!(h.app.window.is_some());
}

#[test]
fn tray_start_and_pause_all_use_screen_commands() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    let tag = h.app.data.tags[0].id;
    h.app.tray = Some(crate::tray::Tray::headless(&h.app.data));
    h.send(Message::TrayMenu(format!("start/{tag}")));
    assert!(h.app.data.any_running());
    assert!(h.shows("Stop"));
    h.advance(15);
    h.send(Message::TrayMenu("pause-all".into()));
    assert!(!h.app.data.any_running());
    assert!(h.shows("Today 15m"));
    assert!(h.shows("Resume"));
}

#[test]
fn tray_quit_exits_without_stopping_timers() {
    use iced::futures::{StreamExt, executor::block_on};
    use iced_test::runtime::{Action, task};

    let mut h = Harness::new();
    h.create_tag("Acme");
    h.click("Start");
    h.app.tray = Some(crate::tray::Tray::headless(&h.app.data));
    let task = h.app.update(Message::TrayMenu("quit".into()));
    let mut actions = task::into_stream(task).unwrap();
    assert!(matches!(block_on(actions.next()), Some(Action::Exit)));
    assert!(h.app.data.any_running());
}

#[test]
fn closing_a_foreign_window_is_ignored() {
    let mut h = Harness::new();
    h.send(Message::ShowWindow);
    let window = h.app.window;
    h.send(Message::ModifiersChanged(Modifiers::COMMAND));
    let task = h
        .app
        .update(Message::CloseWindow(iced::window::Id::unique()));
    assert!(iced_test::runtime::task::into_stream(task).is_none());
    assert_eq!(h.app.window, window);
    assert_eq!(h.app.modifiers, Modifiers::COMMAND);
}

#[test]
fn tray_ids_without_a_tray_are_ignored() {
    let mut h = Harness::new();
    h.create_tag("Acme");
    let tag = h.app.data.tags[0].id;
    for id in [format!("start/{tag}"), "quit".into(), "show".into()] {
        let task = h.app.update(Message::TrayMenu(id));
        assert!(iced_test::runtime::task::into_stream(task).is_none());
    }
    assert!(!h.app.data.any_running());
    assert!(h.app.window.is_none());
}
