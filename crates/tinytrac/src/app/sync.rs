//! Background sync, queued requests, and persistent problem notifications.

use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

use iced::Task;
use tinytrac_engine::{Engine, SyncRun, SyncTarget};

mod worker;
use worker::Worker;

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
