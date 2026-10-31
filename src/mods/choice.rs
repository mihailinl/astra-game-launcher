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
//!
//! `"choice": null` (0.4.1, [`add_mods_root`]): a folder the person added before they answered.
//! It is no answer ([`mods_choice`] says `None`, Play still asks), and discovery looks in the
//! folder. A launcher 0.4.0 cannot read such a file, which its own rule makes harmless: an answer
//! that cannot be read is no answer (it asks, or plays Astra alone), and it looks in no added
//! folder. Every folder is checked before anything touches it (`paths::require_local`: a
//! drive-letter path on Windows — a mapped network drive or `subst` behind the letter is the
//! user's own setting and is followed — never a share, a device or a verbatim namespace), then
//! remembered without a `\\?\` prefix. At most 32 are remembered: the oldest goes first.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ModManager, ModSource};
use crate::error::{LauncherError, Result, codes};
use crate::ledger::ProfileLock;
use crate::paths::{
    MODS_CHOICE_FILE, canonical_dir, check_regular_or_absent, local_root, plain, require_local,
    write_atomic,
};

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

/// The file. `choice: None` (`null`): folders were added, nothing was answered yet.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct OnDisk {
    schema: u32,
    choice: Option<ChoiceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<ChoiceSource>,
    #[serde(default)]
    pub(crate) added_roots: Vec<PathBuf>,
}

impl OnDisk {
    /// The answer, when there is one.
    pub(crate) fn answer(self) -> Option<Choice> {
        self.choice.map(|choice| Choice {
            choice,
            source: self.source,
            added_roots: self.added_roots,
        })
    }
}

/// Whether two remembered folders are one (letter case aside on Windows).
fn same_root(a: &Path, b: &Path) -> bool {
    let (a, b) = (plain(a), plain(b));
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

/// `roots` then `more`, each folder once (a folder given again moves to the end, the newest),
/// and no more than [`MAX_ADDED_ROOTS`]: the oldest go first, so adding one never fails.
fn merged(mut roots: Vec<PathBuf>, more: Vec<PathBuf>) -> Vec<PathBuf> {
    for r in more {
        roots.retain(|x| !same_root(x, &r));
        roots.push(r);
    }
    let over = roots.len().saturating_sub(MAX_ADDED_ROOTS);
    roots.drain(..over);
    roots
}

fn bad(path: &Path, detail: &str) -> LauncherError {
    LauncherError::io(format!("{MODS_CHOICE_FILE}: {detail}"))
        .with("path", path.display().to_string())
}

/// A folder a person or the file names, checked before anything touches it: an absolute
/// drive-letter path on Windows, from the root on Unix (`path-escapes` with its `reason`
/// otherwise: a share, a device, a verbatim namespace), and one that `chain.ini` and JSON carry
/// whole (no line break, no NUL).
fn check_root(path: &Path, root: &Path) -> Result<()> {
    require_local(root)?;
    if !root
        .to_str()
        .is_some_and(|s| !s.contains(['\n', '\r', '\0']))
    {
        return Err(bad(path, "a folder's name holds a line break"));
    }
    Ok(())
}

fn check_roots(path: &Path, roots: &[PathBuf]) -> Result<()> {
    for r in roots {
        check_root(path, r)?;
    }
    Ok(())
}

impl Choice {
    fn check(&self, path: &Path) -> Result<()> {
        if self.choice == ChoiceKind::With && self.source.is_none() {
            return Err(bad(path, "\"with\" names no source"));
        }
        if let Some(s) = &self.source {
            check_root(path, &s.root)?;
            if s.profile.contains(['\n', '\r', '\0']) {
                return Err(bad(path, "the source's profile name holds a line break"));
            }
        }
        check_roots(path, &self.added_roots)
    }

    /// Every folder checked as given ([`local_root`]: `\\?\UNC\…` is `verbatim-unc`), then
    /// without its `\\?\` prefix, as a person and `chain.ini` see it.
    fn plain(mut self, path: &Path) -> Result<Choice> {
        if let Some(s) = &mut self.source {
            s.root = local_root(&s.root)?;
        }
        self.added_roots = self
            .added_roots
            .iter()
            .map(|r| local_root(r))
            .collect::<Result<_>>()?;
        self.check(path)?;
        Ok(self)
    }
}

/// Reads the choice of a profile whose path is canonical, without the lock: it is replaced
/// atomically, so a reader sees one whole file.
pub(crate) fn read(profile: &Path) -> Result<Option<Choice>> {
    Ok(read_file(profile)?.and_then(OnDisk::answer))
}

/// The whole file (answer or not), checked; `None` when there is none.
pub(crate) fn read_file(profile: &Path) -> Result<Option<OnDisk>> {
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
    match &on_disk.choice {
        Some(choice) => Choice {
            choice: *choice,
            source: on_disk.source.clone(),
            added_roots: on_disk.added_roots.clone(),
        }
        .check(&path)?,
        None => check_roots(&path, &on_disk.added_roots)?,
    }
    Ok(Some(on_disk))
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
/// source. Every folder must be an absolute drive-letter path on Windows (`path-escapes` with a
/// `reason` for a share, a device or a verbatim namespace, checked as given before anything
/// touches it) and is remembered without a `\\?\` prefix. The folders already remembered are
/// kept: `choice.added_roots` adds to them (each once; past 32 the oldest go), so an answer never
/// forgets a folder the person added. The profile must exist.
pub fn set_mods_choice(profile_dir: &Path, choice: Choice) -> Result<()> {
    let profile = canonical_dir(profile_dir)?;
    let path = profile.join(MODS_CHOICE_FILE);
    let choice = choice.plain(&path)?;
    let _lock = ProfileLock::acquire(&profile)?;
    check_regular_or_absent(&path)?;
    // What is remembered now (a file that cannot be read remembers nothing).
    let kept = read_file(&profile)
        .ok()
        .flatten()
        .map(|f| f.added_roots)
        .unwrap_or_default();
    let added_roots = merged(kept, choice.added_roots);
    write(
        &path,
        &OnDisk {
            schema: MODS_CHOICE_SCHEMA,
            choice: Some(choice.choice),
            source: choice.source,
            added_roots,
        },
    )
}

/// Remembers a folder the person picked ("Указать папку…") for discovery to look in, whether or
/// not they have answered yet: under the profile lock, any answer and source kept, the folder
/// added once (past 32 the oldest goes, so this never gets stuck). With no answer yet the file
/// says `"choice": null` and [`mods_choice`] still says `None`: Play keeps asking
/// (`mods-choice-needed`). The folder must be an absolute drive-letter path on Windows
/// (`path-escapes` with a `reason` otherwise, checked as given before anything touches it); it is
/// remembered without a `\\?\` prefix. The profile must exist.
pub fn add_mods_root(profile_dir: &Path, root: &Path) -> Result<()> {
    let profile = canonical_dir(profile_dir)?;
    let path = profile.join(MODS_CHOICE_FILE);
    // Checked as given, then without `\\?\`: `\\?\UNC\…` is `verbatim-unc`, never `unc`.
    let root = local_root(root)?;
    check_root(&path, &root)?;
    let _lock = ProfileLock::acquire(&profile)?;
    check_regular_or_absent(&path)?;
    // A file that cannot be read holds no answer and no folder: started anew.
    let mut file = read_file(&profile).ok().flatten().unwrap_or_default();
    file.schema = MODS_CHOICE_SCHEMA;
    file.added_roots = merged(file.added_roots, vec![root]);
    if file.choice.is_none() {
        file.source = None;
    }
    write(&path, &file)
}

fn write(path: &Path, file: &OnDisk) -> Result<()> {
    let text = serde_json::to_vec(file).map_err(LauncherError::io)?;
    write_atomic(path, &text)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_keeps_each_folder_once_the_newest_last_and_no_more_than_32() {
        let r = |n: usize| PathBuf::from(format!("/r{n}"));
        assert_eq!(merged(vec![r(1), r(2)], vec![r(1)]), [r(2), r(1)]);
        let all: Vec<PathBuf> = (0..40).map(r).collect();
        let kept = merged(Vec::new(), all.clone());
        assert_eq!(kept, all[8..].to_vec(), "the oldest went first");
        assert_eq!(merged(kept, vec![r(99)]).first(), Some(&r(9)));
    }
}
