// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Anti-cheat a game installed as a Windows service: the machine-side signal of merged plan §2.4
//! ("a system kernel anti-cheat service"), read from the Service Control Manager's own key,
//! `HKLM\SYSTEM\CurrentControlSet\Services`. Read-only; Windows only (elsewhere every answer is
//! empty).
//!
//! - **`kernel-anti-cheat`**: a kernel-mode driver service (`Type` 1, a kernel driver, or 2, a
//!   file-system driver) whose `ImagePath` resolves INSIDE the game's folder.
//! - **`listed-anti-cheat`**: a service of any type that a `service-name` marker of the signed
//!   catalogue names ([`crate::AntiCheatMarker`]), exactly, ignoring ASCII case — counted the same
//!   way only, when its `ImagePath` resolves inside the game's folder.
//!
//! **A service is tied to a game by its image path, never by its name or by being there.** A
//! kernel anti-cheat installed system-wide (Vanguard, FACEIT, an EasyAntiCheat or BattlEye
//! service under `Program Files`) belongs to no one game: counting it would refuse EVERY game on
//! any machine where one is installed, which is a per-machine blanket, not a signal about this
//! game. Such an anti-cheat ships its own folder signature in the games that use it
//! ([`crate::ANTI_CHEAT_SIGNATURES`]), and those games are refused by that. This is the
//! launcher's reading of games-index-v1 §2.3's `service-name` and §2.5's "system-installed kernel
//! anti-cheat service", which do not say how a service belongs to a game
//! (docs/ENGINE-DESCRIPTORS-DEBT.md row 24).
//!
//! An `ImagePath` is resolved as the kernel and the Service Control Manager read one: `\??\C:\…`
//! and `\DosDevices\C:\…` are `C:\…`; `\SystemRoot\…` and a path with no root (`System32\…`,
//! what a driver's default is) are below `%SystemRoot%`; `%VAR%` is expanded; a quoted path
//! ends at its closing quote, and an unquoted one with arguments at the first `.exe`, `.sys` or
//! `.dll` followed by a space. The game's folder is compared as given and as it is on disk (a
//! junction or a short name resolved); an image as written, ignoring ASCII case and folding `.`
//! and `..`, opening no file — but read from disk when a short name (`PROGRA~1`) is on its way.
//! An image naming a device (`\Device\HarddiskVolume3\…`) resolves to nothing here and ties no
//! service.
//!
//! The read is bounded: at most [`MAX_SERVICES`] keys, names of at most 255 characters, values
//! of at most 32 KiB. Every detection and every plan asks, so what a read learned is kept per key
//! with that key's last-write time, which any value written moves: a later read enumerates the
//! keys (the times come with the names) and reads again only a new or changed one, never a stale
//! one. It reads through a seam ([`ServiceTree`]) so a test can lay a fake tree under
//! `HKEY_CURRENT_USER` and never write `HKLM`.

// The path reading is text and tested on every platform; only Windows calls it.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

use crate::engines::EngineSet;
use crate::error::codes;

/// The most service keys read; a real machine has about a thousand.
pub const MAX_SERVICES: usize = 16_384;

/// The `Type` of a kernel-mode driver service (`SERVICE_KERNEL_DRIVER`) and of a file-system
/// driver (`SERVICE_FILE_SYSTEM_DRIVER`).
const KERNEL_DRIVER_TYPES: [u32; 2] = [1, 2];

/// Where services are read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceTree<'a> {
    /// `HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services`: what this machine runs.
    System,
    /// A key below `HKEY_CURRENT_USER` laid out as the `Services` key is (one subkey per service,
    /// with `Type` and `ImagePath`): for tests, which must never write `HKLM`.
    CurrentUser(&'a str),
}

/// One service as its key says: its name, `Type`, `ImagePath` (unexpanded).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Service {
    name: String,
    kind: Option<u32>,
    image_path: Option<String>,
}

/// The anti-cheat codes the services in `tree` give the game in `game_dir`, each once:
/// `kernel-anti-cheat` (a kernel driver whose image is inside the folder), then
/// `listed-anti-cheat` (a service a `service-name` marker in `engines` names, whose image is inside
/// it). Empty off Windows, when the tree cannot be read, and when nothing is tied to the folder.
pub fn of_game(game_dir: &Path, tree: ServiceTree<'_>, engines: &EngineSet) -> Vec<String> {
    #[cfg(windows)]
    {
        let services = registry::services(tree);
        tied(game_dir, &services, engines, &system_root())
    }
    #[cfg(not(windows))]
    {
        let _ = (game_dir, tree, engines);
        Vec::new()
    }
}

#[cfg(windows)]
fn system_root() -> String {
    std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".to_owned())
}

/// [`of_game`] over services already read, `%SystemRoot%` being `system_root`.
fn tied(
    game_dir: &Path,
    services: &[Service],
    engines: &EngineSet,
    system_root: &str,
) -> Vec<String> {
    // The folder as given and as it is on disk (a junction, a short name resolved): an image
    // below either spelling is in it.
    let given = game_dir.to_string_lossy();
    let mut dirs = vec![lexical(&given)];
    let canonical = comparable(&given);
    if !dirs.contains(&canonical) {
        dirs.push(canonical);
    }
    // An image is compared as written, opening no file — a machine has hundreds of drivers and
    // every detection asks — but read from disk when a short name (`PROGRA~1`) is on its way.
    let inside = |s: &Service| {
        s.image_path
            .as_deref()
            .and_then(|raw| resolve_image_path(raw, system_root))
            .is_some_and(|image| {
                let written = lexical(&image);
                dirs.iter().any(|d| within(&written, d))
                    || (written.contains('~')
                        && dirs.iter().any(|d| within(&comparable(&image), d)))
            })
    };
    let mut out: Vec<String> = Vec::new();
    let kernel = services
        .iter()
        .any(|s| s.kind.is_some_and(|k| KERNEL_DRIVER_TYPES.contains(&k)) && inside(s));
    if kernel {
        out.push(codes::KERNEL_ANTI_CHEAT.to_owned());
    }
    let listed = services
        .iter()
        .any(|s| engines.markers().names_service(&s.name) && inside(s));
    if listed {
        out.push(codes::LISTED_ANTI_CHEAT.to_owned());
    }
    out
}

/// A path as compared: canonical when it exists (the long name, the case on disk), with `\\?\`
/// dropped, `/` as `\`, no trailing `\`, ASCII lower-case.
fn comparable(path: &str) -> String {
    let real = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned());
    lexical(&real)
}

/// [`comparable`]'s text half, no file system: `\\?\` dropped, `/` as `\`, `.` and `..`
/// folded, no empty component, ASCII lower-case.
fn lexical(path: &str) -> String {
    let mut s = path.replace('/', "\\");
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_owned();
    }
    let (lead, body) = match s.strip_prefix(r"\\") {
        Some(rest) => (r"\\", rest.to_owned()),
        None => ("", s),
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in body.split('\\') {
        match c {
            "" | "." => {}
            // Never above the drive (or the share): `C:\..` is `C:\`.
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            c => parts.push(c),
        }
    }
    format!("{lead}{}", parts.join("\\")).to_ascii_lowercase()
}

/// Whether `image` (as [`lexical`] spells it) lies below the folder `dir`, the folder itself not
/// counting.
fn within(image: &str, dir: &str) -> bool {
    !dir.is_empty()
        && image.len() > dir.len() + 1
        && image.starts_with(dir)
        && image.as_bytes()[dir.len()] == b'\\'
}

/// The absolute file an `ImagePath` names, as the kernel and the SCM read it (the module doc);
/// `None` for one that names no file on a drive (empty, a device path).
fn resolve_image_path(raw: &str, system_root: &str) -> Option<String> {
    let mut s = expand(raw.trim()).trim().to_owned();
    // The program: quoted, or up to its arguments.
    if let Some(rest) = s.strip_prefix('"') {
        s = rest.split('"').next().unwrap_or("").to_owned();
    } else if let Some(end) = program_end(&s) {
        s.truncate(end);
    }
    let s = s.trim().replace('/', "\\");
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let root = system_root.trim_end_matches('\\');
    let path = if let Some(rest) = strip_ci(&s, &lower, r"\??\") {
        rest.to_owned()
    } else if let Some(rest) = strip_ci(&s, &lower, r"\dosdevices\") {
        rest.to_owned()
    } else if let Some(rest) = strip_ci(&s, &lower, r"\systemroot\") {
        format!(r"{root}\{rest}")
    } else if lower.starts_with(r"\\") || has_drive(&s) {
        s
    } else if lower.starts_with('\\') {
        // A kernel object path that is no DOS device (`\Device\…`): not a file this can place.
        return None;
    } else {
        // No root: below %SystemRoot%, as a driver's default `System32\drivers\<name>.sys` is.
        format!(r"{root}\{s}")
    };
    has_drive(&path).then_some(path)
}

/// `s` without `prefix`, compared ignoring ASCII case (`lower` is `s` lower-cased).
fn strip_ci<'s>(s: &'s str, lower: &str, prefix: &str) -> Option<&'s str> {
    lower.starts_with(prefix).then(|| &s[prefix.len()..])
}

/// `C:\…`: a drive letter, a colon, a separator.
fn has_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

/// Where an unquoted program ends when arguments follow it: after the first `.exe`, `.sys` or
/// `.dll` that a space follows.
fn program_end(s: &str) -> Option<usize> {
    let lower = s.to_ascii_lowercase();
    [".exe ", ".sys ", ".dll "]
        .iter()
        .filter_map(|ext| lower.find(ext).map(|i| i + ext.len() - 1))
        .min()
}

/// `%NAME%` replaced by the environment's `NAME` where it is set; anything else as it is.
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(windows)]
mod registry {
    //! The Services key, read with the registry API: each subkey's `Type` (DWORD) and its
    //! `ImagePath` (`REG_SZ` or `REG_EXPAND_SZ`, read unexpanded: [`super::expand`] does it, the
    //! same for both).

    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_NOEXPAND, RRF_RT_REG_DWORD,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
        RegOpenKeyExW,
    };

    use super::{MAX_SERVICES, Service, ServiceTree};

    /// The longest value read, in bytes.
    const MAX_VALUE_BYTES: u32 = 32 << 10;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// An open key, closed when dropped.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from RegOpenKeyExW and is closed once, here.
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    /// Each tree's services as last read, by name, with each key's last-write time then. A key
    /// whose time is unchanged holds the values it held (writing any value moves it), so a read
    /// enumerates the keys and reads again only the new and the changed ones: one enumeration per
    /// detection after the first, where reading every value takes about 90 ms on a desktop.
    #[allow(clippy::type_complexity)]
    static CACHE: LazyLock<Mutex<HashMap<String, HashMap<String, (u64, Service)>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    /// The trees kept in [`CACHE`]: the system's, and a test's few.
    const MAX_TREES: usize = 8;

    /// The services in `tree`, each with its `Type` and `ImagePath`. Empty when the key cannot
    /// be opened.
    pub(super) fn services(tree: ServiceTree<'_>) -> Vec<Service> {
        let (root, path, id) = match tree {
            ServiceTree::System => (
                HKEY_LOCAL_MACHINE,
                r"SYSTEM\CurrentControlSet\Services",
                r"HKLM\SYSTEM\CurrentControlSet\Services".to_owned(),
            ),
            ServiceTree::CurrentUser(p) => (HKEY_CURRENT_USER, p, format!(r"HKCU\{p}")),
        };
        let mut handle: HKEY = std::ptr::null_mut();
        let path = wide(path);
        // SAFETY: `path` is NUL-terminated and outlives the call; `handle` is written on success.
        let rc = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut handle) };
        if rc != ERROR_SUCCESS {
            return Vec::new();
        }
        let key = Key(handle);
        let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if !cache.contains_key(&id) && cache.len() >= MAX_TREES {
            cache.clear();
        }
        let before = cache.remove(&id).unwrap_or_default();
        let mut now: HashMap<String, (u64, Service)> = HashMap::new();
        for index in 0..MAX_SERVICES as u32 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let mut written = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            // SAFETY: `name` is writable for `len` UTF-16 units, which RegEnumKeyExW never
            // exceeds; `written` is a FILETIME it may fill; the other out-pointers are null.
            let rc = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut written,
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc != ERROR_SUCCESS {
                // A name past 255 units (ERROR_MORE_DATA) or a key that vanished: skipped.
                continue;
            }
            let units = &name[..(len as usize).min(name.len())];
            let text = String::from_utf16_lossy(units);
            let stamp =
                (u64::from(written.dwHighDateTime) << 32) | u64::from(written.dwLowDateTime);
            if let Some((was, service)) = before.get(&text)
                && *was == stamp
            {
                now.insert(text, (stamp, service.clone()));
                continue;
            }
            let mut sub: Vec<u16> = units.to_vec();
            sub.push(0);
            let service = Service {
                kind: dword(key.0, &sub, "Type"),
                image_path: string(key.0, &sub, "ImagePath"),
                name: text.clone(),
            };
            now.insert(text, (stamp, service));
        }
        let mut out: Vec<Service> = now.values().map(|(_, s)| s.clone()).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        cache.insert(id, now);
        out
    }

    fn dword(key: HKEY, sub: &[u16], value: &str) -> Option<u32> {
        let value = wide(value);
        let mut data = 0u32;
        let mut bytes = 4u32;
        // SAFETY: `sub` and `value` are NUL-terminated; `data` is writable for 4 bytes, the most
        // a REG_DWORD (the only type RRF_RT_REG_DWORD admits) is.
        let rc = unsafe {
            RegGetValueW(
                key,
                sub.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u32).cast(),
                &mut bytes,
            )
        };
        (rc == ERROR_SUCCESS).then_some(data)
    }

    fn string(key: HKEY, sub: &[u16], value: &str) -> Option<String> {
        let value = wide(value);
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut bytes = 0u32;
        // SAFETY: a size query: no data pointer, `bytes` receives the size.
        let rc = unsafe {
            RegGetValueW(
                key,
                sub.as_ptr(),
                value.as_ptr(),
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if rc != ERROR_SUCCESS || bytes == 0 || bytes > MAX_VALUE_BYTES {
            return None;
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
        let mut got = (buf.len() * 2) as u32;
        // SAFETY: `buf` is writable for `got` bytes; RegGetValueW writes at most that many and
        // fails (ERROR_MORE_DATA) rather than overrun when the value grew since the query.
        let rc = unsafe {
            RegGetValueW(
                key,
                sub.as_ptr(),
                value.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut got,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let units = &buf[..(got as usize / 2).min(buf.len())];
        let text = units.split(|&c| c == 0).next().unwrap_or(&[]);
        Some(String::from_utf16_lossy(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r"C:\Windows";

    #[test]
    fn every_image_path_form_resolves_as_the_kernel_reads_it() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                r"\??\C:\Games\G\ac\guard.sys",
                Some(r"C:\Games\G\ac\guard.sys"),
            ),
            (r"\DosDevices\D:\G\guard.sys", Some(r"D:\G\guard.sys")),
            (
                r"\SystemRoot\System32\drivers\x.sys",
                Some(r"C:\Windows\System32\drivers\x.sys"),
            ),
            (
                r"\systemroot\system32\drivers\x.sys",
                Some(r"C:\Windows\system32\drivers\x.sys"),
            ),
            (
                r"System32\drivers\x.sys",
                Some(r"C:\Windows\System32\drivers\x.sys"),
            ),
            (r"C:\Games\G\guard.sys", Some(r"C:\Games\G\guard.sys")),
            (
                r#""C:\Program Files\G\svc.exe" -k netsvcs"#,
                Some(r"C:\Program Files\G\svc.exe"),
            ),
            (
                r"C:\Program Files\G\svc.exe -service",
                Some(r"C:\Program Files\G\svc.exe"),
            ),
            (r"C:/Games/G/guard.sys", Some(r"C:\Games\G\guard.sys")),
            (r"\Device\HarddiskVolume3\G\guard.sys", None),
            ("", None),
            ("   ", None),
            (r#""""#, None),
        ];
        for (raw, want) in cases {
            assert_eq!(resolve_image_path(raw, ROOT).as_deref(), *want, "{raw:?}");
        }
    }

    #[test]
    fn environment_strings_are_expanded_and_unknown_ones_kept() {
        let path = std::env::var("PATH").unwrap_or_default();
        assert_eq!(expand("%PATH%"), path);
        assert_eq!(
            expand("%AGL_NO_SUCH_VARIABLE%\\x"),
            "%AGL_NO_SUCH_VARIABLE%\\x"
        );
        assert_eq!(expand("100% sure"), "100% sure");
        assert_eq!(expand("%%"), "%%");
    }

    #[test]
    fn inside_means_below_the_folder_never_beside_it() {
        let dir = lexical(r"\\?\C:\Games\G\");
        assert_eq!(dir, r"c:\games\g");
        assert!(within(&lexical(r"C:\GAMES\G\ac\guard.sys"), &dir));
        assert!(within(&lexical(r"C:\Games\G\ac\..\guard.sys"), &dir));
        assert!(within(&lexical(r"C:/Games//G/./guard.sys"), &dir));
        assert!(!within(&lexical(r"C:\Games\G"), &dir), "the folder itself");
        assert!(!within(&lexical(r"C:\Games\G\"), &dir), "the folder itself");
        assert!(
            !within(&lexical(r"C:\Games\Gx\guard.sys"), &dir),
            "a sibling sharing its prefix"
        );
        assert!(!within(&lexical(r"C:\Games\G\..\H\guard.sys"), &dir));
        assert!(!within(&lexical(r"C:\Games\G\..\..\..\G\x.sys"), &dir));
        assert!(!within(
            &lexical(r"C:\Windows\System32\drivers\x.sys"),
            &dir
        ));
        assert!(!within(&lexical(r"C:\x.sys"), ""));
        assert_eq!(lexical(r"\\?\UNC\host\share\G"), r"\\host\share\g");
    }

    fn service(name: &str, kind: u32, image: &str) -> Service {
        Service {
            name: name.to_owned(),
            kind: Some(kind),
            image_path: Some(image.to_owned()),
        }
    }

    /// Text paths only (none exists here): what `tied` decides, on every platform.
    #[test]
    fn only_a_service_whose_image_is_in_the_game_folder_counts() {
        let game = Path::new(r"C:\Games\G");
        let marked =
            EngineSet::empty().with_anti_cheat_markers(vec![crate::engines::AntiCheatMarker {
                kind: crate::engines::AntiCheatMarkerKind::ServiceName,
                value: "GuardSvc".to_owned(),
            }]);
        let none: Vec<String> = Vec::new();
        let elsewhere = [
            service("vgk", 1, r"\SystemRoot\System32\drivers\vgk.sys"),
            service("Far", 2, r"\??\C:\Program Files\AC\far.sys"),
            service("Beside", 1, r"\??\C:\Games\Gx\beside.sys"),
            service("GuardSvc", 16, r#""C:\Program Files\AC\svc.exe" -run"#),
        ];
        assert_eq!(tied(game, &elsewhere, &marked, ROOT), none);
        let user_mode_in_folder = [service("Other", 16, r"C:\Games\G\bin\svc.exe")];
        assert_eq!(tied(game, &user_mode_in_folder, &marked, ROOT), none);
        let kernel = [service("Drv", 1, r"\??\C:\GAMES\G\ac\drv.sys")];
        assert_eq!(
            tied(game, &kernel, &EngineSet::empty(), ROOT),
            ["kernel-anti-cheat"]
        );
        let fs_driver = [service("FsDrv", 2, r"C:\Games\G\fs.sys")];
        assert_eq!(
            tied(game, &fs_driver, &EngineSet::empty(), ROOT),
            ["kernel-anti-cheat"]
        );
        let named = [service("guardsvc", 16, r#""C:\Games\G\ac\svc.exe" -run"#)];
        assert_eq!(tied(game, &named, &marked, ROOT), ["listed-anti-cheat"]);
        assert_eq!(tied(game, &named, &EngineSet::empty(), ROOT), none);
        let both = [kernel[0].clone(), named[0].clone()];
        assert_eq!(
            tied(game, &both, &marked, ROOT),
            ["kernel-anti-cheat", "listed-anti-cheat"]
        );
        let no_image = [Service {
            name: "Drv".to_owned(),
            kind: Some(1),
            image_path: None,
        }];
        assert_eq!(tied(game, &no_image, &EngineSet::empty(), ROOT), none);
    }

    #[cfg(windows)]
    mod windows {
        use std::process::Command;

        use super::super::*;
        use crate::engines::{AntiCheatMarker, AntiCheatMarkerKind};

        /// A fake Services tree under HKCU, removed when dropped.
        struct FakeTree(String);

        impl FakeTree {
            fn new(tag: &str) -> FakeTree {
                FakeTree(format!(
                    r"Software\AstraGameLauncherTests\{}-{tag}\Services",
                    std::process::id()
                ))
            }

            fn service(&self, name: &str, kind: u32, image: &str, expand: bool) {
                let key = format!(r"HKCU\{}\{name}", self.0);
                let ty = if expand { "REG_EXPAND_SZ" } else { "REG_SZ" };
                for args in [
                    vec![
                        "add",
                        &key,
                        "/v",
                        "Type",
                        "/t",
                        "REG_DWORD",
                        "/d",
                        &kind.to_string(),
                        "/f",
                    ],
                    vec!["add", &key, "/v", "ImagePath", "/t", ty, "/d", image, "/f"],
                ] {
                    let ok = Command::new("reg").args(&args).output().unwrap();
                    assert!(ok.status.success(), "reg {args:?}: {ok:?}");
                }
            }
        }

        impl Drop for FakeTree {
            fn drop(&mut self) {
                let top = self.0.rsplit_once('\\').unwrap().0;
                let _ = Command::new("reg")
                    .args(["delete", &format!(r"HKCU\{top}"), "/f"])
                    .output();
            }
        }

        fn game() -> std::path::PathBuf {
            let d = std::env::temp_dir().join(format!("agl-acs-{}", std::process::id()));
            std::fs::create_dir_all(d.join("ac")).unwrap();
            std::fs::write(d.join("ac").join("guard.sys"), b"MZ").unwrap();
            std::fs::canonicalize(d).unwrap()
        }

        #[test]
        fn anti_cheat_services_a_driver_in_the_folder_is_kernel_anti_cheat_one_elsewhere_is_not() {
            let dir = game();
            let plain = dir.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
            let t = FakeTree::new("unit");
            t.service(
                "FarDriver",
                1,
                r"\SystemRoot\System32\drivers\far.sys",
                false,
            );
            t.service("UserSvc", 16, &format!(r"{plain}\ac\svc.exe"), false);
            let tree = ServiceTree::CurrentUser(&t.0);
            assert_eq!(
                of_game(&dir, tree, &EngineSet::empty()),
                Vec::<String>::new()
            );
            t.service(
                "GameGuardDrv",
                1,
                &format!(r"\??\{plain}\ac\guard.sys"),
                false,
            );
            assert_eq!(
                of_game(&dir, tree, &EngineSet::empty()),
                ["kernel-anti-cheat"]
            );
            let marked = EngineSet::empty().with_anti_cheat_markers(vec![AntiCheatMarker {
                kind: AntiCheatMarkerKind::ServiceName,
                value: "usersvc".to_owned(),
            }]);
            assert_eq!(
                of_game(&dir, tree, &marked),
                ["kernel-anti-cheat", "listed-anti-cheat"]
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
