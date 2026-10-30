// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play: check the install against its ledger, write the INI keys, apply the Proton override,
//! and build the command. Spawning is the caller's (`Spawn`), so the daemon can own its children.
//!
//! **A file we added that someone else rewrote.** r2modman and Gale copy their profile's Doorstop
//! proxy (`winhttp.dll`) and `doorstop_config.ini` into the game folder at every modded start,
//! over the ones we added, and ReShade rewrites `ReShade.ini` whenever the player changes a
//! setting. Play does not send the person back to Install for that (which would only write ours
//! back, to be overwritten again): a relinked proxy that still does our proxy's job is recorded
//! as shared (`share-loader`, with the digest there now), one that no longer does is
//! `shared-loader-changed`, and a changed config of the kind a plan leaves alone when it is
//! already there (`doorstop_config.ini`, `ReShade.ini`: `plan::keep_if_present`) is recorded as
//! theirs (`keep-existing`) and never rewritten. A Doorstop config that is theirs is kept out of
//! the session by the Doorstop options a foreign config gets (`doorstop::session_options`); only
//! that file decides those options, never `ReShade.ini`. A file of ours that is gone, and any
//! other added file that changed, is still `needs-reinstall`.
//!
//! What Play records about those files is written to the ledger last, once nothing can refuse
//! this Play any more: a refused Play records nothing.
//!
//! **`<profile>/play-arm.json`** (the runner's contract). Steam starts the game, so this is how
//! the runner knows that the start it sees is one Astra asked for, and when:
//! `{"schema":1,"appid":<steam appid>,"started_unix_ms":<ms since the UNIX epoch>}`.
//! [`prepare_launch`] writes nothing: the plan it returns for a Steam game carries the arm, and
//! [`launch`] writes it atomically, under the profile lock, immediately before it spawns Steam.
//! So whatever the caller refuses between the two (Astra's daemon checks its bridge engine there)
//! leaves no fresh arm for a later plain Steam start to be taken for Astra's, and a spawn that
//! fails puts back what was there. `started_unix_ms` is the moment Astra asked Steam to start the
//! game, not the moment the game's process started: Steam may update the game first. A failed
//! write fails the launch before anything is spawned. A game without a Steam appid gets none. The
//! launcher never deletes an arm a launch left (removing the profile folder is the caller's), it
//! is not a file of the ledger's, and no manifest file may be named `play-arm.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::doorstop;
use crate::error::{LauncherError, Result, codes};
use crate::game::{GameTarget, Platform};
use crate::ini;
use crate::ledger::{self, LedgerGameFile, LedgerState, ProfileLock};
use crate::manifest::{Manifest, OWN_PLACEHOLDERS, Piece, pieces};
use crate::paths::{
    FileState, PLAY_ARM_FILE, canonical_dir, check_regular_or_absent, checked_rel, existing_target,
    file_state, parse_jailed, plain, prepare_target, real_case, write_atomic,
};
use crate::plan::{GameAction, is_loader_proxy, keep_if_present};
use crate::proton;
use crate::running::refuse_if_running;

/// A command, as an argv array: never a shell string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// The play-arm [`launch`] writes right before it spawns (a Steam game). Only
    /// [`prepare_launch`] sets it, and it is never serialised or read back, so no plan made
    /// elsewhere can have `launch` write into another folder.
    #[serde(skip)]
    arm: Option<Arm>,
}

/// Where a launch's `play-arm.json` goes, and the Steam app it names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arm {
    /// The profile, as [`prepare_launch`] resolved it.
    profile: PathBuf,
    appid: u32,
}

impl ProcessPlan {
    /// The `play-arm.json` [`launch`] writes immediately before it spawns this plan, and the
    /// Steam appid it names; `None` for a game started without Steam.
    pub fn play_arm(&self) -> Option<(PathBuf, u32)> {
        self.arm
            .as_ref()
            .map(|a| (a.profile.join(PLAY_ARM_FILE), a.appid))
    }
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

/// Runs a prepared command. For a Steam game it first writes `<profile>/play-arm.json` (the
/// module docs), under the profile lock and immediately before the spawn: a failed write fails
/// the launch with nothing started, and a failed spawn puts back what was there before, since an
/// arm left by a launch that never happened would vouch for the next plain start of the game.
pub fn launch(plan: &ProcessPlan, spawner: &dyn Spawn) -> Result<()> {
    let Some(arm) = &plan.arm else {
        return spawner.spawn(plan);
    };
    let _lock = ProfileLock::acquire(&arm.profile)?;
    let path = arm.profile.join(PLAY_ARM_FILE);
    // A link or a folder there is not an arm of ours: never read through it, never replace it.
    check_regular_or_absent(&path)?;
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(LauncherError::io_at(&e, &path)),
    };
    write_play_arm(&path, arm.appid)?;
    if let Err(e) = spawner.spawn(plan) {
        let _ = match previous {
            Some(bytes) => write_atomic(&path, &bytes),
            None => fs::remove_file(&path).map_err(|e| LauncherError::io_at(&e, &path)),
        };
        return Err(e);
    }
    Ok(())
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

/// `<profile>/play-arm.json`, as the runner reads it (the module docs).
#[derive(Serialize)]
struct PlayArm {
    schema: u32,
    appid: u32,
    started_unix_ms: u64,
}

/// Writes the play-arm at `path` for a launch of Steam app `appid` that Astra asks for now.
fn write_play_arm(path: &Path, appid: u32) -> Result<()> {
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let arm = PlayArm {
        schema: 1,
        appid,
        started_unix_ms,
    };
    let text = serde_json::to_vec(&arm).map_err(LauncherError::io)?;
    write_atomic(path, &text)
}

/// Prepares a launch: checks the install, writes `[[config_writes]]` (into the profile only),
/// applies the Proton override (`proton-prefix-missing` when the prefix does not exist yet) and
/// returns the command — `<steam> -applaunch <appid> <args…>` for a Steam game, otherwise the
/// exe itself with its args and env. A Steam game's plan carries the play-arm [`launch`] writes;
/// this writes none.
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

    // This profile's install is THIS game: the folder and the program the ledger recorded.
    let dir = plain(&canonical_dir(&game.dir)?);
    if dir != ledger.game.dir {
        return Err(LauncherError::new(codes::GAME_MISMATCH)
            .with("field", "dir")
            .with("want", ledger.game.dir.display().to_string())
            .with("got", dir.display().to_string()));
    }
    let exe_rel = game.exe_rel()?;
    if !exe_rel.eq_ignore_ascii_case(&ledger.game.exe) {
        return Err(LauncherError::new(codes::GAME_MISMATCH)
            .with("field", "exe")
            .with("want", ledger.game.exe.clone())
            .with("got", exe_rel));
    }

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
    let exe_machine = doorstop::pe_machine_of_file(&game_root.join(game.exe_file_name()?));
    // Only a launch through Doorstop added a proxy another mod manager's Doorstop can stand in
    // for. A package that starts another kind of proxy (ReShade's `dxgi.dll`) has none to share.
    let through_doorstop = doorstop::launch_options(&m.launch.args).is_some();
    // Files we added that someone else has since rewritten, and what they are now:
    // (index into `game_files`, the action that now describes them, the digest there now).
    let mut relinked: Vec<(usize, GameAction, String)> = Vec::new();
    for (i, g) in ledger.game_files.iter().enumerate() {
        if g.action == GameAction::KeepExisting {
            continue;
        }
        let path = existing_target(&game_root, &checked_rel(&g.rel)?)?;
        let state = match &path {
            Some(p) => file_state(p)?,
            None => FileState::Absent,
        };
        let pinned = FileState::File {
            sha256: g.sha256.clone(),
        };
        if g.action != GameAction::ShareLoader && state == pinned {
            continue;
        }
        // Gone: Install again (which adds ours where theirs went).
        let FileState::File { sha256: now } = state else {
            return Err(needs_reinstall(&g.rel));
        };
        // A config we added (or found with our very bytes) that someone else rewrote since — a
        // mod manager's `doorstop_config.ini` over ours (r2modman and Gale copy theirs in at every
        // modded start), the settings ReShade saves into `ReShade.ini`: theirs now, as one that
        // was there before us would be. Never rewritten. Only a Doorstop config changes the
        // session, through `foreign_config`, which reads that file alone.
        if matches!(g.action, GameAction::Add | GameAction::ReuseKnownProxy)
            && keep_if_present(&g.rel)
        {
            relinked.push((i, GameAction::KeepExisting, now));
            continue;
        }
        // A loader proxy another mod manager owns — shared, there before us with our very bytes,
        // or relinked over the Doorstop we added — is relinked by that manager whenever it likes:
        // what matters is that the one there now still does the job, not that it is the same file.
        let relinked_proxy =
            g.action == GameAction::Add && is_loader_proxy(&g.rel) && through_doorstop;
        let theirs = g.action == GameAction::ShareLoader
            || (g.action == GameAction::ReuseKnownProxy && is_loader_proxy(&g.rel))
            || relinked_proxy;
        // Any other file of ours that changed: Install again.
        if !theirs {
            return Err(needs_reinstall(&g.rel));
        }
        let serves = path
            .as_deref()
            .and_then(doorstop::read_proxy)
            .is_some_and(|bytes| doorstop::serves(&bytes, exe_machine, &m.launch.args));
        if !serves {
            // Still there and no longer able (the other manager moved to a loader that does not
            // take our options). Not "another loader is installed": the person installed beside
            // that one, and it is the one that changed. Installing again would be refused with
            // `foreign-loader`, so this does not send them there.
            return Err(
                LauncherError::new(codes::SHARED_LOADER_CHANGED).with("file", g.rel.clone())
            );
        }
        if relinked_proxy {
            relinked.push((i, GameAction::ShareLoader, now));
        }
    }
    // Whose they are now: the session's Doorstop options read it below, and the ledger records it
    // at the very end, once nothing can refuse this Play any more. Then an uninstall leaves them
    // and an update shares them.
    let mut game_files = ledger.game_files.clone();
    for (i, action, sha256) in relinked {
        let g = &mut game_files[i];
        g.action = action;
        g.sha256 = sha256;
    }

    let exe_file = game.exe_file_name()?;
    refuse_if_running(&exe_file)?;
    // Steam can flag a game VAC after it was installed: check again at every Play. Still unread
    // now: only an install the person accepted that for may start.
    let trusted = crate::plan::trusted_appid(game)?;
    let vac_checked = crate::plan::vac_check(game, trusted, m.target.steam_appid)?;
    if !vac_checked && !ledger.vac_unchecked_accepted {
        return Err(LauncherError::new(codes::VAC_UNCHECKED));
    }

    // A Proton override needs the prefix; refuse before writing anything.
    let overrides = match &game.platform {
        Platform::LinuxProton { prefix } => match m.proton_overrides() {
            o if !o.is_empty() => {
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
            _ => None,
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
    // The manifest's arguments, then the Doorstop options that keep a `doorstop_config.ini` that
    // is not ours (a shared loader's, one kept beside our proxy) out of the session.
    let foreign_config = foreign_config(&game_root, &game_files)?;
    let args = m
        .launch
        .args
        .iter()
        .cloned()
        .chain(doorstop::session_options(
            &m.launch.args,
            &exe_file,
            foreign_config,
        ))
        .map(|a| x.expand(&a))
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

    if let Some((prefix, overrides)) = overrides {
        let existing = ledger.proton.clone();
        // The ledger learns of the change BEFORE user.reg is written (see `apply_prefix`).
        proton::apply_prefix(&prefix, &exe_file, &overrides, &existing, |recs| {
            ledger.proton = recs.to_vec();
            ledger::write(&profile, &ledger)
        })?;
    }

    let plan = match game.steam_appid {
        Some(appid) => {
            let steam = steam_exe.expect("checked above");
            let mut argv = vec!["-applaunch".to_owned(), appid.to_string()];
            argv.extend(args);
            ProcessPlan {
                program: steam.to_path_buf(),
                args: argv,
                env: Vec::new(),
                cwd: None,
                // Written by `launch`, right before Steam is asked to start the game.
                arm: Some(Arm {
                    profile: profile.clone(),
                    appid,
                }),
            }
        }
        None => {
            let env = m
                .launch
                .env
                .iter()
                .map(|(k, v)| Ok((k.clone(), x.expand(v)?)))
                .collect::<Result<Vec<_>>>()?;
            ProcessPlan {
                program: plain(&game_root.join(&exe_file)),
                args,
                env,
                cwd: Some(plain(&game_root)),
                arm: None,
            }
        }
    };

    // Last, once nothing can refuse this Play any more: what was rewritten is theirs now.
    if game_files != ledger.game_files {
        ledger.game_files = game_files;
        ledger::write(&profile, &ledger)?;
    }
    Ok(plan)
}

/// Whether the `doorstop_config.ini` Doorstop reads where the game starts (the exe's folder, the
/// working folder of every start) is someone else's: there, and not our own file unchanged. A
/// link or a folder in its place is not ours either.
fn foreign_config(game_root: &Path, game_files: &[LedgerGameFile]) -> Result<bool> {
    let ini = real_case(game_root, &[doorstop::CONFIG_NAME.to_owned()])?;
    let rel = ini.join("/");
    let state = match existing_target(game_root, &ini).and_then(|p| match p {
        Some(p) => file_state(&p),
        None => Ok(FileState::Absent),
    }) {
        Ok(state) => state,
        Err(e) if e.code == codes::PATH_ESCAPES => return Ok(true),
        Err(e) => return Err(e),
    };
    Ok(match state {
        FileState::Absent => false,
        FileState::File { sha256 } => !game_files.iter().any(|g| {
            g.rel.eq_ignore_ascii_case(&rel)
                && matches!(g.action, GameAction::Add | GameAction::ReuseKnownProxy)
                && g.sha256 == sha256
        }),
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
