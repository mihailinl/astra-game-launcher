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
//! The tests in `tests/detect.rs` go through the same check, fixture by fixture. Where 0.5.0
//! knowingly answers otherwise (a link, a FIFO, a hint naming a Windows device), C1 has declared
//! exceptions, each pinned by `tests/divergence.rs` against `tests/corpus/divergence.txt`, where
//! 0.4.1 runs in a child process with a timeout. The corpus holds links but no FIFO, and no link
//! a read would follow, so 0.4.1 runs in-process here.
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

use astra_game_launcher::{
    Detection, Engine, EngineSet, ExePick, detect_with_engines, valve_anti_cheat,
};
use common::identity::{
    checked_with_hostile, hostile_lower_set, hostile_set, moderated_hostile_set, narrowed,
    open_to_descriptors,
};
use common::{E, Tmp, appinfo_vac, sha, write, zip};
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
    paks_only: AtomicUsize,
    embedded_pck: AtomicUsize,
    /// A built-in engine named for a game whose evidence holds a name only a Linux disk holds
    /// (`corpus::ODD_NAMES`); and an Unreal project so named chosen over another.
    odd_named: AtomicUsize,
    odd_project: AtomicUsize,
    /// C1b: plans with Astra's own manifests, those that planned, the actions they hold, and
    /// every refusal by its code.
    c1b_plans: AtomicUsize,
    c1b_planned: AtomicUsize,
    c1b_actions: Mutex<BTreeMap<String, usize>>,
    c1b_refusals: Mutex<BTreeMap<String, usize>>,
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
}

/// A zip of `manifest` and every file it names: each `[[game_files]]` source with bytes made up
/// from its pin (one pin, one content, so a pin shared by two files still matches both) and the
/// pin rewritten to them; each `[[files]]` pattern with a file under it.
fn fixture(name: &'static str, manifest: &str) -> Fixture {
    let table: toml::Table = toml::from_str(manifest).unwrap();
    let mut text = manifest.to_owned();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut beside = Vec::new();
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
                let bytes = format!("C1b {pin}").into_bytes();
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

/// One plan, as text: the plan itself, or its refusal's code and params.
fn plan_new(p: &Planned, zip: &[u8]) -> String {
    use astra_game_launcher::{GameTarget, GiPackage, Platform, plan_install};
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
    match plan_install(&pkg, &game, p.profile) {
        Ok(p) => serde_json::to_string(&p).unwrap(),
        Err(e) => format!("{} {:?}", e.code, e.params),
    }
}

fn plan_legacy(p: &Planned, zip: &[u8]) -> String {
    use launcher_legacy::{GameTarget, GiPackage, Platform, plan_install};
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
    match plan_install(&pkg, &game, p.profile) {
        Ok(p) => serde_json::to_string(&p).unwrap(),
        Err(e) => format!("{} {:?}", e.code, e.params),
    }
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
    fixtures: &[Fixture],
    appinfo: &Path,
    counts: &Counts,
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
    for hint in &tree.hints {
        let hint = (!hint.is_empty()).then_some(hint.as_str());
        let (d, with) = checked_with_hostile(t.path(), hint);
        bump(&counts.detections);
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
        }
        c1b_plan(seed, k, &p, fixtures, counts);
    }
}

/// C1b for one program: one of Astra's own manifests, turning with the seed, its game files laid
/// beside the program now and then (ours, or someone else's), planned by both launchers.
fn c1b_plan(seed: u64, k: usize, p: &Planned, fixtures: &[Fixture], counts: &Counts) {
    let turn = (seed as usize).wrapping_mul(31).wrapping_add(k * 7);
    let f = &fixtures[turn % fixtures.len()];
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
    let new = plan_new(&p, &f.zip);
    let old = plan_legacy(&p, &f.zip);
    assert_eq!(
        new, old,
        "C1b: Astra's own manifest {} plans and refuses exactly as with 0.4.1 (seed {seed:#x}, exe {:?}, appid {:?})",
        f.name, p.exe, p.appid
    );
    bump(&counts.c1b_plans);
    if new.starts_with('{') {
        bump(&counts.c1b_planned);
        let mut actions = counts.c1b_actions.lock().unwrap();
        for a in [
            "add",
            "replace",
            "reuse-known-proxy",
            "keep-existing",
            "share-loader",
        ] {
            *actions.entry(a.to_owned()).or_default() +=
                new.matches(&format!("\"action\":\"{a}\"")).count();
        }
    } else {
        let code = new.split(' ').next().unwrap_or("").to_owned();
        *counts.c1b_refusals.lock().unwrap().entry(code).or_default() += 1;
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
    std::thread::scope(|s| {
        for w in 0..THREADS {
            let (zips, fixtures, counts, appinfo) = (&zips, &fixtures, &counts, appinfo.as_path());
            s.spawn(move || {
                let mut i = w;
                while i < trees {
                    one_tree(SEED.wrapping_add(i), zips, fixtures, appinfo, counts);
                    i += THREADS;
                }
            });
        }
    });
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
        ("weak: paks, no program", get(&c.paks_only)),
        ("weak: embedded pck", get(&c.embedded_pck)),
        ("odd names named", get(&c.odd_named)),
        ("odd projects chosen", get(&c.odd_project)),
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
        ("weak: paks, no program", 400),
        ("weak: embedded pck", 280),
        ("odd names named", 1_400),
        ("odd projects chosen", 750),
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
    ];
    for (k, got, per_20k) in c1b_floors {
        let floor = (per_20k * trees as usize / 20_000).max(1);
        assert!(
            got >= floor,
            "C1b {k}: {got} < {floor}: the corpus no longer reaches it"
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
    let mut plans = 0;
    for f in &fixtures {
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
                let new = plan_new(&p, &f.zip);
                let old = plan_legacy(&p, &f.zip);
                assert_eq!(
                    new, old,
                    "C1b: {} on shape {i} ({exe}), variant {variant}",
                    f.name
                );
                plans += 1;
                if new.starts_with('{') {
                    *planned.entry(f.name).or_default() += 1;
                }
            }
        }
    }
    eprintln!("C1b on its own games: {plans} plans; planned per manifest {planned:?}");
    for f in &fixtures {
        assert!(
            planned.get(f.name).is_some_and(|n| *n > 0),
            "C1b: {} never planned on a game it is for",
            f.name
        );
    }
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
}
