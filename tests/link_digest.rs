// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! [`link_digest`], `[integration.link]`'s consent digest (AstraPlugins `spec/games/manifest.md`
//! §2.6.1): SHA-256 of the RFC 8785 JCS of the table exactly as parsed, no default added.
//!
//! The rule and its two `link.digest` vectors are AstraPlugins PR #134's, at its head
//! [`PR_HEAD`], which is not merged yet: the vectors are final, so their inputs are vendored at
//! `tests/goldens/astraplugins-pr134/` (its NOTICE), apart from the pinned goldens, each held
//! here to its blob id at that commit. When #134 merges, the goldens are re-pinned to its merge
//! commit and these read the pinned `vectors.json` (`docs/ENGINE-DESCRIPTORS-DEBT.md` row 53).
//!
//! Every other expected digest below was computed by two other JCS implementations (Python's
//! `json.dumps(ensure_ascii=False)` with keys sorted by UTF-16, and JavaScript's
//! `JSON.stringify` with `Object.keys().sort()`), never by this crate.

#[path = "common/spec_goldens.rs"]
mod spec_goldens;

use std::borrow::Cow;
use std::fs;
use std::path::Path;

use astra_game_launcher::{LauncherError, Manifest, codes, link_digest, toml_newlines};

/// AstraPlugins PR #134's head: the commit the vendored inputs are copied from.
const PR_HEAD: &str = "7aaa37bb36d7f36662a2f88ef75c3dbafb3f3327";

/// One `link.digest` vector of #134's `testdata/games/vectors.json`.
struct Vector {
    name: &'static str,
    /// Its input, in `tests/goldens/astraplugins-pr134/`.
    file: &'static str,
    /// Its path in AstraPlugins, below `testdata/games/`.
    spec_path: &'static str,
    /// The input's git blob id at [`PR_HEAD`].
    blob: &'static str,
    digest: &'static str,
}

const VECTORS: [Vector; 2] = [
    Vector {
        name: "game-specific-link",
        file: "game-specific-link.toml",
        spec_path: "manifests/valid/game-specific-link.toml",
        blob: "dc08074cf101f51a4626340a6f1dc291455ba369",
        digest: "e26a440ebf1c3cacc9891fc8b031d9c0d8cbde9276ceeba68d1866a972a35ea2",
    },
    // Keys out of order, non-ASCII, a quote, a backslash, an empty array, absent keys absent.
    Vector {
        name: "edge",
        file: "edge.toml",
        spec_path: "link/digest/edge.toml",
        blob: "47b2fb4cef729eee711e73899363c5a4098e0200",
        digest: "e1ddacccb882c0a240d26f933b883cc9ea23da7e50cfb3679c432539d5b95dd2",
    },
];

fn fixture(file: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/astraplugins-pr134")
        .join(file);
    fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `[integration.link]` of a TOML text, parsed as it is (not every input is a whole manifest).
fn link_of(text: &str) -> Option<toml::Value> {
    let t: toml::Table = toml::from_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    t.get("integration")?.get("link").cloned()
}

fn digest_of(text: &str) -> Result<Option<String>, LauncherError> {
    link_digest(link_of(text).as_ref())
}

#[test]
fn both_vectors_of_the_spec_are_this_digest() {
    for v in &VECTORS {
        let bytes = fixture(v.file);
        assert_eq!(
            spec_goldens::git_id("blob", &bytes),
            v.blob,
            "{}: not {} at {PR_HEAD}",
            v.file,
            v.spec_path
        );
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            digest_of(&text).unwrap().as_deref(),
            Some(v.digest),
            "{}",
            v.name
        );
    }
}

/// The first vector is a whole manifest: through [`Manifest::parse`], as `gi-probe check` reads
/// it, the same digest. And it is the golden vendored at the pin byte for byte, which is what
/// lets `tests/gi_probe.rs` hold `check`'s report to the vector.
#[test]
fn a_parsed_manifest_gives_the_vectors_digest() {
    let v = &VECTORS[0];
    let bytes = fixture(v.file);
    let pinned = spec_goldens::root()
        .join("testdata/games")
        .join(v.spec_path);
    assert_eq!(bytes, fs::read(&pinned).unwrap(), "{}", pinned.display());
    let m = Manifest::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(link_digest(m.link()).unwrap().as_deref(), Some(v.digest));
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
        Some(VECTORS[1].digest)
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
/// rewritten; each expected digest is #134's reference (`tomllib` and `tools/contentlite.py`'s
/// JCS) and JavaScript's JCS over the same parse.
#[test]
fn a_crlf_checkout_is_the_lf_files_digest() {
    const LF_IN_THE_STRING: &str =
        "24cc58cc7f228aecde0af2c009ba96b45b1dec8840010b9ce625c1398abc8de1";
    const CRLF_IN_THE_STRING: &str =
        "6980874cd460b6f7eb37b074eb03fcb8d22fabea552c8c6404444dc355b3052f";
    let golden = String::from_utf8(fixture(VECTORS[0].file)).unwrap();
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
