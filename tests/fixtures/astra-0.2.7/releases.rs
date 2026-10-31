//! The release a GI installs: the newest stable GitHub release of its source's repository
//! ([`super::pins::GiSource`]), for EVERY GI — the three universal runners and every game's own
//! integration alike.
//!
//! **The owner's decision, 2026-10-17:** "for now, lets astra just look at release, and take last
//! release, dont need signing, registry, because currently repositories hardcoded, and only my
//! repositories, so dont need to sign." So a new release reaches users through the Games tab's
//! Install and Update with no Astra release and no repin. The compiled pins
//! ([`super::pins::FallbackPin`]) are only the OFFLINE FALLBACK. A signed games registry replaces
//! this in 0.2.8.
//!
//! **What is verified, and what is not** — the whole trust story of a GI:
//! - VERIFIED: the zip is the bytes GitHub stores for that release asset. The asset's `digest`
//!   (the SHA-256 GitHub computes when the asset is uploaded and reports through its API) is the
//!   digest `fetch_verified` and `GiPackage::open` check. The listing is asked by the repository's
//!   numeric id, and the download URL is the canonical
//!   `https://github.com/<repo>/releases/download/<tag>/<asset>` of the COMPILED repository, never
//!   whatever URL the API returned ([`pick`]). The package's manifest must carry the source's
//!   integration id and repository and the version its tag carries; a runner's must name the
//!   runner's `[target] engine` and NO appid, a game's own integration THIS game's Steam appid
//!   ([`check_manifest`]). Then everything the launcher checks for every GI: the path jail,
//!   `[target]` exe/appid/engine/platform, every `[[game_files]]` digest the manifest pins, foreign
//!   loaders, anti-cheat (including Steam's VAC flag).
//! - NOT VERIFIED: who published the release. Nothing in this binary pins the bytes of a followed
//!   release, so anyone who can publish a STABLE release in one of these eight repositories ships
//!   it to every user: at once to a fresh Install, and to every installed game on its next listing
//!   refresh (at most [`LISTING_TTL`] later), through the Update button. Nothing on Astra's side
//!   can withdraw such a release until the signed registry (0.2.8): deleting it on GitHub stops
//!   new installs once the listing refreshes, but an installed copy stays (an older "newest" is
//!   refused as [`RELEASE_OLDER`], never installed as a silent downgrade). In a repository several
//!   games share (`pins::shares_its_repository`), each game takes the newest release that carries
//!   ITS zip, so removing one game's zip from the newest release does not stop that game's fresh
//!   installs — they fall back to the previous release carrying it. To stop one game of a shared
//!   repository, remove its zip from EVERY release. The trust is in the
//!   owner's GitHub account, every token and collaborator with release rights on those
//!   repositories, and GitHub reporting the digest honestly.
//! - THE PUBLISHER'S TEST GATE: a prerelease. A draft, a GitHub prerelease, a semver pre-release
//!   tag (`v0.9.0-rc.1`) and a tag whose remainder is not semver are never picked, so a release
//!   is tested as a prerelease and only then promoted. The one exception is the owner's TEST
//!   build, compiled with `ASTRA_GAMES_PRERELEASES=1` ([`FOLLOWS_PRERELEASES`]): it follows the
//!   newest of stable and prerelease, which is how a prerelease reaches the machine it is tested
//!   on. Drafts never, in any build.
//! - REFUSED: a newest release whose asset has no digest (GitHub did not compute one for assets
//!   uploaded before mid-2025), or whose asset URL is not the canonical one, is refused rather than
//!   skipped to an older release; and a manifest that names another integration, repository,
//!   engine, game or version than the source and tag promise. A MISSING asset is refused the same
//!   way in a repository that is one source's alone (a broken publish must say so). In a
//!   repository several sources share (Death Stranding 1 and 2, the GTA trilogy, the two Unity
//!   runners, astra-reshade's runner and former GTA III zip) a release may carry only some of their
//!   zips, so each source takes the newest release that carries ITS zip ([`pick_with`]).
//! - FALLBACK ([`resolve`]): when GitHub cannot be asked (offline, a 403/429 rate limit, a 5xx, a
//!   private or deleted repository) and no listing it answered is younger than
//!   [`FALLBACK_LISTING_MAX_AGE`], Install uses the source's compiled pin — bytes bound by its
//!   own digest, manifest still held to the source — and the log says so once per source. Update
//!   never offers the fallback (it needs a listing) and nothing ever installs one over a newer
//!   installed version ([`RELEASE_OLDER`]). A source with no pin has no fallback: GitHub's own
//!   refusal stands.
//! - A SUCCEEDED SOURCE ([`super::pins::GiSource::predecessor`]): a game whose source took it over
//!   from another walks its lineage, newest first, and installs from the first source that has
//!   SHIPPED in the listing GitHub answered ([`has_shipped`]: a release this build follows carries
//!   its asset). The predecessor stands in for a successor GitHub says has not shipped — never for
//!   a GitHub that did not answer, which is the successor's own offline path above.
//!
//! The anonymous GitHub API allows 60 requests an hour per address. There are eight repositories;
//! each is listed at most once per [`LISTING_TTL`] for the Update question and only while a GI
//! from it is installed, and a plan reuses a listing for [`PLAN_LISTING_REUSE`].

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use astra_game_launcher::{LauncherError, Manifest};
use serde::Deserialize;

use super::pins::{GiSource, SourceTarget, normalize_repo};

/// The daemon's code when no usable release could be found for a source; `reason` says why.
pub const RELEASE_NOT_FOUND: &str = "release-not-found";
/// The daemon's code when GitHub could not be asked (offline, a private or deleted repo).
pub const RELEASE_UNAVAILABLE: &str = "release-unavailable";
/// The daemon's code when GitHub refused for its rate limit (HTTP 403 or 429): try later.
pub const RELEASE_RATE_LIMITED: &str = "release-rate-limited";
/// The daemon's code when the release chosen is OLDER than what is installed (a release was
/// withdrawn, or only the offline fallback was reachable): installing it would be a downgrade
/// nobody asked for.
pub const RELEASE_OLDER: &str = "release-older";
/// The daemon's code when the package GitHub served is not the source's integration.
pub const RELEASE_MISMATCH: &str = "release-mismatch";

/// How long a listing answers the Update question before `ListGames` asks GitHub again. One
/// request per repository per hour, and only while a GI from it is installed: the anonymous
/// GitHub API allows 60 an hour.
pub const LISTING_TTL: Duration = Duration::from_secs(60 * 60);

/// A releases listing is a few KB per release; 100 of them (one page) stay far below this.
const MAX_LISTING_BYTES: u64 = 4 << 20;
/// GitHub's largest page.
const PER_PAGE: usize = 100;
/// Pages read at most: 1000 releases. A source's newest release is on the first page in
/// practice; the bound only stops a runaway listing.
const MAX_PAGES: u32 = 10;
/// How long `PlanInstall` reuses a listing it (or a refresh) fetched instead of asking again:
/// a re-plan after `plan-stale`, or Install right after an Update offer, costs no request.
pub const PLAN_LISTING_REUSE: Duration = Duration::from_secs(5 * 60);
/// **Owner-test-build knob.** A build compiled with `ASTRA_GAMES_PRERELEASES=1` in its environment
/// (read at COMPILE time through `option_env!`, which cargo tracks, so changing it rebuilds this
/// crate) also follows GitHub PRERELEASES and semver pre-release tags: [`pick`] takes the newest
/// of stable and pre, still never a draft. Without it nothing changes: prereleases are never
/// picked. It exists so a prerelease the engine agent publishes (the publisher's test gate)
/// reaches the owner's test build with no repin; it is never for a build anyone else runs, and
/// two things hold that: a build compiled with it set carries `ASTRA-STAGING-ENDPOINTS-COMPILED-IN`
/// (it is in `build.rs`'s `ENDPOINT_OVERRIDES`), the bytes the release scan refuses an archive
/// for. That marker is the ONE thing keeping it out of shipped binaries since `games` got a
/// release path (0.2.7): a release builds with `games` on, and never with this set. The daemon
/// says at startup when it is on (`main.rs`).
pub const FOLLOWS_PRERELEASES: bool = is_one(option_env!("ASTRA_GAMES_PRERELEASES"));

/// `Some("1")` and nothing else: a knob that loosens a trust rule takes no near-miss spelling.
const fn is_one(value: Option<&str>) -> bool {
    match value {
        Some(v) => matches!(v.as_bytes(), [b'1']),
        None => false,
    }
}

/// The oldest listing `PlanInstall` falls back to when GitHub cannot be asked (offline, 403,
/// 429): a day. Older, the plan uses the source's compiled pin rather than a release list that
/// may have lost a withdrawn version long ago.
pub const FALLBACK_LISTING_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// One release, as GitHub's `GET /repos/{owner}/{repo}/releases` lists it (only what is read).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    /// `"sha256:<hex>"`; absent on assets GitHub never computed a digest for.
    #[serde(default)]
    pub digest: Option<String>,
}

/// The GI an install or an update uses: one release of one source — the newest stable one
/// ([`pick`]) or, offline, the compiled fallback ([`fallback_gi`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gi {
    /// The source this release was chosen for: its id, repository and target are what the
    /// package's manifest is held to ([`check_manifest`]).
    pub source: GiSource,
    pub tag: String,
    /// The tag as a version: without its prefix and leading `v`.
    pub version: String,
    pub sha256: String,
    pub url: String,
}

/// Why no release of a source could be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRelease {
    /// No stable release's tag starts with the prefix and carries a semver version.
    NoTag,
    /// The newest such release has no asset of the source's name.
    NoAsset,
    /// The asset has no SHA-256 digest GitHub vouches for.
    NoDigest,
    /// The asset's URL is not the canonical one of the source's repository.
    UrlMismatch,
}

impl NoRelease {
    pub fn code(self) -> &'static str {
        match self {
            NoRelease::NoTag => "no-release",
            NoRelease::NoAsset => "no-asset",
            NoRelease::NoDigest => "no-digest",
            NoRelease::UrlMismatch => "url-mismatch",
        }
    }

    pub fn error(self) -> LauncherError {
        LauncherError::new(RELEASE_NOT_FOUND).with("reason", self.code())
    }
}

/// The version a tag carries for `prefix`: the tag must start with `prefix` (ASCII letter case
/// ignored), and what follows, without a leading `v`, must be a semver version — a stable one
/// unless `prereleases` ([`FOLLOWS_PRERELEASES`]).
fn version_after(tag: &str, prefix: &str, prereleases: bool) -> Option<semver::Version> {
    let head = tag.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = &tag[prefix.len()..];
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    let v = semver::Version::parse(rest).ok()?;
    (prereleases || v.pre.is_empty()).then_some(v)
}

/// The canonical download URL of `asset` in `repo`'s release `tag`.
fn canonical_url(repo: &str, tag: &str, asset: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/{asset}")
}

/// The releases of `releases` a build follows for `source`, each with the version its tag
/// carries: never a draft, and a prerelease (GitHub's flag or a semver pre-release tag) only when
/// `prereleases`. The one definition [`pick_with`] and [`has_shipped`] share.
fn followed<'a>(
    releases: &'a [GhRelease],
    source: &GiSource,
    prereleases: bool,
) -> impl Iterator<Item = (semver::Version, &'a GhRelease)> {
    let prefix = source.tag_prefix;
    releases
        .iter()
        .filter(move |r| !r.draft && (prereleases || !r.prerelease))
        .filter_map(move |r| Some((version_after(&r.tag_name, prefix, prereleases)?, r)))
}

/// The release `source` installs out of `releases`: the newest stable release whose tag starts
/// with the source's prefix (by semver, not by listing order), with its asset's digest — or, in
/// an owner test build ([`FOLLOWS_PRERELEASES`]), the newest of stable and prerelease. The
/// daemon reaches it as [`pick_with`] with [`FOLLOWS_PRERELEASES`] through the lineage walk
/// ([`resolve`], [`pick_held`]); this name is the tests' and the prose's.
#[cfg_attr(not(test), allow(dead_code))]
pub fn pick(releases: &[GhRelease], source: &GiSource) -> Result<Gi, NoRelease> {
    pick_with(releases, source, FOLLOWS_PRERELEASES)
}

/// [`pick`], with whether prereleases count spelled out (`prereleases`), so both sides of the
/// knob are testable in one build. Drafts never count.
///
/// The release is the newest one followed — and, for a source whose repository other sources
/// share ([`super::pins::shares_its_repository`]), the newest followed one that CARRIES the
/// source's zip: one game's partial release (the trilogy's first carries GTA III's zip alone)
/// neither refuses nor holds back a game it leaves out. A source alone in its repository refuses
/// a newest release without its zip (`no-asset`), as before.
pub fn pick_with(
    releases: &[GhRelease],
    source: &GiSource,
    prereleases: bool,
) -> Result<Gi, NoRelease> {
    let newest =
        |a: &(semver::Version, &GhRelease), b: &(semver::Version, &GhRelease)| a.0.cmp(&b.0);
    let mut candidates = followed(releases, source, prereleases).peekable();
    if candidates.peek().is_none() {
        return Err(NoRelease::NoTag);
    }
    let (version, release) = if super::pins::shares_its_repository(source) {
        candidates
            .filter(|(_, r)| carries(r, source))
            .max_by(newest)
            .ok_or(NoRelease::NoAsset)?
    } else {
        candidates.max_by(newest).ok_or(NoRelease::NoTag)?
    };
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == source.asset)
        .ok_or(NoRelease::NoAsset)?;
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or(NoRelease::NoDigest)?;
    let url = canonical_url(source.repo, &release.tag_name, source.asset);
    if !asset.browser_download_url.eq_ignore_ascii_case(&url) {
        return Err(NoRelease::UrlMismatch);
    }
    Ok(Gi {
        source: *source,
        tag: release.tag_name.clone(),
        version: version.to_string(),
        sha256,
        url,
    })
}

/// Whether `release` lists `source`'s zip.
fn carries(release: &GhRelease, source: &GiSource) -> bool {
    release.assets.iter().any(|a| a.name == source.asset)
}

/// Whether `source` has SHIPPED in `releases`: some release this build follows (as [`pick_with`]
/// reads them) carries its asset. A source that has not yields to its predecessor
/// ([`GiSource::predecessor`]) in [`resolve`] and [`pick_held`]; one that has is [`pick_with`]'s,
/// refusals included (no digest, another URL).
pub fn has_shipped(releases: &[GhRelease], source: &GiSource, prereleases: bool) -> bool {
    followed(releases, source, prereleases).any(|(_, r)| carries(r, source))
}

/// The walk [`resolve`] and [`pick_held`] share over `source`'s lineage
/// ([`GiSource::lineage`]), newest first: the first source that has shipped in the listing
/// `listing_of` gives for it, else the last of the lineage, is [`pick_with`]'s. `listing_of`
/// ends the walk with its own `Err` when it has no listing for a source.
fn walk_lineage<E>(
    source: &GiSource,
    prereleases: bool,
    mut listing_of: impl FnMut(&GiSource) -> Result<Vec<GhRelease>, E>,
) -> Result<Result<Gi, NoRelease>, E> {
    let mut source = *source;
    loop {
        let listing = listing_of(&source)?;
        match source.predecessor {
            Some(before) if !has_shipped(&listing, &source, prereleases) => source = *before,
            _ => return Ok(pick_with(&listing, &source, prereleases)),
        }
    }
}

/// The release the Update question compares against, out of the listings `cache` already holds
/// (however old; never a network call): [`resolve`]'s walk over `source`'s lineage, on the side
/// of the prerelease knob `prereleases` says (the daemon passes [`FOLLOWS_PRERELEASES`]). `None`
/// when a listing the walk needs was never answered, or when the source it lands on has no usable
/// release.
pub fn pick_held(cache: &ReleaseCache, source: &GiSource, prereleases: bool) -> Option<Gi> {
    walk_lineage(source, prereleases, |s| cache.get(s.repo).ok_or(()))
        .ok()?
        .ok()
}

/// The source's compiled release ([`GiSource::fallback`]), with no listing and no network: the
/// digest is the pin's, the URL the canonical one of the source's own repository, and the
/// manifest is still held to the source. `None` for a source with no pin, and for a pin whose tag
/// is not the source's prefix + semver (refused, never guessed, as [`pick`] refuses such a tag) —
/// a test holds every pin to that. The daemon reaches it as [`fallback_gi_with`] with
/// [`FOLLOWS_PRERELEASES`] ([`resolve`]); this name is the tests' and the prose's.
#[cfg_attr(not(test), allow(dead_code))]
pub fn fallback_gi(source: &GiSource) -> Option<Gi> {
    fallback_gi_with(source, FOLLOWS_PRERELEASES)
}

/// [`fallback_gi`] for a build that does (`true`) or does not follow prereleases: a pin of a
/// prerelease is no fallback for a build that would never take that release online.
pub fn fallback_gi_with(source: &GiSource, follows_prereleases: bool) -> Option<Gi> {
    let pin = source.fallback?;
    if pin.prerelease && !follows_prereleases {
        return None;
    }
    let version = version_after(pin.tag, source.tag_prefix, false)?;
    Some(Gi {
        source: *source,
        tag: pin.tag.to_owned(),
        version: version.to_string(),
        sha256: pin.sha256.to_ascii_lowercase(),
        url: canonical_url(source.repo, pin.tag, source.asset),
    })
}

/// A release's package must be its source's integration: the source's id and repository, the
/// version its tag carries, and what it is for — a runner's `[target] engine` with NO appid (a
/// runner that named one would install for that game only, or a game's integration dressed as a
/// runner), or a game's own integration naming THIS game's Steam appid (a manifest without one
/// would let the launcher accept it for any game).
pub fn check_manifest(gi: &Gi, appid: &str, m: &Manifest) -> Result<(), LauncherError> {
    let mismatch = |field: &str, want: &str, got: &str| {
        LauncherError::new(RELEASE_MISMATCH)
            .with("field", field)
            .with("want", want)
            .with("got", got)
    };
    let source = &gi.source;
    if !m.integration.id.eq_ignore_ascii_case(source.id) {
        return Err(mismatch("integration.id", source.id, &m.integration.id));
    }
    let repo = m.integration.repo.as_deref().map(normalize_repo);
    if repo.as_deref() != Some(normalize_repo(source.repo).as_str()) {
        return Err(mismatch(
            "integration.repo",
            source.repo,
            m.integration.repo.as_deref().unwrap_or_default(),
        ));
    }
    let got_appid = m
        .target
        .steam_appid
        .map(|a| a.to_string())
        .unwrap_or_default();
    match source.target {
        SourceTarget::Runner { engine } => {
            let got = m.target.engine.as_deref().unwrap_or_default();
            if got != engine {
                return Err(mismatch("target.engine", engine, got));
            }
            if !got_appid.is_empty() {
                return Err(mismatch("target.steam_appid", "", &got_appid));
            }
        }
        SourceTarget::Game { appid: own, .. } => {
            // The game this install targets, and the game the source is for: one and the same.
            if got_appid != appid || own != appid {
                return Err(mismatch("target.steam_appid", appid, &got_appid));
            }
        }
    }
    // The version the tag promised (and an Update offered) is the version installed.
    let version = m.integration.version.trim();
    if version.strip_prefix('v').unwrap_or(version) != gi.version {
        return Err(mismatch("integration.version", &gi.version, version));
    }
    Ok(())
}

/// The GitHub API URL of one page of a repository's releases, newest first — by the
/// repository's numeric ID, which a rename, a transfer or a deleted-and-recreated repository of
/// the same name does not carry over. (A renamed or moved repository then fails closed at
/// [`pick`]'s canonical-URL check, whose URL still names `owner/repo`.)
pub fn listing_url(repo_id: u64, page: u32) -> String {
    format!(
        "https://api.github.com/repositories/{repo_id}/releases?per_page={PER_PAGE}&page={page}"
    )
}

/// Lists a source's repository's releases through `fetch` (the daemon's outbound-HTTP
/// chokepoint), page by page until a short page, at most [`MAX_PAGES`]. A refusal keeps its HTTP
/// status, and GitHub's rate limit (403, 429) is its own code.
pub fn fetch_listing(
    fetch: &dyn astra_game_launcher::Fetch,
    source: &GiSource,
) -> Result<Vec<GhRelease>, LauncherError> {
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let body = fetch
            .get(&listing_url(source.repo_id, page), MAX_LISTING_BYTES)
            .map_err(|e| unavailable(source, e))?;
        let list: Vec<GhRelease> = serde_json::from_slice(&body).map_err(|e| {
            LauncherError::new(RELEASE_UNAVAILABLE)
                .with("repo", source.repo)
                .with("detail", e.to_string())
        })?;
        let full = list.len() >= PER_PAGE;
        all.extend(list);
        if !full {
            break;
        }
    }
    Ok(all)
}

/// A failed request as the daemon's code, its HTTP status kept.
fn unavailable(source: &GiSource, e: LauncherError) -> LauncherError {
    let status = e.params.get("status").cloned().unwrap_or_default();
    let code = if status.starts_with("403") || status.starts_with("429") {
        RELEASE_RATE_LIMITED
    } else {
        RELEASE_UNAVAILABLE
    };
    let mut out = LauncherError::new(code)
        .with("repo", source.repo)
        .with("detail", e.code.to_string());
    if !status.is_empty() {
        out = out.with("status", status);
    }
    out
}

/// The release `source` installs now — `PlanInstall`'s one question, for every source.
///
/// A listing GitHub answered within [`PLAN_LISTING_REUSE`] is reused (a re-plan after
/// `plan-stale`, or Install right after an Update offer, costs no request); else GitHub is asked
/// and the answer recorded. When GitHub cannot be asked, the last listing it answered is used if
/// it is younger than [`FALLBACK_LISTING_MAX_AGE`] (its release is still held to GitHub's
/// digest); otherwise the source's compiled pin ([`fallback_gi`]), said once in the log. A
/// listing GitHub DID answer that holds no usable release is a refusal ([`NoRelease`]), never
/// the fallback: the fallback stands in for an unreachable GitHub, not for a release its
/// publisher got wrong.
///
/// A source that succeeds another ([`GiSource::predecessor`]) and has not shipped in the listing
/// its GitHub answered yields to that predecessor, asked the same way ([`walk_lineage`]); one
/// whose GitHub could not be asked takes its own offline path above and never its predecessor.
pub fn resolve(
    cache: &ReleaseCache,
    fetch: &dyn astra_game_launcher::Fetch,
    source: &GiSource,
) -> Result<Gi, LauncherError> {
    resolve_with(cache, fetch, source, FOLLOWS_PRERELEASES)
}

/// [`resolve`] on either side of the prerelease knob.
pub fn resolve_with(
    cache: &ReleaseCache,
    fetch: &dyn astra_game_launcher::Fetch,
    source: &GiSource,
    prereleases: bool,
) -> Result<Gi, LauncherError> {
    // The source GitHub could not be asked for, with its refusal (boxed: the walk's `Err`).
    let walked = walk_lineage(source, prereleases, |s| {
        listing_for(cache, fetch, s).map_err(|e| Box::new((*s, e)))
    });
    match walked {
        Ok(picked) => picked.map_err(NoRelease::error),
        Err(unreachable) => {
            let (unreachable, e) = *unreachable;
            offline_fallback(cache, &unreachable, e, prereleases)
        }
    }
}

/// `source`'s listing for a plan: one GitHub answered within [`PLAN_LISTING_REUSE`], else
/// GitHub's answer now (recorded), else one it answered within [`FALLBACK_LISTING_MAX_AGE`];
/// `Err` with GitHub's refusal when there is none of the three.
fn listing_for(
    cache: &ReleaseCache,
    fetch: &dyn astra_game_launcher::Fetch,
    source: &GiSource,
) -> Result<Vec<GhRelease>, LauncherError> {
    if let Some(listing) = cache.get_fresh(source.repo, PLAN_LISTING_REUSE) {
        return Ok(listing);
    }
    match fetch_listing(fetch, source) {
        Ok(listing) => {
            cache.record(source.repo, Some(listing.clone()));
            Ok(listing)
        }
        Err(e) => {
            cache.record(source.repo, None);
            cache
                .get_fresh(source.repo, FALLBACK_LISTING_MAX_AGE)
                .ok_or(e)
        }
    }
}

/// The compiled pin, for a source whose GitHub could not be asked (`e`) with no listing young
/// enough to stand in. Logged once per source per run; `e` itself when there is no usable pin.
fn offline_fallback(
    cache: &ReleaseCache,
    source: &GiSource,
    e: LauncherError,
    prereleases: bool,
) -> Result<Gi, LauncherError> {
    let Some(gi) = fallback_gi_with(source, prereleases) else {
        return Err(e);
    };
    if cache.first_fallback(source.id) {
        tracing::warn!(
            id = source.id,
            repo = source.repo,
            tag = %gi.tag,
            code = e.code,
            status = e.params.get("status").map(String::as_str).unwrap_or_default(),
            "games: GitHub could not be asked for this GI's releases and no recent listing is \
             cached; installing the compiled fallback release"
        );
    }
    Ok(gi)
}

/// Younger than `max_age` by BOTH clocks; a wall clock that went backwards is not young.
fn young(mono: Instant, wall: SystemTime, max_age: Duration) -> bool {
    mono.elapsed() < max_age
        && SystemTime::now()
            .duration_since(wall)
            .is_ok_and(|age| age < max_age)
}

#[derive(Debug)]
struct Entry {
    /// The last time GitHub was asked, answered or not.
    checked: Instant,
    /// When GitHub last ANSWERED: the monotonic clock, and the wall clock beside it. `Instant`
    /// stops while the machine sleeps (a Steam Deck suspended for a week), so an age is trusted
    /// only when BOTH clocks say it is young enough.
    fetched: Option<(Instant, SystemTime)>,
    /// The last listing GitHub answered; a failed refresh keeps it.
    list: Option<Vec<GhRelease>>,
}

/// Each repository's last releases listing, for the Update question `ListGames` answers without
/// touching the network, and the listing `PlanInstall` reuses for [`PLAN_LISTING_REUSE`] or
/// falls back to when GitHub cannot be asked ([`resolve`]).
#[derive(Debug, Default)]
pub struct ReleaseCache {
    entries: Mutex<HashMap<String, Entry>>,
    refreshing: Mutex<HashSet<String>>,
    /// The sources whose compiled fallback was already announced in the log this run.
    fallback_said: Mutex<HashSet<&'static str>>,
}

impl ReleaseCache {
    fn key(repo: &str) -> String {
        normalize_repo(repo)
    }

    /// The last listing of `repo`, however old; `None` when GitHub never answered.
    pub fn get(&self, repo: &str) -> Option<Vec<GhRelease>> {
        let entries = self.entries.lock().unwrap();
        entries.get(&Self::key(repo)).and_then(|e| e.list.clone())
    }

    /// The last listing of `repo` when GitHub answered it less than `max_age` ago.
    pub fn get_fresh(&self, repo: &str, max_age: Duration) -> Option<Vec<GhRelease>> {
        let entries = self.entries.lock().unwrap();
        let e = entries.get(&Self::key(repo))?;
        e.fetched
            .is_some_and(|(mono, wall)| young(mono, wall, max_age))
            .then(|| e.list.clone())
            .flatten()
    }

    /// Records what asking GitHub gave. A failure keeps the previous listing but still counts
    /// as a check, so an unreachable GitHub is asked once per [`LISTING_TTL`], not per listing.
    /// Returns whether the listing changed.
    pub fn record(&self, repo: &str, answer: Option<Vec<GhRelease>>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let e = entries.entry(Self::key(repo)).or_insert(Entry {
            checked: Instant::now(),
            fetched: None,
            list: None,
        });
        e.checked = Instant::now();
        let Some(list) = answer else {
            return false;
        };
        e.fetched = Some((Instant::now(), SystemTime::now()));
        if e.list.as_ref() == Some(&list) {
            return false;
        }
        e.list = Some(list);
        true
    }

    /// Claims the refresh of `repo` when its listing is missing or older than `ttl` and no
    /// refresh is already running. The caller must call [`Self::finish`] when done.
    pub fn claim_refresh(&self, repo: &str, ttl: Duration) -> bool {
        let key = Self::key(repo);
        let stale = self
            .entries
            .lock()
            .unwrap()
            .get(&key)
            .is_none_or(|e| e.checked.elapsed() >= ttl);
        stale && self.refreshing.lock().unwrap().insert(key)
    }

    /// True the first time it is asked for `source_id`: whether the offline fallback of that
    /// source still has to be said in the log.
    fn first_fallback(&self, source_id: &'static str) -> bool {
        self.fallback_said.lock().unwrap().insert(source_id)
    }

    /// Moves a listing's wall-clock fetch time back by `by`, as a suspended machine sees it.
    #[cfg(test)]
    pub fn age_wall_clock(&self, repo: &str, by: Duration) {
        let mut entries = self.entries.lock().unwrap();
        if let Some((_, wall)) = entries
            .get_mut(&Self::key(repo))
            .and_then(|e| e.fetched.as_mut())
        {
            *wall -= by;
        }
    }

    pub fn finish(&self, repo: &str) {
        self.refreshing.lock().unwrap().remove(&Self::key(repo));
    }
}

/// One stable release of `source` as GitHub lists it: its own asset at the canonical URL, digest
/// `n` repeated (`"0a0a…"`). For tests here and in `service`.
#[cfg(test)]
pub(crate) fn test_release(source: &GiSource, tag: &str, n: u8) -> GhRelease {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::pins::{
        self, FallbackPin, GAME_SOURCES, GTA3_ASTRA_RESHADE, UNIVERSAL_RUNNER,
        UNIVERSAL_RUNNER_IL2CPP, UNIVERSAL_RUNNER_UNREAL, all_sources, game_source,
    };

    const DS1: &str = "1850570";
    const DS2: &str = "3280350";
    const DS_REPO: &str = "mihailinl/astra-in-death-stranding";

    fn release(source: &GiSource, tag: &str, n: u8) -> GhRelease {
        test_release(source, tag, n)
    }

    fn as_json(r: &GhRelease) -> serde_json::Value {
        serde_json::json!({
            "tag_name": r.tag_name,
            "draft": r.draft,
            "prerelease": r.prerelease,
            "assets": r.assets.iter().map(|a| serde_json::json!({
                "name": a.name,
                "browser_download_url": a.browser_download_url,
                "digest": a.digest,
            })).collect::<Vec<_>>(),
        })
    }

    fn ds(appid: &str) -> GiSource {
        game_source(appid).expect("a built-in source")
    }

    /// One release of a shared repository carrying every listed source's asset.
    fn release_of(sources: &[GiSource], tag: &str, n: u8) -> GhRelease {
        let mut r = release(&sources[0], tag, n);
        r.assets = sources
            .iter()
            .enumerate()
            .map(|(i, s)| release(s, tag, n.wrapping_add(i as u8)).assets.remove(0))
            .collect();
        r
    }

    /// **Every source follows its newest stable release**, by semver and not listing order, its
    /// own asset of it, with a GitHub prerelease, a draft and a `-rc` tag above it all ignored.
    #[test]
    fn every_source_follows_its_newest_stable_release_and_never_a_prerelease() {
        for s in all_sources() {
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
            let gi = pick_with(&listing, &s, false).unwrap_or_else(|e| panic!("{}: {e:?}", s.name));
            assert_eq!(
                gi.tag, "v0.10.0",
                "{}: semver order, not listing order",
                s.name
            );
            assert_eq!(gi.version, "0.10.0", "{}", s.name);
            assert_eq!(gi.sha256, "10".repeat(32), "{}", s.name);
            assert_eq!(
                gi.url,
                format!(
                    "https://github.com/{}/releases/download/v0.10.0/{}",
                    s.repo, s.asset
                ),
                "{}",
                s.name
            );
            assert_eq!(gi.source, s);
        }
    }

    /// The owner-test-build side of the knob: the newest of stable and prerelease (GitHub's flag
    /// or a semver pre-release tag), a newer stable still winning over an older prerelease, and
    /// a draft never.
    #[test]
    fn with_the_test_build_knob_the_newest_of_stable_and_prerelease_is_picked() {
        for s in all_sources() {
            let mut pre = release(&s, "v0.9.0", 0x90);
            pre.prerelease = true;
            let mut draft = release(&s, "v1.0.0", 0xa0);
            draft.draft = true;
            let listing = vec![draft.clone(), pre.clone(), release(&s, "v0.8.1", 0x81)];
            assert_eq!(
                pick_with(&listing, &s, true).unwrap().tag,
                "v0.9.0",
                "{}",
                s.name
            );
            assert_eq!(
                pick_with(&listing, &s, false).unwrap().tag,
                "v0.8.1",
                "{}",
                s.name
            );
            // A semver pre-release tag counts too, by semver order.
            let rc = vec![pre.clone(), release(&s, "v0.9.1-rc.1", 0x91)];
            let gi = pick_with(&rc, &s, true).unwrap();
            assert_eq!(
                (gi.tag.as_str(), gi.version.as_str()),
                ("v0.9.1-rc.1", "0.9.1-rc.1")
            );
            // A newer stable release beats an older prerelease.
            let newer = vec![pre, release(&s, "v0.10.0", 0x10)];
            assert_eq!(pick_with(&newer, &s, true).unwrap().tag, "v0.10.0");
            // Only a draft: nothing, whatever the knob.
            assert_eq!(pick_with(&[draft], &s, true), Err(NoRelease::NoTag));
        }
    }

    /// The knob takes `1` and nothing else, and a build without it (every gate's) is the stable
    /// side: [`pick`] is `pick_with(.., false)`.
    #[test]
    fn the_prerelease_knob_is_exactly_one_and_off_in_this_build() {
        assert!(is_one(Some("1")));
        for near in [
            Some("0"),
            Some("true"),
            Some("11"),
            Some(" 1"),
            Some(""),
            None,
        ] {
            assert!(!is_one(near), "{near:?}");
        }
        assert_eq!(
            FOLLOWS_PRERELEASES,
            is_one(option_env!("ASTRA_GAMES_PRERELEASES"))
        );
        if !FOLLOWS_PRERELEASES {
            let mut pre = release(&UNIVERSAL_RUNNER, "v0.9.0", 9);
            pre.prerelease = true;
            let listing = vec![pre, release(&UNIVERSAL_RUNNER, "v0.8.1", 8)];
            assert_eq!(pick(&listing, &UNIVERSAL_RUNNER).unwrap().tag, "v0.8.1");
        }
    }

    /// astra-reshade as it is listed on 2026-10-17 (real digests): stable v0.2.1 carries only the
    /// generic runner; prerelease v0.2.2 carries it and GTA III's former zip. A build without the
    /// knob installs v0.2.1 for Scarlet Skips and refuses that GTA III source (`no-asset`: the
    /// newest stable release does not carry its zip); the owner's test build takes v0.2.2 for both.
    #[test]
    fn astra_reshades_real_listing_on_both_sides_of_the_knob() {
        let asset = |tag: &str, name: &str, sha: &str| GhAsset {
            name: name.to_owned(),
            browser_download_url: format!(
                "https://github.com/mihailinl/astra-reshade/releases/download/{tag}/{name}"
            ),
            digest: Some(format!("sha256:{sha}")),
        };
        let listing = vec![
            GhRelease {
                tag_name: "v0.2.2".into(),
                draft: false,
                prerelease: true,
                assets: vec![
                    asset(
                        "v0.2.2",
                        "astra-gi-gta3.zip",
                        "ff49114b2108193ca6f009e07cce679793438f959f797c30cc8968c06b873017",
                    ),
                    asset(
                        "v0.2.2",
                        "astra-gi.zip",
                        "fc9370ed5c186b19b1262d41024b76e8f2ac9fba5e533c68473a09844648c346",
                    ),
                ],
            },
            GhRelease {
                tag_name: "v0.2.1".into(),
                draft: false,
                prerelease: false,
                assets: vec![asset(
                    "v0.2.1",
                    "astra-gi.zip",
                    "e89fcd792fe53816dd47bf6f7cafd436cd59b1c766d6dba5aeb853ac1a1c4eea",
                )],
            },
        ];
        let gta3 = GTA3_ASTRA_RESHADE;
        let runner = pick_with(&listing, &UNIVERSAL_RUNNER_UNREAL, false).unwrap();
        assert_eq!(Some(runner), fallback_gi(&UNIVERSAL_RUNNER_UNREAL));
        assert_eq!(pick_with(&listing, &gta3, false), Err(NoRelease::NoAsset));
        let runner = pick_with(&listing, &UNIVERSAL_RUNNER_UNREAL, true).unwrap();
        assert_eq!(
            (runner.tag.as_str(), runner.sha256.as_str()),
            (
                "v0.2.2",
                "fc9370ed5c186b19b1262d41024b76e8f2ac9fba5e533c68473a09844648c346"
            )
        );
        let gi = pick_with(&listing, &gta3, true).unwrap();
        assert_eq!(
            Some(gi.clone()),
            fallback_gi_with(&gta3, true),
            "the prerelease IS GTA III's fallback — in a build that follows prereleases"
        );
        assert_eq!(
            fallback_gi_with(&gta3, false),
            None,
            "a build that never takes the prerelease online must not take it as a fallback"
        );
        assert!(gi.url.ends_with("/v0.2.2/astra-gi-gta3.zip"));
    }

    /// The Mono and IL2CPP runners share ONE release; each takes its own zip from it, as do
    /// Death Stranding 1 and 2 from theirs.
    #[test]
    fn sources_of_one_repository_take_their_own_asset_of_one_release() {
        let bepinex = [UNIVERSAL_RUNNER, UNIVERSAL_RUNNER_IL2CPP];
        let listing = vec![release_of(&bepinex, "v0.9.0", 0x40)];
        let mono = pick(&listing, &UNIVERSAL_RUNNER).unwrap();
        let il2cpp = pick(&listing, &UNIVERSAL_RUNNER_IL2CPP).unwrap();
        assert!(mono.url.ends_with("/v0.9.0/astra-gi.zip"));
        assert!(il2cpp.url.ends_with("/v0.9.0/astra-gi-il2cpp.zip"));
        assert_ne!(mono.sha256, il2cpp.sha256);
        let ds_listing = vec![release_of(&[ds(DS1), ds(DS2)], "v0.2.0", 0x20)];
        assert!(
            pick(&ds_listing, &ds(DS1))
                .unwrap()
                .url
                .ends_with("/v0.2.0/astra-gi-ds1.zip")
        );
        assert!(
            pick(&ds_listing, &ds(DS2))
                .unwrap()
                .url
                .ends_with("/v0.2.0/astra-gi-ds2.zip")
        );
    }

    #[test]
    fn every_source_has_a_usable_fallback_at_its_canonical_url() {
        for s in all_sources() {
            let Some(pin) = s.fallback else {
                // No pin (the trilogy's three, `pins`' own test names them): nothing offline.
                assert_eq!(fallback_gi_with(&s, true), None, "{}", s.name);
                continue;
            };
            let gi =
                fallback_gi_with(&s, true).unwrap_or_else(|| panic!("{}'s fallback tag", s.name));
            assert_eq!(gi.tag, pin.tag);
            assert_eq!(gi.sha256, pin.sha256);
            assert_eq!(
                gi.url,
                format!(
                    "https://github.com/{}/releases/download/{}/{}",
                    s.repo, pin.tag, s.asset
                )
            );
            assert!(!gi.version.starts_with('v'), "{}", s.name);
        }
        // The literal URLs of three of them, so a change of URL shape cannot hide in the formula.
        assert_eq!(
            fallback_gi(&UNIVERSAL_RUNNER_UNREAL).unwrap().url,
            "https://github.com/mihailinl/astra-reshade/releases/download/v0.2.1/astra-gi.zip"
        );
        assert_eq!(
            fallback_gi(&UNIVERSAL_RUNNER_IL2CPP).unwrap().url,
            "https://github.com/mihailinl/astra-bepinex/releases/download/v0.8.1/astra-gi-il2cpp.zip"
        );
        let ds1 = fallback_gi(&ds(DS1)).unwrap();
        assert_eq!(
            ds1.url,
            "https://github.com/mihailinl/astra-in-death-stranding/releases/download/v0.1.1/astra-gi-ds1.zip"
        );
        assert_eq!(ds1.version, "0.1.1");
        assert_eq!(
            fallback_gi(&ds(DS2)).unwrap().sha256,
            "9b2728ff1a06cdef01465003d38486cd23e99f87b32a304489653aa6c0d26479"
        );
        // A fallback whose tag is not the prefix + semver is refused, never guessed.
        let mut odd = UNIVERSAL_RUNNER;
        odd.fallback = Some(FallbackPin {
            tag: "latest",
            ..UNIVERSAL_RUNNER.fallback.unwrap()
        });
        assert_eq!(fallback_gi(&odd), None);
    }

    #[test]
    fn a_release_without_a_trustworthy_asset_is_refused_not_skipped() {
        // A source alone in its repository: a newest release without its zip is a broken publish,
        // refused rather than skipped.
        let lethal = game_source("1966720").unwrap();
        assert!(!pins::shares_its_repository(&lethal));
        let mut no_asset = release(&lethal, "v0.3.0", 3);
        no_asset.assets[0].name = "astra-gi-other.zip".into();
        assert_eq!(
            pick(&[no_asset, release(&lethal, "v0.2.0", 2)], &lethal),
            Err(NoRelease::NoAsset)
        );
        // In a shared repository a release without one source's zip is another source's release:
        // skipped for that source. A release that carries it but untrustworthily is refused.
        let s = ds(DS1);
        for bad in [
            None,
            Some("md5:00".to_owned()),
            Some("sha256:abc".to_owned()),
        ] {
            let mut r = release(&s, "v0.2.0", 2);
            r.assets[0].digest = bad.clone();
            assert_eq!(pick(&[r], &s), Err(NoRelease::NoDigest), "{bad:?}");
        }
        let mut elsewhere = release(&s, "v0.2.0", 2);
        elsewhere.assets[0].browser_download_url =
            "https://github.com/someone-else/fork/releases/download/v0.2.0/astra-gi-ds1.zip".into();
        assert_eq!(pick(&[elsewhere], &s), Err(NoRelease::UrlMismatch));
        assert_eq!(pick(&[], &s), Err(NoRelease::NoTag));
    }

    #[test]
    fn a_real_github_listing_parses() {
        let body = br#"[{"url":"x","tag_name":"v0.8.1","draft":false,"prerelease":false,
            "assets":[{"name":"astra-gi-il2cpp.zip","size":5,"digest":"sha256:42c8e2b1bddcd5e62d9f87eaccc78c6beed9d7306028ea4706a50c2a08a7d20e",
            "browser_download_url":"https://github.com/mihailinl/astra-bepinex/releases/download/v0.8.1/astra-gi-il2cpp.zip"},
            {"name":"astra-gi.zip","size":5,"digest":"sha256:1ea3ef142e718afeffc9bbbc9dfab12e04a14d657d97524d9e7ba2225789a687",
            "browser_download_url":"https://github.com/mihailinl/astra-bepinex/releases/download/v0.8.1/astra-gi.zip"}]}]"#;
        let list: Vec<GhRelease> = serde_json::from_slice(body).unwrap();
        let gi = pick(&list, &UNIVERSAL_RUNNER).unwrap();
        assert_eq!(gi.version, "0.8.1");
        // Today's newest release IS the compiled fallback.
        assert_eq!(Some(gi), fallback_gi(&UNIVERSAL_RUNNER));
    }

    fn manifest_toml(id: &str, repo: &str, target: &str, version: &str) -> Manifest {
        Manifest::parse(&format!(
            "schema = 1\n[integration]\nid = \"{id}\"\nname = \"x\"\nversion = \"{version}\"\nauthors = [\"a\"]\nlicense = \"MIT\"\nrepo = \"{repo}\"\n[target]\n{target}platforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n"
        ))
        .unwrap()
    }

    fn game_target(appid: Option<u32>, exe: &str) -> String {
        let appid = appid
            .map(|a| format!("steam_appid = {a}\n"))
            .unwrap_or_default();
        format!("{appid}exe = \"{exe}\"\n")
    }

    fn runner_target(engine: Option<&str>, appid: Option<u32>) -> String {
        let engine = engine
            .map(|e| format!("engine = \"{e}\"\n"))
            .unwrap_or_default();
        let appid = appid
            .map(|a| format!("steam_appid = {a}\n"))
            .unwrap_or_default();
        format!("{engine}{appid}exe = \"*\"\n")
    }

    /// Every source's REAL fallback manifest passes its own check: the `[integration]` and
    /// `[target]` of each of the ten release zips, read with `unzip -p <zip> astra-gi.toml` on
    /// 2026-10-17 (the by-name zips downloaded from their releases, GTA III's astra-reshade one
    /// from its v0.2.2 prerelease, the three runners from the local runner cache; digests equal to
    /// the fallbacks'). The check now applies to every GI, so a field it reads wrongly would refuse
    /// a working install. The trilogy's sources have no pin; their real manifest is checked in
    /// `the_trilogys_real_discovery_prerelease_on_both_sides_of_the_knob`.
    #[test]
    fn every_sources_real_fallback_manifest_passes_its_own_check() {
        let real: &[(&GiSource, &str, &str, &str)] = &[
            (
                &UNIVERSAL_RUNNER,
                "astra.unity",
                "astra-bepinex",
                "engine = \"unity-mono\"\nexe = \"*\"\n",
            ),
            (
                &UNIVERSAL_RUNNER_IL2CPP,
                "astra.unity-il2cpp",
                "astra-bepinex",
                "engine = \"unity-il2cpp\"\nexe = \"*\"\n",
            ),
            (
                &UNIVERSAL_RUNNER_UNREAL,
                "astra.unreal",
                "astra-reshade",
                "engine = \"unreal\"\nexe = \"*\"\n",
            ),
            (
                &GAME_SOURCES[0],
                "astra.lethal-company",
                "astra-lethal",
                "steam_appid = 1966720\nexe = \"Lethal Company.exe\"\n",
            ),
            (
                &GAME_SOURCES[1],
                "astra.content-warning",
                "astra-content-warning",
                "steam_appid = 2881650\nexe = \"Content Warning.exe\"\n",
            ),
            (
                &GAME_SOURCES[2],
                "astra.peak",
                "astra-peak",
                "steam_appid = 3527290\nexe = \"PEAK.exe\"\n",
            ),
            (
                &GAME_SOURCES[3],
                "astra.repo",
                "astra-repo",
                "steam_appid = 3241660\nexe = \"REPO.exe\"\n",
            ),
            (
                &GAME_SOURCES[4],
                "astra.death-stranding",
                "astra-in-death-stranding",
                "steam_appid = 1850570\nexe = \"ds.exe\"\n",
            ),
            (
                &GAME_SOURCES[5],
                "astra.death-stranding-2",
                "astra-in-death-stranding",
                "steam_appid = 3280350\nexe = \"DS2.exe\"\n",
            ),
            (
                &GTA3_ASTRA_RESHADE,
                "astra.unreal.gta3-definitive-edition",
                "astra-reshade",
                "engine = \"unreal\"\nexe = \"Gameface/Binaries/Win64/LibertyCity.exe\"\nsteam_appid = 1546970\n",
            ),
        ];
        assert_eq!(
            real.len(),
            all_sources().filter(|s| s.fallback.is_some()).count()
        );
        for (source, id, repo, target) in real {
            // Every pin's manifest, whether or not THIS build would take it as a fallback.
            let gi = fallback_gi_with(source, true).unwrap();
            let m = manifest_toml(
                id,
                &format!("https://github.com/mihailinl/{repo}"),
                target,
                &gi.version,
            );
            // A runner installs on any game; a game's own integration on its own.
            let appid = source.appid().unwrap_or("4513480");
            check_manifest(&gi, appid, &m).unwrap_or_else(|e| panic!("{}: {e:?}", source.name));
        }
    }

    #[test]
    fn a_runners_package_must_name_the_runners_engine_and_no_game() {
        let repo = "https://github.com/mihailinl/astra-reshade";
        let gi = pick(
            &[release(&UNIVERSAL_RUNNER_UNREAL, "v0.3.0", 3)],
            &UNIVERSAL_RUNNER_UNREAL,
        )
        .unwrap();
        let ok = manifest_toml(
            "astra.unreal",
            repo,
            &runner_target(Some("unreal"), None),
            "0.3.0",
        );
        assert!(check_manifest(&gi, "4513480", &ok).is_ok());
        for (target, field) in [
            // A Unity runner's zip published in the Unreal runner's repository.
            (runner_target(Some("unity-mono"), None), "target.engine"),
            (runner_target(Some("unity-il2cpp"), None), "target.engine"),
            // A runner that names no engine would let the launcher skip its engine check.
            (runner_target(None, None), "target.engine"),
            // A game's integration dressed as a runner.
            (
                runner_target(Some("unreal"), Some(4513480)),
                "target.steam_appid",
            ),
        ] {
            let m = manifest_toml("astra.unreal", repo, &target, "0.3.0");
            let e = check_manifest(&gi, "4513480", &m).unwrap_err();
            assert_eq!(e.code, RELEASE_MISMATCH);
            assert_eq!(
                e.params.get("field").map(String::as_str),
                Some(field),
                "{target}"
            );
        }
        // The Mono runner's own zip is held to "unity-mono", not to IL2CPP's engine.
        let mono = pick(
            &[release(&UNIVERSAL_RUNNER, "v0.9.0", 9)],
            &UNIVERSAL_RUNNER,
        )
        .unwrap();
        let bepinex = "https://github.com/mihailinl/astra-bepinex";
        let il2cpp_zip = manifest_toml(
            "astra.unity",
            bepinex,
            &runner_target(Some("unity-il2cpp"), None),
            "0.9.0",
        );
        assert_eq!(
            check_manifest(&mono, "1", &il2cpp_zip)
                .unwrap_err()
                .params
                .get("field")
                .map(String::as_str),
            Some("target.engine")
        );
    }

    #[test]
    fn a_games_package_must_name_the_source_and_this_game() {
        let s = ds(DS1);
        let gi = pick(&[release(&s, "v0.10.0", 0x1a)], &s).unwrap();
        let url = "https://github.com/mihailinl/astra-in-death-stranding";
        let target = |appid| game_target(appid, "ds.exe");
        assert!(
            check_manifest(
                &gi,
                DS1,
                &manifest_toml(
                    "astra.death-stranding",
                    url,
                    &target(Some(1850570)),
                    "0.10.0"
                )
            )
            .is_ok()
        );
        // The tag's version, `v` or not, and nothing else.
        assert!(
            check_manifest(
                &gi,
                DS1,
                &manifest_toml(
                    "astra.death-stranding",
                    url,
                    &target(Some(1850570)),
                    "v0.10.0"
                )
            )
            .is_ok()
        );
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
            // The other game's appid, and none at all.
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
            assert_eq!(e.params.get("field").map(String::as_str), Some(field));
        }
        // A Unity game's own integration naming another game's appid: refused the same way.
        let lethal = game_source("1966720").unwrap();
        let gi = pick(&[release(&lethal, "v0.3.0", 3)], &lethal).unwrap();
        let wrong = manifest_toml(
            "astra.lethal-company",
            "https://github.com/mihailinl/astra-lethal",
            &game_target(Some(2881650), "Lethal Company.exe"),
            "0.3.0",
        );
        let e = check_manifest(&gi, "1966720", &wrong).unwrap_err();
        assert_eq!(
            (e.code, e.params.get("field").map(String::as_str)),
            (RELEASE_MISMATCH, Some("target.steam_appid"))
        );
        // Nor may a game's own source install on another game.
        let right = manifest_toml(
            "astra.lethal-company",
            "https://github.com/mihailinl/astra-lethal",
            &game_target(Some(1966720), "Lethal Company.exe"),
            "0.3.0",
        );
        assert!(check_manifest(&gi, "1966720", &right).is_ok());
        assert!(check_manifest(&gi, "2881650", &right).is_err());
    }

    /// The Games tab turns each of these codes into its own sentence; a renamed code would fall
    /// to "something went wrong" with nothing failing.
    #[test]
    fn the_games_tab_has_a_sentence_for_every_release_code() {
        let tab = include_str!("../../../astra-ui/src/pages/Workshop/GamesTab.tsx");
        for code in [
            RELEASE_NOT_FOUND,
            RELEASE_UNAVAILABLE,
            RELEASE_MISMATCH,
            RELEASE_RATE_LIMITED,
            RELEASE_OLDER,
            astra_game_launcher::codes::VAC_UNCHECKED,
        ] {
            assert!(
                tab.contains(&format!("case \"{code}\":\n      return t(\"games.state.")),
                "GamesTab.tsx errorSentence has no case for {code}"
            );
        }
        // Steam's VAC kind has its own sentence, and an unread VAC flag is shown on the sheet.
        let kind = astra_game_launcher::VALVE_ANTI_CHEAT;
        assert!(tab.contains(&format!(
            "case \"{kind}\":\n      return t(\"games.state.antiCheat.vac\")"
        )));
        assert!(tab.contains("plan.warnings.includes(\"vac-unchecked\")"));
    }

    /// A `Fetch` that answers from a script and records every URL asked; it fails the test if
    /// asked more often than scripted.
    struct Scripted {
        asked: Mutex<Vec<String>>,
        answers: Mutex<Vec<Result<Vec<u8>, LauncherError>>>,
    }

    impl Scripted {
        fn new(answers: Vec<Result<Vec<u8>, LauncherError>>) -> Scripted {
            Scripted {
                asked: Mutex::new(Vec::new()),
                answers: Mutex::new(answers),
            }
        }

        fn listing(list: &[GhRelease]) -> Result<Vec<u8>, LauncherError> {
            Ok(serde_json::to_vec(&list.iter().map(as_json).collect::<Vec<_>>()).unwrap())
        }

        fn asked(&self) -> usize {
            self.asked.lock().unwrap().len()
        }
    }

    impl astra_game_launcher::Fetch for Scripted {
        fn get(&self, url: &str, _max: u64) -> Result<Vec<u8>, LauncherError> {
            self.asked.lock().unwrap().push(url.to_owned());
            let mut answers = self.answers.lock().unwrap();
            assert!(!answers.is_empty(), "asked {url} with no answer scripted");
            answers.remove(0)
        }
    }

    fn refused(status: &str) -> LauncherError {
        LauncherError::new(astra_game_launcher::codes::FETCH_FAILED).with("status", status)
    }

    #[test]
    fn the_listing_is_asked_by_repository_id_page_by_page() {
        let s = ds(DS1);
        // A full first page of other tags, then DS1's release on page two.
        let filler: Vec<GhRelease> = (0..100)
            .map(|n| release(&s, &format!("tool-v0.0.{n}"), 1))
            .collect();
        let page2 = vec![release(&s, "v0.3.0", 0x13)];
        let f = Scripted::new(vec![Scripted::listing(&filler), Scripted::listing(&page2)]);
        let list = fetch_listing(&f, &s).unwrap();
        assert_eq!(list.len(), 101);
        assert_eq!(pick(&list, &s).unwrap().tag, "v0.3.0");
        assert_eq!(
            *f.asked.lock().unwrap(),
            vec![
                "https://api.github.com/repositories/1405082830/releases?per_page=100&page=1",
                "https://api.github.com/repositories/1405082830/releases?per_page=100&page=2",
            ],
            "by the repository's id, never its name"
        );
    }

    #[test]
    fn a_rate_limit_is_its_own_code_and_keeps_its_status() {
        for (status, code) in [
            ("403 Forbidden", RELEASE_RATE_LIMITED),
            ("429 Too Many Requests", RELEASE_RATE_LIMITED),
            ("404 Not Found", RELEASE_UNAVAILABLE),
        ] {
            let f = Scripted::new(vec![Err(refused(status))]);
            let e = fetch_listing(&f, &ds(DS1)).unwrap_err();
            assert_eq!(e.code, code, "{status}");
            assert_eq!(e.params.get("status").map(String::as_str), Some(status));
        }
    }

    /// Install asks GitHub, follows the newest release, and reuses that answer for
    /// [`PLAN_LISTING_REUSE`] — for a runner exactly as for a game's own integration.
    #[test]
    fn resolve_follows_the_newest_release_and_reuses_the_answer() {
        for s in [UNIVERSAL_RUNNER_UNREAL, ds(DS2)] {
            let cache = ReleaseCache::default();
            let f = Scripted::new(vec![Scripted::listing(&[
                release(&s, "v0.1.0", 1),
                release(&s, "v0.4.0", 4),
            ])]);
            let gi = resolve(&cache, &f, &s).unwrap();
            assert_eq!((gi.tag.as_str(), gi.source), ("v0.4.0", s));
            assert_ne!(
                Some(gi.clone()),
                fallback_gi(&s),
                "the release, not the pin"
            );
            // Asked again within the reuse window: the same answer, no second request.
            assert_eq!(resolve(&cache, &f, &s).unwrap(), gi);
            assert_eq!(f.asked(), 1);
            assert_eq!(cache.get(s.repo).map(|l| l.len()), Some(2));
        }
    }

    /// **With no listing and GitHub unreachable, the compiled pin installs**; with a listing it
    /// answered within the day, that listing's newest release does; with only an older one, the
    /// pin again.
    #[test]
    fn resolve_falls_back_to_the_compiled_pin_only_when_github_cannot_be_asked() {
        for status in [
            "403 Forbidden",
            "429 Too Many Requests",
            "503 Service Unavailable",
        ] {
            for s in all_sources() {
                let cache = ReleaseCache::default();
                let f = Scripted::new(vec![Err(refused(status))]);
                if s.fallback
                    .is_none_or(|p| p.prerelease && !FOLLOWS_PRERELEASES)
                {
                    // No pin, or a prerelease pin in a build that follows stable releases only:
                    // GitHub's own refusal stands, nothing is installed — and a successor's
                    // predecessor is not asked in its place (one request, and only its own).
                    let e = resolve(&cache, &f, &s).unwrap_err();
                    assert_eq!(e.params.get("repo").map(String::as_str), Some(s.repo));
                    assert_eq!(f.asked(), 1, "{}: {status}", s.name);
                    continue;
                }
                let gi = resolve(&cache, &f, &s).unwrap_or_else(|e| panic!("{}: {e:?}", s.name));
                assert_eq!(Some(gi), fallback_gi(&s), "{}: {status}", s.name);
            }
        }
        // Offline with no status at all (no connection): the pin too.
        let cache = ReleaseCache::default();
        let f = Scripted::new(vec![Err(LauncherError::new(
            astra_game_launcher::codes::FETCH_FAILED,
        ))]);
        assert_eq!(
            resolve(&cache, &f, &UNIVERSAL_RUNNER).ok(),
            fallback_gi(&UNIVERSAL_RUNNER)
        );
        assert!(
            !cache.first_fallback(UNIVERSAL_RUNNER.id),
            "said once, and only once"
        );

        // A listing GitHub answered an hour ago stands in for it, with GitHub's release.
        let s = UNIVERSAL_RUNNER_IL2CPP;
        let cache = ReleaseCache::default();
        cache.record(s.repo, Some(vec![release(&s, "v0.9.0", 9)]));
        cache.age_wall_clock(s.repo, Duration::from_secs(60 * 60));
        let f = Scripted::new(vec![Err(refused("429 Too Many Requests"))]);
        // Inside the reuse window by the monotonic clock, outside it by the wall clock: GitHub is
        // asked, refuses, and the hour-old listing answers.
        assert_eq!(resolve(&cache, &f, &s).unwrap().tag, "v0.9.0");
        assert_eq!(f.asked(), 1);
        // Older than a day: the pin, not the stale listing.
        cache.age_wall_clock(s.repo, FALLBACK_LISTING_MAX_AGE);
        let f = Scripted::new(vec![Err(refused("429 Too Many Requests"))]);
        assert_eq!(resolve(&cache, &f, &s).ok(), fallback_gi(&s));
        assert_eq!(FALLBACK_LISTING_MAX_AGE, Duration::from_secs(24 * 60 * 60));
    }

    /// A listing GitHub DID answer with no usable release is a refusal, never the fallback.
    #[test]
    fn a_listing_without_a_usable_release_is_refused_not_replaced_by_the_pin() {
        let s = UNIVERSAL_RUNNER;
        let cache = ReleaseCache::default();
        // A draft: never picked in any build, so this holds whichever side of the knob it runs.
        let mut draft = release(&s, "v0.9.0", 9);
        draft.draft = true;
        let f = Scripted::new(vec![Scripted::listing(&[draft])]);
        let e = resolve(&cache, &f, &s).unwrap_err();
        assert_eq!(e.code, RELEASE_NOT_FOUND);
        assert_eq!(
            e.params.get("reason").map(String::as_str),
            Some("no-release")
        );
    }

    #[test]
    fn the_cache_refreshes_once_per_ttl_and_keeps_a_listing_through_a_failure() {
        let s = ds(DS1);
        let listing = || vec![release(&s, "v0.2.0", 2), release(&s, "v0.1.0", 1)];
        let c = ReleaseCache::default();
        assert!(c.claim_refresh(DS_REPO, LISTING_TTL));
        assert!(
            !c.claim_refresh(DS_REPO, LISTING_TTL),
            "one refresh at a time"
        );
        assert!(c.record(DS_REPO, Some(listing())));
        c.finish(DS_REPO);
        assert!(!c.claim_refresh(DS_REPO, LISTING_TTL), "fresh");
        assert!(c.claim_refresh(DS_REPO, Duration::ZERO), "stale");
        assert!(!c.record(DS_REPO, None), "a failure changes nothing");
        c.finish(DS_REPO);
        assert_eq!(c.get(DS_REPO), Some(listing()));
        assert_eq!(c.get_fresh(DS_REPO, PLAN_LISTING_REUSE), Some(listing()));
        assert_eq!(
            c.get_fresh(DS_REPO, Duration::ZERO),
            None,
            "an old answer is not fresh"
        );
        // Suspended for a day: the monotonic clock did not move, the wall clock did.
        c.age_wall_clock(DS_REPO, Duration::from_secs(25 * 60 * 60));
        assert_eq!(
            c.get_fresh(DS_REPO, FALLBACK_LISTING_MAX_AGE),
            None,
            "both clocks must agree it is young"
        );
        assert_eq!(
            c.get(DS_REPO),
            Some(listing()),
            "still there, only not fresh"
        );
        assert_eq!(
            c.get("https://github.com/MihailinL/astra-in-death-stranding"),
            Some(listing()),
            "keyed by the normalised repository"
        );
        assert_eq!(pins::game_source(DS2).unwrap().repo, DS_REPO);
    }

    const TRILOGY_LISTING_PAGE_1: &str =
        "https://api.github.com/repositories/1427110216/releases?per_page=100&page=1";
    const RESHADE_LISTING_PAGE_1: &str =
        "https://api.github.com/repositories/1412685270/releases?per_page=100&page=1";

    fn reason(e: &LauncherError) -> Option<&str> {
        e.params.get("reason").map(String::as_str)
    }

    /// **GTA III installs from the trilogy once a release this build follows carries its zip, and
    /// from astra-reshade (its predecessor) until then** — through `resolve`, asked by each
    /// repository's id in lineage order. A trilogy prerelease is followed only with the knob; a
    /// trilogy that shipped and then got its newest release wrong is refused as the trilogy's, and
    /// an unreachable trilogy is never replaced by astra-reshade.
    #[test]
    fn gta3_installs_from_the_trilogy_once_it_ships_and_from_astra_reshade_until_then() {
        let gta3 = game_source("1546970").unwrap();
        let old = GTA3_ASTRA_RESHADE;
        let vc = game_source("1546990").unwrap();
        // A stable astra-reshade release carrying GTA III's former zip, numbered above anything
        // the trilogy publishes below: the lineage decides, never the version.
        let reshade = vec![release(&old, "v0.3.0", 0x30)];

        // Shipped: the trilogy, astra-reshade not asked.
        for knob in [false, true] {
            let cache = ReleaseCache::default();
            let f = Scripted::new(vec![Scripted::listing(&[release(&gta3, "v0.1.0", 0x01)])]);
            let gi = resolve_with(&cache, &f, &gta3, knob).unwrap();
            assert_eq!((gi.source, gi.tag.as_str()), (gta3, "v0.1.0"), "{knob}");
            assert!(
                gi.url
                    .starts_with("https://github.com/mihailinl/astra-gta-trilogy/")
            );
            assert_eq!(*f.asked.lock().unwrap(), vec![TRILOGY_LISTING_PAGE_1]);
        }

        // Not shipped: astra-reshade, asked second.
        let mut pre = release(&gta3, "v0.1.0-discovery", 0x0d);
        pre.prerelease = true;
        let mut draft = release(&gta3, "v0.1.0", 0x01);
        draft.draft = true;
        let others_only = release(&vc, "v0.2.0", 0x02);
        for (trilogy, knob, why) in [
            (vec![], false, "no release"),
            (vec![], true, "no release, knob on"),
            (
                vec![others_only.clone()],
                false,
                "a release of the other games only",
            ),
            (
                vec![others_only],
                true,
                "a release of the other games only, knob on",
            ),
            (vec![pre.clone()], false, "only a prerelease and no knob"),
            (vec![draft], true, "only a draft, whatever the knob"),
        ] {
            let cache = ReleaseCache::default();
            let f = Scripted::new(vec![
                Scripted::listing(&trilogy),
                Scripted::listing(&reshade),
            ]);
            let gi = resolve_with(&cache, &f, &gta3, knob).expect(why);
            assert_eq!((gi.source, gi.tag.as_str()), (old, "v0.3.0"), "{why}");
            assert!(
                gi.url
                    .ends_with("/astra-reshade/releases/download/v0.3.0/astra-gi-gta3.zip")
            );
            assert_eq!(
                *f.asked.lock().unwrap(),
                vec![TRILOGY_LISTING_PAGE_1, RESHADE_LISTING_PAGE_1],
                "{why}"
            );
        }

        // The same prerelease WITH the knob: the trilogy's, and astra-reshade is not asked.
        let cache = ReleaseCache::default();
        let f = Scripted::new(vec![Scripted::listing(std::slice::from_ref(&pre))]);
        let gi = resolve_with(&cache, &f, &gta3, true).unwrap();
        assert_eq!((gi.source, gi.version.as_str()), (gta3, "0.1.0-discovery"));
        assert_eq!(f.asked(), 1);

        // Shipped, then a newer release of Vice City alone: GTA III keeps the trilogy's release
        // that carries its zip, never handed down to astra-reshade.
        let cache = ReleaseCache::default();
        let f = Scripted::new(vec![Scripted::listing(&[
            release(&gta3, "v0.1.0", 0x01),
            release(&vc, "v0.2.0", 0x02),
        ])]);
        let gi = resolve_with(&cache, &f, &gta3, false).unwrap();
        assert_eq!((gi.source, gi.tag.as_str()), (gta3, "v0.1.0"));
        assert_eq!(f.asked(), 1);

        // The trilogy's GitHub cannot be asked: its own refusal (it has no pin), and astra-reshade
        // is not asked in its place.
        for knob in [false, true] {
            let cache = ReleaseCache::default();
            let f = Scripted::new(vec![Err(refused("503 Service Unavailable"))]);
            let e = resolve_with(&cache, &f, &gta3, knob).unwrap_err();
            assert_eq!(e.code, RELEASE_UNAVAILABLE);
            assert_eq!(e.params.get("repo").map(String::as_str), Some(gta3.repo));
            assert_eq!(f.asked(), 1);
        }
    }

    /// Vice City and San Andreas have no predecessor and no pin: with no release carrying their
    /// zip they are "not released yet" online whatever the knob, and GitHub's own refusal offline.
    #[test]
    fn vice_city_and_san_andreas_without_a_release_are_not_released_yet() {
        for appid in ["1546990", "1547000"] {
            let s = game_source(appid).unwrap();
            for knob in [false, true] {
                let cache = ReleaseCache::default();
                let f = Scripted::new(vec![Scripted::listing(&[])]);
                let e = resolve_with(&cache, &f, &s, knob).unwrap_err();
                assert_eq!(
                    (e.code, reason(&e)),
                    (RELEASE_NOT_FOUND, Some("no-release")),
                    "{appid} {knob}"
                );
                let cache = ReleaseCache::default();
                let f = Scripted::new(vec![Err(refused("503 Service Unavailable"))]);
                let e = resolve_with(&cache, &f, &s, knob).unwrap_err();
                assert_eq!(e.code, RELEASE_UNAVAILABLE, "{appid} {knob}");
                assert_eq!(f.asked(), 1);
            }
        }
    }

    /// The trilogy's REAL listing on 2026-10-20 (`gh api repositories/1427110216/releases`): one
    /// release, the GTA III discovery PRERELEASE `v0.1.0-discovery`, carrying only
    /// `astra-gi-gta3.zip` — beside astra-reshade's real newest prerelease (v0.2.4, which carries
    /// GTA III's former zip) and newest stable (v0.2.1, which does not). Without the knob GTA III
    /// stays on astra-reshade, exactly as before the move ("not released yet": `no-asset`, its
    /// newest stable release has no GTA III zip; the trilogy's verdict would be `no-release`). The
    /// owner's test build takes the discovery build — not astra-reshade's higher-numbered v0.2.4.
    /// Vice City and San Andreas are not released on either side. And the zip's REAL manifest
    /// (`unzip -p astra-gi-gta3.zip astra-gi.toml`, its `[integration]` and `[target]` verbatim),
    /// which names no `[target] engine`, passes GTA III's check: a game's own source is held to
    /// its appid, never to an engine.
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
                 "browser_download_url":"https://github.com/mihailinl/astra-reshade/releases/download/v0.2.1/astra-gi.zip"},
                {"name":"UE4SS_v3.0.1-1164-g5e627997.zip",
                 "digest":"sha256:3c72142a383e25e9f9553ac9e4d5677aa572e98cb369cac57200efb2e381e603",
                 "browser_download_url":"https://github.com/mihailinl/astra-reshade/releases/download/v0.2.1/UE4SS_v3.0.1-1164-g5e627997.zip"}]}]"#,
        )
        .unwrap();
        let gta3 = game_source("1546970").unwrap();
        let old = GTA3_ASTRA_RESHADE;
        let held = ReleaseCache::default();
        held.record(gta3.repo, Some(trilogy.clone()));
        held.record(old.repo, Some(reshade.clone()));
        // Both listings are fresh: `resolve` asks nothing.
        let never = Scripted::new(vec![]);

        // No knob: astra-reshade's verdict, as before the move.
        assert!(!has_shipped(&trilogy, &gta3, false));
        assert_eq!(pick_with(&trilogy, &gta3, false), Err(NoRelease::NoTag));
        assert_eq!(pick_with(&reshade, &old, false), Err(NoRelease::NoAsset));
        let e = resolve_with(&held, &never, &gta3, false).unwrap_err();
        assert_eq!((e.code, reason(&e)), (RELEASE_NOT_FOUND, Some("no-asset")));
        assert_eq!(pick_held(&held, &gta3, false), None);

        // The knob: the discovery build.
        assert!(has_shipped(&trilogy, &gta3, true));
        let gi = resolve_with(&held, &never, &gta3, true).unwrap();
        assert_eq!(
            (
                gi.source,
                gi.tag.as_str(),
                gi.version.as_str(),
                gi.sha256.as_str(),
                gi.url.as_str()
            ),
            (
                gta3,
                "v0.1.0-discovery",
                "0.1.0-discovery",
                "67e6a2a73b392943f99f49aa7733bba5399214bf7dc5fe4e5536cb31e3ba3948",
                "https://github.com/mihailinl/astra-gta-trilogy/releases/download/v0.1.0-discovery/astra-gi-gta3.zip"
            )
        );
        assert_eq!(pick_held(&held, &gta3, true), Some(gi.clone()));
        // Had the trilogy not shipped, the knob would have taken astra-reshade's v0.2.4.
        assert_eq!(pick_with(&reshade, &old, true).unwrap().tag, "v0.2.4");
        assert_eq!(never.asked(), 0);

        // Vice City and San Andreas: nothing in that release for them, on either side.
        for appid in ["1546990", "1547000"] {
            let s = game_source(appid).unwrap();
            for (knob, why) in [(false, "no-release"), (true, "no-asset")] {
                let e = resolve_with(&held, &never, &s, knob).unwrap_err();
                assert_eq!(
                    (e.code, reason(&e)),
                    (RELEASE_NOT_FOUND, Some(why)),
                    "{appid}"
                );
            }
        }

        // The zip's real manifest: no `[target] engine`, and it is GTA III's.
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
        // Not on another game, and not as astra-reshade's GTA III integration.
        assert!(check_manifest(&gi, "1546990", &real).is_err());
        let old_gi = pick_with(&reshade, &old, true).unwrap();
        let e = check_manifest(&old_gi, "1546970", &real).unwrap_err();
        assert_eq!(
            e.params.get("field").map(String::as_str),
            Some("integration.id")
        );
    }

    /// The Update question walks the same lineage over listings already held: nothing while the
    /// newest source's listing was never answered, the predecessor while the newest has not
    /// shipped, the newest once it has.
    #[test]
    fn pick_held_walks_the_lineage_and_needs_every_listing_it_reads() {
        let gta3 = game_source("1546970").unwrap();
        let old = GTA3_ASTRA_RESHADE;
        let held = ReleaseCache::default();
        held.record(old.repo, Some(vec![release(&old, "v0.3.0", 0x30)]));
        assert_eq!(
            pick_held(&held, &gta3, false),
            None,
            "the trilogy's listing was never answered"
        );
        held.record(gta3.repo, Some(vec![]));
        assert_eq!(pick_held(&held, &gta3, false).map(|g| g.source), Some(old));
        held.record(gta3.repo, Some(vec![release(&gta3, "v0.1.0", 0x01)]));
        assert_eq!(pick_held(&held, &gta3, false).map(|g| g.source), Some(gta3));
        // A source with no predecessor reads its own listing and no other.
        assert_eq!(pick_held(&held, &UNIVERSAL_RUNNER, false), None);
        assert_eq!(
            pick_held(&held, &old, false).map(|g| g.tag),
            Some("v0.3.0".to_owned())
        );
    }

    /// **One game's partial release neither refuses nor holds back a game it leaves out**: in a
    /// repository several sources share, each takes the newest release carrying ITS zip — the
    /// trilogy's three, and Death Stranding's two — on both sides of the knob, a prerelease still
    /// counting only with it. A source alone in its repository is unchanged: its newest release.
    #[test]
    fn a_shared_repositorys_source_takes_the_newest_release_carrying_its_zip() {
        let gta3 = game_source("1546970").unwrap();
        let vc = game_source("1546990").unwrap();
        let sa = game_source("1547000").unwrap();
        for s in [gta3, vc, sa, ds(DS1), ds(DS2)] {
            assert!(pins::shares_its_repository(&s), "{}", s.name);
        }
        // v0.1.0 carries all three; v0.2.0 GTA III and San Andreas; v0.3.0 GTA III alone.
        let listing = vec![
            release_of(&[gta3, vc, sa], "v0.1.0", 0x10),
            release_of(&[gta3, sa], "v0.2.0", 0x20),
            release_of(&[gta3], "v0.3.0", 0x30),
        ];
        for knob in [false, true] {
            let tag = |s: &GiSource| pick_with(&listing, s, knob).map(|gi| gi.tag);
            assert_eq!(tag(&gta3), Ok("v0.3.0".to_owned()));
            assert_eq!(tag(&sa), Ok("v0.2.0".to_owned()));
            assert_eq!(tag(&vc), Ok("v0.1.0".to_owned()));
            let gi = pick_with(&listing, &vc, knob).unwrap();
            assert!(gi.url.ends_with("/v0.1.0/astra-gi-vicecity.zip"));
            assert_eq!(gi.version, "0.1.0");
        }
        // A newer PRERELEASE with Vice City's zip: only the knob takes it.
        let mut pre = release_of(&[vc], "v0.4.0", 0x40);
        pre.prerelease = true;
        let with_pre = [listing.clone(), vec![pre]].concat();
        assert_eq!(pick_with(&with_pre, &vc, false).unwrap().tag, "v0.1.0");
        assert_eq!(pick_with(&with_pre, &vc, true).unwrap().tag, "v0.4.0");
        // No followed release carries it at all: `no-asset`; no followed release at all: `no-release`.
        let gta3_only = vec![release_of(&[gta3], "v0.1.0", 0x10)];
        assert_eq!(pick_with(&gta3_only, &vc, false), Err(NoRelease::NoAsset));
        assert_eq!(pick_with(&[], &vc, false), Err(NoRelease::NoTag));
        // Death Stranding: a DS2-only release leaves DS1 on the last one carrying its zip.
        let ds_listing = vec![
            release_of(&[ds(DS1), ds(DS2)], "v0.1.1", 0x11),
            release_of(&[ds(DS2)], "v0.1.2", 0x12),
        ];
        assert_eq!(pick(&ds_listing, &ds(DS1)).unwrap().tag, "v0.1.1");
        assert_eq!(pick(&ds_listing, &ds(DS2)).unwrap().tag, "v0.1.2");
        // A source alone in its repository: still its newest release, and a newest release
        // without its zip still refused (`a_release_without_a_trustworthy_asset_is_refused_not_skipped`).
        let lethal = game_source("1966720").unwrap();
        let lethal_listing = vec![release(&lethal, "v0.2.0", 2), release(&lethal, "v0.3.0", 3)];
        assert_eq!(pick(&lethal_listing, &lethal).unwrap().tag, "v0.3.0");
        // Exactly the four Unity games' own repositories are one source's alone.
        let alone: Vec<&str> = all_sources()
            .filter(|s| !pins::shares_its_repository(s))
            .map(|s| s.id)
            .collect();
        assert_eq!(
            alone,
            [
                "astra.lethal-company",
                "astra.content-warning",
                "astra.peak",
                "astra.repo"
            ]
        );
    }
}
