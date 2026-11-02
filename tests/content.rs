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
    manifest, near_miss_failure, part, release_answer_agrees, release_vectors, vrm1, vrma, zip_of,
};
use serde_json::Value;

fn failures(list: Vec<String>) {
    assert!(list.is_empty(), "\n{}", list.join("\n"));
}

/// Every code and flag `vectors.json` names for content is the spec's and has the launcher's
/// spelling, and the mapping names each launcher code once and only codes the launcher has.
#[test]
fn every_spec_content_code_has_the_launchers_spelling() {
    let v = spec_goldens::vectors();
    for r in v["content"]["releases"].as_array().unwrap() {
        if let Some(code) = r.get("code").and_then(Value::as_str) {
            assert!(launcher_code(code).is_some(), "{code}");
        }
        for flag in r
            .get("flags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            assert!(launcher_code(flag.as_str().unwrap()).is_some(), "{flag}");
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

/// **The releases.** Each folder of `content/releases/`, zipped as it is (with the files too
/// large to commit built as the spec builds them, `synth`), is refused with exactly the one code
/// `vectors.json` gives — however many of its files carry it — or accepted with none, and carries
/// exactly the flags it gives. A `guard` = `strict_json` golden's `.gltf` is refused by the strict
/// parser alone, never a crash.
#[test]
fn every_content_release_golden_gets_its_verdict_and_code() {
    let mut out = Vec::new();
    for v in release_vectors() {
        let (refused, flagged, _) = library_answer(&zip_of(&v.files));
        if !release_answer_agrees(&v, &refused, &flagged) {
            out.push(format!(
                "{}: answered {refused:?} flagged {flagged:?}; wants {:?} flagged {:?}",
                v.name, v.code, v.flags
            ));
        }
        if v.strict_json_guard {
            for (name, bytes) in v.files.iter().filter(|(n, _)| n.ends_with(".gltf")) {
                let why = content::format_problem(bytes, Some(name), None);
                if why.as_ref().map(|p| p.reason) != Some("json-too-deep") {
                    out.push(format!("{}: {name} is {why:?}", v.name));
                }
            }
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
            .find(|v| v.name == n)
            .unwrap()
            .files
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
    // The gate's fields AND the usage flags (legal G5: games-index-v1 `vrmLicenceMeta`), as the
    // file carries them: here the avatar's whole VRM 0.x meta.
    assert_eq!(
        Value::Object(vrm.fields.clone()),
        serde_json::json!({
            "licenseName": "CC_BY",
            "allowedUserName": "Everyone",
            "violentUssageName": "Disallow",
            "sexualUssageName": "Disallow",
            "commercialUssageName": "Allow"
        })
    );
    assert_eq!(r.parts[1].gltf, Some(GltfClass::Avatar));
    assert_eq!(
        r.classes().into_iter().collect::<Vec<_>>(),
        [("character/avatar.vrm".to_owned(), GltfClass::Avatar)]
    );
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
    assert!(fixtures.len() >= 80, "{} fixtures", fixtures.len());
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
    let flagged: BTreeSet<&str> = fixtures
        .iter()
        .flat_map(|f| f.flagged.iter().map(|(c, _)| *c))
        .collect();
    for (spec, code) in SPEC_CODES {
        let has = if spec.starts_with("E_") {
            refused.contains(code)
        } else {
            flagged.contains(code)
        };
        if !has {
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

/// The licence kept with a VRM part (`vrm_licence`): the gate's two fields and every usage flag
/// the file carries (legal G5), of the 1.0 meta where the file carries two that agree — the
/// spec's goldens `allowed-vrm1.vrm` (all six VRM 1.0 keys) and `vrm-metas-agree.vrm`.
#[test]
fn the_vrm_licence_kept_is_every_gate_and_usage_field_of_the_meta_shown() {
    let vrm1_six = serde_json::json!({
        "allowRedistribution": true,
        "avatarPermission": "everyone",
        "allowExcessivelyViolentUsage": false,
        "allowExcessivelySexualUsage": false,
        "allowPoliticalOrReligiousUsage": true,
        "allowAntisocialOrHateUsage": false
    });
    for name in ["allowed-vrm1.vrm", "vrm-metas-agree.vrm"] {
        let (_, file, bytes, _) = gate_vectors()
            .into_iter()
            .find(|(_, f, _, _)| f == name)
            .unwrap();
        let (refused, _, r) = library_answer(&content_fixtures::gate_release(&file, &bytes));
        assert!(refused.is_empty(), "{name}: {refused:?}");
        let vrm = r.unwrap().parts[0].vrm_licence.clone().unwrap();
        assert_eq!(vrm.vrm_version, "1.0", "{name}: the 1.0 meta is shown");
        assert_eq!(Value::Object(vrm.fields), vrm1_six, "{name}");
    }
}

/// Every row of `manifest.md` §2.7.1's table, through `GiPackage::content`: a file of that
/// extension (any letter case) with its format's real signature is content-typed — so undeclared
/// it is `content-undeclared` — and one whose first bytes miss by one is not. (A GLB is a whole
/// one, and a `.gltf` near miss a valid JSON object whose `{` is past the 32 bytes sniffed: every
/// GLB and `.gltf` is held to the format rules whether or not it is content-typed.)
#[test]
fn every_content_extension_is_content_only_with_its_own_signature() {
    const WHOLE_GLB: &[u8] = b"glTF\x02\0\0\0\x18\0\0\0\x04\0\0\0JSON{}  ";
    let zip_magic = [&b"PK\x03\x04"[..], b"PK\x05\x06", b"PK\x07\x08"];
    // (extension, real first bytes, near-miss first bytes)
    type Row<'a> = (&'a str, Vec<&'a [u8]>, Vec<&'a [u8]>);
    let rows: Vec<Row> = vec![
        (
            ".astrapack",
            zip_magic.to_vec(),
            vec![b"PK\x01\x02", b"PK\x04\x03"],
        ),
        (
            ".astrachar",
            zip_magic.to_vec(),
            vec![b"PK\x06\x06", b"KP\x03\x04"],
        ),
        (".vrma", vec![WHOLE_GLB], vec![b"gltf\x02\0\0\0"]),
        (".glb", vec![WHOLE_GLB], vec![b"GLTF\x02\0\0\0"]),
        (".vrm", vec![WHOLE_GLB], vec![b"glTf\x02\0\0\0"]),
        (
            ".gltf",
            vec![b"{\"asset\":{}}", b" \n\t{}"],
            vec![
                b"                                 {}",
                b"\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t{}",
            ],
        ),
        (
            ".fbx",
            vec![b"Kaydara FBX Binary  \0\x1a\0", b"; FBX 7.4.0 project file"],
            vec![b"Kaydara fbx Binary  \0\x1a\0", b"; FBX 7.4"],
        ),
        (
            ".png",
            vec![b"\x89PNG\r\n\x1a\n\0\0"],
            vec![b"\x89PNG\r\n\x1a\x0b\0\0"],
        ),
        (".jpg", vec![b"\xff\xd8\xff\xe0"], vec![b"\xff\xd8\xfe\xe0"]),
        (
            ".jpeg",
            vec![b"\xff\xd8\xff\xdb"],
            vec![b"\xfe\xd8\xff\xdb"],
        ),
        (
            ".webp",
            vec![b"RIFF\0\0\0\0WEBPVP8 "],
            vec![b"RIFF\0\0\0\0WAVEfmt "],
        ),
        (
            ".gif",
            vec![b"GIF87a\x01\0", b"GIF89a\x01\0"],
            vec![b"GIF88a\x01\0"],
        ),
        (".ogg", vec![b"OggS\0\x02"], vec![b"OggT\0\x02"]),
        (
            ".wav",
            vec![b"RIFF\0\0\0\0WAVEfmt "],
            vec![b"RIFF\0\0\0\0WEBPVP8 "],
        ),
        (
            ".mp3",
            vec![b"ID3\x04\0", b"\xff\xfb\x90\x00"],
            vec![b"ID2\x04\0", b"\xff\x1b\x90\x00"],
        ),
        (".flac", vec![b"fLaC\0\0"], vec![b"fLaB\0\0"]),
    ];
    let mut out = Vec::new();
    for (ext, real, near) in &rows {
        for (i, head) in real.iter().enumerate() {
            for name in [
                format!("x/real{i}{ext}"),
                format!("x/REAL{i}{}", ext.to_uppercase()),
            ] {
                let zip = zip_of(&[
                    ("astra-gi.toml".to_owned(), manifest("").into_bytes()),
                    (name.clone(), head.to_vec()),
                ]);
                let (refused, _, _) = library_answer(&zip);
                if refused != [(codes::CONTENT_UNDECLARED.to_owned(), name.clone())] {
                    out.push(format!(
                        "{name} ({head:?}): {refused:?}, not content-undeclared"
                    ));
                }
            }
        }
        for (i, head) in near.iter().enumerate() {
            let name = format!("x/near{i}{ext}");
            let zip = zip_of(&[
                ("astra-gi.toml".to_owned(), manifest("").into_bytes()),
                (name.clone(), head.to_vec()),
            ]);
            let (refused, _, r) = library_answer(&zip);
            if !refused.is_empty() || !r.is_some_and(|r| r.content_files.is_empty()) {
                out.push(format!(
                    "{name} ({head:?}): {refused:?}, but it is no {ext}"
                ));
            }
        }
    }
    // The table is closed: these rows are every extension the launcher reads as content.
    let listed: BTreeSet<&str> = rows.iter().map(|(e, ..)| *e).collect();
    for ext in [
        ".bin", ".dll", ".zip", ".txt", ".json", ".tga", ".bmp", ".mp4",
    ] {
        assert!(!content::has_content_extension(&format!("a{ext}")), "{ext}");
    }
    for ext in &listed {
        assert!(content::has_content_extension(&format!("a{ext}")), "{ext}");
    }
    failures(out);
}
