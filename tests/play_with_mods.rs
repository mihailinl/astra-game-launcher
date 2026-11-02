// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play with the player's own mods: the arguments for the proxy beside the game now (Doorstop 4
//! or 3, IL2CPP's runtime), their pack's own Doorstop settings, `chain.ini`, every refusal before
//! anything is written, nothing of theirs ever written — and, without mods, exactly the Play of
//! 0.3.2. Their "mod manager" is a fixture below a temporary home.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    ArmedBy, Choice, ChoiceKind, ChoiceSource, GameAction, LastRun, LauncherError, Ledger,
    LoaderReason, LoaderStatus, MODS_CHOICE_UNREADABLE, MODS_PACK_PATH_REFUSED, ModManager,
    ModsEnv, Platform, ProcessPlan, STEAM_LAUNCH_OPTIONS_DOORSTOP, codes, installed, loader_status,
    mods_last_run, prepare_launch_with, set_mods_choice,
};
use common::*;

const GAME: &str = "Some Game";
const APPID: u32 = 4_000_300;

/// Every option a Doorstop 4.5 build takes that these tests pass.
const V4_ALL: &[&str] = &[
    "--doorstop-enabled",
    "--doorstop-target-assembly",
    "--doorstop-mono-dll-search-path-override",
    "--doorstop-clr-corlib-dir",
    "--doorstop-clr-runtime-coreclr-path",
    "--doorstop-boot-config-override",
    "--doorstop-mono-debug-enabled",
    "--doorstop-mono-debug-suspend",
];
/// A Doorstop 3 build's (Subnautica's pack).
const V3_ALL: &[&str] = &[
    "--doorstop-enable",
    "--doorstop-target",
    "--doorstop-dll-search-override",
];

fn ours() -> Vec<u8> {
    pe_with_options(X64, V4_ALL, b"4.5.0 ours")
}

const IL2CPP_DECL: &str = "[integration.mods]\nloader = \"bepinex6-il2cpp\"\nbepinex_build = 788\nchain = \"${profile}/chain/Astra.Chain.dll\"\nchain_config = \"${profile}/chain/chain.ini\"\n";

/// A path as the launcher hands it on: canonical on Unix; on Windows as this test built it
/// (below its temporary folder, written plainly), `\`-separated as the launcher joins it.
fn as_handed_on(p: &Path) -> String {
    if cfg!(windows) {
        p.display().to_string().replace('/', "\\")
    } else {
        fs::canonicalize(p).unwrap().display().to_string()
    }
}

struct World {
    tmp: Tmp,
    platform: Platform,
}

impl World {
    fn new(proton: bool) -> World {
        let tmp = Tmp::new("play-mods");
        let platform = if proton {
            let pfx = tmp.join("pfx");
            write(&pfx.join("user.reg"), USER_REG.as_bytes());
            Platform::LinuxProton { prefix: Some(pfx) }
        } else {
            Platform::Windows
        };
        let w = World { tmp, platform };
        unity_game(&w.game(), GAME);
        write(
            &w.game().join(format!("{GAME}.exe")),
            &pe_with_options(X64, &[], b"game"),
        );
        fs::create_dir_all(w.tmp.join("home")).unwrap();
        w
    }

    fn game(&self) -> PathBuf {
        self.tmp.join("common").join(GAME)
    }

    fn profile(&self) -> PathBuf {
        self.tmp.join("profiles").join("p")
    }

    /// The profile as the game sees it, the way 0.3.2 wrote it: `\`-separated, `Z:` under Proton.
    fn profile_text(&self) -> String {
        let p = as_handed_on(&self.profile()).replace('/', "\\");
        match self.platform {
            Platform::LinuxProton { .. } => format!("Z:{p}"),
            _ => p,
        }
    }

    /// A path of theirs as the game sees it: as it is natively, `Z:\…` under Proton.
    fn their_text(&self, p: &Path) -> String {
        let p = as_handed_on(p);
        match self.platform {
            Platform::LinuxProton { .. } => format!("Z:{}", p.replace('/', "\\")),
            _ => p,
        }
    }

    fn env(&self) -> ModsEnv {
        ModsEnv::linux(Some(&self.tmp.join("home")), None, None)
    }

    fn target(&self) -> astra_game_launcher::GameTarget {
        target(
            &self.game(),
            &format!("{GAME}.exe"),
            Some(APPID),
            self.platform.clone(),
        )
    }

    /// The runner installed. An IL2CPP runner is for an IL2CPP game: the built-in data asks for
    /// BepInEx 6 IL2CPP in one only (`codes = ["unity-il2cpp"]`), so the game becomes one first.
    fn install_with(&self, proxy: &[u8], decl: &str) {
        let engine = if decl.contains("bepinex6-il2cpp") {
            write(&self.game().join("GameAssembly.dll"), b"MZ il2cpp");
            "unity-il2cpp"
        } else {
            "unity-mono"
        };
        plan_and_install(
            &mods_runner_for(proxy, decl, engine),
            &self.target(),
            &self.profile(),
        );
    }

    fn install(&self, decl: &str) {
        self.install_with(&ours(), decl);
    }

    /// Their Gale profile for this game.
    fn theirs(&self, name: &str) -> PathBuf {
        self.tmp
            .join("home")
            .join(".local")
            .join("share")
            .join("com.kesomannen.gale")
            .join(GAME)
            .join("profiles")
            .join(name)
    }

    /// Their BepInEx 5 profile `name`, one mod, the pack's own `doorstop_config.ini` = `ini`.
    fn their_profile(&self, name: &str, ini: &str) -> PathBuf {
        let p = self.theirs(name);
        bepinex5_at(&p, "5.4.23.5+57f1fb8");
        plugin_at(&p, "Their-Mod/Their.dll");
        write(&p.join("doorstop_config.ini"), ini.as_bytes());
        write(&p.join("winhttp.dll"), &ours());
        p
    }

    fn choose(&self, choice: ChoiceKind, root: Option<&Path>) {
        set_mods_choice(
            &self.profile(),
            Choice {
                choice,
                source: root.map(|r| ChoiceSource {
                    manager: ModManager::Gale,
                    profile: r.file_name().unwrap().to_string_lossy().into_owned(),
                    root: r.to_path_buf(),
                }),
                added_roots: vec![],
            },
        )
        .unwrap();
    }

    fn play(&self) -> Result<ProcessPlan, LauncherError> {
        prepare_launch_with(
            &self.profile(),
            &self.target(),
            &vars(),
            Some(Path::new("/opt/fake/steam")),
            &self.env(),
        )
    }

    fn chain_ini(&self) -> Option<String> {
        fs::read_to_string(self.profile().join("chain/chain.ini")).ok()
    }

    fn ledger(&self) -> Ledger {
        installed(&self.profile()).unwrap().unwrap()
    }
}

fn s(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

const PACK_INI: &str = "[General]\nenabled = true\ntarget_assembly = BepInEx\\core\\BepInEx.Preloader.dll\nboot_config_override =\n[UnityMono]\ndll_search_path_override =\ndebug_enabled = false\n";

/// What 0.3.2 passed (captured from b94c456 with this very runner, before any of this existed).
fn argv_0_3_2(w: &World, boot_pin: bool) -> Vec<String> {
    let mut a = s(&[
        "-applaunch",
        &APPID.to_string(),
        "--doorstop-enabled",
        "true",
    ]);
    a.push("--doorstop-target-assembly".to_owned());
    a.push(format!(
        "{}\\BepInEx\\core\\BepInEx.Preloader.dll",
        w.profile_text()
    ));
    a.extend(s(&[
        "--doorstop-mono-debug-enabled",
        "false",
        "--doorstop-mono-debug-suspend",
        "false",
    ]));
    if boot_pin {
        a.extend(s(&[
            "--doorstop-boot-config-override",
            &format!("{GAME}_Data/boot.config"),
        ]));
    }
    a
}

// ---- without mods: 0.3.2, argument for argument ----

#[test]
fn without_mods_every_argument_is_what_0_3_2_passed() {
    let proxy = pe_with_options(X64, DOORSTOP_V4, b"4.3.0 ours");
    for proton in [false, true] {
        // No [integration.mods] at all.
        let w = World::new(proton);
        w.install_with(&proxy, "");
        w.their_profile("Default", PACK_INI);
        let p = w.play().unwrap();
        assert_eq!(p.args, argv_0_3_2(&w, false), "proton {proton}");
        assert_eq!(
            (p.program.as_path(), p.env.len(), p.cwd.as_deref()),
            (Path::new("/opt/fake/steam"), 0, None)
        );

        // The table, and the player chose "without" — or nothing is found to ask about.
        let w = World::new(proton);
        w.install_with(&proxy, MODS_DECL);
        assert_eq!(w.play().unwrap().args, argv_0_3_2(&w, false));
        w.their_profile("Default", PACK_INI);
        w.choose(ChoiceKind::Without, None);
        assert_eq!(w.play().unwrap().args, argv_0_3_2(&w, false));
        assert_eq!(w.chain_ini(), None, "nothing of the chain's is written");

        // Their manager relinked its proxy and config over ours: the pin 0.3.2 added.
        write(
            &w.game().join("winhttp.dll"),
            &pe_with_options(
                X64,
                &[DOORSTOP_V4[0], "--doorstop-bypass-vulkan", DOORSTOP_V4[1]],
                b"gale",
            ),
        );
        write(
            &w.game().join("doorstop_config.ini"),
            b"[General]\nenabled = true\n",
        );
        assert_eq!(w.play().unwrap().args, argv_0_3_2(&w, true));
    }
}

// ---- with mods ----

#[test]
fn doorstop_4_starts_the_chain_on_their_bepinex_and_writes_chain_ini() {
    let w = World::new(false);
    w.install(MODS_DECL);
    let theirs = w.their_profile("Default", PACK_INI);
    w.choose(ChoiceKind::With, Some(&theirs));
    let ledger_before = w.ledger();
    let p = w.play().unwrap();
    let mut want = s(&[
        "-applaunch",
        &APPID.to_string(),
        "--doorstop-enabled",
        "true",
    ]);
    want.push("--doorstop-target-assembly".to_owned());
    want.push(format!("{}\\chain\\Astra.Chain.dll", w.profile_text()));
    want.extend(s(&[
        "--doorstop-mono-debug-enabled",
        "false",
        "--doorstop-mono-debug-suspend",
        "false",
    ]));
    assert_eq!(p.args, want);
    let ini = fs::read(w.profile().join("chain/chain.ini")).unwrap();
    assert_eq!(
        String::from_utf8(ini).unwrap(),
        format!(
            "[Chain]\ntarget = {}\nsource = gale\nprofile = Default\n",
            w.their_text(&theirs.join("BepInEx/core/BepInEx.Preloader.dll"))
        ),
        "UTF-8, no BOM, the three keys"
    );
    assert_eq!(w.ledger(), ledger_before, "nothing to record");
    // The play-arm is still `launch`'s, for the same profile.
    assert_eq!(p.play_arm().map(|(_, id)| id), Some(APPID));
}

#[test]
fn under_proton_their_paths_are_z_and_relative_settings_are_found_below_their_root_then_the_game() {
    let w = World::new(true);
    w.install(MODS_DECL);
    let ini = "[General]\nboot_config_override =\n[UnityMono]\ndll_search_path_override = unstripped_corlib;BepInEx\\shared;both;nowhere\n";
    let theirs = w.their_profile("Default", ini);
    // `unstripped_corlib` in their profile; `BepInEx/shared` only in the game folder (their
    // manager copied it there); `nowhere` nowhere: left out.
    write(&theirs.join("unstripped_corlib/mscorlib.dll"), b"MZ");
    write(&w.game().join("BepInEx/shared/x.dll"), b"MZ");
    // `both` in their profile and in the game folder: theirs, looked in first.
    write(&theirs.join("both/x.dll"), b"MZ");
    write(&w.game().join("both/x.dll"), b"MZ");
    w.choose(ChoiceKind::With, Some(&theirs));
    let p = w.play().unwrap();
    let at = p
        .args
        .iter()
        .position(|a| a == "--doorstop-mono-dll-search-path-override")
        .unwrap();
    assert_eq!(
        p.args[at + 1],
        format!(
            "{};{};{}",
            w.their_text(&theirs.join("unstripped_corlib")),
            w.their_text(&w.game().join("BepInEx/shared")),
            w.their_text(&theirs.join("both"))
        )
    );
    // A Linux path under Proton: `Z:` and the root's `/` (a Windows host's path has its drive).
    assert!(
        cfg!(windows) || p.args[at + 1].starts_with("Z:\\"),
        "{}",
        p.args[at + 1]
    );
    assert_eq!(
        p.args[5],
        format!("{}\\chain\\Astra.Chain.dll", w.profile_text())
    );
    let ini = w.chain_ini().unwrap();
    assert!(
        ini.contains(&format!(
            "target = Z:{}\\BepInEx\\core\\BepInEx.Preloader.dll\n",
            as_handed_on(&theirs).replace('/', "\\")
        )),
        "{ini}"
    );
}

#[test]
fn their_own_boot_config_override_is_theirs_and_a_windows_path_is_passed_as_written() {
    let w = World::new(true);
    w.install(MODS_DECL);
    let ini = "[General]\nboot_config_override = C:\\Games\\boot.config\n";
    let theirs = w.their_profile("Default", ini);
    w.choose(ChoiceKind::With, Some(&theirs));
    // A config not ours beside the game: 0.3.2 would pin boot.config; theirs is passed instead.
    write(
        &w.game().join("doorstop_config.ini"),
        b"[General]\nenabled = true\n",
    );
    let p = w.play().unwrap();
    let boots: Vec<&String> = p
        .args
        .iter()
        .skip_while(|a| *a != "--doorstop-boot-config-override")
        .take(2)
        .collect();
    assert_eq!(
        boots,
        ["--doorstop-boot-config-override", "C:\\Games\\boot.config"]
    );
    assert_eq!(
        p.args
            .iter()
            .filter(|a| *a == "--doorstop-boot-config-override")
            .count(),
        1
    );
}

#[test]
fn a_doorstop_3_proxy_their_manager_relinked_gets_doorstop_3_options() {
    let w = World::new(false);
    w.install(MODS_DECL);
    let ini = "[UnityDoorstop]\nenabled=true\ntargetAssembly=BepInEx\\core\\BepInEx.Preloader.dll\ndllSearchPathOverride=unstripped_managed\n";
    let theirs = w.their_profile("Subnautica", ini);
    write(&theirs.join("unstripped_managed/UnityEngine.dll"), b"MZ");
    // Subnautica's pack: their manager copies its Doorstop 3 proxy over ours.
    let v3 = pe_with_options(X64, V3_ALL, b"doorstop 3");
    write(&w.game().join("winhttp.dll"), &v3);

    // Astra alone cannot start through it (as in 0.3.2) …
    w.choose(ChoiceKind::Without, None);
    assert_eq!(w.play().unwrap_err().code, codes::SHARED_LOADER_CHANGED);
    // … but their own BepInEx can: Doorstop 3's spelling, no Doorstop 4 pins.
    w.choose(ChoiceKind::With, Some(&theirs));
    let p = w.play().unwrap();
    let mut want = s(&[
        "-applaunch",
        &APPID.to_string(),
        "--doorstop-enable",
        "true",
    ]);
    want.push("--doorstop-target".to_owned());
    want.push(format!("{}\\chain\\Astra.Chain.dll", w.profile_text()));
    want.push("--doorstop-dll-search-override".to_owned());
    want.push(w.their_text(&theirs.join("unstripped_managed")));
    assert_eq!(p.args, want);
    // The relinked proxy is shared now, as any Play records it.
    let g = w.ledger().game_files;
    let proxy = g.iter().find(|g| g.rel == "winhttp.dll").unwrap();
    assert_eq!(
        (proxy.action, proxy.sha256.clone()),
        (GameAction::ShareLoader, sha(&v3))
    );
}

#[test]
fn il2cpp_passes_their_dotnet_and_needs_doorstop_4() {
    let w = World::new(true);
    w.install(IL2CPP_DECL);
    let theirs = w.theirs("Il2cpp");
    bepinex6_il2cpp_at(
        &theirs,
        "6.0.0-be.788+5b766a3b7f6c164d4798924a93f3acf4db769d06",
    );
    plugin_at(&theirs, "M/M.dll");
    write(
        &theirs.join("doorstop_config.ini"),
        b"[General]\nenabled = true\n[Il2Cpp]\ncoreclr_path = dotnet\\coreclr.dll\ncorlib_dir = dotnet\n",
    );
    w.choose(ChoiceKind::With, Some(&theirs));
    let p = w.play().unwrap();
    let after = |opt: &str| {
        let i = p.args.iter().position(|a| a == opt).unwrap();
        p.args[i + 1].clone()
    };
    assert_eq!(
        after("--doorstop-clr-corlib-dir"),
        w.their_text(&theirs.join("dotnet"))
    );
    assert_eq!(
        after("--doorstop-clr-runtime-coreclr-path"),
        w.their_text(&theirs.join("dotnet/coreclr.dll"))
    );
    assert!(
        w.chain_ini()
            .unwrap()
            .contains("\\BepInEx\\core\\BepInEx.Unity.IL2CPP.dll\n")
    );

    // Their .NET runtime gone: their source is not whole. The built-in data requires `dotnet/`
    // to recognise a BepInEx 6 IL2CPP at all (0.5.0 recognised it and then missed the file), so
    // there is no BepInEx there any more.
    fs::remove_dir_all(theirs.join("dotnet")).unwrap();
    let e = w.play().unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::MODS_SOURCE_MISSING, Some("no-preloader"))
    );
    write(&theirs.join("dotnet/coreclr.dll"), b"MZ coreclr");
    // Their pack's ini naming a runtime folder that is not there: the file it names is missing.
    write(
        &theirs.join("doorstop_config.ini"),
        b"[General]\nenabled = true\n[Il2Cpp]\ncoreclr_path = gone\\coreclr.dll\ncorlib_dir = gone\n",
    );
    fs::remove_dir_all(theirs.join("dotnet")).unwrap();
    fs::create_dir_all(theirs.join("dotnet")).unwrap();
    let e = w.play().unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::MODS_SOURCE_MISSING, Some("dotnet/coreclr.dll"))
    );
    write(
        &theirs.join("doorstop_config.ini"),
        b"[General]\nenabled = true\n[Il2Cpp]\ncoreclr_path = dotnet\\coreclr.dll\ncorlib_dir = dotnet\n",
    );
    write(&theirs.join("dotnet/coreclr.dll"), b"MZ coreclr");

    // A Doorstop 3 proxy cannot start CoreCLR.
    write(
        &w.game().join("winhttp.dll"),
        &pe_with_options(X64, V3_ALL, b"v3"),
    );
    assert_eq!(w.play().unwrap_err().code, codes::SHARED_LOADER_CHANGED);
}

// ---- refusals: every one before anything is written ----

#[test]
fn every_refusal_comes_before_anything_is_written() {
    let w = World::new(true);
    w.install(MODS_DECL);
    let theirs = w.their_profile("Default", PACK_INI);
    let check = |want: &str, params: &[(&str, &str)]| {
        let before = snapshot(w.tmp.path());
        let e = w.play().unwrap_err();
        assert_eq!(e.code, want, "{e}");
        for (k, v) in params {
            assert_eq!(e.param(k), Some(*v), "{e}");
        }
        assert_eq!(snapshot(w.tmp.path()), before, "{want}: nothing written");
        assert_eq!(w.chain_ini(), None);
    };

    // Found, never asked: ask once.
    check(codes::MODS_CHOICE_NEEDED, &[("sources", "1")]);

    // Their profile gone, or its preloader.
    let gone = w.theirs("Gone");
    w.choose(ChoiceKind::With, Some(&gone));
    check(
        codes::MODS_SOURCE_MISSING,
        &[("profile", "Gone"), ("manager", "gale")],
    );
    let no_pre = w.their_profile("NoPreloader", PACK_INI);
    fs::remove_file(no_pre.join("BepInEx/core/BepInEx.Preloader.dll")).unwrap();
    w.choose(ChoiceKind::With, Some(&no_pre));
    check(codes::MODS_SOURCE_MISSING, &[("reason", "no-preloader")]);

    // A BepInEx the chain cannot run beside.
    let old = w.theirs("Old");
    bepinex5_at(&old, "5.3.1.0");
    plugin_at(&old, "M/M.dll");
    w.choose(ChoiceKind::With, Some(&old));
    check(
        codes::MODS_LOADER_UNSUPPORTED,
        &[("have", "5.3.1.0"), ("need", "5.4"), ("profile", "Old")],
    );

    // MelonLoader.
    let melon = w.theirs("Melon");
    fs::create_dir_all(melon.join("MelonLoader")).unwrap();
    write(&melon.join("Mods/M.dll"), b"MZ");
    w.choose(ChoiceKind::With, Some(&melon));
    check(codes::MODS_MELONLOADER, &[("profile", "Melon")]);

    // Our chain loader changed since the install.
    w.choose(ChoiceKind::With, Some(&theirs));
    let chain = w.profile().join("chain/Astra.Chain.Hooks.dll");
    let ours_bytes = fs::read(&chain).unwrap();
    fs::write(&chain, b"MZ someone else's").unwrap();
    check(
        codes::NEEDS_REINSTALL,
        &[("file", "chain/Astra.Chain.Hooks.dll")],
    );
    fs::write(&chain, &ours_bytes).unwrap();

    // The prefix is not there yet: a refusal of today's Play, still before chain.ini.
    let user_reg = w.tmp.join("pfx/user.reg");
    let reg = fs::read(&user_reg).unwrap();
    fs::remove_file(&user_reg).unwrap();
    check(codes::PROTON_PREFIX_MISSING, &[]);
    fs::write(&user_reg, &reg).unwrap();

    // And then it plays.
    w.play().unwrap();
    assert!(w.chain_ini().is_some());
}

#[test]
fn an_integration_without_the_table_never_asks_and_plays_astra_alone_whatever_was_chosen() {
    let proxy = pe_with_options(X64, DOORSTOP_V4, b"4.3.0 ours");
    for proton in [false, true] {
        let w = World::new(proton);
        w.install_with(&proxy, "");
        let theirs = w.their_profile("Default", PACK_INI);
        bepinex5_at(&w.game(), "5.4.23.5");
        plugin_at(&w.game(), "Their/Their.dll");
        // Sources are there (in their manager and in the game folder): never a question.
        assert_eq!(
            w.play().unwrap().args,
            argv_0_3_2(&w, false),
            "proton {proton}"
        );
        // A "with" remembered from a release that had the table: Astra alone, as today.
        w.choose(ChoiceKind::With, Some(&theirs));
        assert_eq!(
            w.play().unwrap().args,
            argv_0_3_2(&w, false),
            "proton {proton}"
        );
        assert_eq!(w.chain_ini(), None);
        // Not even an answer it cannot read stops it.
        fs::write(w.profile().join("mods-choice.json"), b"{ not json").unwrap();
        assert_eq!(w.play().unwrap().args, argv_0_3_2(&w, false));
    }
}

#[test]
fn an_integration_with_a_table_it_cannot_use_here_refuses_a_play_with_mods() {
    let w = World::new(false);
    w.install(&format!("{MODS_DECL}future = 1\n"));
    let theirs = w.their_profile("Default", PACK_INI);
    // Nothing offered: no question, Astra alone.
    w.play().unwrap();
    w.choose(ChoiceKind::With, Some(&theirs));
    let e = w.play().unwrap_err();
    assert_eq!(
        (e.code, e.param("key")),
        (codes::UNKNOWN_KEY, Some("integration.mods.future"))
    );

    // A native Linux game has no proxy to start the chain with.
    let mut native = World::new(false);
    native.platform = Platform::LinuxNative;
    native.install(MODS_DECL);
    let theirs = native.their_profile("Default", PACK_INI);
    native.play().unwrap();
    native.choose(ChoiceKind::With, Some(&theirs));
    let e = native.play().unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::MODS_UNSUPPORTED, Some("platform"))
    );
}

// ---- nothing of theirs is ever written ----

#[test]
fn a_play_with_mods_writes_nothing_of_theirs() {
    let w = World::new(true);
    w.install(MODS_DECL);
    let theirs = w.their_profile("Default", PACK_INI);
    write(&theirs.join("BepInEx/config/BepInEx.cfg"), b"[Logging]\n");
    w.choose(ChoiceKind::With, Some(&theirs));
    let their_dir = w.tmp.join("home");
    let (theirs_before, game_before) = (snapshot(&their_dir), snapshot(&w.game()));
    let profile_before = snapshot(&w.profile());
    w.play().unwrap();
    w.play().unwrap();
    assert_eq!(snapshot(&their_dir), theirs_before);
    assert_eq!(snapshot(&w.game()), game_before);
    // In Astra's profile: chain.ini and today's config writes, nothing else new.
    let after = snapshot(&w.profile());
    let added: Vec<&String> = after
        .keys()
        .filter(|k| !profile_before.contains_key(*k))
        .collect();
    assert_eq!(
        added,
        [
            "BepInEx/config/",
            "BepInEx/config/astra.cfg",
            "chain/chain.ini"
        ]
    );
}

// ---- Steam launch options ----

#[test]
fn steam_launch_options_with_doorstop_options_are_a_warning() {
    let w = World::new(false);
    w.install(MODS_DECL);
    w.choose(ChoiceKind::Without, None);
    let steam = w.tmp.join("steam");
    let mut game = w.target();
    game.steam_appinfo = Some(steam.join("appcache/appinfo.vdf"));
    let play = || {
        prepare_launch_with(
            &w.profile(),
            &game,
            &vars(),
            Some(Path::new("/opt/fake/steam")),
            &w.env(),
        )
        .unwrap()
    };
    assert!(play().warnings.is_empty());
    let config = |options: &str| {
        format!(
            "\"UserLocalConfigStore\"\n{{\n\"Software\"\n{{\n\"Valve\"\n{{\n\"Steam\"\n{{\n\"apps\"\n{{\n\"{APPID}\"\n{{\n\"LaunchOptions\"\t\t\"{options}\"\n}}\n}}\n}}\n}}\n}}\n}}\n"
        )
    };
    write(
        &steam.join("userdata/123/config/localconfig.vdf"),
        config("-novid %command%").as_bytes(),
    );
    assert!(play().warnings.is_empty());
    write(
        &steam.join("userdata/456/config/localconfig.vdf"),
        config("%command% --doorstop-enabled false").as_bytes(),
    );
    let p = play();
    assert_eq!(p.warnings, [STEAM_LAUNCH_OPTIONS_DOORSTOP]);
    assert_eq!(
        p.args,
        argv_0_3_2(&w, false),
        "a warning, never other arguments"
    );
}

// ---- what the runners say ----

#[test]
fn the_chain_s_report_is_read_beside_the_chain() {
    let w = World::new(false);
    assert_eq!(mods_last_run(&w.profile()).unwrap(), None);
    w.install(MODS_DECL);
    assert_eq!(mods_last_run(&w.profile()).unwrap(), None);
    let report = w.profile().join("chain/last-run.txt");
    fs::write(&report, "ok 2026-10-30T12:00:00Z 5.4.23.5\n").unwrap();
    assert_eq!(
        mods_last_run(&w.profile()).unwrap(),
        Some(LastRun::Ok {
            time: "2026-10-30T12:00:00Z".into(),
            version: "5.4.23.5".into()
        })
    );
    fs::write(
        &report,
        "fallback 2026-10-30T12:00:01Z chain.ini: not found at C:\\x\\chain.ini\n",
    )
    .unwrap();
    assert_eq!(
        mods_last_run(&w.profile()).unwrap(),
        Some(LastRun::Fallback {
            time: "2026-10-30T12:00:01Z".into(),
            reason: "chain.ini: not found at C:\\x\\chain.ini".into()
        })
    );
    assert_eq!(
        LastRun::parse("hooks-failed 2026-10-30T12:00:02Z Harmony: x\n"),
        Some(LastRun::HooksFailed {
            time: "2026-10-30T12:00:02Z".into(),
            error: "Harmony: x".into()
        })
    );
    assert_eq!(
        LastRun::parse("\n  later 2026-10-30T12:00:03Z what\n"),
        Some(LastRun::Other {
            status: "later".into(),
            time: "2026-10-30T12:00:03Z".into(),
            detail: "what".into()
        })
    );
    assert_eq!(LastRun::parse(""), None);
    assert_eq!(LastRun::parse("ok"), None);
}

#[test]
fn the_unreal_loader_s_status_is_typed_and_never_refused_for_a_new_reason() {
    let line = r#"{"schema":1,"reason":"stale-astra","armed_by":"play-arm","file":"dxgi.dll","version":"6.8.0","error":"","reshade":false,"ue4ss":false,"their_ue4ss":true,"unix_ms":1761825600000,"pid":4242,"exe":"Game-Win64-Shipping.exe"}"#;
    assert_eq!(
        LoaderStatus::parse(line),
        Some(LoaderStatus {
            schema: 1,
            reason: LoaderReason::StaleAstra,
            armed_by: ArmedBy::PlayArm,
            file: "dxgi.dll".into(),
            version: "6.8.0".into(),
            error: String::new(),
            reshade: false,
            ue4ss: false,
            their_ue4ss: true,
            unix_ms: 1_761_825_600_000,
            pid: 4242,
            exe: "Game-Win64-Shipping.exe".into(),
        })
    );
    for (reason, want) in [
        ("loaded", LoaderReason::Loaded),
        ("foreign-reshade", LoaderReason::ForeignReshade),
        ("load-failed", LoaderReason::LoadFailed),
        ("a-new-one", LoaderReason::Other("a-new-one".into())),
    ] {
        let s = LoaderStatus::parse(&format!(
            r#"{{"schema":1,"reason":"{reason}","armed_by":"argv"}}"#
        ))
        .unwrap();
        assert_eq!((s.reason, s.armed_by), (want, ArmedBy::Argv));
    }
    assert_eq!(
        LoaderStatus::parse(r#"{"reason":"loaded","armed_by":"a-later-way"}"#)
            .unwrap()
            .armed_by,
        ArmedBy::Other("a-later-way".into())
    );
    assert_eq!(LoaderStatus::parse("not json"), None);

    let w = World::new(false);
    w.install("");
    assert_eq!(loader_status(&w.profile()).unwrap(), None);
    fs::write(
        w.profile().join("astra-loader-status.json"),
        format!("{line}\n"),
    )
    .unwrap();
    assert_eq!(
        loader_status(&w.profile()).unwrap().unwrap().reason,
        LoaderReason::StaleAstra
    );
    fs::write(w.profile().join("astra-loader-status.json"), "{ half").unwrap();
    assert_eq!(loader_status(&w.profile()).unwrap_err().code, codes::IO);
}

#[test]
fn our_own_proxy_that_cannot_start_the_chain_refuses_before_anything_is_written() {
    // Our proxy knows Doorstop 4's two options and not IL2CPP's runtime ones.
    let w = World::new(true);
    w.install_with(&pe_with_options(X64, DOORSTOP_V4, b"4.0 ours"), IL2CPP_DECL);
    let theirs = w.theirs("Il2cpp");
    bepinex6_il2cpp_at(&theirs, "6.0.0-be.788+5b766a3");
    plugin_at(&theirs, "M/M.dll");
    w.choose(ChoiceKind::With, Some(&theirs));
    let before = snapshot(w.tmp.path());
    let e = w.play().unwrap_err();
    assert_eq!(
        (e.code, e.param("file")),
        (codes::SHARED_LOADER_CHANGED, Some("winhttp.dll"))
    );
    assert_eq!(snapshot(w.tmp.path()), before);
}

// ---- their paths on Steam's command line ----

#[test]
fn their_paths_with_spaces_and_cyrillic_are_one_argv_element_each() {
    for proton in [false, true] {
        let w = World::new(proton);
        w.install(IL2CPP_DECL);
        let theirs = w.theirs("Мой профиль модов");
        bepinex6_il2cpp_at(&theirs, "6.0.0-be.788+5b766a3");
        plugin_at(&theirs, "M/M.dll");
        write(&theirs.join("unstripped corlib/mscorlib.dll"), b"MZ");
        write(
            &theirs.join("загрузка/boot.config"),
            b"gfx-enable-gfx-jobs=1\n",
        );
        write(
            &theirs.join("doorstop_config.ini"),
            "[General]\nboot_config_override = загрузка\\boot.config\n[UnityMono]\ndll_search_path_override = unstripped corlib\n[Il2Cpp]\ncorlib_dir = dotnet\ncoreclr_path = dotnet\\coreclr.dll\n".as_bytes(),
        );
        w.choose(ChoiceKind::With, Some(&theirs));
        let p = w.play().unwrap();
        let after = |opt: &str| {
            let i = p.args.iter().position(|a| a == opt).unwrap();
            p.args[i + 1].clone()
        };
        assert_eq!(
            after("--doorstop-mono-dll-search-path-override"),
            w.their_text(&theirs.join("unstripped corlib"))
        );
        assert_eq!(
            after("--doorstop-clr-corlib-dir"),
            w.their_text(&theirs.join("dotnet"))
        );
        assert_eq!(
            after("--doorstop-clr-runtime-coreclr-path"),
            w.their_text(&theirs.join("dotnet/coreclr.dll"))
        );
        assert_eq!(
            after("--doorstop-boot-config-override"),
            w.their_text(&theirs.join("загрузка/boot.config"))
        );
        assert!(after("--doorstop-clr-corlib-dir").contains("Мой профиль модов"));
        // Every option is followed by exactly one value: no path was split, none quoted.
        let opts: Vec<&String> = p.args[2..].iter().step_by(2).collect();
        assert!(
            opts.iter().all(|o| o.starts_with("--doorstop-")),
            "{:?}",
            p.args
        );
        assert_eq!(p.args.len() % 2, 0);
        assert!(!p.args.iter().any(|a| a.contains('"')), "{:?}", p.args);
        // chain.ini carries their preloader's path whole too.
        assert!(w.chain_ini().unwrap().contains("Мой профиль модов"),);
    }
}

// ---- an answer that cannot be read ----

#[test]
fn an_answer_that_cannot_be_read_is_no_answer() {
    let w = World::new(false);
    w.install(MODS_DECL);
    let file = w.profile().join("mods-choice.json");
    // Nothing to ask about: Astra alone, saying the answer could not be read.
    fs::write(&file, b"{ not json").unwrap();
    let p = w.play().unwrap();
    assert_eq!(p.args, argv_0_3_2(&w, false));
    assert_eq!(p.warnings, [MODS_CHOICE_UNREADABLE]);
    // Mods to offer: asked again, with why.
    w.their_profile("Default", PACK_INI);
    for bad in [&b"{ not json"[..], br#"{"schema":2,"choice":"maybe"}"#] {
        fs::write(&file, bad).unwrap();
        let e = w.play().unwrap_err();
        assert_eq!(e.code, codes::MODS_CHOICE_NEEDED, "{e}");
        assert!(e.param("choice_unreadable").is_some(), "{e}");
    }
    // A folder in its place.
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert_eq!(w.play().unwrap_err().code, codes::MODS_CHOICE_NEEDED);
}

// ---- chain.ini: checked before anything is written, and taken away by uninstall ----

#[test]
fn a_chain_ini_that_cannot_be_written_refuses_before_anything_is_written() {
    let w = World::new(true);
    w.install(MODS_DECL);
    let theirs = w.their_profile("Default", PACK_INI);
    w.choose(ChoiceKind::With, Some(&theirs));
    // A folder where chain.ini goes.
    fs::create_dir_all(w.profile().join("chain/chain.ini")).unwrap();
    let before = snapshot(w.tmp.path());
    let e = w.play().unwrap_err();
    assert_eq!(e.code, codes::PATH_ESCAPES, "{e}");
    assert_eq!(
        snapshot(w.tmp.path()),
        before,
        "no config write, no user.reg"
    );
}

#[test]
fn uninstall_takes_away_what_play_with_mods_left() {
    let w = World::new(false);
    w.install(MODS_DECL);
    let theirs = w.their_profile("Default", PACK_INI);
    w.choose(ChoiceKind::With, Some(&theirs));
    w.play().unwrap();
    fs::write(
        w.profile().join("chain/last-run.txt"),
        "ok 2026-10-30T12:00:00Z 5.4.23.5\n",
    )
    .unwrap();
    astra_game_launcher::uninstall(&w.profile(), &w.target()).unwrap();
    assert!(
        !w.profile().join("chain").exists(),
        "chain.ini, last-run.txt and the folder"
    );
}

// ---- a Doorstop that does not know boot_config_override ----

#[test]
fn a_doorstop_without_boot_config_override_still_starts_the_chain() {
    let w = World::new(false);
    w.install(MODS_DECL);
    let ini = "[General]\nboot_config_override = theirs.boot.config\n";
    let theirs = w.their_profile("Default", ini);
    write(&theirs.join("theirs.boot.config"), b"x\n");
    w.choose(ChoiceKind::With, Some(&theirs));
    // Their manager relinked a Doorstop 4.0–4.2: it knows switching on and the target, and no
    // boot.config override (4.3 brought it), which it then ignores, file and option alike.
    let old = pe_with_options(X64, DOORSTOP_V4, b"4.2.0 theirs");
    write(&w.game().join("winhttp.dll"), &old);
    let p = w.play().unwrap();
    let i = p
        .args
        .iter()
        .position(|a| a == "--doorstop-boot-config-override")
        .unwrap();
    assert_eq!(
        p.args[i + 1],
        w.their_text(&theirs.join("theirs.boot.config"))
    );
    assert!(p.args.contains(&"--doorstop-target-assembly".to_owned()));
}

#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_their_doorstop_config_never_blocks_a_play() {
    let w = World::new(false);
    w.install(MODS_DECL);
    let theirs = w.their_profile("Default", PACK_INI);
    fs::remove_file(theirs.join("doorstop_config.ini")).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(theirs.join("doorstop_config.ini"))
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("no mkfifo here: skipped");
        return;
    }
    w.choose(ChoiceKind::With, Some(&theirs));
    let (tx, rx) = std::sync::mpsc::channel();
    let (profile, target, env) = (w.profile(), w.target(), w.env());
    std::thread::spawn(move || {
        let _ = tx.send(
            prepare_launch_with(
                &profile,
                &target,
                &vars(),
                Some(Path::new("/opt/fake/steam")),
                &env,
            )
            .map(|p| p.args),
        );
    });
    let args = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("Play opened a FIFO and waited for a writer")
        .unwrap();
    assert!(args.contains(&"--doorstop-target-assembly".to_owned()));
}

// ---- paths in their pack's ini: judged by their text before anything touches them ----

#[test]
fn a_pack_path_written_as_a_share_or_a_device_is_left_out_and_said() {
    let w = World::new(true);
    w.install(IL2CPP_DECL);
    let theirs = w.theirs("Default");
    bepinex6_il2cpp_at(&theirs, "6.0.0-be.788+5b766a3");
    plugin_at(&theirs, "M/M.dll");
    write(&theirs.join("unstripped corlib/mscorlib.dll"), b"MZ");
    let refused = [
        "//host/share/x",
        r"/\host\x",
        r"\\host\share\x",
        r"\\.\pipe\x",
        r"\\?\UNC\host\share\x",
        r"\\?\GLOBALROOT\x",
    ];
    let ini = format!(
        "[General]\nboot_config_override = \\\\host\\boot.config\n[UnityMono]\ndll_search_path_override = {};unstripped corlib;C:\\Corlib\n[Il2Cpp]\ncorlib_dir = \\\\host\\dotnet\ncoreclr_path = //./pipe/coreclr\n",
        refused.join(";")
    );
    write(&theirs.join("doorstop_config.ini"), ini.as_bytes());
    w.choose(ChoiceKind::With, Some(&theirs));
    let p = w.play().unwrap();
    assert_eq!(p.warnings, [MODS_PACK_PATH_REFUSED]);
    let after = |opt: &str| {
        let i = p.args.iter().position(|a| a == opt)?;
        Some(p.args[i + 1].clone())
    };
    // Only what is not a share or a device, the drive path as written.
    assert_eq!(
        after("--doorstop-mono-dll-search-path-override").unwrap(),
        format!(
            "{};C:\\Corlib",
            w.their_text(&theirs.join("unstripped corlib"))
        )
    );
    assert_eq!(after("--doorstop-boot-config-override"), None);
    // Their runtime's refused paths give way to their pack's own dotnet/.
    assert_eq!(
        after("--doorstop-clr-corlib-dir").unwrap(),
        w.their_text(&theirs.join("dotnet"))
    );
    assert_eq!(
        after("--doorstop-clr-runtime-coreclr-path").unwrap(),
        w.their_text(&theirs.join("dotnet/coreclr.dll"))
    );
    let all = format!("{:?}{}", p.args, w.chain_ini().unwrap());
    assert!(
        !all.contains("host") && !all.contains("pipe") && !all.contains("GLOBALROOT"),
        "{all}"
    );

    // Nothing refused: no warning.
    write(
        &theirs.join("doorstop_config.ini"),
        b"[UnityMono]\ndll_search_path_override = unstripped corlib\n",
    );
    assert!(w.play().unwrap().warnings.is_empty());
}
