// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `astra-game-launcher` puts a game-integration mod into a game and launches the game with it.
//! Minimal, no UI: r2modman/Thunderstore in spirit, for one integration per game.
//!
//! The flow: [`GiPackage::open`] verifies an `astra-gi.zip` against its SHA-256 and reads its
//! manifest; [`plan_install`] says exactly what would happen (the consent sheet, as data;
//! [`plan_install_in`] when the game's engine may be one an engine descriptor describes);
//! [`install`] does that and writes a ledger; [`prepare_launch`] builds the command and
//! [`launch`] starts it; [`uninstall`] puts everything back, and [`restore_game_files`] puts the
//! game's files back and keeps the profile (a withdrawn integration).
//!
//! Every refusal is a [`LauncherError`]: a stable code plus params. The library never elevates,
//! never runs a program from a package, and writes into the game folder only the files the
//! manifest names beside the exe, each one pinned by its digest and backed up when it replaces
//! a game file.

pub mod anti_cheat_services;
mod ctx;
mod detect;
mod digest;
mod doorstop;
pub mod engines;
mod error;
mod fetch;
mod former;
mod game;
mod ini;
mod install;
mod launch;
mod ledger;
mod manifest;
mod mods;
mod package;
mod paths;
mod plan;
mod proton;
pub mod reserved;
mod running;
pub mod steam_appinfo;
mod steam_userconfig;
mod zip_consistency;

pub use ctx::{Ctx, Progress};
pub use detect::{
    ANTI_CHEAT_CODES, ANTI_CHEAT_SIGNATURES, AntiCheatSignature, Binary, Confidence, Detection,
    Engine, ExePick, SignatureName, detect, detect_with, detect_with_engines, unity_version_parts,
};
pub use engines::{
    AntiCheatMarker, AntiCheatMarkerKind, Descriptor, EngineCode, EngineSet, Moderation, Origin,
    Skipped, is_reserved_code, is_valid_engine_code,
};
pub use error::{LauncherError, Result, codes};
#[cfg(feature = "cli")]
pub use fetch::UreqFetch;
pub use fetch::{Fetch, MAX_DOWNLOAD_BYTES, fetch_verified};
pub use former::FormerKind;
pub use game::{GameTarget, Platform, proton_prefix_for, steam_appid_of_dir, steam_appids_of_dir};
pub use install::{
    KEPT_FOLDER_GAINED_FILES, UninstallReport, install, install_in, installed, restore_game_files,
    uninstall,
};
pub use launch::{
    MODS_CHOICE_UNREADABLE, MODS_PACK_PATH_REFUSED, ProcessPlan, STEAM_LAUNCH_OPTIONS_DOORSTOP,
    Spawn, StdSpawn, find_steam, launch, prepare_launch, prepare_launch_with,
    remove_play_arm_if_older,
};
pub use ledger::{
    LEDGER_SCHEMA, Ledger, LedgerBackup, LedgerFile, LedgerGame, LedgerGameFile, LedgerSource,
    LedgerState, LoaderBaseline, ProtonRecord, ProxyFolder,
};
pub use manifest::{
    BRIDGE_TOKEN_VARIABLE, COMPANION_PRESENTATION, ConfigWrite, FileRule, FoundationClaim,
    GameFileRule, IntegrationInfo, KEEP_EXTENSIONS, Launch, LoaderDecl, MANIFEST_FILE, Manifest,
    SCHEMA, Target, link_limits, wine_dll_overrides,
};
pub use mods::{
    ArmedBy, Choice, ChoiceKind, ChoiceSource, LOADER_STATUS_FILE_NAME, LastRun, LoaderKind,
    LoaderReason, LoaderStatus, MODS_CHOICE_SCHEMA, ModLoader, ModManager, ModSource, ModsDecl,
    ModsEnv, ModsLoader, Unplayable, add_mods_root, default_mods_source, discover_mods,
    discover_mods_in, loader_status, mods_choice, mods_last_run, set_mods_choice,
};
pub use package::{
    GiPackage, Limits, PackageSummary, PlacedFile, SummaryConfigWrite, SummaryGameFile,
};
pub use plan::{
    Accept, FormerFile, GameAction, GameFileAction, HIJACK_NAMES, InstallPlan, PinnedDescriptor,
    Provenance, VALVE_ANTI_CHEAT, is_hijack_name, plan_install, plan_install_in, vac_verdict,
};
pub use running::is_running;
pub use steam_appinfo::{
    ONLINE_CATEGORIES, VAC_CATEGORY, launch_executable, launch_executables, online_codes,
    online_game, steam_roots, store_categories, store_categories_of, valve_anti_cheat,
    valve_anti_cheat_of,
};
pub use steam_userconfig::{launch_options_pass_doorstop, steam_user_launch_options};

/// The ledger's file name inside a profile.
pub const LEDGER_FILE_NAME: &str = paths::LEDGER_FILE;

/// The play-arm file's name inside a profile: `{"schema":1,"appid":…,"started_unix_ms":…}`,
/// written by [`launch`] immediately before it spawns a Steam game (see the `launch` module's
/// docs).
pub const PLAY_ARM_FILE_NAME: &str = paths::PLAY_ARM_FILE;

/// The mods choice's file name inside a profile (see [`set_mods_choice`]).
pub const MODS_CHOICE_FILE_NAME: &str = paths::MODS_CHOICE_FILE;

/// `path-escapes` (`reason` = `unc`, `verbatim-unc`, `device`, `verbatim`, `not-absolute`) unless
/// `path` is an absolute drive-letter path on Windows (a mapped network drive or `subst` behind the
/// letter is the user's own setting and is followed), or from the root on Unix — judged from its
/// spelling alone, before anything touches it: on Windows merely asking about `\\host\share`
/// opens an SMB session with the user's credentials, and `\\.\pipe\…` connects to a pipe. Every
/// folder the launcher is given goes through this; a caller about to touch one may too.
pub fn check_local_path(path: &std::path::Path) -> Result<()> {
    paths::require_local(path)
}

/// Whether `name` is one path component the launcher would write on every platform it serves:
/// not empty, no `/`, `\`, `:`, `<>"|?*` or control character, not `.` or `..`, no trailing dot
/// or space, no Windows device name (`CON`, `NUL`, `COM1`, `LPT¹`, …), at most 255 bytes. A
/// caller that lays files from data someone else made (a recording's replay, `gi-probe
/// validate-descriptor --corpus`) holds every name to it before joining it to a path: on Windows
/// `..\x`, `C:\…` and `\\host\share\x` are paths, not names.
pub fn is_portable_name(name: &str) -> bool {
    paths::check_component(name)
}

/// This build's own version (`Cargo.toml`'s `[package].version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SHA-256 of this crate's own source tree, computed by `build.rs` (see its doc comment for the
/// exact algorithm). The CLI's `--version` prints it; a release tag's GitHub page publishes it;
/// the README has a shell one-liner anyone can run against a clone to get the same hex back.
/// This is what makes Astra's pinned git dependency on this crate verifiable: Astra compiles in
/// a commit, that commit's tree hashes to this, and that hash is the one the release notes show.
pub const SOURCE_TREE_SHA256: &str = env!("SOURCE_TREE_SHA256");
