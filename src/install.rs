// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Install and uninstall. Install does exactly what the plan says or nothing: it plans again
//! under the profile lock and refuses with `plan-stale` when the result differs, writes the
//! ledger before placing the first file, and rolls back what it placed when it fails.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::digest::sha256_hex;
use crate::error::{LauncherError, Result, codes};
use crate::game::GameTarget;
use crate::ledger::{
    self, LEDGER_SCHEMA, Ledger, LedgerBackup, LedgerFile, LedgerGame, LedgerGameFile,
    LedgerSource, LedgerState, ProfileLock,
};
use crate::manifest::IntegrationInfo;
use crate::package::GiPackage;
use crate::paths::{
    BACKUP_DIR, FileState, LOCK_FILE, canonical_dir, checked_rel, existing_target, file_state,
    plain, prepare_target, prune_empty_dirs, write_atomic,
};
use crate::plan::{GameAction, InstallPlan, plan_install};
use crate::proton;
use crate::running::refuse_if_running;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReport {
    pub integration: IntegrationInfo,
    /// Game files we had added, now removed.
    pub removed: Vec<String>,
    /// Game files we had replaced, now restored from the backup.
    pub restored: Vec<String>,
    /// Game files that changed since we placed them: they are the game's now and were left.
    pub kept: Vec<String>,
    pub profile_files_removed: u32,
    pub proton_restored: bool,
}

fn stale(detail: &str) -> LauncherError {
    LauncherError::new(codes::PLAN_STALE).with("detail", detail)
}

fn backup_rel(rel: &str) -> String {
    format!("{BACKUP_DIR}/game/{rel}")
}

/// Installs `plan`. The plan must be what [`plan_install`] returns for this package, game and
/// profile right now; a changed game folder or profile gives `plan-stale`.
pub fn install(
    pkg: &GiPackage,
    plan: &InstallPlan,
    game: &GameTarget,
    ctx: &Ctx,
) -> Result<Ledger> {
    ctx.check()?;
    fs::create_dir_all(&plan.profile_dir)
        .map_err(|e| LauncherError::io_at(&e, &plan.profile_dir))?;
    let profile = canonical_dir(&plan.profile_dir)?;
    let _lock = ProfileLock::acquire(&profile)?;

    let fresh = plan_install(pkg, game, &plan.profile_dir)?;
    if fresh != *plan {
        return Err(stale("plan"));
    }
    refuse_if_running(&game.exe_file_name()?)?;
    let game_root = game.game_root()?;

    if let Some(old) = ledger::read(&profile)? {
        uninstall_ledger(&profile, &game_root, &old)?;
        let again = plan_install(pkg, game, &plan.profile_dir)?;
        if again.game_files != plan.game_files {
            return Err(stale("after-removing-the-previous-integration"));
        }
    }

    let mut ledger = Ledger {
        schema: LEDGER_SCHEMA,
        state: LedgerState::Installing,
        integration: pkg.manifest().integration.clone(),
        source: LedgerSource {
            sha256: pkg.sha256().to_owned(),
        },
        game: LedgerGame {
            dir: plain(&canonical_dir(&game.dir)?),
            exe: game.exe_rel()?,
        },
        manifest: pkg.manifest_text().to_owned(),
        files: pkg
            .profile_files
            .iter()
            .map(|f| LedgerFile {
                rel: f.rel_string(),
                sha256: f.sha256.clone(),
            })
            .collect(),
        game_files: plan
            .game_files
            .iter()
            .map(|a| LedgerGameFile {
                rel: a.rel_path.clone(),
                sha256: match a.action {
                    GameAction::KeepExisting => a.existing_sha256.clone().unwrap_or_default(),
                    _ => a.sha256.clone(),
                },
                action: a.action,
                backup: (a.action == GameAction::Replace).then(|| LedgerBackup {
                    rel: backup_rel(&a.rel_path),
                    sha256: a.existing_sha256.clone().unwrap_or_default(),
                }),
            })
            .collect(),
        proton: None,
    };
    ledger::write(&profile, &ledger)?;

    match place(pkg, plan, &profile, &game_root, ctx) {
        Ok(()) => {
            ledger.state = LedgerState::Installed;
            ledger::write(&profile, &ledger)?;
            Ok(ledger)
        }
        Err(e) => {
            let _ = uninstall_ledger(&profile, &game_root, &ledger);
            Err(e)
        }
    }
}

fn place(
    pkg: &GiPackage,
    plan: &InstallPlan,
    profile: &Path,
    game_root: &Path,
    ctx: &Ctx,
) -> Result<()> {
    let mut ar = pkg.archive()?;
    let total = pkg.profile_files.len() as u64;
    for (i, f) in pkg.profile_files.iter().enumerate() {
        ctx.check()?;
        let bytes = pkg.read(&mut ar, f)?;
        let target = prepare_target(profile, &f.rel)?;
        write_atomic(&target, &bytes)?;
        ctx.report("profile-files", i as u64 + 1, total);
    }

    let total = pkg.game_files.len() as u64;
    for (i, (f, a)) in pkg.game_files.iter().zip(&plan.game_files).enumerate() {
        ctx.check()?;
        if !a.rel_path.eq_ignore_ascii_case(&f.rel_string()) || a.sha256 != f.sha256 {
            return Err(stale("game-files"));
        }
        // The plan's spelling: an existing case variant is the file Windows and Wine would load.
        let target = prepare_target(game_root, &checked_rel(&a.rel_path)?)?;
        let current = file_state(&target)?;
        match a.action {
            GameAction::Add => {
                if current != FileState::Absent {
                    return Err(stale(&a.rel_path));
                }
                write_atomic(&target, &pkg.read(&mut ar, f)?)?;
            }
            GameAction::Replace => {
                let want = a.existing_sha256.clone().unwrap_or_default();
                if current
                    != (FileState::File {
                        sha256: want.clone(),
                    })
                {
                    return Err(stale(&a.rel_path));
                }
                let original = fs::read(&target).map_err(|e| LauncherError::io_at(&e, &target))?;
                if sha256_hex(&original) != want {
                    return Err(stale(&a.rel_path));
                }
                let backup = prepare_target(profile, &checked_rel(&backup_rel(&a.rel_path))?)?;
                write_atomic(&backup, &original)?;
                write_atomic(&target, &pkg.read(&mut ar, f)?)?;
            }
            GameAction::ReuseKnownProxy => {
                if current
                    != (FileState::File {
                        sha256: f.sha256.clone(),
                    })
                {
                    return Err(stale(&a.rel_path));
                }
            }
            GameAction::KeepExisting => {
                if current
                    != (FileState::File {
                        sha256: a.existing_sha256.clone().unwrap_or_default(),
                    })
                {
                    return Err(stale(&a.rel_path));
                }
            }
        }
        ctx.report("game-files", i as u64 + 1, total);
    }
    Ok(())
}

/// Undoes what a ledger records and removes the ledger. A game file is removed or restored only
/// while it still has our digest; one that changed since is the game's, and it wins.
pub(crate) fn uninstall_ledger(
    profile: &Path,
    game_root: &Path,
    ledger: &Ledger,
) -> Result<UninstallReport> {
    let mut report = UninstallReport {
        integration: ledger.integration.clone(),
        removed: Vec::new(),
        restored: Vec::new(),
        kept: Vec::new(),
        profile_files_removed: 0,
        proton_restored: false,
    };

    for g in ledger.game_files.iter().rev() {
        let comps = checked_rel(&g.rel)?;
        // A link where our file was is never followed: that file is not ours to touch now.
        let found = existing_target(game_root, &comps).and_then(|t| match t {
            Some(t) => file_state(&t).map(|s| Some((t, s))),
            None => Ok(None),
        });
        let (target, current) = match found {
            Ok(Some(x)) => x,
            Ok(None) => continue,
            Err(e) if e.code == codes::PATH_ESCAPES => {
                report.kept.push(g.rel.clone());
                continue;
            }
            Err(e) => return Err(e),
        };
        match g.action {
            GameAction::Add => match current {
                FileState::File { sha256 } if sha256 == g.sha256 => {
                    fs::remove_file(&target).map_err(|e| LauncherError::io_at(&e, &target))?;
                    report.removed.push(g.rel.clone());
                }
                FileState::File { .. } => report.kept.push(g.rel.clone()),
                FileState::Absent => {}
            },
            GameAction::Replace => {
                let backup = g.backup.as_ref();
                match current {
                    FileState::File { sha256 } if sha256 == g.sha256 => {
                        let restored = match backup {
                            Some(b) => restore_backup(profile, b, &target)?,
                            None => false,
                        };
                        if restored {
                            report.restored.push(g.rel.clone());
                        } else {
                            report.kept.push(g.rel.clone());
                        }
                    }
                    // The replace never happened (an install cut off before it).
                    FileState::File { sha256 } if backup.is_some_and(|b| b.sha256 == sha256) => {}
                    FileState::File { .. } => report.kept.push(g.rel.clone()),
                    FileState::Absent => {}
                }
            }
            GameAction::ReuseKnownProxy | GameAction::KeepExisting => {}
        }
    }

    if let Some(rec) = &ledger.proton {
        report.proton_restored = proton::restore_prefix(rec)?;
    }

    for f in &ledger.files {
        let comps = checked_rel(&f.rel)?;
        let found = match existing_target(profile, &comps) {
            Ok(p) => p,
            // Never delete through a link.
            Err(e) if e.code == codes::PATH_ESCAPES => None,
            Err(e) => return Err(e),
        };
        if let Some(p) = found {
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_file() || m.file_type().is_symlink() => {
                    fs::remove_file(&p).map_err(|e| LauncherError::io_at(&e, &p))?;
                    report.profile_files_removed += 1;
                }
                _ => {}
            }
            if let Some(parent) = p.parent() {
                prune_empty_dirs(parent.to_path_buf(), profile);
            }
        }
    }

    remove_tree_no_follow(&profile.join(BACKUP_DIR))?;
    ledger::remove(profile)?;
    Ok(report)
}

/// Copies the backup back over `target` when the backup still has its recorded digest.
fn restore_backup(profile: &Path, b: &LedgerBackup, target: &Path) -> Result<bool> {
    let comps = checked_rel(&b.rel)?;
    let Some(bp) = existing_target(profile, &comps)? else {
        return Ok(false);
    };
    if file_state(&bp)?
        != (FileState::File {
            sha256: b.sha256.clone(),
        })
    {
        return Ok(false);
    }
    let bytes = fs::read(&bp).map_err(|e| LauncherError::io_at(&e, &bp))?;
    if sha256_hex(&bytes) != b.sha256 {
        return Ok(false);
    }
    write_atomic(target, &bytes)?;
    Ok(true)
}

/// Removes a folder of ours. A link in its place is removed as a link, never followed.
fn remove_tree_no_follow(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p).map_err(|e| LauncherError::io_at(&e, p)),
        Ok(_) => fs::remove_file(p).map_err(|e| LauncherError::io_at(&e, p)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LauncherError::io_at(&e, p)),
    }
}

/// `${game}` for an uninstall: the exe's folder, even when the exe itself is gone.
fn game_root_for_uninstall(game: &GameTarget) -> Result<PathBuf> {
    match game.game_root() {
        Ok(r) => Ok(r),
        Err(e) if e.code == codes::EXE_NOT_FOUND => {
            let comps = game.exe_components()?;
            let mut p = game.dir.clone();
            p.extend(&comps[..comps.len().saturating_sub(1)]);
            canonical_dir(&p)
        }
        Err(e) => Err(e),
    }
}

/// The exe folder the ledger recorded at install, when it still exists and lies inside the same
/// game folder `game` names; `None` otherwise.
fn recorded_game_root(game: &GameTarget, ledger: &Ledger) -> Option<PathBuf> {
    let current = canonical_dir(&game.dir).ok()?;
    let recorded = canonical_dir(&ledger.game.dir).ok()?;
    if recorded != current {
        return None;
    }
    let comps = checked_rel(&ledger.game.exe.replace('\\', "/")).ok()?;
    let mut p = current.clone();
    p.extend(&comps[..comps.len().saturating_sub(1)]);
    let root = canonical_dir(&p).ok()?;
    root.starts_with(&current).then_some(root)
}

/// Removes an installed integration: restores the game files, the Proton override and the
/// profile to what they were.
pub fn uninstall(profile_dir: &Path, game: &GameTarget) -> Result<UninstallReport> {
    let profile = match canonical_dir(profile_dir) {
        Ok(p) => p,
        Err(_) => return Err(LauncherError::new(codes::NOT_INSTALLED)),
    };
    let report = {
        let _lock = ProfileLock::acquire(&profile)?;
        let Some(ledger) = ledger::read(&profile)? else {
            return Err(LauncherError::new(codes::NOT_INSTALLED));
        };
        refuse_if_running(&game.exe_file_name()?)?;
        // The folder the files were PLACED in is the ledger's, not today's detection: an exe
        // detected differently now (a launcher fix, a game update) must not send uninstall to
        // another folder while ours stay behind. Trusted only inside the same game folder.
        let game_root = match recorded_game_root(game, &ledger) {
            Some(root) => root,
            None => game_root_for_uninstall(game)?,
        };
        uninstall_ledger(&profile, &game_root, &ledger)?
    };
    let _ = fs::remove_file(profile.join(LOCK_FILE));
    Ok(report)
}

/// The ledger of a profile, if anything is installed there.
pub fn installed(profile_dir: &Path) -> Result<Option<Ledger>> {
    match fs::symlink_metadata(profile_dir) {
        Ok(m) if m.is_dir() || m.file_type().is_symlink() => {
            ledger::read(&canonical_dir(profile_dir)?)
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LauncherError::io_at(&e, profile_dir)),
    }
}
