// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Canary C1 over a generated corpus: for every tree of a fixed seed, under every launch hint
//! the generator gives it, this crate's detector answers exactly as launcher 0.4.1's (main
//! 73bf11e) — the same `serde_json` text of `Detection`, anti-cheat included — with no engine
//! descriptors, and with a hostile set (C1c: unchanged wherever Pass A decided). Planning is
//! held to 0.4.1's too, for EVERY program in each tree and a probe integration per built-in code,
//! with the Steam appid the tree is vouched to be: the anti-cheat and VAC refusals, the
//! `vac-unchecked` warning, and `engine-mismatch`'s `got` (the detector's `engine_of_exe`, which
//! planning alone exposes).
//!
//! Canary C1b rides the same trees: every program planned again with one of Astra's own
//! manifests ([`MANIFESTS`]: the Unity runners, the per-game BepInEx integrations, GTA III, and
//! every Unreal runner release astra-reshade published), its game files laid beside the program
//! now and then — absent, ours, or someone else's — and the plan held to 0.4.1's: the same
//! `GameAction` for every file, the same refusal and its parameters.
//!
//! And past the fresh game ([`c1b_install`]): for a share of the programs that plan, the tree is
//! copied once per launcher, each with its own profile, and installed by 0.4.1 and by this crate
//! through the public `plan_install` and `install`. The two ledgers must be equal; then one change
//! is made to both games (none; a Doorstop of someone else's that does our proxy's job laid over
//! it, which reaches `share-loader`; our `doorstop_config.ini` rewritten, `keep-existing`; a file
//! of ours changed; files named as an older Astra left them laid beside), both are planned again
//! from the ledger the install wrote, and now and then that plan is installed as the update;
//! then both are uninstalled. Plans, refusals, ledgers, `UninstallReport`s and the trees left
//! must be equal — but for C1b's three declared rows (`tests/corpus/divergence.txt`), applied to
//! 0.4.1's answer and counted: `foreign-dwmapi` (step 10a: a foreign `dwmapi.dll` where Astra's
//! own manifest places one is `foreign-loader`, never replaced; neither launcher then installs
//! the update), `proxy-folder-gained-files` (merged plan R42: an added hijack-named file whose
//! folder gained a name since is left by the uninstall, `kept_reasons`, and the ledger keeps the
//! folder's listing the test took before the install) and `restore-partial` (marketplace plan
//! A§4.4, M2: an uninstall that leaves a game file as it is warns `restore-partial`). For a share of those, a third copy is installed by launcher 0.3.1 — what
//! Astra 0.2.7 ships, so what wrote every 0.2.7 user's ledger — whose ledger must be 0.4.1's on
//! every field 0.3.1's has; it is changed alike and planned again by this crate, which must plan
//! as 0.4.1 does over its own install. The older Astra's files are look-alikes (the
//! shipped bytes are not committed; docs/ENGINE-DESCRIPTORS-DEBT.md), so no launcher removes them.
//!
//! The tests in `tests/detect.rs` go through the same check, fixture by fixture. Where 0.5.0
//! knowingly answers otherwise (a link, a FIFO, a hint naming a Windows device), C1 has declared
//! exceptions, each pinned by `tests/divergence.rs` against `tests/corpus/divergence.txt`, where
//! 0.4.1 runs in a child process with a timeout. The corpus holds links but no FIFO, and no link
//! a read would follow, so 0.4.1 runs in-process here.
//!
//! **The planning canary** ([`planning_canary`]) rides the same detections: wherever Pass B
//! names a game with a program, `plan_install_in` of that program as that code — with the
//! program Steam launches for the trusted appid as the hint — plans and pins the very
//! descriptor Pass B reported; wherever Pass A closed the folder, no program plans as a
//! descriptor's code; and a program planning names as a descriptor's code has that
//! descriptor's anchor beside it. C1b's plans are also made again with each hostile set and
//! must not change (C1c for planning; one set per plan, in turn).
//!
//! **Against plan B §2's C1** this is the generated corpus at its full size; the `bin/list-shape`
//! recordings (`tests/corpus/recorded/`) are not here yet (docs/ENGINE-DESCRIPTORS-DEBT.md).

mod common;
#[path = "corpus/gen.rs"]
mod corpus;

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use astra_game_launcher::{
    Detection, Engine, EngineSet, ExePick, GameAction, GameTarget, GiPackage, InstallPlan,
    KEPT_FOLDER_GAINED_FILES, LEDGER_FILE_NAME, LauncherError, Platform, Provenance, codes,
    detect_with_engines, launch_executable, plan_install_in, valve_anti_cheat,
};
use common::identity::{
    checked_with_hostile, hostile_lower_set, hostile_set, moderated_hostile_set, narrowed,
    open_to_descriptors,
};
use common::{
    DOORSTOP_V4, E, Tmp, X64, appinfo_launch, appinfo_vac, copy_tree, pe_with_options, sha,
    snapshot, write, zip,
};
use corpus::{App, Node};

/// Fixed for good: a new seed is a new corpus, and the old one is what C1 was proven on.
const SEED: u64 = 0xA57A_0500;
/// Plan B §2: at least 20,000 trees. That is the gate, and what `cargo test` runs.
const TREES: u64 = 20_000;
/// An inner loop may ask for fewer (`AGL_C1_TREES=2400 cargo test --test identity`); the floors
/// scale with it. Never the gate: a release runs the default.
const TREES_ENV: &str = "AGL_C1_TREES";
const THREADS: u64 = 8;

/// The appids of the fixture appinfo: flagged VAC, cleared, and absent from it.
const VAC: u32 = 4_000_011;
const CLEAR: u32 = 4_000_013;
const UNLISTED: u32 = 4_000_016;

/// The appid the planning canary's plans are vouched to be, whose appinfo launches the tree's
/// hint.
const HINTED: u32 = 4_000_019;

/// The codes the hostile sets admit (`tests/common/identity.rs`), and those of the lower
/// standings alone.
const HOSTILE_CODES: [&str; 4] = [
    "hostile-all",
    "hostile-unity",
    "hostile-paks",
    "hostile-pck",
];
const LOWER_CODES: [&str; 3] = ["hostile-unity", "hostile-paks", "hostile-pck"];

/// The walk's bounds (src/detect.rs `MAX_ENTRIES`, `MAX_DEPTH`).
const MAX_ENTRIES: usize = 5000;
const MAX_DEPTH: usize = 4;

/// What the corpus reached. Each has a floor: a corpus that stops reaching a path proves
/// nothing about it, however green.
#[derive(Default)]
struct Counts {
    trees: AtomicUsize,
    detections: AtomicUsize,
    open: AtomicUsize,
    claimed: AtomicUsize,
    /// Claims of the hostile set's lower standings alone (no moderated claim on everything).
    claimed_lower: AtomicUsize,
    /// Open detections where the moderated hostile claim answered alone, held equal to the
    /// whole set's answer.
    moderated_decides: AtomicUsize,
    plans: AtomicUsize,
    anti_cheat: AtomicUsize,
    vac: AtomicUsize,
    vac_unchecked: AtomicUsize,
    mismatch: AtomicUsize,
    capped: AtomicUsize,
    huge_listing: AtomicUsize,
    /// A hint into a folder past 5,000 entries that its truncated listing left unused.
    truncation_hid: AtomicUsize,
    deep_hint_used: AtomicUsize,
    hint_past_cap: AtomicUsize,
    unity_version: AtomicUsize,
    pick_launch_config: AtomicUsize,
    pick_only: AtomicUsize,
    pick_largest: AtomicUsize,
    tied: AtomicUsize,
    bootstrap_tie: AtomicUsize,
    /// Answers only a weak built-in rule gives: any shipping program (no evidence), paks with
    /// no program, a pck embedded in a top-level program.
    weak_shipping: AtomicUsize,
    /// Half a pairing (weak 100): a shipping program in `<P>/Binaries/Win64/` without paks.
    weak_half: AtomicUsize,
    /// Planning's engine check answered by a planning-only rule: a shipping program outside any
    /// `<P>/Binaries/Win64/` (140), a program below the top with a pck embedded and none beside
    /// it (150).
    plan_shipping_anywhere: AtomicUsize,
    plan_embedded_below: AtomicUsize,
    paks_only: AtomicUsize,
    embedded_pck: AtomicUsize,
    /// A built-in engine named for a game whose evidence holds a name only a Linux disk holds
    /// (`corpus::ODD_NAMES`); and an Unreal project so named chosen over another.
    odd_named: AtomicUsize,
    odd_project: AtomicUsize,
    /// Trees with nine or more programs at the top: past the weak Godot rule's `first = 8`.
    crowded: AtomicUsize,
    /// C1b: plans with Astra's own manifests, those that planned, the actions they hold, and
    /// every refusal by its code.
    c1b_plans: AtomicUsize,
    c1b_planned: AtomicUsize,
    c1b_actions: Mutex<BTreeMap<String, usize>>,
    c1b_refusals: Mutex<BTreeMap<String, usize>>,
    /// C1b past the fresh game ([`c1b_install`]): pairs installed, and pairs whose install (on
    /// the copies) both launchers refused alike.
    c1b_installs: AtomicUsize,
    c1b_install_refused: AtomicUsize,
    /// Plans from the ledger an install wrote, the actions they hold and their refusals.
    c1b_replans: AtomicUsize,
    c1b_replan_actions: Mutex<BTreeMap<String, usize>>,
    c1b_replan_refusals: Mutex<BTreeMap<String, usize>>,
    /// Those plans installed as the update, and the uninstalls that ended every pair.
    c1b_updates: AtomicUsize,
    c1b_uninstalls: AtomicUsize,
    /// Plans by this crate from a ledger launcher 0.3.1 wrote, held to 0.4.1's over its own.
    c1b_cross: AtomicUsize,
    /// The changes made between install and plan, by kind (`nothing` when the kind found nothing
    /// to change), and a file of ours changed where we had replaced the game's.
    c1b_changes: Mutex<BTreeMap<String, usize>>,
    c1b_changed_replaced: AtomicUsize,
    /// Older-Astra look-alikes laid beside the game, every one still there after the uninstall;
    /// and what the uninstalls reported removing as an older Astra's (none can be: the shipped
    /// bytes are not committed).
    c1b_lookalikes_kept: AtomicUsize,
    c1b_former_removed: AtomicUsize,
    /// Installs per manifest.
    c1b_installed: Mutex<BTreeMap<String, usize>>,
    /// C1c for planning: C1b's plans made again with a hostile set (each in turn), held equal.
    c1b_hostile: AtomicUsize,
    /// C1b's declared exceptions (`tests/corpus/divergence.txt`), applied: plans where Astra's
    /// own manifest places `dwmapi.dll` over a foreign one (`foreign-dwmapi`: 0.4.1 replaces it,
    /// this crate refuses `foreign-loader`), fresh and from a ledger; uninstalls that left a
    /// file we added because its folder gained a name (`proxy-folder-gained-files`); and
    /// uninstalls that left a game file as it is, warning `restore-partial`.
    c1b_row_dwmapi: AtomicUsize,
    c1b_row_proxy_folder: AtomicUsize,
    c1b_row_restore_partial: AtomicUsize,
    /// The planning canary ([`planning_canary`]): Pass B named the game with a program, and
    /// planning that program pinned the same descriptor — with the whole hostile set, and with
    /// its lower standings.
    planning_pinned: AtomicUsize,
    planning_pinned_lower: AtomicUsize,
    /// Programs of folders Pass A closed, planned as a descriptor's code and refused by the
    /// engine check with a built-in `got`.
    planning_closed: AtomicUsize,
    /// Programs of open folders planned with the lower standings: named by a descriptor (its
    /// anchor beside them), and left unknown in a game those standings named — of those, the
    /// ones without the winner's anchor, which planning would name were it not restricted to
    /// the program's own matches.
    planning_named: AtomicUsize,
    planning_unclaimed: AtomicUsize,
    planning_unclaimed_anchorless: AtomicUsize,
    /// Plans asked with Steam's hint for the trusted appid (a hint the tree gives).
    planning_hinted: AtomicUsize,
}

fn bump(a: &AtomicUsize) {
    a.fetch_add(1, Ordering::Relaxed);
}

fn probe_zip(engine: &str) -> Vec<u8> {
    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"probe.engine\"\nname = \"Probe\"\nversion = \"1.0.0\"\n\n[target]\nengine = \"{engine}\"\nexe = \"*\"\nplatforms = [\"windows-x64\", \"linux-proton\", \"linux-x64\"]\nanti_cheat = \"none\"\n"
    );
    zip(&[E::File("astra-gi.toml", manifest.as_bytes())])
}

/// C1b's manifests: Astra's own, verbatim, each with where it was read (repository, commit,
/// path). The game files' pins are rewritten to the bytes [`fixture`] makes up for them; nothing
/// else is touched.
const MANIFESTS: &[(&str, &str)] = &[
    (
        "astra-bepinex 793d408 runner/astra-gi.toml",
        include_str!("fixtures/manifests/bepinex-mono.astra-gi.toml"),
    ),
    (
        "astra-bepinex 793d408 runner/il2cpp/astra-gi.toml",
        include_str!("fixtures/manifests/bepinex-il2cpp.astra-gi.toml"),
    ),
    (
        "astra-lethal 0760fd1 astra-gi.toml",
        include_str!("fixtures/manifests/lethal-company.astra-gi.toml"),
    ),
    (
        "astra-peak 9dacb72 astra-gi.toml",
        include_str!("fixtures/manifests/peak.astra-gi.toml"),
    ),
    (
        "astra-repo 44fa6a4 astra-gi.toml",
        include_str!("fixtures/manifests/repo.astra-gi.toml"),
    ),
    (
        "astra-content-warning aebab96 astra-gi.toml",
        include_str!("fixtures/manifests/content-warning.astra-gi.toml"),
    ),
    (
        "astra-gta-trilogy 0e78023 manifests/astra-gi-gta3.toml",
        include_str!("fixtures/manifests/gta3.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.0",
        include_str!("fixtures/astra-reshade-unreal/v0.2.0.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.1",
        include_str!("fixtures/astra-reshade-unreal/v0.2.1.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.2",
        include_str!("fixtures/astra-reshade-unreal/v0.2.2.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.3",
        include_str!("fixtures/astra-reshade-unreal/v0.2.3.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.4",
        include_str!("fixtures/astra-reshade-unreal/v0.2.4.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.5",
        include_str!("fixtures/astra-reshade-unreal/v0.2.5.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.6",
        include_str!("fixtures/astra-reshade-unreal/v0.2.6.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.7",
        include_str!("fixtures/astra-reshade-unreal/v0.2.7.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.8",
        include_str!("fixtures/astra-reshade-unreal/v0.2.8.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.9",
        include_str!("fixtures/astra-reshade-unreal/v0.2.9.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.10",
        include_str!("fixtures/astra-reshade-unreal/v0.2.10.astra-gi.toml"),
    ),
    (
        "astra-reshade v0.2.11",
        include_str!("fixtures/astra-reshade-unreal/v0.2.11.astra-gi.toml"),
    ),
];

/// One of [`MANIFESTS`] as an installable zip.
struct Fixture {
    name: &'static str,
    zip: Vec<u8>,
    appid: Option<u32>,
    /// `[target] exe`.
    exe: Option<String>,
    /// Each game file that lands directly beside the program: its name, and our bytes.
    beside: Vec<(String, Vec<u8>)>,
    /// The Doorstop options its launch passes (`--doorstop-…`); empty when it starts no Doorstop.
    doorstop: Vec<String>,
}

/// The names a loader proxy goes by (src/plan.rs `LOADER_PROXIES`).
const PROXIES: &[&str] = &["winhttp.dll", "version.dll", "dxgi.dll", "d3d11.dll"];
/// The names left alone when present and not ours (src/plan.rs `KEEP_IF_PRESENT`).
const KEEP: &[&str] = &["doorstop_config.ini", "reshade.ini"];
/// The files the Unreal runner 0.2.x left beside a game's program (src/former.rs).
const FORMER_NAMES: &[&str] = &["dwmapi.dll", "dxgi.dll", "astra.addon64"];
/// The names planning treats as a mod loader's (src/plan.rs `HIJACK_NAMES`; Astra's own
/// manifests place three of them: `winhttp.dll`, `dwmapi.dll`, `dxgi.dll`).
const HIJACK: &[&str] = &[
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
    "winmm.dll",
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
    "opengl32.dll",
];
/// How many names of a proxy's folder the ledger keeps (src/plan.rs `PROXY_FOLDER_NAMES`).
const PROXY_FOLDER_NAMES: usize = 5_000;

/// Row `foreign-dwmapi` of `tests/corpus/divergence.txt` (step 10a): where Astra's own
/// manifest `f` places `dwmapi.dll` directly beside the program in `game` and a file that is not
/// ours is there, its name as it is spelled there.
fn foreign_dwmapi(game: &Path, f: &Fixture) -> Option<String> {
    let (leaf, ours) = f
        .beside
        .iter()
        .find(|(leaf, _)| leaf.eq_ignore_ascii_case("dwmapi.dll"))?;
    let at = game.join(leaf);
    if !fs::symlink_metadata(&at).is_ok_and(|m| m.is_file()) {
        return None;
    }
    (fs::read(&at).ok()? != *ours).then(|| leaf.clone())
}

/// Row `foreign-dwmapi` applied to 0.4.1's answer `old` for a game where `foreign` is that
/// foreign `dwmapi.dll`: what this crate answers instead, when the row applies. It applies where
/// 0.4.1 reached the game files — its plan replaces the file, or it refused `foreign-loader` for
/// a later one (`dwmapi.dll` is the first game file of every manifest that places it) — and
/// there this crate refuses `foreign-loader` for `dwmapi.dll`. A refusal before the game files
/// is the same refusal.
fn foreign_dwmapi_row(old: &str, foreign: Option<&str>) -> Option<String> {
    let rel = foreign?;
    if old.starts_with('{') {
        assert!(
            old.contains(&format!("\"rel_path\":\"{rel}\",\"action\":\"replace\"")),
            "foreign-dwmapi: 0.4.1 planned a foreign {rel} as something other than replace: {old}"
        );
    } else if !old.starts_with("foreign-loader ") {
        return None;
    }
    Some(format!("foreign-loader {{\"file\": \"{rel}\"}}"))
}

/// What C1b holds this crate's answer to: 0.4.1's, with row `foreign-dwmapi` applied (counted).
fn want_with_rows(old: &str, foreign: Option<&str>, counts: &Counts) -> String {
    match foreign_dwmapi_row(old, foreign) {
        Some(new) => {
            bump(&counts.c1b_row_dwmapi);
            new
        }
        None => old.to_owned(),
    }
}

/// The names in `dir`, lower case, and whether there were more than [`PROXY_FOLDER_NAMES`]:
/// the folder's listing a ledger keeps for a hijack-named file it adds there (src/plan.rs
/// `proxy_folders`).
fn listing(dir: &Path) -> (Vec<String>, bool) {
    let names = names_in(dir);
    if names.len() > PROXY_FOLDER_NAMES {
        return (Vec::new(), true);
    }
    (names, false)
}

/// Every name in `dir`, lower case, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The last component of a `/`-separated path, in ASCII lower case.
fn leaf(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase()
}

/// A zip of `manifest` and every file it names: each `[[game_files]]` source with bytes made up
/// from its pin (one pin, one content, so a pin shared by two files still matches both) and the
/// pin rewritten to them; each `[[files]]` pattern with a file under it. A loader proxy of a
/// manifest that launches through Doorstop is a Doorstop build (a PE that understands every
/// option the launch passes), as the runners' `winhttp.dll` is, so a Doorstop of someone else's
/// can stand in for it.
fn fixture(name: &'static str, manifest: &str) -> Fixture {
    let table: toml::Table = toml::from_str(manifest).unwrap();
    let mut text = manifest.to_owned();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut beside = Vec::new();
    let doorstop: Vec<String> = table
        .get("launch")
        .and_then(|l| l.get("args"))
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .filter(|a| a.starts_with("--doorstop-"))
        .map(str::to_owned)
        .collect();
    let add = |entries: &mut Vec<(String, Vec<u8>)>, from: &str, bytes: Vec<u8>| {
        let path = match from.strip_suffix("**") {
            Some(dir) => format!("{dir}c1b.bin"),
            None => from.to_owned(),
        };
        if !entries.iter().any(|(p, _)| *p == path) {
            entries.push((path, bytes));
        }
    };
    let list = |key: &str| {
        table
            .get(key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    };
    for g in list("game_files") {
        let from = g["from"].as_str().unwrap();
        let to = g["to"].as_str().unwrap();
        let bytes = match g.get("sha256").and_then(|v| v.as_str()) {
            Some(pin) => {
                let made_up = format!("C1b {pin}").into_bytes();
                let bytes = if PROXIES.contains(&leaf(to).as_str()) && !doorstop.is_empty() {
                    let options: Vec<&str> = doorstop.iter().map(String::as_str).collect();
                    pe_with_options(X64, &options, &made_up)
                } else {
                    made_up
                };
                text = text.replace(pin, &sha(&bytes));
                bytes
            }
            None => format!("C1b {from}").into_bytes(),
        };
        if let Some(leaf) = to.strip_prefix("${game}/")
            && !leaf.contains('/')
            && !from.ends_with("**")
        {
            beside.push((leaf.to_owned(), bytes.clone()));
        }
        add(&mut entries, from, bytes);
    }
    for f in list("files") {
        let from = f["from"].as_str().unwrap();
        add(&mut entries, from, format!("C1b {from}").into_bytes());
    }
    let mut zip_entries = vec![E::File("astra-gi.toml", text.as_bytes())];
    for (p, b) in &entries {
        zip_entries.push(E::File(p, b));
    }
    let appid = table
        .get("target")
        .and_then(|t| t.get("steam_appid"))
        .and_then(|v| v.as_integer())
        .map(|a| a as u32);
    let exe = table
        .get("target")
        .and_then(|t| t.get("exe"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Fixture {
        name,
        zip: zip(&zip_entries),
        appid,
        exe,
        beside,
        doorstop,
    }
}

/// Where a plan runs: the tree, its program, the appid the caller vouches for, the appinfo.
#[derive(Clone, Copy)]
struct Planned<'a> {
    dir: &'a Path,
    exe: &'a Path,
    appid: Option<u32>,
    appinfo: &'a Path,
    profile: &'a Path,
    /// Under Proton (with no prefix known), else native Linux.
    proton: bool,
}

/// One launcher's public install path — `plan_install`, `install`, `uninstall` — in that
/// version's own types, each outcome as text: its JSON, or a refusal's code and params.
macro_rules! launcher {
    ($m:ident, $krate:ident) => {
        // Not every version is asked everything (0.3.1 only installs).
        #[allow(dead_code)]
        mod $m {
            use $krate::{Accept, Ctx, GameTarget, GiPackage, LauncherError, Platform};

            use super::{Planned, sha};

            fn game(p: &Planned) -> GameTarget {
                GameTarget {
                    dir: p.dir.to_path_buf(),
                    exe: p.exe.to_path_buf(),
                    steam_appid: p.appid,
                    platform: if p.proton {
                        Platform::LinuxProton { prefix: None }
                    } else {
                        Platform::LinuxNative
                    },
                    steam_appinfo: Some(p.appinfo.to_path_buf()),
                }
            }

            fn refused(e: LauncherError) -> String {
                format!("{} {:?}", e.code, e.params)
            }

            /// The plan, or its refusal.
            pub fn plan(p: &Planned, zip: &[u8]) -> String {
                let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
                match $krate::plan_install(&pkg, &game(p), p.profile) {
                    Ok(plan) => serde_json::to_string(&plan).unwrap(),
                    Err(e) => refused(e),
                }
            }

            /// [`plan`], and when it plans, that plan installed (the person having accepted an
            /// unread VAC flag): then the ledger the install left on disk, or its refusal.
            pub fn install(p: &Planned, zip: &[u8]) -> (String, Option<String>) {
                let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
                let game = game(p);
                let plan = match $krate::plan_install(&pkg, &game, p.profile) {
                    Ok(plan) => plan,
                    Err(e) => return (refused(e), None),
                };
                let accept = Accept {
                    vac_unchecked: true,
                };
                let done = match $krate::install(&pkg, &plan, &game, accept, &Ctx::silent()) {
                    Ok(_) => {
                        std::fs::read_to_string(p.profile.join($krate::LEDGER_FILE_NAME)).unwrap()
                    }
                    Err(e) => refused(e),
                };
                (serde_json::to_string(&plan).unwrap(), Some(done))
            }

            /// The `UninstallReport`, or the refusal.
            pub fn uninstall(p: &Planned) -> String {
                match $krate::uninstall(p.profile, &game(p)) {
                    Ok(r) => serde_json::to_string(&r).unwrap(),
                    Err(e) => refused(e),
                }
            }
        }
    };
}
// This crate; launcher 0.4.1 (C1's reference); launcher 0.3.1 (what Astra 0.2.7 ships).
launcher!(ours, astra_game_launcher);
launcher!(legacy, launcher_legacy);
launcher!(v031, launcher_031);

fn plan_new(p: &Planned, zip: &[u8]) -> String {
    ours::plan(p, zip)
}

fn plan_legacy(p: &Planned, zip: &[u8]) -> String {
    legacy::plan(p, zip)
}

/// The walk Pass A makes (src/detect.rs `walk`), as `/`-joined paths, and whether it stopped
/// at its cap: breadth first, each folder sorted, links skipped, depth 4 but for a `Win64`
/// inside `Binaries` landing on it. Only for counting what the corpus reaches.
fn walk(root: &Path) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut queue: VecDeque<(PathBuf, Vec<String>)> = VecDeque::from([(root.into(), Vec::new())]);
    while let Some((dir, rel)) = queue.pop_front() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<(String, bool)> = read
            .flatten()
            .filter_map(|e| {
                let ft = e.file_type().ok()?;
                (!ft.is_symlink())
                    .then(|| (e.file_name().to_string_lossy().into_owned(), ft.is_dir()))
            })
            .collect();
        items.sort();
        for (name, is_dir) in items {
            if out.len() >= MAX_ENTRIES {
                return (out, true);
            }
            let mut child = rel.clone();
            child.push(name);
            let boundary = child.len() == MAX_DEPTH
                && child
                    .last()
                    .is_some_and(|n| n.eq_ignore_ascii_case("Win64"))
                && rel
                    .last()
                    .is_some_and(|p| p.eq_ignore_ascii_case("Binaries"));
            if is_dir && (child.len() < MAX_DEPTH || boundary) {
                queue.push_back((dir.join(child.last().unwrap()), child.clone()));
            }
            out.push(child.join("/"));
        }
    }
    (out, false)
}

fn slashed(p: &Path) -> String {
    p.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The real child of `dir` named `name` ignoring ASCII case.
fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Unreal projects (`Binaries/Win64/*-Win64-Shipping.exe` beside `Content/Paks`) at `depth`.
fn paired_projects_at(root: &Path, depth: usize) -> usize {
    let mut level = vec![root.to_path_buf()];
    for _ in 0..depth {
        level = level
            .iter()
            .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
    }
    level
        .iter()
        .filter(|p| {
            let shipping = child_ci(p, "Binaries")
                .and_then(|b| child_ci(&b, "Win64"))
                .and_then(|w| fs::read_dir(w).ok())
                .is_some_and(|mut r| {
                    r.any(|e| {
                        e.is_ok_and(|e| {
                            e.file_name()
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .ends_with("-win64-shipping.exe")
                        })
                    })
                });
            let paks = child_ci(p, "Content")
                .and_then(|c| child_ci(&c, "Paks"))
                .is_some_and(|p| p.is_dir());
            shipping && paks
        })
        .count()
}

/// An Unreal answer with a bootstrapper beside its project while another paired project sits
/// at the same depth: the tie Pass A's `best()` breaks by the bootstrapper before the size.
fn bootstrap_tie(root: &Path, d: &Detection) -> bool {
    if d.engine != Engine::Unreal || d.hint_used {
        return false;
    }
    let Some(ev) = &d.evidence else {
        return false;
    };
    let comps: Vec<String> = ev
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    let Some((leaf, parent)) = comps.split_last() else {
        return false;
    };
    let parent_dir: PathBuf = std::iter::once(root.to_path_buf())
        .chain(parent.iter().map(PathBuf::from))
        .collect();
    child_ci(&parent_dir, &format!("{leaf}.exe")).is_some_and(|p| p.is_file())
        && paired_projects_at(root, comps.len()) >= 2
}

fn one_tree(
    seed: u64,
    zips: &[(Engine, Vec<u8>)],
    probes: &BTreeMap<&'static str, Vec<u8>>,
    fixtures: &[Fixture],
    appinfo: &Path,
    counts: &Counts,
    installs: &SyncSender<Job>,
) {
    let tree = corpus::tree(seed);
    let t = Tmp::new(&format!("corpus-{seed:x}"));
    tree.build(t.path());
    let profile = Tmp::new(&format!("corpus-profile-{seed:x}"));
    bump(&counts.trees);
    let (walked, capped) = walk(t.path());
    if capped {
        bump(&counts.capped);
    }
    let tops = walked
        .iter()
        .filter(|p| {
            let lower = p.to_ascii_lowercase();
            !p.contains('/')
                && [".exe", ".x86_64", ".x86"]
                    .iter()
                    .any(|e| lower.ends_with(e))
        })
        .count();
    if tops >= 9 {
        bump(&counts.crowded);
    }
    if tree.nodes.iter().any(|n| match n {
        Node::Many(_, k) | Node::Fill(_, k) => *k > MAX_ENTRIES,
        _ => false,
    }) {
        bump(&counts.huge_listing);
    }
    if tree
        .nodes
        .iter()
        .any(|n| matches!(n, Node::File(p, _) if p == "Pack0/Same.exe"))
    {
        bump(&counts.tied);
    }

    let mut exes: Vec<PathBuf> = tree.programs.iter().map(PathBuf::from).collect();
    let mut seen: Vec<Seen> = Vec::new();
    for hint in &tree.hints {
        let hint = (!hint.is_empty()).then_some(hint.as_str());
        let (d, with) = checked_with_hostile(t.path(), hint);
        bump(&counts.detections);
        let mut lower_seen = None;
        if let Some(exe) = &d.exe
            && !exes.contains(exe)
        {
            exes.push(exe.clone());
        }
        if d.unity_version.is_some() {
            bump(&counts.unity_version);
        }
        if d.engine == Engine::Unreal && d.evidence.is_none() && d.exe.is_some() {
            bump(&counts.weak_shipping);
        }
        if d.engine == Engine::Unreal
            && d.confidence == astra_game_launcher::Confidence::Low
            && d.evidence.is_some()
            && d.exe.is_some()
        {
            bump(&counts.weak_half);
        }
        if d.engine == Engine::Unreal && d.exe.is_none() {
            bump(&counts.paks_only);
        }
        if d.engine == Engine::Godot && d.confidence == astra_game_launcher::Confidence::Low {
            bump(&counts.embedded_pck);
        }
        if !d.engine.is_unknown()
            && let Some(e) = &d.evidence
        {
            let odd = |c: &std::ffi::OsStr| {
                let c = c.to_string_lossy();
                corpus::ODD_NAMES.iter().any(|n| c.starts_with(n))
            };
            if e.iter().any(odd) {
                bump(&counts.odd_named);
            }
            if d.engine == Engine::Unreal
                && e.file_name()
                    .is_some_and(|l| corpus::ODD_NAMES.iter().any(|n| l.to_string_lossy() == *n))
            {
                bump(&counts.odd_project);
            }
        }
        match d.exe_pick {
            Some(ExePick::LaunchConfig) => bump(&counts.pick_launch_config),
            Some(ExePick::Only) => bump(&counts.pick_only),
            Some(ExePick::Largest) => bump(&counts.pick_largest),
            None => {}
        }
        if d.hint_used
            && let Some(exe) = &d.exe
        {
            if exe.iter().count() >= 5 {
                bump(&counts.deep_hint_used);
            }
            if !walked.contains(&slashed(exe)) {
                bump(&counts.hint_past_cap);
            }
        }
        if tree.truncation
            && hint.is_some_and(|h| h.to_ascii_lowercase().starts_with("huge/"))
            && !d.hint_used
        {
            bump(&counts.truncation_hid);
        }
        if hint.is_none() && bootstrap_tie(t.path(), &d) {
            bump(&counts.bootstrap_tie);
        }
        if open_to_descriptors(&d) {
            bump(&counts.open);
            if with.descriptor_sha256.is_some() {
                bump(&counts.claimed);
            }
            // C1c with the lower standings alone: still narrow-only.
            let lower = detect_with_engines(t.path(), hint, hostile_lower_set()).unwrap();
            narrowed(&d, &lower);
            if lower.descriptor_sha256.is_some() {
                bump(&counts.claimed_lower);
            }
            lower_seen = Some(lower.clone());
            // Where the moderated claim answers alone, nothing below it changes that answer.
            let moderated = detect_with_engines(t.path(), hint, moderated_hostile_set()).unwrap();
            if moderated.descriptor_sha256.is_some() {
                bump(&counts.moderated_decides);
                assert_eq!(
                    with, moderated,
                    "C1c: a lower standing never replaces nor ambiguates a moderated answer (seed {seed:#x}, hint {hint:?})"
                );
            }
        }
        seen.push(Seen {
            hint: hint.map(str::to_owned),
            pass_a: d,
            with,
            lower: lower_seen,
        });
    }
    let hints = seen.len();
    for (i, s) in seen.into_iter().enumerate() {
        let at = Turn { seed, i, hints };
        planning_canary(at, t.path(), profile.path(), s, &exes, probes, counts);
    }

    let appid = tree.app.map(|a| match a {
        App::Vac => VAC,
        App::Clear => CLEAR,
        App::Unlisted => UNLISTED,
    });
    let profile = profile.join("p");
    for (k, exe) in exes.iter().enumerate() {
        let p = Planned {
            dir: t.path(),
            exe,
            appid,
            appinfo,
            profile: &profile,
            proton: false,
        };
        // One probe per program, turning with the seed: a mismatch carries `got`, and a plan
        // means `got` was the probe's own engine, so one is enough to expose `engine_of_exe`.
        let (engine, zip) = &zips[(seed as usize + k) % zips.len()];
        {
            let new = plan_new(&p, zip);
            let old = plan_legacy(&p, zip);
            assert_eq!(
                new, old,
                "C1: planning must refuse and plan exactly as 0.4.1 (seed {seed:#x}, exe {exe:?}, appid {appid:?}, engine {engine:?})"
            );
            bump(&counts.plans);
            if new.starts_with("anti-cheat ") {
                bump(&counts.anti_cheat);
                if new.contains("valve-anti-cheat") {
                    bump(&counts.vac);
                }
            }
            if new.contains("\"vac-unchecked\"") {
                bump(&counts.vac_unchecked);
            }
            if new.starts_with("engine-mismatch ") {
                bump(&counts.mismatch);
            }
            let seen = engine_seen(&new, *engine);
            let parts: Vec<String> = exe
                .iter()
                .map(|c| c.to_string_lossy().to_ascii_lowercase())
                .collect();
            let name = parts.last().map_or("", String::as_str);
            let in_binaries = parts.len() >= 4
                && parts[parts.len() - 3] == "binaries"
                && parts[parts.len() - 2] == "win64";
            if seen.as_deref() == Some("unreal")
                && name.ends_with("-win64-shipping.exe")
                && !in_binaries
            {
                bump(&counts.plan_shipping_anywhere);
            }
            if seen.as_deref() == Some("godot")
                && parts.len() >= 2
                && !t.path().join(exe).with_extension("pck").exists()
                && fs::read(t.path().join(exe)).is_ok_and(|b| b.len() >= 12 && b.ends_with(b"GDPC"))
            {
                bump(&counts.plan_embedded_below);
            }
        }
        c1b_plan(seed, k, &p, fixtures, counts, installs);
    }
}

/// Which detection of which tree the planning canary is asked about: the tree's seed, the
/// hint's index, and how many hints the tree has.
#[derive(Clone, Copy)]
struct Turn {
    seed: u64,
    i: usize,
    hints: usize,
}

/// One detection of a tree, kept for the planning canary ([`planning_canary`]).
struct Seen {
    hint: Option<String>,
    /// Pass A's answer (C1), the hostile set's (C1c), and its lower standings' alone where
    /// Pass A left the game open.
    pass_a: Detection,
    with: Detection,
    lower: Option<Detection>,
}

/// Whether the anchor of the hostile descriptor `code` (`tests/common/identity.rs`) sits beside
/// the program `exe` below `dir`, ignoring ASCII case — the canary's own reading, so a planning
/// that names a program its descriptor never matched is seen as such. `None` for a descriptor
/// with no anchor (`hostile-all`, first-party).
fn anchored(code: &str, dir: &Path, exe: &Path) -> Option<bool> {
    let program = dir.join(exe);
    let folder = program.parent()?;
    let name = program.file_name()?.to_string_lossy().into_owned();
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
    let beside = |want: &str| {
        fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name().to_string_lossy().eq_ignore_ascii_case(want)
                    && fs::metadata(e.path()).is_ok_and(|m| m.is_file())
            })
    };
    match code {
        "hostile-unity" => Some(beside("UnityPlayer.dll") || beside("UnityPlayer.so")),
        "hostile-paks" => Some(beside("Game.pdb")),
        "hostile-pck" => Some(beside(&format!("{stem}.pck"))),
        _ => None,
    }
}

/// The planning canary (what docs/ENGINE-DESCRIPTORS-DEBT.md's row 10 owed, closed by L3-4),
/// for one detection of a tree: planning (`plan_install_in`, community) asks the descriptors as
/// Pass B does, with the program Steam launches for the trusted appid — the tree's hint,
/// written into that appid's appinfo — as the hint.
///
/// - Where Pass B names the game with a program (the whole hostile set, and its lower standings
///   alone), planning that program as that code plans, and pins the very descriptor Pass B
///   reports (`Detection::descriptor_sha256`).
/// - Where Pass A closed the folder, no program plans as any descriptor's code: the engine
///   check refuses it, with a built-in `got` (each program under one of the tree's hints).
/// - Where Pass A left it open, a program the lower standings name as a descriptor's code has
///   that descriptor's anchor beside it ([`anchored`]): a descriptor names only a program it
///   matched, whatever it named the game with.
fn planning_canary(
    turn: Turn,
    dir: &Path,
    steam: &Path,
    seen: Seen,
    exes: &[PathBuf],
    probes: &BTreeMap<&'static str, Vec<u8>>,
    counts: &Counts,
) {
    let Turn { seed, i, hints } = turn;
    // The hint as planning reads it: Steam's launch entry for the trusted appid.
    let appinfo = steam.join(format!("planning-{i}.vdf"));
    let (appid, hint) = match &seen.hint {
        None => (None, None),
        Some(h) => {
            fs::write(&appinfo, appinfo_launch(&[(HINTED, h, "windows")])).unwrap();
            (Some(HINTED), launch_executable(&appinfo, HINTED, "windows"))
        }
    };
    if hint.is_some() {
        bump(&counts.planning_hinted);
    }
    let Seen {
        pass_a,
        with,
        lower,
        ..
    } = if hint == seen.hint {
        seen
    } else {
        // Steam's spelling of the hint (normalised): the detections the daemon would make.
        let h = hint.as_deref();
        let pass_a = detect_with_engines(dir, h, &EngineSet::empty()).unwrap();
        let with = detect_with_engines(dir, h, hostile_set()).unwrap();
        let lower = open_to_descriptors(&pass_a)
            .then(|| detect_with_engines(dir, h, hostile_lower_set()).unwrap());
        Seen {
            hint: hint.clone(),
            pass_a,
            with,
            lower,
        }
    };
    let profile = steam.join("planning-profile");
    let plan = |exe: &Path, code: &str, set: &EngineSet| -> Result<InstallPlan, LauncherError> {
        let zip = &probes[code];
        let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
        let game = GameTarget {
            dir: dir.to_path_buf(),
            exe: exe.to_path_buf(),
            steam_appid: appid,
            platform: Platform::LinuxProton { prefix: None },
            steam_appinfo: Some(appinfo.clone()),
        };
        plan_install_in(&pkg, &game, &profile, set, Provenance::Community, None)
    };
    let at = |exe: &Path| format!("seed {seed:#x}, hint {hint:?}, exe {exe:?}");

    // Pass B named the game with a program: planning it as that code pins Pass B's descriptor.
    for (named, set, pinned) in [
        (Some(&with), hostile_set(), &counts.planning_pinned),
        (
            lower.as_ref(),
            hostile_lower_set(),
            &counts.planning_pinned_lower,
        ),
    ] {
        let Some(named) = named else { continue };
        let (Some(sha256), Some(exe)) = (&named.descriptor_sha256, &named.exe) else {
            continue;
        };
        match plan(exe, named.engine.as_str(), set) {
            Ok(p) => {
                assert_eq!(
                    p.engine_descriptor.as_ref().map(|d| &d.sha256),
                    Some(sha256),
                    "planning canary: planning pins the descriptor Pass B named the game with ({})",
                    at(exe)
                );
                bump(pinned);
            }
            Err(e) => assert_eq!(
                e.code,
                codes::ANTI_CHEAT,
                "planning canary: Pass B named the game {} with this program, so planning it as that is no {e:?} ({})",
                named.engine,
                at(exe)
            ),
        }
    }

    if !open_to_descriptors(&pass_a) {
        // Each program under one of the tree's hints, turning: Pass A's walk per plan is what
        // this costs.
        for (k, exe) in exes
            .iter()
            .enumerate()
            .filter(|(k, _)| (k + i) % hints == 0)
        {
            let code = HOSTILE_CODES[(seed as usize + i + k) % HOSTILE_CODES.len()];
            match plan(exe, code, hostile_set()) {
                Ok(_) => panic!(
                    "planning canary: Pass A closed the folder, yet a program plans as {code} ({})",
                    at(exe)
                ),
                Err(e) if e.code == codes::ENGINE_MISMATCH => {
                    let got = e.param("got").unwrap_or_default();
                    assert!(
                        Engine::from_code(got).is_some(),
                        "planning canary: Pass A closed the folder, yet planning names a program {got} ({})",
                        at(exe)
                    );
                    bump(&counts.planning_closed);
                }
                Err(e) => assert!(
                    ![codes::ENGINE_AMBIGUOUS, codes::DESCRIPTOR_MISMATCH].contains(&e.code),
                    "planning canary: Pass A closed the folder, yet descriptors were asked: {e:?} ({})",
                    at(exe)
                ),
            }
        }
        return;
    }
    let Some(lower) = lower else { return };
    let claim = lower
        .descriptor_sha256
        .is_some()
        .then_some(lower.engine.as_str());
    for (k, exe) in exes.iter().enumerate() {
        let code = LOWER_CODES[(seed as usize + i + k) % LOWER_CODES.len()];
        let named = match plan(exe, code, hostile_lower_set()) {
            Ok(_) => code.to_owned(),
            Err(e) if e.code == codes::ENGINE_MISMATCH => {
                e.param("got").unwrap_or_default().to_owned()
            }
            Err(_) => continue,
        };
        if Engine::from_code(&named).is_none() {
            assert_ne!(
                anchored(&named, dir, exe),
                Some(false),
                "planning canary: planning names a program {named} without that descriptor's anchor beside it ({})",
                at(exe)
            );
            bump(&counts.planning_named);
        } else if named == "unknown"
            && let Some(claim) = claim
        {
            bump(&counts.planning_unclaimed);
            if anchored(claim, dir, exe) == Some(false) {
                bump(&counts.planning_unclaimed_anchorless);
            }
        }
    }
}

/// [`plan_new`], with `set` (first-party, no pick): C1c for planning.
fn plan_new_in(p: &Planned, zip: &[u8], set: &EngineSet) -> String {
    let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
    let game = GameTarget {
        dir: p.dir.to_path_buf(),
        exe: p.exe.to_path_buf(),
        steam_appid: p.appid,
        platform: if p.proton {
            Platform::LinuxProton { prefix: None }
        } else {
            Platform::LinuxNative
        },
        steam_appinfo: Some(p.appinfo.to_path_buf()),
    };
    match plan_install_in(&pkg, &game, p.profile, set, Provenance::FirstParty, None) {
        Ok(plan) => serde_json::to_string(&plan).unwrap(),
        Err(e) => format!("{} {:?}", e.code, e.params),
    }
}

/// The engine planning's engine check saw for the probe `probe`: its own when it planned, `got`
/// when it refused as a mismatch, nothing when it refused before the check.
fn engine_seen(plan: &str, probe: Engine) -> Option<String> {
    if plan.starts_with('{') {
        return Some(probe.code().to_owned());
    }
    let params = plan.strip_prefix("engine-mismatch ")?;
    let got = params.split("\"got\": \"").nth(1)?;
    Some(got.split('"').next()?.to_owned())
}

/// C1b for one program: one of Astra's own manifests, turning with the seed, its game files laid
/// beside the program now and then (ours, or someone else's), planned by both launchers; and a
/// share of those that plan sent on to `installs` ([`c1b_install`]).
fn c1b_plan(
    seed: u64,
    k: usize,
    p: &Planned,
    fixtures: &[Fixture],
    counts: &Counts,
    installs: &SyncSender<Job>,
) {
    let turn = (seed as usize).wrapping_mul(31).wrapping_add(k * 7);
    let fixture = turn % fixtures.len();
    let f = &fixtures[fixture];
    let program = p.dir.join(p.exe);
    let game = program.parent().unwrap();
    let real = |q: &Path, dir: bool| {
        fs::symlink_metadata(q).is_ok_and(|m| if dir { m.is_dir() } else { m.is_file() })
    };
    if real(&program, false) && real(game, true) {
        for (i, (leaf, ours)) in f.beside.iter().enumerate() {
            let at = game.join(leaf);
            if fs::symlink_metadata(&at).is_err() {
                match (turn + i) % 5 {
                    0 => write(&at, ours),
                    1 => write(&at, b"MZ someone else's"),
                    _ => {}
                }
            }
        }
    }
    let p = Planned {
        appid: match f.appid {
            Some(a) if turn.is_multiple_of(2) => Some(a),
            _ => p.appid,
        },
        proton: true,
        ..*p
    };
    let foreign = foreign_dwmapi(game, f);
    let new = plan_new(&p, &f.zip);
    let old = plan_legacy(&p, &f.zip);
    assert_eq!(
        new,
        want_with_rows(&old, foreign.as_deref(), counts),
        "C1b: Astra's own manifest {} plans and refuses exactly as with 0.4.1, but for the declared rows (seed {seed:#x}, exe {:?}, appid {:?})",
        f.name,
        p.exe,
        p.appid
    );
    bump(&counts.c1b_plans);
    // C1c for planning: no engine set changes a plan of Astra's own (a built-in engine). One
    // hostile set per plan, turning, so each is passed in over a third of the corpus.
    let sets = [hostile_set(), hostile_lower_set(), moderated_hostile_set()];
    assert_eq!(
        plan_new_in(&p, &f.zip, sets[turn % sets.len()]),
        new,
        "C1c: Astra's own manifest {} plans alike whatever engine set planning is given (seed {seed:#x}, exe {:?}, set {})",
        f.name,
        p.exe,
        turn % sets.len()
    );
    bump(&counts.c1b_hostile);
    tally(&new, &counts.c1b_actions, &counts.c1b_refusals);
    if new.starts_with('{') {
        bump(&counts.c1b_planned);
        // A share of what plans, by the turn: installed, changed, planned again and uninstalled.
        let h = mix(turn);
        if h.is_multiple_of(INSTALL_SHARE) {
            let past = Past {
                change: CHANGES[(h >> 3) % CHANGES.len()],
                cross: (h >> 6).is_multiple_of(CROSS_SHARE),
                update: (h >> 9).is_multiple_of(2),
                pick: h >> 12,
            };
            let at = format!("seed {seed:#x}");
            installs.send(Job::copy(at, &p, fixture, past)).unwrap();
        }
    }
}

/// The actions a plan holds, or its refusal's code, added to `actions` or `refusals`.
fn tally(
    plan: &str,
    actions: &Mutex<BTreeMap<String, usize>>,
    refusals: &Mutex<BTreeMap<String, usize>>,
) {
    if plan.starts_with('{') {
        let mut actions = actions.lock().unwrap();
        for a in ACTIONS {
            *actions.entry((*a).to_owned()).or_default() +=
                plan.matches(&format!("\"action\":\"{a}\"")).count();
        }
    } else {
        let code = plan.split(' ').next().unwrap_or("").to_owned();
        *refusals.lock().unwrap().entry(code).or_default() += 1;
    }
}

const ACTIONS: &[&str] = &[
    "add",
    "replace",
    "reuse-known-proxy",
    "keep-existing",
    "share-loader",
];

/// One in this many of the programs C1b plans goes on to [`c1b_install`]; one in [`CROSS_SHARE`]
/// of those is installed by launcher 0.3.1 too.
const INSTALL_SHARE: usize = 2;
const CROSS_SHARE: usize = 2;

/// The turn, scattered: its bits choose independently of the `turn % n` C1b's plan already used.
fn mix(turn: usize) -> usize {
    ((turn as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 17) as usize
}

/// What a game goes through between C1b's install and its plan from the ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// Nothing: the plan is an update over our own install.
    Nothing,
    /// A Doorstop of someone else's laid over our proxy, one that does everything ours does (a
    /// BepInEx installed by hand after us): `share-loader`. The game's program is an x64 PE from
    /// the start, so a proxy can serve it.
    TheirDoorstop,
    /// Our `doorstop_config.ini` or `ReShade.ini` rewritten (by a mod manager, by ReShade
    /// itself): `keep-existing`.
    Rewritten,
    /// Another file of ours changed since we placed it (a game update, the player).
    OursChanged,
    /// Files named as the Unreal runner 0.2.x left them, laid beside the program where nothing
    /// is: look-alikes, since the shipped bytes are not committed.
    OlderAstra,
}

const CHANGES: [Change; 5] = [
    Change::Nothing,
    Change::TheirDoorstop,
    Change::Rewritten,
    Change::OursChanged,
    Change::OlderAstra,
];

/// The turn's choices for one [`c1b_install`].
#[derive(Clone, Copy)]
struct Past {
    change: Change,
    /// Also installed by launcher 0.3.1, and planned again by this crate.
    cross: bool,
    /// The plan from the ledger is installed too, as the update, before the uninstall.
    update: bool,
    /// Picks among the files a change could touch.
    pick: usize,
}

/// A copy of a game folder with a profile beside it (never inside it), for one launcher.
struct GameCopy {
    tmp: Tmp,
    game: PathBuf,
    profile: PathBuf,
}

impl GameCopy {
    fn of(dir: &Path, name: &str) -> GameCopy {
        let tmp = Tmp::new(name);
        let game = tmp.join("game");
        copy_tree(dir, &game);
        let profile = tmp.join("profile").join("p");
        GameCopy { tmp, game, profile }
    }

    /// `p`, on this copy.
    fn at<'a>(&'a self, p: &Planned<'a>) -> Planned<'a> {
        Planned {
            dir: &self.game,
            profile: &self.profile,
            ..*p
        }
    }

    /// `text` with this copy's paths written relative to it.
    fn norm(&self, text: &str) -> String {
        text.replace(self.tmp.path().to_str().unwrap(), "<copy>")
    }

    /// A ledger on disk, as text, read with launcher 0.4.1's `Ledger` (every field it has), its
    /// paths relative to this copy.
    fn ledger(&self, text: &str) -> launcher_legacy::Ledger {
        serde_json::from_str(&self.norm(text)).unwrap()
    }
}

/// The program at `path`, when it is a file and no x64 PE, given an x64 PE's headers before its
/// own bytes (which keep their end: what a probe reads there is unchanged).
fn as_x64_pe(path: &Path) {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return;
    }
    let bytes = fs::read(path).unwrap();
    let mut pe = pe_with_options(X64, &[], b"");
    pe.extend_from_slice(&bytes);
    fs::write(path, pe).unwrap();
}

/// Makes `change` to the game whose program is in `root`, installed as `ledger` records: the
/// files it wrote, below `root`.
fn make(
    change: Change,
    root: &Path,
    ledger: &launcher_legacy::Ledger,
    f: &Fixture,
    pick: usize,
) -> Vec<(String, Vec<u8>)> {
    use launcher_legacy::GameAction as A;
    // Ours, on disk with our bytes since the install placed or found them.
    let ours: Vec<&launcher_legacy::LedgerGameFile> = ledger
        .game_files
        .iter()
        .filter(|g| matches!(g.action, A::Add | A::Replace | A::ReuseKnownProxy))
        .collect();
    let one = |of: Vec<&launcher_legacy::LedgerGameFile>| {
        (!of.is_empty()).then(|| of[pick % of.len()].rel.clone())
    };
    let laid: Vec<(String, Vec<u8>)> = match change {
        Change::Nothing => Vec::new(),
        Change::TheirDoorstop => {
            let options: Vec<&str> = if f.doorstop.is_empty() {
                DOORSTOP_V4.to_vec()
            } else {
                f.doorstop.iter().map(String::as_str).collect()
            };
            let theirs = pe_with_options(X64, &options, b"theirs: a BepInEx installed by hand");
            one(ours
                .iter()
                .copied()
                .filter(|g| PROXIES.contains(&leaf(&g.rel).as_str()))
                .collect())
            .map(|rel| (rel, theirs))
            .into_iter()
            .collect()
        }
        Change::Rewritten => one(ours
            .iter()
            .copied()
            .filter(|g| KEEP.contains(&leaf(&g.rel).as_str()))
            .collect())
        .map(|rel| {
            (
                rel,
                b"[General]\nenabled = false\n; rewritten since\n".to_vec(),
            )
        })
        .into_iter()
        .collect(),
        Change::OursChanged => {
            let mine: Vec<_> = ours
                .iter()
                .copied()
                .filter(|g| !KEEP.contains(&leaf(&g.rel).as_str()))
                .collect();
            // Every other time, one we replaced the game's with, where there is one: its backup
            // is what an uninstall must not put back over the change.
            let replaced: Vec<_> = mine
                .iter()
                .copied()
                .filter(|g| g.action == A::Replace)
                .collect();
            let from = if (pick >> 4).is_multiple_of(2) && !replaced.is_empty() {
                replaced
            } else {
                mine
            };
            one(from)
                .map(|rel| (rel, b"MZ changed since we placed it".to_vec()))
                .into_iter()
                .collect()
        }
        Change::OlderAstra => FORMER_NAMES
            .iter()
            .filter(|n| {
                !ledger
                    .game_files
                    .iter()
                    .any(|g| g.rel.eq_ignore_ascii_case(n))
                    && fs::symlink_metadata(root.join(n)).is_err()
            })
            .map(|n| {
                (
                    (*n).to_owned(),
                    format!("MZ like {n} of an older Astra").into_bytes(),
                )
            })
            .collect(),
    };
    for (rel, bytes) in &laid {
        write(&root.join(rel), bytes);
    }
    laid
}

/// The two trees, file for file and byte for byte, relative to each copy — but for `extra`, files
/// only `b` holds (row `proxy-folder-gained-files`: what this crate's uninstall left).
fn same_tree_but(a: &GameCopy, b: &GameCopy, extra: &[String], at: &str) {
    let (sa, mut sb) = (snapshot(a.tmp.path()), snapshot(b.tmp.path()));
    for rel in extra {
        assert!(
            !sa.contains_key(rel) && sb.remove(rel).is_some(),
            "C1b: {rel} is left by this crate's uninstall alone ({at})"
        );
    }
    if sa != sb {
        let only = |x: &BTreeMap<String, Vec<u8>>, y: &BTreeMap<String, Vec<u8>>| {
            x.iter()
                .filter(|(k, v)| y.get(*k) != Some(v))
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>()
        };
        panic!(
            "C1b: the trees the uninstalls left differ ({at}): 0.4.1 has {:?}, this crate {:?}",
            only(&sa, &sb),
            only(&sb, &sa)
        );
    }
}

/// That the ledger this crate wrote (`done`, its text) keeps, for every hijack-named file it
/// added beside the program, the listing the test took there before the install (`before`).
fn proxy_folders_are(done: &str, before: &(Vec<String>, bool), at: &str) {
    let l: astra_game_launcher::Ledger = serde_json::from_str(done).unwrap();
    let added: Vec<&str> = l
        .game_files
        .iter()
        .filter(|g| g.action == GameAction::Add && HIJACK.contains(&leaf(&g.rel).as_str()))
        .map(|g| g.rel.as_str())
        .collect();
    let got: Vec<(&str, Vec<String>, bool)> = l
        .proxy_folders
        .iter()
        .map(|b| {
            let mut names: Vec<String> = b.names.iter().map(|n| n.to_ascii_lowercase()).collect();
            names.sort();
            (b.proxy.as_str(), names, b.capped)
        })
        .collect();
    let want: Vec<(&str, Vec<String>, bool)> = added
        .iter()
        .map(|rel| (*rel, before.0.clone(), before.1))
        .collect();
    assert_eq!(got, want, "C1b: the proxy folders the ledger keeps ({at})");
}

/// Row `proxy-folder-gained-files` of `tests/corpus/divergence.txt` applied to 0.4.1's uninstall
/// report `gone`: every file `ledger` added beside the program by a hijack name that 0.4.1
/// removes is left instead, with the reason, when the folder (`now`, before the uninstall) holds
/// a name the baseline (`before`) does not and `ledger` does not place, or the baseline is
/// capped. Returns what this crate reports, and those files.
fn proxy_folder_row(
    gone: &str,
    ledger: &astra_game_launcher::Ledger,
    before: &(Vec<String>, bool),
    now: &[String],
) -> (String, Vec<String>) {
    let Ok(mut r) = serde_json::from_str::<astra_game_launcher::UninstallReport>(gone) else {
        return (gone.to_owned(), Vec::new());
    };
    assert!(ledger.proton.is_empty(), "C1b never launches");
    let placed: Vec<String> = ledger
        .game_files
        .iter()
        .map(|g| g.rel.split('/').next().unwrap_or("").to_ascii_lowercase())
        .collect();
    let gained = before.1
        || now
            .iter()
            .any(|n| !before.0.contains(n) && !placed.contains(n));
    let moved: Vec<String> = ledger
        .game_files
        .iter()
        .filter(|g| {
            g.action == GameAction::Add
                && !g.rel.contains('/')
                && HIJACK.contains(&leaf(&g.rel).as_str())
                && r.removed.contains(&g.rel)
        })
        .filter(|_| gained)
        .map(|g| g.rel.clone())
        .collect();
    if moved.is_empty() {
        return (gone.to_owned(), moved);
    }
    r.removed.retain(|rel| !moved.contains(rel));
    let kept: Vec<String> = ledger
        .game_files
        .iter()
        .rev()
        .map(|g| g.rel.clone())
        .filter(|rel| r.kept.contains(rel) || moved.contains(rel))
        .collect();
    r.kept = kept;
    for rel in &moved {
        r.kept_reasons
            .insert(rel.clone(), KEPT_FOLDER_GAINED_FILES.to_owned());
    }
    (serde_json::to_string(&r).unwrap(), moved)
}

/// Row `restore-partial` of `tests/corpus/divergence.txt` applied to the uninstall report `want`
/// (0.4.1's, the rows before it applied): where the uninstall leaves a game file as it is — one
/// `kept` names — this crate's report also warns `restore-partial`. (A file we replaced that is
/// gone warns too, and `kept` cannot name it: C1b removes none of ours, so it never meets one.)
/// Returns the report and whether the row applied.
fn restore_partial_row(want: &str) -> (String, bool) {
    match serde_json::from_str::<astra_game_launcher::UninstallReport>(want) {
        Ok(mut r) if !r.kept.is_empty() => {
            r.warnings = vec![codes::RESTORE_PARTIAL.to_owned()];
            (serde_json::to_string(&r).unwrap(), true)
        }
        _ => (want.to_owned(), false),
    }
}

/// One [`c1b_install`]: the program and the appid C1b planned it with, the fixture, the turn's
/// choices, and the copies of its tree as it was then (the corpus tree changes afterwards).
struct Job {
    at: String,
    exe: PathBuf,
    appid: Option<u32>,
    fixture: usize,
    past: Past,
    /// For 0.4.1, this crate, and (with `past.cross`) 0.3.1.
    a: GameCopy,
    b: GameCopy,
    c: Option<GameCopy>,
}

impl Job {
    fn copy(at: String, p: &Planned, fixture: usize, past: Past) -> Job {
        let copy = |name| {
            let copy = GameCopy::of(p.dir, name);
            if past.change == Change::TheirDoorstop {
                as_x64_pe(&copy.game.join(p.exe));
            }
            copy
        };
        Job {
            at,
            exe: p.exe.to_path_buf(),
            appid: p.appid,
            fixture,
            past,
            a: copy("c1b-041"),
            b: copy("c1b-ours"),
            c: past.cross.then(|| copy("c1b-031")),
        }
    }
}

/// How many threads run [`c1b_install`]s, apart from the corpus's: an install mostly waits for
/// the disk (every file it writes is synced), and many such waits overlap.
const POOL: usize = 96;

/// The [`Job`]s waiting for [`c1b_install`], and the first of them that failed.
struct Pool {
    jobs: Mutex<Receiver<Job>>,
    failed: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

impl Pool {
    fn new() -> (SyncSender<Job>, Pool) {
        let (send, jobs) = sync_channel(POOL * 2);
        let failed = Mutex::new(None);
        let jobs = Mutex::new(jobs);
        (send, Pool { jobs, failed })
    }

    /// Runs every job on [`POOL`] threads of `s`, until the senders are gone. Once one has
    /// failed, the rest are only taken off the queue: a sender must never wait on a pool whose
    /// threads all died.
    fn run<'scope>(
        &'scope self,
        s: &'scope std::thread::Scope<'scope, '_>,
        fixtures: &'scope [Fixture],
        appinfo: &'scope Path,
        counts: &'scope Counts,
    ) {
        for _ in 0..POOL {
            s.spawn(move || {
                loop {
                    let job = self.jobs.lock().unwrap().recv();
                    let Ok(job) = job else { break };
                    if self.failed.lock().unwrap().is_some() {
                        continue;
                    }
                    let run = std::panic::AssertUnwindSafe(|| {
                        c1b_install(job, fixtures, appinfo, counts);
                    });
                    if let Err(failure) = std::panic::catch_unwind(run) {
                        self.failed.lock().unwrap().get_or_insert(failure);
                    }
                }
            });
        }
    }

    /// Fails as the first failed job did, if one did.
    fn finish(self) {
        if let Some(failure) = self.failed.into_inner().unwrap() {
            std::panic::resume_unwind(failure);
        }
    }
}

/// C1b past the fresh game, for one program both launchers planned on the corpus tree: installed
/// by 0.4.1 and by this crate into copies of the tree, `past.change` made to both, planned again
/// from the ledgers (and, now and then, that plan installed as the update), uninstalled; the
/// ledgers, plans, refusals, reports and the trees left equal throughout. With `past.cross`, a
/// third copy installed by launcher 0.3.1, changed alike and planned again by this crate, plans
/// as 0.4.1 does over its own install.
fn c1b_install(job: Job, fixtures: &[Fixture], appinfo: &Path, counts: &Counts) {
    let Job {
        at,
        exe,
        appid,
        fixture,
        past,
        a,
        b,
        c,
    } = job;
    let f = &fixtures[fixture];
    let at = format!("{at}, exe {exe:?}, {}, {:?}", f.name, past.change);
    let empty = PathBuf::new();
    let p = &Planned {
        dir: &empty,
        exe: &exe,
        appid,
        appinfo,
        profile: &empty,
        proton: true,
    };
    let (pa, pb) = (a.at(p), b.at(p));
    let root = |copy: &GameCopy| copy.game.join(p.exe).parent().unwrap().to_path_buf();
    // What sits beside the program before the install: the baseline a file we add there keeps.
    let before = listing(&root(&b));

    let foreign = foreign_dwmapi(&root(&b), f);
    let (plan_a, done_a) = legacy::install(&pa, &f.zip);
    let (plan_b, done_b) = ours::install(&pb, &f.zip);
    assert_eq!(
        b.norm(&plan_b),
        a.norm(&want_with_rows(&plan_a, foreign.as_deref(), counts)),
        "C1b: the install's plan ({at})"
    );
    if foreign_dwmapi_row(&plan_a, foreign.as_deref()).is_some() {
        bump(&counts.c1b_install_refused);
        return;
    }
    let (Some(done_a), Some(done_b)) = (done_a, done_b) else {
        bump(&counts.c1b_install_refused);
        return;
    };
    if !done_a.starts_with('{') || !done_b.starts_with('{') {
        assert_eq!(
            b.norm(&done_b),
            a.norm(&done_a),
            "C1b: the install's refusal ({at})"
        );
        bump(&counts.c1b_install_refused);
        return;
    }
    let ledger = a.ledger(&done_a);
    assert_eq!(
        b.ledger(&done_b),
        ledger,
        "C1b: the ledger the install wrote, on every field 0.4.1's has ({at})"
    );
    proxy_folders_are(&done_b, &before, &at);
    bump(&counts.c1b_installs);
    *counts
        .c1b_installed
        .lock()
        .unwrap()
        .entry(f.name.to_owned())
        .or_default() += 1;
    if let Some(c) = &c {
        let (_, done) = v031::install(&c.at(p), &f.zip);
        let done = done.unwrap_or_default();
        assert!(
            done.starts_with('{'),
            "C1b: launcher 0.3.1 installs where 0.4.1 did ({at}): {done}"
        );
        let v031 = |copy: &GameCopy, text: &str| -> launcher_031::Ledger {
            serde_json::from_str(&copy.norm(text)).unwrap()
        };
        assert_eq!(
            v031(c, &done),
            v031(&a, &done_a),
            "C1b: launcher 0.3.1's ledger is 0.4.1's, on every field 0.3.1's has ({at})"
        );
    }

    let laid = make(past.change, &root(&a), &ledger, f, past.pick);
    for copy in [Some(&b), c.as_ref()].into_iter().flatten() {
        assert_eq!(make(past.change, &root(copy), &ledger, f, past.pick), laid);
    }
    let kind = match (past.change, laid.is_empty()) {
        (Change::Nothing, _) | (_, true) => Change::Nothing,
        (change, false) => change,
    };
    *counts
        .c1b_changes
        .lock()
        .unwrap()
        .entry(format!("{kind:?}"))
        .or_default() += 1;
    if kind == Change::OursChanged
        && ledger.game_files.iter().any(|g| {
            g.action == launcher_legacy::GameAction::Replace
                && laid.iter().any(|(rel, _)| *rel == g.rel)
        })
    {
        bump(&counts.c1b_changed_replaced);
    }

    // From the ledger: 0.4.1 over its own install, this crate over its own and over 0.3.1's.
    // Where row `foreign-dwmapi` applies (a file of ours there changed), this crate refuses, and
    // neither launcher installs the update.
    let foreign = foreign_dwmapi(&root(&b), f);
    let row = foreign_dwmapi_row(&legacy::plan(&pa, &f.zip), foreign.as_deref()).is_some();
    let (re_a, update_a, re_b, update_b) = if past.update && !row {
        let (ra, ua) = legacy::install(&pa, &f.zip);
        let (rb, ub) = ours::install(&pb, &f.zip);
        (ra, ua, rb, ub)
    } else {
        (
            legacy::plan(&pa, &f.zip),
            None,
            ours::plan(&pb, &f.zip),
            None,
        )
    };
    if let Some(c) = &c {
        assert_eq!(
            c.norm(&ours::plan(&c.at(p), &f.zip)),
            a.norm(&want_with_rows(
                &re_a,
                foreign_dwmapi(&root(c), f).as_deref(),
                counts
            )),
            "C1b: over the ledger launcher 0.3.1 wrote, this crate plans as 0.4.1 over its own ({at})"
        );
        bump(&counts.c1b_cross);
    }
    assert_eq!(
        b.norm(&re_b),
        a.norm(&want_with_rows(&re_a, foreign.as_deref(), counts)),
        "C1b: the plan from the ledger ({at})"
    );
    bump(&counts.c1b_replans);
    tally(
        &re_b,
        &counts.c1b_replan_actions,
        &counts.c1b_replan_refusals,
    );
    if let (Some(ua), Some(ub)) = (update_a, update_b) {
        if ua.starts_with('{') && ub.starts_with('{') {
            assert_eq!(
                b.ledger(&ub),
                a.ledger(&ua),
                "C1b: the ledger the update wrote ({at})"
            );
        } else {
            assert_eq!(b.norm(&ub), a.norm(&ua), "C1b: the update's refusal ({at})");
        }
        bump(&counts.c1b_updates);
    }

    // The ledger the uninstall reads (the update's, when there was one), and what row
    // `proxy-folder-gained-files` keeps: an added hijack-named file 0.4.1 removes, whose folder
    // holds a name now that the baseline (`before`) does not and the ledger does not place.
    let ledger_b: astra_game_launcher::Ledger = serde_json::from_str(
        &b.norm(&fs::read_to_string(b.profile.join(LEDGER_FILE_NAME)).unwrap()),
    )
    .unwrap();
    let now = names_in(&root(&b));
    let (gone_a, gone_b) = (legacy::uninstall(&pa), ours::uninstall(&pb));
    let (want_b, kept_for_folder) = proxy_folder_row(&gone_a, &ledger_b, &before, &now);
    if !kept_for_folder.is_empty() {
        counts
            .c1b_row_proxy_folder
            .fetch_add(kept_for_folder.len(), Ordering::Relaxed);
    }
    let (want_b, partial) = restore_partial_row(&want_b);
    if partial {
        bump(&counts.c1b_row_restore_partial);
    }
    assert_eq!(
        b.norm(&gone_b),
        a.norm(&want_b),
        "C1b: the uninstall's report ({at})"
    );
    bump(&counts.c1b_uninstalls);
    if let Ok(r) = serde_json::from_str::<launcher_legacy::UninstallReport>(&gone_b) {
        counts.c1b_former_removed.fetch_add(
            r.former_removed.len() + r.not_restored.len(),
            Ordering::Relaxed,
        );
    }
    let rel_root = root(&b)
        .strip_prefix(b.tmp.path())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let extra: Vec<String> = kept_for_folder
        .iter()
        .map(|rel| format!("{rel_root}/{rel}"))
        .collect();
    same_tree_but(&a, &b, &extra, &at);
    if kind == Change::OlderAstra {
        for (rel, bytes) in &laid {
            assert_eq!(
                fs::read(root(&b).join(rel)).ok().as_ref(),
                Some(bytes),
                "C1b: a look-alike of an older Astra's file is the player's, and stays ({at})"
            );
            bump(&counts.c1b_lookalikes_kept);
        }
    }
}

#[test]
fn c1_detection_and_planning_are_launcher_0_4_1s_over_the_corpus() {
    // The hostile sets are the ones `checked` uses; build them once, before the threads race to.
    assert!(!hostile_set().is_empty());
    assert!(!hostile_lower_set().is_empty() && !moderated_hostile_set().is_empty());
    assert!(EngineSet::empty().is_empty());
    let zips: Vec<(Engine, Vec<u8>)> = Engine::ALL
        .iter()
        .filter(|e| **e != Engine::Unknown)
        .map(|e| (*e, probe_zip(e.code())))
        .collect();
    let probes: BTreeMap<&'static str, Vec<u8>> =
        HOSTILE_CODES.iter().map(|c| (*c, probe_zip(c))).collect();
    let fixtures: Vec<Fixture> = MANIFESTS.iter().map(|(n, m)| fixture(n, m)).collect();
    let world = Tmp::new("corpus-appinfo");
    let appinfo = world.join("appinfo.vdf");
    let mut apps = vec![(VAC, Some(true)), (CLEAR, Some(false))];
    apps.extend(
        fixtures
            .iter()
            .filter_map(|f| f.appid)
            .map(|a| (a, Some(false))),
    );
    fs::write(&appinfo, appinfo_vac(&apps)).unwrap();
    assert_eq!(
        [VAC, CLEAR, UNLISTED].map(|id| valve_anti_cheat(&appinfo, id)),
        [Some(true), Some(false), None],
        "the fixture appinfo reads as written"
    );

    let trees: u64 = std::env::var(TREES_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TREES);
    let counts = Counts::default();
    let (installs, pool) = Pool::new();
    std::thread::scope(|s| {
        pool.run(s, &fixtures, &appinfo, &counts);
        for w in 0..THREADS {
            let (zips, probes, fixtures, counts, appinfo) =
                (&zips, &probes, &fixtures, &counts, appinfo.as_path());
            let installs = installs.clone();
            s.spawn(move || {
                let mut i = w;
                while i < trees {
                    one_tree(
                        SEED.wrapping_add(i),
                        zips,
                        probes,
                        fixtures,
                        appinfo,
                        counts,
                        &installs,
                    );
                    i += THREADS;
                }
            });
        }
        drop(installs);
    });
    pool.finish();
    let get = |a: &AtomicUsize| a.load(Ordering::Relaxed);
    let c = &counts;
    let report = [
        ("trees", get(&c.trees)),
        ("detections", get(&c.detections)),
        ("open to descriptors", get(&c.open)),
        ("claimed by the hostile set", get(&c.claimed)),
        ("claimed by its lower standings", get(&c.claimed_lower)),
        ("decided by the moderated claim", get(&c.moderated_decides)),
        ("plans", get(&c.plans)),
        ("anti-cheat refusals", get(&c.anti_cheat)),
        ("VAC refusals", get(&c.vac)),
        ("vac-unchecked", get(&c.vac_unchecked)),
        ("engine-mismatch", get(&c.mismatch)),
        ("walks capped", get(&c.capped)),
        ("listings past 5,000", get(&c.huge_listing)),
        ("hints the truncation hid", get(&c.truncation_hid)),
        ("hints used at depth >= 5", get(&c.deep_hint_used)),
        ("hints used past the cap", get(&c.hint_past_cap)),
        ("unity versions read", get(&c.unity_version)),
        ("exe_pick launch-config", get(&c.pick_launch_config)),
        ("exe_pick only", get(&c.pick_only)),
        ("exe_pick largest", get(&c.pick_largest)),
        ("trees with 5+ ties", get(&c.tied)),
        ("bootstrapper ties", get(&c.bootstrap_tie)),
        ("weak: any shipping program", get(&c.weak_shipping)),
        ("weak: half a pairing", get(&c.weak_half)),
        (
            "plan: a shipping program anywhere",
            get(&c.plan_shipping_anywhere),
        ),
        (
            "plan: a pck embedded below the top",
            get(&c.plan_embedded_below),
        ),
        ("weak: paks, no program", get(&c.paks_only)),
        ("weak: embedded pck", get(&c.embedded_pck)),
        ("odd names named", get(&c.odd_named)),
        ("odd projects chosen", get(&c.odd_project)),
        ("trees with 9+ top programs", get(&c.crowded)),
    ];
    eprintln!("C1 corpus:");
    for (k, v) in &report {
        eprintln!("  {v:7}  {k}");
    }
    // The corpus must reach every path it is for, or its green proves nothing. Each floor is
    // about half what the seed reaches in 20,000 trees (scaled for fewer, never below one), so a
    // generator edit that drops a path turns this red.
    assert_eq!(get(&c.trees), trees as usize);
    for (k, per_20k) in [
        ("detections", 40_000),
        ("open to descriptors", 13_000),
        ("claimed by the hostile set", 11_000),
        ("claimed by its lower standings", 1_800),
        ("decided by the moderated claim", 11_000),
        ("plans", 34_000),
        ("anti-cheat refusals", 8_000),
        ("VAC refusals", 4_500),
        ("vac-unchecked", 1_800),
        ("engine-mismatch", 23_000),
        ("walks capped", 80),
        ("listings past 5,000", 65),
        ("hints the truncation hid", 100),
        ("hints used at depth >= 5", 1_800),
        ("hints used past the cap", 900),
        ("unity versions read", 900),
        ("exe_pick launch-config", 1_500),
        ("exe_pick only", 700),
        ("exe_pick largest", 1_300),
        ("trees with 5+ ties", 380),
        ("bootstrapper ties", 45),
        ("weak: any shipping program", 130),
        ("weak: half a pairing", 2_000),
        ("plan: a shipping program anywhere", 140),
        ("plan: a pck embedded below the top", 390),
        ("weak: paks, no program", 400),
        ("weak: embedded pck", 280),
        ("odd names named", 1_400),
        ("odd projects chosen", 750),
        ("trees with 9+ top programs", 300),
    ] {
        // The odd names are laid on Unix only: no Windows disk holds them.
        if !cfg!(unix) && k.starts_with("odd ") {
            continue;
        }
        let floor = (per_20k * trees as usize / 20_000).max(1);
        let got = report.iter().find(|(name, _)| *name == k).unwrap().1;
        assert!(
            got >= floor,
            "{k}: {got} < {floor}: the corpus no longer reaches it"
        );
    }

    // C1b: one manifest plan per program, beside C1's probe plan; and every outcome it is for.
    let actions = c.c1b_actions.lock().unwrap().clone();
    let refusals = c.c1b_refusals.lock().unwrap().clone();
    eprintln!(
        "C1b corpus: {} plans, {} planned\n  actions {actions:?}\n  refusals {refusals:?}",
        get(&c.c1b_plans),
        get(&c.c1b_planned)
    );
    assert_eq!(get(&c.c1b_plans), get(&c.plans));
    // About half what the seed reaches in 20,000 trees, as C1's floors are.
    let action = |a: &str| actions.get(a).copied().unwrap_or(0);
    let refusal = |r: &str| refusals.get(r).copied().unwrap_or(0);
    let c1b_floors: Vec<(&str, usize, usize)> = vec![
        ("planned", get(&c.c1b_planned), 2_500),
        ("add", action("add"), 44_000),
        ("replace", action("replace"), 1_300),
        ("reuse-known-proxy", action("reuse-known-proxy"), 1_300),
        ("keep-existing", action("keep-existing"), 40),
        ("engine-mismatch", refusal("engine-mismatch"), 15_000),
        ("game-mismatch", refusal("game-mismatch"), 9_000),
        ("anti-cheat", refusal("anti-cheat"), 6_000),
        ("foreign-loader", refusal("foreign-loader"), 700),
        // tests/corpus/divergence.txt `foreign-dwmapi`, applied (fresh plans, plans from a
        // ledger, and 0.3.1's ledgers).
        ("row foreign-dwmapi", get(&c.c1b_row_dwmapi), 800),
    ];
    for (k, got, per_20k) in c1b_floors {
        let floor = (per_20k * trees as usize / 20_000).max(1);
        assert!(
            got >= floor,
            "C1b {k}: {got} < {floor}: the corpus no longer reaches it"
        );
    }

    // C1b past the fresh game: installs, plans from their ledgers, updates, uninstalls, and the
    // upgrade from 0.3.1's ledgers; every action a plan from a ledger is for.
    let past_actions = c.c1b_replan_actions.lock().unwrap().clone();
    let past_refusals = c.c1b_replan_refusals.lock().unwrap().clone();
    let changes = c.c1b_changes.lock().unwrap().clone();
    eprintln!(
        "C1b past the fresh game: {} installs ({} refused alike), {} plans from the ledger, {} updates, {} uninstalls, {} from 0.3.1's ledgers\n  changes {changes:?}\n  actions {past_actions:?}\n  refusals {past_refusals:?}\n  {} changed files we had replaced, {} look-alikes kept, {} former files removed",
        get(&c.c1b_installs),
        get(&c.c1b_install_refused),
        get(&c.c1b_replans),
        get(&c.c1b_updates),
        get(&c.c1b_uninstalls),
        get(&c.c1b_cross),
        get(&c.c1b_changed_replaced),
        get(&c.c1b_lookalikes_kept),
        get(&c.c1b_former_removed),
    );
    eprintln!(
        "C1b's declared rows applied: foreign-dwmapi {}, proxy-folder-gained-files {}, restore-partial {}",
        get(&c.c1b_row_dwmapi),
        get(&c.c1b_row_proxy_folder),
        get(&c.c1b_row_restore_partial)
    );
    assert_eq!(get(&c.c1b_replans), get(&c.c1b_installs));
    assert_eq!(get(&c.c1b_uninstalls), get(&c.c1b_installs));
    let past_action = |a: &str| past_actions.get(a).copied().unwrap_or(0);
    let change = |k: Change| changes.get(&format!("{k:?}")).copied().unwrap_or(0);
    // About half what the seed reaches in 20,000 trees, as C1's floors are. Rewritten and the
    // look-alikes are rare here (they need a BepInEx manifest on a Unity program); the games the
    // manifests are for reach every change for every manifest
    // (`c1b_every_manifest_plans_as_0_4_1_on_the_games_it_is_for`).
    let past_floors: Vec<(&str, usize, usize)> = vec![
        ("installs", get(&c.c1b_installs), 1_500),
        ("plans from the ledger", get(&c.c1b_replans), 1_500),
        ("updates", get(&c.c1b_updates), 580),
        ("uninstalls", get(&c.c1b_uninstalls), 1_500),
        ("plans from 0.3.1's ledgers", get(&c.c1b_cross), 760),
        ("add", past_action("add"), 20_000),
        ("replace", past_action("replace"), 800),
        ("reuse-known-proxy", past_action("reuse-known-proxy"), 600),
        ("keep-existing", past_action("keep-existing"), 30),
        ("share-loader", past_action("share-loader"), 23),
        (
            "their Doorstop over ours",
            change(Change::TheirDoorstop),
            300,
        ),
        ("ours rewritten", change(Change::Rewritten), 12),
        ("ours changed", change(Change::OursChanged), 300),
        (
            "ours changed where we replaced",
            get(&c.c1b_changed_replaced),
            90,
        ),
        (
            "older Astra's look-alikes laid",
            change(Change::OlderAstra),
            13,
        ),
        ("look-alikes kept", get(&c.c1b_lookalikes_kept), 38),
        // tests/corpus/divergence.txt `proxy-folder-gained-files`, applied.
        (
            "row proxy-folder-gained-files",
            get(&c.c1b_row_proxy_folder),
            9,
        ),
        // tests/corpus/divergence.txt `restore-partial`, applied.
        ("row restore-partial", get(&c.c1b_row_restore_partial), 290),
    ];
    for (k, got, per_20k) in past_floors {
        let floor = (per_20k * trees as usize / 20_000).max(1);
        assert!(
            got >= floor,
            "C1b past the fresh game, {k}: {got} < {floor}: the corpus no longer reaches it"
        );
    }

    // C1c for planning, and the planning canary: what each reached.
    // About half what the seed reaches in 20,000 trees, as C1's floors are.
    let planning = [
        ("C1b plans with a hostile set", get(&c.c1b_hostile), 34_000),
        (
            "plans asked with Steam's hint",
            get(&c.planning_hinted),
            32_000,
        ),
        (
            "Pass B's program pinned (hostile set)",
            get(&c.planning_pinned),
            8_000,
        ),
        (
            "Pass B's program pinned (lower standings)",
            get(&c.planning_pinned_lower),
            1_400,
        ),
        (
            "closed folders: programs refused",
            get(&c.planning_closed),
            27_000,
        ),
        (
            "open folders: programs named",
            get(&c.planning_named),
            1_500,
        ),
        (
            "named games: programs left unknown",
            get(&c.planning_unclaimed),
            3_000,
        ),
        (
            "... without the winner's anchor",
            get(&c.planning_unclaimed_anchorless),
            2_700,
        ),
    ];
    eprintln!("C1c for planning and the planning canary:");
    for (k, v, _) in &planning {
        eprintln!("  {v:7}  {k}");
    }
    assert_eq!(get(&c.c1b_hostile), get(&c.c1b_plans));
    for (k, got, per_20k) in planning {
        let floor = (per_20k * trees as usize / 20_000).max(1);
        assert!(
            got >= floor,
            "planning canary, {k}: {got} < {floor}: the corpus no longer reaches it"
        );
    }
}

/// C1b on the games each manifest is for, beside the corpus (where a pinned program such as
/// GTA III's `Gameface/Binaries/Win64/LibertyCity.exe` never occurs): every manifest planned on
/// a Unity Mono, a Unity IL2CPP, an Unreal and an unshipped-Unreal game, and on each pinned
/// program's own, with its game files absent, ours, or someone else's — and every manifest
/// planning at least once.
#[test]
fn c1b_every_manifest_plans_as_0_4_1_on_the_games_it_is_for() {
    assert_eq!(
        HIJACK,
        astra_game_launcher::HIJACK_NAMES,
        "the hijack names C1b's rows read are the launcher's (src/plan.rs HIJACK_NAMES)"
    );
    let fixtures: Vec<Fixture> = MANIFESTS.iter().map(|(n, m)| fixture(n, m)).collect();
    let world = Tmp::new("c1b-appinfo");
    let appinfo = world.join("appinfo.vdf");
    let apps: Vec<(u32, Option<bool>)> = fixtures
        .iter()
        .filter_map(|f| f.appid)
        .map(|a| (a, Some(false)))
        .collect();
    fs::write(&appinfo, appinfo_vac(&apps)).unwrap();

    let mono = |exe: &str| {
        let exe = exe.to_owned();
        move |d: &Path| {
            let stem = exe.trim_end_matches(".exe");
            write(&d.join(&exe), b"MZ game");
            write(&d.join("UnityPlayer.dll"), b"MZ player");
            write(
                &d.join(format!("{stem}_Data/Managed/Assembly-CSharp.dll")),
                b"MZ cs",
            );
        }
    };
    type Lay = Box<dyn Fn(&Path)>;
    let mut shapes: Vec<(String, Lay)> = vec![
        ("Game.exe".into(), Box::new(mono("Game.exe"))),
        (
            "Game.exe".into(),
            Box::new(|d: &Path| {
                write(&d.join("Game.exe"), b"MZ game");
                write(&d.join("UnityPlayer.dll"), b"MZ player");
                write(&d.join("GameAssembly.dll"), b"MZ ga");
                fs::create_dir_all(d.join("Game_Data/il2cpp_data")).unwrap();
            }),
        ),
        (
            "Game/Binaries/Win64/Game-Win64-Shipping.exe".into(),
            Box::new(|d: &Path| {
                write(
                    &d.join("Game/Binaries/Win64/Game-Win64-Shipping.exe"),
                    b"MZ game",
                );
                write(&d.join("Game/Content/Paks/pakchunk0-Windows.pak"), b"pak");
            }),
        ),
        (
            "Gameface/Binaries/Win64/LibertyCity.exe".into(),
            Box::new(|d: &Path| {
                write(
                    &d.join("Gameface/Binaries/Win64/LibertyCity.exe"),
                    b"MZ game",
                );
                write(&d.join("Gameface/Content/Paks/gta3.pak"), b"pak");
                write(&d.join("PlayGTA3.exe"), b"MZ bootstrapper");
            }),
        ),
    ];
    for f in &fixtures {
        if let Some(exe) = &f.exe
            && exe != "*"
            && !exe.contains('/')
        {
            shapes.push((exe.clone(), Box::new(mono(exe))));
        }
    }
    let mut planned: BTreeMap<&str, usize> = BTreeMap::new();
    let counts = Counts::default();
    let mut plans = 0;
    let (installs, pool) = Pool::new();
    std::thread::scope(|s| {
        pool.run(s, &fixtures, &appinfo, &counts);
        for (n, f) in fixtures.iter().enumerate() {
            for (i, (exe, lay)) in shapes.iter().enumerate() {
                for variant in 0..3 {
                    let t = Tmp::new("c1b-game");
                    lay(t.path());
                    let game = t.path().join(exe);
                    let game = game.parent().unwrap();
                    for (leaf, ours) in &f.beside {
                        match variant {
                            1 => write(&game.join(leaf), ours),
                            2 => write(&game.join(leaf), b"MZ someone else's"),
                            _ => {}
                        }
                    }
                    let profile = Tmp::new("c1b-profile");
                    let p = Planned {
                        dir: t.path(),
                        exe: Path::new(exe),
                        appid: f.appid,
                        appinfo: &appinfo,
                        profile: &profile.join("p"),
                        proton: true,
                    };
                    let foreign = foreign_dwmapi(game, f);
                    let new = plan_new(&p, &f.zip);
                    let old = plan_legacy(&p, &f.zip);
                    assert_eq!(
                        new,
                        want_with_rows(&old, foreign.as_deref(), &counts),
                        "C1b: {} on shape {i} ({exe}), variant {variant}",
                        f.name
                    );
                    plans += 1;
                    if new.starts_with('{') {
                        *planned.entry(f.name).or_default() += 1;
                        // And past the fresh game: every change, each from its own copies.
                        for (j, change) in CHANGES.iter().enumerate() {
                            let past = Past {
                                change: *change,
                                cross: true,
                                update: (variant + j).is_multiple_of(2),
                                pick: n + i + j,
                            };
                            let at = format!("shape {i} ({exe}), variant {variant}");
                            installs.send(Job::copy(at, &p, n, past)).unwrap();
                        }
                    }
                }
            }
        }
        drop(installs);
    });
    pool.finish();
    let installed = counts.c1b_installed.lock().unwrap().clone();
    eprintln!(
        "C1b on its own games: {plans} plans; planned per manifest {planned:?}; installed {installed:?}; changes {:?}; actions from the ledger {:?}",
        counts.c1b_changes.lock().unwrap(),
        counts.c1b_replan_actions.lock().unwrap()
    );
    for f in &fixtures {
        assert!(
            planned.get(f.name).is_some_and(|n| *n > 0),
            "C1b: {} never planned on a game it is for",
            f.name
        );
        assert!(
            installed.get(f.name).is_some_and(|n| *n > 0),
            "C1b: {} never installed on a game it is for",
            f.name
        );
    }
    // Each change made, and every action a plan from a ledger is for.
    let changes = counts.c1b_changes.lock().unwrap().clone();
    let actions = counts.c1b_replan_actions.lock().unwrap().clone();
    for change in &CHANGES[1..] {
        assert!(
            changes.get(&format!("{change:?}")).is_some_and(|n| *n > 0),
            "C1b: {change:?} never made on the games the manifests are for"
        );
    }
    for a in ACTIONS {
        assert!(
            actions.get(*a).is_some_and(|n| *n > 0),
            "C1b: no plan from a ledger holds {a} on the games the manifests are for"
        );
    }
    // Every declared row, applied on the games the manifests are for.
    let rows = (
        counts.c1b_row_dwmapi.load(Ordering::Relaxed),
        counts.c1b_row_proxy_folder.load(Ordering::Relaxed),
        counts.c1b_row_restore_partial.load(Ordering::Relaxed),
    );
    eprintln!(
        "C1b on its own games: rows applied (foreign-dwmapi, proxy-folder-gained-files, restore-partial) {rows:?}"
    );
    assert!(
        rows.0 >= 1 && rows.1 >= 1 && rows.2 >= 1,
        "C1b: a declared row never applied: {rows:?}"
    );
}

/// The generator is what C1 was proven on: the same seed, the same tree.
#[test]
fn the_corpus_is_deterministic() {
    let shape = |seed| {
        let t = corpus::tree(seed);
        let mut s = String::new();
        for n in &t.nodes {
            match n {
                Node::File(p, b) => s.push_str(&format!("F {p} {}\n", b.len())),
                Node::Dir(p) => s.push_str(&format!("D {p}\n")),
                Node::Link(a, b) => s.push_str(&format!("L {a} {b}\n")),
                Node::Many(p, n) => s.push_str(&format!("M {p} {n}\n")),
                Node::Fill(p, n) => s.push_str(&format!("Z {p} {n}\n")),
            }
        }
        s.push_str(&t.hints.join("|"));
        s.push_str(&format!("\n{:?} {:?} {}", t.app, t.programs, t.truncation));
        s
    };
    for seed in [SEED, SEED + 1, SEED + 999] {
        assert_eq!(shape(seed), shape(seed));
    }
    assert_ne!(shape(SEED), shape(SEED + 1));
    // Pinned across commits: a generator edit changes the corpus C1 was proven on only when
    // this constant changes with it, in the same commit, and says so. Unix: the odd names are
    // laid there only.
    let first: String = (0..PINNED).map(|i| shape(SEED + i)).collect();
    // Every stream lays something within the pinned range, or the pin would not see it change.
    let mut streams: Vec<&str> = (0..PINNED)
        .flat_map(|i| corpus::tree(SEED + i).streams)
        .collect();
    streams.sort_unstable();
    streams.dedup();
    let want: &[&str] = if cfg!(unix) {
        &["crowds", "extras", "odd_names", "strays"]
    } else {
        &["crowds", "extras", "strays"]
    };
    assert_eq!(streams, want);
    if cfg!(unix) {
        assert_eq!(
            sha(first.as_bytes()),
            CORPUS_SHA256,
            "the corpus changed: update CORPUS_SHA256 deliberately"
        );
    }
}

/// How many trees, from [`SEED`] on, [`CORPUS_SHA256`] pins.
const PINNED: u64 = 2_000;
/// `sha256` of the first [`PINNED`] trees' shapes ([`the_corpus_is_deterministic`]), on Unix.
const CORPUS_SHA256: &str = "c01a2c920f86762eb1f9e1de5df4b4461bb23317862457b84813109f1bb66a1a";
