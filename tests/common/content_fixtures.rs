// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `[[integration.content]]`'s goldens and hostile fixtures, judged one way for `tests/content.rs`
//! (the library: `GiPackage::open` and `GiPackage::content`) and `tests/gi_probe.rs` (`gi-probe
//! check`, step 1 and step 8).
//!
//! - The spec's: every `testdata/games/content/releases/` folder, zipped as it is — with the
//!   files too large to commit (`synth`) built here as AstraPlugins'
//!   `testdata/games/content/vrm/gen.py` builds them and held to the sha256 `vectors.json` pins —
//!   and every `testdata/games/content/vrm/` file in a release of one `character` part with a
//!   declared licence, which makes the part's verdict exactly the gate's (a plain glTF is judged
//!   in a `character` part). Each must get the verdict, the code and the flags `vectors.json`
//!   gives.
//! - Ours ([`hostile`]): one release per refusal, each beside its near miss that passes.
//!
//! An answer is `(code, key)` per refusal in order: `key` is `content-format`'s and
//! `pack-inbox-invalid`'s `reason`, `content-licence-refused`'s `meta`, a parse refusal's `field`
//! or `key`, and the `path` otherwise; flags are answered the same way.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use astra_game_launcher::{GiPackage, content};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::common::sha;
use crate::spec_goldens::{root, vectors};

pub type Answer = Vec<(String, String)>;

/// A release's files: `(name, bytes)`.
pub type Files = Vec<(String, Vec<u8>)>;

const MIB: usize = 1024 * 1024;

/// The answer's key of one refusal or flag (the module doc).
pub fn key_of(code: &str, params: &BTreeMap<String, String>) -> (String, String) {
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();
    let key = match code {
        c if c == astra_game_launcher::codes::CONTENT_FORMAT => get("reason"),
        c if c == astra_game_launcher::codes::CONTENT_LICENCE_REFUSED => get("meta"),
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

/// One `content.releases[]` record.
pub struct ReleaseVector {
    pub name: String,
    /// Its folder's files, and the ones `synth` builds.
    pub files: Files,
    /// The launcher code a refusal must carry, `None` when it is accepted.
    pub code: Option<&'static str>,
    /// The launcher's spelling of every flag it must carry.
    pub flags: Vec<&'static str>,
    /// `guard` = `strict_json`: the `.gltf` the games checker's own strict parser must refuse.
    pub strict_json_guard: bool,
}

/// `content.releases[]`.
pub fn release_vectors() -> Vec<ReleaseVector> {
    let list = vectors()["content"]["releases"].as_array().unwrap();
    assert!(list.len() >= 49, "{} release vectors", list.len());
    let code_of = |spec: &str| {
        content::launcher_code(spec).unwrap_or_else(|| panic!("{spec}: no launcher code"))
    };
    list.iter()
        .map(|v| {
            let name = v["name"].as_str().unwrap().to_owned();
            let dir = root()
                .join("testdata/games")
                .join(v["dir"].as_str().unwrap());
            let mut files = files_below(&dir);
            assert!(
                files.iter().any(|(n, _)| n == "astra-gi.toml"),
                "{}",
                dir.display()
            );
            if let Some(synth) = v.get("synth").and_then(Value::as_object) {
                for (rel, which) in synth {
                    let bytes = synth_golden(which.as_str().unwrap());
                    assert_eq!(
                        hex::encode(Sha256::digest(&bytes)),
                        v["sha256"][rel].as_str().unwrap(),
                        "{name}: {rel} is not the bytes AstraPlugins' gen.py builds"
                    );
                    files.push((rel.clone(), bytes));
                }
                files.sort();
            }
            let code = match v["verdict"].as_str().unwrap() {
                "accept" => None,
                "refuse" => Some(code_of(v["code"].as_str().unwrap())),
                other => panic!("a release verdict this test does not read: {other}"),
            };
            let flags = v
                .get("flags")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|f| code_of(f.as_str().unwrap())).collect())
                .unwrap_or_default();
            ReleaseVector {
                name,
                files,
                code,
                flags,
                strict_json_guard: v.get("guard").and_then(Value::as_str) == Some("strict_json"),
            }
        })
        .collect()
}

/// Whether a release's answer is what its vector asks: exactly the one code (however many files
/// carry it), or none; and exactly its flags.
pub fn release_answer_agrees(v: &ReleaseVector, refused: &Answer, flagged: &Answer) -> bool {
    let mut codes: Vec<&str> = refused.iter().map(|(c, _)| c.as_str()).collect();
    codes.sort();
    codes.dedup();
    let mut flags: Vec<&str> = flagged.iter().map(|(c, _)| c.as_str()).collect();
    flags.sort();
    flags.dedup();
    let mut want_flags = v.flags.clone();
    want_flags.sort();
    codes == v.code.into_iter().collect::<Vec<_>>() && flags == want_flags
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

// ---------------------------------------------------------------------------------------------
// The goldens too large to commit, as AstraPlugins builds them.
// ---------------------------------------------------------------------------------------------

/// `json.dumps({"asset": {"version": "2.0"}})`.
pub const PLAIN_GLTF: &[u8] = br#"{"asset": {"version": "2.0"}}"#;

/// gen.py's `padded_glb`: a whole GLB whose one JSON chunk is a plain glTF padded with spaces to
/// `chunk_len` bytes.
pub fn padded_glb(chunk_len: usize) -> Vec<u8> {
    let mut chunk = PLAIN_GLTF.to_vec();
    chunk.resize(chunk_len, b' ');
    let mut out = b"glTF".to_vec();
    out.extend(2u32.to_le_bytes());
    out.extend(((20 + chunk.len()) as u32).to_le_bytes());
    out.extend((chunk.len() as u32).to_le_bytes());
    out.extend(b"JSON");
    out.extend(chunk);
    out
}

/// gen.py's `deep_gltf`: a `.gltf` whose JSON nests exactly `depth` deep.
pub fn deep_gltf(depth: usize) -> Vec<u8> {
    let mut out = br#"{"asset": {"version": "2.0"}, "extras": "#.to_vec();
    out.extend(std::iter::repeat_n(b'[', depth - 1));
    out.extend(std::iter::repeat_n(b']', depth - 1));
    out.push(b'}');
    out
}

/// The content catalogue's `_big_json`: a valid glTF JSON object of exactly `size` bytes.
pub fn big_json(size: usize) -> Vec<u8> {
    let (head, tail) = (&br#"{"asset":{"version":"2.0"},"pad":""#[..], &br#""}"#[..]);
    let mut out = head.to_vec();
    out.extend(std::iter::repeat_n(b'a', size - head.len() - tail.len()));
    out.extend(tail);
    out
}

/// The content catalogue's `ASCII_PART`: a pack's own files.
pub fn ascii_part() -> Vec<(String, Vec<u8>)> {
    vec![
        ("animpack.toml".to_owned(), b"name = \"wave\"\n".to_vec()),
        ("clips/a.fbx".to_owned(), b"FBX-A".to_vec()),
        ("clips/b.fbx".to_owned(), b"FBX-B".to_vec()),
    ]
}

/// One `synth` golden of `vectors.json`, by its name (gen.py's `SYNTH`).
pub fn synth_golden(name: &str) -> Vec<u8> {
    let x16 = || std::iter::repeat_n(b'x', 16 * MIB);
    match name {
        "json-over-16mib-nested-asset" => {
            let mut b = br#"{"meta": {"asset": {}}, "strings": ""#.to_vec();
            b.extend(x16());
            b.extend(br#""}"#);
            b
        }
        "json-over-16mib-plain" => {
            let mut b = br#"{"strings": ""#.to_vec();
            b.extend(x16());
            b.extend(br#""}"#);
            b
        }
        "pak-utf16-bom" => {
            let mut b = b"\xff\xfe".to_vec();
            b.extend(std::iter::repeat_n([0x00u8, 0xd8], 16 * MIB).flatten());
            b
        }
        "json-16mib-many-keys" => {
            let mut b = b"{".to_vec();
            for _ in 0..(16 * MIB / 5 - 1) {
                b.extend(br#""":0,"#);
            }
            b.extend(br#""":0}"#);
            b
        }
        "glb-json-over-16mib" => padded_glb(16 * MIB + 4),
        "gltf-over-16mib" => {
            let mut b = PLAIN_GLTF.to_vec();
            b.extend(std::iter::repeat_n(b' ', 16 * MIB));
            b
        }
        "glb-json-11mib" => padded_glb(11 * MIB),
        "gltf-million-deep" => deep_gltf(1_000_000),
        "jar-5000-entries" => games_zip(
            &(0..5000)
                .map(|i| Z::new(&format!("assets/lang/{i}.txt"), b"x"))
                .collect::<Vec<_>>(),
            b"",
        ),
        "astrapack-gltf-11mib" => {
            let mut files = ascii_part();
            files.push(("clips/g.gltf".to_owned(), big_json(11 * MIB)));
            content_gen_bundle("astra.animpack", &files, None)
        }
        other => panic!("a synth golden this test cannot build: {other}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Zips as AstraPlugins' games goldens write them, and what a fixture changes of one.
// ---------------------------------------------------------------------------------------------

/// One entry of [`games_zip`] (gen.py's `zip_of`): Stored, or Deflate as stored blocks, a regular
/// file of a unix host; and what a fixture changes.
#[derive(Debug, Clone)]
pub struct Z {
    pub name: String,
    pub data: Vec<u8>,
    /// 0 Stored, 8 Deflate (stored blocks, unless `comp` gives the bytes), any other written as
    /// it is with `data` as its bytes.
    pub method: u16,
    pub extra: Vec<u8>,
    /// Bytes before its local header (a gap, where a cloaked entry hides).
    pub pad_before: Vec<u8>,
    /// OR'd into both headers' flags (1: encrypted).
    pub flags: u16,
    pub made_by: u16,
    pub external: u32,
    /// The uncompressed size both headers record, when it is not `data`'s.
    pub size: Option<u32>,
    /// The compressed bytes, when they are not what `method` makes of `data`.
    pub comp: Option<Vec<u8>>,
}

impl Z {
    pub fn new(name: &str, data: &[u8]) -> Z {
        Z {
            name: name.to_owned(),
            data: data.to_vec(),
            method: 0,
            extra: Vec::new(),
            pad_before: Vec::new(),
            flags: 0,
            made_by: (3 << 8) | 20,
            external: 0o100644 << 16,
            size: None,
            comp: None,
        }
    }

    pub fn deflated(name: &str, data: &[u8]) -> Z {
        Z {
            method: 8,
            ..Z::new(name, data)
        }
    }
}

/// gen.py's `stored_deflate`: a Deflate stream of stored blocks only.
pub fn stored_deflate(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return b"\x01\x00\x00\xff\xff".to_vec();
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let block = &data[at..(at + 0xFFFF).min(data.len())];
        out.push(u8::from(at + 0xFFFF >= data.len()));
        out.extend((block.len() as u16).to_le_bytes());
        out.extend((block.len() as u16 ^ 0xFFFF).to_le_bytes());
        out.extend(block);
        at += 0xFFFF;
    }
    out
}

/// A real deflate stream of `data`.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// gen.py's `zip_of`, `trailing` after its end record.
pub fn games_zip(entries: &[Z], trailing: &[u8]) -> Vec<u8> {
    const DOS_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for e in entries {
        out.extend(&e.pad_before);
        let off = out.len() as u32;
        let name = e.name.as_bytes();
        let comp = e.comp.clone().unwrap_or_else(|| match e.method {
            8 => stored_deflate(&e.data),
            _ => e.data.clone(),
        });
        let crc = crc32fast::hash(&e.data);
        let size = e.size.unwrap_or(e.data.len() as u32);
        let flags = e.flags
            | if name.iter().any(|b| *b > 0x7F) {
                0x800
            } else {
                0
            };
        let le16 = |v: &mut Vec<u8>, x: u16| v.extend(x.to_le_bytes());
        let le32 = |v: &mut Vec<u8>, x: u32| v.extend(x.to_le_bytes());
        le32(&mut out, 0x0403_4B50);
        for x in [20, flags, e.method, 0, DOS_DATE] {
            le16(&mut out, x);
        }
        for x in [crc, comp.len() as u32, size] {
            le32(&mut out, x);
        }
        le16(&mut out, name.len() as u16);
        le16(&mut out, e.extra.len() as u16);
        out.extend(name);
        out.extend(&e.extra);
        out.extend(&comp);
        le32(&mut central, 0x0201_4B50);
        for x in [e.made_by, 20, flags, e.method, 0, DOS_DATE] {
            le16(&mut central, x);
        }
        for x in [crc, comp.len() as u32, size] {
            le32(&mut central, x);
        }
        for x in [name.len() as u16, e.extra.len() as u16, 0, 0, 0] {
            le16(&mut central, x);
        }
        le32(&mut central, e.external);
        le32(&mut central, off);
        central.extend(name);
        central.extend(&e.extra);
    }
    let cd_off = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4B50u32.to_le_bytes());
    for x in [0u16, 0, entries.len() as u16, entries.len() as u16] {
        out.extend(x.to_le_bytes());
    }
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_off.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(trailing);
    out
}

/// The content catalogue's own `astrachar.json` (its gen.py's `manifest`): `format`, the
/// character `aiko`, `body` and every file listed with its digest and size.
pub fn content_gen_manifest(
    format: &str,
    files: &[(String, Vec<u8>)],
    body: Option<&str>,
) -> Value {
    let mut listed = serde_json::Map::new();
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (p, b) in sorted {
        listed.insert(
            p.clone(),
            json!({ "sha256": format!("sha256:{}", hex::encode(Sha256::digest(b))), "size": b.len() }),
        );
    }
    // Keys inserted sorted, as json.dumps(sort_keys=True) writes them, whatever map serde_json uses.
    let mut m = serde_json::Map::new();
    m.insert("accessories".to_owned(), json!([]));
    m.insert(
        "body".to_owned(),
        body.map_or(Value::Null, |b| json!({ "file": b })),
    );
    m.insert("builtins".to_owned(), json!({}));
    m.insert(
        "character".to_owned(),
        json!({ "dir": "characters/aiko", "id": "aiko", "name": "Aiko" }),
    );
    m.insert("excluded".to_owned(), json!([]));
    m.insert("files".to_owned(), Value::Object(listed));
    m.insert("format".to_owned(), json!(format));
    m.insert("format_version".to_owned(), json!(1));
    m.insert("generator".to_owned(), json!("testdata/content/gen.py"));
    m.insert("kind".to_owned(), json!("vrm"));
    m.insert("needs".to_owned(), json!([]));
    m.insert("packs".to_owned(), json!([]));
    m.insert("schema".to_owned(), json!(5));
    Value::Object(m)
}

/// `json.dumps(m, ensure_ascii=False, indent=1, sort_keys=True) + "\n"`.
pub fn python_json(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(b" "),
    );
    serde::Serialize::serialize(v, &mut ser).unwrap();
    out.push(b'\n');
    out
}

/// gen.py's `stored_bundle`: the content catalogue's bundle — its files sorted, then its
/// `astrachar.json` — every entry Stored.
pub fn content_gen_bundle(
    format: &str,
    files: &[(String, Vec<u8>)],
    body: Option<&str>,
) -> Vec<u8> {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut entries: Vec<Z> = sorted.iter().map(|(n, b)| Z::new(n, b)).collect();
    entries.push(Z::new(
        "astrachar.json",
        &python_json(&content_gen_manifest(format, files, body)),
    ));
    games_zip(&entries, b"")
}

/// A content bundle of `files`, as [`content_gen_bundle`] with `tweak` applied to its entries
/// (the manifest last) before they are written.
pub fn bundle_with(
    format: &str,
    files: &[(&str, &[u8])],
    body: Option<&str>,
    tweak: impl FnOnce(&mut Vec<Z>),
) -> Vec<u8> {
    let owned: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, b)| ((*n).to_owned(), b.to_vec()))
        .collect();
    let mut entries: Vec<Z> = owned.iter().map(|(n, b)| Z::new(n, b)).collect();
    entries.push(Z::new(
        "astrachar.json",
        &python_json(&content_gen_manifest(format, &owned, body)),
    ));
    tweak(&mut entries);
    games_zip(&entries, b"")
}

/// A character bundle of `files` whose body is `body`.
pub fn character(files: &[(&str, &[u8])], body: Option<&str>) -> Vec<u8> {
    bundle_with("astra.character", files, body, |_| {})
}

/// An animation pack of `files`.
pub fn pack(files: &[(&str, &[u8])]) -> Vec<u8> {
    bundle_with("astra.animpack", files, None, |_| {})
}

/// A character bundle whose `astrachar.json` is `manifest` (the files listed as they are).
pub fn character_manifest(files: &[(&str, &[u8])], manifest: impl FnOnce(&mut Value)) -> Vec<u8> {
    let owned: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, b)| ((*n).to_owned(), b.to_vec()))
        .collect();
    let mut m = content_gen_manifest("astra.character", &owned, Some(BODY));
    manifest(&mut m);
    let mut entries: Vec<Z> = owned.iter().map(|(n, b)| Z::new(n, b)).collect();
    entries.push(Z::new("astrachar.json", &python_json(&m)));
    games_zip(&entries, b"")
}

/// The body path of the fixtures' character bundles.
pub const BODY: &str = "characters/aiko/body.vrm";

/// A mod jar (gen.py's `avatar_jar`): a manifest and a Deflated `.png`, and `model` Stored at
/// `name`.
pub fn mod_jar(model: &[u8], name: &str) -> Vec<u8> {
    games_zip(&mod_jar_entries(model, name), b"")
}

pub fn mod_jar_entries(model: &[u8], name: &str) -> Vec<Z> {
    vec![
        Z::deflated(
            "META-INF/MANIFEST.MF",
            b"Manifest-Version: 1.0\r\nCreated-By: gen.py\r\n",
        ),
        Z::deflated("assets/logo.png", PNG16),
        Z::new(name, model),
    ]
}

/// A PNG's first 16 bytes.
pub const PNG16: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0\0\0\0\0";

/// The magic of an Ogg file and some bytes.
pub const OGG: &[u8] = b"OggS\0\x02\0\0\0\0\0\0\0\0";

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
pub const VRM0_OTHER: &str = r#"{"licenseName":"Other","allowedUserName":"Everyone"}"#;

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

/// A `[[files]]` rule (TOML), for a destination such as `${astra_pack_inbox}/`.
pub fn inbox_rule(from: &str, to: &str) -> String {
    format!("[[files]]\nfrom = \"{from}\"\nto = \"{to}\"\n\n")
}

/// An animation pack of this fixture's integration, `<id>/<tail>`, holding one `.vrma`: a content
/// bundle (`astrachar.json` last) the companion's inbox also reads (`animpack.toml`).
pub fn animpack(tail: &str) -> Vec<u8> {
    astrapack(format!("[animpack]\nuid = \"someone.content-fixture/{tail}\"\n").as_bytes())
}

/// An animation pack whose `animpack.toml` is `toml`, holding one `.vrma`.
pub fn astrapack(toml: &[u8]) -> Vec<u8> {
    pack(&[("animpack.toml", toml), ("clips/climb.vrma", &vrma())])
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

fn utf16le(s: &str) -> Vec<u8> {
    let mut out = b"\xff\xfe".to_vec();
    for u in s.encode_utf16() {
        out.extend(u.to_le_bytes());
    }
    out
}

/// A `.gltf` whose one buffer is at `uri`.
fn gltf_with_uri(uri: &str) -> Vec<u8> {
    format!(r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":4,"uri":"{uri}"}}]}}"#)
        .into_bytes()
}

/// A JSON object of `size` bytes naming `asset` only below the top level.
fn nested_asset_json(size: usize) -> Vec<u8> {
    let mut b = br#"{"meta":{"asset":{}},"pad":""#.to_vec();
    b.resize(size - 2, b'x');
    b.extend(br#""}"#);
    b
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
    let cloaked = fs::read(root().join("testdata/zip-cloaking/upath.zip")).unwrap();
    let allowed = vrm1(VRM1_ALLOWED);
    let prohibited = vrm1(VRM1_PROHIBITED);
    let plain = glb(r#"{"asset":{"version":"2.0"}}"#);
    // A release of one character bundle part, its licence `licence`.
    let char_release = |bundle: Vec<u8>, licence: Option<&str>| {
        rel(
            &part("characters/aiko.astrachar", "character", licence),
            &[("characters/aiko.astrachar", bundle)],
        )
    };
    let body = |b: &[u8]| character(&[(BODY, b)], Some(BODY));
    let good_char = || char_release(body(&allowed), cc);
    let bundle_part = "characters/aiko.astrachar";
    // A release of one mod jar part and the model it holds, declared at `<jar>!<inner>`.
    let jar_release = |jar: Vec<u8>, declared: Option<&str>| {
        let parts = declared.map_or(String::new(), |d| part(d, "character", cc));
        rel(&parts, &[("mods/avatar.jar", jar)])
    };
    let in_jar = "mods/avatar.jar!assets/avatar.vrm";

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
                &[("art/poster.glb", prohibited.clone())],
            ),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(rel(
                &part("art/poster.glb", "art", cc),
                &[("art/poster.glb", allowed.clone())],
            )),
        ),
        fx(
            "an avatar renamed as an animation is judged as an avatar",
            rel(
                &part("animations/wave.vrma", "animations", cc),
                &[("animations/wave.vrma", prohibited.clone())],
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
                    let mut g = plain.clone();
                    g[16..20].copy_from_slice(b"BIN\0");
                    g
                })],
            ),
            vec![(CONTENT_FORMAT, "first-chunk-not-json")],
            Some(rel(
                &part("models/prop.glb", "art", cc),
                &[("models/prop.glb", plain.clone())],
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
                &[("character/a.vrm", allowed.clone())],
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
            vec![(CONTENT_FORMAT, "uri-forbidden-character")],
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
        // -- by bytes, never by name (manifest.md §2.7.1) --
        fx(
            "a prohibited avatar renamed .bin",
            rel("", &[("data/avatar.bin", prohibited.clone())]),
            vec![(CONTENT_FORMAT, "glb-misnamed")],
            Some(rel(
                &part("data/avatar.glb", "character", cc),
                &[("data/avatar.glb", allowed.clone())],
            )),
        ),
        fx(
            "an avatar under a folder's astra-gi.toml",
            rel("", &[("models/astra-gi.toml", prohibited.clone())]),
            vec![(CONTENT_FORMAT, "glb-misnamed")],
            Some(rel(
                "",
                &[("models/astra-gi.toml", b"schema = 1\n".to_vec())],
            )),
        ),
        fx(
            "a text glTF renamed .json",
            rel(
                "",
                &[(
                    "data/avatar.json",
                    br#"{"asset":{"version":"2.0"},"scenes":[]}"#.to_vec(),
                )],
            ),
            vec![(CONTENT_FORMAT, "gltf-misnamed")],
            // A game's data file whose top-level asset is no object is not a glTF.
            Some(rel(
                "",
                &[(
                    "data/avatar.json",
                    br#"{"asset":"textures/hero.png","scenes":[]}"#.to_vec(),
                )],
            )),
        ),
        fx(
            "a UTF-16 glTF renamed .json",
            rel(
                "",
                &[(
                    "data/avatar.json",
                    utf16le(r#"{"asset":{"version":"2.0"}}"#),
                )],
            ),
            vec![(CONTENT_FORMAT, "gltf-misnamed")],
            Some(rel(
                "",
                &[(
                    "lang/strings.json",
                    utf16le(r#"{"greeting":"Привет","asset_note":{}}"#),
                )],
            )),
        ),
        fx(
            "a JSON over 16 MiB naming an asset object is refused unread",
            rel("", &[("data/big.json", nested_asset_json(16 * MIB + 2))]),
            vec![(CONTENT_FORMAT, "gltf-misnamed")],
            Some(rel("", &[("data/big.json", nested_asset_json(16 * MIB))])),
        ),
        fx(
            "past the release's 64 MiB of sniffing, a JSON naming an asset object is refused unread",
            rel(
                "",
                &[
                    ("data/a.json", nested_asset_json(15 * MIB)),
                    ("data/b.json", nested_asset_json(15 * MIB)),
                    ("data/c.json", nested_asset_json(15 * MIB)),
                    ("data/d.json", nested_asset_json(15 * MIB)),
                    ("data/e.json", nested_asset_json(15 * MIB)),
                ],
            ),
            vec![(CONTENT_FORMAT, "gltf-misnamed")],
            Some(rel(
                "",
                &[
                    ("data/a.json", nested_asset_json(15 * MIB)),
                    ("data/b.json", nested_asset_json(15 * MIB)),
                    ("data/c.json", nested_asset_json(15 * MIB)),
                    ("data/d.json", nested_asset_json(15 * MIB)),
                ],
            )),
        ),
        fx(
            "a content bundle renamed .zip",
            rel("", &[("packs/wave.zip", pack(&[("clips/a.fbx", b"FBX-A")]))]),
            vec![(CONTENT_FORMAT, "bundle-misnamed")],
            Some(rel(
                &part("packs/wave.astrapack", "animations", cc),
                &[("packs/wave.astrapack", pack(&[("clips/a.fbx", b"FBX-A")]))],
            )),
        ),
        // -- the format: the caps, the mark, the uri decoded once (§2.7.3) --
        fx(
            "a .gltf with a byte-order mark",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", {
                    let mut g = b"\xef\xbb\xbf".to_vec();
                    g.extend(PLAIN_GLTF);
                    g
                })],
            ),
            vec![(CONTENT_FORMAT, "bom")],
            Some(rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", PLAIN_GLTF.to_vec())],
            )),
        ),
        fx(
            "a .gltf over 16 MiB",
            rel(
                &part("models/big.gltf", "art", cc),
                &[("models/big.gltf", big_json(16 * MIB + 1))],
            ),
            vec![(CONTENT_FORMAT, "json-over-16mib")],
            Some(rel(
                &part("models/big.gltf", "art", cc),
                &[("models/big.gltf", big_json(16 * MIB))],
            )),
        ),
        fx(
            "a GLB whose JSON chunk is over 16 MiB",
            rel(
                &part("models/big.glb", "art", cc),
                &[("models/big.glb", padded_glb(16 * MIB + 4))],
            ),
            vec![(CONTENT_FORMAT, "json-over-16mib")],
            Some(rel(
                &part("models/big.glb", "art", cc),
                &[("models/big.glb", padded_glb(16 * MIB))],
            )),
        ),
        fx(
            "a .gltf nested 129 deep",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", deep_gltf(129))],
            ),
            vec![(CONTENT_FORMAT, "json-too-deep")],
            Some(rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", deep_gltf(128))],
            )),
        ),
        fx(
            "a release of more than 32 MiB of glTF JSON",
            rel(
                &(part("models/a.glb", "art", cc)
                    + &part("models/b.glb", "art", cc)
                    + &part("models/c.glb", "art", cc)),
                &[
                    ("models/a.glb", padded_glb(11 * MIB)),
                    ("models/b.glb", padded_glb(11 * MIB)),
                    ("models/c.glb", padded_glb(11 * MIB)),
                ],
            ),
            vec![(CONTENT_FORMAT, "gltf-json-total")],
            Some(rel(
                &(part("models/a.glb", "art", cc) + &part("models/b.glb", "art", cc)),
                &[
                    ("models/a.glb", padded_glb(11 * MIB)),
                    ("models/b.glb", padded_glb(11 * MIB)),
                ],
            )),
        ),
        fx(
            "a .gltf uri with a query",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[
                    ("models/prop.gltf", gltf_with_uri("prop.bin?x=1")),
                    ("models/prop.bin", b"\0\0\0\0".to_vec()),
                ],
            ),
            vec![(CONTENT_FORMAT, "uri-forbidden-character")],
            Some(rel(
                &part("models/prop.gltf", "art", cc),
                &[
                    ("models/prop.gltf", gltf_with_uri("Base%20Color.bin")),
                    ("models/Base Color.bin", b"\0\0\0\0".to_vec()),
                ],
            )),
        ),
        fx(
            "a .gltf uri with a malformed escape",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[("models/prop.gltf", gltf_with_uri("prop%2.bin"))],
            ),
            vec![(CONTENT_FORMAT, "uri-escape-invalid")],
            None,
        ),
        fx(
            "a .gltf uri encoded twice",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[
                    ("models/prop.gltf", gltf_with_uri("%252e%252e/secret.bin")),
                    ("models/%2e%2e/secret.bin", b"\0\0\0\0".to_vec()),
                ],
            ),
            vec![(CONTENT_FORMAT, "uri-double-encoded")],
            None,
        ),
        fx(
            "a .gltf uri that climbs out once decoded",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[
                    ("models/prop.gltf", gltf_with_uri("..%2fsecret.bin")),
                    ("models/..%2fsecret.bin", b"\0\0\0\0".to_vec()),
                ],
            ),
            vec![(CONTENT_FORMAT, "uri-not-relative")],
            None,
        ),
        fx(
            "a .gltf whose images are an object a loader reads by its keys",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[(
                    "models/prop.gltf",
                    br#"{"asset":{"version":"2.0"},"images":{"0":{"uri":"https://x/y.png"}}}"#
                        .to_vec(),
                )],
            ),
            vec![(CONTENT_FORMAT, "gltf-array-shape")],
            Some(rel(
                &part("models/prop.gltf", "art", cc),
                &[(
                    "models/prop.gltf",
                    br#"{"asset":{"version":"2.0"},"images":[{"uri":"data:image/png;base64,AA=="}]}"#
                        .to_vec(),
                )],
            )),
        ),
        fx(
            "a .gltf whose image is no object",
            rel(
                &part("models/prop.gltf", "art", cc),
                &[(
                    "models/prop.gltf",
                    br#"{"asset":{"version":"2.0"},"images":["https://x/y.png"]}"#.to_vec(),
                )],
            ),
            vec![(CONTENT_FORMAT, "gltf-array-shape")],
            None,
        ),
        // -- a game's own archives, one level deep (§2.7.1) --
        fx(
            "a VRM in a mod jar no entry declares",
            jar_release(mod_jar(&allowed, "assets/avatar.vrm"), None),
            vec![(CONTENT_UNDECLARED, in_jar)],
            Some(jar_release(
                mod_jar(&allowed, "assets/avatar.vrm"),
                Some(in_jar),
            )),
        ),
        fx(
            "a prohibited VRM in a mod jar",
            jar_release(mod_jar(&prohibited, "assets/avatar.vrm"), Some(in_jar)),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(jar_release(
                mod_jar(&allowed, "assets/avatar.vrm"),
                Some(in_jar),
            )),
        ),
        fx(
            "a VRM in a mod jar renamed .bin",
            jar_release(mod_jar(&prohibited, "assets/avatar.bin"), None),
            vec![(CONTENT_FORMAT, "glb-misnamed")],
            Some(jar_release(
                mod_jar(&allowed, "assets/avatar.vrm"),
                Some(in_jar),
            )),
        ),
        fx(
            "a mod jar with a gap before an entry",
            jar_release(
                games_zip(
                    &{
                        let mut e = mod_jar_entries(&allowed, "assets/avatar.vrm");
                        e[2].pad_before = b"PK\x03\x04HIDDEN-ENTRY".to_vec();
                        e
                    },
                    b"",
                ),
                None,
            ),
            vec![(ZIP_HEADERS_INCONSISTENT, "mods/avatar.jar")],
            Some(jar_release(
                mod_jar(&allowed, "assets/avatar.vrm"),
                Some(in_jar),
            )),
        ),
        fx(
            "a mod jar with bytes after its end record",
            jar_release(
                games_zip(&mod_jar_entries(&allowed, "assets/avatar.vrm"), b"x"),
                None,
            ),
            vec![(ZIP_END_RECORD, "mods/avatar.jar")],
            Some(jar_release(
                mod_jar(&allowed, "assets/avatar.vrm"),
                Some(in_jar),
            )),
        ),
        fx(
            "a file named like a member of the jar beside it",
            {
                let mut f = jar_release(mod_jar(&allowed, "assets/avatar.vrm"), Some(in_jar));
                f.push((
                    "mods/avatar.jar!assets/logo.png".to_owned(),
                    b"a shadow".to_vec(),
                ));
                f
            },
            vec![(CONTENT_FORMAT, "member-shadowed")],
            Some({
                let mut f = jar_release(mod_jar(&allowed, "assets/avatar.vrm"), Some(in_jar));
                f.push(("mods/avatar.jar!readme.txt".to_owned(), b"a note".to_vec()));
                f
            }),
        ),
        fx(
            "a mod jar past the release's 32 MiB of glTF JSON",
            rel(
                &(part("models/a.glb", "art", cc) + &part("models/b.glb", "art", cc)),
                &[
                    ("models/a.glb", padded_glb(11 * MIB)),
                    ("models/b.glb", padded_glb(11 * MIB)),
                    ("mods/props.jar", mod_jar(&padded_glb(11 * MIB), "assets/p.glb")),
                ],
            ),
            vec![(CONTENT_FORMAT, "gltf-json-total")],
            Some(rel(
                &(part("models/a.glb", "art", cc)
                    + &part("models/b.glb", "art", cc)
                    + &part("mods/props.jar!assets/p.glb", "art", cc)),
                &[
                    ("models/a.glb", padded_glb(11 * MIB)),
                    ("models/b.glb", padded_glb(11 * MIB)),
                    ("mods/props.jar", mod_jar(&padded_glb(9 * MIB), "assets/p.glb")),
                ],
            )),
        ),
        // -- a bundle, judged by the content catalogue's own code (§2.7.3) --
        fx(
            "a character bundle whose body refuses redistribution",
            char_release(body(&prohibited), cc),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(good_char()),
        ),
        fx(
            "a character bundle whose accessory refuses redistribution beside an allowed body",
            char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("characters/aiko/accessories/hat/hat.glb", &prohibited),
                    ],
                    Some(BODY),
                ),
                cc,
            ),
            vec![(
                CONTENT_LICENCE_REFUSED,
                "extensions.VRMC_vrm.meta.allowRedistribution",
            )],
            Some(char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("characters/aiko/accessories/hat/hat.glb", &allowed),
                    ],
                    Some(BODY),
                ),
                cc,
            )),
        ),
        fx(
            "a character bundle whose body's two metas disagree",
            char_release(
                body(&both_metas(
                    r#"{"licenseName":"Redistribution_Prohibited","allowedUserName":"Everyone"}"#,
                )),
                cc,
            ),
            vec![(CONTENT_LICENCE_CONFLICT, bundle_part)],
            Some(char_release(
                body(&both_metas(
                    r#"{"licenseName":"CC0","allowedUserName":"Everyone"}"#,
                )),
                cc,
            )),
        ),
        fx(
            "a character bundle whose picture has no licence",
            char_release(
                character(&[(BODY, &allowed), ("cover.png", PNG16)], Some(BODY)),
                None,
            ),
            vec![(CONTENT_LICENCE_UNDECLARED, bundle_part)],
            Some(char_release(
                character(&[(BODY, &allowed), ("cover.png", PNG16)], Some(BODY)),
                cc,
            )),
        ),
        fx(
            "a character bundle whose body is a GLB naming a remote image",
            char_release(
                body(&glb(
                    r#"{"asset":{"version":"2.0"},"extensions":{"VRMC_vrm":{"meta":{"allowRedistribution":true,"avatarPermission":"everyone"}}},"images":[{"uri":"https://x/y.png"}]}"#,
                )),
                cc,
            ),
            vec![(CONTENT_FORMAT, "external-uri")],
            Some(good_char()),
        ),
        fx(
            "a character bundle whose body is no VRM",
            char_release(body(&plain), cc),
            vec![(CONTENT_FORMAT, "body-not-vrm")],
            Some(good_char()),
        ),
        fx(
            "a character bundle holding an archive",
            char_release(
                character(
                    &[(BODY, &allowed), ("cover.png", &mod_jar(PNG16, "a.png"))],
                    Some(BODY),
                ),
                cc,
            ),
            vec![(CONTENT_FORMAT, "nested-archive")],
            Some(char_release(
                character(&[(BODY, &allowed), ("cover.png", PNG16)], Some(BODY)),
                cc,
            )),
        ),
        fx(
            "a character bundle's accessory GLB renamed .bin",
            char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("characters/aiko/accessories/hat/hat.bin", &allowed),
                    ],
                    Some(BODY),
                ),
                cc,
            ),
            vec![(CONTENT_FORMAT, "name-magic")],
            Some(char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("characters/aiko/accessories/hat/hat.glb", &allowed),
                    ],
                    Some(BODY),
                ),
                cc,
            )),
        ),
        fx(
            "a character bundle whose character.toml is no TOML",
            char_release(
                character(
                    &[(BODY, &allowed), ("characters/aiko/character.toml", b"name = ")],
                    Some(BODY),
                ),
                cc,
            ),
            vec![(CONTENT_FORMAT, "settings-invalid")],
            Some(char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("characters/aiko/character.toml", b"name = \"Aiko\"\n"),
                    ],
                    Some(BODY),
                ),
                cc,
            )),
        ),
        fx(
            "a character bundle's .gltf naming a file outside its own part",
            char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("cover.png", PNG16),
                        ("scene.gltf", &gltf_with_uri("cover.png")),
                    ],
                    Some(BODY),
                ),
                cc,
            ),
            vec![(CONTENT_FORMAT, "uri-outside-part")],
            Some(char_release(
                character(
                    &[
                        (BODY, &allowed),
                        ("animpacks/wave/clip.gltf", &gltf_with_uri("clip.bin")),
                        ("animpacks/wave/clip.bin", b"\0\0\0\0"),
                    ],
                    Some(BODY),
                ),
                cc,
            )),
        ),
        fx(
            "a character bundle whose body is named by a string",
            char_release(
                character_manifest(&[(BODY, &allowed)], |m| m["body"] = json!(BODY)),
                cc,
            ),
            vec![(CONTENT_FORMAT, "bundle-unreadable")],
            Some(good_char()),
        ),
        fx(
            "a character bundle whose body is over 50 MiB",
            char_release(
                body(&glb_with(
                    r#"{"asset":{"version":"2.0"},"extensions":{"VRMC_vrm":{"meta":{"allowRedistribution":true,"avatarPermission":"everyone"}}}}"#,
                    Some(&vec![0u8; 50 * MIB + 4]),
                )),
                cc,
            ),
            vec![(CONTENT_TOO_LARGE, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle holding a file in no part",
            char_release(
                character(&[(BODY, &allowed), ("readme.txt", b"hi")], Some(BODY)),
                cc,
            ),
            vec![(CONTENT_FILE_OUTSIDE_PARTS, bundle_part)],
            Some(char_release(
                character(&[(BODY, &allowed), ("readme.png", PNG16)], Some(BODY)),
                cc,
            )),
        ),
        fx(
            "a character bundle that reads as two archives",
            char_release(cloaked, cc),
            vec![(ZIP_HEADERS_INCONSISTENT, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle with a gap before an entry",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e[0].pad_before = b"PK\x03\x04HIDDEN".to_vec();
                }),
                cc,
            ),
            vec![(ZIP_HEADERS_INCONSISTENT, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle with bytes after its end record",
            char_release(
                {
                    let mut b = body(&allowed);
                    b.push(b'x');
                    b
                },
                cc,
            ),
            vec![(ZIP_END_RECORD, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle of more than 4,096 entries",
            char_release(
                {
                    let names: Vec<String> = (0..4096).map(|i| format!("art/{i}.png")).collect();
                    let files: Vec<(&str, &[u8])> =
                        names.iter().map(|n| (n.as_str(), PNG16)).collect();
                    pack(&files)
                },
                cc,
            ),
            vec![(ARCHIVE_TOO_MANY_ENTRIES, bundle_part)],
            Some(char_release(
                {
                    let names: Vec<String> = (0..4095).map(|i| format!("art/{i}.png")).collect();
                    let files: Vec<(&str, &[u8])> =
                        names.iter().map(|n| (n.as_str(), PNG16)).collect();
                    pack(&files)
                },
                cc,
            )),
        ),
        fx(
            "a character bundle holding an encrypted entry",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e[0].flags = 1;
                }),
                cc,
            ),
            vec![(ARCHIVE_ENCRYPTED, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle holding an entry of another method",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e[0].method = 12;
                }),
                cc,
            ),
            vec![(ARCHIVE_METHOD, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle holding a link",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e[0].external = 0o120777 << 16;
                }),
                cc,
            ),
            vec![(ARCHIVE_LINK, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle holding two names one file system folds together",
            char_release(
                character(
                    &[(BODY, &allowed), ("cover.png", PNG16), ("COVER.png", PNG16)],
                    Some(BODY),
                ),
                cc,
            ),
            vec![(ARCHIVE_NAME_UNSAFE, bundle_part)],
            Some(char_release(
                character(
                    &[(BODY, &allowed), ("cover.png", PNG16), ("poster.png", PNG16)],
                    Some(BODY),
                ),
                cc,
            )),
        ),
        fx(
            "a character bundle that says it unpacks to more than 128 MiB",
            char_release(
                bundle_with(
                    "astra.character",
                    &[(BODY, &allowed), ("theme.ogg", OGG)],
                    Some(BODY),
                    |e| {
                        e[1].method = 8;
                        e[1].size = Some(129 * MIB as u32);
                    },
                ),
                cc,
            ),
            vec![(ARCHIVE_TOO_LARGE, bundle_part)],
            Some(char_release(
                bundle_with(
                    "astra.character",
                    &[(BODY, &allowed), ("theme.ogg", OGG)],
                    Some(BODY),
                    |e| e[1].method = 8,
                ),
                cc,
            )),
        ),
        fx(
            "a character bundle's entry deflated more than 100 to 1",
            {
                let mut ogg = OGG.to_vec();
                ogg.resize(256 * 1024, 0);
                let comp = deflate(&ogg);
                char_release(
                    bundle_with(
                        "astra.character",
                        &[(BODY, &allowed), ("theme.ogg", &ogg)],
                        Some(BODY),
                        move |e| {
                            e[1].method = 8;
                            e[1].comp = Some(comp);
                        },
                    ),
                    cc,
                )
            },
            vec![(ARCHIVE_RATIO, bundle_part)],
            Some(char_release(
                {
                    let mut ogg = OGG.to_vec();
                    ogg.resize(256 * 1024, 0);
                    character(&[(BODY, &allowed), ("theme.ogg", &ogg)], Some(BODY))
                },
                cc,
            )),
        ),
        fx(
            "a character bundle's picture deflated where it must be stored",
            char_release(
                bundle_with(
                    "astra.character",
                    &[(BODY, &allowed), ("cover.png", PNG16)],
                    Some(BODY),
                    |e| e[1].method = 8,
                ),
                cc,
            ),
            vec![(ARCHIVE_MUST_BE_STORED, bundle_part)],
            Some(char_release(
                character(&[(BODY, &allowed), ("cover.png", PNG16)], Some(BODY)),
                cc,
            )),
        ),
        fx(
            "a character bundle whose astrachar.json is not its last entry",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e.rotate_right(1);
                }),
                cc,
            ),
            vec![(BUNDLE_MANIFEST, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle naming its body twice",
            char_release(
                {
                    let files = vec![(BODY.to_owned(), allowed.clone())];
                    let m = python_json(&content_gen_manifest("astra.character", &files, Some(BODY)));
                    let text = String::from_utf8(m).unwrap().replacen(
                        "\"body\": {",
                        "\"body\": {\"file\": \"characters/aiko/other.vrm\"},\n \"body\": {",
                        1,
                    );
                    games_zip(
                        &[Z::new(BODY, &allowed), Z::new("astrachar.json", text.as_bytes())],
                        b"",
                    )
                },
                cc,
            ),
            vec![(BUNDLE_MANIFEST, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "a character bundle holding a file its astrachar.json does not list",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e.insert(1, Z::new("cover.png", PNG16));
                }),
                cc,
            ),
            vec![(BUNDLE_FILE_UNLISTED, bundle_part)],
            Some(char_release(
                character(&[(BODY, &allowed), ("cover.png", PNG16)], Some(BODY)),
                cc,
            )),
        ),
        fx(
            "a character bundle whose file is not what astrachar.json lists",
            char_release(
                bundle_with("astra.character", &[(BODY, &allowed)], Some(BODY), |e| {
                    e[0].data = vrm1(VRM1_PROHIBITED);
                }),
                cc,
            ),
            vec![(BUNDLE_FILE_DIGEST, bundle_part)],
            Some(good_char()),
        ),
        fx(
            "two packs past the release's 32 MiB of glTF JSON",
            rel(
                &(part("packs/a.astrapack", "animations", cc)
                    + &part("packs/b.astrapack", "animations", cc)),
                &[
                    (
                        "packs/a.astrapack",
                        pack(&[
                            ("clips/a.gltf", &big_json(11 * MIB)),
                            ("clips/b.gltf", &big_json(11 * MIB)),
                        ]),
                    ),
                    (
                        "packs/b.astrapack",
                        pack(&[
                            ("clips/a.gltf", &big_json(11 * MIB)),
                            ("clips/b.gltf", &big_json(11 * MIB)),
                        ]),
                    ),
                ],
            ),
            vec![(CONTENT_FORMAT, "gltf-json-total")],
            Some(rel(
                &part("packs/a.astrapack", "animations", cc),
                &[(
                    "packs/a.astrapack",
                    pack(&[
                        ("clips/a.gltf", &big_json(11 * MIB)),
                        ("clips/b.gltf", &big_json(11 * MIB)),
                    ]),
                )],
            )),
        ),
        // -- the pack inbox --
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
    ];
    // Flagged, never refused: a bundle whose body is a `builtin:` reference ships no file, so a
    // licence must be declared — and it passes with one.
    let builtin_body = character_manifest(&[], |m| {
        m["body"] = json!({ "ref": "builtin:nayu", "sha256": "x" })
    });
    out.push(Fixture {
        name: "a character bundle whose body is a builtin: reference",
        files: char_release(builtin_body.clone(), None),
        refused: vec![(CONTENT_LICENCE_UNDECLARED, bundle_part.to_owned())],
        flagged: Vec::new(),
        near_miss: Some(char_release(builtin_body, cc)),
        near_miss_flagged: Vec::new(),
    });
    // A plain glTF in a character part is judged, and reads unknown: flagged, and refused only
    // for want of the licence it then needs.
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
            &[("character/plain.glb", plain.clone())],
        )),
        near_miss_flagged: vec![(
            content::FLAG_VRM_META_UNKNOWN,
            "character/plain.glb".to_owned(),
        )],
    });
    // The content catalogue holds a bundle whose VRM reads unknown: published flagged, and only
    // with the part's declared licence.
    let unknown_body = body(&glb(&format!(
        r#"{{"asset":{{"version":"2.0"}},"extensions":{{"VRM":{{"meta":{VRM0_OTHER}}}}}}}"#
    )));
    out.push(Fixture {
        name: "a character bundle whose body's licence the gate does not know",
        files: char_release(unknown_body.clone(), None),
        refused: vec![(CONTENT_LICENCE_UNDECLARED, bundle_part.to_owned())],
        flagged: vec![(content::FLAG_VRM_META_UNKNOWN, bundle_part.to_owned())],
        near_miss: Some(char_release(unknown_body, cc)),
        near_miss_flagged: vec![(content::FLAG_VRM_META_UNKNOWN, bundle_part.to_owned())],
    });
    // A game archive this gate cannot open is published flagged, never refused; nor is the
    // archive inside one.
    let zip64_extra = {
        let mut x = 1u16.to_le_bytes().to_vec();
        x.extend(16u16.to_le_bytes());
        x.extend(1u64.to_le_bytes());
        x.extend(1u64.to_le_bytes());
        x
    };
    for (name, jar) in [
        ("a mod jar that needs zip64", {
            let mut e = mod_jar_entries(&prohibited, "assets/avatar.vrm");
            e[0].extra = zip64_extra.clone();
            games_zip(&e, b"")
        }),
        ("a mod jar holding an encrypted entry", {
            let mut e = mod_jar_entries(&prohibited, "assets/avatar.vrm");
            e[1].flags = 1;
            games_zip(&e, b"")
        }),
    ] {
        out.push(Fixture {
            name,
            files: jar_release(jar, None),
            refused: Vec::new(),
            flagged: vec![(
                content::FLAG_ARCHIVE_UNSCANNED,
                "mods/avatar.jar".to_owned(),
            )],
            near_miss: Some(jar_release(
                mod_jar(&allowed, "assets/avatar.vrm"),
                Some(in_jar),
            )),
            near_miss_flagged: Vec::new(),
        });
    }
    out.push(Fixture {
        name: "a jar in a jar",
        files: jar_release(
            games_zip(
                &[
                    Z::deflated("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                    Z::new(
                        "META-INF/jars/inner.jar",
                        &mod_jar(&prohibited, "assets/avatar.vrm"),
                    ),
                ],
                b"",
            ),
            None,
        ),
        refused: Vec::new(),
        flagged: vec![(
            content::FLAG_ARCHIVE_UNSCANNED,
            "mods/avatar.jar!META-INF/jars/inner.jar".to_owned(),
        )],
        near_miss: Some(jar_release(
            games_zip(
                &[
                    Z::deflated("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                    Z::new("META-INF/jars/inner.txt", b"not a jar"),
                ],
                b"",
            ),
            None,
        )),
        near_miss_flagged: Vec::new(),
    });
    // A bundle in a game's archive is judged by all its bytes, never its first 64 KiB: a
    // harmless pack of exactly 64 KiB with the prohibited golden character after it is the
    // character to every reader of the whole entry (its last end record is the character's).
    let scout = fs::read(
        root().join(
            "testdata/games/content/releases/bundle-vrm-redistribution-prohibited/characters/scout.astrachar",
        ),
    )
    .unwrap();
    let cloaked = [pack_of_len(64 * 1024), scout].concat();
    out.push(fx(
        "a bundle in a jar whose first 64 KiB are another bundle",
        inner_bundle_release(&cloaked),
        vec![(ZIP_END_RECORD, INNER_BUNDLE)],
        Some(inner_bundle_release(&pack_of_len(100 * 1024))),
    ));
    out
}

/// Where [`inner_bundle_release`] declares its bundle.
pub const INNER_BUNDLE: &str = "mods/pack.jar!packs/big.astrapack";

/// A release of one mod jar holding `bundle` (Stored) at `packs/big.astrapack`, declared.
pub fn inner_bundle_release(bundle: &[u8]) -> Vec<(String, Vec<u8>)> {
    rel(
        &part(INNER_BUNDLE, "animations", Some("CC-BY-4.0")),
        &[(
            "mods/pack.jar",
            games_zip(
                &[
                    Z::deflated("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                    Z::new("packs/big.astrapack", bundle),
                ],
                b"",
            ),
        )],
    )
}

/// A valid animation pack of exactly `len` bytes: one clip, padded until the pack is that long.
pub fn pack_of_len(len: usize) -> Vec<u8> {
    let mut pad = 0usize;
    for _ in 0..16 {
        let clip = [FBX, &vec![b'x'; pad]].concat();
        let p = pack(&[("clips/a.fbx", &clip)]);
        if p.len() == len {
            return p;
        }
        pad = (pad + len).checked_sub(p.len()).expect("a pack that long");
    }
    panic!("no pack of exactly {len} bytes");
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
