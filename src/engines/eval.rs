// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The one evaluator of engine descriptors, under two policies: the built-in set (Pass A) and an
//! [`EngineSet`] (Pass B). Both read a game folder through the same lookups — never through a
//! link, never blocking on a FIFO — and choose among matches the same way; they differ in what
//! they are allowed, which is what each section below says.
//!
//! **Pass A: the built-in descriptors** (`builtin/*.toml`; Unity, Godot, Unreal, in that order).
//! Astra's own, so nothing is counted against a budget and no program's magic is asked, and a
//! rule whose `classify` ends at `unknown` (a Unity player whose data names neither flavour) still
//! names its program. In order:
//!
//! 1. Steam's launch hint, jailed: the first strong rule (descriptor order, then rule order) that
//!    matches it wins outright.
//! 2. The folder search: every file of the walk asks the strong rules, then the strong-fallback
//!    ones, in that order, and the first that matches speaks for it. Of all those matches, `best`
//!    chooses one (below).
//! 3. The weak rules, by `weak_order`: a rule of the default scope as in 2, chosen by `best`; a
//!    rule of scope `anywhere` by the first match in walk order. The first order with a match
//!    answers. Rules of scope `named` are never asked here.
//!
//! **Pass B: an [`EngineSet`]**, asked only about a game Pass A left `unknown` with no evidence
//! and no hint used. Standing by standing — moderated, then unchecked, then local — and the first
//! standing with any match decides: a lower one is never asked, so it can neither replace nor
//! make ambiguous a match above it. Within a standing, Steam's launch hint first (a descriptor
//! matching the hint wins over every folder-search match), then the folder search. A program
//! must be what its name says (a PE or ELF image), and a `classify` ending at `unknown` names
//! nothing. Among one standing's matches: those from the hint; then strong rules over weak ones
//! (and the lowest `weak_order` among weak ones); the shallowest; one that `passes` over one
//! that does not. If more than one engine code is still standing that is `engine-ambiguous`:
//! nothing is preselected. One code: `best`.
//!
//! **`best`** (Pass A's own order, 0.4.1's `best()`): the shallowest (by evidence, else the
//! program); one that passes over one that does not; a matching bootstrapper; the largest
//! evidence; the name repeating its evidence's; the first by path. A first-party tie sizes every
//! evidence folder; any other sizes at most [`limits::SIZINGS_PER_GAME`] folders, and past that
//! sizes none and falls to the name and the path — an answer either way.
//!
//! **Bounds** (Pass B). Everything is counted ([`limits`]): a descriptor over its own lookups or
//! probes yields nothing for this game; the game over its lookups or bytes gets no Pass B answer
//! at all. A rule's anchors are looked for before anything else it asks, in the listing already
//! in memory: absent beside the program — or anywhere in the walk — the rule costs nothing, on
//! Steam's hint as in the folder search. Choosing programs is linear in the walk per rule:
//! `also_if` and the anchors are answered from per-folder name sets built once per game, never
//! by scanning a listing ([`Folder`]).
//!
//! **A named program** ([`engine_of_program`], planning's engine check): the rules asked about
//! the one program the caller names, read from a fresh listing of its folder — strong, then
//! strong-fallback, then weak by `weak_order` (scope default or `named`) — the first that matches
//! giving the engine; only when no built-in rule matches, the set's descriptors.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;

use super::descriptor::{
    At, BinaryRule, Check, Kind, Model, NameVar, NamedAlike, Outcome, PASS_B_PROGRAM_EXTS, PathT,
    PathVar, Piece, Programs, Rule, RuleTier, Scope, Select, Tok, WrapperSkip, limits,
};
use super::{EngineCode, EngineSet, Moderation, reads};
use crate::detect::{
    self, Binary, Confidence, Detection, Entry, ExePick, MAX_ENTRIES, Walk, abs, rel_path,
};
use crate::error::codes;
use crate::paths::{check_component, relative_components};

type Listing = Vec<(String, bool)>;

/// A path below the game's root, one component per name.
type Rel = Vec<String>;
/// Folders read once per game, by path below the root: `None` when not a real folder.
type Memo<T> = RefCell<HashMap<Rel, Option<Rc<T>>>>;

use folder::Folder;

/// Its own module so that its listing is private: the evaluator reads a listing only through
/// [`Folder::scan`], which charges every name of it to the game's work counter — a scan nobody
/// counts cannot be written by accident.
mod folder {
    use std::cell::Cell;
    use std::collections::HashSet;

    use super::Listing;

    /// One folder's names: as listed (sorted), and lower-cased by kind, so "is there a folder (a
    /// file) of this name, ignoring ASCII case" — `also_if`, the anchors, every `listed` that
    /// finds nothing — is one set lookup, never a scan of a listing that may hold 5,000 names.
    pub(super) struct Folder {
        listing: Listing,
        dirs: HashSet<String>,
        files: HashSet<String>,
    }

    impl Folder {
        pub(super) fn new(listing: Listing) -> Folder {
            let mut dirs = HashSet::new();
            let mut files = HashSet::new();
            for (n, d) in &listing {
                if *d { &mut dirs } else { &mut files }.insert(n.to_ascii_lowercase());
            }
            Folder {
                listing,
                dirs,
                files,
            }
        }

        /// The listing, to be scanned: every name in it is counted into `work` as examined.
        pub(super) fn scan(&self, work: &Cell<u64>) -> &Listing {
            work.set(work.get() + self.listing.len() as u64);
            &self.listing
        }

        /// Whether a folder (`dir`) or a file of this name, already lower case, is here
        /// ignoring ASCII case: exactly `detect::find_in(&listing, name, dir).is_some()`.
        pub(super) fn holds_lower(&self, lower: &str, dir: bool) -> bool {
            if dir { &self.dirs } else { &self.files }.contains(lower)
        }

        pub(super) fn holds(&self, name: &str, dir: bool) -> bool {
            self.holds_lower(&name.to_ascii_lowercase(), dir)
        }
    }
}

/// What the evaluator is allowed: see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// The built-in descriptors (Pass A, and a named program's engine).
    BuiltIn,
    /// An [`EngineSet`]'s descriptors (Pass B).
    Descriptors,
}

/// A walk file a Pass B rule may take for a program: extension-less, or one of
/// [`PASS_B_PROGRAM_EXTS`]. Found once per game.
struct ProgramLike {
    /// Its index in the walk.
    at: usize,
    /// Its name, lower case.
    lower: String,
}

/// Why an evaluation stopped before its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    /// This descriptor is over its own budget: it yields nothing for this game.
    Descriptor,
    /// The game is over its budget: Pass B gives it no answer.
    Game,
}

type Step<T> = std::result::Result<T, Halt>;

/// What one descriptor has spent on one game.
#[derive(Default)]
struct Spent {
    lookups: u32,
    probes: u32,
}

/// One program a rule is asked about.
struct Prog {
    rel: Vec<String>,
    dir: Vec<String>,
    name: String,
    project: Option<Vec<String>>,
    /// The program's folder: as the walk listed it (the folder search), or read afresh (the
    /// hint, a named program).
    folder: Rc<Folder>,
    /// Steam's hint, or the caller, named it: `pick` takes it as it is.
    from_hint: bool,
}

/// One descriptor being asked.
struct Member<'s> {
    desc: usize,
    model: &'s Model,
    first_party: bool,
}

/// One rule's view while it is evaluated: its program (none for `programs = "none"`), its
/// `{project}`, and what its checks captured — each capture with every path it stands for, the
/// first being the one a template reads (`programs = "none"` captures every match of its project).
struct View<'p> {
    prog: Option<&'p Prog>,
    project: Option<Vec<String>>,
    captures: Vec<(String, Vec<Vec<String>>)>,
}

/// A template part, expanded.
enum Seg {
    Name(String),
    /// Lower-case literal runs and `*`s.
    Glob(Vec<GPart>),
}

enum GPart {
    Lit(String),
    Star,
}

/// One match: a program a rule names (or none), with everything a detection reports for it.
struct Cand {
    desc: usize,
    /// Its descriptor is Astra's own: a tie among its matches is sized without a bound.
    first_party: bool,
    code: EngineCode,
    confidence: Confidence,
    exe: Option<Vec<String>>,
    binary: Binary,
    evidence: Option<Vec<String>>,
    passes: bool,
    named_alike: bool,
    exe_pick: Option<ExePick>,
    bootstrap_matched: bool,
    tier: RuleTier,
    weak_order: u32,
    from_hint: bool,
    /// The version files, expanded; read only for the one chosen.
    version: Option<(Vec<Vec<String>>, Vec<Tok>)>,
}

impl Cand {
    /// How deep the game sits: its evidence's depth, else its program's.
    fn depth(&self) -> usize {
        self.evidence
            .as_ref()
            .or(self.exe.as_ref())
            .map_or(usize::MAX, Vec::len)
    }
}

enum Resolution {
    Nothing,
    One(Cand),
    Ambiguous(Vec<Cand>),
}

struct Game<'a> {
    root: &'a Path,
    entries: &'a [Entry],
    policy: Policy,
    /// Every folder's listing, as the walk saw it.
    walk_dirs: HashMap<&'a [String], Rc<Folder>>,
    /// The walk's files Pass B may take for a program, in walk order.
    programs: Vec<ProgramLike>,
    /// Every name in the walk, lower case, with whether it is a folder: the anchors' prefilter.
    names: HashSet<(String, bool)>,
    /// Folders read afresh, memoised: by path below the root; `None` when not a real folder.
    folders: Memo<Folder>,
    /// The names `child` looks through ignoring case, memoised: see [`Game::window`].
    windows: Memo<Vec<String>>,
    lookups: Cell<u32>,
    bytes: Cell<u64>,
    sizings: Cell<u32>,
    /// The walk stopped at its cap with more left to list.
    capped: bool,
    /// Names examined: walk entries, set lookups, and every name of a listing scanned. Not a
    /// bound — the budgets above are — but a measure, so a test can hold Pass B's work to its
    /// structural ceiling without a clock ([`pass_b_counted`]).
    work: Cell<u64>,
    hint: Option<&'a str>,
}

/// The descriptors of `set` as the evaluator asks them, each with its index.
fn members(set: &EngineSet) -> Vec<Member<'_>> {
    set.entries
        .iter()
        .enumerate()
        .map(|(desc, a)| Member {
            desc,
            model: &a.descriptor.model,
            first_party: a.origin.first_party,
        })
        .collect()
}

/// The rules of `members` that `keep` admits, as Pass A asks them: strong ones first, then
/// strong-fallback, then weak by `weak_order` — each in descriptor order, then rule order.
fn ordered<'m, 's>(
    members: &'m [Member<'s>],
    keep: impl Fn(&Rule) -> bool,
) -> Vec<(&'m Member<'s>, &'s Rule)> {
    let mut out: Vec<(&Member, &Rule)> = members
        .iter()
        .flat_map(|mb| mb.model.rules.iter().map(move |r| (mb, r)))
        .filter(|(_, r)| keep(r))
        .collect();
    // Stable: descriptor order, then rule order, within one tier and order.
    out.sort_by_key(|(_, r)| (r.tier, r.weak_order.unwrap_or(0)));
    out
}

/// Pass A: the built-in descriptors (`set`, see [`super::builtin`]) over a walk already made.
/// `None` when no rule names a game: the caller's one-program fallback answers then.
pub(crate) fn pass_a(
    root: &Path,
    hint: Option<&str>,
    entries: &[Entry],
    set: &EngineSet,
) -> Option<Detection> {
    let game = Game::new(root, entries, hint, Policy::BuiltIn);
    let c = game.builtin(&members(set))?;
    Some(game.builtin_detection(c, detect::anti_cheat_of(entries)))
}

/// The engine of the program `exe_rel` (below `root`) by the rules asked about it alone (the
/// module doc's "named program"): the built-in descriptors (`builtin`), and only when none of
/// their rules matches, `set`'s. `unknown` when nothing matches, or when the program's folder is
/// not a real folder below `root` (a link on its way is never followed).
pub(crate) fn engine_of_program(
    root: &Path,
    exe_rel: &[String],
    builtin: &EngineSet,
    set: &EngineSet,
) -> EngineCode {
    let Some((_, dir)) = exe_rel.split_last() else {
        return EngineCode::unknown();
    };
    let game = Game::new(root, &[], None, Policy::BuiltIn);
    let Some(folder) = game.folder(dir) else {
        return EngineCode::unknown();
    };
    let builtins = members(builtin);
    let mut spent = Spent::default();
    for (mb, rule) in ordered(&builtins, |r| r.scope != Scope::Anywhere) {
        if let Ok(Some(c)) = game.on_program(mb, rule, exe_rel, &folder, &mut spent) {
            return c.code;
        }
    }
    if set.is_empty() {
        return EngineCode::unknown();
    }
    // The set's descriptors, as Pass B would ask them about this one program: the first
    // standing with a match answers, and only with one code.
    let game = Game::new(root, &[], None, Policy::Descriptors);
    let described = members(set);
    for standing in [
        Moderation::Moderated,
        Moderation::Unchecked,
        Moderation::Local,
    ] {
        let mut found: BTreeSet<EngineCode> = BTreeSet::new();
        for mb in described
            .iter()
            .filter(|mb| set.entries[mb.desc].origin.moderation == standing)
        {
            let mut spent = Spent::default();
            for rule in mb.model.rules.iter().filter(|r| r.scope != Scope::Anywhere) {
                match game.on_program(mb, rule, exe_rel, &folder, &mut spent) {
                    Ok(Some(c)) => {
                        found.insert(c.code);
                    }
                    Ok(None) => {}
                    Err(Halt::Descriptor) => break,
                    Err(Halt::Game) => return EngineCode::unknown(),
                }
            }
        }
        match found.len() {
            0 => continue,
            1 => return found.into_iter().next().unwrap_or_else(EngineCode::unknown),
            _ => return EngineCode::unknown(),
        }
    }
    EngineCode::unknown()
}

/// Pass B over a game whose Pass A answer is `base` (`unknown`, no evidence, no hint used).
/// Returns `base` itself when no descriptor names the game, when the game goes over its
/// budget, and when `set` is empty.
pub(crate) fn pass_b(
    root: &Path,
    hint: Option<&str>,
    walk: &Walk,
    set: &EngineSet,
    base: Detection,
) -> Detection {
    pass_b_counted(root, hint, &walk.entries, walk.capped, set, base).0
}

/// [`pass_b`], with the names it examined ([`Game::work`]).
pub(crate) fn pass_b_counted(
    root: &Path,
    hint: Option<&str>,
    entries: &[Entry],
    capped: bool,
    set: &EngineSet,
    base: Detection,
) -> (Detection, u64) {
    if set.is_empty() || !base.open_to_descriptors() {
        return (base, 0);
    }
    let mut game = Game::new(root, entries, hint, Policy::Descriptors);
    game.capped = capped;
    let answer = game.answer(set, base);
    (answer, game.work.get())
}

impl<'a> Game<'a> {
    fn new(
        root: &'a Path,
        entries: &'a [Entry],
        hint: Option<&'a str>,
        policy: Policy,
    ) -> Game<'a> {
        let mut dirs: HashMap<&'a [String], Listing> = HashMap::new();
        let mut names = HashSet::new();
        let mut programs = Vec::new();
        for (at, e) in entries.iter().enumerate() {
            if let Some((name, parent)) = e.rel.split_last() {
                dirs.entry(parent)
                    .or_default()
                    .push((name.clone(), e.is_dir));
                let lower = name.to_ascii_lowercase();
                if !e.is_dir && program_like(&lower) {
                    programs.push(ProgramLike {
                        at,
                        lower: lower.clone(),
                    });
                }
                names.insert((lower, e.is_dir));
            }
        }
        Game {
            root,
            entries,
            policy,
            walk_dirs: dirs
                .into_iter()
                .map(|(k, v)| (k, Rc::new(Folder::new(v))))
                .collect(),
            programs,
            names,
            folders: RefCell::new(HashMap::new()),
            windows: RefCell::new(HashMap::new()),
            lookups: Cell::new(0),
            bytes: Cell::new(0),
            sizings: Cell::new(0),
            capped: false,
            work: Cell::new(entries.len() as u64),
            hint,
        }
    }

    fn builtin_policy(&self) -> bool {
        self.policy == Policy::BuiltIn
    }

    fn answer(&self, set: &EngineSet, base: Detection) -> Detection {
        for standing in [
            Moderation::Moderated,
            Moderation::Unchecked,
            Moderation::Local,
        ] {
            let members: Vec<Member> = members(set)
                .into_iter()
                .filter(|mb| set.entries[mb.desc].origin.moderation == standing)
                .collect();
            if members.is_empty() {
                continue;
            }
            let resolved = self
                .standing(&members)
                .and_then(|cands| self.resolve(cands));
            match resolved {
                Err(_) => return base,
                Ok(Resolution::Nothing) => continue,
                Ok(Resolution::One(c)) => return self.finish(set, base, c),
                Ok(Resolution::Ambiguous(cands)) => return self.ambiguous(set, base, cands),
            }
        }
        base
    }

    fn worked(&self, names: usize) {
        self.work.set(self.work.get() + names as u64);
    }

    /// `name` in a folder's listing, of the kind asked (`detect::find_in`: the exact spelling,
    /// else ignoring ASCII case): a set lookup first, the listing scanned only when it is there.
    fn find_in<'f>(&self, f: &'f Folder, name: &str, dir: bool) -> Option<&'f str> {
        self.worked(1);
        if !f.holds(name, dir) {
            return None;
        }
        detect::find_in(f.scan(&self.work), name, dir)
    }

    // ---- budgets (Pass B only: the built-ins are bounded by the walk alone) ----

    fn lookup(&self, spent: &mut Spent) -> Step<()> {
        if self.builtin_policy() {
            return Ok(());
        }
        spent.lookups += 1;
        if spent.lookups > limits::LOOKUPS_PER_DESCRIPTOR {
            return Err(Halt::Descriptor);
        }
        self.game_lookup()
    }

    fn game_lookup(&self) -> Step<()> {
        if self.builtin_policy() {
            return Ok(());
        }
        self.lookups.set(self.lookups.get() + 1);
        if self.lookups.get() > limits::LOOKUPS_PER_GAME {
            return Err(Halt::Game);
        }
        Ok(())
    }

    fn probe(&self, spent: &mut Spent, bytes: u64) -> Step<()> {
        if self.builtin_policy() {
            return Ok(());
        }
        spent.probes += 1;
        if spent.probes > limits::PROBES_PER_DESCRIPTOR {
            return Err(Halt::Descriptor);
        }
        self.read_bytes(bytes)
    }

    fn read_bytes(&self, bytes: u64) -> Step<()> {
        if self.builtin_policy() {
            return Ok(());
        }
        self.bytes.set(self.bytes.get() + bytes);
        if self.bytes.get() > limits::BYTES_PER_GAME {
            return Err(Halt::Game);
        }
        Ok(())
    }

    // ---- the filesystem, never through a link ----

    /// Whether every component of `comps` below the root is a real folder (no link on the way).
    fn real_dir(&self, comps: &[String]) -> bool {
        let mut p = self.root.to_path_buf();
        for c in comps {
            p.push(c);
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {}
                _ => return false,
            }
        }
        true
    }

    /// A folder read afresh — truncated before it is sorted, exactly as `detect::listing_of`
    /// — when it is a real folder. Memoised.
    fn folder(&self, comps: &[String]) -> Option<Rc<Folder>> {
        if let Some(v) = self.folders.borrow().get(comps) {
            return v.clone();
        }
        let v = if self.real_dir(comps) {
            detect::listing_of(&abs(self.root, comps)).map(|l| {
                self.worked(l.len());
                Rc::new(Folder::new(l))
            })
        } else {
            None
        };
        self.folders.borrow_mut().insert(comps.to_vec(), v.clone());
        v
    }

    /// What `child` looks through for a name spelled otherwise, when `comps` is a real folder:
    /// the names (UTF-8 ones) among the first [`MAX_ENTRIES`] a fresh `read_dir` returns, in
    /// its order, links and all — exactly `detect::child`'s. Memoised.
    fn window(&self, comps: &[String]) -> Option<Rc<Vec<String>>> {
        if let Some(v) = self.windows.borrow().get(comps) {
            return v.clone();
        }
        let v = if self.real_dir(comps) {
            fs::read_dir(abs(self.root, comps)).ok().map(|read| {
                let names: Vec<String> = read
                    .flatten()
                    .take(MAX_ENTRIES)
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .collect();
                self.worked(names.len());
                Rc::new(names)
            })
        } else {
            None
        };
        self.windows.borrow_mut().insert(comps.to_vec(), v.clone());
        v
    }

    // ---- templates ----

    fn name_value(v: NameVar, sc: &View) -> Option<String> {
        match v {
            NameVar::Name => Some(sc.prog?.name.clone()),
            NameVar::Stem => Some(detect::program_stem(&sc.prog?.name).to_owned()),
            NameVar::ProjectLeaf => sc.project.as_ref()?.last().cloned(),
        }
    }

    fn name_of(pieces: &[Piece], sc: &View) -> Option<String> {
        let mut s = String::new();
        for p in pieces {
            match p {
                Piece::Lit(l) => s.push_str(l),
                Piece::Var(v) => s.push_str(&Game::name_value(*v, sc)?),
                Piece::Star => return None,
            }
        }
        check_component(&s).then_some(s)
    }

    /// The template's base (below the root) and its own parts, expanded; `None` when a
    /// placeholder has no value here or a part expands to a name no folder may hold.
    fn expand(t: &PathT, sc: &View) -> Option<(Vec<String>, Vec<Seg>)> {
        let base = match &t.base {
            None | Some(PathVar::Dir) => sc.prog?.dir.clone(),
            Some(PathVar::Program) => sc.prog?.rel.clone(),
            Some(PathVar::Project) => sc.project.clone()?,
            Some(PathVar::ProjectParent) => {
                let p = sc.project.as_ref()?;
                p[..p.len().checked_sub(1)?].to_vec()
            }
            Some(PathVar::Capture(n)) => {
                sc.captures.iter().find(|(k, _)| k == n)?.1.first()?.clone()
            }
        };
        let mut segs = Vec::new();
        for seg in &t.segs {
            if seg.contains(&Piece::Star) {
                let mut parts = Vec::new();
                for p in seg {
                    parts.push(match p {
                        Piece::Lit(l) => GPart::Lit(l.to_ascii_lowercase()),
                        Piece::Var(v) => GPart::Lit(Game::name_value(*v, sc)?.to_ascii_lowercase()),
                        Piece::Star => GPart::Star,
                    });
                }
                segs.push(Seg::Glob(parts));
            } else {
                segs.push(Seg::Name(Game::name_of(seg, sc)?));
            }
        }
        Some((base, segs))
    }

    /// A template with no glob, as one path below the root.
    fn full_path(t: &PathT, sc: &View) -> Option<Vec<String>> {
        let (mut base, segs) = Game::expand(t, sc)?;
        for s in segs {
            match s {
                Seg::Name(n) => base.push(n),
                Seg::Glob(_) => return None,
            }
        }
        Some(base)
    }

    // ---- lookups ----

    /// `listed`: a name in a folder's listing — the program's own as the search saw it, any
    /// other (or, for a bootstrap, every one) read afresh. Never a wrapper's name.
    fn listed(
        &self,
        m: &Model,
        sc: &View,
        path: &PathT,
        kind: Kind,
        fresh: bool,
        spent: &mut Spent,
    ) -> Step<Option<Vec<String>>> {
        self.lookup(spent)?;
        let Some((mut parent, mut segs)) = Game::expand(path, sc) else {
            return Ok(None);
        };
        let Some(last) = segs.pop() else {
            return Ok(None);
        };
        for s in segs {
            match s {
                Seg::Name(n) => parent.push(n),
                Seg::Glob(_) => return Ok(None),
            }
        }
        let folder = match sc.prog {
            Some(prog) if !fresh && parent == prog.dir => prog.folder.clone(),
            _ => match self.folder(&parent) {
                Some(f) => f,
                None => return Ok(None),
            },
        };
        let want_dir = kind.is_dir();
        let found = match &last {
            Seg::Name(n) => self.find_in(&folder, n, want_dir).map(str::to_owned),
            Seg::Glob(g) => folder
                .scan(&self.work)
                .iter()
                .filter(|(n, d)| *d == want_dir && !m.is_wrapper(n))
                .find(|(n, _)| glob_match(g, &n.to_ascii_lowercase()))
                .map(|(n, _)| n.clone()),
        };
        Ok(found.filter(|n| !m.is_wrapper(n)).map(|n| {
            parent.push(n);
            parent
        }))
    }

    /// `child`: each part below the base resolved ignoring ASCII case, one at a time, each a
    /// real folder (the last a real folder or a regular file, as `kind` asks), never through a
    /// link. One lookup per part.
    fn child(
        &self,
        sc: &View,
        path: &PathT,
        kind: Kind,
        spent: &mut Spent,
    ) -> Step<Option<Vec<String>>> {
        let Some((base, segs)) =
            Game::expand(path, sc).filter(|(b, s)| !s.is_empty() && self.real_dir(b))
        else {
            self.lookup(spent)?;
            return Ok(None);
        };
        let mut real = base;
        let n = segs.len();
        for (i, s) in segs.iter().enumerate() {
            self.lookup(spent)?;
            let want_dir = i + 1 < n || kind.is_dir();
            let found = match s {
                Seg::Name(name) => self.child_named(&real, name, want_dir),
                Seg::Glob(g) => self.folder(&real).and_then(|f| {
                    f.scan(&self.work)
                        .iter()
                        .filter(|(_, d)| *d == want_dir)
                        .filter(|(n, _)| glob_match(g, &n.to_ascii_lowercase()))
                        .map(|(n, _)| n.clone())
                        .find(|n| self.is_kind(&real, n, want_dir))
                }),
            };
            match found {
                Some(f) => real.push(f),
                None => return Ok(None),
            }
        }
        Ok(Some(real))
    }

    /// `name` in the real folder `dir`, a real folder or a regular file as asked, never a link:
    /// the exact spelling, else the least (by byte order) of the names spelled otherwise among
    /// what [`Game::window`] holds — `detect::child`, the hint's jail, exactly.
    fn child_named(&self, dir: &[String], name: &str, want_dir: bool) -> Option<String> {
        if self.is_kind(dir, name, want_dir) {
            return Some(name.to_owned());
        }
        let window = self.window(dir)?;
        self.worked(window.len());
        window
            .iter()
            .filter(|n| n.eq_ignore_ascii_case(name) && self.is_kind(dir, n, want_dir))
            .min()
            .cloned()
    }

    fn is_kind(&self, dir: &[String], name: &str, want_dir: bool) -> bool {
        let mut p = abs(self.root, dir);
        p.push(name);
        fs::symlink_metadata(p).is_ok_and(|m| if want_dir { m.is_dir() } else { m.is_file() })
    }

    /// `stat`: the exact spelling, every component a real folder and the last of `kind`.
    fn stat(
        &self,
        sc: &View,
        path: &PathT,
        kind: Kind,
        spent: &mut Spent,
    ) -> Step<Option<Vec<String>>> {
        self.lookup(spent)?;
        let Some(full) = Game::full_path(path, sc) else {
            return Ok(None);
        };
        Ok(self.is_exact(&full, kind).then_some(full))
    }

    fn is_exact(&self, full: &[String], kind: Kind) -> bool {
        let Some((_, parent)) = full.split_last() else {
            return false;
        };
        if !self.real_dir(parent) {
            return false;
        }
        match fs::symlink_metadata(abs(self.root, full)) {
            Ok(m) => {
                if kind.is_dir() {
                    m.is_dir()
                } else {
                    m.is_file()
                }
            }
            Err(_) => false,
        }
    }

    /// The path of a regular file to read, its folders checked for links (the file itself is
    /// opened without following one).
    fn readable(&self, full: &[String]) -> Option<std::path::PathBuf> {
        let (_, parent) = full.split_last()?;
        self.real_dir(parent).then(|| abs(self.root, full))
    }

    fn tail(
        &self,
        sc: &View,
        path: &PathT,
        equals: &[u8],
        min_len: u64,
        spent: &mut Spent,
    ) -> Step<bool> {
        self.probe(spent, equals.len() as u64)?;
        let Some(p) = Game::full_path(path, sc).and_then(|f| self.readable(&f)) else {
            return Ok(false);
        };
        Ok(reads::tail(&p, equals.len(), min_len).is_some_and(|b| b == equals))
    }

    /// The program is what its name says: a PE image (`MZ`) for `.exe`, an ELF image for any
    /// other. One lookup, four bytes. Pass B only.
    fn magic(&self, prog: &Prog, spent: &mut Spent) -> Step<bool> {
        if self.builtin_policy() {
            return Ok(true);
        }
        self.lookup(spent)?;
        self.read_bytes(4)?;
        let want: &[u8] = if detect::ext_lower(&prog.name) == "exe" {
            b"MZ"
        } else {
            b"\x7fELF"
        };
        let Some(p) = self.readable(&prog.rel) else {
            return Ok(false);
        };
        Ok(reads::head(&p, 4).is_some_and(|b| b.starts_with(want)))
    }

    fn check(&self, m: &Model, c: &Check, sc: &mut View, spent: &mut Spent) -> Step<bool> {
        let (found, capture) = match c {
            Check::Listed {
                path,
                kind,
                capture,
            } => (self.listed(m, sc, path, *kind, false, spent)?, capture),
            Check::Child {
                path,
                kind,
                capture,
            } => (self.child(sc, path, *kind, spent)?, capture),
            Check::Stat {
                path,
                kind,
                capture,
            } => (self.stat(sc, path, *kind, spent)?, capture),
            Check::Tail {
                path,
                equals,
                min_len,
            } => return self.tail(sc, path, equals, *min_len, spent),
            Check::Name(glob) => {
                return Ok(sc
                    .prog
                    .is_some_and(|p| glob_match(&glob_parts(glob), &p.name.to_ascii_lowercase())));
            }
            Check::Any(list) => {
                for x in list {
                    if self.check(m, x, sc, spent)? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            Check::All(list) => {
                for x in list {
                    if !self.check(m, x, sc, spent)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            Check::None(list) => {
                for x in list {
                    if self.check(m, x, sc, spent)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
        };
        if let (Some(p), Some(name)) = (&found, capture) {
            sc.captures.push((name.clone(), vec![p.clone()]));
        }
        Ok(found.is_some())
    }

    // ---- programs ----

    /// Whether the file `name` (`lower`: the same, lower case) in `dir` is a program `rule`
    /// considers; with the `{project}` it captures. One set lookup for `also_if`, never a scan.
    fn fits(
        &self,
        m: &Model,
        rule: &Rule,
        dir: &[String],
        name: &str,
        lower: &str,
        folder: &Folder,
    ) -> Option<Option<Vec<String>>> {
        let p = rule.programs.as_ref()?;
        self.worked(1);
        let selected = selects_lower(p, lower)
            || p.also_if.as_ref().is_some_and(|t| {
                self.worked(1);
                also_if_name(t, name).is_some_and(|mut d| {
                    d.make_ascii_lowercase();
                    folder.holds_lower(&d, true)
                })
            });
        if !selected {
            return None;
        }
        if !self.where_ok(m, rule, dir) {
            return None;
        }
        if rule.skip.wrappers == WrapperSkip::All && m.is_wrapper(name) {
            return None;
        }
        match &p.at {
            None => Some(None),
            Some(at) => match_at(at, dir),
        }
    }

    /// Whether `rule` may look below the folders `dir`: none of them a wrapper's (unless it skips
    /// no wrappers), none named in its `skip_dirs`.
    fn where_ok(&self, m: &Model, rule: &Rule, dir: &[String]) -> bool {
        if rule.skip.wrappers != WrapperSkip::None && dir.iter().any(|c| m.is_wrapper(c)) {
            return false;
        }
        !dir.iter()
            .any(|c| rule.skip.dirs.iter().any(|s| c.eq_ignore_ascii_case(s)))
    }

    /// The programs a Pass B rule considers in the folder search: the first `programs.first` of
    /// the walk's program-like files, in walk order, that it fits.
    fn walk_programs(&self, m: &Model, rule: &Rule) -> Vec<Prog> {
        let first = rule
            .programs
            .as_ref()
            .and_then(|p| p.first)
            .unwrap_or(limits::FIRST) as usize;
        let mut out = Vec::new();
        for pl in &self.programs {
            let e = &self.entries[pl.at];
            let Some(prog) = self.walk_prog(m, rule, e, &pl.lower) else {
                continue;
            };
            out.push(prog);
            if out.len() >= first {
                break;
            }
        }
        out
    }

    /// The walk file `e` as a program `rule` considers, with the folder the walk listed.
    fn walk_prog(&self, m: &Model, rule: &Rule, e: &Entry, lower: &str) -> Option<Prog> {
        let (name, dir) = e.rel.split_last()?;
        let folder = self.walk_dirs.get(dir)?;
        let project = self.fits(m, rule, dir, name, lower, folder)?;
        Some(Prog {
            rel: e.rel.clone(),
            dir: dir.to_vec(),
            name: name.clone(),
            project,
            folder: folder.clone(),
            from_hint: false,
        })
    }

    /// Steam's launch hint as a program: jailed exactly as 0.4.1's `hinted()` (relative, no `..`,
    /// no link on the way, matched ignoring ASCII case), never a wrapper; for Pass B also only a
    /// program-like name. Its folder read afresh.
    fn hint_program(&self) -> Option<(Vec<String>, Rc<Folder>)> {
        let mut h = self.hint?.trim();
        while let Some(rest) = h.strip_prefix("./").or_else(|| h.strip_prefix(".\\")) {
            h = rest;
        }
        let wanted = relative_components(h)?;
        let mut real: Vec<String> = Vec::with_capacity(wanted.len());
        for (i, c) in wanted.iter().enumerate() {
            let is_last = i + 1 == wanted.len();
            real.push(detect::child(&abs(self.root, &real), c, !is_last)?);
        }
        let (name, parent) = real.split_last()?;
        if detect::is_wrapper_name(name)
            || (!self.builtin_policy() && !program_like(&name.to_ascii_lowercase()))
        {
            return None;
        }
        let folder = self.folder(parent)?;
        Some((real, folder))
    }

    // ---- one rule, one program ----

    fn rule_on(
        &self,
        mb: &Member,
        rule: &Rule,
        prog: &Prog,
        spent: &mut Spent,
    ) -> Step<Option<Cand>> {
        // The anchors before anything else: each is a literal `listed` name beside the program,
        // so absent from the listing already in memory it fails its own check anyway — and
        // here it costs nothing, wherever it sits in `require`, on Steam's hint as in the walk.
        self.worked(rule.anchors.len());
        if !rule
            .anchors
            .iter()
            .all(|(n, k)| prog.folder.holds_lower(n, k.is_dir()))
        {
            return Ok(None);
        }
        let mut sc = View {
            prog: Some(prog),
            project: prog.project.clone(),
            captures: Vec::new(),
        };
        for c in &rule.require {
            if !self.check(mb.model, c, &mut sc, spent)? {
                return Ok(None);
            }
        }
        if !self.magic(prog, spent)? {
            return Ok(None);
        }
        self.matched(mb, rule, sc, spent)
    }

    /// A rule whose `require` held, from its outcome on: the candidate, or `None` when its
    /// outcome names nothing.
    fn matched(
        &self,
        mb: &Member,
        rule: &Rule,
        mut sc: View,
        spent: &mut Spent,
    ) -> Step<Option<Cand>> {
        let m = mb.model;
        let (code, confidence) = match &rule.outcome {
            Outcome::Fixed(c, f) => (c.clone(), *f),
            Outcome::Classify(list) => {
                let mut chosen = None;
                for (when, c, f) in list {
                    let ok = match when {
                        None => true,
                        Some(w) => self.check(m, w, &mut sc, spent)?,
                    };
                    if ok {
                        chosen = Some((c.clone(), *f));
                        break;
                    }
                }
                match chosen {
                    Some(x) => x,
                    None => return Ok(None),
                }
            }
        };
        // A built-in rule that ends at `unknown` still names its program (a Unity player whose
        // data names neither flavour is the game); a descriptor's names nothing.
        if code.is_unknown() && !self.builtin_policy() {
            return Ok(None);
        }
        let exe_pick = match sc.prog {
            Some(prog) if rule.pick => match self.pick(m, rule, prog, spent)? {
                Some(p) => Some(p),
                None => return Ok(None),
            },
            _ => None,
        };
        let evidence = match &rule.evidence {
            None => None,
            Some(t) => match Game::full_path(t, &sc) {
                // Never the root: evidence says WHY, and the root says nothing.
                Some(e) if !e.is_empty() => Some(e),
                _ => return Ok(None),
            },
        };
        let binary = self.binary(&rule.binary, &sc);
        let named_alike = match (&rule.named_alike, sc.prog) {
            (NamedAlike::No, _) => false,
            (NamedAlike::Yes, _) => true,
            (NamedAlike::Template(t), Some(prog)) => Game::name_of(t, &sc)
                .is_some_and(|want| program_base(rule, &prog.name).eq_ignore_ascii_case(&want)),
            (NamedAlike::Template(_), None) => false,
        };
        let bootstrap_matched = match &rule.bootstrap {
            None => false,
            Some(b) => {
                let on_disk = match &b.listed {
                    Some(l) => self.listed(m, &sc, l, Kind::File, true, spent)?.is_some(),
                    None => false,
                };
                on_disk
                    || b.hint_stem
                        .as_ref()
                        .is_some_and(|t| self.hint_stem_matches(t, &sc))
            }
        };
        let version = rule.version.as_ref().map(|v| {
            (
                v.head
                    .iter()
                    .filter_map(|t| Game::full_path(t, &sc))
                    .collect(),
                v.template.clone(),
            )
        });
        Ok(Some(Cand {
            desc: mb.desc,
            first_party: mb.first_party,
            code,
            confidence,
            exe: sc.prog.map(|p| p.rel.clone()),
            binary,
            evidence,
            passes: rule.passes,
            named_alike,
            exe_pick,
            bootstrap_matched,
            tier: rule.tier,
            weak_order: rule.weak_order.unwrap_or(u32::MAX),
            from_hint: sc.prog.is_some_and(|p| p.from_hint),
            version,
        }))
    }

    /// `pick = "hint-else-largest"`: the program the hint (or the caller) names; else, of the
    /// programs the rule selects in that folder, only the largest (the first by name of equal
    /// sizes) answers — one lookup per program sized.
    fn pick(
        &self,
        m: &Model,
        rule: &Rule,
        prog: &Prog,
        spent: &mut Spent,
    ) -> Step<Option<ExePick>> {
        if prog.from_hint {
            return Ok(Some(ExePick::LaunchConfig));
        }
        let Some(p) = &rule.programs else {
            return Ok(None);
        };
        let names: Vec<&str> = prog
            .folder
            .scan(&self.work)
            .iter()
            .filter(|(n, d)| !*d && selects_lower(p, &n.to_ascii_lowercase()) && !m.is_wrapper(n))
            .map(|(n, _)| n.as_str())
            .collect();
        let folder = abs(self.root, &prog.dir);
        let mut best: Option<(u64, &str)> = None;
        for n in &names {
            self.lookup(spent)?;
            let size = fs::symlink_metadata(folder.join(n))
                .ok()
                .filter(fs::Metadata::is_file)
                .map_or(0, |md| md.len());
            if best.is_none_or(|(bs, bn)| size > bs || (size == bs && *n < bn)) {
                best = Some((size, n));
            }
        }
        Ok(match best {
            Some((_, n)) if n == prog.name => Some(if names.len() == 1 {
                ExePick::Only
            } else {
                ExePick::Largest
            }),
            _ => None,
        })
    }

    fn binary(&self, b: &BinaryRule, sc: &View) -> Binary {
        match b {
            BinaryRule::Windows => Binary::Windows,
            BinaryRule::Linux => Binary::Linux,
            // `.exe` is a Windows program; any other a rule takes for one is a Linux program.
            BinaryRule::ByExt => match sc.prog {
                Some(p) if detect::ext_lower(&p.name) == "exe" => Binary::Windows,
                Some(_) => Binary::Linux,
                None => Binary::Unknown,
            },
            // The first of the names the placeholder stands for whose last `-` part begins with
            // a key of the map.
            BinaryRule::FromName { var, map } => {
                let names: Vec<String> = match var {
                    PathVar::Capture(n) => sc
                        .captures
                        .iter()
                        .find(|(k, _)| k == n)
                        .map(|(_, all)| all.iter().filter_map(|p| p.last().cloned()).collect())
                        .unwrap_or_default(),
                    PathVar::Program => sc.prog.map(|p| p.name.clone()).into_iter().collect(),
                    _ => Vec::new(),
                };
                names
                    .iter()
                    .find_map(|name| {
                        let lower = name.to_ascii_lowercase();
                        let i = lower.rfind('-')?;
                        map.iter()
                            .find(|(k, _)| lower[i..].starts_with(k.as_str()))
                            .map(|(_, v)| *v)
                    })
                    .unwrap_or(Binary::Unknown)
            }
        }
    }

    fn hint_stem_matches(&self, t: &[Piece], sc: &View) -> bool {
        let Some(h) = self.hint else {
            return false;
        };
        if detect::is_wrapper_name(h.rsplit(['/', '\\']).next().unwrap_or(h)) {
            return false;
        }
        Game::name_of(t, sc).is_some_and(|want| detect::hint_stem(h).eq_ignore_ascii_case(&want))
    }

    // ---- Pass A: the built-in descriptors ----

    /// The built-ins' answer for this game (the module doc's three steps), before the
    /// one-program fallback.
    fn builtin(&self, members: &[Member]) -> Option<Cand> {
        let mut spent = Spent::default();
        // 1. Steam's hint: the first strong rule that matches it.
        if let Some((rel, folder)) = self.hint_program() {
            let strong = ordered(members, |r| {
                r.tier == RuleTier::Strong && r.scope == Scope::Beside
            });
            for (mb, rule) in strong {
                if let Ok(Some(c)) = self.on_program(mb, rule, &rel, &folder, &mut spent) {
                    return Some(c);
                }
            }
        }
        // 2. The folder search: strong and strong-fallback rules, the first per program.
        let rules = ordered(members, |r| r.tier != RuleTier::Weak);
        if let Some(c) = self.best_per_program(&rules) {
            return Some(c);
        }
        // 3. The weak rules, one `weak_order` at a time.
        let weak = ordered(members, |r| {
            r.tier == RuleTier::Weak && r.scope != Scope::Named
        });
        let mut orders: Vec<u32> = weak
            .iter()
            .map(|(_, r)| r.weak_order.unwrap_or(0))
            .collect();
        orders.dedup();
        for order in orders {
            let at: Vec<(&Member, &Rule)> = weak
                .iter()
                .filter(|(_, r)| r.weak_order.unwrap_or(0) == order)
                .copied()
                .collect();
            let beside: Vec<(&Member, &Rule)> = at
                .iter()
                .filter(|(_, r)| r.scope == Scope::Beside)
                .copied()
                .collect();
            if let Some(c) = self.best_per_program(&beside) {
                return Some(c);
            }
            for (mb, rule) in at.iter().filter(|(_, r)| r.scope == Scope::Anywhere) {
                if let Some(c) = self.first_anywhere(mb, rule) {
                    return Some(c);
                }
            }
        }
        None
    }

    /// The program `rel` that Steam's hint or the caller names, its folder read afresh, asked of
    /// one rule: `pick` takes it as it is.
    fn on_program(
        &self,
        mb: &Member,
        rule: &Rule,
        rel: &[String],
        folder: &Rc<Folder>,
        spent: &mut Spent,
    ) -> Step<Option<Cand>> {
        let Some((name, dir)) = rel.split_last() else {
            return Ok(None);
        };
        let lower = name.to_ascii_lowercase();
        let Some(project) = self.fits(mb.model, rule, dir, name, &lower, folder) else {
            return Ok(None);
        };
        let prog = Prog {
            rel: rel.to_vec(),
            dir: dir.to_vec(),
            name: name.clone(),
            project,
            folder: folder.clone(),
            from_hint: true,
        };
        self.rule_on(mb, rule, &prog, spent)
    }

    /// Every file of the walk asks `rules` in their order, and the first that matches speaks
    /// for it; of those matches, [`Game::resolve`]'s `best`.
    fn best_per_program(&self, rules: &[(&Member, &Rule)]) -> Option<Cand> {
        if rules.is_empty() {
            return None;
        }
        let mut spent = Spent::default();
        let mut taken = vec![0usize; rules.len()];
        let mut cands = Vec::new();
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            let lower = e.name().to_ascii_lowercase();
            for (i, (mb, rule)) in rules.iter().enumerate() {
                let first = rule
                    .programs
                    .as_ref()
                    .and_then(|p| p.first)
                    .map_or(usize::MAX, |f| f as usize);
                if taken[i] >= first {
                    continue;
                }
                let Some(prog) = self.walk_prog(mb.model, rule, e, &lower) else {
                    continue;
                };
                taken[i] += 1;
                if let Ok(Some(c)) = self.rule_on(mb, rule, &prog, &mut spent) {
                    cands.push(c);
                    break;
                }
            }
        }
        match self.resolve(cands) {
            Ok(Resolution::One(c)) => Some(c),
            _ => None,
        }
    }

    /// A rule of scope `anywhere`: the first match in walk order — of the programs it selects
    /// (the first `programs.first` of them), or, with no program, of the walk's entries its
    /// first `listed` names below some `{project}`.
    fn first_anywhere(&self, mb: &Member, rule: &Rule) -> Option<Cand> {
        let mut spent = Spent::default();
        if rule.programs.is_none() {
            return self.first_project(mb, rule, &mut spent);
        }
        let first = rule
            .programs
            .as_ref()
            .and_then(|p| p.first)
            .map_or(usize::MAX, |f| f as usize);
        let mut taken = 0;
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            if taken >= first {
                break;
            }
            let Some(prog) = self.walk_prog(mb.model, rule, e, &e.name().to_ascii_lowercase())
            else {
                continue;
            };
            taken += 1;
            if let Ok(Some(c)) = self.rule_on(mb, rule, &prog, &mut spent) {
                return Some(c);
            }
        }
        None
    }

    /// `programs = "none"`, scope `anywhere`: the first walk entry, in walk order, that the
    /// rule's first `listed` names below some `{project}` (never the root, never below a folder
    /// it skips) gives the project; the capture stands for every entry it names below that same
    /// project, in walk order. Then the rest of `require`, and the outcome.
    fn first_project(&self, mb: &Member, rule: &Rule, spent: &mut Spent) -> Option<Cand> {
        let Some(Check::Listed {
            path,
            kind,
            capture,
        }) = rule.require.first()
        else {
            return None;
        };
        let k = path.segs.len();
        let below = |e: &Entry| -> Option<Vec<String>> {
            if e.is_dir != kind.is_dir() || e.rel.len() <= k {
                return None;
            }
            let (project, tail) = e.rel.split_at(e.rel.len() - k);
            if !self.where_ok(mb.model, rule, project) {
                return None;
            }
            let sc = View {
                prog: None,
                project: Some(project.to_vec()),
                captures: Vec::new(),
            };
            let (_, segs) = Game::expand(path, &sc)?;
            segs.iter()
                .zip(tail)
                .all(|(s, n)| match s {
                    Seg::Name(want) => n.eq_ignore_ascii_case(want),
                    Seg::Glob(g) => glob_match(g, &n.to_ascii_lowercase()),
                })
                .then(|| project.to_vec())
        };
        let (at, project) = self
            .entries
            .iter()
            .enumerate()
            .find_map(|(i, e)| below(e).map(|p| (i, p)))?;
        let all: Vec<Vec<String>> = self.entries[at..]
            .iter()
            .filter(|e| below(e).as_ref() == Some(&project))
            .map(|e| e.rel.clone())
            .collect();
        let mut sc = View {
            prog: None,
            project: Some(project),
            captures: Vec::new(),
        };
        if let Some(name) = capture {
            sc.captures.push((name.clone(), all));
        }
        for c in &rule.require[1..] {
            if !self.check(mb.model, c, &mut sc, spent).ok()? {
                return None;
            }
        }
        self.matched(mb, rule, sc, spent).ok().flatten()
    }

    /// The detection a built-in match makes: Pass A's, the Unity version in `unity_version`
    /// (read only when the rule names an engine).
    fn builtin_detection(&self, c: Cand, anti_cheat: Vec<String>) -> Detection {
        let unity_version = if c.code.is_unknown() {
            None
        } else {
            self.read_version(&c)
        };
        Detection {
            engine: c.code,
            confidence: c.confidence,
            exe: c.exe.as_deref().map(rel_path),
            binary: c.binary,
            anti_cheat,
            evidence: c.evidence.as_deref().map(rel_path),
            hint_used: c.from_hint,
            unity_version,
            exe_pick: c.exe_pick,
            version: None,
            engine_candidates: Vec::new(),
            descriptor_sha256: None,
        }
    }

    /// The version a match's `version` rule reads: the first of its files whose head names one.
    fn read_version(&self, c: &Cand) -> Option<String> {
        let (heads, template) = c.version.as_ref()?;
        for h in heads {
            if self.read_bytes(limits::HEAD_BYTES).is_err() {
                return None;
            }
            let Some(p) = self.readable(h) else {
                continue;
            };
            if let Some(v) = reads::head(&p, limits::HEAD_BYTES)
                .as_deref()
                .and_then(|b| find_version(template, b))
            {
                return Some(v);
            }
        }
        None
    }

    // ---- Pass B: one descriptor ----

    /// Every match of descriptor `desc` on Steam's hint. No prefilter: the hint's folder may lie
    /// past the walk.
    fn on_hint(
        &self,
        mb: &Member,
        hint: &(Vec<String>, Rc<Folder>),
        spent: &mut Spent,
    ) -> Step<Vec<Cand>> {
        let (rel, folder) = hint;
        let mut out = Vec::new();
        for rule in &mb.model.rules {
            if let Some(c) = self.on_program(mb, rule, rel, folder, spent)? {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Every match of descriptor `desc` in the folder search. A rule whose anchors the walk
    /// does not hold is not tried, and costs nothing.
    fn on_walk(&self, mb: &Member, spent: &mut Spent) -> Step<Vec<Cand>> {
        let mut out = Vec::new();
        for rule in &mb.model.rules {
            self.worked(rule.anchors.len());
            if !rule
                .anchors
                .iter()
                .all(|(n, k)| self.names.contains(&(n.clone(), k.is_dir())))
            {
                continue;
            }
            if rule.programs.is_none() {
                continue;
            }
            for prog in self.walk_programs(mb.model, rule) {
                if let Some(c) = self.rule_on(mb, rule, &prog, spent)? {
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    /// Every match of one standing's descriptors: on the hint when any match there, else in
    /// the folder search. A descriptor over its budget contributes nothing; the game over its
    /// budget stops everything.
    fn standing(&self, members: &[Member]) -> Step<Vec<Cand>> {
        let mut spent: Vec<Spent> = members.iter().map(|_| Spent::default()).collect();
        let mut over = vec![false; members.len()];
        let mut out = Vec::new();
        if let Some(hint) = self.hint_program() {
            for (i, mb) in members.iter().enumerate() {
                match self.on_hint(mb, &hint, &mut spent[i]) {
                    Ok(c) => out.extend(c),
                    Err(Halt::Descriptor) => over[i] = true,
                    Err(Halt::Game) => return Err(Halt::Game),
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
        for (i, mb) in members.iter().enumerate() {
            if over[i] {
                continue;
            }
            match self.on_walk(mb, &mut spent[i]) {
                Ok(c) => out.extend(c),
                Err(Halt::Descriptor) => {}
                Err(Halt::Game) => return Err(Halt::Game),
            }
        }
        Ok(out)
    }

    // ---- choosing ----

    /// The module doc's choosing: Pass B's filters, then (one code, or the built-ins) `best`.
    fn resolve(&self, mut cands: Vec<Cand>) -> Step<Resolution> {
        if cands.is_empty() {
            return Ok(Resolution::Nothing);
        }
        if cands.iter().any(|c| c.from_hint) {
            cands.retain(|c| c.from_hint);
        }
        if cands.iter().any(|c| c.tier != RuleTier::Weak) {
            cands.retain(|c| c.tier != RuleTier::Weak);
        } else if let Some(w) = cands.iter().map(|c| c.weak_order).min() {
            cands.retain(|c| c.weak_order == w);
        }
        if let Some(d) = cands.iter().map(Cand::depth).min() {
            cands.retain(|c| c.depth() == d);
        }
        if cands.iter().any(|c| c.passes) {
            cands.retain(|c| c.passes);
        }
        if !self.builtin_policy() {
            let codes: BTreeSet<&EngineCode> = cands.iter().map(|c| &c.code).collect();
            if codes.len() > 1 {
                return Ok(Resolution::Ambiguous(cands));
            }
        }
        if cands.iter().any(|c| c.bootstrap_matched) {
            cands.retain(|c| c.bootstrap_matched);
        }
        // The same program found twice (two rules, two descriptors of one code): once.
        let mut seen: Vec<(Option<Rel>, Option<Rel>)> = Vec::new();
        cands.retain(|c| {
            let key = (c.exe.clone(), c.evidence.clone());
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });
        if cands.len() < 2 {
            return Ok(cands.pop().map_or(Resolution::Nothing, Resolution::One));
        }
        let sizes = self.sizes(&cands)?;
        let mut keyed: Vec<(u64, Cand)> = cands
            .into_iter()
            .map(|c| {
                let size = c
                    .evidence
                    .as_ref()
                    .and_then(|e| sizes.iter().find(|(k, _)| k == e))
                    .map_or(0, |(_, s)| *s);
                (size, c)
            })
            .collect();
        keyed.sort_by(|(sa, a), (sb, b)| {
            sb.cmp(sa)
                .then(b.named_alike.cmp(&a.named_alike))
                .then_with(|| a.exe.cmp(&b.exe))
        });
        Ok(keyed
            .into_iter()
            .next()
            .map_or(Resolution::Nothing, |(_, c)| Resolution::One(c)))
    }

    /// The bytes below each tied evidence, for the tie-break: a file's length costs one lookup
    /// of the game's; a folder is a bounded walk ([`detect::evidence_size`]). A tie among a
    /// first-party descriptor's matches sizes every folder, as Pass A's `best()` does; any other
    /// sizes them only when they number at most what is left of
    /// [`limits::SIZINGS_PER_GAME`] — else none at all, and the tie falls to the name, then the
    /// path. Empty: nothing sized.
    fn sizes(&self, cands: &[Cand]) -> Step<Vec<(Vec<String>, u64)>> {
        let mut evidences: Vec<(&Vec<String>, Option<fs::Metadata>)> = Vec::new();
        for e in cands.iter().filter_map(|c| c.evidence.as_ref()) {
            if !evidences.iter().any(|(k, _)| *k == e) {
                evidences.push((e, fs::symlink_metadata(abs(self.root, e)).ok()));
            }
        }
        let exempt = cands.iter().all(|c| c.first_party);
        let folders = evidences
            .iter()
            .filter(|(_, m)| m.as_ref().is_some_and(fs::Metadata::is_dir))
            .count() as u32;
        if !exempt && self.sizings.get() + folders > limits::SIZINGS_PER_GAME {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (e, m) in evidences {
            let size = match m {
                Some(m) if m.is_dir() => {
                    if !exempt {
                        self.sizings.set(self.sizings.get() + 1);
                    }
                    detect::evidence_size(&abs(self.root, e))
                }
                m => {
                    self.game_lookup()?;
                    m.filter(fs::Metadata::is_file).map_or(0, |m| m.len())
                }
            };
            out.push((e.clone(), size));
        }
        Ok(out)
    }

    /// The anti-cheat a Pass B answer carries: Pass A's markers; the descriptors' own markers,
    /// for their own claims; and `anti-cheat-unknown` when the walk stopped at its cap or a
    /// kernel driver (`*.sys`) ships — a list of known names cannot vouch for that folder.
    fn anti_cheat(&self, base: &[String], models: &[&Model]) -> Vec<String> {
        let mut out = base.to_vec();
        let mut add = |code: &str| {
            if !out.iter().any(|c| c == code) {
                out.push(code.to_owned());
            }
        };
        for m in models {
            for mk in &m.markers {
                if self.entries.iter().any(|e| {
                    e.is_dir == mk.kind.is_dir() && e.name().eq_ignore_ascii_case(&mk.name)
                }) {
                    add(&mk.code);
                }
            }
        }
        let driver = self
            .entries
            .iter()
            .any(|e| !e.is_dir && detect::ext_lower(e.name()) == "sys");
        if self.capped || driver {
            add(codes::ANTI_CHEAT_UNKNOWN);
        }
        out
    }

    fn finish(&self, set: &EngineSet, base: Detection, c: Cand) -> Detection {
        let admitted = &set.entries[c.desc];
        let version = self.read_version(&c);
        Detection {
            anti_cheat: self.anti_cheat(&base.anti_cheat, &[&admitted.descriptor.model]),
            engine: c.code,
            confidence: c.confidence,
            exe: c.exe.as_deref().map(rel_path),
            binary: c.binary,
            evidence: c.evidence.as_deref().map(rel_path),
            hint_used: c.from_hint,
            unity_version: None,
            exe_pick: c.exe_pick,
            version,
            engine_candidates: Vec::new(),
            descriptor_sha256: Some(admitted.descriptor.digest().to_owned()),
        }
    }

    /// `engine-ambiguous`: Pass A's answer, untouched but for the codes standing and the
    /// anti-cheat every one of their descriptors would have carried.
    fn ambiguous(&self, set: &EngineSet, base: Detection, cands: Vec<Cand>) -> Detection {
        let codes: BTreeSet<EngineCode> = cands.iter().map(|c| c.code.clone()).collect();
        let descs: BTreeSet<usize> = cands.iter().map(|c| c.desc).collect();
        let models: Vec<&Model> = descs
            .iter()
            .map(|d| &set.entries[*d].descriptor.model)
            .collect();
        Detection {
            anti_cheat: self.anti_cheat(&base.anti_cheat, &models),
            engine_candidates: codes.into_iter().collect(),
            ..base
        }
    }
}

/// The extension of a lower-case name (`detect::ext_lower`, without the allocation).
fn ext_of(lower: &str) -> &str {
    lower.rsplit_once('.').map_or("", |(_, e)| e)
}

/// Whether a file of this lower-case name may be a Pass B program: extension-less, or one of
/// [`PASS_B_PROGRAM_EXTS`].
fn program_like(lower: &str) -> bool {
    !lower.contains('.') || PASS_B_PROGRAM_EXTS.contains(&ext_of(lower))
}

/// Whether `p` selects the program whose name, lower case, is `lower`.
fn selects_lower(p: &Programs, lower: &str) -> bool {
    match &p.select {
        Select::Ext(list) => {
            let extensionless = !lower.contains('.');
            let ext = ext_of(lower);
            list.iter().any(|e| {
                if e.is_empty() {
                    extensionless
                } else {
                    !extensionless && ext == e
                }
            })
        }
        Select::Suffix(s) => lower.len() > s.len() && lower.ends_with(s.as_str()),
    }
}

/// What a program's name says beside its evidence: without a suffix the rule selects by, the
/// file stem.
fn program_base<'n>(rule: &Rule, name: &'n str) -> &'n str {
    if let Some(Programs {
        select: Select::Suffix(s),
        ..
    }) = &rule.programs
        && name.len() > s.len()
        && name.to_ascii_lowercase().ends_with(s.as_str())
        && name.is_char_boundary(name.len() - s.len())
    {
        return &name[..name.len() - s.len()];
    }
    detect::program_stem(name)
}

/// `also_if.dir`, for a program not yet chosen (only `{name}` and `{stem}` mean anything).
fn also_if_name(pieces: &[Piece], name: &str) -> Option<String> {
    let mut s = String::new();
    for p in pieces {
        match p {
            Piece::Lit(l) => s.push_str(l),
            Piece::Var(NameVar::Name) => s.push_str(name),
            Piece::Var(NameVar::Stem) => s.push_str(detect::program_stem(name)),
            _ => return None,
        }
    }
    check_component(&s).then_some(s)
}

/// Whether the program's folder `dir` is where `at` says; with the `{project}` it captures
/// (at least one part).
fn match_at(at: &At, dir: &[String]) -> Option<Option<Vec<String>>> {
    let t = at.tail.len();
    if at.project {
        if dir.len() < t + 1 {
            return None;
        }
        let (project, rest) = dir.split_at(dir.len() - t);
        rest.iter()
            .zip(&at.tail)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
            .then(|| Some(project.to_vec()))
    } else {
        (dir.len() == t
            && dir
                .iter()
                .zip(&at.tail)
                .all(|(a, b)| a.eq_ignore_ascii_case(b)))
        .then_some(None)
    }
}

/// A `name` check's glob as [`glob_match`] reads it.
fn glob_parts(pieces: &[Piece]) -> Vec<GPart> {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(l) => GPart::Lit(l.clone()),
            _ => GPart::Star,
        })
        .collect()
}

/// A `*` glob against a lower-case name: literal runs in order, the first anchored to the start
/// and the last to the end unless a `*` stands there.
fn glob_match(parts: &[GPart], name: &str) -> bool {
    let n = parts.len();
    let mut pos = 0;
    for (i, p) in parts.iter().enumerate() {
        let GPart::Lit(l) = p else {
            continue;
        };
        if i == 0 {
            if !name.starts_with(l.as_str()) {
                return false;
            }
            pos = l.len();
            if n == 1 {
                return name.len() == pos;
            }
        } else if i + 1 == n {
            return name.len() >= pos + l.len() && name.ends_with(l.as_str());
        } else {
            match name[pos..].find(l.as_str()) {
                Some(k) => pos += k + l.len(),
                None => return false,
            }
        }
    }
    true
}

/// The first match of a version template in `bytes`, never starting right after a digit:
/// digit runs greedy up to their maximum, never backtracking.
pub(crate) fn find_version(tokens: &[Tok], bytes: &[u8]) -> Option<String> {
    for start in 0..bytes.len() {
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            continue;
        }
        let mut i = start;
        let mut ok = true;
        for t in tokens {
            match t {
                Tok::Digits { min, max } => {
                    let n = bytes[i..]
                        .iter()
                        .take(usize::from(*max))
                        .take_while(|c| c.is_ascii_digit())
                        .count();
                    if n < usize::from(*min) {
                        ok = false;
                        break;
                    }
                    i += n;
                }
                Tok::Set(set) => {
                    if bytes.get(i).is_some_and(|b| set.contains(b)) {
                        i += 1;
                    } else {
                        ok = false;
                        break;
                    }
                }
                Tok::Lit(c) => {
                    if bytes.get(i) == Some(c) {
                        i += 1;
                    } else {
                        ok = false;
                        break;
                    }
                }
            }
        }
        if ok && i > start {
            return Some(String::from_utf8_lossy(&bytes[start..i]).into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::{Descriptor, Origin};

    fn entry(rel: &[&str], is_dir: bool) -> Entry {
        Entry {
            rel: rel.iter().map(|s| (*s).to_owned()).collect(),
            is_dir,
        }
    }

    fn unknown() -> Detection {
        Detection {
            engine: EngineCode::unknown(),
            confidence: Confidence::Low,
            exe: None,
            binary: Binary::Unknown,
            anti_cheat: Vec::new(),
            evidence: None,
            hint_used: false,
            unity_version: None,
            exe_pick: None,
            version: None,
            engine_candidates: Vec::new(),
            descriptor_sha256: None,
        }
    }

    /// "Slow detection", counted, never timed: the most Pass B admits (64 descriptors, four per
    /// publisher, sixteen rules each, every rule with `also_if`) over one folder of 4,990
    /// extension-less files, each a program by its kind and none selected — the shape that took
    /// 17 s when `also_if` scanned the folder's listing for every program. Each rule now
    /// examines each program once and asks one set for its `also_if` folder: linear in the walk.
    /// The walk is synthetic (nothing on disk is read: no program is ever selected).
    #[test]
    fn also_if_is_one_set_lookup_per_program_counted() {
        const PROGRAMS: usize = 4_990;
        let mut entries = vec![entry(&["big"], true), entry(&["zzanchor.idx"], false)];
        for i in 0..PROGRAMS {
            entries.push(entry(&["big", &format!("{i:05}")], false));
        }
        let rule = "[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], also_if = { dir = \"{name}_Data\" }, first = 8 }\nrequire = [ { listed = \"zzanchor.idx\", kind = \"file\" } ]\ncode = \"CODE\"\nconfidence = \"high\"\nevidence = \"{program}\"\nbinary = \"windows\"\n";
        let set = |n: usize| {
            EngineSet::build((0..n).map(|i| {
                let code = format!("work-{i}");
                let text = format!(
                    "schema = \"astra.engine/1\"\nengine = {{ codes = {{ {code} = \"Work {i}\" }} }}\n{}",
                    rule.replace("CODE", &code).repeat(limits::RULES)
                );
                (
                    Origin {
                        moderation: Moderation::Unchecked,
                        publisher: format!("p{}", i / limits::PER_PUBLISHER),
                        first_party: false,
                    },
                    Descriptor::from_toml(&text),
                )
            }))
        };
        // Per rule: its one anchor, then each program examined once and its `also_if` asked once.
        let per_rule = (1 + 2 * PROGRAMS) as u64;
        for n in [1, limits::PASS_B_DESCRIPTORS] {
            let set = set(n);
            assert_eq!(set.len(), n, "{:?}", set.skipped());
            let (got, work) = pass_b_counted(
                Path::new("/nonexistent/agl-counted"),
                None,
                &entries,
                false,
                &set,
                unknown(),
            );
            assert_eq!(got, unknown(), "no program is selected");
            assert_eq!(
                work,
                entries.len() as u64 + (n * limits::RULES) as u64 * per_rule,
                "{n} descriptors: Pass B's work must stay linear in the walk per rule"
            );
        }
    }

    /// 0.4.1's own reader of a Unity version (src/detect.rs `find_unity_version` at 73bf11e),
    /// verbatim: the oracle the template is held to.
    fn find_unity_version(bytes: &[u8]) -> Option<String> {
        fn digits(b: &[u8], i: usize, max: usize) -> usize {
            b[i..]
                .iter()
                .take(max)
                .take_while(|c| c.is_ascii_digit())
                .count()
        }
        for start in 0..bytes.len() {
            if start > 0 && bytes[start - 1].is_ascii_digit() {
                continue;
            }
            let mut i = start;
            let mut ok = true;
            for (max, sep) in [(4, Some(b'.')), (2, Some(b'.')), (3, None)] {
                let n = digits(bytes, i, max);
                if n == 0 {
                    ok = false;
                    break;
                }
                i += n;
                if let Some(sep) = sep {
                    if bytes.get(i) != Some(&sep) {
                        ok = false;
                        break;
                    }
                    i += 1;
                }
            }
            if !ok || !matches!(bytes.get(i), Some(b'a' | b'b' | b'f' | b'p' | b'x')) {
                continue;
            }
            i += 1;
            let n = digits(bytes, i, 3);
            if n == 0 {
                continue;
            }
            return Some(String::from_utf8_lossy(&bytes[start..i + n]).into_owned());
        }
        None
    }

    /// The template plan B writes for the built-in version reader finds exactly what Pass A's
    /// own reader finds — on headers made to trip a reader up.
    #[test]
    fn the_unity_template_reads_as_pass_a_does() {
        let tokens = vec![
            Tok::Digits { min: 1, max: 4 },
            Tok::Lit(b'.'),
            Tok::Digits { min: 1, max: 2 },
            Tok::Lit(b'.'),
            Tok::Digits { min: 1, max: 3 },
            Tok::Set(b"abfpx".to_vec()),
            Tok::Digits { min: 1, max: 3 },
        ];
        let mut seed: u64 = 0x5eed_1e55;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = b"0123456789..abfpxz\0 ";
        let mut checked = 0;
        for _ in 0..20_000 {
            let len = (next() % 40) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            assert_eq!(
                find_version(&tokens, &bytes),
                find_unity_version(&bytes),
                "{:?}",
                String::from_utf8_lossy(&bytes)
            );
            checked += usize::from(find_version(&tokens, &bytes).is_some());
        }
        assert!(
            checked > 50,
            "the fuzz must reach real versions ({checked})"
        );
        for real in [
            &b"\0\0\x16126000.3.1f1\0"[..],
            b"Bundle\0\0\0\0\x085.6.0\x002021.3.16f1\0",
            b"20211.3.4f1",
            b"1.2.3",
        ] {
            assert_eq!(find_version(&tokens, real), find_unity_version(real));
        }
    }

    /// The built-ins parse, are Astra's own, fit every limit, and are asked in 0.4.1's order:
    /// Unity, then Godot, then Unreal.
    #[test]
    fn the_built_ins_parse_and_are_asked_in_0_4_1s_order() {
        let set = crate::engines::builtin();
        let codes: Vec<Vec<&str>> = set
            .descriptors()
            .map(|(d, _)| d.codes().map(|(c, _)| c.as_str()).collect())
            .collect();
        assert_eq!(
            codes,
            [
                vec!["unity-il2cpp", "unity-mono"],
                vec!["godot"],
                vec!["unreal"]
            ]
        );
        for (d, o) in set.descriptors() {
            assert!(o.first_party && o.moderation == Moderation::Moderated);
            assert!(d.canonical_json().len() <= limits::DESCRIPTOR_BYTES);
            // Built-in codes are reserved: no set may admit them, the built-ins included.
            assert!(d.check_pass_b(true).is_err());
        }
        let paths: Vec<Vec<&str>> = set
            .list_at_limit()
            .iter()
            .map(|p| p.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(paths, [vec!["Binaries", "Win64"]]);
    }

    /// Planning's engine check asks a set's descriptors only when no built-in rule matches the
    /// program, the first standing with a match answering — with one code, or none.
    #[test]
    fn a_named_program_asks_the_set_after_the_built_ins() {
        let root = std::env::temp_dir().join(format!("agl-eval-named-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let put = |rel: &str, bytes: &[u8]| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        };
        put("game/bin/win64/cs2.exe", b"MZ\x90\0 a program");
        put("game/bin/win64/engine2.dll", b"MZ");
        put("game/core/pak01_dir.vpk", b"vpk");
        put("Tool.exe", b"MZ tool");
        put("Tool_Data/Managed/Assembly-CSharp.dll", b"MZ");
        put("UnityPlayer.dll", b"MZ");
        let text = |code: &str| {
            format!(
                "schema = \"astra.engine/1\"\nengine = {{ codes = {{ {code} = \"S {code}\" }} }}\n[[rule]]\ntier = \"strong\"\nprograms = {{ ext = [\"exe\"], at = \"{{project}}/bin/win64\", first = 8 }}\nrequire = [ {{ listed = \"engine2.dll\", kind = \"file\" }}, {{ child = \"{{project}}/core\", kind = \"dir\", as = \"core\" }} ]\ncode = \"{code}\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{{core}}\"\n"
            )
        };
        let set = |items: &[(&str, Moderation, &str)]| {
            EngineSet::build(items.iter().map(|(code, m, publisher)| {
                (
                    Origin {
                        moderation: *m,
                        publisher: (*publisher).to_owned(),
                        first_party: false,
                    },
                    Descriptor::from_toml(&text(code)),
                )
            }))
        };
        let rel = |s: &str| s.split('/').map(str::to_owned).collect::<Vec<_>>();
        let builtin = crate::engines::builtin();
        let ask = |exe: &str, set: &EngineSet| engine_of_program(&root, &rel(exe), builtin, set);

        let one = set(&[("source2", Moderation::Unchecked, "a")]);
        assert_eq!(
            ask("game/bin/win64/cs2.exe", &EngineSet::empty()),
            "unknown"
        );
        assert_eq!(ask("game/bin/win64/cs2.exe", &one), "source2");
        // A built-in rule that matches decides: the set is not asked.
        assert_eq!(ask("Tool.exe", &one), "unity-mono");
        // A lower standing never overrules a higher one; two codes of one standing are none.
        let two = set(&[
            ("source2", Moderation::Local, "a"),
            ("vapor", Moderation::Moderated, "b"),
        ]);
        assert_eq!(ask("game/bin/win64/cs2.exe", &two), "vapor");
        let tied = set(&[
            ("source2", Moderation::Unchecked, "a"),
            ("vapor", Moderation::Unchecked, "b"),
        ]);
        assert_eq!(ask("game/bin/win64/cs2.exe", &tied), "unknown");
        let _ = fs::remove_dir_all(&root);
    }

    /// The unshipped rule never takes a program named `-Win64-Shipping.exe`, even when the
    /// listing of its folder was cut before it (Steam's hint into a folder past 5,000 names): in
    /// 0.4.1 every shipping name went to the shipping rule, which takes none with nothing before
    /// the suffix.
    #[test]
    fn no_built_in_rule_takes_a_bare_shipping_name_on_the_hint() {
        let root = std::env::temp_dir().join(format!("agl-eval-bare-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, bytes) in [
            ("Proj/Binaries/Win64/-Win64-Shipping.exe", &b"MZ"[..]),
            ("Proj/Content/Paks/pakchunk0-Windows.pak", b"pak"),
        ] {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        }
        let rel: Vec<String> = ["Proj", "Binaries", "Win64", "-Win64-Shipping.exe"]
            .map(str::to_owned)
            .to_vec();
        let game = Game::new(&root, &[], None, Policy::BuiltIn);
        let members = members(crate::engines::builtin());
        let whole = game.folder(&rel[..3]).unwrap();
        let cut = Rc::new(Folder::new(Vec::new()));
        for folder in [whole, cut] {
            for (mb, rule) in ordered(&members, |r| r.tier == RuleTier::Strong) {
                let got = game.on_program(mb, rule, &rel, &folder, &mut Spent::default());
                assert!(matches!(got, Ok(None)), "a strong rule took {rel:?}");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// `child` looks through the first 5,000 names a `read_dir` returns, as 0.4.1's `child()`
    /// did, never more.
    #[test]
    fn childs_window_is_the_first_5000_names_read() {
        let root = std::env::temp_dir().join(format!("agl-eval-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for i in 0..MAX_ENTRIES + 10 {
            fs::write(root.join(format!("{i:05}.dat")), b"").unwrap();
        }
        let game = Game::new(&root, &[], None, Policy::BuiltIn);
        assert_eq!(game.window(&[]).map(|w| w.len()), Some(MAX_ENTRIES));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn globs_match_as_written() {
        let g = |s: &str| -> Vec<GPart> {
            let mut out = Vec::new();
            for (i, part) in s.split('*').enumerate() {
                if i > 0 {
                    out.push(GPart::Star);
                }
                if !part.is_empty() {
                    out.push(GPart::Lit(part.to_owned()));
                }
            }
            out
        };
        for (pat, name, want) in [
            ("*-x64-release.exe", "game-x64-release.exe", true),
            ("*-x64-release.exe", "-x64-release.exe", true),
            ("*-x64-release.exe", "game-x64-release.exe.bak", false),
            ("pak*.pak", "pakchunk0.pak", true),
            ("pak*.pak", "pak.pak", true),
            ("pak*.pak", "pa.pak", false),
            ("ab*ab", "ab", false),
            ("ab*ab", "abab", true),
            ("*x*", "box", true),
            ("*x*", "bo", false),
            ("a*b*c", "aXbYc", true),
            ("a*b*c", "acb", false),
            ("*", "anything", true),
        ] {
            assert_eq!(glob_match(&g(pat), name), want, "{pat} {name}");
        }
    }
}
