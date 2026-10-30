// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play with the player's own mods (unity-mods-design.md §5.2–§5.4).
//!
//! Doorstop starts the integration's chain loader (`[integration.mods].chain`) instead of its own
//! preloader; the chain reads `chain.ini` and hands the game to THEIR preloader, then adds Astra's
//! plugins. What Play passes:
//!
//! - the proxy that is beside the game NOW (ours, shared, or relinked by their mod manager),
//!   whose generation its bytes tell: Doorstop 4 `--doorstop-enabled true
//!   --doorstop-target-assembly <chain>`, Doorstop 3 `--doorstop-enable true --doorstop-target
//!   <chain>` (no IL2CPP on 3);
//! - their pack's own settings, read from the SOURCE's `doorstop_config.ini` (`<root>/…`; the
//!   game folder's for a BepInEx installed there): its Mono search path override, its
//!   `boot_config_override`, and for IL2CPP its .NET runtime (`corlib_dir`, `coreclr_path`,
//!   their `dotnet/` by default). A relative path is looked up below their root, then the game
//!   folder; a Windows path (`C:\…`, `Z:\…`) is passed as written;
//! - then today's pins (`doorstop::session_options`).
//!
//! Everything is checked before anything is written (§5.3), and nothing of theirs is ever written
//! (§5.4): the one file this adds is `chain.ini`, in Astra's profile (uninstall removes it, and the
//! chain's `last-run.txt`).
//!
//! **Their paths ride Steam's command line.** Every path of theirs (the search override, IL2CPP's
//! runtime, the `boot.config` override) is one element of the argv `steam -applaunch` is spawned
//! with, never a shell string and never quoted by hand — the way Astra's own profile path has
//! always travelled. The tests prove one element per path, spaces and Cyrillic included; that Steam
//! hands each to the game whole is a live-check item, as it was for Astra's own path.
//!
//! **An answer that cannot be read is no answer.** A `mods-choice.json` that is not JSON, too new,
//! or not a file is never a reason Play fails: it is treated as not asked yet
//! (`mods-choice-needed`, with `choice_unreadable` saying why) or, with nothing to ask about, a Play
//! of Astra alone that says so (`mods-choice-unreadable` in `ProcessPlan::warnings`).

use std::path::{Path, PathBuf};

use super::{
    Budget, ChoiceKind, ChoiceSource, ModsDecl, ModsEnv, ModsLoader, Unplayable, accepts,
    bepinex_at, discover, melonloader_at, read_choice,
};
use crate::doorstop::{self, Generation};
use crate::error::{LauncherError, Result, codes};
use crate::game::{GameTarget, Platform};
use crate::ledger::Ledger;
use crate::manifest::Manifest;
use crate::paths::{FileState, checked_rel, existing_target, file_state, find_any_case};
use crate::plan::is_loader_proxy;

/// A path for the game's command line or `chain.ini`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GamePath {
    /// On this machine: written as the game sees it (`Z:\…` under Proton).
    Local(PathBuf),
    /// Already a Windows path in their file (`C:\…`, `Z:\…`): passed as written.
    Verbatim(String),
}

/// A Play with mods, checked: everything it needs, read before anything is written.
#[derive(Debug, Clone)]
pub(crate) struct WithMods {
    pub decl: ModsDecl,
    pub source: ChoiceSource,
    /// Their preloader: `<root>/BepInEx/core/BepInEx.Preloader.dll` (`…BepInEx.Unity.IL2CPP.dll`).
    pub target: PathBuf,
    /// Their pack's Mono search path override, each path found.
    pub search_override: Vec<GamePath>,
    /// Their pack's `boot_config_override`, when it sets one that is there.
    pub boot_config: Option<GamePath>,
    /// IL2CPP: their .NET runtime's folder and `coreclr.dll`.
    pub clr: Option<(GamePath, GamePath)>,
}

/// The new refusals' codes, with their params.
fn source_missing(source: &ChoiceSource) -> LauncherError {
    LauncherError::new(codes::MODS_SOURCE_MISSING)
        .with("profile", source.profile.clone())
        .with("manager", source.manager.code())
        .with("root", source.root.display().to_string())
}

fn unsupported(reason: &str) -> LauncherError {
    LauncherError::new(codes::MODS_UNSUPPORTED).with("reason", reason)
}

/// A Windows path written in a file: `C:\…`, `C:/…`, `\\server\…`.
fn windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
        || s.starts_with("\\\\")
}

/// One path from their `doorstop_config.ini`, found: below `root`, then the game folder.
fn resolve(value: &str, root: &Path, game_root: &Path) -> Option<GamePath> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if windows_absolute(value) {
        return Some(GamePath::Verbatim(value.to_owned()));
    }
    if value.starts_with('/') {
        return Path::new(value)
            .exists()
            .then(|| GamePath::Local(PathBuf::from(value)));
    }
    let parts: Vec<&str> = value.split(['\\', '/']).collect();
    [root, game_root]
        .iter()
        .find_map(|base| find_any_case(base, &parts))
        .map(GamePath::Local)
}

/// Whether the file at `rel` (`/`-separated, below `profile`) is whole: the ledger's digest.
fn whole(profile: &Path, rel: &str, sha256: &str) -> Result<bool> {
    let Some(p) = existing_target(profile, &checked_rel(rel)?)? else {
        return Ok(false);
    };
    Ok(file_state(&p)?
        == FileState::File {
            sha256: sha256.to_owned(),
        })
}

/// What [`resolve_play`] found: the Play with mods, if it is one, and why the player's answer
/// could not be read, if it could not.
#[derive(Debug, Default)]
pub(crate) struct Resolved {
    pub with: Option<WithMods>,
    pub choice_unreadable: Option<String>,
}

/// What Play does about the player's mods: `with: None` for today's Play (no
/// `[integration.mods]`, whatever was chosen; the player chose "without"; or nothing to ask
/// about), `Some` for a Play with mods, or the refusal (§5.3). Reads only. `profile` is canonical
/// and locked.
pub(crate) fn resolve_play(
    profile: &Path,
    m: &Manifest,
    ledger: &Ledger,
    game: &GameTarget,
    env: &ModsEnv,
) -> Result<Resolved> {
    // An integration with no table plays Astra on its own, whatever was chosen (an IL2CPP runner
    // ships without it until it is proven; a "with" from a release that had it): its Play reads
    // nothing of the player's, not even their answer.
    let decl = match m.mods() {
        None => return Ok(Resolved::default()),
        Some(d) => d,
    };
    // An answer that cannot be read is no answer: never a reason this Play fails.
    let (choice, choice_unreadable) = match read_choice(profile) {
        Ok(c) => (c, None),
        Err(e) => (None, Some(e.to_string())),
    };
    let Some(choice) = choice else {
        // Asked once: only when the integration can play with mods and something is offered.
        if super::capability(game, Some(ledger)).is_ok() {
            let offered = discover(game, Some(ledger), &[], env)
                .iter()
                .filter(|s| s.playable.is_ok())
                .count();
            if offered > 0 {
                let mut e = LauncherError::new(codes::MODS_CHOICE_NEEDED)
                    .with("sources", offered.to_string());
                if let Some(why) = &choice_unreadable {
                    e = e.with("choice_unreadable", why.clone());
                }
                return Err(e);
            }
        }
        return Ok(Resolved {
            with: None,
            choice_unreadable,
        });
    };
    if choice.choice == ChoiceKind::Without {
        return Ok(Resolved::default());
    }
    let source = choice.source.clone().expect("checked when read");

    // The integration can.
    let decl = match decl {
        Err(e) => return Err(e.clone()),
        Ok(d) => d.clone(),
    };
    if !matches!(
        game.platform,
        Platform::Windows | Platform::LinuxProton { .. }
    ) {
        return Err(unsupported("platform"));
    }
    if doorstop::launch_options(&m.launch.args).is_none()
        || !ledger.game_files.iter().any(|g| is_loader_proxy(&g.rel))
    {
        return Err(unsupported("no-doorstop"));
    }

    // Our chain loader is installed and whole: every file of the install in its folder.
    let chain = decl.chain_rel();
    let chain_file = chain.join("/");
    if !ledger
        .files
        .iter()
        .any(|f| f.rel.eq_ignore_ascii_case(&chain_file))
    {
        return Err(LauncherError::new(codes::NEEDS_REINSTALL)
            .with("file", chain_file)
            .with("reason", "not-in-the-install"));
    }
    let folder = chain[..chain.len() - 1].join("/").to_ascii_lowercase();
    for f in &ledger.files {
        let rel = f.rel.to_ascii_lowercase();
        let in_folder = if folder.is_empty() {
            rel == chain_file.to_ascii_lowercase()
        } else {
            rel.starts_with(&format!("{folder}/"))
        };
        if in_folder && !whole(profile, &f.rel, &f.sha256)? {
            return Err(LauncherError::new(codes::NEEDS_REINSTALL).with("file", f.rel.clone()));
        }
    }
    let chain_config = decl.chain_config_rel().join("/");
    if ledger
        .files
        .iter()
        .any(|f| f.rel.eq_ignore_ascii_case(&chain_config))
    {
        return Err(LauncherError::new(codes::MANIFEST_INVALID)
            .with("key", "integration.mods.chain_config")
            .with(
                "detail",
                "a file of the install, which Play would overwrite",
            ));
    }

    // Their mods are still there, and still a BepInEx the chain runs beside.
    let root = &source.root;
    let usable = root.is_absolute()
        && root
            .to_str()
            .is_some_and(|s| !s.contains(['\n', '\r', '\0']));
    if !usable || !root.is_dir() {
        return Err(source_missing(&source));
    }
    let Some(loader) = bepinex_at(root, &Budget::new()) else {
        if melonloader_at(root, false).is_some() {
            return Err(
                LauncherError::new(codes::MODS_MELONLOADER).with("profile", source.profile.clone())
            );
        }
        return Err(source_missing(&source).with("reason", "no-preloader"));
    };
    if let Err(Unplayable::LoaderUnsupported { have, need }) = accepts(decl.loader, &loader) {
        return Err(LauncherError::new(codes::MODS_LOADER_UNSUPPORTED)
            .with("have", have)
            .with("need", need)
            .with("profile", source.profile.clone()));
    }
    // The one preloader the chain hands the game to (astra-bepinex's chain, §4.3 step 2).
    let want = match decl.loader {
        ModsLoader::Bepinex5 => "BepInEx.Preloader.dll",
        ModsLoader::Bepinex6Il2cpp { .. } => "BepInEx.Unity.IL2CPP.dll",
    };
    let preloader = loader.preloader.clone().unwrap_or_default();
    if !preloader.eq_ignore_ascii_case(want) {
        return Err(LauncherError::new(codes::MODS_LOADER_UNSUPPORTED)
            .with("have", loader.kind.code())
            .with("need", decl.loader.need())
            .with("profile", source.profile.clone()));
    }
    let core =
        find_any_case(root, &["BepInEx", "core"]).unwrap_or_else(|| root.join("BepInEx/core"));
    let target = core.join(&preloader);

    // Their pack's own Doorstop settings, from their copy of its ini.
    let game_root = game.game_root()?;
    // Their file, read only as a regular file: a FIFO there must not hang the Play.
    let ini = find_any_case(root, &[doorstop::CONFIG_NAME])
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
        .and_then(|p| doorstop::read_config(&p));
    let get = |section: &str, key: &str| {
        ini.as_deref()
            .and_then(|t| crate::ini::get(t, section, key))
            .filter(|v| !v.trim().is_empty())
    };
    let search_override = get("UnityMono", "dll_search_path_override")
        .or_else(|| get("UnityDoorstop", "dllSearchPathOverride"))
        .map(|v| {
            v.split(';')
                .filter_map(|p| resolve(p, root, &game_root))
                .collect()
        })
        .unwrap_or_default();
    let boot_config =
        get("General", "boot_config_override").and_then(|v| resolve(&v, root, &game_root));
    let clr = match decl.loader {
        ModsLoader::Bepinex5 => None,
        ModsLoader::Bepinex6Il2cpp { .. } => {
            let corlib = get("Il2Cpp", "corlib_dir").unwrap_or_else(|| "dotnet".to_owned());
            let coreclr =
                get("Il2Cpp", "coreclr_path").unwrap_or_else(|| "dotnet/coreclr.dll".to_owned());
            let found = |v: &str| {
                resolve(v, root, &game_root)
                    .ok_or_else(|| source_missing(&source).with("file", v.to_owned()))
            };
            Some((found(&corlib)?, found(&coreclr)?))
        }
    };
    Ok(Resolved {
        with: Some(WithMods {
            decl,
            source,
            target,
            search_override,
            boot_config,
            clr,
        }),
        choice_unreadable: None,
    })
}

/// Removes what Play with mods left in the profile of an uninstalled integration: `chain.ini`
/// (its `[integration.mods].chain_config`) and the chain's `last-run.txt` beside it, then the
/// chain's folder when that left it empty. Never through a link: one is removed as a link.
pub(crate) fn remove_play_files(profile: &Path, ledger: &Ledger) {
    let Ok(m) = Manifest::parse(&ledger.manifest) else {
        return;
    };
    let Some(Ok(decl)) = m.mods() else {
        return;
    };
    let mut dir = decl.chain_config_rel();
    let Some(ini) = dir.pop() else {
        return;
    };
    for name in [ini, LAST_RUN_FILE.to_owned()] {
        let mut rel = dir.clone();
        rel.push(name);
        if let Ok(Some(p)) = existing_target(profile, &rel)
            && std::fs::symlink_metadata(&p).is_ok_and(|m| !m.is_dir())
        {
            let _ = std::fs::remove_file(&p);
        }
    }
    let folder = dir.iter().fold(profile.to_path_buf(), |p, c| p.join(c));
    crate::paths::prune_empty_dirs(folder, profile);
}

/// The chain loader's report of its last start, beside it.
pub(crate) const LAST_RUN_FILE: &str = "last-run.txt";

impl WithMods {
    /// The Doorstop options this Play passes to a proxy of `generation`, with each path as
    /// `text` writes it; `None` when no such proxy can start this chain (IL2CPP on Doorstop 3).
    /// The first four are the ones a proxy must understand ([`Self::required`]).
    pub(crate) fn doorstop_args(
        &self,
        generation: Generation,
        chain: &str,
        text: &dyn Fn(&GamePath) -> Result<String>,
    ) -> Result<Option<Vec<String>>> {
        let joined = |paths: &[GamePath]| -> Result<String> {
            Ok(paths
                .iter()
                .map(text)
                .collect::<Result<Vec<_>>>()?
                .join(";"))
        };
        let mut a: Vec<String> = Vec::new();
        match generation {
            Generation::V4 => {
                a.extend(
                    ["--doorstop-enabled", "true", "--doorstop-target-assembly"].map(String::from),
                );
                a.push(chain.to_owned());
                if !self.search_override.is_empty() {
                    a.push("--doorstop-mono-dll-search-path-override".to_owned());
                    a.push(joined(&self.search_override)?);
                }
                if let Some((corlib, coreclr)) = &self.clr {
                    a.push("--doorstop-clr-corlib-dir".to_owned());
                    a.push(text(corlib)?);
                    a.push("--doorstop-clr-runtime-coreclr-path".to_owned());
                    a.push(text(coreclr)?);
                }
                if let Some(boot) = &self.boot_config {
                    a.push("--doorstop-boot-config-override".to_owned());
                    a.push(text(boot)?);
                }
            }
            Generation::V3 => {
                if self.clr.is_some() {
                    return Ok(None);
                }
                a.extend(["--doorstop-enable", "true", "--doorstop-target"].map(String::from));
                a.push(chain.to_owned());
                if !self.search_override.is_empty() {
                    a.push("--doorstop-dll-search-override".to_owned());
                    a.push(joined(&self.search_override)?);
                }
            }
        }
        Ok(Some(a))
    }

    /// Whether `bin`, a proxy beside the game, can start this Play: a Doorstop whose generation
    /// takes every option it must understand (switching on, the chain, their search override,
    /// IL2CPP's runtime), in the game's architecture. `boot_config_override` is not asked: a
    /// Doorstop without it reads no such setting either.
    pub(crate) fn served_by(&self, bin: &[u8], exe_machine: Option<u16>) -> bool {
        let Some(generation) = doorstop::generation(bin) else {
            return false;
        };
        let placeholder = |_: &GamePath| Ok("x".to_owned());
        let Ok(Some(args)) = self.doorstop_args(generation, "x", &placeholder) else {
            return false;
        };
        let required: Vec<String> = args
            .into_iter()
            .filter(|a| a != "--doorstop-boot-config-override")
            .collect();
        doorstop::serves(bin, exe_machine, &required)
    }

    /// `chain.ini` (astra-bepinex §4.3): UTF-8 without a BOM. `source` and `profile` are for the
    /// log only; a line break in a name could add a key, so it never reaches the file.
    pub(crate) fn chain_ini(&self, target: &str) -> String {
        let profile = if self.source.profile.is_empty() {
            self.source
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            self.source.profile.clone()
        };
        let one_line = |s: &str| s.replace(['\n', '\r', '\0'], " ");
        format!(
            "[Chain]\ntarget = {}\nsource = {}\nprofile = {}\n",
            one_line(target),
            self.source.manager.code(),
            one_line(&profile)
        )
    }
}

/// The manifest's own arguments without its Doorstop options (each `--doorstop-…` and its value):
/// a Play with mods passes its own.
pub(crate) fn without_doorstop_options(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a.to_ascii_lowercase().starts_with("--doorstop-") {
            it.next();
        } else {
            out.push(a.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_windows_path_is_passed_as_written() {
        assert!(windows_absolute("C:\\x"));
        assert!(windows_absolute("z:/x"));
        assert!(windows_absolute("\\\\server\\share"));
        assert!(!windows_absolute("BepInEx\\unstripped"));
        assert!(!windows_absolute("/home/u"));
        assert!(!windows_absolute("C:"));
    }

    #[test]
    fn the_manifest_s_doorstop_options_go_with_their_values() {
        let args: Vec<String> = [
            "--doorstop-enabled",
            "true",
            "-AstraConfig",
            "x.ini",
            "--Doorstop-Target-Assembly",
            "p.dll",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(without_doorstop_options(&args), ["-AstraConfig", "x.ini"]);
    }
}
