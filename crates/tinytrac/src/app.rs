//! The top-level iced program: routes messages to screens and reloads the
//! shared data after every command.

mod google;
mod lifecycle;
mod sync;

use std::{path::Path, time::Duration};

use iced::{
    Alignment::Center,
    Element, Fill, Subscription, Task, keyboard, time,
    widget::{button, column, container, row, scrollable, text},
    window,
};
use tinytrac_core::DateRange;
use tinytrac_engine::{Engine, SyncRun};

use crate::{
    data::Data,
    duration,
    screen::{self, Outcome, history, reports, settings, timers},
    tray,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Track,
    Reports,
    Settings,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    CloseWindow(window::Id),
    ShowWindow,
    Quit,
    TrayMenu(String),
    TrayHost(bool),
    Refresh,
    #[cfg(test)]
    Synced(Result<SyncRun, sync::Failure>),
    WorkerSynced(u64, Result<SyncRun, sync::Failure>),
    BrowserFinished(u64, Result<(), sync::Failure>),
    SignedIn(u64, Result<google::SignedIn, sync::Failure>),
    ModifiersChanged(keyboard::Modifiers),
    Show(Screen),
    DismissError,
    Timers(timers::Message),
    History(history::Message),
    Reports(reports::Message),
    Settings(settings::Message),
}

#[derive(Debug)]
pub struct App {
    window: Option<window::Id>,
    tray: Option<tray::Tray>,
    engine: Engine,
    sync: sync::State,
    google: google::State,
    data: Data,
    screen: Screen,
    modifiers: keyboard::Modifiers,
    error: Option<String>,
    timers: timers::State,
    history: history::State,
    reports: reports::State,
    settings: settings::State,
}

impl App {
    pub fn new(engine: Engine) -> screen::Result<Self> {
        let data = Data::load(&engine)?;
        let mut reports = reports::State::new(&data);
        reports.reload(&engine, &data)?;
        let sync = sync::State::new(&engine, &data)?;
        Ok(Self {
            window: None,
            tray: None,
            settings: settings::State::new(&data),
            reports,
            engine,
            sync,
            google: google::State::default(),
            data,
            screen: Screen::Track,
            modifiers: keyboard::Modifiers::default(),
            error: None,
            timers: timers::State::default(),
            history: history::State::default(),
        })
    }

    /// Opens the database at `path`, exiting with a message if that fails.
    pub fn boot(path: &Path) -> (Self, Task<Message>) {
        let mut app = Engine::open(path)
            .map_err(screen::Error::from)
            .and_then(Self::new)
            .unwrap_or_else(|error| {
                eprintln!("tinytrac: cannot open {}: {error}", path.display());
                std::process::exit(1);
            });
        let task = Task::batch([app.open_window(), app.sync.request()]);
        (app, task)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let outcome = match message {
            Message::Timers(message) => self.timers.update(message, &mut self.engine),
            Message::History(message) => self.history.update(message, &mut self.engine, &self.data),
            Message::Reports(message) => self.reports.update(message, &self.engine, &self.data),
            Message::Settings(
                message @ (settings::Message::SignIn
                | settings::Message::CancelSignIn
                | settings::Message::SignOut
                | settings::Message::SyncMode(_)),
            ) => return self.google_action(&message),
            Message::Settings(message) => {
                if matches!(message, settings::Message::SaveSyncFolder) {
                    self.google.cancel();
                }
                self.settings.update(message, &mut self.engine)
            }
            message => return self.lifecycle(message),
        };
        match outcome {
            Ok(Outcome::Edited) => {}
            Ok(Outcome::Saved) => {
                self.error = None;
                self.refresh();
                return self.sync.request();
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.refresh();
            }
        }
        Task::none()
    }

    /// Moves the clock on; a new day also needs fresh data.
    fn tick(&mut self) {
        let now = self.engine.now();
        let new_day = now.date() != self.data.today();
        self.data.now = now;
        if new_day {
            self.refresh();
        }
    }

    /// Reloads the data, e.g. after a command or to pick up changes made elsewhere.
    fn refresh(&mut self) {
        let reloaded = Data::load(&self.engine).and_then(|data| {
            if self.data.sync_target != data.sync_target {
                self.google.cancel();
                let same_identity = self
                    .data
                    .sync_target
                    .as_ref()
                    .zip(data.sync_target.as_ref())
                    .is_some_and(|(old, new)| old.same_identity(new));
                if !same_identity {
                    self.sync.invalidate();
                }
            }
            self.data = data;
            if let Some(tray) = &mut self.tray
                && let Err(error) = tray.refresh(&self.data)
            {
                eprintln!("tinytrac: cannot update tray: {error}");
            }
            self.reports.reload(&self.engine, &self.data)
        });
        if let Err(error) = reloaded {
            self.error = Some(error.to_string());
        }
    }

    fn show(&mut self, screen: Screen) {
        if screen == Screen::Settings && self.screen != Screen::Settings {
            let selected = self.settings.sync_mode;
            self.settings = settings::State::new(&self.data);
            if self.data.sync_target.is_none() {
                self.settings.sync_mode = selected;
            }
        }
        self.screen = screen;
    }

    pub fn view(&self, _window: window::Id) -> Element<'_, Message> {
        let today = self.data.totals(DateRange::day(self.data.today())).total();
        let tab = |label, screen| {
            let style = if self.screen == screen {
                button::primary
            } else {
                button::text
            };
            button(label).style(style).on_press(Message::Show(screen))
        };
        let header = row![
            tab("Track", Screen::Track),
            tab("Reports", Screen::Reports),
            tab("Settings", Screen::Settings),
            text(format!("Today {}", duration::short(today)))
                .width(Fill)
                .align_x(iced::Right),
        ]
        .spacing(4)
        .align_y(Center);

        let content: Element<'_, Message> = match self.screen {
            Screen::Track => column![
                self.timers
                    .view(&self.data, self.modifiers)
                    .map(Message::Timers),
                self.history.view(&self.data).map(Message::History),
            ]
            .spacing(28)
            .into(),
            Screen::Reports => self.reports.view(&self.data).map(Message::Reports),
            Screen::Settings => self
                .settings
                .view(
                    &self.data,
                    &self.sync.status,
                    self.sync.client.is_some(),
                    self.google.waiting(),
                    self.sync.auth_required,
                )
                .map(Message::Settings),
        };

        let mut page = column![header].spacing(12).padding(16);
        if let Some(error) = &self.error {
            page = page.push(
                container(
                    row![
                        text(error).width(Fill),
                        button("Dismiss")
                            .style(button::text)
                            .on_press(Message::DismissError),
                    ]
                    .align_y(Center),
                )
                .padding(8)
                .style(container::danger),
            );
        }
        page.push(scrollable(container(content).padding([0, 4])).height(Fill))
            .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let modifiers = keyboard::listen().filter_map(|event| match event {
            keyboard::Event::ModifiersChanged(modifiers) => {
                Some(Message::ModifiersChanged(modifiers))
            }
            _ => None,
        });
        // Tick every second while a timer runs; otherwise just often enough to
        // notice a new day. Refresh now and then to pick up changes made elsewhere.
        let period = if self.data.any_running() { 1 } else { 60 };
        let tick = time::every(Duration::from_secs(period)).map(|_| Message::Tick);
        let refresh = time::every(Duration::from_secs(30)).map(|_| Message::Refresh);
        Subscription::batch([
            modifiers,
            tick,
            refresh,
            window::close_requests().map(Message::CloseWindow),
            tray::subscription(),
        ])
    }
}

#[cfg(test)]
mod tests;
