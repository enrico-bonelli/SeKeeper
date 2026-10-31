use super::tests::{MemoryS3, PASSWORD, Workdir, add, durable, seed, send};
use super::*;
use crate::{
    keyfile::KeyFile,
    model::RevisionLink,
    storage::{self, FailureStage},
};

fn compact(vault: &mut Vault, path: &Path, remote: &mut MemoryS3, cache: &Path) -> Catalog {
    let copies = download(remote, cache).unwrap();
    let revision = vault.data.revision.as_ref().unwrap().id.clone();
    let step = reconcile(vault, path, copies, &revision, true);
    let Ok(Next::Publish { publication, .. }) = step.next else {
        panic!("publication expected");
    };
    let mut catalog = step.catalog.unwrap();
    publication.send(remote, cache, path).unwrap();
    catalog.retained(&publication);
    catalog
}

#[test]
fn thirty_complete_copies_recovery_old_pc_and_offline_conflicts_survive_pruning() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    let original = b.encrypted().to_vec();
    let old_id = b.data.revision.as_ref().unwrap().id.clone();
    let cache = work.0.join("cache-a");
    let mut remote = MemoryS3::default();
    let mut ids = Vec::new();
    // Simulate histories created before metadata retention was implemented.
    for index in 0..45 {
        a.data.lineage.clear();
        add(&mut a, &format!("entry {index}"));
        ids.push(a.data.revision.as_ref().unwrap().id.clone());
    }
    send(&a, &work.vault("a"), &mut remote, &cache);
    assert_eq!(remote.objects.len(), 46);
    let data = serde_json::to_value(&a.data.credentials).unwrap();
    let catalog = compact(&mut a, &work.vault("a"), &mut remote, &cache);
    assert_eq!(remote.objects.len(), history::RETAINED_VERSIONS);
    assert_eq!(catalog.snapshots.len(), history::RETAINED_VERSIONS);
    assert_eq!(data, serde_json::to_value(&a.data.credentials).unwrap());
    assert_eq!(history::load(&cache).unwrap().len(), 30);
    assert_eq!(
        history::load(&history::directory(&work.vault("a")))
            .unwrap()
            .len(),
        30
    );
    assert!(!remote.objects.contains_key(&old_id));
    assert!(
        ids.iter()
            .rev()
            .take(29)
            .all(|id| remote.objects.contains_key(id))
    );

    // A fresh installation needs only retained payloads, including the encrypted
    // ancestry, and never has to retrieve deleted historical vault contents.
    let copies = download(&mut remote, &work.0.join("new-cache")).unwrap();
    assert_eq!(copies.len(), 30);
    let fresh = Vault::from_encrypted(
        &work.vault("new"),
        copies[0].1.clone(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    let recovered = Catalog::build(&fresh, &work.vault("new"), copies).unwrap();
    let mut fresh = Vault::from_encrypted(
        &work.vault("new"),
        recovered.snapshots[&recovered.heads[0]].bytes.clone(),
        PASSWORD,
        &KeyFile::test_key(),
    )
    .unwrap();
    durable(fresh.recover_remote().unwrap());
    assert_eq!(data, serde_json::to_value(&fresh.data.credentials).unwrap());

    // A stale, unmodified device can fast-forward through metadata-only nodes.
    let b_cache = work.0.join("cache-b");
    let b_catalog = Catalog::build(
        &b,
        &work.vault("b"),
        download(&mut remote, &b_cache).unwrap(),
    )
    .unwrap();
    let Decision::FastForward(id) = b_catalog.decision(&old_id) else {
        panic!("stale PC must fast-forward");
    };
    durable(
        b.install_remote(b_catalog.snapshots[&id].bytes.clone())
            .unwrap(),
    );
    compact(&mut b, &work.vault("b"), &mut remote, &b_cache);
    assert_eq!(remote.objects.len(), 30);
    assert!(!remote.objects.contains_key(&old_id));

    // Editing while offline creates a real conflict, even when the fork's
    // parent has already been pruned. No branch is silently overwritten.
    let path = work.vault("offline");
    let mut offline =
        Vault::from_encrypted(&path, original, PASSWORD, &KeyFile::test_key()).unwrap();
    durable(offline.recover_remote().unwrap());
    add(&mut offline, "offline edit");
    let offline_id = offline.data.revision.as_ref().unwrap().id.clone();
    let offline_cache = work.0.join("offline-cache");
    let mut conflict = Catalog::build(
        &offline,
        &path,
        download(&mut remote, &offline_cache).unwrap(),
    )
    .unwrap();
    assert!(matches!(conflict.decision(&offline_id), Decision::Conflict));
    let before = offline.encrypted().to_vec();
    durable(conflict.prepare_retention(&mut offline).unwrap());
    assert_eq!(before, offline.encrypted());
    let publication = conflict.publication(&offline).unwrap();
    assert!(publication.obsolete.is_empty());
    publication
        .send(&mut remote, &offline_cache, &path)
        .unwrap();
    assert!(remote.objects.contains_key(&offline_id));
    assert!(remote.objects.len() > 30);
    durable(
        offline
            .resolve_remote(
                &conflict.snapshots[&offline_id].bytes,
                conflict.heads.clone(),
            )
            .unwrap(),
    );
    compact(&mut offline, &path, &mut remote, &offline_cache);
    assert_eq!(remote.objects.len(), 30);
    assert!(remote.objects.contains_key(&offline_id));
    assert_eq!(offline.data.credentials[0].description, "offline edit");
    // A subsequent edit does not require a second maintenance checkpoint.
    add(&mut offline, "another edit");
    let id = offline.data.revision.as_ref().unwrap().id.clone();
    compact(&mut offline, &path, &mut remote, &offline_cache);
    assert_eq!(offline.data.revision.as_ref().unwrap().id, id);
    assert_eq!(remote.objects.len(), 30);
}

#[test]
fn upload_verification_and_deletion_failures_are_retryable_without_early_local_pruning() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    let cache = work.0.join("cache");
    let path = work.vault("a");
    let mut remote = MemoryS3::default();
    for index in 0..35 {
        a.data.lineage.clear();
        add(&mut a, &index.to_string());
    }
    let mut catalog = send(&a, &path, &mut remote, &cache);
    durable(catalog.prepare_retention(&mut a).unwrap());
    let publication = catalog.publication(&a).unwrap();
    let head = a.data.revision.as_ref().unwrap().id.clone();
    let local = history::load(&history::directory(&path)).unwrap().len();
    let before = remote.objects.len();
    remote.fail_put = true;
    assert!(publication.send(&mut remote, &cache, &path).is_err());
    assert_eq!(remote.objects.len(), before);
    assert_eq!(
        history::load(&history::directory(&path)).unwrap().len(),
        local
    );
    assert!(
        !remote
            .events
            .iter()
            .any(|event| event.starts_with("delete:"))
    );
    remote.fail_put = false;
    remote.corrupt_get = Some(head.clone());
    assert!(publication.send(&mut remote, &cache, &path).is_err());
    assert!(
        !remote
            .events
            .iter()
            .any(|event| event.starts_with("delete:"))
    );
    remote.corrupt_get = None;
    remote.delete_budget = Some(1);
    assert!(publication.send(&mut remote, &cache, &path).is_err());
    assert_eq!(
        history::load(&history::directory(&path)).unwrap().len(),
        local
    );
    assert!(remote.objects.contains_key(&head));
    remote.delete_budget = None;
    publication.send(&mut remote, &cache, &path).unwrap();
    assert_eq!(remote.objects.len(), 30);
    assert_eq!(history::load(&history::directory(&path)).unwrap().len(), 30);
    let first_delete = remote
        .events
        .iter()
        .position(|event| event.starts_with("delete:"))
        .unwrap();
    assert!(remote.events[..first_delete].contains(&format!("get:{head}")));
    let latest = Vault::open(&path, PASSWORD, &KeyFile::test_key()).unwrap();
    assert_eq!(latest.data.credentials.len(), 35);
    publication.send(&mut remote, &cache, &path).unwrap();
    assert_eq!(remote.objects.len(), 30);
}

#[test]
fn failed_checkpoint_rolls_back_and_committed_warning_defers_pruning() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    for index in 0..31 {
        a.data.lineage.clear();
        add(&mut a, &index.to_string());
    }
    let path = work.vault("a");
    let mut catalog = Catalog::build(&a, &path, Vec::new()).unwrap();
    let before = a.encrypted().to_vec();
    let metadata = serde_json::to_value(&a.data.lineage).unwrap();
    storage::inject_failure(&path, FailureStage::BeforeCommit);
    assert!(catalog.prepare_retention(&mut a).is_err());
    assert_eq!(a.encrypted(), before);
    assert_eq!(serde_json::to_value(&a.data.lineage).unwrap(), metadata);
    assert!(catalog.publication(&a).is_err());
    storage::inject_failure(&path, FailureStage::AfterCommit);
    assert!(matches!(
        catalog.prepare_retention(&mut a).unwrap(),
        SaveOutcome::DurabilityUncertain(_)
    ));
    assert_ne!(a.encrypted(), before);
    let reopened = Vault::open(&path, PASSWORD, &KeyFile::test_key()).unwrap();
    assert_eq!(
        serde_json::to_value(&reopened.data).unwrap(),
        serde_json::to_value(&a.data).unwrap()
    );
}

#[test]
fn download_restarts_when_a_listed_object_is_pruned_and_ignores_obsolete_cache_entries() {
    let work = Workdir::new();
    let cache = work.0.join("cache");
    let ids: Vec<String> = (0..3).map(|n| format!("{n:032x}")).collect();
    let mut remote = MemoryS3::default();
    for id in &ids {
        remote
            .objects
            .insert(id.clone(), b"public test ciphertext".to_vec());
    }
    remote.vanish_on_get = Some(ids[0].clone());
    assert_eq!(download(&mut remote, &cache).unwrap().len(), 2);
    history::remember(&cache, &ids[0], b"old cached ciphertext").unwrap();
    let copies = download(&mut remote, &cache).unwrap();
    assert_eq!(copies.len(), 2);
    assert!(copies.iter().all(|(id, _)| *id != ids[0]));
}

#[test]
fn contradictory_missing_cyclic_and_detached_lineage_is_rejected() {
    for kind in 0..4 {
        let work = Workdir::new();
        let (mut a, _) = seed(&work);
        add(&mut a, "initial");
        let x = "f".repeat(32);
        let y = "e".repeat(32);
        match kind {
            0 => a.data.lineage[0].parents = vec![x],
            1 => {
                a.data.lineage.push(RevisionLink {
                    id: x,
                    parents: vec![y],
                });
            }
            2 => {
                a.data.lineage.push(RevisionLink {
                    id: x.clone(),
                    parents: vec![y.clone()],
                });
                a.data.lineage.push(RevisionLink {
                    id: y,
                    parents: vec![x],
                });
            }
            _ => {
                a.data.lineage.push(RevisionLink {
                    id: x,
                    parents: Vec::new(),
                });
            }
        }
        add(&mut a, "forged metadata");
        assert!(Catalog::build(&a, &work.vault("a"), Vec::new()).is_err());
    }
}

#[test]
fn a_concurrent_new_branch_is_never_deleted_by_an_already_prepared_cleanup() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    let path = work.vault("a");
    let cache = work.0.join("cache");
    let mut remote = MemoryS3::default();
    for index in 0..35 {
        add(&mut a, &index.to_string());
    }
    let mut catalog = send(&a, &path, &mut remote, &cache);
    durable(catalog.prepare_retention(&mut a).unwrap());
    let publication = catalog.publication(&a).unwrap();
    add(&mut b, "concurrent PC edit");
    let id = b.data.revision.as_ref().unwrap().id.clone();
    remote.inject_on_delete = Some((id.clone(), b.encrypted().to_vec()));
    publication.send(&mut remote, &cache, &path).unwrap();
    assert!(remote.objects.contains_key(&id));
    assert_eq!(remote.objects.len(), 31);
    let catalog = Catalog::build(&a, &path, download(&mut remote, &cache).unwrap()).unwrap();
    assert!(matches!(
        catalog.decision(&a.data.revision.as_ref().unwrap().id),
        Decision::Conflict
    ));
    assert!(catalog.heads.contains(&id));
    assert_eq!(b.data.credentials[0].description, "concurrent PC edit");
}

#[test]
fn leftover_history_of_a_deleted_vault_does_not_block_a_new_one() {
    let work = Workdir::new();
    let path = work.vault("a");
    let (mut old, outcome) = Vault::create(&path, PASSWORD, &KeyFile::test_key()).unwrap();
    durable(outcome);
    durable(old.enable_sync("PC").unwrap());
    add(&mut old, "vecchia");
    let foreign = old.encrypted().to_vec();
    drop(old);
    for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "vault"
            || entry.file_name().to_string_lossy().starts_with("vault.v3")
        {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
    let (mut fresh, outcome) = Vault::create(&path, PASSWORD, &KeyFile::test_key()).unwrap();
    durable(outcome);
    durable(fresh.enable_sync("PC").unwrap());
    let catalog = Catalog::build(&fresh, &path, Vec::new()).unwrap();
    assert_eq!(catalog.snapshots.len(), 1);
    // The tolerance is only for local leftovers: S3 still has to be coherent.
    assert!(Catalog::build(&fresh, &path, vec![("0".repeat(32), foreign)]).is_err());
}

#[test]
fn without_sync_no_complete_copies_are_stored_and_ancestry_survives() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    let path = work.vault("a");
    let cache = work.0.join("cache");
    let mut remote = MemoryS3::default();
    compact(&mut a, &path, &mut remote, &cache);
    drop(a);
    // A new session with S3 turned off never calls enable_sync.
    let mut a = Vault::open(&path, PASSWORD, &KeyFile::test_key()).unwrap();
    let history = history::directory(&path);
    let before = history::load(&history).unwrap().len();
    for index in 0..5 {
        add(&mut a, &index.to_string());
    }
    assert_eq!(history::load(&history).unwrap().len(), before);
    durable(a.enable_sync("PC").unwrap());
    assert_eq!(history::load(&history).unwrap().len(), before + 1);
    let catalog = compact(&mut a, &path, &mut remote, &cache);
    assert_eq!(
        catalog.heads,
        vec![a.data.revision.as_ref().unwrap().id.clone()]
    );
    assert_eq!(remote.objects.len(), 2);
}

#[test]
fn an_idle_sync_after_cleanup_downloads_nothing() {
    let work = Workdir::new();
    let (mut a, _) = seed(&work);
    let path = work.vault("a");
    let cache = work.0.join("cache");
    let mut remote = MemoryS3::default();
    for index in 0..35 {
        add(&mut a, &index.to_string());
    }
    compact(&mut a, &path, &mut remote, &cache);
    assert_eq!(remote.objects.len(), 30);
    remote.events.clear();
    compact(&mut a, &path, &mut remote, &cache);
    assert!(remote.events.is_empty(), "{:?}", remote.events);
}

#[test]
fn reconcile_defers_an_update_for_the_editor_then_installs_it() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    let mut remote = MemoryS3::default();
    let cache_a = work.0.join("cache-a");
    let cache_b = work.0.join("cache-b");
    let path_b = work.vault("b");
    send(&b, &path_b, &mut remote, &cache_b);
    add(&mut a, "nuova");
    send(&a, &work.vault("a"), &mut remote, &cache_a);
    let revision = b.data.revision.as_ref().unwrap().id.clone();

    let copies = download(&mut remote, &cache_b).unwrap();
    let step = reconcile(
        &mut b,
        &path_b,
        copies.clone(),
        "f".repeat(32).as_str(),
        true,
    );
    assert!(matches!(step.next, Ok(Next::LocalChanged)));
    assert!(step.installed.is_none());

    let step = reconcile(&mut b, &path_b, copies.clone(), &revision, false);
    assert!(matches!(step.next, Ok(Next::UpdateDeferred)));
    assert!(step.installed.is_none() && step.catalog.is_some());
    assert!(b.data.credentials.is_empty());

    let step = reconcile(&mut b, &path_b, copies, &revision, true);
    durable(step.installed.unwrap());
    assert!(matches!(
        step.next,
        Ok(Next::Publish {
            conflict: false,
            ..
        })
    ));
    assert_eq!(b.data.credentials[0].description, "nuova");
}

#[test]
fn reconcile_reports_conflicts_and_compare_names_the_differences() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    add(&mut a, "comune");
    add(&mut a, "da modificare");
    let mut remote = MemoryS3::default();
    let cache_a = work.0.join("cache-a");
    let cache_b = work.0.join("cache-b");
    send(&a, &work.vault("a"), &mut remote, &cache_a);
    let copies = download(&mut remote, &cache_b).unwrap();
    let revision = b.data.revision.as_ref().unwrap().id.clone();
    durable(
        reconcile(&mut b, &work.vault("b"), copies, &revision, true)
            .installed
            .unwrap(),
    );

    add(&mut a, "solo A");
    let mut changed = a.data.credentials[1].clone();
    changed.password = "nuovo segreto pubblico".into();
    durable(a.update_credential(changed).unwrap());
    add(&mut b, "solo B");
    send(&a, &work.vault("a"), &mut remote, &cache_a);
    let copies = download(&mut remote, &cache_b).unwrap();
    let revision = b.data.revision.as_ref().unwrap().id.clone();
    let step = reconcile(&mut b, &work.vault("b"), copies, &revision, true);
    assert!(matches!(
        step.next,
        Ok(Next::Publish { conflict: true, .. })
    ));
    let other = b.read_credentials(a.encrypted()).unwrap();
    assert_eq!(
        compare(&b.data.credentials, &other),
        VersionDiff {
            added: vec!["solo A".into()],
            removed: vec!["solo B".into()],
            changed: vec!["da modificare".into()],
        }
    );
}

#[test]
fn recovery_requires_both_factors_and_creates_the_chosen_version() {
    let work = Workdir::new();
    let (mut a, mut b) = seed(&work);
    add(&mut a, "solo A");
    add(&mut b, "solo B");
    let mut remote = MemoryS3::default();
    let cache = work.0.join("cache");
    send(&a, &work.vault("a"), &mut remote, &cache);
    send(&b, &work.vault("b"), &mut remote, &cache);
    let copies = download(&mut remote, &work.0.join("cache-new")).unwrap();
    let path = work.vault("new");
    assert!(inspect_recovery(&path, Vec::new(), PASSWORD, &KeyFile::test_key()).is_err());
    assert!(
        inspect_recovery(
            &path,
            copies.clone(),
            "password errata",
            &KeyFile::test_key()
        )
        .is_err()
    );
    let catalog = inspect_recovery(&path, copies, PASSWORD, &KeyFile::test_key()).unwrap();
    assert!(!path.exists());
    assert_eq!(catalog.heads.len(), 2);
    let (vault, outcome) =
        complete_recovery(&path, catalog, 1, PASSWORD, &KeyFile::test_key()).unwrap();
    durable(outcome);
    assert!(path.exists());
    assert_eq!(vault.data.revision.as_ref().unwrap().parents.len(), 2);
    assert_eq!(vault.data.credentials.len(), 1);
}
