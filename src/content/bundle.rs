// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An embedded `.astrachar` or `.astrapack`, judged whole by the content catalogue's own code
//! (`spec/games/manifest.md` §2.7.3): `ingest_bundle` of AstraPlugins
//! `tools/check-content-goldens.py` over a bundle [`super::zipwalk::walk`] has read
//! (`ingest-v1.md` §3 steps 8, 10, 11 and step 12's every-file-in-a-part rule; step 9 is the
//! server's blocklist, which this gate does not hold, and step 12's parts-mismatch half compares
//! declared part digests, which a game integration does not declare).
//!
//! The reference reads `astrachar.json` as Python reads a JSON object, so this does too: a value
//! is falsy as Python's is (`null`, `false`, `0`, `""`, `[]`, `{}`), `format_version` equals `1`
//! when it is `1`, `1.0` or `true`, and where the reference's own reader raises on a shape rather
//! than refuse it (`"body": "x"`, a `files` list) the answer is [`Failure::Unreadable`], which
//! the gate refuses `content-format{reason=bundle-unreadable}` — never a crash.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use super::zipwalk::GLB_MAGIC;
use super::{
    FormatProblem, MAX_SETTINGS, SETTINGS_NAME, StrictError, gltf_problem_content, problem,
    strict_value, uri_problem,
};

pub(crate) const E_BUNDLE_MANIFEST: &str = "E_BUNDLE_MANIFEST";
pub(crate) const E_BUNDLE_FILE_UNLISTED: &str = "E_BUNDLE_FILE_UNLISTED";
pub(crate) const E_BUNDLE_FILE_DIGEST: &str = "E_BUNDLE_FILE_DIGEST";
pub(crate) const E_CONTENT_FORMAT: &str = "E_CONTENT_FORMAT";
pub(crate) const E_CONTENT_TOO_LARGE: &str = "E_CONTENT_TOO_LARGE";
pub(crate) const E_CONTENT_LICENCE_REFUSED: &str = "E_CONTENT_LICENCE_REFUSED";
pub(crate) const E_CONTENT_LICENCE_CONFLICT: &str = "E_CONTENT_LICENCE_CONFLICT";
pub(crate) const E_CONTENT_LICENCE_UNDECLARED: &str = "E_CONTENT_LICENCE_UNDECLARED";
pub(crate) const E_CONTENT_FILE_OUTSIDE_PARTS: &str = "E_CONTENT_FILE_OUTSIDE_PARTS";

const MANIFEST: &str = "astrachar.json";
const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_BODY: usize = 50 * 1024 * 1024;
const GLB_EXTS: &[&str] = &[".vrm", ".glb", ".vrma"];
const ZIP_SIGNATURES: &[&[u8]] = &[b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"];

/// One refusal of the content contract: its code, the reference's words, the bundle's file it
/// names (when it names one) and, for `E_CONTENT_FORMAT`, the launcher's `reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    pub code: &'static str,
    pub detail: String,
    pub inner: Option<String>,
    pub reason: &'static str,
}

/// Why a bundle is not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    Refused(Refused),
    /// The reference's reader raises on this shape.
    Unreadable(String),
}

type Judged<T> = std::result::Result<T, Failure>;

fn refuse<T>(code: &'static str, detail: impl Into<String>, inner: Option<&str>) -> Judged<T> {
    Err(Failure::Refused(Refused {
        code,
        detail: detail.into(),
        inner: inner.map(str::to_owned),
        reason: "",
    }))
}

fn format<T>(inner: &str, p: FormatProblem) -> Judged<T> {
    Err(Failure::Refused(Refused {
        code: E_CONTENT_FORMAT,
        detail: format!("{inner}: {}", p.detail),
        inner: Some(inner.to_owned()),
        reason: p.reason,
    }))
}

fn raises<T>(what: &str) -> Judged<T> {
    Err(Failure::Unreadable(what.to_owned()))
}

/// Python's truth value of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `v == n` for a JSON value and an integer (`1.0 == 1`, `True == 1`).
fn py_eq_int(v: Option<&Value>, n: u64) -> bool {
    match v {
        Some(Value::Number(x)) => {
            x.as_u64() == Some(n) || (x.is_f64() && x.as_f64() == Some(n as f64))
        }
        Some(Value::Bool(b)) => u64::from(*b) == n,
        _ => false,
    }
}

/// `dict.get(key)`: JSON `null` is Python's `None`.
fn get<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    m.get(key).filter(|v| !v.is_null())
}

/// `(x or {}).get(key)`: nothing when `x` is falsy; a truthy `x` that is no object raises.
fn or_empty_get<'a>(x: Option<&'a Value>, key: &str, what: &str) -> Judged<Option<&'a Value>> {
    match x {
        None => Ok(None),
        Some(v) if !truthy(v) => Ok(None),
        Some(Value::Object(m)) => Ok(get(m, key)),
        Some(_) => raises(&format!("{what} is not an object")),
    }
}

/// A bundle's files, as the walk read them: every file in directory order (folders left out).
pub(crate) struct Files {
    pub list: Vec<(String, Vec<u8>)>,
    index: HashMap<String, usize>,
}

impl Files {
    pub fn new(list: Vec<(String, Vec<u8>)>) -> Files {
        let index = list
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i))
            .collect();
        Files { list, index }
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.index.get(name).map(|&i| self.list[i].1.as_slice())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }
}

/// `is_settings`: the ONE settings file, `<character.dir>/character.toml`.
fn is_settings(p: &str, settings_dir: Option<&Value>) -> Judged<bool> {
    match settings_dir {
        None => Ok(false),
        Some(v) if !truthy(v) => Ok(false),
        Some(Value::String(d)) => Ok(p == format!("{}/{SETTINGS_NAME}", d.trim_end_matches('/'))),
        Some(_) => raises("character.dir is not a string"),
    }
}

/// `pack_kind`: a `.astrapack` is one pack, animations or 2D art.
fn pack_kind(man: &Map<String, Value>) -> Option<&'static str> {
    if man.get("format") != Some(&Value::String("astra.animpack".to_owned())) {
        return None;
    }
    if man.get("kind") == Some(&Value::String("pixel".to_owned())) {
        Some("art")
    } else {
        Some("animations")
    }
}

/// A part: (its kind, its root label).
type PartKey = (String, String);

/// `parts_of` (ingest-v1.md §5): the body, each `animpacks/<id>/`, each accessory's folder, each
/// standalone image (art) and audio file; a pack's every file is one part at the zip root. Every
/// other file is refused when `strict`, else left out. Each part's files are their indexes.
fn parts_of(
    files: &Files,
    body: Option<&Value>,
    settings_dir: Option<&Value>,
    strict: bool,
    pack: Option<&'static str>,
) -> Judged<BTreeMap<PartKey, Vec<usize>>> {
    let mut parts: BTreeMap<PartKey, Vec<usize>> = BTreeMap::new();
    for (i, (p, _)) in files.list.iter().enumerate() {
        if p == MANIFEST {
            continue;
        }
        if let Some(kind) = pack {
            parts
                .entry((kind.to_owned(), String::new()))
                .or_default()
                .push(i);
            continue;
        }
        if is_settings(p, settings_dir)? {
            continue;
        }
        let segs: Vec<&str> = p.split('/').collect();
        let lower = p.to_lowercase();
        let before_last_two = &segs[..segs.len().saturating_sub(2)];
        let key = if body.is_some_and(truthy) && body == Some(&Value::String(p.clone())) {
            ("character", p.clone())
        } else if segs[0] == "animpacks" && segs.len() > 2 {
            ("animations", segs[..2].join("/"))
        } else if before_last_two.contains(&"accessories") {
            let at = segs.iter().position(|s| *s == "accessories").unwrap_or(0);
            ("character", segs[..at + 2].join("/"))
        } else if [".png", ".jpg", ".webp", ".ktx2"]
            .iter()
            .any(|x| lower.ends_with(x))
        {
            ("art", p.clone())
        } else if [".ogg", ".wav", ".mp3", ".flac"]
            .iter()
            .any(|x| lower.ends_with(x))
        {
            ("audio", p.clone())
        } else {
            if strict {
                return refuse(E_CONTENT_FILE_OUTSIDE_PARTS, p.clone(), Some(p));
            }
            continue;
        };
        parts.entry((key.0.to_owned(), key.1)).or_default().push(i);
    }
    Ok(parts)
}

/// Whether `b` begins with the GLB magic.
fn glb(b: &[u8]) -> bool {
    b.starts_with(GLB_MAGIC)
}

/// `glb_truncated`: a GLB whose header length is not the file's, or whose first chunk runs past.
fn glb_truncated(raw: &[u8]) -> bool {
    if raw.len() < 20 {
        return true;
    }
    let total = u64::from(u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]));
    let n = u64::from(u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]));
    total != raw.len() as u64 || 20 + n > raw.len() as u64
}

/// The glTF JSON of a GLB or a `.gltf` as the contract reads it (`glb_json`, `gltf_json`):
/// `Ok(None)` when it is no readable glTF, `Err` when it names a member twice.
fn gltf_json(b: &[u8]) -> Result<Option<Value>, ()> {
    let data = if glb(b) {
        if b.len() < 20 || &b[16..20] != b"JSON" || glb_truncated(b) {
            return Ok(None);
        }
        let n = u32::from_le_bytes([b[12], b[13], b[14], b[15]]) as usize;
        &b[20..20 + n]
    } else {
        b
    };
    match strict_value(data) {
        Ok(v) => Ok(Some(v)),
        Err(StrictError::Duplicate(_)) => Err(()),
        Err(_) => Ok(None),
    }
}

/// `judged`: the verdict runs on content, never on a name — every GLB by its magic, every `.gltf`.
fn judged(p: &str, b: &[u8]) -> bool {
    glb(b) || p.to_lowercase().ends_with(".gltf")
}

/// `name_matches_magic`: the GLB magic under a GLB name and a text glTF under `.gltf`, both ways.
fn name_matches_magic(p: &str, b: &[u8]) -> bool {
    let lower = p.to_lowercase();
    if glb(b) != GLB_EXTS.iter().any(|x| lower.ends_with(x)) {
        return false;
    }
    lower.ends_with(".gltf") || !super::sniff::is_gltf_json(b)
}

/// `is_vrm_body`: glTF binary carrying the `VRM` or `VRMC_vrm` extension.
fn is_vrm_body(raw: &[u8]) -> bool {
    if !glb(raw) {
        return false;
    }
    match gltf_json(raw) {
        Ok(Some(Value::Object(g))) => g
            .get("extensions")
            .and_then(Value::as_object)
            .is_some_and(|e| e.contains_key("VRM") || e.contains_key("VRMC_vrm")),
        _ => false,
    }
}

/// `glb_class`: avatar, animation or other, by the extensions. `set()` of an `extensionsUsed`
/// holding an object or an array raises in the reference.
fn glb_class(b: &[u8]) -> Judged<&'static str> {
    let Ok(Some(Value::Object(g))) = gltf_json(b) else {
        return Ok("other");
    };
    let mut named: Vec<&str> = g
        .get("extensions")
        .and_then(Value::as_object)
        .map(|e| e.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if let Some(Value::Array(used)) = g.get("extensionsUsed") {
        if used.iter().any(|u| u.is_object() || u.is_array()) {
            return raises("extensionsUsed holds an object or an array");
        }
        named.extend(used.iter().filter_map(Value::as_str));
    }
    Ok(if named.iter().any(|n| *n == "VRM" || *n == "VRMC_vrm") {
        "avatar"
    } else if named.contains(&"VRMC_vrm_animation") {
        "animation"
    } else {
        "other"
    })
}

/// `vrm_verdict` over both metas: allowed, prohibited, unknown or conflict.
fn vrm_verdict(b: &[u8]) -> &'static str {
    let Ok(Some(Value::Object(g))) = gltf_json(b) else {
        return "unknown";
    };
    let verdicts: Vec<super::Verdict> = super::vrm_metas(&g)
        .into_iter()
        .map(|(v, m)| super::meta_verdict(v, m).verdict)
        .collect();
    match verdicts.as_slice() {
        [] => "unknown",
        [a, b] if a != b => "conflict",
        [v, ..] => v.as_str(),
    }
}

/// What an accepted bundle is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Accepted {
    /// It is held by the content catalogue (`F_CONTENT_VRM_META_UNKNOWN`): a VRM it holds reads
    /// unknown.
    pub unknown: bool,
    /// Its body (`body.file`) and its bytes, when it has one the bundle holds.
    pub body: Option<(String, Vec<u8>)>,
}

/// `ingest_bundle` (steps 8–12) over a walked bundle: `order` every entry's name, folders
/// included; `licence` whether the game integration's part declares one, which then stands for
/// every part of the bundle.
pub(crate) fn ingest(files: &Files, order: &[String], licence: bool) -> Judged<Accepted> {
    if order.last().map(String::as_str) != Some(MANIFEST) {
        return refuse(
            E_BUNDLE_MANIFEST,
            "astrachar.json is not the last entry",
            None,
        );
    }
    let man_raw = files.get(MANIFEST).unwrap_or_default();
    if man_raw.len() > MAX_MANIFEST {
        return refuse(E_BUNDLE_MANIFEST, "too large", Some(MANIFEST));
    }
    let man = match strict_value(man_raw) {
        Ok(Value::Object(m)) => m,
        Ok(v) => {
            let kind = match v {
                Value::Array(_) => "list",
                Value::String(_) => "str",
                Value::Bool(_) => "bool",
                Value::Null => "NoneType",
                _ => "number",
            };
            return refuse(
                E_BUNDLE_MANIFEST,
                format!("astrachar.json is a JSON {kind}, not an object"),
                Some(MANIFEST),
            );
        }
        Err(_) => return refuse(E_BUNDLE_MANIFEST, "does not parse", Some(MANIFEST)),
    };
    let one_of = |k: &str, values: &[&str]| {
        man.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| values.contains(&s))
    };
    if !one_of("format", &["astra.character", "astra.animpack"])
        || !py_eq_int(man.get("format_version"), 1)
        || !py_eq_int(man.get("schema"), 5)
    {
        return refuse(
            E_BUNDLE_MANIFEST,
            "format, format_version or schema",
            Some(MANIFEST),
        );
    }
    listed_check(files, get(&man, "files"))?;
    let body = or_empty_get(get(&man, "body"), "file", "body")?;
    let character = get(&man, "character");
    let settings_dir = or_empty_get(character, "dir", "character")?;
    let pack = pack_kind(&man);
    let part_root_of: HashMap<usize, String> = parts_of(files, body, settings_dir, false, pack)?
        .into_iter()
        .flat_map(|((_, root), pf)| pf.into_iter().map(move |i| (i, root.clone())))
        .collect();
    let root_of = |name: &str| files.index.get(name).and_then(|i| part_root_of.get(i));

    for (i, (p, b)) in files.list.iter().enumerate() {
        if let Some(why) = super::sniff::bom_refusal(p, b) {
            return format(p, problem("bom", why));
        }
        if ZIP_SIGNATURES.iter().any(|z| b.starts_with(z)) {
            return format(
                p,
                problem("nested-archive", "a nested archive is not a content part"),
            );
        }
        if glb(b) && glb_truncated(b) {
            return format(
                p,
                problem(
                    "truncated",
                    "a GLB whose length or first chunk runs past its end",
                ),
            );
        }
        if judged(p, b) {
            if let Some(why) = gltf_problem_content(b) {
                return format(p, why);
            }
            let doc = match gltf_json(b) {
                Err(()) => {
                    return format(
                        p,
                        problem(
                            "duplicate-member",
                            "a duplicate member name in its glTF JSON",
                        ),
                    );
                }
                Ok(doc) => doc,
            };
            if let Some(Value::Object(doc)) = &doc {
                let own = part_root_of.get(&i);
                if let Some(why) = uri_problem(
                    doc,
                    glb(b),
                    p,
                    &|target| files.contains(target),
                    &|target| root_of(target) == own,
                ) {
                    return format(p, why);
                }
            }
        }
        if !name_matches_magic(p, b) {
            return format(
                p,
                problem("name-magic", "a glTF's name and content disagree"),
            );
        }
    }

    let mut body_of = None;
    if let Some(body) = body {
        let Some(name) = body.as_str() else {
            return raises("body.file is not a string");
        };
        match files.get(name) {
            Some(raw) if name.to_lowercase().ends_with(".vrm") && is_vrm_body(raw) => {
                if raw.len() > MAX_BODY {
                    return refuse(E_CONTENT_TOO_LARGE, name, Some(name));
                }
                body_of = Some((name.to_owned(), raw.to_vec()));
            }
            _ => return format(name, problem("body-not-vrm", "not a VRM")),
        }
    }
    for (p, b) in &files.list {
        if is_settings(p, settings_dir)? {
            let why = if b.len() > MAX_SETTINGS {
                Some("too large".to_owned())
            } else {
                match std::str::from_utf8(b) {
                    Err(e) => Some(e.to_string()),
                    // tomllib reads no byte-order mark; the toml crate skips one
                    Ok(s) if s.starts_with('\u{feff}') => Some("a byte-order mark".to_owned()),
                    Ok(s) => s.parse::<toml::Table>().err().map(|e| e.to_string()),
                }
            };
            if let Some(why) = why {
                return format(
                    p,
                    problem(
                        "settings-invalid",
                        format!("not UTF-8 TOML of at most {MAX_SETTINGS} bytes ({why})"),
                    ),
                );
            }
        }
    }
    let kind_of: HashMap<usize, String> = parts_of(files, body, settings_dir, false, pack)?
        .into_iter()
        .flat_map(|((kind, _), pf)| pf.into_iter().map(move |i| (i, kind.clone())))
        .collect();
    let mut verdicts: Vec<(usize, &'static str)> = Vec::new();
    for (i, (p, b)) in files.list.iter().enumerate() {
        if !judged(p, b) {
            continue;
        }
        let gets = match glb_class(b)? {
            "avatar" => true,
            "animation" => false,
            _ => kind_of.get(&i).map(String::as_str) == Some("character"),
        };
        if gets {
            verdicts.push((i, vrm_verdict(b)));
        }
    }
    for (want, code) in [
        ("conflict", E_CONTENT_LICENCE_CONFLICT),
        ("prohibited", E_CONTENT_LICENCE_REFUSED),
    ] {
        if let Some((i, _)) = verdicts.iter().find(|(_, v)| *v == want) {
            let p = &files.list[*i].0;
            return refuse(code, p.clone(), Some(p));
        }
    }
    // Step 12: every file in exactly one part. A declared licence stands for every part (the
    // reference's `declared_licence`, every root of this same reading).
    let parts = parts_of(files, body, settings_dir, true, pack)?;
    let verdict_of: HashMap<usize, &str> = verdicts.iter().copied().collect();
    for ((_, root), pf) in &parts {
        let has_meta = pf.iter().any(|i| verdict_of.get(i) == Some(&"allowed"));
        if !has_meta && !licence {
            let unknown = pf.iter().any(|i| verdict_of.get(i) == Some(&"unknown"));
            if !unknown {
                return refuse(E_CONTENT_LICENCE_UNDECLARED, root.clone(), None);
            }
        }
    }
    Ok(Accepted {
        unknown: verdicts.iter().any(|(_, v)| *v == "unknown"),
        body: body_of,
    })
}

/// Step 8's listing: every file but the manifest listed in `files` with its digest and size, and
/// every listed file there — read as the reference reads `man.get("files") or {}`, whatever it is.
fn listed_check(files: &Files, listed: Option<&Value>) -> Judged<()> {
    let listed = listed.filter(|v| truthy(v));
    let contains = |p: &str| -> Judged<bool> {
        match listed {
            None => Ok(false),
            Some(Value::Object(m)) => Ok(m.contains_key(p)),
            Some(Value::Array(a)) => Ok(a.iter().any(|x| x.as_str() == Some(p))),
            Some(Value::String(s)) => Ok(s.contains(p)),
            Some(_) => raises("files is not iterable"),
        }
    };
    for (p, b) in &files.list {
        if p == MANIFEST {
            continue;
        }
        if !contains(p)? {
            return refuse(E_BUNDLE_FILE_UNLISTED, p.clone(), Some(p));
        }
        let Some(Value::Object(m)) = listed else {
            return raises("files is indexed by a name");
        };
        let Some(Value::Object(entry)) = m.get(p.as_str()) else {
            return raises("a files entry is not an object");
        };
        let want = format!("sha256:{}", crate::digest::sha256_hex(b));
        if entry.get("sha256").and_then(Value::as_str) != Some(want.as_str())
            || !py_eq_int(entry.get("size"), b.len() as u64)
        {
            return refuse(E_BUNDLE_FILE_DIGEST, p.clone(), Some(p));
        }
    }
    // `for p in listed: if p not in files`: a name, or something that is never one.
    let absent = |p: &str| {
        refuse(
            E_BUNDLE_FILE_UNLISTED,
            format!("{p} listed and absent"),
            Some(p),
        )
    };
    match listed {
        None => {}
        Some(Value::Object(m)) => {
            if let Some(p) = m.keys().find(|k| !files.contains(k)) {
                return absent(p);
            }
        }
        Some(Value::Array(a)) => {
            for x in a {
                match x {
                    Value::String(s) if files.contains(s) => {}
                    Value::String(s) => return absent(s),
                    Value::Object(_) | Value::Array(_) => {
                        return raises("files lists an object or an array");
                    }
                    other => return absent(&other.to_string()),
                }
            }
        }
        Some(Value::String(s)) => {
            if let Some(c) = s.chars().map(String::from).find(|c| !files.contains(c)) {
                return absent(&c);
            }
        }
        Some(_) => return raises("files is not iterable"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_reads_a_json_value_as_this_does() {
        for v in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(&v), "{v}");
        }
        for v in [
            json!(true),
            json!(1),
            json!("x"),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(&v), "{v}");
        }
        assert!(py_eq_int(Some(&json!(1)), 1));
        assert!(py_eq_int(Some(&json!(1.0)), 1));
        assert!(py_eq_int(Some(&json!(true)), 1));
        assert!(!py_eq_int(Some(&json!("1")), 1));
        assert!(!py_eq_int(Some(&json!(1.5)), 1));
        assert!(!py_eq_int(None, 1));
        assert_eq!(or_empty_get(Some(&json!("")), "file", "body"), Ok(None));
        assert!(or_empty_get(Some(&json!("x")), "file", "body").is_err());
        assert_eq!(
            or_empty_get(Some(&json!({"file": "a.vrm"})), "file", "body"),
            Ok(Some(&json!("a.vrm")))
        );
    }
}
