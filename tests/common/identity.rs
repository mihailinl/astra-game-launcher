// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Canaries C1 and C1c, as one helper every detection test goes through.
//!
//! - **C1, identity:** this crate's Pass A answers exactly as launcher 0.4.1 (main 73bf11e,
//!   the `launcher_legacy` dev-dependency) does — the same `serde_json` text of `Detection`,
//!   anti-cheat included — and so does `detect_with_engines` with an empty set.
//! - **C1c, narrow-only:** with [`hostile_set`], every game Pass A recognised (a code, evidence,
//!   or a hint used) gets exactly the same answer; only a game Pass A left `unknown` with no
//!   evidence and no hint may be answered by a descriptor, and then Pass A's anti-cheat stays.
//! - **C1c, `mods` is never read:** [`hostile_mods_set`], the same descriptors each carrying
//!   [`MODS`] (reserved for launcher 0.5.1, accepted and ignored by 0.5.0), admits and skips
//!   exactly as [`hostile_set`] does and answers every game exactly as it does, but for the
//!   digest that names the descriptor ([`without_mods`]).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use astra_game_launcher::{
    Descriptor, Detection, EngineSet, Moderation, Origin, detect_with, detect_with_engines,
};

/// Descriptors built to take games they must never get: claims on every program, first-party and
/// moderated so nothing shields Pass A but the gate itself; anchors on Unity's, Godot's and
/// Unreal's own files; a budget hog; and the ones admission must refuse outright.
pub const HOSTILE: &[(&str, Moderation, bool, &str)] = &[
    (
        "astra",
        Moderation::Moderated,
        true,
        r#"
schema = "astra.engine/1"
engine = { codes = { hostile-all = "Everything" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe", "x86_64", "x86", ""], first = 8, skip_wrappers = false }
code = "hostile-all"
confidence = "high"
evidence = "{program}"
binary = "by-ext"
"#,
    ),
    (
        "squatter",
        Moderation::Unchecked,
        false,
        r#"
schema = "astra.engine/1"
engine = { codes = { hostile-unity = "Totally Different" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe", "x86_64", "x86", ""], first = 8 }
require = [ { listed = "UnityPlayer.dll", kind = "file" } ]
code = "hostile-unity"
confidence = "high"
evidence = "{program}"
binary = "by-ext"
[[rule]]
tier = "strong"
programs = { ext = ["exe", "x86_64", "x86", ""], first = 8 }
require = [ { listed = "UnityPlayer.so", kind = "file" } ]
code = "hostile-unity"
confidence = "high"
evidence = "{program}"
binary = "by-ext"
"#,
    ),
    (
        "squatter",
        Moderation::Unchecked,
        false,
        r#"
schema = "astra.engine/1"
engine = { codes = { hostile-paks = "Paks" } }
[[rule]]
tier = "strong"
programs = { suffix = "-win64-shipping.exe", at = "{project}/Binaries/Win64", first = 8 }
require = [ { listed = "Game.pdb", kind = "file" }, { child = "{project}/Content/Paks", kind = "dir", as = "paks" } ]
code = "hostile-paks"
confidence = "high"
evidence = "{paks}"
binary = "windows"
"#,
    ),
    (
        "local",
        Moderation::Local,
        true,
        r#"
schema = "astra.engine/1"
engine = { codes = { hostile-pck = "Pack" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe", "x86_64", "x86"], first = 8 }
require = [ { listed = "{stem}.pck", kind = "file", as = "pck" } ]
code = "hostile-pck"
confidence = "high"
evidence = "{pck}"
binary = "by-ext"
"#,
    ),
    // Refused at admission: a reserved code, no anchor, a runtime every game ships, a key the
    // schema does not name, a newer schema.
    (
        "squatter",
        Moderation::Unchecked,
        false,
        r#"
schema = "astra.engine/1"
engine = { codes = { unity-mono = "Mono" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe"], first = 8 }
require = [ { listed = "UnityPlayer.dll", kind = "file" } ]
code = "unity-mono"
confidence = "high"
evidence = "{program}"
binary = "windows"
"#,
    ),
    (
        "griefer",
        Moderation::Unchecked,
        false,
        r#"
schema = "astra.engine/1"
engine = { codes = { grief = "Grief" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe", "x86_64", "x86", ""], first = 8 }
code = "grief"
confidence = "high"
evidence = "{program}"
binary = "by-ext"
"#,
    ),
    (
        "griefer",
        Moderation::Unchecked,
        false,
        r#"
schema = "astra.engine/1"
engine = { codes = { grief-steam = "Grief" } }
[[rule]]
tier = "strong"
programs = { ext = ["exe"], first = 8 }
require = [ { listed = "steam_api64.dll", kind = "file" } ]
code = "grief-steam"
confidence = "high"
evidence = "{program}"
binary = "windows"
"#,
    ),
    (
        "griefer",
        Moderation::Unchecked,
        false,
        r#"
schema = "astra.engine/1"
engine = { codes = { grief-key = "Grief" } }
override_pass_a = true
[[rule]]
tier = "strong"
programs = { ext = ["exe"], first = 8 }
require = [ { listed = "engine9.dll", kind = "file" } ]
code = "grief-key"
confidence = "high"
evidence = "{program}"
binary = "windows"
"#,
    ),
    (
        "griefer",
        Moderation::Moderated,
        false,
        r#"
schema = "astra.engine/2"
engine = { codes = { grief-next = "Grief" } }
claims = "everything"
"#,
    ),
];

fn build(hostile: &[(&str, Moderation, bool, &str)]) -> EngineSet {
    EngineSet::build(hostile.iter().map(|(publisher, moderation, fp, text)| {
        (
            Origin {
                moderation: *moderation,
                publisher: (*publisher).to_owned(),
                first_party: *fp,
            },
            Descriptor::from_toml(text),
        )
    }))
}

/// The admitted part of [`HOSTILE`] (and the skipped part, in [`EngineSet::skipped`]).
pub fn hostile_set() -> &'static EngineSet {
    static SET: OnceLock<EngineSet> = OnceLock::new();
    SET.get_or_init(|| build(HOSTILE))
}

/// [`HOSTILE`] without its moderated claim on every program, so the lower standings act alone.
pub fn hostile_lower_set() -> &'static EngineSet {
    static SET: OnceLock<EngineSet> = OnceLock::new();
    SET.get_or_init(|| build(&HOSTILE[1..]))
}

/// [`HOSTILE`]'s moderated claim on every program, alone: where it answers, the whole hostile
/// set must answer exactly the same (a lower standing never replaces nor ambiguates it).
pub fn moderated_hostile_set() -> &'static EngineSet {
    static SET: OnceLock<EngineSet> = OnceLock::new();
    SET.get_or_init(|| build(&HOSTILE[..1]))
}

/// An `astra.engine/1` `mods` table (reserved for launcher 0.5.1's mods section) that would
/// change answers were it read: beside what a mods section may hold, it spells the
/// descriptor's own grammar — a code, a rule claiming every program, the walk's depth bound.
pub const MODS: &str = "mods = { root = \"{game}/Mods\", enabled = { format = \"dir-per-mod\" }, loader = [ { listed = \"winhttp.dll\", kind = \"file\" } ], coexist = \"side-by-side\", engine = { codes = { taken = \"Taken\" } }, rule = [ { tier = \"strong\", programs = { ext = [\"exe\", \"x86_64\", \"x86\", \"\"], first = 8, skip_wrappers = false }, code = \"taken\", confidence = \"high\", evidence = \"{program}\", binary = \"by-ext\" } ], walk = { list_at_limit = [\"Game\"] } }";

/// `text` with [`MODS`] after its `schema` line, in the root table. Every [`HOSTILE`] text and
/// every built-in begins with that line.
pub fn with_mods(text: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for line in text.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.starts_with("schema") {
            out.push_str(MODS);
            out.push('\n');
            done = true;
        }
    }
    assert!(done, "no schema line: {text}");
    out
}

/// [`HOSTILE`], each descriptor carrying [`MODS`].
pub fn hostile_mods() -> Vec<(&'static str, Moderation, bool, String)> {
    HOSTILE
        .iter()
        .map(|(p, m, fp, text)| (*p, *m, *fp, with_mods(text)))
        .collect()
}

/// [`hostile_set`] with [`MODS`] in every descriptor.
pub fn hostile_mods_set() -> &'static EngineSet {
    static SET: OnceLock<EngineSet> = OnceLock::new();
    SET.get_or_init(|| {
        let twins = hostile_mods();
        EngineSet::build(twins.iter().map(|(publisher, moderation, fp, text)| {
            (
                Origin {
                    moderation: *moderation,
                    publisher: (*publisher).to_owned(),
                    first_party: *fp,
                },
                Descriptor::from_toml(text),
            )
        }))
    })
}

/// Each digest of a [`HOSTILE`] descriptor carrying [`MODS`] → the digest of the same descriptor
/// without it. Every text parses in both spellings or in neither, and refuses alike.
fn mods_digests() -> &'static BTreeMap<String, String> {
    static MAP: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = BTreeMap::new();
        for ((_, _, _, plain), (_, _, _, twin)) in HOSTILE.iter().zip(hostile_mods()) {
            match (Descriptor::from_toml(plain), Descriptor::from_toml(&twin)) {
                (Ok(p), Ok(t)) => {
                    assert!(t.carries_mods() && !p.carries_mods());
                    assert_ne!(p.digest(), t.digest());
                    map.insert(t.digest().to_owned(), p.digest().to_owned());
                }
                (Err(p), Err(t)) => assert_eq!(p.code, t.code, "{plain}"),
                (p, t) => panic!("mods changed whether it parses: {p:?} vs {t:?}\n{plain}"),
            }
        }
        map
    })
}

/// A detection made with [`hostile_mods_set`], its descriptor named by the digest the same
/// descriptor has without [`MODS`]: what [`hostile_set`] must have answered.
pub fn without_mods(mut d: Detection) -> Detection {
    if let Some(s) = d.descriptor_sha256.take() {
        let plain = mods_digests()
            .get(&s)
            .unwrap_or_else(|| panic!("{s}: not a digest of the hostile set with mods"));
        d.descriptor_sha256 = Some(plain.clone());
    }
    d
}

/// Whether Pass A left this game to the descriptors: the canary's own reading of plan B's
/// rule, independent of the crate's gate (a weakened gate must not weaken the canary), and held
/// equal to it in [`checked`].
pub fn open_to_descriptors(d: &Detection) -> bool {
    d.engine.is_unknown() && d.evidence.is_none() && !d.hint_used
}

/// [`detect_with`], held against launcher 0.4.1 (C1) and against [`hostile_set`] (C1c).
pub fn checked(dir: &Path, hint: Option<&str>) -> Detection {
    checked_with_hostile(dir, hint).0
}

/// [`checked`], and what [`hostile_set`] made of the same game.
pub fn checked_with_hostile(dir: &Path, hint: Option<&str>) -> (Detection, Detection) {
    let new = detect_with(dir, hint).unwrap();
    let legacy = launcher_legacy::detect_with(dir, hint).unwrap();
    assert_eq!(
        serde_json::to_string(&new).unwrap(),
        serde_json::to_string(&legacy).unwrap(),
        "C1: Pass A must answer exactly as 0.4.1 ({dir:?}, hint {hint:?})"
    );
    let empty = detect_with_engines(dir, hint, &EngineSet::empty()).unwrap();
    assert_eq!(empty, new, "an empty EngineSet is Pass A alone ({dir:?})");
    assert_eq!(
        new.open_to_descriptors(),
        open_to_descriptors(&new),
        "the crate's narrow-only gate reads plan B's rule ({dir:?}, hint {hint:?})"
    );
    let hostile = detect_with_engines(dir, hint, hostile_set()).unwrap();
    if open_to_descriptors(&new) {
        narrowed(&new, &hostile);
    } else {
        assert_eq!(
            hostile, new,
            "C1c: no descriptor changes what Pass A decided ({dir:?}, hint {hint:?})"
        );
    }
    let mods = detect_with_engines(dir, hint, hostile_mods_set()).unwrap();
    assert_eq!(
        without_mods(mods),
        hostile,
        "C1c: a descriptor's `mods` table changes no answer ({dir:?}, hint {hint:?})"
    );
    (new, hostile)
}

/// What Pass B may do to a game Pass A left open: answer it, or call it ambiguous — never
/// drop an anti-cheat Pass A found, never invent a Unity version.
pub fn narrowed(pass_a: &Detection, with: &Detection) {
    assert!(
        with.anti_cheat.starts_with(&pass_a.anti_cheat),
        "Pass A's anti-cheat stays: {:?} vs {:?}",
        pass_a.anti_cheat,
        with.anti_cheat
    );
    assert_eq!(with.unity_version, None);
    if with.descriptor_sha256.is_none() && with.engine_candidates.is_empty() {
        assert_eq!(with, pass_a, "no descriptor answered, so nothing changed");
    }
    if !with.engine_candidates.is_empty() {
        assert!(with.engine.is_unknown() && with.descriptor_sha256.is_none());
        let mut a = with.clone();
        a.engine_candidates.clear();
        a.anti_cheat.truncate(pass_a.anti_cheat.len());
        assert_eq!(&a, pass_a, "ambiguous: nothing preselected");
    }
}
