// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Steam as the Flathub Flatpak (`com.valvesoftware.Steam`, 0.5.3).
//!
//! Flathub's Steam has no access to the user's home, and Doorstop's target is
//! `${profile}/BepInEx/core/BepInEx.Preloader.dll`, in the caller's data folder: a game Steam
//! starts from its sandbox would never see the integration. So [`crate::launch()`], immediately
//! before it spawns a plan whose Steam is the Flatpak ([`crate::ProcessPlan::sandbox_grant`]),
//! grants Steam's sandbox read/write access to exactly that profile folder (BepInEx writes its
//! log there) in the user's Flatpak overrides, and [`crate::uninstall`] takes it away again.
//!
//! - **The overrides file** is a GKeyFile, `$FLATPAK_USER_DIR/overrides/com.valvesoftware.Steam`,
//!   else under `$XDG_DATA_HOME/flatpak`, else `~/.local/share/flatpak` ([`user_overrides_file`]).
//!   The launcher adds one entry, `<canonical profile folder>:rw`, to `[Context] filesystems` and
//!   leaves every other byte of the file as it was: other entries, keys, groups, comments, order.
//!   A folder already listed (any mode) is left alone and recorded as the user's
//!   (`preexisting`); one the user denies (`!<folder>`) is refused, never overridden.
//! - **What was granted** is recorded in the profile, `flatpak-grant.json` ([`FlatpakGrant`]), a
//!   profile file no manifest may write ([`crate::reserved::PROFILE_FILE_NAMES`]).
//! - **Revoke** ([`crate::uninstall`], before the profile's files go): when the record says the
//!   launcher added the entry, exactly that token is removed, every other entry left as it is
//!   (the user's `!/x`, `/y:ro`, another spelling of another folder). A list left empty drops its
//!   `filesystems=` line and keeps the group. Never a reason for an uninstall to fail: what could
//!   not be undone is the warning [`FLATPAK_REVOKE_FAILED`].
//! - **Steam's own permissions** are read before anything is written: the game reaches the
//!   companion over a localhost WebSocket and a shared-memory ring, so Steam's sandbox must have
//!   both `network` and `ipc` (`flatpak info --show-permissions`, what Flatpak says now, never a
//!   manifest's claim). Missing, or not readable: `steam-flatpak-unusable`.
//!
//! Every refusal is `steam-flatpak-unusable` with `reason`: `permission` (`missing` =
//! `network`, `ipc` or `network,ipc`), `flatpak-info` (the command failed or did not answer),
//! `unparseable` (its output is no GKeyFile), `path` (the profile folder is not a folder, not
//! its canonical spelling — a link anywhere on the way — or holds a character a GKeyFile list
//! would need escaped, or is the home folder or above it), `denied` (the user's overrides deny
//! the folder), `overrides` (the overrides file cannot be found, read or written; `path`).
//!
//! Every function takes the overrides file and the `flatpak` program as parameters; only
//! [`user_overrides_file`] reads the environment.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::paths::{FLATPAK_GRANT_FILE, check_regular_or_absent, plain, write_atomic};

/// Flathub's Steam.
pub const STEAM_FLATPAK_APP: &str = "com.valvesoftware.Steam";

/// [`crate::UninstallReport::warnings`]: the entry the launcher added to Steam's Flatpak
/// overrides could not be taken out again (the overrides file could not be read or written, or
/// the profile's `flatpak-grant.json` could not be read or names another folder). Steam's
/// sandbox may still reach the removed profile's folder; `flatpak override --user --reset
/// com.valvesoftware.Steam` or an edit of the file removes it. Never a refusal.
pub const FLATPAK_REVOKE_FAILED: &str = "flatpak-revoke-failed";

/// The most of an overrides file or of `flatpak`'s output read: far more than any real one.
const MAX_TEXT: u64 = 1024 * 1024;

/// How long a `flatpak` command may take before it is taken for a failure.
const FLATPAK_DEADLINE: Duration = Duration::from_secs(10);

/// One overrides file is shared by every profile: its read-modify-write is serialised in this
/// process (the daemon). Another process editing it at the same instant is not excluded.
static OVERRIDES_EDIT: Mutex<()> = Mutex::new(());

/// What [`grant_profile_access`] did, as `<profile>/flatpak-grant.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatpakGrant {
    /// Always [`STEAM_FLATPAK_APP`].
    pub app: String,
    /// The `filesystems` token: `<canonical profile folder>:rw`.
    pub entry: String,
    /// The overrides file it went into.
    pub overrides: PathBuf,
    /// The folder was listed already (the user's own entry): nothing was written, and revoke
    /// takes nothing out.
    pub preexisting: bool,
}

fn unusable(reason: &str) -> LauncherError {
    LauncherError::new(codes::STEAM_FLATPAK_UNUSABLE).with("reason", reason)
}

fn overrides_error(path: &Path) -> LauncherError {
    unusable("overrides").with("path", path.display().to_string())
}

/// The user's overrides file for Flathub's Steam: `$FLATPAK_USER_DIR/overrides/…`, else
/// `$XDG_DATA_HOME/flatpak/overrides/…`, else `~/.local/share/flatpak/overrides/…`. A relative or
/// empty variable is ignored, as Flatpak and the XDG specification ignore it. `None` on Windows,
/// and with no home folder.
pub fn user_overrides_file() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    overrides_file_from(
        std::env::var_os("FLATPAK_USER_DIR"),
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`user_overrides_file`] from those three variables' values.
pub(crate) fn overrides_file_from(
    flatpak_user_dir: Option<OsString>,
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let absolute = |v: Option<OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    let base = absolute(flatpak_user_dir)
        .or_else(|| absolute(xdg_data_home).map(|d| d.join("flatpak")))
        .or_else(|| absolute(home).map(|h| h.join(".local/share/flatpak")))?;
    Some(base.join("overrides").join(STEAM_FLATPAK_APP))
}

/// Runs `program args…` with no input, its error output discarded, for at most
/// [`FLATPAK_DEADLINE`]: whether it exited 0, and its output (at most [`MAX_TEXT`]). `None` when
/// it could not be started or did not finish in time (it is killed).
fn run(program: &Path, args: &[&str]) -> Option<(bool, Vec<u8>)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A program written a moment ago can be busy for as long as another thread's fork holds it.
    let mut tries = 0;
    let mut child = loop {
        match cmd.spawn() {
            Ok(c) => break c,
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy && tries < 5 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    };
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut out).take(MAX_TEXT).read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    let deadline = Instant::now() + FLATPAK_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let bytes = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()?;
    Some((status.success(), bytes))
}

/// Whether `flatpak info com.valvesoftware.Steam` (with this `flatpak` program) exits 0: Flathub's
/// Steam is installed, for the user or system-wide.
pub fn steam_flatpak_installed(flatpak: &Path) -> bool {
    run(flatpak, &["info", STEAM_FLATPAK_APP]).is_some_and(|(ok, _)| ok)
}

/// `steam-flatpak-unusable` unless Steam's sandbox has both `network` and `ipc`, as
/// `flatpak info --show-permissions com.valvesoftware.Steam` (this `flatpak` program) says now:
/// `reason` = `flatpak-info` when it fails or does not answer, else [`parse_steam_permissions`].
pub fn check_steam_permissions(flatpak: &Path) -> Result<()> {
    let Some((true, out)) = run(flatpak, &["info", "--show-permissions", STEAM_FLATPAK_APP]) else {
        return Err(unusable("flatpak-info"));
    };
    let text = String::from_utf8(out).map_err(|_| unusable("unparseable"))?;
    parse_steam_permissions(&text)
}

/// Reads `flatpak info --show-permissions`' output (a GKeyFile): `Ok` when `[Context] shared`
/// lists both `network` and `ipc`; `steam-flatpak-unusable` with `reason` = `permission` and
/// `missing` (`network`, `ipc`, `network,ipc`) when one is not; `reason` = `unparseable` for a
/// text that is no GKeyFile (no group, or a line that is neither a group, a key, a comment nor
/// blank).
pub fn parse_steam_permissions(text: &str) -> Result<()> {
    let mut groups = 0;
    let mut group = String::new();
    let mut shared: Option<String> = None;
    for (line, _) in lines(text) {
        match classify(line) {
            Kind::Blank => {}
            Kind::Group(name) => {
                groups += 1;
                group = name.to_owned();
            }
            Kind::Key { key, value } if groups > 0 => {
                if group == "Context" && key == "shared" {
                    shared = Some(value.to_owned());
                }
            }
            Kind::Key { .. } | Kind::Invalid => return Err(unusable("unparseable")),
        }
    }
    if groups == 0 {
        return Err(unusable("unparseable"));
    }
    let shared = shared.unwrap_or_default();
    let has = |want: &str| tokens(&shared).iter().any(|&(s, e)| &shared[s..e] == want);
    let missing: Vec<&str> = ["network", "ipc"].into_iter().filter(|w| !has(w)).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(unusable("permission").with("missing", missing.join(",")))
    }
}

/// Grants Flathub's Steam read/write access to exactly `profile` (the plan's own spelling of its
/// canonical profile folder) in the overrides file `overrides`, after `flatpak`'s
/// [`check_steam_permissions`], and records it in `<profile>/flatpak-grant.json`. The module docs
/// say what is written and what is refused; nothing is written before every check passed.
pub fn grant_profile_access(
    profile: &Path,
    overrides: &Path,
    flatpak: &Path,
) -> Result<FlatpakGrant> {
    check_steam_permissions(flatpak)?;
    let entry_path = checked_profile(profile)?;
    let entry = format!("{entry_path}:rw");
    let record_path = profile.join(FLATPAK_GRANT_FILE);
    check_regular_or_absent(&record_path)?;

    let _edit = OVERRIDES_EDIT.lock().unwrap_or_else(|p| p.into_inner());
    let target = overrides_target(overrides)?;
    let text = read_overrides(&target)?.unwrap_or_default();
    let preexisting = match listing(&text, &entry_path) {
        Listing::Denied => return Err(unusable("denied").with("path", entry_path)),
        Listing::Absent => {
            let new = append_entry(&text, &entry);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|_| overrides_error(&target))?;
            }
            write_atomic(&target, new.as_bytes()).map_err(|_| overrides_error(&target))?;
            false
        }
        // Ours from an earlier Play (our record says so, and our exact token is there): still
        // ours, so uninstall still takes it out.
        Listing::Listed
            if read_record(profile).is_some_and(|r| {
                !r.preexisting && r.entry == entry && has_token(&text, &entry)
            }) =>
        {
            false
        }
        Listing::Listed => true,
    };
    let grant = FlatpakGrant {
        app: STEAM_FLATPAK_APP.to_owned(),
        entry,
        overrides: overrides.to_path_buf(),
        preexisting,
    };
    let json = serde_json::to_vec(&grant).map_err(LauncherError::io)?;
    write_atomic(&record_path, &json)?;
    Ok(grant)
}

/// Takes the entry `<profile>/flatpak-grant.json` records out of the overrides file it names,
/// when the launcher added it (`preexisting` false), then removes the record. `profile` is the
/// canonical profile folder. Nothing recorded, an overrides file that is gone, or an entry that is
/// gone already: nothing to do. [`FLATPAK_REVOKE_FAILED`] when it could not be done (the record
/// is kept then); never an error.
pub(crate) fn revoke_recorded(profile: &Path) -> Option<&'static str> {
    let record_path = profile.join(FLATPAK_GRANT_FILE);
    match fs::symlink_metadata(&record_path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Ok(m) if m.is_file() => {}
        _ => return Some(FLATPAK_REVOKE_FAILED),
    }
    let Some(record) = read_record(profile) else {
        return Some(FLATPAK_REVOKE_FAILED);
    };
    let done = || {
        let _ = fs::remove_file(&record_path);
        None
    };
    if record.preexisting {
        return done();
    }
    // Only this profile's own entry, in a file that is an overrides file of Steam's: a record
    // edited to name anything else takes nothing out.
    let mine = plain(profile)
        .to_str()
        .is_some_and(|p| record.entry == format!("{p}:rw"));
    let names_steams = record.app == STEAM_FLATPAK_APP
        && record.overrides.is_absolute()
        && record.overrides.file_name() == Some(OsStr::new(STEAM_FLATPAK_APP))
        && record
            .overrides
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "overrides");
    if !mine || !names_steams {
        return Some(FLATPAK_REVOKE_FAILED);
    }
    let _edit = OVERRIDES_EDIT.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(target) = overrides_target(&record.overrides) else {
        return Some(FLATPAK_REVOKE_FAILED);
    };
    let text = match read_overrides(&target) {
        Ok(Some(t)) => t,
        Ok(None) => return done(),
        Err(_) => return Some(FLATPAK_REVOKE_FAILED),
    };
    let Some(new) = remove_entry(&text, &record.entry) else {
        return done();
    };
    if write_atomic(&target, new.as_bytes()).is_err() {
        return Some(FLATPAK_REVOKE_FAILED);
    }
    done()
}

/// `<profile>/flatpak-grant.json`, when it is there and reads as one.
fn read_record(profile: &Path) -> Option<FlatpakGrant> {
    let path = profile.join(FLATPAK_GRANT_FILE);
    if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let bytes = fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The text a grant names `profile` by: its own spelling, which must be its canonical one (no
/// link anywhere on the way), a folder, UTF-8, absolute, with no character a GKeyFile list or a
/// Flatpak filesystem entry would need escaped (`;`, `\`, `=`, `[`, `]`, `:`, a control
/// character, a space at either end), and neither `/` nor the home folder or above it.
fn checked_profile(profile: &Path) -> Result<String> {
    let path_refused = || unusable("path").with("path", profile.display().to_string());
    let text = profile.to_str().ok_or_else(path_refused)?;
    let escapes = text
        .chars()
        .any(|c| matches!(c, ';' | '\\' | '=' | '[' | ']' | ':') || c.is_control());
    if escapes || text.starts_with(' ') || text.ends_with(' ') || !profile.is_absolute() {
        return Err(path_refused());
    }
    let canonical = fs::canonicalize(profile).map_err(|_| path_refused())?;
    if plain(&canonical) != profile || !canonical.is_dir() {
        return Err(path_refused());
    }
    if canonical.parent().is_none() {
        return Err(path_refused());
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(home) = fs::canonicalize(&home)
        && home.starts_with(&canonical)
    {
        return Err(path_refused());
    }
    Ok(text.to_owned())
}

/// The file an edit of `overrides` writes: the file itself, or what a link in its place points
/// at (a dotfile manager's), so the link stays.
fn overrides_target(overrides: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(overrides) {
        Ok(m) if m.file_type().is_symlink() => {
            fs::canonicalize(overrides).map_err(|_| overrides_error(overrides))
        }
        Ok(m) if !m.is_file() => Err(overrides_error(overrides)),
        _ => Ok(overrides.to_path_buf()),
    }
}

/// The overrides file's text; `None` when there is none.
fn read_overrides(path: &Path) -> Result<Option<String>> {
    let f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(overrides_error(path)),
    };
    let mut bytes = Vec::new();
    f.take(MAX_TEXT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| overrides_error(path))?;
    if bytes.len() as u64 > MAX_TEXT {
        return Err(overrides_error(path));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| overrides_error(path))
}

// --- The GKeyFile, as text. ---

/// Each line of `text` without its terminator, and its terminator (`\n`, `\r\n`, or none for a
/// last line without one). Joined back, they are `text` byte for byte.
fn lines(text: &str) -> Vec<(&str, &str)> {
    text.split_inclusive('\n')
        .map(|l| {
            if let Some(body) = l.strip_suffix("\r\n") {
                (body, "\r\n")
            } else if let Some(body) = l.strip_suffix('\n') {
                (body, "\n")
            } else {
                (l, "")
            }
        })
        .collect()
}

enum Kind<'a> {
    /// Empty, blank or a `#` comment.
    Blank,
    Group(&'a str),
    /// `key=value`: the key trimmed, the value from its first non-blank character.
    Key {
        key: &'a str,
        value: &'a str,
    },
    Invalid,
}

fn classify(line: &str) -> Kind<'_> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with('#') {
        return Kind::Blank;
    }
    if let Some(rest) = t.strip_prefix('[') {
        return match rest.trim_end().strip_suffix(']') {
            Some(name) => Kind::Group(name),
            None => Kind::Invalid,
        };
    }
    match line.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Kind::Key {
            key: k.trim(),
            value: v.trim_start_matches([' ', '\t']),
        },
        _ => Kind::Invalid,
    }
}

/// The byte ranges of a GKeyFile list's items in `value`: split at every `;` no `\` escapes; an
/// empty last piece (after a final `;`) is no item.
fn tokens(value: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            ';' => {
                out.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < value.len() {
        out.push((start, value.len()));
    }
    out
}

/// Where `[Context] filesystems` is: the index of the first `[Context]` header line, and the
/// index of the last `filesystems` key line inside any `[Context]` group (GKeyFile merges a group
/// written twice, and the last value of a key wins), with the byte offset of its value.
struct ContextList {
    header: Option<usize>,
    key: Option<(usize, usize)>,
}

fn context_list(lines: &[(&str, &str)]) -> ContextList {
    let mut header = None;
    let mut key = None;
    let mut in_context = false;
    for (i, (line, _)) in lines.iter().enumerate() {
        match classify(line) {
            Kind::Group(name) => {
                in_context = name == "Context";
                if in_context && header.is_none() {
                    header = Some(i);
                }
            }
            Kind::Key { key: k, value } if in_context && k == "filesystems" => {
                key = Some((i, line.len() - value.len()));
            }
            _ => {}
        }
    }
    ContextList { header, key }
}

/// `[Context] filesystems`' value in `text`, empty when there is none.
fn filesystems(text: &str) -> String {
    let ls = lines(text);
    match context_list(&ls).key {
        Some((i, at)) => ls[i].0[at..].to_owned(),
        None => String::new(),
    }
}

/// Whether `[Context] filesystems` holds exactly `token`.
fn has_token(text: &str, token: &str) -> bool {
    let v = filesystems(text);
    tokens(&v).iter().any(|&(s, e)| &v[s..e] == token)
}

#[derive(Debug, PartialEq, Eq)]
enum Listing {
    Absent,
    /// `path`, or `path:<mode>`.
    Listed,
    /// `!path` (or `!path:<mode>`): the user's own refusal.
    Denied,
}

/// What `[Context] filesystems` in `text` says about `path`, spelled exactly so: the last item
/// that names it decides, as Flatpak reads the list in order.
fn listing(text: &str, path: &str) -> Listing {
    let names = |t: &str| {
        t == path
            || t.strip_prefix(path)
                .and_then(|r| r.strip_prefix(':'))
                .is_some_and(|m| matches!(m, "rw" | "ro" | "create"))
    };
    let v = filesystems(text);
    let mut out = Listing::Absent;
    for (s, e) in tokens(&v) {
        let t = &v[s..e];
        if let Some(denied) = t.strip_prefix('!') {
            if names(denied) {
                out = Listing::Denied;
            }
        } else if names(t) {
            out = Listing::Listed;
        }
    }
    out
}

/// `text` with `entry` appended to `[Context] filesystems`: to the list's line when there is one,
/// on a new line right after the group's header when the group has no such key, and in a new
/// group at the end otherwise. Every other byte as it was.
fn append_entry(text: &str, entry: &str) -> String {
    let ls = lines(text);
    let at = context_list(&ls);
    let mut out = String::with_capacity(text.len() + entry.len() + 32);
    if let Some((k, start)) = at.key {
        for (i, (line, end)) in ls.iter().enumerate() {
            out.push_str(line);
            if i == k {
                let value = &line[start..];
                let closed =
                    value.is_empty() || tokens(value).last().is_none_or(|&(_, e)| e < value.len());
                if !closed {
                    out.push(';');
                }
                out.push_str(entry);
                out.push(';');
            }
            out.push_str(end);
        }
        return out;
    }
    if let Some(h) = at.header {
        for (i, (line, end)) in ls.iter().enumerate() {
            out.push_str(line);
            if i == h {
                let end = if end.is_empty() { "\n" } else { end };
                out.push_str(end);
                out.push_str("filesystems=");
                out.push_str(entry);
                out.push(';');
                // The new line ends as the header does (a header that ended the file gets `\n`).
                out.push_str(end);
            } else {
                out.push_str(end);
            }
        }
        return out;
    }
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("[Context]\nfilesystems=");
    out.push_str(entry);
    out.push_str(";\n");
    out
}

/// `text` without the item `entry` (every item spelled exactly so) in `[Context] filesystems`,
/// each with its own `;`; `None` when the list does not hold it. A list left empty loses its
/// line; the group stays. Every other byte as it was.
fn remove_entry(text: &str, entry: &str) -> Option<String> {
    let ls = lines(text);
    let (k, start) = context_list(&ls).key?;
    let (line, end) = ls[k];
    let value = &line[start..];
    let toks = tokens(value);
    if !toks.iter().any(|&(s, e)| &value[s..e] == entry) {
        return None;
    }
    let mut kept = String::with_capacity(value.len());
    let mut from = 0;
    for (i, &(s, e)) in toks.iter().enumerate() {
        if &value[s..e] != entry {
            continue;
        }
        // The item and the `;` after it; the last item with none takes the `;` before it.
        let (cut_s, cut_e) = if e < value.len() {
            (s, e + 1)
        } else if i > 0 {
            (s - 1, e)
        } else {
            (s, e)
        };
        let cut_s = cut_s.max(from);
        kept.push_str(&value[from..cut_s]);
        from = cut_e;
    }
    kept.push_str(&value[from..]);
    let mut out = String::with_capacity(text.len());
    for (i, (l, e)) in ls.iter().enumerate() {
        if i == k {
            if kept.trim().is_empty() {
                continue;
            }
            out.push_str(&l[..start]);
            out.push_str(&kept);
            out.push_str(end);
        } else {
            out.push_str(l);
            out.push_str(e);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: &str = "/data/astra/profiles/game:rw";

    #[test]
    fn appends_into_an_absent_file() {
        let new = append_entry("", ENTRY);
        assert_eq!(new, format!("[Context]\nfilesystems={ENTRY};\n"));
        assert_eq!(remove_entry(&new, ENTRY).unwrap(), "[Context]\n");
    }

    #[test]
    fn appends_into_a_file_with_no_context_group() {
        let seed = "[Session Bus Policy]\norg.freedesktop.Flatpak=talk";
        let new = append_entry(seed, ENTRY);
        assert_eq!(
            new,
            format!("{seed}\n[Context]\nfilesystems={ENTRY};\n"),
            "the other group as it was"
        );
    }

    #[test]
    fn appends_into_a_context_group_with_other_keys() {
        let seed = "# mine\n[Context]\nshared=network;ipc;\r\nsockets=x11;\n\n[Environment]\nA=1\n";
        let new = append_entry(seed, ENTRY);
        assert_eq!(
            new,
            format!(
                "# mine\n[Context]\nfilesystems={ENTRY};\nshared=network;ipc;\r\nsockets=x11;\n\n[Environment]\nA=1\n"
            )
        );
        // A list already there, closed or not, gains one item.
        let seed = "[Context]\nfilesystems=/a;/b:ro\nshared=ipc;\n";
        let new = append_entry(seed, ENTRY);
        assert_eq!(
            new,
            format!("[Context]\nfilesystems=/a;/b:ro;{ENTRY};\nshared=ipc;\n")
        );
        assert_eq!(
            remove_entry(&new, ENTRY).unwrap(),
            "[Context]\nfilesystems=/a;/b:ro;\nshared=ipc;\n"
        );
        // A header ending the file.
        assert_eq!(
            append_entry("[Context]", ENTRY),
            format!("[Context]\nfilesystems={ENTRY};\n")
        );
    }

    /// A user's own entries survive the grant and the revoke, byte for byte.
    ///
    /// Mutation (watched red): `remove_entry` cutting the item without its `;`.
    #[test]
    fn a_users_override_survives_grant_and_revoke() {
        let seed = "[Context]\nfilesystems=/home/u/Games:ro;!/home/u/.ssh;\n\n[Session Bus Policy]\norg.kde.StatusNotifierWatcher=talk\n";
        let granted = append_entry(seed, ENTRY);
        assert!(granted.contains(&format!("!/home/u/.ssh;{ENTRY};\n")));
        assert_eq!(remove_entry(&granted, ENTRY).unwrap(), seed);
        // Another spelling of the folder, or the same folder read-only, is not ours.
        let other =
            "[Context]\nfilesystems=/data/astra/profiles/game/;/data/astra/profiles/game:ro;\n";
        assert_eq!(remove_entry(other, ENTRY), None);
    }

    #[test]
    fn a_listed_or_denied_path_is_read_as_such() {
        let p = "/data/astra/profiles/game";
        assert_eq!(listing("", p), Listing::Absent);
        for v in [
            p.to_owned(),
            format!("{p}:ro"),
            format!("{p}:rw"),
            format!("{p}:create"),
        ] {
            assert_eq!(
                listing(&format!("[Context]\nfilesystems=/x;{v};\n"), p),
                Listing::Listed,
                "{v}"
            );
        }
        assert_eq!(
            listing(&format!("[Context]\nfilesystems=!{p};\n"), p),
            Listing::Denied
        );
        assert_eq!(
            listing(&format!("[Context]\nfilesystems={p}x;\n"), p),
            Listing::Absent
        );
        // Only `[Context]`'s list counts.
        assert_eq!(
            listing(&format!("[Other]\nfilesystems={p};\n"), p),
            Listing::Absent
        );
        // The last value of the key wins.
        assert_eq!(
            listing(
                &format!("[Context]\nfilesystems={p};\nfilesystems=/y;\n"),
                p
            ),
            Listing::Absent
        );
    }

    #[test]
    fn escaped_separators_are_no_split() {
        let v = r"/a\;b;/c";
        let t: Vec<&str> = tokens(v).iter().map(|&(s, e)| &v[s..e]).collect();
        assert_eq!(t, [r"/a\;b", "/c"]);
    }

    /// Mutation (watched red): `parse_steam_permissions` testing `network` only.
    #[test]
    fn steams_permissions_need_network_and_ipc() {
        let ok = "[Context]\nshared=network;ipc;\nsockets=x11;wayland;\n\n[Session Bus Policy]\norg.freedesktop.Notifications=talk\n";
        parse_steam_permissions(ok).unwrap();
        for (shared, missing) in [
            ("ipc;", "network"),
            ("network;", "ipc"),
            ("", "network,ipc"),
            ("!network;ipc;", "network"),
        ] {
            let e = parse_steam_permissions(&format!("[Context]\nshared={shared}\n")).unwrap_err();
            assert_eq!(e.code, codes::STEAM_FLATPAK_UNUSABLE);
            assert_eq!(e.params["reason"], "permission");
            assert_eq!(e.params["missing"], missing, "{shared}");
        }
        let e = parse_steam_permissions("[Application]\nname=x\n").unwrap_err();
        assert_eq!(e.params["missing"], "network,ipc");
        for garbage in [
            "",
            "error: app not installed\n",
            "shared=network;ipc;\n",
            "[Context\n",
        ] {
            let e = parse_steam_permissions(garbage).unwrap_err();
            assert_eq!(e.code, codes::STEAM_FLATPAK_UNUSABLE);
            assert_eq!(e.params["reason"], "unparseable", "{garbage:?}");
        }
    }

    #[test]
    fn the_overrides_file_follows_flatpak_and_xdg() {
        let s = |v: &str| Some(OsString::from(v));
        let f = |p: &str| Some(PathBuf::from(p).join(STEAM_FLATPAK_APP));
        assert_eq!(
            overrides_file_from(s("/fp"), s("/xdg"), s("/home/u")),
            f("/fp/overrides")
        );
        assert_eq!(
            overrides_file_from(None, s("/xdg"), s("/home/u")),
            f("/xdg/flatpak/overrides")
        );
        assert_eq!(
            overrides_file_from(s(""), s("rel"), s("/home/u")),
            f("/home/u/.local/share/flatpak/overrides")
        );
        assert_eq!(overrides_file_from(None, None, None), None);
    }
}
