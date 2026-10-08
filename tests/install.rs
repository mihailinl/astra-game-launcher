// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The files beside the exe, the ledger, uninstall, and the path jail on disk.

mod common;

use std::cell::Cell;
use std::fs;

use astra_game_launcher::{
    Ctx, GameAction, GiPackage, Platform, codes, install, installed, plan_install, uninstall,
};
use common::*;

const GAME: &str = "Some Game";

fn exe() -> String {
    format!("{GAME}.exe")
}

/// The runner plus one more game file, `settings.json`, that replaces a game file of that name.
fn with_settings() -> GiPackage {
    let m = runner_manifest(
        "\n[[game_files]]\nfrom = \"extra/settings.json\"\nto = \"${game}/settings.json\"\n",
    );
    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
        E::File("extra/settings.json", b"{\"ours\":true}"),
    ]);
    open(&z).unwrap()
}

struct World {
    game: Tmp,
    profile_parent: Tmp,
}

impl World {
    fn new() -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), GAME);
        World {
            game,
            profile_parent: Tmp::new("profiles"),
        }
    }

    fn profile(&self) -> std::path::PathBuf {
        self.profile_parent.join("p")
    }

    fn target(&self) -> astra_game_launcher::GameTarget {
        target(self.game.path(), &exe(), None, Platform::Windows)
    }
}

#[test]
fn add_then_uninstall_leaves_the_game_as_it_was() {
    let w = World::new();
    let before = snapshot(w.game.path());
    let pkg = runner();
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    assert!(!w.profile().exists(), "planning writes nothing");
    let actions: Vec<_> = plan
        .game_files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect();
    assert_eq!(
        actions,
        vec![
            ("winhttp.dll", GameAction::Add),
            ("doorstop_config.ini", GameAction::Add)
        ]
    );
    assert_eq!(plan.warnings, vec!["experimental", "not-reviewed"]);
    assert_eq!(plan.proton_override, None, "no Proton on Windows");

    let ledger = install(&pkg, &plan, &w.target(), &Ctx::silent()).unwrap();
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), WINHTTP);
    assert_eq!(
        fs::read(w.profile().join("BepInEx/core/BepInEx.Preloader.dll")).unwrap(),
        PRELOADER
    );
    assert!(
        !w.profile().join("licenses").exists(),
        "files the manifest does not name stay in the zip"
    );
    assert_eq!(installed(&w.profile()).unwrap(), Some(ledger));

    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(r.removed, vec!["doorstop_config.ini", "winhttp.dll"]);
    assert_eq!(r.profile_files_removed, 2);
    assert_eq!(snapshot(w.game.path()), before);
    assert_eq!(installed(&w.profile()).unwrap(), None);
    assert_eq!(
        uninstall(&w.profile(), &w.target()).unwrap_err().code,
        codes::NOT_INSTALLED
    );
}

#[test]
fn replace_backs_up_and_uninstall_restores() {
    let w = World::new();
    write(&w.game.join("settings.json"), b"{\"game\":\"original\"}");
    let before = snapshot(w.game.path());
    let pkg = with_settings();
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    let s = plan
        .game_files
        .iter()
        .find(|g| g.rel_path == "settings.json")
        .unwrap();
    assert_eq!(s.action, GameAction::Replace);
    assert_eq!(
        s.existing_sha256.as_deref(),
        Some(sha(b"{\"game\":\"original\"}").as_str())
    );

    let ledger = install(&pkg, &plan, &w.target(), &Ctx::silent()).unwrap();
    assert_eq!(
        fs::read(w.game.join("settings.json")).unwrap(),
        b"{\"ours\":true}"
    );
    let backup = ledger
        .game_files
        .iter()
        .find(|g| g.rel == "settings.json")
        .unwrap()
        .backup
        .clone()
        .unwrap();
    assert_eq!(
        fs::read(w.profile().join(&backup.rel)).unwrap(),
        b"{\"game\":\"original\"}"
    );

    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(r.restored, vec!["settings.json"]);
    assert_eq!(snapshot(w.game.path()), before);
}

#[test]
fn uninstall_restores_only_a_file_that_still_has_our_digest() {
    let w = World::new();
    write(&w.game.join("settings.json"), b"original");
    let pkg = with_settings();
    plan_and_install(&pkg, &w.target(), &w.profile());
    // The game updates both files we placed after we placed them.
    write(&w.game.join("settings.json"), b"game update 2");
    write(&w.game.join("doorstop_config.ini"), b"someone else's ini");

    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(r.kept, vec!["settings.json", "doorstop_config.ini"]);
    assert_eq!(r.removed, vec!["winhttp.dll"]);
    assert_eq!(
        fs::read(w.game.join("settings.json")).unwrap(),
        b"game update 2",
        "the game's file wins"
    );
    assert_eq!(
        fs::read(w.game.join("doorstop_config.ini")).unwrap(),
        b"someone else's ini"
    );
}

#[test]
fn a_known_proxy_is_reused_and_never_rewritten_nor_removed() {
    let w = World::new();
    write(&w.game.join("winhttp.dll"), WINHTTP);
    let before_meta = fs::metadata(w.game.join("winhttp.dll")).unwrap();
    let pkg = runner();
    let plan = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(plan.game_files[0].action, GameAction::ReuseKnownProxy);
    let after_meta = fs::metadata(w.game.join("winhttp.dll")).unwrap();
    assert_eq!(
        before_meta.modified().unwrap(),
        after_meta.modified().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            before_meta.ino(),
            after_meta.ino(),
            "the same file, not a rewrite"
        );
    }
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(
        fs::read(w.game.join("winhttp.dll")).unwrap(),
        WINHTTP,
        "it was not ours to remove"
    );
}

#[test]
fn a_foreign_loader_is_refused() {
    for name in ["winhttp.dll", "WinHttp.dll"] {
        let w = World::new();
        write(&w.game.join(name), b"MZ another loader's proxy");
        let e = plan_install(&runner(), &w.target(), &w.profile()).unwrap_err();
        assert_eq!(e.code, codes::FOREIGN_LOADER, "{name}");
        let file = e.param("file").unwrap();
        assert!(file.eq_ignore_ascii_case("winhttp.dll"), "a case variant is the same file to Wine");
    }
}

#[test]
fn an_existing_doorstop_config_is_left_untouched() {
    let w = World::new();
    let theirs = b"[General]\nenabled = true\ntarget_assembly = theirs.dll\n";
    write(&w.game.join("doorstop_config.ini"), theirs);
    let pkg = runner();
    let plan = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(plan.game_files[1].action, GameAction::KeepExisting);
    assert_eq!(
        fs::read(w.game.join("doorstop_config.ini")).unwrap(),
        theirs
    );
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(
        fs::read(w.game.join("doorstop_config.ini")).unwrap(),
        theirs
    );
}

#[test]
fn a_changed_game_folder_makes_the_plan_stale() {
    let w = World::new();
    let pkg = runner();
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    write(&w.game.join("doorstop_config.ini"), b"appeared");
    let e = install(&pkg, &plan, &w.target(), &Ctx::silent()).unwrap_err();
    assert_eq!(e.code, codes::PLAN_STALE);
    assert!(!w.game.join("winhttp.dll").exists());
}

#[test]
fn a_cancelled_install_rolls_back() {
    let w = World::new();
    write(&w.game.join("settings.json"), b"original");
    let before = snapshot(w.game.path());
    let pkg = with_settings();
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    let calls = Cell::new(0);
    // Let the profile files and the first game file through, then cancel.
    let cancel = || {
        calls.set(calls.get() + 1);
        calls.get() > 5
    };
    let progress = |_| {};
    let e = install(
        &pkg,
        &plan,
        &w.target(),
        &Ctx {
            cancel: &cancel,
            progress: &progress,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, codes::CANCELLED);
    assert_eq!(snapshot(w.game.path()), before);
    assert_eq!(installed(&w.profile()).unwrap(), None);
    assert!(!w.profile().join("BepInEx/core/BepInEx.dll").exists());
}

#[test]
fn installing_again_replaces_the_previous_install_cleanly() {
    let w = World::new();
    write(&w.game.join("settings.json"), b"original");
    let before = snapshot(w.game.path());
    let pkg = with_settings();
    plan_and_install(&pkg, &w.target(), &w.profile());
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    assert_eq!(
        plan.replaces.as_ref().map(|i| i.id.as_str()),
        Some("test.unity")
    );
    assert!(plan.warnings.contains(&"replaces-integration".to_owned()));
    let actions: Vec<_> = plan.game_files.iter().map(|g| g.action).collect();
    assert_eq!(
        actions,
        vec![GameAction::Add, GameAction::Add, GameAction::Replace],
        "as if uninstalled first"
    );
    install(&pkg, &plan, &w.target(), &Ctx::silent()).unwrap();
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(
        snapshot(w.game.path()),
        before,
        "the original survives two installs"
    );
}

#[test]
fn the_jail_holds_on_disk_through_links() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        // A link inside the profile pointing outside it.
        let w = World::new();
        let outside = Tmp::new("outside");
        fs::create_dir_all(w.profile()).unwrap();
        symlink(outside.path(), w.profile().join("BepInEx")).unwrap();
        let pkg = runner();
        let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
        let e = install(&pkg, &plan, &w.target(), &Ctx::silent()).unwrap_err();
        assert_eq!(e.code, codes::PATH_ESCAPES);
        assert!(
            fs::read_dir(outside.path()).unwrap().next().is_none(),
            "nothing written outside"
        );
        assert!(!w.game.join("winhttp.dll").exists(), "rolled back");

        // A link beside the exe, where a game file goes.
        let m = runner_manifest(
            "\n[[game_files]]\nfrom = \"extra/x.dll\"\nto = \"${game}/sub/x.dll\"\n",
        );
        let z = zip(&[
            E::File("astra-gi.toml", m.as_bytes()),
            E::File("winhttp.dll", WINHTTP),
            E::File("doorstop_config.ini", DOORSTOP_INI),
            E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
            E::File("extra/x.dll", b"x"),
        ]);
        let pkg = open(&z).unwrap();
        let w = World::new();
        symlink(outside.path(), w.game.join("sub")).unwrap();
        let e = plan_install(&pkg, &w.target(), &w.profile()).unwrap_err();
        assert_eq!(e.code, codes::PATH_ESCAPES);
    }
}

#[test]
fn the_profile_may_not_overlap_the_game() {
    let w = World::new();
    let e = plan_install(&runner(), &w.target(), &w.game.join("profile")).unwrap_err();
    assert_eq!(e.code, codes::PATH_ESCAPES);
    assert_eq!(e.param("reason"), Some("profile-overlaps-game"));
}

#[test]
fn the_game_is_checked_before_anything() {
    let pkg = runner();
    // Anti-cheat in the folder.
    let w = World::new();
    write(&w.game.join("EasyAntiCheat/EasyAntiCheat_x64.dll"), b"MZ");
    let e = plan_install(&pkg, &w.target(), &w.profile()).unwrap_err();
    assert_eq!(
        (e.code, e.param("kind")),
        (codes::ANTI_CHEAT, Some("easy-anti-cheat"))
    );

    // Anti-cheat declared by the manifest.
    let m = runner_manifest("").replacen(
        "anti_cheat = \"none\"",
        "anti_cheat = \"easy-anti-cheat\"",
        1,
    );
    let w = World::new();
    let e = plan_install(&open(&runner_zip(&m)).unwrap(), &w.target(), &w.profile()).unwrap_err();
    assert_eq!(e.code, codes::ANTI_CHEAT);

    // An IL2CPP game for a Mono runner.
    let g = Tmp::new("il2cpp");
    write(&g.join("Game.exe"), b"MZ");
    write(&g.join("GameAssembly.dll"), b"MZ");
    write(&g.join("Game_Data/il2cpp_data/x"), b"x");
    let p = Tmp::new("p");
    let e = plan_install(
        &pkg,
        &target(g.path(), "Game.exe", None, Platform::Windows),
        &p.join("p"),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("want"), e.param("got")),
        (
            codes::ENGINE_MISMATCH,
            Some("unity-mono"),
            Some("unity-il2cpp")
        )
    );

    // No such exe.
    let w = World::new();
    let e = plan_install(
        &pkg,
        &target(w.game.path(), "Nope.exe", None, Platform::Windows),
        &w.profile(),
    )
    .unwrap_err();
    assert_eq!(e.code, codes::EXE_NOT_FOUND);

    // A platform the manifest does not list.
    let m = runner_manifest("").replacen(", \"linux-x64\"]", "]", 1);
    let e = plan_install(
        &open(&runner_zip(&m)).unwrap(),
        &target(w.game.path(), &exe(), None, Platform::LinuxNative),
        &w.profile(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("platform")),
        (codes::PLATFORM_UNSUPPORTED, Some("linux-x64"))
    );

    // A per-game manifest for another exe.
    let m = runner_manifest("").replacen("exe = \"*\"", "exe = \"Other Game.exe\"", 1);
    let e = plan_install(&open(&runner_zip(&m)).unwrap(), &w.target(), &w.profile()).unwrap_err();
    assert_eq!(
        (e.code, e.param("field")),
        (codes::GAME_MISMATCH, Some("exe"))
    );
}
