// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Engine descriptors (`astra.engine/1`): parsing, admission into an [`EngineSet`], and Pass B —
//! with one hostile fixture per row of the safety table that applies to detection (plan B §3):
//! taking a recognised game, griefing unknown ones without an anchor, budget exhaustion, links
//! and FIFOs, an unknown field or schema, reserved codes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use astra_game_launcher::{
    Binary, Confidence, Descriptor, Detection, EngineCode, EngineSet, ExePick, LauncherError,
    Moderation, Origin, codes, detect_with, detect_with_engines, is_reserved_code,
};
use common::identity::{checked, hostile_set, narrowed};
use common::*;

// ---- helpers ----

fn origin(moderation: Moderation, publisher: &str, first_party: bool) -> Origin {
    Origin {
        moderation,
        publisher: publisher.to_owned(),
        first_party,
    }
}

fn set_of(items: &[(Moderation, &str, bool, &str)]) -> EngineSet {
    EngineSet::build(
        items
            .iter()
            .map(|(m, p, fp, text)| (origin(*m, p, *fp), Descriptor::from_toml(text))),
    )
}

/// One community descriptor, unchecked.
fn one(text: &str) -> EngineSet {
    set_of(&[(Moderation::Unchecked, "community", false, text)])
}

/// The refusal admitting `text` alone meets.
fn refusal(text: &str, first_party: bool) -> LauncherError {
    let set = set_of(&[(Moderation::Unchecked, "community", first_party, text)]);
    assert!(set.is_empty(), "expected a refusal, admitted: {text}");
    set.skipped()[0].error.clone()
}

fn parse_err(text: &str) -> LauncherError {
    Descriptor::from_toml(text).expect_err(text)
}

fn p(s: &str) -> Option<PathBuf> {
    Some(PathBuf::from(s))
}

fn d(dir: &Path, hint: Option<&str>, set: &EngineSet) -> Detection {
    // Pass A first: whatever Pass B says, Pass A's own answer is still 0.4.1's.
    let pass_a = checked(dir, hint);
    let with = detect_with_engines(dir, hint, set).unwrap();
    narrowed(&pass_a, &with);
    with
}

const SOURCE2: &str = r#"
schema = "astra.engine/1"
engine = { codes = { source2 = "Source 2" } }

[[rule]]
tier = "strong"
programs = { ext = ["exe"], at = "{project}/bin/win64", first = 8 }
require = [ { listed = "engine2.dll", kind = "file" },
            { child = "{project}/core", kind = "dir", as = "core" } ]
code = "source2"
confidence = "high"
binary = "windows"
evidence = "{core}"
"#;

/// A Source 2 shaped folder Pass A knows nothing about.
fn source2_game(t: &Tmp) {
    write(&t.join("game/bin/win64/cs2.exe"), b"MZ\x90\0 a program");
    write(&t.join("game/bin/win64/engine2.dll"), b"MZ engine");
    write(&t.join("game/core/pak01_dir.vpk"), b"vpk");
    write(&t.join("game/csgo/pak01_dir.vpk"), b"vpk");
}

// ---- parsing ----

#[test]
fn the_plans_source2_example_parses_and_names_its_game() {
    let set = one(SOURCE2);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let t = Tmp::new("source2");
    source2_game(&t);
    let got = d(t.path(), None, &set);
    assert_eq!(got.engine, "source2");
    assert_eq!(
        (got.confidence, got.binary, got.hint_used),
        (Confidence::High, Binary::Windows, false)
    );
    assert_eq!(got.exe, p("game/bin/win64/cs2.exe"));
    assert_eq!(got.evidence, p("game/core"));
    let (desc, _) = set.descriptors().next().unwrap();
    assert_eq!(got.descriptor_sha256.as_deref(), Some(desc.digest()));
    assert_eq!(
        set.engine_name(&EngineCode::new("source2").unwrap()),
        Some("Source 2")
    );

    // The same game when Steam names the program: the hint is used, and said so.
    let got = d(t.path(), Some(r"game\bin\win64\CS2.EXE"), &set);
    assert_eq!(
        (got.engine.as_str(), got.hint_used, got.exe.clone()),
        ("source2", true, p("game/bin/win64/cs2.exe"))
    );
    // And without descriptors it is unknown, exactly as 0.4.1 says.
    assert!(detect_with(t.path(), None).unwrap().engine.is_unknown());
}

#[test]
fn toml_and_json_spell_one_descriptor_with_one_digest() {
    let toml = Descriptor::from_toml(SOURCE2).unwrap();
    let json = r#"{"engine":{"codes":{"source2":"Source 2"}},"rule":[{"tier":"strong","programs":{"first":8,"at":"{project}/bin/win64","ext":["exe"]},"require":[{"kind":"file","listed":"engine2.dll"},{"child":"{project}/core","kind":"dir","as":"core"}],"code":"source2","confidence":"high","binary":"windows","evidence":"{core}"}],"schema":"astra.engine/1"}"#;
    let from_json = Descriptor::from_json(json.as_bytes()).unwrap();
    assert_eq!(toml.digest(), from_json.digest());
    // JCS: keys sorted, no whitespace.
    assert!(toml.canonical_json().starts_with(
        r#"{"engine":{"codes":{"source2":"Source 2"}},"rule":[{"binary":"windows","code":"source2""#
    ));
    assert_eq!(
        toml.digest(),
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
            toml.canonical_json().as_bytes()
        ))
    );
    // One changed byte, another digest.
    let other = Descriptor::from_toml(&SOURCE2.replace("Source 2", "Source Two")).unwrap();
    assert_ne!(toml.digest(), other.digest());
}

#[test]
fn jcs_escapes_only_what_it_must() {
    // A tail's `equals` may hold any bytes: a control, a quote, a backslash, and U+E000.
    let text = SOURCE2.replace(
        "{ listed = \"engine2.dll\", kind = \"file\" },",
        "{ listed = \"engine2.dll\", kind = \"file\" }, { tail = \"{program}\", bytes = 6, equals = \"\\u0001\\\"\\\\\u{e000}\" },",
    );
    let d = Descriptor::from_toml(&text).unwrap();
    assert!(
        d.canonical_json()
            .contains("\"equals\":\"\\u0001\\\"\\\\\u{e000}\""),
        "{}",
        d.canonical_json()
    );
}

#[test]
fn an_unknown_key_anywhere_refuses_the_descriptor() {
    let cases = [
        SOURCE2.replace("[[rule]]", "colour = \"red\"\n[[rule]]"),
        SOURCE2.replace("engine = { codes", "engine = { owner = \"x\", codes"),
        SOURCE2.replace("tier = \"strong\"", "tier = \"strong\"\noverride = true"),
        SOURCE2.replace("first = 8 }", "first = 8, follow_links = true }"),
        SOURCE2.replace("kind = \"file\" }", "kind = \"file\", regex = \".*\" }"),
        SOURCE2.replace("\"strong\"", "\"strongest\""),
    ];
    for c in cases {
        assert_eq!(parse_err(&c).code, codes::DESCRIPTOR_INVALID, "{c}");
    }
}

#[test]
fn an_unknown_schema_is_skipped_whole_before_any_key_is_read() {
    let next = "schema = \"astra.engine/2\"\nroots = [\"/\"]\nwhatever = { new = true }\n";
    let e = parse_err(next);
    assert_eq!(e.code, codes::DESCRIPTOR_SCHEMA_UNKNOWN);
    assert_eq!(e.param("schema"), Some("astra.engine/2"));
    let e = Descriptor::from_json(br#"{"schema":"astra.engine/2","x":1.5}"#).unwrap_err();
    // Floats are refused by the JSON reader itself, before the schema is looked at.
    assert_eq!(e.code, codes::DESCRIPTOR_INVALID);
    let e = Descriptor::from_json(br#"{"schema":"astra.engine/2","x":1}"#).unwrap_err();
    assert_eq!(e.code, codes::DESCRIPTOR_SCHEMA_UNKNOWN);
    assert_eq!(parse_err("engine = 1").code, codes::DESCRIPTOR_INVALID);
}

/// "An unknown field or schema skips only that descriptor": the others are admitted.
#[test]
fn a_descriptor_that_does_not_parse_costs_only_itself() {
    let set = set_of(&[
        (
            Moderation::Moderated,
            "a",
            false,
            "schema = \"astra.engine/9\"\n",
        ),
        (
            Moderation::Unchecked,
            "b",
            false,
            &SOURCE2.replace("[[rule]]", "surprise = 1\n[[rule]]"),
        ),
        (Moderation::Unchecked, "c", false, SOURCE2),
    ]);
    assert_eq!(set.len(), 1);
    let skipped: Vec<(usize, &str)> = set
        .skipped()
        .iter()
        .map(|s| (s.index, s.error.code))
        .collect();
    assert_eq!(
        skipped,
        [
            (0, codes::DESCRIPTOR_SCHEMA_UNKNOWN),
            (1, codes::DESCRIPTOR_INVALID)
        ]
    );
    let t = Tmp::new("source2-skips");
    source2_game(&t);
    assert_eq!(d(t.path(), None, &set).engine, "source2");
}

#[test]
fn floats_datetimes_nulls_duplicates_and_huge_integers_are_refused() {
    let float = SOURCE2.replace("first = 8", "first = 8.0");
    assert_eq!(parse_err(&float).code, codes::DESCRIPTOR_INVALID);
    let date = SOURCE2.replace("[[rule]]", "published = 2026-10-31T00:00:00Z\n[[rule]]");
    assert_eq!(parse_err(&date).code, codes::DESCRIPTOR_INVALID);
    let date = SOURCE2.replace("[[rule]]", "published = 2026-10-31\n[[rule]]");
    assert_eq!(parse_err(&date).code, codes::DESCRIPTOR_INVALID);
    for json in [
        r#"{"schema":"astra.engine/1","engine":{"codes":{"a1":"A"}},"rule":[],"x":null}"#,
        r#"{"schema":"astra.engine/1","schema":"astra.engine/1"}"#,
        r#"{"schema":"astra.engine/1","engine":{"codes":{"ab":"A","ab":"B"}}}"#,
        r#"{"schema":"astra.engine/1","n":1e3}"#,
        r#"{"schema":"astra.engine/1","n":9007199254740992}"#,
        r#"{"schema":"astra.engine/1"} trailing"#,
    ] {
        let e = Descriptor::from_json(json.as_bytes()).unwrap_err();
        assert_eq!(e.code, codes::DESCRIPTOR_INVALID, "{json}");
    }
    let v = serde_json::json!({"schema": "astra.engine/1", "f": 0.5});
    assert_eq!(
        Descriptor::from_json_value(&v).unwrap_err().code,
        codes::DESCRIPTOR_INVALID
    );
}

#[test]
fn the_size_limits_hold() {
    let huge = format!("{SOURCE2}# {}\n", "x".repeat(70 * 1024));
    assert_eq!(parse_err(&huge).code, codes::DESCRIPTOR_TOO_LARGE);
    let rule = SOURCE2.split_once("[[rule]]").unwrap().1;
    let seventeen = format!(
        "schema = \"astra.engine/1\"\nengine = {{ codes = {{ source2 = \"Source 2\" }} }}\n{}",
        format!("[[rule]]{rule}").repeat(17)
    );
    assert_eq!(parse_err(&seventeen).code, codes::DESCRIPTOR_INVALID);
    let sixteen = format!(
        "schema = \"astra.engine/1\"\nengine = {{ codes = {{ source2 = \"Source 2\" }} }}\n{}",
        format!("[[rule]]{rule}").repeat(16)
    );
    assert!(Descriptor::from_toml(&sixteen).is_ok());
    let padded = format!(
        "schema = \"astra.engine/1\"\nengine = {{ codes = {{ source2 = \"Source 2\" }} }}\nwrappers = [{}]\n[[rule]]{rule}",
        (0..16)
            .map(|i| format!("\"{}{i:02}\"", "w".repeat(30)))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(Descriptor::from_toml(&padded).is_ok());
    // Under the input limit, over the canonical one.
    let bloated = format!(
        "schema = \"astra.engine/1\"\nengine = {{ codes = {{ source2 = \"Source 2\" }} }}\n{}",
        format!(
            "[[rule]]{}",
            rule.replace(
                "{ listed = \"engine2.dll\", kind = \"file\" },",
                &format!(
                    "{{ listed = \"engine2.dll\", kind = \"file\" }}, {}",
                    (0..6)
                        .map(|i| format!(
                            "{{ stat = \"{}{i}\", kind = \"file\" }},",
                            "s".repeat(200)
                        ))
                        .collect::<String>()
                )
            )
        )
        .repeat(16)
    );
    assert_eq!(parse_err(&bloated).code, codes::DESCRIPTOR_TOO_LARGE);
}

#[test]
fn the_grammar_refuses_what_it_does_not_name() {
    let bad = [
        // code syntax, count
        SOURCE2.replace("source2 = \"Source 2\"", "Source2 = \"Source 2\""),
        SOURCE2.replace("source2 = \"Source 2\"", "s = \"Source 2\""),
        SOURCE2.replace(
            "source2 = \"Source 2\"",
            "a1 = \"A\", a2 = \"B\", a3 = \"C\", a4 = \"D\", source2 = \"Source 2\"",
        ),
        SOURCE2.replace("codes = { source2 = \"Source 2\" }", "codes = {}"),
        // a code the descriptor does not declare
        SOURCE2.replace("code = \"source2\"", "code = \"source3\""),
        // paths
        SOURCE2.replace("{project}/core", "{project}/../core"),
        SOURCE2.replace("{project}/core", "/core"),
        SOURCE2.replace("{project}/core", "{project}\\core"),
        SOURCE2.replace("{project}/core", "core/{project}"),
        SOURCE2.replace("{project}/core", "{project}/c*re/x"),
        SOURCE2.replace("{project}/core", "{project}/{unknown}"),
        SOURCE2.replace("{project}/core", "{project}/co{re"),
        SOURCE2.replace("{project}/core", "{project}/con:x"),
        SOURCE2.replace("listed = \"engine2.dll\"", "stat = \"engine*.dll\""),
        // {project} with no `at` capturing it
        SOURCE2.replace("at = \"{project}/bin/win64\", ", ""),
        // a capture used before it exists, or in a nested check
        SOURCE2.replace("evidence = \"{core}\"", "evidence = \"{data}\""),
        SOURCE2.replace(
            "{ listed = \"engine2.dll\", kind = \"file\" }",
            "{ any = [ { listed = \"engine2.dll\", kind = \"file\", as = \"x\" } ] }",
        ),
        // kind is mandatory; tail's keys belong to tail
        SOURCE2.replace(
            "listed = \"engine2.dll\", kind = \"file\"",
            "listed = \"engine2.dll\"",
        ),
        SOURCE2.replace("kind = \"file\" }", "kind = \"file\", bytes = 4 }"),
        // two primaries in one check
        SOURCE2.replace(
            "listed = \"engine2.dll\", kind = \"file\"",
            "listed = \"engine2.dll\", stat = \"x\", kind = \"file\"",
        ),
        // evidence must be a capture, {program} or {project}; never {dir}
        SOURCE2.replace("evidence = \"{core}\"", "evidence = \"{dir}\""),
        SOURCE2.replace("evidence = \"{core}\"", "evidence = \"{core}/x\""),
        // first is 1..=8
        SOURCE2.replace("first = 8", "first = 9"),
        SOURCE2.replace("first = 8", "first = 0"),
        // ext and suffix, exactly one
        SOURCE2.replace("ext = [\"exe\"]", "ext = [\"exe\"], suffix = \".exe\""),
        SOURCE2.replace("ext = [\"exe\"]", "ext = [\"EXE\"]"),
        // code and confidence, or classify
        SOURCE2.replace("confidence = \"high\"\n", ""),
    ];
    for b in &bad {
        let e = parse_err(b);
        assert_eq!(e.code, codes::DESCRIPTOR_INVALID, "{b}\n{e}");
    }
}

#[test]
fn any_all_none_nest_at_most_three_deep_and_hold_at_most_eight() {
    let nest = |depth: usize| {
        let mut c = "{ listed = \"engine2.dll\", kind = \"file\" }".to_owned();
        for _ in 0..depth {
            c = format!("{{ any = [ {c} ] }}");
        }
        SOURCE2.replace(
            "{ child = \"{project}/core\", kind = \"dir\", as = \"core\" }",
            &format!("{c}, {{ child = \"{{project}}/core\", kind = \"dir\", as = \"core\" }}"),
        )
    };
    assert!(Descriptor::from_toml(&nest(3)).is_ok());
    assert_eq!(parse_err(&nest(4)).code, codes::DESCRIPTOR_INVALID);
    let nine = (0..9)
        .map(|i| format!("{{ stat = \"f{i}\", kind = \"file\" }}"))
        .collect::<Vec<_>>()
        .join(", ");
    let wide = SOURCE2.replace(
        "{ listed = \"engine2.dll\", kind = \"file\" },",
        &format!("{{ listed = \"engine2.dll\", kind = \"file\" }}, {{ any = [ {nine} ] }},"),
    );
    assert_eq!(parse_err(&wide).code, codes::DESCRIPTOR_INVALID);
}

#[test]
fn version_templates_and_display_names_are_checked() {
    let with_template = |t: &str| {
        SOURCE2.replace(
            "evidence = \"{core}\"",
            &format!(
                "evidence = \"{{core}}\"\nversion = {{ head = [\"{{core}}/version.txt\"], template = \"{t}\" }}"
            ),
        )
    };
    assert!(Descriptor::from_toml(&with_template("N{1,4}.N{1,2}.N{1,3}[abfpx]N{1,3}")).is_ok());
    for bad in [
        "",
        "N{0,2}",
        "N{3,2}",
        "N{1,10}",
        "[]",
        "[A]",
        "N{1,2",
        "abc",
        "N{1,9}N{1,9}N{1,9}N{1,9}N{1,9}N{1,9}N{1,9}N{1,9}",
    ] {
        assert_eq!(
            parse_err(&with_template(bad)).code,
            codes::DESCRIPTOR_INVALID,
            "{bad:?}"
        );
    }
    for name in [
        "",
        " Source 2",
        "Source\u{202E}2",
        "Source\u{200B}2",
        "Source \u{00B7} 2",
        "Source | 2",
        "Source\u{FF5C}2",
        "Source\u{2028}2",
        &"x".repeat(49),
    ] {
        // Written into the TOML as the characters themselves, which TOML strings allow.
        let text = SOURCE2.replace("\"Source 2\"", &format!("\"{name}\""));
        assert!(toml::from_str::<toml::Table>(&text).is_ok() || name.is_empty());
        assert_eq!(parse_err(&text).code, codes::DESCRIPTOR_INVALID, "{name:?}");
    }
}

// ---- admission ----

/// Reserved codes: the built-ins, `unknown`, and anything that reads as them.
#[test]
fn reserved_codes_are_refused() {
    for code in [
        "unity-mono",
        "unity-il2cpp",
        "unreal",
        "godot",
        "unknown",
        "unity-mon0",
        "un1ty-mono",
        "unlty",
        "unity-hdrp",
        "unreal5",
        "g0d0t",
        "godot-4",
        "unknown-engine",
        "astra-engine",
        "engine-ambiguous",
        "unrea1",
    ] {
        assert!(is_reserved_code(code), "{code}");
        let text = SOURCE2.replace("source2", code);
        let e = refusal(&text, true);
        assert_eq!(e.code, codes::ENGINE_CODE_RESERVED, "{code}");
    }
    for code in [
        "source2",
        "monogame",
        "renpy",
        "rpg-maker-mv",
        "unigine",
        "love2d",
    ] {
        assert!(!is_reserved_code(code), "{code}");
    }
    // A name that reads as a built-in engine's, or as a review, is reserved too.
    for name in [
        "Unity (Mono)",
        "UNITY",
        "Unreal Engine 5",
        "Godot",
        "G0dot Fork",
        "Moderated Engine",
        "Verified by Astra",
        "First Party",
        "Астра",
    ] {
        let text = SOURCE2.replace("\"Source 2\"", &format!("\"{name}\""));
        assert_eq!(
            refusal(&text, false).code,
            codes::ENGINE_NAME_RESERVED,
            "{name}"
        );
    }
    let fine = SOURCE2.replace("\"Source 2\"", "\"Community Engine\"");
    assert_eq!(one(&fine).len(), 1);
}

/// "Grief unknown games": a rule with no literal anchor — or one on a program, a wrapper, a
/// runtime every game ships, or a name too common — is refused; first-party is exempt.
#[test]
fn every_community_rule_needs_a_specific_anchor() {
    let anchored = |listed: &str, kind: &str| {
        SOURCE2.replace(
            "{ listed = \"engine2.dll\", kind = \"file\" }",
            &format!("{{ listed = \"{listed}\", kind = \"{kind}\" }}"),
        )
    };
    let none = SOURCE2.replace("{ listed = \"engine2.dll\", kind = \"file\" },", "");
    let e = refusal(&none, false);
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::DESCRIPTOR_TOO_BROAD, Some("no-anchor"))
    );
    assert_eq!(
        set_of(&[(Moderation::Moderated, "astra", true, &none)]).len(),
        1
    );

    for (listed, kind, reason) in [
        ("steam_api64.dll", "file", "ubiquitous-runtime"),
        ("vcruntime140.dll", "file", "ubiquitous-runtime"),
        ("d3dcompiler_47.dll", "file", "ubiquitous-runtime"),
        (
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "file",
            "ubiquitous-runtime",
        ),
        ("UnityCrashHandler64.exe", "file", "program"),
        ("server", "file", "program"),
        ("GameLauncher.dat", "file", "wrapper"),
        ("Data", "dir", "too-short"),
        ("Content", "dir", "generic-name"),
        ("Binaries", "dir", "generic-name"),
        ("readme.txt", "file", "generic-name"),
        ("x.ab", "file", "too-short"),
    ] {
        let e = refusal(&anchored(listed, kind), false);
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::DESCRIPTOR_TOO_BROAD, Some(reason)),
            "{listed}"
        );
    }
    // A glob, a placeholder or a path is not a literal anchor.
    for listed in ["engine*.dll", "{stem}.dll", "bin/engine2.dll"] {
        let e = refusal(&anchored(listed, "file"), false);
        assert_eq!(e.code, codes::DESCRIPTOR_TOO_BROAD, "{listed}");
    }
    // An anchor inside `none` or `any` is not positive.
    let negative = SOURCE2.replace(
        "{ listed = \"engine2.dll\", kind = \"file\" }",
        "{ none = [ { listed = \"engine2.dll\", kind = \"file\" } ] }",
    );
    assert_eq!(refusal(&negative, false).code, codes::DESCRIPTOR_TOO_BROAD);
    // One good anchor beside a refused one is enough (both must still be present to match).
    let both = SOURCE2.replace(
        "{ listed = \"engine2.dll\", kind = \"file\" },",
        "{ listed = \"engine2.dll\", kind = \"file\" }, { listed = \"steam_api64.dll\", kind = \"file\" },",
    );
    assert_eq!(one(&both).len(), 1);
}

#[test]
fn first_party_only_and_unsupported_parts_are_refused_as_such() {
    let fp_only = [
        SOURCE2.replace("tier = \"strong\"", "tier = \"weak\"\nweak_order = 130"),
        SOURCE2.replace("first = 8", "first = 8, skip_wrappers = false"),
    ];
    for t in &fp_only {
        assert_eq!(
            refusal(t, false).code,
            codes::DESCRIPTOR_FIRST_PARTY_ONLY,
            "{t}"
        );
        assert_eq!(
            set_of(&[(Moderation::Moderated, "astra", true, t)]).len(),
            1
        );
    }
    let later = [
        SOURCE2.replace(
            "[[rule]]",
            "[walk]\nlist_at_limit = [\"Binaries/Win64\"]\n[[rule]]",
        ),
        SOURCE2.replace("tier = \"strong\"", "tier = \"weak\"\nscope = \"anywhere\""),
        SOURCE2
            .replace(
                "programs = { ext = [\"exe\"], at = \"{project}/bin/win64\", first = 8 }",
                "programs = \"none\"",
            )
            .replace(
                "{ child = \"{project}/core\", kind = \"dir\", as = \"core\" }",
                "{ child = \"core\", kind = \"dir\", as = \"core\" }",
            ),
    ];
    for t in &later {
        Descriptor::from_toml(t).unwrap_or_else(|e| panic!("{t}\n{e}"));
        assert_eq!(
            refusal(t, false).code,
            codes::DESCRIPTOR_FIRST_PARTY_ONLY,
            "{t}"
        );
        assert_eq!(refusal(t, true).code, codes::DESCRIPTOR_UNSUPPORTED, "{t}");
    }
    // Pass B's own: `first` given, programs it considers, evidence given.
    let pass_b = [
        SOURCE2.replace(", first = 8", ""),
        SOURCE2.replace("ext = [\"exe\"]", "ext = [\"dll\"]"),
        SOURCE2.replace("ext = [\"exe\"]", "suffix = \"-shipping.bat\""),
        SOURCE2.replace("evidence = \"{core}\"\n", ""),
    ];
    for t in &pass_b {
        assert_eq!(refusal(t, false).code, codes::DESCRIPTOR_INVALID, "{t}");
    }
}

#[test]
fn codes_belong_to_the_first_publisher_and_counts_are_capped() {
    // Offered unchecked first, moderated second: the moderated one is admitted first and owns
    // the code.
    let set = set_of(&[
        (
            Moderation::Unchecked,
            "squatter",
            false,
            &SOURCE2.replace("Source 2", "Source Two"),
        ),
        (Moderation::Moderated, "valve-fan", false, SOURCE2),
    ]);
    assert_eq!(set.len(), 1);
    let (_, o) = set.descriptors().next().unwrap();
    assert_eq!(o.publisher, "valve-fan");
    let s = &set.skipped()[0];
    assert_eq!((s.index, s.error.code), (0, codes::ENGINE_CODE_TAKEN));
    assert_eq!(s.error.param("owner"), Some("valve-fan"));

    // One publisher may ship several descriptors of its own code; the same one twice is one.
    let set = set_of(&[
        (Moderation::Unchecked, "p", false, SOURCE2),
        (Moderation::Unchecked, "p", false, SOURCE2),
        (
            Moderation::Unchecked,
            "p",
            false,
            &SOURCE2.replace("Source 2", "Source Two"),
        ),
    ]);
    assert_eq!(set.len(), 2);
    assert_eq!(set.skipped()[0].error.code, codes::DESCRIPTOR_DUPLICATE);

    // At most 4 per publisher, 64 in all.
    let numbered = |i: usize| SOURCE2.replace("source2", &format!("engine-{i}"));
    let five: Vec<String> = (0..5).map(numbered).collect();
    let set = EngineSet::build(five.iter().map(|t| {
        (
            origin(Moderation::Unchecked, "one", false),
            Descriptor::from_toml(t),
        )
    }));
    assert_eq!(set.len(), 4);
    assert_eq!(set.skipped()[0].error.param("reason"), Some("publisher"));
    let many: Vec<String> = (0..70).map(numbered).collect();
    let set = EngineSet::build(many.iter().enumerate().map(|(i, t)| {
        (
            origin(Moderation::Unchecked, &format!("p{i}"), false),
            Descriptor::from_toml(t),
        )
    }));
    assert_eq!(set.len(), 64);
    assert!(
        set.skipped()
            .iter()
            .all(|s| s.error.param("reason") == Some("total"))
    );
}

#[test]
fn the_hostile_set_admits_what_it_should_and_skips_the_rest() {
    let set = hostile_set();
    let admitted: Vec<&str> = set
        .descriptors()
        .flat_map(|(d, _)| d.codes().map(|(c, _)| c.as_str()))
        .collect();
    assert_eq!(
        admitted,
        [
            "hostile-all",
            "hostile-unity",
            "hostile-paks",
            "hostile-pck"
        ]
    );
    let skipped: Vec<&str> = set.skipped().iter().map(|s| s.error.code).collect();
    assert_eq!(
        skipped,
        [
            codes::ENGINE_CODE_RESERVED,
            codes::DESCRIPTOR_TOO_BROAD,
            codes::DESCRIPTOR_TOO_BROAD,
            codes::DESCRIPTOR_INVALID,
            codes::DESCRIPTOR_SCHEMA_UNKNOWN,
        ]
    );
}

// ---- Pass B: narrow-only (C1c) ----

/// "Take a recognised game": descriptors anchored on Unity's, Godot's and Unreal's own files,
/// and one claiming every program, moderated and first-party, change nothing Pass A decided —
/// a code, evidence, or a hint used.
#[test]
fn no_descriptor_takes_a_game_pass_a_recognised() {
    let hostile = hostile_set();
    let t = Tmp::new("c1c-unity");
    unity_game(t.path(), "Game");
    write(&t.join("Game.pdb"), b"pdb");
    assert_eq!(d(t.path(), None, hostile).engine, "unity-mono");

    // Unity with no flavour: `unknown`, but with evidence — Pass A's to keep.
    let t = Tmp::new("c1c-no-flavour");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("UnityPlayer.dll"), b"MZ");
    write(&t.join("Game_Data/globalgamemanagers"), b"x");
    let got = d(t.path(), None, hostile);
    assert!(got.engine.is_unknown() && got.evidence.is_some());
    assert_eq!(got.descriptor_sha256, None);

    // Unreal paks with no program, the weakest built-in sign.
    let t = Tmp::new("c1c-paks");
    write(
        &t.join("Gameface/Content/Paks/pakchunk0-WindowsNoEditor.pak"),
        b"pak",
    );
    write(&t.join("Gameface/Binaries/Win64/Game.pdb"), b"pdb");
    assert_eq!(d(t.path(), None, hostile).engine, "unreal");

    // Godot pairs.
    let t = Tmp::new("c1c-godot");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("Game.pck"), b"GDPC");
    assert_eq!(d(t.path(), Some("Game.exe"), hostile).engine, "godot");
}

/// Where Pass A had nothing, the hostile set may answer — that is what Pass B is for — and the
/// moderated claim wins over every lower one.
#[test]
fn pass_b_answers_only_where_pass_a_had_nothing() {
    let t = Tmp::new("open");
    write(&t.join("hl2.exe"), b"MZ");
    write(&t.join("hl2/gameinfo.txt"), b"x");
    let got = d(t.path(), None, hostile_set());
    assert_eq!(got.engine, "hostile-all");
    assert_eq!(got.exe, p("hl2.exe"));
    assert_eq!(got.evidence, p("hl2.exe"));
}

// ---- Pass B: choosing ----

const TOOL_ALPHA: &str = r#"
schema = "astra.engine/1"
engine = { codes = { alpha = "Alpha" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe", "x86_64"], first = 8 }
require = [ { listed = "alpha.core", kind = "file", as = "core" } ]
code = "alpha"
confidence = "high"
evidence = "{core}"
binary = "by-ext"
"#;

fn alpha_like(code: &str, anchor: &str) -> String {
    TOOL_ALPHA
        .replace("alpha = \"Alpha\"", &format!("{code} = \"Engine {code}\""))
        .replace("code = \"alpha\"", &format!("code = \"{code}\""))
        .replace("alpha.core", anchor)
}

#[test]
fn two_descriptors_of_one_standing_that_disagree_are_ambiguous() {
    let t = Tmp::new("ambiguous");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("Other.exe"), b"MZ");
    write(&t.join("alpha.core"), b"a");
    write(&t.join("beta.core"), b"b");
    let set = set_of(&[
        (
            Moderation::Unchecked,
            "a",
            false,
            &alpha_like("alpha", "alpha.core"),
        ),
        (
            Moderation::Unchecked,
            "b",
            false,
            &alpha_like("beta", "beta.core"),
        ),
    ]);
    let got = d(t.path(), None, &set);
    assert!(got.is_ambiguous());
    assert!(got.engine.is_unknown());
    assert_eq!(
        got.engine_candidates,
        [
            EngineCode::new("alpha").unwrap(),
            EngineCode::new("beta").unwrap()
        ]
    );
    assert_eq!(got.exe, None, "nothing preselected: Pass A's own answer");

    // A moderated one decides; the unchecked one neither replaces nor ambiguates it.
    let set = set_of(&[
        (
            Moderation::Unchecked,
            "b",
            false,
            &alpha_like("beta", "beta.core"),
        ),
        (
            Moderation::Moderated,
            "a",
            false,
            &alpha_like("alpha", "alpha.core"),
        ),
    ]);
    let got = d(t.path(), None, &set);
    assert_eq!(got.engine, "alpha");
    assert!(!got.is_ambiguous());
    // Local never outranks unchecked.
    let set = set_of(&[
        (
            Moderation::Local,
            "dev",
            false,
            &alpha_like("beta", "beta.core"),
        ),
        (
            Moderation::Unchecked,
            "a",
            false,
            &alpha_like("alpha", "alpha.core"),
        ),
    ]);
    assert_eq!(d(t.path(), None, &set).engine, "alpha");
}

#[test]
fn a_match_on_steams_hint_wins_within_its_standing() {
    let t = Tmp::new("hint-standing");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("alpha.core"), b"a");
    write(&t.join("deep/a/b/c/d/Tool.exe"), b"MZ");
    write(&t.join("deep/a/b/c/d/beta.core"), b"b");
    let set = set_of(&[
        (
            Moderation::Unchecked,
            "a",
            false,
            &alpha_like("alpha", "alpha.core"),
        ),
        (
            Moderation::Unchecked,
            "b",
            false,
            &alpha_like("beta", "beta.core"),
        ),
    ]);
    // Blind: only alpha's program is within the walk.
    assert_eq!(d(t.path(), None, &set).engine, "alpha");
    // Steam names the deep one: past the walk, still read (the hint skips the prefilter).
    let got = d(t.path(), Some("deep/a/b/c/d/Tool.exe"), &set);
    assert_eq!(
        (got.engine.as_str(), got.hint_used, got.exe),
        ("beta", true, p("deep/a/b/c/d/Tool.exe"))
    );
    // A hint naming a wrapper is never followed.
    write(&t.join("deep/a/b/c/d/Launcher.exe"), b"MZ");
    let got = d(t.path(), Some("deep/a/b/c/d/Launcher.exe"), &set);
    assert_eq!((got.engine.as_str(), got.hint_used), ("alpha", false));
}

#[test]
fn the_shallowest_then_the_largest_of_one_code_is_the_game() {
    let t = Tmp::new("one-code");
    for (dir, size) in [("A", 1024), ("B", 8192)] {
        write(&t.join(&format!("{dir}/Game.exe")), b"MZ");
        write(&t.join(&format!("{dir}/alpha.core")), &vec![0u8; size]);
    }
    let set = one(&alpha_like("alpha", "alpha.core"));
    assert_eq!(d(t.path(), None, &set).exe, p("B/Game.exe"));
    write(&t.join("Root.exe"), b"MZ");
    write(&t.join("alpha.core"), b"tiny");
    assert_eq!(d(t.path(), None, &set).exe, p("Root.exe"));
}

#[test]
fn programs_are_only_programs_by_their_magic_and_never_wrappers() {
    let set = one(&alpha_like("alpha", "alpha.core"));
    let t = Tmp::new("magic");
    write(&t.join("Game.exe"), b"PK not a PE");
    write(&t.join("alpha.core"), b"a");
    assert!(d(t.path(), None, &set).engine.is_unknown());
    write(&t.join("Game.exe"), b"MZ");
    assert_eq!(d(t.path(), None, &set).engine, "alpha");

    let t = Tmp::new("elf");
    write(&t.join("game.x86_64"), b"MZ wrong magic");
    write(&t.join("alpha.core"), b"a");
    assert!(d(t.path(), None, &set).engine.is_unknown());
    write(&t.join("game.x86_64"), b"\x7fELF");
    assert_eq!(d(t.path(), None, &set).binary, Binary::Linux);

    let t = Tmp::new("wrappers");
    write(&t.join("GameLauncher.exe"), b"MZ");
    write(&t.join("vc_redist.x64.exe"), b"MZ");
    write(&t.join("alpha.core"), b"a");
    assert!(d(t.path(), None, &set).engine.is_unknown());
    // Scoped wrappers: this descriptor's own, for its own rules.
    write(&t.join("Patcher.exe"), b"MZ");
    assert_eq!(d(t.path(), None, &set).exe, p("Patcher.exe"));
    let scoped =
        alpha_like("alpha", "alpha.core").replace("[[rule]]", "wrappers = [\"patcher\"]\n[[rule]]");
    assert!(d(t.path(), None, &one(&scoped)).engine.is_unknown());
}

#[test]
fn only_the_first_programs_are_considered() {
    let t = Tmp::new("first");
    for i in 0..10 {
        write(&t.join(&format!("d{i}/Tool.exe")), b"MZ");
    }
    write(&t.join("d9/alpha.core"), b"a");
    let eight = one(&alpha_like("alpha", "alpha.core"));
    assert!(
        d(t.path(), None, &eight).engine.is_unknown(),
        "d9 is the tenth program"
    );
    write(&t.join("d1/alpha.core"), b"a");
    assert_eq!(d(t.path(), None, &eight).exe, p("d1/Tool.exe"));
}

#[test]
fn classify_any_none_tail_and_version_read_as_written() {
    let text = r#"
schema = "astra.engine/1"
engine = { codes = { gamma = "Gamma", gamma-lite = "Gamma Lite" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe"], first = 8, also_if = { dir = "{stem}_res" } }
require = [ { listed = "gamma.runtime", kind = "file" },
            { listed = "{stem}_res", kind = "dir", as = "res" },
            { none = [ { listed = "nogamma.flag", kind = "file" } ] },
            { tail = "{program}", bytes = 4, equals = "GMMA", min_len = 12 } ]
classify = [ { when = { any = [ { stat = "{res}/full.pak", kind = "file" }, { child = "{res}/FULL", kind = "dir" } ] }, code = "gamma", confidence = "high" },
             { when = { all = [ { stat = "{res}/lite.pak", kind = "file" } ] }, code = "gamma-lite", confidence = "low" },
             { code = "unknown", confidence = "low" } ]
evidence = "{res}"
binary = "by-ext"
named_alike = "{stem}"
version = { head = ["{res}/missing.txt", "{res}/version.txt"], template = "N{1,2}.N{1,3}-[rb]N{1,2}" }
"#;
    let set = one(text);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let t = Tmp::new("gamma");
    let mut exe = b"MZ program bytes".to_vec();
    exe.extend_from_slice(b"GMMA");
    write(&t.join("Run.exe"), &exe);
    write(&t.join("gamma.runtime"), b"r");
    write(&t.join("Run_res/full/x"), b"x");
    write(
        &t.join("Run_res/version.txt"),
        b"build 112.4.7-r3 and 9.9-b1",
    );
    let got = d(t.path(), None, &set);
    assert_eq!(
        (got.engine.as_str(), got.confidence, got.version.as_deref()),
        ("gamma", Confidence::High, Some("4.7-r3"))
    );
    assert_eq!(got.evidence, p("Run_res"));

    // The lite flavour, then neither: `unknown` from classify names nothing.
    std::fs::remove_dir_all(t.join("Run_res/full")).unwrap();
    write(&t.join("Run_res/lite.pak"), b"x");
    assert_eq!(d(t.path(), None, &set).engine, "gamma-lite");
    std::fs::remove_file(t.join("Run_res/lite.pak")).unwrap();
    assert!(d(t.path(), None, &set).engine.is_unknown());
    write(&t.join("Run_res/full.pak"), b"x");
    assert_eq!(d(t.path(), None, &set).engine, "gamma");

    // `none` refuses; the tail must match and the file be long enough.
    write(&t.join("nogamma.flag"), b"x");
    assert!(d(t.path(), None, &set).engine.is_unknown());
    std::fs::remove_file(t.join("nogamma.flag")).unwrap();
    write(&t.join("Run.exe"), b"MZGMMA");
    assert!(
        d(t.path(), None, &set).engine.is_unknown(),
        "shorter than min_len"
    );
    write(&t.join("Run.exe"), &exe);
    assert_eq!(d(t.path(), None, &set).engine, "gamma");
}

#[test]
fn pick_takes_the_hint_else_the_largest_and_binary_follows_the_name() {
    let text = r#"
schema = "astra.engine/1"
engine = { codes = { delta = "Delta" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe"], at = "{project}/Bin", first = 8 }
require = [ { listed = "delta.vm", kind = "file" }, { child = "{project}/Packs/*.dpk", kind = "file", as = "pack" } ]
pick = "hint-else-largest"
code = "delta"
confidence = "high"
evidence = "{project}"
binary = { from_name = "{pack}", map = { "-win" = "windows", "-linux" = "linux" } }
"#;
    let set = one(text);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let t = Tmp::new("delta");
    write(&t.join("Proj/Bin/Small.exe"), b"MZ small");
    write(
        &t.join("Proj/Bin/Big.exe"),
        &[b'M', b'Z', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    write(&t.join("Proj/Bin/delta.vm"), b"vm");
    write(&t.join("Proj/Packs/main-linux.dpk"), b"p");
    let got = d(t.path(), None, &set);
    assert_eq!(
        (
            got.exe.clone(),
            got.exe_pick,
            got.binary,
            got.evidence.clone()
        ),
        (
            p("Proj/Bin/Big.exe"),
            Some(ExePick::Largest),
            Binary::Linux,
            p("Proj")
        )
    );
    let got = d(t.path(), Some("Proj/Bin/Small.exe"), &set);
    assert_eq!(
        (got.exe, got.exe_pick, got.hint_used),
        (p("Proj/Bin/Small.exe"), Some(ExePick::LaunchConfig), true)
    );
}

// ---- Pass B: anti-cheat ----

#[test]
fn a_claim_carries_its_own_markers_and_anti_cheat_unknown() {
    let with_marker = alpha_like("alpha", "alpha.core").replace(
        "[[rule]]",
        "anti_cheat = [ { listed = \"Guard\", kind = \"dir\", code = \"alpha-guard\" } ]\n[[rule]]",
    );
    let set = one(&with_marker);
    let t = Tmp::new("markers");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("alpha.core"), b"a");
    assert!(d(t.path(), None, &set).anti_cheat.is_empty());
    write(&t.join("sub/Guard/x.bin"), b"x");
    assert_eq!(d(t.path(), None, &set).anti_cheat, ["alpha-guard"]);
    write(&t.join("drivers/gamedrv.sys"), b"MZ");
    assert_eq!(
        d(t.path(), None, &set).anti_cheat,
        ["alpha-guard", codes::ANTI_CHEAT_UNKNOWN]
    );
    // Without a claim, nothing is added: the markers are the claim's own.
    std::fs::remove_file(t.join("alpha.core")).unwrap();
    assert!(d(t.path(), None, &set).anti_cheat.is_empty());
}

#[test]
fn a_walk_cut_at_its_cap_cannot_vouch_for_anti_cheat() {
    let set = one(&alpha_like("alpha", "alpha.core"));
    let t = Tmp::new("cap");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("alpha.core"), b"a");
    for i in 0..5001 {
        write(&t.join(&format!("zz/{i:05}.dat")), b"");
    }
    let got = d(t.path(), None, &set);
    assert_eq!(got.engine, "alpha");
    assert_eq!(got.anti_cheat, [codes::ANTI_CHEAT_UNKNOWN]);
}

// ---- Pass B: budgets ----

/// "Slow detection": a descriptor that would read too much yields nothing, quickly, and costs
/// no other descriptor its answer.
#[test]
fn a_descriptor_over_its_budget_yields_nothing_and_never_hangs() {
    let t = Tmp::new("budget");
    for i in 0..8 {
        write(&t.join(&format!("p{i}/Tool.exe")), b"MZ");
        write(&t.join(&format!("p{i}/hog.anchor")), b"h");
    }
    write(&t.join("p7/real.anchor"), b"r");
    // One rule: 8 programs × (anchor + 6 stats + magic) = 64 lookups, exactly the budget;
    // sixteen of them are far past it.
    let checks = (0..6)
        .map(|i| format!("{{ stat = \"missing{i}\", kind = \"file\" }}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rule = format!(
        "[[rule]]\ntier = \"strong\"\nprograms = {{ ext = [\"exe\"], first = 8 }}\nrequire = [ {{ listed = \"hog.anchor\", kind = \"file\" }}, {{ none = [ {checks} ] }} ]\ncode = \"hog\"\nconfidence = \"high\"\nevidence = \"{{program}}\"\nbinary = \"windows\"\n"
    );
    let hog = format!(
        "schema = \"astra.engine/1\"\nengine = {{ codes = {{ hog = \"Hog\" }} }}\n{}",
        rule.repeat(16)
    );
    let real = alpha_like("alpha", "real.anchor");
    let started = std::time::Instant::now();
    let set = set_of(&[
        (Moderation::Unchecked, "hog", false, &hog),
        (Moderation::Unchecked, "real", false, &real),
    ]);
    assert_eq!(set.len(), 2, "{:?}", set.skipped());
    let got = d(t.path(), None, &set);
    assert_eq!(
        got.engine, "alpha",
        "the hog yields nothing; alpha still answers"
    );
    // The hog alone: nothing at all, though every one of its rules would match.
    let only = one(&hog);
    assert!(d(t.path(), None, &only).engine.is_unknown());
    let small = one(&hog.replacen(&rule, "", 15));
    assert_eq!(
        d(t.path(), None, &small).engine,
        "hog",
        "one rule fits its budget"
    );
    assert!(started.elapsed() < Duration::from_secs(20));

    // Probes: eight tails fit, nine are one over.
    let tail = "{ tail = \"{program}\", bytes = 2, equals = \"MZ\" }";
    let probes = alpha_like("alpha", "real.anchor").replace(
        "{ listed = \"real.anchor\", kind = \"file\", as = \"core\" }",
        &format!(
            "{{ listed = \"real.anchor\", kind = \"file\", as = \"core\" }}, {{ all = [ {} ] }}",
            [tail; 8].join(", ")
        ),
    );
    assert_eq!(d(t.path(), None, &one(&probes)).engine, "alpha");
    let probes9 = probes.replace(
        "{ all = [ ",
        "{ tail = \"{program}\", bytes = 2, equals = \"MZ\" }, { all = [ ",
    );
    assert!(d(t.path(), None, &one(&probes9)).engine.is_unknown());
}

/// The game's own budget: twenty descriptors spending nearly all of theirs leave the game with
/// no Pass B answer at all — not the answer of whichever one came first.
#[test]
fn a_game_over_its_budget_gets_no_pass_b_answer() {
    let t = Tmp::new("game-budget");
    for i in 0..8 {
        write(&t.join(&format!("p{i}/Tool.exe")), b"MZ");
        write(&t.join(&format!("p{i}/shared.anchor")), b"s");
    }
    let spender = |i: usize| {
        let checks = (0..6)
            .map(|k| format!("{{ stat = \"missing{k}\", kind = \"file\" }}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "schema = \"astra.engine/1\"\nengine = {{ codes = {{ spend-{i} = \"Spender {i}\" }} }}\n[[rule]]\ntier = \"strong\"\nprograms = {{ ext = [\"exe\"], first = 8 }}\nrequire = [ {{ listed = \"shared.anchor\", kind = \"file\" }}, {{ none = [ {checks} ] }} ]\ncode = \"spend-{i}\"\nconfidence = \"high\"\nevidence = \"{{program}}\"\nbinary = \"windows\"\n"
        )
    };
    // Each: 8 programs × (1 anchor + 6 stats + 1 magic) = 64 lookups, exactly its budget.
    let texts: Vec<String> = (0..20).map(spender).collect();
    let set = EngineSet::build(texts.iter().enumerate().map(|(i, s)| {
        (
            origin(Moderation::Unchecked, &format!("p{i}"), false),
            Descriptor::from_toml(s),
        )
    }));
    assert_eq!(set.len(), 20);
    let few = EngineSet::build(texts.iter().take(2).enumerate().map(|(i, s)| {
        (
            origin(Moderation::Unchecked, &format!("p{i}"), false),
            Descriptor::from_toml(s),
        )
    }));
    assert!(
        d(t.path(), None, &few).is_ambiguous(),
        "two fit the game's budget"
    );
    let got = d(t.path(), None, &set);
    assert!(got.engine.is_unknown() && !got.is_ambiguous(), "{got:?}");
    assert_eq!(got, detect_with(t.path(), None).unwrap());
}

/// The anchors are the O(1) prefilter: descriptors whose anchors the folder does not hold cost
/// the game nothing, however many rules they carry — the one that does match still answers.
#[test]
fn a_descriptor_whose_anchors_are_absent_costs_nothing() {
    let t = Tmp::new("prefilter");
    for i in 0..8 {
        write(&t.join(&format!("p{i}/Tool.exe")), b"MZ");
    }
    write(&t.join("p3/real.anchor"), b"r");
    let rule = "[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], first = 8 }\nrequire = [ { listed = \"absent.anchor\", kind = \"file\" } ]\ncode = \"CODE\"\nconfidence = \"high\"\nevidence = \"{program}\"\nbinary = \"windows\"\n";
    let absent: Vec<String> = (0..20)
        .map(|i| {
            format!(
                "schema = \"astra.engine/1\"\nengine = {{ codes = {{ absent-{i} = \"Absent {i}\" }} }}\n{}",
                rule.replace("CODE", &format!("absent-{i}")).repeat(16)
            )
        })
        .collect();
    let real = alpha_like("alpha", "real.anchor");
    // 20 × 16 rules × 8 programs would be 2,560 lookups were the anchors not a prefilter.
    let set = EngineSet::build(
        absent
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    origin(Moderation::Unchecked, &format!("a{i}"), false),
                    Descriptor::from_toml(s),
                )
            })
            .chain(std::iter::once((
                origin(Moderation::Unchecked, "real", false),
                Descriptor::from_toml(&real),
            ))),
    );
    assert_eq!(set.len(), 21, "{:?}", set.skipped());
    let got = d(t.path(), None, &set);
    assert_eq!((got.engine.as_str(), got.exe), ("alpha", p("p3/Tool.exe")));
}

#[test]
fn listed_never_resolves_to_a_wrappers_name_and_skip_dirs_skip() {
    let text = alpha_like("alpha", "alpha.core").replace(
        "{ listed = \"alpha.core\", kind = \"file\", as = \"core\" }",
        "{ listed = \"alpha.core\", kind = \"file\", as = \"core\" }, { listed = \"{stem}*.cfg\", kind = \"file\" }",
    );
    let set = one(&text);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let t = Tmp::new("listed-wrapper");
    write(&t.join("Game.exe"), b"MZ");
    write(&t.join("alpha.core"), b"a");
    write(&t.join("GameLauncher.cfg"), b"c");
    assert!(
        d(t.path(), None, &set).engine.is_unknown(),
        "only a wrapper's name matches"
    );
    write(&t.join("GameSettings.cfg"), b"c");
    assert_eq!(d(t.path(), None, &set).engine, "alpha");

    let skipping = one(&alpha_like("alpha", "alpha.core")
        .replace("first = 8 }", "first = 8, skip_dirs = [\"Tools\"] }"));
    let t = Tmp::new("skip-dirs");
    write(&t.join("tools/Editor.exe"), b"MZ");
    write(&t.join("tools/alpha.core"), b"a");
    assert!(d(t.path(), None, &skipping).engine.is_unknown());
    assert_eq!(
        d(t.path(), None, &one(&alpha_like("alpha", "alpha.core"))).engine,
        "alpha"
    );
}

/// `{project}` is at least one folder: a program directly at `at`'s tail, at the top, has none.
#[test]
fn a_project_is_never_the_root() {
    let set = one(SOURCE2);
    let t = Tmp::new("flat-source2");
    write(&t.join("bin/win64/cs2.exe"), b"MZ");
    write(&t.join("bin/win64/engine2.dll"), b"MZ");
    write(&t.join("core/pak01_dir.vpk"), b"vpk");
    assert!(d(t.path(), None, &set).engine.is_unknown());
}

// ---- Pass B: links and FIFOs ----

/// "Read outside the folder": no lookup or probe goes through a link, and none blocks on a FIFO.
#[cfg(unix)]
#[test]
fn links_are_never_followed_and_fifos_never_block() {
    use std::os::unix::fs::symlink;
    let outside = Tmp::new("outside");
    write(&outside.join("core/pak01_dir.vpk"), b"vpk");
    write(&outside.join("engine2.dll"), b"MZ");
    write(&outside.join("cs2.exe"), b"MZ outside");

    let set = one(SOURCE2);
    // The game's `core` is a link out of the folder: `child` refuses it.
    let t = Tmp::new("link-core");
    write(&t.join("game/bin/win64/cs2.exe"), b"MZ");
    write(&t.join("game/bin/win64/engine2.dll"), b"MZ");
    symlink(outside.join("core"), t.join("game/core")).unwrap();
    assert!(d(t.path(), None, &set).engine.is_unknown());
    // The anchor is a link: the walk does not list it.
    let t = Tmp::new("link-anchor");
    write(&t.join("game/bin/win64/cs2.exe"), b"MZ");
    write(&t.join("game/core/x"), b"x");
    symlink(
        outside.join("engine2.dll"),
        t.join("game/bin/win64/engine2.dll"),
    )
    .unwrap();
    assert!(d(t.path(), None, &set).engine.is_unknown());
    // The program is a link: not a program. Through the hint either.
    let t = Tmp::new("link-program");
    write(&t.join("game/bin/win64/engine2.dll"), b"MZ");
    write(&t.join("game/core/x"), b"x");
    symlink(outside.join("cs2.exe"), t.join("game/bin/win64/cs2.exe")).unwrap();
    assert!(d(t.path(), None, &set).engine.is_unknown());
    assert!(
        d(t.path(), Some("game/bin/win64/cs2.exe"), &set)
            .engine
            .is_unknown()
    );
    // A whole folder on the way is a link.
    let t = Tmp::new("link-dir");
    write(&outside.join("bin/win64/cs2.exe"), b"MZ");
    write(&outside.join("bin/win64/engine2.dll"), b"MZ");
    write(&t.join("game/core/x"), b"x");
    symlink(outside.join("bin"), t.join("game/bin")).unwrap();
    assert!(
        d(t.path(), Some("game/bin/win64/cs2.exe"), &set)
            .engine
            .is_unknown()
    );

    // `stat`, `tail` and `version.head` on links and FIFOs, with a deadline on the whole call.
    let text = r#"
schema = "astra.engine/1"
engine = { codes = { eps = "Eps" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe"], at = "bin", first = 8 }
require = [ { listed = "eps.runtime", kind = "file" }, { stat = "{dir}/data/core.bin", kind = "file", as = "core" }, { tail = "{dir}/data/tail.bin", bytes = 3, equals = "EPS" } ]
code = "eps"
confidence = "high"
evidence = "{core}"
binary = "windows"
version = { head = ["{dir}/data/version.txt"], template = "N{1,3}.N{1,3}" }
"#;
    let set = one(text);
    assert_eq!(set.len(), 1, "{:?}", set.skipped());
    let make = |name: &str| {
        let t = Tmp::new(name);
        write(&t.join("bin/Game.exe"), b"MZ");
        write(&t.join("bin/eps.runtime"), b"r");
        write(&t.join("bin/data/core.bin"), b"c");
        write(&t.join("bin/data/tail.bin"), b"..EPS");
        write(&t.join("bin/data/version.txt"), b"v 1.25");
        t
    };
    let t = make("eps");
    let got = d(t.path(), None, &set);
    assert_eq!(
        (got.engine.as_str(), got.version.as_deref()),
        ("eps", Some("1.25"))
    );

    let fifo = |p: &Path| {
        let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path, as mkfifo wants.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    };
    let within = |t: &Tmp, set: &EngineSet| -> Detection {
        let (tx, rx) = mpsc::channel();
        let dir = t.path().to_path_buf();
        let set = set.clone();
        std::thread::spawn(move || {
            let _ = tx.send(detect_with_engines(&dir, None, &set).unwrap());
        });
        rx.recv_timeout(Duration::from_secs(20))
            .expect("Pass B blocked on a FIFO")
    };
    let t = make("eps-tail-fifo");
    std::fs::remove_file(t.join("bin/data/tail.bin")).unwrap();
    fifo(&t.join("bin/data/tail.bin"));
    assert!(within(&t, &set).engine.is_unknown());
    let t = make("eps-version-fifo");
    std::fs::remove_file(t.join("bin/data/version.txt")).unwrap();
    fifo(&t.join("bin/data/version.txt"));
    let got = within(&t, &set);
    assert_eq!((got.engine.as_str(), got.version), ("eps", None));
    let t = make("eps-program-fifo");
    std::fs::remove_file(t.join("bin/Game.exe")).unwrap();
    fifo(&t.join("bin/Game.exe"));
    assert!(within(&t, &set).engine.is_unknown());
    let t = make("eps-tail-link");
    std::fs::remove_file(t.join("bin/data/tail.bin")).unwrap();
    write(&outside.join("tail.bin"), b"EPS");
    symlink(outside.join("tail.bin"), t.join("bin/data/tail.bin")).unwrap();
    assert!(d(t.path(), None, &set).engine.is_unknown());
    let t = make("eps-stat-link");
    std::fs::remove_dir_all(t.join("bin/data")).unwrap();
    write(&outside.join("data/core.bin"), b"c");
    write(&outside.join("data/tail.bin"), b"EPS");
    symlink(outside.join("data"), t.join("bin/data")).unwrap();
    assert!(d(t.path(), None, &set).engine.is_unknown());
    // `stat` alone through a linked folder (no probe there to refuse it instead).
    let stat_only = text.replace(
        ", { tail = \"{dir}/data/tail.bin\", bytes = 3, equals = \"EPS\" }",
        "",
    );
    let stat_only = one(&stat_only);
    assert_eq!(stat_only.len(), 1, "{:?}", stat_only.skipped());
    assert!(d(t.path(), None, &stat_only).engine.is_unknown());
    let t = make("eps-stat-real");
    assert_eq!(d(t.path(), None, &stat_only).engine, "eps");
    let t = make("eps-version-link");
    std::fs::remove_file(t.join("bin/data/version.txt")).unwrap();
    write(&outside.join("version.txt"), b"9.99");
    symlink(outside.join("version.txt"), t.join("bin/data/version.txt")).unwrap();
    let got = d(t.path(), None, &set);
    assert_eq!((got.engine.as_str(), got.version), ("eps", None));
}

/// A detection saved by 0.4.1 still reads, and every built-in code survives the round trip.
#[test]
fn engine_codes_serialise_as_the_bare_string() {
    let old = r#"{"engine":"unity-il2cpp","confidence":"high","exe":"Game.exe","binary":"windows","anti_cheat":[]}"#;
    let d: Detection = serde_json::from_str(old).unwrap();
    assert_eq!(d.engine, astra_game_launcher::Engine::UnityIl2cpp);
    assert_eq!(serde_json::to_value(&d.engine).unwrap(), "unity-il2cpp");
    let bad = old.replace("unity-il2cpp", "Unity Il2cpp");
    assert!(serde_json::from_str::<Detection>(&bad).is_err());
    for e in astra_game_launcher::Engine::ALL {
        let c = EngineCode::from(e);
        assert_eq!((c.as_str(), c.builtin()), (e.code(), Some(e)));
        assert!(c.is_reserved());
    }
}
