//! The business logic behind `GamesService`'s five real RPCs — `GamesStatus`
//! answers directly in the thin gRPC layer (`server/services/games.rs`) and
//! needs none of this.
//!
//! Calls straight into `astra_game_launcher`
//! (<https://github.com/mihailinl/astra-game-launcher>, MPL-2.0, pinned by
//! commit in `Cargo.toml`) for detection, the manifest/zip pipeline, the
//! install/uninstall ledger and the launch plan. Every public function here
//! returns plain data (never a `tonic::Status`): the gRPC layer is the ONLY
//! place a [`LauncherError`] becomes wire fields, so a refusal reaches the
//! UI as `{code, params}` and never as English prose.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use futures::StreamExt as _;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tracing::info;

use astra_game_launcher::{
    Binary, Detection, Engine, GameAction, GameFileAction, GameTarget, GiPackage, InstallPlan,
    LauncherError, Ledger, LedgerGameFile, Manifest, Platform, ProcessPlan, Spawn as _, codes,
    find_steam, install, installed, launch, plan_install, prepare_launch, uninstall,
};

use super::releases::{self, Gi, ReleaseCache};
use crate::AppState;
use crate::platform::steam_library::{SteamApp, installed_apps};

/// The engine's games-bridge listening port, unless a future `game` level
/// reports another — the MVP always uses the default.
const DEFAULT_BRIDGE_PORT: u16 = 25600;

// ───────────────────────── Listing ─────────────────────────

#[derive(Debug, Clone)]
pub struct GameRow {
    pub appid: String,
    pub name: String,
    pub engine: String,
    pub binary: String,
    pub state: String,
    pub refusal_code: String,
    pub refusal_params: BTreeMap<String, String>,
    pub installed: bool,
    pub has_universal_runner_alt: bool,
    /// `data:image/jpeg;base64,…`, or `""` when Steam's library-cache has no
    /// small icon for this appid — see [`icon_for`].
    pub icon: String,
    /// True only when `installed` AND the newest listed release of the source the ledger names
    /// is newer than what is installed — see [`update_for`].
    pub update_available: bool,
    /// The ledger's `integration.version`. Empty when `update_available` is false.
    pub installed_version: String,
    /// The release's version. Empty when `update_available` is false.
    pub available_version: String,
    /// `PlanInstall`'s `use_universal_runner` for this update. Meaningless
    /// when `update_available` is false.
    pub update_use_universal_runner: bool,
    /// The integration the update MOVES the game to ([`super::pins::GiSource::name`]) when the
    /// offered release is another source's than the one the ledger names (`follows`): a
    /// successor that has shipped ([`update_for`]). "" for an update of the installed
    /// integration, and when `update_available` is false.
    pub update_integration: String,
    /// The source the installed GI's ledger names (`pins::source_for_ledger`): with any source
    /// that has since succeeded it (`pins::update_sources`), the repositories
    /// [`spawn_release_refresh`] keeps listed for this row's Update offer. `None` when nothing is
    /// installed or the ledger names no known source. Not on the wire.
    pub follows: Option<super::pins::GiSource>,
}

/// Every installed Steam app, detected and classified. Sort order matches
/// the UX plan: installed first, then Unity Mono, then the rest.
///
/// Synchronous and filesystem-only (Steam's manifests + one `detect()` per
/// game, cached) — the gRPC handler runs this inside `spawn_blocking`.
pub fn list_games(state: &AppState) -> Vec<GameRow> {
    let apps: Vec<SteamApp> = installed_apps()
        .into_iter()
        .filter(|app| !is_steam_tool(app))
        .collect();
    let ids: Vec<u32> = apps.iter().filter_map(|a| a.appid.parse().ok()).collect();
    let hints = steam_launch_hints(&ids);
    // Steam's own VAC flag, one pass for the whole library: such a game shows as anti-cheat
    // before anyone presses Install (the launcher refuses it at plan and launch either way).
    let vac = steam_appinfo_path()
        .map(|p| astra_game_launcher::valve_anti_cheat_of(&p, &ids))
        .unwrap_or_default();
    let mut rows: Vec<GameRow> = apps
        .iter()
        .filter_map(|app| {
            let id = app.appid.parse::<u32>().ok();
            let hint = id.and_then(|id| hints.get(&id));
            let vac_flagged = id.is_some_and(|id| vac.get(&id) == Some(&true));
            row_for(state, app, hint.map(String::as_str), vac_flagged)
        })
        .collect();
    rows.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    rows
}

/// Steam's own runtime plumbing, not a game — Proton (every build:
/// "Proton 7.0", "Proton Experimental", …), the Steam Linux Runtime
/// containers, and the Steamworks Common Redistributables all show up in
/// the same manifests `installed_apps()` reads, and none of them is
/// something a player ever "plays". Checked by both the DISPLAY name (what
/// Steam shows) and the `installdir` (the folder on disk, which for some of
/// these differs from the display name — the Redistributables' own
/// `installdir` is `Steamworks Shared`) because either alone has missed a
/// variant before.
fn is_steam_tool(app: &SteamApp) -> bool {
    if app.name.starts_with("Proton")
        || app.name.starts_with("Steam Linux Runtime")
        || app.name == "Steamworks Common Redistributables"
    {
        return true;
    }
    match app.installdir.as_deref() {
        Some(dir) => {
            dir.starts_with("Proton")
                || dir.starts_with("SteamLinuxRuntime")
                || dir.starts_with("Steamworks Shared")
        }
        None => false,
    }
}

fn sort_key(row: &GameRow) -> (u8, String) {
    let bucket = if row.installed {
        0
    } else if row.engine == "unity-mono" {
        1
    } else {
        2
    };
    (bucket, row.name.to_lowercase())
}

fn row_for(
    state: &AppState,
    app: &SteamApp,
    hint: Option<&str>,
    vac_flagged: bool,
) -> Option<GameRow> {
    // The appid must be numeric before anything derives a filesystem path
    // from it (`profile_dir_for` keys the profile dir by appid alone) — a
    // hand-edited or corrupt Steam manifest is the only way a non-numeric
    // one reaches here, and the row is silently skipped rather than risking
    // a path built from an attacker- or corruption-controlled string.
    app.appid.parse::<u32>().ok()?;
    let installdir = app.installdir.as_deref()?;
    let game_dir = app
        .library
        .join("steamapps")
        .join("common")
        .join(installdir);
    if !game_dir.is_dir() {
        return None;
    }
    let detection = with_vac(
        with_source_exe(
            &app.appid,
            &game_dir,
            cached_detect(state, &game_dir, hint)?,
        ),
        vac_flagged,
    );
    let appid_num = app.appid.parse::<u32>().ok();
    let platform = platform_for(&game_dir, appid_num, &detection);
    let profile_dir = profile_dir_for(state, app);
    let own_program = super::pins::game_source(&app.appid).is_some_and(|s| s.exe().is_some());
    let installed_ledger = installed(&profile_dir).ok().flatten();
    let follows = installed_ledger.as_ref().and_then(|l| {
        super::pins::source_for_ledger(&app.appid, &l.integration.id, l.integration.repo.as_deref())
            .map(|(source, _)| source)
    });

    let busy = state.games.busy.lock().unwrap().get(&app.appid).copied();
    let (state_code, refusal_code, refusal_params) = match busy {
        Some(busy_state) => (busy_state.to_string(), String::new(), BTreeMap::new()),
        None => compute_state(
            &detection,
            &platform,
            installed_ledger.is_some(),
            own_program,
        ),
    };

    let update = installed_ledger
        .as_ref()
        .and_then(|ledger| update_for(&app.appid, &detection, ledger, &state.games.release_cache));
    let update_integration = moves_to(update.as_ref(), follows);
    let (update_available, installed_version, available_version, update_use_universal_runner) =
        match &update {
            Some((gi, use_universal_runner)) => (
                true,
                installed_ledger
                    .as_ref()
                    .map(|l| l.integration.version.clone())
                    .unwrap_or_default(),
                gi.version.clone(),
                *use_universal_runner,
            ),
            None => (false, String::new(), String::new(), false),
        };

    Some(GameRow {
        appid: app.appid.clone(),
        name: app.name.clone(),
        engine: detection.engine.code().to_string(),
        binary: detection
            .exe
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        state: state_code,
        refusal_code,
        refusal_params,
        installed: installed_ledger.is_some(),
        has_universal_runner_alt: super::pins::has_universal_runner_alt(&app.appid),
        icon: icon_for(state, &app.appid),
        update_available,
        installed_version,
        available_version,
        update_use_universal_runner,
        update_integration,
        follows,
    })
}

/// Whether an update exists for an already-installed row, and what it would be: the newest
/// release, in the last listing `releases` holds, of the SAME source the ledger's own identity
/// names ([`super::pins::source_for_ledger`] — a game's own integration or a universal runner,
/// and the runner's Mono/IL2CPP/Unreal FLAVOUR from that same identity, never from today's
/// detection) whose `sha256` differs from what the ledger has AND whose version is NEWER (semver;
/// `version_is_newer_or_undecidable`) — a same-version digest difference is a republish, not "an
/// update". Never a network call here: [`spawn_release_refresh`] keeps the listing.
///
/// A ledger of a source the game's own source has since SUCCEEDED (GTA III installed from
/// astra-reshade, now the trilogy's game: [`super::pins::GiSource::predecessor`]) is offered what
/// `PlanInstall` would install — the same lineage walk over the listings held
/// ([`releases::pick_held`]) — and once that lands on a newer source of the lineage, the move IS
/// the update, whatever the two version numbers say (they count different repositories' releases;
/// the trilogy's first is 0.1.0, astra-reshade's last 0.2.x). Installing it replaces the old
/// integration in the game's one profile. A source older than the ledger's is never offered.
///
/// `None` when the source cannot be decided, when a listing the walk needs was never answered (the
/// offline fallback is never offered as an update), when the newest release is not newer, when
/// `PlanInstall` would plan ANOTHER source today ([`super::pins::source_for`] with today's
/// detection — a ledger of another flavour, a Mono install on a game now detected as IL2CPP or
/// the reverse, is a reinstall, not an update), and — the Unity-version refusal still applies
/// here, exactly as it does at `PlanInstall` time — when [`il2cpp_runner_refuses`] refuses this
/// game.
fn update_for(
    appid: &str,
    detection: &Detection,
    ledger: &Ledger,
    releases: &ReleaseCache,
) -> Option<(Gi, bool)> {
    update_for_with(
        appid,
        detection,
        ledger,
        releases,
        releases::FOLLOWS_PRERELEASES,
    )
}

/// [`update_for`] on either side of the prerelease knob (`releases::FOLLOWS_PRERELEASES`).
fn update_for_with(
    appid: &str,
    detection: &Detection,
    ledger: &Ledger,
    releases: &ReleaseCache,
    prereleases: bool,
) -> Option<(Gi, bool)> {
    let (source, use_universal_runner) = super::pins::source_for_ledger(
        appid,
        &ledger.integration.id,
        ledger.integration.repo.as_deref(),
    )?;
    if il2cpp_runner_refuses(detection).is_some() {
        return None;
    }
    // The row offers only what PlanInstall will then plan, through the one mapping both use: the
    // ledger's source must be in the planned source's lineage, and the release is the one the
    // same walk lands on.
    let planned = super::pins::source_for(
        appid,
        use_universal_runner,
        universal_flavour(detection.engine),
    );
    let lineage: Vec<_> = planned.lineage().collect();
    let installed_at = lineage.iter().position(|s| *s == source)?;
    let gi = releases::pick_held(releases, &planned, prereleases)?;
    let offered_at = lineage.iter().position(|s| *s == gi.source)?;
    let update = match offered_at.cmp(&installed_at) {
        // A newer source of the lineage has shipped: moving to it is the update.
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => is_update(&gi, ledger),
        // Never back to a source the ledger's has succeeded.
        std::cmp::Ordering::Greater => false,
    };
    update.then_some((gi, use_universal_runner))
}

/// The integration an Update offer MOVES the game to (`GameRow::update_integration`): the
/// offered release's source by name when it is not the source the ledger names (`follows`), and
/// "" when the update stays on it or there is no update.
fn moves_to(update: Option<&(Gi, bool)>, follows: Option<super::pins::GiSource>) -> String {
    match (update, follows) {
        (Some((gi, _)), Some(installed)) if gi.source != installed => gi.source.name.to_owned(),
        _ => String::new(),
    }
}

/// `gi` is an update over what `ledger` installed: other bytes AND a newer version (a
/// same-version digest difference is a republish, not an update).
fn is_update(gi: &Gi, ledger: &Ledger) -> bool {
    !gi.sha256.eq_ignore_ascii_case(&ledger.source.sha256)
        && version_is_newer_or_undecidable(&gi.version, &ledger.integration.version)
}

/// Is `available` a later version than `installed`? Reuses
/// [`crate::plugins::registry_client::is_newer`]'s semver ordering (a
/// downgrade, or an equal version, is never "newer") so this question is
/// answered the same way everywhere this daemon asks it. The one difference
/// from that helper: when either side fails to parse as semver, THIS
/// function answers `true` rather than `false` — the caller only reaches it
/// after already confirming the two digests differ, so an unparseable
/// version string must fall back to that digest signal (the behaviour
/// before this version gate existed) rather than silently refusing to ever
/// offer an update past a version string a pin or an old ledger did not
/// write as strict semver.
fn version_is_newer_or_undecidable(available: &str, installed: &str) -> bool {
    let both_parse = semver::Version::parse(available.trim()).is_ok()
        && semver::Version::parse(installed.trim()).is_ok();
    if !both_parse {
        return true;
    }
    crate::plugins::registry_client::is_newer(available, installed)
}

/// The universal runner flavour a detected engine plans with — the ONE mapping `PlanInstall`
/// ([`choose_gi`]) and the row's update offer ([`update_for`]) share, so the two cannot disagree.
/// Unity IL2CPP and Unreal have runners of their own; everything else plans with the Mono runner,
/// as before there were three: Godot and an unknown engine are `not-supported` rows, and a caller
/// that plans one anyway is refused by the launcher's own engine check (`engine-mismatch`).
fn universal_flavour(engine: Engine) -> super::pins::Universal {
    use super::pins::Universal;
    match engine {
        Engine::UnityIl2cpp => Universal::UnityIl2cpp,
        Engine::Unreal => Universal::Unreal,
        Engine::UnityMono | Engine::Godot | Engine::Unknown => Universal::UnityMono,
    }
}

fn cached_detect(state: &AppState, game_dir: &Path, hint: Option<&str>) -> Option<Detection> {
    let mtime = std::fs::metadata(game_dir)
        .and_then(|m| m.modified())
        .ok()?;
    let key = (game_dir.to_path_buf(), hint.map(str::to_owned));
    {
        let cache = state.games.detect_cache.lock().unwrap();
        if let Some((cached_mtime, detection)) = cache.get(&key) {
            if *cached_mtime == mtime {
                return Some(detection.clone());
            }
        }
    }
    let detection = astra_game_launcher::detect_with(game_dir, hint).ok()?;
    // Once per game per daemon run (the cache holds it after): what was decided and by which
    // folder, for support. The folder name is the game's install folder, nothing personal.
    tracing::info!(
        game = %game_dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        engine = detection.engine.code(),
        exe = ?detection.exe,
        evidence = ?detection.evidence,
        exe_pick = ?detection.exe_pick,
        steam_hint_used = detection.hint_used,
        "games: detected"
    );
    state
        .games
        .detect_cache
        .lock()
        .unwrap()
        .insert(key, (mtime, detection.clone()));
    Some(detection)
}

/// The program Steam launches for each app (`appcache/appinfo.vdf`), read in ONE pass. A Proton
/// game's entry is the Windows one, so "windows" is asked first; on Linux the apps with no
/// Windows entry are asked again as "linux". A hint is only that: `detect_with` checks it
/// against what sits beside the program and ignores one that does not fit.
/// Steam's `appcache/appinfo.vdf` on this machine: the first Steam install that has one, from
/// the places `steam_library` knows and then the launcher's own list.
fn steam_appinfo_path() -> Option<PathBuf> {
    crate::platform::steam_library::install_candidates()
        .into_iter()
        .chain(astra_game_launcher::steam_roots())
        .map(|root| root.join("appcache").join("appinfo.vdf"))
        .find(|p| p.is_file())
}

fn steam_launch_hints(appids: &[u32]) -> BTreeMap<u32, String> {
    let Some(appinfo) = steam_appinfo_path() else {
        return BTreeMap::new();
    };
    #[allow(unused_mut)]
    let mut hints = astra_game_launcher::launch_executables(&appinfo, appids, "windows");
    #[cfg(not(windows))]
    {
        let rest: Vec<u32> = appids
            .iter()
            .copied()
            .filter(|id| !hints.contains_key(id))
            .collect();
        if !rest.is_empty() {
            hints.extend(astra_game_launcher::launch_executables(
                &appinfo, &rest, "linux",
            ));
        }
    }
    hints
}

// ───────────────────────── Icons ─────────────────────────

/// A 32×32 Steam library-cache icon is small, but nothing stops a corrupt or
/// hand-placed file from being huge — this is the refusal line, not a
/// measurement of a normal icon (those run ~1 KiB).
const MAX_ICON_BYTES: u64 = 64 * 1024;

fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// A path is only ever trusted once it has been canonicalized AND shown to
/// still sit inside `jail` — the one check that stands between "a directory
/// listing" and "whatever a symlink planted in that directory points to".
fn within(jail: &Path, candidate: &Path) -> bool {
    let Ok(jail) = std::fs::canonicalize(jail) else {
        return false;
    };
    match std::fs::canonicalize(candidate) {
        Ok(real) => real.starts_with(&jail),
        Err(_) => false,
    }
}

/// Steam's own small app icon for `appid`, under one Steam root's
/// `appcache/librarycache/`:
///
/// - **New layout**: `<appid>/<40-hex-chars>.jpg` — a top-level file only
///   (`header.jpg`, `library_600x900.jpg` and `logo.png` all fail the
///   hex-stem test on name alone; a subfolder fails the `is_file` test).
/// - **Old layout**: `<appid>_icon.jpg`, directly under `librarycache`.
///
/// Returns `None` when neither exists, the candidate is over
/// [`MAX_ICON_BYTES`], or it resolves outside the `librarycache` folder (see
/// [`within`]) — never a path a crafted symlink could redirect elsewhere.
fn find_icon_file(steam_root: &Path, appid: &str) -> Option<PathBuf> {
    let lib_cache = steam_root.join("appcache").join("librarycache");

    let old = lib_cache.join(format!("{appid}_icon.jpg"));
    if let Ok(meta) = std::fs::metadata(&old) {
        if meta.is_file() && meta.len() <= MAX_ICON_BYTES && within(&lib_cache, &old) {
            return Some(old);
        }
    }

    let dir = lib_cache.join(appid);
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case("jpg") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_hex40(stem) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_ICON_BYTES {
            continue;
        }
        if !within(&lib_cache, &path) {
            continue;
        }
        return Some(path);
    }
    None
}

/// Every Steam root worth asking for an icon, in [`list_games`]'s own order —
/// the install location (which on Windows reads the registry) first, then the
/// crate's own PATH-based search (see `steam_launch_hints`'s identical
/// chain). The library-cache that holds icons lives beside `appcache/`
/// regardless of which LIBRARY (possibly a different drive) a game itself
/// installed to, so this is deliberately Steam ROOTS, never
/// `library_folders()`.
fn icon_file_for(appid: &str) -> Option<(PathBuf, SystemTime)> {
    crate::platform::steam_library::install_candidates()
        .into_iter()
        .chain(astra_game_launcher::steam_roots())
        .find_map(|root| {
            let path = find_icon_file(&root, appid)?;
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            Some((path, mtime))
        })
}

/// `data:image/jpeg;base64,…` for `appid`, or `""` when Steam has no small
/// icon for it. Cached per appid in [`super::GamesRuntime::icon_cache`] keyed
/// by the FOUND FILE's own mtime, so a `ListGames` on every focus refresh
/// does not re-read and re-base64 every icon in the library each time — only
/// a changed (or newly appeared / disappeared) file costs the read.
fn icon_for(state: &AppState, appid: &str) -> String {
    if appid.parse::<u32>().is_err() {
        return String::new();
    }
    let Some((path, mtime)) = icon_file_for(appid) else {
        state.games.icon_cache.lock().unwrap().remove(appid);
        return String::new();
    };
    {
        let cache = state.games.icon_cache.lock().unwrap();
        if let Some((cached_mtime, data_url)) = cache.get(appid) {
            if *cached_mtime == mtime {
                return data_url.clone();
            }
        }
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return String::new();
    };
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return String::new();
    }
    let data_url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    state
        .games
        .icon_cache
        .lock()
        .unwrap()
        .insert(appid.to_string(), (mtime, data_url.clone()));
    data_url
}

/// `Platform::Windows` on a Windows build, unconditionally — the game's own
/// binary flavor does not matter there. On Linux: a native Linux binary is
/// `LinuxNative`, and only that; everything else is `LinuxProton` (prefix from
/// the crate's own `proton_prefix_for`, which reads this game's `steamapps`
/// ancestry). `Binary::Unknown` means no program was found, which is not
/// evidence of a Linux build: GTA III – The Definitive Edition, a Windows
/// build run under Proton, once came out `Unknown` and read "this game's Linux
/// build isn't supported". Its row now says what is true, `exe-not-found`
/// ([`compute_state`]). The launcher names a Linux build when it sees one (a
/// `.x86_64` or extension-less Unity player, an Unreal `-LinuxNoEditor.pak`).
#[cfg(windows)]
fn platform_for(_game_dir: &Path, _appid: Option<u32>, _detection: &Detection) -> Platform {
    Platform::Windows
}

#[cfg(not(windows))]
fn platform_for(game_dir: &Path, appid: Option<u32>, detection: &Detection) -> Platform {
    match detection.binary {
        Binary::Linux => Platform::LinuxNative,
        Binary::Windows | Binary::Unknown => {
            let prefix = appid
                .and_then(|id| astra_game_launcher::proton_prefix_for(game_dir, id))
                .filter(|p| p.is_dir());
            Platform::LinuxProton { prefix }
        }
    }
}

/// A game Steam flags as Valve Anti-Cheat protected carries that as one more anti-cheat kind,
/// so the row's anti-cheat state names it like any kind the folder scan finds.
fn with_vac(mut detection: Detection, vac_flagged: bool) -> Detection {
    if vac_flagged {
        detection
            .anti_cheat
            .push(astra_game_launcher::VALVE_ANTI_CHEAT.to_string());
    }
    detection
}

/// A game whose own source names its program (`pins::SourceTarget::Game`'s `exe`: Death
/// Stranding, which is not Unity; GTA III – The Definitive Edition, whose integration is not the
/// generic Unreal runner's) takes THAT program, not the one engine detection chose. It is looked
/// up under the game's folder one path component at a time (`Gameface/Binaries/Win64/...`),
/// letter case ignored, as a Windows program (it runs under Proton on Linux). Not there: `exe` is
/// `None`, which the row shows as `exe-not-found`. Any other game's detection is returned as it
/// is.
fn with_source_exe(appid: &str, game_dir: &Path, mut detection: Detection) -> Detection {
    let Some(exe) = super::pins::game_source(appid).and_then(|s| s.exe()) else {
        return detection;
    };
    detection.exe = find_relative_ignoring_case(game_dir, exe);
    detection.binary = Binary::Windows;
    detection
}

/// `rel` (`/`-separated) under `root`, each component matched ignoring ASCII case, as it is
/// spelled on disk: every component but the last a folder, the last a file. `None` when any
/// component is missing, empty, `.` or `..`.
fn find_relative_ignoring_case(root: &Path, rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut found = PathBuf::new();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || *part == "." || *part == ".." {
            return None;
        }
        let last = i + 1 == parts.len();
        let name = std::fs::read_dir(root.join(&found))
            .ok()?
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type()
                    .is_ok_and(|t| if last { t.is_file() } else { t.is_dir() })
            })
            .map(|e| e.file_name())
            .find(|n| n.to_str().is_some_and(|n| n.eq_ignore_ascii_case(part)))?;
        found.push(name);
    }
    Some(found)
}

/// The state table from the UX plan, minus `foreign-loader` (a install-time
/// refusal `plan_install` surfaces — pre-computing it here would mean
/// classifying every candidate file's digest for every row on every tab
/// open, which is the expensive half of `plan_install` done twice) and minus
/// `installing`/`launching` (read from `busy` by the caller, above).
/// `own_program`: the game's own source names its program (a game that is not Unity, whose
/// integration names the game itself, not an engine), so the engine rule is skipped; every other
/// rule still applies.
fn compute_state(
    detection: &Detection,
    platform: &Platform,
    installed: bool,
    own_program: bool,
) -> (String, String, BTreeMap<String, String>) {
    if installed {
        return ("installed".into(), String::new(), BTreeMap::new());
    }
    match detection.engine {
        _ if own_program => {}
        // IL2CPP installs the experimental IL2CPP runner (`pins::UNIVERSAL_RUNNER_IL2CPP`); the UI
        // labels it from the row's engine. Except on the Unity lines that runner crashes.
        Engine::UnityIl2cpp => {
            if let Some(version) = il2cpp_runner_refuses(detection) {
                let mut params = BTreeMap::new();
                params.insert("version".to_string(), version);
                return (
                    UNITY_VERSION_NOT_SUPPORTED.into(),
                    UNITY_VERSION_NOT_SUPPORTED.into(),
                    params,
                );
            }
        }
        Engine::Godot | Engine::Unknown => {
            let mut params = BTreeMap::new();
            params.insert("engine".to_string(), detection.engine.code().to_string());
            return ("not-supported".into(), "not-supported".into(), params);
        }
        // Unreal installs the experimental Unreal runner (`pins::UNIVERSAL_RUNNER_UNREAL`) and
        // takes every rule below exactly as a Mono game does.
        Engine::UnityMono | Engine::Unreal => {}
    }
    if matches!(platform, Platform::LinuxNative) {
        return (
            "linux-native-not-supported".into(),
            "linux-native-not-supported".into(),
            BTreeMap::new(),
        );
    }
    if let Some(kind) = detection.anti_cheat.first() {
        let mut params = BTreeMap::new();
        params.insert("kind".to_string(), kind.clone());
        return ("anti-cheat".into(), "anti-cheat".into(), params);
    }
    // Before the prefix: a game whose program was not found is not helped by running it once
    // from Steam, so "run it first" would send the person round a loop that ends here anyway.
    if detection.exe.is_none() {
        return (
            "exe-not-found".into(),
            "exe-not-found".into(),
            BTreeMap::new(),
        );
    }
    if let Platform::LinuxProton { prefix: None } = platform {
        return (
            "proton-prefix-missing".into(),
            "proton-prefix-missing".into(),
            BTreeMap::new(),
        );
    }
    ("ready-to-install".into(), String::new(), BTreeMap::new())
}

/// Not one of the launcher's codes: the game is IL2CPP on a Unity line the IL2CPP runner crashes
/// (`pins::IL2CPP_RUNNER_REFUSES_FROM_UNITY`). A row state and a Plan/Install refusal alike.
const UNITY_VERSION_NOT_SUPPORTED: &str = "unity-version-not-supported";

/// The game's Unity version when it is IL2CPP on a line the IL2CPP runner must not touch. An
/// unknown version is not refused: that is today's behaviour, and the runner is labelled
/// experimental either way.
fn il2cpp_runner_refuses(detection: &Detection) -> Option<String> {
    if !matches!(detection.engine, Engine::UnityIl2cpp) {
        return None;
    }
    let version = detection.unity_version.as_deref()?;
    let parts = astra_game_launcher::unity_version_parts(version)?;
    (parts >= super::pins::IL2CPP_RUNNER_REFUSES_FROM_UNITY).then(|| version.to_string())
}

/// `<data>/games/<appid>`, keyed by appid alone — see `Paths::games_profile_dir`.
fn profile_dir_for(state: &AppState, app: &SteamApp) -> PathBuf {
    state.paths.games_profile_dir(&app.appid)
}

fn invalid_appid_error() -> LauncherError {
    // Not one of the launcher's own codes — this is rejected before any of
    // its types are even built, the same way `busy_error()` names a
    // daemon-level refusal distinctly from the crate's own vocabulary.
    LauncherError::new("invalid-appid")
}

/// Resolve `appid` to everything the launcher needs: the `GameTarget` and
/// this game's profile dir (`plan_install`'s `profile_root` IS the profile
/// folder itself, not a parent — see `Paths::games_profile_dir`).
///
/// `appid` must parse as `u32` BEFORE it is used to build any path — this is
/// the one chokepoint every Install/Play/Uninstall/PlanInstall request
/// passes through, so the check lives here rather than once per caller.
fn game_target(state: &AppState, appid: &str) -> Result<(GameTarget, PathBuf), LauncherError> {
    appid.parse::<u32>().map_err(|_| invalid_appid_error())?;
    let app = installed_apps()
        .into_iter()
        .find(|a| a.appid == appid)
        .ok_or_else(|| LauncherError::new(codes::EXE_NOT_FOUND))?;
    let installdir = app
        .installdir
        .clone()
        .ok_or_else(|| LauncherError::new(codes::EXE_NOT_FOUND))?;
    let game_dir = app
        .library
        .join("steamapps")
        .join("common")
        .join(&installdir);
    let hint = steam_launch_hints(&[appid.parse::<u32>().map_err(|_| invalid_appid_error())?])
        .into_values()
        .next();
    let detection = match cached_detect(state, &game_dir, hint.as_deref()) {
        Some(d) => d,
        None => astra_game_launcher::detect_with(&game_dir, hint.as_deref())?,
    };
    let detection = with_source_exe(appid, &game_dir, detection);
    let exe = detection
        .exe
        .clone()
        .ok_or_else(|| LauncherError::new(codes::EXE_NOT_FOUND))?;
    let appid_num = app.appid.parse::<u32>().ok();
    let platform = platform_for(&game_dir, appid_num, &detection);
    let target = GameTarget {
        dir: game_dir,
        exe,
        steam_appid: appid_num,
        platform,
        // The launcher refuses a game Steam flags as VAC-protected; this is where it reads that.
        // `None` (no appinfo found here) makes it try Steam's usual places itself.
        steam_appinfo: steam_appinfo_path(),
    };
    let profile_dir = profile_dir_for(state, &app);
    Ok((target, profile_dir))
}

// ───────────────────────── The runner cache + fetch ─────────────────────────

/// `astra_game_launcher::Fetch` through `net::http::shared_client()` — the
/// outbound-HTTP chokepoint — called from inside a `spawn_blocking` (see
/// every caller below), so `Handle::block_on` here is driving the async
/// client from a thread that is not the async reactor.
struct DaemonFetch;

impl astra_game_launcher::Fetch for DaemonFetch {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, LauncherError> {
        let handle = tokio::runtime::Handle::current();
        let url = url.to_string();
        handle.block_on(async move {
            let resp = crate::net::http::shared_client()
                .get(&url)
                .send()
                .await
                .map_err(|e| {
                    LauncherError::new(codes::FETCH_FAILED).with("detail", e.to_string())
                })?;
            if !resp.status().is_success() {
                return Err(LauncherError::new(codes::FETCH_FAILED)
                    .with("status", resp.status().to_string()));
            }
            // `Content-Length` first: a server honest about an oversized
            // body is refused before a single byte of it is read.
            if let Some(len) = resp.content_length() {
                if len > max_bytes {
                    return Err(LauncherError::new(codes::TOO_LARGE));
                }
            }
            // Then a RUNNING cap while streaming — `Content-Length` is
            // advisory (absent on a chunked response, or simply wrong), so
            // the real guard is bailing out of the stream itself the moment
            // the running total crosses the limit, rather than buffering an
            // unbounded body in memory and checking only once it is all in.
            let mut stream = resp.bytes_stream();
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| {
                    LauncherError::new(codes::FETCH_FAILED).with("detail", e.to_string())
                })?;
                if body.len() as u64 + chunk.len() as u64 > max_bytes {
                    return Err(LauncherError::new(codes::TOO_LARGE));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        })
    }
}

/// `<data>/games/runners/<sha256>.zip`, content-addressed so the universal
/// runner and every first-party GI share one cache with no collision risk.
fn ensure_runner_cached(state: &AppState, pin: &Gi) -> Result<Vec<u8>, LauncherError> {
    let cache_path = state
        .paths
        .games_runners_dir()
        .join(format!("{}.zip", pin.sha256));
    if let Ok(bytes) = std::fs::read(&cache_path) {
        if digest_matches(&bytes, &pin.sha256) {
            return Ok(bytes);
        }
        // Corrupt or stale — refetch rather than trust a name that lied
        // about its own content.
    }
    let bytes = astra_game_launcher::fetch_verified(
        &DaemonFetch,
        &pin.url,
        &pin.sha256,
        astra_game_launcher::MAX_DOWNLOAD_BYTES,
    )?;
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).map_err(LauncherError::from)?;
    }
    let tmp = cache_path.with_extension("tmp-download");
    std::fs::write(&tmp, &bytes).map_err(LauncherError::from)?;
    std::fs::rename(&tmp, &cache_path).map_err(LauncherError::from)?;
    Ok(bytes)
}

fn digest_matches(bytes: &[u8], expected_sha256: &str) -> bool {
    let mut h = Sha256::new();
    h.update(bytes);
    crate::plugins::bundle::hex(&h.finalize()).eq_ignore_ascii_case(expected_sha256)
}

// ───────────────────────── PlanInstall ─────────────────────────

#[derive(Debug, Clone, Default)]
pub struct PlanOut {
    pub ok: bool,
    pub error_code: String,
    pub error_params: BTreeMap<String, String>,
    pub integration_name: String,
    pub integration_version: String,
    pub integration_authors: String,
    pub integration_license: String,
    pub integration_repo: String,
    pub profile_dir: String,
    pub profile_files: i64,
    pub profile_bytes: i64,
    /// (rel_path, action, sha256) — the gRPC layer maps this to `GameFileAction`.
    pub game_files: Vec<(String, String, String)>,
    pub launch_args: Vec<String>,
    pub warnings: Vec<String>,
    pub used_universal_runner: bool,
    /// The token `Install` must echo back to install THIS plan — see
    /// [`CachedPlan`]. Empty when `ok` is false.
    pub plan_id: String,
    /// True when this plan changes any of `[target]`, `[[game_files]]`,
    /// `[launch]` or `[[config_writes]]` from what is already installed —
    /// see [`consent_needed_for`]. Meaningless when `ok` is false.
    pub consent_needed: bool,
}

fn plan_err(e: LauncherError, used_universal_runner: bool) -> PlanOut {
    PlanOut {
        error_code: e.code.to_string(),
        error_params: e.params,
        used_universal_runner,
        ..Default::default()
    }
}

/// How long a `PlanInstall` response's `plan_id` stays installable
/// (`GamesRuntime::plan_cache`). Short on purpose: consent is tied to the
/// plan the user just saw, not to a token that stays good indefinitely while
/// the game folder underneath it keeps changing.
const PLAN_EXPIRY: Duration = Duration::from_secs(15 * 60);

/// What `Install` actually installs — cached by `PlanInstall` under its own
/// `plan_id` (`GamesRuntime::plan_cache`, keyed by appid, one entry per
/// appid at a time). Carries everything `install_sync` needs to call the
/// launcher's `install()` with the EXACT plan the consent sheet showed,
/// never one re-derived from whatever `use_universal_runner` a later
/// request happens to carry.
#[derive(Debug, Clone)]
pub struct CachedPlan {
    plan_id: String,
    pin: Gi,
    // `plan.profile_dir` (`InstallPlan`'s own field) already carries this —
    // a second copy here would be unread duplication, not a second source
    // of truth.
    plan: InstallPlan,
    target: GameTarget,
    expires_at: Instant,
}

fn join_err(e: tokio::task::JoinError) -> LauncherError {
    LauncherError::new(codes::IO).with("detail", e.to_string())
}

/// Builds the consent sheet AS DATA; writes nothing. Runs the (synchronous,
/// filesystem- and possibly network-bound) work in `spawn_blocking`.
pub async fn plan_install_rpc(
    state: &Arc<AppState>,
    appid: &str,
    use_universal_runner: bool,
    offered_version: &str,
) -> PlanOut {
    let state2 = state.clone();
    let appid2 = appid.to_string();
    let offered = offered_version.trim().to_string();
    let joined = tokio::task::spawn_blocking(move || {
        plan_install_sync(&state2, &appid2, use_universal_runner, &offered)
    })
    .await;
    match joined {
        Ok(Ok(plan)) => plan,
        Ok(Err(e)) => plan_err(e, use_universal_runner),
        Err(je) => plan_err(join_err(je), use_universal_runner),
    }
}

/// Whether accepting this plan needs to re-ask for consent. `true` for a
/// fresh install (`existing` is `None` — nothing to compare against) and
/// `true` when the OLD manifest text cannot even be parsed (an older schema
/// or a damaged file must never read as "nothing changed" — fail safe).
/// Otherwise `true` when ANY of:
/// - the installed `integration.license` or `.authors` differ from the new
///   package's — both structured fields the ledger already carries, read
///   directly rather than re-parsed;
/// - the game files differ — via [`game_files_match`], against the PLAN's
///   real, computed-from-the-actually-shipped-bytes digests
///   (`plan.game_files`), never the manifest's OWN `[[game_files]]` table:
///   that `sha256` is OPTIONAL and every shipped `doorstop_config.ini`
///   omits it, so comparing the manifest let a changed file's CONTENT
///   through the sheet unnoticed (S2, review of bc7c16a1);
/// - `[target]`, `[launch]` or `[[config_writes]]` differ — the OLD
///   manifest, parsed from the ledger's own stored text, against the NEW
///   one.
///
/// - the integration MOVED: another `[integration].id`, or another `repo`
///   where both name one — GTA III going from `astra-reshade` to
///   `astra-gta-trilogy` is a different package from different people, even
///   when its game files, license and authors happen to read the same.
///
/// `[integration].name`/`.version` (a version bump alone changes nothing
/// here) and `[[files]]` (profile-only, never shown on the consent sheet)
/// deliberately do not trigger it on their own.
fn consent_needed_for(existing: Option<&Ledger>, plan: &InstallPlan, new: &Manifest) -> bool {
    // Steam's VAC flag could not be read: the person must see that, every time.
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
    old.target != new.target || old.launch != new.launch || old.config_writes != new.config_writes
}

/// Same rel path, same [`astra_game_launcher::GameAction`], same real
/// shipped digest, on both sides — order does not matter (a `BTreeMap`
/// keyed by rel path absorbs that), but a path present on only one side, or
/// present on both with a different action or digest, means the game-folder
/// shape this plan would leave behind differs from what the user already
/// approved.
fn game_files_match(old: &[LedgerGameFile], new: &[GameFileAction]) -> bool {
    let old_map = old
        .iter()
        .map(|f| (f.rel.as_str(), (f.action, f.sha256.as_str())))
        .collect::<BTreeMap<_, _>>();
    // For a file we place, the plan's `sha256` is the digest the ledger WILL record. For a file
    // the plan leaves alone, the ledger records THAT file's digest — and no digest is compared.
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

/// Whether one game file, as the ledger has it and as the plan would leave it, is the shape the
/// person already approved.
///
/// The same action with the same compared digest ([`compared_digest`]) is. So is a flip between
/// [`GameAction::ReuseKnownProxy`] and [`GameAction::ShareLoader`]: either way a loader proxy is
/// already in the folder and the game runs through it, and the install writes, backs up and
/// removes nothing. Which one a plan says depends only on whether the manager that owns the
/// proxy last linked a build byte-identical to ours — r2modman relinks at every modded start —
/// and re-asking on that would ask the people who keep another mod manager over and over.
fn same_consented_file(old: (GameAction, &str), new: (GameAction, &str)) -> bool {
    use GameAction::{ReuseKnownProxy, ShareLoader};
    match (old.0, new.0) {
        (ReuseKnownProxy, ShareLoader) | (ShareLoader, ReuseKnownProxy) => true,
        (a, b) => a == b && compared_digest(a, old.1) == compared_digest(b, new.1),
    }
}

/// Whether two `[integration].repo` values name one repository. Unknown on either side (a
/// manifest that does not say) proves no move — the integration id is the guard there.
fn same_repo(old: Option<&str>, new: Option<&str>) -> bool {
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

/// The digest the consent sheet shows for one game file: OURS for a file the install places,
/// THEIRS for one it leaves alone (`keep-existing`, `share-loader`) — the file that stays in
/// the folder is the one on disk, not the one we would have shipped.
fn shown_digest(f: &GameFileAction) -> String {
    match f.action {
        GameAction::KeepExisting | GameAction::ShareLoader => {
            f.existing_sha256.clone().unwrap_or_default()
        }
        _ => f.sha256.clone(),
    }
}

/// The digest a consent comparison weighs for a game file: ours, for a file we place. None for a
/// file somebody else owns and the install leaves alone — the user's own `doorstop_config.ini`
/// (`KeepExisting`), or another mod manager's loader the integration runs through
/// (`ShareLoader`, Thunderstore's or r2modman's Doorstop). Their owner rewrites them whenever it
/// likes (r2modman relinks both at every modded start), and what the person approved — "left as
/// it is", "used as it is" — holds for any version of them. Comparing their digest re-asked
/// consent on updates for exactly the people who keep another mod manager beside Astra.
fn compared_digest(action: GameAction, sha256: &str) -> &str {
    match action {
        GameAction::KeepExisting | GameAction::ShareLoader => "",
        _ => sha256,
    }
}

/// The GI `appid` installs: the newest stable release of the source [`super::pins::source_for`]
/// names (`appid`'s own, or its flavour's universal runner), resolved by [`releases::resolve`] —
/// the reused or freshly asked listing, a day-old one when GitHub cannot be asked, and the
/// source's compiled pin when there is none.
fn choose_gi(
    state: &AppState,
    appid: &str,
    use_universal_runner: bool,
    flavour: super::pins::Universal,
) -> Result<Gi, LauncherError> {
    let source = super::pins::source_for(appid, use_universal_runner, flavour);
    releases::resolve(&state.games.release_cache, &DaemonFetch, &source)
}

/// The plan installs a version nobody was shown, so the person must be asked again: an Update
/// that offered `offered` and plans another version, or a request that offered nothing (Install,
/// the reinstall button) over an installed integration of ANOTHER version (a newer release
/// would otherwise go in with no sheet).
fn offer_differs(offered: &str, gi: &Gi, existing: Option<&Ledger>) -> bool {
    let bare = |v: &str| v.trim().trim_start_matches('v').to_string();
    if !offered.is_empty() {
        return bare(offered) != gi.version;
    }
    existing.is_some_and(|l| bare(&l.integration.version) != gi.version)
}

/// A release older than the installed integration of the same source: refused, never a silent
/// downgrade (a withdrawn release makes "the newest" an older one, and offline the compiled
/// fallback can be older than what GitHub delivered earlier). So is any release of a source the
/// installed integration's source has SUCCEEDED (astra-reshade's GTA III over the trilogy's, once
/// every trilogy release carrying it was withdrawn): older by lineage, whatever its number says.
fn refuse_older(gi: &Gi, existing: Option<&Ledger>) -> Result<(), LauncherError> {
    let Some(ledger) = existing else {
        return Ok(());
    };
    let installed = ledger.integration.version.trim();
    let installed_succeeds_it = super::pins::all_sources()
        .any(|s| ledger.integration.id.eq_ignore_ascii_case(s.id) && s.succeeds(&gi.source));
    if installed_succeeds_it {
        // Older by lineage, not by number: `reason` says so, `installed`/`release` stay the
        // two versions as every RELEASE_OLDER carries them.
        return Err(LauncherError::new(releases::RELEASE_OLDER)
            .with("installed", installed)
            .with("release", gi.version.clone())
            .with("reason", "superseded"));
    }
    if !ledger.integration.id.eq_ignore_ascii_case(gi.source.id) {
        return Ok(());
    }
    let older = match (
        semver::Version::parse(&gi.version),
        semver::Version::parse(installed.strip_prefix('v').unwrap_or(installed)),
    ) {
        (Ok(release), Ok(have)) => release < have,
        _ => false,
    };
    if older {
        return Err(LauncherError::new(releases::RELEASE_OLDER)
            .with("installed", installed)
            .with("release", gi.version.clone()));
    }
    Ok(())
}

/// Asks GitHub, in the background, for the releases of every repository an INSTALLED GI follows
/// (each row's `follows`, and every source that has since succeeded it for that game:
/// [`super::pins::update_sources`]) — one listing per repository, at most once per
/// [`releases::LISTING_TTL`] each, and not at all when nothing is installed. A listing that
/// changed re-lists the tab (`GamesChanged`): that is how an Update button appears while
/// `ListGames` itself never waits on the network.
pub fn spawn_release_refresh(state: &Arc<AppState>, rows: &[GameRow]) {
    for (repo, source) in refresh_sources(rows) {
        if !state
            .games
            .release_cache
            .claim_refresh(repo, releases::LISTING_TTL)
        {
            continue;
        }
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let answer = releases::fetch_listing(&DaemonFetch, &source);
            if let Err(e) = &answer {
                tracing::debug!(repo, code = %e.code, "games: release listing refresh failed");
            }
            let changed = state.games.release_cache.record(repo, answer.ok());
            state.games.release_cache.finish(repo);
            if changed {
                state
                    .event_bus
                    .publish(astra_core::AstraEvent::GamesChanged);
            }
        });
    }
}

/// The repositories [`spawn_release_refresh`] lists for `rows`, each with a source of it to ask
/// by (one listing per repository serves every source in it): for every INSTALLED row, the
/// source its ledger names and every source that has since succeeded it for that game
/// ([`super::pins::update_sources`]) — without the latter an astra-reshade GTA III install would
/// never see the trilogy ship, and so never be offered the move.
fn refresh_sources(rows: &[GameRow]) -> BTreeMap<&'static str, super::pins::GiSource> {
    rows.iter()
        .filter(|r| r.installed)
        .filter_map(|r| r.follows.map(|follows| (r, follows)))
        .flat_map(|(r, follows)| super::pins::update_sources(&r.appid, follows))
        .map(|s| (s.repo, s))
        .collect()
}

fn plan_install_sync(
    state: &AppState,
    appid: &str,
    use_universal_runner: bool,
    offered_version: &str,
) -> Result<PlanOut, LauncherError> {
    let (target, profile_dir) = game_target(state, appid)?;
    let hint = target
        .steam_appid
        .and_then(|id| steam_launch_hints(&[id]).remove(&id));
    let detection = cached_detect(state, &target.dir, hint.as_deref());
    // The row already says so; this is for a caller that asks anyway.
    if let Some(version) = detection.as_ref().and_then(il2cpp_runner_refuses) {
        return Err(LauncherError::new(UNITY_VERSION_NOT_SUPPORTED).with("version", version));
    }
    let flavour = universal_flavour(detection.map_or(Engine::Unknown, |d| d.engine));
    let pin = choose_gi(state, appid, use_universal_runner, flavour)?;
    // Read BEFORE `plan_install` (which re-derives and acts on its own read of the same
    // ledger under the lock) — this is only a consent DECISION, never a second source of
    // truth for what install actually does.
    let existing_ledger = installed(&profile_dir).ok().flatten();
    refuse_older(&pin, existing_ledger.as_ref())?;
    let zip = ensure_runner_cached(state, &pin)?;
    let pkg = GiPackage::open(&zip, &pin.sha256)?;
    releases::check_manifest(&pin, appid, pkg.manifest())?;
    let plan = plan_install(&pkg, &target, &profile_dir)?;
    // The version planned is not the one the Update offered (a release appeared in between):
    // the person is asked again rather than handed a version nobody showed them.
    let consent_needed = offer_differs(offered_version, &pin, existing_ledger.as_ref())
        || consent_needed_for(existing_ledger.as_ref(), &plan, pkg.manifest());

    // Cache the plan under a fresh token, keyed by appid — a later
    // `PlanInstall` for the same game simply supersedes the one before it.
    // `Install` must echo this back: consent is tied to the plan the
    // consent sheet actually showed, not to a `use_universal_runner` value
    // a separate request could otherwise supply unchecked.
    let plan_id = mint_token();
    let used_universal_runner = pin.source.is_runner();
    state.games.plan_cache.lock().unwrap().insert(
        appid.to_string(),
        CachedPlan {
            plan_id: plan_id.clone(),
            pin,
            plan: plan.clone(),
            target,
            expires_at: Instant::now() + PLAN_EXPIRY,
        },
    );

    Ok(PlanOut {
        ok: true,
        error_code: String::new(),
        error_params: BTreeMap::new(),
        integration_name: plan.integration.name.clone(),
        integration_version: plan.integration.version.clone(),
        integration_authors: plan.integration.authors.join(", "),
        integration_license: plan.integration.license.clone(),
        integration_repo: plan.integration.repo.clone().unwrap_or_default(),
        profile_dir: plan.profile_dir.display().to_string(),
        profile_files: plan.profile_files as i64,
        profile_bytes: plan.profile_bytes as i64,
        game_files: plan
            .game_files
            .iter()
            .map(|f| {
                (
                    f.rel_path.clone(),
                    f.action.code().to_string(),
                    shown_digest(f),
                )
            })
            .collect(),
        launch_args: plan.launch_args.clone(),
        warnings: plan.warnings.clone(),
        used_universal_runner,
        plan_id,
        consent_needed,
    })
}

// ───────────────────────── Install / Uninstall / Play ─────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ActionOut {
    pub ok: bool,
    pub error_code: String,
    pub error_params: BTreeMap<String, String>,
}

impl ActionOut {
    fn ok() -> Self {
        Self {
            ok: true,
            ..Default::default()
        }
    }
    fn err(e: LauncherError) -> Self {
        Self {
            ok: false,
            error_code: e.code.to_string(),
            error_params: e.params,
        }
    }
}

/// Only one Install/Uninstall/Play may run at a time for a given appid — two
/// Installs racing on one profile directory is exactly the corruption a lock
/// exists to forbid (the crate's own `ProfileLock` guards the ON-DISK race;
/// this guards the DAEMON from starting a second one at all, so "installing"
/// is a state the UI can show instead of a lock-contention error).
///
/// An RAII guard, not a bare bool: [`BusyGuard::drop`] is the ONLY place the
/// appid is cleared, so a handler that returns early through `?` (a plan
/// that failed to build, a cache miss, a join error) cannot leave the appid
/// busy forever waiting for a `clear_busy` call that an early `return` skipped
/// past. `None` when already busy.
struct BusyGuard<'a> {
    games: &'a super::GamesRuntime,
    appid: String,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.games.busy.lock().unwrap().remove(&self.appid);
    }
}

/// Takes `&GamesRuntime` rather than `&AppState` — the narrower borrow is
/// what makes [`BusyGuard`] exercisable without building a whole `AppState`
/// in a test, and it is all this ever needed.
fn mark_busy<'a>(
    games: &'a super::GamesRuntime,
    appid: &str,
    label: &'static str,
) -> Option<BusyGuard<'a>> {
    let mut busy = games.busy.lock().unwrap();
    if busy.contains_key(appid) {
        return None;
    }
    busy.insert(appid.to_string(), label);
    Some(BusyGuard {
        games,
        appid: appid.to_string(),
    })
}

fn busy_error() -> LauncherError {
    // Not one of the launcher's own codes — a daemon-level concurrency
    // guard, named distinctly so the UI never confuses "already working on
    // it" with a launcher refusal.
    LauncherError::new("action-in-progress")
}

/// Install without a valid `plan_id` → refused. Distinct from `plan-stale`
/// (which means the crate itself re-derived the cached plan and found the
/// game folder or profile had changed): this means there is no cached plan
/// to even try — missing, expired (`PLAN_EXPIRY`), or superseded by a later
/// `PlanInstall` for the same appid.
fn plan_required_error() -> LauncherError {
    LauncherError::new("plan-required")
}

/// Takes (removes) the cached plan for `appid` if, and only if, `plan_id`
/// matches it and it has not expired — a one-time read, so a second Install
/// with the same `plan_id` gets `plan-required` rather than silently
/// re-running a plan that already had its chance.
fn take_cached_plan(state: &AppState, appid: &str, plan_id: &str) -> Option<CachedPlan> {
    if plan_id.is_empty() {
        return None;
    }
    let mut cache = state.games.plan_cache.lock().unwrap();
    let still_good = cache
        .get(appid)
        .is_some_and(|c| c.plan_id == plan_id && Instant::now() <= c.expires_at);
    if still_good {
        cache.remove(appid)
    } else {
        None
    }
}

/// `accept_vac_unchecked` is the request's own field, set only by the consent sheet's confirm:
/// a plan whose VAC flag could not be read is refused without it (`vac-unchecked`), and the
/// plan is left in the cache for that confirm.
pub async fn install_rpc(
    state: &Arc<AppState>,
    appid: &str,
    plan_id: &str,
    accept_vac_unchecked: bool,
) -> ActionOut {
    let Some(_busy) = mark_busy(&state.games, appid, "installing") else {
        return ActionOut::err(busy_error());
    };
    if let Err(e) = vac_acceptance(&state.games, appid, plan_id, accept_vac_unchecked) {
        return ActionOut::err(e);
    }
    let Some(cached) = take_cached_plan(state, appid, plan_id) else {
        return ActionOut::err(plan_required_error());
    };
    let state2 = state.clone();
    let joined =
        tokio::task::spawn_blocking(move || install_sync(&state2, &cached, accept_vac_unchecked))
            .await;
    let out = match joined {
        Ok(Ok(())) => ActionOut::ok(),
        Ok(Err(e)) => ActionOut::err(e),
        Err(je) => ActionOut::err(join_err(je)),
    };
    if out.ok {
        state
            .event_bus
            .publish(astra_core::AstraEvent::GamesChanged);
    }
    out
}

/// Refuses an Install of a cached plan whose VAC flag is unread unless the request accepted it.
fn vac_acceptance(
    games: &super::GamesRuntime,
    appid: &str,
    plan_id: &str,
    accept_vac_unchecked: bool,
) -> Result<(), LauncherError> {
    let cache = games.plan_cache.lock().unwrap();
    let unread = cache
        .get(appid)
        .is_some_and(|c| c.plan_id == plan_id && !c.plan.vac_checked);
    if unread && !accept_vac_unchecked {
        return Err(LauncherError::new(codes::VAC_UNCHECKED));
    }
    Ok(())
}

fn install_sync(
    state: &AppState,
    cached: &CachedPlan,
    accept_vac_unchecked: bool,
) -> Result<(), LauncherError> {
    let zip = ensure_runner_cached(state, &cached.pin)?;
    let pkg = GiPackage::open(&zip, &cached.pin.sha256)?;
    // `install` re-derives and checks the plan itself under the profile
    // lock, refusing with `plan-stale` if the game folder or profile moved
    // in between — we do not need to (and must not try to) short-circuit
    // that ourselves.
    // The request's own acceptance, never derived from the plan: such a plan always needs
    // consent (`consent_needed_for`), and only the sheet's confirm sets it.
    let accept = astra_game_launcher::Accept {
        vac_unchecked: accept_vac_unchecked,
    };
    install(
        &pkg,
        &cached.plan,
        &cached.target,
        accept,
        &astra_game_launcher::Ctx::silent(),
    )?;
    prune_runner_cache(state);
    Ok(())
}

pub async fn uninstall_rpc(state: &Arc<AppState>, appid: &str) -> ActionOut {
    let Some(_busy) = mark_busy(&state.games, appid, "uninstalling") else {
        return ActionOut::err(busy_error());
    };
    let state2 = state.clone();
    let appid2 = appid.to_string();
    let joined = tokio::task::spawn_blocking(move || uninstall_sync(&state2, &appid2)).await;
    let out = match joined {
        Ok(Ok(())) => ActionOut::ok(),
        Ok(Err(e)) => ActionOut::err(e),
        Err(je) => ActionOut::err(join_err(je)),
    };
    if out.ok {
        state
            .event_bus
            .publish(astra_core::AstraEvent::GamesChanged);
    }
    out
}

fn uninstall_sync(state: &AppState, appid: &str) -> Result<(), LauncherError> {
    let (target, profile_dir) = game_target(state, appid)?;
    uninstall(&profile_dir, &target)?;
    // The crate leaves files it wrote at RUNTIME (config_writes, the Proton
    // record) in the profile — its own doc says removal of the profile
    // folder itself is the caller's job.
    let _ = std::fs::remove_dir_all(&profile_dir);
    prune_runner_cache(state);
    Ok(())
}

/// Deletes every downloaded GI zip (`<runners>/<sha256>.zip`) that no installed integration's
/// ledger and no pending plan names: an update or an uninstall otherwise leaves every older
/// release's zip behind forever. Best effort; a zip deleted wrongly is only downloaded again.
fn prune_runner_cache(state: &AppState) {
    let mut keep = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(state.paths.games_dir()) {
        for e in entries.flatten() {
            // A profile is `<games>/<appid>`, all digits (`Paths::games_profile_dir`).
            let is_profile = e
                .file_name()
                .to_str()
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            if is_profile && let Ok(Some(ledger)) = installed(&e.path()) {
                keep.insert(ledger.source.sha256.to_ascii_lowercase());
            }
        }
    }
    for cached in state.games.plan_cache.lock().unwrap().values() {
        keep.insert(cached.pin.sha256.to_ascii_lowercase());
    }
    prune_runner_dir(&state.paths.games_runners_dir(), &keep);
}

/// The file half of [`prune_runner_cache`]: removes `<sha256>.zip` files of `dir` whose digest
/// is not in `keep` (lowercase). Anything else in the folder is left.
fn prune_runner_dir(dir: &Path, keep: &std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(sha) = name.to_str().and_then(|n| n.strip_suffix(".zip")) else {
            continue;
        };
        if sha.len() == 64
            && sha.bytes().all(|b| b.is_ascii_hexdigit())
            && !keep.contains(&sha.to_ascii_lowercase())
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `astra_game_launcher::Spawn` for the ACTUAL game process. The crate's own
/// `StdSpawn` already does the Windows `CREATE_BREAKAWAY_FROM_JOB` (with
/// fallback) and Unix new-process-group detach this needs, so there is
/// nothing to add here beyond logging — using `process::hardened_command`
/// instead would have been wrong (it attaches `KillOnDrop` + a pdeathsig,
/// precisely wrong for a process that must outlive this daemon).
struct DaemonSpawn;

impl astra_game_launcher::Spawn for DaemonSpawn {
    fn spawn(&self, plan: &ProcessPlan) -> Result<(), LauncherError> {
        info!(
            target: "process",
            "Launching game process {} (detached from the daemon)",
            plan.program.display(),
        );
        astra_game_launcher::StdSpawn.spawn(plan)
    }
}

/// A fresh opaque random token — the game bridge token the engine and a
/// game's cfg must agree on, and the `plan_id` a `PlanInstall` response
/// hands back. Same randomness requirement either way: unguessable, and
/// never reused across two different sessions or plans.
fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The game bridge token, ONE per install: minted on first use, kept in
/// `<data>/games/bridge-token` (owner-only) and read back on every Play.
///
/// It used to be minted per Play and held only in memory, so a daemon
/// restart in the middle of a game (an update, a crash, the user's own
/// restart) forgot it while the game's cfg still carried it: nothing Astra
/// started afterwards could answer that game. What keeping it costs: the
/// token no longer rotates. Its secrecy is unchanged, because every
/// profile's cfg carries the same value and the launcher writes those with
/// the default umask, so the cfg was always the weaker copy.
fn bridge_token(state: &AppState) -> Result<String, LauncherError> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ONE_AT_A_TIME.lock().unwrap();
    read_or_mint_token(&state.paths.games_bridge_token_file())
}

/// The stored token, or `None` when there is no valid one. Never mints.
fn stored_bridge_token(state: &AppState) -> Option<String> {
    let s = std::fs::read_to_string(state.paths.games_bridge_token_file()).ok()?;
    let s = s.trim();
    is_token(s).then(|| s.to_string())
}

fn read_or_mint_token(path: &Path) -> Result<String, LauncherError> {
    match std::fs::read_to_string(path) {
        Ok(s) if is_token(s.trim()) => return Ok(s.trim().to_string()),
        Ok(_) => tracing::warn!(
            path = %path.display(),
            "games: the stored bridge token is malformed; minting a new one"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Unreadable is not absent: minting over it would leave a game that
        // holds the old value unanswerable, which is the bug this file fixes.
        Err(e) => return Err(LauncherError::io(e)),
    }
    let token = mint_token();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::write_secret_file(path, &token)?;
    Ok(token)
}

/// What [`mint_token`] produces: 32 bytes, URL-safe base64, no padding.
fn is_token(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Prefers Astra's own Steam discovery (`platform::steam_library`, which on
/// Windows reads the registry) over the crate's own `find_steam` on Windows;
/// on Linux the crate's PATH search is exactly what Astra's own would do.
#[cfg(windows)]
fn find_steam_exe() -> Option<PathBuf> {
    crate::platform::steam_library::install_candidates()
        .into_iter()
        .map(|dir| dir.join("steam.exe"))
        .find(|p| p.is_file())
}

#[cfg(not(windows))]
fn find_steam_exe() -> Option<PathBuf> {
    find_steam()
}

pub async fn play_rpc(state: &Arc<AppState>, appid: &str) -> ActionOut {
    let Some(_busy) = mark_busy(&state.games, appid, "launching") else {
        return ActionOut::err(busy_error());
    };
    match play_inner(state, appid).await {
        Ok(()) => {
            state
                .event_bus
                .publish(astra_core::AstraEvent::GamesChanged);
            ActionOut::ok()
        }
        Err(e) => ActionOut::err(e),
    }
}

/// Brings her back into a game Astra launched that outlived the daemon.
///
/// A games-only engine ends with the daemon that spawned it, and Play was
/// the only thing that ever started one, so after a restart in the middle
/// of a game (an update, a crash, the user's own restart) the game kept
/// knocking and nothing answered. Called once at boot, after sign-in has
/// settled. With the desktop companion on there is nothing to do: that
/// engine opens the same door, and the token no longer changes.
pub async fn resume_after_restart(state: Arc<AppState>) {
    if state.settings.load().companion.enabled || !state.auth_manager.is_authorized_live().await {
        return;
    }
    let state2 = state.clone();
    let found = tokio::task::spawn_blocking(move || {
        // Read, never mint: a game launched before this file existed holds a
        // token nobody has any more, and a new one would only start an
        // engine that game can never reach.
        let token = stored_bridge_token(&state2)?;
        running_installed_game(&state2).map(|appid| (appid, token))
    })
    .await;
    let Ok(Some((appid, token))) = found else {
        return;
    };
    let Some(_busy) = mark_busy(&state.games, &appid, "launching") else {
        return;
    };
    // Re-read: the user may have turned the companion on during the scan,
    // and that engine opens the same door on its own.
    let companion_enabled = state.settings.load().companion.enabled;
    match ensure_bridge_engine(&state, companion_enabled, &token).await {
        Ok(pid) => {
            info!(
                appid,
                ?pid,
                "games: the game outlived a restart; she is back for it"
            );
            state
                .event_bus
                .publish(astra_core::AstraEvent::GamesChanged);
        }
        Err(e) => tracing::warn!(
            appid,
            code = %e.code,
            "games: could not resume the running game"
        ),
    }
}

/// The appid of a game with an Astra profile whose program is running now.
/// Best effort, by program file name (the launcher's own `is_running`, the
/// check uninstall makes); a false match costs one games-only engine that
/// exits on its own when no game claims her.
fn running_installed_game(state: &AppState) -> Option<String> {
    let profiles: Vec<String> = std::fs::read_dir(state.paths.games_dir())
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        .filter(|appid| {
            installed(&state.paths.games_profile_dir(appid))
                .ok()
                .flatten()
                .is_some()
        })
        .collect();
    profiles.into_iter().find(|appid| {
        game_target(state, appid).is_ok_and(|(target, _)| {
            target
                .exe
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(astra_game_launcher::is_running)
        })
    })
}

/// Not one of the launcher's own codes. Means: the companion ON cannot say
/// it routes the game bridge, or a games-only engine could not be started
/// (or confirmed) saying so within [`BRIDGE_READY_TIMEOUT`] — either way,
/// Play must refuse rather than ever answer `ok` over an engine that cannot
/// be trusted to serve this game (never a desktop pet the user did not ask
/// for and cannot turn off because nothing told them why it was there).
fn engine_too_old_error() -> LauncherError {
    LauncherError::new("engine-too-old")
}

/// How long Play waits for a freshly spawned games-only engine to subscribe
/// and report its capabilities before giving up.
const BRIDGE_READY_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_READY_POLL: Duration = Duration::from_millis(200);

/// Confirms (or starts) an engine that has said it routes the game bridge,
/// BEFORE Play launches anything that depends on one — S1's first and
/// second findings: Play must refuse with `engine-too-old` rather than ever
/// leave a desktop pet up that nothing confirmed can serve this game, and a
/// second Play must not spawn a competing generation out from under a live
/// games-only session.
///
/// Returns the pid of the games-only engine now backing `token`, or `None`
/// when the desktop companion is the one serving this Play.
///
/// The games-only session is recorded by `start_avatar_generation` itself,
/// under the child lock, in the same act as the spawn (see the comment
/// there); every way out of the readiness wait below still ends it, since
/// the timeout's kill and the supervisor's exit notice both clear it by pid.
async fn ensure_bridge_engine(
    state: &Arc<AppState>,
    companion_enabled: bool,
    token: &str,
) -> Result<Option<u32>, LauncherError> {
    if companion_enabled {
        // The desktop companion is whatever `reconcile` already keeps up —
        // Play neither starts nor stops it, only asks whether it can serve
        // a game.
        let cs = state.companion_state.read().await;
        return if cs.connected && cs.allows_game_bridge() {
            Ok(None)
        } else {
            Err(engine_too_old_error())
        };
    }

    // A live games-only generation is JOINED, not replaced, even while it is
    // still coming up (a second Play, or a Play during a resume): the token
    // is one per install, so it already answers this game, and spawning
    // beside it only met "already running" and refused. The pid is read and
    // the session lock dropped BEFORE asking about the child, because
    // `start_avatar_generation` and `reconcile` take the child lock first.
    let session_pid = state
        .games
        .active_session
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.pid);
    let pid = match session_pid.filter(|&pid| crate::avatar_child_running(state, pid)) {
        Some(pid) => pid,
        None => {
            let daemon_secret = state.client_auth.daemon_secret().to_string();
            let port = *state
                .self_grpc_port
                .get()
                .expect("self_grpc_port is set in main() before any gRPC request can reach Play");
            let Some((pid, exited)) = crate::start_avatar_generation(
                state,
                &daemon_secret,
                port,
                Some(token.to_string()),
            )
            .await
            else {
                return Err(engine_too_old_error());
            };
            tokio::spawn(crate::supervise_avatar_engine(
                state.clone(),
                daemon_secret,
                port,
                pid,
                exited,
            ));
            pid
        }
    };

    let deadline = Instant::now() + BRIDGE_READY_TIMEOUT;
    loop {
        {
            let cs = state.companion_state.read().await;
            if cs.connected && cs.allows_game_bridge() {
                return Ok(Some(pid));
            }
        }
        // `avatar_child_running`, not `avatar_child_is`: a generation that
        // crashed stays in the slot until the next spawn, and waiting out the
        // whole timeout for it helped nobody.
        if Instant::now() >= deadline || !crate::avatar_child_running(state, pid) {
            break;
        }
        tokio::time::sleep(BRIDGE_READY_POLL).await;
    }

    // Timed out, or the generation already died on its own — stop it (only
    // if it is STILL the tracked one; a replacement that has since taken
    // its place must not be killed by a decision about this, older, pid)
    // and refuse.
    if crate::avatar_child_is(state, pid) {
        crate::kill_companion_process(state).await;
    }
    Err(engine_too_old_error())
}

async fn play_inner(state: &Arc<AppState>, appid: &str) -> Result<(), LauncherError> {
    let state2 = state.clone();
    let appid2 = appid.to_string();
    let (target, profile_dir, token) = tokio::task::spawn_blocking(move || {
        let (target, profile_dir) = game_target(&state2, &appid2)?;
        if installed(&profile_dir)?.is_none() {
            return Err(LauncherError::new(codes::NOT_INSTALLED));
        }
        Ok::<_, LauncherError>((target, profile_dir, bridge_token(&state2)?))
    })
    .await
    .map_err(join_err)??;

    let companion_enabled = state.settings.load().companion.enabled;

    let mut vars = BTreeMap::new();
    vars.insert("bridge_port".to_string(), DEFAULT_BRIDGE_PORT.to_string());
    vars.insert("bridge_token".to_string(), token.clone());

    let steam_exe = find_steam_exe();
    let plan = tokio::task::spawn_blocking(move || {
        prepare_launch(&profile_dir, &target, &vars, steam_exe.as_deref())
    })
    .await
    .map_err(join_err)??;

    // Only now — the cfg written — confirm (or start) a bridge-capable
    // engine behind that same token. Order matters: a refusal here must
    // land before `launch()` ever runs the game, and a failed
    // `prepare_launch` above must never reach this far.
    if ensure_bridge_engine(state, companion_enabled, &token)
        .await?
        .is_some()
    {
        state
            .event_bus
            .publish(astra_core::AstraEvent::GamesChanged);
    }

    launch(&plan, &DaemonSpawn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests' games carry no Steam appid, so VAC is never read for them.
    const TEST_ACCEPT: astra_game_launcher::Accept = astra_game_launcher::Accept {
        vac_unchecked: true,
    };
    use std::io::Write;

    use crate::games::pins;
    use astra_game_launcher::{
        Confidence, ConfigWrite, FileRule, GameAction, IntegrationInfo, LedgerGame, LedgerSource,
        LedgerState,
    };

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astra-games-service-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        crate::plugins::bundle::hex(&h.finalize())
    }

    /// Mirrors `astra-game-launcher`'s own zip-building test helper, against
    /// astra-daemon's own `zip` dependency (a different major version of the
    /// same crate than the launcher pins for itself — zip is a stable file
    /// format, not an API-coupled one, so a zip this helper builds is read
    /// correctly by `GiPackage::open` either way).
    fn build_test_zip(manifest_toml: &str, extra_files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            writer.start_file("astra-gi.toml", opts).unwrap();
            writer.write_all(manifest_toml.as_bytes()).unwrap();
            for (name, bytes) in extra_files {
                writer.start_file(*name, opts).unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    fn row(installed: bool, engine: &str, name: &str) -> GameRow {
        GameRow {
            appid: "1".into(),
            name: name.into(),
            engine: engine.into(),
            binary: String::new(),
            state: String::new(),
            refusal_code: String::new(),
            refusal_params: BTreeMap::new(),
            installed,
            has_universal_runner_alt: false,
            icon: String::new(),
            update_available: false,
            installed_version: String::new(),
            available_version: String::new(),
            update_use_universal_runner: false,
            update_integration: String::new(),
            follows: None,
        }
    }

    #[test]
    fn sort_key_puts_installed_first_then_unity_mono_then_the_rest() {
        let mut rows = vec![
            row(false, "unreal", "Zeta"),
            row(true, "unity-mono", "Alpha"),
            row(false, "unity-mono", "Beta"),
        ];
        rows.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        assert_eq!(
            rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            vec!["Alpha", "Beta", "Zeta"],
        );
    }

    fn steam_app(name: &str, installdir: &str) -> SteamApp {
        SteamApp {
            appid: "1".into(),
            name: name.into(),
            installdir: Some(installdir.into()),
            library: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn is_steam_tool_catches_proton_runtime_and_redistributables_by_name_or_installdir() {
        assert!(is_steam_tool(&steam_app("Proton 7.0", "Proton 7.0")));
        assert!(is_steam_tool(&steam_app(
            "Proton Experimental",
            "Proton - Experimental"
        )));
        assert!(is_steam_tool(&steam_app(
            "Steam Linux Runtime 3.0 (sniper)",
            "SteamLinuxRuntime_sniper"
        )));
        assert!(is_steam_tool(&steam_app(
            "Steamworks Common Redistributables",
            "Steamworks Shared"
        )));
        // caught by installdir even if a theoretical renamed listing missed the name check
        assert!(is_steam_tool(&steam_app(
            "Some Redist Bundle",
            "Steamworks Shared"
        )));
        assert!(!is_steam_tool(&steam_app(
            "Lethal Company",
            "Lethal Company"
        )));
    }

    #[test]
    fn the_bridge_token_is_minted_once_and_survives_a_restart() {
        let dir = tempdir();
        let path = dir.join("games").join("bridge-token");
        let first = read_or_mint_token(&path).unwrap();
        assert!(is_token(&first), "{first:?}");
        // A restarted daemon has nothing in memory; the file is the token.
        assert_eq!(read_or_mint_token(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A damaged file is replaced, never handed to a game.
        std::fs::write(&path, "not a token\n").unwrap();
        let second = read_or_mint_token(&path).unwrap();
        assert!(is_token(&second) && second != first);
        assert_eq!(read_or_mint_token(&path).unwrap(), second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_token_file_is_refused_not_overwritten() {
        let dir = tempdir();
        // A directory where the file should be: present, and unreadable as text.
        let path = dir.join("bridge-token");
        std::fs::create_dir_all(&path).unwrap();
        assert!(read_or_mint_token(&path).is_err());
        assert!(path.is_dir(), "nothing was written over it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn game_target_refuses_a_non_numeric_appid_before_touching_any_path() {
        // No fixture, no AppState needed: the check is the first line of the
        // function, before `installed_apps()` is even asked.
        let err = invalid_appid_error();
        assert_eq!(err.code, "invalid-appid");
    }

    #[test]
    fn a_busy_guard_clears_the_appid_on_drop_even_through_an_early_return() {
        let games = super::super::GamesRuntime::default();

        fn early_return(games: &super::super::GamesRuntime) -> Option<()> {
            let _busy = mark_busy(games, "1", "installing")?;
            None // the early return `?` skips past any explicit `clear_busy`
        }
        early_return(&games);
        assert!(
            !games.busy.lock().unwrap().contains_key("1"),
            "the guard must have cleared the appid when it dropped, even on an early return"
        );

        // A second mark while busy is refused.
        let guard = mark_busy(&games, "1", "installing").expect("not busy yet");
        assert!(mark_busy(&games, "1", "installing").is_none());
        drop(guard);
        assert!(!games.busy.lock().unwrap().contains_key("1"));
    }

    fn detection(engine: Engine, anti_cheat: &[&str], exe: Option<&str>) -> Detection {
        Detection {
            engine,
            confidence: Confidence::High,
            exe: exe.map(PathBuf::from),
            binary: Binary::Windows,
            anti_cheat: anti_cheat.iter().map(|s| s.to_string()).collect(),
            evidence: None,
            hint_used: false,
            unity_version: None,
            exe_pick: None,
        }
    }

    #[test]
    fn the_il2cpp_runner_is_refused_on_the_unity_lines_it_crashes() {
        let state_for = |engine: Engine, version: Option<&str>| {
            let mut d = detection(engine, &[], Some("g.exe"));
            d.unity_version = version.map(str::to_string);
            compute_state(&d, &Platform::Windows, false, false)
        };
        let refused = state_for(Engine::UnityIl2cpp, Some("6000.3.1f1"));
        assert_eq!(refused.0, "unity-version-not-supported");
        assert_eq!(
            refused.2.get("version").map(String::as_str),
            Some("6000.3.1f1")
        );
        assert_eq!(
            state_for(Engine::UnityIl2cpp, Some("6000.4.0f1")).0,
            "unity-version-not-supported"
        );
        // The line below, an unknown version, and Mono on the same line all stay installable.
        for (engine, version) in [
            (Engine::UnityIl2cpp, Some("6000.2.9f1")),
            (Engine::UnityIl2cpp, Some("2022.3.62f2")),
            (Engine::UnityIl2cpp, None),
            (Engine::UnityMono, Some("6000.3.15f1")),
        ] {
            assert_eq!(
                state_for(engine, version).0,
                "ready-to-install",
                "{version:?}"
            );
        }
    }

    #[test]
    fn compute_state_matches_the_task_state_table() {
        assert_eq!(
            compute_state(
                &detection(Engine::UnityIl2cpp, &[], Some("g.exe")),
                &Platform::Windows,
                false,
                false
            )
            .0,
            "ready-to-install",
        );
        // Unreal installs the Unreal runner (`pins::UNIVERSAL_RUNNER_UNREAL`) since 2026-10-15.
        assert_eq!(
            compute_state(
                &detection(Engine::Unreal, &[], Some("g.exe")),
                &Platform::Windows,
                false,
                false
            )
            .0,
            "ready-to-install",
        );
        let godot = compute_state(
            &detection(Engine::Godot, &[], Some("g.exe")),
            &Platform::Windows,
            false,
            false,
        );
        assert_eq!(godot.0, "not-supported");
        assert_eq!(godot.2.get("engine").map(String::as_str), Some("godot"));
        assert_eq!(
            compute_state(
                &detection(Engine::Unknown, &[], None),
                &Platform::Windows,
                false,
                false
            )
            .0,
            "not-supported",
        );
        assert_eq!(
            compute_state(
                &detection(Engine::UnityMono, &[], Some("g.exe")),
                &Platform::LinuxNative,
                false,
                false
            )
            .0,
            "linux-native-not-supported",
        );
        assert_eq!(
            compute_state(
                &detection(Engine::UnityMono, &["battleye"], Some("g.exe")),
                &Platform::Windows,
                false,
                false
            )
            .0,
            "anti-cheat",
        );
        assert_eq!(
            compute_state(
                &detection(Engine::UnityMono, &[], Some("g.exe")),
                &Platform::LinuxProton { prefix: None },
                false,
                false
            )
            .0,
            "proton-prefix-missing",
        );
        assert_eq!(
            compute_state(
                &detection(Engine::UnityMono, &[], None),
                &Platform::Windows,
                false,
                false
            )
            .0,
            "exe-not-found",
        );
        assert_eq!(
            compute_state(
                &detection(Engine::UnityMono, &[], Some("g.exe")),
                &Platform::Windows,
                false,
                false
            )
            .0,
            "ready-to-install",
        );
        assert_eq!(
            compute_state(
                &detection(Engine::UnityMono, &[], Some("g.exe")),
                &Platform::Windows,
                true,
                false
            )
            .0,
            "installed",
        );
    }

    #[test]
    fn an_unreal_row_takes_exactly_the_rules_a_mono_row_does() {
        let platforms = [
            Platform::Windows,
            Platform::LinuxNative,
            Platform::LinuxProton { prefix: None },
            Platform::LinuxProton {
                prefix: Some(PathBuf::from("/lib/steamapps/compatdata/4513480/pfx")),
            },
        ];
        let mut ready = 0;
        for platform in &platforms {
            for anti_cheat in [&[][..], &["easy-anti-cheat"][..]] {
                for exe in [
                    None,
                    Some("ScarletSkips/Binaries/Win64/ScarletSkips-Win64-Shipping.exe"),
                ] {
                    for installed in [false, true] {
                        let state = |engine| {
                            compute_state(
                                &detection(engine, anti_cheat, exe),
                                platform,
                                installed,
                                false,
                            )
                        };
                        let unreal = state(Engine::Unreal);
                        assert_eq!(
                            unreal,
                            state(Engine::UnityMono),
                            "{platform:?} {anti_cheat:?} {exe:?} installed={installed}"
                        );
                        ready += usize::from(unreal.0 == "ready-to-install");
                    }
                }
            }
        }
        // Windows and Proton-with-a-prefix, with the program and no anti-cheat: the two rows a
        // Mono game reaches `ready-to-install` on, and so does Unreal.
        assert_eq!(ready, 2);
    }

    /// GTA III – The Definitive Edition as the launcher saw it before it learned the Gameface
    /// layout: Unreal, no program, `Binary::Unknown`. That is a Windows build run under Proton
    /// whose program was not found, and its row says exactly that — never "this game's Linux build
    /// isn't supported", which belongs to a build the launcher names Linux.
    #[test]
    fn a_game_whose_program_was_not_found_reads_exe_not_found_not_linux() {
        let game_dir = tempdir();
        for engine in [Engine::Unreal, Engine::UnityMono] {
            let mut d = detection(engine, &[], None);
            d.binary = Binary::Unknown;
            let platform = platform_for(&game_dir, Some(1546970), &d);
            assert_ne!(platform, Platform::LinuxNative, "{engine:?}");
            assert_eq!(
                compute_state(&d, &platform, false, false).0,
                "exe-not-found",
                "{engine:?}"
            );
            // With a Proton prefix or without one: running the game once from Steam would not
            // find its program, so the row does not ask for that first.
            for platform in [
                Platform::Windows,
                Platform::LinuxProton { prefix: None },
                Platform::LinuxProton {
                    prefix: Some(game_dir.join("pfx")),
                },
            ] {
                assert_eq!(
                    compute_state(&d, &platform, false, false).0,
                    "exe-not-found",
                    "{engine:?} {platform:?}"
                );
            }
        }
        // A build the launcher names Linux keeps the one sentence that says so.
        #[cfg(not(windows))]
        {
            let mut linux = detection(Engine::UnityMono, &[], Some("Game.x86_64"));
            linux.binary = Binary::Linux;
            let platform = platform_for(&game_dir, None, &linux);
            assert_eq!(platform, Platform::LinuxNative);
            assert_eq!(
                compute_state(&linux, &platform, false, false).0,
                "linux-native-not-supported"
            );
        }
        let _ = std::fs::remove_dir_all(&game_dir);
    }

    /// GTA III – The Definitive Edition's shape, through the launcher this daemon pins: no
    /// `-Win64-Shipping.exe`, the program is `Gameface/Binaries/Win64/LibertyCity.exe` — not the
    /// `PlayGTA3.exe` bootstrapper Steam launches, nor an installer in `Redistributables/`, both
    /// larger. A Windows build that runs under Proton: ready to install once Steam made its prefix.
    #[test]
    fn gta3_definitive_edition_is_an_unreal_windows_game_ready_to_install() {
        let library = tempdir();
        let game_dir = library.join("steamapps/common/GTA III - The Definitive Edition");
        let put = |rel: &str, bytes: &[u8]| {
            let p = game_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        };
        put("PlayGTA3.exe", &[b'M'; 4096]);
        put("Redistributables/vc_redist.x64.exe", &[b'M'; 8192]);
        put("Gameface/Binaries/Win64/LibertyCity.exe", b"MZ");
        put(
            "Gameface/Content/Paks/pakchunk0-WindowsNoEditor.pak",
            b"pak",
        );

        let d = astra_game_launcher::detect_with(&game_dir, Some("PlayGTA3.exe")).unwrap();
        assert_eq!(
            (d.engine, d.binary, d.exe.as_deref(), d.hint_used),
            (
                Engine::Unreal,
                Binary::Windows,
                Some(Path::new("Gameface/Binaries/Win64/LibertyCity.exe")),
                false
            )
        );
        #[cfg(not(windows))]
        {
            let platform = platform_for(&game_dir, Some(1546970), &d);
            assert_eq!(platform, Platform::LinuxProton { prefix: None });
            assert_eq!(
                compute_state(&d, &platform, false, false).0,
                "proton-prefix-missing"
            );
            std::fs::create_dir_all(library.join("steamapps/compatdata/1546970/pfx")).unwrap();
        }
        let platform = platform_for(&game_dir, Some(1546970), &d);
        assert_eq!(
            compute_state(&d, &platform, false, false).0,
            "ready-to-install"
        );
        // Its own source names the same program, found by path whatever its letter case, and
        // the row skips the engine rule as for any game whose source names its program.
        let own = with_source_exe("1546970", &game_dir, d.clone());
        assert_eq!(own.exe, d.exe);
        assert_eq!(own.binary, Binary::Windows);
        let own_program = pins::game_source("1546970").is_some_and(|s| s.exe().is_some());
        assert!(own_program);
        assert_eq!(
            compute_state(&own, &platform, false, own_program).0,
            "ready-to-install"
        );
        std::fs::rename(
            game_dir.join("Gameface/Binaries/Win64/LibertyCity.exe"),
            game_dir.join("Gameface/Binaries/Win64/LIBERTYCITY.EXE"),
        )
        .unwrap();
        assert_eq!(
            with_source_exe("1546970", &game_dir, d.clone())
                .exe
                .as_deref(),
            Some(Path::new("Gameface/Binaries/Win64/LIBERTYCITY.EXE"))
        );
        let _ = std::fs::remove_dir_all(&library);
    }

    #[test]
    fn a_relative_program_path_is_found_component_by_component_and_never_escapes() {
        let root = tempdir();
        std::fs::create_dir_all(root.join("GameFace/binaries/WIN64")).unwrap();
        std::fs::write(root.join("GameFace/binaries/WIN64/LibertyCity.exe"), b"MZ").unwrap();
        // A folder named like the program is not the program.
        std::fs::create_dir_all(root.join("GameFace/binaries/WIN64/Other.exe")).unwrap();
        assert_eq!(
            find_relative_ignoring_case(&root, "Gameface/Binaries/Win64/LibertyCity.exe"),
            Some(PathBuf::from("GameFace/binaries/WIN64/LibertyCity.exe"))
        );
        for rel in [
            "Gameface/Binaries/Win64/Other.exe",
            "Gameface/Binaries/Missing.exe",
            "Gameface//Binaries/Win64/LibertyCity.exe",
            "../x/LibertyCity.exe",
            "./Gameface/Binaries/Win64/LibertyCity.exe",
            "",
        ] {
            assert_eq!(find_relative_ignoring_case(&root, rel), None, "{rel:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn universal_flavour_gives_unreal_its_own_runner_and_everything_else_what_it_had() {
        use pins::Universal;
        assert_eq!(universal_flavour(Engine::Unreal), Universal::Unreal);
        assert_eq!(
            universal_flavour(Engine::UnityIl2cpp),
            Universal::UnityIl2cpp
        );
        // Mono, and the engines nothing installs for (the launcher refuses them by engine).
        for engine in [Engine::UnityMono, Engine::Godot, Engine::Unknown] {
            assert_eq!(
                universal_flavour(engine),
                Universal::UnityMono,
                "{engine:?}"
            );
        }
        assert_eq!(
            universal_flavour(Engine::Unreal).runner().id,
            pins::UNIVERSAL_RUNNER_UNREAL.id
        );
        // Each runner's manifest is held to its engine by the launcher's own spelling of it.
        for engine in [Engine::UnityMono, Engine::UnityIl2cpp, Engine::Unreal] {
            assert_eq!(
                universal_flavour(engine).runner().target,
                pins::SourceTarget::Runner {
                    engine: engine.code()
                },
                "{engine:?}"
            );
        }
    }

    /// `astra-game-launcher`'s own manifest schema (`src/manifest.rs`): no
    /// `engine` key, so `plan_install`'s engine check is skipped — this test
    /// is about the file-placement/ledger round trip, not engine matching.
    const MANIFEST: &str = r#"
schema = 1

[integration]
id = "astra.test-runner"
name = "Astra Test Runner"
version = "0.1.0"
authors = ["mihailinl"]
license = "MIT"
repo = "mihailinl/astra-test-runner"

[target]
exe = "*"
platforms = ["windows-x64"]
anti_cheat = "none"

[[files]]
from = "core.dll"
to = "${profile}/BepInEx/core/core.dll"

[[game_files]]
from = "winhttp.dll"
to = "${game}/winhttp.dll"
sha256 = "__WINHTTP_SHA__"

[launch]
args = ["--doorstop-enabled", "true"]
"#;

    #[test]
    fn plan_install_then_install_then_uninstall_round_trips_against_the_real_crate() {
        let winhttp = b"fake winhttp bytes";
        let manifest = MANIFEST.replace("__WINHTTP_SHA__", &sha256_hex(winhttp));
        let zip_bytes = build_test_zip(
            &manifest,
            &[("core.dll", b"core bytes"), ("winhttp.dll", winhttp)],
        );
        let digest = sha256_hex(&zip_bytes);

        let pkg = GiPackage::open(&zip_bytes, &digest).expect("a well-formed package should open");
        assert_eq!(pkg.manifest().integration.name, "Astra Test Runner");

        // Two INDEPENDENT roots — `plan_install` refuses a profile nested
        // inside the game dir ("profile-overlaps-game"), the same jail rule
        // `Paths::games_profile_dir` already keeps outside `steamapps/common`.
        let tmp = tempdir();
        std::fs::write(tmp.join("game.exe"), b"exe").unwrap();
        let target = GameTarget {
            dir: tmp.clone(),
            exe: PathBuf::from("game.exe"),
            steam_appid: None,
            platform: Platform::Windows,
            steam_appinfo: None,
        };
        let profile = tempdir();

        let plan = plan_install(&pkg, &target, &profile)
            .expect("plan_install should accept this manifest");
        assert_eq!(plan.game_files.len(), 1);
        assert_eq!(plan.game_files[0].action, GameAction::Add);

        let ledger = install(
            &pkg,
            &plan,
            &target,
            TEST_ACCEPT,
            &astra_game_launcher::Ctx::silent(),
        )
        .expect("install should place the files the plan named");
        assert_eq!(ledger.integration.id, "astra.test-runner");
        assert!(
            profile
                .join("BepInEx")
                .join("core")
                .join("core.dll")
                .is_file()
        );
        assert_eq!(std::fs::read(tmp.join("winhttp.dll")).unwrap(), winhttp);

        assert!(installed(&profile).unwrap().is_some());

        // A second plan against the now-installed profile reports `Add` again
        // (the crate's own semantics: it plans as though the existing ledger
        // is undone first — `install.rs`'s own doc, "an update is one plan
        // plus install; the old integration is uninstalled under the lock")
        // and warns that it REPLACES the integration already there.
        let plan2 = plan_install(&pkg, &target, &profile).unwrap();
        assert_eq!(plan2.game_files[0].action, GameAction::Add);
        assert!(plan2.warnings.iter().any(|w| w == "replaces-integration"));

        let report = uninstall(&profile, &target).expect("uninstall should undo what install did");
        assert_eq!(report.removed, vec!["winhttp.dll".to_string()]);
        assert!(!tmp.join("winhttp.dll").exists());

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn open_refuses_a_digest_mismatch() {
        let winhttp = b"fake winhttp bytes";
        let manifest = MANIFEST.replace("__WINHTTP_SHA__", &sha256_hex(winhttp));
        let zip_bytes = build_test_zip(&manifest, &[("core.dll", b"x"), ("winhttp.dll", winhttp)]);
        let err = GiPackage::open(&zip_bytes, &"0".repeat(64)).unwrap_err();
        assert_eq!(err.code, codes::DIGEST_MISMATCH);
    }

    // ── Icon picker ──────────────────────────────────────────────────────

    #[test]
    fn find_icon_file_picks_the_new_layout_top_level_jpg_with_a_40_hex_stem() {
        let root = tempdir();
        let dir = root.join("appcache").join("librarycache").join("123");
        std::fs::create_dir_all(&dir).unwrap();
        let hex = "a".repeat(40);
        std::fs::write(dir.join(format!("{hex}.jpg")), b"icon bytes").unwrap();
        // Decoys the picker must ignore: known non-icon top-level files, and
        // a subfolder that would otherwise also pass the hex-stem test.
        std::fs::write(dir.join("header.jpg"), b"not this").unwrap();
        std::fs::write(dir.join("library_600x900.jpg"), b"not this").unwrap();
        std::fs::write(dir.join("logo.png"), b"not this").unwrap();
        let sub = dir.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("{}.jpg", "b".repeat(40))), b"ignored").unwrap();

        let found = find_icon_file(&root, "123").expect("should find the new-layout icon");
        assert_eq!(found, dir.join(format!("{hex}.jpg")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_icon_file_falls_back_to_the_old_layout() {
        let root = tempdir();
        let lib_cache = root.join("appcache").join("librarycache");
        std::fs::create_dir_all(&lib_cache).unwrap();
        std::fs::write(lib_cache.join("456_icon.jpg"), b"icon bytes").unwrap();

        let found = find_icon_file(&root, "456").expect("should find the old-layout icon");
        assert_eq!(found, lib_cache.join("456_icon.jpg"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_icon_file_returns_none_when_there_is_no_icon_at_all() {
        let root = tempdir();
        std::fs::create_dir_all(root.join("appcache").join("librarycache")).unwrap();
        assert!(find_icon_file(&root, "789").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_icon_file_skips_a_file_over_the_size_cap() {
        let root = tempdir();
        let dir = root.join("appcache").join("librarycache").join("999");
        std::fs::create_dir_all(&dir).unwrap();
        let hex = "c".repeat(40);
        let big = vec![0u8; (MAX_ICON_BYTES + 1) as usize];
        std::fs::write(dir.join(format!("{hex}.jpg")), &big).unwrap();

        assert!(find_icon_file(&root, "999").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_icon_file_ignores_a_non_hex_stem() {
        let root = tempdir();
        let dir = root.join("appcache").join("librarycache").join("111");
        std::fs::create_dir_all(&dir).unwrap();
        // Forty characters, but not hex — must not be mistaken for the
        // library-cache naming convention.
        let not_hex = "z".repeat(40);
        std::fs::write(dir.join(format!("{not_hex}.jpg")), b"x").unwrap();

        assert!(find_icon_file(&root, "111").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Update available (row) ──────────────────────────────────────────
    //
    // Identities below are the REAL `[integration]` tables (read via `git show
    // <tag>:<path>` on this machine, not invented) — see `pins.rs`'s own
    // `REAL_*` fixtures for exactly where each came from. A first cut of this
    // feature invented "mihailinl/astra-lethal"-shorthand fixtures and every
    // test passed while `update_for` matched not one real ledger (review of
    // bc7c16a1).

    const REAL_UNIVERSAL_MONO_ID: &str = "astra.unity";
    const REAL_UNIVERSAL_IL2CPP_ID: &str = "astra.unity-il2cpp";
    const REAL_BEPINEX_REPO: &str = "https://github.com/mihailinl/astra-bepinex";
    const REAL_LETHAL_ID: &str = "astra.lethal-company";
    const REAL_LETHAL_REPO: &str = "https://github.com/mihailinl/astra-lethal";

    fn test_ledger(repo: Option<&str>, id: &str, version: &str, sha256: &str) -> Ledger {
        Ledger {
            schema: astra_game_launcher::LEDGER_SCHEMA,
            state: LedgerState::Installed,
            integration: IntegrationInfo {
                id: id.to_string(),
                name: "Test".to_string(),
                version: version.to_string(),
                authors: vec![],
                license: String::new(),
                repo: repo.map(str::to_string),
            },
            source: LedgerSource {
                sha256: sha256.to_string(),
            },
            game: LedgerGame {
                dir: PathBuf::from("/tmp/game"),
                exe: "game.exe".to_string(),
            },
            manifest: String::new(),
            files: vec![],
            game_files: vec![],
            proton: vec![],
            vac_unchecked_accepted: false,
            loader_baseline: Vec::new(),
        }
    }

    const DS_REPO: &str = "https://github.com/mihailinl/astra-in-death-stranding";

    /// A cache holding one answered listing of `sources[0]`'s repository: each `(tag, n)` one
    /// stable release carrying the asset of every source in `sources` (one repository), digest
    /// `n` repeated.
    fn cache_with(sources: &[pins::GiSource], tags: &[(&str, u8)]) -> ReleaseCache {
        let cache = ReleaseCache::default();
        let listing = tags
            .iter()
            .map(|(tag, n)| {
                let mut r = releases::test_release(&sources[0], tag, *n);
                r.assets = sources
                    .iter()
                    .map(|s| releases::test_release(s, tag, *n).assets.remove(0))
                    .collect();
                r
            })
            .collect();
        cache.record(sources[0].repo, Some(listing));
        cache
    }

    fn ds(appid: &str) -> pins::GiSource {
        pins::game_source(appid).expect("a built-in source")
    }

    fn bepinex_cache(tags: &[(&str, u8)]) -> ReleaseCache {
        cache_with(
            &[pins::UNIVERSAL_RUNNER, pins::UNIVERSAL_RUNNER_IL2CPP],
            tags,
        )
    }

    /// **An older install is offered the newest listed release of its own source; one already
    /// at it is offered nothing** — for a game's own integration (each Death Stranding game its
    /// own zip of one release) exactly as for a runner.
    #[test]
    fn update_offers_the_newest_release_of_the_same_source_and_nothing_when_at_it() {
        let d = detection(Engine::Unknown, &[], Some("ds.exe"));
        let cache = cache_with(
            &[ds("1850570"), ds("3280350")],
            &[("v0.1.1", 0x11), ("v0.3.0", 0x30), ("v0.2.0", 0x20)],
        );
        let ds1_old = test_ledger(
            Some(DS_REPO),
            "astra.death-stranding",
            "0.1.1",
            &"11".repeat(32),
        );
        let (gi, universal) = update_for("1850570", &d, &ds1_old, &cache).expect("DS1 update");
        assert_eq!(
            (gi.tag.as_str(), gi.version.as_str(), universal),
            ("v0.3.0", "0.3.0", false)
        );
        assert!(gi.url.ends_with("/v0.3.0/astra-gi-ds1.zip"));
        assert_eq!(gi.source, ds("1850570"));
        // Death Stranding 2 is offered ITS zip of the same release.
        let ds2_old = test_ledger(
            Some(DS_REPO),
            "astra.death-stranding-2",
            "0.1.1",
            &"11".repeat(32),
        );
        let (gi2, _) = update_for("3280350", &d, &ds2_old, &cache).expect("DS2 update");
        assert!(gi2.url.ends_with("/v0.3.0/astra-gi-ds2.zip"));
        // Already at the newest: nothing.
        let ds1_now = test_ledger(
            Some(DS_REPO),
            "astra.death-stranding",
            "0.3.0",
            &"30".repeat(32),
        );
        assert!(update_for("1850570", &d, &ds1_now, &cache).is_none());

        // A runner the same way.
        let mono = detection(Engine::UnityMono, &[], Some("g.exe"));
        let cache = bepinex_cache(&[("v0.8.1", 0x81), ("v0.9.0", 0x90)]);
        let old = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.8.1",
            &"81".repeat(32),
        );
        let (gi, universal) = update_for("999999", &mono, &old, &cache).expect("Mono update");
        assert_eq!((gi.version.as_str(), universal), ("0.9.0", true));
        assert_eq!(gi.source, pins::UNIVERSAL_RUNNER);
        let now = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.9.0",
            &"90".repeat(32),
        );
        assert!(update_for("999999", &mono, &now, &cache).is_none());
    }

    /// With no listing (GitHub never answered), Update offers nothing — not the compiled pin,
    /// whatever version is installed.
    #[test]
    fn with_no_listing_update_offers_nothing_not_the_fallback() {
        let mono = detection(Engine::UnityMono, &[], Some("g.exe"));
        let ancient = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.1.0",
            &"01".repeat(32),
        );
        assert!(update_for("999999", &mono, &ancient, &ReleaseCache::default()).is_none());
        let d = detection(Engine::Unknown, &[], Some("ds.exe"));
        let ds1 = test_ledger(
            Some(DS_REPO),
            "astra.death-stranding",
            "0.0.1",
            &"01".repeat(32),
        );
        assert!(update_for("1850570", &d, &ds1, &ReleaseCache::default()).is_none());
    }

    /// A Unity game with its own integration follows ITS repository; installed with the
    /// universal runner instead, it follows the runner's — each offered only its own source's
    /// release, never steered to the other.
    #[test]
    fn a_unity_games_own_source_and_its_universal_alternative_each_follow_their_own() {
        let lethal = pins::game_source("1966720").unwrap();
        let d = detection(Engine::UnityMono, &[], Some("Lethal Company.exe"));
        let lethal_cache = cache_with(&[lethal], &[("v0.2.0", 0x20), ("v0.3.0", 0x30)]);
        let own = test_ledger(
            Some(REAL_LETHAL_REPO),
            REAL_LETHAL_ID,
            "0.2.0",
            &"20".repeat(32),
        );
        let (gi, universal) = update_for("1966720", &d, &own, &lethal_cache).expect("own update");
        assert_eq!(
            (gi.source, gi.version.as_str(), universal),
            (lethal, "0.3.0", false)
        );
        assert!(
            gi.url
                .starts_with("https://github.com/mihailinl/astra-lethal/")
        );

        let runner = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.8.1",
            &"81".repeat(32),
        );
        // Lethal Company's listing says nothing about the runner.
        assert!(update_for("1966720", &d, &runner, &lethal_cache).is_none());
        let runner_cache = bepinex_cache(&[("v0.9.0", 0x90)]);
        let (gi, universal) =
            update_for("1966720", &d, &runner, &runner_cache).expect("runner update");
        assert_eq!((gi.source, universal), (pins::UNIVERSAL_RUNNER, true));
        // And the by-name ledger is never offered the runner's release.
        assert!(update_for("1966720", &d, &own, &runner_cache).is_none());
        assert_eq!(
            pins::source_for("1966720", universal, universal_flavour(Engine::UnityMono)),
            pins::UNIVERSAL_RUNNER,
            "the offer's use_universal_runner plans the runner"
        );
    }

    #[test]
    fn an_unread_vac_plan_installs_only_with_the_requests_own_accept() {
        let games = crate::games::GamesRuntime::default();
        let mut plan = plan_for(
            IntegrationInfo {
                id: "astra.death-stranding".into(),
                name: "x".into(),
                version: "0.1.0".into(),
                authors: vec![],
                license: "MIT".into(),
                repo: None,
            },
            vec![],
        );
        plan.vac_checked = false;
        games.plan_cache.lock().unwrap().insert(
            "1850570".into(),
            CachedPlan {
                plan_id: "p1".into(),
                pin: releases::fallback_gi(&pins::UNIVERSAL_RUNNER).unwrap(),
                plan: plan.clone(),
                target: GameTarget {
                    dir: PathBuf::from("/tmp/game"),
                    exe: PathBuf::from("ds.exe"),
                    steam_appid: None,
                    platform: Platform::Windows,
                    steam_appinfo: None,
                },
                expires_at: Instant::now() + PLAN_EXPIRY,
            },
        );
        let e = vac_acceptance(&games, "1850570", "p1", false).unwrap_err();
        assert_eq!(e.code, codes::VAC_UNCHECKED);
        assert!(vac_acceptance(&games, "1850570", "p1", true).is_ok());
        // A checked plan needs no accept.
        games
            .plan_cache
            .lock()
            .unwrap()
            .get_mut("1850570")
            .unwrap()
            .plan
            .vac_checked = true;
        assert!(vac_acceptance(&games, "1850570", "p1", false).is_ok());
    }

    #[test]
    fn an_update_planned_for_another_version_than_offered_asks_again() {
        let gi = releases::fallback_gi(&pins::UNIVERSAL_RUNNER).unwrap();
        assert!(
            !offer_differs("", &gi, None),
            "a fresh install offered nothing"
        );
        assert!(!offer_differs(&gi.version, &gi, None));
        assert!(!offer_differs(&format!("v{}", gi.version), &gi, None));
        assert!(offer_differs("0.0.1", &gi, None));
        // No offer (Install, the reinstall button) over an installed integration: the same
        // version reinstalls quietly, another one asks.
        let same = test_ledger(None, "astra.unity", &gi.version, &gi.sha256);
        assert!(!offer_differs("", &gi, Some(&same)));
        let older = test_ledger(None, "astra.unity", "0.0.1", &"0".repeat(64));
        assert!(offer_differs("", &gi, Some(&older)));
    }

    #[test]
    fn a_vac_game_shows_as_anti_cheat_with_its_kind() {
        let d = with_vac(detection(Engine::UnityMono, &[], Some("g.exe")), true);
        let (state, code, params) = compute_state(&d, &Platform::Windows, false, false);
        assert_eq!(
            (state.as_str(), code.as_str()),
            ("anti-cheat", "anti-cheat")
        );
        assert_eq!(
            params.get("kind").map(String::as_str),
            Some("valve-anti-cheat")
        );
        let clear = with_vac(detection(Engine::UnityMono, &[], Some("g.exe")), false);
        assert_eq!(
            compute_state(&clear, &Platform::Windows, false, false).0,
            "ready-to-install"
        );
    }

    #[test]
    fn only_zips_no_ledger_or_plan_names_are_pruned() {
        let dir = tempdir();
        let keep_sha = "a".repeat(64);
        let drop_sha = "B".repeat(64);
        for name in [
            format!("{keep_sha}.zip"),
            format!("{drop_sha}.zip"),
            "notes.txt".to_string(),
            "short.zip".to_string(),
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        prune_runner_dir(&dir, &std::collections::HashSet::from([keep_sha.clone()]));
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                format!("{keep_sha}.zip"),
                "notes.txt".into(),
                "short.zip".into()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A release older than the installed integration of the same source is refused — for a
    /// runner as for a game's own integration, and so for the offline fallback when GitHub
    /// delivered something newer earlier: never a silent downgrade.
    #[test]
    fn a_release_older_than_the_installed_one_is_refused() {
        let mut gi = releases::fallback_gi(&ds("1850570")).unwrap();
        gi.version = "0.1.0".into();
        let newer = test_ledger(
            Some(DS_REPO),
            "astra.death-stranding",
            "0.2.0",
            &"2".repeat(64),
        );
        let e = refuse_older(&gi, Some(&newer)).unwrap_err();
        assert_eq!(e.code, releases::RELEASE_OLDER);
        let same = test_ledger(
            Some(DS_REPO),
            "astra.death-stranding",
            "0.1.0",
            &"1".repeat(64),
        );
        assert!(refuse_older(&gi, Some(&same)).is_ok());
        assert!(refuse_older(&gi, None).is_ok());
        // Another integration in the profile (not this source's): not a downgrade of it.
        let other = test_ledger(None, "astra.unity", "9.0.0", &"3".repeat(64));
        assert!(refuse_older(&gi, Some(&other)).is_ok());
        // The Mono runner's fallback (0.8.1) over a 0.9.0 GitHub delivered: refused.
        let fallback = releases::fallback_gi(&pins::UNIVERSAL_RUNNER).unwrap();
        let delivered = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.9.0",
            &"9".repeat(64),
        );
        assert_eq!(
            refuse_older(&fallback, Some(&delivered)).unwrap_err().code,
            releases::RELEASE_OLDER
        );
    }

    #[test]
    fn a_game_naming_its_program_is_installable_whatever_engine_detection_says() {
        let unknown = detection(Engine::Unknown, &[], Some("ds.exe"));
        assert_eq!(
            compute_state(&unknown, &Platform::Windows, false, true).0,
            "ready-to-install"
        );
        assert_eq!(
            compute_state(&unknown, &Platform::Windows, false, false).0,
            "not-supported"
        );
        // Every other rule still holds for it.
        let eac = detection(Engine::Unknown, &["easy-anti-cheat"], Some("ds.exe"));
        assert_eq!(
            compute_state(&eac, &Platform::Windows, false, true).0,
            "anti-cheat"
        );
        let no_exe = detection(Engine::Unknown, &[], None);
        assert_eq!(
            compute_state(&no_exe, &Platform::Windows, false, true).0,
            "exe-not-found"
        );
    }

    #[test]
    fn a_games_program_is_its_sources_whatever_its_letter_case() {
        let dir = tempdir();
        std::fs::write(dir.join("DS.EXE"), b"MZ").unwrap();
        std::fs::write(dir.join("other.exe"), b"MZ").unwrap();
        std::fs::create_dir_all(dir.join("ds2.exe")).unwrap();
        let d = detection(Engine::Unknown, &[], Some("other.exe"));
        let ds1 = with_source_exe("1850570", &dir, d.clone());
        assert_eq!(ds1.exe.as_deref(), Some(Path::new("DS.EXE")));
        assert_eq!(ds1.binary, Binary::Windows);
        // DS2's program is not there (a folder of that name is not a program).
        assert_eq!(with_source_exe("3280350", &dir, d.clone()).exe, None);
        // A Unity game with its own source names no program: detection as it was.
        assert_eq!(with_source_exe("1966720", &dir, d.clone()).exe, d.exe);
        // Any other game: detection as it was.
        assert_eq!(with_source_exe("999999", &dir, d.clone()).exe, d.exe);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_flavour_that_changed_since_install_is_not_offered_as_an_update() {
        // Installed with the Mono runner; the game is IL2CPP today. Plan would pick the
        // IL2CPP runner, so offering "Mono 0.6.0 -> 0.9.0" would promise something else.
        let ledger = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.6.0",
            "0".repeat(64).as_str(),
        );
        let cache = bepinex_cache(&[("v0.9.0", 0x90)]);
        let mono = detection(Engine::UnityMono, &[], Some("g.exe"));
        assert!(
            update_for("999999", &mono, &ledger, &cache).is_some(),
            "the listing does offer it on a Mono game"
        );
        let d = detection(Engine::UnityIl2cpp, &[], Some("g.exe"));
        assert!(update_for("999999", &d, &ledger, &cache).is_none());
        // A Mono runner installed on a game whose own source names its program: Plan would
        // install that game's own integration, so the runner is not offered either.
        let ds_detection = detection(Engine::Unknown, &[], Some("ds.exe"));
        assert!(update_for("1850570", &ds_detection, &ledger, &cache).is_none());
    }

    /// The `[integration]` of astra-reshade's v0.2.1 release (`pins.rs`'s `REAL_UNREAL_ID`).
    const REAL_UNREAL_ID: &str = "astra.unreal";
    const REAL_RESHADE_REPO: &str = "https://github.com/mihailinl/astra-reshade";

    #[test]
    fn an_unreal_install_is_offered_the_unreal_runner_and_only_on_an_unreal_game() {
        let ledger = test_ledger(
            Some(REAL_RESHADE_REPO),
            REAL_UNREAL_ID,
            "0.2.1",
            "1".repeat(64).as_str(),
        );
        let cache = cache_with(&[pins::UNIVERSAL_RUNNER_UNREAL], &[("v0.3.0", 0x30)]);
        let unreal = detection(Engine::Unreal, &[], Some("g.exe"));
        let (gi, use_universal_runner) = update_for("4513480", &unreal, &ledger, &cache)
            .expect("an older Unreal install should be offered the newest Unreal runner");
        assert!(use_universal_runner);
        assert_eq!(gi.source, pins::UNIVERSAL_RUNNER_UNREAL);
        assert_eq!(gi.version, "0.3.0");
        // The same ledger on a game detected as Unity today: Plan would pick a Unity runner.
        let mono = detection(Engine::UnityMono, &[], Some("g.exe"));
        assert!(update_for("4513480", &mono, &ledger, &cache).is_none());
        // And a Unity install on a game detected as Unreal today: a reinstall, not an update.
        let mono_ledger = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.6.0",
            "0".repeat(64).as_str(),
        );
        let both = bepinex_cache(&[("v0.9.0", 0x90)]);
        assert!(update_for("4513480", &unreal, &mono_ledger, &both).is_none());
    }

    const GTA3_RESHADE_ID: &str = "astra.unreal.gta3-definitive-edition";
    const TRILOGY_REPO: &str = "https://github.com/mihailinl/astra-gta-trilogy";
    const LIBERTY_CITY: &str = "Gameface/Binaries/Win64/LibertyCity.exe";

    /// GTA III – The Definitive Edition with astra-reshade's integration installed: while the
    /// trilogy has not shipped GTA III, it is offered astra-reshade's newer release of its own zip,
    /// as before the move; the generic Unreal runner installed there is a reinstall to the game's
    /// own source, never an update of the runner (Plan would install the integration).
    #[test]
    fn gta3s_astra_reshade_integration_is_updated_until_the_trilogy_ships() {
        let gta3 = pins::game_source("1546970").unwrap();
        let old = pins::GTA3_ASTRA_RESHADE;
        let cache = cache_with(
            &[pins::UNIVERSAL_RUNNER_UNREAL, old],
            &[("v0.2.2", 0x22), ("v0.2.3", 0x23)],
        );
        let d = detection(Engine::Unreal, &[], Some(LIBERTY_CITY));
        let own = test_ledger(
            Some(REAL_RESHADE_REPO),
            GTA3_RESHADE_ID,
            "0.2.2",
            &"22".repeat(32),
        );
        // The trilogy's listing never answered: nothing is offered rather than a guess.
        assert!(update_for("1546970", &d, &own, &cache).is_none());
        // Answered, with nothing in it: astra-reshade's own newer release.
        cache.record(gta3.repo, Some(vec![]));
        let (gi, universal) = update_for("1546970", &d, &own, &cache).expect("its own update");
        assert_eq!((gi.source, universal), (old, false));
        assert!(
            gi.url
                .ends_with("/astra-reshade/releases/download/v0.2.3/astra-gi-gta3.zip")
        );
        let runner = test_ledger(
            Some(REAL_RESHADE_REPO),
            REAL_UNREAL_ID,
            "0.2.1",
            &"21".repeat(32),
        );
        assert!(update_for("1546970", &d, &runner, &cache).is_none());
        // The same runner ledger on another Unreal game is offered the runner's own zip.
        let (gi, universal) = update_for("4513480", &d, &runner, &cache).expect("runner update");
        assert_eq!(
            (gi.source, universal),
            (pins::UNIVERSAL_RUNNER_UNREAL, true)
        );
        assert!(gi.url.ends_with("/v0.2.3/astra-gi.zip"));
    }

    /// **An astra-reshade GTA III install is offered the trilogy's GTA III as its Update once the
    /// trilogy ships it**, though its number is lower (0.1.0 after 0.2.3): the offer plans the
    /// trilogy's source, which `PlanInstall` installs over the old integration in the game's one
    /// profile (no refusal: it is no downgrade). From then on the trilogy install is updated within
    /// the trilogy, and never offered astra-reshade back — nor allowed to install it over itself.
    #[test]
    fn an_astra_reshade_gta3_install_is_offered_the_trilogys_once_it_ships() {
        let gta3 = pins::game_source("1546970").unwrap();
        let old = pins::GTA3_ASTRA_RESHADE;
        let d = detection(Engine::Unreal, &[], Some(LIBERTY_CITY));
        let cache = cache_with(&[pins::UNIVERSAL_RUNNER_UNREAL, old], &[("v0.2.3", 0x23)]);
        let installed = test_ledger(
            Some(REAL_RESHADE_REPO),
            GTA3_RESHADE_ID,
            "0.2.3",
            &"23".repeat(32),
        );
        cache.record(gta3.repo, Some(vec![]));
        assert!(
            update_for("1546970", &d, &installed, &cache).is_none(),
            "at astra-reshade's newest, and the trilogy has nothing"
        );
        cache.record(
            gta3.repo,
            Some(vec![releases::test_release(&gta3, "v0.1.0", 0x01)]),
        );
        let (gi, universal) =
            update_for("1546970", &d, &installed, &cache).expect("the move is the update");
        assert_eq!(
            (gi.source, gi.version.as_str(), universal),
            (gta3, "0.1.0", false)
        );
        assert!(gi.url.starts_with(
            "https://github.com/mihailinl/astra-gta-trilogy/releases/download/v0.1.0/"
        ));
        assert_eq!(
            pins::source_for("1546970", universal, universal_flavour(Engine::Unreal)),
            gta3,
            "the offer's use_universal_runner plans the trilogy's source"
        );
        assert!(refuse_older(&gi, Some(&installed)).is_ok());
        // The row names the move: a new integration, not `v0.2.3 → v0.1.0`.
        assert_eq!(
            moves_to(Some(&(gi.clone(), universal)), Some(old)),
            gta3.name
        );
        // The row keeps BOTH repositories listed for this install: the trilogy's first.
        let follows = pins::source_for_ledger(
            "1546970",
            &installed.integration.id,
            installed.integration.repo.as_deref(),
        )
        .map(|(s, _)| s);
        assert_eq!(follows, Some(old));
        assert_eq!(pins::update_sources("1546970", old), vec![gta3, old]);

        // Moved: updated within the trilogy.
        let moved = test_ledger(
            Some(TRILOGY_REPO),
            "astra.gta-trilogy.gta3",
            "0.1.0",
            &"01".repeat(32),
        );
        assert!(
            update_for("1546970", &d, &moved, &cache).is_none(),
            "at its newest"
        );
        cache.record(
            gta3.repo,
            Some(vec![
                releases::test_release(&gta3, "v0.1.0", 0x01),
                releases::test_release(&gta3, "v0.2.0", 0x02),
            ]),
        );
        let (gi, _) = update_for("1546970", &d, &moved, &cache).expect("the trilogy's update");
        assert_eq!((gi.source, gi.version.as_str()), (gta3, "0.2.0"));
        // An update of the installed integration names no new one.
        assert_eq!(moves_to(Some(&(gi.clone(), false)), Some(gta3)), "");
        assert_eq!(moves_to(None, Some(gta3)), "");
        // Every trilogy release withdrawn: astra-reshade is never offered back, nor installed
        // over the trilogy's integration.
        cache.record(gta3.repo, Some(vec![]));
        assert!(update_for("1546970", &d, &moved, &cache).is_none());
        let back = releases::pick(&cache.get(old.repo).unwrap(), &old).unwrap();
        let e = refuse_older(&back, Some(&moved)).unwrap_err();
        assert_eq!(
            (e.code, e.params.get("reason").map(String::as_str)),
            (releases::RELEASE_OLDER, Some("superseded"))
        );
        // An ordinary downgrade of one source carries no such reason.
        let mut older = gi.clone();
        older.version = "0.0.1".into();
        let e = refuse_older(&older, Some(&moved)).unwrap_err();
        assert_eq!(
            (e.code, e.params.get("reason")),
            (releases::RELEASE_OLDER, None)
        );
    }

    /// **The background refresh lists BOTH repositories for an astra-reshade GTA III install**,
    /// the trilogy's included: without it the trilogy's listing is never fetched, `pick_held`
    /// has nothing to walk, and the row is never offered the move. A trilogy install, any other
    /// install and a row with nothing installed list what they always did.
    #[test]
    fn the_refresh_lists_the_successors_repository_for_an_installed_predecessor() {
        let gta3 = pins::game_source("1546970").unwrap();
        let old = pins::GTA3_ASTRA_RESHADE;
        let lethal = pins::game_source("1966720").unwrap();
        let installed = |appid: &str, follows| GameRow {
            appid: appid.into(),
            follows: Some(follows),
            ..row(true, "unreal", "x")
        };
        let repos = |rows: &[GameRow]| refresh_sources(rows).into_keys().collect::<Vec<_>>();
        assert_eq!(
            repos(&[installed("1546970", old)]),
            ["mihailinl/astra-gta-trilogy", "mihailinl/astra-reshade"]
        );
        assert_eq!(
            refresh_sources(&[installed("1546970", old)]).get(gta3.repo),
            Some(&gta3),
            "asked by the trilogy's own source (its repository id)"
        );
        assert_eq!(
            repos(&[installed("1546970", gta3)]),
            ["mihailinl/astra-gta-trilogy"]
        );
        assert_eq!(
            repos(&[installed("1966720", lethal)]),
            ["mihailinl/astra-lethal"]
        );
        let not_installed = GameRow {
            appid: "1546970".into(),
            follows: Some(old),
            ..row(false, "unreal", "x")
        };
        assert!(refresh_sources(&[not_installed]).is_empty());
        // One listing per repository, however many rows follow it.
        assert_eq!(
            repos(&[
                installed("1546970", old),
                installed("4513480", pins::UNIVERSAL_RUNNER_UNREAL)
            ]),
            ["mihailinl/astra-gta-trilogy", "mihailinl/astra-reshade"]
        );
    }

    /// The owner's discovery run: the trilogy's first release is the PRERELEASE
    /// `v0.1.0-discovery`. A test build (`ASTRA_GAMES_PRERELEASES=1`) that installed astra-reshade's
    /// newest prerelease (v0.2.4) is offered it as an Update; a build without the knob offers
    /// nothing and stays on astra-reshade.
    #[test]
    fn the_trilogys_discovery_prerelease_is_offered_over_astra_reshade_only_with_the_knob() {
        let gta3 = pins::game_source("1546970").unwrap();
        let old = pins::GTA3_ASTRA_RESHADE;
        let d = detection(Engine::Unreal, &[], Some(LIBERTY_CITY));
        let cache = ReleaseCache::default();
        let mut reshade = releases::test_release(&old, "v0.2.4", 0x24);
        reshade.prerelease = true;
        cache.record(old.repo, Some(vec![reshade]));
        let mut discovery = releases::test_release(&gta3, "v0.1.0-discovery", 0x0d);
        discovery.prerelease = true;
        cache.record(gta3.repo, Some(vec![discovery]));
        let installed = test_ledger(
            Some(REAL_RESHADE_REPO),
            GTA3_RESHADE_ID,
            "0.2.4",
            &"24".repeat(32),
        );
        let (gi, universal) = update_for_with("1546970", &d, &installed, &cache, true)
            .expect("the knob follows the discovery prerelease");
        assert_eq!(
            (gi.source, gi.version.as_str(), universal),
            (gta3, "0.1.0-discovery", false)
        );
        assert!(update_for_with("1546970", &d, &installed, &cache, false).is_none());
        // This build is the stable side of the knob (`releases`' own test pins that).
        if !releases::FOLLOWS_PRERELEASES {
            assert!(update_for("1546970", &d, &installed, &cache).is_none());
        }
    }

    #[test]
    fn update_for_is_none_when_the_release_is_not_newer_even_if_the_digest_differs() {
        // Same version as the newest release, but a (hypothetically) different digest — a
        // republished asset under the same version tag is not "an update": the version gate
        // refuses it even though the digest alone would have said otherwise.
        let ledger = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "0.9.0",
            "0".repeat(64).as_str(),
        );
        let d = detection(Engine::UnityMono, &[], Some("g.exe"));
        let cache = bepinex_cache(&[("v0.9.0", 0x90)]);
        assert!(update_for("999999", &d, &ledger, &cache).is_none());
        // Nor is an OLDER newest release (one was withdrawn): no offer, no downgrade.
        let ahead = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "1.0.0",
            "0".repeat(64).as_str(),
        );
        assert!(update_for("999999", &d, &ahead, &cache).is_none());
    }

    #[test]
    fn update_for_falls_back_to_the_digest_when_the_installed_version_is_not_semver() {
        // An old ledger (or a hand-edited one) whose version string is not strict semver must
        // not permanently block every future update — fall back to the digest signal, which
        // the caller already knows differs by the time this gate runs.
        let ledger = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_MONO_ID,
            "not-a-version",
            "0".repeat(64).as_str(),
        );
        let d = detection(Engine::UnityMono, &[], Some("g.exe"));
        let cache = bepinex_cache(&[("v0.9.0", 0x90)]);
        assert!(update_for("999999", &d, &ledger, &cache).is_some());
    }

    #[test]
    fn update_for_is_none_when_the_source_cannot_be_decided() {
        let ledger = test_ledger(
            Some("https://github.com/someone/else"),
            "someone.else",
            "1.0.0",
            "0".repeat(64).as_str(),
        );
        let d = detection(Engine::UnityMono, &[], Some("g.exe"));
        let cache = bepinex_cache(&[("v9.0.0", 0x90)]);
        assert!(update_for("1966720", &d, &ledger, &cache).is_none());
    }

    #[test]
    fn update_for_never_offers_the_il2cpp_runner_where_it_is_refused() {
        // The REAL IL2CPP id — the bug this exists to prevent only reproduces when the
        // ledger's own identity, not today's detection, is what picks the IL2CPP flavour.
        let ledger = test_ledger(
            Some(REAL_BEPINEX_REPO),
            REAL_UNIVERSAL_IL2CPP_ID,
            "0.5.0",
            "0".repeat(64).as_str(),
        );
        let cache = bepinex_cache(&[("v0.9.0", 0x90)]);
        let mut d = detection(Engine::UnityIl2cpp, &[], Some("g.exe"));
        d.unity_version = Some("6000.2.9f1".to_string());
        let (gi, _) = update_for("999999", &d, &ledger, &cache)
            .expect("below the refused line the IL2CPP runner's release is offered");
        assert!(gi.url.ends_with("/v0.9.0/astra-gi-il2cpp.zip"));
        d.unity_version = Some("6000.3.1f1".to_string());
        assert!(
            update_for("999999", &d, &ledger, &cache).is_none(),
            "the row must never offer an update to a runner PlanInstall would itself refuse"
        );
    }

    // ── Consent needed (PlanInstall) ────────────────────────────────────

    /// `MANIFEST` (below) parsed once, plus the exact text it came from — the ledger's
    /// `integration` in these tests is this SAME manifest's own parsed `integration`, never a
    /// hand-typed duplicate that could drift from it.
    fn consent_fixture() -> (String, Manifest) {
        let text = MANIFEST.replace("__WINHTTP_SHA__", &"0".repeat(64));
        let manifest = Manifest::parse(&text).unwrap();
        (text, manifest)
    }

    fn ledger_for(
        manifest_text: &str,
        integration: &IntegrationInfo,
        game_files: Vec<LedgerGameFile>,
    ) -> Ledger {
        Ledger {
            schema: astra_game_launcher::LEDGER_SCHEMA,
            state: LedgerState::Installed,
            integration: integration.clone(),
            source: LedgerSource {
                sha256: "0".repeat(64),
            },
            game: LedgerGame {
                dir: PathBuf::from("/tmp/game"),
                exe: "game.exe".to_string(),
            },
            manifest: manifest_text.to_string(),
            files: vec![],
            game_files,
            proton: vec![],
            vac_unchecked_accepted: false,
            loader_baseline: Vec::new(),
        }
    }

    fn plan_for(integration: IntegrationInfo, game_files: Vec<GameFileAction>) -> InstallPlan {
        InstallPlan {
            integration,
            source_sha256: "0".repeat(64),
            profile_dir: PathBuf::from("/tmp/profile"),
            game_dir: PathBuf::from("/tmp/game"),
            profile_files: 0,
            profile_bytes: 0,
            game_files,
            launch_args: vec![],
            proton_overrides: vec![],
            replaces: None,
            warnings: vec![],
            vac_checked: true,
        }
    }

    /// One game file both sides agree on by default — tests perturb one side.
    fn baseline_game_file() -> (LedgerGameFile, GameFileAction) {
        let sha = "0".repeat(64);
        (
            LedgerGameFile {
                rel: "winhttp.dll".to_string(),
                sha256: sha.clone(),
                action: GameAction::Add,
                backup: None,
            },
            GameFileAction {
                rel_path: "winhttp.dll".to_string(),
                action: GameAction::Add,
                sha256: sha,
                existing_sha256: None,
            },
        )
    }

    #[test]
    fn consent_needed_for_is_true_for_a_fresh_install() {
        let (_, manifest) = consent_fixture();
        let plan = plan_for(manifest.integration.clone(), vec![baseline_game_file().1]);
        assert!(consent_needed_for(None, &plan, &manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_the_old_manifest_cannot_be_parsed() {
        let (_, manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(
            "this is not valid toml",
            &manifest.integration,
            vec![old_gf],
        );
        let plan = plan_for(manifest.integration.clone(), vec![new_gf]);
        assert!(consent_needed_for(Some(&ledger), &plan, &manifest));
    }

    #[test]
    fn consent_needed_for_is_false_when_only_the_version_changes() {
        let (text, old_manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &old_manifest.integration, vec![old_gf]);
        let mut new_manifest = old_manifest.clone();
        new_manifest.integration.version = "9.9.9".to_string();
        let plan = plan_for(new_manifest.integration.clone(), vec![new_gf]);
        assert!(!consent_needed_for(Some(&ledger), &plan, &new_manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_the_license_changes() {
        let (text, old_manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &old_manifest.integration, vec![old_gf]);
        let mut new_manifest = old_manifest.clone();
        new_manifest.integration.license = "GPL-3.0".to_string();
        let plan = plan_for(new_manifest.integration.clone(), vec![new_gf]);
        assert!(consent_needed_for(Some(&ledger), &plan, &new_manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_the_authors_change() {
        let (text, old_manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &old_manifest.integration, vec![old_gf]);
        let mut new_manifest = old_manifest.clone();
        new_manifest.integration.authors = vec!["someone-else".to_string()];
        let plan = plan_for(new_manifest.integration.clone(), vec![new_gf]);
        assert!(consent_needed_for(Some(&ledger), &plan, &new_manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_launch_args_change() {
        let (text, old_manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &old_manifest.integration, vec![old_gf]);
        let mut new_manifest = old_manifest.clone();
        new_manifest.launch.args.push("--extra-flag".to_string());
        let plan = plan_for(new_manifest.integration.clone(), vec![new_gf]);
        assert!(consent_needed_for(Some(&ledger), &plan, &new_manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_the_target_changes() {
        let (text, old_manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &old_manifest.integration, vec![old_gf]);
        let mut new_manifest = old_manifest.clone();
        new_manifest.target.platforms = vec!["linux-x64".to_string()];
        let plan = plan_for(new_manifest.integration.clone(), vec![new_gf]);
        assert!(consent_needed_for(Some(&ledger), &plan, &new_manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_config_writes_change() {
        let (text, old_manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &old_manifest.integration, vec![old_gf]);
        let mut new_manifest = old_manifest.clone();
        new_manifest.config_writes.push(ConfigWrite {
            file: "${profile}/x.cfg".to_string(),
            section: "Section".to_string(),
            set: Default::default(),
        });
        let plan = plan_for(new_manifest.integration.clone(), vec![new_gf]);
        assert!(consent_needed_for(Some(&ledger), &plan, &new_manifest));
    }

    #[test]
    fn consent_needed_for_is_false_when_the_plans_game_files_exactly_match_the_ledger() {
        let (text, manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let ledger = ledger_for(&text, &manifest.integration, vec![old_gf]);
        let plan = plan_for(manifest.integration.clone(), vec![new_gf]);
        assert!(!consent_needed_for(Some(&ledger), &plan, &manifest));
    }

    #[test]
    fn a_users_own_file_kept_by_the_last_install_needs_no_new_consent() {
        let (text, manifest) = consent_fixture();
        let theirs = "a".repeat(64);
        let ours = "0".repeat(64);
        // What `install` wrote for a file the user already had: THEIR digest.
        let old = LedgerGameFile {
            rel: "doorstop_config.ini".to_string(),
            sha256: theirs.clone(),
            action: GameAction::KeepExisting,
            backup: None,
        };
        let ledger = ledger_for(&text, &manifest.integration, vec![old]);
        let plan = plan_for(
            manifest.integration.clone(),
            vec![GameFileAction {
                rel_path: "doorstop_config.ini".to_string(),
                action: GameAction::KeepExisting,
                sha256: ours,
                existing_sha256: Some(theirs),
            }],
        );
        assert!(!consent_needed_for(Some(&ledger), &plan, &manifest));
    }

    #[test]
    fn another_managers_files_it_updated_need_no_new_consent() {
        let (text, manifest) = consent_fixture();
        // Thunderstore's Doorstop and its `doorstop_config.ini` when Astra installed, and newer
        // ones it linked since (r2modman relinks both at every modded start).
        let left_alone = |rel: &str, action: GameAction, theirs: &str| LedgerGameFile {
            rel: rel.to_string(),
            sha256: theirs.to_string(),
            action,
            backup: None,
        };
        let ledger = ledger_for(
            &text,
            &manifest.integration,
            vec![
                left_alone("winhttp.dll", GameAction::ShareLoader, &"a".repeat(64)),
                left_alone(
                    "doorstop_config.ini",
                    GameAction::KeepExisting,
                    &"c".repeat(64),
                ),
            ],
        );
        let shared = |theirs: String| GameFileAction {
            rel_path: "winhttp.dll".to_string(),
            action: GameAction::ShareLoader,
            sha256: "0".repeat(64),
            existing_sha256: Some(theirs),
        };
        let kept = GameFileAction {
            rel_path: "doorstop_config.ini".to_string(),
            action: GameAction::KeepExisting,
            sha256: "1".repeat(64),
            existing_sha256: Some("d".repeat(64)),
        };
        let plan = plan_for(
            manifest.integration.clone(),
            vec![shared("b".repeat(64)), kept],
        );
        assert!(!consent_needed_for(Some(&ledger), &plan, &manifest));
        let ledger = ledger_for(
            &text,
            &manifest.integration,
            vec![left_alone(
                "winhttp.dll",
                GameAction::ShareLoader,
                &"a".repeat(64),
            )],
        );
        // Going from sharing to placing our own (their loader is gone) is a new shape: ask.
        let plan = plan_for(
            manifest.integration.clone(),
            vec![GameFileAction {
                action: GameAction::Add,
                existing_sha256: None,
                ..shared(String::new())
            }],
        );
        assert!(consent_needed_for(Some(&ledger), &plan, &manifest));
    }

    /// GTA III's integration moved from `astra-reshade` to `astra-gta-trilogy`: another package
    /// from other people. Its game files, license and authors reading the same must not let it
    /// in without the sheet — and neither may another repository under the same id.
    #[test]
    fn a_move_to_another_integration_always_asks() {
        let (text, manifest) = consent_fixture();
        let (old_gf, new_gf) = baseline_game_file();
        let mut moved = manifest.clone();
        moved.integration.id = format!("{}-moved", manifest.integration.id);
        let ledger = ledger_for(&text, &manifest.integration, vec![old_gf.clone()]);
        let plan = plan_for(moved.integration.clone(), vec![new_gf.clone()]);
        assert!(consent_needed_for(Some(&ledger), &plan, &moved));

        let mut from = manifest.integration.clone();
        from.repo = Some("https://github.com/someone/astra-reshade".into());
        let mut to = manifest.clone();
        to.integration.repo = Some("https://github.com/someone-else/astra-reshade".into());
        let ledger = ledger_for(&text, &from, vec![old_gf.clone()]);
        let plan = plan_for(to.integration.clone(), vec![new_gf.clone()]);
        assert!(consent_needed_for(Some(&ledger), &plan, &to));

        // The same repository spelled another way is no move; neither is a manifest that
        // names none — the id decides there.
        to.integration.repo = Some("https://GitHub.com/someone/astra-reshade.git/".into());
        let plan = plan_for(to.integration.clone(), vec![new_gf.clone()]);
        assert!(!consent_needed_for(Some(&ledger), &plan, &to));
        to.integration.repo = None;
        let plan = plan_for(to.integration.clone(), vec![new_gf]);
        assert!(!consent_needed_for(Some(&ledger), &plan, &to));
    }

    /// Ours, or another manager's proxy that happens to match ours byte for byte: a loader is
    /// already there either way and nothing is written. A flip between the two is no new shape.
    #[test]
    fn a_loader_proxy_flipping_between_reused_and_shared_needs_no_new_consent() {
        let (text, manifest) = consent_fixture();
        let ours = "0".repeat(64);
        let theirs = "a".repeat(64);
        let ledger_with = |action: GameAction, sha: &str| {
            ledger_for(
                &text,
                &manifest.integration,
                vec![LedgerGameFile {
                    rel: "winhttp.dll".to_string(),
                    sha256: sha.to_string(),
                    action,
                    backup: None,
                }],
            )
        };
        let plan_with = |action: GameAction, existing: &str| {
            plan_for(
                manifest.integration.clone(),
                vec![GameFileAction {
                    rel_path: "winhttp.dll".to_string(),
                    action,
                    sha256: ours.clone(),
                    existing_sha256: Some(existing.to_string()),
                }],
            )
        };
        let reused = ledger_with(GameAction::ReuseKnownProxy, &ours);
        let shared = ledger_with(GameAction::ShareLoader, &theirs);
        assert!(!consent_needed_for(
            Some(&reused),
            &plan_with(GameAction::ShareLoader, &theirs),
            &manifest
        ));
        assert!(!consent_needed_for(
            Some(&shared),
            &plan_with(GameAction::ReuseKnownProxy, &ours),
            &manifest
        ));
        // Placing a file over one that was only used is still a new shape.
        assert!(consent_needed_for(
            Some(&reused),
            &plan_with(GameAction::Replace, &theirs),
            &manifest
        ));
    }

    /// The sheet shows the digest of the file that will be IN the folder: theirs for a file the
    /// install leaves alone, ours for one it places.
    #[test]
    fn the_sheet_shows_the_digest_of_the_file_that_stays() {
        let row = |action: GameAction| GameFileAction {
            rel_path: "winhttp.dll".to_string(),
            action,
            sha256: "0".repeat(64),
            existing_sha256: Some("a".repeat(64)),
        };
        for action in [GameAction::KeepExisting, GameAction::ShareLoader] {
            assert_eq!(shown_digest(&row(action)), "a".repeat(64), "{action:?}");
        }
        for action in [
            GameAction::Add,
            GameAction::Replace,
            GameAction::ReuseKnownProxy,
        ] {
            assert_eq!(shown_digest(&row(action)), "0".repeat(64), "{action:?}");
        }
    }

    /// The consent sheet names each game-file action in words (`GamesTab.tsx`'s `actionLabel`);
    /// an action it does not know is shown as its raw code with every other gate green.
    #[test]
    fn the_games_tab_names_every_game_file_action() {
        let every = [
            GameAction::Add,
            GameAction::Replace,
            GameAction::ReuseKnownProxy,
            GameAction::KeepExisting,
            GameAction::ShareLoader,
        ];
        // A new launcher action stops this compiling: add it to `every` and to `actionLabel`.
        for action in every {
            match action {
                GameAction::Add
                | GameAction::Replace
                | GameAction::ReuseKnownProxy
                | GameAction::KeepExisting
                | GameAction::ShareLoader => {}
            }
        }
        let tab = include_str!("../../../astra-ui/src/pages/Workshop/GamesTab.tsx");
        let start = tab
            .find("function actionLabel(")
            .expect("GamesTab.tsx has actionLabel");
        let body = &tab[start..start + tab[start..].find("\n}").expect("actionLabel ends")];
        for action in every {
            assert!(
                body.contains(&format!("case \"{}\":", action.code())),
                "GamesTab.tsx actionLabel has no case for {}",
                action.code()
            );
        }
    }

    #[test]
    fn a_shared_foreign_loader_proxy_needs_no_new_consent_when_its_digest_is_unchanged() {
        // v0.3.0's `GameAction::ShareLoader`: another mod manager's loader proxy already does
        // our job, so we never write it — `existing_sha256` carries THEIR digest, exactly like
        // `KeepExisting` above, and `sha256` is OUR (never-written) digest, which legitimately
        // differs. A re-plan of the same install must not re-ask consent just because that
        // comparison fell back to comparing `sha256` instead.
        let (text, manifest) = consent_fixture();
        let theirs = "a".repeat(64);
        let ours = "0".repeat(64);
        let old = LedgerGameFile {
            rel: "winhttp.dll".to_string(),
            sha256: theirs.clone(),
            action: GameAction::ShareLoader,
            backup: None,
        };
        let ledger = ledger_for(&text, &manifest.integration, vec![old]);
        let plan = plan_for(
            manifest.integration.clone(),
            vec![GameFileAction {
                rel_path: "winhttp.dll".to_string(),
                action: GameAction::ShareLoader,
                sha256: ours,
                existing_sha256: Some(theirs),
            }],
        );
        assert!(!consent_needed_for(Some(&ledger), &plan, &manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_the_plans_game_file_action_changed() {
        let (text, manifest) = consent_fixture();
        let (old_gf, _) = baseline_game_file();
        let ledger = ledger_for(&text, &manifest.integration, vec![old_gf]);
        // Same digest, but the PLAN now says `Replace` — a file our previous install marked
        // `Add` would now back up and overwrite something. The manifest carries no ACTION at
        // all, so only the plan can surface this.
        let plan = plan_for(
            manifest.integration.clone(),
            vec![GameFileAction {
                rel_path: "winhttp.dll".to_string(),
                action: GameAction::Replace,
                sha256: "0".repeat(64),
                existing_sha256: Some("f".repeat(64)),
            }],
        );
        assert!(consent_needed_for(Some(&ledger), &plan, &manifest));
    }

    #[test]
    fn consent_needed_for_is_true_when_doorstop_config_content_changes_with_no_pinned_sha256() {
        // The real-world bug this exists to catch: `doorstop_config.ini`'s own
        // `[[game_files]]` rule NEVER pins a `sha256` (every real GI manifest omits it — see
        // `pins.rs`'s own citations), so only the PLAN's real, computed-from-the-actual-bytes
        // digest sees its content change. A manifest-text comparison (the first cut of this
        // feature) cannot see this at all (review of bc7c16a1, S2).
        let winhttp = b"winhttp bytes";
        let manifest_tmpl = MANIFEST.replace("__WINHTTP_SHA__", &sha256_hex(winhttp));
        let manifest_tmpl = manifest_tmpl.replacen(
            "[launch]",
            "[[game_files]]\nfrom = \"doorstop_config.ini\"\nto = \"${game}/doorstop_config.ini\"\n\n[launch]",
            1,
        );

        let tmp = tempdir();
        std::fs::write(tmp.join("game.exe"), b"exe").unwrap();
        let target = GameTarget {
            dir: tmp.clone(),
            exe: PathBuf::from("game.exe"),
            steam_appid: None,
            platform: Platform::Windows,
            steam_appinfo: None,
        };
        let profile = tempdir();

        let zip_v1 = build_test_zip(
            &manifest_tmpl,
            &[
                ("core.dll", b"core bytes"),
                ("winhttp.dll", winhttp),
                ("doorstop_config.ini", b"enabled = false\n"),
            ],
        );
        let digest_v1 = sha256_hex(&zip_v1);
        let pkg_v1 = GiPackage::open(&zip_v1, &digest_v1).expect("v1 package should open");
        let plan_v1 = plan_install(&pkg_v1, &target, &profile).expect("v1 should plan");
        let ledger = install(
            &pkg_v1,
            &plan_v1,
            &target,
            TEST_ACCEPT,
            &astra_game_launcher::Ctx::silent(),
        )
        .expect("v1 should install");

        // v2 ships the SAME winhttp.dll but doorstop_config.ini's CONTENT differs.
        let zip_v2 = build_test_zip(
            &manifest_tmpl,
            &[
                ("core.dll", b"core bytes"),
                ("winhttp.dll", winhttp),
                ("doorstop_config.ini", b"enabled = true\n"),
            ],
        );
        let digest_v2 = sha256_hex(&zip_v2);
        let pkg_v2 = GiPackage::open(&zip_v2, &digest_v2).expect("v2 package should open");
        let mut plan_v2 = plan_install(&pkg_v2, &target, &profile).expect("v2 should plan");
        // This test game has no Steam appid, so VAC was not read; as if it had been, here.
        plan_v2.vac_checked = true;

        assert!(
            consent_needed_for(Some(&ledger), &plan_v2, pkg_v2.manifest()),
            "a changed doorstop_config.ini must re-ask for consent even with no pinned sha256"
        );
        // The negative, on the same real install: re-planning the package that is
        // installed asks nothing. Without it an always-true check passes this test.
        let mut plan_again =
            plan_install(&pkg_v1, &target, &profile).expect("v1 should plan again");
        assert!(
            consent_needed_for(Some(&ledger), &plan_again, pkg_v1.manifest()),
            "an unread VAC flag is asked about every time, even for the same package"
        );
        plan_again.vac_checked = true;
        assert!(
            !consent_needed_for(Some(&ledger), &plan_again, pkg_v1.manifest()),
            "the package that is already installed needs no new consent"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&profile);
    }
}
