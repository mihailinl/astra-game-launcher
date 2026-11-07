// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play and Steam's updates (0.5.2). `prepare_launch` checks the game's folder, then `launch` asks
//! Steam to start the game, and Steam installs a pending update first: what was checked would not
//! be what starts, and an update may add an anti-cheat. So Play refuses `game-update-pending`
//! while the app's `StateFlags` carry an update bit, and `launch` asks the anti-cheat gate and
//! Steam's state again under the profile lock, right before the spawn (the daemon starts its
//! bridge engine between the two, for seconds).

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    AntiCheatMarker, AntiCheatMarkerKind, EngineSet, LauncherError, PLAY_ARM_FILE_NAME, Platform,
    ProcessPlan, STEAM_UPDATE_STATES, Spawn, codes, launch, prepare_launch, steam_app_state,
    steam_update_states,
};
use common::*;

/// The app these tests play; no real Steam holds it.
const APPID: u32 = 4_000_123;

/// A Unity game installed from a Steam library of its own: `<lib>/steamapps/common/Game`, with
/// the `appmanifest_<appid>.acf` that names it.
struct Lib {
    lib: Tmp,
    profiles: Tmp,
}

impl Lib {
    fn installed() -> Lib {
        let w = Lib {
            lib: Tmp::new("upd-lib"),
            profiles: Tmp::new("upd-profiles"),
        };
        unity_game(&w.dir(), "Game");
        w.state("4", "");
        plan_and_install(&runner(), &w.target(), &w.profile());
        w
    }

    fn dir(&self) -> PathBuf {
        self.lib.join("steamapps/common/Game")
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> astra_game_launcher::GameTarget {
        target(&self.dir(), "Game.exe", Some(APPID), Platform::Windows)
    }

    fn manifest(&self) -> PathBuf {
        self.lib.join(&format!("steamapps/appmanifest_{APPID}.acf"))
    }

    /// Steam's manifest for the game with `StateFlags` `flags` (left out when empty) and `extra`
    /// top-level lines.
    fn state(&self, flags: &str, extra: &str) {
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!("\t\"StateFlags\"\t\t\"{flags}\"\n")
        };
        write(
            &self.manifest(),
            format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"{APPID}\"\n\t\"name\"\t\t\"Game\"\n{flags}\t\"installdir\"\t\t\"Game\"\n{extra}\t\"UserConfig\"\n\t{{\n\t\t\"StateFlags\"\t\t\"6\"\n\t}}\n}}\n"
            )
            .as_bytes(),
        );
    }

    fn prepare(&self, set: &EngineSet) -> Result<ProcessPlan, LauncherError> {
        prepare_launch(
            &self.profile(),
            &self.target(),
            &vars(),
            Some(Path::new("/opt/fake/steam")),
            set,
        )
    }

    fn play(&self) -> Option<(String, Option<String>)> {
        self.prepare(&EngineSet::empty())
            .err()
            .map(|e| (e.code.to_owned(), e.params.get("state").cloned()))
    }

    fn arm(&self) -> PathBuf {
        self.profile().join(PLAY_ARM_FILE_NAME)
    }

    /// Every byte of the profile and of the library.
    fn bytes(&self) -> (BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<u8>>) {
        (snapshot(&self.profile()), snapshot(self.lib.path()))
    }
}

fn pending(state: &str) -> Option<(String, Option<String>)> {
    Some((
        codes::GAME_UPDATE_PENDING.to_owned(),
        Some(state.to_owned()),
    ))
}

/// A spawner that only records that it was asked.
struct Count(RefCell<usize>);

impl Spawn for Count {
    fn spawn(&self, _: &ProcessPlan) -> Result<(), LauncherError> {
        *self.0.borrow_mut() += 1;
        Ok(())
    }
}

/// The update bits, alone and as Steam combines them, refuse with their names; every other bit
/// — and a stale "app running" (this desk's Half-Life 2 episodes carry `68`), a download Steam
/// left counted in `BytesToDownload`, a nested `StateFlags` — plays. A refused Play writes
/// nothing. A game outside a library, or whose manifest gives no flags, is not refused on it.
///
/// Mutation (watched red): `prepare_launch_with` without its `refuse_update_pending` call.
#[test]
fn play_refuses_an_update_steam_has_not_finished() {
    let w = Lib::installed();
    assert_eq!(w.play(), None, "fully installed: it plays");
    for (bit, name) in STEAM_UPDATE_STATES {
        w.state(&(4 | bit).to_string(), "");
        let before = w.bytes();
        assert_eq!(w.play(), pending(name), "{bit}");
        assert!(w.bytes() == before, "a refused Play wrote nothing ({name})");
    }
    for (flags, state) in [
        ("6", "update-required"),
        ("1026", "update-required,update-started"),
        ("1542", "update-required,update-paused,update-started"),
        ("2", "update-required"),
    ] {
        w.state(flags, "");
        assert_eq!(w.play(), pending(state), "{flags}");
    }
    for flags in ["4", "68", "36", "132", "131076", "20", "4100", "0", " 4 "] {
        w.state(flags, "");
        assert_eq!(w.play(), None, "{flags}");
    }
    w.state(
        "4",
        "\t\"BytesToDownload\"\t\t\"957685\"\n\t\"BytesDownloaded\"\t\t\"0\"\n",
    );
    assert_eq!(w.play(), None, "a stale byte count alone");
    for unreadable in ["", "six", "-2", "99999999999"] {
        w.state(unreadable, "");
        assert_eq!(w.play(), None, "{unreadable:?}: not known, not refused");
    }
    // The reader: a game folder below the library's own folder, and outside any library.
    w.state("6", "");
    assert_eq!(steam_app_state(&w.dir(), APPID), Some(6));
    assert_eq!(steam_app_state(&w.dir().join("bin/x64"), APPID), Some(6));
    assert_eq!(steam_app_state(&w.dir(), APPID + 1), None);
    assert_eq!(steam_app_state(w.lib.path(), APPID), None);
    assert_eq!(
        steam_update_states(6 | 256),
        vec!["update-required", "update-running"]
    );
    assert!(steam_update_states(4 | 64 | 131_072).is_empty());
}

/// `launch` asks again, under the lock, right before the spawn: an update Steam started, an
/// anti-cheat that arrived and a marker the catalogue added since `prepare_launch` each refuse,
/// with nothing started and no play-arm written; once they are gone, the same plan starts.
///
/// Mutation (watched red): `launch` without its `gate.refuse(engines)` call.
#[test]
fn launch_asks_again_right_before_the_spawn() {
    let w = Lib::installed();
    let plan = w.prepare(&EngineSet::empty()).unwrap();
    let none = EngineSet::empty();
    let refused = |set: &EngineSet| {
        let spawn = Count(RefCell::new(0));
        let e = launch(&plan, &spawn, set).unwrap_err();
        assert_eq!(*spawn.0.borrow(), 0, "nothing started ({})", e.code);
        assert!(!w.arm().exists(), "no arm ({})", e.code);
        (e.code.to_owned(), e.params)
    };

    // Steam began the update in between.
    w.state("1030", "");
    let (code, params) = refused(&none);
    assert_eq!(code, codes::GAME_UPDATE_PENDING);
    assert_eq!(params.get("appid").map(String::as_str), Some("4000123"));
    w.state("4", "");

    // The update landed with an anti-cheat.
    write(&w.dir().join("EasyAntiCheat/readme.txt"), b"x");
    let (code, params) = refused(&none);
    assert_eq!(
        (code.as_str(), params.get("kind").map(String::as_str)),
        (codes::ANTI_CHEAT, Some("easy-anti-cheat"))
    );
    fs::remove_dir_all(w.dir().join("EasyAntiCheat")).unwrap();

    // The catalogue lists the game's guard now: the caller's set at launch is what counts.
    write(&w.dir().join("Guard/readme.txt"), b"x");
    let guard = EngineSet::empty().with_anti_cheat_markers(vec![AntiCheatMarker {
        kind: AntiCheatMarkerKind::FolderMarker,
        value: "guard".to_owned(),
    }]);
    let (code, params) = refused(&guard);
    assert_eq!(
        (code.as_str(), params.get("kind").map(String::as_str)),
        (codes::ANTI_CHEAT, Some(codes::LISTED_ANTI_CHEAT))
    );

    // Nothing in the way: the same plan starts, and the arm is written.
    let spawn = Count(RefCell::new(0));
    launch(&plan, &spawn, &none).unwrap();
    assert_eq!(*spawn.0.borrow(), 1);
    assert!(w.arm().is_file());
}

/// A game started without Steam is checked again too: its anti-cheat, under the lock (no arm, no
/// Steam state to ask).
#[test]
fn a_game_started_without_steam_is_checked_again_too() {
    let game = Tmp::new("upd-nosteam");
    unity_game(game.path(), "Game");
    let profiles = Tmp::new("upd-nosteam-profiles");
    let t = target(game.path(), "Game.exe", None, Platform::Windows);
    plan_and_install(&runner(), &t, &profiles.join("p"));
    let plan = prepare_launch(&profiles.join("p"), &t, &vars(), None, &no_markers()).unwrap();
    write(&game.join("BattlEye/readme.txt"), b"x");
    let spawn = Count(RefCell::new(0));
    let e = launch(&plan, &spawn, &no_markers()).unwrap_err();
    assert_eq!(
        (e.code, e.param("kind")),
        (codes::ANTI_CHEAT, Some("battleye"))
    );
    assert_eq!(*spawn.0.borrow(), 0);
    fs::remove_dir_all(game.join("BattlEye")).unwrap();
    launch(&plan, &spawn, &no_markers()).unwrap();
    assert_eq!(*spawn.0.borrow(), 1);
}

/// A plan read back from JSON carries no gate (it is never serialised), so `launch` spawns it as it
/// is and writes no arm: no plan made elsewhere can have `launch` write into a folder.
#[test]
fn a_plan_read_back_is_spawned_as_it_is() {
    let w = Lib::installed();
    let plan = w.prepare(&EngineSet::empty()).unwrap();
    let back: ProcessPlan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
    assert_eq!(back.play_arm(), None);
    w.state("6", "");
    let spawn = Count(RefCell::new(0));
    launch(&back, &spawn, &EngineSet::empty()).unwrap();
    assert_eq!(*spawn.0.borrow(), 1);
    assert!(!w.arm().exists());
}
