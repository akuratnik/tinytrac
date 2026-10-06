//! Background sync, queued requests, and persistent problem notifications.

use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

use iced::Task;
use tinytrac_engine::{Engine, Folder, SyncContext, SyncRun, SyncTarget, Transport};

use super::Message;
use crate::{data::Data, screen};

/// Keeps authentication failures distinct across the cloneable UI message boundary.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Failure {
    #[error("authentication required: {0}")]
    AuthRequired(String),
    #[error("{0}")]
    Other(String),
}

impl From<tinytrac_engine::Error> for Failure {
    fn from(error: tinytrac_engine::Error) -> Self {
        match error {
            tinytrac_engine::Error::AuthRequired(message) => Self::AuthRequired(message),
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Other(message.into())
    }
}

#[derive(Debug)]
pub(super) struct State {
    engine: Option<Arc<Mutex<Worker>>>,
    pub(super) syncing: bool,
    pub(super) queued: bool,
    pub(super) problems: SyncProblems,
    pub(super) status: String,
    last_success: Option<String>,
    pub(super) auth_required: bool,
    generation: u64,
    pub(super) client: Option<tinytrac_drive::Client>,
    pub(super) credentials: Arc<dyn crate::credentials::Store>,
}

impl State {
    pub(super) fn new(engine: &Engine, data: &Data) -> screen::Result<Self> {
        // A second connection, so syncing never blocks the window's reads and writes.
        let worker = engine.path().map(Engine::open).transpose()?.map(|engine| {
            Arc::new(Mutex::new(Worker {
                engine,
                transport: None,
                generation: 0,
            }))
        });
        let problems = SyncProblems {
            rejected: data.sync_diagnostics.rejected,
            ..Default::default()
        };
        let status = if data.sync_target.is_none() {
            "Sync disabled"
        } else {
            "Not synced yet"
        }
        .into();
        Ok(Self {
            engine: worker,
            syncing: false,
            queued: false,
            problems,
            status,
            last_success: None,
            auth_required: false,
            generation: 0,
            client: super::google::client(),
            credentials: Arc::new(crate::credentials::Keychain),
        })
    }

    pub(super) fn request(&mut self) -> Task<Message> {
        if self.auth_required {
            return Task::none();
        }
        let Some(engine) = self.engine.clone() else {
            return Task::none();
        };
        if self.syncing {
            self.queued = true;
            return Task::none();
        }
        self.syncing = true;
        let generation = self.generation;
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        Task::perform(
            blocking(move || {
                let mut worker = engine
                    .lock()
                    .map_err(|error| Failure::Other(error.to_string()))?;
                if worker.generation != generation {
                    worker.generation = generation;
                    worker.transport = None;
                }
                worker
                    .run(client, credentials.as_ref())
                    .map_err(Failure::from)
            }),
            move |result| Message::WorkerSynced(generation, result),
        )
    }

    pub(super) fn invalidate(&mut self) {
        self.generation += 1;
        self.auth_required = false;
        self.problems.transient.clear();
        self.last_success = None;
        self.status = "Not synced yet".into();
    }

    pub(super) fn current(&mut self, generation: u64) -> bool {
        if generation == self.generation {
            return true;
        }
        self.syncing = false;
        self.queued = false;
        false
    }

    pub(super) fn finish(
        &mut self,
        result: &Result<SyncRun, Failure>,
        data: &Data,
        now: &jiff::Zoned,
    ) -> (Option<String>, Task<Message>) {
        self.syncing = false;
        self.auth_required = matches!(result, Err(Failure::AuthRequired(_)))
            && matches!(data.sync_target, Some(SyncTarget::Remote { .. }));
        let mut problems = match result {
            Ok(run) => run
                .skipped
                .iter()
                .map(|issue| {
                    format!(
                        "{}: {}",
                        issue.file.as_deref().unwrap_or("sync"),
                        issue.reason
                    )
                })
                .collect::<Vec<_>>(),
            Err(error) => vec![error.to_string()],
        };
        problems.sort();
        problems.dedup();
        let at = now.strftime("%H:%M:%S").to_string();
        self.status = match result {
            _ if data.sync_target.is_none() => "Sync disabled".into(),
            Ok(_) if !problems.is_empty() => {
                format!("Sync incomplete at {at}: {}", problems.join("; "))
            }
            Ok(run) => format!(
                "Synced at {at}: {} exported, {} imported",
                run.exported, run.imported
            ),
            Err(error) => format!("Sync failed at {at}: {error}"),
        };
        if data.sync_target.is_some() {
            if result.is_ok() && problems.is_empty() {
                self.last_success = Some(at);
            } else if let Some(at) = &self.last_success {
                let _ = write!(self.status, "\nLast successful sync at {at}");
            }
        }
        let rejected = data.sync_diagnostics.rejected;
        let banner = self.problems.update(rejected, problems);
        let task = if std::mem::take(&mut self.queued) {
            self.request()
        } else {
            Task::none()
        };
        (banner, task)
    }
}

/// Compare problem identities, not run timestamps, so dismissal survives retries.
#[derive(Debug, Default)]
pub(super) struct SyncProblems {
    pub(super) rejected: i64,
    transient: Vec<String>,
}

impl SyncProblems {
    fn update(&mut self, rejected: i64, transient: Vec<String>) -> Option<String> {
        let mut new = Vec::new();
        let count = rejected - self.rejected;
        if count > 0 {
            new.push(format!(
                "{count} new {} rejected",
                if count == 1 {
                    "change was"
                } else {
                    "changes were"
                }
            ));
        }
        new.extend(
            transient
                .iter()
                .filter(|problem| !self.transient.contains(problem))
                .cloned(),
        );
        self.rejected = rejected;
        self.transient = transient;
        (!new.is_empty()).then(|| new.join("; "))
    }
}

/// Retains transport caches between serialized runs, bound to one context.
#[derive(Debug)]
struct Worker<T = Backend> {
    engine: Engine,
    transport: Option<T>,
    generation: u64,
}

#[derive(Debug)]
enum Backend {
    Folder(Folder),
    Drive(Box<tinytrac_drive::Drive>),
}
impl Transport for Backend {
    fn context(&self) -> &SyncContext {
        match self {
            Self::Folder(t) => t.context(),
            Self::Drive(t) => t.context(),
        }
    }
    fn publish(&mut self, name: &str, bytes: &[u8]) -> tinytrac_engine::Result<()> {
        match self {
            Self::Folder(t) => t.publish(name, bytes),
            Self::Drive(t) => t.publish(name, bytes),
        }
    }
    fn list(&mut self) -> tinytrac_engine::Result<Vec<String>> {
        match self {
            Self::Folder(t) => t.list(),
            Self::Drive(t) => t.list(),
        }
    }
    fn read(&mut self, key: &str) -> tinytrac_engine::Result<Vec<u8>> {
        match self {
            Self::Folder(t) => t.read(key),
            Self::Drive(t) => t.read(key),
        }
    }
}
impl Worker {
    fn run(
        &mut self,
        client: Option<tinytrac_drive::Client>,
        credentials: &dyn crate::credentials::Store,
    ) -> tinytrac_engine::Result<SyncRun> {
        self.run_with(|context| {
            if let Some(folder) = Folder::for_context(context) {
                return Ok(Backend::Folder(folder));
            }
            let SyncTarget::Remote { id, .. } = context.target() else {
                unreachable!()
            };
            let client = client.ok_or_else(|| {
                tinytrac_engine::Error::AuthRequired(
                    "This build has no Google client credentials".into(),
                )
            })?;
            let token = credentials
                .load(id)
                .map_err(tinytrac_engine::Error::AuthRequired)?
                .ok_or_else(|| {
                    tinytrac_engine::Error::AuthRequired(
                        "Google sign-in expired; sign in again".into(),
                    )
                })?;
            Ok(Backend::Drive(Box::new(tinytrac_drive::Drive::new(
                client,
                token,
                context.clone(),
                tinytrac_drive::Endpoints::default(),
            ))))
        })
    }
}

impl<T: Transport> Worker<T> {
    fn run_with(
        &mut self,
        build: impl FnOnce(&SyncContext) -> tinytrac_engine::Result<T>,
    ) -> tinytrac_engine::Result<SyncRun> {
        let context = self.engine.sync_context()?;
        self.prepare(context.as_ref(), build)?;
        self.sync()
    }

    fn prepare(
        &mut self,
        context: Option<&SyncContext>,
        build: impl FnOnce(&SyncContext) -> tinytrac_engine::Result<T>,
    ) -> tinytrac_engine::Result<()> {
        let reusable = self
            .transport
            .as_ref()
            .zip(context)
            .is_some_and(|(transport, context)| {
                transport.context().epoch() == context.epoch()
                    && transport.context().target().same_identity(context.target())
            });
        if !reusable {
            self.transport = None;
            if let Some(context) = context {
                self.transport = Some(build(context)?);
            }
        }
        Ok(())
    }

    fn sync(&mut self) -> tinytrac_engine::Result<SyncRun> {
        let Some(transport) = &mut self.transport else {
            return Ok(SyncRun::default());
        };
        match self.engine.sync(transport) {
            Err(tinytrac_engine::Error::StaleSyncContext) => Ok(SyncRun::default()),
            result => result,
        }
    }
}

pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    let (sender, receiver) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .await
        .unwrap_or_else(|error| Err(Failure::Other(error.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinytrac_engine::{SyncIssue, SyncTarget};

    #[derive(Debug)]
    struct Cached {
        context: SyncContext,
        lists: usize,
        drops: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Drop for Cached {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    impl Transport for Cached {
        fn context(&self) -> &SyncContext {
            &self.context
        }
        fn publish(&mut self, _: &str, _: &[u8]) -> tinytrac_engine::Result<()> {
            Ok(())
        }
        fn list(&mut self) -> tinytrac_engine::Result<Vec<String>> {
            self.lists += 1;
            Ok(Vec::new())
        }
        fn read(&mut self, _: &str) -> tinytrac_engine::Result<Vec<u8>> {
            unreachable!()
        }
    }

    #[test]
    fn remote_without_token_requires_auth_before_network() {
        let mut engine = Engine::open_in_memory().unwrap();
        engine
            .set_sync_target(Some(SyncTarget::Remote {
                id: "google-drive:missing".into(),
                label: "Missing".into(),
            }))
            .unwrap();
        let mut worker = Worker {
            engine,
            transport: None,
            generation: 0,
        };
        let credentials = crate::credentials::Memory::default();
        let client = tinytrac_drive::Client {
            id: "test".into(),
            secret: "test".into(),
        };
        assert!(matches!(
            worker.run(Some(client), &credentials),
            Err(tinytrac_engine::Error::AuthRequired(_))
        ));
        assert!(worker.transport.is_none());
    }

    #[test]
    fn worker_preserves_caches_until_identity_or_epoch_changes() {
        let drops = std::rc::Rc::new(std::cell::Cell::new(0));
        let build = |context: &SyncContext| {
            Ok(Cached {
                context: context.clone(),
                lists: 0,
                drops: drops.clone(),
            })
        };
        let mut worker = Worker {
            engine: Engine::open_in_memory().unwrap(),
            transport: None,
            generation: 0,
        };
        let remote = |id: &str, label: &str| {
            Some(SyncTarget::Remote {
                id: id.into(),
                label: label.into(),
            })
        };
        worker.engine.set_sync_target(remote("a", "Old")).unwrap();
        worker.run_with(build).unwrap();
        worker.run_with(build).unwrap();
        assert_eq!(worker.transport.as_ref().unwrap().lists, 2);
        worker.engine.set_sync_target(remote("a", "New")).unwrap();
        worker.run_with(build).unwrap();
        assert_eq!(worker.transport.as_ref().unwrap().lists, 3);
        assert_eq!(drops.get(), 0);
        worker.engine.set_sync_target(remote("b", "New")).unwrap();
        worker.run_with(build).unwrap();
        assert_eq!(worker.transport.as_ref().unwrap().lists, 1);
        assert_eq!(drops.get(), 1);
        worker.engine.set_sync_target(None).unwrap();
        worker.engine.set_sync_target(remote("b", "New")).unwrap();
        worker.run_with(build).unwrap();
        assert_eq!(drops.get(), 2);
        worker.engine.set_sync_target(None).unwrap();
        worker.run_with(build).unwrap();
        assert!(worker.transport.is_none());
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn target_race_returns_an_empty_run_without_a_banner() {
        let mut engine = Engine::open_in_memory().unwrap();
        engine
            .set_sync_target(Some(SyncTarget::Remote {
                id: "a".into(),
                label: "A".into(),
            }))
            .unwrap();
        let mut worker = Worker {
            engine,
            transport: None,
            generation: 0,
        };
        let context = worker.engine.sync_context().unwrap().unwrap();
        let build = |context: &SyncContext| {
            Ok(Cached {
                context: context.clone(),
                lists: 0,
                drops: std::rc::Rc::new(std::cell::Cell::new(0)),
            })
        };
        worker.prepare(Some(&context), build).unwrap();
        worker.engine.set_sync_target(None).unwrap();
        let run = worker.sync().unwrap();
        assert_eq!(run, SyncRun::default());
        let data = Data::load(&worker.engine).unwrap();
        let mut state = State::new(&worker.engine, &data).unwrap();
        let now = "2026-10-06T12:00:00[UTC]".parse::<jiff::Zoned>().unwrap();
        assert!(state.finish(&Ok(run), &data, &now).0.is_none());
        worker.run_with(build).unwrap();
        assert!(worker.transport.is_none());
    }

    #[test]
    fn incomplete_and_failed_runs_keep_the_last_clean_time() {
        let mut engine = Engine::open_in_memory().unwrap();
        engine
            .set_sync_target(Some(SyncTarget::Remote {
                id: "test".into(),
                label: "Account".into(),
            }))
            .unwrap();
        let data = Data::load(&engine).unwrap();
        let mut state = State::new(&engine, &data).unwrap();
        let now = "2026-10-06T12:00:00[UTC]".parse::<jiff::Zoned>().unwrap();
        let later = "2026-10-06T12:01:00[UTC]".parse::<jiff::Zoned>().unwrap();
        let _ = state.finish(&Ok(SyncRun::default()), &data, &now);
        let partial = SyncRun {
            skipped: vec![SyncIssue {
                file: None,
                reason: "offline".into(),
            }],
            ..SyncRun::default()
        };
        let _ = state.finish(&Ok(partial), &data, &later);
        assert_eq!(
            state.status,
            "Sync incomplete at 12:01:00: sync: offline\nLast successful sync at 12:00:00"
        );
        let failure = Failure::from(tinytrac_engine::Error::AuthRequired("sign in".into()));
        assert!(matches!(failure, Failure::AuthRequired(_)));
        let _ = state.finish(&Err(failure), &data, &later);
        assert_eq!(
            state.status,
            "Sync failed at 12:01:00: authentication required: sign in\nLast successful sync at 12:00:00"
        );
        let _ = state.finish(&Ok(SyncRun::default()), &data, &later);
        assert_eq!(state.status, "Synced at 12:01:00: 0 exported, 0 imported");
    }
}
