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

/// The variables that decide WHICH program the Linux runner starts, with which libraries, in
/// which prefix and from where it downloads. A game integration's `[launch] env` is
/// third-party text and the runner env is the caller's: neither may steer the runner, or an
/// integration could make Wine load its own `LD_PRELOAD`, run its own `PATH` programs, or
/// move the prefix the launcher checked. The only source of the prefix and the compat folder is
/// [`WineRunner`]'s fields; `WINEDLLOVERRIDES` is the one `WINE*` name that is merged instead.
/// This is the ONE place the set lives.
pub(crate) fn is_runner_control(key: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "LD_",
        "XDG_",
        "PYTHON",
        "WINE",
        "PROTON",
        "STEAM_COMPAT_",
        "UMU_",
    ];
    const NAMES: &[&str] = &["PATH", "HOME", "GAMEID"];
    key != OVERRIDES_VAR && (PREFIXES.iter().any(|p| key.starts_with(p)) || NAMES.contains(&key))
}

/// `WINE*` names the caller's `runner.env` (never an integration's `[launch] env`) may carry:
/// Heroic sets them as ordinary performance and debug toggles of the game's own settings, and
/// none of them chooses a program, a library or a folder.
const CALLER_WINE_TOGGLES: &[&str] = &[
    "WINEDEBUG",
    "WINEESYNC",
    "WINEFSYNC",
    "WINE_LARGE_ADDRESS_AWARE",
    "WINE_FULLSCREEN_FSR",
    "WINE_FULLSCREEN_FSR_STRENGTH",
];

/// Whether the CALLER's `runner.env` may carry `key`: Heroic's own `PROTON_*`, `UMU_*`,
/// `STEAM_COMPAT_*` and `GAMEID` (the runner's settings), but never what steers the process
/// ([`is_runner_control`]) nor the four names the fields own.
pub(crate) fn caller_may_carry(key: &str) -> bool {
    if CALLER_WINE_TOGGLES.contains(&key) {
        return true;
    }
    const OWNED: &[&str] = &[
        "WINEPREFIX",
        "STEAM_COMPAT_DATA_PATH",
        "WINELOADER",
        "WINESERVER",
    ];
    if OWNED.contains(&key) {
        return false;
    }
    !is_runner_control(key)
        || ["PROTON", "STEAM_COMPAT_", "UMU_"]
            .iter()
            .any(|p| key.starts_with(p))
        || key == "GAMEID"
}

/// What an integration's `[launch] env` may set under a Wine runner: an ALLOWLIST, not a
/// denylist, because the list of variables that make a process load code from a path
/// (`VK_LAYER_PATH`, `VK_ICD_FILENAMES`, `GST_PLUGIN_PATH`, `LIBGL_DRIVERS_PATH`,
/// `__EGL_VENDOR_LIBRARY_*`, `GCONV_PATH`, `BASH_ENV`, `ENV`, …) has no end, and a relative value
/// resolves against the working directory, which is the game's folder. Only the loaders' own
/// settings (`DOORSTOP_*`, `BEPINEX_*`, `ASTRA_*`) and `WINEDLLOVERRIDES` (merged) pass.
/// [`is_runner_control`] is the CALLER side's rule, for the runner's own `env`.
pub(crate) fn manifest_may_set(key: &str) -> bool {
    key == OVERRIDES_VAR
        || ["DOORSTOP_", "BEPINEX_", "ASTRA_"]
            .iter()
            .any(|p| key.starts_with(p))
}

/// Whether `key` names a place (`…_PATH`, `…_PATHS`, `…_DIR`, `…_MOUNTS`); `Some(true)` when its
/// value is a `:`-separated list.
fn path_variable(key: &str) -> Option<bool> {
    if key.ends_with("_PATHS") || key.ends_with("_MOUNTS") {
        Some(true)
    } else if key.ends_with("_PATH") || key.ends_with("_DIR") {
        Some(false)
    } else {
        None
    }
}

/// What [`WineRunner::resolve`] checked: the canonical program and folders the process is built
/// from. The launch gate resolves again and refuses when any of them differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    bin: PathBuf,
    prefix: PathBuf,
    compat: Option<PathBuf>,
}

fn canon(p: &Path, reason: &str) -> Result<PathBuf> {
    std::fs::canonicalize(p).map_err(|_| invalid(reason, p))
}

fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl WineRunner {
    /// Everything about the runner that can be known without starting it, checked before
    /// anything is spawned or written and again right before the spawn (`runner-invalid`,
    /// `reason` and `path` or `key`). `profile` is the launcher's folder, `game_dir` the game's.
    /// - `bin` is absolute and resolves (a link is followed: distributions install `wine` as a
    ///   link) to a regular file with an execute bit that is not under the game folder or the
    ///   profile (`bin-in-game`, `bin-in-profile`: a program the game or an integration can
    ///   rewrite is never the runner);
    /// - `prefix` is absolute, an existing folder, and neither inside the profile nor holding
    ///   it (`prefix-in-profile`, `profile-in-prefix`); it is what `WINEPREFIX` says, canonical;
    /// - `compat_data` (REQUIRED for [`RunnerKind::Proton`], `compat-data-required`) is
    ///   absolute, an existing folder, outside the profile in both directions, and the prefix is
    ///   it (`compat_data` itself) or `compat_data/pfx` (`compat-data-mismatch` otherwise);
    /// - `env` keys that name a place (`*_PATH`, `*_DIR`, `*_PATHS`, `*_MOUNTS` — the plural forms
    ///   `:`-separated) hold only absolute existing folders away from the game and the profile
    ///   (`env-path`); `PROTON_REMOTE_DEBUG_CMD` is refused;
    /// - `env` keys are `[A-Z_][A-Z0-9_]*`, none is a name the fields own or one that steers the
    ///   runner (`env-key`, see [`caller_may_carry`]), and no value holds a NUL (`env-value`);
    /// - [`RunnerKind::Umu`] carries a non-empty `GAMEID` and a `PROTONPATH` that is an absolute
    ///   existing folder outside the game and the profile (`umu-gameid`, `umu-protonpath`,
    ///   `protonpath-in-game`, `protonpath-in-profile`): without it umu downloads a Proton.
    pub fn check(&self, profile: &Path, game_dir: &Path) -> Result<()> {
        self.resolve(profile, game_dir).map(|_| ())
    }

    pub(crate) fn resolve(&self, profile: &Path, game_dir: &Path) -> Result<Resolved> {
        let profile = canon(profile, "profile-missing")?;
        let game = std::fs::canonicalize(game_dir).unwrap_or_else(|_| game_dir.to_path_buf());
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
        let bin = canon(&self.bin, "bin-not-file")?;
        if bin.starts_with(&game) {
            return Err(invalid("bin-in-game", &self.bin));
        }
        if bin.starts_with(&profile) {
            return Err(invalid("bin-in-profile", &self.bin));
        }

        if !self.prefix.is_absolute() {
            return Err(invalid("prefix-not-absolute", &self.prefix));
        }
        if !std::fs::metadata(&self.prefix).is_ok_and(|m| m.is_dir()) {
            return Err(invalid("prefix-missing", &self.prefix));
        }
        let prefix = canon(&self.prefix, "prefix-missing")?;
        if prefix.starts_with(&profile) {
            return Err(invalid("prefix-in-profile", &self.prefix));
        }
        if profile.starts_with(&prefix) {
            return Err(invalid("profile-in-prefix", &self.prefix));
        }

        let compat = match &self.compat_data {
            None if self.kind == RunnerKind::Proton => {
                return Err(invalid("compat-data-required", &self.prefix));
            }
            None => None,
            Some(c) => {
                if !c.is_absolute() {
                    return Err(invalid("compat-data-not-absolute", c));
                }
                if !std::fs::metadata(c).is_ok_and(|m| m.is_dir()) {
                    return Err(invalid("compat-data-missing", c));
                }
                let cc = canon(c, "compat-data-missing")?;
                if cc.starts_with(&profile) {
                    return Err(invalid("compat-data-in-profile", c));
                }
                if profile.starts_with(&cc) {
                    return Err(invalid("profile-in-compat-data", c));
                }
                let pfx = std::fs::canonicalize(cc.join("pfx")).ok();
                // Proton keeps its prefix in `<compat>/pfx` and nowhere else; Wine and umu may
                // be given the folder itself.
                let ok = pfx.as_ref() == Some(&prefix)
                    || (self.kind != RunnerKind::Proton && prefix == cc);
                if !ok {
                    return Err(invalid("compat-data-mismatch", c));
                }
                Some(cc)
            }
        };

        for (k, v) in &self.env {
            if !is_env_key(k) || !caller_may_carry(k) {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "env-key")
                    .with("key", k.clone()));
            }
            if v.contains('\0') {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "env-value")
                    .with("key", k.clone()));
            }
            // A caller's variable that names where the runner reads or writes is held to the
            // rule `PROTONPATH` is: an existing absolute folder away from the game and profile.
            if k == "PROTON_REMOTE_DEBUG_CMD" {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "env-key")
                    .with("key", k.clone()));
            }
            if let Some(plural) = path_variable(k) {
                let paths: Vec<&str> = if plural {
                    v.split(':').collect()
                } else {
                    vec![v.as_str()]
                };
                for p in paths.into_iter().filter(|p| !p.is_empty()) {
                    let p = Path::new(p);
                    let bad = || {
                        LauncherError::new(codes::RUNNER_INVALID)
                            .with("reason", "env-path")
                            .with("key", k.clone())
                            .with("path", p.display().to_string())
                    };
                    if !p.is_absolute() || !std::fs::metadata(p).is_ok_and(|m| m.is_dir()) {
                        return Err(bad());
                    }
                    let pc = canon(p, "env-path")?;
                    if nested(&pc, &game) || nested(&pc, &profile) {
                        return Err(bad());
                    }
                }
            }
        }
        if self.kind == RunnerKind::Umu {
            let get = |k: &str| {
                self.env
                    .iter()
                    .find(|(ek, v)| ek == k && !v.is_empty())
                    .map(|(_, v)| v.as_str())
            };
            if get("GAMEID").is_none() {
                return Err(invalid("umu-gameid", &self.bin));
            }
            let pp = PathBuf::from(get("PROTONPATH").unwrap_or_default());
            if !pp.is_absolute() || !std::fs::metadata(&pp).is_ok_and(|m| m.is_dir()) {
                return Err(invalid("umu-protonpath", &pp));
            }
            let ppc = canon(&pp, "umu-protonpath")?;
            if ppc.starts_with(&game) || game.starts_with(&ppc) {
                return Err(invalid("protonpath-in-game", &pp));
            }
            if nested(&ppc, &profile) {
                return Err(invalid("protonpath-in-profile", &pp));
            }
        }
        Ok(Resolved {
            bin,
            prefix,
            compat,
        })
    }

    /// The program, its arguments and its environment for starting `exe` with `args`.
    /// `base_env` is the integration's own (`[launch] env`, expanded, already refused if it names
    /// a control variable); the runner's `env` is applied over it, as given, except
    /// `WINEDLLOVERRIDES`, which is merged ([`merge_dll_overrides`]) with `overrides`
    /// (`(dll, value)` pairs, the integration's). `WINEPREFIX` is always the CHECKED, canonical
    /// prefix; for Proton `STEAM_COMPAT_DATA_PATH` is the checked `compat_data`, and an empty
    /// `STEAM_COMPAT_CLIENT_INSTALL_PATH` is added when unset (the `proton` script reads it).
    pub(crate) fn command(
        &self,
        at: &Resolved,
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
        set(
            &mut env,
            "WINEPREFIX".into(),
            at.prefix.display().to_string(),
        );
        if self.kind == RunnerKind::Proton {
            if let Some(c) = &at.compat {
                set(
                    &mut env,
                    "STEAM_COMPAT_DATA_PATH".into(),
                    c.display().to_string(),
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
        (at.bin.clone(), argv, env)
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
    fn what_steers_the_runner_is_refused_and_heroics_own_is_not() {
        for k in [
            "LD_PRELOAD",
            "PATH",
            "HOME",
            "XDG_DATA_HOME",
            "PYTHONPATH",
            "WINEPREFIX",
            "WINELOADER",
            "PROTON_LOG",
            "STEAM_COMPAT_X",
            "UMU_X",
            "GAMEID",
        ] {
            assert!(is_runner_control(k), "{k}");
        }
        assert!(!is_runner_control("WINEDLLOVERRIDES") && !is_runner_control("DXVK_HUD"));
        for k in [
            "PROTON_LOG",
            "UMU_LOG",
            "GAMEID",
            "STEAM_COMPAT_CLIENT_INSTALL_PATH",
            "DXVK_HUD",
        ] {
            assert!(caller_may_carry(k), "{k}");
        }
        for k in [
            "LD_PRELOAD",
            "PATH",
            "HOME",
            "XDG_X",
            "PYTHONHOME",
            "WINEPREFIX",
            "WINESERVER",
            "WINEFOO",
            "STEAM_COMPAT_DATA_PATH",
        ] {
            assert!(!caller_may_carry(k), "{k}");
        }
    }

    #[test]
    fn env_keys() {
        assert!(is_env_key("WINE_X1") && is_env_key("_A"));
        assert!(!is_env_key("") && !is_env_key("1A") && !is_env_key("wine") && !is_env_key("A-B"));
    }
}
