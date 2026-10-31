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
    /// Steam's VAC flag could not be read and the caller did not pass `Accept::vac_unchecked`.
    pub const VAC_UNCHECKED: &str = "vac-unchecked";
    pub const FOREIGN_LOADER: &str = "foreign-loader";
    /// Play: the loader proxy shared with another mod manager (`file`) was replaced by one that
    /// no longer takes our launch options. Installing again would answer `foreign-loader`.
    pub const SHARED_LOADER_CHANGED: &str = "shared-loader-changed";
    /// Plan: a mod loader in the game folder is switched off by its missing proxy, while its
    /// `doorstop_config.ini` (`file`) still switches Doorstop on with a target that exists
    /// (`target`). Adding our proxy would bring its mods back on every plain start of the game.
    pub const SWITCHED_OFF_LOADER: &str = "switched-off-loader";
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
    // Play with the player's own mods.
    /// Play: the integration can play together with the player's own mods, some are offered
    /// (`sources`, how many) and the player was never asked — or their answer cannot be read
    /// (`choice_unreadable`, why): ask once ([`crate::set_mods_choice`]), then Play again.
    pub const MODS_CHOICE_NEEDED: &str = "mods-choice-needed";
    /// Play with mods: the chosen mod profile (`profile`, `manager`, `root`) is gone, or no longer
    /// holds its BepInEx preloader (`reason` = `no-preloader`), or its .NET runtime (`file`).
    pub const MODS_SOURCE_MISSING: &str = "mods-source-missing";
    /// Play with mods: their BepInEx (`have`) is not one the integration's chain runs beside
    /// (`need`: `5.4`, `6.0.0-be.788`); `profile` names it.
    pub const MODS_LOADER_UNSUPPORTED: &str = "mods-loader-unsupported";
    /// Play with mods: the chosen mods run through MelonLoader (`profile`).
    pub const MODS_MELONLOADER: &str = "mods-melonloader";
    /// Play with mods: the installed integration declares `[integration.mods]` and still cannot
    /// here (`reason` = `platform`: a native Linux game; `no-doorstop`: its launch goes through no
    /// Doorstop proxy beside the game). One with no table plays Astra alone, never this.
    pub const MODS_UNSUPPORTED: &str = "mods-unsupported";
    // Engine descriptors (`astra.engine/1`, [`crate::engines`]). Each refusal costs that one
    // descriptor only; the daemon raises one banner for all of them.
    /// Not a descriptor this launcher can read (`field`, `reason`): a key the schema does not
    /// name, a float, a datetime, a null, a duplicate key, a bad path or placeholder, a limit.
    pub const DESCRIPTOR_INVALID: &str = "descriptor-invalid";
    /// The descriptor's `schema` (`schema`) is not one this launcher reads (`supported`).
    pub const DESCRIPTOR_SCHEMA_UNKNOWN: &str = "descriptor-schema-unknown";
    /// Larger than a descriptor may be (`bytes`).
    pub const DESCRIPTOR_TOO_LARGE: &str = "descriptor-too-large";
    /// A rule (`field`) with no usable anchor (`reason` = `no-anchor`, `program`, `wrapper`,
    /// `ubiquitous-runtime`, `generic-name`, `too-short`): it would claim games it never saw.
    pub const DESCRIPTOR_TOO_BROAD: &str = "descriptor-too-broad";
    /// A part of the grammar (`field`) only Astra's own descriptors may use.
    pub const DESCRIPTOR_FIRST_PARTY_ONLY: &str = "descriptor-first-party-only";
    /// A part of the grammar (`field`) this launcher parses but does not evaluate yet.
    pub const DESCRIPTOR_UNSUPPORTED: &str = "descriptor-unsupported";
    /// The same descriptor (by digest) was admitted already.
    pub const DESCRIPTOR_DUPLICATE: &str = "descriptor-duplicate";
    /// Past what Pass B asks (`reason` = `total`: 64 descriptors; `publisher`: 4 from one).
    pub const DESCRIPTOR_OVER_LIMIT: &str = "descriptor-over-limit";
    /// The descriptor declares a reserved engine code (`code`): a built-in one, `unknown`, or
    /// one that reads as either.
    pub const ENGINE_CODE_RESERVED: &str = "engine-code-reserved";
    /// The display name (`name`) of `code` reads as a built-in engine's, or as a review.
    pub const ENGINE_NAME_RESERVED: &str = "engine-name-reserved";
    /// `code` belongs to another publisher (`owner`), admitted with it first.
    pub const ENGINE_CODE_TAKEN: &str = "engine-code-taken";
    /// Not a refusal of the launcher's: the state of a [`crate::Detection`] whose
    /// `engine_candidates` are not empty — descriptors of one standing disagree.
    pub const ENGINE_AMBIGUOUS: &str = "engine-ambiguous";
    /// Not a refusal of the launcher's: the anti-cheat entry a descriptor's claim carries when
    /// the walk stopped at its cap or a kernel driver (`*.sys`) ships beside the game.
    pub const ANTI_CHEAT_UNKNOWN: &str = "anti-cheat-unknown";
    // `[integration.*]` (`crate::Manifest`) and what is checked before publishing
    // (`crate::Manifest::lint`, `crate::reserved`).
    /// `[integration.engine]` declares engine codes (`codes`) and `[target] engine` (`engine`,
    /// empty when missing) is not one of them: a foundation owns the code it detects.
    pub const ENGINE_UNDECLARED: &str = "engine-undeclared";
    /// The integration asks for a capability (`capability`) this launcher does not know: a newer
    /// Astra grants it, and it must not look granted here.
    pub const NEEDS_NEWER_ASTRA: &str = "needs-newer-astra";
    /// `[integration.loader]` keeps (`keep_if_present`) a file it also hijack-loads (`proxies`),
    /// or a loader (`reason` = `proxy` or `loader`; `name`).
    pub const LOADER_POLICY_CONFLICT: &str = "loader-policy-conflict";
    /// The integration hands Astra's avatar bridge token on (`variable`) without declaring the
    /// capability (`capability` = `companion.presentation`) in `[integration.capabilities]`.
    pub const CAPABILITY_UNDECLARED: &str = "capability-undeclared";
    /// A community integration's id (`id`) begins with a first-party prefix (`prefix`).
    pub const RESERVED_ID: &str = "reserved-id";
    /// A community integration's name (`name`) holds a first-party word.
    pub const RESERVED_NAME: &str = "reserved-name";
    /// `[integration.link]` past a rule or limit of Astra Link v1 (`field`; `reason` = `shape`,
    /// `too-many`, `name`, `duplicate`, `too-long`, `missing`, `not-a-path`, `not-in-zip`,
    /// `too-large`; `limit` where there is one).
    pub const LINK_INVALID: &str = "link-invalid";
    // Other.
    pub const MISSING_VARIABLE: &str = "missing-variable";
    /// A variable's value holds a character it may not carry (a newline into an INI, a NUL).
    pub const INVALID_VARIABLE: &str = "invalid-variable";
    pub const CANCELLED: &str = "cancelled";
    pub const IO: &str = "io";
    // Command-line program only.
    pub const USAGE: &str = "usage";
    pub const CONSENT_REQUIRED: &str = "consent-required";

    /// Every code above, in the order they are declared: what a caller that maps codes to its own
    /// words (the daemon's `games.state.*`, canary K15) must cover. A test holds it equal to the
    /// constants of this module.
    pub const ALL: &[&str] = &[
        FETCH_FAILED,
        TOO_LARGE,
        DIGEST_MISMATCH,
        ZIP_UNSAFE_ENTRY,
        ZIP_TOO_MANY_ENTRIES,
        MANIFEST_MISSING,
        MANIFEST_INVALID,
        SCHEMA_TOO_NEW,
        UNKNOWN_KEY,
        UNKNOWN_PLACEHOLDER,
        ENGINE_MISMATCH,
        EXE_NOT_FOUND,
        ANTI_CHEAT,
        VAC_UNCHECKED,
        FOREIGN_LOADER,
        SHARED_LOADER_CHANGED,
        SWITCHED_OFF_LOADER,
        GAME_MISMATCH,
        PLATFORM_UNSUPPORTED,
        PATH_ESCAPES,
        NOT_INSTALLED,
        GAME_RUNNING,
        PLAN_STALE,
        NEEDS_REINSTALL,
        PROTON_PREFIX_MISSING,
        STEAM_NOT_FOUND,
        MODS_CHOICE_NEEDED,
        MODS_SOURCE_MISSING,
        MODS_LOADER_UNSUPPORTED,
        MODS_MELONLOADER,
        MODS_UNSUPPORTED,
        DESCRIPTOR_INVALID,
        DESCRIPTOR_SCHEMA_UNKNOWN,
        DESCRIPTOR_TOO_LARGE,
        DESCRIPTOR_TOO_BROAD,
        DESCRIPTOR_FIRST_PARTY_ONLY,
        DESCRIPTOR_UNSUPPORTED,
        DESCRIPTOR_DUPLICATE,
        DESCRIPTOR_OVER_LIMIT,
        ENGINE_CODE_RESERVED,
        ENGINE_NAME_RESERVED,
        ENGINE_CODE_TAKEN,
        ENGINE_AMBIGUOUS,
        ANTI_CHEAT_UNKNOWN,
        ENGINE_UNDECLARED,
        NEEDS_NEWER_ASTRA,
        LOADER_POLICY_CONFLICT,
        CAPABILITY_UNDECLARED,
        RESERVED_ID,
        RESERVED_NAME,
        LINK_INVALID,
        MISSING_VARIABLE,
        INVALID_VARIABLE,
        CANCELLED,
        IO,
        USAGE,
        CONSENT_REQUIRED,
    ];
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
            codes::VAC_UNCHECKED => {
                "Steam's Valve Anti-Cheat flag for this game could not be read; accept that explicitly"
            }
            codes::FOREIGN_LOADER => "another mod loader is installed beside the game",
            codes::SHARED_LOADER_CHANGED => {
                "the mod loader shared with another mod manager was replaced by one that does not take this integration's launch options; update that mod manager, or remove its loader and install again"
            }
            codes::SWITCHED_OFF_LOADER => {
                "another mod loader in the game folder is switched off, but its doorstop_config.ini still switches it on; installing would bring its mods back on every start of the game. Set enabled = false in that file, or delete it, then try again"
            }
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
            codes::MODS_CHOICE_NEEDED => {
                "the game can be played together with your own mods; choose with or without them first"
            }
            codes::MODS_SOURCE_MISSING => "the mod profile chosen for this game is no longer there",
            codes::MODS_LOADER_UNSUPPORTED => {
                "your mods' BepInEx is not a version this integration can play together with"
            }
            codes::MODS_MELONLOADER => {
                "the mods chosen for this game run through MelonLoader; Astra plays together with BepInEx only"
            }
            codes::MODS_UNSUPPORTED => "this integration cannot play together with your own mods",
            codes::DESCRIPTOR_INVALID => "an engine descriptor is not valid",
            codes::DESCRIPTOR_SCHEMA_UNKNOWN => "an engine descriptor needs a newer launcher",
            codes::DESCRIPTOR_TOO_LARGE => "an engine descriptor is larger than allowed",
            codes::DESCRIPTOR_TOO_BROAD => {
                "an engine descriptor's rule names nothing specific enough beside the game"
            }
            codes::DESCRIPTOR_FIRST_PARTY_ONLY => {
                "an engine descriptor uses a part only Astra's own descriptors may use"
            }
            codes::DESCRIPTOR_UNSUPPORTED => {
                "an engine descriptor uses a part this launcher cannot evaluate yet"
            }
            codes::DESCRIPTOR_DUPLICATE => "the same engine descriptor was given twice",
            codes::DESCRIPTOR_OVER_LIMIT => "more engine descriptors than are asked",
            codes::ENGINE_CODE_RESERVED => "an engine descriptor declares a reserved engine code",
            codes::ENGINE_NAME_RESERVED => "an engine descriptor's engine name is reserved",
            codes::ENGINE_CODE_TAKEN => "the engine code belongs to another publisher",
            codes::ENGINE_AMBIGUOUS => "more than one engine matches this game",
            codes::ANTI_CHEAT_UNKNOWN => {
                "whether the game uses anti-cheat cannot be told from its folder"
            }
            codes::ENGINE_UNDECLARED => {
                "astra-gi.toml declares an engine but [target] engine does not name it"
            }
            codes::NEEDS_NEWER_ASTRA => "this integration needs a newer Astra",
            codes::LOADER_POLICY_CONFLICT => {
                "astra-gi.toml keeps a file at uninstall that is a mod loader"
            }
            codes::CAPABILITY_UNDECLARED => {
                "astra-gi.toml passes the avatar bridge token on without declaring the capability"
            }
            codes::RESERVED_ID => "the integration's id is reserved for Astra's own integrations",
            codes::RESERVED_NAME => {
                "the integration's name is reserved for Astra's own integrations"
            }
            codes::LINK_INVALID => "astra-gi.toml's [integration.link] is past a limit",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The names and values of the `pub const X: &str = "…";` lines of `codes`, as written.
    fn declared() -> Vec<(String, String)> {
        let src = include_str!("error.rs");
        let body = src
            .split_once("pub mod codes {")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("the codes module");
        body.lines()
            .filter_map(|l| {
                let rest = l.trim().strip_prefix("pub const ")?;
                let (name, rest) = rest.split_once(": &str = \"")?;
                let (value, _) = rest.split_once('"')?;
                Some((name.to_owned(), value.to_owned()))
            })
            .collect()
    }

    /// K15's launcher end: `codes::ALL` is every code, once, in declaration order; every code is
    /// spelled as a code; each has its own sentence.
    #[test]
    fn all_is_every_code_and_each_has_a_sentence() {
        let declared = declared();
        assert!(declared.len() >= 50, "{} codes scraped", declared.len());
        let values: Vec<&str> = declared.iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(codes::ALL, values.as_slice());
        let mut seen = std::collections::BTreeSet::new();
        for (name, code) in &declared {
            assert!(seen.insert(code.clone()), "{name}: {code} twice");
            assert!(
                code.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{name}: {code}"
            );
            let code: &'static str = codes::ALL.iter().find(|c| **c == code).unwrap();
            assert_ne!(LauncherError::new(code).sentence(), "error", "{name}");
        }
    }
}
