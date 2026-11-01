// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe capture`: a recording of a real game folder, `astra.list-shape/1` (merged plan
//! R29; the format and its replay are `tests/corpus/replay.rs`), for canary C1's
//! `tests/corpus/recorded/` and for `validate-descriptor --corpus`.
//!
//! **What it reads.** Exactly the folders detection lists, each with every name it holds:
//!
//! 1. the walk: breadth first from the game folder, every folder above the walk's depth bound
//!    and those a built-in descriptor still lists at it (`walk.list_at_limit`), until the walk's
//!    5,000 entries are reached (the folder where they are is still recorded whole: a fresh
//!    listing of it reads as many);
//! 2. near every program the built-in rules may ask about (a file of a built-in rule's
//!    `programs.ext`, or one its `also_if` names a folder beside) and near Steam's hint: the
//!    folders on the hint's way, the program's own folder and the [`UP`] folders above it, and
//!    below each of those the folders [`DOWN`] levels deep — what the rules look up from a
//!    program (a `{stem}_Data/Managed/<file>`, a `{project}/Content/Paks/<file>`, a
//!    `{project.parent}/<file>`), listed whole.
//!
//! **What bytes.** Only those the built-ins and Pass B read: the first [`MAX_HEAD`] bytes of a
//! file a built-in rule's `version.head` names, and of every program-like file (Pass B's
//! extensions or none) its first [`MAGIC`] bytes and its last [`MAX_TAIL`]. No other content;
//! every other file is a name, a kind and a size. Links are marked and never followed; FIFOs are
//! marked and never opened.
//!
//! The capture also records what this build's Pass A answers for the folder itself (with the
//! hint), so a replay is held to the folder it was taken from; and the label, by default that
//! answer's engine (`unknown` when no built-in names it). The hint is `--hint`, else, given
//! `--appid`, what Steam's `appinfo.vdf` launches for the app ([`steam_hint`]). A recording of
//! more than [`MAX_RECORDING_BYTES`] is refused, saying so, and nothing is written.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use astra_game_launcher::engines::BUILTIN;
use astra_game_launcher::engines::descriptor::PASS_B_PROGRAM_EXTS;
use astra_game_launcher::{
    detect_with, is_valid_engine_code, launch_executable, proton_prefix_for, steam_roots,
};
use serde_json::{Value, json};

use crate::replay::{Entry, Kind, MAGIC, MAX_HEAD, MAX_RECORDING_BYTES, MAX_TAIL, Recording};
use crate::{Fatal, Finding, report, tool};

/// The walk's bounds (`src/detect.rs` `MAX_DEPTH`, `MAX_ENTRIES`): entries below this many
/// folders are listed but not looked into, and the walk stops at this many entries.
const MAX_DEPTH: usize = 4;
const MAX_ENTRIES: usize = 5000;
/// Around a program: its folder and this many folders above it ...
pub const UP: usize = 3;
/// ... each listed with the folders this many levels below it.
pub const DOWN: usize = 2;

/// The finding of a recording over [`MAX_RECORDING_BYTES`] (`gi-probe`'s own code: no launcher
/// refuses anything for it).
pub const RECORDING_TOO_LARGE: &str = "recording-too-large";

/// The command line.
pub struct CaptureArgs {
    pub dir: PathBuf,
    pub out: PathBuf,
    pub label: Option<String>,
    pub appid: Option<u32>,
    pub hint: Option<String>,
}

/// What the built-in descriptors tell a capture: what the walk lists at its depth bound, which
/// names are programs, and which files a version is read from.
struct Rules {
    at_limit: Vec<Vec<String>>,
    /// Lower-case extensions a built-in rule's `programs.ext` names.
    program_exts: BTreeSet<String>,
    /// `also_if = { dir = "…" }` templates (`{name}` the file's own name).
    also_if: BTreeSet<String>,
    /// Lower-case file names a `version.head` names (each path's last part).
    version_heads: BTreeSet<String>,
}

fn strings(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

impl Rules {
    fn of_builtins() -> Rules {
        let mut r = Rules {
            at_limit: Vec::new(),
            program_exts: BTreeSet::new(),
            also_if: BTreeSet::new(),
            version_heads: BTreeSet::new(),
        };
        for text in BUILTIN {
            let t: toml::Table = toml::from_str(text).expect("a built-in descriptor is TOML");
            for p in strings(t.get("walk").and_then(|w| w.get("list_at_limit"))) {
                r.at_limit.push(p.split('/').map(str::to_owned).collect());
            }
            let rules = t.get("rule").and_then(toml::Value::as_array);
            for rule in rules.into_iter().flatten() {
                if let Some(p) = rule.get("programs") {
                    for e in strings(p.get("ext")) {
                        r.program_exts.insert(e.to_ascii_lowercase());
                    }
                    if let Some(d) = p
                        .get("also_if")
                        .and_then(|a| a.get("dir"))
                        .and_then(toml::Value::as_str)
                    {
                        r.also_if.insert(d.to_owned());
                    }
                }
                for h in strings(rule.get("version").and_then(|v| v.get("head"))) {
                    if let Some(leaf) = h.rsplit('/').next() {
                        r.version_heads.insert(leaf.to_ascii_lowercase());
                    }
                }
            }
        }
        r.program_exts.retain(|e| !e.is_empty());
        r
    }

    fn listed_at_limit(&self, child: &[String]) -> bool {
        child.len() == MAX_DEPTH
            && self.at_limit.iter().any(|p| {
                p.len() <= child.len()
                    && child[child.len() - p.len()..]
                        .iter()
                        .zip(p)
                        .all(|(a, b)| a.eq_ignore_ascii_case(b))
            })
    }

    fn ext(name: &str) -> Option<String> {
        name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
    }

    /// A file a built-in rule may take as a program in a folder holding `listing`.
    fn is_program(&self, name: &str, listing: &[Entry]) -> bool {
        Rules::ext(name).is_some_and(|e| self.program_exts.contains(&e))
            || self.also_if.iter().any(|t| {
                let dir = t.replace("{name}", name);
                listing.iter().any(|e| e.kind == Kind::Dir && e.name == dir)
            })
    }

    /// A file Pass B may take as a program: one of its extensions, or none.
    fn program_like(name: &str) -> bool {
        match Rules::ext(name) {
            None => true,
            Some(e) => PASS_B_PROGRAM_EXTS.contains(&e.as_str()),
        }
    }
}

/// One folder's names, as a listing read with no link followed. `None` when it cannot be read.
fn read_folder(dir: &Path) -> Option<Vec<Entry>> {
    let mut out: Vec<Entry> = Vec::new();
    let mut seen = BTreeSet::new();
    for item in fs::read_dir(dir).ok()?.flatten() {
        let Ok(ft) = item.file_type() else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if !seen.insert(name.clone()) {
            continue;
        }
        let kind = if ft.is_symlink() {
            Kind::Link
        } else if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File {
                size: item.metadata().map_or(0, |m| m.len()),
                head: Vec::new(),
                tail: Vec::new(),
            }
        } else if is_fifo(&ft) {
            Kind::Fifo
        } else {
            Kind::Other
        };
        out.push(Entry { name, kind });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(out)
}

#[cfg(unix)]
fn is_fifo(ft: &fs::FileType) -> bool {
    std::os::unix::fs::FileTypeExt::is_fifo(ft)
}

#[cfg(not(unix))]
fn is_fifo(_: &fs::FileType) -> bool {
    false
}

/// A regular file opened for reading, never through a link and never waiting on a FIFO.
fn open(path: &Path) -> Option<File> {
    let mut o = fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = o.open(path).ok()?;
    f.metadata().ok()?.is_file().then_some(f)
}

fn head_of(path: &Path, n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(f) = open(path) {
        let _ = f.take(n).read_to_end(&mut out);
    }
    out
}

fn tail_of(path: &Path, size: u64, n: u64) -> Vec<u8> {
    let n = n.min(size);
    let Some(mut f) = open(path) else {
        return Vec::new();
    };
    let mut out = vec![0u8; n as usize];
    let ok = f.seek(SeekFrom::Start(size - n)).is_ok() && f.read_exact(&mut out).is_ok();
    if ok { out } else { Vec::new() }
}

fn abs(root: &Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel);
    p
}

/// The folders a capture lists, and how far its walk went.
struct Lister<'a> {
    root: &'a Path,
    rules: &'a Rules,
    folders: BTreeMap<Vec<String>, Vec<Entry>>,
    walked: usize,
    capped: bool,
}

impl Lister<'_> {
    /// `rel`'s listing, read once and recorded. `None` when it is not a folder that reads.
    fn list(&mut self, rel: &[String]) -> Option<&Vec<Entry>> {
        if !self.folders.contains_key(rel) {
            let entries = read_folder(&abs(self.root, rel))?;
            self.folders.insert(rel.to_vec(), entries);
        }
        self.folders.get(rel)
    }

    fn walk(&mut self) -> Result<(), Fatal> {
        let mut queue: VecDeque<Vec<String>> = VecDeque::from([Vec::new()]);
        while let Some(rel) = queue.pop_front() {
            let Some(entries) = self.list(&rel).cloned() else {
                if rel.is_empty() {
                    return Err(Fatal::io(self.root, "the game folder cannot be listed"));
                }
                continue;
            };
            // The walk's own view: no links, sorted with whether each is a folder.
            let mut view: Vec<(String, bool)> = entries
                .iter()
                .filter(|e| e.kind != Kind::Link)
                .map(|e| (e.name.clone(), e.kind == Kind::Dir))
                .collect();
            view.sort();
            for (name, is_dir) in view {
                if self.walked >= MAX_ENTRIES {
                    self.capped = true;
                    return Ok(());
                }
                let mut child = rel.clone();
                child.push(name);
                if is_dir && (child.len() < MAX_DEPTH || self.rules.listed_at_limit(&child)) {
                    queue.push_back(child);
                }
                self.walked += 1;
            }
        }
        Ok(())
    }

    /// `rel` and the folders `levels` deep below it, listed.
    fn below(&mut self, rel: &[String], levels: usize) {
        let mut level = vec![rel.to_vec()];
        for depth in 0..=levels {
            let mut next = Vec::new();
            for dir in &level {
                let Some(entries) = self.list(dir) else {
                    continue;
                };
                if depth < levels {
                    for e in entries.iter().filter(|e| e.kind == Kind::Dir) {
                        let mut c = dir.clone();
                        c.push(e.name.clone());
                        next.push(c);
                    }
                }
            }
            level = next;
        }
    }

    /// The neighbourhood of a program in folder `rel`: see the module doc.
    fn near(&mut self, rel: &[String]) {
        for up in 0..=UP.min(rel.len()) {
            self.below(&rel[..rel.len() - up], DOWN);
        }
    }

    /// The folders on the hint's way, each listed, its parts resolved as the launcher resolves
    /// them (the exact spelling, else ignoring ASCII case); the program's folder when it is a
    /// file there.
    fn hint(&mut self, hint: &str) -> Option<Vec<String>> {
        let parts: Vec<&str> = hint
            .split(['/', '\\'])
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        if parts.is_empty() || parts.contains(&"..") {
            return None;
        }
        let mut real: Vec<String> = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let entries = self.list(&real)?;
            let want = |e: &&Entry| {
                if last {
                    matches!(e.kind, Kind::File { .. })
                } else {
                    e.kind == Kind::Dir
                }
            };
            let found = entries
                .iter()
                .filter(want)
                .find(|e| e.name == *part)
                .or_else(|| {
                    entries
                        .iter()
                        .filter(want)
                        .filter(|e| e.name.eq_ignore_ascii_case(part))
                        .min_by(|a, b| a.name.cmp(&b.name))
                })?
                .name
                .clone();
            if last {
                return Some(real);
            }
            real.push(found);
        }
        None
    }

    /// Every program folder in what is listed so far.
    fn program_folders(&self) -> Vec<Vec<String>> {
        self.folders
            .iter()
            .filter(|(_, entries)| {
                entries.iter().any(|e| {
                    matches!(e.kind, Kind::File { .. }) && self.rules.is_program(&e.name, entries)
                })
            })
            .map(|(rel, _)| rel.clone())
            .collect()
    }

    /// The bytes of every recorded file the rules read: see the module doc.
    fn read_bytes(&mut self) -> usize {
        let mut with_bytes = 0;
        for (rel, entries) in &mut self.folders {
            for e in entries.iter_mut() {
                let Kind::File { size, head, tail } = &mut e.kind else {
                    continue;
                };
                let lower = e.name.to_ascii_lowercase();
                let version = self.rules.version_heads.contains(&lower);
                let program = Rules::program_like(&e.name);
                if !version && !program {
                    continue;
                }
                let p = abs(self.root, rel).join(&e.name);
                let n = if version { MAX_HEAD } else { MAGIC };
                *head = head_of(&p, n as u64);
                if program {
                    *tail = tail_of(&p, *size, MAX_TAIL as u64);
                }
                if !head.is_empty() || !tail.is_empty() {
                    with_bytes += 1;
                }
            }
        }
        with_bytes
    }
}

/// The program Steam launches for `appid`, as the CLI's `detect --appid` finds it: from the
/// first Steam root's `appcache/appinfo.vdf`, the launch entry for the OS the game runs as (on
/// Linux, Windows first when the game has a Proton prefix), the first one the rules take.
fn steam_hint(root: &Path, appid: u32) -> Option<String> {
    let appinfo = steam_roots()
        .into_iter()
        .map(|r| r.join("appcache").join("appinfo.vdf"))
        .find(|p| p.is_file())?;
    let proton = proton_prefix_for(root, appid).is_some_and(|p| p.is_dir());
    let oses: &[&str] = if cfg!(windows) {
        &["windows"]
    } else if proton {
        &["windows", "linux"]
    } else {
        &["linux", "windows"]
    };
    let hints: Vec<String> = oses
        .iter()
        .filter_map(|os| launch_executable(&appinfo, appid, os))
        .collect();
    hints
        .iter()
        .find(|h| detect_with(root, Some(h)).is_ok_and(|d| d.hint_used))
        .or(hints.first())
        .cloned()
}

/// `gi-probe capture`.
pub fn run(a: &CaptureArgs) -> Result<(Value, bool), Fatal> {
    let root = fs::canonicalize(&a.dir).map_err(|e| Fatal::io(&a.dir, e))?;
    if !fs::metadata(&root).is_ok_and(|m| m.is_dir()) {
        return Err(Fatal::io(&a.dir, "not a folder"));
    }
    if let Some(l) = &a.label
        && !is_valid_engine_code(l)
    {
        return Err(Fatal::usage(format!(
            "--label is an engine code (`unknown` when nobody knows), not {l:?}"
        )));
    }
    let (hint, hint_from) = match (&a.hint, a.appid) {
        (Some(h), _) => (Some(h.clone()), "--hint"),
        (None, Some(id)) => match steam_hint(&root, id) {
            Some(h) => (Some(h), "steam"),
            None => (None, "none"),
        },
        (None, None) => (None, "none"),
    };
    let detection = detect_with(&root, hint.as_deref())
        .map_err(|e| Fatal::io(&a.dir, format!("{}: {:?}", e.code, e.params)))?;
    let rules = Rules::of_builtins();
    let mut lister = Lister {
        root: &root,
        rules: &rules,
        folders: BTreeMap::new(),
        walked: 0,
        capped: false,
    };
    lister.walk()?;
    let mut programs = lister.program_folders();
    if let Some(h) = &hint
        && let Some(folder) = lister.hint(h)
    {
        programs.push(folder);
    }
    for p in programs {
        lister.near(&p);
    }
    let with_bytes = lister.read_bytes();
    let walked = lister.walked;
    let capped = lister.capped;
    let rec = Recording {
        game: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        label: a
            .label
            .clone()
            .unwrap_or_else(|| detection.engine.as_str().to_owned()),
        appid: a.appid,
        hint: hint.clone(),
        os: std::env::consts::OS.to_owned(),
        tool: tool(),
        detection: serde_json::to_value(&detection).expect("a detection serialises"),
        folders: lister
            .folders
            .into_iter()
            .map(|(rel, entries)| (rel.join("/"), entries))
            .collect(),
    };
    let summary = json!({
        "schema": crate::replay::SCHEMA,
        "game": rec.game,
        "label": rec.label,
        "folders": rec.folders.len(),
        "entries": rec.entries(),
        "files_with_bytes": with_bytes,
        "walk": { "entries": walked, "capped": capped },
        "engine": detection.engine.as_str(),
    });
    let mut findings = Vec::new();
    let mut summary = summary;
    match rec.to_text() {
        Ok(text) => {
            fs::write(&a.out, &text).map_err(|e| Fatal::io(&a.out, e))?;
            summary["bytes"] = json!(text.len());
        }
        Err(size) => {
            summary["bytes"] = json!(size);
            findings.push(
                Finding::new(
                    RECORDING_TOO_LARGE,
                    format!(
                        "the recording would be {size} bytes, more than {MAX_RECORDING_BYTES}: nothing was written"
                    ),
                )
                .with("bytes", size.to_string())
                .with("limit", MAX_RECORDING_BYTES.to_string()),
            );
        }
    }
    let passed = findings.is_empty();
    let rest = json!({
        "input": {
            "dir": a.dir.display().to_string(),
            "out": a.out.display().to_string(),
            "hint": hint,
            "hint_from": hint_from,
            "appid": a.appid,
        },
        "recording": summary,
    });
    Ok((report("capture", rest, &findings), passed))
}
