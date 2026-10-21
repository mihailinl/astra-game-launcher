// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A BepInEx the person runs beside ours, or switched off themselves. Uninstall never takes away
//! a Doorstop their loader now runs through; a plan never brings switched-off mods back; and the
//! settings in a `doorstop_config.ini` that is not ours never decide Astra's session.

mod common;

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;

use astra_game_launcher::{
    Ctx, GameAction, GameTarget, InstallPlan, LauncherError, Platform, Progress, codes, install,
    plan_install, prepare_launch, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
const APPID: u32 = 4_000_003;

/// The `doorstop_config.ini` BepInEx 5.4.23's zip puts beside the game, shortened, CRLF and all.
const BEPINEX_INI: &[u8] = b"# General options for Unity Doorstop\r\n[General]\r\n\r\n# Enable Doorstop?\r\nenabled = true\r\n\r\n# Path to the assembly to load and execute\r\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\r\n\r\nboot_config_override =\r\n\r\n[UnityMono]\r\ndll_search_path_override =\r\ndebug_enabled = false\r\ndebug_address = 127.0.0.1:10000\r\ndebug_suspend = false\r\n";

/// The proxy our runner ships: BepInEx 5.4.23's own Doorstop 4 `winhttp.dll`, which a BepInEx
/// installed by hand puts there byte for byte.
fn ours() -> Vec<u8> {
    pe_with_options(X64, DOORSTOP_V4, b"BepInEx 5.4.23.5")
}

struct World {
    game: Tmp,
    profiles: Tmp,
}

impl World {
    fn new() -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), GAME);
        write(
            &game.join(&format!("{GAME}.exe")),
            &pe_with_options(X64, &[], b"game"),
        );
        World {
            game,
            profiles: Tmp::new("profiles"),
        }
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn on(&self, platform: Platform) -> GameTarget {
        target(
            self.game.path(),
            &format!("{GAME}.exe"),
            Some(APPID),
            platform,
        )
    }

    fn target(&self) -> GameTarget {
        self.on(Platform::Windows)
    }

    /// The folder a BepInEx install leaves beside the game, its proxy aside.
    fn bepinex_folder(&self) {
        write(
            &self.game.join("BepInEx/core/BepInEx.Preloader.dll"),
            b"their preloader",
        );
        write(
            &self.game.join("BepInEx/plugins/OldMod.dll"),
            b"a mod for last year's game",
        );
    }

    fn plan(&self, platform: Platform) -> Result<InstallPlan, LauncherError> {
        plan_install(
            &doorstop_runner(&ours()),
            &self.on(platform),
            &self.profile(),
        )
    }
}

fn actions(plan: &InstallPlan) -> Vec<(&str, GameAction)> {
    plan.game_files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect()
}

fn launch(w: &World) -> Vec<String> {
    prepare_launch(
        &w.profile(),
        &w.target(),
        &vars(),
        Some(std::path::Path::new("/opt/fake/steam")),
    )
    .unwrap()
    .args
}

// ---- Uninstall keeps a Doorstop their loader runs through. ----

#[test]
fn uninstall_leaves_our_doorstop_when_their_bepinex_config_replaced_ours() {
    // Astra added BepInEx's own winhttp.dll to a vanilla game; the person then unzipped that same
    // BepInEx by hand. Their winhttp.dll IS ours, byte for byte, so its digest says nothing.
    let w = World::new();
    plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    write(&w.game.join("doorstop_config.ini"), BEPINEX_INI);

    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), ours());
    assert!(report.kept.iter().any(|k| k == "winhttp.dll"), "{report:?}");
    assert!(!report.removed.iter().any(|r| r == "winhttp.dll"));
    assert_eq!(
        fs::read(w.game.join("doorstop_config.ini")).unwrap(),
        BEPINEX_INI
    );
}

#[test]
fn uninstall_leaves_our_doorstop_when_a_bepinex_appeared_beside_it() {
    // They kept our (switched-off) config when they unzipped BepInEx: its folder says it all.
    let w = World::new();
    plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    w.bepinex_folder();

    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), ours());
    assert_eq!(report.kept, vec!["winhttp.dll".to_owned()]);
    assert_eq!(report.removed, vec!["doorstop_config.ini".to_owned()]);
}

#[test]
fn with_nothing_of_theirs_beside_it_our_doorstop_goes() {
    let w = World::new();
    let before = snapshot(w.game.path());
    plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(report.kept.is_empty(), "{report:?}");
    assert_eq!(snapshot(w.game.path()), before);

    // An empty BepInEx/core is no BepInEx; their unchanged config that was there before us is
    // not a new one.
    let w = World::new();
    fs::create_dir_all(w.game.join("BepInEx/core")).unwrap();
    write(
        &w.game.join("doorstop_config.ini"),
        b"[General]\nenabled = false\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\n",
    );
    let before = snapshot(w.game.path());
    let plan = plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    assert_eq!(
        actions(&plan),
        vec![
            ("winhttp.dll", GameAction::Add),
            ("doorstop_config.ini", GameAction::KeepExisting),
        ]
    );
    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(report.removed, vec!["winhttp.dll".to_owned()]);
    assert_eq!(snapshot(w.game.path()), before);
}

#[test]
fn an_update_after_their_bepinex_arrived_keeps_the_proxy_and_is_not_stale() {
    // The plan must expect what uninstalling the old integration leaves, or every update would
    // answer plan-stale.
    let w = World::new();
    let pkg = doorstop_runner(&ours());
    plan_and_install(&pkg, &w.target(), &w.profile());
    write(&w.game.join("doorstop_config.ini"), BEPINEX_INI);
    w.bepinex_folder();

    let again = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(
        actions(&again),
        vec![
            ("winhttp.dll", GameAction::ReuseKnownProxy),
            ("doorstop_config.ini", GameAction::KeepExisting),
        ]
    );
    launch(&w);
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), ours());
}

#[test]
fn a_cancelled_install_takes_back_its_doorstop_even_beside_a_bepinex() {
    // A rollback undoes exactly what it placed a moment ago: the BepInEx beside it was there
    // before, switched off, and must stay switched off.
    let w = World::new();
    w.bepinex_folder();
    write(
        &w.game.join("doorstop_config.ini"),
        b"[General]\nenabled = false\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\n",
    );
    let before = snapshot(w.game.path());
    let pkg = doorstop_runner(&ours());
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    assert_eq!(plan.game_files[0].action, GameAction::Add);

    let placed = Cell::new(false);
    let cancel = || placed.get();
    let progress = |p: Progress| {
        if p.stage == "game-files" {
            placed.set(true);
        }
    };
    let ctx = Ctx {
        cancel: &cancel,
        progress: &progress,
    };
    let e = install(&pkg, &plan, &w.target(), ACCEPT, &ctx).unwrap_err();
    assert_eq!(e.code, codes::CANCELLED);
    assert!(placed.get(), "the proxy was placed before the cancel");
    assert_eq!(snapshot(w.game.path()), before);
}

#[test]
fn a_proxy_that_is_not_a_doorstop_is_never_kept_for_a_bepinex() {
    // `runner()` ships a proxy of another kind (think ReShade's dxgi.dll): no BepInEx runs
    // through it, so a BepInEx beside it is no reason to leave it.
    let w = World::new();
    plan_and_install(&runner(), &w.target(), &w.profile());
    w.bepinex_folder();
    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(
        report.removed.iter().any(|r| r == "winhttp.dll"),
        "{report:?}"
    );
    assert!(!w.game.join("winhttp.dll").exists());
}

#[test]
fn a_bepinex_core_holding_only_what_we_placed_there_is_not_theirs() {
    // An integration may put a file of its own into `${game}/BepInEx/core`: that folder is then
    // there because of us, and says nothing about anyone else's BepInEx.
    let m = runner_manifest(
        "\n[[game_files]]\nfrom = \"Ours.dll\"\nto = \"${game}/BepInEx/core/Ours.dll\"\n",
    )
    .replace(&sha(WINHTTP), &sha(&ours()));
    let pkg = open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", &ours()),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("Ours.dll", b"ours, in the game's BepInEx/core"),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]))
    .unwrap();
    let w = World::new();
    plan_and_install(&pkg, &w.target(), &w.profile());
    assert!(w.game.join("BepInEx/core/Ours.dll").is_file());
    // A plan made with our file still in that folder (an update) agrees.
    let again = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    assert_eq!(again.game_files[0].action, GameAction::Add);
    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(report.kept.is_empty(), "{report:?}");
    assert!(!w.game.join("winhttp.dll").exists());
}

// ---- A plan never brings switched-off mods back. ----

#[test]
fn a_bepinex_switched_off_by_removing_its_proxy_is_refused_not_revived() {
    let w = World::new();
    w.bepinex_folder();
    write(&w.game.join("doorstop_config.ini"), BEPINEX_INI);
    let before = snapshot(w.game.path());

    let e = w.plan(Platform::Windows).unwrap_err();
    assert_eq!(e.code, codes::SWITCHED_OFF_LOADER);
    assert_eq!(e.param("file"), Some("doorstop_config.ini"));
    assert_eq!(
        e.param("target"),
        Some("BepInEx\\core\\BepInEx.Preloader.dll")
    );
    assert_eq!(snapshot(w.game.path()), before, "a plan writes nothing");

    // The remedy the refusal names: switch that file off, or delete it.
    write(
        &w.game.join("doorstop_config.ini"),
        &String::from_utf8_lossy(BEPINEX_INI)
            .replace("enabled = true", "enabled = false")
            .into_bytes(),
    );
    let plan = w.plan(Platform::Windows).unwrap();
    assert_eq!(
        actions(&plan),
        vec![
            ("winhttp.dll", GameAction::Add),
            ("doorstop_config.ini", GameAction::KeepExisting),
        ]
    );
    fs::remove_file(w.game.join("doorstop_config.ini")).unwrap();
    let plan = w.plan(Platform::Windows).unwrap();
    assert_eq!(plan.game_files[1].action, GameAction::Add);
}

#[test]
fn their_config_is_read_the_way_doorstop_reads_it() {
    let preloader = "BepInEx/core/BepInEx.Preloader.dll";
    // (config, files beside the game, refused)
    let cases: &[(&str, &[&str], bool)] = &[
        (
            "[General]\nenabled = true\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\n",
            &[preloader],
            true,
        ),
        (
            "[general]\nENABLED = TRUE\ntarget_assembly = \"bepinex/CORE/bepinex.preloader.dll\"\n",
            &[preloader],
            true,
        ),
        // A missing `enabled` is Doorstop's default for a missing key: true.
        (
            "[General]\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\n",
            &[preloader],
            true,
        ),
        // Neither `true` nor `false` leaves Doorstop's built-in default: off.
        (
            "[General]\nenabled = 1\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\n",
            &[preloader],
            false,
        ),
        (
            "[General]\nenabled = false\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\n",
            &[preloader],
            false,
        ),
        // r2modman's and Thunderstore Mod Manager's: the target lives in their own profile.
        (
            "[General]\nenabled = true\ntarget_assembly=BepInEx\\core\\BepInEx.Preloader.dll\n",
            &[],
            false,
        ),
        // An empty target names nothing; a missing one is `Doorstop.dll`.
        (
            "[General]\nenabled = true\ntarget_assembly =\n",
            &[preloader],
            false,
        ),
        ("[General]\nenabled = true\n", &[preloader], false),
        ("[General]\nenabled = true\n", &["Doorstop.dll"], true),
        // Doorstop 3's section means nothing to the Doorstop 4 we add: no [General], so its
        // defaults (on, `Doorstop.dll`), which is not there.
        (
            "[UnityDoorstop]\nenabled=true\ntargetAssembly=BepInEx\\core\\BepInEx.Preloader.dll\n",
            &[preloader],
            false,
        ),
    ];
    for (ini, files, refused) in cases {
        let w = World::new();
        for f in *files {
            write(&w.game.join(f), b"x");
        }
        write(&w.game.join("doorstop_config.ini"), ini.as_bytes());
        let got = w.plan(Platform::Windows);
        assert_eq!(
            got.as_ref().err().map(|e| e.code),
            refused.then_some(codes::SWITCHED_OFF_LOADER),
            "{ini:?} with {files:?}: {got:?}"
        );
    }
}

#[test]
fn a_utf16_config_is_read_as_windows_reads_it() {
    let w = World::new();
    w.bepinex_folder();
    let mut wide = vec![0xFF, 0xFE];
    wide.extend(
        String::from_utf8_lossy(BEPINEX_INI)
            .encode_utf16()
            .flat_map(u16::to_le_bytes),
    );
    write(&w.game.join("doorstop_config.ini"), &wide);
    assert_eq!(
        w.plan(Platform::Windows).unwrap_err().code,
        codes::SWITCHED_OFF_LOADER
    );
}

#[cfg(unix)]
#[test]
fn an_absolute_target_under_proton_is_looked_up_through_the_drive_letters() {
    let w = World::new();
    w.bepinex_folder();
    let pfx = Tmp::new("pfx");
    let proton = || Platform::LinuxProton {
        prefix: Some(pfx.path().to_path_buf()),
    };
    let game = fs::canonicalize(w.game.path()).unwrap();
    let z = |rel: &str| {
        format!(
            "[General]\nenabled = true\ntarget_assembly = Z:{}\\{rel}\n",
            game.to_str().unwrap().replace('/', "\\")
        )
    };
    let refused = |ini: &str| {
        write(&w.game.join("doorstop_config.ini"), ini.as_bytes());
        match w.plan(proton()) {
            Err(e) if e.code == codes::SWITCHED_OFF_LOADER => true,
            Ok(_) => false,
            Err(e) => panic!("{e}"),
        }
    };
    assert!(refused(&z("BepInEx\\core\\BepInEx.Preloader.dll")));
    assert!(!refused(&z("BepInEx\\core\\Nothing.dll")));
    // The prefix maps C: and has no such file.
    fs::create_dir_all(pfx.join("drive_c")).unwrap();
    fs::create_dir_all(pfx.join("dosdevices")).unwrap();
    std::os::unix::fs::symlink("../drive_c", pfx.join("dosdevices/c:")).unwrap();
    assert!(!refused(
        "[General]\nenabled = true\ntarget_assembly = C:\\mods\\x.dll\n"
    ));
    write(&pfx.join("drive_c/mods/x.dll"), b"x");
    assert!(refused(
        "[General]\nenabled = true\ntarget_assembly = C:\\mods\\x.dll\n"
    ));
    // A drive nothing maps cannot be looked up: it counts as there.
    assert!(refused(
        "[General]\nenabled = true\ntarget_assembly = Q:\\mods\\x.dll\n"
    ));
}

#[test]
fn only_a_game_that_loads_the_proxy_and_only_a_doorstop_are_concerned() {
    let w = World::new();
    w.bepinex_folder();
    write(&w.game.join("doorstop_config.ini"), BEPINEX_INI);
    // A native Linux game never loads a winhttp.dll.
    w.plan(Platform::LinuxNative).unwrap();
    // A proxy that is not a Doorstop never reads doorstop_config.ini.
    plan_install(&runner(), &w.target(), &w.profile()).unwrap();
}

// ---- Their settings never decide Astra's session. ----

#[test]
fn a_config_that_is_not_ours_cannot_hold_the_session_at_a_debugger_or_swap_boot_config() {
    // Their Doorstop knows only the two options a share needs: the session options are not asked
    // of it (a Doorstop that does not know one also reads no such setting), and Play still works.
    let w = World::new();
    let theirs = pe_with_options(X64, DOORSTOP_V4, b"theirs");
    write(&w.game.join("winhttp.dll"), &theirs);
    write(
        &w.game.join("doorstop_config.ini"),
        b"[General]\nenabled = true\ntarget_assembly = elsewhere.dll\nboot_config_override = their.boot.config\n[UnityMono]\ndebug_enabled = true\ndebug_suspend = true\n",
    );
    let plan = plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    assert_eq!(plan.game_files[0].action, GameAction::ShareLoader);
    let tail: Vec<&str> = plan.launch_args[4..].iter().map(String::as_str).collect();
    assert_eq!(
        tail,
        vec![
            "--doorstop-mono-debug-enabled",
            "false",
            "--doorstop-mono-debug-suspend",
            "false",
            "--doorstop-boot-config-override",
            "${game}/Some Game_Data/boot.config",
        ]
    );

    let args = launch(&w);
    let value = |opt: &str| {
        let i = args.iter().position(|a| a == opt).unwrap();
        args[i + 1].clone()
    };
    assert_eq!(value("--doorstop-mono-debug-enabled"), "false");
    assert_eq!(value("--doorstop-mono-debug-suspend"), "false");
    let game = fs::canonicalize(w.game.path()).unwrap();
    assert_eq!(
        value("--doorstop-boot-config-override"),
        format!(
            "{}\\{GAME}_Data\\boot.config",
            game.to_str().unwrap().replace('/', "\\")
        ),
        "the game's own boot.config: Doorstop redirects it to itself"
    );
}
