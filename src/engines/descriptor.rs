// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `astra.engine/1` schema: parsing, validation, the canonical form and its digest, every
//! limit and every reserved name. ONE module, so the public specification's goldens pin it.
//!
//! A descriptor is one JSON object (the catalogue carries it as JCS JSON inside a signed entry)
//! or the same object as TOML (`[integration.engine]` in a foundation's manifest, or a file).
//! Both parse to the same value; its digest is `sha256(JCS(value))`, so the two spellings of one
//! descriptor have one digest. Refused at parse: floats, datetimes, `null`, duplicate keys,
//! integers past 2^53 − 1, any key the schema does not name (at every level), and anything over
//! [`limits::DESCRIPTOR_BYTES`]. An unknown `schema` is `descriptor-schema-unknown` before any
//! other key is looked at, so a newer schema is skipped whole, never half read.
//!
//! **`mods`** (the specification's §7, [`super::mods`]): an optional top-level table saying how
//! to recognise the player's own mod loader. It must be a table (anything else is
//! `descriptor-invalid`, field `mods`, and skips the whole descriptor, as launcher 0.5.0 did),
//! and what it holds keeps to the value rules above; it is part of the canonical form, so of
//! the digest and of the size limit. What is wrong INSIDE it voids only `mods`
//! ([`Descriptor::mods_refusals`]: `mods-invalid{field, reason}`), never the descriptor. Its
//! model ([`Descriptor::mods`]) is kept beside the one the evaluator reads, never in it, so no
//! detection, no offer and no plan can depend on it.
//!
//! ```toml
//! schema = "astra.engine/1"
//! engine = { codes = { source2 = "Source 2" } }
//!
//! [[rule]]
//! tier = "strong"
//! programs = { ext = ["exe"], at = "{project}/bin/win64", first = 8 }
//! require = [ { listed = "engine2.dll", kind = "file" },              # the literal anchor
//!             { child = "{project}/core", kind = "dir", as = "core" } ]
//! code = "source2"
//! confidence = "high"
//! binary = "windows"
//! evidence = "{core}"
//! ```
//!
//! **Paths.** A path is `/`-separated, relative, with no `.` or `..`, no `\`, no empty part. It
//! is relative to the program's own folder unless its first part is one of the path
//! placeholders `{dir}` (that folder), `{program}`, `{project}`, `{project.parent}` or a capture
//! (`as = "data"` → `{data}`), which stand alone as that first part. The name placeholders
//! `{name}`, `{stem}` and `{project.leaf}` may sit anywhere inside a part. A `*` glob only in
//! the last part, at most two, and only in `listed` and `child`.
//!
//! **Lookups.** `listed`: a name in a folder's listing — the program's folder as the search saw
//! it (the walk's listing in the folder search, a fresh one for Steam's hint), any other folder
//! freshly read; the exact spelling first, else ignoring ASCII case; never a wrapper's name.
//! `child`: each part resolved below the base ignoring ASCII case, one at a time, never through
//! a link. `stat`: the exact spelling, each part checked to be a real folder (the last of the
//! `kind` asked), never through a link. `kind` (`file` or `dir`) is mandatory on all three.
//!
//! **Probes.** `tail`: the last `bytes` (≤ 16) of a regular file equal `equals`, the file at least
//! `min_len` long. `version.head`: the first [`limits::HEAD_BYTES`] of up to three files, the
//! first match of `template` (`N{1,4}` digits, `[abf]` one of, any other printable ASCII
//! itself), never starting right after a digit. Every file is opened without following a link
//! and without blocking on a FIFO, and read only when it is a regular file.
//!
//! **First-party only** (Astra's own descriptors; the built-in Unity, Godot and Unreal ones in
//! `builtin/*.toml` are written with them): `weak_order`; `walk.list_at_limit`; `scope = "anywhere"` (the first
//! match in walk order answers, unranked) and `scope = "named"` (asked only about a program the
//! caller names, never in detection), both on weak rules; `programs = "none"` (or `{ none = true,
//! skip_dirs = … }`), with `scope = "anywhere"` and a first `listed` that begins `{project}`;
//! `skip_wrappers = false` and `"folders"`; the `name` check (a `*` glob over the program's own
//! name, ignoring ASCII case); rules with no literal anchor. Pass B admits `weak_order`,
//! `skip_wrappers = false` and anchorless rules from a first-party descriptor; the rest only the
//! built-ins use, and Pass B refuses them as `descriptor-unsupported`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

use super::EngineCode;
use super::mods::{Mods, ModsFinding};
use crate::detect::{Confidence, Engine, is_wrapper_name};
use crate::digest::jcs;
use crate::error::{LauncherError, Result, codes};
use crate::paths::check_component;

/// The one schema this launcher reads.
pub const SCHEMA: &str = "astra.engine/1";

/// Every bound, parse-time and evaluation-time. Counted, never timed.
pub mod limits {
    /// The raw text of one descriptor, before it is parsed at all.
    pub const INPUT_BYTES: usize = 64 * 1024;
    /// One descriptor's canonical (JCS) JSON.
    pub const DESCRIPTOR_BYTES: usize = 16 * 1024;
    /// Engine codes one descriptor declares.
    pub const CODES: usize = 4;
    /// A display name, in characters.
    pub const NAME_CHARS: usize = 48;
    pub const RULES: usize = 16;
    /// Top-level checks in one rule's `require`, entries in its `classify`, and members of one
    /// `any`/`all`/`none`.
    pub const CHECKS: usize = 8;
    /// Checks of every kind in one rule, nested ones included.
    pub const ATOMS_PER_RULE: usize = 32;
    /// `any`/`all`/`none` inside one another.
    pub const NESTING: usize = 3;
    /// Programs one rule considers in the folder search (`programs.first`, mandatory in Pass B).
    pub const FIRST: u32 = 8;
    /// One path template, in bytes, and in `/`-separated parts.
    pub const TEMPLATE_BYTES: usize = 255;
    pub const SEGMENTS: usize = 8;
    /// `*` in one path.
    pub const STARS: usize = 2;
    /// `programs.ext` entries, each at most `EXT_CHARS` long.
    pub const EXTS: usize = 4;
    pub const EXT_CHARS: usize = 8;
    pub const SUFFIX_CHARS: usize = 64;
    pub const SKIP_DIRS: usize = 8;
    pub const WRAPPERS: usize = 16;
    pub const WRAPPER_CHARS: usize = 32;
    pub const ANTI_CHEAT_MARKERS: usize = 8;
    pub const BINARY_MAP: usize = 8;
    /// `version.head`: files read, and bytes read from each.
    pub const HEAD_FILES: usize = 3;
    pub const HEAD_BYTES: u64 = 4096;
    /// `version.template`: tokens, and the longest version it can match.
    pub const TEMPLATE_TOKENS: usize = 16;
    pub const VERSION_CHARS: usize = 64;
    pub const TAIL_BYTES: u32 = 16;
    /// The shortest anchor name.
    pub const ANCHOR_CHARS: usize = 5;

    /// Per descriptor, per game: lookups (every `listed` and `stat`, every part a `child`
    /// resolves, every program's magic, every size `pick` reads) and probes (every `tail`).
    /// Over either → that descriptor yields nothing for that game.
    pub const LOOKUPS_PER_DESCRIPTOR: u32 = 64;
    pub const PROBES_PER_DESCRIPTOR: u32 = 8;
    /// Per game, all descriptors together: lookups, bytes read, and evidence folders sized to
    /// break a tie. Over any → Pass B gives that game no answer at all.
    pub const LOOKUPS_PER_GAME: u32 = 1024;
    pub const BYTES_PER_GAME: u64 = 64 * 1024;
    pub const SIZINGS_PER_GAME: u32 = 4;

    /// Pass B asks at most this many descriptors, at most `PER_PUBLISHER` from one publisher.
    pub const PASS_B_DESCRIPTORS: usize = 64;
    pub const PER_PUBLISHER: usize = 4;
}

/// Beyond the built-in codes and their families (`unity…`, `unreal…`, `godot…`): codes that may
/// not begin with these, after folding ([`fold_confusables`]).
pub const RESERVED_CODE_PREFIXES: &[&str] = &["unknown", "astra"];
/// Codes that may not hold these anywhere, after folding.
pub const RESERVED_CODE_INFIXES: &[&str] = &["ambiguous"];

/// Words no display name may hold where one of its words begins, after folding and with the
/// separators between its words dropped (so `Un real`, `V.e.r.i.f.i.e.d` and `My Un ity` read as
/// what they spell): Astra's own tiers and team, words that would read as a review, and what
/// reads as a built-in engine beyond its family name (`UE5`, `Юнити`). The built-in engines'
/// family names are added to these ([`check_pass_b`](Descriptor::check_pass_b)). A word holding
/// `-` is looked for anywhere in the folded name instead.
pub const RESERVED_NAME_WORDS: &[&str] = &[
    "ue4",
    "ue5",
    "юнити",
    "унити",
    "анриал",
    "анреал",
    "годот",
    "astra",
    "астра",
    "minice",
    "moderated",
    "moderator",
    "reviewed",
    "verified",
    "official",
    "first-party",
    "firstparty",
    "recommended",
    "trusted",
    "built-on",
    "builton",
    "проверен",
    "модерат",
    "официальн",
    "рекоменд",
];

/// Characters no display name holds, beyond controls, `Bidi_Control` and
/// `Default_Ignorable_Code_Point`: the separators the UI draws between a name and its badges,
/// and their look-alikes.
pub const FORBIDDEN_NAME_CHARS: &[char] = &[
    '\u{00B7}', '|', '\u{0387}', '\u{01C0}', '\u{05C0}', '\u{16EB}', '\u{2022}', '\u{2027}',
    '\u{2219}', '\u{2223}', '\u{22C5}', '\u{23D0}', '\u{2758}', '\u{2E31}', '\u{30FB}', '\u{FE31}',
    '\u{FE33}', '\u{FF5C}', '\u{FF65}', '\u{2028}', '\u{2029}',
];

/// Name prefixes (lower case) that no anchor may begin with: runtimes and middleware so many
/// games ship that an anchor on one would claim them all.
pub const UBIQUITOUS_RUNTIME_PREFIXES: &[&str] = &[
    "steam_api",
    "steamclient",
    "steam_appid",
    "steamworks",
    "sdkencryptedappticket",
    "vcruntime",
    "msvcp",
    "msvcr",
    "vccorlib",
    "concrt",
    "ucrtbase",
    "api-ms-win-",
    "d3dcompiler_",
    "d3dx",
    "d3d8",
    "d3d9",
    "d3d10",
    "d3d11",
    "d3d12",
    "dxgi",
    "dxil",
    "dxcompiler",
    "dinput",
    "xinput",
    "xaudio",
    "x3daudio",
    "xapofx",
    "openal",
    "libogg",
    "libvorbis",
    "ogg.dll",
    "vorbis",
    "fmod",
    "bink",
    "nvngx",
    "nvapi",
    "amd_ags",
    "amd_fidelityfx",
    "ffx_",
    "sl.interposer",
    "galaxy",
    "eossdk",
    "discord",
    "sdl2",
    "sdl3",
    "libsdl",
    "libgcc",
    "libstdc++",
    "libc.so",
    "libm.so",
    "libpthread",
    "libdl",
    "libz.",
    "zlib",
    "libcrypto",
    "libssl",
    "libcurl",
    "vulkan",
    "opengl",
    "physx",
    "nvtt",
    "dbghelp",
    "crashrpt",
    "crashpad",
    "breakpad",
    "icudt",
    "icuuc",
    "icuin",
    "libcef",
    "chrome_elf",
    "unins",
    "oo2core",
    "oo2net",
    "openvr_api",
    "openxr_loader",
    "mono-2.0",
    "monobleedingedge",
    "desktop.ini",
    "thumbs.db",
];

/// Folder names (lower case) too common to anchor on.
pub const GENERIC_ANCHOR_DIRS: &[&str] = &[
    "data",
    "bin",
    "binaries",
    "content",
    "assets",
    "resources",
    "res",
    "plugins",
    "lib",
    "lib64",
    "libs",
    "x64",
    "x86",
    "win64",
    "win32",
    "engine",
    "localization",
    "locale",
    "lang",
    "languages",
    "logs",
    "saves",
    "config",
    "docs",
    "manual",
    "movies",
    "videos",
    "video",
    "sound",
    "sounds",
    "audio",
    "music",
    "shaders",
    "fonts",
    "redist",
    "_commonredist",
    "directx",
    "dotnet",
    "mods",
    "scripts",
    "maps",
    "levels",
    "textures",
    "models",
    "packages",
    "cache",
    "tools",
    "extras",
    "support",
    "common",
];

/// File names (lower case) too common to anchor on.
pub const GENERIC_ANCHOR_FILES: &[&str] = &[
    "readme.txt",
    "readme.md",
    "license.txt",
    "licence.txt",
    "license.md",
    "eula.txt",
    "changelog.txt",
    "credits.txt",
    "version.txt",
    "config.ini",
    "settings.ini",
    "options.ini",
    "icon.ico",
    "icon.png",
    "installscript.vdf",
];

/// The program kinds Pass B considers: these extensions, or none.
pub const PASS_B_PROGRAM_EXTS: &[&str] = &["exe", "x86_64", "x86"];

/// What a Latin-1 or Latin Extended-A letter (lower case) reads as without its marks: `é` → `e`,
/// `ł` → `l`, `æ` → `ae`, `ß` → `ss`. `None` for any other character.
fn latin_base(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ð' | 'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' | 'ĸ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ſ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// A code or a name as a person reads it: lower case, Latin letters without their marks
/// (`Unreál` → `unreal`), `_`/`-`/space/`.` dropped, look-alike letters and digits (Latin,
/// Cyrillic, Greek) folded together, `rn`→`m`, `vv`→`w`, `cl`→`d`. A display name may hold no
/// other script for this to miss ([`name_char_allowed`]).
pub fn fold_confusables(s: &str) -> String {
    let mut base = String::with_capacity(s.len());
    for ch in s.chars() {
        let ch = ch.to_lowercase().next().unwrap_or(ch);
        match latin_base(ch) {
            Some(b) => base.push_str(b),
            None => base.push(ch),
        }
    }
    let mut out = String::with_capacity(base.len());
    for ch in base.chars() {
        let c = match ch {
            '0' | 'о' | 'ο' | 'օ' => 'o',
            '1' | 'i' | 'l' | '|' | 'і' | 'ї' | 'ι' | 'ӏ' | '!' => 'l',
            '5' | 'ѕ' | '$' => 's',
            '2' => 'z',
            '3' | 'е' | 'ё' | 'ε' | 'є' => 'e',
            '4' | 'а' | 'α' | '@' => 'a',
            '6' | '8' | 'в' | 'β' => 'b',
            '7' | 'т' | 'τ' => 't',
            '9' => 'g',
            'р' | 'ρ' => 'p',
            'с' | 'ϲ' => 'c',
            'у' | 'γ' => 'y',
            'х' | 'χ' => 'x',
            'к' | 'κ' => 'k',
            'м' => 'm',
            'н' | 'η' | 'һ' => 'h',
            'ј' => 'j',
            'ԁ' => 'd',
            'ո' => 'n',
            'ս' | 'υ' => 'u',
            'ν' => 'v',
            '-' | '_' | ' ' | '.' | '\u{00A0}' => continue,
            c => c,
        };
        out.push(c);
    }
    out.replace("rn", "m").replace("vv", "w").replace("cl", "d")
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> LauncherError {
    LauncherError::new(codes::DESCRIPTOR_INVALID)
        .with("field", field.into())
        .with("reason", reason.into())
}

// ---- The parsed value: strict JSON, and TOML into the same shape. ----

/// The largest integer JCS (IEEE-754 doubles) carries exactly.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON without floats, nulls or duplicate keys")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<StrictValue, E> {
        Ok(StrictValue(Value::Bool(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<StrictValue, E> {
        if v.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err(E::custom("an integer past 2^53 - 1"));
        }
        Ok(StrictValue(Value::Number(Number::from(v))))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<StrictValue, E> {
        if v > MAX_SAFE_INTEGER {
            return Err(E::custom("an integer past 2^53 - 1"));
        }
        Ok(StrictValue(Value::Number(Number::from(v))))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<StrictValue, E> {
        Err(E::custom("floats are refused"))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<StrictValue, E> {
        Ok(StrictValue(Value::String(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> std::result::Result<StrictValue, E> {
        Ok(StrictValue(Value::String(v)))
    }

    fn visit_unit<E: de::Error>(self) -> std::result::Result<StrictValue, E> {
        Err(E::custom("null is refused"))
    }

    fn visit_none<E: de::Error>(self) -> std::result::Result<StrictValue, E> {
        Err(E::custom("null is refused"))
    }

    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<StrictValue, A::Error> {
        let mut out = Vec::new();
        while let Some(StrictValue(v)) = seq.next_element()? {
            out.push(v);
        }
        Ok(StrictValue(Value::Array(out)))
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<StrictValue, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
            let StrictValue(v) = map.next_value()?;
            out.insert(key, v);
        }
        Ok(StrictValue(Value::Object(out)))
    }
}

/// A JSON value as a descriptor may hold it: no float, no null, no integer past 2^53 − 1.
fn check_value(v: &Value, path: &str) -> Result<()> {
    match v {
        Value::Null => Err(invalid(path, "null is refused")),
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) if u <= MAX_SAFE_INTEGER => Ok(()),
            (_, Some(i)) if i.unsigned_abs() <= MAX_SAFE_INTEGER => Ok(()),
            _ => Err(invalid(
                path,
                "floats and integers past 2^53 - 1 are refused",
            )),
        },
        Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .try_for_each(|(i, v)| check_value(v, &format!("{path}[{i}]"))),
        Value::Object(o) => o.iter().try_for_each(|(k, v)| {
            check_value(
                v,
                &if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                },
            )
        }),
    }
}

fn toml_to_json(v: &toml::Value, path: &str) -> Result<Value> {
    Ok(match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => {
            if i.unsigned_abs() > MAX_SAFE_INTEGER {
                return Err(invalid(path, "an integer past 2^53 - 1"));
            }
            Value::Number(Number::from(*i))
        }
        toml::Value::Float(_) => return Err(invalid(path, "floats are refused")),
        toml::Value::Datetime(_) => return Err(invalid(path, "datetimes are refused")),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Array(a) => Value::Array(
            a.iter()
                .enumerate()
                .map(|(i, v)| toml_to_json(v, &format!("{path}[{i}]")))
                .collect::<Result<_>>()?,
        ),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| {
                    let p = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    Ok((k.clone(), toml_to_json(v, &p)?))
                })
                .collect::<Result<_>>()?,
        ),
    })
}

// ---- The schema, as serde reads it. Every struct denies unknown fields. ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    /// Checked before anything else is read ([`Descriptor::from_json`]).
    #[serde(rename = "schema")]
    _schema: String,
    engine: EngineSpec,
    #[serde(default)]
    walk: Option<WalkSpec>,
    rule: Vec<RuleSpec>,
    #[serde(default)]
    wrappers: Vec<String>,
    #[serde(default)]
    anti_cheat: Vec<MarkerSpec>,
    /// The module doc's `mods`: checked to be a table before this is read
    /// ([`Descriptor::from_value`]) and skipped here, so nothing of it reaches the [`Model`];
    /// [`super::mods::validate`] reads it beside.
    #[serde(default, rename = "mods")]
    _mods: Option<de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineSpec {
    codes: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WalkSpec {
    #[serde(default)]
    list_at_limit: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkerSpec {
    listed: String,
    kind: KindSpec,
    code: String,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum TierSpec {
    Strong,
    StrongFallback,
    Weak,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum ScopeSpec {
    Anywhere,
    Named,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum KindSpec {
    File,
    Dir,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSpec {
    tier: TierSpec,
    #[serde(default)]
    weak_order: Option<u32>,
    #[serde(default)]
    scope: Option<ScopeSpec>,
    programs: ProgramsSpec,
    #[serde(default)]
    require: Vec<CheckSpec>,
    #[serde(default)]
    classify: Vec<ClassifySpec>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    confidence: Option<Confidence>,
    #[serde(default)]
    passes: Option<bool>,
    #[serde(default)]
    evidence: Option<String>,
    binary: BinarySpec,
    #[serde(default)]
    named_alike: Option<NamedAlikeSpec>,
    #[serde(default)]
    bootstrap: Option<BootstrapSpec>,
    #[serde(default)]
    pick: Option<PickSpec>,
    #[serde(default)]
    version: Option<VersionSpec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProgramsSpec {
    Word(NoneWord),
    Select(SelectSpec),
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NoneWord {
    None,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectSpec {
    /// `true`: the rule names no program (`programs = "none"`, with folders to skip).
    #[serde(default)]
    none: Option<bool>,
    #[serde(default)]
    ext: Option<Vec<String>>,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    first: Option<u32>,
    #[serde(default)]
    also_if: Option<AlsoIfSpec>,
    #[serde(default)]
    skip_wrappers: Option<SkipWrappersSpec>,
    #[serde(default)]
    skip_dirs: Vec<String>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(untagged)]
enum SkipWrappersSpec {
    Bool(bool),
    Word(FoldersWord),
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum FoldersWord {
    Folders,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlsoIfSpec {
    dir: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckSpec {
    #[serde(default)]
    listed: Option<String>,
    #[serde(default)]
    child: Option<String>,
    #[serde(default)]
    stat: Option<String>,
    #[serde(default)]
    tail: Option<String>,
    #[serde(default)]
    any: Option<Vec<CheckSpec>>,
    #[serde(default)]
    all: Option<Vec<CheckSpec>>,
    #[serde(default)]
    none: Option<Vec<CheckSpec>>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<KindSpec>,
    #[serde(default, rename = "as")]
    capture: Option<String>,
    #[serde(default)]
    bytes: Option<u32>,
    #[serde(default)]
    equals: Option<String>,
    #[serde(default)]
    min_len: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassifySpec {
    #[serde(default)]
    when: Option<CheckSpec>,
    code: String,
    confidence: Confidence,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BinarySpec {
    Word(BinaryWord),
    FromName(FromNameSpec),
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum BinaryWord {
    Windows,
    Linux,
    ByExt,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FromNameSpec {
    from_name: String,
    map: BTreeMap<String, PlatformWord>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum PlatformWord {
    Windows,
    Linux,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NamedAlikeSpec {
    Bool(bool),
    Template(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapSpec {
    #[serde(default)]
    listed: Option<String>,
    #[serde(default)]
    hint_stem: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PickSpec {
    HintElseLargest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionSpec {
    head: Vec<String>,
    template: String,
}

// ---- The validated model the evaluator reads. ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Dir,
}

impl Kind {
    pub(crate) fn is_dir(self) -> bool {
        self == Kind::Dir
    }
}

impl From<KindSpec> for Kind {
    fn from(k: KindSpec) -> Kind {
        match k {
            KindSpec::File => Kind::File,
            KindSpec::Dir => Kind::Dir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuleTier {
    Strong,
    StrongFallback,
    Weak,
}

/// A placeholder that stands for a path (root-relative components).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathVar {
    Dir,
    Program,
    Project,
    ProjectParent,
    Capture(String),
}

/// A placeholder that stands for one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameVar {
    Name,
    Stem,
    ProjectLeaf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    Lit(String),
    Var(NameVar),
    Star,
}

/// A path template: a base (`None`: the program's folder) and the parts below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathT {
    pub(crate) base: Option<PathVar>,
    pub(crate) segs: Vec<Vec<Piece>>,
}

impl PathT {
    /// One literal name directly in the program's folder: what an anchor is.
    fn literal_name(&self) -> Option<String> {
        if self.base.is_some() || self.segs.len() != 1 {
            return None;
        }
        let mut s = String::new();
        for p in &self.segs[0] {
            match p {
                Piece::Lit(l) => s.push_str(l),
                _ => return None,
            }
        }
        Some(s)
    }

    fn has_star(&self) -> bool {
        self.segs.iter().flatten().any(|p| *p == Piece::Star)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Check {
    Listed {
        path: PathT,
        kind: Kind,
        capture: Option<String>,
    },
    Child {
        path: PathT,
        kind: Kind,
        capture: Option<String>,
    },
    Stat {
        path: PathT,
        kind: Kind,
        capture: Option<String>,
    },
    Tail {
        path: PathT,
        equals: Vec<u8>,
        min_len: u64,
    },
    /// The program's own name, ignoring ASCII case, against a `*` glob (lower case; first-party
    /// only). Reads nothing.
    Name(Vec<Piece>),
    Any(Vec<Check>),
    All(Vec<Check>),
    None(Vec<Check>),
}

#[derive(Debug, Clone)]
pub(crate) enum Outcome {
    Fixed(EngineCode, Confidence),
    Classify(Vec<(Option<Check>, EngineCode, Confidence)>),
}

#[derive(Debug, Clone)]
pub(crate) enum BinaryRule {
    Windows,
    Linux,
    ByExt,
    FromName {
        var: PathVar,
        map: Vec<(String, crate::detect::Binary)>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum NamedAlike {
    No,
    Yes,
    Template(Vec<Piece>),
}

/// A bootstrapper naming the rule's project: on disk (`listed`), or as Steam's launch hint's stem
/// (`hint_stem`), or either. At least one.
#[derive(Debug, Clone)]
pub(crate) struct Bootstrap {
    pub(crate) listed: Option<PathT>,
    pub(crate) hint_stem: Option<Vec<Piece>>,
}

/// One token of a version template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    Digits { min: u8, max: u8 },
    Set(Vec<u8>),
    Lit(u8),
}

#[derive(Debug, Clone)]
pub(crate) struct VersionRule {
    pub(crate) head: Vec<PathT>,
    pub(crate) template: Vec<Tok>,
}

#[derive(Debug, Clone)]
pub(crate) enum Select {
    /// Lower case; `""` is a program with no extension.
    Ext(Vec<String>),
    /// Lower case.
    Suffix(String),
}

/// `programs.at`: the folder the program sits in. `project` captures everything before `tail`
/// (at least one part); without it the folder is exactly `tail` (`""`: the top level).
#[derive(Debug, Clone)]
pub(crate) struct At {
    pub(crate) project: bool,
    pub(crate) tail: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Programs {
    pub(crate) select: Select,
    pub(crate) at: Option<At>,
    pub(crate) first: Option<u32>,
    pub(crate) also_if: Option<Vec<Piece>>,
}

/// `skip_wrappers`: which wrapper names keep a rule from looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrapperSkip {
    /// The program's own name, and every folder on its way (`true`, the default).
    All,
    /// Only the folders on its way (`"folders"`; first-party only).
    Folders,
    /// Neither (`false`; first-party only).
    None,
}

/// Where a rule never looks: below a wrapper's folder (and, unless [`WrapperSkip::Folders`], at a
/// wrapper's program), or below a folder named in `skip_dirs` (ignoring ASCII case).
#[derive(Debug, Clone)]
pub(crate) struct Skip {
    pub(crate) wrappers: WrapperSkip,
    pub(crate) dirs: Vec<String>,
}

/// Where a rule is asked (`scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// About each program it selects, from what sits around it: the default.
    Beside,
    /// Over the whole walk, the first match in walk order answering, unranked (first-party,
    /// weak rules only).
    Anywhere,
    /// Only about a program the caller names (planning's engine check), never in detection
    /// (first-party, weak rules only).
    Named,
}

#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub(crate) tier: RuleTier,
    pub(crate) weak_order: Option<u32>,
    pub(crate) scope: Scope,
    /// `None`: `programs = "none"`.
    pub(crate) programs: Option<Programs>,
    pub(crate) skip: Skip,
    pub(crate) require: Vec<Check>,
    pub(crate) outcome: Outcome,
    pub(crate) passes: bool,
    pub(crate) evidence: Option<PathT>,
    pub(crate) binary: BinaryRule,
    pub(crate) named_alike: NamedAlike,
    pub(crate) bootstrap: Option<Bootstrap>,
    pub(crate) pick: bool,
    pub(crate) version: Option<VersionRule>,
    /// The top-level literal `listed` names in the program's folder (lower case, with their
    /// kind): every one must be somewhere in the walk for the rule to be tried at all.
    pub(crate) anchors: Vec<(String, Kind)>,
}

impl Rule {
    /// Every check the rule makes, nested ones and `classify`'s included.
    pub(crate) fn checks(&self) -> impl Iterator<Item = &Check> {
        fn flat<'c>(c: &'c Check, out: &mut Vec<&'c Check>) {
            out.push(c);
            if let Check::Any(l) | Check::All(l) | Check::None(l) = c {
                for x in l {
                    flat(x, out);
                }
            }
        }
        let mut out = Vec::new();
        for c in &self.require {
            flat(c, &mut out);
        }
        if let Outcome::Classify(list) = &self.outcome {
            for (w, _, _) in list {
                if let Some(c) = w {
                    flat(c, &mut out);
                }
            }
        }
        out.into_iter()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Marker {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) code: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Model {
    pub(crate) codes: Vec<(EngineCode, String)>,
    pub(crate) list_at_limit: Vec<Vec<String>>,
    pub(crate) rules: Vec<Rule>,
    /// Lower case; matched as substrings, on top of the built-in wrapper names.
    pub(crate) wrappers: Vec<String>,
    pub(crate) markers: Vec<Marker>,
}

impl Model {
    /// Whether `name` (a program or a folder on its way) is a wrapper for this descriptor: the
    /// built-in wrappers, and its own additions.
    pub(crate) fn is_wrapper(&self, name: &str) -> bool {
        if is_wrapper_name(name) {
            return true;
        }
        if self.wrappers.is_empty() {
            return false;
        }
        let lower = name.to_ascii_lowercase();
        self.wrappers.iter().any(|w| lower.contains(w.as_str()))
    }
}

// ---- Validation: Spec → Model. ----

/// What a template in one rule may name.
struct Vars<'a> {
    program: bool,
    project: bool,
    captures: &'a [String],
}

enum VarKind {
    Path(PathVar),
    Name(NameVar),
}

fn classify_var(v: &str, vars: &Vars, field: &str) -> Result<VarKind> {
    let need = |ok: bool, kind: VarKind| {
        if ok {
            Ok(kind)
        } else {
            Err(invalid(field, format!("{{{v}}} is not available here")))
        }
    };
    match v {
        "dir" => need(vars.program, VarKind::Path(PathVar::Dir)),
        "program" => need(vars.program, VarKind::Path(PathVar::Program)),
        "name" => need(vars.program, VarKind::Name(NameVar::Name)),
        "stem" => need(vars.program, VarKind::Name(NameVar::Stem)),
        "project" => need(vars.project, VarKind::Path(PathVar::Project)),
        "project.parent" => need(vars.project, VarKind::Path(PathVar::ProjectParent)),
        "project.leaf" => need(vars.project, VarKind::Name(NameVar::ProjectLeaf)),
        c if vars.captures.iter().any(|k| k == c) => {
            Ok(VarKind::Path(PathVar::Capture(c.to_owned())))
        }
        _ => Err(invalid(field, format!("unknown placeholder {{{v}}}"))),
    }
}

enum RawPiece {
    Lit(String),
    Var(String),
    Star,
}

fn bad_char(c: char) -> bool {
    c < ' ' || c == '\u{7f}' || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '\\' | '/')
}

fn raw_pieces(seg: &str, field: &str) -> Result<Vec<RawPiece>> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if !lit.is_empty() {
                    out.push(RawPiece::Lit(std::mem::take(&mut lit)));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(ch)
                            if ch.is_ascii_lowercase()
                                || ch.is_ascii_digit()
                                || ch == '.'
                                || ch == '_' =>
                        {
                            name.push(ch)
                        }
                        _ => return Err(invalid(field, "a placeholder is not closed")),
                    }
                }
                if name.is_empty() {
                    return Err(invalid(field, "an empty placeholder"));
                }
                out.push(RawPiece::Var(name));
            }
            '}' => return Err(invalid(field, "a '}' with no '{'")),
            '*' => {
                if !lit.is_empty() {
                    out.push(RawPiece::Lit(std::mem::take(&mut lit)));
                }
                out.push(RawPiece::Star);
            }
            c if bad_char(c) => {
                return Err(invalid(field, format!("{c:?} is not allowed in a path")));
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        out.push(RawPiece::Lit(lit));
    }
    Ok(out)
}

fn parse_path(s: &str, vars: &Vars, glob: bool, field: &str) -> Result<PathT> {
    if s.is_empty() || s.len() > limits::TEMPLATE_BYTES {
        return Err(invalid(field, "empty, or longer than a path may be"));
    }
    if s.starts_with('/') || s.ends_with('/') || s.contains("//") || s.contains('\\') {
        return Err(invalid(field, "not a relative `/`-separated path"));
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() > limits::SEGMENTS {
        return Err(invalid(field, "too many parts"));
    }
    let mut base = None;
    let mut segs = Vec::new();
    let mut stars = 0;
    for (i, part) in parts.iter().enumerate() {
        let raw = raw_pieces(part, field)?;
        if let [RawPiece::Var(v)] = raw.as_slice()
            && let VarKind::Path(pv) = classify_var(v, vars, field)?
        {
            if i != 0 {
                return Err(invalid(field, format!("{{{v}}} may only begin a path")));
            }
            base = Some(pv);
            continue;
        }
        let mut seg = Vec::new();
        for p in raw {
            match p {
                RawPiece::Lit(l) => seg.push(Piece::Lit(l)),
                RawPiece::Var(v) => match classify_var(&v, vars, field)? {
                    VarKind::Name(n) => seg.push(Piece::Var(n)),
                    VarKind::Path(_) => {
                        return Err(invalid(field, format!("{{{v}}} must stand alone")));
                    }
                },
                RawPiece::Star => {
                    if !glob {
                        return Err(invalid(field, "no glob here"));
                    }
                    if i + 1 != parts.len() {
                        return Err(invalid(field, "a glob only in the last part"));
                    }
                    stars += 1;
                    seg.push(Piece::Star);
                }
            }
        }
        if seg.iter().all(|p| matches!(p, Piece::Lit(_))) {
            let lit: String = seg
                .iter()
                .map(|p| match p {
                    Piece::Lit(l) => l.as_str(),
                    _ => "",
                })
                .collect();
            if !check_component(&lit) {
                return Err(invalid(field, format!("{lit:?} is not a safe name")));
            }
        }
        segs.push(seg);
    }
    if stars > limits::STARS {
        return Err(invalid(field, "too many globs"));
    }
    Ok(PathT { base, segs })
}

/// One name: literal text and name placeholders, no `/`, no glob.
fn parse_name(s: &str, vars: &Vars, field: &str) -> Result<Vec<Piece>> {
    if s.is_empty() || s.len() > limits::TEMPLATE_BYTES || s.contains('/') {
        return Err(invalid(field, "not one name"));
    }
    let mut out = Vec::new();
    for p in raw_pieces(s, field)? {
        match p {
            RawPiece::Lit(l) => out.push(Piece::Lit(l)),
            RawPiece::Var(v) => match classify_var(&v, vars, field)? {
                VarKind::Name(n) => out.push(Piece::Var(n)),
                VarKind::Path(_) => {
                    return Err(invalid(field, format!("{{{v}}} is a path, not a name")));
                }
            },
            RawPiece::Star => return Err(invalid(field, "no glob here")),
        }
    }
    if let [Piece::Lit(l)] = out.as_slice()
        && !check_component(l)
    {
        return Err(invalid(field, format!("{l:?} is not a safe name")));
    }
    Ok(out)
}

/// A path that is one placeholder and nothing else, of an allowed kind.
fn parse_var_only(
    s: &str,
    vars: &Vars,
    field: &str,
    allowed: fn(&PathVar) -> bool,
) -> Result<PathVar> {
    let t = parse_path(s, vars, false, field)?;
    match t.base {
        Some(v) if t.segs.is_empty() && allowed(&v) => Ok(v),
        _ => Err(invalid(field, "must be a capture, {program} or {project}")),
    }
}

fn parse_at(s: &str, field: &str) -> Result<At> {
    if s.is_empty() {
        return Ok(At {
            project: false,
            tail: Vec::new(),
        });
    }
    if s.starts_with('/') || s.ends_with('/') || s.contains("//") || s.contains('\\') {
        return Err(invalid(field, "not a relative `/`-separated path"));
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() > limits::SEGMENTS {
        return Err(invalid(field, "too many parts"));
    }
    let project = parts[0] == "{project}";
    let mut tail = Vec::new();
    for p in &parts[usize::from(project)..] {
        if p.contains(['{', '}', '*']) || !check_component(p) {
            return Err(invalid(
                field,
                "only {project} first, then literal folder names",
            ));
        }
        tail.push((*p).to_owned());
    }
    Ok(At { project, tail })
}

fn parse_template(s: &str, field: &str) -> Result<Vec<Tok>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'N' => {
                // N{m,n}
                let rest = &s[i + 1..];
                let close = rest.find('}').ok_or_else(|| invalid(field, "N{m,n}"))?;
                let inner = rest
                    .strip_prefix('{')
                    .map(|r| &r[..close - 1])
                    .ok_or_else(|| invalid(field, "N{m,n}"))?;
                let (m, n) = inner
                    .split_once(',')
                    .ok_or_else(|| invalid(field, "N{m,n}"))?;
                let (m, n): (u8, u8) = (
                    m.parse().map_err(|_| invalid(field, "N{m,n}"))?,
                    n.parse().map_err(|_| invalid(field, "N{m,n}"))?,
                );
                if m == 0 || m > n || n > 9 {
                    return Err(invalid(field, "N{m,n} with 1 <= m <= n <= 9"));
                }
                out.push(Tok::Digits { min: m, max: n });
                i += 1 + close + 1;
            }
            b'[' => {
                let close = s[i..].find(']').ok_or_else(|| invalid(field, "[...]"))?;
                let set: Vec<u8> = b[i + 1..i + close].to_vec();
                let unique: BTreeSet<u8> = set.iter().copied().collect();
                if set.is_empty()
                    || set.len() > 16
                    || unique.len() != set.len()
                    || !set
                        .iter()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                {
                    return Err(invalid(field, "[...] of 1 to 16 distinct a-z 0-9"));
                }
                out.push(Tok::Set(set));
                i += close + 1;
            }
            b']' | b'{' | b'}' | b'\\' => return Err(invalid(field, "unbalanced template")),
            c if c.is_ascii_graphic() => {
                out.push(Tok::Lit(c));
                i += 1;
            }
            _ => return Err(invalid(field, "printable ASCII only")),
        }
    }
    let longest: usize = out
        .iter()
        .map(|t| match t {
            Tok::Digits { max, .. } => usize::from(*max),
            _ => 1,
        })
        .sum();
    if out.is_empty()
        || out.len() > limits::TEMPLATE_TOKENS
        || longest > limits::VERSION_CHARS
        || !out.iter().any(|t| matches!(t, Tok::Digits { .. }))
    {
        return Err(invalid(field, "1 to 16 tokens, at least one N{m,n}"));
    }
    Ok(out)
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn is_default_ignorable(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}'..='\u{1160}' | '\u{17B4}'..='\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{FFF0}'..='\u{FFF8}' | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E0FFF}')
}

/// Whether a display name may hold `c`: printable ASCII, a Latin-1 or Latin Extended-A letter
/// ([`fold_confusables`] reads it without its marks), or a Cyrillic letter. Nothing else —
/// fullwidth and mathematical letters, small capitals, enclosed and letter-like symbols, Roman
/// numerals, combining marks, Greek, any other script — because each can spell a reserved word
/// (`Ｕｎｉｔｙ`, `𝐔𝐧𝐢𝐭𝐲`, `ᴜɴɪᴛʏ`, `Unⅰty`, `Unreál`) that no fold of a closed list can see.
pub fn name_char_allowed(c: char) -> bool {
    matches!(c,
        ' '..='~'
        | '\u{00C0}'..='\u{00D6}'
        | '\u{00D8}'..='\u{00F6}'
        | '\u{00F8}'..='\u{017F}'
        | '\u{0400}'..='\u{0482}'
        | '\u{048A}'..='\u{04FF}')
}

/// A display name as a person reads it: 1 to 48 characters, trimmed, every one allowed
/// ([`name_char_allowed`]) and none a separator the UI draws ([`FORBIDDEN_NAME_CHARS`]).
fn check_display_name(name: &str, field: &str) -> Result<()> {
    let n = name.chars().count();
    if n == 0 || n > limits::NAME_CHARS || name.trim() != name {
        return Err(invalid(field, "1 to 48 characters, trimmed"));
    }
    if !display_chars_ok(name) {
        return Err(invalid(field, "a character a name may not hold"));
    }
    Ok(())
}

/// Every character of `name` one a display name may hold: [`check_display_name`]'s second half,
/// which a mod loader's `name` is also held to (`super::mods`).
pub(crate) fn display_chars_ok(name: &str) -> bool {
    !name.chars().any(|c| {
        !name_char_allowed(c)
            || c.is_control()
            || is_bidi_control(c)
            || is_default_ignorable(c)
            || FORBIDDEN_NAME_CHARS.contains(&c)
    })
}

struct RuleCtx<'a> {
    own: &'a [(EngineCode, String)],
    atoms: usize,
}

fn parse_check(
    spec: &CheckSpec,
    vars: &Vars,
    captures: &mut Vec<String>,
    depth: usize,
    top: bool,
    field: &str,
    ctx: &mut RuleCtx,
) -> Result<Check> {
    ctx.atoms += 1;
    if ctx.atoms > limits::ATOMS_PER_RULE {
        return Err(invalid(field, "too many checks in one rule"));
    }
    let primaries = [
        spec.listed.is_some(),
        spec.child.is_some(),
        spec.stat.is_some(),
        spec.tail.is_some(),
        spec.any.is_some(),
        spec.all.is_some(),
        spec.none.is_some(),
        spec.name.is_some(),
    ];
    if primaries.iter().filter(|p| **p).count() != 1 {
        return Err(invalid(
            field,
            "exactly one of listed, child, stat, tail, name, any, all, none",
        ));
    }
    if let Some(n) = &spec.name {
        if spec.kind.is_some()
            || spec.capture.is_some()
            || spec.bytes.is_some()
            || spec.equals.is_some()
            || spec.min_len.is_some()
        {
            return Err(invalid(field, "name takes its glob only"));
        }
        if !vars.program {
            return Err(invalid(field, "a name check needs a program"));
        }
        if n.is_empty() || n.len() > limits::TEMPLATE_BYTES || n.contains('/') {
            return Err(invalid(field, "one name"));
        }
        let mut glob = Vec::new();
        for p in raw_pieces(n, field)? {
            match p {
                RawPiece::Lit(l) => glob.push(Piece::Lit(l.to_ascii_lowercase())),
                RawPiece::Star => glob.push(Piece::Star),
                RawPiece::Var(_) => return Err(invalid(field, "no placeholder in a name check")),
            }
        }
        if glob.iter().filter(|p| **p == Piece::Star).count() > limits::STARS {
            return Err(invalid(field, "too many globs"));
        }
        return Ok(Check::Name(glob));
    }
    let lookup = |path: &str, glob: bool, vars: &Vars| -> Result<(PathT, Kind)> {
        if spec.bytes.is_some() || spec.equals.is_some() || spec.min_len.is_some() {
            return Err(invalid(field, "bytes, equals and min_len belong to tail"));
        }
        let kind = spec
            .kind
            .ok_or_else(|| invalid(field, "kind is required"))?;
        let t = parse_path(path, vars, glob, field)?;
        Ok((t, kind.into()))
    };
    let capture = |captures: &mut Vec<String>| -> Result<Option<String>> {
        let Some(c) = &spec.capture else {
            return Ok(None);
        };
        if !top {
            return Err(invalid(field, "`as` only at the top of require"));
        }
        let ok = c.len() <= 16
            && c.starts_with(|ch: char| ch.is_ascii_lowercase())
            && c.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
        let builtin = [
            "dir",
            "program",
            "name",
            "stem",
            "project",
            "project.parent",
            "project.leaf",
        ];
        if !ok || builtin.contains(&c.as_str()) || captures.contains(c) {
            return Err(invalid(
                field,
                "`as`: a new name, a-z 0-9 _, starting with a letter",
            ));
        }
        Ok(Some(c.clone()))
    };
    if let Some(p) = &spec.listed {
        let (path, kind) = lookup(p, true, vars)?;
        if path.segs.is_empty() {
            return Err(invalid(field, "listed names something below its base"));
        }
        let capture = capture(captures)?;
        if let Some(c) = &capture {
            captures.push(c.clone());
        }
        return Ok(Check::Listed {
            path,
            kind,
            capture,
        });
    }
    if let Some(p) = &spec.child {
        let (path, kind) = lookup(p, true, vars)?;
        if path.segs.is_empty() {
            return Err(invalid(field, "child names something below its base"));
        }
        let capture = capture(captures)?;
        if let Some(c) = &capture {
            captures.push(c.clone());
        }
        return Ok(Check::Child {
            path,
            kind,
            capture,
        });
    }
    if let Some(p) = &spec.stat {
        let (path, kind) = lookup(p, false, vars)?;
        if path.base.is_none() && path.segs.is_empty() {
            return Err(invalid(field, "stat names nothing"));
        }
        let capture = capture(captures)?;
        if let Some(c) = &capture {
            captures.push(c.clone());
        }
        return Ok(Check::Stat {
            path,
            kind,
            capture,
        });
    }
    if let Some(p) = &spec.tail {
        if spec.kind.is_some() || spec.capture.is_some() {
            return Err(invalid(field, "tail takes bytes, equals and min_len only"));
        }
        let path = parse_path(p, vars, false, field)?;
        let bytes = spec
            .bytes
            .ok_or_else(|| invalid(field, "bytes is required"))?;
        let equals = spec
            .equals
            .as_ref()
            .ok_or_else(|| invalid(field, "equals is required"))?;
        if bytes == 0 || bytes > limits::TAIL_BYTES || equals.len() != bytes as usize {
            return Err(invalid(
                field,
                "1 to 16 bytes, and equals exactly that long",
            ));
        }
        let min_len = spec.min_len.unwrap_or(u64::from(bytes));
        if min_len < u64::from(bytes) || min_len > MAX_SAFE_INTEGER {
            return Err(invalid(field, "min_len at least bytes"));
        }
        return Ok(Check::Tail {
            path,
            equals: equals.as_bytes().to_vec(),
            min_len,
        });
    }
    // any / all / none
    if spec.kind.is_some()
        || spec.capture.is_some()
        || spec.bytes.is_some()
        || spec.equals.is_some()
        || spec.min_len.is_some()
    {
        return Err(invalid(field, "any, all and none take their list only"));
    }
    if depth >= limits::NESTING {
        return Err(invalid(field, "any/all/none nested more than 3 deep"));
    }
    let (list, which) = if let Some(l) = &spec.any {
        (l, "any")
    } else if let Some(l) = &spec.all {
        (l, "all")
    } else {
        (spec.none.as_ref().expect("one primary"), "none")
    };
    if list.is_empty() || list.len() > limits::CHECKS {
        return Err(invalid(field, format!("{which}: 1 to 8 checks")));
    }
    let mut inner = Vec::new();
    for (i, c) in list.iter().enumerate() {
        inner.push(parse_check(
            c,
            vars,
            captures,
            depth + 1,
            false,
            &format!("{field}.{which}[{i}]"),
            ctx,
        )?);
    }
    Ok(match which {
        "any" => Check::Any(inner),
        "all" => Check::All(inner),
        _ => Check::None(inner),
    })
}

fn own_code(code: &str, ctx: &RuleCtx, field: &str, unknown_ok: bool) -> Result<EngineCode> {
    let c = EngineCode::new(code).ok_or_else(|| invalid(field, "not an engine code"))?;
    if c.is_unknown() && unknown_ok {
        return Ok(c);
    }
    if !ctx.own.iter().any(|(o, _)| *o == c) {
        return Err(invalid(field, "a code this descriptor does not declare"));
    }
    Ok(c)
}

fn parse_rule(r: &RuleSpec, own: &[(EngineCode, String)], field: &str) -> Result<Rule> {
    let tier = match r.tier {
        TierSpec::Strong => RuleTier::Strong,
        TierSpec::StrongFallback => RuleTier::StrongFallback,
        TierSpec::Weak => RuleTier::Weak,
    };
    if r.weak_order.is_some() && tier != RuleTier::Weak {
        return Err(invalid(format!("{field}.weak_order"), "weak rules only"));
    }
    let scope = match r.scope {
        None => Scope::Beside,
        Some(ScopeSpec::Anywhere) => Scope::Anywhere,
        Some(ScopeSpec::Named) => Scope::Named,
    };
    if scope != Scope::Beside && tier != RuleTier::Weak {
        return Err(invalid(format!("{field}.scope"), "weak rules only"));
    }
    let (programs, skip) = match &r.programs {
        ProgramsSpec::Word(NoneWord::None) => (
            None,
            Skip {
                wrappers: WrapperSkip::All,
                dirs: Vec::new(),
            },
        ),
        ProgramsSpec::Select(s) => parse_programs(s, &format!("{field}.programs"))?,
    };
    if programs.is_none() && scope != Scope::Anywhere {
        return Err(invalid(
            format!("{field}.programs"),
            "a rule with no program looks anywhere (scope = \"anywhere\")",
        ));
    }
    let has_program = programs.is_some();
    // With no program, `{project}` is what the first `listed` finds it to be, anywhere in the walk.
    let has_project = programs
        .as_ref()
        .is_none_or(|p| p.at.as_ref().is_some_and(|a| a.project));

    let mut ctx = RuleCtx { own, atoms: 0 };
    let mut captures: Vec<String> = Vec::new();
    if r.require.len() > limits::CHECKS {
        return Err(invalid(format!("{field}.require"), "at most 8 checks"));
    }
    let mut require = Vec::new();
    for (i, c) in r.require.iter().enumerate() {
        let snapshot = captures.clone();
        let vars = Vars {
            program: has_program,
            project: has_project,
            captures: &snapshot,
        };
        require.push(parse_check(
            c,
            &vars,
            &mut captures,
            0,
            true,
            &format!("{field}.require[{i}]"),
            &mut ctx,
        )?);
    }
    if !has_program
        && !matches!(
            require.first(),
            Some(Check::Listed {
                path: PathT {
                    base: Some(PathVar::Project),
                    ..
                },
                ..
            })
        )
    {
        return Err(invalid(
            format!("{field}.require"),
            "a rule with no program begins with a `listed` below {project}",
        ));
    }
    let vars = Vars {
        program: has_program,
        project: has_project,
        captures: &captures,
    };

    let outcome = match (&r.code, &r.confidence, r.classify.is_empty()) {
        (Some(code), Some(conf), true) => Outcome::Fixed(
            own_code(code, &ctx, &format!("{field}.code"), false)?,
            *conf,
        ),
        (None, None, false) => {
            if r.classify.len() > limits::CHECKS {
                return Err(invalid(format!("{field}.classify"), "at most 8 entries"));
            }
            let mut list = Vec::new();
            for (i, c) in r.classify.iter().enumerate() {
                let f = format!("{field}.classify[{i}]");
                let last = i + 1 == r.classify.len();
                let when = match (&c.when, last) {
                    (Some(w), _) => {
                        let v = Vars {
                            program: has_program,
                            project: has_project,
                            captures: &captures,
                        };
                        let mut no_new = captures.clone();
                        Some(parse_check(
                            w,
                            &v,
                            &mut no_new,
                            0,
                            false,
                            &format!("{f}.when"),
                            &mut ctx,
                        )?)
                    }
                    (None, true) => None,
                    (None, false) => {
                        return Err(invalid(f, "only the last entry has no `when`"));
                    }
                };
                if last && when.is_some() {
                    return Err(invalid(f, "the last entry has no `when`"));
                }
                list.push((when, own_code(&c.code, &ctx, &f, true)?, c.confidence));
            }
            if list.iter().all(|(_, c, _)| c.is_unknown()) {
                return Err(invalid(format!("{field}.classify"), "names no engine"));
            }
            Outcome::Classify(list)
        }
        _ => {
            return Err(invalid(
                field.to_owned(),
                "either code and confidence, or classify",
            ));
        }
    };

    let evidence = r
        .evidence
        .as_deref()
        .map(|e| {
            parse_var_only(e, &vars, &format!("{field}.evidence"), |v| {
                matches!(v, PathVar::Capture(_) | PathVar::Program | PathVar::Project)
            })
            .map(|base| PathT {
                base: Some(base),
                segs: Vec::new(),
            })
        })
        .transpose()?;

    let binary = match &r.binary {
        BinarySpec::Word(BinaryWord::Windows) => BinaryRule::Windows,
        BinarySpec::Word(BinaryWord::Linux) => BinaryRule::Linux,
        BinarySpec::Word(BinaryWord::ByExt) => {
            if !has_program {
                return Err(invalid(format!("{field}.binary"), "by-ext needs a program"));
            }
            BinaryRule::ByExt
        }
        BinarySpec::FromName(f) => {
            let ff = format!("{field}.binary.from_name");
            let var = parse_var_only(&f.from_name, &vars, &ff, |v| {
                matches!(v, PathVar::Capture(_) | PathVar::Program)
            })?;
            if f.map.is_empty() || f.map.len() > limits::BINARY_MAP {
                return Err(invalid(format!("{field}.binary.map"), "1 to 8 entries"));
            }
            let mut map = Vec::new();
            for (k, v) in &f.map {
                let ok = k.len() >= 2
                    && k.len() <= 32
                    && k.starts_with('-')
                    && k[1..]
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
                if !ok {
                    return Err(invalid(
                        format!("{field}.binary.map"),
                        "keys are `-` and lower-case letters",
                    ));
                }
                map.push((
                    k.clone(),
                    match v {
                        PlatformWord::Windows => crate::detect::Binary::Windows,
                        PlatformWord::Linux => crate::detect::Binary::Linux,
                    },
                ));
            }
            BinaryRule::FromName { var, map }
        }
    };

    let named_alike = match &r.named_alike {
        None | Some(NamedAlikeSpec::Bool(false)) => NamedAlike::No,
        Some(NamedAlikeSpec::Bool(true)) => NamedAlike::Yes,
        Some(NamedAlikeSpec::Template(t)) => {
            if !has_program {
                return Err(invalid(
                    format!("{field}.named_alike"),
                    "a name compares with a program",
                ));
            }
            NamedAlike::Template(parse_name(t, &vars, &format!("{field}.named_alike"))?)
        }
    };

    let bootstrap = r
        .bootstrap
        .as_ref()
        .map(|b| -> Result<Bootstrap> {
            if b.listed.is_none() && b.hint_stem.is_none() {
                return Err(invalid(
                    format!("{field}.bootstrap"),
                    "listed, hint_stem, or both",
                ));
            }
            let listed = b
                .listed
                .as_deref()
                .map(|l| -> Result<PathT> {
                    let f = format!("{field}.bootstrap.listed");
                    let listed = parse_path(l, &vars, false, &f)?;
                    if listed.segs.is_empty() {
                        return Err(invalid(f, "names nothing"));
                    }
                    Ok(listed)
                })
                .transpose()?;
            let hint_stem = b
                .hint_stem
                .as_deref()
                .map(|h| parse_name(h, &vars, &format!("{field}.bootstrap.hint_stem")))
                .transpose()?;
            Ok(Bootstrap { listed, hint_stem })
        })
        .transpose()?;

    let pick = matches!(r.pick, Some(PickSpec::HintElseLargest));
    if pick && !has_program {
        return Err(invalid(format!("{field}.pick"), "a pick needs programs"));
    }

    let version = r
        .version
        .as_ref()
        .map(|v| -> Result<VersionRule> {
            let f = format!("{field}.version");
            if v.head.is_empty() || v.head.len() > limits::HEAD_FILES {
                return Err(invalid(format!("{f}.head"), "1 to 3 files"));
            }
            let head = v
                .head
                .iter()
                .enumerate()
                .map(|(i, h)| parse_path(h, &vars, false, &format!("{f}.head[{i}]")))
                .collect::<Result<Vec<_>>>()?;
            Ok(VersionRule {
                head,
                template: parse_template(&v.template, &format!("{f}.template"))?,
            })
        })
        .transpose()?;

    let anchors = require
        .iter()
        .filter_map(|c| match c {
            Check::Listed { path, kind, .. } if !path.has_star() => {
                path.literal_name().map(|n| (n.to_ascii_lowercase(), *kind))
            }
            _ => None,
        })
        .collect();

    Ok(Rule {
        tier,
        weak_order: r.weak_order,
        scope,
        programs,
        skip,
        require,
        outcome,
        // `strong-fallback` is a strong match that does not pass (Unity before 2017.2), unless
        // it says otherwise; at one depth a rule that passes is chosen over it.
        passes: r.passes.unwrap_or(tier != RuleTier::StrongFallback),
        evidence,
        binary,
        named_alike,
        bootstrap,
        pick,
        version,
        anchors,
    })
}

fn parse_programs(s: &SelectSpec, field: &str) -> Result<(Option<Programs>, Skip)> {
    if s.skip_dirs.len() > limits::SKIP_DIRS || !s.skip_dirs.iter().all(|d| check_component(d)) {
        return Err(invalid(
            format!("{field}.skip_dirs"),
            "at most 8 folder names",
        ));
    }
    let skip = Skip {
        wrappers: match s.skip_wrappers {
            None | Some(SkipWrappersSpec::Bool(true)) => WrapperSkip::All,
            Some(SkipWrappersSpec::Bool(false)) => WrapperSkip::None,
            Some(SkipWrappersSpec::Word(FoldersWord::Folders)) => WrapperSkip::Folders,
        },
        dirs: s.skip_dirs.clone(),
    };
    match s.none {
        None | Some(false) => {}
        Some(true) => {
            if s.ext.is_some()
                || s.suffix.is_some()
                || s.at.is_some()
                || s.first.is_some()
                || s.also_if.is_some()
            {
                return Err(invalid(
                    field,
                    "none = true takes skip_wrappers and skip_dirs only",
                ));
            }
            return Ok((None, skip));
        }
    }
    let select = match (&s.ext, &s.suffix) {
        (Some(ext), None) => {
            if ext.is_empty() || ext.len() > limits::EXTS {
                return Err(invalid(format!("{field}.ext"), "1 to 4 extensions"));
            }
            let unique: BTreeSet<&String> = ext.iter().collect();
            if unique.len() != ext.len()
                || !ext.iter().all(|e| {
                    e.len() <= limits::EXT_CHARS
                        && e.bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                })
            {
                return Err(invalid(
                    format!("{field}.ext"),
                    "distinct lower-case extensions without the dot",
                ));
            }
            Select::Ext(ext.clone())
        }
        (None, Some(suffix)) => {
            if suffix.len() < 2
                || suffix.len() > limits::SUFFIX_CHARS
                || suffix.bytes().any(|c| c.is_ascii_uppercase())
                || suffix
                    .chars()
                    .any(|c| bad_char(c) || c == '*' || c == '{' || c == '}')
            {
                return Err(invalid(
                    format!("{field}.suffix"),
                    "a lower-case name ending",
                ));
            }
            Select::Suffix(suffix.clone())
        }
        _ => return Err(invalid(field, "exactly one of ext and suffix")),
    };
    let at =
        s.at.as_deref()
            .map(|a| parse_at(a, &format!("{field}.at")))
            .transpose()?;
    if let Some(f) = s.first
        && (f == 0 || f > limits::FIRST)
    {
        return Err(invalid(format!("{field}.first"), "1 to 8"));
    }
    let also_if = s
        .also_if
        .as_ref()
        .map(|a| {
            parse_name(
                &a.dir,
                &Vars {
                    program: true,
                    project: false,
                    captures: &[],
                },
                &format!("{field}.also_if.dir"),
            )
        })
        .transpose()?;
    Ok((
        Some(Programs {
            select,
            at,
            first: s.first,
            also_if,
        }),
        skip,
    ))
}

fn model(spec: Spec) -> Result<Model> {
    if spec.engine.codes.is_empty() || spec.engine.codes.len() > limits::CODES {
        return Err(invalid("engine.codes", "1 to 4 codes"));
    }
    let mut codes = Vec::new();
    for (code, name) in &spec.engine.codes {
        let c = EngineCode::new(code).ok_or_else(|| {
            invalid(
                "engine.codes",
                format!("{code:?} is not ^[a-z][a-z0-9-]{{1,31}}$"),
            )
        })?;
        check_display_name(name, &format!("engine.codes.{code}"))?;
        codes.push((c, name.clone()));
    }
    let list_at_limit = match &spec.walk {
        None => Vec::new(),
        Some(w) => {
            if w.list_at_limit.is_empty() || w.list_at_limit.len() > 4 {
                return Err(invalid("walk.list_at_limit", "1 to 4 paths"));
            }
            w.list_at_limit
                .iter()
                .map(|p| {
                    let parts: Vec<String> = p.split('/').map(str::to_owned).collect();
                    if parts.len() > 4 || !parts.iter().all(|c| check_component(c)) {
                        return Err(invalid("walk.list_at_limit", "literal relative paths"));
                    }
                    Ok(parts)
                })
                .collect::<Result<_>>()?
        }
    };
    if spec.rule.is_empty() || spec.rule.len() > limits::RULES {
        return Err(invalid("rule", "1 to 16 rules"));
    }
    let rules = spec
        .rule
        .iter()
        .enumerate()
        .map(|(i, r)| parse_rule(r, &codes, &format!("rule[{i}]")))
        .collect::<Result<Vec<_>>>()?;
    if spec.wrappers.len() > limits::WRAPPERS
        || !spec.wrappers.iter().all(|w| {
            (3..=limits::WRAPPER_CHARS).contains(&w.len())
                && w.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
                })
        })
    {
        return Err(invalid(
            "wrappers",
            "at most 16 lower-case names of 3 to 32 characters",
        ));
    }
    if spec.anti_cheat.len() > limits::ANTI_CHEAT_MARKERS {
        return Err(invalid("anti_cheat", "at most 8 markers"));
    }
    let markers = spec
        .anti_cheat
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if !check_component(&m.listed) || m.listed.contains(['{', '}', '*']) {
                return Err(invalid(
                    format!("anti_cheat[{i}].listed"),
                    "one literal name",
                ));
            }
            if !is_valid_engine_code(&m.code) {
                return Err(invalid(
                    format!("anti_cheat[{i}].code"),
                    "^[a-z][a-z0-9-]{1,31}$",
                ));
            }
            Ok(Marker {
                name: m.listed.clone(),
                kind: m.kind.into(),
                code: m.code.clone(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(Model {
        codes,
        list_at_limit,
        rules,
        wrappers: spec.wrappers.clone(),
        markers,
    })
}

/// Why `name` (lower case) may not anchor a rule, if it may not.
pub(crate) fn anchor_refusal(name: &str, kind: Kind, m: &Model) -> Option<&'static str> {
    if name.chars().count() < limits::ANCHOR_CHARS {
        return Some("too-short");
    }
    if kind == Kind::File {
        let ext = name.rsplit_once('.').map(|(_, e)| e);
        if ext.is_none_or(|e| PASS_B_PROGRAM_EXTS.contains(&e)) {
            return Some("program");
        }
    }
    if m.is_wrapper(name) {
        return Some("wrapper");
    }
    if UBIQUITOUS_RUNTIME_PREFIXES
        .iter()
        .any(|p| name.starts_with(p))
    {
        return Some("ubiquitous-runtime");
    }
    let generic = match kind {
        Kind::Dir => GENERIC_ANCHOR_DIRS,
        Kind::File => GENERIC_ANCHOR_FILES,
    };
    if generic.contains(&name) {
        return Some("generic-name");
    }
    None
}

/// Whether a display name reads as a built-in engine's (`families`) or holds one of
/// [`RESERVED_NAME_WORDS`]: from some word of the name on, the rest of it — its words run
/// together, folded — begins with a reserved word (`Unreal Engine 5`, `Un real`, `G.o.d.o.t`,
/// `My Un ity`), or, for a reserved phrase of several words (`first-party`), the whole name,
/// folded, holds it. A reserved word inside a word does not count: `Community` holds `unity`.
pub fn name_is_reserved(name: &str, families: &[String]) -> bool {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let tails: Vec<String> = (0..words.len())
        .map(|i| fold_confusables(&words[i..].concat()))
        .collect();
    let whole = fold_confusables(name);
    families
        .iter()
        .map(String::as_str)
        .chain(RESERVED_NAME_WORDS.iter().copied())
        .any(|w| {
            let fw = fold_confusables(w);
            if w.contains('-') {
                whole.contains(&fw)
            } else {
                tails.iter().any(|t| t.starts_with(&fw))
            }
        })
}

/// The built-in engines' family names (`unity`, `unreal`, `godot`): reserved as code prefixes and
/// as name words.
pub fn builtin_families() -> Vec<String> {
    Engine::ALL
        .iter()
        .filter(|e| **e != Engine::Unknown)
        .map(|e| e.code().split('-').next().unwrap_or("").to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What two publishers' engine codes may not share ([`super::EngineSet::build`]'s ownership):
/// the code with `-` dropped and the look-alikes of its own alphabet read alike — `0` as `o`,
/// `1` and `i` as `l`, `rn` as `m`, `vv` as `w`, `cl` as `d`. Not [`fold_confusables`]'s
/// reading of every digit as a letter (that guards the reserved codes): `id-tech-6` and
/// `id-tech-8` stay two codes, while `s0urce-2` is `source2`.
pub fn code_identity(code: &str) -> String {
    let mapped: String = code
        .chars()
        .filter(|c| *c != '-')
        .map(|c| match c {
            '0' => 'o',
            '1' | 'i' => 'l',
            c => c,
        })
        .collect();
    mapped
        .replace("rn", "m")
        .replace("vv", "w")
        .replace("cl", "d")
}

/// Whether `code` is spelled as an engine code may be: `^[a-z][a-z0-9-]{1,31}$`.
pub fn is_valid_engine_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Whether no engine descriptor may declare `code` (any spelling, valid or not): a built-in
/// code, `unknown`, or one that reads as one of them once folded ([`fold_confusables`]:
/// `unity-mon0`, `un1ty-x`, `g0dot`); one that begins with a built-in family name
/// ([`builtin_families`]) or a [`RESERVED_CODE_PREFIXES`] entry; one holding a
/// [`RESERVED_CODE_INFIXES`] entry.
pub fn is_reserved_code(code: &str) -> bool {
    let folded = fold_confusables(code);
    if Engine::ALL
        .iter()
        .any(|e| folded == fold_confusables(e.code()))
    {
        return true;
    }
    if builtin_families()
        .iter()
        .map(String::as_str)
        .chain(RESERVED_CODE_PREFIXES.iter().copied())
        .any(|f| folded.starts_with(&fold_confusables(f)))
    {
        return true;
    }
    RESERVED_CODE_INFIXES
        .iter()
        .any(|w| folded.contains(&fold_confusables(w)))
}

// ---- The descriptor. ----

/// One parsed, validated `astra.engine/1` descriptor. Equal when their digests are.
#[derive(Debug, Clone)]
pub struct Descriptor {
    canonical: String,
    digest: String,
    /// Its `mods` table, read beside [`Model`] and never into it.
    mods: ModsState,
    pub(crate) model: Model,
}

/// What a descriptor's `mods` table came to (§7.9).
#[derive(Debug, Clone)]
enum ModsState {
    Absent,
    Valid(Mods),
    /// Every finding: the descriptor is as without `mods`.
    Invalid(Vec<ModsFinding>),
}

impl PartialEq for Descriptor {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest
    }
}

impl Eq for Descriptor {}

impl Descriptor {
    /// From JSON (the catalogue's spelling). Refuses what the module doc lists.
    pub fn from_json(bytes: &[u8]) -> Result<Descriptor> {
        if bytes.len() > limits::INPUT_BYTES {
            return Err(LauncherError::new(codes::DESCRIPTOR_TOO_LARGE)
                .with("bytes", bytes.len().to_string()));
        }
        let mut de = serde_json::Deserializer::from_slice(bytes);
        let StrictValue(v) =
            StrictValue::deserialize(&mut de).map_err(|e| invalid("", e.to_string()))?;
        de.end().map_err(|e| invalid("", e.to_string()))?;
        Descriptor::from_value(v)
    }

    /// From an already-parsed JSON value. Refuses floats, nulls and integers past 2^53 − 1, as
    /// [`Descriptor::from_json`] does; duplicate keys are the parser's to refuse.
    pub fn from_json_value(v: &Value) -> Result<Descriptor> {
        check_value(v, "")?;
        Descriptor::from_value(v.clone())
    }

    /// From TOML (a file, or `[integration.engine]` as text).
    pub fn from_toml(text: &str) -> Result<Descriptor> {
        if text.len() > limits::INPUT_BYTES {
            return Err(LauncherError::new(codes::DESCRIPTOR_TOO_LARGE)
                .with("bytes", text.len().to_string()));
        }
        let table: toml::Table = toml::from_str(text).map_err(|e| invalid("", e.message()))?;
        Descriptor::from_toml_value(&toml::Value::Table(table))
    }

    /// From a TOML value already parsed: `[integration.engine]` out of a foundation's manifest.
    /// The same object as its JSON spelling has the same digest.
    pub fn from_toml_value(v: &toml::Value) -> Result<Descriptor> {
        Descriptor::from_value(toml_to_json(v, "")?)
    }

    fn from_value(v: Value) -> Result<Descriptor> {
        let Value::Object(obj) = &v else {
            return Err(invalid("", "a descriptor is an object"));
        };
        match obj.get("schema") {
            Some(Value::String(s)) if s == SCHEMA => {}
            Some(Value::String(s)) => {
                return Err(LauncherError::new(codes::DESCRIPTOR_SCHEMA_UNKNOWN)
                    .with("schema", s.clone())
                    .with("supported", SCHEMA));
            }
            _ => return Err(invalid("schema", "missing")),
        }
        // §7.1: a table, or the whole descriptor is skipped; read after the model, beside it.
        let mods = match obj.get("mods") {
            None => None,
            Some(Value::Object(m)) => Some(m.clone()),
            Some(_) => return Err(invalid("mods", "a table (spec §7.1)")),
        };
        let mut canonical = String::new();
        jcs(&v, &mut canonical);
        if canonical.len() > limits::DESCRIPTOR_BYTES {
            return Err(LauncherError::new(codes::DESCRIPTOR_TOO_LARGE)
                .with("bytes", canonical.len().to_string()));
        }
        let spec: Spec = serde_json::from_value(v).map_err(|e| invalid("", e.to_string()))?;
        let model = model(spec)?;
        let mods = match mods {
            None => ModsState::Absent,
            Some(m) => match super::mods::validate(&m, &model) {
                Ok(m) => ModsState::Valid(m),
                Err(findings) => ModsState::Invalid(findings),
            },
        };
        let digest = hex::encode(Sha256::digest(canonical.as_bytes()));
        Ok(Descriptor {
            canonical,
            digest,
            mods,
            model,
        })
    }

    /// Whether it carries a `mods` table, valid or not.
    pub fn carries_mods(&self) -> bool {
        !matches!(self.mods, ModsState::Absent)
    }

    /// Its `mods` (§7), when it carries one and nothing in it is wrong. Read only by a
    /// mods-aware install or Play ([`super::mods::evaluate`]); never by detection.
    pub fn mods(&self) -> Option<&Mods> {
        match &self.mods {
            ModsState::Valid(m) => Some(m),
            _ => None,
        }
    }

    /// Everything wrong inside its `mods` (§7.9), each `mods-invalid{field, reason}`; empty when
    /// it carries none or a valid one. Ingest and lint refuse the descriptor for any of them; a
    /// client treats it as carrying no `mods`, and says so once.
    pub fn mods_refusals(&self) -> Vec<LauncherError> {
        self.mods_findings()
            .iter()
            .map(ModsFinding::error)
            .collect()
    }

    /// [`Descriptor::mods_refusals`], as `(field, reason)`.
    pub fn mods_findings(&self) -> &[ModsFinding] {
        match &self.mods {
            ModsState::Invalid(f) => f,
            _ => &[],
        }
    }

    /// `sha256(JCS(descriptor))`, lower-case hex.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The canonical (JCS) JSON the digest is of.
    pub fn canonical_json(&self) -> &str {
        &self.canonical
    }

    /// The codes this descriptor declares, each with its display name, sorted by code.
    pub fn codes(&self) -> impl Iterator<Item = (&EngineCode, &str)> {
        self.model.codes.iter().map(|(c, n)| (c, n.as_str()))
    }

    /// Whether Pass B may ask this descriptor, given whether it is Astra's own
    /// (`first_party`): codes and names not reserved (`engine-code-reserved`,
    /// `engine-name-reserved`); the first-party-only parts of the grammar only when it is
    /// (`descriptor-first-party-only`); `programs.first` given and the programs ones Pass B
    /// considers (`descriptor-invalid`); `evidence` given and every rule anchored when it is not
    /// (`descriptor-invalid`, `descriptor-too-broad`). The parts only the built-ins are evaluated
    /// with (`walk.list_at_limit`, `scope`, `programs = "none"`, the `name` check,
    /// `skip_wrappers = "folders"`) are `descriptor-unsupported` here.
    pub fn check_pass_b(&self, first_party: bool) -> Result<()> {
        let m = &self.model;
        let families = builtin_families();
        for (code, name) in &m.codes {
            if code.is_reserved() {
                return Err(
                    LauncherError::new(codes::ENGINE_CODE_RESERVED).with("code", code.as_str())
                );
            }
            if name_is_reserved(name, &families) {
                return Err(LauncherError::new(codes::ENGINE_NAME_RESERVED)
                    .with("code", code.as_str())
                    .with("name", name.clone()));
            }
        }
        let first_party_only = |field: String| -> Result<()> {
            if first_party {
                Ok(())
            } else {
                Err(LauncherError::new(codes::DESCRIPTOR_FIRST_PARTY_ONLY).with("field", field))
            }
        };
        let unsupported =
            |field: String| LauncherError::new(codes::DESCRIPTOR_UNSUPPORTED).with("field", field);
        // Built-in only: it would reshape the one walk Pass A has already made.
        if !m.list_at_limit.is_empty() {
            first_party_only("walk.list_at_limit".into())?;
            return Err(unsupported("walk.list_at_limit".into()));
        }
        for (i, r) in m.rules.iter().enumerate() {
            let f = |k: &str| format!("rule[{i}].{k}");
            if r.scope != Scope::Beside {
                first_party_only(f("scope"))?;
                return Err(unsupported(f("scope")));
            }
            let Some(p) = &r.programs else {
                first_party_only(f("programs"))?;
                return Err(unsupported(f("programs")));
            };
            if r.weak_order.is_some() {
                first_party_only(f("weak_order"))?;
            }
            match r.skip.wrappers {
                WrapperSkip::All => {}
                WrapperSkip::None => first_party_only(f("programs.skip_wrappers"))?,
                WrapperSkip::Folders => {
                    first_party_only(f("programs.skip_wrappers"))?;
                    return Err(unsupported(f("programs.skip_wrappers")));
                }
            }
            if r.checks().any(|c| matches!(c, Check::Name(_))) {
                first_party_only(f("require"))?;
                return Err(unsupported(f("require")));
            }
            if p.first.is_none() {
                return Err(invalid(f("programs.first"), "required in Pass B"));
            }
            let kinds_ok = match &p.select {
                Select::Ext(list) => list
                    .iter()
                    .all(|e| e.is_empty() || PASS_B_PROGRAM_EXTS.contains(&e.as_str())),
                Select::Suffix(s) => PASS_B_PROGRAM_EXTS
                    .iter()
                    .any(|e| s.ends_with(&format!(".{e}"))),
            };
            if !kinds_ok {
                return Err(invalid(
                    f("programs"),
                    "Pass B considers .exe, .x86_64, .x86 and extension-less programs only",
                ));
            }
            if first_party {
                continue;
            }
            if r.evidence.is_none() {
                return Err(invalid(f("evidence"), "required"));
            }
            if r.anchors.is_empty() {
                return Err(LauncherError::new(codes::DESCRIPTOR_TOO_BROAD)
                    .with("field", f("require"))
                    .with("reason", "no-anchor"));
            }
            let refusals: Vec<&str> = r
                .anchors
                .iter()
                .filter_map(|(n, k)| anchor_refusal(n, *k, m))
                .collect();
            if refusals.len() == r.anchors.len() {
                return Err(LauncherError::new(codes::DESCRIPTOR_TOO_BROAD)
                    .with("field", f("require"))
                    .with("reason", refusals[0]));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `mods` table that would change answers were it read: it spells the descriptor's own
    /// grammar (a code, a rule claiming every program, the walk's depth bound). It is refused
    /// inside (`mods-invalid`), so a client drops it.
    const MODS: &str = "mods = { root = \"{game}/Mods\", enabled = { format = \"dir-per-mod\" }, engine = { codes = { taken = \"Taken\" } }, rule = [ { tier = \"strong\", programs = { ext = [\"exe\", \"\"], first = 8, skip_wrappers = false }, code = \"taken\", confidence = \"high\", binary = \"by-ext\" } ], walk = { list_at_limit = [\"x\"] } }";

    /// A valid `mods` on every built-in and on the community descriptor below: one loader of
    /// its own, chained.
    const VALID_MODS: &str = "mods = { loader = [ { id = \"probe-loader\", name = \"Probe Loader\", source = [\"game\", \"manager\", \"picked\"], require = [ { stat = \"{source}/probe-loader/probe-core.dll\", kind = \"file\" } ], root = \"{source}/probe-loader/plugins\", enabled = [ { format = \"dir-per-mod\" } ], coexist = [ { strategy = \"chain\", entry = \"{source}/probe-loader/probe-core.dll\" } ] } ] }";

    /// `text` with `mods` after its `schema` line (the root table).
    fn with_mods(text: &str, mods: &str) -> String {
        let mut out = String::new();
        let mut done = false;
        for line in text.split_inclusive('\n') {
            out.push_str(line);
            if !done && line.starts_with("schema") {
                out.push_str(mods);
                out.push('\n');
                done = true;
            }
        }
        assert!(done, "{text}");
        out
    }

    /// `mods` never reaches the model the evaluator reads — the built-ins' (Pass A) and a
    /// community descriptor's (Pass B), from TOML and from JSON, valid or refused inside — nor
    /// Pass B's admission: so it cannot change which engine a game is, nor which descriptors are
    /// asked. It is in the digest, as every member is.
    #[test]
    fn a_mods_table_never_reaches_the_model() {
        let community = "schema = \"astra.engine/1\"\nengine = { codes = { probe = \"Probe\" } }\n[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], first = 8 }\nrequire = [ { listed = \"probe.dat\", kind = \"file\" } ]\ncode = \"probe\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{program}\"\n";
        let model = |d: &Descriptor| format!("{:?}", d.model);
        for (text, mods) in super::super::BUILTIN
            .into_iter()
            .chain([community])
            .flat_map(|t| [(t, MODS), (t, VALID_MODS)])
        {
            let plain = Descriptor::from_toml(text).unwrap();
            let twin = Descriptor::from_toml(&with_mods(text, mods)).unwrap();
            assert!(!plain.carries_mods() && twin.carries_mods());
            assert_eq!(twin.mods().is_some(), mods == VALID_MODS, "{text}");
            assert_ne!(plain.digest(), twin.digest());
            assert!(twin.canonical_json().contains("\"mods\":{"));
            assert_eq!(model(&twin), model(&plain), "{text}");
            let json = Descriptor::from_json(twin.canonical_json().as_bytes()).unwrap();
            assert_eq!(json.digest(), twin.digest());
            assert!(json.carries_mods());
            assert_eq!(model(&json), model(&plain));
            for first_party in [false, true] {
                assert_eq!(
                    twin.check_pass_b(first_party),
                    plain.check_pass_b(first_party),
                    "{text}"
                );
            }
        }
    }
}
