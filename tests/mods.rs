// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Finding the player's own mods (`discover_mods`) and remembering their answer
//! (`mods-choice.json`). Every mod manager folder here is a fixture below a temporary "home": no
//! test reads or writes a real one.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use astra_game_launcher::{
    Choice, ChoiceKind, ChoiceSource, Coexistence, LoaderKind, MODS_CHOICE_FILE_NAME, Manifest,
    ModManager, ModSource, ModsEnv, ModsLoader, Platform, Unplayable, add_mods_root, codes,
    default_mods_source, discover_mods_in, mods_choice, set_mods_choice, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
/// Not in Thunderstore's table: found only by the fallbacks.
const NEW_APPID: u32 = 4_000_100;
/// Lethal Company, in the table: r2modman's `LethalCompany`, Gale's `lethal-company`.
const LETHAL: u32 = 1_966_720;

fn proxy() -> Vec<u8> {
    pe_with_options(X64, DOORSTOP_V4, b"4.5.0 ours")
}

/// A game in `<tmp>/common/<GAME>` (so its Steam install folder's name is `GAME`), a profile, and
/// a home whose mod managers are fixtures.
struct World {
    tmp: Tmp,
    appid: u32,
    platform: Platform,
    il2cpp: bool,
}

impl World {
    fn new(appid: u32) -> World {
        let tmp = Tmp::new("mods");
        let w = World {
            tmp,
            appid,
            platform: Platform::Windows,
            il2cpp: false,
        };
        unity_game(&w.game_dir(), GAME);
        write(
            &w.game_dir().join(format!("{GAME}.exe")),
            &pe_with_options(X64, &[], b"game"),
        );
        fs::create_dir_all(w.home()).unwrap();
        w
    }

    /// The same, an IL2CPP game; its runner says `engine = "unity-il2cpp"`.
    fn il2cpp(appid: u32) -> World {
        let tmp = Tmp::new("mods-il2cpp");
        let w = World {
            tmp,
            appid,
            platform: Platform::Windows,
            il2cpp: true,
        };
        unity_il2cpp_game(&w.game_dir(), GAME);
        write(
            &w.game_dir().join(format!("{GAME}.exe")),
            &pe_with_options(X64, &[], b"game"),
        );
        fs::create_dir_all(w.home()).unwrap();
        w
    }

    fn game_dir(&self) -> PathBuf {
        self.tmp.join("common").join(GAME)
    }

    fn home(&self) -> PathBuf {
        self.tmp.join("home")
    }

    fn profile(&self) -> PathBuf {
        self.tmp.join("profiles").join("p")
    }

    fn env(&self) -> ModsEnv {
        ModsEnv::linux(Some(&self.home()), None, None)
    }

    fn target(&self) -> astra_game_launcher::GameTarget {
        target(
            &self.game_dir(),
            &format!("{GAME}.exe"),
            Some(self.appid),
            self.platform.clone(),
        )
    }

    /// The runner installed, with `decl` as its `[integration.mods]` (or none).
    fn install(&self, decl: &str) {
        let pkg = if self.il2cpp {
            mods_runner_for(&proxy(), decl, "unity-il2cpp")
        } else {
            mods_runner(&proxy(), decl)
        };
        plan_and_install(&pkg, &self.target(), &self.profile());
    }

    /// `<data>/com.kesomannen.gale/<folder>/profiles/<profile>`.
    fn gale(&self, folder: &str, profile: &str) -> PathBuf {
        self.home()
            .join(".local/share/com.kesomannen.gale")
            .join(folder)
            .join("profiles")
            .join(profile)
    }

    /// `<config>/r2modmanPlus-local/<folder>/profiles/<profile>`.
    fn r2(&self, folder: &str, profile: &str) -> PathBuf {
        self.home()
            .join(".config/r2modmanPlus-local")
            .join(folder)
            .join("profiles")
            .join(profile)
    }

    fn discover(&self) -> Vec<ModSource> {
        discover_mods_in(&self.target(), &self.profile(), &self.env())
    }
}

/// A BepInEx 5.4.23.5 profile at `root` holding one mod.
fn profile_with_a_mod(root: &Path) {
    bepinex5_at(root, "5.4.23.5+57f1fb8");
    plugin_at(root, "Author-Mod/Mod.dll");
}

/// BepInEx's log at `root`, its first lines `lines`, last written `ago` before now.
fn log(root: &Path, lines: &[&str], ago: Duration) {
    let p = root.join("BepInEx/LogOutput.log");
    write(&p, lines.join("\n").as_bytes());
    let f = fs::File::options().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::now() - ago).unwrap();
}

fn start_line(program: &str) -> String {
    format!("[Message:   BepInEx] BepInEx 5.4.23.5 - {program} (10/30/2026 1:23:45 PM)")
}

fn roots(found: &[ModSource]) -> Vec<(ModManager, PathBuf)> {
    found
        .iter()
        .map(|s| (s.manager, fs::canonicalize(&s.root).unwrap()))
        .collect()
}

fn real(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap()
}

// ---- where the mod managers keep their data ----

#[test]
fn the_mod_managers_data_folders_per_os() {
    let t = Tmp::new("env");
    let appdata = t.join("AppData/Roaming");
    for d in [
        "r2modmanPlus-local",
        "Thunderstore Mod Manager/DataFolder",
        "com.kesomannen.gale",
    ] {
        fs::create_dir_all(appdata.join(d)).unwrap();
    }
    assert_eq!(
        ModsEnv::windows(Some(appdata.clone())).manager_roots(),
        vec![
            (ModManager::R2modman, appdata.join("r2modmanPlus-local")),
            (
                ModManager::Thunderstore,
                appdata.join("Thunderstore Mod Manager/DataFolder")
            ),
            (ModManager::Gale, appdata.join("com.kesomannen.gale")),
        ]
    );

    // Linux: the XDG defaults below home, and every Flatpak's own config and data.
    let home = t.join("home");
    for d in [
        ".config/r2modmanPlus-local",
        ".local/share/com.kesomannen.gale",
        ".var/app/com.github.ebkr.r2modmanPlus/config/r2modmanPlus-local",
        ".var/app/com.kesomannen.gale/data/com.kesomannen.gale",
        ".var/app/org.other.App/config",
    ] {
        fs::create_dir_all(home.join(d)).unwrap();
    }
    let env = ModsEnv::linux(Some(&home), None, None);
    assert_eq!(env.config_home, Some(home.join(".config")));
    assert_eq!(env.data_home, Some(home.join(".local/share")));
    assert_eq!(
        env.manager_roots(),
        vec![
            (
                ModManager::R2modman,
                home.join(".config/r2modmanPlus-local")
            ),
            (
                ModManager::Gale,
                home.join(".local/share/com.kesomannen.gale")
            ),
            (
                ModManager::R2modman,
                home.join(".var/app/com.github.ebkr.r2modmanPlus/config/r2modmanPlus-local")
            ),
            (
                ModManager::Gale,
                home.join(".var/app/com.kesomannen.gale/data/com.kesomannen.gale")
            ),
        ]
    );

    // XDG folders that are set win; a relative one is invalid and ignored.
    let xdg_config = t.join("xdg-config");
    fs::create_dir_all(xdg_config.join("r2modmanPlus-local")).unwrap();
    let env = ModsEnv::linux(Some(&home), Some(&xdg_config), Some(Path::new("relative")));
    assert_eq!(env.config_home, Some(xdg_config.clone()));
    assert_eq!(env.data_home, Some(home.join(".local/share")));
    assert_eq!(
        env.manager_roots()[0],
        (ModManager::R2modman, xdg_config.join("r2modmanPlus-local"))
    );
    // No home at all: nothing but what is set.
    assert_eq!(ModsEnv::linux(None, None, None).manager_roots(), vec![]);
}

// ---- which folder is this game's ----

#[test]
fn thunderstores_table_names_each_managers_folder_for_the_game() {
    let w = World::new(LETHAL);
    profile_with_a_mod(&w.r2("LethalCompany", "Default"));
    profile_with_a_mod(&w.gale("lethal-company", "Main"));
    // Each manager's name for the game in the other's folder, another game's folder, a folder
    // named after the install folder, and a log naming the program: the table decides, alone.
    profile_with_a_mod(&w.gale("LethalCompany", "R2sName"));
    profile_with_a_mod(&w.r2("lethal-company", "GalesName"));
    profile_with_a_mod(&w.gale("valheim", "Default"));
    profile_with_a_mod(&w.r2(GAME, "ByName"));
    let logged = w.gale("renamed", "Logged");
    profile_with_a_mod(&logged);
    log(&logged, &[&start_line(GAME)], Duration::from_secs(60));
    let found = w.discover();
    assert_eq!(
        roots(&found),
        vec![
            (
                ModManager::R2modman,
                real(&w.r2("LethalCompany", "Default"))
            ),
            (ModManager::Gale, real(&w.gale("lethal-company", "Main"))),
        ],
        "{found:#?}"
    );
    // Nothing installed in the profile: shown, never offered.
    assert!(found.iter().all(|s| s.playable
        == Err(Unplayable::NotSupported {
            reason: "not-installed".into()
        })));
}

#[test]
fn a_game_newer_than_the_table_is_found_by_its_install_folder_or_its_log() {
    let w = World::new(NEW_APPID);
    // The Steam install folder's name.
    profile_with_a_mod(&w.gale(GAME, "ByFolder"));
    // A log naming the program on its second line (Unity 6 writes an error first).
    let by_log = w.r2("SomethingElse", "ByLog");
    profile_with_a_mod(&by_log);
    log(
        &by_log,
        &[
            "[Error  : Unity Log] Unable to start Unity log writer",
            &start_line(GAME),
        ],
        Duration::from_secs(60),
    );
    // Another program, and this one named only past the first five lines: never a guess.
    let other = w.r2("SomethingElse", "Other");
    profile_with_a_mod(&other);
    log(
        &other,
        &[&start_line("Other Game")],
        Duration::from_secs(60),
    );
    let late = w.gale("late", "Late");
    profile_with_a_mod(&late);
    let mut lines = vec!["x"; 5];
    let named = start_line(GAME);
    lines.push(&named);
    log(&late, &lines, Duration::from_secs(60));

    let mut found = roots(&w.discover());
    found.sort();
    let mut want = vec![
        (ModManager::R2modman, real(&by_log)),
        (ModManager::Gale, real(&w.gale(GAME, "ByFolder"))),
    ];
    want.sort();
    assert_eq!(found, want);
}

// ---- what counts ----

/// What counts is what the built-in data finds enabled in BepInEx's mods root
/// (`unity.mods.toml`: each folder and each `*.dll` in `BepInEx/plugins`), less what the player's
/// manager switched off (its adapter, §7.7) and Astra's own items. Launcher 0.5.0 counted
/// otherwise (`tests/mods_identity.rs` declares each row): a folder holding no `*.dll` (a pack of
/// textures another mod reads) was no mod to it, and `BepInEx/patchers` was counted too.
#[test]
fn what_the_data_finds_enabled_counts_as_a_mod() {
    let w = World::new(NEW_APPID);
    let p = w.gale(GAME, "Counted");
    bepinex5_at(&p, "5.4.23.5");
    plugin_at(&p, "A-Mod/A.dll");
    plugin_at(&p, "B-Nested/plugins/deep/B.DLL");
    // Switched off by Gale: renamed, so BepInEx loads nothing of it.
    plugin_at(&p, "C-Disabled/C.dll.old");
    plugin_at(&p, "C-Disabled/manifest.json.old");
    plugin_at(&p, "D-Textures/suit.png");
    plugin_at(&p, "Loose.dll");
    plugin_at(&p, "Loose2.DLL");
    plugin_at(&p, "LooseDisabled.dll.old");
    plugin_at(&p, "notes.txt");
    // Astra's own items never count.
    plugin_at(&p, "Astra/Astra.Bridge.dll");
    write(&p.join("BepInEx/plugins/Astra/astra-item.json"), b"{}");
    // Not in the mods root.
    write(&p.join("BepInEx/patchers/P-Patcher/P.dll"), b"MZ");

    // A profile with Astra alone, one with no preloader, one with only disabled mods: none.
    let astra_only = w.gale(GAME, "AstraOnly");
    bepinex5_at(&astra_only, "5.4.23.5");
    plugin_at(&astra_only, "Astra/Astra.dll");
    write(
        &astra_only.join("BepInEx/plugins/Astra/astra-item.json"),
        b"{}",
    );
    let no_core = w.gale(GAME, "NoCore");
    plugin_at(&no_core, "A-Mod/A.dll");
    let disabled = w.gale(GAME, "Disabled");
    bepinex5_at(&disabled, "5.4.23.5");
    plugin_at(&disabled, "A-Mod/A.dll.old");

    let found = w.discover();
    assert_eq!(roots(&found), vec![(ModManager::Gale, real(&p))]);
    assert_eq!(found[0].mods, 5, "A, B, D, Loose, Loose2");
    assert_eq!(found[0].profile, "Counted");
    assert_eq!(
        found[0].loader.preloader.as_deref(),
        Some("BepInEx.Preloader.dll")
    );
    assert_eq!(found[0].coexist, [Coexistence::Chain]);

    // The same folder picked where no manager keeps it: no adapter reads a manager's state there,
    // so the data alone counts, the folder Gale switched off included.
    let picked = w.tmp.join("Elsewhere/Mine");
    copy_tree(&p, &picked);
    let mut picked_found: Vec<ModSource> = discover_with_added(&w, &picked)
        .into_iter()
        .filter(|s| s.manager == ModManager::Folder)
        .collect();
    assert_eq!(picked_found.len(), 1, "{picked_found:#?}");
    assert_eq!(picked_found.remove(0).mods, 6, "A, B, C, D, Loose, Loose2");
}

/// Discovery with `folder` added by the player.
fn discover_with_added(w: &World, folder: &Path) -> Vec<ModSource> {
    fs::create_dir_all(w.profile()).unwrap();
    add_mods_root(&w.profile(), folder).unwrap();
    w.discover()
}

#[test]
fn a_bepinex_in_the_game_folder_is_the_manual_source_and_melonloader_is_shown_not_offered() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let game = w.game_dir();
    bepinex5_at(&game, "5.4.23.5");
    plugin_at(&game, "Their-Mod/Their.dll");
    // MelonLoader with its proxy: it loads into every start.
    fs::create_dir_all(game.join("MelonLoader")).unwrap();
    write(&game.join("version.dll"), b"MZ melon proxy");
    // A mod manager's MelonLoader profile.
    let melon = w.r2(GAME, "Melon");
    fs::create_dir_all(melon.join("MelonLoader")).unwrap();
    write(&melon.join("Mods/Melon.dll"), b"MZ");

    let found = w.discover();
    let game_root = real(&game);
    let manual: Vec<&ModSource> = found
        .iter()
        .filter(|s| s.manager == ModManager::Manual)
        .collect();
    assert_eq!(manual.len(), 2, "{found:#?}");
    let bep = manual
        .iter()
        .find(|s| s.loader.kind == LoaderKind::Bepinex5)
        .unwrap();
    assert_eq!(
        (real(&bep.root), bep.profile.as_str(), bep.mods),
        (game_root.clone(), "", 1)
    );
    assert_eq!(bep.playable, Ok(()));
    let ml = manual
        .iter()
        .find(|s| s.loader.kind == LoaderKind::MelonLoader)
        .unwrap();
    assert_eq!(ml.playable, Err(Unplayable::MelonLoader));
    let profile = found
        .iter()
        .find(|s| s.manager == ModManager::R2modman)
        .unwrap();
    assert_eq!(
        (profile.loader.kind, profile.mods),
        (LoaderKind::MelonLoader, 1)
    );
    assert_eq!(profile.playable, Err(Unplayable::MelonLoader));
    assert_eq!(
        default_mods_source(&found).map(|s| s.manager),
        Some(ModManager::Manual)
    );

    // MelonLoader's folder without its proxy loads nothing.
    fs::remove_file(game.join("version.dll")).unwrap();
    assert!(
        !w.discover()
            .iter()
            .any(|s| s.manager == ModManager::Manual && s.loader.kind == LoaderKind::MelonLoader)
    );
}

// ---- the default pick ----

#[test]
fn the_most_recently_played_source_is_offered_first_ties_by_name() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let old = w.gale(GAME, "Old");
    let beta = w.gale(GAME, "Beta");
    let alpha = w.r2(GAME, "Alpha");
    let never = w.gale(GAME, "Never");
    let newest_unplayable = w.gale(GAME, "Unplayable");
    for p in [&old, &beta, &alpha, &never] {
        profile_with_a_mod(p);
    }
    bepinex5_at(&newest_unplayable, "5.3.1.0");
    plugin_at(&newest_unplayable, "M/M.dll");
    log(&old, &[], Duration::from_secs(3600));
    // Beta and Alpha were played at the very same moment.
    let at = SystemTime::now() - Duration::from_secs(60);
    for p in [&beta, &alpha] {
        log(p, &[], Duration::ZERO);
        fs::File::options()
            .write(true)
            .open(p.join("BepInEx/LogOutput.log"))
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    log(&newest_unplayable, &[], Duration::from_secs(1));

    let found = w.discover();
    let names: Vec<&str> = found.iter().map(|s| s.profile.as_str()).collect();
    assert_eq!(names, ["Unplayable", "Alpha", "Beta", "Old", "Never"]);
    assert_eq!(found[4].last_played, None);
    assert_eq!(
        found[0].playable,
        Err(Unplayable::LoaderUnsupported {
            have: "5.3.1.0".into(),
            need: "5.4".into()
        })
    );
    assert_eq!(default_mods_source(&found).unwrap().profile, "Alpha");
}

// ---- their BepInEx's version, from its version resource ----

#[test]
fn their_bepinex_is_judged_by_its_version_resource() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let cases = [
        (
            "Valheim",
            "5.4.23.6+b89fcc8ecbec7e07cd37b67315b932506084e8b4",
            Ok(()),
        ),
        ("Ror2", "5.4.21.0", Ok(())),
        (
            "Subnautica",
            "5.3.0.0",
            Err(Unplayable::LoaderUnsupported {
                have: "5.3.0.0".into(),
                need: "5.4".into(),
            }),
        ),
    ];
    for (name, version, _) in &cases {
        let p = w.gale(GAME, name);
        bepinex5_at(&p, version);
        plugin_at(&p, "M/M.dll");
    }
    let found = w.discover();
    for (name, version, playable) in cases {
        let s = found.iter().find(|s| s.profile == name).unwrap();
        assert_eq!(s.loader.version.as_deref(), Some(version), "{name}");
        assert_eq!(s.playable, playable, "{name}");
    }
}

#[test]
fn an_il2cpp_integration_needs_their_exact_bleeding_edge_build() {
    let w = World::il2cpp(NEW_APPID);
    w.install(&MODS_DECL.replace(
        "loader = \"bepinex5\"",
        "loader = \"bepinex6-il2cpp\"\nbepinex_build = 788",
    ));
    let be788 = w.gale(GAME, "Be788");
    bepinex6_il2cpp_at(
        &be788,
        "6.0.0-be.788+5b766a3b7f6c164d4798924a93f3acf4db769d06",
    );
    // V Rising's pack (1.733.2): a dev build, whose version names no build number at all.
    let dev733 = w.gale(GAME, "Dev733");
    bepinex6_il2cpp_at(
        &dev733,
        "6.0.0-dev+3a12d1810bf4dd5ca1635352ccf5bff5d95f987f",
    );
    // A BepInEx 5 for this IL2CPP game: the built-in data asks BepInEx 5 of a Mono game only
    // (`codes = ["unity-mono"]`), and BepInEx 5 runs no IL2CPP game. Not the player's mods here.
    let be5 = w.gale(GAME, "Be5");
    bepinex5_at(&be5, "5.4.23.5");
    // A BepInEx 6 that never ran: no `BepInEx/interop/` yet, which the data requires.
    let fresh = w.gale(GAME, "NeverRun");
    bepinex6_il2cpp_at(&fresh, "6.0.0-be.788+5b766a3");
    fs::remove_dir_all(fresh.join("BepInEx/interop")).unwrap();
    for p in [&be788, &dev733, &be5, &fresh] {
        plugin_at(p, "M/M.dll");
    }
    let found = w.discover();
    let get = |n: &str| found.iter().find(|s| s.profile == n);
    let be788 = get("Be788").unwrap();
    assert_eq!(be788.loader.kind, LoaderKind::Bepinex6Il2cpp);
    assert_eq!(
        be788.loader.preloader.as_deref(),
        Some("BepInEx.Unity.IL2CPP.dll")
    );
    assert_eq!(be788.playable, Ok(()));
    assert_eq!(be788.coexist, [Coexistence::Chain]);
    assert_eq!(
        get("Dev733").unwrap().playable,
        Err(Unplayable::LoaderUnsupported {
            have: "6.0.0-dev".into(),
            need: "6.0.0-be.788".into()
        })
    );
    assert_eq!(
        get("Dev733").unwrap().loader.version.as_deref(),
        Some("6.0.0-dev+3a12d1810bf4dd5ca1635352ccf5bff5d95f987f"),
        "what its version resource says, though it is no `-be.N` build"
    );
    assert!(get("Be5").is_none(), "{found:#?}");
    assert!(get("NeverRun").is_none(), "{found:#?}");

    // The same profiles for a Mono game: BepInEx 6 IL2CPP is not asked there, BepInEx 5 is.
    let mono = World::new(NEW_APPID);
    mono.install(MODS_DECL);
    for (name, from) in [("Be788", &w), ("Be5", &w)] {
        copy_tree(&from.gale(GAME, name), &mono.gale(GAME, name));
    }
    let found = mono.discover();
    let names: Vec<(&str, LoaderKind)> = found
        .iter()
        .map(|s| (s.profile.as_str(), s.loader.kind))
        .collect();
    assert_eq!(names, [("Be5", LoaderKind::Bepinex5)]);
}

/// An IL2CPP integration declaring a build the built-in data does not hold (`versions =
/// "=6.0.0-be.788"`; a foundation narrows it, never widens it, §7.4) can be offered with no
/// BepInEx of the player's, whatever its build: the player is never told to install a version
/// the other side refuses (735 for the integration, 788 for the data).
#[test]
fn an_il2cpp_integration_at_a_build_outside_the_data_is_offered_with_no_source() {
    let w = World::il2cpp(NEW_APPID);
    w.install(&MODS_DECL.replace(
        "loader = \"bepinex5\"",
        "loader = \"bepinex6-il2cpp\"\nbepinex_build = 735",
    ));
    for (name, version) in [
        ("Be735", "6.0.0-be.735+5fef357"),
        ("Be788", "6.0.0-be.788+5b766a3"),
        ("Dev", "6.0.0-dev+3a12d18"),
    ] {
        let p = w.gale(GAME, name);
        bepinex6_il2cpp_at(&p, version);
        plugin_at(&p, "M/M.dll");
    }
    let found = w.discover();
    assert_eq!(found.len(), 3, "{found:#?}");
    for s in &found {
        assert_eq!(
            s.playable,
            Err(Unplayable::NotSupported {
                reason: "outside-versions".into()
            }),
            "{}",
            s.profile
        );
    }
    // At 788, the integration the data holds: offered.
    let w = World::il2cpp(NEW_APPID);
    w.install(&MODS_DECL.replace(
        "loader = \"bepinex5\"",
        "loader = \"bepinex6-il2cpp\"\nbepinex_build = 788",
    ));
    let p = w.gale(GAME, "Be788");
    bepinex6_il2cpp_at(&p, "6.0.0-be.788+5b766a3");
    plugin_at(&p, "M/M.dll");
    assert_eq!(w.discover()[0].playable, Ok(()));
}

#[test]
fn an_integration_that_cannot_play_with_mods_says_why() {
    let reason = |w: &World| match &w.discover()[0].playable {
        Err(Unplayable::NotSupported { reason }) => reason.clone(),
        other => panic!("{other:?}"),
    };
    let w = World::new(NEW_APPID);
    profile_with_a_mod(&w.gale(GAME, "P"));
    assert_eq!(reason(&w), "not-installed");
    // No table at all (an IL2CPP runner until it is proven): listed, "only without mods for now".
    w.install("");
    let found = w.discover();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].playable, Err(Unplayable::IntegrationWithoutMods));
    assert_eq!(default_mods_source(&found), None);
    // A table this launcher cannot use is not a reason to refuse the install.
    w.install(&format!("{MODS_DECL}future_key = 1\n"));
    assert_eq!(reason(&w), codes::UNKNOWN_KEY);
    w.install(&MODS_DECL.replace("bepinex5", "bepinex7"));
    assert_eq!(reason(&w), codes::MANIFEST_INVALID);

    let mut native = World::new(NEW_APPID);
    native.platform = Platform::LinuxNative;
    profile_with_a_mod(&native.gale(GAME, "P"));
    native.install(MODS_DECL);
    assert_eq!(reason(&native), "platform");
}

// ---- folders the player added ----

#[test]
fn folders_the_player_added_are_looked_in_too() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    // A custom r2modman data folder (its setting lives in its database: added by hand).
    let custom = w.tmp.join("D/r2modmanPlus-local");
    profile_with_a_mod(&custom.join(GAME).join("profiles/Custom"));
    // A Gale profile itself, and a folder of profiles somewhere else.
    let gale_profile = w
        .tmp
        .join("E/com.kesomannen.gale/some-game/profiles/Direct");
    profile_with_a_mod(&gale_profile);
    let folder = w.tmp.join("F/Peak mods");
    profile_with_a_mod(&folder.join("profiles/Mine"));
    set_mods_choice(
        &w.profile(),
        Choice {
            choice: ChoiceKind::Without,
            source: None,
            added_roots: vec![custom.clone(), gale_profile.clone(), folder.clone()],
        },
    )
    .unwrap();
    let mut found = roots(&w.discover());
    found.sort();
    let mut want = vec![
        (
            ModManager::R2modman,
            real(&custom.join(GAME).join("profiles/Custom")),
        ),
        (ModManager::Gale, real(&gale_profile)),
        (ModManager::Folder, real(&folder.join("profiles/Mine"))),
    ];
    want.sort();
    assert_eq!(found, want);
}

// ---- discovery writes nothing ----

#[test]
fn discovery_writes_nothing_anywhere() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    profile_with_a_mod(&w.gale(GAME, "Main"));
    log(
        &w.gale(GAME, "Main"),
        &[&start_line(GAME)],
        Duration::from_secs(5),
    );
    bepinex5_at(&w.game_dir(), "5.4.23.5");
    plugin_at(&w.game_dir(), "Their/Their.dll");
    let before = snapshot(w.tmp.path());
    let found = w.discover();
    assert_eq!(found.len(), 2);
    assert_eq!(snapshot(w.tmp.path()), before);
}

// ---- the remembered choice ----

#[test]
fn the_choice_round_trips_in_its_documented_shape() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    assert_eq!(mods_choice(&w.profile()).unwrap(), None);
    assert_eq!(mods_choice(&w.tmp.join("no-such-profile")).unwrap(), None);
    let root = w.gale(GAME, "Default");
    let choice = Choice {
        choice: ChoiceKind::With,
        source: Some(ChoiceSource {
            manager: ModManager::Gale,
            profile: "Default".into(),
            root: root.clone(),
        }),
        added_roots: vec![w.tmp.join("D")],
    };
    set_mods_choice(&w.profile(), choice.clone()).unwrap();
    assert_eq!(mods_choice(&w.profile()).unwrap(), Some(choice));
    let on_disk: serde_json::Value =
        serde_json::from_slice(&fs::read(w.profile().join(MODS_CHOICE_FILE_NAME)).unwrap())
            .unwrap();
    assert_eq!(
        on_disk,
        serde_json::json!({
            "schema": 1,
            "choice": "with",
            "source": {"manager": "gale", "profile": "Default", "root": root},
            "added_roots": [w.tmp.join("D")],
        })
    );

    // "without" needs no source; "with" does; every path is absolute. A new answer keeps the
    // folders already remembered (0.4.1): it never forgets one the person added.
    let without = Choice {
        choice: ChoiceKind::Without,
        source: None,
        added_roots: vec![],
    };
    set_mods_choice(&w.profile(), without.clone()).unwrap();
    assert_eq!(
        mods_choice(&w.profile()).unwrap(),
        Some(Choice {
            added_roots: vec![w.tmp.join("D")],
            ..without
        })
    );
    let no_source = Choice {
        choice: ChoiceKind::With,
        source: None,
        added_roots: vec![],
    };
    assert_eq!(
        set_mods_choice(&w.profile(), no_source).unwrap_err().code,
        codes::IO
    );
    let relative = Choice {
        choice: ChoiceKind::Without,
        source: None,
        added_roots: vec![PathBuf::from("relative/x")],
    };
    let e = set_mods_choice(&w.profile(), relative).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PATH_ESCAPES, Some("not-absolute"))
    );

    // A file edited by hand is judged the same way when it is read.
    fs::write(
        w.profile().join(MODS_CHOICE_FILE_NAME),
        br#"{"schema":1,"choice":null,"added_roots":["relative/x"]}"#,
    )
    .unwrap();
    let e = mods_choice(&w.profile()).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PATH_ESCAPES, Some("not-absolute"))
    );

    // A newer schema is said as such.
    fs::write(
        w.profile().join(MODS_CHOICE_FILE_NAME),
        br#"{"schema":2,"choice":"maybe"}"#,
    )
    .unwrap();
    let e = mods_choice(&w.profile()).unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::SCHEMA_TOO_NEW, Some(MODS_CHOICE_FILE_NAME))
    );
}

#[test]
fn an_update_keeps_the_choice_and_uninstall_takes_it_away() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let choice = Choice {
        choice: ChoiceKind::Without,
        source: None,
        added_roots: vec![],
    };
    set_mods_choice(&w.profile(), choice.clone()).unwrap();
    // Installing again (an update) is not a reason to ask again.
    w.install(MODS_DECL);
    assert_eq!(mods_choice(&w.profile()).unwrap(), Some(choice));
    uninstall(&w.profile(), &w.target()).unwrap();
    assert!(!w.profile().join(MODS_CHOICE_FILE_NAME).exists());
}

#[test]
fn no_manifest_file_may_be_named_like_the_choice() {
    let m =
        runner_manifest("\n[[files]]\nfrom = \"x.json\"\nto = \"${profile}/MODS-CHOICE.json\"\n");
    let e = open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("x.json", b"{}"),
    ]))
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PATH_ESCAPES, Some("reserved"))
    );
}

// ---- the manifest's table ----

#[test]
fn integration_mods_is_read_apart_and_never_refuses_the_manifest() {
    let parse = |extra: &str| Manifest::parse(&runner_manifest(extra)).unwrap();
    assert!(parse("").mods().is_none());
    let m = parse(&format!("\n{MODS_DECL}"));
    let d = m.mods().unwrap().unwrap();
    assert_eq!(d.loader, ModsLoader::Bepinex5);
    assert_eq!(d.chain, "${profile}/chain/Astra.Chain.dll");
    assert_eq!(d.chain_config, "${profile}/chain/chain.ini");
    let il2cpp = parse(&format!(
        "\n{}",
        MODS_DECL.replace("\"bepinex5\"", "\"bepinex6-il2cpp\"\nbepinex_build = 788")
    ));
    assert_eq!(
        il2cpp.mods().unwrap().unwrap().loader,
        ModsLoader::Bepinex6Il2cpp { build: 788 }
    );

    // Whatever is in it, the manifest itself parses: an older launcher ignores the table, and
    // this one only refuses a Play with mods.
    for (extra, code, key) in [
        (
            "future = true\n",
            codes::UNKNOWN_KEY,
            Some("integration.mods.future"),
        ),
        (
            "",
            codes::MANIFEST_INVALID,
            Some("integration.mods.bepinex_build"),
        ),
    ] {
        let decl = if extra.is_empty() {
            format!("\n{MODS_DECL}bepinex_build = 1\n")
        } else {
            format!("\n{MODS_DECL}{extra}")
        };
        let e = parse(&decl).mods().unwrap().unwrap_err().clone();
        assert_eq!((e.code, e.param("key")), (code, key), "{decl}");
    }
    let escaping = parse(&format!(
        "\n{}",
        MODS_DECL.replace("${profile}/chain/chain.ini", "${game}/chain.ini")
    ));
    assert_eq!(
        escaping.mods().unwrap().unwrap_err().code,
        codes::PATH_ESCAPES
    );
    let reserved = parse(&format!(
        "\n{}",
        MODS_DECL.replace("${profile}/chain/chain.ini", "${profile}/play-arm.json")
    ));
    assert_eq!(
        reserved.mods().unwrap().unwrap_err().code,
        codes::PATH_ESCAPES
    );
    // The chain reads chain.ini beside itself, and nothing else.
    for elsewhere in [
        "${profile}/chain.ini",
        "${profile}/chain/other.ini",
        "${profile}/chain/sub/chain.ini",
    ] {
        let m = parse(&format!(
            "\n{}",
            MODS_DECL.replace("${profile}/chain/chain.ini", elsewhere)
        ));
        let e = m.mods().unwrap().unwrap_err().clone();
        assert_eq!(
            (e.code, e.param("key")),
            (
                codes::MANIFEST_INVALID,
                Some("integration.mods.chain_config")
            ),
            "{elsewhere}"
        );
    }
    let cased = parse(&format!(
        "\n{}",
        MODS_DECL.replace("${profile}/chain/chain.ini", "${profile}/Chain/Chain.INI")
    ));
    assert!(
        cased.mods().unwrap().is_ok(),
        "Windows and Wine find it in any letter case"
    );
    let not_a_table = Manifest::parse(
        &runner_manifest("").replace("license = \"MIT\"", "license = \"MIT\"\nmods = \"yes\""),
    )
    .unwrap();
    assert_eq!(
        not_a_table.mods().unwrap().unwrap_err().code,
        codes::MANIFEST_INVALID
    );
}

// ---- their files are only ever read when they are regular files ----

/// A FIFO at `path` (`mkfifo`); `false` where there is no such tool.
#[cfg(unix)]
fn fifo(path: &Path) -> bool {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
#[test]
fn a_fifo_where_a_log_or_an_assembly_should_be_never_blocks_discovery() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    // A log naming the program would be read; this one is a FIFO no one writes.
    let logged = w.r2("Elsewhere", "Logged");
    profile_with_a_mod(&logged);
    // BepInEx.dll itself a FIFO.
    let odd = w.gale(GAME, "Odd");
    write(&odd.join("BepInEx/core/BepInEx.Preloader.dll"), b"MZ");
    plugin_at(&odd, "M/M.dll");
    if !fifo(&logged.join("BepInEx/LogOutput.log")) || !fifo(&odd.join("BepInEx/core/BepInEx.dll"))
    {
        eprintln!("no mkfifo here: skipped");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (target, profile, env) = (w.target(), w.profile(), w.env());
    std::thread::spawn(move || {
        let _ = tx.send(discover_mods_in(&target, &profile, &env));
    });
    let found = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("discovery opened a FIFO and waited for a writer");
    // The built-in data asks for `BepInEx.dll` as a FILE (`kind = "file"`, a probe opens only a
    // regular file): a FIFO there is no BepInEx 5, so "Odd" is no source (0.5.0 listed it with no
    // version; `tests/mods_identity.rs` declares the row).
    assert!(!found.iter().any(|s| s.profile == "Odd"), "{found:#?}");
    assert!(!found.iter().any(|s| s.profile == "Logged"));
    // A FIFO as the preloader, beside a `BepInEx.dll` that is a file: no source either, and no
    // wait.
    let odd2 = w.gale(GAME, "Odd2");
    bepinex5_at(&odd2, "5.4.23.5");
    plugin_at(&odd2, "M/M.dll");
    fs::remove_file(odd2.join("BepInEx/core/BepInEx.Preloader.dll")).unwrap();
    if !fifo(&odd2.join("BepInEx/core/BepInEx.Preloader.dll")) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (target, profile, env) = (w.target(), w.profile(), w.env());
    std::thread::spawn(move || {
        let _ = tx.send(discover_mods_in(&target, &profile, &env));
    });
    let found = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("discovery opened a FIFO and waited for a writer");
    assert!(!found.iter().any(|s| s.profile == "Odd2"), "{found:#?}");
}

#[test]
fn no_more_than_32_folders_are_remembered_and_the_oldest_goes_first() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let roots: Vec<PathBuf> = (0..34).map(|i| w.tmp.join(&format!("added{i}"))).collect();
    let choice = |n: usize| Choice {
        choice: ChoiceKind::Without,
        source: None,
        added_roots: roots[..n].to_vec(),
    };
    set_mods_choice(&w.profile(), choice(32)).unwrap();
    // Never stuck: one more pushes the oldest out.
    set_mods_choice(&w.profile(), choice(34)).unwrap();
    let kept = mods_choice(&w.profile()).unwrap().unwrap().added_roots;
    assert_eq!(kept, roots[2..].to_vec());
    add_mods_root(&w.profile(), &w.tmp.join("newest")).unwrap();
    let kept = mods_choice(&w.profile()).unwrap().unwrap().added_roots;
    assert_eq!(kept.len(), 32);
    assert_eq!(kept.first(), Some(&roots[3]));
    assert_eq!(kept.last(), Some(&w.tmp.join("newest")));
}

// ---- a folder added before any answer (0.4.1) ----

#[test]
fn a_folder_added_before_any_answer_is_looked_in_and_play_still_asks() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let custom = w.tmp.join("D/My mods");
    profile_with_a_mod(&custom.join("profiles/Mine"));
    add_mods_root(&w.profile(), &custom).unwrap();
    // Twice is once.
    add_mods_root(&w.profile(), &custom).unwrap();
    let on_disk: serde_json::Value =
        serde_json::from_slice(&fs::read(w.profile().join(MODS_CHOICE_FILE_NAME)).unwrap())
            .unwrap();
    assert_eq!(
        on_disk,
        serde_json::json!({"schema": 1, "choice": null, "added_roots": [custom]})
    );
    // No answer: Play keeps asking, and discovery looks in the folder.
    assert_eq!(mods_choice(&w.profile()).unwrap(), None);
    let found = w.discover();
    assert_eq!(
        roots(&found),
        [(ModManager::Folder, real(&custom.join("profiles/Mine")))]
    );
    let e = astra_game_launcher::prepare_launch_with(
        &w.profile(),
        &w.target(),
        &vars(),
        Some(Path::new("/opt/fake/steam")),
        &w.env(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("sources")),
        (codes::MODS_CHOICE_NEEDED, Some("1"))
    );
    assert_eq!(e.param("choice_unreadable"), None, "no answer, read whole");

    // The answer then keeps the folder, and a folder added after it keeps the answer.
    let answer = Choice {
        choice: ChoiceKind::With,
        source: Some(ChoiceSource::of(&found[0])),
        added_roots: vec![],
    };
    set_mods_choice(&w.profile(), answer.clone()).unwrap();
    let other = w.tmp.join("E");
    add_mods_root(&w.profile(), &other).unwrap();
    assert_eq!(
        mods_choice(&w.profile()).unwrap(),
        Some(Choice {
            added_roots: vec![custom.clone(), other.clone()],
            ..answer
        })
    );

    // Refused before anything touches it; past 32 the oldest goes, the answer stays.
    let e = add_mods_root(&w.profile(), Path::new("relative")).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PATH_ESCAPES, Some("not-absolute"))
    );
    for i in 0..31 {
        add_mods_root(&w.profile(), &w.tmp.join(&format!("F{i}"))).unwrap();
    }
    let now = mods_choice(&w.profile()).unwrap().unwrap();
    assert_eq!(now.choice, ChoiceKind::With);
    assert_eq!(now.added_roots.len(), 32);
    assert_eq!(
        now.added_roots[0], other,
        "the oldest, `custom`, went first"
    );
}

/// What launcher 0.4.0 does with a file 0.4.1 wrote with `"choice": null`. Its reader, as it was
/// at c832515 (`src/mods/choice.rs`: `OnDisk { schema, #[serde(flatten)] choice: Choice }`, with
/// `Choice::choice` a `ChoiceKind`, not an `Option`), cannot read it; and its rule for an answer
/// that cannot be read (`mods::play::resolve_play` at c832515, the very code still here, tested by
/// `an_answer_that_cannot_be_read_is_no_answer` in tests/play_with_mods.rs) is "no answer":
/// Play asks, or plays Astra alone, and discovery looks in no added folder. Harmless. 0.4.0
/// itself is not run here: that would need it as a git dev-dependency, which a `cargo test` with
/// no network cannot fetch; its reader is copied instead.
#[test]
fn a_file_with_no_answer_is_no_answer_to_launcher_0_4_0() {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Choice040 {
        choice: ChoiceKind,
        #[serde(default)]
        source: Option<ChoiceSource>,
        #[serde(default)]
        added_roots: Vec<PathBuf>,
    }
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OnDisk040 {
        schema: u32,
        #[serde(flatten)]
        choice: Choice040,
    }
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    add_mods_root(&w.profile(), &w.tmp.join("D")).unwrap();
    let text = fs::read_to_string(w.profile().join(MODS_CHOICE_FILE_NAME)).unwrap();
    assert!(serde_json::from_str::<OnDisk040>(&text).is_err(), "{text}");
    // And a file 0.4.1 writes with an answer is one 0.4.0 reads as before.
    set_mods_choice(
        &w.profile(),
        Choice {
            choice: ChoiceKind::Without,
            source: None,
            added_roots: vec![],
        },
    )
    .unwrap();
    let text = fs::read_to_string(w.profile().join(MODS_CHOICE_FILE_NAME)).unwrap();
    let old: OnDisk040 = serde_json::from_str(&text).unwrap();
    assert_eq!(old.choice.choice, ChoiceKind::Without);
    assert_eq!(old.choice.added_roots, [w.tmp.join("D")]);
}

/// Windows only: a share, a pipe or a verbatim namespace is refused before anything touches it,
/// and a verbatim drive path is remembered as a person sees it.
#[cfg(windows)]
#[test]
fn a_share_or_a_device_is_never_a_folder_to_look_in() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    for (p, reason) in [
        (r"\\host\share\mods", "unc"),
        (r"\\?\UNC\host\share\mods", "verbatim-unc"),
        (r"\\.\pipe\mods", "device"),
        (r"\\?\GLOBALROOT\x", "verbatim"),
    ] {
        let e = add_mods_root(&w.profile(), Path::new(p)).unwrap_err();
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PATH_ESCAPES, Some(reason)),
            "{p}"
        );
    }
    let verbatim = format!(r"\\?\{}", w.tmp.join("D").display());
    add_mods_root(&w.profile(), Path::new(&verbatim)).unwrap();
    let text = fs::read_to_string(w.profile().join(MODS_CHOICE_FILE_NAME)).unwrap();
    assert!(!text.contains(r"\\\\?"), "{text}");
}

/// The CLI's `mods --choose with --source <folder>` judges the folder's spelling before it
/// touches it (on Windows, canonicalizing a share would open an SMB session).
#[test]
fn the_cli_checks_a_sources_spelling_before_touching_it() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_astra-game-launcher"))
        .args(["mods", "--profile"])
        .arg(w.profile())
        .arg("--game")
        .arg(w.game_dir())
        .args([
            "--exe",
            &format!("{GAME}.exe"),
            "--choose",
            "with",
            "--source",
            "relative/x",
            "--json",
        ])
        .current_dir(w.tmp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let e: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(e["code"], "path-escapes", "{e}");
    assert_eq!(e["params"]["reason"], "not-absolute", "{e}");
}

// ---- Unreal: UE4SS and the `~mods` paks, found through the built-in data (0.5.1) ----

/// An Unreal game (`Palworld/Pal/…`), a profile, and a home with no mod manager.
struct Unreal {
    tmp: Tmp,
    exe: String,
}

impl Unreal {
    fn new() -> Unreal {
        let tmp = Tmp::new("mods-unreal");
        let exe = unreal_game(&tmp.join("Palworld"), "Pal");
        fs::create_dir_all(tmp.join("home")).unwrap();
        Unreal { tmp, exe }
    }

    fn dir(&self) -> PathBuf {
        self.tmp.join("Palworld")
    }

    fn bin(&self) -> PathBuf {
        self.dir().join("Pal/Binaries/Win64")
    }

    fn target(&self) -> astra_game_launcher::GameTarget {
        target(&self.dir(), &self.exe, Some(NEW_APPID), Platform::Windows)
    }

    fn profile(&self) -> PathBuf {
        self.tmp.join("profiles/p")
    }

    fn discover(&self) -> Vec<ModSource> {
        discover_mods_in(
            &self.target(),
            &self.profile(),
            &ModsEnv::linux(Some(&self.tmp.join("home")), None, None),
        )
    }

    /// A test Unreal runner installed, placing `game_files` (`(rel below the program's folder,
    /// bytes)`) beside the program and nothing else of the player's kind.
    fn install(&self, game_files: &[(&str, &[u8])]) {
        let mut m = String::from(
            "schema = 1\n\n[integration]\nid = \"test.unreal\"\nname = \"Test Unreal runner\"\nversion = \"1.0.0\"\nauthors = [\"tester\"]\nlicense = \"MIT\"\n\n[target]\nengine = \"unreal\"\nexe = \"*\"\nplatforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"runner/**\"\nto = \"${profile}/runner/\"\n",
        );
        let mut entries = vec![E::File("runner/Runner.dll", b"MZ runner")];
        for (rel, bytes) in game_files {
            m.push_str(&format!(
                "\n[[game_files]]\nfrom = \"game/{rel}\"\nto = \"${{game}}/{rel}\"\nsha256 = \"{}\"\n",
                sha(bytes)
            ));
        }
        m.push_str("\n[launch]\nargs = []\n");
        let names: Vec<String> = game_files
            .iter()
            .map(|(r, _)| format!("game/{r}"))
            .collect();
        entries.insert(0, E::File("astra-gi.toml", m.as_bytes()));
        for ((_, bytes), name) in game_files.iter().zip(&names) {
            entries.push(E::File(name.as_str(), bytes));
        }
        plan_and_install(
            &open(&zip(&entries)).unwrap(),
            &self.target(),
            &self.profile(),
        );
    }
}

#[test]
fn a_ue4ss_3_of_the_players_is_a_source_astra_chains() {
    let u = Unreal::new();
    ue4ss3_at(
        &u.dir(),
        "Pal",
        "3.0.1",
        "dwmapi.dll",
        &[("Alpha", true), ("Beta", false), ("Gamma", true)],
    );
    // `enabled.txt` in a mod's own folder switches it on with no list edit.
    write(&u.bin().join("ue4ss/Mods/Delta/enabled.txt"), b"");
    let found = u.discover();
    assert_eq!(found.len(), 1, "{found:#?}");
    let s = &found[0];
    assert_eq!(
        (s.manager, s.profile.as_str(), s.loader.kind),
        (ModManager::Manual, "", LoaderKind::Ue4ss3)
    );
    assert_eq!(real(&s.root), real(&u.dir()), "the folder detection ran on");
    assert_eq!(s.loader.version.as_deref(), Some("3.0.1"));
    assert_eq!(s.loader.preloader.as_deref(), Some("UE4SS.dll"));
    assert_eq!(s.mods, 3, "Alpha, Gamma, Delta");
    assert_eq!(s.coexist, [Coexistence::Chain]);
    assert_eq!(s.last_played, None, "UE4SS's own log is not read");
    assert_eq!(
        s.playable,
        Err(Unplayable::NotSupported {
            reason: "not-installed".into()
        })
    );

    // Some installs load it through `xinput1_3.dll`; with no proxy at all it loads into nothing.
    fs::rename(u.bin().join("dwmapi.dll"), u.bin().join("xinput1_3.dll")).unwrap();
    assert_eq!(u.discover().len(), 1);
    fs::remove_file(u.bin().join("xinput1_3.dll")).unwrap();
    assert_eq!(u.discover(), []);
    write(&u.bin().join("dwmapi.dll"), b"MZ ue4ss proxy");

    // A UE4SS 4: recognised, and not one Astra's runner runs beside (`versions = ">=3, <4"`).
    write(
        &u.bin().join("ue4ss/UE4SS.dll"),
        &pe_with_resource(&[("ProductVersion", "4.0.0")]),
    );
    assert_eq!(
        u.discover()[0].playable,
        Err(Unplayable::NotSupported {
            reason: "not-installed".into()
        }),
        "the integration first, as for BepInEx"
    );

    // An Unreal runner installed: it has no `[integration.mods]` (it chains their UE4SS by
    // itself), so the choice of "with" is not asked.
    write(
        &u.bin().join("ue4ss/UE4SS.dll"),
        &pe_with_resource(&[("ProductVersion", "3.0.1")]),
    );
    u.install(&[("astra-runner.txt", b"ours")]);
    let found = u.discover();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].playable, Err(Unplayable::IntegrationWithoutMods));
    assert_eq!(found[0].coexist, [Coexistence::Chain]);
}

#[test]
fn ue4ss_2_is_recognised_and_astra_cannot_run_beside_it() {
    let u = Unreal::new();
    ue4ss2_at(&u.dir(), "Pal", "2.5.2", &[("Old", true), ("Off", false)]);
    let found = u.discover();
    assert_eq!(found.len(), 1, "{found:#?}");
    let s = &found[0];
    assert_eq!(s.loader.kind, LoaderKind::Ue4ss2);
    assert_eq!(s.loader.version.as_deref(), Some("2.5.2"));
    assert_eq!(s.loader.preloader, None, "nothing of it is chained");
    assert_eq!(s.mods, 1);
    assert_eq!(s.coexist, [], "chain-incapable: `coexist = []`");
    // Said before anything about the integration: no install makes it playable.
    assert_eq!(
        s.playable,
        Err(Unplayable::LoaderUnsupported {
            have: "2.5.2".into(),
            need: "UE4SS 3".into()
        })
    );
    u.install(&[("astra-runner.txt", b"ours")]);
    assert_eq!(u.discover()[0].playable, s.playable);
}

#[test]
fn the_mods_paks_are_shown_and_run_beside_astra() {
    let u = Unreal::new();
    paks_at(
        &u.dir(),
        "Pal",
        &["a_P.pak", "B.PAK", "c.pak.old", "readme.txt"],
    );
    fs::create_dir_all(u.dir().join("Pal/Content/Paks/~mods/sub")).unwrap();
    let found = u.discover();
    assert_eq!(found.len(), 1, "{found:#?}");
    let s = &found[0];
    assert_eq!(s.loader.kind, LoaderKind::UnrealPaks);
    assert_eq!(
        (s.loader.version.as_deref(), s.loader.preloader.as_deref()),
        (None, None)
    );
    assert_eq!(s.mods, 2, "a_P.pak and B.PAK: present means enabled");
    assert_eq!(s.coexist, [Coexistence::SideBySide]);
    // An empty `~mods` holds no mod of the player's: not listed.
    for p in ["a_P.pak", "B.PAK"] {
        fs::remove_file(u.dir().join("Pal/Content/Paks/~mods").join(p)).unwrap();
    }
    assert_eq!(u.discover(), []);

    // Beside a UE4SS 3: two sources of one folder, in the data's order.
    paks_at(&u.dir(), "Pal", &["x.pak"]);
    ue4ss3_at(&u.dir(), "Pal", "3.0.1", "dwmapi.dll", &[("M", true)]);
    let kinds: Vec<LoaderKind> = u.discover().iter().map(|s| s.loader.kind).collect();
    assert_eq!(kinds, [LoaderKind::Ue4ss3, LoaderKind::UnrealPaks]);
}

#[test]
fn astras_own_ue4ss_in_the_game_folder_is_not_the_players() {
    let u = Unreal::new();
    ue4ss3_at(&u.dir(), "Pal", "3.0.1", "dwmapi.dll", &[("Theirs", true)]);
    // An older Unreal runner of Astra's placed UE4SS itself beside the program.
    let theirs = fs::read(u.bin().join("ue4ss/UE4SS.dll")).unwrap();
    fs::remove_dir_all(u.bin().join("ue4ss")).unwrap();
    u.install(&[
        ("ue4ss/UE4SS.dll", &theirs),
        ("ue4ss/UE4SS-settings.ini", b"[General]\n"),
    ]);
    write(&u.bin().join("ue4ss/Mods/Ours/enabled.txt"), b"");
    assert_eq!(
        u.discover(),
        [],
        "Astra's own loader is never one of the player's sources"
    );
}

#[test]
fn a_game_a_weak_rule_places_binds_no_project_and_finds_no_unreal_mods() {
    let u = Unreal::new();
    ue4ss3_at(&u.dir(), "Pal", "3.0.1", "dwmapi.dll", &[("M", true)]);
    assert_eq!(u.discover().len(), 1);
    // No `Content/Paks/`: only a weak rule names the program, and it binds no `{project}`
    // (engine-descriptor-v1.md §7.3): "not with mods", never a guess.
    fs::remove_dir_all(u.dir().join("Pal/Content")).unwrap();
    assert_eq!(u.discover(), []);
}

/// A folder past one evaluation's bounds (engine-descriptor-v1.md §7.8: here 1,025 mods in a
/// root) yields nothing of its mods: the source is shown, says so, and a Play with it is refused
/// for that reason — never a partial list.
#[test]
fn a_folder_past_one_evaluations_bounds_is_shown_and_says_so() {
    let w = World::new(NEW_APPID);
    w.install(MODS_DECL);
    let p = w.gale(GAME, "Huge");
    bepinex5_at(&p, "5.4.23.5");
    for i in 0..1025 {
        plugin_at(&p, &format!("M{i:04}/M.dll"));
    }
    let found = w.discover();
    assert_eq!(found.len(), 1, "{found:#?}");
    let s = &found[0];
    assert_eq!((s.loader.kind, s.mods), (LoaderKind::Bepinex5, 0));
    assert_eq!(
        s.loader.version, None,
        "nothing of it is read past the bound"
    );
    assert_eq!(
        s.playable,
        Err(Unplayable::NotSupported {
            reason: "over-budget".into()
        })
    );
    assert_eq!(s.coexist, [Coexistence::Chain]);
    set_mods_choice(
        &w.profile(),
        Choice {
            choice: ChoiceKind::With,
            source: Some(ChoiceSource::of(s)),
            added_roots: vec![],
        },
    )
    .unwrap();
    let e = astra_game_launcher::prepare_launch_with(
        &w.profile(),
        &w.target(),
        &vars(),
        Some(Path::new("/opt/fake/steam")),
        &w.env(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::MODS_UNSUPPORTED, Some("over-budget"))
    );
    // One fewer: every mod listed.
    fs::remove_dir_all(p.join("BepInEx/plugins/M1024")).unwrap();
    let s = &w.discover()[0];
    assert_eq!((s.mods, s.playable.clone()), (1024, Ok(())));
}

/// UE4SS and the paks are `source = ["game"]` in the built-in data: a folder the player picked
/// holding the same layout is never read for them (nor a manager's profile).
#[test]
fn unreal_loaders_are_looked_for_in_the_game_folder_only() {
    let u = Unreal::new();
    let picked = u.tmp.join("picked/Palworld copy");
    unreal_game(&picked, "Pal");
    ue4ss3_at(&picked, "Pal", "3.0.1", "dwmapi.dll", &[("M", true)]);
    paks_at(&picked, "Pal", &["x.pak"]);
    fs::create_dir_all(u.profile()).unwrap();
    add_mods_root(&u.profile(), &picked).unwrap();
    assert_eq!(u.discover(), []);
    // The same layout in the game folder is.
    ue4ss3_at(&u.dir(), "Pal", "3.0.1", "dwmapi.dll", &[("M", true)]);
    let found = u.discover();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].manager, ModManager::Manual);
}
