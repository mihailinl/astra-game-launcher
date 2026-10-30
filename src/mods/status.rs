// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What the runners say about the last start, for the daemon to put in words:
//!
//! - `chain/last-run.txt`, written by the Unity chain loader at every start with mods:
//!   `ok <time> <their BepInEx version>`, `fallback <time> <reason>` (Astra ran on its own) or
//!   `hooks-failed <time> <error>` (their mods ran, Astra was missing from that session);
//! - `<profile>/astra-loader-status.json`, written by the Unreal loader at every armed start:
//!   `{"schema":1,"reason":"loaded|foreign-reshade|stale-astra|load-failed","armed_by":"argv|play-arm",
//!   "file":"","version":"","error":"","reshade":bool,"ue4ss":bool,"their_ue4ss":bool,"unix_ms":N,
//!   "pid":N,"exe":"…"}`. A `reason` or `armed_by` this launcher does not know is kept as
//!   `Other`, never refused: the loader may learn words first.
//!
//! Both are someone else's files: read bounded, never through a link, never written.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{LauncherError, Result};
use crate::ledger;
use crate::manifest::Manifest;
use crate::mods::play::LAST_RUN_FILE;
use crate::paths::{canonical_dir, check_regular_or_absent, checked_rel, existing_target};

/// The Unreal loader's status file's name inside a profile.
pub const LOADER_STATUS_FILE_NAME: &str = "astra-loader-status.json";
/// More than either file.
const MAX_STATUS_BYTES: u64 = 64 * 1024;

/// The chain loader's report of the last start with mods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum LastRun {
    /// Their BepInEx ran, and Astra beside it. `version`: their BepInEx's.
    Ok { time: String, version: String },
    /// Astra ran on its own: their mods did not load (`reason`, in the chain's words).
    Fallback { time: String, reason: String },
    /// Their mods ran; folding Astra in failed, so Astra was missing from that session.
    HooksFailed { time: String, error: String },
    /// A status this launcher does not know yet.
    Other {
        status: String,
        time: String,
        detail: String,
    },
}

impl LastRun {
    /// The report in `text`: its first non-empty line, `<status> <time> <detail>`.
    pub fn parse(text: &str) -> Option<LastRun> {
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        let (status, rest) = line.split_once(' ')?;
        let (time, detail) = rest.split_once(' ').unwrap_or((rest, ""));
        let (time, detail) = (time.to_owned(), detail.trim().to_owned());
        Some(match status {
            "ok" => LastRun::Ok {
                time,
                version: detail,
            },
            "fallback" => LastRun::Fallback {
                time,
                reason: detail,
            },
            "hooks-failed" => LastRun::HooksFailed {
                time,
                error: detail,
            },
            other => LastRun::Other {
                status: other.to_owned(),
                time,
                detail,
            },
        })
    }
}

/// What the Unreal loader decided at an armed start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoaderReason {
    /// Astra's ReShade and UE4SS loaded.
    Loaded,
    /// The player's own ReShade is beside the exe: nothing of Astra's loaded.
    ForeignReshade,
    /// Astra's own 0.2.x ReShade is beside the exe (`astra.addon64` next to it): nothing loaded
    /// until it is cleaned away.
    StaleAstra,
    /// Something failed to load (`LoaderStatus::error`).
    LoadFailed,
    /// A reason this launcher does not know yet.
    #[serde(untagged)]
    Other(String),
}

/// What armed the Unreal loader's start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArmedBy {
    /// Astra's arguments on the command line.
    Argv,
    /// A fresh `play-arm.json`.
    PlayArm,
    #[serde(untagged)]
    Other(String),
}

impl<'de> Deserialize<'de> for LoaderReason {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "loaded" => LoaderReason::Loaded,
            "foreign-reshade" => LoaderReason::ForeignReshade,
            "stale-astra" => LoaderReason::StaleAstra,
            "load-failed" => LoaderReason::LoadFailed,
            other => LoaderReason::Other(other.to_owned()),
        })
    }
}

impl<'de> Deserialize<'de> for ArmedBy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "argv" => ArmedBy::Argv,
            "play-arm" => ArmedBy::PlayArm,
            other => ArmedBy::Other(other.to_owned()),
        })
    }
}

impl Default for LoaderReason {
    fn default() -> Self {
        LoaderReason::Other(String::new())
    }
}

impl Default for ArmedBy {
    fn default() -> Self {
        ArmedBy::Other(String::new())
    }
}

/// `astra-loader-status.json`. Every key the loader writes; one it leaves out reads as empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoaderStatus {
    pub schema: u32,
    pub reason: LoaderReason,
    pub armed_by: ArmedBy,
    /// `foreign-reshade` / `stale-astra`: the ReShade file beside the exe (its name only).
    pub file: String,
    /// Its `ProductVersion` (else `FileVersion`).
    pub version: String,
    /// `load-failed`: what failed, in words.
    pub error: String,
    /// Astra's ReShade is loaded.
    pub reshade: bool,
    /// UE4SS is loaded.
    pub ue4ss: bool,
    /// A UE4SS of the player's own is beside the exe (Astra's was loaded, not theirs).
    pub their_ue4ss: bool,
    pub unix_ms: u64,
    pub pid: u32,
    /// The game's program (its name only).
    pub exe: String,
}

impl LoaderStatus {
    /// The status in `text`; `None` when it is not one (not a JSON object).
    pub fn parse(text: &str) -> Option<LoaderStatus> {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }
}

/// The text of `path`, bounded; `None` when it is not there. A link or a folder in its place is
/// not a status anyone wrote for us.
fn read_small(path: &Path) -> Result<Option<String>> {
    check_regular_or_absent(path)?;
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(LauncherError::io_at(&e, path)),
    };
    let mut bytes = Vec::new();
    f.take(MAX_STATUS_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| LauncherError::io_at(&e, path))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The Unreal loader's status for the game installed in `profile_dir`; `None` when there is none
/// (no armed start yet). A file that is not a status object is an `io` error.
pub fn loader_status(profile_dir: &Path) -> Result<Option<LoaderStatus>> {
    if fs::symlink_metadata(profile_dir).is_err() {
        return Ok(None);
    }
    let path = canonical_dir(profile_dir)?.join(LOADER_STATUS_FILE_NAME);
    match read_small(&path)? {
        None => Ok(None),
        Some(text) => LoaderStatus::parse(&text).map(Some).ok_or_else(|| {
            LauncherError::io(format!("{LOADER_STATUS_FILE_NAME}: not a status"))
                .with("path", path.display().to_string())
        }),
    }
}

/// The chain loader's report of the last start with mods, for the game installed in
/// `profile_dir`: `last-run.txt` beside its `[integration.mods].chain`. `None` when nothing is
/// installed, the integration has no chain, or no start with mods wrote one yet.
pub fn mods_last_run(profile_dir: &Path) -> Result<Option<LastRun>> {
    if fs::symlink_metadata(profile_dir).is_err() {
        return Ok(None);
    }
    let profile = canonical_dir(profile_dir)?;
    let Some(ledger) = ledger::read(&profile)? else {
        return Ok(None);
    };
    let Ok(m) = Manifest::parse(&ledger.manifest) else {
        return Ok(None);
    };
    let Some(Ok(decl)) = m.mods() else {
        return Ok(None);
    };
    let mut rel = decl.chain_rel();
    rel.pop();
    rel.push(LAST_RUN_FILE.to_owned());
    let Some(path) = existing_target(&profile, &checked_rel(&rel.join("/"))?)? else {
        return Ok(None);
    };
    Ok(read_small(&path)?.and_then(|t| LastRun::parse(&t)))
}
