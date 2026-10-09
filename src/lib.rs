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
pub use detect::{Binary, Confidence, Detection, Engine, detect};
pub use error::{LauncherError, Result, codes};
#[cfg(feature = "cli")]
pub use fetch::UreqFetch;
pub use fetch::{Fetch, MAX_DOWNLOAD_BYTES, fetch_verified};
pub use game::{GameTarget, Platform, proton_prefix_for};
pub use install::{UninstallReport, install, installed, uninstall};
pub use launch::{ProcessPlan, Spawn, StdSpawn, find_steam, launch, prepare_launch};
pub use ledger::{
    LEDGER_SCHEMA, Ledger, LedgerBackup, LedgerFile, LedgerGame, LedgerGameFile, LedgerSource,
    LedgerState, ProtonRecord,
};
pub use manifest::{
    ConfigWrite, FileRule, GameFileRule, IntegrationInfo, Launch, MANIFEST_FILE, Manifest, SCHEMA,
    Target,
};
pub use package::{GiPackage, Limits, PackageSummary, SummaryConfigWrite, SummaryGameFile};
pub use plan::{GameAction, GameFileAction, InstallPlan, plan_install};
pub use steam_appinfo::{launch_executable, steam_roots};

/// The ledger's file name inside a profile.
pub const LEDGER_FILE_NAME: &str = paths::LEDGER_FILE;
