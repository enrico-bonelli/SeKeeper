use crate::storage::{self, SaveOutcome};
use sekeeper_core::i18n::tr;
pub use sekeeper_core::limits::{
    MAX_HISTORY_BYTES, MAX_LINEAGE, MAX_SNAPSHOT_BYTES, MAX_VERSIONS, RETAINED_VERSIONS,
    fingerprint, hex, valid_id,
};
use std::io;
use std::path::{Path, PathBuf};

pub fn directory(vault_path: &Path) -> PathBuf {
    storage::sibling(vault_path, ".history-v3")
}

pub fn read_snapshot(path: &Path) -> io::Result<Vec<u8>> {
    storage::read(path, MAX_SNAPSHOT_BYTES)
}

pub fn remember(directory: &Path, id: &str, bytes: &[u8]) -> io::Result<()> {
    if !valid_id(id) || bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::other(tr(
            "invalid vault copy",
            "copia del vault non valida",
        )));
    }
    let path = directory.join(format!("{id}.sek"));
    match storage::write_atomic(&path, bytes, true) {
        Ok(SaveOutcome::Durable) => Ok(()),
        Ok(SaveOutcome::DurabilityUncertain(error) | SaveOutcome::CloudCopyPending(error)) => {
            Err(error)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_snapshot(&path)? != bytes {
                return Err(io::Error::other(tr(
                    "two different copies have the same identifier",
                    "due copie diverse hanno lo stesso identificativo",
                )));
            }
            // A previous attempt may have installed this file but failed fsync.
            let directory = storage::PrivateDir::open(directory, false)?;
            directory.sync_file(path.file_name().expect("snapshot"))?;
            directory.sync()
        }
        Err(error) => Err(error),
    }
}

pub fn load(directory: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let directory = match storage::PrivateDir::open(directory, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut result = Vec::new();
    let mut total = 0usize;
    for name in directory.entries()? {
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".sek")) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let bytes = directory.read(&name, MAX_SNAPSHOT_BYTES)?;
        total += bytes.len();
        if result.len() >= MAX_VERSIONS || total > MAX_HISTORY_BYTES {
            return Err(io::Error::other(tr(
                "history too large: archive some copies before syncing",
                "storico troppo grande: archivia alcune copie prima di sincronizzare",
            )));
        }
        result.push((id.to_string(), bytes));
    }
    Ok(result)
}

/// Removes the copies that `authentic` rejects. Downloads are cached before
/// they can be authenticated, so a tampered or damaged object must not stay
/// on disk, poisoning every later sync even after S3 is repaired.
pub fn evict(directory: &Path, authentic: impl Fn(&str, &[u8]) -> bool) -> io::Result<usize> {
    let directory = match storage::PrivateDir::open(directory, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for name in directory.entries()? {
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".sek")) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let keep = directory
            .read(&name, MAX_SNAPSHOT_BYTES)
            .is_ok_and(|bytes| authentic(id, &bytes));
        if !keep {
            directory.remove(&name)?;
            removed += 1;
        }
    }
    if removed > 0 {
        directory.sync()?;
    }
    Ok(removed)
}

pub fn prune(directory: &Path, obsolete: &std::collections::BTreeSet<String>) -> io::Result<()> {
    let directory = match storage::PrivateDir::open(directory, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut removed = false;
    for name in directory.entries()? {
        if let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".sek"))
            && valid_id(id)
            && obsolete.contains(id)
        {
            directory.remove(&name)?;
            removed = true;
        }
    }
    if removed { directory.sync() } else { Ok(()) }
}
