// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Install and uninstall. Install does exactly what the plan says or nothing: it plans again
//! under the profile lock and refuses with `plan-stale` when the result differs, writes the
//! ledger before placing the first file, and rolls back what it placed when it fails.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::digest::sha256_hex;
use crate::engines::EngineSet;
use crate::error::{LauncherError, Result, codes};
use crate::former;
use crate::game::GameTarget;
use crate::ledger::{
    self, Ledger, LedgerBackup, LedgerFile, LedgerGame, LedgerGameFile, LedgerSource, LedgerState,
    ProfileLock,
};
use crate::manifest::IntegrationInfo;
use crate::pack_inbox::{self, PackInboxPlan, PackInboxReport};
use crate::package::GiPackage;
use crate::paths::{
    BACKUP_DIR, FileState, LOCK_FILE, canonical_dir, checked_rel, existing_target, file_state,
    plain, prepare_target, prune_empty_dirs, write_atomic,
};
use crate::plan::{
    Accept, FormerFile, GameAction, GameFileAction, InstallPlan, Planning, kept_for_their_loader,
    loader_baselines, plan_install_with, proxy_folder_gained, proxy_folders,
};
use crate::proton;
use crate::running::refuse_if_running;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReport {
    pub integration: IntegrationInfo,
    /// Game files we had added, now removed.
    pub removed: Vec<String>,
    /// Game files we had replaced, now restored from the backup.
    pub restored: Vec<String>,
    /// Game files left in place: ones that changed since we placed them (the game's now), and a
    /// loader proxy we added that another loader now runs through (a BepInEx installed by hand
    /// over ours ships the very same `winhttp.dll`).
    pub kept: Vec<String>,
    pub profile_files_removed: u32,
    pub proton_restored: bool,
    /// Wine DLL overrides (`winhttp`) left in the Proton prefix because the proxy they load was
    /// kept for another loader, or is another mod manager's shared one (`share-loader`), which
    /// needs them to be loaded at all. Absent before 0.3.1.
    #[serde(default)]
    pub proton_overrides_kept: Vec<String>,
    /// Game files we had replaced whose backup is a file an older Astra left there (UE4SS's
    /// v0.2.1 `dwmapi.dll`, the 0.2.x ReShade, `astra.addon64`: `crate::former`) and not the
    /// player's: ours is removed (it is in `removed` too) and the backup is not put back. Absent
    /// before 0.4.0.
    #[serde(default)]
    pub not_restored: Vec<String>,
    /// Files an older Astra left beside the game's program that this ledger did not name, removed
    /// by `uninstall`. Absent before 0.4.0.
    #[serde(default)]
    pub former_removed: Vec<String>,
    /// Why a file in `kept` was left, where it is not that it changed: `folder-gained-files` for
    /// a hijack-named file we added whose folder holds a name now that was not there before Astra
    /// first added it, is not ours, and can carry a loader — a folder, a link, a program or a
    /// library; never a config, a log or a screenshot (someone else's loader may run through it;
    /// a game update that added a library there reads the same). Its Proton override stays too
    /// (`proton_overrides_kept`). Absent when empty, and before 0.5.0.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kept_reasons: BTreeMap<String, String>,
    /// `restore-partial` ([`codes::RESTORE_PARTIAL`]) when a game file is not as it was before
    /// Astra: one in `kept` (changed since we placed it, a proxy kept for another loader, one we
    /// replaced whose backup is missing or changed, a link or a folder in its place), or one we
    /// replaced that is gone (its backup goes back only over our file). The person is told to
    /// verify the game's files in Steam (marketplace plan A§4.4, M2). A backup that is an older
    /// Astra's own file (`not_restored`) is not one. Absent when empty, and before 0.5.0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// What was done in the desktop companion's inbox for the packs the install handed it
    /// (`${astra_pack_inbox}`): packs still waiting there removed, a tombstone written for each
    /// (the companion trashes the pack), or held for another profile's install of the same
    /// integration. Absent when the install handed it none, and before 0.5.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_inbox: Option<PackInboxReport>,
}

/// [`UninstallReport::kept_reasons`]: the folder of a file we added gained, since, a name that
/// can carry another loader.
pub const KEPT_FOLDER_GAINED_FILES: &str = "folder-gained-files";

fn stale(detail: &str) -> LauncherError {
    LauncherError::new(codes::PLAN_STALE).with("detail", detail)
}

fn backup_rel(rel: &str) -> String {
    format!("{BACKUP_DIR}/game/{rel}")
}

/// Installs `plan`. The plan must be what [`crate::plan_install_in`] (or
/// [`crate::plan_install_with_user_pack_root`]) returns for this package, game and profile right
/// now — planned again here with the plan's own provenance, the companion's user root it names
/// ([`InstallPlan::pack_inbox`]), and with the engine descriptor it pinned alone
/// ([`InstallPlan::engine_descriptor`], parsed again and its digest checked), so no plan is
/// installed against another descriptor than the one it was made with. A changed game folder,
/// profile or descriptor gives `plan-stale`. [`install_in`] with no anti-cheat markers.
pub fn install(
    pkg: &GiPackage,
    plan: &InstallPlan,
    game: &GameTarget,
    accept: Accept,
    ctx: &Ctx,
) -> Result<Ledger> {
    install_in(pkg, plan, game, accept, &EngineSet::empty(), ctx)
}

/// [`install`], planning again also with the anti-cheat markers `current` carries now
/// ([`EngineSet::anti_cheat_markers`]: the signed catalogue's `anticheat[]`, as the daemon hands
/// them to [`crate::plan_install_in`]), so a marker the catalogue added between the consent sheet
/// and the install still refuses it (`anti-cheat{kind=listed-anti-cheat}`). Only the markers are
/// read from `current`: the engine is checked with the pinned descriptor alone, as [`install`]
/// checks it.
pub fn install_in(
    pkg: &GiPackage,
    plan: &InstallPlan,
    game: &GameTarget,
    accept: Accept,
    current: &EngineSet,
    ctx: &Ctx,
) -> Result<Ledger> {
    ctx.check()?;
    // Unread VAC is never covered by a plain "yes": the caller says the person was told.
    if !plan.vac_checked && !accept.vac_unchecked {
        return Err(LauncherError::new(codes::VAC_UNCHECKED));
    }
    fs::create_dir_all(&plan.profile_dir)
        .map_err(|e| LauncherError::io_at(&e, &plan.profile_dir))?;
    let profile = canonical_dir(&plan.profile_dir)?;
    let _lock = ProfileLock::acquire(&profile)?;

    // The descriptor the plan pinned, alone; none for a built-in engine. A pinned descriptor
    // named the manifest's own code, as the player's pick or as the only one: so it is the pick.
    let engines = match &plan.engine_descriptor {
        Some(pinned) => pinned.engine_set()?,
        None => EngineSet::empty(),
    }
    .with_anti_cheat_markers(current.anti_cheat_markers().to_vec());
    let pick = plan
        .engine_descriptor
        .as_ref()
        .and(pkg.manifest().engine_code());
    // With the companion's user root the plan was made with, if it hands the companion packs.
    let how = Planning {
        engines: &engines,
        provenance: plan.provenance,
        engine_pick: pick.as_ref(),
        user_pack_root: plan.pack_inbox.as_ref().map(|p| p.user_root.as_path()),
    };
    let fresh = plan_install_with(pkg, game, &plan.profile_dir, how, None)?;
    if fresh != *plan {
        return Err(stale("plan"));
    }
    refuse_if_running(&game.exe_file_name()?)?;
    let game_root = game.game_root()?;

    let old = ledger::read(&profile)?;
    // The folder the replaced integration placed its files in, which is this plan's only when
    // the program is still where it was (a game update may have moved it: then the old one is
    // undone in its own folder, and nothing of it is this folder's).
    let old_root = old.as_ref().map(|l| placed_root(game, l)).transpose()?;
    let here = old
        .as_ref()
        .filter(|_| old_root.as_ref().is_none_or(|r| *r == game_root));
    // Measured before the replaced integration is undone, which may take files from that folder.
    let loader_baseline = loader_baselines(&game_root, &plan.game_files, here)?;
    let proxy_folders = proxy_folders(&game_root, &plan.game_files, pkg.manifest(), here)?;
    // The replaced integration's packs this plan hands the companion again: their tombstones are
    // held back below, and owed until this install's own ledger records the packs.
    let mut handed_over: Option<&Ledger> = None;
    if let (Some(old), Some(old_root)) = (&old, &old_root) {
        let readd: &[GameFileAction] = if here.is_some() {
            &plan.game_files
        } else {
            &[]
        };
        // A pack this plan hands the companion again is no tombstone (the engine replaces it by
        // its version, and a tombstone would take it off whoever wears it).
        uninstall_ledger(&profile, old_root, old, readd, plan.pack_inbox.as_ref())?;
        if old.pack_inbox.is_some() && plan.pack_inbox.is_some() {
            handed_over = Some(old);
        }
    }

    // From the old install's undoing to this one's ledger: a refusal here leaves neither ledger,
    // so the tombstones held back for the hand-over are written now, or nothing ever would.
    let written = (|| -> Result<Ledger> {
        if old.is_some() {
            ctx.check()?;
            // The game folder as the plan expected it once the old integration is gone. The
            // plan's refusals were judged with that integration in place, as the person will find
            // the game.
            let again = plan_install_with(pkg, game, &plan.profile_dir, how, here)?;
            if again.game_files != plan.game_files {
                return Err(stale("after-removing-the-previous-integration"));
            }
        }
        // What an older Astra left beside the program, and no ledger names: gone before ours
        // arrive.
        remove_former(&game_root, &plan.former_files)?;
        let ledger = new_ledger(pkg, plan, game, &accept, loader_baseline, proxy_folders)?;
        ledger::write(&profile, &ledger)?;
        Ok(ledger)
    })();
    let mut ledger = written.inspect_err(|_| {
        if let Some(old) = handed_over {
            let _ = pack_inbox::undo(&profile, old, None);
        }
    })?;

    match place(pkg, plan, &profile, &game_root, ctx) {
        Ok(()) => {
            ledger.state = LedgerState::Installed;
            ledger::write(&profile, &ledger)?;
            Ok(ledger)
        }
        Err(e) => {
            let _ = uninstall_ledger(&profile, &game_root, &ledger, &[], None);
            Err(e)
        }
    }
}

/// The ledger of `plan` as an install begins it (`Installing`).
fn new_ledger(
    pkg: &GiPackage,
    plan: &InstallPlan,
    game: &GameTarget,
    accept: &Accept,
    loader_baseline: Vec<crate::ledger::LoaderBaseline>,
    proxy_folders: Vec<crate::ledger::ProxyFolder>,
) -> Result<Ledger> {
    Ok(Ledger {
        // `ledger::write` stores the lowest schema that carries it; nothing is in `proton` yet.
        schema: 1,
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
                    GameAction::KeepExisting | GameAction::ShareLoader => {
                        a.existing_sha256.clone().unwrap_or_default()
                    }
                    _ => a.sha256.clone(),
                },
                action: a.action,
                backup: (a.action == GameAction::Replace).then(|| LedgerBackup {
                    rel: backup_rel(&a.rel_path),
                    sha256: a.existing_sha256.clone().unwrap_or_default(),
                }),
            })
            .collect(),
        proton: Vec::new(),
        vac_unchecked_accepted: !plan.vac_checked && accept.vac_unchecked,
        loader_baseline,
        provenance: plan.provenance,
        descriptor_sha256: plan.engine_descriptor.as_ref().map(|d| d.sha256.clone()),
        proxy_folders,
        pack_inbox: plan.pack_inbox.as_ref().map(pack_inbox::ledger_of),
    })
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

    if let Some(inbox) = &plan.pack_inbox {
        pack_inbox::place(pkg, inbox, profile, &mut ar, ctx)?;
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
                // Nothing there, or a file an older Astra left that ours takes the place of.
                let former = a.existing_sha256.as_ref().is_some_and(|sha256| {
                    current
                        == FileState::File {
                            sha256: sha256.clone(),
                        }
                });
                if current != FileState::Absent && !former {
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
            GameAction::KeepExisting | GameAction::ShareLoader => {
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

/// Undoes what a ledger records and removes the ledger: the game half ([`undo_game`]), then the
/// profile's files, the packs it handed the desktop companion ([`crate::pack_inbox`]: a tombstone
/// for each, but for one `again` — the plan of the update replacing it — hands over again), the
/// backups and the ledger itself. A rollback passes no `readd`, and keeps a
/// proxy exactly where an uninstall of the failed install would: an update carries the first
/// install's baselines, so what arrived since that install and can carry another loader (a
/// folder, a library; never the configs and logs our own loaders write, [`proxy_folder_gained`])
/// keeps it, as the uninstall it stands in for would.
pub(crate) fn uninstall_ledger(
    profile: &Path,
    game_root: &Path,
    ledger: &Ledger,
    readd: &[GameFileAction],
    again: Option<&PackInboxPlan>,
) -> Result<UninstallReport> {
    let mut report = undo_game(profile, game_root, ledger, readd)?;

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

    report.pack_inbox = pack_inbox::undo(profile, ledger, again)?;

    remove_tree_no_follow(&profile.join(BACKUP_DIR))?;
    ledger::remove(profile)?;
    Ok(report)
}

/// The game half of undoing a ledger — what [`uninstall`] and [`restore_game_files`] both do, and
/// install before an update: the game files and the Proton overrides, nothing in the profile but
/// reading the backups. The restore rules (marketplace plan A§4.4, M2): a game file is removed or
/// restored only while it still has our digest, and a backup goes back only over our file, and
/// only while the backup still has the digest recorded for it; anything else — a file that
/// changed since (the game's, and it wins), a link or a folder in our file's place (never
/// followed) — is left alone, and the report says `restore-partial`
/// ([`UninstallReport::warnings`]). An added Doorstop someone else's loader now runs through stays
/// too ([`kept_for_their_loader`]), and so does the Proton override that loads it — unless
/// `readd`, the plan of the integration replacing this one, adds it again right away. A shared
/// proxy (`share-loader`) still there keeps its override as well. So does a hijack-named file we
/// added whose folder gained, since, a name that can carry another loader
/// ([`proxy_folder_gained`], reported in `kept_reasons`).
fn undo_game(
    profile: &Path,
    game_root: &Path,
    ledger: &Ledger,
    readd: &[GameFileAction],
) -> Result<UninstallReport> {
    // Before anything is removed: a Wine still running in a prefix we wrote to would write its
    // registry back over the restore, and the uninstall must not stop half done.
    for rec in &ledger.proton {
        proton::refuse_if_wineserver(&rec.prefix)?;
    }
    let mut report = UninstallReport {
        integration: ledger.integration.clone(),
        removed: Vec::new(),
        restored: Vec::new(),
        kept: Vec::new(),
        profile_files_removed: 0,
        proton_restored: false,
        proton_overrides_kept: Vec::new(),
        not_restored: Vec::new(),
        former_removed: Vec::new(),
        kept_reasons: BTreeMap::new(),
        warnings: Vec::new(),
        pack_inbox: None,
    };
    let readded = |rel: &str| {
        readd
            .iter()
            .any(|a| a.action == GameAction::Add && a.rel_path.eq_ignore_ascii_case(rel))
    };
    // The DLLs (file stems) of proxies kept for another loader.
    let mut kept_dlls: Vec<String> = Vec::new();
    // A game file we had replaced is gone: the player's is not back (no backup goes back over
    // anything but our file), and `kept` cannot name what is not there.
    let mut replaced_gone = false;

    for g in ledger.game_files.iter().rev() {
        let comps = checked_rel(&g.rel)?;
        // A link where our file was is never followed: that file is not ours to touch now.
        let found = existing_target(game_root, &comps).and_then(|t| match t {
            Some(t) => file_state(&t).map(|s| Some((t, s))),
            None => Ok(None),
        });
        let (target, current) = match found {
            Ok(Some(x)) => x,
            // A folder on the way is gone (or is a file now): ours went with it.
            Ok(None) => {
                replaced_gone |= g.action == GameAction::Replace;
                continue;
            }
            Err(e) if e.code == codes::PATH_ESCAPES => {
                report.kept.push(g.rel.clone());
                continue;
            }
            Err(e) => return Err(e),
        };
        match g.action {
            GameAction::Add => match current {
                FileState::File { sha256 }
                    if sha256 == g.sha256
                        && !readded(&g.rel)
                        && kept_for_their_loader(game_root, &g.rel, &target, ledger)? =>
                {
                    report.kept.push(g.rel.clone());
                    if let Some(stem) = Path::new(&g.rel).file_stem() {
                        kept_dlls.push(stem.to_string_lossy().into_owned());
                    }
                }
                // Its folder gained a name since Astra first added it that can carry a loader:
                // someone else's loader may run through it now (theirs can be byte for byte ours).
                FileState::File { sha256 }
                    if sha256 == g.sha256
                        && !readded(&g.rel)
                        && proxy_folder_gained(game_root, &g.rel, ledger)? =>
                {
                    report.kept.push(g.rel.clone());
                    report
                        .kept_reasons
                        .insert(g.rel.clone(), KEPT_FOLDER_GAINED_FILES.to_owned());
                    if let Some(stem) = Path::new(&g.rel).file_stem() {
                        kept_dlls.push(stem.to_string_lossy().into_owned());
                    }
                }
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
                    // What we replaced was a file an older Astra left (a launcher before 0.4.0
                    // took it for the player's): never put back. Ours goes, as an `add` would.
                    FileState::File { sha256 }
                        if sha256 == g.sha256
                            && backup.is_some_and(|b| backup_is_former(game_root, &g.rel, b)) =>
                    {
                        fs::remove_file(&target).map_err(|e| LauncherError::io_at(&e, &target))?;
                        report.removed.push(g.rel.clone());
                        report.not_restored.push(g.rel.clone());
                    }
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
                    // Ours is gone, and what we replaced is not back — unless that was an older
                    // Astra's own file, which is never put back anyway.
                    FileState::Absent => {
                        replaced_gone |=
                            !backup.is_some_and(|b| backup_is_former(game_root, &g.rel, b));
                    }
                }
            }
            // A shared proxy stays where it is, and so does the override that loads it: under
            // Proton the other manager's loader needs it as ours did (one that relinked its proxy
            // over ours may have come to rely on the override we wrote).
            GameAction::ShareLoader => {
                if let (FileState::File { .. }, Some(stem)) =
                    (current, Path::new(&g.rel).file_stem())
                {
                    kept_dlls.push(stem.to_string_lossy().into_owned());
                }
            }
            GameAction::ReuseKnownProxy | GameAction::KeepExisting => {}
        }
    }

    // An override that loads a kept proxy stays: under Proton their loader needs it as ours did.
    let (left, restore): (Vec<_>, Vec<_>) = ledger
        .proton
        .iter()
        .cloned()
        .partition(|r| kept_dlls.iter().any(|d| d.eq_ignore_ascii_case(&r.dll)));
    report.proton_restored = proton::restore_prefix(&restore)?;
    for r in left {
        if !report.proton_overrides_kept.contains(&r.dll) {
            report.proton_overrides_kept.push(r.dll);
        }
    }

    // Every file left in place is not as it was before Astra, whatever kept it.
    if !report.kept.is_empty() || replaced_gone {
        report.warnings.push(codes::RESTORE_PARTIAL.to_owned());
    }
    Ok(report)
}

/// Whether the backup `b` of the game file `rel` holds a file an older Astra left there rather than
/// the player's (`crate::former`'s rules exactly, beside the file as the game folder is now).
fn backup_is_former(game_root: &Path, rel: &str, b: &LedgerBackup) -> bool {
    let Ok(mut comps) = checked_rel(rel) else {
        return false;
    };
    let Some(name) = comps.pop() else {
        return false;
    };
    let dir = comps.iter().fold(game_root.to_path_buf(), |p, c| p.join(c));
    former::former_kind(&dir, &name, &b.sha256).is_some()
}

/// Removes, beside the game's program, what an older Astra left there and the plan lists — each
/// hashed again right before, and removed only while it is exactly what the plan showed
/// (`plan-stale` otherwise). Never backed up: Astra's own, never the player's.
fn remove_former(game_root: &Path, former: &[FormerFile]) -> Result<()> {
    for f in former {
        match former::remove_exact(game_root, &f.rel_path, f.kind, &f.sha256) {
            Ok(former::Removal::Removed | former::Removal::Absent) => {}
            Ok(former::Removal::Changed) => return Err(stale(&f.rel_path)),
            Err(e) => return Err(LauncherError::io_at(&e, &game_root.join(&f.rel_path))),
        }
    }
    Ok(())
}

/// Removes what an older Astra left beside the game's program that `ledger` does not name (the
/// ledger governs its own files), for an uninstall: each hashed again right before, and removed
/// only while it is exactly what was judged. Best effort: the integration is already gone, and a
/// file that cannot be removed is only not reported.
fn sweep_former(game_root: &Path, ledger: &Ledger) -> Vec<String> {
    let names = |rel: &str, name: &str| {
        let (rel, name) = (rel.to_ascii_lowercase(), name.to_ascii_lowercase());
        rel == name || rel.starts_with(&format!("{name}/"))
    };
    let mut removed = Vec::new();
    for f in former::former_set(game_root) {
        if ledger.game_files.iter().any(|g| names(&g.rel, &f.name)) {
            continue;
        }
        if let Ok(former::Removal::Removed) =
            former::remove_exact(game_root, &f.name, f.kind, &f.sha256)
        {
            removed.push(f.name);
        }
    }
    removed
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

/// The folder `ledger`'s game files were PLACED in: the ledger's, not today's detection. An exe
/// detected differently now (a launcher fix, a game update) must not send an uninstall, a
/// restore or an update's undo to another folder while ours stay behind. Trusted only inside the
/// same game folder.
pub(crate) fn placed_root(game: &GameTarget, ledger: &Ledger) -> Result<PathBuf> {
    match recorded_game_root(game, ledger) {
        Some(root) => Ok(root),
        None => game_root_for_uninstall(game),
    }
}

/// Whether `ledger`'s game files are in `game_root`, the folder of the program `game` names
/// today ([`placed_root`]): only then does planning read them there, as the files it replaces.
pub(crate) fn placed_in(game: &GameTarget, ledger: &Ledger, game_root: &Path) -> Result<bool> {
    Ok(placed_root(game, ledger)? == game_root)
}

/// Removes an installed integration: restores the game files, the Proton override and the
/// profile to what they were. Over a [`LedgerState::Restored`] ledger (whose game files went back
/// already) it removes the profile's files and the ledger.
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
        let game_root = placed_root(game, &ledger)?;
        let mut report = uninstall_ledger(&profile, &game_root, &ledger, &[], None)?;
        report.former_removed = sweep_former(&game_root, &ledger);
        // The player's answer to "play with your mods?" goes with the install, so a reinstall asks
        // again. An update keeps it (it never comes through here). Best effort: the integration is
        // gone already, and a choice left behind only means the next install does not ask.
        let _ = crate::mods::remove_choice(&profile);
        // And what Play with mods left in the profile: chain.ini, the chain's last-run.txt.
        crate::mods::play::remove_play_files(&profile, &ledger);
        report
    };
    let _ = fs::remove_file(profile.join(LOCK_FILE));
    Ok(report)
}

/// Puts the game's files back and keeps the profile: the restore for an integration that was
/// withdrawn (the daemon's `RestoreGameFiles` and its revocations; marketplace plan A§4.4).
///
/// Under the profile lock, and refused with `game-running` while the game runs: the caller waits
/// for the game to exit, and blocks Play meanwhile (M2). The game files and the Proton override are
/// undone exactly as [`uninstall`] undoes them — one function, the restore rules M2 sets
/// ([`UninstallReport::warnings`]: `restore-partial` for anything left as it is) — and so are the
/// files an older Astra left beside the program. The profile's own files stay (the mods choice
/// and Play's files too), and so does the backup of every file we replaced that could not be put
/// back — the game's or the player's own is there now, or nothing is (M2 puts a backup back only
/// over our file) — while it still holds what was backed up: the player's file (a mod loader of
/// theirs, step 10a) lasts as long as the profile does. Every other backup goes. The ledger is
/// then rewritten as [`LedgerState::Restored`] with no game files but those `replace`s whose
/// backup stays, no Proton records and no baselines: schema 4, which a launcher before 0.5.0
/// refuses (`schema-too-new`) rather than plays. After it, Play is `needs-reinstall` (`reason` =
/// `restored`), [`uninstall`] undoes what is left as the restore rules allow and removes the
/// profile's files and the ledger, and [`install`] is a fresh install. A restore over a restored
/// ledger changes nothing. `not-installed` when the profile holds no ledger.
pub fn restore_game_files(profile_dir: &Path, game: &GameTarget) -> Result<UninstallReport> {
    let profile = match canonical_dir(profile_dir) {
        Ok(p) => p,
        Err(_) => return Err(LauncherError::new(codes::NOT_INSTALLED)),
    };
    let _lock = ProfileLock::acquire(&profile)?;
    let Some(ledger) = ledger::read(&profile)? else {
        return Err(LauncherError::new(codes::NOT_INSTALLED));
    };
    refuse_if_running(&game.exe_file_name()?)?;
    let game_root = placed_root(game, &ledger)?;
    let mut report = undo_game(&profile, &game_root, &ledger, &[])?;
    report.former_removed = sweep_former(&game_root, &ledger);
    // A file we replaced whose original is not back (and is no older Astra's file): its backup
    // stays, recorded, while it still holds what was backed up.
    let mut held = Vec::new();
    for g in &ledger.game_files {
        let Some(b) = g
            .backup
            .as_ref()
            .filter(|_| g.action == GameAction::Replace)
        else {
            continue;
        };
        if report.restored.contains(&g.rel) || report.not_restored.contains(&g.rel) {
            continue;
        }
        let back = existing_target(&game_root, &checked_rel(&g.rel)?)
            .ok()
            .flatten()
            .and_then(|t| file_state(&t).ok())
            == Some(FileState::File {
                sha256: b.sha256.clone(),
            });
        let intact = existing_target(&profile, &checked_rel(&b.rel)?)
            .ok()
            .flatten()
            .and_then(|p| file_state(&p).ok())
            == Some(FileState::File {
                sha256: b.sha256.clone(),
            });
        if !back && intact {
            held.push(g.clone());
        }
    }
    let keep: Vec<String> = held
        .iter()
        .filter_map(|g| g.backup.as_ref().map(|b| b.rel.clone()))
        .collect();
    let restored = Ledger {
        state: LedgerState::Restored,
        game_files: held,
        proton: Vec::new(),
        loader_baseline: Vec::new(),
        proxy_folders: Vec::new(),
        ..ledger
    };
    ledger::write(&profile, &restored)?;
    remove_backups_but(&profile, &keep)?;
    Ok(report)
}

/// Removes the profile's backups but the files `keep` names (`/`-separated, below the profile),
/// never following a link; a folder left empty goes too.
fn remove_backups_but(profile: &Path, keep: &[String]) -> Result<()> {
    if keep.is_empty() {
        return remove_tree_no_follow(&profile.join(BACKUP_DIR));
    }
    fn walk(profile: &Path, dir: &Path, keep: &[String]) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(LauncherError::io_at(&e, dir)),
        };
        for e in entries {
            let e = e.map_err(|e| LauncherError::io_at(&e, dir))?;
            let p = e.path();
            let rel = p
                .strip_prefix(profile)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {
                    walk(profile, &p, keep)?;
                    let _ = fs::remove_dir(&p);
                }
                Ok(_) if keep.iter().any(|k| k.eq_ignore_ascii_case(&rel)) => {}
                Ok(_) => fs::remove_file(&p).map_err(|e| LauncherError::io_at(&e, &p))?,
                Err(e) => return Err(LauncherError::io_at(&e, &p)),
            }
        }
        Ok(())
    }
    let root = profile.join(BACKUP_DIR);
    match fs::symlink_metadata(&root) {
        Ok(m) if m.is_dir() => walk(profile, &root, keep),
        // A link or a file in its place: no backup of ours is behind it.
        Ok(_) => remove_tree_no_follow(&root),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LauncherError::io_at(&e, &root)),
    }
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
