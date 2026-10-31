use super::*;
use crate::keyfile::KeyFile;
use crate::model::Credential;
use crate::storage::{FailureStage, inject_failure};
use std::fs;
use std::os::unix::fs::PermissionsExt;

const PASSWORD: &str = "laguna cedro nuvola marmo sentiero";

struct TestVault(PathBuf);
impl TestVault {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("sekeeper-test-{}-{}", std::process::id(), new_id()));
        fs::create_dir(&directory).unwrap();
        Self(directory.join("vault"))
    }
    fn create(&self) -> Vault {
        let (vault, outcome) = Vault::create(&self.0, PASSWORD, &KeyFile::test_key()).unwrap();
        durable(outcome);
        vault
    }
    fn open(&self) -> Vault {
        Vault::open(&self.0, PASSWORD, &KeyFile::test_key()).unwrap()
    }
}
impl Drop for TestVault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn durable(outcome: SaveOutcome) {
    assert!(matches!(outcome, SaveOutcome::Durable));
}
fn credential(name: &str) -> Credential {
    Credential {
        id: String::new(),
        description: name.into(),
        username: "utente-é".into(),
        password: "public-test-secret".into(),
        address: "https://example.test".into(),
        notes: "nota\nsu due righe 🔑".into(),
        created_at: None,
        updated_at: None,
    }
}
fn json(vault: &Vault) -> serde_json::Value {
    serde_json::to_value(&vault.data).unwrap()
}

#[test]
fn legacy_v1_is_rejected_without_migration_or_kdf_work() {
    let test = TestVault::new();
    durable(
        storage::write_atomic(
            &test.0,
            include_bytes!("../../tests/fixtures/vault-v1"),
            true,
        )
        .unwrap(),
    );
    assert!(matches!(
        Vault::open(&test.0, "a", &KeyFile::test_key()),
        Err(VaultError::InvalidFormat)
    ));
}

#[test]
fn password_only_v2_is_rejected_without_fallback() {
    let test = TestVault::new();
    assert!(matches!(
        Vault::from_encrypted(
            &test.0,
            include_bytes!("../../tests/fixtures/vault-v2").to_vec(),
            "public fixture master password",
            &KeyFile::test_key()
        ),
        Err(VaultError::InvalidFormat)
    ));
}

#[test]
fn neither_factor_alone_opens_the_vault_and_wrong_key_cannot_restore_it() {
    let test = TestVault::new();
    let key_path = test.0.with_file_name("factor.key");
    durable(KeyFile::generate(&key_path).unwrap());
    let key = KeyFile::read(&key_path).unwrap();
    let (mut vault, outcome) = Vault::create(&test.0, PASSWORD, &key).unwrap();
    durable(outcome);
    durable(vault.add_credential(credential("segreto")).unwrap());
    let original = vault.encrypted().to_vec();
    let password_only = {
        let _guard = KDF_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut output = Zeroizing::new([0u8; 32]);
        let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, Some(32)).unwrap();
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(PASSWORD.as_bytes(), &vault.salt, output.as_mut_slice())
            .unwrap();
        output
    };
    assert!(
        Aes256Gcm::new_from_slice(password_only.as_slice())
            .unwrap()
            .decrypt(
                Nonce::from_slice(&original[NONCE_START..HEADER_LEN]),
                Payload {
                    msg: &original[HEADER_LEN..],
                    aad: &original[..HEADER_LEN]
                }
            )
            .is_err()
    );
    assert!(matches!(
        Vault::open(&test.0, PASSWORD, &KeyFile::test_key()),
        Err(VaultError::InvalidPasswordOrCorrupted)
    ));
    assert!(matches!(
        Vault::open(&test.0, "wrong password", &key),
        Err(VaultError::InvalidPasswordOrCorrupted)
    ));
    assert!(Vault::restore_backup(&test.0, PASSWORD, 1, &KeyFile::test_key()).is_err());
    assert_eq!(fs::read(&test.0).unwrap(), original);
    assert!(!storage::sibling(&test.0, ".v3.before-restore").exists());
    fs::rename(&key_path, key_path.with_file_name("moved.key")).unwrap();
    let moved = KeyFile::read(&key_path.with_file_name("moved.key")).unwrap();
    assert_eq!(
        json(&vault),
        json(&Vault::open(&test.0, PASSWORD, &moved).unwrap())
    );
}

#[test]
fn v3_fixture_uses_both_factors_and_the_strong_profile_and_opens() {
    let test = TestVault::new();
    let bytes = include_bytes!("../../tests/fixtures/vault-v3");
    assert_eq!(&bytes[..12], b"SEKEEPER\x03\x02\x13\x01");
    assert_eq!(
        u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        256 * 1024
    );
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 4);
    durable(storage::write_atomic(&test.0, bytes, true).unwrap());
    let mut vault = Vault::open(
        &test.0,
        "public fixture master password",
        &KeyFile::test_key(),
    )
    .unwrap();
    assert_eq!(vault.data.credentials[0].password, "public-test-secret");
    assert_eq!(vault.data.credentials[0].created_at, None);
    assert_eq!(vault.data.credentials[0].updated_at, None);
    let mut edited = vault.data.credentials[0].clone();
    edited.notes.push_str(" modificata");
    durable(vault.update_credential(edited).unwrap());
    assert_eq!(vault.data.credentials[0].created_at, None);
    assert!(vault.data.credentials[0].updated_at.is_some());
    assert_eq!(
        json(&vault),
        json(
            &Vault::open(
                &test.0,
                "public fixture master password",
                &KeyFile::test_key()
            )
            .unwrap()
        )
    );
}

#[test]
fn v3_requires_header_authentication_and_rejects_weakened_or_excessive_costs() {
    let test = TestVault::new();
    let vault = test.create();
    let original = vault.encrypted();
    let cipher = Aes256Gcm::new_from_slice(vault.key.as_slice()).unwrap();
    assert!(
        cipher
            .decrypt(
                Nonce::from_slice(&original[NONCE_START..HEADER_LEN]),
                &original[HEADER_LEN..]
            )
            .is_err()
    );
    let nonce = [42; NONCE_LEN];
    let mut forged = header(&vault.salt, &nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), b"{}".as_ref())
        .unwrap();
    forged.extend_from_slice(&ciphertext);
    assert!(matches!(
        vault.decode(&forged),
        Err(VaultError::InvalidPasswordOrCorrupted)
    ));
    for (start, cost) in [
        (12, 19 * 1024u32),
        (12, u32::MAX),
        (16, 2),
        (16, u32::MAX),
        (20, 1),
    ] {
        let mut changed = original.to_vec();
        changed[start..start + 4].copy_from_slice(&cost.to_le_bytes());
        assert!(matches!(
            Vault::from_encrypted(&test.0, changed, PASSWORD, &KeyFile::test_key()),
            Err(VaultError::InvalidFormat)
        ));
    }
}

#[test]
fn crud_survives_reopening_and_keeps_all_fields() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("servizio")).unwrap());
    assert_eq!(vault.data.credentials[0].id.len(), 32);
    let created_at = vault.data.credentials[0].created_at;
    assert!(created_at.is_some());
    assert_eq!(created_at, vault.data.credentials[0].updated_at);
    assert_eq!(json(&vault), json(&test.open()));
    let mut changed = vault.data.credentials[0].clone();
    let id = changed.id.clone();
    changed.password = "updated-public-test-secret".into();
    changed.notes.push_str(" aggiornata");
    changed.created_at = Some(1);
    changed.updated_at = Some(1);
    durable(vault.update_credential(changed).unwrap());
    assert_eq!(vault.data.credentials[0].created_at, created_at);
    assert!(vault.data.credentials[0].updated_at >= created_at);
    assert_eq!(json(&vault), json(&test.open()));
    durable(vault.delete_credential(&id).unwrap());
    assert!(test.open().data.credentials.is_empty());
}

#[test]
fn unchanged_edit_keeps_dates_ciphertext_and_history_but_checks_for_external_changes() {
    let test = TestVault::new();
    let mut vault = test.create();
    let mut incoming = credential("servizio");
    incoming.created_at = Some(1);
    incoming.updated_at = Some(1);
    durable(vault.add_credential(incoming).unwrap());
    assert_ne!(vault.data.credentials[0].created_at, Some(1));
    let before = vault.encrypted().to_vec();
    let data = json(&vault);
    let mut unchanged = vault.data.credentials[0].clone();
    unchanged.created_at = Some(2);
    unchanged.updated_at = Some(2);
    durable(vault.update_credential(unchanged).unwrap());
    assert_eq!(vault.encrypted(), before);
    assert_eq!(json(&vault), data);
    let mut other = test.open();
    durable(other.add_credential(credential("secondo")).unwrap());
    assert!(matches!(
        vault.update_credential(vault.data.credentials[0].clone()),
        Err(VaultError::ChangedOnDisk)
    ));
    assert_eq!(json(&vault), data);
}

#[test]
fn wrong_password_corruption_truncation_and_appended_bytes_are_rejected() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("servizio")).unwrap());
    assert!(matches!(
        Vault::open(&test.0, "wrong", &KeyFile::test_key()),
        Err(VaultError::InvalidPasswordOrCorrupted)
    ));
    let original = fs::read(&test.0).unwrap();
    for offset in [
        0,
        8,
        9,
        10,
        11,
        12,
        16,
        20,
        SALT_START,
        NONCE_START,
        HEADER_LEN,
        original.len() - 1,
    ] {
        let mut bytes = original.clone();
        bytes[offset] ^= 1;
        fs::write(&test.0, bytes).unwrap();
        assert!(
            Vault::open(&test.0, PASSWORD, &KeyFile::test_key()).is_err(),
            "offset {offset}"
        );
    }
    for length in [
        0,
        7,
        8,
        9,
        SALT_START,
        NONCE_START,
        HEADER_LEN + TAG_LEN - 1,
        original.len() - 1,
    ] {
        fs::write(&test.0, &original[..length]).unwrap();
        assert!(
            Vault::open(&test.0, PASSWORD, &KeyFile::test_key()).is_err(),
            "length {length}"
        );
    }
    let mut bytes = original;
    bytes.push(0);
    fs::write(&test.0, bytes).unwrap();
    assert!(Vault::open(&test.0, PASSWORD, &KeyFile::test_key()).is_err());
}

#[test]
fn fresh_nonces_and_salts_and_private_encrypted_files() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("servizio")).unwrap());
    let before = fs::read(&test.0).unwrap();
    durable(vault.save().unwrap());
    let after = fs::read(&test.0).unwrap();
    assert_eq!(&before[..NONCE_START], &after[..NONCE_START]);
    assert_ne!(
        &before[NONCE_START..HEADER_LEN],
        &after[NONCE_START..HEADER_LEN]
    );
    assert_ne!(&before[HEADER_LEN..], &after[HEADER_LEN..]);
    let other = TestVault::new();
    other.create();
    assert_ne!(
        &before[SALT_START..NONCE_START],
        &fs::read(&other.0).unwrap()[SALT_START..NONCE_START]
    );
    assert_eq!(
        fs::metadata(test.0.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for path in [
        &test.0,
        &Vault::backup_path(&test.0, 1),
        &storage::sibling(&test.0, ".lock"),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = fs::read(path).unwrap();
        assert!(!bytes.windows(18).any(|w| w == b"public-test-secret"));
    }
}

#[test]
fn creation_rejects_weak_password_and_never_overwrites_existing_vault() {
    let test = TestVault::new();
    assert!(matches!(
        Vault::create(&test.0, "a", &KeyFile::test_key()),
        Err(VaultError::WeakPassword(_))
    ));
    assert!(!test.0.exists());
    test.create();
    let before = fs::read(&test.0).unwrap();
    assert!(matches!(
        Vault::create(&test.0, PASSWORD, &KeyFile::test_key()),
        Err(VaultError::AlreadyExists)
    ));
    assert_eq!(before, fs::read(&test.0).unwrap());
}

#[test]
fn stale_open_handle_cannot_silently_overwrite_newer_credentials() {
    let test = TestVault::new();
    let mut first = test.create();
    let mut second = test.open();
    durable(first.add_credential(credential("primo")).unwrap());
    let before = fs::read(&test.0).unwrap();
    assert!(matches!(
        second.add_credential(credential("secondo")),
        Err(VaultError::ChangedOnDisk)
    ));
    assert!(second.data.credentials.is_empty());
    assert_eq!(before, fs::read(&test.0).unwrap());
}

#[test]
fn competing_writer_is_rejected_without_modifying_the_file() {
    let test = TestVault::new();
    let mut vault = test.create();
    let before = fs::read(&test.0).unwrap();
    let _lock = storage::lock(&test.0).unwrap();
    assert!(vault.add_credential(credential("secondo")).is_err());
    assert_eq!(before, fs::read(&test.0).unwrap());
    assert!(vault.data.credentials.is_empty());
}

#[test]
fn backup_rotation_retains_previous_valid_snapshots_and_their_dates() {
    let test = TestVault::new();
    let mut vault = test.create();
    let oldest = BACKUP_COUNT + 2;
    for index in 0..oldest {
        durable(
            vault
                .add_credential(credential(&index.to_string()))
                .unwrap(),
        );
    }
    let slots: Vec<usize> = (1..=BACKUP_COUNT).collect();
    assert_eq!(Vault::available_backups(&test.0), slots);
    for slot in slots {
        let backup = Vault::open(
            &Vault::backup_path(&test.0, slot),
            PASSWORD,
            &KeyFile::test_key(),
        )
        .unwrap();
        assert_eq!(backup.data.credentials.len(), oldest - slot);
    }
    let date = |slot| Vault::backup_modified(&test.0, slot).unwrap();
    assert!(date(1) >= date(BACKUP_COUNT));
    let earlier = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::open(Vault::backup_path(&test.0, 1))
        .unwrap()
        .set_modified(earlier)
        .unwrap();
    durable(vault.add_credential(credential("rotazione")).unwrap());
    assert_eq!(date(2), 1_000_000);
    assert_eq!(Vault::backup_modified(&test.0, 99), None);
}

#[test]
fn import_adds_entries_in_one_save_keeps_dates_and_skips_duplicates() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("esistente")).unwrap());
    let backups = Vault::available_backups(&test.0).len();
    let mut dated = credential("con data");
    dated.created_at = Some(1_700_000_000);
    dated.updated_at = Some(1_700_000_100);
    let report = vault
        .import_credentials(vec![credential("esistente"), dated, credential("nuova")])
        .unwrap();
    durable(report.outcome);
    assert_eq!((report.added, report.skipped), (2, 1));
    assert_eq!(Vault::available_backups(&test.0).len(), backups + 1);
    let stored = test.open();
    assert_eq!(stored.data.credentials.len(), 3);
    assert_eq!(stored.data.credentials[1].created_at, Some(1_700_000_000));
    assert_eq!(stored.data.credentials[1].updated_at, Some(1_700_000_100));
    assert!(stored.data.credentials[2].created_at.is_some());
    assert!(!stored.data.credentials[1].id.is_empty());

    let before = vault.encrypted().to_vec();
    let report = vault.import_credentials(vec![credential("nuova")]).unwrap();
    durable(report.outcome);
    assert_eq!((report.added, report.skipped), (0, 1));
    assert_eq!(before, vault.encrypted());

    inject_failure(&test.0, FailureStage::BeforeCommit);
    assert!(
        vault
            .import_credentials(vec![credential("fallita")])
            .is_err()
    );
    assert_eq!(vault.data.credentials.len(), 3);
}

#[test]
fn corrupted_primary_can_be_restored_without_destroying_selected_backup() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("uno")).unwrap());
    durable(vault.add_credential(credential("due")).unwrap());
    let backup = fs::read(Vault::backup_path(&test.0, 1)).unwrap();
    fs::write(&test.0, b"damaged").unwrap();
    let (restored, outcome) =
        Vault::restore_backup(&test.0, PASSWORD, 1, &KeyFile::test_key()).unwrap();
    durable(outcome);
    assert_eq!(restored.data.credentials.len(), 1);
    assert_eq!(json(&restored), json(&test.open()));
    assert_eq!(
        fs::read(storage::sibling(&test.0, ".v3.before-restore")).unwrap(),
        b"damaged"
    );
    assert_eq!(backup, fs::read(Vault::backup_path(&test.0, 1)).unwrap());
}

#[test]
fn unauthenticated_backup_never_replaces_primary() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("uno")).unwrap());
    let before = fs::read(&test.0).unwrap();
    assert!(Vault::restore_backup(&test.0, "wrong", 1, &KeyFile::test_key()).is_err());
    assert_eq!(before, fs::read(&test.0).unwrap());
    fs::write(Vault::backup_path(&test.0, 1), b"corrupted backup").unwrap();
    assert!(Vault::restore_backup(&test.0, PASSWORD, 1, &KeyFile::test_key()).is_err());
    assert_eq!(before, fs::read(&test.0).unwrap());
    assert!(!storage::sibling(&test.0, ".v3.before-restore").exists());
}

#[test]
fn missing_primary_can_be_recovered_from_backup() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("uno")).unwrap());
    durable(vault.add_credential(credential("due")).unwrap());
    fs::remove_file(&test.0).unwrap();
    assert!(matches!(
        Vault::create(&test.0, PASSWORD, &KeyFile::test_key()),
        Err(VaultError::AlreadyExists)
    ));
    let (restored, outcome) =
        Vault::restore_backup(&test.0, PASSWORD, 1, &KeyFile::test_key()).unwrap();
    durable(outcome);
    assert_eq!(restored.data.credentials.len(), 1);
    assert_eq!(json(&restored), json(&test.open()));
}

#[derive(Clone, Copy)]
enum Mutation {
    Add,
    Update,
    Delete,
}
fn mutate(vault: &mut Vault, mutation: Mutation) -> Result<SaveOutcome, VaultError> {
    match mutation {
        Mutation::Add => vault.add_credential(credential("aggiunta")),
        Mutation::Update => {
            let mut changed = vault.data.credentials[0].clone();
            changed.password = "changed-secret".into();
            vault.update_credential(changed)
        }
        Mutation::Delete => {
            let id = vault.data.credentials[0].id.clone();
            vault.delete_credential(&id)
        }
    }
}

#[test]
fn failures_before_commit_roll_back_all_mutations_and_leave_original_file() {
    for mutation in [Mutation::Add, Mutation::Update, Mutation::Delete] {
        let test = TestVault::new();
        let mut vault = test.create();
        durable(vault.add_credential(credential("iniziale")).unwrap());
        let before = json(&vault);
        let bytes = fs::read(&test.0).unwrap();
        inject_failure(&test.0, FailureStage::BeforeCommit);
        assert!(mutate(&mut vault, mutation).is_err());
        assert_eq!(json(&vault), before);
        assert_eq!(bytes, fs::read(&test.0).unwrap());
        assert!(
            !fs::read_dir(test.0.parent().unwrap())
                .unwrap()
                .any(|file| file
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
        );
    }
}

#[test]
fn failures_after_commit_keep_new_memory_and_snapshot_for_all_mutations() {
    for mutation in [Mutation::Add, Mutation::Update, Mutation::Delete] {
        let test = TestVault::new();
        let mut vault = test.create();
        durable(vault.add_credential(credential("iniziale")).unwrap());
        inject_failure(&test.0, FailureStage::AfterCommit);
        assert!(matches!(
            mutate(&mut vault, mutation).unwrap(),
            SaveOutcome::DurabilityUncertain(_)
        ));
        assert_eq!(json(&vault), json(&test.open()));
        durable(vault.add_credential(credential("successiva")).unwrap());
        assert_eq!(json(&vault), json(&test.open()));
    }
}

#[test]
fn backup_failure_prevents_primary_replacement() {
    for stage in [FailureStage::BeforeCommit, FailureStage::AfterCommit] {
        let test = TestVault::new();
        let mut vault = test.create();
        let before = fs::read(&test.0).unwrap();
        inject_failure(&Vault::backup_path(&test.0, 1), stage);
        assert!(vault.add_credential(credential("non salvata")).is_err());
        assert!(vault.data.credentials.is_empty());
        assert_eq!(before, fs::read(&test.0).unwrap());
    }
}

#[test]
fn creation_after_commit_warning_still_returns_usable_vault() {
    let test = TestVault::new();
    inject_failure(&test.0, FailureStage::AfterCommit);
    let (mut vault, outcome) = Vault::create(&test.0, PASSWORD, &KeyFile::test_key()).unwrap();
    assert!(matches!(outcome, SaveOutcome::DurabilityUncertain(_)));
    durable(vault.add_credential(credential("successiva")).unwrap());
    assert_eq!(json(&vault), json(&test.open()));
}

#[test]
fn restore_after_commit_warning_returns_restored_state() {
    let test = TestVault::new();
    let mut vault = test.create();
    durable(vault.add_credential(credential("uno")).unwrap());
    durable(vault.add_credential(credential("due")).unwrap());
    inject_failure(&test.0, FailureStage::AfterCommit);
    let (restored, outcome) =
        Vault::restore_backup(&test.0, PASSWORD, 1, &KeyFile::test_key()).unwrap();
    assert!(matches!(outcome, SaveOutcome::DurabilityUncertain(_)));
    assert_eq!(restored.data.credentials.len(), 1);
    assert_eq!(json(&restored), json(&test.open()));
}
