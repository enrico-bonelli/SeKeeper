use crate::history;
use crate::model::Revision;
use crate::s3::{CloudError, RemoteStore, Result, error};
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
}
pub enum Decision {
    Current,
    FastForward(String),
    Conflict,
}

pub fn download(remote: &mut impl RemoteStore, cache: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let ids = remote.list()?;
    if ids.len() > history::MAX_VERSIONS {
        return Err(error("troppe copie S3"));
    }
    let cached = history::load(cache)?;
    let mut total: usize = cached.iter().map(|(_, bytes)| bytes.len()).sum();
    let cached_ids: BTreeSet<&str> = cached.iter().map(|(id, _)| id.as_str()).collect();
    for id in ids {
        if !history::valid_id(&id) {
            return Err(error("identificativo S3 non valido"));
        }
        if !cached_ids.contains(id.as_str()) {
            let bytes = remote.get(&id)?;
            total += bytes.len();
            if total > history::MAX_HISTORY_BYTES {
                return Err(error(
                    "storico S3 troppo grande: nessun vault locale è stato sostituito",
                ));
            }
            history::remember(cache, &id, &bytes)?;
        }
    }
    history::load(cache).map_err(Into::into)
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
        copies.extend(history::load(&history::directory(vault_path))?);
        if let Some(revision) = &vault.data.revision {
            copies.push((revision.id.clone(), vault.encrypted().to_vec()));
        }
        let mut snapshots: BTreeMap<String, Snapshot> = BTreeMap::new();
        let mut size = 0usize;
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
            let revision = vault.read_revision(&bytes).map_err(|_| error(
                "una copia S3 è danneggiata o appartiene a un altro vault: verifica il prefisso scelto; nessuna copia locale è stata sostituita"))?;
            if revision.id != id {
                return Err(error(
                    "identificativo S3 non corrispondente al contenuto autenticato",
                ));
            }
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
        let mut referenced = BTreeSet::new();
        for snapshot in snapshots.values() {
            for parent in &snapshot.revision.parents {
                if !snapshots.contains_key(parent) {
                    return Err(error(
                        "storico incompleto: manca una versione precedente; nessun vault è stato sostituito",
                    ));
                }
                referenced.insert(parent.clone());
            }
        }
        // Kahn's algorithm validates even a cycle hidden behind an otherwise valid head.
        let mut pending: BTreeMap<String, BTreeSet<String>> = snapshots
            .iter()
            .map(|(id, s)| (id.clone(), s.revision.parents.iter().cloned().collect()))
            .collect();
        let mut ready: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.is_empty())
            .map(|(id, _)| id.clone())
            .collect();
        let mut visited = 0usize;
        while let Some(id) = ready.pop() {
            visited += 1;
            pending.remove(&id);
            for (child, parents) in &mut pending {
                if parents.remove(&id) && parents.is_empty() {
                    ready.push(child.clone());
                }
            }
        }
        if visited != snapshots.len() {
            return Err(error("relazioni tra versioni non valide"));
        }
        let heads = snapshots
            .keys()
            .filter(|id| !referenced.contains(*id))
            .cloned()
            .collect();
        Ok(Self { snapshots, heads })
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
                && let Some(snapshot) = self.snapshots.get(&id)
            {
                todo.extend(snapshot.revision.parents.iter().cloned());
            }
        }
        false
    }
    pub fn copies(&self) -> Vec<(String, Vec<u8>)> {
        self.snapshots
            .values()
            .map(|s| (s.id.clone(), s.bytes.clone()))
            .collect()
    }
}

impl From<crate::vault::VaultError> for CloudError {
    fn from(value: crate::vault::VaultError) -> Self {
        CloudError::Message(value.to_string())
    }
}

#[cfg(test)]
mod tests;
