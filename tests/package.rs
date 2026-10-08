// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The zip rules, the manifest keys and the placeholders.

mod common;

use astra_game_launcher::{GiPackage, Limits, Manifest, codes};
use common::*;

fn refusal(zip: &[u8]) -> astra_game_launcher::LauncherError {
    open(zip).unwrap_err()
}

#[test]
fn the_runner_shape_opens_and_names_its_files() {
    let pkg = runner();
    let s = pkg.summary();
    assert_eq!(s.integration.id, "test.unity");
    assert_eq!(
        s.profile_files, 2,
        "only BepInEx/core/** goes into the profile; licenses/ is ignored"
    );
    assert_eq!(s.game_files.len(), 2);
    assert!(s.game_files[0].pinned && !s.game_files[1].pinned);
    assert_eq!(s.variables, vec!["bridge_port", "bridge_token"]);
    assert_eq!(
        s.proton_override,
        Some(("winhttp".into(), "native,builtin".into()))
    );
}

#[test]
fn the_digest_is_checked_before_anything_else() {
    let z = runner_zip(&runner_manifest(""));
    let e = GiPackage::open(&z, &"0".repeat(64)).unwrap_err();
    assert_eq!(e.code, codes::DIGEST_MISMATCH);
    assert_eq!(
        GiPackage::open(&z, "not-hex").unwrap_err().code,
        codes::DIGEST_MISMATCH
    );
    // Upper case is the same digest.
    assert!(GiPackage::open(&z, &sha(&z).to_uppercase()).is_ok());
}

#[test]
fn a_parent_entry_is_refused() {
    let m = runner_manifest("");
    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("../evil.dll", b"x"),
    ]);
    let e = refusal(&z);
    assert_eq!(e.code, codes::ZIP_UNSAFE_ENTRY);
    assert_eq!(e.param("reason"), Some("parent"));

    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("BepInEx/core/../../evil.dll", b"x"),
    ]);
    assert_eq!(refusal(&z).code, codes::ZIP_UNSAFE_ENTRY);

    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("BepInEx\\..\\..\\evil.dll", b"x"),
    ]);
    assert_eq!(
        refusal(&z).code,
        codes::ZIP_UNSAFE_ENTRY,
        "backslashes are separators too"
    );
}

#[test]
fn an_absolute_entry_is_refused() {
    let m = runner_manifest("");
    for name in ["/etc/evil", "C:/Windows/evil.dll", "\\\\server\\share\\x"] {
        let z = zip(&[E::File("astra-gi.toml", m.as_bytes()), E::File(name, b"x")]);
        assert_eq!(refusal(&z).code, codes::ZIP_UNSAFE_ENTRY, "{name}");
    }
}

#[test]
fn a_symlink_entry_is_refused_even_when_nothing_names_it() {
    let m = runner_manifest("");
    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
        E::Symlink("BepInEx/core/link", "/etc/passwd"),
    ]);
    let e = refusal(&z);
    assert_eq!(e.code, codes::ZIP_UNSAFE_ENTRY);
    assert_eq!(e.param("reason"), Some("symlink"));
}

#[test]
fn the_caps_hold() {
    let z = runner_zip(&runner_manifest(""));
    let digest = sha(&z);
    let small = |f: fn(&mut Limits)| {
        let mut l = Limits::default();
        f(&mut l);
        GiPackage::open_with_limits(&z, &digest, &l).unwrap_err()
    };
    assert_eq!(small(|l| l.max_zip_bytes = 100).code, codes::TOO_LARGE);
    assert_eq!(
        small(|l| l.max_uncompressed_bytes = 100).code,
        codes::TOO_LARGE
    );
    assert_eq!(
        small(|l| l.max_entries = 3).code,
        codes::ZIP_TOO_MANY_ENTRIES
    );
    assert_eq!(small(|l| l.max_manifest_bytes = 10).code, codes::TOO_LARGE);
    assert_eq!(Limits::default().max_zip_bytes, 256 * 1024 * 1024);
    assert_eq!(Limits::default().max_uncompressed_bytes, 1024 * 1024 * 1024);
    assert_eq!(Limits::default().max_entries, 20_000);
}

#[test]
fn twenty_thousand_and_one_entries_are_refused_at_the_real_cap() {
    let names: Vec<String> = (0..20_001).map(|i| format!("f/{i}")).collect();
    let mut entries: Vec<E> = vec![E::File("astra-gi.toml", b"schema = 1")];
    entries.extend(names.iter().map(|n| E::File(n.as_str(), b"")));
    let z = zip(&entries);
    assert_eq!(refusal(&z).code, codes::ZIP_TOO_MANY_ENTRIES);
}

#[test]
fn a_zip_without_a_manifest_or_not_a_zip() {
    let z = zip(&[E::File("readme.txt", b"hi")]);
    assert_eq!(refusal(&z).code, codes::MANIFEST_MISSING);
    let z = zip(&[E::File("sub/astra-gi.toml", runner_manifest("").as_bytes())]);
    assert_eq!(
        refusal(&z).code,
        codes::MANIFEST_MISSING,
        "only at the root"
    );
    assert_eq!(refusal(b"not a zip at all").code, codes::ZIP_UNSAFE_ENTRY);
}

#[test]
fn only_named_files_are_extracted_and_a_named_file_must_exist() {
    let m = runner_manifest("");
    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]);
    let e = refusal(&z);
    assert_eq!(e.code, codes::MANIFEST_INVALID);
    assert_eq!(e.param("key"), Some("game_files[1].from"));
}

#[test]
fn a_pinned_game_file_with_other_bytes_is_refused() {
    let m = runner_manifest("");
    let z = zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", b"MZ a different proxy"),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]);
    let e = refusal(&z);
    assert_eq!(e.code, codes::DIGEST_MISMATCH);
    assert_eq!(e.param("file"), Some("winhttp.dll"));
}

fn parse_err(text: &str) -> astra_game_launcher::LauncherError {
    Manifest::parse(text).unwrap_err()
}

#[test]
fn unknown_keys_in_acting_sections_are_refused() {
    let base = runner_manifest("");
    let cases = [
        (
            "[target]\n",
            "[target]\nauto_update = true\n",
            "target.auto_update",
        ),
        (
            "[[files]]\n",
            "[[files]]\nmode = \"0755\"\n",
            "files[0].mode",
        ),
        (
            "[[game_files]]\nfrom = \"winhttp.dll\"",
            "[[game_files]]\nrun = true\nfrom = \"winhttp.dll\"",
            "game_files[0].run",
        ),
        (
            "[launch]\n",
            "[launch]\nshell = \"sh -c x\"\n",
            "launch.shell",
        ),
        (
            "[[config_writes]]\n",
            "[[config_writes]]\nencoding = \"utf16\"\n",
            "config_writes[0].encoding",
        ),
    ];
    for (find, replace, key) in cases {
        assert!(base.contains(find), "{find}");
        let text = base.replacen(find, replace, 1);
        let e = parse_err(&text);
        assert_eq!(e.code, codes::UNKNOWN_KEY, "{key}");
        assert_eq!(e.param("key"), Some(key));
    }
    // A new top-level section is a feature too.
    let e = parse_err(&runner_manifest(
        "\n[[run_at_install]]\nexe = \"setup.exe\"\n",
    ));
    assert_eq!(
        (e.code, e.param("key")),
        (codes::UNKNOWN_KEY, Some("run_at_install"))
    );
    // Metadata may grow freely.
    assert!(
        Manifest::parse(&base).is_ok(),
        "an unknown key in [integration] is ignored"
    );
}

#[test]
fn a_newer_schema_is_refused_by_its_own_code() {
    let text =
        runner_manifest("").replacen("schema = 1", "schema = 2", 1) + "\n[something_new]\nx = 1\n";
    assert_eq!(parse_err(&text).code, codes::SCHEMA_TOO_NEW);
}

#[test]
fn placeholders_are_checked() {
    let base = runner_manifest("");
    // A variable may not choose a path.
    let text = base.replacen(
        "to = \"${profile}/BepInEx/core/\"",
        "to = \"${profile}/${bridge_port}/\"",
        1,
    );
    let e = parse_err(&text);
    assert_eq!(
        (e.code, e.param("name")),
        (codes::UNKNOWN_PLACEHOLDER, Some("bridge_port"))
    );
    // Neither may an unknown root.
    let text = base.replacen(
        "to = \"${profile}/BepInEx/core/\"",
        "to = \"${home}/x/\"",
        1,
    );
    assert_eq!(parse_err(&text).param("name"), Some("home"));
    // A malformed or unterminated placeholder in an argument.
    for bad in ["${bad-name}", "${unterminated", "${}"] {
        let text = base.replacen("\"--doorstop-enabled\"", &format!("\"{bad}\""), 1);
        assert_eq!(parse_err(&text).code, codes::UNKNOWN_PLACEHOLDER, "{bad}");
    }
    // Variables in arguments are the caller's, checked at launch.
    let text = base.replacen("\"--doorstop-enabled\"", "\"--port=${bridge_port}\"", 1);
    assert!(Manifest::parse(&text).is_ok());
}

#[test]
fn the_path_jail_is_lexical_first() {
    let base = runner_manifest("");
    let cases = [
        (
            "to = \"${profile}/BepInEx/core/\"",
            "to = \"${profile}/../outside/\"",
        ),
        ("to = \"${profile}/BepInEx/core/\"", "to = \"/etc/\""),
        (
            "to = \"${profile}/BepInEx/core/\"",
            "to = \"${game}/BepInEx/core/\"",
        ),
        (
            "to = \"${game}/winhttp.dll\"",
            "to = \"${profile}/winhttp.dll\"",
        ),
        (
            "to = \"${game}/winhttp.dll\"",
            "to = \"${game}/../winhttp.dll\"",
        ),
        (
            "file = \"${profile}/BepInEx/config/astra.cfg\"",
            "file = \"${game}/astra.cfg\"",
        ),
        (
            "file = \"${profile}/BepInEx/config/astra.cfg\"",
            "file = \"${profile}/astra-launcher-ledger.json\"",
        ),
    ];
    for (find, replace) in cases {
        let text = base.replacen(find, replace, 1);
        assert_eq!(parse_err(&text).code, codes::PATH_ESCAPES, "{replace}");
    }
}

#[test]
fn the_manifest_s_anti_cheat_and_overrides_are_checked() {
    let base = runner_manifest("");
    let text = base.replacen(
        "winhttp = \"native,builtin\"",
        "winhttp = \"native\\n[x]\"",
        1,
    );
    assert_eq!(parse_err(&text).code, codes::MANIFEST_INVALID);
    let text = base.replacen(
        "{ winhttp = \"native,builtin\" }",
        "{ winhttp = \"n,b\", version = \"n,b\" }",
        1,
    );
    assert_eq!(
        parse_err(&text).code,
        codes::MANIFEST_INVALID,
        "schema 1: one override"
    );
    let text = base.replacen("engine = \"unity-mono\"", "engine = \"source2\"", 1);
    assert_eq!(parse_err(&text).code, codes::MANIFEST_INVALID);
}

#[test]
fn a_wildcard_cannot_land_on_the_profile_s_bookkeeping() {
    for name in [
        "astra-launcher-ledger.json",
        "Astra-Launcher-Ledger.lock",
        "astra-launcher-backup/game/winhttp.dll",
    ] {
        let m = runner_manifest("\n[[files]]\nfrom = \"**\"\nto = \"${profile}/\"\n");
        let z = zip(&[
            E::File("astra-gi.toml", m.as_bytes()),
            E::File("winhttp.dll", WINHTTP),
            E::File("doorstop_config.ini", DOORSTOP_INI),
            E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
            E::File(name, b"{\"forged\": true}"),
        ]);
        let e = refusal(&z);
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PATH_ESCAPES, Some("reserved")),
            "{name}"
        );
    }
}
