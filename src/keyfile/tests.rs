use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sekeeper-keyfile-test-{:032x}",
            rand::random::<u128>()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn generate(path: &Path) {
    assert!(matches!(
        KeyFile::generate(path).unwrap(),
        SaveOutcome::Durable
    ));
}

#[test]
fn generated_keys_are_independent_portable_private_and_never_overwrite() {
    let directory = Directory::new();
    // Mounted volume roots need not be 0700. Never change their permissions.
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    let a = directory.path("a.key");
    let b = directory.path("b.key");
    generate(&a);
    generate(&b);
    assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(&a).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&a).unwrap().len(), FILE_LEN as u64);
    let first = KeyFile::read(&a).unwrap();
    assert_ne!(first.secret(), KeyFile::read(&b).unwrap().secret());
    let original = fs::read(&a).unwrap();
    assert_eq!(
        KeyFile::generate(&a).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&a).unwrap(), original);
    fs::rename(&a, directory.path("renamed.key")).unwrap();
    fs::copy(directory.path("renamed.key"), directory.path("backup.key")).unwrap();
    assert_eq!(
        first.secret(),
        KeyFile::read(&directory.path("backup.key"))
            .unwrap()
            .secret()
    );
}

#[test]
fn malformed_truncated_extended_and_corrupted_keyfiles_are_rejected() {
    let directory = Directory::new();
    let path = directory.path("key");
    generate(&path);
    let original = fs::read(&path).unwrap();
    for offset in [0, 8, 39, 40, 71] {
        let mut bytes = original.clone();
        bytes[offset] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(KeyFile::read(&path).is_err());
    }
    for length in [0, 32, FILE_LEN - 1, FILE_LEN + 1, 4096] {
        fs::write(&path, vec![0u8; length]).unwrap();
        assert!(KeyFile::read(&path).is_err());
    }
    assert!(KeyFile::read(&directory.path("absent")).is_err());
}

#[test]
fn unsafe_files_and_paths_are_rejected_without_touching_targets() {
    let directory = Directory::new();
    let path = directory.path("key");
    generate(&path);
    let original = fs::read(&path).unwrap();
    let link = directory.path("link");
    symlink(&path, &link).unwrap();
    assert!(KeyFile::read(&link).is_err());
    assert!(KeyFile::generate(&link).is_err());
    let parent_link = directory.path("linked-directory");
    symlink(&directory.0, &parent_link).unwrap();
    assert!(KeyFile::read(&parent_link.join("key")).is_err());
    assert!(KeyFile::generate(&parent_link.join("new")).is_err());
    let fifo = directory.path("fifo");
    let name =
        std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(fifo.as_os_str())).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(KeyFile::read(&fifo).is_err());
    assert!(KeyFile::read(&directory.0).is_err());
    let hard = directory.path("hard");
    fs::hard_link(&path, &hard).unwrap();
    assert!(KeyFile::read(&path).is_err());
    fs::remove_file(&hard).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(KeyFile::read(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(KeyFile::generate(&directory.path("new")).is_err());
    assert!(KeyFile::read(&path).is_err());
    assert!(!directory.path("new").exists());
}

#[test]
fn generation_failure_before_commit_leaves_no_file_and_after_commit_preserves_key() {
    use crate::storage::{FailureStage, inject_failure};
    let directory = Directory::new();
    let path = directory.path("key");
    inject_failure(&path, FailureStage::BeforeCommit);
    assert!(KeyFile::generate(&path).is_err());
    assert!(!path.exists());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    inject_failure(&path, FailureStage::AfterCommit);
    assert!(matches!(
        KeyFile::generate(&path).unwrap(),
        SaveOutcome::DurabilityUncertain(_)
    ));
    assert!(KeyFile::read(&path).is_ok());
    assert!(KeyFile::generate(&path).is_err());
}
