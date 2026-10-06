//! Window lifetime and tray actions share the same app state.

use iced::{Task, keyboard, window};

use super::{App, Message};
use crate::tray;

impl App {
    pub(super) fn lifecycle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CloseWindow(id) => {
                if self.window != Some(id) {
                    return Task::none();
                }
                self.window = None;
                self.modifiers = keyboard::Modifiers::default();
                return if self.tray.is_some() {
                    window::close(id)
                } else {
                    iced::exit()
                };
            }
            Message::ShowWindow => return self.open_window(),
            Message::Quit => return iced::exit(),
            Message::TrayHost(available) => {
                self.set_tray_available(available);
                return if self.tray.is_none() && self.window.is_none() {
                    self.open_window()
                } else {
                    Task::none()
                };
            }
            Message::TrayMenu(id) => {
                if let Some(message) = self.tray.as_ref().and_then(|tray| tray.message(&id)) {
                    return self.update(message);
                }
            }
            Message::Tick => {
                self.tick();
            }
            Message::Refresh => return self.sync.request(),
            Message::BrowserFinished(generation, result) => {
                if let Err(error) = result {
                    return self.signed_in(generation, Err(error));
                }
            }
            Message::SignedIn(generation, result) => return self.signed_in(generation, result),
            Message::WorkerSynced(generation, result) => {
                self.refresh();
                if !self.sync.current(generation) {
                    return self.sync.request();
                }
                return self.finish_sync(&result);
            }
            #[cfg(test)]
            Message::Synced(result) => {
                self.refresh();
                return self.finish_sync(&result);
            }
            Message::Show(screen) => {
                self.show(screen);
            }
            Message::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
            }
            Message::DismissError => {
                self.error = None;
            }
            Message::Timers(_)
            | Message::History(_)
            | Message::Reports(_)
            | Message::Settings(_) => {
                unreachable!("screen messages are handled by update")
            }
        }
        Task::none()
    }

    fn finish_sync(
        &mut self,
        result: &Result<tinytrac_engine::SyncRun, super::sync::Failure>,
    ) -> Task<Message> {
        let (banner, task) = self.sync.finish(result, &self.data, &self.engine.now());
        if let Some(message) = banner {
            self.error = Some(message);
        }
        task
    }

    pub(super) fn open_window(&mut self) -> Task<Message> {
        if let Some(id) = self.window {
            return window::minimize(id, false).chain(window::gain_focus(id));
        }
        let (id, task) = window::open(window::Settings {
            size: (460.0, 720.0).into(),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        self.window = Some(id);
        task.discard()
    }

    fn set_tray_available(&mut self, available: bool) {
        if !available {
            self.tray = None;
        } else if self.tray.is_none() {
            match tray::Tray::new(&self.data) {
                Ok(tray) => self.tray = Some(tray),
                Err(error) => eprintln!("tinytrac: tray unavailable: {error}"),
            }
        }
    }
}
