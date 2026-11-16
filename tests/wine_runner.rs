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
fn a_proton_plan_says_run_and_names_its_compat_data() {
    let w = World::new();
    // A prefix named `pfx`: Proton's data folder is its parent.
    let data = Tmp::new("compat");
    let pfx = data.join("pfx");
    fs::create_dir(&pfx).unwrap();
    let mut r = w.runner(RunnerKind::Proton, &[]);
    r.prefix = pfx.clone();
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
    assert_eq!(env_of(&p, "STEAM_COMPAT_CLIENT_INSTALL_PATH"), Some(""));
    assert_eq!(env_of(&p, "WINEDLLOVERRIDES"), Some("winhttp=n,b"));

    // Any other prefix is the data folder itself (Proton makes its own `pfx` in it), and the
    // caller's own value always wins.
    let w2 = World::new();
    let mut r = w2.runner(RunnerKind::Proton, &[]);
    let g = w2.game(r.clone());
    w2.installed(&g);
    let p = w2.play(&g).unwrap();
    assert_eq!(
        env_of(&p, "STEAM_COMPAT_DATA_PATH"),
        Some(w2.prefix.path().to_str().unwrap())
    );
    r.env = vec![("STEAM_COMPAT_DATA_PATH".into(), "/elsewhere".into())];
    let g = w2.game(r);
    assert_eq!(
        env_of(&w2.play(&g).unwrap(), "STEAM_COMPAT_DATA_PATH"),
        Some("/elsewhere")
    );
}

#[test]
fn an_umu_plan_has_no_run_word_and_keeps_the_callers_prefix_variable() {
    let w = World::new();
    let r = w.runner(
        RunnerKind::Umu,
        &[("GAMEID", "umu-12345"), ("WINEPREFIX", "/their/pfx")],
    );
    let game = w.game(r.clone());
    w.installed(&game);
    let p = w.play(&game).unwrap();
    assert_eq!(p.program, r.bin);
    assert!(p.args[0].ends_with(&format!("{GAME}.exe")));
    assert_eq!(env_of(&p, "GAMEID"), Some("umu-12345"));
    assert_eq!(env_of(&p, "WINEPREFIX"), Some("/their/pfx"));
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

fn refused(w: &World, r: WineRunner, reason: &str) {
    let game = w.game(r);
    let before = snapshot(w.prefix.path());
    let e = w.play(&game).unwrap_err();
    assert_eq!(e.code, codes::RUNNER_INVALID, "{e:?}");
    assert_eq!(
        e.params.get("reason").map(String::as_str),
        Some(reason),
        "{e:?}"
    );
    assert_eq!(snapshot(w.prefix.path()), before);
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

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = PathBuf::from("rel/pfx");
    refused(&w, r, "prefix-not-absolute");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = w.prefix.join("gone");
    refused(&w, r, "prefix-missing");

    let mut r = w.runner(RunnerKind::Wine, &[]);
    r.prefix = w.profile();
    refused(&w, r, "prefix-in-profile");

    let r = w.runner(RunnerKind::Wine, &[("wine_debug", "1")]);
    refused(&w, r, "env-key");
    let r = w.runner(RunnerKind::Wine, &[("A-B", "1")]);
    refused(&w, r, "env-key");
    let r = w.runner(RunnerKind::Wine, &[("WINEDEBUG", "a\0b")]);
    refused(&w, r, "env-value");

    refused(&w, w.runner(RunnerKind::Umu, &[]), "umu-gameid");
    refused(
        &w,
        w.runner(RunnerKind::Umu, &[("GAMEID", "")]),
        "umu-gameid",
    );
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
    assert!(runner.env.is_empty());
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
