// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The only way detection reads a file's bytes, the built-in descriptors' and Pass B's alike:
//! opened only when it is a regular file as looked at, then opened without following a link
//! (`O_NOFOLLOW`; Windows `FILE_FLAG_OPEN_REPARSE_POINT`), without waiting on a FIFO
//! (`O_NONBLOCK`), and read only when what was opened is a regular file on disk (`S_ISREG`;
//! Windows `FILE_TYPE_DISK`, see [`windows`]). Any doubt reads as "nothing there".
//!
//! The look before the open is what keeps a FIFO from being opened at all; `O_NONBLOCK` is
//! defence in depth for a FIFO swapped in between the look and the open (no test can make that
//! race), and the check on what was opened refuses it then.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// `path`, opened as described above, with its length. Anything that is not a regular file when
/// looked at (a link, a FIFO, a device node) is not opened at all; one swapped in between the
/// look and the open is refused by the checks on what was opened.
fn open_regular(path: &Path) -> Option<(File, u64)> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let f = opts.open(path).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    #[cfg(windows)]
    let f = windows::disk_file(f, path)?;
    let len = f.metadata().ok()?.len();
    Some((f, len))
}

/// What a handle opened with `FILE_FLAG_OPEN_REPARSE_POINT` may be read as.
#[cfg(windows)]
mod windows {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FILE_TYPE_DISK, FileAttributeTagInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetFileType,
    };

    /// `IsReparseTagNameSurrogate`: the tag names another file (a symbolic link, a junction).
    const NAME_SURROGATE: u32 = 0x2000_0000;

    fn on_disk(f: &File) -> bool {
        // SAFETY: the handle is `f`'s own, open for the whole call.
        unsafe { GetFileType(f.as_raw_handle()) == FILE_TYPE_DISK }
    }

    fn attributes_and_tag(f: &File) -> Option<(u32, u32)> {
        // SAFETY: a zeroed FILE_ATTRIBUTE_TAG_INFO is valid; the call writes at most its size
        // into it, through `f`'s own handle, open for the whole call.
        unsafe {
            let mut info: FILE_ATTRIBUTE_TAG_INFO = std::mem::zeroed();
            let ok = GetFileInformationByHandleEx(
                f.as_raw_handle(),
                FileAttributeTagInfo,
                (&raw mut info).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            );
            (ok != 0).then_some((info.FileAttributes, info.ReparseTag))
        }
    }

    /// The file a handle is open on: its volume and its index there.
    fn identity(f: &File) -> Option<(u32, u32, u32)> {
        // SAFETY: as above.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            (GetFileInformationByHandle(f.as_raw_handle(), &raw mut info) != 0).then_some((
                info.dwVolumeSerialNumber,
                info.nFileIndexHigh,
                info.nFileIndexLow,
            ))
        }
    }

    /// `f` (opened on `path` with `FILE_FLAG_OPEN_REPARSE_POINT`) as a file to read, or `None`.
    /// A file on disk with no reparse point is `f` itself. A name surrogate (a symbolic link, a
    /// junction) is a link, never followed. Any other reparse point — a file compressed by
    /// `compact /exe` or CompactGUI (WOF), deduplicated, a cloud placeholder — is a regular
    /// file whose bytes a filter supplies only to a handle opened THROUGH it, as 0.4.1's plain
    /// `File::open` was: so `path` is opened again the ordinary way, while `f` is still open,
    /// and read only when that is the very same file on disk. A link swapped in between names
    /// another file and is refused; a reparse point no filter serves (an app execution alias)
    /// does not open.
    pub(super) fn disk_file(f: File, path: &Path) -> Option<File> {
        if !on_disk(&f) {
            return None;
        }
        let (attributes, tag) = attributes_and_tag(&f)?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            return Some(f);
        }
        if tag & NAME_SURROGATE != 0 {
            return None;
        }
        let through = File::open(path).ok()?;
        let same = on_disk(&through)
            && through.metadata().is_ok_and(|m| m.is_file())
            && identity(&f).is_some_and(|a| identity(&through) == Some(a));
        same.then_some(through)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn compact(path: &Path) {
            let out = std::process::Command::new("compact.exe")
                .args(["/c", "/exe:xpress4k"])
                .arg(path)
                .output()
                .expect("compact.exe runs");
            assert!(out.status.success(), "compact /exe: {out:?}");
        }

        /// A file compressed by `compact /exe` (WOF, as CompactGUI does) reads as its bytes,
        /// as 0.4.1's plain open read it. Measured on the Windows VM (2026-10-31): with the WOF
        /// filter attached it hides its reparse point altogether (`fsutil reparsepoint query`:
        /// "not a reparse point"; attributes `Archive`), so this is the ordinary path; the
        /// reopen in [`disk_file`] is for a reparse point a filter leaves visible.
        #[test]
        fn a_compressed_file_is_read_as_its_bytes() {
            use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
            let dir = std::env::temp_dir().join(format!("agl-wof-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("globalgamemanagers");
            let mut bytes = b"\0\x162021.3.16f1\0".to_vec();
            bytes.resize(64 * 1024, b'u');
            bytes.extend_from_slice(b"GDPC");
            std::fs::write(&p, &bytes).unwrap();
            compact(&p);
            let wide: Vec<u16> = {
                use std::os::windows::ffi::OsStrExt;
                p.as_os_str().encode_wide().chain([0]).collect()
            };
            let mut high = 0u32;
            // SAFETY: a NUL-terminated wide path; `high` is written once.
            let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &raw mut high) };
            let stored = (u64::from(high) << 32) | u64::from(low);
            assert!(
                stored < bytes.len() as u64,
                "compact /exe compressed it: {stored} bytes stored"
            );
            assert_eq!(super::super::head(&p, 32).as_deref(), Some(&bytes[..32]));
            assert_eq!(super::super::tail(&p, 4, 12).as_deref(), Some(&b"GDPC"[..]));
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// An app execution alias (`%LOCALAPPDATA%\Microsoft\WindowsApps\*.exe`) is a reparse
        /// point no filter serves to a reader (tag `0x8000001B`, no name surrogate): opened
        /// again the ordinary way it does not open, and nothing is read.
        #[test]
        fn an_app_execution_alias_is_not_read() {
            use std::os::windows::fs::OpenOptionsExt;
            let apps = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
                .join("Microsoft")
                .join("WindowsApps");
            let alias = std::fs::read_dir(&apps)
                .expect("WindowsApps")
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(
                            windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
                        )
                        .open(p)
                        .ok()
                        .and_then(|f| attributes_and_tag(&f))
                        .is_some_and(|(a, tag)| {
                            a & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag == 0x8000_001B
                        })
                })
                .expect("an app execution alias in WindowsApps");
            assert!(
                std::fs::symlink_metadata(&alias).unwrap().is_file(),
                "std calls it a file: the reopen is what refuses it"
            );
            assert_eq!(super::super::head(&alias, 4), None, "{alias:?}");
            assert_eq!(super::super::tail(&alias, 4, 0), None, "{alias:?}");
        }

        /// A symbolic link to a file and a junction to a folder are links: nothing is read
        /// through either. Debt row 3: run on the Windows VM (a symbolic link needs an elevated
        /// session or Developer Mode).
        #[test]
        fn a_link_is_never_read_through() {
            let dir = std::env::temp_dir().join(format!("agl-links-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("outside")).unwrap();
            let target = dir.join("outside").join("globalgamemanagers");
            std::fs::write(&target, b"\0\x162021.3.16f1\0 GDPC").unwrap();
            assert!(
                super::super::head(&target, 8).is_some(),
                "the file itself reads"
            );
            let file_link = dir.join("linked");
            std::os::windows::fs::symlink_file(&target, &file_link)
                .expect("a symbolic link (an elevated session, or Developer Mode)");
            assert_eq!(super::super::head(&file_link, 8), None);
            assert_eq!(super::super::tail(&file_link, 4, 0), None);
            let junction = dir.join("junction");
            let out = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&junction)
                .arg(dir.join("outside"))
                .output()
                .unwrap();
            assert!(out.status.success(), "mklink /J: {out:?}");
            assert_eq!(super::super::head(&junction, 8), None);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// The first `max` bytes of the regular file at `path` (fewer when it is shorter).
pub(crate) fn head(path: &Path, max: u64) -> Option<Vec<u8>> {
    let (f, _) = open_regular(path)?;
    let mut out = Vec::new();
    f.take(max).read_to_end(&mut out).ok()?;
    Some(out)
}

/// The last `n` bytes of the regular file at `path`, when it is at least `min_len` long.
pub(crate) fn tail(path: &Path, n: usize, min_len: u64) -> Option<Vec<u8>> {
    let (mut f, len) = open_regular(path)?;
    if len < min_len || len < n as u64 {
        return None;
    }
    f.seek(SeekFrom::End(-i64::try_from(n).ok()?)).ok()?;
    let mut out = vec![0u8; n];
    f.read_exact(&mut out).ok()?;
    Some(out)
}
