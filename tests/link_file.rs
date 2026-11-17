// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Astra Link's discovery file (AstraPlugins `spec/games/link-v1.md` §2): `launch_with_link`
//! writes `<profile>/astra-link.json` under the profile lock, beside `play-arm.json` and by its
//! rule — atomically, `0600` on Unix, the exact JSON — and a Play that is refused or fails
//! leaves no fresh one; a Play without a link removes a stale one; a game the launcher starts
//! itself finds the file's path in `ASTRA_LINK_FILE`; no integration may place the name.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use astra_game_launcher::{
    EngineSet, GameTarget, LINK_FILE_ENV, LINK_FILE_NAME, LauncherError, LinkFile, Manifest,
    PLAY_ARM_FILE_NAME, Platform, ProcessPlan, Spawn, codes, launch, launch_with_link,
    prepare_launch,
};
use common::*;

const APPID: u32 = 4_000_321;

fn link() -> LinkFile {
    LinkFile {
        url: "ws://127.0.0.1:49152/link".to_owned(),
        token: "q8Zt-0123456789abcdefABCDEF_xyzQWERTYUIOPasdfg".to_owned(),
        integration: "someone.peak-astra".to_owned(),
        game: "892970".to_owned(),
        issued_unix_ms: 1_800_000_000_000,
        expires_unix_ms: 1_800_000_600_000,
    }
}

const EXACT: &str = r#"{"schema":1,"url":"ws://127.0.0.1:49152/link","token":"q8Zt-0123456789abcdefABCDEF_xyzQWERTYUIOPasdfg","integration":"someone.peak-astra","game":"892970","issued_unix_ms":1800000000000,"expires_unix_ms":1800000600000}"#;

/// A Steam game in a library (so Steam's update state can be changed under a prepared plan).
struct Lib {
    lib: Tmp,
    profiles: Tmp,
}

impl Lib {
    fn installed() -> Lib {
        let w = Lib {
            lib: Tmp::new("link-lib"),
            profiles: Tmp::new("link-profiles"),
        };
        unity_game(&w.dir(), "Game");
        w.state("4");
        plan_and_install(&runner(), &w.target(), &w.profile());
        w
    }

    fn dir(&self) -> PathBuf {
        self.lib.join("steamapps/common/Game")
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> GameTarget {
        target(&self.dir(), "Game.exe", Some(APPID), Platform::Windows)
    }

    fn state(&self, flags: &str) {
        write(
            &self.lib.join(&format!("steamapps/appmanifest_{APPID}.acf")),
            format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"{APPID}\"\n\t\"name\"\t\t\"Game\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"installdir\"\t\t\"Game\"\n}}\n"
            )
            .as_bytes(),
        );
    }

    fn plan(&self) -> ProcessPlan {
        prepare_launch(
            &self.profile(),
            &self.target(),
            &vars(),
            fake_steam(),
            &no_markers(),
        )
        .unwrap()
    }

    fn link_file(&self) -> PathBuf {
        self.profile().join(LINK_FILE_NAME)
    }

    fn arm(&self) -> PathBuf {
        self.profile().join(PLAY_ARM_FILE_NAME)
    }
}

/// A spawner that starts nothing: it records, at each spawn, the link file's text and the plan's
/// environment, or fails.
struct Seen {
    fail: bool,
    file: PathBuf,
    texts: RefCell<Vec<Option<String>>>,
    envs: RefCell<Vec<Vec<(String, String)>>>,
}

impl Seen {
    fn new(file: PathBuf, fail: bool) -> Seen {
        Seen {
            fail,
            file,
            texts: RefCell::new(Vec::new()),
            envs: RefCell::new(Vec::new()),
        }
    }
}

impl Spawn for Seen {
    fn spawn(&self, plan: &ProcessPlan) -> Result<(), LauncherError> {
        self.texts
            .borrow_mut()
            .push(fs::read_to_string(&self.file).ok());
        self.envs.borrow_mut().push(plan.env.clone());
        if self.fail {
            return Err(LauncherError::io("the spawn failed"));
        }
        Ok(())
    }
}

fn go(plan: &ProcessPlan, spawn: &Seen, link: Option<&LinkFile>) -> Result<(), LauncherError> {
    launch_with_link(plan, spawn, &EngineSet::empty(), None, link)
}

/// Every name in the profile's top folder.
fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn launch_writes_the_exact_link_file_privately_before_it_starts_the_game() {
    let w = Lib::installed();
    let plan = w.plan();
    assert_eq!(
        plan.link_file().map(|p| plain(&p)),
        Some(plain(
            &fs::canonicalize(w.profile()).unwrap().join(LINK_FILE_NAME)
        ))
    );
    assert!(!w.link_file().exists(), "prepare_launch writes none");
    let before: Vec<String> = names(&w.profile());

    let spawn = Seen::new(w.link_file(), false);
    go(&plan, &spawn, Some(&link())).unwrap();

    // There when the game was asked to start, byte for byte the spec's object.
    assert_eq!(spawn.texts.borrow()[0].as_deref(), Some(EXACT));
    assert_eq!(fs::read_to_string(w.link_file()).unwrap(), EXACT);
    assert_eq!(link().to_json(), EXACT.as_bytes());
    // Beside the arm, and nothing else left behind (no temporary file).
    let mut after = before.clone();
    after.extend([LINK_FILE_NAME.to_owned(), PLAY_ARM_FILE_NAME.to_owned()]);
    after.sort();
    after.dedup();
    assert_eq!(names(&w.profile()), after);
    // Steam is passed no environment: its game finds the file in its profile.
    assert_eq!(spawn.envs.borrow()[0], Vec::<(String, String)>::new());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(w.link_file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
}

/// Atomic: the file is replaced by a rename, never rewritten in place. A hard link to the old
/// file keeps the old bytes (an in-place write would change them through it), and a looser mode
/// is never carried over.
#[cfg(unix)]
#[test]
fn the_link_file_is_replaced_by_a_rename_never_rewritten_in_place() {
    use std::os::unix::fs::PermissionsExt;
    let w = Lib::installed();
    let old = b"{\"schema\":1,\"token\":\"old\"}";
    fs::write(w.link_file(), old).unwrap();
    fs::set_permissions(w.link_file(), fs::Permissions::from_mode(0o644)).unwrap();
    let elsewhere = w.profiles.join("hard-link");
    fs::hard_link(w.link_file(), &elsewhere).unwrap();
    go(&w.plan(), &Seen::new(w.link_file(), false), Some(&link())).unwrap();
    assert_eq!(fs::read(&elsewhere).unwrap(), old);
    assert_eq!(fs::read_to_string(w.link_file()).unwrap(), EXACT);
    let mode = fs::metadata(w.link_file()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
}

/// A Play refused under the lock (Steam began an update after `prepare_launch`) writes no link
/// file, and leaves an earlier one as it was; so does a link file that cannot be one of ours (a
/// folder in its place).
#[test]
fn a_refused_launch_leaves_no_fresh_link_file() {
    let w = Lib::installed();
    let plan = w.plan();
    w.state("1030");
    let spawn = Seen::new(w.link_file(), false);
    let e = go(&plan, &spawn, Some(&link())).unwrap_err();
    assert_eq!(e.code, codes::GAME_UPDATE_PENDING);
    assert!(spawn.texts.borrow().is_empty(), "nothing was started");
    assert!(!w.link_file().exists());
    assert!(!w.arm().exists());

    fs::write(w.link_file(), b"earlier").unwrap();
    go(&plan, &spawn, Some(&link())).unwrap_err();
    assert_eq!(fs::read(w.link_file()).unwrap(), b"earlier");
    w.state("4");

    // A folder at the name: refused before the link file or the arm is written.
    fs::remove_file(w.link_file()).unwrap();
    fs::create_dir(w.link_file()).unwrap();
    let e = go(&plan, &spawn, Some(&link())).unwrap_err();
    assert_eq!(e.code, codes::PATH_ESCAPES);
    assert!(spawn.texts.borrow().is_empty(), "nothing was started");
    assert!(w.link_file().is_dir() && !w.arm().exists());
}

/// A spawn that fails takes the fresh link file away; a write that fails starts nothing.
#[test]
fn a_launch_that_fails_takes_its_link_file_away() {
    let w = Lib::installed();
    let spawn = Seen::new(w.link_file(), true);
    go(&w.plan(), &spawn, Some(&link())).unwrap_err();
    assert_eq!(
        spawn.texts.borrow()[0].as_deref(),
        Some(EXACT),
        "written before the spawn"
    );
    assert!(
        !w.link_file().exists(),
        "no token for a Play that never happened"
    );

    // An earlier Play's file is not put back either: its token is spent.
    fs::write(w.link_file(), b"earlier").unwrap();
    go(&w.plan(), &Seen::new(w.link_file(), true), Some(&link())).unwrap_err();
    assert!(!w.link_file().exists());

    // A write that fails (the profile refuses new names): nothing starts, nothing is left.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let w = Lib::installed();
        let plan = w.plan();
        let profile = w.profile();
        let writable = fs::metadata(&profile).unwrap().permissions();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o555)).unwrap();
        let spawn = Seen::new(w.link_file(), false);
        let r = go(&plan, &spawn, Some(&link()));
        fs::set_permissions(&profile, writable).unwrap();
        r.unwrap_err();
        assert!(spawn.texts.borrow().is_empty(), "nothing was started");
        assert!(!w.link_file().exists() && !w.arm().exists());
    }
}

/// A Play without a link removes the file an earlier Play left (a link in its place is removed
/// as a link, never followed), with `launch` as with `launch_with_link(None)`.
#[test]
fn a_launch_without_a_link_removes_a_stale_one() {
    let w = Lib::installed();
    fs::write(w.link_file(), EXACT).unwrap();
    let spawn = Seen::new(w.link_file(), false);
    go(&w.plan(), &spawn, None).unwrap();
    assert_eq!(spawn.texts.borrow()[0], None, "gone before the game starts");
    assert!(!w.link_file().exists());

    fs::write(w.link_file(), EXACT).unwrap();
    launch(
        &w.plan(),
        &Seen::new(w.link_file(), false),
        &EngineSet::empty(),
    )
    .unwrap();
    assert!(!w.link_file().exists());

    #[cfg(unix)]
    {
        let target = w.profiles.join("someone-elses.json");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, w.link_file()).unwrap();
        go(&w.plan(), &Seen::new(w.link_file(), false), None).unwrap();
        assert!(fs::symlink_metadata(w.link_file()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
}

/// A plan read back (not made by `prepare_launch`) has no profile to write into.
#[test]
fn a_plan_read_back_carries_no_link_file() {
    let w = Lib::installed();
    let back: ProcessPlan =
        serde_json::from_str(&serde_json::to_string(&w.plan()).unwrap()).unwrap();
    assert_eq!(back.link_file(), None);
    let spawn = Seen::new(w.link_file(), false);
    let e = go(&back, &spawn, Some(&link())).unwrap_err();
    assert_eq!(
        (e.code, e.param("detail")),
        (codes::PLAN_STALE, Some("link-unprepared"))
    );
    assert!(spawn.texts.borrow().is_empty());
    go(&back, &spawn, None).unwrap();
    assert_eq!(spawn.texts.borrow().len(), 1);
    assert!(!w.link_file().exists());
}

/// A game the launcher starts itself gets the file's path in `ASTRA_LINK_FILE`; the variable is
/// the launcher's: an integration's `[launch] env` cannot set it, with a link file or without.
#[test]
fn a_game_started_directly_finds_the_link_file_in_its_environment() {
    let game = Tmp::new("link-direct");
    let profiles = Tmp::new("link-direct-profiles");
    let profile = profiles.join("p");
    let m = runner_manifest("").replacen(
        "proton_dll_overrides",
        "env = { ASTRA_LINK_FILE = \"${profile}/forged.json\", ASTRA_PORT = \"${bridge_port}\" }\nproton_dll_overrides",
        1,
    );
    let pkg = open(&runner_zip(&m)).unwrap();
    let t = target(game.path(), "Some Game.exe", None, Platform::LinuxNative);
    unity_game(game.path(), "Some Game");
    plan_and_install(&pkg, &t, &profile);
    let plan = prepare_launch(&profile, &t, &vars(), None, &no_markers()).unwrap();
    let file = plain(&fs::canonicalize(&profile).unwrap().join(LINK_FILE_NAME));
    let env = |spawn: &Seen| -> BTreeMap<String, Vec<String>> {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, v) in &spawn.envs.borrow()[0] {
            m.entry(k.clone()).or_default().push(v.clone());
        }
        m
    };

    let spawn = Seen::new(file.clone(), false);
    go(&plan, &spawn, Some(&link())).unwrap();
    let e = env(&spawn);
    assert_eq!(e[LINK_FILE_ENV], [file.to_str().unwrap().to_owned()]);
    assert_eq!(e["ASTRA_PORT"], [vars()["bridge_port"].clone()]);
    assert_eq!(spawn.texts.borrow()[0].as_deref(), Some(EXACT));
    // The plan itself is unchanged: `launch` spawns a copy.
    assert!(
        plan.env
            .iter()
            .any(|(k, v)| k == LINK_FILE_ENV && v.ends_with("forged.json"))
    );

    // A direct start that fails takes it away too.
    let spawn = Seen::new(file.clone(), true);
    go(&plan, &spawn, Some(&link())).unwrap_err();
    assert_eq!(spawn.texts.borrow()[0].as_deref(), Some(EXACT));
    assert!(!file.exists());

    let spawn = Seen::new(file.clone(), false);
    go(&plan, &spawn, None).unwrap();
    assert!(
        !env(&spawn).contains_key(LINK_FILE_ENV),
        "{:?}",
        env(&spawn)
    );
    assert!(!file.exists());
}

/// No integration may place `astra-link.json` in a profile: neither a file nor a config write.
#[test]
fn no_manifest_may_place_the_link_file() {
    for (what, extra) in [
        (
            "files",
            "\n[[files]]\nfrom = \"plugins/x.json\"\nto = \"${profile}/Astra-Link.JSON\"\n",
        ),
        (
            "config_writes",
            "\n[[config_writes]]\nfile = \"${profile}/astra-link.json\"\nsection = \"s\"\nset = { a = \"b\" }\n",
        ),
    ] {
        let e = Manifest::parse(&runner_manifest(extra)).unwrap_err();
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PATH_ESCAPES, Some("reserved")),
            "{what}"
        );
    }
}
