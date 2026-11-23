use crate::history;
use crate::keyfile::KeyFile;
use crate::model::{Revision, RevisionLink, VaultData};
use crate::security::validate_new_password;
use crate::storage::{self, SaveOutcome};
use rand::{RngCore, rngs::OsRng};
use sekeeper_core::format::{FormatError, VaultKey};
use sekeeper_core::i18n::tr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub const BACKUP_COUNT: usize = 10;

#[derive(Debug)]
pub enum VaultError {
    HomeNotFound,
    InvalidPasswordOrCorrupted,
    InvalidFormat,
    AlreadyExists,
    WeakPassword(&'static str),
    ChangedOnDisk,
    NotFound,
    KeyDerivation,
    Encryption,
    InvalidData,
    Io(io::Error),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeNotFound => f.write_str(tr(
                "unable to determine the user's home directory",
                "impossibile determinare la directory home dell'utente",
            )),
            Self::InvalidPasswordOrCorrupted => f.write_str(tr(
                "wrong password or key file, or corrupted vault",
                "password o file chiave errati, oppure vault danneggiato",
            )),
            Self::InvalidFormat => f.write_str(tr(
                "invalid or unsupported vault format: a v3 vault with a key file is required",
                "formato del vault non valido o non supportato: serve un vault v3 con file chiave",
            )),
            Self::AlreadyExists => f.write_str(tr(
                "the vault or one of its backup copies already exists",
                "il vault o una sua copia di backup esiste già",
            )),
            Self::WeakPassword(message) => f.write_str(message),
            Self::ChangedOnDisk => f.write_str(tr(
                "the vault changed on disk: lock and reopen it before editing",
                "il vault è cambiato su disco: bloccalo e riaprilo prima di modificarlo",
            )),
            Self::NotFound => f.write_str(tr("the vault does not exist", "il vault non esiste")),
            Self::KeyDerivation => f.write_str(tr(
                "error during key derivation",
                "errore durante la derivazione della chiave",
            )),
            Self::Encryption => f.write_str(tr(
                "error while encrypting the vault",
                "errore durante la cifratura del vault",
            )),
            Self::InvalidData => f.write_str(tr("invalid vault data", "dati del vault non validi")),
            Self::Io(error) => write!(f, "{}: {error}", tr("I/O error", "errore di I/O")),
        }
    }
}

impl std::error::Error for VaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for VaultError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FormatError> for VaultError {
    fn from(error: FormatError) -> Self {
        match error {
            FormatError::InvalidPasswordOrCorrupted => Self::InvalidPasswordOrCorrupted,
            FormatError::InvalidFormat => Self::InvalidFormat,
            FormatError::KeyDerivation => Self::KeyDerivation,
            FormatError::Encryption => Self::Encryption,
            FormatError::InvalidData => Self::InvalidData,
        }
    }
}

pub struct Vault {
    path: PathBuf,
    key: VaultKey,
    previous_ciphertext: Option<Vec<u8>>,
    device: String,
    // Set once this session uses S3. Without it nothing needs the complete
    // copies in the history directory: lineage alone preserves the ancestry.
    sync_active: bool,
    pub data: VaultData,
}

pub struct ImportReport {
    pub added: usize,
    pub skipped: usize,
    pub outcome: SaveOutcome,
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
        let key = VaultKey::generate(password, keyfile.core())?;
        let mut vault = Self {
            path: path.to_path_buf(),
            key,
            previous_ciphertext: None,
            device: tr("This PC", "Questo PC").into(),
            sync_active: false,
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
        let key = VaultKey::for_snapshot(&encrypted, password, keyfile.core())?;
        let mut vault = Self {
            path: path.to_path_buf(),
            key,
            previous_ciphertext: None,
            device: tr("This PC", "Questo PC").into(),
            sync_active: false,
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

    /// Adds all entries in one save, skipping those already in the vault.
    pub fn import_credentials(
        &mut self,
        incoming: Vec<crate::model::Credential>,
    ) -> Result<ImportReport, VaultError> {
        let existing = self.data.credentials.len();
        let total = incoming.len();
        let now = timestamp();
        for mut credential in incoming {
            let known = self
                .data
                .credentials
                .iter()
                .any(|item| item.same_content(&credential));
            if known {
                continue;
            }
            credential.id = new_id();
            credential.created_at = credential.created_at.or(Some(now));
            credential.updated_at = credential.updated_at.or(Some(now));
            self.data.credentials.push(credential);
        }
        let added = self.data.credentials.len() - existing;
        let outcome = if added == 0 {
            SaveOutcome::Durable
        } else {
            match self.save() {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.data.credentials.truncate(existing);
                    return Err(error);
                }
            }
        };
        Ok(ImportReport {
            added,
            skipped: total - added,
            outcome,
        })
    }

    fn save(&mut self) -> Result<SaveOutcome, VaultError> {
        let previous_revision = self.data.revision.clone();
        let previous_lineage = self.data.lineage.clone();
        self.advance_revision(None);
        let result = (|| {
            let _lock = storage::lock(&self.path)?;
            self.save_locked()
        })();
        if result.is_err() {
            self.data.revision = previous_revision;
            self.data.lineage = previous_lineage;
        }
        result
    }

    fn save_locked(&mut self) -> Result<SaveOutcome, VaultError> {
        let output = self.encode()?;
        self.commit(output)
    }

    fn encode(&self) -> Result<Vec<u8>, VaultError> {
        Ok(self.key.seal(&self.data)?)
    }

    fn decode(&self, encrypted: &[u8]) -> Result<VaultData, VaultError> {
        Ok(self.key.open(encrypted)?)
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
        if self.sync_active
            && let Some(revision) = &self.data.revision
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
        self.key.vault_id()
    }

    fn advance_revision(&mut self, parents: Option<Vec<String>>) {
        if let Some(previous) = &self.data.revision {
            self.data.lineage.push(RevisionLink {
                id: previous.id.clone(),
                parents: previous.parents.clone(),
            });
            self.data.revision = Some(Revision {
                vault_id: previous.vault_id.clone(),
                id: new_id(),
                parents: parents.unwrap_or_else(|| vec![previous.id.clone()]),
                created_at: timestamp(),
                device: self.device.clone(),
            });
        }
    }

    /// A copy of the vault key, for a background job that must authenticate
    /// downloads before caching them. It is wiped when dropped.
    pub fn sync_key(&self) -> VaultKey {
        self.key.clone()
    }

    pub fn encrypted(&self) -> &[u8] {
        self.previous_ciphertext
            .as_deref()
            .expect("encrypted vault")
    }

    pub fn enable_sync(&mut self, device: &str) -> Result<SaveOutcome, VaultError> {
        self.device = device.chars().take(64).collect();
        self.sync_active = true;
        if self.data.revision.is_some() {
            return Ok(self.remember(SaveOutcome::Durable));
        }
        self.data.revision = Some(Revision {
            vault_id: self.vault_id(),
            id: new_id(),
            parents: Vec::new(),
            created_at: timestamp(),
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

    /// Decrypts another copy of this vault for comparison, without installing it.
    pub fn read_credentials(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<crate::model::Credential>, VaultError> {
        Ok(std::mem::take(&mut self.decode(bytes)?.credentials))
    }

    pub fn read_sync_metadata(
        &self,
        bytes: &[u8],
    ) -> Result<(Revision, Vec<RevisionLink>), VaultError> {
        let mut data = self.decode(bytes)?;
        let revision = data.revision.take().ok_or(VaultError::InvalidData)?;
        Ok((revision, std::mem::take(&mut data.lineage)))
    }

    // Upgrade existing v3 histories in place before the first deletion. Future
    // saves inherit this metadata and append their immediate predecessor.
    pub fn record_lineage(
        &mut self,
        mut links: Vec<RevisionLink>,
    ) -> Result<SaveOutcome, VaultError> {
        let current = self.data.revision.as_ref().ok_or(VaultError::InvalidData)?;
        links.retain(|link| link.id != current.id);
        links.sort_by(|a, b| a.id.cmp(&b.id));
        let mut previous = self.data.lineage.clone();
        previous.sort_by(|a, b| a.id.cmp(&b.id));
        if previous == links {
            return Ok(SaveOutcome::Durable);
        }
        if links.len() >= history::MAX_LINEAGE {
            return Err(VaultError::InvalidData);
        }
        let previous = std::mem::replace(&mut self.data.lineage, links);
        let result = self.save();
        if result.is_err() {
            self.data.lineage = previous;
        }
        result
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
        self.sync_active = true;
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
        let previous_lineage = self.data.lineage.clone();
        self.advance_revision(Some(parents));
        let encoded = match self.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.data.revision = previous;
                self.data.lineage = previous_lineage;
                return Err(error);
            }
        };
        let original = self.previous_ciphertext.replace(encoded);
        let result = self.recover_remote();
        if result.is_err() {
            self.previous_ciphertext = original;
            self.data.revision = previous;
            self.data.lineage = previous_lineage;
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

    pub fn backup_modified(path: &Path, slot: usize) -> Option<u64> {
        storage::modified(&Self::backup_path(path, slot))
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|age| age.as_secs())
    }

    fn backup(&self, previous: &[u8]) -> Result<(), VaultError> {
        // Rename from oldest to newest: a failed rotation leaves the primary
        // file untouched, and each copy keeps the date it was really written.
        for slot in (2..=BACKUP_COUNT).rev() {
            let source = Self::backup_path(&self.path, slot - 1);
            match storage::rotate(&source, &Self::backup_path(&self.path, slot)) {
                Ok(()) => {}
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

fn new_id() -> String {
    history::hex(&random_bytes::<16>())
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
mod tests;
