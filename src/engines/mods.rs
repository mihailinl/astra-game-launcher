// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `mods`, the optional member of `astra.engine/1` that says how to recognise a mod loader the
//! player installed themselves, where it keeps their mods, which of them are enabled and how
//! Astra runs beside it (AstraPlugins `spec/games/engine-descriptor-v1.md` §7, at the pin in
//! `docs/ENGINE-DESCRIPTOR.md`). A descriptor holds no code: what a loader looks like is data,
//! and this module implements only the closed vocabularies §7 names.
//!
//! **Never detection.** [`Descriptor`](super::Descriptor) keeps the model built here beside the
//! one the evaluator reads, never in it, so no detection, no Pass B admission, no offer and no
//! plan can depend on it (§7.1; canary C1c). The only things `mods` changes are the digest and
//! the room left under the 16 KiB bound.
//!
//! **Validation** ([`validate`]) mirrors the specification's own checker
//! (`tools/check-games-goldens.py` `mods_reasons`, at the pin): every §7.9 reason a `mods` table
//! is refused for, each with the member it is about (`field`, `mods.loader[1].root`), derived
//! from the grammar and not from a schema's error text, so a hostile golden is refused for
//! exactly the reasons it is named for. A `mods` that is not a table is the descriptor's own
//! `descriptor-invalid` (field `mods`) and never reaches here. Anything else wrong inside it
//! voids only `mods` ([`Descriptor::mods_refusals`](super::Descriptor::mods_refusals)): the
//! descriptor detects, offers and plans as without it.
//!
//! Where this launcher reads more strictly than the checker (each also a launcher-only note in
//! `docs/ENGINE-DESCRIPTOR.md`): a loader's `name` keeps to the characters a display name may
//! hold ([`super::descriptor::display_chars_ok`], trimmed; `shape`); a `require` entry that is
//! not a table and a combinator that holds anything beside its one list, or an empty one, are
//! `shape` and `unknown`, as the schema refuses them; a `tail` without `equals` (1 to 16 bytes,
//! `bytes` its length when given) is `shape`; a code, id, template or `versions` followed by a
//! line break is refused, where the checker's Python `$` lets it through; a component is at most
//! 255 BYTES, as §7.3 says (the checker counts characters). Where the checker's `re.I` folds a
//! letter outside ASCII onto an ASCII one, so does this module ([`ci_char`]).
//!
//! **Evaluation** ([`evaluate`], in `mods/eval.rs`) reads one source — the game folder, the
//! active profile of a mod manager an adapter knows, or the folder the player picked — jailed
//! below it, read-only, within §7.8's per-evaluation bounds; nothing it finds is logged or
//! written anywhere (§7.10).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::EngineCode;
use super::descriptor::{Check, Kind, Model, PathT, Piece, display_chars_ok, is_valid_engine_code};
use crate::error::{LauncherError, codes};
use crate::plan::HIJACK_NAMES;

mod eval;
mod pe;

pub use eval::{
    Bound, Evaluation, LoaderOutcome, Mod, Recognised, Sources, Strategy, evaluate, evaluate_one,
};

/// §7.8's bounds and §7.1's nesting. Counted, never timed.
pub mod limits {
    /// Loaders in one `mods` member.
    pub const LOADERS: usize = 4;
    /// Top-level entries of one loader's `require` (§4's per-rule bound).
    pub const REQUIRE: usize = 8;
    /// `any`/`all`/`none` inside one another.
    pub const DEPTH: usize = 3;
    pub const ENABLED: usize = 3;
    pub const COEXIST: usize = 3;
    pub const SOURCES: usize = 3;
    pub const CODES: usize = 4;
    pub const KEYS: usize = 2;
    /// Across all loaders of one member: `listed`, `child` and `stat` lookups, and content
    /// probes (a `tail`, a `version`, an `enable-list` list and its `marker` each one).
    pub const LOOKUPS: usize = 64;
    pub const PROBES: usize = 8;
    /// Tables and arrays nested in `mods`, `mods` itself counted (§7.1).
    pub const NESTING: usize = 11;
    /// One §7.3 path, in characters; one component, in bytes.
    pub const PATH_CHARS: usize = 512;
    pub const COMPONENT_BYTES: usize = 255;
    /// A `file-glob` glob, in characters, and its `*`s.
    pub const GLOB_CHARS: usize = 64;
    pub const GLOB_STARS: usize = 2;
    /// A version template's tokens, and a `versions` operand's characters.
    pub const TEMPLATE_TOKENS: usize = 32;
    pub const OPERAND_CHARS: usize = 32;
    /// A `tail`'s bytes.
    pub const TAIL_BYTES: usize = 16;

    // Per evaluation: one game, one source. Over any → that loader yields nothing there.
    /// Entries listed in any one directory.
    pub const ENTRIES_PER_DIR: usize = 5000;
    /// Mods in one mods root.
    pub const MODS_PER_ROOT: usize = 1024;
    /// Lookups: every `require` lookup, every directory listed, every `marker` check.
    pub const LOOKUPS_PER_EVALUATION: u32 = 2048;
    /// Bytes read in all; of them a `version` probe at most [`VERSION_BYTES`], a list at most
    /// [`LIST_BYTES`].
    pub const BYTES_PER_EVALUATION: u64 = 128 * 1024;
    pub const VERSION_BYTES: u64 = 32 * 1024;
    pub const LIST_BYTES: u64 = 16 * 1024;
}

/// §7.9's reasons: the `reason` of every `mods-invalid`.
pub mod reasons {
    pub const UNKNOWN: &str = "unknown";
    pub const SHAPE: &str = "shape";
    pub const PATH_OUTSIDE_JAIL: &str = "path-outside-jail";
    pub const PATH_ESCAPES: &str = "path-escapes";
    pub const PATH_INVALID: &str = "path-invalid";
    pub const PLACEHOLDER_UNKNOWN: &str = "placeholder-unknown";
    pub const GLOB_OVER_BUDGET: &str = "glob-over-budget";
    pub const LOADER_ANCHOR_INVALID: &str = "loader-anchor-invalid";
    pub const ROOT_ANCHOR_NOT_PASSIVE: &str = "root-anchor-not-passive";
    pub const CODE_UNDECLARED: &str = "code-undeclared";
    pub const VERSION_INVALID: &str = "version-invalid";
    pub const CHAIN_ENTRY_INVALID: &str = "chain-entry-invalid";
    pub const LOADER_ID_DUPLICATE: &str = "loader-id-duplicate";
    pub const OVER_BUDGET: &str = "over-budget";
    pub const MANAGER_AS_DATA: &str = "manager-as-data";

    /// Every reason, in §7.9's order.
    pub const ALL: &[&str] = &[
        UNKNOWN,
        SHAPE,
        PATH_OUTSIDE_JAIL,
        PATH_ESCAPES,
        PATH_INVALID,
        PLACEHOLDER_UNKNOWN,
        GLOB_OVER_BUDGET,
        LOADER_ANCHOR_INVALID,
        ROOT_ANCHOR_NOT_PASSIVE,
        CODE_UNDECLARED,
        VERSION_INVALID,
        CHAIN_ENTRY_INVALID,
        LOADER_ID_DUPLICATE,
        OVER_BUDGET,
        MANAGER_AS_DATA,
    ];
}

use reasons::*;

/// §2.4's placeholder names; a descriptor's own `as` captures join them (§7.3).
pub const PLACEHOLDERS: &[&str] = &[
    "dir",
    "name",
    "stem",
    "program",
    "project",
    "project.leaf",
    "project.parent",
    "pak",
];

/// §2.4's ubiquitous runtime names, as name prefixes of a `.dll` (ignoring ASCII case): no loader
/// anchor and no chain entry (§7.4, §7.6). §2.4's own list, not the launcher's wider
/// [`super::descriptor::UBIQUITOUS_RUNTIME_PREFIXES`] for Pass B anchors: a loader's file holds a
/// name such a prefix covers (`xinput*` is a proxy, which [`HIJACK_NAMES`] names).
pub const UBIQUITOUS_RUNTIME_DLL_PREFIXES: &[&str] =
    &["steam_api", "vcruntime", "d3dcompiler_", "msvcp"];

/// §2.4's "must not itself be a program file", for §7.4's anchor and §7.6's entry: a name built
/// from the game's own program, or a name that runs.
pub const PROGRAM_PLACEHOLDERS: &[&str] = &["{program}", "{stem}", "{name}"];
pub const PROGRAM_EXTENSIONS: &[&str] = &[
    ".exe", ".com", ".bat", ".cmd", ".ps1", ".sh", ".x86", ".x86_64",
];

/// One refusal inside `mods`: the member it is about and its §7.9 reason.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModsFinding {
    pub field: String,
    pub reason: &'static str,
}

impl ModsFinding {
    /// As the launcher reports it: `mods-invalid{field, reason}`.
    pub fn error(&self) -> LauncherError {
        LauncherError::new(codes::MODS_INVALID)
            .with("field", self.field.clone())
            .with("reason", self.reason)
    }
}

// ---- The validated model. ----

/// Where `{source}` may resolve (§7.3): a closed set the launcher implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    /// The game folder that detection ran on.
    Game,
    /// The one profile a mod-manager adapter reports as that manager's ACTIVE profile for this
    /// game; never every profile it keeps.
    Manager,
    /// The folder the player chose in Astra ("Choose folder…").
    Picked,
}

impl SourceKind {
    pub const ALL: [SourceKind; 3] = [SourceKind::Game, SourceKind::Manager, SourceKind::Picked];

    pub fn code(self) -> &'static str {
        match self {
            SourceKind::Game => "game",
            SourceKind::Manager => "manager",
            SourceKind::Picked => "picked",
        }
    }

    fn from_code(s: &str) -> Option<SourceKind> {
        SourceKind::ALL.into_iter().find(|k| k.code() == s)
    }
}

/// A checked §7.3 path: `{source}`, then `/`-separated components, with placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModsPath(String);

impl ModsPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its components below `{source}` at evaluation (§7.3): every placeholder replaced by its
    /// game-relative value, the path split on `/` again, every component checked again with no
    /// placeholder left. `None`: the loader yields nothing (an unbound placeholder, a value that
    /// is empty, holds `..`, is absolute, a device name, …).
    pub fn substitute(&self, bindings: &Bindings) -> Option<Vec<String>> {
        substitute(&self.0, &bindings.0)
    }
}

/// The values a placing rule bound (§7.3): `project` → `Game/Pal`, `stem` → `Game`, a capture
/// → its game-relative path. A mods path's placeholder takes its value from here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bindings(BTreeMap<String, String>);

impl Bindings {
    pub fn new() -> Bindings {
        Bindings::default()
    }

    /// `self` with `{name}` bound to `value` (game-relative, `/`-separated).
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Bindings {
        self.0.insert(name.into(), value.into());
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

/// One lookup or probe of a loader's `require` (§7.4).
#[derive(Debug, Clone)]
pub(crate) enum MCheck {
    Listed {
        path: ModsPath,
        kind: Kind,
    },
    Child {
        path: ModsPath,
        kind: Kind,
    },
    Stat {
        path: ModsPath,
        kind: Kind,
    },
    Tail {
        path: ModsPath,
        equals: Vec<u8>,
        min_len: u64,
    },
    Any(Vec<MCheck>),
    All(Vec<MCheck>),
    None(Vec<MCheck>),
}

/// `version.key` (§7.4): closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionKey {
    ProductVersion,
    FileVersion,
}

impl VersionKey {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionKey::ProductVersion => "ProductVersion",
            VersionKey::FileVersion => "FileVersion",
        }
    }

    fn from_str(s: &str) -> Option<VersionKey> {
        [VersionKey::ProductVersion, VersionKey::FileVersion]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// One token of a §7.4 version template: a run of `min..=max` ASCII digits, or a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VTok {
    Run { min: u8, max: u8 },
    Lit(u8),
}

/// A version template (§2.5's shape, §7.4's reading): `N{a,b}` runs (1 ≤ a ≤ b ≤ 5) and the
/// literals `A-Za-z.+_-`, 1 to 32 tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(Vec<VTok>);

impl Template {
    pub fn parse(s: &str) -> Option<Template> {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'N'
                && let [b'{', a @ b'1'..=b'5', b',', z @ b'1'..=b'5', b'}', ..] = &b[i + 1..]
                && a <= z
            {
                out.push(VTok::Run {
                    min: a - b'0',
                    max: z - b'0',
                });
                i += 6;
                continue;
            }
            if b[i].is_ascii_alphabetic() || matches!(b[i], b'.' | b'+' | b'_' | b'-') {
                out.push(VTok::Lit(b[i]));
                i += 1;
                continue;
            }
            return None;
        }
        (1..=limits::TEMPLATE_TOKENS)
            .contains(&out.len())
            .then_some(Template(out))
    }

    /// The digit runs of the template's match at the start of `value`, whose next character is
    /// not a digit, with the match's length; `None` when it does not match. The first match in
    /// the order a backtracking regex with greedy runs finds (the checker's `read_version`).
    pub fn read(&self, value: &str) -> Option<(Vec<u32>, usize)> {
        let b = value.as_bytes();
        let mut failed = BTreeSet::new();
        let mut runs = Vec::new();
        let end = match_from(&self.0, b, 0, 0, false, &mut failed, &mut runs)?;
        Some((runs, end))
    }

    /// A `versions` operand read in the template's shape: the template's leading part cut
    /// after a digit run, matching the whole operand (the checker's `read_operand`).
    fn operand(&self, operand: &str) -> Option<Vec<u32>> {
        let b = operand.as_bytes();
        for (i, t) in self.0.iter().enumerate() {
            if !matches!(t, VTok::Run { .. }) {
                continue;
            }
            let mut failed = BTreeSet::new();
            let mut runs = Vec::new();
            if match_from(&self.0[..=i], b, 0, 0, true, &mut failed, &mut runs).is_some() {
                return Some(runs);
            }
        }
        None
    }
}

/// Matches `toks[t..]` against `s[pos..]`; `full`: to the end of `s`, else followed by no ASCII
/// digit. Greedy runs, backtracking, failed states memoised (so linear in tokens × positions).
fn match_from(
    toks: &[VTok],
    s: &[u8],
    t: usize,
    pos: usize,
    full: bool,
    failed: &mut BTreeSet<(usize, usize)>,
    runs: &mut Vec<u32>,
) -> Option<usize> {
    if t == toks.len() {
        let ok = if full {
            pos == s.len()
        } else {
            !s.get(pos).is_some_and(u8::is_ascii_digit)
        };
        return ok.then_some(pos);
    }
    if failed.contains(&(t, pos)) {
        return None;
    }
    match toks[t] {
        VTok::Lit(c) => {
            if s.get(pos) == Some(&c)
                && let Some(e) = match_from(toks, s, t + 1, pos + 1, full, failed, runs)
            {
                return Some(e);
            }
        }
        VTok::Run { min, max } => {
            let avail = s[pos.min(s.len())..]
                .iter()
                .take(usize::from(max))
                .take_while(|c| c.is_ascii_digit())
                .count();
            for n in (usize::from(min)..=avail).rev() {
                let v = s[pos..pos + n]
                    .iter()
                    .fold(0u32, |a, d| a * 10 + u32::from(d - b'0'));
                runs.push(v);
                if let Some(e) = match_from(toks, s, t + 1, pos + n, full, failed, runs) {
                    return Some(e);
                }
                runs.pop();
            }
        }
    }
    failed.insert((t, pos));
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

/// `versions` (§7.4): one or two comparators joined by `, `, each with an operand read in its
/// template's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versions {
    text: String,
    comparators: Vec<(Op, Vec<u32>)>,
}

impl Versions {
    /// `versions` as written (`>=5.4, <5.5`): what the player is told Astra needs.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether a version whose runs are `have` satisfies every comparator: numeric, run by
    /// run, over the operand's runs only.
    pub fn holds(&self, have: &[u32]) -> bool {
        self.comparators.iter().all(|(op, want)| {
            let got = &have[..want.len().min(have.len())];
            match op {
                Op::Eq => got == want.as_slice(),
                Op::Lt => got < want.as_slice(),
                Op::Le => got <= want.as_slice(),
                Op::Gt => got > want.as_slice(),
                Op::Ge => got >= want.as_slice(),
            }
        })
    }
}

/// `versions`' comparators as text (the checker's `_VERSIONS` and `version_comparators`).
fn comparators(text: &str) -> Option<Vec<(Op, &str)>> {
    let parts: Vec<&str> = text.split(", ").collect();
    if parts.len() > 2 {
        return None;
    }
    parts
        .into_iter()
        .map(|p| {
            let (op, rest) = [
                ("<=", Op::Le),
                (">=", Op::Ge),
                ("=", Op::Eq),
                ("<", Op::Lt),
                (">", Op::Gt),
            ]
            .into_iter()
            .find_map(|(s, op)| p.strip_prefix(s).map(|r| (op, r)))?;
            let ok = (1..=limits::OPERAND_CHARS).contains(&rest.chars().count())
                && rest
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'_' | b'-'));
            ok.then_some((op, rest))
        })
        .collect()
}

/// A loader's `version` (§7.4): a bounded probe of one PE file's version resource.
#[derive(Debug, Clone)]
pub struct LoaderVersion {
    pub(crate) pe: ModsPath,
    pub(crate) keys: Vec<VersionKey>,
    pub(crate) template: Template,
    pub(crate) versions: Option<Versions>,
}

impl LoaderVersion {
    /// The PE file whose version resource is read.
    pub fn pe(&self) -> &ModsPath {
        &self.pe
    }

    /// The keys tried, in order.
    pub fn keys(&self) -> &[VersionKey] {
        &self.keys
    }

    pub fn template(&self) -> &Template {
        &self.template
    }

    pub fn versions(&self) -> Option<&Versions> {
        self.versions.as_ref()
    }
}

/// A `file-glob` glob (§7.5): one file name with one or two `*`, matched ignoring ASCII case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob(String);

impl Glob {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, name: &str) -> bool {
        star_match(&self.0.to_ascii_lowercase(), &name.to_ascii_lowercase())
    }
}

/// `pattern` (its only wildcard `*`, any run of characters) against all of `name`.
fn star_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if name.len() < first.len() + last.len() || !name.starts_with(first) || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(at) => rest = &rest[at + mid.len()..],
            None => return false,
        }
    }
    true
}

/// `enabled` (§7.5): a closed set of formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enabled {
    /// A mod is a folder directly in the root, enabled when it is there.
    DirPerMod,
    /// A mod is a folder directly in the root, enabled when the first line of `list` naming it
    /// says `1` (`name-colon-flag`), or it holds a file named `marker`.
    EnableList {
        list: ModsPath,
        marker: Option<String>,
    },
    /// A mod is a file directly in the root whose name matches the glob, enabled when there.
    FileGlob(Glob),
}

/// `tell` (§7.6): closed; how the player's loader is told where Astra's folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tell {
    /// `UE4SS_MODS_PATHS`, a `;`-separated list of extra Mods folders UE4SS reads: Astra's
    /// folder is appended, an existing value kept.
    Ue4ssModsPaths,
}

impl Tell {
    pub fn code(self) -> &'static str {
        match self {
            Tell::Ue4ssModsPaths => "ue4ss-mods-paths",
        }
    }

    /// The environment variable the channel sets.
    pub fn variable(self) -> &'static str {
        match self {
            Tell::Ue4ssModsPaths => "UE4SS_MODS_PATHS",
        }
    }

    fn from_code(s: &str) -> Option<Tell> {
        (s == "ue4ss-mods-paths").then_some(Tell::Ue4ssModsPaths)
    }
}

/// `coexist` (§7.6): a closed set of strategies, in order of preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coexist {
    /// Astra's runner starts first and hands control to the player's loader at `entry` (one of
    /// the loader's own top-level file lookups, a `.dll`), loaded in place.
    Chain { entry: ModsPath, tell: Option<Tell> },
    /// The player's loader starts as without Astra; Astra's runner runs beside it.
    SideBySide { tell: Option<Tell> },
    /// The player's mod manager starts the game; Astra rides along through its adapter.
    ViaManager,
}

impl Coexist {
    pub fn code(&self) -> &'static str {
        match self {
            Coexist::Chain { .. } => "chain",
            Coexist::SideBySide { .. } => "side-by-side",
            Coexist::ViaManager => "via-manager",
        }
    }
}

/// One loader of a valid `mods` (§7.2).
#[derive(Debug, Clone)]
pub struct Loader {
    pub(crate) id: String,
    pub(crate) name: String,
    /// The codes it applies to: its `codes`, else every code of the descriptor.
    pub(crate) codes: Vec<EngineCode>,
    pub(crate) sources: Vec<SourceKind>,
    pub(crate) require: Vec<MCheck>,
    pub(crate) version: Option<LoaderVersion>,
    pub(crate) root: ModsPath,
    pub(crate) enabled: Vec<Enabled>,
    pub(crate) coexist: Vec<Coexist>,
}

impl Loader {
    /// How a foundation names it, how the player's choice is remembered, how the player is told.
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where its install may be.
    pub fn sources(&self) -> &[SourceKind] {
        &self.sources
    }

    /// Whether it applies to a game this descriptor placed under `code`: one of its `codes`,
    /// or, with none, of the descriptor's own.
    pub fn applies_to(&self, code: &EngineCode) -> bool {
        self.codes.contains(code)
    }

    pub fn root(&self) -> &ModsPath {
        &self.root
    }

    pub fn version(&self) -> Option<&LoaderVersion> {
        self.version.as_ref()
    }

    pub fn enabled(&self) -> &[Enabled] {
        &self.enabled
    }

    pub fn coexist(&self) -> &[Coexist] {
        &self.coexist
    }

    /// §7.4, exactly: no `chain`, no `via-manager`, and no strategy with a `tell`.
    pub fn is_passive(&self) -> bool {
        self.coexist.iter().all(|s| match s {
            Coexist::Chain { .. } | Coexist::ViaManager => false,
            Coexist::SideBySide { tell } => tell.is_none(),
        })
    }
}

/// A valid `mods` member: 1 to 4 loaders, tried in order.
#[derive(Debug, Clone)]
pub struct Mods {
    pub(crate) loaders: Vec<Loader>,
}

impl Mods {
    pub fn loaders(&self) -> &[Loader] {
        &self.loaders
    }

    pub fn loader(&self, id: &str) -> Option<&Loader> {
        self.loaders.iter().find(|l| l.id == id)
    }
}

// ---- What the descriptor's own detection names (§7.4, §7.3). ----

/// The descriptor's detection, as `mods` may not repeat it: its codes, its `as` captures, and
/// the last component of every name its rules look up (`listed`, `child`, `stat`, `tail`, a
/// bootstrap's `listed`, an `also_if` folder), lower case, each placeholder read as `*`; one that
/// is nothing but placeholders and `*` is the program, which the program rule covers.
pub(crate) struct Own {
    codes: BTreeSet<String>,
    captures: BTreeSet<String>,
    names: Vec<String>,
}

fn last_pattern(pieces: &[Piece]) -> Option<String> {
    let mut s = String::new();
    for p in pieces {
        match p {
            Piece::Lit(l) => s.push_str(&l.to_ascii_lowercase()),
            Piece::Var(_) | Piece::Star => s.push('*'),
        }
    }
    s.chars().any(|c| c != '*').then_some(s)
}

fn path_pattern(t: &PathT) -> Option<String> {
    last_pattern(t.segs.last()?)
}

impl Own {
    pub(crate) fn of(m: &Model) -> Own {
        let mut names = Vec::new();
        let mut captures = BTreeSet::new();
        for r in &m.rules {
            for c in r.checks() {
                match c {
                    Check::Listed { path, capture, .. }
                    | Check::Child { path, capture, .. }
                    | Check::Stat { path, capture, .. } => {
                        names.extend(path_pattern(path));
                        captures.extend(capture.clone());
                    }
                    Check::Tail { path, .. } => names.extend(path_pattern(path)),
                    Check::Name(_) | Check::Any(_) | Check::All(_) | Check::None(_) => {}
                }
            }
            if let Some(b) = &r.bootstrap
                && let Some(l) = &b.listed
            {
                names.extend(path_pattern(l));
            }
            if let Some(p) = &r.programs
                && let Some(a) = &p.also_if
            {
                names.extend(last_pattern(a));
            }
        }
        Own {
            codes: m.codes.iter().map(|(c, _)| c.as_str().to_owned()).collect(),
            captures,
            names,
        }
    }

    /// Whether the last component of `path` is a name this descriptor's detection looks up,
    /// ignoring ASCII case.
    fn names(&self, path: &str) -> bool {
        let last = last_component(path).to_ascii_lowercase();
        self.names.iter().any(|p| star_match(p, &last))
    }
}

// ---- §7.3: paths, components, placeholders. ----

/// Each `{name}` in `s` (`[a-z][a-z0-9_]*`, then optionally `.` and `[a-z]+`): its byte range.
fn placeholders(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{'
            && let Some(end) = placeholder_at(b, i)
        {
            out.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn placeholder_at(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if !b.get(j)?.is_ascii_lowercase() {
        return None;
    }
    j += 1;
    while b
        .get(j)
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
    {
        j += 1;
    }
    if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(u8::is_ascii_lowercase) {
        let mut k = j + 1;
        while b.get(k).is_some_and(u8::is_ascii_lowercase) {
            k += 1;
        }
        if b.get(k) == Some(&b'}') {
            return Some(k + 1);
        }
    }
    (b.get(j) == Some(&b'}')).then_some(j + 1)
}

/// `s` with every placeholder replaced by `with`.
fn replace_placeholders(s: &str, with: &str) -> String {
    let mut out = String::new();
    let mut at = 0;
    for (a, b) in placeholders(s) {
        out.push_str(&s[at..a]);
        out.push_str(with);
        at = b;
    }
    out.push_str(&s[at..]);
    out
}

/// The placeholder names `s` holds.
fn placeholder_names(s: &str) -> Vec<&str> {
    placeholders(s)
        .into_iter()
        .map(|(a, b)| &s[a + 1..b - 1])
        .collect()
}

fn last_component(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `stem` (upper case) is a Windows device name.
fn is_device(stem: &str) -> bool {
    let numbered = |p: &str| {
        stem.strip_prefix(p).is_some_and(|n| {
            let mut c = n.chars();
            matches!(
                (c.next(), c.next()),
                (Some('0'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}'), None)
            )
        })
    };
    matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || numbered("COM")
        || numbered("LPT")
}

/// §7.3's component rules (`paths::check_component`'s, plus C1 controls and `~` before a
/// digit): `None`, [`PATH_ESCAPES`] or [`PATH_INVALID`]. `placeholders`: a placeholder may stand
/// in it (before substitution), else any brace is refused.
fn component_problem(c: &str, placeholders: bool) -> Option<&'static str> {
    if c == ".." {
        return Some(PATH_ESCAPES);
    }
    if c.is_empty() || c == "." || c.len() > limits::COMPONENT_BYTES {
        return Some(PATH_INVALID);
    }
    if c.chars().any(|ch| {
        let u = ch as u32;
        u < 0x20 || (0x7f..=0x9f).contains(&u) || "\\:*?<>\"|/".contains(ch)
    }) {
        return Some(PATH_INVALID);
    }
    let bare = if placeholders {
        replace_placeholders(c, "")
    } else {
        c.to_owned()
    };
    if bare.contains(['{', '}']) || (placeholders && c.contains("{source}")) {
        return Some(PATH_INVALID);
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Some(PATH_INVALID);
    }
    // An 8.3 short name (XINPUT~1.DLL, PROGRA~1): a direct lookup resolves it to some other
    // file, a proxy under a name no list knows.
    if c.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'~' && w[1].is_ascii_digit())
    {
        return Some(PATH_INVALID);
    }
    let stem = c.split('.').next().unwrap_or("").trim_end_matches(' ');
    if is_device(&stem.to_uppercase()) {
        return Some(PATH_INVALID);
    }
    None
}

/// `None` for a §7.3 path, else its §7.9 reason.
pub fn path_reason(p: &str) -> Option<&'static str> {
    if p.chars().count() > limits::PATH_CHARS {
        return Some(SHAPE);
    }
    if p == "{source}" {
        return None;
    }
    let Some(rest) = p.strip_prefix("{source}/") else {
        return Some(PATH_OUTSIDE_JAIL);
    };
    let found: Vec<_> = rest
        .split('/')
        .map(|c| component_problem(c, true))
        .collect();
    if found.contains(&Some(PATH_ESCAPES)) {
        return Some(PATH_ESCAPES);
    }
    found.into_iter().flatten().next()
}

fn value_path_reason(v: &Value) -> Option<&'static str> {
    match v {
        Value::String(s) => path_reason(s),
        _ => Some(SHAPE),
    }
}

/// §7.3 at evaluation ([`ModsPath::substitute`]): `path`'s components below `{source}` with
/// every placeholder replaced by its value in `values`, each checked again; `None` when the path
/// is no §7.3 path, a placeholder has no value, or a component is refused.
pub fn substitute(path: &str, values: &BTreeMap<String, String>) -> Option<Vec<String>> {
    if path_reason(path).is_some() {
        return None;
    }
    let rest = &path["{source}".len()..];
    let mut out = String::new();
    let mut at = 0;
    for (a, b) in placeholders(rest) {
        out.push_str(&rest[at..a]);
        out.push_str(values.get(&rest[a + 1..b - 1])?);
        at = b;
    }
    out.push_str(&rest[at..]);
    if out.is_empty() {
        return Some(Vec::new());
    }
    let comps: Vec<String> = out.split('/').skip(1).map(str::to_owned).collect();
    comps
        .iter()
        .all(|c| component_problem(c, false).is_none())
        .then_some(comps)
}

/// §7.5's `marker`: one §7.3 component, no placeholder.
fn file_name_reason(v: &Value) -> Option<&'static str> {
    match v {
        Value::String(s) => component_problem(s, false).map(|_| PATH_INVALID),
        _ => Some(SHAPE),
    }
}

/// §7.5: one file name, one or two `*`, no other wildcard or separator.
fn glob_reason(v: &Value) -> Option<&'static str> {
    let ok = v.as_str().is_some_and(|g| {
        (1..=limits::GLOB_CHARS).contains(&g.chars().count())
            && (1..=limits::GLOB_STARS).contains(&g.matches('*').count())
            && !g.contains("**")
            && !g.chars().any(|ch| {
                let u = ch as u32;
                u < 0x20 || (0x7f..=0x9f).contains(&u) || "/\\:?[]{}<>\"|".contains(ch)
            })
    });
    (!ok).then_some(GLOB_OVER_BUDGET)
}

// ---- §7.4, §7.6: anchors and entries. ----

fn anchor_name(path: &str) -> String {
    replace_placeholders(last_component(path), "")
}

/// `c` as the checker's case-insensitive patterns compare it with an ASCII letter or digit
/// (Python's `re.IGNORECASE`): ASCII lower case, and the four letters outside ASCII it folds onto
/// one (`İ` and `ı` onto `i`, `ſ` onto `s`, the Kelvin sign onto `k`). Every other character
/// matches no ASCII one, so it is kept as it is.
fn ci_char(c: char) -> char {
    match c {
        '\u{130}' | '\u{131}' => 'i',
        '\u{17f}' => 's',
        '\u{212a}' => 'k',
        _ => c.to_ascii_lowercase(),
    }
}

fn ci_fold(s: &str) -> String {
    s.chars().map(ci_char).collect()
}

/// A name mod loaders hijack-load by ([`HIJACK_NAMES`], ignoring ASCII case) or a §2.4
/// ubiquitous runtime: one of [`UBIQUITOUS_RUNTIME_DLL_PREFIXES`], then anything but a line
/// break, then `.dll` — the checker's `^steam_api.*\.dll$`, case folded as its `re.I` folds, and
/// with its `$`, which also stands before one final line break.
fn proxy_or_runtime(path: &str) -> bool {
    let name = anchor_name(path);
    if HIJACK_NAMES.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
        return true;
    }
    let folded = ci_fold(&name);
    let body = folded.strip_suffix('\n').unwrap_or(&folded);
    body.strip_suffix(".dll").is_some_and(|stem| {
        !stem.contains('\n')
            && UBIQUITOUS_RUNTIME_DLL_PREFIXES
                .iter()
                .any(|p| stem.starts_with(p))
    })
}

fn program_shaped(path: &str) -> bool {
    let last = last_component(path);
    let lower = last.to_lowercase();
    PROGRAM_PLACEHOLDERS.iter().any(|p| last.contains(p))
        || PROGRAM_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

const LOOKUPS: [&str; 3] = ["listed", "child", "stat"];
const COMBINATORS: [&str; 3] = ["any", "all", "none"];

/// The top-level `listed`/`child`/`stat` paths of `kind` in a loader's `require` (never one
/// inside `any`/`all`/`none`).
fn top_lookups<'v>(loader: &'v Map<String, Value>, kind: &str) -> Vec<&'v str> {
    let Some(Value::Array(req)) = loader.get("require") else {
        return Vec::new();
    };
    req.iter()
        .filter_map(Value::as_object)
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some(kind))
        .filter_map(|e| {
            let key = LOOKUPS.iter().find(|k| e.contains_key(**k))?;
            e[*key].as_str()
        })
        .collect()
}

/// §7.4: top-level `file` lookups whose last component holds a literal name that is not a proxy,
/// a ubiquitous runtime, the game's own program or a name this descriptor's detection looks up.
fn file_anchors<'v>(loader: &'v Map<String, Value>, own: &Own) -> Vec<&'v str> {
    top_lookups(loader, "file")
        .into_iter()
        .filter(|p| {
            !anchor_name(p).is_empty()
                && !proxy_or_runtime(p)
                && !program_shaped(p)
                && !own.names(p)
        })
        .collect()
}

/// §7.4: anchored on its root — no file anchor, and a top-level `child` or `stat` of kind `dir`
/// of exactly its own `root` (never `listed`).
fn root_anchored(loader: &Map<String, Value>, own: &Own) -> bool {
    if !file_anchors(loader, own).is_empty() {
        return false;
    }
    let Some(Value::Array(req)) = loader.get("require") else {
        return false;
    };
    req.iter().filter_map(Value::as_object).any(|e| {
        e.get("kind").and_then(Value::as_str) == Some("dir")
            && ["child", "stat"].iter().any(|k| {
                e.get(*k).and_then(Value::as_str).is_some_and(|p| {
                    loader.get("root").and_then(Value::as_str) == Some(p)
                        && !anchor_name(p).is_empty()
                        && !own.names(p)
                })
            })
    })
}

/// §7.4, exactly, read off the table: `coexist` a list of strategies none of which is `chain`
/// or `via-manager` and none of which has a `tell`.
fn raw_passive(loader: &Map<String, Value>) -> bool {
    let Some(Value::Array(co)) = loader.get("coexist") else {
        return false;
    };
    co.iter().all(|s| {
        s.as_object().is_some_and(|s| {
            !matches!(
                s.get("strategy").and_then(Value::as_str),
                Some("chain" | "via-manager")
            ) && !s.contains_key("tell")
        })
    })
}

// ---- The walk of a `require`. ----

/// One entry of a `require` walk: a lookup, a probe, or a combinator past the depth bound.
struct Walked<'v> {
    entry: &'v Map<String, Value>,
    depth: usize,
    field: String,
}

/// Every lookup or probe of a `require` with its combinator depth, document order, no recursion
/// (the checker's `_require_walk`): a combinator whose entries would sit past
/// [`limits::DEPTH`] + 1 is yielded itself, one level deeper, and never descended into. Entries
/// that are not tables, and combinators holding anything but their one non-empty list, go to
/// `bad` (`shape`, `unknown`).
fn require_walk<'v>(
    req: &'v Value,
    field: &str,
    bad: &mut Vec<(String, &'static str)>,
) -> Vec<Walked<'v>> {
    let limit = limits::DEPTH + 1;
    let mut out = Vec::new();
    let Value::Array(top) = req else {
        return out;
    };
    let mut stack: Vec<(&Value, usize, String)> = top
        .iter()
        .enumerate()
        .rev()
        .map(|(i, e)| (e, 0, format!("{field}[{i}]")))
        .collect();
    while let Some((e, d, f)) = stack.pop() {
        let Value::Object(e) = e else {
            bad.push((f, SHAPE));
            continue;
        };
        let Some(key) = COMBINATORS.iter().find(|k| e.contains_key(**k)) else {
            out.push(Walked {
                entry: e,
                depth: d,
                field: f,
            });
            continue;
        };
        if e.len() != 1 {
            bad.push((f.clone(), UNKNOWN));
        }
        if d + 1 > limit {
            out.push(Walked {
                entry: e,
                depth: d + 1,
                field: f,
            });
            continue;
        }
        match &e[*key] {
            Value::Array(list) if !list.is_empty() => {
                for (i, x) in list.iter().enumerate().rev() {
                    stack.push((x, d + 1, format!("{f}.{key}[{i}]")));
                }
            }
            _ => bad.push((format!("{f}.{key}"), SHAPE)),
        }
    }
    out
}

/// How deep tables and arrays nest in `v` (a container counts 1), with no recursion.
fn nesting(v: &Value) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(v, 1)];
    while let Some((v, d)) = stack.pop() {
        match v {
            Value::Object(o) => {
                deepest = deepest.max(d);
                stack.extend(o.values().map(|x| (x, d + 1)));
            }
            Value::Array(a) => {
                deepest = deepest.max(d);
                stack.extend(a.iter().map(|x| (x, d + 1)));
            }
            _ => {}
        }
    }
    deepest
}

/// Whether `s` names a mod manager (§7.7): `r2modman`, `Thunderstore`, `Mod Organizer` (`mod`,
/// one optional space, `_` or `-`, `organiser` or `organizer`) and `Overwolf` anywhere, `Gale`,
/// `MO2`, `Vortex` and `TMM` as words — not beside an ASCII letter or digit. Case is ignored as
/// the checker's `re.I` ignores it ([`ci_char`]).
pub fn names_a_manager(s: &str) -> bool {
    let c: Vec<char> = s.chars().map(ci_char).collect();
    let at = |i: usize, w: &str| w.chars().enumerate().all(|(k, x)| c.get(i + k) == Some(&x));
    let word = |i: Option<usize>| {
        i.and_then(|i| c.get(i))
            .is_some_and(|x| x.is_ascii_lowercase() || x.is_ascii_digit())
    };
    (0..c.len()).any(|i| {
        ["r2modman", "thunderstore", "overwolf"]
            .iter()
            .any(|w| at(i, w))
            || (at(i, "mod") && {
                let j = i + 3 + usize::from(matches!(c.get(i + 3), Some(' ' | '_' | '-')));
                at(j, "organi") && matches!(c.get(j + 6), Some('s' | 'z')) && at(j + 7, "er")
            })
            || ["gale", "mo2", "vortex", "tmm"]
                .iter()
                .any(|w| at(i, w) && !word(i.checked_sub(1)) && !word(Some(i + w.chars().count())))
    })
}

/// Every string in `v` (keys included) with the member it is in, no recursion.
fn strings<'v>(v: &'v Value, field: &str) -> Vec<(String, &'v str)> {
    let mut out = Vec::new();
    let mut stack = vec![(v, field.to_owned())];
    while let Some((v, f)) = stack.pop() {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    let kf = format!("{f}.{k}");
                    out.push((kf.clone(), k.as_str()));
                    stack.push((x, kf));
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    stack.push((x, format!("{f}[{i}]")));
                }
            }
            Value::String(s) => out.push((f, s.as_str())),
            _ => {}
        }
    }
    out
}

fn list_len(v: &Value, lo: usize, hi: usize) -> bool {
    matches!(v, Value::Array(a) if (lo..=hi).contains(&a.len()))
}

// ---- Validation. ----

const LOADER_KEYS: [&str; 9] = [
    "id", "name", "codes", "source", "require", "version", "root", "enabled", "coexist",
];
const LOADER_REQUIRED: [&str; 7] = [
    "id", "name", "source", "require", "root", "enabled", "coexist",
];

/// Collects findings, once each.
struct Out(Vec<ModsFinding>);

impl Out {
    fn add(&mut self, field: impl Into<String>, reason: &'static str) {
        let f = ModsFinding {
            field: field.into(),
            reason,
        };
        if !self.0.contains(&f) {
            self.0.push(f);
        }
    }
}

/// The checks of a `mods` member that is a table, against the descriptor it rides on (`model`):
/// the valid model, or every §7.9 finding (see the module doc).
pub(crate) fn validate(mods: &Map<String, Value>, model: &Model) -> Result<Mods, Vec<ModsFinding>> {
    let own = Own::of(model);
    let mut out = Out(Vec::new());
    let whole = Value::Object(mods.clone());
    if nesting(&whole) > limits::NESTING {
        out.add("mods", OVER_BUDGET);
    }
    for k in mods.keys().filter(|k| *k != "loader") {
        out.add(format!("mods.{k}"), UNKNOWN);
    }
    let loaders = mods.get("loader");
    if !loaders.is_some_and(|l| list_len(l, 1, limits::LOADERS)) {
        out.add("mods.loader", SHAPE);
    }
    let loaders: Vec<(usize, &Map<String, Value>)> = match loaders {
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l {
                Value::Object(l) => Some((i, l)),
                _ => {
                    out.add(format!("mods.loader[{i}]"), SHAPE);
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    let (mut lookups, mut probes) = (0usize, 0usize);
    for (i, l) in &loaders {
        let (lk, pr) = check_loader(&format!("mods.loader[{i}]"), l, &own, &mut out);
        lookups += lk;
        probes += pr;
    }
    if lookups > limits::LOOKUPS || probes > limits::PROBES {
        out.add("mods", OVER_BUDGET);
    }
    // §7.2: ids unique within the descriptor (the checker's: as text, a missing one `None`).
    let mut ids = BTreeSet::new();
    for (i, l) in &loaders {
        let id = match l.get("id") {
            None => "None".to_owned(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        if !ids.insert(id) {
            out.add(format!("mods.loader[{i}].id"), LOADER_ID_DUPLICATE);
        }
    }
    // §7.7: no manager named anywhere, as a member or a value.
    for (f, s) in strings(&whole, "mods") {
        if names_a_manager(s) {
            out.add(f, MANAGER_AS_DATA);
        }
    }
    if !out.0.is_empty() {
        return Err(out.0);
    }
    let codes: Vec<EngineCode> = model.codes.iter().map(|(c, _)| c.clone()).collect();
    let built: Option<Vec<Loader>> = loaders
        .iter()
        .map(|(_, l)| build_loader(l, &codes))
        .collect();
    built.map(|loaders| Mods { loaders }).ok_or_else(|| {
        // Every shape `build_loader` reads was checked above: unreachable, and never a crash.
        vec![ModsFinding {
            field: "mods".to_owned(),
            reason: SHAPE,
        }]
    })
}

/// One loader's own checks; its `(lookups, probes)` for §7.8's per-member bound.
fn check_loader(lf: &str, l: &Map<String, Value>, own: &Own, out: &mut Out) -> (usize, usize) {
    let f = |k: &str| format!("{lf}.{k}");
    for k in l.keys().filter(|k| !LOADER_KEYS.contains(&k.as_str())) {
        out.add(f(k), UNKNOWN);
    }
    for k in LOADER_REQUIRED.iter().filter(|k| !l.contains_key(**k)) {
        out.add(f(k), SHAPE);
    }
    if let Some(id) = l.get("id")
        && !id.as_str().is_some_and(is_valid_engine_code)
    {
        out.add(f("id"), SHAPE);
    }
    if let Some(name) = l.get("name")
        && !name.as_str().is_some_and(|n| {
            (1..=super::descriptor::limits::NAME_CHARS).contains(&n.chars().count())
                && n.trim() == n
                && display_chars_ok(n)
        })
    {
        out.add(f("name"), SHAPE);
    }
    if let Some(c) = l.get("codes")
        && !(list_len(c, 1, limits::CODES)
            && c.as_array().is_some_and(|a| {
                a.iter()
                    .all(|x| x.as_str().is_some_and(is_valid_engine_code))
            }))
    {
        out.add(f("codes"), SHAPE);
    }
    if let Some(s) = l.get("source") {
        if !list_len(s, 1, limits::SOURCES) {
            out.add(f("source"), SHAPE);
        } else if s.as_array().is_some_and(|a| {
            a.iter()
                .any(|x| x.as_str().and_then(SourceKind::from_code).is_none())
        }) {
            out.add(f("source"), UNKNOWN);
        }
    }
    if let Some(r) = l.get("require")
        && !list_len(r, 1, limits::REQUIRE)
    {
        out.add(f("require"), SHAPE);
    }
    let mut bad = Vec::new();
    let walked = l
        .get("require")
        .map(|r| require_walk(r, &f("require"), &mut bad))
        .unwrap_or_default();
    for (bf, reason) in bad {
        out.add(bf, reason);
    }
    let (mut lookups, mut probes) = (0, 0);
    for w in &walked {
        if w.depth > limits::DEPTH {
            out.add(w.field.clone(), OVER_BUDGET);
        }
        if LOOKUPS.iter().any(|k| w.entry.contains_key(*k)) {
            lookups += 1;
        } else if w.entry.contains_key("tail") {
            probes += 1;
        }
        if w.depth > limits::DEPTH {
            continue;
        }
        let Some(key) = LOOKUPS
            .iter()
            .chain(["tail"].iter())
            .find(|k| w.entry.contains_key(**k))
        else {
            out.add(w.field.clone(), UNKNOWN);
            continue;
        };
        let allowed: &[&str] = if *key == "tail" {
            &["tail", "bytes", "equals", "min_len"]
        } else {
            &[key, "kind"]
        };
        for k in w.entry.keys().filter(|k| !allowed.contains(&k.as_str())) {
            out.add(format!("{}.{k}", w.field), UNKNOWN);
        }
        if *key == "tail" {
            if tail_of(w.entry).is_none() {
                out.add(w.field.clone(), SHAPE);
            }
        } else {
            match w.entry.get("kind") {
                None => out.add(format!("{}.kind", w.field), SHAPE),
                Some(k) if !matches!(k.as_str(), Some("file" | "dir")) => {
                    out.add(format!("{}.kind", w.field), UNKNOWN);
                }
                Some(_) => {}
            }
        }
    }
    if let Some(v) = l.get("version") {
        probes += usize::from(v.is_object());
        match v {
            Value::Object(v) => {
                for k in v
                    .keys()
                    .filter(|k| !["pe", "key", "template", "versions"].contains(&k.as_str()))
                {
                    out.add(format!("{lf}.version.{k}"), UNKNOWN);
                }
                for k in ["pe", "key", "template"]
                    .iter()
                    .filter(|k| !v.contains_key(**k))
                {
                    out.add(format!("{lf}.version.{k}"), SHAPE);
                }
                if let Some(key) = v.get("key") {
                    if !list_len(key, 1, limits::KEYS) {
                        out.add(format!("{lf}.version.key"), SHAPE);
                    } else if key.as_array().is_some_and(|a| {
                        a.iter()
                            .any(|x| x.as_str().and_then(VersionKey::from_str).is_none())
                    }) {
                        out.add(format!("{lf}.version.key"), UNKNOWN);
                    }
                }
                let template = v
                    .get("template")
                    .map(|t| t.as_str().and_then(Template::parse));
                if let Some(None) = template {
                    out.add(format!("{lf}.version.template"), VERSION_INVALID);
                }
                let versions = v.get("versions").map(|t| t.as_str().and_then(comparators));
                if let Some(None) = versions {
                    out.add(format!("{lf}.version.versions"), VERSION_INVALID);
                }
                // §7.4: every operand reads in its template's shape.
                if let (Some(Some(t)), Some(Some(c))) = (template, versions)
                    && c.iter().any(|(_, o)| t.operand(o).is_none())
                {
                    out.add(format!("{lf}.version.versions"), VERSION_INVALID);
                }
            }
            _ => out.add(format!("{lf}.version"), SHAPE),
        }
    }
    if let Some(en) = l.get("enabled")
        && !list_len(en, 1, limits::ENABLED)
    {
        out.add(f("enabled"), SHAPE);
    }
    if let Some(Value::Array(en)) = l.get("enabled") {
        for (j, fm) in en.iter().enumerate() {
            let ff = format!("{lf}.enabled[{j}]");
            let Value::Object(fm) = fm else {
                out.add(ff, SHAPE);
                continue;
            };
            let (allowed, required): (&[&str], &[&str]) =
                match fm.get("format").and_then(Value::as_str) {
                    Some("dir-per-mod") => (&["format"], &["format"]),
                    Some("enable-list") => {
                        probes += 1 + usize::from(fm.contains_key("marker"));
                        (
                            &["format", "list", "line", "marker"],
                            &["format", "list", "line"],
                        )
                    }
                    Some("file-glob") => (&["format", "glob"], &["format", "glob"]),
                    _ => {
                        out.add(format!("{ff}.format"), UNKNOWN);
                        continue;
                    }
                };
            for k in fm.keys().filter(|k| !allowed.contains(&k.as_str())) {
                out.add(format!("{ff}.{k}"), UNKNOWN);
            }
            for k in required.iter().filter(|k| !fm.contains_key(**k)) {
                out.add(format!("{ff}.{k}"), SHAPE);
            }
            if let Some(line) = fm.get("line")
                && line.as_str() != Some("name-colon-flag")
            {
                out.add(format!("{ff}.line"), UNKNOWN);
            }
            if let Some(r) = fm.get("marker").and_then(file_name_reason) {
                out.add(format!("{ff}.marker"), r);
            }
            if let Some(r) = fm.get("glob").and_then(glob_reason) {
                out.add(format!("{ff}.glob"), r);
            }
        }
    }
    if let Some(co) = l.get("coexist")
        && !list_len(co, 0, limits::COEXIST)
    {
        out.add(f("coexist"), SHAPE);
    }
    if let Some(Value::Array(co)) = l.get("coexist") {
        for (j, s) in co.iter().enumerate() {
            let sf = format!("{lf}.coexist[{j}]");
            let Value::Object(s) = s else {
                out.add(sf, SHAPE);
                continue;
            };
            let (allowed, required): (&[&str], &[&str]) =
                match s.get("strategy").and_then(Value::as_str) {
                    Some("chain") => (&["strategy", "entry", "tell"], &["strategy", "entry"]),
                    Some("side-by-side") => (&["strategy", "tell"], &["strategy"]),
                    Some("via-manager") => (&["strategy"], &["strategy"]),
                    _ => {
                        out.add(format!("{sf}.strategy"), UNKNOWN);
                        continue;
                    }
                };
            for k in s.keys().filter(|k| !allowed.contains(&k.as_str())) {
                out.add(format!("{sf}.{k}"), UNKNOWN);
            }
            for k in required.iter().filter(|k| !s.contains_key(**k)) {
                out.add(format!("{sf}.{k}"), SHAPE);
            }
            if let Some(t) = s.get("tell")
                && t.as_str().and_then(Tell::from_code).is_none()
            {
                out.add(format!("{sf}.tell"), UNKNOWN);
            }
        }
    }
    // §7.3: every path a §7.3 path, its placeholders §2.4's or this descriptor's captures.
    for (pf, p) in loader_paths(lf, l, &walked) {
        if let Some(r) = value_path_reason(p) {
            out.add(pf.clone(), r);
        }
        if let Some(s) = p.as_str()
            && placeholder_names(s)
                .into_iter()
                .any(|n| n != "source" && !PLACEHOLDERS.contains(&n) && !own.captures.contains(n))
        {
            out.add(pf, PLACEHOLDER_UNKNOWN);
        }
    }
    // §7.4: anchored, by a file or on its own root; a root-anchored loader passive.
    let rooted = root_anchored(l, own);
    if file_anchors(l, own).is_empty() && !rooted {
        out.add(f("require"), LOADER_ANCHOR_INVALID);
    }
    if rooted && !raw_passive(l) {
        out.add(f("coexist"), ROOT_ANCHOR_NOT_PASSIVE);
    }
    // §7.6: a chain entry is a `.dll` its recognition found, and none of §7.4's four.
    let files: BTreeSet<&str> = top_lookups(l, "file").into_iter().collect();
    if let Some(Value::Array(co)) = l.get("coexist") {
        for (j, s) in co.iter().enumerate() {
            let Some(s) = s.as_object() else { continue };
            if s.get("strategy").and_then(Value::as_str) != Some("chain") {
                continue;
            }
            let Some(e) = s.get("entry").and_then(Value::as_str) else {
                continue;
            };
            if !files.contains(e)
                || proxy_or_runtime(e)
                || program_shaped(e)
                || own.names(e)
                || !e.to_lowercase().ends_with(".dll")
            {
                out.add(format!("{lf}.coexist[{j}].entry"), CHAIN_ENTRY_INVALID);
            }
        }
    }
    // §7.4: `codes` among the descriptor's own.
    if let Some(Value::Array(c)) = l.get("codes")
        && c.iter()
            .any(|x| !x.as_str().is_some_and(|x| own.codes.contains(x)))
    {
        out.add(f("codes"), CODE_UNDECLARED);
    }
    (lookups, probes)
}

/// Every §7.3 path a loader holds, with its member.
fn loader_paths<'v>(
    lf: &str,
    l: &'v Map<String, Value>,
    walked: &[Walked<'v>],
) -> Vec<(String, &'v Value)> {
    let mut out = Vec::new();
    if let Some(r) = l.get("root") {
        out.push((format!("{lf}.root"), r));
    }
    for w in walked {
        for k in LOOKUPS.iter().chain(["tail"].iter()) {
            if let Some(p) = w.entry.get(*k) {
                out.push((format!("{}.{k}", w.field), p));
            }
        }
    }
    if let Some(Value::Object(v)) = l.get("version")
        && let Some(p) = v.get("pe")
    {
        out.push((format!("{lf}.version.pe"), p));
    }
    if let Some(Value::Array(en)) = l.get("enabled") {
        for (j, fm) in en.iter().enumerate() {
            if let Some(p) = fm.as_object().and_then(|fm| fm.get("list")) {
                out.push((format!("{lf}.enabled[{j}].list"), p));
            }
        }
    }
    if let Some(Value::Array(co)) = l.get("coexist") {
        for (j, s) in co.iter().enumerate() {
            if let Some(p) = s.as_object().and_then(|s| s.get("entry")) {
                out.push((format!("{lf}.coexist[{j}].entry"), p));
            }
        }
    }
    out
}

/// A `tail` probe's `(equals, min_len)`: `equals` 1 to 16 bytes, `bytes` its length when given,
/// `min_len` a count (default `bytes`).
fn tail_of(e: &Map<String, Value>) -> Option<(Vec<u8>, u64)> {
    let equals = e.get("equals")?.as_str()?.as_bytes().to_vec();
    if !(1..=limits::TAIL_BYTES).contains(&equals.len()) {
        return None;
    }
    if let Some(b) = e.get("bytes")
        && b.as_u64() != Some(equals.len() as u64)
    {
        return None;
    }
    let min_len = match e.get("min_len") {
        None => equals.len() as u64,
        Some(m) => m.as_u64()?,
    };
    Some((equals, min_len))
}

// ---- The typed model, from a table [`validate`] found nothing wrong with. ----

fn path_of(v: &Value) -> Option<ModsPath> {
    let s = v.as_str()?;
    path_reason(s).is_none().then(|| ModsPath(s.to_owned()))
}

fn build_check(v: &Value) -> Option<MCheck> {
    let e = v.as_object()?;
    for k in COMBINATORS {
        if let Some(list) = e.get(k) {
            let inner = list
                .as_array()?
                .iter()
                .map(build_check)
                .collect::<Option<Vec<_>>>()?;
            return Some(match k {
                "any" => MCheck::Any(inner),
                "all" => MCheck::All(inner),
                _ => MCheck::None(inner),
            });
        }
    }
    if let Some(p) = e.get("tail") {
        let (equals, min_len) = tail_of(e)?;
        return Some(MCheck::Tail {
            path: path_of(p)?,
            equals,
            min_len,
        });
    }
    let kind = match e.get("kind")?.as_str()? {
        "file" => Kind::File,
        "dir" => Kind::Dir,
        _ => return None,
    };
    if let Some(p) = e.get("listed") {
        return Some(MCheck::Listed {
            path: path_of(p)?,
            kind,
        });
    }
    if let Some(p) = e.get("child") {
        return Some(MCheck::Child {
            path: path_of(p)?,
            kind,
        });
    }
    let p = e.get("stat")?;
    Some(MCheck::Stat {
        path: path_of(p)?,
        kind,
    })
}

fn build_loader(l: &Map<String, Value>, own: &[EngineCode]) -> Option<Loader> {
    let strings = |k: &str| -> Option<Vec<&str>> {
        l.get(k)?.as_array()?.iter().map(Value::as_str).collect()
    };
    let codes = match l.get("codes") {
        None => own.to_vec(),
        Some(_) => strings("codes")?
            .into_iter()
            .map(EngineCode::new)
            .collect::<Option<Vec<_>>>()?,
    };
    let mut sources = Vec::new();
    for s in strings("source")? {
        let k = SourceKind::from_code(s)?;
        if !sources.contains(&k) {
            sources.push(k);
        }
    }
    let require = l
        .get("require")?
        .as_array()?
        .iter()
        .map(build_check)
        .collect::<Option<Vec<_>>>()?;
    let version = match l.get("version") {
        None => None,
        Some(v) => {
            let v = v.as_object()?;
            let template = Template::parse(v.get("template")?.as_str()?)?;
            let versions = match v.get("versions") {
                None => None,
                Some(t) => {
                    let text = t.as_str()?;
                    let comparators = comparators(text)?
                        .into_iter()
                        .map(|(op, o)| Some((op, template.operand(o)?)))
                        .collect::<Option<Vec<_>>>()?;
                    Some(Versions {
                        text: text.to_owned(),
                        comparators,
                    })
                }
            };
            Some(LoaderVersion {
                pe: path_of(v.get("pe")?)?,
                keys: v
                    .get("key")?
                    .as_array()?
                    .iter()
                    .map(|k| VersionKey::from_str(k.as_str()?))
                    .collect::<Option<Vec<_>>>()?,
                template,
                versions,
            })
        }
    };
    let enabled = l
        .get("enabled")?
        .as_array()?
        .iter()
        .map(|f| {
            let f = f.as_object()?;
            Some(match f.get("format")?.as_str()? {
                "dir-per-mod" => Enabled::DirPerMod,
                "enable-list" => Enabled::EnableList {
                    list: path_of(f.get("list")?)?,
                    marker: match f.get("marker") {
                        None => None,
                        Some(m) => Some(m.as_str()?.to_owned()),
                    },
                },
                "file-glob" => Enabled::FileGlob(Glob(f.get("glob")?.as_str()?.to_owned())),
                _ => return None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let tell_of = |s: &Map<String, Value>| -> Option<Option<Tell>> {
        match s.get("tell") {
            None => Some(None),
            Some(t) => Some(Some(Tell::from_code(t.as_str()?)?)),
        }
    };
    let coexist = l
        .get("coexist")?
        .as_array()?
        .iter()
        .map(|s| {
            let s = s.as_object()?;
            Some(match s.get("strategy")?.as_str()? {
                "chain" => Coexist::Chain {
                    entry: path_of(s.get("entry")?)?,
                    tell: tell_of(s)?,
                },
                "side-by-side" => Coexist::SideBySide { tell: tell_of(s)? },
                "via-manager" => Coexist::ViaManager,
                _ => return None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Loader {
        id: l.get("id")?.as_str()?.to_owned(),
        name: l.get("name")?.as_str()?.to_owned(),
        codes,
        sources,
        require,
        version,
        root: path_of(l.get("root")?)?,
        enabled,
        coexist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// Every name `paths::check_component` refuses, but for its own temporary-file mark, §7.3
    /// refuses as a component too: §7.3 is that function's rules plus C1 and the 8.3 names.
    #[test]
    fn a_component_keeps_to_check_component_and_more() {
        for c in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "a*b",
            "a?b",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a\u{1}b",
            "a\u{7f}b",
            "a.",
            "a ",
            "CON",
            "con.txt",
            "NUL",
            "COM1",
            "lpt9.log",
            "COM\u{b9}",
            "CONIN$",
            "conout$.x",
            "AUX .txt",
        ] {
            assert!(!crate::paths::check_component(c), "{c:?}");
            assert!(component_problem(c, false).is_some(), "{c:?}");
            assert!(component_problem(c, true).is_some(), "{c:?}");
        }
        for c in [
            "a\u{85}b",
            "a\u{9f}b",
            "XINPUT~1.DLL",
            "PROGRA~1",
            "x~9",
            "{x",
            "a}b",
        ] {
            assert!(component_problem(c, false).is_some(), "{c:?}");
        }
        for c in [
            "~mods",
            "a~b.dll",
            "COM10",
            "CONFIG",
            "console.dll",
            "nullable.txt",
        ] {
            assert_eq!(component_problem(c, false), None, "{c:?}");
        }
        assert_eq!(component_problem("{stem}_Data", true), None);
        assert_eq!(component_problem("{stem}_Data", false), Some(PATH_INVALID));
        assert_eq!(component_problem("{source}", true), Some(PATH_INVALID));
        assert_eq!(component_problem(&"a".repeat(255), false), None);
        assert_eq!(
            component_problem(&"a".repeat(256), false),
            Some(PATH_INVALID)
        );
        // 255 bytes, as §7.3 says, not 255 characters.
        assert_eq!(
            component_problem(&"é".repeat(128), false),
            Some(PATH_INVALID)
        );
    }

    #[test]
    fn placeholders_are_the_checkers() {
        assert_eq!(
            placeholder_names("{a}{project.leaf}x{b_1}"),
            ["a", "project.leaf", "b_1"]
        );
        assert!(placeholder_names("{A}{1a}{a.}{a.b.c}{}{a-b}").is_empty());
        assert_eq!(placeholder_names("{a{b}"), ["b"]);
        assert_eq!(placeholder_names("{a.b1}{c}"), ["c"]);
        assert_eq!(replace_placeholders("x{a}y{b.c}z", "*"), "x*y*z");
    }

    #[test]
    fn a_path_that_escapes_is_path_escapes_whatever_else_is_wrong() {
        assert_eq!(path_reason("{source}/a:b/../x"), Some(PATH_ESCAPES));
        assert_eq!(path_reason("{source}/CON/x/.."), Some(PATH_ESCAPES));
        assert_eq!(path_reason("{source}/a:b/x"), Some(PATH_INVALID));
        assert_eq!(path_reason("/abs/../x"), Some(PATH_OUTSIDE_JAIL));
        assert_eq!(path_reason("{source}"), None);
    }

    #[test]
    fn substitution_rechecks_every_component() {
        let p = "{source}/{project}/Content/Paks/~mods";
        assert_eq!(
            substitute(p, &values(&[("project", "Game/Pal")])),
            Some(vec![
                "Game".into(),
                "Pal".into(),
                "Content".into(),
                "Paks".into(),
                "~mods".into()
            ])
        );
        assert_eq!(substitute("{source}", &values(&[])), Some(Vec::new()));
        for bad in [
            "", "../x", "a//b", "/abs", "C:", "CON", "{source}", "a\\b", "x.",
        ] {
            assert_eq!(
                substitute("{source}/{dir}/x.dll", &values(&[("dir", bad)])),
                None,
                "{bad:?}"
            );
        }
        assert_eq!(substitute("{source}/{project}/x.dll", &values(&[])), None);
        assert_eq!(
            substitute("{source}/{stem}_Data/Managed", &values(&[("stem", "Game")])),
            Some(vec!["Game_Data".into(), "Managed".into()])
        );
    }

    #[test]
    fn templates_read_as_the_checker_reads_them() {
        for bad in [
            "", "N{5,1}", "N{0,1}", "N{1,6}", "N{1,2", "N{1,2}{", "N{1,2}!", "a b",
        ] {
            assert_eq!(Template::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Template::parse(&"a".repeat(33)), None);
        assert!(Template::parse(&"a".repeat(32)).is_some());
        let t = Template::parse("N{1,2}.N{1,2}.N{1,3}").unwrap();
        assert_eq!(t.read("5.4.23.3"), Some((vec![5, 4, 23], 6)));
        assert_eq!(t.read("5.4.2345"), None, "followed by a digit");
        assert_eq!(t.read("x5.4.2"), None);
        let be = Template::parse("N{1,2}.N{1,3}.N{1,3}-be.N{1,5}").unwrap();
        assert_eq!(
            be.read("6.0.0-be.788+5bd4ae5"),
            Some((vec![6, 0, 0, 788], 12))
        );
        assert_eq!(be.operand("6.0.0-be.788"), Some(vec![6, 0, 0, 788]));
        assert_eq!(be.operand("6.0"), Some(vec![6, 0]));
        assert_eq!(be.operand("6.0."), None);
        assert_eq!(t.operand("1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(
            Template::parse("N{1,2}.N{1,3}").unwrap().operand("1.2.3"),
            None
        );
        // Backtracking: a greedy run gives back what the next token needs.
        let adj = Template::parse("N{1,3}N{1,1}").unwrap();
        assert_eq!(adj.read("123"), Some((vec![12, 3], 3)));
        // Adjacent runs never take exponential time: 32 tokens, a long value of digits.
        let many = Template::parse(&"N{1,5}".repeat(5)).unwrap();
        assert_eq!(many.read(&"1".repeat(400)), None);
    }

    #[test]
    fn versions_compare_run_by_run_over_the_operand() {
        let t = Template::parse("N{1,2}.N{1,2}.N{1,3}").unwrap();
        let v = |text: &str| {
            let c = comparators(text).unwrap();
            Versions {
                text: text.to_owned(),
                comparators: c
                    .into_iter()
                    .map(|(op, o)| (op, t.operand(o).unwrap()))
                    .collect(),
            }
        };
        let r = v(">=5.4, <5.5");
        assert!(r.holds(&[5, 4, 23]));
        assert!(!r.holds(&[5, 5, 0]));
        assert!(!r.holds(&[5, 3, 9]));
        let be = Template::parse("N{1,2}.N{1,3}.N{1,3}-be.N{1,5}").unwrap();
        let exact = Versions {
            text: "=6.0.0-be.788".into(),
            comparators: vec![(Op::Eq, be.operand("6.0.0-be.788").unwrap())],
        };
        assert!(exact.holds(&be.read("6.0.0-be.788+5bd4ae5").unwrap().0));
        assert!(!exact.holds(&be.read("6.0.0-be.789").unwrap().0));
        for bad in [
            "",
            ">=",
            "~5",
            ">=5.4,<5.5",
            "=1, =2, =3",
            "=1, ",
            "==5",
            ">= 5",
        ] {
            assert_eq!(comparators(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn globs_match_one_listing_ignoring_case() {
        let g = Glob("*.dll".into());
        assert!(g.matches("Plugin.DLL") && g.matches(".dll"));
        assert!(!g.matches("Plugin.dll.old"));
        let two = Glob("a*b*c".into());
        assert!(two.matches("abc") && two.matches("aXbYc") && !two.matches("acb"));
        assert!(!Glob("ab*ba".into()).matches("aba"));
        for bad in ["**/*.pak", "a/b*", "*", "a", "a?*", "[a]*", "***", "{a}*"] {
            let want = if bad == "*" {
                None
            } else {
                Some(GLOB_OVER_BUDGET)
            };
            assert_eq!(glob_reason(&Value::String(bad.into())), want, "{bad:?}");
        }
        assert_eq!(glob_reason(&Value::Bool(true)), Some(GLOB_OVER_BUDGET));
    }

    #[test]
    fn managers_are_named_by_name_and_by_word() {
        for s in [
            "r2modmanPlus-local",
            "Thunderstore Mod Manager",
            "Mod Organizer 2",
            "modorganiser",
            "Mod_Organizer",
            "Overwolf",
            "gale",
            "Gale/profiles",
            "MO2",
            "x-vortex-y",
            "TMM",
            // The checker's `re.I` folds these onto ASCII letters.
            "Thunder\u{17f}tore",
            "Mod Organ\u{131}zer",
            "r2modman\u{0}",
            // Beside a character no ASCII letter folds from, a word is a word.
            "\u{e9}gale",
            "gale\u{e9}",
        ] {
            assert!(names_a_manager(s), "{s}");
        }
        for s in [
            "\u{130}gale",
            "gale\u{212a}",
            "nightingale",
            "galena",
            "mo21",
            "vortexes",
            "tmmx",
            "BepInEx",
            "mods",
            "mod  organizer",
            "mod_-organizer",
            "\u{131}tmm",
            "gale\u{17f}",
            "mo\u{0332}2",
        ] {
            assert!(!names_a_manager(s), "{s}");
        }
    }

    #[test]
    fn proxies_and_runtimes_are_read_as_the_checker_reads_them() {
        for p in [
            "{source}/WINHTTP.dll",
            "{source}/a/xinput1_3.DLL",
            "{source}/steam_api64.dll",
            "{source}/{stem}steam_api.dll",
            "{source}/VCRUNTIME140_1.dll",
            "{source}/vcrunt\u{130}me140.dll",
            "{source}/\u{17f}team_api.dll",
            "{source}/msvcp.dll",
            "{source}/steam_api.dll\n",
        ] {
            assert!(proxy_or_runtime(p), "{p:?}");
        }
        for p in [
            "{source}/winhttp.dll.bak",
            "{source}/w\u{130}nhttp.dll",
            "{source}/steam_api.dll\n\n",
            "{source}/steam_api\n.dll",
            "{source}/steam_ap.dll",
            "{source}/steam_api64.so",
            "{source}/BepInEx.dll",
            "{source}/mscvp140.dll",
        ] {
            assert!(!proxy_or_runtime(p), "{p:?}");
        }
    }

    #[test]
    fn nesting_counts_mods_itself() {
        let v: Value = serde_json::from_str(r#"{"a":[{"b":1}]}"#).unwrap();
        assert_eq!(nesting(&v), 3);
        assert_eq!(nesting(&Value::Bool(true)), 0);
    }

    /// A community descriptor of two codes whose detection looks up `probe.dat` and `{name}_Lib`.
    const HEAD: &str = "schema = \"astra.engine/1\"\nengine = { codes = { probe = \"Probe\", probe-two = \"Probe Two\" } }\n[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], at = \"{project}/bin\", first = 8 }\nrequire = [ { listed = \"probe.dat\", kind = \"file\" }, { child = \"{name}_Lib\", kind = \"dir\", as = \"lib\" } ]\ncode = \"probe\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{project}\"\n";

    /// One loader whose `require` is `require` and with `rest` after its members.
    fn loader(id: &str, require: &str, rest: &str) -> String {
        format!(
            "[[mods.loader]]\nid = \"{id}\"\nname = \"Loader\"\nsource = [\"game\"]\nrequire = [ {require} ]\nroot = \"{{source}}/l/mods\"\nenabled = [ {{ format = \"dir-per-mod\" }} ]\ncoexist = [ {{ strategy = \"side-by-side\" }} ]\n{rest}"
        )
    }

    const FILE: &str = "{ stat = \"{source}/l/loader.dll\", kind = \"file\" }";

    fn findings(mods: &str) -> Vec<(String, &'static str)> {
        let d = super::super::Descriptor::from_toml(&format!("{HEAD}{mods}")).unwrap();
        d.mods_findings()
            .iter()
            .map(|f| (f.field.clone(), f.reason))
            .collect()
    }

    fn one(field: &str, reason: &'static str) -> Vec<(String, &'static str)> {
        vec![(field.to_owned(), reason)]
    }

    /// Each finding names its member, and the launcher's stricter readings (the module doc) are
    /// refused: a name outside §2.1's characters, a `require` entry that is no table, a
    /// combinator holding more than its list or an empty one, a `tail` comparing nothing, a
    /// component past 255 bytes, a code with a line break after it.
    #[test]
    fn each_finding_names_its_member() {
        assert_eq!(findings(&loader("lo", FILE, "")), []);
        // A placeholder: §2.4's, or a capture of the descriptor's own detection (`lib`).
        for (path, want) in [
            ("{source}/{lib}/x.dll", vec![]),
            ("{source}/{project.leaf}/x.dll", vec![]),
            (
                "{source}/{data}/x.dll",
                one("mods.loader[0].require[1].stat", PLACEHOLDER_UNKNOWN),
            ),
        ] {
            let req = format!("{FILE}, {{ stat = \"{path}\", kind = \"file\" }}");
            assert_eq!(findings(&loader("lo", &req, "")), want, "{path}");
        }
        let named = |name: &str| loader("lo", FILE, "").replace("\"Loader\"", name);
        assert_eq!(
            findings(&named("\"x\u{b7}y\"")),
            one("mods.loader[0].name", SHAPE)
        );
        assert_eq!(
            findings(&named("\" x\"")),
            one("mods.loader[0].name", SHAPE)
        );
        let n = |k: usize| format!("\"{}\"", "n".repeat(k));
        assert_eq!(findings(&named(&n(49))), one("mods.loader[0].name", SHAPE));
        assert_eq!(findings(&named(&n(48))), []);
        assert_eq!(
            findings(&loader("lo", &format!("{FILE}, 5"), "")),
            one("mods.loader[0].require[1]", SHAPE)
        );
        assert_eq!(
            findings(&loader(
                "lo",
                &format!("{FILE}, {{ any = [ {FILE} ], kind = \"file\" }}"),
                ""
            )),
            one("mods.loader[0].require[1]", UNKNOWN)
        );
        assert_eq!(
            findings(&loader("lo", &format!("{FILE}, {{ all = [] }}"), "")),
            one("mods.loader[0].require[1].all", SHAPE)
        );
        assert_eq!(
            findings(&loader("lo", &format!("{FILE}, {{ none = 1 }}"), "")),
            one("mods.loader[0].require[1].none", SHAPE)
        );
        for tail in [
            "{ tail = \"{source}/l/loader.dll\" }",
            "{ tail = \"{source}/l/loader.dll\", equals = \"\" }",
            "{ tail = \"{source}/l/loader.dll\", equals = \"GDPC\", bytes = 3 }",
            "{ tail = \"{source}/l/loader.dll\", equals = \"0123456789abcdefg\" }",
        ] {
            assert_eq!(
                findings(&loader("lo", &format!("{FILE}, {tail}"), "")),
                one("mods.loader[0].require[1]", SHAPE),
                "{tail}"
            );
        }
        let tail =
            "{ tail = \"{source}/l/loader.dll\", equals = \"GDPC\", bytes = 4, min_len = 9 }";
        assert_eq!(findings(&loader("lo", &format!("{FILE}, {tail}"), "")), []);
        let long = format!(
            "{{ stat = \"{{source}}/{}\", kind = \"file\" }}",
            "\u{e9}".repeat(128)
        );
        assert_eq!(
            findings(&loader("lo", &format!("{FILE}, {long}"), "")),
            one("mods.loader[0].require[1].stat", PATH_INVALID)
        );
        assert_eq!(
            findings(&loader("lo\\n", FILE, "")),
            one("mods.loader[0].id", SHAPE)
        );
        assert_eq!(
            findings(&loader("lo", FILE, "codes = [\"probe\\n\"]\n")),
            [
                ("mods.loader[0].codes".to_owned(), SHAPE),
                ("mods.loader[0].codes".to_owned(), CODE_UNDECLARED)
            ]
        );
        // The four anchors §7.4 refuses, the checker's case folds included, and a detection
        // name read through its placeholder (`{name}_Lib`).
        for anchor in [
            "{source}/l/WinHttp.dll",
            "{source}/l/vcrunt\u{130}me140.dll",
            "{source}/l/{stem}.dll",
            "{source}/l/Game.exe",
            "{source}/l/PROBE.DAT",
            "{source}/l/My_Lib",
        ] {
            let req = format!("{{ stat = \"{anchor}\", kind = \"file\" }}");
            assert_eq!(
                findings(&loader("lo", &req, "")),
                one("mods.loader[0].require", LOADER_ANCHOR_INVALID),
                "{anchor}"
            );
        }
        // A chain entry is one of the loader's own top-level file lookups, never one inside a
        // combinator.
        let nested = format!(
            "{FILE}, {{ any = [ {{ stat = \"{{source}}/l/other.dll\", kind = \"file\" }} ] }}"
        );
        let chain = |entry: &str| {
            loader("lo", &nested, "").replace(
                "coexist = [ { strategy = \"side-by-side\" } ]",
                &format!("coexist = [ {{ strategy = \"chain\", entry = \"{entry}\" }} ]"),
            )
        };
        assert_eq!(findings(&chain("{source}/l/loader.dll")), []);
        assert_eq!(
            findings(&chain("{source}/l/other.dll")),
            one("mods.loader[0].coexist[0].entry", CHAIN_ENTRY_INVALID)
        );
        // `codes`: the descriptor's own, and a loader without them applies to every one.
        let m = |mods: &str| {
            super::super::Descriptor::from_toml(&format!("{HEAD}{mods}"))
                .unwrap()
                .mods()
                .unwrap()
                .clone()
        };
        let probe = EngineCode::new("probe").unwrap();
        let two = EngineCode::new("probe-two").unwrap();
        let all = m(&loader("lo", FILE, ""));
        assert!(all.loaders()[0].applies_to(&probe) && all.loaders()[0].applies_to(&two));
        let only = m(&loader("lo", FILE, "codes = [\"probe-two\"]\n"));
        assert!(!only.loaders()[0].applies_to(&probe) && only.loaders()[0].applies_to(&two));
        assert_eq!(
            findings(&loader("lo", FILE, "codes = [\"unity-mono\"]\n")),
            one("mods.loader[0].codes", CODE_UNDECLARED)
        );
        // A source named twice is one source.
        let twice =
            m(&loader("lo", FILE, "").replace("[\"game\"]", "[\"game\", \"picked\", \"game\"]"));
        assert_eq!(
            twice.loaders()[0].sources(),
            [SourceKind::Game, SourceKind::Picked]
        );
        // Ids: unique, each duplicate named.
        assert_eq!(
            findings(&format!(
                "{}{}{}",
                loader("aa", FILE, ""),
                loader("bb", FILE, ""),
                loader("aa", FILE, "")
            )),
            one("mods.loader[2].id", LOADER_ID_DUPLICATE)
        );
    }

    /// The names a descriptor's own detection looks up (§7.4) come from every place its rules
    /// look: a `listed`, a `tail`, a bootstrap's `listed` and an `also_if` folder; none of them
    /// anchors a loader, by a file or as its root. A name that is nothing but placeholders and
    /// `*` (`{stem}*`, `{name}`) is the program's, which the program rule covers: it is no
    /// detection name, or every name would be one.
    #[test]
    fn detection_names_come_from_every_lookup_of_the_rules() {
        let head = "schema = \"astra.engine/1\"\nengine = { codes = { probe = \"Probe\" } }\n[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], at = \"{project}/bin\", first = 8, also_if = { dir = \"Res_Pack\" } }\nrequire = [ { listed = \"probe.dat\", kind = \"file\" }, { tail = \"{project}/bin/data.bin\", bytes = 4, equals = \"DATA\" }, { any = [ { listed = \"{stem}*\", kind = \"file\" }, { child = \"{project}/{name}\", kind = \"dir\" } ] } ]\nbootstrap = { listed = \"{project.parent}/Boot.cfg\" }\ncode = \"probe\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{project}\"\n";
        let found = |mods: &str| -> Vec<(String, &'static str)> {
            super::super::Descriptor::from_toml(&format!("{head}{mods}"))
                .unwrap()
                .mods_findings()
                .iter()
                .map(|f| (f.field.clone(), f.reason))
                .collect()
        };
        for anchor in ["probe.dat", "DATA.BIN", "boot.cfg", "res_pack"] {
            let req = format!("{{ stat = \"{{source}}/x/{anchor}\", kind = \"file\" }}");
            assert_eq!(
                found(&loader("lo", &req, "")),
                one("mods.loader[0].require", LOADER_ANCHOR_INVALID),
                "{anchor}"
            );
        }
        let req = "{ stat = \"{source}/x/other.bin\", kind = \"file\" }";
        assert_eq!(found(&loader("lo", req, "")), []);
        // A root-anchored loader: its own root, unless detection looks that name up.
        for (root, want) in [
            ("Mods", vec![]),
            (
                "Res_Pack",
                one("mods.loader[0].require", LOADER_ANCHOR_INVALID),
            ),
        ] {
            let req = format!("{{ child = \"{{source}}/{root}\", kind = \"dir\" }}");
            let mods =
                loader("lo", &req, "").replace("{source}/l/mods", &format!("{{source}}/{root}"));
            assert_eq!(found(&mods), want, "{root}");
        }
    }

    /// Every member keeps to its closed set and its shape, each finding at the member it is
    /// about (§7.2, §7.4-§7.9).
    #[test]
    fn each_member_keeps_to_its_closed_set_and_its_shape() {
        let base = loader("lo", FILE, "");
        let with = |from: &str, to: &str| {
            assert_eq!(base.matches(from).count(), 1, "{from}");
            base.replacen(from, to, 1)
        };
        let l = "mods.loader[0]";
        let f = |rest: &str, reason: &'static str| (format!("{l}.{rest}"), reason);
        let cases: Vec<(String, Vec<(String, &'static str)>)> = vec![
            // A member §7.2 does not name; each required one missing.
            (format!("{base}later = 1\n"), vec![f("later", UNKNOWN)]),
            (with("id = \"lo\"\n", ""), vec![f("id", SHAPE)]),
            (with("name = \"Loader\"\n", ""), vec![f("name", SHAPE)]),
            (with("source = [\"game\"]\n", ""), vec![f("source", SHAPE)]),
            (
                with(&format!("require = [ {FILE} ]\n"), ""),
                vec![f("require", SHAPE), f("require", LOADER_ANCHOR_INVALID)],
            ),
            (
                with("root = \"{source}/l/mods\"\n", ""),
                vec![f("root", SHAPE)],
            ),
            (
                with("enabled = [ { format = \"dir-per-mod\" } ]\n", ""),
                vec![f("enabled", SHAPE)],
            ),
            (
                with("coexist = [ { strategy = \"side-by-side\" } ]\n", ""),
                vec![f("coexist", SHAPE)],
            ),
            // `source`: 1 to 3, closed.
            (with("[\"game\"]", "[]"), vec![f("source", SHAPE)]),
            (
                with(
                    "[\"game\"]",
                    "[\"game\", \"manager\", \"picked\", \"game\"]",
                ),
                vec![f("source", SHAPE)],
            ),
            (
                with("[\"game\"]", "[\"game\", \"steam\"]"),
                vec![f("source", UNKNOWN)],
            ),
            // `require`: 1 to 8 checks; a lookup with a key and a closed `kind`; nothing else.
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!("[ {} ]", [FILE; 9].join(", ")),
                ),
                vec![f("require", SHAPE)],
            ),
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!("[ {FILE}, {{ kind = \"file\" }} ]"),
                ),
                vec![f("require[1]", UNKNOWN)],
            ),
            (
                with("kind = \"file\" }", "kind = \"file\", as = \"x\" }"),
                vec![f("require[0].as", UNKNOWN)],
            ),
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!("[ {FILE}, {{ stat = \"{{source}}/l/b.dll\" }} ]"),
                ),
                vec![f("require[1].kind", SHAPE)],
            ),
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!("[ {FILE}, {{ stat = \"{{source}}/l/b.dll\", kind = \"link\" }} ]"),
                ),
                vec![f("require[1].kind", UNKNOWN)],
            ),
            // `any`/`all`/`none` 3 deep at most: a fourth is over budget where it is, and
            // nests past §7.1's 11 too.
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!(
                        "[ {FILE}, {{ any = [ {{ all = [ {{ none = [ {{ any = [ {FILE} ] }} ] }} ] }} ] }} ]"
                    ),
                ),
                vec![
                    ("mods".to_owned(), OVER_BUDGET),
                    f("require[1].any[0].all[0].none[0].any[0]", OVER_BUDGET),
                ],
            ),
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!("[ {FILE}, {{ any = [ {{ all = [ {{ none = [ {FILE} ] }} ] }} ] }} ]"),
                ),
                vec![],
            ),
            // Nine content probes in one member.
            (
                with(
                    &format!("[ {FILE} ]"),
                    &format!(
                        "[ {FILE}, {{ any = [ {} ] }} ]",
                        ["{ tail = \"{source}/l/loader.dll\", equals = \"MZ\" }"; 9].join(", ")
                    ),
                ),
                vec![("mods".to_owned(), OVER_BUDGET)],
            ),
            // `version`: a table of `pe`, `key` (1 or 2 of a closed set) and `template`, with
            // `versions` optional; each read as §7.4 says.
            (format!("{base}version = 1\n"), vec![f("version", SHAPE)]),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [\"FileVersion\"], template = \"N{{1,2}}\", later = 1 }}\n"
                ),
                vec![f("version.later", UNKNOWN)],
            ),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [\"FileVersion\"] }}\n"
                ),
                vec![f("version.template", SHAPE)],
            ),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [\"ProductName\"], template = \"N{{1,2}}\" }}\n"
                ),
                vec![f("version.key", UNKNOWN)],
            ),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [], template = \"N{{1,2}}\" }}\n"
                ),
                vec![f("version.key", SHAPE)],
            ),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [\"FileVersion\"], template = \"N{{1,2}}\", versions = \">= 5\" }}\n"
                ),
                vec![f("version.versions", VERSION_INVALID)],
            ),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [\"FileVersion\"], template = \"N{{1,2}}\", versions = \"=1, =2, =3\" }}\n"
                ),
                vec![f("version.versions", VERSION_INVALID)],
            ),
            (
                format!(
                    "{base}version = {{ pe = \"{{source}}/l/loader.dll\", key = [\"FileVersion\"], template = \"N{{1,2}}.N{{1,2}}\", versions = \">=5.4, <5.5\" }}\n"
                ),
                vec![],
            ),
            // `enabled`: 1 to 3 of a closed set, each with its own members.
            (
                with("[ { format = \"dir-per-mod\" } ]", "[]"),
                vec![f("enabled", SHAPE)],
            ),
            (
                with(
                    "[ { format = \"dir-per-mod\" } ]",
                    "[ { format = \"dir-per-mod\" }, { format = \"dir-per-mod\" }, { format = \"dir-per-mod\" }, { format = \"dir-per-mod\" } ]",
                ),
                vec![f("enabled", SHAPE)],
            ),
            (
                with("\"dir-per-mod\"", "\"json\""),
                vec![f("enabled[0].format", UNKNOWN)],
            ),
            (
                with(
                    "{ format = \"dir-per-mod\" }",
                    "{ format = \"dir-per-mod\", glob = \"*.dll\" }",
                ),
                vec![f("enabled[0].glob", UNKNOWN)],
            ),
            (
                with(
                    "{ format = \"dir-per-mod\" }",
                    "{ format = \"enable-list\", list = \"{source}/l/mods.txt\" }",
                ),
                vec![f("enabled[0].line", SHAPE)],
            ),
            (
                with(
                    "{ format = \"dir-per-mod\" }",
                    "{ format = \"enable-list\", list = \"{source}/l/mods.txt\", line = \"json\" }",
                ),
                vec![f("enabled[0].line", UNKNOWN)],
            ),
            (
                with(
                    "{ format = \"dir-per-mod\" }",
                    "{ format = \"enable-list\", list = \"{source}/l/mods.txt\", line = \"name-colon-flag\", marker = \"a/b\" }",
                ),
                vec![f("enabled[0].marker", PATH_INVALID)],
            ),
            (
                with(
                    "{ format = \"dir-per-mod\" }",
                    "{ format = \"file-glob\", glob = \"**\" }",
                ),
                vec![f("enabled[0].glob", GLOB_OVER_BUDGET)],
            ),
            // `coexist`: 0 to 3 of a closed set, each with its own members; `tell` closed.
            (with("[ { strategy = \"side-by-side\" } ]", "[]"), vec![]),
            (
                with(
                    "[ { strategy = \"side-by-side\" } ]",
                    "[ { strategy = \"side-by-side\" }, { strategy = \"side-by-side\" }, { strategy = \"side-by-side\" }, { strategy = \"side-by-side\" } ]",
                ),
                vec![f("coexist", SHAPE)],
            ),
            (
                with("\"side-by-side\"", "\"copy\""),
                vec![f("coexist[0].strategy", UNKNOWN)],
            ),
            (
                with(
                    "{ strategy = \"side-by-side\" }",
                    "{ strategy = \"via-manager\", tell = \"ue4ss-mods-paths\" }",
                ),
                vec![f("coexist[0].tell", UNKNOWN)],
            ),
            (
                with(
                    "{ strategy = \"side-by-side\" }",
                    "{ strategy = \"chain\" }",
                ),
                vec![f("coexist[0].entry", SHAPE)],
            ),
            (
                with(
                    "{ strategy = \"side-by-side\" }",
                    "{ strategy = \"side-by-side\", tell = \"env\" }",
                ),
                vec![f("coexist[0].tell", UNKNOWN)],
            ),
            // `codes`: 1 to 4.
            (format!("{base}codes = []\n"), vec![f("codes", SHAPE)]),
        ];
        for (mods, want) in cases {
            assert_eq!(findings(&mods), want, "{mods}");
        }
        // Five loaders; four that each read a version, a list and a marker (12 probes).
        let five: String = ["aa", "bb", "cc", "dd", "ee"]
            .iter()
            .map(|id| loader(id, FILE, ""))
            .collect();
        assert_eq!(findings(&five), one("mods.loader", SHAPE));
        let probing: String = ["aa", "bb", "cc", "dd"]
            .iter()
            .map(|id| {
                loader(id, FILE, "version = { pe = \"{source}/l/loader.dll\", key = [\"FileVersion\"], template = \"N{1,2}\" }\n")
                    .replace(
                        "{ format = \"dir-per-mod\" }",
                        "{ format = \"enable-list\", list = \"{source}/l/mods.txt\", line = \"name-colon-flag\", marker = \"on.txt\" }",
                    )
            })
            .collect();
        assert_eq!(findings(&probing), one("mods", OVER_BUDGET));
        assert_eq!(findings(&probing.replace(", marker = \"on.txt\"", "")), []);
    }
}
