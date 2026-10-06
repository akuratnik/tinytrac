//! Immutable batches keep the transport separate from revision merging.

use std::{fs, io::Write, path::PathBuf};

use uuid::Uuid;

use crate::{Error, Result, SyncContext, SyncTarget};

/// Exchanges immutable batches for one target and device.
///
/// Implementations exclude this device's batches from listings. Read keys must be
/// stable and unique within the target, but need not equal publication names
/// (a remote service may use file ids). Never reuse a key for different bytes.
/// Return [`Error::AuthRequired`] when credentials need user intervention; other
/// failures are retryable issues. The engine performs no network I/O itself.
pub trait Transport {
    /// Stores an immutable batch. Publishing the same batch twice is harmless.
    fn publish(&mut self, name: &str, bytes: &[u8]) -> Result<()>;
    /// Returns opaque read keys for batches published by other devices.
    fn list(&mut self) -> Result<Vec<String>>;
    /// Returns the immutable bytes associated with a listed read key.
    fn read(&mut self, key: &str) -> Result<Vec<u8>>;
}

#[derive(Debug)]
pub struct Folder {
    root: PathBuf,
    device: String,
}

impl Folder {
    /// Builds a folder transport, or returns `None` for a remote target.
    pub fn for_context(context: &SyncContext) -> Option<Self> {
        match context.target() {
            SyncTarget::Folder(root) => Some(Self {
                root: root.clone(),
                device: context.device().into(),
            }),
            SyncTarget::Remote { .. } => None,
        }
    }
}

impl Transport for Folder {
    fn publish(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        let directory = self.root.join(&self.device);
        fs::create_dir_all(&directory)?;
        let target = directory.join(name);
        let temporary = directory.join(format!("{}.tmp", Uuid::now_v7()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &target)?;
            sync_directory(&directory)?;
            sync_directory(&self.root)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn list(&mut self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || entry.file_name() == self.device.as_str() {
                continue;
            }
            let device = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Sync("non-UTF-8 device directory".into()))?;
            for file in fs::read_dir(entry.path())? {
                let file = file?;
                if file.path().extension().is_some_and(|ext| ext == "json") {
                    let name = file
                        .file_name()
                        .into_string()
                        .map_err(|_| Error::Sync("non-UTF-8 batch name".into()))?;
                    names.push(format!("{device}/{name}"));
                }
            }
        }
        Ok(names)
    }

    fn read(&mut self, key: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.root.join(key))?)
    }
}

fn sync_directory(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
