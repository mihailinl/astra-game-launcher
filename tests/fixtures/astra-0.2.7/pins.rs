//! The GI sources — a const table of WHERE each GI comes from: a repository, its release asset,
//! and what the release's manifest must name. Never a network decision on its own.
//!
//! **Every GI follows its repository's newest stable GitHub release** (the owner's decision of
//! 2026-10-17; `games::releases` resolves it and says exactly what is and is not verified). A
//! source whose repository has a reviewed release keeps a compiled [`FallbackPin`] — the release
//! this table last reviewed — which Install uses only when GitHub cannot be asked and no listing it
//! answered is recent enough. It is never an Update target: an Update needs a listing. A source
//! whose repository has no release yet has no pin (`fallback: None`): it installs nothing offline,
//! and says "not released yet" until GitHub lists a release carrying its asset.
//!
//! Two kinds of source, one type ([`GiSource`], told apart by its [`SourceTarget`]):
//! - The universal runners, one per [`Universal`] flavour: [`UNIVERSAL_RUNNER`], the one GI
//!   every Unity Mono game without its own integration gets (built by `astra-bepinex`, generic
//!   BepInEx/Doorstop only); [`UNIVERSAL_RUNNER_IL2CPP`] beside it for Unity IL2CPP; and
//!   [`UNIVERSAL_RUNNER_UNREAL`] for every Unreal game (built by `astra-reshade`). A runner's
//!   manifest names an engine and no appid.
//! - [`GAME_SOURCES`]: nine games with their OWN integration, each built by its repo's CI.
//!   Four are Unity Mono games, where Install still offers the universal runner as a secondary
//!   action (`use_universal_runner` on the gRPC request) — some player might have a reason to
//!   prefer the generic one, and that choice then follows the runner's source. Five name their
//!   program and have no alternative: Death Stranding 1 and 2 (not Unity) and the three games of
//!   GTA: The Trilogy – The Definitive Edition (Unreal, but each integration carries game-specific
//!   UE4SS overrides, so for those appids it wins over the generic Unreal runner).
//!
//! **A game's source can SUCCEED an older one** ([`GiSource::predecessor`]): GTA III's integration
//! moved from `astra-reshade` ([`GTA3_ASTRA_RESHADE`]) to `astra-gta-trilogy` (the owner's decision,
//! 2026-10-20). Install walks a game's [`GiSource::lineage`] newest first and takes the first source
//! that has SHIPPED (some release this build follows carries its asset): until the new repository
//! ships GTA III, GTA III installs what it did before; from then on the new one, and an installed
//! copy of the old one is offered the new one as its Update (`games::service::update_for`), which
//! replaces it in the game's one profile. The one difference before it ships: the predecessor
//! stands in only for a successor GitHub ANSWERED for, so with the trilogy unreachable an owner
//! test build no longer installs astra-reshade's prerelease pin offline (a release build never
//! did: that pin is a prerelease).
//!
//! `repo_id` is load-bearing: the releases listing is asked BY it (`releases::listing_url`), so a
//! renamed, transferred or deleted-and-recreated repository of the same name is not followed
//! (a renamed one then fails closed at `releases::pick`'s canonical-URL check). Verified against
//! `gh api repos/<repo> --jq .id` for seven repositories on 2026-10-17, and for
//! `astra-gta-trilogy` on 2026-10-20.
//!
//! Always compiled, unlike the rest of `games::` — this is plain data with no dependency on the
//! launcher crate, so there is nothing for the `games` build feature to gate here.

/// One GI's source: the repository whose newest stable release it installs, and what that
/// release's manifest must name (`releases::check_manifest`).
#[cfg_attr(not(astra_games), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GiSource {
    /// For the UI: "Astra for Lethal Company" / "Astra for any Unity game".
    pub name: &'static str,
    /// The `[integration] id` every release of this source must carry (e.g.
    /// `"astra.unity-il2cpp"`) — also what a ledger's `integration.id` is compared against in
    /// [`source_for_ledger`], primarily to tell the two `astra-bepinex` runners apart (they share
    /// one `repo`).
    pub id: &'static str,
    /// "owner/repo" — the GitHub repository this source's releases belong to. A real manifest's
    /// `[integration].repo` is the FULL URL (`"https://github.com/owner/repo"`), never this
    /// shorthand — [`normalize_repo`] brings both to one shape before any comparison.
    pub repo: &'static str,
    /// GitHub's own numeric repository id: the releases listing is asked by it (module doc).
    pub repo_id: u64,
    /// The tag prefix of this source's releases: `"v"` everywhere today (`v0.8.1`).
    pub tag_prefix: &'static str,
    /// The release asset that is the GI zip. Sources sharing a repository differ here (the Mono
    /// and IL2CPP runners; Death Stranding 1 and 2), which is what keeps each on its own zip.
    pub asset: &'static str,
    /// A runner (an engine, no appid) or one game's own integration (its appid).
    pub target: SourceTarget,
    /// The release Install uses when GitHub cannot be asked (module doc); `None` for a source
    /// whose repository has no reviewed release yet, which then installs nothing offline.
    pub fallback: Option<FallbackPin>,
    /// The source this one SUCCEEDS for the same game (module doc), tried when this one has not
    /// shipped yet; `None` for every source but one that took a game over from another.
    pub predecessor: Option<&'static GiSource>,
}

/// What a source's releases are FOR, which is what their manifests must name.
#[cfg_attr(not(astra_games), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTarget {
    /// A universal runner: any game of one engine. Its manifest names NO appid, and its
    /// `[target] engine` is exactly this code (the launcher's `Engine::code`: `"unity-mono"`,
    /// `"unity-il2cpp"`, `"unreal"`).
    Runner { engine: &'static str },
    /// One game's own integration: its manifest names this Steam appid.
    Game {
        appid: &'static str,
        /// The program, relative to the game's folder with `/` between folders (its manifest's
        /// `[target] exe`), for a game whose integration names its program instead of an
        /// engine: engine detection does not choose its program, the engine rule does not apply
        /// to it, and no universal runner stands in for it (Death Stranding is not Unity; GTA
        /// III's integration would break as the generic runner and the generic runner would
        /// miss its overrides). `None` for a Unity Mono game, whose program detection finds and
        /// for which the universal Mono runner is the alternative (every Unity game's own
        /// integration is a Mono build).
        exe: Option<&'static str>,
    },
}

/// A source's compiled release: the bytes this table last reviewed, bound by `sha256` exactly
/// and downloaded from the source's canonical URL (`releases::fallback_gi`). Used only when
/// GitHub cannot be asked; the manifest is still held to the source.
///
/// `prerelease` is GitHub's own flag on that release when it was pinned. A build that follows only
/// STABLE releases (every release build; `releases::FOLLOWS_PRERELEASES` is a test-build knob)
/// never installs a prerelease ONLINE, so it must not install one through the back door either:
/// such a pin is no fallback there (`releases::fallback_gi`). The owner's rule (2026-10-19) is that
/// integrations come from GitHub releases and are not repinned, so a pin promoted to stable later
/// stays marked here, which only costs that source its offline fallback.
#[cfg_attr(not(astra_games), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackPin {
    pub tag: &'static str,
    pub sha256: &'static str,
    pub prerelease: bool,
}

impl GiSource {
    /// The Steam appid this source's integration names; `None` for a runner.
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub const fn appid(&self) -> Option<&'static str> {
        match self.target {
            SourceTarget::Game { appid, .. } => Some(appid),
            SourceTarget::Runner { .. } => None,
        }
    }

    /// The program a non-Unity game's source names ([`SourceTarget::Game`]'s `exe`).
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub const fn exe(&self) -> Option<&'static str> {
        match self.target {
            SourceTarget::Game { exe, .. } => exe,
            SourceTarget::Runner { .. } => None,
        }
    }

    /// Whether this is a universal runner.
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub const fn is_runner(&self) -> bool {
        matches!(self.target, SourceTarget::Runner { .. })
    }

    /// This source, then each source it succeeds ([`Self::predecessor`]), newest first: the order
    /// Install tries them in (`releases::resolve`).
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub fn lineage(&self) -> impl Iterator<Item = GiSource> + use<> {
        std::iter::successors(Some(*self), |s| s.predecessor.copied())
    }

    /// Whether `older` is in this source's lineage AFTER it: a source this one took over from.
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub fn succeeds(&self, older: &GiSource) -> bool {
        self.lineage().skip(1).any(|s| s == *older)
    }
}

/// The GI every Unity Mono game without its own integration installs: generic
/// BepInEx/Doorstop, nothing game-specific.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub const UNIVERSAL_RUNNER: GiSource = GiSource {
    name: "Astra for any Unity game",
    // Real string, `runner/astra-gi.toml` in astra-bepinex (verified at v0.8.1, in the zip itself).
    id: "astra.unity",
    repo: "mihailinl/astra-bepinex",
    repo_id: 1_408_152_993,
    tag_prefix: "v",
    asset: "astra-gi.zip",
    target: SourceTarget::Runner {
        engine: "unity-mono",
    },
    // v0.5.5 (2026-10-09) keeps her on the ground (she fell 253 m in Extermination Ship) and
    // renders above 720p. v0.5.4 survives a stripped TerrainCollider (MiSide's first level). v0.5.3
    // draws her on the screen camera, not a monitor camera's texture
    // (Extermination Ship), and ships a Unity 2021 shader (MiSide). v0.5.2 also asks URP for a depth texture on the MAIN camera, which made her
    // invisible in URP games such as Extermination Ship. v0.5.1 found the camera that renders to the screen and the player
    // (Player tag → the camera's own body → the nearest CharacterController or humanoid) in
    // untagged games, which the "a Unity game nobody tried" case needs. v0.7.0 (2026-10-15): her
    // depth pass (particles and glass in front of her cover her where the game allows; games with
    // their own effects there, like ULTRAKILL, keep the 0.6.1 point), Unity 2020 games, a clearer
    // `her:` diagnosis line; live-checked in ULTRAKILL, REPO, MiSide, Extermination Ship,
    // Schedule I and PEAK, and it needs no engine past 0c64f6a. v0.8.0 (2026-10-15) adds
    // game-animation capture: an integration records the player's own animation into a local
    // .vrma for the engine's capture inbox, off by default (the engine UI window's Games section),
    // and a pack holding a captured clip cannot be exported. Backward compatible with 0.7.x
    // integrations. v0.8.1 (2026-10-16) replaces 0.8.0, which went BLACK in Unity 6.3 URP games
    // (PEAK, 6000.3.15: the URP 17 adapter threw a MissingMethodException every frame and URP
    // dropped the camera's graph): one URP 17 adapter for every Unity 6 line, and a guard that
    // proves a URP 17 / URP 14 / HDRP pass binds before injecting it — one that does not is left
    // out and she is drawn after the camera, so the game never goes black. Digest and size
    // re-checked against the downloaded release asset (829,750 B); still the newest stable
    // release on 2026-10-17.
    fallback: Some(FallbackPin {
        tag: "v0.8.1",
        sha256: "1ea3ef142e718afeffc9bbbc9dfab12e04a14d657d97524d9e7ba2225789a687",
        prerelease: false,
    }),
    predecessor: None,
};

/// The IL2CPP runner (2026-10-08, experimental; v0.5.3 also hides BepInEx 6's console through a
/// second `[[config_writes]]` into `BepInEx.cfg`, which the launcher creates when missing): BepInEx 6 be.788 with its `dotnet/` runtime and
/// the IL2CPP foundation flavour. Its winhttp.dll is the same Doorstop 4.5.0 proxy as the Mono
/// runner's. The first start of a game is slow, because BepInEx generates interop assemblies into
/// the profile (and may fetch Unity's base libraries from unity.bepinex.dev). Same repository and
/// releases as [`UNIVERSAL_RUNNER`], its own asset.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub const UNIVERSAL_RUNNER_IL2CPP: GiSource = GiSource {
    name: "Astra for any Unity game (IL2CPP, experimental)",
    // Real string, `runner/il2cpp/astra-gi.toml` in astra-bepinex (verified at v0.8.1, in the zip).
    id: "astra.unity-il2cpp",
    repo: "mihailinl/astra-bepinex",
    repo_id: 1_408_152_993,
    tag_prefix: "v",
    asset: "astra-gi-il2cpp.zip",
    target: SourceTarget::Runner {
        engine: "unity-il2cpp",
    },
    // v0.8.1 (the Unity 6 URP fix above): digest and size (34,605,498 B) re-checked against the
    // downloaded release asset.
    fallback: Some(FallbackPin {
        tag: "v0.8.1",
        sha256: "42c8e2b1bddcd5e62d9f87eaccc78c6beed9d7306028ea4706a50c2a08a7d20e",
        prerelease: false,
    }),
    predecessor: None,
};

/// The first Unity `(major, minor)` the IL2CPP runner above must NOT be installed on: BepInEx
/// be.788 / Il2CppInterop 1.5.3 crash those games at boot, before anything of ours loads
/// (Il2CppInterop issues #283 and #274; seen on the engine agent's 6000.3 testbed). Raise it
/// only with a newer interop released AND tested on such a game in staging (a prerelease is the
/// way to stage one: it is never picked). Mono games are not affected.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub const IL2CPP_RUNNER_REFUSES_FROM_UNITY: (u32, u32) = (6000, 3);

/// The Unreal runner (2026-10-15, experimental): `astra-reshade`'s `runners/unreal/astra-gi.toml`,
/// "Astra for any Unreal Engine game". Two upstream loaders and Astra's two pieces, all in the
/// game's own process: UE4SS's `dwmapi.dll` proxy and ReShade's `dxgi.dll` proxy beside the
/// real program (`<Project>/Binaries/Win64/<Project>-Win64-Shipping.exe` — the launcher's
/// `${game}` is that folder, never the root stub Steam starts), UE4SS itself and the AstraUE
/// Lua mod in the profile, and the `astra.addon64` ReShade add-on that composites her with the
/// game's depth. A runner, not a by-name GI: its manifest names no appid (`exe = "*"`) and
/// `[target] engine = "unreal"`, which the launcher checks against the chosen program at plan
/// time. Every Unreal game takes it except one whose own source names its program (the three
/// games of GTA: The Trilogy – The Definitive Edition, [`GAME_SOURCES`]), which wins for that
/// appid ([`source_for`]).
///
/// Fallback **v0.2.1** (2026-10-16), after the owner's first live session in Scarlet Skips: no
/// twitch while the camera is still, no white edge pixels, no GPU wait while she is absent, and
/// the add-on names its picture size to the engine (hello `want`; engine 0.2.2 sizes its frame ring
/// to it, an older engine still sends 1920x1080). The digest and size (11,716,114 B; manifest
/// `astra.unreal` 0.2.1, `engine = "unreal"`, `exe = "*"`) were re-checked against the downloaded
/// release asset.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub const UNIVERSAL_RUNNER_UNREAL: GiSource = GiSource {
    name: "Astra for any Unreal game (experimental)",
    // Real string, the `astra-gi.toml` inside the v0.2.1 release zip:
    // `id = "astra.unreal"`, `repo = "https://github.com/mihailinl/astra-reshade"`.
    id: "astra.unreal",
    repo: "mihailinl/astra-reshade",
    repo_id: 1_412_685_270,
    tag_prefix: "v",
    asset: "astra-gi.zip",
    target: SourceTarget::Runner { engine: "unreal" },
    fallback: Some(FallbackPin {
        tag: "v0.2.1",
        sha256: "e89fcd792fe53816dd47bf6f7cafd436cd59b1c766d6dba5aeb853ac1a1c4eea",
        prerelease: false,
    }),
    predecessor: None,
};

/// Which universal runner a game takes, by its engine. Plain data, so this module stays free of
/// the launcher crate: `games::service::universal_flavour` maps a detected `Engine` onto it.
#[cfg_attr(not(astra_games), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Universal {
    /// Unity Mono — and the only flavour a Unity game's own integration can stand in for.
    UnityMono,
    UnityIl2cpp,
    Unreal,
}

impl Universal {
    /// This flavour's universal runner.
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub const fn runner(self) -> GiSource {
        match self {
            Universal::UnityMono => UNIVERSAL_RUNNER,
            Universal::UnityIl2cpp => UNIVERSAL_RUNNER_IL2CPP,
            Universal::Unreal => UNIVERSAL_RUNNER_UNREAL,
        }
    }

    /// Every flavour, for the ledger's reverse lookup and the tests.
    #[cfg_attr(not(astra_games), allow(dead_code))]
    pub const ALL: [Universal; 3] = [
        Universal::UnityMono,
        Universal::UnityIl2cpp,
        Universal::Unreal,
    ];
}

/// GTA III – The Definitive Edition's integration as it was until the trilogy repository took the
/// game over: astra-reshade's per-game zip, the `astra-gi-gta3.zip` beside the generic runner's
/// `astra-gi.zip` in the same releases. Not in [`GAME_SOURCES`]: it is the PREDECESSOR of GTA III's
/// source there, which Install falls back to while `astra-gta-trilogy` has not shipped GTA III
/// (module doc), and the source a copy installed from it is matched to ([`source_for_ledger`]).
///
/// Fallback v0.2.2 is a PRERELEASE (11,719,479 B; the digest re-checked against the downloaded
/// asset, its manifest `astra.unreal.gta3-definitive-edition` 0.2.2, `engine = "unreal"`, appid
/// 1546970, this exe): no stable release carries this asset yet. So it is pinned
/// `prerelease: true`: a release build (stable releases only) never takes it as a fallback, and
/// until a stable release carries `astra-gi-gta3.zip` it says "not released yet"
/// (`release-not-found` / `no-asset`) whether or not GitHub answers. Only a test build compiled
/// with `ASTRA_GAMES_PRERELEASES=1` falls back to it.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub const GTA3_ASTRA_RESHADE: GiSource = GiSource {
    name: "Astra for GTA III - The Definitive Edition (astra-reshade, experimental)",
    id: "astra.unreal.gta3-definitive-edition",
    repo: "mihailinl/astra-reshade",
    repo_id: 1_412_685_270,
    tag_prefix: "v",
    asset: "astra-gi-gta3.zip",
    target: SourceTarget::Game {
        appid: "1546970",
        exe: Some("Gameface/Binaries/Win64/LibertyCity.exe"),
    },
    fallback: Some(FallbackPin {
        tag: "v0.2.2",
        sha256: "ff49114b2108193ca6f009e07cce679793438f959f797c30cc8968c06b873017",
        prerelease: true,
    }),
    predecessor: None,
};

/// The games with their OWN integration, keyed by the Steam appid in their target (as a string —
/// `GameRow::appid` is a string throughout this module, never parsed back to an integer). Any
/// other game uses the universal runner of its engine.
///
/// Every fallback below is the repository's newest stable release on 2026-10-17 (`gh api`), and
/// every fallback zip's manifest was read: its version is its tag's, its id and repository are the
/// source's, and it names the source's appid. The three GTA sources have none (no release yet).
#[cfg_attr(not(astra_games), allow(dead_code))]
pub const GAME_SOURCES: &[GiSource] = &[
    GiSource {
        name: "Astra for Lethal Company",
        // Real string, astra-lethal's `astra-gi.toml` (verified at v0.2.0).
        id: "astra.lethal-company",
        repo: "mihailinl/astra-lethal",
        repo_id: 1_409_552_539,
        tag_prefix: "v",
        asset: "astra-gi.zip",
        target: SourceTarget::Game {
            appid: "1966720",
            exe: None,
        },
        fallback: Some(FallbackPin {
            tag: "v0.2.0",
            sha256: "08b6683b447dc37e96a35632287cdbbae8f9f9733dbf2aaa488e6b1213544178",
            prerelease: false,
        }),
        predecessor: None,
    },
    GiSource {
        name: "Astra for Content Warning",
        // Real string, astra-content-warning's `astra-gi.toml` (verified at v0.2.0).
        id: "astra.content-warning",
        repo: "mihailinl/astra-content-warning",
        repo_id: 1_409_552_609,
        tag_prefix: "v",
        asset: "astra-gi.zip",
        target: SourceTarget::Game {
            appid: "2881650",
            exe: None,
        },
        fallback: Some(FallbackPin {
            tag: "v0.2.0",
            sha256: "bbdc6559904e17e1c4ac97aae021e96402abca78eea76ca651c91cd92fc18a38",
            prerelease: false,
        }),
        predecessor: None,
    },
    GiSource {
        name: "Astra for PEAK",
        // Real string, astra-peak's `astra-gi.toml` (verified at v0.3.1, in the zip itself).
        id: "astra.peak",
        repo: "mihailinl/astra-peak",
        repo_id: 1_409_552_680,
        tag_prefix: "v",
        asset: "astra-gi.zip",
        target: SourceTarget::Game {
            appid: "3527290",
            exe: None,
        },
        // v0.3.1 (2026-10-16): bundles foundation 0.8.1 (its astra-item requires >= 0.8.1),
        // which fixed v0.3.0's black screen on PEAK's Unity 6000.3 URP; keeps 0.3.0's
        // player-animation capture (climbing), off by default. 837,209 B; the digest was
        // re-checked against the downloaded release asset.
        fallback: Some(FallbackPin {
            tag: "v0.3.1",
            sha256: "0fc9fe9443f1940517f185e260e02176439e08d1f5b18891496971fa7e23b73d",
            prerelease: false,
        }),
        predecessor: None,
    },
    GiSource {
        name: "Astra for R.E.P.O.",
        // Real string, astra-repo's `astra-gi.toml` (verified at v0.2.0).
        id: "astra.repo",
        repo: "mihailinl/astra-repo",
        repo_id: 1_409_552_749,
        tag_prefix: "v",
        asset: "astra-gi.zip",
        target: SourceTarget::Game {
            appid: "3241660",
            exe: None,
        },
        fallback: Some(FallbackPin {
            tag: "v0.2.0",
            sha256: "dd069578b7b2a3ccf7756e9054041a2b985800956296a840a1106a11f0827296",
            prerelease: false,
        }),
        predecessor: None,
    },
    // Death Stranding Director's Cut and Death Stranding 2, both from
    // `mihailinl/astra-in-death-stranding` (ReShade plus Astra's add-ons, a `dxgi.dll` proxy).
    // The ids, exes and appids are the repository's `gi/ds1/` and `gi/ds2/` manifests'. ONE `v*`
    // tag carries both games' zips, so each follows its own asset of the same release.
    //
    // Fallback v0.1.1 (2026-10-17, experimental, built on astra-reshade v0.2.1): 0.1.1 re-finds
    // the compositor whenever its module changes (ReShade reloads add-ons when DS2 recreates its
    // device at start-up; 0.1.0 latched the first one and she could stay away all session). The
    // digests and sizes were re-checked against the downloaded release assets (DS1 2,845,046 B,
    // DS2 2,845,056 B). Steam must be running: both games exit without SteamAPI_Init, which
    // `steam -applaunch` provides.
    GiSource {
        name: "Astra in Death Stranding Director's Cut",
        id: "astra.death-stranding",
        repo: "mihailinl/astra-in-death-stranding",
        repo_id: 1_405_082_830,
        tag_prefix: "v",
        asset: "astra-gi-ds1.zip",
        target: SourceTarget::Game {
            appid: "1850570",
            exe: Some("ds.exe"),
        },
        fallback: Some(FallbackPin {
            tag: "v0.1.1",
            sha256: "7b88de52cecaec665fa9683eaa1b1e332497668b51b7760e4dea49dbec9ef71c",
            prerelease: false,
        }),
        predecessor: None,
    },
    GiSource {
        name: "Astra in Death Stranding 2: On the Beach",
        id: "astra.death-stranding-2",
        repo: "mihailinl/astra-in-death-stranding",
        repo_id: 1_405_082_830,
        tag_prefix: "v",
        asset: "astra-gi-ds2.zip",
        target: SourceTarget::Game {
            appid: "3280350",
            exe: Some("DS2.exe"),
        },
        fallback: Some(FallbackPin {
            tag: "v0.1.1",
            sha256: "9b2728ff1a06cdef01465003d38486cd23e99f87b32a304489653aa6c0d26479",
            prerelease: false,
        }),
        predecessor: None,
    },
    // GTA: The Trilogy – The Definitive Edition (Unreal 4 "Gameface", Windows-only, run under
    // Proton on Linux), all three from `mihailinl/astra-gta-trilogy` (the owner's mapping,
    // 2026-10-20): like Death Stranding, ONE `v*` tag carries every game's zip, so each follows
    // its own asset of the same release. Each carries UE4SS signature overrides that would break
    // any other Unreal game, so each is its game's own source and not the runner: for its appid it
    // wins over UNIVERSAL_RUNNER_UNREAL, and its program is the one named here. The ids, assets
    // and programs are the engine session's; the appids were checked against the Steam store
    // (`store.steampowered.com/api/appdetails`) on 2026-10-20.
    //
    // The repository had no release when this was written (its first is a GTA III prerelease),
    // so none of the three has a pin: each says "not released yet" (`release-not-found`) until a
    // release this build follows carries its asset, and installs nothing offline. GTA III alone
    // has somewhere to go meanwhile: its integration used to be astra-reshade's, which stays its
    // PREDECESSOR (GTA3_ASTRA_RESHADE), so until the trilogy ships GTA III it installs exactly what
    // it did before.
    GiSource {
        name: "Astra for GTA III - The Definitive Edition (experimental)",
        id: "astra.gta-trilogy.gta3",
        repo: "mihailinl/astra-gta-trilogy",
        repo_id: 1_427_110_216,
        tag_prefix: "v",
        asset: "astra-gi-gta3.zip",
        target: SourceTarget::Game {
            appid: "1546970",
            exe: Some("Gameface/Binaries/Win64/LibertyCity.exe"),
        },
        fallback: None,
        predecessor: Some(&GTA3_ASTRA_RESHADE),
    },
    GiSource {
        name: "Astra for GTA: Vice City - The Definitive Edition (experimental)",
        id: "astra.gta-trilogy.vicecity",
        repo: "mihailinl/astra-gta-trilogy",
        repo_id: 1_427_110_216,
        tag_prefix: "v",
        asset: "astra-gi-vicecity.zip",
        target: SourceTarget::Game {
            appid: "1546990",
            exe: Some("Gameface/Binaries/Win64/ViceCity.exe"),
        },
        fallback: None,
        predecessor: None,
    },
    GiSource {
        name: "Astra for GTA: San Andreas - The Definitive Edition (experimental)",
        id: "astra.gta-trilogy.sanandreas",
        repo: "mihailinl/astra-gta-trilogy",
        repo_id: 1_427_110_216,
        tag_prefix: "v",
        asset: "astra-gi-sanandreas.zip",
        target: SourceTarget::Game {
            appid: "1547000",
            exe: Some("Gameface/Binaries/Win64/SanAndreas.exe"),
        },
        fallback: None,
        predecessor: None,
    },
];

/// Every source: the three runners, then [`GAME_SOURCES`], each followed by the sources it
/// succeeds ([`GiSource::lineage`]).
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn all_sources() -> impl Iterator<Item = GiSource> {
    Universal::ALL
        .into_iter()
        .map(Universal::runner)
        .chain(GAME_SOURCES.iter().flat_map(GiSource::lineage))
}

/// `appid`'s own source, if it has one.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn game_source(appid: &str) -> Option<GiSource> {
    GAME_SOURCES
        .iter()
        .find(|s| s.appid() == Some(appid))
        .copied()
}

/// Whether `appid` has a universal-runner alternative to offer — what
/// `GameRow::has_universal_runner_alt` answers: a Unity Mono game with its own integration. Every
/// other game already uses its engine's runner with nothing to choose, and an integration that
/// names its program has no runner to stand in for it.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn has_universal_runner_alt(appid: &str) -> bool {
    game_source(appid).is_some_and(|s| s.exe().is_none())
}

/// The source `appid` installs from, given its engine's universal `flavour` — the newest of its
/// lineage, which Install walks back from when it has not shipped (`releases::resolve`):
/// - a game whose own source names its program always gets that source (Death Stranding, the
///   three GTA Definitive Editions over the generic Unreal runner);
/// - a Unity Mono game with its own source gets it unless `use_universal_runner` is true;
/// - everything else gets its flavour's runner — so an IL2CPP game always gets
///   [`UNIVERSAL_RUNNER_IL2CPP`] and any other Unreal game [`UNIVERSAL_RUNNER_UNREAL`], whatever
///   its appid.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn source_for(appid: &str, use_universal_runner: bool, flavour: Universal) -> GiSource {
    match game_source(appid) {
        Some(own) if own.exe().is_some() => own,
        // Every Unity game's own integration is a Mono build, so no other flavour can take one.
        Some(own) if flavour == Universal::UnityMono && !use_universal_runner => own,
        _ => flavour.runner(),
    }
}

/// Whether another source of this table lives in `source`'s repository (by its numeric id): such
/// a repository's release may carry only some of its sources' zips, so each source follows the
/// newest release carrying its own (`releases::pick_with`).
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn shares_its_repository(source: &GiSource) -> bool {
    all_sources()
        .filter(|s| s.repo_id == source.repo_id)
        .nth(1)
        .is_some()
}

/// The sources whose listings the Update question of an installed GI reads (`games::service`
/// keeps each listed): the source its ledger names (`follows`) and, when that is a source
/// `appid`'s own one succeeds, every source of that lineage from the newest down to it — an
/// installed predecessor is offered its successor's release once the successor ships
/// (`games::service::update_for`). Never a source older than `follows`: nothing offers one.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn update_sources(appid: &str, follows: GiSource) -> Vec<GiSource> {
    match game_source(appid) {
        Some(own) if own.lineage().any(|s| s == follows) => {
            let mut out: Vec<GiSource> = own.lineage().take_while(|s| *s != follows).collect();
            out.push(follows);
            out
        }
        _ => vec![follows],
    }
}

/// Normalises a `[integration].repo` value — from a real ledger, OR from this
/// table's own `GiSource::repo` — down to lowercase `"owner/repo"`: strips a
/// `http(s)://` scheme, a `github.com/` host, a trailing `.git`, and a
/// trailing `/`.
///
/// Every real manifest writes the FULL URL. Verified by reading the actual
/// release tags rather than guessing: astra-bepinex v0.6.1's
/// `runner/astra-gi.toml` and `runner/il2cpp/astra-gi.toml`, and
/// astra-lethal/-content-warning/-peak/-repo v0.2.0's `astra-gi.toml`, all
/// write `repo = "https://github.com/mihailinl/<repo>"` — never this table's
/// own `"owner/repo"` shorthand. Comparing the two shapes unnormalised, as
/// the first cut of this feature did, means `update_available` can never be
/// true against a single real ledger (found in review of bc7c16a1).
pub(crate) fn normalize_repo(repo: &str) -> String {
    // Lowercase FIRST: `strip_prefix` is byte-exact, so an upper- or mixed-case scheme/host
    // (a real possibility — URLs are case-insensitive here) would otherwise survive unstripped.
    let lower = repo.trim().to_ascii_lowercase();
    let s = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let s = s.strip_prefix("github.com/").unwrap_or(s);
    // Trailing slash BEFORE the `.git` suffix check — a URL can carry both
    // (`.../repo.git/`), and stripping in the other order would leave `.git` behind.
    let s = s.trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    s.trim_end_matches('/').to_string()
}

/// The source an already-installed integration's OWN identity names, and the
/// `use_universal_runner` value that same source needs for `PlanInstall`
/// (`false` = this appid's own source, `true` = a universal runner). `None` when neither test
/// below lands on a known source — the caller's signal to offer no update rather than guess.
///
/// A source is named by:
/// 1. `integration_repo`, normalised (see [`normalize_repo`]) and compared against the source's
///    own `repo`;
/// 2. only if `integration_repo` is absent entirely (an older or hand-edited manifest),
///    `integration_id` compared directly against the source's own `id`. A repo that IS present
///    and does not match is never overridden by a coincidental id.
///
/// The candidates are `appid`'s own source and every source it succeeds (keyed by appid: both
/// Death Stranding sources, and the three GTA sources, share one repository, so the repository
/// alone cannot say which game's integration this is), then every runner, each kept when the
/// ledger names it that way. Among them the one whose id the ledger carries wins — which tells
/// apart the two `astra-bepinex` runners (one repository), and on GTA III its former
/// astra-reshade integration ([`GTA3_ASTRA_RESHADE`]) from the generic Unreal runner (one
/// repository too) — and with no id match, the first: the appid's own source, else the Mono
/// runner. So a Unity id under `astra-reshade` is the Unreal runner, and anything else under
/// `astra-bepinex` the Mono one.
/// This is read from the LEDGER's own identity, never re-derived from today's engine detection,
/// which this function never takes: a game installed on purpose with the universal runner must
/// be offered an update to a newer UNIVERSAL build, never silently steered back to its own
/// source, and a transient detection disagreement must never flip which flavour an update
/// compares against.
#[cfg_attr(not(astra_games), allow(dead_code))]
pub fn source_for_ledger(
    appid: &str,
    integration_id: &str,
    integration_repo: Option<&str>,
) -> Option<(GiSource, bool)> {
    let ledger_repo = integration_repo.map(normalize_repo);
    let names = |s: &GiSource| match ledger_repo.as_deref() {
        Some(repo) => repo == normalize_repo(s.repo),
        None => integration_id.eq_ignore_ascii_case(s.id),
    };
    let candidates: Vec<(GiSource, bool)> = game_source(appid)
        .into_iter()
        .flat_map(|own| own.lineage())
        .filter(names)
        .map(|own| (own, false))
        .chain(
            Universal::ALL
                .into_iter()
                .map(Universal::runner)
                .filter(names)
                .map(|runner| (runner, true)),
        )
        .collect();
    candidates
        .iter()
        .find(|(s, _)| integration_id.eq_ignore_ascii_case(s.id))
        .or(candidates.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sources with no compiled pin, by id: the trilogy's three, whose repository had no
    /// release when they were added. Any other source without one would install nothing offline
    /// with nothing saying so.
    const UNPINNED: [&str; 3] = [
        "astra.gta-trilogy.gta3",
        "astra.gta-trilogy.vicecity",
        "astra.gta-trilogy.sanandreas",
    ];

    #[test]
    fn every_fallback_has_a_64_character_lowercase_hex_digest() {
        for s in all_sources() {
            let Some(pin) = s.fallback else {
                assert!(UNPINNED.contains(&s.id), "{} has no fallback pin", s.name);
                continue;
            };
            let sha = pin.sha256;
            assert_eq!(
                sha.len(),
                64,
                "{}'s sha256 is {} characters, not 64: {sha:?}",
                s.name,
                sha.len()
            );
            assert!(
                sha.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}'s sha256 is not lowercase hex: {sha:?}",
                s.name
            );
        }
    }

    #[test]
    fn every_source_follows_v_tags_and_names_a_gi_zip() {
        for s in all_sources() {
            assert_eq!(s.tag_prefix, "v", "{}", s.name);
            if let Some(pin) = s.fallback {
                assert!(
                    pin.tag.starts_with(s.tag_prefix),
                    "{}'s fallback tag {:?} is not one of its own releases",
                    s.name,
                    pin.tag
                );
            }
            assert!(
                s.asset.starts_with("astra-gi") && s.asset.ends_with(".zip"),
                "{}'s asset {:?} is not a GI zip",
                s.name,
                s.asset
            );
            assert!(!s.id.is_empty(), "{}", s.name);
            assert_eq!(
                normalize_repo(s.repo),
                s.repo,
                "{}: owner/repo, lowercase",
                s.name
            );
        }
    }

    /// Two sources of one repository follow the SAME releases, so only their asset keeps each on
    /// its own zip: one release, two games (Death Stranding) or two runners (Mono and IL2CPP).
    #[test]
    fn sources_sharing_a_repository_never_share_an_asset() {
        let all: Vec<GiSource> = all_sources().collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                if a.repo == b.repo {
                    assert_eq!(a.repo_id, b.repo_id, "{} / {}", a.name, b.name);
                    assert_ne!(a.asset, b.asset, "{} and {} share a zip", a.name, b.name);
                }
                assert_ne!(a.id, b.id, "two sources carry one integration id");
            }
        }
    }

    #[test]
    fn a_runner_names_an_engine_and_no_appid_and_a_game_names_its_appid() {
        for u in Universal::ALL {
            let r = u.runner();
            assert!(r.is_runner(), "{}", r.name);
            assert_eq!(r.appid(), None, "{}", r.name);
            assert_eq!(r.exe(), None, "{}", r.name);
        }
        let engines = Universal::ALL.map(|u| match u.runner().target {
            SourceTarget::Runner { engine } => engine,
            SourceTarget::Game { .. } => unreachable!(),
        });
        assert_eq!(engines, ["unity-mono", "unity-il2cpp", "unreal"]);
        let mut appids = Vec::new();
        for s in GAME_SOURCES {
            let appid = s.appid().expect("a game's own source names its appid");
            assert!(appid.parse::<u32>().is_ok(), "{}", s.name);
            assert!(!s.is_runner());
            assert!(!appids.contains(&appid), "{appid} has two sources");
            appids.push(appid);
        }
    }

    /// A predecessor installs on the same game and program as the source that succeeds it: the
    /// launcher target is built from the game's own (newest) source's program whichever source of
    /// the lineage Install lands on, and an Update moves between them in the one profile. Nor is a
    /// predecessor a runner, or a lineage longer than the move it records.
    #[test]
    fn every_source_of_a_lineage_targets_the_same_game_and_program() {
        for own in GAME_SOURCES {
            for older in own.lineage().skip(1) {
                assert_eq!(older.target, own.target, "{} / {}", own.name, older.name);
                assert!(own.succeeds(&older));
                assert_ne!(older.id, own.id);
            }
            assert!(own.lineage().count() <= 2, "{}", own.name);
        }
        for u in Universal::ALL {
            assert_eq!(u.runner().predecessor, None, "a runner succeeds nothing");
        }
    }

    #[test]
    fn an_il2cpp_game_always_gets_the_il2cpp_runner() {
        assert_eq!(
            source_for("1966720", false, Universal::UnityIl2cpp),
            UNIVERSAL_RUNNER_IL2CPP
        );
        assert_eq!(
            source_for("999", true, Universal::UnityIl2cpp),
            UNIVERSAL_RUNNER_IL2CPP
        );
    }

    #[test]
    fn an_unreal_game_always_gets_the_unreal_runner_never_a_unity_games_own_source() {
        // Scarlet Skips, the first Unreal game this was built for.
        assert_eq!(
            source_for("4513480", false, Universal::Unreal).id,
            "astra.unreal"
        );
        // Under EVERY Unity game's appid, asked for its own source or not: those are Mono builds.
        for own in GAME_SOURCES.iter().filter(|s| s.exe().is_none()) {
            let appid = own.appid().unwrap();
            for use_universal_runner in [false, true] {
                let s = source_for(appid, use_universal_runner, Universal::Unreal);
                assert_eq!(s, UNIVERSAL_RUNNER_UNREAL, "{appid}");
            }
        }
    }

    #[test]
    fn the_unreal_runner_is_astra_reshade() {
        let s = UNIVERSAL_RUNNER_UNREAL;
        assert_eq!(s.name, "Astra for any Unreal game (experimental)");
        assert_eq!(
            (s.id, s.repo, s.repo_id, s.asset, s.fallback.map(|p| p.tag)),
            (
                "astra.unreal",
                "mihailinl/astra-reshade",
                1_412_685_270,
                "astra-gi.zip",
                Some("v0.2.1")
            )
        );
        assert_eq!(s.target, SourceTarget::Runner { engine: "unreal" });
        assert_eq!(Universal::Unreal.runner(), s);
    }

    /// RED ON PURPOSE if a placeholder digest is ever written here: an Unreal runner that cannot
    /// install offline must not ship looking finished.
    #[test]
    fn the_unreal_runner_digest_is_not_the_placeholder() {
        assert!(
            UNIVERSAL_RUNNER_UNREAL
                .fallback
                .expect("the Unreal runner has a pin")
                .sha256
                .chars()
                .any(|c| c != '0'),
            "UNIVERSAL_RUNNER_UNREAL's fallback sha256 is the all-zero placeholder"
        );
    }

    #[test]
    fn source_for_prefers_the_games_own_source_unless_told_otherwise() {
        let lethal = source_for("1966720", false, Universal::UnityMono);
        assert_eq!(lethal.name, "Astra for Lethal Company");
        // The alternative follows the runner's source, not Lethal Company's.
        let lethal_universal = source_for("1966720", true, Universal::UnityMono);
        assert_eq!(lethal_universal, UNIVERSAL_RUNNER);
        let unknown = source_for("999999", false, Universal::UnityMono);
        assert_eq!(unknown, UNIVERSAL_RUNNER);
    }

    #[test]
    fn a_non_unity_games_own_source_is_its_only_source() {
        let ds1 = game_source("1850570").unwrap();
        let ds2 = game_source("3280350").unwrap();
        assert_eq!((ds1.asset, ds1.exe()), ("astra-gi-ds1.zip", Some("ds.exe")));
        assert_eq!(
            (ds2.asset, ds2.exe()),
            ("astra-gi-ds2.zip", Some("DS2.exe"))
        );
        assert_eq!(ds1.repo, ds2.repo, "one repository, two games");
        // Whatever detection said and whatever the request asked: no runner stands in.
        for flavour in Universal::ALL {
            for use_universal_runner in [false, true] {
                assert_eq!(source_for("1850570", use_universal_runner, flavour), ds1);
                assert_eq!(source_for("3280350", use_universal_runner, flavour), ds2);
            }
        }
        assert!(!has_universal_runner_alt("1850570"));
        assert!(!has_universal_runner_alt("3280350"));
        assert_eq!(game_source("999999"), None);
    }

    /// GTA: The Trilogy – The Definitive Edition: three games, one repository, each its own asset
    /// and program (the owner's mapping, 2026-10-20), none pinned yet. GTA III's source succeeds
    /// astra-reshade's, which it replaces; Vice City and San Andreas never had one.
    #[test]
    fn the_trilogys_three_games_each_take_their_own_asset_of_one_repository() {
        let expected = [
            (
                "1546970",
                "astra.gta-trilogy.gta3",
                "astra-gi-gta3.zip",
                "Gameface/Binaries/Win64/LibertyCity.exe",
            ),
            (
                "1546990",
                "astra.gta-trilogy.vicecity",
                "astra-gi-vicecity.zip",
                "Gameface/Binaries/Win64/ViceCity.exe",
            ),
            (
                "1547000",
                "astra.gta-trilogy.sanandreas",
                "astra-gi-sanandreas.zip",
                "Gameface/Binaries/Win64/SanAndreas.exe",
            ),
        ];
        for (appid, id, asset, exe) in expected {
            let s = game_source(appid).expect("each trilogy game has its own source");
            assert_eq!(
                (s.id, s.repo, s.repo_id, s.tag_prefix, s.asset, s.exe()),
                (
                    id,
                    "mihailinl/astra-gta-trilogy",
                    1_427_110_216,
                    "v",
                    asset,
                    Some(exe)
                ),
            );
            assert_eq!(s.fallback, None, "{id}: no release reviewed yet");
            assert!(!has_universal_runner_alt(appid), "{id}");
            // Its own source whatever detection said and whatever the request asked.
            for flavour in Universal::ALL {
                for use_universal_runner in [false, true] {
                    assert_eq!(source_for(appid, use_universal_runner, flavour), s, "{id}");
                }
            }
        }
        let gta3 = game_source("1546970").unwrap();
        assert_eq!(gta3.predecessor, Some(&GTA3_ASTRA_RESHADE));
        assert_eq!(game_source("1546990").unwrap().predecessor, None);
        assert_eq!(game_source("1547000").unwrap().predecessor, None);
        // Every other Unreal game still takes the generic runner.
        assert_eq!(
            source_for("4513480", false, Universal::Unreal),
            UNIVERSAL_RUNNER_UNREAL
        );
    }

    /// GTA III's source is the trilogy's, never astra-reshade's — which stays its predecessor:
    /// the same game and program, its own zip of astra-reshade's releases, still pinned.
    #[test]
    fn gta3_resolves_to_the_trilogy_and_astra_reshade_is_only_its_predecessor() {
        let gta3 = game_source("1546970").expect("GTA III - The Definitive Edition");
        assert_eq!(gta3.repo, "mihailinl/astra-gta-trilogy");
        assert_ne!(gta3.repo, UNIVERSAL_RUNNER_UNREAL.repo);
        let old = GTA3_ASTRA_RESHADE;
        assert_eq!(
            (old.id, old.repo, old.repo_id, old.asset, old.exe()),
            (
                "astra.unreal.gta3-definitive-edition",
                UNIVERSAL_RUNNER_UNREAL.repo,
                UNIVERSAL_RUNNER_UNREAL.repo_id,
                "astra-gi-gta3.zip",
                gta3.exe()
            )
        );
        assert_eq!(old.appid(), Some("1546970"));
        assert_eq!(
            old.fallback.map(|p| (p.tag, p.prerelease)),
            Some(("v0.2.2", true))
        );
        assert_eq!(gta3.lineage().collect::<Vec<_>>(), vec![gta3, old]);
        assert!(gta3.succeeds(&old));
        assert!(!old.succeeds(&gta3));
        assert!(!gta3.succeeds(&gta3));
        // Not a game's own source of its own: GTA III has ONE entry in the table.
        assert!(!GAME_SOURCES.contains(&old));
        assert!(
            all_sources().any(|s| s == old),
            "but every-source checks see it"
        );
        // The Update question of an astra-reshade install reads the trilogy's listing too, newest
        // first; a trilogy install reads only its own; anything else only what it follows.
        assert_eq!(update_sources("1546970", old), vec![gta3, old]);
        assert_eq!(update_sources("1546970", gta3), vec![gta3]);
        assert_eq!(
            update_sources("1546970", UNIVERSAL_RUNNER_UNREAL),
            vec![UNIVERSAL_RUNNER_UNREAL]
        );
        let lethal = game_source("1966720").unwrap();
        assert_eq!(update_sources("1966720", lethal), vec![lethal]);
    }

    /// On GTA III a ledger from `astra-reshade` is its former integration (now the trilogy's
    /// predecessor) or the generic runner installed before it existed: the ledger's id decides,
    /// never the shared repository; a ledger from the trilogy is the trilogy's.
    #[test]
    fn source_for_ledger_tells_gta3s_integration_from_the_generic_runner_by_id() {
        let reshade = "https://github.com/mihailinl/astra-reshade";
        let trilogy = "https://github.com/mihailinl/astra-gta-trilogy";
        assert_eq!(
            source_for_ledger("1546970", "astra.gta-trilogy.gta3", Some(trilogy)),
            Some((game_source("1546970").unwrap(), false))
        );
        assert_eq!(
            source_for_ledger(
                "1546970",
                "astra.unreal.gta3-definitive-edition",
                Some(reshade)
            ),
            Some((GTA3_ASTRA_RESHADE, false))
        );
        // The trilogy's repository names nothing on another game, nor astra-reshade's id under it.
        assert!(source_for_ledger("4513480", "astra.gta-trilogy.gta3", Some(trilogy)).is_none());
        assert_eq!(
            source_for_ledger(
                "1546970",
                "astra.unreal.gta3-definitive-edition",
                Some(trilogy)
            )
            .map(|(s, _)| s.id),
            Some("astra.gta-trilogy.gta3")
        );
        assert_eq!(
            source_for_ledger(
                "1546970",
                "astra.unreal.gta3-definitive-edition",
                Some(reshade)
            )
            .map(|(s, u)| (s.id, u)),
            Some(("astra.unreal.gta3-definitive-edition", false))
        );
        assert_eq!(
            source_for_ledger("1546970", "astra.unreal", Some(reshade)).map(|(s, u)| (s.id, u)),
            Some(("astra.unreal", true))
        );
        // No repo recorded: the id alone, either way.
        assert_eq!(
            source_for_ledger("1546970", "astra.unreal", None).map(|(s, u)| (s.id, u)),
            Some(("astra.unreal", true))
        );
        // On any other game, GTA III's id under that repository is just the runner's family.
        assert_eq!(
            source_for_ledger(
                "4513480",
                "astra.unreal.gta3-definitive-edition",
                Some(reshade)
            )
            .map(|(s, u)| (s.id, u)),
            Some(("astra.unreal", true))
        );
    }

    #[test]
    fn source_for_ledger_keys_a_shared_repository_by_appid_then_identity() {
        let url = "https://github.com/mihailinl/astra-in-death-stranding";
        // The real ds1 manifest's identity, on DS1: its source.
        assert_eq!(
            source_for_ledger("1850570", "astra.death-stranding", Some(url))
                .map(|(s, u)| (s.id, u)),
            Some(("astra.death-stranding", false))
        );
        // The same repository on DS2 is DS2's source, never DS1's.
        assert_eq!(
            source_for_ledger("3280350", "astra.death-stranding-2", Some(url)).map(|(s, _)| s.id),
            Some("astra.death-stranding-2")
        );
        // Another repository on DS1 (a fork): nothing.
        assert!(
            source_for_ledger(
                "1850570",
                "astra.death-stranding",
                Some("https://github.com/someone/fork")
            )
            .is_none()
        );
        // No repository recorded: the id decides.
        assert!(source_for_ledger("1850570", "astra.death-stranding", None).is_some());
        // Not DS's game: its repository names no source here.
        assert!(source_for_ledger("1966720", "astra.death-stranding", Some(url)).is_none());
    }

    #[test]
    fn has_universal_runner_alt_is_exactly_the_four_unity_games() {
        assert!(has_universal_runner_alt("1966720"));
        assert!(has_universal_runner_alt("2881650"));
        assert!(has_universal_runner_alt("3527290"));
        assert!(has_universal_runner_alt("3241660"));
        assert!(!has_universal_runner_alt("1966721"));
        assert!(!has_universal_runner_alt(""));
        // The five that name their program have none.
        for appid in ["1850570", "3280350", "1546970", "1546990", "1547000"] {
            assert!(!has_universal_runner_alt(appid), "{appid}");
        }
        assert_eq!(GAME_SOURCES.len(), 9);
    }

    #[test]
    fn normalize_repo_strips_scheme_host_dot_git_and_trailing_slash() {
        assert_eq!(
            normalize_repo("https://github.com/mihailinl/astra-bepinex"),
            "mihailinl/astra-bepinex"
        );
        assert_eq!(
            normalize_repo("http://github.com/mihailinl/astra-bepinex/"),
            "mihailinl/astra-bepinex"
        );
        assert_eq!(
            normalize_repo("https://github.com/mihailinl/astra-bepinex.git"),
            "mihailinl/astra-bepinex"
        );
        // Already this table's own shorthand — a no-op.
        assert_eq!(
            normalize_repo("mihailinl/astra-bepinex"),
            "mihailinl/astra-bepinex"
        );
        // Case folds, since GitHub's own routing does.
        assert_eq!(
            normalize_repo("https://github.com/Mihailinl/Astra-Bepinex"),
            "mihailinl/astra-bepinex"
        );
    }

    // The fixtures below are the REAL `[integration]` tables, read with
    // `git show <tag>:<path>` from each GI repo on this machine — not
    // invented shapes. A first cut of this feature invented
    // "mihailinl/astra-lethal"-shorthand `repo` values for its fixtures and
    // every one of its tests passed while the ledger lookup's predecessor
    // matched not one real ledger (review of bc7c16a1): every real `repo` is
    // the full URL.

    /// `git -C astra-bepinex show v0.6.1:runner/astra-gi.toml` — `[integration]`; unchanged in v0.8.1's zip.
    const REAL_UNIVERSAL_MONO_ID: &str = "astra.unity";
    /// `git -C astra-bepinex show v0.6.1:runner/il2cpp/astra-gi.toml` — `[integration]`; unchanged in v0.8.1's zip.
    const REAL_UNIVERSAL_IL2CPP_ID: &str = "astra.unity-il2cpp";
    /// Both of the above share this `repo` line.
    const REAL_BEPINEX_REPO: &str = "https://github.com/mihailinl/astra-bepinex";
    /// `unzip -p astra-gi.zip astra-gi.toml` of astra-reshade's v0.2.1 release — `[integration]` (unchanged since the v0.2.0 candidate).
    const REAL_UNREAL_ID: &str = "astra.unreal";
    const REAL_RESHADE_REPO: &str = "https://github.com/mihailinl/astra-reshade";
    /// `git -C astra-lethal show v0.2.0:astra-gi.toml` — `[integration]`.
    const REAL_LETHAL_ID: &str = "astra.lethal-company";
    const REAL_LETHAL_REPO: &str = "https://github.com/mihailinl/astra-lethal";

    #[test]
    fn source_for_ledger_matches_the_real_lethal_company_manifest_identity() {
        let (s, use_universal_runner) =
            source_for_ledger("1966720", REAL_LETHAL_ID, Some(REAL_LETHAL_REPO))
                .expect("the real astra-lethal v0.2.0 identity should match its own source");
        assert_eq!(s.name, "Astra for Lethal Company");
        assert!(!use_universal_runner);
    }

    #[test]
    fn source_for_ledger_is_keyed_by_appid_not_repo_alone() {
        // Lethal Company's real repo/id, under Content Warning's appid — the lookup is
        // keyed by appid first, so this must not fall through to a DIFFERENT game's source.
        assert!(
            source_for_ledger("2881650", REAL_LETHAL_ID, Some(REAL_LETHAL_REPO)).is_none(),
            "Lethal Company's identity must not match under a different appid"
        );
    }

    #[test]
    fn source_for_ledger_matches_the_real_universal_mono_manifest_identity() {
        // A Unity game with its own source (Lethal Company) installed with the universal runner
        // on purpose — the source comes from the LEDGER's identity, never from
        // `has_universal_runner_alt`.
        let (s, use_universal_runner) =
            source_for_ledger("1966720", REAL_UNIVERSAL_MONO_ID, Some(REAL_BEPINEX_REPO))
                .expect("the real universal Mono identity should match the universal Mono runner");
        assert_eq!(s, UNIVERSAL_RUNNER);
        assert!(use_universal_runner);
    }

    #[test]
    fn source_for_ledger_matches_the_real_universal_il2cpp_manifest_identity() {
        // The Mono and IL2CPP runners share ONE `repo` — only `integration_id` tells
        // them apart, and it must come from the ledger, not from re-running detection.
        let (s, use_universal_runner) = source_for_ledger(
            "999999", // no source of its own
            REAL_UNIVERSAL_IL2CPP_ID,
            Some(REAL_BEPINEX_REPO),
        )
        .expect("the real universal IL2CPP identity should match the universal IL2CPP runner");
        assert_eq!(s, UNIVERSAL_RUNNER_IL2CPP);
        assert!(use_universal_runner);
    }

    #[test]
    fn source_for_ledger_maps_the_real_unreal_manifest_identity_back_to_the_unreal_runner() {
        // Scarlet Skips; and under a Unity game's appid too — the ledger's identity decides.
        for appid in ["4513480", "1966720"] {
            let (s, use_universal_runner) =
                source_for_ledger(appid, REAL_UNREAL_ID, Some(REAL_RESHADE_REPO))
                    .expect("the real astra-reshade identity should match the Unreal runner");
            assert_eq!(s, UNIVERSAL_RUNNER_UNREAL, "{appid}");
            assert!(use_universal_runner, "{appid}");
        }
        // No repo recorded: the id alone.
        assert_eq!(
            source_for_ledger("4513480", REAL_UNREAL_ID, None).map(|(s, u)| (s.id, u)),
            Some(("astra.unreal", true))
        );
        // The Unreal id under astra-bepinex's repo is not the Unreal runner: the repo decides.
        assert_ne!(
            source_for_ledger("4513480", REAL_UNREAL_ID, Some(REAL_BEPINEX_REPO))
                .map(|(s, _)| s.id),
            Some("astra.unreal")
        );
        // Nor does a Unity id under astra-reshade's repo become a Unity runner.
        assert_eq!(
            source_for_ledger("4513480", REAL_UNIVERSAL_MONO_ID, Some(REAL_RESHADE_REPO))
                .map(|(s, _)| s.id),
            Some("astra.unreal")
        );
    }

    #[test]
    fn source_for_ledger_normalizes_a_url_shaped_repo_against_this_tables_shorthand() {
        // Trailing slash, `.git`, and different case — all three traps `normalize_repo`
        // exists for, exercised through the public entry point rather than only the helper.
        let (s, _) = source_for_ledger(
            "1966720",
            REAL_LETHAL_ID,
            Some("HTTPS://GitHub.com/Mihailinl/Astra-Lethal.git/"),
        )
        .expect("a differently-cased, slash- and .git-suffixed URL must still match");
        assert_eq!(s.name, "Astra for Lethal Company");
    }

    #[test]
    fn source_for_ledger_falls_back_to_id_only_when_repo_is_entirely_absent() {
        assert_eq!(
            source_for_ledger("1966720", REAL_LETHAL_ID, None).map(|(s, u)| (s.name, u)),
            Some(("Astra for Lethal Company", false))
        );
        assert_eq!(
            source_for_ledger("999999", REAL_UNIVERSAL_IL2CPP_ID, None).map(|(s, u)| (s.name, u)),
            Some((UNIVERSAL_RUNNER_IL2CPP.name, true))
        );
        // Neither the id nor the (absent) repo names anything known.
        assert!(source_for_ledger("1966720", "some-other-mod", None).is_none());
    }

    #[test]
    fn source_for_ledger_never_lets_a_coincidental_id_override_a_present_nonmatching_repo() {
        // The repo IS present and names something else entirely — a ledger this malformed
        // must refuse to decide, not fall through to the id fallback meant only for a
        // manifest that recorded no repo at all.
        assert!(
            source_for_ledger(
                "1966720",
                REAL_LETHAL_ID,
                Some("https://github.com/someone/else")
            )
            .is_none()
        );
    }

    #[test]
    fn source_for_ledger_cannot_decide_an_unrelated_identity() {
        assert!(
            source_for_ledger(
                "1966720",
                "someone.else",
                Some("https://github.com/someone/else")
            )
            .is_none()
        );
        assert!(source_for_ledger("1966720", "", None).is_none());
    }
}
