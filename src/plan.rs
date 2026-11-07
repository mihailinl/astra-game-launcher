// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The install plan: the consent sheet, as data. Computing it changes nothing on disk.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::{anti_cheat_scan, engine_of_exe_with, is_walked_program};
use crate::digest::sha256_hex;
use crate::doorstop;
use crate::engines::{Descriptor, EngineCode, EngineSet, Moderation, Origin};
use crate::error::{LauncherError, Result, codes};
use crate::former::{self, FormerKind};
use crate::game::{GameTarget, Platform};
use crate::game::{several_apps, steam_appids_of_dir};
use crate::ledger::{self, Ledger, LoaderBaseline, ProxyFolder};
use crate::manifest::{IntegrationInfo, LoaderDecl, Manifest};
use crate::pack_inbox::PackInboxPlan;
use crate::package::{GiPackage, PackFile};
use crate::paths::{
    FileState, absolute, canonical_dir, checked_rel, existing_target, file_state, find_any_case,
    real_case, resolve_lenient,
};
use crate::steam_appinfo::{launch_executable, steam_roots, valve_anti_cheat};

/// What happens to one file beside the game's exe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GameAction {
    /// Nothing is there; ours is added. Uninstall removes it.
    Add,
    /// A game file is there; it is backed up into the profile and ours takes its place.
    /// Uninstall restores it, but only while ours is still there unchanged.
    Replace,
    /// The very file we ship is already there. It is never rewritten, and never removed.
    ReuseKnownProxy,
    /// A config file we did not write (`doorstop_config.ini`, `ReShade.ini`) is already there.
    /// It is left as it is, and uninstall neither removes nor "restores" it. Play also records
    /// such a config we added as this once it changed: a mod manager's `doorstop_config.ini`
    /// copied over ours, a `ReShade.ini` ReShade saved the player's settings into.
    KeepExisting,
    /// Another mod manager's loader proxy (r2modman's, Thunderstore Mod Manager's, a hand-made
    /// BepInEx install's Doorstop) is already there, and it honours every option our launch
    /// passes ([`crate::doorstop`]): the game runs through it. It is never written, backed up or
    /// removed, and its digest is not pinned, since the manager that owns it may update it. Play
    /// also records a proxy we added as this once a mod manager has copied one over ours that
    /// still does the job.
    ShareLoader,
}

impl GameAction {
    pub fn code(self) -> &'static str {
        match self {
            GameAction::Add => "add",
            GameAction::Replace => "replace",
            GameAction::ReuseKnownProxy => "reuse-known-proxy",
            GameAction::KeepExisting => "keep-existing",
            GameAction::ShareLoader => "share-loader",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFileAction {
    /// Below the exe's folder, `/`-separated.
    pub rel_path: String,
    pub action: GameAction,
    /// The digest of the file we ship.
    pub sha256: String,
    /// The digest of the file already there (`replace`, `keep-existing`, `share-loader`; `add`
    /// over a file an older Astra left there, which is neither backed up nor ever put back).
    pub existing_sha256: Option<String>,
}

/// A file (or the `astra-reshade/` folder) an older Astra left beside the game's program that no
/// ledger names (`crate::former`): the install removes it — without a backup, since it is Astra's
/// own and never the player's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormerFile {
    /// Below the exe's folder.
    pub rel_path: String,
    /// The digest it has now (a folder's: of everything below it); the install hashes it again
    /// right before removing it, and refuses with `plan-stale` when it changed since.
    pub sha256: String,
    pub kind: FormerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub integration: IntegrationInfo,
    /// The zip's SHA-256.
    pub source_sha256: String,
    pub profile_dir: PathBuf,
    /// `${game}`: the folder holding the exe, where the game files go.
    pub game_dir: PathBuf,
    pub profile_files: u32,
    pub profile_bytes: u64,
    pub game_files: Vec<GameFileAction>,
    /// The manifest's, with placeholders still visible. (A launch adds Doorstop options of the
    /// launcher's own, `doorstop::session_options`; they are not the integration's choice.)
    pub launch_args: Vec<String>,
    /// Written into the Proton prefix at launch (Proton only), every one: (dll, value), in the
    /// manifest's key order.
    #[serde(default)]
    pub proton_overrides: Vec<(String, String)>,
    /// The integration installed in this profile now; install replaces it.
    pub replaces: Option<IntegrationInfo>,
    /// Files an older Astra left beside the game's program (Unreal runner 0.2.x: UE4SS's v0.2.1
    /// `dwmapi.dll`, its ReShade with `astra.addon64` beside it, the add-on) that no ledger names
    /// any more: the install removes them. One the package itself places is not here: its game
    /// file is an `add` that takes its place (`existing_sha256` = the former file's digest), so an
    /// uninstall puts nothing back. Absent in an older serialised plan: none.
    #[serde(default)]
    pub former_files: Vec<FormerFile>,
    /// Codes: "experimental", "not-reviewed", "replaces-integration", "no-license",
    /// "vac-unchecked" (the same fact as `vac_checked == false`, for a sheet that lists codes),
    /// "former-astra-files" (`former_files` is not empty), "foreign-loader-in-backup" (a
    /// `replace` of a loader's file the current install already made, kept: the player's loader
    /// is in Astra's backup, and comes back at uninstall while ours is still in its place).
    pub warnings: Vec<String>,
    /// Steam's Valve Anti-Cheat flag was READ for this app and is clear. `false`: no TRUSTED Steam
    /// appid (the caller's, or the one the folder's appmanifest proves; never the integration
    /// manifest's), or no `appinfo.vdf` answered for it — not
    /// refused on that ground alone, but [`crate::install`] then needs
    /// [`Accept::vac_unchecked`] from the person, never a plain "yes". Absent in an older
    /// serialised plan: `false`.
    #[serde(default)]
    pub vac_checked: bool,
    /// Who vouches for the integration, as the caller of [`plan_install_in`] said. Absent from
    /// the JSON when first-party, so a first-party plan of a built-in engine's manifest
    /// serialises exactly as launcher 0.4.1's.
    #[serde(default, skip_serializing_if = "Provenance::is_first_party")]
    pub provenance: Provenance,
    /// The engine descriptor that named the game's engine (`[target] engine` a descriptor's
    /// code): install plans again with it alone, so no plan is installed against another one.
    /// `None` (and absent from the JSON) for a built-in engine, and for none asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_descriptor: Option<PinnedDescriptor>,
    /// The packs the integration hands the desktop companion (`${astra_pack_inbox}`), and the
    /// companion's user root the caller named for them ([`plan_install_with_user_pack_root`]):
    /// install plans again with that root. Absent (and absent from the JSON) when the package
    /// places none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_inbox: Option<PackInboxPlan>,
}

/// Who vouches for an integration — what its plan may do (plan B §3, "Overwrite a player's
/// file"). The launcher cannot tell: the caller vouches for it, and the launcher takes its word.
/// **First-party** is an integration whose `[integration] id` AND repository are both in
/// Astra's compiled seed of its own integrations; every other one — from the catalogue,
/// moderated or not, or a developer's local folder — is **community**. Recorded in the plan and
/// the ledger. A community plan has no `replace` arm (`foreign-file`), keeps to the community
/// limits on `[[game_files]]` and `[launch]`, and plans only on a program the built-in walk lists
/// (`community-limit{rule=exe-not-a-program}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    /// Astra's own: id and repository in the compiled seed.
    #[default]
    FirstParty,
    /// Anyone else's.
    Community,
}

impl Provenance {
    pub fn is_first_party(&self) -> bool {
        *self == Provenance::FirstParty
    }
}

/// The engine descriptor a plan was made with, pinned: what named the game's engine, and where
/// the caller said it came from. [`crate::install`] parses it again, checks its digest, and
/// plans again with it alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedDescriptor {
    /// `sha256(JCS(descriptor))`, lower-case hex ([`Descriptor::digest`]).
    pub sha256: String,
    /// The canonical JSON that digest is of ([`Descriptor::canonical_json`]).
    pub canonical_json: String,
    /// Its [`Origin`], as the caller vouched for it.
    pub moderation: Moderation,
    pub publisher: String,
    pub first_party: bool,
}

impl PinnedDescriptor {
    fn of(d: &Descriptor, origin: &Origin) -> PinnedDescriptor {
        PinnedDescriptor {
            sha256: d.digest().to_owned(),
            canonical_json: d.canonical_json().to_owned(),
            moderation: origin.moderation,
            publisher: origin.publisher.clone(),
            first_party: origin.first_party,
        }
    }

    /// An [`EngineSet`] of this descriptor alone, parsed again from its canonical JSON:
    /// `plan-stale` (`detail` = `engine-descriptor`) when it does not parse, has another digest
    /// than the one pinned, or is not one Pass B admits.
    pub(crate) fn engine_set(&self) -> Result<EngineSet> {
        let stale = || LauncherError::new(codes::PLAN_STALE).with("detail", "engine-descriptor");
        let d = Descriptor::from_json(self.canonical_json.as_bytes()).map_err(|_| stale())?;
        if d.digest() != self.sha256 {
            return Err(stale());
        }
        let origin = Origin {
            moderation: self.moderation,
            publisher: self.publisher.clone(),
            first_party: self.first_party,
        };
        let set = EngineSet::build([(origin, Ok(d))]);
        if set.len() != 1 {
            return Err(stale());
        }
        Ok(set)
    }
}

/// What [`plan_install_in`] is given beyond the package, the game and the profile.
#[derive(Clone, Copy)]
pub(crate) struct Planning<'a> {
    pub(crate) engines: &'a EngineSet,
    pub(crate) provenance: Provenance,
    pub(crate) engine_pick: Option<&'a EngineCode>,
    /// The desktop companion's user root (`ASTRA_USER_PACK`), for `${astra_pack_inbox}`.
    pub(crate) user_pack_root: Option<&'a Path>,
}

/// Names a Doorstop proxy of ours goes by that Play, uninstall and the loader baselines treat as
/// one another mod manager may share: relinked by it, kept for its BepInEx
/// ([`kept_for_their_loader`]), measured at install ([`loader_baselines`]). Planning reads the
/// wider [`HIJACK_NAMES`]; this set is what every launcher since 0.1.0 has had, and it does not
/// change.
const LOADER_PROXIES: &[&str] = &["winhttp.dll", "version.dll", "dxgi.dll", "d3d11.dll"];

/// Whether the file at `rel` (`/`-separated) goes by a [`LOADER_PROXIES`] name, in any letter
/// case.
pub(crate) fn is_loader_proxy(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    LOADER_PROXIES.iter().any(|p| p.eq_ignore_ascii_case(name))
}

/// The DLL names a mod loader hijack-loads by: Windows (and Wine, with a `native` override) looks
/// for each in the game program's own folder before the system's, so a file by one of them there
/// runs in every start of the game, through Astra or not. Planning never replaces such a file it
/// did not place, for any game and any integration: it is shared where a Doorstop of ours can run
/// through it ([`GameAction::ShareLoader`], [`doorstop::stands_in`]), and `foreign-loader`
/// otherwise. A manifest's `[integration.loader] proxies` add to it for that install only.
/// Matched by the last path component, in any letter case.
///
/// The one list; each name's source:
/// - merged plan §2.5 / plan A §4.5 "Loaders (M1)": `winhttp`, `version`, `dwmapi`, `dxgi`,
///   `d3d11`, `dinput8`, and `xinput*` — `xinput1_1` … `xinput1_4`, `xinput9_1_0` and
///   `xinputuap`, the XInput names the ASI loader below takes;
/// - [`doorstop::PROXY_NAMES`], the names a Doorstop build goes by: the above, and `winmm`;
/// - the system-DLL names Ultimate ASI Loader may be renamed to (ThirteenAG/Ultimate-ASI-Loader
///   `readme.md`, read 2026-10-31, its x86 and x64 lists): `dinput`, `dsound`, `d3d8`, `d3d9`,
///   `d3d10`, `d3d12`, `ddraw`, `msacm32`, `msvfw32`, `wininet`, `xlive`. Its game-library names
///   (`binkw32`, `binkw64`, `bink2w32`, `bink2w64`, `vorbisFile`) are a game's own files, not
///   hijacks of a system DLL: they are not here;
/// - ReShade's OpenGL proxy, `opengl32` (its setup's choice for an OpenGL game).
///
/// A unit test below holds [`LOADER_PROXIES`] and [`doorstop::PROXY_NAMES`] inside it, and
/// [`KEEP_IF_PRESENT`] outside.
pub const HIJACK_NAMES: &[&str] = &[
    // Plan A §4.5.
    "winhttp.dll",
    "version.dll",
    "dwmapi.dll",
    "dxgi.dll",
    "d3d11.dll",
    "dinput8.dll",
    "xinput1_1.dll",
    "xinput1_2.dll",
    "xinput1_3.dll",
    "xinput1_4.dll",
    "xinput9_1_0.dll",
    "xinputuap.dll",
    // doorstop::PROXY_NAMES (the rest of it is above).
    "winmm.dll",
    // Ultimate ASI Loader's system-DLL names.
    "dinput.dll",
    "dsound.dll",
    "d3d8.dll",
    "d3d9.dll",
    "d3d10.dll",
    "d3d12.dll",
    "ddraw.dll",
    "msacm32.dll",
    "msvfw32.dll",
    "wininet.dll",
    "xlive.dll",
    // ReShade for OpenGL.
    "opengl32.dll",
];

/// Whether the file at `rel` (`/`-separated) goes by a [`HIJACK_NAMES`] name, in any letter case.
pub fn is_hijack_name(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    HIJACK_NAMES.iter().any(|p| p.eq_ignore_ascii_case(name))
}

/// What planning reads a game file's name against, for one install: the compiled lists and the
/// manifest's own `[integration.loader]` (merged plan §2.5: the manifest decides, the catalogue's
/// `loaders[]` only labels). The two are disjoint: [`crate::Manifest::parse`] refuses a manifest
/// that keeps a name it loads, or loads a name the compiled list keeps
/// (`loader-policy-conflict{reason=proxy}`), and no compiled keep name is a hijack name.
pub(crate) struct LoaderPolicy<'m> {
    decl: Option<&'m LoaderDecl>,
}

impl<'m> LoaderPolicy<'m> {
    pub(crate) fn of(m: &'m Manifest) -> LoaderPolicy<'m> {
        LoaderPolicy { decl: m.loader() }
    }

    fn leaf(rel: &str) -> &str {
        rel.rsplit('/').next().unwrap_or(rel)
    }

    /// The name is one the manifest declares in `proxies`.
    pub(crate) fn declared(&self, rel: &str) -> bool {
        let name = Self::leaf(rel);
        self.decl
            .is_some_and(|l| l.proxies.iter().any(|p| p.eq_ignore_ascii_case(name)))
    }

    /// A hijack-loaded name for this install: [`HIJACK_NAMES`] or the manifest's `proxies`.
    pub(crate) fn hijack(&self, rel: &str) -> bool {
        is_hijack_name(rel) || self.declared(rel)
    }

    /// A name left alone when present and not ours, for this install: [`KEEP_IF_PRESENT`] or the
    /// manifest's `keep_if_present`.
    pub(crate) fn keep(&self, rel: &str) -> bool {
        let name = Self::leaf(rel);
        keep_if_present(rel)
            || self.decl.is_some_and(|l| {
                l.keep_if_present
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(name))
            })
    }
}

/// Folders another mod loader owns beside a game, by name, in any letter case: a community
/// integration places no game file below one (plan B §3, "Plant code in another loader's
/// folder").
const OTHER_LOADERS_FOLDERS: &[&str] = &["BepInEx", "MelonLoader", "ue4ss", "Mods", "~mods"];

/// The rule a `community-limit` names (its `rule` param).
mod limit {
    /// A game file goes neither directly beside the program nor below a folder named after the
    /// integration's id, or below a folder of another loader's.
    pub(super) const GAME_FILE_FOLDER: &str = "game-file-folder";
    /// The planned program is not one the built-in walk lists as a program.
    pub(super) const EXE_NOT_A_PROGRAM: &str = "exe-not-a-program";
    /// `[launch] env`: refused.
    pub(super) const LAUNCH_ENV: &str = "launch-env";
    /// A Proton DLL override for a DLL the integration does not place directly beside the
    /// program, or with a value other than `native,builtin` / `n,b`.
    pub(super) const LAUNCH_OVERRIDE: &str = "launch-override";
    /// A launch argument that is (or holds) a path not under `${profile}/` or `${game}/`.
    pub(super) const LAUNCH_ARG_PATH: &str = "launch-arg-path";
}

fn community_limit(rule: &str) -> LauncherError {
    LauncherError::new(codes::COMMUNITY_LIMIT).with("rule", rule)
}

/// The override values a community integration may ask Proton for.
const COMMUNITY_OVERRIDE_VALUES: &[&str] = &["native,builtin", "n,b"];

/// Whether a launch argument of a community integration stays under the profile or the game:
/// one holding `/`, `\` or a placeholder starts with `${profile}/` or `${game}/`, and what
/// follows is a relative path — no `..`, no other placeholder. Any other argument is a word, and
/// passes, unless it — or what follows any `=` in it — is a path by itself: `.`, `..` or any
/// other name of dots (the game's working folder, or the one above it), or a drive-relative
/// `C:x`, `C:..` (relative to that drive's current folder, on Windows and under Wine alike).
fn arg_stays_inside(arg: &str) -> bool {
    if !arg.contains(['/', '\\']) && !arg.contains("${") {
        // Windows reads a name of dots and spaces alone (`...`) as the folder it is in.
        return arg.split('=').all(|w| {
            (w.is_empty() || !w.trim_end_matches(['.', ' ']).is_empty())
                && crate::paths::windows_text_verdict(w) == Ok(crate::paths::WindowsText::Relative)
        });
    }
    let Some(rest) = ["${profile}/", "${game}/"]
        .iter()
        .find_map(|p| arg.strip_prefix(p))
    else {
        return false;
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    rest.is_empty() || (!rest.contains("${") && crate::paths::relative_components(rest).is_some())
}

/// What a community integration's manifest may not do, said by the manifest alone (plan B §3; no
/// game folder needed), every one found, in this order:
///
/// 1. per `[[game_files]]` entry: `loader-undeclared{file}` for a [`HIJACK_NAMES`] name its
///    `[integration.loader] proxies` do not declare (merged plan §2.5, K18), and
///    `community-limit{rule=game-file-folder, file}` unless it goes directly beside the program
///    (`${game}/<name>`) or below a folder named after the integration's id
///    (`${game}/<id>/…`), with no folder of another loader's ([`OTHER_LOADERS_FOLDERS`]) on its
///    way;
/// 2. `community-limit{rule=launch-env}` for any `[launch] env`;
/// 3. `community-limit{rule=launch-override, field}` for a Proton override of a DLL its own
///    `[[game_files]]` do not place directly beside the program (`${game}/<name>`: an override
///    applies to the whole prefix by the module's name, and Windows and Wine look in the
///    program's own folder first, so one placed deeper is never what it loads — a dormant loader
///    of the player's beside the program is), or with a value other than `native,builtin` /
///    `n,b`;
/// 4. `community-limit{rule=launch-arg-path, field}` for an argument holding a path that is not
///    under `${profile}/` or `${game}/` ([`arg_stays_inside`]).
///
/// [`plan_install_in`] refuses a community plan with the first; [`crate::Manifest::lint`]
/// reports them all (`gi-probe check`). Astra's own integrations are not asked.
pub(crate) fn community_limits(m: &Manifest) -> Vec<LauncherError> {
    let policy = LoaderPolicy::of(m);
    let mut out = Vec::new();
    let dests: Vec<Vec<String>> = (0..m.game_files.len())
        .map(|n| m.game_file_dest(n).rel)
        .collect();
    for rel in &dests {
        let file = rel.join("/");
        if is_hijack_name(&file) && !policy.declared(&file) {
            out.push(LauncherError::new(codes::LOADER_UNDECLARED).with("file", file.clone()));
        }
        let beside = rel.len() == 1 || rel[0].eq_ignore_ascii_case(&m.integration.id);
        let theirs = rel.iter().any(|c| {
            OTHER_LOADERS_FOLDERS
                .iter()
                .any(|f| f.eq_ignore_ascii_case(c))
        });
        if !beside || theirs {
            out.push(community_limit(limit::GAME_FILE_FOLDER).with("file", file));
        }
    }
    if let Some(k) = m.launch.env.keys().next() {
        out.push(community_limit(limit::LAUNCH_ENV).with("field", format!("launch.env.{k}")));
    }
    for (dll, value) in &m.launch.proton_dll_overrides {
        // Wine names a DLL by its stem (`dwmapi`) or its file name (`dwmapi.dll`).
        let dll_lower = dll.to_ascii_lowercase();
        let placed = dests.iter().any(|rel| {
            let [name] = rel.as_slice() else {
                return false;
            };
            let name = name.to_ascii_lowercase();
            name == dll_lower || name == format!("{dll_lower}.dll")
        });
        if !placed || !COMMUNITY_OVERRIDE_VALUES.contains(&value.as_str()) {
            out.push(
                community_limit(limit::LAUNCH_OVERRIDE)
                    .with("field", format!("launch.proton_dll_overrides.{dll}")),
            );
        }
    }
    for (i, a) in m.launch.args.iter().enumerate() {
        if !arg_stays_inside(a) {
            out.push(
                community_limit(limit::LAUNCH_ARG_PATH).with("field", format!("launch.args[{i}]")),
            );
        }
    }
    out
}

/// Files that, when present and not ours, are left alone. Matched case-insensitively: Wine and
/// Windows see `ReShade.ini` and `reshade.ini` as one file.
///
/// `ReShade.ini` is here because ReShade REWRITES it while the game runs (the overlay saves the
/// user's settings into it). A user's own file must survive install and uninstall, and ours, once
/// ReShade has rewritten it, is the user's settings: an update must not back it up and replace it,
/// and an uninstall must not "restore" an older copy over it. Play applies the same rule to one we
/// added that changed since ([`crate::launch`]): it is the user's, or a mod manager's, now.
const KEEP_IF_PRESENT: &[&str] = &["doorstop_config.ini", "ReShade.ini"];

/// Whether the file at `rel` (`/`-separated) goes by a [`KEEP_IF_PRESENT`] name, in any letter
/// case.
pub(crate) fn keep_if_present(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    KEEP_IF_PRESENT.iter().any(|k| k.eq_ignore_ascii_case(name))
}

/// What the person accepted beyond the plan itself, passed to [`crate::install`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accept {
    /// The plan could not read Steam's VAC flag (`InstallPlan::vac_checked` is false) and the
    /// person installs anyway, having been told. Install refuses such a plan without it.
    pub vac_unchecked: bool,
}

/// The `kind` of an `anti-cheat` refusal for a game Steam flags as VAC-protected.
pub const VALVE_ANTI_CHEAT: &str = "valve-anti-cheat";

/// The Steam appid this game is KNOWN to be: the caller's, or the one the game folder proves
/// through its library's `appmanifest_*.acf` ([`crate::steam_appids_of_dir`]). The caller's must
/// be among the folder's apps when the folder names any (several share GoldSrc's folder);
/// otherwise, or with several and no caller, `game-mismatch`. An integration manifest's
/// `steam_appid` is never a source here.
pub(crate) fn trusted_appid(game: &GameTarget) -> Result<Option<u32>> {
    let folder = steam_appids_of_dir(&game.dir);
    match (game.steam_appid, folder.as_slice()) {
        (caller, []) => Ok(caller),
        // A folder several apps share (GoldSrc's `common/Half-Life`): the caller's, when it is
        // one of them.
        (Some(caller), apps) if apps.contains(&caller) => Ok(Some(caller)),
        (None, [one]) => Ok(Some(*one)),
        (caller, apps) => Err(several_apps(apps, caller)),
    }
}

/// Every `appinfo.vdf` on this machine worth asking: the caller's, then every Steam install's
/// (a wrong path from the caller never switches those off), each file once.
fn appinfo_files(game: &GameTarget) -> Vec<std::path::PathBuf> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    let all = game.steam_appinfo.iter().cloned().chain(
        steam_roots()
            .into_iter()
            .map(|r| r.join("appcache").join("appinfo.vdf")),
    );
    for p in all {
        let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(p);
        }
    }
    out
}

/// The VAC gate, shared by plan and launch. Every appinfo that answers is read, and ANY one
/// flagging VAC refuses — for the trusted appid ([`trusted_appid`]) and for the manifest's own
/// `steam_appid` alike (the manifest may cause a refusal, never a pass). Returns whether the flag
/// was READ for the trusted appid (`false`: no trusted appid, or no appinfo answered for it).
pub(crate) fn vac_check(
    game: &GameTarget,
    trusted: Option<u32>,
    manifest: Option<u32>,
) -> Result<bool> {
    vac_verdict(&appinfo_files(game), trusted, manifest)
}

/// [`vac_check`] over an explicit list of `appinfo.vdf` files: refuses when ANY file flags the
/// trusted appid or the manifest's appid as VAC (never the first answer alone), and returns
/// whether some file answered for the trusted appid.
pub fn vac_verdict(
    files: &[std::path::PathBuf],
    trusted: Option<u32>,
    manifest: Option<u32>,
) -> Result<bool> {
    let answers = |id: u32| -> Vec<bool> {
        files
            .iter()
            .filter_map(|p| valve_anti_cheat(p, id))
            .collect()
    };
    let for_trusted = trusted.map(answers).unwrap_or_default();
    let for_manifest = manifest
        .filter(|m| Some(*m) != trusted)
        .map(answers)
        .unwrap_or_default();
    if for_trusted.iter().chain(&for_manifest).any(|vac| *vac) {
        return Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", VALVE_ANTI_CHEAT));
    }
    Ok(!for_trusted.is_empty())
}

/// **The anti-cheat gate, shared by plan and Play** (launcher 0.5.2): `anti-cheat{kind}` with
/// the first code [`anti_cheat_scan`] finds in the game's folder now — a folder signature
/// ([`crate::ANTI_CHEAT_SIGNATURES`]), `listed-anti-cheat` (a marker `engines` carries,
/// [`EngineSet::with_anti_cheat_markers`]) or `kernel-anti-cheat` (a kernel driver whose image
/// is in the folder, [`crate::anti_cheat_services`]). `game_dir` is the folder resolved
/// ([`canonical_dir`]); the services are compared with the folder as the caller spelled it too.
/// Only the markers and the services read `engines`, never its descriptors. Play asks it at every
/// launch, so a game that gained an anti-cheat after it was installed (an update added one, or a
/// kernel driver) is never started with the integration; VAC is [`vac_check`], beside it.
pub(crate) fn refuse_anti_cheat(
    game: &GameTarget,
    game_dir: &Path,
    engines: &EngineSet,
) -> Result<()> {
    match anti_cheat_scan(game_dir, &absolute(&game.dir)?, engines)?
        .into_iter()
        .next()
    {
        Some(kind) => Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", kind)),
        None => Ok(()),
    }
}

/// What the game file at `path` will be once the current install (if any) is uninstalled:
/// uninstall removes our `add`s and restores our `replace`s, while they still have our digest. An
/// added Doorstop someone else's loader now runs through is no exception here: replacing the
/// integration removes it and places the new one's (`uninstall_ledger`'s `readd`), so it stays
/// ours (`add`) for the uninstall that may one day find nobody else needs it.
///
/// Also whether the file that state names is one a ledger holds in its backup: the current
/// ledger's `replace` with ours still in place (the state is the backup's), or — in install's
/// look once `replaced` is undone — the file the uninstall just put back from `replaced`'s
/// backup. Step 10a keeps such a `replace` of a foreign loader for Astra's own integrations.
fn state_before(
    path: &Path,
    rel: &str,
    existing: Option<&Ledger>,
    replaced: Option<&Ledger>,
) -> Result<(FileState, bool)> {
    let actual = file_state(path)?;
    let recorded = |l: &Ledger| {
        l.game_files
            .iter()
            .find(|g| g.rel.eq_ignore_ascii_case(rel))
            .cloned()
    };
    if let (Some(l), FileState::File { sha256 }) = (existing, &actual)
        && let Some(e) = recorded(l)
        && *sha256 == e.sha256
    {
        match e.action {
            GameAction::Add => return Ok((FileState::Absent, false)),
            GameAction::Replace => {
                if let Some(b) = &e.backup {
                    // A backup an uninstall never puts back: a file an older Astra left there
                    // (`crate::former`, its rules exactly). What the uninstall leaves is nothing.
                    let name = rel.rsplit('/').next().unwrap_or(rel);
                    if path
                        .parent()
                        .is_some_and(|d| former::former_kind(d, name, &b.sha256).is_some())
                    {
                        return Ok((FileState::Absent, false));
                    }
                    return Ok((
                        FileState::File {
                            sha256: b.sha256.clone(),
                        },
                        true,
                    ));
                }
            }
            GameAction::ReuseKnownProxy | GameAction::KeepExisting | GameAction::ShareLoader => {}
        }
    }
    let put_back = match (replaced, &actual) {
        (Some(old), FileState::File { sha256 }) => recorded(old).is_some_and(|e| {
            e.action == GameAction::Replace && e.backup.is_some_and(|b| b.sha256 == *sha256)
        }),
        _ => false,
    };
    Ok((actual, put_back))
}

/// The digest of the foreign loader proxy at `path` when our launch can run through it instead
/// of ours (`f`), taken from the very bytes judged; `None` when it cannot.
fn shared_loader(
    pkg: &GiPackage,
    f: &PackFile,
    path: &Path,
    exe_path: &Path,
) -> Result<Option<String>> {
    let Some(theirs) = doorstop::read_proxy(path) else {
        return Ok(None);
    };
    let ours = pkg.read(&mut pkg.archive()?, f)?;
    let machine = doorstop::pe_machine_of_file(exe_path);
    let args = &pkg.manifest().launch.args;
    Ok(doorstop::stands_in(&ours, &theirs, machine, args).then(|| sha256_hex(&theirs)))
}

/// The folder of a game file (`/`-separated, below the exe's folder) and the path of that folder.
fn folder_of(game_root: &Path, rel: &str) -> Result<(Vec<String>, PathBuf)> {
    let mut comps = checked_rel(rel)?;
    comps.pop();
    let path = comps.iter().fold(game_root.to_path_buf(), |p, c| p.join(c));
    Ok((comps, path))
}

/// The `doorstop_config.ini` in `dir` (below `game_root`): its spelling on disk and its state.
fn config_in(game_root: &Path, dir: &[String]) -> Result<(String, FileState)> {
    let mut ini = dir.to_vec();
    ini.push(doorstop::CONFIG_NAME.to_owned());
    let ini = real_case(game_root, &ini)?;
    let state = match existing_target(game_root, &ini)? {
        Some(p) => file_state(&p)?,
        None => FileState::Absent,
    };
    Ok((ini.join("/"), state))
}

/// Whether `<dir>BepInEx/core/<name>` (or something below it) is a game file of `ledger`.
fn placed_by(ledger: &Ledger, dir: &[String], name: &str) -> bool {
    let base: String = dir.iter().map(|c| format!("{c}/")).collect();
    let r = format!("{base}BepInEx/core/{name}").to_ascii_lowercase();
    ledger.game_files.iter().any(|g| {
        let g = g.rel.to_ascii_lowercase();
        g == r || g.starts_with(&format!("{r}/"))
    })
}

/// The names in the `BepInEx/core` in `dir_path`, if there is one.
fn bepinex_core(dir_path: &Path) -> Vec<String> {
    find_any_case(dir_path, &["BepInEx", "core"])
        .and_then(|core| std::fs::read_dir(core).ok())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// What sits beside each Doorstop proxy `planned` adds, before this integration first added it:
/// the [`LoaderBaseline`] the ledger keeps. A proxy the replaced integration (`old`) had added
/// already keeps that install's baseline, carried unchanged, so that what arrived since stays
/// someone else's (a ledger from before 0.3.1 has none to carry). A proxy added now for the first
/// time is measured now, before `old` is undone: its config as the plan leaves it, and the
/// `BepInEx/core` names that are not `old`'s files.
pub(crate) fn loader_baselines(
    game_root: &Path,
    planned: &[GameFileAction],
    old: Option<&Ledger>,
) -> Result<Vec<LoaderBaseline>> {
    let mut out = Vec::new();
    for a in planned
        .iter()
        .filter(|a| a.action == GameAction::Add && is_loader_proxy(&a.rel_path))
    {
        if let Some(old) = old
            && old
                .game_files
                .iter()
                .any(|g| g.rel.eq_ignore_ascii_case(&a.rel_path) && g.action == GameAction::Add)
        {
            out.extend(
                old.loader_baseline
                    .iter()
                    .find(|b| b.proxy.eq_ignore_ascii_case(&a.rel_path))
                    .cloned(),
            );
            continue;
        }
        let (dir, dir_path) = folder_of(game_root, &a.rel_path)?;
        let (ini_rel, on_disk) = config_in(game_root, &dir)?;
        let ini_sha256 = match planned
            .iter()
            .find(|p| p.rel_path.eq_ignore_ascii_case(&ini_rel))
        {
            Some(p) if p.action == GameAction::Add => None,
            Some(p) if p.action == GameAction::ReuseKnownProxy => Some(p.sha256.clone()),
            Some(p) => p.existing_sha256.clone(),
            // Not this integration's file. One the replaced integration added goes with it.
            None => match on_disk {
                FileState::File { sha256 }
                    if !old.is_some_and(|l| {
                        l.game_files.iter().any(|g| {
                            g.rel.eq_ignore_ascii_case(&ini_rel)
                                && g.action == GameAction::Add
                                && g.sha256 == sha256
                        })
                    }) =>
                {
                    Some(sha256)
                }
                _ => None,
            },
        };
        let bepinex_core = bepinex_core(&dir_path)
            .into_iter()
            .filter(|n| !old.is_some_and(|l| placed_by(l, &dir, n)))
            .collect();
        out.push(LoaderBaseline {
            proxy: a.rel_path.clone(),
            ini_sha256,
            bepinex_core,
        });
    }
    Ok(out)
}

/// Whether the Doorstop proxy at `path` (`rel` below `game_root`), which `ledger`'s install added
/// and which still has the bytes it placed, now also serves someone else's loader, so that
/// removing it would silently switch their mods off — a person who installs BepInEx by hand after
/// us gets the very same `winhttp.dll`, so its digest says nothing. What does is something beside
/// it that was not there before Astra first added it ([`LoaderBaseline`]) and is not ours: a
/// `doorstop_config.ini` other than the one there then and other than ours (or a link or folder
/// in its place), or a name in `BepInEx/core` that was not there then and is no file of ours.
/// Without a baseline (a ledger from before 0.3.1), every config that is not our own file and
/// every `BepInEx/core` entry that is not ours counts. Uninstall then leaves the proxy (reported
/// in `kept`). A proxy that is not a Doorstop (ReShade's `dxgi.dll`) serves no BepInEx: never kept.
pub(crate) fn kept_for_their_loader(
    game_root: &Path,
    rel: &str,
    path: &Path,
    ledger: &Ledger,
) -> Result<bool> {
    if !is_loader_proxy(rel)
        || !doorstop::read_proxy(path).is_some_and(|b| doorstop::generation(&b).is_some())
    {
        return Ok(false);
    }
    let baseline = ledger
        .loader_baseline
        .iter()
        .find(|b| b.proxy.eq_ignore_ascii_case(rel));
    let (dir, dir_path) = folder_of(game_root, rel)?;
    match config_in(game_root, &dir) {
        Ok((_, FileState::Absent)) => {}
        Ok((ini_rel, FileState::File { sha256 })) => {
            // Ours: our own file, with the digest the ledger holds for it. Never a `keep-existing`
            // config: that records one we did not write, which may have arrived since we came
            // (Play records one a mod manager copied over ours that way). The one from before us
            // is the baseline's.
            let ours = ledger.game_files.iter().any(|g| {
                g.rel.eq_ignore_ascii_case(&ini_rel)
                    && g.sha256 == sha256
                    && matches!(g.action, GameAction::Add | GameAction::ReuseKnownProxy)
            });
            let before_us =
                baseline.is_some_and(|b| b.ini_sha256.as_deref() == Some(sha256.as_str()));
            if !ours && !before_us {
                return Ok(true);
            }
        }
        // A link or a folder in its place: not a file anyone left there for us.
        Err(e) if e.code == codes::PATH_ESCAPES => return Ok(true),
        Err(e) => return Err(e),
    }
    let before = |name: &str| {
        baseline.is_some_and(|b| b.bepinex_core.iter().any(|n| n.eq_ignore_ascii_case(name)))
    };
    Ok(bepinex_core(&dir_path)
        .iter()
        .any(|n| !placed_by(ledger, &dir, n) && !before(n)))
}

/// How many names of a proxy's folder a [`ProxyFolder`] holds. Past that it is `capped`, holds
/// none, and the uninstall keeps the proxy whatever the folder holds then.
pub(crate) const PROXY_FOLDER_NAMES: usize = 5_000;

/// Whether `ledger` places `name` in the folder `dir` (below the exe's folder): one of its game
/// files is that name there, or lies below it. `added`: only what it added (what undoing it
/// takes away).
fn places(ledger: &Ledger, dir: &[String], name: &str, added: bool) -> bool {
    ledger
        .game_files
        .iter()
        .filter(|g| !added || g.action == GameAction::Add)
        .any(|g| {
            let comps: Vec<&str> = g.rel.split('/').collect();
            comps.len() > dir.len()
                && comps
                    .iter()
                    .zip(dir)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
                && comps[dir.len()].eq_ignore_ascii_case(name)
        })
}

/// What sits beside each hijack-named file `planned` adds (plan B §3, "Uninstall deletes their
/// identical loader"; merged plan R42): the [`ProxyFolder`] the ledger keeps, so the uninstall
/// can tell a folder that gained someone else's files since. A proxy the replaced integration
/// (`old`) had added already keeps that install's baseline, carried unchanged, so that what
/// arrived since stays someone else's (as [`loader_baselines`] carries its own). A proxy added
/// now for the first time is measured now, before `old` is undone: the folder's names that are
/// not `old`'s additions, at most [`PROXY_FOLDER_NAMES`].
pub(crate) fn proxy_folders(
    game_root: &Path,
    planned: &[GameFileAction],
    m: &Manifest,
    old: Option<&Ledger>,
) -> Result<Vec<ProxyFolder>> {
    let policy = LoaderPolicy::of(m);
    let mut out = Vec::new();
    for a in planned
        .iter()
        .filter(|a| a.action == GameAction::Add && policy.hijack(&a.rel_path))
    {
        if let Some(old) = old
            && old
                .game_files
                .iter()
                .any(|g| g.rel.eq_ignore_ascii_case(&a.rel_path) && g.action == GameAction::Add)
            && let Some(carried) = old
                .proxy_folders
                .iter()
                .find(|b| b.proxy.eq_ignore_ascii_case(&a.rel_path))
        {
            out.push(carried.clone());
            continue;
        }
        let (dir, dir_path) = folder_of(game_root, &a.rel_path)?;
        let mut names = Vec::new();
        let mut capped = false;
        match std::fs::read_dir(&dir_path) {
            Ok(entries) => {
                for e in entries {
                    let e = e.map_err(|e| LauncherError::io_at(&e, &dir_path))?;
                    let name = e.file_name().to_string_lossy().into_owned();
                    if old.is_some_and(|l| places(l, &dir, &name, true)) {
                        continue;
                    }
                    if names.len() == PROXY_FOLDER_NAMES {
                        capped = true;
                        names.clear();
                        break;
                    }
                    names.push(name);
                }
            }
            // A folder the install creates for it: nothing was there.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(LauncherError::io_at(&e, &dir_path)),
        }
        names.sort();
        out.push(ProxyFolder {
            proxy: a.rel_path.clone(),
            names,
            capped,
        });
    }
    Ok(out)
}

/// The extensions of a file that can be, or carry, another mod loader beside a proxy of ours: a
/// program, a library, an ASI plugin, a ReShade add-on. Nothing else that appears in a proxy's
/// folder can: a config, a log, a screenshot — what the loaders we place write there themselves
/// while the game runs (ReShade's `ReShade.ini`, `ReShade.log`, its presets and screenshots) and
/// what a game writes beside its program.
const LOADER_CODE_EXTENSIONS: &[&str] = &["dll", "asi", "exe", "addon", "addon32", "addon64"];

/// Whether a name that appeared in a proxy's folder (`dir`: a folder or a link there) can carry
/// another mod loader: a folder (BepInEx's, UE4SS's, `Mods/`), a link, or a file with one of
/// [`LOADER_CODE_EXTENSIONS`].
fn can_carry_a_loader(name: &str, file_type: std::fs::FileType) -> bool {
    file_type.is_dir()
        || file_type.is_symlink()
        || LOADER_CODE_EXTENSIONS.contains(&crate::detect::ext_lower(name).as_str())
}

/// Whether the folder of `rel`, a hijack-named file `ledger`'s install added, holds a name now
/// that its [`ProxyFolder`] does not, `ledger` does not place, and that can carry a mod loader
/// ([`can_carry_a_loader`]: a folder, a link, a program or a library — never a config, a log or
/// a screenshot, which is what our own ReShade and UE4SS write beside their proxies), or its
/// baseline is `capped`: someone else's loader may run through our proxy now (theirs is byte for
/// byte ours: UE4SS's `dwmapi.dll`, a hand-made BepInEx's `winhttp.dll`), so the uninstall
/// leaves it. Never for UE4SS's proxy with no UE4SS beside it (`crate::former`'s rule 1): nothing
/// runs through it, and kept it fails every plain start of the game. The trade-off, as R42 binds
/// it: a game update that adds a library or a folder there keeps the proxy too, and the
/// uninstall says why (`kept_reasons`). No baseline (a ledger from before 0.5.0): never.
pub(crate) fn proxy_folder_gained(game_root: &Path, rel: &str, ledger: &Ledger) -> Result<bool> {
    let Some(b) = ledger
        .proxy_folders
        .iter()
        .find(|b| b.proxy.eq_ignore_ascii_case(rel))
    else {
        return Ok(false);
    };
    let (dir, dir_path) = folder_of(game_root, rel)?;
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let orphaned_ue4ss = ledger
        .game_files
        .iter()
        .find(|g| g.rel.eq_ignore_ascii_case(rel))
        .is_some_and(|g| {
            former::former_kind(&dir_path, name, &g.sha256) == Some(FormerKind::Ue4ssProxy)
        });
    if orphaned_ue4ss {
        return Ok(false);
    }
    if b.capped {
        return Ok(true);
    }
    let entries = std::fs::read_dir(&dir_path).map_err(|e| LauncherError::io_at(&e, &dir_path))?;
    for e in entries {
        let e = e.map_err(|e| LauncherError::io_at(&e, &dir_path))?;
        let name = e.file_name().to_string_lossy().into_owned();
        let file_type = e
            .file_type()
            .map_err(|e| LauncherError::io_at(&e, &dir_path))?;
        if !b.names.iter().any(|n| n.eq_ignore_ascii_case(&name))
            && !places(ledger, &dir, &name, false)
            && can_carry_a_loader(&name, file_type)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Another Doorstop in `dir_path`, under another proxy name than `ours` (their BepInEx as
/// `version.dll` or `winmm.dll`), judged by its bytes; links followed, as Windows follows them.
fn other_doorstop(dir_path: &Path, ours: &str) -> Option<String> {
    doorstop::PROXY_NAMES
        .iter()
        .filter(|n| !n.eq_ignore_ascii_case(ours))
        .find_map(|n| {
            let p = find_any_case(dir_path, &[n])?;
            let mut bytes = Vec::new();
            std::fs::File::open(&p)
                .ok()?
                .take(doorstop::MAX_PROXY_BYTES)
                .read_to_end(&mut bytes)
                .ok()?;
            doorstop::generation(&bytes)?;
            p.file_name().map(|f| f.to_string_lossy().into_owned())
        })
}

/// Refuses adding `proxy`, a Doorstop (`ours`, its bytes), beside a mod loader of the person's:
///
/// - `foreign-loader`, naming it, when another Doorstop sits in that folder under another name
///   (BepInEx as `version.dll`, `winmm.dll`): it loads into every start too, and its config is
///   theirs to keep switched on, so the `switched-off-loader` advice would be wrong;
/// - `switched-off-loader` when a `doorstop_config.ini` this install does not write stays beside
///   it and switches Doorstop on with a target that exists. That is a mod loader the person
///   switched off by removing its proxy (BepInEx's `winhttp.dll` deleted or renamed, its folder
///   and settings left): ours would read their file on every start Steam makes without our
///   options, and load their mods again — old mods on an updated game can crash it. A target
///   that is not there (r2modman's and Thunderstore Mod Manager's file names one in their own
///   profile, passed on their command line) loads nothing, and is no reason to refuse.
///
/// A proxy already there is the replaced integration's own (`state_before` maps it to absent):
/// the plan changes nothing about how the game starts, and is not refused for it.
fn refuse_reviving_their_loader(
    game_root: &Path,
    proxy: &GameFileAction,
    ours: &[u8],
    planned: &[GameFileAction],
    platform: &Platform,
) -> Result<()> {
    let Some(generation) = doorstop::generation(ours) else {
        return Ok(());
    };
    let comps = checked_rel(&proxy.rel_path)?;
    if existing_target(game_root, &comps)?.is_some_and(|p| std::fs::symlink_metadata(p).is_ok()) {
        return Ok(());
    }
    let (dir, dir_path) = folder_of(game_root, &proxy.rel_path)?;
    let name = comps.last().map(String::as_str).unwrap_or_default();
    if let Some(other) = other_doorstop(&dir_path, name) {
        let base: String = dir.iter().map(|c| format!("{c}/")).collect();
        return Err(
            LauncherError::new(codes::FOREIGN_LOADER).with("file", format!("{base}{other}"))
        );
    }
    let mut ini = dir.clone();
    ini.push(doorstop::CONFIG_NAME.to_owned());
    let ini_rel = ini.join("/");
    if planned.iter().any(|a| {
        a.rel_path.eq_ignore_ascii_case(&ini_rel)
            && matches!(a.action, GameAction::Add | GameAction::Replace)
    }) {
        return Ok(());
    }
    let Some(text) =
        find_any_case(&dir_path, &[doorstop::CONFIG_NAME]).and_then(|p| doorstop::read_config(&p))
    else {
        return Ok(());
    };
    let Some(target) = doorstop::plain_start_target(&text, generation) else {
        return Ok(());
    };
    if !target_exists(&dir_path, &target, platform) {
        return Ok(());
    }
    let file = real_case(game_root, &ini).map_or(ini_rel, |r| r.join("/"));
    Err(LauncherError::new(codes::SWITCHED_OFF_LOADER)
        .with("file", file)
        .with("target", target))
}

/// Whether the assembly a Doorstop in `dir` is told to load (`target`, as its config writes it)
/// exists on this machine. Relative: below `dir`, the game's working folder, in any letter case.
/// Absolute: as Windows sees it, or under Proton through the prefix's drive letters (`Z:`, and a
/// path from the root of the game's drive, are `/` when the prefix is not known). A path that
/// cannot be looked up counts as existing: the refusal that follows names the file to fix.
fn target_exists(dir: &Path, target: &str, platform: &Platform) -> bool {
    let parts: Vec<&str> = target.split(['\\', '/']).collect();
    let first = parts[0];
    let drive = first.len() == 2
        && first.as_bytes()[1] == b':'
        && first.as_bytes()[0].is_ascii_alphabetic();
    if !drive && !first.is_empty() {
        return find_any_case(dir, &parts).is_some();
    }
    if cfg!(windows) && matches!(platform, Platform::Windows) {
        return Path::new(target).exists();
    }
    let Platform::LinuxProton { prefix } = platform else {
        return true;
    };
    let root = if drive && parts.len() > 1 {
        let letter = first[..1].to_ascii_lowercase();
        prefix
            .as_deref()
            .map(|p| p.join("dosdevices").join(format!("{letter}:")))
            .filter(|r| r.exists())
            .or_else(|| (letter == "z").then(|| PathBuf::from("/")))
    } else if first.is_empty() && parts.get(1).is_some_and(|p| !p.is_empty()) {
        // `\a\b`: from the root of the drive the game runs from, which Proton puts on `Z:`.
        Some(PathBuf::from("/"))
    } else {
        None
    };
    match root {
        Some(root) => find_any_case(&root, &parts[1..]).is_some(),
        None => true,
    }
}

/// Checks the package against the game and says exactly what an install would do: launcher
/// 0.4.1's planning, exactly — [`plan_install_in`] with no engine descriptors, first-party, and
/// no pick.
pub fn plan_install(
    pkg: &GiPackage,
    game: &GameTarget,
    profile_root: &Path,
) -> Result<InstallPlan> {
    plan_install_in(
        pkg,
        game,
        profile_root,
        &EngineSet::empty(),
        Provenance::FirstParty,
        None,
    )
}

/// [`plan_install`], asking the engine descriptors in `engines` (the set the daemon detects
/// with) when `[target] engine` is a descriptor's code, for an integration `provenance` vouches
/// for.
///
/// - **A built-in code** (`unity-mono`, `unreal`, …) is checked exactly as [`plan_install`]
///   checks it, by the built-in rules alone, whatever `engines` holds.
/// - **A descriptor's code** passes where the descriptor Pass B would choose among the game
///   program's own matches names it — never in a folder the built-ins decided
///   ([`Detection::open_to_descriptors`]), and never on a program a built-in rule names; both
///   asked with the program Steam launches for the trusted appid as the hint, as the daemon
///   detects. Where descriptors of one standing disagree about that program, it passes only
///   when `engine_pick` (the player's choice among them) is the manifest's code and that code
///   is among them: `engine-ambiguous{want, codes}` otherwise. Elsewhere it is
///   `engine-mismatch{want, got}`.
/// - **`[integration.engine]`**: the manifest's own descriptor must be the very one that named
///   the engine, `descriptor-mismatch{want, got}` otherwise (`got` = `none` when no descriptor
///   did) — and its own refusal when it is no descriptor at all.
/// - **Anti-cheat**, before the engine is checked and for every game, whatever engine it has:
///   `anti-cheat{kind}`, `kind` the first of a folder signature's code
///   ([`crate::ANTI_CHEAT_SIGNATURES`]), `listed-anti-cheat` (a marker `engines` carries,
///   [`EngineSet::with_anti_cheat_markers`]) and `kernel-anti-cheat` (a kernel driver whose
///   image is in the game's folder, [`crate::anti_cheat_services`]); then VAC
///   (`valve-anti-cheat`). Every code is in [`crate::ANTI_CHEAT_CODES`]. And for a descriptor's
///   code, once the descriptors' claim on the program is known (before a pick is asked for):
///   what that claim carries, as [`crate::Detection::anti_cheat`] says it — a marker of the
///   claiming descriptor's own (its code as written), then `anti-cheat-unknown` when the walk
///   stopped at its cap or a kernel driver (`*.sys`) ships (plan B §3).
///
/// The plan pins the descriptor that named the engine ([`InstallPlan::engine_descriptor`]) and
/// records `provenance`; [`crate::install`] plans again with both.
///
/// [`Detection::open_to_descriptors`]: crate::Detection::open_to_descriptors
pub fn plan_install_in(
    pkg: &GiPackage,
    game: &GameTarget,
    profile_root: &Path,
    engines: &EngineSet,
    provenance: Provenance,
    engine_pick: Option<&EngineCode>,
) -> Result<InstallPlan> {
    plan_install_with_user_pack_root(
        pkg,
        game,
        profile_root,
        engines,
        provenance,
        engine_pick,
        None,
    )
}

/// [`plan_install_in`], with the desktop companion's user root (`user_pack_root`: the
/// `ASTRA_USER_PACK` the caller pairs with the companion's engine, the daemon's
/// `CompanionLocation`) for a package that hands the companion its own animation packs
/// (`${astra_pack_inbox}`, [`crate::pack_inbox`]): they go to
/// `<user_pack_root>/inbox/packs/<integration id>/` ([`InstallPlan::pack_inbox`]).
///
/// Such a package planned without one is `pack-inbox-unavailable{reason=no-user-root}`: the
/// launcher never guesses where the companion keeps its data. The root must be an absolute local
/// folder, or nothing yet (the install makes it, as the companion would at its first start):
/// `path-escapes` (`reason` = `unc`, `verbatim-unc`, `device`, `verbatim`, `not-absolute`,
/// `parent`, judged by its text as Windows reads it on every machine; `pack-inbox-overlaps-game`;
/// a link or a file on the way below it), `pack-inbox-unavailable{reason=not-a-directory}`. A
/// package that places no pack plans exactly as [`plan_install_in`], whatever root is given.
pub fn plan_install_with_user_pack_root(
    pkg: &GiPackage,
    game: &GameTarget,
    profile_root: &Path,
    engines: &EngineSet,
    provenance: Provenance,
    engine_pick: Option<&EngineCode>,
    user_pack_root: Option<&Path>,
) -> Result<InstallPlan> {
    let how = Planning {
        engines,
        provenance,
        engine_pick,
        user_pack_root,
    };
    plan_install_with(pkg, game, profile_root, how, None)
}

/// [`plan_install_in`]; with `after` (the ledger install has just undone), install's look at the
/// game folder once the integration it replaces is undone: the refusals about a mod loader of
/// the person's beside the game are not made again (undoing it removes our own proxy for a
/// moment), and a file the undoing put back from `after`'s backup is still the one held there
/// (step 10a).
pub(crate) fn plan_install_with(
    pkg: &GiPackage,
    game: &GameTarget,
    profile_root: &Path,
    how: Planning,
    after: Option<&Ledger>,
) -> Result<InstallPlan> {
    let loader_checks = after.is_none();
    let m = pkg.manifest();
    if m.target.anti_cheat != "none" {
        return Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", m.target.anti_cheat.clone()));
    }
    let exe = game.exe_components()?;
    let exe_rel = exe.join("/");
    let game_root = game.game_root()?;
    let game_dir = canonical_dir(&game.dir)?;

    if m.target.exe != "*" && !m.target.exe.eq_ignore_ascii_case(&exe_rel) {
        return Err(LauncherError::new(codes::GAME_MISMATCH)
            .with("field", "exe")
            .with("want", m.target.exe.clone())
            .with("got", exe_rel));
    }
    // The appid this game is known to be; the manifest's may only refuse against it.
    let trusted = trusted_appid(game)?;
    if let (Some(want), Some(got)) = (m.target.steam_appid, trusted)
        && want != got
    {
        return Err(LauncherError::new(codes::GAME_MISMATCH)
            .with("field", "steam_appid")
            .with("want", want.to_string())
            .with("got", got.to_string()));
    }
    let platform = game.platform.code();
    if !m.target.platforms.iter().any(|p| p == platform) {
        return Err(LauncherError::new(codes::PLATFORM_UNSUPPORTED)
            .with("platform", platform)
            .with("supported", m.target.platforms.join(",")));
    }
    // Every anti-cheat signal the launcher reads (merged plan §2.4), before the engine check: the
    // folder signatures, the catalogue's markers `how.engines` carries, and a kernel driver
    // installed from the game's folder; a descriptor's claim adds its own in `engine_check`.
    // Steam's store categories (an online game) never refuse here; that is the daemon's offer
    // rule. Play asks the very same ([`refuse_anti_cheat`]).
    refuse_anti_cheat(game, &game_dir, how.engines)?;
    // Cleared only for an appid the caller gave or the folder proves; the manifest's appid can
    // only add a refusal. No trusted appid is UNCHECKED, never clear.
    let vac = vac_check(game, trusted, m.target.steam_appid)?;
    let engine_descriptor = engine_check(m, how, game, trusted, &game_dir, &exe)?;
    // What a community integration may not do (plan B §3). After the engine check, so a game
    // made with another engine is told so first.
    if how.provenance == Provenance::Community {
        crate::reserved::check_identity(&m.integration)?;
        if let Some(e) = community_limits(m).into_iter().next() {
            return Err(e);
        }
        // The program its files go beside: one Pass B took for the descriptor that named the
        // engine (by that descriptor's own tests), else one the built-in walk or Steam's launch
        // entry gives — and never inside another loader's folder.
        let theirs = exe.iter().any(|c| {
            OTHER_LOADERS_FOLDERS
                .iter()
                .any(|f| f.eq_ignore_ascii_case(c))
        });
        let program = engine_descriptor.is_some()
            || is_walked_program(&game_dir, &exe, planning_hint(game, trusted).as_deref())?;
        if theirs || !program {
            return Err(community_limit(limit::EXE_NOT_A_PROGRAM).with("exe", exe_rel.clone()));
        }
    }

    let profile_dir = absolute(profile_root)?;
    let profile_resolved = resolve_lenient(&profile_dir)?;
    if profile_resolved.starts_with(&game_dir) || game_dir.starts_with(&profile_resolved) {
        return Err(LauncherError::new(codes::PATH_ESCAPES)
            .with("path", profile_dir.display().to_string())
            .with("reason", "profile-overlaps-game"));
    }
    // The packs for the desktop companion, and where they go: a package that has some is planned
    // only with the companion's folder named.
    let pack_inbox = crate::pack_inbox::plan(pkg, how.user_pack_root, &game_dir)?;
    let existing = if profile_resolved.is_dir() {
        ledger::read(&profile_resolved)?
    } else {
        None
    };
    // The installed integration's files are this folder's only when it placed them here: a game
    // update that moved the program leaves them in the folder the ledger names, where install
    // undoes them ([`crate::install`]), and nothing here is theirs.
    let here = match &existing {
        Some(l) if crate::install::placed_in(game, l, &game_root)? => Some(l),
        _ => None,
    };

    let exe_path = game_root.join(game.exe_file_name()?);
    let policy = LoaderPolicy::of(m);
    let community = how.provenance == Provenance::Community;
    // A foreign loader a ledger of Astra's own holds in its backup, kept as `replace` (10a).
    let mut loader_in_backup = false;
    let mut game_files = Vec::new();
    for f in &pkg.game_files {
        let real = real_case(&game_root, &f.rel)?;
        let rel = real.join("/");
        let path = existing_target(&game_root, &real)?;
        let (state, in_backup) = match &path {
            Some(path) => state_before(path, &rel, here, after)?,
            None => (FileState::Absent, false),
        };
        let name = f
            .rel
            .last()
            .map(|n| n.to_ascii_lowercase())
            .unwrap_or_default();
        // A file an older Astra left there (`crate::former`, its rules exactly) and no ledger
        // names: ours takes its place, as onto nothing. A file of the current ledger is the
        // ledger's, never this.
        let recorded = here.is_some_and(|l| {
            l.game_files
                .iter()
                .any(|g| g.rel.eq_ignore_ascii_case(&rel))
        });
        let former = match (&state, &path) {
            (FileState::File { sha256 }, Some(path)) if !recorded => {
                let file = real.last().map(String::as_str).unwrap_or_default();
                path.parent()
                    .and_then(|d| former::former_kind(d, file, sha256))
            }
            _ => None,
        };
        // In this order: an older Astra's file first, then ours, then a name this install keeps,
        // then a loader's name, then anything else.
        let (action, existing_sha256) = match state {
            FileState::Absent => (GameAction::Add, None),
            FileState::File { sha256 } if former.is_some() => (GameAction::Add, Some(sha256)),
            FileState::File { sha256 } if sha256 == f.sha256 => (GameAction::ReuseKnownProxy, None),
            FileState::File { sha256 } if policy.keep(&name) => {
                (GameAction::KeepExisting, Some(sha256))
            }
            // A foreign loader in the backup of a `replace` an earlier launcher made (a UE4SS
            // `dwmapi.dll` before step 10a): ours stays its stand-in, and the uninstall puts theirs
            // back. Never for a community integration, which has no `replace` arm at all.
            FileState::File { sha256 } if policy.hijack(&name) && in_backup && !community => {
                loader_in_backup = true;
                (GameAction::Replace, Some(sha256))
            }
            FileState::File { .. } if policy.hijack(&name) => {
                let shared = match &path {
                    Some(path) => shared_loader(pkg, f, path, &exe_path)?,
                    None => None,
                };
                match shared {
                    Some(theirs) => (GameAction::ShareLoader, Some(theirs)),
                    None => {
                        return Err(LauncherError::new(codes::FOREIGN_LOADER).with("file", rel));
                    }
                }
            }
            // A player's file: a community integration never replaces one (plan B §3).
            FileState::File { .. } if community => {
                return Err(LauncherError::new(codes::FOREIGN_FILE).with("file", rel));
            }
            FileState::File { sha256 } => (GameAction::Replace, Some(sha256)),
        };
        game_files.push(GameFileAction {
            rel_path: rel,
            action,
            sha256: f.sha256.clone(),
            existing_sha256,
        });
    }

    // A Doorstop we add reads the `doorstop_config.ini` beside it whenever the game starts
    // without our options. Only Windows and Proton load a proxy at all.
    if loader_checks
        && matches!(
            game.platform,
            Platform::Windows | Platform::LinuxProton { .. }
        )
    {
        for (f, a) in pkg.game_files.iter().zip(&game_files) {
            if a.action == GameAction::Add && is_loader_proxy(&a.rel_path) {
                let ours = pkg.read(&mut pkg.archive()?, f)?;
                refuse_reviving_their_loader(&game_root, a, &ours, &game_files, &game.platform)?;
            }
        }
    }

    // What an older Astra left beside the program that neither this package places nor a ledger
    // names: the install removes it.
    // (The shaders' folder counts as named when anything is placed or recorded below it.)
    let names = |rel: &str, name: &str| {
        let (rel, name) = (rel.to_ascii_lowercase(), name.to_ascii_lowercase());
        rel == name || rel.starts_with(&format!("{name}/"))
    };
    let former_files: Vec<FormerFile> = former::former_set(&game_root)
        .into_iter()
        .filter(|f| {
            !game_files.iter().any(|g| names(&g.rel_path, &f.name))
                && !here.is_some_and(|l| l.game_files.iter().any(|g| names(&g.rel, &f.name)))
        })
        .map(|f| FormerFile {
            rel_path: f.name,
            sha256: f.sha256,
            kind: f.kind,
        })
        .collect();

    let mut warnings = vec!["experimental".to_owned(), "not-reviewed".to_owned()];
    if existing.is_some() {
        warnings.push("replaces-integration".to_owned());
    }
    if m.integration.license.trim().is_empty() {
        warnings.push("no-license".to_owned());
    }
    if !vac {
        warnings.push("vac-unchecked".to_owned());
    }
    if !former_files.is_empty() {
        warnings.push("former-astra-files".to_owned());
    }
    if loader_in_backup {
        warnings.push(codes::FOREIGN_LOADER_IN_BACKUP.to_owned());
    }

    Ok(InstallPlan {
        integration: m.integration.clone(),
        source_sha256: pkg.sha256().to_owned(),
        profile_dir,
        game_dir: crate::paths::plain(&game_root),
        profile_files: pkg.profile_files.len() as u32,
        profile_bytes: pkg.profile_files.iter().map(|f| f.size).sum(),
        game_files,
        launch_args: m.launch.args.clone(),
        proton_overrides: match game.platform {
            Platform::LinuxProton { .. } => m.proton_overrides(),
            _ => Vec::new(),
        },
        replaces: existing.map(|l| l.integration),
        former_files,
        warnings,
        vac_checked: vac,
        provenance: how.provenance,
        engine_descriptor,
        pack_inbox,
    })
}

/// Planning's engine check ([`plan_install_in`]'s list), for every code `[target]` may name:
/// a code nothing here detects is a refusal, never a check skipped. Returns the descriptor that
/// named the engine, pinned; `None` for a built-in engine, and when `[target]` names none.
fn engine_check(
    m: &Manifest,
    how: Planning,
    game: &GameTarget,
    trusted: Option<u32>,
    game_dir: &Path,
    exe: &[String],
) -> Result<Option<PinnedDescriptor>> {
    let Some(want) = m.engine_code() else {
        return Ok(None);
    };
    let named = if want.builtin().is_some() || how.engines.is_empty() {
        // A built-in code: by the built-in rules alone, as launcher 0.4.1 checked it (C1b, and
        // C1c for planning: no set changes such a plan).
        engine_of_exe_with(&EngineSet::empty(), game_dir, exe, None)
    } else {
        engine_of_exe_with(
            how.engines,
            game_dir,
            exe,
            planning_hint(game, trusted).as_deref(),
        )
    };
    // The anti-cheat a descriptor's claim carries (`Detection::anti_cheat` for that claim): its
    // own markers, and `anti-cheat-unknown` where the walk cannot vouch for the folder. Refused
    // before a pick is asked for: it is the same whichever code is picked.
    let claims_want = named.code == want || named.candidates.iter().any(|(c, _)| *c == want);
    if claims_want && let Some(kind) = named.anti_cheat.first() {
        return Err(LauncherError::new(codes::ANTI_CHEAT).with("kind", kind.clone()));
    }
    let digest = if named.code == want {
        named.descriptor_sha256
    } else if let Some((_, d)) = named.candidates.iter().find(|(c, _)| *c == want) {
        if how.engine_pick != Some(&want) {
            let standing: Vec<&str> = named.candidates.iter().map(|(c, _)| c.as_str()).collect();
            return Err(LauncherError::new(codes::ENGINE_AMBIGUOUS)
                .with("want", want.as_str())
                .with("codes", standing.join(",")));
        }
        Some(d.clone())
    } else {
        return Err(LauncherError::new(codes::ENGINE_MISMATCH)
            .with("want", want.as_str())
            .with("got", named.code.as_str()));
    };
    if let Some(own) = m.engine_descriptor() {
        let own = own.map_err(Clone::clone)?;
        let got = digest.as_deref().unwrap_or("none");
        if own.digest() != got {
            return Err(LauncherError::new(codes::DESCRIPTOR_MISMATCH)
                .with("want", own.digest())
                .with("got", got));
        }
    }
    let Some(digest) = digest else {
        return Ok(None);
    };
    how.engines
        .descriptors()
        .find(|(d, _)| d.digest() == digest)
        .map(|(d, origin)| Some(PinnedDescriptor::of(d, origin)))
        .ok_or_else(|| LauncherError::io("the descriptor that named the engine is not in the set"))
}

/// The program Steam launches for this game, as the daemon detects with: [`launch_executable`]
/// for the trusted appid, from the first `appinfo.vdf` that answers ([`appinfo_files`]), for the
/// OS the game runs as (`windows` for Windows and for a Windows game under Proton, `linux` for a
/// native one). `None` without a trusted appid. A hint, never trusted: the evaluator jails it.
fn planning_hint(game: &GameTarget, trusted: Option<u32>) -> Option<String> {
    let appid = trusted?;
    let os = match game.platform {
        Platform::Windows | Platform::LinuxProton { .. } => "windows",
        Platform::LinuxNative => "linux",
    };
    appinfo_files(game)
        .iter()
        .find_map(|p| launch_executable(p, appid, os))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one list holds the Doorstop-sharing names and every name a Doorstop goes by, and no
    /// name every install keeps: so `keep` (which planning asks first) never shadows a loader.
    #[test]
    fn hijack_names_hold_the_doorstop_names_and_no_keep_name() {
        for n in LOADER_PROXIES.iter().chain(doorstop::PROXY_NAMES) {
            assert!(is_hijack_name(n), "{n}");
        }
        for k in KEEP_IF_PRESENT {
            assert!(!is_hijack_name(k), "{k}");
        }
        let mut seen = std::collections::BTreeSet::new();
        for n in HIJACK_NAMES {
            assert!(seen.insert(n.to_ascii_lowercase()), "{n} twice");
            assert!(n.ends_with(".dll") && *n == n.to_ascii_lowercase(), "{n}");
        }
        assert!(is_hijack_name("Some/Folder/DWMAPI.DLL"));
        assert!(!is_hijack_name("dwmapi.dll.bak"));
    }

    #[test]
    fn a_path_argument_stays_under_the_profile_or_the_game() {
        for ok in [
            "-windowed",
            "--doorstop-enabled",
            "true",
            "${profile}/BepInEx/core/BepInEx.Preloader.dll",
            "${game}/someone.example/",
            "${game}/",
            "${profile}/a b/c.ini",
            "-x=1",
            "a=b=c",
            "--level=..x",
            "-name:value",
            "-x=",
        ] {
            assert!(arg_stays_inside(ok), "{ok}");
        }
        for refused in [
            "/etc/passwd",
            "C:\\Windows\\x.dll",
            "..\\x",
            "${game}",
            "${game}/../x",
            "${profile}/a/../../x",
            "${profile}/${bridge_token}",
            "${bridge_token}",
            "-x=${profile}/a",
            "${game}//x",
            "https://example.com/x",
            ".",
            "..",
            "-x=..",
            "-x=.",
            "--path=a=..",
            "C:..",
            "c:evil.dll",
            "C:",
            "-path=C:x",
            "...",
            ". .",
        ] {
            assert!(!arg_stays_inside(refused), "{refused}");
        }
    }
}
