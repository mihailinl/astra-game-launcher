// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What Astra 0.2.7 holds a game integration's release to, as Astra 0.2.7 shipped it: the
//! functions of tag `v0.2.7` (commit `6fea01a01c1ca4209a967ff69d9afa065bdb22bb`),
//! `astra-rs/astra-daemon/src/games/{pins,releases,service}.rs`, copied here VERBATIM. Those three
//! files are vendored in `tests/fixtures/astra-0.2.7/` (their digests in its `SOURCE`), and
//! `tests/gi_probe.rs` holds every function and type below to its original, token for token
//! (comments and white space aside), and each doc's `file:lines` citation to where that item
//! sits there. A replica that drifts from 0.2.7 fails that test before it can pass a release
//! 0.2.7 would refuse.
//!
//! What is NOT 0.2.7's: the table. 0.2.7 compiles `pins.rs`'s sources in; [`pins::sources`]
//! reads the same rows from `compat-0.2.7.json` (canary K4), which the same test holds equal to
//! every `GiSource` and `FallbackPin` literal of the vendored `pins.rs`. `GiSource` here has no
//! `name` (nothing replicated reads it), and its strings are leaked once to be `&'static`, as the
//! compiled ones are.
//!
//! Launcher types are launcher 0.3.1's (`launcher_031`), the one Astra 0.2.7 links.

/// `pins.rs`: the GI sources and the two helpers the replicas read.
pub mod pins {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use serde::Deserialize;

    /// `pins.rs:50-77` (`GiSource`), without `name`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GiSource {
        pub id: &'static str,
        pub repo: &'static str,
        pub repo_id: u64,
        pub tag_prefix: &'static str,
        pub asset: &'static str,
        pub target: SourceTarget,
        pub fallback: Option<FallbackPin>,
        pub predecessor: Option<&'static GiSource>,
    }

    /// `pins.rs:82-100` (`SourceTarget`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SourceTarget {
        Runner {
            engine: &'static str,
        },
        Game {
            appid: &'static str,
            exe: Option<&'static str>,
        },
    }

    /// `pins.rs:114-118` (`FallbackPin`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FallbackPin {
        pub tag: &'static str,
        pub sha256: &'static str,
        pub prerelease: bool,
    }

    impl GiSource {
        /// `pins.rs:123-128`.
        pub const fn appid(&self) -> Option<&'static str> {
            match self.target {
                SourceTarget::Game { appid, .. } => Some(appid),
                SourceTarget::Runner { .. } => None,
            }
        }

        /// `pins.rs:132-137`.
        pub const fn exe(&self) -> Option<&'static str> {
            match self.target {
                SourceTarget::Game { exe, .. } => exe,
                SourceTarget::Runner { .. } => None,
            }
        }

        /// `pins.rs:141-143`.
        #[allow(dead_code)] // verbatim; the ported tests read it
        pub const fn is_runner(&self) -> bool {
            matches!(self.target, SourceTarget::Runner { .. })
        }

        /// `pins.rs:148-150`.
        #[allow(dead_code)] // verbatim; the ported tests read it
        pub fn lineage(&self) -> impl Iterator<Item = GiSource> + use<> {
            std::iter::successors(Some(*self), |s| s.predecessor.copied())
        }
    }

    /// Every source of the table, in its order: 0.2.7's `all_sources()` (`pins.rs:537-542`) yields
    /// the same set (the runners, then each game source's lineage), which is all
    /// [`shares_its_repository`] counts.
    pub fn all_sources() -> impl Iterator<Item = GiSource> {
        sources().iter().map(|s| **s)
    }

    /// `pins.rs:584-589`.
    pub fn shares_its_repository(source: &GiSource) -> bool {
        all_sources()
            .filter(|s| s.repo_id == source.repo_id)
            .nth(1)
            .is_some()
    }

    /// `pins.rs:621-635`.
    pub(crate) fn normalize_repo(repo: &str) -> String {
        let lower = repo.trim().to_ascii_lowercase();
        let s = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
            .unwrap_or(&lower);
        let s = s.strip_prefix("github.com/").unwrap_or(s);
        let s = s.trim_end_matches('/');
        let s = s.strip_suffix(".git").unwrap_or(s);
        s.trim_end_matches('/').to_string()
    }

    /// The source whose `[integration] id` is `id` (the table's ids are unique).
    pub fn source(id: &str) -> Option<GiSource> {
        all_sources().find(|s| s.id == id)
    }

    /// Where the table says it comes from (the report names it).
    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Provenance {
        pub tag: String,
        pub commit: String,
        pub file: String,
    }

    /// The launcher the table's Astra links.
    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Launcher {
        pub version: String,
        pub rev: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Table {
        schema: String,
        astra: Provenance,
        launcher: Launcher,
        sources: Vec<Row>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        id: String,
        repo: String,
        repo_id: u64,
        tag_prefix: String,
        asset: String,
        target: Target,
        fallback: Option<Pin>,
        predecessor: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "lowercase", deny_unknown_fields)]
    enum Target {
        Runner { engine: String },
        Game { appid: String, exe: Option<String> },
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pin {
        tag: String,
        sha256: String,
        prerelease: bool,
    }

    /// The K4 file: every source Astra 0.2.7 compiles in, and every fallback pin.
    pub const TABLE_JSON: &str = include_str!("compat-0.2.7.json");
    /// The schema [`TABLE_JSON`] is written in.
    pub const TABLE_SCHEMA: &str = "astra.gi-probe.compat-0.2.7/1";

    struct Loaded {
        astra: Provenance,
        launcher: Launcher,
        sources: Vec<&'static GiSource>,
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn loaded() -> &'static Loaded {
        static TABLE: OnceLock<Loaded> = OnceLock::new();
        TABLE.get_or_init(|| {
            // Compiled in, and held to the vendored pins.rs by a test: a table that does not
            // read is a broken build, not an input.
            let t: Table = serde_json::from_str(TABLE_JSON).expect("compat-0.2.7.json reads");
            assert_eq!(t.schema, TABLE_SCHEMA, "compat-0.2.7.json schema");
            let mut built: HashMap<String, &'static GiSource> = HashMap::new();
            let mut left: Vec<&Row> = t.sources.iter().collect();
            while !left.is_empty() {
                let before = left.len();
                left.retain(|r| {
                    let predecessor = match &r.predecessor {
                        None => None,
                        Some(p) => match built.get(p) {
                            Some(p) => Some(*p),
                            None => return true,
                        },
                    };
                    let target = match &r.target {
                        Target::Runner { engine } => SourceTarget::Runner {
                            engine: leak(engine.clone()),
                        },
                        Target::Game { appid, exe } => SourceTarget::Game {
                            appid: leak(appid.clone()),
                            exe: exe.clone().map(leak),
                        },
                    };
                    let s: &'static GiSource = Box::leak(Box::new(GiSource {
                        id: leak(r.id.clone()),
                        repo: leak(r.repo.clone()),
                        repo_id: r.repo_id,
                        tag_prefix: leak(r.tag_prefix.clone()),
                        asset: leak(r.asset.clone()),
                        target,
                        fallback: r.fallback.as_ref().map(|p| FallbackPin {
                            tag: leak(p.tag.clone()),
                            sha256: leak(p.sha256.clone()),
                            prerelease: p.prerelease,
                        }),
                        predecessor,
                    }));
                    assert!(
                        built.insert(r.id.clone(), s).is_none(),
                        "compat-0.2.7.json: {} twice",
                        r.id
                    );
                    false
                });
                assert!(
                    left.len() < before,
                    "compat-0.2.7.json: a predecessor that is no row, or a cycle"
                );
            }
            Loaded {
                astra: t.astra,
                launcher: t.launcher,
                sources: t.sources.iter().map(|r| built[&r.id]).collect(),
            }
        })
    }

    /// The table's sources, in its order.
    pub fn sources() -> &'static [&'static GiSource] {
        &loaded().sources
    }

    /// The Astra release the table is of.
    pub fn astra() -> &'static Provenance {
        &loaded().astra
    }

    /// The launcher that Astra release links.
    pub fn launcher() -> &'static Launcher {
        &loaded().launcher
    }
}

/// `releases.rs`: which release a source installs, and what its manifest must name.
pub mod releases {
    use launcher_031::{LauncherError, Manifest};
    use serde::Deserialize;

    use super::pins::{GiSource, SourceTarget, normalize_repo};

    /// `releases.rs:78`.
    pub const RELEASE_NOT_FOUND: &str = "release-not-found";
    /// `releases.rs:88`.
    pub const RELEASE_MISMATCH: &str = "release-mismatch";

    /// `releases.rs:116`, as every release build of 0.2.7 has it: `ASTRA_GAMES_PRERELEASES` is an
    /// owner test-build knob, and a build with it set cannot pass 0.2.7's release scan.
    pub const FOLLOWS_PRERELEASES: bool = false;

    /// `releases.rs:133-141`.
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

    /// `releases.rs:144-150`.
    #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
    pub struct GhAsset {
        pub name: String,
        pub browser_download_url: String,
        #[serde(default)]
        pub digest: Option<String>,
    }

    /// `releases.rs:155-164`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Gi {
        pub source: GiSource,
        pub tag: String,
        pub version: String,
        pub sha256: String,
        pub url: String,
    }

    /// `releases.rs:168-177`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NoRelease {
        NoTag,
        NoAsset,
        NoDigest,
        UrlMismatch,
    }

    /// `releases.rs:179-192`.
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

    /// `releases.rs:197-206`: the version a tag carries for `prefix` — the prefix (ASCII case
    /// ignored), an optional `v`, then semver; a pre-release only when `prereleases`.
    pub fn version_after(tag: &str, prefix: &str, prereleases: bool) -> Option<semver::Version> {
        let head = tag.get(..prefix.len())?;
        if !head.eq_ignore_ascii_case(prefix) {
            return None;
        }
        let rest = &tag[prefix.len()..];
        let rest = rest.strip_prefix('v').unwrap_or(rest);
        let v = semver::Version::parse(rest).ok()?;
        (prereleases || v.pre.is_empty()).then_some(v)
    }

    /// `releases.rs:209-211`.
    pub fn canonical_url(repo: &str, tag: &str, asset: &str) -> String {
        format!("https://github.com/{repo}/releases/download/{tag}/{asset}")
    }

    /// `releases.rs:216-226`.
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

    /// `releases.rs:234-236`: what a 0.2.7 release build installs out of `releases`.
    #[allow(dead_code)] // verbatim; the ported tests read it
    pub fn pick(releases: &[GhRelease], source: &GiSource) -> Result<Gi, NoRelease> {
        pick_with(releases, source, FOLLOWS_PRERELEASES)
    }

    /// `releases.rs:246-288`.
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

    /// `releases.rs:291-293`.
    fn carries(release: &GhRelease, source: &GiSource) -> bool {
        release.assets.iter().any(|a| a.name == source.asset)
    }

    /// `releases.rs:346-359`.
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

    /// `releases.rs:366-413`: the package must be its source's integration — the source's id and
    /// repository, the version its tag carries, and a runner's `[target] engine` with no appid or
    /// a game's own appid.
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
                if got_appid != appid || own != appid {
                    return Err(mismatch("target.steam_appid", appid, &got_appid));
                }
            }
        }
        let version = m.integration.version.trim();
        if version.strip_prefix('v').unwrap_or(version) != gi.version {
            return Err(mismatch("integration.version", &gi.version, version));
        }
        Ok(())
    }
}

/// `service.rs`: whether an update asks for consent again.
pub mod service {
    use std::collections::BTreeMap;

    use launcher_031::{GameAction, GameFileAction, InstallPlan, Ledger, LedgerGameFile, Manifest};

    /// `service.rs:995-1023`. `true` for a fresh install, for an unread VAC flag, and whenever
    /// the integration moved, its license or authors changed, its game files differ
    /// ([`game_files_match`]), or its `[target]`, `[launch]` or `[[config_writes]]` changed.
    pub fn consent_needed_for(
        existing: Option<&Ledger>,
        plan: &InstallPlan,
        new: &Manifest,
    ) -> bool {
        if !plan.vac_checked {
            return true;
        }
        let Some(ledger) = existing else {
            return true;
        };
        if ledger.integration.id != new.integration.id
            || !same_repo(
                ledger.integration.repo.as_deref(),
                new.integration.repo.as_deref(),
            )
        {
            return true;
        }
        if ledger.integration.license != new.integration.license
            || ledger.integration.authors != new.integration.authors
        {
            return true;
        }
        if !game_files_match(&ledger.game_files, &plan.game_files) {
            return true;
        }
        let Ok(old) = Manifest::parse(&ledger.manifest) else {
            return true;
        };
        old.target != new.target
            || old.launch != new.launch
            || old.config_writes != new.config_writes
    }

    /// `service.rs:1031-1048`.
    pub fn game_files_match(old: &[LedgerGameFile], new: &[GameFileAction]) -> bool {
        let old_map = old
            .iter()
            .map(|f| (f.rel.as_str(), (f.action, f.sha256.as_str())))
            .collect::<BTreeMap<_, _>>();
        let new_map = new
            .iter()
            .map(|f| (f.rel_path.as_str(), (f.action, f.sha256.as_str())))
            .collect::<BTreeMap<_, _>>();
        old_map.len() == new_map.len()
            && old_map.iter().all(|(rel, old)| {
                new_map
                    .get(rel)
                    .is_some_and(|new| same_consented_file(*old, *new))
            })
    }

    /// `service.rs:1059-1065`.
    fn same_consented_file(old: (GameAction, &str), new: (GameAction, &str)) -> bool {
        use GameAction::{ReuseKnownProxy, ShareLoader};
        match (old.0, new.0) {
            (ReuseKnownProxy, ShareLoader) | (ShareLoader, ReuseKnownProxy) => true,
            (a, b) => a == b && compared_digest(a, old.1) == compared_digest(b, new.1),
        }
    }

    /// `service.rs:1069-1082`.
    pub fn same_repo(old: Option<&str>, new: Option<&str>) -> bool {
        let norm = |repo: &str| {
            let repo = repo.trim().trim_end_matches('/');
            repo.strip_suffix(".git")
                .unwrap_or(repo)
                .to_ascii_lowercase()
        };
        match (old, new) {
            (Some(old), Some(new)) if !old.trim().is_empty() && !new.trim().is_empty() => {
                norm(old) == norm(new)
            }
            _ => true,
        }
    }

    /// `service.rs:1103-1108`.
    fn compared_digest(action: GameAction, sha256: &str) -> &str {
        match action {
            GameAction::KeepExisting | GameAction::ShareLoader => "",
            _ => sha256,
        }
    }
}
