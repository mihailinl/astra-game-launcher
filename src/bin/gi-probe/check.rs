// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe check`: may this release be published? What a catalogue's ingest asks of every
//! release before it is listed (merged plan §3.2 lane L, A§5.2 step 5; AstraPlugins
//! `spec/games/manifest.md` §4, at [`crate::descriptor::SPEC_SHA`]), with this launcher's own
//! parser — there is one, and the launcher installs with it:
//!
//! 1. the launcher opens the zip ([`GiPackage::open`]: one archive to every reader, its hygiene
//!    under the default [`Limits`], the manifest, every file it names in the zip, no reserved
//!    profile name placed, a `**` expansion included). Given a bare `astra-gi.toml` instead of a
//!    zip, only [`Manifest::parse`]; the zip's checks are said not to be made. A zip refused as
//!    two archives (`zip-inconsistent`) is read no further: every later step that would read it
//!    is skipped, and says so. The manifest is TOML 1.0, as the specification parses it
//!    (`manifest.md` §2.6.1): one the launcher reads that uses TOML 1.1 or starts with a
//!    byte-order mark is `manifest-invalid{reason=toml-1.0}` ([`crate::toml10`]), and the
//!    report's digests (`link.digest`, `descriptor.sha256`) `null`;
//! 2. [`Manifest::lint`]: a community integration's identity (`reserved-id`, `reserved-name`),
//!    `capability-undeclared`, Astra Link v1's counts, names and lengths (`link-invalid`), the
//!    link's consent digest (`link.digest`: [`astra_game_launcher::link_digest`], spec
//!    `manifest.md` §2.6.1 at [`crate::descriptor::SPEC_SHA`], a raw CRLF read as LF as it
//!    says; a table it cannot digest is
//!    `link-invalid{reason=not-digestable}`, and the digest `null`), the foundation claim's keys,
//!    `[integration.engine]` and Pass B's admission of it, and everything wrong inside
//!    `[integration.engine.mods]` (`mods-invalid{field, reason}`, spec
//!    `engine-descriptor-v1.md` §7.9: refused here as at ingest, dropped alone by a client) —
//!    with `--provenance first-party`, Astra's own ids and names and the first-party grammar are
//!    allowed. When the manifest does not parse, `[integration.engine]` is still asked;
//! 3. every file `[integration.link]` names is in the zip, a `params`/`data` schema at most
//!    4 KiB and the skill at most 16 KiB. Whether each schema keeps to `link-v1.md` §6's
//!    restricted profile waits for `link-v1` to freeze; the report's `link` says so;
//! 4. the descriptor claims none of the spec's negative folders (`validate-descriptor`'s step 3),
//!    whatever its `mods` holds;
//! 5. `built_on{id, version, verified}`: the foundation (`--foundation`, a release zip) as this
//!    release is built on it — `verified` only when every file the foundation places, this one
//!    places alike (destination and digest; A§4.4 M3, the full set). A report, never a finding:
//!    an unverified claim earns no badge. A claim (`[integration.foundation]`) that names another
//!    foundation is a notice;
//! 6. `code_changed`: every placed file that is code ([`is_code`]), from `[[files]]` or
//!    `[[game_files]]`, new or changed against the previous release (`--previous`) — A§4.2's rule
//!    read the stricter way, game files included. A report: what the update sheet shows;
//! 7. `loaders`: each `[[game_files]]` entry classified against the catalogue's `loaders[]`
//!    allowlist (`--loaders LOADERS.json`, `[{name, sha256, default_enabled}]`; spec
//!    `games-index-v1.md` §2.4) — `none` (no name a loader hijack-loads by: neither
//!    [`HIJACK_NAMES`] nor the manifest's `[integration.loader] proxies`), `allowlisted` (such a
//!    name whose digest the allowlist lists with `default_enabled` false), or `other` (a notice,
//!    `loader-other`: the red sheet line and the red moderator line). Labels only (merged plan
//!    §2.5): the allowlist never permits what the manifest does not declare, and an undeclared
//!    loader is step 2's `loader-undeclared` (K18);
//! 8. `content`: the release's own animations, characters, art and audio against
//!    `[[integration.content]]` ([`GiPackage::content`], spec `manifest.md` §2.7, the codes the
//!    catalogue's content check raises before gi-probe, in this launcher's spelling): every
//!    content-typed file one declared part (`content-undeclared`, `content-missing`; a path
//!    declared twice is step 1's `content-duplicate`; a model in a game's own archive is
//!    `<archive>!<inner>`), every file judged by its bytes and every GLB and `.gltf` whole,
//!    within the caps and self-contained (`content-format`), a game's archives opened one level
//!    deep (two readings refused with the content catalogue's code; one it cannot open is the
//!    notice `content-archive-unscanned`), each `.astrachar` and `.astrapack` judged by the
//!    content catalogue's own code (its codes), the VRM licence gate (`content-licence-refused`,
//!    `content-licence-conflict`; an unknown meta is the notice `content-vrm-meta-unknown`), and
//!    a licence for every part (`content-licence-undeclared`). The report's `content.parts` is
//!    what a catalogue entry's
//!    `content[]` carries, member for member (`games-index-v1.schema.json` `contentPart`), and
//!    `content.classes` what each glTF part's extensions make it (`avatar`, `animation`,
//!    `plain`), which `content[]` has no member for.
//!
//! Step 2 holds a community release to what a community integration may place and launch
//! ([`Manifest::lint`]: `loader-undeclared`, `community-limit` for a `[[game_files]]` folder, `[launch]
//! env`, overrides and path arguments); the limit that needs a game folder (the program a walked
//! one) is planning's alone.
//!
//! Exit 0: no findings; 1: findings. `--provenance` defaults to `community`.

use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    GiPackage, HIJACK_NAMES, LauncherError, Limits, MANIFEST_FILE, Manifest, PlacedFile, codes,
    is_hijack_name, link_digest, link_limits,
};
use serde_json::{Value, json};
use zip::ZipArchive;

use crate::compat::sha256_hex;
use crate::descriptor::{negative_folders, summary};
use crate::{Fatal, Finding, Probe, report, toml10};

const STEPS: [&str; 8] = [
    "the launcher opens the zip and parses its manifest",
    "identity, capabilities, what a community release may place and launch, Astra Link's limits, the foundation claim and the engine descriptor",
    "every file [integration.link] names is in the zip, within its size",
    "the engine descriptor claims none of the spec's negative folders",
    "built on the foundation: every file it places, placed alike",
    "code changed since the previous release",
    "the loader each game file is: none, allowlisted or other",
    "every animation, model, picture and sound is one declared part, whole, with a licence the VRM gate allows",
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
    /// The catalogue's `loaders[]` (`games-index-v1.md` §2.4), as a JSON array.
    pub loaders: Option<PathBuf>,
}

/// One `loaders[]` entry: a known loader build, by digest.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowedLoader {
    name: String,
    sha256: String,
    default_enabled: bool,
}

/// `--loaders`: a JSON array of `{name, sha256, default_enabled}`; anything else is a wrong input
/// (exit 2), never an empty allowlist.
fn read_loaders(path: &Path) -> Result<Vec<AllowedLoader>, Fatal> {
    let bytes = read(path)?;
    let list: Vec<AllowedLoader> =
        serde_json::from_slice(&bytes).map_err(|e| Fatal::io(path, format!("loaders[]: {e}")))?;
    for l in &list {
        if l.sha256.len() != 64 || !l.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Fatal::io(
                path,
                format!("loaders[] {:?}: sha256 is not 64 hex digits", l.name),
            ));
        }
    }
    Ok(list)
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
            && let Ok(name) = f.name()
        {
            out.insert(name.replace('\\', "/"), f.size());
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
            // A zip two readers read as two archives: nothing in it is read, not even for the
            // lint, which would judge one of the two.
            Err(e) if e.code == codes::ZIP_INCONSISTENT => {
                p.finding(1, refusal(&e));
                open_error = Some(e);
                (None, None)
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
    // TOML 1.0, as the specification parses the manifest (`manifest.md` §2.6.1 step 1): a text
    // the launcher reads and ingest does not (`toml10`) is no release, and has no digest there.
    let not_toml_1_0 = text.as_deref().and_then(toml10::refusal).map(|detail| {
        LauncherError::new(codes::MANIFEST_INVALID)
            .with("key", "")
            .with("reason", "toml-1.0")
            .with("detail", detail)
    });
    if let Some(e) = &not_toml_1_0 {
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
    // Every later step that would read the zip says why it does not.
    const TWO_ARCHIVES: &str = "the zip reads as two archives (step 1): nothing in it is read";
    let two_archives = open_error
        .as_ref()
        .is_some_and(|e| e.code == codes::ZIP_INCONSISTENT);
    let unread = |why: &'static str| if two_archives { TWO_ARCHIVES } else { why };

    // `[integration.link]`'s consent digest (spec `manifest.md` §2.6.1): the report's
    // `link.digest`. A table it cannot digest is step 2's finding: never a release without one.
    // None of a text that is not TOML 1.0: ingest computes none.
    let digest = match &not_toml_1_0 {
        Some(_) => None,
        None => manifest.as_ref().map(|m| link_digest(m.link())),
    };

    // 2. Lint.
    match &parsed {
        None => p.skip(2, unread("the zip holds no readable astra-gi.toml")),
        Some((_, lint)) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            for e in lint {
                p.finding(2, refusal(e));
            }
            if let Some(Err(e)) = &digest {
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
            "digest": match &digest {
                Some(Ok(Some(d))) => Value::String(d.clone()),
                _ => Value::Null,
            },
            "files": link_files.len(),
            "schemas_validated": false,
            "why": "validating params/data schemas against link-v1.md §6's restricted profile waits for link-v1 to freeze",
        });
    }
    match (&manifest, is_zip) {
        (None, _) => p.skip(3, unread("the manifest does not parse")),
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
        None => p.skip(4, unread("no [integration.engine] the launcher reads")),
        Some(Err(_)) => p.skip(4, "[integration.engine] is not a descriptor (step 2)"),
        Some(Ok(d)) => {
            descriptor = summary(d);
            // Not TOML 1.0: ingest has no descriptor, so no digest, of it.
            if not_toml_1_0.is_some() {
                descriptor["sha256"] = Value::Null;
            }
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
        (Some(_), None) => p.skip(5, unread("the release does not open (step 1)")),
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
        (Some(_), None) => p.skip(6, unread("the release does not open (step 1)")),
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

    // 7. Loaders.
    let allowlist = a.loaders.as_deref().map(read_loaders).transpose()?;
    let mut loaders = Value::Null;
    match &manifest {
        None => p.skip(7, unread("the manifest does not parse")),
        Some(m) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            // Each game file and the digest it is known by: the zip's, else the manifest's pin.
            let files: Vec<(String, Option<String>)> = match &pkg {
                Some(pkg) => pkg
                    .placed_files()
                    .into_iter()
                    .filter(|f| f.dest.starts_with("${game}/"))
                    .map(|f| (f.dest, Some(f.sha256)))
                    .collect(),
                None => m
                    .game_files
                    .iter()
                    .map(|g| {
                        (
                            g.to.clone(),
                            g.sha256.as_ref().map(|h| h.to_ascii_lowercase()),
                        )
                    })
                    .collect(),
            };
            let declared = |name: &str| {
                m.loader()
                    .is_some_and(|l| l.proxies.iter().any(|p| p.eq_ignore_ascii_case(name)))
            };
            let mut rows = Vec::new();
            for (dest, sha256) in files {
                let name = dest.rsplit('/').next().unwrap_or(&dest).to_owned();
                let listed = allowlist.iter().flatten().find(|l| {
                    sha256
                        .as_deref()
                        .is_some_and(|h| l.sha256.eq_ignore_ascii_case(h))
                });
                let class = if !is_hijack_name(&name) && !declared(&name) {
                    "none"
                } else if listed.is_some_and(|l| !l.default_enabled) {
                    "allowlisted"
                } else {
                    "other"
                };
                if class == "other" {
                    p.notice(
                        7,
                        Finding::new(
                            "loader-other",
                            format!(
                                "{dest} is a name mod loaders hijack-load by, and no loader the allowlist knows as off by default: it runs every time the game starts, even without Astra"
                            ),
                        )
                        .field(dest.clone()),
                    );
                }
                rows.push(json!({
                    "dest": dest,
                    "sha256": sha256,
                    "class": class,
                    "allowlisted_as": listed.filter(|_| class == "allowlisted").map(|l| l.name.clone()),
                }));
            }
            loaders = json!({
                "allowlist": a.loaders.as_ref().map(|p| p.display().to_string()),
                "allowlist_entries": allowlist.as_ref().map(Vec::len),
                "hijack_names": HIJACK_NAMES,
                "files": rows,
            });
            p.close(7, f0, n0, Value::Null);
        }
    }

    // 8. Content.
    let mut content = Value::Null;
    match &pkg {
        None if !is_zip => p.skip(
            8,
            "a manifest, not a zip: the content check reads the release's files",
        ),
        None => p.skip(8, unread("the release does not open (step 1)")),
        Some(pkg) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            let r = pkg
                .content()
                .map_err(|e| Fatal::io(&a.file, e.to_string()))?;
            for e in &r.refusals {
                p.finding(8, refusal(e));
            }
            for f in &r.flags {
                let detail = match f.code {
                    astra_game_launcher::content::FLAG_VRM_META_UNKNOWN => {
                        "the VRM licence gate cannot read this part's licence (no VRM meta, or a token it does not know): it passes with the licence its entry declares, flagged"
                    }
                    _ => {
                        "this archive could not be opened (zip64, too large, encrypted, another method, or an archive inside an archive), so nothing inside it was judged: it publishes flagged, for a moderator"
                    }
                };
                p.notice(8, Finding::from_refusal(f.code, &f.params, detail));
            }
            content = json!({
                "content_files": r.content_files,
                "parts": r.parts,
                "classes": r.classes(),
            });
            p.close(8, f0, n0, Value::Null);
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
            "loaders": a.loaders.as_ref().map(|p| p.display().to_string()),
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
        "loaders": loaders,
        "content": content,
        "steps": p.steps,
        "notices": p.notices,
    });
    Ok((report("check", rest, &p.findings), passed))
}
