use anyhow::{Result, bail};
use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
        },
    },
    path::{Component, Path},
};

fn components(path: &Path) -> Result<Vec<OsString>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| anyhow::anyhow!("developer_state_path_invalid"))?
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
            _ => bail!("developer_state_path_invalid"),
        }
    }
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
    CString::new(name.as_bytes()).map_err(|_| anyhow::anyhow!("developer_state_path_invalid"))
}

fn opened_file(mut open: impl FnMut() -> libc::c_int) -> io::Result<File> {
    let mut interrupted = 0;
    loop {
        let fd = open();
        if fd >= 0 {
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
    opened_file(|| unsafe {
        libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, mode as libc::c_uint)
    })
}

struct Directory(File);
impl Directory {
    fn open(parts: &[OsString], create: bool) -> Result<Self> {
        let mut file = opened_file(|| unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })
        .map_err(|_| anyhow::anyhow!("developer_private_directory_required"))?;
        for part in parts {
            let name = c_name(part)?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let next = match open_at(&file, &name, flags, 0) {
                Ok(next) => next,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    let result = unsafe { libc::mkdirat(file.as_raw_fd(), name.as_ptr(), 0o700) };
                    if result != 0
                        && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                    {
                        bail!("developer_state_unwritable");
                    }
                    open_at(&file, &name, flags, 0)
                        .map_err(|_| anyhow::anyhow!("developer_private_directory_required"))?
                }
                Err(_) => bail!("developer_private_directory_required"),
            };
            file = next;
        }
        Ok(Self(file))
    }

    fn private(path: &Path) -> Result<Self> {
        let parts = components(path)?;
        if parts.is_empty() {
            bail!("developer_state_path_invalid");
        }
        let directory = Self::open(&parts, true)?;
        if directory
            .0
            .metadata()
            .map_err(|_| anyhow::anyhow!("developer_private_directory_required"))?
            .uid()
            != unsafe { libc::geteuid() }
        {
            bail!("developer_private_directory_required");
        }
        if unsafe { libc::fchmod(directory.0.as_raw_fd(), 0o700) } != 0 {
            bail!("developer_state_unwritable");
        }
        Ok(directory)
    }

    fn file_parent(path: &Path, create: bool) -> Result<(Self, CString)> {
        let mut parts = components(path)?;
        let name = parts
            .pop()
            .ok_or_else(|| anyhow::anyhow!("developer_state_path_invalid"))?;
        let directory = Self::open(&parts, create)?;
        if create {
            if parts.is_empty() {
                bail!("developer_state_path_invalid");
            }
            if directory
                .0
                .metadata()
                .map_err(|_| anyhow::anyhow!("developer_private_directory_required"))?
                .uid()
                != unsafe { libc::geteuid() }
            {
                bail!("developer_private_directory_required");
            }
            if unsafe { libc::fchmod(directory.0.as_raw_fd(), 0o700) } != 0 {
                bail!("developer_state_unwritable");
            }
        }
        Ok((directory, c_name(&name)?))
    }

    fn remove(&self, name: &CString) {
        let _ = unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) };
    }
}

pub(crate) fn private_read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let (directory, name) = Directory::file_parent(path, false)?;
    let parent = directory
        .0
        .metadata()
        .map_err(|_| anyhow::anyhow!("developer_private_directory_required"))?;
    if parent.permissions().mode() & 0o077 != 0 || parent.uid() != unsafe { libc::geteuid() } {
        bail!("developer_private_directory_required");
    }
    let file = open_at(
        &directory.0,
        &name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0,
    )
    .map_err(|_| anyhow::anyhow!("developer_session_required: run interactive developer-login"))?;
    let meta = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("developer_private_file_unreadable"))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        bail!("developer_private_file_required");
    }
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("developer_session_permissions: file must have mode 600");
    }
    if meta.len() > max as u64 {
        bail!("developer_private_file_too_large");
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("developer_private_file_unreadable"))?;
    if bytes.len() > max {
        bail!("developer_private_file_too_large");
    }
    Ok(bytes)
}

pub(crate) fn private_lock(root: &Path, name: &str) -> Result<File> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        bail!("developer_state_path_invalid");
    }
    let directory = Directory::private(root)?;
    let name = c_name(std::ffi::OsStr::new(name))?;
    let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    let mut creators = 0;
    let file = loop {
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
            Err(_) => bail!("developer_session_lock_unwritable"),
        }
    };
    let metadata = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("developer_session_lock_permissions"))?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("developer_private_file_required");
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!("developer_session_lock_permissions");
    }
    Ok(file)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let (directory, target) = Directory::file_parent(path, true)?;
    let (temporary, mut file) = (0..10)
        .find_map(|_| {
            let name = CString::new(format!(".developer-{}.tmp", uuid::Uuid::new_v4())).ok()?;
            let file = open_at(
                &directory.0,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
            .ok()?;
            Some((name, file))
        })
        .ok_or_else(|| anyhow::anyhow!("developer_state_unwritable"))?;
    let result = (|| -> io::Result<()> {
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        file.write_all(bytes)?;
        file.sync_all()?;
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
        bail!("developer_state_unwritable");
    }
    Ok(())
}
