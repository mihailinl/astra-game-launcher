// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The only way detection reads a file's bytes, the built-in descriptors' and Pass B's alike:
//! opened only when it is a regular file as looked at, then opened without following a link
//! (`O_NOFOLLOW`; Windows `FILE_FLAG_OPEN_REPARSE_POINT`), without waiting on a FIFO
//! (`O_NONBLOCK`), and read only when what was opened is a regular file on disk (`S_ISREG`;
//! Windows: no reparse point and `FILE_TYPE_DISK`). Any doubt reads as "nothing there".

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
    let m = f.metadata().ok()?;
    if !m.is_file() {
        return None;
    }
    #[cfg(windows)]
    {
        if !windows_disk_file(&f, &m) {
            return None;
        }
    }
    Some((f, m.len()))
}

#[cfg(windows)]
fn windows_disk_file(f: &File, m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_TYPE_DISK, GetFileType,
    };
    if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return false;
    }
    // SAFETY: the handle is `f`'s own, open for the whole call.
    unsafe { GetFileType(f.as_raw_handle()) == FILE_TYPE_DISK }
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
