//! Background sync, queued requests, and persistent problem notifications.

use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

use iced::Task;
use tinytrac_engine::{Engine, SyncRun};

use super::Message;
use crate::{data::Data, screen};

#[derive(Debug)]
pub(super) struct State {
    engine: Option<Arc<Mutex<Engine>>>,
    pub(super) syncing: bool,
    pub(super) queued: bool,
    pub(super) problems: SyncProblems,
    pub(super) status: String,
}

impl State {
    pub(super) fn new(engine: &Engine, data: &Data) -> screen::Result<Self> {
        // A second connection, so syncing never blocks the window's reads and writes.
        let worker = engine
            .path()
            .map(Engine::open)
            .transpose()?
            .map(|engine| Arc::new(Mutex::new(engine)));
        let problems = SyncProblems {
            rejected: data.sync_diagnostics.rejected,
            ..Default::default()
        };
        let status = if data.sync_folder.is_empty() {
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
        })
    }

    pub(super) fn request(&mut self) -> Task<Message> {
        let Some(engine) = self.engine.clone() else {
            return Task::none();
        };
        if self.syncing {
            self.queued = true;
            return Task::none();
        }
        self.syncing = true;
        Task::perform(
            async move {
                let (sender, receiver) = iced::futures::channel::oneshot::channel();
                std::thread::spawn(move || {
                    let result = engine
                        .lock()
                        .map_err(|error| error.to_string())
                        .and_then(|mut engine| engine.sync().map_err(|error| error.to_string()));
                    let _ = sender.send(result);
                });
                receiver
                    .await
                    .unwrap_or_else(|error| Err(error.to_string()))
            },
            Message::Synced,
        )
    }

    pub(super) fn finish(
        &mut self,
        result: &Result<SyncRun, String>,
        data: &Data,
        now: &jiff::Zoned,
    ) -> (Option<String>, Task<Message>) {
        self.syncing = false;
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
            Err(error) => vec![error.clone()],
        };
        problems.sort();
        problems.dedup();
        let at = now.strftime("%H:%M:%S").to_string();
        self.status = match result {
            _ if data.sync_folder.is_empty() => "Sync disabled".into(),
            Ok(run) => format!(
                "Synced at {at}: {} exported, {} imported",
                run.exported, run.imported
            ),
            Err(error) => format!("Sync failed at {at}: {error}"),
        };
        if result.is_ok() && !problems.is_empty() {
            let _ = write!(self.status, "; {}", problems.join("; "));
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
