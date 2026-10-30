// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `[integration.mods]`: an integration's declaration that it can play together with the
//! player's own BepInEx (astra-bepinex `docs/GAME-INTEGRATION-MANIFEST.md` §7).
//!
//! It lives inside `[integration]`, which every launcher reads with no key check, ON PURPOSE: a
//! launcher from before this table installs the same release and plays Astra on its own. So this
//! launcher never refuses a MANIFEST for this table either — a table it cannot use (an unknown
//! key, an unknown loader, a path outside the profile) only means "not with mods", and is
//! refused, with its own code, by a Play that was asked to be one.

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::paths::{Root, parse_jailed};

/// Which BepInEx the integration's chain loader runs beside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ModsLoader {
    /// `loader = "bepinex5"`: the player's BepInEx 5.4.x (a Mono game).
    Bepinex5,
    /// `loader = "bepinex6-il2cpp"` with `bepinex_build`: the player's BepInEx 6 bleeding-edge
    /// build `build` exactly (an IL2CPP game).
    Bepinex6Il2cpp { build: u32 },
}

impl ModsLoader {
    /// What the player's BepInEx must be, as a person reads it: `5.4`, `6.0.0-be.788`.
    pub fn need(self) -> String {
        match self {
            ModsLoader::Bepinex5 => "5.4".to_owned(),
            ModsLoader::Bepinex6Il2cpp { build } => format!("6.0.0-be.{build}"),
        }
    }
}

/// A checked `[integration.mods]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModsDecl {
    pub loader: ModsLoader,
    /// The assembly Doorstop starts instead of the integration's own preloader:
    /// `${profile}/chain/Astra.Chain.dll`. A file of the install.
    pub chain: String,
    /// Where Play writes `chain.ini`: `${profile}/chain/chain.ini`. Never a file of the install.
    pub chain_config: String,
}

const KEYS: &[&str] = &["loader", "bepinex_build", "chain", "chain_config"];
/// The one file the chain loader reads, beside itself.
const CHAIN_INI: &str = "chain.ini";

fn invalid(key: &str, detail: &str) -> LauncherError {
    let key = if key.is_empty() {
        "integration.mods".to_owned()
    } else {
        format!("integration.mods.{key}")
    };
    LauncherError::new(codes::MANIFEST_INVALID)
        .with("key", key)
        .with("detail", detail)
}

/// A file in the profile: `${profile}/a/b`, not a folder, not the profile's own bookkeeping.
fn profile_file(value: Option<&toml::Value>, key: &str) -> Result<String> {
    let Some(toml::Value::String(s)) = value else {
        return Err(invalid(key, "missing, or not a string"));
    };
    let p = parse_jailed(s)?;
    if p.root != Root::Profile {
        return Err(LauncherError::new(codes::PATH_ESCAPES)
            .with("path", s.clone())
            .with("reason", "mods-files-are-in-the-profile"));
    }
    if p.is_dir || p.rel.is_empty() {
        return Err(invalid(key, "must name a file"));
    }
    Ok(s.clone())
}

impl ModsDecl {
    /// Reads `[integration.mods]` as the manifest holds it. The error is what a Play with mods
    /// refuses with; it never refuses the manifest.
    pub(crate) fn from_toml(value: &toml::Value) -> Result<ModsDecl> {
        let toml::Value::Table(t) = value else {
            return Err(invalid("", "must be a table"));
        };
        // A key this launcher does not know may change how the chain must be started: a newer
        // feature read as nothing. Not with mods, then.
        if let Some(k) = t.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(
                LauncherError::new(codes::UNKNOWN_KEY).with("key", format!("integration.mods.{k}"))
            );
        }
        let build = match t.get("bepinex_build") {
            None => None,
            Some(toml::Value::Integer(n)) if (1..=i64::from(u32::MAX)).contains(n) => {
                Some(*n as u32)
            }
            Some(_) => return Err(invalid("bepinex_build", "a build number")),
        };
        let loader = match (t.get("loader").and_then(toml::Value::as_str), build) {
            (Some("bepinex5"), None) => ModsLoader::Bepinex5,
            (Some("bepinex5"), Some(_)) => {
                return Err(invalid(
                    "bepinex_build",
                    "only for loader = \"bepinex6-il2cpp\"",
                ));
            }
            (Some("bepinex6-il2cpp"), Some(build)) => ModsLoader::Bepinex6Il2cpp { build },
            (Some("bepinex6-il2cpp"), None) => {
                return Err(invalid(
                    "bepinex_build",
                    "missing: the build the chain is made for",
                ));
            }
            _ => return Err(invalid("loader", "\"bepinex5\" or \"bepinex6-il2cpp\"")),
        };
        let chain = profile_file(t.get("chain"), "chain")?;
        let chain_config = profile_file(t.get("chain_config"), "chain_config")?;
        // The chain loader reads `chain.ini` beside itself and nothing else (astra-bepinex
        // `ChainCore`: `Path.Combine(dir(self), "chain.ini")`): a file anywhere else would be
        // written at every Play and never read. Letter case: Windows and Wine find it either way.
        let (c, cc) = (parse_jailed(&chain)?.rel, parse_jailed(&chain_config)?.rel);
        let beside = c.len() == cc.len()
            && c[..c.len() - 1]
                .iter()
                .zip(&cc[..cc.len() - 1])
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
            && cc.last().is_some_and(|n| n.eq_ignore_ascii_case(CHAIN_INI));
        if !beside || chain.eq_ignore_ascii_case(&chain_config) {
            return Err(invalid(
                "chain_config",
                "must be chain.ini in the chain's own folder",
            ));
        }
        Ok(ModsDecl {
            loader,
            chain,
            chain_config,
        })
    }

    /// `chain` below the profile.
    pub(crate) fn chain_rel(&self) -> Vec<String> {
        parse_jailed(&self.chain).expect("checked when read").rel
    }

    /// `chain_config` below the profile.
    pub(crate) fn chain_config_rel(&self) -> Vec<String> {
        parse_jailed(&self.chain_config)
            .expect("checked when read")
            .rel
    }
}
