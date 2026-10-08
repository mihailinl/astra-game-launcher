// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play: check the install against its ledger, write the INI keys, apply the Proton override,
//! and build the command. Spawning is the caller's (`Spawn`), so the daemon can own its children.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result, codes};
use crate::game::{GameTarget, Platform};
use crate::ini;
use crate::ledger::{self, LedgerState, ProfileLock};
use crate::manifest::{Manifest, OWN_PLACEHOLDERS, Piece, pieces};
use crate::paths::{
    FileState, canonical_dir, checked_rel, existing_target, file_state, parse_jailed, plain,
    prepare_target, write_atomic,
};
use crate::plan::GameAction;
use crate::proton;
use crate::running::refuse_if_running;

/// A command, as an argv array: never a shell string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

/// Starts a process. The daemon passes its own; [`StdSpawn`] is the plain one.
pub trait Spawn {
    fn spawn(&self, plan: &ProcessPlan) -> Result<()>;
}

/// Starts the process detached (its own process group; on Windows also out of the caller's job
/// when the job allows it) and reaps it on a background thread.
pub struct StdSpawn;

impl Spawn for StdSpawn {
    fn spawn(&self, plan: &ProcessPlan) -> Result<()> {
        let mut cmd = Command::new(&plan.program);
        cmd.args(&plan.args)
            .envs(plan.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(cwd) = &plan.cwd {
            cmd.current_dir(cwd);
        }
        let failed = |e: std::io::Error| {
            LauncherError::io(e).with("program", plan.program.display().to_string())
        };
        #[cfg(unix)]
        let child = {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
            cmd.spawn().map_err(failed)?
        };
        #[cfg(windows)]
        let child = {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            const ERROR_ACCESS_DENIED: i32 = 5;
            cmd.creation_flags(
                DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
            );
            match cmd.spawn() {
                Ok(c) => c,
                Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
                    cmd.spawn().map_err(failed)?
                }
                Err(e) => return Err(failed(e)),
            }
        };
        #[cfg(not(any(unix, windows)))]
        let child = cmd.spawn().map_err(failed)?;
        let mut child = child;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Runs a prepared command.
pub fn launch(plan: &ProcessPlan, spawner: &dyn Spawn) -> Result<()> {
    spawner.spawn(plan)
}

/// How a path is written for the game.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Native,
    /// Under Proton: `Z:\…` with backslashes.
    Proton,
}

fn path_text(p: &Path, flavor: Flavor) -> Result<String> {
    let s = p.to_str().ok_or_else(|| {
        LauncherError::io("a path is not UTF-8").with("path", p.display().to_string())
    })?;
    Ok(match flavor {
        Flavor::Native => s.to_owned(),
        Flavor::Proton => format!("Z:{}", s.replace('/', "\\")),
    })
}

struct Expander<'a> {
    profile: String,
    game: String,
    vars: &'a BTreeMap<String, String>,
    /// Windows-style paths (Windows, or Proton).
    backslashes: bool,
}

impl Expander<'_> {
    /// Fills one argv element. An element that starts with `${profile}` or `${game}` is a path:
    /// on Windows and under Proton its slashes become backslashes.
    fn expand(&self, s: &str) -> Result<String> {
        let ps = pieces(s)?;
        let mut out = String::new();
        for p in &ps {
            match p {
                Piece::Lit(l) => out.push_str(l),
                Piece::Var("profile") => out.push_str(&self.profile),
                Piece::Var("game") => out.push_str(&self.game),
                Piece::Var(name) => match self.vars.get(*name) {
                    Some(v) => out.push_str(v),
                    None => {
                        return Err(LauncherError::new(codes::MISSING_VARIABLE).with("name", *name));
                    }
                },
            }
        }
        let is_path = matches!(ps.first(), Some(Piece::Var(n)) if OWN_PLACEHOLDERS.contains(n));
        if is_path && self.backslashes {
            out = out.replace('/', "\\");
        }
        Ok(out)
    }
}

fn vars_in(s: &str) -> Vec<String> {
    pieces(s)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p {
            Piece::Var(n) if !OWN_PLACEHOLDERS.contains(&n) => Some(n.to_owned()),
            _ => None,
        })
        .collect()
}

fn needs_reinstall(file: &str) -> LauncherError {
    LauncherError::new(codes::NEEDS_REINSTALL).with("file", file)
}

/// Prepares a launch: checks the install, writes `[[config_writes]]` (into the profile only),
/// applies the Proton override (`proton-prefix-missing` when the prefix does not exist yet) and
/// returns the command — `<steam> -applaunch <appid> <args…>` for a Steam game, otherwise the
/// exe itself with its args and env.
pub fn prepare_launch(
    profile_dir: &Path,
    game: &GameTarget,
    vars: &BTreeMap<String, String>,
    steam_exe: Option<&Path>,
) -> Result<ProcessPlan> {
    let profile =
        canonical_dir(profile_dir).map_err(|_| LauncherError::new(codes::NOT_INSTALLED))?;
    let _lock = ProfileLock::acquire(&profile)?;
    let Some(mut ledger) = ledger::read(&profile)? else {
        return Err(LauncherError::new(codes::NOT_INSTALLED));
    };
    if ledger.state != LedgerState::Installed {
        return Err(needs_reinstall("").with("reason", "incomplete"));
    }
    let m = Manifest::parse(&ledger.manifest)?;

    // Every variable present, and safe where it goes.
    for name in m.variables() {
        match vars.get(&name) {
            None => return Err(LauncherError::new(codes::MISSING_VARIABLE).with("name", name)),
            Some(v) if v.contains('\0') => {
                return Err(LauncherError::new(codes::INVALID_VARIABLE).with("name", name));
            }
            Some(_) => {}
        }
    }
    for c in &m.config_writes {
        for v in c.set.values() {
            for name in vars_in(v) {
                if vars.get(&name).is_some_and(|v| v.contains(['\n', '\r'])) {
                    return Err(LauncherError::new(codes::INVALID_VARIABLE).with("name", name));
                }
            }
        }
    }

    // The install is whole.
    for f in &ledger.files {
        let ok = match existing_target(&profile, &checked_rel(&f.rel)?)? {
            Some(p) => fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()),
            None => false,
        };
        if !ok {
            return Err(needs_reinstall(&f.rel));
        }
    }
    let game_root = game.game_root()?;
    for g in &ledger.game_files {
        if g.action == GameAction::KeepExisting {
            continue;
        }
        let state = match existing_target(&game_root, &checked_rel(&g.rel)?)? {
            Some(p) => file_state(&p)?,
            None => FileState::Absent,
        };
        if state
            != (FileState::File {
                sha256: g.sha256.clone(),
            })
        {
            return Err(needs_reinstall(&g.rel));
        }
    }

    let exe_file = game.exe_file_name()?;
    refuse_if_running(&exe_file)?;

    // A Proton override needs the prefix; refuse before writing anything.
    let override_ = match &game.platform {
        Platform::LinuxProton { prefix } => match m.proton_override() {
            Some(o) => {
                let ok = prefix.as_deref().is_some_and(|p| {
                    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
                        && fs::symlink_metadata(p.join("user.reg")).is_ok_and(|m| m.is_file())
                });
                if !ok {
                    return Err(LauncherError::new(codes::PROTON_PREFIX_MISSING).with(
                        "prefix",
                        prefix
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    ));
                }
                Some((prefix.clone().unwrap_or_default(), o))
            }
            None => None,
        },
        _ => None,
    };
    if game.steam_appid.is_none() && matches!(game.platform, Platform::LinuxProton { .. }) {
        return Err(LauncherError::new(codes::PLATFORM_UNSUPPORTED)
            .with("platform", "linux-proton")
            .with("reason", "needs-steam-appid"));
    }
    if game.steam_appid.is_some() && steam_exe.is_none() {
        return Err(LauncherError::new(codes::STEAM_NOT_FOUND));
    }

    let flavor = match game.platform {
        Platform::LinuxProton { .. } => Flavor::Proton,
        _ => Flavor::Native,
    };
    let x = Expander {
        profile: path_text(&plain(&profile), flavor)?,
        game: path_text(&plain(&game_root), flavor)?,
        vars,
        backslashes: flavor == Flavor::Proton || matches!(game.platform, Platform::Windows),
    };
    let args = m
        .launch
        .args
        .iter()
        .map(|a| x.expand(a))
        .collect::<Result<Vec<_>>>()?;

    for c in &m.config_writes {
        let jp = parse_jailed(&c.file)?;
        let target = prepare_target(&profile, &jp.rel)?;
        let old = match fs::read(&target) {
            Ok(b) => String::from_utf8(b).map_err(|_| {
                LauncherError::io("not UTF-8").with("path", target.display().to_string())
            })?,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(LauncherError::io_at(&e, &target)),
        };
        let keys = c
            .set
            .iter()
            .map(|(k, v)| Ok((k.clone(), x.expand(v)?)))
            .collect::<Result<Vec<_>>>()?;
        let new = ini::set_keys(&old, &c.section, &keys);
        if new != old {
            write_atomic(&target, new.as_bytes())?;
        }
    }

    if let Some((prefix, (dll, value))) = override_ {
        let rec = proton::apply_prefix(&prefix, &exe_file, &dll, &value, ledger.proton.as_ref())?;
        if ledger.proton.as_ref() != Some(&rec) {
            ledger.proton = Some(rec);
            ledger::write(&profile, &ledger)?;
        }
    }

    if let Some(appid) = game.steam_appid {
        let steam = steam_exe.expect("checked above");
        let mut argv = vec!["-applaunch".to_owned(), appid.to_string()];
        argv.extend(args);
        return Ok(ProcessPlan {
            program: steam.to_path_buf(),
            args: argv,
            env: Vec::new(),
            cwd: None,
        });
    }
    let env = m
        .launch
        .env
        .iter()
        .map(|(k, v)| Ok((k.clone(), x.expand(v)?)))
        .collect::<Result<Vec<_>>>()?;
    Ok(ProcessPlan {
        program: plain(&game_root.join(&exe_file)),
        args,
        env,
        cwd: Some(plain(&game_root)),
    })
}

/// Where Steam's launcher is on this machine, if it can be found.
pub fn find_steam() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for var in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(base) = std::env::var_os(var) {
                let p = PathBuf::from(base).join("Steam").join("steam.exe");
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join("steam"))
            .find(|p| p.is_file())
    }
}
