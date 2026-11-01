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
//!    one, through the same [`astra_game_launcher::detect_with_engines`] Astra calls. Any claim
//!    (it names the game, or makes it ambiguous) is `descriptor-too-broad{reason=negative:<folder>}`.
//!
//! 4. with `--corpus <folder>`: it claims no recordings of two or more engines — every
//!    `astra.list-shape/1` recording (`*.json`, as `gi-probe capture` writes them; the format is
//!    `tests/corpus/replay.rs`) in the folder is laid below `TMPDIR` and detected with this
//!    descriptor alone, as in step 3, with no hint and with the recording's own; the labels of
//!    those it claims, `unknown` aside (it names no engine), are the engines it claims.
//!    Two or more is `descriptor-too-broad{reason=two-engines, labels}` (spec §5,
//!    `E_DESCRIPTOR_TOO_BROAD`, which this launcher spells as its own refusal: debt row 13).
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

/// The negative folders of spec §5: `(name, files)`; a file name ending in `/` is a folder.
pub const NEGATIVE_FOLDERS: &[(&str, &[&str])] = &[
    ("empty", &[]),
    ("exe-only", &["Game.exe"]),
    (
        "system-dlls",
        &[
            "Game.exe",
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
    ("steam-api-beside-exe", &["Game.exe", "steam_api64.dll"]),
];

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
/// (`first_party` as given): one row per folder (`{folder, claimed, engine}`) and a
/// `descriptor-too-broad{reason=negative:<folder>}` finding per claim (step unset). `None` when
/// Pass B does not admit `d`, with the refusals of the set that would not hold it.
pub fn negative_folders(d: &Descriptor, first_party: bool) -> Result<NegativeFolders, Fatal> {
    let set = match alone(d, first_party) {
        Ok(set) => set,
        Err(refused) => return Ok(Err(refused)),
    };
    let scratch = Scratch::new("negative")?;
    let mut folders = Vec::new();
    let mut findings = Vec::new();
    for (name, files) in NEGATIVE_FOLDERS {
        let dir = scratch.path().join(name);
        std::fs::create_dir_all(&dir).map_err(|e| Fatal::io(&dir, e))?;
        for f in *files {
            let p = dir.join(f);
            let bytes = if f.ends_with(".exe") {
                pe_x64()
            } else {
                b"MZ".to_vec()
            };
            std::fs::write(&p, bytes).map_err(|e| Fatal::io(&p, e))?;
        }
        let det = detect_with_engines(&dir, None, &set)
            .map_err(|e| Fatal::io(&dir, format!("{}: {:?}", e.code, e.params)))?;
        let claimed = claims(d, &det);
        folders.push(json!({
            "folder": name,
            "claimed": claimed,
            "engine": det.engine.as_str(),
        }));
        if claimed {
            findings.push(
                Finding::new(
                    codes::DESCRIPTOR_TOO_BROAD,
                    format!(
                        "the descriptor claims the negative folder {name:?} (spec §5): it would name games it knows nothing of"
                    ),
                )
                .field("rule")
                .with("reason", format!("negative:{name}")),
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
    let mut folders = Vec::new();
    let mut descriptor = Value::Null;
    let mut corpus_report = None;
    match parsed {
        Err(e) => findings.push(refusal(&e, "not an astra.engine/1 descriptor").step(1)),
        Ok(d) => {
            descriptor = summary(&d);
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
