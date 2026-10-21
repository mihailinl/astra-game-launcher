// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `<profile>/astra-launcher-ledger.json`: what was placed, where, with which digest, and what it
//! replaced. Uninstall acts on the ledger and nothing else. Every change happens under a file lock.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::manifest::IntegrationInfo;
use crate::paths::{LEDGER_FILE, LOCK_FILE, check_regular_or_absent, write_atomic};
use crate::plan::GameAction;

/// The newest ledger schema this launcher reads. A ledger is WRITTEN as the lowest schema that
/// carries it: 1, or 2 for a `proton` array (several overrides), or 3 for a game file shared with
/// another mod manager (`share-loader`) — shapes an older launcher cannot parse. [`read`] judges
/// the schema before the shape, so from this launcher on a newer ledger is `schema-too-new`
/// rather than a parse failure; a launcher from before schema 3 parsed the shape first and fails
/// on a schema-3 ledger with an `io` error naming the unknown action.
pub const LEDGER_SCHEMA: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LedgerState {
    /// Written before the first file is placed; a crash leaves this, and uninstall rolls it back.
    Installing,
    Installed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerSource {
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerGame {
    pub dir: PathBuf,
    pub exe: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerFile {
    /// Below the profile, `/`-separated.
    pub rel: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerBackup {
    /// Below the profile, `/`-separated.
    pub rel: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerGameFile {
    /// Below the exe's folder, `/`-separated.
    pub rel: String,
    /// Ours, or (for `keep-existing` and `share-loader`) the file that was left alone, as it was
    /// at install.
    pub sha256: String,
    pub action: GameAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<LedgerBackup>,
}

/// What sat beside a Doorstop proxy before Astra first added it to this game: the yardstick for
/// "someone else's loader runs through ours now" ([`crate::plan`]'s `kept_for_their_loader`).
/// Recorded by the install that first adds the proxy and carried, unchanged, by every update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderBaseline {
    /// The proxy, below the exe's folder, `/`-separated.
    pub proxy: String,
    /// The digest of the `doorstop_config.ini` beside it then; `None`: there was none.
    #[serde(default)]
    pub ini_sha256: Option<String>,
    /// The names in the `BepInEx/core` beside it then (a BepInEx the person had switched off).
    #[serde(default)]
    pub bepinex_core: Vec<String>,
}

/// The Wine DLL override written into a Proton prefix's `user.reg`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtonRecord {
    pub prefix: PathBuf,
    /// The registry key, unescaped: `Software\Wine\AppDefaults\<exe>\DllOverrides`.
    pub section: String,
    pub dll: String,
    pub value: String,
    /// The value text that was there before (after `=`), or none.
    pub previous: Option<String>,
    /// The section did not exist; uninstall removes it when it is empty again.
    pub section_created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub schema: u32,
    pub state: LedgerState,
    pub integration: IntegrationInfo,
    pub source: LedgerSource,
    pub game: LedgerGame,
    /// The manifest text, read again at every launch.
    pub manifest: String,
    pub files: Vec<LedgerFile>,
    pub game_files: Vec<LedgerGameFile>,
    /// One record per DLL override, in the order they were applied. On disk one record is an
    /// object, exactly as a launcher that knew one override wrote it, and several are an array.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "one_or_many")]
    pub proton: Vec<ProtonRecord>,
    /// The person installed although Steam's VAC flag could not be read (`Accept::vac_unchecked`
    /// on an unchecked plan). Without it, a Play whose VAC check is still unread is refused.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vac_unchecked_accepted: bool,
    /// One per Doorstop proxy this integration added. Absent from a ledger written before 0.3.1,
    /// which a launcher before it ignores (no schema change): without one, everything beside the
    /// proxy that is not ours counts as someone else's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loader_baseline: Vec<LoaderBaseline>,
}

/// `proton` is an object when there is one record (every ledger written before several overrides
/// existed) and an array when there are more.
mod one_or_many {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::ProtonRecord;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(ProtonRecord),
        Many(Vec<ProtonRecord>),
    }

    pub fn serialize<S: Serializer>(v: &[ProtonRecord], s: S) -> Result<S::Ok, S::Error> {
        match v {
            [one] => one.serialize(s),
            many => many.serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ProtonRecord>, D::Error> {
        Ok(match Option::<OneOrMany>::deserialize(d)? {
            None => Vec::new(),
            Some(OneOrMany::One(r)) => vec![r],
            Some(OneOrMany::Many(v)) => v,
        })
    }
}

pub(crate) fn ledger_path(profile: &Path) -> PathBuf {
    profile.join(LEDGER_FILE)
}

/// Reads the ledger of a profile, if there is one.
pub(crate) fn read(profile: &Path) -> Result<Option<Ledger>> {
    let p = ledger_path(profile);
    check_regular_or_absent(&p)?;
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(LauncherError::io_at(&e, &p)),
    };
    let malformed = |e: serde_json::Error| {
        LauncherError::io(format!("ledger: {e}")).with("path", p.display().to_string())
    };
    // The schema first, on its own: a newer ledger's shape is exactly what this launcher cannot
    // parse, so judging it after the whole parse would report the shape instead of the schema.
    #[derive(Deserialize)]
    struct Schema {
        schema: u32,
    }
    let Schema { schema } = serde_json::from_str(&text).map_err(malformed)?;
    if schema > LEDGER_SCHEMA {
        return Err(LauncherError::new(codes::SCHEMA_TOO_NEW)
            .with("schema", schema.to_string())
            .with("file", LEDGER_FILE));
    }
    let ledger: Ledger = serde_json::from_str(&text).map_err(malformed)?;
    Ok(Some(ledger))
}

/// The schema a ledger is written as: the lowest that can carry it.
fn schema_for(ledger: &Ledger) -> u32 {
    if ledger
        .game_files
        .iter()
        .any(|g| g.action == GameAction::ShareLoader)
    {
        3
    } else if ledger.proton.len() > 1 {
        2
    } else {
        1
    }
}

pub(crate) fn write(profile: &Path, ledger: &Ledger) -> Result<()> {
    let mut on_disk = ledger.clone();
    on_disk.schema = schema_for(ledger);
    let text = serde_json::to_string_pretty(&on_disk).map_err(LauncherError::io)?;
    write_atomic(&ledger_path(profile), text.as_bytes())
}

pub(crate) fn remove(profile: &Path) -> Result<()> {
    let p = ledger_path(profile);
    match fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LauncherError::io_at(&e, &p)),
    }
}

/// An exclusive lock on a profile, held for a whole install, uninstall or launch preparation.
pub(crate) struct ProfileLock {
    file: File,
}

impl ProfileLock {
    /// Waits up to five seconds for another launcher to finish with the profile.
    pub fn acquire(profile: &Path) -> Result<ProfileLock> {
        let p = profile.join(LOCK_FILE);
        check_regular_or_absent(&p)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&p)
            .map_err(|e| LauncherError::io_at(&e, &p))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(ProfileLock { file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(
                        LauncherError::io("the profile is in use by another launcher")
                            .with("path", p.display().to_string()),
                    );
                }
                Err(TryLockError::Error(e)) => return Err(LauncherError::io_at(&e, &p)),
            }
        }
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
