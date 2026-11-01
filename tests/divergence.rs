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

    // refused-name (a device name only compared with a listing is not one: `nul.pck` beside
    // `nul.exe` is Godot on both sides, tests/detect.rs `a_name_only_compared_with_a_listing_…`)
    {
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

    // foreign-dwmapi (step 10a)
    {
        foreign_dwmapi();
        pinned.insert("foreign-dwmapi");
    }

    // proxy-folder-gained-files (merged plan R42)
    {
        proxy_folder_gained_files();
        pinned.insert("proxy-folder-gained-files");
    }

    // restore-partial (marketplace plan A§4.4, M2)
    {
        restore_partial();
        pinned.insert("restore-partial");
    }

    // extended-anti-cheat-* (merged plan §2.4, A§4.5)
    pinned.extend(extended_anti_cheat());
    pinned
}

/// Rows `extended-anti-cheat-<code>`: each name its public source gives (src/detect.rs cites
/// them), beside a Unity game. 0.4.1 sees no anti-cheat and plans; 0.5.0 reports the code and
/// refuses `anti-cheat` with it as `kind`. Nothing else in the detection differs. Spelled here,
/// not read from the table, so a name the table loses turns its row red.
fn extended_anti_cheat() -> Vec<&'static str> {
    /// The row's id, the code, and the names (`true`: a folder).
    type Row = (&'static str, &'static str, &'static [(&'static str, bool)]);
    const ROWS: &[Row] = &[
        (
            "extended-anti-cheat-ea-javelin",
            "ea-javelin",
            &[
                ("EAAntiCheat.Installer.exe", false),
                ("EAAntiCheat.GameServiceLauncher.exe", false),
            ],
        ),
        (
            "extended-anti-cheat-anti-cheat-expert",
            "anti-cheat-expert",
            &[("AntiCheatExpert", true), ("AceAntibotClient", true)],
        ),
        (
            "extended-anti-cheat-equ8",
            "equ8",
            &[("equ8_conf.json", false)],
        ),
        (
            "extended-anti-cheat-denuvo-anti-cheat",
            "denuvo-anti-cheat",
            &[
                ("denuvo-anti-cheat.sys", false),
                ("denuvo-anti-cheat-update-service.exe", false),
                ("denuvo-anti-cheat-runtime.dll", false),
                ("Denuvo Anti-Cheat Installer.exe", false),
            ],
        ),
    ];
    let z = placing("astra-probe.txt", b"probe");
    let mut pinned = Vec::new();
    for (row, code, names) in ROWS {
        for (name, is_dir) in *names {
            let t = Tmp::new("div-extended-ac");
            mono_game(t.path());
            if *is_dir {
                write(&t.join(&format!("{name}/readme.txt")), b"x");
            } else {
                write(&t.join(name), b"MZ");
            }
            let old = answered(legacy(t.path(), None));
            assert!(
                old.anti_cheat.is_empty(),
                "{name}: 0.4.1 sees no anti-cheat"
            );
            let new = detect_with(t.path(), None).unwrap();
            assert_eq!(new.anti_cheat, [*code], "{name}: 0.5.0 sees {code}");
            let mut same = new.clone();
            same.anti_cheat.clear();
            assert_eq!(json(&same), json(&old), "{name}: nothing else differs");

            let profile = Tmp::new("div-extended-ac-profile");
            assert!(
                legacy::plan(t.path(), &profile.join("p"), &z).is_ok(),
                "{name}: 0.4.1 plans"
            );
            assert_eq!(
                ours::plan(t.path(), &profile.join("p"), &z).unwrap_err(),
                format!("anti-cheat {{\"kind\": \"{code}\"}}"),
                "{name}: 0.5.0 refuses"
            );
        }
        pinned.push(*row);
    }
    pinned
}

/// A first-party manifest placing `name` (our bytes `ours`) beside any program, its launch
/// passing nothing: the shape of GTA III's and the Unreal runners' `dwmapi.dll`.
fn placing(name: &str, ours: &[u8]) -> Vec<u8> {
    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"astra.probe\"\nname = \"Probe\"\nversion = \"1.0.0\"\n\n[target]\nexe = \"*\"\nplatforms = [\"linux-proton\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"p.txt\"\nto = \"${{profile}}/p.txt\"\n\n[[game_files]]\nfrom = \"{name}\"\nto = \"${{game}}/{name}\"\nsha256 = \"{}\"\n",
        sha(ours)
    );
    zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("p.txt", b"p"),
        E::File(name, ours),
    ])
}

/// Each launcher's public path, in its own types, each outcome as text (the plan's JSON, or a
/// refusal's code and params), on `dir` with its program `Game.exe` under Proton.
macro_rules! launcher {
    ($m:ident, $krate:ident) => {
        mod $m {
            use std::path::Path;

            use $krate::{Accept, Ctx, GameTarget, GiPackage, Platform};

            use super::sha;

            pub fn game(dir: &Path) -> GameTarget {
                GameTarget {
                    dir: dir.to_path_buf(),
                    exe: "Game.exe".into(),
                    steam_appid: None,
                    platform: Platform::LinuxProton { prefix: None },
                    steam_appinfo: Some(dir.join("no-appinfo.vdf")),
                }
            }

            pub fn plan(dir: &Path, profile: &Path, zip: &[u8]) -> Result<String, String> {
                let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
                $krate::plan_install(&pkg, &game(dir), profile)
                    .map(|p| serde_json::to_string(&p).unwrap())
                    .map_err(|e| format!("{} {:?}", e.code, e.params))
            }

            pub fn install(dir: &Path, profile: &Path, zip: &[u8]) {
                let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
                let plan = $krate::plan_install(&pkg, &game(dir), profile).unwrap();
                let accept = Accept {
                    vac_unchecked: true,
                };
                $krate::install(&pkg, &plan, &game(dir), accept, &Ctx::silent()).unwrap();
            }

            pub fn uninstall(dir: &Path, profile: &Path) -> String {
                serde_json::to_string(&$krate::uninstall(profile, &game(dir)).unwrap()).unwrap()
            }
        }
    };
}
launcher!(ours, astra_game_launcher);
launcher!(legacy, launcher_legacy);

/// Row `foreign-dwmapi`: a player's UE4SS (its `dwmapi.dll` and `ue4ss/UE4SS.dll`) beside the
/// program, and Astra's own manifest placing its `dwmapi.dll` there. 0.4.1 replaces theirs;
/// 0.5.0 refuses `foreign-loader`. Over the ledger 0.4.1's install wrote, 0.5.0 keeps that
/// `replace`, warns `foreign-loader-in-backup`, installs the update, and its uninstall puts
/// theirs back.
fn foreign_dwmapi() {
    let ours_bytes: &[u8] = b"MZ astra_loader: an inert dwmapi forwarder";
    let theirs: &[u8] = b"MZ a UE4SS of the player's";
    let z = placing("dwmapi.dll", ours_bytes);
    let lay = |d: &Path| {
        write(&d.join("Game.exe"), b"MZ game");
        write(&d.join("dwmapi.dll"), theirs);
        write(&d.join("ue4ss/UE4SS.dll"), b"MZ UE4SS");
    };
    let t = Tmp::new("div-dwmapi");
    let game = t.join("game");
    lay(&game);
    let profile = t.join("profile/p");
    let old = legacy::plan(&game, &profile, &z).unwrap();
    assert!(
        old.contains("\"rel_path\":\"dwmapi.dll\",\"action\":\"replace\""),
        "0.4.1 replaces it: {old}"
    );
    assert_eq!(
        ours::plan(&game, &profile, &z).unwrap_err(),
        "foreign-loader {\"file\": \"dwmapi.dll\"}"
    );
    // 0.4.1 installed it: 0.5.0 updates over that ledger, its backup kept.
    legacy::install(&game, &profile, &z);
    let pkg = astra_game_launcher::GiPackage::open(&z, &sha(&z)).unwrap();
    let plan = astra_game_launcher::plan_install(&pkg, &ours::game(&game), &profile).unwrap();
    assert_eq!(
        (
            plan.game_files[0].action,
            plan.game_files[0].existing_sha256.clone()
        ),
        (astra_game_launcher::GameAction::Replace, Some(sha(theirs)))
    );
    assert!(
        plan.warnings
            .contains(&astra_game_launcher::codes::FOREIGN_LOADER_IN_BACKUP.to_owned()),
        "{:?}",
        plan.warnings
    );
    // The same plan but for that warning is 0.4.1's over its own ledger.
    let again_old = legacy::plan(&game, &profile, &z).unwrap();
    let mut without: serde_json::Value = serde_json::to_value(&plan).unwrap();
    without["warnings"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w != "foreign-loader-in-backup");
    assert_eq!(
        without,
        serde_json::from_str::<serde_json::Value>(&again_old).unwrap()
    );
    astra_game_launcher::install(
        &pkg,
        &plan,
        &ours::game(&game),
        astra_game_launcher::Accept {
            vac_unchecked: true,
        },
        &astra_game_launcher::Ctx::silent(),
    )
    .unwrap();
    let gone = ours::uninstall(&game, &profile);
    assert!(gone.contains("\"restored\":[\"dwmapi.dll\"]"), "{gone}");
    assert_eq!(std::fs::read(game.join("dwmapi.dll")).unwrap(), theirs);
}

/// Row `proxy-folder-gained-files`: Astra's own manifest adds `winhttp.dll` (no Doorstop: no
/// BepInEx rule keeps it); a `Mods/` folder arrives beside it. 0.4.1's uninstall removes ours;
/// 0.5.0's keeps it, saying why. With nothing arrived, both remove it alike.
fn proxy_folder_gained_files() {
    let ours_bytes: &[u8] = b"MZ a proxy of ours";
    let z = placing("winhttp.dll", ours_bytes);
    for gained in [false, true] {
        let mut gone = Vec::new();
        for (who, install, uninstall) in [
            (
                "legacy",
                legacy::install as fn(&Path, &Path, &[u8]),
                legacy::uninstall as fn(&Path, &Path) -> String,
            ),
            ("ours", ours::install, ours::uninstall),
        ] {
            let t = Tmp::new(&format!("div-proxy-folder-{who}-{gained}"));
            let game = t.join("game");
            write(&game.join("Game.exe"), b"MZ game");
            let profile = t.join("profile/p");
            install(&game, &profile, &z);
            if gained {
                write(&game.join("Mods/their_mod.lua"), b"-- theirs");
            }
            gone.push((
                uninstall(&game, &profile),
                game.join("winhttp.dll").exists(),
            ));
        }
        let (old, new) = (&gone[0], &gone[1]);
        assert!(!old.1, "0.4.1 removes ours");
        assert!(old.0.contains("\"removed\":[\"winhttp.dll\"]"), "{}", old.0);
        if gained {
            assert!(new.1, "0.5.0 keeps ours");
            let r: astra_game_launcher::UninstallReport = serde_json::from_str(&new.0).unwrap();
            assert_eq!(r.kept, ["winhttp.dll"]);
            assert_eq!(
                r.kept_reasons.get("winhttp.dll").map(String::as_str),
                Some(astra_game_launcher::KEPT_FOLDER_GAINED_FILES)
            );
        } else {
            assert_eq!(new, old, "nothing arrived: 0.5.0 uninstalls as 0.4.1");
        }
    }
}

/// Row `restore-partial`: Astra's own manifest adds `settings.json` (no loader's name) and
/// replaces the game's `options.ini`. Changed after the install — ours rewritten, the game's
/// updated over ours, the backup gone or changed — each uninstall leaves the file and names it in
/// `kept`, alike; 0.5.0's report also warns `restore-partial`, and is otherwise 0.4.1's. With
/// nothing changed, both reports are the same, warning nothing.
fn restore_partial() {
    let ours_bytes: &[u8] = b"{\"ours\":true}";
    let z = {
        let manifest = format!(
            "schema = 1\n\n[integration]\nid = \"astra.probe\"\nname = \"Probe\"\nversion = \"1.0.0\"\n\n[target]\nexe = \"*\"\nplatforms = [\"linux-proton\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"p.txt\"\nto = \"${{profile}}/p.txt\"\n\n[[game_files]]\nfrom = \"settings.json\"\nto = \"${{game}}/settings.json\"\nsha256 = \"{}\"\n\n[[game_files]]\nfrom = \"options.ini\"\nto = \"${{game}}/options.ini\"\n",
            sha(ours_bytes)
        );
        zip(&[
            E::File("astra-gi.toml", manifest.as_bytes()),
            E::File("p.txt", b"p"),
            E::File("settings.json", ours_bytes),
            E::File("options.ini", b"[ours]\n"),
        ])
    };
    let backup = "astra-launcher-backup/game/options.ini";
    type Change = fn(&Path, &Path);
    let changes: [(&str, Option<&str>, Change); 5] = [
        ("nothing", None, |_, _| {}),
        ("ours rewritten", Some("settings.json"), |game, _| {
            write(&game.join("settings.json"), b"{\"theirs\":true}")
        }),
        (
            "the game's updated over ours",
            Some("options.ini"),
            |game, _| write(&game.join("options.ini"), b"[game update]\n"),
        ),
        ("the backup gone", Some("options.ini"), |_, profile| {
            std::fs::remove_file(profile.join("astra-launcher-backup/game/options.ini")).unwrap()
        }),
        ("the backup changed", Some("options.ini"), |_, profile| {
            write(
                &profile.join("astra-launcher-backup/game/options.ini"),
                b"[not what was backed up]\n",
            )
        }),
    ];
    for (what, kept, change) in changes {
        let mut gone = Vec::new();
        for (who, install, uninstall) in [
            (
                "legacy",
                legacy::install as fn(&Path, &Path, &[u8]),
                legacy::uninstall as fn(&Path, &Path) -> String,
            ),
            ("ours", ours::install, ours::uninstall),
        ] {
            let t = Tmp::new(&format!("div-restore-partial-{who}"));
            let game = t.join("game");
            write(&game.join("Game.exe"), b"MZ game");
            write(&game.join("options.ini"), b"[the game's]\n");
            let profile = t.join("profile/p");
            install(&game, &profile, &z);
            assert!(profile.join(backup).is_file(), "{who}: {what}");
            change(&game, &profile);
            gone.push(uninstall(&game, &profile));
        }
        let (old, new) = (&gone[0], &gone[1]);
        let r: astra_game_launcher::UninstallReport = serde_json::from_str(new).unwrap();
        match kept {
            None => {
                assert_eq!(new, old, "{what}: 0.5.0 uninstalls as 0.4.1");
                assert!(r.warnings.is_empty(), "{what}");
            }
            Some(rel) => {
                assert_eq!(r.kept, [rel], "{what}: {new}");
                assert_eq!(
                    r.warnings,
                    [astra_game_launcher::codes::RESTORE_PARTIAL],
                    "{what}"
                );
                let mut without = r.clone();
                without.warnings.clear();
                assert_eq!(
                    serde_json::to_string(&without).unwrap(),
                    *old,
                    "{what}: but for the warning, 0.4.1's report"
                );
            }
        }
    }
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
