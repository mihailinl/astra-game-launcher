// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Canary C1's declared exceptions (`tests/corpus/divergence.txt`), each pinned by a fixture:
//! where 0.5.0 answers otherwise than launcher 0.4.1 (main 73bf11e), it answers exactly as the
//! row says, and 0.4.1 exactly as its column says. Launcher 0.4.1 runs in a child process — this
//! same test binary, re-run as its `legacy_worker` — under a timeout, because on a FIFO it
//! blocks for good. The rows and the fixtures must name the same set.

#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::ffi::CString;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use astra_game_launcher::{Detection, detect_with};
use common::{E, Tmp, sha, write, zip};

const WORKER_ENV: &str = "AGL_LEGACY_WORKER";
const MARK: &str = "AGL-LEGACY ";

/// How long 0.4.1 may take before it is taken to block. Everything it answers here takes
/// milliseconds; a FIFO opened for reading with no writer never returns.
const BLOCKED_AFTER: Duration = Duration::from_secs(5);

/// Not a test of its own: the child process [`legacy`] starts. Runs 0.4.1's `detect_with` on
/// the folder and hint in [`WORKER_ENV`] and prints the answer's JSON. Without the variable it
/// does nothing.
#[test]
fn legacy_worker() {
    let Ok(job) = std::env::var(WORKER_ENV) else {
        return;
    };
    let (dir, hint) = job.split_once('\n').unwrap_or((&job, ""));
    let hint = (!hint.is_empty()).then_some(hint);
    let d = launcher_legacy::detect_with(Path::new(dir), hint).unwrap();
    println!("{MARK}{}", serde_json::to_string(&d).unwrap());
}

enum Legacy {
    Answered(String),
    Blocked,
}

/// 0.4.1's answer for `dir`, from a child process killed after [`BLOCKED_AFTER`].
fn legacy(dir: &Path, hint: Option<&str>) -> Legacy {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "legacy_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            WORKER_ENV,
            format!("{}\n{}", dir.display(), hint.unwrap_or("")),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            let out = child.wait_with_output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            let line = text
                .lines()
                .find_map(|l| l.split_once(MARK).map(|(_, json)| json))
                .unwrap_or_else(|| panic!("the legacy worker printed no answer: {text}"));
            return Legacy::Answered(line.to_owned());
        }
        if started.elapsed() > BLOCKED_AFTER {
            child.kill().unwrap();
            child.wait().unwrap();
            return Legacy::Blocked;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn answered(l: Legacy) -> Detection {
    match l {
        Legacy::Answered(json) => serde_json::from_str(&json).unwrap(),
        Legacy::Blocked => panic!("0.4.1 blocked where it should answer"),
    }
}

fn json(d: &Detection) -> String {
    serde_json::to_string(d).unwrap()
}

fn fifo(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let c = CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path, as mkfifo wants.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
}

/// A Unity Mono game at the top of `dir`: `Game.exe`, its player, its data.
fn mono_game(dir: &Path) {
    write(&dir.join("Game.exe"), b"MZ game");
    write(&dir.join("UnityPlayer.dll"), b"MZ player");
    write(&dir.join("Game_Data/Managed/Assembly-CSharp.dll"), b"MZ cs");
}

/// 0.5.0 on `dir` (built by `lay`) against 0.4.1 on the same tree built by `oracle_lay`: equal.
fn as_if(name: &str, hint: Option<&str>, lay: impl Fn(&Path), oracle_lay: impl Fn(&Path)) {
    let t = Tmp::new(name);
    lay(t.path());
    let new = detect_with(t.path(), hint).unwrap();
    let o = Tmp::new(&format!("{name}-oracle"));
    oracle_lay(o.path());
    let oracle = answered(legacy(o.path(), hint));
    assert_eq!(
        json(&new),
        json(&oracle),
        "{name}: 0.5.0 answers as the row says"
    );
}

/// Each row's fixture, by id. Returns the ids it pinned.
fn fixtures() -> BTreeSet<&'static str> {
    let mut pinned = BTreeSet::new();

    // fifo-top-program
    {
        let t = Tmp::new("div-fifo-top");
        fifo(&t.join("Game.exe"));
        assert!(
            matches!(legacy(t.path(), None), Legacy::Blocked),
            "0.4.1 blocks on the FIFO"
        );
        as_if(
            "div-fifo-top",
            None,
            |d| fifo(&d.join("Game.exe")),
            |d| write(&d.join("Game.exe"), b""),
        );
        let got = detect_with(t.path(), None).unwrap();
        assert!(got.engine.is_unknown() && got.exe.as_deref() == Some(Path::new("Game.exe")));
        pinned.insert("fifo-top-program");
    }

    // fifo-version-header
    {
        let lay = |d: &Path, header: &dyn Fn(&Path)| {
            mono_game(d);
            header(&d.join("Game_Data/globalgamemanagers"));
        };
        let t = Tmp::new("div-fifo-header");
        lay(t.path(), &|p| fifo(p));
        assert!(matches!(legacy(t.path(), None), Legacy::Blocked));
        as_if(
            "div-fifo-header",
            None,
            |d| lay(d, &|p| fifo(p)),
            |d| lay(d, &|p| write(p, b"")),
        );
        let got = detect_with(t.path(), None).unwrap();
        assert_eq!(
            (got.engine.as_str(), got.unity_version),
            ("unity-mono", None)
        );
        pinned.insert("fifo-version-header");
    }

    // link-version-header
    {
        let outside = Tmp::new("div-link-header-outside");
        write(&outside.join("ggm"), b"\0\0\x162021.3.16f1\0");
        let target = outside.join("ggm");
        let t = Tmp::new("div-link-header");
        mono_game(t.path());
        symlink(&target, t.join("Game_Data/globalgamemanagers")).unwrap();
        let old = answered(legacy(t.path(), None));
        assert_eq!(
            old.unity_version.as_deref(),
            Some("2021.3.16f1"),
            "0.4.1 follows"
        );
        as_if(
            "div-link-header",
            None,
            |d| {
                mono_game(d);
                symlink(&target, d.join("Game_Data/globalgamemanagers")).unwrap();
            },
            mono_game,
        );
        pinned.insert("link-version-header");
    }

    // link-in-a-stat
    {
        let outside = Tmp::new("div-link-stat-outside");
        write(&outside.join("Assembly-CSharp.dll"), b"MZ cs");
        let target = outside.path().to_path_buf();
        let lay = |d: &Path| {
            write(&d.join("Game.exe"), b"MZ game");
            write(&d.join("UnityPlayer.dll"), b"MZ player");
            std::fs::create_dir_all(d.join("Game_Data")).unwrap();
            symlink(&target, d.join("Game_Data/Managed")).unwrap();
        };
        let t = Tmp::new("div-link-stat");
        lay(t.path());
        let old = answered(legacy(t.path(), None));
        assert_eq!(old.engine.as_str(), "unity-mono", "0.4.1 follows");
        as_if("div-link-stat", None, lay, |d| {
            write(&d.join("Game.exe"), b"MZ game");
            write(&d.join("UnityPlayer.dll"), b"MZ player");
            std::fs::create_dir_all(d.join("Game_Data")).unwrap();
        });
        assert!(detect_with(t.path(), None).unwrap().engine.is_unknown());
        pinned.insert("link-in-a-stat");
    }

    // link-program-folder
    {
        let t = Tmp::new("div-link-program");
        mono_game(t.path());
        symlink(".", t.join("LinkedDir")).unwrap();
        let profile = Tmp::new("div-link-program-profile");
        let manifest = "schema = 1\n\n[integration]\nid = \"probe.engine\"\nname = \"Probe\"\nversion = \"1.0.0\"\n\n[target]\nengine = \"unity-mono\"\nexe = \"*\"\nplatforms = [\"linux-x64\"]\nanti_cheat = \"none\"\n";
        let z = zip(&[E::File("astra-gi.toml", manifest.as_bytes())]);
        let exe = Path::new("LinkedDir/Game.exe");
        let old = {
            use launcher_legacy::{GameTarget, GiPackage, Platform, plan_install};
            let pkg = GiPackage::open(&z, &sha(&z)).unwrap();
            let game = GameTarget {
                dir: t.path().to_path_buf(),
                exe: exe.to_path_buf(),
                steam_appid: None,
                platform: Platform::LinuxNative,
                steam_appinfo: None,
            };
            plan_install(&pkg, &game, &profile.join("p")).map(|_| ())
        };
        assert!(old.is_ok(), "0.4.1 follows the link: {old:?}");
        let new = {
            use astra_game_launcher::{GameTarget, GiPackage, Platform, plan_install};
            let pkg = GiPackage::open(&z, &sha(&z)).unwrap();
            let game = GameTarget {
                dir: t.path().to_path_buf(),
                exe: exe.to_path_buf(),
                steam_appid: None,
                platform: Platform::LinuxNative,
                steam_appinfo: None,
            };
            plan_install(&pkg, &game, &profile.join("p")).unwrap_err()
        };
        assert_eq!(
            (new.code, new.params.get("got").map(String::as_str)),
            ("engine-mismatch", Some("unknown"))
        );
        pinned.insert("link-program-folder");
    }

    // refused-name
    {
        let t = Tmp::new("div-device-name");
        write(&t.join("nul.exe"), b"MZ game");
        write(&t.join("nul.pck"), b"GDPC");
        let old = answered(legacy(t.path(), None));
        assert_eq!(old.engine.as_str(), "godot", "0.4.1 matched nul.pck");
        let new = detect_with(t.path(), None).unwrap();
        assert!(new.engine.is_unknown() && new.evidence.is_none());
        assert_eq!(new.exe.as_deref(), Some(Path::new("nul.exe")));

        let t = Tmp::new("div-device-hint");
        let name = "COM\u{00B9}";
        write(&t.join(&format!("{name}.exe")), b"MZ game");
        write(&t.join("UnityPlayer.dll"), b"MZ player");
        write(
            &t.join(&format!("{name}_Data/Managed/Assembly-CSharp.dll")),
            b"MZ cs",
        );
        let hint = format!("{name}.exe");
        let old = answered(legacy(t.path(), Some(&hint)));
        assert!(old.hint_used, "0.4.1 followed the hint");
        let new = detect_with(t.path(), Some(&hint)).unwrap();
        assert!(!new.hint_used);
        let mut old_unhinted = old.clone();
        old_unhinted.hint_used = false;
        assert_eq!(
            json(&new),
            json(&old_unhinted),
            "the folder search finds the game"
        );
        pinned.insert("refused-name");
    }
    pinned
}

#[test]
fn every_declared_divergence_is_pinned_and_holds() {
    if std::env::var(WORKER_ENV).is_ok() {
        return;
    }
    let declared: BTreeSet<String> = include_str!("corpus/divergence.txt")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let fields: Vec<&str> = l.split(" | ").collect();
            assert_eq!(fields.len(), 4, "id | where | 0.4.1 | 0.5.0: {l}");
            fields[0].trim().to_owned()
        })
        .collect();
    let pinned: BTreeSet<String> = fixtures().into_iter().map(str::to_owned).collect();
    assert_eq!(
        declared, pinned,
        "tests/corpus/divergence.txt and its fixtures name the same divergences"
    );
}
