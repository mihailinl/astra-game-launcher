// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `[[integration.content]]` (`astra_game_launcher::content`, AstraPlugins `manifest.md` §2.7)
//! against the specification's own goldens, as `tests/goldens.rs` holds the rest of them: every
//! `testdata/games/content/releases/` folder and every `testdata/games/content/vrm/` file gets the
//! verdict and the code `vectors.json` gives it — the same goldens AstraPlugins'
//! `tools/check-games-goldens.py` and the catalogue's `lib/content.mjs` are held to — and every
//! refusal of ours its hostile fixture beside a near miss that passes
//! (`tests/common/content_fixtures.rs`; `tests/gi_probe.rs` runs all of them through `gi-probe
//! check`).

mod common;
#[path = "common/content_fixtures.rs"]
mod content_fixtures;
#[path = "common/spec_goldens.rs"]
mod spec_goldens;

use std::collections::BTreeSet;

use astra_game_launcher::content::{
    self, ContentKind, Gate, GltfClass, LicenceSource, SPEC_CODES, launcher_code,
};
use astra_game_launcher::{Manifest, codes};
use content_fixtures::{
    FBX, GateWant, PNG, VRM1_ALLOWED, fixture_failure, gate_vectors, glb, hostile, library_answer,
    manifest, near_miss_failure, part, release_vectors, vrm1, vrma, zip_of,
};
use serde_json::Value;

fn failures(list: Vec<String>) {
    assert!(list.is_empty(), "\n{}", list.join("\n"));
}

/// Every code `vectors.json` names for content is the spec's and has the launcher's spelling, and
/// the mapping names each launcher code once and only codes the launcher has.
#[test]
fn every_spec_content_code_has_the_launchers_spelling() {
    let v = spec_goldens::vectors();
    for r in v["content"]["releases"].as_array().unwrap() {
        if let Some(code) = r.get("code").and_then(Value::as_str) {
            assert!(launcher_code(code).is_some(), "{code}");
        }
    }
    let mut seen = BTreeSet::new();
    for (spec, ours) in SPEC_CODES {
        assert!(seen.insert(*ours), "{ours} twice");
        let tail = spec
            .strip_prefix("E_")
            .or_else(|| spec.strip_prefix("F_"))
            .unwrap();
        assert_eq!(tail.to_ascii_lowercase().replace('_', "-"), *ours, "{spec}");
        if spec.starts_with("E_") {
            assert!(codes::ALL.contains(ours), "{ours} is not in codes::ALL");
        } else {
            assert!(
                !codes::ALL.contains(ours),
                "a flag is never a refusal: {ours}"
            );
        }
    }
}

/// **The releases.** Each folder of `content/releases/`, zipped as it is, is refused with
/// exactly the one code `vectors.json` gives, or accepted with no refusal.
#[test]
fn every_content_release_golden_gets_its_verdict_and_code() {
    let mut out = Vec::new();
    for (name, files, code) in release_vectors() {
        let (refused, _, _) = library_answer(&zip_of(&files));
        let codes: Vec<&str> = refused.iter().map(|(c, _)| c.as_str()).collect();
        let ok = match code {
            None => codes.is_empty(),
            Some(c) => codes == [c],
        };
        if !ok {
            out.push(format!("{name}: answered {refused:?}; wants {code:?}"));
        }
    }
    failures(out);
}

/// What `allowed` and `vrm-unknown-but-declared` carry beyond their verdict: the licence each
/// part's own licence came from, and the unknown meta's flag.
#[test]
fn an_accepted_release_says_where_each_licence_came_from() {
    let by_name = |n: &str| {
        release_vectors()
            .into_iter()
            .find(|(name, _, _)| name == n)
            .unwrap()
            .1
    };
    let (_, flags, r) = library_answer(&zip_of(&by_name("allowed")));
    let r = r.unwrap();
    assert!(flags.is_empty(), "{flags:?}");
    let sources: Vec<(&str, Option<LicenceSource>)> = r
        .parts
        .iter()
        .map(|p| (p.path.as_str(), p.licence_source))
        .collect();
    assert_eq!(
        sources,
        [
            ("art/cover.png", Some(LicenceSource::Declared)),
            ("character/avatar.vrm", Some(LicenceSource::Vrm)),
        ]
    );
    let vrm = r.parts[1].vrm_licence.as_ref().unwrap();
    assert_eq!(vrm.vrm_version, "0.x");
    assert_eq!(vrm.fields["licenseName"], "CC_BY");
    assert_eq!(r.parts[1].gltf, Some(GltfClass::Avatar));
    assert!(r.parts.iter().all(|p| p.sha256.len() == 64));

    let (refused, flags, r) = library_answer(&zip_of(&by_name("vrm-unknown-but-declared")));
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(
        flags,
        [(
            content::FLAG_VRM_META_UNKNOWN.to_owned(),
            "character/avatar.vrm".to_owned()
        )]
    );
    let p = &r.unwrap().parts[0];
    assert_eq!(p.licence_source, Some(LicenceSource::Declared));
    assert!(p.vrm_licence.is_none());
}

/// **The gate.** Each file of `content/vrm/` through [`content::vrm_gate`] alone, as AstraPlugins'
/// checker runs it (no path): its verdict, and for a refusal the deciding field, its value and
/// whether a default decided; for an unknown the reading that decided, when one did; for a
/// format refusal its rule.
#[test]
fn every_vrm_gate_golden_gets_its_verdict() {
    let mut out = Vec::new();
    for (name, _, bytes, want) in gate_vectors() {
        let got = content::vrm_gate(&bytes, None, None);
        let ok = match (&want, &got) {
            (GateWant::Accept, Gate::Allowed(_)) => true,
            (GateWant::Refuse(field, value, defaulted), Gate::Refused { deciding, .. }) => {
                deciding.meta == *field
                    && deciding.value == *value
                    && deciding.defaulted == *defaulted
            }
            (GateWant::Unknown(None), Gate::Unknown(None)) => true,
            (GateWant::Unknown(Some((field, value))), Gate::Unknown(Some(d))) => {
                d.meta == *field && d.value == *value
            }
            (GateWant::Conflict, Gate::Conflict { .. }) => true,
            (GateWant::Format(reason), Gate::Format(p)) => p.reason == *reason,
            _ => false,
        };
        if !ok {
            out.push(format!("{name}: {got:?}; wants {want:?}"));
        }
    }
    failures(out);
}

/// The same files as the one `character` part of a release, its licence declared: a release
/// refused exactly where the gate refuses, flagged where it reads unknown.
#[test]
fn every_vrm_gate_golden_decides_its_release() {
    let mut out = Vec::new();
    for (name, file, bytes, want) in gate_vectors() {
        let (refused, flags, report) =
            library_answer(&content_fixtures::gate_release(&file, &bytes));
        let path = format!("character/{file}");
        let codes: Vec<&str> = refused.iter().map(|(c, _)| c.as_str()).collect();
        let ok = match &want {
            GateWant::Accept => {
                refused.is_empty()
                    && flags.is_empty()
                    && report.unwrap().parts[0].licence_source == Some(LicenceSource::Vrm)
            }
            GateWant::Refuse(field, _, _) => {
                refused == [(codes::CONTENT_LICENCE_REFUSED.to_owned(), field.clone())]
            }
            GateWant::Unknown(_) => {
                refused.is_empty()
                    && flags == [(content::FLAG_VRM_META_UNKNOWN.to_owned(), path.clone())]
                    && report.unwrap().parts[0].licence_source == Some(LicenceSource::Declared)
            }
            GateWant::Conflict => codes == [codes::CONTENT_LICENCE_CONFLICT],
            GateWant::Format(reason) => {
                refused == [(codes::CONTENT_FORMAT.to_owned(), (*reason).to_owned())]
            }
        };
        if !ok {
            out.push(format!(
                "{name}: refused {refused:?} flagged {flags:?}; wants {want:?}"
            ));
        }
    }
    failures(out);
}

/// **Our hostile fixtures**: each refused as it says, each near miss passing.
#[test]
fn every_hostile_content_fixture_is_refused_and_its_near_miss_passes() {
    let fixtures = hostile();
    assert!(fixtures.len() >= 25, "{} fixtures", fixtures.len());
    let mut out = Vec::new();
    for f in &fixtures {
        let (refused, flagged, _) = library_answer(&zip_of(&f.files));
        out.extend(fixture_failure(f, &refused, &flagged));
        out.extend(near_miss_failure(f, |zip| {
            let (r, fl, _) = library_answer(zip);
            (r, fl)
        }));
    }
    // Every refusal of the module has a fixture.
    let refused: BTreeSet<&str> = fixtures
        .iter()
        .flat_map(|f| f.refused.iter().map(|(c, _)| *c))
        .collect();
    for (_, code) in SPEC_CODES.iter().filter(|(s, _)| s.starts_with("E_")) {
        if !refused.contains(code) {
            out.push(format!("{code}: no hostile fixture"));
        }
    }
    failures(out);
}

/// What the table reads: every key, a missing one refused, and the order kept.
#[test]
fn the_table_is_read_as_written() {
    let m = Manifest::parse(&manifest(
        &(part("animations/climb.fbx", "animations", Some("CC-BY-4.0"))
            + &part("character/avatar.vrm", "character", None)),
    ))
    .unwrap();
    let parts = m.content();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].kind, ContentKind::Animations);
    assert_eq!(parts[0].licence.as_deref(), Some("CC-BY-4.0"));
    assert_eq!(parts[1].kind, ContentKind::Character);
    assert_eq!(parts[1].licence, None);
    for (missing, key) in [
        ("path", "integration.content[0].path"),
        ("kind", "integration.content[0].kind"),
        ("title", "integration.content[0].title"),
        ("credit", "integration.content[0].credit"),
    ] {
        let table: String = part("a.png", "art", None)
            .lines()
            .filter(|l| !l.starts_with(&format!("{missing} =")))
            .map(|l| format!("{l}\n"))
            .collect();
        let e = Manifest::parse(&manifest(&table)).unwrap_err();
        assert_eq!(
            (e.code, e.param("key")),
            (codes::MANIFEST_INVALID, Some(key)),
            "{missing}"
        );
    }
    // A table, not an array of them.
    let e = Manifest::parse(&manifest("[integration.content]\npath = \"a.png\"\n")).unwrap_err();
    assert_eq!(e.param("key"), Some("integration.content"));
    // Absent: no parts, as every manifest from before.
    assert!(Manifest::parse(&manifest("")).unwrap().content().is_empty());
}

/// A file is content only when its extension AND its first bytes say so: code named as content
/// is never asked about, and a content file named as code is not either (a game integration is
/// native code, the check covers what Astra's loaders would read).
#[test]
fn only_content_typed_files_need_declaring() {
    let zip = zip_of(&[
        ("astra-gi.toml".to_owned(), manifest("").into_bytes()),
        ("plugins/climb.fbx".to_owned(), b"MZ loader".to_vec()),
        ("plugins/Astra.dll".to_owned(), PNG.to_vec()),
        ("docs/readme.txt".to_owned(), FBX.to_vec()),
    ]);
    let (refused, flags, r) = library_answer(&zip);
    assert!(
        refused.is_empty() && flags.is_empty(),
        "{refused:?} {flags:?}"
    );
    assert!(r.unwrap().content_files.is_empty());
}

/// A plain glTF is ordinary art outside a `character` part, and an animation gets no avatar
/// verdict in any part; an avatar gets one in every part.
#[test]
fn a_glb_is_judged_by_what_its_extensions_make_it() {
    let plain = glb(r#"{"asset":{"version":"2.0"}}"#);
    for (kind, bytes, judged) in [
        ("art", plain.clone(), false),
        ("character", plain, true),
        ("character", vrma(), false),
        ("animations", vrma(), false),
        ("audio", vrm1(VRM1_ALLOWED), true),
    ] {
        let zip = zip_of(&[
            (
                "astra-gi.toml".to_owned(),
                manifest(&part("x/model.glb", kind, Some("CC0-1.0"))).into_bytes(),
            ),
            ("x/model.glb".to_owned(), bytes),
        ]);
        let (refused, flags, r) = library_answer(&zip);
        assert!(refused.is_empty(), "{kind}: {refused:?}");
        let p = &r.unwrap().parts[0];
        let gate_said = p.vrm_licence.is_some() || !flags.is_empty();
        assert_eq!(gate_said, judged, "{kind}: {p:?} {flags:?}");
    }
}

/// Every `.astrachar` bundle of one release is read within ONE budget, the limits the release was
/// opened with: two bundles that each fit, together do not, and the second is refused
/// (`content-format{reason=too-large}`) where a fresh budget per bundle let N of them cost N times
/// what one may.
#[test]
fn every_character_bundle_of_a_release_shares_its_one_budget() {
    let padding = vec![0u8; 150 << 10];
    let bundle = content_fixtures::astrachar(
        &content_fixtures::astrachar_json(r#"{"file":"models/Mira.vrm"}"#),
        &[
            ("models/Mira.vrm", &vrm1(VRM1_ALLOWED)),
            ("textures/pad.bin", &padding),
        ],
    );
    let release = |names: &[&str]| {
        let parts: String = names
            .iter()
            .map(|n| part(n, "character", Some("CC-BY-4.0")))
            .collect();
        let mut files = vec![("astra-gi.toml".to_owned(), manifest(&parts).into_bytes())];
        files.extend(names.iter().map(|n| ((*n).to_owned(), bundle.clone())));
        zip_of(&files)
    };
    let limits = astra_game_launcher::Limits {
        max_uncompressed_bytes: 256 << 10,
        ..Default::default()
    };
    let content = |zip: &[u8]| {
        astra_game_launcher::GiPackage::open_with_limits(zip, &common::sha(zip), &limits)
            .unwrap()
            .content()
            .unwrap()
    };
    let one = content(&release(&["characters/a.astrachar"]));
    assert!(one.refusals.is_empty(), "{:?}", one.refusals);
    let two = content(&release(&[
        "characters/a.astrachar",
        "characters/b.astrachar",
    ]));
    let refused: Vec<_> = two
        .refusals
        .iter()
        .map(|e| (e.code, e.param("path"), e.param("reason")))
        .collect();
    assert_eq!(
        refused,
        [(
            codes::CONTENT_FORMAT,
            Some("characters/b.astrachar"),
            Some("too-large")
        )]
    );
    // With the default limits, both fit.
    let both = release(&["characters/a.astrachar", "characters/b.astrachar"]);
    let r = astra_game_launcher::GiPackage::open(&both, &common::sha(&both))
        .unwrap()
        .content()
        .unwrap();
    assert!(r.refusals.is_empty(), "{:?}", r.refusals);
}
