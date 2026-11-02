// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The player's own mods for a game, found where their mod managers keep them: r2modman,
//! Thunderstore Mod Manager, Gale, a loader installed by hand in the game folder, and folders the
//! player added. Pure reads: nothing here ever writes, and nothing of theirs ever is.
//!
//! - **Where** (code: the adapters, engine-descriptor-v1.md §7.7): each manager's data folder
//!   ([`ModsEnv`]), then the game's folder inside it — from `games.tsv` (Thunderstore's ecosystem
//!   schema: Steam appid → r2modman's folder, Gale's community), or, for a game newer than the
//!   table, the Steam install folder's name, or any `<data>/*/profiles/*` whose
//!   `BepInEx/LogOutput.log` names this game's program in its first lines. Never a guess beyond
//!   that. Then the folders the player added, and the game folder ([`ModManager::Manual`]).
//! - **What is there** (data: the built-in `mods`, [`builtin`]): in each of those folders, the
//!   built-in descriptor that placed the game ([`crate::engines::builtin_placement`]) says which
//!   loader it holds, its version, its mods root and which mods are enabled (§7.4, §7.5), through
//!   [`crate::engines::mods::evaluate_one`]: BepInEx 5 and BepInEx 6 (IL2CPP) for Unity; UE4SS 3,
//!   UE4SS 2 and the `~mods` paks for Unreal. An adapter adds only its manager's own enable state
//!   (a mod it switched off, [`switched_off`]); Astra's own items and Astra's own loader in the
//!   game folder are never the player's. A source with no mod of the player's
//!   ([`ModSource::mods`] = 0) is not listed: that is Astra's own items alone, or an empty profile.
//! - **What the data does not describe** (code, shown and never offered): MelonLoader, and a
//!   BepInEx 6 that is not the IL2CPP build the data knows (Mono, CoreCLR, the IL2CPP preloader's
//!   older name). They are recognised only to tell the player why "with your mods" is not offered,
//!   exactly as launcher 0.5.0 did ([`other_bepinex_at`], [`melonloader_at`]).
//! - **Whether it can be played together**: the installed integration's `[integration.mods]`
//!   ([`ModsDecl`]) against the loader the data recognised, its own `versions` and the
//!   declaration's (narrower, never wider); and how Astra runs beside it ([`ModSource::coexist`]).
//!
//! `tests/mods_identity.rs` holds every Unity answer to launcher 0.5.0's hand-written discovery,
//! but for the rows it declares, each counted.
//!
//! **Private** (§7.10): nothing here logs, and nothing it finds leaves the caller's hands.

pub(crate) mod builtin;
pub(crate) mod choice;
mod decl;
mod games;
pub(crate) mod play;
mod status;
pub(crate) mod version;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

pub use choice::{
    Choice, ChoiceKind, ChoiceSource, MODS_CHOICE_SCHEMA, add_mods_root, mods_choice,
    set_mods_choice,
};
pub(crate) use choice::{MAX_ADDED_ROOTS, read_file as read_choice_file, remove as remove_choice};
pub use decl::{ModsDecl, ModsLoader};
pub use status::{
    ArmedBy, LOADER_STATUS_FILE_NAME, LastRun, LoaderReason, LoaderStatus, loader_status,
    mods_last_run,
};

use crate::doorstop;
use crate::engines::EngineCode;
use crate::engines::mods::{
    Bindings, Coexist, Enabled, LoaderOutcome, Meter, Mods, Recognised, SourceKind, Strategy,
    evaluate_one_within,
};
use crate::game::{GameTarget, Platform, steam_appid_of_dir};
use crate::ledger::{self, Ledger};
use crate::manifest::Manifest;
use crate::paths::{canonical_dir, find_any_case, local_verdict, plain};
use crate::plan::is_loader_proxy;

/// Who keeps a mod source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModManager {
    R2modman,
    /// Thunderstore Mod Manager (Overwolf, Windows only): r2modman's code, its own data folder.
    Thunderstore,
    Gale,
    /// A loader installed by hand in the game folder.
    Manual,
    /// A folder the player added whose owner is not recognisable from its path.
    Folder,
}

impl ModManager {
    /// The name `chain.ini` and the daemon use: `r2modman`, `thunderstore`, `gale`, `manual`,
    /// `folder`.
    pub fn code(self) -> &'static str {
        match self {
            ModManager::R2modman => "r2modman",
            ModManager::Thunderstore => "thunderstore",
            ModManager::Gale => "gale",
            ModManager::Manual => "manual",
            ModManager::Folder => "folder",
        }
    }

    /// Where the specification's `{source}` is for a folder this manager keeps (§7.3): the game
    /// folder for [`ModManager::Manual`], the folder the player picked for
    /// [`ModManager::Folder`], an adapter's profile for the rest (a picked folder inside a
    /// manager's data is read as that manager's, §7.7).
    pub(crate) fn source_kind(self) -> SourceKind {
        match self {
            ModManager::Manual => SourceKind::Game,
            ModManager::Folder => SourceKind::Picked,
            ModManager::R2modman | ModManager::Thunderstore | ModManager::Gale => {
                SourceKind::Manager
            }
        }
    }
}

/// Which mod loader a source runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoaderKind {
    /// BepInEx 5 (`bepinex5`, built-in data).
    Bepinex5,
    /// `BepInEx.Unity.Mono.Preloader.dll`: BepInEx 6 for a Mono game. Not in the data: shown,
    /// never offered.
    Bepinex6Mono,
    /// BepInEx 6 for an IL2CPP game (`bepinex6-il2cpp`, built-in data); its preloader's older
    /// name `BepInEx.IL2CPP.dll` is shown and never offered.
    Bepinex6Il2cpp,
    /// `BepInEx.NET.CoreCLR.dll`: BepInEx 6 for a .NET game that is not Unity. Not in the data.
    Bepinex6Coreclr,
    #[serde(rename = "melonloader")]
    MelonLoader,
    /// UE4SS 3 (`ue4ss3`, built-in data): beside the game's real program, chained.
    Ue4ss3,
    /// UE4SS 2 (`ue4ss2`, built-in data): recognised, and Astra cannot run beside it.
    Ue4ss2,
    /// The engine's own `~mods` paks (`unreal-paks`, built-in data): no loader and no list; they
    /// load beside Astra whatever it does.
    UnrealPaks,
}

impl LoaderKind {
    pub fn code(self) -> &'static str {
        match self {
            LoaderKind::Bepinex5 => "bepinex5",
            LoaderKind::Bepinex6Mono => "bepinex6-mono",
            LoaderKind::Bepinex6Il2cpp => "bepinex6-il2cpp",
            LoaderKind::Bepinex6Coreclr => "bepinex6-coreclr",
            LoaderKind::MelonLoader => "melonloader",
            LoaderKind::Ue4ss3 => "ue4ss3",
            LoaderKind::Ue4ss2 => "ue4ss2",
            LoaderKind::UnrealPaks => "unreal-paks",
        }
    }

    /// The kind a built-in `mods` loader's `id` names: its code. `None` for an id no kind has.
    pub(crate) fn of_id(id: &str) -> Option<LoaderKind> {
        [
            LoaderKind::Bepinex5,
            LoaderKind::Bepinex6Il2cpp,
            LoaderKind::Ue4ss3,
            LoaderKind::Ue4ss2,
            LoaderKind::UnrealPaks,
        ]
        .into_iter()
        .find(|k| k.code() == id)
    }

    /// One BepInEx per folder (as 0.5.0 listed them): every BepInEx kind is one family.
    fn family(self) -> &'static str {
        match self {
            LoaderKind::Bepinex5
            | LoaderKind::Bepinex6Mono
            | LoaderKind::Bepinex6Il2cpp
            | LoaderKind::Bepinex6Coreclr => "bepinex",
            other => other.code(),
        }
    }
}

/// How Astra runs beside the player's loader (engine-descriptor-v1.md §7.6), as the built-in data
/// says for that loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Coexistence {
    /// Astra's runner starts first and hands control to their loader, in place (BepInEx, UE4SS 3).
    Chain,
    /// Their loader starts as it would without Astra, and Astra runs beside it (the `~mods`
    /// paks: nothing to hand over, nothing told).
    SideBySide,
    /// Their mod manager starts the game, and Astra rides along through an adapter.
    ViaManager,
}

impl Coexistence {
    pub fn code(self) -> &'static str {
        match self {
            Coexistence::Chain => "chain",
            Coexistence::SideBySide => "side-by-side",
            Coexistence::ViaManager => "via-manager",
        }
    }

    fn of(s: &Strategy) -> Coexistence {
        match s {
            Strategy::Chain { .. } => Coexistence::Chain,
            Strategy::SideBySide { .. } => Coexistence::SideBySide,
            Strategy::ViaManager => Coexistence::ViaManager,
        }
    }

    fn of_data(s: &Coexist) -> Coexistence {
        match s {
            Coexist::Chain { .. } => Coexistence::Chain,
            Coexist::SideBySide { .. } => Coexistence::SideBySide,
            Coexist::ViaManager => Coexistence::ViaManager,
        }
    }
}

/// A source's mod loader, as its files say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModLoader {
    pub kind: LoaderKind,
    /// Its version resource's own words (`BepInEx.dll` for BepInEx 5, `BepInEx.Core.dll` for 6,
    /// `UE4SS.dll`): `5.4.23.6+b89fcc8…`, `6.0.0-be.788+5b766a3…`, `3.0.1`. `None`: unreadable,
    /// or a loader that has none (the paks).
    pub version: Option<String>,
    /// The file a chain hands control to, as spelled on disk: their preloader in `BepInEx/core`,
    /// `UE4SS.dll`. `None` when Astra does not chain the loader (MelonLoader, UE4SS 2, the paks).
    pub preloader: Option<String>,
}

/// Why a source is shown but not offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum Unplayable {
    /// Their mods run through MelonLoader; Astra plays together with BepInEx only.
    #[serde(rename = "melonloader")]
    MelonLoader,
    /// Their loader is not one the integration's chain loader runs beside: `have` is theirs (its
    /// version without the commit, or the loader's code when the version is unreadable), `need`
    /// what the integration declares (`5.4`, `6.0.0-be.788`). A loader Astra cannot run beside at
    /// all, whatever is installed (UE4SS 2: `coexist = []`, §7.6), says so the same way: `need`
    /// is then the name of the loader of the same engine that Astra does run beside (`UE4SS 3`).
    LoaderUnsupported { have: String, need: String },
    /// The integration installed for this game declares no `[integration.mods]`: it plays Astra
    /// on its own for now (an IL2CPP runner ships without the table until it is proven; an Unreal
    /// runner, which needs no table: it chains a UE4SS 3 of the player's by itself). Its Play
    /// never asks, and plays Astra alone whatever was chosen.
    IntegrationWithoutMods,
    /// The integration installed for this game cannot play together with any mods: `reason` is
    /// `not-installed`, the code a Play with mods would refuse its `[integration.mods]` with
    /// (`unknown-key`, `manifest-invalid`, `path-escapes`), `platform` (a native Linux game: no
    /// loader proxy), or `no-doorstop` (its launch does not go through a Doorstop proxy beside the
    /// game). Or `over-budget`: their folder is past a bound of one evaluation (§7.8: more than
    /// 1,024 mods, a folder of more than 5,000 entries, …) or of the discovery's, so nothing of it
    /// is listed, and a start is without mods. Or `outside-versions`: the integration declares a
    /// version of this loader the built-in data's `versions` do not hold (an IL2CPP foundation at a
    /// build other than 788), so no loader of the player's can be offered with it.
    NotSupported { reason: String },
}

/// One place the player's mods for this game live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModSource {
    pub manager: ModManager,
    /// The profile's folder name; empty for [`ModManager::Manual`] (the game folder).
    pub profile: String,
    /// The folder that holds their loader (`{source}`): the profile, the folder they picked, or
    /// the game folder.
    pub root: PathBuf,
    pub loader: ModLoader,
    /// What their loader will load besides Astra: the mods the built-in data finds enabled in its
    /// mods root (BepInEx: each folder and each `*.dll` in `BepInEx/plugins`; a disabled
    /// `*.dll.old` is not one, nor a folder their manager switched off), Astra's own
    /// (`astra-item.json` inside) not counted. MelonLoader: the `*.dll` files in `Mods/`; a BepInEx
    /// the data does not describe: 0.5.0's count (`BepInEx/plugins` and `BepInEx/patchers`).
    pub mods: u32,
    /// When their loader last ran there: its log's modification time (`BepInEx/LogOutput.log`,
    /// `MelonLoader/Latest.log`), milliseconds since the UNIX epoch. `None`: never, or a loader
    /// whose log is not read.
    pub last_played: Option<u64>,
    /// `Ok`: offered. `Err`: shown, with the reason, and never offered.
    pub playable: Result<(), Unplayable>,
    /// How Astra runs beside their loader, in the built-in data's order of preference (§7.6):
    /// `chain` for BepInEx and UE4SS 3 (the Unreal runner chains a UE4SS 3 of the player's by
    /// itself), `side-by-side` for the paks. Empty: Astra cannot run beside it (MelonLoader, a
    /// BepInEx the data does not describe, UE4SS 2). New in 0.5.1; what a choice of "with" does
    /// is still [`ModSource::playable`]'s.
    #[serde(default)]
    pub coexist: Vec<Coexistence>,
}

/// Where the mod managers keep their data on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModsEnv {
    /// Windows' `%APPDATA%` (Roaming): r2modman, Thunderstore Mod Manager and Gale.
    pub appdata: Option<PathBuf>,
    /// Linux: `$XDG_CONFIG_HOME`, else `~/.config` (r2modman, an Electron app).
    pub config_home: Option<PathBuf>,
    /// Linux: `$XDG_DATA_HOME`, else `~/.local/share` (Gale).
    pub data_home: Option<PathBuf>,
    /// Linux: `~/.var/app`, every Flatpak's home (`<app>/config/…`, `<app>/data/…`).
    pub flatpak_apps: Option<PathBuf>,
}

const R2MODMAN_DIR: &str = "r2modmanPlus-local";
const GALE_DIR: &str = "com.kesomannen.gale";
const TMM_DIR: &[&str] = &["Thunderstore Mod Manager", "DataFolder"];
/// Bounds on one folder's listing.
const MAX_DIR_ENTRIES: usize = 4096;
const MAX_PROFILES: usize = 1024;
const MAX_FLATPAKS: usize = 512;
/// The first lines of `LogOutput.log` the program's name is looked for in, and how much of it.
const LOG_LINES: usize = 5;
const LOG_HEAD_BYTES: u64 = 8 * 1024;
/// More than any `BepInEx.dll` / `BepInEx.Core.dll` (~140 KB): a larger one's version is not read.
const MAX_ASSEMBLY_BYTES: u64 = 16 * 1024 * 1024;
/// What one discovery may read in all, over every data folder and source: folder entries listed,
/// and bytes of their files read — every built-in data evaluation's too, beside its own bounds
/// (§7.8). Spent, it stops looking: what it found whole so far stands, a source whose look it cut
/// short is shown over budget (never a partial list), and no source after it is looked at.
const BUDGET_ENTRIES: usize = 200_000;
const BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// How deep a mod's folder is looked into for what its manager switched off (0.5.0's bound).
const MOD_DEPTH: usize = 8;

/// The shared budget of one discovery ([`BUDGET_ENTRIES`], [`BUDGET_BYTES`]).
pub(crate) struct Budget {
    entries: std::cell::Cell<usize>,
    bytes: std::cell::Cell<u64>,
}

impl Budget {
    pub(crate) fn new() -> Budget {
        Budget {
            entries: std::cell::Cell::new(BUDGET_ENTRIES),
            bytes: std::cell::Cell::new(BUDGET_BYTES),
        }
    }

    /// One more folder entry; `false` once spent.
    fn entry(&self) -> bool {
        let left = self.entries.get();
        self.entries.set(left.saturating_sub(1));
        left > 0
    }

    /// `n` more bytes about to be read; `false` (and nothing charged) when they do not fit.
    fn bytes(&self, n: u64) -> bool {
        let left = self.bytes.get();
        if n > left {
            return false;
        }
        self.bytes.set(left - n);
        true
    }

    /// Nothing more may be listed or read.
    fn spent(&self) -> bool {
        self.entries.get() == 0 || self.bytes.get() == 0
    }
}

impl Meter for Budget {
    fn entry(&self) -> bool {
        Budget::entry(self)
    }

    fn bytes(&self, n: u64) -> bool {
        Budget::bytes(self, n)
    }

    fn bytes_left(&self) -> u64 {
        self.bytes.get()
    }
}

impl ModsEnv {
    /// This process's: `%APPDATA%` on Windows; `$HOME`, `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`
    /// elsewhere.
    pub fn from_process() -> ModsEnv {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        if cfg!(windows) {
            ModsEnv::windows(var("APPDATA"))
        } else {
            ModsEnv::linux(
                var("HOME").as_deref(),
                var("XDG_CONFIG_HOME").as_deref(),
                var("XDG_DATA_HOME").as_deref(),
            )
        }
    }

    /// Windows: everything is below `%APPDATA%`.
    pub fn windows(appdata: Option<PathBuf>) -> ModsEnv {
        ModsEnv {
            appdata,
            ..ModsEnv::default()
        }
    }

    /// Linux: the XDG folders when set to an absolute path (a relative one is invalid and ignored,
    /// as the XDG spec says), else their defaults below `home`; Flatpaks below `~/.var/app`.
    pub fn linux(
        home: Option<&Path>,
        xdg_config_home: Option<&Path>,
        xdg_data_home: Option<&Path>,
    ) -> ModsEnv {
        let xdg = |v: Option<&Path>, default: &[&str]| {
            v.filter(|p| p.is_absolute())
                .map(Path::to_path_buf)
                .or_else(|| home.map(|h| default.iter().fold(h.to_path_buf(), |p, c| p.join(c))))
        };
        ModsEnv {
            appdata: None,
            config_home: xdg(xdg_config_home, &[".config"]),
            data_home: xdg(xdg_data_home, &[".local", "share"]),
            flatpak_apps: home.map(|h| h.join(".var").join("app")),
        }
    }

    /// Every mod manager's data folder that exists here, with its manager: the folder that holds
    /// one folder per game.
    pub fn manager_roots(&self) -> Vec<(ModManager, PathBuf)> {
        let mut all: Vec<(ModManager, PathBuf)> = Vec::new();
        if let Some(a) = &self.appdata {
            all.push((ModManager::R2modman, a.join(R2MODMAN_DIR)));
            all.push((
                ModManager::Thunderstore,
                TMM_DIR.iter().fold(a.clone(), |p, c| p.join(c)),
            ));
            all.push((ModManager::Gale, a.join(GALE_DIR)));
        }
        if let Some(c) = &self.config_home {
            all.push((ModManager::R2modman, c.join(R2MODMAN_DIR)));
        }
        if let Some(d) = &self.data_home {
            all.push((ModManager::Gale, d.join(GALE_DIR)));
        }
        if let Some(apps) = &self.flatpak_apps {
            for app in sorted_dirs(apps, &Budget::new())
                .into_iter()
                .take(MAX_FLATPAKS)
            {
                all.push((ModManager::R2modman, app.join("config").join(R2MODMAN_DIR)));
                all.push((ModManager::Gale, app.join("data").join(GALE_DIR)));
            }
        }
        let mut seen: Vec<PathBuf> = Vec::new();
        all.into_iter()
            .filter(|(_, p)| {
                // A data folder on a share (a redirected %APPDATA%) is never asked about: no
                // source there could be played anyway, and asking opens an SMB session.
                if local_verdict(p).is_err() {
                    return false;
                }
                let Ok(real) = fs::canonicalize(p) else {
                    return false;
                };
                if !real.is_dir() || seen.contains(&real) {
                    return false;
                }
                seen.push(real);
                true
            })
            .collect()
    }
}

/// The folders in `dir`, sorted by name; none when it cannot be read. Every entry listed is
/// charged to `b`.
fn sorted_dirs(dir: &Path, b: &Budget) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .take(MAX_DIR_ENTRIES)
        .take_while(|_| b.entry())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// A regular file at `path` (links followed, as the loader follows them), with its size: never a
/// FIFO, a device or a folder, which `open` could block on or misread.
fn regular_file(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .filter(fs::Metadata::is_file)
        .map(|m| m.len())
}

/// Preloader names in `BepInEx/core`, in the order 0.5.0 probed them, and what each one is.
const PRELOADERS: &[(&str, LoaderKind)] = &[
    ("BepInEx.Preloader.dll", LoaderKind::Bepinex5),
    ("BepInEx.Unity.IL2CPP.dll", LoaderKind::Bepinex6Il2cpp),
    ("BepInEx.Unity.Mono.Preloader.dll", LoaderKind::Bepinex6Mono),
    ("BepInEx.IL2CPP.dll", LoaderKind::Bepinex6Il2cpp),
    ("BepInEx.NET.CoreCLR.dll", LoaderKind::Bepinex6Coreclr),
];

/// The first known preloader in `<root>/BepInEx/core`, as 0.5.0 probed them: its name as spelled
/// on disk, and its kind.
fn first_preloader(root: &Path) -> Option<(PathBuf, String, LoaderKind)> {
    let core = find_any_case(root, &["BepInEx", "core"]).filter(|p| p.is_dir())?;
    PRELOADERS.iter().find_map(|(name, kind)| {
        let pre = find_any_case(&core, &[name]).filter(|p| p.is_file())?;
        let spelled = pre.file_name()?.to_string_lossy().into_owned();
        Some((core.clone(), spelled, *kind))
    })
}

/// Whether `name` (a preloader's) is the `chain` entry of a loader of the built-in data: then the
/// data, not [`other_bepinex_at`], says what that BepInEx is.
fn data_entry_name(name: &str) -> bool {
    builtin::descriptors()
        .iter()
        .filter_map(|d| d.mods())
        .flat_map(Mods::loaders)
        .flat_map(|l| l.coexist())
        .any(|s| match s {
            Coexist::Chain { entry, .. } => entry
                .as_str()
                .rsplit('/')
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case(name)),
            _ => false,
        })
}

/// A BepInEx below `root` that the built-in data does not describe — BepInEx 6 for Mono or
/// CoreCLR, BepInEx 6 IL2CPP under its preloader's older name — exactly as 0.5.0 recognised it
/// (its first known preloader, 0.5.0's order). Shown, never offered: no integration's chain runs
/// beside one. `None` when the first preloader there is one the data describes (then the data
/// answers, or nothing does). Its version is read only from a regular file of an assembly's size,
/// charged to `b`.
fn other_bepinex_at(root: &Path, b: &Budget) -> Option<ModLoader> {
    let (core, preloader, kind) = first_preloader(root)?;
    if data_entry_name(&preloader) {
        return None;
    }
    let lib = if kind == LoaderKind::Bepinex5 {
        "BepInEx.dll"
    } else {
        "BepInEx.Core.dll"
    };
    let version = find_any_case(&core, &[lib]).and_then(|p| {
        let len = regular_file(&p).filter(|n| *n <= MAX_ASSEMBLY_BYTES)?;
        b.bytes(len)
            .then(|| version::version_string_of_file(&p, "ProductVersion"))
            .flatten()
            .and_then(|v| crate::engines::mods::printable(&v))
    });
    Some(ModLoader {
        kind,
        version,
        preloader: Some(preloader),
    })
}

/// MelonLoader below `root`: its `MelonLoader/` folder (and, in a game folder, the `version.dll`
/// proxy that loads it into every start).
fn melonloader_at(root: &Path, in_game_folder: bool) -> Option<ModLoader> {
    find_any_case(root, &["MelonLoader"]).filter(|p| p.is_dir())?;
    if in_game_folder && find_any_case(root, &["version.dll"]).is_none_or(|p| !p.is_file()) {
        return None;
    }
    Some(ModLoader {
        kind: LoaderKind::MelonLoader,
        version: None,
        preloader: None,
    })
}

/// `5.4.23.6+b89f…` → `5.4.23.6`.
fn short(version: &str) -> &str {
    version.split('+').next().unwrap_or(version)
}

/// What a loader is, as `have` says it: its version without the commit, else its code.
fn have(version: Option<&str>, kind: LoaderKind) -> String {
    version.map_or(kind.code(), short).to_owned()
}

/// Whether the integration installed with `ledger` can play together with mods on `game`, and
/// its declaration when it can; otherwise why not ([`Unplayable::IntegrationWithoutMods`],
/// [`Unplayable::NotSupported`]).
pub(crate) fn capability(
    game: &GameTarget,
    ledger: Option<&Ledger>,
) -> Result<(Manifest, ModsDecl), Unplayable> {
    let not = |reason: &str| Unplayable::NotSupported {
        reason: reason.to_owned(),
    };
    let ledger = ledger.ok_or_else(|| not("not-installed"))?;
    let m = Manifest::parse(&ledger.manifest).map_err(|e| not(e.code))?;
    let decl = match m.mods() {
        None => return Err(Unplayable::IntegrationWithoutMods),
        Some(Err(e)) => return Err(not(e.code)),
        Some(Ok(d)) => d.clone(),
    };
    if !matches!(
        game.platform,
        Platform::Windows | Platform::LinuxProton { .. }
    ) {
        return Err(not("platform"));
    }
    let proxy = ledger.game_files.iter().any(|g| is_loader_proxy(&g.rel));
    if doorstop::launch_options(&m.launch.args).is_none() || !proxy {
        return Err(not("no-doorstop"));
    }
    Ok((m, decl))
}

/// The game as the built-in data reads it: the code the built-in rule that names its program
/// gives, that rule's bindings (§7.3), and the built-in `mods` for that code. `None` when no
/// built-in rule names the program: then no built-in loader is asked, anywhere.
pub(crate) struct Placed {
    code: EngineCode,
    bindings: Bindings,
    mods: Option<&'static Mods>,
    /// The folder detection runs on, canonical: `{source}` for the game folder.
    game_dir: PathBuf,
    /// `${game}`, the folder holding the program: where Astra's own game files are.
    game_root: Option<PathBuf>,
}

impl Placed {
    pub(crate) fn of(game: &GameTarget) -> Option<Placed> {
        let game_dir = canonical_dir(&game.dir).ok()?;
        let exe = game.exe_components().ok()?;
        let (code, bindings) =
            crate::engines::builtin_placement(&game_dir, &exe, crate::engines::builtin())?;
        Some(Placed {
            mods: builtin::for_code(&code),
            code,
            bindings,
            game_dir,
            game_root: game.game_root().ok(),
        })
    }
}

/// A folder that may be a mod source, before it is looked at.
pub(crate) struct Candidate {
    pub(crate) manager: ModManager,
    pub(crate) profile: String,
    pub(crate) root: PathBuf,
    /// The game folder: its loader is [`ModManager::Manual`], and MelonLoader there needs its
    /// proxy.
    pub(crate) game_folder: bool,
}

/// One loader found in one source, judged: the source as a row says it, where a chain hands
/// control to their loader (in place), and the family that keeps it once per folder.
pub(crate) struct Found {
    pub(crate) source: ModSource,
    pub(crate) entry: Option<PathBuf>,
}

/// What discovery knows about the game while it looks.
struct Lookup<'a> {
    row: Option<games::GameRow>,
    /// The Steam install folder's name (the game folder's own name).
    installdir: Option<&'a str>,
    /// The program's name without its extension, as BepInEx logs it.
    stem: Option<String>,
    /// What this discovery may still read.
    budget: &'a Budget,
}

impl Lookup<'_> {
    /// Each manager's folder for this game below `data` (one folder per game): the table's, else
    /// the Steam install folder's name and every profile whose log names the program.
    fn game_profiles(&self, manager: ModManager, data: &Path, out: &mut Vec<Candidate>) {
        let names: Vec<&str> = match (self.row, manager) {
            (Some(r), ModManager::Gale) => vec![r.community],
            (Some(r), ModManager::R2modman | ModManager::Thunderstore) => vec![r.r2_folder],
            (Some(r), _) => vec![r.r2_folder, r.community],
            (None, _) => self.installdir.into_iter().collect(),
        };
        for name in names {
            if let Some(dir) = find_any_case(data, &[name]) {
                profiles_in(manager, &dir, out, self.budget);
            }
        }
        if self.row.is_some() {
            return;
        }
        let Some(stem) = &self.stem else {
            return;
        };
        for game_dir in sorted_dirs(data, self.budget) {
            let Some(profiles) = find_any_case(&game_dir, &["profiles"]) else {
                continue;
            };
            for p in sorted_dirs(&profiles, self.budget)
                .into_iter()
                .take(MAX_PROFILES)
            {
                if log_names_program(&p, stem, self.budget) {
                    out.push(candidate(manager, p));
                }
            }
        }
    }
}

fn candidate(manager: ModManager, root: PathBuf) -> Candidate {
    Candidate {
        manager,
        profile: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        root,
        game_folder: false,
    }
}

/// Every profile in `<game_dir>/profiles`.
fn profiles_in(manager: ModManager, game_dir: &Path, out: &mut Vec<Candidate>, b: &Budget) {
    if let Some(profiles) = find_any_case(game_dir, &["profiles"]) {
        for p in sorted_dirs(&profiles, b).into_iter().take(MAX_PROFILES) {
            out.push(candidate(manager, p));
        }
    }
}

/// Whether BepInEx's log in `profile` names `stem` as the program it ran in: the line
/// `[Message:   BepInEx] BepInEx <version> - <ProcessName> (<date>)`, looked for in the first few
/// lines (on Unity 6 an `Unable to start Unity log writer` error comes first).
fn log_names_program(profile: &Path, stem: &str, b: &Budget) -> bool {
    let Some(log) = find_any_case(profile, &["BepInEx", "LogOutput.log"]) else {
        return false;
    };
    let Some(len) = regular_file(&log) else {
        return false;
    };
    if !b.bytes(len.min(LOG_HEAD_BYTES)) {
        return false;
    }
    let mut head = Vec::new();
    if fs::File::open(&log)
        .and_then(|f| f.take(LOG_HEAD_BYTES).read_to_end(&mut head))
        .is_err()
    {
        return false;
    }
    String::from_utf8_lossy(&head)
        .lines()
        .take(LOG_LINES)
        .any(|line| logged_program(line).is_some_and(|p| p.eq_ignore_ascii_case(stem)))
}

/// The `<ProcessName>` of a BepInEx start line.
fn logged_program(line: &str) -> Option<&str> {
    const MARK: &str = "] BepInEx ";
    let rest = &line[line.find(MARK)? + MARK.len()..];
    let (_, after) = rest.split_once(" - ")?;
    let name = after.rfind(" (").map_or(after, |i| &after[..i]);
    Some(name.trim())
}

/// Who owns a folder the player added, from the names on its path.
fn manager_of_path(p: &Path) -> ModManager {
    let has = |name: &str| {
        p.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(name))
        })
    };
    if has(GALE_DIR) {
        ModManager::Gale
    } else if has(TMM_DIR[0]) {
        ModManager::Thunderstore
    } else if has(R2MODMAN_DIR) {
        ModManager::R2modman
    } else {
        ModManager::Folder
    }
}

/// A folder the player added: a profile itself (a BepInEx's `BepInEx/core` with a preloader, or
/// MelonLoader, is there: the adapters' reading of where a profile is, as in 0.5.0; what is in it
/// is then the data's to say), a game's folder of profiles, or a manager's whole (custom) data
/// folder.
fn added_root(look: &Lookup, root: &Path, out: &mut Vec<Candidate>) {
    let manager = manager_of_path(root);
    if first_preloader(root).is_some() || melonloader_at(root, false).is_some() {
        out.push(candidate(manager, root.to_path_buf()));
    } else if find_any_case(root, &["profiles"]).is_some() {
        profiles_in(manager, root, out, look.budget);
    } else {
        look.game_profiles(manager, root, out);
    }
}

/// The mods the first-level entries of `BepInEx/plugins` and `BepInEx/patchers` hold, as 0.5.0
/// counted them: for a BepInEx the data does not describe.
fn count_bepinex_mods(root: &Path, b: &Budget) -> u32 {
    let mut n = 0;
    for sub in ["plugins", "patchers"] {
        let Some(dir) = find_any_case(root, &["BepInEx", sub]) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries
            .flatten()
            .take(MAX_DIR_ENTRIES)
            .take_while(|_| b.entry())
        {
            let p = e.path();
            let Ok(meta) = fs::metadata(&p) else { continue };
            if meta.is_file() {
                n += u32::from(is_dll(&p));
            } else if meta.is_dir() && !astra_item(&p) && holds_dll(&p, 0, b) {
                n += 1;
            }
        }
    }
    n
}

/// `*.dll` in any letter case; `*.dll.old` (disabled by r2modman and Gale) is not.
fn is_dll(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".dll"))
}

/// Whether `dir` holds a `*.dll` somewhere below it, within bounds (links are followed by the
/// loader too; the depth bound ends a loop, the budget a large tree).
fn holds_dll(dir: &Path, depth: usize, b: &Budget) -> bool {
    if depth > MOD_DEPTH {
        return false;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut subdirs = Vec::new();
    for e in entries.flatten().take(MAX_DIR_ENTRIES) {
        if !b.entry() {
            return false;
        }
        let p = e.path();
        match fs::metadata(&p) {
            Ok(m) if m.is_file() && is_dll(&p) => return true,
            Ok(m) if m.is_dir() => subdirs.push(p),
            _ => {}
        }
    }
    subdirs.iter().any(|d| holds_dll(d, depth + 1, b))
}

/// Astra's own item, never one of the player's mods: a folder holding `astra-item.json`.
fn astra_item(dir: &Path) -> bool {
    find_any_case(dir, &["astra-item.json"]).is_some()
}

/// What a mod's folder holds below it, within bounds, links never followed: whether a file one
/// of `globs` names is there (the walk stops at the first), else whether a file renamed `*.old`
/// is. `None` when the budget ran out before the walk could say: never a guess.
fn names_below(dir: &Path, globs: &[&crate::engines::mods::Glob], b: &Budget) -> Option<Below> {
    fn walk(
        dir: &Path,
        depth: usize,
        globs: &[&crate::engines::mods::Glob],
        b: &Budget,
        renamed: &mut bool,
    ) -> Option<bool> {
        if depth > MOD_DEPTH {
            return Some(false);
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return Some(false);
        };
        let mut subdirs = Vec::new();
        for e in entries.flatten().take(MAX_DIR_ENTRIES) {
            if !b.entry() {
                return None;
            }
            let Ok(t) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if t.is_file() {
                if globs.iter().any(|g| g.matches(&name)) {
                    return Some(true);
                }
                *renamed |= name.to_ascii_lowercase().ends_with(".old");
            } else if t.is_dir() {
                subdirs.push(e.path());
            }
        }
        for d in subdirs {
            if walk(&d, depth + 1, globs, b, renamed)? {
                return Some(true);
            }
        }
        Some(false)
    }
    let mut renamed = false;
    Some(if walk(dir, 0, globs, b, &mut renamed)? {
        Below::Loadable
    } else if renamed {
        Below::Renamed
    } else {
        Below::Neither
    })
}

/// What [`names_below`] found below a mod's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Below {
    /// A file the loader's `file-glob`s name: it loads something.
    Loadable,
    /// None of those, and a file renamed `*.old`.
    Renamed,
    Neither,
}

/// A mod folder its manager switched off: the manager's own enable state, which an adapter reads
/// (engine-descriptor-v1.md §7.7). r2modman, Thunderstore Mod Manager and Gale switch a mod off by
/// renaming its files `<name>.old`, so the folder stays and its loader loads nothing of it: it
/// holds a file renamed `*.old` and none the loader's own `file-glob`s name (BepInEx: `*.dll`).
/// Only for a source such a manager keeps; a folder of any other source is the data's alone.
/// `None` when the discovery's budget ran out before the folder could be read whole.
fn switched_off(
    manager: ModManager,
    dir: &Path,
    globs: &[&crate::engines::mods::Glob],
    b: &Budget,
) -> Option<bool> {
    if !matches!(
        manager,
        ModManager::R2modman | ModManager::Thunderstore | ModManager::Gale
    ) || globs.is_empty()
    {
        return Some(false);
    }
    Some(names_below(dir, globs, b)? == Below::Renamed)
}

/// MelonLoader's mods: the `*.dll` files in `Mods/`.
fn count_melon_mods(root: &Path, b: &Budget) -> u32 {
    let Some(dir) = find_any_case(root, &["Mods"]) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .take(MAX_DIR_ENTRIES)
        .take_while(|_| b.entry())
        .filter(|e| e.path().is_file() && is_dll(&e.path()))
        .count() as u32
}

/// The modification time of the file at `rel` below `root`, in ms since the UNIX epoch.
fn modified_ms(root: &Path, rel: &[&str]) -> Option<u64> {
    let p = find_any_case(root, rel)?;
    let t = fs::metadata(p).ok()?.modified().ok()?;
    u64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The log a loader writes when it runs, below its source: what [`ModSource::last_played`] reads.
/// BepInEx's only; another loader's last run is not read.
fn log_of(kind: LoaderKind) -> Option<&'static [&'static str]> {
    match kind.family() {
        "bepinex" => Some(&["BepInEx", "LogOutput.log"]),
        _ if kind == LoaderKind::MelonLoader => Some(&["MelonLoader", "Latest.log"]),
        _ => None,
    }
}

/// A path that can be written into `chain.ini` and remembered whole: local, no line break.
fn usable(p: &Path) -> bool {
    local_verdict(p).is_ok() && p.to_str().is_some_and(|s| !s.contains(['\n', '\r', '\0']))
}

/// Whether a loader the data recognised in the game folder is Astra's own install: the ledger put
/// a game file in the folder of one of the files recognising it, or below it — Astra's per-game
/// BepInEx (`BepInEx/core/…`, 0.5.0's rule), an older runner's UE4SS (`ue4ss/…`). A recognising
/// file directly beside the program (UE4SS 2's) is Astra's only when the ledger placed that very
/// file: every runner puts its proxy beside the program.
fn astra_placed(ledger: Option<&Ledger>, game_root: Option<&Path>, files: &[PathBuf]) -> bool {
    let (Some(ledger), Some(game_root)) = (ledger, game_root) else {
        return false;
    };
    let placed: Vec<String> = ledger
        .game_files
        .iter()
        .map(|g| g.rel.to_ascii_lowercase())
        .collect();
    files.iter().any(|f| {
        let Ok(rel) = f.strip_prefix(game_root) else {
            return false;
        };
        let rel: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect();
        let Some((_, folder)) = rel.split_last() else {
            return false;
        };
        if folder.is_empty() {
            let file = rel.join("/");
            placed.contains(&file)
        } else {
            let folder = format!("{}/", folder.join("/"));
            placed.iter().any(|g| g.starts_with(&folder))
        }
    })
}

/// The built-in data's loaders recognised in `c`, for a game `p` placed: each loader's outcome
/// (§7.4–§7.8), every entry listed and byte read charged to `b`. The game folder is `{source}` as
/// detection reads it (the folder detection ran on); any other source is its own folder.
fn data_loaders(p: &Placed, c: &Candidate, b: &Budget) -> Vec<LoaderOutcome> {
    let Some(mods) = p.mods else {
        return Vec::new();
    };
    let root = if c.game_folder { &p.game_dir } else { &c.root };
    evaluate_one_within(mods, &p.code, &p.bindings, c.manager.source_kind(), root, b).loaders
}

/// Whether the data's `versions` for loader `id` hold for `need`, the version an integration
/// declares it runs beside. A need the loader's template cannot read (BepInEx 5's `5.4`, a series,
/// not a version) is not asked: `true`.
fn data_allows(mods: &Mods, id: &str, need: &str) -> bool {
    let Some(v) = mods.loader(id).and_then(|l| l.version()) else {
        return true;
    };
    match (v.versions(), v.template().read(need)) {
        (Some(want), Some((runs, _))) => want.holds(&runs),
        _ => true,
    }
}

/// The integration's verdict on a loader the built-in data recognised, in this order: a loader
/// Astra cannot run beside at all (`coexist = []`), whatever is installed; the integration
/// ([`capability`]); a declaration of this loader whose own version lies outside the data's
/// `versions` (§7.4: a foundation narrows them, never widens them), with which no loader of the
/// player's can ever be offered (`not-supported{reason=outside-versions}`, never "update to" a
/// version the other side refuses); then the declaration — the loader it names, the data's
/// `versions`, and its own narrower one ([`ModsLoader::accepts`]).
fn data_verdict(
    r: &Recognised,
    kind: LoaderKind,
    mods: &Mods,
    code: &EngineCode,
    can: &Result<(Manifest, ModsDecl), Unplayable>,
) -> Result<(), Unplayable> {
    let have = have(r.written.as_deref(), kind);
    if r.coexist.is_empty() {
        // The loader of the same engine that Astra does chain: what the player is told it needs.
        let need = mods
            .loaders()
            .iter()
            .find(|l| {
                l.applies_to(code)
                    && l.coexist()
                        .iter()
                        .any(|s| matches!(s, Coexist::Chain { .. }))
            })
            .map(|l| l.name().to_owned())
            .unwrap_or_default();
        return Err(Unplayable::LoaderUnsupported { have, need });
    }
    let (_, decl) = can.as_ref().map_err(Clone::clone)?;
    if decl.loader.id() == r.id && !data_allows(mods, &r.id, &decl.loader.need()) {
        return Err(Unplayable::NotSupported {
            reason: "outside-versions".to_owned(),
        });
    }
    let chains = r
        .coexist
        .iter()
        .any(|s| matches!(s, Strategy::Chain { .. }));
    if decl.loader.id() == r.id
        && r.supported
        && decl.loader.accepts(r.written.as_deref())
        && chains
    {
        return Ok(());
    }
    Err(Unplayable::LoaderUnsupported {
        have,
        need: decl.loader.need(),
    })
}

/// Every loader recognised in `c`, judged against what is installed (`can`, `ledger`): the
/// built-in data's first, then a BepInEx the data does not describe, then MelonLoader. Rows of no mod at all are kept:
/// [`discover`] leaves them out, a Play re-judging its source does not. `None` when `c` is not a
/// folder a row may name (a share, a line break in it).
pub(crate) fn judge(
    c: &Candidate,
    placed: Option<&Placed>,
    can: &Result<(Manifest, ModsDecl), Unplayable>,
    ledger: Option<&Ledger>,
    b: &Budget,
) -> Vec<Found> {
    let mut out = Vec::new();
    if !usable(&c.root) || c.profile.contains(['\n', '\r', '\0']) {
        return out;
    }
    let row = |source_root: &Path, loader: ModLoader, mods: u32, playable, coexist| {
        let last_played = log_of(loader.kind).and_then(|rel| modified_ms(source_root, rel));
        ModSource {
            manager: c.manager,
            profile: c.profile.clone(),
            // As a person and `chain.ini` see it: no `\\?\` prefix.
            root: plain(source_root),
            loader,
            mods,
            last_played,
            playable,
            coexist,
        }
    };
    if let Some(p) = placed
        && let Some(mods) = p.mods
    {
        let source_root = if c.game_folder { &p.game_dir } else { &c.root };
        // A loader past a bound — one evaluation's (§7.8), or the discovery's — is shown to say
        // so, with nothing of it listed.
        let over = |id: &str| {
            let kind = LoaderKind::of_id(id)?;
            let coexist = mods
                .loader(id)
                .map(|l| l.coexist().iter().map(Coexistence::of_data).collect())
                .unwrap_or_default();
            Some(Found {
                source: row(
                    source_root,
                    ModLoader {
                        kind,
                        version: None,
                        preloader: None,
                    },
                    0,
                    Err(Unplayable::NotSupported {
                        reason: "over-budget".to_owned(),
                    }),
                    coexist,
                ),
                entry: None,
            })
        };
        for outcome in data_loaders(p, c, b) {
            match outcome {
                LoaderOutcome::Recognised(r) => {
                    let Some(kind) = LoaderKind::of_id(&r.id) else {
                        continue;
                    };
                    if c.game_folder && astra_placed(ledger, p.game_root.as_deref(), &r.files) {
                        continue;
                    }
                    let loader = mods.loader(&r.id);
                    let globs: Vec<&crate::engines::mods::Glob> = loader
                        .map(|l| l.enabled())
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|e| match e {
                            Enabled::FileGlob(g) => Some(g),
                            _ => None,
                        })
                        .collect();
                    let mut count = 0usize;
                    let mut cut = false;
                    for m in r.mods.iter().filter(|m| m.enabled) {
                        let counts = !m.folder
                            || r.root.as_ref().is_none_or(|root| {
                                let dir = root.join(&m.name);
                                !astra_item(&dir)
                                    && match switched_off(c.manager, &dir, &globs, b) {
                                        Some(off) => !off,
                                        None => {
                                            cut = true;
                                            false
                                        }
                                    }
                            });
                        if cut {
                            break;
                        }
                        count += usize::from(counts);
                    }
                    if cut {
                        out.extend(over(&r.id));
                        continue;
                    }
                    let entry = r.coexist.iter().find_map(|s| match s {
                        Strategy::Chain { entry, .. } => Some(entry.clone()),
                        _ => None,
                    });
                    let playable = data_verdict(&r, kind, mods, &p.code, can);
                    out.push(Found {
                        source: row(
                            source_root,
                            ModLoader {
                                kind,
                                version: r.written.clone(),
                                preloader: entry
                                    .as_ref()
                                    .and_then(|e| e.file_name())
                                    .map(|n| n.to_string_lossy().into_owned()),
                            },
                            u32::try_from(count).unwrap_or(u32::MAX),
                            playable,
                            r.coexist.iter().map(Coexistence::of).collect(),
                        ),
                        entry,
                    });
                }
                LoaderOutcome::OverBudget { id, .. } => out.extend(over(&id)),
            }
        }
    }
    // A BepInEx the data does not describe: never beside one it does (its first preloader would
    // be the data's), so one BepInEx per folder, as 0.5.0 listed them.
    if let Some(loader) = other_bepinex_at(&c.root, b) {
        let mods = count_bepinex_mods(&c.root, b);
        let playable = match can {
            Err(why) => Err(why.clone()),
            Ok((_, decl)) => Err(Unplayable::LoaderUnsupported {
                have: have(loader.version.as_deref(), loader.kind),
                need: decl.loader.need(),
            }),
        };
        out.push(Found {
            source: row(&c.root, loader, mods, playable, Vec::new()),
            entry: None,
        });
    }
    if let Some(loader) = melonloader_at(&c.root, c.game_folder) {
        let mods = count_melon_mods(&c.root, b);
        out.push(Found {
            source: row(
                &c.root,
                loader,
                mods,
                Err(Unplayable::MelonLoader),
                Vec::new(),
            ),
            entry: None,
        });
    }
    out
}

/// Every mod source for `game`, judged against the integration installed in `profile_dir`, the
/// most recently played first (ties by profile name), each folder once per loader. The mod
/// managers' data folders are this process's ([`ModsEnv::from_process`]).
pub fn discover_mods(game: &GameTarget, profile_dir: &Path) -> Vec<ModSource> {
    discover_mods_in(game, profile_dir, &ModsEnv::from_process())
}

/// [`discover_mods`] with the mod managers' data folders given.
pub fn discover_mods_in(game: &GameTarget, profile_dir: &Path, env: &ModsEnv) -> Vec<ModSource> {
    let profile = canonical_dir(profile_dir).ok();
    let ledger = profile
        .as_deref()
        .and_then(|p| ledger::read(p).ok().flatten());
    // The folders the person added, answered or not (`"choice": null`).
    let added = profile
        .as_deref()
        .and_then(|p| read_choice_file(p).ok().flatten())
        .map(|f| f.added_roots)
        .unwrap_or_default();
    discover(game, ledger.as_ref(), &added, env)
}

pub(crate) fn discover(
    game: &GameTarget,
    ledger: Option<&Ledger>,
    added_roots: &[PathBuf],
    env: &ModsEnv,
) -> Vec<ModSource> {
    let can = capability(game, ledger);
    let budget = Budget::new();
    let placed = Placed::of(game);
    let appid = game
        .steam_appid
        .or_else(|| steam_appid_of_dir(&game.dir).ok().flatten());
    let look = Lookup {
        row: appid.and_then(games::row),
        installdir: game.dir.file_name().and_then(|n| n.to_str()),
        stem: game.exe_file_name().ok().and_then(|n| {
            Path::new(&n)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        }),
        budget: &budget,
    };

    let mut candidates = Vec::new();
    for (manager, data) in env.manager_roots() {
        look.game_profiles(manager, &data, &mut candidates);
    }
    // Never a share, a device or a verbatim namespace: checked before anything touches it.
    // The newest 32 (a file edited by hand may hold more).
    let newest = added_roots.len().saturating_sub(MAX_ADDED_ROOTS);
    for root in added_roots[newest..]
        .iter()
        .filter(|r| local_verdict(r).is_ok())
    {
        added_root(&look, root, &mut candidates);
    }
    if let Ok(game_root) = game.game_root() {
        candidates.push(Candidate {
            manager: ModManager::Manual,
            profile: String::new(),
            root: game_root,
            game_folder: true,
        });
    }

    sources(&candidates, placed.as_ref(), &can, ledger, &budget)
}

/// The sources of `candidates`, in order, each folder once per loader family, the most recently
/// played first. Every look is charged to `budget`; once it is spent nothing more is looked at (a
/// source it cut short is shown over budget).
fn sources(
    candidates: &[Candidate],
    placed: Option<&Placed>,
    can: &Result<(Manifest, ModsDecl), Unplayable>,
    ledger: Option<&Ledger>,
    budget: &Budget,
) -> Vec<ModSource> {
    let mut seen: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut out: Vec<ModSource> = Vec::new();
    for c in candidates {
        if budget.spent() {
            break;
        }
        for f in judge(c, placed, can, ledger, budget) {
            let s = f.source;
            // A profile with nothing in it is no one's mods; MelonLoader in the game folder
            // loads into every start, mods or not; a loader past a bound is shown to say so.
            let over = matches!(&s.playable, Err(Unplayable::NotSupported { reason }) if reason == "over-budget");
            let melon_here = s.loader.kind == LoaderKind::MelonLoader && c.game_folder;
            if s.mods == 0 && !melon_here && !over {
                continue;
            }
            let key = (
                fs::canonicalize(&s.root).unwrap_or_else(|_| s.root.clone()),
                s.loader.kind.family(),
            );
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            out.push(s);
        }
    }
    out.sort_by(|a, b| {
        b.last_played
            .cmp(&a.last_played)
            .then_with(|| a.profile.cmp(&b.profile))
            .then_with(|| a.manager.cmp(&b.manager))
            .then_with(|| a.root.cmp(&b.root))
    });
    out
}

/// The source offered first: the most recently played one that can be played together, ties by
/// profile name — the first offered in [`discover_mods`]' order.
pub fn default_mods_source(sources: &[ModSource]) -> Option<&ModSource> {
    sources.iter().find(|s| s.playable.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_read_from_bepinex_s_start_line() {
        let line = "[Message:   BepInEx] BepInEx 5.4.23.3 - PEAK (10/30/2026 1:23:45 PM)";
        assert_eq!(logged_program(line), Some("PEAK"));
        let spaced = "[Message:   BepInEx] BepInEx 6.0.0-be.788+5b766a3 - Lethal Company (x) (30.10.2026 13:23:45)";
        assert_eq!(logged_program(spaced), Some("Lethal Company (x)"));
        assert_eq!(
            logged_program("[Error  : Unity Log] Unable to start Unity log writer"),
            None
        );
    }

    #[test]
    fn one_budget_bounds_every_listing_and_read_of_a_discovery() {
        let dir = std::env::temp_dir().join(format!("agl-budget-{}", std::process::id()));
        for i in 0..5 {
            fs::create_dir_all(dir.join(format!("d{i}"))).unwrap();
        }
        let b = Budget {
            entries: std::cell::Cell::new(3),
            bytes: std::cell::Cell::new(10),
        };
        assert_eq!(sorted_dirs(&dir, &b).len(), 3);
        assert!(
            sorted_dirs(&dir, &b).is_empty(),
            "spent: nothing more is listed"
        );
        assert!(b.bytes(10) && !b.bytes(1));
        let _ = fs::remove_dir_all(&dir);
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agl-mods-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"MZ").unwrap();
    }

    fn budget(entries: usize) -> Budget {
        Budget {
            entries: std::cell::Cell::new(entries),
            bytes: std::cell::Cell::new(BUDGET_BYTES),
        }
    }

    /// A Unity Mono game as the built-in rule places it: the built-in `mods` for `unity-mono`.
    fn mono() -> Placed {
        let code = EngineCode::new("unity-mono").unwrap();
        Placed {
            mods: builtin::for_code(&code),
            code,
            bindings: Bindings::new(),
            game_dir: PathBuf::from("/nonexistent"),
            game_root: None,
        }
    }

    /// An r2modman profile at `root` holding a BepInEx 5.
    fn r2_profile(root: &Path) -> Candidate {
        for f in ["BepInEx.dll", "BepInEx.Preloader.dll"] {
            put(&root.join("BepInEx/core").join(f));
        }
        candidate(ModManager::R2modman, root.to_path_buf())
    }

    fn summary(sources: &[ModSource]) -> Vec<(String, u32, Result<(), Unplayable>)> {
        sources
            .iter()
            .map(|s| (s.profile.clone(), s.mods, s.playable.clone()))
            .collect()
    }

    /// Every entry an evaluation of the built-in data lists is charged to the discovery's one
    /// budget: a source whose look the budget cuts short is shown over budget, never a partial
    /// list, and no source after it is looked at.
    #[test]
    fn every_evaluation_is_charged_to_the_discoverys_one_budget() {
        let dir = tmp("charged");
        let candidates: Vec<Candidate> = (0..4)
            .map(|i| {
                let root = dir.join(format!("P{i}"));
                put(&root.join("BepInEx/plugins/Mod.dll"));
                for n in 0..99 {
                    put(&root.join(format!("BepInEx/plugins/notes{n}.txt")));
                }
                r2_profile(&root)
            })
            .collect();
        let can = Err(Unplayable::NotSupported {
            reason: "not-installed".to_owned(),
        });
        let not = can.clone().unwrap_err();
        let whole = Budget::new();
        let all = sources(&candidates, Some(&mono()), &can, None, &whole);
        assert_eq!(
            summary(&all),
            (0..4)
                .map(|i| (format!("P{i}"), 1, Err(not.clone())))
                .collect::<Vec<_>>()
        );
        // Four listings of a hundred plugins' entries: each charged.
        assert!(BUDGET_ENTRIES - whole.entries.get() >= 400);
        let over = Err(Unplayable::NotSupported {
            reason: "over-budget".to_owned(),
        });
        let cut = sources(&candidates, Some(&mono()), &can, None, &budget(250));
        assert_eq!(
            summary(&cut),
            [
                ("P0".to_owned(), 1, Err(not.clone())),
                ("P1".to_owned(), 1, Err(not.clone())),
                ("P2".to_owned(), 0, over),
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A mod folder an r2modman profile holds is read for its manager's switch only as far as the
    /// budget goes: cut short, the source is over budget, never a guess that the mod is enabled.
    #[test]
    fn a_mod_folder_the_budget_cannot_read_whole_is_never_guessed_enabled() {
        let dir = tmp("cut");
        let root = dir.join("P");
        for n in 0..300 {
            put(&root.join(format!("BepInEx/plugins/Big/data{n}.txt")));
        }
        put(&root.join("BepInEx/plugins/Big/bin/Big.dll"));
        put(&root.join("BepInEx/plugins/ZOff/ZOff.dll.old"));
        let c = [r2_profile(&root)];
        let can = Err(Unplayable::NotSupported {
            reason: "not-installed".to_owned(),
        });
        let whole = sources(&c, Some(&mono()), &can, None, &Budget::new());
        assert_eq!(
            summary(&whole),
            [("P".to_owned(), 1, Err(can.clone().unwrap_err()))]
        );
        let cut = sources(&c, Some(&mono()), &can, None, &budget(102));
        assert_eq!(
            summary(&cut),
            [(
                "P".to_owned(),
                0,
                Err(Unplayable::NotSupported {
                    reason: "over-budget".to_owned()
                })
            )]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A mod folder that holds a file its loader loads is not walked any further.
    #[test]
    fn an_enabled_mod_is_not_walked_past_its_first_loadable_file() {
        let dir = tmp("first");
        put(&dir.join("A.dll"));
        for n in 0..50 {
            put(&dir.join(format!("data/d{n}/x.txt")));
        }
        // BepInEx 5's own `file-glob`, `*.dll`.
        let glob = builtin::for_code(&EngineCode::new("unity-mono").unwrap())
            .and_then(|m| m.loader("bepinex5"))
            .unwrap()
            .enabled()
            .iter()
            .find_map(|e| match e {
                Enabled::FileGlob(g) => Some(g),
                _ => None,
            })
            .unwrap();
        assert_eq!(glob.as_str(), "*.dll");
        let b = Budget::new();
        assert_eq!(names_below(&dir, &[glob], &b), Some(Below::Loadable));
        assert!(BUDGET_ENTRIES - b.entries.get() <= 2, "{}", b.entries.get());
        fs::remove_file(dir.join("A.dll")).unwrap();
        put(&dir.join("data/d7/A.dll.old"));
        assert_eq!(names_below(&dir, &[glob], &b), Some(Below::Renamed));
        assert_eq!(names_below(&dir, &[glob], &budget(3)), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_and_builds() {
        assert_eq!(short("5.4.23.6+b89fcc8"), "5.4.23.6");
        assert_eq!(decl::be_build("6.0.0-be.788+5b766a3"), Some(788));
        assert_eq!(decl::be_build("6.0.0-be.788"), Some(788));
        assert_eq!(decl::be_build("6.0.0-dev+3a12d18"), None);
        assert_eq!(decl::be_build("6.0.0-be.+x"), None);
    }

    /// Every loader of the built-in data is a kind a row can name, and its code is the loader's
    /// id; the kinds the data names are the only ones [`LoaderKind::of_id`] gives.
    #[test]
    fn every_builtin_loader_is_a_kind_a_row_names() {
        let ids: Vec<&str> = builtin::descriptors()
            .iter()
            .filter_map(|d| d.mods())
            .flat_map(Mods::loaders)
            .map(|l| l.id())
            .collect();
        assert_eq!(
            ids,
            [
                "bepinex5",
                "bepinex6-il2cpp",
                "ue4ss3",
                "ue4ss2",
                "unreal-paks"
            ]
        );
        for id in &ids {
            assert_eq!(LoaderKind::of_id(id).map(LoaderKind::code), Some(*id));
        }
        for k in [
            LoaderKind::Bepinex6Mono,
            LoaderKind::Bepinex6Coreclr,
            LoaderKind::MelonLoader,
        ] {
            assert_eq!(LoaderKind::of_id(k.code()), None, "{k:?}");
        }
        // The two BepInEx chain entries are the preloaders 0.5.0 played with mods.
        assert!(data_entry_name("BepInEx.Preloader.dll"));
        assert!(data_entry_name("bepinex.unity.il2cpp.DLL"));
        for other in [
            "BepInEx.Unity.Mono.Preloader.dll",
            "BepInEx.IL2CPP.dll",
            "BepInEx.NET.CoreCLR.dll",
        ] {
            assert!(!data_entry_name(other), "{other}");
        }
    }

    /// A row's kinds and strategies serialise as their codes (the CLI's JSON, the daemon's row).
    #[test]
    fn kinds_and_strategies_serialise_as_their_codes() {
        for k in [
            LoaderKind::Bepinex5,
            LoaderKind::Bepinex6Mono,
            LoaderKind::Bepinex6Il2cpp,
            LoaderKind::Bepinex6Coreclr,
            LoaderKind::MelonLoader,
            LoaderKind::Ue4ss3,
            LoaderKind::Ue4ss2,
            LoaderKind::UnrealPaks,
        ] {
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::Value::from(k.code())
            );
        }
        for c in [
            Coexistence::Chain,
            Coexistence::SideBySide,
            Coexistence::ViaManager,
        ] {
            assert_eq!(
                serde_json::to_value(c).unwrap(),
                serde_json::Value::from(c.code())
            );
        }
    }
}
