// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play: the Proton override in `user.reg`, the paths the game sees, the command, and the INI
//! keys written into the profile.

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    GameTarget, LauncherError, Platform, ProcessPlan, Spawn, codes, installed, launch,
    prepare_launch, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
const APPID: u32 = 4_000_001;

struct World {
    game: Tmp,
    profiles: Tmp,
    prefix: Tmp,
}

impl World {
    fn new() -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), GAME);
        let prefix = Tmp::new("pfx");
        fs::write(prefix.join("user.reg"), USER_REG).unwrap();
        World {
            game,
            profiles: Tmp::new("profiles"),
            prefix,
        }
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn proton(&self) -> GameTarget {
        target(
            self.game.path(),
            &format!("{GAME}.exe"),
            Some(APPID),
            Platform::LinuxProton {
                prefix: Some(self.prefix.path().to_path_buf()),
            },
        )
    }

    fn user_reg(&self) -> String {
        fs::read_to_string(self.prefix.join("user.reg")).unwrap()
    }

    fn installed(self, game: &GameTarget) -> Self {
        plan_and_install(&runner(), game, &self.profile());
        self
    }
}

fn steam() -> Option<&'static Path> {
    Some(Path::new("/opt/fake/steam"))
}

#[test]
fn proton_override_is_applied_and_restored_leaving_other_sections_alone() {
    let w = World::new();
    let game = w.proton();
    let w = w.installed(&game);
    let p = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();

    let reg = w.user_reg();
    let section = format!("[Software\\\\Wine\\\\AppDefaults\\\\{GAME}.exe\\\\DllOverrides] ");
    assert!(reg.contains(&section), "{reg}");
    assert!(
        reg.contains("\"winhttp\"=\"native,builtin\"\n")
    );
    // Every byte of the original is still there, in order, ahead of the new section.
    assert!(reg.starts_with(USER_REG.trim_end_matches('\n')));
    assert!(reg.contains("[Software\\\\Wine\\\\DllOverrides] 1784681205\n#time=1dcf9d1a3c4e000\n\"winhttp\"=\"builtin\"\n"));

    let ledger = installed(&w.profile()).unwrap().unwrap();
    let rec = ledger.proton.unwrap();
    assert!(rec.section_created && rec.previous.is_none());

    // A second Play changes nothing and keeps the record of what was there first.
    let p2 = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();
    assert_eq!(p, p2);
    assert_eq!(w.user_reg(), reg);

    let r = uninstall(&w.profile(), &game).unwrap();
    assert!(r.proton_restored);
    assert_eq!(
        w.user_reg(),
        USER_REG,
        "user.reg is byte-for-byte what it was"
    );
}

#[test]
fn a_value_already_in_the_section_is_put_back() {
    let w = World::new();
    let before = format!(
        "{USER_REG}\n[Software\\\\Wine\\\\AppDefaults\\\\{GAME}.exe\\\\DllOverrides] 1784681205\n#time=1dcf9d1a3c4e000\n\"d3d11\"=\"native\"\n\"winhttp\"=\"builtin\"\n"
    );
    fs::write(w.prefix.join("user.reg"), &before).unwrap();
    let game = w.proton();
    let w = w.installed(&game);
    prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();
    let reg = w.user_reg();
    assert!(reg.contains("\"d3d11\"=\"native\"\n\"winhttp\"=\"native,builtin\"\n"));
    let rec = installed(&w.profile()).unwrap().unwrap().proton.unwrap();
    assert_eq!(rec.previous.as_deref(), Some("\"builtin\""));
    uninstall(&w.profile(), &game).unwrap();
    assert_eq!(w.user_reg(), before);
}

#[test]
fn under_proton_the_profile_is_a_z_drive_path_and_steam_gets_applaunch() {
    let w = World::new();
    let game = w.proton();
    let w = w.installed(&game);
    let p = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();
    let profile = fs::canonicalize(w.profile()).unwrap();
    let z = format!(
        "Z:{}\\BepInEx\\core\\BepInEx.Preloader.dll",
        profile.to_str().unwrap().replace('/', "\\")
    );
    assert_eq!(
        p,
        ProcessPlan {
            program: PathBuf::from("/opt/fake/steam"),
            args: vec![
                "-applaunch".into(),
                APPID.to_string(),
                "--doorstop-enabled".into(),
                "true".into(),
                "--doorstop-target-assembly".into(),
                z,
            ],
            env: vec![],
            cwd: None,
        }
    );
}

#[test]
fn a_game_not_on_steam_is_started_directly_with_args_and_env() {
    let w = World::new();
    let m = runner_manifest("").replacen(
        "proton_dll_overrides",
        "env = { ASTRA_PROFILE = \"${profile}\", ASTRA_PORT = \"${bridge_port}\" }\nproton_dll_overrides",
        1,
    );
    let pkg = open(&runner_zip(&m)).unwrap();
    let game = target(
        w.game.path(),
        &format!("{GAME}.exe"),
        None,
        Platform::LinuxNative,
    );
    plan_and_install(&pkg, &game, &w.profile());
    let p = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();
    let profile = fs::canonicalize(w.profile()).unwrap();
    let game_dir = fs::canonicalize(w.game.path()).unwrap();
    assert_eq!(p.program, game_dir.join(format!("{GAME}.exe")));
    assert_eq!(p.cwd.as_deref(), Some(game_dir.as_path()));
    assert_eq!(
        p.args[3],
        format!("{}/BepInEx/core/BepInEx.Preloader.dll", profile.display())
    );
    assert_eq!(
        p.env,
        vec![
            ("ASTRA_PORT".to_owned(), "5555".to_owned()),
            ("ASTRA_PROFILE".to_owned(), profile.display().to_string()),
        ]
    );

    // A Steam game needs Steam.
    let steam_game = target(
        w.game.path(),
        &format!("{GAME}.exe"),
        Some(APPID),
        Platform::LinuxNative,
    );
    let e = prepare_launch(&w.profile(), &steam_game, &vars(), None).unwrap_err();
    assert_eq!(e.code, codes::STEAM_NOT_FOUND);

    // launch() hands the plan to the injected spawner, untouched.
    struct Record(RefCell<Vec<ProcessPlan>>);
    impl Spawn for Record {
        fn spawn(&self, plan: &ProcessPlan) -> Result<(), LauncherError> {
            self.0.borrow_mut().push(plan.clone());
            Ok(())
        }
    }
    let rec = Record(RefCell::new(Vec::new()));
    launch(&p, &rec).unwrap();
    assert_eq!(rec.0.borrow().as_slice(), std::slice::from_ref(&p));
}

#[test]
fn on_windows_a_path_argument_uses_backslashes() {
    let w = World::new();
    let game = target(
        w.game.path(),
        &format!("{GAME}.exe"),
        Some(APPID),
        Platform::Windows,
    );
    let w = w.installed(&game);
    let p = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();
    assert!(
        p.args[5].ends_with("\\BepInEx\\core\\BepInEx.Preloader.dll"),
        "{}",
        p.args[5]
    );
    assert_eq!(&p.args[..2], &["-applaunch".to_owned(), APPID.to_string()]);
    assert!(w.user_reg() == USER_REG, "no Proton, no user.reg edit");
}

#[test]
fn config_writes_into_a_missing_ini_and_an_existing_one() {
    let w = World::new();
    let game = w.proton();
    let w = w.installed(&game);
    let cfg = w.profile().join("BepInEx/config/astra.cfg");
    assert!(!cfg.exists());
    prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap();
    assert_eq!(
        fs::read_to_string(&cfg).unwrap(),
        "[Connection]\nPort = 5555\nToken = s3cret\n"
    );

    // BepInEx rewrote it with its own comments and another section; a new token arrives.
    let bepinex = "## Settings file was created by plugin Astra v0.5.0\n\n[Connection]\n\n## The port\n# Setting type: Int32\n# Default value: 0\nPort = 5555\n\n## The token\nToken = s3cret\n\n[Logging]\nLevel = Info\n";
    fs::write(&cfg, bepinex).unwrap();
    let mut v = vars();
    v.insert("bridge_token".into(), "n3w".into());
    prepare_launch(&w.profile(), &game, &v, steam()).unwrap();
    assert_eq!(
        fs::read_to_string(&cfg).unwrap(),
        bepinex.replace("Token = s3cret", "Token = n3w")
    );
}

#[test]
fn variables_must_all_be_given_and_be_safe() {
    let w = World::new();
    let game = w.proton();
    let w = w.installed(&game);
    let mut v = vars();
    v.remove("bridge_token");
    let e = prepare_launch(&w.profile(), &game, &v, steam()).unwrap_err();
    assert_eq!(
        (e.code, e.param("name")),
        (codes::MISSING_VARIABLE, Some("bridge_token"))
    );

    let mut v = vars();
    v.insert("bridge_token".into(), "x\n[Evil]\nk=v".into());
    let e = prepare_launch(&w.profile(), &game, &v, steam()).unwrap_err();
    assert_eq!(
        (e.code, e.param("name")),
        (codes::INVALID_VARIABLE, Some("bridge_token"))
    );
    assert!(
        !w.profile().join("BepInEx/config/astra.cfg").exists(),
        "refused before writing"
    );
}

#[test]
fn a_missing_prefix_is_refused_before_anything_is_written() {
    let w = World::new();
    let gone = w.prefix.join("not-yet");
    let game = target(
        w.game.path(),
        &format!("{GAME}.exe"),
        Some(APPID),
        Platform::LinuxProton { prefix: Some(gone) },
    );
    let w = w.installed(&game);
    let e = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap_err();
    assert_eq!(e.code, codes::PROTON_PREFIX_MISSING);
    assert!(!w.profile().join("BepInEx/config/astra.cfg").exists());
}

#[test]
fn a_damaged_install_is_not_launched() {
    let w = World::new();
    let game = w.proton();
    let w = w.installed(&game);
    fs::remove_file(w.game.join("winhttp.dll")).unwrap();
    let e = prepare_launch(&w.profile(), &game, &vars(), steam()).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::NEEDS_REINSTALL, Some("winhttp.dll"))
    );

    let p = Tmp::new("empty");
    let e = prepare_launch(p.path(), &game, &vars(), steam()).unwrap_err();
    assert_eq!(e.code, codes::NOT_INSTALLED);
}
