// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! One mods evaluation (engine-descriptor-v1.md §7.4-§7.8): which loaders of a valid `mods`
//! are in one source, their versions, their mods and which of them are enabled, and how Astra
//! can run beside each.
//!
//! **The mods context** (§7.4): `{source}` is the jail. A path is resolved below it one
//! component at a time and never through a link (a symbolic link, a junction, any name
//! surrogate: `symlink_metadata`, and a listing's own entry type, never followed); a probe opens
//! only a regular file ([`crate::engines::reads::open_regular`]). `listed` reads the listing of
//! the folder its path names (the exact spelling, else the least name that matches ignoring
//! ASCII case); `child` resolves each component ignoring ASCII case (the exact spelling first);
//! `stat` is the exact spelling. A listing is read once per folder per evaluation, memoised,
//! and so is whether a folder on the way is a real one. The root, a `version`'s file and an
//! `enable-list`'s list resolve as `child` does.
//!
//! **Bounds** (§7.8, per evaluation: one game, one source; counted, never timed): at most
//! [`limits::ENTRIES_PER_DIR`] entries listed in any one folder, [`limits::MODS_PER_ROOT`] mods
//! in a root, [`limits::LOOKUPS_PER_EVALUATION`] lookups (every `require` lookup, every folder
//! listed, and a `marker` check in every mod folder, enabled by the list or not) and
//! [`limits::BYTES_PER_EVALUATION`] read (a `version` probe at most [`limits::VERSION_BYTES`], a
//! list at most [`limits::LIST_BYTES`]). Over any, that loader yields nothing for that source —
//! [`LoaderOutcome::OverBudget`], never a partial list, and never a version "unknown" for a probe
//! cut short — and the start is without mods. A start evaluates at most one source of each kind
//! ([`Sources`]): the game folder, the ACTIVE profile of the one manager an adapter reports, the
//! picked folder. A caller that evaluates many sources (the launcher's discovery lists every
//! profile it finds) charges each evaluation's listed entries and bytes read to a [`Meter`] of
//! its own as well ([`evaluate_one_within`]); spent, the loader yields nothing the same way
//! ([`Bound::Discovery`]).
//!
//! **Read-only and private** (§7.10): nothing here opens anything for writing, and nothing it
//! finds is logged; what it returns is for the player's own install sheet and that one start.
//! A runner is handed a chain's `entry` and `tell`, never the list of mods.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::pe;
use super::{Bindings, Coexist, Enabled, Kind, Loader, MCheck, ModsPath, SourceKind, Tell, limits};
use crate::engines::EngineCode;
use crate::engines::reads;

/// The bound a loader went over in one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// A folder listed more than [`limits::ENTRIES_PER_DIR`] entries.
    Entries,
    /// The root held more than [`limits::MODS_PER_ROOT`] mods.
    Mods,
    /// More than [`limits::LOOKUPS_PER_EVALUATION`] lookups.
    Lookups,
    /// More than [`limits::BYTES_PER_EVALUATION`] read, or a list longer than
    /// [`limits::LIST_BYTES`].
    Bytes,
    /// The caller's own [`Meter`], shared by every evaluation it makes, is spent (the launcher's,
    /// beside §7.8's per-evaluation bounds).
    Discovery,
}

impl Bound {
    pub fn code(self) -> &'static str {
        match self {
            Bound::Entries => "entries",
            Bound::Mods => "mods",
            Bound::Lookups => "lookups",
            Bound::Bytes => "bytes",
            Bound::Discovery => "discovery",
        }
    }
}

/// One of the player's mods in a loader's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mod {
    /// Its name in the root, as spelled on disk.
    pub name: String,
    /// A folder (`dir-per-mod`, `enable-list`), else a file (`file-glob`).
    pub folder: bool,
    /// Whether a format of the loader's `enabled` says it is enabled.
    pub enabled: bool,
}

/// How Astra can run beside a recognised loader, resolved in this source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    /// Astra's runner hands control to the player's loader at `entry` (a file the loader's
    /// recognition found, below the source), loaded in place.
    Chain {
        entry: PathBuf,
        tell: Option<Tell>,
    },
    SideBySide {
        tell: Option<Tell>,
    },
    ViaManager,
}

/// A loader recognised in one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognised {
    pub id: String,
    pub name: String,
    /// The version its `version` probe read (the value of the first key whose value reads in
    /// the template), `None` when unknown or not asked.
    pub version: Option<String>,
    /// What the same probe found as written, for the player to read: the value of the first key
    /// that has one, whether or not it reads in the template (`6.0.0-dev+3a12d18`, which is no
    /// `-be.N` build). Never what decides support: that is [`Recognised::version`].
    pub written: Option<String>,
    /// Whether its `versions` hold (`true` when it declares none). Recognised but unsupported:
    /// the player is told which loader was found and what Astra needs ([`Recognised::need`]),
    /// and offered to play without mods.
    pub supported: bool,
    /// `versions` as written, when declared.
    pub need: Option<String>,
    /// Its mods, by name.
    pub mods: Vec<Mod>,
    /// Its mods root as found below the source (its spelling on disk), when it is there.
    pub root: Option<PathBuf>,
    /// The files its top-level `file` lookups found, as spelled on disk: what recognised it.
    pub files: Vec<PathBuf>,
    /// `coexist`, in order of preference; empty: Astra cannot run beside it.
    pub coexist: Vec<Strategy>,
}

/// What one loader of a `mods` came to in one source. A loader that does not apply (another
/// source kind, another code), whose placeholder the placing rule did not bind, or that is not
/// recognised there yields nothing and is not listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoaderOutcome {
    Recognised(Recognised),
    /// Over a per-evaluation bound: it yields nothing for this source, and the start is
    /// without mods.
    OverBudget {
        id: String,
        name: String,
        bound: Bound,
    },
}

/// One source's evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub source: SourceKind,
    pub loaders: Vec<LoaderOutcome>,
}

/// The sources one start reads (§7.8: at most one of each kind). Which folder each is — the
/// game folder, the active profile an adapter reports for this game (never another of its
/// profiles), the folder the player picked (read as `manager` when it is inside a manager's
/// data folder, §7.7) — is the caller's: a descriptor never chooses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    pub game: Option<PathBuf>,
    pub manager: Option<PathBuf>,
    pub picked: Option<PathBuf>,
}

/// Every source of `sources`, in the order game, manager, picked, evaluated for a game a
/// descriptor placed under `code` with `bindings`; each with its own bounds.
pub fn evaluate(
    mods: &super::Mods,
    code: &EngineCode,
    bindings: &Bindings,
    sources: &Sources,
) -> Vec<Evaluation> {
    [
        (SourceKind::Game, &sources.game),
        (SourceKind::Manager, &sources.manager),
        (SourceKind::Picked, &sources.picked),
    ]
    .into_iter()
    .filter_map(|(kind, root)| Some(evaluate_one(mods, code, bindings, kind, root.as_deref()?)))
    .collect()
}

/// What a caller may still spend over all the evaluations it makes, beside each one's own §7.8
/// bounds: entries listed and bytes read. Every entry an evaluation lists and every byte it reads
/// is charged; once it is spent, the loader being evaluated yields nothing
/// ([`Bound::Discovery`]).
pub trait Meter {
    /// One more entry listed; `false` once spent.
    fn entry(&self) -> bool;
    /// `n` more bytes read; `false` (and nothing charged) when they do not fit.
    fn bytes(&self, n: u64) -> bool;
    /// What may still be read.
    fn bytes_left(&self) -> u64;
}

/// No meter: each evaluation has its own bounds only.
struct Unmetered;

impl Meter for Unmetered {
    fn entry(&self) -> bool {
        true
    }

    fn bytes(&self, _: u64) -> bool {
        true
    }

    fn bytes_left(&self) -> u64 {
        u64::MAX
    }
}

/// One source of kind `kind` at `root`.
pub fn evaluate_one(
    mods: &super::Mods,
    code: &EngineCode,
    bindings: &Bindings,
    kind: SourceKind,
    root: &Path,
) -> Evaluation {
    evaluate_one_within(mods, code, bindings, kind, root, &Unmetered)
}

/// [`evaluate_one`], every entry listed and byte read charged to `meter` as well.
pub fn evaluate_one_within(
    mods: &super::Mods,
    code: &EngineCode,
    bindings: &Bindings,
    kind: SourceKind,
    root: &Path,
    meter: &dyn Meter,
) -> Evaluation {
    let mut ctx = Ctx {
        root,
        lookups: 0,
        bytes: 0,
        listings: HashMap::new(),
        dirs: HashMap::new(),
        meter,
    };
    let mut loaders = Vec::new();
    if fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        for l in mods.loaders() {
            if !l.sources.contains(&kind) || !l.applies_to(code) {
                continue;
            }
            let Some(paths) = Paths::of(l, bindings) else {
                continue;
            };
            match ctx.loader(l, &paths) {
                Ok(Some(r)) => loaders.push(LoaderOutcome::Recognised(r)),
                Ok(None) => {}
                Err(bound) => loaders.push(LoaderOutcome::OverBudget {
                    id: l.id.clone(),
                    name: l.name.clone(),
                    bound,
                }),
            }
        }
    }
    Evaluation {
        source: kind,
        loaders,
    }
}

type Step<T> = Result<T, Bound>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EKind {
    Dir,
    File,
    Other,
}

/// One folder's entries, sorted by name; their kinds as listed (a link is [`EKind::Other`]).
type Listing = Vec<(String, EKind)>;

/// Every path of one loader, substituted (§7.3), keyed by its text.
struct Paths(BTreeMap<String, Vec<String>>);

impl Paths {
    /// `None` when any of the loader's paths does not substitute: the loader yields nothing.
    fn of(l: &Loader, b: &Bindings) -> Option<Paths> {
        fn checks<'l>(c: &'l [MCheck], out: &mut Vec<&'l ModsPath>) {
            for c in c {
                match c {
                    MCheck::Listed { path, .. }
                    | MCheck::Child { path, .. }
                    | MCheck::Stat { path, .. }
                    | MCheck::Tail { path, .. } => out.push(path),
                    MCheck::Any(x) | MCheck::All(x) | MCheck::None(x) => checks(x, out),
                }
            }
        }
        let mut all = vec![&l.root];
        checks(&l.require, &mut all);
        all.extend(l.version.iter().map(|v| &v.pe));
        for e in &l.enabled {
            if let Enabled::EnableList { list, .. } = e {
                all.push(list);
            }
        }
        for s in &l.coexist {
            if let Coexist::Chain { entry, .. } = s {
                all.push(entry);
            }
        }
        let mut out = BTreeMap::new();
        for p in all {
            out.insert(p.as_str().to_owned(), p.substitute(b)?);
        }
        Some(Paths(out))
    }

    fn get(&self, p: &ModsPath) -> &[String] {
        &self.0[p.as_str()]
    }
}

struct Ctx<'a> {
    root: &'a Path,
    lookups: u32,
    bytes: u64,
    /// Folders below the root, listed: `None` when not a real folder; `Err` when over the bound.
    listings: HashMap<Vec<String>, Step<Option<Rc<Listing>>>>,
    /// Whether each folder below the root asked about (`/`-joined) is a real one, its own
    /// folders real: each is looked at once per evaluation, so a deep path costs one look per
    /// component, never one per component per lookup.
    dirs: HashMap<String, bool>,
    /// The caller's, charged with every entry listed and byte read.
    meter: &'a dyn Meter,
}

impl Ctx<'_> {
    fn abs(&self, comps: &[String]) -> PathBuf {
        comps.iter().fold(self.root.to_path_buf(), |p, c| p.join(c))
    }

    fn lookup(&mut self) -> Step<()> {
        self.lookups += 1;
        if self.lookups > limits::LOOKUPS_PER_EVALUATION {
            return Err(Bound::Lookups);
        }
        Ok(())
    }

    /// `n` bytes read: charged to the evaluation and to the caller's meter.
    fn read(&mut self, n: u64) -> Step<()> {
        self.bytes += n;
        if self.bytes > limits::BYTES_PER_EVALUATION {
            return Err(Bound::Bytes);
        }
        if !self.meter.bytes(n) {
            return Err(Bound::Discovery);
        }
        Ok(())
    }

    /// The most a probe bounded by `own` may read: what is left of the evaluation's bytes and of
    /// the meter's, and whether it is the meter that bounds it.
    fn room(&self, own: u64) -> (u64, bool) {
        let left = own.min(limits::BYTES_PER_EVALUATION.saturating_sub(self.bytes));
        let shared = self.meter.bytes_left();
        (left.min(shared), shared < left)
    }

    /// Whether every component of `comps` is a real folder below the root (no link on the way).
    fn real_dir(&mut self, comps: &[String]) -> bool {
        let mut key = String::new();
        let mut p = self.root.to_path_buf();
        for c in comps {
            key.push('/');
            key.push_str(c);
            p.push(c);
            let real = match self.dirs.get(&key) {
                Some(r) => *r,
                None => {
                    let r = fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
                    self.dirs.insert(key.clone(), r);
                    r
                }
            };
            if !real {
                return false;
            }
        }
        true
    }

    /// Whether `comps` names a real folder or a regular file (as `kind` asks), its folders real.
    fn is_kind(&mut self, comps: &[String], kind: Kind) -> bool {
        let Some((_, parent)) = comps.split_last() else {
            return kind.is_dir();
        };
        self.real_dir(parent)
            && fs::symlink_metadata(self.abs(comps)).is_ok_and(|m| {
                if kind.is_dir() {
                    m.is_dir()
                } else {
                    m.is_file()
                }
            })
    }

    /// The folder `comps`, listed once per evaluation: one lookup, at most
    /// [`limits::ENTRIES_PER_DIR`] entries.
    fn listing(&mut self, comps: &[String]) -> Step<Option<Rc<Listing>>> {
        if let Some(v) = self.listings.get(comps) {
            return v.clone();
        }
        let v = if self.real_dir(comps) {
            self.lookup()?;
            self.read_listing(comps)
        } else {
            Ok(None)
        };
        self.listings.insert(comps.to_vec(), v.clone());
        v
    }

    fn read_listing(&self, comps: &[String]) -> Step<Option<Rc<Listing>>> {
        let Ok(read) = fs::read_dir(self.abs(comps)) else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for (n, item) in read.flatten().enumerate() {
            if n >= limits::ENTRIES_PER_DIR {
                return Err(Bound::Entries);
            }
            if !self.meter.entry() {
                return Err(Bound::Discovery);
            }
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let kind = match item.file_type() {
                Ok(t) if t.is_symlink() => EKind::Other,
                Ok(t) if t.is_dir() => EKind::Dir,
                Ok(t) if t.is_file() => EKind::File,
                _ => EKind::Other,
            };
            out.push((name, kind));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Some(Rc::new(out)))
    }

    /// `name` in the listing of `dir`, of `kind`: the exact spelling, else the least that
    /// matches ignoring ASCII case.
    fn in_listing(&mut self, dir: &[String], name: &str, kind: Kind) -> Step<Option<String>> {
        let want = if kind.is_dir() {
            EKind::Dir
        } else {
            EKind::File
        };
        let Some(l) = self.listing(dir)? else {
            return Ok(None);
        };
        Ok(l.iter()
            .find(|(n, k)| *k == want && n == name)
            .or_else(|| {
                l.iter()
                    .find(|(n, k)| *k == want && n.eq_ignore_ascii_case(name))
            })
            .map(|(n, _)| n.clone()))
    }

    /// `comps` resolved one component at a time ignoring ASCII case, the exact spelling first,
    /// every folder on the way real, the last of `kind`.
    fn resolve(&mut self, comps: &[String], kind: Kind) -> Step<Option<Vec<String>>> {
        let mut real: Vec<String> = Vec::new();
        for (i, c) in comps.iter().enumerate() {
            let k = if i + 1 < comps.len() { Kind::Dir } else { kind };
            let mut exact = real.clone();
            exact.push(c.clone());
            if self.is_kind(&exact, k) {
                real = exact;
                continue;
            }
            match self.in_listing(&real, c, k)? {
                Some(found) => real.push(found),
                None => return Ok(None),
            }
        }
        if comps.is_empty() && !kind.is_dir() {
            return Ok(None);
        }
        Ok(Some(real))
    }

    /// One `require` entry: whether it holds. A top-level file lookup that holds is recorded in
    /// `found` (its text → where it was found), for a chain's `entry`.
    fn check(
        &mut self,
        c: &MCheck,
        paths: &Paths,
        top: bool,
        found: &mut BTreeMap<String, Vec<String>>,
    ) -> Step<bool> {
        let (path, kind, hit) = match c {
            MCheck::Listed { path, kind } => {
                self.lookup()?;
                let comps = paths.get(path);
                let hit = match comps.split_last() {
                    Some((last, parent)) if self.real_dir(parent) => self
                        .in_listing(parent, last, *kind)?
                        .map(|n| [parent, &[n]].concat()),
                    _ => None,
                };
                (path, *kind, hit)
            }
            MCheck::Child { path, kind } => {
                self.lookup()?;
                (path, *kind, self.resolve(paths.get(path), *kind)?)
            }
            MCheck::Stat { path, kind } => {
                self.lookup()?;
                let comps = paths.get(path);
                (
                    path,
                    *kind,
                    self.is_kind(comps, *kind).then(|| comps.to_vec()),
                )
            }
            MCheck::Tail {
                path,
                equals,
                min_len,
            } => {
                self.read(equals.len() as u64)?;
                let comps = paths.get(path);
                let ok = match comps.split_last() {
                    Some((_, parent)) if self.real_dir(parent) => {
                        reads::tail(&self.abs(comps), equals.len(), *min_len)
                            .is_some_and(|b| b == *equals)
                    }
                    _ => false,
                };
                return Ok(ok);
            }
            MCheck::Any(list) => {
                for x in list {
                    if self.check(x, paths, false, found)? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            MCheck::All(list) => {
                for x in list {
                    if !self.check(x, paths, false, found)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            MCheck::None(list) => {
                for x in list {
                    if self.check(x, paths, false, found)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
        };
        if let Some(h) = &hit
            && top
            && !kind.is_dir()
        {
            found
                .entry(path.as_str().to_owned())
                .or_insert_with(|| h.clone());
        }
        Ok(hit.is_some())
    }

    /// One loader in this source: `None` when it is not recognised there.
    fn loader(&mut self, l: &Loader, paths: &Paths) -> Step<Option<Recognised>> {
        let mut found = BTreeMap::new();
        for c in &l.require {
            if !self.check(c, paths, true, &mut found)? {
                return Ok(None);
            }
        }
        let (version, written, supported) = self.version(l, paths)?;
        let (root, mods) = self.mods(l, paths)?;
        let coexist = l
            .coexist
            .iter()
            .filter_map(|s| {
                Some(match s {
                    Coexist::Chain { entry, tell } => Strategy::Chain {
                        entry: self.abs(found.get(entry.as_str())?),
                        tell: *tell,
                    },
                    Coexist::SideBySide { tell } => Strategy::SideBySide { tell: *tell },
                    Coexist::ViaManager => Strategy::ViaManager,
                })
            })
            .collect();
        Ok(Some(Recognised {
            id: l.id.clone(),
            name: l.name.clone(),
            version,
            written,
            supported,
            need: l
                .version
                .as_ref()
                .and_then(|v| v.versions.as_ref())
                .map(|v| v.as_str().to_owned()),
            mods,
            root: root.map(|r| self.abs(&r)),
            files: found.values().map(|f| self.abs(f)).collect(),
            coexist,
        }))
    }

    /// The loader's version, what its probe found as written, and whether its `versions` hold.
    fn version(
        &mut self,
        l: &Loader,
        paths: &Paths,
    ) -> Step<(Option<String>, Option<String>, bool)> {
        let Some(v) = &l.version else {
            return Ok((None, None, true));
        };
        let mut read = None;
        let mut written = None;
        if let Some(comps) = self.resolve(paths.get(&v.pe), Kind::File)?
            && let Some((f, len)) = reads::open_regular(&self.abs(&comps))
        {
            let (room, metered) = self.room(limits::VERSION_BYTES);
            let mut r = pe::Counted::new(f, len, room);
            let strings = pe::version_strings(&mut r, &v.keys);
            self.read(r.used)?;
            if r.exhausted {
                // A probe that would read past its own bound, or past what is left of the
                // evaluation's (or the meter's): over a bound, never a version "unknown" (§7.8).
                return Err(if metered {
                    Bound::Discovery
                } else {
                    Bound::Bytes
                });
            }
            let strings: Vec<String> = strings.into_iter().flatten().flatten().collect();
            written = strings.first().cloned();
            read = strings
                .into_iter()
                .find_map(|s| v.template.read(&s).map(|(runs, _)| (s, runs)));
        }
        let supported = match (&v.versions, &read) {
            (None, _) => true,
            (Some(want), Some((_, runs))) => want.holds(runs),
            (Some(_), None) => false,
        };
        Ok((read.map(|(s, _)| s), written, supported))
    }

    /// The loader's root as found (its components below the source), and the mods in it, each
    /// with whether it is enabled.
    fn mods(&mut self, l: &Loader, paths: &Paths) -> Step<(Option<Vec<String>>, Vec<Mod>)> {
        let Some(root) = self.resolve(paths.get(&l.root), Kind::Dir)? else {
            return Ok((None, Vec::new()));
        };
        let Some(listing) = self.listing(&root)? else {
            return Ok((Some(root), Vec::new()));
        };
        let folders = l
            .enabled
            .iter()
            .any(|e| matches!(e, Enabled::DirPerMod | Enabled::EnableList { .. }));
        let mut out = Vec::new();
        for (name, kind) in listing.iter() {
            let m = match kind {
                EKind::Dir if folders => Mod {
                    name: name.clone(),
                    folder: true,
                    enabled: l.enabled.iter().any(|e| matches!(e, Enabled::DirPerMod)),
                },
                EKind::File
                    if l.enabled
                        .iter()
                        .any(|e| matches!(e, Enabled::FileGlob(g) if g.matches(name))) =>
                {
                    Mod {
                        name: name.clone(),
                        folder: false,
                        enabled: true,
                    }
                }
                _ => continue,
            };
            out.push(m);
            if out.len() > limits::MODS_PER_ROOT {
                return Err(Bound::Mods);
            }
        }
        for e in &l.enabled {
            let Enabled::EnableList { list, marker } = e else {
                continue;
            };
            let lines = self.list(paths.get(list))?;
            for m in out.iter_mut().filter(|m| m.folder) {
                let listed = lines
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&m.name))
                    .is_some_and(|(_, on)| *on);
                // One marker check per mod folder, whatever the list says (§7.8).
                let marked = match marker {
                    Some(marker) => {
                        self.lookup()?;
                        let mut p = root.clone();
                        p.push(m.name.clone());
                        p.push(marker.clone());
                        self.is_kind(&p, Kind::File)
                    }
                    None => false,
                };
                m.enabled |= listed || marked;
            }
        }
        Ok((Some(root), out))
    }

    /// An `enable-list`'s `name-colon-flag` lines: `(name, enabled)`, in order. A missing list
    /// is no lines; one longer than [`limits::LIST_BYTES`] is over the bound.
    fn list(&mut self, comps: &[String]) -> Step<Vec<(String, bool)>> {
        let Some(found) = self.resolve(comps, Kind::File)? else {
            return Ok(Vec::new());
        };
        let (room, metered) = self.room(limits::LIST_BYTES);
        let Some(bytes) = reads::head(&self.abs(&found), room + 1) else {
            return Ok(Vec::new());
        };
        let n = bytes.len() as u64;
        if metered && n > room {
            return Err(Bound::Discovery);
        }
        self.read(n)?;
        if n > limits::LIST_BYTES {
            return Err(Bound::Bytes);
        }
        Ok(name_colon_flag(&String::from_utf8_lossy(&bytes)))
    }
}

/// UE4SS's `mods.txt` (§7.5): one mod per line, `<name> : 1` or `<name> : 0`, spaces around the
/// colon optional. A line that is empty, starts with `;`, or does not read this way is skipped.
pub(crate) fn name_colon_flag(text: &str) -> Vec<(String, bool)> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                return None;
            }
            let (name, flag) = line.rsplit_once(':')?;
            let name = name.trim();
            let on = match flag.trim() {
                "1" => true,
                "0" => false,
                _ => return None,
            };
            (!name.is_empty()).then(|| (name.to_owned(), on))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::Descriptor;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agl-mods-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    const HEAD: &str = "schema = \"astra.engine/1\"\nengine = { codes = { probe = \"Probe\" } }\n[[rule]]\ntier = \"strong\"\nprograms = { ext = [\"exe\"], at = \"{project}/bin\", first = 8 }\nrequire = [ { listed = \"probe.dat\", kind = \"file\" } ]\ncode = \"probe\"\nconfidence = \"high\"\nbinary = \"windows\"\nevidence = \"{project}\"\n";

    fn mods_of(loaders: &str) -> super::super::Mods {
        let d = Descriptor::from_toml(&format!("{HEAD}{loaders}")).unwrap();
        assert_eq!(d.mods_refusals(), [], "{loaders}");
        d.mods().unwrap().clone()
    }

    fn code() -> EngineCode {
        EngineCode::new("probe").unwrap()
    }

    fn game(mods: &super::super::Mods, root: &Path, b: &Bindings) -> Vec<LoaderOutcome> {
        evaluate_one(mods, &code(), b, SourceKind::Game, root).loaders
    }

    const UE: &str = "[[mods.loader]]\nid = \"ue4ss3\"\nname = \"UE4SS 3\"\nsource = [\"game\"]\nrequire = [ { stat = \"{source}/{project}/bin/ue4ss/UE4SS.dll\", kind = \"file\" } ]\nversion = { pe = \"{source}/{project}/bin/ue4ss/UE4SS.dll\", key = [\"ProductVersion\", \"FileVersion\"], template = \"N{1,2}.N{1,3}.N{1,3}\", versions = \">=3, <4\" }\nroot = \"{source}/{project}/bin/ue4ss/Mods\"\nenabled = [ { format = \"enable-list\", list = \"{source}/{project}/bin/ue4ss/Mods/mods.txt\", line = \"name-colon-flag\", marker = \"enabled.txt\" } ]\ncoexist = [ { strategy = \"chain\", entry = \"{source}/{project}/bin/ue4ss/UE4SS.dll\", tell = \"ue4ss-mods-paths\" } ]\n";

    #[test]
    fn a_loader_is_recognised_with_its_version_and_its_enabled_mods() {
        let root = tmp("ue");
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "3.0.1")], 0),
        );
        put(
            &root,
            "Pal/bin/ue4ss/Mods/mods.txt",
            b"\xef\xbb\xbfAlpha : 1\r\nbeta:0\n; Gamma : 1\nDelta : 0\nDelta : 1\nbroken line\nZETA:1\n",
        );
        for m in ["Alpha", "Beta", "Delta", "Gamma", "Epsilon", "Zeta"] {
            fs::create_dir_all(root.join("Pal/bin/ue4ss/Mods").join(m)).unwrap();
        }
        put(&root, "Pal/bin/ue4ss/Mods/Epsilon/enabled.txt", b"");
        put(&root, "Pal/bin/ue4ss/Mods/loose.lua", b"");
        let mods = mods_of(UE);
        let b = Bindings::new().with("project", "Pal");
        let got = game(&mods, &root, &b);
        let enabled = |r: &Recognised| -> Vec<(String, bool)> {
            r.mods.iter().map(|m| (m.name.clone(), m.enabled)).collect()
        };
        let [LoaderOutcome::Recognised(r)] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!((r.version.as_deref(), r.supported), (Some("3.0.1"), true));
        assert_eq!(r.need.as_deref(), Some(">=3, <4"));
        assert_eq!(
            enabled(r),
            [
                ("Alpha".to_owned(), true),
                ("Beta".to_owned(), false),
                ("Delta".to_owned(), false),
                ("Epsilon".to_owned(), true),
                ("Gamma".to_owned(), false),
                ("Zeta".to_owned(), true),
            ]
        );
        assert_eq!(
            r.coexist,
            [Strategy::Chain {
                entry: root.join("Pal/bin/ue4ss/UE4SS.dll"),
                tell: Some(Tell::Ue4ssModsPaths)
            }]
        );
        // Unbound `{project}`: the loader yields nothing — even where `{source}` itself would
        // have answered for the placeholder (its mods root here: the folders at the top).
        assert_eq!(game(&mods, &root, &Bindings::new()), []);
        let top = "[[mods.loader]]\nid = \"top\"\nname = \"Top\"\nsource = [\"game\"]\nrequire = [ { stat = \"{source}/top.dll\", kind = \"file\" } ]\nroot = \"{source}/{project}\"\nenabled = [ { format = \"dir-per-mod\" } ]\ncoexist = [ { strategy = \"side-by-side\" } ]\n";
        put(&root, "top.dll", b"");
        let top = mods_of(top);
        assert_eq!(game(&top, &root, &Bindings::new()), []);
        assert!(matches!(
            &game(&top, &root, &Bindings::new().with("project", "Pal/bin"))[..],
            [LoaderOutcome::Recognised(r)] if r.mods.len() == 1 && r.mods[0].name == "ue4ss"
        ));
        // Another source kind, another code: not asked.
        assert_eq!(
            evaluate_one(&mods, &code(), &b, SourceKind::Picked, &root).loaders,
            []
        );
        assert_eq!(
            evaluate_one(
                &mods,
                &EngineCode::new("other").unwrap(),
                &b,
                SourceKind::Game,
                &root
            )
            .loaders,
            []
        );
        // A version outside `versions`: recognised, unsupported, told what is needed.
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "2.5.2")], 0),
        );
        let got = game(&mods, &root, &b);
        let [LoaderOutcome::Recognised(r)] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!((r.version.as_deref(), r.supported), (Some("2.5.2"), false));
        // Unreadable: unknown, unsupported.
        put(&root, "Pal/bin/ue4ss/UE4SS.dll", b"MZ");
        let got = game(&mods, &root, &b);
        let [LoaderOutcome::Recognised(r)] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!((r.version.as_deref(), r.supported), (None, false));
        let _ = fs::remove_dir_all(&root);
    }

    const GLOB: &str = "[[mods.loader]]\nid = \"paks\"\nname = \"Paks\"\nsource = [\"game\", \"picked\"]\nrequire = [ { child = \"{source}/Content/Paks/~mods\", kind = \"dir\" } ]\nroot = \"{source}/Content/Paks/~mods\"\nenabled = [ { format = \"file-glob\", glob = \"*.pak\" } ]\ncoexist = [ { strategy = \"side-by-side\" } ]\n";

    #[test]
    fn a_root_anchored_loader_lists_its_files_and_ignores_case() {
        let root = tmp("paks");
        put(&root, "content/PAKS/~MODS/b_P.pak", b"");
        put(&root, "content/PAKS/~MODS/a.PAK", b"");
        put(&root, "content/PAKS/~MODS/a.pak.old", b"");
        put(&root, "content/PAKS/~MODS/sub/c.pak", b"");
        let mods = mods_of(GLOB);
        let got = game(&mods, &root, &Bindings::new());
        let [LoaderOutcome::Recognised(r)] = got.as_slice() else {
            panic!("{got:?}")
        };
        let names: Vec<&str> = r.mods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["a.PAK", "b_P.pak"]);
        assert!(r.mods.iter().all(|m| m.enabled && !m.folder));
        assert_eq!(r.coexist, [Strategy::SideBySide { tell: None }]);
        assert_eq!(r.version, None);
        assert!(r.supported);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_read_through_a_link() {
        let root = tmp("links");
        let outside = tmp("links-outside");
        put(&outside, "~mods/x.pak", b"");
        fs::create_dir_all(root.join("Content/Paks")).unwrap();
        std::os::unix::fs::symlink(outside.join("~mods"), root.join("Content/Paks/~mods")).unwrap();
        let mods = mods_of(GLOB);
        assert_eq!(game(&mods, &root, &Bindings::new()), []);
        // A link to a mod inside the root is not a mod.
        fs::remove_file(root.join("Content/Paks/~mods")).unwrap();
        put(&root, "Content/Paks/~mods/real.pak", b"");
        std::os::unix::fs::symlink(
            outside.join("~mods/x.pak"),
            root.join("Content/Paks/~mods/x.pak"),
        )
        .unwrap();
        let got = game(&mods, &root, &Bindings::new());
        let [LoaderOutcome::Recognised(r)] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!(r.mods.len(), 1);
        assert_eq!(r.mods[0].name, "real.pak");
        // A link on the way to a file looked up exactly (`stat`), the loader's own folder linked
        // in from outside: not there.
        put(
            &outside,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "3.0.1")], 0),
        );
        std::os::unix::fs::symlink(outside.join("Pal"), root.join("Pal")).unwrap();
        let ue = mods_of(UE);
        let b = Bindings::new().with("project", "Pal");
        assert_eq!(game(&ue, &root, &b), []);
        assert!(matches!(
            &game(&ue, &outside, &b)[..],
            [LoaderOutcome::Recognised(_)]
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn every_per_evaluation_bound_makes_the_loader_yield_nothing() {
        let mods = mods_of(GLOB);
        // 1,024 mods: listed. 1,025: over.
        let root = tmp("many");
        for i in 0..limits::MODS_PER_ROOT {
            put(&root, &format!("Content/Paks/~mods/{i:05}.pak"), b"");
        }
        let got = game(&mods, &root, &Bindings::new());
        let [LoaderOutcome::Recognised(r)] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!(r.mods.len(), limits::MODS_PER_ROOT);
        put(&root, "Content/Paks/~mods/one-more.pak", b"");
        assert_eq!(
            game(&mods, &root, &Bindings::new()),
            [LoaderOutcome::OverBudget {
                id: "paks".into(),
                name: "Paks".into(),
                bound: Bound::Mods
            }]
        );
        // 5,000 entries in a folder: listed (not mods: no glob matches). 5,001: over.
        let _ = fs::remove_dir_all(&root);
        let root = tmp("entries");
        for i in 0..limits::ENTRIES_PER_DIR {
            put(&root, &format!("Content/Paks/~mods/{i:05}.txt"), b"");
        }
        let got = game(&mods, &root, &Bindings::new());
        assert!(
            matches!(&got[..], [LoaderOutcome::Recognised(r)] if r.mods.is_empty()),
            "{got:?}"
        );
        put(&root, "Content/Paks/~mods/one-more.txt", b"");
        assert!(matches!(
            &game(&mods, &root, &Bindings::new())[..],
            [LoaderOutcome::OverBudget {
                bound: Bound::Entries,
                ..
            }]
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn markers_count_as_lookups() {
        // 2,048 lookups in all: every folder listed and every marker check counts one — one per
        // mod folder, whatever the list says (§7.8). A root of 1,024 folders costs 1,024 marker
        // checks beside the require lookup and the listings, enabled by the list or not; two
        // such loaders cannot both be read.
        let two = format!(
            "{}{}",
            UE,
            UE.replace("ue4ss3", "ue4ss3b")
                .replace("UE4SS 3", "UE4SS 3b")
        );
        let mods = mods_of(&two);
        let root = tmp("markers");
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "3.0.1")], 0),
        );
        let mut list = String::new();
        for i in 0..limits::MODS_PER_ROOT {
            fs::create_dir_all(root.join(format!("Pal/bin/ue4ss/Mods/m{i:04}"))).unwrap();
            list.push_str(&format!("m{i:04}:1\n"));
        }
        let b = Bindings::new().with("project", "Pal");
        for listed in [false, true] {
            if listed {
                put(&root, "Pal/bin/ue4ss/Mods/mods.txt", list.as_bytes());
            }
            let got = game(&mods, &root, &b);
            assert!(
                matches!(&got[0], LoaderOutcome::Recognised(r)
                    if r.mods.len() == 1024 && r.mods.iter().all(|m| m.enabled == listed)),
                "{got:?}"
            );
            assert!(
                matches!(
                    &got[1],
                    LoaderOutcome::OverBudget {
                        bound: Bound::Lookups,
                        ..
                    }
                ),
                "{got:?}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_evaluation_reads_128_kib_in_all() {
        // Three loaders (six content probes) read the same 30 KB version resource and the same
        // 15 KB list: each within its own bound, the third past the evaluation's 128 KiB, so
        // that one yields nothing — never a list cut short.
        let three: String = ["ue-a", "ue-b", "ue-c"]
            .iter()
            .map(|id| {
                UE.replace("ue4ss3", id)
                    .replace(", marker = \"enabled.txt\"", "")
            })
            .collect();
        let mods = mods_of(&three);
        let root = tmp("eval-bytes");
        let value = format!("3.0.1 {}", "x".repeat(15_000));
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", value.as_str())], 0),
        );
        let mut list = String::new();
        let mut i = 0;
        while list.len() < 15_000 {
            list.push_str(&format!("Mod{i:05} : 1\n"));
            i += 1;
        }
        put(&root, "Pal/bin/ue4ss/Mods/mods.txt", list.as_bytes());
        fs::create_dir_all(root.join("Pal/bin/ue4ss/Mods/Mod00000")).unwrap();
        let b = Bindings::new().with("project", "Pal");
        let got = game(&mods, &root, &b);
        assert!(
            matches!(
                &got[..],
                [
                    LoaderOutcome::Recognised(a),
                    LoaderOutcome::Recognised(b),
                    LoaderOutcome::OverBudget { bound: Bound::Bytes, .. }
                ] if a.supported && b.supported && a.mods.iter().all(|m| m.enabled)
            ),
            "{got:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A caller's own meter, spent mid-evaluation: the loader yields nothing, as over any bound.
    #[test]
    fn a_meter_spent_mid_evaluation_yields_nothing() {
        struct Left(std::cell::Cell<usize>, std::cell::Cell<u64>);
        impl Meter for Left {
            fn entry(&self) -> bool {
                let n = self.0.get();
                self.0.set(n.saturating_sub(1));
                n > 0
            }
            fn bytes(&self, n: u64) -> bool {
                let left = self.1.get();
                self.1.set(left.saturating_sub(n));
                n <= left
            }
            fn bytes_left(&self) -> u64 {
                self.1.get()
            }
        }
        let meter =
            |entries, bytes| Left(std::cell::Cell::new(entries), std::cell::Cell::new(bytes));
        let paks = mods_of(GLOB);
        let root = tmp("meter");
        for i in 0..10 {
            put(&root, &format!("Content/Paks/~mods/{i}.pak"), b"");
        }
        let with = |mods: &super::super::Mods, b: &Bindings, m: &Left| {
            evaluate_one_within(mods, &code(), b, SourceKind::Game, &root, m).loaders
        };
        let enough = meter(10, 0);
        assert!(matches!(
            &with(&paks, &Bindings::new(), &enough)[..],
            [LoaderOutcome::Recognised(r)] if r.mods.len() == 10
        ));
        assert_eq!(enough.0.get(), 0, "every entry listed is charged");
        assert!(matches!(
            &with(&paks, &Bindings::new(), &meter(9, 0))[..],
            [LoaderOutcome::OverBudget {
                bound: Bound::Discovery,
                ..
            }]
        ));
        // The version probe and the list are read within what the meter has left.
        let ue = mods_of(UE);
        let b = Bindings::new().with("project", "Pal");
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "3.0.1")], 0),
        );
        put(&root, "Pal/bin/ue4ss/Mods/mods.txt", b"A : 1\n");
        assert!(matches!(
            &with(&ue, &b, &meter(100, 1 << 20))[..],
            [LoaderOutcome::Recognised(r)] if r.supported
        ));
        assert!(matches!(
            &with(&ue, &b, &meter(100, 64))[..],
            [LoaderOutcome::OverBudget {
                bound: Bound::Discovery,
                ..
            }]
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_version_probe_past_its_bound_is_over_budget() {
        let mods = mods_of(UE);
        let root = tmp("bigversion");
        // A version resource of 40 KB: more than a probe may read (32 KiB), so the loader yields
        // nothing here, never "unsupported" for a version it could not read.
        let long = format!("3.0.1{}", "0".repeat(20_000));
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", long.as_str())], 0),
        );
        let b = Bindings::new().with("project", "Pal");
        assert!(matches!(
            &game(&mods, &root, &b)[..],
            [LoaderOutcome::OverBudget {
                bound: Bound::Bytes,
                ..
            }]
        ));
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "3.0.1")], 0),
        );
        assert!(matches!(
            &game(&mods, &root, &b)[..],
            [LoaderOutcome::Recognised(r)] if r.supported
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_list_past_its_bound_is_never_half_read() {
        let mods = mods_of(UE);
        let root = tmp("biglist");
        put(
            &root,
            "Pal/bin/ue4ss/UE4SS.dll",
            &pe::build::pe(&[("ProductVersion", "3.0.1")], 0),
        );
        let mut text = String::new();
        while text.len() as u64 <= limits::LIST_BYTES {
            text.push_str("SomeMod : 1\n");
        }
        put(&root, "Pal/bin/ue4ss/Mods/mods.txt", text.as_bytes());
        fs::create_dir_all(root.join("Pal/bin/ue4ss/Mods/SomeMod")).unwrap();
        let b = Bindings::new().with("project", "Pal");
        assert!(matches!(
            &game(&mods, &root, &b)[..],
            [LoaderOutcome::OverBudget {
                bound: Bound::Bytes,
                ..
            }]
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sources_are_one_of_each_kind() {
        let mods = mods_of(GLOB);
        let game_dir = tmp("src-game");
        let picked = tmp("src-picked");
        let manager = tmp("src-manager");
        for d in [&game_dir, &picked, &manager] {
            put(d, "Content/Paks/~mods/x.pak", b"");
        }
        let s = Sources {
            game: Some(game_dir.clone()),
            manager: Some(manager.clone()),
            picked: Some(picked.clone()),
        };
        let got = evaluate(&mods, &code(), &Bindings::new(), &s);
        let kinds: Vec<(SourceKind, usize)> =
            got.iter().map(|e| (e.source, e.loaders.len())).collect();
        // The loader names `game` and `picked`, never `manager`.
        assert_eq!(
            kinds,
            [
                (SourceKind::Game, 1),
                (SourceKind::Manager, 0),
                (SourceKind::Picked, 1)
            ]
        );
        for d in [&game_dir, &picked, &manager] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn name_colon_flag_reads_ue4ss_lists() {
        assert_eq!(
            name_colon_flag("A : 1\n B:0 \n;C : 1\n\nD : x\n: 1\nE :1\r\n"),
            [
                ("A".to_owned(), true),
                ("B".to_owned(), false),
                ("E".to_owned(), true)
            ]
        );
    }
}
