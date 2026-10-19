// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sharing the Doorstop another mod manager (r2modman, Thunderstore Mod Manager) left beside the
//! game: the install runs through it and never writes, backs up or removes it, Play checks it
//! still does the job, and uninstall leaves the other manager's files exactly as they were.

mod common;

use std::fs;
use std::path::PathBuf;

use astra_game_launcher::{
    Ctx, GameAction, GameTarget, Platform, codes, install, plan_install, prepare_launch, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
const APPID: u32 = 4_000_002;

/// What Thunderstore Mod Manager leaves in the game folder: its Doorstop (which also knows an
/// option ours does not), a `doorstop_config.ini` that is switched ON and points at a BepInEx
/// that is not in the game folder, and its marker file.
const THEIR_INI: &[u8] =
    b"[General]\nenabled = true\ntarget_assembly=BepInEx\\core\\BepInEx.Preloader.dll\n";

fn theirs() -> Vec<u8> {
    pe_with_options(
        X64,
        &[DOORSTOP_V4[0], "--doorstop-bypass-vulkan", DOORSTOP_V4[1]],
        b"4.4.1PEAK",
    )
}

fn ours() -> Vec<u8> {
    pe_with_options(X64, DOORSTOP_V4, b"4.3.0")
}

struct World {
    game: Tmp,
    profiles: Tmp,
}

impl World {
    /// A Unity game whose exe is an x64 PE image, with or without the other manager's files.
    fn new(with_other_manager: bool) -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), GAME);
        write(
            &game.join(&format!("{GAME}.exe")),
            &pe_with_options(X64, &[], b"game"),
        );
        if with_other_manager {
            write(&game.join("winhttp.dll"), &theirs());
            write(&game.join("doorstop_config.ini"), THEIR_INI);
            write(&game.join(".thunderstoremm"), b"");
        }
        World {
            game,
            profiles: Tmp::new("profiles"),
        }
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> GameTarget {
        target(
            self.game.path(),
            &format!("{GAME}.exe"),
            Some(APPID),
            Platform::Windows,
        )
    }
}

fn actions(plan: &astra_game_launcher::InstallPlan) -> Vec<(&str, GameAction)> {
    plan.game_files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect()
}

fn launch(w: &World) -> Result<Vec<String>, astra_game_launcher::LauncherError> {
    prepare_launch(
        &w.profile(),
        &w.target(),
        &vars(),
        Some(std::path::Path::new("/opt/fake/steam")),
    )
    .map(|p| p.args)
}

#[test]
fn another_managers_doorstop_is_shared_and_left_exactly_as_it_was() {
    let w = World::new(true);
    let before = snapshot(w.game.path());
    let pkg = doorstop_runner(&ours());

    let plan = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(
        actions(&plan),
        vec![
            ("winhttp.dll", GameAction::ShareLoader),
            ("doorstop_config.ini", GameAction::KeepExisting),
        ]
    );
    assert_eq!(
        plan.game_files[0].existing_sha256.as_deref(),
        Some(sha(&theirs()).as_str())
    );
    assert_eq!(
        snapshot(w.game.path()),
        before,
        "nothing in the game folder was written"
    );

    let ledger =
        fs::read_to_string(w.profile().join(astra_game_launcher::LEDGER_FILE_NAME)).unwrap();
    assert!(ledger.contains("\"schema\": 3"), "{ledger}");
    assert!(ledger.contains("share-loader"), "{ledger}");

    // The command line chooses OUR profile: that is what makes their proxy load our BepInEx.
    let args = launch(&w).unwrap();
    let target = args
        .iter()
        .position(|a| a == "--doorstop-target-assembly")
        .map(|i| &args[i + 1])
        .unwrap();
    assert!(target.contains("BepInEx"), "{args:?}");
    assert!(args.iter().any(|a| a == "--doorstop-enabled"));

    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(
        report.removed.is_empty() && report.restored.is_empty(),
        "{report:?}"
    );
    assert_eq!(
        snapshot(w.game.path()),
        before,
        "uninstall left their files as they were"
    );
}

#[test]
fn the_other_manager_may_update_its_doorstop_and_play_still_works() {
    let w = World::new(true);
    plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    write(
        &w.game.join("winhttp.dll"),
        &pe_with_options(X64, DOORSTOP_V4, b"4.5.0, relinked"),
    );
    launch(&w).unwrap();
}

#[test]
fn a_shared_doorstop_that_is_gone_asks_for_an_install_which_adds_ours() {
    let w = World::new(true);
    let pkg = doorstop_runner(&ours());
    plan_and_install(&pkg, &w.target(), &w.profile());
    fs::remove_file(w.game.join("winhttp.dll")).unwrap();
    let e = launch(&w).unwrap_err();
    assert_eq!(e.code, codes::NEEDS_REINSTALL);
    assert_eq!(e.param("file"), Some("winhttp.dll"));
    let plan = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(plan.game_files[0].action, GameAction::Add);
    launch(&w).unwrap();
}

#[test]
fn a_shared_doorstop_no_longer_able_is_a_foreign_loader_at_play() {
    // Installing again would be refused the same way, so Play does not send the person there.
    let cases = [
        (
            "doorstop 3",
            pe_with_options(X64, &["--doorstop-enable"], b"v3"),
        ),
        ("32-bit", pe_with_options(0x014C, DOORSTOP_V4, b"x86")),
    ];
    for (why, replacement) in cases {
        let w = World::new(true);
        let pkg = doorstop_runner(&ours());
        plan_and_install(&pkg, &w.target(), &w.profile());
        write(&w.game.join("winhttp.dll"), &replacement);
        let e = launch(&w).unwrap_err();
        assert_eq!(e.code, codes::FOREIGN_LOADER, "{why}");
        assert_eq!(e.param("file"), Some("winhttp.dll"), "{why}");
        let again = plan_install(&pkg, &w.target(), &w.profile()).unwrap_err();
        assert_eq!(again.code, codes::FOREIGN_LOADER, "{why}");
    }
}

#[test]
fn an_update_over_a_shared_loader_keeps_sharing_and_touches_nothing() {
    let w = World::new(true);
    let before = snapshot(w.game.path());
    let pkg = doorstop_runner(&ours());
    plan_and_install(&pkg, &w.target(), &w.profile());
    let again = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(
        actions(&again),
        vec![
            ("winhttp.dll", GameAction::ShareLoader),
            ("doorstop_config.ini", GameAction::KeepExisting),
        ]
    );
    assert!(again.warnings.iter().any(|w| w == "replaces-integration"));
    assert_eq!(snapshot(w.game.path()), before);
    launch(&w).unwrap();
}

#[test]
fn a_proxy_with_our_very_bytes_that_another_manager_relinks_still_plays() {
    let w = World::new(false);
    write(&w.game.join("winhttp.dll"), &ours());
    let pkg = doorstop_runner(&ours());
    let plan = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(plan.game_files[0].action, GameAction::ReuseKnownProxy);
    write(
        &w.game.join("winhttp.dll"),
        &pe_with_options(X64, DOORSTOP_V4, b"newer"),
    );
    launch(&w).unwrap();
    write(&w.game.join("winhttp.dll"), b"MZ another loader's proxy");
    assert_eq!(launch(&w).unwrap_err().code, codes::FOREIGN_LOADER);
}

#[test]
fn a_doorstop_never_stands_in_for_a_proxy_of_ours_that_is_not_one() {
    // `runner()` ships a proxy that is not a Doorstop build (think ReShade's dxgi.dll), though
    // its launch passes Doorstop options: theirs cannot do ITS job.
    let w = World::new(true);
    let e = plan_install(&runner(), &w.target(), &w.profile()).unwrap_err();
    assert_eq!(e.code, codes::FOREIGN_LOADER);
}

#[test]
fn a_ledger_newer_than_the_launcher_is_schema_too_new_whatever_its_shape() {
    let p = Tmp::new("ledger");
    write(
        &p.join(astra_game_launcher::LEDGER_FILE_NAME),
        br#"{"schema": 99, "game_files": [{"action": "something-from-the-future"}]}"#,
    );
    let e = astra_game_launcher::installed(p.path()).unwrap_err();
    assert_eq!(e.code, codes::SCHEMA_TOO_NEW);
}

#[test]
fn ours_overwritten_by_the_other_manager_is_shared_after_an_install() {
    let w = World::new(false);
    let pkg = doorstop_runner(&ours());
    plan_and_install(&pkg, &w.target(), &w.profile());
    // The other manager links its own proxy over ours, as r2modman does at every modded start.
    write(&w.game.join("winhttp.dll"), &theirs());
    assert_eq!(launch(&w).unwrap_err().code, codes::NEEDS_REINSTALL);

    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    assert_eq!(plan.game_files[0].action, GameAction::ShareLoader);
    install(&pkg, &plan, &w.target(), ACCEPT, &Ctx::silent()).unwrap();
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), theirs());
    launch(&w).unwrap();
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), theirs());
}

#[test]
fn a_foreign_proxy_that_cannot_do_our_job_is_still_refused() {
    let cases: [(&str, Vec<u8>); 3] = [
        (
            "doorstop 3 does not know doorstop 4's options",
            pe_with_options(X64, &["--doorstop-enable", "--doorstop-target"], b"3.4"),
        ),
        (
            "a 32-bit proxy is not loaded by a 64-bit game",
            pe_with_options(0x014C, DOORSTOP_V4, b"x86"),
        ),
        (
            "another loader altogether",
            b"MZ another loader's proxy".to_vec(),
        ),
    ];
    for (why, foreign) in cases {
        let w = World::new(false);
        write(&w.game.join("winhttp.dll"), &foreign);
        let e = plan_install(&doorstop_runner(&ours()), &w.target(), &w.profile()).unwrap_err();
        assert_eq!(e.code, codes::FOREIGN_LOADER, "{why}");
    }
}

#[test]
fn a_launch_that_does_not_choose_the_target_never_shares() {
    // Without `--doorstop-target-assembly` their proxy would load what THEIR config names.
    let ours = ours();
    let m = runner_manifest("")
        .replace(&sha(WINHTTP), &sha(&ours))
        .replace(
            r#""--doorstop-target-assembly", "${profile}/BepInEx/core/BepInEx.Preloader.dll""#,
            "",
        )
        .replace(r#""true", ]"#, r#""true"]"#);
    assert!(!m.contains("--doorstop-target-assembly"), "{m}");
    let pkg = open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", &ours),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]))
    .unwrap();
    let w = World::new(true);
    let e = plan_install(&pkg, &w.target(), &w.profile()).unwrap_err();
    assert_eq!(e.code, codes::FOREIGN_LOADER);
}
