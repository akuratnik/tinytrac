//! Retained transport and serialized engine work.

use tinytrac_engine::{Engine, Folder, SyncContext, SyncRun, SyncTarget, Transport};

/// Retains transport caches between serialized runs, bound to one context.
#[derive(Debug)]
pub(super) struct Worker<T = Backend> {
    pub(super) engine: Engine,
    pub(super) transport: Option<T>,
    pub(super) generation: u64,
}

#[derive(Debug)]
pub(super) enum Backend {
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
    pub(super) fn run(
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
                .map_err(|error| tinytrac_engine::Error::AuthRequired(error.to_string()))?
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
            .is_some_and(|(transport, context)| transport.context().compatible_with(context));
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

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::data::Data;

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
    fn worker_replaces_a_transport_from_another_device() {
        let mut engine = Engine::open_in_memory().unwrap();
        let mut other = Engine::open_in_memory().unwrap();
        for engine in [&mut engine, &mut other] {
            engine
                .set_sync_target(Some(SyncTarget::Remote {
                    id: "same".into(),
                    label: "Same".into(),
                }))
                .unwrap();
        }
        let drops = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut worker = Worker {
            engine,
            transport: Some(Cached {
                context: other.sync_context().unwrap().unwrap(),
                lists: 0,
                drops: drops.clone(),
            }),
            generation: 0,
        };
        worker
            .run_with(|context| {
                Ok(Cached {
                    context: context.clone(),
                    lists: 0,
                    drops: drops.clone(),
                })
            })
            .unwrap();
        assert_eq!(drops.get(), 1);
        assert_eq!(worker.transport.as_ref().unwrap().lists, 1);
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
}
