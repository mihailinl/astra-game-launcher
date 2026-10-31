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
//! The report gives the descriptor's digest, `sha256(JCS(descriptor))`, which the catalogue
//! entry's `descriptor_sha256` must equal. The input is the descriptor as TOML or JSON, or a
//! foundation's `astra-gi.toml` holding it as `[integration.engine]`. Matching the descriptor
//! against a corpus of labelled folders (`--corpus`) is a later step's.

use std::path::Path;

use astra_game_launcher::{
    Descriptor, Detection, EngineSet, LauncherError, Moderation, Origin, codes, detect_with_engines,
};
use serde_json::{Value, json};

use crate::{Fatal, Finding, Scratch, report};

/// The AstraPlugins commit whose `spec/games/engine-descriptor-v1.md` §5 this lint follows (PR
/// #117). Re-pinning the spec is this one constant, and whatever the new §5 says.
pub const SPEC_SHA: &str = "d46e44a089387589018f28cfc3403399f7c3b89d";

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

/// Whether `d` speaks for the game in a detection made with it alone.
fn claims(d: &Descriptor, det: &Detection) -> bool {
    det.descriptor_sha256.as_deref() == Some(d.digest())
        || d.codes()
            .any(|(c, _)| det.engine == *c || det.engine_candidates.contains(c))
}

/// `gi-probe validate-descriptor`.
pub fn run(file: &Path, first_party: bool) -> Result<(Value, bool), Fatal> {
    let bytes = std::fs::read(file).map_err(|e| Fatal::io(file, e))?;
    let text = String::from_utf8(bytes).map_err(|_| Fatal::io(file, "not UTF-8"))?;
    let (input, parsed) = parse(&text);
    let mut findings = Vec::new();
    let mut folders = Vec::new();
    let mut descriptor = Value::Null;
    match parsed {
        Err(e) => findings.push(refusal(&e, "not an astra.engine/1 descriptor").step(1)),
        Ok(d) => {
            descriptor = json!({
                "sha256": d.digest(),
                "codes": d.codes().map(|(c, n)| (c.as_str().to_owned(), json!(n))).collect::<serde_json::Map<_, _>>(),
            });
            if let Err(e) = d.check_pass_b(first_party) {
                findings.push(refusal(&e, "Pass B does not admit this descriptor").step(2));
            }
            let origin = Origin {
                moderation: Moderation::Unchecked,
                publisher: "gi-probe".to_owned(),
                first_party,
            };
            let set = EngineSet::build([(origin, Ok(d.clone()))]);
            if set.is_empty() {
                // Pass B refused it (step 2 says why): no folder can be asked with it.
                for s in set.skipped() {
                    if findings.iter().all(|f| f.code != s.error.code) {
                        findings.push(refusal(&s.error, "Pass B does not admit it").step(2));
                    }
                }
            } else {
                let scratch = Scratch::new("negative")?;
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
                    let claimed = claims(&d, &det);
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
                            .step(3)
                            .field("rule")
                            .with("reason", format!("negative:{name}")),
                        );
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
        "spec": {
            "repo": "mihailinl/AstraPlugins",
            "sha": SPEC_SHA,
            "section": "spec/games/engine-descriptor-v1.md §5",
        },
        "descriptor": descriptor,
        "negative_folders": folders,
    });
    Ok((report("validate-descriptor", rest, &findings), passed))
}
