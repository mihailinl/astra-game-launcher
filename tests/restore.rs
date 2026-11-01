// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Restores that never touch the player's files (marketplace plan A§4.4, M2): `uninstall` and
//! `restore_game_files` undo the game half through one function, which removes or restores a
//! game file only while it still has our digest and puts a backup back only over our file, and
//! only while the backup is what was recorded. Anything else is left exactly as it is, named in
//! `kept`, and the report warns `restore-partial`. `restore_game_files` keeps the profile and
//! leaves a `restored` ledger (schema 4): Play refuses it, uninstall removes the profile, an
//! install over it is a fresh one, and launchers 0.3.1 and 0.4.1 refuse to read it.

mod common;

use std::fs;
use std::path::PathBuf;

use astra_game_launcher::{
    Ctx, GameTarget, GiPackage, KEPT_FOLDER_GAINED_FILES, LEDGER_FILE_NAME, LedgerState, Platform,
    UninstallReport, codes, install, installed, plan_install, prepare_launch, restore_game_files,
    uninstall,
};
use common::*;

const GAME: &str = "Restore Game";
const ORIGINAL: &[u8] = b"{\"game\":\"original\"}";
const OURS: &[u8] = b"{\"ours\":true}";

/// The runner (an added `winhttp.dll` and `doorstop_config.ini`) plus `settings.json`, which
/// replaces the game's own: backed up into the profile, put back by an uninstall or a restore.
fn with_settings() -> GiPackage {
    let m = runner_manifest(
        "\n[[game_files]]\nfrom = \"extra/settings.json\"\nto = \"${game}/settings.json\"\n",
    );
    open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
        E::File("extra/settings.json", OURS),
    ]))
    .unwrap()
}

struct World {
    game: Tmp,
    profiles: Tmp,
    name: String,
}

impl World {
    fn new() -> World {
        World::named(GAME)
    }

    fn named(name: &str) -> World {
        let game = Tmp::new("restore-game");
        unity_game(game.path(), name);
        write(&game.join("settings.json"), ORIGINAL);
        World {
            game,
            profiles: Tmp::new("restore-profiles"),
            name: name.to_owned(),
        }
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> GameTarget {
        target(
            self.game.path(),
            &format!("{}.exe", self.name),
            None,
            Platform::Windows,
        )
    }

    fn install(self) -> Self {
        plan_and_install(&with_settings(), &self.target(), &self.profile());
        self
    }

    /// The backup of `settings.json`, where the ledger says it is.
    fn backup(&self) -> PathBuf {
        let ledger = installed(&self.profile()).unwrap().unwrap();
        let g = ledger
            .game_files
            .iter()
            .find(|g| g.rel == "settings.json")
            .unwrap();
        self.profile().join(&g.backup.as_ref().unwrap().rel)
    }

    fn ledger_json(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(self.profile().join(LEDGER_FILE_NAME)).unwrap())
            .unwrap()
    }
}

/// The two ways the game half is undone, side by side: every rule below holds for both.
type Undo = fn(&std::path::Path, &GameTarget) -> astra_game_launcher::Result<UninstallReport>;
const UNDOS: [(&str, Undo); 2] = [
    ("uninstall", uninstall),
    ("restore_game_files", restore_game_files),
];

/// What can sit where Astra placed a file once the game, the player or another tool had their
/// turn. None of it is Astra's to touch.
#[derive(Debug, Clone, Copy)]
enum Left {
    /// A file we added, rewritten since (the player's `doorstop_config.ini` now).
    AddedChanged,
    /// A file we replaced, updated by the game since.
    ReplacedChanged,
    /// The backup of a file we replaced, its bytes changed in the profile.
    BackupChanged,
    /// The backup of a file we replaced, gone from the profile.
    BackupMissing,
    /// A folder where a file we added was.
    FolderInPlace,
    /// A link where a file we replaced was, to a file with our very bytes elsewhere.
    #[cfg(unix)]
    LinkInPlace,
    /// A proxy we added whose folder gained a name since (someone's loader may run through it).
    ProxyFolderGained,
}

const LEFT: &[Left] = &[
    Left::AddedChanged,
    Left::ReplacedChanged,
    Left::BackupChanged,
    Left::BackupMissing,
    Left::FolderInPlace,
    #[cfg(unix)]
    Left::LinkInPlace,
    Left::ProxyFolderGained,
];

#[test]
fn a_restore_never_overwrites_a_file_astra_did_not_place() {
    for (how, undo) in UNDOS {
        for left in LEFT {
            let at = format!("{how}, {left:?}");
            let w = World::new().install();
            let outside = Tmp::new("restore-outside");
            // The file the change leaves, and the reason `kept_reasons` gives, if any.
            let (rel, reason) = match left {
                Left::AddedChanged => {
                    write(
                        &w.game.join("doorstop_config.ini"),
                        b"[General]\nenabled = true\n",
                    );
                    ("doorstop_config.ini", None)
                }
                Left::ReplacedChanged => {
                    write(&w.game.join("settings.json"), b"{\"game\":\"update 2\"}");
                    ("settings.json", None)
                }
                Left::BackupChanged => {
                    write(&w.backup(), b"{\"game\":\"not what was backed up\"}");
                    ("settings.json", None)
                }
                Left::BackupMissing => {
                    fs::remove_file(w.backup()).unwrap();
                    ("settings.json", None)
                }
                Left::FolderInPlace => {
                    fs::remove_file(w.game.join("doorstop_config.ini")).unwrap();
                    write(&w.game.join("doorstop_config.ini/inside.txt"), b"theirs");
                    ("doorstop_config.ini", None)
                }
                #[cfg(unix)]
                Left::LinkInPlace => {
                    write(&outside.join("settings.json"), OURS);
                    fs::remove_file(w.game.join("settings.json")).unwrap();
                    std::os::unix::fs::symlink(
                        outside.join("settings.json"),
                        w.game.join("settings.json"),
                    )
                    .unwrap();
                    ("settings.json", None)
                }
                Left::ProxyFolderGained => {
                    write(&w.game.join("Mods/their_mod.lua"), b"-- theirs");
                    ("winhttp.dll", Some(KEPT_FOLDER_GAINED_FILES))
                }
            };
            let before = snapshot(w.game.path());
            let outside_before = snapshot(outside.path());

            let r = undo(&w.profile(), &w.target()).unwrap();
            assert_eq!(r.warnings, [codes::RESTORE_PARTIAL], "{at}: {r:?}");
            assert_eq!(r.kept, [rel], "{at}: the file is named");
            assert_eq!(
                r.kept_reasons.get(rel).map(String::as_str),
                reason,
                "{at}: {r:?}"
            );
            let after = snapshot(w.game.path());
            for (path, bytes) in &before {
                if path == rel || path.starts_with(&format!("{rel}/")) {
                    assert_eq!(after.get(path), Some(bytes), "{at}: {path} left as it was");
                }
            }
            #[cfg(unix)]
            if matches!(left, Left::LinkInPlace) {
                let m = fs::symlink_metadata(w.game.join(rel)).unwrap();
                assert!(m.file_type().is_symlink(), "{at}: the link is still a link");
            }
            assert_eq!(snapshot(outside.path()), outside_before, "{at}");

            // Everything else of ours went, and the game's own file came back where it could.
            for ours in ["winhttp.dll", "doorstop_config.ini"] {
                if ours != rel {
                    assert!(!w.game.join(ours).exists(), "{at}: {ours} removed");
                }
            }
            if rel != "settings.json" {
                assert_eq!(fs::read(w.game.join("settings.json")).unwrap(), ORIGINAL);
            }
        }
    }
}

/// The rule M2 keeps, read from the other side: a file that still carries our digest is ours and
/// is removed or restored, and a backup that is what was recorded goes back over it. Nothing is
/// partial then.
#[test]
fn a_file_with_our_digest_is_removed_or_restored_and_nothing_else_is_touched() {
    for (how, undo) in UNDOS {
        let w = World::new();
        write(&w.game.join("readme.txt"), b"the game's own");
        let before = snapshot(w.game.path());
        let w = w.install();
        assert_eq!(fs::read(w.game.join("settings.json")).unwrap(), OURS);
        let r = undo(&w.profile(), &w.target()).unwrap();
        assert_eq!(r.restored, ["settings.json"], "{how}");
        assert_eq!(r.removed, ["doorstop_config.ini", "winhttp.dll"], "{how}");
        assert!(r.kept.is_empty() && r.warnings.is_empty(), "{how}: {r:?}");
        assert_eq!(snapshot(w.game.path()), before, "{how}");
        // Not partial, and not in the JSON at all.
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("warnings"), "{how}: {json}");
    }
}

/// A file we replaced that is gone: the backup goes back only over our file, so the player's
/// is not back, and the report says so though `kept` has nothing to name.
#[test]
fn a_replaced_file_that_is_gone_is_not_put_back_and_is_partial() {
    for (how, undo) in UNDOS {
        let w = World::new().install();
        fs::remove_file(w.game.join("settings.json")).unwrap();
        let r = undo(&w.profile(), &w.target()).unwrap();
        assert_eq!(r.warnings, [codes::RESTORE_PARTIAL], "{how}: {r:?}");
        assert!(r.kept.is_empty() && r.restored.is_empty(), "{how}: {r:?}");
        assert!(!w.game.join("settings.json").exists(), "{how}");
    }
}

#[test]
fn a_clean_restore_puts_the_game_folder_back_and_keeps_the_profile() {
    let w = World::new();
    let before = snapshot(w.game.path());
    let w = w.install();
    let ledger = installed(&w.profile()).unwrap().unwrap();
    let backup = w.backup();
    assert!(backup.is_file());

    let r = restore_game_files(&w.profile(), &w.target()).unwrap();
    assert_eq!(r.restored, ["settings.json"]);
    assert_eq!(r.removed, ["doorstop_config.ini", "winhttp.dll"]);
    assert!(r.kept.is_empty() && r.warnings.is_empty(), "{r:?}");
    assert_eq!(r.profile_files_removed, 0, "the profile is kept");
    assert_eq!(snapshot(w.game.path()), before, "byte for byte");

    // The profile's own files stay; the backups go, as nothing names them now.
    for f in &ledger.files {
        assert!(w.profile().join(&f.rel).is_file(), "{} kept", f.rel);
    }
    assert!(!backup.exists());
    let restored = installed(&w.profile()).unwrap().unwrap();
    assert_eq!(restored.state, LedgerState::Restored);
    assert!(restored.game_files.is_empty() && restored.proton.is_empty());
    assert!(restored.loader_baseline.is_empty() && restored.proxy_folders.is_empty());
    assert_eq!(
        (&restored.files, &restored.manifest, &restored.integration),
        (&ledger.files, &ledger.manifest, &ledger.integration)
    );

    // Again: nothing left to do, and nothing changes.
    let on_disk = w.ledger_json();
    let again = restore_game_files(&w.profile(), &w.target()).unwrap();
    assert!(
        again.removed.is_empty() && again.restored.is_empty() && again.warnings.is_empty(),
        "{again:?}"
    );
    assert_eq!(w.ledger_json(), on_disk);
    assert_eq!(snapshot(w.game.path()), before);
}

#[test]
fn play_after_a_restore_is_refused() {
    let w = World::new().install();
    prepare_launch(&w.profile(), &w.target(), &vars(), None).unwrap();
    restore_game_files(&w.profile(), &w.target()).unwrap();
    let e = prepare_launch(&w.profile(), &w.target(), &vars(), None).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::NEEDS_REINSTALL, Some("restored"))
    );
}

#[test]
fn uninstall_after_a_restore_removes_the_profile() {
    let w = World::new();
    let before = snapshot(w.game.path());
    let w = w.install();
    let files = installed(&w.profile()).unwrap().unwrap().files.len() as u32;
    restore_game_files(&w.profile(), &w.target()).unwrap();
    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(r.profile_files_removed, files);
    assert!(r.removed.is_empty() && r.restored.is_empty() && r.warnings.is_empty());
    assert_eq!(installed(&w.profile()).unwrap(), None);
    assert!(!w.profile().join("BepInEx/core/BepInEx.dll").exists());
    assert_eq!(snapshot(w.game.path()), before);
    assert_eq!(
        restore_game_files(&w.profile(), &w.target())
            .unwrap_err()
            .code,
        codes::NOT_INSTALLED
    );
}

/// Installing over a restored ledger is a fresh install: the ledger it writes is the one the
/// first install wrote on the same folder, nothing carried from the restored one.
#[test]
fn an_install_over_a_restored_ledger_is_a_fresh_install() {
    let w = World::new();
    let before = snapshot(w.game.path());
    let w = w.install();
    let first = installed(&w.profile()).unwrap().unwrap();
    restore_game_files(&w.profile(), &w.target()).unwrap();

    let pkg = with_settings();
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    let actions: Vec<_> = plan
        .game_files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect();
    use astra_game_launcher::GameAction::{Add, Replace};
    assert_eq!(
        actions,
        [
            ("winhttp.dll", Add),
            ("doorstop_config.ini", Add),
            ("settings.json", Replace)
        ]
    );
    let again = install(&pkg, &plan, &w.target(), ACCEPT, &Ctx::silent()).unwrap();
    assert_eq!(again, first);
    prepare_launch(&w.profile(), &w.target(), &vars(), None).unwrap();
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(snapshot(w.game.path()), before);
}

/// The ledger a restore leaves is schema 4: launchers 0.3.1 (what Astra 0.2.7 ships) and 0.4.1
/// refuse it as too new, rather than play or uninstall it by files it no longer lists.
#[test]
fn the_restored_ledger_is_schema_4_and_older_launchers_refuse_it() {
    let w = World::new().install();
    assert_eq!(
        w.ledger_json()["schema"],
        1,
        "an install's is the lowest schema"
    );
    restore_game_files(&w.profile(), &w.target()).unwrap();
    let v = w.ledger_json();
    assert_eq!(v["schema"], 4);
    assert_eq!(astra_game_launcher::LEDGER_SCHEMA, 4);
    assert_eq!(v["state"], "restored");
    assert_eq!(v["game_files"], serde_json::json!([]));
    for absent in ["proton", "loader_baseline", "proxy_folders"] {
        assert!(v.get(absent).is_none(), "{absent}: {v}");
    }
    assert!(!v["files"].as_array().unwrap().is_empty());

    let legacy = launcher_legacy::installed(&w.profile()).unwrap_err();
    assert_eq!(
        (legacy.code, legacy.param("schema")),
        (launcher_legacy::codes::SCHEMA_TOO_NEW, Some("4"))
    );
    let v031 = launcher_031::installed(&w.profile()).unwrap_err();
    assert_eq!(
        (v031.code, v031.param("schema")),
        (launcher_031::codes::SCHEMA_TOO_NEW, Some("4"))
    );
    // And this launcher reads it.
    assert_eq!(
        installed(&w.profile()).unwrap().unwrap().state,
        LedgerState::Restored
    );
}

/// A process the launcher takes for the game `exe` running, for as long as it lives: on Linux a
/// `sleep` whose first argument is the game's Windows path, as Proton starts a game; on Windows
/// a copy of this test program named `exe`, running [`stand_in_for_a_running_game`].
#[cfg(any(target_os = "linux", windows))]
struct Running(std::process::Child);

#[cfg(any(target_os = "linux", windows))]
impl Running {
    fn start(dir: &std::path::Path, exe: &str) -> Running {
        #[cfg(target_os = "linux")]
        let child = {
            use std::os::unix::process::CommandExt;
            let _ = dir;
            std::process::Command::new("sleep")
                .arg0(format!("Z:\\games\\{exe}"))
                .arg("60")
                .spawn()
                .unwrap()
        };
        #[cfg(windows)]
        let child = {
            let copy = dir.join(exe);
            fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
            std::process::Command::new(&copy)
                .args(["--exact", "stand_in_for_a_running_game", "--ignored"])
                .env(STAND_IN, "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let running = Running(child);
        let seen = (0..250).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            astra_game_launcher::is_running(exe)
        });
        assert!(seen, "the stand-in game never showed up as running");
        running
    }
}

#[cfg(any(target_os = "linux", windows))]
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(windows)]
const STAND_IN: &str = "AGL_RESTORE_STAND_IN";

/// Started by [`Running`] on Windows only, as a copy of this program named after the game.
#[cfg(windows)]
#[test]
#[ignore = "a stand-in for a running game, which Running::start runs"]
fn stand_in_for_a_running_game() {
    if std::env::var_os(STAND_IN).is_some() {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_restore_while_the_game_runs_is_refused() {
    // A name of this test's own, so no other test's game counts as running.
    let name = format!("Restore Probe {}", std::process::id());
    let w = World::named(&name).install();
    let before = snapshot(w.game.path());
    let ledger = w.ledger_json();
    let bin = Tmp::new("restore-running");
    {
        let _game = Running::start(bin.path(), &format!("{name}.exe"));
        let e = restore_game_files(&w.profile(), &w.target()).unwrap_err();
        assert_eq!(
            (e.code, e.param("exe")),
            (codes::GAME_RUNNING, Some(format!("{name}.exe").as_str()))
        );
        assert_eq!(snapshot(w.game.path()), before, "nothing touched");
        assert_eq!(w.ledger_json(), ledger, "the ledger as it was");
    }
    // Once it exits, the restore runs.
    restore_game_files(&w.profile(), &w.target()).unwrap();
    assert_eq!(
        installed(&w.profile()).unwrap().unwrap().state,
        LedgerState::Restored
    );
}
