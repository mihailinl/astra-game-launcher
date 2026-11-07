// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a community integration may and may not do in the game folder (plan B §3; merged plan
//! R15, R16, R42, §2.5), one hostile fixture per row, each beside an Astra's-own control that
//! shows what the provenance changes — or that it changes nothing, where the rule is every
//! game's and every integration's:
//!
//! - **Foreign loader → keep**: a file by a name loaders hijack (`HIJACK_NAMES`, or the
//!   manifest's `[integration.loader] proxies`) that Astra did not place is never replaced;
//!   keep names come from `[integration.loader]` for this install only, and never overlap a
//!   proxy (`loader-policy-conflict`).
//! - **Overwrite a player's file**: a community integration has no `replace` arm
//!   (`foreign-file`).
//! - **Plant code in another loader's folder**: `[[game_files]]` only beside the program or below
//!   a folder named after the id, never below `BepInEx/`, `MelonLoader/`, `ue4ss/`, `Mods/`,
//!   `~mods/`; a loader's name only when declared (`loader-undeclared`, K18); the program a
//!   walked one (`community-limit{rule=exe-not-a-program}`).
//! - **Abuse `[launch]`**: no `env`; overrides only for DLLs it places, `native,builtin` or `n,b`;
//!   path arguments only under `${profile}/` or `${game}/`.
//! - **Uninstall deletes their identical loader**: a hijack-named file we added stays when its
//!   folder gained, since the install, a name that can carry a loader (`kept_reasons` =
//!   `folder-gained-files`), its Proton override with it.
//!
//! And the loader policy never widens Play: only the Doorstop-sharing names are another
//! manager's to relink.

mod common;

use std::fs;
use std::path::Path;

use astra_game_launcher::{
    EngineSet, GameAction, GameTarget, GiPackage, InstallPlan, KEPT_FOLDER_GAINED_FILES,
    LauncherError, Manifest, Platform, Provenance, codes, install, installed, plan_install_in,
    prepare_launch, uninstall,
};
use common::*;

const ID: &str = "someone.example";
/// The game's program: a PE, at the top of its folder.
const PROGRAM: &[u8] = b"MZ\x90\0 the game";

/// A release of `id` placing each `(destination below ${game}, bytes)`, with `extra` TOML after
/// everything else (`[integration.loader]`, `[launch]`).
fn release(id: &str, files: &[(&str, &[u8])], extra: &str) -> GiPackage {
    let mut manifest = format!(
        "schema = 1\n\n[integration]\nid = \"{id}\"\nname = \"Astra in Example\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n\n[target]\nexe = \"*\"\nplatforms = [\"windows-x64\", \"linux-proton\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"p.txt\"\nto = \"${{profile}}/p.txt\"\n"
    );
    let mut entries: Vec<(String, Vec<u8>)> = vec![("p.txt".into(), b"p".to_vec())];
    for (i, (to, bytes)) in files.iter().enumerate() {
        let from = format!("g{i}/{}", to.rsplit('/').next().unwrap());
        manifest.push_str(&format!(
            "\n[[game_files]]\nfrom = \"{from}\"\nto = \"${{game}}/{to}\"\nsha256 = \"{}\"\n",
            sha(bytes)
        ));
        entries.push((from, bytes.to_vec()));
    }
    manifest.push('\n');
    manifest.push_str(extra);
    let mut z = vec![E::File("astra-gi.toml", manifest.as_bytes())];
    for (p, b) in &entries {
        z.push(E::File(p, b));
    }
    open(&zip(&z)).unwrap()
}

struct World {
    game: Tmp,
    profiles: Tmp,
}

impl World {
    fn new(name: &str) -> World {
        let game = Tmp::new(&format!("community-{name}"));
        write(&game.join("Game.exe"), PROGRAM);
        World {
            game,
            profiles: Tmp::new(&format!("community-{name}-profiles")),
        }
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        write(&self.game.join(rel), bytes);
    }

    fn at(&self, exe: &str) -> GameTarget {
        target(self.game.path(), exe, None, Platform::Windows)
    }

    fn profile(&self) -> std::path::PathBuf {
        self.profiles.join("p")
    }

    fn plan_on(
        &self,
        exe: &str,
        pkg: &GiPackage,
        provenance: Provenance,
    ) -> Result<InstallPlan, LauncherError> {
        plan_install_in(
            pkg,
            &self.at(exe),
            &self.profile(),
            &EngineSet::empty(),
            provenance,
            None,
        )
    }

    fn plan(&self, pkg: &GiPackage, provenance: Provenance) -> Result<InstallPlan, LauncherError> {
        self.plan_on("Game.exe", pkg, provenance)
    }

    fn install(&self, pkg: &GiPackage, provenance: Provenance) -> InstallPlan {
        let plan = self.plan(pkg, provenance).unwrap();
        install(
            pkg,
            &plan,
            &self.at("Game.exe"),
            ACCEPT,
            &astra_game_launcher::Ctx::silent(),
        )
        .unwrap();
        plan
    }
}

fn refusal(r: Result<InstallPlan, LauncherError>) -> (String, Vec<(String, String)>) {
    let e = r.unwrap_err();
    (
        e.code.to_owned(),
        e.params.into_iter().collect::<Vec<(String, String)>>(),
    )
}

fn limit(rule: &str, key: &str, value: &str) -> (String, Vec<(String, String)>) {
    let mut params = vec![
        (key.to_owned(), value.to_owned()),
        ("rule".to_owned(), rule.to_owned()),
    ];
    params.sort();
    (codes::COMMUNITY_LIMIT.to_owned(), params)
}

fn actions(plan: &InstallPlan) -> Vec<(String, GameAction)> {
    plan.game_files
        .iter()
        .map(|g| (g.rel_path.clone(), g.action))
        .collect()
}

const BOTH: [Provenance; 2] = [Provenance::FirstParty, Provenance::Community];

// ---------------------------------------------------------------------------------------------
// Overwrite a player's file.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_community_integration_never_replaces_a_players_file() {
    let ours: &[u8] = b"astra's level data";
    let pkg = release(ID, &[("level.bin", ours)], "");
    let w = World::new("players-file");
    w.put("level.bin", b"the player's level");
    assert_eq!(
        refusal(w.plan(&pkg, Provenance::Community)),
        (
            codes::FOREIGN_FILE.to_owned(),
            vec![("file".to_owned(), "level.bin".to_owned())]
        )
    );
    // Astra's own: backed up, ours in its place (the uninstall puts theirs back).
    let plan = w.plan(&pkg, Provenance::FirstParty).unwrap();
    assert_eq!(
        (
            plan.game_files[0].action,
            plan.game_files[0].existing_sha256.clone()
        ),
        (GameAction::Replace, Some(sha(b"the player's level")))
    );
    // Nothing there, or our very bytes: alike for both.
    for p in BOTH {
        w.put("level.bin", ours);
        assert_eq!(
            actions(&w.plan(&pkg, p).unwrap()),
            [("level.bin".to_owned(), GameAction::ReuseKnownProxy)]
        );
        fs::remove_file(w.game.join("level.bin")).unwrap();
        assert_eq!(
            actions(&w.plan(&pkg, p).unwrap()),
            [("level.bin".to_owned(), GameAction::Add)]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Foreign loader → keep.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_foreign_loader_is_never_replaced_whoever_asks() {
    let ours: &[u8] = b"MZ astra's dinput8 proxy";
    let declared = "[integration.loader]\nproxies = [\"dinput8.dll\", \"astra_hook.asi\"]\n";
    let w = World::new("foreign-loader");
    // A compiled hijack name (dinput8.dll, dwmapi.dll, …): for Astra's own as for anyone's.
    w.put("dinput8.dll", b"MZ a mod loader of the player's");
    for (p, extra) in [
        (Provenance::FirstParty, ""),
        (Provenance::FirstParty, declared),
        (Provenance::Community, declared),
    ] {
        let pkg = release(ID, &[("dinput8.dll", ours)], extra);
        assert_eq!(
            refusal(w.plan(&pkg, p)),
            (
                codes::FOREIGN_LOADER.to_owned(),
                vec![("file".to_owned(), "dinput8.dll".to_owned())]
            ),
            "{p:?} {extra:?}"
        );
    }
    // A name only the manifest declares: a foreign loader for this install; without the
    // declaration, Astra's own replaces it as any other file.
    let hook: &[u8] = b"MZ astra's asi hook";
    w.put("astra_hook.asi", b"MZ someone's asi");
    let undeclared = release(ID, &[("astra_hook.asi", hook)], "");
    let plan = w.plan(&undeclared, Provenance::FirstParty).unwrap();
    assert_eq!(plan.game_files[0].action, GameAction::Replace);
    for p in BOTH {
        let pkg = release(ID, &[("astra_hook.asi", hook)], declared);
        assert_eq!(refusal(w.plan(&pkg, p)).0, codes::FOREIGN_LOADER, "{p:?}");
    }
}

#[test]
fn keep_names_come_from_this_installs_manifest_and_never_name_a_loader() {
    let ini: &[u8] = b"[bridge]\nport = 0\n";
    let keeps =
        "[integration.loader]\nproxies = [\"dinput8.dll\"]\nkeep_if_present = [\"s2bridge.ini\"]\n";
    let w = World::new("keep");
    w.put("s2bridge.ini", b"[bridge]\nport = 4242 ; the player's\n");
    // Kept, as left, whoever asks — this install's manifest names it.
    for p in BOTH {
        let pkg = release(ID, &[("s2bridge.ini", ini)], keeps);
        let plan = w.plan(&pkg, p).unwrap();
        assert_eq!(plan.game_files[0].action, GameAction::KeepExisting, "{p:?}");
    }
    // Another install's manifest without the name: Astra's own replaces it, anyone else is
    // refused.
    let pkg = release(ID, &[("s2bridge.ini", ini)], "");
    assert_eq!(
        w.plan(&pkg, Provenance::FirstParty).unwrap().game_files[0].action,
        GameAction::Replace
    );
    assert_eq!(
        refusal(w.plan(&pkg, Provenance::Community)).0,
        codes::FOREIGN_FILE
    );
    // A keep name is never one this install loads, nor a loader: refused at parse, for anyone.
    for (body, name) in [
        (
            "proxies = [\"s2bridge.ini\"]\nkeep_if_present = [\"S2Bridge.ini\"]",
            "S2Bridge.ini",
        ),
        ("proxies = [\"doorstop_config.ini\"]", "doorstop_config.ini"),
        ("proxies = [\"ReShade.INI\"]", "ReShade.INI"),
        ("keep_if_present = [\"dinput8.dll\"]", "dinput8.dll"),
    ] {
        let text = format!(
            "schema = 1\n[integration]\nid = \"{ID}\"\nname = \"X\"\nversion = \"1\"\n[integration.loader]\n{body}\n[target]\nexe = \"*\"\nplatforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n"
        );
        let e = Manifest::parse(&text).unwrap_err();
        assert_eq!(
            (e.code, e.param("name")),
            (codes::LOADER_POLICY_CONFLICT, Some(name)),
            "{body}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Plant code in another loader's folder.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_community_integration_places_files_beside_the_program_or_below_its_own_folder_only() {
    let w = World::new("folders");
    let dll: &[u8] = b"MZ astra";
    for (to, ok) in [
        ("astra.dll", true),
        ("someone.example/astra.dll", true),
        ("Someone.Example/sub/astra.dll", true),
        ("BepInEx/plugins/astra.dll", false),
        ("bepinex/astra.dll", false),
        ("MelonLoader/astra.dll", false),
        ("UE4SS/Mods/astra/main.lua", false),
        ("Mods/astra.dll", false),
        ("~mods/astra.pak", false),
        ("someone.example/Mods/astra.dll", false),
        ("plugins/astra.dll", false),
        ("someone.other/astra.dll", false),
    ] {
        let pkg = release(ID, &[(to, dll)], "");
        let got = w.plan(&pkg, Provenance::Community);
        if ok {
            got.unwrap_or_else(|e| panic!("{to}: {e}"));
        } else {
            assert_eq!(refusal(got), limit("game-file-folder", "file", to), "{to}");
        }
        // Astra's own places where its manifest says.
        w.plan(&pkg, Provenance::FirstParty)
            .unwrap_or_else(|e| panic!("{to}, first-party: {e}"));
    }
}

/// K18 in planning: a loader's name a community integration places, undeclared.
#[test]
fn a_community_integration_places_a_loaders_name_only_when_it_declares_it() {
    let w = World::new("undeclared");
    let ours: &[u8] = b"MZ astra's proxy";
    for name in ["dinput8.dll", "XInput1_3.dll", "dwmapi.dll", "winhttp.dll"] {
        let pkg = release(ID, &[(name, ours)], "");
        assert_eq!(
            refusal(w.plan(&pkg, Provenance::Community)),
            (
                codes::LOADER_UNDECLARED.to_owned(),
                vec![("file".to_owned(), name.to_owned())]
            )
        );
        let declared = release(
            ID,
            &[(name, ours)],
            &format!(
                "[integration.loader]\nproxies = [\"{}\"]\n",
                name.to_ascii_lowercase()
            ),
        );
        assert_eq!(
            actions(&w.plan(&declared, Provenance::Community).unwrap()),
            [(name.to_owned(), GameAction::Add)]
        );
        // Astra's own manifests from before [integration.loader]: as they always planned.
        assert_eq!(
            actions(&w.plan(&pkg, Provenance::FirstParty).unwrap()),
            [(name.to_owned(), GameAction::Add)]
        );
    }
}

#[test]
fn a_community_integration_plans_only_on_a_program_the_walk_lists() {
    let w = World::new("exe");
    w.put("Launcher.exe", PROGRAM);
    w.put("Game.dll", PROGRAM);
    w.put("notes.exe", b"not a program at all");
    w.put("tools/UnityCrashHandler64.exe", PROGRAM);
    w.put("a/b/c/d/e/Deep.exe", PROGRAM);
    w.put("BepInEx/Game.exe", PROGRAM);
    w.put("Linux.x86_64", b"\x7fELF a linux game");
    let pkg = release(ID, &[("astra.dll", b"MZ astra")], "");
    for exe in [
        "Launcher.exe",
        "Game.dll",
        "notes.exe",
        "tools/UnityCrashHandler64.exe",
        "a/b/c/d/e/Deep.exe",
        "BepInEx/Game.exe",
    ] {
        assert_eq!(
            refusal(w.plan_on(exe, &pkg, Provenance::Community)),
            limit("exe-not-a-program", "exe", exe),
            "{exe}"
        );
        // Astra's own plans on the program the person picked.
        w.plan_on(exe, &pkg, Provenance::FirstParty)
            .unwrap_or_else(|e| panic!("{exe}, first-party: {e}"));
    }
    for exe in ["Game.exe", "Linux.x86_64"] {
        w.plan_on(exe, &pkg, Provenance::Community)
            .unwrap_or_else(|e| panic!("{exe}: {e}"));
    }
    // Past the walk, but the program Steam launches for the game: taken, as Pass B takes it.
    let appinfo = w.game.join("appinfo.vdf");
    fs::write(
        &appinfo,
        appinfo_launch(&[(4_000_001, "a\\b\\c\\d\\e\\Deep.exe", "windows")]),
    )
    .unwrap();
    let steam = GameTarget {
        steam_appid: Some(4_000_001),
        steam_appinfo: Some(appinfo),
        ..w.at("a/b/c/d/e/Deep.exe")
    };
    plan_install_in(
        &pkg,
        &steam,
        &w.profile(),
        &EngineSet::empty(),
        Provenance::Community,
        None,
    )
    .unwrap();
}

#[test]
fn a_community_integrations_id_and_name_are_never_astras() {
    let w = World::new("reserved");
    let pkg = release("astra.example", &[("astra.dll", b"MZ astra")], "");
    assert_eq!(
        refusal(w.plan(&pkg, Provenance::Community)).0,
        codes::RESERVED_ID
    );
    w.plan(&pkg, Provenance::FirstParty).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Abuse [launch].
// ---------------------------------------------------------------------------------------------

#[test]
fn a_community_integration_cannot_abuse_launch() {
    let w = World::new("launch");
    let files: &[(&str, &[u8])] = &[("dwmapi.dll", b"MZ astra's proxy")];
    let loader = "[integration.loader]\nproxies = [\"dwmapi.dll\"]\n";
    let refused = [
        (
            "[launch]\nenv = { LD_PRELOAD = \"${game}/x.so\" }\n",
            limit("launch-env", "field", "launch.env.LD_PRELOAD"),
        ),
        (
            "[launch]\nproton_dll_overrides = { dxgi = \"native,builtin\" }\n",
            limit(
                "launch-override",
                "field",
                "launch.proton_dll_overrides.dxgi",
            ),
        ),
        (
            "[launch]\nproton_dll_overrides = { dwmapi = \"builtin\" }\n",
            limit(
                "launch-override",
                "field",
                "launch.proton_dll_overrides.dwmapi",
            ),
        ),
        (
            "[launch]\nproton_dll_overrides = { dwmapi = \"\" }\n",
            limit(
                "launch-override",
                "field",
                "launch.proton_dll_overrides.dwmapi",
            ),
        ),
        (
            "[launch]\nargs = [\"-log\", \"/home/someone/.ssh/id_ed25519\"]\n",
            limit("launch-arg-path", "field", "launch.args[1]"),
        ),
        (
            "[launch]\nargs = [\"C:\\\\Windows\\\\System32\\\\x.dll\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
        (
            "[launch]\nargs = [\"${game}/../other-game/x.ini\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
        (
            "[launch]\nargs = [\"-config=${profile}/x.ini\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
        (
            "[launch]\nargs = [\"${game}\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
        // A path by itself in a word: the game's working folder's parent, or a drive's current
        // folder (on Windows and under Wine alike).
        (
            "[launch]\nargs = [\"-windowed\", \"..\"]\n",
            limit("launch-arg-path", "field", "launch.args[1]"),
        ),
        (
            "[launch]\nargs = [\"C:..\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
        (
            "[launch]\nargs = [\"-x=..\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
        (
            "[launch]\nargs = [\"-plugin=C:evil.dll\"]\n",
            limit("launch-arg-path", "field", "launch.args[0]"),
        ),
    ];
    for (launch, want) in &refused {
        let pkg = release(ID, files, &format!("{loader}{launch}"));
        assert_eq!(
            &refusal(w.plan(&pkg, Provenance::Community)),
            want,
            "{launch}"
        );
        w.plan(&pkg, Provenance::FirstParty)
            .unwrap_or_else(|e| panic!("{launch}, first-party: {e}"));
    }
    for launch in [
        "[launch]\nproton_dll_overrides = { dwmapi = \"native,builtin\" }\n",
        "[launch]\nproton_dll_overrides = { \"DWMAPI.dll\" = \"n,b\" }\n",
        "[launch]\nargs = [\"-windowed\", \"--astra\", \"${profile}/astra.ini\", \"${game}/someone.example/\", \"${game}/\"]\n",
    ] {
        let pkg = release(ID, files, &format!("{loader}{launch}"));
        w.plan(&pkg, Provenance::Community)
            .unwrap_or_else(|e| panic!("{launch}: {e}"));
    }
}

/// An override applies to the whole Proton prefix by the module's name, and Windows and Wine look
/// in the program's own folder first: a community integration's `winhttp.dll` below its own
/// folder is never what a `winhttp` override loads — a dormant loader of the player's beside the
/// program is. So the override is refused unless the DLL it names is placed directly beside the
/// program.
#[test]
fn a_community_override_names_only_a_dll_it_places_beside_the_program() {
    let w = World::new("override-depth");
    let loader = "[integration.loader]\nproxies = [\"winhttp.dll\"]\n";
    let deep = release(
        ID,
        &[("someone.example/winhttp.dll", b"MZ ours, below our folder")],
        &format!("{loader}[launch]\nproton_dll_overrides = {{ winhttp = \"n,b\" }}\n"),
    );
    assert_eq!(
        refusal(w.plan(&deep, Provenance::Community)),
        limit(
            "launch-override",
            "field",
            "launch.proton_dll_overrides.winhttp"
        )
    );
    // The player's dormant BepInEx beside the program changes nothing: refused all the same.
    write(
        &w.game.join("winhttp.dll"),
        b"MZ their BepInEx, switched off",
    );
    assert_eq!(
        refusal(w.plan(&deep, Provenance::Community)),
        limit(
            "launch-override",
            "field",
            "launch.proton_dll_overrides.winhttp"
        )
    );
    fs::remove_file(w.game.join("winhttp.dll")).unwrap();
    // Beside the program it is what the override loads.
    let beside = release(
        ID,
        &[("winhttp.dll", b"MZ ours, beside the program")],
        &format!("{loader}[launch]\nproton_dll_overrides = {{ winhttp = \"n,b\" }}\n"),
    );
    w.plan(&beside, Provenance::Community).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Uninstall deletes their identical loader.
// ---------------------------------------------------------------------------------------------

#[test]
fn uninstall_keeps_a_proxy_whose_folder_gained_files_whoever_installed_it() {
    let ours: &[u8] = b"MZ the very bytes a UE4SS ships";
    let loader = "[integration.loader]\nproxies = [\"dwmapi.dll\"]\n";
    for p in BOTH {
        // Their loader arrives after us: its own files beside ours, its proxy ours byte for byte.
        let w = World::new(&format!("gained-{p:?}"));
        w.put("before-us.txt", b"there before");
        w.install(&release(ID, &[("dwmapi.dll", ours)], loader), p);
        let ledger = installed(&w.profile()).unwrap().unwrap();
        assert_eq!(ledger.proxy_folders.len(), 1);
        assert_eq!(ledger.proxy_folders[0].proxy, "dwmapi.dll");
        assert_eq!(ledger.proxy_folders[0].names, ["Game.exe", "before-us.txt"]);
        assert!(!ledger.proxy_folders[0].capped);
        w.put("UE4SS.dll", b"MZ their UE4SS");
        w.put("Mods/mods.txt", b"theirs");
        let r = uninstall(&w.profile(), &w.at("Game.exe")).unwrap();
        assert_eq!(r.kept, ["dwmapi.dll"], "{p:?}");
        assert_eq!(
            r.kept_reasons.get("dwmapi.dll").map(String::as_str),
            Some(KEPT_FOLDER_GAINED_FILES)
        );
        assert_eq!(fs::read(w.game.join("dwmapi.dll")).unwrap(), ours);

        // Nothing arrived (what was there before is no arrival): ours goes, the folder as it was.
        let w = World::new(&format!("nothing-{p:?}"));
        w.put("before-us.txt", b"there before");
        let before = snapshot(w.game.path());
        w.install(&release(ID, &[("dwmapi.dll", ours)], loader), p);
        let r = uninstall(&w.profile(), &w.at("Game.exe")).unwrap();
        assert_eq!(
            (r.removed, r.kept_reasons.len()),
            (vec!["dwmapi.dll".to_owned()], 0)
        );
        assert_eq!(snapshot(w.game.path()), before);
    }
}

/// An update carries the first install's baseline: what arrived between the install and the
/// update stays someone else's, so the update never launders it into "there before us".
#[test]
fn an_update_carries_the_proxy_folders_baseline() {
    let ours: &[u8] = b"MZ the very bytes a UE4SS ships";
    let pkg = release(
        ID,
        &[("dwmapi.dll", ours)],
        "[integration.loader]\nproxies = [\"dwmapi.dll\"]\n",
    );
    let w = World::new("carried");
    w.install(&pkg, Provenance::Community);
    w.put("UE4SS.dll", b"MZ their UE4SS");
    let update = w.install(&pkg, Provenance::Community);
    assert_eq!(update.game_files[0].action, GameAction::Add);
    let ledger = installed(&w.profile()).unwrap().unwrap();
    assert_eq!(ledger.proxy_folders[0].names, ["Game.exe"]);
    let r = uninstall(&w.profile(), &w.at("Game.exe")).unwrap();
    assert_eq!(r.kept, ["dwmapi.dll"]);
}

#[test]
fn a_proxy_folder_past_its_baseline_keeps_the_proxy_always() {
    let w = World::new("capped");
    for i in 0..5_001 {
        w.put(&format!("crowd/{i}.dat"), b"");
    }
    write(&w.game.join("crowd/Game.exe"), PROGRAM);
    let ours: &[u8] = b"MZ astra's proxy";
    let pkg = release(
        ID,
        &[("dinput8.dll", ours)],
        "[integration.loader]\nproxies = [\"dinput8.dll\"]\n",
    );
    // Astra's own, as any: the rule is every integration's (and a community one's program must
    // be one the walk lists, which a folder this crowded is past).
    let game = w.at("crowd/Game.exe");
    let plan = plan_install_in(
        &pkg,
        &game,
        &w.profile(),
        &EngineSet::empty(),
        Provenance::FirstParty,
        None,
    )
    .unwrap();
    install(
        &pkg,
        &plan,
        &game,
        ACCEPT,
        &astra_game_launcher::Ctx::silent(),
    )
    .unwrap();
    let ledger = installed(&w.profile()).unwrap().unwrap();
    assert!(ledger.proxy_folders[0].capped && ledger.proxy_folders[0].names.is_empty());
    let r = uninstall(&w.profile(), &game).unwrap();
    assert_eq!(r.kept, ["dinput8.dll"]);
}

#[cfg(unix)]
#[test]
fn under_proton_a_proxy_kept_for_its_folder_keeps_its_override() {
    let w = World::new("proton");
    let pfx = Tmp::new("community-pfx");
    fs::write(pfx.join("user.reg"), USER_REG).unwrap();
    let game = target(
        w.game.path(),
        "Game.exe",
        Some(4_000_001),
        Platform::LinuxProton {
            prefix: Some(pfx.path().to_path_buf()),
        },
    );
    let pkg = release(
        ID,
        &[("dinput8.dll", b"MZ astra's proxy")],
        "[integration.loader]\nproxies = [\"dinput8.dll\"]\n[launch]\nproton_dll_overrides = { dinput8 = \"native,builtin\" }\n",
    );
    let plan = plan_install_in(
        &pkg,
        &game,
        &w.profile(),
        &EngineSet::empty(),
        Provenance::Community,
        None,
    )
    .unwrap();
    install(
        &pkg,
        &plan,
        &game,
        ACCEPT,
        &astra_game_launcher::Ctx::silent(),
    )
    .unwrap();
    prepare_launch(
        &w.profile(),
        &game,
        &vars(),
        Some(Path::new("/opt/fake/steam")),
        &no_markers(),
    )
    .unwrap();
    w.put("Mods/theirs.lua", b"-- theirs");
    let r = uninstall(&w.profile(), &game).unwrap();
    assert_eq!(r.kept, ["dinput8.dll"]);
    assert_eq!(r.proton_overrides_kept, ["dinput8"]);
    let reg = fs::read_to_string(pfx.join("user.reg")).unwrap();
    assert!(reg.contains("\"dinput8\"=\"native,builtin\""), "{reg}");
}

// ---------------------------------------------------------------------------------------------
// The loader policy never widens Play.
// ---------------------------------------------------------------------------------------------

/// Only the Doorstop-sharing names (`winhttp`, `version`, `dxgi`, `d3d11`) are another mod
/// manager's to relink at Play. A file of ours by any other loader's name (`dwmapi.dll`) that
/// changed since is a broken install: Install again.
#[test]
fn play_relinks_only_the_doorstop_sharing_names() {
    let w = World::new("play");
    let ours: &[u8] = b"MZ astra's dwmapi proxy";
    // Found with our very bytes (a `reuse-known-proxy`), then changed.
    w.put("dwmapi.dll", ours);
    let plan = w.install(
        &release(ID, &[("dwmapi.dll", ours)], ""),
        Provenance::FirstParty,
    );
    assert_eq!(plan.game_files[0].action, GameAction::ReuseKnownProxy);
    w.put("dwmapi.dll", b"MZ someone else's now");
    let e = prepare_launch(
        &w.profile(),
        &w.at("Game.exe"),
        &vars(),
        Some(Path::new("/opt/fake/steam")),
        &no_markers(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::NEEDS_REINSTALL, Some("dwmapi.dll"))
    );
}
