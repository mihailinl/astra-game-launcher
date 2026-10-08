// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Engine detection on synthetic trees, and the anti-cheat markers.

mod common;

use std::path::PathBuf;

use astra_game_launcher::{Binary, Confidence, Engine, detect};
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
