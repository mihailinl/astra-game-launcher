// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Pass B: the engine descriptors of an [`EngineSet`], asked about one game folder that Pass A
//! left `unknown` with no evidence and no hint used.
//!
//! **Order.** Standing by standing — moderated, then unchecked, then local — and the first
//! standing with any match decides: a lower one is never asked, so it can neither replace nor
//! make ambiguous a match above it. Within a standing, Steam's launch hint first (a descriptor
//! matching the hint wins over every folder-search match), then the folder search.
//!
//! **Choosing.** Among one standing's matches: those from the hint; then strong rules over weak
//! ones (and the lowest `weak_order` among weak ones); the shallowest (by evidence, else the
//! program); one that `passes` over one that does not. If more than one engine code is still
//! standing that is `engine-ambiguous`: nothing is preselected. One code: Pass A's own order —
//! a matching bootstrapper, the largest evidence (sized at most
//! [`limits::SIZINGS_PER_GAME`] times), the name repeating its evidence's, the first by path.
//!
//! **Bounds.** Everything is counted ([`limits`]): a descriptor over its own lookups or probes
//! yields nothing for this game; the game over its lookups, bytes or sizings gets no Pass B
//! answer at all. A rule whose anchors the walk does not hold costs nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;

use super::descriptor::{
    At, BinaryRule, Check, Kind, Model, NameVar, NamedAlike, PASS_B_PROGRAM_EXTS, PathT, PathVar,
    Piece, Programs, Rule, RuleTier, Select, Tok, limits,
};
use super::{Admitted, EngineCode, EngineSet, Moderation, reads};
use crate::detect::{
    self, Binary, Confidence, Detection, Entry, ExePick, MAX_ENTRIES, abs, rel_path,
};
use crate::error::codes;
use crate::paths::{check_component, relative_components};

type Listing = Vec<(String, bool)>;

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
    /// hint).
    listing: Rc<Listing>,
    from_hint: bool,
}

/// One rule's view while it is evaluated: its program and what its checks captured.
struct Scope<'p> {
    prog: &'p Prog,
    captures: Vec<(String, Vec<String>)>,
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

/// One match: a program a rule names, with everything Pass B reports for it.
struct Cand {
    desc: usize,
    code: EngineCode,
    confidence: Confidence,
    exe: Vec<String>,
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
    fn depth(&self) -> usize {
        self.evidence.as_ref().unwrap_or(&self.exe).len()
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
    /// Every folder's listing, as the walk saw it.
    walk_dirs: HashMap<&'a [String], Rc<Listing>>,
    /// Every name in the walk, lower case, with whether it is a folder: the anchors' prefilter.
    names: HashSet<(String, bool)>,
    /// Folders read afresh, memoised: by path below the root; `None` when not a real folder.
    folders: RefCell<HashMap<Vec<String>, Option<Rc<Listing>>>>,
    lookups: Cell<u32>,
    bytes: Cell<u64>,
    sizings: Cell<u32>,
    hint: Option<&'a str>,
}

/// Pass B over a game whose Pass A answer is `base` (`unknown`, no evidence, no hint used).
/// Returns `base` itself when no descriptor names the game, when the game goes over its
/// budget, and when `set` is empty.
pub(crate) fn pass_b(
    root: &Path,
    hint: Option<&str>,
    entries: &[Entry],
    set: &EngineSet,
    base: Detection,
) -> Detection {
    if set.is_empty() || !base.open_to_descriptors() {
        return base;
    }
    let game = Game::new(root, entries, hint);
    for standing in [
        Moderation::Moderated,
        Moderation::Unchecked,
        Moderation::Local,
    ] {
        let members: Vec<(usize, &Admitted)> = set
            .entries
            .iter()
            .enumerate()
            .filter(|(_, a)| a.origin.moderation == standing)
            .collect();
        if members.is_empty() {
            continue;
        }
        let resolved = game
            .standing(&members)
            .and_then(|cands| game.resolve(cands));
        match resolved {
            Err(_) => return base,
            Ok(Resolution::Nothing) => continue,
            Ok(Resolution::One(c)) => return game.finish(set, base, c),
            Ok(Resolution::Ambiguous(cands)) => return game.ambiguous(set, base, cands),
        }
    }
    base
}

impl<'a> Game<'a> {
    fn new(root: &'a Path, entries: &'a [Entry], hint: Option<&'a str>) -> Game<'a> {
        let mut dirs: HashMap<&'a [String], Listing> = HashMap::new();
        let mut names = HashSet::new();
        for e in entries {
            if let Some((name, parent)) = e.rel.split_last() {
                dirs.entry(parent)
                    .or_default()
                    .push((name.clone(), e.is_dir));
                names.insert((name.to_ascii_lowercase(), e.is_dir));
            }
        }
        Game {
            root,
            entries,
            walk_dirs: dirs.into_iter().map(|(k, v)| (k, Rc::new(v))).collect(),
            names,
            folders: RefCell::new(HashMap::new()),
            lookups: Cell::new(0),
            bytes: Cell::new(0),
            sizings: Cell::new(0),
            hint,
        }
    }

    // ---- budgets ----

    fn lookup(&self, spent: &mut Spent) -> Step<()> {
        spent.lookups += 1;
        if spent.lookups > limits::LOOKUPS_PER_DESCRIPTOR {
            return Err(Halt::Descriptor);
        }
        self.lookups.set(self.lookups.get() + 1);
        if self.lookups.get() > limits::LOOKUPS_PER_GAME {
            return Err(Halt::Game);
        }
        Ok(())
    }

    fn probe(&self, spent: &mut Spent, bytes: u64) -> Step<()> {
        spent.probes += 1;
        if spent.probes > limits::PROBES_PER_DESCRIPTOR {
            return Err(Halt::Descriptor);
        }
        self.read_bytes(bytes)
    }

    fn read_bytes(&self, bytes: u64) -> Step<()> {
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

    /// A folder read afresh — truncated before it is sorted, exactly as Pass A's own
    /// `listing_of` — when it is a real folder. Memoised.
    fn folder(&self, comps: &[String]) -> Option<Rc<Listing>> {
        if let Some(v) = self.folders.borrow().get(comps) {
            return v.clone();
        }
        let v = if self.real_dir(comps) {
            detect::listing_of(&abs(self.root, comps)).map(Rc::new)
        } else {
            None
        };
        self.folders.borrow_mut().insert(comps.to_vec(), v.clone());
        v
    }

    // ---- templates ----

    fn name_value(v: NameVar, sc: &Scope) -> Option<String> {
        match v {
            NameVar::Name => Some(sc.prog.name.clone()),
            NameVar::Stem => Some(detect::program_stem(&sc.prog.name).to_owned()),
            NameVar::ProjectLeaf => sc.prog.project.as_ref()?.last().cloned(),
        }
    }

    fn name_of(pieces: &[Piece], sc: &Scope) -> Option<String> {
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
    fn expand(t: &PathT, sc: &Scope) -> Option<(Vec<String>, Vec<Seg>)> {
        let prog = sc.prog;
        let base = match &t.base {
            None | Some(PathVar::Dir) => prog.dir.clone(),
            Some(PathVar::Program) => prog.rel.clone(),
            Some(PathVar::Project) => prog.project.clone()?,
            Some(PathVar::ProjectParent) => {
                let p = prog.project.as_ref()?;
                p[..p.len().checked_sub(1)?].to_vec()
            }
            Some(PathVar::Capture(n)) => sc.captures.iter().find(|(k, _)| k == n)?.1.clone(),
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
    fn full_path(t: &PathT, sc: &Scope) -> Option<Vec<String>> {
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
        sc: &Scope,
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
        let listing = if !fresh && parent == sc.prog.dir {
            sc.prog.listing.clone()
        } else {
            match self.folder(&parent) {
                Some(l) => l,
                None => return Ok(None),
            }
        };
        let want_dir = kind.is_dir();
        let found = match &last {
            Seg::Name(n) => detect::find_in(&listing, n, want_dir).map(str::to_owned),
            Seg::Glob(g) => listing
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
    /// link. One lookup per part: a part that is not spelled exactly is looked for in its
    /// folder's listing, read once per game.
    fn child(
        &self,
        sc: &Scope,
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
                Seg::Glob(g) => self.folder(&real).and_then(|l| {
                    l.iter()
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

    /// `name` in the real folder `dir`: the exact spelling, else the listing's first spelling
    /// ignoring ASCII case — a real folder or a regular file as asked, never a link.
    fn child_named(&self, dir: &[String], name: &str, want_dir: bool) -> Option<String> {
        if self.is_kind(dir, name, want_dir) {
            return Some(name.to_owned());
        }
        let listing = self.folder(dir)?;
        detect::find_in(&listing, name, want_dir)
            .filter(|n| self.is_kind(dir, n, want_dir))
            .map(str::to_owned)
    }

    fn is_kind(&self, dir: &[String], name: &str, want_dir: bool) -> bool {
        let mut p = abs(self.root, dir);
        p.push(name);
        fs::symlink_metadata(p).is_ok_and(|m| if want_dir { m.is_dir() } else { m.is_file() })
    }

    /// `stat`: the exact spelling, every component a real folder and the last of `kind`.
    fn stat(
        &self,
        sc: &Scope,
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
        sc: &Scope,
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
    /// other. One lookup, four bytes.
    fn magic(&self, prog: &Prog, spent: &mut Spent) -> Step<bool> {
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

    fn check(&self, m: &Model, c: &Check, sc: &mut Scope, spent: &mut Spent) -> Step<bool> {
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
            sc.captures.push((name.clone(), p.clone()));
        }
        Ok(found.is_some())
    }

    // ---- programs ----

    /// Whether `name` in `dir` is a program `p` considers; with the `{project}` it captures.
    fn fits(
        m: &Model,
        p: &Programs,
        dir: &[String],
        name: &str,
        listing: &Listing,
    ) -> Option<Option<Vec<String>>> {
        let ext = detect::ext_lower(name);
        let extensionless = !name.contains('.');
        if !extensionless && !PASS_B_PROGRAM_EXTS.contains(&ext.as_str()) {
            return None;
        }
        let selected = selects(p, name)
            || p.also_if.as_ref().is_some_and(|t| {
                also_if_name(t, name).is_some_and(|d| detect::find_in(listing, &d, true).is_some())
            });
        if !selected {
            return None;
        }
        if p.skip_wrappers && (m.is_wrapper(name) || dir.iter().any(|c| m.is_wrapper(c))) {
            return None;
        }
        if dir
            .iter()
            .any(|c| p.skip_dirs.iter().any(|s| c.eq_ignore_ascii_case(s)))
        {
            return None;
        }
        match &p.at {
            None => Some(None),
            Some(at) => match_at(at, dir),
        }
    }

    /// The programs a rule considers in the folder search: the first `programs.first` of the
    /// walk's files, in walk order, that it fits.
    fn walk_programs(&self, m: &Model, p: &Programs) -> Vec<Prog> {
        let first = p.first.unwrap_or(limits::FIRST) as usize;
        let mut out = Vec::new();
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            let Some((name, dir)) = e.rel.split_last() else {
                continue;
            };
            let Some(listing) = self.walk_dirs.get(dir) else {
                continue;
            };
            if let Some(project) = Game::fits(m, p, dir, name, listing) {
                out.push(Prog {
                    rel: e.rel.clone(),
                    dir: dir.to_vec(),
                    name: name.clone(),
                    project,
                    listing: listing.clone(),
                    from_hint: false,
                });
                if out.len() >= first {
                    break;
                }
            }
        }
        out
    }

    /// Steam's launch hint as a program: jailed exactly as Pass A's (relative, no `..`, no link
    /// on the way, matched ignoring ASCII case), never a wrapper; its folder read afresh.
    fn hint_program(&self) -> Option<(Vec<String>, Rc<Listing>)> {
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
        if detect::is_wrapper_name(name) {
            return None;
        }
        let listing = self.folder(parent)?;
        Some((real, listing))
    }

    // ---- one rule, one program ----

    fn rule_on(
        &self,
        m: &Model,
        desc: usize,
        rule: &Rule,
        prog: &Prog,
        spent: &mut Spent,
    ) -> Step<Option<Cand>> {
        let mut sc = Scope {
            prog,
            captures: Vec::new(),
        };
        for c in &rule.require {
            if !self.check(m, c, &mut sc, spent)? {
                return Ok(None);
            }
        }
        if !self.magic(prog, spent)? {
            return Ok(None);
        }
        let (code, confidence) = match &rule.outcome {
            super::descriptor::Outcome::Fixed(c, f) => (c.clone(), *f),
            super::descriptor::Outcome::Classify(list) => {
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
        if code.is_unknown() {
            return Ok(None);
        }
        let exe_pick = if rule.pick {
            match self.pick(m, rule, prog, spent)? {
                Some(p) => Some(p),
                None => return Ok(None),
            }
        } else {
            None
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
        let named_alike = match &rule.named_alike {
            NamedAlike::No => false,
            NamedAlike::Yes => true,
            NamedAlike::Template(t) => Game::name_of(t, &sc)
                .is_some_and(|want| program_base(rule, &prog.name).eq_ignore_ascii_case(&want)),
        };
        let bootstrap_matched = match &rule.bootstrap {
            None => false,
            Some(b) => {
                self.listed(m, &sc, &b.listed, Kind::File, true, spent)?
                    .is_some()
                    || self.hint_stem_matches(&b.hint_stem, &sc)
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
            desc,
            code,
            confidence,
            exe: prog.rel.clone(),
            binary,
            evidence,
            passes: rule.passes,
            named_alike,
            exe_pick,
            bootstrap_matched,
            tier: rule.tier,
            weak_order: rule.weak_order.unwrap_or(u32::MAX),
            from_hint: prog.from_hint,
            version,
        }))
    }

    /// `pick = "hint-else-largest"`: the program the hint names; else, of the programs the
    /// rule selects in that folder, only the largest (the first by name of equal sizes)
    /// answers — one lookup per program sized.
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
            .listing
            .iter()
            .filter(|(n, d)| !*d && selects(p, n) && !m.is_wrapper(n))
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

    fn binary(&self, b: &BinaryRule, sc: &Scope) -> Binary {
        match b {
            BinaryRule::Windows => Binary::Windows,
            BinaryRule::Linux => Binary::Linux,
            BinaryRule::ByExt => {
                let name = &sc.prog.name;
                match detect::ext_lower(name).as_str() {
                    "exe" => Binary::Windows,
                    "x86_64" | "x86" => Binary::Linux,
                    _ if !name.contains('.') => Binary::Linux,
                    _ => Binary::Unknown,
                }
            }
            BinaryRule::FromName { var, map } => {
                let t = PathT {
                    base: Some(var.clone()),
                    segs: Vec::new(),
                };
                let Some(name) = Game::full_path(&t, sc).and_then(|p| p.last().cloned()) else {
                    return Binary::Unknown;
                };
                let lower = name.to_ascii_lowercase();
                let Some(i) = lower.rfind('-') else {
                    return Binary::Unknown;
                };
                map.iter()
                    .find(|(k, _)| lower[i..].starts_with(k.as_str()))
                    .map_or(Binary::Unknown, |(_, v)| *v)
            }
        }
    }

    fn hint_stem_matches(&self, t: &[Piece], sc: &Scope) -> bool {
        let Some(h) = self.hint else {
            return false;
        };
        if detect::is_wrapper_name(h.rsplit(['/', '\\']).next().unwrap_or(h)) {
            return false;
        }
        Game::name_of(t, sc).is_some_and(|want| detect::hint_stem(h).eq_ignore_ascii_case(&want))
    }

    // ---- one descriptor ----

    /// Every match of descriptor `desc` on Steam's hint. No prefilter: the hint's folder may lie
    /// past the walk.
    fn on_hint(
        &self,
        desc: usize,
        m: &Model,
        hint: &(Vec<String>, Rc<Listing>),
        spent: &mut Spent,
    ) -> Step<Vec<Cand>> {
        let (rel, listing) = hint;
        let Some((name, dir)) = rel.split_last() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for rule in &m.rules {
            let Some(p) = &rule.programs else {
                continue;
            };
            let Some(project) = Game::fits(m, p, dir, name, listing) else {
                continue;
            };
            let prog = Prog {
                rel: rel.clone(),
                dir: dir.to_vec(),
                name: name.clone(),
                project,
                listing: listing.clone(),
                from_hint: true,
            };
            if let Some(c) = self.rule_on(m, desc, rule, &prog, spent)? {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Every match of descriptor `desc` in the folder search. A rule whose anchors the walk
    /// does not hold is not tried, and costs nothing.
    fn on_walk(&self, desc: usize, m: &Model, spent: &mut Spent) -> Step<Vec<Cand>> {
        let mut out = Vec::new();
        for rule in &m.rules {
            if !rule
                .anchors
                .iter()
                .all(|(n, k)| self.names.contains(&(n.clone(), k.is_dir())))
            {
                continue;
            }
            let Some(p) = &rule.programs else {
                continue;
            };
            for prog in self.walk_programs(m, p) {
                if let Some(c) = self.rule_on(m, desc, rule, &prog, spent)? {
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    /// Every match of one standing's descriptors: on the hint when any match there, else in
    /// the folder search. A descriptor over its budget contributes nothing; the game over its
    /// budget stops everything.
    fn standing(&self, members: &[(usize, &Admitted)]) -> Step<Vec<Cand>> {
        let mut spent: Vec<Spent> = members.iter().map(|_| Spent::default()).collect();
        let mut over = vec![false; members.len()];
        let mut out = Vec::new();
        if let Some(hint) = self.hint_program() {
            for (i, (desc, a)) in members.iter().enumerate() {
                match self.on_hint(*desc, &a.descriptor.model, &hint, &mut spent[i]) {
                    Ok(c) => out.extend(c),
                    Err(Halt::Descriptor) => over[i] = true,
                    Err(Halt::Game) => return Err(Halt::Game),
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
        for (i, (desc, a)) in members.iter().enumerate() {
            if over[i] {
                continue;
            }
            match self.on_walk(*desc, &a.descriptor.model, &mut spent[i]) {
                Ok(c) => out.extend(c),
                Err(Halt::Descriptor) => {}
                Err(Halt::Game) => return Err(Halt::Game),
            }
        }
        Ok(out)
    }

    // ---- choosing ----

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
        let codes: BTreeSet<&EngineCode> = cands.iter().map(|c| &c.code).collect();
        if codes.len() > 1 {
            return Ok(Resolution::Ambiguous(cands));
        }
        if cands.iter().any(|c| c.bootstrap_matched) {
            cands.retain(|c| c.bootstrap_matched);
        }
        // The same program found twice (two rules, two descriptors of one code): once.
        let mut seen: Vec<(Vec<String>, Option<Vec<String>>)> = Vec::new();
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
        let mut sizes: Vec<(Vec<String>, u64)> = Vec::new();
        let mut keyed: Vec<(u64, Cand)> = Vec::new();
        for c in cands {
            let size = match &c.evidence {
                None => 0,
                Some(e) => match sizes.iter().find(|(k, _)| k == e) {
                    Some((_, s)) => *s,
                    None => {
                        let s = self.size_of(e)?;
                        sizes.push((e.clone(), s));
                        s
                    }
                },
            };
            keyed.push((size, c));
        }
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

    /// The bytes below one evidence path, for a tie-break: a file's length costs one lookup of
    /// the game's; a folder is a bounded walk, one of the game's [`limits::SIZINGS_PER_GAME`].
    fn size_of(&self, evidence: &[String]) -> Step<u64> {
        let p = abs(self.root, evidence);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => {
                self.sizings.set(self.sizings.get() + 1);
                if self.sizings.get() > limits::SIZINGS_PER_GAME {
                    return Err(Halt::Game);
                }
                Ok(detect::evidence_size(&p))
            }
            m => {
                self.lookups.set(self.lookups.get() + 1);
                if self.lookups.get() > limits::LOOKUPS_PER_GAME {
                    return Err(Halt::Game);
                }
                Ok(m.ok().filter(fs::Metadata::is_file).map_or(0, |m| m.len()))
            }
        }
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
        let capped = self.entries.len() >= MAX_ENTRIES;
        let driver = self
            .entries
            .iter()
            .any(|e| !e.is_dir && detect::ext_lower(e.name()) == "sys");
        if capped || driver {
            add(codes::ANTI_CHEAT_UNKNOWN);
        }
        out
    }

    fn finish(&self, set: &EngineSet, base: Detection, c: Cand) -> Detection {
        let admitted = &set.entries[c.desc];
        let version = c.version.as_ref().and_then(|(heads, template)| {
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
        });
        Detection {
            anti_cheat: self.anti_cheat(&base.anti_cheat, &[&admitted.descriptor.model]),
            engine: c.code,
            confidence: c.confidence,
            exe: Some(rel_path(&c.exe)),
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

fn selects(p: &Programs, name: &str) -> bool {
    match &p.select {
        Select::Ext(list) => {
            let extensionless = !name.contains('.');
            let ext = detect::ext_lower(name);
            list.iter().any(|e| {
                if e.is_empty() {
                    extensionless
                } else {
                    !extensionless && ext == *e
                }
            })
        }
        Select::Suffix(s) => {
            let lower = name.to_ascii_lowercase();
            lower.len() > s.len() && lower.ends_with(s.as_str())
        }
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
                crate::detect::find_unity_version(&bytes),
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
            assert_eq!(
                find_version(&tokens, real),
                crate::detect::find_unity_version(real)
            );
        }
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
