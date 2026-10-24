use crate::history;
use crate::keyfile::KeyFile;
use crate::model::{Revision, VaultData};
use crate::security::validate_new_password;
use crate::storage::{self, SaveOutcome};
use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use rand::RngCore;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"SEKEEPER";
const VERSION: u8 = 3;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const SALT_START: usize = 24;
const NONCE_START: usize = SALT_START + SALT_LEN;
const HEADER_LEN: usize = NONCE_START + NONCE_LEN;
const TAG_LEN: usize = 16;
pub const BACKUP_COUNT: usize = 3;
const MEMORY_KIB: u32 = 256 * 1024;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 4;

fn header(salt: &[u8; SALT_LEN], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&[VERSION, 2, 0x13, 1]); // Argon2id, Argon2 v19, AES-256-GCM
    for parameter in [MEMORY_KIB, ITERATIONS, PARALLELISM] {
        header.extend_from_slice(&parameter.to_le_bytes());
    }
    header.extend_from_slice(salt);
    header.extend_from_slice(nonce);
    header
}

fn validate_header(encrypted: &[u8]) -> Result<(), VaultError> {
    if encrypted.len() < HEADER_LEN + TAG_LEN
        || encrypted.len() as u64 > history::MAX_SNAPSHOT_BYTES
        || encrypted[..SALT_START] != header(&[0; SALT_LEN], &[0; NONCE_LEN])[..SALT_START]
    {
        return Err(VaultError::InvalidFormat);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("impossibile determinare la directory home dell'utente")]
    HomeNotFound,
    #[error("password o file chiave errati, oppure vault danneggiato")]
    InvalidPasswordOrCorrupted,
    #[error("formato del vault non valido o non supportato: serve un vault v3 con file chiave")]
    InvalidFormat,
    #[error("il vault o una sua copia di backup esiste già")]
    AlreadyExists,
    #[error("{0}")]
    WeakPassword(&'static str),
    #[error("il vault è cambiato su disco: bloccalo e riaprilo prima di modificarlo")]
    ChangedOnDisk,
    #[error("il vault non esiste")]
    NotFound,
    #[error("errore durante la derivazione della chiave")]
    KeyDerivation,
    #[error("errore durante la cifratura del vault")]
    Encryption,
    #[error("dati del vault non validi")]
    InvalidData,
    #[error("errore di I/O: {0}")]
    Io(#[from] io::Error),
}

pub struct Vault {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    salt: [u8; SALT_LEN],
    previous_ciphertext: Option<Vec<u8>>,
    device: String,
    pub data: VaultData,
}

impl Vault {
    pub fn default_path() -> Result<PathBuf, VaultError> {
        let home = std::env::var_os("HOME").ok_or(VaultError::HomeNotFound)?;
        Ok(PathBuf::from(home).join(".config/sekeeper/vault"))
    }

    pub fn create(
        path: &Path,
        password: &str,
        keyfile: &KeyFile,
    ) -> Result<(Self, SaveOutcome), VaultError> {
        validate_new_password(password).map_err(VaultError::WeakPassword)?;
        let _lock = storage::lock(path)?;
        if storage::exists(path)? || !Self::available_backups(path).is_empty() {
            return Err(VaultError::AlreadyExists);
        }
        let salt = random_bytes::<SALT_LEN>();
        let key = derive_key(password, &salt, keyfile)?;
        let mut vault = Self {
            path: path.to_path_buf(),
            key,
            salt,
            previous_ciphertext: None,
            device: "Questo PC".into(),
            data: VaultData::default(),
        };
        let outcome = vault.save_locked()?;
        Ok((vault, outcome))
    }

    pub fn open(path: &Path, password: &str, keyfile: &KeyFile) -> Result<Self, VaultError> {
        let encrypted = history::read_snapshot(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VaultError::NotFound
            } else {
                VaultError::Io(error)
            }
        })?;
        Self::from_encrypted(path, encrypted, password, keyfile)
    }

    pub fn from_encrypted(
        path: &Path,
        encrypted: Vec<u8>,
        password: &str,
        keyfile: &KeyFile,
    ) -> Result<Self, VaultError> {
        validate_header(&encrypted)?;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&encrypted[SALT_START..NONCE_START]);
        let key = derive_key(password, &salt, keyfile)?;
        let mut vault = Self {
            path: path.to_path_buf(),
            key,
            salt,
            previous_ciphertext: None,
            device: "Questo PC".into(),
            data: VaultData::default(),
        };
        vault.data = vault.decode(&encrypted)?;
        vault.previous_ciphertext = Some(encrypted);
        Ok(vault)
    }

    pub fn add_credential(
        &mut self,
        mut credential: crate::model::Credential,
    ) -> Result<SaveOutcome, VaultError> {
        credential.id = new_id();
        let now = timestamp();
        credential.created_at = Some(now);
        credential.updated_at = Some(now);
        self.data.credentials.push(credential);
        match self.save() {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.data.credentials.pop();
                Err(error)
            }
        }
    }

    pub fn update_credential(
        &mut self,
        mut credential: crate::model::Credential,
    ) -> Result<SaveOutcome, VaultError> {
        let index = self
            .data
            .credentials
            .iter()
            .position(|item| item.id == credential.id)
            .ok_or(VaultError::InvalidData)?;
        let existing = &self.data.credentials[index];
        if credential.same_content(existing) {
            let _lock = storage::lock(&self.path)?;
            self.ensure_current()?;
            return Ok(SaveOutcome::Durable);
        }
        // Dates come from the vault, never from editable form fields. Missing
        // historical creation dates remain unknown rather than being invented.
        credential.created_at = existing.created_at;
        credential.updated_at = Some(
            timestamp()
                .max(existing.updated_at.unwrap_or(0))
                .max(existing.created_at.unwrap_or(0)),
        );
        let previous = std::mem::replace(&mut self.data.credentials[index], credential);
        match self.save() {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.data.credentials[index] = previous;
                Err(error)
            }
        }
    }

    pub fn delete_credential(&mut self, id: &str) -> Result<SaveOutcome, VaultError> {
        let index = self
            .data
            .credentials
            .iter()
            .position(|item| item.id == id)
            .ok_or(VaultError::InvalidData)?;
        let credential = self.data.credentials.remove(index);
        match self.save() {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.data.credentials.insert(index, credential);
                Err(error)
            }
        }
    }

    fn save(&mut self) -> Result<SaveOutcome, VaultError> {
        let previous_revision = self.data.revision.clone();
        self.advance_revision(None);
        let result = (|| {
            let _lock = storage::lock(&self.path)?;
            self.save_locked()
        })();
        if result.is_err() {
            self.data.revision = previous_revision;
        }
        result
    }

    fn save_locked(&mut self) -> Result<SaveOutcome, VaultError> {
        let output = self.encode()?;
        self.commit(output)
    }

    fn encode(&self) -> Result<Vec<u8>, VaultError> {
        let cipher =
            Aes256Gcm::new_from_slice(self.key.as_slice()).map_err(|_| VaultError::Encryption)?;
        let nonce = random_bytes::<NONCE_LEN>();
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&self.data).map_err(|_| VaultError::InvalidData)?);
        if plaintext.len() as u64 + (HEADER_LEN + TAG_LEN) as u64 > history::MAX_SNAPSHOT_BYTES {
            return Err(VaultError::InvalidData);
        }
        let mut output = header(&self.salt, &nonce);
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_slice(),
                    aad: &output,
                },
            )
            .map_err(|_| VaultError::Encryption)?;
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    fn decode(&self, encrypted: &[u8]) -> Result<VaultData, VaultError> {
        validate_header(encrypted)?;
        if encrypted[SALT_START..NONCE_START] != self.salt {
            return Err(VaultError::InvalidFormat);
        }
        let cipher =
            Aes256Gcm::new_from_slice(self.key.as_slice()).map_err(|_| VaultError::Encryption)?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&encrypted[NONCE_START..HEADER_LEN]),
                    Payload {
                        msg: &encrypted[HEADER_LEN..],
                        aad: &encrypted[..HEADER_LEN],
                    },
                )
                .map_err(|_| VaultError::InvalidPasswordOrCorrupted)?,
        );
        let data: VaultData =
            serde_json::from_slice(&plaintext).map_err(|_| VaultError::InvalidData)?;
        if let Some(revision) = &data.revision
            && (revision.vault_id != self.vault_id()
                || !history::valid_id(&revision.id)
                || revision.parents.len() > 128
                || revision.device.len() > 128
                || revision
                    .parents
                    .iter()
                    .any(|id| !history::valid_id(id) || id == &revision.id))
        {
            return Err(VaultError::InvalidData);
        }
        Ok(data)
    }

    fn commit(&mut self, output: Vec<u8>) -> Result<SaveOutcome, VaultError> {
        self.ensure_current()?;
        if let Some(previous) = &self.previous_ciphertext {
            self.backup(previous)?;
        }
        let outcome =
            storage::write_atomic(&self.path, &output, self.previous_ciphertext.is_none())?;
        // Even if directory fsync failed, replacement succeeded. Never roll back
        // memory or keep the old concurrency snapshot after this point.
        self.previous_ciphertext = Some(output);
        Ok(self.remember(outcome))
    }

    fn ensure_current(&self) -> Result<(), VaultError> {
        if let Some(previous) = &self.previous_ciphertext
            && history::read_snapshot(&self.path)? != *previous
        {
            return Err(VaultError::ChangedOnDisk);
        }
        Ok(())
    }

    fn remember(&self, outcome: SaveOutcome) -> SaveOutcome {
        if let Some(revision) = &self.data.revision
            && let Err(error) = history::remember(
                &history::directory(&self.path),
                &revision.id,
                self.encrypted(),
            )
            && matches!(outcome, SaveOutcome::Durable)
        {
            return SaveOutcome::CloudCopyPending(error);
        }
        outcome
    }

    fn vault_id(&self) -> String {
        self.salt.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn advance_revision(&mut self, parents: Option<Vec<String>>) {
        if let Some(previous) = &self.data.revision {
            self.data.revision = Some(Revision {
                vault_id: previous.vault_id.clone(),
                id: new_id(),
                parents: parents.unwrap_or_else(|| vec![previous.id.clone()]),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                device: self.device.clone(),
            });
        }
    }

    pub fn encrypted(&self) -> &[u8] {
        self.previous_ciphertext.as_deref().expect("vault cifrato")
    }

    pub fn enable_sync(&mut self, device: &str) -> Result<SaveOutcome, VaultError> {
        self.device = device.chars().take(64).collect();
        if self.data.revision.is_some() {
            return Ok(self.remember(SaveOutcome::Durable));
        }
        self.data.revision = Some(Revision {
            vault_id: self.vault_id(),
            id: new_id(),
            parents: Vec::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            device: self.device.clone(),
        });
        let result = (|| {
            let _lock = storage::lock(&self.path)?;
            self.save_locked()
        })();
        if result.is_err() {
            self.data.revision = None;
        }
        result
    }

    pub fn read_revision(&self, bytes: &[u8]) -> Result<Revision, VaultError> {
        self.decode(bytes)?
            .revision
            .clone()
            .ok_or(VaultError::InvalidData)
    }

    pub fn install_remote(&mut self, bytes: Vec<u8>) -> Result<SaveOutcome, VaultError> {
        let data = self.decode(&bytes)?;
        if data.revision.is_none() {
            return Err(VaultError::InvalidData);
        }
        let _lock = storage::lock(&self.path)?;
        let previous = std::mem::replace(&mut self.data, data);
        let result = self.commit(bytes);
        if result.is_err() {
            self.data = previous;
        }
        result
    }

    pub fn resolve_remote(
        &mut self,
        bytes: &[u8],
        mut parents: Vec<String>,
    ) -> Result<SaveOutcome, VaultError> {
        if let Some(current) = &self.data.revision {
            parents.push(current.id.clone());
        }
        parents.sort();
        parents.dedup();
        if parents.len() > 128 || parents.iter().any(|id| !history::valid_id(id)) {
            return Err(VaultError::InvalidData);
        }
        let data = self.decode(bytes)?;
        if data.revision.is_none() {
            return Err(VaultError::InvalidData);
        }
        let _lock = storage::lock(&self.path)?;
        let previous = std::mem::replace(&mut self.data, data);
        self.advance_revision(Some(parents));
        let result = self.save_locked();
        if result.is_err() {
            self.data = previous;
        }
        result
    }

    pub fn recover_remote(&mut self) -> Result<SaveOutcome, VaultError> {
        let _lock = storage::lock(&self.path)?;
        if storage::exists(&self.path)? || !Self::available_backups(&self.path).is_empty() {
            return Err(VaultError::AlreadyExists);
        }
        let outcome = storage::write_atomic(&self.path, self.encrypted(), true)?;
        Ok(self.remember(outcome))
    }

    pub fn recover_resolving(&mut self, parents: Vec<String>) -> Result<SaveOutcome, VaultError> {
        if self.data.revision.is_none()
            || parents.len() > 128
            || parents.iter().any(|id| !history::valid_id(id))
        {
            return Err(VaultError::InvalidData);
        }
        let previous = self.data.revision.clone();
        self.advance_revision(Some(parents));
        let encoded = match self.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.data.revision = previous;
                return Err(error);
            }
        };
        let original = self.previous_ciphertext.replace(encoded);
        let result = self.recover_remote();
        if result.is_err() {
            self.previous_ciphertext = original;
            self.data.revision = previous;
        }
        result
    }

    pub fn backup_path(path: &Path, slot: usize) -> PathBuf {
        storage::sibling(path, &format!(".v3.bak.{slot}"))
    }

    pub fn available_backups(path: &Path) -> Vec<usize> {
        (1..=BACKUP_COUNT)
            .filter(|slot| storage::exists(&Self::backup_path(path, *slot)).unwrap_or(true))
            .collect()
    }

    fn backup(&self, previous: &[u8]) -> Result<(), VaultError> {
        // Copy from oldest to newest: a failed rotation leaves the primary file
        // untouched and never destroys the newest valid backup first.
        for slot in (2..=BACKUP_COUNT).rev() {
            let source = Self::backup_path(&self.path, slot - 1);
            match history::read_snapshot(&source) {
                Ok(bytes) => require_durable(storage::write_atomic(
                    &Self::backup_path(&self.path, slot),
                    &bytes,
                    false,
                )?)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        require_durable(storage::write_atomic(
            &Self::backup_path(&self.path, 1),
            previous,
            false,
        )?)
    }

    pub fn restore_backup(
        path: &Path,
        password: &str,
        slot: usize,
        keyfile: &KeyFile,
    ) -> Result<(Self, SaveOutcome), VaultError> {
        if !(1..=BACKUP_COUNT).contains(&slot) {
            return Err(VaultError::InvalidData);
        }
        let _lock = storage::lock(path)?;
        // Authenticate and decode the selected backup before touching the primary.
        let mut vault = Self::open(&Self::backup_path(path, slot), password, keyfile)?;
        match history::read_snapshot(path) {
            Ok(bytes) => require_durable(storage::write_atomic(
                &storage::sibling(path, ".v3.before-restore"),
                &bytes,
                false,
            )?)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        vault.advance_revision(None);
        let bytes = if vault.data.revision.is_some() {
            vault.encode()?
        } else {
            vault.encrypted().to_vec()
        };
        let outcome = storage::write_atomic(path, &bytes, false)?;
        vault.path = path.to_path_buf();
        vault.previous_ciphertext = Some(bytes);
        let outcome = vault.remember(outcome);
        Ok((vault, outcome))
    }
}

fn require_durable(outcome: SaveOutcome) -> Result<(), VaultError> {
    match outcome {
        SaveOutcome::Durable => Ok(()),
        SaveOutcome::DurabilityUncertain(error) | SaveOutcome::CloudCopyPending(error) => {
            Err(error.into())
        }
    }
}

fn derive_key(
    password: &str,
    salt: &[u8],
    keyfile: &KeyFile,
) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let mut key = Zeroizing::new([0u8; 32]);
    // Tests use the real production profile. Serialize their KDF allocations to
    // keep a parallel test runner from allocating many GiB at once.
    #[cfg(test)]
    let _guard = KDF_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, Some(32))
        .map_err(|_| VaultError::KeyDerivation)?;
    let mut memory = Zeroizing::new(Vec::new());
    memory
        .try_reserve_exact(params.block_count())
        .map_err(|_| VaultError::KeyDerivation)?;
    memory.resize(params.block_count(), Block::default());
    // RFC 9106 secret input K: both independent factors enter the KDF itself.
    // Neither a password verifier nor a key-file verifier is stored in the vault.
    Argon2::new_with_secret(
        keyfile.secret(),
        Algorithm::Argon2id,
        Version::V0x13,
        params,
    )
    .map_err(|_| VaultError::KeyDerivation)?
    .hash_password_into_with_memory(
        password.as_bytes(),
        salt,
        key.as_mut_slice(),
        memory.as_mut_slice(),
    )
    .map_err(|_| VaultError::KeyDerivation)?;
    Ok(key)
}

fn new_id() -> String {
    random_bytes::<16>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
static KDF_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests;
