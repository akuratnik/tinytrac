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
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return None;
    }
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

#[derive(Debug, Clone)]
pub struct Saved {
    account: Account,
    previous: Option<Secret>,
}

#[derive(Debug)]
pub(super) struct State {
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
    busy: Option<u64>,
    link: Option<String>,
    pub(super) client: Option<Client>,
    pub(super) credentials: Option<Arc<dyn crate::credentials::Store>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            generation: 0,
            cancel: None,
            busy: None,
            link: None,
            client: client(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            credentials: Some(Arc::new(crate::credentials::Keychain)),
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            credentials: None,
        }
    }
}
impl State {
    pub(super) fn waiting(&self) -> bool {
        self.cancel.is_some() || self.busy.is_some()
    }
    pub(super) fn busy(&self) -> bool {
        self.busy.is_some()
    }
    pub(super) fn cancel(&mut self) {
        self.generation += 1;
        self.link = None;
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    pub(super) fn account<'a>(
        &'a self,
        target: Option<&'a SyncTarget>,
        reason: Option<&'a str>,
    ) -> settings::AccountState<'a> {
        if self.busy.is_some() {
            return settings::AccountState::Saving;
        }
        if self.waiting() {
            return if self.link.is_some() {
                settings::AccountState::LinkFallback
            } else {
                settings::AccountState::Waiting
            };
        }
        if self.client.is_none() {
            return settings::AccountState::Unavailable;
        }
        match target {
            Some(SyncTarget::Remote { label, .. }) => reason
                .map_or(settings::AccountState::SignedIn(label), |reason| {
                    settings::AccountState::Expired { label, reason }
                }),
            _ => settings::AccountState::SignedOut,
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl App {
    pub(super) fn google_action(&mut self, message: &settings::Account) -> Task<Message> {
        if self.google.busy.is_some() {
            return Task::none();
        }
        match message {
            settings::Account::CopyLink => {
                if let Some(link) = &self.google.link {
                    return iced::clipboard::write(link.clone());
                }
            }
            settings::Account::SignIn => {
                let Some(client) = self.google.client.clone() else {
                    return Task::none();
                };
                self.google.cancel();
                let cancel = Arc::new(AtomicBool::new(false));
                self.google.cancel = Some(cancel.clone());
                return authorize(client, self.google.generation, cancel);
            }
            settings::Account::CancelSignIn => self.google.cancel(),
            settings::Account::SignOut => return self.change_target(None, false),
            settings::Account::SyncMode(mode) => {
                if *mode == SyncMode::Google && self.google.client.is_none() {
                    return Task::none();
                }
                if *mode != self.settings.sync_mode {
                    self.google.cancel();
                    self.settings.sync_mode = *mode;
                    if *mode == SyncMode::Off {
                        return self.change_target(None, false);
                    }
                }
            }
        }
        Task::none()
    }

    // All concrete target changes retire the old account through the same path.
    pub(super) fn change_target(
        &mut self,
        target: Option<SyncTarget>,
        renewed: bool,
    ) -> Task<Message> {
        match self.commit_target(target, renewed) {
            Ok(task) => task,
            Err(error) => {
                self.error = Some(error.to_string());
                Task::none()
            }
        }
    }

    fn commit_target(
        &mut self,
        target: Option<SyncTarget>,
        renewed: bool,
    ) -> tinytrac_engine::Result<Task<Message>> {
        let previous = self.data.sync_target.clone();
        self.engine.set_sync_target(target)?;
        self.google.cancel();
        self.error = None;
        self.refresh_sync(renewed);
        let cleanup = self.sign_out(previous);
        Ok(Task::batch([cleanup, self.sync.request(&self.google)]))
    }

    fn sign_out(&mut self, previous: Option<SyncTarget>) -> Task<Message> {
        let Some(SyncTarget::Remote { id, .. }) = previous else {
            return Task::none();
        };
        if matches!(&self.data.sync_target, Some(SyncTarget::Remote { id: active, .. }) if active == &id)
        {
            return Task::none();
        }
        let Some(credentials) = self.google.credentials.clone() else {
            return Task::none();
        };
        let generation = self.google.generation;
        self.google.busy = Some(generation);
        Task::perform(
            sync::blocking(move || {
                credentials
                    .delete(&id)
                    .map_err(|error| Failure::Other(error.to_string()))
            }),
            move |result| Message::CredentialsDeleted(generation, result),
        )
    }

    pub(super) fn credentials_deleted(
        &mut self,
        generation: u64,
        result: Result<(), Failure>,
    ) -> Task<Message> {
        if self.google.busy != Some(generation) {
            return Task::none();
        }
        self.google.busy = None;
        if generation == self.google.generation
            && let Err(error) = result
        {
            self.error = Some(error.to_string());
        }
        Task::none()
    }

    pub(super) fn signed_in(
        &mut self,
        generation: u64,
        result: Result<Option<SignedIn>, Failure>,
    ) -> Task<Message> {
        self.refresh();
        if generation != self.google.generation
            || !self.google.waiting()
            || self.google.busy.is_some()
        {
            return Task::none();
        }
        match result {
            Ok(Some(signed)) => {
                let Some(credentials) = self.google.credentials.clone() else {
                    return Task::none();
                };
                self.google.busy = Some(generation);
                Task::perform(
                    sync::blocking(move || {
                        let id = format!("google-drive:{}", signed.account.id);
                        let previous = credentials
                            .load(&id)
                            .map_err(|error| Failure::Other(error.to_string()))?;
                        credentials
                            .save(&id, &signed.token)
                            .map_err(|error| Failure::Other(error.to_string()))?;
                        Ok(Saved {
                            account: signed.account,
                            previous,
                        })
                    }),
                    move |result| Message::CredentialsSaved(generation, result),
                )
            }
            result => {
                self.google.cancel();
                self.error = result.err().map(|error| error.to_string());
                Task::none()
            }
        }
    }

    pub(super) fn credentials_saved(
        &mut self,
        generation: u64,
        result: Result<Saved, Failure>,
    ) -> Task<Message> {
        if self.google.busy != Some(generation) {
            return Task::none();
        }
        self.refresh();
        self.google.busy = None;
        let saved = match result {
            Ok(saved) => saved,
            Err(error) => {
                if generation == self.google.generation {
                    self.google.cancel();
                    self.error = Some(error.to_string());
                }
                return Task::none();
            }
        };
        let id = format!("google-drive:{}", saved.account.id);
        if generation == self.google.generation {
            match self.commit_target(
                Some(SyncTarget::Remote {
                    id: id.clone(),
                    label: saved.account.email,
                }),
                true,
            ) {
                Ok(task) => return task,
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        // A stale completion or failed target write must not leave an orphan token,
        // or overwrite the prior token when signing into the same account again.
        self.google.cancel();
        let Some(credentials) = self.google.credentials.clone() else {
            return Task::none();
        };
        let generation = self.google.generation;
        self.google.busy = Some(generation);
        Task::perform(
            sync::blocking(move || {
                match saved.previous {
                    Some(token) => credentials.save(&id, &token),
                    None => credentials.delete(&id),
                }
                .map_err(|error| Failure::Other(error.to_string()))
            }),
            move |result| Message::CredentialsDeleted(generation, result),
        )
    }

    pub(super) fn browser_finished(
        &mut self,
        generation: u64,
        link: Option<String>,
    ) -> Task<Message> {
        if generation == self.google.generation && self.google.waiting() {
            self.google.link = link;
        }
        Task::none()
    }
}

fn authorize(client: Client, generation: u64, cancel: Arc<AtomicBool>) -> Task<Message> {
    let (sender, receiver) = iced::futures::channel::oneshot::channel();
    let authorization = Task::perform(
        sync::blocking(move || {
            tinytrac_drive::authorize(&client, &Endpoints::default(), &cancel, |url| {
                open_browser(url, sender);
                Ok(())
            })
            .map(|authorization| {
                authorization.map(|authorization| SignedIn {
                    account: authorization.account,
                    token: authorization.refresh_token,
                })
            })
            .map_err(Failure::from)
        }),
        move |result| Message::SignedIn(generation, result),
    );
    let browser = Task::perform(async move { receiver.await.unwrap_or(None) }, move |link| {
        Message::BrowserFinished(generation, link)
    });
    Task::batch([authorization, browser])
}

fn open_browser(url: &str, sender: iced::futures::channel::oneshot::Sender<Option<String>>) {
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let child = Command::new(launcher).arg(url).spawn();
    let url = url.to_owned();
    std::thread::spawn(move || {
        let opened = child
            .and_then(|mut child| child.wait())
            .is_ok_and(|status| status.success());
        let _ = sender.send((!opened).then_some(url));
    });
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
        app.google.credentials = Some(store.clone());
        app.google.client = Some(Client {
            id: "test".into(),
            secret: "test".into(),
        });
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Google));
        (app, store)
    }
    fn start(app: &mut App) -> u64 {
        // Dropping an unpolled iced task avoids all browser and network I/O.
        let _ = app.google_action(&settings::Account::SignIn);
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

    fn complete(
        app: &mut App,
        generation: u64,
        result: Result<Option<SignedIn>, Failure>,
    ) -> Task<Message> {
        let task = app.signed_in(generation, result);
        if app.google.busy.is_none() {
            return task;
        }
        let next = deliver(app, task);
        if app.google.busy.is_some() {
            let _ = deliver(app, next);
            Task::none()
        } else {
            next
        }
    }

    fn deliver(app: &mut App, task: Task<Message>) -> Task<Message> {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_test::runtime::{Action, task};
        let Some(mut stream) = task::into_stream(task) else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        while let Some(action) = block_on(stream.next()) {
            if let Action::Output(message) = action {
                tasks.push(app.update(message));
            }
        }
        Task::batch(tasks)
    }

    #[test]
    fn exploring_modes_preserves_folder_and_cancelled_sign_in() {
        let (mut app, _) = app();
        let directory = tempfile::tempdir().unwrap();
        let target = Some(SyncTarget::Folder(directory.path().to_owned()));
        app.engine.set_sync_target(target.clone()).unwrap();
        app.refresh();
        let epoch = app.engine.sync_context().unwrap().unwrap().epoch();
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Folder));
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Google));
        let generation = start(&mut app);
        let _ = complete(
            &mut app,
            generation,
            Err(Failure::Other("Cancelled in browser".into())),
        );
        assert_eq!(app.data.sync_target, target);
        assert_eq!(app.engine.sync_context().unwrap().unwrap().epoch(), epoch);
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            (600, 1000),
            app.settings.view(
                &app.data,
                &app.sync.status,
                app.google.account(app.data.sync_target.as_ref(), None),
            ),
        );
        assert!(ui.find("Sign in with Google").is_ok());
        assert!(
            ui.find(format!(
                "Still syncing to {} until you finish signing in.",
                directory.path().display()
            ))
            .is_ok()
        );
    }

    #[test]
    fn saving_folder_retires_google_only_after_save() {
        let (mut app, store) = app();
        let generation = start(&mut app);
        let _ = complete(&mut app, generation, Ok(Some(success())));
        let target = app.data.sync_target.clone();
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Folder));
        assert_eq!(app.data.sync_target, target);
        assert!(store.load("google-drive:123").unwrap().is_some());
        let directory = tempfile::tempdir().unwrap();
        let _ = app.update(Message::Settings(settings::Message::SyncFolder(
            directory.path().display().to_string(),
        )));
        let task = app.update(Message::Settings(settings::Message::SaveSyncFolder));
        assert_eq!(
            app.data.sync_target,
            Some(SyncTarget::Folder(directory.path().to_owned()))
        );
        assert!(store.load("google-drive:123").unwrap().is_some());
        let _ = deliver(&mut app, task);
        assert!(store.load("google-drive:123").unwrap().is_none());
    }

    #[test]
    fn save_completion_is_required_and_stale_save_restores_prior_token() {
        let (mut app, store) = app();
        store
            .save("google-drive:123", &Secret::new("prior".into()))
            .unwrap();
        let generation = start(&mut app);
        let task = app.signed_in(generation, Ok(Some(success())));
        assert!(app.data.sync_target.is_none());
        assert_eq!(
            store.load("google-drive:123").unwrap().unwrap().expose(),
            "prior"
        );
        // Another process changes this device's target while the keychain prompt is open.
        let directory = tempfile::tempdir().unwrap();
        let target = Some(SyncTarget::Folder(directory.path().to_owned()));
        app.engine.set_sync_target(target.clone()).unwrap();
        let rollback = deliver(&mut app, task);
        assert_eq!(app.data.sync_target, target);
        let _ = deliver(&mut app, rollback);
        assert_eq!(
            store.load("google-drive:123").unwrap().unwrap().expose(),
            "prior"
        );
        assert!(!app.google.waiting());
    }

    #[test]
    fn browser_failure_keeps_authorization_and_copy_link_until_cancelled() {
        let (mut app, _) = app();
        let generation = start(&mut app);
        let _ = app.browser_finished(generation, Some("https://example.com/sign-in".into()));
        assert!(matches!(
            app.google.account(None, None),
            settings::AccountState::LinkFallback
        ));
        assert_eq!(app.google_action(&settings::Account::CopyLink).units(), 1);
        let _ = app.google_action(&settings::Account::CancelSignIn);
        let _ = app.browser_finished(generation, Some("stale".into()));
        assert!(app.google.link.is_none());
        assert!(!app.google.waiting());
    }

    #[test]
    fn leaving_google_deletes_token() {
        let (mut app, store) = app();
        let generation = start(&mut app);
        let _ = complete(&mut app, generation, Ok(Some(success())));
        let task = app.google_action(&settings::Account::SyncMode(SyncMode::Off));
        let _ = deliver(&mut app, task);
        assert!(app.data.sync_target.is_none());
        assert!(store.load("google-drive:123").unwrap().is_none());
    }

    #[test]
    fn replacing_account_deletes_previous_token() {
        let (mut app, store) = app();
        store.save("old", &Secret::new("old token".into())).unwrap();
        app.engine
            .set_sync_target(Some(SyncTarget::Remote {
                id: "old".into(),
                label: "Old".into(),
            }))
            .unwrap();
        app.refresh();
        let generation = start(&mut app);
        let _ = complete(&mut app, generation, Ok(Some(success())));
        assert!(store.load("old").unwrap().is_none());
        assert!(store.load("google-drive:123").unwrap().is_some());
    }

    #[test]
    fn cancelled_authorization_clears_waiting_without_an_error() {
        let (mut app, store) = app();
        let generation = start(&mut app);
        assert_eq!(complete(&mut app, generation, Ok(None)).units(), 0);
        assert!(!app.google.waiting());
        assert!(app.error.is_none());
        assert!(app.data.sync_target.is_none());
        assert!(store.tokens.lock().unwrap().is_empty());
    }

    #[test]
    fn sign_in_and_local_sign_out() {
        let (mut app, store) = app();
        let generation = start(&mut app);
        let _ = complete(&mut app, generation, Ok(Some(success())));
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
        let task = app.google_action(&settings::Account::SignOut);
        let _ = deliver(&mut app, task);
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
        let _ = complete(&mut app, generation, Ok(Some(success())));
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
        let _ = complete(&mut app, generation, Ok(Some(success())));
        assert_eq!(app.data.sync_target, original);
        assert!(store.load("google-drive:123").unwrap().is_none());
        assert!(
            app.error
                .as_deref()
                .unwrap()
                .contains("unlock your login keychain")
        );
    }

    #[test]
    fn cancelled_replaced_and_target_changed_results_store_nothing() {
        let (mut app, store) = app();
        let cancelled = start(&mut app);
        let flag = app.google.cancel.clone().unwrap();
        let _ = app.google_action(&settings::Account::CancelSignIn);
        assert!(flag.load(Ordering::Relaxed));
        let _ = app.signed_in(cancelled, Ok(Some(success())));
        let old = start(&mut app);
        let newer = start(&mut app);
        let _ = app.signed_in(old, Ok(Some(success())));
        assert!(app.google.waiting());
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Folder));
        let _ = app.signed_in(newer, Ok(Some(success())));
        assert!(store.tokens.lock().unwrap().is_empty());
        assert!(app.data.sync_target.is_none());
    }

    #[test]
    fn auth_failure_pauses_and_sign_in_resumes_without_repeated_banner() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, _) = configured(Engine::open(directory.path().join("db")).unwrap());
        let generation = start(&mut app);
        assert_eq!(
            complete(&mut app, generation, Ok(Some(success()))).units(),
            1
        );
        let failure = Err(Failure::AuthRequired("expired".into()));
        let _ = app.update(Message::Synced(failure.clone()));
        assert!(app.sync.auth_required.is_some());
        assert!(app.error.is_some());
        let _ = app.update(Message::DismissError);
        for _ in 0..3 {
            assert_eq!(app.update(Message::Refresh).units(), 0);
        }
        assert!(!app.sync.queued);
        let _ = app.update(Message::Synced(failure));
        assert!(app.error.is_none());
        let generation = start(&mut app);
        assert_eq!(
            complete(&mut app, generation, Ok(Some(success()))).units(),
            1
        );
        assert!(app.sync.auth_required.is_none());
        drop(app);
    }

    #[test]
    fn folder_selection_preserves_same_target_progress_and_republishes_on_switch() {
        let (mut app, _) = app();
        app.engine.create_tag("Local history").unwrap();
        let folder = std::env::temp_dir().join(format!(
            "tinytrac-folder-{}",
            tinytrac_core::TagId::generate()
        ));
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Folder));
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
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Google));
        let generation = start(&mut app);
        let _ = complete(&mut app, generation, Ok(Some(success())));
        assert!(app.engine.sync_context().unwrap().unwrap().epoch() > first.epoch());
        assert_eq!(app.engine.tags().unwrap().len(), 1);
        let _ = app.google_action(&settings::Account::SyncMode(SyncMode::Folder));
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
        let _ = complete(&mut app, generation, Ok(Some(success())));
        let _ = app.update(Message::WorkerSynced(
            0,
            Err(Failure::AuthRequired("old account".into())),
        ));
        assert!(app.sync.auth_required.is_none());
        assert!(app.error.is_none());
    }
}
