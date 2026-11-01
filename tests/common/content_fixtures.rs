// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `[[integration.content]]`'s goldens and hostile fixtures, judged one way for `tests/content.rs`
//! (the library: `GiPackage::open` and `GiPackage::content`) and `tests/gi_probe.rs` (`gi-probe
//! check`, step 1 and step 8).
//!
//! - The spec's: every `testdata/games/content/releases/` folder, zipped as it is, and every
//!   `testdata/games/content/vrm/` file in a release of one `character` part with a declared
//!   licence, which makes the part's verdict exactly the gate's (a plain glTF is judged in a
//!   `character` part). Each must get the verdict and the code `vectors.json` gives.
//! - Ours ([`hostile`]): one release per refusal, each beside its near miss that passes.
//!
//! An answer is `(code, key)` per refusal in order: `key` is `content-format`'s and
//! `pack-inbox-invalid`'s `reason`, `content-licence-refused`'s `meta`, a parse refusal's `field`
//! or `key`, and the `path` otherwise; flags are answered the same way, `content-astrachar-unreadable` by its `reason`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use astra_game_launcher::{GiPackage, content};
use serde_json::Value;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::common::sha;
use crate::spec_goldens::{root, vectors};

pub type Answer = Vec<(String, String)>;

/// A release's files: `(name, bytes)`.
pub type Files = Vec<(String, Vec<u8>)>;

/// The answer's key of one refusal or flag (the module doc).
pub fn key_of(code: &str, params: &BTreeMap<String, String>) -> (String, String) {
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();
    let key = match code {
        c if c == astra_game_launcher::codes::CONTENT_FORMAT => get("reason"),
        c if c == astra_game_launcher::codes::CONTENT_LICENCE_REFUSED => get("meta"),
        c if c == content::FLAG_ASTRACHAR_UNREADABLE => get("reason"),
        c if c == astra_game_launcher::codes::PACK_INBOX_INVALID => get("reason"),
        _ => params
            .get("path")
            .or_else(|| params.get("field"))
            .or_else(|| params.get("key"))
            .cloned()
            .unwrap_or_default(),
    };
    (code.to_owned(), key)
}

/// A zip of `files`, deflated, in the order given.
pub fn zip_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (n, b) in files {
        w.start_file(n.as_str(), opts).unwrap();
        w.write_all(b).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// Every file below `dir`, `/`-separated relative names, sorted.
pub fn files_below(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_str().unwrap();
                out.push((rel.replace('\\', "/"), fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// What the library makes of a release zip: `GiPackage::open`'s refusal, else
/// `GiPackage::content`'s refusals; and the flags.
pub fn library_answer(zip: &[u8]) -> (Answer, Answer, Option<content::Report>) {
    match GiPackage::open(zip, &sha(zip)) {
        Err(e) => (vec![key_of(e.code, &e.params)], Vec::new(), None),
        Ok(pkg) => {
            let r = pkg.content().unwrap();
            let refusals = r
                .refusals
                .iter()
                .map(|e| key_of(e.code, &e.params))
                .collect();
            let flags = r.flags.iter().map(|f| key_of(f.code, &f.params)).collect();
            (refusals, flags, Some(r))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The spec's goldens.
// ---------------------------------------------------------------------------------------------

/// `content.releases[]`: `(name, its folder's files, the launcher code a refusal must carry,
/// None for accepted)`.
pub fn release_vectors() -> Vec<(String, Files, Option<&'static str>)> {
    let list = vectors()["content"]["releases"].as_array().unwrap();
    assert!(list.len() >= 9, "{} release vectors", list.len());
    list.iter()
        .map(|v| {
            let dir = root()
                .join("testdata/games")
                .join(v["dir"].as_str().unwrap());
            let files = files_below(&dir);
            assert!(
                files.iter().any(|(n, _)| n == "astra-gi.toml"),
                "{}",
                dir.display()
            );
            let code = match v["verdict"].as_str().unwrap() {
                "accept" => None,
                "refuse" => {
                    let spec = v["code"].as_str().unwrap();
                    Some(
                        content::launcher_code(spec)
                            .unwrap_or_else(|| panic!("{spec}: no launcher code")),
                    )
                }
                other => panic!("a release verdict this test does not read: {other}"),
            };
            (v["name"].as_str().unwrap().to_owned(), files, code)
        })
        .collect()
}

/// What `vectors.json` asks of one `content.vrm_gate[]` file.
#[derive(Debug, Clone, PartialEq)]
pub enum GateWant {
    Accept,
    /// Refused: the deciding meta field, its value, whether a default decided.
    Refuse(String, Value, bool),
    /// Unknown: the deciding reading, when one decided.
    Unknown(Option<(String, Value)>),
    Conflict,
    /// `E_CONTENT_FORMAT`: the launcher's `reason` for the vector's `detail`.
    Format(&'static str),
}

/// The `reason` for each `detail` the vectors name a format rule by.
pub const FORMAT_DETAILS: &[(&str, &str)] = &[
    ("twice", "duplicate-member"),
    ("past the end", "chunk-past-end"),
    ("header says", "length-mismatch"),
    ("external uri", "external-uri"),
];

/// `content.vrm_gate[]`: `(name, the file's name, its bytes, what is wanted)`.
pub fn gate_vectors() -> Vec<(String, String, Vec<u8>, GateWant)> {
    let list = vectors()["content"]["vrm_gate"].as_array().unwrap();
    assert!(list.len() >= 25, "{} gate vectors", list.len());
    list.iter()
        .map(|v| {
            let file = v["file"].as_str().unwrap();
            let bytes = fs::read(root().join("testdata/games").join(file)).unwrap();
            let field = v.get("field").and_then(Value::as_str).map(str::to_owned);
            let value = v.get("value").cloned().unwrap_or(Value::Null);
            let want = match v["verdict"].as_str().unwrap() {
                "accept" => GateWant::Accept,
                "refuse" => GateWant::Refuse(
                    field.expect("a refusal names its field"),
                    value,
                    v.get("defaulted").and_then(Value::as_bool).unwrap_or(false),
                ),
                "unknown" => GateWant::Unknown(field.map(|f| (f, value))),
                "conflict" => GateWant::Conflict,
                "format" => {
                    let detail = v["detail"].as_str().unwrap();
                    let (_, reason) = FORMAT_DETAILS
                        .iter()
                        .find(|(d, _)| *d == detail)
                        .unwrap_or_else(|| panic!("{file}: a format detail {detail:?} not mapped"));
                    GateWant::Format(reason)
                }
                other => panic!("{file}: a gate verdict this test does not read: {other}"),
            };
            let name = file.rsplit('/').next().unwrap().to_owned();
            (v["name"].as_str().unwrap().to_owned(), name, bytes, want)
        })
        .collect()
}

/// A community runner of `parts` (`[[integration.content]]` tables, TOML).
pub fn manifest(parts: &str) -> String {
    format!(
        r#"schema = 1

[integration]
id = "someone.content-fixture"
name = "Content fixture"
version = "0.1.0"
license = "MIT"

{parts}
[target]
exe = "*"
platforms = ["windows-x64"]
anti_cheat = "none"
"#
    )
}

/// One `[[integration.content]]` table.
pub fn part(path: &str, kind: &str, licence: Option<&str>) -> String {
    let licence = licence.map_or(String::new(), |l| format!("licence = \"{l}\"\n"));
    format!(
        "[[integration.content]]\npath = \"{path}\"\nkind = \"{kind}\"\ntitle = \"A part\"\ncredit = \"someone\"\n{licence}\n"
    )
}

/// The release of one `character` part holding a gate vector's bytes, its licence declared.
pub fn gate_release(name: &str, bytes: &[u8]) -> Vec<u8> {
    let path = format!("character/{name}");
    zip_of(&[
        (
            "astra-gi.toml".to_owned(),
            manifest(&part(&path, "character", Some("CC-BY-4.0"))).into_bytes(),
        ),
        (path, bytes.to_vec()),
    ])
}

// ---------------------------------------------------------------------------------------------
// Our hostile fixtures.
// ---------------------------------------------------------------------------------------------

/// A GLB around `json` (padded with spaces, as the format pads), with `bin` as a second chunk.
pub fn glb_with(json: &str, bin: Option<&[u8]>) -> Vec<u8> {
    let mut j = json.as_bytes().to_vec();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut chunks = Vec::new();
    chunks.extend((j.len() as u32).to_le_bytes());
    chunks.extend(b"JSON");
    chunks.extend(&j);
    if let Some(bin) = bin {
        let mut b = bin.to_vec();
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        chunks.extend((b.len() as u32).to_le_bytes());
        chunks.extend(b"BIN\0");
        chunks.extend(b);
    }
    let mut out = b"glTF".to_vec();
    out.extend(2u32.to_le_bytes());
    out.extend(((12 + chunks.len()) as u32).to_le_bytes());
    out.extend(chunks);
    out
}

pub fn glb(json: &str) -> Vec<u8> {
    glb_with(json, None)
}

/// A VRM 1.0 avatar whose meta is `meta` (JSON).
pub fn vrm1(meta: &str) -> Vec<u8> {
    glb(&format!(
        r#"{{"asset":{{"version":"2.0"}},"extensionsUsed":["VRMC_vrm"],"extensions":{{"VRMC_vrm":{{"specVersion":"1.0","meta":{meta}}}}}}}"#
    ))
}

/// An avatar carrying a VRM 0.x meta `meta0` and an allowing VRM 1.0 one.
pub fn both_metas(meta0: &str) -> Vec<u8> {
    glb(&format!(
        r#"{{"asset":{{"version":"2.0"}},"extensions":{{"VRM":{{"meta":{meta0}}},"VRMC_vrm":{{"specVersion":"1.0","meta":{VRM1_ALLOWED}}}}}}}"#
    ))
}

pub const VRM1_ALLOWED: &str = r#"{"name":"A","authors":["someone"],"allowRedistribution":true,"avatarPermission":"everyone"}"#;
pub const VRM1_PROHIBITED: &str = r#"{"name":"A","authors":["someone"],"allowRedistribution":false,"avatarPermission":"everyone"}"#;

/// An animation: `VRMC_vrm_animation` and no avatar extension.
pub fn vrma() -> Vec<u8> {
    glb(
        r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRMC_vrm_animation"],"extensions":{"VRMC_vrm_animation":{"specVersion":"1.0"}}}"#,
    )
}

/// A binary FBX's first bytes.
pub const FBX: &[u8] = b"Kaydara FBX Binary  \0\x1a\0\xe8\x1c\0\0";
/// A PNG's first bytes.
pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
/// A Windows DLL's first bytes.
pub const DLL: &[u8] = b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0\0\xb8\0\0\0\0\0\0\0@\0\0\0\0\0\0\0";

/// An `.astrachar` bundle: `astrachar.json` and `files`.
pub fn astrachar(json: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut all: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, b)| ((*n).to_owned(), b.to_vec()))
        .collect();
    all.push(("astrachar.json".to_owned(), json.as_bytes().to_vec()));
    zip_of(&all)
}

/// A `[[files]]` rule (TOML), for a destination such as `${astra_pack_inbox}/`.
pub fn inbox_rule(from: &str, to: &str) -> String {
    format!("[[files]]\nfrom = \"{from}\"\nto = \"{to}\"\n\n")
}

/// An animation pack of this fixture's integration, `<id>/<tail>`, holding one `.vrma`.
pub fn animpack(tail: &str) -> Vec<u8> {
    zip_of(&[
        (
            "animpack.toml".to_owned(),
            format!("[animpack]\nuid = \"someone.content-fixture/{tail}\"\n").into_bytes(),
        ),
        ("clips/climb.vrma".to_owned(), vrma()),
    ])
}

/// An `.astrachar` bundle written byte by byte: `astrachar.json` and an allowed VRM 1.0 body, and
/// `notes.bin`, a stored entry with bit 3 whose data hides a descriptor (the stored-dd trick) —
/// with `flags` (encrypted: 1) and `method` (another: 12) as given, a plain one otherwise.
pub fn raw_bundle(flags: Option<u16>, method: Option<u16>) -> Vec<u8> {
    use crate::common::rawzip::{Raw, archive, descriptor};
    let mut hidden = b"x".to_vec();
    hidden.extend_from_slice(&descriptor(crc32fast::hash(b"x"), 1, 1));
    hidden.extend_from_slice(b"PK\x03\x04 a header for a reader of local headers");
    let mut notes = Raw::streamed("notes.bin", b"plain notes");
    if flags.is_some() || method.is_some() {
        notes = Raw::streamed("notes.bin", &hidden);
        notes.flags |= flags.unwrap_or(0);
        notes.method = method.unwrap_or(0);
    }
    archive(&[
        Raw::stored(
            "astrachar.json",
            astrachar_json(r#"{"file":"models/Mira.vrm"}"#).as_bytes(),
        ),
        Raw::stored("models/Mira.vrm", &vrm1(VRM1_ALLOWED)),
        notes,
    ])
}

pub fn astrachar_json(body: &str) -> String {
    format!(
        r#"{{"format":"astra.character","format_version":1,"needs":[],"schema":5,"kind":"vrm","character":{{"id":"mira","name":"Mira","dir":"characters/mira"}},"body":{body}}}"#
    )
}

/// One hostile release: its files, what is refused, what is flagged; and its near miss, a
/// release that differs in the one thing refused and passes (`None` when the refusal is the
/// rule's only shape: a parse refusal of a value the spec forbids).
pub struct Fixture {
    pub name: &'static str,
    pub files: Vec<(String, Vec<u8>)>,
    pub refused: Vec<(&'static str, String)>,
    pub flagged: Vec<(&'static str, String)>,
    pub near_miss: Option<Vec<(String, Vec<u8>)>>,
    /// The near miss's flags.
    pub near_miss_flagged: Vec<(&'static str, String)>,
}

fn rel(manifest_parts: &str, files: &[(&str, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![(
        "astra-gi.toml".to_owned(),
        manifest(manifest_parts).into_bytes(),
    )];
    out.extend(files.iter().map(|(n, b)| ((*n).to_owned(), b.clone())));
    out
}

pub fn hostile() -> Vec<Fixture> {
    use astra_game_launcher::codes::*;
    let cc = Some("CC-BY-4.0");
    let fx = |name, files, refused: Vec<(&'static str, &str)>, near_miss| Fixture {
        name,
        files,
        refused: refused
            .into_iter()
            .map(|(c, k)| (c, k.to_owned()))
            .collect(),
        flagged: Vec::new(),
        near_miss,
        near_miss_flagged: Vec::new(),
    };
    let gltf_with_uri = |uri: &str| {
        format!(r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":4,"uri":"{uri}"}}]}}"#)
            .into_bytes()
    };
    let cloaked = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zip-cloaking/upath.zip"),
    )
    .unwrap();
    let mut out = vec![
        fx(
            "a picture no entry declares",
            rel(
                &part("art/cover.png", "art", cc),
                &[
                    ("art/cover.png", PNG.to_vec()),
                    ("art/poster.png", PNG.to_vec()),
                ],
            ),
            vec![(CONTENT_UNDECLARED, "art/poster.png")],
            Some(rel(
                &(part("art/cover.png", "art", cc) + &part("art/poster.png", "art", cc)),
                &[
                    ("art/cover.png", PNG.to_vec()),
                    ("art/poster.png", PNG.to_vec()),
                ],
            )),
        ),
        fx(
            "a loader DLL named as the animation it declares covers nothing",
            rel(
                &part("animations/climb.fbx", "animations", cc),
                &[("animations/climb.fbx", DLL.to_vec())],
            ),
            vec![(CONTENT_MISSING, "animations/climb.fbx")],
            Some(rel(
                &part("animations/climb.fbx", "animations", cc),
                &[("animations/climb.fbx", FBX.to_vec())],
            )),
        ),
        fx(
            "an entry for a file the release does not hold",
            rel(&part("audio/theme.ogg", "audio", cc), &[]),
            vec![(CONTENT_MISSING, "audio/theme.ogg")],
            None,
        ),
        fx(
            "one path declared twice",
            rel(
                &(part("art/cover.png", "art", cc) + &part("art/cover.png", "art", cc)),
                &[("art/cover.png", PNG.to_vec())],
            ),
            vec![(CONTENT_DUPLICATE, "art/cover.png")],
            None,
        ),
        fx(
            "a builtin: reference declared as a part",
            rel(&part("builtin:nayu", "character", cc), &[]),
            vec![(MANIFEST_INVALID, "integration.content[0].path")],
            None,
        ),
        fx(
            "a path that climbs out with `..`",
            rel(&part("art/../../cover.png", "art", cc), &[]),
            vec![(MANIFEST_INVALID, "integration.content[0].path")],
            None,
        ),
        fx(
            "a key the entry does not have",
            rel(
                &part("art/cover.png", "art", cc).replace("credit", "sha256 = \"00\"\ncredit"),
                &[("art/cover.png", PNG.to_vec())],
            ),
            vec![(UNKNOWN_KEY, "integration.content[0].sha256")],
            None,
        ),
        fx(
            "an empty licence",
            rel(
                &part("art/cover.png", "art", Some("")),
                &[("art/cover.png", PNG.to_vec())],
            ),
            vec![(MANIFEST_INVALID, "integration.content[0].licence")],
            None,
        ),
        fx(
            "the reserved [[integration.uses]]",
            rel(
                "[[integration.uses]]\nkind = \"character\"\nid = \"someone/mira\"\nversion_req = \"=1.0.0\"\n",
                &[],
            ),
            vec![(UNKNOWN_KEY, "integration.uses")],
            None,
        ),
        fx(
            "a part with no licence of any kind",
            rel(
                &part("animations/climb.fbx", "animations", None),
                &[("animations/climb.fbx", FBX.to_vec())],
            ),
            vec![(CONTENT_LICENCE_UNDECLARED, "animations/climb.fbx")],
            Some(rel(
                &part("animations/climb.fbx", "animations", cc),
                &[("animations/climb.fbx", FBX.to_vec())],
            )),
        ),
        fx(
            "an avatar renamed as art is judged as an avatar",
            rel(
                &part("art/poster.glb", "art", cc),
                &[("art/poster.glb", vrm1(VRM1_PROHIBITED))],
            ),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(rel(
                &part("art/poster.glb", "art", cc),
                &[("art/poster.glb", vrm1(VRM1_ALLOWED))],
            )),
        ),
        fx(
            "an avatar renamed as an animation is judged as an avatar",
            rel(
                &part("animations/wave.vrma", "animations", cc),
                &[("animations/wave.vrma", vrm1(VRM1_PROHIBITED))],
            ),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(rel(
                &part("animations/wave.vrma", "animations", cc),
                &[("animations/wave.vrma", vrma())],
            )),
        ),
        fx(
            "an avatar whose two metas read differently",
            rel(
                &part("character/both.vrm", "character", cc),
                &[(
                    "character/both.vrm",
                    both_metas(
                        r#"{"licenseName":"Redistribution_Prohibited","allowedUserName":"Everyone"}"#,
                    ),
                )],
            ),
            vec![(CONTENT_LICENCE_CONFLICT, "character/both.vrm")],
            Some(rel(
                &part("character/both.vrm", "character", cc),
                &[(
                    "character/both.vrm",
                    both_metas(r#"{"licenseName":"CC0","allowedUserName":"Everyone"}"#),
                )],
            )),
        ),
        fx(
            "a GLB whose first chunk is not JSON",
            rel(
                &part("models/prop.glb", "art", cc),
                &[("models/prop.glb", {
                    let mut g = glb(r#"{"asset":{"version":"2.0"}}"#);
                    g[16..20].copy_from_slice(b"BIN\0");
                    g
                })],
            ),
            vec![(CONTENT_FORMAT, "first-chunk-not-json")],
            Some(rel(
                &part("models/prop.glb", "art", cc),
                &[("models/prop.glb", glb(r#"{"asset":{"version":"2.0"}}"#))],
            )),
        ),
        fx(
            "a GLB shorter than its headers",
            rel(
                &part("models/prop.glb", "art", cc),
                &[(
                    "models/prop.glb",
                    b"glTF\x02\0\0\0\x10\0\0\0\0\0\0\0".to_vec(),
                )],
            ),
            vec![(CONTENT_FORMAT, "truncated")],
            None,
        ),
        fx(
            "a GLB whose JSON does not parse",
            rel(
                &part("models/prop.glb", "art", cc),
                &[("models/prop.glb", glb(r#"{"asset":{"version":"2.0"},}"#))],
            ),
            vec![(CONTENT_FORMAT, "json-invalid")],
            None,
        ),
        fx(
            "a GLB whose JSON is not an object",
            rel(
                &part("models/prop.glb", "art", cc),
                &[("models/prop.glb", glb("[]"))],
            ),
            vec![(CONTENT_FORMAT, "not-an-object")],
            None,
        ),
        fx(
            "a GLB naming a member twice deep inside its VRM meta",
            rel(
                &part("character/a.vrm", "character", cc),
                &[(
                    "character/a.vrm",
                    vrm1(
                        r#"{"allowRedistribution":true,"avatarPermission":"onlyAuthor","avatarPermission":"everyone"}"#,
                    ),
                )],
            ),
            vec![(CONTENT_FORMAT, "duplicate-member")],
            Some(rel(
                &part("character/a.vrm", "character", cc),
                &[("character/a.vrm", vrm1(VRM1_ALLOWED))],
            )),
        ),
        fx(
            "a GLB whose image uri is a number",
            rel(
                &part("models/prop.glb", "art", cc),
                &[(
                    "models/prop.glb",
                    glb(r#"{"asset":{"version":"2.0"},"images":[{"uri":7}]}"#),
                )],
            ),
            vec![(CONTENT_FORMAT, "uri-not-a-string")],
            None,
        ),
        fx(
            "a .gltf buffer fetched from a URL",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[(
                    "models/prop.gltf",
                    gltf_with_uri("https://example.com/who-is-looking.bin"),
                )],
            ),
            vec![(CONTENT_FORMAT, "uri-not-relative")],
            Some(rel(
                &part("models/prop.gltf", "art", cc),
                &[
                    ("models/prop.gltf", gltf_with_uri("prop.bin")),
                    ("models/prop.bin", b"\0\0\0\0".to_vec()),
                ],
            )),
        ),
        fx(
            "a .gltf buffer at an absolute path",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", gltf_with_uri("/etc/passwd"))],
            ),
            vec![(CONTENT_FORMAT, "uri-not-relative")],
            None,
        ),
        fx(
            "a .gltf buffer behind a backslash",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", gltf_with_uri("sub\\\\prop.bin"))],
            ),
            vec![(CONTENT_FORMAT, "uri-not-relative")],
            None,
        ),
        fx(
            "a .gltf buffer the release does not hold",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", gltf_with_uri("gone.bin"))],
            ),
            vec![(CONTENT_FORMAT, "uri-not-in-release")],
            None,
        ),
        fx(
            "a character bundle whose body refuses redistribution",
            rel(
                &part("characters/mira.astrachar", "character", cc),
                &[(
                    "characters/mira.astrachar",
                    astrachar(
                        &astrachar_json(r#"{"file":"models/Mira.vrm","sha256":"x","size":1}"#),
                        &[("models/Mira.vrm", &vrm1(VRM1_PROHIBITED))],
                    ),
                )],
            ),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(rel(
                &part("characters/mira.astrachar", "character", cc),
                &[(
                    "characters/mira.astrachar",
                    astrachar(
                        &astrachar_json(r#"{"file":"models/Mira.vrm","sha256":"x","size":1}"#),
                        &[("models/Mira.vrm", &vrm1(VRM1_ALLOWED))],
                    ),
                )],
            )),
        ),
        fx(
            "a character bundle whose body is a GLB naming a remote image",
            rel(
                &part("characters/mira.astrachar", "character", cc),
                &[(
                    "characters/mira.astrachar",
                    astrachar(
                        &astrachar_json(r#"{"file":"models/Mira.vrm"}"#),
                        &[(
                            "models/Mira.vrm",
                            &glb(
                                r#"{"asset":{"version":"2.0"},"images":[{"uri":"https://x/y.png"}]}"#,
                            ),
                        )],
                    ),
                )],
            ),
            vec![(CONTENT_FORMAT, "external-uri")],
            None,
        ),
        fx(
            "a character bundle that reads as two archives",
            rel(
                &part("characters/mira.astrachar", "character", cc),
                &[("characters/mira.astrachar", cloaked)],
            ),
            vec![(CONTENT_FORMAT, "zip-inconsistent")],
            None,
        ),
        fx(
            "a character bundle holding an encrypted entry",
            rel(
                &part("characters/mira.astrachar", "character", cc),
                &[("characters/mira.astrachar", raw_bundle(Some(1), None))],
            ),
            vec![(CONTENT_FORMAT, "zip-unsafe-entry")],
            Some(rel(
                &part("characters/mira.astrachar", "character", cc),
                &[("characters/mira.astrachar", raw_bundle(None, None))],
            )),
        ),
        fx(
            "a character bundle holding an entry of another method",
            rel(
                &part("characters/mira.astrachar", "character", cc),
                &[("characters/mira.astrachar", raw_bundle(None, Some(12)))],
            ),
            vec![(CONTENT_FORMAT, "zip-unsafe-entry")],
            Some(rel(
                &part("characters/mira.astrachar", "character", cc),
                &[("characters/mira.astrachar", raw_bundle(None, None))],
            )),
        ),
        fx(
            "a pack for the companion read from a file of another name",
            rel(
                &inbox_rule("data/climb.bin", "${astra_pack_inbox}/climb.astrapack"),
                &[("data/climb.bin", animpack("climb"))],
            ),
            vec![(PACK_INBOX_INVALID, "not-content-typed")],
            Some(rel(
                &format!(
                    "{}{}",
                    part("packs/climb.astrapack", "animations", cc),
                    inbox_rule("packs/climb.astrapack", "${astra_pack_inbox}/")
                ),
                &[("packs/climb.astrapack", animpack("climb"))],
            )),
        ),
        fx(
            "a pack for the companion behind a prefix",
            rel(
                &format!(
                    "{}{}",
                    part("packs/climb.astrapack", "animations", cc),
                    inbox_rule("packs/**", "${astra_pack_inbox}/")
                ),
                &[("packs/climb.astrapack", {
                    let mut p = b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0".to_vec();
                    p.extend(animpack("climb"));
                    p
                })],
            ),
            vec![(PACK_INBOX_INVALID, "not-content-typed")],
            Some(rel(
                &format!(
                    "{}{}",
                    part("packs/climb.astrapack", "animations", cc),
                    inbox_rule("packs/**", "${astra_pack_inbox}/")
                ),
                &[("packs/climb.astrapack", animpack("climb"))],
            )),
        ),
        fx(
            "a pack for the companion no entry declares",
            rel(
                &inbox_rule("packs/**", "${astra_pack_inbox}/"),
                &[("packs/climb.astrapack", animpack("climb"))],
            ),
            vec![(CONTENT_UNDECLARED, "packs/climb.astrapack")],
            Some(rel(
                &format!(
                    "{}{}",
                    part("packs/climb.astrapack", "animations", cc),
                    inbox_rule("packs/**", "${astra_pack_inbox}/")
                ),
                &[("packs/climb.astrapack", animpack("climb"))],
            )),
        ),
        fx(
            "a character bundle naming its body twice",
            rel(
                &part("characters/mira.astrachar", "character", cc),
                &[(
                    "characters/mira.astrachar",
                    astrachar(
                        &astrachar_json(
                            r#"{"file":"models/Mira.vrm"},"body":{"file":"models/Other.vrm"}"#,
                        ),
                        &[
                            ("models/Mira.vrm", &vrm1(VRM1_ALLOWED)),
                            ("models/Other.vrm", &vrm1(VRM1_PROHIBITED)),
                        ],
                    ),
                )],
            ),
            vec![(CONTENT_FORMAT, "astrachar-json-duplicate-member")],
            None,
        ),
    ];
    // Flagged, never refused: a bundle whose body is a `builtin:` reference (it ships no bytes),
    // which then needs its declared licence — and passes with one.
    let builtin_body = astrachar(
        &astrachar_json(r#"{"ref":"builtin:nayu","sha256":"x"}"#),
        &[],
    );
    out.push(Fixture {
        name: "a character bundle whose body is a builtin: reference",
        files: rel(
            &part("characters/nayu.astrachar", "character", None),
            &[("characters/nayu.astrachar", builtin_body.clone())],
        ),
        refused: vec![(
            CONTENT_LICENCE_UNDECLARED,
            "characters/nayu.astrachar".to_owned(),
        )],
        flagged: vec![(
            content::FLAG_ASTRACHAR_UNREADABLE,
            "no-file-body".to_owned(),
        )],
        near_miss: Some(rel(
            &part("characters/nayu.astrachar", "character", cc),
            &[("characters/nayu.astrachar", builtin_body)],
        )),
        near_miss_flagged: vec![(
            content::FLAG_ASTRACHAR_UNREADABLE,
            "no-file-body".to_owned(),
        )],
    });
    // A plain glTF in a character part is judged, and reads unknown: flagged, and refused only
    // for want of the licence it then needs.
    let plain = glb(r#"{"asset":{"version":"2.0"}}"#);
    out.push(Fixture {
        name: "a plain glTF as a character, no licence declared",
        files: rel(
            &part("character/plain.glb", "character", None),
            &[("character/plain.glb", plain.clone())],
        ),
        refused: vec![(CONTENT_LICENCE_UNDECLARED, "character/plain.glb".to_owned())],
        flagged: vec![(
            content::FLAG_VRM_META_UNKNOWN,
            "character/plain.glb".to_owned(),
        )],
        near_miss: Some(rel(
            &part("character/plain.glb", "character", cc),
            &[("character/plain.glb", plain)],
        )),
        near_miss_flagged: vec![(
            content::FLAG_VRM_META_UNKNOWN,
            "character/plain.glb".to_owned(),
        )],
    });
    out
}

/// `fixture`'s near miss, when it has one, passes: no refusal, exactly its flags. And a near
/// miss that is the fixture itself is no near miss.
pub fn near_miss_failure(
    f: &Fixture,
    answer: impl Fn(&[u8]) -> (Answer, Answer),
) -> Option<String> {
    let files = f.near_miss.as_ref()?;
    if *files == f.files {
        return Some(format!("{}: the near miss is the fixture", f.name));
    }
    let (refused, flagged) = answer(&zip_of(files));
    let want_flags: Answer = f
        .near_miss_flagged
        .iter()
        .map(|(c, k)| ((*c).to_owned(), k.clone()))
        .collect();
    (!refused.is_empty() || flagged != want_flags).then(|| {
        format!(
            "{}: the near miss is answered {refused:?} flagged {flagged:?}",
            f.name
        )
    })
}

/// `fixture` answered as it says.
pub fn fixture_failure(f: &Fixture, refused: &Answer, flagged: &Answer) -> Option<String> {
    let want: Answer = f
        .refused
        .iter()
        .map(|(c, k)| ((*c).to_owned(), k.clone()))
        .collect();
    let want_flags: Answer = f
        .flagged
        .iter()
        .map(|(c, k)| ((*c).to_owned(), k.clone()))
        .collect();
    (*refused != want || *flagged != want_flags).then(|| {
        format!(
            "{}: answered {refused:?} flagged {flagged:?}; wants {want:?} flagged {want_flags:?}",
            f.name
        )
    })
}
