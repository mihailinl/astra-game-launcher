// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `<profile>/mods-choice.json`: the player's answer to "play with your mods?" for this game, and
//! the folders they added by hand. Owned by this crate alone (one chokepoint): the daemon writes it
//! through [`set_mods_choice`], and [`crate::prepare_launch`] reads it.
//!
//! `{"schema":1,"choice":"with"|"without","source":{"manager":"gale","profile":"Default","root":"…"},
//! "added_roots":["…"]}`. Asked once: a new mod profile appearing later never asks again. It is not
//! a file of the ledger's and an update keeps it; [`crate::uninstall`] removes it, so a reinstall
//! asks again.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ModManager, ModSource};
use crate::error::{LauncherError, Result, codes};
use crate::ledger::ProfileLock;
use crate::paths::{MODS_CHOICE_FILE, canonical_dir, check_regular_or_absent, write_atomic};

/// The schema `mods-choice.json` is written as, and the newest one read.
pub const MODS_CHOICE_SCHEMA: u32 = 1;
/// More than any choice file.
const MAX_CHOICE_BYTES: u64 = 1024 * 1024;
/// Folders a player may add by hand; discovery looks in no more than these.
pub(crate) const MAX_ADDED_ROOTS: usize = 32;

/// Play with the player's own mods, or Astra on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChoiceKind {
    With,
    Without,
}

/// The mod source a choice names: what [`crate::discover_mods`] reported for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceSource {
    pub manager: ModManager,
    pub profile: String,
    /// The folder that holds their `BepInEx/` (the game folder for [`ModManager::Manual`]).
    pub root: PathBuf,
}

impl ChoiceSource {
    /// The source `s` names.
    pub fn of(s: &ModSource) -> ChoiceSource {
        ChoiceSource {
            manager: s.manager,
            profile: s.profile.clone(),
            root: s.root.clone(),
        }
    }
}

/// The remembered answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub choice: ChoiceKind,
    /// Which mods: required with [`ChoiceKind::With`], kept (the last pick) with `Without`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ChoiceSource>,
    /// Folders the player added by hand ("Указать папку…"): a mod profile, a folder of profiles, or
    /// a mod manager's custom data folder. [`crate::discover_mods`] looks in them too.
    #[serde(default)]
    pub added_roots: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct OnDisk {
    schema: u32,
    #[serde(flatten)]
    choice: Choice,
}

fn bad(path: &Path, detail: &str) -> LauncherError {
    LauncherError::io(format!("{MODS_CHOICE_FILE}: {detail}"))
        .with("path", path.display().to_string())
}

/// A path that can go into `chain.ini` and a JSON file whole: absolute, no line break, no NUL.
fn usable_path(p: &Path) -> bool {
    p.is_absolute() && p.to_str().is_some_and(|s| !s.contains(['\n', '\r', '\0']))
}

impl Choice {
    fn check(&self, path: &Path) -> Result<()> {
        if self.choice == ChoiceKind::With && self.source.is_none() {
            return Err(bad(path, "\"with\" names no source"));
        }
        if let Some(s) = &self.source
            && (!usable_path(&s.root) || s.profile.contains(['\n', '\r', '\0']))
        {
            return Err(bad(path, "the source's root is not an absolute path"));
        }
        if self.added_roots.iter().any(|r| !usable_path(r)) {
            return Err(bad(path, "an added root is not an absolute path"));
        }
        if self.added_roots.len() > MAX_ADDED_ROOTS {
            return Err(bad(path, "more than 32 added roots"));
        }
        Ok(())
    }
}

/// Reads the choice of a profile whose path is canonical, without the lock: it is replaced
/// atomically, so a reader sees one whole file.
pub(crate) fn read(profile: &Path) -> Result<Option<Choice>> {
    let path = profile.join(MODS_CHOICE_FILE);
    check_regular_or_absent(&path)?;
    let mut text = String::new();
    match File::open(&path) {
        Ok(f) => {
            f.take(MAX_CHOICE_BYTES + 1)
                .read_to_string(&mut text)
                .map_err(|e| LauncherError::io_at(&e, &path))?;
        }
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(LauncherError::io_at(&e, &path)),
    }
    if text.len() as u64 > MAX_CHOICE_BYTES {
        return Err(bad(&path, "too large"));
    }
    #[derive(Deserialize)]
    struct Schema {
        schema: u32,
    }
    let Schema { schema } = serde_json::from_str(&text).map_err(|e| bad(&path, &e.to_string()))?;
    if schema > MODS_CHOICE_SCHEMA {
        return Err(LauncherError::new(codes::SCHEMA_TOO_NEW)
            .with("schema", schema.to_string())
            .with("file", MODS_CHOICE_FILE));
    }
    let on_disk: OnDisk = serde_json::from_str(&text).map_err(|e| bad(&path, &e.to_string()))?;
    on_disk.choice.check(&path)?;
    Ok(Some(on_disk.choice))
}

/// The player's remembered answer for the game installed in `profile_dir`; `None` when they have
/// not been asked yet (or the profile does not exist).
pub fn mods_choice(profile_dir: &Path) -> Result<Option<Choice>> {
    match fs::symlink_metadata(profile_dir) {
        Ok(_) => read(&canonical_dir(profile_dir)?),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LauncherError::io_at(&e, profile_dir)),
    }
}

/// Remembers the player's answer: written atomically, under the profile lock. `with` needs a
/// source; every path must be absolute. The profile must exist.
pub fn set_mods_choice(profile_dir: &Path, choice: Choice) -> Result<()> {
    let profile = canonical_dir(profile_dir)?;
    let path = profile.join(MODS_CHOICE_FILE);
    choice.check(&path)?;
    let _lock = ProfileLock::acquire(&profile)?;
    check_regular_or_absent(&path)?;
    let text = serde_json::to_vec(&OnDisk {
        schema: MODS_CHOICE_SCHEMA,
        choice,
    })
    .map_err(LauncherError::io)?;
    write_atomic(&path, &text)
}

/// Removes the choice (uninstall: a reinstall asks again). A link in its place is removed as a
/// link, never followed.
pub(crate) fn remove(profile: &Path) -> Result<()> {
    let path = profile.join(MODS_CHOICE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => fs::remove_file(&path).map_err(|e| LauncherError::io_at(&e, &path)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LauncherError::io_at(&e, &path)),
    }
}
