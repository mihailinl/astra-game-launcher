// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Canary C1 over a generated corpus: for every tree of a fixed seed, under every launch hint
//! the generator gives it, this crate's detector answers exactly as launcher 0.4.1's (main
//! 73bf11e) — the same `serde_json` text of `Detection`, anti-cheat included — with no engine
//! descriptors, and with a hostile set (C1c: unchanged wherever Pass A decided). Planning is
//! held to 0.4.1's too: the anti-cheat refusal, and `engine-mismatch`'s `got` (the detector's
//! `engine_of_exe`, which planning alone exposes), for a probe integration per built-in code.
//!
//! The tests in `tests/detect.rs` go through the same check, fixture by fixture.

mod common;
#[path = "corpus/gen.rs"]
mod corpus;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use astra_game_launcher::{Engine, EngineSet, detect_with_engines};
use common::identity::{checked, hostile_set, open_to_descriptors};
use common::{E, Tmp, sha, zip};

/// Fixed for good: a new seed is a new corpus, and the old one is what C1 was proven on.
const SEED: u64 = 0xA57A_0500;
/// L1's corpus; L2 raises it to 20,000.
const TREES: u64 = 2_400;
const THREADS: u64 = 8;

#[derive(Default)]
struct Counts {
    trees: AtomicUsize,
    detections: AtomicUsize,
    open: AtomicUsize,
    claimed: AtomicUsize,
    plans: AtomicUsize,
    anti_cheat: AtomicUsize,
    mismatch: AtomicUsize,
}

fn probe_zip(engine: &str) -> Vec<u8> {
    let manifest = format!(
        "schema = 1\n\n[integration]\nid = \"probe.engine\"\nname = \"Probe\"\nversion = \"1.0.0\"\n\n[target]\nengine = \"{engine}\"\nexe = \"*\"\nplatforms = [\"windows-x64\", \"linux-proton\", \"linux-x64\"]\nanti_cheat = \"none\"\n"
    );
    zip(&[E::File("astra-gi.toml", manifest.as_bytes())])
}

/// One plan, as text: the plan itself, or its refusal's code and params.
fn plan_new(dir: &Path, exe: &Path, zip: &[u8], profile: &Path) -> String {
    use astra_game_launcher::{GameTarget, GiPackage, Platform, plan_install};
    let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
    let game = GameTarget {
        dir: dir.to_path_buf(),
        exe: exe.to_path_buf(),
        steam_appid: None,
        platform: Platform::LinuxNative,
        steam_appinfo: Some(dir.join("no-appinfo.vdf")),
    };
    match plan_install(&pkg, &game, profile) {
        Ok(p) => serde_json::to_string(&p).unwrap(),
        Err(e) => format!("{} {:?}", e.code, e.params),
    }
}

fn plan_legacy(dir: &Path, exe: &Path, zip: &[u8], profile: &Path) -> String {
    use launcher_legacy::{GameTarget, GiPackage, Platform, plan_install};
    let pkg = GiPackage::open(zip, &sha(zip)).unwrap();
    let game = GameTarget {
        dir: dir.to_path_buf(),
        exe: exe.to_path_buf(),
        steam_appid: None,
        platform: Platform::LinuxNative,
        steam_appinfo: Some(dir.join("no-appinfo.vdf")),
    };
    match plan_install(&pkg, &game, profile) {
        Ok(p) => serde_json::to_string(&p).unwrap(),
        Err(e) => format!("{} {:?}", e.code, e.params),
    }
}

fn one_tree(seed: u64, zips: &[(Engine, Vec<u8>)], counts: &Counts) {
    let tree = corpus::tree(seed);
    let t = Tmp::new(&format!("corpus-{seed:x}"));
    tree.build(t.path());
    let profile = Tmp::new(&format!("corpus-profile-{seed:x}"));
    counts.trees.fetch_add(1, Ordering::Relaxed);
    for hint in &tree.hints {
        let hint = (!hint.is_empty()).then_some(hint.as_str());
        let d = checked(t.path(), hint);
        counts.detections.fetch_add(1, Ordering::Relaxed);
        if open_to_descriptors(&d) {
            counts.open.fetch_add(1, Ordering::Relaxed);
            let with = detect_with_engines(t.path(), hint, hostile_set()).unwrap();
            if with.descriptor_sha256.is_some() {
                counts.claimed.fetch_add(1, Ordering::Relaxed);
            }
        }
        if hint.is_some() {
            continue;
        }
        let Some(exe) = &d.exe else {
            continue;
        };
        for (engine, zip) in zips {
            let new = plan_new(t.path(), exe, zip, &profile.join("p"));
            let old = plan_legacy(t.path(), exe, zip, &profile.join("p"));
            assert_eq!(
                new, old,
                "C1: planning must refuse and plan exactly as 0.4.1 (seed {seed:#x}, exe {exe:?}, engine {engine:?})"
            );
            counts.plans.fetch_add(1, Ordering::Relaxed);
            if new.starts_with("anti-cheat ") {
                counts.anti_cheat.fetch_add(1, Ordering::Relaxed);
            }
            if new.starts_with("engine-mismatch ") {
                counts.mismatch.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[test]
fn c1_detection_and_planning_are_launcher_0_4_1s_over_the_corpus() {
    // The hostile set is the one `checked` uses; build it once, before the threads race to.
    assert!(!hostile_set().is_empty());
    assert!(EngineSet::empty().is_empty());
    let zips: Vec<(Engine, Vec<u8>)> = Engine::ALL
        .iter()
        .filter(|e| **e != Engine::Unknown)
        .map(|e| (*e, probe_zip(e.code())))
        .collect();
    let counts = Counts::default();
    std::thread::scope(|s| {
        for w in 0..THREADS {
            let (zips, counts) = (&zips, &counts);
            s.spawn(move || {
                let mut i = w;
                while i < TREES {
                    one_tree(SEED.wrapping_add(i), zips, counts);
                    i += THREADS;
                }
            });
        }
    });
    let get = |a: &AtomicUsize| a.load(Ordering::Relaxed);
    eprintln!(
        "C1 corpus: {} trees, {} detections ({} open to descriptors, {} claimed by the hostile set), {} plans ({} anti-cheat, {} engine-mismatch)",
        get(&counts.trees),
        get(&counts.detections),
        get(&counts.open),
        get(&counts.claimed),
        get(&counts.plans),
        get(&counts.anti_cheat),
        get(&counts.mismatch),
    );
    // The corpus must reach every path it is for, or its green proves nothing.
    assert_eq!(get(&counts.trees), TREES as usize);
    assert!(get(&counts.detections) >= 2 * TREES as usize);
    assert!(
        get(&counts.open) > 100,
        "trees Pass A leaves to descriptors"
    );
    assert!(
        get(&counts.claimed) > 50,
        "the hostile set must act somewhere"
    );
    assert!(get(&counts.anti_cheat) > 20, "anti-cheat refusals compared");
    assert!(get(&counts.mismatch) > 100, "engine_of_exe compared");
}

/// The generator is what C1 was proven on: the same seed, the same tree.
#[test]
fn the_corpus_is_deterministic() {
    let shape = |seed| {
        let t = corpus::tree(seed);
        let mut s = String::new();
        for n in &t.nodes {
            match n {
                corpus::Node::File(p, b) => s.push_str(&format!("F {p} {}\n", b.len())),
                corpus::Node::Dir(p) => s.push_str(&format!("D {p}\n")),
                corpus::Node::Link(a, b) => s.push_str(&format!("L {a} {b}\n")),
                corpus::Node::Many(p, n) => s.push_str(&format!("M {p} {n}\n")),
            }
        }
        s.push_str(&t.hints.join("|"));
        s
    };
    for seed in [SEED, SEED + 1, SEED + 999] {
        assert_eq!(shape(seed), shape(seed));
    }
    assert_ne!(shape(SEED), shape(SEED + 1));
}
