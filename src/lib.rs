// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `astra-game-launcher` puts a game-integration mod into a game and launches the game with it.
//! Minimal, no UI: r2modman/Thunderstore in spirit, for one integration per game.
//!
//! The flow: [`GiPackage::open`] verifies an `astra-gi.zip` against its SHA-256 and reads its
//! manifest; [`plan_install`] says exactly what would happen (the consent sheet, as data);
//! [`install`] does that and writes a ledger; [`prepare_launch`] builds the command and
//! [`launch`] starts it; [`uninstall`] puts everything back.
//!
//! Every refusal is a [`LauncherError`]: a stable code plus params. The library never elevates,
//! never runs a program from a package, and writes into the game folder only the files the
//! manifest names beside the exe, each one pinned by its digest and backed up when it replaces
//! a game file.

mod ctx;
mod detect;
mod digest;
mod doorstop;
mod error;
mod fetch;
mod game;
mod ini;
mod install;
mod launch;
mod ledger;
mod manifest;
mod package;
mod paths;
mod plan;
mod proton;
mod running;
pub mod steam_appinfo;

pub use ctx::{Ctx, Progress};
pub use detect::{
    Binary, Confidence, Detection, Engine, ExePick, detect, detect_with, unity_version_parts,
};
pub use error::{LauncherError, Result, codes};
#[cfg(feature = "cli")]
pub use fetch::UreqFetch;
pub use fetch::{Fetch, MAX_DOWNLOAD_BYTES, fetch_verified};
pub use game::{GameTarget, Platform, proton_prefix_for, steam_appid_of_dir, steam_appids_of_dir};
pub use install::{UninstallReport, install, installed, uninstall};
pub use launch::{ProcessPlan, Spawn, StdSpawn, find_steam, launch, prepare_launch};
pub use ledger::{
    LEDGER_SCHEMA, Ledger, LedgerBackup, LedgerFile, LedgerGame, LedgerGameFile, LedgerSource,
    LedgerState, LoaderBaseline, ProtonRecord,
};
pub use manifest::{
    ConfigWrite, FileRule, GameFileRule, IntegrationInfo, Launch, MANIFEST_FILE, Manifest, SCHEMA,
    Target, wine_dll_overrides,
};
pub use package::{GiPackage, Limits, PackageSummary, SummaryConfigWrite, SummaryGameFile};
pub use plan::{
    Accept, GameAction, GameFileAction, InstallPlan, VALVE_ANTI_CHEAT, plan_install, vac_verdict,
};
pub use running::is_running;
pub use steam_appinfo::{
    launch_executable, launch_executables, steam_roots, valve_anti_cheat, valve_anti_cheat_of,
};

/// The ledger's file name inside a profile.
pub const LEDGER_FILE_NAME: &str = paths::LEDGER_FILE;

/// This build's own version (`Cargo.toml`'s `[package].version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SHA-256 of this crate's own source tree, computed by `build.rs` (see its doc comment for the
/// exact algorithm). The CLI's `--version` prints it; a release tag's GitHub page publishes it;
/// the README has a shell one-liner anyone can run against a clone to get the same hex back.
/// This is what makes Astra's pinned git dependency on this crate verifiable: Astra compiles in
/// a commit, that commit's tree hashes to this, and that hash is the one the release notes show.
pub const SOURCE_TREE_SHA256: &str = env!("SOURCE_TREE_SHA256");
