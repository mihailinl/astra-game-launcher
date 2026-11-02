// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! AstraPlugins' game-integration goldens as `tests/goldens/astraplugins/` holds them (at its
//! `PIN`), read and judged one way for `tests/goldens.rs` (the launcher's library) and
//! `tests/gi_probe.rs` (`gi-probe check`): what `vectors.json` labels each one, what the launcher
//! pins where the label names no code, and [`SPEC_DEFECTS_AT_PIN`], the one list of every
//! golden the launcher answers otherwise at this pin.
//!
//! A golden is judged by its verdict AND its codes, never a reason's text: an answer is the list
//! of `(code, field)` the launcher refuses with, in order (`field` = the refusal's `field` param,
//! else its `key`, else `""`), empty for accepted.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The vendored copy.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/astraplugins")
}

/// `testdata/games/<rel>`'s text.
pub fn golden_text(rel: &str) -> String {
    let p = root().join("testdata/games").join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The pinned AstraPlugins commit.
pub fn pin() -> String {
    fs::read_to_string(root().join("PIN"))
        .expect("PIN")
        .trim()
        .to_owned()
}

/// The sums, checked once: every file of the copy is listed in `SHA256SUMS` with its digest,
/// and nothing listed is missing — and every file of AstraPlugins' is the commit's at `PIN`
/// ([`proven_at_pin`]), which no regenerated sum can make so. Every judgement below asks this
/// first.
pub fn sums() -> &'static BTreeMap<String, String> {
    static SUMS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
        let root = root();
        let text = fs::read_to_string(root.join("SHA256SUMS")).expect("SHA256SUMS");
        let mut listed = BTreeMap::new();
        for line in text.lines() {
            let (hex, path) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("SHA256SUMS: not a sha256sum line: {line:?}"));
            assert!(
                hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                "SHA256SUMS: {line:?}"
            );
            assert!(
                listed.insert(path.to_owned(), hex.to_owned()).is_none(),
                "SHA256SUMS lists {path} twice"
            );
        }
        let mut on_disk = Vec::new();
        walk(&root, &root, &mut on_disk);
        on_disk.retain(|p| p != "SHA256SUMS");
        on_disk.sort();
        let listed_paths: Vec<&String> = listed.keys().collect();
        assert_eq!(
            listed_paths,
            on_disk.iter().collect::<Vec<_>>(),
            "SHA256SUMS does not list exactly the vendored files: re-vendor with tools/vendor-astraplugins-goldens.sh, never by hand"
        );
        for (path, hex) in &listed {
            let bytes = fs::read(root.join(path)).unwrap();
            assert_eq!(
                &hex::encode(Sha256::digest(&bytes)),
                hex,
                "{path} is not the vendored bytes: re-vendor with tools/vendor-astraplugins-goldens.sh, never by hand"
            );
        }
        assert!(listed.contains_key("PIN"), "PIN is not summed");
        let pin = fs::read_to_string(root.join("PIN")).unwrap();
        let pin = pin.trim();
        assert!(
            pin.len() == 40 && pin.bytes().all(|b| b.is_ascii_hexdigit()),
            "PIN: {pin:?}"
        );
        proven_at_pin(&root, pin, &listed);
        listed
    });
    &SUMS
}

/// Files of the copy that are the vendoring's own, not AstraPlugins'.
fn own(path: &str) -> bool {
    matches!(path, "PIN" | "NOTICE" | "SHA256SUMS") || path.starts_with("GIT-OBJECTS/")
}

/// What git names an object of `kind` holding `data`: SHA-1 of `<kind> <len>\0<data>`.
fn git_id(kind: &str, data: &[u8]) -> String {
    let mut framed = format!("{kind} {}\0", data.len()).into_bytes();
    framed.extend_from_slice(data);
    hex::encode(sha1(&framed))
}

/// SHA-1 (FIPS 180-4), for git's object ids alone: a commit id commits to its tree, each tree to
/// what it holds, each blob to its bytes.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (o, x) in out.chunks_mut(4).zip(h) {
        o.copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// A raw git tree: each entry's name, with its mode and its object's id.
fn tree_entries(data: &[u8]) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|b| *b == b' ')
            .expect("a tree entry's mode");
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .expect("a tree entry's name");
        let mode = String::from_utf8(rest[..space].to_vec()).unwrap();
        let name = String::from_utf8(rest[space + 1..nul].to_vec()).unwrap();
        let id = hex::encode(&rest[nul + 1..nul + 21]);
        out.insert(name, (mode, id));
        rest = &rest[nul + 21..];
    }
    out
}

/// That the copy is AstraPlugins' at `pin`, by git's own ids and nothing the vendoring wrote:
/// `GIT-OBJECTS/<pin>` is the commit whose SHA-1 is `pin`; from its tree, each folder's tree
/// object (each also hashing to its name) leads to every vendored file, whose bytes hash to the
/// blob id that tree names; and every file the pinned trees hold below `testdata/games/` and
/// `testdata/zip-cloaking/`, every `*.json` of `spec/games/schemas/` and
/// `spec/games/reserved.yaml` is vendored. A hand edit of
/// any byte, its sums regenerated, still fails here; so does any file added or left out.
fn proven_at_pin(root: &Path, pin: &str, listed: &BTreeMap<String, String>) {
    assert_eq!(
        hex::encode(sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d",
        "SHA-1 itself"
    );
    assert_eq!(
        git_id("blob", b""),
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
    );
    let object = |id: &str, kind: &str| -> Vec<u8> {
        let data = fs::read(root.join("GIT-OBJECTS").join(id))
            .unwrap_or_else(|e| panic!("GIT-OBJECTS/{id}: {e}: re-vendor"));
        assert_eq!(
            git_id(kind, &data),
            id,
            "GIT-OBJECTS/{id} is no {kind} of that id: re-vendor"
        );
        data
    };
    let commit = object(pin, "commit");
    let tree = std::str::from_utf8(&commit)
        .ok()
        .and_then(|t| t.lines().next())
        .and_then(|l| l.strip_prefix("tree "))
        .expect("the commit's first line names its tree")
        .to_owned();
    // The object at a `/`-separated path below the commit's tree: its mode and id.
    let at = |path: &str| -> Option<(String, String)> {
        let mut entry = ("40000".to_owned(), tree.clone());
        for c in path.split('/') {
            assert_eq!(entry.0, "40000", "{path}: a file on the way at PIN");
            entry = tree_entries(&object(&entry.1, "tree")).get(c)?.clone();
        }
        Some(entry)
    };
    for path in listed.keys().filter(|p| !own(p)) {
        let (mode, id) = at(path).unwrap_or_else(|| panic!("{path}: not in AstraPlugins at PIN"));
        assert!(mode.starts_with("100"), "{path}: no file at PIN ({mode})");
        let bytes = fs::read(root.join(path)).unwrap();
        assert_eq!(
            git_id("blob", &bytes),
            id,
            "{path} is not AstraPlugins' at PIN {pin}: re-vendor with tools/vendor-astraplugins-goldens.sh, never by hand"
        );
    }
    // Nothing the pinned folders hold is left out.
    fn files_below(
        object: &dyn Fn(&str, &str) -> Vec<u8>,
        id: &str,
        prefix: &str,
        out: &mut Vec<String>,
    ) {
        for (name, (mode, oid)) in tree_entries(&object(id, "tree")) {
            let path = format!("{prefix}/{name}");
            if mode == "40000" {
                files_below(object, &oid, &path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut want = vec!["spec/games/reserved.yaml".to_owned()];
    for folder in ["testdata/games", "testdata/zip-cloaking"] {
        let (_, tree) = at(folder).unwrap_or_else(|| panic!("{folder} at PIN"));
        files_below(&object, &tree, folder, &mut want);
    }
    let (_, schemas) = at("spec/games/schemas").expect("spec/games/schemas at PIN");
    let mut all = Vec::new();
    files_below(&object, &schemas, "spec/games/schemas", &mut all);
    want.extend(all.into_iter().filter(|p| p.ends_with(".json")));
    want.sort();
    let got: Vec<String> = listed.keys().filter(|p| !own(p)).cloned().collect();
    assert_eq!(
        got, want,
        "the copy is exactly what PIN holds of those folders"
    );
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let t = e.file_type().unwrap();
        assert!(
            !t.is_symlink(),
            "{}: a link in the vendored copy",
            p.display()
        );
        if t.is_dir() {
            walk(root, &p, out);
        } else {
            out.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
            );
        }
    }
}

/// `testdata/games/vectors.json`, after the sums.
pub fn vectors() -> &'static Value {
    static V: LazyLock<Value> = LazyLock::new(|| {
        sums();
        serde_json::from_str(&golden_text("vectors.json")).expect("vectors.json")
    });
    &V
}

/// One answer: `(code, field)` per refusal, in order; empty = accepted.
pub type Answer = Vec<(String, String)>;

/// `(code, field)` of a launcher refusal's code and params.
pub fn finding(code: &str, params: &BTreeMap<String, String>) -> (String, String) {
    let field = params
        .get("field")
        .or_else(|| params.get("key"))
        .cloned()
        .unwrap_or_default();
    (code.to_owned(), field)
}

/// What a golden's `vectors.json` record asks of the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Accepted (a built-in also equal to the launcher's own, by digest).
    Accept,
    /// Refused, and with exactly this code (`check: semantic`, its `rule`).
    Rule(String),
    /// Refused by the spec's JSON Schema alone (`check: schema`): the launcher's answer is pinned
    /// in [`SCHEMA_REFUSALS`].
    Pinned(Answer),
    /// Refused elsewhere (`check: pinned`, `enforced_by` lane D or C): the launcher's parser and
    /// Pass B admit it, and the refusal is a later layer's.
    AdmittedHere,
}

/// The launcher's answer to each golden the spec refuses by its schema alone, which names no
/// code: pinned here, code and field.
pub const SCHEMA_REFUSALS: &[(&str, &[(&str, &str)])] = &[
    (
        "descriptors/hostile/read-outside-the-folder.toml",
        &[("descriptor-invalid", "rule[0].require[0]")],
    ),
    // The rule count, past 16 (`descriptor::limits::RULES`).
    (
        "descriptors/hostile/slow-detection-budget-exhaustion.toml",
        &[("descriptor-invalid", "rule")],
    ),
    (
        "manifests/invalid/target-anti-cheat-missing.toml",
        &[("manifest-invalid", "")],
    ),
    (
        "manifests/invalid/link-too-many-tools.toml",
        &[("link-invalid", "integration.link.tools")],
    ),
    (
        "manifests/invalid/unknown-top-level-key.toml",
        &[("unknown-key", "mods")],
    ),
    // `command` is no kind of a game integration's part (`content::ContentKind`).
    (
        "manifests/invalid/content-kind-not-allowed.toml",
        &[("manifest-invalid", "integration.content[0].kind")],
    ),
    (
        "manifests/invalid/content-path-traversal.toml",
        &[("manifest-invalid", "integration.content[0].path")],
    ),
    // `reserved::INTEGRATION_RESERVED_KEYS` (`reason` = `reserved`).
    (
        "manifests/invalid/integration-uses-reserved.toml",
        &[("unknown-key", "integration.uses")],
    ),
];

/// What `record` (a `vectors.json` entry) wants.
pub fn want(record: &Value) -> Want {
    let file = record["file"].as_str().unwrap();
    match (
        record["verdict"].as_str().unwrap(),
        record["check"].as_str(),
        record["rule"].as_str(),
    ) {
        ("accept", _, _) => Want::Accept,
        ("refuse", Some("semantic"), Some(rule)) => Want::Rule(rule.to_owned()),
        ("refuse", Some("schema"), None) => {
            let (_, pinned) = SCHEMA_REFUSALS
                .iter()
                .find(|(f, _)| *f == file)
                .unwrap_or_else(|| {
                    panic!("{file}: refused by the spec's schema, and SCHEMA_REFUSALS pins no answer for it")
                });
            Want::Pinned(owned(pinned))
        }
        ("refuse", Some("pinned"), None) => Want::AdmittedHere,
        other => panic!("{file}: a record this test does not read: {other:?}"),
    }
}

pub fn owned(a: &[(&str, &str)]) -> Answer {
    a.iter()
        .map(|(c, f)| ((*c).to_owned(), (*f).to_owned()))
        .collect()
}

/// Whether `answer` is what `want` asks.
pub fn agrees(want: &Want, answer: &Answer) -> bool {
    match want {
        Want::Accept | Want::AdmittedHere => answer.is_empty(),
        Want::Rule(rule) => answer.len() == 1 && answer[0].0 == *rule,
        Want::Pinned(p) => answer == p,
    }
}

/// How the launcher disagrees with a golden at this pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Answered with these refusals, `(code, field)` in order.
    Answered(&'static [(&'static str, &'static str)]),
    /// A built-in golden that parses, with a digest other than the launcher's built-in's.
    DigestDiffers,
    /// Accepted only from Astra's own publisher, which `vectors.json` cannot say: as a community
    /// release, answered with these.
    FirstPartyOnly(&'static [(&'static str, &'static str)]),
    /// `reserved.yaml`'s `profile_file_names` lacks these, which the launcher reserves.
    ProfileNamesLack(&'static [&'static str]),
    /// `reserved.yaml` reserves engine codes as an exact list, where the launcher reserves by rule.
    EngineCodesListedOnly,
}

/// A golden the launcher answers otherwise than the spec labels it, at this pin.
#[derive(Debug)]
pub struct SpecDefect {
    /// Below the copy: `testdata/games/…` or `spec/games/…`.
    pub golden: &'static str,
    pub seen: Seen,
    /// Why it is the spec's to fix, for its owner.
    pub why: &'static str,
}

/// **Every golden the launcher answers otherwise than `vectors.json` (or `reserved.yaml`) says,
/// at AstraPlugins `PIN`.** Each entry ASSERTS that the golden still disagrees, exactly as
/// recorded: a golden fixed at a new pin turns the tests red until its entry goes (and a golden
/// that disagrees anew turns them red until it is recorded here, or the launcher is fixed). Its
/// copy for the spec's owner is `docs/ENGINE-DESCRIPTORS-DEBT.md`, "For the specification (K13)".
pub const SPEC_DEFECTS_AT_PIN: &[SpecDefect] = &[
    SpecDefect {
        golden: "testdata/games/descriptors/builtin/unity.toml",
        seen: Seen::DigestDiffers,
        why: "copied from plan B §1's text, not from the launcher's src/engines/builtin/unity.toml (L2), which C1 holds to 0.4.1's detector",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/builtin/godot.toml",
        seen: Seen::DigestDiffers,
        why: "copied from plan B §1's text, not from the launcher's src/engines/builtin/godot.toml (L2)",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/builtin/unreal.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "")]),
        why: "does not parse: its `programs` matches no form of the grammar (ProgramsSpec); the launcher's src/engines/builtin/unreal.toml is the worked example",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/hostile/take-recognised-game.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "rule[0].evidence")]),
        why: "evidence = \"{project}\", which a `programs` with no `at` never captures, refuses it before its rule (engine-code-reserved) is reached",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/hostile/grief-unknown-games-no-anchor.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "rule[0].evidence")]),
        why: "evidence {project} is never captured, so it refuses before its rule; and the rule's name is the spec's descriptor-anchor-invalid where the launcher says descriptor-too-broad{reason} (debt row 13)",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/hostile/anticheat-list-misses.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "rule[0].evidence")]),
        why: "labelled for a later layer (lane D, against a real folder), but evidence {project} is never captured, so the launcher's parser refuses it first and the anti-cheat question is never asked",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/hostile/slow-detection-budget-exhaustion.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "")]),
        why: "its rules have no `binary` (a key every rule must have), so it refuses for that before its 17 rules reach the budget it is named for",
    },
    SpecDefect {
        golden: "testdata/games/descriptors/mods/hostile/mods-deep-nesting.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "")]),
        why: "labelled reader_ignoring_mods = accept, but its TOML is one level past the toml crate's fixed recursion limit (toml 1.1 de::parser::LIMIT = 80; only its `unbounded` feature lifts it, which a reader of hostile input does not take): the parser refuses the whole document before `mods` is reached, in 0.5.0 and in any launcher that reads TOML with toml. 39 `any` in `any` parse, 40 do not; its JSON spelling parses (serde_json's limit is 128). Nest it 39 deep or less, still far past §7.1's 11, and say in §7.1 that a TOML reader's limit is lower than serde_json's",
    },
    SpecDefect {
        golden: "testdata/games/manifests/invalid/engine-code-reserved.toml",
        seen: Seen::Answered(&[("descriptor-invalid", "rule[0].evidence")]),
        why: "its descriptor's evidence {project} is never captured, so it refuses before engine-code-reserved",
    },
    SpecDefect {
        golden: "testdata/games/manifests/invalid/engine-undeclared.toml",
        seen: Seen::Answered(&[
            ("engine-undeclared", ""),
            ("descriptor-invalid", "rule[0].evidence"),
        ]),
        why: "refused for its rule, and its descriptor breaks a second one: evidence {core} names a capture no lookup makes (foundation-owns-engine has the `child … as = \"core\"`)",
    },
    SpecDefect {
        golden: "testdata/games/manifests/invalid/engine-undeclared-mismatch.toml",
        seen: Seen::Answered(&[
            ("engine-undeclared", ""),
            ("descriptor-invalid", "rule[0].evidence"),
        ]),
        why: "as engine-undeclared: its descriptor's evidence {core} is never captured",
    },
    SpecDefect {
        golden: "testdata/games/manifests/valid/game-specific-link.toml",
        seen: Seen::Answered(&[("loader-undeclared", "")]),
        why: "places winhttp.dll, a name mod loaders hijack-load by, with no [integration.loader] proxies naming it: as a community release that is loader-undeclared (merged plan §2.5, K18). Add `[integration.loader] proxies = [\"winhttp.dll\"]`",
    },
    SpecDefect {
        golden: "testdata/games/manifests/valid/runner-builtin.toml",
        seen: Seen::FirstPartyOnly(&[("reserved-id", "")]),
        why: "accepted only from Astra's own publisher (its id is astra.unity), which vectors.json has no field to say",
    },
    SpecDefect {
        golden: "spec/games/reserved.yaml",
        seen: Seen::ProfileNamesLack(&["mods-choice.json"]),
        why: "profile_file_names lacks mods-choice.json, the launcher's mods choice (0.4.x), which no manifest may place",
    },
    SpecDefect {
        golden: "spec/games/reserved.yaml#engine_codes",
        seen: Seen::EngineCodesListedOnly,
        why: "engine_codes lists 5 codes; the launcher reserves by rule (is_reserved_code): the codes, their look-alikes folded with digits read as letters (un1ty-mono, g0dot), the prefixes unknown and astra, every built-in family name as a prefix (unity, unreal, godot), and the infix ambiguous",
    },
];

/// The entry for `golden`, if it disagrees at this pin.
pub fn defect(golden: &str) -> Option<&'static SpecDefect> {
    SPEC_DEFECTS_AT_PIN.iter().find(|d| d.golden == golden)
}

/// Judges one golden's answer: it agrees with `want` and has no entry, or it is exactly what its
/// entry records and does not agree. Returns the failure, if any, for a test to collect.
pub fn judge(golden: &str, want: &Want, answer: &Answer) -> Option<String> {
    let agrees = agrees(want, answer);
    match (defect(golden), agrees) {
        (None, true) => None,
        (None, false) => Some(format!(
            "{golden}: the launcher answers {answer:?} where the golden wants {want:?} (record it in SPEC_DEFECTS_AT_PIN if the spec is wrong, else fix the launcher)"
        )),
        (Some(d), true) => Some(format!(
            "{golden}: agrees now ({answer:?}); its SPEC_DEFECTS_AT_PIN entry ({:?}) must go",
            d.seen
        )),
        (Some(d), false) => match d.seen {
            Seen::Answered(a) if owned(a) == *answer => None,
            seen => Some(format!(
                "{golden}: the launcher answers {answer:?}; SPEC_DEFECTS_AT_PIN records {seen:?}: update the entry or the launcher"
            )),
        },
    }
}

/// Every manifest golden: `(golden path, record)`.
pub fn manifest_goldens() -> Vec<(String, Value)> {
    let v = &vectors()["manifests"];
    ["valid", "invalid"]
        .iter()
        .flat_map(|k| v[*k].as_array().unwrap().iter())
        .map(|r| {
            (
                format!("testdata/games/{}", r["file"].as_str().unwrap()),
                r.clone(),
            )
        })
        .collect()
}

/// Whether a manifest golden is asked as Astra's own: its record says so (`provenance`), or its
/// [`Seen::FirstPartyOnly`] entry does.
pub fn first_party(golden: &str, record: &Value) -> bool {
    match record.get("provenance").and_then(Value::as_str) {
        Some(p) => p == "first-party",
        None => matches!(
            defect(golden).map(|d| d.seen),
            Some(Seen::FirstPartyOnly(_))
        ),
    }
}

/// Judges a manifest golden whose answers as `first_party` and as community are given.
pub fn judge_manifest(
    golden: &str,
    record: &Value,
    answer: &Answer,
    community: impl FnOnce() -> Answer,
) -> Option<String> {
    let want = want(record);
    if let Some(SpecDefect {
        seen: Seen::FirstPartyOnly(as_community),
        ..
    }) = defect(golden)
    {
        if record.get("provenance").is_some() {
            return Some(format!(
                "{golden}: vectors.json now says its provenance; its FirstPartyOnly entry must go"
            ));
        }
        if !agrees(&want, answer) {
            return Some(format!(
                "{golden}: as first-party, the launcher answers {answer:?} where the golden wants {want:?}"
            ));
        }
        let c = community();
        return (c != owned(as_community)).then(|| {
            format!("{golden}: as community, the launcher answers {c:?}; the entry records {as_community:?}")
        });
    }
    judge(golden, &want, answer)
}
