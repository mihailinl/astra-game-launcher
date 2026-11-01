// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe compat`: may this release reach Astra 0.2.7's users? Plan B §2's C2, its six steps
//! run on the real zip against launcher 0.3.1 (what 0.2.7 ships), plus Astra 0.2.7's own rules for
//! a source's releases (merged plan R30, canary K4):
//!
//! 1. launcher 0.3.1 opens the zip (`GiPackage::open`, which parses the manifest) — and so does
//!    this launcher, whose `open` is asked first: a zip it refuses as two (`zip-inconsistent`, as
//!    `check` does) fails here, and 0.3.1, whose zip 8 cannot tell, never reads it. A zip this
//!    launcher refuses for anything else that 0.3.1 opens fails here with this launcher's refusal
//!    (`launcher` = its version): 0.2.7 would install it, and the Astra after it would then
//!    refuse its Play (it parses the ledger's manifest again) and a reinstall. A placeholder this
//!    launcher reads and 0.3.1 refuses ([`NEWER_PLACEHOLDERS`]: `${astra_pack_inbox}`) is the
//!    finding `needs-newer-astra{min_astra}` only when this launcher opens the zip: the release
//!    needs that Astra, and must not reach 0.2.7's users. When both refuse it otherwise, the
//!    finding is 0.3.1's;
//! 2. 0.2.7's `check_manifest` passes for the source's row (its id and repository, a runner's
//!    `[target] engine` and no appid or a game's own appid, the tag's version), and so does
//!    `pick`'s rule for the tag and the asset: the row's tag prefix, an optional `v`, semver,
//!    stable unless `--prerelease`, and the asset named as the row's;
//! 3. 0.3.1's `plan_install` of the zip equals its plan of the same zip rebuilt without every
//!    `[integration.*]` table but `[integration.mods]` (the zip's digest aside), on a synthetic
//!    game that fits the row ([`crate::trees`]): what this launcher learns from those tables is
//!    nothing 0.3.1 acts on;
//! 4. 0.3.1 installs that plan, then prepares a launch with exactly the variables 0.2.7 passes,
//!    `{bridge_port, bridge_token}` (a new `${var}` fails here), and a Steam path;
//! 5. the files under every `**` `[[files]]` entry are the previous release's (`--previous`), or
//!    each one added or gone is named in the release notes (`--notes`); without `--previous` the
//!    step is skipped, and the report says so;
//! 6. what 0.2.7's `consent_needed_for` weighs is the previous release's: the integration id and
//!    repository, license, authors, `[target]`, `[launch]`, `[[config_writes]]`, and the game
//!    files of the plan that updates the previous release's install. A change is the notice
//!    `consent-sheet-expected` (0.2.7 will show its sheet again), never a finding.
//!
//! `pick`'s digest and canonical-URL halves need GitHub's published asset, so they are not
//! compat's; `gi-probe pick` runs them over a listing.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read as _, Write as _};
use std::path::{Path, PathBuf};

use launcher_031::{
    Accept, Ctx, GiPackage, InstallPlan, LauncherError, MANIFEST_FILE, Manifest, install,
    installed, plan_install, prepare_launch,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::v027::pins::{self, GiSource, SourceTarget};
use crate::v027::releases::{
    self, GhRelease, Gi, canonical_url, check_manifest, fallback_gi_with, pick_with, version_after,
};
use crate::v027::service::{consent_needed_for, same_repo};
use crate::{Fatal, Finding, Probe, Scratch, report, trees};

/// The port Astra 0.2.7 passes (`service.rs:36`, `DEFAULT_BRIDGE_PORT`).
const BRIDGE_PORT: &str = "25600";
/// A token of the shape 0.2.7 mints (64 hex digits).
const BRIDGE_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub struct CompatArgs {
    pub zip: PathBuf,
    pub source: String,
    pub tag: String,
    pub asset: Option<String>,
    pub sha256: Option<String>,
    pub prerelease: bool,
    pub previous: Option<PathBuf>,
    pub notes: Option<PathBuf>,
}

const STEPS: [&str; 6] = [
    "launcher 0.3.1 opens the zip and parses its manifest",
    "Astra 0.2.7's check_manifest, and pick's tag and asset rule, for the source",
    "0.3.1 plans the same with and without the [integration.*] tables",
    "0.3.1 installs, then prepares a launch with exactly {bridge_port, bridge_token}",
    "the files under every `**` are the previous release's, or named in the notes",
    "the consent sheet's inputs are the previous release's",
];

fn refusal(e: &LauncherError, detail: &str) -> Finding {
    Finding::from_refusal(e.code, &e.params, detail)
}

/// What this launcher reads that launcher 0.3.1 refuses as it parses the manifest
/// (`unknown-placeholder{name}`), and the first Astra whose launcher reads it: a release that
/// uses one cannot reach Astra 0.2.7's users, and a catalogue entry of it needs that `min_astra`.
pub const NEWER_PLACEHOLDERS: &[(&str, &str)] = &[("astra_pack_inbox", "0.2.8")];

/// The [`NEWER_PLACEHOLDERS`] row 0.3.1's refusal `e` is about, if it is one.
fn newer_placeholder(e: &LauncherError) -> Option<&'static (&'static str, &'static str)> {
    (e.code == launcher_031::codes::UNKNOWN_PLACEHOLDER)
        .then(|| e.params.get("name"))
        .flatten()
        .and_then(|n| NEWER_PLACEHOLDERS.iter().find(|(p, _)| p == n))
}

/// Step 1's finding for a zip THIS launcher refuses (not as two archives): its own refusal, with
/// `launcher` = its version.
fn by_this_launcher(e: &astra_game_launcher::LauncherError) -> Finding {
    let version = env!("CARGO_PKG_VERSION");
    Finding::from_refusal(
        e.code,
        &e.params,
        &format!(
            "launcher {version} (the Astra after 0.2.7) refuses this zip: {e}. Launcher 0.3.1 would install it, and the next Astra would then refuse its Play and a reinstall"
        ),
    )
    .with("launcher", version)
}

/// Step 1's finding for a zip 0.3.1 refuses and this launcher opens: `needs-newer-astra{min_astra,
/// placeholder, refused}` for one of [`NEWER_PLACEHOLDERS`] (what a release script reads to keep
/// the release from Astra 0.2.7's users), 0.3.1's own refusal otherwise.
fn step1_refusal(e: &LauncherError) -> Finding {
    match newer_placeholder(e) {
        Some((name, min)) => Finding::new(
            astra_game_launcher::codes::NEEDS_NEWER_ASTRA,
            format!(
                "launcher 0.3.1 (Astra 0.2.7) refuses ${{{name}}}, which Astra {min} reads: this release must not reach Astra 0.2.7's users, and its catalogue entry needs min_astra {min} or later"
            ),
        )
        .with("min_astra", *min)
        .with("placeholder", *name)
        .with("refused", e.code),
        None => refusal(e, "launcher 0.3.1 (Astra 0.2.7) refuses this zip"),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read(path: &Path) -> Result<Vec<u8>, Fatal> {
    std::fs::read(path).map_err(|e| Fatal::io(path, e))
}

fn row(id: &str) -> Result<GiSource, Fatal> {
    pins::source(id).ok_or_else(|| {
        let ids: Vec<&str> = pins::sources().iter().map(|s| s.id).collect();
        Fatal::usage(format!(
            "--source {id:?} is none of Astra 0.2.7's sources: {}",
            ids.join(", ")
        ))
    })
}

fn source_json(s: &GiSource) -> Value {
    let target = match s.target {
        SourceTarget::Runner { engine } => json!({ "runner": { "engine": engine } }),
        SourceTarget::Game { appid, exe } => json!({ "game": { "appid": appid, "exe": exe } }),
    };
    json!({
        "id": s.id,
        "repo": s.repo,
        "repo_id": s.repo_id,
        "tag_prefix": s.tag_prefix,
        "asset": s.asset,
        "target": target,
    })
}

fn against() -> Value {
    json!({ "astra": pins::astra(), "launcher": pins::launcher() })
}

/// `gi-probe compat`.
pub fn run(a: &CompatArgs) -> Result<(Value, bool), Fatal> {
    let source = row(&a.source)?;
    let zip = read(&a.zip)?;
    let previous = a.previous.as_deref().map(read).transpose()?;
    let notes = match &a.notes {
        Some(p) => Some(String::from_utf8_lossy(&read(p)?).into_owned()),
        None => None,
    };
    let asset = match &a.asset {
        Some(n) => n.clone(),
        None => a
            .zip
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned(),
    };
    let zip_sha256 = sha256_hex(&zip);
    let scratch = Scratch::new("compat")?;
    let mut p = Probe::new(&STEPS);

    // 1. Open and parse. First, the one question 0.3.1's zip 8 cannot ask: a zip that reads as
    // two archives (`check`'s step 1 refusal, `src/zip_consistency.rs`) is judged no further, so
    // compat never passes a release under one reading that a reader of local headers sees as
    // another.
    let ours = astra_game_launcher::GiPackage::open(&zip, &zip_sha256).err();
    if let Some(e) = &ours
        && e.code == astra_game_launcher::codes::ZIP_INCONSISTENT
    {
        p.finding(1, Finding::from_refusal(e.code, &e.params, &e.to_string()));
        p.close(1, 0, 0, Value::Null);
        for step in 2..=6 {
            p.skip(
                step,
                "the zip reads as two archives (step 1): nothing in it is read",
            );
        }
        return Ok(finish(a, &source, &asset, &zip_sha256, None, p));
    }
    let pkg = match (
        GiPackage::open(&zip, a.sha256.as_deref().unwrap_or(&zip_sha256)),
        &ours,
    ) {
        (Ok(pkg), None) => {
            p.close(1, 0, 0, Value::Null);
            Some(pkg)
        }
        // 0.3.1 installs what this launcher refuses (an `[integration.*]` table 0.3.1 reads past,
        // a destination it never sees): this launcher's refusal, or the release reaches 0.2.7.
        (Ok(_), Some(ours)) => {
            p.finding(1, by_this_launcher(ours));
            p.close(1, 0, 0, Value::Null);
            None
        }
        (Err(e), ours) => {
            // `needs-newer-astra` says the next Astra reads it: never of a zip this one refuses.
            let finding = match ours {
                Some(ours) if newer_placeholder(&e).is_some() => by_this_launcher(ours),
                _ => step1_refusal(&e),
            };
            p.finding(1, finding);
            p.close(1, 0, 0, Value::Null);
            None
        }
    };
    let Some(pkg) = pkg else {
        for step in 2..=6 {
            p.skip(step, "step 1 failed");
        }
        return Ok(finish(a, &source, &asset, &zip_sha256, None, p));
    };
    let m = pkg.manifest().clone();

    // 2. The source's rules.
    let (f0, n0) = (p.findings.len(), p.notices.len());
    step_source(&mut p, a, &source, &asset, &zip_sha256, &m);
    p.close(2, f0, n0, Value::Null);

    // 3 and 4. The same plan without the [integration.*] tables; install and launch.
    let (f0, n0) = (p.findings.len(), p.notices.len());
    let tree_root = scratch.path().join("game");
    let laid = trees::lay(&tree_root, &source, &m).map_err(|e| Fatal::io(&tree_root, e))?;
    let profile = scratch.path().join("profile");
    let (plan, stripped) = step_inert(&mut p, &zip, &pkg, &laid.game, &profile);
    p.close(
        3,
        f0,
        n0,
        json!({
            "game": {
                "exe": trees::exe_for(&source, &m),
                "engine": trees::engine_for(&source, &m),
                "steam_appid": trees::appid_for(&source),
            },
            "stripped": stripped,
        }),
    );
    match plan {
        Some(plan) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            step_launch(&mut p, &pkg, &plan, &laid, &profile);
            p.close(4, f0, n0, Value::Null);
        }
        None => p.skip(4, "step 3 has no plan to install"),
    }

    // 5 and 6. Against the previous release.
    match &previous {
        None => {
            p.skip(5, "no --previous release");
            p.skip(6, "no --previous release");
        }
        Some(prev) => {
            let (f0, n0) = (p.findings.len(), p.notices.len());
            let counts = step_swept(&mut p, &zip, prev, notes.as_deref());
            p.close(5, f0, n0, counts);
            step_consent(&mut p, prev, &pkg, &source, &m, &scratch);
        }
    }

    let integration = json!({
        "id": m.integration.id,
        "version": m.integration.version,
        "repo": m.integration.repo,
    });
    Ok(finish(
        a,
        &source,
        &asset,
        &zip_sha256,
        Some(integration),
        p,
    ))
}

fn finish(
    a: &CompatArgs,
    source: &GiSource,
    asset: &str,
    zip_sha256: &str,
    integration: Option<Value>,
    p: Probe,
) -> (Value, bool) {
    let passed = p.findings.is_empty();
    let rest = json!({
        "input": {
            "zip": a.zip.display().to_string(),
            "zip_sha256": zip_sha256,
            "digest_checked": a.sha256.is_some(),
            "tag": a.tag,
            "asset": asset,
            "prerelease": a.prerelease,
            "previous": a.previous.as_ref().map(|p| p.display().to_string()),
            "notes": a.notes.as_ref().map(|p| p.display().to_string()),
        },
        "source": source_json(source),
        "against": against(),
        "integration": integration,
        "steps": p.steps,
        "notices": p.notices,
    });
    (report("compat", rest, &p.findings), passed)
}

/// Step 2: `check_manifest` for the row, and `pick`'s tag and asset rule.
fn step_source(
    p: &mut Probe,
    a: &CompatArgs,
    source: &GiSource,
    asset: &str,
    zip_sha256: &str,
    m: &Manifest,
) {
    if asset != source.asset {
        p.finding(
            2,
            Finding::new(
                "asset-mismatch",
                format!(
                    "0.2.7 installs the release asset {:?} for this source, never {asset:?}",
                    source.asset
                ),
            )
            .field("asset")
            .with("want", source.asset)
            .with("got", asset),
        );
    }
    let Some(version) = version_after(&a.tag, source.tag_prefix, a.prerelease) else {
        let why = if version_after(&a.tag, source.tag_prefix, true).is_some() {
            "a semver pre-release, which 0.2.7 never picks from a stable release (use --prerelease for a GitHub prerelease)"
        } else {
            "not the source's tag prefix, an optional `v`, then a semver version"
        };
        p.finding(
            2,
            Finding::new("tag-not-followed", format!("the tag {:?} is {why}", a.tag))
                .field("tag")
                .with("tag", a.tag.clone())
                .with("tag_prefix", source.tag_prefix),
        );
        return;
    };
    if a.prerelease {
        p.notice(
            2,
            Finding::new(
                "prerelease",
                "a 0.2.7 release build never installs a prerelease; only its owner test build does",
            ),
        );
    }
    let gi = Gi {
        source: *source,
        tag: a.tag.clone(),
        version: version.to_string(),
        sha256: zip_sha256.to_owned(),
        url: canonical_url(source.repo, &a.tag, source.asset),
    };
    // A runner's check reads no appid; a game's own is the game it installs on.
    let appid = trees::appid_for(source).to_string();
    if let Err(e) = check_manifest(&gi, &appid, m) {
        p.finding(
            2,
            refusal(
                &e,
                "Astra 0.2.7 refuses this release for its source (releases::check_manifest)",
            ),
        );
    }
}

/// Step 3: the plan with and without the `[integration.*]` tables. Returns the zip's plan, for
/// step 4, and the tables it took out.
fn step_inert(
    p: &mut Probe,
    zip: &[u8],
    pkg: &GiPackage,
    game: &launcher_031::GameTarget,
    profile: &Path,
) -> (Option<InstallPlan>, Vec<String>) {
    let (bare, stripped) = match without_integration_tables(zip) {
        Ok(v) => v,
        Err(e) => {
            p.finding(3, Finding::new("zip-unrebuildable", e));
            return (None, Vec::new());
        }
    };
    let plan = plan_install(pkg, game, profile);
    let bare_plan = GiPackage::open(&bare, &sha256_hex(&bare))
        .and_then(|bare| plan_install(&bare, game, profile));
    match (plan, bare_plan) {
        (Ok(plan), Ok(bare_plan)) => {
            let differs = plan_differences(&plan, &bare_plan);
            if !differs.is_empty() {
                p.finding(
                    3,
                    Finding::new(
                        "integration-tables-not-inert",
                        format!(
                            "launcher 0.3.1 plans this zip differently once its [integration.*] tables are taken out: {}",
                            differs.join(", ")
                        ),
                    )
                    .field(differs.join(",")),
                );
            }
            (Some(plan), stripped)
        }
        (Err(e), _) => {
            p.finding(
                3,
                refusal(
                    &e,
                    "launcher 0.3.1 refuses to plan this release on a game its source fits",
                ),
            );
            (None, stripped)
        }
        (Ok(plan), Err(e)) => {
            p.finding(
                3,
                refusal(
                    &e,
                    "launcher 0.3.1 plans this zip, and refuses it once its [integration.*] tables are taken out",
                )
                .field("integration"),
            );
            (Some(plan), stripped)
        }
    }
}

/// The top-level fields in which two plans differ, the zip's digest aside.
pub fn plan_differences(a: &InstallPlan, b: &InstallPlan) -> Vec<String> {
    let mut b = b.clone();
    b.source_sha256 = a.source_sha256.clone();
    if *a == b {
        return Vec::new();
    }
    let (Value::Object(a), Value::Object(b)) = (
        serde_json::to_value(a).expect("a plan serialises"),
        serde_json::to_value(&b).expect("a plan serialises"),
    ) else {
        return vec!["plan".to_owned()];
    };
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let fields: Vec<String> = keys
        .into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect();
    if fields.is_empty() {
        vec!["plan".to_owned()]
    } else {
        fields
    }
}

/// `zip` with its manifest rewritten without every `[integration.*]` table (a table, or an array
/// of tables, below `[integration]`) but `[integration.mods]`; every other entry is copied as it
/// is, compressed bytes and all. Returns the zip and the tables taken out.
pub fn without_integration_tables(zip: &[u8]) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut ar = ZipArchive::new(Cursor::new(zip)).map_err(|e| e.to_string())?;
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    let mut stripped = Vec::new();
    for i in 0..ar.len() {
        let name = ar
            .by_index_raw(i)
            .map_err(|e| e.to_string())?
            .name()
            .map_err(|e| e.to_string())?
            .into_owned();
        if name.replace('\\', "/") == MANIFEST_FILE {
            let mut text = String::new();
            ar.by_index(i)
                .map_err(|e| e.to_string())?
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            let (bare, taken) = strip_manifest(&text)?;
            stripped = taken;
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            w.start_file(name, opts).map_err(|e| e.to_string())?;
            w.write_all(bare.as_bytes()).map_err(|e| e.to_string())?;
        } else {
            let f = ar.by_index_raw(i).map_err(|e| e.to_string())?;
            w.raw_copy_file(f).map_err(|e| e.to_string())?;
        }
    }
    let out = w.finish().map_err(|e| e.to_string())?.into_inner();
    Ok((out, stripped))
}

/// The manifest without its `[integration.*]` tables but `[integration.mods]`.
pub fn strip_manifest(text: &str) -> Result<(String, Vec<String>), String> {
    let mut t: toml::Table = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    let mut taken = Vec::new();
    if let Some(toml::Value::Table(i)) = t.get_mut("integration") {
        let keys: Vec<String> = i
            .iter()
            .filter(|(k, v)| *k != "mods" && is_table_like(v))
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            i.remove(&k);
            taken.push(format!("integration.{k}"));
        }
    }
    let text = toml::to_string(&t).map_err(|e| e.to_string())?;
    Ok((text, taken))
}

fn is_table_like(v: &toml::Value) -> bool {
    match v {
        toml::Value::Table(_) => true,
        toml::Value::Array(a) => !a.is_empty() && a.iter().all(|e| e.is_table()),
        _ => false,
    }
}

fn launch_vars() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("bridge_port".to_owned(), BRIDGE_PORT.to_owned()),
        ("bridge_token".to_owned(), BRIDGE_TOKEN.to_owned()),
    ])
}

/// Step 4: install the plan, then prepare a launch as 0.2.7 does.
fn step_launch(
    p: &mut Probe,
    pkg: &GiPackage,
    plan: &InstallPlan,
    laid: &trees::Tree,
    profile: &Path,
) {
    // The VAC flag of a synthetic game is unread; 0.2.7 tells the person and installs.
    let accept = Accept {
        vac_unchecked: true,
    };
    if let Err(e) = install(pkg, plan, &laid.game, accept, &Ctx::silent()) {
        p.finding(
            4,
            refusal(&e, "launcher 0.3.1 refuses to install the plan it made").field("install"),
        );
        return;
    }
    if let Err(e) = prepare_launch(profile, &laid.game, &launch_vars(), Some(&laid.steam)) {
        p.finding(
            4,
            refusal(
                &e,
                "launcher 0.3.1 refuses to launch it with the variables Astra 0.2.7 passes ({bridge_port, bridge_token})",
            )
            .field("prepare_launch"),
        );
    }
}

/// Every file a `**` `[[files]]` entry puts into the profile: its path there → its zip entry.
pub fn swept_files(zip: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let mut ar = ZipArchive::new(Cursor::new(zip)).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    let mut manifest = None;
    for i in 0..ar.len() {
        let f = ar.by_index_raw(i).map_err(|e| e.to_string())?;
        let name = f.name().map_err(|e| e.to_string())?.replace('\\', "/");
        if f.is_dir() || name.ends_with('/') {
            continue;
        }
        if name == MANIFEST_FILE {
            manifest = Some(i);
        } else {
            entries.push(name);
        }
    }
    let Some(mi) = manifest else {
        return Err(format!("no {MANIFEST_FILE}"));
    };
    let mut text = String::new();
    ar.by_index(mi)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let t: toml::Table = toml::from_str(&text).map_err(|e| e.message().to_owned())?;
    let mut out = BTreeMap::new();
    let Some(toml::Value::Array(files)) = t.get("files") else {
        return Ok(out);
    };
    for f in files {
        let (Some(from), Some(to)) = (
            f.get("from").and_then(toml::Value::as_str),
            f.get("to").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        let prefix = if from == "**" {
            String::new()
        } else if let Some(dir) = from.strip_suffix("/**") {
            format!("{dir}/")
        } else {
            continue;
        };
        let base = to
            .strip_prefix("${profile}")
            .unwrap_or(to)
            .trim_matches('/')
            .to_owned();
        for e in &entries {
            if let Some(rest) = e.strip_prefix(prefix.as_str()) {
                let dest = if base.is_empty() {
                    rest.to_owned()
                } else {
                    format!("{base}/{rest}")
                };
                out.insert(dest, e.clone());
            }
        }
    }
    Ok(out)
}

/// Whether `notes` names `path` as a whole path (not inside a longer name).
pub fn named_in(notes: &str, path: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
    notes.match_indices(path).any(|(i, _)| {
        let before = notes[..i].chars().next_back();
        let after = notes[i + path.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(|c| part(c) || c == '/')
    })
}

/// Step 5: the swept files against the previous release's.
fn step_swept(p: &mut Probe, zip: &[u8], previous: &[u8], notes: Option<&str>) -> Value {
    let (now, before) = match (swept_files(zip), swept_files(previous)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            p.finding(5, Finding::new("swept-files-unreadable", e));
            return Value::Null;
        }
    };
    let mut added = 0;
    let mut removed = 0;
    let mut changes: Vec<(&String, &String, &str)> = Vec::new();
    for (dest, entry) in &now {
        if !before.contains_key(dest) {
            added += 1;
            changes.push((dest, entry, "added"));
        }
    }
    for (dest, entry) in &before {
        if !now.contains_key(dest) {
            removed += 1;
            changes.push((dest, entry, "removed"));
        }
    }
    let mut named = 0;
    for (dest, entry, how) in changes {
        let is_named = notes.is_some_and(|n| named_in(n, dest) || named_in(n, entry));
        if is_named {
            named += 1;
            continue;
        }
        p.finding(
            5,
            Finding::new(
                "swept-files-changed",
                format!(
                    "${{profile}}/{dest} is {how} under a `**` [[files]] entry since the previous release, and the release notes do not name it"
                ),
            )
            .field(dest.clone())
            .with("change", how),
        );
    }
    json!({ "swept": now.len(), "added": added, "removed": removed, "named_in_notes": named })
}

/// Step 6: what `consent_needed_for` weighs, against the previous release's install.
fn step_consent(
    p: &mut Probe,
    previous: &[u8],
    pkg: &GiPackage,
    source: &GiSource,
    m: &Manifest,
    scratch: &Scratch,
) {
    let (f0, n0) = (p.findings.len(), p.notices.len());
    let prev = match GiPackage::open(previous, &sha256_hex(previous)) {
        Ok(prev) => prev,
        Err(e) => {
            p.notice(
                6,
                refusal(
                    &e,
                    "launcher 0.3.1 cannot open the previous release, so no install of it can be updated",
                )
                .field("previous"),
            );
            p.skip(6, "the previous release does not open");
            return;
        }
    };
    let root = scratch.path().join("update");
    let laid = match trees::lay(&root, source, prev.manifest()) {
        Ok(t) => t,
        Err(e) => {
            p.finding(6, Finding::new("io", e.to_string()));
            p.close(6, f0, n0, Value::Null);
            return;
        }
    };
    let profile = scratch.path().join("update-profile");
    let accept = Accept {
        vac_unchecked: true,
    };
    let before = plan_install(&prev, &laid.game, &profile)
        .and_then(|plan| install(&prev, &plan, &laid.game, accept, &Ctx::silent()));
    let ledger = match before.and_then(|_| installed(&profile)) {
        Ok(Some(l)) => l,
        Ok(None) | Err(_) => {
            p.notice(
                6,
                Finding::new(
                    "previous-not-installable",
                    "launcher 0.3.1 does not install the previous release on the game this source fits, so there is no install to update",
                )
                .field("previous"),
            );
            p.skip(6, "the previous release does not install");
            return;
        }
    };
    let mut update = match plan_install(pkg, &laid.game, &profile) {
        Ok(plan) => plan,
        Err(e) => {
            p.finding(
                6,
                refusal(
                    &e,
                    "launcher 0.3.1 refuses to update the previous release's install to this one",
                )
                .field("update"),
            );
            p.close(6, f0, n0, Value::Null);
            return;
        }
    };
    // The synthetic game's VAC flag is unread, and 0.2.7 always asks then. This step asks
    // whether the RELEASE changes what the sheet shows, so it weighs a read and clear flag.
    update.vac_checked = true;
    let mut changed = Vec::new();
    if consent_needed_for(Some(&ledger), &update, m) {
        changed = consent_changes(&ledger, &update, m);
        p.notice(
            6,
            Finding::new(
                "consent-sheet-expected",
                format!(
                    "Astra 0.2.7 will show the consent sheet again for this update: {}",
                    if changed.is_empty() {
                        "its inputs changed".to_owned()
                    } else {
                        changed.join(", ")
                    }
                ),
            )
            .with("changed", changed.join(",")),
        );
    }
    p.close(6, f0, n0, json!({ "changed": changed }));
}

/// Which of `consent_needed_for`'s inputs differ, for the notice (the replica decides).
fn consent_changes(
    ledger: &launcher_031::Ledger,
    plan: &InstallPlan,
    new: &Manifest,
) -> Vec<String> {
    let mut out = Vec::new();
    let old = Manifest::parse(&ledger.manifest).ok();
    if ledger.integration.id != new.integration.id {
        out.push("integration.id");
    }
    if !same_repo(
        ledger.integration.repo.as_deref(),
        new.integration.repo.as_deref(),
    ) {
        out.push("integration.repo");
    }
    if ledger.integration.license != new.integration.license {
        out.push("integration.license");
    }
    if ledger.integration.authors != new.integration.authors {
        out.push("integration.authors");
    }
    if !crate::v027::service::game_files_match(&ledger.game_files, &plan.game_files) {
        out.push("game_files");
    }
    match &old {
        None => out.push("manifest"),
        Some(old) => {
            if old.target != new.target {
                out.push("target");
            }
            if old.launch != new.launch {
                out.push("launch");
            }
            if old.config_writes != new.config_writes {
                out.push("config_writes");
            }
        }
    }
    out.into_iter().map(str::to_owned).collect()
}

/// `gi-probe pick`: 0.2.7's choice out of a listing, and the source's fallback pin.
pub fn pick(id: &str, listing: &Path, prerelease: bool) -> Result<(Value, bool), Fatal> {
    let source = row(id)?;
    let bytes = read(listing)?;
    let releases: Vec<GhRelease> = serde_json::from_slice(&bytes)
        .map_err(|e| Fatal::io(listing, format!("not a releases listing: {e}")))?;
    let gi_json =
        |g: &Gi| json!({ "tag": g.tag, "version": g.version, "sha256": g.sha256, "url": g.url });
    let mut findings = Vec::new();
    let release = match pick_with(&releases, &source, prerelease) {
        Ok(g) => Some(gi_json(&g)),
        Err(e) => {
            let err = e.error();
            findings.push(refusal(
                &err,
                "Astra 0.2.7 installs no release of this source from this listing",
            ));
            None
        }
    };
    let fallback = fallback_gi_with(&source, true).map(|g| {
        let mut v = gi_json(&g);
        v["prerelease"] = json!(source.fallback.is_some_and(|f| f.prerelease));
        v
    });
    let passed = findings.is_empty();
    let rest = json!({
        "source": source_json(&source),
        "prerelease": prerelease,
        "releases_listed": releases.len(),
        "release": release,
        "fallback": fallback,
        "follows_prereleases_in_release_builds": releases::FOLLOWS_PRERELEASES,
    });
    Ok((report("pick", rest, &findings), passed))
}
