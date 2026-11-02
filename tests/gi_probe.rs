// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe` (`src/bin/gi-probe/`): canary K4's launcher half, the replica of Astra 0.2.7 held to
//! the code 0.2.7 shipped, the first-party manifests held to launcher 0.3.1 and that replica, and
//! the two subcommands run as a release script runs them.
//!
//! - K4: `compat-0.2.7.json` is exactly the `GiSource` and `FallbackPin` literals of
//!   `tests/fixtures/astra-0.2.7/pins.rs`, which is byte for byte tag v0.2.7's (its `SOURCE`).
//! - Every item `src/bin/gi-probe/v027.rs` says it copies is the vendored item, token for token,
//!   at the lines its doc cites; v0.2.7's own tests of those items, ported, pass against it.
//! - 0.3.1 parses every first-party manifest (`tests/fixtures/first-party/`, written by
//!   `tools/fetch-first-party-manifests.sh`), and each passes 0.2.7's check for its source.
//! - `compat` passes a well-formed release of every source and fails each kind of break at the
//!   step that owns it; `validate-descriptor` passes plan B §1's Source 2 descriptor and refuses
//!   the anchorless, the runtime-anchored and the too broad, wherever its rules look for a
//!   program (`tests/descriptors/hostile/`, and debt row 33's weakness pinned).
//! - `check` answers every manifest golden of the specification as `tests/goldens.rs` judges the
//!   library (the same `SPEC_DEFECTS_AT_PIN`), and on release zips: Link files, reserved ids by
//!   provenance, the descriptor's negative folders, `built_on`, `code_changed`, and the release's
//!   own content — every content golden of the specification (`testdata/games/content/`) with the
//!   verdict and the code `vectors.json` gives, every hostile content fixture refused
//!   (`tests/common/content_fixtures.rs`, which `tests/content.rs` runs through the library).
//! - `capture` records what detection lists and only the bytes the rules read, refuses a
//!   recording over 1 MiB, and replays as the folder it was taken from; the format's reader
//!   (`tests/corpus/replay.rs`) refuses what breaks it; `validate-descriptor --corpus` refuses a
//!   descriptor claiming recordings of two engines.

#![cfg(feature = "gi-probe")]

mod common;
#[path = "common/content_fixtures.rs"]
mod content_fixtures;
#[path = "common/spec_goldens.rs"]
mod spec_goldens;

#[path = "corpus/replay.rs"]
mod replay;
#[allow(dead_code)]
#[path = "../src/bin/gi-probe/v027.rs"]
mod v027;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{E, Tmp, sha, zip};
use launcher_031::Manifest;
use serde_json::{Value, json};
use v027::pins::{self, GiSource, SourceTarget};
use v027::releases::{
    GhAsset, GhRelease, NoRelease, RELEASE_MISMATCH, canonical_url, check_manifest,
    fallback_gi_with, pick, pick_with, version_after,
};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROBE: &str = env!("CARGO_BIN_EXE_gi-probe");

fn path(rel: &str) -> PathBuf {
    Path::new(ROOT).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(path(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Reading Rust source text: comments and literal contents masked, line numbers kept.
// ---------------------------------------------------------------------------------------------

/// `text`: the source with its comments blanked. `code`: also every string and char literal's
/// contents blanked (the quotes kept). Both are the source's length, newlines in place.
struct Masked {
    text: String,
    code: String,
}

fn mask(src: &str) -> Masked {
    let b = src.as_bytes();
    let mut text = b.to_vec();
    let mut code = b.to_vec();
    let blank = |v: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut v[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            let end = b[i..]
                .iter()
                .position(|c| *c == b'\n')
                .map_or(b.len(), |n| i + n);
            blank(&mut text, i, end);
            blank(&mut code, i, end);
            i = end;
        } else if b[i..].starts_with(b"/*") {
            let end = src[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
            blank(&mut text, i, end);
            blank(&mut code, i, end);
            i = end;
        } else if b[i] == b'r' && (i == 0 || !ident(b[i - 1])) && {
            let hashes = b[i + 1..].iter().take_while(|c| **c == b'#').count();
            b.get(i + 1 + hashes) == Some(&b'"')
        } {
            let hashes = b[i + 1..].iter().take_while(|c| **c == b'#').count();
            let open = i + 1 + hashes + 1;
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[open..].find(&close).map_or(b.len(), |n| open + n);
            blank(&mut code, open, end);
            i = end + close.len();
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            blank(&mut code, i + 1, j);
            i = j + 1;
        } else if b[i] == b'\'' {
            // A char literal (`'v'`, `'\''`), else a lifetime.
            if b.get(i + 1) == Some(&b'\\') {
                let end = b[i + 2..]
                    .iter()
                    .position(|c| *c == b'\'')
                    .map_or(b.len(), |n| i + 3 + n);
                blank(&mut code, i + 1, end - 1);
                i = end;
            } else if let Some(n) = src[i + 1..].chars().next()
                && src[i + 1 + n.len_utf8()..].starts_with('\'')
            {
                let end = i + 1 + n.len_utf8();
                blank(&mut code, i + 1, end);
                i = end + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    Masked {
        text: String::from_utf8(text).unwrap(),
        code: String::from_utf8(code).unwrap(),
    }
}

/// The 1-based line `pos` sits on.
fn line_of(src: &str, pos: usize) -> usize {
    src[..pos].matches('\n').count() + 1
}

/// The position just past the brace closing the one at `open` (in `code`).
fn matching_brace(code: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (i, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + i + 1;
                }
            }
            _ => {}
        }
    }
    panic!("an unclosed brace at {open}");
}

/// Where the first item `<kind> <name>` (`fn`, `struct`, `enum`, `const`, `impl`) is in `m`,
/// before `limit`: its keyword's position and the end of its body (its `;`, or the brace closing
/// it).
fn find_item(m: &Masked, kind: &str, name: &str, limit: usize) -> Option<(usize, usize)> {
    let pat = format!("{kind} {name}");
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let (start, _) = m.code[..limit].match_indices(&pat).find(|(i, _)| {
        let before = m.code[..*i].chars().next_back();
        let after = m.code[i + pat.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })?;
    if kind == "const" {
        let end = start + m.code[start..].find(';')? + 1;
        return Some((start, end));
    }
    let open = start + m.code[start..].find('{')?;
    Some((start, matching_brace(&m.code, open)))
}

/// The item's text, every comment, every white space and every trailing comma gone (rustfmt
/// adds or drops those with the line's width): what "verbatim" compares.
fn tokens(m: &Masked, span: (usize, usize)) -> String {
    let bare: String = m.text[span.0..span.1]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    bare.replace(",}", "}")
        .replace(",)", ")")
        .replace(",]", "]")
}

/// Where a vendored file's own tests begin (nothing after it is 0.2.7's shipped code).
fn shipped_end(src: &str) -> usize {
    src.find("#[cfg(test)]\nmod tests").unwrap_or(src.len())
}

// ---------------------------------------------------------------------------------------------
// The vendored files and K4.
// ---------------------------------------------------------------------------------------------

/// `tests/fixtures/astra-0.2.7/SOURCE`: the tag, the commit, and each file's digest.
#[test]
fn the_vendored_astra_files_are_the_tags_byte_for_byte() {
    let source = read("tests/fixtures/astra-0.2.7/SOURCE");
    let lines: Vec<&str> = source.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(lines[0], "tag v0.2.7");
    assert_eq!(lines[1], "commit 6fea01a01c1ca4209a967ff69d9afa065bdb22bb");
    let mut files = BTreeSet::new();
    for l in &lines[2..] {
        let parts: Vec<&str> = l.split("  ").collect();
        let [digest, file, original] = parts.as_slice() else {
            panic!("SOURCE line {l:?}");
        };
        assert_eq!(
            *original,
            format!("astra-rs/astra-daemon/src/games/{file}"),
            "{file}"
        );
        let bytes = fs::read(path(&format!("tests/fixtures/astra-0.2.7/{file}"))).unwrap();
        assert_eq!(sha(&bytes), *digest, "{file} is not the tag's");
        files.insert(*file);
    }
    assert_eq!(
        files,
        BTreeSet::from(["pins.rs", "releases.rs", "service.rs"])
    );
    // The table names the same release.
    assert_eq!(pins::astra().tag, "v0.2.7");
    assert_eq!(
        pins::astra().commit,
        "6fea01a01c1ca4209a967ff69d9afa065bdb22bb"
    );
    assert_eq!(
        pins::astra().file,
        "astra-rs/astra-daemon/src/games/pins.rs"
    );
    assert_eq!(pins::launcher().version, "0.3.1");
    assert_eq!(
        pins::launcher().rev,
        "9aa7874b60f6b10bd67411b7aad40fcc968b78bc"
    );
}

/// `key: value` pairs of a struct literal's body (`text`/`code` of the same span), split at
/// its top-level commas.
fn fields(text: &str, code: &str) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut push = |from: usize, to: usize| {
        let (t, c) = (text[from..to].trim(), code[from..to].trim());
        if t.is_empty() {
            return;
        }
        let colon = c.find(':').unwrap_or_else(|| panic!("no `key:` in {t:?}"));
        let lead = text[from..to].len() - text[from..to].trim_start().len();
        let at = from + lead + colon;
        let key = text[from..at].trim().to_owned();
        let value = (
            text[at + 1..to].trim().to_owned(),
            code[at + 1..to].trim().to_owned(),
        );
        assert!(out.insert(key.clone(), value).is_none(), "{key} twice");
    };
    for (i, ch) in code.char_indices() {
        match ch {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                push(start, i);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(start, code.len());
    out
}

fn string_lit(v: &str) -> String {
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or_else(|| panic!("not a string literal: {v:?}"))
        .to_owned()
}

/// `Some(X)` → `X`, `None` → `None`.
fn option(v: &(String, String)) -> Option<(String, String)> {
    if v.0 == "None" {
        return None;
    }
    let inner = |s: &str| {
        s.strip_prefix("Some(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or_else(|| panic!("not an Option: {s:?}"))
            .to_owned()
    };
    Some((inner(&v.0), inner(&v.1)))
}

/// `Path { a: …, b: … }` → (`Path`, its fields).
fn struct_lit(v: &(String, String)) -> (String, BTreeMap<String, (String, String)>) {
    let open =
        v.1.find('{')
            .unwrap_or_else(|| panic!("not a struct literal: {:?}", v.0));
    let close = v.1.rfind('}').unwrap();
    (
        v.0[..open].trim().to_owned(),
        fields(&v.0[open + 1..close], &v.1[open + 1..close]),
    )
}

/// Every `GiSource { … }` literal of the shipped part of `pins.rs`, as `compat-0.2.7.json`
/// spells a row, and how many `FallbackPin { … }` literals there are.
fn pins_literals(src: &str) -> (Vec<Value>, usize) {
    let m = mask(src);
    let limit = shipped_end(src);
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let literal_starts = |ty: &str| -> Vec<usize> {
        let pat = format!("{ty} {{");
        m.code[..limit]
            .match_indices(&pat)
            .map(|(i, _)| i)
            .filter(|i| {
                let before = m.code[..*i].trim_end();
                !(before.ends_with("struct") || before.ends_with("impl") || before.ends_with("->"))
            })
            .collect()
    };
    let mut consts: BTreeMap<String, String> = BTreeMap::new();
    let mut rows = Vec::new();
    for start in literal_starts("GiSource") {
        let open = start + m.code[start..].find('{').unwrap();
        let close = matching_brace(&m.code, open) - 1;
        let f = fields(&m.text[open + 1..close], &m.code[open + 1..close]);
        let s = |k: &str| string_lit(&f[k].0);
        let id = s("id");
        // `pub const NAME: GiSource = GiSource {`
        let head = m.code[..start].trim_end();
        if let Some(head) = head.strip_suffix(": GiSource =") {
            let name: String = head
                .chars()
                .rev()
                .take_while(|c| ident(*c))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            consts.insert(name, id.clone());
        }
        let (ty, t) = struct_lit(&f["target"]);
        let target = match ty.as_str() {
            "SourceTarget::Runner" => json!({ "runner": { "engine": string_lit(&t["engine"].0) } }),
            "SourceTarget::Game" => json!({ "game": {
                "appid": string_lit(&t["appid"].0),
                "exe": option(&t["exe"]).map(|e| string_lit(&e.0)),
            }}),
            other => panic!("target {other:?}"),
        };
        let fallback = option(&f["fallback"]).map(|p| {
            let (ty, p) = struct_lit(&p);
            assert_eq!(ty, "FallbackPin");
            json!({
                "tag": string_lit(&p["tag"].0),
                "sha256": string_lit(&p["sha256"].0),
                "prerelease": match p["prerelease"].0.as_str() {
                    "true" => true,
                    "false" => false,
                    other => panic!("prerelease {other:?}"),
                },
            })
        });
        let predecessor = option(&f["predecessor"]).map(|p| {
            p.0.strip_prefix('&')
                .unwrap_or_else(|| panic!("predecessor {:?}", p.0))
                .to_owned()
        });
        let keys: BTreeSet<&str> = f.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "name",
                "id",
                "repo",
                "repo_id",
                "tag_prefix",
                "asset",
                "target",
                "fallback",
                "predecessor"
            ]),
            "{id}: GiSource's fields"
        );
        rows.push(json!({
            "id": id,
            "repo": s("repo"),
            "repo_id": f["repo_id"].0.replace('_', "").parse::<u64>().unwrap(),
            "tag_prefix": s("tag_prefix"),
            "asset": s("asset"),
            "target": target,
            "fallback": fallback,
            "predecessor": predecessor,
        }));
    }
    for r in &mut rows {
        if let Some(name) = r["predecessor"].as_str().map(str::to_owned) {
            r["predecessor"] = json!(consts.get(&name).unwrap_or_else(|| panic!("{name}")));
        }
    }
    (rows, literal_starts("FallbackPin").len())
}

/// **K4, the launcher half.** `compat-0.2.7.json` is every `GiSource` literal of the pins.rs
/// Astra 0.2.7 shipped, in its order, field for field, and every `FallbackPin` literal is one of
/// its rows' (none is left out, none made up).
#[test]
fn k4_the_compat_table_is_every_source_and_pin_of_the_shipped_pins_rs() {
    let (literals, pin_literals) = pins_literals(&read("tests/fixtures/astra-0.2.7/pins.rs"));
    let table: Value = serde_json::from_str(&read("src/bin/gi-probe/compat-0.2.7.json")).unwrap();
    let rows = table["sources"].as_array().unwrap();
    assert_eq!(rows, &literals, "compat-0.2.7.json against pins.rs");
    // Thirteen sources (three runners, GTA III's predecessor, nine games), ten pins.
    assert_eq!(literals.len(), 13);
    assert_eq!(pin_literals, 10);
    assert_eq!(
        rows.iter().filter(|r| !r["fallback"].is_null()).count(),
        pin_literals
    );
    // The replica loads the same table.
    assert_eq!(pins::sources().len(), rows.len());
    for (s, r) in pins::sources().iter().zip(rows) {
        assert_eq!(s.id, r["id"]);
        assert_eq!(s.predecessor.map(|p| p.id), r["predecessor"].as_str());
    }
}

// ---------------------------------------------------------------------------------------------
// The replica is 0.2.7's code.
// ---------------------------------------------------------------------------------------------

/// The items `v027.rs` copies verbatim: `(file, kind, name)`.
const VERBATIM: &[(&str, &str, &str)] = &[
    ("pins.rs", "enum", "SourceTarget"),
    ("pins.rs", "struct", "FallbackPin"),
    ("pins.rs", "fn", "appid"),
    ("pins.rs", "fn", "exe"),
    ("pins.rs", "fn", "is_runner"),
    ("pins.rs", "fn", "lineage"),
    ("pins.rs", "fn", "shares_its_repository"),
    ("pins.rs", "fn", "normalize_repo"),
    ("releases.rs", "const", "RELEASE_NOT_FOUND"),
    ("releases.rs", "const", "RELEASE_MISMATCH"),
    ("releases.rs", "struct", "GhRelease"),
    ("releases.rs", "struct", "GhAsset"),
    ("releases.rs", "struct", "Gi"),
    ("releases.rs", "enum", "NoRelease"),
    ("releases.rs", "impl", "NoRelease"),
    ("releases.rs", "fn", "version_after"),
    ("releases.rs", "fn", "canonical_url"),
    ("releases.rs", "fn", "followed"),
    ("releases.rs", "fn", "pick"),
    ("releases.rs", "fn", "pick_with"),
    ("releases.rs", "fn", "carries"),
    ("releases.rs", "fn", "fallback_gi_with"),
    ("releases.rs", "fn", "check_manifest"),
    ("service.rs", "fn", "consent_needed_for"),
    ("service.rs", "fn", "game_files_match"),
    ("service.rs", "fn", "same_consented_file"),
    ("service.rs", "fn", "same_repo"),
    ("service.rs", "fn", "compared_digest"),
];

/// The `file:A-B` (or `file:A`) a doc citation in front of `pos` names: the doc lines directly
/// above the item (attributes between them skipped), the first of which begins with it.
fn citation(m: &Masked, src: &str, pos: usize) -> Option<(String, usize, usize)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut l = line_of(src, pos) - 1; // 0-based line of the item
    let mut doc_top = None;
    while l > 0 {
        l -= 1;
        let t = lines[l].trim_start();
        if t.starts_with("#[") {
            continue;
        }
        if t.starts_with("///") {
            doc_top = Some(l);
            continue;
        }
        break;
    }
    let _ = m;
    let first = lines[doc_top?].trim_start().strip_prefix("/// `")?;
    let cite = &first[..first.find('`')?];
    let (file, range) = cite.split_once(':')?;
    let (a, b) = match range.split_once('-') {
        Some((a, b)) => (a.parse().ok()?, b.parse().ok()?),
        None => (range.parse().ok()?, range.parse().ok()?),
    };
    Some((file.to_owned(), a, b))
}

/// **The replica is 0.2.7's code.** Each item `v027.rs` copies equals the vendored one token for
/// token (comments and white space aside), and every `file:lines` citation that opens a doc in
/// `v027.rs` names exactly the lines the item it documents spans in the vendored file. A replica
/// that drifted (say, one that took a version other than its tag's) fails here first.
#[test]
fn the_replica_is_v027s_code_token_for_token_at_the_lines_it_cites() {
    let replica_src = read("src/bin/gi-probe/v027.rs");
    let replica = mask(&replica_src);
    let vendored: BTreeMap<&str, (String, Masked)> = ["pins.rs", "releases.rs", "service.rs"]
        .into_iter()
        .map(|f| {
            let src = read(&format!("tests/fixtures/astra-0.2.7/{f}"));
            let m = mask(&src);
            (f, (src, m))
        })
        .collect();
    for (file, kind, name) in VERBATIM {
        let (src, m) = &vendored[file];
        let theirs = find_item(m, kind, name, shipped_end(src))
            .unwrap_or_else(|| panic!("{file}: no {kind} {name}"));
        let ours = find_item(&replica, kind, name, replica_src.len())
            .unwrap_or_else(|| panic!("v027.rs: no {kind} {name}"));
        assert_eq!(
            tokens(&replica, ours),
            tokens(m, theirs),
            "v027.rs's {kind} {name} is not {file}'s"
        );
        let cited = citation(&replica, &replica_src, ours.0)
            .unwrap_or_else(|| panic!("v027.rs's {kind} {name} cites no lines"));
        assert_eq!(
            cited,
            (
                file.to_string(),
                line_of(src, theirs.0),
                line_of(src, theirs.1 - 1)
            ),
            "v027.rs's citation for {kind} {name}"
        );
    }
    // Every citation that opens a doc names the lines of the item it documents, copied or not.
    let mut checked = 0;
    for (i, line) in replica_src.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix("/// `") else {
            continue;
        };
        let Some((file, _)) = rest.split_once(':') else {
            continue;
        };
        let Some((src, m)) = vendored.get(file) else {
            continue;
        };
        // The item below the doc: the first line that is neither doc nor attribute.
        let below = replica_src
            .lines()
            .enumerate()
            .skip(i + 1)
            .find(|(_, l)| {
                let t = l.trim_start();
                !t.starts_with("///") && !t.starts_with("#[")
            })
            .unwrap();
        let pos = replica_src
            .lines()
            .take(below.0)
            .map(|l| l.len() + 1)
            .sum::<usize>();
        let (kind, name) = item_head(below.1);
        let cited = citation(&replica, &replica_src, pos).unwrap();
        let theirs = find_item(m, kind, &name, shipped_end(src))
            .unwrap_or_else(|| panic!("{file} has no {kind} {name}, which v027.rs cites"));
        assert_eq!(
            (cited.1, cited.2),
            (line_of(src, theirs.0), line_of(src, theirs.1 - 1)),
            "v027.rs's citation for {kind} {name}"
        );
        checked += 1;
    }
    assert!(checked >= VERBATIM.len(), "{checked} citations checked");
}

/// `(kind, name)` of an item's first line (`pub const fn exe(&self)` → `("fn", "exe")`).
fn item_head(line: &str) -> (&'static str, String) {
    for kind in ["fn", "struct", "enum", "const", "impl"] {
        let pat = format!("{kind} ");
        let mut rest = line;
        while let Some(i) = rest.find(&pat) {
            let before = rest[..i].chars().next_back();
            if !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                let after = &rest[i + pat.len()..];
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && !(kind == "const" && name == "fn") {
                    return (kind, name);
                }
            }
            rest = &rest[i + pat.len()..];
        }
    }
    panic!("no item on {line:?}");
}

// ---------------------------------------------------------------------------------------------
// v0.2.7's own tests of the replicated functions, ported (GiSource has no `name`: ids instead).
// ---------------------------------------------------------------------------------------------

fn src(id: &str) -> GiSource {
    pins::source(id).unwrap_or_else(|| panic!("no source {id}"))
}

/// v0.2.7 `releases.rs` `test_release` (:673-684).
fn release(source: &GiSource, tag: &str, n: u8) -> GhRelease {
    GhRelease {
        tag_name: tag.to_owned(),
        draft: false,
        prerelease: false,
        assets: vec![GhAsset {
            name: source.asset.to_owned(),
            browser_download_url: canonical_url(source.repo, tag, source.asset),
            digest: Some(format!("sha256:{}", format!("{n:02x}").repeat(32))),
        }],
    }
}

/// v0.2.7 `releases.rs` `tests::manifest_toml`.
fn manifest_toml(id: &str, repo: &str, target: &str, version: &str) -> Manifest {
    Manifest::parse(&format!(
        "schema = 1\n[integration]\nid = \"{id}\"\nname = \"x\"\nversion = \"{version}\"\nauthors = [\"a\"]\nlicense = \"MIT\"\nrepo = \"{repo}\"\n[target]\n{target}platforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n"
    ))
    .unwrap()
}

/// v0.2.7 `releases.rs` `tests::game_target`.
fn game_target(appid: Option<u32>, exe: &str) -> String {
    let appid = appid
        .map(|a| format!("steam_appid = {a}\n"))
        .unwrap_or_default();
    format!("{appid}exe = \"{exe}\"\n")
}

/// v0.2.7 `releases.rs` `tests::runner_target`.
fn runner_target(engine: Option<&str>, appid: Option<u32>) -> String {
    let engine = engine
        .map(|e| format!("engine = \"{e}\"\n"))
        .unwrap_or_default();
    let appid = appid
        .map(|a| format!("steam_appid = {a}\n"))
        .unwrap_or_default();
    format!("{engine}{appid}exe = \"*\"\n")
}

/// Ported: v0.2.7 `releases.rs` `tests::every_source_follows_its_newest_stable_release_and_never_a_prerelease`.
#[test]
fn every_source_follows_its_newest_stable_release_and_never_a_prerelease() {
    for s in pins::all_sources() {
        let mut pre = release(&s, "v9.0.0", 0x90);
        pre.prerelease = true;
        let mut draft = release(&s, "v8.0.0", 0x80);
        draft.draft = true;
        let listing = vec![
            pre,
            draft,
            release(&s, "v7.0.0-rc.1", 0x70),
            release(&s, "v0.9.0", 0x09),
            release(&s, "v0.10.0", 0x10),
            release(&s, "latest", 0x60),
        ];
        let gi = pick_with(&listing, &s, false).unwrap_or_else(|e| panic!("{}: {e:?}", s.id));
        assert_eq!(
            gi.tag, "v0.10.0",
            "{}: semver order, not listing order",
            s.id
        );
        assert_eq!(gi.version, "0.10.0", "{}", s.id);
        assert_eq!(gi.sha256, "10".repeat(32), "{}", s.id);
        assert_eq!(
            gi.url,
            format!(
                "https://github.com/{}/releases/download/v0.10.0/{}",
                s.repo, s.asset
            ),
            "{}",
            s.id
        );
        assert_eq!(gi.source, s);
        // `pick` is a release build's: the same.
        assert_eq!(pick(&listing, &s), Ok(gi));
    }
}

/// Ported: v0.2.7 `releases.rs` `tests::with_the_test_build_knob_the_newest_of_stable_and_prerelease_is_picked`.
#[test]
fn with_the_test_build_knob_the_newest_of_stable_and_prerelease_is_picked() {
    for s in pins::all_sources() {
        let mut pre = release(&s, "v0.9.0", 0x90);
        pre.prerelease = true;
        let mut draft = release(&s, "v1.0.0", 0xa0);
        draft.draft = true;
        let listing = vec![draft.clone(), pre.clone(), release(&s, "v0.8.1", 0x81)];
        assert_eq!(
            pick_with(&listing, &s, true).unwrap().tag,
            "v0.9.0",
            "{}",
            s.id
        );
        assert_eq!(
            pick_with(&listing, &s, false).unwrap().tag,
            "v0.8.1",
            "{}",
            s.id
        );
        let rc = vec![pre.clone(), release(&s, "v0.9.1-rc.1", 0x91)];
        let gi = pick_with(&rc, &s, true).unwrap();
        assert_eq!(
            (gi.tag.as_str(), gi.version.as_str()),
            ("v0.9.1-rc.1", "0.9.1-rc.1")
        );
        let newer = vec![pre, release(&s, "v0.10.0", 0x10)];
        assert_eq!(pick_with(&newer, &s, true).unwrap().tag, "v0.10.0");
        assert_eq!(pick_with(&[draft], &s, true), Err(NoRelease::NoTag));
    }
}

/// Ported: v0.2.7 `releases.rs` `tests::every_sources_real_fallback_manifest_passes_its_own_check`.
#[test]
fn every_sources_real_fallback_manifest_passes_its_own_check() {
    let real: &[(&str, &str, &str)] = &[
        (
            "astra.unity",
            "astra-bepinex",
            "engine = \"unity-mono\"\nexe = \"*\"\n",
        ),
        (
            "astra.unity-il2cpp",
            "astra-bepinex",
            "engine = \"unity-il2cpp\"\nexe = \"*\"\n",
        ),
        (
            "astra.unreal",
            "astra-reshade",
            "engine = \"unreal\"\nexe = \"*\"\n",
        ),
        (
            "astra.lethal-company",
            "astra-lethal",
            "steam_appid = 1966720\nexe = \"Lethal Company.exe\"\n",
        ),
        (
            "astra.content-warning",
            "astra-content-warning",
            "steam_appid = 2881650\nexe = \"Content Warning.exe\"\n",
        ),
        (
            "astra.peak",
            "astra-peak",
            "steam_appid = 3527290\nexe = \"PEAK.exe\"\n",
        ),
        (
            "astra.repo",
            "astra-repo",
            "steam_appid = 3241660\nexe = \"REPO.exe\"\n",
        ),
        (
            "astra.death-stranding",
            "astra-in-death-stranding",
            "steam_appid = 1850570\nexe = \"ds.exe\"\n",
        ),
        (
            "astra.death-stranding-2",
            "astra-in-death-stranding",
            "steam_appid = 3280350\nexe = \"DS2.exe\"\n",
        ),
        (
            "astra.unreal.gta3-definitive-edition",
            "astra-reshade",
            "engine = \"unreal\"\nexe = \"Gameface/Binaries/Win64/LibertyCity.exe\"\nsteam_appid = 1546970\n",
        ),
    ];
    assert_eq!(
        real.len(),
        pins::all_sources().filter(|s| s.fallback.is_some()).count()
    );
    for (id, repo, target) in real {
        let source = src(id);
        let gi = fallback_gi_with(&source, true).unwrap();
        let m = manifest_toml(
            id,
            &format!("https://github.com/mihailinl/{repo}"),
            target,
            &gi.version,
        );
        let appid = source.appid().unwrap_or("4513480");
        check_manifest(&gi, appid, &m).unwrap_or_else(|e| panic!("{id}: {e:?}"));
    }
}

fn field_of(e: &launcher_031::LauncherError) -> Option<&str> {
    e.params.get("field").map(String::as_str)
}

/// Ported: v0.2.7 `releases.rs` `tests::a_runners_package_must_name_the_runners_engine_and_no_game`.
#[test]
fn a_runners_package_must_name_the_runners_engine_and_no_game() {
    let unreal = src("astra.unreal");
    let repo = "https://github.com/mihailinl/astra-reshade";
    let gi = pick(&[release(&unreal, "v0.3.0", 3)], &unreal).unwrap();
    let ok = manifest_toml(
        "astra.unreal",
        repo,
        &runner_target(Some("unreal"), None),
        "0.3.0",
    );
    assert!(check_manifest(&gi, "4513480", &ok).is_ok());
    for (target, field) in [
        (runner_target(Some("unity-mono"), None), "target.engine"),
        (runner_target(Some("unity-il2cpp"), None), "target.engine"),
        (runner_target(None, None), "target.engine"),
        (
            runner_target(Some("unreal"), Some(4513480)),
            "target.steam_appid",
        ),
    ] {
        let m = manifest_toml("astra.unreal", repo, &target, "0.3.0");
        let e = check_manifest(&gi, "4513480", &m).unwrap_err();
        assert_eq!(e.code, RELEASE_MISMATCH);
        assert_eq!(field_of(&e), Some(field), "{target}");
    }
    let mono_source = src("astra.unity");
    let mono = pick(&[release(&mono_source, "v0.9.0", 9)], &mono_source).unwrap();
    let bepinex = "https://github.com/mihailinl/astra-bepinex";
    let il2cpp_zip = manifest_toml(
        "astra.unity",
        bepinex,
        &runner_target(Some("unity-il2cpp"), None),
        "0.9.0",
    );
    assert_eq!(
        field_of(&check_manifest(&mono, "1", &il2cpp_zip).unwrap_err()),
        Some("target.engine")
    );
}

/// Ported: v0.2.7 `releases.rs` `tests::a_games_package_must_name_the_source_and_this_game`.
#[test]
fn a_games_package_must_name_the_source_and_this_game() {
    const DS1: &str = "1850570";
    let s = src("astra.death-stranding");
    let gi = pick(&[release(&s, "v0.10.0", 0x1a)], &s).unwrap();
    let url = "https://github.com/mihailinl/astra-in-death-stranding";
    let target = |appid| game_target(appid, "ds.exe");
    for version in ["0.10.0", "v0.10.0"] {
        let m = manifest_toml(
            "astra.death-stranding",
            url,
            &target(Some(1850570)),
            version,
        );
        assert!(check_manifest(&gi, DS1, &m).is_ok(), "{version}");
    }
    for (m, field) in [
        (
            manifest_toml(
                "astra.death-stranding",
                url,
                &target(Some(1850570)),
                "0.9.0",
            ),
            "integration.version",
        ),
        (
            manifest_toml(
                "astra.death-stranding-2",
                url,
                &target(Some(1850570)),
                "0.10.0",
            ),
            "integration.id",
        ),
        (
            manifest_toml(
                "astra.death-stranding",
                "https://github.com/someone/fork",
                &target(Some(1850570)),
                "0.10.0",
            ),
            "integration.repo",
        ),
        (
            manifest_toml(
                "astra.death-stranding",
                url,
                &target(Some(3280350)),
                "0.10.0",
            ),
            "target.steam_appid",
        ),
        (
            manifest_toml("astra.death-stranding", url, &target(None), "0.10.0"),
            "target.steam_appid",
        ),
    ] {
        let e = check_manifest(&gi, DS1, &m).unwrap_err();
        assert_eq!(e.code, RELEASE_MISMATCH);
        assert_eq!(field_of(&e), Some(field));
    }
    let lethal = src("astra.lethal-company");
    let gi = pick(&[release(&lethal, "v0.3.0", 3)], &lethal).unwrap();
    let wrong = manifest_toml(
        "astra.lethal-company",
        "https://github.com/mihailinl/astra-lethal",
        &game_target(Some(2881650), "Lethal Company.exe"),
        "0.3.0",
    );
    let e = check_manifest(&gi, "1966720", &wrong).unwrap_err();
    assert_eq!(
        (e.code, field_of(&e)),
        (RELEASE_MISMATCH, Some("target.steam_appid"))
    );
    let right = manifest_toml(
        "astra.lethal-company",
        "https://github.com/mihailinl/astra-lethal",
        &game_target(Some(1966720), "Lethal Company.exe"),
        "0.3.0",
    );
    assert!(check_manifest(&gi, "1966720", &right).is_ok());
    assert!(check_manifest(&gi, "2881650", &right).is_err());
}

/// Ported, its `check_manifest` half: v0.2.7 `releases.rs`
/// `tests::the_trilogys_real_discovery_prerelease_on_both_sides_of_the_knob`.
#[test]
fn the_trilogys_real_discovery_prerelease_on_both_sides_of_the_knob() {
    let trilogy: Vec<GhRelease> = serde_json::from_str(
        r#"[{"tag_name":"v0.1.0-discovery","draft":false,"prerelease":true,"assets":[
            {"name":"astra-gi-gta3.zip","size":11732052,
             "digest":"sha256:67e6a2a73b392943f99f49aa7733bba5399214bf7dc5fe4e5536cb31e3ba3948",
             "browser_download_url":"https://github.com/mihailinl/astra-gta-trilogy/releases/download/v0.1.0-discovery/astra-gi-gta3.zip"}]}]"#,
    )
    .unwrap();
    let reshade: Vec<GhRelease> = serde_json::from_str(
        r#"[{"tag_name":"v0.2.4","draft":false,"prerelease":true,"assets":[
            {"name":"astra-gi-gta3.zip",
             "digest":"sha256:7de18bdf9ce14b95f3d193d02814aeb364726a9dc35d6af44a87950f2765cae8",
             "browser_download_url":"https://github.com/mihailinl/astra-reshade/releases/download/v0.2.4/astra-gi-gta3.zip"},
            {"name":"astra-gi.zip",
             "digest":"sha256:2383be40a68db93b516e3389d3fbee46d97022b374344d93ec940d93da2d3618",
             "browser_download_url":"https://github.com/mihailinl/astra-reshade/releases/download/v0.2.4/astra-gi.zip"}]},
          {"tag_name":"v0.2.1","draft":false,"prerelease":false,"assets":[
            {"name":"astra-gi.zip",
             "digest":"sha256:e89fcd792fe53816dd47bf6f7cafd436cd59b1c766d6dba5aeb853ac1a1c4eea",
             "browser_download_url":"https://github.com/mihailinl/astra-reshade/releases/download/v0.2.1/astra-gi.zip"}]}]"#,
    )
    .unwrap();
    let gta3 = src("astra.gta-trilogy.gta3");
    let old = src("astra.unreal.gta3-definitive-edition");
    assert_eq!(gta3.predecessor.copied(), Some(old));
    assert_eq!(pick_with(&trilogy, &gta3, false), Err(NoRelease::NoTag));
    assert_eq!(pick_with(&reshade, &old, false), Err(NoRelease::NoAsset));
    let gi = pick_with(&trilogy, &gta3, true).unwrap();
    assert_eq!(gi.version, "0.1.0-discovery");
    assert_eq!(pick_with(&reshade, &old, true).unwrap().tag, "v0.2.4");
    // The unreal runner's own v0.2.1 is what a release build takes from that listing.
    assert_eq!(pick(&reshade, &src("astra.unreal")).unwrap().tag, "v0.2.1");
    let real = Manifest::parse(
        r#"schema = 1

[integration]
id = "astra.gta-trilogy.gta3"
name = "Astra for GTA III - The Definitive Edition"
version = "0.1.0-discovery"
authors = ["mihailinl"]
license = "MIT AND BSD-3-Clause AND CC0-1.0"
repo = "https://github.com/mihailinl/astra-gta-trilogy"

[target]
exe = "Gameface/Binaries/Win64/LibertyCity.exe"
steam_appid = 1546970
platforms = ["windows-x64", "linux-proton"]
anti_cheat = "none"
"#,
    )
    .unwrap();
    assert_eq!(real.target.engine, None);
    check_manifest(&gi, "1546970", &real).unwrap();
    assert!(check_manifest(&gi, "1546990", &real).is_err());
    let old_gi = pick_with(&reshade, &old, true).unwrap();
    let e = check_manifest(&old_gi, "1546970", &real).unwrap_err();
    assert_eq!(field_of(&e), Some("integration.id"));
}

// ---------------------------------------------------------------------------------------------
// The first-party manifests.
// ---------------------------------------------------------------------------------------------

/// A first-party fixture's header: `# key: value` lines up to `# --- verbatim below ---`.
fn fixture_header(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        if line == "# --- verbatim below ---" {
            return out;
        }
        if let Some((k, v)) = line.strip_prefix("# ").and_then(|l| l.split_once(": ")) {
            out.insert(k.to_owned(), v.to_owned());
        }
    }
    panic!("a first-party fixture without its header's end");
}

/// **0.3.1 parses every first-party manifest.** Every `astra-gi.toml` the release Astra 0.2.7
/// installs today carries (and the newest prerelease's, where that is another release) parses
/// with launcher 0.3.1 and passes 0.2.7's `check_manifest` for its source and tag. Every source
/// with a compiled pin has its stable manifest here.
#[test]
fn launcher_031_parses_every_first_party_manifest_and_each_passes_its_sources_check() {
    let dir = path("tests/fixtures/first-party");
    let mut stable = BTreeSet::new();
    let mut seen = 0;
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let file = p.file_name().unwrap().to_str().unwrap().to_owned();
        let (id, pre) = if let Some(id) = file.strip_suffix(".pre.astra-gi.toml") {
            (id.to_owned(), true)
        } else if let Some(id) = file.strip_suffix(".astra-gi.toml") {
            (id.to_owned(), false)
        } else {
            panic!("{file}: not a fixture name");
        };
        let text = fs::read_to_string(&p).unwrap();
        let h = fixture_header(&text);
        assert_eq!(h["source"], id, "{file}");
        let source = src(&id);
        assert_eq!(h["repo"], source.repo, "{file}");
        assert_eq!(h["asset"], source.asset, "{file}");
        let m =
            Manifest::parse(&text).unwrap_or_else(|e| panic!("{file}: 0.3.1 refuses it: {e:?}"));
        // A semver pre-release tag is followed only where prereleases are.
        let version = version_after(&h["tag"], source.tag_prefix, pre)
            .unwrap_or_else(|| panic!("{file}: tag {} is not its source's", h["tag"]));
        let gi = v027::releases::Gi {
            source,
            tag: h["tag"].clone(),
            version: version.to_string(),
            sha256: h["sha256"].clone(),
            url: canonical_url(source.repo, &h["tag"], source.asset),
        };
        let appid = source.appid().unwrap_or("4513480");
        check_manifest(&gi, appid, &m).unwrap_or_else(|e| panic!("{file}: {e:?}"));
        if !pre {
            stable.insert(id);
        }
        seen += 1;
    }
    assert!(seen >= 10, "{seen} fixtures");
    for s in pins::all_sources() {
        if s.fallback.is_some() {
            assert!(
                stable.contains(s.id),
                "{}: no stable manifest fixture",
                s.id
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `gi-probe compat`, run as a release script runs it.
// ---------------------------------------------------------------------------------------------

/// Runs the probe; returns its exit code and report.
fn probe(args: &[&str]) -> (i32, Value) {
    let out = Command::new(PROBE).args(args).output().unwrap();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not a report ({e}): {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(report["schema"], "astra.gi-probe/1");
    assert_eq!(report["tool"]["version"], astra_game_launcher::VERSION);
    (out.status.code().unwrap(), report)
}

const DLL: &[u8] = b"MZ astra probe";

/// A release of `s` that Astra 0.2.7 takes: what a first-party release script hands compat.
fn manifest_for(s: &GiSource, version: &str) -> String {
    let target = match s.target {
        SourceTarget::Runner { engine } => format!("engine = \"{engine}\"\nexe = \"*\""),
        SourceTarget::Game { appid, exe } => {
            format!(
                "steam_appid = {appid}\nexe = \"{}\"",
                exe.unwrap_or("Game.exe")
            )
        }
    };
    format!(
        r#"schema = 1

[integration]
id = "{id}"
name = "gi-probe test release"
version = "{version}"
authors = ["tester"]
license = "MIT"
repo = "https://github.com/{repo}"

[integration.capabilities]
companion = ["presentation"]

[target]
{target}
platforms = ["windows-x64", "linux-proton"]
anti_cheat = "none"

[[files]]
from = "payload/**"
to = "${{profile}}/payload/"

[[game_files]]
from = "astra-probe.dll"
to = "${{game}}/astra-probe.dll"
sha256 = "{dll}"

[launch]
args = ["-AstraConfig", "${{profile}}/astra.ini"]

[[config_writes]]
file = "${{profile}}/astra.ini"
section = "bridge"
set = {{ Port = "${{bridge_port}}", Token = "${{bridge_token}}" }}
"#,
        id = s.id,
        repo = s.repo,
        dll = sha(DLL),
    )
}

/// The zip of `manifest`, with `payload` below `payload/`.
fn release_zip(manifest: &str, payload: &[&str]) -> Vec<u8> {
    let mut entries = vec![
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
    ];
    entries.extend(payload.iter().map(|p| E::File(p, b"payload")));
    zip(&entries)
}

const PAYLOAD: &[&str] = &["payload/a.txt", "payload/sub/b.txt"];

/// `zip` written as `<dir>/<name>`.
fn put(tmp: &Tmp, sub: &str, name: &str, zip: &[u8]) -> String {
    let p = tmp.join(sub).join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, zip).unwrap();
    p.to_str().unwrap().to_owned()
}

fn statuses(report: &Value) -> Vec<String> {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["status"].as_str().unwrap().to_owned())
        .collect()
}

/// The first step with a finding, and that finding's code.
fn first_failure(report: &Value) -> (u64, String) {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["step"].as_u64().unwrap(),
                f["code"].as_str().unwrap().to_owned(),
            )
        })
        .min()
        .unwrap_or_else(|| panic!("no finding: {report:#}"))
}

/// A well-formed release of EVERY source passes steps 1 to 4, on a game that fits its row; 5
/// and 6 say they were skipped without `--previous`.
#[test]
fn compat_passes_a_well_formed_release_of_every_source() {
    let tmp = Tmp::new("compat-every");
    for s in pins::all_sources() {
        let zip = release_zip(&manifest_for(&s, "1.2.3"), PAYLOAD);
        let z = put(&tmp, s.id, s.asset, &zip);
        let (code, r) = probe(&[
            "compat",
            "--zip",
            &z,
            "--source",
            s.id,
            "--tag",
            "v1.2.3",
            "--sha256",
            &sha(&zip),
        ]);
        assert_eq!(code, 0, "{}: {r:#}", s.id);
        assert_eq!(r["verdict"], "pass");
        assert_eq!(
            statuses(&r),
            ["pass", "pass", "pass", "pass", "skipped", "skipped"],
            "{}",
            s.id
        );
        assert_eq!(
            r["steps"][2]["stripped"],
            json!(["integration.capabilities"])
        );
        assert_eq!(r["input"]["digest_checked"], true);
        assert_eq!(r["integration"]["id"], s.id);
    }
}

/// A release's own content (`[[integration.content]]`, spec `manifest.md` §2.7) is data launcher
/// 0.3.1 reads past: `compat` passes a release that declares a part, with nothing 0.3.1 plans
/// otherwise once the table is taken out (step 3 names it), and `check` passes the same zip.
#[test]
fn compat_and_check_pass_a_release_with_its_own_content() {
    let tmp = Tmp::new("compat-content");
    let s = src("astra.unity");
    let manifest = manifest_for(&s, "1.2.3").replacen(
        "[target]",
        "[[integration.content]]\npath = \"anim/climb.fbx\"\nkind = \"animations\"\ntitle = \"Climb\"\ncredit = \"someone\"\nlicence = \"CC-BY-4.0\"\n\n[target]",
        1,
    );
    let mut entries = vec![
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
        E::File("anim/climb.fbx", content_fixtures::FBX),
    ];
    entries.extend(PAYLOAD.iter().map(|p| E::File(p, b"payload")));
    let zip = zip(&entries);
    let z = put(&tmp, "r", s.asset, &zip);
    let (code, r) = probe(&["compat", "--zip", &z, "--source", s.id, "--tag", "v1.2.3"]);
    assert_eq!(code, 0, "{r:#}");
    assert_eq!(
        r["steps"][2]["stripped"],
        json!(["integration.capabilities", "integration.content"])
    );
    let (code, r) = check(&z, &["--provenance", "first-party"]);
    assert_eq!(code, 0, "{r:#}");
    assert_eq!(r["content"]["parts"][0]["path"], "anim/climb.fbx");
    assert_eq!(r["content"]["parts"][0]["licence_source"], "declared");
}

/// C2 for `${astra_pack_inbox}` (`src/pack_inbox.rs`): launcher 0.3.1 refuses the placeholder as
/// it parses the manifest, so `compat` fails the release at step 1 with the finding a release
/// script reads — `needs-newer-astra{min_astra = 0.2.8, placeholder = astra_pack_inbox}`, never
/// 0.3.1's bare `unknown-placeholder` — and reads it no further; `check` (this launcher, the
/// catalogue's ingest) passes the same zip, its pack declared as the release's own content.
#[test]
fn compat_holds_a_pack_inbox_release_to_astra_0_2_8() {
    let tmp = Tmp::new("compat-pack-inbox");
    let s = src("astra.unity");
    let manifest = manifest_for(&s, "1.2.3").replacen(
        "[target]",
        "[[integration.content]]\npath = \"packs/peak-climb.astrapack\"\nkind = \"animations\"\ntitle = \"Climb\"\ncredit = \"someone\"\nlicence = \"CC-BY-4.0\"\n\n[target]",
        1,
    ) + "\n[[files]]\nfrom = \"packs/**\"\nto = \"${astra_pack_inbox}/\"\n";
    // A pack the content catalogue reads (its `astrachar.json` last) and the companion's inbox
    // too (its `animpack.toml`).
    let pack = content_fixtures::astrapack(
        b"[animpack]\nname = \"Climb\"\nkind = \"vrm\"\nuid = \"astra.unity/climb\"\nversion = \"1.0.0\"\nowner = \"integration:astra.unity\"\ngames = [\"astra.unity\"]\n",
    );
    let mut entries = vec![
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
        E::File("packs/peak-climb.astrapack", &pack),
    ];
    entries.extend(PAYLOAD.iter().map(|p| E::File(p, b"payload")));
    let zip = zip(&entries);
    let z = put(&tmp, "r", s.asset, &zip);
    let (exit, r) = probe(&["compat", "--zip", &z, "--source", s.id, "--tag", "v1.2.3"]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(r["verdict"], "fail");
    let findings = r["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{r:#}");
    assert_eq!(first_failure(&r), (1, "needs-newer-astra".to_owned()));
    assert_eq!(findings[0]["params"]["min_astra"], "0.2.8");
    assert_eq!(findings[0]["params"]["placeholder"], "astra_pack_inbox");
    assert_eq!(findings[0]["params"]["refused"], "unknown-placeholder");
    assert_eq!(
        statuses(&r),
        [
            "fail", "skipped", "skipped", "skipped", "skipped", "skipped"
        ]
    );

    let (code, r) = check(&z, &["--provenance", "first-party"]);
    assert_eq!(code, 0, "{r:#}");
    assert_eq!(
        r["content"]["parts"][0]["path"],
        "packs/peak-climb.astrapack"
    );
}

/// Step 1 asks this launcher too, for every refusal: a release 0.3.1 installs (it reads past
/// every `[integration.*]` table) and this launcher refuses — the reserved
/// `[[integration.uses]]`, a content kind it does not know — fails there with this launcher's
/// refusal (`launcher` its version), or Astra 0.2.7 would install a release the next Astra then
/// refuses to Play (it parses the ledger's manifest again) or reinstall. And a
/// `${astra_pack_inbox}` release this launcher refuses is that refusal, never
/// `needs-newer-astra`'s "Astra 0.2.8 reads it".
#[test]
fn compat_fails_a_release_this_launcher_refuses() {
    let tmp = Tmp::new("compat-refused-by-0.5");
    let s = src("astra.unity");
    let with_table = |table: &str| {
        manifest_for(&s, "1.2.3").replacen("[target]", &format!("{table}\n[target]"), 1)
    };
    for (what, manifest, code, key, reason) in [
        (
            "the reserved [[integration.uses]]",
            with_table("[[integration.uses]]\nid = \"someone.pack\"\nversion_req = \"=1.0.0\"\n"),
            "unknown-key",
            Some("integration.uses"),
            Some("reserved"),
        ),
        (
            "a content kind this launcher does not know",
            with_table(
                "[[integration.content]]\npath = \"anim/climb.fbx\"\nkind = \"command\"\ntitle = \"Climb\"\ncredit = \"someone\"\nlicence = \"CC-BY-4.0\"\n",
            ),
            "manifest-invalid",
            Some("integration.content[0].kind"),
            None,
        ),
    ] {
        let zip = release_zip(&manifest, PAYLOAD);
        assert!(
            launcher_031::GiPackage::open(&zip, &sha(&zip)).is_ok(),
            "{what}: 0.3.1 installs it, or this shows nothing"
        );
        let z = put(&tmp, code, s.asset, &zip);
        let (exit, r) = probe(&["compat", "--zip", &z, "--source", s.id, "--tag", "v1.2.3"]);
        assert_eq!(exit, 1, "{what}: {r:#}");
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{what}: {r:#}");
        let f = &findings[0];
        assert_eq!(
            (f["step"].as_u64(), f["code"].as_str()),
            (Some(1), Some(code)),
            "{what}"
        );
        assert_eq!(f["params"]["key"].as_str(), key, "{what}: {r:#}");
        assert_eq!(f["params"]["reason"].as_str(), reason, "{what}");
        assert_eq!(f["params"]["launcher"], env!("CARGO_PKG_VERSION"), "{what}");
        assert_eq!(
            statuses(&r),
            [
                "fail", "skipped", "skipped", "skipped", "skipped", "skipped"
            ],
            "{what}"
        );
    }

    // A pack-inbox release in a folder below the inbox: both launchers refuse it, and the finding
    // is this one's (`pack-inbox-invalid`), never needs-newer-astra.
    let manifest = manifest_for(&s, "1.2.3")
        + "\n[[files]]\nfrom = \"packs/**\"\nto = \"${astra_pack_inbox}/sub/\"\n";
    let mut entries = vec![
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
        E::File("packs/climb.astrapack", b"PK\x05\x06"),
    ];
    entries.extend(PAYLOAD.iter().map(|p| E::File(p, b"payload")));
    let zip = zip(&entries);
    let z = put(&tmp, "in-a-folder", s.asset, &zip);
    let (exit, r) = probe(&["compat", "--zip", &z, "--source", s.id, "--tag", "v1.2.3"]);
    assert_eq!(exit, 1, "{r:#}");
    let findings = r["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{r:#}");
    assert_eq!(findings[0]["code"], "pack-inbox-invalid", "{r:#}");
    assert_eq!(findings[0]["params"]["reason"], "in-a-folder");
    assert_eq!(findings[0]["params"]["launcher"], env!("CARGO_PKG_VERSION"));
}

/// Every kind of break fails at the step that owns it, and no earlier one.
#[test]
fn compat_fails_each_break_at_its_step() {
    let tmp = Tmp::new("compat-breaks");
    let runner = src("astra.unity");
    let game = src("astra.death-stranding");
    let good_runner = manifest_for(&runner, "1.2.3");
    let good_game = manifest_for(&game, "1.2.3");
    // (what, source, manifest, extra arguments, the step, its code)
    type Case<'a> = (&'a str, &'a GiSource, String, Vec<&'a str>, u64, &'a str);
    let cases: Vec<Case> = vec![
        (
            "an unknown top-level key",
            &runner,
            good_runner.replacen("schema = 1\n", "schema = 1\nfuture = true\n", 1),
            vec![],
            1,
            "unknown-key",
        ),
        (
            "a [target] engine that is no built-in",
            &runner,
            good_runner.replace("engine = \"unity-mono\"", "engine = \"source2\""),
            vec![],
            1,
            "manifest-invalid",
        ),
        (
            "a version the tag does not carry",
            &game,
            good_game.replace("version = \"1.2.3\"", "version = \"1.2.4\""),
            vec![],
            2,
            "release-mismatch",
        ),
        (
            "the wrong asset",
            &game,
            good_game.clone(),
            vec!["--asset", "astra-gi-ds2.zip"],
            2,
            "asset-mismatch",
        ),
        (
            "a runner that names an appid",
            &runner,
            good_runner.replace("exe = \"*\"", "exe = \"*\"\nsteam_appid = 4513480"),
            vec![],
            2,
            "release-mismatch",
        ),
        (
            "another source's id",
            &game,
            good_game.replace(
                "id = \"astra.death-stranding\"",
                "id = \"astra.death-stranding-2\"",
            ),
            vec![],
            2,
            "release-mismatch",
        ),
        (
            "a manifest that copies itself into the profile (its [integration.*] tables then are not inert)",
            &runner,
            good_runner.replace(
                "[[game_files]]",
                "[[files]]\nfrom = \"astra-gi.toml\"\nto = \"${profile}/\"\n\n[[game_files]]",
            ),
            vec![],
            3,
            "integration-tables-not-inert",
        ),
        (
            "a new ${var}",
            &runner,
            good_runner.replace(
                "args = [\"-AstraConfig\", \"${profile}/astra.ini\"]",
                "args = [\"-AstraConfig\", \"${profile}/astra.ini\", \"-User\", \"${steam_user}\"]",
            ),
            vec![],
            4,
            "missing-variable",
        ),
    ];
    for (what, s, manifest, extra, step, code) in cases {
        assert!(
            manifest != manifest_for(s, "1.2.3") || !extra.is_empty(),
            "{what}: the case changes nothing"
        );
        let zip = release_zip(&manifest, PAYLOAD);
        let z = put(&tmp, &format!("{step}-{code}-{}", s.id), s.asset, &zip);
        let mut args = vec!["compat", "--zip", &z, "--source", s.id, "--tag", "v1.2.3"];
        args.extend(extra);
        let (exit, r) = probe(&args);
        assert_eq!(exit, 1, "{what}: {r:#}");
        assert_eq!(r["verdict"], "fail", "{what}");
        assert_eq!(first_failure(&r), (step, code.to_owned()), "{what}: {r:#}");
    }
}

/// Step 1 asks this launcher first: a zip it refuses as two archives (`zip-inconsistent`, as
/// `check` does) fails compat there, and launcher 0.3.1, whose zip 8 cannot tell, never reads it.
/// AstraPlugins' three vectors (`tests/zip_consistency.rs` holds them to its digests).
#[test]
fn compat_refuses_a_zip_that_reads_as_two_archives_before_031_reads_it() {
    let s = src("astra.unity");
    for (name, reason) in [
        ("slack.zip", "deflate-end"),
        ("upath.zip", "unicode-path"),
        ("stored-dd.zip", "descriptor-missing"),
    ] {
        let z = spec_goldens::root()
            .join("testdata/zip-cloaking")
            .join(name);
        let (exit, r) = probe(&[
            "compat",
            "--zip",
            z.to_str().unwrap(),
            "--source",
            s.id,
            "--tag",
            "v1.2.3",
            "--asset",
            s.asset,
        ]);
        assert_eq!(exit, 1, "{name}: {r:#}");
        assert_eq!(r["verdict"], "fail", "{name}");
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{name}: {r:#}");
        assert_eq!(findings[0]["step"], 1, "{name}");
        assert_eq!(findings[0]["code"], "zip-inconsistent", "{name}: {r:#}");
        assert_eq!(findings[0]["params"]["reason"], reason, "{name}");
        for step in 2..=6 {
            let st = &r["steps"][step - 1];
            assert_eq!(st["status"], "skipped", "{name} step {step}: {r:#}");
            assert_eq!(
                st["why"], "the zip reads as two archives (step 1): nothing in it is read",
                "{name} step {step}"
            );
        }
    }
}

/// Step 5: a file gone from under a `**` fails unless the notes name it; one added likewise.
/// Step 6: a changed license is the notice that 0.2.7 will ask again, never a finding.
#[test]
fn compat_holds_a_release_to_the_previous_one() {
    let tmp = Tmp::new("compat-previous");
    let s = src("astra.unreal");
    let previous_zip = release_zip(
        &manifest_for(&s, "1.2.2"),
        &["payload/a.txt", "payload/sub/b.txt", "payload/old.txt"],
    );
    let previous = put(&tmp, "prev", s.asset, &previous_zip);
    let zip = release_zip(&manifest_for(&s, "1.2.3"), PAYLOAD);
    let z = put(&tmp, "now", s.asset, &zip);
    let compat = |extra: &[&str]| {
        let mut args = vec![
            "compat",
            "--zip",
            &z,
            "--source",
            s.id,
            "--tag",
            "v1.2.3",
            "--previous",
            &previous,
        ];
        args.extend(extra);
        probe(&args)
    };

    // Removed, the notes silent.
    let (exit, r) = compat(&[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(first_failure(&r), (5, "swept-files-changed".to_owned()));
    assert_eq!(r["findings"][0]["field"], "payload/old.txt");
    assert_eq!(r["findings"][0]["params"]["change"], "removed");
    assert_eq!(statuses(&r)[5], "pass", "{r:#}");

    // Removed, and the notes say so.
    let notes = tmp.join("notes.md");
    fs::write(&notes, "Dropped `payload/old.txt`, nobody read it.\n").unwrap();
    let (exit, r) = compat(&["--notes", notes.to_str().unwrap()]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(
        statuses(&r),
        ["pass", "pass", "pass", "pass", "pass", "pass"]
    );
    assert_eq!(r["steps"][4]["named_in_notes"], 1);

    // A name inside a longer one is not naming it.
    fs::write(&notes, "Dropped payload/old.txt.bak only.\n").unwrap();
    let (exit, _) = compat(&["--notes", notes.to_str().unwrap()]);
    assert_eq!(exit, 1);

    // Added, the notes silent.
    let added = release_zip(
        &manifest_for(&s, "1.2.3"),
        &[
            "payload/a.txt",
            "payload/sub/b.txt",
            "payload/old.txt",
            "payload/new.txt",
        ],
    );
    let za = put(&tmp, "added", s.asset, &added);
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        &za,
        "--source",
        s.id,
        "--tag",
        "v1.2.3",
        "--previous",
        &previous,
    ]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(r["findings"][0]["field"], "payload/new.txt");
    assert_eq!(r["findings"][0]["params"]["change"], "added");

    // The same swept files and another license: 0.2.7 shows its sheet again; a notice.
    let relicensed = release_zip(
        &manifest_for(&s, "1.2.3").replace("license = \"MIT\"", "license = \"MIT AND CC0-1.0\""),
        &["payload/a.txt", "payload/sub/b.txt", "payload/old.txt"],
    );
    let zr = put(&tmp, "relicensed", s.asset, &relicensed);
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        &zr,
        "--source",
        s.id,
        "--tag",
        "v1.2.3",
        "--previous",
        &previous,
    ]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(statuses(&r)[5], "notice");
    assert_eq!(r["notices"][0]["code"], "consent-sheet-expected");
    assert_eq!(r["notices"][0]["params"]["changed"], "integration.license");

    // Only the version moved: nothing to ask.
    let same = release_zip(
        &manifest_for(&s, "1.2.3"),
        &["payload/a.txt", "payload/sub/b.txt", "payload/old.txt"],
    );
    let zs = put(&tmp, "same", s.asset, &same);
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        &zs,
        "--source",
        s.id,
        "--tag",
        "v1.2.3",
        "--previous",
        &previous,
    ]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(
        statuses(&r),
        ["pass", "pass", "pass", "pass", "pass", "pass"]
    );
    assert_eq!(r["notices"], json!([]));
}

/// A tag the source does not follow, and a prerelease's tag only with `--prerelease`.
#[test]
fn compat_holds_the_tag_to_the_sources_rule() {
    let tmp = Tmp::new("compat-tags");
    let s = src("astra.peak");
    let zip = release_zip(&manifest_for(&s, "1.2.3-rc.1"), PAYLOAD);
    let z = put(&tmp, "rc", s.asset, &zip);
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        &z,
        "--source",
        s.id,
        "--tag",
        "v1.2.3-rc.1",
    ]);
    assert_eq!(exit, 1);
    assert_eq!(first_failure(&r), (2, "tag-not-followed".to_owned()));
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        &z,
        "--source",
        s.id,
        "--tag",
        "v1.2.3-rc.1",
        "--prerelease",
    ]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["notices"][0]["code"], "prerelease");
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        &z,
        "--source",
        s.id,
        "--tag",
        "release-1.2.3",
    ]);
    assert_eq!(exit, 1);
    assert_eq!(first_failure(&r), (2, "tag-not-followed".to_owned()));
}

/// Nothing concluded: exit 2 with the error said.
#[test]
fn compat_and_its_siblings_exit_2_on_a_wrong_command_line() {
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        "x.zip",
        "--source",
        "astra.nobody",
        "--tag",
        "v1",
    ]);
    assert_eq!(exit, 2);
    assert_eq!(r["error"]["code"], "usage");
    let (exit, r) = probe(&[
        "compat",
        "--zip",
        "/nonexistent/astra-gi.zip",
        "--source",
        "astra.peak",
        "--tag",
        "v1.0.0",
    ]);
    assert_eq!(exit, 2);
    assert_eq!(r["error"]["code"], "io");
    let (exit, r) = probe(&["validate-descriptor"]);
    assert_eq!(exit, 2);
    assert_eq!(r["error"]["code"], "usage");
    let (exit, _) = probe(&["frobnicate"]);
    assert_eq!(exit, 2);
}

/// `gi-probe pick` is the replica over a listing, and names the pin.
#[test]
fn pick_takes_what_a_027_release_build_takes() {
    let tmp = Tmp::new("pick");
    let s = src("astra.unreal");
    let mut pre = release(&s, "v0.2.11", 0x11);
    pre.prerelease = true;
    let listing = json!([
        { "tag_name": pre.tag_name, "prerelease": true, "assets": [{
            "name": s.asset, "browser_download_url": canonical_url(s.repo, "v0.2.11", s.asset),
            "digest": format!("sha256:{}", "11".repeat(32)) }] },
        { "tag_name": "v0.2.1", "prerelease": false, "assets": [{
            "name": s.asset, "browser_download_url": canonical_url(s.repo, "v0.2.1", s.asset),
            "digest": format!("sha256:{}", "21".repeat(32)) }] },
    ]);
    let file = tmp.join("listing.json");
    fs::write(&file, listing.to_string()).unwrap();
    let (exit, r) = probe(&[
        "pick",
        "--source",
        s.id,
        "--listing",
        file.to_str().unwrap(),
    ]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["release"]["tag"], "v0.2.1");
    assert_eq!(r["fallback"]["tag"], s.fallback.unwrap().tag);
    let (_, r) = probe(&[
        "pick",
        "--source",
        s.id,
        "--listing",
        file.to_str().unwrap(),
        "--prerelease",
    ]);
    assert_eq!(r["release"]["tag"], "v0.2.11");
    fs::write(&file, "[]").unwrap();
    let (exit, r) = probe(&[
        "pick",
        "--source",
        s.id,
        "--listing",
        file.to_str().unwrap(),
    ]);
    assert_eq!(exit, 1);
    assert_eq!(r["findings"][0]["code"], "release-not-found");
    assert_eq!(r["findings"][0]["params"]["reason"], "no-release");
}

// ---------------------------------------------------------------------------------------------
// `gi-probe validate-descriptor`.
// ---------------------------------------------------------------------------------------------

/// Plan B §1's Source 2 descriptor (the community foundation `community.source2`).
const SOURCE2: &str = r#"schema = "astra.engine/1"
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

fn validate(tmp: &Tmp, name: &str, text: &str, extra: &[&str]) -> (i32, Value) {
    let p = tmp.join(name);
    fs::write(&p, text).unwrap();
    let mut args = vec!["validate-descriptor", p.to_str().unwrap()];
    args.extend(extra);
    probe(&args)
}

/// Plan B §1's Source 2 descriptor passes, as itself and as a foundation's
/// `[integration.engine]`, with one digest, and claims no negative folder.
#[test]
fn validate_descriptor_passes_the_source2_descriptor() {
    let tmp = Tmp::new("validate-source2");
    let digest = astra_game_launcher::Descriptor::from_toml(SOURCE2)
        .unwrap()
        .digest()
        .to_owned();
    let (exit, r) = validate(&tmp, "source2.toml", SOURCE2, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["descriptor"]["sha256"], digest);
    assert_eq!(r["descriptor"]["codes"], json!({ "source2": "Source 2" }));
    assert_eq!(r["input"]["kind"], "descriptor-toml");
    assert_eq!(r["spec"]["sha"], spec_goldens::pin().as_str());
    let folders = r["negative_folders"].as_array().unwrap();
    assert_eq!(folders.len(), 4);
    assert!(folders.iter().all(|f| f["claimed"] == false));
    // Asked where it looks, `<project>/bin/win64`, as well as where the spec draws them.
    let dlls = folders
        .iter()
        .find(|f| f["folder"] == "system-dlls")
        .unwrap();
    let programs: Vec<&str> = dlls["places"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["program"].as_str().unwrap())
        .collect();
    assert_eq!(programs, ["Game.exe", "Game/bin/win64/Game.exe"], "{r:#}");

    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"community.source2\"\nname = \"Source 2\"\nversion = \"1.0.0\"\n\n{}\n[target]\nengine = \"source2\"\nexe = \"*\"\nplatforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n",
        SOURCE2
            .replace(
                "schema = \"astra.engine/1\"\nengine =",
                "[integration.engine]\nschema = \"astra.engine/1\"\nengine ="
            )
            .replace("[[rule]]", "[[integration.engine.rule]]")
    );
    let (exit, r) = validate(&tmp, "astra-gi.toml", &manifest, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["input"]["kind"], "manifest-integration-engine");
    assert_eq!(r["descriptor"]["sha256"], digest);

    // JSON, from the catalogue's spelling: the same digest.
    let canonical = astra_game_launcher::Descriptor::from_toml(SOURCE2)
        .unwrap()
        .canonical_json()
        .to_owned();
    let (exit, r) = validate(&tmp, "source2.json", &canonical, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["input"]["kind"], "descriptor-json");
    assert_eq!(r["descriptor"]["sha256"], digest);
}

/// A valid `mods` for [`SOURCE2`] (the compat golden's loader, inline): a loader of its own,
/// side by side.
const VALID_MODS: &str = "mods = { loader = [ { id = \"s2-loader\", name = \"S2 Loader\", codes = [\"source2\"], source = [\"game\", \"picked\"], require = [ { stat = \"{source}/s2loader/s2loader.dll\", kind = \"file\" } ], root = \"{source}/s2loader/mods\", enabled = [ { format = \"dir-per-mod\" } ], coexist = [ { strategy = \"side-by-side\" } ] } ] }";

/// A `mods` with three things wrong inside it: two members §7.2 does not name, and no `loader`.
const BAD_MODS: &str = "mods = { root = \"{game}/Mods\", enabled = { format = \"dir-per-mod\" } }";

/// `text` (a descriptor whose root table has an `engine = {` line) with `mods` before that line.
fn with_mods_line(text: &str, mods: &str) -> String {
    assert!(text.contains("\nengine = {"), "{text}");
    text.replacen("\nengine = {", &format!("\n{mods}\nengine = {{"), 1)
}

/// A report's `mods-invalid` findings, as `(step, field, reason)`.
fn mods_invalid(r: &Value) -> Vec<(u64, String, String)> {
    r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == "mods-invalid")
        .map(|f| {
            assert_eq!(f["field"], f["params"]["field"], "{f:#}");
            (
                f["step"].as_u64().unwrap(),
                f["field"].as_str().unwrap().to_owned(),
                f["params"]["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// A report's other findings.
fn not_mods_invalid(r: &Value) -> Vec<Value> {
    r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] != "mods-invalid")
        .cloned()
        .collect()
}

/// What the library refuses inside `text`'s `mods`, as `(step, field, reason)` at `step`.
fn library_mods_invalid(text: &str, step: u64) -> Vec<(u64, String, String)> {
    astra_game_launcher::Descriptor::from_toml(text)
        .unwrap()
        .mods_refusals()
        .iter()
        .map(|e| {
            assert_eq!(e.code, "mods-invalid");
            (
                step,
                e.param("field").unwrap().to_owned(),
                e.param("reason").unwrap().to_owned(),
            )
        })
        .collect()
}

/// `mods` (spec §7): `validate-descriptor` judges it at step 1. A valid one passes with no
/// finding and no notice, its loaders named in `descriptor.mods_loaders`; one with anything wrong
/// inside is refused for exactly the library's `mods-invalid{field, reason}`s, at step 1, and
/// everything else is judged as without it — the negative folders, and the nested griefer's own
/// findings. In all three spellings. `check` refuses the same at step 2 (`Manifest::lint`, as
/// ingest does) and judges step 4 as without it. A `mods` that is no table is no descriptor.
#[test]
fn validate_descriptor_judges_mods_and_the_rest_alike() {
    let tmp = Tmp::new("validate-mods");
    let (exit, plain) = validate(&tmp, "plain.toml", SOURCE2, &[]);
    assert_eq!(exit, 0, "{plain:#}");
    assert_eq!(plain["notices"], json!([]));
    assert_eq!(plain["descriptor"]["mods_loaders"], Value::Null);

    let manifest = |engine: &str| {
        format!(
            "schema = 1\n\n[integration]\nid = \"community.source2\"\nname = \"Source 2\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n\n{}\n[target]\nengine = \"source2\"\nexe = \"*\"\nplatforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"a.txt\"\nto = \"${{profile}}/a.txt\"\n",
            engine
                .replace(
                    "schema = \"astra.engine/1\"\n",
                    "[integration.engine]\nschema = \"astra.engine/1\"\n"
                )
                .replace("[[rule]]", "[[integration.engine.rule]]")
        )
    };
    for (mods, loaders) in [(VALID_MODS, json!(["s2-loader"])), (BAD_MODS, json!([]))] {
        let text = with_mods_line(SOURCE2, mods);
        let d = astra_game_launcher::Descriptor::from_toml(&text).unwrap();
        let want = library_mods_invalid(&text, 1);
        assert_eq!(want.is_empty(), mods == VALID_MODS, "{want:?}");
        let canonical = d.canonical_json().to_owned();
        for (name, input, kind) in [
            ("mods.toml", text.clone(), "descriptor-toml"),
            ("mods.json", canonical, "descriptor-json"),
            (
                "astra-gi.toml",
                manifest(&text),
                "manifest-integration-engine",
            ),
        ] {
            let (exit, r) = validate(&tmp, name, &input, &[]);
            assert_eq!(r["input"]["kind"], kind);
            assert_eq!(exit, i32::from(!want.is_empty()), "{name}: {r:#}");
            assert_eq!(
                r["verdict"],
                if want.is_empty() { "pass" } else { "fail" },
                "{name}"
            );
            assert_eq!(mods_invalid(&r), want, "{name}: {r:#}");
            assert_eq!(not_mods_invalid(&r), Vec::<Value>::new(), "{name}: {r:#}");
            assert_eq!(r["notices"], json!([]), "{name}");
            assert_eq!(r["descriptor"]["sha256"], d.digest(), "{name}");
            assert_ne!(r["descriptor"]["sha256"], plain["descriptor"]["sha256"]);
            assert_eq!(r["descriptor"]["mods_loaders"], loaders, "{name}");
            assert_eq!(r["negative_folders"], plain["negative_folders"], "{name}");
        }
    }

    // The nested griefer is refused for its own findings as without `mods`, and for `mods`'.
    let (exit, griefer) = validate(&tmp, "nested.toml", NESTED_GRIEFER, &[]);
    assert_eq!(exit, 1);
    for mods in [VALID_MODS, BAD_MODS] {
        let text = with_mods_line(NESTED_GRIEFER, mods);
        let (exit, r) = validate(&tmp, "nested-mods.toml", &text, &[]);
        assert_eq!(exit, 1, "{r:#}");
        assert_eq!(not_mods_invalid(&r), not_mods_invalid(&griefer), "{r:#}");
        assert_eq!(mods_invalid(&r), library_mods_invalid(&text, 1), "{r:#}");
        assert_eq!(r["negative_folders"], griefer["negative_folders"]);
        assert_eq!(r["notices"], json!([]));
    }

    // Not a table: no descriptor (step 1), and nothing else to say about `mods`.
    let (exit, r) = validate(
        &tmp,
        "mods-list.toml",
        &with_mods_line(SOURCE2, "mods = [\"Mods\"]"),
        &[],
    );
    assert_eq!(exit, 1);
    let f = r["findings"].as_array().unwrap();
    assert_eq!(f.len(), 1, "{r:#}");
    assert_eq!(
        (&f[0]["code"], &f[0]["step"], &f[0]["field"]),
        (&json!("descriptor-invalid"), &json!(1), &json!("mods"))
    );
    assert_eq!(r["notices"], json!([]));

    // `check`, of the foundation as a bare manifest: step 2 refuses what is wrong inside `mods`,
    // as ingest does, and nothing else changes.
    let plain_file = tmp.join("check-plain.toml");
    fs::write(&plain_file, manifest(SOURCE2)).unwrap();
    let (exit_plain, plain) = check(plain_file.to_str().unwrap(), &[]);
    assert_eq!(exit_plain, 0, "{plain:#}");
    assert_eq!(plain["notices"], json!([]), "{plain:#}");
    for mods in [VALID_MODS, BAD_MODS] {
        let text = with_mods_line(SOURCE2, mods);
        let file = tmp.join("check-mods.toml");
        fs::write(&file, manifest(&text)).unwrap();
        let (exit, r) = check(file.to_str().unwrap(), &[]);
        let want = library_mods_invalid(&text, 2);
        assert_eq!(exit, i32::from(!want.is_empty()), "{r:#}");
        assert_eq!(mods_invalid(&r), want, "{r:#}");
        assert_eq!(not_mods_invalid(&r), not_mods_invalid(&plain), "{r:#}");
        assert_eq!(r["notices"], plain["notices"], "{r:#}");
        let mut s = statuses(&r);
        if !want.is_empty() {
            assert_eq!(s[1], "fail", "{r:#}");
            s[1] = "pass".to_owned();
        }
        assert_eq!(s, statuses(&plain), "{r:#}");
    }
}

/// The specification's `mods` goldens (`vectors.json` `descriptors.mods`, engine-descriptor-v1.md
/// §7.11) through `validate-descriptor`, which refuses what ingest refuses (§7.9): each compat
/// and hostile golden is refused at step 1 for exactly its `reasons` — every such finding
/// `mods-invalid`, its `field` a member of `mods` — and passes when it has none, naming its
/// loaders; steps 2 to 4 judge each exactly as the same descriptor without `mods` (no other
/// finding, the same negative folders). `not-a-table` is no descriptor (step 1, field `mods`).
/// The built-in fragments on the launcher's own built-ins have no `mods-invalid`, and name the
/// loaders `vectors.json` names. Every one of them parses at this pin (`mods-deep-nesting` nests
/// 39 deep since AstraPlugins #133).
#[test]
fn validate_descriptor_refuses_each_mods_golden_for_exactly_its_reasons() {
    let tmp = Tmp::new("validate-mods-goldens");
    let v = spec_goldens::vectors();
    let mut out = Vec::new();
    let (mut judged, mut skipped, mut passed) = (0, 0, 0);
    for key in ["compat", "hostile"] {
        for r in v["descriptors"]["mods"][key].as_array().unwrap() {
            let golden = format!("testdata/games/{}", r["file"].as_str().unwrap());
            let path = spec_goldens::root().join(&golden);
            let (exit, report) = probe(&["validate-descriptor", path.to_str().unwrap()]);
            assert_eq!(report["spec"]["sha"], spec_goldens::pin().as_str());
            let mut want: Vec<String> = r["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_owned())
                .collect();
            want.sort();
            if r["reader_ignoring_mods"] == "refuse" {
                assert_eq!(want, ["descriptor-invalid"], "{golden}");
                assert_eq!(exit, 1, "{golden}: {report:#}");
                let f = report["findings"].as_array().unwrap();
                assert_eq!(f.len(), 1, "{golden}: {report:#}");
                assert_eq!(
                    (&f[0]["code"], &f[0]["step"], &f[0]["field"]),
                    (&json!("descriptor-invalid"), &json!(1), &json!("mods")),
                    "{golden}"
                );
                skipped += 1;
                continue;
            }
            let found = mods_invalid(&report);
            let mut got: Vec<String> = found.iter().map(|(_, _, r)| r.clone()).collect();
            got.sort();
            got.dedup();
            if got != want {
                out.push(format!(
                    "{golden}: refused for {got:?}, the golden names {want:?}"
                ));
            }
            assert!(
                found
                    .iter()
                    .all(|(step, field, _)| *step == 1 && field.starts_with("mods")),
                "{golden}: {found:?}"
            );
            assert_eq!(
                report["verdict"] == "fail",
                r["verdict"] == "refuse",
                "{golden}: {report:#}"
            );
            assert_eq!(exit, i32::from(r["verdict"] == "refuse"), "{golden}");
            let d = astra_game_launcher::Descriptor::from_toml(&fs::read_to_string(&path).unwrap())
                .unwrap();
            assert_eq!(report["descriptor"]["sha256"], d.digest(), "{golden}");
            let loaders = d
                .mods()
                .map(|m| json!(m.loaders().iter().map(|l| l.id()).collect::<Vec<_>>()))
                .unwrap_or(json!([]));
            assert_eq!(report["descriptor"]["mods_loaders"], loaders, "{golden}");
            let mut plain: Value = serde_json::from_str(d.canonical_json()).unwrap();
            plain.as_object_mut().unwrap().remove("mods").unwrap();
            let (exit_plain, plain) = validate(&tmp, "plain.json", &plain.to_string(), &[]);
            assert_eq!(exit_plain, 0, "{golden}: {plain:#}");
            assert_eq!(not_mods_invalid(&report), Vec::<Value>::new(), "{golden}");
            assert_eq!(report["notices"], json!([]), "{golden}");
            assert_eq!(
                report["negative_folders"], plain["negative_folders"],
                "{golden}"
            );
            passed += usize::from(found.is_empty());
            judged += 1;
        }
    }
    assert!(out.is_empty(), "\n{}", out.join("\n"));
    assert_eq!(skipped, 1, "not-a-table");
    assert_eq!(passed, 1, "source2-with-mods");
    assert!(judged >= 31, "{judged} mods goldens judged");

    for r in v["descriptors"]["mods"]["builtin"].as_array().unwrap() {
        let name = r["name"].as_str().unwrap();
        let fragment = spec_goldens::golden_text(r["file"].as_str().unwrap());
        let at = fragment.find("[[mods.loader]]").unwrap();
        let ours = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("src/engines/builtin/{name}.toml")),
        )
        .unwrap();
        let (_, report) = validate(
            &tmp,
            &format!("{name}.toml"),
            &format!("{ours}\n{}", &fragment[at..]),
            &["--first-party"],
        );
        assert_eq!(mods_invalid(&report), [], "{name}: {report:#}");
        assert_eq!(report["descriptor"]["mods_loaders"], r["loaders"], "{name}");
    }
}

fn reason_of(r: &Value) -> Vec<(String, String)> {
    r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["code"].as_str().unwrap().to_owned(),
                f["params"]["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

/// A rule with no literal anchor, and one anchored on `steam_api64.dll`, are refused.
#[test]
fn validate_descriptor_refuses_the_anchorless_and_the_runtime_anchored() {
    let tmp = Tmp::new("validate-anchor");
    let anchorless = SOURCE2.replace(
        "require = [ { listed = \"engine2.dll\", kind = \"file\" },\n            ",
        "require = [ ",
    );
    assert_ne!(anchorless, SOURCE2);
    let (exit, r) = validate(&tmp, "anchorless.toml", &anchorless, &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(
        reason_of(&r),
        [("descriptor-too-broad".to_owned(), "no-anchor".to_owned())]
    );
    let steam_api = SOURCE2.replace("engine2.dll", "steam_api64.dll");
    let (exit, r) = validate(&tmp, "steam-api.toml", &steam_api, &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(
        reason_of(&r),
        [(
            "descriptor-too-broad".to_owned(),
            "ubiquitous-runtime".to_owned()
        )]
    );
    // An unknown key: not a descriptor at all.
    let (exit, r) = validate(
        &tmp,
        "unknown-key.toml",
        &SOURCE2.replace(
            "confidence = \"high\"",
            "confidence = \"high\"\nmatch_anything = true",
        ),
        &[],
    );
    assert_eq!(exit, 1);
    assert_eq!(r["findings"][0]["code"], "descriptor-invalid");
    assert_eq!(r["findings"][0]["step"], 1);
}

/// A descriptor that names a folder holding one bare program is too broad, even one Astra
/// publishes itself (first-party may go without an anchor; no descriptor may claim that).
#[test]
fn validate_descriptor_refuses_one_matching_a_bare_exe_folder() {
    let tmp = Tmp::new("validate-broad");
    let broad = r#"schema = "astra.engine/1"
engine = { codes = { probe = "Probe Engine" } }

[[rule]]
tier = "weak"
programs = { ext = ["exe"], at = "", first = 8 }
require = [ { stat = "{program}", kind = "file" } ]
code = "probe"
confidence = "low"
binary = "windows"
evidence = "{program}"
"#;
    let (exit, r) = validate(&tmp, "broad.toml", broad, &["--first-party"]);
    assert_eq!(exit, 1, "{r:#}");
    let reasons = reason_of(&r);
    assert!(
        reasons.contains(&(
            "descriptor-too-broad".to_owned(),
            "negative:exe-only".to_owned()
        )),
        "{r:#}"
    );
    let empty = r["negative_folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["folder"] == "empty")
        .unwrap()
        .clone();
    assert_eq!(empty["claimed"], false);
    // Without --first-party it never gets that far: no anchor.
    let (exit, r) = validate(&tmp, "broad.toml", broad, &[]);
    assert_eq!(exit, 1);
    assert_eq!(r["findings"][0]["step"], 2);
}

/// `tests/descriptors/hostile/negative-folders-nested.toml`.
const NESTED_GRIEFER: &str = include_str!("descriptors/hostile/negative-folders-nested.toml");

/// `(code, reason, program)` of every finding of a report at `step`.
fn too_broad_at(r: &Value, step: u64) -> Vec<(String, String, String)> {
    r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["step"] == step)
        .map(|f| {
            let s = |k: &str| f["params"][k].as_str().unwrap_or_default().to_owned();
            (
                f["code"].as_str().unwrap().to_owned(),
                s("reason"),
                s("program"),
            )
        })
        .collect()
}

/// The engine `descriptor`, alone as a community foundation's (unchecked), names for a game it
/// knows nothing of: `winhttp.dll` beside `Some Game/bin/win64/SomeGame.exe`.
fn engine_of_a_winhttp_game(tmp: &Tmp, descriptor: &str) -> String {
    use astra_game_launcher::{Descriptor, EngineSet, Moderation, Origin, detect_with_engines};
    let game = tmp.join("winhttp-game");
    let win64 = game.join("Some Game/bin/win64");
    fs::create_dir_all(&win64).unwrap();
    let mut pe = vec![0u8; 0x80];
    pe[..2].copy_from_slice(b"MZ");
    fs::write(win64.join("SomeGame.exe"), &pe).unwrap();
    fs::write(win64.join("winhttp.dll"), b"MZ").unwrap();
    let origin = Origin {
        moderation: Moderation::Unchecked,
        publisher: "someone".to_owned(),
        first_party: false,
    };
    let d = Descriptor::from_toml(descriptor).unwrap();
    let set = EngineSet::build([(origin, Ok(d))]);
    detect_with_engines(&game, None, &set)
        .unwrap()
        .engine
        .as_str()
        .to_owned()
}

/// The hostile fixture names real games: alone in a set, as the unchecked descriptor a
/// community foundation's is, it claims a folder holding `winhttp.dll` beside a program in
/// `<project>/bin/win64` — a game it knows nothing of. `validate-descriptor` (step 3) and
/// `gi-probe check` (step 4) refuse it, at the place it looks; laid only as the spec draws the
/// folder (the flat row), it is not caught, which is all launcher 0.5.0 L3 asked.
#[test]
fn the_nested_griefer_is_refused_by_validate_descriptor_and_check() {
    let tmp = Tmp::new("validate-nested");
    let d = astra_game_launcher::Descriptor::from_toml(NESTED_GRIEFER).unwrap();
    d.check_pass_b(false).unwrap();
    assert_eq!(engine_of_a_winhttp_game(&tmp, NESTED_GRIEFER), "grief");

    let (exit, r) = validate(&tmp, "nested.toml", NESTED_GRIEFER, &[]);
    assert_eq!(exit, 1, "{r:#}");
    let at = "Game/bin/win64/Game.exe".to_owned();
    let want = vec![(
        "descriptor-too-broad".to_owned(),
        "negative:system-dlls".to_owned(),
        at.clone(),
    )];
    assert_eq!(too_broad_at(&r, 3), want, "{r:#}");
    assert_eq!(r["findings"].as_array().unwrap().len(), 1, "{r:#}");
    let dlls = r["negative_folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["folder"] == "system-dlls")
        .unwrap()
        .clone();
    assert_eq!(dlls["claimed"], true);
    assert_eq!(dlls["engine"], "grief");
    assert_eq!(dlls["places"][0]["program"], "Game.exe");
    assert_eq!(dlls["places"][0]["claimed"], false, "the spec's drawing");
    assert_eq!(dlls["places"][1]["program"], at.as_str());
    assert_eq!(dlls["places"][1]["claimed"], true);

    // A community foundation carrying it as [integration.engine]: `gi-probe check` step 4.
    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"someone.grief\"\nname = \"Grief\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n\n{}\n[target]\nengine = \"grief\"\nexe = \"*\"\nplatforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n\n[[files]]\nfrom = \"a.txt\"\nto = \"${{profile}}/a.txt\"\n",
        NESTED_GRIEFER
            .replace(
                "schema = \"astra.engine/1\"\nengine =",
                "[integration.engine]\nschema = \"astra.engine/1\"\nengine ="
            )
            .replace("[[rule]]", "[[integration.engine.rule]]")
    );
    assert!(manifest.contains("[integration.engine]\n"), "{manifest}");
    let z = zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("a.txt", b"a"),
    ]);
    let z = put(&tmp, "r", "astra-gi.zip", &z);
    let (exit, r) = check(&z, &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(too_broad_at(&r, 4), want, "{r:#}");
    assert_eq!(r["findings"].as_array().unwrap().len(), 1, "{r:#}");
    assert_eq!(statuses(&r)[3], "fail", "{r:#}");
}

/// A rule is held to the negative folders wherever it looks for a program and whatever program
/// it selects: each of these names games it knows nothing of, and each passed when the folders
/// were laid only as the spec draws them. The beyond-the-walk one is caught with Steam's hint
/// alone; the ones that skip the probe's first name are laid under the next.
#[test]
fn the_negative_folders_are_laid_wherever_a_rule_looks() {
    let tmp = Tmp::new("validate-places");
    let rule = |programs: &str, require: &str, extra: &str| {
        format!(
            "schema = \"astra.engine/1\"\nengine = {{ codes = {{ grief = \"Grief Engine\" }} }}\n{extra}\n[[rule]]\ntier = \"strong\"\nprograms = {programs}\nrequire = [ {require} ]\ncode = \"grief\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{{program}}\"\n"
        )
    };
    let winhttp = "{ listed = \"winhttp.dll\", kind = \"file\" }";
    /// What a case is: its descriptor, `validate-descriptor`'s flags, the folder it claims first
    /// and where, and whether only Steam's hint reaches that place.
    struct Case {
        what: &'static str,
        text: String,
        flags: &'static [&'static str],
        reason: &'static str,
        at: &'static str,
        hint_only: bool,
    }
    let cases = [
        Case {
            what: "any program in <project>/bin/win64 (first-party)",
            text: rule(
                "{ ext = [\"exe\"], at = \"{project}/bin/win64\", first = 8 }",
                "{ stat = \"{program}\", kind = \"file\" }",
                "",
            ),
            flags: &["--first-party"],
            reason: "negative:exe-only",
            at: "Game/bin/win64/Game.exe",
            hint_only: false,
        },
        Case {
            what: "below the walk's depth: Steam's hint alone reaches it",
            text: rule(
                "{ ext = [\"exe\"], at = \"{project}/a/b/c\", first = 8 }",
                winhttp,
                "",
            ),
            flags: &[],
            reason: "negative:system-dlls",
            at: "Game/a/b/c/Game.exe",
            hint_only: true,
        },
        Case {
            what: "a suffix",
            text: rule(
                "{ suffix = \"-retail.exe\", at = \"{project}/bin\", first = 8 }",
                winhttp,
                "",
            ),
            flags: &[],
            reason: "negative:system-dlls",
            at: "Game/bin/Game-retail.exe",
            hint_only: false,
        },
        Case {
            what: "an ELF program",
            text: rule("{ ext = [\"x86_64\"], at = \"\", first = 8 }", winhttp, ""),
            flags: &[],
            reason: "negative:system-dlls",
            at: "Game.x86_64",
            hint_only: false,
        },
        Case {
            what: "an extension-less program, in a fixed folder",
            text: rule("{ ext = [\"\"], at = \"bin\", first = 8 }", winhttp, ""),
            flags: &[],
            reason: "negative:system-dlls",
            at: "bin/Game",
            hint_only: false,
        },
        Case {
            what: "a wrapper the probe's first name holds",
            text: rule(
                "{ ext = [\"exe\"], at = \"{project}/bin/win64\", first = 8 }",
                winhttp,
                "wrappers = [\"game\"]\n",
            ),
            flags: &[],
            reason: "negative:system-dlls",
            at: "Koala/bin/win64/Koala.exe",
            hint_only: false,
        },
        Case {
            what: "a skip_dirs naming the probe's first name",
            text: rule(
                "{ ext = [\"exe\"], at = \"{project}/bin/win64\", first = 8, skip_dirs = [\"Game\"] }",
                winhttp,
                "",
            ),
            flags: &[],
            reason: "negative:system-dlls",
            at: "Koala/bin/win64/Game.exe",
            hint_only: false,
        },
    ];
    for (i, c) in cases.iter().enumerate() {
        let Case {
            what,
            text,
            flags,
            reason,
            at,
            hint_only,
        } = c;
        let d = astra_game_launcher::Descriptor::from_toml(text)
            .unwrap_or_else(|e| panic!("{what}: {e:?}\n{text}"));
        d.check_pass_b(flags.contains(&"--first-party"))
            .unwrap_or_else(|e| panic!("{what}: Pass B: {e:?}"));
        let (exit, r) = validate(&tmp, &format!("{i}.toml"), text, flags);
        assert_eq!(exit, 1, "{what}: {r:#}");
        let got = too_broad_at(&r, 3);
        assert_eq!(
            got.first(),
            Some(&(
                "descriptor-too-broad".to_owned(),
                (*reason).to_owned(),
                (*at).to_owned()
            )),
            "{what}: {r:#}"
        );
        let folder = reason.strip_prefix("negative:").unwrap();
        let place = r["negative_folders"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["folder"] == folder)
            .flat_map(|f| f["places"].as_array().unwrap().iter())
            .find(|p| p["program"] == *at)
            .unwrap_or_else(|| panic!("{what}: no place {at}: {r:#}"))
            .clone();
        assert_eq!(place["claimed"], true, "{what}: {r:#}");
        if *hint_only {
            assert_eq!(place["engine"], "unknown", "{what}: {r:#}");
            assert_eq!(place["engine_with_hint"], "grief", "{what}: {r:#}");
        }
    }
}

/// **Debt row 33, the weakness pinned:** the negative folders are fixed, so a rule may name one
/// of their files in a `none` and step around them all — this one, the hostile fixture with
/// `none = [dwmapi.dll]`, passes `validate-descriptor` and still claims a real game holding
/// `winhttp.dll` and not `dwmapi.dll`. When the row closes, this test flips.
#[test]
fn a_rule_that_names_a_negative_folder_steps_around_it() {
    let tmp = Tmp::new("validate-evades");
    let evading = NESTED_GRIEFER.replace(
        "require = [ { listed = \"winhttp.dll\", kind = \"file\" } ]",
        "require = [ { listed = \"winhttp.dll\", kind = \"file\" },\n            { none = [ { listed = \"dwmapi.dll\", kind = \"file\" } ] } ]",
    );
    assert_ne!(evading, NESTED_GRIEFER);
    let (exit, r) = validate(&tmp, "evading.toml", &evading, &[]);
    assert_eq!(exit, 0, "row 33 closed? {r:#}");
    assert_eq!(engine_of_a_winhttp_game(&tmp, &evading), "grief");
}

// ---------------------------------------------------------------------------------------------
// `gi-probe check`.
// ---------------------------------------------------------------------------------------------

/// `(code, field)` of every finding of a report, in order.
fn answer(r: &Value) -> spec_goldens::Answer {
    r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["code"].as_str().unwrap().to_owned(),
                f["field"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn check(file: &str, extra: &[&str]) -> (i32, Value) {
    let mut args = vec!["check", file];
    args.extend(extra);
    let (exit, r) = probe(&args);
    assert_eq!(r["subcommand"], "check");
    (exit, r)
}

/// **K17 through `gi-probe check`:** every manifest golden of the specification, given to the
/// probe as the bare manifest it is, is answered as `tests/goldens.rs` judges the library —
/// `runner-builtin` as Astra's own, the rest as community, every disagreement the one recorded in
/// `SPEC_DEFECTS_AT_PIN`.
#[test]
fn check_answers_every_manifest_golden_as_labelled() {
    let mut out = Vec::new();
    for (golden, r) in spec_goldens::manifest_goldens() {
        let file = spec_goldens::root().join(&golden);
        let file = file.to_str().unwrap();
        let fp = spec_goldens::first_party(&golden, &r);
        let provenance = if fp { "first-party" } else { "community" };
        let (exit, report) = check(file, &["--provenance", provenance]);
        let a = answer(&report);
        assert_eq!(
            exit,
            if a.is_empty() { 0 } else { 1 },
            "{golden}: {report:#}"
        );
        assert_eq!(report["input"]["kind"], "manifest", "{golden}");
        assert_eq!(report["spec"]["sha"], spec_goldens::pin().as_str());
        out.extend(spec_goldens::judge_manifest(&golden, &r, &a, || {
            answer(&check(file, &["--provenance", "community"]).1)
        }));
    }
    assert!(out.is_empty(), "\n{}", out.join("\n"));
}

const LINK_SCHEMA: &[u8] =
    br#"{"type":"object","properties":{"item":{"type":"string","maxLength":32}}}"#;

/// A community release with a Link, its schema files and skill in the zip.
fn community_release(id: &str, extra_files: &[(&str, &[u8])]) -> Vec<u8> {
    let manifest = format!(
        r#"schema = 1

[integration]
id = "{id}"
name = "Astra in Example"
version = "1.0.0"
license = "MIT"

[integration.capabilities]
companion = ["presentation"]

[integration.link]
context = true
skill = "astra/SKILL.md"
[[integration.link.tools]]
name = "drop_item"
description = "Drop the item the player holds"
params = "astra/tools/drop_item.json"

[target]
steam_appid = 4000
exe = "Example.exe"
platforms = ["windows-x64"]
anti_cheat = "none"

[[files]]
from = "plugins/**"
to = "${{profile}}/plugins/"

[[game_files]]
from = "astra-probe.dll"
to = "${{game}}/astra-probe.dll"

[[config_writes]]
file = "${{profile}}/astra.ini"
section = "bridge"
set = {{ Token = "${{bridge_token}}" }}
"#
    );
    let mut entries = vec![
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
        E::File("plugins/Astra.Example.dll", b"MZ plugin"),
        E::File("plugins/readme.txt", b"hello"),
        E::File("astra/SKILL.md", b"# Example"),
        E::File("astra/tools/drop_item.json", LINK_SCHEMA),
    ];
    entries.retain(|e| {
        !extra_files
            .iter()
            .any(|(n, _)| matches!(e, E::File(m, _) if m == n))
    });
    entries.extend(extra_files.iter().map(|(n, b)| E::File(n, b)));
    zip(&entries)
}

#[test]
fn check_passes_a_well_formed_community_release_and_says_what_it_did_not_check() {
    let tmp = Tmp::new("check-ok");
    let z = put(
        &tmp,
        "r",
        "astra-gi.zip",
        &community_release("someone.example", &[]),
    );
    let (exit, r) = check(&z, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["input"]["kind"], "zip");
    assert_eq!(r["input"]["provenance"], "community");
    assert_eq!(r["integration"]["id"], "someone.example");
    assert_eq!(r["link"]["schemas_validated"], false);
    assert_eq!(
        statuses(&r),
        [
            "pass", "pass", "notice", "skipped", "skipped", "skipped", "pass", "pass"
        ]
    );
    assert_eq!(r["loaders"]["files"][0]["dest"], "${game}/astra-probe.dll");
    assert_eq!(r["loaders"]["files"][0]["class"], "none");
    assert_eq!(r["notices"][0]["code"], "link-schemas-unvalidated");
    assert!(r["built_on"].is_null() && r["code_changed"].is_null());
}

#[test]
fn check_holds_the_links_files_to_the_zip() {
    let tmp = Tmp::new("check-link");
    // At its sizes exactly: passes. One byte over each: refused.
    let schema = vec![b' '; 4 * 1024];
    let skill = vec![b'#'; 16 * 1024];
    let z = community_release(
        "someone.example",
        &[
            ("astra/tools/drop_item.json", &schema),
            ("astra/SKILL.md", &skill),
        ],
    );
    let (exit, r) = check(&put(&tmp, "at", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 0, "{r:#}");
    let big_schema = vec![b' '; 4 * 1024 + 1];
    let big_skill = vec![b'#'; 16 * 1024 + 1];
    let z = community_release(
        "someone.example",
        &[
            ("astra/tools/drop_item.json", &big_schema),
            ("astra/SKILL.md", &big_skill),
        ],
    );
    let (exit, r) = check(&put(&tmp, "big", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(
        answer(&r),
        [
            (
                "link-invalid".to_owned(),
                "integration.link.tools[0].params".to_owned()
            ),
            (
                "link-invalid".to_owned(),
                "integration.link.skill".to_owned()
            ),
        ]
    );
    assert!(
        r["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["step"] == 3 && f["params"]["reason"] == "too-large")
    );
    // A file the link names that the zip lacks.
    let text = community_release("someone.example", &[]);
    let pkg = astra_game_launcher::GiPackage::open(&text, &sha(&text)).unwrap();
    let manifest = pkg
        .manifest_text()
        .replace("astra/tools/drop_item.json", "astra/tools/gone.json");
    let z = zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
        E::File("plugins/Astra.Example.dll", b"MZ plugin"),
        E::File("astra/SKILL.md", b"# Example"),
    ]);
    let (exit, r) = check(&put(&tmp, "gone", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(r["findings"][0]["params"]["reason"], "not-in-zip");
    assert_eq!(r["findings"][0]["params"]["path"], "astra/tools/gone.json");
}

#[test]
fn check_reserves_astras_ids_to_its_own_provenance() {
    let tmp = Tmp::new("check-reserved");
    let z = put(
        &tmp,
        "r",
        "astra-gi.zip",
        &community_release("astra.example", &[]),
    );
    let (exit, r) = check(&z, &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(r["findings"][0]["code"], "reserved-id");
    assert_eq!(r["findings"][0]["step"], 2);
    let (exit, r) = check(&z, &["--provenance", "first-party"]);
    assert_eq!(exit, 0, "{r:#}");
    let (exit, r) = check(&z, &["--provenance", "everyone"]);
    assert_eq!(exit, 2, "{r:#}");
    assert_eq!(r["error"]["code"], "usage");
}

/// A reserved profile name placed through a `**`: the launcher's own open refuses it (step 1).
#[test]
fn check_refuses_a_reserved_profile_name_inside_a_sweep() {
    let tmp = Tmp::new("check-profile-name");
    let release = community_release("someone.example", &[]);
    let pkg = astra_game_launcher::GiPackage::open(&release, &sha(&release)).unwrap();
    let manifest = pkg
        .manifest_text()
        .replace("to = \"${profile}/plugins/\"", "to = \"${profile}/\"");
    let z = zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("astra-probe.dll", DLL),
        E::File("plugins/astra-link.json", b"{}"),
        E::File("astra/SKILL.md", b"# Example"),
        E::File("astra/tools/drop_item.json", LINK_SCHEMA),
    ]);
    let (exit, r) = check(&put(&tmp, "r", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(r["findings"][0]["step"], 1);
    assert_eq!(r["findings"][0]["code"], "path-escapes");
    assert_eq!(r["findings"][0]["params"]["reason"], "reserved");
}

#[test]
fn check_reports_built_on_only_for_the_full_set() {
    let tmp = Tmp::new("check-built-on");
    let foundation = community_release("someone.foundation", &[]);
    let f = put(&tmp, "f", "foundation.zip", &foundation);
    // The same files: verified.
    let gi = community_release("someone.example", &[]);
    let z = put(&tmp, "a", "astra-gi.zip", &gi);
    let (exit, r) = check(&z, &["--foundation", &f]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["built_on"]["id"], "someone.foundation");
    assert_eq!(r["built_on"]["version"], "1.0.0");
    assert_eq!(r["built_on"]["verified"], true, "{r:#}");
    // One of the foundation's files changed: not verified, and it is named.
    let changed = community_release("someone.example", &[("plugins/readme.txt", b"changed")]);
    let (exit, r) = check(
        &put(&tmp, "b", "astra-gi.zip", &changed),
        &["--foundation", &f],
    );
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["built_on"]["verified"], false);
    assert_eq!(
        r["built_on"]["differing"],
        json!(["${profile}/plugins/readme.txt"])
    );
}

#[test]
fn check_reports_code_changed_against_the_previous_release() {
    let tmp = Tmp::new("check-code-changed");
    let previous = put(
        &tmp,
        "p",
        "prev.zip",
        &community_release("someone.example", &[]),
    );
    // Text only changed: no code changed.
    let text_only = community_release("someone.example", &[("plugins/readme.txt", b"v2")]);
    let (exit, r) = check(
        &put(&tmp, "t", "astra-gi.zip", &text_only),
        &["--previous", &previous],
    );
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["code_changed"]["changed"], false, "{r:#}");
    // A plugin DLL changed, and a Lua file added.
    let code = community_release(
        "someone.example",
        &[
            ("plugins/Astra.Example.dll", b"MZ plugin v2"),
            ("plugins/scripts/main.lua", b"print(1)"),
        ],
    );
    let (exit, r) = check(
        &put(&tmp, "c", "astra-gi.zip", &code),
        &["--previous", &previous],
    );
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["code_changed"]["changed"], true);
    assert_eq!(
        r["code_changed"]["files"],
        json!([
            { "dest": "${profile}/plugins/Astra.Example.dll", "change": "changed" },
            { "dest": "${profile}/plugins/scripts/main.lua", "change": "added" },
        ])
    );
}

/// A foundation whose descriptor claims a folder holding one bare program: step 4.
#[test]
fn check_asks_the_descriptors_negative_folders() {
    let tmp = Tmp::new("check-negative");
    let manifest = r#"schema = 1

[integration]
id = "astra.probe"
name = "Probe"
version = "1.0.0"

[integration.engine]
schema = "astra.engine/1"
engine = { codes = { probe = "Probe Engine" } }
[[integration.engine.rule]]
tier = "weak"
programs = { ext = ["exe"], at = "", first = 8 }
require = [ { stat = "{program}", kind = "file" } ]
code = "probe"
confidence = "low"
binary = "windows"
evidence = "{program}"

[target]
engine = "probe"
exe = "*"
platforms = ["windows-x64"]
anti_cheat = "none"

[[files]]
from = "a.txt"
to = "${profile}/a.txt"
"#;
    let z = zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("a.txt", b"a"),
    ]);
    let z = put(&tmp, "r", "astra-gi.zip", &z);
    let (exit, r) = check(&z, &["--provenance", "first-party"]);
    assert_eq!(exit, 1, "{r:#}");
    assert!(
        r["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["step"] == 4 && f["params"]["reason"] == "negative:exe-only"),
        "{r:#}"
    );
    assert_eq!(r["descriptor"]["codes"], json!({ "probe": "Probe Engine" }));
    // As a community release it never gets that far: no anchor (and the id is Astra's).
    let (exit, r) = check(&z, &[]);
    assert_eq!(exit, 1);
    assert_eq!(
        answer(&r),
        [
            ("reserved-id".to_owned(), String::new()),
            (
                "descriptor-too-broad".to_owned(),
                "rule[0].require".to_owned()
            ),
        ]
    );
}

/// Every fault a release has, at once: a zip the launcher does not open for its entries still
/// has its manifest read, and that manifest's own refusal and its descriptor's are named too.
#[test]
fn check_names_every_fault_of_a_release_that_does_not_open() {
    let tmp = Tmp::new("check-faults");
    let manifest = spec_goldens::golden_text("manifests/invalid/engine-undeclared.toml");
    let z = zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("a.txt", b"a"),
        E::File("A.TXT", b"b"),
    ]);
    let (exit, r) = check(&put(&tmp, "r", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 1, "{r:#}");
    let steps: Vec<(u64, String)> = r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["step"].as_u64().unwrap(),
                f["code"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            (1, "zip-unsafe-entry".to_owned()),
            (1, "engine-undeclared".to_owned()),
            (2, "descriptor-invalid".to_owned()),
        ]
    );
}

/// The one exception: a zip that reads as two archives (`src/zip_consistency.rs`) is refused at
/// step 1 with the launcher's own refusal, and nothing more is read out of it — a lint of its
/// manifest would judge one of the two. AstraPlugins' three vectors (`tests/zip_consistency.rs`
/// holds them to its digests).
#[test]
fn check_refuses_a_zip_that_reads_as_two_archives_and_reads_no_further() {
    for (name, entry, reason) in [
        ("slack.zip", "a.txt", "deflate-end"),
        ("upath.zip", "notes.txt", "unicode-path"),
        ("stored-dd.zip", "notes.txt", "descriptor-missing"),
    ] {
        let file = spec_goldens::root()
            .join("testdata/zip-cloaking")
            .join(name);
        for provenance in ["community", "first-party"] {
            let (exit, r) = check(file.to_str().unwrap(), &["--provenance", provenance]);
            assert_eq!(exit, 1, "{name}: {r:#}");
            let found: Vec<(u64, String, String, String)> = r["findings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    let p = |k: &str| f["params"][k].as_str().unwrap_or_default().to_owned();
                    (
                        f["step"].as_u64().unwrap(),
                        f["code"].as_str().unwrap().to_owned(),
                        p("entry"),
                        p("reason"),
                    )
                })
                .collect();
            assert_eq!(
                found,
                [(
                    1,
                    "zip-inconsistent".to_owned(),
                    entry.to_owned(),
                    reason.to_owned()
                )],
                "{name}: {r:#}"
            );
            for step in [2, 3, 4, 7, 8] {
                let s = &r["steps"][step - 1];
                assert_eq!(s["status"], "skipped", "{name} step {step}: {r:#}");
                assert_eq!(
                    s["why"], "the zip reads as two archives (step 1): nothing in it is read",
                    "{name} step {step}"
                );
            }
            assert_eq!(r["integration"], Value::Null, "{name}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `gi-probe check`: the release's own content (step 8).
// ---------------------------------------------------------------------------------------------

/// A finding's or notice's params.
fn params_of(f: &Value) -> BTreeMap<String, String> {
    f["params"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// `gi-probe check` of a release zip as `content_fixtures` answers: every finding, and step 8's
/// notices.
fn content_check(
    tmp: &Tmp,
    name: &str,
    zip: &[u8],
) -> (i32, Value, spec_goldens::Answer, spec_goldens::Answer) {
    let (exit, r) = check(&put(tmp, name, "astra-gi.zip", zip), &[]);
    let refused = r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| content_fixtures::key_of(f["code"].as_str().unwrap(), &params_of(f)))
        .collect();
    let flagged = r["notices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["step"] == 8)
        .map(|n| content_fixtures::key_of(n["code"].as_str().unwrap(), &params_of(n)))
        .collect();
    (exit, r, refused, flagged)
}

/// **The content goldens through `gi-probe check`**: each folder of `content/releases/`, zipped
/// as it is, refused with exactly the one code `vectors.json` gives or passed; and each file of
/// `content/vrm/` as the one `character` part of a release (its licence declared), refused
/// exactly where the gate refuses (the deciding field; the format rule), passed with
/// `licence_source = vrm` where it allows, flagged `content-vrm-meta-unknown` and passed with
/// the declared licence where it reads unknown. The same goldens as AstraPlugins'
/// `tools/check-games-goldens.py`, judged by the same verdicts.
#[test]
fn check_gives_every_content_golden_its_verdict_and_code() {
    let tmp = Tmp::new("check-content-goldens");
    let mut out = Vec::new();
    for v in content_fixtures::release_vectors() {
        let (exit, r, refused, flagged) =
            content_check(&tmp, &v.name, &content_fixtures::zip_of(&v.files));
        let ok = content_fixtures::release_answer_agrees(&v, &refused, &flagged)
            && match v.code {
                None => exit == 0 && r["steps"][7]["status"] != "skipped",
                Some(_) => exit == 1,
            };
        if !ok {
            out.push(format!(
                "release {}: {refused:?} flagged {flagged:?}; wants {:?} flagged {:?}",
                v.name, v.code, v.flags
            ));
        }
    }
    for (name, file, bytes, want) in content_fixtures::gate_vectors() {
        let zip = content_fixtures::gate_release(&file, &bytes);
        let (exit, r, refused, flagged) = content_check(&tmp, &format!("vrm-{name}"), &zip);
        let path = format!("character/{file}");
        let source = &r["content"]["parts"][0]["licence_source"];
        use content_fixtures::GateWant;
        let ok = match &want {
            GateWant::Accept => exit == 0 && flagged.is_empty() && source == "vrm",
            GateWant::Refuse(field, value, defaulted) => {
                let f = &r["findings"][0]["params"];
                refused == [("content-licence-refused".to_owned(), field.clone())]
                    && serde_json::from_str::<Value>(f["value"].as_str().unwrap()).unwrap()
                        == *value
                    && f["defaulted"].as_str() == Some(if *defaulted { "true" } else { "false" })
                    && r["findings"][0]["step"] == 8
            }
            GateWant::Unknown(deciding) => {
                let n = &r["notices"][0]["params"];
                exit == 0
                    && flagged == [("content-vrm-meta-unknown".to_owned(), path.clone())]
                    && source == "declared"
                    && match deciding {
                        None => n.get("meta").is_none(),
                        Some((field, value)) => {
                            n["meta"] == field.as_str()
                                && serde_json::from_str::<Value>(n["value"].as_str().unwrap())
                                    .unwrap()
                                    == *value
                        }
                    }
            }
            GateWant::Conflict => {
                refused.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>()
                    == ["content-licence-conflict"]
            }
            GateWant::Format(reason) => {
                refused == [("content-format".to_owned(), (*reason).to_owned())]
            }
        };
        if !ok {
            out.push(format!(
                "vrm {name}: {refused:?} flagged {flagged:?}; wants {want:?}"
            ));
        }
    }
    assert!(out.is_empty(), "\n{}", out.join("\n"));
}

/// **Every hostile content fixture through `gi-probe check`**: refused as the library refuses it
/// (`tests/content.rs`), and its near miss passing, flagged as it says.
#[test]
fn check_refuses_every_hostile_content_fixture_and_passes_its_near_miss() {
    let tmp = Tmp::new("check-content-hostile");
    let mut out = Vec::new();
    for (i, f) in content_fixtures::hostile().iter().enumerate() {
        let (_, _, refused, flagged) =
            content_check(&tmp, &format!("h{i}"), &content_fixtures::zip_of(&f.files));
        out.extend(content_fixtures::fixture_failure(f, &refused, &flagged));
        out.extend(content_fixtures::near_miss_failure(f, |zip| {
            let (_, _, r, fl) = content_check(&tmp, &format!("h{i}-near"), zip);
            (r, fl)
        }));
    }
    assert!(out.is_empty(), "\n{}", out.join("\n"));
}

/// What step 8 reports of a release that passes: every content-typed file, and each part as a
/// catalogue entry's `content[]` carries it (`games-index-v1.md` §2.1) — what ingest hashed,
/// where its licence came from, the VRM licence read.
#[test]
fn check_reports_the_parts_a_catalogue_entry_carries() {
    let tmp = Tmp::new("check-content-parts");
    let files = content_fixtures::release_vectors()
        .into_iter()
        .find(|v| v.name == "allowed")
        .unwrap()
        .files;
    let (exit, r, _, _) = content_check(&tmp, "allowed", &content_fixtures::zip_of(&files));
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["steps"][7]["status"], "pass");
    assert_eq!(
        r["content"]["content_files"],
        json!(["art/cover.png", "character/avatar.vrm"])
    );
    let parts = r["content"]["parts"].as_array().unwrap();
    let avatar = files
        .iter()
        .find(|(n, _)| n == "character/avatar.vrm")
        .unwrap();
    assert_eq!(parts[1]["sha256"], sha(&avatar.1));
    assert_eq!(parts[1]["licence_source"], "vrm");
    assert_eq!(parts[1]["vrm_licence"]["vrm_version"], "0.x");
    assert_eq!(parts[0]["licence_source"], "declared");
    assert_eq!(parts[0]["licence"], "CC0-1.0");
    assert!(parts[0].get("vrm_licence").is_none());
    // Each part is a `contentPart` of the pinned games-index schema: its members, its required
    // ones, and `vrm_licence` a `vrmLicenceMeta` — taken from the vendored schema itself.
    let schema: Value = serde_json::from_slice(
        &fs::read(spec_goldens::root().join("spec/games/schemas/games-index-v1.schema.json"))
            .unwrap(),
    )
    .unwrap();
    let members = |def: &str| -> (BTreeSet<String>, BTreeSet<String>) {
        let d = &schema["$defs"][def];
        assert_eq!(d["additionalProperties"], false, "{def}");
        let props = d["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let required = d["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        (props, required)
    };
    let (part_props, part_required) = members("contentPart");
    let (meta_props, meta_required) = members("vrmLicenceMeta");
    let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    for p in parts {
        let k = keys(p);
        assert!(
            k.is_subset(&part_props),
            "{:?} beyond contentPart",
            k.difference(&part_props)
        );
        assert!(
            part_required.is_subset(&k),
            "{p}: a required member missing"
        );
        assert!(part_required.iter().all(|m| !p[m].is_null()), "{p}");
        if let Some(m) = p.get("vrm_licence") {
            assert!(
                keys(m).is_subset(&meta_props) && meta_required.is_subset(&keys(m)),
                "{m}"
            );
        }
    }
    // What a glTF part is goes beside the parts.
    assert_eq!(
        r["content"]["classes"],
        json!({ "character/avatar.vrm": "avatar" })
    );
    // A bare manifest: the step says why it did not look.
    let m = tmp.join("astra-gi.toml");
    fs::write(
        &m,
        &files.iter().find(|(n, _)| n == "astra-gi.toml").unwrap().1,
    )
    .unwrap();
    let (exit, r) = check(m.to_str().unwrap(), &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["steps"][7]["status"], "skipped");
    assert!(r["content"].is_null());
}

/// Merged plan §5.3, which the catalogue's ingest raises as `E_CAPABILITY_UNDECLARED`: a release
/// whose `[[config_writes]]` hand `${bridge_token}` on without `[integration.capabilities]
/// companion = ["presentation"]` is refused (`capability-undeclared`, step 2) — the
/// specification's own golden, as the release zip it would be — and declaring it is enough.
#[test]
fn check_refuses_a_bridge_token_release_without_the_capability() {
    let tmp = Tmp::new("check-capability");
    let text = spec_goldens::golden_text("manifests/invalid/capability-undeclared.toml");
    let z = zip(&[E::File("astra-gi.toml", text.as_bytes())]);
    let (exit, r) = check(&put(&tmp, "undeclared", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(
        codes_of(&r)
            .into_iter()
            .map(|(s, c, p)| (s, c, p["capability"].clone(), p["variable"].clone()))
            .collect::<Vec<_>>(),
        [(
            2,
            "capability-undeclared".to_owned(),
            json!("companion.presentation"),
            json!("bridge_token")
        )]
    );
    let declared = text.replacen(
        "[target]",
        "[integration.capabilities]\ncompanion = [\"presentation\"]\n\n[target]",
        1,
    );
    let z = zip(&[E::File("astra-gi.toml", declared.as_bytes())]);
    let (exit, r) = check(&put(&tmp, "declared", "astra-gi.zip", &z), &[]);
    assert_eq!(exit, 0, "{r:#}");
}

// ---------------------------------------------------------------------------------------------
// `gi-probe check`: loaders (K18) and what a community release may place and launch.
// ---------------------------------------------------------------------------------------------

/// A release placing `name` (bytes `bytes`) at `${game}/<to>`, with `extra` TOML after it.
fn placing_release(to: &str, bytes: &[u8], extra: &str) -> Vec<u8> {
    let name = to.rsplit('/').next().unwrap();
    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"someone.example\"\nname = \"Astra in Example\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n\n[target]\nsteam_appid = 4000\nexe = \"Example.exe\"\nplatforms = [\"windows-x64\", \"linux-proton\"]\nanti_cheat = \"none\"\n\n[[game_files]]\nfrom = \"{name}\"\nto = \"${{game}}/{to}\"\n\n{extra}"
    );
    zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File(name, bytes),
    ])
}

fn codes_of(r: &Value) -> Vec<(u64, String, Value)> {
    r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["step"].as_u64().unwrap(),
                f["code"].as_str().unwrap().to_owned(),
                f["params"].clone(),
            )
        })
        .collect()
}

/// **K18**: a hijack-named `[[game_files]]` entry that `[integration.loader] proxies` does not
/// declare fails a community release's check (`loader-undeclared`, step 2); declared, it passes,
/// and step 7 labels it — `other`, or `allowlisted` when `--loaders` lists its digest as off by
/// default. Astra's own releases from before `[integration.loader]` are not asked.
#[test]
fn k18_check_refuses_a_loader_a_community_release_does_not_declare() {
    let tmp = Tmp::new("check-k18");
    let proxy: &[u8] = b"MZ a Doorstop build, off by default";
    let undeclared = put(
        &tmp,
        "u",
        "astra-gi.zip",
        &placing_release("winhttp.dll", proxy, ""),
    );
    let (exit, r) = check(&undeclared, &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(
        codes_of(&r),
        [(
            2,
            "loader-undeclared".to_owned(),
            json!({ "file": "winhttp.dll" })
        )]
    );
    // Astra's own: not asked (labelled all the same).
    let (exit, r) = check(&undeclared, &["--provenance", "first-party"]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["loaders"]["files"][0]["class"], "other");

    let declared = put(
        &tmp,
        "d",
        "astra-gi.zip",
        &placing_release(
            "winhttp.dll",
            proxy,
            "[integration.loader]\nproxies = [\"winhttp.dll\"]\n",
        ),
    );
    let (exit, r) = check(&declared, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["loaders"]["files"][0]["class"], "other");
    assert_eq!(r["notices"][0]["code"], "loader-other");
    assert_eq!(statuses(&r)[6], "notice");
    // The catalogue's allowlist: off by default labels it; on by default does not.
    for (enabled, class) in [(false, "allowlisted"), (true, "other")] {
        let list = tmp.join(&format!("loaders-{enabled}.json"));
        fs::write(
            &list,
            serde_json::to_vec(&json!([{ "name": "doorstop-winhttp", "sha256": sha(proxy), "default_enabled": enabled }]))
                .unwrap(),
        )
        .unwrap();
        let (exit, r) = check(&declared, &["--loaders", list.to_str().unwrap()]);
        assert_eq!(exit, 0, "{r:#}");
        assert_eq!(r["loaders"]["files"][0]["class"], class, "{r:#}");
        assert_eq!(r["loaders"]["allowlist_entries"], 1);
        assert_eq!(
            r["loaders"]["files"][0]["allowlisted_as"],
            if enabled {
                Value::Null
            } else {
                json!("doorstop-winhttp")
            }
        );
    }
    // The allowlist never permits what the manifest does not declare.
    let list = tmp.join("loaders-false.json");
    let (exit, r) = check(&undeclared, &["--loaders", list.to_str().unwrap()]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(codes_of(&r)[0].1, "loader-undeclared");
    // A name that is no loader's: none.
    let plain = put(
        &tmp,
        "p",
        "astra-gi.zip",
        &placing_release("astra.dat", b"data", ""),
    );
    let (exit, r) = check(&plain, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["loaders"]["files"][0]["class"], "none");
    // An allowlist that is not one is a wrong input, never an empty allowlist.
    let bad = tmp.join("bad.json");
    fs::write(
        &bad,
        br#"[{"name": "x", "sha256": "abc", "default_enabled": false}]"#,
    )
    .unwrap();
    let out = Command::new(PROBE)
        .args(["check", &declared, "--loaders", bad.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// The community limits that need no game folder are `gi-probe check` findings, step 2.
#[test]
fn check_holds_a_community_release_to_what_it_may_place_and_launch() {
    let tmp = Tmp::new("check-limits");
    let cases = [
        (
            "BepInEx/plugins/astra.dll",
            "",
            vec![(
                "community-limit",
                json!({ "rule": "game-file-folder", "file": "BepInEx/plugins/astra.dll" }),
            )],
        ),
        (
            "astra.dll",
            "[launch]\nenv = { DOTNET_STARTUP_HOOKS = \"${game}/astra.dll\" }\n",
            vec![(
                "community-limit",
                json!({ "rule": "launch-env", "field": "launch.env.DOTNET_STARTUP_HOOKS" }),
            )],
        ),
        (
            "astra.dll",
            "[launch]\nproton_dll_overrides = { winmm = \"native\" }\n",
            vec![(
                "community-limit",
                json!({ "rule": "launch-override", "field": "launch.proton_dll_overrides.winmm" }),
            )],
        ),
        (
            "astra.dll",
            "[launch]\nargs = [\"-plugin\", \"../../astra.dll\"]\n",
            vec![(
                "community-limit",
                json!({ "rule": "launch-arg-path", "field": "launch.args[1]" }),
            )],
        ),
        (
            "Mods/dinput8.dll",
            "",
            vec![
                ("loader-undeclared", json!({ "file": "Mods/dinput8.dll" })),
                (
                    "community-limit",
                    json!({ "rule": "game-file-folder", "file": "Mods/dinput8.dll" }),
                ),
            ],
        ),
    ];
    for (i, (to, extra, want)) in cases.iter().enumerate() {
        let z = put(
            &tmp,
            &i.to_string(),
            "astra-gi.zip",
            &placing_release(to, b"MZ x", extra),
        );
        let (exit, r) = check(&z, &[]);
        assert_eq!(exit, 1, "{to} {extra}: {r:#}");
        let got: Vec<(String, Value)> = codes_of(&r)
            .into_iter()
            .map(|(step, code, params)| {
                assert_eq!(step, 2);
                (code, params)
            })
            .collect();
        let want: Vec<(String, Value)> = want
            .iter()
            .map(|(c, p)| ((*c).to_owned(), p.clone()))
            .collect();
        assert_eq!(got, want, "{to} {extra}");
        // Astra's own: none of these.
        let (exit, r) = check(&z, &["--provenance", "first-party"]);
        assert_eq!(exit, 0, "{to} {extra}, first-party: {r:#}");
    }
}

/// Every first-party release fixture (`tests/fixtures/first-party/`) passes K18 and the
/// community limits as Astra's own: no `loader-undeclared`, no `community-limit`. (Their one
/// finding, `capability-undeclared`, is L3-3's lint of a release from before 0.5.0, which hands
/// `${bridge_token}` on without `[integration.capabilities]`.) As community releases, every one
/// that places a loader's name beside the game would be `loader-undeclared`.
#[test]
fn the_first_party_fixtures_pass_k18_as_astras_own() {
    let dir = path("tests/fixtures/first-party");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    assert!(files.len() >= 10, "{files:?}");
    let mut undeclared = 0;
    for f in &files {
        let file = f.to_str().unwrap();
        let (_, r) = check(file, &["--provenance", "first-party"]);
        let found: BTreeSet<String> = codes_of(&r).into_iter().map(|(_, c, _)| c).collect();
        assert!(
            found.iter().all(|c| c == "capability-undeclared"),
            "{file}: {found:?}"
        );
        let loaders: Vec<&str> = r["loaders"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["class"] != "none")
            .map(|row| row["dest"].as_str().unwrap())
            .collect();
        let (_, as_community) = check(file, &["--provenance", "community"]);
        let k18: Vec<String> = codes_of(&as_community)
            .into_iter()
            .filter(|(_, c, _)| c == "loader-undeclared")
            .map(|(_, _, p)| format!("${{game}}/{}", p["file"].as_str().unwrap()))
            .collect();
        assert_eq!(k18, loaders, "{file}");
        undeclared += k18.len();
    }
    assert!(
        undeclared > 0,
        "no first-party fixture places a loader's name"
    );
}

// ---------------------------------------------------------------------------------------------
// `gi-probe capture`, the recordings' format, and `validate-descriptor --corpus`.
// ---------------------------------------------------------------------------------------------

/// A PE image's first bytes, then `fill` to `len` bytes, then `tail`.
fn program_bytes(fill: &[u8], len: usize, tail: &[u8]) -> Vec<u8> {
    let mut b = common::pe_with_options(common::X64, &[], b"");
    while b.len() + tail.len() < len {
        b.extend_from_slice(fill);
    }
    b.truncate(len - tail.len());
    b.extend_from_slice(tail);
    b
}

/// What a recording lists in one folder, by name.
fn listed<'a>(rec: &'a replay::Recording, folder: &str) -> BTreeMap<&'a str, &'a replay::Kind> {
    rec.folders[folder]
        .iter()
        .map(|e| (e.name.as_str(), &e.kind))
        .collect()
}

fn capture(dir: &Path, out: &Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec![
        "capture",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ];
    args.extend(extra);
    probe(&args)
}

/// A Unity game with every kind of name beside it: the recording holds every name the walk and
/// the reads near the program list, a file's size, a version header's first 4 KiB, a
/// program's first 4 and last 16 bytes — and no other byte, nor any name past what is listed.
/// Its replay answers as the folder did.
#[test]
fn capture_records_what_detection_lists_and_only_the_bytes_the_rules_read() {
    let tmp = Tmp::new("capture");
    let game = tmp.join("My Game");
    let secret = b"SECRET-CONTENT-NEVER-RECORDED";
    let exe = program_bytes(secret, 9000, b"0123456789abcdef");
    common::write(&game.join("Game.exe"), &exe);
    common::write(&game.join("UnityPlayer.dll"), secret);
    common::write(&game.join("notes.txt"), secret);
    let mut readme = b"#!/bin/sh\n".to_vec();
    readme.extend_from_slice(secret);
    readme.extend_from_slice(b"\nlast sixteen B!\n");
    common::write(&game.join("start"), &readme);
    let mut ggm = b"\0\0\0\0\x16\x002021.3.16f1\0".to_vec();
    ggm.resize(4096, b'.');
    ggm.extend_from_slice(secret);
    ggm.extend_from_slice(b"..last 16 bytes.");
    common::write(&game.join("Game_Data/globalgamemanagers"), &ggm);
    common::write(&game.join("Game_Data/Managed/Assembly-CSharp.dll"), secret);
    common::write(&game.join("Game_Data/Managed/deep/deeper/past.dll"), secret);
    common::write(&game.join("a/b/c/d/e/past-the-walk.txt"), secret);
    // A program at the walk's depth: the folders the rules look up from it are listed too.
    common::write(&game.join("x/y/z/Other.exe"), &exe);
    common::write(
        &game.join("x/y/z/Other_Data/Managed/Assembly-CSharp.dll"),
        secret,
    );
    common::write(&game.join("x/y/z/Other_Data/Managed/a/b/past.dll"), secret);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("Game.exe", game.join("linked.exe")).unwrap();
        let c = std::ffi::CString::new(game.join("pipe").to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }
    let out = tmp.join("game.json");
    let (exit, r) = capture(&game, &out, &[]);
    assert_eq!(exit, 0, "{r:#}");
    assert_eq!(r["subcommand"], "capture");
    let text = fs::read_to_string(&out).unwrap();
    assert!(text.len() <= replay::MAX_RECORDING_BYTES);
    assert_eq!(r["recording"]["bytes"], text.len());
    let rec = replay::Recording::parse(&text).unwrap();
    assert!(
        !text.contains(&hex::encode(b"SECRET")),
        "no byte the rules do not read is recorded: {text}"
    );
    assert!(
        !text.contains(tmp.path().to_str().unwrap()),
        "no path above the folder"
    );
    assert_eq!(rec.game, "My Game");
    let detection = astra_game_launcher::detect_with(&game, None).unwrap();
    assert_eq!(rec.detection, serde_json::to_value(&detection).unwrap());
    assert_eq!(detection.engine.as_str(), "unity-mono");
    assert_eq!(
        rec.label, "unity-mono",
        "the label is what the built-ins name"
    );
    assert_eq!(rec.os, std::env::consts::OS);

    let top = listed(&rec, "");
    use replay::Kind;
    let Kind::File { size, head, tail } = top["Game.exe"] else {
        panic!("{top:?}")
    };
    assert_eq!(
        (*size, head.as_slice(), tail.as_slice()),
        (9000, &exe[..4], &exe[9000 - 16..])
    );
    let Kind::File { head, tail, .. } = top["start"] else {
        panic!()
    };
    assert_eq!(
        (head.as_slice(), tail.as_slice()),
        (&readme[..4], &readme[readme.len() - 16..]),
        "an extension-less file may be a program: its magic and tail"
    );
    for name in ["UnityPlayer.dll", "notes.txt"] {
        assert_eq!(
            top[name],
            &Kind::File {
                size: secret.len() as u64,
                head: vec![],
                tail: vec![]
            },
            "{name}: a name and a size"
        );
    }
    let data = listed(&rec, "Game_Data");
    let Kind::File { size, head, tail } = data["globalgamemanagers"] else {
        panic!()
    };
    assert_eq!(*size, ggm.len() as u64);
    assert_eq!(head.as_slice(), &ggm[..4096], "a version header's 4 KiB");
    assert_eq!(
        tail.as_slice(),
        &ggm[ggm.len() - 16..],
        "extension-less, so Pass B may take it for a program: its tail too"
    );
    assert!(listed(&rec, "Game_Data/Managed").contains_key("Assembly-CSharp.dll"));
    // The walk lists three folders deep and names the fourth; nothing past it.
    assert!(rec.folders.contains_key("Game_Data/Managed/deep"));
    assert!(!rec.folders.contains_key("Game_Data/Managed/deep/deeper"));
    assert!(listed(&rec, "a/b/c").contains_key("d"));
    assert!(!rec.folders.contains_key("a/b/c/d"));
    // Past the walk, the two folders below a program's are listed; the next one is a name.
    assert!(
        listed(&rec, "x/y/z/Other_Data/Managed").contains_key("Assembly-CSharp.dll"),
        "{:?}",
        rec.folders.keys().collect::<Vec<_>>()
    );
    assert!(listed(&rec, "x/y/z/Other_Data/Managed").contains_key("a"));
    assert!(!rec.folders.contains_key("x/y/z/Other_Data/Managed/a"));
    #[cfg(unix)]
    {
        assert_eq!(top["linked.exe"], &Kind::Link);
        assert_eq!(top["pipe"], &Kind::Fifo);
    }

    // The replay is the folder: sparse files of the recorded sizes, the recorded bytes in place.
    let again = tmp.join("replay");
    fs::create_dir(&again).unwrap();
    replay::materialise(&rec, &again).unwrap();
    let laid = fs::read(again.join("Game.exe")).unwrap();
    assert_eq!(laid.len(), 9000);
    assert_eq!(
        (&laid[..4], &laid[9000 - 16..]),
        (&exe[..4], &exe[9000 - 16..])
    );
    assert!(laid[4..9000 - 16].iter().all(|b| *b == 0));
    assert_eq!(
        serde_json::to_value(astra_game_launcher::detect_with(&again, None).unwrap()).unwrap(),
        rec.detection
    );
    #[cfg(unix)]
    {
        let m = fs::symlink_metadata(again.join("linked.exe")).unwrap();
        assert!(m.file_type().is_symlink());
        assert!(
            fs::metadata(again.join("linked.exe")).is_err(),
            "it leads nowhere"
        );
        use std::os::unix::fs::FileTypeExt;
        assert!(
            fs::symlink_metadata(again.join("pipe"))
                .unwrap()
                .file_type()
                .is_fifo()
        );
    }

    // A label and an appid given, and Steam's hint: recorded as given.
    let (exit, r) = capture(
        &game,
        &out,
        &[
            "--label",
            "probe-engine",
            "--appid",
            "480",
            "--hint",
            "Game.exe",
        ],
    );
    assert_eq!(exit, 0, "{r:#}");
    let rec = replay::Recording::parse(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        (rec.label.as_str(), rec.appid, rec.hint.as_deref()),
        ("probe-engine", Some(480), Some("Game.exe"))
    );
    let hinted = astra_game_launcher::detect_with(&game, Some("Game.exe")).unwrap();
    assert!(hinted.hint_used);
    assert_eq!(rec.detection, serde_json::to_value(&hinted).unwrap());
    let (exit, r) = capture(&game, &out, &["--label", "Not A Code"]);
    assert_eq!(exit, 2, "{r:#}");
}

/// A recording over 1 MiB is refused, saying so, and nothing is written.
#[test]
fn capture_refuses_a_recording_over_a_mebibyte() {
    let tmp = Tmp::new("capture-large");
    let game = tmp.join("game");
    fs::create_dir_all(game.join("many")).unwrap();
    let long = "n".repeat(120);
    for i in 0..10_000 {
        fs::File::create(game.join("many").join(format!("{long}{i:05}"))).unwrap();
    }
    let out = tmp.join("large.json");
    let (exit, r) = capture(&game, &out, &[]);
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(r["findings"][0]["code"], "recording-too-large");
    assert_eq!(
        r["findings"][0]["params"]["limit"],
        replay::MAX_RECORDING_BYTES.to_string()
    );
    assert!(r["recording"]["bytes"].as_u64().unwrap() > replay::MAX_RECORDING_BYTES as u64);
    assert!(!out.exists(), "nothing written");
    assert_eq!(r["recording"]["walk"]["capped"], true);
}

/// A minimal recording, as text: `folders` given as JSON.
fn recording_text(label: &str, folders: Value) -> String {
    let mut v = json!({
        "schema": replay::SCHEMA,
        "game": "synthetic",
        "label": label,
        "captured": {"os": "synthetic", "tool": {}},
        "detection": {},
        "folders": folders,
    });
    if folders_is_null(&v) {
        v.as_object_mut().unwrap().remove("folders");
    }
    v.to_string()
}

fn folders_is_null(v: &Value) -> bool {
    v["folders"].is_null()
}

/// The reader refuses every recording that breaks the format, and lays what it accepts as it
/// says: sparse, the recorded bytes at their offsets.
#[test]
fn a_recording_that_breaks_the_format_is_refused() {
    let ok = json!({"": [["Game.exe", "f", 100, "4d5a", "ffff"], ["Data", "d"], ["l", "l"]],
                    "Data": [["x", "f", 0]]});
    let rec = replay::Recording::parse(&recording_text("unknown", ok.clone())).unwrap();
    assert_eq!(rec.entries(), 4);
    let tmp = Tmp::new("replay-format");
    replay::materialise(&rec, tmp.path()).unwrap();
    let laid = fs::read(tmp.join("Game.exe")).unwrap();
    assert_eq!(laid.len(), 100);
    assert_eq!((&laid[..2], &laid[98..]), (&b"MZ"[..], &[0xff, 0xff][..]));
    assert!(tmp.join("Data").is_dir() && tmp.join("Data/x").is_file());
    assert!(
        replay::materialise(&rec, tmp.path()).is_err(),
        "only in an empty folder"
    );
    let head_4k = "00".repeat(replay::MAX_HEAD + 1);
    let tail_17 = "00".repeat(replay::MAX_TAIL + 1);
    for (why, text) in [
        ("an unknown key", {
            let mut v: Value =
                serde_json::from_str(&recording_text("unknown", ok.clone())).unwrap();
            v["extra"] = json!(1);
            v.to_string()
        }),
        (
            "another schema",
            recording_text("unknown", ok.clone()).replace("list-shape/1", "list-shape/2"),
        ),
        (
            "a label that is no code",
            recording_text("Not A Code", ok.clone()),
        ),
        (
            "no game folder",
            recording_text("unknown", json!({"Data": []})),
        ),
        (
            "a folder its parent does not hold",
            recording_text("unknown", json!({"": [], "Data": []})),
        ),
        (
            "a name twice",
            recording_text("unknown", json!({"": [["a", "d"], ["a", "f", 1]]})),
        ),
        (
            "a name with a slash",
            recording_text("unknown", json!({"": [["a/b", "f", 1]]})),
        ),
        (
            "a dot-dot",
            recording_text("unknown", json!({"": [["..", "d"]]})),
        ),
        (
            "a head over 4 KiB",
            recording_text("unknown", json!({"": [["a", "f", 10_000, head_4k]]})),
        ),
        (
            "a tail over 16 B",
            recording_text("unknown", json!({"": [["a", "f", 100, "", tail_17]]})),
        ),
        (
            "more bytes than the file",
            recording_text("unknown", json!({"": [["a", "f", 1, "0000"]]})),
        ),
        (
            "an unknown kind",
            recording_text("unknown", json!({"": [["a", "x"]]})),
        ),
        (
            "a size on a folder",
            recording_text("unknown", json!({"": [["a", "d", 1]]})),
        ),
        ("no folders", recording_text("unknown", Value::Null)),
        ("over 1 MiB", {
            let names: Vec<Value> = (0..60_000)
                .map(|i| json!([format!("name-{i:08}"), "f", 0]))
                .collect();
            recording_text("unknown", json!({"": names}))
        }),
    ] {
        assert!(replay::Recording::parse(&text).is_err(), "{why}: accepted");
    }
}

/// A Source 2-shaped game as a synthetic recording, labelled `label`: what the Source 2
/// descriptor claims (`engine2.dll` beside the program, `core/` in its project), or, with
/// `anchored` false, the same game without its anchor.
fn source2_recording(label: &str, anchored: bool) -> String {
    let mut bin = vec![json!(["game.exe", "f", 4096, "4d5a9000"])];
    if anchored {
        bin.push(json!(["engine2.dll", "f", 1024]));
    }
    recording_text(
        label,
        json!({
            "": [["game", "d"]],
            "game": [["bin", "d"], ["core", "d"]],
            "game/bin": [["win64", "d"]],
            "game/bin/win64": bin,
        }),
    )
}

/// Spec §5: a descriptor that claims recordings labelled with two or more engines is too broad
/// (`descriptor-too-broad{reason=two-engines}`); claiming one engine's, or recordings no label
/// names an engine of, is not.
#[test]
fn validate_descriptor_corpus_refuses_claims_on_two_engines() {
    let tmp = Tmp::new("validate-corpus");
    let corpus = |name: &str, files: &[(&str, String)]| -> PathBuf {
        let dir = tmp.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (f, text) in files {
            fs::write(dir.join(f), text).unwrap();
        }
        dir
    };
    let two = corpus(
        "two",
        &[
            ("a.json", source2_recording("source2", true)),
            ("b.json", source2_recording("id-tech-7", true)),
            ("c.json", source2_recording("unity-mono", false)),
        ],
    );
    let (exit, r) = validate(
        &tmp,
        "source2.toml",
        SOURCE2,
        &["--corpus", two.to_str().unwrap()],
    );
    assert_eq!(exit, 1, "{r:#}");
    assert_eq!(
        reason_of(&r),
        [("descriptor-too-broad".to_owned(), "two-engines".to_owned())]
    );
    assert_eq!(r["findings"][0]["step"], 4);
    assert_eq!(r["findings"][0]["params"]["labels"], "id-tech-7,source2");
    assert_eq!(
        r["corpus"]["engines_claimed"],
        json!(["id-tech-7", "source2"])
    );
    let rows = r["corpus"]["recordings"].as_array().unwrap();
    let claimed: Vec<(&str, bool)> = rows
        .iter()
        .map(|row| {
            (
                row["recording"].as_str().unwrap(),
                row["claimed"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        claimed,
        [("a.json", true), ("b.json", true), ("c.json", false)]
    );
    assert_eq!(rows[0]["engine"], "source2");

    for (name, files) in [
        (
            "one-engine",
            vec![
                ("a.json", source2_recording("source2", true)),
                ("b.json", source2_recording("source2", true)),
                ("c.json", source2_recording("id-tech-7", false)),
            ],
        ),
        (
            "unlabelled",
            vec![
                ("a.json", source2_recording("unknown", true)),
                ("b.json", source2_recording("source2", true)),
            ],
        ),
    ] {
        let dir = corpus(name, &files);
        let (exit, r) = validate(
            &tmp,
            "source2.toml",
            SOURCE2,
            &["--corpus", dir.to_str().unwrap()],
        );
        assert_eq!(exit, 0, "{name}: {r:#}");
    }

    // A folder holding something that is not a recording: nothing concluded.
    let bad = corpus("bad", &[("a.json", "{}".to_owned())]);
    let (exit, r) = validate(
        &tmp,
        "source2.toml",
        SOURCE2,
        &["--corpus", bad.to_str().unwrap()],
    );
    assert_eq!(exit, 2, "{r:#}");

    // The real recordings: none of them is a Source 2 game, and none is claimed.
    let (exit, r) = validate(
        &tmp,
        "source2.toml",
        SOURCE2,
        &["--corpus", path("tests/corpus/recorded").to_str().unwrap()],
    );
    assert_eq!(exit, 0, "{r:#}");
    let rows = r["corpus"]["recordings"].as_array().unwrap();
    assert!(rows.len() >= 8 && rows.iter().all(|row| row["claimed"] == false));
}

/// A recording is data someone else made (a pull request's recordings, `validate-descriptor
/// --corpus`): every name it holds, a file's or a folder key's, must be one component the
/// launcher would write on every OS. One Windows reads as a path — climbing out (`..\x`), a
/// drive (`C:\…`, `C:x`), a share (`\\host\share\x`, which opens SMB with the author's
/// credentials), a stream (`a:b`), a device (`CON`), a trailing dot — refuses the whole
/// recording, never laid, on this OS as on Windows.
#[test]
fn a_recording_whose_names_windows_reads_as_paths_is_refused() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/recorded");
    let first = replay::recordings_in(&dir).unwrap().remove(0);
    let text = fs::read_to_string(&first).unwrap();
    replay::Recording::parse(&text).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let hostile = [
        r"..\..\x.cmd",
        r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\x.cmd",
        r"\\attacker\share\x",
        "C:x",
        "a:b",
        "CON",
        "nul.txt",
        "trailing.",
        "trailing ",
        "a\u{1}b",
        "what?",
    ];
    for name in hostile {
        // As a file's name in the top folder.
        let mut file = v.clone();
        file["folders"][""]
            .as_array_mut()
            .unwrap()
            .push(json!([name, "f", 4, "68690a00"]));
        let e = replay::Recording::parse(&file.to_string()).unwrap_err();
        assert!(e.contains("is not a name a folder holds"), "{name:?}: {e}");
        // As a folder key.
        let mut folder = v.clone();
        folder["folders"][""]
            .as_array_mut()
            .unwrap()
            .push(json!([name, "d"]));
        folder["folders"][name] = json!([["x.txt", "f", 1, "78"]]);
        assert!(
            replay::Recording::parse(&folder.to_string()).is_err(),
            "{name:?} as a folder"
        );
    }
    // The committed recordings all keep to the rule.
    for f in replay::recordings_in(&dir).unwrap() {
        replay::Recording::parse(&fs::read_to_string(&f).unwrap())
            .unwrap_or_else(|e| panic!("{f:?}: {e}"));
    }
}
