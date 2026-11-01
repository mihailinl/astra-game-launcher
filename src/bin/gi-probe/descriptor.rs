// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe validate-descriptor`: the lint of the engine descriptor specification
//! (`spec/games/engine-descriptor-v1.md` §5, AstraPlugins at [`SPEC_SHA`]). A descriptor that
//! parses may still be refused before it ever reaches a moderator:
//!
//! 1. it parses as `astra.engine/1` ([`Descriptor`]'s refusals: an unknown schema or key, a
//!    float, one over its size);
//! 2. Pass B admits it ([`Descriptor::check_pass_b`]): no reserved code or name, the first-party
//!    parts of the grammar only with `--first-party`, and every rule anchored on a literal name
//!    that is no program, wrapper, ubiquitous runtime or generic name
//!    (`descriptor-too-broad{reason}`);
//! 3. it claims none of the spec's negative folders — an empty folder, a folder holding a
//!    program only, one of bare system DLLs beside a program, and `steam_api64.dll` beside a
//!    program — each laid below `TMPDIR` and detected with this descriptor alone, as an unchecked
//!    one, through the same [`astra_game_launcher::detect_with_engines`] Astra calls. Each is laid
//!    as the spec draws it (`Game.exe` at the top) and again at every place the descriptor's
//!    rules look for a program ([`places`]: the folder `programs.at` names, `{project}` one
//!    folder, holding each program its `ext` or `suffix` selects), and each place is asked twice:
//!    as the folder search finds it, and with Steam's launch hint naming its program — a place
//!    deeper than the walk lists is reached only so. Any claim (it names the game, or makes it
//!    ambiguous) is `descriptor-too-broad{reason=negative:<folder>, program}`.
//!
//! 4. with `--corpus <folder>`: it claims no recordings of two or more engines — every
//!    `astra.list-shape/1` recording (`*.json`, as `gi-probe capture` writes them; the format is
//!    `tests/corpus/replay.rs`) in the folder is laid below `TMPDIR` and detected with this
//!    descriptor alone, as in step 3, with no hint and with the recording's own; the labels of
//!    those it claims, `unknown` aside (it names no engine), are the engines it claims.
//!    Two or more is `descriptor-too-broad{reason=two-engines, labels}` (spec §5,
//!    `E_DESCRIPTOR_TOO_BROAD`, which this launcher spells as its own refusal: debt row 13).
//!
//! A descriptor carrying `mods` (reserved for launcher 0.5.1's mods section) is a notice,
//! `mods-not-evaluated` (step 1, field `mods`: "mods: not evaluated by this version"), never a
//! finding: this version accepts the table and reads nothing in it ([`mods_notice`]).
//!
//! The report gives the descriptor's digest, `sha256(JCS(descriptor))`, which the catalogue
//! entry's `descriptor_sha256` must equal. The input is the descriptor as TOML or JSON, or a
//! foundation's `astra-gi.toml` holding it as `[integration.engine]`.

use std::collections::BTreeSet;
use std::path::Path;

use astra_game_launcher::{
    Descriptor, Detection, EngineSet, LauncherError, Moderation, Origin, codes, detect_with_engines,
};
use serde_json::{Value, json};

use crate::replay::{self, Recording};
use crate::{Fatal, Finding, Scratch, report};

/// The AstraPlugins commit whose `spec/games/` this probe follows (PR #117), and whose goldens
/// `tests/goldens/astraplugins/` holds: read from that copy's `PIN`, so a re-pin is
/// `tools/vendor-astraplugins-goldens.sh` alone (and whatever the new spec says).
pub const SPEC_SHA: &str = include_str!("../../../tests/goldens/astraplugins/PIN").trim_ascii_end();

/// The negative folders of spec §5: `(name, program, beside)` — whether it holds a program, and
/// the files beside that program (or, with none, in the folder). The program is laid at every
/// [`Place`] the descriptor looks for one ([`places`]).
pub const NEGATIVE_FOLDERS: &[(&str, bool, &[&str])] = &[
    ("empty", false, &[]),
    ("exe-only", true, &[]),
    (
        "system-dlls",
        true,
        &[
            "d3d11.dll",
            "d3d12.dll",
            "dxgi.dll",
            "d3dcompiler_47.dll",
            "dinput8.dll",
            "xinput1_3.dll",
            "XAudio2_9.dll",
            "X3DAudio1_7.dll",
            "vcruntime140.dll",
            "vcruntime140_1.dll",
            "msvcp140.dll",
            "ucrtbase.dll",
            "winhttp.dll",
            "version.dll",
            "dwmapi.dll",
            "openal32.dll",
        ],
    ),
    ("steam-api-beside-exe", true, &["steam_api64.dll"]),
];

/// The program as the spec draws the negative folders: `Game.exe`, at the top.
const SPEC_PROGRAM: &str = "Game.exe";

/// The names the probe gives the folders and programs it lays where a rule looks: [`places`]
/// takes the first one the rule does not skip. No two share three letters in a row and no two end
/// in the same letter (ASCII case ignored), none is long enough to hold a built-in wrapper's name
/// (the shortest is six letters), and there are more of them than a rule has wrappers and
/// `skip_dirs` together: each skip catches one name at most, so a folder name is always left, and
/// when no program name is, every one the rule would select ends in a wrapper's name — it selects
/// none, anywhere (`tests::no_rule_can_skip_every_neutral_name`).
const NEUTRAL_NAMES: &[&str] = &[
    "Game", "Koala", "Cob", "Zinc", "Field", "Waif", "Smog", "Moth", "Kiwi", "Hajj", "Yolk",
    "Petal", "Plum", "Fern", "Banjo", "Tulip", "Tranq", "Cedar", "Moss", "Quilt", "Emu", "Shiv",
    "Yew", "Onyx", "Ivy", "Topaz",
];

/// Where a negative folder is laid: the folder its program sits in (components below the
/// folder's root) and the program's file name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    dir: Vec<String>,
    program: String,
}

impl Place {
    /// The program below the folder's root, `/`-separated: Steam's launch hint for it.
    fn rel(&self) -> String {
        let mut parts = self.dir.clone();
        parts.push(self.program.clone());
        parts.join("/")
    }
}

/// The descriptor has something [`places`] does not read: an error, never a place left out.
fn unread(field: &str, what: impl std::fmt::Display) -> Fatal {
    Fatal {
        code: "io",
        detail: format!(
            "the descriptor's {field}: {what}: gi-probe reads less of astra.engine/1 than the launcher admits"
        ),
    }
}

/// A descriptor value's array of strings; `[]` when it is absent.
fn strings(v: Option<&Value>, field: &str) -> Result<Vec<String>, Fatal> {
    match v {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|s| s.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or_else(|| unread(field, "not an array of strings")),
        Some(_) => Err(unread(field, "not an array")),
    }
}

/// Every place `d`'s rules look for a program, the spec's own drawing ([`SPEC_PROGRAM`] at the
/// top) first: for each rule, each program its `programs` selects (`ext`: `<name>.<ext>`, or
/// `<name>` for `""`; `suffix`: `<name><suffix>`) in the folder its `programs.at` names
/// (`{project}`: one folder), or at the top when it names none. The names are the first of
/// [`NEUTRAL_NAMES`] the rule does not skip: a folder named in its `skip_dirs` or holding one of
/// the descriptor's `wrappers`, a program holding a wrapper (neither, with `skip_wrappers =
/// false`). Read off the descriptor's canonical JSON, which the launcher has parsed and admitted.
fn places(d: &Descriptor) -> Result<Vec<Place>, Fatal> {
    let v: Value =
        serde_json::from_str(d.canonical_json()).map_err(|e| unread("canonical JSON", e))?;
    let wrappers = strings(v.get("wrappers"), "wrappers")?;
    let wrapped = |name: &str| {
        let lower = name.to_ascii_lowercase();
        wrappers.iter().any(|w| lower.contains(w.as_str()))
    };
    let mut out = vec![Place {
        dir: Vec::new(),
        program: SPEC_PROGRAM.to_owned(),
    }];
    let rules = v
        .get("rule")
        .and_then(Value::as_array)
        .ok_or_else(|| unread("rule", "not an array"))?;
    for (i, r) in rules.iter().enumerate() {
        let field = format!("rule[{i}].programs");
        // `programs = "none"` (or `none = true`): Pass B refused it before any folder is asked.
        let Some(p) = r.get("programs").and_then(Value::as_object) else {
            continue;
        };
        if p.get("none") == Some(&Value::Bool(true)) {
            continue;
        }
        let skip_dirs = strings(p.get("skip_dirs"), &format!("{field}.skip_dirs"))?;
        // `skip_wrappers`: `true` (the default) skips both, `"folders"` folders only.
        let (dirs_wrapped, programs_wrapped) = match p.get("skip_wrappers") {
            None | Some(Value::Bool(true)) => (true, true),
            Some(Value::Bool(false)) => (false, false),
            Some(Value::String(s)) if s == "folders" => (true, false),
            Some(other) => return Err(unread(&field, format!("skip_wrappers = {other}"))),
        };
        let folder_skipped = |n: &str| {
            skip_dirs.iter().any(|s| s.eq_ignore_ascii_case(n)) || (dirs_wrapped && wrapped(n))
        };
        let dir = match p.get("at") {
            None => Vec::new(),
            Some(Value::String(at)) if at.is_empty() => Vec::new(),
            Some(Value::String(at)) => {
                let mut parts = at.split('/').peekable();
                let mut dir = Vec::new();
                if parts.next_if_eq(&"{project}").is_some() {
                    let project = NEUTRAL_NAMES
                        .iter()
                        .find(|n| !folder_skipped(n))
                        .expect("more neutral names than a rule's skips");
                    dir.push((*project).to_owned());
                }
                for part in parts {
                    if part.is_empty() || part.contains(['{', '}', '*']) {
                        return Err(unread(&format!("{field}.at"), format!("{at:?}")));
                    }
                    dir.push(part.to_owned());
                }
                dir
            }
            Some(other) => return Err(unread(&format!("{field}.at"), other)),
        };
        let endings: Vec<String> = match (p.get("ext"), p.get("suffix")) {
            (Some(_), None) => strings(p.get("ext"), &format!("{field}.ext"))?
                .into_iter()
                .map(|e| if e.is_empty() { e } else { format!(".{e}") })
                .collect(),
            (None, Some(Value::String(s))) => vec![s.clone()],
            _ => return Err(unread(&field, "neither ext nor suffix")),
        };
        for ending in endings {
            // None left: every program it selects ends in a wrapper's name, so it selects none.
            let Some(program) = NEUTRAL_NAMES
                .iter()
                .map(|n| format!("{n}{ending}"))
                .find(|n| !(programs_wrapped && wrapped(n)))
            else {
                continue;
            };
            let place = Place {
                dir: dir.clone(),
                program,
            };
            if !out.contains(&place) {
                out.push(place);
            }
        }
    }
    Ok(out)
}

/// What the file held.
enum Input {
    Json,
    Toml,
    Manifest,
}

impl Input {
    fn code(&self) -> &'static str {
        match self {
            Input::Json => "descriptor-json",
            Input::Toml => "descriptor-toml",
            Input::Manifest => "manifest-integration-engine",
        }
    }
}

fn refusal(e: &LauncherError, detail: &str) -> Finding {
    Finding::from_refusal(e.code, &e.params, detail)
}

fn parse(text: &str) -> (Input, astra_game_launcher::Result<Descriptor>) {
    if text.trim_start().starts_with('{') {
        return (Input::Json, Descriptor::from_json(text.as_bytes()));
    }
    // A foundation's manifest (schema 1, an integer) holds it as [integration.engine].
    if let Ok(t) = toml::from_str::<toml::Table>(text)
        && matches!(t.get("schema"), Some(toml::Value::Integer(_)))
    {
        let engine = t.get("integration").and_then(|i| i.get("engine")).cloned();
        return match engine {
            Some(e) => (Input::Manifest, Descriptor::from_toml_value(&e)),
            None => (
                Input::Manifest,
                Err(LauncherError::new(codes::DESCRIPTOR_INVALID)
                    .with("field", "integration.engine")
                    .with("detail", "a manifest without [integration.engine]")),
            ),
        };
    }
    (Input::Toml, Descriptor::from_toml(text))
}

fn pe_x64() -> Vec<u8> {
    let mut b = vec![0u8; 0x80];
    b[..2].copy_from_slice(b"MZ");
    b[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    b[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
    b
}

/// A program's bytes, what its name says it is (Pass B reads its magic): a PE image for `.exe`,
/// an ELF image for any other.
fn program_bytes(name: &str) -> Vec<u8> {
    if name.to_ascii_lowercase().ends_with(".exe") {
        return pe_x64();
    }
    let mut b = vec![0u8; 0x40];
    b[..4].copy_from_slice(b"\x7fELF");
    b[4..7].copy_from_slice(&[2, 1, 1]);
    b
}

/// `d` alone in a set, as an unchecked descriptor (`first_party` as given); the refusals of the
/// set that would not hold it when Pass B does not admit it.
fn alone(d: &Descriptor, first_party: bool) -> Result<EngineSet, Vec<LauncherError>> {
    let origin = Origin {
        moderation: Moderation::Unchecked,
        publisher: "gi-probe".to_owned(),
        first_party,
    };
    let set = EngineSet::build([(origin, Ok(d.clone()))]);
    if set.is_empty() {
        return Err(set.skipped().into_iter().map(|s| s.error.clone()).collect());
    }
    Ok(set)
}

/// Whether `d` speaks for the game in a detection made with it alone.
fn claims(d: &Descriptor, det: &Detection) -> bool {
    det.descriptor_sha256.as_deref() == Some(d.digest())
        || d.codes()
            .any(|(c, _)| det.engine == *c || det.engine_candidates.contains(c))
}

/// The folders' rows and the claims as findings, or the refusals of a set that would not hold
/// the descriptor.
pub type NegativeFolders = Result<(Vec<Value>, Vec<Finding>), Vec<LauncherError>>;

/// What the spec's negative folders make of `d`, asked alone as an unchecked descriptor
/// (`first_party` as given): each laid at every [`places`] of `d` (one folder without a program
/// is laid once: there is nothing to place), and each place detected as the folder search finds
/// it and with Steam's launch hint naming its program. One row per folder (`{folder, claimed,
/// engine, places: [{program, claimed, engine, engine_with_hint}]}`) and a
/// `descriptor-too-broad{reason=negative:<folder>, program}` finding per folder claimed, at the
/// first place that claims it (step unset). `Err` when Pass B does not admit `d`, with the
/// refusals of the set that would not hold it.
pub fn negative_folders(d: &Descriptor, first_party: bool) -> Result<NegativeFolders, Fatal> {
    let set = match alone(d, first_party) {
        Ok(set) => set,
        Err(refused) => return Ok(Err(refused)),
    };
    let places = places(d)?;
    let scratch = Scratch::new("negative")?;
    let detect = |root: &Path, hint: Option<&str>| {
        detect_with_engines(root, hint, &set)
            .map_err(|e| Fatal::io(root, format!("{}: {:?}", e.code, e.params)))
    };
    let mut folders = Vec::new();
    let mut findings = Vec::new();
    for (name, program, beside) in NEGATIVE_FOLDERS {
        let laid = if *program { &places[..] } else { &places[..1] };
        let mut rows = Vec::new();
        let mut claim: Option<(Option<String>, String)> = None;
        let mut first_engine = None;
        for (k, place) in laid.iter().enumerate() {
            let root = scratch.path().join(name).join(k.to_string());
            let dir = place.dir.iter().fold(root.clone(), |p, c| p.join(c));
            std::fs::create_dir_all(&dir).map_err(|e| Fatal::io(&dir, e))?;
            for f in *beside {
                let p = dir.join(f);
                std::fs::write(&p, b"MZ").map_err(|e| Fatal::io(&p, e))?;
            }
            let rel = if *program {
                let p = dir.join(&place.program);
                std::fs::write(&p, program_bytes(&place.program)).map_err(|e| Fatal::io(&p, e))?;
                Some(place.rel())
            } else {
                None
            };
            let walked = detect(&root, None)?;
            let hinted = rel.as_deref().map(|h| detect(&root, Some(h))).transpose()?;
            let by = [Some(&walked), hinted.as_ref()]
                .into_iter()
                .flatten()
                .find(|det| claims(d, det));
            first_engine.get_or_insert_with(|| walked.engine.as_str().to_owned());
            if let Some(det) = by
                && claim.is_none()
            {
                claim = Some((rel.clone(), det.engine.as_str().to_owned()));
            }
            rows.push(json!({
                "program": rel,
                "claimed": by.is_some(),
                "engine": walked.engine.as_str(),
                "engine_with_hint": hinted.as_ref().map(|h| h.engine.as_str()),
            }));
        }
        folders.push(json!({
            "folder": name,
            "claimed": claim.is_some(),
            "engine": claim.as_ref().map(|(_, e)| e.clone()).or(first_engine),
            "places": rows,
        }));
        if let Some((rel, _)) = claim {
            let at = rel.as_deref().unwrap_or("");
            findings.push(
                Finding::new(
                    codes::DESCRIPTOR_TOO_BROAD,
                    format!(
                        "the descriptor claims the negative folder {name:?}, its program laid as {at:?} (spec §5): it would name games it knows nothing of"
                    ),
                )
                .field("rule")
                .with("reason", format!("negative:{name}"))
                .with("program", at),
            );
        }
    }
    Ok(Ok((folders, findings)))
}

/// The label of a recording that names no engine.
const UNLABELLED: &str = "unknown";

/// Step 4: `d`, alone in `set`, asked of every recording in `dir` (see the module doc). The
/// report's `corpus`, and a `descriptor-too-broad{reason=two-engines}` finding (step unset) when
/// the recordings it claims carry two or more engines' labels.
pub fn corpus(d: &Descriptor, set: &EngineSet, dir: &Path) -> Result<(Value, Vec<Finding>), Fatal> {
    let files = replay::recordings_in(dir).map_err(|e| Fatal::io(dir, e))?;
    let scratch = Scratch::new("corpus")?;
    let mut rows = Vec::new();
    let mut engines = BTreeSet::new();
    for (i, file) in files.iter().enumerate() {
        let text = std::fs::read_to_string(file).map_err(|e| Fatal::io(file, e))?;
        let rec = Recording::parse(&text)
            .map_err(|e| Fatal::io(file, format!("not an {} recording: {e}", replay::SCHEMA)))?;
        let root = scratch.path().join(i.to_string());
        std::fs::create_dir(&root).map_err(|e| Fatal::io(&root, e))?;
        replay::materialise(&rec, &root).map_err(|e| Fatal::io(file, e))?;
        let mut claimed = None;
        let hints = std::iter::once(None).chain(rec.hint.as_deref().map(Some));
        for hint in hints {
            let det = detect_with_engines(&root, hint, set)
                .map_err(|e| Fatal::io(file, format!("{}: {:?}", e.code, e.params)))?;
            if claims(d, &det) {
                claimed = Some(det.engine.as_str().to_owned());
                break;
            }
        }
        if claimed.is_some() && rec.label != UNLABELLED {
            engines.insert(rec.label.clone());
        }
        rows.push(json!({
            "recording": file.file_name().map(|n| n.to_string_lossy().into_owned()),
            "label": rec.label,
            "claimed": claimed.is_some(),
            "engine": claimed,
        }));
    }
    let mut findings = Vec::new();
    if engines.len() >= 2 {
        let labels = engines.iter().cloned().collect::<Vec<_>>().join(",");
        findings.push(
            Finding::new(
                codes::DESCRIPTOR_TOO_BROAD,
                format!(
                    "the descriptor claims recordings of {} engines ({labels}) (spec §5): a descriptor names one engine's games",
                    engines.len()
                ),
            )
            .field("rule")
            .with("reason", "two-engines")
            .with("labels", labels),
        );
    }
    let report = json!({
        "dir": dir.display().to_string(),
        "recordings": rows,
        "engines_claimed": engines,
    });
    Ok((report, findings))
}

/// The notice (never a finding) for a descriptor carrying `mods`: reserved for launcher 0.5.1's
/// mods section, which this version accepts and never reads, so nothing here judged it.
pub fn mods_notice(d: &Descriptor) -> Option<Finding> {
    d.carries_mods().then(|| {
        Finding::new(
            MODS_NOT_EVALUATED,
            format!(
                "mods: not evaluated by this version (launcher {}): reserved for launcher 0.5.1's mods section; accepted, part of the digest, and read by no detection, offer or plan",
                astra_game_launcher::VERSION
            ),
        )
        .field("mods")
    })
}

/// [`mods_notice`]'s code.
pub const MODS_NOT_EVALUATED: &str = "mods-not-evaluated";

/// The report's `descriptor`: its digest and codes.
pub fn summary(d: &Descriptor) -> Value {
    json!({
        "sha256": d.digest(),
        "codes": d.codes().map(|(c, n)| (c.as_str().to_owned(), json!(n))).collect::<serde_json::Map<_, _>>(),
    })
}

/// `gi-probe validate-descriptor`.
pub fn run(
    file: &Path,
    first_party: bool,
    corpus_dir: Option<&Path>,
) -> Result<(Value, bool), Fatal> {
    let bytes = std::fs::read(file).map_err(|e| Fatal::io(file, e))?;
    let text = String::from_utf8(bytes).map_err(|_| Fatal::io(file, "not UTF-8"))?;
    let (input, parsed) = parse(&text);
    let mut findings = Vec::new();
    let mut notices = Vec::new();
    let mut folders = Vec::new();
    let mut descriptor = Value::Null;
    let mut corpus_report = None;
    match parsed {
        Err(e) => findings.push(refusal(&e, "not an astra.engine/1 descriptor").step(1)),
        Ok(d) => {
            descriptor = summary(&d);
            notices.extend(mods_notice(&d).map(|n| n.step(1)));
            if let Err(e) = d.check_pass_b(first_party) {
                findings.push(refusal(&e, "Pass B does not admit this descriptor").step(2));
            }
            match negative_folders(&d, first_party)? {
                // Pass B refused it (step 2 says why): no folder can be asked with it.
                Err(skipped) => {
                    for e in skipped {
                        if findings.iter().all(|f| f.code != e.code) {
                            findings.push(refusal(&e, "Pass B does not admit it").step(2));
                        }
                    }
                }
                Ok((rows, claims)) => {
                    folders = rows;
                    findings.extend(claims.into_iter().map(|f| f.step(3)));
                    if let Some(dir) = corpus_dir {
                        let set = alone(&d, first_party).expect("admitted for step 3");
                        let (r, claims) = corpus(&d, &set, dir)?;
                        corpus_report = Some(r);
                        findings.extend(claims.into_iter().map(|f| f.step(4)));
                    }
                }
            }
        }
    }
    let passed = findings.is_empty();
    let rest = json!({
        "input": {
            "file": file.display().to_string(),
            "kind": input.code(),
            "first_party": first_party,
        },
        "spec": spec(),
        "descriptor": descriptor,
        "negative_folders": folders,
        "notices": notices,
    });
    let mut rest = rest;
    if let Some(c) = corpus_report {
        rest["corpus"] = c;
    }
    Ok((report("validate-descriptor", rest, &findings), passed))
}

/// The report's `spec`: the AstraPlugins commit this follows.
pub fn spec() -> Value {
    json!({
        "repo": "mihailinl/AstraPlugins",
        "sha": SPEC_SHA,
        "section": "spec/games/engine-descriptor-v1.md §5",
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use astra_game_launcher::engines::descriptor::limits;

    use super::*;

    /// [`NEUTRAL_NAMES`]' promise, which [`places`] relies on: each of a rule's skips (a
    /// wrapper: three letters at least, matched inside a name; a `skip_dirs` entry: a whole
    /// name) catches one of them at most, and there are more of them than skips. And none is a
    /// name a built-in wrapper rule skips: each alone at the top of a folder is the program the
    /// folder search names.
    #[test]
    fn no_rule_can_skip_every_neutral_name() {
        assert!(NEUTRAL_NAMES.len() > limits::WRAPPERS + limits::SKIP_DIRS);
        assert_eq!(NEUTRAL_NAMES[0], "Game", "the spec's drawing first");
        let mut threes = BTreeSet::new();
        let mut ends = BTreeSet::new();
        for n in NEUTRAL_NAMES {
            let lower = n.to_ascii_lowercase();
            assert!(
                (3..=5).contains(&lower.len()) && lower.bytes().all(|b| b.is_ascii_lowercase()),
                "{n}"
            );
            assert!(
                ends.insert(lower.as_bytes()[lower.len() - 1]),
                "{n}'s last letter"
            );
            for w in lower.as_bytes().windows(3) {
                assert!(
                    threes.insert(w.to_vec()),
                    "{n}: {:?}",
                    String::from_utf8_lossy(w)
                );
            }
        }
        let scratch = Scratch::new("neutral").unwrap();
        for n in NEUTRAL_NAMES {
            let root = scratch.path().join(n);
            std::fs::create_dir(&root).unwrap();
            let program = format!("{n}.exe");
            std::fs::write(root.join(&program), program_bytes(&program)).unwrap();
            let det = astra_game_launcher::detect(&root).unwrap();
            assert_eq!(det.engine, "unknown", "{n}");
            assert_eq!(det.exe.as_deref(), Some(Path::new(&program)), "{n}");
        }
    }

    fn places_of(toml: &str) -> Vec<String> {
        let d = Descriptor::from_toml(toml).unwrap();
        places(&d).unwrap().iter().map(Place::rel).collect()
    }

    const HEAD: &str = "schema = \"astra.engine/1\"\nengine = { codes = { probe = \"Probe\" } }\n";

    fn rule(programs: &str) -> String {
        format!(
            "{HEAD}[[rule]]\ntier = \"strong\"\nprograms = {programs}\nrequire = [ {{ listed = \"probe.dat\", kind = \"file\" }} ]\ncode = \"probe\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{{program}}\"\n"
        )
    }

    /// Each program a rule selects, in the folder it looks in; the spec's drawing first, once.
    #[test]
    fn places_are_where_each_rule_looks() {
        assert_eq!(
            places_of(&rule("{ ext = [\"exe\"], first = 8 }")),
            ["Game.exe"]
        );
        assert_eq!(
            places_of(&rule(
                "{ ext = [\"exe\", \"x86_64\", \"\"], at = \"{project}/bin/win64\", first = 8 }"
            )),
            [
                "Game.exe",
                "Game/bin/win64/Game.exe",
                "Game/bin/win64/Game.x86_64",
                "Game/bin/win64/Game"
            ]
        );
        assert_eq!(
            places_of(&rule("{ suffix = \"-x.x86\", at = \"a/b\", first = 8 }")),
            ["Game.exe", "a/b/Game-x.x86"]
        );
        // `skip_wrappers = false`: a wrapper's name is no skip.
        let mut wrapped =
            rule("{ ext = [\"exe\"], at = \"{project}\", first = 8, skip_wrappers = false }");
        wrapped.insert_str(HEAD.len(), "wrappers = [\"game\", \"koa\"]\n");
        assert_eq!(places_of(&wrapped), ["Game.exe", "Game/Game.exe"]);
        let wrapped = wrapped.replace(", skip_wrappers = false", "");
        assert_eq!(places_of(&wrapped), ["Game.exe", "Cob/Cob.exe"]);
        // Every name it would select holds a wrapper: it selects none, and is laid nowhere.
        let mut dead = rule("{ ext = [\"exe\"], at = \"bin\", first = 8 }");
        dead.insert_str(HEAD.len(), "wrappers = [\".exe\"]\n");
        assert_eq!(places_of(&dead), ["Game.exe"]);
    }
}
