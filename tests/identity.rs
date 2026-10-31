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
//! The tests in `tests/detect.rs` go through the same check, fixture by fixture.
//!
//! **Against plan B §2's C1** this is the generated corpus at its full size; the `bin/list-shape`
//! recordings (`tests/corpus/recorded/`), `divergence.txt` and running 0.4.1 in a child with a
//! timeout arrive with L2, when the built-ins leave the code that 0.4.1 shares with this crate
//! byte for byte (docs/ENGINE-DESCRIPTORS-DEBT.md).

mod common;
#[path = "corpus/gen.rs"]
mod corpus;

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use astra_game_launcher::{
    Detection, Engine, EngineSet, ExePick, detect_with_engines, valve_anti_cheat,
};
use common::identity::{
    checked_with_hostile, hostile_lower_set, hostile_set, moderated_hostile_set, narrowed,
    open_to_descriptors,
};
use common::{E, Tmp, appinfo_vac, sha, zip};
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

/// Where a plan runs: the tree, its program, the appid the caller vouches for, the appinfo.
struct Planned<'a> {
    dir: &'a Path,
    exe: &'a Path,
    appid: Option<u32>,
    appinfo: &'a Path,
    profile: &'a Path,
}

/// One plan, as text: the plan itself, or its refusal's code and params.
fn plan_new(p: &Planned, zip: &[u8]) -> String {
    use astra_game_launcher::{GameTarget, GiPackage, Platform, plan_install};
    let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
    let game = GameTarget {
        dir: p.dir.to_path_buf(),
        exe: p.exe.to_path_buf(),
        steam_appid: p.appid,
        platform: Platform::LinuxNative,
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
        platform: Platform::LinuxNative,
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

fn one_tree(seed: u64, zips: &[(Engine, Vec<u8>)], appinfo: &Path, counts: &Counts) {
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
    let world = Tmp::new("corpus-appinfo");
    let appinfo = world.join("appinfo.vdf");
    fs::write(
        &appinfo,
        appinfo_vac(&[(VAC, Some(true)), (CLEAR, Some(false))]),
    )
    .unwrap();
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
            let (zips, counts, appinfo) = (&zips, &counts, appinfo.as_path());
            s.spawn(move || {
                let mut i = w;
                while i < trees {
                    one_tree(SEED.wrapping_add(i), zips, appinfo, counts);
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
    ] {
        let floor = (per_20k * trees as usize / 20_000).max(1);
        let got = report.iter().find(|(name, _)| *name == k).unwrap().1;
        assert!(
            got >= floor,
            "{k}: {got} < {floor}: the corpus no longer reaches it"
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
