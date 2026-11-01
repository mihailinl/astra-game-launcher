// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Engines the launcher learns from data: `astra.engine/1` engine descriptors.
//!
//! A descriptor describes the SHAPE of a game folder made with one engine — which program, what
//! sits beside it — never a list of games. The daemon gathers them (the signed games catalogue,
//! a developer's local folder, an installed foundation) into an [`EngineSet`] and hands it to
//! [`crate::detect_with_engines`]. Three rules make that safe to do with strangers' data:
//!
//! - **Narrow-only.** The built-in descriptors (`builtin/*.toml`: Unity, Godot, Unreal; Pass A)
//!   run first, through the same evaluator (`eval.rs`), under their own policy. Descriptors
//!   (Pass B) are asked only when Pass A says `unknown` with no evidence and no hint used, so no
//!   descriptor can take or change a game Unity, Unreal or Godot already recognised; their codes,
//!   and `unknown`, are reserved ([`EngineCode::is_reserved`]).
//! - **Anchored.** Every rule of a descriptor that is not first-party names a literal file or
//!   folder beside the program (`listed`, at the top of `require`) that is neither a program nor a
//!   runtime every game ships (`steam_api64.dll`, `vcruntime140.dll`, …). It is also the O(1)
//!   prefilter: a descriptor whose anchors the folder does not hold costs nothing.
//! - **Counted, never timed.** Each descriptor has a budget of lookups and probes per game
//!   ([`descriptor::limits`]); a descriptor over its budget yields nothing, and a game over its
//!   own budget gets no Pass B answer at all. Choosing programs is linear in the walk per rule.
//!   Nothing in either pass waits on a clock, follows a link, or blocks on a FIFO — the built-ins
//!   included, which 0.4.1 read with a plain open (`tests/corpus/divergence.txt`).
//!
//! The schema, every limit and the reserved names live in [`descriptor`], one module, so the
//! public specification's goldens can be pinned against it.

pub mod descriptor;
mod eval;
mod reads;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

pub use descriptor::{Descriptor, is_reserved_code, is_valid_engine_code};
pub(crate) use eval::{NamedEngine, engine_of_program, pass_a, pass_b};

/// The built-in descriptors, as text, in the order Pass A asks them: 0.4.1's `passes()` asked
/// Unity, then Godot, then Unreal of each program.
const BUILTIN: [&str; 3] = [
    include_str!("builtin/unity.toml"),
    include_str!("builtin/godot.toml"),
    include_str!("builtin/unreal.toml"),
];

/// The built-in descriptors (`builtin/*.toml`): Astra's own, asked in Pass A before any other,
/// and held to launcher 0.4.1's detector by canary C1. Parsed once; a built-in that does not
/// parse is a build defect, and every test fails on it.
pub(crate) fn builtin() -> &'static EngineSet {
    static SET: LazyLock<EngineSet> = LazyLock::new(|| {
        let entries = BUILTIN
            .iter()
            .map(|text| Admitted {
                descriptor: Descriptor::from_toml(text)
                    .unwrap_or_else(|e| panic!("a built-in engine descriptor does not parse: {e}")),
                origin: Origin {
                    moderation: Moderation::Moderated,
                    publisher: "astra".to_owned(),
                    first_party: true,
                },
            })
            .collect();
        EngineSet {
            entries,
            skipped: Vec::new(),
        }
    });
    &SET
}

use crate::detect::Engine;
use crate::error::{LauncherError, Result, codes};

/// An engine's code, as a [`crate::Detection`] reports it and `[target] engine` names it:
/// `^[a-z][a-z0-9-]{1,31}$`. The built-in ones are [`Engine`]'s (`"unity-mono"`,
/// `"unity-il2cpp"`, `"unreal"`, `"godot"`, and `"unknown"` for none); any other comes from an
/// engine descriptor. Serialises as the bare string, exactly as [`Engine`] always did.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EngineCode(String);

impl EngineCode {
    /// The code, if it is spelled as one may be ([`is_valid_engine_code`]).
    pub fn new(code: &str) -> Option<EngineCode> {
        is_valid_engine_code(code).then(|| EngineCode(code.to_owned()))
    }

    /// `"unknown"`: no engine recognised.
    pub fn unknown() -> EngineCode {
        Engine::Unknown.into()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The same as [`EngineCode::as_str`], spelled as [`Engine::code`] is.
    pub fn code(&self) -> &str {
        &self.0
    }

    /// The built-in engine this code is, `Engine::Unknown` included; `None` for a code an engine
    /// descriptor declares.
    pub fn builtin(&self) -> Option<Engine> {
        Engine::from_code(&self.0)
    }

    pub fn is_unknown(&self) -> bool {
        self.builtin() == Some(Engine::Unknown)
    }

    /// Whether no engine descriptor may declare this code ([`is_reserved_code`]): a built-in
    /// one, `unknown`, or one that reads as one of them once folded (`unity-mon0`, `un1ty-x`,
    /// `g0dot`); one beginning with a built-in engine's family name or `astra`; one holding
    /// `ambiguous`. They would read as Astra's own.
    pub fn is_reserved(&self) -> bool {
        is_reserved_code(&self.0)
    }
}

impl From<Engine> for EngineCode {
    fn from(e: Engine) -> Self {
        EngineCode(e.code().to_owned())
    }
}

/// A string that is not an engine code ([`is_valid_engine_code`]): what reading a
/// [`crate::Detection`] whose `engine` is misspelled says. Not a descriptor's fault, so not a
/// `descriptor-*` code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnEngineCode(pub String);

impl fmt::Display for NotAnEngineCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not an engine code: {:?}", self.0)
    }
}

impl std::error::Error for NotAnEngineCode {}

impl TryFrom<String> for EngineCode {
    type Error = NotAnEngineCode;
    fn try_from(s: String) -> std::result::Result<Self, NotAnEngineCode> {
        if is_valid_engine_code(&s) {
            Ok(EngineCode(s))
        } else {
            Err(NotAnEngineCode(s))
        }
    }
}

impl From<EngineCode> for String {
    fn from(c: EngineCode) -> String {
        c.0
    }
}

impl fmt::Display for EngineCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<Engine> for EngineCode {
    fn eq(&self, other: &Engine) -> bool {
        self.0 == other.code()
    }
}

impl PartialEq<EngineCode> for Engine {
    fn eq(&self, other: &EngineCode) -> bool {
        other == self
    }
}

impl PartialEq<str> for EngineCode {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for EngineCode {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// How far a descriptor's source has been looked at. Pass B asks them in this order, and a
/// match of a lower standing never replaces, nor makes ambiguous, one of a higher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Moderation {
    /// In the signed catalogue, reviewed by a moderator.
    Moderated,
    /// In the signed catalogue, not reviewed.
    Unchecked,
    /// From this machine only (developer mode).
    Local,
}

/// Where a descriptor came from, as the caller vouches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Origin {
    pub moderation: Moderation,
    /// Who publishes it: an opaque identity the caller assigns (the catalogue's bound publisher).
    /// An engine code belongs to the first publisher admitted with it ([`EngineSet::build`]),
    /// and no publisher has more than [`descriptor::limits::PER_PUBLISHER`] descriptors asked.
    pub publisher: String,
    /// Astra's own: may use the first-party-only parts of the grammar (`weak_order`,
    /// `skip_wrappers = false`, rules with no anchor, …).
    pub first_party: bool,
}

/// One descriptor an [`EngineSet`] declined, and why: for the one banner the daemon shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// Its position in what [`EngineSet::build`] was given.
    pub index: usize,
    pub origin: Origin,
    /// Its digest, when it parsed at all.
    pub descriptor_sha256: Option<String>,
    pub error: LauncherError,
}

#[derive(Debug, Clone)]
pub(crate) struct Admitted {
    pub(crate) descriptor: Descriptor,
    pub(crate) origin: Origin,
}

/// The engine descriptors Pass B asks, in the order it asks them: moderated, then unchecked,
/// then local, each in the order given. At most [`descriptor::limits::PASS_B_DESCRIPTORS`], at
/// most [`descriptor::limits::PER_PUBLISHER`] from one publisher. Empty by default, and an empty
/// set is Pass A alone.
#[derive(Debug, Clone, Default)]
pub struct EngineSet {
    pub(crate) entries: Vec<Admitted>,
    skipped: Vec<Skipped>,
}

impl EngineSet {
    /// No descriptors: [`crate::detect_with_engines`] is then exactly [`crate::detect_with`].
    pub fn empty() -> EngineSet {
        EngineSet::default()
    }

    /// Admits each offered descriptor that may be asked in Pass B, and records each one that
    /// may not ([`EngineSet::skipped`]) — a descriptor that did not parse (pass its error
    /// through), one Pass B refuses ([`Descriptor::check_pass_b`]), one claiming a code
    /// another publisher was admitted with first (`engine-code-taken`), a copy of one admitted
    /// already (`descriptor-duplicate`), or one past the count limits (`descriptor-over-limit`).
    /// One refusal costs that descriptor only.
    pub fn build<I>(offered: I) -> EngineSet
    where
        I: IntoIterator<Item = (Origin, Result<Descriptor>)>,
    {
        let mut offered: Vec<(usize, Origin, Result<Descriptor>)> = offered
            .into_iter()
            .enumerate()
            .map(|(i, (o, d))| (i, o, d))
            .collect();
        // Stable: within one standing, the caller's order.
        offered.sort_by_key(|(_, o, _)| o.moderation);

        let mut set = EngineSet::default();
        let mut owners: BTreeMap<String, String> = BTreeMap::new();
        let mut per_publisher: BTreeMap<String, usize> = BTreeMap::new();
        for (index, origin, parsed) in offered {
            let skip = |set: &mut EngineSet, origin: Origin, digest, error| {
                set.skipped.push(Skipped {
                    index,
                    origin,
                    descriptor_sha256: digest,
                    error,
                })
            };
            let d = match parsed {
                Ok(d) => d,
                Err(e) => {
                    skip(&mut set, origin, None, e);
                    continue;
                }
            };
            let digest = Some(d.digest().to_owned());
            if let Err(e) = d.check_pass_b(origin.first_party) {
                skip(&mut set, origin, digest, e);
                continue;
            }
            if set
                .entries
                .iter()
                .any(|a| a.descriptor.digest() == d.digest())
            {
                skip(
                    &mut set,
                    origin,
                    digest,
                    LauncherError::new(codes::DESCRIPTOR_DUPLICATE),
                );
                continue;
            }
            // Owned as a person reads it: a look-alike of an owned code is that code.
            if let Some((code, owner)) = d.codes().find_map(|(c, _)| {
                owners
                    .get(&descriptor::code_identity(c.as_str()))
                    .filter(|owner| **owner != origin.publisher)
                    .map(|owner| (c.as_str().to_owned(), owner.clone()))
            }) {
                skip(
                    &mut set,
                    origin,
                    digest,
                    LauncherError::new(codes::ENGINE_CODE_TAKEN)
                        .with("code", code)
                        .with("owner", owner),
                );
                continue;
            }
            if set.entries.len() >= descriptor::limits::PASS_B_DESCRIPTORS {
                skip(
                    &mut set,
                    origin,
                    digest,
                    LauncherError::new(codes::DESCRIPTOR_OVER_LIMIT).with("reason", "total"),
                );
                continue;
            }
            let count = per_publisher.entry(origin.publisher.clone()).or_default();
            if *count >= descriptor::limits::PER_PUBLISHER {
                skip(
                    &mut set,
                    origin,
                    digest,
                    LauncherError::new(codes::DESCRIPTOR_OVER_LIMIT).with("reason", "publisher"),
                );
                continue;
            }
            *count += 1;
            for (c, _) in d.codes() {
                owners
                    .entry(descriptor::code_identity(c.as_str()))
                    .or_insert_with(|| origin.publisher.clone());
            }
            set.entries.push(Admitted {
                descriptor: d,
                origin,
            });
        }
        set
    }

    /// How many descriptors Pass B asks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The descriptors Pass B asks, in its order.
    pub fn descriptors(&self) -> impl Iterator<Item = (&Descriptor, &Origin)> {
        self.entries.iter().map(|a| (&a.descriptor, &a.origin))
    }

    /// The descriptors declined, in the order they were offered.
    pub fn skipped(&self) -> Vec<&Skipped> {
        let mut v: Vec<&Skipped> = self.skipped.iter().collect();
        v.sort_by_key(|s| s.index);
        v
    }

    /// The folders the walk still lists at its depth bound (`walk.list_at_limit`), from every
    /// descriptor here: only the built-in set's ever name any.
    pub(crate) fn list_at_limit(&self) -> Vec<&[String]> {
        self.entries
            .iter()
            .flat_map(|a| a.descriptor.model.list_at_limit.iter().map(Vec::as_slice))
            .collect()
    }

    /// The display name of `code`, from the descriptor that declares it.
    pub fn engine_name(&self, code: &EngineCode) -> Option<&str> {
        self.entries.iter().find_map(|a| {
            a.descriptor
                .codes()
                .find(|(c, _)| *c == code)
                .map(|(_, n)| n)
        })
    }
}
