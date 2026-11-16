// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Starting a Windows game under a Wine or Proton the caller chose ([`Platform::LinuxWine`]):
//! Heroic's games, which Steam does not know.
//!
//! The runner and its prefix belong to the launcher that made them, so nothing here writes
//! into either: the integration's Wine DLL overrides (`[launch] proton_dll_overrides`, which
//! under Steam's Proton are written into the prefix's `user.reg`) travel in `WINEDLLOVERRIDES`
//! on the spawned process instead, merged with any value the runner's own environment carries.
//! The ledger's Proton records stay Steam-only.

use std::path::{Path, PathBuf};

use crate::error::{LauncherError, Result, codes};
use crate::game::{RunnerKind, WineRunner};

const OVERRIDES_VAR: &str = "WINEDLLOVERRIDES";

fn invalid(reason: &str, path: &Path) -> LauncherError {
    LauncherError::new(codes::RUNNER_INVALID)
        .with("reason", reason)
        .with("path", path.display().to_string())
}

/// `^[A-Z_][A-Z0-9_]*$`.
fn is_env_key(k: &str) -> bool {
    let mut c = k.chars();
    c.next().is_some_and(|f| f.is_ascii_uppercase() || f == '_')
        && c.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl WineRunner {
    /// Everything about the runner that can be known without starting it, checked before anything
    /// is spawned or written (`runner-invalid`, `reason` and `path` or `key`):
    /// - `bin` is absolute and resolves (a link is followed: distributions install `wine` as a
    ///   link) to a regular file with an execute bit;
    /// - `prefix` is absolute and an existing folder (a link to one is fine), and not inside
    ///   `profile` (the launcher's own folder, which it may rewrite);
    /// - every `env` key is `[A-Z_][A-Z0-9_]*` and no value holds a NUL;
    /// - [`RunnerKind::Umu`] carries a non-empty `GAMEID`.
    pub fn check(&self, profile: &Path) -> Result<()> {
        if !self.bin.is_absolute() {
            return Err(invalid("bin-not-absolute", &self.bin));
        }
        let meta = std::fs::metadata(&self.bin).map_err(|_| invalid("bin-not-file", &self.bin))?;
        if !meta.is_file() {
            return Err(invalid("bin-not-file", &self.bin));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                return Err(invalid("bin-not-executable", &self.bin));
            }
        }
        if !self.prefix.is_absolute() {
            return Err(invalid("prefix-not-absolute", &self.prefix));
        }
        if !std::fs::metadata(&self.prefix).is_ok_and(|m| m.is_dir()) {
            return Err(invalid("prefix-missing", &self.prefix));
        }
        let inside = match (
            std::fs::canonicalize(&self.prefix),
            std::fs::canonicalize(profile),
        ) {
            (Ok(p), Ok(prof)) => p.starts_with(prof),
            _ => self.prefix.starts_with(profile),
        };
        if inside {
            return Err(invalid("prefix-in-profile", &self.prefix));
        }
        for (k, v) in &self.env {
            if !is_env_key(k) {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "env-key")
                    .with("key", k.clone()));
            }
            if v.contains('\0') {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "env-value")
                    .with("key", k.clone()));
            }
        }
        if self.kind == RunnerKind::Umu
            && !self.env.iter().any(|(k, v)| k == "GAMEID" && !v.is_empty())
        {
            return Err(invalid("umu-gameid", &self.bin));
        }
        Ok(())
    }

    /// The program, its arguments and its environment for starting `exe` with `args`.
    /// `base_env` is the integration's own (`[launch] env`, expanded); the runner's `env` is
    /// applied over it, as given, except `WINEDLLOVERRIDES`, which is merged
    /// ([`merge_dll_overrides`]) with `overrides` (`(dll, value)` pairs, the integration's). Adds
    /// `WINEPREFIX` unless set. For Proton also `STEAM_COMPAT_DATA_PATH` (the folder holding
    /// `pfx`: the prefix's parent when it is named `pfx`, else the prefix itself, which Proton
    /// then fills with its own `pfx`) and an empty `STEAM_COMPAT_CLIENT_INSTALL_PATH` when unset
    /// (Proton's script reads the variable; Heroic sets it to the Steam folder when it has one).
    pub(crate) fn command(
        &self,
        exe: &Path,
        args: Vec<String>,
        base_env: Vec<(String, String)>,
        overrides: &[(String, String)],
    ) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
        let mut env: Vec<(String, String)> = Vec::new();
        let mut preset: Vec<String> = Vec::new();
        for (k, v) in base_env.into_iter().chain(self.env.iter().cloned()) {
            if k == OVERRIDES_VAR {
                preset.push(v);
            } else {
                set(&mut env, k, v);
            }
        }
        if !env.iter().any(|(k, _)| k == "WINEPREFIX") {
            set(
                &mut env,
                "WINEPREFIX".into(),
                self.prefix.display().to_string(),
            );
        }
        if self.kind == RunnerKind::Proton {
            if !env.iter().any(|(k, _)| k == "STEAM_COMPAT_DATA_PATH") {
                let data = if self.prefix.file_name().is_some_and(|n| n == "pfx") {
                    self.prefix.parent().unwrap_or(&self.prefix)
                } else {
                    &self.prefix
                };
                set(
                    &mut env,
                    "STEAM_COMPAT_DATA_PATH".into(),
                    data.display().to_string(),
                );
            }
            if !env
                .iter()
                .any(|(k, _)| k == "STEAM_COMPAT_CLIENT_INSTALL_PATH")
            {
                set(
                    &mut env,
                    "STEAM_COMPAT_CLIENT_INSTALL_PATH".into(),
                    String::new(),
                );
            }
        }
        let merged = merge_dll_overrides(&preset, overrides);
        if !merged.is_empty() {
            set(&mut env, OVERRIDES_VAR.into(), merged);
        }
        let mut argv = Vec::new();
        if self.kind == RunnerKind::Proton {
            argv.push("run".to_owned());
        }
        argv.push(exe.display().to_string());
        argv.extend(args);
        (self.bin.clone(), argv, env)
    }
}

fn set(env: &mut Vec<(String, String)>, k: String, v: String) {
    match env.iter_mut().find(|(ek, _)| *ek == k) {
        Some(e) => e.1 = v,
        None => env.push((k, v)),
    }
}

/// `WINEDLLOVERRIDES` as one value: the `preset` values already in the environment (Wine's
/// `dll1,dll2=n,b;dll3=d`), then `ours` (`(dll, value)`, short form: `winhttp=n,b`). A DLL both
/// name is ours: it is dropped from the preset's entry (the entry goes with its last DLL).
/// Entries that are not `names=value` are kept as written.
pub(crate) fn merge_dll_overrides(preset: &[String], ours: &[(String, String)]) -> String {
    let ours_text = crate::manifest::wine_dll_overrides(ours);
    let mut out: Vec<String> = Vec::new();
    for entry in preset.iter().flat_map(|p| p.split(';')) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((names, value)) = entry.split_once('=') else {
            out.push(entry.to_owned());
            continue;
        };
        let kept: Vec<&str> = names
            .split(',')
            .filter(|n| !ours.iter().any(|(d, _)| d.eq_ignore_ascii_case(n.trim())))
            .collect();
        if !kept.is_empty() {
            out.push(format!("{}={value}", kept.join(",")));
        }
    }
    out.extend(ours_text);
    out.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(d: &str, v: &str) -> (String, String) {
        (d.to_owned(), v.to_owned())
    }

    #[test]
    fn ours_replaces_a_dll_the_preset_names_and_keeps_the_rest() {
        let m = merge_dll_overrides(
            &["mscoree=d;winhttp,dxgi=n".to_owned(), "x=b;".to_owned()],
            &[o("winhttp", "native,builtin")],
        );
        assert_eq!(m, "mscoree=d;dxgi=n;x=b;winhttp=n,b");
    }

    #[test]
    fn nothing_to_merge_is_empty() {
        assert_eq!(merge_dll_overrides(&[], &[]), "");
    }

    #[test]
    fn env_keys() {
        assert!(is_env_key("WINE_X1") && is_env_key("_A"));
        assert!(!is_env_key("") && !is_env_key("1A") && !is_env_key("wine") && !is_env_key("A-B"));
    }
}
