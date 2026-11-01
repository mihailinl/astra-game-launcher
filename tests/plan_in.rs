// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `plan_install_in`: planning that asks the engine descriptors (plan B §5 step 3, debt row 10).
//!
//! - A descriptor's code plans only where the descriptor Pass B would choose names the game's
//!   program: never in a folder Pass A decided (with Steam's launch hint, as the daemon
//!   detects), never on a program Pass B formed no match for, and — where descriptors of one
//!   standing disagree about the program — only with the player's pick (`engine-ambiguous`).
//! - The plan pins the descriptor and records the provenance; a first-party plan of a built-in
//!   engine's manifest serialises exactly as `plan_install`'s (launcher 0.4.1's).
//! - Install plans again with the pin alone, parsed again and its digest checked: a plan whose
//!   pin was changed is `plan-stale`.

mod common;

use std::fs;
use std::path::Path;

use astra_game_launcher::{
    Ctx, Descriptor, EngineCode, EngineSet, GameTarget, GiPackage, Moderation, Origin, Platform,
    Provenance, codes, detect_with_engines, install, plan_install, plan_install_in,
};
use common::*;

/// The Source 2 shape (the spec's own example), under another `code`.
fn descriptor_text(code: &str) -> String {
    format!(
        "schema = \"astra.engine/1\"\nengine = {{ codes = {{ {code} = \"Engine {code}\" }} }}\n[[rule]]\ntier = \"strong\"\nprograms = {{ ext = [\"exe\"], at = \"{{project}}/bin/win64\", first = 8 }}\nrequire = [ {{ listed = \"engine2.dll\", kind = \"file\" }}, {{ child = \"{{project}}/core\", kind = \"dir\", as = \"core\" }} ]\ncode = \"{code}\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{{core}}\"\n"
    )
}

fn descriptor(code: &str) -> Descriptor {
    Descriptor::from_toml(&descriptor_text(code)).unwrap()
}

/// One unchecked descriptor per code, each from its own publisher.
fn set_of(codes: &[&str]) -> EngineSet {
    let set = EngineSet::build(codes.iter().map(|c| {
        (
            Origin {
                moderation: Moderation::Unchecked,
                publisher: format!("pub-{c}"),
                first_party: false,
            },
            Ok(descriptor(c)),
        )
    }));
    assert_eq!(set.len(), codes.len(), "{:?}", set.skipped());
    set
}

/// A foundation built on `code` that does not own it: `[target] engine` only, no
/// `[integration.engine]` (merged plan §2.1, "many foundations per code").
fn foundation(code: &str) -> GiPackage {
    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"someone.{code}-runner\"\nname = \"A runner\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n\n[target]\nengine = \"{code}\"\nexe = \"*\"\nplatforms = [\"windows-x64\", \"linux-proton\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"**\"\nto = \"${{profile}}/\"\n"
    );
    open(&zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("runner/r.dll", b"MZ runner"),
    ]))
    .unwrap()
}

/// `{project}/bin/win64/<name>.exe` (a PE), `engine2.dll` beside it, `{project}/core/`.
fn shaped(dir: &Path, project: &str, name: &str) {
    write(
        &dir.join(format!("{project}/bin/win64/{name}.exe")),
        b"MZ\x90\0 a game",
    );
    write(
        &dir.join(format!("{project}/bin/win64/engine2.dll")),
        b"MZ engine",
    );
    write(&dir.join(format!("{project}/core/pak01_dir.vpk")), b"vpk");
}

fn game(dir: &Path, exe: &str) -> GameTarget {
    target(dir, exe, None, Platform::Windows)
}

fn code(c: &str) -> EngineCode {
    EngineCode::new(c).unwrap()
}

/// Where two descriptors of one standing both name the program, neither code is preselected:
/// `engine-ambiguous{want, codes}` unless the pick is the manifest's code — then it plans, pins
/// that code's descriptor, and installs (planned again with the pin alone, no pick needed).
#[test]
fn an_ambiguous_game_plans_only_with_the_pick_of_the_manifests_code() {
    let dir = Tmp::new("ambiguous");
    shaped(dir.path(), "game", "g");
    let set = set_of(&["alpha", "beta"]);
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found.engine_candidates, [code("alpha"), code("beta")]);
    let profiles = Tmp::new("ambiguous-profiles");
    let profile = profiles.join("p");
    let g = game(dir.path(), "game/bin/win64/g.exe");
    let pkg = foundation("alpha");
    let plan = |pick: Option<&str>| {
        let pick = pick.map(code);
        plan_install_in(
            &pkg,
            &g,
            &profile,
            &set,
            Provenance::Community,
            pick.as_ref(),
        )
    };
    for pick in [None, Some("beta"), Some("gamma")] {
        let e = plan(pick).unwrap_err();
        assert_eq!(
            (e.code, e.param("want"), e.param("codes")),
            (codes::ENGINE_AMBIGUOUS, Some("alpha"), Some("alpha,beta")),
            "pick {pick:?}"
        );
    }
    let planned = plan(Some("alpha")).unwrap();
    let pinned = planned.engine_descriptor.as_ref().unwrap();
    assert_eq!(pinned.sha256, descriptor("alpha").digest());
    assert_eq!(pinned.publisher, "pub-alpha");
    install(&pkg, &planned, &g, ACCEPT, &Ctx::silent()).unwrap();

    // A code no descriptor of the game names, picked or not: no pick makes it the game's.
    let gamma = foundation("gamma");
    let pick = code("gamma");
    let e = plan_install_in(
        &gamma,
        &g,
        &profiles.join("q"),
        &set,
        Provenance::Community,
        Some(&pick),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("want"), e.param("got")),
        (codes::ENGINE_MISMATCH, Some("gamma"), Some("unknown"))
    );
}

/// The program planned is the one Pass B's match must be for: a game Pass B names with one
/// program says nothing about another program of the same folder that no descriptor matched.
#[test]
fn a_program_no_descriptor_matched_is_named_by_none() {
    let dir = Tmp::new("restricted");
    shaped(dir.path(), "game", "g");
    // The same shape of folder, with nothing beside its program.
    write(&dir.join("tools/bin/win64/t.exe"), b"MZ\x90\0 a tool");
    write(&dir.join("tools/core/x.vpk"), b"vpk");
    let set = set_of(&["alpha"]);
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found.engine.as_str(), "alpha");
    let profiles = Tmp::new("restricted-profiles");
    let pkg = foundation("alpha");
    let plan = |exe: &str, p: &str| {
        plan_install_in(
            &pkg,
            &game(dir.path(), exe),
            &profiles.join(p),
            &set,
            Provenance::Community,
            None,
        )
    };
    plan("game/bin/win64/g.exe", "p").unwrap();
    let e = plan("tools/bin/win64/t.exe", "q").unwrap_err();
    assert_eq!(
        (e.code, e.param("want"), e.param("got")),
        (codes::ENGINE_MISMATCH, Some("alpha"), Some("unknown"))
    );
}

/// Where Pass A decided the game — a Unity game here — no program in its folder plans as a
/// descriptor's code, though one sits there in exactly the shape the descriptor describes.
#[test]
fn a_folder_pass_a_decided_plans_no_descriptors_code() {
    let dir = Tmp::new("closed");
    unity_game(dir.path(), "Tool");
    shaped(dir.path(), "game", "g");
    let set = set_of(&["alpha"]);
    assert_eq!(
        detect_with_engines(dir.path(), None, &set)
            .unwrap()
            .engine
            .as_str(),
        "unity-mono"
    );
    let profiles = Tmp::new("closed-profiles");
    let e = plan_install_in(
        &foundation("alpha"),
        &game(dir.path(), "game/bin/win64/g.exe"),
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("want"), e.param("got")),
        (codes::ENGINE_MISMATCH, Some("alpha"), Some("unknown"))
    );
}

/// Planning asks Pass A and Pass B with the program Steam launches for the trusted appid, as the
/// daemon detects: a hint into a Unity game deeper than the walk closes the folder (Pass A used
/// it), and a hint naming another program of the descriptor's shape takes the match from the
/// one planned (Pass B prefers the hint's). Without an appid there is no hint, and both plan.
#[test]
fn planning_detects_with_steams_launch_hint_for_the_trusted_appid() {
    const APPID: u32 = 4_000_101;
    let dir = Tmp::new("hinted");
    shaped(dir.path(), "game", "g");
    shaped(dir.path(), "other", "o");
    // A Unity game below the walk's depth: only a hint reaches it.
    unity_game(&dir.join("a/b/c/d/e"), "Deep");
    let set = set_of(&["alpha"]);
    let profiles = Tmp::new("hinted-profiles");
    let pkg = foundation("alpha");
    let world = Tmp::new("hinted-steam");
    let plan = |hint: Option<&str>, exe: &str, p: &str| {
        let mut g = game(dir.path(), exe);
        if let Some(hint) = hint {
            let appinfo = world.join(&format!("{p}.vdf"));
            fs::write(&appinfo, appinfo_launch(&[(APPID, hint, "windows")])).unwrap();
            assert_eq!(
                astra_game_launcher::launch_executable(&appinfo, APPID, "windows").as_deref(),
                Some(hint)
            );
            g.steam_appid = Some(APPID);
            g.steam_appinfo = Some(appinfo);
        }
        plan_install_in(
            &pkg,
            &g,
            &profiles.join(p),
            &set,
            Provenance::Community,
            None,
        )
    };
    let g = "game/bin/win64/g.exe";
    plan(None, g, "a").unwrap();
    let deep = "a/b/c/d/e/Deep.exe";
    assert_eq!(
        detect_with_engines(dir.path(), Some(deep), &set)
            .unwrap()
            .engine
            .as_str(),
        "unity-mono"
    );
    let e = plan(Some(deep), g, "b").unwrap_err();
    assert_eq!(
        (e.code, e.param("got")),
        (codes::ENGINE_MISMATCH, Some("unknown")),
        "Pass A used the hint: the folder is closed"
    );
    let other = "other/bin/win64/o.exe";
    let hinted = detect_with_engines(dir.path(), Some(other), &set).unwrap();
    assert_eq!(hinted.exe.as_deref(), Some(Path::new(other)));
    let e = plan(Some(other), g, "c").unwrap_err();
    assert_eq!(
        (e.code, e.param("got")),
        (codes::ENGINE_MISMATCH, Some("unknown")),
        "Pass B's match is the hint's program"
    );
    plan(Some(other), other, "d").unwrap();
}

/// A first-party plan of a built-in engine's manifest has neither new key, and is
/// `plan_install`'s to the byte, whatever set is passed; a community plan of a descriptor's
/// engine has both.
#[test]
fn only_a_community_or_descriptor_plan_carries_the_new_keys() {
    let unity = Tmp::new("keys-unity");
    unity_game(unity.path(), "Game");
    let profiles = Tmp::new("keys-profiles");
    let g = game(unity.path(), "Game.exe");
    let pkg = runner();
    let old = serde_json::to_string(&plan_install(&pkg, &g, &profiles.join("p")).unwrap()).unwrap();
    let set = set_of(&["alpha"]);
    for engines in [&EngineSet::empty(), &set] {
        let new = plan_install_in(
            &pkg,
            &g,
            &profiles.join("p"),
            engines,
            Provenance::FirstParty,
            None,
        )
        .unwrap();
        assert_eq!(serde_json::to_string(&new).unwrap(), old);
    }
    assert!(!old.contains("\"provenance\"") && !old.contains("\"engine_descriptor\""));
    // A community runner declares the loader it places (`loader-undeclared` otherwise).
    let declared = open(&runner_zip(&runner_manifest(
        "\n[integration.loader]\nproxies = [\"winhttp.dll\"]\n",
    )))
    .unwrap();
    let community = plan_install_in(
        &declared,
        &g,
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap();
    let json = serde_json::to_value(&community).unwrap();
    assert_eq!(json["provenance"], "community");
    assert!(json.get("engine_descriptor").is_none());

    let s2 = Tmp::new("keys-s2");
    shaped(s2.path(), "game", "g");
    let plan = plan_install_in(
        &foundation("alpha"),
        &game(s2.path(), "game/bin/win64/g.exe"),
        &profiles.join("q"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap();
    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["provenance"], "community");
    assert_eq!(
        json["engine_descriptor"]["sha256"],
        descriptor("alpha").digest()
    );
    assert_eq!(json["engine_descriptor"]["moderation"], "unchecked");
    // It reads back as it was written.
    let back: astra_game_launcher::InstallPlan = serde_json::from_value(json).unwrap();
    assert_eq!(back, plan);
}

/// Install plans again with the descriptor the plan pinned, parsed again from its canonical
/// JSON: a pin whose text or digest was changed is `plan-stale`, and nothing is placed.
#[test]
fn a_plan_whose_pinned_descriptor_was_changed_is_stale_at_install() {
    let dir = Tmp::new("tampered");
    shaped(dir.path(), "game", "g");
    let set = set_of(&["alpha"]);
    let profiles = Tmp::new("tampered-profiles");
    let profile = profiles.join("p");
    let g = game(dir.path(), "game/bin/win64/g.exe");
    let pkg = foundation("alpha");
    let plan = plan_install_in(&pkg, &g, &profile, &set, Provenance::Community, None).unwrap();
    let pinned = plan.engine_descriptor.clone().unwrap();
    let other = descriptor("beta");
    let tampered = [
        // Another descriptor's text under this one's digest.
        {
            let mut p = plan.clone();
            p.engine_descriptor.as_mut().unwrap().canonical_json =
                other.canonical_json().to_owned();
            p
        },
        // One byte of the text.
        {
            let mut p = plan.clone();
            p.engine_descriptor.as_mut().unwrap().canonical_json =
                pinned
                    .canonical_json
                    .replacen("\"first\":8", "\"first\":7", 1);
            assert_ne!(
                p.engine_descriptor.as_ref().unwrap().canonical_json,
                pinned.canonical_json
            );
            p
        },
        // Another digest.
        {
            let mut p = plan.clone();
            p.engine_descriptor.as_mut().unwrap().sha256 = other.digest().to_owned();
            p
        },
        // Not JSON at all.
        {
            let mut p = plan.clone();
            p.engine_descriptor.as_mut().unwrap().canonical_json = "{".to_owned();
            p
        },
    ];
    for p in tampered {
        let e = install(&pkg, &p, &g, ACCEPT, &Ctx::silent()).unwrap_err();
        assert_eq!(e.code, codes::PLAN_STALE, "{e:?}");
        assert!(!profile.join("runner/r.dll").exists());
    }
    // A plan stripped of its pin plans again without the descriptor: the engine is nobody's.
    let mut unpinned = plan.clone();
    unpinned.engine_descriptor = None;
    let e = install(&pkg, &unpinned, &g, ACCEPT, &Ctx::silent()).unwrap_err();
    assert_eq!(e.code, codes::ENGINE_MISMATCH, "{e:?}");
    // The plan as made installs.
    install(&pkg, &plan, &g, ACCEPT, &Ctx::silent()).unwrap();
    assert!(profile.join("runner/r.dll").is_file());
}

/// A claim on every program, Astra's own and moderated, so nothing but the evaluator's own
/// rules shields a program from it.
fn everything() -> EngineSet {
    let set = EngineSet::build([(
        Origin {
            moderation: Moderation::Moderated,
            publisher: "astra".to_owned(),
            first_party: true,
        },
        Descriptor::from_toml(
            "schema = \"astra.engine/1\"\nengine = { codes = { everything = \"Everything\" } }\n[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\", \"x86_64\", \"x86\", \"\"], first = 8, skip_wrappers = false }\ncode = \"everything\"\nconfidence = \"high\"\nevidence = \"{program}\"\nbinary = \"by-ext\"\n",
        ),
    )]);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    set
}

/// Pass B names only a program planning could plan as a descriptor's engine: never one a
/// built-in rule names when asked about it alone — planning asks the built-ins first, so a
/// descriptor naming it would name a game every plan for it refuses. An Unreal shipping program
/// below a launcher's folder is such a one: Pass A leaves the game open (the walk skips the
/// launcher's folder), and planning's built-in half says Unreal.
#[test]
fn pass_b_never_names_a_program_the_built_ins_name_alone() {
    let dir = Tmp::new("builtin-program");
    let shipping = "launcher/Proj/Binaries/Win64/Proj-Win64-Shipping.exe";
    write(&dir.join(shipping), b"MZ\x90\0 a shipping build");
    let set = everything();
    let alone = detect_with_engines(dir.path(), None, &EngineSet::empty()).unwrap();
    assert!(alone.open_to_descriptors(), "{alone:?}");
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found, alone, "no descriptor names the shipping program");
    // On Steam's hint too.
    let hinted = detect_with_engines(dir.path(), Some(shipping), &set).unwrap();
    assert_eq!(hinted.descriptor_sha256, None, "{hinted:?}");
    let profiles = Tmp::new("builtin-program-profiles");
    let e = plan_install_in(
        &foundation("everything"),
        &game(dir.path(), shipping),
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("got")),
        (codes::ENGINE_MISMATCH, Some("unreal"))
    );
    // Beside it, a program no built-in rule knows is the descriptor's, and plans as it.
    write(&dir.join("Other.exe"), b"MZ\x90\0 another program");
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(
        (found.engine.as_str(), found.exe.as_deref()),
        ("everything", Some(Path::new("Other.exe")))
    );
    plan_install_in(
        &foundation("everything"),
        &game(dir.path(), "Other.exe"),
        &profiles.join("q"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap();
}

/// Nor one whose path the launcher refuses on some platform — a Windows device name, which a
/// Linux disk holds — since no plan can name it as the game's program.
#[cfg(unix)]
#[test]
fn pass_b_never_names_a_program_no_plan_can_name() {
    let dir = Tmp::new("device-program");
    write(&dir.join("Odd/lpt3.x.exe"), b"MZ\x90\0 a device's name");
    let set = everything();
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found.descriptor_sha256, None, "{found:?}");
    write(&dir.join("Odd/fine.exe"), b"MZ\x90\0 a program");
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found.exe.as_deref(), Some(Path::new("Odd/fine.exe")));
}

/// Plan B §3, "Anti-cheat the list misses" (the spec's hostile `anticheat-list-misses` shape,
/// its `{project}` capture fixed): a descriptor that declares no anti-cheat of its own claims a
/// folder that ships a kernel driver no list names. Detection says `anti-cheat-unknown`; planning
/// a community foundation on that claim refuses it, as it refuses every other signal — and so
/// does the install of a plan made before the driver arrived, which plans again.
#[test]
fn a_claim_the_walk_cannot_vouch_for_is_refused_as_anti_cheat() {
    const SNEAKY: &str = "schema = \"astra.engine/1\"\n[engine]\ncodes = { sneaky-engine = \"Sneaky Engine\" }\n[[rule]]\ntier = \"weak\"\nprograms = { ext = [\"exe\"], first = 8 }\nrequire = [ { listed = \"sneaky-engine-core.dll\", kind = \"file\" } ]\ncode = \"sneaky-engine\"\nconfidence = \"low\"\nbinary = \"windows\"\nevidence = \"{program}\"\n";
    let set = EngineSet::build([(
        Origin {
            moderation: Moderation::Unchecked,
            publisher: "sneaky".to_owned(),
            first_party: false,
        },
        Descriptor::from_toml(SNEAKY),
    )]);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let dir = Tmp::new("ac-unknown");
    write(&dir.join("Sneaky.exe"), b"MZ\x90\0 a game");
    write(&dir.join("sneaky-engine-core.dll"), b"MZ core");
    let g = game(dir.path(), "Sneaky.exe");
    let pkg = foundation("sneaky-engine");
    let profiles = Tmp::new("ac-unknown-profiles");
    let profile = profiles.join("p");
    let refused =
        |e: astra_game_launcher::LauncherError| (e.code, e.param("kind").map(str::to_owned));
    let want = (
        codes::ANTI_CHEAT,
        Some(codes::ANTI_CHEAT_UNKNOWN.to_owned()),
    );
    let plan = plan_install_in(&pkg, &g, &profile, &set, Provenance::Community, None).unwrap();
    // The driver arrives between the consent sheet and the install.
    write(&dir.join("drivers/newguard.sys"), b"MZ a kernel driver");
    assert_eq!(
        detect_with_engines(dir.path(), None, &set)
            .unwrap()
            .anti_cheat,
        [codes::ANTI_CHEAT_UNKNOWN]
    );
    let again = plan_install_in(&pkg, &g, &profile, &set, Provenance::Community, None);
    assert_eq!(refused(again.unwrap_err()), want);
    assert_eq!(
        refused(install(&pkg, &plan, &g, ACCEPT, &Ctx::silent()).unwrap_err()),
        want,
        "install plans again with the pin, and refuses the same"
    );
    assert!(!dir.join("runner").exists() && !profile.join("runner").exists());
}

/// A descriptor's own anti-cheat markers (its `anti_cheat` scoped additions) refuse a plan on
/// its claim, with the marker's code; where two descriptors stand, refused before any pick is
/// asked for — every standing descriptor's markers and the driver check count, whichever code
/// the player would pick.
#[test]
fn a_claims_own_markers_refuse_the_plan_before_a_pick() {
    let guarded = descriptor_text("alpha").replace(
        "[[rule]]",
        "anti_cheat = [ { listed = \"Guard\", kind = \"dir\", code = \"alpha-guard\" } ]\n[[rule]]",
    );
    let origin = |p: &str| Origin {
        moderation: Moderation::Unchecked,
        publisher: p.to_owned(),
        first_party: false,
    };
    let set = EngineSet::build([(origin("a"), Descriptor::from_toml(&guarded))]);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let dir = Tmp::new("ac-markers");
    shaped(dir.path(), "game", "g");
    let g = game(dir.path(), "game/bin/win64/g.exe");
    let profiles = Tmp::new("ac-markers-profiles");
    let pkg = foundation("alpha");
    plan_install_in(
        &pkg,
        &g,
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap();
    write(&dir.join("game/Guard/guard.bin"), b"a guard");
    let e = plan_install_in(
        &pkg,
        &g,
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("kind")),
        (codes::ANTI_CHEAT, Some("alpha-guard"))
    );

    // Ambiguous: refused before the pick, whichever is picked.
    let both = EngineSet::build([
        (origin("a"), Descriptor::from_toml(&guarded)),
        (origin("b"), Descriptor::from_toml(&descriptor_text("beta"))),
    ]);
    assert_eq!(both.len(), 2, "{:?}", both.skipped());
    for pick in [None, Some(code("alpha")), Some(code("beta"))] {
        for want in ["alpha", "beta"] {
            let e = plan_install_in(
                &foundation(want),
                &g,
                &profiles.join("q"),
                &both,
                Provenance::Community,
                pick.as_ref(),
            )
            .unwrap_err();
            assert_eq!(
                (e.code, e.param("kind")),
                (codes::ANTI_CHEAT, Some("alpha-guard")),
                "{want}, pick {pick:?}"
            );
        }
    }
}

