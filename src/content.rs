// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A game integration's own content: `[[integration.content]]` (AstraPlugins
//! `spec/games/manifest.md` §2.7, at the commit `tests/goldens/astraplugins/PIN` names) — its own
//! animations, characters, art and audio, one entry per file, each confirmed by its author at
//! publication — and what is checked of a release's bytes against it.
//!
//! - [`crate::Manifest::parse`] reads the table (`read_table`). Every entry is
//!   `{path, kind, title, credit, licence?}`: another key is `unknown-key`; a `path` that is empty,
//!   begins with `/`, or holds `..`, `:` (so a `builtin:` reference is never a part) or a NUL, a
//!   `kind` other than `animations` `character` `art` `audio`, an empty `title`, `credit` or
//!   `licence` are `manifest-invalid`; a `path` two entries declare is `content-duplicate`.
//!   Launcher 0.3.1 ignores the table, as every `[integration.*]` one.
//! - [`judge`] holds a release's files to it ([`crate::GiPackage::content`] over a zip; `gi-probe
//!   check`'s step 8), in the spec's order:
//!   1. **coverage.** A *content-typed* file is one whose extension AND first bytes both say so
//!      ([`is_content_typed`]: `.astrapack` `.astrachar` a zip, `.vrma` `.glb` `.vrm` the GLB
//!      magic, `.gltf` JSON text, `.fbx` a binary or ASCII FBX, `.png` `.jpg` `.jpeg` `.webp`
//!      `.gif` and `.ogg` `.wav` `.mp3` `.flac` their signatures); code and loaders never are,
//!      whatever they are called. Each is covered by exactly one entry: `content-undeclared`
//!      names a file no entry covers, `content-missing` an entry no content-typed file is at (a
//!      DLL renamed `climb.fbx` covers nothing);
//!   2. **the format** of every GLB (by its magic, whatever it is called) and `.gltf` part
//!      ([`format_problem`]): the GLB whole (its header's length the file's, its first chunk JSON
//!      and inside the file), its glTF JSON parsing with no member named twice, and no external
//!      reference (a GLB's `images[].uri` and `buffers[].uri` all `data:`; a `.gltf`'s `data:` or
//!      a plain relative path to a file the release holds) — `content-format{reason}`;
//!   3. **the VRM licence gate** ([`vrm_gate`]), on what is judged by its glTF JSON's extensions
//!      and never by its name ([`class_of`]): an avatar (`VRM`, `VRMC_vrm`) in any part, a plain
//!      glTF in a `character` part, and the `.vrm` body of a readable `.astrachar` (its
//!      `astrachar.json` names `body.file`, a `.vrm` it holds; the bundle itself held to what
//!      the release is, every bundle of it within one budget). An animation
//!      (`VRMC_vrm_animation` alone) gets no avatar verdict. Every VRM meta the file carries is
//!      read, each by its own spec's defaults ([`meta_verdict`]; an unrecognised token is
//!      unknown, never allowed; a non-bool `allowRedistribution` is prohibited): prohibited is
//!      `content-licence-refused{meta, value, defaulted}`, two metas that disagree
//!      `content-licence-conflict`, and unknown (no meta, an unrecognised token) the flag
//!      `content-vrm-meta-unknown`, never a refusal. An `.astrachar` that cannot be read so is the
//!      flag `content-astrachar-unreadable`;
//!   4. **a licence for every covered part**: the VRM meta's when the gate read one
//!      (`licence_source` `vrm`), else the entry's own `licence` (`declared`); with neither,
//!      `content-licence-undeclared`.
//!
//! The codes are the spec's `E_CONTENT_*` and `F_CONTENT_*` in this launcher's spelling (merged
//! plan R32: kebab-case wire codes), one for one ([`SPEC_CODES`]). This is an implementation of
//! the rule independent of AstraPlugins' `tools/check-games-goldens.py` and of the catalogue's
//! `lib/content.mjs`, held to the same goldens (`testdata/games/content/`, `tests/content.rs`
//! and, through `gi-probe check`, `tests/gi_probe.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Cursor, Read};

use serde::Serialize;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use zip::ZipArchive;

use crate::digest::sha256_hex;
use crate::error::{LauncherError, Result, codes};
use crate::package::Limits;

/// The key of the table below `[integration]`.
pub const TABLE: &str = "content";

/// A flag (never a refusal): a part the VRM gate judges reads `unknown` — no VRM meta, or a token
/// the gate does not recognise. It passes with its declared licence (`F_CONTENT_VRM_META_UNKNOWN`).
pub const FLAG_VRM_META_UNKNOWN: &str = "content-vrm-meta-unknown";

/// A flag (never a refusal): an `.astrachar` part whose body cannot be read as the spec reads it,
/// so the gate never saw it (`F_CONTENT_ASTRACHAR_UNREADABLE`; `reason` says why).
pub const FLAG_ASTRACHAR_UNREADABLE: &str = "content-astrachar-unreadable";

/// The spec's codes (`manifest.md` §2.7, `vectors.json`) and this launcher's, one for one.
pub const SPEC_CODES: &[(&str, &str)] = &[
    ("E_CONTENT_UNDECLARED", codes::CONTENT_UNDECLARED),
    ("E_CONTENT_MISSING", codes::CONTENT_MISSING),
    ("E_CONTENT_DUPLICATE", codes::CONTENT_DUPLICATE),
    ("E_CONTENT_FORMAT", codes::CONTENT_FORMAT),
    ("E_CONTENT_LICENCE_REFUSED", codes::CONTENT_LICENCE_REFUSED),
    (
        "E_CONTENT_LICENCE_CONFLICT",
        codes::CONTENT_LICENCE_CONFLICT,
    ),
    (
        "E_CONTENT_LICENCE_UNDECLARED",
        codes::CONTENT_LICENCE_UNDECLARED,
    ),
    ("F_CONTENT_VRM_META_UNKNOWN", FLAG_VRM_META_UNKNOWN),
    ("F_CONTENT_ASTRACHAR_UNREADABLE", FLAG_ASTRACHAR_UNREADABLE),
];

/// The launcher's code for the spec's `spec_code`.
pub fn launcher_code(spec_code: &str) -> Option<&'static str> {
    SPEC_CODES
        .iter()
        .find(|(s, _)| *s == spec_code)
        .map(|(_, l)| *l)
}

// ---------------------------------------------------------------------------------------------
// The table.
// ---------------------------------------------------------------------------------------------

/// `kind`: which rights confirmation the part's checkbox carries. The shared asset-rights rule's
/// `command` is not one: a game integration is native code already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Animations,
    Character,
    Art,
    Audio,
}

impl ContentKind {
    pub const ALL: [ContentKind; 4] = [
        ContentKind::Animations,
        ContentKind::Character,
        ContentKind::Art,
        ContentKind::Audio,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ContentKind::Animations => "animations",
            ContentKind::Character => "character",
            ContentKind::Art => "art",
            ContentKind::Audio => "audio",
        }
    }

    fn parse(s: &str) -> Option<ContentKind> {
        ContentKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One `[[integration.content]]` entry: a part, its id its `path` in the release zip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentPart {
    pub path: String,
    pub kind: ContentKind,
    /// Shown beside the part's checkbox.
    pub title: String,
    /// Shown beside the part's checkbox.
    pub credit: String,
    /// An SPDX id or free text, shown and never policed; the part's whole licence when its bytes
    /// carry no readable VRM meta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
}

const ENTRY_KEYS: &[&str] = &["path", "kind", "title", "credit", "licence"];

fn invalid(key: &str, detail: &str) -> LauncherError {
    LauncherError::new(codes::MANIFEST_INVALID)
        .with("key", key)
        .with("detail", detail)
}

/// Whether `path` may name a part: not empty, no leading `/`, no `..`, no `:` and no NUL — the
/// spec's `^(?!/)(?!.*\.\.)[^:\0]+$`, with `..` refused across lines too.
fn is_part_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains("..")
        && !path.contains([':', '\0'])
}

/// `[[integration.content]]` as written: every entry, or the first refusal.
pub(crate) fn read_table(v: &toml::Value) -> Result<Vec<ContentPart>> {
    let name = format!("integration.{TABLE}");
    let toml::Value::Array(items) = v else {
        return Err(invalid(&name, "must be an array of tables"));
    };
    let mut parts: Vec<ContentPart> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let at = format!("{name}[{i}]");
        let toml::Value::Table(t) = item else {
            return Err(invalid(&at, "must be a table"));
        };
        if let Some(k) = t.keys().find(|k| !ENTRY_KEYS.contains(&k.as_str())) {
            return Err(LauncherError::new(codes::UNKNOWN_KEY).with("key", format!("{at}.{k}")));
        }
        let text = |k: &str, required: bool| -> Result<Option<String>> {
            match t.get(k) {
                Some(toml::Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
                Some(toml::Value::String(_)) => Err(invalid(&format!("{at}.{k}"), "empty")),
                Some(_) => Err(invalid(&format!("{at}.{k}"), "must be a string")),
                None if required => Err(invalid(&format!("{at}.{k}"), "missing")),
                None => Ok(None),
            }
        };
        let path = text("path", true)?.unwrap_or_default();
        if !is_part_path(&path) {
            return Err(invalid(
                &format!("{at}.path"),
                "relative, with no `..`, no `:` and no NUL",
            ));
        }
        let kind = text("kind", true)?.unwrap_or_default();
        let kind = ContentKind::parse(&kind).ok_or_else(|| {
            invalid(
                &format!("{at}.kind"),
                "one of animations, character, art, audio",
            )
        })?;
        let title = text("title", true)?.unwrap_or_default();
        let credit = text("credit", true)?.unwrap_or_default();
        let licence = text("licence", false)?;
        if let Some(first) = parts.iter().position(|p| p.path == path) {
            return Err(LauncherError::new(codes::CONTENT_DUPLICATE)
                .with("field", format!("{at}.path"))
                .with("path", path)
                .with("first", format!("{name}[{first}].path")));
        }
        parts.push(ContentPart {
            path,
            kind,
            title,
            credit,
            licence,
        });
    }
    Ok(parts)
}

// ---------------------------------------------------------------------------------------------
// Content-typed files.
// ---------------------------------------------------------------------------------------------

/// How many first bytes [`is_content_typed`] reads.
pub const SNIFF_BYTES: usize = 32;

/// What a content-typed file's first bytes are, by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Magic {
    Zip,
    Glb,
    GltfText,
    Fbx,
    Png,
    Jpeg,
    Webp,
    Gif,
    Ogg,
    Wav,
    Mp3,
    Flac,
}

/// `manifest.md` §2.7.1's table: extension (lower case, with its dot) → magic. Closed: nothing
/// else is ever content.
const EXTENSIONS: &[(&str, Magic)] = &[
    (".astrapack", Magic::Zip),
    (".astrachar", Magic::Zip),
    (".vrma", Magic::Glb),
    (".glb", Magic::Glb),
    (".vrm", Magic::Glb),
    (".gltf", Magic::GltfText),
    (".fbx", Magic::Fbx),
    (".png", Magic::Png),
    (".jpg", Magic::Jpeg),
    (".jpeg", Magic::Jpeg),
    (".webp", Magic::Webp),
    (".gif", Magic::Gif),
    (".ogg", Magic::Ogg),
    (".wav", Magic::Wav),
    (".mp3", Magic::Mp3),
    (".flac", Magic::Flac),
];

/// The extension of the last `/`-separated component of `path`, lower case with its dot, as the
/// spec's checker reads one: from the last `.` that is neither the first nor the last character
/// (`.png` and `cover.` have none).
pub fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn magic_of(path: &str) -> Option<Magic> {
    let ext = extension(path);
    EXTENSIONS.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

/// Whether `path`'s extension is one a content-typed file may have: only those need their first
/// bytes read.
pub fn has_content_extension(path: &str) -> bool {
    magic_of(path).is_some()
}

fn starts(b: &[u8], with: &[u8]) -> bool {
    b.get(..with.len()) == Some(with)
}

fn at(b: &[u8], i: usize, with: &[u8]) -> bool {
    b.get(i..i + with.len()) == Some(with)
}

impl Magic {
    /// Whether `head` (a file's first [`SNIFF_BYTES`]) is this format's, exactly as the spec's
    /// checker reads it.
    fn matches(self, b: &[u8]) -> bool {
        match self {
            Magic::Zip => b.len() >= 4 && starts(b, b"PK") && matches!(b[2], 3 | 5 | 7),
            Magic::Glb => starts(b, b"glTF"),
            Magic::GltfText => {
                let rest = b
                    .iter()
                    .position(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c));
                rest.is_some_and(|i| b[i] == b'{')
            }
            Magic::Fbx => {
                b.len() >= 18 && (starts(b, b"Kaydara FBX Binary") || starts(b, b"; FBX"))
            }
            Magic::Png => starts(b, b"\x89PNG\r\n\x1a\n"),
            Magic::Jpeg => starts(b, b"\xff\xd8\xff"),
            Magic::Webp => starts(b, b"RIFF") && at(b, 8, b"WEBP"),
            Magic::Gif => starts(b, b"GIF87a") || starts(b, b"GIF89a"),
            Magic::Ogg => starts(b, b"OggS"),
            Magic::Wav => starts(b, b"RIFF") && at(b, 8, b"WAVE"),
            Magic::Mp3 => {
                starts(b, b"ID3") || (b.len() >= 2 && b[0] == 0xff && b[1] & 0xe0 == 0xe0)
            }
            Magic::Flac => starts(b, b"fLaC"),
        }
    }
}

/// Whether the file at `path` whose first bytes are `head` is content-typed: its extension AND
/// its magic both say so (only the first [`SNIFF_BYTES`] of `head` are read). The manifest itself
/// never is.
pub fn is_content_typed(path: &str, head: &[u8]) -> bool {
    let head = &head[..head.len().min(SNIFF_BYTES)];
    magic_of(path).is_some_and(|m| m.matches(head))
}

// ---------------------------------------------------------------------------------------------
// glTF JSON, read strictly.
// ---------------------------------------------------------------------------------------------

const DUPLICATE_MEMBER: &str = "the glTF JSON names a member twice: ";

/// A JSON value, refused when an object names a member twice: one reader keeps the first, another
/// the last.
struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> std::result::Result<Value, E> {
        Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> std::result::Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Strict)? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("{DUPLICATE_MEMBER}{key:?}")));
            }
            let v = map.next_value_seed(Strict)?;
            out.insert(key, v);
        }
        Ok(Value::Object(out))
    }
}

/// Why a GLB or a `.gltf` breaks the format rules: `content-format{reason, detail}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatProblem {
    /// `truncated`, `length-mismatch`, `first-chunk-not-json`, `chunk-past-end`, `json-invalid`,
    /// `duplicate-member`, `not-an-object`, `uri-not-a-string`, `external-uri` (a GLB's),
    /// `uri-not-relative`, `uri-not-in-release` (a `.gltf`'s); `zip-inconsistent` and
    /// `astrachar-json-duplicate-member` for an `.astrachar`'s own.
    pub reason: &'static str,
    pub detail: String,
}

fn problem(reason: &'static str, detail: impl Into<String>) -> FormatProblem {
    FormatProblem {
        reason,
        detail: detail.into(),
    }
}

/// `bytes` as one JSON object, no member named twice.
fn strict_object(bytes: &[u8]) -> std::result::Result<Map<String, Value>, FormatProblem> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let v = Strict
        .deserialize(&mut de)
        .and_then(|v| de.end().map(|()| v));
    match v {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(problem("not-an-object", "the glTF JSON is not an object")),
        Err(e)
            if e.classify() == serde_json::error::Category::Data
                && e.to_string().starts_with(DUPLICATE_MEMBER) =>
        {
            Err(problem("duplicate-member", e.to_string()))
        }
        Err(e) => Err(problem(
            "json-invalid",
            format!("the glTF JSON does not parse: {e}"),
        )),
    }
}

fn u32_le(b: &[u8], i: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]))
}

/// A GLB's glTF JSON, the container whole: its header's total length is the file's, its first
/// chunk is JSON and ends inside the file, and the JSON is one object naming no member twice.
fn glb_json(raw: &[u8]) -> std::result::Result<Map<String, Value>, FormatProblem> {
    if raw.len() < 20 || !starts(raw, b"glTF") {
        return Err(problem(
            "truncated",
            "not a GLB: shorter than its header and first chunk's",
        ));
    }
    let total = u32_le(raw, 8);
    if total != raw.len() as u64 {
        return Err(problem(
            "length-mismatch",
            format!(
                "the GLB header says {total} bytes and the file has {}",
                raw.len()
            ),
        ));
    }
    if &raw[16..20] != b"JSON" {
        return Err(problem(
            "first-chunk-not-json",
            "the GLB's first chunk is not JSON",
        ));
    }
    let len = u32_le(raw, 12);
    if 20 + len > raw.len() as u64 {
        return Err(problem(
            "chunk-past-end",
            "the GLB's JSON chunk runs past the end of the file",
        ));
    }
    strict_object(&raw[20..20 + len as usize])
}

/// Whether `u` begins with a URI scheme (`[A-Za-z][A-Za-z0-9+.-]*:`).
fn has_scheme(u: &str) -> bool {
    let b = u.as_bytes();
    if !b.first().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    for &c in &b[1..] {
        if c == b':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-')) {
            return false;
        }
    }
    false
}

/// `uri` relative to the folder of `path` (which holds no `..`), `.` and empty components dropped.
fn resolve(path: &str, uri: &str) -> String {
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let joined = if dir.is_empty() {
        uri.to_owned()
    } else {
        format!("{dir}/{uri}")
    };
    let parts: Vec<&str> = joined
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

/// No external reference: in a GLB every `images[].uri` and `buffers[].uri` is a `data:` URI; in
/// a `.gltf`, a `data:` URI or a plain relative path (no scheme, no `..`, no leading `/`, no
/// backslash) naming a file of `present` (when given, with the part's `path`).
fn uri_problem(
    doc: &Map<String, Value>,
    is_glb: bool,
    path: Option<&str>,
    present: Option<&BTreeSet<String>>,
) -> Option<FormatProblem> {
    for key in ["images", "buffers"] {
        let Some(Value::Array(items)) = doc.get(key) else {
            continue;
        };
        for item in items {
            let Some(u) = item.as_object().and_then(|o| o.get("uri")) else {
                continue;
            };
            let Some(u) = u.as_str() else {
                return Some(problem(
                    "uri-not-a-string",
                    format!("a {key} uri that is not a string: {u}"),
                ));
            };
            if u.starts_with("data:") {
                continue;
            }
            if is_glb {
                return Some(problem(
                    "external-uri",
                    format!("a GLB names an external uri {u:?}"),
                ));
            }
            if has_scheme(u)
                || u.starts_with('/')
                || u.contains('\\')
                || format!("/{u}/").contains("/../")
                || u.is_empty()
                || u == "."
            {
                return Some(problem(
                    "uri-not-relative",
                    format!("a uri that is not a plain relative path: {u:?}"),
                ));
            }
            if let (Some(present), Some(path)) = (present, path)
                && !present.contains(&resolve(path, u))
            {
                return Some(problem(
                    "uri-not-in-release",
                    format!("a uri naming a file the release does not hold: {u:?}"),
                ));
            }
        }
    }
    None
}

/// Whether `path` names a `.gltf` (by its extension, any case).
fn is_gltf_path(path: Option<&str>) -> bool {
    path.is_some_and(|p| extension(p) == ".gltf")
}

/// The glTF JSON of a GLB (by its magic, whatever it is called) or of a `.gltf` (`path`), held
/// to the format rules: `None` for any other file, else the document or why it breaks them.
pub fn gltf_document(
    raw: &[u8],
    path: Option<&str>,
    present: Option<&BTreeSet<String>>,
) -> Option<std::result::Result<Map<String, Value>, FormatProblem>> {
    let is_glb = starts(raw, b"glTF");
    if !is_glb && !is_gltf_path(path) {
        return None;
    }
    let doc = if is_glb {
        glb_json(raw)
    } else {
        strict_object(raw)
    };
    Some(
        doc.and_then(|d| match uri_problem(&d, is_glb, path, present) {
            Some(p) => Err(p),
            None => Ok(d),
        }),
    )
}

/// Why `raw` (a GLB by its magic, or the `.gltf` at `path`) breaks the format rules, if it does:
/// what every such part is held to before it is classified.
pub fn format_problem(
    raw: &[u8],
    path: Option<&str>,
    present: Option<&BTreeSet<String>>,
) -> Option<FormatProblem> {
    gltf_document(raw, path, present).and_then(|d| d.err())
}

// ---------------------------------------------------------------------------------------------
// The VRM licence gate.
// ---------------------------------------------------------------------------------------------

/// What a glTF is, by the extensions its JSON names in `extensions` or `extensionsUsed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GltfClass {
    /// `VRM` or `VRMC_vrm`: the avatar verdict, in any part.
    Avatar,
    /// `VRMC_vrm_animation` and no avatar extension: the `animations` rights, no avatar verdict.
    Animation,
    /// Neither: judged only in a `character` part.
    Plain,
}

/// A glTF document's [`GltfClass`] — never its file's name.
pub fn class_of(doc: &Map<String, Value>) -> GltfClass {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    if let Some(Value::Object(e)) = doc.get("extensions") {
        names.extend(e.keys().map(String::as_str));
    }
    if let Some(Value::Array(used)) = doc.get("extensionsUsed") {
        names.extend(used.iter().filter_map(Value::as_str));
    }
    if names.contains("VRM") || names.contains("VRMC_vrm") {
        GltfClass::Avatar
    } else if names.contains("VRMC_vrm_animation") {
        GltfClass::Animation
    } else {
        GltfClass::Plain
    }
}

/// Which spec a VRM meta is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrmVersion {
    /// `extensions.VRM.meta`.
    V0,
    /// `extensions.VRMC_vrm.meta`.
    V1,
}

impl VrmVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            VrmVersion::V0 => "0.x",
            VrmVersion::V1 => "1.0",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            VrmVersion::V0 => "VRM",
            VrmVersion::V1 => "VRMC_vrm",
        }
    }

    /// The gate's two fields, the redistribution one first.
    fn gate_fields(self) -> [&'static str; 2] {
        match self {
            VrmVersion::V0 => ["licenseName", "allowedUserName"],
            VrmVersion::V1 => ["allowRedistribution", "avatarPermission"],
        }
    }

    /// The usage flags kept with the part for display (G5), never judged.
    fn usage_fields(self) -> &'static [&'static str] {
        match self {
            VrmVersion::V0 => &[
                "violentUssageName",
                "sexualUssageName",
                "commercialUssageName",
            ],
            VrmVersion::V1 => &[
                "allowExcessivelyViolentUsage",
                "allowExcessivelySexualUsage",
                "allowPoliticalOrReligiousUsage",
                "allowAntisocialOrHateUsage",
            ],
        }
    }
}

/// Every VRM meta `doc` carries, 0.x first: both when it carries both.
pub fn vrm_metas(doc: &Map<String, Value>) -> Vec<(VrmVersion, &Map<String, Value>)> {
    let Some(Value::Object(exts)) = doc.get("extensions") else {
        return Vec::new();
    };
    [VrmVersion::V0, VrmVersion::V1]
        .into_iter()
        .filter_map(|v| {
            exts.get(v.extension())
                .and_then(Value::as_object)
                .and_then(|e| e.get("meta"))
                .and_then(Value::as_object)
                .map(|m| (v, m))
        })
        .collect()
}

/// One reading, or a meta's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allowed,
    Prohibited,
    Unknown,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Allowed => "allowed",
            Verdict::Prohibited => "prohibited",
            Verdict::Unknown => "unknown",
        }
    }
}

/// The reading that decided a prohibited or unknown verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Deciding {
    /// `extensions.VRM.meta.licenseName`, …
    pub meta: String,
    /// Its value as the file has it (`null` when absent).
    pub value: Value,
    /// Absent, so its spec's default decided.
    pub defaulted: bool,
}

/// One meta's verdict, and the reading that decided it (none when allowed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetaVerdict {
    pub version: &'static str,
    pub verdict: Verdict,
    pub deciding: Option<Deciding>,
}

/// A permission or licence token: absent or not a string is prohibited (the spec's default),
/// `allowed(token)` allowed, one of `prohibited` prohibited, any other token unknown.
fn reading(value: Option<&Value>, allowed: fn(&str) -> bool, prohibited: &[&str]) -> Verdict {
    match value.and_then(Value::as_str) {
        None => Verdict::Prohibited,
        Some(s) if allowed(s) => Verdict::Allowed,
        Some(s) if prohibited.contains(&s) => Verdict::Prohibited,
        Some(_) => Verdict::Unknown,
    }
}

/// A meta's verdict, each field by its own spec's default (astra-companion `src/vrm/meta.rs`,
/// the content contract's `ingest-v1.md` §4.2):
///
/// | | value | reading |
/// |---|---|---|
/// | 1.0 `allowRedistribution` | `true` | allowed |
/// | | `false`, absent, not a bool (`"true"` included) | prohibited |
/// | 0.x `licenseName` | `CC0`, any `CC_…` | allowed |
/// | | `Redistribution_Prohibited`, absent, not a string | prohibited |
/// | | `Other`, any other token | unknown |
/// | 1.0 `avatarPermission` / 0.x `allowedUserName` | `everyone` / `Everyone` | allowed |
/// | | `onlyAuthor`, `onlySeparatelyLicensedPerson` / `OnlyAuthor`, `ExplicitlyLicensedPerson`, absent, not a string | prohibited |
/// | | any other token | unknown |
///
/// Prohibited if either reading is, else unknown if either is, else allowed; the deciding reading
/// is the first of that verdict, the redistribution field first.
pub fn meta_verdict(version: VrmVersion, meta: &Map<String, Value>) -> MetaVerdict {
    let [redistribution, permission] = version.gate_fields();
    let readings = match version {
        VrmVersion::V0 => [
            reading(
                meta.get(redistribution),
                |v| v == "CC0" || v.starts_with("CC_"),
                &["Redistribution_Prohibited"],
            ),
            reading(
                meta.get(permission),
                |v| v == "Everyone",
                &["OnlyAuthor", "ExplicitlyLicensedPerson"],
            ),
        ],
        VrmVersion::V1 => [
            if meta.get(redistribution) == Some(&Value::Bool(true)) {
                Verdict::Allowed
            } else {
                Verdict::Prohibited
            },
            reading(
                meta.get(permission),
                |v| v == "everyone",
                &["onlyAuthor", "onlySeparatelyLicensedPerson"],
            ),
        ],
    };
    for want in [Verdict::Prohibited, Verdict::Unknown] {
        if let Some(i) = readings.iter().position(|r| *r == want) {
            let key = [redistribution, permission][i];
            return MetaVerdict {
                version: version.as_str(),
                verdict: want,
                deciding: Some(Deciding {
                    meta: format!("extensions.{}.meta.{key}", version.extension()),
                    value: meta.get(key).cloned().unwrap_or(Value::Null),
                    defaulted: !meta.contains_key(key),
                }),
            };
        }
    }
    MetaVerdict {
        version: version.as_str(),
        verdict: Verdict::Allowed,
        deciding: None,
    }
}

/// The licence the gate read, kept with the part (`vrm_licence` in a catalogue entry): the meta's
/// gate fields and usage flags as the file carries them, no default filled in. When both metas
/// are there and agree, the 1.0 one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VrmLicence {
    pub vrm_version: &'static str,
    pub fields: Map<String, Value>,
}

/// The gate's answer for one glTF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// `content-format`: the file breaks the format rules, so nothing in it is believed.
    Format(FormatProblem),
    Allowed(VrmLicence),
    /// `content-licence-refused`.
    Refused {
        licence: VrmLicence,
        deciding: Deciding,
    },
    /// `content-licence-conflict`: two metas, two verdicts (an unknown against an allowed
    /// included).
    Conflict {
        vrm0: MetaVerdict,
        vrm1: MetaVerdict,
    },
    /// No VRM meta (`None`), or one whose verdict is unknown (the reading that decided): the flag
    /// `content-vrm-meta-unknown`, never a refusal.
    Unknown(Option<Deciding>),
}

/// The gate over an already-read glTF document.
fn gate_of(doc: &Map<String, Value>) -> Gate {
    let metas = vrm_metas(doc);
    if metas.is_empty() {
        return Gate::Unknown(None);
    }
    if let [(v0, m0), (v1, m1)] = metas.as_slice() {
        let (a, b) = (meta_verdict(*v0, m0), meta_verdict(*v1, m1));
        if a.verdict != b.verdict {
            return Gate::Conflict { vrm0: a, vrm1: b };
        }
    }
    // One meta, or two that agree: the 1.0 one is shown.
    let (version, meta) = *metas.last().expect("one at least");
    let verdict = meta_verdict(version, meta);
    let fields = version
        .gate_fields()
        .iter()
        .chain(version.usage_fields())
        .filter_map(|k| meta.get(*k).map(|v| ((*k).to_owned(), v.clone())))
        .collect();
    let licence = VrmLicence {
        vrm_version: version.as_str(),
        fields,
    };
    match (verdict.verdict, verdict.deciding) {
        (Verdict::Allowed, _) => Gate::Allowed(licence),
        (Verdict::Prohibited, Some(deciding)) => Gate::Refused { licence, deciding },
        (_, deciding) => Gate::Unknown(deciding),
    }
}

/// The VRM licence gate over `raw` (a GLB by its magic; a `.gltf` when `path` names one): the
/// format rules first (`present`, the release's files, for a `.gltf`'s relative URIs), then every
/// VRM meta it carries. Any other file reads [`Gate::Unknown`] with nothing decided.
pub fn vrm_gate(raw: &[u8], path: Option<&str>, present: Option<&BTreeSet<String>>) -> Gate {
    match gltf_document(raw, path, present) {
        None => Gate::Unknown(None),
        Some(Err(p)) => Gate::Format(p),
        Some(Ok(doc)) => gate_of(&doc),
    }
}

// ---------------------------------------------------------------------------------------------
// `.astrachar` bundles.
// ---------------------------------------------------------------------------------------------

/// The largest `astrachar.json` read (astra-companion's own cap).
const ASTRACHAR_JSON_BYTES: u64 = 1024 * 1024;

/// What an `.astrachar`'s body is to the gate.
enum Body {
    /// Its `astrachar.json` names `body.file`, a `.vrm` the bundle holds: its name and bytes.
    Readable(String, Vec<u8>),
    /// Not readable as the spec reads a bundle (`reason`): the flag.
    Unreadable(&'static str),
    /// The bundle two readers read as two (or its manifest names a member twice): what is
    /// inside is never believed, so it is refused.
    Format(FormatProblem),
}

fn read_inner(ar: &mut ZipArchive<Cursor<&[u8]>>, name: &str, most: u64) -> Option<Vec<u8>> {
    let f = ar.by_name(name).ok()?;
    let size = f.size();
    if size > most {
        return None;
    }
    let mut out = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
    f.take(size.saturating_add(1)).read_to_end(&mut out).ok()?;
    (out.len() as u64 == size).then_some(out)
}

/// The body of the `.astrachar` `raw` (astra-companion `docs/character-files-and-2d-editor.md`
/// §1.3: `astrachar.json`'s `body` is `{"file": "models/<name>.vrm", …}`), read within `budget`,
/// which every bundle of one release shares. The bundle is first held to what the release zip
/// around it is ([`crate::zip_consistency::check_nested`]): one that two readers read as two, that
/// holds an entry no reader may read, or that would take more than the budget left is never
/// believed (`zip-inconsistent`, `zip-unsafe-entry`, `too-large`).
fn astrachar_body(raw: &[u8], budget: &mut crate::zip_consistency::Budget) -> Body {
    let Ok(mut ar) = ZipArchive::new(Cursor::new(raw)) else {
        return Body::Unreadable("not-a-zip");
    };
    if ar.len() > Limits::default().max_entries {
        return Body::Unreadable("too-many-entries");
    }
    if let Err(e) = crate::zip_consistency::check_nested(&mut ar, raw, budget) {
        let (reason, what) = match e.code {
            codes::ZIP_UNSAFE_ENTRY => ("zip-unsafe-entry", "holds an entry no reader may read"),
            codes::TOO_LARGE => ("too-large", "unpacks to more than the release may"),
            _ => ("zip-inconsistent", "reads as two archives"),
        };
        return Body::Format(problem(
            reason,
            format!(
                "the bundle {what} ({})",
                e.param("reason").or(e.param("entry")).unwrap_or("headers")
            ),
        ));
    }
    let Some(json) = read_inner(&mut ar, "astrachar.json", ASTRACHAR_JSON_BYTES) else {
        return Body::Unreadable("no-astrachar-json");
    };
    let manifest = match strict_object(&json) {
        Ok(m) => m,
        Err(p) if p.reason == "duplicate-member" => {
            return Body::Format(problem(
                "astrachar-json-duplicate-member",
                p.detail.replacen("glTF", "astrachar", 1),
            ));
        }
        Err(_) => return Body::Unreadable("astrachar-json-invalid"),
    };
    let Some(file) = manifest
        .get("body")
        .and_then(Value::as_object)
        .and_then(|b| b.get("file"))
        .and_then(Value::as_str)
    else {
        return Body::Unreadable("no-file-body");
    };
    if extension(file) != ".vrm" {
        return Body::Unreadable("body-not-vrm");
    }
    let Some(size) = ar.by_name(file).ok().map(|f| f.size()) else {
        return Body::Unreadable("body-missing");
    };
    if budget.spend(size, file).is_err() {
        return Body::Format(problem(
            "too-large",
            format!("the bundle's body {file} unpacks to more than the release may"),
        ));
    }
    let Some(bytes) = read_inner(&mut ar, file, size) else {
        return Body::Unreadable("body-missing");
    };
    if !starts(&bytes, b"glTF") {
        return Body::Unreadable("body-not-glb");
    }
    Body::Readable(file.to_owned(), bytes)
}

// ---------------------------------------------------------------------------------------------
// A release.
// ---------------------------------------------------------------------------------------------

/// Where a covered part's licence comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenceSource {
    /// The gate read a VRM meta (allowed or refused): `vrm_licence`.
    Vrm,
    /// The entry's own `licence` is the part's whole licence.
    Declared,
}

/// One covered part as a catalogue entry's `content[]` carries it (`games-index-v1.md` §2.1), its
/// JSON exactly that schema's `contentPart` members, whatever was refused about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PartReport {
    pub path: String,
    pub kind: ContentKind,
    pub title: String,
    pub credit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    /// `None` is `content-licence-undeclared`.
    pub licence_source: Option<LicenceSource>,
    pub sha256: String,
    /// What the glTF's extensions make it, when it is a glTF (an `.astrachar`: its body's). Not
    /// a `contentPart` member, so never in the part's JSON: [`Report::classes`] says it beside
    /// the parts.
    #[serde(skip)]
    pub gltf: Option<GltfClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrm_licence: Option<VrmLicence>,
}

/// A flag: noted for whoever publishes the release, never a refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Flag {
    pub code: &'static str,
    pub params: BTreeMap<String, String>,
}

/// [`judge`]'s answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Every content-typed file of the release, sorted.
    pub content_files: Vec<String>,
    /// Each entry a content-typed file covers, in the manifest's order.
    pub parts: Vec<PartReport>,
    /// Every refusal, in order: the files no entry covers, then each entry's.
    pub refusals: Vec<LauncherError>,
    pub flags: Vec<Flag>,
}

impl Report {
    /// What each part that is a glTF is (its path, its class), for a report beside the parts: a
    /// catalogue entry's `content[]` has no member for it.
    pub fn classes(&self) -> BTreeMap<String, GltfClass> {
        self.parts
            .iter()
            .filter_map(|p| p.gltf.map(|c| (p.path.clone(), c)))
            .collect()
    }
}

fn json_text(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn with_deciding(mut params: BTreeMap<String, String>, d: &Deciding) -> BTreeMap<String, String> {
    params.insert("meta".to_owned(), d.meta.clone());
    params.insert("value".to_owned(), json_text(&d.value));
    params.insert("defaulted".to_owned(), d.defaulted.to_string());
    params
}

fn refusal(code: &'static str, params: &BTreeMap<String, String>) -> LauncherError {
    let mut e = LauncherError::new(code);
    e.params = params.clone();
    e
}

/// Holds a release's files to its `[[integration.content]]` (the module's four steps).
/// `found` is every content-typed file of the release ([`is_content_typed`]); `present` every file
/// it holds (what a `.gltf`'s relative URI may name); `max_inflated` what every `.astrachar`
/// bundle of it together may unpack to (`Limits::max_uncompressed_bytes`); `read` gives a file's
/// bytes, asked once for each covered part.
pub fn judge(
    parts: &[ContentPart],
    found: &BTreeSet<String>,
    present: &BTreeSet<String>,
    max_inflated: u64,
    mut read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Report> {
    let mut budget = crate::zip_consistency::Budget::new(max_inflated);
    let table = format!("integration.{TABLE}");
    let mut r = Report {
        content_files: found.iter().cloned().collect(),
        ..Report::default()
    };
    let declared: BTreeSet<&str> = parts.iter().map(|p| p.path.as_str()).collect();
    for f in found.iter().filter(|f| !declared.contains(f.as_str())) {
        r.refusals.push(
            LauncherError::new(codes::CONTENT_UNDECLARED)
                .with("field", table.clone())
                .with("path", f.clone()),
        );
    }
    for (i, part) in parts.iter().enumerate() {
        // Two entries of one path are refused at parse; the first is the one judged.
        if parts[..i].iter().any(|p| p.path == part.path) {
            continue;
        }
        let field = format!("{table}[{i}]");
        if !found.contains(&part.path) {
            r.refusals.push(
                LauncherError::new(codes::CONTENT_MISSING)
                    .with("field", format!("{field}.path"))
                    .with("path", part.path.clone()),
            );
            continue;
        }
        let raw = read(&part.path)?;
        let mut base = BTreeMap::new();
        base.insert("field".to_owned(), field.clone());
        base.insert("path".to_owned(), part.path.clone());

        // What the gate is asked about this part: (the gate, the bundle's body when it is one).
        let mut gltf = None;
        let mut gate: Option<(Gate, Option<String>)> = None;
        if extension(&part.path) == ".astrachar" {
            match astrachar_body(&raw, &mut budget) {
                Body::Readable(name, body) => {
                    if let Some(Ok(doc)) = gltf_document(&body, Some(&name), None) {
                        gltf = Some(class_of(&doc));
                    }
                    gate = Some((vrm_gate(&body, Some(&name), None), Some(name)));
                }
                Body::Unreadable(reason) => {
                    let mut params = base.clone();
                    params.insert("reason".to_owned(), reason.to_owned());
                    r.flags.push(Flag {
                        code: FLAG_ASTRACHAR_UNREADABLE,
                        params,
                    });
                }
                Body::Format(p) => gate = Some((Gate::Format(p), None)),
            }
        } else {
            match gltf_document(&raw, Some(&part.path), Some(present)) {
                None => {}
                Some(Err(p)) => gate = Some((Gate::Format(p), None)),
                Some(Ok(doc)) => {
                    let class = class_of(&doc);
                    gltf = Some(class);
                    if class == GltfClass::Avatar
                        || (class == GltfClass::Plain && part.kind == ContentKind::Character)
                    {
                        gate = Some((gate_of(&doc), None));
                    }
                }
            }
        }

        let mut vrm_licence = None;
        if let Some((gate, body)) = gate {
            let mut params = base.clone();
            if let Some(body) = body {
                params.insert("body".to_owned(), body);
            }
            match gate {
                Gate::Format(p) => {
                    params.insert("reason".to_owned(), p.reason.to_owned());
                    params.insert("detail".to_owned(), p.detail);
                    r.refusals.push(refusal(codes::CONTENT_FORMAT, &params));
                }
                Gate::Allowed(licence) => vrm_licence = Some(licence),
                Gate::Refused { licence, deciding } => {
                    vrm_licence = Some(licence);
                    let params = with_deciding(params, &deciding);
                    r.refusals
                        .push(refusal(codes::CONTENT_LICENCE_REFUSED, &params));
                }
                Gate::Conflict { vrm0, vrm1 } => {
                    for m in [&vrm0, &vrm1] {
                        let side = if m.version == "0.x" { "vrm0" } else { "vrm1" };
                        params.insert(side.to_owned(), m.verdict.as_str().to_owned());
                        if let Some(d) = &m.deciding {
                            params.insert(format!("{side}_meta"), d.meta.clone());
                        }
                    }
                    r.refusals
                        .push(refusal(codes::CONTENT_LICENCE_CONFLICT, &params));
                }
                Gate::Unknown(deciding) => {
                    let params = match &deciding {
                        Some(d) => with_deciding(params, d),
                        None => params,
                    };
                    r.flags.push(Flag {
                        code: FLAG_VRM_META_UNKNOWN,
                        params,
                    });
                }
            }
        }

        let licence_source = if vrm_licence.is_some() {
            Some(LicenceSource::Vrm)
        } else if part.licence.is_some() {
            Some(LicenceSource::Declared)
        } else {
            r.refusals.push(
                LauncherError::new(codes::CONTENT_LICENCE_UNDECLARED)
                    .with("field", format!("{field}.licence"))
                    .with("path", part.path.clone()),
            );
            None
        };
        r.parts.push(PartReport {
            path: part.path.clone(),
            kind: part.kind,
            title: part.title.clone(),
            credit: part.credit.clone(),
            licence: part.licence.clone(),
            licence_source,
            sha256: sha256_hex(&raw),
            gltf,
            vrm_licence,
        });
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_read_as_the_specs_checker_reads_them() {
        for (path, want) in [
            ("a/b/Climb.FBX", ".fbx"),
            ("cover.png", ".png"),
            (".png", ""),
            ("cover.", ""),
            ("a.b/c", ""),
            ("x.tar.gz", ".gz"),
            ("noext", ""),
        ] {
            assert_eq!(extension(path), want, "{path}");
        }
    }

    #[test]
    fn a_file_is_content_only_when_its_extension_and_magic_both_say_so() {
        assert!(is_content_typed("a/x.png", b"\x89PNG\r\n\x1a\n...."));
        assert!(!is_content_typed("a/x.png", b"MZ\x90\0"));
        // A loader DLL named as an animation is code, never content.
        assert!(!is_content_typed(
            "climb.fbx",
            b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0\0\xb8\0"
        ));
        assert!(is_content_typed("climb.fbx", b"Kaydara FBX Binary  \0"));
        // The ASCII header is read only at 18 bytes and more, as the spec's checker reads it.
        assert!(!is_content_typed("climb.fbx", b"; FBX 7.4"));
        assert!(is_content_typed("climb.fbx", b"; FBX 7.4.0 project file"));
        assert!(is_content_typed("m.GLTF", b"\n  {\"asset\":{}}"));
        assert!(!is_content_typed("m.gltf", b"[1]"));
        assert!(is_content_typed("p.astrapack", b"PK\x03\x04"));
        assert!(is_content_typed("p.astrapack", b"PK\x05\x06"));
        assert!(!is_content_typed("p.astrapack", b"PK\x01\x02"));
        assert!(is_content_typed("w.webp", b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_content_typed("w.webp", b"RIFF\0\0\0\0WAVEfmt "));
        assert!(is_content_typed("w.wav", b"RIFF\0\0\0\0WAVEfmt "));
        assert!(is_content_typed("s.mp3", b"\xff\xfb\x90\x00"));
        assert!(is_content_typed("s.mp3", b"ID3\x04"));
        assert!(!is_content_typed("s.mp3", b"\xff\x1b"));
        assert!(!is_content_typed("plugin.dll", b"\x89PNG\r\n\x1a\n"));
        assert!(!is_content_typed("astra-gi.toml", b"schema = 1"));
        // Only the first 32 bytes are read: a brace behind 32 spaces is not seen.
        let mut late = vec![b' '; SNIFF_BYTES];
        late.push(b'{');
        assert!(!is_content_typed("m.gltf", &late));
    }

    #[test]
    fn a_uri_is_resolved_beside_its_gltf() {
        assert_eq!(resolve("models/prop.gltf", "prop.bin"), "models/prop.bin");
        assert_eq!(
            resolve("models/prop.gltf", "./tex/a.png"),
            "models/tex/a.png"
        );
        assert_eq!(resolve("prop.gltf", "a//b.bin"), "a/b.bin");
        assert!(has_scheme("https://x"));
        assert!(has_scheme("c:x"));
        assert!(!has_scheme("1x:y"));
        assert!(!has_scheme("a/b:c"));
    }

    #[test]
    fn strict_json_refuses_a_member_named_twice_at_any_depth() {
        assert!(strict_object(br#"{"a":{"b":1,"c":[{"d":1}]}}"#).is_ok());
        for twice in [
            &br#"{"a":1,"a":2}"#[..],
            br#"{"x":{"b":false,"b":true}}"#,
            br#"{"x":[{"k":1,"k":1}]}"#,
            br#"{"a":1,"a":2}"#,
        ] {
            let e = strict_object(twice).unwrap_err();
            assert_eq!(
                e.reason,
                "duplicate-member",
                "{}",
                String::from_utf8_lossy(twice)
            );
        }
        assert_eq!(strict_object(b"[1]").unwrap_err().reason, "not-an-object");
        assert_eq!(
            strict_object(b"{\"a\":").unwrap_err().reason,
            "json-invalid"
        );
        assert_eq!(strict_object(b"{} x").unwrap_err().reason, "json-invalid");
    }

    fn meta(json: &str) -> Map<String, Value> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn an_unrecognised_token_is_never_allowed() {
        let v = |version, json: &str| meta_verdict(version, &meta(json)).verdict;
        use Verdict::*;
        use VrmVersion::*;
        assert_eq!(
            v(
                V0,
                r#"{"licenseName":"CC_BY","allowedUserName":"Everyone"}"#
            ),
            Allowed
        );
        assert_eq!(
            v(V0, r#"{"licenseName":"CC0","allowedUserName":"Everyone"}"#),
            Allowed
        );
        assert_eq!(
            v(
                V0,
                r#"{"licenseName":"Other","allowedUserName":"Everyone"}"#
            ),
            Unknown
        );
        assert_eq!(
            v(
                V0,
                r#"{"licenseName":"CC-BY","allowedUserName":"Everyone"}"#
            ),
            Unknown
        );
        assert_eq!(
            v(
                V0,
                r#"{"licenseName":"CC_BY","allowedUserName":"everyone"}"#
            ),
            Unknown
        );
        assert_eq!(
            v(
                V0,
                r#"{"licenseName":"Other","allowedUserName":"OnlyAuthor"}"#
            ),
            Prohibited
        );
        assert_eq!(
            v(V0, r#"{"licenseName":1,"allowedUserName":"Everyone"}"#),
            Prohibited
        );
        assert_eq!(v(V0, r#"{"allowedUserName":"Everyone"}"#), Prohibited);
        assert_eq!(
            v(
                V1,
                r#"{"allowRedistribution":true,"avatarPermission":"everyone"}"#
            ),
            Allowed
        );
        assert_eq!(
            v(
                V1,
                r#"{"allowRedistribution":"true","avatarPermission":"everyone"}"#
            ),
            Prohibited
        );
        assert_eq!(
            v(
                V1,
                r#"{"allowRedistribution":1,"avatarPermission":"everyone"}"#
            ),
            Prohibited
        );
        assert_eq!(
            v(
                V1,
                r#"{"allowRedistribution":true,"avatarPermission":"Everyone"}"#
            ),
            Unknown
        );
        assert_eq!(v(V1, r#"{"allowRedistribution":true}"#), Prohibited);
        let d = meta_verdict(V1, &meta("{}")).deciding.unwrap();
        assert_eq!(
            (d.meta.as_str(), &d.value, d.defaulted),
            (
                "extensions.VRMC_vrm.meta.allowRedistribution",
                &Value::Null,
                true
            )
        );
    }

    #[test]
    fn a_glb_is_classified_by_its_extensions_never_its_name() {
        let doc = |json: &str| meta(json);
        assert_eq!(
            class_of(&doc(r#"{"extensions":{"VRMC_vrm":{}}}"#)),
            GltfClass::Avatar
        );
        assert_eq!(
            class_of(&doc(r#"{"extensionsUsed":["VRM"]}"#)),
            GltfClass::Avatar
        );
        assert_eq!(
            class_of(&doc(
                r#"{"extensionsUsed":["VRMC_vrm_animation","VRMC_vrm"]}"#
            )),
            GltfClass::Avatar
        );
        assert_eq!(
            class_of(&doc(r#"{"extensions":{"VRMC_vrm_animation":{}}}"#)),
            GltfClass::Animation
        );
        assert_eq!(
            class_of(&doc(r#"{"extensionsUsed":"VRM"}"#)),
            GltfClass::Plain
        );
        assert_eq!(class_of(&doc(r#"{"asset":{}}"#)), GltfClass::Plain);
    }
}
