// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The install plan: the consent sheet, as data. Computing it changes nothing on disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::{anti_cheat_scan, engine_of_exe};
use crate::error::{LauncherError, Result, codes};
use crate::game::{GameTarget, Platform};
use crate::ledger::{self, Ledger};
use crate::manifest::IntegrationInfo;
use crate::package::GiPackage;
use crate::paths::{
    FileState, absolute, canonical_dir, existing_target, file_state, resolve_lenient,
};

/// What happens to one file beside the game's exe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GameAction {
    /// Nothing is there; ours is added. Uninstall removes it.
    Add,
    /// A game file is there; it is backed up into the profile and ours takes its place.
    /// Uninstall restores it, but only while ours is still there unchanged.
    Replace,
    /// The very file we ship is already there. It is never rewritten, and never removed.
    ReuseKnownProxy,
    /// A `doorstop_config.ini` we did not write is already there. It is left as it is.
    KeepExisting,
}

impl GameAction {
    pub fn code(self) -> &'static str {
        match self {
            GameAction::Add => "add",
            GameAction::Replace => "replace",
            GameAction::ReuseKnownProxy => "reuse-known-proxy",
            GameAction::KeepExisting => "keep-existing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFileAction {
    /// Below the exe's folder, `/`-separated.
    pub rel_path: String,
    pub action: GameAction,
    /// The digest of the file we ship.
    pub sha256: String,
    /// The digest of the file already there (`replace`, `keep-existing`).
    pub existing_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub integration: IntegrationInfo,
    /// The zip's SHA-256.
    pub source_sha256: String,
    pub profile_dir: PathBuf,
    /// `${game}`: the folder holding the exe, where the game files go.
    pub game_dir: PathBuf,
    pub profile_files: u32,
    pub profile_bytes: u64,
    pub game_files: Vec<GameFileAction>,
    /// With placeholders still visible.
    pub launch_args: Vec<String>,
    /// Written into the Proton prefix at launch (Proton only): (dll, value).
    pub proton_override: Option<(String, String)>,
    /// The integration installed in this profile now; install replaces it.
    pub replaces: Option<IntegrationInfo>,
    /// Codes: "experimental", "not-reviewed", "replaces-integration", "no-license".
    pub warnings: Vec<String>,
}

/// Names a mod loader's proxy DLL goes by. A foreign one is another loader: never replaced.
const LOADER_PROXIES: &[&str] = &["winhttp.dll", "version.dll", "dxgi.dll", "d3d11.dll"];

/// Files that, when present and not ours, are left alone.
const KEEP_IF_PRESENT: &[&str] = &["doorstop_config.ini"];

/// What the game file at `path` will be once the current install (if any) is uninstalled:
/// uninstall removes our `add`s and restores our `replace`s, while they still have our digest.
fn state_before(path: &Path, rel: &str, existing: Option<&Ledger>) -> Result<FileState> {
    let actual = file_state(path)?;
    if let (Some(l), FileState::File { sha256 }) = (existing, &actual)
        && let Some(e) = l
            .game_files
            .iter()
            .find(|g| g.rel.eq_ignore_ascii_case(rel))
        && *sha256 == e.sha256
    {
        match e.action {
            GameAction::Add => return Ok(FileState::Absent),
            GameAction::Replace => {
                if let Some(b) = &e.backup {
                    return Ok(FileState::File {
                        sha256: b.sha256.clone(),
                    });
                }
            }
            GameAction::ReuseKnownProxy | GameAction::KeepExisting => {}
        }
    }
    Ok(actual)
}

/// Checks the package against the game and says exactly what an install would do.
pub fn plan_install(
    pkg: &GiPackage,
    game: &GameTarget,
    profile_root: &Path,
) -> Result<InstallPlan> {
    let m = pkg.manifest();
    if m.target.anti_cheat != "none" {
        return Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", m.target.anti_cheat.clone()));
    }
    let exe = game.exe_components()?;
    let exe_rel = exe.join("/");
    let game_root = game.game_root()?;
    let game_dir = canonical_dir(&game.dir)?;

    if m.target.exe != "*" && !m.target.exe.eq_ignore_ascii_case(&exe_rel) {
        return Err(LauncherError::new(codes::GAME_MISMATCH)
            .with("field", "exe")
            .with("want", m.target.exe.clone())
            .with("got", exe_rel));
    }
    if let (Some(want), Some(got)) = (m.target.steam_appid, game.steam_appid)
        && want != got
    {
        return Err(LauncherError::new(codes::GAME_MISMATCH)
            .with("field", "steam_appid")
            .with("want", want.to_string())
            .with("got", got.to_string()));
    }
    let platform = game.platform.code();
    if !m.target.platforms.iter().any(|p| p == platform) {
        return Err(LauncherError::new(codes::PLATFORM_UNSUPPORTED)
            .with("platform", platform)
            .with("supported", m.target.platforms.join(",")));
    }
    if let Some(kind) = anti_cheat_scan(&game_dir)?.into_iter().next() {
        return Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", kind));
    }
    if let Some(want) = m.engine() {
        let got = engine_of_exe(&game_dir, &exe)?;
        if got != want {
            return Err(LauncherError::new(codes::ENGINE_MISMATCH)
                .with("want", want.code())
                .with("got", got.code()));
        }
    }

    let profile_dir = absolute(profile_root)?;
    let profile_resolved = resolve_lenient(&profile_dir)?;
    if profile_resolved.starts_with(&game_dir) || game_dir.starts_with(&profile_resolved) {
        return Err(LauncherError::new(codes::PATH_ESCAPES)
            .with("path", profile_dir.display().to_string())
            .with("reason", "profile-overlaps-game"));
    }
    let existing = if profile_resolved.is_dir() {
        ledger::read(&profile_resolved)?
    } else {
        None
    };

    let mut game_files = Vec::new();
    for f in &pkg.game_files {
        let rel = f.rel_string();
        let state = match existing_target(&game_root, &f.rel)? {
            Some(path) => state_before(&path, &rel, existing.as_ref())?,
            None => FileState::Absent,
        };
        let name = f
            .rel
            .last()
            .map(|n| n.to_ascii_lowercase())
            .unwrap_or_default();
        let (action, existing_sha256) = match state {
            FileState::Absent => (GameAction::Add, None),
            FileState::File { sha256 } if sha256 == f.sha256 => (GameAction::ReuseKnownProxy, None),
            FileState::File { sha256 } if KEEP_IF_PRESENT.contains(&name.as_str()) => {
                (GameAction::KeepExisting, Some(sha256))
            }
            FileState::File { .. } if LOADER_PROXIES.contains(&name.as_str()) => {
                return Err(LauncherError::new(codes::FOREIGN_LOADER).with("file", rel));
            }
            FileState::File { sha256 } => (GameAction::Replace, Some(sha256)),
        };
        game_files.push(GameFileAction {
            rel_path: rel,
            action,
            sha256: f.sha256.clone(),
            existing_sha256,
        });
    }

    let mut warnings = vec!["experimental".to_owned(), "not-reviewed".to_owned()];
    if existing.is_some() {
        warnings.push("replaces-integration".to_owned());
    }
    if m.integration.license.trim().is_empty() {
        warnings.push("no-license".to_owned());
    }

    Ok(InstallPlan {
        integration: m.integration.clone(),
        source_sha256: pkg.sha256().to_owned(),
        profile_dir,
        game_dir: crate::paths::plain(&game_root),
        profile_files: pkg.profile_files.len() as u32,
        profile_bytes: pkg.profile_files.iter().map(|f| f.size).sum(),
        game_files,
        launch_args: m.launch.args.clone(),
        proton_override: match game.platform {
            Platform::LinuxProton { .. } => m.proton_override(),
            _ => None,
        },
        replaces: existing.map(|l| l.integration),
        warnings,
    })
}
