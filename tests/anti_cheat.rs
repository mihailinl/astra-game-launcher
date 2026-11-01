// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Every anti-cheat signal merged plan §2.4 gives the launcher, one fixture each, and planning
//! refusing on each before the engine is checked:
//!
//! - the folder signatures (`ANTI_CHEAT_SIGNATURES`: 0.4.1's and A§4.5's extended ones);
//! - the signed catalogue's `anticheat[]` markers (`EngineSet::with_anti_cheat_markers`):
//!   folder, file (`process-name`) and service names, which only ever add, and apply to EVERY
//!   game — one Pass A recognised included;
//! - a kernel-mode driver service whose image is in the game's folder (Windows; a fake tree under
//!   `HKEY_CURRENT_USER`, never `HKLM`).
//!
//! Steam's own disclosure is not in `appinfo.vdf` (debt row 25) and VAC has its own tests
//! (`tests/steam_appinfo.rs`). And K16's launcher half: one anti-cheat table and one Steam
//! category table, each code spelled once.

mod common;
#[path = "common/spec_goldens.rs"]
mod spec_goldens;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use astra_game_launcher::{
    ANTI_CHEAT_CODES, ANTI_CHEAT_SIGNATURES, AntiCheatMarker, AntiCheatMarkerKind, Ctx, Descriptor,
    Detection, EngineSet, Moderation, ONLINE_CATEGORIES, Origin, Platform, Provenance,
    SignatureName, VALVE_ANTI_CHEAT, codes, detect_with, detect_with_engines, install, install_in,
    installed, plan_install_in, uninstall,
};
use common::scrape::{literals, rust_files};
use common::*;

fn marker(kind: AntiCheatMarkerKind, value: &str) -> AntiCheatMarker {
    AntiCheatMarker {
        kind,
        value: value.to_owned(),
    }
}

fn markers(list: &[(AntiCheatMarkerKind, &str)]) -> EngineSet {
    EngineSet::empty().with_anti_cheat_markers(list.iter().map(|(k, v)| marker(*k, v)).collect())
}

/// Lays `name` in `dir`: a folder (with a file in it) or a file.
fn lay(dir: &Path, name: &str, is_dir: bool) {
    if is_dir {
        write(&dir.join(name).join("readme.txt"), b"x");
    } else {
        write(&dir.join(name), b"MZ");
    }
}

fn detect(dir: &Path, set: &EngineSet) -> Detection {
    detect_with_engines(dir, None, set).unwrap()
}

/// The Unity runner planned on `dir`'s `Game.exe` with `set`, first-party: its refusal's code and
/// `kind`, or `None` when it plans.
fn refusal(dir: &Path, set: &EngineSet) -> Option<(String, Option<String>)> {
    let profile = Tmp::new("ac-profile");
    let game = target(dir, "Game.exe", None, Platform::Windows);
    plan_install_in(
        &runner(),
        &game,
        &profile.join("p"),
        set,
        Provenance::FirstParty,
        None,
    )
    .err()
    .map(|e| (e.code.to_owned(), e.params.get("kind").cloned()))
}

fn anti_cheat(kind: &str) -> Option<(String, Option<String>)> {
    Some((codes::ANTI_CHEAT.to_owned(), Some(kind.to_owned())))
}

/// A name `n` of a signature matches, cased otherwise than the table spells it.
fn instance(n: &SignatureName) -> (String, bool) {
    let cased = |s: &str| {
        s.chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect::<String>()
    };
    match n {
        SignatureName::Folder(f) => (cased(f), true),
        SignatureName::File(f) => (cased(f), false),
        SignatureName::FileAround(start, end) => (cased(&format!("{start}x{end}")), false),
    }
}

/// Each name of each signature, beside a Unity game: the signature's code, and the plan refused
/// with it as its `kind`. Every code is one of `ANTI_CHEAT_CODES`.
#[test]
fn every_signature_name_gives_its_code_and_a_refusal() {
    let mut names = 0;
    for sig in ANTI_CHEAT_SIGNATURES {
        assert!(ANTI_CHEAT_CODES.contains(&sig.code), "{}", sig.code);
        assert!(!sig.names.is_empty(), "{}", sig.code);
        for n in sig.names {
            let (name, is_dir) = instance(n);
            let t = Tmp::new("ac-sig");
            unity_game(t.path(), "Game");
            lay(t.path(), &name, is_dir);
            let d = detect(t.path(), &EngineSet::empty());
            assert_eq!(d.engine.as_str(), "unity-mono", "{name}");
            assert_eq!(d.anti_cheat, [sig.code], "{name}");
            assert_eq!(
                refusal(t.path(), &EngineSet::empty()),
                anti_cheat(sig.code),
                "{name}"
            );
            // The other kind of entry by the same name is not this signature.
            let t = Tmp::new("ac-sig-kind");
            unity_game(t.path(), "Game");
            lay(t.path(), &name, !is_dir);
            assert!(
                detect(t.path(), &EngineSet::empty()).anti_cheat.is_empty(),
                "{name}"
            );
            names += 1;
        }
    }
    assert!(names >= 15, "{names} names");
}

/// A§4.5's extended signatures, by the names their public sources spell (src/detect.rs cites
/// each): EA Javelin, ACE, EQU8, Denuvo Anti-Cheat. The same set `tests/divergence.rs` holds
/// against 0.4.1, on every platform.
#[test]
fn the_extended_signatures_see_the_names_their_sources_give() {
    let cases: &[(&str, bool, &str)] = &[
        ("EAAntiCheat.Installer.exe", false, "ea-javelin"),
        ("EAAntiCheat.GameServiceLauncher.exe", false, "ea-javelin"),
        ("AntiCheatExpert", true, "anti-cheat-expert"),
        ("AceAntibotClient", true, "anti-cheat-expert"),
        ("equ8_conf.json", false, "equ8"),
        ("denuvo-anti-cheat.sys", false, "denuvo-anti-cheat"),
        (
            "denuvo-anti-cheat-update-service.exe",
            false,
            "denuvo-anti-cheat",
        ),
        ("denuvo-anti-cheat-runtime.dll", false, "denuvo-anti-cheat"),
        (
            "Denuvo Anti-Cheat Installer.exe",
            false,
            "denuvo-anti-cheat",
        ),
    ];
    for (name, is_dir, code) in cases {
        let t = Tmp::new("ac-ext");
        unity_game(t.path(), "Game");
        lay(&t.join("Game_Data"), name, *is_dir);
        assert_eq!(
            detect(t.path(), &EngineSet::empty()).anti_cheat,
            [*code],
            "{name}"
        );
        assert_eq!(
            refusal(t.path(), &EngineSet::empty()),
            anti_cheat(code),
            "{name}"
        );
    }
}

/// A folder marker names a folder, a process name a FILE (never a folder), each exactly and
/// ignoring ASCII case; a service name names nothing in the walk.
#[test]
fn a_folder_marker_names_a_folder_and_a_process_name_a_file() {
    use AntiCheatMarkerKind::*;
    let set = markers(&[
        (FolderMarker, "GuardDir"),
        (ProcessName, "Guard.exe"),
        (ServiceName, "x.dat"),
    ]);
    let listed = || vec![codes::LISTED_ANTI_CHEAT.to_owned()];
    let cases: &[(&str, bool, bool)] = &[
        ("GuardDir", true, true),
        ("guarddir", true, true),
        ("GUARDDIR", true, true),
        ("GuardDir", false, false),
        ("GuardDir2", true, false),
        ("MyGuardDir", true, false),
        ("Guard.exe", false, true),
        ("guard.EXE", false, true),
        ("Guard.exe", true, false),
        ("Guard.exe.bak", false, false),
        ("x.dat", false, false),
        ("x.dat", true, false),
    ];
    for (name, is_dir, hit) in cases {
        let t = Tmp::new("ac-marker");
        write(&t.join("Game.exe"), b"MZ a game");
        lay(&t.join("bin"), name, *is_dir);
        let d = detect(t.path(), &set);
        let want = if *hit { listed() } else { Vec::new() };
        assert_eq!(d.anti_cheat, want, "{name} dir={is_dir}");
        let want = hit.then(|| anti_cheat(codes::LISTED_ANTI_CHEAT)).flatten();
        let got = refusal(t.path(), &set).filter(|(c, _)| c == codes::ANTI_CHEAT);
        assert_eq!(got, want, "{name} dir={is_dir}");
    }
}

/// Markers only add: beside a folder signature they follow it; no marker, or one naming nothing
/// here, leaves the detection exactly the passes' (C1's), and so does an empty list.
#[test]
fn markers_only_add_and_none_changes_nothing() {
    use AntiCheatMarkerKind::*;
    let t = Tmp::new("ac-add");
    unity_game(t.path(), "Game");
    lay(t.path(), "EasyAntiCheat", true);
    lay(&t.join("Game_Data"), "Guard", true);
    let alone = detect_with(t.path(), None).unwrap();
    assert_eq!(alone.anti_cheat, ["easy-anti-cheat"]);
    let json = |d: &Detection| serde_json::to_string(d).unwrap();
    for set in [
        EngineSet::empty(),
        EngineSet::empty().with_anti_cheat_markers(Vec::new()),
        markers(&[
            (FolderMarker, "NotHere"),
            (ProcessName, "Guard"),
            (ServiceName, "Guard"),
        ]),
    ] {
        assert_eq!(json(&detect(t.path(), &set)), json(&alone));
        assert_eq!(refusal(t.path(), &set), anti_cheat("easy-anti-cheat"));
    }
    let set = markers(&[(FolderMarker, "Guard")]);
    assert_eq!(set.anti_cheat_markers(), [marker(FolderMarker, "Guard")]);
    let d = detect(t.path(), &set);
    assert_eq!(d.anti_cheat, ["easy-anti-cheat", codes::LISTED_ANTI_CHEAT]);
    let mut without = d.clone();
    without.anti_cheat.pop();
    assert_eq!(json(&without), json(&alone), "nothing else changes");
    // The signature is the first signal: the refusal's kind stays its code.
    assert_eq!(refusal(t.path(), &set), anti_cheat("easy-anti-cheat"));
    // Added again, the markers add up; a copy changes nothing.
    let more = set.clone().with_anti_cheat_markers(vec![
        marker(FolderMarker, "guard"),
        marker(ProcessName, "readme.txt"),
    ]);
    assert_eq!(more.anti_cheat_markers().len(), 3);
    assert_eq!(detect(t.path(), &more).anti_cheat, d.anti_cheat);
}

/// A marker the catalogue adds between the consent sheet and the install still refuses it:
/// `install_in` plans again with the markers the daemon's set carries then. `install` (no
/// markers) and a set whose markers the game does not hold install as planned.
#[test]
fn install_plans_again_with_the_markers_the_catalogue_has_now() {
    use AntiCheatMarkerKind::*;
    let t = Tmp::new("ac-install");
    unity_game(t.path(), "Game");
    lay(&t.join("Game_Data"), "Guard", true);
    let profiles = Tmp::new("ac-install-profiles");
    let game = target(t.path(), "Game.exe", None, Platform::Windows);
    let plan_with = |profile: &Path, set: &EngineSet| {
        plan_install_in(&runner(), &game, profile, set, Provenance::FirstParty, None).unwrap()
    };
    // Planned while the catalogue listed nothing; installed once it lists the folder.
    let now = markers(&[(FolderMarker, "Guard")]);
    let p = profiles.join("p");
    let plan = plan_with(&p, &EngineSet::empty());
    let e = install_in(&runner(), &plan, &game, ACCEPT, &now, &Ctx::silent()).unwrap_err();
    assert_eq!(
        (e.code, e.param("kind")),
        (codes::ANTI_CHEAT, Some(codes::LISTED_ANTI_CHEAT))
    );
    assert_eq!(installed(&p).unwrap(), None, "nothing installed");
    assert!(!t.join("winhttp.dll").exists());
    // A marker the game does not hold, and no markers at all, change nothing.
    let elsewhere = markers(&[(FolderMarker, "NotHere")]);
    install_in(&runner(), &plan, &game, ACCEPT, &elsewhere, &Ctx::silent()).unwrap();
    uninstall(&p, &game).unwrap();
    let plan = plan_with(&p, &EngineSet::empty());
    install(&runner(), &plan, &game, ACCEPT, &Ctx::silent()).unwrap();
}

/// A game Pass A recognised (closed to descriptors) still carries a listed marker; so does one
/// nothing recognised, and one an engine descriptor named in Pass B. The plan refuses each, before
/// it checks the engine: a Unity runner on a game of no engine is `anti-cheat`, not
/// `engine-mismatch`.
#[test]
fn every_game_gets_the_markers_whichever_pass_named_it() {
    use AntiCheatMarkerKind::*;
    let set = markers(&[(FolderMarker, "AcGuard")]);

    let unity = Tmp::new("ac-unity");
    unity_game(unity.path(), "Game");
    lay(unity.path(), "AcGuard", true);
    let d = detect(unity.path(), &set);
    assert!(!d.open_to_descriptors());
    assert_eq!(
        (d.engine.as_str(), d.anti_cheat.as_slice()),
        ("unity-mono", &["listed-anti-cheat".to_owned()][..])
    );
    assert_eq!(
        refusal(unity.path(), &set),
        anti_cheat(codes::LISTED_ANTI_CHEAT)
    );
    assert_eq!(
        refusal(unity.path(), &EngineSet::empty()),
        None,
        "it plans without the marker"
    );

    let nothing = Tmp::new("ac-nothing");
    write(&nothing.join("Game.exe"), b"MZ a game");
    lay(nothing.path(), "AcGuard", true);
    let d = detect(nothing.path(), &set);
    assert!(d.engine.is_unknown() && d.open_to_descriptors());
    assert_eq!(d.anti_cheat, ["listed-anti-cheat"]);
    assert_eq!(
        refusal(nothing.path(), &set),
        anti_cheat(codes::LISTED_ANTI_CHEAT)
    );
    assert_eq!(
        refusal(nothing.path(), &EngineSet::empty()).map(|r| r.0),
        Some(codes::ENGINE_MISMATCH.to_owned()),
        "without the marker, the engine check speaks"
    );

    // Pass B: a descriptor names the game, and the marker still joins its answer.
    let text = "schema = \"astra.engine/1\"\nengine = { codes = { probe-ac = \"Probe\" } }\n[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], at = \"{project}/bin/win64\", first = 8 }\nrequire = [ { listed = \"engine2.dll\", kind = \"file\" }, { child = \"{project}/core\", kind = \"dir\", as = \"core\" } ]\ncode = \"probe-ac\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{core}\"\n";
    let described = EngineSet::build([(
        Origin {
            moderation: Moderation::Unchecked,
            publisher: "someone".to_owned(),
            first_party: false,
        },
        Descriptor::from_toml(text),
    )])
    .with_anti_cheat_markers(vec![marker(ProcessName, "acguard.bin")]);
    assert_eq!(described.len(), 1, "{:?}", described.skipped());
    let b = Tmp::new("ac-passb");
    write(&b.join("game/bin/win64/G.exe"), b"MZ\x90\0 a game");
    write(&b.join("game/bin/win64/engine2.dll"), b"MZ engine");
    write(&b.join("game/core/pak01_dir.vpk"), b"vpk");
    write(&b.join("game/bin/win64/acguard.bin"), b"x");
    let d = detect(b.path(), &described);
    assert_eq!(d.engine.as_str(), "probe-ac");
    assert_eq!(d.anti_cheat, ["listed-anti-cheat"]);
}

/// The spec's own `anticheat[]` rows (games-index-v1 §2.3), from AstraPlugins' goldens as
/// `tests/goldens/astraplugins/` vendors them at its `PIN` (d46e44a): the valid index's rows are
/// `AntiCheatMarker`s as they are (their display `name` is not read), and fire; a row of an
/// unknown `kind` (`vectors.json`: `anticheat-marker-bad-kind`, refuse) is no marker.
#[test]
fn the_specs_anticheat_rows_are_markers_as_they_are() {
    use AntiCheatMarkerKind::*;
    let index = |rel: &str| -> serde_json::Value {
        serde_json::from_str(&spec_goldens::golden_text(rel)).unwrap()
    };
    let valid = index("index/valid-index.json");
    let rows: Vec<AntiCheatMarker> = serde_json::from_value(valid["anticheat"].clone()).unwrap();
    assert_eq!(rows, [marker(FolderMarker, "EasyAntiCheat")]);
    let t = Tmp::new("ac-spec");
    unity_game(t.path(), "Game");
    lay(t.path(), "EasyAntiCheat", true);
    let set = EngineSet::empty().with_anti_cheat_markers(rows);
    assert_eq!(
        detect(t.path(), &set).anti_cheat,
        ["easy-anti-cheat", codes::LISTED_ANTI_CHEAT]
    );
    let bad = index("index/invalid-anticheat-marker-bad-kind.json");
    assert!(serde_json::from_value::<Vec<AntiCheatMarker>>(bad["anticheat"].clone()).is_err());
    for kind in ["folder-marker", "process-name", "service-name"] {
        let row = serde_json::json!({ "name": "n", "kind": kind, "value": "v" });
        assert!(
            serde_json::from_value::<AntiCheatMarker>(row).is_ok(),
            "{kind}"
        );
    }
}

/// The kernel signal and the service-name marker, on Windows: a fake Services tree under
/// `HKEY_CURRENT_USER` (`reg.exe`), read through the seam. A driver whose image is inside the
/// game's folder fires; one elsewhere (System32, another folder, a sibling sharing the folder's
/// name as a prefix) does not; a user-mode service fires only when a marker names it and its
/// image is inside.
#[cfg(windows)]
#[test]
fn a_service_is_tied_to_the_game_by_its_image_path() {
    use std::process::Command;

    use AntiCheatMarkerKind::*;
    use astra_game_launcher::anti_cheat_services::{ServiceTree, of_game};

    struct Tree(String);
    impl Tree {
        fn add(&self, name: &str, kind: u32, ty: &str, image: &str) {
            let key = format!(r"HKCU\{}\{name}", self.0);
            for args in [
                vec![
                    "add".to_owned(),
                    key.clone(),
                    "/v".into(),
                    "Type".into(),
                    "/t".into(),
                    "REG_DWORD".into(),
                    "/d".into(),
                    kind.to_string(),
                    "/f".into(),
                ],
                vec![
                    "add".to_owned(),
                    key.clone(),
                    "/v".into(),
                    "ImagePath".into(),
                    "/t".into(),
                    ty.into(),
                    "/d".into(),
                    image.into(),
                    "/f".into(),
                ],
            ] {
                let out = Command::new("reg").args(&args).output().unwrap();
                assert!(out.status.success(), "reg {args:?}: {out:?}");
            }
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let top = self.0.rsplit_once('\\').unwrap().0.to_owned();
            let _ = Command::new("reg")
                .args(["delete", &format!(r"HKCU\{top}"), "/f"])
                .output();
        }
    }

    let game = Tmp::new("ac-svc");
    unity_game(game.path(), "Game");
    write(&game.join("AntiCheat/guard.sys"), b"MZ driver");
    let sibling = Tmp::new("ac-svc-sibling");
    let plain = |p: &Path| p.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
    let dir = plain(game.path());
    let t = Tree(format!(
        r"Software\AstraGameLauncherTests\{}-it\Services",
        std::process::id()
    ));
    let tree = ServiceTree::CurrentUser(&t.0);
    let none: Vec<String> = Vec::new();
    assert_eq!(
        of_game(game.path(), tree, &EngineSet::empty()),
        none,
        "no tree yet"
    );

    t.add(
        "SysDriver",
        1,
        "REG_EXPAND_SZ",
        r"\SystemRoot\System32\drivers\sysdrv.sys",
    );
    t.add(
        "RelDriver",
        1,
        "REG_EXPAND_SZ",
        r"System32\drivers\reldrv.sys",
    );
    t.add(
        "ElseDriver",
        1,
        "REG_SZ",
        &format!(r"\??\{}\guard.sys", plain(sibling.path())),
    );
    t.add(
        "PrefixDriver",
        2,
        "REG_SZ",
        &format!(r"\??\{dir}x\guard.sys"),
    );
    t.add(
        "UserSvc",
        16,
        "REG_SZ",
        &format!(r#""{dir}\AntiCheat\svc.exe" -run"#),
    );
    assert_eq!(
        of_game(game.path(), tree, &EngineSet::empty()),
        none,
        "nothing tied"
    );

    let named = markers(&[(ServiceName, "usersvc")]);
    assert_eq!(of_game(game.path(), tree, &named), ["listed-anti-cheat"]);
    let named_elsewhere = markers(&[(ServiceName, "ElseDriver")]);
    assert_eq!(of_game(game.path(), tree, &named_elsewhere), none);

    t.add(
        "GameDriver",
        1,
        "REG_SZ",
        &format!(r"\??\{dir}\AntiCheat\guard.sys"),
    );
    assert_eq!(
        of_game(game.path(), tree, &EngineSet::empty()),
        ["kernel-anti-cheat"]
    );
    assert_eq!(
        of_game(game.path(), tree, &named),
        ["kernel-anti-cheat", "listed-anti-cheat"]
    );
    // A key read before and changed since is read again (the read keeps what it learned per key,
    // by its last-write time): a driver moved out, another moved in, then no longer a driver.
    let empty = EngineSet::empty();
    t.add(
        "GameDriver",
        1,
        "REG_SZ",
        r"\SystemRoot\System32\drivers\moved.sys",
    );
    assert_eq!(of_game(game.path(), tree, &empty), none, "moved out");
    t.add(
        "ElseDriver",
        1,
        "REG_SZ",
        &format!(r"{dir}\AntiCheat\guard.sys"),
    );
    assert_eq!(
        of_game(game.path(), tree, &empty),
        ["kernel-anti-cheat"],
        "moved in"
    );
    t.add(
        "ElseDriver",
        16,
        "REG_SZ",
        &format!(r"{dir}\AntiCheat\guard.sys"),
    );
    assert_eq!(
        of_game(game.path(), tree, &empty),
        none,
        "no longer a driver"
    );
    // A driver installed through a junction to the game's folder (a library reached through
    // one): tied to the folder it reaches, whichever spelling either side uses — planning asks
    // with the folder resolved.
    let links = Tmp::new("ac-svc-junction");
    let junction = links.join("Library");
    let made = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&junction)
        .arg(game.path())
        .output()
        .unwrap();
    assert!(made.status.success(), "mklink /J: {made:?}");
    let through = plain(junction.as_path());
    t.add(
        "JunctionDriver",
        1,
        "REG_SZ",
        &format!(r"\??\{through}\AntiCheat\guard.sys"),
    );
    assert_eq!(
        of_game(game.path(), tree, &empty),
        ["kernel-anti-cheat"],
        "written through the junction, asked with the folder"
    );
    assert_eq!(
        of_game(&junction, tree, &empty),
        ["kernel-anti-cheat"],
        "written through the junction, asked through it"
    );
    t.add(
        "JunctionDriver",
        1,
        "REG_SZ",
        r"\SystemRoot\System32\drivers\junction.sys",
    );
    t.add(
        "ElseDriver",
        1,
        "REG_SZ",
        &format!(r"{dir}\AntiCheat\guard.sys"),
    );
    assert_eq!(
        of_game(&junction, tree, &empty),
        ["kernel-anti-cheat"],
        "written with the folder, asked through the junction"
    );
    t.add(
        "ElseDriver",
        16,
        "REG_SZ",
        &format!(r"{dir}\AntiCheat\guard.sys"),
    );
    assert_eq!(of_game(game.path(), tree, &empty), none);
    let _ = std::fs::remove_dir(&junction);
    // An image spelled with a short (8.3) name is the same folder, read from disk.
    let long = format!(r"{dir}\AntiCheat\guard.sys");
    let short = {
        use std::os::windows::process::CommandExt;
        // Passed as written: cmd reads its own quotes, not the C runtime's escapes.
        Command::new("cmd")
            .raw_arg(format!("/c for %I in (\"{long}\") do @echo %~sI"))
            .output()
            .unwrap()
    };
    let short = String::from_utf8_lossy(&short.stdout).trim().to_owned();
    if short.contains('~') {
        eprintln!("the short-name case: {short}");
        t.add("ShortDriver", 1, "REG_SZ", &format!(r"\??\{short}"));
        assert_eq!(
            of_game(game.path(), tree, &empty),
            ["kernel-anti-cheat"],
            "{short}"
        );
        t.add("ShortDriver", 1, "REG_SZ", r"System32\drivers\short.sys");
        assert_eq!(of_game(game.path(), tree, &empty), none);
    } else {
        eprintln!("no 8.3 names on this volume ({short:?}): the short-name case is not reached");
    }
    // The real Services key of this machine ties nothing to a folder made a moment ago, so the
    // detection and the plan are as without services.
    assert!(
        detect(game.path(), &EngineSet::empty())
            .anti_cheat
            .is_empty()
    );
    assert_eq!(refusal(game.path(), &EngineSet::empty()), None);
}

/// The values of `pub const NAME: &str = "value";` in `src`.
fn str_consts(src: &str) -> BTreeMap<String, String> {
    src.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("pub const ")?;
            let (name, rest) = rest.split_once(": &str = \"")?;
            let (value, _) = rest.split_once('"')?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// `src` with its comments and the insides of its string and char literals blanked (a `\u{20}`
/// escape is no number), lifetimes kept.
fn code_only(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            out.push(' ');
            continue;
        }
        if c == 'r' && (next == Some('"') || next == Some('#')) {
            let mut j = i + 1;
            let mut hashes = 0;
            while b.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&'"') {
                j += 1;
                loop {
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == '"' && (0..hashes).all(|h| b.get(j + 1 + h) == Some(&'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                out.push_str("\"\"");
                i = j;
                continue;
            }
        }
        if c == '"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != '"' {
                j += if b[j] == '\\' { 2 } else { 1 };
            }
            out.push_str("\"\"");
            i = j + 1;
            continue;
        }
        if c == '\'' {
            // A char literal (`'x'`, `'\n'`, `'\u{20}'`), else a lifetime.
            let end = if next == Some('\\') {
                (i + 2..b.len().min(i + 14)).find(|&j| b[j] == '\'')
            } else if b.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(end) = end {
                out.push_str("' '");
                i = end + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Every `[…]` and `(…)` with no bracket inside it, in `src`'s code ([`code_only`]), as the
/// integer literals written in it: decimal, `_` allowed, with or without an integer suffix
/// (`36`, `36u32`, `1_000`); a hex digit run, a float or a number inside a name is not one.
fn number_lists(src: &str) -> Vec<Vec<u32>> {
    const SUFFIXES: &[&str] = &[
        "", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    ];
    let code = code_only(src);
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for (i, c) in code.char_indices() {
        match c {
            '[' | '(' => open = Some(i + 1),
            ']' | ')' => {
                if let Some(start) = open.take() {
                    let nums = code[start..i]
                        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.'))
                        .filter_map(|word| {
                            let word = word.replace('_', "");
                            let digits: String =
                                word.chars().take_while(char::is_ascii_digit).collect();
                            (!digits.is_empty() && SUFFIXES.contains(&&word[digits.len()..]))
                                .then(|| digits.parse().ok())
                                .flatten()
                        })
                        .collect();
                    out.push(nums);
                }
            }
            _ => {}
        }
    }
    out
}

/// `src` up to its first `#[cfg(test)]`: the code a build compiles.
fn before_tests(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

/// K16, the launcher's half: ONE anti-cheat table and ONE Steam category table.
///
/// - No source outside `src/steam_appinfo.rs` spells a store category (a `category_` literal, a
///   `*CATEGOR*` constant, a list of numbers holding two of the online categories' ids, under any
///   name) or an online-game code.
/// - Every code the signatures, the markers and the services can emit is in `ANTI_CHEAT_CODES`,
///   which holds each once.
/// - Each code is spelled as a literal exactly once in `src/`: in its one table (a signature row
///   in `src/detect.rs`, `codes` in `src/error.rs`, `VALVE_ANTI_CHEAT` in `src/plan.rs`).
#[test]
fn one_category_table_and_one_anti_cheat_table() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    assert!(files.len() >= 30, "{files:?}");
    let rel = |f: &Path| {
        f.strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|f| (rel(f), fs::read_to_string(f).unwrap()))
        .collect();

    // The category table.
    let online: Vec<&str> = ONLINE_CATEGORIES.iter().map(|(_, c)| *c).collect();
    for (path, src) in &sources {
        if path == "src/steam_appinfo.rs" {
            continue;
        }
        for lit in literals(src) {
            assert!(
                !lit.contains("category_"),
                "{path}: a category literal {lit:?}"
            );
            assert!(
                !online.contains(&lit.as_str()),
                "{path}: an online-game code {lit:?}"
            );
        }
        for list in number_lists(before_tests(src)) {
            let ids: Vec<u32> = ONLINE_CATEGORIES
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| list.contains(id))
                .collect();
            assert!(
                ids.len() < 2,
                "{path}: a list of Steam's online category ids {ids:?} outside src/steam_appinfo.rs: {list:?}"
            );
        }
        for line in before_tests(src).lines() {
            let t = line.trim();
            let item = t.strip_prefix("pub ").unwrap_or(t);
            let item = item.strip_prefix("(crate) ").unwrap_or(item);
            if let Some(name) = item
                .strip_prefix("const ")
                .or_else(|| item.strip_prefix("static "))
            {
                assert!(
                    !name
                        .to_ascii_uppercase()
                        .split(':')
                        .next()
                        .unwrap()
                        .contains("CATEGOR"),
                    "{path}: a category table outside src/steam_appinfo.rs: {t}"
                );
            }
        }
    }
    let appinfo = &sources
        .iter()
        .find(|(p, _)| p == "src/steam_appinfo.rs")
        .unwrap()
        .1;
    for code in &online {
        assert_eq!(
            literals(appinfo).iter().filter(|l| l == code).count(),
            1,
            "{code} once in src/steam_appinfo.rs"
        );
    }

    // The anti-cheat table: each code once in ANTI_CHEAT_CODES.
    let mut seen = std::collections::BTreeSet::new();
    for c in ANTI_CHEAT_CODES {
        assert!(seen.insert(*c), "{c} twice in ANTI_CHEAT_CODES");
        assert!(
            c.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{c}"
        );
    }
    for sig in ANTI_CHEAT_SIGNATURES {
        assert!(seen.contains(sig.code), "{}", sig.code);
        for n in sig.names {
            let spelled = match n {
                SignatureName::Folder(s) | SignatureName::File(s) => vec![*s],
                SignatureName::FileAround(a, b) => vec![*a, *b],
            };
            for s in spelled {
                assert_eq!(
                    s,
                    s.to_ascii_lowercase(),
                    "{}: names are lower case",
                    sig.code
                );
            }
        }
    }
    for c in [
        codes::LISTED_ANTI_CHEAT,
        codes::KERNEL_ANTI_CHEAT,
        codes::ANTI_CHEAT_UNKNOWN,
        VALVE_ANTI_CHEAT,
    ] {
        assert!(seen.contains(c), "{c}");
    }

    // Every code the signal code reaches through `codes::` is in the table.
    let error = &sources.iter().find(|(p, _)| p == "src/error.rs").unwrap().1;
    let values = str_consts(error);
    for path in ["src/detect.rs", "src/anti_cheat_services.rs"] {
        let src = before_tests(&sources.iter().find(|(p, _)| p == path).unwrap().1);
        let mut used = 0;
        for (i, _) in src.match_indices("codes::") {
            let name: String = src[i + "codes::".len()..]
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || *c == '_' || c.is_ascii_digit())
                .collect();
            if name.is_empty() {
                continue;
            }
            let value = values
                .get(&name)
                .unwrap_or_else(|| panic!("{path}: codes::{name}"));
            assert!(
                seen.contains(value.as_str()),
                "{path}: codes::{name} ({value}) is emitted but not in ANTI_CHEAT_CODES"
            );
            used += 1;
        }
        assert!(used >= 2, "{path}: {used} codes:: uses seen");
    }

    // Each code spelled in its one table and nowhere else: a signature's code in its row of
    // ANTI_CHEAT_SIGNATURES (where a name may be spelled as the code is: the `battleye` folder),
    // `listed-anti-cheat`, `kernel-anti-cheat` and `anti-cheat-unknown` once in `codes`, VAC once in
    // src/plan.rs.
    let detect = &sources
        .iter()
        .find(|(p, _)| p == "src/detect.rs")
        .unwrap()
        .1;
    let table_at = detect.find("pub const ANTI_CHEAT_SIGNATURES").unwrap();
    let table_len = detect[table_at..].find("\n];").unwrap();
    let table = &detect[table_at..table_at + table_len];
    let outside_table = format!("{}{}", &detect[..table_at], &detect[table_at + table_len..]);
    for code in ANTI_CHEAT_CODES {
        let count = |src: &str| literals(src).iter().filter(|l| l == code).count();
        let signature = ANTI_CHEAT_SIGNATURES.iter().any(|s| s.code == *code);
        let home = if signature {
            "src/detect.rs"
        } else if *code == VALVE_ANTI_CHEAT {
            "src/plan.rs"
        } else {
            "src/error.rs"
        };
        for (path, src) in &sources {
            if path != home {
                assert_eq!(
                    count(src),
                    0,
                    "{code}: spelled in {path}, outside its table"
                );
            }
        }
        if signature {
            assert!(count(table) >= 1, "{code}: its row");
            assert_eq!(
                count(&outside_table),
                0,
                "{code}: in src/detect.rs outside the table"
            );
        } else {
            let src = &sources.iter().find(|(p, _)| p == home).unwrap().1;
            assert_eq!(count(src), 1, "{code}: once in {home}");
        }
    }
}
