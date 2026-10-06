//! Browser authorization and local account transitions.
use super::{
    App, Message,
    sync::{self, Failure},
};
use crate::screen::settings::{self, SyncMode};
use iced::Task;
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tinytrac_drive::{Account, Client, Endpoints, Secret};
use tinytrac_engine::SyncTarget;

pub(super) fn client() -> Option<Client> {
    let id = option_env!("TINYTRAC_GOOGLE_CLIENT_ID")?.trim();
    let secret = option_env!("TINYTRAC_GOOGLE_CLIENT_SECRET")?.trim();
    (!id.is_empty() && !secret.is_empty()).then(|| Client {
        id: id.into(),
        secret: secret.into(),
    })
}

#[derive(Debug, Clone)]
pub struct SignedIn {
    pub account: Account,
    pub token: Secret,
}

#[derive(Debug, Default)]
pub(super) struct State {
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
}
impl State {
    pub(super) fn waiting(&self) -> bool {
        self.cancel.is_some()
    }
    pub(super) fn cancel(&mut self) {
        self.generation += 1;
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl App {
    pub(super) fn google_action(&mut self, message: &settings::Message) -> Task<Message> {
        match message {
            settings::Message::SignIn => {
                let Some(client) = self.sync.client.clone() else {
                    return Task::none();
                };
                self.google.cancel();
                let generation = self.google.generation;
                let cancel = Arc::new(AtomicBool::new(false));
                self.google.cancel = Some(cancel.clone());
                return authorize(client, generation, cancel);
            }
            settings::Message::CancelSignIn => self.google.cancel(),
            settings::Message::SignOut => {
                self.google.cancel();
                let target = self.data.sync_target.clone();
                match self.engine.set_sync_target(None) {
                    Ok(()) => {
                        self.sync.invalidate();
                        self.refresh();
                        self.error = None;
                        if let Some(SyncTarget::Remote { id, .. }) = target {
                            self.error = self.sync.credentials.delete(&id).err();
                        }
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            settings::Message::SyncMode(mode) => {
                if *mode == SyncMode::Google && self.sync.client.is_none() {
                    return Task::none();
                }
                if *mode != self.settings.sync_mode {
                    self.google.cancel();
                    match self.engine.set_sync_target(None) {
                        Ok(()) => {
                            self.settings.sync_mode = *mode;
                            self.sync.invalidate();
                            self.refresh();
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            _ => unreachable!("only account actions are routed here"),
        }
        Task::none()
    }

    pub(super) fn signed_in(
        &mut self,
        generation: u64,
        result: Result<SignedIn, Failure>,
    ) -> Task<Message> {
        self.refresh();
        if generation != self.google.generation || !self.google.waiting() {
            return Task::none();
        }
        self.google.cancel();
        let result = result.and_then(|signed| {
            let id = format!("google-drive:{}", signed.account.id);
            self.sync
                .credentials
                .save(&id, &signed.token)
                .map_err(Failure::Other)?;
            self.engine
                .set_sync_target(Some(SyncTarget::Remote {
                    id,
                    label: signed.account.email,
                }))
                .map_err(Failure::from)
        });
        match result {
            Ok(()) => {
                self.error = None;
                self.sync.invalidate();
                self.refresh();
                self.sync.request()
            }
            Err(error) => {
                self.error = Some(error.to_string());
                Task::none()
            }
        }
    }
}

fn authorize(client: Client, generation: u64, cancel: Arc<AtomicBool>) -> Task<Message> {
    let (sender, receiver) = iced::futures::channel::oneshot::channel();
    let authorization = Task::perform(
        sync::blocking(move || {
            tinytrac_drive::authorize(&client, &Endpoints::default(), &cancel, |url| {
                open_browser(url, sender)
            })
            .map(|authorization| SignedIn {
                account: authorization.account,
                token: authorization.refresh_token,
            })
            .map_err(Failure::from)
        }),
        move |result| Message::SignedIn(generation, result),
    );
    let browser = Task::perform(
        async move { receiver.await.unwrap_or(Ok(())) },
        move |result| Message::BrowserFinished(generation, result),
    );
    Task::batch([authorization, browser])
}

fn open_browser(
    url: &str,
    sender: iced::futures::channel::oneshot::Sender<Result<(), Failure>>,
) -> std::io::Result<()> {
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(launcher).arg(url).spawn()?;
    // A launcher can remain alive until the browser closes. Reap it separately
    // so authorization keeps accepting callbacks and observing cancellation.
    std::thread::spawn(move || {
        let result = match child.wait() {
            Ok(status) if status.success() => Ok(()),
            _ => Err(Failure::Other("Could not open the sign-in browser".into())),
        };
        let _ = sender.send(result);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{Memory, Store};
    use tinytrac_engine::Engine;

    fn app() -> (App, Arc<Memory>) {
        configured(Engine::open_in_memory().unwrap())
    }
    fn configured(engine: Engine) -> (App, Arc<Memory>) {
        let mut app = App::new(engine).unwrap();
        let store = Arc::new(Memory::default());
        app.sync.credentials = store.clone();
        app.sync.client = Some(Client {
            id: "test".into(),
            secret: "test".into(),
        });
        let _ = app.google_action(&settings::Message::SyncMode(SyncMode::Google));
        (app, store)
    }
    fn start(app: &mut App) -> u64 {
        // Dropping an unpolled iced task avoids all browser and network I/O.
        let _ = app.google_action(&settings::Message::SignIn);
        app.google.generation
    }
    fn success() -> SignedIn {
        SignedIn {
            account: Account {
                id: "123".into(),
                email: "me@example.com".into(),
            },
            token: Secret::new("refresh".into()),
        }
    }

    #[test]
    fn sign_in_and_local_sign_out() {
        let (mut app, store) = app();
        let generation = start(&mut app);
        let _ = app.signed_in(generation, Ok(success()));
        assert_eq!(
            store.load("google-drive:123").unwrap().unwrap().expose(),
            "refresh"
        );
        assert_eq!(
            app.data.sync_target,
            Some(SyncTarget::Remote {
                id: "google-drive:123".into(),
                label: "me@example.com".into()
            })
        );
        let _ = app.google_action(&settings::Message::SignOut);
        assert!(app.data.sync_target.is_none());
        assert!(store.load("google-drive:123").unwrap().is_none());
    }

    #[test]
    fn navigating_keeps_pending_google_selection_and_external_target_changes_cancel() {
        let (mut app, store) = app();
        let generation = start(&mut app);
        app.show(super::super::Screen::Settings);
        assert_eq!(app.settings.sync_mode, SyncMode::Google);
        app.engine
            .set_sync_target(Some(SyncTarget::Remote {
                id: "other".into(),
                label: "Other".into(),
            }))
            .unwrap();
        let _ = app.signed_in(generation, Ok(success()));
        assert!(store.tokens.lock().unwrap().is_empty());
        assert!(!app.google.waiting());
    }

    #[test]
    fn keychain_failure_preserves_target_and_reports_error() {
        let (mut app, store) = app();
        let original = Some(SyncTarget::Remote {
            id: "old".into(),
            label: "Old account".into(),
        });
        app.engine.set_sync_target(original.clone()).unwrap();
        app.refresh();
        store.fail.store(true, Ordering::Relaxed);
        let generation = start(&mut app);
        let _ = app.signed_in(generation, Ok(success()));
        assert_eq!(app.data.sync_target, original);
        assert!(store.load("google-drive:123").unwrap().is_none());
        assert_eq!(app.error.as_deref(), Some("Keychain locked"));
    }

    #[test]
    fn cancelled_replaced_and_target_changed_results_store_nothing() {
        let (mut app, store) = app();
        let cancelled = start(&mut app);
        let flag = app.google.cancel.clone().unwrap();
        let _ = app.google_action(&settings::Message::CancelSignIn);
        assert!(flag.load(Ordering::Relaxed));
        let _ = app.signed_in(cancelled, Ok(success()));
        let old = start(&mut app);
        let newer = start(&mut app);
        let _ = app.signed_in(old, Ok(success()));
        assert!(app.google.waiting());
        let _ = app.google_action(&settings::Message::SyncMode(SyncMode::Folder));
        let _ = app.signed_in(newer, Ok(success()));
        assert!(store.tokens.lock().unwrap().is_empty());
        assert!(app.data.sync_target.is_none());
    }

    #[test]
    fn auth_failure_pauses_and_sign_in_resumes_without_repeated_banner() {
        let directory = std::env::temp_dir().join(format!(
            "tinytrac-auth-{}",
            tinytrac_core::TagId::generate()
        ));
        std::fs::create_dir(&directory).unwrap();
        let (mut app, _) = configured(Engine::open(directory.join("db")).unwrap());
        let generation = start(&mut app);
        assert_eq!(app.signed_in(generation, Ok(success())).units(), 1);
        let failure = Err(Failure::AuthRequired("expired".into()));
        let _ = app.update(Message::Synced(failure.clone()));
        assert!(app.sync.auth_required);
        assert!(app.error.is_some());
        let _ = app.update(Message::DismissError);
        for _ in 0..3 {
            assert_eq!(app.update(Message::Refresh).units(), 0);
        }
        assert!(!app.sync.queued);
        let _ = app.update(Message::Synced(failure));
        assert!(app.error.is_none());
        let generation = start(&mut app);
        assert_eq!(app.signed_in(generation, Ok(success())).units(), 1);
        assert!(!app.sync.auth_required);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn folder_selection_preserves_same_target_progress_and_republishes_on_switch() {
        let (mut app, _) = app();
        app.engine.create_tag("Local history").unwrap();
        let folder = std::env::temp_dir().join(format!(
            "tinytrac-folder-{}",
            tinytrac_core::TagId::generate()
        ));
        let _ = app.google_action(&settings::Message::SyncMode(SyncMode::Folder));
        let _ = app.update(Message::Settings(settings::Message::SyncFolder(
            folder.display().to_string(),
        )));
        let _ = app.update(Message::Settings(settings::Message::SaveSyncFolder));
        let first = app.engine.sync_context().unwrap().unwrap();
        let _ = app.update(Message::Settings(settings::Message::SaveSyncFolder));
        assert_eq!(
            first.epoch(),
            app.engine.sync_context().unwrap().unwrap().epoch()
        );
        let _ = app.google_action(&settings::Message::SyncMode(SyncMode::Google));
        let generation = start(&mut app);
        let _ = app.signed_in(generation, Ok(success()));
        assert!(app.engine.sync_context().unwrap().unwrap().epoch() > first.epoch());
        assert_eq!(app.engine.tags().unwrap().len(), 1);
        let _ = app.google_action(&settings::Message::SyncMode(SyncMode::Folder));
        let _ = app.update(Message::Settings(settings::Message::SaveSyncFolder));
        assert_eq!(
            app.data.sync_target,
            Some(SyncTarget::Folder(folder.clone()))
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn late_sync_auth_failure_cannot_expire_new_sign_in() {
        let (mut app, _) = app();
        let generation = start(&mut app);
        let _ = app.signed_in(generation, Ok(success()));
        let _ = app.update(Message::WorkerSynced(
            0,
            Err(Failure::AuthRequired("old account".into())),
        ));
        assert!(!app.sync.auth_required);
        assert!(app.error.is_none());
    }
}
