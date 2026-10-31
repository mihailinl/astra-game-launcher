// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe check`: may this release be published? What a catalogue's ingest asks of every
//! release before it is listed (merged plan §3.2 lane L, A§5.2 step 5; AstraPlugins
//! `spec/games/manifest.md` §4, at [`crate::descriptor::SPEC_SHA`]), with this launcher's own
//! parser — there is one, and the launcher installs with it:
//!
//! 1. the launcher opens the zip ([`GiPackage::open`]: its hygiene under the default [`Limits`],
//!    the manifest, every file it names in the zip, no reserved profile name placed, a `**`
//!    expansion included). Given a bare `astra-gi.toml` instead of a zip, only
//!    [`Manifest::parse`]; the zip's checks are said not to be made;
//! 2. [`Manifest::lint`]: a community integration's identity (`reserved-id`, `reserved-name`),
//!    `capability-undeclared`, Astra Link v1's counts, names and lengths (`link-invalid`), the
//!    foundation claim's keys, `[integration.engine]` and Pass B's admission of it — with
//!    `--provenance first-party`, Astra's own ids and names and the first-party grammar are
//!    allowed. When the manifest does not parse, `[integration.engine]` is still asked;
//! 3. every file `[integration.link]` names is in the zip, a `params`/`data` schema at most
//!    4 KiB and the skill at most 16 KiB. Whether each schema keeps to `link-v1.md` §6's
//!    restricted profile waits for `link-v1` to freeze; the report's `link` says so;
//! 4. the descriptor claims none of the spec's negative folders (`validate-descriptor`'s step 3);
//! 5. `built_on{id, version, verified}`: the foundation (`--foundation`, a release zip) as this
//!    release is built on it — `verified` only when every file the foundation places, this one
//!    places alike (destination and digest; A§4.4 M3, the full set). A report, never a finding:
//!    an unverified claim earns no badge. A claim (`[integration.foundation]`) that names another
//!    foundation is a notice;
//! 6. `code_changed`: every placed file that is code ([`is_code`]), from `[[files]]` or
//!    `[[game_files]]`, new or changed against the previous release (`--previous`) — A§4.2's rule
//!    read the stricter way, game files included. A report: what the update sheet shows.
//!
//! Exit 0: no findings; 1: findings. `--provenance` defaults to `community`.

use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    GiPackage, LauncherError, Limits, MANIFEST_FILE, Manifest, PlacedFile, codes, link_limits,
};
use serde_json::{Value, json};
use zip::ZipArchive;

use crate::compat::sha256_hex;
use crate::descriptor::{negative_folders, summary};
use crate::{Fatal, Finding, Probe, report};

const STEPS: [&str; 6] = [
    "the launcher opens the zip and parses its manifest",
    "identity, capabilities, Astra Link's limits, the foundation claim and the engine descriptor",
    "every file [integration.link] names is in the zip, within its size",
    "the engine descriptor claims none of the spec's negative folders",
    "built on the foundation: every file it places, placed alike",
    "code changed since the previous release",
];

/// What a file holding code ends in: A§4.2's `.dll .so .dylib .jar .lua .py .js .exe`, and what
/// loads as code just the same (`.asi` plugins, `.pyd`/`.pyc` Python, a versioned `.so.N`).
const CODE_EXTENSIONS: &[&str] = &[
    "dll", "so", "dylib", "jar", "lua", "py", "js", "exe", "asi", "pyd", "pyc",
];

/// Whether a placed file's name is code ([`CODE_EXTENSIONS`]).
pub fn is_code(dest: &str) -> bool {
    let name = dest.rsplit('/').next().unwrap_or(dest).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    CODE_EXTENSIONS.contains(&ext) || name.contains(".so.")
}

pub struct CheckArgs {
    pub file: PathBuf,
    pub first_party: bool,
    pub foundation: Option<PathBuf>,
    pub previous: Option<PathBuf>,
}

fn read(path: &Path) -> Result<Vec<u8>, Fatal> {
    std::fs::read(path).map_err(|e| Fatal::io(path, e))
}

fn refusal(e: &LauncherError) -> Finding {
    Finding::from_refusal(e.code, &e.params, &e.to_string())
}

/// A zip's entries: name (`/`-separated) → uncompressed size. Unreadable entries are left out:
/// step 1 has said what is wrong with the zip.
fn entries(zip: &[u8]) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let Ok(mut ar) = ZipArchive::new(Cursor::new(zip)) else {
        return out;
    };
    for i in 0..ar.len() {
        if let Ok(f) = ar.by_index_raw(i)
            && !f.is_dir()
        {
            out.insert(f.name().replace('\\', "/"), f.size());
        }
    }
    out
}

/// `astra-gi.toml`'s text out of a zip the launcher would not open, for step 2: at most the
/// default manifest limit, UTF-8.
fn manifest_text_of(zip: &[u8]) -> Option<String> {
    let mut ar = ZipArchive::new(Cursor::new(zip)).ok()?;
    let f = ar.by_name(MANIFEST_FILE).ok()?;
    let limit = Limits::default().max_manifest_bytes;
    if f.size() > limit {
        return None;
    }
    let mut text = String::new();
    f.take(limit).read_to_string(&mut text).ok()?;
    Some(text)
}

/// A release zip given beside the one checked, opened by the launcher.
fn open_other(path: &Path) -> Result<std::result::Result<GiPackage, LauncherError>, Fatal> {
    let bytes = read(path)?;
    Ok(GiPackage::open(&bytes, &sha256_hex(&bytes)))
}

/// Destination (ASCII case folded: Windows' and the ledger's reading) → the file.
fn by_dest(files: Vec<PlacedFile>) -> BTreeMap<String, PlacedFile> {
    files
        .into_iter()
        .map(|f| (f.dest.to_ascii_lowercase(), f))
        .collect()
}

/// `gi-probe check`.
pub fn run(a: &CheckArgs) -> Result<(Value, bool), Fatal> {
    let bytes = read(&a.file)?;
    let is_zip = bytes.starts_with(b"PK");
    let sha256 = sha256_hex(&bytes);
    let mut p = Probe::new(&STEPS);

    // 1. Open (a zip) or parse (a manifest).
    let mut open_error = None;
    let (pkg, text) = if is_zip {
        match GiPackage::open(&bytes, &sha256) {
            Ok(pkg) => {
                let text = pkg.manifest_text().to_owned();
                (Some(pkg), Some(text))
            }
            Err(e) => {
                p.finding(1, refusal(&e));
                open_error = Some(e);
                (None, manifest_text_of(&bytes))
            }
        }
    } else {
        let text = String::from_utf8(bytes.clone()).map_err(|_| Fatal::io(&a.file, "not UTF-8"))?;
        (None, Some(text))
    };
    let parsed = text
        .as_deref()
        .map(|t| Manifest::parse_and_lint(t, a.first_party));
    // The manifest's own refusal, unless it is what stopped the zip's opening already.
    if let Some((Err(e), _)) = &parsed
        && open_error.as_ref() != Some(e)
    {
        p.finding(1, refusal(e));
    }
    p.close(
        1,
        0,
        0,
        json!({ "zip": is_zip, "zip_checks": if is_zip { "made" } else { "not made: a manifest, not a zip" } }),
    );
    let manifest = pkg
        .as_ref()
        .map(|k| k.manifest().clone())
        .or_else(|| parsed.as_ref().and_then(|(m, _)| m.as_ref().ok().cloned()));

    // 2. Lint.
    match &parsed {
        None => p.skip(2, "the zip holds no readable astra-gi.toml"),
        Some((_, lint)) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            for e in lint {
                p.finding(2, refusal(e));
            }
            p.close(2, f0, n0, Value::Null);
        }
    }

    // 3. The files a link names.
    let link_files = manifest
        .as_ref()
        .map(Manifest::link_files)
        .unwrap_or_default();
    let mut link = Value::Null;
    if let Some(m) = &manifest
        && m.link().is_some()
    {
        link = json!({
            "files": link_files.len(),
            "schemas_validated": false,
            "why": "validating params/data schemas against link-v1.md §6's restricted profile waits for link-v1 to freeze",
        });
    }
    match (&manifest, is_zip) {
        (None, _) => p.skip(3, "the manifest does not parse"),
        (Some(_), false) => p.skip(3, "a manifest, not a zip"),
        (Some(_), true) if link_files.is_empty() => p.skip(3, "no [integration.link] files"),
        (Some(_), true) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            let in_zip = entries(&bytes);
            for (field, path) in &link_files {
                let limit = if field.ends_with(".skill") {
                    link_limits::SKILL_BYTES
                } else {
                    link_limits::SCHEMA_BYTES
                };
                let e = match in_zip.get(path) {
                    None => LauncherError::new(codes::LINK_INVALID)
                        .with("field", field.clone())
                        .with("reason", "not-in-zip")
                        .with("path", path.clone()),
                    Some(size) if *size > limit => LauncherError::new(codes::LINK_INVALID)
                        .with("field", field.clone())
                        .with("reason", "too-large")
                        .with("limit", limit.to_string())
                        .with("bytes", size.to_string()),
                    Some(_) => continue,
                };
                p.finding(3, refusal(&e));
            }
            p.notice(
                3,
                Finding::new(
                    "link-schemas-unvalidated",
                    "the params/data schemas are not yet held to link-v1.md §6's restricted profile: that waits for link-v1 to freeze",
                ),
            );
            p.close(3, f0, n0, Value::Null);
        }
    }

    // 4. The descriptor's negative folders.
    let mut descriptor = Value::Null;
    let mut folders = Vec::new();
    match manifest.as_ref().and_then(Manifest::engine_descriptor) {
        None => p.skip(4, "no [integration.engine] the launcher reads"),
        Some(Err(_)) => p.skip(4, "[integration.engine] is not a descriptor (step 2)"),
        Some(Ok(d)) => {
            descriptor = summary(d);
            match negative_folders(d, a.first_party)? {
                Err(_) => p.skip(4, "Pass B does not admit the descriptor (step 2)"),
                Ok((rows, claims)) => {
                    let (f0, n0) = (p.findings.len(), p.notices.len());
                    folders = rows;
                    for f in claims {
                        p.finding(4, f);
                    }
                    p.close(4, f0, n0, Value::Null);
                }
            }
        }
    }

    // 5. Built on.
    let claim = manifest.as_ref().and_then(|m| m.foundation().cloned());
    let mut built_on = claim.as_ref().map_or(
        Value::Null,
        |c| json!({ "id": c.id, "version": c.version, "verified": false, "claimed": c }),
    );
    match (&a.foundation, &pkg) {
        (None, _) => p.skip(5, "no --foundation"),
        (Some(_), None) => p.skip(5, "the release does not open (step 1)"),
        (Some(path), Some(pkg)) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            match open_other(path)? {
                Err(e) => {
                    p.notice(
                        5,
                        Finding::from_refusal(e.code, &e.params, "the foundation does not open")
                            .field("foundation"),
                    );
                    built_on = json!({ "id": Value::Null, "version": Value::Null, "verified": false, "claimed": claim });
                }
                Ok(f) => {
                    let mine = by_dest(pkg.placed_files());
                    let theirs = f.placed_files();
                    let mut missing = Vec::new();
                    let mut differing = Vec::new();
                    for t in &theirs {
                        match mine.get(&t.dest.to_ascii_lowercase()) {
                            None => missing.push(t.dest.clone()),
                            Some(m) if m.sha256 != t.sha256 => differing.push(t.dest.clone()),
                            Some(_) => {}
                        }
                    }
                    let fi = &f.manifest().integration;
                    let verified = !theirs.is_empty() && missing.is_empty() && differing.is_empty();
                    if let Some(c) = &claim
                        && (c.id != fi.id || c.version != fi.version)
                    {
                        p.notice(
                            5,
                            Finding::new(
                                "foundation-claim-differs",
                                format!(
                                    "[integration.foundation] claims {}@{}; --foundation is {}@{}",
                                    c.id, c.version, fi.id, fi.version
                                ),
                            )
                            .field("integration.foundation"),
                        );
                    }
                    built_on = json!({
                        "id": fi.id,
                        "version": fi.version,
                        "verified": verified,
                        "foundation_sha256": f.sha256(),
                        "files": theirs.len(),
                        "missing": missing,
                        "differing": differing,
                        "claimed": claim,
                    });
                }
            }
            p.close(5, f0, n0, Value::Null);
        }
    }

    // 6. Code changed.
    let mut code_changed = Value::Null;
    match (&a.previous, &pkg) {
        (None, _) => p.skip(6, "no --previous"),
        (Some(_), None) => p.skip(6, "the release does not open (step 1)"),
        (Some(path), Some(pkg)) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            match open_other(path)? {
                Err(e) => p.notice(
                    6,
                    Finding::from_refusal(e.code, &e.params, "the previous release does not open")
                        .field("previous"),
                ),
                Ok(prev) => {
                    let before = by_dest(prev.placed_files());
                    let files: Vec<Value> = pkg
                        .placed_files()
                        .into_iter()
                        .filter(|f| is_code(&f.dest))
                        .filter_map(|f| {
                            let change = match before.get(&f.dest.to_ascii_lowercase()) {
                                None => "added",
                                Some(b) if b.sha256 != f.sha256 => "changed",
                                Some(_) => return None,
                            };
                            Some(json!({ "dest": f.dest, "change": change }))
                        })
                        .collect();
                    code_changed = json!({
                        "against_sha256": prev.sha256(),
                        "changed": !files.is_empty(),
                        "files": files,
                    });
                }
            }
            p.close(6, f0, n0, Value::Null);
        }
    }

    let integration = manifest.as_ref().map_or(Value::Null, |m| {
        json!({
            "id": m.integration.id,
            "name": m.integration.name,
            "version": m.integration.version,
            "repo": m.integration.repo,
            "engine": m.engine_code().map(|c| c.as_str().to_owned()),
            "sdk": m.sdk(),
        })
    });
    let passed = p.findings.is_empty();
    let rest = json!({
        "input": {
            "file": a.file.display().to_string(),
            "kind": if is_zip { "zip" } else { "manifest" },
            "sha256": sha256,
            "provenance": if a.first_party { "first-party" } else { "community" },
            "foundation": a.foundation.as_ref().map(|p| p.display().to_string()),
            "previous": a.previous.as_ref().map(|p| p.display().to_string()),
        },
        "spec": {
            "repo": "mihailinl/AstraPlugins",
            "sha": crate::descriptor::SPEC_SHA,
            "section": "spec/games/manifest.md §4",
        },
        "integration": integration,
        "descriptor": descriptor,
        "negative_folders": folders,
        "link": link,
        "built_on": built_on,
        "code_changed": code_changed,
        "steps": p.steps,
        "notices": p.notices,
    });
    Ok((report("check", rest, &p.findings), passed))
}
