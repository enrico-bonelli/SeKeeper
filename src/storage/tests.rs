use super::*;
use std::os::unix::fs::symlink;

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let path =
            std::env::temp_dir().join(format!("sekeeper-storage-{:032x}", rand::random::<u128>()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn saved(outcome: SaveOutcome) {
    assert!(matches!(outcome, SaveOutcome::Durable));
}

#[test]
fn symlink_destinations_and_lock_files_do_not_touch_their_targets() {
    let root = TestDirectory::new();
    let target = root.0.join("external");
    fs::write(&target, b"external data").unwrap();
    let original_mode = fs::metadata(&target).unwrap().mode();
    let vault = root.0.join("vault");
    symlink(&target, &vault).unwrap();
    assert!(read(&vault, 1024).is_err());
    assert!(write_atomic(&vault, b"replacement", false).is_err());
    assert!(exists(&vault).is_err());
    fs::remove_file(&vault).unwrap();
    symlink(&target, sibling(&vault, ".lock")).unwrap();
    assert!(lock(&vault).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"external data");
    assert_eq!(fs::metadata(&target).unwrap().mode(), original_mode);
}

#[test]
fn directory_symlinks_and_parent_traversal_are_rejected() {
    let root = TestDirectory::new();
    let external = TestDirectory::new();
    let link = root.0.join("redirect");
    symlink(&external.0, &link).unwrap();
    assert!(write_atomic(&link.join("vault"), b"data", false).is_err());
    assert!(PrivateDir::open(&link, false).is_err());
    assert!(PrivateDir::open(&root.0.join("../escape"), true).is_err());
    assert!(PrivateDir::open(Path::new("relative"), true).is_err());
    assert!(!external.0.join("vault").exists());
}

#[test]
fn hardlinks_and_shared_permissions_are_rejected_before_read_or_replace() {
    let root = TestDirectory::new();
    let vault = root.0.join("vault");
    saved(write_atomic(&vault, b"original", true).unwrap());
    let alias = root.0.join("alias");
    fs::hard_link(&vault, &alias).unwrap();
    assert!(read(&vault, 100).is_err());
    assert!(write_atomic(&vault, b"new", false).is_err());
    assert_eq!(fs::read(&alias).unwrap(), b"original");
    fs::remove_file(&alias).unwrap();
    fs::set_permissions(&vault, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read(&vault, 100).is_err());
    assert!(write_atomic(&vault, b"new", false).is_err());
    assert_eq!(fs::metadata(&vault).unwrap().mode() & 0o777, 0o644);
}

#[test]
fn non_regular_files_are_rejected_without_blocking() {
    let root = TestDirectory::new();
    let path = root.0.join("fifo");
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(read(&path, 100).is_err());
    assert!(write_atomic(&path, b"data", false).is_err());
    assert!(read(&root.0, 100).is_err());
}

#[test]
fn pinned_directory_does_not_follow_a_replaced_parent() {
    let root = TestDirectory::new();
    let original = root.0.join("original");
    saved(write_atomic(&original.join("vault"), b"original", true).unwrap());
    let directory = PrivateDir::open(&original, false).unwrap();
    let moved = root.0.join("moved");
    fs::rename(&original, &moved).unwrap();
    let external = TestDirectory::new();
    saved(write_atomic(&external.0.join("vault"), b"external", true).unwrap());
    symlink(&external.0, &original).unwrap();
    assert_eq!(
        directory.read(OsStr::new("vault"), 100).unwrap(),
        b"original"
    );
    assert!(read(&original.join("vault"), 100).is_err());
}

#[test]
fn private_regular_files_enforce_limits_and_never_replace_on_create() {
    let root = TestDirectory::new();
    let path = root.0.join("vault");
    saved(write_atomic(&path, b"original", true).unwrap());
    assert!(read(&path, 3).is_err());
    assert_eq!(read(&path, 8).unwrap(), b"original");
    assert_eq!(
        write_atomic(&path, b"new", true).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(read(&path, 100).unwrap(), b"original");
    assert_eq!(
        PrivateDir::open(&root.0, false)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pruning_uses_the_pinned_directory_and_refuses_links_and_other_files() {
    let root = TestDirectory::new();
    let obsolete = "a".repeat(32);
    let retained = "b".repeat(32);
    let directory = root.0.join("history");
    saved(
        write_atomic(
            &directory.join(format!("{obsolete}.sek")),
            b"obsolete",
            true,
        )
        .unwrap(),
    );
    saved(
        write_atomic(
            &directory.join(format!("{retained}.sek")),
            b"retained",
            true,
        )
        .unwrap(),
    );
    saved(write_atomic(&directory.join("unrelated"), b"keep", true).unwrap());
    let ids = [obsolete.clone()].into_iter().collect();
    crate::history::prune(&directory, &ids).unwrap();
    assert!(!directory.join(format!("{obsolete}.sek")).exists());
    assert!(directory.join(format!("{retained}.sek")).exists());
    assert!(directory.join("unrelated").exists());
    let external = root.0.join("external");
    saved(write_atomic(&external, b"external", true).unwrap());
    symlink(&external, directory.join(format!("{obsolete}.sek"))).unwrap();
    assert!(crate::history::prune(&directory, &ids).is_err());
    assert_eq!(fs::read(&external).unwrap(), b"external");
    let pinned = PrivateDir::open(&directory, false).unwrap();
    let moved = root.0.join("moved");
    fs::rename(&directory, &moved).unwrap();
    let other = TestDirectory::new();
    saved(write_atomic(&other.0.join(format!("{retained}.sek")), b"external", true).unwrap());
    symlink(&other.0, &directory).unwrap();
    pinned
        .remove(OsStr::new(&format!("{retained}.sek")))
        .unwrap();
    pinned.sync().unwrap();
    assert!(!moved.join(format!("{retained}.sek")).exists());
    assert_eq!(
        fs::read(other.0.join(format!("{retained}.sek"))).unwrap(),
        b"external"
    );
}
