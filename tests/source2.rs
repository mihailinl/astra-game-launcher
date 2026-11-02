// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A new engine, end to end (merged plan §3.1, the G1 → G2 gate's "end-to-end Source 2 fixture
//! install"; plan B §5 step 3): the specification's own Source 2 foundation
//! (`manifests/valid/foundation-owns-engine.toml`, AstraPlugins' goldens as
//! `tests/goldens/astraplugins/PIN` vendors them) on a folder shaped as its `[integration.engine]`
//! describes. Its descriptor is in the engine set the daemon would detect with (unchecked,
//! publisher `someone`): Pass B names the game, planning asks the same descriptor and pins it,
//! the install plans again with the pin alone, Play re-reads the ledger, and the uninstall puts
//! the folder back byte for byte — while launchers 0.4.1 and 0.3.1 refuse the manifest outright,
//! and planning refuses wherever the descriptor is not the one that named the game.
//!
//! Step 5 of L3 adds the loader rules on its game (a foreign `dinput8.dll`, a kept `s2bridge.ini`,
//! the proxy-folder baseline); step 6 the restore of a withdrawn integration (`restore_game_files`:
//! the folder back, the profile kept, Play refused, then the uninstall).

mod common;
#[path = "common/spec_goldens.rs"]
mod spec_goldens;

use std::path::Path;

use astra_game_launcher::{
    Ctx, Descriptor, Engine, EngineSet, GameAction, GiPackage, KEPT_FOLDER_GAINED_FILES,
    LEDGER_FILE_NAME, Ledger, Manifest, Moderation, Origin, Platform, Provenance, codes,
    detect_with_engines, install, installed, plan_install, plan_install_in, prepare_launch,
    restore_game_files, uninstall,
};
use common::*;

/// The golden, below `testdata/games/`.
const GOLDEN: &str = "manifests/valid/foundation-owns-engine.toml";
const EXE: &str = "game/bin/win64/s2game.exe";

/// The golden's text, from the vendored copy, its bytes checked against `SHA256SUMS` first.
fn manifest_text() -> String {
    spec_goldens::sums();
    spec_goldens::golden_text(GOLDEN)
}

/// The golden's `[integration.engine]`, as the catalogue would carry it.
fn descriptor_of(text: &str) -> Descriptor {
    let table: toml::Table = toml::from_str(text).unwrap();
    Descriptor::from_toml_value(&table["integration"]["engine"]).unwrap()
}

/// The engine set the daemon detects with: the golden's descriptor, unchecked, published by
/// `someone`.
fn engines(d: &Descriptor) -> EngineSet {
    let set = EngineSet::build([(
        Origin {
            moderation: Moderation::Unchecked,
            publisher: "someone".to_owned(),
            first_party: false,
        },
        Ok(d.clone()),
    )]);
    assert_eq!(set.len(), 1, "admitted: {:?}", set.skipped());
    set
}

/// The zip: the manifest and one profile file for its `[[files]] from = "**"`.
fn zip_of(text: &str) -> Vec<u8> {
    zip(&[
        E::File("astra-gi.toml", text.as_bytes()),
        E::File("bridge/s2bridge.dll", b"MZ bridge"),
    ])
}

/// A folder shaped as the descriptor describes: `{project}/bin/win64/<program>.exe` (a PE)
/// with `engine2.dll` beside it, and `{project}/core/`.
fn source2_game(dir: &Path) {
    write(&dir.join(EXE), b"MZ\x90\0 the game");
    write(&dir.join("game/bin/win64/engine2.dll"), b"MZ engine");
    write(&dir.join("game/core/pak01_dir.vpk"), b"vpk");
}

#[test]
fn a_source2_foundation_installs_plays_and_uninstalls_on_a_source2_game() {
    let text = manifest_text();
    let d = descriptor_of(&text);
    let set = engines(&d);
    let dir = Tmp::new("s2-game");
    source2_game(dir.path());
    let before = snapshot(dir.path());

    // Pass B names the game, with the descriptor's digest.
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found.engine.as_str(), "source2");
    assert_eq!(found.descriptor_sha256.as_deref(), Some(d.digest()));
    assert_eq!(found.exe.as_deref(), Some(Path::new(EXE)));
    assert_eq!(found.evidence.as_deref(), Some(Path::new("game/core")));

    // Planning asks the same descriptor, and pins it.
    let zip = zip_of(&text);
    let pkg = open(&zip).unwrap();
    let game = target(dir.path(), EXE, None, Platform::Windows);
    let profiles = Tmp::new("s2-profiles");
    let profile = profiles.join("p");
    let plan = plan_install_in(&pkg, &game, &profile, &set, Provenance::Community, None).unwrap();
    assert_eq!(plan.provenance, Provenance::Community);
    let pinned = plan
        .engine_descriptor
        .as_ref()
        .expect("the descriptor is pinned");
    assert_eq!(pinned.sha256, found.descriptor_sha256.clone().unwrap());
    assert_eq!(pinned.canonical_json, d.canonical_json());
    assert_eq!(
        (
            pinned.moderation,
            pinned.publisher.as_str(),
            pinned.first_party
        ),
        (Moderation::Unchecked, "someone", false)
    );
    assert!(plan.game_files.is_empty());
    assert_eq!(plan.profile_files, 1);

    // The install plans again with the pin alone, and the ledger records both.
    let ledger = install(&pkg, &plan, &game, ACCEPT, &Ctx::silent()).unwrap();
    assert_eq!(ledger.provenance, Provenance::Community);
    assert_eq!(ledger.descriptor_sha256.as_deref(), Some(d.digest()));
    let text_on_disk = std::fs::read_to_string(profile.join(LEDGER_FILE_NAME)).unwrap();
    let on_disk: Ledger = serde_json::from_str(&text_on_disk).unwrap();
    assert_eq!(on_disk, ledger);
    // The new keys are ones an older launcher reads past (no schema change).
    let legacy: launcher_legacy::Ledger = serde_json::from_str(&text_on_disk).unwrap();
    assert_eq!(legacy.source.sha256, ledger.source.sha256);
    let v031: launcher_031::Ledger = serde_json::from_str(&text_on_disk).unwrap();
    assert_eq!(v031.game_files.len(), ledger.game_files.len());
    assert!(profile.join("bridge/s2bridge.dll").is_file());

    // Play: the ledger's manifest is parsed again, with exactly the variables 0.2.7 passes.
    let m = Manifest::parse(&ledger.manifest).unwrap();
    assert_eq!(m.engine_code().unwrap().as_str(), "source2");
    assert_eq!(
        m.engine_descriptor().unwrap().unwrap().digest(),
        d.digest(),
        "the ledger keeps the descriptor the plan pinned"
    );
    prepare_launch(&profile, &game, &vars(), None).unwrap();

    // Uninstall: the game folder byte for byte as it was.
    uninstall(&profile, &game).unwrap();
    assert_eq!(snapshot(dir.path()), before);
}

/// `[integration.engine.mods]` (engine-descriptor-v1.md §7), valid or refused inside: the
/// foundation detects, plans, installs and uninstalls exactly as without it, but for the digest
/// that names its descriptor — nothing that plans, installs or launches reads `mods`, and a
/// client drops one refused inside (§7.9). Only the lint (ingest's) differs: it refuses exactly
/// what is wrong inside `mods` (`mods-invalid`), and nothing for a valid one. It is part of that
/// descriptor: the catalogue's copy without it is another descriptor (`descriptor-mismatch`).
#[test]
fn a_foundation_whose_descriptor_carries_mods_installs_as_without_it() {
    let plain_text = manifest_text();
    let plain = descriptor_of(&plain_text);
    let plain_lint = Manifest::parse_and_lint(&plain_text, false).1;
    assert!(plain_lint.is_empty(), "{plain_lint:?}");
    let refused = "[integration.engine.mods]\nroot = \"{game}/Mods\"\nengine = { codes = { taken = \"Taken\" } }\n\n[integration.engine.mods.enabled]\nformat = \"dir-per-mod\"\n\n";
    let valid = "[[integration.engine.mods.loader]]\nid = \"s2-loader\"\nname = \"S2 Loader\"\nsource = [\"game\", \"manager\", \"picked\"]\nrequire = [ { stat = \"{source}/s2loader/s2loader.dll\", kind = \"file\" } ]\nroot = \"{source}/s2loader/mods\"\nenabled = [ { format = \"dir-per-mod\" } ]\ncoexist = [ { strategy = \"chain\", entry = \"{source}/s2loader/s2loader.dll\" } ]\n\n";
    for (mods, is_valid) in [(refused, false), (valid, true)] {
        let text = plain_text.replacen(
            "[integration.loader]\n",
            &format!("{mods}[integration.loader]\n"),
            1,
        );
        assert_ne!(text, plain_text);
        let d = descriptor_of(&text);
        assert!(d.carries_mods() && !plain.carries_mods());
        assert_eq!(d.mods().is_some(), is_valid, "{:?}", d.mods_findings());
        assert_ne!(d.digest(), plain.digest());
        let (m, lint) = Manifest::parse_and_lint(&text, false);
        assert_eq!(lint, d.mods_refusals(), "only what is wrong inside `mods`");
        assert_eq!(lint.is_empty(), is_valid);
        assert!(lint.iter().all(|e| e.code == codes::MODS_INVALID));
        assert!(
            m.unwrap()
                .engine_descriptor()
                .unwrap()
                .unwrap()
                .carries_mods()
        );

        let run = |d: &Descriptor, text: &str| {
            let set = engines(d);
            let dir = Tmp::new("s2-mods-game");
            source2_game(dir.path());
            let before = snapshot(dir.path());
            let mut found = detect_with_engines(dir.path(), None, &set).unwrap();
            assert_eq!(found.descriptor_sha256.as_deref(), Some(d.digest()));
            found.descriptor_sha256 = None;
            let pkg = open(&zip_of(text)).unwrap();
            let game = target(dir.path(), EXE, None, Platform::Windows);
            let profiles = Tmp::new("s2-mods-profiles");
            let profile = profiles.join("p");
            let plan =
                plan_install_in(&pkg, &game, &profile, &set, Provenance::Community, None).unwrap();
            let pinned = plan.engine_descriptor.clone().unwrap();
            assert_eq!(pinned.sha256, d.digest());
            let ledger = install(&pkg, &plan, &game, ACCEPT, &Ctx::silent()).unwrap();
            assert_eq!(ledger.descriptor_sha256.as_deref(), Some(d.digest()));
            prepare_launch(&profile, &game, &vars(), None).unwrap();
            uninstall(&profile, &game).unwrap();
            assert_eq!(snapshot(dir.path()), before);
            (
                found,
                plan.game_files,
                plan.profile_files,
                plan.provenance,
                (pinned.moderation, pinned.publisher, pinned.first_party),
                ledger.game_files,
            )
        };
        assert_eq!(run(&d, &text), run(&plain, &plain_text));

        // Launchers 0.4.1 and 0.3.1 refuse it as they refuse the foundation without it.
        let refusals = |text: &str| {
            let zip = zip_of(text);
            let sha = sha(&zip);
            (
                launcher_legacy::GiPackage::open(&zip, &sha)
                    .unwrap_err()
                    .code,
                launcher_031::GiPackage::open(&zip, &sha).unwrap_err().code,
            )
        };
        assert_eq!(refusals(&text), refusals(&plain_text));

        // The catalogue's descriptor without `mods` did not name this foundation's.
        let dir = Tmp::new("s2-mods-mismatch");
        source2_game(dir.path());
        let profiles = Tmp::new("s2-mods-profiles");
        let e = plan_install_in(
            &open(&zip_of(&text)).unwrap(),
            &target(dir.path(), EXE, None, Platform::Windows),
            &profiles.join("p"),
            &engines(&plain),
            Provenance::Community,
            None,
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.param("want"), e.param("got")),
            (
                codes::DESCRIPTOR_MISMATCH,
                Some(d.digest()),
                Some(plain.digest())
            )
        );
    }
}

/// Launcher 0.4.1 (what the daemon links before D1) and 0.3.1 (what Astra 0.2.7 ships) refuse
/// the manifest at parse: `[target] engine = "source2"` is no engine they know, so no older
/// Astra is ever offered a new engine's foundation.
#[test]
fn launchers_0_4_1_and_0_3_1_refuse_a_new_engines_manifest_at_parse() {
    let zip = zip_of(&manifest_text());
    let sha = sha(&zip);
    let legacy = launcher_legacy::GiPackage::open(&zip, &sha).unwrap_err();
    assert_eq!(
        legacy.code,
        launcher_legacy::codes::MANIFEST_INVALID,
        "{legacy:?}"
    );
    let v031 = launcher_031::GiPackage::open(&zip, &sha).unwrap_err();
    assert_eq!(v031.code, launcher_031::codes::MANIFEST_INVALID, "{v031:?}");
    // This launcher reads it.
    GiPackage::open(&zip, &sha).unwrap();
}

/// The manifest's own `[integration.engine]` must be the very descriptor that named the game:
/// one byte off is another descriptor, `descriptor-mismatch{want, got}`.
#[test]
fn a_foundation_whose_descriptor_differs_by_one_byte_is_descriptor_mismatch() {
    let text = manifest_text();
    let d = descriptor_of(&text);
    let set = engines(&d);
    let changed = text.replacen("first = 8", "first = 7", 1);
    assert_ne!(changed, text);
    let other = descriptor_of(&changed);
    assert_ne!(other.digest(), d.digest());
    let dir = Tmp::new("s2-game");
    source2_game(dir.path());
    let profiles = Tmp::new("s2-profiles");
    let pkg = open(&zip_of(&changed)).unwrap();
    let e = plan_install_in(
        &pkg,
        &target(dir.path(), EXE, None, Platform::Windows),
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("want"), e.param("got")),
        (
            codes::DESCRIPTOR_MISMATCH,
            Some(other.digest()),
            Some(d.digest())
        )
    );
}

/// Without the descriptor in the set, nothing names the game's engine: `engine-mismatch`,
/// `got` = `unknown` — `plan_install`, and `plan_install_in` with an empty set.
#[test]
fn without_its_descriptor_a_source2_game_is_engine_mismatch() {
    let dir = Tmp::new("s2-game");
    source2_game(dir.path());
    let profiles = Tmp::new("s2-profiles");
    let pkg = open(&zip_of(&manifest_text())).unwrap();
    let game = target(dir.path(), EXE, None, Platform::Windows);
    for e in [
        plan_install(&pkg, &game, &profiles.join("p")).unwrap_err(),
        plan_install_in(
            &pkg,
            &game,
            &profiles.join("p"),
            &EngineSet::empty(),
            Provenance::Community,
            None,
        )
        .unwrap_err(),
    ] {
        assert_eq!(
            (e.code, e.param("want"), e.param("got")),
            (codes::ENGINE_MISMATCH, Some("source2"), Some("unknown"))
        );
    }
}

/// A Unity game that also ships an `engine2.dll` beside its program, in a folder of the same
/// shape: Pass A decided it, so it stays Unity whatever the set holds, and the Source 2
/// foundation is refused on it.
#[test]
fn a_unity_game_with_engine2_beside_its_program_stays_unity() {
    let text = manifest_text();
    let d = descriptor_of(&text);
    let set = engines(&d);
    let dir = Tmp::new("s2-unity");
    unity_game(&dir.join("game/bin/win64"), "s2game");
    write(&dir.join("game/bin/win64/engine2.dll"), b"MZ engine");
    write(&dir.join("game/core/pak01_dir.vpk"), b"vpk");
    let found = detect_with_engines(dir.path(), None, &set).unwrap();
    assert_eq!(found.engine, Engine::UnityMono);
    assert_eq!(found.descriptor_sha256, None);
    let profiles = Tmp::new("s2-profiles");
    let pkg = open(&zip_of(&text)).unwrap();
    let e = plan_install_in(
        &pkg,
        &target(dir.path(), EXE, None, Platform::Windows),
        &profiles.join("p"),
        &set,
        Provenance::Community,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("want"), e.param("got")),
        (codes::ENGINE_MISMATCH, Some("source2"), Some("unity-mono"))
    );
}

/// The foundation's own proxy and the config it keeps, as the loader tests place them.
const PROXY: &[u8] = b"MZ the foundation's dinput8 proxy";
const BRIDGE_INI: &[u8] = b"[bridge]\nport = 0\n";

/// The golden placing both files its `[integration.loader]` names beside the program:
/// `dinput8.dll` (a declared proxy) and `s2bridge.ini` (kept when present).
fn with_loader_files(golden: &str) -> GiPackage {
    let text = format!(
        "{golden}\n[[game_files]]\nfrom = \"loader/dinput8.dll\"\nto = \"${{game}}/dinput8.dll\"\nsha256 = \"{}\"\n\n[[game_files]]\nfrom = \"loader/s2bridge.ini\"\nto = \"${{game}}/s2bridge.ini\"\nsha256 = \"{}\"\n",
        sha(PROXY),
        sha(BRIDGE_INI)
    );
    open(&zip(&[
        E::File("astra-gi.toml", text.as_bytes()),
        E::File("bridge/s2bridge.dll", b"MZ bridge"),
        E::File("loader/dinput8.dll", PROXY),
        E::File("loader/s2bridge.ini", BRIDGE_INI),
    ]))
    .unwrap()
}

/// The foundation's own `[integration.loader]` (`proxies = ["dinput8.dll"]`, `keep_if_present =
/// ["s2bridge.ini"]`) on its game, the golden placing both beside the program: a foreign
/// `dinput8.dll` is another loader, never replaced; a foreign `s2bridge.ini` is the player's and
/// stays; and a file that arrives beside our proxy after the install keeps it at uninstall
/// (`folder-gained-files`), the player's `s2bridge.ini` untouched.
#[test]
fn the_source2_foundations_loader_rules_hold_on_its_game() {
    let golden = manifest_text();
    let d = descriptor_of(&golden);
    let set = engines(&d);
    let proxy = PROXY;
    let pkg = with_loader_files(&golden);
    let dir = Tmp::new("s2-loader");
    source2_game(dir.path());
    let beside = dir.join("game/bin/win64");
    let game = target(dir.path(), EXE, None, Platform::Windows);
    let profiles = Tmp::new("s2-loader-profiles");
    let profile = profiles.join("p");
    let plan =
        |pkg: &GiPackage| plan_install_in(pkg, &game, &profile, &set, Provenance::Community, None);

    // A foreign dinput8.dll: another loader.
    write(&beside.join("dinput8.dll"), b"MZ a loader of the player's");
    let e = plan(&pkg).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::FOREIGN_LOADER, Some("dinput8.dll"))
    );
    std::fs::remove_file(beside.join("dinput8.dll")).unwrap();

    // A foreign s2bridge.ini: the player's, kept.
    let theirs: &[u8] = b"[bridge]\nport = 4242\n";
    write(&beside.join("s2bridge.ini"), theirs);
    let p = plan(&pkg).unwrap();
    let actions: Vec<(&str, GameAction)> = p
        .game_files
        .iter()
        .map(|g| (g.rel_path.as_str(), g.action))
        .collect();
    assert_eq!(
        actions,
        [
            ("dinput8.dll", GameAction::Add),
            ("s2bridge.ini", GameAction::KeepExisting)
        ]
    );
    install(&pkg, &p, &game, ACCEPT, &Ctx::silent()).unwrap();
    let ledger = installed(&profile).unwrap().unwrap();
    assert_eq!(ledger.proxy_folders.len(), 1);
    assert_eq!(
        ledger.proxy_folders[0].names,
        ["engine2.dll", "s2bridge.ini", "s2game.exe"]
    );

    // A file arrives beside our proxy: the uninstall leaves it, and says why.
    write(&beside.join("scripts/their_plugin.asi"), b"MZ theirs");
    let r = uninstall(&profile, &game).unwrap();
    assert_eq!(r.kept, ["dinput8.dll"]);
    assert_eq!(
        r.kept_reasons.get("dinput8.dll").map(String::as_str),
        Some(KEPT_FOLDER_GAINED_FILES)
    );
    assert_eq!(r.warnings, [codes::RESTORE_PARTIAL], "ours is still there");
    assert_eq!(std::fs::read(beside.join("dinput8.dll")).unwrap(), proxy);
    assert_eq!(std::fs::read(beside.join("s2bridge.ini")).unwrap(), theirs);
}

/// A withdrawn Source 2 foundation (a revoke, A§4.4): `restore_game_files` puts the game folder
/// back byte for byte and keeps the profile; Play is then refused; the uninstall removes the
/// profile.
#[test]
fn a_restored_source2_foundation_refuses_play_and_uninstalls() {
    let golden = manifest_text();
    let set = engines(&descriptor_of(&golden));
    let pkg = with_loader_files(&golden);
    let dir = Tmp::new("s2-restore");
    source2_game(dir.path());
    let before = snapshot(dir.path());
    let game = target(dir.path(), EXE, None, Platform::Windows);
    let profiles = Tmp::new("s2-restore-profiles");
    let profile = profiles.join("p");
    let plan = plan_install_in(&pkg, &game, &profile, &set, Provenance::Community, None).unwrap();
    install(&pkg, &plan, &game, ACCEPT, &Ctx::silent()).unwrap();
    prepare_launch(&profile, &game, &vars(), None).unwrap();

    let r = restore_game_files(&profile, &game).unwrap();
    assert_eq!(r.removed, ["s2bridge.ini", "dinput8.dll"]);
    assert!(r.kept.is_empty() && r.warnings.is_empty(), "{r:?}");
    assert_eq!(
        snapshot(dir.path()),
        before,
        "the game folder byte for byte"
    );
    assert!(
        profile.join("bridge/s2bridge.dll").is_file(),
        "the profile is kept"
    );
    let restored = installed(&profile).unwrap().unwrap();
    assert_eq!(restored.state, astra_game_launcher::LedgerState::Restored);
    assert_eq!(
        restored.descriptor_sha256,
        plan.engine_descriptor.map(|d| d.sha256)
    );

    let e = prepare_launch(&profile, &game, &vars(), None).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::NEEDS_REINSTALL, Some("restored"))
    );

    let r = uninstall(&profile, &game).unwrap();
    assert_eq!(r.profile_files_removed as usize, restored.files.len());
    assert_eq!(installed(&profile).unwrap(), None);
    assert!(!profile.join("bridge/s2bridge.dll").exists());
    assert_eq!(snapshot(dir.path()), before);
}
