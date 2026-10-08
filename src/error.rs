// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Every refusal is a stable CODE plus params. The library never returns prose: the English
//! `Display` text exists for the command-line program only.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

/// The stable refusal codes. A caller matches on these strings; their meaning never changes.
pub mod codes {
    // Fetch and package.
    pub const FETCH_FAILED: &str = "fetch-failed";
    pub const TOO_LARGE: &str = "too-large";
    pub const DIGEST_MISMATCH: &str = "digest-mismatch";
    pub const ZIP_UNSAFE_ENTRY: &str = "zip-unsafe-entry";
    pub const ZIP_TOO_MANY_ENTRIES: &str = "zip-too-many-entries";
    pub const MANIFEST_MISSING: &str = "manifest-missing";
    pub const MANIFEST_INVALID: &str = "manifest-invalid";
    pub const SCHEMA_TOO_NEW: &str = "schema-too-new";
    pub const UNKNOWN_KEY: &str = "unknown-key";
    pub const UNKNOWN_PLACEHOLDER: &str = "unknown-placeholder";
    // Game.
    pub const ENGINE_MISMATCH: &str = "engine-mismatch";
    pub const EXE_NOT_FOUND: &str = "exe-not-found";
    pub const ANTI_CHEAT: &str = "anti-cheat";
    pub const FOREIGN_LOADER: &str = "foreign-loader";
    /// The manifest names a different game (`field` = "exe" or "steam_appid").
    pub const GAME_MISMATCH: &str = "game-mismatch";
    /// The game's platform is not in the manifest's `platforms`, or cannot be launched.
    pub const PLATFORM_UNSUPPORTED: &str = "platform-unsupported";
    // Install state.
    pub const PATH_ESCAPES: &str = "path-escapes";
    pub const NOT_INSTALLED: &str = "not-installed";
    pub const GAME_RUNNING: &str = "game-running";
    /// The game folder or profile changed between `plan_install` and `install`.
    pub const PLAN_STALE: &str = "plan-stale";
    /// A file the ledger records is missing or changed; install again.
    pub const NEEDS_REINSTALL: &str = "needs-reinstall";
    // Proton.
    pub const PROTON_PREFIX_MISSING: &str = "proton-prefix-missing";
    // Launch.
    pub const STEAM_NOT_FOUND: &str = "steam-not-found";
    // Other.
    pub const MISSING_VARIABLE: &str = "missing-variable";
    /// A variable's value holds a character it may not carry (a newline into an INI, a NUL).
    pub const INVALID_VARIABLE: &str = "invalid-variable";
    pub const CANCELLED: &str = "cancelled";
    pub const IO: &str = "io";
    // Command-line program only.
    pub const USAGE: &str = "usage";
    pub const CONSENT_REQUIRED: &str = "consent-required";
}

/// A refusal or failure: a stable code and its parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherError {
    pub code: &'static str,
    pub params: BTreeMap<String, String>,
}

pub type Result<T> = std::result::Result<T, LauncherError>;

impl LauncherError {
    pub fn new(code: &'static str) -> Self {
        LauncherError {
            code,
            params: BTreeMap::new(),
        }
    }

    /// Adds one parameter.
    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.params.insert(key.to_owned(), value.into());
        self
    }

    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }

    pub fn io(detail: impl fmt::Display) -> Self {
        LauncherError::new(codes::IO).with("detail", detail.to_string())
    }

    pub(crate) fn io_at(err: &std::io::Error, path: &std::path::Path) -> Self {
        LauncherError::new(codes::IO)
            .with("detail", err.to_string())
            .with("path", path.display().to_string())
    }

    fn sentence(&self) -> &'static str {
        match self.code {
            codes::FETCH_FAILED => "the download failed",
            codes::TOO_LARGE => "the package is larger than allowed",
            codes::DIGEST_MISMATCH => "the file's SHA-256 is not the expected one",
            codes::ZIP_UNSAFE_ENTRY => "the zip holds an unsafe entry",
            codes::ZIP_TOO_MANY_ENTRIES => "the zip holds too many entries",
            codes::MANIFEST_MISSING => "the zip has no astra-gi.toml at its root",
            codes::MANIFEST_INVALID => "astra-gi.toml is not valid",
            codes::SCHEMA_TOO_NEW => "this integration needs a newer launcher",
            codes::UNKNOWN_KEY => "astra-gi.toml uses a key this launcher does not know",
            codes::UNKNOWN_PLACEHOLDER => {
                "astra-gi.toml uses a placeholder where it is not allowed"
            }
            codes::ENGINE_MISMATCH => "the game is not made with the engine this integration needs",
            codes::EXE_NOT_FOUND => "the game's program was not found",
            codes::ANTI_CHEAT => "the game uses anti-cheat; mods there can get the account banned",
            codes::FOREIGN_LOADER => "another mod loader is installed beside the game",
            codes::GAME_MISMATCH => "this integration is for a different game",
            codes::PLATFORM_UNSUPPORTED => "this integration does not support the game's platform",
            codes::PATH_ESCAPES => "a path leaves the folder it must stay in",
            codes::NOT_INSTALLED => "nothing is installed in this profile",
            codes::GAME_RUNNING => "the game is running; close it first",
            codes::PLAN_STALE => "the game folder changed since the plan was made; plan again",
            codes::NEEDS_REINSTALL => "an installed file is missing or changed; install again",
            codes::PROTON_PREFIX_MISSING => {
                "the game's Proton prefix does not exist yet; start the game once from Steam"
            }
            codes::STEAM_NOT_FOUND => "Steam was not found",
            codes::MISSING_VARIABLE => "a variable the integration needs was not given",
            codes::INVALID_VARIABLE => "a variable's value holds a forbidden character",
            codes::CANCELLED => "cancelled",
            codes::IO => "a file operation failed",
            codes::USAGE => "bad command line",
            codes::CONSENT_REQUIRED => "pass --yes to install after reading the plan",
            _ => "error",
        }
    }
}

impl fmt::Display for LauncherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.sentence(), self.code)?;
        for (k, v) in &self.params {
            write!(f, "; {k}: {v}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LauncherError {}

impl From<std::io::Error> for LauncherError {
    fn from(err: std::io::Error) -> Self {
        LauncherError::io(err)
    }
}
