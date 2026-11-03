// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! [`link_digest`], `[integration.link]`'s consent digest (AstraPlugins `spec/games/manifest.md`
//! §2.6.1): SHA-256 of the RFC 8785 JCS of the table exactly as parsed, no default added.
//!
//! The rule's vectors are `vectors.json`'s `link.digest`, read out of the goldens vendored at
//! `tests/goldens/astraplugins/PIN` (AstraPlugins master, since PR #134 merged), whose every byte
//! the pin proves (`spec_goldens::sums`): each vector there, whatever its number, is this crate's
//! digest of its input. [`KNOWN`] names the four #134 wrote, so a re-pin that dropped one cannot
//! leave this file vacuously green. `crlf` and `crlf-as-lf` are the spec's own
//! CRLF-normalisation fixture — the same claim this crate's own hand-rolled
//! [`a_crlf_checkout_is_the_lf_files_digest`] tests, checked against an independently-authored
//! vector instead of only our own construction.
//!
//! Every other expected digest below was computed by two other JCS implementations (Python's
//! `json.dumps(ensure_ascii=False)` with keys sorted by UTF-16, and JavaScript's
//! `JSON.stringify` with `Object.keys().sort()`), never by this crate.

#[path = "common/spec_goldens.rs"]
mod spec_goldens;

use std::borrow::Cow;
use std::fs;

use astra_game_launcher::{LauncherError, Manifest, codes, link_digest, toml_newlines};
use serde_json::Value;

/// The `link.digest` vectors AstraPlugins PR #134 wrote: the pinned list must hold each of them.
const KNOWN: [&str; 4] = ["game-specific-link", "edge", "crlf", "crlf-as-lf"];

/// One `link.digest` vector of the pinned `testdata/games/vectors.json`.
struct Vector {
    name: String,
    /// Its input, below `testdata/games/` (the record's `manifest`).
    input: String,
    digest: String,
    /// The record's `crlf`: the input's lines end in CRLF, which the digest reads as LF.
    crlf: bool,
}

/// Every `link.digest` vector at the pin, in its order. A record key this file does not read
/// fails here, so a vector that one day says more than an input and its digest (a refusal, a
/// TOML version) is read before it can pass for one that does not.
fn vectors() -> Vec<Vector> {
    let list = spec_goldens::vectors()["link"]["digest"]
        .as_array()
        .expect("vectors.json: link.digest is not an array");
    list.iter()
        .map(|r| {
            let r = r.as_object().expect("vectors.json: a link.digest record is not an object");
            for k in r.keys() {
                assert!(
                    matches!(k.as_str(), "name" | "manifest" | "digest" | "crlf" | "note"),
                    "vectors.json: link.digest record {r:?} has {k:?}, which this file does not read"
                );
            }
            let text = |k: &str| {
                r.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("vectors.json: link.digest {r:?} has no {k}"))
                    .to_owned()
            };
            Vector {
                name: text("name"),
                input: text("manifest"),
                digest: text("digest"),
                crlf: match r.get("crlf") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(x) => panic!("vectors.json: link.digest {r:?}: crlf = {x}"),
                },
            }
        })
        .collect()
}

/// The pinned vector `name`.
fn vector(name: &str) -> Vector {
    vectors()
        .into_iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("vectors.json at the pin has no link.digest vector {name:?}"))
}

/// The input's bytes, as vendored (and proven) at the pin.
fn input(v: &Vector) -> Vec<u8> {
    let p = spec_goldens::root().join("testdata/games").join(&v.input);
    fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `[integration.link]` of a TOML text, parsed as [`Manifest::parse`] reads one (not every input
/// is a whole manifest, so this is the narrower parse, but the same [`toml_newlines`] reading —
/// without it a raw-CRLF fixture (`crlf.toml`) would digest its un-normalised string and this
/// helper would silently test something `Manifest::parse` never produces).
fn link_of(text: &str) -> Option<toml::Value> {
    let text = toml_newlines(text);
    let t: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    t.get("integration")?.get("link").cloned()
}

fn digest_of(text: &str) -> Result<Option<String>, LauncherError> {
    link_digest(link_of(text).as_ref())
}

#[test]
fn every_vector_of_the_spec_is_this_digest() {
    let all = vectors();
    for name in KNOWN {
        assert!(
            all.iter().any(|v| v.name == name),
            "vectors.json at the pin lost the link.digest vector {name:?}"
        );
    }
    for v in &all {
        let bytes = input(v);
        // Each input's line endings are what its record says, so a checkout that normalised
        // them (the `-text` in `tests/goldens/.gitattributes` is what stops git doing it) fails
        // here instead of testing the LF file twice.
        assert_eq!(
            bytes.windows(2).any(|w| w == b"\r\n"),
            v.crlf,
            "{}: {} CRLF bytes",
            v.name,
            if v.crlf { "lost its" } else { "has" }
        );
        if !v.crlf {
            assert!(!bytes.contains(&b'\r'), "{}: a CR in an LF input", v.name);
        }
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            digest_of(&text).unwrap().as_deref(),
            Some(v.digest.as_str()),
            "{}",
            v.name
        );
    }
}

/// `crlf` and `crlf-as-lf`: the same table, so the same digest — cross-checked against each
/// other as computed, not only each against its record.
#[test]
fn crlf_and_crlf_as_lf_golden_vectors_are_equal() {
    let (crlf, lf) = (vector("crlf"), vector("crlf-as-lf"));
    assert!(crlf.crlf && !lf.crlf, "the records' crlf flags");
    assert_eq!(crlf.digest, lf.digest, "the records' digests");
    let crlf_digest = digest_of(&String::from_utf8(input(&crlf)).unwrap()).unwrap();
    let lf_digest = digest_of(&String::from_utf8(input(&lf)).unwrap()).unwrap();
    assert_eq!(crlf_digest, lf_digest, "crlf must equal crlf-as-lf");
    assert_eq!(crlf_digest.as_deref(), Some(crlf.digest.as_str()));
}

/// The first vector is a whole manifest: through [`Manifest::parse`], as `gi-probe check` reads
/// it (`tests/gi_probe.rs` holds `check`'s report to the same vector), the same digest.
#[test]
fn a_parsed_manifest_gives_the_vectors_digest() {
    let v = vector("game-specific-link");
    assert_eq!(v.input, "manifests/valid/game-specific-link.toml");
    let m = Manifest::parse(std::str::from_utf8(&input(&v)).unwrap()).unwrap();
    assert_eq!(
        link_digest(m.link()).unwrap().as_deref(),
        Some(v.digest.as_str())
    );
}

/// JCS sorts members, so neither the order keys are written in nor TOML's spelling of a table
/// (an inline table, or `[[...]]` sections) changes the digest: this is the edge vector's table.
#[test]
fn the_order_keys_are_written_in_does_not_change_the_digest() {
    let reordered = r#"
[integration.link]
events = []
context = false
shares_other_players = true

[[integration.link.tools]]
name = "wave"
description = "Помахать — \"привет\" \\ 👋"
params = "astra/tools/wave.json"
"#;
    assert_eq!(
        digest_of(reordered).unwrap().as_deref(),
        Some(vector("edge").digest.as_str())
    );
}

/// RFC 8785 §3.2.3: members sorted by their names' UTF-16 code units. U+1F600 is the surrogate
/// pair D83D DE00 and sorts before U+E000, the other way round from UTF-8 (F0 9F 98 80 after
/// EE 80 80) and from a `toml::Table` (`BTreeMap<String, _>`, by UTF-8 bytes). The string holds
/// what JCS escapes (`"`, `\`, every control: the short escapes, else `\u` and lower-case hex)
/// and what it writes as itself (DEL, `/`, non-ASCII).
#[test]
fn members_are_sorted_by_utf16_and_strings_escaped_only_as_jcs_says() {
    let text = r#"
[integration.link]
context = false
"\uE000" = "private use"
"\U0001F600" = "a surrogate pair in UTF-16"
z = "\u0001\b\f\n\r\t\u001F\u007F/é€\"\\"
"#;
    // {"context":false,"z":"\u0001\b\f\n\r\t\u001f<DEL>/é€\"\\","😀":"…","\u{e000}":"…"}
    assert_eq!(
        digest_of(text).unwrap().as_deref(),
        Some("c23ea8ef5e5bcb41af64238d55bde78198ca1a738374a6deb2e88b2e453836a0")
    );
}

/// No table, no digest: a catalogue entry's `link: null`.
#[test]
fn no_link_table_is_no_digest() {
    assert_eq!(link_digest(None).unwrap(), None);
    let m = Manifest::parse(&spec_goldens::golden_text(
        "manifests/valid/foundation-owns-engine.toml",
    ))
    .unwrap();
    assert!(m.link().is_none());
    assert_eq!(link_digest(m.link()).unwrap(), None);
}

/// §2.6.1: no default is added for a key the table leaves out, so writing one out is another
/// digest (and a new consent). Each spelling's digest is pinned, so a digest that added the
/// default to both would fail here too.
#[test]
fn a_default_written_out_is_another_digest() {
    let omits = "[integration.link]\ncontext = true\nshares_other_players = false\n";
    let spells = "[integration.link]\ncontext = true\nshares_other_players = false\ntools = []\n";
    let (omits, spells) = (
        digest_of(omits).unwrap().unwrap(),
        digest_of(spells).unwrap().unwrap(),
    );
    // {"context":true,"shares_other_players":false}
    assert_eq!(
        omits,
        "7b355d1cdcf07fd5c345af35e451ad90ce7a2558ee79e9f35aab220634c22783"
    );
    // {"context":true,"shares_other_players":false,"tools":[]}
    assert_eq!(
        spells,
        "f372629750cfa68898ba6f592ad700688bf162d19b259c5df175087b517e4c49"
    );
    assert_ne!(omits, spells);
}

/// The table's schema admits strings, booleans, arrays and tables. A number, a float or a
/// datetime, wherever it is, has no JSON spelling two implementations are sure to share, so it
/// is refused, never digested: `link-invalid{field, reason=not-digestable, type}`.
#[test]
fn a_number_float_or_datetime_is_refused_never_digested() {
    let cases = [
        (
            "[integration.link]\ncontext = 1\n",
            "integration.link.context",
            "integer",
        ),
        (
            "[integration.link]\ncontext = nan\n",
            "integration.link.context",
            "float",
        ),
        (
            "[integration.link]\ncontext = true\n[[integration.link.tools]]\nname = \"x\"\nweight = 1.5\n",
            "integration.link.tools[0].weight",
            "float",
        ),
        (
            "[integration.link]\nwhen = 1979-05-27T07:32:00Z\n",
            "integration.link.when",
            "datetime",
        ),
        (
            "[integration.link]\nevents = [[\"a\", 1]]\n",
            "integration.link.events[0][1]",
            "integer",
        ),
        // `link` itself must be the table.
        (
            "[integration]\nlink = \"x\"\n",
            "integration.link",
            "string",
        ),
    ];
    for (text, field, ty) in cases {
        let e = digest_of(text).expect_err(text);
        assert_eq!(e.code, codes::LINK_INVALID, "{text}");
        assert_eq!(e.params["field"], field, "{text}");
        assert_eq!(e.params["reason"], "not-digestable", "{text}");
        assert_eq!(e.params["type"], ty, "{text}");
    }
}

/// A raw newline inside a multi-line string is LF in the digest, as the spec's reference reads it
/// (`tomllib` replaces every CRLF of the text before it parses; the `toml` crate keeps it): a
/// release checked out with git's `autocrlf` on Windows has the LF file's digest. A `\r` written
/// as an escape keeps its CR either way. The game-specific-link vector, its tool's description
/// rewritten; each expected digest is AstraPlugins' reference (`tomllib` and
/// `tools/contentlite.py`'s JCS, as computed for #134) and JavaScript's JCS over the same parse.
#[test]
fn a_crlf_checkout_is_the_lf_files_digest() {
    const LF_IN_THE_STRING: &str =
        "24cc58cc7f228aecde0af2c009ba96b45b1dec8840010b9ce625c1398abc8de1";
    const CRLF_IN_THE_STRING: &str =
        "6980874cd460b6f7eb37b074eb03fcb8d22fabea552c8c6404444dc355b3052f";
    let golden = String::from_utf8(input(&vector("game-specific-link"))).unwrap();
    let line = "description = \"Drop the item the player holds\"";
    assert_eq!(golden.matches(line).count(), 1);
    assert!(!golden.contains('\r'));
    for (spelling, want) in [
        (
            "description = \"\"\"\nDrop the item\nthe player holds\"\"\"",
            LF_IN_THE_STRING,
        ),
        (
            "description = '''\nDrop the item\nthe player holds'''",
            LF_IN_THE_STRING,
        ),
        (
            "description = \"Drop the item\\r\\nthe player holds\"",
            CRLF_IN_THE_STRING,
        ),
        (
            "description = \"\"\"\nDrop the item\\r\nthe player holds\"\"\"",
            CRLF_IN_THE_STRING,
        ),
    ] {
        let lf = golden.replace(line, spelling);
        let crlf = lf.replace('\n', "\r\n");
        for (newline, text) in [("LF", &lf), ("CRLF", &crlf)] {
            let m = Manifest::parse(text).unwrap_or_else(|e| panic!("{newline} {spelling}: {e}"));
            assert_eq!(
                link_digest(m.link()).unwrap().as_deref(),
                Some(want),
                "{newline} {spelling}"
            );
        }
    }
    // Only a raw CRLF changes: a lone CR (the parse's own refusal) and escapes stay as they are.
    assert_eq!(toml_newlines("a\r\nb\rc\\r\\n"), "a\nb\rc\\r\\n");
    assert!(matches!(toml_newlines("a\nb"), Cow::Borrowed("a\nb")));
}
