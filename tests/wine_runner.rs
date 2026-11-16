// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play under a Wine or Proton the caller chose (`Platform::LinuxWine`, Heroic's games): the
//! plan for each runner kind, nothing written into the prefix, the refusals before a spawn.
#![cfg(unix)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use astra_game_launcher::{
    GameTarget, LauncherError, Platform, ProcessPlan, RunnerKind, Spawn, WineRunner, codes,
    installed, launch_with, prepare_launch,
};
use common::*;

const GAME: &str = "Some Game";

struct World {
    game: Tmp,
    profiles: Tmp,
    prefix: Tmp,
    bins: Tmp,
    proton_dir: Tmp,
}

impl World {
    fn new() -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), GAME);
        let prefix = Tmp::new("heroic-pfx");
        fs::write(prefix.join("user.reg"), USER_REG).unwrap();
        fs::write(prefix.join("system.reg"), b"WINE REGISTRY Version 2\n").unwrap();
        World {
            game,
            profiles: Tmp::new("profiles"),
            prefix,
            bins: Tmp::new("bins"),
            proton_dir: Tmp::new("proton-dir"),
        }
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn bin(&self, name: &str) -> PathBuf {
        let p = self.bins.join(name);
        fs::write(&p, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn runner(&self, kind: RunnerKind, env: &[(&str, &str)]) -> WineRunner {
        WineRunner {
            kind,
            bin: self.bin("runner"),
            prefix: self.prefix.path().to_path_buf(),
            compat_data: None,
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn game(&self, runner: WineRunner) -> GameTarget {
        target(
            self.game.path(),
            &format!("{GAME}.exe"),
            None,
            Platform::LinuxWine { runner },
        )
    }

    fn installed(&self, game: &GameTarget) {
        plan_and_install(&runner(), game, &self.profile());
    }

    fn play(&self, game: &GameTarget) -> Result<ProcessPlan, LauncherError> {
        prepare_launch(&self.profile(), game, &vars(), None, &no_markers())
    }
}

fn env_of<'a>(p: &'a ProcessPlan, k: &str) -> Option<&'a str> {
    p.env
        .iter()
        .find(|(ek, _)| ek == k)
        .map(|(_, v)| v.as_str())
}

#[test]
fn a_wine_plan_runs_the_exe_with_the_prefix_and_the_merged_overrides() {
    let w = World::new();
    let r = w.runner(
        RunnerKind::Wine,
        &[
            ("WINEDLLOVERRIDES", "mscoree=d;winhttp=b"),
            ("DXVK_HUD", "0"),
            ("PROTON_LOG", "1"),
        ],
    );
    let game = w.game(r.clone());
    w.installed(&game);
    let p = w.play(&game).unwrap();

    assert_eq!(p.program, r.bin);
    assert!(p.args[0].ends_with(&format!("{GAME}.exe")), "{:?}", p.args);
    assert!(p.args.iter().all(|a| a != "run"));
    // The Doorstop options are the Proton flavour: Windows paths on drive Z.
    let target = p
        .args
        .iter()
        .position(|a| a == "--doorstop-target-assembly")
        .unwrap();
    assert!(p.args[target + 1].starts_with("Z:\\"), "{:?}", p.args);
    assert_eq!(
        env_of(&p, "WINEPREFIX"),
        Some(w.prefix.path().to_str().unwrap())
    );
    assert_eq!(env_of(&p, "DXVK_HUD"), Some("0"));
    // The manifest's `winhttp = native,builtin` replaces the caller's `winhttp=b`; `mscoree=d` stays.
    assert_eq!(
        env_of(&p, "WINEDLLOVERRIDES"),
        Some("mscoree=d;winhttp=n,b")
    );
    assert_eq!(p.cwd.as_deref(), Some(w.game.path()));
    assert!(p.play_arm().is_none(), "the play-arm is Steam-only");
}

#[test]
fn a_proton_plan_says_run_and_names_its_checked_compat_data() {
    let w = World::new();
    let data = Tmp::new("compat");
    let pfx = data.join("pfx");
    fs::create_dir(&pfx).unwrap();
    let mut r = w.runner(RunnerKind::Proton, &[]);
    r.prefix = pfx.clone();
    r.compat_data = Some(data.path().to_path_buf());
    let game = w.game(r.clone());
    w.installed(&game);
    let p = w.play(&game).unwrap();
    assert_eq!(p.program, r.bin);
    assert_eq!(p.args[0], "run");
    assert!(p.args[1].ends_with(&format!("{GAME}.exe")));
    assert_eq!(
        env_of(&p, "STEAM_COMPAT_DATA_PATH"),
        Some(data.path().to_str().unwrap())
    );
    assert_eq!(env_of(&p, "WINEPREFIX"), Some(pfx.to_str().unwrap()));
    assert_eq!(env_of(&p, "STEAM_COMPAT_CLIENT_INSTALL_PATH"), Some(""));
    assert_eq!(env_of(&p, "WINEDLLOVERRIDES"), Some("winhttp=n,b"));

    // The prefix may be the compat folder itself.
    let mut r2 = w.runner(RunnerKind::Proton, &[]);
    r2.compat_data = Some(w.prefix.path().to_path_buf());
    let p = w.play(&w.game(r2)).unwrap();
    assert_eq!(
        env_of(&p, "STEAM_COMPAT_DATA_PATH"),
        Some(w.prefix.path().to_str().unwrap())
    );
}

#[test]
fn the_prefix_in_the_environment_is_the_canonical_checked_one() {
    let w = World::new();
    let link = w.bins.join("pfx-link");
    std::os::unix::fs::symlink(w.prefix.path(), &link).unwrap();
    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = link;
    let g = w.game(r);
    w.installed(&g);
    let p = w.play(&g).unwrap();
    assert_eq!(
        env_of(&p, "WINEPREFIX"),
        Some(w.prefix.path().to_str().unwrap())
    );
}

#[test]
fn an_umu_plan_has_no_run_word_and_the_callers_proton() {
    let w = World::new();
    let pp = w.proton_dir.path().to_str().unwrap().to_owned();
    let r = w.runner(
        RunnerKind::Umu,
        &[("GAMEID", "umu-12345"), ("PROTONPATH", &pp)],
    );
    let game = w.game(r.clone());
    w.installed(&game);
    let p = w.play(&game).unwrap();
    assert_eq!(p.program, r.bin);
    assert!(p.args[0].ends_with(&format!("{GAME}.exe")));
    assert_eq!(env_of(&p, "GAMEID"), Some("umu-12345"));
    assert_eq!(env_of(&p, "PROTONPATH"), Some(pp.as_str()));
    assert_eq!(
        env_of(&p, "WINEPREFIX"),
        Some(w.prefix.path().to_str().unwrap())
    );
}

#[test]
fn an_appid_beside_a_wine_runner_never_starts_steam() {
    let w = World::new();
    let r = w.runner(RunnerKind::Wine, &[]);
    let mut game = w.game(r.clone());
    game.steam_appid = Some(4_000_001);
    w.installed(&game);
    let p = prepare_launch(&w.profile(), &game, &vars(), fake_steam(), &no_markers()).unwrap();
    assert_eq!(p.program, r.bin);
    assert!(p.play_arm().is_none());
}

struct Record(RefCell<Vec<ProcessPlan>>);
impl Spawn for Record {
    fn spawn(&self, plan: &ProcessPlan) -> Result<(), LauncherError> {
        self.0.borrow_mut().push(plan.clone());
        Ok(())
    }
}

#[test]
fn nothing_is_written_into_the_prefix_nor_recorded_as_proton() {
    let w = World::new();
    let game = w.game(w.runner(RunnerKind::Wine, &[]));
    w.installed(&game);
    let before = snapshot(w.prefix.path());
    let p = w.play(&game).unwrap();
    let rec = Record(RefCell::new(Vec::new()));
    launch_with(&p, &rec, &no_markers(), None).unwrap();
    assert_eq!(rec.0.borrow().len(), 1);
    assert_eq!(snapshot(w.prefix.path()), before, "the prefix is as it was");
    assert_eq!(
        fs::read_to_string(w.prefix.join("user.reg")).unwrap(),
        USER_REG
    );
    assert!(installed(&w.profile()).unwrap().unwrap().proton.is_empty());
    // And the manifest's override went to the process instead.
    assert!(
        env_of(&rec.0.borrow()[0], "WINEDLLOVERRIDES")
            .unwrap()
            .contains("winhttp=n,b")
    );
}

fn refused_with(w: &World, r: WineRunner, reason: &str, key: Option<&str>) {
    let game = w.game(r);
    let before = snapshot(w.prefix.path());
    let e = w.play(&game).unwrap_err();
    assert_eq!(e.code, codes::RUNNER_INVALID, "{reason}: {e:?}");
    assert_eq!(
        e.params.get("reason").map(String::as_str),
        Some(reason),
        "{e:?}"
    );
    if let Some(k) = key {
        assert_eq!(e.params.get("key").map(String::as_str), Some(k), "{e:?}");
    }
    assert_eq!(snapshot(w.prefix.path()), before);
}

fn refused(w: &World, r: WineRunner, reason: &str) {
    refused_with(w, r, reason, None);
}

#[test]
fn a_runner_that_cannot_start_a_game_is_refused_before_anything_is_spawned() {
    let w = World::new();
    w.installed(&w.game(w.runner(RunnerKind::Wine, &[])));

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.bin = PathBuf::from("wine");
    refused(&w, r, "bin-not-absolute");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.bin = w.bins.join("nothing");
    refused(&w, r, "bin-not-file");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    fs::set_permissions(&r.bin, fs::Permissions::from_mode(0o644)).unwrap();
    refused(&w, r.clone(), "bin-not-executable");
    r.bin = w.bins.path().to_path_buf();
    refused(&w, r, "bin-not-file");

    // A program the game or the profile holds is never the runner.
    let mut r = w.runner(RunnerKind::Wine, &[]);
    let inside = w.game.join("wine");
    fs::copy(&r.bin, &inside).unwrap();
    r.bin = inside;
    refused(&w, r, "bin-in-game");
    let mut r = w.runner(RunnerKind::Wine, &[]);
    let inside = w.profile().join("wine");
    fs::copy(&r.bin, &inside).unwrap();
    r.bin = inside;
    refused(&w, r, "bin-in-profile");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = PathBuf::from("rel/pfx");
    refused(&w, r, "prefix-not-absolute");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = w.prefix.join("gone");
    refused(&w, r, "prefix-missing");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = w.profile();
    refused(&w, r, "prefix-in-profile");
    // The profile inside the prefix, the other direction.
    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = w.profiles.path().to_path_buf();
    refused(&w, r, "profile-in-prefix");

    refused(
        &w,
        w.runner(RunnerKind::Wine, &[("wine_debug", "1")]),
        "env-key",
    );
    refused(&w, w.runner(RunnerKind::Wine, &[("A-B", "1")]), "env-key");
    refused(
        &w,
        w.runner(RunnerKind::Wine, &[("DXVK_HUD", "a\0b")]),
        "env-value",
    );
}

#[test]
fn the_fields_are_the_only_source_of_the_prefix_and_what_steers_the_runner_is_refused() {
    let w = World::new();
    w.installed(&w.game(w.runner(RunnerKind::Wine, &[])));
    let pfx = w.prefix.path().to_str().unwrap().to_owned();
    // Even a value equal to the checked prefix.
    for (k, v) in [
        ("WINEPREFIX", pfx.as_str()),
        ("STEAM_COMPAT_DATA_PATH", pfx.as_str()),
        ("WINELOADER", "/bin/sh"),
        ("WINESERVER", "/bin/sh"),
        ("LD_PRELOAD", "/x.so"),
        ("LD_LIBRARY_PATH", "/x"),
        ("PATH", "/x"),
        ("HOME", "/x"),
        ("XDG_DATA_HOME", "/x"),
        ("PYTHONPATH", "/x"),
        ("WINEFOO", "1"),
        ("WINE_FOO", "1"),
    ] {
        refused_with(
            &w,
            w.runner(RunnerKind::Wine, &[(k, v)]),
            "env-key",
            Some(k),
        );
    }
}

#[test]
fn the_manifest_env_cannot_steer_the_runner_but_may_merge_overrides() {
    let w = World::new();
    let with = |env: &str| {
        let m = runner_manifest("").replace(
            "proton_dll_overrides =",
            &format!("env = {{ {env} }}\nproton_dll_overrides ="),
        );
        open(&runner_zip(&m)).unwrap()
    };
    let game = w.game(w.runner(RunnerKind::Wine, &[]));
    for k in [
        "LD_PRELOAD",
        "PATH",
        "WINEPREFIX",
        "PROTON_X",
        "STEAM_COMPAT_DATA_PATH",
        "GAMEID",
        "UMU_X",
        "HOME",
    ] {
        let pkg = with(&format!("{k} = \"x\""));
        let dir = Tmp::new("prof");
        let profile = dir.join("p");
        plan_and_install(&pkg, &game, &profile);
        let before = snapshot(w.prefix.path());
        let e = prepare_launch(&profile, &game, &vars(), None, &no_markers()).unwrap_err();
        assert_eq!(e.code, codes::RUNNER_INVALID, "{k}: {e:?}");
        assert_eq!(
            e.params.get("reason").map(String::as_str),
            Some("manifest-env-key")
        );
        assert_eq!(e.params.get("key").map(String::as_str), Some(k));
        assert_eq!(snapshot(w.prefix.path()), before);
    }
    // An ordinary variable and WINEDLLOVERRIDES are fine, and the overrides merge.
    let pkg = with("DXVK_HUD = \"1\", WINEDLLOVERRIDES = \"mscoree=d\"");
    let dir = Tmp::new("prof2");
    let profile = dir.join("p");
    plan_and_install(&pkg, &game, &profile);
    let p = prepare_launch(&profile, &game, &vars(), None, &no_markers()).unwrap();
    assert_eq!(env_of(&p, "DXVK_HUD"), Some("1"));
    assert_eq!(
        env_of(&p, "WINEDLLOVERRIDES"),
        Some("mscoree=d;winhttp=n,b")
    );
}

#[test]
fn proton_needs_a_checked_compat_data_and_umu_a_checked_protonpath() {
    let w = World::new();
    w.installed(&w.game(w.runner(RunnerKind::Wine, &[])));
    let proton = |c: Option<PathBuf>| {
        let mut r = w.runner(RunnerKind::Proton, &[]);
        r.compat_data = c;
        r
    };
    refused(&w, proton(None), "compat-data-required");
    refused(
        &w,
        proton(Some(PathBuf::from("rel"))),
        "compat-data-not-absolute",
    );
    refused(
        &w,
        proton(Some(w.prefix.join("gone"))),
        "compat-data-missing",
    );
    refused(&w, proton(Some(w.profile())), "compat-data-in-profile");
    refused(
        &w,
        proton(Some(w.profiles.path().to_path_buf())),
        "profile-in-compat-data",
    );
    // A folder that neither is the prefix nor holds it as `pfx`.
    refused(
        &w,
        proton(Some(w.bins.path().to_path_buf())),
        "compat-data-mismatch",
    );

    let umu = |env: &[(&str, &str)]| w.runner(RunnerKind::Umu, env);
    let pp = w.proton_dir.path().to_str().unwrap();
    refused(&w, umu(&[("PROTONPATH", pp)]), "umu-gameid");
    refused(&w, umu(&[("GAMEID", "g")]), "umu-protonpath");
    refused(
        &w,
        umu(&[("GAMEID", "g"), ("PROTONPATH", "GE-Proton9")]),
        "umu-protonpath",
    );
    let gone = w.proton_dir.join("gone");
    refused(
        &w,
        umu(&[("GAMEID", "g"), ("PROTONPATH", gone.to_str().unwrap())]),
        "umu-protonpath",
    );
    let in_game = w.game.join("proton");
    fs::create_dir(&in_game).unwrap();
    refused(
        &w,
        umu(&[("GAMEID", "g"), ("PROTONPATH", in_game.to_str().unwrap())]),
        "protonpath-in-game",
    );
    let in_profile = w.profile().join("proton");
    fs::create_dir(&in_profile).unwrap();
    refused(
        &w,
        umu(&[
            ("GAMEID", "g"),
            ("PROTONPATH", in_profile.to_str().unwrap()),
        ]),
        "protonpath-in-profile",
    );
}

#[test]
fn the_runner_is_checked_again_right_before_the_spawn() {
    let w = World::new();
    let r = w.runner(RunnerKind::Wine, &[]);
    let game = w.game(r.clone());
    w.installed(&game);
    let p = w.play(&game).unwrap();
    // Between prepare and launch the runner stops being a program.
    fs::set_permissions(&r.bin, fs::Permissions::from_mode(0o644)).unwrap();
    let rec = Record(RefCell::new(Vec::new()));
    let e = launch_with(&p, &rec, &no_markers(), None).unwrap_err();
    assert_eq!(e.code, codes::RUNNER_INVALID);
    assert_eq!(
        e.params.get("reason").map(String::as_str),
        Some("bin-not-executable")
    );
    assert!(rec.0.borrow().is_empty());
}

#[test]
fn an_older_platform_or_target_still_reads_and_the_new_one_round_trips() {
    let old: Platform = serde_json::from_str(r#"{"kind":"linux-proton","prefix":null}"#).unwrap();
    assert_eq!(old, Platform::LinuxProton { prefix: None });
    let old: GameTarget = serde_json::from_str(
        r#"{"dir":"/g","exe":"G.exe","steam_appid":7,"platform":{"kind":"windows"}}"#,
    )
    .unwrap();
    assert_eq!(old.platform, Platform::Windows);

    // `env` may be left out of a runner.
    let w: Platform = serde_json::from_str(
        r#"{"kind":"linux-wine","runner":{"kind":"umu","bin":"/b","prefix":"/p"}}"#,
    )
    .unwrap();
    let Platform::LinuxWine { runner } = &w else {
        panic!("{w:?}")
    };
    assert_eq!(runner.kind, RunnerKind::Umu);
    assert!(runner.env.is_empty() && runner.compat_data.is_none());
    assert_eq!(
        serde_json::from_str::<Platform>(&serde_json::to_string(&w).unwrap()).unwrap(),
        w
    );
    assert_eq!(w.code(), Platform::LinuxProton { prefix: None }.code());

    // A plan made before this variant existed is still a plan.
    let p: ProcessPlan = serde_json::from_str(
        r#"{"program":"/steam","args":["-applaunch","1"],"env":[],"cwd":null}"#,
    )
    .unwrap();
    assert_eq!(p.program, PathBuf::from("/steam"));
}

#[test]
fn doorstops_winhttp_is_set_even_when_the_manifest_names_no_override() {
    let w = World::new();
    let manifest = runner_manifest("").replace(
        "proton_dll_overrides = { winhttp = \"native,builtin\" }",
        "",
    );
    assert!(!manifest.contains("proton_dll_overrides"));
    let pkg = open(&runner_zip(&manifest)).unwrap();
    let game = w.game(w.runner(RunnerKind::Wine, &[("WINEDLLOVERRIDES", "mscoree=d")]));
    plan_and_install(&pkg, &game, &w.profile());
    let p = w.play(&game).unwrap();
    assert_eq!(
        env_of(&p, "WINEDLLOVERRIDES"),
        Some("mscoree=d;winhttp=n,b")
    );
}

#[test]
fn a_listed_wine_toggle_is_the_callers_alone() {
    let w = World::new();
    let toggles = [
        "WINEDEBUG",
        "WINEESYNC",
        "WINEFSYNC",
        "WINE_LARGE_ADDRESS_AWARE",
        "WINE_FULLSCREEN_FSR",
        "WINE_FULLSCREEN_FSR_STRENGTH",
    ];
    // In `runner.env`: carried to the process.
    let env: Vec<(&str, &str)> = toggles.iter().map(|k| (*k, "1")).collect();
    let game = w.game(w.runner(RunnerKind::Wine, &env));
    w.installed(&game);
    let p = w.play(&game).unwrap();
    for k in toggles {
        assert_eq!(env_of(&p, k), Some("1"), "{k}");
    }
    // An unlisted one is still refused there.
    refused_with(
        &w,
        w.runner(RunnerKind::Wine, &[("WINEFOO", "1")]),
        "env-key",
        Some("WINEFOO"),
    );
    // In an integration's env, a listed one is refused.
    let m = runner_manifest("").replace(
        "proton_dll_overrides =",
        "env = { WINEDEBUG = \"-all\" }\nproton_dll_overrides =",
    );
    let dir = Tmp::new("prof3");
    let profile = dir.join("p");
    plan_and_install(&open(&runner_zip(&m)).unwrap(), &game, &profile);
    let e = prepare_launch(&profile, &game, &vars(), None, &no_markers()).unwrap_err();
    assert_eq!(
        e.params.get("reason").map(String::as_str),
        Some("manifest-env-key")
    );
    assert_eq!(e.params.get("key").map(String::as_str), Some("WINEDEBUG"));
}
