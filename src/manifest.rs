// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `astra-gi.toml`, schema 1. The authority is astra-bepinex `docs/GAME-INTEGRATION-MANIFEST.md`.
//!
//! An unknown key in a section that ACTS (`[target]`, `[[files]]`, `[[game_files]]`, `[launch]`,
//! `[[config_writes]]`) is refused: a newer feature read as nothing would make an install that
//! looks fine and is broken. Unknown keys in `[integration]` (metadata) are ignored.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::detect::Engine;
use crate::digest::normalize_hex;
use crate::error::{LauncherError, Result, codes};
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
        match table.get("integration") {
            Some(toml::Value::Table(_)) => {}
            Some(_) => return Err(invalid("integration", "must be a table")),
            None => return Err(invalid("integration", "missing")),
        }
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

        let m: Manifest = toml::from_str(text).map_err(|e| invalid("", e.message().to_owned()))?;
        m.check()?;
        Ok(m)
    }

    fn check(&self) -> Result<()> {
        let i = &self.integration;
        if i.id.is_empty()
            || !i
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
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
        if let Some(e) = &t.engine
            && Engine::from_code(e).is_none_or(|e| e == Engine::Unknown)
        {
            return Err(invalid("target.engine", format!("unknown engine {e:?}")));
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
        if self.launch.proton_dll_overrides.len() > 1 {
            return Err(invalid(
                "launch.proton_dll_overrides",
                "schema 1 supports one override",
            ));
        }
        for (dll, value) in &self.launch.proton_dll_overrides {
            if dll.is_empty()
                || !dll
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(invalid(
                    &format!("launch.proton_dll_overrides.{dll}"),
                    "not a DLL name",
                ));
            }
            if !OVERRIDE_VALUES.contains(&value.as_str()) {
                return Err(invalid(
                    &format!("launch.proton_dll_overrides.{dll}"),
                    "not a Wine override",
                ));
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

    /// The single Proton override, if any: (dll, value).
    pub fn proton_override(&self) -> Option<(String, String)> {
        self.launch
            .proton_dll_overrides
            .iter()
            .next()
            .map(|(d, v)| (d.clone(), v.clone()))
    }

    /// The engine `[target]` asks for.
    pub fn engine(&self) -> Option<Engine> {
        self.target.engine.as_deref().and_then(Engine::from_code)
    }

    pub(crate) fn game_file_dest(&self, n: usize) -> JailPath {
        parse_jailed(&self.game_files[n].to).expect("checked at parse")
    }
}

/// True when a name is a component a zip entry or a manifest may use.
pub(crate) fn safe_component(c: &str) -> bool {
    check_component(c)
}
