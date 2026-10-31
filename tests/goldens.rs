// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Canaries K13 and K17: the specification's goldens (AstraPlugins `testdata/games/`,
//! `spec/games/reserved.yaml`, vendored at `tests/goldens/astraplugins/PIN` by
//! `tools/vendor-astraplugins-goldens.sh`) against this launcher's own parser.
//!
//! - The vendored copy is the pinned bytes (`SHA256SUMS`), before anything reads it.
//! - **K17.** The built-in descriptor goldens parse and are, by digest, the launcher's
//!   `src/engines/builtin/*.toml`. Each hostile descriptor is refused as `vectors.json` labels it:
//!   `engine-code-taken` by [`EngineSet::build`] behind the golden it is paired with, the rest by
//!   the parser or [`Descriptor::check_pass_b`] (a community descriptor). Each manifest golden is
//!   accepted or refused as labelled by [`Manifest::parse_and_lint`] — `runner-builtin` as
//!   Astra's own, the rest as community.
//! - **K13.** `reserved.yaml`'s profile file names, id prefixes and name substrings are
//!   [`reserved`]'s, and every engine code it reserves the launcher refuses to descriptors.
//!
//! The verdict AND the code are pinned, never a reason's text. Every disagreement at this pin is
//! one entry of `SPEC_DEFECTS_AT_PIN` (`tests/common/spec_goldens.rs`), which asserts it still
//! holds; `tests/gi_probe.rs` judges `gi-probe check` against the same list.

mod common;
#[path = "common/spec_goldens.rs"]
mod spec_goldens;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use astra_game_launcher::engines::descriptor::limits;
use astra_game_launcher::{
    Descriptor, EngineSet, LauncherError, Manifest, Moderation, Origin, Platform, codes,
    is_reserved_code, is_valid_engine_code, plan_install, reserved,
};
use common::{E, Tmp, open, target, unity_game, write, zip};
use serde_json::Value;
use spec_goldens::{
    Answer, Seen, SpecDefect, defect, finding, first_party, golden_text, judge, judge_manifest,
    manifest_goldens, root, sums, vectors, want,
};

fn answer_of(e: &LauncherError) -> Answer {
    vec![finding(e.code, &e.params)]
}

fn failures(list: Vec<String>) {
    assert!(list.is_empty(), "\n{}", list.join("\n"));
}

/// The sums first: every vendored file is the pinned bytes, and only those are there.
#[test]
fn the_vendored_goldens_are_the_pinned_bytes() {
    let sums = sums();
    assert!(sums.len() > 40, "{} files", sums.len());
    for want in [
        "testdata/games/vectors.json",
        "spec/games/reserved.yaml",
        "spec/games/schemas/manifest.schema.json",
        "spec/games/schemas/engine-descriptor-v1.schema.json",
        "NOTICE",
    ] {
        assert!(sums.contains_key(want), "{want} is not vendored");
    }
}

// ---------------------------------------------------------------------------------------------
// K17: descriptors.
// ---------------------------------------------------------------------------------------------

#[test]
fn k17_the_builtin_goldens_are_the_launchers_builtins() {
    let mut out = Vec::new();
    for r in vectors()["descriptors"]["builtin"].as_array().unwrap() {
        let name = r["name"].as_str().unwrap();
        let golden = format!("testdata/games/{}", r["file"].as_str().unwrap());
        assert_eq!(r["verdict"], "accept", "{golden}");
        let ours_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/engines/builtin")
            .join(format!("{name}.toml"));
        let ours = Descriptor::from_toml(&fs::read_to_string(&ours_path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", ours_path.display()));
        let seen = match Descriptor::from_toml(&golden_text(r["file"].as_str().unwrap())) {
            Err(e) => Some(answer_of(&e)),
            Ok(d) if d.digest() == ours.digest() => None,
            Ok(_) => Some(Vec::new()),
        };
        let fail = match (defect(&golden).map(|d| d.seen), seen) {
            (None, None) => None,
            (None, Some(a)) if a.is_empty() => Some(format!(
                "{golden}: parses, and is not the launcher's {name}.toml by digest"
            )),
            (None, Some(a)) => Some(format!("{golden}: the launcher refuses it: {a:?}")),
            (Some(s), None) => Some(format!(
                "{golden}: is the launcher's {name}.toml now; its SPEC_DEFECTS_AT_PIN entry ({s:?}) must go"
            )),
            (Some(Seen::DigestDiffers), Some(a)) if a.is_empty() => None,
            (Some(Seen::Answered(want)), Some(a)) if spec_goldens::owned(want) == a => None,
            (Some(s), Some(a)) => Some(format!(
                "{golden}: the launcher answers {a:?} (empty: a digest other than its own); the entry records {s:?}"
            )),
        };
        out.extend(fail);
    }
    failures(out);
}

fn community(publisher: &str) -> Origin {
    Origin {
        moderation: Moderation::Unchecked,
        publisher: publisher.to_owned(),
        first_party: false,
    }
}

/// `[integration.engine]` of the manifest golden `rel`, as a descriptor.
fn descriptor_of_manifest(rel: &str) -> Descriptor {
    let m = Manifest::parse(&golden_text(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    m.engine_descriptor()
        .unwrap_or_else(|| panic!("{rel}: no [integration.engine]"))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .clone()
}

#[test]
fn k17_each_hostile_descriptor_is_refused_as_labelled() {
    let mut out = Vec::new();
    let hostile = vectors()["descriptors"]["hostile"].as_array().unwrap();
    assert!(hostile.len() >= 6, "{} hostile goldens", hostile.len());
    for r in hostile {
        let file = r["file"].as_str().unwrap();
        let golden = format!("testdata/games/{file}");
        let text = golden_text(file);
        let answer: Answer = if r["rule"] == "engine-code-taken" {
            // Behind the golden it is paired with, which claimed the code first.
            let owner = descriptor_of_manifest(r["paired_with"].as_str().unwrap());
            let set = EngineSet::build([
                (community("owner"), Ok(owner)),
                (community("squatter"), Descriptor::from_toml(&text)),
            ]);
            assert_eq!(set.len() + set.skipped().len(), 2, "{golden}");
            set.skipped()
                .iter()
                .map(|s| {
                    assert_eq!(s.index, 1, "{golden}: the owner was refused: {:?}", s.error);
                    finding(s.error.code, &s.error.params)
                })
                .collect()
        } else {
            match Descriptor::from_toml(&text) {
                Err(e) => answer_of(&e),
                Ok(d) => d
                    .check_pass_b(false)
                    .err()
                    .map(|e| answer_of(&e))
                    .unwrap_or_default(),
            }
        };
        out.extend(judge(&golden, &want(r), &answer));
    }
    failures(out);
}

/// The engine-code-taken pair: the owner alone is admitted, so the refusal is the squatter's.
#[test]
fn k17_the_owner_of_a_code_is_admitted_alone() {
    let owner = descriptor_of_manifest("manifests/valid/foundation-owns-engine.toml");
    let set = EngineSet::build([(community("owner"), Ok(owner))]);
    assert_eq!((set.len(), set.skipped().len()), (1, 0));
}

// ---------------------------------------------------------------------------------------------
// K17: manifests.
// ---------------------------------------------------------------------------------------------

/// Every refusal [`Manifest::parse_and_lint`] makes of `text`, in order.
fn manifest_answer(text: &str, first_party: bool) -> Answer {
    let (parsed, lint) = Manifest::parse_and_lint(text, first_party);
    parsed
        .err()
        .iter()
        .chain(lint.iter())
        .map(|e| finding(e.code, &e.params))
        .collect()
}

#[test]
fn k17_each_manifest_golden_is_accepted_or_refused_as_labelled() {
    let goldens = manifest_goldens();
    assert!(goldens.len() >= 15, "{} manifest goldens", goldens.len());
    let mut out = Vec::new();
    for (golden, r) in &goldens {
        let text = golden_text(r["file"].as_str().unwrap());
        let fp = first_party(golden, r);
        let answer = manifest_answer(&text, fp);
        out.extend(judge_manifest(golden, r, &answer, || {
            manifest_answer(&text, false)
        }));
    }
    failures(out);
}

/// Plan B §1's two foundations of one engine: the owner, and one sharing its code.
#[test]
fn the_specs_foundations_parse() {
    for rel in [
        "manifests/valid/foundation-owns-engine.toml",
        "manifests/valid/foundation-shares-engine.toml",
        "manifests/valid/foundation-dependency-claim.toml",
    ] {
        let m = Manifest::parse(&golden_text(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert!(m.lint(false).is_empty(), "{rel}: {:?}", m.lint(false));
    }
    let owns =
        Manifest::parse(&golden_text("manifests/valid/foundation-owns-engine.toml")).unwrap();
    assert_eq!(owns.engine_code().unwrap().as_str(), "source2");
    assert!(owns.engine().is_none());
    let d = owns.engine_descriptor().unwrap().unwrap();
    assert_eq!(
        d.codes().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
        ["source2"]
    );
    let loader = owns.loader().unwrap();
    assert_eq!(loader.proxies, ["dinput8.dll"]);
    assert_eq!(loader.keep_if_present, ["s2bridge.ini"]);
    assert!(owns.declares_companion("presentation"));
    let shares = Manifest::parse(&golden_text(
        "manifests/valid/foundation-shares-engine.toml",
    ))
    .unwrap();
    assert_eq!(shares.engine_code().unwrap().as_str(), "source2");
    assert!(shares.engine_descriptor().is_none());
    let claim = Manifest::parse(&golden_text(
        "manifests/valid/foundation-dependency-claim.toml",
    ))
    .unwrap();
    let f = claim.foundation().unwrap();
    assert_eq!(
        (f.id.as_str(), f.version.as_str(), f.engine.as_str()),
        ("astra.unity", "0.8.1", "unity-mono")
    );
}

// ---------------------------------------------------------------------------------------------
// K13: reserved.yaml.
// ---------------------------------------------------------------------------------------------

/// `reserved.yaml` in its own hand-parseable subset: `key: value`, or `key:` then `  - item`;
/// `#` starts a comment outside quotes.
fn reserved_yaml() -> BTreeMap<String, Vec<String>> {
    sums();
    let text = fs::read_to_string(root().join("spec/games/reserved.yaml")).unwrap();
    let strip = |s: &str| -> String {
        let mut out = String::new();
        let mut quoted = false;
        for c in s.chars() {
            match c {
                '"' => quoted = !quoted,
                '#' if !quoted => break,
                c => out.push(c),
            }
        }
        out.trim().to_owned()
    };
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            let key = current.clone().expect("a list item under no key");
            out.get_mut(&key).unwrap().push(strip(item));
        } else {
            let (k, v) = line.split_once(':').expect("key: value");
            let v = strip(v);
            out.insert(
                k.trim().to_owned(),
                if v.is_empty() { Vec::new() } else { vec![v] },
            );
            current = Some(k.trim().to_owned());
        }
    }
    out
}

fn set<I: IntoIterator<Item = S>, S: Into<String>>(i: I) -> BTreeSet<String> {
    i.into_iter().map(Into::into).collect()
}

#[test]
fn k13_reserved_names_and_ids_are_the_specs() {
    let y = reserved_yaml();
    let mut out = Vec::new();

    // Profile file names, compared as the launcher compares them: ignoring ASCII case.
    let ours = set(reserved::PROFILE_FILE_NAMES
        .iter()
        .map(|n| n.to_ascii_lowercase()));
    let theirs = set(y["profile_file_names"]
        .iter()
        .map(|n| n.to_ascii_lowercase()));
    let lack: BTreeSet<String> = ours.difference(&theirs).cloned().collect();
    let extra: BTreeSet<String> = theirs.difference(&ours).cloned().collect();
    assert!(
        extra.is_empty(),
        "reserved.yaml reserves profile names the launcher does not: {extra:?}"
    );
    match defect("spec/games/reserved.yaml").map(|d| d.seen) {
        Some(Seen::ProfileNamesLack(names)) if lack == set(names.iter().copied()) => {}
        Some(s) => out.push(format!(
            "profile_file_names: the spec lacks {lack:?}; SPEC_DEFECTS_AT_PIN records {s:?}"
        )),
        None if lack.is_empty() => {}
        None => out.push(format!("profile_file_names: the spec lacks {lack:?}")),
    }
    for n in &theirs {
        assert!(reserved::profile_name_is_reserved(n), "{n}");
    }

    // Ids and names: equal, in order.
    assert_eq!(
        y["integration_id_reserved_prefixes"],
        reserved::INTEGRATION_ID_PREFIXES
    );
    assert_eq!(
        y["integration_name_reserved_substrings"],
        reserved::INTEGRATION_NAME_SUBSTRINGS
    );

    // Engine codes: every one the spec reserves, the launcher refuses to descriptors, and the
    // spec's list is the built-in codes and `unknown`.
    let codes_yaml = &y["engine_codes"];
    for c in codes_yaml {
        assert!(is_valid_engine_code(c), "{c}");
        assert!(
            is_reserved_code(c),
            "{c}: reserved by the spec, not by the launcher"
        );
    }
    assert_eq!(
        set(codes_yaml.iter().cloned()),
        set(reserved::engine_codes()
            .iter()
            .map(|c| c.as_str().to_owned()))
    );
    assert_eq!(y["engine_code_pattern"], ["^[a-z][a-z0-9-]{1,31}$"]);
    assert_eq!(
        y["max_engine_codes_per_descriptor"],
        [limits::CODES.to_string()]
    );
    // What the launcher reserves beyond the list, which the spec does not state.
    const KEYS: &[&str] = &[
        "engine_code_pattern",
        "engine_codes",
        "max_engine_codes_per_descriptor",
        "integration_id_reserved_prefixes",
        "integration_name_reserved_substrings",
        "profile_file_names",
    ];
    let unread: Vec<&String> = y.keys().filter(|k| !KEYS.contains(&k.as_str())).collect();
    let by_rule = [
        "un1ty-mono",
        "g0dot",
        "astra-engine",
        "unknown-x",
        "unreal-fork",
        "x-ambiguous",
    ];
    let unlisted: Vec<&str> = by_rule
        .iter()
        .copied()
        .filter(|c| is_reserved_code(c) && !codes_yaml.iter().any(|y| y == c))
        .collect();
    match defect("spec/games/reserved.yaml#engine_codes").map(|d| d.seen) {
        Some(Seen::EngineCodesListedOnly) if unread.is_empty() && unlisted.len() == by_rule.len() => {}
        Some(_) => out.push(format!(
            "reserved.yaml now states more of the engine-code rule ({unread:?}): compare it here, and drop its SPEC_DEFECTS_AT_PIN entry"
        )),
        None if unread.is_empty() => out.push(format!(
            "reserved.yaml lists engine codes only, while the launcher also reserves {unlisted:?}"
        )),
        None => out.push(format!("reserved.yaml keys this test does not read: {unread:?}")),
    }
    failures(out);
}

// ---------------------------------------------------------------------------------------------
// The list itself.
// ---------------------------------------------------------------------------------------------

/// Every entry names a golden that is vendored and asked by a test above, once.
#[test]
fn every_spec_defect_names_a_golden_that_is_asked() {
    let v = vectors();
    let mut asked = BTreeSet::new();
    let records = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| format!("testdata/games/{}", r["file"].as_str().unwrap()))
            .collect()
    };
    asked.extend(records(&v["descriptors"]["builtin"]));
    asked.extend(records(&v["descriptors"]["hostile"]));
    asked.extend(manifest_goldens().into_iter().map(|(g, _)| g));
    asked.insert("spec/games/reserved.yaml".to_owned());
    asked.insert("spec/games/reserved.yaml#engine_codes".to_owned());
    let mut seen = BTreeSet::new();
    for SpecDefect { golden, why, .. } in spec_goldens::SPEC_DEFECTS_AT_PIN {
        assert!(asked.contains(*golden), "{golden}: no such golden is asked");
        assert!(seen.insert(*golden), "{golden}: two entries");
        assert!(!why.is_empty(), "{golden}: say why");
        let file = golden.split('#').next().unwrap();
        assert!(sums().contains_key(file), "{file}: not vendored");
    }
    // The schema-pinned answers name refusals of the launcher's, by code.
    for (file, answer) in spec_goldens::SCHEMA_REFUSALS {
        assert!(asked.contains(&format!("testdata/games/{file}")), "{file}");
        for (code, _) in *answer {
            assert!(
                codes::ALL.contains(code),
                "{file}: {code} is not a launcher code"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What the [integration.*] tables change for 0.3.1 and for planning.
// ---------------------------------------------------------------------------------------------

/// Every `.toml` below `dir`.
fn tomls(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            tomls(&p, out);
        } else if p.extension().is_some_and(|x| x == "toml") {
            out.push(p);
        }
    }
}

/// A source manifest's pins as its packaging fills them (`gta3.astra-gi.toml` is astra-gta-
/// trilogy's template: `sha256 = "@sha256:<path>@"`, written over by `tools/package-gta.sh`).
fn filled(text: &str) -> String {
    text.lines()
        .map(|l| match l.split_once("sha256 = \"@sha256:") {
            Some((head, _)) => format!("{head}sha256 = \"{}\"", "0".repeat(64)),
            None => l.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// **0.3.1 parses every first-party manifest, and so does this launcher.** Every manifest under
/// `tests/fixtures/` (the first-party releases Astra 0.2.7 installs, astra-reshade's history, the
/// C1b fixtures; a template's pins filled) parses with this crate and with launcher 0.3.1, what
/// Astra 0.2.7 ships.
#[test]
fn every_fixture_manifest_parses_with_this_launcher_and_with_031() {
    let mut files = Vec::new();
    tomls(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
        &mut files,
    );
    assert!(files.len() >= 30, "{} manifests", files.len());
    for p in files {
        let text = filled(&fs::read_to_string(&p).unwrap());
        let ours = Manifest::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        launcher_031::Manifest::parse(&text)
            .unwrap_or_else(|e| panic!("{}: 0.3.1 refuses it: {e:?}", p.display()));
        // None of them declares a table of 0.5.0's, so nothing of them changes.
        assert!(ours.engine_descriptor().is_none(), "{}", p.display());
        assert!(ours.loader().is_none(), "{}", p.display());
    }
}

/// What 0.3.1 makes of the spec's manifest goldens: it reads past every `[integration.*]` table
/// (a game-specific GI with a Link, a foundation claim), and refuses a foundation of an engine
/// it does not know at parse, so no 0.2.7 user is ever offered one (plan B §2).
#[test]
fn launcher_031_reads_past_the_integration_tables() {
    for rel in [
        "manifests/valid/game-specific-link.toml",
        "manifests/valid/foundation-dependency-claim.toml",
        "manifests/valid/runner-builtin.toml",
        "manifests/invalid/capability-undeclared.toml",
    ] {
        launcher_031::Manifest::parse(&golden_text(rel))
            .unwrap_or_else(|e| panic!("{rel}: 0.3.1 refuses it: {e:?}"));
    }
    for rel in [
        "manifests/valid/foundation-owns-engine.toml",
        "manifests/valid/foundation-shares-engine.toml",
    ] {
        let e = launcher_031::Manifest::parse(&golden_text(rel)).unwrap_err();
        assert_eq!(
            (e.code, e.params.get("key").map(String::as_str)),
            ("manifest-invalid", Some("target.engine")),
            "{rel}"
        );
    }
}

/// The spec's Source 2 foundation as a release zip.
fn source2_foundation() -> astra_game_launcher::GiPackage {
    let text = golden_text("manifests/valid/foundation-owns-engine.toml");
    open(&zip(&[
        E::File("astra-gi.toml", text.as_bytes()),
        E::File("bridge/s2bridge.dll", b"MZ bridge"),
    ]))
    .unwrap()
}

/// **A descriptor's code plans on no game until planning is given descriptors.** `[target]
/// engine = "source2"` is no longer refused at parse, and the plan's engine check is not skipped
/// for it: on a Unity game, on a game no engine recognises, and on a folder shaped as the
/// descriptor describes, it is `engine-mismatch{want, got}`.
#[test]
fn a_descriptors_engine_is_engine_mismatch_wherever_planning_has_no_descriptors() {
    let pkg = source2_foundation();
    assert_eq!(pkg.manifest().engine_code().unwrap().as_str(), "source2");

    let unity = Tmp::new("s2-on-unity");
    unity_game(unity.path(), "Game");
    let unknown = Tmp::new("s2-on-unknown");
    write(&unknown.join("Game.exe"), b"MZ game");
    let shaped = Tmp::new("s2-on-shaped");
    write(&shaped.join("game/bin/win64/cs2.exe"), b"MZ game");
    write(&shaped.join("game/bin/win64/engine2.dll"), b"MZ engine");
    write(&shaped.join("game/core/x.vpk"), b"vpk");
    for (dir, exe, got) in [
        (&unity, "Game.exe", "unity-mono"),
        (&unknown, "Game.exe", "unknown"),
        (&shaped, "game/bin/win64/cs2.exe", "unknown"),
    ] {
        let profile = Tmp::new("s2-profile");
        let e = plan_install(
            &pkg,
            &target(dir.path(), exe, None, Platform::Windows),
            &profile.join("p"),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.param("want"), e.param("got")),
            (codes::ENGINE_MISMATCH, Some("source2"), Some(got)),
            "{exe}"
        );
    }
}
