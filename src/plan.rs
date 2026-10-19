// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The install plan: the consent sheet, as data. Computing it changes nothing on disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::{anti_cheat_scan, engine_of_exe};
use crate::digest::sha256_hex;
use crate::doorstop;
use crate::error::{LauncherError, Result, codes};
use crate::game::{GameTarget, Platform};
use crate::game::{several_apps, steam_appids_of_dir};
use crate::ledger::{self, Ledger};
use crate::manifest::IntegrationInfo;
use crate::package::{GiPackage, PackFile};
use crate::paths::{
    FileState, absolute, canonical_dir, existing_target, file_state, real_case, resolve_lenient,
};
use crate::steam_appinfo::{steam_roots, valve_anti_cheat};

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
    /// A config file we did not write (`doorstop_config.ini`, `ReShade.ini`) is already there.
    /// It is left as it is, and uninstall neither removes nor "restores" it.
    KeepExisting,
    /// Another mod manager's loader proxy (r2modman's, Thunderstore Mod Manager's, a hand-made
    /// BepInEx install's Doorstop) is already there, and it honours every option our launch
    /// passes ([`crate::doorstop`]): the game runs through it. It is never written, backed up or
    /// removed, and its digest is not pinned, since the manager that owns it may update it.
    ShareLoader,
}

impl GameAction {
    pub fn code(self) -> &'static str {
        match self {
            GameAction::Add => "add",
            GameAction::Replace => "replace",
            GameAction::ReuseKnownProxy => "reuse-known-proxy",
            GameAction::KeepExisting => "keep-existing",
            GameAction::ShareLoader => "share-loader",
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
    /// The digest of the file already there (`replace`, `keep-existing`, `share-loader`).
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
    /// Written into the Proton prefix at launch (Proton only), every one: (dll, value), in the
    /// manifest's key order.
    #[serde(default)]
    pub proton_overrides: Vec<(String, String)>,
    /// The integration installed in this profile now; install replaces it.
    pub replaces: Option<IntegrationInfo>,
    /// Codes: "experimental", "not-reviewed", "replaces-integration", "no-license",
    /// "vac-unchecked" (the same fact as `vac_checked == false`, for a sheet that lists codes).
    pub warnings: Vec<String>,
    /// Steam's Valve Anti-Cheat flag was READ for this app and is clear. `false`: no TRUSTED Steam
    /// appid (the caller's, or the one the folder's appmanifest proves; never the integration
    /// manifest's), or no `appinfo.vdf` answered for it — not
    /// refused on that ground alone, but [`crate::install`] then needs
    /// [`Accept::vac_unchecked`] from the person, never a plain "yes". Absent in an older
    /// serialised plan: `false`.
    #[serde(default)]
    pub vac_checked: bool,
}

/// Names a mod loader's proxy DLL goes by. A foreign one is another loader: never replaced. It is
/// shared when it can do our proxy's job ([`GameAction::ShareLoader`]), and refused otherwise.
const LOADER_PROXIES: &[&str] = &["winhttp.dll", "version.dll", "dxgi.dll", "d3d11.dll"];

/// Whether the file at `rel` (`/`-separated) goes by a loader proxy's name, in any letter case.
pub(crate) fn is_loader_proxy(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    LOADER_PROXIES.iter().any(|p| p.eq_ignore_ascii_case(name))
}

/// Files that, when present and not ours, are left alone. Matched case-insensitively: Wine and
/// Windows see `ReShade.ini` and `reshade.ini` as one file.
///
/// `ReShade.ini` is here because ReShade REWRITES it while the game runs (the overlay saves the
/// user's settings into it). A user's own file must survive install and uninstall, and ours, once
/// ReShade has rewritten it, is the user's settings: an update must not back it up and replace it,
/// and an uninstall must not "restore" an older copy over it.
const KEEP_IF_PRESENT: &[&str] = &["doorstop_config.ini", "ReShade.ini"];

fn keep_if_present(name: &str) -> bool {
    KEEP_IF_PRESENT.iter().any(|k| k.eq_ignore_ascii_case(name))
}

/// What the person accepted beyond the plan itself, passed to [`crate::install`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accept {
    /// The plan could not read Steam's VAC flag (`InstallPlan::vac_checked` is false) and the
    /// person installs anyway, having been told. Install refuses such a plan without it.
    pub vac_unchecked: bool,
}

/// The `kind` of an `anti-cheat` refusal for a game Steam flags as VAC-protected.
pub const VALVE_ANTI_CHEAT: &str = "valve-anti-cheat";

/// The Steam appid this game is KNOWN to be: the caller's, or the one the game folder proves
/// through its library's `appmanifest_*.acf` ([`crate::steam_appids_of_dir`]). The caller's must
/// be among the folder's apps when the folder names any (several share GoldSrc's folder);
/// otherwise, or with several and no caller, `game-mismatch`. An integration manifest's
/// `steam_appid` is never a source here.
pub(crate) fn trusted_appid(game: &GameTarget) -> Result<Option<u32>> {
    let folder = steam_appids_of_dir(&game.dir);
    match (game.steam_appid, folder.as_slice()) {
        (caller, []) => Ok(caller),
        // A folder several apps share (GoldSrc's `common/Half-Life`): the caller's, when it is
        // one of them.
        (Some(caller), apps) if apps.contains(&caller) => Ok(Some(caller)),
        (None, [one]) => Ok(Some(*one)),
        (caller, apps) => Err(several_apps(apps, caller)),
    }
}

/// Every `appinfo.vdf` on this machine worth asking: the caller's, then every Steam install's
/// (a wrong path from the caller never switches those off), each file once.
fn appinfo_files(game: &GameTarget) -> Vec<std::path::PathBuf> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    let all = game.steam_appinfo.iter().cloned().chain(
        steam_roots()
            .into_iter()
            .map(|r| r.join("appcache").join("appinfo.vdf")),
    );
    for p in all {
        let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(p);
        }
    }
    out
}

/// The VAC gate, shared by plan and launch. Every appinfo that answers is read, and ANY one
/// flagging VAC refuses — for the trusted appid ([`trusted_appid`]) and for the manifest's own
/// `steam_appid` alike (the manifest may cause a refusal, never a pass). Returns whether the flag
/// was READ for the trusted appid (`false`: no trusted appid, or no appinfo answered for it).
pub(crate) fn vac_check(
    game: &GameTarget,
    trusted: Option<u32>,
    manifest: Option<u32>,
) -> Result<bool> {
    vac_verdict(&appinfo_files(game), trusted, manifest)
}

/// [`vac_check`] over an explicit list of `appinfo.vdf` files: refuses when ANY file flags the
/// trusted appid or the manifest's appid as VAC (never the first answer alone), and returns
/// whether some file answered for the trusted appid.
pub fn vac_verdict(
    files: &[std::path::PathBuf],
    trusted: Option<u32>,
    manifest: Option<u32>,
) -> Result<bool> {
    let answers = |id: u32| -> Vec<bool> {
        files
            .iter()
            .filter_map(|p| valve_anti_cheat(p, id))
            .collect()
    };
    let for_trusted = trusted.map(answers).unwrap_or_default();
    let for_manifest = manifest
        .filter(|m| Some(*m) != trusted)
        .map(answers)
        .unwrap_or_default();
    if for_trusted.iter().chain(&for_manifest).any(|vac| *vac) {
        return Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", VALVE_ANTI_CHEAT));
    }
    Ok(!for_trusted.is_empty())
}

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
            GameAction::ReuseKnownProxy | GameAction::KeepExisting | GameAction::ShareLoader => {}
        }
    }
    Ok(actual)
}

/// The digest of the foreign loader proxy at `path` when our launch can run through it instead
/// of ours (`f`), taken from the very bytes judged; `None` when it cannot.
fn shared_loader(
    pkg: &GiPackage,
    f: &PackFile,
    path: &Path,
    exe_path: &Path,
) -> Result<Option<String>> {
    let Some(theirs) = doorstop::read_proxy(path) else {
        return Ok(None);
    };
    let ours = pkg.read(&mut pkg.archive()?, f)?;
    let machine = doorstop::pe_machine_of_file(exe_path);
    let args = &pkg.manifest().launch.args;
    Ok(doorstop::stands_in(&ours, &theirs, machine, args).then(|| sha256_hex(&theirs)))
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
    // The appid this game is known to be; the manifest's may only refuse against it.
    let trusted = trusted_appid(game)?;
    if let (Some(want), Some(got)) = (m.target.steam_appid, trusted)
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
    // Cleared only for an appid the caller gave or the folder proves; the manifest's appid can
    // only add a refusal. No trusted appid is UNCHECKED, never clear.
    let vac = vac_check(game, trusted, m.target.steam_appid)?;
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

    let exe_path = game_root.join(game.exe_file_name()?);
    let mut game_files = Vec::new();
    for f in &pkg.game_files {
        let real = real_case(&game_root, &f.rel)?;
        let rel = real.join("/");
        let path = existing_target(&game_root, &real)?;
        let state = match &path {
            Some(path) => state_before(path, &rel, existing.as_ref())?,
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
            FileState::File { sha256 } if keep_if_present(&name) => {
                (GameAction::KeepExisting, Some(sha256))
            }
            FileState::File { .. } if is_loader_proxy(&name) => {
                let shared = match &path {
                    Some(path) => shared_loader(pkg, f, path, &exe_path)?,
                    None => None,
                };
                match shared {
                    Some(theirs) => (GameAction::ShareLoader, Some(theirs)),
                    None => {
                        return Err(LauncherError::new(codes::FOREIGN_LOADER).with("file", rel));
                    }
                }
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
    if !vac {
        warnings.push("vac-unchecked".to_owned());
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
        proton_overrides: match game.platform {
            Platform::LinuxProton { .. } => m.proton_overrides(),
            _ => Vec::new(),
        },
        replaces: existing.map(|l| l.integration),
        warnings,
        vac_checked: vac,
    })
}
