// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The path jail. A manifest names a destination as `${profile}/…` or `${game}/…`; nothing else
//! is a destination. The relative part is checked lexically when the manifest is read, and again
//! on disk — after parent directories are created and links resolved — before every write.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::digest::sha256_file;
use crate::error::{LauncherError, Result, codes};

/// The profile's own bookkeeping names. A manifest may not write them.
pub(crate) const LEDGER_FILE: &str = "astra-launcher-ledger.json";
pub(crate) const LOCK_FILE: &str = "astra-launcher-ledger.lock";
pub(crate) const BACKUP_DIR: &str = "astra-launcher-backup";
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

fn is_reserved(first: &str) -> bool {
    let f = first.to_ascii_lowercase();
    f == LEDGER_FILE || f == LOCK_FILE || f == BACKUP_DIR
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
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit());
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
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Ok(m) = fs::symlink_metadata(target)
            && m.is_file()
        {
            let _ = fs::set_permissions(&tmp, m.permissions());
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
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

/// An absolute form of `p` that keeps the caller's spelling (no link resolution).
pub(crate) fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p)
        .map(|a| plain(&a))
        .map_err(|e| LauncherError::io_at(&e, p))
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
    fn components_refuse_windows_traps() {
        for bad in [
            "", ".", "..", "a:b", "CON", "com1.txt", "lpt9", "x.", "x ", "a*b", "a\u{0}b",
        ] {
            assert!(!check_component(bad), "{bad:?}");
        }
        for good in [
            "Lethal Company.exe",
            "winhttp.dll",
            "BepInEx",
            "com10",
            "a.b.c",
        ] {
            assert!(check_component(good), "{good:?}");
        }
    }
}
