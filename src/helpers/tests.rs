use super::*;

#[test]
fn trusted_curl_runs_from_its_verified_descriptor() {
    let output = command(Program::Curl)
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"curl "));
}

#[test]
fn trusted_secret_tool_executes_and_children_inherit_a_zero_core_limit() {
    let output = command(Program::SecretTool)
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    // secret-tool reports its usage on stderr and exits 2 for --help.
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.starts_with(b"usage: secret-tool"));
    let output = system_command(Path::new("/usr/bin/cat"))
        .unwrap()
        .arg("/proc/self/limits")
        .output()
        .unwrap();
    assert!(output.status.success());
    let limits = String::from_utf8(output.stdout).unwrap();
    let core = limits
        .lines()
        .find(|line| line.starts_with("Max core file size"))
        .unwrap();
    let fields: Vec<_> = core.split_whitespace().collect();
    assert_eq!(&fields[4..6], &["0", "0"]);
}

#[test]
fn user_owned_scripts_and_symlinks_are_not_system_programs() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let path = std::env::temp_dir().join(format!(
        "sekeeper-untrusted-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(system_command(&path).is_err());
    std::fs::remove_file(&path).unwrap();
    symlink("/usr/bin/curl", &path).unwrap();
    assert!(system_command(&path).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(system_command(Path::new("curl")).is_err());
}

#[test]
fn helpers_receive_a_small_environment_without_credential_or_loader_overrides() {
    let output = system_command(Path::new("/usr/bin/env"))
        .unwrap()
        .output()
        .unwrap();
    assert!(output.status.success());
    let environment = String::from_utf8(output.stdout).unwrap();
    let allowed = [
        "PATH",
        "HOME",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "LANG",
        "LC_ALL",
        "TZ",
    ];
    for line in environment.lines() {
        let (name, _) = line.split_once('=').unwrap();
        assert!(allowed.contains(&name));
    }
    assert!(environment.lines().any(|line| line == "PATH=/usr/bin:/bin"));
}

#[test]
fn upload_is_sealed_and_only_the_selected_child_inherits_it() {
    let mut upload = SealedUpload::new(b"authenticated ciphertext").unwrap();
    assert!(upload.file.write_all(b"tamper").is_err());
    assert!(upload.file.set_len(0).is_err());
    let fd = upload.file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    let mut cat = system_command(Path::new("/usr/bin/cat")).unwrap();
    cat.inherit_descriptor(fd);
    cat.arg(format!("/proc/self/fd/{fd}"));
    let output = cat.output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"authenticated ciphertext");
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, flags);
}
