// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The path jail. A manifest names a destination as `${profile}/…` or `${game}/…`; nothing else
//! is a destination. The relative part is checked lexically when the manifest is read, and again
//! on disk — after parent directories are created and links resolved — before every write.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::digest::sha256_file;
use crate::error::{LauncherError, Result, codes};

/// The profile's own bookkeeping names. A manifest may not write them.
pub(crate) const LEDGER_FILE: &str = "astra-launcher-ledger.json";
pub(crate) const LOCK_FILE: &str = "astra-launcher-ledger.lock";
pub(crate) const BACKUP_DIR: &str = "astra-launcher-backup";
/// Written by [`crate::launch()`] right before it asks Steam to start a game: the runner's sign
/// that Astra started this launch. Never one of the ledger's files.
pub(crate) const PLAY_ARM_FILE: &str = "play-arm.json";
/// The player's answer to "play with your mods?" for this game ([`crate::set_mods_choice`]).
/// Never one of the ledger's files.
pub(crate) const MODS_CHOICE_FILE: &str = "mods-choice.json";
/// Astra Link's discovery file (marketplace plan A§3.1). Reserved now, before its writer ships
/// (it waits for `link-v1` to freeze), so no integration installed meanwhile can hold the name.
pub(crate) const LINK_FILE: &str = "astra-link.json";
const TEMP_MARK: &str = ".astra-tmp-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Root {
    Profile,
    Game,
}

/// A destination inside one of the two roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JailPath {
    pub root: Root,
    /// Path components below the root, each one checked by [`check_component`].
    pub rel: Vec<String>,
    /// Written with a trailing `/`: a folder that files go into.
    pub is_dir: bool,
}

fn escapes(path: &str) -> LauncherError {
    LauncherError::new(codes::PATH_ESCAPES).with("path", path)
}

/// The name inside the first `${…}` of `s`, for an `unknown-placeholder` refusal.
pub(crate) fn first_placeholder_name(s: &str) -> String {
    let after = s.split_once("${").map(|(_, a)| a).unwrap_or("");
    after.split('}').next().unwrap_or("").to_owned()
}

/// Reads `${profile}/a/b` or `${game}/a/b`.
pub(crate) fn parse_jailed(s: &str) -> Result<JailPath> {
    let (root, rest) = if let Some(r) = s.strip_prefix("${profile}") {
        (Root::Profile, r)
    } else if let Some(r) = s.strip_prefix("${game}") {
        (Root::Game, r)
    } else if s.starts_with("${") {
        return Err(
            LauncherError::new(codes::UNKNOWN_PLACEHOLDER).with("name", first_placeholder_name(s))
        );
    } else {
        return Err(escapes(s));
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return Err(escapes(s));
    }
    let rest = rest.strip_prefix('/').unwrap_or("");
    if rest.contains("${") {
        return Err(LauncherError::new(codes::UNKNOWN_PLACEHOLDER)
            .with("name", first_placeholder_name(rest)));
    }
    let is_dir = rest.is_empty() || rest.ends_with('/');
    let body = rest.strip_suffix('/').unwrap_or(rest);
    let mut rel = Vec::new();
    if !body.is_empty() {
        for c in body.split('/') {
            if !check_component(c) {
                return Err(escapes(s));
            }
            rel.push(c.to_owned());
        }
    }
    if root == Root::Profile && rel.first().is_some_and(|c| is_reserved(c)) {
        return Err(escapes(s).with("reason", "reserved"));
    }
    Ok(JailPath { root, rel, is_dir })
}

/// The profile's own files ([`crate::reserved::PROFILE_FILE_NAMES`]: the ledger, its lock, the
/// backups, the play-arm file, the mods choice, `astra-link.json`). Case-insensitive.
pub(crate) fn is_reserved(first: &str) -> bool {
    crate::reserved::profile_name_is_reserved(first)
}

/// One path component that is safe on every platform the launcher serves: no separators, no
/// `.`/`..`, no drive or stream colon, no Windows device name, no trailing dot or space.
pub(crate) fn check_component(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.len() > 255 || c.contains(TEMP_MARK) {
        return false;
    }
    if c.chars().any(|ch| {
        ch < ' '
            || ch == '\u{7f}'
            || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/')
    }) {
        return false;
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return false;
    }
    let stem = c
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    // `COM0`-`COM9` and `LPT0`-`LPT9`, and Windows reads the superscripts `¹²³` as digits there
    // too (`COM¹` is a device).
    let numbered = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|n| {
            let mut c = n.chars();
            matches!(
                (c.next(), c.next()),
                (Some('0'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}'), None)
            )
        });
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered;
    !device
}

/// A relative path written by a caller or a manifest, such as the game's exe: `a/b/c.exe`.
/// Accepts `\` as a separator too. Returns the checked components.
pub(crate) fn relative_components(s: &str) -> Option<Vec<String>> {
    let s = s.replace('\\', "/");
    if s.is_empty() || s.starts_with('/') {
        return None;
    }
    let mut out = Vec::new();
    for c in s.split('/') {
        if !check_component(c) {
            return None;
        }
        out.push(c.to_owned());
    }
    Some(out)
}

/// A root folder that must exist, in canonical form.
pub(crate) fn canonical_dir(p: &Path) -> Result<PathBuf> {
    let c = fs::canonicalize(p).map_err(|e| LauncherError::io_at(&e, p))?;
    if !c.is_dir() {
        return Err(LauncherError::io("not a directory").with("path", p.display().to_string()));
    }
    Ok(c)
}

/// Re-checks a recorded relative path before acting on it (a ledger is a file on disk).
pub(crate) fn checked_rel(rel: &str) -> Result<Vec<String>> {
    relative_components(rel)
        .filter(|c| !rel.contains('\\') && !c.is_empty())
        .ok_or_else(|| escapes(rel))
}

/// Creates the parent folders of `rel` below the canonical `root`, refusing every link on the way,
/// re-checks the jail on the resolved parent, and returns the target path. The target itself
/// must be absent or a regular file.
pub(crate) fn prepare_target(root: &Path, rel: &[String]) -> Result<PathBuf> {
    let Some((last, parents)) = rel.split_last() else {
        return Err(escapes(""));
    };
    let mut cur = root.to_path_buf();
    for c in parents {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(escapes(&cur.display().to_string())),
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(
                    LauncherError::io("not a directory").with("path", cur.display().to_string())
                );
            }
            Err(e) if e.kind() == ErrorKind::NotFound => match fs::create_dir(&cur) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let m =
                        fs::symlink_metadata(&cur).map_err(|e| LauncherError::io_at(&e, &cur))?;
                    if !m.is_dir() || m.file_type().is_symlink() {
                        return Err(escapes(&cur.display().to_string()));
                    }
                }
                Err(e) => return Err(LauncherError::io_at(&e, &cur)),
            },
            Err(e) => return Err(LauncherError::io_at(&e, &cur)),
        }
    }
    let resolved = fs::canonicalize(&cur).map_err(|e| LauncherError::io_at(&e, &cur))?;
    if !resolved.starts_with(root) {
        return Err(escapes(&cur.display().to_string()));
    }
    let target = resolved.join(last);
    check_regular_or_absent(&target)?;
    Ok(target)
}

/// The path of `rel` below the canonical `root` when every parent folder exists as a real
/// folder (no link anywhere on the way). `None` when a parent is missing. Creates nothing.
pub(crate) fn existing_target(root: &Path, rel: &[String]) -> Result<Option<PathBuf>> {
    let Some((last, parents)) = rel.split_last() else {
        return Err(escapes(""));
    };
    let mut cur = root.to_path_buf();
    for c in parents {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(escapes(&cur.display().to_string())),
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LauncherError::io_at(&e, &cur)),
        }
    }
    Ok(Some(cur.join(last)))
}

/// The real spelling of `rel`'s last component below `root` when only a case variant of it
/// exists: Windows and Wine treat the two as one file. Returns `rel` unchanged otherwise.
pub(crate) fn real_case(root: &Path, rel: &[String]) -> Result<Vec<String>> {
    let Some(path) = existing_target(root, rel)? else {
        return Ok(rel.to_vec());
    };
    if fs::symlink_metadata(&path).is_ok() {
        return Ok(rel.to_vec());
    }
    let (Some(parent), Some(last)) = (path.parent(), rel.last()) else {
        return Ok(rel.to_vec());
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Ok(rel.to_vec());
    };
    for e in entries.flatten() {
        if let Some(n) = e.file_name().to_str()
            && n.eq_ignore_ascii_case(last)
            && check_component(n)
        {
            let mut out = rel.to_vec();
            if let Some(l) = out.last_mut() {
                *l = n.to_owned();
            }
            return Ok(out);
        }
    }
    Ok(rel.to_vec())
}

/// Resolves `p` as far as it exists (links included) and appends the rest, for comparing two
/// folders that may not exist yet.
pub(crate) fn resolve_lenient(p: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(p).map_err(|e| LauncherError::io_at(&e, p))?;
    let mut existing = abs.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = fs::canonicalize(&existing) {
            let mut out = c;
            for r in rest.iter().rev() {
                if r == ".." {
                    return Err(escapes(&p.display().to_string()));
                }
                if r != "." {
                    out.push(r);
                }
            }
            return Ok(out);
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return Ok(abs),
        }
    }
}

/// Refuses a link or a folder at `p`.
pub(crate) fn check_regular_or_absent(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(escapes(&p.display().to_string())),
        Ok(m) if !m.is_file() => {
            Err(escapes(&p.display().to_string()).with("reason", "not-a-file"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LauncherError::io_at(&e, p)),
    }
}

/// What sits at a path, without following a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileState {
    Absent,
    File { sha256: String },
}

pub(crate) fn file_state(p: &Path) -> Result<FileState> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(escapes(&p.display().to_string())),
        Ok(m) if m.is_file() => Ok(FileState::File {
            sha256: sha256_file(p).map_err(|e| LauncherError::io_at(&e, p))?,
        }),
        Ok(_) => Err(escapes(&p.display().to_string()).with("reason", "not-a-file")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(FileState::Absent),
        Err(e) => Err(LauncherError::io_at(&e, p)),
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `target` through a temporary file in the same folder and a rename, so a
/// reader sees the old file or the new one, never half of one. A link at `target` is replaced,
/// never followed. Keeps the old file's permissions when there was one.
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_as(target, bytes, false)
}

/// [`write_atomic`] for a file that holds a secret: on Unix the file is mode `0600` — the
/// temporary file is created so and set so before the rename, and an existing file's looser mode
/// is not carried over (the new file replaces it). Elsewhere it is exactly [`write_atomic`],
/// attributes carried over and all: on Windows the file's privacy rests on the ACL its folder
/// inherits (a profile is below the user's own data folder), which the launcher neither sets nor
/// checks.
///
/// Used for every `[[config_writes]]` entry that sets a caller's variable (anything but
/// `${profile}` and `${game}`; `link-v1.md` §2's "Mode 0600 on Unix" for the same kind of file).
/// The decision: the launcher cannot tell which caller variable is a secret, and every one today
/// is a bridge credential (`${bridge_token}`, and `${bridge_port}` beside it), so every write that
/// carries one is private. The game runs as the same user, so nothing that read the file loses
/// access. [`is_private`] says whether a file left unchanged still needs tightening.
pub(crate) fn write_atomic_private(target: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_as(target, bytes, true)
}

/// Whether the file at `target` is readable by its owner alone (mode `0600` or stricter on Unix),
/// or there is none to tighten; always `true` elsewhere.
pub(crate) fn is_private(target: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::symlink_metadata(target) {
            Ok(m) => m.permissions().mode() & 0o077 == 0,
            Err(_) => true,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        true
    }
}

fn write_atomic_as(target: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| escapes(&target.display().to_string()))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}{TEMP_MARK}{}-{n}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            // Never readable by anyone else, not even for the moment before the bytes land.
            open.mode(0o600);
        }
        let mut f = open.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // A private file never takes over a looser mode on Unix; elsewhere `private` changes
        // nothing.
        if (!private || cfg!(not(unix)))
            && let Ok(m) = fs::symlink_metadata(target)
            && m.is_file()
        {
            let _ = fs::set_permissions(&tmp, m.permissions());
        }
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::PermissionsExt;
            // Exactly 0600, whatever the umask took away at creation.
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, target)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(LauncherError::io_at(&e, target));
    }
    Ok(())
}

/// A path as the user and the game see it: no `\\?\` prefix on Windows.
pub(crate) fn plain(p: &Path) -> PathBuf {
    if cfg!(windows)
        && let Some(s) = without_verbatim(&p.to_string_lossy())
    {
        return PathBuf::from(s);
    }
    p.to_path_buf()
}

/// A Windows path's spelling without its verbatim `\\?\` prefix (`\\?\C:\x` → `C:\x`,
/// `\\?\UNC\h\s` → `\\h\s`); `None` when it has none. A pure string function, so that it is
/// tested on every platform; only Windows applies it ([`plain`]).
pub(crate) fn without_verbatim(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    s.strip_prefix(r"\\?\").map(str::to_owned)
}

/// Why a path is not one the launcher may touch: a share, a device, a verbatim namespace, or not
/// absolute. On Windows merely asking about `\\host\share\…` (`canonicalize`, `metadata`) opens an
/// SMB session with the user's credentials, and `\\.\pipe\…` connects to a pipe: such a path is
/// refused before ANY filesystem call. What passes on Windows is a drive-letter path (`C:\…`,
/// `\\?\C:\…`); a mapped network drive or a `subst` behind the letter is the user's own setting,
/// and is followed.
pub(crate) fn prefix_verdict(
    prefix: Option<Prefix<'_>>,
    rooted: bool,
) -> std::result::Result<(), &'static str> {
    match prefix {
        Some(Prefix::Disk(_) | Prefix::VerbatimDisk(_)) if rooted => Ok(()),
        // `C:x`: relative to that drive's current folder.
        Some(Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => Err("not-absolute"),
        Some(Prefix::UNC(..)) => Err("unc"),
        Some(Prefix::VerbatimUNC(..)) => Err("verbatim-unc"),
        Some(Prefix::DeviceNS(_)) => Err("device"),
        Some(Prefix::Verbatim(_)) => Err("verbatim"),
        // Unix: from the root. Windows: `\x` is relative to the current drive.
        None if rooted && !cfg!(windows) => Ok(()),
        None => Err("not-absolute"),
    }
}

/// [`prefix_verdict`] of `p`, from its components alone: no filesystem call.
pub(crate) fn local_verdict(p: &Path) -> std::result::Result<(), &'static str> {
    let mut c = p.components();
    match c.next() {
        Some(Component::Prefix(pc)) => prefix_verdict(
            Some(pc.kind()),
            matches!(c.next(), Some(Component::RootDir)),
        ),
        Some(Component::RootDir) => prefix_verdict(None, true),
        _ => prefix_verdict(None, false),
    }
}

/// `path-escapes` (`path`, `reason` = `unc`, `verbatim-unc`, `device`, `verbatim`,
/// `not-absolute`) unless `p` is an absolute drive-letter path on Windows, or from the root on
/// Unix ([`local_verdict`]). Call it before any filesystem call on a path a person or a file
/// named, and before stripping `\\?\` ([`local_root`]): `\\?\UNC\…` is `verbatim-unc`, never `unc`.
pub(crate) fn require_local(p: &Path) -> Result<()> {
    local_verdict(p).map_err(|reason| {
        LauncherError::new(codes::PATH_ESCAPES)
            .with("path", p.display().to_string())
            .with("reason", reason)
    })
}

/// A folder a person or a file named, checked ([`require_local`]) and THEN without its `\\?\`
/// prefix, as a person and `chain.ini` see it: the check sees the spelling given.
pub(crate) fn local_root(p: &Path) -> Result<PathBuf> {
    require_local(p)?;
    Ok(plain(p))
}

/// How a path written in a Windows file reads (a pack's `doorstop_config.ini`), whatever this
/// machine is: the text alone, `\` and `/` alike, the reasons of [`prefix_verdict`] for what is
/// refused. Under Proton the game is a Windows program, so this is how it would read the value;
/// on Linux `Path` parses no prefix at all, which is why it is a function of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowsText {
    /// `C:\…`, `C:/…`, `\\?\C:\…`: a drive-letter path, passed as written.
    Drive,
    /// `\x`, `/x`: from the root of the current drive (on this machine, `/x`).
    Rooted,
    /// Below the folder it is read beside.
    Relative,
}

/// [`WindowsText`] of `s`, or why it is refused: `unc` (`\\host\…`, `//host/…`, `/\host\…`),
/// `verbatim-unc` (`\\?\UNC\…`), `device` (`\\.\…`), `verbatim` (`\\?\` anything but a drive),
/// `not-absolute` (`C:x`, relative to that drive's current folder).
pub(crate) fn windows_text_verdict(s: &str) -> std::result::Result<WindowsText, &'static str> {
    let b = s.as_bytes();
    let sep = |i: usize| b.get(i).is_some_and(|c| matches!(c, b'\\' | b'/'));
    let drive =
        |i: usize| b.get(i).is_some_and(u8::is_ascii_alphabetic) && b.get(i + 1) == Some(&b':');
    if sep(0) && sep(1) {
        return match b.get(2) {
            Some(b'?') if sep(3) => {
                let rest = &s[4..];
                if rest
                    .as_bytes()
                    .get(..3)
                    .is_some_and(|u| u.eq_ignore_ascii_case(b"UNC"))
                    && sep(7)
                {
                    Err("verbatim-unc")
                } else if drive(4) && (sep(6) || b.len() == 6) {
                    Ok(WindowsText::Drive)
                } else {
                    Err("verbatim")
                }
            }
            Some(b'.') if sep(3) => Err("device"),
            _ => Err("unc"),
        };
    }
    if drive(0) {
        return if sep(2) {
            Ok(WindowsText::Drive)
        } else {
            Err("not-absolute")
        };
    }
    if sep(0) {
        return Ok(WindowsText::Rooted);
    }
    Ok(WindowsText::Relative)
}

/// An absolute form of `p` that keeps the caller's spelling (no link resolution).
pub(crate) fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p)
        .map(|a| plain(&a))
        .map_err(|e| LauncherError::io_at(&e, p))
}

/// What Windows (or Wine) opens for `rel` below `base`: each component in any letter case, `.`
/// and `..` taken lexically (as `GetFullPathName` does), links followed. A read-only probe for
/// what a loader beside the game will find, never a destination: nothing is written through it.
pub(crate) fn find_any_case(base: &Path, rel: &[&str]) -> Option<PathBuf> {
    let mut cur = base.to_path_buf();
    for c in rel {
        match *c {
            "" | "." => continue,
            ".." => {
                cur.pop();
                continue;
            }
            _ => {}
        }
        let exact = cur.join(c);
        if fs::metadata(&exact).is_ok() {
            cur = exact;
            continue;
        }
        let entry = fs::read_dir(&cur).ok()?.flatten().find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(c))
        })?;
        cur = entry.path();
    }
    fs::metadata(&cur).is_ok().then_some(cur)
}

/// Removes empty folders from `dir` up to (not including) `stop`.
pub(crate) fn prune_empty_dirs(mut dir: PathBuf, stop: &Path) {
    while dir.starts_with(stop) && dir != stop {
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        if !dir.pop() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jail_accepts_the_two_roots_only() {
        let p = parse_jailed("${profile}/BepInEx/core/").unwrap();
        assert_eq!(p.root, Root::Profile);
        assert_eq!(p.rel, vec!["BepInEx", "core"]);
        assert!(p.is_dir);
        let g = parse_jailed("${game}/winhttp.dll").unwrap();
        assert_eq!(g.root, Root::Game);
        assert!(!g.is_dir);
        for bad in [
            "/etc/passwd",
            "C:/Windows/x.dll",
            "${profile}/../x",
            "${profile}/a/../../x",
            "${profile}x/a",
            "${game}/a\\..\\b",
            "${game}/C:x",
            "${game}/NUL",
            "${game}/a//b",
            "${game}/./a",
            "BepInEx/x",
            "${profile}/astra-launcher-ledger.json",
            "${profile}/astra-launcher-backup/x",
            "${profile}/play-arm.json",
            "${profile}/Mods-Choice.json",
        ] {
            assert_eq!(
                parse_jailed(bad).unwrap_err().code,
                codes::PATH_ESCAPES,
                "{bad}"
            );
        }
        for bad in ["${home}/x", "${profile}/${bridge_port}/x"] {
            assert_eq!(
                parse_jailed(bad).unwrap_err().code,
                codes::UNKNOWN_PLACEHOLDER,
                "{bad}"
            );
        }
    }

    #[test]
    fn only_a_local_drive_or_the_root_is_a_place_to_look() {
        use std::ffi::OsStr;
        let h = OsStr::new("host");
        let sh = OsStr::new("share");
        assert_eq!(prefix_verdict(Some(Prefix::Disk(b'C')), true), Ok(()));
        assert_eq!(
            prefix_verdict(Some(Prefix::VerbatimDisk(b'C')), true),
            Ok(())
        );
        assert_eq!(
            prefix_verdict(Some(Prefix::Disk(b'C')), false),
            Err("not-absolute")
        );
        assert_eq!(prefix_verdict(Some(Prefix::UNC(h, sh)), true), Err("unc"));
        assert_eq!(
            prefix_verdict(Some(Prefix::VerbatimUNC(h, sh)), true),
            Err("verbatim-unc")
        );
        assert_eq!(
            prefix_verdict(Some(Prefix::DeviceNS(OsStr::new("pipe"))), true),
            Err("device")
        );
        assert_eq!(
            prefix_verdict(Some(Prefix::Verbatim(OsStr::new("GLOBALROOT"))), true),
            Err("verbatim")
        );
        assert_eq!(
            prefix_verdict(None, true),
            if cfg!(windows) {
                Err("not-absolute")
            } else {
                Ok(())
            }
        );
        assert_eq!(prefix_verdict(None, false), Err("not-absolute"));
        assert_eq!(local_verdict(Path::new("relative/x")), Err("not-absolute"));
        if cfg!(windows) {
            for (p, want) in [
                (r"C:\Games\x", Ok(())),
                (r"\\?\C:\Games\x", Ok(())),
                (r"\\host\share\x", Err("unc")),
                (r"\\?\UNC\host\share\x", Err("verbatim-unc")),
                (r"\\.\pipe\x", Err("device")),
                (r"\\?\GLOBALROOT\x", Err("verbatim")),
            ] {
                assert_eq!(local_verdict(Path::new(p)), want, "{p}");
            }
        } else {
            assert_eq!(local_verdict(Path::new("/home/u/x")), Ok(()));
            // No prefixes on Unix: a share's spelling is a relative name here.
            assert_eq!(
                local_verdict(Path::new(r"\\host\share")),
                Err("not-absolute")
            );
        }
        assert_eq!(without_verbatim(r"\\?\C:\x").as_deref(), Some(r"C:\x"));
        assert_eq!(
            without_verbatim(r"\\?\UNC\h\s\x").as_deref(),
            Some(r"\\h\s\x")
        );
        assert_eq!(without_verbatim(r"C:\x"), None);
    }

    #[test]
    fn a_windows_path_in_a_file_is_judged_by_its_text_on_every_machine() {
        use WindowsText::*;
        for (s, want) in [
            (r"C:\Games\x", Ok(Drive)),
            ("c:/games/x", Ok(Drive)),
            (r"\\?\C:\Games\x", Ok(Drive)),
            (r"\\?\c:", Ok(Drive)),
            (r"\\host\share\x", Err("unc")),
            ("//host/share/x", Err("unc")),
            (r"/\host\share", Err("unc")),
            (r"\/host/share", Err("unc")),
            (r"\\?\UNC\host\share\x", Err("verbatim-unc")),
            (r"//?/unc/host/share", Err("verbatim-unc")),
            (r"\\.\pipe\x", Err("device")),
            ("//./COM1", Err("device")),
            (r"\\?\GLOBALROOT\Device\x", Err("verbatim")),
            (r"\\?\", Err("verbatim")),
            ("C:x", Err("not-absolute")),
            (r"\x", Ok(Rooted)),
            ("/home/u/x", Ok(Rooted)),
            (r"BepInEx\unstripped", Ok(Relative)),
            ("unstripped corlib", Ok(Relative)),
        ] {
            assert_eq!(windows_text_verdict(s), want, "{s}");
        }
    }

    #[test]
    fn components_refuse_windows_traps() {
        for bad in [
            "",
            ".",
            "..",
            "a:b",
            "CON",
            "com1.txt",
            "lpt9",
            "x.",
            "x ",
            "a*b",
            "a\u{0}b",
            "COM0",
            "lpt0.log",
            "COM\u{00B9}",
            "com\u{00B2}.txt",
            "LPT\u{00B3}",
        ] {
            assert!(!check_component(bad), "{bad:?}");
        }
        for good in [
            "Lethal Company.exe",
            "winhttp.dll",
            "BepInEx",
            "com10",
            "a.b.c",
            "COM\u{2074}",
            "comx",
        ] {
            assert!(check_component(good), "{good:?}");
        }
    }
}
