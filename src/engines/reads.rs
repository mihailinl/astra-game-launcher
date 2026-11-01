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
        /// through either. Run on the Windows VM (a symbolic link needs an elevated session or
        /// Developer Mode).
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

        /// What the cloud-files placeholder below serves.
        static SERVED: &[u8] = b"\0\x162021.3.16f1\0 the bytes the provider serves .. GDPC";

        /// The test's provider: every FETCH_DATA is answered with [`SERVED`], from the asked
        /// offset to its end.
        unsafe extern "system" fn fetch(
            info: *const windows_sys::Win32::Storage::CloudFilters::CF_CALLBACK_INFO,
            params: *const windows_sys::Win32::Storage::CloudFilters::CF_CALLBACK_PARAMETERS,
        ) {
            use windows_sys::Win32::Storage::CloudFilters::*;
            // SAFETY: the filter passes a valid info and a FETCH_DATA parameter block, alive for
            // the whole call; every pointer handed back lives as long as `SERVED`.
            unsafe {
                let info = &*info;
                let offset = (*params).Anonymous.FetchData.RequiredFileOffset.max(0) as usize;
                let offset = offset.min(SERVED.len());
                let op = CF_OPERATION_INFO {
                    StructSize: std::mem::size_of::<CF_OPERATION_INFO>() as u32,
                    Type: CF_OPERATION_TYPE_TRANSFER_DATA,
                    ConnectionKey: info.ConnectionKey,
                    TransferKey: info.TransferKey,
                    CorrelationVector: std::ptr::null(),
                    SyncStatus: std::ptr::null(),
                    RequestKey: info.RequestKey,
                };
                let mut p: CF_OPERATION_PARAMETERS = std::mem::zeroed();
                p.ParamSize = (std::mem::offset_of!(CF_OPERATION_PARAMETERS, Anonymous)
                    + std::mem::size_of::<CF_OPERATION_PARAMETERS_0_0>())
                    as u32;
                p.Anonymous.TransferData = CF_OPERATION_PARAMETERS_0_0 {
                    Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
                    CompletionStatus: 0,
                    Buffer: SERVED[offset..].as_ptr().cast(),
                    Offset: offset as i64,
                    Length: (SERVED.len() - offset) as i64,
                };
                CfExecute(&op, &mut p);
            }
        }

        fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            s.encode_wide().chain([0]).collect()
        }

        /// The sync root, registered and connected for one test, under its own temporary
        /// folder: disconnected, unregistered and removed when dropped, whatever failed.
        struct SyncRoot {
            dir: std::path::PathBuf,
            key: Option<windows_sys::Win32::Storage::CloudFilters::CF_CONNECTION_KEY>,
            registered: bool,
        }

        impl Drop for SyncRoot {
            fn drop(&mut self) {
                use windows_sys::Win32::Storage::CloudFilters::*;
                // SAFETY: the key and the path are the ones this root was set up with.
                unsafe {
                    if let Some(k) = self.key.take() {
                        CfDisconnectSyncRoot(k);
                    }
                    if self.registered {
                        CfUnregisterSyncRoot(wide(self.dir.as_os_str()).as_ptr());
                    }
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        #[link(name = "ntdll")]
        unsafe extern "system" {
            /// `PHCM_EXPOSE_PLACEHOLDERS` (2): this process sees a placeholder's reparse point.
            fn RtlSetProcessPlaceholderCompatibilityMode(mode: i8) -> i8;
        }

        /// A reparse point that is no link, that a filter leaves visible and serves: the branch
        /// of [`disk_file`] that opens a file again. A sync root registered for this test (the
        /// Cloud Files API, as OneDrive's Files On-Demand uses it) holds a dehydrated
        /// placeholder whose bytes the test's own provider serves; with the process asking to
        /// see placeholders (`PHCM_EXPOSE_PLACEHOLDERS`), its cloud reparse tag shows through
        /// `FILE_FLAG_OPEN_REPARSE_POINT` and it is no name surrogate, so it is opened again the
        /// ordinary way — through the filter, which fetches the bytes — found to be the same
        /// file (volume, index), and read: exactly what 0.4.1's plain open read. Measured on the
        /// Windows VM (2026-10-31, Windows 11, an elevated SSH session): a build that refuses
        /// every visible reparse point fails here, which the three tests above did not catch;
        /// one that reads the first handle without opening again passes here (the filter
        /// serves that handle too) and fails [`an_app_execution_alias_is_not_read`].
        #[test]
        fn a_cloud_placeholder_is_read_as_the_file_its_filter_serves() {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::CloudFilters::*;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ATTRIBUTE_NORMAL, FILE_BASIC_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
            };

            let dir = std::env::temp_dir().join(format!("agl-cloud-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = std::fs::canonicalize(&dir).unwrap();
            // `canonicalize` gives `\\?\C:\…`; the Cloud Files API wants a plain path.
            let plain = std::path::PathBuf::from(
                dir.to_str().unwrap().trim_start_matches(r"\\?\").to_owned(),
            );
            let mut root = SyncRoot {
                dir: plain.clone(),
                key: None,
                registered: false,
            };
            let root_w = wide(plain.as_os_str());
            let provider = wide("astra-game-launcher test".as_ref());
            let version = wide("1".as_ref());
            let identity = b"agl-test-sync-root";
            let registration = CF_SYNC_REGISTRATION {
                StructSize: std::mem::size_of::<CF_SYNC_REGISTRATION>() as u32,
                ProviderName: provider.as_ptr(),
                ProviderVersion: version.as_ptr(),
                SyncRootIdentity: identity.as_ptr().cast(),
                SyncRootIdentityLength: identity.len() as u32,
                FileIdentity: std::ptr::null(),
                FileIdentityLength: 0,
                ProviderId: windows_sys::core::GUID::from_u128(
                    0x5a7a_0500_0003_4c38_9a1e_0b5d_0c0d_0a03,
                ),
            };
            let policies = CF_SYNC_POLICIES {
                StructSize: std::mem::size_of::<CF_SYNC_POLICIES>() as u32,
                Hydration: CF_HYDRATION_POLICY {
                    Primary: CF_HYDRATION_POLICY_FULL,
                    Modifier: CF_HYDRATION_POLICY_MODIFIER_NONE,
                },
                Population: CF_POPULATION_POLICY {
                    Primary: CF_POPULATION_POLICY_ALWAYS_FULL,
                    Modifier: CF_POPULATION_POLICY_MODIFIER_NONE,
                },
                InSync: CF_INSYNC_POLICY_NONE,
                HardLink: CF_HARDLINK_POLICY_NONE,
                PlaceholderManagement: CF_PLACEHOLDER_MANAGEMENT_POLICY_DEFAULT,
            };
            // SAFETY: every pointer is to a NUL-terminated wide string or a struct alive for
            // the call.
            let hr = unsafe {
                CfRegisterSyncRoot(
                    root_w.as_ptr(),
                    &registration,
                    &policies,
                    CF_REGISTER_FLAG_NONE,
                )
            };
            assert_eq!(hr, 0, "CfRegisterSyncRoot: {hr:#x}");
            root.registered = true;
            let callbacks = [
                CF_CALLBACK_REGISTRATION {
                    Type: CF_CALLBACK_TYPE_FETCH_DATA,
                    Callback: Some(fetch),
                },
                CF_CALLBACK_REGISTRATION {
                    Type: CF_CALLBACK_TYPE_NONE,
                    Callback: None,
                },
            ];
            let mut key: CF_CONNECTION_KEY = 0;
            // SAFETY: as above; the table outlives the connection (it is dropped after `root`).
            let hr = unsafe {
                CfConnectSyncRoot(
                    root_w.as_ptr(),
                    callbacks.as_ptr(),
                    std::ptr::null(),
                    CF_CONNECT_FLAG_NONE,
                    &mut key,
                )
            };
            assert_eq!(hr, 0, "CfConnectSyncRoot: {hr:#x}");
            root.key = Some(key);

            let name = wide("globalgamemanagers".as_ref());
            let file_identity = b"served";
            let mut info: CF_PLACEHOLDER_CREATE_INFO = unsafe { std::mem::zeroed() };
            info.RelativeFileName = name.as_ptr();
            info.FsMetadata = CF_FS_METADATA {
                BasicInfo: FILE_BASIC_INFO {
                    FileAttributes: FILE_ATTRIBUTE_NORMAL,
                    ..unsafe { std::mem::zeroed() }
                },
                FileSize: SERVED.len() as i64,
            };
            info.FileIdentity = file_identity.as_ptr().cast();
            info.FileIdentityLength = file_identity.len() as u32;
            info.Flags = CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC;
            let mut done = 0u32;
            // SAFETY: one create info, alive for the call.
            let hr = unsafe {
                CfCreatePlaceholders(
                    root_w.as_ptr(),
                    &mut info,
                    1,
                    CF_CREATE_FLAG_NONE,
                    &mut done,
                )
            };
            assert_eq!((hr, done, info.Result), (0, 1, 0), "CfCreatePlaceholders");
            let p = dir.join("globalgamemanagers");

            // SAFETY: a process-wide mode, set before anything here opens the placeholder.
            unsafe { RtlSetProcessPlaceholderCompatibilityMode(2) };
            let raw = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&p)
                .unwrap();
            let (attributes, tag) = attributes_and_tag(&raw).unwrap();
            drop(raw);
            assert!(
                attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
                "the filter shows the placeholder's reparse point: attributes {attributes:#x}"
            );
            assert_eq!(tag & 0xFFFF_0FFF, 0x9000_001A, "a cloud tag: {tag:#x}");
            assert_eq!(tag & NAME_SURROGATE, 0, "no name surrogate");
            assert_eq!(
                super::super::head(&p, 4096).as_deref(),
                Some(SERVED),
                "the placeholder reads as the bytes its filter serves"
            );
            assert_eq!(super::super::tail(&p, 4, 12).as_deref(), Some(&b"GDPC"[..]));
            drop(root);
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
