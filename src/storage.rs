use rand::{RngCore, rngs::OsRng};
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
#[must_use]
pub enum SaveOutcome {
    Durable,
    // The replacement has committed. Callers must keep the new in-memory state.
    DurabilityUncertain(io::Error),
    CloudCopyPending(io::Error),
}

pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "percorso non sicuro: servono directory private e file regolari dell'utente, senza link o permessi per altri utenti",
    )
}

fn component(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
        return Err(denied());
    }
    CString::new(name.as_bytes()).map_err(|_| denied())
}

fn open_at(directory: &File, name: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let name = component(name)?;
    // SAFETY: live directory descriptor and NUL-terminated single component;
    // from_raw_fd takes ownership only after a successful openat.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub struct PrivateDir {
    file: File,
}
impl PrivateDir {
    pub fn open(path: &Path, create: bool) -> io::Result<Self> {
        Self::open_with_policy(path, create, true)
    }

    // A user-selected key file may live at the root of a mounted USB volume.
    // Do not create or chmod that directory; disallow writes by other users.
    fn open_external(path: &Path) -> io::Result<Self> {
        Self::open_with_policy(path, false, false)
    }

    fn open_with_policy(path: &Path, create: bool, private_leaf: bool) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(denied());
        }
        let mut directory = File::open("/")?;
        // The owner of / is the system owner, including UID mappings in a
        // container. Never infer trust from a user-controlled directory.
        let system_uid = directory.metadata()?.uid();
        let user_uid = unsafe { libc::geteuid() };
        let parts: Vec<_> = path
            .components()
            .filter(|c| !matches!(c, Component::RootDir))
            .collect();
        if parts.is_empty() {
            return Err(denied());
        }
        for (index, part) in parts.iter().enumerate() {
            let Component::Normal(name) = part else {
                return Err(denied());
            };
            let next = match open_at(&directory, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                    let name_c = component(name)?;
                    let result =
                        unsafe { libc::mkdirat(directory.as_raw_fd(), name_c.as_ptr(), 0o700) };
                    if result != 0
                        && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if result == 0 {
                        directory.sync_all()?;
                    }
                    open_at(&directory, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
                }
                Err(error) => return Err(error),
            };
            let metadata = next.metadata()?;
            if index + 1 == parts.len() {
                if metadata.uid() != user_uid {
                    return Err(denied());
                }
                if create {
                    next.set_permissions(fs::Permissions::from_mode(0o700))?;
                } else {
                    let forbidden = if private_leaf { 0o077 } else { 0o022 };
                    if metadata.mode() & forbidden != 0 {
                        return Err(denied());
                    }
                }
            } else {
                if metadata.uid() != user_uid && metadata.uid() != system_uid {
                    return Err(denied());
                }
                let shared_sticky =
                    metadata.uid() == system_uid && metadata.mode() & libc::S_ISVTX != 0;
                if metadata.mode() & 0o022 != 0 && !shared_sticky {
                    return Err(denied());
                }
            }
            directory = next;
        }
        Ok(Self { file: directory })
    }

    fn parent(path: &Path, create: bool) -> io::Result<(Self, &OsStr)> {
        let parent = path.parent().ok_or_else(denied)?;
        let name = path.file_name().ok_or_else(denied)?;
        component(name)?;
        Ok((Self::open(parent, create)?, name))
    }

    fn regular(&self, name: &OsStr, flags: i32) -> io::Result<File> {
        let file = open_at(&self.file, name, flags, 0o600)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(denied());
        }
        Ok(file)
    }

    pub fn read(&self, name: &OsStr, limit: u64) -> io::Result<Vec<u8>> {
        let file = self.regular(name, libc::O_RDONLY)?;
        if file.metadata()?.len() > limit {
            return Err(io::Error::other("file troppo grande"));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::other("file troppo grande"));
        }
        Ok(bytes)
    }

    pub fn entries(&self) -> io::Result<Vec<OsString>> {
        // This is our pinned descriptor, not an untrusted path. Entries are
        // subsequently opened relative to the same descriptor with O_NOFOLLOW.
        fs::read_dir(format!("/proc/self/fd/{}", self.file.as_raw_fd()))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    pub fn sync_file(&self, name: &OsStr) -> io::Result<()> {
        self.regular(name, libc::O_RDONLY)?.sync_all()
    }

    fn exists(&self, name: &OsStr) -> io::Result<bool> {
        match self.regular(name, libc::O_RDONLY) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let (directory, name) = PrivateDir::parent(path, false)?;
    directory.read(name, limit)
}

pub fn exists(path: &Path) -> io::Result<bool> {
    let (directory, name) = match PrivateDir::parent(path, false) {
        Ok(parent) => parent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    directory.exists(name)
}

pub fn lock(path: &Path) -> io::Result<File> {
    let lock_path = sibling(path, ".lock");
    let (directory, name) = PrivateDir::parent(&lock_path, true)?;
    let file = directory.regular(name, libc::O_RDWR | libc::O_CREAT)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "vault in uso da un'altra operazione; riprova",
        )),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

struct Temporary {
    directory: File,
    name: CString,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        unsafe {
            libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8], create_new: bool) -> io::Result<SaveOutcome> {
    let (directory, destination) = PrivateDir::parent(path, true)?;
    write_atomic_in(directory, destination, bytes, create_new)
}

pub fn create_keyfile(path: &Path, bytes: &[u8]) -> io::Result<SaveOutcome> {
    let directory = PrivateDir::open_external(path.parent().ok_or_else(denied)?)?;
    let name = path.file_name().ok_or_else(denied)?;
    write_atomic_in(directory, name, bytes, true)
}

pub fn read_keyfile(path: &Path, bytes: &mut [u8]) -> io::Result<()> {
    let directory = PrivateDir::open_external(path.parent().ok_or_else(denied)?)?;
    let mut file = directory.regular(path.file_name().ok_or_else(denied)?, libc::O_RDONLY)?;
    if file.metadata()?.len() != bytes.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "dimensione del file chiave non valida",
        ));
    }
    file.read_exact(bytes)?;
    if file.read(&mut [0u8; 1])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "dimensione del file chiave non valida",
        ));
    }
    Ok(())
}

fn write_atomic_in(
    directory: PrivateDir,
    destination: &OsStr,
    bytes: &[u8],
    create_new: bool,
) -> io::Result<SaveOutcome> {
    let present = directory.exists(destination)?;
    if create_new && present {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let mut random = [0u8; 8];
    OsRng.fill_bytes(&mut random);
    let temporary_name = format!(".vault.{:016x}.tmp", u64::from_ne_bytes(random));
    let mut file = directory.regular(
        OsStr::new(&temporary_name),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    let temporary = Temporary {
        directory: directory.file.try_clone()?,
        name: component(OsStr::new(&temporary_name))?,
    };
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(test)]
    fail_at_in(&directory, destination, FailureStage::BeforeCommit)?;
    let destination_c = component(destination)?;
    let result = if create_new {
        unsafe {
            libc::linkat(
                directory.file.as_raw_fd(),
                temporary.name.as_ptr(),
                directory.file.as_raw_fd(),
                destination_c.as_ptr(),
                0,
            )
        }
    } else {
        unsafe {
            libc::renameat(
                directory.file.as_raw_fd(),
                temporary.name.as_ptr(),
                directory.file.as_raw_fd(),
                destination_c.as_ptr(),
            )
        }
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    drop(temporary);
    #[cfg(test)]
    if let Err(error) = fail_at_in(&directory, destination, FailureStage::AfterCommit) {
        return Ok(SaveOutcome::DurabilityUncertain(error));
    }
    Ok(match directory.sync() {
        Ok(()) => SaveOutcome::Durable,
        Err(error) => SaveOutcome::DurabilityUncertain(error),
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq)]
pub enum FailureStage {
    BeforeCommit,
    AfterCommit,
}

#[cfg(test)]
thread_local! {
    static FAILURE: std::cell::RefCell<Option<(PathBuf, FailureStage)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn inject_failure(path: &Path, stage: FailureStage) {
    FAILURE.with(|failure| *failure.borrow_mut() = Some((path.to_path_buf(), stage)));
}

#[cfg(test)]
fn fail_at(path: &Path, stage: FailureStage) -> io::Result<()> {
    FAILURE.with(|failure| {
        let mut failure = failure.borrow_mut();
        if failure
            .as_ref()
            .is_some_and(|(p, s)| p == path && *s == stage)
        {
            *failure = None;
            Err(io::Error::other("errore di I/O simulato"))
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
fn fail_at_in(directory: &PrivateDir, name: &OsStr, stage: FailureStage) -> io::Result<()> {
    let parent = fs::read_link(format!("/proc/self/fd/{}", directory.file.as_raw_fd()))?;
    fail_at(&parent.join(name), stage)
}
