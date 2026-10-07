//! Unix private-state I/O. Every path component is opened without following symlinks;
//! mutation stays relative to the resulting pinned directory descriptor.
use anyhow::{Result, bail};
use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{Component, Path},
};

fn components(path: &Path) -> Result<Vec<OsString>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| anyhow::anyhow!("reviewer_state_path_invalid"))?
            .join(path)
    };
    let mut result = Vec::new();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => (),
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(name) => result.push(name.to_os_string()),
            _ => bail!("reviewer_state_path_invalid"),
        }
    }
    // macOS owns these exact root aliases. Rewrite them lexically to their fixed
    // /private locations; never resolve an arbitrary symlink in a configured path.
    #[cfg(target_os = "macos")]
    if result
        .first()
        .is_some_and(|name| matches!(name.to_str(), Some("var" | "tmp" | "etc")))
    {
        result.insert(0, OsString::from("private"));
    }
    Ok(result)
}

fn c_name(name: &std::ffi::OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| anyhow::anyhow!("reviewer_state_path_invalid"))
}

fn opened_file(mut open: impl FnMut() -> libc::c_int) -> io::Result<File> {
    let mut interrupted = 0;
    loop {
        let fd = open();
        if fd >= 0 {
            // SAFETY: a successful open/openat returns an owned descriptor, consumed exactly once.
            return Ok(unsafe { File::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted || interrupted >= 8 {
            return Err(error);
        }
        interrupted += 1;
    }
}

fn open_at(dir: &File, name: &CString, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
    // SAFETY: the borrowed directory descriptor and NUL-terminated name remain live for the call.
    opened_file(|| unsafe {
        libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, mode as libc::c_uint)
    })
}

struct Directory(File);
impl Directory {
    fn open(parts: &[OsString], create: bool) -> Result<Self> {
        // SAFETY: the root string is static and NUL-terminated.
        let mut file = opened_file(|| unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })
        .map_err(|_| anyhow::anyhow!("reviewer_private_directory_required"))?;
        for part in parts {
            let name = c_name(part)?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let next = match open_at(&file, &name, flags, 0) {
                Ok(next) => next,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    // SAFETY: mkdirat changes only the named entry under the pinned directory.
                    let result = unsafe { libc::mkdirat(file.as_raw_fd(), name.as_ptr(), 0o700) };
                    if result != 0
                        && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                    {
                        bail!("reviewer_state_unwritable");
                    }
                    // A concurrently inserted symlink is refused here rather than followed.
                    open_at(&file, &name, flags, 0)
                        .map_err(|_| anyhow::anyhow!("reviewer_private_directory_required"))?
                }
                Err(_) => bail!("reviewer_private_directory_required"),
            };
            file = next;
        }
        Ok(Self(file))
    }

    fn private(path: &Path) -> Result<Self> {
        let parts = components(path)?;
        if parts.is_empty() {
            bail!("reviewer_state_path_invalid");
        }
        let directory = Self::open(&parts, true)?;
        // SAFETY: chmod operates on the descriptor we checked, never a path that can be swapped.
        if unsafe { libc::fchmod(directory.0.as_raw_fd(), 0o700) } != 0 {
            bail!("reviewer_state_unwritable");
        }
        Ok(directory)
    }

    fn file_parent(path: &Path, create: bool) -> Result<(Self, CString)> {
        let mut parts = components(path)?;
        let name = parts
            .pop()
            .ok_or_else(|| anyhow::anyhow!("reviewer_state_path_invalid"))?;
        let directory = Self::open(&parts, create)?;
        if create {
            if parts.is_empty() {
                bail!("reviewer_state_path_invalid");
            }
            // SAFETY: the descriptor pins the actual parent; only this directory is privatized.
            if unsafe { libc::fchmod(directory.0.as_raw_fd(), 0o700) } != 0 {
                bail!("reviewer_state_unwritable");
            }
        }
        Ok((directory, c_name(&name)?))
    }

    fn remove(&self, name: &CString) {
        // SAFETY: unlinkat removes the entry itself, including symlinks, without following it.
        let _ = unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) };
    }
}

pub(super) fn ensure_root(root: &Path) -> Result<()> {
    Directory::private(root)?;
    Ok(())
}

pub(super) fn private_read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let (directory, name) = Directory::file_parent(path, false)?;
    // NONBLOCK prevents a malicious FIFO from blocking before the regular-file check.
    let file = open_at(
        &directory.0,
        &name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0,
    )
    .map_err(|_| anyhow::anyhow!("reviewer_session_required: import a valid Apple ID session"))?;
    let meta = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("reviewer_private_file_unreadable"))?;
    if !meta.is_file() {
        bail!("reviewer_private_file_required");
    }
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("reviewer_session_permissions: file must have mode 600");
    }
    if meta.len() > max as u64 {
        bail!("reviewer_private_file_too_large");
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("reviewer_private_file_unreadable"))?;
    if bytes.len() > max {
        bail!("reviewer_private_file_too_large");
    }
    Ok(bytes)
}

pub(super) fn private_lock(root: &Path, name: &str) -> Result<File> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        bail!("reviewer_state_path_invalid");
    }
    let directory = Directory::private(root)?;
    let name = c_name(std::ffi::OsStr::new(name))?;
    let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    let mut creators = 0;
    let file = loop {
        // Open existing locks separately from creation. On macOS, concurrent first
        // opens with O_CREAT|O_NOFOLLOW can return ENOENT despite a live directory.
        let candidate = match open_at(&directory.0, &name, flags, 0) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => open_at(
                &directory.0,
                &name,
                flags | libc::O_CREAT | libc::O_EXCL,
                0o600,
            ),
            result => result,
        };
        match candidate {
            Ok(file) => break file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && creators < 8 => {
                creators += 1;
            }
            Err(_) => bail!("reviewer_session_lock_unwritable"),
        }
    };
    let metadata = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("reviewer_session_lock_permissions"))?;
    if !metadata.is_file() {
        bail!("reviewer_private_file_required");
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!("reviewer_session_lock_permissions");
    }
    Ok(file)
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let (directory, target) = Directory::file_parent(path, true)?;
    let (temporary, mut file) = (0..10)
        .find_map(|_| {
            let name = CString::new(format!(".reviewer-{}.tmp", uuid::Uuid::new_v4())).ok()?;
            let file = open_at(
                &directory.0,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
            .ok()?;
            Some((name, file))
        })
        .ok_or_else(|| anyhow::anyhow!("reviewer_state_unwritable"))?;
    let result = (|| -> io::Result<()> {
        // SAFETY: only the newly created file descriptor is modified; umask cannot weaken privacy.
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        // SAFETY: both names refer to the same pinned parent; rename replaces the target entry.
        if unsafe {
            libc::renameat(
                directory.0.as_raw_fd(),
                temporary.as_ptr(),
                directory.0.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        directory.0.sync_all()
    })();
    if result.is_err() {
        directory.remove(&temporary);
        bail!("reviewer_state_unwritable");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn concurrent_first_lock_opens_share_a_single_regular_file() {
        use std::{os::unix::fs::MetadataExt, sync::Barrier};
        let dir = tempfile::tempdir().unwrap();
        for round in 0..24 {
            let root = dir.path().join(format!("round-{round}"));
            ensure_root(&root).unwrap();
            let barrier = Barrier::new(16);
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..16)
                    .map(|_| {
                        let root = &root;
                        let barrier = &barrier;
                        scope.spawn(move || {
                            barrier.wait();
                            private_lock(root, ".session.lock")
                                .unwrap()
                                .metadata()
                                .unwrap()
                                .ino()
                        })
                    })
                    .collect();
                let inodes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
                assert!(inodes.iter().all(|inode| *inode == inodes[0]));
            });
        }
    }

    #[test]
    fn dangling_symlinks_are_refused_for_every_private_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shared");
        ensure_root(&root).unwrap();
        for name in [".session.lock", ".monitor.lock", ".renewal.lock"] {
            let outside = dir.path().join(format!("outside-{name}"));
            symlink(&outside, root.join(name)).unwrap();
            assert!(private_lock(&root, name).is_err());
            assert!(!outside.exists());
        }
    }

    #[test]
    fn atomic_destination_symlink_does_not_change_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shared");
        ensure_root(&root).unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        symlink(&outside, root.join("state.json")).unwrap();
        atomic_write(&root.join("state.json"), b"private state").unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
        assert_eq!(
            private_read(&root.join("state.json"), 1024).unwrap(),
            b"private state"
        );
    }

    #[test]
    fn directory_and_lock_components_cannot_escape_via_user_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link");
        symlink(&outside, &link).unwrap();
        assert!(ensure_root(&link).is_err());
        assert!(private_lock(&link, ".renewal.lock").is_err());
        assert!(private_lock(dir.path(), "../outside.lock").is_err());
        assert!(!outside.join(".renewal.lock").exists());
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn exact_macos_root_aliases_work_while_user_symlinks_fail() {
        for name in ["var", "tmp", "etc"] {
            assert_eq!(
                components(Path::new(&format!("/{name}/example"))).unwrap(),
                ["private", name, "example"].map(OsString::from)
            );
        }
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("session.cookie");
        atomic_write(&path, b"fixture").unwrap();
        assert_eq!(private_read(&path, 1024).unwrap(), b"fixture");
        let link = dir.path().join("user-link");
        symlink(dir.path(), &link).unwrap();
        assert!(private_read(&link.join("session.cookie"), 1024).is_err());
        assert!(atomic_write(&link.join("other.cookie"), b"fixture").is_err());
        assert!(!dir.path().join("other.cookie").exists());
    }
}
