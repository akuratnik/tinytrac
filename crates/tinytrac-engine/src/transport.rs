//! Immutable batches keep the transport separate from revision merging.

use std::{fs, io::Write, path::PathBuf};

use uuid::Uuid;

use crate::{Error, Result};

pub(crate) trait Transport {
    fn publish(&self, name: &str, bytes: &[u8]) -> Result<()>;
    fn list(&self) -> Result<Vec<String>>;
    fn read(&self, name: &str) -> Result<Vec<u8>>;
}

pub(crate) struct Folder {
    pub root: PathBuf,
    pub device: String,
}

impl Transport for Folder {
    fn publish(&self, name: &str, bytes: &[u8]) -> Result<()> {
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

    fn list(&self) -> Result<Vec<String>> {
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

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.root.join(name))?)
    }
}

fn sync_directory(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
