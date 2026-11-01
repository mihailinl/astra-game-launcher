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
//!   the anchorless, the runtime-anchored and the too broad.
//! - `check` answers every manifest golden of the specification as `tests/goldens.rs` judges the
//!   library (the same `SPEC_DEFECTS_AT_PIN`), and on release zips: Link files, reserved ids by
//!   provenance, the descriptor's negative folders, `built_on` and `code_changed`.
//! - `capture` records what detection lists and only the bytes the rules read, refuses a
//!   recording over 1 MiB, and replays as the folder it was taken from; the format's reader
//!   (`tests/corpus/replay.rs`) refuses what breaks it; `validate-descriptor --corpus` refuses a
//!   descriptor claiming recordings of two engines.

#![cfg(feature = "gi-probe")]

mod common;
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
    assert_eq!(
        spec_goldens::pin(),
        "d46e44a089387589018f28cfc3403399f7c3b89d"
    );
    let folders = r["negative_folders"].as_array().unwrap();
    assert_eq!(folders.len(), 4);
    assert!(folders.iter().all(|f| f["claimed"] == false));

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
            "pass", "pass", "notice", "skipped", "skipped", "skipped", "pass"
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
