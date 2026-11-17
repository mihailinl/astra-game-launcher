// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `[integration.link]` as Astra Link v1 has it at AstraPlugins `fe665e4` (`manifest.md` §2.6,
//! `link-v1.md` §5): the spec's own manifest goldens for `talk`, a tool's `danger`, `companion`
//! and required `result`, an event's `speaks` and closed name, held to [`Manifest::lint`] and
//! [`link_digest`]. The goldens are `tests/fixtures/link-v1/` (its README says why they are not in
//! the vendored copy yet), each byte checked against its `SHA256SUMS`.
//!
//! The digests: `game-specific-link`'s is the spec's own vector (`vectors.json` `link.digest` at
//! `fe665e4`); `link-danger-speaks-talk`, which no vector covers, was computed by a separate
//! implementation (Python `tomllib`, members sorted by UTF-16 code units, `json.dumps` with
//! `ensure_ascii=False`), never by this crate — the same implementation reproduces the spec's
//! `game-specific-link` vector.

use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{Manifest, codes, link_digest, toml_newlines};
use sha2::{Digest, Sha256};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/link-v1")
}

fn text(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `(code, field, reason)` of every refusal Astra's own publisher would meet: the link's alone,
/// since a first-party lint asks none of a community integration's limits.
fn lint(rel: &str) -> Vec<(String, String, String)> {
    let m = Manifest::parse(&text(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    m.lint(true)
        .iter()
        .map(|e| {
            (
                e.code.to_owned(),
                e.param("field").or(e.param("key")).unwrap_or("").to_owned(),
                e.param("reason").unwrap_or("").to_owned(),
            )
        })
        .collect()
}

#[test]
fn the_fixtures_are_the_copied_bytes() {
    let sums = text("SHA256SUMS");
    let mut listed = Vec::new();
    for line in sums.lines() {
        let (hex, rel) = line.split_once("  ").expect("sha256sum format");
        let bytes = fs::read(root().join(rel)).unwrap();
        assert_eq!(
            hex,
            format!("{:x}", Sha256::digest(&bytes)),
            "{rel} is not the copied file"
        );
        listed.push(rel.to_owned());
    }
    let mut found = Vec::new();
    for dir in ["manifests/valid", "manifests/invalid"] {
        for e in fs::read_dir(root().join(dir)).unwrap() {
            found.push(format!(
                "{dir}/{}",
                e.unwrap().file_name().to_str().unwrap()
            ));
        }
    }
    found.sort();
    assert_eq!(listed, found, "every fixture is summed, and only they");
}

#[test]
fn the_specs_valid_link_manifests_pass_the_lint() {
    for rel in [
        "manifests/valid/game-specific-link.toml",
        "manifests/valid/link-danger-speaks-talk.toml",
    ] {
        assert_eq!(lint(rel), [], "{rel}");
    }
}

#[test]
fn each_invalid_link_manifest_is_refused_for_its_rule_alone() {
    let link = |field: &str, reason: &str| {
        (
            codes::LINK_INVALID.to_owned(),
            format!("integration.link.{field}"),
            reason.to_owned(),
        )
    };
    for (rel, want) in [
        // `context` names the context schema now: the draft's boolean is refused.
        ("link-context-boolean", link("context", "shape")),
        ("link-danger-unknown", link("tools[0].danger", "danger")),
        (
            "link-event-unknown-word",
            link("events[0].name", "unknown-event"),
        ),
        // Only an event speaks: on a trigger the key is not one it has.
        (
            "link-speaks-on-trigger",
            (
                codes::UNKNOWN_KEY.to_owned(),
                "integration.link.triggers[0].speaks".to_owned(),
                String::new(),
            ),
        ),
        ("link-tool-no-result", link("tools[0].result", "missing")),
        ("link-too-many-tools", link("tools", "too-many")),
    ] {
        assert_eq!(
            lint(&format!("manifests/invalid/{rel}.toml")),
            [want],
            "{rel}"
        );
    }
}

#[test]
fn the_link_digest_covers_the_new_keys_as_written() {
    for (rel, digest) in [
        (
            "manifests/valid/game-specific-link.toml",
            "6163886130696822d12248f4f7342e427b72d7808daab8ebc8cec050e4f77a2b",
        ),
        (
            "manifests/valid/link-danger-speaks-talk.toml",
            "acd45dca016e09cda6aa53c8d3a857b26b0abf586ff604b8612965aee5ac8281",
        ),
    ] {
        let lf = text(rel);
        let crlf = lf.replace('\n', "\r\n");
        for t in [&lf, &crlf] {
            let m = Manifest::parse(t).unwrap();
            assert_eq!(
                link_digest(m.link()).unwrap().as_deref(),
                Some(digest),
                "{rel}"
            );
        }
        // Each new key is in the digest: written differently, the digest differs.
        for (from, to) in [
            ("talk = true", "talk = false"),
            ("danger = \"low\"", "danger = \"high\""),
            ("companion = true", "companion = false"),
            ("speaks = true", "speaks = false"),
            (
                "result = \"astra/tools/drop_item",
                "result = \"astra/tools/drop_itemx",
            ),
        ] {
            if !lf.contains(from) {
                continue;
            }
            let m = Manifest::parse(&toml_newlines(&lf.replacen(from, to, 1))).unwrap();
            assert_ne!(
                link_digest(m.link()).unwrap().as_deref(),
                Some(digest),
                "{rel}: {from} -> {to}"
            );
        }
    }
}
