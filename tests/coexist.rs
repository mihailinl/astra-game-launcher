// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Living beside r2modman and Gale, which copy their profile's Doorstop (`winhttp.dll`) and
//! `doorstop_config.ini` into the game folder at every modded start, over the ones Astra added,
//! and beside ReShade, which saves the player's settings into the `ReShade.ini` Astra added. Play
//! records what is there now instead of sending the person back to Install (which would only
//! write ours back, to be overwritten again), and only once nothing can refuse it any more;
//! uninstall leaves what it recorded. And `launch` leaves `<profile>/play-arm.json` for the
//! runner, immediately before it asks Steam to start the game.

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use astra_game_launcher::{
    GameAction, GameTarget, LEDGER_FILE_NAME, LauncherError, Ledger, PLAY_ARM_FILE_NAME, Platform,
    ProcessPlan, Spawn, codes, installed, launch, prepare_launch, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
const APPID: u32 = 4_000_004;

/// The proxy our runner ships: a Doorstop 4 build.
fn ours() -> Vec<u8> {
    pe_with_options(X64, DOORSTOP_V4, b"4.3.0 ours")
}

/// The Doorstop a mod manager's profile carries (it also knows an option ours does not): it
/// takes every option our launch passes, so the game can run through it.
fn managers_proxy() -> Vec<u8> {
    pe_with_options(
        X64,
        &[DOORSTOP_V4[0], "--doorstop-bypass-vulkan", DOORSTOP_V4[1]],
        b"4.4.1 gale",
    )
}

/// The config a mod manager copies in beside its proxy: switched on, its target in its own
/// profile folder.
const MANAGERS_INI: &[u8] = b"[General]\nenabled = true\ntarget_assembly = C:\\Users\\me\\AppData\\Roaming\\com.kesomannen.gale\\some-game\\profiles\\Default\\BepInEx\\core\\BepInEx.Preloader.dll\n[UnityMono]\ndebug_enabled = true\ndebug_suspend = true\n";

struct World {
    game: Tmp,
    profiles: Tmp,
    /// The exe's file stem.
    name: String,
    platform: Platform,
}

impl World {
    /// A Windows Unity game named `name` whose exe is an x64 PE image, nothing installed.
    fn bare_as(name: &str, platform: Platform) -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), name);
        write(
            &game.join(&format!("{name}.exe")),
            &pe_with_options(X64, &[], b"game"),
        );
        World {
            game,
            profiles: Tmp::new("profiles"),
            name: name.to_owned(),
            platform,
        }
    }

    fn bare() -> World {
        World::bare_as(GAME, Platform::Windows)
    }

    /// Our runner installed: both game files ADDED by us.
    fn install_runner(self) -> World {
        let plan = plan_and_install(&doorstop_runner(&ours()), &self.target(), &self.profile());
        assert_eq!(
            actions(&plan.game_files),
            [
                ("winhttp.dll", GameAction::Add),
                ("doorstop_config.ini", GameAction::Add),
            ]
        );
        self
    }

    fn installed() -> World {
        World::bare().install_runner()
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> GameTarget {
        target(
            self.game.path(),
            &format!("{}.exe", self.name),
            Some(APPID),
            self.platform.clone(),
        )
    }

    /// What r2modman and Gale do at every modded start.
    fn manager_relinks(&self, proxy: &[u8]) {
        write(&self.game.join("winhttp.dll"), proxy);
        write(&self.game.join("doorstop_config.ini"), MANAGERS_INI);
    }

    fn ledger(&self) -> Ledger {
        installed(&self.profile()).unwrap().unwrap()
    }

    fn plan(&self) -> Result<ProcessPlan, LauncherError> {
        prepare_launch(
            &self.profile(),
            &self.target(),
            &vars(),
            steam(),
            &no_markers(),
        )
    }

    fn play_arm(&self) -> PathBuf {
        self.profile().join(PLAY_ARM_FILE_NAME)
    }
}

fn actions(files: &[astra_game_launcher::GameFileAction]) -> Vec<(&str, GameAction)> {
    files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect()
}

fn steam() -> Option<&'static Path> {
    Some(Path::new("/opt/fake/steam"))
}

/// Prepares a Play (no spawn): the arguments it would start the game with.
fn prepare(w: &World) -> Result<Vec<String>, LauncherError> {
    w.plan().map(|p| p.args)
}

fn entry<'a>(ledger: &'a Ledger, rel: &str) -> (&'a str, GameAction) {
    let g = ledger.game_files.iter().find(|g| g.rel == rel).unwrap();
    (g.sha256.as_str(), g.action)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A package like the runner, with one more game file `name` (bytes `bytes`) appended.
fn runner_with(name: &str, bytes: &[u8]) -> astra_game_launcher::GiPackage {
    let extra = format!("\n[[game_files]]\nfrom = \"{name}\"\nto = \"${{game}}/{name}\"\n");
    let m = runner_manifest(&extra).replace(&sha(WINHTTP), &sha(&ours()));
    open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", &ours()),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File(name, bytes),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]))
    .unwrap()
}

/// A spawner that starts nothing: it reads what the play-arm says at the moment it is asked to
/// start, or fails.
struct FakeSpawn {
    fail: bool,
    /// The play-arm's text at each spawn, `None` when there was none.
    seen: RefCell<Vec<Option<String>>>,
    arm: PathBuf,
}

impl FakeSpawn {
    fn new(w: &World, fail: bool) -> FakeSpawn {
        FakeSpawn {
            fail,
            seen: RefCell::new(Vec::new()),
            arm: w.play_arm(),
        }
    }
}

impl Spawn for FakeSpawn {
    fn spawn(&self, _plan: &ProcessPlan) -> Result<(), LauncherError> {
        self.seen
            .borrow_mut()
            .push(fs::read_to_string(&self.arm).ok());
        if self.fail {
            return Err(LauncherError::io("the spawn failed"));
        }
        Ok(())
    }
}

// ---- A relinked proxy and config are recorded, and Play goes on. ----

#[test]
fn a_managers_doorstop_and_config_copied_over_ours_are_recorded_and_play_goes_on() {
    let w = World::installed();
    w.manager_relinks(&managers_proxy());
    let before = snapshot(w.game.path());

    let args = prepare(&w).unwrap();

    // Whose they are now, with the digests there now.
    let ledger = w.ledger();
    let proxy_sha = sha(&managers_proxy());
    let ini_sha = sha(MANAGERS_INI);
    assert_eq!(
        entry(&ledger, "winhttp.dll"),
        (proxy_sha.as_str(), GameAction::ShareLoader)
    );
    assert_eq!(
        entry(&ledger, "doorstop_config.ini"),
        (ini_sha.as_str(), GameAction::KeepExisting)
    );
    let on_disk = fs::read_to_string(w.profile().join(LEDGER_FILE_NAME)).unwrap();
    assert!(on_disk.contains("\"schema\": 3"), "{on_disk}");

    // Their config is foreign: the session pins what it could otherwise decide.
    assert_eq!(
        &args[args.len() - 6..],
        [
            "--doorstop-mono-debug-enabled",
            "false",
            "--doorstop-mono-debug-suspend",
            "false",
            "--doorstop-boot-config-override",
            "Some Game_Data/boot.config",
        ],
        "{args:?}"
    );
    // And the command still chooses OUR profile's BepInEx.
    let target = args
        .iter()
        .position(|a| a == "--doorstop-target-assembly")
        .map(|i| &args[i + 1])
        .unwrap();
    assert!(target.contains("BepInEx.Preloader.dll"), "{args:?}");

    // Nothing in the game folder was written: theirs is never "repaired".
    assert_eq!(snapshot(w.game.path()), before);

    // The next Play is the same Play, and records nothing new.
    assert_eq!(prepare(&w).unwrap(), args);
    assert_eq!(w.ledger(), ledger);
}

#[test]
fn a_relinked_proxy_that_cannot_do_our_job_is_shared_loader_changed_never_needs_reinstall() {
    let cases = [
        (
            "doorstop 3",
            pe_with_options(X64, &["--doorstop-enable"], b"v3"),
        ),
        ("32-bit", pe_with_options(0x014C, DOORSTOP_V4, b"x86")),
        ("another loader", b"MZ another loader's proxy".to_vec()),
    ];
    for (why, proxy) in cases {
        let w = World::installed();
        let ledger = w.ledger();
        w.manager_relinks(&proxy);
        let e = prepare(&w).unwrap_err();
        assert_eq!(
            (e.code, e.param("file")),
            (codes::SHARED_LOADER_CHANGED, Some("winhttp.dll")),
            "{why}"
        );
        assert_eq!(
            w.ledger(),
            ledger,
            "{why}: nothing recorded for a refused Play"
        );
    }
}

#[test]
fn an_added_file_that_is_gone_or_any_other_that_changed_is_still_needs_reinstall() {
    let w = World::installed();
    fs::remove_file(w.game.join("winhttp.dll")).unwrap();
    let e = prepare(&w).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::NEEDS_REINSTALL, Some("winhttp.dll"))
    );

    let w = World::installed();
    fs::remove_file(w.game.join("doorstop_config.ini")).unwrap();
    let e = prepare(&w).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::NEEDS_REINSTALL, Some("doorstop_config.ini"))
    );

    // A game file of ours that is neither a loader proxy nor a config: one added, and one that
    // replaced the game's own.
    let pkg = runner_with("extra.dll", b"MZ our extra");
    for (why, game_has_one, action) in [
        ("added", false, GameAction::Add),
        ("replaced", true, GameAction::Replace),
    ] {
        let w = World::installed();
        if game_has_one {
            write(&w.game.join("extra.dll"), b"MZ the game's own");
        }
        let plan = plan_and_install(&pkg, &w.target(), &w.profile());
        assert_eq!(plan.game_files[2].action, action, "{why}");
        write(&w.game.join("extra.dll"), b"MZ changed since");
        let e = prepare(&w).unwrap_err();
        assert_eq!(
            (e.code, e.param("file")),
            (codes::NEEDS_REINSTALL, Some("extra.dll")),
            "{why}"
        );
    }
}

#[test]
fn after_recording_uninstall_leaves_both_files_exactly_as_they_are() {
    let w = World::installed();
    w.manager_relinks(&managers_proxy());
    prepare(&w).unwrap();
    let before = snapshot(w.game.path());

    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(
        report.removed.is_empty() && report.restored.is_empty(),
        "{report:?}"
    );
    assert_eq!(snapshot(w.game.path()), before);
    assert_eq!(
        fs::read(w.game.join("winhttp.dll")).unwrap(),
        managers_proxy()
    );
    assert_eq!(
        fs::read(w.game.join("doorstop_config.ini")).unwrap(),
        MANAGERS_INI
    );
}

#[test]
fn a_reshade_ini_reshade_rewrote_is_kept_and_has_no_say_in_the_doorstop_session() {
    const OURS: &[u8] = b"[GENERAL]\nEffectSearchPaths=.\\reshade-shaders\\Shaders\n";
    const SAVED: &[u8] = b"[GENERAL]\nEffectSearchPaths=.\\reshade-shaders\\Shaders\nPresetPath=.\\MyPreset.ini\n[OVERLAY]\nTutorialProgress=4\n";
    let w = World::bare();
    let plan = plan_and_install(&runner_with("ReShade.ini", OURS), &w.target(), &w.profile());
    assert_eq!(
        actions(&plan.game_files),
        [
            ("winhttp.dll", GameAction::Add),
            ("doorstop_config.ini", GameAction::Add),
            ("ReShade.ini", GameAction::Add),
        ]
    );
    let args = prepare(&w).unwrap();
    assert!(
        !args.iter().any(|a| a == "--doorstop-boot-config-override"),
        "our own Doorstop config: {args:?}"
    );

    // The player changes a setting in ReShade's overlay; ReShade saves it.
    write(&w.game.join("ReShade.ini"), SAVED);
    let before = snapshot(w.game.path());
    assert_eq!(
        prepare(&w).unwrap(),
        args,
        "ReShade.ini has no say in the Doorstop options"
    );
    let ledger = w.ledger();
    let (ours_sha, ini_sha, saved_sha) = (sha(&ours()), sha(DOORSTOP_INI), sha(SAVED));
    assert_eq!(
        entry(&ledger, "ReShade.ini"),
        (saved_sha.as_str(), GameAction::KeepExisting)
    );
    assert_eq!(
        entry(&ledger, "winhttp.dll"),
        (ours_sha.as_str(), GameAction::Add)
    );
    assert_eq!(
        entry(&ledger, "doorstop_config.ini"),
        (ini_sha.as_str(), GameAction::Add)
    );
    assert_eq!(snapshot(w.game.path()), before, "never rewritten");

    // The player's settings outlive the integration.
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(fs::read(w.game.join("ReShade.ini")).unwrap(), SAVED);
}

#[test]
fn a_config_found_with_our_very_bytes_and_rewritten_since_is_kept_too() {
    // The game already had a doorstop_config.ini with our bytes: `reuse-known-proxy`.
    let w = World::bare();
    write(&w.game.join("doorstop_config.ini"), DOORSTOP_INI);
    let plan = plan_and_install(&doorstop_runner(&ours()), &w.target(), &w.profile());
    assert_eq!(
        actions(&plan.game_files),
        [
            ("winhttp.dll", GameAction::Add),
            ("doorstop_config.ini", GameAction::ReuseKnownProxy),
        ]
    );
    write(&w.game.join("doorstop_config.ini"), MANAGERS_INI);
    let args = prepare(&w).unwrap();
    let ini_sha = sha(MANAGERS_INI);
    assert_eq!(
        entry(&w.ledger(), "doorstop_config.ini"),
        (ini_sha.as_str(), GameAction::KeepExisting)
    );
    assert!(
        args.iter().any(|a| a == "--doorstop-boot-config-override"),
        "theirs now: {args:?}"
    );
}

#[test]
fn a_changed_proxy_of_an_integration_that_does_not_launch_through_doorstop_is_needs_reinstall() {
    // A ReShade-style integration: its `dxgi.dll` is no Doorstop, and its launch passes no
    // Doorstop option. A changed one is not "another mod manager's loader".
    let ini_block =
        "[[game_files]]\nfrom = \"doorstop_config.ini\"\nto = \"${game}/doorstop_config.ini\"\n\n";
    let dxgi: &[u8] = b"MZ ReShade 6.3 dxgi";
    let m = runner_manifest("");
    assert!(m.contains(ini_block), "{m}");
    let m = m
        .replace(ini_block, "")
        .replace(
            "from = \"winhttp.dll\"\nto = \"${game}/winhttp.dll\"",
            "from = \"dxgi.dll\"\nto = \"${game}/dxgi.dll\"",
        )
        .replace(&sha(WINHTTP), &sha(dxgi));
    let args_line = m
        .lines()
        .find(|l| l.starts_with("args = "))
        .unwrap()
        .to_owned();
    let m = m.replace(&args_line, "args = [\"-dx11\"]");
    assert!(!m.contains("doorstop"), "{m}");
    let pkg = open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("dxgi.dll", dxgi),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]))
    .unwrap();
    let w = World::bare();
    let plan = plan_and_install(&pkg, &w.target(), &w.profile());
    assert_eq!(actions(&plan.game_files), [("dxgi.dll", GameAction::Add)]);
    prepare(&w).unwrap();

    write(&w.game.join("dxgi.dll"), b"MZ ReShade 6.4 dxgi");
    let ledger = w.ledger();
    let e = prepare(&w).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::NEEDS_REINSTALL, Some("dxgi.dll"))
    );
    assert_eq!(w.ledger(), ledger);
}

// ---- A refused Play records nothing. ----

#[test]
fn a_play_refused_further_down_the_game_files_leaves_no_partial_record() {
    // The config comes before the extra file: it is checked, and found theirs, first.
    let w = World::bare();
    plan_and_install(
        &runner_with("extra.dll", b"MZ our extra"),
        &w.target(),
        &w.profile(),
    );
    let ledger = w.ledger();
    write(&w.game.join("doorstop_config.ini"), MANAGERS_INI);
    write(&w.game.join("extra.dll"), b"MZ changed since");
    let e = prepare(&w).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::NEEDS_REINSTALL, Some("extra.dll"))
    );
    assert_eq!(w.ledger(), ledger, "no partial record");
}

#[test]
fn a_play_refused_after_the_game_files_were_checked_records_nothing() {
    let w = World::installed();
    w.manager_relinks(&managers_proxy());
    let ledger = w.ledger();

    // Steam is nowhere.
    let e = prepare_launch(&w.profile(), &w.target(), &vars(), None, &no_markers()).unwrap_err();
    assert_eq!(e.code, codes::STEAM_NOT_FOUND);
    assert_eq!(w.ledger(), ledger);

    // A config write that fails: the profile's cfg is not UTF-8.
    let cfg = w.profile().join("BepInEx/config/astra.cfg");
    write(&cfg, b"[Connection]\nPort = \xff\xfe\n");
    let e = prepare(&w).unwrap_err();
    assert_eq!(e.code, codes::IO, "{e:?}");
    assert_eq!(w.ledger(), ledger);
    assert!(!w.play_arm().exists());

    // Once nothing refuses, the same Play records it.
    fs::remove_file(&cfg).unwrap();
    prepare(&w).unwrap();
    assert_eq!(entry(&w.ledger(), "winhttp.dll").1, GameAction::ShareLoader);
}

#[cfg(target_os = "linux")]
#[test]
fn a_play_refused_because_the_game_is_running_records_nothing() {
    use std::os::unix::process::CommandExt;

    // A name of this test's own, so no other test's game counts as running.
    let name = format!("Coexist Probe {}", std::process::id());
    let w = World::bare_as(&name, Platform::Windows).install_runner();
    w.manager_relinks(&managers_proxy());
    let ledger = w.ledger();

    // The game, as Proton starts it: its Windows path as the first argument.
    let mut game = std::process::Command::new("sleep")
        .arg0(format!("Z:\\games\\{name}.exe"))
        .arg("30")
        .spawn()
        .unwrap();
    let exe = format!("{name}.exe");
    let seen = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        astra_game_launcher::is_running(&exe)
    });
    let refused = prepare(&w);
    game.kill().unwrap();
    game.wait().unwrap();
    assert!(seen, "the fake game never showed up as running");
    assert_eq!(refused.unwrap_err().code, codes::GAME_RUNNING);
    assert_eq!(w.ledger(), ledger, "nothing recorded for a refused Play");

    prepare(&w).unwrap();
    assert_eq!(
        entry(&w.ledger(), "doorstop_config.ini").1,
        GameAction::KeepExisting
    );
}

// ---- Uninstall after a recording. ----

#[test]
fn a_config_recorded_at_play_never_counts_as_ours_for_a_ledger_without_a_baseline() {
    // Their proxy has our very bytes (one BepInEx build), so only the config changed; the ledger
    // is from before 0.3.1 (no baseline). Their config is not ours: our proxy stays for them.
    let w = World::installed();
    write(&w.game.join("doorstop_config.ini"), MANAGERS_INI);
    prepare(&w).unwrap();
    assert_eq!(
        entry(&w.ledger(), "doorstop_config.ini").1,
        GameAction::KeepExisting
    );
    let path = w.profile().join(LEDGER_FILE_NAME);
    let mut ledger: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        ledger
            .as_object_mut()
            .unwrap()
            .remove("loader_baseline")
            .is_some()
    );
    fs::write(&path, serde_json::to_string(&ledger).unwrap()).unwrap();

    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(report.kept, vec!["winhttp.dll".to_owned()], "{report:?}");
    assert_eq!(fs::read(w.game.join("winhttp.dll")).unwrap(), ours());
}

#[cfg(unix)]
#[test]
fn under_proton_uninstall_keeps_the_override_that_loads_a_shared_proxy() {
    let pfx = Tmp::new("pfx");
    fs::write(pfx.join("user.reg"), USER_REG).unwrap();
    let w = World::bare_as(
        GAME,
        Platform::LinuxProton {
            prefix: Some(pfx.path().to_path_buf()),
        },
    )
    .install_runner();
    prepare(&w).unwrap();
    w.manager_relinks(&managers_proxy());
    prepare(&w).unwrap();
    assert_eq!(entry(&w.ledger(), "winhttp.dll").1, GameAction::ShareLoader);

    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(report.proton_overrides_kept, vec!["winhttp".to_owned()]);
    let reg = fs::read_to_string(pfx.join("user.reg")).unwrap();
    let section = format!("[Software\\\\Wine\\\\AppDefaults\\\\{GAME}.exe\\\\DllOverrides]");
    let at = reg.find(&section).expect("the game's section stays");
    assert!(
        reg[at..].contains("\"winhttp\"=\"native,builtin\""),
        "{reg}"
    );
    assert_eq!(
        fs::read(w.game.join("winhttp.dll")).unwrap(),
        managers_proxy()
    );
}

// ---- play-arm.json ----

#[test]
fn launch_writes_the_play_arm_immediately_before_it_asks_steam_to_start_the_game() {
    let w = World::installed();
    let plan = w.plan().unwrap();
    assert!(!w.play_arm().exists(), "prepare_launch writes none");
    assert_eq!(
        plan.play_arm().map(|(arm, appid)| (plain(&arm), appid)),
        Some((w.play_arm(), APPID))
    );

    let spawn = FakeSpawn::new(&w, false);
    let from = now_ms();
    launch(&plan, &spawn).unwrap();
    let to = now_ms();

    // Already there when Steam was asked.
    let seen = spawn.seen.borrow();
    let text = seen[0].as_deref().expect("written before the spawn");
    assert_eq!(fs::read_to_string(w.play_arm()).unwrap(), text);
    let arm: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(arm.as_object().unwrap().len(), 3, "{text}");
    assert_eq!(arm["schema"], 1, "{text}");
    assert_eq!(arm["appid"], APPID, "{text}");
    let started = arm["started_unix_ms"].as_u64().unwrap();
    assert!(
        from <= started && started <= to,
        "{from} <= {started} <= {to}"
    );

    // Not a file of the ledger's, and no reason for the next Play to refuse.
    let ledger = w.ledger();
    assert!(
        !ledger
            .files
            .iter()
            .any(|f| f.rel.eq_ignore_ascii_case(PLAY_ARM_FILE_NAME)),
        "{ledger:?}"
    );
    let again = w.plan().unwrap();
    launch(&again, &FakeSpawn::new(&w, false)).unwrap();
    let next: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(w.play_arm()).unwrap()).unwrap();
    assert!(next["started_unix_ms"].as_u64().unwrap() >= started);
    assert_eq!(w.ledger(), ledger);

    // Uninstall never deletes it: removing the profile folder is the caller's.
    uninstall(&w.profile(), &w.target()).unwrap();
    assert!(w.play_arm().is_file());
}

#[test]
fn a_refusal_between_prepare_and_launch_leaves_no_arm() {
    // Astra's daemon checks its bridge engine between the two; a refusal there never launches.
    let w = World::installed();
    let _plan = w.plan().unwrap();
    assert!(!w.play_arm().exists());
}

#[test]
fn a_spawn_that_fails_puts_back_what_was_there() {
    let w = World::installed();
    let e = launch(&w.plan().unwrap(), &FakeSpawn::new(&w, true)).unwrap_err();
    assert_eq!(e.code, codes::IO);
    assert!(
        !w.play_arm().exists(),
        "no arm for a launch that never happened"
    );

    launch(&w.plan().unwrap(), &FakeSpawn::new(&w, false)).unwrap();
    let earlier = fs::read(w.play_arm()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let failing = FakeSpawn::new(&w, true);
    launch(&w.plan().unwrap(), &failing).unwrap_err();
    assert!(
        failing.seen.borrow()[0].is_some(),
        "written before the spawn"
    );
    assert_eq!(fs::read(w.play_arm()).unwrap(), earlier);
}

#[test]
fn an_arm_that_cannot_be_written_fails_the_launch_before_anything_starts() {
    // A folder in its place.
    let w = World::installed();
    let plan = w.plan().unwrap();
    fs::create_dir_all(w.play_arm()).unwrap();
    let spawn = FakeSpawn::new(&w, false);
    launch(&plan, &spawn).unwrap_err();
    assert!(spawn.seen.borrow().is_empty(), "nothing was started");
}

#[cfg(unix)]
#[test]
fn an_arm_write_that_fails_or_a_link_in_its_place_starts_nothing() {
    use std::os::unix::fs::PermissionsExt;

    // The profile cannot be written to: the write itself fails.
    let w = World::installed();
    let plan = w.plan().unwrap();
    let profile = w.profile();
    let writable = fs::metadata(&profile).unwrap().permissions();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o555)).unwrap();
    let spawn = FakeSpawn::new(&w, false);
    let result = launch(&plan, &spawn);
    fs::set_permissions(&profile, writable).unwrap();
    result.unwrap_err();
    assert!(spawn.seen.borrow().is_empty(), "nothing was started");
    assert!(!w.play_arm().exists());

    // A link in its place is never followed nor replaced.
    let w = World::installed();
    let plan = w.plan().unwrap();
    let elsewhere = Tmp::new("elsewhere");
    let target = elsewhere.join("note.txt");
    fs::write(&target, b"not an arm").unwrap();
    std::os::unix::fs::symlink(&target, w.play_arm()).unwrap();
    let spawn = FakeSpawn::new(&w, false);
    launch(&plan, &spawn).unwrap_err();
    assert!(spawn.seen.borrow().is_empty(), "nothing was started");
    assert!(
        fs::symlink_metadata(w.play_arm())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), b"not an arm");
}

#[test]
fn a_game_without_a_steam_appid_gets_no_play_arm() {
    let game = Tmp::new("game");
    unity_game(game.path(), GAME);
    let profiles = Tmp::new("profiles");
    let profile = profiles.join("p");
    let t = target(game.path(), &format!("{GAME}.exe"), None, Platform::Windows);
    plan_and_install(&doorstop_runner(&ours()), &t, &profile);
    let plan = prepare_launch(&profile, &t, &vars(), None, &no_markers()).unwrap();
    assert_eq!(plan.program, game.join(&format!("{GAME}.exe")));
    assert_eq!(plan.play_arm(), None);
    let spawn = FakeSpawn {
        fail: false,
        seen: RefCell::new(Vec::new()),
        arm: profile.join(PLAY_ARM_FILE_NAME),
    };
    launch(&plan, &spawn).unwrap();
    assert_eq!(spawn.seen.borrow().as_slice(), [None]);
    assert!(!profile.join(PLAY_ARM_FILE_NAME).exists());
}
