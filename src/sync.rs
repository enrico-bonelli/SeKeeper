use crate::history;
use crate::keyfile::KeyFile;
use crate::model::RevisionLink;
use crate::s3::{CloudError, RemoteStore, Result, error};
use crate::storage::SaveOutcome;
use crate::vault::Vault;
use sekeeper_core::format::VaultKey;
use sekeeper_core::i18n::{is_italian, tr};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub use sekeeper_core::catalog::{Catalog, CatalogError, Decision, Snapshot, compare, merge_link};

impl From<CatalogError> for CloudError {
    fn from(value: CatalogError) -> Self {
        CloudError::Message(value.to_string())
    }
}

/// Checks that a downloaded copy belongs to the vault and carries the
/// revision its name announces. Only such copies are ever written to the cache.
pub fn authentic(key: &VaultKey) -> impl Fn(&str, &[u8]) -> bool + '_ {
    move |id, bytes| {
        key.read_sync_metadata(bytes)
            .is_ok_and(|(revision, _)| revision.id == id)
    }
}

/// For a download that cannot authenticate yet, such as a recovery before the
/// password is known: nothing is cached.
pub fn untrusted(_: &str, _: &[u8]) -> bool {
    false
}

/// Fetches the listed copies, reusing the cache. A new download is cached only
/// if `authentic` accepts it: tampered objects stay in memory, fail the
/// catalog check and are fetched again next time, never poisoning the disk.
pub fn download(
    remote: &mut impl RemoteStore,
    cache: &Path,
    authentic: &dyn Fn(&str, &[u8]) -> bool,
) -> Result<Vec<(String, Vec<u8>)>> {
    // A concurrent client may prune between LIST and GET. Restart from a fresh
    // listing instead of treating an intentional deletion as a corrupt history.
    for attempt in 0..3 {
        let ids: BTreeSet<String> = remote.list()?.into_iter().collect();
        if ids.len() > history::MAX_VERSIONS {
            return Err(error(tr("too many S3 copies", "troppe copie S3")));
        }
        let mut copies = Vec::new();
        let mut total = 0usize;
        let mut retry = false;
        for id in &ids {
            if !history::valid_id(id) {
                return Err(error(tr(
                    "invalid S3 identifier",
                    "identificativo S3 non valido",
                )));
            }
            let bytes = match history::read_snapshot(&cache.join(format!("{id}.sek"))) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let bytes = match remote.get(id) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            if attempt < 2 && !remote.list()?.contains(id) {
                                retry = true;
                                break;
                            }
                            return Err(e);
                        }
                    };
                    // Enforce the total limit before remembering network data.
                    if total + bytes.len() > history::MAX_HISTORY_BYTES {
                        return Err(error(tr(
                            "S3 history too large",
                            "storico S3 troppo grande",
                        )));
                    }
                    if authentic(id, &bytes) {
                        history::remember(cache, id, &bytes)?;
                    }
                    bytes
                }
                Err(e) => return Err(e.into()),
            };
            total += bytes.len();
            if total > history::MAX_HISTORY_BYTES {
                return Err(error(tr(
                    "S3 history too large",
                    "storico S3 troppo grande",
                )));
            }
            copies.push((id.clone(), bytes));
        }
        if !retry {
            return Ok(copies);
        }
    }
    Err(error(tr(
        "S3 history is changing: retry synchronization",
        "lo storico S3 sta cambiando: riprova la sincronizzazione",
    )))
}

/// After a failed sync or recovery: drops cached downloads that do not
/// belong to this vault, so the next attempt fetches them again.
pub fn evict_unauthentic(cache: &Path, vault: &Vault) -> std::io::Result<usize> {
    history::evict(cache, |id, bytes| {
        vault
            .read_sync_metadata(bytes)
            .is_ok_and(|(revision, _)| revision.id == id)
    })
}

pub fn publish(
    remote: &mut impl RemoteStore,
    cache: &Path,
    copies: &[(String, Vec<u8>)],
) -> Result<()> {
    let existing: BTreeSet<String> = remote.list()?.into_iter().collect();
    for (id, bytes) in copies {
        if !history::valid_id(id) {
            return Err(error(tr(
                "invalid copy identifier",
                "identificativo della copia non valido",
            )));
        }
        if !existing.contains(id) {
            history::remember(cache, id, bytes)?;
            remote.put(id, bytes)?;
        }
    }
    Ok(())
}

/// The parts of the revision catalog that touch this computer: local history,
/// the lineage checkpoint written into the vault, and the cleanup plan.
pub trait LocalCatalog {
    fn build(vault: &Vault, vault_path: &Path, remote: Vec<(String, Vec<u8>)>) -> Result<Catalog>;
    fn prepare_retention(&mut self, vault: &mut Vault) -> Result<SaveOutcome>;
    fn publication(&self, vault: &Vault) -> Result<Publication>;
    fn retained(&mut self, publication: &Publication);
}

impl LocalCatalog for Catalog {
    fn build(vault: &Vault, vault_path: &Path, remote: Vec<(String, Vec<u8>)>) -> Result<Catalog> {
        let mut copies = remote;
        // Remote copies must all belong to this vault. Local leftovers of a
        // vault that was deleted and recreated are unrelated, not an attack.
        copies.extend(
            history::load(&history::directory(vault_path))?
                .into_iter()
                .filter(|(_, bytes)| vault.read_sync_metadata(bytes).is_ok()),
        );
        if let Some(revision) = &vault.data.revision {
            copies.push((revision.id.clone(), vault.encrypted().to_vec()));
        }
        Ok(Catalog::assemble(copies, |bytes| {
            vault.read_sync_metadata(bytes)
        })?)
    }

    fn prepare_retention(&mut self, vault: &mut Vault) -> Result<SaveOutcome> {
        if self.heads.len() != 1
            || self.links.len() <= self.snapshots.len().min(history::RETAINED_VERSIONS)
        {
            return Ok(SaveOutcome::Durable);
        }
        let current = vault
            .data
            .revision
            .as_ref()
            .ok_or_else(|| error(tr("vault has no version", "vault senza versione")))?;
        if self.heads[0] != current.id {
            return Err(error(tr(
                "update the vault before pruning",
                "aggiorna il vault prima della pulizia",
            )));
        }
        let before = current.id.clone();
        let links = self
            .links
            .iter()
            .map(|(id, parents)| RevisionLink {
                id: id.clone(),
                parents: parents.clone(),
            })
            .collect();
        let outcome = vault.record_lineage(links)?;
        let (revision, _) = vault.read_sync_metadata(vault.encrypted())?;
        if revision.id != before {
            merge_link(&mut self.links, &revision.id, &revision.parents)?;
            let id = revision.id.clone();
            self.snapshots.insert(
                id.clone(),
                Snapshot {
                    id: id.clone(),
                    bytes: vault.encrypted().to_vec(),
                    revision,
                },
            );
            self.heads = vec![id];
            for rank in self.order.values_mut() {
                *rank += 1;
            }
            self.order.insert(self.heads[0].clone(), 0);
        }
        Ok(outcome)
    }

    fn publication(&self, vault: &Vault) -> Result<Publication> {
        // Unresolved branches are all retained until the user chooses a state.
        if self.heads.len() != 1 {
            return Ok(Publication {
                copies: self.copies(),
                obsolete: BTreeSet::new(),
                witness: None,
            });
        }
        let head = &self.heads[0];
        let mut snapshots: Vec<&Snapshot> = self.snapshots.values().collect();
        snapshots.sort_by(|a, b| {
            (b.id == *head)
                .cmp(&(a.id == *head))
                .then_with(|| self.order[&a.id].cmp(&self.order[&b.id]))
                .then_with(|| b.id.cmp(&a.id))
        });
        let keep: BTreeSet<String> = snapshots
            .iter()
            .take(history::RETAINED_VERSIONS)
            .map(|s| s.id.clone())
            .collect();
        let obsolete: BTreeSet<String> = self
            .links
            .keys()
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect();
        if obsolete.is_empty() {
            return Ok(Publication {
                copies: self.copies(),
                obsolete,
                witness: None,
            });
        }
        let snapshot = &self.snapshots[head];
        let (_, ancestry) = vault.read_sync_metadata(&snapshot.bytes)?;
        let proof: BTreeMap<String, Vec<String>> = ancestry
            .iter()
            .map(|link| {
                let mut parents = link.parents.clone();
                parents.sort();
                parents.dedup();
                (link.id.clone(), parents)
            })
            .collect();
        if self
            .links
            .iter()
            .any(|(id, parents)| id != head && proof.get(id) != Some(parents))
        {
            return Err(error(tr(
                "encrypted history must be consolidated before pruning",
                "cronologia cifrata da consolidare prima della pulizia",
            )));
        }
        Ok(Publication {
            copies: snapshots
                .into_iter()
                .filter(|s| keep.contains(&s.id))
                .map(|s| (s.id.clone(), s.bytes.clone()))
                .collect(),
            obsolete,
            witness: Some((head.clone(), snapshot.bytes.clone())),
        })
    }

    fn retained(&mut self, publication: &Publication) {
        self.snapshots
            .retain(|id, _| !publication.obsolete.contains(id));
    }
}

pub struct Publication {
    copies: Vec<(String, Vec<u8>)>,
    obsolete: BTreeSet<String>,
    witness: Option<(String, Vec<u8>)>,
}
impl Publication {
    pub fn send(
        &self,
        remote: &mut impl RemoteStore,
        cache: &Path,
        vault_path: &Path,
    ) -> Result<()> {
        publish(remote, cache, &self.copies)?;
        if let Some((id, bytes)) = &self.witness {
            let existing: BTreeSet<String> = remote.list()?.into_iter().collect();
            let stale: Vec<&String> = self.obsolete.intersection(&existing).collect();
            // Do not remove even one object until the retained checkpoint has
            // actually reached S3 and has been read back byte for byte. With
            // nothing to remove, an idle sync does not need to download it.
            if !stale.is_empty() && remote.get(id)? != *bytes {
                return Err(error(tr(
                    "S3 copy verification failed: pruning cancelled",
                    "verifica della copia S3 fallita: pulizia annullata",
                )));
            }
            for id in stale {
                remote.delete(id).map_err(|e| {
                    CloudError::Message(if is_italian() {
                        format!(
                            "copie sincronizzate, pulizia S3 in attesa: {e}. Verifica s3:DeleteObject sul prefisso del vault"
                        )
                    } else {
                        format!(
                            "copies synced, S3 pruning pending: {e}. Check s3:DeleteObject on the vault prefix"
                        )
                    })
                })?;
            }
            // Local pruning happens only after successful remote publication
            // and deletion; failures remain retryable without changing the vault.
            history::prune(cache, &self.obsolete)?;
            history::prune(&history::directory(vault_path), &self.obsolete)?;
        }
        Ok(())
    }
}

/// What the UI must do after comparing the local vault with the downloaded copies.
pub enum Next {
    /// The vault changed during the download: compare again.
    LocalChanged,
    /// A newer version exists but must wait for the open editor.
    UpdateDeferred,
    /// The lineage checkpoint is saved, but its durability is unconfirmed.
    RetentionPending,
    Publish {
        publication: Publication,
        conflict: bool,
    },
}

pub struct Reconciliation {
    pub installed: Option<SaveOutcome>,
    pub catalog: Option<Catalog>,
    pub next: Result<Next>,
}

/// Local half of a synchronization, between the download and the publication.
/// The order of these steps is what makes the later deletions safe.
pub fn reconcile(
    vault: &mut Vault,
    vault_path: &Path,
    remote: Vec<(String, Vec<u8>)>,
    expected: &str,
    may_install: bool,
) -> Reconciliation {
    let mut result = Reconciliation {
        installed: None,
        catalog: None,
        next: Ok(Next::LocalChanged),
    };
    result.next = reconcile_steps(
        vault,
        vault_path,
        remote,
        expected,
        may_install,
        &mut result,
    );
    result
}

fn reconcile_steps(
    vault: &mut Vault,
    vault_path: &Path,
    remote: Vec<(String, Vec<u8>)>,
    expected: &str,
    may_install: bool,
    result: &mut Reconciliation,
) -> Result<Next> {
    let current = vault
        .data
        .revision
        .as_ref()
        .ok_or_else(|| error(tr("vault has no version", "vault senza versione")))?
        .id
        .clone();
    if current != expected {
        return Ok(Next::LocalChanged);
    }
    let mut catalog = Catalog::build(vault, vault_path, remote)?;
    let decision = catalog.decision(&current);
    let conflict = matches!(decision, Decision::Conflict);
    if let Decision::FastForward(id) = decision {
        if !may_install {
            result.catalog = Some(catalog);
            return Ok(Next::UpdateDeferred);
        }
        result.installed = Some(vault.install_remote(catalog.snapshots[&id].bytes.clone())?);
    }
    if !matches!(catalog.prepare_retention(vault)?, SaveOutcome::Durable) {
        return Ok(Next::RetentionPending);
    }
    let publication = catalog.publication(vault)?;
    result.catalog = Some(catalog);
    Ok(Next::Publish {
        publication,
        conflict,
    })
}

/// Authenticates downloaded copies with both factors, before anything is written.
pub fn inspect_recovery(
    vault_path: &Path,
    cache: &Path,
    copies: Vec<(String, Vec<u8>)>,
    password: &str,
    keyfile: &KeyFile,
) -> Result<Catalog> {
    let first = copies
        .first()
        .ok_or_else(|| error(tr("no S3 copies available", "nessuna copia S3 disponibile")))?;
    let seed = Vault::from_encrypted(vault_path, first.1.clone(), password, keyfile)?;
    let catalog = Catalog::build(&seed, vault_path, copies.clone()).inspect_err(|_| {
        let _ = evict_unauthentic(cache, &seed);
    })?;
    // Every copy is authenticated now: keep them, so a retry or the first
    // sync does not download them again.
    for (id, bytes) in &copies {
        let _ = history::remember(cache, id, bytes);
    }
    Ok(catalog)
}

/// Creates the local vault from the chosen current version.
pub fn complete_recovery(
    vault_path: &Path,
    catalog: Catalog,
    head: usize,
    password: &str,
    keyfile: &KeyFile,
) -> Result<(Vault, SaveOutcome)> {
    let id = catalog
        .heads
        .get(head)
        .ok_or_else(|| error(tr("choose a version", "scegli una versione")))?;
    let bytes = catalog.snapshots[id].bytes.clone();
    let mut vault = Vault::from_encrypted(vault_path, bytes, password, keyfile)?;
    let outcome = if catalog.heads.len() > 1 {
        vault.recover_resolving(catalog.heads)?
    } else {
        vault.recover_remote()?
    };
    Ok((vault, outcome))
}

impl From<crate::vault::VaultError> for CloudError {
    fn from(value: crate::vault::VaultError) -> Self {
        CloudError::Message(value.to_string())
    }
}

#[cfg(test)]
mod retention_tests;
#[cfg(test)]
mod tests;
