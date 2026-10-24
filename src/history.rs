use crate::storage::{self, SaveOutcome};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_VERSIONS: usize = 2048;

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn fingerprint(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn directory(vault_path: &Path) -> PathBuf {
    storage::sibling(vault_path, ".history-v3")
}

pub fn read_snapshot(path: &Path) -> io::Result<Vec<u8>> {
    storage::read(path, MAX_SNAPSHOT_BYTES)
}

pub fn remember(directory: &Path, id: &str, bytes: &[u8]) -> io::Result<()> {
    if !valid_id(id) || bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::other("copia del vault non valida"));
    }
    let path = directory.join(format!("{id}.sek"));
    match storage::write_atomic(&path, bytes, true) {
        Ok(SaveOutcome::Durable) => Ok(()),
        Ok(SaveOutcome::DurabilityUncertain(error) | SaveOutcome::CloudCopyPending(error)) => {
            Err(error)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_snapshot(&path)? != bytes {
                return Err(io::Error::other(
                    "due copie diverse hanno lo stesso identificativo",
                ));
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
            return Err(io::Error::other(
                "storico troppo grande: archivia alcune copie prima di sincronizzare",
            ));
        }
        result.push((id.to_string(), bytes));
    }
    Ok(result)
}
