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

pub const LEDGER_SCHEMA: u32 = 1;

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
    /// Ours, or (for `keep-existing`) the file that was left alone.
    pub sha256: String,
    pub action: GameAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<LedgerBackup>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proton: Option<ProtonRecord>,
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
    let ledger: Ledger = serde_json::from_str(&text).map_err(|e| {
        LauncherError::io(format!("ledger: {e}")).with("path", p.display().to_string())
    })?;
    if ledger.schema > LEDGER_SCHEMA {
        return Err(LauncherError::new(codes::SCHEMA_TOO_NEW)
            .with("schema", ledger.schema.to_string())
            .with("file", LEDGER_FILE));
    }
    Ok(Some(ledger))
}

pub(crate) fn write(profile: &Path, ledger: &Ledger) -> Result<()> {
    let text = serde_json::to_string_pretty(ledger).map_err(LauncherError::io)?;
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
