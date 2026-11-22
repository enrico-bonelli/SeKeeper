use crate::storage::{self, SaveOutcome};
use sekeeper_core::keyfile::FILE_LEN;
use std::io;
use std::path::Path;
use zeroize::Zeroizing;

/// The key file on disk. Its format lives in `sekeeper-core`; this wrapper only
/// adds the private, link-free file handling of the desktop application.
pub struct KeyFile(sekeeper_core::keyfile::KeyFile);

impl KeyFile {
    pub fn generate(path: &Path) -> io::Result<SaveOutcome> {
        let bytes = sekeeper_core::keyfile::KeyFile::new_file().map_err(io::Error::other)?;
        storage::create_keyfile(path, bytes.as_slice())
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = Self::read_file(path)?;
        sekeeper_core::keyfile::KeyFile::parse(bytes.as_slice())
            .map(Self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    /// The verified content of the file, for the transfer to the phone.
    pub fn read_file(path: &Path) -> io::Result<Zeroizing<[u8; FILE_LEN]>> {
        let mut bytes = Zeroizing::new([0u8; FILE_LEN]);
        storage::read_keyfile(path, bytes.as_mut_slice())?;
        sekeeper_core::keyfile::KeyFile::parse(bytes.as_slice())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        Ok(bytes)
    }

    #[cfg(test)]
    pub fn secret(&self) -> &[u8; 32] {
        self.0.secret()
    }

    pub fn core(&self) -> &sekeeper_core::keyfile::KeyFile {
        &self.0
    }

    #[cfg(test)]
    pub fn test_key() -> Self {
        Self(sekeeper_core::keyfile::KeyFile::test_key())
    }
}

#[cfg(test)]
mod tests;
