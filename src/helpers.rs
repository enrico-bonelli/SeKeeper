use crate::security;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::Command;

pub enum Program {
    Curl,
    SecretTool,
}

pub struct TrustedCommand {
    command: Command,
    // Pin the exact executable until spawn has completed execve, avoiding a
    // check-then-execute race on its original pathname.
    _executable: Option<File>,
}
impl Deref for TrustedCommand {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.command
    }
}
impl DerefMut for TrustedCommand {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.command
    }
}

fn reject() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "eseguibile di sistema non sicuro: richiesto un binario ELF del proprietario di /, senza link, permessi di scrittura per altri o privilegi speciali",
    )
}

fn executable(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(reject());
    }
    let mut file = File::open("/")?;
    let system_uid = file.metadata()?.uid();
    let parts: Vec<_> = path
        .components()
        .filter(|p| !matches!(p, Component::RootDir))
        .collect();
    if parts.is_empty() {
        return Err(reject());
    }
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err(reject());
        };
        let name = CString::new(name.as_bytes()).map_err(|_| reject())?;
        let directory = index + 1 != parts.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: the parent descriptor remains live; name is one component.
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if metadata.uid() != system_uid
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o6000 != 0
        {
            return Err(reject());
        }
        if !directory && (!metadata.is_file() || metadata.mode() & 0o111 == 0) {
            return Err(reject());
        }
    }
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    if magic != *b"\x7fELF" {
        return Err(reject());
    }
    Ok(file)
}

fn environment(command: &mut Command) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/");
    // Secret Service uses the session bus. No LD_*, GIO_* or CURL_*/TLS/proxy
    // overrides are inherited by the helpers.
    for key in [
        "HOME",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "LANG",
        "LC_ALL",
        "TZ",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    // SAFETY: protect_process only issues async-signal-safe Linux operations.
    unsafe {
        command.pre_exec(security::protect_process);
    }
}

pub fn command(program: Program) -> io::Result<TrustedCommand> {
    let path = match program {
        Program::Curl => "/usr/bin/curl",
        Program::SecretTool => "/usr/bin/secret-tool",
    };
    system_command(Path::new(path))
}

fn system_command(path: &Path) -> io::Result<TrustedCommand> {
    let executable = executable(path)?;
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    environment(&mut command);
    Ok(TrustedCommand {
        command,
        _executable: Some(executable),
    })
}

impl TrustedCommand {
    fn inherit_descriptor(&mut self, fd: RawFd) {
        // Only this child inherits the sealed upload. The parent's FD_CLOEXEC
        // flag is unchanged, so concurrent helpers never inherit it accidentally.
        unsafe {
            self.command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

pub struct SealedUpload {
    file: File,
}
impl SealedUpload {
    pub fn new(bytes: &[u8]) -> io::Result<Self> {
        let name = c"sekeeper-encrypted-upload";
        // SAFETY: static C string and flags supported by Linux memfd_create.
        let fd = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes)?;
        file.rewind()?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    pub fn attach(&self, command: &mut TrustedCommand) {
        command.inherit_descriptor(self.file.as_raw_fd());
        command
            .arg("--upload-file")
            .arg(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
    }
}

#[cfg(test)]
pub fn testing_command(path: &Path) -> TrustedCommand {
    let mut command = Command::new(path);
    environment(&mut command);
    TrustedCommand {
        command,
        _executable: None,
    }
}

#[cfg(test)]
mod tests;
