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
//!   check`'s step 8), in the order of AstraPlugins' `tools/check-games-goldens.py`
//!   `release_findings`, over EVERY file of the release, declared or not:
//!   1. **the release's glTF JSON**: every GLB's declared first-chunk length and every `.gltf`'s
//!      size, read before anything is parsed, at most 32 MiB (`content-format{reason=
//!      gltf-json-total}`); the embedded bundles and archives below charge the same budget;
//!   2. **by bytes, never by name** (§2.7.1): a GLB not named `.vrm` `.glb` `.vrma`, a text glTF
//!      (a JSON object naming a top-level `asset` object, a UTF-16/32 text sniffed as the text it
//!      decodes to: `sniff`) not named `.gltf`, and a content bundle (a zip whose last entry is
//!      `astrachar.json`) not named `.astrachar` `.astrapack` are `content-format{reason=
//!      glb-misnamed | gltf-misnamed | bundle-misnamed}`; the manifest is `astra-gi.toml` at the
//!      root only, a file of that name in a folder is judged like any other;
//!   3. **the format** of every GLB (by its magic) and `.gltf` (by its name), in any part and of
//!      any class ([`format_problem`]): the GLB whole, its JSON chunk or the `.gltf` at most
//!      16 MiB, a `.gltf` with no byte-order mark, its glTF JSON parsing, nesting at most 128
//!      deep, naming no member twice, and no external reference — `images` and `buffers` arrays
//!      of objects, a GLB's every `uri` a `data:` one, a `.gltf`'s a `data:` one or a plain
//!      relative path, percent-decoded exactly once (no raw `?` `#` `\`, no escape left), naming
//!      a file the release holds (`content-format{reason}`);
//!   4. **a game's own archives, one level deep**: a zip that is no bundle (a `.jar`, a mod
//!      `.zip`), walked with the content catalogue's own reader (`zipwalk`, every cloaking check
//!      included; 65,536 entries, 512 MiB): two readings of it are refused with content's code
//!      (`zip-headers-inconsistent`, `zip-end-record`); one it cannot otherwise open (zip64, too
//!      big, encrypted, another method, …) and an archive inside it are the flag
//!      `content-archive-unscanned`; inside it every file is judged by steps 2 and 3, and each
//!      GLB, `.gltf` and bundle is a part to declare as `<archive path>!<inner path>`;
//!   5. **coverage.** A *content-typed* file is one whose extension AND first bytes both say so
//!      ([`is_content_typed`]: `.astrapack` `.astrachar` a zip, `.vrma` `.glb` `.vrm` the GLB
//!      magic, `.gltf` JSON text, `.fbx` a binary or ASCII FBX, `.png` `.jpg` `.jpeg` `.webp`
//!      `.gif` and `.ogg` `.wav` `.mp3` `.flac` their signatures); code and loaders never are,
//!      whatever they are called. Each is covered by exactly one entry: `content-undeclared`
//!      names a file no entry covers, `content-missing` an entry no content-typed file is at (a
//!      DLL renamed `climb.fbx` covers nothing);
//!   6. **the VRM licence gate** ([`vrm_gate`]), on what is judged by its glTF JSON's extensions
//!      and never by its name ([`class_of`]): an avatar (`VRM`, `VRMC_vrm`) in any part, a plain
//!      glTF in a `character` part. An animation (`VRMC_vrm_animation` alone) gets no avatar
//!      verdict. Every VRM meta the file carries is read, each by its own spec's defaults
//!      ([`meta_verdict`]; an unrecognised token is unknown, never allowed; a non-bool
//!      `allowRedistribution` is prohibited): prohibited is `content-licence-refused{meta, value,
//!      defaulted}`, two metas that disagree `content-licence-conflict`, and unknown (no meta, an
//!      unrecognised token) the flag `content-vrm-meta-unknown`, never a refusal. An
//!      `.astrachar` or `.astrapack` part is judged whole by the content catalogue's own code
//!      (`bundle`: its walk, its manifest, every file's format, every GLB's verdict, every file
//!      in a part), refused with content's own code; the part's declared licence stands for every
//!      part of the bundle, and a bundle the content catalogue holds is the same flag;
//!   7. **a licence for every covered part**: the VRM meta's when the gate read one
//!      (`licence_source` `vrm`; a bundle's, its body's), else the entry's own `licence`
//!      (`declared`); with neither, `content-licence-undeclared`.
//!
//! The codes are the spec's (`E_CONTENT_*`, the content contract's `E_ZIP_*`, `E_ARCHIVE_*`,
//! `E_BUNDLE_*`) and flags (`F_CONTENT_*`) in this launcher's spelling (merged plan R32:
//! kebab-case wire codes), one for one ([`SPEC_CODES`]). This is an implementation of the rule
//! independent of AstraPlugins' checkers and of the catalogue's `lib/content.mjs`, held to the
//! same goldens (`testdata/games/content/`, `tests/content.rs` and, through `gi-probe check`,
//! `tests/gi_probe.rs`).

mod bundle;
mod sniff;
mod zipwalk;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::digest::sha256_hex;
use crate::error::{LauncherError, Result, codes};

use zipwalk::{GLB_MAGIC, MAX_GLTF_JSON, MAX_GLTF_JSON_TOTAL};

/// The key of the table below `[integration]`.
pub const TABLE: &str = "content";

/// A flag (never a refusal): a part the VRM gate judges reads `unknown` — no VRM meta, or a token
/// the gate does not recognise — or a bundle the content catalogue would hold for it. It passes
/// with its declared licence (`F_CONTENT_VRM_META_UNKNOWN`).
pub const FLAG_VRM_META_UNKNOWN: &str = "content-vrm-meta-unknown";

/// A flag (never a refusal): a game's own archive the gate could not open (zip64, too large,
/// encrypted, another method, …: `reason`), or an archive inside one (`reason` = `nested`), so
/// nothing inside it was judged (`F_CONTENT_ARCHIVE_UNSCANNED`, shown to moderators).
pub const FLAG_ARCHIVE_UNSCANNED: &str = "content-archive-unscanned";

/// The spec's codes (`manifest.md` §2.7, the content contract's `ingest-v1.md` §3, `vectors.json`)
/// and this launcher's, one for one.
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
    ("E_CONTENT_TOO_LARGE", codes::CONTENT_TOO_LARGE),
    (
        "E_CONTENT_FILE_OUTSIDE_PARTS",
        codes::CONTENT_FILE_OUTSIDE_PARTS,
    ),
    ("E_ZIP_END_RECORD", codes::ZIP_END_RECORD),
    (
        "E_ZIP_HEADERS_INCONSISTENT",
        codes::ZIP_HEADERS_INCONSISTENT,
    ),
    (
        "E_ARCHIVE_TOO_MANY_ENTRIES",
        codes::ARCHIVE_TOO_MANY_ENTRIES,
    ),
    ("E_ARCHIVE_ENCRYPTED", codes::ARCHIVE_ENCRYPTED),
    ("E_ARCHIVE_METHOD", codes::ARCHIVE_METHOD),
    ("E_ARCHIVE_LINK", codes::ARCHIVE_LINK),
    ("E_ARCHIVE_NAME_UNSAFE", codes::ARCHIVE_NAME_UNSAFE),
    ("E_ARCHIVE_TOO_LARGE", codes::ARCHIVE_TOO_LARGE),
    ("E_ARCHIVE_RATIO", codes::ARCHIVE_RATIO),
    ("E_ARCHIVE_MUST_BE_STORED", codes::ARCHIVE_MUST_BE_STORED),
    ("E_BUNDLE_MANIFEST", codes::BUNDLE_MANIFEST),
    ("E_BUNDLE_FILE_UNLISTED", codes::BUNDLE_FILE_UNLISTED),
    ("E_BUNDLE_FILE_DIGEST", codes::BUNDLE_FILE_DIGEST),
    ("F_CONTENT_VRM_META_UNKNOWN", FLAG_VRM_META_UNKNOWN),
    ("F_CONTENT_ARCHIVE_UNSCANNED", FLAG_ARCHIVE_UNSCANNED),
];

/// The launcher's code for the spec's `spec_code`.
pub fn launcher_code(spec_code: &str) -> Option<&'static str> {
    SPEC_CODES
        .iter()
        .find(|(s, _)| *s == spec_code)
        .map(|(_, l)| *l)
}

/// [`launcher_code`] for a code this module itself raises, which always has one.
fn ours(spec_code: &str) -> &'static str {
    launcher_code(spec_code).unwrap_or(codes::CONTENT_FORMAT)
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

/// One `[[integration.content]]` entry: a part, its id its `path` in the release zip (a file in a
/// game's own archive: `<archive path>!<inner path>`).
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

/// The names a bundle carries.
const BUNDLE_EXTS: &[&str] = &[".astrachar", ".astrapack"];
/// The names a file in a game's own archive is a part to declare by: a GLB, a `.gltf`, a bundle.
const INNER_PART_EXTS: &[&str] = &[".vrm", ".glb", ".vrma", ".gltf", ".astrachar", ".astrapack"];
/// The manifest: `astra-gi.toml` at the release root, and nowhere else.
const MANIFEST_NAME: &str = "astra-gi.toml";

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

/// Whether `path`'s extension is one a content-typed file may have.
pub fn has_content_extension(path: &str) -> bool {
    magic_of(path).is_some()
}

fn ends_with_any(path: &str, exts: &[&str]) -> bool {
    let lower = path.to_lowercase();
    exts.iter().any(|x| lower.ends_with(x))
}

fn starts(b: &[u8], with: &[u8]) -> bool {
    b.get(..with.len()) == Some(with)
}

fn at(b: &[u8], i: usize, with: &[u8]) -> bool {
    b.get(i..i + with.len()) == Some(with)
}

/// A zip by its magic: `PK` and 3, 5 or 7.
fn is_zip(b: &[u8]) -> bool {
    b.len() >= 4 && starts(b, b"PK") && matches!(b[2], 3 | 5 | 7)
}

impl Magic {
    /// Whether `head` (a file's first [`SNIFF_BYTES`]) is this format's, exactly as the spec's
    /// checker reads it.
    fn matches(self, b: &[u8]) -> bool {
        match self {
            Magic::Zip => is_zip(b),
            Magic::Glb => starts(b, GLB_MAGIC),
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
    path != MANIFEST_NAME && magic_of(path).is_some_and(|m| m.matches(head))
}

// ---------------------------------------------------------------------------------------------
// glTF JSON, read strictly.
// ---------------------------------------------------------------------------------------------

const DUPLICATE_MEMBER: &str = "the glTF JSON names a member twice: ";
const TOO_DEEP: &str = "the JSON nests deeper than ";
/// The deepest JSON read, the top-level value being depth 1 (the content catalogue's cap).
pub(crate) const MAX_JSON_DEPTH: usize = 128;

/// A JSON value, refused when an object names a member twice (one reader keeps the first, another
/// the last) or when it nests deeper than [`MAX_JSON_DEPTH`].
struct Strict {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        d.deserialize_any(StrictVisitor { depth: self.depth })
    }
}

struct StrictVisitor {
    depth: usize,
}

impl StrictVisitor {
    fn deeper<E: de::Error>(&self) -> std::result::Result<Strict, E> {
        if self.depth > MAX_JSON_DEPTH {
            return Err(E::custom(format!("{TOO_DEEP}{MAX_JSON_DEPTH}")));
        }
        Ok(Strict {
            depth: self.depth + 1,
        })
    }
}

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
        let child = self.deeper()?;
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Strict { depth: child.depth })? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let child = self.deeper()?;
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("{DUPLICATE_MEMBER}{key:?}")));
            }
            let v = map.next_value_seed(Strict { depth: child.depth })?;
            out.insert(key, v);
        }
        Ok(Value::Object(out))
    }
}

/// Why JSON is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StrictError {
    /// An object names this member twice.
    Duplicate(String),
    /// Deeper than [`MAX_JSON_DEPTH`].
    TooDeep,
    /// Not JSON (or not UTF-8).
    Invalid(String),
}

/// `bytes` as one JSON value, no member named twice, at most [`MAX_JSON_DEPTH`] deep (the top
/// level is 1: `serde_json`'s own limit is lifted, so the visitor's is the only one).
pub(crate) fn strict_value(bytes: &[u8]) -> std::result::Result<Value, StrictError> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.disable_recursion_limit();
    let v = Strict { depth: 1 }
        .deserialize(&mut de)
        .and_then(|v| de.end().map(|()| v));
    v.map_err(|e| {
        let text = e.to_string();
        if e.classify() == serde_json::error::Category::Data && text.starts_with(DUPLICATE_MEMBER) {
            StrictError::Duplicate(text)
        } else if e.classify() == serde_json::error::Category::Data && text.starts_with(TOO_DEEP) {
            StrictError::TooDeep
        } else {
            StrictError::Invalid(text)
        }
    })
}

/// Why a GLB or a `.gltf` breaks the format rules: `content-format{reason, detail}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatProblem {
    /// The rule. A GLB's container: `truncated`, `length-mismatch`, `first-chunk-not-json`,
    /// `chunk-past-end`; its JSON: `json-over-16mib`, `bom` (a `.gltf`'s byte-order mark),
    /// `json-invalid`, `json-too-deep`, `duplicate-member`, `not-an-object`; its references:
    /// `gltf-array-shape` (`images`/`buffers` not an array of objects), `uri-not-a-string`,
    /// `external-uri` (a GLB's), `uri-forbidden-character` (a raw `?` `#` `\`),
    /// `uri-escape-invalid`, `uri-double-encoded`, `uri-not-relative`, `uri-not-in-release`,
    /// `uri-outside-part` (a `.gltf`'s). By bytes: `glb-misnamed`, `gltf-misnamed`,
    /// `bundle-misnamed`; the release's `gltf-json-total`; an archive's `archive-unreadable`
    /// and `member-shadowed` (a file named like an archive's member beside it); a bundle's
    /// `bundle-unreadable`, `nested-archive`, `name-magic`, `body-not-vrm`, `settings-invalid`.
    pub reason: &'static str,
    pub detail: String,
}

fn problem(reason: &'static str, detail: impl Into<String>) -> FormatProblem {
    FormatProblem {
        reason,
        detail: detail.into(),
    }
}

/// [`StrictError`] as a [`FormatProblem`].
fn json_problem(e: StrictError) -> FormatProblem {
    match e {
        StrictError::Duplicate(d) => problem("duplicate-member", d),
        StrictError::TooDeep => problem(
            "json-too-deep",
            format!("the glTF JSON nests deeper than {MAX_JSON_DEPTH}"),
        ),
        StrictError::Invalid(d) => {
            problem("json-invalid", format!("the glTF JSON does not parse: {d}"))
        }
    }
}

/// `bytes` as one JSON object, no member named twice, at most 128 deep.
fn strict_object(bytes: &[u8]) -> std::result::Result<Map<String, Value>, FormatProblem> {
    match strict_value(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(problem("not-an-object", "the glTF JSON is not an object")),
        Err(e) => Err(json_problem(e)),
    }
}

fn u32_le(b: &[u8], i: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]))
}

/// A GLB's glTF JSON, the container whole: its header's total length is the file's, its first
/// chunk is JSON of at most 16 MiB and ends inside the file, and the JSON is one object.
fn glb_json(raw: &[u8]) -> std::result::Result<Map<String, Value>, FormatProblem> {
    if raw.len() < 20 || !starts(raw, GLB_MAGIC) {
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
    if len > MAX_GLTF_JSON {
        return Err(problem(
            "json-over-16mib",
            format!("a JSON chunk of {len} bytes, over {MAX_GLTF_JSON}"),
        ));
    }
    strict_object(&raw[20..20 + len as usize])
}

/// The content catalogue's own `gltf_problem` (ingest-v1.md §3 step 10) for a bundle's GLB or
/// `.gltf`: a first chunk that is not JSON, JSON over 16 MiB (checked before it is parsed), JSON
/// that does not parse, is no object or nests deeper than 128. A member named twice is not this
/// rule's.
pub(crate) fn gltf_problem_content(b: &[u8]) -> Option<FormatProblem> {
    let data = if starts(b, GLB_MAGIC) {
        if b.get(16..20) != Some(b"JSON") {
            return Some(problem(
                "first-chunk-not-json",
                "its first chunk is not JSON",
            ));
        }
        let n = u32_le(b, 12);
        if n > MAX_GLTF_JSON {
            return Some(problem(
                "json-over-16mib",
                format!("a JSON chunk of {n} bytes, over {MAX_GLTF_JSON}"),
            ));
        }
        &b[20.min(b.len())..(20 + n as usize).min(b.len())]
    } else {
        if b.len() as u64 > MAX_GLTF_JSON {
            return Some(problem(
                "json-over-16mib",
                format!("a .gltf of {} bytes, over {MAX_GLTF_JSON}", b.len()),
            ));
        }
        b
    };
    match strict_value(data) {
        Err(StrictError::Duplicate(_)) => None,
        Err(e) => Some(json_problem(e)),
        Ok(Value::Object(_)) => None,
        Ok(_) => Some(problem("not-an-object", "its JSON is not an object")),
    }
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

/// `uri` relative to the folder of `path` (which holds no `..`), `.` and empty components dropped
/// (`posixpath.normpath(posixpath.join(posixpath.dirname(path), uri))`).
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

/// A `.gltf` uri as a loader reads it (the content catalogue's `_decode_gltf_uri`, N2):
/// percent-decoded exactly once, after no raw `?`, `#` or `\`; refused when an escape is
/// malformed, the decoded bytes are no UTF-8 or hold a control character, or a `%` is left
/// (double-encoded).
fn decode_gltf_uri(u: &str) -> std::result::Result<String, FormatProblem> {
    let bad: String = ['?', '#', '\\']
        .iter()
        .filter(|c| u.contains(**c))
        .collect();
    if !bad.is_empty() {
        return Err(problem(
            "uri-forbidden-character",
            format!("a uri containing {bad:?}: {u:?}"),
        ));
    }
    let src = u.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' {
            let two = src
                .get(i + 1..i + 3)
                .filter(|t| t.iter().all(u8::is_ascii_hexdigit));
            let Some(two) = two else {
                return Err(problem(
                    "uri-escape-invalid",
                    format!("a uri with a '%' not followed by two hex digits: {u:?}"),
                ));
            };
            let hex = std::str::from_utf8(two).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(src[i]);
            i += 1;
        }
    }
    let Ok(decoded) = String::from_utf8(out) else {
        return Err(problem(
            "uri-escape-invalid",
            format!("a uri not valid UTF-8 once percent-decoded: {u:?}"),
        ));
    };
    if decoded.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return Err(problem(
            "uri-escape-invalid",
            format!("a uri with a control character once percent-decoded: {u:?}"),
        ));
    }
    if decoded.contains('%') {
        return Err(problem(
            "uri-double-encoded",
            format!("a uri still percent-encoded once decoded (double encoding): {u:?}"),
        ));
    }
    Ok(decoded)
}

/// No external reference (the content catalogue's own `uri_refusal`): `images` and `buffers`,
/// when there, are arrays of objects; in a GLB every `uri` is a `data:` URI; in a `.gltf` each is
/// a `data:` URI or, decoded once, a plain relative path (no scheme, no `..`, no leading `/`, no
/// backslash) naming a file `holds` holds, in the part `same_part` says is the file's own.
pub(crate) fn uri_problem(
    doc: &Map<String, Value>,
    is_glb: bool,
    path: &str,
    holds: &dyn Fn(&str) -> bool,
    same_part: &dyn Fn(&str) -> bool,
) -> Option<FormatProblem> {
    for key in ["images", "buffers"] {
        let items = match doc.get(key) {
            None | Some(Value::Null) => continue,
            Some(Value::Array(items)) => items,
            Some(other) => {
                return Some(problem(
                    "gltf-array-shape",
                    format!("{key} is a JSON {}, not an array", json_kind(other)),
                ));
            }
        };
        if let Some(x) = items.iter().find(|x| !x.is_object()) {
            return Some(problem(
                "gltf-array-shape",
                format!("{key} contains an element that is not an object: {x}"),
            ));
        }
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
            let decoded = match decode_gltf_uri(u) {
                Ok(d) => d,
                Err(p) => return Some(p),
            };
            if has_scheme(&decoded)
                || decoded.starts_with('/')
                || decoded.contains('\\')
                || format!("/{decoded}/").contains("/../")
                || decoded.is_empty()
                || decoded == "."
            {
                return Some(problem(
                    "uri-not-relative",
                    format!("a uri that is not a plain relative path: {decoded:?}"),
                ));
            }
            let target = resolve(path, &decoded);
            if !holds(&target) {
                return Some(problem(
                    "uri-not-in-release",
                    format!("a uri naming a file the release does not hold: {decoded:?}"),
                ));
            }
            if !same_part(&target) {
                return Some(problem(
                    "uri-outside-part",
                    format!("a uri resolving outside its own part: {decoded:?}"),
                ));
            }
        }
    }
    None
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Whether `path` names a `.gltf` (by its extension, any case).
fn is_gltf_path(path: Option<&str>) -> bool {
    path.is_some_and(|p| p.to_lowercase().ends_with(".gltf"))
}

/// The glTF JSON of a GLB (by its magic, whatever it is called) or of a `.gltf` (`path`), held
/// to the format rules (the games checker's `gltf_format_problem`): `None` for any other file,
/// else the document or why it breaks them. `present` is every file a `.gltf`'s relative uri may
/// name; with none, only the uri's shape is judged.
pub fn gltf_document(
    raw: &[u8],
    path: Option<&str>,
    present: Option<&BTreeSet<String>>,
) -> Option<std::result::Result<Map<String, Value>, FormatProblem>> {
    let is_glb = starts(raw, GLB_MAGIC);
    if !is_glb && !is_gltf_path(path) {
        return None;
    }
    let doc = if is_glb {
        glb_json(raw)
    } else {
        let p = path.unwrap_or_default();
        if let Some(why) = sniff::bom_refusal(p, raw) {
            return Some(Err(problem("bom", why)));
        }
        if raw.len() as u64 > MAX_GLTF_JSON {
            return Some(Err(problem(
                "json-over-16mib",
                format!("a .gltf of {} bytes, over {MAX_GLTF_JSON}", raw.len()),
            )));
        }
        strict_object(raw)
    };
    let holds = |t: &str| match (present, path) {
        (Some(present), Some(_)) => present.contains(t),
        _ => true,
    };
    Some(doc.and_then(|d| {
        match uri_problem(&d, is_glb, path.unwrap_or_default(), &holds, &|_| true) {
            Some(p) => Err(p),
            None => Ok(d),
        }
    }))
}

/// Why `raw` (a GLB by its magic, or the `.gltf` at `path`) breaks the format rules, if it does:
/// what every such file is held to before it is classified.
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
// By bytes.
// ---------------------------------------------------------------------------------------------

/// `astra-gi.toml`, `character.toml` and their caps (the content contract's settings file).
pub(crate) const SETTINGS_NAME: &str = "character.toml";
pub(crate) const MAX_SETTINGS: usize = 256 * 1024;

/// Whether `raw` is a content bundle by the content catalogue's own definition: a zip whose LAST
/// entry is `astrachar.json`, read with its own reader — only an archive Astra's importer would
/// open as a bundle counts.
fn is_bundle_bytes(raw: &[u8]) -> bool {
    if !is_zip(raw) || memchr::memmem::find(raw, b"astrachar.json").is_none() {
        return false;
    }
    zipwalk::walk(raw, zipwalk::BUNDLE, false)
        .is_ok_and(|w| w.order().last() == Some("astrachar.json"))
}

/// Why a file's name and its bytes disagree (`content-format`), or `None` — asked of every file
/// of the release, declared or not (the games checker's `name_problem`).
fn name_problem(path: &str, raw: &[u8], budget: &mut sniff::SniffBudget) -> Option<FormatProblem> {
    if path == MANIFEST_NAME {
        return None;
    }
    if starts(raw, GLB_MAGIC) {
        return (!ends_with_any(path, &[".vrm", ".glb", ".vrma"])).then(|| {
            problem(
                "glb-misnamed",
                "a GLB (it begins with glTF) named something not .vrm, .glb or .vrma",
            )
        });
    }
    if !ends_with_any(path, &[".gltf"]) && sniff::is_text_gltf(raw, budget) {
        return Some(problem(
            "gltf-misnamed",
            if raw.len() as u64 > MAX_GLTF_JSON {
                format!(
                    "a JSON text over 16 MiB ({} bytes) that names an asset key, named something not .gltf",
                    raw.len()
                )
            } else {
                "a text glTF (a JSON object with a top-level asset member) named something not .gltf"
                    .to_owned()
            },
        ));
    }
    if is_zip(raw) && !ends_with_any(path, BUNDLE_EXTS) && is_bundle_bytes(raw) {
        return Some(problem(
            "bundle-misnamed",
            "a content bundle (a zip whose last entry is astrachar.json) named something not .astrachar or .astrapack",
        ));
    }
    None
}

/// How much of a file is read first: whether it needs the rest is decided from it.
const PREFIX: usize = 64 * 1024;

/// Whether the by-bytes rules need all of a file whose first bytes are `head` (of `size`): a
/// GLB under a GLB name (its format), a `.gltf`, a zip that is no bundle by name, and any text
/// that is, or may yet be, JSON-looking. Anything else is decided by its first bytes.
fn needs_whole(path: &str, head: &[u8], size: u64) -> bool {
    if head.len() as u64 >= size {
        return false; // it is whole
    }
    if starts(head, GLB_MAGIC) {
        return ends_with_any(path, &[".vrm", ".glb", ".vrma"]);
    }
    if ends_with_any(path, &[".gltf"]) {
        return true;
    }
    if is_zip(head) {
        return !ends_with_any(path, BUNDLE_EXTS);
    }
    !matches!(sniff::first_char(head, false), Some(c) if c != b'{')
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
    /// `None` is `content-licence-undeclared` (or a part the gate refused).
    pub licence_source: Option<LicenceSource>,
    pub sha256: String,
    /// What the glTF's extensions make it, when it is a glTF (a bundle: its body's). Not a
    /// `contentPart` member, so never in the part's JSON: [`Report::classes`] says it beside the
    /// parts.
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
    /// Every content-typed file of the release, sorted: a part inside a game's own archive by
    /// `<archive path>!<inner path>`.
    pub content_files: Vec<String>,
    /// Each entry a content-typed file covers, in the manifest's order.
    pub parts: Vec<PartReport>,
    /// Every refusal, in order: what the release's files are, then the files no entry covers and
    /// the entries no file is at, then each entry's. One per code and path.
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

/// The refusals of one release: one per `(code, path)`, in the order they are found.
struct Findings<'a> {
    parts: &'a [ContentPart],
    out: Vec<LauncherError>,
    seen: BTreeSet<(&'static str, String)>,
}

impl Findings<'_> {
    /// The `field` a refusal of `path` names: the entry that declares it, else the table.
    fn field(&self, path: &str) -> String {
        self.parts
            .iter()
            .position(|p| p.path == path)
            .map_or(format!("integration.{TABLE}"), |i| {
                format!("integration.{TABLE}[{i}]")
            })
    }

    fn push(&mut self, code: &'static str, path: &str, mut params: BTreeMap<String, String>) {
        if !self.seen.insert((code, path.to_owned())) {
            return;
        }
        params
            .entry("field".to_owned())
            .or_insert_with(|| self.field(path));
        params.insert("path".to_owned(), path.to_owned());
        let mut e = LauncherError::new(code);
        e.params = params;
        self.out.push(e);
    }

    fn format(&mut self, path: &str, p: FormatProblem) {
        let mut params = BTreeMap::new();
        params.insert("reason".to_owned(), p.reason.to_owned());
        params.insert("detail".to_owned(), p.detail);
        self.push(codes::CONTENT_FORMAT, path, params);
    }

    fn holds(&self, code: &'static str, path: &str) -> bool {
        self.seen.contains(&(code, path.to_owned()))
    }
}

fn unscanned(path: &str, reason: &str, detail: impl Into<String>) -> Flag {
    let mut params = BTreeMap::new();
    params.insert("path".to_owned(), path.to_owned());
    params.insert("reason".to_owned(), reason.to_owned());
    params.insert("detail".to_owned(), detail.into());
    Flag {
        code: FLAG_ARCHIVE_UNSCANNED,
        params,
    }
}

/// What the content catalogue's own code says of one embedded bundle (the games checker's
/// `bundle_verdict`).
enum BundleVerdict {
    /// Refused with content's code (its launcher spelling), its params.
    Refused(&'static str, BTreeMap<String, String>),
    /// Accepted; `unknown` held by the content catalogue (the flag); its body, when it has one.
    Accepted {
        unknown: bool,
        body: Option<(String, Vec<u8>)>,
    },
}

/// The params of a bundle's refusal: content's words prefixed `<path>!`, the bundle's file it
/// names, and for `content-format` its reason.
fn bundle_params(
    path: &str,
    detail: &str,
    inner: Option<&str>,
    reason: &str,
) -> BTreeMap<String, String> {
    let mut params = BTreeMap::new();
    params.insert("detail".to_owned(), format!("{path}!{detail}"));
    if let Some(inner) = inner {
        params.insert("inner".to_owned(), inner.to_owned());
    }
    if !reason.is_empty() {
        params.insert("reason".to_owned(), reason.to_owned());
    }
    params
}

/// One embedded bundle at `path`, judged by the content catalogue's own code within the release's
/// budget: walked once (its own 32 MiB of glTF JSON charged to the release's before anything in it
/// is parsed), then ingested.
fn bundle_verdict(
    path: &str,
    raw: &[u8],
    licence: bool,
    budget: &mut sniff::SniffBudget,
) -> BundleVerdict {
    let unreadable = |why: &str| {
        BundleVerdict::Refused(
            codes::CONTENT_FORMAT,
            bundle_params(
                path,
                &format!(": the content checker cannot read it ({why})"),
                None,
                "bundle-unreadable",
            ),
        )
    };
    let w = match zipwalk::walk(raw, zipwalk::BUNDLE, true) {
        Ok(w) => w,
        Err(zipwalk::WalkError::Refused(r)) => {
            return BundleVerdict::Refused(ours(r.code), bundle_params(path, &r.detail, None, ""));
        }
        Err(zipwalk::WalkError::Unreadable(why)) => return unreadable(&why),
    };
    let order: Vec<String> = w.order().map(str::to_owned).collect();
    let list: Vec<(String, Vec<u8>)> = w.into_files();
    let own: u64 = list
        .iter()
        .map(|(n, d)| zipwalk::gltf_json_bytes(n, &d[..d.len().min(16)], d.len() as u64))
        .sum();
    if budget.gltf_json + own > MAX_GLTF_JSON_TOTAL {
        return BundleVerdict::Refused(
            codes::CONTENT_FORMAT,
            bundle_params(
                path,
                &format!(
                    ": {} bytes of glTF JSON in the release with this bundle, over {MAX_GLTF_JSON_TOTAL} (32 MiB)",
                    budget.gltf_json + own
                ),
                None,
                "gltf-json-total",
            ),
        );
    }
    budget.gltf_json += own;
    let files = bundle::Files::new(list);
    match bundle::ingest(&files, &order, licence) {
        Ok(a) => BundleVerdict::Accepted {
            unknown: a.unknown,
            body: a.body,
        },
        Err(bundle::Failure::Unreadable(why)) => unreadable(&why),
        Err(bundle::Failure::Refused(r)) => {
            let mut params = bundle_params(path, &r.detail, r.inner.as_deref(), r.reason);
            // The reading that decided a refused VRM, as for a part of the release's own.
            if r.code == bundle::E_CONTENT_LICENCE_REFUSED
                && let Some(inner) = &r.inner
                && let Some(Gate::Refused { deciding, .. }) =
                    files.get(inner).map(|b| vrm_gate(b, Some(inner), None))
            {
                params = with_deciding(params, &deciding);
            }
            BundleVerdict::Refused(ours(r.code), params)
        }
    }
}

/// A file found content-typed: where its bytes are read from when its entry is judged.
enum Found {
    /// A file of the release.
    Release,
    /// A file in a game's own archive (its bytes kept when an entry declares it), and the files
    /// beside it (what a `.gltf`'s relative uri may name).
    Inner(Option<Vec<u8>>, std::rc::Rc<BTreeSet<String>>),
}

/// Holds a release's files to its `[[integration.content]]` (the module's seven steps). `files`
/// is every file of the release, `(name, size)`; `read` gives the first bytes of one (`Some(n)`)
/// or all of it (`None`).
pub fn judge(
    parts: &[ContentPart],
    files: &[(String, u64)],
    mut read: impl FnMut(&str, Option<usize>) -> Result<Vec<u8>>,
) -> Result<Report> {
    let mut files: Vec<(String, u64)> = files.to_vec();
    files.sort();
    let present: BTreeSet<String> = files.iter().map(|(n, _)| n.clone()).collect();
    let declared: BTreeSet<&str> = parts.iter().map(|p| p.path.as_str()).collect();
    let mut f = Findings {
        parts,
        out: Vec::new(),
        seen: BTreeSet::new(),
    };
    let mut flags: Vec<Flag> = Vec::new();
    let mut budget = sniff::SniffBudget::default();

    // 1. The release's glTF JSON, from headers.
    let mut heads: Vec<Vec<u8>> = Vec::with_capacity(files.len());
    for (name, _) in &files {
        heads.push(read(name, Some(PREFIX))?);
    }
    budget.gltf_json = files
        .iter()
        .zip(&heads)
        .map(|((n, size), h)| zipwalk::gltf_json_bytes(n, &h[..h.len().min(16)], *size))
        .sum();
    if budget.gltf_json > MAX_GLTF_JSON_TOTAL {
        f.format(
            "",
            problem(
                "gltf-json-total",
                format!(
                    "{} bytes of glTF JSON in the release, over {MAX_GLTF_JSON_TOTAL} (32 MiB)",
                    budget.gltf_json
                ),
            ),
        );
    }
    let mut found: BTreeMap<String, Found> = files
        .iter()
        .zip(&heads)
        .filter(|((n, _), h)| is_content_typed(n, h))
        .map(|((n, _), _)| (n.clone(), Found::Release))
        .collect();

    // 2–4. Every file by its bytes; a game's own archives, one level deep.
    for ((name, size), head) in files.iter().zip(heads) {
        let raw: Vec<u8> = if needs_whole(name, &head, *size) {
            read(name, None)?
        } else {
            head
        };
        if let Some(p) = name_problem(name, &raw, &mut budget) {
            f.format(name, p);
            continue;
        }
        if let Some(p) = format_problem(&raw, Some(name), Some(&present)) {
            f.format(name, p);
            continue;
        }
        // A zip no bundle by name; `name_problem` found it no bundle by its bytes either.
        if !is_zip(&raw) || ends_with_any(name, BUNDLE_EXTS) {
            continue;
        }
        let w = match zipwalk::walk(&raw, zipwalk::GAME_ARCHIVE, false) {
            Ok(w) => w,
            Err(zipwalk::WalkError::Refused(r)) => {
                let cloak = matches!(
                    r.code,
                    zipwalk::E_ZIP_HEADERS_INCONSISTENT | zipwalk::E_ZIP_END_RECORD
                ) && !r.zip64;
                if cloak {
                    let mut params = BTreeMap::new();
                    params.insert("detail".to_owned(), r.detail);
                    f.push(ours(r.code), name, params);
                } else if r.code == zipwalk::E_ARCHIVE_TOO_LARGE && r.gltf_json {
                    f.format(name, problem("gltf-json-total", r.detail));
                } else {
                    flags.push(unscanned(name, ours(r.code), r.detail));
                }
                continue;
            }
            Err(zipwalk::WalkError::Unreadable(why)) => {
                f.format(
                    name,
                    problem(
                        "archive-unreadable",
                        format!("the archive cannot be read ({why})"),
                    ),
                );
                continue;
            }
        };
        let inner: Vec<(String, usize)> = w
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_dir())
            .map(|(i, e)| (format!("{name}!{}", e.name), i))
            .collect();
        let own: u64 = inner
            .iter()
            .map(|(k, i)| {
                let e = &w.entries[*i];
                zipwalk::gltf_json_bytes(k, &e.head, u64::from(e.size))
            })
            .sum();
        if budget.gltf_json + own > MAX_GLTF_JSON_TOTAL {
            f.format(
                name,
                problem(
                    "gltf-json-total",
                    format!(
                        "{} bytes of glTF JSON in the release with this archive, over {MAX_GLTF_JSON_TOTAL} (32 MiB)",
                        budget.gltf_json + own
                    ),
                ),
            );
            continue;
        }
        budget.gltf_json += own;
        let beside = std::rc::Rc::new(
            inner
                .iter()
                .map(|(k, _)| k.clone())
                .collect::<BTreeSet<_>>(),
        );
        let mut inner = inner;
        inner.sort();
        for (k, i) in inner {
            if present.contains(&k) {
                f.format(
                    &k,
                    problem(
                        "member-shadowed",
                        "a file named like a member of an archive beside it",
                    ),
                );
                continue;
            }
            let size = u64::from(w.entries[i].size);
            let head = w.data(i, Some(PREFIX));
            let d = if needs_whole(&k, &head, size) {
                w.data(i, None)
            } else {
                head
            };
            if let Some(p) = name_problem(&k, &d, &mut budget)
                .or_else(|| format_problem(&d, Some(&k), Some(&beside)))
            {
                f.format(&k, p);
                continue;
            }
            if is_zip(&d) && !ends_with_any(&k, BUNDLE_EXTS) {
                flags.push(unscanned(
                    &k,
                    "nested",
                    "an archive inside an archive: opened one level deep",
                ));
                continue;
            }
            if ends_with_any(&k, INNER_PART_EXTS) && is_content_typed(&k, &d) {
                let keep = declared.contains(k.as_str()).then(|| d.into_owned());
                found.insert(k, Found::Inner(keep, beside.clone()));
            }
        }
    }

    // 5. Coverage.
    for p in found.keys().filter(|p| !declared.contains(p.as_str())) {
        f.push(codes::CONTENT_UNDECLARED, p, BTreeMap::new());
    }
    let missing: BTreeSet<&str> = declared
        .iter()
        .copied()
        .filter(|p| !found.contains_key(*p))
        .collect();
    for p in missing {
        if !f.holds(codes::CONTENT_FORMAT, p) {
            let field = format!("{}.path", f.field(p));
            let mut params = BTreeMap::new();
            params.insert("field".to_owned(), field);
            f.push(codes::CONTENT_MISSING, p, params);
        }
    }

    // 6–7. Each entry: the gate, and its licence.
    let mut reports = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        // Two entries of one path are refused at parse; the first is the one judged.
        if parts[..i].iter().any(|p| p.path == part.path) {
            continue;
        }
        let Some(how) = found.get(&part.path) else {
            continue;
        };
        let (raw, beside): (Vec<u8>, &BTreeSet<String>) = match how {
            Found::Release => (read(&part.path, None)?, &present),
            Found::Inner(Some(b), beside) => (b.clone(), beside),
            Found::Inner(None, _) => continue,
        };
        let field = format!("integration.{TABLE}[{i}]");
        let mut base = BTreeMap::new();
        base.insert("field".to_owned(), field.clone());

        let mut gltf = None;
        let mut vrm_licence = None;
        let mut refused = false;
        match gltf_document(&raw, Some(&part.path), Some(beside)) {
            None => {}
            Some(Err(p)) => {
                f.format(&part.path, p);
                refused = true;
            }
            Some(Ok(doc)) => {
                let class = class_of(&doc);
                gltf = Some(class);
                if class == GltfClass::Avatar
                    || (class == GltfClass::Plain && part.kind == ContentKind::Character)
                {
                    match gate_of(&doc) {
                        Gate::Format(p) => {
                            f.format(&part.path, p);
                            refused = true;
                        }
                        Gate::Allowed(licence) => vrm_licence = Some(licence),
                        Gate::Refused { licence, deciding } => {
                            vrm_licence = Some(licence);
                            f.push(
                                codes::CONTENT_LICENCE_REFUSED,
                                &part.path,
                                with_deciding(base.clone(), &deciding),
                            );
                            refused = true;
                        }
                        Gate::Conflict { vrm0, vrm1 } => {
                            let mut params = base.clone();
                            for m in [&vrm0, &vrm1] {
                                let side = if m.version == "0.x" { "vrm0" } else { "vrm1" };
                                params.insert(side.to_owned(), m.verdict.as_str().to_owned());
                                if let Some(d) = &m.deciding {
                                    params.insert(format!("{side}_meta"), d.meta.clone());
                                }
                            }
                            f.push(codes::CONTENT_LICENCE_CONFLICT, &part.path, params);
                            refused = true;
                        }
                        Gate::Unknown(deciding) => {
                            let mut params = match &deciding {
                                Some(d) => with_deciding(base.clone(), d),
                                None => base.clone(),
                            };
                            params.insert("path".to_owned(), part.path.clone());
                            flags.push(Flag {
                                code: FLAG_VRM_META_UNKNOWN,
                                params,
                            });
                        }
                    }
                }
            }
        }
        if ends_with_any(&part.path, BUNDLE_EXTS) && is_zip(&raw) {
            match bundle_verdict(&part.path, &raw, part.licence.is_some(), &mut budget) {
                BundleVerdict::Refused(code, params) => {
                    let mut params = params;
                    params.insert("field".to_owned(), field.clone());
                    f.push(code, &part.path, params);
                    refused = true;
                }
                BundleVerdict::Accepted { unknown, body } => {
                    if unknown {
                        let mut params = base.clone();
                        params.insert("path".to_owned(), part.path.clone());
                        flags.push(Flag {
                            code: FLAG_VRM_META_UNKNOWN,
                            params,
                        });
                    }
                    if let Some((name, bytes)) = body {
                        if let Some(Ok(doc)) = gltf_document(&bytes, Some(&name), None) {
                            gltf = Some(class_of(&doc));
                        }
                        if !unknown
                            && let Gate::Allowed(licence) = vrm_gate(&bytes, Some(&name), None)
                        {
                            vrm_licence = Some(licence);
                        }
                    }
                }
            }
        }
        let licence_source = if vrm_licence.is_some() {
            Some(LicenceSource::Vrm)
        } else if part.licence.is_some() {
            Some(LicenceSource::Declared)
        } else {
            None
        };
        // A part the gate refused is not also asked for its licence.
        if licence_source.is_none() && !refused {
            let mut params = BTreeMap::new();
            params.insert("field".to_owned(), format!("{field}.licence"));
            f.push(codes::CONTENT_LICENCE_UNDECLARED, &part.path, params);
        }
        reports.push(PartReport {
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
    let content_files: Vec<String> = found.keys().cloned().collect();
    Ok(Report {
        content_files,
        parts: reports,
        refusals: f.out,
        flags,
    })
}

/// A release's files as `(name, size)`, read from a map (the unit tests').
#[cfg(test)]
fn judge_files(
    parts: &[ContentPart],
    files: &std::collections::HashMap<String, Vec<u8>>,
) -> Result<Report> {
    let list: Vec<(String, u64)> = files
        .iter()
        .map(|(n, b)| (n.clone(), b.len() as u64))
        .collect();
    judge(parts, &list, |name, at_most| {
        let b = &files[name];
        Ok(b[..b.len().min(at_most.unwrap_or(usize::MAX))].to_vec())
    })
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
            ("mods/a.jar!assets/b.vrm", ".vrm"),
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
    fn a_uri_is_resolved_beside_its_gltf_and_decoded_once() {
        assert_eq!(resolve("models/prop.gltf", "prop.bin"), "models/prop.bin");
        assert_eq!(
            resolve("models/prop.gltf", "./tex/a.png"),
            "models/tex/a.png"
        );
        assert_eq!(resolve("prop.gltf", "a//b.bin"), "a/b.bin");
        assert_eq!(resolve("m/a.jar!x/p.gltf", "p.bin"), "m/a.jar!x/p.bin");
        assert!(has_scheme("https://x"));
        assert!(has_scheme("c:x"));
        assert!(!has_scheme("1x:y"));
        assert!(!has_scheme("a/b:c"));
        assert_eq!(
            decode_gltf_uri("Base%20Color.bin").unwrap(),
            "Base Color.bin"
        );
        assert_eq!(decode_gltf_uri("..%2fx").unwrap(), "../x");
        for (u, reason) in [
            ("a?b", "uri-forbidden-character"),
            ("a#b", "uri-forbidden-character"),
            ("a\\b", "uri-forbidden-character"),
            ("a%2", "uri-escape-invalid"),
            ("a%zz", "uri-escape-invalid"),
            ("a%ff", "uri-escape-invalid"),
            ("a%0a", "uri-escape-invalid"),
            ("%252e%252e/x", "uri-double-encoded"),
        ] {
            assert_eq!(decode_gltf_uri(u).unwrap_err().reason, reason, "{u}");
        }
    }

    #[test]
    fn strict_json_refuses_a_member_named_twice_at_any_depth() {
        assert!(strict_object(br#"{"a":{"b":1,"c":[{"d":1}]}}"#).is_ok());
        for twice in [
            &br#"{"a":1,"a":2}"#[..],
            br#"{"x":{"b":false,"b":true}}"#,
            br#"{"x":[{"k":1,"k":1}]}"#,
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

    /// The content catalogue's depth cap: the top-level object is depth 1, 128 is allowed, 129
    /// is not — and a million levels are refused, never a crash.
    #[test]
    fn json_nests_at_most_128_deep_and_a_million_levels_never_crash() {
        let deep = |depth: usize| {
            let mut s = b"{\"a\":".to_vec();
            s.extend(std::iter::repeat_n(b'[', depth - 1));
            s.extend(std::iter::repeat_n(b']', depth - 1));
            s.push(b'}');
            s
        };
        assert!(strict_object(&deep(128)).is_ok());
        assert_eq!(
            strict_object(&deep(129)).unwrap_err().reason,
            "json-too-deep"
        );
        assert_eq!(
            strict_object(&deep(1_000_000)).unwrap_err().reason,
            "json-too-deep"
        );
    }

    fn meta(json: &str) -> Map<String, Value> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn an_unrecognised_token_is_never_allowed() {
        let v = |version, json: &str| meta_verdict(version, &meta(json)).verdict;
        use Verdict::*;
        use VrmVersion::*;
        for (version, json, want) in [
            (
                V0,
                r#"{"licenseName":"CC_BY","allowedUserName":"Everyone"}"#,
                Allowed,
            ),
            (
                V0,
                r#"{"licenseName":"CC0","allowedUserName":"Everyone"}"#,
                Allowed,
            ),
            (
                V0,
                r#"{"licenseName":"Other","allowedUserName":"Everyone"}"#,
                Unknown,
            ),
            (
                V0,
                r#"{"licenseName":"CC-BY","allowedUserName":"Everyone"}"#,
                Unknown,
            ),
            (
                V0,
                r#"{"licenseName":"CC_BY","allowedUserName":"everyone"}"#,
                Unknown,
            ),
            (
                V0,
                r#"{"licenseName":"Other","allowedUserName":"OnlyAuthor"}"#,
                Prohibited,
            ),
            (
                V0,
                r#"{"licenseName":1,"allowedUserName":"Everyone"}"#,
                Prohibited,
            ),
            (V0, r#"{"allowedUserName":"Everyone"}"#, Prohibited),
            (
                V1,
                r#"{"allowRedistribution":true,"avatarPermission":"everyone"}"#,
                Allowed,
            ),
            (
                V1,
                r#"{"allowRedistribution":"true","avatarPermission":"everyone"}"#,
                Prohibited,
            ),
            (
                V1,
                r#"{"allowRedistribution":1,"avatarPermission":"everyone"}"#,
                Prohibited,
            ),
            (
                V1,
                r#"{"allowRedistribution":true,"avatarPermission":"Everyone"}"#,
                Unknown,
            ),
            (V1, r#"{"allowRedistribution":true}"#, Prohibited),
        ] {
            assert_eq!(v(version, json), want, "{json}");
        }
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

    /// A file of the release whose first bytes decide it is never read whole: a big binary, a
    /// UTF-16 text whose first character is no `{`.
    #[test]
    fn only_what_the_rules_need_is_read_whole() {
        let mut pak = b"\xff\xfe".to_vec();
        pak.extend(std::iter::repeat_n([0x00, 0xd8], 1 << 20).flatten());
        let big = pak.len() as u64;
        assert!(!needs_whole("data/level.pak", &pak[..PREFIX], big));
        assert!(!needs_whole("plugins/a.dll", b"MZ\x90\0", 1 << 20));
        assert!(needs_whole("data/a.json", b"  {\"a\"", 1 << 20));
        assert!(needs_whole("data/a.json", b"        ", 1 << 20));
        assert!(needs_whole("mods/a.jar", b"PK\x03\x04", 1 << 20));
        assert!(!needs_whole("packs/a.astrapack", b"PK\x03\x04", 1 << 20));
        assert!(needs_whole("m/a.glb", b"glTF\x02\0\0\0", 1 << 20));
        assert!(!needs_whole("m/a.bin", b"glTF\x02\0\0\0", 1 << 20));
        assert!(needs_whole("m/a.gltf", b"xx", 1 << 20));
        // And judged so, the release holds what the whole file would make it.
        let files: std::collections::HashMap<String, Vec<u8>> = [
            ("astra-gi.toml".to_owned(), b"schema = 1".to_vec()),
            ("data/level.pak".to_owned(), pak),
        ]
        .into_iter()
        .collect();
        let r = judge_files(&[], &files).unwrap();
        assert!(r.refusals.is_empty() && r.flags.is_empty(), "{r:?}");
    }
}
