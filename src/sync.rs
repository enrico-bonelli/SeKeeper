use crate::history;
use crate::keyfile::KeyFile;
use crate::model::{Credential, Revision, RevisionLink};
use crate::s3::{CloudError, RemoteStore, Result, error};
use crate::storage::SaveOutcome;
use crate::vault::Vault;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone)]
pub struct Snapshot {
    pub id: String,
    pub bytes: Vec<u8>,
    pub revision: Revision,
}
pub struct Catalog {
    pub snapshots: BTreeMap<String, Snapshot>,
    pub heads: Vec<String>,
    links: BTreeMap<String, Vec<String>>,
    order: BTreeMap<String, usize>,
}
pub enum Decision {
    Current,
    FastForward(String),
    Conflict,
}

pub fn download(remote: &mut impl RemoteStore, cache: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    // A concurrent client may prune between LIST and GET. Restart from a fresh
    // listing instead of treating an intentional deletion as a corrupt history.
    for attempt in 0..3 {
        let ids: BTreeSet<String> = remote.list()?.into_iter().collect();
        if ids.len() > history::MAX_VERSIONS {
            return Err(error("troppe copie S3"));
        }
        let mut copies = Vec::new();
        let mut total = 0usize;
        let mut retry = false;
        for id in &ids {
            if !history::valid_id(id) {
                return Err(error("identificativo S3 non valido"));
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
                        return Err(error("storico S3 troppo grande"));
                    }
                    history::remember(cache, id, &bytes)?;
                    bytes
                }
                Err(e) => return Err(e.into()),
            };
            total += bytes.len();
            if total > history::MAX_HISTORY_BYTES {
                return Err(error("storico S3 troppo grande"));
            }
            copies.push((id.clone(), bytes));
        }
        if !retry {
            return Ok(copies);
        }
    }
    Err(error(
        "lo storico S3 sta cambiando: riprova la sincronizzazione",
    ))
}

pub fn publish(
    remote: &mut impl RemoteStore,
    cache: &Path,
    copies: &[(String, Vec<u8>)],
) -> Result<()> {
    let existing: BTreeSet<String> = remote.list()?.into_iter().collect();
    for (id, bytes) in copies {
        if !history::valid_id(id) {
            return Err(error("identificativo della copia non valido"));
        }
        if !existing.contains(id) {
            history::remember(cache, id, bytes)?;
            remote.put(id, bytes)?;
        }
    }
    Ok(())
}

impl Catalog {
    pub fn build(vault: &Vault, vault_path: &Path, remote: Vec<(String, Vec<u8>)>) -> Result<Self> {
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
        let mut snapshots: BTreeMap<String, Snapshot> = BTreeMap::new();
        let mut size = 0usize;
        let mut links = BTreeMap::new();
        for (id, bytes) in copies {
            if let Some(previous) = snapshots.get(&id) {
                if previous.bytes != bytes {
                    return Err(error(
                        "copie diverse hanno lo stesso identificativo; sincronizzazione interrotta",
                    ));
                }
                continue;
            }
            size += bytes.len();
            if snapshots.len() >= history::MAX_VERSIONS || size > history::MAX_HISTORY_BYTES {
                return Err(error(
                    "storico troppo grande: archivia alcune copie prima di sincronizzare",
                ));
            }
            let (revision, ancestry) = vault.read_sync_metadata(&bytes).map_err(|_| error(
                "una copia S3 è danneggiata o appartiene a un altro vault: verifica il prefisso scelto; nessuna copia locale è stata sostituita"))?;
            if revision.id != id {
                return Err(error(
                    "identificativo S3 non corrispondente al contenuto autenticato",
                ));
            }
            for link in ancestry {
                merge_link(&mut links, &link.id, &link.parents)?;
            }
            merge_link(&mut links, &revision.id, &revision.parents)?;
            snapshots.insert(
                id.clone(),
                Snapshot {
                    id,
                    bytes,
                    revision,
                },
            );
        }
        if snapshots.is_empty() {
            return Err(error("nessuna copia versionata disponibile"));
        }
        // The small, authenticated links replace complete historical vaults.
        // Missing links, contradictions, cycles and detached fabricated nodes
        // still fail closed, even when all remaining snapshots decrypt.
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut pending = BTreeMap::new();
        for (id, parents) in &links {
            pending.insert(id.clone(), parents.len());
            for parent in parents {
                if !links.contains_key(parent) {
                    return Err(error(
                        "storico incompleto: manca una versione precedente; nessun vault è stato sostituito",
                    ));
                }
                children.entry(parent.clone()).or_default().push(id.clone());
            }
        }
        let mut ready: Vec<String> = pending
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| id.clone())
            .collect();
        let mut visited = 0usize;
        while let Some(id) = ready.pop() {
            visited += 1;
            if let Some(next) = children.get(&id) {
                for child in next {
                    let count = pending.get_mut(child).expect("nodo verificato");
                    *count -= 1;
                    if *count == 0 {
                        ready.push(child.clone());
                    }
                }
            }
        }
        if visited != links.len() {
            return Err(error("relazioni tra versioni non valide"));
        }
        let heads: Vec<String> = links
            .keys()
            .filter(|id| !children.contains_key(*id))
            .cloned()
            .collect();
        if heads.iter().any(|id| !snapshots.contains_key(id)) {
            return Err(error(
                "storico incompleto: manca il contenuto di una versione corrente",
            ));
        }
        // Reverse topological order: descendants always precede their ancestors,
        // while authenticated dates choose between independent branches. This
        // keeps a fresh short offline branch ahead of an old long branch.
        let mut remaining: BTreeMap<String, usize> = links
            .keys()
            .map(|id| (id.clone(), children.get(id).map_or(0, Vec::len)))
            .collect();
        let date = |id: &String| snapshots.get(id).map_or(0, |s| s.revision.created_at);
        let mut ready: BTreeSet<(u64, String)> =
            heads.iter().map(|id| (date(id), id.clone())).collect();
        let mut order = BTreeMap::new();
        while let Some((_, id)) = ready.pop_last() {
            order.insert(id.clone(), order.len());
            for parent in &links[&id] {
                let count = remaining.get_mut(parent).expect("genitore verificato");
                *count -= 1;
                if *count == 0 {
                    ready.insert((date(parent), parent.clone()));
                }
            }
        }
        Ok(Self {
            snapshots,
            heads,
            links,
            order,
        })
    }

    pub fn decision(&self, local: &str) -> Decision {
        match self.heads.as_slice() {
            [head] if head == local => Decision::Current,
            [head] if self.descends_from(head, local) => Decision::FastForward(head.clone()),
            _ => Decision::Conflict,
        }
    }
    fn descends_from(&self, head: &str, ancestor: &str) -> bool {
        let mut todo = vec![head.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(id) = todo.pop() {
            if id == ancestor {
                return true;
            }
            if seen.insert(id.clone())
                && let Some(parents) = self.links.get(&id)
            {
                todo.extend(parents.iter().cloned());
            }
        }
        false
    }
    pub fn prepare_retention(&mut self, vault: &mut Vault) -> Result<SaveOutcome> {
        if self.heads.len() != 1
            || self.links.len() <= self.snapshots.len().min(history::RETAINED_VERSIONS)
        {
            return Ok(SaveOutcome::Durable);
        }
        let current = vault
            .data
            .revision
            .as_ref()
            .ok_or_else(|| error("vault senza versione"))?;
        if self.heads[0] != current.id {
            return Err(error("aggiorna il vault prima della pulizia"));
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

    pub fn publication(&self, vault: &Vault) -> Result<Publication> {
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
            return Err(error(
                "cronologia cifrata da consolidare prima della pulizia",
            ));
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

    pub fn retained(&mut self, publication: &Publication) {
        self.snapshots
            .retain(|id, _| !publication.obsolete.contains(id));
    }

    pub fn copies(&self) -> Vec<(String, Vec<u8>)> {
        self.snapshots
            .values()
            .map(|s| (s.id.clone(), s.bytes.clone()))
            .collect()
    }
}

fn merge_link(
    links: &mut BTreeMap<String, Vec<String>>,
    id: &str,
    parents: &[String],
) -> Result<()> {
    let mut parents = parents.to_vec();
    parents.sort();
    parents.dedup();
    if let Some(previous) = links.get(id) {
        if *previous != parents {
            return Err(error("collegamenti autenticati in contraddizione"));
        }
    } else {
        if links.len() >= history::MAX_LINEAGE + history::MAX_VERSIONS {
            return Err(error("troppi collegamenti nello storico"));
        }
        links.insert(id.to_string(), parents);
    }
    Ok(())
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
                return Err(error("verifica della copia S3 fallita: pulizia annullata"));
            }
            for id in stale {
                remote.delete(id).map_err(|e| error(&format!("copie sincronizzate, pulizia S3 in attesa: {e}. Verifica s3:DeleteObject sul prefisso del vault")))?;
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
        .ok_or_else(|| error("vault senza versione"))?
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

/// Entries that differ between the local vault and another version, by name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VersionDiff {
    /// Only in the other version.
    pub added: Vec<String>,
    /// Only in the local vault.
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

pub fn compare(local: &[Credential], other: &[Credential]) -> VersionDiff {
    let mut diff = VersionDiff::default();
    for entry in other {
        match local.iter().find(|item| item.id == entry.id) {
            None => diff.added.push(entry.description.clone()),
            Some(item) if !item.same_content(entry) => diff.changed.push(entry.description.clone()),
            Some(_) => {}
        }
    }
    diff.removed = local
        .iter()
        .filter(|item| !other.iter().any(|entry| entry.id == item.id))
        .map(|item| item.description.clone())
        .collect();
    diff
}

/// Authenticates downloaded copies with both factors, before anything is written.
pub fn inspect_recovery(
    vault_path: &Path,
    copies: Vec<(String, Vec<u8>)>,
    password: &str,
    keyfile: &KeyFile,
) -> Result<Catalog> {
    let first = copies
        .first()
        .ok_or_else(|| error("nessuna copia S3 disponibile"))?;
    let seed = Vault::from_encrypted(vault_path, first.1.clone(), password, keyfile)?;
    Catalog::build(&seed, vault_path, copies)
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
        .ok_or_else(|| error("scegli una versione"))?;
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
