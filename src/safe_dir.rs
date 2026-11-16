// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A folder below a canonical root that someone else may be writing into at the same time —
//! a profile's `BepInEx`, which Flathub's Steam is granted (`crate::flatpak`), so any program in
//! Steam's sandbox can swap a file or a folder there for a link at any moment (0.5.3).
//!
//! On Unix the folder is held as a file descriptor reached by an `openat` walk from the root,
//! every component opened `O_NOFOLLOW | O_DIRECTORY`: a link anywhere on the way is refused
//! (`path-escapes`), and once it is open, no later swap of a parent can move it. Every read,
//! write and delete is then relative to that descriptor, and the last component is never
//! followed either: a read opens it `O_NOFOLLOW | O_NONBLOCK` and takes only a regular file, a
//! write creates its temporary file `O_EXCL | O_NOFOLLOW` beside it, sets its mode on the open
//! file and `renameat`s it over the name (a link there is replaced, never written through), and
//! a delete is `unlinkat` (a link is removed itself). Checking a path and then using it — the
//! race this closes — happens nowhere.
//!
//! Elsewhere (Windows, where no sandbox writes into the profile) it is the path-based behaviour
//! the launcher always had: the parents checked for links, then the path used.

use std::path::{Path, PathBuf};

use crate::error::{LauncherError, Result, codes};

fn escapes(path: &Path) -> LauncherError {
    LauncherError::new(codes::PATH_ESCAPES).with("path", path.display().to_string())
}

/// A file read out of a [`SafeDir`]: its bytes, and whether only its owner may read it (mode
/// `0600` or stricter on Unix; always `true` elsewhere).
pub(crate) struct Read {
    pub bytes: Vec<u8>,
    pub private: bool,
}

/// A folder below a canonical root, reached without following a link (the module docs).
pub(crate) struct SafeDir {
    /// The folder's path, for messages; never used to reach it on Unix.
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

/// `rel` split into its folders and its file name; `path-escapes` for an empty one.
pub(crate) fn split(rel: &[String]) -> Result<(&[String], &str)> {
    match rel.split_last() {
        Some((name, parents)) => Ok((parents, name.as_str())),
        None => Err(LauncherError::new(codes::PATH_ESCAPES).with("path", "")),
    }
}

impl SafeDir {
    /// The path of `name` in this folder, for messages.
    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::{Read, SafeDir, escapes};
    use crate::error::{LauncherError, Result};

    fn cstr(s: &str, at: &Path) -> Result<CString> {
        CString::new(s).map_err(|_| escapes(at))
    }

    fn last_error() -> std::io::Error {
        std::io::Error::last_os_error()
    }

    /// `openat(dir, name, O_DIRECTORY | O_NOFOLLOW)`.
    fn open_dir_at(dir: libc::c_int, name: &CString) -> std::io::Result<OwnedFd> {
        // SAFETY: `name` is a NUL-terminated string that outlives the call; the flags make the
        // result a directory descriptor or an error.
        let fd = unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_error());
        }
        // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`; `None` when there is nothing there.
    fn lstat_at(dir: &OwnedFd, name: &CString) -> std::io::Result<Option<libc::stat>> {
        // SAFETY: a zeroed `stat` is a valid buffer for `fstatat` to fill.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `dir` is an open descriptor, `name` NUL-terminated, `st` a valid buffer.
        let r = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if r == 0 {
            return Ok(Some(st));
        }
        let e = last_error();
        if e.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(e)
        }
    }

    fn kind(st: &libc::stat) -> libc::mode_t {
        st.st_mode & libc::S_IFMT
    }

    impl SafeDir {
        /// The folder `rel` below the canonical folder `root`, every component opened without
        /// following a link (`path-escapes` for a link, or anything but a folder, on the way).
        /// Missing folders are created when `create`, else the answer is `None`.
        pub(crate) fn open(root: &Path, rel: &[String], create: bool) -> Result<Option<SafeDir>> {
            let c_root = CString::new(root.as_os_str().as_bytes()).map_err(|_| escapes(root))?;
            // The root itself is the caller's canonical folder (resolved, no link left in it);
            // it is opened as a folder and never re-resolved.
            // SAFETY: `c_root` is NUL-terminated and outlives the call.
            let fd = unsafe {
                libc::open(
                    c_root.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(LauncherError::io_at(&last_error(), root));
            }
            // SAFETY: `fd` was just returned by `open` and is owned by nobody else.
            let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
            let mut path = root.to_path_buf();
            for c in rel {
                path.push(c);
                let name = cstr(c, &path)?;
                dir = match open_dir_at(dir.as_raw_fd(), &name) {
                    Ok(next) => next,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        if !create {
                            return Ok(None);
                        }
                        // SAFETY: `dir` is open, `name` NUL-terminated.
                        let made = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o777) };
                        if made != 0 {
                            let e = last_error();
                            if e.kind() != std::io::ErrorKind::AlreadyExists {
                                return Err(LauncherError::io_at(&e, &path));
                            }
                        }
                        open_dir_at(dir.as_raw_fd(), &name).map_err(|_| escapes(&path))?
                    }
                    // ELOOP (a link), ENOTDIR (a file): never walked through.
                    Err(_) => return Err(escapes(&path)),
                };
            }
            Ok(Some(SafeDir { path, fd: dir }))
        }

        /// The regular file `name` here, opened without following a link and without waiting
        /// on a FIFO; `None` when there is none. A link, a folder or anything else in its place
        /// is `path-escapes`, and nothing of it is read. At most `max` bytes (`io` beyond).
        pub(crate) fn read(&self, name: &str, max: u64) -> Result<Option<Read>> {
            let at = self.join(name);
            let c = cstr(name, &at)?;
            // SAFETY: `self.fd` is open, `c` NUL-terminated.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let e = last_error();
                return match e.kind() {
                    std::io::ErrorKind::NotFound => Ok(None),
                    _ if e.raw_os_error() == Some(libc::ELOOP) => Err(escapes(&at)),
                    _ => Err(LauncherError::io_at(&e, &at)),
                };
            }
            // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
            let f = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            let m = f.metadata().map_err(|e| LauncherError::io_at(&e, &at))?;
            if !m.is_file() {
                return Err(escapes(&at).with("reason", "not-a-file"));
            }
            let mut bytes = Vec::new();
            (&f).take(max + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| LauncherError::io_at(&e, &at))?;
            if bytes.len() as u64 > max {
                return Err(LauncherError::io("too large").with("path", at.display().to_string()));
            }
            Ok(Some(Read {
                bytes,
                private: m.permissions().mode() & 0o077 == 0,
            }))
        }

        /// `path-escapes` when a link or anything but a regular file is at `name`.
        pub(crate) fn check_regular_or_absent(&self, name: &str) -> Result<()> {
            let at = self.join(name);
            match lstat_at(&self.fd, &cstr(name, &at)?) {
                Ok(None) => Ok(()),
                Ok(Some(st)) if kind(&st) == libc::S_IFREG => Ok(()),
                Ok(Some(st)) if kind(&st) == libc::S_IFLNK => Err(escapes(&at)),
                Ok(Some(_)) => Err(escapes(&at).with("reason", "not-a-file")),
                Err(e) => Err(LauncherError::io_at(&e, &at)),
            }
        }

        /// Writes `bytes` as `name` here: a temporary file created beside it (`O_EXCL |
        /// O_NOFOLLOW`), its mode set on the open file (`private`: exactly `0600`; otherwise the
        /// mode of the regular file it replaces, or the umask's), synced, and `renameat` over
        /// the name. A link at `name` is replaced, never written through.
        pub(crate) fn write_atomic(&self, name: &str, bytes: &[u8], private: bool) -> Result<()> {
            let at = self.join(name);
            let c_name = cstr(name, &at)?;
            let tmp = crate::paths::temp_name(name);
            let c_tmp = cstr(&tmp, &at)?;
            let keep = match lstat_at(&self.fd, &c_name) {
                Ok(Some(st)) if kind(&st) == libc::S_IFREG => Some(st.st_mode & 0o7777),
                _ => None,
            };
            let create_mode: libc::c_uint = if private { 0o600 } else { 0o666 };
            // SAFETY: `self.fd` is open, `c_tmp` NUL-terminated; O_EXCL|O_NOFOLLOW never opens
            // anything that was already there.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    c_tmp.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    create_mode,
                )
            };
            if fd < 0 {
                return Err(LauncherError::io_at(&last_error(), &at));
            }
            // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
            let mut f = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            let written = (|| -> std::io::Result<()> {
                f.write_all(bytes)?;
                let mode = if private { Some(0o600) } else { keep };
                if let Some(mode) = mode {
                    // `mode_t` is `u16` on macOS and `u32` on Linux.
                    #[allow(clippy::useless_conversion)]
                    let mode = u32::from(mode);
                    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
                }
                f.sync_all()?;
                // SAFETY: `self.fd` is open, both names NUL-terminated.
                let r = unsafe {
                    libc::renameat(
                        self.fd.as_raw_fd(),
                        c_tmp.as_ptr(),
                        self.fd.as_raw_fd(),
                        c_name.as_ptr(),
                    )
                };
                if r != 0 {
                    return Err(last_error());
                }
                Ok(())
            })();
            drop(f);
            if let Err(e) = written {
                // SAFETY: `self.fd` is open, `c_tmp` NUL-terminated; removes our temporary file.
                unsafe { libc::unlinkat(self.fd.as_raw_fd(), c_tmp.as_ptr(), 0) };
                return Err(LauncherError::io_at(&e, &at));
            }
            Ok(())
        }

        /// Removes the file or the link `name` here (a link itself, never what it names);
        /// `Ok(false)` when there is none, or a folder is there (left alone).
        pub(crate) fn remove_file(&self, name: &str) -> Result<bool> {
            let at = self.join(name);
            let c = cstr(name, &at)?;
            match lstat_at(&self.fd, &c) {
                Ok(Some(st)) if matches!(kind(&st), libc::S_IFREG | libc::S_IFLNK) => {}
                Ok(_) => return Ok(false),
                Err(e) => return Err(LauncherError::io_at(&e, &at)),
            }
            // SAFETY: `self.fd` is open, `c` NUL-terminated; `unlinkat` never follows a link.
            if unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) } != 0 {
                let e = last_error();
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(false);
                }
                return Err(LauncherError::io_at(&e, &at));
            }
            Ok(true)
        }

        /// Removes the empty folder `name` here; whether it did.
        pub(crate) fn remove_empty_dir(&self, name: &str) -> bool {
            let Ok(c) = CString::new(name) else {
                return false;
            };
            // SAFETY: `self.fd` is open, `c` NUL-terminated; AT_REMOVEDIR removes only an
            // empty folder, and never through a link.
            unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) == 0 }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::fs;
    use std::io::{ErrorKind, Read as _};
    use std::path::Path;

    use super::{Read, SafeDir, escapes};
    use crate::error::{LauncherError, Result};
    use crate::paths::{check_regular_or_absent, write_atomic, write_atomic_private};

    impl SafeDir {
        /// The folder `rel` below `root`, every component checked not to be a link; created when
        /// `create`, else `None` when missing.
        pub(crate) fn open(root: &Path, rel: &[String], create: bool) -> Result<Option<SafeDir>> {
            let mut path = root.to_path_buf();
            for c in rel {
                path.push(c);
                match fs::symlink_metadata(&path) {
                    Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                        return Err(escapes(&path));
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        if !create {
                            return Ok(None);
                        }
                        match fs::create_dir(&path) {
                            Ok(()) => {}
                            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                                let m = fs::symlink_metadata(&path)
                                    .map_err(|e| LauncherError::io_at(&e, &path))?;
                                if m.file_type().is_symlink() || !m.is_dir() {
                                    return Err(escapes(&path));
                                }
                            }
                            Err(e) => return Err(LauncherError::io_at(&e, &path)),
                        }
                    }
                    Err(e) => return Err(LauncherError::io_at(&e, &path)),
                }
            }
            Ok(Some(SafeDir { path }))
        }

        pub(crate) fn read(&self, name: &str, max: u64) -> Result<Option<Read>> {
            let at = self.join(name);
            check_regular_or_absent(&at)?;
            let f = match fs::File::open(&at) {
                Ok(f) => f,
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(LauncherError::io_at(&e, &at)),
            };
            let mut bytes = Vec::new();
            f.take(max + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| LauncherError::io_at(&e, &at))?;
            if bytes.len() as u64 > max {
                return Err(LauncherError::io("too large").with("path", at.display().to_string()));
            }
            Ok(Some(Read {
                bytes,
                private: true,
            }))
        }

        pub(crate) fn check_regular_or_absent(&self, name: &str) -> Result<()> {
            check_regular_or_absent(&self.join(name))
        }

        pub(crate) fn write_atomic(&self, name: &str, bytes: &[u8], private: bool) -> Result<()> {
            let at = self.join(name);
            if private {
                write_atomic_private(&at, bytes)
            } else {
                write_atomic(&at, bytes)
            }
        }

        pub(crate) fn remove_file(&self, name: &str) -> Result<bool> {
            let at = self.join(name);
            match fs::symlink_metadata(&at) {
                Ok(m) if m.is_file() || m.file_type().is_symlink() => {
                    fs::remove_file(&at).map_err(|e| LauncherError::io_at(&e, &at))?;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }

        pub(crate) fn remove_empty_dir(&self, name: &str) -> bool {
            fs::remove_dir(self.join(name)).is_ok()
        }
    }
}

/// Removes the file or link `rel` below the canonical `root` without following a link on the
/// way, then every folder it was in that is empty now, up to (never) `root`. `Ok(false)` when
/// there was nothing to remove, or a link stood on the way (nothing is removed through one).
pub(crate) fn remove_in(root: &Path, rel: &[String]) -> Result<bool> {
    let (parents, name) = split(rel)?;
    let removed = match SafeDir::open(root, parents, false) {
        Ok(Some(dir)) => dir.remove_file(name)?,
        Ok(None) => false,
        Err(e) if e.code == codes::PATH_ESCAPES => false,
        Err(e) => return Err(e),
    };
    prune_empty_in(root, parents);
    Ok(removed)
}

/// Removes the folder `rel` below `root` when it is empty, then each folder above it that is
/// empty then, up to (never) `root`; stops at the first that is not, or at a link.
pub(crate) fn prune_empty_in(root: &Path, rel: &[String]) {
    for depth in (1..=rel.len()).rev() {
        let Ok(Some(dir)) = SafeDir::open(root, &rel[..depth - 1], false) else {
            return;
        };
        if !dir.remove_empty_dir(&rel[depth - 1]) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use std::fs;
        use std::os::unix::fs::{PermissionsExt, symlink};

        fn tmp(name: &str) -> PathBuf {
            let p =
                std::env::temp_dir().join(format!("agl-safe-dir-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            fs::canonicalize(&p).unwrap()
        }

        fn rel(s: &str) -> Vec<String> {
            s.split('/').map(str::to_owned).collect()
        }

        /// A link in place of a parent folder is never walked through, to read, write or delete.
        #[test]
        fn a_linked_parent_is_never_walked_through() {
            let root = tmp("parent");
            let host = tmp("parent-host");
            fs::write(host.join("secret"), b"key").unwrap();
            fs::create_dir_all(root.join("BepInEx")).unwrap();
            symlink(&host, root.join("BepInEx/config")).unwrap();
            let e = SafeDir::open(&root, &rel("BepInEx/config"), true)
                .err()
                .unwrap();
            assert_eq!(e.code, codes::PATH_ESCAPES);
            assert!(!remove_in(&root, &rel("BepInEx/config/secret")).unwrap());
            assert_eq!(fs::read(host.join("secret")).unwrap(), b"key");
        }

        /// A link in place of the file: a read refuses it and reads nothing, a write replaces the
        /// link (the file it named untouched), a delete removes the link only.
        #[test]
        fn a_linked_file_is_never_followed() {
            let root = tmp("file");
            let host = tmp("file-host");
            fs::write(host.join("secret"), b"key").unwrap();
            let dir = SafeDir::open(&root, &rel("BepInEx/config"), true)
                .unwrap()
                .unwrap();
            symlink(host.join("secret"), dir.join("astra.cfg")).unwrap();
            assert_eq!(
                dir.read("astra.cfg", 1024).err().unwrap().code,
                codes::PATH_ESCAPES
            );
            assert_eq!(
                dir.check_regular_or_absent("astra.cfg").err().unwrap().code,
                codes::PATH_ESCAPES
            );
            dir.write_atomic("astra.cfg", b"ours", true).unwrap();
            assert_eq!(fs::read(host.join("secret")).unwrap(), b"key");
            assert_eq!(fs::read(dir.join("astra.cfg")).unwrap(), b"ours");
            let m = fs::symlink_metadata(dir.join("astra.cfg")).unwrap();
            assert!(m.is_file() && m.permissions().mode() & 0o777 == 0o600);
            symlink(host.join("secret"), dir.join("other")).unwrap();
            assert!(dir.remove_file("other").unwrap());
            assert_eq!(fs::read(host.join("secret")).unwrap(), b"key");
        }

        /// A write keeps the mode of the file it replaces, set on the open file.
        #[test]
        fn a_write_keeps_the_replaced_files_mode() {
            let root = tmp("mode");
            let dir = SafeDir::open(&root, &rel("a"), true).unwrap().unwrap();
            fs::write(dir.join("f"), b"old").unwrap();
            fs::set_permissions(dir.join("f"), fs::Permissions::from_mode(0o640)).unwrap();
            dir.write_atomic("f", b"new", false).unwrap();
            let m = fs::metadata(dir.join("f")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o640);
            let r = dir.read("f", 10).unwrap().unwrap();
            assert_eq!(r.bytes, b"new");
            assert!(!r.private);
        }
    }
}
