// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What an older Astra left beside a game's program is recognised only by the exact digests the
//! Unreal runner 0.2.x shipped (`src/former.rs`, whose unit tests stand test-only bytes in for
//! them). Here, from outside, with the real digests: a look-alike of every one of them — a
//! ReShade-branded `dxgi.dll`, an `astra.addon64`, a `dwmapi.dll` with no UE4SS, an
//! `astra-reshade/` folder — is the player's, and nothing touches it. And a lost profile: what the
//! next install and Play do.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    ChoiceKind, GameAction, GameTarget, ModsEnv, Platform, codes, plan_install,
    prepare_launch_with, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
const APPID: u32 = 4_000_400;

/// A ReShade build: its version resource says so.
fn reshade(tag: &str) -> Vec<u8> {
    let mut b = pe_with_version(&[
        ("ProductName", "ReShade"),
        (
            "FileDescription",
            "crosire's ReShade post-processing injector",
        ),
        ("ProductVersion", "6.8.0"),
    ]);
    b.extend(tag.as_bytes());
    b
}

struct World {
    tmp: Tmp,
}

impl World {
    fn new() -> World {
        let w = World {
            tmp: Tmp::new("former"),
        };
        unity_game(&w.game(), GAME);
        w
    }
    fn game(&self) -> PathBuf {
        self.tmp.join("common").join(GAME)
    }
    fn profile(&self) -> PathBuf {
        self.tmp.join("profiles").join("p")
    }
    fn target(&self) -> GameTarget {
        target(
            &self.game(),
            &format!("{GAME}.exe"),
            Some(APPID),
            Platform::Windows,
        )
    }
}

/// The runner with the Unreal runner 0.2.x's two files beside the exe as well.
fn runner_with_reshade(reshade_bytes: &[u8], addon: &[u8]) -> astra_game_launcher::GiPackage {
    let extra = "\n[[game_files]]\nfrom = \"dxgi.dll\"\nto = \"${game}/dxgi.dll\"\n\
                 \n[[game_files]]\nfrom = \"astra.addon64\"\nto = \"${game}/astra.addon64\"\n";
    open(&zip(&[
        E::File("astra-gi.toml", runner_manifest(extra).as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("dxgi.dll", reshade_bytes),
        E::File("astra.addon64", addon),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]))
    .unwrap()
}

#[test]
fn nothing_but_exactly_what_astra_shipped_is_ever_touched() {
    let w = World::new();
    write(
        &w.game().join("dxgi.dll"),
        &reshade("a build of the player's"),
    );
    write(&w.game().join("astra.addon64"), b"MZ some astra.addon64");
    write(
        &w.game().join("dwmapi.dll"),
        b"MZ a dwmapi.dll, no UE4SS beside it",
    );
    write(&w.game().join("astra-reshade/Shaders/x.fx"), b"// theirs");
    let before = snapshot(&w.game());
    let plan = plan_install(&runner(), &w.target(), &w.profile()).unwrap();
    assert!(plan.former_files.is_empty());
    assert!(!plan.warnings.contains(&"former-astra-files".to_owned()));
    // A package with a dxgi.dll of its own: their ReShade is another loader, as it always was.
    let e = plan_install(
        &runner_with_reshade(&reshade("ours"), b"MZ add-on"),
        &w.target(),
        &w.profile(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::FOREIGN_LOADER, Some("dxgi.dll"))
    );
    plan_and_install(&runner(), &w.target(), &w.profile());
    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(r.former_removed.is_empty() && r.not_restored.is_empty());
    assert_eq!(snapshot(&w.game()), before);
}

/// A lost profile (its folder deleted, the ledger and the mods choice with it): the game folder
/// still holds the Unity runner's own two files. Play says nothing is installed; the next install
/// finds our very bytes there and reuses them (`reuse-known-proxy`: never removed, since nothing
/// can tell them from a BepInEx's own), and the next Play asks about the player's mods again.
#[test]
fn a_lost_profile_is_installed_again_and_asks_again() {
    let w = World::new();
    write(
        &w.game().join(format!("{GAME}.exe")),
        &pe_with_options(X64, &[], b"game"),
    );
    let ours = pe_with_options(X64, DOORSTOP_V4, b"4.5.0 ours");
    let pkg = mods_runner(&ours, MODS_DECL);
    plan_and_install(&pkg, &w.target(), &w.profile());
    let home = w.tmp.join("home");
    let theirs = home
        .join(".local/share/com.kesomannen.gale")
        .join(GAME)
        .join("profiles/Default");
    bepinex5_at(&theirs, "5.4.23.5");
    plugin_at(&theirs, "M/M.dll");
    let env = ModsEnv::linux(Some(&home), None, None);
    let play = || {
        prepare_launch_with(
            &w.profile(),
            &w.target(),
            &vars(),
            Some(Path::new("/opt/fake/steam")),
            &env,
        )
    };
    astra_game_launcher::set_mods_choice(
        &w.profile(),
        astra_game_launcher::Choice {
            choice: ChoiceKind::Without,
            source: None,
            added_roots: vec![],
        },
    )
    .unwrap();
    play().unwrap();

    fs::remove_dir_all(w.profile()).unwrap();
    assert_eq!(play().unwrap_err().code, codes::NOT_INSTALLED);
    let plan = plan_install(&pkg, &w.target(), &w.profile()).unwrap();
    let actions: Vec<(&str, GameAction)> = plan
        .game_files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect();
    assert_eq!(
        actions,
        [
            ("winhttp.dll", GameAction::ReuseKnownProxy),
            ("doorstop_config.ini", GameAction::ReuseKnownProxy)
        ]
    );
    plan_and_install(&pkg, &w.target(), &w.profile());
    let e = play().unwrap_err();
    assert_eq!(
        (e.code, e.param("sources")),
        (codes::MODS_CHOICE_NEEDED, Some("1"))
    );
}
