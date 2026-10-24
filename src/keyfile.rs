use crate::storage::{self, SaveOutcome};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"SEKEY001";
const SECRET_END: usize = 40;
const FILE_LEN: usize = SECRET_END + 32;

// The secret is never serialized into the vault, configuration or S3 objects.
// This type deliberately has no Debug or Serialize implementation.
pub struct KeyFile(Zeroizing<[u8; 32]>);

impl KeyFile {
    pub fn generate(path: &Path) -> io::Result<SaveOutcome> {
        let mut bytes = Zeroizing::new([0u8; FILE_LEN]);
        bytes[..8].copy_from_slice(MAGIC);
        OsRng
            .try_fill_bytes(&mut bytes[8..SECRET_END])
            .map_err(io::Error::other)?;
        let checksum = Zeroizing::new(<[u8; 32]>::from(Sha256::digest(&bytes[..SECRET_END])));
        bytes[SECRET_END..].copy_from_slice(checksum.as_slice());
        storage::create_keyfile(path, bytes.as_slice())
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let mut bytes = Zeroizing::new([0u8; FILE_LEN]);
        storage::read_keyfile(path, bytes.as_mut_slice())?;
        let checksum = Zeroizing::new(<[u8; 32]>::from(Sha256::digest(&bytes[..SECRET_END])));
        if &bytes[..8] != MAGIC || bytes[SECRET_END..] != *checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file chiave non valido o danneggiato: seleziona il file generato da Sekeeper",
            ));
        }
        let mut secret = Zeroizing::new([0u8; 32]);
        secret.copy_from_slice(&bytes[8..SECRET_END]);
        Ok(Self(secret))
    }

    pub fn secret(&self) -> &[u8; 32] {
        &self.0
    }

    #[cfg(test)]
    pub fn test_key() -> Self {
        // Public test value, never used by production creation or unlock paths.
        Self(Zeroizing::new([0x42; 32]))
    }
}

#[cfg(test)]
mod tests;
