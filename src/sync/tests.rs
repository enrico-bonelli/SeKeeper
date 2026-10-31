use super::*;
use crate::keyfile::KeyFile;
use crate::{
    model::Credential,
    storage::{self, FailureStage, SaveOutcome},
};
use std::path::PathBuf;

pub(super) const PASSWORD: &str = "laguna cedro nuvola marmo sentiero";
pub(super) struct Workdir(pub(super) PathBuf);
impl Workdir {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sekeeper-sync-test-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub(super) fn vault(&self, name: &str) -> PathBuf {
        self.0.join(name).join("vault")
    }
}
impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(super) fn durable(outcome: SaveOutcome) {
    assert!(matches!(outcome, SaveOutcome::Durable));
}
pub(super) fn add(vault: &mut Vault, name: &str) {
    durable(
        vault
            .add_credential(Credential {
                id: String::new(),
                description: name.into(),
                username: "demo".into(),
                password: "public-test-secret".into(),
                address: String::new(),
                notes: String::new(),
                created_at: None,
                updated_at: None,
            })
            .unwrap(),
    );
}
pub(super) fn seed(work: &Workdir) -> (Vault, Vault) {
    let path_a = work.vault("a");
    let path_b = work.vault("b");
    let (mut a, outcome) = Vault::create(&path_a, PASSWORD, &KeyFile::test_key()).unwrap();
    durable(outcome);
    durable(a.enable_sync("PC A").unwrap());
    let mut b = Vault::from_encrypted(
        &path_b,
        a.encrypted().to_vec(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    durable(b.recover_remote().unwrap());
    durable(b.enable_sync("PC B").unwrap());
    (a, b)
}
#[derive(Default)]
pub(super) struct MemoryS3 {
    pub(super) objects: BTreeMap<String, Vec<u8>>,
    fail: bool,
    reads: usize,
    pub(super) fail_put: bool,
    pub(super) delete_budget: Option<usize>,
    pub(super) corrupt_get: Option<String>,
    pub(super) vanish_on_get: Option<String>,
    pub(super) inject_on_delete: Option<(String, Vec<u8>)>,
    pub(super) events: Vec<String>,
}
impl RemoteStore for MemoryS3 {
    fn list(&mut self) -> Result<Vec<String>> {
        if self.fail {
            Err(error("offline"))
        } else {
            Ok(self.objects.keys().cloned().collect())
        }
    }
    fn get(&mut self, id: &str) -> Result<Vec<u8>> {
        self.reads += 1;
        self.events.push(format!("get:{id}"));
        if self.vanish_on_get.as_deref() == Some(id) {
            self.vanish_on_get = None;
            self.objects.remove(id);
            return Err(error("removed concurrently"));
        }
        let mut bytes = self
            .objects
            .get(id)
            .cloned()
            .ok_or_else(|| error("not found"))?;
        if self.corrupt_get.as_deref() == Some(id) {
            bytes.push(0);
        }
        Ok(bytes)
    }
    fn put(&mut self, id: &str, bytes: &[u8]) -> Result<()> {
        self.events.push(format!("put:{id}"));
        if self.fail || self.fail_put {
            return Err(error("offline"));
        }
        if let Some(existing) = self.objects.get(id)
            && existing != bytes
        {
            return Err(error("412 collision"));
        }
        self.objects.insert(id.to_string(), bytes.to_vec());
        Ok(())
    }
    fn delete(&mut self, id: &str) -> Result<()> {
        self.events.push(format!("delete:{id}"));
        if let Some((id, bytes)) = self.inject_on_delete.take() {
            self.objects.insert(id, bytes);
        }
        if self.fail {
            return Err(error("offline"));
        }
        if let Some(remaining) = &mut self.delete_budget {
            if *remaining == 0 {
                return Err(error("403 access denied"));
            }
            *remaining -= 1;
        }
        self.objects.remove(id);
        Ok(())
    }
}
pub(super) fn send(vault: &Vault, path: &Path, remote: &mut MemoryS3, cache: &Path) -> Catalog {
    let downloaded = download(remote, cache).unwrap();
    let catalog = Catalog::build(vault, path, downloaded).unwrap();
    publish(remote, cache, &catalog.copies()).unwrap();
    catalog
}

#[test]
fn two_devices_fast_forward_without_timestamps_and_new_pc_recovers() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    let mut remote = MemoryS3::default();
    let cache_a = work.0.join("cache-a");
    let cache_b = work.0.join("cache-b");
    send(&a, &work.vault("a"), &mut remote, &cache_a);
    add(&mut a, "nuova password");
    send(&a, &work.vault("a"), &mut remote, &cache_a);
    let catalog = send(&b, &work.vault("b"), &mut remote, &cache_b);
    let local = b.data.revision.as_ref().unwrap().id.clone();
    let Decision::FastForward(id) = catalog.decision(&local) else {
        panic!("fast forward expected");
    };
    durable(
        b.install_remote(catalog.snapshots[&id].bytes.clone())
            .unwrap(),
    );
    assert_eq!(b.data.credentials[0].description, "nuova password");
    assert_eq!(
        b.data.credentials[0].created_at,
        a.data.credentials[0].created_at
    );
    assert_eq!(
        b.data.credentials[0].updated_at,
        a.data.credentials[0].updated_at
    );
    let remote_copies = download(&mut remote, &work.0.join("cache-c")).unwrap();
    let other_key_path = work.0.join("other.key");
    durable(KeyFile::generate(&other_key_path).unwrap());
    let other_key = KeyFile::read(&other_key_path).unwrap();
    assert!(
        Vault::from_encrypted(
            &work.vault("c"),
            remote_copies[0].1.clone(),
            PASSWORD,
            &other_key
        )
        .is_err()
    );
    assert!(!work.vault("c").exists());
    let seed = Vault::from_encrypted(
        &work.vault("c"),
        remote_copies[0].1.clone(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    let catalog = Catalog::build(&seed, &work.vault("c"), remote_copies).unwrap();
    let mut c = Vault::from_encrypted(
        &work.vault("c"),
        catalog.snapshots[&catalog.heads[0]].bytes.clone(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    durable(c.recover_remote().unwrap());
    assert_eq!(c.data.credentials[0].description, "nuova password");
    assert_eq!(a.encrypted(), c.encrypted());
}

#[test]
fn offline_branches_preserve_both_and_explicit_resolution_descends_from_both() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    let mut remote = MemoryS3::default();
    let cache_a = work.0.join("cache-a");
    let cache_b = work.0.join("cache-b");
    add(&mut a, "solo A");
    add(&mut a, "seconda modifica offline A");
    add(&mut b, "solo B");
    send(&a, &work.vault("a"), &mut remote, &cache_a);
    let catalog = send(&b, &work.vault("b"), &mut remote, &cache_b);
    assert!(matches!(
        catalog.decision(&b.data.revision.as_ref().unwrap().id),
        Decision::Conflict
    ));
    assert_eq!(catalog.heads.len(), 2);
    assert_eq!(b.data.credentials[0].description, "solo B");
    let a_id = a.data.revision.as_ref().unwrap().id.clone();
    let b_id = b.data.revision.as_ref().unwrap().id.clone();
    durable(
        b.resolve_remote(&catalog.snapshots[&a_id].bytes, catalog.heads.clone())
            .unwrap(),
    );
    assert_eq!(b.data.credentials.len(), 2);
    assert_eq!(b.data.revision.as_ref().unwrap().parents.len(), 2);
    let resolved = send(&b, &work.vault("b"), &mut remote, &cache_b);
    assert_eq!(resolved.heads.len(), 1);
    assert!(remote.objects.contains_key(&a_id));
    assert!(remote.objects.contains_key(&b_id));
    let catalog = send(&a, &work.vault("a"), &mut remote, &cache_a);
    assert!(matches!(catalog.decision(&a_id), Decision::FastForward(_)));
}

#[test]
fn outage_keeps_local_passwords_and_all_offline_ancestors_for_retry() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    let cache = work.0.join("cache");
    let mut remote = MemoryS3 {
        fail: true,
        ..MemoryS3::default()
    };
    for name in ["uno", "due", "tre", "quattro", "cinque"] {
        add(&mut a, name);
    }
    let bytes = a.encrypted().to_vec();
    assert!(download(&mut remote, &cache).is_err());
    assert_eq!(bytes, std::fs::read(work.vault("a")).unwrap());
    assert_eq!(
        history::load(&history::directory(&work.vault("a")))
            .unwrap()
            .len(),
        6
    );
    remote.fail = false;
    let catalog = send(&a, &work.vault("a"), &mut remote, &cache);
    assert_eq!(catalog.heads.len(), 1);
    assert_eq!(remote.objects.len(), 6);
}

#[test]
fn invalid_or_foreign_remote_is_rejected_before_publishing_local_data() {
    let work = Workdir::new();
    let (a, _) = seed(&work);
    let (mut foreign, outcome) =
        Vault::create(&work.vault("foreign"), PASSWORD, &KeyFile::test_key()).unwrap();
    durable(outcome);
    durable(foreign.enable_sync("Altro vault").unwrap());
    let copies = vec![(
        foreign.data.revision.as_ref().unwrap().id.clone(),
        foreign.encrypted().to_vec(),
    )];
    assert!(Catalog::build(&a, &work.vault("a"), copies).is_err());
    let mut bytes = a.encrypted().to_vec();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(
        Catalog::build(
            &a,
            &work.vault("a"),
            vec![(a.data.revision.as_ref().unwrap().id.clone(), bytes)]
        )
        .is_err()
    );
    assert!(
        Catalog::build(
            &a,
            &work.vault("a"),
            vec![("0".repeat(32), a.encrypted().to_vec())]
        )
        .is_err()
    );
}

#[test]
fn remote_cache_is_incremental_and_retries_do_not_replace_existing_objects() {
    let work = Workdir::new();
    let (mut a, b) = seed(&work);
    let mut remote = MemoryS3::default();
    let cache = work.0.join("cache");
    add(&mut a, "uno");
    send(&a, &work.vault("a"), &mut remote, &cache);
    let other_cache = work.0.join("other-cache");
    let initial = download(&mut remote, &other_cache).unwrap();
    let reads = remote.reads;
    let again = download(&mut remote, &other_cache).unwrap();
    assert_eq!(reads, remote.reads);
    assert_eq!(initial.len(), again.len());
    let before = remote.objects.clone();
    let catalog = Catalog::build(&b, &work.vault("b"), again).unwrap();
    publish(&mut remote, &other_cache, &catalog.copies()).unwrap();
    assert_eq!(before, remote.objects);
}

#[test]
fn interrupted_revision_save_restores_metadata_and_credentials() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    let before = a.encrypted().to_vec();
    let revision = a.data.revision.as_ref().unwrap().id.clone();
    storage::inject_failure(&work.vault("a"), FailureStage::BeforeCommit);
    assert!(a.add_credential(Credential::default()).is_err());
    assert_eq!(a.data.revision.as_ref().unwrap().id, revision);
    assert!(a.data.credentials.is_empty());
    assert_eq!(before, std::fs::read(work.vault("a")).unwrap());
    storage::inject_failure(&work.vault("a"), FailureStage::AfterCommit);
    assert!(matches!(
        a.add_credential(Credential::default()).unwrap(),
        SaveOutcome::DurabilityUncertain(_)
    ));
    let reopened = Vault::open(&work.vault("a"), PASSWORD, &KeyFile::test_key()).unwrap();
    assert_eq!(
        a.data.revision.as_ref().unwrap().id,
        reopened.data.revision.as_ref().unwrap().id
    );
    assert_eq!(
        history::load(&history::directory(&work.vault("a")))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn history_failure_after_local_commit_does_not_undo_a_saved_password() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    // Remove the history directory and occupy its path with a file.
    std::fs::remove_dir_all(history::directory(&work.vault("a"))).unwrap();
    std::fs::write(history::directory(&work.vault("a")), b"blocked").unwrap();
    let outcome = a.add_credential(Credential::default()).unwrap();
    assert!(matches!(outcome, SaveOutcome::CloudCopyPending(_)));
    assert_eq!(
        Vault::open(&work.vault("a"), PASSWORD, &KeyFile::test_key())
            .unwrap()
            .data
            .credentials
            .len(),
        1
    );
    assert_eq!(a.data.credentials.len(), 1);
}

#[test]
fn remote_import_failure_preserves_current_data_and_recovery_never_overwrites() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    add(&mut a, "nuova");
    let before = b.encrypted().to_vec();
    storage::inject_failure(&work.vault("b"), FailureStage::BeforeCommit);
    assert!(b.install_remote(a.encrypted().to_vec()).is_err());
    assert!(b.data.credentials.is_empty());
    assert_eq!(before, b.encrypted());
    let mut attempted = Vault::from_encrypted(
        &work.vault("b"),
        a.encrypted().to_vec(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    assert!(attempted.recover_remote().is_err());
    assert_eq!(before, std::fs::read(work.vault("b")).unwrap());
}

#[test]
fn backup_restore_creates_a_branch_instead_of_silently_disappearing_on_next_sync() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    add(&mut a, "uno");
    add(&mut a, "due");
    let (restored, outcome) =
        Vault::restore_backup(&work.vault("a"), PASSWORD, 1, &KeyFile::test_key()).unwrap();
    durable(outcome);
    assert_eq!(restored.data.credentials.len(), 1);
    let catalog = Catalog::build(&restored, &work.vault("a"), Vec::new()).unwrap();
    assert_eq!(catalog.heads.len(), 2);
    assert!(matches!(
        catalog.decision(&restored.data.revision.as_ref().unwrap().id),
        Decision::Conflict
    ));
}

#[test]
fn recovering_a_conflict_is_one_atomic_creation_and_failed_creation_keeps_original_seed() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    add(&mut a, "solo A");
    add(&mut b, "solo B");
    let mut remote = MemoryS3::default();
    let cache = work.0.join("cache");
    send(&a, &work.vault("a"), &mut remote, &cache);
    let catalog = send(&b, &work.vault("b"), &mut remote, &cache);
    let path = work.vault("recovery");
    let mut recovered = Vault::from_encrypted(
        &path,
        a.encrypted().to_vec(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    let seed_revision = recovered.data.revision.as_ref().unwrap().id.clone();
    storage::inject_failure(&path, FailureStage::BeforeCommit);
    assert!(recovered.recover_resolving(catalog.heads.clone()).is_err());
    assert!(!path.exists());
    assert_eq!(recovered.data.revision.as_ref().unwrap().id, seed_revision);
    durable(recovered.recover_resolving(catalog.heads.clone()).unwrap());
    assert_eq!(recovered.data.credentials[0].description, "solo A");
    let new_catalog = Catalog::build(&recovered, &path, catalog.copies()).unwrap();
    assert_eq!(new_catalog.heads.len(), 1);
}
