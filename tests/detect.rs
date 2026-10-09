// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Engine detection on synthetic trees, and the anti-cheat markers.

mod common;

use std::path::PathBuf;

use astra_game_launcher::{Binary, Confidence, Engine, detect, detect_with};
use common::*;

#[test]
fn unity_mono_for_windows() {
    let t = Tmp::new("mono");
    unity_game(t.path(), "Lethal Company");
    write(&t.join("UnityCrashHandler64.exe"), b"MZ");
    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::UnityMono, Confidence::High, Binary::Windows)
    );
    assert_eq!(d.exe, Some(PathBuf::from("Lethal Company.exe")));
    assert!(d.anti_cheat.is_empty());
}

#[test]
fn unity_mono_native_linux_is_detected_as_linux() {
    let t = Tmp::new("mono-linux");
    write(&t.join("Game.x86_64"), b"\x7fELF");
    write(&t.join("UnityPlayer.so"), b"\x7fELF");
    write(&t.join("Game_Data/Managed/Assembly-CSharp.dll"), b"MZ");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.binary), (Engine::UnityMono, Binary::Linux));
    assert_eq!(d.exe, Some(PathBuf::from("Game.x86_64")));
}

#[test]
fn unity_il2cpp() {
    let t = Tmp::new("il2cpp");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("UnityPlayer.dll"), b"MZ");
    write(&t.join("GameAssembly.dll"), b"MZ");
    write(
        &t.join("Game_Data/il2cpp_data/Metadata/global-metadata.dat"),
        b"x",
    );
    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::UnityIl2cpp, Confidence::High, Binary::Windows)
    );
}

#[test]
fn unreal() {
    let t = Tmp::new("unreal");
    write(&t.join("Game.exe"), b"MZ launcher");
    write(
        &t.join("Game/Binaries/Win64/Game-Win64-Shipping.exe"),
        b"MZ",
    );
    write(
        &t.join("Game/Content/Paks/Game-WindowsNoEditor.pak"),
        b"pak",
    );
    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::Unreal, Confidence::High, Binary::Windows)
    );
    assert_eq!(
        d.exe,
        Some(PathBuf::from("Game/Binaries/Win64/Game-Win64-Shipping.exe"))
    );
}

#[test]
fn godot_with_a_pck_beside_the_exe() {
    let t = Tmp::new("godot");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("Game.pck"), b"GDPC");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.confidence), (Engine::Godot, Confidence::High));
    assert_eq!(d.exe, Some(PathBuf::from("Game.exe")));
}

#[test]
fn godot_with_an_embedded_pck_is_low_confidence() {
    let t = Tmp::new("godot-embedded");
    let mut exe = b"MZ program bytes".to_vec();
    exe.extend_from_slice(b"GDPC pack bytes");
    exe.extend_from_slice(&15u64.to_le_bytes());
    exe.extend_from_slice(b"GDPC");
    write(&t.join("Game.exe"), &exe);
    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::Godot, Confidence::Low, Binary::Windows)
    );
}

#[test]
fn something_else_is_unknown() {
    let t = Tmp::new("unknown");
    write(&t.join("hl2.exe"), b"MZ");
    write(&t.join("hl2/gameinfo.txt"), b"x");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.confidence), (Engine::Unknown, Confidence::Low));
    assert_eq!(d.exe, Some(PathBuf::from("hl2.exe")));
}

#[test]
fn anti_cheat_markers_are_found() {
    let cases: &[(&str, bool, &str)] = &[
        ("EasyAntiCheat", true, "easy-anti-cheat"),
        ("EasyAntiCheat_EOS_Setup.exe", false, "easy-anti-cheat"),
        ("start_protected_game.exe", false, "easy-anti-cheat"),
        ("BattlEye", true, "battleye"),
        ("BEService_x64.exe", false, "battleye"),
        ("GameGuard", true, "gameguard"),
        ("XIGNCODE", true, "xigncode"),
        ("mhypbase.dll", false, "hoyo-protect"),
        ("HoYoKProtect.sys", false, "hoyo-protect"),
    ];
    for (name, is_dir, code) in cases {
        let t = Tmp::new("ac");
        unity_game(t.path(), "Game");
        if *is_dir {
            write(&t.join(&format!("{name}/readme.txt")), b"x");
        } else {
            write(&t.join(name), b"MZ");
        }
        let d = detect(t.path()).unwrap();
        assert_eq!(d.anti_cheat, vec![code.to_string()], "{name}");
    }
}

#[test]
fn the_walk_does_not_follow_links() {
    #[cfg(unix)]
    {
        let outside = Tmp::new("outside");
        write(&outside.join("EasyAntiCheat/x.txt"), b"x");
        let t = Tmp::new("links");
        unity_game(t.path(), "Game");
        std::os::unix::fs::symlink(outside.path(), t.join("linked")).unwrap();
        let d = detect(t.path()).unwrap();
        assert!(d.anti_cheat.is_empty(), "a link is never followed");
    }
}

/// MiSide's layout: an IL2CPP game at the root (`GameAssembly.dll` beside `UnityPlayer.dll`, no
/// `il2cpp_data`) and a Mono tool in a subfolder. The ROOT game is the one detected, as IL2CPP.
#[test]
fn a_root_game_wins_over_a_mono_tool_in_a_subfolder() {
    let t = Tmp::new("miside");
    write(&t.join("MiSideFull.exe"), b"MZ game");
    write(&t.join("UnityPlayer.dll"), b"MZ player");
    write(&t.join("GameAssembly.dll"), b"MZ ga");
    write(&t.join("MiSideFull_Data/globalgamemanagers"), b"x");
    unity_game(&t.join("Voice Editor"), "Miside Voice Editor");
    let d = detect(t.path()).unwrap();
    assert_eq!(d.engine, Engine::UnityIl2cpp);
    assert_eq!(d.exe, Some(PathBuf::from("MiSideFull.exe")));
    assert_eq!(d.confidence, Confidence::High);
}

/// With nothing Unity at the root, a game one level down is still found.
#[test]
fn a_game_one_level_down_is_found_when_the_root_has_none() {
    let t = Tmp::new("nested");
    unity_game(&t.join("Game"), "Inner");
    let d = detect(t.path()).unwrap();
    assert_eq!(d.engine, Engine::UnityMono);
    assert_eq!(d.exe, Some(PathBuf::from("Game/Inner.exe")));
}

// ---- The rules: a program is the game by what sits beside it. ----

fn p(s: &str) -> Option<PathBuf> {
    Some(PathBuf::from(s))
}

/// MiSide's real layout, in full: the IL2CPP game at the root (`GameAssembly.dll` beside
/// `UnityPlayer.dll`) and a Mono tool in `Voice Editor/` whose `_Data` is LARGER, so it is the
/// depth that decides and not the size. No rule here names MiSide.
#[test]
fn the_miside_layout_is_il2cpp_at_the_root_by_the_general_rule() {
    let t = Tmp::new("miside-full");
    write(&t.join("MiSideFull.exe"), b"MZ game");
    write(&t.join("UnityPlayer.dll"), b"MZ player");
    write(&t.join("GameAssembly.dll"), b"MZ ga");
    write(&t.join("baselib.dll"), b"MZ");
    write(&t.join("UnityCrashHandler64.exe"), b"MZ");
    write(&t.join("MiSideFull_Data/globalgamemanagers"), b"x");
    write(
        &t.join("MiSideFull_Data/il2cpp_data/Metadata/global-metadata.dat"),
        b"x",
    );
    write(&t.join("Data/Voices/readme.txt"), b"x");
    unity_game(&t.join("Voice Editor"), "Miside Voice Editor");
    write(
        &t.join("Voice Editor/Miside Voice Editor_Data/sharedassets0.assets"),
        &[0u8; 64 * 1024],
    );

    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::UnityIl2cpp, Confidence::High, Binary::Windows)
    );
    assert_eq!(d.exe, p("MiSideFull.exe"));
    assert_eq!(d.evidence, p("MiSideFull_Data"));
    assert!(!d.hint_used);

    // What Steam launches for MiSide: the same answer, and the hint is said to be used.
    let d = detect_with(t.path(), Some("MiSideFull.exe")).unwrap();
    assert_eq!(
        (d.engine, d.exe, d.evidence, d.hint_used),
        (
            Engine::UnityIl2cpp,
            p("MiSideFull.exe"),
            p("MiSideFull_Data"),
            true
        )
    );
}

/// Two Unity programs at the root share one `UnityPlayer.dll`: the larger `_Data` wins, and it is
/// the size, not the name, that decides.
#[test]
fn of_two_root_unity_programs_the_larger_data_wins() {
    for (big, small) in [("B", "A"), ("A", "B")] {
        let t = Tmp::new("two-root");
        write(&t.join("UnityPlayer.dll"), b"MZ player");
        for name in [big, small] {
            write(&t.join(&format!("{name}.exe")), b"MZ");
            write(
                &t.join(&format!("{name}_Data/Managed/Assembly-CSharp.dll")),
                b"MZ",
            );
        }
        write(
            &t.join(&format!("{big}_Data/sharedassets0.assets")),
            &[0u8; 32 * 1024],
        );
        let d = detect(t.path()).unwrap();
        assert_eq!(d.exe, p(&format!("{big}.exe")), "{big} is larger");
        assert_eq!(d.evidence, p(&format!("{big}_Data")));
        assert_eq!(
            (d.engine, d.confidence),
            (Engine::UnityMono, Confidence::High)
        );
    }
}

/// A hint that passes the rules wins outright: over a larger `_Data`, and over a shallower game.
#[test]
fn a_hint_that_passes_wins_outright() {
    let t = Tmp::new("hint-size");
    write(&t.join("UnityPlayer.dll"), b"MZ player");
    for name in ["A", "B"] {
        write(&t.join(&format!("{name}.exe")), b"MZ");
        write(
            &t.join(&format!("{name}_Data/Managed/Assembly-CSharp.dll")),
            b"MZ",
        );
    }
    write(&t.join("B_Data/sharedassets0.assets"), &[0u8; 32 * 1024]);
    assert_eq!(detect(t.path()).unwrap().exe, p("B.exe"));
    let d = detect_with(t.path(), Some("A.exe")).unwrap();
    assert_eq!((d.exe, d.hint_used), (p("A.exe"), true));

    let t = Tmp::new("hint-depth");
    unity_game(t.path(), "Root");
    unity_game(&t.join("Sub"), "Tool");
    assert_eq!(detect(t.path()).unwrap().exe, p("Root.exe"));
    let d = detect_with(t.path(), Some("Sub/Tool.exe")).unwrap();
    assert_eq!(
        (d.exe, d.evidence, d.hint_used),
        (p("Sub/Tool.exe"), p("Sub/Tool_Data"), true)
    );
    // As Steam may write it: Windows separators, a leading `./`, another case. The answer is
    // spelled as on disk.
    let d = detect_with(t.path(), Some(r".\SUB\tool.EXE")).unwrap();
    assert_eq!((d.exe, d.hint_used), (p("Sub/Tool.exe"), true));
}

/// A hint that is not a game by the rules, or not inside the folder, is ignored: the folder is
/// detected as if there were none, and `hint_used` says so.
#[test]
fn a_hint_that_does_not_pass_is_ignored() {
    let outside = Tmp::new("hint-outside");
    unity_game(outside.path(), "Elsewhere");
    let t = Tmp::new("hint-bad");
    unity_game(t.path(), "Game");
    write(&t.join("Launcher.exe"), b"MZ launcher");
    let outside_name = outside.path().file_name().unwrap().to_str().unwrap();
    let escape = format!("../{outside_name}/Elsewhere.exe");
    let absolute = outside.join("Elsewhere.exe").display().to_string();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(t.join("Game.exe"), t.join("Linked.exe")).unwrap();
        std::os::unix::fs::symlink(outside.path(), t.join("LinkedDir")).unwrap();
    }
    let bad = [
        "Launcher.exe",            // exists, but nothing beside it makes it a game
        "Missing.exe",             // not there
        "Game_Data",               // a folder
        "",                        // nothing
        &escape,                   // outside the folder, by `..`
        &absolute,                 // outside the folder, absolute
        "Sub/../Game.exe",         // `..` even when it would land inside
        "Linked.exe",              // a link to the game: never followed
        "LinkedDir/Elsewhere.exe", // a real game, through a linked folder: never followed
    ];
    for hint in bad {
        let d = detect_with(t.path(), Some(hint)).unwrap();
        assert_eq!(d.exe, p("Game.exe"), "{hint:?}");
        assert_eq!(d.evidence, p("Game_Data"), "{hint:?}");
        assert!(!d.hint_used, "{hint:?} is ignored");
    }
}

/// Unreal: the shipping program and `Content/Paks` in ONE project folder. The bootstrap at the
/// top (what Steam launches) does not pass and is ignored; a project without paks loses.
#[test]
fn unreal_pairs_binaries_with_content_paks_in_one_project() {
    let t = Tmp::new("unreal-pair");
    write(&t.join("Game.exe"), b"MZ bootstrap");
    write(
        &t.join("Engine/Binaries/Win64/CrashReportClient.exe"),
        b"MZ",
    );
    write(
        &t.join("Game/Binaries/Win64/Game-Win64-Shipping.exe"),
        b"MZ",
    );
    write(&t.join("Game/Content/Paks/Game-Windows.pak"), b"pak");
    write(
        &t.join("Editor/Binaries/Win64/Editor-Win64-Shipping.exe"),
        b"MZ",
    );
    let shipping = p("Game/Binaries/Win64/Game-Win64-Shipping.exe");

    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::Unreal, Confidence::High, Binary::Windows)
    );
    assert_eq!((d.exe.clone(), d.evidence), (shipping.clone(), p("Game")));

    let d = detect_with(t.path(), Some("Game.exe")).unwrap();
    assert_eq!((d.exe.clone(), d.hint_used), (shipping.clone(), false));

    let d = detect_with(
        t.path(),
        Some(r"Game\Binaries\Win64\Game-Win64-Shipping.exe"),
    )
    .unwrap();
    assert_eq!((d.exe, d.hint_used), (shipping, true));
}

/// The pairing is the folder, not the name: Palworld ships
/// `Pal/Binaries/Win64/Palworld-Win64-Shipping.exe` beside `Pal/Content/Paks`.
#[test]
fn an_unreal_project_folder_need_not_repeat_the_program_name() {
    let t = Tmp::new("unreal-pal");
    write(
        &t.join("Pal/Binaries/Win64/Palworld-Win64-Shipping.exe"),
        b"MZ",
    );
    write(&t.join("Pal/Content/Paks/Pal-Windows.pak"), b"pak");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.confidence), (Engine::Unreal, Confidence::High));
    assert_eq!(d.evidence, p("Pal"));
}

/// Half a pairing is still Unreal, at Low confidence.
#[test]
fn unreal_without_content_paks_is_low() {
    let t = Tmp::new("unreal-half");
    write(
        &t.join("Game/Binaries/Win64/Game-Win64-Shipping.exe"),
        b"MZ",
    );
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.confidence), (Engine::Unreal, Confidence::Low));
    assert_eq!(d.exe, p("Game/Binaries/Win64/Game-Win64-Shipping.exe"));
    assert_eq!(d.evidence, p("Game"));
    // And it does not pass rule 5, so as a hint it is ignored.
    let d = detect_with(
        t.path(),
        Some("Game/Binaries/Win64/Game-Win64-Shipping.exe"),
    )
    .unwrap();
    assert!(!d.hint_used);
}

/// Godot: the program with `<stem>.pck` beside it, Windows or Linux.
#[test]
fn godot_pairs_a_program_with_its_pck() {
    let t = Tmp::new("godot-pair");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("Game.pck"), b"GDPC");
    write(&t.join("crash_handler.exe"), b"MZ");
    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence, d.binary),
        (Engine::Godot, Confidence::High, Binary::Windows)
    );
    assert_eq!((d.exe, d.evidence), (p("Game.exe"), p("Game.pck")));
    assert!(detect_with(t.path(), Some("Game.exe")).unwrap().hint_used);
    assert!(
        !detect_with(t.path(), Some("crash_handler.exe"))
            .unwrap()
            .hint_used
    );

    let t = Tmp::new("godot-linux");
    write(&t.join("Game.x86_64"), b"\x7fELF");
    write(&t.join("Game.pck"), b"GDPC");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.binary), (Engine::Godot, Binary::Linux));
    assert_eq!((d.exe, d.evidence), (p("Game.x86_64"), p("Game.pck")));
}

/// A `.pck` that repeats no program's name is not the pairing: Wwise sound banks are `.pck` too.
#[test]
fn a_pck_named_after_no_program_is_not_godot() {
    let t = Tmp::new("pck-other");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("Audio.pck"), b"AKPK");
    let d = detect(t.path()).unwrap();
    assert_eq!(d.engine, Engine::Unknown);
    assert_eq!(d.exe, p("Game.exe"));
    assert_eq!(d.evidence, None);
}

/// Before Unity 2017.2 the player was linked into the program: no `UnityPlayer`, but
/// `<stem>_Data/Managed/Assembly-CSharp.dll`. Mono at Low, and still the game over a newer Unity
/// tool in a subfolder.
#[test]
fn a_unity_game_from_before_2017_is_mono_at_low_confidence() {
    let t = Tmp::new("legacy");
    write(&t.join("Game.exe"), b"MZ game and player");
    write(&t.join("Game_Data/Managed/Assembly-CSharp.dll"), b"MZ");
    unity_game(&t.join("Tools"), "Tool");
    let d = detect(t.path()).unwrap();
    assert_eq!(
        (d.engine, d.confidence),
        (Engine::UnityMono, Confidence::Low)
    );
    assert_eq!((d.exe, d.evidence), (p("Game.exe"), p("Game_Data")));
    // Not rule 2, so as a hint it is not "used".
    assert!(!detect_with(t.path(), Some("Game.exe")).unwrap().hint_used);
}

/// A Unity player and its `_Data` with neither flavour's proof: still THE program, flavour
/// unknown, so nothing engine-specific is installed into it.
#[test]
fn a_unity_game_without_a_flavour_is_named_but_unknown() {
    let t = Tmp::new("no-flavour");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("UnityPlayer.dll"), b"MZ");
    write(&t.join("Game_Data/globalgamemanagers"), b"x");
    unity_game(&t.join("Tools"), "Tool");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.engine, d.confidence), (Engine::Unknown, Confidence::Low));
    assert_eq!((d.exe, d.evidence), (p("Game.exe"), p("Game_Data")));
}

/// A detection saved before `evidence` and `hint_used` existed still reads.
#[test]
fn an_older_detection_still_deserialises() {
    let old = r#"{"engine":"unity-mono","confidence":"high","exe":"Game.exe","binary":"windows","anti_cheat":[]}"#;
    let d: astra_game_launcher::Detection = serde_json::from_str(old).unwrap();
    assert_eq!((d.evidence, d.hint_used), (None, false));
}

/// At the same depth, a program that passes beats a pre-2017.2 Unity game, even a larger one.
#[test]
fn at_one_depth_a_program_that_passes_beats_a_legacy_one() {
    let t = Tmp::new("pass-vs-legacy");
    write(&t.join("A/Old.exe"), b"MZ");
    write(&t.join("A/Old_Data/Managed/Assembly-CSharp.dll"), b"MZ");
    write(
        &t.join("A/Old_Data/sharedassets0.assets"),
        &[0u8; 32 * 1024],
    );
    unity_game(&t.join("B"), "New");
    let d = detect(t.path()).unwrap();
    assert_eq!((d.exe, d.confidence), (p("B/New.exe"), Confidence::High));
}
