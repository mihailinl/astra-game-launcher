// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `astra.list-shape/1`: a recording of a real game folder (merged plan R29, plan B §2's
//! `bin/list-shape`), as `gi-probe capture` writes it, and its replay — the folder laid again
//! below the temporary directory, as detection sees it.
//!
//! A recording holds what the detection walk and the folder reads near a program would see —
//! every name of each folder it lists, with its kind and a file's size — and only the bytes the
//! built-in descriptors and Pass B read: the first bytes of a version header (at most
//! [`MAX_HEAD`]), a program's magic (its first 4 bytes) and its last bytes (at most
//! [`MAX_TAIL`]). No other content. At most [`MAX_RECORDING_BYTES`] of text.
//!
//! ```json
//! {"schema": "astra.list-shape/1", "game": "<the folder's own name>", "label": "unknown",
//!  "appid": 320, "hint": "hl2.exe",
//!  "captured": {"os": "linux", "tool": {"version": "0.5.0", "source_tree_sha256": "…"}},
//!  "detection": {…},
//!  "folders": {"": [["Game.exe", "f", 651264, "4d5a9000", "…"], ["Game_Data", "d"]],
//!              "Game_Data": [["globalgamemanagers", "f", 98304, "…"]]}}
//! ```
//!
//! - `label`: the engine the game is, as far as anyone knows (`unknown` when nobody does): what
//!   `validate-descriptor --corpus` holds a descriptor's claims to.
//! - `appid`, `hint`: the Steam app and the program Steam launches for it, relative to the
//!   folder, when known; detection is asked with the hint.
//! - `detection`: what the capturing build's Pass A answered for the folder itself, with the
//!   hint (`Detection`'s JSON). A replay on the same OS must answer the same.
//! - `folders`: every folder whose listing was read, by its `/`-joined path below the game
//!   folder (`""` is the folder itself), each with every name it held: `[name, kind]`, or for a
//!   regular file `[name, "f", size]`, `[name, "f", size, head]` or `[name, "f", size, head,
//!   tail]` (lower-case hex; `head` the file's first bytes, `tail` its last, `""` when not
//!   recorded). `kind`: `f` a regular file, `d` a folder, `l` a link (a symbolic link or a
//!   junction; never followed, its target not recorded), `p` a FIFO, `o` anything else (a
//!   socket, a device). A folder named by a `d` entry but absent from `folders` was not listed:
//!   it is laid empty.
//!
//! The replay ([`materialise`]) lays each file sparse, at its recorded size, holding the recorded
//! bytes at their offsets and zeros elsewhere; a link as a link that leads nowhere (launcher
//! 0.5.0 reads nothing through a link, and 0.4.1 then finds nothing behind it either); a FIFO,
//! and anything else, as a FIFO (Unix only: making a device node needs privileges, and every
//! reader here treats the two alike — neither a file nor a folder). On Windows a link that cannot
//! be made (no Developer Mode, no elevation) is left out, which the launcher reads the same way.
//!
//! This one file is the format's only reader and writer: `gi-probe` (`src/bin/gi-probe/`)
//! includes it by path, as the tests do.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use astra_game_launcher::engines::descriptor::limits;
use serde_json::{Map, Value, json};

/// The format's `schema`.
pub const SCHEMA: &str = "astra.list-shape/1";
/// A recording is at most this many bytes of text: a larger one is refused, by `capture` and by
/// every reader.
pub const MAX_RECORDING_BYTES: usize = 1 << 20;
/// The most of a version header's head a recording holds: what a version is read from.
pub const MAX_HEAD: usize = limits::HEAD_BYTES as usize;
/// The most of a file's tail a recording holds: the longest `tail` a rule may compare.
pub const MAX_TAIL: usize = limits::TAIL_BYTES as usize;
/// The first bytes of a program a recording holds: its magic (`MZ`, `\x7fELF`).
pub const MAGIC: usize = 4;
/// What a replayed link points at: a name no replay lays, relative, so it leads nowhere.
pub const DANGLING: &str = ".astra-list-shape-link-leads-nowhere";

/// What one name in a folder is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    File {
        size: u64,
        head: Vec<u8>,
        tail: Vec<u8>,
    },
    Dir,
    Link,
    Fifo,
    Other,
}

impl Kind {
    pub fn letter(&self) -> &'static str {
        match self {
            Kind::File { .. } => "f",
            Kind::Dir => "d",
            Kind::Link => "l",
            Kind::Fifo => "p",
            Kind::Other => "o",
        }
    }
}

/// One name in a folder's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

impl Entry {
    fn to_value(&self) -> Value {
        match &self.kind {
            Kind::File { size, head, tail } => {
                let mut v = vec![json!(self.name), json!("f"), json!(size)];
                if !tail.is_empty() {
                    v.push(json!(hex::encode(head)));
                    v.push(json!(hex::encode(tail)));
                } else if !head.is_empty() {
                    v.push(json!(hex::encode(head)));
                }
                Value::Array(v)
            }
            k => json!([self.name, k.letter()]),
        }
    }

    fn from_value(v: &Value, at: &str) -> Result<Entry, String> {
        let a = v
            .as_array()
            .ok_or_else(|| format!("{at}: an entry is an array"))?;
        let name = a
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{at}: an entry starts with its name"))?;
        valid_name(name).map_err(|e| format!("{at}: {e}"))?;
        let at = format!("{at}/{name}");
        let kind = a
            .get(1)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{at}: no kind"))?;
        let kind = match (kind, a.len()) {
            ("f", 3..=5) => {
                let size = a[2]
                    .as_u64()
                    .ok_or_else(|| format!("{at}: a size is a whole number"))?;
                let bytes = |i: usize, max: usize| -> Result<Vec<u8>, String> {
                    let Some(v) = a.get(i) else {
                        return Ok(Vec::new());
                    };
                    let s = v.as_str().ok_or_else(|| format!("{at}: bytes are hex"))?;
                    let b = hex::decode(s).map_err(|e| format!("{at}: bytes are hex: {e}"))?;
                    if b.len() > max || b.len() as u64 > size {
                        return Err(format!(
                            "{at}: {} bytes recorded, at most {max} and the file's size",
                            b.len()
                        ));
                    }
                    Ok(b)
                };
                Kind::File {
                    size,
                    head: bytes(3, MAX_HEAD)?,
                    tail: bytes(4, MAX_TAIL)?,
                }
            }
            ("d", 2) => Kind::Dir,
            ("l", 2) => Kind::Link,
            ("p", 2) => Kind::Fifo,
            ("o", 2) => Kind::Other,
            (k, n) => return Err(format!("{at}: kind {k:?} with {n} fields")),
        };
        Ok(Entry {
            name: name.to_owned(),
            kind,
        })
    }
}

/// A name a folder may hold in a recording: one path component the launcher itself would write
/// on every platform it serves (`astra_game_launcher::is_portable_name`) — no `/`, `\`, `:`,
/// `<>"|?*`, control character, `.`/`..`, trailing dot or space, or Windows device name. A
/// recording is data someone else made: a name legal on Linux that Windows reads as a path
/// (`..\x`, `C:\…`, `\\host\share\x`) would make a replay there lay files outside its folder,
/// so such a recording is refused, never laid, on every OS.
pub fn valid_name(name: &str) -> Result<(), String> {
    if !astra_game_launcher::is_portable_name(name) {
        return Err(format!("{name:?} is not a name a folder holds"));
    }
    Ok(())
}

/// One recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// The game folder's own name (never the path above it).
    pub game: String,
    pub label: String,
    pub appid: Option<u32>,
    pub hint: Option<String>,
    /// `std::env::consts::OS` of the capturing machine, and the capturing build.
    pub os: String,
    pub tool: Value,
    /// The capturing build's Pass A answer for the folder itself, with [`Recording::hint`].
    pub detection: Value,
    /// Every listed folder (`""` the game folder), each name it held.
    pub folders: BTreeMap<String, Vec<Entry>>,
}

const KEYS: &[&str] = &[
    "schema",
    "game",
    "label",
    "appid",
    "hint",
    "captured",
    "detection",
    "folders",
];

impl Recording {
    /// The recording as text: its fields one per line, then one line per folder — so a change
    /// to a game folder is a change to its lines. At most [`MAX_RECORDING_BYTES`]: `Err` with the
    /// size it would have had.
    pub fn to_text(&self) -> Result<String, usize> {
        let mut head = Map::new();
        head.insert("schema".into(), json!(SCHEMA));
        head.insert("game".into(), json!(self.game));
        head.insert("label".into(), json!(self.label));
        if let Some(a) = self.appid {
            head.insert("appid".into(), json!(a));
        }
        if let Some(h) = &self.hint {
            head.insert("hint".into(), json!(h));
        }
        head.insert(
            "captured".into(),
            json!({ "os": self.os, "tool": self.tool }),
        );
        head.insert("detection".into(), self.detection.clone());
        let mut out = String::from("{\n");
        for k in KEYS.iter().filter(|k| **k != "folders") {
            if let Some(v) = head.get(*k) {
                out.push_str(&format!("{}: {},\n", json!(k), v));
            }
        }
        out.push_str("\"folders\": {\n");
        let last = self.folders.len().saturating_sub(1);
        for (i, (folder, entries)) in self.folders.iter().enumerate() {
            let list = Value::Array(entries.iter().map(Entry::to_value).collect());
            out.push_str(&format!(
                "{}: {list}{}\n",
                json!(folder),
                if i == last { "" } else { "," }
            ));
        }
        out.push_str("}\n}\n");
        if out.len() > MAX_RECORDING_BYTES {
            return Err(out.len());
        }
        Ok(out)
    }

    /// A recording read from its text, every rule of the format checked.
    pub fn parse(text: &str) -> Result<Recording, String> {
        if text.len() > MAX_RECORDING_BYTES {
            return Err(format!(
                "{} bytes: a recording is at most {MAX_RECORDING_BYTES}",
                text.len()
            ));
        }
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        let o = v.as_object().ok_or("a recording is an object")?;
        if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown key {k:?}"));
        }
        if o.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
            return Err(format!("schema is not {SCHEMA:?}"));
        }
        let string = |k: &str| -> Result<String, String> {
            o.get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{k} is a string"))
        };
        let game = string("game")?;
        let label = string("label")?;
        if !astra_game_launcher::is_valid_engine_code(&label) {
            return Err(format!("label {label:?} is no engine code"));
        }
        let appid = match o.get("appid") {
            None => None,
            Some(a) => Some(
                a.as_u64()
                    .and_then(|a| u32::try_from(a).ok())
                    .ok_or("appid is a number")?,
            ),
        };
        let hint = match o.get("hint") {
            None => None,
            Some(h) => Some(h.as_str().ok_or("hint is a string")?.to_owned()),
        };
        let captured = o.get("captured").ok_or("no captured")?;
        let os = captured
            .get("os")
            .and_then(Value::as_str)
            .ok_or("captured.os is a string")?
            .to_owned();
        let tool = captured.get("tool").cloned().unwrap_or(Value::Null);
        let detection = o.get("detection").cloned().ok_or("no detection")?;
        let mut folders = BTreeMap::new();
        for (folder, list) in o
            .get("folders")
            .and_then(Value::as_object)
            .ok_or("folders is an object")?
        {
            if !folder.is_empty() {
                for part in folder.split('/') {
                    valid_name(part).map_err(|e| format!("folder {folder:?}: {e}"))?;
                }
            }
            let mut entries = Vec::new();
            let mut names = BTreeSet::new();
            for e in list
                .as_array()
                .ok_or_else(|| format!("folder {folder:?} is an array"))?
            {
                let e = Entry::from_value(e, folder)?;
                if !names.insert(e.name.clone()) {
                    return Err(format!("folder {folder:?} names {:?} twice", e.name));
                }
                entries.push(e);
            }
            folders.insert(folder.clone(), entries);
        }
        if !folders.contains_key("") {
            return Err("the game folder itself is not listed".into());
        }
        for folder in folders.keys().filter(|f| !f.is_empty()) {
            let (parent, leaf) = folder.rsplit_once('/').unwrap_or(("", folder));
            let listed = folders
                .get(parent)
                .is_some_and(|es| es.iter().any(|e| e.name == leaf && e.kind == Kind::Dir));
            if !listed {
                return Err(format!(
                    "folder {folder:?} is listed, but its parent does not hold it as a folder"
                ));
            }
        }
        Ok(Recording {
            game,
            label,
            appid,
            hint,
            os,
            tool,
            detection,
            folders,
        })
    }

    /// Every regular file: its `/`-joined path, size, head and tail.
    pub fn files(&self) -> impl Iterator<Item = (String, u64, &[u8], &[u8])> {
        self.folders.iter().flat_map(|(folder, entries)| {
            entries.iter().filter_map(move |e| match &e.kind {
                Kind::File { size, head, tail } => Some((
                    if folder.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{folder}/{}", e.name)
                    },
                    *size,
                    head.as_slice(),
                    tail.as_slice(),
                )),
                _ => None,
            })
        })
    }

    /// How many names it holds, of every kind.
    pub fn entries(&self) -> usize {
        self.folders.values().map(Vec::len).sum()
    }

    /// Whether it holds a FIFO or anything else that is neither a file, a folder nor a link.
    pub fn holds_a_fifo(&self) -> bool {
        self.folders
            .values()
            .flatten()
            .any(|e| matches!(e.kind, Kind::Fifo | Kind::Other))
    }
}

/// `path` as a sparse file of `size` bytes, holding `head` at its start and `tail` at its end.
fn lay_file(path: &Path, size: u64, head: &[u8], tail: &[u8]) -> io::Result<()> {
    let f = OpenOptions::new().write(true).create_new(true).open(path)?;
    #[cfg(windows)]
    sparse(&f)?;
    f.set_len(size)?;
    write_at(&f, head, 0)?;
    write_at(&f, tail, size - tail.len() as u64)?;
    Ok(())
}

#[cfg(unix)]
fn write_at(f: &File, bytes: &[u8], at: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, bytes, at)
}

#[cfg(windows)]
fn write_at(f: &File, mut bytes: &[u8], mut at: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_write(f, bytes, at)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        bytes = &bytes[n..];
        at += n as u64;
    }
    Ok(())
}

/// Marks a file sparse (`FSCTL_SET_SPARSE`), so NTFS stores only the bytes written: a recording
/// of a game holds files of gigabytes.
#[cfg(windows)]
fn sparse(f: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let mut returned = 0u32;
    // SAFETY: the handle is `f`'s own, open for the whole call; no input or output buffer.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn lay_fifo(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: a NUL-terminated path, owned for the whole call.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn lay_fifo(path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{}: a FIFO cannot be laid on Windows", path.display()),
    ))
}

#[cfg(unix)]
fn lay_link(path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(DANGLING, path)
}

#[cfg(windows)]
fn lay_link(path: &Path) -> io::Result<()> {
    // Without Developer Mode or elevation no link can be made: left out, which the launcher
    // reads exactly as a link (it never reads through one).
    let _ = std::os::windows::fs::symlink_file(DANGLING, path);
    Ok(())
}

/// Lays `rec` in `root`, an empty folder: see the module doc.
pub fn materialise(rec: &Recording, root: &Path) -> io::Result<()> {
    if fs::read_dir(root)?.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{}: a replay is laid in an empty folder", root.display()),
        ));
    }
    // Sorted: a folder comes before every folder below it.
    for (folder, entries) in &rec.folders {
        let dir = if folder.is_empty() {
            root.to_path_buf()
        } else {
            root.join(folder)
        };
        for e in entries {
            let p = dir.join(&e.name);
            // Every name was held to `valid_name` at parse: a path that is not below `root` now
            // is a recording that did not come through it.
            if !p.starts_with(root) || p.components().any(|c| c.as_os_str() == "..") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: a name leaves the replay's folder", p.display()),
                ));
            }
            match &e.kind {
                Kind::Dir => fs::create_dir(&p)?,
                Kind::File { size, head, tail } => lay_file(&p, *size, head, tail)?,
                Kind::Link => lay_link(&p)?,
                Kind::Fifo | Kind::Other => lay_fifo(&p)?,
            }
        }
    }
    Ok(())
}

/// Every `*.json` file of `dir`, sorted by name.
pub fn recordings_in(dir: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut out: Vec<_> = fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    out.sort();
    Ok(out)
}
