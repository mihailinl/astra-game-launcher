// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The game being modded: its folder, its program and how it runs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::paths::{canonical_dir, relative_components};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Platform {
    Windows,
    /// A Windows game on Linux under Steam's Proton. `prefix` is `steamapps/compatdata/<appid>/pfx`.
    LinuxProton {
        prefix: Option<PathBuf>,
    },
    LinuxNative,
}

impl Platform {
    /// The manifest's name for this platform (`[target] platforms`).
    pub fn code(&self) -> &'static str {
        match self {
            Platform::Windows => "windows-x64",
            Platform::LinuxProton { .. } => "linux-proton",
            Platform::LinuxNative => "linux-x64",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameTarget {
    pub dir: PathBuf,
    /// Relative to `dir`.
    pub exe: PathBuf,
    pub steam_appid: Option<u32>,
    pub platform: Platform,
}

impl GameTarget {
    /// The exe's checked components below `dir`.
    pub(crate) fn exe_components(&self) -> Result<Vec<String>> {
        let s = self.exe.to_str().ok_or_else(|| {
            LauncherError::new(codes::PATH_ESCAPES).with("path", self.exe.display().to_string())
        })?;
        relative_components(s)
            .ok_or_else(|| LauncherError::new(codes::PATH_ESCAPES).with("path", s.to_owned()))
    }

    /// The exe's components joined with `/`.
    pub(crate) fn exe_rel(&self) -> Result<String> {
        Ok(self.exe_components()?.join("/"))
    }

    pub(crate) fn exe_file_name(&self) -> Result<String> {
        Ok(self.exe_components()?.pop().unwrap_or_default())
    }

    /// `${game}`: the folder holding the exe, canonical. The exe must exist.
    pub(crate) fn game_root(&self) -> Result<PathBuf> {
        let comps = self.exe_components()?;
        let dir = canonical_dir(&self.dir)?;
        let mut exe = dir.clone();
        exe.extend(&comps);
        match std::fs::symlink_metadata(&exe) {
            Ok(m) if m.is_file() => {}
            _ => {
                return Err(LauncherError::new(codes::EXE_NOT_FOUND).with("exe", comps.join("/")));
            }
        }
        let parent = exe.parent().map(Path::to_path_buf).unwrap_or(dir.clone());
        let parent = canonical_dir(&parent)?;
        if !parent.starts_with(&dir) {
            return Err(LauncherError::new(codes::PATH_ESCAPES).with("path", comps.join("/")));
        }
        Ok(parent)
    }
}

/// The Proton prefix Steam keeps for `appid`, for a game installed at
/// `<library>/steamapps/common/<game>`: `<library>/steamapps/compatdata/<appid>/pfx`.
/// Returns the path whether it exists or not; `None` when the folder is not in a Steam library.
pub fn proton_prefix_for(game_dir: &Path, appid: u32) -> Option<PathBuf> {
    let common = game_dir.parent()?;
    let steamapps = common.parent()?;
    let is = |p: &Path, name: &str| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name));
    if !is(common, "common") || !is(steamapps, "steamapps") {
        return None;
    }
    Some(
        steamapps
            .join("compatdata")
            .join(appid.to_string())
            .join("pfx"),
    )
}
