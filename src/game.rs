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

/// A top-level value of an `appmanifest_*.acf` (Valve's text KeyValues: `"AppState" { "key"
/// "value" ... }`): the first `"key" "value"` pair directly inside the root object, key compared
/// ignoring ASCII case. Nested objects are skipped.
fn acf_value(text: &str, key: &str) -> Option<String> {
    enum Tok {
        Str(String),
        Open,
        Close,
    }
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
    let mut depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        match (&toks[i], toks.get(i + 1)) {
            (Tok::Open, _) => depth += 1,
            (Tok::Close, _) => depth = depth.saturating_sub(1),
            (Tok::Str(k), Some(Tok::Str(v))) => {
                if depth == 1 && k.eq_ignore_ascii_case(key) {
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
}
