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
//! already there (`doorstop_config.ini`, `ReShade.ini`, and the manifest's own
//! `[integration.loader] keep_if_present`: `plan::LoaderPolicy::keep`) is recorded as
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
//! [`launch`] writes it atomically, under the profile lock, immediately before it spawns Steam
//! (after asking the anti-cheat gate and Steam's update state again).
//! So whatever the caller refuses between the two (Astra's daemon checks its bridge engine there)
//! leaves no fresh arm for a later plain Steam start to be taken for Astra's, and a spawn that
//! fails puts back what was there. `started_unix_ms` is the moment Astra asked Steam to start the
//! game, not the moment the game's process started: Steam may update the game first (an update
//! it already knows of is refused, `game-update-pending`; one it learns of only once asked is
//! not seen, docs/ENGINE-DESCRIPTORS-DEBT.md row 56). A failed
//! write fails the launch before anything is spawned. A game without a Steam appid gets none. The
//! one way an arm a launch left is taken away is [`remove_play_arm_if_older`] (the daemon's crash
//! guard: under the lock, only an arm older than its decision); uninstall leaves it (removing the
//! profile folder is the caller's). It is not a file of the ledger's, and no manifest file may be
//! named `play-arm.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::doorstop;
use crate::engines::EngineSet;
use crate::error::{LauncherError, Result, codes};
use crate::game::{GameTarget, Platform};
use crate::ini;
use crate::ledger::{self, LedgerGameFile, LedgerState, ProfileLock};
use crate::manifest::{Manifest, OWN_PLACEHOLDERS, Piece, pieces};
use crate::mods::ModsEnv;
use crate::mods::play::{GamePath, without_doorstop_options};
use crate::paths::{
    FileState, PLAY_ARM_FILE, canonical_dir, check_regular_or_absent, checked_rel, existing_target,
    file_state, parse_jailed, plain, real_case, write_atomic,
};
use crate::plan::{GameAction, LoaderPolicy, is_loader_proxy, refuse_anti_cheat};
use crate::proton;
use crate::running::refuse_if_running;
use crate::safe_dir::SafeDir;

/// A command, as an argv array: never a shell string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// Things the person should know before playing, as codes:
    /// [`STEAM_LAUNCH_OPTIONS_DOORSTOP`] (their Steam launch options for the game pass Doorstop
    /// options of their own, which may win over these), [`MODS_CHOICE_UNREADABLE`] (their
    /// answer about their mods could not be read), [`MODS_PACK_PATH_REFUSED`] (a path in their
    /// pack's Doorstop settings was left out). Never a reason not to play.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The folder [`launch`] grants Steam's sandbox read/write access to right before the spawn,
    /// when Steam is Flathub's Flatpak (`crate::flatpak`): `<profile>/BepInEx`, canonical, never
    /// the profile itself; `None` for any other plan, and for one that hands the game nothing of
    /// the profile. Only a plan [`prepare_launch`] made is granted, and only this
    /// folder: a plan read back, or one whose field was changed, is refused or spawned as it is
    /// ([`launch`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_grant: Option<PathBuf>,
    /// What [`launch`] checks again under the profile lock right before it spawns, and the
    /// play-arm it writes then (a Steam game). Only [`prepare_launch`] sets it, and it is never
    /// serialised or read back, so no plan made elsewhere can have `launch` write into another
    /// folder (a plan read back is spawned as it is, checking nothing).
    #[serde(skip)]
    gate: Option<Gate>,
}

/// The game a plan was prepared for, as [`prepare_launch`] resolved it: what [`launch`] asks the
/// anti-cheat gate and Steam's update state about again, and where its `play-arm.json` goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Gate {
    /// The profile, as [`prepare_launch`] resolved it.
    profile: PathBuf,
    game: GameTarget,
    /// The game's folder, resolved.
    game_dir: PathBuf,
    /// The Steam appid the game is known to be (`plan::trusted_appid`).
    trusted: Option<u32>,
    /// The Steam app the play-arm names; `None` for a game started without Steam (no arm).
    arm: Option<u32>,
    /// The `flatpak` program, when Steam is Flathub's Flatpak: what [`launch`] asks for Steam's
    /// permissions before it grants the profile to Steam's sandbox.
    flatpak: Option<PathBuf>,
    /// The folder [`launch`] grants Flathub's Steam (`<profile>/BepInEx`), as this plan's
    /// [`ProcessPlan::sandbox_grant`] states it.
    grant: Option<PathBuf>,
    /// What the plan's program and environment were built from (a Wine runner's checked
    /// program, prefix and compat folder); [`launch`] resolves the runner again and refuses a
    /// plan whose runner is no longer that.
    wine: Option<crate::wine::Resolved>,
}

impl Gate {
    /// The refusals that can become true between [`prepare_launch`] and the spawn: the anti-cheat
    /// the game carries now (`plan::refuse_anti_cheat`, with the caller's markers now) and an
    /// update Steam has not finished ([`refuse_update_pending`]).
    fn refuse(&self, engines: &EngineSet) -> Result<()> {
        refuse_anti_cheat(&self.game, &self.game_dir, self.trusted, engines)?;
        refuse_update_pending(&self.game.dir, self.trusted)?;
        // The runner is the caller's and may have changed since: asked again right before the spawn.
        if let Platform::LinuxWine { runner } = &self.game.platform {
            let now = runner.resolve(&self.profile, &self.game_dir)?;
            if self.wine.as_ref() != Some(&now) {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "runner-changed")
                    .with("path", runner.bin.display().to_string()));
            }
        }
        Ok(())
    }
}

impl ProcessPlan {
    /// The `play-arm.json` [`launch`] writes immediately before it spawns this plan, and the
    /// Steam appid it names; `None` for a game started without Steam.
    pub fn play_arm(&self) -> Option<(PathBuf, u32)> {
        let g = self.gate.as_ref()?;
        g.arm.map(|appid| (g.profile.join(PLAY_ARM_FILE), appid))
    }
}

/// `game-update-pending` when Steam will change the game's files before it starts it: the
/// `StateFlags` of `appid` in the library holding `game_dir` ([`crate::steam_app_state`]) carry
/// an update bit ([`crate::STEAM_UPDATE_STATES`]), `state` naming them. `game_dir` is the
/// caller's spelling ([`GameTarget::dir`]), never the folder resolved: a library's folder may be
/// a link to another disk, and resolved it would lie in no library (the reader tries both, the
/// caller's first). Nothing is refused for a game outside a Steam library, with no appid, or
/// whose manifest cannot be read: Steam's own state is not known then, and the folder's
/// anti-cheat is checked all the same.
fn refuse_update_pending(game_dir: &Path, appid: Option<u32>) -> Result<()> {
    let Some(appid) = appid else {
        return Ok(());
    };
    let Some(flags) = crate::game::steam_app_state(game_dir, appid) else {
        return Ok(());
    };
    let states = crate::game::steam_update_states(flags);
    if states.is_empty() {
        return Ok(());
    }
    Err(LauncherError::new(codes::GAME_UPDATE_PENDING)
        .with("appid", appid.to_string())
        .with("state", states.join(",")))
}

/// The most of a `[[config_writes]]` file Play reads before it rewrites it.
const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

/// [`ProcessPlan::warnings`]: a Steam user's own launch options for the game carry `--doorstop-…`
/// (read from `localconfig.vdf`, [`crate::steam_user_launch_options`]). Steam adds them to every
/// start, Astra's included, and Doorstop may take them over the ones Astra passes.
pub const STEAM_LAUNCH_OPTIONS_DOORSTOP: &str = "steam-launch-options-doorstop";

/// [`ProcessPlan::warnings`]: `mods-choice.json` could not be read (not JSON, too new, not a file)
/// and nothing could be played together anyway, so this Play is Astra's alone, as if the player
/// had not been asked yet. Never a refusal.
pub const MODS_CHOICE_UNREADABLE: &str = "mods-choice-unreadable";

/// [`ProcessPlan::warnings`]: a path in the player's pack's `doorstop_config.ini` was left out of
/// this Play with mods because of how it is written — a share (`\\host\…`, `//host/…`), a device
/// (`\\.\…`), a verbatim namespace — before anything touched it. The Play goes on without it.
pub const MODS_PACK_PATH_REFUSED: &str = "mods-pack-path-refused";

/// How Steam is started: `program prefix_args… -applaunch <appid> …`. A native Steam is its own
/// program with no prefix ([`SteamCommand::native`]); Flathub's Steam is
/// `flatpak run com.valvesoftware.Steam` ([`SteamCommand::flatpak`]), whose sandbox [`launch`]
/// grants the profile folder before it asks it to start the game (`crate::flatpak`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamCommand {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
    /// Steam is Flathub's Flatpak and `program` is the `flatpak` program.
    pub flatpak: bool,
}

impl SteamCommand {
    /// A Steam started as `program` itself (`steam`, `steam.exe`).
    pub fn native(program: impl Into<PathBuf>) -> SteamCommand {
        SteamCommand {
            program: program.into(),
            prefix_args: Vec::new(),
            flatpak: false,
        }
    }

    /// Flathub's Steam, started through the `flatpak` program `flatpak`:
    /// `flatpak run com.valvesoftware.Steam`.
    pub fn flatpak(flatpak: impl Into<PathBuf>) -> SteamCommand {
        SteamCommand {
            program: flatpak.into(),
            prefix_args: vec![
                "run".to_owned(),
                crate::flatpak::STEAM_FLATPAK_APP.to_owned(),
            ],
            flatpak: true,
        }
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

/// Runs a prepared command. Under the profile lock and immediately before the spawn it asks
/// again what may have changed since [`prepare_launch`] released the lock (the daemon starts its
/// bridge engine in between, for seconds): the anti-cheat gate with `engines` (the caller's set
/// now; only its markers are read), refusing `anti-cheat{kind}` as `prepare_launch` does, and
/// Steam's update state (`game-update-pending`). Then, for a Steam game, it writes
/// `<profile>/play-arm.json` (the module docs): a failed write fails the launch with nothing
/// started, and a failed spawn puts back what was there before, since an arm left by a launch
/// that never happened would vouch for the next plain start of the game. A refusal here starts
/// nothing and writes nothing.
///
/// What it cannot see: an update Steam has not discovered yet when it is asked to start the game
/// (docs/ENGINE-DESCRIPTORS-DEBT.md row 56).
///
/// **Flathub's Steam** ([`ProcessPlan::sandbox_grant`]): after those checks and right before the
/// spawn (before the play-arm), Steam's permissions are read and the profile's `BepInEx` folder
/// is granted to its sandbox in the user's Flatpak overrides ([`crate::flatpak`]'s rules,
/// `steam-flatpak-unusable` for every refusal). A plan whose `sandbox_grant` is not the folder
/// [`prepare_launch`] resolved is refused (`reason` = `path`); a plan read back (no gate) is
/// spawned as it is, granting nothing. [`launch_with`] names the overrides file.
pub fn launch(plan: &ProcessPlan, spawner: &dyn Spawn, engines: &EngineSet) -> Result<()> {
    launch_with(
        plan,
        spawner,
        engines,
        crate::flatpak::user_overrides_file().as_deref(),
    )
}

/// [`launch`], granting Flathub's Steam the profile in the Flatpak overrides file `overrides`
/// (the user's is [`crate::flatpak::user_overrides_file`]; `None`: there is none to write, and a
/// plan that needs a grant is refused, `steam-flatpak-unusable` with `reason` = `overrides`).
pub fn launch_with(
    plan: &ProcessPlan,
    spawner: &dyn Spawn,
    engines: &EngineSet,
    overrides: Option<&Path>,
) -> Result<()> {
    let Some(gate) = &plan.gate else {
        return spawner.spawn(plan);
    };
    let _lock = ProfileLock::acquire(&gate.profile)?;
    gate.refuse(engines)?;
    if let Some(stated) = &plan.sandbox_grant {
        let refused = |reason: &str| {
            LauncherError::new(codes::STEAM_FLATPAK_UNUSABLE)
                .with("reason", reason)
                .with("path", stated.display().to_string())
        };
        let Some(flatpak) = &gate.flatpak else {
            return Err(refused("path"));
        };
        if gate.grant.as_ref() != Some(stated) {
            return Err(refused("path"));
        }
        let Some(overrides) = overrides else {
            return Err(refused("overrides"));
        };
        crate::flatpak::grant_sandbox_access(&gate.profile, overrides, flatpak)?;
    }
    let Some(appid) = gate.arm else {
        return spawner.spawn(plan);
    };
    let path = gate.profile.join(PLAY_ARM_FILE);
    // A link or a folder there is not an arm of ours: never read through it, never replace it.
    check_regular_or_absent(&path)?;
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(LauncherError::io_at(&e, &path)),
    };
    write_play_arm(&path, appid)?;
    if let Err(e) = spawner.spawn(plan) {
        let _ = match previous {
            Some(bytes) => write_atomic(&path, &bytes),
            None => fs::remove_file(&path).map_err(|e| LauncherError::io_at(&e, &path)),
        };
        return Err(e);
    }
    Ok(())
}

/// A play-arm written before `decided_unix_ms` (ms since the UNIX epoch): the crash guard's
/// compare-and-delete. Under the profile lock (so no [`launch`] writes a fresh arm in between) it
/// reads `<profile>/play-arm.json` without following a link and without waiting on a FIFO, and
/// deletes it only when its `started_unix_ms` is older than `decided_unix_ms`. A link in its place
/// (on Windows a directory symlink or a junction too) is removed as a link, never followed: it is
/// no arm of ours.
///
/// The contract: `Ok(true)` when something was removed; `Ok(false)` for no arm (or one gone by the
/// time it is removed), a newer one, one that cannot be parsed or says no `started_unix_ms` (it
/// stays: it arms nothing the runner would take), and anything that is not a regular file (a
/// folder, a FIFO), which is left alone. `Err` when the arm is there and cannot be read for any
/// other reason (permissions): never a silent `false` for that.
pub fn remove_play_arm_if_older(profile_dir: &Path, decided_unix_ms: u64) -> Result<bool> {
    let profile = match canonical_dir(profile_dir) {
        Ok(p) => p,
        Err(_) if fs::symlink_metadata(profile_dir).is_err() => return Ok(false),
        Err(e) => return Err(e),
    };
    let _lock = ProfileLock::acquire(&profile)?;
    let path = profile.join(PLAY_ARM_FILE);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(LauncherError::io_at(&e, &path)),
        Ok(m) if m.file_type().is_symlink() => return remove_gone_ok(&path, true),
        Ok(m) if !m.is_file() => return Ok(false),
        Ok(_) => {}
    }
    let Some(text) = read_arm_no_follow(&path)? else {
        return Ok(false);
    };
    #[derive(Deserialize)]
    struct Started {
        started_unix_ms: u64,
    }
    let Ok(Started { started_unix_ms }) = serde_json::from_slice::<Started>(&text) else {
        return Ok(false);
    };
    if started_unix_ms >= decided_unix_ms {
        return Ok(false);
    }
    remove_gone_ok(&path, false)
}

/// Removes the arm at `path` (`link`: a link found in its place, removed as a link: on Windows a
/// directory link or junction with `remove_dir`, which takes the link and never what it points
/// at). `Ok(false)` when it is gone already.
fn remove_gone_ok(path: &Path, link: bool) -> Result<bool> {
    #[cfg(windows)]
    let dir_link = link
        && fs::symlink_metadata(path).is_ok_and(|m| {
            use std::os::windows::fs::FileTypeExt;
            m.file_type().is_symlink_dir()
        });
    #[cfg(not(windows))]
    let dir_link = {
        let _ = link;
        false
    };
    let removed = if dir_link {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    match removed {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(LauncherError::io_at(&e, path)),
    }
}

/// The bytes of the regular file at `path`, opened without following a link and without waiting
/// on a FIFO (it may have been swapped since it was looked at); `None` when it is not a regular
/// file, a link, or more than any arm.
fn read_arm_no_follow(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: a link (reparse point) is opened itself, not followed.
        opts.custom_flags(0x0020_0000);
    }
    let f = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        // A link put in its place since it was looked at (O_NOFOLLOW: ELOOP): no arm of ours.
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
        // Anything else (permissions, …) is said, never taken for "no arm".
        Err(e) => return Err(LauncherError::io_at(&e, path)),
    };
    if !f.metadata().is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    f.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| LauncherError::io_at(&e, path))?;
    Ok((bytes.len() <= 64 * 1024).then_some(bytes))
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

/// Prepares a launch: checks the install (`needs-reinstall`, `reason` = `incomplete` for an
/// install cut off, `restored` after [`crate::restore_game_files`]), writes `[[config_writes]]`
/// (into the profile only; an entry that sets a caller's variable is mode 0600 on Unix),
/// applies the Proton override (`proton-prefix-missing` when the prefix does not exist yet) and
/// returns the command — `<steam> <prefix…> -applaunch <appid> <args…>` for a Steam game
/// ([`SteamCommand`]; `steam-not-found` without one), otherwise the exe itself with its args and
/// env. A Steam game's plan carries the play-arm [`launch`] writes; this writes none. With
/// Flathub's Steam ([`SteamCommand::flatpak`]) the plan's [`ProcessPlan::sandbox_grant`] names
/// the folder [`launch`] grants Steam's sandbox, `<profile>/BepInEx`; this grants nothing, and
/// refuses a plan that hands the game a profile path outside it (`steam-flatpak-unusable`,
/// `reason` = `layout`, `path`) before anything is written.
///
/// **With the player's own mods** (`[integration.mods]`, `mods-choice.json`; the `mods` module
/// docs): the player's answer is read here. Not asked yet while mods are found that could be
/// played together: `mods-choice-needed`. "With": Doorstop starts the integration's chain loader
/// on THEIR BepInEx, with their pack's Doorstop settings, for the generation of the proxy beside
/// the game now; everything is checked first (`mods-source-missing`, `mods-loader-unsupported`,
/// `mods-melonloader`, `mods-unsupported`), and `chain.ini` is written into the profile last,
/// once nothing can refuse this Play any more. Nothing of theirs is written. "Without", or an
/// integration without the table: exactly the Play of 0.3.2, argument for argument. The mod
/// managers' folders looked in are this process's ([`ModsEnv::from_process`]).
///
/// **Anti-cheat, at every Play as at install** (0.5.2): once the install is known to be this
/// game's, `anti-cheat{kind}` exactly as planning refuses it (`plan::refuse_anti_cheat`, the
/// one gate both ask): a folder signature, `listed-anti-cheat` for a marker `engines` carries
/// (the signed catalogue's `anticheat[]` now, [`EngineSet::with_anti_cheat_markers`]; only its
/// markers are read) and `kernel-anti-cheat` for a kernel driver installed from the game's
/// folder (Windows). A game that gained an anti-cheat after it was installed — an update added
/// one, a driver was installed — is never started with the integration. For a Steam app the
/// programs Steam's launch entries name are held to the same signatures and markers, so an
/// update that points the game at `start_protected_game.exe` is refused before its files land.
/// VAC is checked again too (`valve-anti-cheat`; `vac-unchecked` when Steam's flag cannot be
/// read now and the person did not accept that at install). Nothing is written before either
/// refusal.
///
/// **An update Steam has not finished** (0.5.2): Steam installs a pending update before it starts
/// the game, so the folder just checked would not be the one that starts, and the update may add
/// an anti-cheat. `game-update-pending` (`appid`, `state`) when the app's `StateFlags`
/// ([`crate::steam_app_state`]) carry an update bit ([`crate::STEAM_UPDATE_STATES`]): an update
/// required, running, paused or started, a reconfiguration, a download, staging or committing.
/// Right after the anti-cheat gate, before anything about the install itself. [`launch`] asks the
/// anti-cheat gate and this again, under the lock, right before the spawn.
pub fn prepare_launch(
    profile_dir: &Path,
    game: &GameTarget,
    vars: &BTreeMap<String, String>,
    steam: Option<&SteamCommand>,
    engines: &EngineSet,
) -> Result<ProcessPlan> {
    prepare_launch_with(
        profile_dir,
        game,
        vars,
        steam,
        engines,
        &ModsEnv::from_process(),
    )
}

/// [`prepare_launch`], looking for the player's mods in the mod managers' folders `mods_env` names.
pub fn prepare_launch_with(
    profile_dir: &Path,
    game: &GameTarget,
    vars: &BTreeMap<String, String>,
    steam: Option<&SteamCommand>,
    engines: &EngineSet,
    mods_env: &ModsEnv,
) -> Result<ProcessPlan> {
    let profile =
        canonical_dir(profile_dir).map_err(|_| LauncherError::new(codes::NOT_INSTALLED))?;
    let _lock = ProfileLock::acquire(&profile)?;
    let Some(mut ledger) = ledger::read(&profile)? else {
        return Err(LauncherError::new(codes::NOT_INSTALLED));
    };
    match ledger.state {
        LedgerState::Installed => {}
        LedgerState::Installing => {
            return Err(needs_reinstall("").with("reason", "incomplete"));
        }
        // Its game files were put back (`crate::restore_game_files`, a withdrawn integration):
        // only installing again plays it.
        LedgerState::Restored => return Err(needs_reinstall("").with("reason", "restored")),
    }
    let m = Manifest::parse(&ledger.manifest)?;

    // This profile's install is THIS game: the folder and the program the ledger recorded.
    let game_dir = canonical_dir(&game.dir)?;
    let dir = plain(&game_dir);
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

    // The appid this game is known to be: the caller's, or the one its folder proves.
    let trusted = crate::plan::trusted_appid(game)?;
    // The anti-cheat this game carries NOW, as install asks it (the one gate both share): an
    // update may have added one since, installed a kernel driver, or pointed Steam's launch
    // entry at an anti-cheat's bootstrapper before its files land. Before anything about the
    // install itself, so the person is told why it may never start rather than sent to Install.
    refuse_anti_cheat(game, &game_dir, trusted, engines)?;
    // An update Steam knows of and has not finished: Steam would install it before it starts the
    // game, so what was just checked would not be what starts. `launch` asks both again.
    refuse_update_pending(&game.dir, trusted)?;

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
    // The player's answer about their own mods, and everything a Play with them needs, read now
    // (reads only); refused, if at all, once the install itself is known to be whole.
    let mods = crate::mods::play::resolve_play(&profile, &m, &ledger, game, mods_env);
    let with_mods = mods.as_ref().ok().and_then(|r| r.with.as_ref());
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
        // modded start), the settings ReShade saves into `ReShade.ini`, a name the manifest's own
        // `[integration.loader] keep_if_present` lists (what its loader saves into): theirs now,
        // as one that was there before us would be — the names planning keeps. Never rewritten.
        // Only a Doorstop config changes the session, through `foreign_config`, which reads that
        // file alone.
        if matches!(g.action, GameAction::Add | GameAction::ReuseKnownProxy)
            && LoaderPolicy::of(&m).keep(&g.rel)
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
        // With mods, Play passes its own options, for the proxy's own generation.
        let serves = path
            .as_deref()
            .and_then(doorstop::read_proxy)
            .is_some_and(|bytes| match with_mods {
                Some(w) => w.served_by(&bytes, exe_machine),
                None => doorstop::serves(&bytes, exe_machine, &m.launch.args),
            });
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
    // A game under the caller's own Wine/Proton is started by that runner, never through Steam,
    // whatever appid the caller also knows it by: no Steam command, no play-arm, no grant.
    let wine = match &game.platform {
        Platform::LinuxWine { runner } => {
            // The integration's env is third-party text: only its loaders' own settings pass.
            if let Some(k) = m
                .launch
                .env
                .keys()
                .find(|k| !crate::wine::manifest_may_set(k))
            {
                return Err(LauncherError::new(codes::RUNNER_INVALID)
                    .with("reason", "manifest-env-key")
                    .with("key", k.clone()));
            }
            Some((runner, runner.resolve(&profile, &game_dir)?))
        }
        _ => None,
    };
    let start_appid = game.steam_appid.filter(|_| wine.is_none());
    if start_appid.is_some() && steam.is_none() {
        return Err(LauncherError::new(codes::STEAM_NOT_FOUND));
    }

    // Nothing has been written yet: the last refusals are about the player's mods.
    let resolved = mods?;
    let mods = resolved.with;

    let flavor = match game.platform {
        Platform::LinuxProton { .. } | Platform::LinuxWine { .. } => Flavor::Proton,
        _ => Flavor::Native,
    };
    let x = Expander {
        profile: path_text(&plain(&profile), flavor)?,
        game: path_text(&plain(&game_root), flavor)?,
        vars,
        backslashes: flavor == Flavor::Proton || matches!(game.platform, Platform::Windows),
    };
    // The manifest's arguments, then the Doorstop options that keep a `doorstop_config.ini` that
    // is not ours (a shared loader's, one kept beside our proxy) out of the session. With mods,
    // the manifest's own Doorstop options give way to this Play's ([`doorstop_with_mods`]).
    let foreign_config = foreign_config(&game_root, &game_files)?;
    let with_mods = match &mods {
        Some(w) => Some(doorstop_with_mods(
            w,
            &x,
            flavor,
            &game_root,
            &game_files,
            exe_machine,
        )?),
        None => None,
    };
    let (own, doorstop_opts) = match &with_mods {
        Some((opts, _)) => (without_doorstop_options(&m.launch.args), opts.clone()),
        None => (m.launch.args.clone(), Vec::new()),
    };
    let pins_for = if with_mods.is_some() {
        &doorstop_opts
    } else {
        &m.launch.args
    };
    let mut args = own
        .iter()
        .map(|a| x.expand(a))
        .collect::<Result<Vec<_>>>()?;
    args.extend(doorstop_opts.iter().cloned());
    for pin in doorstop::session_options(pins_for, &exe_file, foreign_config) {
        args.push(x.expand(&pin)?);
    }
    // Where `chain.ini` goes, resolved and checked now (a link or a folder in its place, or on
    // the way, is refused here): a refusal there comes before `[[config_writes]]` and the Proton
    // override have written anything. Its parent is the chain's own folder, already there, held
    // open from here to the write (`SafeDir`: a folder Flathub's Steam may write into).
    let chain_ini = match (&mods, &with_mods) {
        (Some(w), Some((_, text))) => {
            let jp = parse_jailed(&w.decl.chain_config)?;
            let (parents, name) = crate::safe_dir::split(&jp.rel)?;
            let dir = SafeDir::open(&profile, parents, true)?
                .ok_or_else(|| LauncherError::new(codes::PATH_ESCAPES))?;
            dir.check_regular_or_absent(name)?;
            Some((dir, name.to_owned(), text.clone()))
        }
        _ => None,
    };
    // Flathub's Steam is granted one folder of the profile, `BepInEx`, never the profile itself:
    // everything this plan hands the game from the profile must be in it. Refused before
    // `[[config_writes]]` and the Proton override write anything.
    let sandbox_grant = match steam {
        Some(s) if s.flatpak && start_appid.is_some() => sandbox_layout(
            &profile,
            &x.profile,
            &args,
            &m,
            chain_ini
                .as_ref()
                .map(|(dir, name, _)| dir.join(name))
                .as_deref(),
        )?,
        _ => None,
    };

    for c in &m.config_writes {
        let jp = parse_jailed(&c.file)?;
        // Read and written through its folder held open (`SafeDir`), never by path: a program in
        // Flathub's Steam's sandbox may swap the file or a folder above it for a link at any
        // moment, and neither is ever followed.
        let (parents, name) = crate::safe_dir::split(&jp.rel)?;
        let dir = SafeDir::open(&profile, parents, true)?
            .ok_or_else(|| LauncherError::new(codes::PATH_ESCAPES))?;
        let target = dir.join(name);
        let (old, was_private) = match dir.read(name, MAX_CONFIG_BYTES)? {
            Some(r) => (
                String::from_utf8(r.bytes).map_err(|_| {
                    LauncherError::io("not UTF-8").with("path", target.display().to_string())
                })?,
                r.private,
            ),
            None => (String::new(), true),
        };
        let keys = c
            .set
            .iter()
            .map(|(k, v)| Ok((k.clone(), x.expand(v)?)))
            .collect::<Result<Vec<_>>>()?;
        let new = ini::set_keys(&old, &c.section, &keys);
        // A value that carries a caller's variable (a bridge credential) is the user's alone: mode
        // 0600 on Unix, and a file left unchanged is tightened too (`SafeDir::write_atomic`).
        let private = c.set.values().any(|v| !vars_in(v).is_empty());
        if (private && (new != old || !was_private)) || (!private && new != old) {
            dir.write_atomic(name, new.as_bytes(), private)?;
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

    let gate = |arm: Option<u32>, flatpak: Option<PathBuf>, grant: Option<PathBuf>| Gate {
        profile: profile.clone(),
        game: game.clone(),
        game_dir: game_dir.clone(),
        trusted,
        arm,
        flatpak,
        grant,
        wine: wine.as_ref().map(|(_, at)| at.clone()),
    };
    let mut plan = match start_appid {
        Some(appid) => {
            let steam = steam.expect("checked above");
            let mut argv = steam.prefix_args.clone();
            argv.extend(["-applaunch".to_owned(), appid.to_string()]);
            argv.extend(args);
            ProcessPlan {
                program: steam.program.clone(),
                args: argv,
                env: Vec::new(),
                cwd: None,
                warnings: steam_warnings(game, appid),
                // Flathub's Steam sees `<profile>/BepInEx` only once `launch` has granted it.
                sandbox_grant: sandbox_grant.clone(),
                // Checked again, the folder granted to Flathub's Steam and the arm written, by
                // `launch`, right before Steam is asked to start the game.
                gate: Some(gate(
                    Some(appid),
                    steam.flatpak.then(|| steam.program.clone()),
                    sandbox_grant,
                )),
            }
        }
        None => {
            let env = m
                .launch
                .env
                .iter()
                .map(|(k, v)| Ok((k.clone(), x.expand(v)?)))
                .collect::<Result<Vec<_>>>()?;
            let exe = plain(&game_root.join(&exe_file));
            let (program, args, env) = match &wine {
                // The integration's Wine overrides ride `WINEDLLOVERRIDES`, with Doorstop's
                // `winhttp` when it loads the game's proxy (a Steam launch carries that in the
                // user's launch options, a Proton prefix in `user.reg`; neither is ours here).
                Some((runner, at)) => {
                    let mut overrides = m.proton_overrides();
                    if (through_doorstop || with_mods.is_some())
                        && !overrides
                            .iter()
                            .any(|(d, _)| d.eq_ignore_ascii_case("winhttp"))
                    {
                        overrides.push(("winhttp".to_owned(), "n,b".to_owned()));
                    }
                    runner.command(at, &exe, args, env, &overrides)
                }
                None => (exe, args, env),
            };
            ProcessPlan {
                program,
                args,
                env,
                cwd: Some(plain(&game_root)),
                warnings: Vec::new(),
                sandbox_grant: None,
                gate: Some(gate(None, None, None)),
            }
        }
    };

    if resolved.choice_unreadable.is_some() {
        plan.warnings.push(MODS_CHOICE_UNREADABLE.to_owned());
    }
    if mods.as_ref().is_some_and(|w| !w.refused.is_empty()) {
        plan.warnings.push(MODS_PACK_PATH_REFUSED.to_owned());
    }

    // Last, once nothing can refuse this Play any more: the chain's `chain.ini` (the one file a
    // Play with mods adds, in the profile), then what was rewritten is theirs now.
    if let Some((dir, name, text)) = &chain_ini {
        dir.write_atomic(name, text.as_bytes(), false)?;
    }
    if game_files != ledger.game_files {
        ledger.game_files = game_files;
        ledger::write(&profile, &ledger)?;
    }
    Ok(plan)
}

/// The folder Flathub's Steam is granted for this plan, `<profile>/BepInEx`
/// ([`crate::flatpak::SANDBOX_FOLDER`]), once every profile path the plan hands the game is in
/// it: each of `args` that names the profile (`profile_text`, as the game sees it), each
/// `[[config_writes]]` file, and a Play with mods' `chain.ini` (`chain_ini`). `None` when the
/// plan hands the game nothing of the profile. A path outside it is `steam-flatpak-unusable`
/// (`reason` = `layout`, `path`): the grant is never widened to the profile, whose root holds
/// the ledger and the backups.
fn sandbox_layout(
    profile: &Path,
    profile_text: &str,
    args: &[String],
    m: &Manifest,
    chain_ini: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let folder = crate::flatpak::SANDBOX_FOLDER;
    let outside = |path: &str| {
        LauncherError::new(codes::STEAM_FLATPAK_UNUSABLE)
            .with("reason", "layout")
            .with("path", path)
    };
    let sep = |c: Option<char>| matches!(c, Some('/' | '\\'));
    let mut named = false;
    // As the game is handed it: a path argument gets backslashes on Windows and under Proton.
    let back = profile_text.replace('/', "\\");
    let spellings: &[&str] = if back == profile_text {
        &[profile_text]
    } else {
        &[profile_text, &back]
    };
    for arg in args {
        for (at, spelling) in spellings
            .iter()
            .flat_map(|sp| arg.match_indices(sp).map(move |(at, _)| (at, *sp)))
        {
            let rest = &arg[at + spelling.len()..];
            let mut chars = rest.chars();
            match chars.next() {
                // Another folder whose name begins with the profile's.
                Some(c) if !sep(Some(c)) => continue,
                // The profile itself, or a path in it.
                None => return Err(outside(arg)),
                Some(_) => {}
            }
            let below = chars.as_str();
            let inside = below
                .strip_prefix(folder)
                .is_some_and(|r| r.is_empty() || sep(r.chars().next()));
            if !inside {
                return Err(outside(arg));
            }
            named = true;
        }
    }
    for c in &m.config_writes {
        let jp = parse_jailed(&c.file)?;
        if jp.rel.len() < 2 || jp.rel[0] != folder {
            return Err(outside(&c.file));
        }
        named = true;
    }
    let granted = profile.join(folder);
    if let Some(chain) = chain_ini {
        if !chain.starts_with(&granted) || chain == granted {
            return Err(outside(&chain.display().to_string()));
        }
        named = true;
    }
    Ok(named.then(|| plain(&granted)))
}

/// A Play with mods' Doorstop options and its `chain.ini`, for the proxy beside the game now: the
/// first loader proxy of the install still there (ours, shared, or relinked by their manager).
/// One that cannot start this chain is `shared-loader-changed`, as for any Play.
fn doorstop_with_mods(
    w: &crate::mods::play::WithMods,
    x: &Expander,
    flavor: Flavor,
    game_root: &Path,
    game_files: &[LedgerGameFile],
    exe_machine: Option<u16>,
) -> Result<(Vec<String>, String)> {
    let proxies: Vec<&LedgerGameFile> = game_files
        .iter()
        .filter(|g| is_loader_proxy(&g.rel) && g.action != GameAction::KeepExisting)
        .collect();
    let changed = |rel: &str| LauncherError::new(codes::SHARED_LOADER_CHANGED).with("file", rel);
    let mut now = None;
    for g in &proxies {
        if let Some(p) = existing_target(game_root, &checked_rel(&g.rel)?)?
            && let Some(bytes) = doorstop::read_proxy(&p)
        {
            now = Some((g.rel.clone(), bytes));
            break;
        }
    }
    let first = proxies.first().map(|g| g.rel.as_str()).unwrap_or_default();
    let Some((rel, bytes)) = now else {
        return Err(changed(first));
    };
    let generation = match doorstop::generation(&bytes) {
        Some(g) if w.served_by(&bytes, exe_machine) => g,
        _ => return Err(changed(&rel)),
    };
    let text = |p: &GamePath| -> Result<String> {
        match p {
            GamePath::Local(p) => path_text(&plain(p), flavor),
            GamePath::Verbatim(s) => Ok(s.clone()),
        }
    };
    let chain = x.expand(&w.decl.chain)?;
    let opts = w
        .doorstop_args(generation, &chain, &text)?
        .ok_or_else(|| changed(&rel))?;
    let chain_ini = w.chain_ini(&path_text(&plain(&w.target), flavor)?);
    Ok((opts, chain_ini))
}

/// [`STEAM_LAUNCH_OPTIONS_DOORSTOP`] when a Steam user's own launch options for `appid` pass
/// Doorstop options: every Steam install this machine has, the caller's first.
fn steam_warnings(game: &GameTarget, appid: u32) -> Vec<String> {
    let caller = game
        .steam_appinfo
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut doorstop = false;
    for root in caller
        .into_iter()
        .chain(crate::steam_appinfo::steam_roots())
    {
        let key = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        doorstop |= crate::steam_userconfig::steam_user_launch_options(&root, &[appid])
            .get(&appid)
            .is_some_and(|all| {
                all.iter()
                    .any(|o| crate::steam_userconfig::launch_options_pass_doorstop(o))
            });
    }
    if doorstop {
        vec![STEAM_LAUNCH_OPTIONS_DOORSTOP.to_owned()]
    } else {
        Vec::new()
    }
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

/// How to start Steam on this machine, if it can be found: [`find_steam`]'s native Steam first;
/// otherwise, on Linux, Flathub's Steam when a `flatpak` program — `/usr/bin/flatpak` first, then
/// the first on `PATH`; an executable file either way — says it is installed
/// (`flatpak info com.valvesoftware.Steam` exits 0; [`SteamCommand::flatpak`]).
pub fn find_steam_command() -> Option<SteamCommand> {
    #[cfg(windows)]
    {
        find_steam().map(SteamCommand::native)
    }
    #[cfg(not(windows))]
    {
        find_steam_command_on(
            std::env::var_os("PATH").as_deref(),
            &[Path::new("/usr/bin/flatpak")],
        )
    }
}

/// [`find_steam_command`] on Unix: a native `steam` in the folders of the `PATH` value `path`,
/// else the first executable of `flatpaks` (the system's own `flatpak`), else a `flatpak` on
/// `path`.
#[cfg(not(windows))]
fn find_steam_command_on(
    path: Option<&std::ffi::OsStr>,
    flatpaks: &[&Path],
) -> Option<SteamCommand> {
    if let Some(steam) = path.and_then(|p| on_path(p, "steam")) {
        return Some(SteamCommand::native(steam));
    }
    let flatpak = flatpaks
        .iter()
        .find(|p| is_executable(p))
        .map(|p| p.to_path_buf())
        .or_else(|| path.and_then(|p| on_path(p, "flatpak")))?;
    crate::flatpak::steam_flatpak_installed(&flatpak).then(|| SteamCommand::flatpak(flatpak))
}

/// The first executable file named `name` in the folders of the `PATH` value `path`.
#[cfg(not(windows))]
fn on_path(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/// Whether `p` is a file (a link to one counts) someone may execute.
#[cfg(not(windows))]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Where Steam's launcher is on this machine, if it can be found: a native Steam only
/// ([`find_steam_command`] also finds Flathub's).
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
        on_path(&std::env::var_os("PATH")?, "steam")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A native `steam` on `PATH` first; else Flathub's, when a `flatpak` on `PATH` says it is
    /// installed; else none.
    ///
    /// Mutation (watched red): `find_steam_command_on` returning the Flatpak without asking;
    /// ignoring the system's `flatpak`; `is_executable` without the mode bits.
    #[cfg(unix)]
    #[test]
    fn find_steam_command_prefers_native_then_an_installed_flatpak() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("agl-find-steam-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let script = |dir: &str, text: &str| -> PathBuf {
            let d = root.join(dir);
            fs::create_dir_all(&d).unwrap();
            let p = d.join("flatpak");
            fs::write(&p, text).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            d
        };
        let yes = script(
            "yes",
            "#!/bin/sh\n[ \"$#\" = 2 ] && [ \"$1\" = info ] && [ \"$2\" = com.valvesoftware.Steam ] && exit 0\nexit 1\n",
        );
        let no = script("no", "#!/bin/sh\nexit 1\n");
        let native = root.join("native");
        fs::create_dir_all(&native).unwrap();
        fs::write(native.join("steam"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(native.join("steam"), fs::Permissions::from_mode(0o755)).unwrap();
        let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();

        assert_eq!(
            find_steam_command_on(Some(&path(&[&root.join("empty"), &yes])), &[]),
            Some(SteamCommand::flatpak(yes.join("flatpak")))
        );
        let flat = SteamCommand::flatpak(yes.join("flatpak"));
        assert_eq!(flat.prefix_args, ["run", "com.valvesoftware.Steam"]);
        assert!(flat.flatpak);
        assert_eq!(find_steam_command_on(Some(&path(&[&no])), &[]), None);
        assert_eq!(
            find_steam_command_on(Some(&path(&[&yes, &native])), &[]),
            Some(SteamCommand::native(native.join("steam")))
        );
        assert_eq!(
            find_steam_command_on(Some(&path(&[&root.join("empty")])), &[]),
            None
        );
        assert_eq!(find_steam_command_on(None, &[]), None);
        // The system's own `flatpak` is asked before one on PATH: one that says no wins over a
        // PATH one that would say yes.
        let no_flatpak = no.join("flatpak");
        assert_eq!(
            find_steam_command_on(Some(&path(&[&yes])), &[no_flatpak.as_path()]),
            None
        );
        let yes_flatpak = yes.join("flatpak");
        assert_eq!(
            find_steam_command_on(
                None,
                &[root.join("absent").as_path(), yes_flatpak.as_path()]
            ),
            Some(SteamCommand::flatpak(yes.join("flatpak")))
        );
        // A file nobody may execute is no program, on PATH or not.
        fs::set_permissions(&yes_flatpak, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            find_steam_command_on(Some(&path(&[&yes])), &[yes_flatpak.as_path()]),
            None
        );
        fs::set_permissions(native.join("steam"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find_steam_command_on(Some(&path(&[&native])), &[]), None);
        let _ = fs::remove_dir_all(&root);
    }

    /// The crash guard's compare-and-delete waits for the profile lock: an arm `launch` is about to
    /// write (it holds the lock) is never judged half-way.
    #[test]
    fn removing_an_old_arm_waits_for_the_profile_lock() {
        let dir = std::env::temp_dir().join(format!("agl-arm-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let file = dir.join(PLAY_ARM_FILE);
        fs::write(&file, br#"{"schema":1,"appid":1,"started_unix_ms":5}"#).unwrap();
        let lock = ProfileLock::acquire(&dir).unwrap();
        let d = dir.clone();
        let t = std::thread::spawn(move || remove_play_arm_if_older(&d, 10).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(file.exists(), "removed while the lock was held");
        drop(lock);
        assert!(t.join().unwrap());
        assert!(!file.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
