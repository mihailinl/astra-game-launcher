// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The game being modded: its folder, its program and how it runs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::paths::{canonical_dir, relative_components};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Platform {
    Windows,
    /// A Windows game on Linux under Steam's Proton. `prefix` is `steamapps/compatdata/<appid>/pfx`.
    LinuxProton {
        prefix: Option<PathBuf>,
    },
    LinuxNative,
}

impl Platform {
    /// The manifest's name for this platform (`[target] platforms`).
    pub fn code(&self) -> &'static str {
        match self {
            Platform::Windows => "windows-x64",
            Platform::LinuxProton { .. } => "linux-proton",
            Platform::LinuxNative => "linux-x64",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameTarget {
    pub dir: PathBuf,
    /// Relative to `dir`.
    pub exe: PathBuf,
    pub steam_appid: Option<u32>,
    pub platform: Platform,
    /// Steam's `appcache/appinfo.vdf`, read at plan time for the app's Valve Anti-Cheat flag
    /// when `steam_appid` is set. `None`: every Steam install [`crate::steam_roots`] finds is
    /// tried, so a caller that does not know where Steam is still gets the check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_appinfo: Option<PathBuf>,
}

impl GameTarget {
    /// The exe's checked components below `dir`.
    pub(crate) fn exe_components(&self) -> Result<Vec<String>> {
        let s = self.exe.to_str().ok_or_else(|| {
            LauncherError::new(codes::PATH_ESCAPES).with("path", self.exe.display().to_string())
        })?;
        relative_components(s)
            .ok_or_else(|| LauncherError::new(codes::PATH_ESCAPES).with("path", s.to_owned()))
    }

    /// The exe's components joined with `/`.
    pub(crate) fn exe_rel(&self) -> Result<String> {
        Ok(self.exe_components()?.join("/"))
    }

    pub(crate) fn exe_file_name(&self) -> Result<String> {
        Ok(self.exe_components()?.pop().unwrap_or_default())
    }

    /// `${game}`: the folder holding the exe, canonical. The exe must exist.
    pub(crate) fn game_root(&self) -> Result<PathBuf> {
        let comps = self.exe_components()?;
        let dir = canonical_dir(&self.dir)?;
        let mut exe = dir.clone();
        exe.extend(&comps);
        match std::fs::symlink_metadata(&exe) {
            Ok(m) if m.is_file() => {}
            _ => {
                return Err(LauncherError::new(codes::EXE_NOT_FOUND).with("exe", comps.join("/")));
            }
        }
        let parent = exe.parent().map(Path::to_path_buf).unwrap_or(dir.clone());
        let parent = canonical_dir(&parent)?;
        if !parent.starts_with(&dir) {
            return Err(LauncherError::new(codes::PATH_ESCAPES).with("path", comps.join("/")));
        }
        Ok(parent)
    }
}

/// The Proton prefix Steam keeps for `appid`, for a game installed at
/// `<library>/steamapps/common/<game>`: `<library>/steamapps/compatdata/<appid>/pfx`.
/// Returns the path whether it exists or not; `None` when the folder is not in a Steam library.
pub fn proton_prefix_for(game_dir: &Path, appid: u32) -> Option<PathBuf> {
    let common = game_dir.parent()?;
    let steamapps = common.parent()?;
    let is = |p: &Path, name: &str| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name));
    if !is(common, "common") || !is(steamapps, "steamapps") {
        return None;
    }
    Some(
        steamapps
            .join("compatdata")
            .join(appid.to_string())
            .join("pfx"),
    )
}

/// The Steam appid that owns `game_dir`, read from the library itself: the
/// `steamapps/appmanifest_<appid>.acf` whose `installdir` resolves to this very folder
/// (canonical paths compared). `None` when the folder is not in a Steam library or no manifest
/// names it. Several apps sharing the folder: `game-mismatch` (the folder alone cannot say which;
/// [`steam_appids_of_dir`] lists them for a caller that knows).
///
/// This is the appid a game folder PROVES; a caller's `--appid` is the other trusted source, and
/// an integration manifest's `steam_appid` never is (it may only cause a refusal).
pub fn steam_appid_of_dir(game_dir: &Path) -> Result<Option<u32>> {
    match steam_appids_of_dir(game_dir).as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        several => Err(several_apps(several, None)),
    }
}

/// `game-mismatch` for a folder several Steam apps share and a caller that named none of them.
pub(crate) fn several_apps(apps: &[u32], caller: Option<u32>) -> LauncherError {
    let want = apps
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    LauncherError::new(codes::GAME_MISMATCH)
        .with("field", "steam_appid")
        .with("want", want)
        .with("got", caller.map(|c| c.to_string()).unwrap_or_default())
}

/// Every Steam appid whose `appmanifest_*.acf` names `game_dir` as its install folder, sorted,
/// each once: several for games that share one folder (the GoldSrc titles in
/// `common/Half-Life`), none outside a Steam library.
pub fn steam_appids_of_dir(game_dir: &Path) -> Vec<u32> {
    let Ok(dir) = std::fs::canonicalize(game_dir) else {
        return Vec::new();
    };
    let Some(steamapps) = dir.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    let is = |p: Option<&std::ffi::OsStr>, name: &str| {
        p.and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    if !is(dir.parent().and_then(Path::file_name), "common")
        || !is(steamapps.file_name(), "steamapps")
    {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(steamapps) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let lower = name.to_ascii_lowercase();
        if !(lower.starts_with("appmanifest_") && lower.ends_with(".acf")) {
            continue;
        }
        // A manifest is a few KB; a huge one is not Steam's.
        if e.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > 1 << 20 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let (Some(appid), Some(installdir)) =
            (acf_value(&text, "appid"), acf_value(&text, "installdir"))
        else {
            continue;
        };
        let Ok(appid) = appid.trim().parse::<u32>() else {
            continue;
        };
        if !installdir.is_empty()
            && std::fs::canonicalize(steamapps.join("common").join(&installdir))
                .is_ok_and(|p| p == dir)
        {
            found.push(appid);
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// The bits of an app's `StateFlags` (its `steamapps/appmanifest_<appid>.acf`) that say Steam
/// will change the game's files before it next starts it, each with the name a
/// `game-update-pending` refusal gives it (`state`). Steam's own app states (`EAppState`):
/// `2` an update is required (Steam knows of one and has not started it: with "only update this
/// game when I launch it", a scheduled download window, an update released minutes ago), `256`
/// running, `512` paused, `1024` started, `65536` reconfiguring (a DLC, language or beta branch
/// changed), `262144` adding files, `524288` preallocating, `1048576` downloading, `2097152`
/// staging, `4194304` committing, `8388608` stopping. Asked to start the game, Steam finishes
/// these first, so what Play scanned would not be what starts.
///
/// The other bits never refuse: `4` fully installed, `1` uninstalled, `16` locked, `32` files
/// missing, `64` app running (a stale one is common: this desk's Half-Life 2 episodes carry
/// `68` with nothing running), `128` files corrupt, `2048` uninstalling, `4096` backup running,
/// `131072` validating — none of them is a new build. Nor do `BytesToDownload` and
/// `BytesDownloaded`: two up-to-date apps on this desk (`StateFlags` `4`, `TargetBuildID` equal
/// to `buildid`) keep `BytesToDownload` above a `BytesDownloaded` of `0` from an earlier download.
pub const STEAM_UPDATE_STATES: &[(u32, &str)] = &[
    (2, "update-required"),
    (256, "update-running"),
    (512, "update-paused"),
    (1024, "update-started"),
    (65_536, "reconfiguring"),
    (262_144, "adding-files"),
    (524_288, "preallocating"),
    (1_048_576, "downloading"),
    (2_097_152, "staging"),
    (4_194_304, "committing"),
    (8_388_608, "update-stopping"),
];

/// The names ([`STEAM_UPDATE_STATES`]) of the update bits set in `state_flags`, in that table's
/// order; empty when Steam will start the game as its folder is now.
pub fn steam_update_states(state_flags: u32) -> Vec<&'static str> {
    STEAM_UPDATE_STATES
        .iter()
        .filter(|(bit, _)| state_flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Steam app `appid`'s `StateFlags`, from the `steamapps/appmanifest_<appid>.acf` of the library
/// holding `game_dir` ([`steam_app_manifest`]). `None` when no such manifest is found: nothing is
/// known then, and nothing is refused on it.
pub fn steam_app_state(game_dir: &Path, appid: u32) -> Option<u32> {
    manifest_state(&steam_app_manifest(game_dir, appid)?)
}

/// Steam's name for an app's default branch, as the `BetaKey` of a launch entry may spell it
/// ([`steam_app_branches`]).
pub const STEAM_DEFAULT_BRANCH: &str = "public";

/// The branches Steam app `appid` is on, from the manifest [`steam_app_state`] reads: the
/// `BetaKey` under `UserConfig` (the branch the person chose) and under `MountedConfig` (the one
/// whose files are in the folder), each once, ignoring ASCII case; an absent or empty one is the
/// default branch, [`STEAM_DEFAULT_BRANCH`]. The two differ while Steam has not finished moving
/// the game to the chosen branch. This desk's real manifests (2026-11-07, 30 apps in three
/// libraries): no `BetaKey` at all on the default branch, and `"BetaKey" "internal"` under both
/// objects for an app on its `internal` branch.
///
/// `None` when no manifest is found: which branch is played is not known then.
pub fn steam_app_branches(game_dir: &Path, appid: u32) -> Option<Vec<String>> {
    let text = steam_app_manifest(game_dir, appid)?;
    let mut out: Vec<String> = Vec::new();
    for object in ["UserConfig", "MountedConfig"] {
        let key = acf_at(&text, &[object, "BetaKey"]).unwrap_or_default();
        let key = match key.trim() {
            "" => STEAM_DEFAULT_BRANCH,
            k => k,
        };
        if !out.iter().any(|o| o.eq_ignore_ascii_case(key)) {
            out.push(key.to_owned());
        }
    }
    Some(out)
}

/// The text of Steam app `appid`'s `steamapps/appmanifest_<appid>.acf` in the library holding
/// `game_dir` (the library's `steamapps/common/<folder>` is `game_dir` or one of its parents).
/// The library is looked up from `game_dir` as the caller spells it (made absolute, links NOT
/// resolved) first, then from `game_dir` resolved: a library's folder may be a link to another
/// disk (`steamapps/common/Game -> /mnt/games/Game`, a Windows junction), and resolved first it
/// would lie in no library at all. The first manifest found that is Steam's wins.
///
/// `None` when the folder is in no Steam library, or the manifest is absent, not a regular file,
/// larger than any Steam writes (1 MiB), unreadable, or gives no decimal `StateFlags` at its top
/// level (not Steam's): nothing is known then.
fn steam_app_manifest(game_dir: &Path, appid: u32) -> Option<String> {
    let mut tried: Vec<PathBuf> = Vec::new();
    for dir in [
        std::path::absolute(game_dir).ok(),
        std::fs::canonicalize(game_dir).ok(),
    ]
    .into_iter()
    .flatten()
    {
        if tried.contains(&dir) {
            continue;
        }
        let text = library_of(&dir).and_then(|steamapps| {
            let path = steamapps.join(format!("appmanifest_{appid}.acf"));
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > 1 << 20 {
                return None;
            }
            std::fs::read_to_string(&path).ok()
        });
        if let Some(text) = text.filter(|t| manifest_state(t).is_some()) {
            return Some(text);
        }
        tried.push(dir);
    }
    None
}

/// The `steamapps` folder of the library holding `dir`: the first of `dir` and its parents that
/// sits in a folder named `common` inside one named `steamapps` (names compared ignoring ASCII
/// case), as spelled — nothing is resolved here.
fn library_of(dir: &Path) -> Option<&Path> {
    let is = |p: Option<&Path>, name: &str| {
        p.and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    dir.ancestors().find_map(|folder| {
        let common = folder.parent();
        let steamapps = common.and_then(Path::parent);
        if is(common, "common") && is(steamapps, "steamapps") {
            steamapps
        } else {
            None
        }
    })
}

/// A manifest's top-level `StateFlags`, when it is decimal.
fn manifest_state(text: &str) -> Option<u32> {
    acf_value(text, "StateFlags")?.trim().parse().ok()
}

/// A top-level value of an `appmanifest_*.acf` (Valve's text KeyValues: `"AppState" { "key"
/// "value" ... }`): the first `"key" "value"` pair directly inside the root object, key compared
/// ignoring ASCII case. Nested objects are skipped.
fn acf_value(text: &str, key: &str) -> Option<String> {
    acf_at(text, &[key])
}

/// The value at `path` below an `appmanifest_*.acf`'s root object: `["StateFlags"]` directly
/// inside it, `["UserConfig", "BetaKey"]` inside its `UserConfig` object. The first `"key"
/// "value"` pair whose objects and key match `path`, each compared ignoring ASCII case; any other
/// object is walked over.
fn acf_at(text: &str, path: &[&str]) -> Option<String> {
    enum Tok {
        Str(String),
        Open,
        Close,
    }
    let (key, objects) = path.split_last()?;
    let mut toks = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '\\' => s.push(chars.next()?),
                        '"' => break,
                        c => s.push(c),
                    }
                }
                toks.push(Tok::Str(s));
            }
            '{' => toks.push(Tok::Open),
            '}' => toks.push(Tok::Close),
            _ => {}
        }
    }
    // The names of the objects open at this point, the root's first ("" for an unnamed one).
    let mut open: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match (&toks[i], toks.get(i + 1)) {
            (Tok::Str(name), Some(Tok::Open)) => {
                open.push(name);
                i += 1;
            }
            (Tok::Open, _) => open.push(""),
            (Tok::Close, _) => {
                open.pop();
            }
            (Tok::Str(k), Some(Tok::Str(v))) => {
                if open.len() == objects.len() + 1
                    && k.eq_ignore_ascii_case(key)
                    && open[1..]
                        .iter()
                        .zip(objects)
                        .all(|(a, b)| a.eq_ignore_ascii_case(b))
                {
                    return Some(v.clone());
                }
                i += 1;
            }
            (Tok::Str(_), _) => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acf_values_are_read_at_the_top_level_only() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"1850570\"\n\t\"name\"\t\t\"DEATH {STRANDING}\"\n\t\"UserConfig\"\n\t{\n\t\t\"installdir\"\t\t\"nested\"\n\t}\n\t\"installdir\"\t\t\"DS \\\"DC\\\"\"\n}\n";
        assert_eq!(acf_value(acf, "appid").as_deref(), Some("1850570"));
        assert_eq!(acf_value(acf, "INSTALLDIR").as_deref(), Some("DS \"DC\""));
        assert_eq!(acf_value(acf, "missing"), None);
        assert_eq!(
            acf_value("\"AppState\" { \"appid\" \"unterminated", "appid"),
            None
        );
    }

    /// A value inside a named object: only there, never the same key elsewhere or deeper.
    #[test]
    fn acf_values_are_read_inside_the_object_named() {
        let acf = "\"AppState\"\n{\n\t\"BetaKey\"\t\"top\"\n\t\"InstalledDepots\"\n\t{\n\t\t\"UserConfig\"\n\t\t{\n\t\t\t\"BetaKey\"\t\"deep\"\n\t\t}\n\t}\n\t\"userconfig\"\n\t{\n\t\t\"language\"\t\"english\"\n\t\t\"BETAKEY\"\t\"public_test\"\n\t}\n\t\"MountedConfig\"\n\t{\n\t}\n}\n";
        assert_eq!(
            acf_at(acf, &["UserConfig", "BetaKey"]).as_deref(),
            Some("public_test")
        );
        assert_eq!(acf_at(acf, &["MountedConfig", "BetaKey"]), None);
        assert_eq!(acf_at(acf, &["BetaKey"]).as_deref(), Some("top"));
        assert_eq!(acf_at(acf, &[]), None);
    }
}
