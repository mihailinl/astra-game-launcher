// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `astra-gi.toml`, schema 1. The authority is astra-bepinex `docs/GAME-INTEGRATION-MANIFEST.md`
//! for the sections 0.3.1 knew, and AstraPlugins `spec/games/manifest.md` for `[integration.*]`.
//!
//! An unknown key in a section that ACTS (`[target]`, `[[files]]`, `[[game_files]]`, `[launch]`,
//! `[[config_writes]]`) is refused: a newer feature read as nothing would make an install that
//! looks fine and is broken. Unknown keys in `[integration]` (metadata) are ignored — which is why
//! every newer table lives there, as `[integration.*]`: a launcher from before it (0.3.1, what
//! Astra 0.2.7 ships) installs the same release. This one reads them apart, once, in
//! [`Manifest::parse`]:
//!
//! - `[integration.mods]` ([`Manifest::mods`]): never refuses the manifest; a table it cannot use
//!   only means "not with mods".
//! - `[integration.engine]` ([`Manifest::engine_descriptor`]): the `astra.engine/1` descriptor of
//!   the foundation that owns an engine code. Its refusal is kept, never the manifest's: a
//!   descriptor this launcher cannot read only detects nothing. When present, `[target] engine`
//!   must be one of its codes (`engine-undeclared`).
//! - `[integration.loader]` ([`Manifest::loader`]): it acts, so an unknown key is `unknown-key`;
//!   `proxies` and `keep_if_present` are disjoint and keep names are config files
//!   (`loader-policy-conflict`).
//! - `[integration.capabilities]` ([`Manifest::declares_companion`]): a capability this launcher
//!   does not know is `needs-newer-astra`, never dropped.
//! - `[integration.foundation]` ([`Manifest::foundation`]): a claim, read and shown, never acted
//!   on; unknown keys in it are [`Manifest::lint`]'s to name.
//! - `[integration.link]` ([`Manifest::link`]): read as it is, never refused here; the launcher
//!   acts on none of it, and [`Manifest::lint`] holds it to Astra Link v1's limits.
//! - `[integration] sdk` ([`Manifest::sdk`]): an `https://` URL, metadata.
//!
//! None of these changes what [`IntegrationInfo`], [`crate::InstallPlan`] or [`crate::Ledger`]
//! serialise to.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::detect::Engine;
use crate::digest::normalize_hex;
use crate::engines::{Descriptor, EngineCode};
use crate::error::{LauncherError, Result, codes};
use crate::mods::ModsDecl;
use crate::paths::{JailPath, Root, check_component, parse_jailed, relative_components};

/// The newest manifest schema this launcher reads.
pub const SCHEMA: u32 = 1;

/// The manifest's file name at the root of the zip.
pub const MANIFEST_FILE: &str = "astra-gi.toml";

/// `[integration]`: shown to the user, never acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub repo: Option<String>,
}

/// `[target]`: which game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    #[serde(default)]
    pub steam_appid: Option<u32>,
    /// Relative to the game folder, or `"*"` for a runner (the user picks the exe).
    pub exe: String,
    #[serde(default)]
    pub engine: Option<String>,
    pub platforms: Vec<String>,
    pub anti_cheat: String,
}

/// `[[files]]`: into the profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRule {
    pub from: String,
    pub to: String,
}

/// `[[game_files]]`: beside the game's exe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFileRule {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// `[launch]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub proton_dll_overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// `[[config_writes]]`: INI keys set at every Play, inside the profile only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigWrite {
    pub file: String,
    pub section: String,
    pub set: BTreeMap<String, String>,
}

/// `[integration.loader]`: what this install's own loader does beside the game (spec
/// `manifest.md` §2.2). Both lists hold bare file names, in the manifest's order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LoaderDecl {
    /// The DLLs this integration hijack-loads beside the exe. One of these names the launcher
    /// did not place is a foreign loader.
    pub proxies: Vec<String>,
    /// Config files kept at uninstall when present and not Astra's own placement: a sharing
    /// marker, never a loader. Their extension is one of [`KEEP_EXTENSIONS`].
    pub keep_if_present: Vec<String>,
}

/// The extensions a `keep_if_present` name may have: configuration, never code. A name ending in
/// `.dll`, `.asi` or `.exe` is `loader-policy-conflict{reason=loader}`, any other is
/// `manifest-invalid`.
pub const KEEP_EXTENSIONS: &[&str] = &["ini", "cfg", "json", "toml", "txt", "xml", "yml", "yaml"];

/// What a loader is spelled with: a keep name ending in one of these keeps a loader.
const LOADER_EXTENSIONS: &[&str] = &["dll", "asi", "exe"];

/// `[integration.foundation]`: "built against foundation `id` at `version`, for `engine`" (spec
/// `manifest.md` §2.4). A claim only: what is true is computed from digests (`gi-probe check`'s
/// `built_on`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundationClaim {
    pub id: String,
    pub version: String,
    pub engine: EngineCode,
}

/// The one `companion` capability of Astra Link v1: the integration's `${bridge_token}` may be
/// handed to the desktop companion (`[integration.capabilities] companion = ["presentation"]`).
pub const COMPANION_PRESENTATION: &str = "presentation";

/// The variable Astra passes the avatar bridge's token in. Handing it on needs
/// [`COMPANION_PRESENTATION`] ([`Manifest::check_capabilities_used`]).
pub const BRIDGE_TOKEN_VARIABLE: &str = "bridge_token";

/// The capability kinds `[integration.capabilities]` may name, and each one's ids.
const CAPABILITIES: &[(&str, &[&str])] = &[("companion", &[COMPANION_PRESENTATION])];

/// Astra Link v1's limits on `[integration.link]` (A§2, `link-v1.md` §5): tools, triggers and
/// events at most; a name's longest; a description's or label's longest, in characters.
pub mod link_limits {
    pub const TOOLS: usize = 16;
    pub const TRIGGERS: usize = 16;
    pub const EVENTS: usize = 32;
    pub const NAME_CHARS: usize = 32;
    pub const DESCRIPTION_CHARS: usize = 200;
    /// A `params`/`data` schema file's size, in bytes (`gi-probe check` reads the zip).
    pub const SCHEMA_BYTES: u64 = 4 * 1024;
    /// The `skill` file's size, in bytes.
    pub const SKILL_BYTES: u64 = 16 * 1024;
}

/// The `[integration.*]` tables and `[integration] sdk`, read once by [`Manifest::parse`].
#[derive(Debug, Clone, Default)]
struct IntegrationTables {
    /// `[integration.engine]` as written, and what [`Descriptor::from_toml_value`] made of it.
    engine: Option<(toml::Value, Result<Descriptor>)>,
    loader: Option<LoaderDecl>,
    /// `[integration.capabilities]`: each kind's ids (empty when the table is absent).
    capabilities: BTreeMap<String, BTreeSet<String>>,
    foundation: Option<FoundationClaim>,
    /// Keys of `[integration.foundation]` this launcher does not read: [`Manifest::lint`]'s.
    foundation_unknown: Vec<String>,
    link: Option<toml::Value>,
    sdk: Option<String>,
    /// `[integration] sdk` is there but no usable URL: metadata, so never a refusal at parse —
    /// [`Manifest::lint`]'s finding.
    sdk_unusable: bool,
}

// `toml::Value` is not `Eq` (it may hold a float); the tables are equal when their values are.
impl PartialEq for IntegrationTables {
    fn eq(&self, o: &Self) -> bool {
        self.engine == o.engine
            && self.loader == o.loader
            && self.capabilities == o.capabilities
            && self.foundation == o.foundation
            && self.foundation_unknown == o.foundation_unknown
            && self.link == o.link
            && self.sdk == o.sdk
            && self.sdk_unusable == o.sdk_unusable
    }
}

impl Eq for IntegrationTables {}

/// A parsed and checked manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub integration: IntegrationInfo,
    pub target: Target,
    #[serde(default)]
    pub files: Vec<FileRule>,
    #[serde(default)]
    pub game_files: Vec<GameFileRule>,
    #[serde(default)]
    pub launch: Launch,
    #[serde(default)]
    pub config_writes: Vec<ConfigWrite>,
    /// `[integration.mods]` as [`Manifest::mods`] reads it: absent, usable, or what a Play with
    /// mods refuses with. Never part of the manifest's own checks.
    #[serde(skip)]
    mods: Option<std::result::Result<ModsDecl, LauncherError>>,
    /// `[integration.*]` as [`Manifest::parse`] read it (the module doc).
    #[serde(skip)]
    tables: IntegrationTables,
}

const TOP_KEYS: &[&str] = &[
    "schema",
    "integration",
    "target",
    "files",
    "game_files",
    "launch",
    "config_writes",
];
const TARGET_KEYS: &[&str] = &["steam_appid", "exe", "engine", "platforms", "anti_cheat"];
const FILES_KEYS: &[&str] = &["from", "to"];
const GAME_FILES_KEYS: &[&str] = &["from", "to", "sha256"];
const LAUNCH_KEYS: &[&str] = &["args", "proton_dll_overrides", "env"];
const CONFIG_WRITES_KEYS: &[&str] = &["file", "section", "set"];

/// The Wine DLL-override values a manifest may ask for.
const OVERRIDE_VALUES: &[&str] = &[
    "native",
    "builtin",
    "native,builtin",
    "builtin,native",
    "n",
    "b",
    "n,b",
    "b,n",
    "",
];

/// A DLL name an override may name: a bare file name, never a path, and nothing that could split
/// a `WINEDLLOVERRIDES` value or a `user.reg` line (`;` `=` `,` `"` a space, a slash, `*`).
fn is_dll_name(dll: &str) -> bool {
    dll.len() <= 64
        && dll
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && dll
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// One override value in `WINEDLLOVERRIDES`' short form: `native,builtin` -> `n,b`.
fn short_override(value: &str) -> String {
    value
        .split(',')
        .map(|v| match v {
            "native" => "n",
            "builtin" => "b",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// `(dll, value)` pairs as one `WINEDLLOVERRIDES` value, in the order given: `a=n,b;c=n`.
pub fn wine_dll_overrides(overrides: &[(String, String)]) -> Option<String> {
    (!overrides.is_empty()).then(|| {
        overrides
            .iter()
            .map(|(d, v)| format!("{d}={}", short_override(v)))
            .collect::<Vec<_>>()
            .join(";")
    })
}

fn invalid(key: &str, detail: impl Into<String>) -> LauncherError {
    LauncherError::new(codes::MANIFEST_INVALID)
        .with("key", key)
        .with("detail", detail)
}

fn unknown_key(key: String) -> LauncherError {
    LauncherError::new(codes::UNKNOWN_KEY).with("key", key)
}

fn check_keys(table: &toml::Table, allowed: &[&str], prefix: &str) -> Result<()> {
    for k in table.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(unknown_key(format!("{prefix}{k}")));
        }
    }
    Ok(())
}

fn check_table(value: Option<&toml::Value>, allowed: &[&str], name: &str) -> Result<()> {
    match value {
        None => Ok(()),
        Some(toml::Value::Table(t)) => check_keys(t, allowed, &format!("{name}.")),
        Some(_) => Err(invalid(name, "must be a table")),
    }
}

fn check_array(value: Option<&toml::Value>, allowed: &[&str], name: &str) -> Result<()> {
    match value {
        None => Ok(()),
        Some(toml::Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                let toml::Value::Table(t) = item else {
                    return Err(invalid(&format!("{name}[{i}]"), "must be a table"));
                };
                check_keys(t, allowed, &format!("{name}[{i}]."))?;
            }
            Ok(())
        }
        Some(_) => Err(invalid(name, "must be an array of tables")),
    }
}

/// A bare file name a loader policy may hold: what [`is_dll_name`] allows.
fn is_bare_file_name(name: &str) -> bool {
    is_dll_name(name) && check_component(name)
}

/// The lower-case extension of a bare file name, `""` for none.
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// A list of strings at `key`, or `manifest-invalid`.
fn string_list(v: &toml::Value, key: &str) -> Result<Vec<String>> {
    let toml::Value::Array(items) = v else {
        return Err(invalid(key, "must be an array of strings"));
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(&format!("{key}[{i}]"), "must be a string"))
        })
        .collect()
}

/// `[integration.loader]`: it acts, so every key must be one this launcher reads.
fn read_loader(v: &toml::Value) -> Result<LoaderDecl> {
    let toml::Value::Table(t) = v else {
        return Err(invalid("integration.loader", "must be a table"));
    };
    check_keys(t, &["proxies", "keep_if_present"], "integration.loader.")?;
    let list = |k: &str| -> Result<Vec<String>> {
        t.get(k).map_or(Ok(Vec::new()), |v| {
            string_list(v, &format!("integration.loader.{k}"))
        })
    };
    let decl = LoaderDecl {
        proxies: list("proxies")?,
        keep_if_present: list("keep_if_present")?,
    };
    let mut proxies = BTreeSet::new();
    for (i, name) in decl.proxies.iter().enumerate() {
        let key = format!("integration.loader.proxies[{i}]");
        if !is_bare_file_name(name) {
            return Err(invalid(&key, "not a DLL name"));
        }
        if !proxies.insert(name.to_ascii_lowercase()) {
            return Err(invalid(&key, "named twice (Windows ignores letter case)"));
        }
        // A name every install keeps (`doorstop_config.ini`, `ReShade.ini`) is never one this
        // install loads: planning keeps before it asks about loaders.
        if crate::plan::keep_if_present(name) {
            return Err(LauncherError::new(codes::LOADER_POLICY_CONFLICT)
                .with("name", name.clone())
                .with("reason", "proxy"));
        }
    }
    let mut keeps = BTreeSet::new();
    for (i, name) in decl.keep_if_present.iter().enumerate() {
        let key = format!("integration.loader.keep_if_present[{i}]");
        if !is_bare_file_name(name) {
            return Err(invalid(&key, "not a bare file name"));
        }
        let lower = name.to_ascii_lowercase();
        // The conflict first: a name the integration both loads and keeps.
        if proxies.contains(&lower) {
            return Err(LauncherError::new(codes::LOADER_POLICY_CONFLICT)
                .with("name", name.clone())
                .with("reason", "proxy"));
        }
        let ext = extension(name);
        if LOADER_EXTENSIONS.contains(&ext.as_str()) {
            return Err(LauncherError::new(codes::LOADER_POLICY_CONFLICT)
                .with("name", name.clone())
                .with("reason", "loader"));
        }
        if !KEEP_EXTENSIONS.contains(&ext.as_str()) {
            return Err(invalid(
                &key,
                format!("a config file ({})", KEEP_EXTENSIONS.join(", ")),
            ));
        }
        if !keeps.insert(lower) {
            return Err(invalid(&key, "named twice (Windows ignores letter case)"));
        }
    }
    Ok(decl)
}

/// `[integration.capabilities]`: each kind's ids, or `needs-newer-astra{capability}` for a kind
/// or an id this launcher does not know.
fn read_capabilities(v: &toml::Value) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let toml::Value::Table(t) = v else {
        return Err(invalid("integration.capabilities", "must be a table"));
    };
    let mut out = BTreeMap::new();
    for (kind, ids) in t {
        let Some((_, known)) = CAPABILITIES.iter().find(|(k, _)| k == kind) else {
            return Err(
                LauncherError::new(codes::NEEDS_NEWER_ASTRA).with("capability", kind.clone())
            );
        };
        let mut set = BTreeSet::new();
        for id in string_list(ids, &format!("integration.capabilities.{kind}"))? {
            if !known.contains(&id.as_str()) {
                return Err(LauncherError::new(codes::NEEDS_NEWER_ASTRA)
                    .with("capability", format!("{kind}.{id}")));
            }
            set.insert(id);
        }
        out.insert(kind.clone(), set);
    }
    Ok(out)
}

/// `[integration.foundation]`: a claim; its keys must be readable, and keys it does not know are
/// returned for [`Manifest::lint`].
fn read_foundation(v: &toml::Value) -> Result<(FoundationClaim, Vec<String>)> {
    let toml::Value::Table(t) = v else {
        return Err(invalid("integration.foundation", "must be a table"));
    };
    let text = |k: &str| -> Result<String> {
        match t.get(k) {
            Some(toml::Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
            Some(_) => Err(invalid(&format!("integration.foundation.{k}"), "a string")),
            None => Err(invalid(&format!("integration.foundation.{k}"), "missing")),
        }
    };
    let id = text("id")?;
    if !is_integration_id(&id) {
        return Err(invalid(
            "integration.foundation.id",
            "letters, digits, . - _",
        ));
    }
    let version = text("version")?;
    let engine = text("engine")?;
    let engine = EngineCode::new(&engine)
        .filter(|c| !c.is_unknown())
        .ok_or_else(|| {
            invalid(
                "integration.foundation.engine",
                format!("not an engine code {engine:?}"),
            )
        })?;
    let unknown = t
        .keys()
        .filter(|k| !["id", "version", "engine"].contains(&k.as_str()))
        .map(|k| format!("integration.foundation.{k}"))
        .collect();
    Ok((
        FoundationClaim {
            id,
            version,
            engine,
        },
        unknown,
    ))
}

/// `[integration] sdk`: an `https://` URL with no space or control character, at most 2,048
/// bytes. Shown (in developer mode), so nothing else. Metadata (manifest.md §2.5): an unusable
/// one is dropped at parse and is [`Manifest::lint`]'s finding, never the manifest's refusal.
fn read_sdk(v: &toml::Value) -> Result<String> {
    match v.as_str() {
        Some(url)
            if url.len() <= 2048
                && url.len() > "https://".len()
                && url
                    .get(..8)
                    .is_some_and(|h| h.eq_ignore_ascii_case("https://"))
                && !url.chars().any(|c| c.is_whitespace() || c.is_control()) =>
        {
            Ok(url.to_owned())
        }
        _ => Err(invalid("integration.sdk", "an https:// URL")),
    }
}

impl IntegrationTables {
    /// Reads `[integration]`'s newer tables. A refusal is the manifest's, except
    /// `[integration.engine]`'s (kept) and `[integration.link]`'s (never made here).
    fn read(integration: &toml::Table) -> Result<IntegrationTables> {
        let mut t = IntegrationTables::default();
        if let Some(v) = integration.get("engine") {
            t.engine = Some((v.clone(), Descriptor::from_toml_value(v)));
        }
        if let Some(v) = integration.get("loader") {
            t.loader = Some(read_loader(v)?);
        }
        if let Some(v) = integration.get("capabilities") {
            t.capabilities = read_capabilities(v)?;
        }
        if let Some(v) = integration.get("foundation") {
            let (claim, unknown) = read_foundation(v)?;
            t.foundation = Some(claim);
            t.foundation_unknown = unknown;
        }
        t.link = integration.get("link").cloned();
        if let Some(v) = integration.get("sdk") {
            match read_sdk(v) {
                Ok(url) => t.sdk = Some(url),
                Err(_) => t.sdk_unusable = true,
            }
        }
        Ok(t)
    }

    /// The codes `[integration.engine]` declares (`engine.codes`' keys), when it can be read so
    /// far: an unreadable descriptor says why itself.
    fn declared_codes(&self) -> Option<Vec<String>> {
        let (raw, _) = self.engine.as_ref()?;
        raw.get("engine")
            .and_then(|e| e.get("codes"))
            .and_then(toml::Value::as_table)
            .map(|c| c.keys().cloned().collect())
    }
}

/// `[integration] id`'s alphabet.
fn is_integration_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// One piece of a string that may carry placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece<'a> {
    Lit(&'a str),
    Var(&'a str),
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Splits `a ${b} c` into literal and placeholder pieces. A `${` that does not close, or a name
/// that is not an identifier, is `unknown-placeholder`.
pub(crate) fn pieces(s: &str) -> Result<Vec<Piece<'_>>> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        if start > 0 {
            out.push(Piece::Lit(&rest[..start]));
        }
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(LauncherError::new(codes::UNKNOWN_PLACEHOLDER).with("name", after));
        };
        let name = &after[..end];
        if !is_identifier(name) {
            return Err(LauncherError::new(codes::UNKNOWN_PLACEHOLDER).with("name", name));
        }
        out.push(Piece::Var(name));
        rest = &after[end + 1..];
    }
    if !rest.is_empty() {
        out.push(Piece::Lit(rest));
    }
    Ok(out)
}

/// The launcher's own placeholders.
pub(crate) const OWN_PLACEHOLDERS: &[&str] = &["profile", "game"];

fn collect_vars(s: &str, into: &mut BTreeSet<String>) -> Result<()> {
    for p in pieces(s)? {
        if let Piece::Var(name) = p
            && !OWN_PLACEHOLDERS.contains(&name)
        {
            into.insert(name.to_owned());
        }
    }
    Ok(())
}

/// A `from` pattern inside the zip: an exact path, or `dir/**` (everything below `dir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FromPattern {
    Exact(String),
    Below(String),
}

pub(crate) fn from_pattern(from: &str, key: &str) -> Result<FromPattern> {
    if from.contains("${") {
        return Err(LauncherError::new(codes::UNKNOWN_PLACEHOLDER)
            .with("name", crate::paths::first_placeholder_name(from)));
    }
    if from == "**" {
        return Ok(FromPattern::Below(String::new()));
    }
    if let Some(dir) = from.strip_suffix("/**") {
        if dir.contains(['*', '?', '[']) || relative_components(dir).is_none() || dir.contains('\\')
        {
            return Err(invalid(key, "only `dir/**` and exact paths are supported"));
        }
        return Ok(FromPattern::Below(format!("{dir}/")));
    }
    if from.contains(['*', '?', '[']) || from.contains('\\') || relative_components(from).is_none()
    {
        return Err(invalid(key, "only `dir/**` and exact paths are supported"));
    }
    Ok(FromPattern::Exact(from.to_owned()))
}

impl Manifest {
    /// Reads and checks a manifest. Every refusal carries a code.
    pub fn parse(text: &str) -> Result<Manifest> {
        let table: toml::Table =
            toml::from_str(text).map_err(|e| invalid("", e.message().to_owned()))?;

        // The schema first: a newer schema may well carry keys this launcher does not know.
        match table.get("schema") {
            Some(toml::Value::Integer(n)) if *n > i64::from(SCHEMA) => {
                return Err(LauncherError::new(codes::SCHEMA_TOO_NEW)
                    .with("schema", n.to_string())
                    .with("supported", SCHEMA.to_string()));
            }
            Some(toml::Value::Integer(n)) if *n == i64::from(SCHEMA) => {}
            Some(_) => return Err(invalid("schema", format!("must be {SCHEMA}"))),
            None => return Err(invalid("schema", "missing")),
        }

        check_keys(&table, TOP_KEYS, "")?;
        let integration = match table.get("integration") {
            Some(toml::Value::Table(i)) => i,
            Some(_) => return Err(invalid("integration", "must be a table")),
            None => return Err(invalid("integration", "missing")),
        };
        if !matches!(table.get("target"), Some(toml::Value::Table(_))) {
            return Err(invalid("target", "missing"));
        }
        check_table(table.get("target"), TARGET_KEYS, "target")?;
        check_array(table.get("files"), FILES_KEYS, "files")?;
        check_array(table.get("game_files"), GAME_FILES_KEYS, "game_files")?;
        check_table(table.get("launch"), LAUNCH_KEYS, "launch")?;
        check_array(
            table.get("config_writes"),
            CONFIG_WRITES_KEYS,
            "config_writes",
        )?;

        let tables = IntegrationTables::read(integration)?;

        let mut m: Manifest =
            toml::from_str(text).map_err(|e| invalid("", e.message().to_owned()))?;
        m.tables = tables;
        m.check()?;
        m.mods = integration.get("mods").map(ModsDecl::from_toml);
        Ok(m)
    }

    /// `[integration.mods]`: `None` when the integration does not declare it (it plays Astra on
    /// its own only), `Some(Err)` when it does in a way this launcher cannot use (an unknown key or
    /// loader, a path outside the profile): the refusal a Play with mods answers with.
    pub fn mods(&self) -> Option<std::result::Result<&ModsDecl, &LauncherError>> {
        self.mods.as_ref().map(|r| r.as_ref())
    }

    fn check(&self) -> Result<()> {
        let i = &self.integration;
        if !is_integration_id(&i.id) {
            return Err(invalid("integration.id", "letters, digits, . - _"));
        }
        if i.name.trim().is_empty() {
            return Err(invalid("integration.name", "missing"));
        }
        if i.version.trim().is_empty() {
            return Err(invalid("integration.version", "missing"));
        }

        let t = &self.target;
        if t.exe != "*" && (t.exe.contains('\\') || relative_components(&t.exe).is_none()) {
            return Err(LauncherError::new(codes::PATH_ESCAPES).with("path", t.exe.clone()));
        }
        // Any engine code but `unknown`: whether a game is made with it is planning's question
        // (`engine-mismatch`), so a Play or a mods look, which read the ledger's copy again, never
        // need the engines a catalogue taught.
        if let Some(e) = &t.engine
            && EngineCode::new(e).is_none_or(|c| c.is_unknown())
        {
            return Err(invalid(
                "target.engine",
                format!("not an engine code {e:?}"),
            ));
        }
        // A foundation owns the code it detects: `[target] engine` names one of them.
        if self.tables.engine.is_some() {
            let declared = self.tables.declared_codes();
            let named = match (&t.engine, &declared) {
                (None, _) => false,
                (Some(want), Some(codes)) => codes.contains(want),
                (Some(_), None) => true,
            };
            if !named {
                return Err(LauncherError::new(codes::ENGINE_UNDECLARED)
                    .with("engine", t.engine.clone().unwrap_or_default())
                    .with("codes", declared.unwrap_or_default().join(",")));
            }
        }
        if t.platforms.is_empty() {
            return Err(invalid("target.platforms", "empty"));
        }
        if t.anti_cheat.trim().is_empty() {
            return Err(invalid("target.anti_cheat", "missing"));
        }

        for (n, f) in self.files.iter().enumerate() {
            let key = format!("files[{n}]");
            let pattern = from_pattern(&f.from, &format!("{key}.from"))?;
            let to = parse_jailed(&f.to)?;
            if to.root != Root::Profile {
                return Err(LauncherError::new(codes::PATH_ESCAPES)
                    .with("path", f.to.clone())
                    .with("reason", "files-go-into-the-profile"));
            }
            if matches!(pattern, FromPattern::Below(_)) && !to.is_dir {
                return Err(invalid(
                    &format!("{key}.to"),
                    "a `dir/**` source needs a folder (`…/`)",
                ));
            }
            if to.rel.is_empty() && !to.is_dir {
                return Err(invalid(&format!("{key}.to"), "names no file"));
            }
        }

        for (n, g) in self.game_files.iter().enumerate() {
            let key = format!("game_files[{n}]");
            if let FromPattern::Below(_) = from_pattern(&g.from, &format!("{key}.from"))? {
                return Err(invalid(
                    &format!("{key}.from"),
                    "game files are named one by one",
                ));
            }
            let to = parse_jailed(&g.to)?;
            if to.root != Root::Game {
                return Err(LauncherError::new(codes::PATH_ESCAPES)
                    .with("path", g.to.clone())
                    .with("reason", "game-files-go-beside-the-exe"));
            }
            if to.is_dir || to.rel.is_empty() {
                return Err(invalid(&format!("{key}.to"), "must name a file"));
            }
            if let Some(h) = &g.sha256
                && normalize_hex(h).is_none()
            {
                return Err(invalid(&format!("{key}.sha256"), "64 hex digits"));
            }
        }

        let mut vars = BTreeSet::new();
        for a in &self.launch.args {
            collect_vars(a, &mut vars)?;
        }
        for (k, v) in &self.launch.env {
            if !is_identifier(k) {
                return Err(invalid(&format!("launch.env.{k}"), "not a variable name"));
            }
            collect_vars(v, &mut vars)?;
        }
        let mut dlls = BTreeSet::new();
        for (dll, value) in &self.launch.proton_dll_overrides {
            let key = format!("launch.proton_dll_overrides.{dll}");
            if !is_dll_name(dll) {
                return Err(invalid(&key, "not a DLL name"));
            }
            if !dlls.insert(dll.to_ascii_lowercase()) {
                return Err(invalid(&key, "named twice (Wine ignores letter case)"));
            }
            if !OVERRIDE_VALUES.contains(&value.as_str()) {
                return Err(invalid(&key, "not a Wine override"));
            }
        }

        for (n, c) in self.config_writes.iter().enumerate() {
            let key = format!("config_writes[{n}]");
            let file = parse_jailed(&c.file)?;
            if file.root != Root::Profile {
                return Err(LauncherError::new(codes::PATH_ESCAPES)
                    .with("path", c.file.clone())
                    .with("reason", "config-writes-stay-in-the-profile"));
            }
            if file.is_dir || file.rel.is_empty() {
                return Err(invalid(&format!("{key}.file"), "must name a file"));
            }
            if c.section.is_empty() || c.section.contains([']', '[', '\n', '\r']) {
                return Err(invalid(
                    &format!("{key}.section"),
                    "not an INI section name",
                ));
            }
            for (k, v) in &c.set {
                if k.trim().is_empty()
                    || k.trim() != k
                    || k.contains(['=', '[', ']', '\n', '\r', '#', ';'])
                {
                    return Err(invalid(&format!("{key}.set.{k}"), "not an INI key"));
                }
                if v.contains(['\n', '\r']) {
                    return Err(invalid(&format!("{key}.set.{k}"), "a value is one line"));
                }
                collect_vars(v, &mut vars)?;
            }
        }
        Ok(())
    }

    /// [`Manifest::parse`] of `text`, and every refusal [`Manifest::lint`] makes beyond it. When
    /// `parse` refuses, the one thing still asked is `[integration.engine]` (its own refusal,
    /// else Pass B's): a refusal there stops `parse` before the descriptor an author is working
    /// on is read, and `engine-undeclared` is one of them.
    pub fn parse_and_lint(text: &str, first_party: bool) -> (Result<Manifest>, Vec<LauncherError>) {
        match Manifest::parse(text) {
            Ok(m) => {
                let lint = m.lint(first_party);
                (Ok(m), lint)
            }
            Err(e) => {
                let raw = toml::from_str::<toml::Table>(text)
                    .ok()
                    .and_then(|t| t.get("integration")?.get("engine").cloned());
                let lint = match raw.map(|v| Descriptor::from_toml_value(&v)) {
                    Some(Err(e)) => vec![e],
                    Some(Ok(d)) => d.check_pass_b(first_party).err().into_iter().collect(),
                    None => Vec::new(),
                };
                (Err(e), lint)
            }
        }
    }

    /// The caller's variables this manifest uses (everything but `${profile}` and `${game}`).
    pub fn variables(&self) -> BTreeSet<String> {
        let mut vars = BTreeSet::new();
        let strings = self
            .launch
            .args
            .iter()
            .chain(self.launch.env.values())
            .chain(self.config_writes.iter().flat_map(|c| c.set.values()));
        for s in strings {
            let _ = collect_vars(s, &mut vars);
        }
        vars
    }

    /// Every Proton override, `(dll, value)`, in the manifest's key order (byte order, so the same
    /// manifest always yields the same list).
    pub fn proton_overrides(&self) -> Vec<(String, String)> {
        self.launch
            .proton_dll_overrides
            .iter()
            .map(|(d, v)| (d.clone(), v.clone()))
            .collect()
    }

    /// The overrides as one `WINEDLLOVERRIDES` value, `a=n,b;c=n`, in the order of
    /// [`Self::proton_overrides`]; `None` when there are none. The launcher itself writes the
    /// overrides into the prefix's `user.reg` (a Steam launch carries no environment); this is the
    /// same set for a person or a caller that has to show or paste it.
    pub fn wine_dll_overrides(&self) -> Option<String> {
        wine_dll_overrides(&self.proton_overrides())
    }

    /// The built-in engine `[target]` asks for; `None` for none, and for a code an engine
    /// descriptor declares ([`Manifest::engine_code`] has every code).
    pub fn engine(&self) -> Option<Engine> {
        self.target.engine.as_deref().and_then(Engine::from_code)
    }

    /// The engine `[target]` asks for: a built-in one's code or one an engine descriptor declares
    /// (never `unknown`, which [`Manifest::parse`] refuses).
    pub fn engine_code(&self) -> Option<EngineCode> {
        self.target.engine.as_deref().and_then(EngineCode::new)
    }

    /// `[integration.engine]` exactly as the manifest writes it: what a catalogue entry's
    /// `descriptor` must be the same object as.
    pub fn engine_table(&self) -> Option<&toml::Value> {
        self.tables.engine.as_ref().map(|(raw, _)| raw)
    }

    /// `[integration.engine]` as an `astra.engine/1` descriptor: `None` when the manifest has
    /// none, else the descriptor or why it is not one. The manifest is never refused for it.
    pub fn engine_descriptor(&self) -> Option<std::result::Result<&Descriptor, &LauncherError>> {
        self.tables.engine.as_ref().map(|(_, d)| d.as_ref())
    }

    /// `[integration.loader]`, when the manifest has one.
    pub fn loader(&self) -> Option<&LoaderDecl> {
        self.tables.loader.as_ref()
    }

    /// Whether `[integration.capabilities] companion` lists `id` ([`COMPANION_PRESENTATION`] is
    /// v1's only one).
    pub fn declares_companion(&self, id: &str) -> bool {
        self.tables
            .capabilities
            .get("companion")
            .is_some_and(|ids| ids.contains(id))
    }

    /// `[integration.foundation]`, the claim, when the manifest makes one.
    pub fn foundation(&self) -> Option<&FoundationClaim> {
        self.tables.foundation.as_ref()
    }

    /// `[integration.link]` exactly as written. The launcher acts on none of it;
    /// [`Manifest::lint`] holds it to Astra Link v1's limits, and [`Manifest::link_files`] names
    /// the files it points at.
    pub fn link(&self) -> Option<&toml::Value> {
        self.tables.link.as_ref()
    }

    /// `[integration] sdk`: the URL a developer building on this integration starts from. `None`
    /// when absent or no usable URL (an `https://` URL, no space or control character, at most
    /// 2,048 bytes); the latter is [`Manifest::lint`]'s finding.
    pub fn sdk(&self) -> Option<&str> {
        self.tables.sdk.as_deref()
    }

    /// `capability-undeclared{capability, variable}` when the manifest hands `${bridge_token}` on
    /// (`[[config_writes]]`, `[launch]` args or env) without `companion = ["presentation"]`. Not
    /// a refusal of [`Manifest::parse`]: every first-party release from before 0.5.0 hands it on
    /// undeclared, and must still install. [`Manifest::lint`] makes it.
    pub fn check_capabilities_used(&self) -> Result<()> {
        if self.variables().contains(BRIDGE_TOKEN_VARIABLE)
            && !self.declares_companion(COMPANION_PRESENTATION)
        {
            return Err(LauncherError::new(codes::CAPABILITY_UNDECLARED)
                .with("capability", format!("companion.{COMPANION_PRESENTATION}"))
                .with("variable", BRIDGE_TOKEN_VARIABLE));
        }
        Ok(())
    }

    /// The files `[integration.link]` names, `(field, path)` in the manifest's order: each
    /// tool's `params`, each trigger's and event's `data`, then `skill`. Only the string ones.
    pub fn link_files(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let Some(link) = self.link() else {
            return out;
        };
        for (list, file) in [
            ("tools", "params"),
            ("triggers", "data"),
            ("events", "data"),
        ] {
            if let Some(toml::Value::Array(items)) = link.get(list) {
                for (i, item) in items.iter().enumerate() {
                    if let Some(p) = item.get(file).and_then(toml::Value::as_str) {
                        out.push((format!("integration.link.{list}[{i}].{file}"), p.to_owned()));
                    }
                }
            }
        }
        if let Some(p) = link.get("skill").and_then(toml::Value::as_str) {
            out.push(("integration.link.skill".to_owned(), p.to_owned()));
        }
        out
    }

    /// What a release must pass beyond [`Manifest::parse`] before it is published, with no zip
    /// and no game: what `gi-probe check` (a catalogue's ingest) refuses, and a local import can.
    /// In this order, every one found:
    ///
    /// 1. a community integration's identity (`reserved-id`, `reserved-name`;
    ///    [`crate::reserved::check_identity`]), then what a community integration may not place
    ///    or launch (`loader-undeclared`, `community-limit`; plan B §3, `crate::plan`'s
    ///    `community_limits`) — Astra's own (`first_party`) are not asked;
    /// 2. `capability-undeclared` ([`Manifest::check_capabilities_used`]);
    /// 3. `[integration.link]`: its shape, Astra Link v1's counts, names (`^[a-z][a-z0-9_]{0,31}$`,
    ///    once each per list), description and label lengths, and relative file paths
    ///    (`link-invalid{field, reason}`, or `unknown-key` for a key it does not have);
    /// 4. `[integration.foundation]`'s unknown keys (`unknown-key`);
    /// 5. an `[integration] sdk` that is no usable URL (`manifest-invalid{key=integration.sdk}`:
    ///    metadata, which parse drops);
    /// 6. `[integration.engine]`: its own refusal, else Pass B's
    ///    ([`Descriptor::check_pass_b`] with `first_party`).
    ///
    /// The zip's half (the files a link names, their sizes; `built_on`; `code_changed`) and the
    /// descriptor's negative folders are `gi-probe check`'s. [`Manifest::parse_and_lint`] runs
    /// this on text.
    pub fn lint(&self, first_party: bool) -> Vec<LauncherError> {
        let mut out = Vec::new();
        if !first_party {
            if let Err(e) = crate::reserved::check_identity(&self.integration) {
                out.push(e);
            }
            out.extend(crate::plan::community_limits(self));
        }
        if let Err(e) = self.check_capabilities_used() {
            out.push(e);
        }
        if let Some(link) = self.link() {
            lint_link(link, &mut out);
        }
        for key in &self.tables.foundation_unknown {
            out.push(unknown_key(key.clone()));
        }
        if self.tables.sdk_unusable {
            out.push(invalid("integration.sdk", "an https:// URL"));
        }
        match self.engine_descriptor() {
            Some(Err(e)) => out.push(e.clone()),
            Some(Ok(d)) => {
                if let Err(e) = d.check_pass_b(first_party) {
                    out.push(e);
                }
            }
            None => {}
        }
        out
    }

    pub(crate) fn game_file_dest(&self, n: usize) -> JailPath {
        parse_jailed(&self.game_files[n].to).expect("checked at parse")
    }
}

fn link_invalid(field: &str, reason: &str) -> LauncherError {
    LauncherError::new(codes::LINK_INVALID)
        .with("field", field)
        .with("reason", reason)
}

/// A Link name: `^[a-z][a-z0-9_]{0,31}$`.
fn is_link_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=link_limits::NAME_CHARS).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// `[integration.link]` against Astra Link v1 (spec `manifest.md` §2.6, `link-v1.md`): every
/// finding, in the table's order.
fn lint_link(link: &toml::Value, out: &mut Vec<LauncherError>) {
    let toml::Value::Table(t) = link else {
        out.push(link_invalid("integration.link", "shape"));
        return;
    };
    const KEYS: &[&str] = &[
        "context",
        "shares_other_players",
        "skill",
        "tools",
        "triggers",
        "events",
    ];
    for k in t.keys().filter(|k| !KEYS.contains(&k.as_str())) {
        out.push(unknown_key(format!("integration.link.{k}")));
    }
    for k in ["context", "shares_other_players"] {
        if t.get(k).is_some_and(|v| !v.is_bool()) {
            out.push(link_invalid(&format!("integration.link.{k}"), "shape"));
        }
    }
    let is_path = |p: &str| !p.contains('\\') && relative_components(p).is_some();
    match t.get("skill") {
        Some(toml::Value::String(p)) if !is_path(p) => {
            out.push(link_invalid("integration.link.skill", "not-a-path"));
        }
        Some(toml::Value::String(_)) | None => {}
        Some(_) => out.push(link_invalid("integration.link.skill", "shape")),
    }
    // (list, at most, the file key, the text key and whether it is required).
    let lists: [(&str, usize, &str, Option<&str>); 3] = [
        ("tools", link_limits::TOOLS, "params", Some("description")),
        ("triggers", link_limits::TRIGGERS, "data", Some("label")),
        ("events", link_limits::EVENTS, "data", None),
    ];
    for (list, most, file, text) in lists {
        let field = format!("integration.link.{list}");
        let items = match t.get(list) {
            None => continue,
            Some(toml::Value::Array(items)) => items,
            Some(_) => {
                out.push(link_invalid(&field, "shape"));
                continue;
            }
        };
        if items.len() > most {
            out.push(
                link_invalid(&field, "too-many")
                    .with("limit", most.to_string())
                    .with("count", items.len().to_string()),
            );
        }
        let mut names = BTreeSet::new();
        for (i, item) in items.iter().enumerate() {
            let at = format!("{field}[{i}]");
            let toml::Value::Table(e) = item else {
                out.push(link_invalid(&at, "shape"));
                continue;
            };
            for k in e.keys() {
                if !["name", file].contains(&k.as_str()) && text != Some(k.as_str()) {
                    out.push(unknown_key(format!("{at}.{k}")));
                }
            }
            match e.get("name").and_then(toml::Value::as_str) {
                Some(n) if !is_link_name(n) => {
                    out.push(link_invalid(&format!("{at}.name"), "name"))
                }
                Some(n) if !names.insert(n.to_owned()) => {
                    out.push(link_invalid(&format!("{at}.name"), "duplicate"));
                }
                Some(_) => {}
                None => out.push(link_invalid(&format!("{at}.name"), "missing")),
            }
            if let Some(text) = text {
                match e.get(text).and_then(toml::Value::as_str) {
                    Some(d) if d.chars().count() > link_limits::DESCRIPTION_CHARS => out.push(
                        link_invalid(&format!("{at}.{text}"), "too-long")
                            .with("limit", link_limits::DESCRIPTION_CHARS.to_string()),
                    ),
                    Some(d) if d.trim().is_empty() || d.chars().any(char::is_control) => {
                        out.push(link_invalid(&format!("{at}.{text}"), "shape"));
                    }
                    Some(_) => {}
                    None => out.push(link_invalid(&format!("{at}.{text}"), "missing")),
                }
            }
            match e.get(file).and_then(toml::Value::as_str) {
                Some(p) if !is_path(p) => {
                    out.push(link_invalid(&format!("{at}.{file}"), "not-a-path"));
                }
                Some(_) => {}
                None => out.push(link_invalid(&format!("{at}.{file}"), "missing")),
            }
        }
    }
}

/// True when a name is a component a zip entry or a manifest may use.
pub(crate) fn safe_component(c: &str) -> bool {
    check_component(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A community game integration with `integration` appended to `[integration]` and `target`
    /// as `[target]`'s extra lines.
    fn manifest(integration: &str, target: &str, rest: &str) -> String {
        format!(
            r#"schema = 1

[integration]
id = "someone.example"
name = "Astra in Example"
version = "1.0.0"
{integration}

[target]
exe = "*"
{target}
platforms = ["windows-x64"]
anti_cheat = "none"
{rest}"#
        )
    }

    const SOURCE2: &str = r#"
[integration.engine]
schema = "astra.engine/1"
engine = { codes = { source2 = "Source 2" } }
[[integration.engine.rule]]
tier = "strong"
programs = { ext = ["exe"], at = "{project}/bin/win64", first = 8 }
require = [ { listed = "engine2.dll", kind = "file" }, { child = "{project}/core", kind = "dir", as = "core" } ]
code = "source2"
confidence = "high"
binary = "windows"
evidence = "{core}"
"#;

    fn code_of(text: &str) -> &'static str {
        Manifest::parse(text).unwrap_err().code
    }

    #[test]
    fn target_engine_is_any_code_but_unknown() {
        for e in ["unity-mono", "source2", "id-tech-6"] {
            let m = Manifest::parse(&manifest("", &format!("engine = \"{e}\""), "")).unwrap();
            assert_eq!(m.engine_code().unwrap().as_str(), e);
            assert_eq!(
                m.engine().map(Engine::code),
                Engine::from_code(e).map(Engine::code)
            );
        }
        for e in ["unknown", "Source2", "s", "a_b", ""] {
            let e = Manifest::parse(&manifest("", &format!("engine = \"{e}\""), "")).unwrap_err();
            assert_eq!(
                (e.code, e.param("key")),
                (codes::MANIFEST_INVALID, Some("target.engine"))
            );
        }
    }

    #[test]
    fn engine_undeclared() {
        // The owner names its code: parses, and the descriptor is read.
        let m = Manifest::parse(&manifest(SOURCE2, "engine = \"source2\"", "")).unwrap();
        assert!(m.engine_descriptor().unwrap().is_ok());
        // No [target] engine, or one the descriptor does not declare.
        for target in ["", "engine = \"unity-mono\"", "engine = \"source3\""] {
            let e = Manifest::parse(&manifest(SOURCE2, target, "")).unwrap_err();
            assert_eq!(e.code, codes::ENGINE_UNDECLARED, "{target:?}");
            assert_eq!(e.param("codes"), Some("source2"));
        }
        // A descriptor that does not parse is kept, never the manifest's refusal.
        let broken = SOURCE2.replace("binary = \"windows\"\n", "");
        let m = Manifest::parse(&manifest(&broken, "engine = \"source2\"", "")).unwrap();
        assert_eq!(
            m.engine_descriptor().unwrap().unwrap_err().code,
            codes::DESCRIPTOR_INVALID
        );
        assert_eq!(
            m.lint(false).iter().map(|e| e.code).collect::<Vec<_>>(),
            [codes::DESCRIPTOR_INVALID]
        );
        // An unknown descriptor schema: still the manifest of a launcher that cannot read it.
        let newer = SOURCE2.replace("astra.engine/1", "astra.engine/2");
        let m = Manifest::parse(&manifest(&newer, "engine = \"source2\"", "")).unwrap();
        assert_eq!(
            m.engine_descriptor().unwrap().unwrap_err().code,
            codes::DESCRIPTOR_SCHEMA_UNKNOWN
        );
        // parse_and_lint still reads the descriptor of a manifest parse refuses.
        let (parsed, lint) = Manifest::parse_and_lint(&manifest(&broken, "", ""), false);
        assert_eq!(parsed.unwrap_err().code, codes::ENGINE_UNDECLARED);
        assert_eq!(
            lint.iter().map(|e| e.code).collect::<Vec<_>>(),
            [codes::DESCRIPTOR_INVALID]
        );
    }

    #[test]
    fn needs_newer_astra() {
        let ok = manifest(
            "[integration.capabilities]\ncompanion = [\"presentation\"]",
            "",
            "",
        );
        assert!(
            Manifest::parse(&ok)
                .unwrap()
                .declares_companion(COMPANION_PRESENTATION)
        );
        let empty = manifest("[integration.capabilities]\ncompanion = []", "", "");
        assert!(
            !Manifest::parse(&empty)
                .unwrap()
                .declares_companion(COMPANION_PRESENTATION)
        );
        for (caps, cap) in [
            (
                "companion = [\"presentation\", \"verbs\"]",
                "companion.verbs",
            ),
            ("input = [\"keyboard\"]", "input"),
        ] {
            let text = manifest(&format!("[integration.capabilities]\n{caps}"), "", "");
            let e = Manifest::parse(&text).unwrap_err();
            assert_eq!(
                (e.code, e.param("capability")),
                (codes::NEEDS_NEWER_ASTRA, Some(cap))
            );
        }
        let wrong = manifest(
            "[integration.capabilities]\ncompanion = \"presentation\"",
            "",
            "",
        );
        assert_eq!(code_of(&wrong), codes::MANIFEST_INVALID);
    }

    #[test]
    fn loader_policy_conflict() {
        let loader = |body: &str| manifest(&format!("[integration.loader]\n{body}"), "", "");
        let m = Manifest::parse(&loader(
            "proxies = [\"dinput8.dll\", \"winhttp.dll\"]\nkeep_if_present = [\"s2bridge.ini\", \"doorstop_config.INI\"]",
        ))
        .unwrap();
        assert_eq!(m.loader().unwrap().proxies, ["dinput8.dll", "winhttp.dll"]);
        for (body, reason) in [
            (
                "proxies = [\"dinput8.dll\"]\nkeep_if_present = [\"DInput8.DLL\"]",
                "proxy",
            ),
            ("keep_if_present = [\"version.dll\"]", "loader"),
            ("keep_if_present = [\"plugin.ASI\"]", "loader"),
            ("keep_if_present = [\"launcher.exe\"]", "loader"),
        ] {
            let e = Manifest::parse(&loader(body)).unwrap_err();
            assert_eq!(
                (e.code, e.param("reason")),
                (codes::LOADER_POLICY_CONFLICT, Some(reason)),
                "{body}"
            );
        }
        for body in [
            "keep_if_present = [\"notes.md\"]",
            "keep_if_present = [\"sub/x.ini\"]",
            "proxies = [\"../x.dll\"]",
            "proxies = [\"a.dll\", \"A.DLL\"]",
            "proxies = \"a.dll\"",
        ] {
            assert_eq!(code_of(&loader(body)), codes::MANIFEST_INVALID, "{body}");
        }
        // It acts: a key it does not know is refused.
        let e = Manifest::parse(&loader("proxies = []\nreplace = [\"x.dll\"]")).unwrap_err();
        assert_eq!(
            (e.code, e.param("key")),
            (codes::UNKNOWN_KEY, Some("integration.loader.replace"))
        );
    }

    #[test]
    fn capability_undeclared() {
        let writes = "[[config_writes]]\nfile = \"${profile}/a.ini\"\nsection = \"s\"\nset = { Token = \"${bridge_token}\" }\n";
        let m = Manifest::parse(&manifest("", "", writes)).unwrap();
        let e = m.check_capabilities_used().unwrap_err();
        assert_eq!(
            (e.code, e.param("capability")),
            (codes::CAPABILITY_UNDECLARED, Some("companion.presentation"))
        );
        assert_eq!(
            m.lint(true).iter().map(|e| e.code).collect::<Vec<_>>(),
            [codes::CAPABILITY_UNDECLARED]
        );
        // In [launch] too.
        let args = "[launch]\nargs = [\"--token\", \"${bridge_token}\"]\n";
        assert!(
            Manifest::parse(&manifest("", "", args))
                .unwrap()
                .check_capabilities_used()
                .is_err()
        );
        let declared = manifest(
            "[integration.capabilities]\ncompanion = [\"presentation\"]",
            "",
            writes,
        );
        assert!(Manifest::parse(&declared).unwrap().lint(false).is_empty());
        let port_only = writes.replace("bridge_token", "bridge_port");
        assert!(
            Manifest::parse(&manifest("", "", &port_only))
                .unwrap()
                .lint(false)
                .is_empty()
        );
    }

    #[test]
    fn reserved_id_and_name() {
        let with = |id: &str, name: &str| {
            manifest("", "", "")
                .replace("someone.example", id)
                .replace("Astra in Example", name)
        };
        let lint = |text: String, fp: bool| -> Vec<&'static str> {
            Manifest::parse(&text)
                .unwrap()
                .lint(fp)
                .iter()
                .map(|e| e.code)
                .collect()
        };
        assert_eq!(
            lint(with("astra.example", "Astra in Example"), false),
            [codes::RESERVED_ID]
        );
        assert_eq!(
            lint(with("someone.x", "The Official Mod"), false),
            [codes::RESERVED_NAME]
        );
        // Astra's own are not asked.
        assert!(lint(with("astra.example", "Official"), true).is_empty());
    }

    #[test]
    fn link_invalid() {
        let link = |body: &str| manifest(&format!("[integration.link]\n{body}"), "", "");
        let lint = |body: &str| -> Vec<(&'static str, String, String)> {
            Manifest::parse(&link(body))
                .unwrap()
                .lint(false)
                .iter()
                .map(|e| {
                    (
                        e.code,
                        e.param("field").or(e.param("key")).unwrap_or("").to_owned(),
                        e.param("reason").unwrap_or("").to_owned(),
                    )
                })
                .collect()
        };
        let tool = |name: &str, d: &str| {
            format!(
                "[[integration.link.tools]]\nname = \"{name}\"\ndescription = \"{d}\"\nparams = \"astra/t.json\"\n"
            )
        };
        assert!(
            lint(&format!(
                "context = true\nskill = \"astra/SKILL.md\"\n{}",
                tool("drop_item", "Drop it")
            ))
            .is_empty()
        );
        let many: String = (0..17).map(|i| tool(&format!("t{i}"), "x")).collect();
        assert_eq!(
            lint(&many),
            [(
                codes::LINK_INVALID,
                "integration.link.tools".into(),
                "too-many".into()
            )]
        );
        // Each list at its own limit (Link v1: 16 tools, 16 triggers, 32 events), and one past it.
        let trigger = |i: usize| {
            format!(
                "[[integration.link.triggers]]\nname = \"g{i}\"\nlabel = \"G\"\ndata = \"astra/g.json\"\n"
            )
        };
        let event = |i: usize| {
            format!("[[integration.link.events]]\nname = \"e{i}\"\ndata = \"astra/e.json\"\n")
        };
        for (list, item, most) in [
            (
                "tools",
                &(|i: usize| tool(&format!("t{i}"), "x")) as &dyn Fn(usize) -> String,
                16,
            ),
            ("triggers", &trigger, 16),
            ("events", &event, 32),
        ] {
            let at: String = (0..most).map(item).collect();
            assert!(lint(&at).is_empty(), "{list} at {most}: {:?}", lint(&at));
            let past: String = (0..=most).map(item).collect();
            assert_eq!(
                lint(&past),
                [(
                    codes::LINK_INVALID,
                    format!("integration.link.{list}"),
                    "too-many".into()
                )],
                "{list} past {most}"
            );
        }
        let field = |f: &str| format!("integration.link.tools[0].{f}");
        assert_eq!(
            lint(&tool("Drop", "x")),
            [(codes::LINK_INVALID, field("name"), "name".into())]
        );
        assert_eq!(
            lint(&tool(&"a".repeat(33), "x")),
            [(codes::LINK_INVALID, field("name"), "name".into())]
        );
        assert!(lint(&tool(&"a".repeat(32), "x")).is_empty());
        assert_eq!(
            lint(&tool("t", &"é".repeat(201))),
            [(codes::LINK_INVALID, field("description"), "too-long".into())]
        );
        assert!(lint(&tool("t", &"é".repeat(200))).is_empty());
        assert_eq!(
            lint(&format!("{}{}", tool("t", "x"), tool("t", "y"))),
            [(
                codes::LINK_INVALID,
                "integration.link.tools[1].name".into(),
                "duplicate".into()
            )]
        );
        assert_eq!(
            lint(&tool("t", "x").replace("astra/t.json", "../t.json")),
            [(codes::LINK_INVALID, field("params"), "not-a-path".into())]
        );
        assert_eq!(
            lint("[[integration.link.triggers]]\nname = \"boss\"\ndata = \"a.json\"\n"),
            [(
                codes::LINK_INVALID,
                "integration.link.triggers[0].label".into(),
                "missing".into()
            )]
        );
        assert_eq!(
            lint("verbs = true\n"),
            [(
                codes::UNKNOWN_KEY,
                "integration.link.verbs".into(),
                String::new()
            )]
        );
        assert_eq!(
            lint("context = \"yes\"\n"),
            [(
                codes::LINK_INVALID,
                "integration.link.context".into(),
                "shape".into()
            )]
        );
        let m = Manifest::parse(&link(&format!(
            "skill = \"astra/SKILL.md\"\n{}[[integration.link.events]]\nname = \"died\"\ndata = \"astra/e.json\"\n",
            tool("t", "x")
        )))
        .unwrap();
        assert_eq!(
            m.link_files(),
            [
                (
                    "integration.link.tools[0].params".to_owned(),
                    "astra/t.json".to_owned()
                ),
                (
                    "integration.link.events[0].data".to_owned(),
                    "astra/e.json".to_owned()
                ),
                (
                    "integration.link.skill".to_owned(),
                    "astra/SKILL.md".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn foundation_and_sdk() {
        let f = |body: &str| manifest(&format!("[integration.foundation]\n{body}"), "", "");
        let m = Manifest::parse(&f(
            "id = \"astra.unity\"\nversion = \"0.8.1\"\nengine = \"unity-mono\"\nsince = 3",
        ))
        .unwrap();
        assert_eq!(m.foundation().unwrap().engine.as_str(), "unity-mono");
        // An unknown key in the claim is the linter's, never the install's.
        assert_eq!(
            m.lint(false)
                .iter()
                .map(|e| (e.code, e.param("key")))
                .collect::<Vec<_>>(),
            [(codes::UNKNOWN_KEY, Some("integration.foundation.since"))]
        );
        for body in [
            "id = \"astra.unity\"\nversion = \"0.8.1\"",
            "id = \"a b\"\nversion = \"1\"\nengine = \"godot\"",
            "id = \"x\"\nversion = \"1\"\nengine = \"unknown\"",
        ] {
            assert_eq!(code_of(&f(body)), codes::MANIFEST_INVALID, "{body}");
        }
        let sdk = |url: &str| manifest(&format!("sdk = \"{url}\""), "", "");
        assert_eq!(
            Manifest::parse(&sdk("https://github.com/x/sdk"))
                .unwrap()
                .sdk(),
            Some("https://github.com/x/sdk")
        );
        // Metadata: an unusable one never refuses the manifest (an install, a Play that parses
        // the ledger's copy); it is dropped, and the linter says so.
        for url in ["http://x", "javascript:alert(1)", "https://a b", "https://"] {
            let m = Manifest::parse(&sdk(url)).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert_eq!(m.sdk(), None, "{url}");
            assert_eq!(
                m.lint(true)
                    .iter()
                    .map(|e| (e.code, e.param("key")))
                    .collect::<Vec<_>>(),
                [(codes::MANIFEST_INVALID, Some("integration.sdk"))],
                "{url}"
            );
        }
        let m = Manifest::parse(&manifest("sdk = 3", "", "")).unwrap();
        assert_eq!((m.sdk(), m.lint(true).len()), (None, 1));
    }

    #[test]
    fn the_tables_do_not_reach_what_serialises() {
        let plain = Manifest::parse(&manifest("", "engine = \"godot\"", "")).unwrap();
        let rich = Manifest::parse(&manifest(
            "sdk = \"https://x.example/sdk\"\n[integration.capabilities]\ncompanion = [\"presentation\"]\n[integration.loader]\nproxies = [\"dinput8.dll\"]",
            "engine = \"godot\"",
            "",
        ))
        .unwrap();
        assert_ne!(plain, rich);
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            serde_json::to_string(&rich).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&plain.integration).unwrap(),
            serde_json::to_value(&rich.integration).unwrap()
        );
    }
}
