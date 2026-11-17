// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Steam as the Flathub Flatpak (`com.valvesoftware.Steam`, 0.5.3).
//!
//! Flathub's Steam has no access to the user's home, and Doorstop's target is
//! `${profile}/BepInEx/core/BepInEx.Preloader.dll`, in the caller's data folder: a game Steam
//! starts from its sandbox would never see the integration. So [`crate::launch()`], immediately
//! before it spawns a plan whose Steam is the Flatpak ([`crate::ProcessPlan::sandbox_grant`]),
//! grants Steam's sandbox read/write access to exactly the profile's `BepInEx` folder (BepInEx
//! writes its log there) and read access to exactly its `link` folder (Astra Link's
//! `astra-link.json`, [`crate::launch_with_link`]) in the user's Flatpak overrides, and
//! [`crate::uninstall`] takes both away again.
//!
//! - **What is granted** is `<profile>/BepInEx` ([`SANDBOX_FOLDER`], `rw`) and `<profile>/link`
//!   ([`LINK_FOLDER`], `ro`) and nothing else, never the profile: the profile's root holds the
//!   ledger, the backups and `flatpak-grant.json`, which the launcher reads and acts on outside
//!   the sandbox. A plan that hands the game a profile path outside `BepInEx` (an argument, a
//!   `[[config_writes]]` file, a mods chain's `chain.ini`) is refused by
//!   [`crate::prepare_launch`] (`reason` = `layout`) rather than granted more.
//! - **Why a folder for the link file** (0.6.1): Flatpak binds a granted path when Steam's
//!   sandbox starts, long before Play. A grant of the file itself is skipped while the file is
//!   missing, and once it exists pins the inode it had then, so every later Play's atomic rename
//!   would be invisible inside. A folder's grant sees every rename inside it. Like `BepInEx`'s,
//!   it reaches a sandbox Steam started after the grant: the first grant needs one restart of
//!   Steam. The folder is the launcher's, created before the grant when missing; a link or a file
//!   at its name is refused (`path`).
//! - **The overrides file** is a GKeyFile, `$FLATPAK_USER_DIR/overrides/com.valvesoftware.Steam`,
//!   else under `$XDG_DATA_HOME/flatpak`, else `~/.local/share/flatpak` ([`user_overrides_file`]).
//!   The launcher adds exactly two entries, `<canonical profile folder>/BepInEx:rw` then
//!   `<canonical profile folder>/link:ro`, to `[Context] filesystems` and leaves every other byte
//!   of the file as it was: other entries, keys, groups, comments, order. A folder already listed
//!   (any mode) is left alone and recorded as the user's (`preexisting`, each folder on its own);
//!   one the user denies (`!<folder>`) is refused, never overridden, and nothing is written.
//! - **What was granted** is recorded in the profile, `flatpak-grant.json` ([`FlatpakGrant`]: the
//!   `BepInEx` entry in its top-level fields, the `link` entry in [`FlatpakGrant::link`]), a
//!   profile file no manifest may write ([`crate::reserved::PROFILE_FILE_NAMES`]). A record 0.6.0
//!   wrote holds the `BepInEx` entry alone: the next grant keeps it as it was and adds the `link`
//!   entry beside it.
//! - **Revoke** ([`crate::uninstall`], before the profile's files go): for each entry the record
//!   says the launcher added, the `link` one first and then the `BepInEx` one (the reverse of the
//!   order they were added in), exactly that token is removed, every other entry left as it is
//!   (the user's `!/x`, `/y:ro`, another spelling of another folder), and so is whatever else the
//!   grant added and nothing more ([`GrantEdit`]): the `;` it closed a list with, the
//!   `filesystems=` line, the `[Context]` group, the file. The file comes back byte for byte.
//!   Never a reason for an uninstall to fail: what could not be undone is the warning
//!   [`FLATPAK_REVOKE_FAILED`].
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
//! the folder), `overrides` (the overrides file cannot be found, read or written; `path`),
//! `layout` (from `prepare_launch`: the plan hands the game a profile path outside `BepInEx`;
//! `path`).
//!
//! Every function takes the overrides file and the `flatpak` program as parameters; only
//! [`user_overrides_file`] reads the environment.
//!
//! **Known limit:** one overrides file serves every profile, and its read-modify-write is
//! serialised only inside this process (the daemon). A second process (the command line) editing
//! it at the same instant can lose one of the two edits: a grant lost is a game that cannot see
//! its folder until the next Play, a revoke lost an entry left behind.

use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::paths::{FLATPAK_GRANT_FILE, LINK_DIR, check_regular_or_absent, plain, write_atomic};
use crate::safe_dir::SafeDir;

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

/// What [`grant_sandbox_access`] did, as `<profile>/flatpak-grant.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatpakGrant {
    /// Always [`STEAM_FLATPAK_APP`].
    pub app: String,
    /// The `BepInEx` folder's `filesystems` token: `<canonical profile folder>/BepInEx:rw`.
    pub entry: String,
    /// The overrides file it went into.
    pub overrides: PathBuf,
    /// The folder was listed already (the user's own entry): nothing was written for it, and
    /// revoke takes nothing of it out.
    pub preexisting: bool,
    /// What the grant added to the overrides file, so that revoke takes out exactly that and the
    /// file comes back byte for byte. `None` when it added nothing (`preexisting`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<GrantEdit>,
    /// The `link` folder's entry ([`LINK_FOLDER`], 0.6.1): `<canonical profile folder>/link:ro`.
    /// `None` in a record 0.6.0 wrote, until the next grant adds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<GrantEntry>,
}

/// One more folder's entry in a [`FlatpakGrant`] ([`FlatpakGrant::link`]), by the same rules as
/// the record's top-level `entry`, `preexisting` and `edit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantEntry {
    /// The `filesystems` token.
    pub entry: String,
    /// The folder was listed already (the user's own entry): nothing was written for it.
    pub preexisting: bool,
    /// What the grant added for it; `None` when it added nothing (`preexisting`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<GrantEdit>,
}

/// What a grant added to the overrides file ([`FlatpakGrant::edit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantEdit {
    pub added: Added,
    /// A `;` was put before the item to close a list that did not end with one
    /// (`a;b` became `a;b;<entry>;`).
    #[serde(default)]
    pub separator: bool,
    /// A `\n` was put at the end of a file that did not end with one, before the new group.
    #[serde(default)]
    pub newline: bool,
}

/// The most a grant added: one item to a list that was there, the `filesystems=` line (right
/// after the `[Context]` header that was there), the `[Context]` group (at the end of the file),
/// or the file itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Added {
    Item,
    Key,
    Group,
    File,
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

/// The folder of a profile Flathub's Steam is granted read/write access to: what the game reads
/// and writes at runtime (Doorstop's target `BepInEx/core/…`, the plugins, `BepInEx/config/*.cfg`,
/// the log). Never the profile itself, whose ledger, backups and records the launcher trusts
/// later. The one other folder granted is [`LINK_FOLDER`], read-only.
pub const SANDBOX_FOLDER: &str = "BepInEx";

/// The folder of a profile Flathub's Steam is granted read access to: where
/// [`crate::launch_with_link`] writes Astra Link's `astra-link.json` (0.6.1). A folder and not the
/// file, because Flatpak binds a granted path once, when Steam's sandbox starts (the module docs).
pub const LINK_FOLDER: &str = LINK_DIR;

/// Grants Flathub's Steam read/write access to exactly `<profile>/BepInEx` ([`SANDBOX_FOLDER`])
/// and read access to exactly `<profile>/link` ([`LINK_FOLDER`], created first when missing;
/// `profile` is the canonical profile folder) in the overrides file `overrides`, after
/// `flatpak`'s [`check_steam_permissions`], and records it in `<profile>/flatpak-grant.json`,
/// outside the grant. The module docs say what is written and what is refused; nothing is
/// written to the overrides file or the record before every check passed, and the overrides
/// file is written once.
pub fn grant_sandbox_access(
    profile: &Path,
    overrides: &Path,
    flatpak: &Path,
) -> Result<FlatpakGrant> {
    check_steam_permissions(flatpak)?;
    let profile_refused = || unusable("path").with("path", profile.display().to_string());
    let canonical = fs::canonicalize(profile).map_err(|_| profile_refused())?;
    if plain(&canonical) != profile || !canonical.is_dir() {
        return Err(profile_refused());
    }
    let entry_path = checked_folder(&profile.join(SANDBOX_FOLDER))?;
    let entry = format!("{entry_path}:rw");
    // The link folder is the launcher's: made here when missing, a real folder (a link or a file
    // at its name is refused, never followed).
    let link_folder = profile.join(LINK_FOLDER);
    SafeDir::open(profile, &[LINK_FOLDER.to_owned()], true)
        .map_err(|_| unusable("path").with("path", link_folder.display().to_string()))?;
    let link_path = checked_folder(&link_folder)?;
    let link_entry = format!("{link_path}:ro");
    let record_path = profile.join(FLATPAK_GRANT_FILE);
    check_regular_or_absent(&record_path)?;

    let _edit = OVERRIDES_EDIT.lock().unwrap_or_else(|p| p.into_inner());
    let target = overrides_target(overrides)?;
    let existing = read_overrides(&target)?;
    let mut text = existing.clone().unwrap_or_default();
    let earlier = read_record(profile);
    let mut bepinex = place(
        &mut text,
        &entry_path,
        &entry,
        earlier.as_ref().map(|r| GrantEntry {
            entry: r.entry.clone(),
            preexisting: r.preexisting,
            edit: r.edit,
        }),
    )?;
    let mut link = place(
        &mut text,
        &link_path,
        &link_entry,
        earlier.and_then(|r| r.link),
    )?;
    if existing.as_deref() != Some(text.as_str()) {
        // The first entry added to a file that was not there created it.
        if existing.is_none()
            && let Some(edit) = [&mut bepinex.edit, &mut link.edit]
                .into_iter()
                .flatten()
                .next()
        {
            edit.added = Added::File;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|_| overrides_error(&target))?;
        }
        write_atomic(&target, text.as_bytes()).map_err(|_| overrides_error(&target))?;
    }
    let grant = FlatpakGrant {
        app: STEAM_FLATPAK_APP.to_owned(),
        entry: bepinex.entry,
        overrides: overrides.to_path_buf(),
        preexisting: bepinex.preexisting,
        edit: bepinex.edit,
        link: Some(link),
    };
    let json = serde_json::to_vec(&grant).map_err(LauncherError::io)?;
    write_atomic(&record_path, &json)?;
    Ok(grant)
}

/// One folder's entry placed in `text` (`folder` is its path, `entry` its token): appended when
/// `[Context] filesystems` does not name the folder, refused (`denied`) when the user denies it,
/// and otherwise the user's (`preexisting`), unless `earlier` (what the profile's record says an
/// earlier grant did) says the launcher added exactly this token, which is still there: then it
/// is still ours, with what that grant added, so revoke still takes exactly that out.
fn place(
    text: &mut String,
    folder: &str,
    entry: &str,
    earlier: Option<GrantEntry>,
) -> Result<GrantEntry> {
    let (preexisting, edit) = match listing(text, folder) {
        Listing::Denied => return Err(unusable("denied").with("path", folder)),
        Listing::Absent => {
            let (new, edit) = append_entry(text, entry);
            *text = new;
            (false, Some(edit))
        }
        Listing::Listed => match earlier {
            Some(r) if !r.preexisting && r.entry == entry && has_token(text, entry) => {
                (false, r.edit)
            }
            _ => (true, None),
        },
    };
    Ok(GrantEntry {
        entry: entry.to_owned(),
        preexisting,
        edit,
    })
}

/// Takes the entries `<profile>/flatpak-grant.json` records out of the user's overrides file
/// `overrides` ([`user_overrides_file`]), undoing exactly what the grant added for each
/// ([`FlatpakGrant::edit`], [`FlatpakGrant::link`]) when the launcher added it (`preexisting`
/// false), the `link` entry first, then removes the record. `profile` is the canonical profile
/// folder. The record must name exactly `overrides`, exactly this profile's
/// `<profile>/BepInEx:rw` and, when it has one, exactly its `<profile>/link:ro`: a record that
/// names anything else edits nothing. A record 0.6.0 wrote (no `link`) takes out its `BepInEx`
/// entry alone. Nothing recorded, an overrides file that is gone, or entries that are gone
/// already: nothing to do. [`FLATPAK_REVOKE_FAILED`] when it could not be done (the record is
/// kept then); never an error.
pub(crate) fn revoke_recorded(profile: &Path, overrides: Option<&Path>) -> Option<&'static str> {
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
    // What the launcher added, the `link` entry first: the reverse of the order a grant adds
    // them in, so each edit is undone on the text it was made on.
    let ours: Vec<(&str, Option<&GrantEdit>)> = record
        .link
        .iter()
        .filter(|l| !l.preexisting)
        .map(|l| (l.entry.as_str(), l.edit.as_ref()))
        .chain((!record.preexisting).then_some((record.entry.as_str(), record.edit.as_ref())))
        .collect();
    if ours.is_empty() {
        return done();
    }
    let token = |folder: &str, mode: &str| {
        plain(&profile.join(folder))
            .to_str()
            .map(|p| format!("{p}:{mode}"))
    };
    let mine = token(SANDBOX_FOLDER, "rw").is_some_and(|t| record.entry == t)
        && record
            .link
            .as_ref()
            .is_none_or(|l| token(LINK_FOLDER, "ro").is_some_and(|t| l.entry == t));
    // The file edited is the caller's, never one the record chose: the record must name it
    // exactly.
    let Some(overrides) =
        overrides.filter(|o| record.app == STEAM_FLATPAK_APP && record.overrides == *o)
    else {
        return Some(FLATPAK_REVOKE_FAILED);
    };
    if !mine {
        return Some(FLATPAK_REVOKE_FAILED);
    }
    let _edit = OVERRIDES_EDIT.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(target) = overrides_target(overrides) else {
        return Some(FLATPAK_REVOKE_FAILED);
    };
    let text = match read_overrides(&target) {
        Ok(Some(t)) => t,
        Ok(None) => return done(),
        Err(_) => return Some(FLATPAK_REVOKE_FAILED),
    };
    let mut new = text.clone();
    for (entry, edit) in &ours {
        if let Some(t) = remove_entry(&new, entry, *edit) {
            new = t;
        }
    }
    if new == text {
        return done();
    }
    let created = ours
        .iter()
        .any(|(_, e)| e.is_some_and(|e| e.added == Added::File));
    let written = if new.is_empty() && created {
        fs::remove_file(&target).map_err(|e| LauncherError::io_at(&e, &target))
    } else {
        write_atomic(&target, new.as_bytes())
    };
    if written.is_err() {
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

/// The text a grant names `profile` (the folder granted) by: its own spelling, which must be its canonical one (no
/// link anywhere on the way), a folder, UTF-8, absolute, with no character a GKeyFile list or a
/// Flatpak filesystem entry would need escaped (`;`, `\`, `=`, `[`, `]`, `:`, a control
/// character, a space at either end), and neither `/` nor the home folder or above it.
fn checked_folder(profile: &Path) -> Result<String> {
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
/// group at the end otherwise; and what was added. Every other byte as it was.
fn append_entry(text: &str, entry: &str) -> (String, GrantEdit) {
    let ls = lines(text);
    let at = context_list(&ls);
    let mut out = String::with_capacity(text.len() + entry.len() + 32);
    let mut edit = GrantEdit {
        added: Added::Item,
        separator: false,
        newline: false,
    };
    if let Some((k, start)) = at.key {
        for (i, (line, end)) in ls.iter().enumerate() {
            out.push_str(line);
            if i == k {
                let value = &line[start..];
                let closed =
                    value.is_empty() || tokens(value).last().is_none_or(|&(_, e)| e < value.len());
                if !closed {
                    out.push(';');
                    edit.separator = true;
                }
                out.push_str(entry);
                out.push(';');
            }
            out.push_str(end);
        }
        return (out, edit);
    }
    if let Some(h) = at.header {
        edit.added = Added::Key;
        for (i, (line, end)) in ls.iter().enumerate() {
            out.push_str(line);
            if i == h {
                if end.is_empty() {
                    edit.newline = true;
                }
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
        return (out, edit);
    }
    edit.added = Added::Group;
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
        edit.newline = true;
    }
    out.push_str("[Context]\nfilesystems=");
    out.push_str(entry);
    out.push_str(";\n");
    (out, edit)
}

/// `text` without the item `entry` (every item spelled exactly so) in `[Context] filesystems`,
/// each with its own `;`, and without whatever else the grant `edit` added: the `;` it closed the
/// list with, the line, the group, the `\n` before it (an empty text then means the file it
/// created). `None` when the list does not hold the item. What the user changed since stays: a
/// line or a group still holding anything is kept. Without an `edit` (a record written before
/// 0.5.3 was released) a list left empty loses its line. Every other byte as it was.
fn remove_entry(text: &str, entry: &str, edit: Option<&GrantEdit>) -> Option<String> {
    let ls = lines(text);
    let at = context_list(&ls);
    let (k, start) = at.key?;
    let (line, end) = ls[k];
    let value = &line[start..];
    let toks = tokens(value);
    let ours: Vec<usize> = (0..toks.len())
        .filter(|&i| &value[toks[i].0..toks[i].1] == entry)
        .collect();
    let last = *ours.last()?;
    let mut kept = String::with_capacity(value.len());
    let mut from = 0;
    for &i in &ours {
        let (s, e) = toks[i];
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
    // The `;` the grant closed the list with, while our item is still its last.
    if edit.is_some_and(|e| e.separator) && last + 1 == toks.len() && kept.ends_with(';') {
        kept.pop();
    }
    let line_ours = edit.is_none_or(|e| e.added != Added::Item);
    let drop_line = line_ours && kept.trim().is_empty();
    let mut out: Vec<(String, &str)> = Vec::with_capacity(ls.len());
    for (i, (l, e)) in ls.iter().enumerate() {
        if i == k {
            if !drop_line {
                out.push((format!("{}{kept}", &l[..start]), end));
            }
        } else {
            out.push(((*l).to_owned(), e));
        }
    }
    let group_ours = edit.is_some_and(|e| matches!(e.added, Added::Group | Added::File));
    let key_ours = edit.is_some_and(|e| e.added == Added::Key);
    if drop_line {
        // The header the group starts with (before the list's line, so its index stands).
        if let Some(h) = at.header {
            if group_ours && h + 1 == out.len() {
                out.pop();
                if edit.is_some_and(|e| e.newline)
                    && let Some(prev) = out.last_mut()
                {
                    prev.1 = "";
                }
            } else if key_ours && edit.is_some_and(|e| e.newline) && h + 1 == out.len() {
                // The header ended the file with no `\n` before the grant gave it one.
                out[h].1 = "";
            }
        }
    }
    Some(out.into_iter().map(|(l, e)| l + e).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: &str = "/data/astra/profiles/game:rw";

    /// Grant then revoke, on `seed`: the text in between, and what revoke left.
    fn round_trip(seed: &str) -> (String, String) {
        let (granted, edit) = append_entry(seed, ENTRY);
        let back = remove_entry(&granted, ENTRY, Some(&edit)).unwrap();
        (granted, back)
    }

    const LINK: &str = "/data/astra/profiles/game/link:ro";

    /// Both entries, as a grant adds them (`BepInEx` then `link`) and as revoke takes them out
    /// (`link` then `BepInEx`): every seed comes back byte for byte, a file the first created
    /// included.
    ///
    /// The order matters: undone `BepInEx` first, the line its grant added would be kept, empty.
    #[test]
    fn two_entries_come_out_in_the_reverse_order_they_went_in() {
        for seed in [
            "",
            "[Context]\nfilesystems=/a;/b:ro",
            "[Context]\nshared=network;\n",
            "[Session Bus Policy]\nx=talk",
            "[Context]\r\nfilesystems=/a\r\n",
        ] {
            let (one, first) = append_entry(seed, ENTRY);
            let (two, second) = append_entry(&one, LINK);
            assert_eq!(second.added, Added::Item, "{seed:?}");
            let mut back = two.clone();
            for (entry, edit) in [(LINK, &second), (ENTRY, &first)] {
                back = remove_entry(&back, entry, Some(edit)).unwrap();
            }
            assert_eq!(back, seed, "{seed:?} -> {two:?}");
        }
        let (one, first) = append_entry("", ENTRY);
        let (two, second) = append_entry(&one, LINK);
        let wrong = remove_entry(&two, ENTRY, Some(&first)).unwrap();
        let wrong = remove_entry(&wrong, LINK, Some(&second)).unwrap();
        assert_eq!(wrong, "[Context]\nfilesystems=\n");
    }

    #[test]
    fn appends_into_an_absent_file() {
        let (new, edit) = append_entry("", ENTRY);
        assert_eq!(new, format!("[Context]\nfilesystems={ENTRY};\n"));
        assert_eq!(edit.added, Added::Group);
        assert_eq!(remove_entry(&new, ENTRY, Some(&edit)).unwrap(), "");
    }

    #[test]
    fn appends_into_a_file_with_no_context_group() {
        let seed = "[Session Bus Policy]\norg.freedesktop.Flatpak=talk";
        let (new, back) = round_trip(seed);
        assert_eq!(
            new,
            format!("{seed}\n[Context]\nfilesystems={ENTRY};\n"),
            "the other group as it was"
        );
        assert_eq!(back, seed);
    }

    #[test]
    fn appends_into_a_context_group_with_other_keys() {
        let seed = "# mine\n[Context]\nshared=network;ipc;\r\nsockets=x11;\n\n[Environment]\nA=1\n";
        let (new, back) = round_trip(seed);
        assert_eq!(
            new,
            format!(
                "# mine\n[Context]\nfilesystems={ENTRY};\nshared=network;ipc;\r\nsockets=x11;\n\n[Environment]\nA=1\n"
            )
        );
        assert_eq!(back, seed);
        // A list already there, closed or not, gains one item.
        let seed = "[Context]\nfilesystems=/a;/b:ro\nshared=ipc;\n";
        let (new, back) = round_trip(seed);
        assert_eq!(
            new,
            format!("[Context]\nfilesystems=/a;/b:ro;{ENTRY};\nshared=ipc;\n")
        );
        assert_eq!(back, seed);
        // A header ending the file.
        let (new, back) = round_trip("[Context]");
        assert_eq!(new, format!("[Context]\nfilesystems={ENTRY};\n"));
        assert_eq!(back, "[Context]");
    }

    /// Revoke undoes exactly what grant did: every seed comes back byte for byte — no trailing
    /// `;`, an empty list the user had, a trailing space, CRLF lines, no `[Context]`, no newline
    /// at the end, a group with no list.
    ///
    /// Mutation (watched red): `remove_entry` ignoring `separator`; ignoring `newline`.
    #[test]
    fn revoke_undoes_exactly_what_the_grant_added() {
        for seed in [
            "",
            "[Context]\nfilesystems=\n",
            "[Context]\nfilesystems=/a; \n",
            "[Context]\nfilesystems=/a;/b:ro",
            "[Context]\r\nfilesystems=/a\r\nshared=ipc;\r\n",
            "[Context]\nfilesystems=/home/u/Games:ro;!/home/u/.ssh;\n\n[Session Bus Policy]\norg.kde.StatusNotifierWatcher=talk\n",
            "[Session Bus Policy]\nx=talk",
            "[Session Bus Policy]\nx=talk\n",
            "[Context]\nshared=network;\n",
            "[Context]",
            "# only a comment",
        ] {
            let (granted, back) = round_trip(seed);
            assert_ne!(granted, seed);
            assert_eq!(back, seed, "{seed:?} -> {granted:?}");
        }
        // The user edited the list after the grant: their item stays, and the `;` the grant
        // closed the list with stays too, since an item follows ours now.
        let (granted, edit) = append_entry("[Context]\nfilesystems=/a\n", ENTRY);
        assert!(edit.separator);
        let edited = granted.replace(&format!("{ENTRY};"), &format!("{ENTRY};/c;"));
        assert_eq!(
            remove_entry(&edited, ENTRY, Some(&edit)).unwrap(),
            "[Context]\nfilesystems=/a;/c;\n"
        );
        // A group the grant created that the user wrote into since stays, with their key.
        let (granted, edit) = append_entry("", ENTRY);
        let edited = format!("{granted}shared=ipc;\n");
        assert_eq!(
            remove_entry(&edited, ENTRY, Some(&edit)).unwrap(),
            "[Context]\nshared=ipc;\n"
        );
    }

    /// A user's own entries survive the grant and the revoke, byte for byte.
    ///
    /// Mutation (watched red): `remove_entry` cutting the item without its `;`.
    #[test]
    fn a_users_override_survives_grant_and_revoke() {
        let seed = "[Context]\nfilesystems=/home/u/Games:ro;!/home/u/.ssh;\n\n[Session Bus Policy]\norg.kde.StatusNotifierWatcher=talk\n";
        let (granted, back) = round_trip(seed);
        assert!(granted.contains(&format!("!/home/u/.ssh;{ENTRY};\n")));
        assert_eq!(back, seed);
        // Another spelling of the folder, or the same folder read-only, is not ours.
        let other =
            "[Context]\nfilesystems=/data/astra/profiles/game/;/data/astra/profiles/game:ro;\n";
        assert_eq!(remove_entry(other, ENTRY, None), None);
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
